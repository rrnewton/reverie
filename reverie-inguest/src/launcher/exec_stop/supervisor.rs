// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the LICENSE file.

//! Dormant-seize exec supervisor.
//!
//! The child self-stops before its initial exec without calling
//! `PTRACE_TRACEME`; the parent seizes it with `EXITKILL`,
//! `TRACEEXEC`, and `TRACESYSGOOD` and resumes it. Two modes share
//! that seize and the initial-handshake discipline:
//!
//! * [`Supervisor::spawn`] drains only the initial exec syscall
//!   return and detaches. No runtime image is mapped and no
//!   successor is armed: after detach the new image runs
//!   unsupervised. This proves only the launch/event/detach
//!   plumbing and its cost floor.
//! * [`Supervisor::spawn_watched`] stays seized while the guest
//!   runs. Every caught exec event is drained and captured exactly
//!   like the first-image exec stop and handed to the caller as a
//!   [`SuccessorStop`]. The caller's hook re-runs whatever
//!   stopped-image work its policy requires (map, filter, handover,
//!   admission) and returns the stop; the supervisor then restores
//!   the exact original entry context and either resumes the guest
//!   still seized for the next exec or detaches it through the
//!   production release transition. This module keeps no admission
//!   or ticket state of its own: successor admission belongs
//!   entirely to the caller's hook.
//!
//! Stop policy: the watched leader is not job-controlled. While
//! watched, its signal deliveries are forwarded unchanged and a
//! group stop is continued at once: the supervisor sends its own
//! real `SIGCONT` while still seized, consumes the notify trap,
//! and forwards that SIGCONT delivery. The leader therefore
//! ordinarily observes a SIGCONT from the supervisor pid
//! (including a handler, if installed; if a foreign SIGCONT is
//! already pending, standard-signal merging may preserve that
//! sender instead), queued stop signals are flushed by that
//! generation, and the stop lasts zero guest-visible time. A
//! job-control continuer's SIGCONT can therefore become a second
//! delivery (an extra handler run or EINTR), and a leader that
//! keeps stopping itself spins through stop/continue cycles
//! instead of remaining stopped. Only the initial pre-exec
//! window keeps the stricter handshake ownership proof. After
//! `release_detached`, the detached wait is terminal-only: a
//! guest that then stops itself is reported as an unexpected
//! post-detach stop and cancelled.
//!
//! Fork children keep native job control. Their signal
//! deliveries are forwarded unchanged; a group stop is parked
//! with `PTRACE_LISTEN`, so the child stays stopped until a
//! guest continuer's `SIGCONT` (or `SIGKILL`) arrives, and that
//! delivery is then forwarded. A fork child's exec is refused
//! by name (`exec_supervisor_fork_child_exec_refused`): no
//! caller re-arm hook exists for fork children yet, so this
//! module does not re-arm new leaders and does not satisfy a
//! fork-tree successor-arming acceptance on its own. Exit-only
//! fork and vfork children are observed to exit unchanged.
//!
//! Scope limits: seized descendants get `EPERM` if they try to
//! ptrace (guest debuggers and tracers do not work inside the
//! watched tree); forks from untraced non-leader threads are
//! not followed; `clone` children whose exit signal is not
//! `SIGCHLD` are not followed (`PTRACE_O_TRACECLONE` is not
//! requested); an auto-attached child whose fork event is
//! never reported (for example its parent is killed before the
//! event stop) is never driven, because there is no
//! `wait(-1)` discovery; and children are not driven while the
//! caller holds a [`SuccessorStop`]. An exec from a non-leader
//! thread of the watched leader is not observed. The same exec
//! inside a fork child makes that child undrivable and ends
//! the watched run with the named bounded refusal
//! `exec_supervisor_fork_child_undrivable` rather than hanging.
//! On leader exit, release, error, cancellation and drop, the
//! supervisor attempts to detach every driven child (bounded
//! per child), killing any child it cannot detach.

use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::time::Duration;
use std::time::Instant;

use safeptrace::Event;
use safeptrace::ExitStatus;
use safeptrace::Options;
use safeptrace::Running;
use safeptrace::Signal;
use safeptrace::Stopped;
use safeptrace::Wait;

use super::ChildOwner;
use super::CleanupReceipt;
use super::CleanupState;
use super::EntryContext;
use super::ExecBoundary;
use super::LaunchFailure;
use super::Result;
use super::child::PreparedExec;
use super::trace;

/// Whether the dormant supervisor participates in a launch at all.
/// The default keeps every existing launch path unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SupervisorMode {
    /// No seize, no supervisor thread obligation; spawn refuses.
    #[default]
    Disabled,
    /// Seize before the initial exec, catch that exec event, detach.
    DormantSeize,
}

/// Completed detached run: the observed exec, its terminal status,
/// measured spawn-to-after-detach interval, and confirmed cleanup
/// observations.
#[derive(Debug)]
pub struct SupervisorOutcome {
    /// Actual child identity.
    pub pid: safeptrace::Pid,
    /// Former tid reported by the kernel exec event.
    pub former_tid: safeptrace::Pid,
    /// Actual terminal status from the retained physical wait.
    pub status: ExitStatus,
    /// Spawn-entry to just-after-detach interval. It includes fork,
    /// seize, the initial exec event, its one-exit drain, and detach;
    /// it is not a seize-only latency sample.
    pub spawn_to_after_detach: Duration,
    /// Spawn-entry to confirmed terminal wait interval.
    pub total: Duration,
    /// Cleanup observations, including confirmed retirement.
    pub cleanup: CleanupState,
}

/// A detached child whose exec event was caught. Retains the physical
/// owner until [`DetachedSupervisor::wait`] (or drop, which retires
/// through the owner's bounded best-effort cleanup).
#[must_use = "retain the detached supervisor through terminal wait or cancellation"]
pub struct DetachedSupervisor<'fd> {
    pid: safeptrace::Pid,
    former_tid: safeptrace::Pid,
    spawn_to_after_detach: Duration,
    started: Instant,
    owner: ChildOwner<'fd>,
    running: Option<Running>,
    receipt: CleanupReceipt,
}

impl std::fmt::Debug for DetachedSupervisor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetachedSupervisor")
            .field("pid", &self.pid)
            .field("former_tid", &self.former_tid)
            .field("spawn_to_after_detach", &self.spawn_to_after_detach)
            .field("owner", &self.owner)
            .field("detached", &self.running.is_some())
            .field("cleanup", &self.receipt.snapshot())
            .finish()
    }
}

impl<'fd> DetachedSupervisor<'fd> {
    /// The actual child identity.
    pub fn pid(&self) -> safeptrace::Pid {
        self.pid
    }

    /// Former tid from the caught kernel exec event.
    pub fn former_tid(&self) -> safeptrace::Pid {
        self.former_tid
    }

    /// Same-generation shared signal/observation handle; it cannot wait here.
    pub fn cancellation_handle(&self) -> safeptrace::TerminalCleanup {
        self.owner.cancellation_handle()
    }

    /// Take the original child's parent pipe endpoints once. Captured
    /// streams must be drained concurrently with the wait below.
    pub fn take_parent_stdio(&mut self) -> Option<reverie::process::ParentStdio> {
        self.owner._request.take_parent_stdio()
    }

    /// Wait for this same detached generation on the original ptracer.
    pub fn wait(mut self) -> std::result::Result<SupervisorOutcome, Box<DetachedWaitFailure<'fd>>> {
        let running = self.running.take().expect("detached running state");
        match self.owner.wait(running) {
            Ok(status) => Ok(SupervisorOutcome {
                pid: self.pid,
                former_tid: self.former_tid,
                status,
                spawn_to_after_detach: self.spawn_to_after_detach,
                total: self.started.elapsed(),
                cleanup: self.receipt.snapshot(),
            }),
            Err(failure) => Err(Box::new(DetachedWaitFailure {
                failure,
                pid: self.pid,
            })),
        }
    }
}

/// A detached wait that did not complete. The retained launch failure
/// keeps any unresolved physical owner for retry; this wrapper only
/// names the child whose detached wait failed.
#[derive(Debug)]
pub struct DetachedWaitFailure<'fd> {
    /// The retained launch failure from the physical owner.
    pub failure: LaunchFailure<'fd>,
    /// The detached child whose terminal wait failed.
    pub pid: safeptrace::Pid,
}

impl std::fmt::Display for DetachedWaitFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exec supervisor detached wait for {:?}: {}",
            self.pid, self.failure
        )
    }
}
impl std::error::Error for DetachedWaitFailure<'_> {}

/// The handshake signal is the child's own `tgkill(SIGSTOP)`: its
/// siginfo must name that exact cause and sender. Anything else is an
/// unrelated stop signal whose provenance this supervisor may not
/// claim, suppress, or forward on trust.
#[allow(unsafe_code)]
fn require_owned_handshake_sigstop(task: &Stopped, pid: safeptrace::Pid) -> Result<()> {
    let info = task.getsiginfo()?;
    // SAFETY: this is the just-read Linux siginfo for the current stop.
    let sender = unsafe { info.si_pid() };
    if info.si_signo != libc::SIGSTOP || info.si_code != libc::SI_TKILL || sender != pid.as_raw() {
        return Err(format!(
            "exec supervisor encountered unowned SIGSTOP before exec: si_code={}, sender={sender}",
            info.si_code
        )
        .into());
    }
    Ok(())
}

/// A real SIGCONT sent to clear the handshake group stop flushes
/// every queued stop signal at generation time. Refuse first if any
/// stop signal is already queued when peeked, so the clear claims
/// none of those. This does not close the peek-to-kill window: a
/// stop signal queued after the peek can still be flushed by the
/// clearing SIGCONT and leave no trace.
#[allow(unsafe_code)]
fn refuse_pending_stop_signal(task: &Stopped) -> Result<()> {
    for flags in [None, Some(safeptrace::PeekSigInfoFlags::SHARED)] {
        for info in task.peeksiginfo_all(flags)? {
            if matches!(
                info.si_signo,
                libc::SIGSTOP | libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU
            ) {
                // SAFETY: this is a queued Linux siginfo just returned
                // by PTRACE_PEEKSIGINFO for the current stop.
                let sender = unsafe { info.si_pid() };
                return Err(format!(
                    "exec supervisor encountered unowned SIGSTOP before exec: pending stop signal {} si_code={}, sender={sender}",
                    info.si_signo, info.si_code
                )
                .into());
            }
        }
    }
    Ok(())
}

/// The SIGCONT used to clear a handshake group stop is this
/// supervisor process's own `kill`: its delivery stop must name
/// that exact sender and cause before it may be suppressed.
#[allow(unsafe_code)]
fn require_owned_sigcont(task: &Stopped) -> Result<()> {
    let info = task.getsiginfo()?;
    // SAFETY: this is the just-read Linux siginfo for the current stop.
    let sender = unsafe { info.si_pid() };
    if info.si_signo != libc::SIGCONT
        || info.si_code != libc::SI_USER
        || sender != std::process::id() as i32
    {
        return Err(format!(
            "exec supervisor encountered unowned SIGCONT before exec: si_code={}, sender={sender}",
            info.si_code
        )
        .into());
    }
    Ok(())
}

/// Send this supervisor process's real SIGCONT to a seized child.
/// Generating it (not injecting it on a ptrace resume) is what
/// clears the kernel's hidden natural-parent group-stop report.
fn send_own_sigcont(pid: safeptrace::Pid) -> Result<()> {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid.as_raw()),
        nix::sys::signal::Signal::SIGCONT,
    )?;
    Ok(())
}

/// The first stop after sending the clearing SIGCONT is the ptrace
/// trap notification for that generation, not a fresh group stop:
/// its siginfo must be the child's own PTRACE_EVENT_STOP trap.
#[allow(unsafe_code)]
fn require_group_stop_clear_trap(task: &Stopped, pid: safeptrace::Pid) -> Result<()> {
    let info = task.getsiginfo()?;
    // SAFETY: this is the just-read Linux siginfo for the current stop.
    let sender = unsafe { info.si_pid() };
    let expected = libc::SIGTRAP | (libc::PTRACE_EVENT_STOP << 8);
    if info.si_signo != libc::SIGTRAP || info.si_code != expected || sender != pid.as_raw() {
        return Err(format!(
            "exec supervisor encountered unexpected group-stop clear trap: si_signo={}, si_code={}, sender={sender}",
            info.si_signo, info.si_code
        )
        .into());
    }
    Ok(())
}

/// Classify a PTRACE_EVENT_STOP trap by its siginfo. A SIGCONT
/// notify trap names SIGTRAP; a genuine group stop names its stop
/// signal in both the signal number and the low byte of the code.
/// The stopped child is the sender of the trap siginfo in both
/// cases. Returns the validated signal number.
#[allow(unsafe_code)]
fn classify_group_stop_trap(
    task: &Stopped,
    pid: safeptrace::Pid,
    refusal: &'static str,
) -> Result<i32> {
    let info = task.getsiginfo()?;
    // SAFETY: this is the just-read Linux siginfo for the current stop.
    let sender = unsafe { info.si_pid() };
    if !matches!(
        info.si_signo,
        libc::SIGTRAP | libc::SIGSTOP | libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU
    ) || info.si_code != info.si_signo | (libc::PTRACE_EVENT_STOP << 8)
        || sender != pid.as_raw()
    {
        return Err(format!(
            "{refusal}: si_signo={}, si_code={}, sender={sender}",
            info.si_signo, info.si_code
        )
        .into());
    }
    Ok(info.si_signo)
}

/// Wait for the child's handshake stop after seize. A group stop
/// has already consumed the handshake signal; no signal-delivery
/// phase remains outstanding. A signal-delivery stop is accepted
/// only if its siginfo proves it is the child's own handshake.
/// Every other pre-exec stop fails closed rather than being
/// silently forwarded or dropped. Bounded only by the caller's
/// external deadline, like every synchronous exec-stop wait.
fn wait_initial_stop(running: Running, pid: safeptrace::Pid) -> Result<(Stopped, bool)> {
    match trace::wait(running)? {
        Wait::Stopped(task, event) => match event {
            Event::Stop => {
                let signo = classify_group_stop_trap(
                    &task,
                    pid,
                    "exec supervisor encountered unexpected handshake group stop",
                )?;
                if signo != libc::SIGSTOP {
                    return Err(format!(
                        "exec supervisor encountered unexpected handshake group stop: si_signo={signo}"
                    )
                    .into());
                }
                Ok((task, true))
            }
            Event::Signal(Signal::SIGSTOP) => {
                require_owned_handshake_sigstop(&task, pid)?;
                Ok((task, false))
            }
            Event::Signal(signal) => Err(format!(
                "exec supervisor encountered unexpected signal before exec: {signal:?}"
            )
            .into()),
            Event::RawSignal(raw) => Err(format!(
                "exec supervisor encountered unexpected raw signal before exec: {raw}"
            )
            .into()),
            Event::Exec(_) => {
                Err("exec supervisor received an exec event before its seize stop".into())
            }
            other_event => Err(format!(
                "exec supervisor encountered unexpected stop before exec: {other_event:?}"
            )
            .into()),
        },
        other => Err(format!("exec supervisor expected its seize stop, got {other:?}").into()),
    }
}

/// Consume the seize stop and resume toward the initial exec. A
/// handshake that completed as a group stop consumed its SIGSTOP,
/// but the natural parent's group-stop report remains hidden while
/// seized and would become a stale stopped status after detach.
/// Send this supervisor's own real SIGCONT while still seized so
/// the kernel clears that report at generation time; the returned
/// flag says its trap/delivery stops must still be consumed before
/// the first exec. A signal-delivery handshake stop is simply
/// suppressed. Runs entirely on the original ptracer thread.
fn consume_handshake(
    owner: &mut ChildOwner<'_>,
    running: Running,
    pid: safeptrace::Pid,
    before_initial_resume: &mut dyn FnMut(safeptrace::Pid),
) -> Result<(Running, bool)> {
    owner._thread = Some(owner.generation.ptracer_thread_guard()?);
    let (initial, group_stop) = wait_initial_stop(running, pid)?;
    before_initial_resume(pid);
    let running = if group_stop {
        refuse_pending_stop_signal(&initial)?;
        send_own_sigcont(pid)?;
        initial.resume(None)?
    } else {
        initial.resume(None)?
    };
    Ok((running, group_stop))
}

/// Wait for the genuine exec event, verify it names this child and the
/// launcher's own `execve`, then detach. The new image runs unarmed.
fn catch_initial_exec(
    owner: &mut ChildOwner<'_>,
    running: Running,
    pid: safeptrace::Pid,
    started: Instant,
    before_initial_resume: &mut dyn FnMut(safeptrace::Pid),
) -> Result<(Running, safeptrace::Pid, Duration)> {
    let (running, group_stop) = consume_handshake(owner, running, pid, before_initial_resume)?;
    let (running, former) = wait_exec_detach(running, pid, group_stop)?;
    Ok((running, former, started.elapsed()))
}

fn wait_exec_detach(
    mut running: Running,
    pid: safeptrace::Pid,
    mut group_stop_clear_pending: bool,
) -> Result<(Running, safeptrace::Pid)> {
    loop {
        match trace::wait(running)? {
            Wait::Stopped(task, Event::Exec(former)) => {
                if former != pid {
                    return Err(format!(
                        "exec supervisor expected exec of {pid:?}, got former {former:?}"
                    )
                    .into());
                }
                // Drain only the exec syscall return, like ExecStop,
                // so detach leaves the new image at its entry with no
                // in-flight syscall. A group-stop handshake was
                // already cleared by a real SIGCONT while seized, so
                // no stale natural-parent stop remains for the
                // detached wait. The captured context is discarded:
                // this mode detaches without touching the new image.
                let (task, _context, _boundary) = super::drain_and_capture(task, former)?;
                let running = task.detach(Some(Signal::SIGCONT))?;
                return Ok((running, former));
            }
            Wait::Stopped(task, Event::Stop) if group_stop_clear_pending => {
                require_group_stop_clear_trap(&task, pid)?;
                running = task.resume(None)?;
            }
            Wait::Stopped(task, Event::Signal(Signal::SIGCONT)) if group_stop_clear_pending => {
                require_owned_sigcont(&task)?;
                group_stop_clear_pending = false;
                running = task.resume(None)?;
            }
            Wait::Stopped(task, Event::Signal(Signal::SIGSTOP)) => {
                // No handshake signal-delivery phase remains: a group
                // stop consumed the child's SIGSTOP and a
                // signal-delivery handshake was suppressed at the
                // seize stop. Preserve the ownership proof so a
                // SIGSTOP from any other sender is named unowned,
                // then fail closed rather than swallow either kind.
                require_owned_handshake_sigstop(&task, pid)?;
                return Err(
                    "exec supervisor encountered SIGSTOP before exec: no handshake signal was pending"
                        .into(),
                );
            }
            Wait::Stopped(_task, Event::Signal(signal)) => {
                return Err(format!(
                    "exec supervisor encountered unexpected signal before exec: {signal:?}"
                )
                .into());
            }
            Wait::Stopped(_task, Event::RawSignal(raw)) => {
                return Err(format!(
                    "exec supervisor encountered unexpected raw signal before exec: {raw}"
                )
                .into());
            }
            Wait::Stopped(_task, other_event) => {
                return Err(format!(
                    "exec supervisor encountered unexpected stop before exec: {other_event:?}"
                )
                .into());
            }
            other => {
                return Err(format!(
                    "exec supervisor child reached {other:?} before an exec event"
                )
                .into());
            }
        }
    }
}

/// Assemble a launch failure from a retained owner: preserve any
/// parent stdio, cancel, and keep an unconfirmed owner for retry.
fn launch_failure<'fd>(
    mut owner: ChildOwner<'fd>,
    receipt: CleanupReceipt,
    error: Box<dyn std::error::Error>,
) -> LaunchFailure<'fd> {
    let parent_stdio = owner._request.take_parent_stdio();
    let deferred = owner.cancel().err();
    LaunchFailure {
        error,
        receipt,
        owner: deferred,
        pending: None,
        parent_stdio,
    }
}

/// Seize options for the supervisor. Fork following is watched-only:
/// the dormant path detaches at the first exec and must not
/// auto-attach children it would never drive.
fn supervisor_options(follow_forks: bool) -> Options {
    let base =
        Options::PTRACE_O_EXITKILL | Options::PTRACE_O_TRACEEXEC | Options::PTRACE_O_TRACESYSGOOD;
    if follow_forks {
        base | Options::PTRACE_O_TRACEFORK | Options::PTRACE_O_TRACEVFORK
    } else {
        base
    }
}

/// One auto-attached fork child being driven by the watched
/// supervisor. Its wait is polled, never blocked on, so the leader
/// and every sibling keep making progress.
struct ChildWait {
    driver: safeptrace::PtracerWaitDriver,
    initialized: bool,
}

impl ChildWait {
    fn new(running: Running) -> Self {
        Self::from_running(running, false)
    }

    fn from_running(running: Running, initialized: bool) -> Self {
        Self {
            driver: running.wait_owned_on_ptracer_thread().into_driver(),
            initialized,
        }
    }
}

fn poll_driver(
    driver: &mut safeptrace::PtracerWaitDriver,
) -> Option<std::result::Result<Wait, safeptrace::OwnedWaitError>> {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    match driver.poll_on_ptracer_thread(&mut cx) {
        Poll::Ready(result) => Some(result),
        Poll::Pending => None,
    }
}

/// The driven fork children of a watched run. Dropping the set
/// without an explicit detach (drop of the supervisor, unwinding)
/// attempts to detach every child still in it, killing any child
/// that cannot be detached, so an exit path does not knowingly
/// leave a guest process seized by a supervisor that will never
/// wait it.
struct ChildSet(Vec<ChildWait>);

impl ChildSet {
    fn take(&mut self) -> Vec<ChildWait> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for ChildSet {
    fn drop(&mut self) {
        detach_children_best_effort(std::mem::take(&mut self.0));
    }
}

/// How long one fork child's detach or cancel may take before it
/// is reported instead of waited on forever.
const CHILD_DETACH_TIMEOUT: Duration = Duration::from_secs(2);

fn interrupt_child(pid: safeptrace::Pid) -> Result<()> {
    match Running::new_on_ptracer_thread(pid) {
        Ok(running) => match running.interrupt() {
            Ok(()) => Ok(()),
            Err(reverie::Errno::ESRCH | reverie::Errno::ENOENT) => Ok(()),
            Err(error) => Err(format!(
                "exec supervisor fork-child detach interrupt failed for {pid:?}: {error}"
            )
            .into()),
        },
        Err(reverie::Errno::ESRCH | reverie::Errno::ENOENT) => Ok(()),
        Err(error) => Err(format!(
            "exec supervisor fork-child detach capture failed for {pid:?}: {error}"
        )
        .into()),
    }
}

/// Detach one driven fork child, taking ownership of any child
/// its stop carries. A signal-delivery stop detaches with its own
/// signal forwarded; event and trap stops detach with none. No
/// `SIGCONT` is ever injected here. A child whose detach fails is
/// queued in `failed` for bounded cancellation, so an error can
/// be reported without leaving that child seized.
fn detach_child(
    child: ChildWait,
    work: &mut Vec<ChildWait>,
    failed: &mut Vec<ChildWait>,
) -> Result<()> {
    let generation = child.driver.generation();
    let initialized = child.initialized;
    let result = detach_child_inner(child, work);
    if result.is_err()
        && let Some(generation) = generation
    {
        let pid = generation.pid();
        let cleanup = generation.terminal_cleanup();
        let _ = cleanup.request_sigkill();
        if let Ok(running) = Running::new_on_ptracer_thread(pid) {
            failed.push(ChildWait::from_running(running, initialized));
        }
    }
    result
}

fn detach_child_inner(mut child: ChildWait, work: &mut Vec<ChildWait>) -> Result<()> {
    let Some(generation) = child.driver.generation() else {
        return Ok(());
    };
    let pid = generation.pid();
    let deadline = Instant::now() + CHILD_DETACH_TIMEOUT;
    let mut interrupted = false;
    loop {
        match poll_driver(&mut child.driver) {
            Some(Ok(Wait::Exited(_, _))) => return Ok(()),
            Some(Ok(Wait::Stopped(task, event))) => match event {
                Event::Signal(signal) => {
                    task.detach(Some(signal))?;
                    return Ok(());
                }
                Event::RawSignal(raw) => {
                    task.detach_raw(raw)?;
                    return Ok(());
                }
                Event::NewChild(_op, grandchild) => {
                    work.push(ChildWait::new(grandchild));
                    task.detach(None)?;
                    return Ok(());
                }
                _ => {
                    task.detach(None)?;
                    return Ok(());
                }
            },
            Some(Err(safeptrace::OwnedWaitError::Completed)) => return Ok(()),
            Some(Err(safeptrace::OwnedWaitError::Died)) => {
                return Err(format!(
                    "exec_supervisor_fork_child_undrivable: fork child {pid:?} died without a terminal status during detach"
                )
                .into());
            }
            Some(Err(safeptrace::OwnedWaitError::Errno(reverie::Errno::ECHILD))) => {
                return Err(format!(
                    "exec_supervisor_fork_child_undrivable: fork child {pid:?} wait refused with ECHILD during detach"
                )
                .into());
            }
            Some(Err(error)) => {
                return Err(format!(
                    "exec supervisor fork-child detach wait failed for {pid:?}: {error:?}"
                )
                .into());
            }
            None => {
                if !interrupted {
                    interrupted = true;
                    interrupt_child(pid)?;
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "exec_supervisor_fork_child_undrivable: fork-child detach timed out for {pid:?}"
                    )
                    .into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Detach every driven fork child, including any child met while
/// detaching. Used when the leader is released or terminates: no
/// child may be left seized by a supervisor that will never wait
/// it again. Every child is attempted; the first error is kept.
fn detach_children(children: Vec<ChildWait>) -> Result<()> {
    let mut work = children;
    let mut failed = Vec::new();
    let mut first_error: Option<Box<dyn std::error::Error>> = None;
    while let Some(child) = work.pop() {
        if let Err(error) = detach_child(child, &mut work, &mut failed)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    if !failed.is_empty() {
        cancel_children(failed);
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Detach where the caller has no error channel (Drop and the
/// fixed-signature cancel methods). [`detach_children`] has
/// already attempted every child and attempted to kill any it
/// could not detach before the caller proceeds.
fn detach_children_best_effort(children: Vec<ChildWait>) {
    match detach_children(children) {
        Ok(()) => {}
        Err(_error) => {}
    }
}

/// Append a child-detach failure to the primary launch error
/// instead of discarding either one.
fn with_child_detach_result(
    primary: Box<dyn std::error::Error>,
    children: Vec<ChildWait>,
) -> Box<dyn std::error::Error> {
    match detach_children(children) {
        Ok(()) => primary,
        Err(detach_error) => {
            format!("{primary}; exec supervisor fork-child detach also failed: {detach_error}")
                .into()
        }
    }
}

/// Kill one driven fork child and drive it to a terminal state,
/// bounded. Used only for a child that could not be detached:
/// delivery stops forward their own signal (including the
/// requested `SIGKILL`), and children met on the way are queued.
fn cancel_child(mut child: ChildWait, work: &mut Vec<ChildWait>) {
    let Some(generation) = child.driver.generation() else {
        return;
    };
    let cleanup = generation.terminal_cleanup();
    let _ = cleanup.request_sigkill();
    let deadline = Instant::now() + CHILD_DETACH_TIMEOUT;
    loop {
        match poll_driver(&mut child.driver) {
            Some(Ok(Wait::Exited(_, _))) => break,
            Some(Ok(Wait::Stopped(task, event))) => {
                let resumed = match event {
                    Event::NewChild(_op, grandchild) => {
                        work.push(ChildWait::new(grandchild));
                        task.resume(None)
                    }
                    Event::Signal(signal) => task.resume(Some(signal)),
                    Event::RawSignal(raw) => task.resume_raw(raw),
                    _ => task.resume(None),
                };
                match resumed {
                    Ok(running) => {
                        child = ChildWait::from_running(running, child.initialized);
                    }
                    Err(_) => break,
                }
            }
            Some(Err(_)) => break,
            None => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn cancel_children(children: Vec<ChildWait>) {
    let mut work = children;
    while let Some(child) = work.pop() {
        cancel_child(child, &mut work);
    }
}

/// Claim the receipt, clone one owned child and seize it before its
/// initial exec. Shared by the detached and watched entry points.
fn seize_child<'fd>(
    mut request: PreparedExec<'fd>,
    receipt: CleanupReceipt,
    follow_forks: bool,
    before_seize: &mut dyn FnMut(safeptrace::Pid),
) -> std::result::Result<
    (
        ChildOwner<'fd>,
        Running,
        safeptrace::Pid,
        Instant,
        CleanupReceipt,
    ),
    LaunchFailure<'fd>,
> {
    let started = Instant::now();
    if !receipt.claim() {
        return Err(LaunchFailure {
            error: "exec supervisor cleanup receipt already used".into(),
            receipt,
            owner: None,
            pending: None,
            parent_stdio: None,
        });
    }
    request = request.with_supervisor_seize();
    let mut pending = match request.fork(receipt.clone()) {
        Ok(pending) => pending,
        Err(error) => {
            return Err(LaunchFailure {
                error: format!("loader_clone_pidfd_refused: {error}").into(),
                receipt,
                owner: None,
                pending: None,
                parent_stdio: None,
            });
        }
    };
    #[cfg(feature = "loader-private-runtime")]
    let birth_pidfd = match pending.duplicate_birth_pidfd() {
        Ok(fd) => fd,
        Err(error) => {
            let parent_stdio = request.take_parent_stdio();
            pending.retire();
            let deferred = pending.is_owned().then(|| Box::new((pending, request)));
            return Err(LaunchFailure {
                error: format!("exec_stop_birth_pidfd_retention_failed: {error}").into(),
                receipt,
                owner: None,
                pending: deferred,
                parent_stdio,
            });
        }
    };
    let pid = pending.pid();
    before_seize(pid);
    let running = match Running::seize_on_ptracer_thread(pid, supervisor_options(follow_forks)) {
        Ok(running) => running,
        Err(error) => {
            let parent_stdio = request.take_parent_stdio();
            pending.retire();
            let deferred = pending.is_owned().then(|| Box::new((pending, request)));
            return Err(LaunchFailure {
                error: error.into(),
                receipt,
                owner: None,
                pending: deferred,
                parent_stdio,
            });
        }
    };
    let cleanup = running.terminal_cleanup_on_ptracer_thread();
    let generation = running.generation();
    // No fallible operation between installing the original
    // generation's cleanup owner and disarming the atomic
    // pre-registration pidfd owner (same invariant as ExecStop).
    let owner = ChildOwner {
        cleanup,
        generation,
        #[cfg(feature = "loader-private-runtime")]
        birth_pidfd,
        _thread: None,
        _request: request,
        receipt: receipt.clone(),
        complete: false,
    };
    pending.transfer_to_ptracer();
    Ok((owner, running, pid, started, receipt))
}

/// Dormant-seize supervisor entry point. See module docs.
pub struct Supervisor;

impl Supervisor {
    /// Clone one owned child, seize it before its initial exec, catch
    /// that exec event, and detach. Caller must retain an external
    /// physical deadline for synchronous waits. With
    /// [`SupervisorMode::Disabled`] (the default) this refuses before
    /// any child is created; the receipt is left unclaimed.
    pub fn spawn<'fd>(
        request: PreparedExec<'fd>,
        receipt: CleanupReceipt,
        mode: SupervisorMode,
    ) -> std::result::Result<DetachedSupervisor<'fd>, LaunchFailure<'fd>> {
        Self::spawn_inner(request, receipt, mode, &mut |_| {}, &mut |_| {})
    }

    fn spawn_inner<'fd>(
        request: PreparedExec<'fd>,
        receipt: CleanupReceipt,
        mode: SupervisorMode,
        before_seize: &mut dyn FnMut(safeptrace::Pid),
        before_initial_resume: &mut dyn FnMut(safeptrace::Pid),
    ) -> std::result::Result<DetachedSupervisor<'fd>, LaunchFailure<'fd>> {
        if mode == SupervisorMode::Disabled {
            return Err(LaunchFailure {
                error: "exec supervisor is disabled".into(),
                receipt,
                owner: None,
                pending: None,
                parent_stdio: None,
            });
        }
        let (mut owner, running, pid, started, receipt) =
            seize_child(request, receipt, false, before_seize)?;
        let result = catch_initial_exec(&mut owner, running, pid, started, before_initial_resume);
        match result {
            Ok((running, former_tid, spawn_to_after_detach)) => Ok(DetachedSupervisor {
                pid,
                former_tid,
                spawn_to_after_detach,
                started,
                owner,
                running: Some(running),
                receipt,
            }),
            Err(error) => Err(launch_failure(owner, receipt, error)),
        }
    }

    /// Clone one owned child, seize it before its initial exec, and
    /// stay seized while it runs. Each [`WatchedSupervisor::wait_next`]
    /// returns the next caught exec as a [`SuccessorStop`] (drained
    /// and captured before any guest instruction of the new image)
    /// or the terminal outcome. Caller must retain an external
    /// physical deadline for synchronous waits.
    pub fn spawn_watched<'fd>(
        request: PreparedExec<'fd>,
        receipt: CleanupReceipt,
    ) -> std::result::Result<WatchedSupervisor<'fd>, LaunchFailure<'fd>> {
        Self::spawn_watched_inner(request, receipt, &mut |_| {}, &mut |_| {})
    }

    fn spawn_watched_inner<'fd>(
        request: PreparedExec<'fd>,
        receipt: CleanupReceipt,
        before_seize: &mut dyn FnMut(safeptrace::Pid),
        before_initial_resume: &mut dyn FnMut(safeptrace::Pid),
    ) -> std::result::Result<WatchedSupervisor<'fd>, LaunchFailure<'fd>> {
        let (mut owner, running, pid, started, receipt) =
            seize_child(request, receipt, true, before_seize)?;
        match consume_handshake(&mut owner, running, pid, before_initial_resume) {
            Ok((running, group_stop_clear_pending)) => Ok(WatchedSupervisor {
                pid,
                started,
                owner,
                running: Some(running),
                receipt,
                pre_first_exec: true,
                group_stop_clear_pending,
                execs_seen: 0,
                children: ChildSet(Vec::new()),
            }),
            Err(error) => Err(launch_failure(owner, receipt, error)),
        }
    }
}

/// Terminal outcome of a watched run: actual status from the retained
/// physical wait, how many exec events were caught, and confirmed
/// cleanup observations.
#[derive(Debug)]
pub struct WatchedOutcome {
    /// Actual child identity.
    pub pid: safeptrace::Pid,
    /// Actual terminal status from the retained physical wait.
    pub status: ExitStatus,
    /// Exec events caught and handed out as successor stops.
    pub execs_seen: u64,
    /// Spawn-entry to confirmed terminal wait interval.
    pub total: Duration,
    /// Cleanup observations, including confirmed retirement.
    pub cleanup: CleanupState,
}

/// One step of a watched run: the next caught exec stop, or the end.
#[derive(Debug)]
pub enum WatchedStep<'fd> {
    /// An exec event was caught, drained and captured; no guest
    /// instruction of the new image has run. Boxed because the stop
    /// carries the full captured entry context and physical owner.
    Exec(Box<SuccessorStop<'fd>>),
    /// The watched child terminated and retirement was recorded
    /// from the actual terminal wait.
    Finished(WatchedOutcome),
}

enum NextExec {
    Exec(Stopped),
    Finished(ExitStatus),
}

/// Drive one stopped fork child to its next running state. Its
/// first stop is the auto-attach initial stop: install the same
/// exec/fork options and suppress it. Signals forward, group
/// stops are parked in the kernel listening state (native job
/// control: the child stays stopped until a guest continuer),
/// and nested forks are captured as new driven children. A fork
/// child's exec is refused by name: no caller re-arm hook exists
/// for fork children yet, and running one unarmed would escape
/// supervision. An exit-only child needs no exec handling: its
/// `Exited` wait is consumed by the caller and the guest parent's
/// own wait proceeds.
fn handle_child_stop(
    task: Stopped,
    event: Event,
    initialized: &mut bool,
    new_children: &mut Vec<ChildWait>,
) -> Result<Running> {
    let pid = task.pid();
    let cleanup = task.terminal_cleanup();
    let result = handle_child_stop_inner(task, event, initialized, new_children);
    if result.is_err() {
        // The stopped capability was consumed by the failing
        // operation. Kill the exact generation and queue a fresh
        // driver for it so the caller's detach path can still
        // settle this child (and any child captured above).
        let _ = cleanup.request_sigkill();
        if let Ok(running) = Running::new_on_ptracer_thread(pid) {
            new_children.push(ChildWait::from_running(running, *initialized));
        }
    }
    result
}

fn handle_child_stop_inner(
    task: Stopped,
    event: Event,
    initialized: &mut bool,
    new_children: &mut Vec<ChildWait>,
) -> Result<Running> {
    if !*initialized {
        task.setoptions(supervisor_options(true))?;
        *initialized = true;
        if matches!(event, Event::Stop | Event::Signal(Signal::SIGSTOP)) {
            return Ok(task.resume(None)?);
        }
    }
    match event {
        Event::Exec(_former) => {
            // No caller hook contract exists for fork-child execs:
            // kill this child and fail the launch closed by name
            // rather than run its new image unarmed.
            let cleanup = task.terminal_cleanup();
            let _ = cleanup.request_sigkill();
            let _ = task.resume(None);
            Err(
                "exec_supervisor_fork_child_exec_refused: fork-child execs have no caller re-arm hook"
                    .into(),
            )
        }
        Event::NewChild(_op, child) => {
            new_children.push(ChildWait::new(child));
            Ok(task.resume(None)?)
        }
        Event::VforkDone => Ok(task.resume(None)?),
        Event::Signal(signal) => Ok(task.resume(Some(signal))?),
        Event::RawSignal(raw) => Ok(task.resume_raw(raw)?),
        Event::Stop => {
            let pid = task.pid();
            let signo = classify_group_stop_trap(
                &task,
                pid,
                "exec supervisor encountered unexpected fork-child stop trap",
            )?;
            if signo == libc::SIGTRAP {
                Ok(task.resume(None)?)
            } else {
                // Native job control: park the child in the kernel
                // listening state instead of continuing it. It
                // re-traps only when a guest continuer's SIGCONT
                // (or SIGKILL) actually continues it.
                Ok(task.listen()?)
            }
        }
        Event::Seccomp | Event::Syscall | Event::Exit => Ok(task.resume(None)?),
    }
}

/// Drive the seized guest until its next exec event or terminal
/// exit. Before the first exec, only this supervisor's own
/// group-stop-clearing SIGCONT trap/delivery may be suppressed;
/// any other pre-exec SIGCONT notify trap is resumed and its
/// delivery is forwarded.
/// Afterwards guest signal deliveries are forwarded unchanged; a
/// guest group stop is continued at once with this supervisor's
/// real `SIGCONT`, whose delivery is forwarded. The stop lasts
/// zero guest-visible time and the guest observes a SIGCONT from
/// the supervisor pid.
fn wait_watched(
    running: Running,
    pid: safeptrace::Pid,
    pre_first_exec: bool,
    group_stop_clear_pending: &mut bool,
    children: &mut ChildSet,
) -> Result<NextExec> {
    let result = wait_watched_inner(
        running,
        pid,
        pre_first_exec,
        group_stop_clear_pending,
        children,
    );
    match result {
        Ok(next) => Ok(next),
        Err(error) => Err(with_child_detach_result(error, children.take())),
    }
}

fn wait_watched_inner(
    running: Running,
    pid: safeptrace::Pid,
    pre_first_exec: bool,
    group_stop_clear_pending: &mut bool,
    children: &mut ChildSet,
) -> Result<NextExec> {
    let mut leader = running.wait_owned_on_ptracer_thread().into_driver();
    loop {
        let mut progressed = false;
        let mut index = 0;
        while index < children.0.len() {
            match poll_driver(&mut children.0[index].driver) {
                Some(Ok(Wait::Exited(_, _))) => {
                    children.0.remove(index);
                    progressed = true;
                }
                Some(Ok(Wait::Stopped(task, event))) => {
                    progressed = true;
                    let mut new_children = Vec::new();
                    let next = handle_child_stop(
                        task,
                        event,
                        &mut children.0[index].initialized,
                        &mut new_children,
                    );
                    let initialized = children.0[index].initialized;
                    match next {
                        Ok(next) => {
                            children.0[index] = ChildWait::from_running(next, initialized);
                            children.0.extend(new_children);
                            index += 1;
                        }
                        Err(error) => {
                            // Keep any child captured by the failing
                            // call (and its recovery driver) owned by
                            // the set the error path detaches.
                            children.0.extend(new_children);
                            return Err(error);
                        }
                    }
                }
                Some(Err(safeptrace::OwnedWaitError::Died)) => {
                    return Err(
                        "exec_supervisor_fork_child_undrivable: fork child died without a terminal status"
                            .into(),
                    );
                }
                Some(Err(safeptrace::OwnedWaitError::Errno(reverie::Errno::ECHILD))) => {
                    return Err(
                        "exec_supervisor_fork_child_undrivable: fork child wait refused with ECHILD"
                            .into(),
                    );
                }
                Some(Err(error)) => {
                    return Err(format!("exec supervisor fork-child wait failed: {error:?}").into());
                }
                None => index += 1,
            }
        }
        if let Some(waited) = poll_driver(&mut leader) {
            progressed = true;
            match waited? {
                Wait::Exited(_, status) => {
                    detach_children(children.take())?;
                    return Ok(NextExec::Finished(status));
                }
                Wait::Stopped(task, Event::Exec(former)) => {
                    if former != pid {
                        return Err(format!(
                            "exec supervisor expected exec of {pid:?}, got former {former:?}"
                        )
                        .into());
                    }
                    return Ok(NextExec::Exec(task));
                }
                Wait::Stopped(task, Event::NewChild(_op, child)) => {
                    children.0.push(ChildWait::new(child));
                    leader = task
                        .resume(None)?
                        .wait_owned_on_ptracer_thread()
                        .into_driver();
                }
                Wait::Stopped(task, Event::Stop) if pre_first_exec && *group_stop_clear_pending => {
                    require_group_stop_clear_trap(&task, pid)?;
                    leader = task
                        .resume(None)?
                        .wait_owned_on_ptracer_thread()
                        .into_driver();
                }
                Wait::Stopped(task, Event::Signal(Signal::SIGCONT))
                    if pre_first_exec && *group_stop_clear_pending =>
                {
                    require_owned_sigcont(&task)?;
                    *group_stop_clear_pending = false;
                    leader = task
                        .resume(None)?
                        .wait_owned_on_ptracer_thread()
                        .into_driver();
                }
                Wait::Stopped(task, Event::Signal(Signal::SIGSTOP)) if pre_first_exec => {
                    require_owned_handshake_sigstop(&task, pid)?;
                    return Err(
                        "exec supervisor encountered SIGSTOP before exec: no handshake signal was pending"
                            .into(),
                    );
                }
                Wait::Stopped(task, Event::Signal(signal)) => {
                    leader = task
                        .resume(Some(signal))?
                        .wait_owned_on_ptracer_thread()
                        .into_driver();
                }
                Wait::Stopped(task, Event::RawSignal(raw)) => {
                    leader = task
                        .resume_raw(raw)?
                        .wait_owned_on_ptracer_thread()
                        .into_driver();
                }
                Wait::Stopped(task, Event::Stop) => {
                    // Classify by siginfo, with no pending flag: every
                    // SIGCONT to a seized task arms a SIGTRAP notify
                    // trap, whether or not the guest was stopped, and a
                    // SIGCONT delivery need not reach the traced leader.
                    let signo = classify_group_stop_trap(
                        &task,
                        pid,
                        "exec supervisor encountered unexpected watched stop trap",
                    )?;
                    if signo == libc::SIGTRAP {
                        // SIGCONT notify trap. No PTRACE_INTERRUPT or
                        // PTRACE_LISTEN is issued by this module; a
                        // future interrupt-based cancel must not reuse
                        // this resume without distinguishing its trap.
                        leader = task
                            .resume(None)?
                            .wait_owned_on_ptracer_thread()
                            .into_driver();
                        continue;
                    }
                    if pre_first_exec {
                        return Err(
                            "exec supervisor encountered unexpected group stop before exec".into(),
                        );
                    }
                    // A signal injected on the group-stop resume would be
                    // ignored and leave the natural-parent stop report
                    // for after detach. Generate a real SIGCONT instead;
                    // its notify trap is consumed above and its delivery
                    // reaches the general signal arm.
                    send_own_sigcont(pid)?;
                    leader = task
                        .resume(None)?
                        .wait_owned_on_ptracer_thread()
                        .into_driver();
                }
                Wait::Stopped(_task, other_event) => {
                    return Err(format!(
                        "exec supervisor encountered unexpected stop while watched: {other_event:?}"
                    )
                    .into());
                }
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// A seized, running guest under exec watch. Retains the physical
/// owner until [`WatchedSupervisor::wait_next`] reports the terminal
/// outcome (or drop, which retires through the owner's bounded
/// best-effort cleanup; `EXITKILL` covers supervisor death).
#[must_use = "retain the watched supervisor through its terminal outcome or cancellation"]
pub struct WatchedSupervisor<'fd> {
    pid: safeptrace::Pid,
    started: Instant,
    owner: ChildOwner<'fd>,
    running: Option<Running>,
    receipt: CleanupReceipt,
    pre_first_exec: bool,
    group_stop_clear_pending: bool,
    execs_seen: u64,
    children: ChildSet,
}

impl std::fmt::Debug for WatchedSupervisor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchedSupervisor")
            .field("pid", &self.pid)
            .field("execs_seen", &self.execs_seen)
            .field("children", &self.children.0.len())
            .field("owner", &self.owner)
            .field("seized", &self.running.is_some())
            .field("cleanup", &self.receipt.snapshot())
            .finish()
    }
}

impl<'fd> WatchedSupervisor<'fd> {
    /// The actual child identity.
    pub fn pid(&self) -> safeptrace::Pid {
        self.pid
    }

    /// Same-generation shared signal/observation handle; it cannot wait here.
    pub fn cancellation_handle(&self) -> safeptrace::TerminalCleanup {
        self.owner.cancellation_handle()
    }

    /// Take the original child's parent pipe endpoints once. Captured
    /// streams must be drained concurrently with the waits below.
    pub fn take_parent_stdio(&mut self) -> Option<reverie::process::ParentStdio> {
        self.owner._request.take_parent_stdio()
    }

    /// Run the guest until its next exec event or terminal exit, on
    /// the original ptracer thread. A caught exec is drained and
    /// captured before this returns, so the successor stop sits at
    /// the new image's kernel entry with no guest progress.
    pub fn wait_next(mut self) -> std::result::Result<WatchedStep<'fd>, LaunchFailure<'fd>> {
        let running = self.running.take().expect("watched running state");
        let Self {
            pid,
            started,
            mut owner,
            receipt,
            pre_first_exec,
            mut group_stop_clear_pending,
            execs_seen,
            mut children,
            ..
        } = self;
        let next = wait_watched(
            running,
            pid,
            pre_first_exec,
            &mut group_stop_clear_pending,
            &mut children,
        );
        match next {
            Ok(NextExec::Finished(status)) => {
                owner.record_detached_terminal(status);
                Ok(WatchedStep::Finished(WatchedOutcome {
                    pid,
                    status,
                    execs_seen,
                    total: started.elapsed(),
                    cleanup: receipt.snapshot(),
                }))
            }
            Ok(NextExec::Exec(task)) => match super::drain_and_capture(task, pid) {
                Ok((task, context, boundary)) => Ok(WatchedStep::Exec(Box::new(SuccessorStop {
                    task,
                    context,
                    boundary,
                    owner,
                    receipt,
                    pid,
                    started,
                    execs_seen: execs_seen + 1,
                    children,
                }))),
                Err(error) => Err(launch_failure(
                    owner,
                    receipt,
                    with_child_detach_result(error, children.take()),
                )),
            },
            Err(error) => Err(launch_failure(owner, receipt, error)),
        }
    }

    /// Cancel while seized, before any terminal outcome. Driven
    /// fork children are detached first (killed only if a detach
    /// fails); the leader is then cancelled.
    pub fn cancel(mut self) -> std::result::Result<CleanupState, Box<ChildOwner<'fd>>> {
        detach_children_best_effort(self.children.take());
        self.owner.cancel()
    }
}

/// One caught successor exec, stopped at the new image's kernel
/// entry with the exec return drained and the original entry
/// context captured. The caller's hook performs the stopped-image
/// re-arm work (map, filter, handover, admission) and returns the
/// stop; only then does the supervisor restore the entry context
/// and let the guest run.
#[must_use = "retain the successor stop through re-arm, release, or cancellation"]
pub struct SuccessorStop<'fd> {
    task: Stopped,
    context: EntryContext,
    boundary: ExecBoundary,
    owner: ChildOwner<'fd>,
    receipt: CleanupReceipt,
    pid: safeptrace::Pid,
    started: Instant,
    execs_seen: u64,
    children: ChildSet,
}

impl std::fmt::Debug for SuccessorStop<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuccessorStop")
            .field("pid", &self.pid)
            .field("former_tid", &self.boundary.former_tid)
            .field("execs_seen", &self.execs_seen)
            .field("children", &self.children.0.len())
            .field("owner", &self.owner)
            .field("cleanup", &self.receipt.snapshot())
            .finish()
    }
}

impl<'fd> SuccessorStop<'fd> {
    /// The actual child identity.
    pub fn pid(&self) -> safeptrace::Pid {
        self.pid
    }

    /// Former tid from the caught kernel exec event.
    pub fn former_tid(&self) -> safeptrace::Pid {
        self.boundary.former_tid
    }

    /// Inspect the successor's original entry and initial stack.
    pub fn entry(&self) -> &EntryContext {
        &self.context
    }

    /// Inspect the exact exec boundary observations.
    pub fn boundary(&self) -> &ExecBoundary {
        &self.boundary
    }

    /// Same-generation shared signal/observation handle; it cannot wait here.
    pub fn cancellation_handle(&self) -> safeptrace::TerminalCleanup {
        self.owner.cancellation_handle()
    }

    /// Take the original child's parent pipe endpoints once, before
    /// releasing guest execution.
    pub fn take_parent_stdio(&mut self) -> Option<reverie::process::ParentStdio> {
        self.owner._request.take_parent_stdio()
    }

    /// Run the caller's re-arm hook on the stopped successor, restore
    /// the exact original entry context, and resume the guest still
    /// seized for the next exec. The hook owns mapping, filter
    /// installation, handover, admission and every readiness check;
    /// this transition proves only that the hook returned the stop
    /// and the original context (unchanged guest stack included)
    /// restored exactly. Any hook or restore error fails the launch
    /// closed with the child cancelled.
    pub fn rearm_and_resume<F>(
        self,
        hook: F,
    ) -> std::result::Result<WatchedSupervisor<'fd>, LaunchFailure<'fd>>
    where
        F: FnOnce(Stopped, &EntryContext, &ExecBoundary) -> Result<Stopped>,
    {
        let Self {
            task,
            context,
            boundary,
            owner,
            receipt,
            pid,
            started,
            execs_seen,
            mut children,
        } = self;
        let result = hook(task, &context, &boundary).and_then(|task| {
            context.restore(&task)?;
            Ok(task.resume(None)?)
        });
        match result {
            Ok(running) => Ok(WatchedSupervisor {
                pid,
                started,
                owner,
                running: Some(running),
                receipt,
                pre_first_exec: false,
                group_stop_clear_pending: false,
                execs_seen,
                children,
            }),
            Err(error) => Err(launch_failure(
                owner,
                receipt,
                with_child_detach_result(error, children.take()),
            )),
        }
    }

    /// Run the caller's re-arm hook, then take the production
    /// release transition: restore the exact original entry context
    /// and detach at the kernel entry. The guest runs unsupervised
    /// afterwards; retain the result through its terminal wait.
    pub fn release_detached<F>(
        self,
        hook: F,
    ) -> std::result::Result<DetachedSupervisor<'fd>, LaunchFailure<'fd>>
    where
        F: FnOnce(Stopped, &EntryContext, &ExecBoundary) -> Result<Stopped>,
    {
        let Self {
            task,
            context,
            boundary,
            owner,
            receipt,
            pid,
            started,
            execs_seen: _,
            mut children,
        } = self;
        if let Err(error) = detach_children(children.take()) {
            return Err(launch_failure(owner, receipt, error));
        }
        let former_tid = boundary.former_tid;
        // Detach with SIGCONT, like the dormant path. Group stops
        // encountered while watched were already cleared while
        // seized, so this detach signal is not what clears their
        // natural-parent reports. It flushes a stop signal queued
        // while this successor stop was held; if the guest blocks
        // SIGCONT, the detach SIGCONT stays pending in the new image.
        let result = hook(task, &context, &boundary)
            .and_then(|task| super::release_restored(task, &context, Some(Signal::SIGCONT)));
        match result {
            Ok(running) => Ok(DetachedSupervisor {
                pid,
                former_tid,
                spawn_to_after_detach: started.elapsed(),
                started,
                owner,
                running: Some(running),
                receipt,
            }),
            Err(error) => Err(launch_failure(owner, receipt, error)),
        }
    }

    /// Cancel while the successor is stopped, before any release.
    /// Driven fork children are detached first (killed only if a
    /// detach fails); the leader is then cancelled.
    pub fn cancel(mut self) -> std::result::Result<CleanupState, Box<ChildOwner<'fd>>> {
        detach_children_best_effort(self.children.take());
        self.owner.cancel()
    }

    /// Read the successor's original input bytes from this actual
    /// stopped generation, exactly as
    /// [`super::InitialExecStop::capture_initial_inputs`] does for a
    /// first image. No resume capability escapes.
    pub fn capture_initial_inputs(&self) -> Result<super::initial_inputs::CapturedInitialInputs> {
        super::initial_inputs::capture_initial_inputs_parts(&self.task, &self.context, &self.owner)
    }

    /// Assemble copied original inputs for this successor stop,
    /// exactly as [`super::InitialExecStop::prepare_copied_inputs`]
    /// does for a first image. Returns no handoff or entry
    /// capability.
    #[cfg(feature = "loader-private-runtime")]
    pub fn prepare_copied_inputs(
        &self,
        captured: &super::initial_inputs::CapturedInitialInputs,
        image: &reverie_loader::image::ValidatedImage<'_>,
        request: &super::CopiedInputRequest,
    ) -> Result<super::CopiedInitialData> {
        super::input_data::prepare_copied_inputs_parts(
            &self.task,
            &self.context,
            &self.owner,
            captured,
            image,
            request,
        )
    }

    /// Crate-private decomposition into stopped-image parts. The
    /// public hook surface always keeps the stop whole; only the
    /// physical-preparation entry in this crate may split it, and
    /// the parts are rechecked against each other there.
    #[cfg(feature = "loader-private-runtime")]
    pub(crate) fn into_parts(
        mut self,
    ) -> (
        Stopped,
        EntryContext,
        ExecBoundary,
        ChildOwner<'fd>,
        Result<()>,
    ) {
        // The prepared-mapping path owns only the leader stop; fork
        // children of the old image are detached here rather than
        // left seized with nobody left to wait them. The detach
        // result travels with the parts so the preparation entry
        // can fail with it instead of discarding it.
        let detach = detach_children(self.children.take());
        (self.task, self.context, self.boundary, self.owner, detach)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use super::*;

    fn request(program: &str, arguments: &[&str]) -> PreparedExec<'static> {
        let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        PreparedExec::new(Path::new(program), &arguments, &[], None, Vec::new()).unwrap()
    }

    fn fork_guest() -> &'static Path {
        static GUEST: OnceLock<PathBuf> = OnceLock::new();
        GUEST.get_or_init(|| {
            let directory = std::env::temp_dir().join(format!(
                "reverie-supervisor-fork-guest-{}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).unwrap();
            let source = directory.join("guest.c");
            let program = directory.join("guest");
            fs::write(&source, FORK_GUEST_C).unwrap();
            let output = std::process::Command::new("cc")
                .args([
                    "-std=gnu11",
                    "-O1",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-pthread",
                ])
                .arg(&source)
                .arg("-o")
                .arg(&program)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "fork guest cc failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            program
        })
    }

    fn fork_request(mode: &str, extra: &[&Path]) -> PreparedExec<'static> {
        let program = fork_guest();
        let mut arguments = vec![program.as_os_str().to_owned(), OsString::from(mode)];
        arguments.extend(extra.iter().map(|path| path.as_os_str().to_owned()));
        PreparedExec::new(program, &arguments, &[], None, Vec::new()).unwrap()
    }

    fn supervisor_test_file(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-supervisor-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        path
    }

    #[allow(unsafe_code)]
    fn current_tid() -> i32 {
        // SAFETY: gettid takes no arguments and cannot fail for the
        // calling thread.
        unsafe { libc::syscall(libc::SYS_gettid) as i32 }
    }

    fn traced_by_current_thread() -> Vec<i32> {
        let tid = current_tid();
        let mut traced = Vec::new();
        for entry in fs::read_dir("/proc").unwrap() {
            let Ok(entry) = entry else { continue };
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("TracerPid:")
                    && rest.trim().parse::<i32>() == Ok(tid)
                {
                    traced.push(pid);
                }
            }
        }
        traced
    }

    fn assert_no_traced_by_current_thread(context: &str) {
        let traced = traced_by_current_thread();
        assert!(
            traced.is_empty(),
            "{context}: tasks still traced by supervisor thread {}: {traced:?}",
            current_tid()
        );
    }

    fn watched_next_exec(watched: WatchedSupervisor<'_>) -> Box<SuccessorStop<'_>> {
        match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected an exec stop, got terminal {outcome:?}")
            }
        }
    }

    fn rearm_ok(stop: Box<SuccessorStop<'_>>) -> WatchedSupervisor<'_> {
        stop.rearm_and_resume(|task, _entry, _boundary| Ok(task))
            .unwrap()
    }

    const FORK_GUEST_C: &str = r#"
#define _DEFAULT_SOURCE 1
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int write_text(const char *path, const char *text) {
  FILE *file = fopen(path, "w");
  if (!file) return -1;
  int ok = fputs(text, file) >= 0;
  if (fclose(file) != 0) ok = 0;
  return ok ? 0 : -1;
}

static void close_output(void) {
  close(STDOUT_FILENO);
  close(STDERR_FILENO);
}

static int wait_status(pid_t pid) {
  int status = 0;
  if (waitpid(pid, &status, 0) != pid) return -1;
  return WIFEXITED(status) ? WEXITSTATUS(status) : -2;
}

static int state_of(pid_t pid) {
  char path[64];
  char buffer[4096];
  snprintf(path, sizeof(path), "/proc/%d/status", (int)pid);
  FILE *file = fopen(path, "r");
  if (!file) return '?';
  size_t count = fread(buffer, 1, sizeof(buffer) - 1, file);
  fclose(file);
  buffer[count] = '\0';
  char *state = strstr(buffer, "\nState:");
  if (!state) return '?';
  state += strlen("\nState:");
  while (*state == ' ' || *state == '\t') state++;
  return *state ? *state : '?';
}

static int stop_child(void) {
  pid_t pid = fork();
  if (pid == 0) {
    kill(getpid(), SIGSTOP);
    _exit(5);
  }
  if (pid < 0) return 9;
  usleep(200000);
  int state = state_of(pid);
  kill(pid, SIGCONT);
  int status = wait_status(pid);
  return (state == 'T' || state == 't') && status == 5 ? 7 : 9;
}

static int stop_child_leader_exit(const char *pid_path) {
  pid_t pid = fork();
  if (pid == 0) {
    char text[64];
    snprintf(text, sizeof(text), "%d\n", (int)getpid());
    if (write_text(pid_path, text) != 0) _exit(10);
    close_output();
    kill(getpid(), SIGSTOP);
    _exit(5);
  }
  if (pid < 0) return 9;
  usleep(200000);
  return 7;
}

static int sleep_child_exec_leader(void) {
  pid_t pid = fork();
  if (pid == 0) {
    close_output();
    usleep(700000);
    _exit(0);
  }
  if (pid < 0) return 9;
  execl("/bin/true", "true", (char *)NULL);
  return 111;
}

static int sleep_leader_exec_true(void) {
  usleep(300000);
  execl("/bin/true", "true", (char *)NULL);
  return 111;
}

static int grandchild_at_detach(void) {
  pid_t pid = fork();
  if (pid == 0) {
    close_output();
    usleep(300000);
    pid_t grandchild = fork();
    if (grandchild == 0) {
      usleep(800000);
      _exit(6);
    }
    if (grandchild < 0) _exit(10);
    _exit(wait_status(grandchild) == 6 ? 0 : 10);
  }
  if (pid < 0) return 9;
  execl("/bin/true", "true", (char *)NULL);
  return 111;
}

static volatile sig_atomic_t usr1_count;
static volatile sig_atomic_t cont_count;
static void on_usr1(int signal) { (void)signal; usr1_count++; }
static void on_cont(int signal) { (void)signal; cont_count++; }

static int signal_child(const char *pid_path, const char *result_path) {
  pid_t pid = fork();
  if (pid == 0) {
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    sigemptyset(&action.sa_mask);
    action.sa_handler = on_usr1;
    if (sigaction(SIGUSR1, &action, NULL) != 0) _exit(10);
    action.sa_handler = on_cont;
    if (sigaction(SIGCONT, &action, NULL) != 0) _exit(10);
    char text[64];
    snprintf(text, sizeof(text), "%d\n", (int)getpid());
    if (write_text(pid_path, text) != 0) _exit(10);
    while (!usr1_count && !cont_count) pause();
    snprintf(text, sizeof(text), "usr1=%d cont=%d\n", (int)usr1_count,
             (int)cont_count);
    if (write_text(result_path, text) != 0) _exit(10);
    _exit(0);
  }
  if (pid < 0) return 9;
  for (int i = 0; i < 200 && access(pid_path, F_OK) != 0; i++) usleep(5000);
  execl("/bin/true", "true", (char *)NULL);
  return 111;
}

static void *exec_true(void *unused) {
  (void)unused;
  execl("/bin/true", "true", (char *)NULL);
  _exit(112);
}

static int thread_exec_child(void) {
  pid_t pid = fork();
  if (pid == 0) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, exec_true, NULL) != 0) _exit(10);
    if (pthread_join(thread, NULL) != 0) _exit(11);
    _exit(12);
  }
  if (pid < 0) return 9;
  return wait_status(pid) == 0 ? 7 : 9;
}

static int vfork_exit(void) {
  pid_t pid = vfork();
  if (pid == 0) _exit(5);
  if (pid < 0) return 9;
  return wait_status(pid) == 5 ? 7 : 9;
}

static int vfork_exec(void) {
  pid_t pid = vfork();
  if (pid == 0) {
    execl("/bin/true", "true", (char *)NULL);
    _exit(111);
  }
  if (pid < 0) return 9;
  return wait_status(pid) == 0 ? 7 : 9;
}

int main(int argc, char **argv) {
  if (argc < 2) return 2;
  if (strcmp(argv[1], "stop-child") == 0) return stop_child();
  if (strcmp(argv[1], "stop-child-leader-exit") == 0 && argc == 3)
    return stop_child_leader_exit(argv[2]);
  if (strcmp(argv[1], "sleep-child-exec-leader") == 0)
    return sleep_child_exec_leader();
  if (strcmp(argv[1], "sleep-leader-exec-true") == 0)
    return sleep_leader_exec_true();
  if (strcmp(argv[1], "grandchild-at-detach") == 0)
    return grandchild_at_detach();
  if (strcmp(argv[1], "signal-child") == 0 && argc == 4)
    return signal_child(argv[2], argv[3]);
  if (strcmp(argv[1], "thread-exec-child") == 0) return thread_exec_child();
  if (strcmp(argv[1], "vfork-exit") == 0) return vfork_exit();
  if (strcmp(argv[1], "vfork-exec") == 0) return vfork_exec();
  return 2;
}
"#;

    #[test]
    fn supervisor_disabled_refuses_before_fork() {
        let receipt = CleanupReceipt::default();
        let failure = Supervisor::spawn(
            request("/bin/true", &["true"]),
            receipt.clone(),
            SupervisorMode::Disabled,
        )
        .unwrap_err();
        assert!(failure.error.to_string().contains("disabled"));
        assert!(!receipt.snapshot().child_created);
    }

    #[test]
    fn supervisor_catches_initial_exec_and_detaches() {
        let detached = Supervisor::spawn(
            request("/bin/true", &["true"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
        )
        .unwrap();
        assert_eq!(detached.former_tid(), detached.pid());
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert_eq!(outcome.former_tid, outcome.pid);
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn supervisor_guest_runs_unarmed_after_detach() {
        // The shell's own exec is the caught initial exec; its later
        // exec to /bin/false happens after detach, unsupervised.
        let detached = Supervisor::spawn(
            request("/bin/sh", &["sh", "-c", "exec /bin/false"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
        )
        .unwrap();
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(1));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn supervisor_missing_program_fails_before_exec_event() {
        let failure = Supervisor::spawn(
            request("/nonexistent-s2a-supervisor-target", &["absent"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
        )
        .unwrap_err();
        assert!(
            failure.error.to_string().contains("before an exec event"),
            "unexpected failure: {}",
            failure.error
        );
    }

    #[test]
    fn supervisor_parent_origin_sigstop_after_handshake_is_not_swallowed() {
        let mut injected_pid = None;
        let failure = Supervisor::spawn_inner(
            request("/bin/true", &["true"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
            &mut |_| {},
            &mut |pid| {
                injected_pid = Some(pid);
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid.as_raw()),
                    nix::sys::signal::Signal::SIGSTOP,
                )
                .unwrap();
            },
        )
        .unwrap_err();
        let pid = injected_pid.expect("initial-resume hook ran");
        assert!(
            failure.error.to_string().contains("unowned SIGSTOP"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert!(!Path::new(&format!("/proc/{}", pid.as_raw())).exists());
    }

    #[allow(unsafe_code)]
    fn sigqueue_sigcont(pid: safeptrace::Pid) {
        let value = libc::sigval {
            sival_ptr: std::ptr::null_mut(),
        };
        assert_eq!(
            unsafe { libc::sigqueue(pid.as_raw(), libc::SIGCONT, value) },
            0
        );
    }

    #[test]
    fn supervisor_foreign_sigcont_during_group_stop_clear_is_refused() {
        let mut injected_pid = None;
        let failure = Supervisor::spawn_inner(
            request("/bin/true", &["true"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
            &mut |_| std::thread::sleep(Duration::from_millis(100)),
            &mut |pid| {
                injected_pid = Some(pid);
                let pid_arg = pid.as_raw().to_string();
                assert!(
                    std::process::Command::new("/bin/kill")
                        .args(["-CONT", &pid_arg])
                        .status()
                        .unwrap()
                        .success()
                );
            },
        )
        .unwrap_err();
        let pid = injected_pid.expect("initial-resume hook ran");
        assert!(
            failure.error.to_string().contains("unowned SIGCONT"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert!(!Path::new(&format!("/proc/{}", pid.as_raw())).exists());
    }

    #[test]
    fn watched_foreign_sigcont_during_group_stop_clear_is_refused() {
        let watched = Supervisor::spawn_watched_inner(
            request("/bin/true", &["true"]),
            CleanupReceipt::default(),
            &mut |_| std::thread::sleep(Duration::from_millis(100)),
            &mut |pid| {
                let pid_arg = pid.as_raw().to_string();
                assert!(
                    std::process::Command::new("/bin/kill")
                        .args(["-CONT", &pid_arg])
                        .status()
                        .unwrap()
                        .success()
                );
            },
        )
        .unwrap();
        let pid = watched.pid();
        let failure = watched.wait_next().unwrap_err();
        assert!(
            failure.error.to_string().contains("unowned SIGCONT"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert!(!Path::new(&format!("/proc/{}", pid.as_raw())).exists());
    }

    #[test]
    fn supervisor_sigqueue_sigcont_during_group_stop_clear_is_refused() {
        let mut injected_pid = None;
        let failure = Supervisor::spawn_inner(
            request("/bin/true", &["true"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
            &mut |_| std::thread::sleep(Duration::from_millis(100)),
            &mut |pid| {
                injected_pid = Some(pid);
                sigqueue_sigcont(pid);
            },
        )
        .unwrap_err();
        let pid = injected_pid.expect("initial-resume hook ran");
        assert!(
            failure.error.to_string().contains("unowned SIGCONT"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert!(!Path::new(&format!("/proc/{}", pid.as_raw())).exists());
    }

    #[allow(unsafe_code)]
    fn pin_current_and_child(pid: safeptrace::Pid) {
        let cpu = nix::sched::sched_getcpu().unwrap();
        let mut set = nix::sched::CpuSet::new();
        set.set(cpu).unwrap();
        nix::sched::sched_setaffinity(nix::unistd::Pid::from_raw(0), &set).unwrap();
        nix::sched::sched_setaffinity(nix::unistd::Pid::from_raw(pid.as_raw()), &set).unwrap();
        // Keep the detached parent ahead of the woken child on that one
        // CPU long enough for its first wait to observe kernel state
        // before the child can process the detach SIGCONT.
        assert_eq!(
            unsafe { libc::setpriority(libc::PRIO_PROCESS, pid.as_raw().try_into().unwrap(), 19,) },
            0
        );
    }

    #[test]
    fn supervisor_group_stop_handshake_detached_wait_reaps_running_guest() {
        // Delaying seize until after the child's self-SIGSTOP forces
        // the pre-seize group-stop handshake. The detached guest then
        // sleeps long enough that a stale group-stop wait report, if
        // one remained for the natural parent, would be collected
        // before its exit rather than racing it.
        let detached = Supervisor::spawn_inner(
            request("/bin/sh", &["sh", "-c", "exec sleep 0.2"]),
            CleanupReceipt::default(),
            SupervisorMode::DormantSeize,
            &mut |_| std::thread::sleep(Duration::from_millis(100)),
            &mut pin_current_and_child,
        )
        .unwrap();
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_group_stop_release_detached_wait_reaps_running_guest() {
        let watched = Supervisor::spawn_watched_inner(
            request("/bin/sh", &["sh", "-c", "exec sleep 0.2"]),
            CleanupReceipt::default(),
            &mut |_| std::thread::sleep(Duration::from_millis(100)),
            &mut pin_current_and_child,
        )
        .unwrap();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected an exec stop, got terminal {outcome:?}")
            }
        };
        let detached = stop
            .release_detached(|task, _entry, _boundary| Ok(task))
            .unwrap();
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_guest_group_stop_release_detached_reaps_running_guest() {
        // The guest stops itself after the first exec, while still
        // seized. Its group stop must be cleared before release, or
        // the detached wait can collect the stale SIGSTOP report
        // ahead of the detach SIGCONT and kill the running sleep.
        // Pinning plus nice 19 makes that ordering deterministic.
        let watched = Supervisor::spawn_watched_inner(
            request("/bin/sh", &["sh", "-c", "kill -STOP $$; exec sleep 0.2"]),
            CleanupReceipt::default(),
            &mut |_| {},
            &mut pin_current_and_child,
        )
        .unwrap();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected the shell exec stop, got terminal {outcome:?}")
            }
        };
        let watched = stop
            .rearm_and_resume(|task, _entry, _boundary| Ok(task))
            .unwrap();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected the sleep exec stop, got terminal {outcome:?}")
            }
        };
        let detached = stop
            .release_detached(|task, _entry, _boundary| Ok(task))
            .unwrap();
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    fn rearm_until_finished(mut watched: WatchedSupervisor<'_>) -> WatchedOutcome {
        loop {
            match watched.wait_next().unwrap() {
                WatchedStep::Exec(stop) => {
                    watched = stop
                        .rearm_and_resume(|task, _entry, _boundary| Ok(task))
                        .unwrap();
                }
                WatchedStep::Finished(outcome) => return outcome,
            }
        }
    }

    #[test]
    fn watched_guest_self_sigcont_runs_to_completion() {
        let watched = Supervisor::spawn_watched(
            request("/bin/sh", &["sh", "-c", "kill -CONT $$; exec /bin/true"]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let outcome = rearm_until_finished(watched);
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_foreign_tstp_then_cont_runs_to_completion() {
        // Leader-only guest: a shell's external `sleep` would be
        // a fork-child exec, which this successor refuses by
        // name; the behavior under test is the leader's foreign
        // TSTP/CONT handling.
        let watched = Supervisor::spawn_watched(
            fork_request("sleep-leader-exec-true", &[]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected the guest exec stop, got terminal {outcome:?}")
            }
        };
        let watched = stop
            .rearm_and_resume(|task, _entry, _boundary| Ok(task))
            .unwrap();
        let pid = watched.pid().as_raw().to_string();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                std::process::Command::new("/bin/kill")
                    .args(["-TSTP", &pid])
                    .status()
                    .unwrap()
                    .success()
            );
            std::thread::sleep(Duration::from_millis(20));
            assert!(
                std::process::Command::new("/bin/kill")
                    .args(["-CONT", &pid])
                    .status()
                    .unwrap()
                    .success()
            );
        });
        let outcome = rearm_until_finished(watched);
        sender.join().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_blocked_sigcont_two_stops_run_to_completion() {
        // A single-threaded perl guest blocks SIGCONT process-wide
        // for its only thread, stops itself twice, then execs from
        // the traced leader. Both execs must be observed; a libtest
        // guest would block SIGCONT only on a non-leader thread and
        // its final exec would escape through de_thread.
        let perl = concat!(
            "use POSIX qw(:signal_h sigprocmask); ",
            "my $set = POSIX::SigSet->new(); ",
            "$set->addset(SIGCONT); ",
            "sigprocmask(SIG_BLOCK, $set) or die $!; ",
            "kill STOP => $$; ",
            "kill STOP => $$; ",
            "exec '/bin/true' or die $!;"
        );
        let watched = Supervisor::spawn_watched(
            request("/usr/bin/perl", &["perl", "-e", perl]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let outcome = rearm_until_finished(watched);
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert_eq!(outcome.execs_seen, 2);
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_guest_sees_forwarded_sigcont_handler() {
        let watched = Supervisor::spawn_watched(
            request(
                "/bin/sh",
                &[
                    "sh",
                    "-c",
                    "trap 'exit 7' CONT; kill -STOP $$; sleep 0.3; exit 0",
                ],
            ),
            CleanupReceipt::default(),
        )
        .unwrap();
        let outcome = rearm_until_finished(watched);
        assert_eq!(outcome.status, ExitStatus::Exited(7));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_fork_child_group_stop_stays_stopped_until_guest_continues() {
        // The C guest's leader forks a child that stops itself and
        // then exits without exec. Native job control: it must
        // still read stopped after the parent's sleep, and only
        // the guest parent's SIGCONT continues it. The guest
        // checks the state itself and exits 9 if the supervisor
        // auto-continued the child.
        let watched =
            Supervisor::spawn_watched(fork_request("stop-child", &[]), CleanupReceipt::default())
                .unwrap();
        let outcome = rearm_until_finished(watched);
        assert_eq!(outcome.status, ExitStatus::Exited(7));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_leader_exit_detaches_listening_fork_child_still_stopped() {
        // The fork child is parked in LISTEN when its leader
        // exits. Detach must leave it untraced but still in its
        // native group stop; only the test's later SIGCONT (a
        // guest-side continuer from the child's new parent
        // chain) lets it exit.
        let pid_file = supervisor_test_file("stop-child.pid");
        let watched = Supervisor::spawn_watched(
            fork_request("stop-child-leader-exit", &[&pid_file]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let outcome = rearm_until_finished(watched);
        assert_eq!(outcome.status, ExitStatus::Exited(7));
        assert!(outcome.cleanup.confirmed);
        assert_no_traced_by_current_thread("leader exit with listening child");
        let child_pid: i32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let status = fs::read_to_string(format!("/proc/{child_pid}/status")).unwrap();
        let state = status
            .lines()
            .find(|line| line.starts_with("State:"))
            .expect("child state");
        assert!(
            state.contains("(stopped)"),
            "detached listening child must stay stopped, got {state}"
        );
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child_pid),
            nix::sys::signal::Signal::SIGCONT,
        )
        .unwrap();
        for _ in 0..200 {
            if !Path::new(&format!("/proc/{child_pid}")).exists() {
                let _ = fs::remove_file(&pid_file);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("continued fork child {child_pid} did not exit");
    }

    #[test]
    fn watched_sh_chain_fork_child_exec_is_refused_by_name() {
        // No caller re-arm hook exists for fork-child execs yet, so
        // the inner shell's exec fails the launch closed by name
        // instead of running its new image unarmed.
        let watched = Supervisor::spawn_watched(
            request(
                "/bin/sh",
                &["sh", "-c", "sh -c 'sh -c \"exec /bin/true\"'; exit 7"],
            ),
            CleanupReceipt::default(),
        )
        .unwrap();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected the shell exec stop, got terminal {outcome:?}")
            }
        };
        let watched = stop
            .rearm_and_resume(|task, _entry, _boundary| Ok(task))
            .unwrap();
        let failure = watched.wait_next().unwrap_err();
        assert!(
            failure
                .error
                .to_string()
                .contains("exec_supervisor_fork_child_exec_refused"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
    }

    #[test]
    fn watched_exit_only_vfork_child_is_unchanged() {
        // The C guest vforks from its traced leader (main) thread.
        // Dropping the NewChild capture leaves the vfork child in
        // its initial stop and the parent blocked in vfork, so
        // this test hangs under that mutant.
        let watched =
            Supervisor::spawn_watched(fork_request("vfork-exit", &[]), CleanupReceipt::default())
                .unwrap();
        let outcome = rearm_until_finished(watched);
        assert_eq!(outcome.status, ExitStatus::Exited(7));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_vfork_child_exec_is_refused_by_name() {
        // A vfork child's exec is a fork-child exec: with no
        // per-child re-arm hook it fails closed by name rather
        // than running the new image unarmed.
        let watched =
            Supervisor::spawn_watched(fork_request("vfork-exec", &[]), CleanupReceipt::default())
                .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let failure = watched.wait_next().unwrap_err();
        assert!(
            failure
                .error
                .to_string()
                .contains("exec_supervisor_fork_child_exec_refused"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert_no_traced_by_current_thread("vfork-exec refusal");
    }

    #[test]
    fn watched_release_detached_forwards_in_flight_sigusr1_to_fork_child() {
        // While the leader successor stop is held, a SIGUSR1 is in
        // flight to the fork child. Detaching that child during
        // release must forward its own signal, not replace it
        // with a supervisor-invented SIGCONT: the child records
        // usr1=1 cont=0 (the old detach recorded usr1=0 cont=1).
        let pid_file = supervisor_test_file("signal-child.pid");
        let result_file = supervisor_test_file("signal-child.result");
        let watched = Supervisor::spawn_watched(
            fork_request("signal-child", &[&pid_file, &result_file]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let stop = watched_next_exec(watched);
        let child_pid: i32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child_pid),
            nix::sys::signal::Signal::SIGUSR1,
        )
        .unwrap();
        let detached = stop
            .release_detached(|task, _entry, _boundary| Ok(task))
            .unwrap();
        assert_no_traced_by_current_thread("release_detached signal child");
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
        let mut result = String::new();
        for _ in 0..200 {
            if let Ok(text) = fs::read_to_string(&result_file) {
                result = text;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(result.trim(), "usr1=1 cont=0");
        let _ = fs::remove_file(&pid_file);
        let _ = fs::remove_file(&result_file);
    }

    #[test]
    fn watched_release_detached_detaches_grandchild_met_while_detaching() {
        // The direct child forks a grandchild while the caller
        // holds the leader successor stop, so the grandchild is
        // first met by the detach worklist during release. It
        // must be taken over and detached too, not dropped in
        // its initial tracing stop.
        let watched = Supervisor::spawn_watched(
            fork_request("grandchild-at-detach", &[]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let stop = watched_next_exec(watched);
        std::thread::sleep(Duration::from_millis(400));
        let detached = stop
            .release_detached(|task, _entry, _boundary| Ok(task))
            .unwrap();
        assert_no_traced_by_current_thread("release_detached grandchild");
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_hook_error_detaches_fork_children() {
        let watched = Supervisor::spawn_watched(
            fork_request("sleep-child-exec-leader", &[]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let stop = watched_next_exec(watched);
        let failure = stop
            .rearm_and_resume(|_task, _entry, _boundary| Err("test hook refusal".into()))
            .unwrap_err();
        assert!(
            failure.error.to_string().contains("test hook refusal"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert_no_traced_by_current_thread("hook error");
    }

    #[test]
    fn watched_drop_successor_stop_detaches_fork_children() {
        let watched = Supervisor::spawn_watched(
            fork_request("sleep-child-exec-leader", &[]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let stop = watched_next_exec(watched);
        drop(stop);
        assert_no_traced_by_current_thread("drop successor stop");
    }

    #[test]
    fn watched_drop_supervisor_detaches_fork_children() {
        // Drive both leader execs first so the sleeping fork
        // child is captured in the supervisor's child set before
        // the drop; dropping right after the first re-arm could
        // race the leader's fork itself.
        let watched = Supervisor::spawn_watched(
            fork_request("sleep-child-exec-leader", &[]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let watched = rearm_ok(watched_next_exec(watched));
        drop(watched);
        assert_no_traced_by_current_thread("drop watched supervisor");
    }

    #[test]
    fn watched_fork_child_nonleader_thread_exec_is_named_bounded_refusal() {
        // The fork child execs from a non-leader thread, so its
        // traced leader is released by de_thread and its wait
        // becomes undrivable. The run must end with the named
        // refusal (bounded by the child detach timeout), never a
        // generic ECHILD wait failure or a hang; natively the
        // guest exits 7.
        let watched = Supervisor::spawn_watched(
            fork_request("thread-exec-child", &[]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched = rearm_ok(watched_next_exec(watched));
        let failure = watched.wait_next().unwrap_err();
        assert!(
            failure
                .error
                .to_string()
                .contains("exec_supervisor_fork_child_undrivable"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert_no_traced_by_current_thread("nonleader thread exec");
    }

    #[test]
    fn watched_supervisor_rearms_each_successor_exec() {
        let mut watched = Supervisor::spawn_watched(
            request("/bin/sh", &["sh", "-c", "exec /bin/true"]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let watched_pid = watched.pid();
        let mut execs = 0u64;
        let outcome = loop {
            match watched.wait_next().unwrap() {
                WatchedStep::Exec(stop) => {
                    execs += 1;
                    assert_eq!(stop.pid(), watched_pid);
                    assert_eq!(stop.former_tid(), watched_pid);
                    assert_eq!(stop.entry().regs().rip, stop.boundary().exit_regs.rip);
                    watched = stop
                        .rearm_and_resume(|task, _entry, _boundary| Ok(task))
                        .unwrap();
                }
                WatchedStep::Finished(outcome) => break outcome,
            }
        };
        assert_eq!(execs, 2);
        assert_eq!(outcome.execs_seen, 2);
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_supervisor_hook_error_fails_closed() {
        let watched =
            Supervisor::spawn_watched(request("/bin/true", &["true"]), CleanupReceipt::default())
                .unwrap();
        let pid = watched.pid();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected an exec stop, got terminal {outcome:?}")
            }
        };
        let failure = stop
            .rearm_and_resume(|_task, _entry, _boundary| Err("test hook refusal".into()))
            .unwrap_err();
        assert!(
            failure.error.to_string().contains("test hook refusal"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert!(!Path::new(&format!("/proc/{}", pid.as_raw())).exists());
    }

    #[test]
    fn watched_supervisor_restore_overrides_hook_register_mutation() {
        let mut watched =
            Supervisor::spawn_watched(request("/bin/true", &["true"]), CleanupReceipt::default())
                .unwrap();
        let outcome = loop {
            match watched.wait_next().unwrap() {
                WatchedStep::Exec(stop) => {
                    watched = stop
                        .rearm_and_resume(|task, _entry, _boundary| {
                            let mut regs = task.getregs()?;
                            regs.rip = 0x4141_4141;
                            task.setregs(&regs)?;
                            Ok(task)
                        })
                        .unwrap();
                }
                WatchedStep::Finished(outcome) => break outcome,
            }
        };
        assert_eq!(outcome.status, ExitStatus::Exited(0));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn watched_supervisor_forwards_guest_signal() {
        let mut watched = Supervisor::spawn_watched(
            request("/bin/sh", &["sh", "-c", "kill -TERM $$"]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let outcome = loop {
            match watched.wait_next().unwrap() {
                WatchedStep::Exec(stop) => {
                    watched = stop
                        .rearm_and_resume(|task, _entry, _boundary| Ok(task))
                        .unwrap();
                }
                WatchedStep::Finished(outcome) => break outcome,
            }
        };
        assert_eq!(outcome.execs_seen, 1);
        assert!(
            matches!(outcome.status, ExitStatus::Signaled(Signal::SIGTERM, _)),
            "unexpected status: {:?}",
            outcome.status
        );
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn successor_stop_release_detached_runs_unupervised() {
        let watched = Supervisor::spawn_watched(
            request("/bin/sh", &["sh", "-c", "exec /bin/false"]),
            CleanupReceipt::default(),
        )
        .unwrap();
        let stop = match watched.wait_next().unwrap() {
            WatchedStep::Exec(stop) => stop,
            WatchedStep::Finished(outcome) => {
                panic!("expected an exec stop, got terminal {outcome:?}")
            }
        };
        let detached = stop
            .release_detached(|task, _entry, _boundary| Ok(task))
            .unwrap();
        let outcome = detached.wait().unwrap();
        assert_eq!(outcome.status, ExitStatus::Exited(1));
        assert!(outcome.cleanup.confirmed);
    }

    #[test]
    fn initial_exec_stop_release_runs_guest() {
        use super::super::ExecStop;

        let stop =
            ExecStop::spawn(request("/bin/true", &["true"]), CleanupReceipt::default()).unwrap();
        let (running, owner) = stop.release(|task, _entry| Ok(task)).unwrap();
        assert_eq!(owner.wait(running).unwrap(), ExitStatus::Exited(0));
    }

    #[test]
    fn initial_exec_stop_release_hook_error_fails_closed() {
        use super::super::ExecStop;

        let receipt = CleanupReceipt::default();
        let stop = ExecStop::spawn(request("/bin/true", &["true"]), receipt.clone()).unwrap();
        let pid = stop.pid();
        let failure = stop
            .release(|_task, _entry| Err("test hook refusal".into()))
            .unwrap_err();
        assert!(
            failure.error.to_string().contains("test hook refusal"),
            "unexpected failure: {}",
            failure.error
        );
        assert!(failure.cleanup().confirmed);
        assert!(!Path::new(&format!("/proc/{}", pid.as_raw())).exists());
    }

    /// Preparation-re-run witness (W9 mechanical half): full physical
    /// preparation — reserve, map, filter-last, transcript-checked —
    /// run on a first image and again on a self-exec successor,
    /// cancel-terminated. This is NOT successor arming: no image
    /// entry runs, no relay is installed, no Ready or admission is
    /// claimed, and the guest is never released after preparation.
    #[cfg(all(feature = "loader-private-runtime", feature = "stage1a-probe"))]
    #[allow(unsafe_code)]
    mod preparation_rerun {
        use std::fs::File;
        use std::os::fd::AsFd;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileExt;

        use nix::fcntl::FcntlArg;
        use nix::fcntl::SealFlag;
        use nix::fcntl::fcntl;
        use nix::sys::memfd::MFdFlags;
        use nix::sys::memfd::memfd_create;
        use reverie_loader::artifact::RUNTIME_WINDOW;
        use reverie_loader::artifact::TOOL_REGION;
        use reverie_loader::handoff;
        use reverie_loader::handoff::Extent;
        use reverie_loader::handoff::View;
        use reverie_loader::image::Image;
        use reverie_loader::image::Policy;
        use reverie_loader::image::ValidatedImage;

        use super::*;
        use crate::launcher::exec_stop::CopiedInputRequest;
        use crate::launcher::exec_stop::physical::Preparation;
        use crate::launcher::exec_stop::register_words;

        const PAGE: u64 = 4096;
        const INERT_PACKET: &[u8] = b"PREPARATION_ONLY_NOT_HLIINIT1";

        fn synthetic_runtime() -> Vec<u8> {
            let base = RUNTIME_WINDOW.start;
            let mut gate = vec![0x90_u8; 4096];
            for offset in [0x80_usize, 0x90, 0xa0, 0xb0, 0x200] {
                gate[offset..offset + 2].copy_from_slice(&[0x0f, 0x05]);
            }
            let mut bytes = vec![0; 16384];
            bytes[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
            bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
            bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
            bytes[24..32].copy_from_slice(&(base + 4096).to_le_bytes());
            bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
            bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
            bytes[56..58].copy_from_slice(&7_u16.to_le_bytes());
            for (i, (kind, flags, offset, address, file, memory, align)) in [
                (1_u32, 4_u32, 0_u64, base, 4096_u64, 4096_u64, 4096_u64),
                (1, 5, 4096, base + 4096, 4096, 4096, 4096),
                (1, 6, 8192, base + 8192, 512, 4096, 4096),
                (7, 4, 8192, base + 8192, 16, 80, 8),
                (0x6474_e551, 6, 0, 0, 0, 8 * 1024 * 1024, 0),
                (6, 4, 64, base + 64, 392, 392, 8),
                (1, 4, 12288, base + 12288, 4096, 4096, 4096),
            ]
            .into_iter()
            .enumerate()
            {
                let at = 64 + i * 56;
                bytes[at..at + 4].copy_from_slice(&kind.to_le_bytes());
                bytes[at + 4..at + 8].copy_from_slice(&flags.to_le_bytes());
                for (relative, value) in [
                    (8, offset),
                    (16, address),
                    (32, file),
                    (40, memory),
                    (48, align),
                ] {
                    bytes[at + relative..at + relative + 8].copy_from_slice(&value.to_le_bytes());
                }
            }
            for (i, word) in [
                u64::from_le_bytes(*b"HLMLAY01"),
                1,
                96,
                handoff::VERSION,
                handoff::HEADER_BYTES as u64,
                handoff::MAX_BYTES as u64,
                64,
                16,
                128,
                8,
                base + 4096,
                base + 4096 + 0x7c,
            ]
            .into_iter()
            .enumerate()
            {
                bytes[512 + i * 8..520 + i * 8].copy_from_slice(&word.to_le_bytes());
            }
            bytes[1024..1034].copy_from_slice(b"/dev/null\0");
            bytes[4096..8192].copy_from_slice(&gate);
            bytes[12288..16384].copy_from_slice(&gate);
            bytes
        }

        fn descriptor_words(runtime: &[u8]) -> [u64; 12] {
            let mut words = [0_u64; 12];
            for (i, word) in words.iter_mut().enumerate() {
                *word = u64::from_le_bytes(runtime[512 + i * 8..520 + i * 8].try_into().unwrap());
            }
            words
        }

        fn memfd(bytes: &[u8], seal: bool) -> std::io::Result<File> {
            let mut file = File::from(
                memfd_create(
                    c"preparation-rerun-witness",
                    MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
                )
                .map_err(std::io::Error::from)?,
            );
            std::io::Write::write_all(&mut file, bytes)?;
            if seal {
                seal_file(&file)?;
            }
            Ok(file)
        }

        fn seal_file(file: &File) -> std::io::Result<()> {
            fcntl(
                file,
                FcntlArg::F_ADD_SEALS(
                    SealFlag::F_SEAL_WRITE
                        | SealFlag::F_SEAL_GROW
                        | SealFlag::F_SEAL_SHRINK
                        | SealFlag::F_SEAL_SEAL,
                ),
            )
            .map_err(std::io::Error::from)?;
            Ok(())
        }

        fn validated(runtime: &[u8]) -> ValidatedImage<'_> {
            let parsed = Image::parse(runtime).expect("synthetic runtime parses");
            let image = parsed
                .validate(&Policy {
                    window: RUNTIME_WINDOW,
                    allow_tls: true,
                })
                .expect("synthetic runtime validates");
            image
                .private_runtime_metadata()
                .expect("synthetic runtime metadata");
            image
        }

        fn exact_rx<'a>(
            image: &ValidatedImage<'a>,
            address: u64,
            length: usize,
        ) -> Result<&'a [u8]> {
            let end = address
                .checked_add(length as u64)
                .ok_or("RX span overflow")?;
            for load in image.segments() {
                let initialized = image
                    .mapped_file_bytes(load)
                    .map_err(|e| format!("RX load: {e:?}"))?;
                if load.permissions.read
                    && load.permissions.execute
                    && !load.permissions.write
                    && load.memory.start <= address
                    && end
                        <= load
                            .memory
                            .start
                            .checked_add(load.file.end - load.file.start)
                            .ok_or("RX end overflow")?
                {
                    let at = usize::try_from(address - load.memory.start)?;
                    return initialized
                        .get(at..at + length)
                        .ok_or_else(|| "RX file bounds".into());
                }
            }
            Err("outside initialized RX bytes".into())
        }

        fn aligned(value: u64, alignment: u64) -> u64 {
            assert!(alignment.is_power_of_two());
            (value + alignment - 1) & !(alignment - 1)
        }

        fn payload_words(
            data: &crate::launcher::exec_stop::CopiedInitialData,
            address: u64,
            count: u64,
        ) -> Result<Vec<u64>> {
            let start = usize::try_from(address - data.address())?;
            let length = usize::try_from(count * 8)?;
            Ok(data.bytes()[start..start + length]
                .as_chunks::<8>()
                .0
                .iter()
                .map(|row| u64::from_le_bytes(*row))
                .collect())
        }

        fn payload_bytes(
            data: &crate::launcher::exec_stop::CopiedInitialData,
            address: u64,
            length: u64,
        ) -> Result<&[u8]> {
            let start = usize::try_from(address - data.address())?;
            let end = start + usize::try_from(length)?;
            data.bytes()
                .get(start..end)
                .ok_or_else(|| "copied bytes bounds".into())
        }

        struct Geometry {
            roles: [Extent; 5],
            free_band: Extent,
            control: u64,
            relay: u64,
            filter_scratch: u64,
            copied_address: u64,
            copied_capacity: usize,
        }

        fn geometry(descriptor: &[u64; 12]) -> Geometry {
            let free_band = crate::guest::tool_region::scalar_free_band();
            let base = free_band.start;
            let roles = core::array::from_fn(|i| Extent {
                start: base + i as u64 * 0x10000,
                end: base + i as u64 * 0x10000 + 0x8000,
            });
            let control = roles[4].start + PAGE;
            let relay = aligned(control + descriptor[6], descriptor[9]);
            let address = roles[3].start + PAGE;
            let copied_address = address + 2 * PAGE;
            let copied_capacity = usize::try_from(roles[3].end - PAGE - copied_address).unwrap();
            Geometry {
                roles,
                free_band,
                control,
                relay,
                filter_scratch: roles[4].start + 8192,
                copied_address,
                copied_capacity,
            }
        }

        struct WireContext {
            pid: u64,
            regs: [u64; 27],
            mask: u64,
            input_fd: u64,
            pkru: u32,
        }

        // Happy-path numeric wire only, adapted from the probe's
        // preparation-only wire. It is deliberately not an admission
        // or execution-eligible handoff producer.
        fn build_wire(
            image: &ValidatedImage<'_>,
            descriptor: &[u64; 12],
            geometry: &Geometry,
            data: &crate::launcher::exec_stop::CopiedInitialData,
            context: &WireContext,
        ) -> Result<Vec<u8>> {
            let roles = &geometry.roles;
            let gate = descriptor[10];
            let address = roles[3].start + PAGE;
            let length = usize::try_from(roles[3].end - address - PAGE)?;
            assert!(length <= handoff::MAX_BYTES && length >= 3 * PAGE as usize);
            let mut bytes = vec![0; length];
            let at = usize::try_from(data.address() - address)?;
            bytes[at..at + data.bytes().len()].copy_from_slice(data.bytes());
            let metadata = data.image_metadata();
            let handler = gate + 0x100;
            assert_eq!(exact_rx(image, handler, 16)?.len(), 16);
            let dev_null = RUNTIME_WINDOW.start + 1024;
            let mut code = Vec::new();
            for load in image.segments() {
                if load.permissions.read && load.permissions.execute && !load.permissions.write {
                    code.push(handoff::Code {
                        start: load.memory.start,
                        end: load.memory.start + (load.file.end - load.file.start),
                    });
                }
            }
            let mut sites = Vec::new();
            for (offset, number, operation) in [
                (0x80_u64, 158_u64, 1_u64),
                (0x90, 218, 2),
                (0xa0, 7, 3),
                (0xb0, 2, 4),
            ] {
                let ip = gate + offset;
                assert_eq!(exact_rx(image, ip, 2)?, [0x0f, 0x05]);
                sites.push(handoff::SiteRecord {
                    syscall_ip: ip,
                    return_ip: ip + 2,
                    number,
                    operation,
                });
            }
            sites.sort_unstable_by_key(|site| site.return_ip);
            let tool_init = [gate + 0x202];
            assert_eq!(exact_rx(image, gate + 0x200, 2)?, [0x0f, 0x05]);
            let maps = data.planned_maps().to_vec();
            let locations = data.locations();
            let env = payload_words(data, locations.envp, locations.envp_words)?;
            let aux = payload_words(data, locations.raw_aux, locations.raw_aux_words)?;
            let name = payload_bytes(data, locations.program_name, locations.program_name_bytes)?;
            let tls = &metadata.tls;
            let backing = data.tls_geometry().backing();
            let mut header = handoff::Header {
                prefix: [
                    handoff::MAGIC,
                    handoff::VERSION,
                    length as u64,
                    handoff::HEADER_BYTES as u64,
                    0,
                    0,
                    0,
                    0,
                ],
                binding: [[0; 4]; 5],
                roles: *roles,
                registers: context.regs,
                scalars: [
                    context.pid,
                    geometry.control,
                    data.control_bytes(),
                    geometry.relay,
                    data.relay_bytes(),
                    context.input_fd,
                    u64::from(context.pkru),
                    context.mask,
                    data.tls_geometry().pthread().start as u64,
                    handler,
                    RUNTIME_WINDOW.start + 12288,
                    PAGE,
                    metadata.gnu_stack_bytes,
                    dev_null,
                    0,
                    0,
                ],
                tables: std::array::from_fn(|_| handoff::Table {
                    offset: 0,
                    count: 0,
                    stride: 0,
                }),
                tls: [
                    metadata.program_headers.start,
                    56,
                    u64::from(metadata.program_header_count),
                    tls.address,
                    tls.file_bytes,
                    tls.memory_bytes,
                    tls.alignment,
                    backing.start as u64,
                    backing.bytes as u64,
                    tls.load.memory.start,
                    tls.load.memory.end - tls.load.memory.start,
                    tls.load.file.end - tls.load.file.start,
                ],
            };
            let mut cursor = handoff::HEADER_BYTES as u64;
            for (index, count, stride) in [
                (0, maps.len(), 32_u64),
                (1, code.len(), 16),
                (2, sites.len(), 32),
                (3, INERT_PACKET.len(), 1),
                (7, 10, 1),
                (8, tool_init.len(), 8),
            ] {
                cursor = aligned(cursor, 8);
                header.tables[index] = handoff::Table {
                    offset: cursor,
                    count: count as u64,
                    stride,
                };
                cursor += count as u64 * stride;
            }
            assert!(cursor <= at as u64, "metadata overlaps copied payload");
            for (index, location, count, stride) in [
                (4, locations.envp, locations.envp_words, 8_u64),
                (5, locations.raw_aux, locations.raw_aux_words, 8),
                (6, locations.program_name, locations.program_name_bytes, 1),
            ] {
                header.tables[index] = handoff::Table {
                    offset: location - address,
                    count,
                    stride,
                };
            }
            let tables = handoff::TableData {
                maps: &maps,
                code: &code,
                sites: &sites,
                packet: INERT_PACKET,
                env: &env,
                aux: &aux,
                program_name: name,
                dev_null: b"/dev/null\0",
                tool_init: &tool_init,
            };
            handoff::serialize(&header, &tables, address, &mut bytes)
                .map_err(|e| format!("wire serialize: {e:?}"))?;
            reverie_loader::mapped_inputs::ProductionMappedInputs::validate_at(
                &bytes,
                address,
                reverie_loader::mapped_inputs::ConsumerLayout {
                    free_band: geometry.free_band,
                    region: Extent {
                        start: TOOL_REGION.start,
                        end: TOOL_REGION.end,
                    },
                    control_bytes: descriptor[6],
                    relay_bytes: descriptor[8],
                    control_align: descriptor[7],
                    relay_align: descriptor[9],
                },
            )
            .map_err(|e| format!("wire validate_at: {e:?}"))?;
            View::parse(&bytes, address).map_err(|e| format!("wire parse: {e:?}"))?;
            Ok(bytes)
        }

        fn watched_guest<'fd>(
            runtime_file: &'fd File,
            handoff_file: &'fd File,
            input_file: &'fd File,
        ) -> WatchedSupervisor<'fd> {
            let args: Vec<OsString> = ["sh", "-c", "exec /bin/sh -c true"]
                .iter()
                .map(OsString::from)
                .collect();
            let request = PreparedExec::new(
                Path::new("/bin/sh"),
                &args,
                &[],
                None,
                vec![
                    runtime_file.as_fd(),
                    handoff_file.as_fd(),
                    input_file.as_fd(),
                ],
            )
            .unwrap();
            Supervisor::spawn_watched(request, CleanupReceipt::default()).unwrap()
        }

        fn next_exec(watched: WatchedSupervisor<'_>) -> SuccessorStop<'_> {
            match watched.wait_next().unwrap() {
                WatchedStep::Exec(stop) => *stop,
                WatchedStep::Finished(outcome) => {
                    panic!("expected an exec stop, got terminal {outcome:?}")
                }
            }
        }

        fn status_u64(status: &str, key: &str) -> u64 {
            status
                .lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line[key.len()..].trim().parse().ok())
                .unwrap_or_else(|| panic!("status key {key} missing"))
        }

        // Full physical preparation on this stop, then cancel. The
        // guest is never released after preparation.
        fn prepare_and_cancel<'fd>(
            stop: SuccessorStop<'fd>,
            runtime: &[u8],
            descriptor: &[u64; 12],
            geometry: &Geometry,
            runtime_file: &'fd File,
            handoff_file: &'fd File,
            input_file: &'fd File,
        ) {
            let original_regs = register_words(stop.entry().regs());
            let original_mask = stop.entry().signal_mask();
            let original_entry = trace::read(&stop.task, stop.entry().regs().rip, 16).unwrap();
            let image = validated(runtime);
            let captured = stop.capture_initial_inputs().unwrap();
            let copied = stop
                .prepare_copied_inputs(
                    &captured,
                    &image,
                    &CopiedInputRequest {
                        roles: geometry.roles,
                        free_band: geometry.free_band,
                        layout_address: RUNTIME_WINDOW.start + 512,
                        copied_address: geometry.copied_address,
                        copied_capacity: geometry.copied_capacity,
                        control_address: geometry.control,
                        relay_address: geometry.relay,
                        filter_scratch: geometry.filter_scratch,
                    },
                )
                .unwrap();
            // SAFETY: EntryContext owns the complete unchanged real
            // ptrace XSAVE capture for this held generation (same
            // obligation the probe documents for this read).
            let pkru = unsafe { stop.entry().xstate().pkru() }.expect("stopped PKRU");
            let wire = build_wire(
                &image,
                descriptor,
                geometry,
                &copied,
                &WireContext {
                    pid: stop.pid().as_raw() as u64,
                    regs: original_regs,
                    mask: original_mask,
                    input_fd: input_file.as_raw_fd() as u64,
                    pkru,
                },
            )
            .unwrap();
            let role_bytes = geometry.roles[3].end - geometry.roles[3].start - 2 * PAGE;
            assert_eq!(role_bytes, (wire.len() as u64).div_ceil(PAGE) * PAGE);
            handoff_file.write_all_at(&wire, 0).unwrap();
            handoff_file.set_len(role_bytes).unwrap();
            seal_file(handoff_file).unwrap();
            let preparation = Preparation {
                runtime_fd: runtime_file.as_fd(),
                runtime_bytes: runtime,
                layout_address: RUNTIME_WINDOW.start + 512,
                handoff_fd: handoff_file.as_fd(),
                handoff_bytes: &wire,
                handoff_address: geometry.roles[3].start + PAGE,
                input_fd: input_file.as_fd(),
                free_band: geometry.free_band,
                filter_scratch: geometry.filter_scratch,
            };
            // SAFETY: original ptracer thread, inherited immutable
            // descriptors retained by the launch request, genuine
            // captured context of this stop, outer test bounds. No
            // image/guest entry is called; preparation only.
            let prepared = match unsafe { stop.prepare_mapping(preparation) } {
                Ok(prepared) => prepared,
                Err(failure) => panic!(
                    "preparation refused: {} cleanup={:?}",
                    failure.error,
                    failure.cleanup()
                ),
            };
            let observed = prepared.diagnostic_observation().unwrap();
            assert_eq!(observed.registers, original_regs);
            assert_eq!(observed.signal_mask, original_mask);
            assert!(observed.xstate_exact);
            assert!(observed.stack_exact);
            assert_eq!(observed.entry_bytes, original_entry);
            assert!(observed.runtime_contents_exact);
            assert!(observed.handoff_exact);
            assert_eq!(status_u64(&observed.status, "Seccomp:"), 2);
            assert_eq!(status_u64(&observed.status, "NoNewPrivs:"), 1);
            let cancelled = match prepared.cancel() {
                Ok(cancelled) => cancelled,
                Err(retained) => panic!("preparation cancel unconfirmed: {:?}", retained.cleanup()),
            };
            assert!(cancelled.cleanup.confirmed);
            assert_eq!(cancelled.filter_bytes.len(), 23 * 8);
            let last = cancelled.injected_calls.last().expect("injected calls");
            assert_eq!(last.0, libc::SYS_seccomp as u64);
            assert_eq!(last.2, 0);
            assert!(
                cancelled
                    .injected_calls
                    .iter()
                    .any(|call| call.0 == libc::SYS_mmap as u64),
                "preparation transcript must contain real mappings"
            );
        }

        #[test]
        fn preparation_rerun_first_image() {
            let runtime = synthetic_runtime();
            let descriptor = descriptor_words(&runtime);
            let geometry = geometry(&descriptor);
            let runtime_file = memfd(&runtime, true).unwrap();
            let handoff_file = memfd(&[], false).unwrap();
            let input_file = memfd(INERT_PACKET, true).unwrap();
            let watched = watched_guest(&runtime_file, &handoff_file, &input_file);
            let stop = next_exec(watched);
            prepare_and_cancel(
                stop,
                &runtime,
                &descriptor,
                &geometry,
                &runtime_file,
                &handoff_file,
                &input_file,
            );
        }

        #[test]
        fn preparation_rerun_self_exec_successor() {
            let runtime = synthetic_runtime();
            let descriptor = descriptor_words(&runtime);
            let geometry = geometry(&descriptor);
            let runtime_file = memfd(&runtime, true).unwrap();
            let handoff_file = memfd(&[], false).unwrap();
            let input_file = memfd(INERT_PACKET, true).unwrap();
            let mut watched = watched_guest(&runtime_file, &handoff_file, &input_file);
            // Release the first image UNPREPARED so it can self-exec;
            // a prepared image has no release transition in this
            // slice (cancel-only), which is the named residual gap
            // for a one-lineage prepared->exec->prepared chain.
            let first = next_exec(watched);
            let pid = first.pid();
            watched = first
                .rearm_and_resume(|task, _entry, _boundary| Ok(task))
                .unwrap();
            let successor = next_exec(watched);
            assert_eq!(successor.pid(), pid);
            assert_eq!(successor.former_tid(), pid);
            prepare_and_cancel(
                successor,
                &runtime,
                &descriptor,
                &geometry,
                &runtime_file,
                &handoff_file,
                &input_file,
            );
        }

        #[test]
        fn release_after_ready_mismatch_returns_unspent_mapping() {
            use crate::launcher::exec_stop::physical::VerifiedReady;

            let runtime = synthetic_runtime();
            let descriptor = descriptor_words(&runtime);
            let geometry = geometry(&descriptor);
            let runtime_file = memfd(&runtime, true).unwrap();
            let handoff_file = memfd(&[], false).unwrap();
            let input_file = memfd(INERT_PACKET, true).unwrap();
            let watched = watched_guest(&runtime_file, &handoff_file, &input_file);
            let stop = next_exec(watched);
            let pid = stop.pid();
            let original_regs = register_words(stop.entry().regs());
            let original_mask = stop.entry().signal_mask();
            let image = validated(&runtime);
            let captured = stop.capture_initial_inputs().unwrap();
            let copied = stop
                .prepare_copied_inputs(
                    &captured,
                    &image,
                    &CopiedInputRequest {
                        roles: geometry.roles,
                        free_band: geometry.free_band,
                        layout_address: RUNTIME_WINDOW.start + 512,
                        copied_address: geometry.copied_address,
                        copied_capacity: geometry.copied_capacity,
                        control_address: geometry.control,
                        relay_address: geometry.relay,
                        filter_scratch: geometry.filter_scratch,
                    },
                )
                .unwrap();
            // SAFETY: same held-generation XSAVE read as the
            // preparation witnesses above.
            let pkru = unsafe { stop.entry().xstate().pkru() }.expect("stopped PKRU");
            let wire = build_wire(
                &image,
                &descriptor,
                &geometry,
                &copied,
                &WireContext {
                    pid: pid.as_raw() as u64,
                    regs: original_regs,
                    mask: original_mask,
                    input_fd: input_file.as_raw_fd() as u64,
                    pkru,
                },
            )
            .unwrap();
            let role_bytes = geometry.roles[3].end - geometry.roles[3].start - 2 * PAGE;
            handoff_file.write_all_at(&wire, 0).unwrap();
            handoff_file.set_len(role_bytes).unwrap();
            seal_file(&handoff_file).unwrap();
            let preparation = Preparation {
                runtime_fd: runtime_file.as_fd(),
                runtime_bytes: &runtime,
                layout_address: RUNTIME_WINDOW.start + 512,
                handoff_fd: handoff_file.as_fd(),
                handoff_bytes: &wire,
                handoff_address: geometry.roles[3].start + PAGE,
                input_fd: input_file.as_fd(),
                free_band: geometry.free_band,
                filter_scratch: geometry.filter_scratch,
            };
            // SAFETY: original ptracer thread, inherited immutable
            // descriptors, genuine captured context, as in
            // prepare_and_cancel. Preparation only; the mismatched
            // release below refuses before any restore or entry.
            let mut prepared = match unsafe { stop.prepare_mapping(preparation) } {
                Ok(prepared) => prepared,
                Err(failure) => panic!(
                    "preparation refused: {} cleanup={:?}",
                    failure.error,
                    failure.cleanup()
                ),
            };
            // Name an exact initialized stop, then present a token for
            // another pid naming that same stop. Only the release
            // call site's owner-pid argument can refuse this; passing
            // the token's own pid would wrongly proceed to restore.
            let terminal = RUNTIME_WINDOW.start + 8192;
            let reserved = 1_u64 << (libc::SIGSYS - 1);
            prepared.set_initialized_stop_for_test(Some((terminal, reserved)));
            let wrong = VerifiedReady::for_test(
                safeptrace::Pid::from_raw(pid.as_raw() + 1),
                terminal,
                reserved,
            );
            let tokio = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            // SAFETY: the token names the exact recorded stop but
            // another child, so release must refuse at its recheck
            // before any physical change.
            let failure = tokio.block_on(async {
                match unsafe {
                    prepared
                        .release_after_ready(
                            wrong,
                            std::time::Instant::now() + std::time::Duration::from_secs(30),
                        )
                        .await
                } {
                    Ok(_) => panic!("mismatched release unexpectedly detached"),
                    Err(failure) => failure,
                }
            });
            assert!(
                failure
                    .error
                    .to_string()
                    .contains("mapped_ready_token_mismatch"),
                "unexpected release refusal: {}",
                failure.error
            );
            let mut mapping = failure.into_mapping();
            assert_eq!(
                mapping.handover_flags_for_test(),
                (false, false),
                "mismatch must leave failed and handover_started unset"
            );
            // Keep the stop-mismatch case as well: present a token
            // for the correct pid after clearing the recorded stop.
            // Only the call site's initialized-stop argument can
            // refuse this; passing the token's own stop would proceed.
            mapping.set_initialized_stop_for_test(None);
            let wrong_stop = VerifiedReady::for_test(pid, terminal, reserved);
            // SAFETY: the token names this child but no recorded
            // initialized stop remains, so release must refuse at its
            // recheck before any physical change.
            let failure = tokio.block_on(async {
                match unsafe {
                    mapping
                        .release_after_ready(
                            wrong_stop,
                            std::time::Instant::now() + std::time::Duration::from_secs(30),
                        )
                        .await
                } {
                    Ok(_) => panic!("stop-mismatched release unexpectedly detached"),
                    Err(failure) => failure,
                }
            });
            assert!(
                failure
                    .error
                    .to_string()
                    .contains("mapped_ready_token_mismatch"),
                "unexpected release refusal: {}",
                failure.error
            );
            let mapping = failure.into_mapping();
            assert_eq!(
                mapping.handover_flags_for_test(),
                (false, false),
                "mismatch must leave failed and handover_started unset"
            );
            let observed = mapping.diagnostic_observation().unwrap();
            assert_eq!(observed.registers, original_regs);
            assert!(observed.xstate_exact);
            let cancelled = match mapping.cancel() {
                Ok(cancelled) => cancelled,
                Err(retained) => panic!("cancel unconfirmed: {:?}", retained.cleanup()),
            };
            assert!(cancelled.cleanup.confirmed);
        }

        #[test]
        fn preparation_parts_refuse_foreign_context() {
            use crate::launcher::exec_stop::physical::prepare_mapping_parts;

            let runtime = synthetic_runtime();
            let descriptor = descriptor_words(&runtime);
            let geometry = geometry(&descriptor);
            let runtime_file = memfd(&runtime, true).unwrap();
            let handoff_file = memfd(&[], false).unwrap();
            let input_file = memfd(INERT_PACKET, true).unwrap();
            let watched_a = watched_guest(&runtime_file, &handoff_file, &input_file);
            let stop_a = next_exec(watched_a);
            let runtime_file_b = memfd(&runtime, true).unwrap();
            let handoff_file_b = memfd(&[], false).unwrap();
            let input_file_b = memfd(INERT_PACKET, true).unwrap();
            let watched_b = watched_guest(&runtime_file_b, &handoff_file_b, &input_file_b);
            let stop_b = next_exec(watched_b);
            let (task_a, _context_a, boundary_a, owner_a, detach_a) = stop_a.into_parts();
            let (task_b, context_b, _boundary_b, owner_b, detach_b) = stop_b.into_parts();
            detach_a.unwrap();
            detach_b.unwrap();
            let preparation = Preparation {
                runtime_fd: runtime_file.as_fd(),
                runtime_bytes: &runtime,
                layout_address: RUNTIME_WINDOW.start + 512,
                handoff_fd: handoff_file.as_fd(),
                handoff_bytes: &[],
                handoff_address: geometry.roles[3].start + PAGE,
                input_fd: input_file.as_fd(),
                free_band: geometry.free_band,
                filter_scratch: geometry.filter_scratch,
            };
            // SAFETY: same obligations as the happy witness; the
            // deliberately foreign pairing must refuse before any
            // child mutation, which is the fence under test.
            let failure = match unsafe {
                prepare_mapping_parts(task_a, context_b, boundary_a, owner_a, preparation)
            } {
                Ok(_) => panic!("foreign parts unexpectedly prepared"),
                Err(failure) => failure,
            };
            assert!(
                failure
                    .error
                    .to_string()
                    .contains("mapped_preparation_foreign_parts"),
                "unexpected refusal: {}",
                failure.error
            );
            assert!(failure.cleanup().confirmed);
            drop(task_b);
            let cancelled = owner_b.cancel().unwrap();
            assert!(cancelled.confirmed);
        }
    }
}
