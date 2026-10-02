/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Native before-repair regressions for an actually consumed, unpublished stop.
//! No source state, identity, wait owner, or kernel status is installed here.

use std::io::Read;
use std::task::Wake;

use super::*;
use crate::Signal;

const COMPONENT: Duration = Duration::from_secs(3);
const CLEANUP: Duration = Duration::from_secs(2);
const SETUP: Duration = Duration::from_secs(10);
const RACE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct ConsumptionPause {
    consumed: mpsc::SyncSender<(i32, libc::pid_t)>,
    resume: mpsc::Receiver<()>,
    released: mpsc::SyncSender<PauseRelease>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PauseRelease {
    Explicit,
    Disconnected,
    TimedOut,
}

/// Called only after the actual worker's consuming wait returned this status.
/// Take the hook out of its original Event before waiting. Disconnection and
/// the unchanged two-second bound both release it, with a distinct receipt.
pub(super) fn after_consumption(event: &Event, status: i32) {
    let pause = event.consumed_publication_pause.lock().take();
    if let Some(pause) = pause {
        let result = if pause.consumed.try_send((status, gettid())).is_err() {
            PauseRelease::Disconnected
        } else {
            match pause.resume.recv_timeout(CLEANUP) {
                Ok(()) => PauseRelease::Explicit,
                Err(mpsc::RecvTimeoutError::Disconnected) => PauseRelease::Disconnected,
                Err(mpsc::RecvTimeoutError::Timeout) => PauseRelease::TimedOut,
            }
        };
        let _ = pause.released.try_send(result);
    }
}

struct HookRelease {
    sender: Option<mpsc::SyncSender<()>>,
    event: Arc<Event>,
}

impl HookRelease {
    fn release(&mut self) {
        let sender = self.sender.take().expect("one explicit hook release");
        let result = sender.try_send(());
        drop(sender);
        self.event.consumed_publication_pause.lock().take();
        result.expect("worker must still be inside its real publication gap");
    }
}

impl Drop for HookRelease {
    fn drop(&mut self) {
        // Disconnect first, even on assertion unwind, before touching the
        // installation mutex. This guard always drops before child custody.
        drop(self.sender.take());
        self.event.consumed_publication_pause.lock().take();
    }
}

struct ChildCustody {
    terminal: TerminalCleanup,
    finished: bool,
}

impl ChildCustody {
    fn try_finish(&self) -> Result<(), Errno> {
        let deadline = Instant::now() + CLEANUP;
        let signal = if self.terminal.is_reaped()? {
            Ok(())
        } else {
            self.terminal.request_sigkill()
        };
        match signal {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(error),
        }
        if !self.terminal.wait(remaining(deadline)) {
            return Err(Errno::ETIMEDOUT);
        }
        let status = self
            .terminal
            .observed_exit_status()?
            .ok_or(Errno::ENODATA)?;
        let parent = futures::executor::block_on(self.terminal.reap_parent_terminal())?;
        if !matches!(parent, ParentReap::Reaped | ParentReap::AlreadyReaped)
            || !self.terminal.is_reaped()?
        {
            return Err(Errno::EBUSY);
        }
        // WORKER_DONE precedes remove(). Require actual original registry
        // retirement too, within the SAME two-second cleanup deadline.
        loop {
            let registered = NOTIFIER
                .pids
                .lock()
                .get(&self.terminal.pid)
                .is_some_and(|entry| entry.handle == self.terminal.event);
            if !registered {
                break;
            }
            if remaining(deadline).is_zero() {
                return Err(Errno::ETIMEDOUT);
            }
            thread::sleep(Duration::from_millis(1));
        }
        if self
            .terminal
            .event
            .event()
            .worker_state
            .load(Ordering::Acquire)
            != WORKER_DONE
        {
            return Err(Errno::EBUSY);
        }
        eprintln!(
            "484 cleanup pid={}: signal={signal:?}, actual_terminal={status:?}, worker=DONE, registry_retired=true, parent={parent:?}, original_pidfd_hup=true",
            self.terminal.pid
        );
        Ok(())
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        if let Err(error) = self.try_finish() {
            eprintln!("484 exact original-child cleanup FAILED: {error}");
            // Do not discard custody, start a second cleanup deadline, or
            // reinterpret ESRCH/ECHILD as a terminal acknowledgment. The
            // child installs PDEATHSIG before TRACEME as a final backstop.
            std::process::abort();
        }
        self.finished = true;
    }
}

impl Drop for ChildCustody {
    fn drop(&mut self) {
        self.finish();
    }
}

fn gettid() -> libc::pid_t {
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn component_deadline(phase: Instant) -> Instant {
    phase.min(Instant::now() + COMPONENT)
}

fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn read_marker(fd: &OwnedFd, phase: Instant) {
    let deadline = component_deadline(phase);
    loop {
        let timeout = remaining(deadline);
        assert!(
            !timeout.is_zero(),
            "actual child execution marker timed out"
        );
        let mut pollfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut pollfd, 1, timeout.as_millis().max(1) as i32) };
        if result == -1 && Errno::last() == Errno::EINTR {
            continue;
        }
        assert_eq!(result, 1, "execution marker readiness");
        assert_eq!(pollfd.revents, libc::POLLIN, "child must remain alive");
        let mut byte = 0u8;
        assert_eq!(
            unsafe { libc::read(fd.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) },
            1
        );
        assert_eq!(byte, b'R', "actual post-resume child marker");
        return;
    }
}

fn proc_text(identity: &WorkerIdentity, name: &std::ffi::CStr) -> String {
    let fd = unsafe {
        libc::openat(
            identity.proc_dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    assert!(fd >= 0, "open original task observation: {}", Errno::last());
    let mut file = unsafe { fs::File::from_raw_fd(fd) }.take(16 * 1024 + 1);
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert!(text.len() <= 16 * 1024 && text.ends_with('\n'));
    text
}

fn task_state(identity: &WorkerIdentity, ptracer: libc::pid_t) -> char {
    let text = proc_text(identity, c"status");
    let field = |prefix| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .expect("required original proc field")
            .trim()
    };
    assert_eq!(field("TracerPid:").parse::<libc::pid_t>().unwrap(), ptracer);
    assert_eq!(field("Pid:").parse::<i32>().unwrap(), identity.pid.as_raw());
    field("State:").chars().next().unwrap()
}

fn assert_blocked_read(identity: &WorkerIdentity, ptracer: libc::pid_t, fd: i32, phase: Instant) {
    let deadline = component_deadline(phase);
    loop {
        assert!(
            identity.pidfd_is_live().unwrap(),
            "original child remains live"
        );
        let state = task_state(identity, ptracer);
        let syscall = proc_text(identity, c"syscall");
        let mut words = syscall.split_whitespace();
        let actual_call = words.next().and_then(|word| word.parse::<i64>().ok());
        let actual_fd = words
            .next()
            .and_then(|word| word.strip_prefix("0x"))
            .and_then(|word| i32::from_str_radix(word, 16).ok());
        if state == 'S' && actual_call == Some(libc::SYS_read) && actual_fd == Some(fd) {
            eprintln!(
                "484 live child blocked: state={state}, syscall={}",
                syscall.trim()
            );
            return;
        }
        assert!(
            !remaining(deadline).is_zero(),
            "original child did not block in read: state={state}, syscall={syscall:?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

struct CurrentThreadWake(thread::Thread);

impl Wake for CurrentThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn wait_on_original_thread<F: Future>(future: F, phase: Instant) -> F::Output {
    let deadline = component_deadline(phase);
    let waker = Waker::from(Arc::new(CurrentThreadWake(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
        let timeout = remaining(deadline);
        assert!(
            !timeout.is_zero(),
            "original authentic wait component timed out"
        );
        thread::park_timeout(timeout);
    }
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Raw,
    WaitStatus,
}

impl Route {
    fn alias(self, pid: Pid, consumed_raw: i32) -> Stopped {
        let wait = match self {
            Self::Raw => Wait::from_raw(pid, consumed_raw),
            Self::WaitStatus => Wait::try_from(nix::sys::wait::WaitStatus::Stopped(
                pid.into(),
                Signal::SIGSTOP,
            )),
        };
        let (stopped, event) = wait.unwrap().assume_stopped();
        assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
        assert_eq!(stopped.source_stop().map(|_| ()), Err(Errno::ENODATA));
        stopped
    }
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    Resume(Route),
    SameRegisters(Route),
}

fn actual_consumption_gap(mutation: Mutation) {
    let setup_deadline = Instant::now() + SETUP;
    let ptracer = gettid();
    let parent = unsafe { libc::getpid() };
    let (marker_read, marker_write) = pipe();
    let (block_read, block_write) = pipe();
    let child_block_fd = block_read.as_raw_fd();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            libc::close(marker_read.as_raw_fd());
            libc::close(block_write.as_raw_fd());
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                libc::_exit(121);
            }
            if libc::getppid() != parent {
                libc::_exit(122);
            }
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                libc::_exit(120);
            }
            if libc::raise(libc::SIGSTOP) != 0 {
                libc::_exit(123);
            }
            let marker = b'R';
            if libc::write(marker_write.as_raw_fd(), (&marker as *const u8).cast(), 1) != 1 {
                libc::_exit(124);
            }
            let mut byte = 0u8;
            loop {
                let result = libc::read(child_block_fd, (&mut byte as *mut u8).cast(), 1);
                if result != -1 || *libc::__errno_location() != libc::EINTR {
                    libc::_exit(125);
                }
            }
        }
    }
    let pid = Pid::from_raw(child);
    let running = Running::new(pid);
    // Own exact cleanup before registration, any real wait, and the hook.
    let mut custody = ChildCustody {
        terminal: TerminalCleanup::new_unregistered(pid, &running.1),
        finished: false,
    };
    let event = Arc::clone(running.1.event().event());
    let (consumed, observed) = mpsc::sync_channel(1);
    let (resume, release_wait) = mpsc::sync_channel(1);
    let (released, release_receipt) = mpsc::sync_channel(1);
    *event.consumed_publication_pause.lock() = Some(ConsumptionPause {
        consumed,
        resume: release_wait,
        released,
    });
    let mut release = HookRelease {
        sender: Some(resume),
        event: Arc::clone(&event),
    };
    drop(marker_write);
    drop(block_read);
    custody
        .terminal
        .ensure_registered()
        .expect("authentic notifier registration");
    let (raw, worker_tid) = observed
        .recv_timeout(remaining(component_deadline(setup_deadline)))
        .expect("actual worker nonterminal consumption receipt");
    let race_deadline = Instant::now() + RACE;
    assert!(!remaining(setup_deadline).is_zero(), "setup bound");
    assert_eq!(raw, (libc::SIGSTOP << 8) | 0x7f);
    assert_ne!(worker_tid, ptracer, "actual dedicated notifier worker");
    assert_eq!(
        event.wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NOTIFIER
    );
    assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_RUNNING);
    assert!(event.status.lock().pending.is_empty(), "not yet published");
    let identity = Arc::clone(custody.terminal.event.identity().unwrap());
    assert_eq!(
        task_state(&identity, ptracer),
        't',
        "original real ptrace stop"
    );
    assert_eq!(gettid(), ptracer, "control stays on the original ptracer");
    match mutation {
        Mutation::None => {}
        Mutation::Resume(route) => {
            let alias = route.alias(pid, raw);
            assert!(Arc::ptr_eq(alias.1.event().event(), &event));
            let result = alias.resume_retaining(None);
            eprintln!(
                "484 {mutation:?}: original_ptracer_tid={ptracer}, control_tid={}, actual_resume={result:?}",
                gettid()
            );
            let alias_running = result.expect("actual same-ptracer raw resume must succeed");
            assert!(Arc::ptr_eq(alias_running.1.event().event(), &event));
            read_marker(&marker_read, race_deadline);
            assert_blocked_read(&identity, ptracer, child_block_fd, race_deadline);
        }
        Mutation::SameRegisters(route) => {
            let alias = route.alias(pid, raw);
            assert!(Arc::ptr_eq(alias.1.event().event(), &event));
            let regs = alias.getregs().expect("actual original stopped registers");
            let result = alias.setregs(&regs);
            eprintln!(
                "484 {mutation:?}: original_ptracer_tid={ptracer}, control_tid={}, actual_setregs={result:?}",
                gettid()
            );
            result.expect("actual same-ptracer same-value register write must succeed");
            let after = alias.getregs().expect("actual register readback");
            assert_eq!(
                format!("{after:?}"),
                format!("{regs:?}"),
                "all register fields unchanged"
            );
            assert_eq!(task_state(&identity, ptracer), 't');
        }
    }
    assert_eq!(gettid(), ptracer);
    assert_eq!(release_receipt.try_recv(), Err(mpsc::TryRecvError::Empty));
    release.release();
    assert_eq!(
        release_receipt.recv_timeout(remaining(component_deadline(race_deadline))),
        Ok(PauseRelease::Explicit),
        "hook timeout/disconnection is not an intended regression outcome"
    );
    let (stopped, actual_event) = wait_on_original_thread(running.wait_owned(), race_deadline)
        .expect("authentic original FIFO wait must succeed")
        .assume_stopped();
    assert_eq!(gettid(), ptracer);
    assert_eq!(stopped.pid(), pid);
    assert_eq!(actual_event, crate::Event::Signal(Signal::SIGSTOP));
    assert!(Arc::ptr_eq(stopped.1.event().event(), &event));
    if matches!(mutation, Mutation::Resume(_)) {
        assert_blocked_read(&identity, ptracer, child_block_fd, race_deadline);
    } else {
        assert_eq!(task_state(&identity, ptracer), 't');
    }
    let source_result = stopped.source_stop();
    let source_outcome = source_result.as_ref().map(|_| ()).map_err(|error| *error);
    eprintln!(
        "484 {mutation:?}: pid={pid}, original_ptracer_tid={ptracer}, worker_tid={worker_tid}, consumed_raw={raw:#x}, authentic_wait=SIGSTOP, same_original_event=true, source_stop={source_outcome:?}"
    );
    if let Mutation::None = mutation {
        let source = source_result.expect("genuine unchanged stop must issue source");
        source.validate_current().unwrap();
        assert!(source.same_generation(&custody.terminal));
        let acquisition = source
            .begin_acquisition()
            .expect("genuine original acquisition");
        let regs = acquisition
            .with_stopped(|view| view.getregs())
            .expect("original ptracer authenticated capture")
            .expect("actual kernel register capture");
        eprintln!("484 positive: authentic source/acquisition/getregs succeeded: {regs:?}");
        acquisition.finish_binding();
        source.validate_current().unwrap();
    }
    drop(release); // Every hook is gone before retained-pidfd kill or retirement.
    custody.finish(); // MUST precede the final expected-refusal assertion.
    drop(block_write);
    if !matches!(mutation, Mutation::None) {
        assert_eq!(
            source_outcome,
            Err(Errno::EPERM),
            "intervening successful raw control must not authenticate the old consumed stop"
        );
    }
}

#[test]
fn unchanged_consumed_stop_authenticates_source() {
    actual_consumption_gap(Mutation::None);
}

#[test]
fn raw_resume_before_publication_refuses_old_source() {
    actual_consumption_gap(Mutation::Resume(Route::Raw));
}

#[test]
fn waitstatus_resume_before_publication_refuses_old_source() {
    actual_consumption_gap(Mutation::Resume(Route::WaitStatus));
}

#[test]
fn raw_setregs_before_publication_refuses_old_source() {
    actual_consumption_gap(Mutation::SameRegisters(Route::Raw));
}

#[test]
fn waitstatus_setregs_before_publication_refuses_old_source() {
    actual_consumption_gap(Mutation::SameRegisters(Route::WaitStatus));
}
