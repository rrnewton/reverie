/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Timer events end to end: `Guest::set_timer` and
//! `Guest::set_timer_precise` through the production CPL3 SYSCALL transport
//! to `Tool::handle_timer_event`, under the Reverie contract. These tests
//! require KVM and the raw PMU event; unavailable hardware is not a pass.
//! A precise event must land exactly at its target, at the instruction
//! boundary right after the target branch, with the same registers in every
//! run.
//!
//! The guests run a loop of 3 RCBs per iteration far past each target, by at
//! least [`TAIL`] RCBs, so a kick has time to land however late its
//! interrupt comes; the guest's next subscribed syscall would otherwise
//! cancel the request. Every delivery is checked against a ceiling as well as
//! the target: it comes less than [`CEILING`] RCBs after the target. A test
//! that expects no event runs the same tail after the event that should
//! cancel the request, and each has a control without that event.

#![cfg(target_arch = "x86_64")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use futures::FutureExt;
use reverie::BackendStatsRequest;
use reverie::BackendStatsSource;
use reverie::CpuIdResult;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::ParkedObservationLease;
use reverie::Pid;
use reverie::ProcessAlarmSignalOutcome;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Signal;
use reverie::SignalEvent;
use reverie::SignalTarget;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::ArchPrctl;
use reverie::syscalls::ArchPrctlCmd;
use reverie::syscalls::Errno;
use reverie::syscalls::Execve;
use reverie::syscalls::ExitGroup;
use reverie::syscalls::Fork;
use reverie::syscalls::Getpid;
use reverie::syscalls::RtSigprocmask;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie::syscalls::Tgkill;
use reverie_kvm::KvmBackend;
use serde::Deserialize;
use serde::Serialize;

const MEMORY: usize = 16 * 1024 * 1024;

/// RCBs each guest runs past its last target: far beyond the longest kick
/// latency measured on a loaded host (about 320,000 RCBs).
const TAIL: u64 = 30_000_000;

/// How far past its target an event may come: three times that latency, and
/// a thirtieth of [`TAIL`].
const CEILING: u64 = 1_000_000;

/// Checks that `clock` lies in the delivery window of a request for `ticks`
/// RCBs made at clock `requested_at`: at or past its target, and less than
/// [`CEILING`] past it.
fn assert_delivered(clock: u64, requested_at: u64, ticks: u64, context: &str) {
    let target = requested_at + ticks;
    assert!(
        (target..target + CEILING).contains(&clock),
        "{context}: delivered at {clock}, target {target}"
    );
}

/// Runs `test` again in a child process with a time limit, as read_clock.rs
/// does, and checks that the child ran exactly that test. Returns false in
/// the child, which then runs the test body.
fn bounded(test: &str) -> bool {
    bounded_with(test, &[])
}

/// [`bounded`], with `env` set for the child.
fn bounded_with(test: &str, env: &[(&str, &str)]) -> bool {
    if std::env::var_os("REVERIE_KVM_TIMER_TEST_CHILD").is_some() {
        return false;
    }
    let output = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "120s"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("REVERIE_KVM_TIMER_TEST_CHILD", "1")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{test}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// A static guest assembled from `body`, as in read_clock.rs, or a guest
/// compiled from C, and the guest memory it runs in.
struct Program {
    directory: PathBuf,
    executable: PathBuf,
    memory: usize,
}

impl Program {
    fn new(body: &str) -> Self {
        Self::build(
            "guest.S",
            &format!(".global _start\n.text\n_start:\n{body}\n"),
            &["-nostdlib", "-static", "-Wl,--build-id=none"],
            MEMORY,
        )
    }

    /// A dynamically linked guest compiled from the C `source` with `flags`,
    /// with room for its loader and libraries.
    fn c(source: &str, flags: &[&str]) -> Self {
        Self::build(
            "guest.c",
            source,
            &[&["-O2"], flags].concat(),
            256 * 1024 * 1024,
        )
    }

    fn build(name: &str, text: &str, flags: &[&str], memory: usize) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "reverie-kvm-timer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let source = directory.join(name);
        let executable = directory.join("guest");
        std::fs::write(&source, text).unwrap();
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=2s", "10s", "/usr/bin/gcc"])
            .args(flags)
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            directory,
            executable,
            memory,
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

/// A loop of at least `rcbs` RCBs, 3 per iteration (see read_clock.rs).
fn branches(rcbs: u64) -> String {
    let iterations = rcbs.div_ceil(3);
    format!(
        "mov ${iterations}, %rcx\n1: xor %eax,%eax\njz 2f\nud2\n2: jnz 3f\n3: dec %rcx\njnz 1b\n"
    )
}

const GETPPID: &str = "mov $110,%eax\nsyscall\n";
const EXIT: &str = "xor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n";

/// Where a [`TimerTool`] requests its timer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Arm {
    /// In `handle_thread_start`, before the initial exec.
    ThreadStart,
    /// In `handle_post_exec`: at the start of each image.
    #[default]
    PostExec,
    /// In the callback of the guest's `getppid`, the marker.
    Marker,
    /// In the callback of the guest's `execve`, before it runs.
    Execve,
}

/// An injection that a timer event's callback cannot perform on KVM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Refused {
    Fork,
    Exec,
    /// `tgkill` of SIGUSR1 to the thread itself.
    Signal,
    /// A tail injection other than an exit.
    Tail,
}

/// What a timer event's callback does after it polls a refused injection
/// once and drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum AfterDropped {
    /// Tail-inject an exit.
    Exit,
    /// Retire the thread.
    Retire,
    /// Poll a getpid injection once, report whether it completed, and return.
    Inject,
}

/// A signal-mask change a timer event's callback injects while SIGUSR1 is
/// blocked and pending. The guest keeps the address of SIGUSR1's mask in
/// r14, with SIGUSR2's mask after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum MaskChange {
    /// Unblock SIGUSR1, which would make it deliverable.
    UnblockPending,
    /// Block SIGUSR2, which makes nothing deliverable.
    BlockAnother,
    /// Unblock SIGUSR1 with a sigset size of 4: `EINVAL`.
    BadSize,
    /// Unblock SIGUSR1 with an invalid `how`: `EINVAL`.
    BadHow,
    /// Unblock the signals of a mask at address 8, which the guest cannot
    /// read: `EFAULT`.
    BadSet,
}

impl MaskChange {
    fn request(self, masks: u64) -> RtSigprocmask {
        let (how, set, size) = match self {
            Self::UnblockPending => (libc::SIG_UNBLOCK, masks, 8),
            Self::BlockAnother => (libc::SIG_BLOCK, masks + 8, 8),
            Self::BadSize => (libc::SIG_UNBLOCK, masks, 4),
            Self::BadHow => (99, masks, 8),
            Self::BadSet => (libc::SIG_UNBLOCK, 8, 8),
        };
        RtSigprocmask::new()
            .with_how(how)
            .with_set(Addr::from_raw(set as usize))
            .with_oldset(None)
            .with_sigsetsize(size)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Plan {
    arm: Arm,
    /// RCB counts requested there, in order: each replaces the one before.
    requests: Vec<u64>,
    /// Requests made again from `handle_timer_event`, for the last count.
    rearm: u32,
    /// Subscribe to RDTSC.
    rdtsc: bool,
    /// Subscribe to CPUID.
    cpuid: bool,
    /// Leave `tgkill` unsubscribed, so that its signal is the only event.
    quiet_tgkill: bool,
    /// Subscribe to no syscall at all.
    no_syscalls: bool,
    /// End the run from the timer event.
    exit_at_event: bool,
    /// Request precise events (`Guest::set_timer_precise`).
    precise: bool,
    /// From the callback of the guest's `getpid`, request this many RCBs.
    rearm_at_getpid: Option<u64>,
    /// From a signal's callback, request this many RCBs.
    rearm_at_signal: Option<u64>,
    /// Ask for a precise event with an instruction offset, and for 0 RCBs
    /// precisely and imprecisely, after the requests.
    refusals: bool,
    /// Inject this from the timer event, ignore the error, and carry on.
    refused_at_event: Option<Refused>,
    /// Poll that injection once instead, drop it, and do this.
    after_dropped: Option<AfterDropped>,
    /// Inject this from the timer event, report its result, and carry on.
    mask_change: Option<MaskChange>,
    /// Report the registers the timer event sees.
    registers: bool,
    /// At the marker, after the request, queue a process SIGALRM and observe
    /// it while the marker's callback stays parked.
    parked_signal: bool,
    /// At the marker, return ERESTARTSYS from the first attempt of the
    /// callback, after the request; the attempt that follows requests again
    /// only if this is `Some(true)`.
    restart: Option<bool>,
    /// Hold the first timer event's callback until another thread's `tgkill`
    /// has run, and hold that `tgkill` until the callback has begun, as a
    /// Tool that runs one thread at a time would (see [`Gate`]).
    rendezvous: bool,
    /// Deliver every signal, and report the clock at its callback.
    deliver: bool,
    /// Subscribe to `tgkill` alone.
    only_tgkill: bool,
    /// From SIGSEGV's callback, send SIGUSR1 to the thread itself.
    self_signal_at_fault: bool,
    /// Report the registers each signal's callback sees.
    signal_registers: bool,
    /// Hold a new thread's start until another thread's `tgkill` has run.
    hold_thread_start: bool,
    /// How many `tgkill`s must have run before [`Gate::Sent`] opens, under
    /// [`Plan::rendezvous`] or [`Plan::hold_thread_start`]: 0 means 1.
    sends: u32,
    /// Take the backend's signal control and grant a delivery permit at every
    /// boundary that has something to select, as a scheduler that runs one
    /// thread at a time does, logging each permit and its receipt.
    permits: bool,
    /// From SIGUSR1's callback, set the first segment base to the address in
    /// r13, and from SIGUSR2's, the second to the address in r14, each with an
    /// injected `arch_prctl`.
    bases_at_signals: Option<[SegmentBase; 2]>,
}

/// A segment base that a signal's callback sets under
/// [`Plan::bases_at_signals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum SegmentBase {
    Fs,
    Gs,
}

/// The two points a [`Plan::rendezvous`] orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Gate {
    /// The first timer event's callback has begun.
    Timer,
    /// The other thread's `tgkill` has run.
    Sent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Requested {
        clock: u64,
        ticks: u64,
    },
    Refused(String),
    Timer {
        clock: u64,
    },
    Signal(i32),
    /// A signal's callback under [`Plan::deliver`], and the clock there.
    Signalled {
        signal: i32,
        clock: u64,
    },
    /// Opens a gate (not logged).
    Open(Gate),
    /// Waits until a gate is open (not logged).
    Wait(Gate),
    /// One more `tgkill` has run; [`Gate::Sent`] opens once this many have
    /// (not logged).
    Sent(u32),
    /// A delivery permit granted under [`Plan::permits`], by its sequence.
    Permit(u64),
    /// The receipt that settled a permit: its sequence and its outcome.
    Receipt(u64, String),
    Rdtsc,
    Cpuid,
    /// An attempt of the marker's callback.
    Attempt(u32),
    /// The registers a timer event saw.
    Registers(Registers),
    /// The result of an injection from a timer event.
    Injected(String),
}

/// The user registers a timer event's callback sees (`Guest::regs`), and the
/// thread's pid as the Tool sees it (`Guest::pid`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Registers {
    pid: u64,
    rip: u64,
    rax: u64,
    orig_rax: u64,
    rcx: u64,
    r11: u64,
    eflags: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    r10: u64,
    r8: u64,
    r9: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    rsp: u64,
    rbx: u64,
    rbp: u64,
    r15: u64,
    cs: u64,
    ss: u64,
    ds: u64,
    es: u64,
    fs: u64,
    gs: u64,
    fs_base: u64,
    gs_base: u64,
}

#[derive(Default)]
struct Log {
    events: Mutex<Vec<Event>>,
    timer: Latch,
    sent: Latch,
    /// The `tgkill`s [`Event::Sent`] has counted.
    sends: std::sync::atomic::AtomicU32,
    /// [`Plan::permits`], and the signal control it takes.
    permits: bool,
    control: Mutex<Option<reverie::BackendSignalControl>>,
    /// The last permit's sequence.
    permit: AtomicU64,
}

/// A gate that opens once and stays open.
#[derive(Default)]
struct Latch(Mutex<(bool, Vec<std::task::Waker>)>);

impl Latch {
    fn open(&self) {
        let mut latch = self.0.lock().unwrap();
        latch.0 = true;
        latch.1.drain(..).for_each(std::task::Waker::wake);
    }

    async fn wait(&self) {
        futures::future::poll_fn(|context| {
            let mut latch = self.0.lock().unwrap();
            if latch.0 {
                return std::task::Poll::Ready(());
            }
            latch.1.push(context.waker().clone());
            std::task::Poll::Pending
        })
        .await
    }
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = Plan;
    type Request = Event;
    type Response = ();

    async fn init_global_state(plan: &Plan) -> Self {
        Self {
            permits: plan.permits,
            ..Self::default()
        }
    }

    fn install_backend_signal_control(
        &self,
        control: Option<reverie::BackendSignalControl>,
    ) -> Result<reverie::BackendSignalControlMode, reverie::Error> {
        if !self.permits {
            return Ok(reverie::BackendSignalControlMode::Unchanged);
        }
        *self.control.lock().unwrap() = Some(control.ok_or(Errno::ENOSYS)?);
        Ok(reverie::BackendSignalControlMode::ToolControlled)
    }

    fn skip_empty_signal_boundaries(&self) -> bool {
        self.permits
    }

    fn authorize_backend_signal_boundary(
        &self,
        task: reverie::SignalTaskIdentity,
    ) -> Result<Option<reverie::SignalDeliveryPermit>, reverie::Error> {
        let Some(control) = self.control.lock().unwrap().clone() else {
            return Ok(None);
        };
        let sequence = self.permit.fetch_add(1, Ordering::SeqCst) + 1;
        let permit = reverie::SignalDeliveryPermit {
            task,
            sequence,
            site: None,
        };
        control.process.reserve_delivery(permit)?;
        self.events.lock().unwrap().push(Event::Permit(sequence));
        Ok(Some(permit))
    }

    async fn on_backend_signal_boundary(
        &self,
        receipt: reverie::SignalBoundaryReceipt,
    ) -> Result<(), reverie::Error> {
        let outcome = format!("{:?}", receipt.outcome);
        self.events
            .lock()
            .unwrap()
            .push(Event::Receipt(receipt.permit.sequence, outcome));
        Ok(())
    }

    async fn receive_rpc(&self, _from: Pid, event: Event) {
        let latch = |gate| match gate {
            Gate::Timer => &self.timer,
            Gate::Sent => &self.sent,
        };
        match event {
            Event::Open(gate) => latch(gate).open(),
            Event::Wait(gate) => latch(gate).wait().await,
            Event::Sent(needed) => {
                if self.sends.fetch_add(1, Ordering::SeqCst) + 1 >= needed {
                    self.sent.open();
                }
            }
            event => self.events.lock().unwrap().push(event),
        }
    }
}

#[derive(Default)]
struct TimerTool;

/// Each thread's counts of its timer events and of its marker attempts.
#[derive(Default, Serialize, Deserialize)]
struct Counts {
    delivered: u32,
    attempts: u32,
}

/// A process SIGALRM, as the kernel would queue it.
fn alarm(pid: Pid) -> SignalEvent {
    let mut info = [0; reverie::SIGNAL_INFO_SIZE];
    info[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
    info[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
    SignalEvent::new(libc::SIGALRM, info, SignalTarget::Process { pid }).unwrap()
}

/// Injects `refused` from a timer event's callback, which cannot perform it.
async fn inject_refused<G: Guest<TimerTool>>(
    guest: &mut G,
    refused: Refused,
) -> Result<i64, Errno> {
    let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
    match refused {
        Refused::Fork => guest.inject(Fork::new()).await,
        Refused::Exec => guest.inject(Execve::new()).await,
        Refused::Signal => {
            guest
                .inject(
                    Tgkill::new()
                        .with_tgid(pid)
                        .with_tid(tid)
                        .with_sig(libc::SIGUSR1),
                )
                .await
        }
        Refused::Tail => guest.tail_inject(Getpid::new()).await,
    }
}

/// Reports the user registers and the pid a callback sees.
async fn report_registers<G: Guest<TimerTool>>(guest: &mut G) {
    let r = guest.regs().await;
    let registers = Registers {
        pid: guest.pid().as_raw() as u64,
        rip: r.rip,
        rax: r.rax,
        orig_rax: r.orig_rax,
        rcx: r.rcx,
        r11: r.r11,
        eflags: r.eflags,
        rdi: r.rdi,
        rsi: r.rsi,
        rdx: r.rdx,
        r10: r.r10,
        r8: r.r8,
        r9: r.r9,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        rsp: r.rsp,
        rbx: r.rbx,
        rbp: r.rbp,
        r15: r.r15,
        cs: r.cs,
        ss: r.ss,
        ds: r.ds,
        es: r.es,
        fs: r.fs,
        gs: r.gs,
        fs_base: r.fs_base,
        gs_base: r.gs_base,
    };
    guest.send_rpc(Event::Registers(registers)).await;
}

/// Requests a timer event `ticks` RCBs on, precise if the plan says so.
async fn arm<G: Guest<TimerTool>>(guest: &mut G, ticks: u64) {
    let clock = guest.read_clock().unwrap();
    let schedule = TimerSchedule::Rcbs(ticks);
    if guest.config().precise {
        guest.set_timer_precise(schedule).unwrap();
    } else {
        guest.set_timer(schedule).unwrap();
    }
    guest.send_rpc(Event::Requested { clock, ticks }).await;
}

async fn request<G: Guest<TimerTool>>(guest: &mut G) {
    let plan = guest.config().clone();
    for ticks in plan.requests {
        arm(guest, ticks).await;
    }
    if plan.refusals {
        let schedule = TimerSchedule::RcbsAndInstructions(1_000, 3);
        let refused = match guest.set_timer_precise(schedule) {
            Err(reverie::Error::Io(error)) => format!("{:?}: {error}", error.kind()),
            other => format!("{other:?}"),
        };
        guest.send_rpc(Event::Refused(refused)).await;
        let refused = guest.set_timer_precise(TimerSchedule::Rcbs(0));
        guest.send_rpc(Event::Refused(format!("{refused:?}"))).await;
        let refused = guest.set_timer(TimerSchedule::Rcbs(0));
        guest.send_rpc(Event::Refused(format!("{refused:?}"))).await;
    }
}

#[reverie::tool]
impl Tool for TimerTool {
    type GlobalState = Log;
    type ThreadState = Counts;

    fn may_observe_signal_dequeues(plan: &Plan) -> bool {
        plan.parked_signal
    }

    fn subscriptions(plan: &Plan) -> Subscription {
        if plan.no_syscalls {
            return Subscription::none();
        }
        let mut subscription = Subscription::all_syscalls();
        if plan.rdtsc {
            subscription.rdtsc();
        }
        if plan.cpuid {
            subscription.cpuid();
        }
        if plan.quiet_tgkill {
            subscription.disable_syscall(Sysno::tgkill);
        }
        if plan.only_tgkill {
            subscription = Subscription::none();
            subscription.syscall(Sysno::tgkill);
        }
        subscription
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        if guest.config().arm == Arm::ThreadStart {
            request(guest).await;
        }
        if guest.config().hold_thread_start && guest.tid() != guest.pid() {
            guest.send_rpc(Event::Wait(Gate::Sent)).await;
        }
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        if guest.config().arm == Arm::PostExec {
            request(guest).await;
        }
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let plan = guest.config().clone();
        match (plan.arm, syscall.number()) {
            (Arm::Marker, Sysno::getppid) => {
                let attempt = guest.thread_state().attempts;
                guest.thread_state_mut().attempts += 1;
                guest.send_rpc(Event::Attempt(attempt)).await;
                match (plan.restart, attempt) {
                    (Some(_), 0) => {
                        request(guest).await;
                        return Err(Errno::ERESTARTSYS.into());
                    }
                    (Some(false), _) => {}
                    _ => request(guest).await,
                }
                if plan.parked_signal {
                    let site = guest.parked_signal_site().expect("a parked callback site");
                    let pid = guest.pid();
                    let queued = guest.queue_process_alarm_signal(alarm(pid)).await;
                    assert!(
                        matches!(queued, ProcessAlarmSignalOutcome::Accepted(_)),
                        "{queued:?}"
                    );
                    guest
                        .observe_parked_signal(site, ParkedObservationLease { nonce: 1 })
                        .await
                        .unwrap();
                }
            }
            (Arm::Execve, Sysno::execve) => request(guest).await,
            (_, Sysno::tgkill) if plan.rendezvous || plan.hold_thread_start => {
                if plan.rendezvous {
                    guest.send_rpc(Event::Wait(Gate::Timer)).await;
                }
                let sent = guest.inject(syscall).await;
                guest.send_rpc(Event::Sent(plan.sends.max(1))).await;
                return Ok(sent?);
            }
            (_, Sysno::getpid) if plan.rearm_at_getpid.is_some() => {
                arm(guest, plan.rearm_at_getpid.unwrap()).await
            }
            _ => {}
        }
        guest.tail_inject(syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        let plan = guest.config().clone();
        if plan.deliver {
            let clock = guest.read_clock().unwrap();
            let signal = signal as i32;
            guest.send_rpc(Event::Signalled { signal, clock }).await;
        } else {
            guest.send_rpc(Event::Signal(signal as i32)).await;
        }
        if plan.signal_registers {
            report_registers(guest).await;
        }
        if let Some(bases) = plan.bases_at_signals {
            let registers = guest.regs().await;
            let set = match signal {
                Signal::SIGUSR1 => Some((bases[0], registers.r13)),
                Signal::SIGUSR2 => Some((bases[1], registers.r14)),
                _ => None,
            };
            if let Some((base, address)) = set {
                let command = match base {
                    SegmentBase::Fs => ArchPrctlCmd::ARCH_SET_FS(address),
                    SegmentBase::Gs => ArchPrctlCmd::ARCH_SET_GS(address),
                };
                let set = guest.inject(ArchPrctl::new().with_cmd(command)).await;
                assert_eq!(set, Ok(0), "{base:?} {address:#x}");
            }
        }
        if plan.self_signal_at_fault && signal == Signal::SIGSEGV {
            let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
            let sent = Tgkill::new()
                .with_tgid(pid)
                .with_tid(tid)
                .with_sig(libc::SIGUSR1);
            assert_eq!(guest.inject(sent).await, Ok(0));
        }
        if let Some(ticks) = plan.rearm_at_signal {
            arm(guest, ticks).await;
        }
        let suppressed = signal == Signal::SIGUSR1 || signal == Signal::SIGALRM;
        Ok((plan.deliver || !suppressed).then_some(signal))
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, Errno> {
        guest.send_rpc(Event::Rdtsc).await;
        Ok(RdtscResult::new(request))
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, Errno> {
        guest.send_rpc(Event::Cpuid).await;
        let host = std::arch::x86_64::__cpuid_count(eax, ecx);
        Ok(CpuIdResult {
            eax: host.eax,
            ebx: host.ebx,
            ecx: host.ecx,
            edx: host.edx,
        })
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let clock = guest.read_clock().unwrap();
        guest.send_rpc(Event::Timer { clock }).await;
        let plan = guest.config().clone();
        if plan.registers {
            report_registers(guest).await;
        }
        if plan.rendezvous && guest.thread_state().delivered == 0 {
            guest.send_rpc(Event::Open(Gate::Timer)).await;
            guest.send_rpc(Event::Wait(Gate::Sent)).await;
        }
        if let Some(change) = plan.mask_change {
            let masks = guest.regs().await.r14;
            let result = guest.inject(change.request(masks)).await;
            guest.send_rpc(Event::Injected(format!("{result:?}"))).await;
            return;
        }
        if let Some(refused) = plan.refused_at_event {
            let Some(after) = plan.after_dropped else {
                // Each of these ends the run; a Tool that ignores the error,
                // as this one does, cannot carry on as if the syscall had
                // failed.
                let _ignored = inject_refused(guest, refused).await;
                return;
            };
            // Polled once and dropped, the refused injection still ends the
            // run, and what the callback does next neither replaces that
            // outcome nor reaches the guest.
            let _dropped = inject_refused(guest, refused).now_or_never();
            match after {
                AfterDropped::Exit => guest.tail_inject(ExitGroup::new().with_status(0)).await,
                AfterDropped::Retire => guest.retire_current_thread().await,
                AfterDropped::Inject => {
                    let later = guest.inject(Getpid::new()).now_or_never();
                    guest.send_rpc(Event::Injected(format!("{later:?}"))).await;
                }
            }
            return;
        }
        let counts = guest.thread_state_mut();
        counts.delivered += 1;
        if counts.delivered <= plan.rearm {
            let ticks = *plan.requests.last().unwrap();
            guest.set_timer(TimerSchedule::Rcbs(ticks)).unwrap();
            guest.send_rpc(Event::Requested { clock, ticks }).await;
            // A syscall the Tool injects is not an event: the request stays.
            assert!(guest.inject(Getpid::new()).await.unwrap() > 0);
        } else if plan.exit_at_event {
            guest.tail_inject(ExitGroup::new().with_status(0)).await
        }
    }
}

/// How a run ended: the guest's exit code, stdout and stderr, or the run's
/// failure, as text.
type Ending = Result<(i32, Vec<u8>, Vec<u8>), String>;

/// Runs `program` under a [`TimerTool`] following `plan`, and returns the
/// Tool's events and how the run ended. The image is installed from its file,
/// which `/proc/self/exe` names.
/// The entries set up within a precise request's stepping window in the
/// last [`run_ending`] (`KvmBackendStats::timer_window_entries`): none means
/// that no step, run to a breakpoint or unstepped entry toward a precise
/// target began in that run.
static WINDOW_ENTRIES: AtomicU64 = AtomicU64::new(0);

fn run_ending(program: &Program, plan: Plan) -> (Vec<Event>, Ending) {
    let mut backend = KvmBackend::new(program.memory).unwrap();
    backend.set_backend_stats_request(BackendStatsRequest::ENABLED);
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&program.executable).unwrap(),
            &[program.executable.to_str().unwrap()],
            &[],
            &program.directory,
        )
        .unwrap();
    let completion = futures::executor::block_on(
        backend.run_static_elf_with_tool_completion::<TimerTool>(plan, true),
    )
    .unwrap();
    let events = completion.global_state.events.into_inner().unwrap();
    WINDOW_ENTRIES.store(
        backend.backend_stats().timer_window_entries(),
        Ordering::Relaxed,
    );
    let ending = completion
        .result
        .map(|(status, out, err)| {
            let code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().expect("a signaled status"));
            (code, out, err)
        })
        .map_err(|error| error.to_string());
    (events, ending)
}

/// Runs `program` under a [`TimerTool`] following `plan`, checks that it
/// exits with status 0 and no output, and returns the Tool's events.
fn run(program: &Program, plan: Plan) -> Vec<Event> {
    let (events, ending) = run_ending(program, plan);
    assert_eq!(ending, Ok((0, vec![], vec![])), "{events:?}");
    events
}

fn timer_clocks(events: &[Event]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Timer { clock } => Some(*clock),
            _ => None,
        })
        .collect()
}

/// The clock at which the last request before the first timer event (or
/// the last request at all) was made, and its count.
fn last_request(events: &[Event]) -> (u64, u64) {
    let before_timer = events
        .iter()
        .take_while(|event| !matches!(event, Event::Timer { .. }));
    before_timer
        .filter_map(|event| match event {
            Event::Requested { clock, ticks } => Some((*clock, *ticks)),
            _ => None,
        })
        .last()
        .expect("a request was made")
}

fn skid_margin() -> u64 {
    reverie::pmu_config::try_get_pmu_config()
        .expect("the preemption timer requires this CPU's PMU profile")
        .skid_margin()
}

#[test]
fn a_timer_event_arrives_once_at_or_after_its_target() {
    if bounded("a_timer_event_arrives_once_at_or_after_its_target") {
        return;
    }
    let lead = 1_000;
    for ticks in [1, 2, 5, skid_margin(), 1_000_000, 100_000_000] {
        // Requested where the image starts, and in the callback of a syscall.
        let at_start = Program::new(&(branches(ticks + TAIL) + EXIT));
        let at_marker = Program::new(&(branches(lead) + GETPPID + &branches(ticks + TAIL) + EXIT));
        for (arm, program) in [(Arm::PostExec, &at_start), (Arm::Marker, &at_marker)] {
            let events = run(
                program,
                Plan {
                    arm,
                    requests: vec![ticks],
                    ..Plan::default()
                },
            );
            let (requested_at, _) = last_request(&events);
            let clocks = timer_clocks(&events);
            assert_eq!(clocks.len(), 1, "{arm:?} {ticks}: {events:?}");
            assert_delivered(
                clocks[0],
                requested_at,
                ticks,
                &format!("{arm:?} {ticks}: {events:?}"),
            );
            println!(
                "{arm:?}: requested {ticks} RCBs at {requested_at}, delivered at {}",
                clocks[0]
            );
        }
    }
}

#[test]
fn a_timer_event_reaches_a_guest_that_never_exits() {
    if bounded("a_timer_event_reaches_a_guest_that_never_exits") {
        return;
    }
    // One taken JZ per iteration, forever: no syscall, no fault.
    let program = Program::new("1: xor %eax,%eax\njz 1b\nud2\n");
    let events = run(
        &program,
        Plan {
            requests: vec![1_000_000],
            exit_at_event: true,
            ..Plan::default()
        },
    );
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    assert_delivered(clocks[0], 0, 1_000_000, &format!("{events:?}"));
}

#[test]
fn a_new_request_replaces_the_outstanding_one() {
    if bounded("a_new_request_replaces_the_outstanding_one") {
        return;
    }
    let program = Program::new(&(branches(100_000_000 + TAIL) + EXIT));
    for requests in [
        // A nearer request replaces a farther one, which then never fires.
        vec![100_000_000, 1_000_000],
        // A farther one replaces a nearer one, which then never fires.
        vec![1_000_000, 30_000_000],
    ] {
        let events = run(
            &program,
            Plan {
                requests: requests.clone(),
                ..Plan::default()
            },
        );
        let clocks = timer_clocks(&events);
        assert_eq!(clocks.len(), 1, "{requests:?}: {events:?}");
        let (requested_at, ticks) = last_request(&events);
        assert_eq!(ticks, requests[1]);
        assert_delivered(
            clocks[0],
            requested_at,
            ticks,
            &format!("{requests:?}: {events:?}"),
        );
    }
}

#[test]
fn another_event_cancels_an_outstanding_request() {
    if bounded("another_event_cancels_an_outstanding_request") {
        return;
    }
    let ticks = 1_000_000;
    // getpid and gettid (kept in r12 and r13), then the marker, where the
    // request is made, then the event under test, then the tail.
    let program = |event: &str| {
        Program::new(
            &("mov $39,%eax\nsyscall\nmov %rax,%r12\nmov $186,%eax\nsyscall\nmov %rax,%r13\n"
                .to_owned()
                + &branches(1_000)
                + GETPPID
                + event
                + &branches(ticks + TAIL)
                + EXIT),
        )
    };
    let tgkill = "mov %r12,%rdi\nmov %r13,%rsi\nmov $10,%edx\nmov $234,%eax\nsyscall\n";
    let plan = |rdtsc, cpuid, quiet_tgkill| Plan {
        arm: Arm::Marker,
        requests: vec![ticks],
        rdtsc,
        cpuid,
        quiet_tgkill,
        ..Plan::default()
    };
    for (name, event, plan, witness) in [
        (
            "a subscribed syscall",
            "mov $39,%eax\nsyscall\n",
            plan(false, false, false),
            None,
        ),
        (
            "a signal",
            tgkill,
            plan(false, false, true),
            Some(Event::Signal(libc::SIGUSR1)),
        ),
        (
            "RDTSC",
            "rdtsc\n",
            plan(true, false, false),
            Some(Event::Rdtsc),
        ),
        (
            "CPUID",
            "xor %eax,%eax\nxor %ecx,%ecx\ncpuid\n",
            plan(false, true, false),
            Some(Event::Cpuid),
        ),
    ] {
        let events = run(&program(event), plan.clone());
        assert_eq!(timer_clocks(&events), [0; 0], "{name}: {events:?}");
        if let Some(witness) = witness {
            assert!(events.contains(&witness), "{name}: {events:?}");
        }
        // The control: without the event, the request fires.
        let events = run(&program(""), plan);
        let clocks = timer_clocks(&events);
        assert_eq!(clocks.len(), 1, "{name} control: {events:?}");
        let (requested_at, _) = last_request(&events);
        assert_delivered(
            clocks[0],
            requested_at,
            ticks,
            &format!("{name}: {events:?}"),
        );
    }
}

#[test]
fn an_exec_cancels_an_outstanding_request() {
    if bounded("an_exec_cancels_an_outstanding_request") {
        return;
    }
    let ticks = 1_000_000;
    // The first image executes itself with two arguments; the second, seeing
    // them, runs the tail.
    let program = Program::new(&format!(
        "cmpq $2, (%rsp)\nje after_exec\nlea self_path(%rip), %rdi\nlea arguments(%rip), %rsi\n\
         lea environment(%rip), %rdx\nmov $59, %eax\nsyscall\nneg %eax\nmov %eax, %edi\n\
         mov $231, %eax\nsyscall\nud2\nafter_exec:\n{}{EXIT}\
         .data\nself_path: .asciz \"/proc/self/exe\"\nfirst: .asciz \"timer\"\n\
         second: .asciz \"again\"\n.balign 8\narguments: .quad first, second, 0\n\
         environment: .quad 0\n",
        branches(ticks + TAIL)
    ));
    // Requested in the execve callback: the exec cancels it.
    let events = run(
        &program,
        Plan {
            arm: Arm::Execve,
            requests: vec![ticks],
            ..Plan::default()
        },
    );
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    assert_eq!(last_request(&events).1, ticks, "{events:?}");
    // Requested at thread start, before the initial exec, which cancels it.
    let tail = Program::new(&(branches(ticks + TAIL) + EXIT));
    let events = run(
        &tail,
        Plan {
            arm: Arm::ThreadStart,
            requests: vec![ticks],
            ..Plan::default()
        },
    );
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    // The control: requested at the start of each image, it fires in the
    // second (the execve's syscall event cancels the first image's request).
    let events = run(
        &program,
        Plan {
            requests: vec![ticks],
            ..Plan::default()
        },
    );
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    let (requested_at, _) = last_request(&events);
    assert_delivered(clocks[0], requested_at, ticks, &format!("{events:?}"));
}

#[test]
fn a_timer_event_can_request_the_next_one() {
    if bounded("a_timer_event_can_request_the_next_one") {
        return;
    }
    let ticks = 1_000_000;
    let rearm = 4;
    let program = Program::new(&(branches(u64::from(rearm + 1) * (ticks + TAIL)) + EXIT));
    let events = run(
        &program,
        Plan {
            requests: vec![ticks],
            rearm,
            ..Plan::default()
        },
    );
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), rearm as usize + 1, "{events:?}");
    assert_delivered(clocks[0], 0, ticks, &format!("{events:?}"));
    for pair in clocks.windows(2) {
        assert_delivered(pair[1], pair[0], ticks, &format!("{events:?}"));
    }
}

#[test]
fn instruction_offsets_and_zero_counts_are_refused() {
    if bounded("instruction_offsets_and_zero_counts_are_refused") {
        return;
    }
    let ticks = 1_000_000;
    let program = Program::new(&(branches(ticks + TAIL) + EXIT));
    let events = run(
        &program,
        Plan {
            requests: vec![ticks],
            refusals: true,
            ..Plan::default()
        },
    );
    let refusals: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Refused(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(refusals.len(), 3, "{events:?}");
    assert!(
        refusals[0].starts_with("Unsupported: ")
            && refusals[0].contains("TimerSchedule::RcbsAndInstructions"),
        "{events:?}"
    );
    assert_eq!(refusals[1..], ["Err(Errno(EINVAL))"; 2], "{events:?}");
    // Neither refusal disturbed the outstanding request.
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    assert_delivered(clocks[0], 0, ticks, &format!("{events:?}"));
}

#[test]
fn a_timer_event_reaches_a_guest_making_syscalls_the_tool_does_not_see() {
    if bounded("a_timer_event_reaches_a_guest_making_syscalls_the_tool_does_not_see") {
        return;
    }
    // A Tool that subscribes to no syscall, and a guest that makes 50,000
    // getpid calls, 4 RCBs apart, then the tail. The i-th call's exit is at
    // clock 4i + 3, so the first at or past the target of 100,000 is at
    // 100,003. A kick lands about 100 RCBs after its overflow, after that
    // call has left user mode (to land before it, it would have to come
    // within 3 RCBs), so the request comes due at that exit and is delivered
    // at that syscall's return. Left for a kick that finds the guest in user
    // mode instead, it would wait for a rare early kick or for the last call.
    //
    // Before the loop, the guest keeps getpid's result in r13, its stack
    // pointer in r12 and the address after the loop's syscall in r14, and
    // puts known values in the six argument registers, which the calls leave
    // as they are.
    let ticks = 100_000;
    let calls = 50_000;
    let due_exit = ticks + 3;
    let program = Program::new(&format!(
        "mov $39,%eax\nsyscall\nmov %rax,%r13\nmov %rsp,%r12\nlea 7f(%rip),%r14\n\
         mov $0x1111,%edi\nmov $0x2222,%esi\nmov $0x3333,%edx\n\
         mov $0x4444,%r10d\nmov $0x5555,%r8d\nmov $0x6666,%r9d\n\
         mov ${calls}, %r15\n4: mov $3, %rbx\n5: dec %rbx\njnz 5b\n\
         mov $39,%eax\nsyscall\n7: dec %r15\njnz 4b\n{}{EXIT}",
        branches(TAIL)
    ));
    let events = run(
        &program,
        Plan {
            requests: vec![ticks],
            no_syscalls: true,
            registers: true,
            ..Plan::default()
        },
    );
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    assert_eq!(
        clocks[0], due_exit,
        "target {ticks}, due at the exit at {due_exit}: {events:?}"
    );
    // The callback sees the registers the guest resumes with at that
    // syscall's return: rip at the instruction after the syscall, which is
    // where SYSCALL left rcx, r11 holding the flags SYSCALL saved, rax the
    // call's result, and the arguments and the stack pointer as the guest
    // left them. orig_rax reads -1.
    let r = events
        .iter()
        .find_map(|event| match event {
            Event::Registers(registers) => Some(registers),
            _ => None,
        })
        .expect("the timer event reported its registers");
    assert_ne!(r.pid, 0, "{r:#x?}");
    assert_eq!(r.r13, r.pid, "getpid's result before the loop: {r:#x?}");
    assert_eq!((r.rip, r.rcx), (r.r14, r.r14), "rip and rcx: {r:#x?}");
    assert_eq!(r.rax, r.pid, "rax, getpid's result: {r:#x?}");
    assert_eq!(r.r11, r.eflags, "r11, the flags SYSCALL saved: {r:#x?}");
    assert_eq!(
        [r.rdi, r.rsi, r.rdx, r.r10, r.r8, r.r9],
        [0x1111, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666],
        "argument registers: {r:#x?}"
    );
    assert_eq!(r.rsp, r.r12, "rsp: {r:#x?}");
    assert_eq!(r.orig_rax, u64::MAX, "orig_rax: {r:#x?}");
    println!(
        "requested {ticks} RCBs during {calls} unsubscribed syscalls, delivered at {} with {r:#x?}",
        clocks[0]
    );
}

#[test]
fn a_timer_callback_cannot_inject_a_process_action_a_signal_or_a_tail() {
    if bounded("a_timer_callback_cannot_inject_a_process_action_a_signal_or_a_tail") {
        return;
    }
    let ticks = 1_000_000;
    let program = Program::new(&(branches(ticks + TAIL) + EXIT));
    for (refused, syscall) in [
        (Refused::Fork, libc::SYS_fork),
        (Refused::Exec, libc::SYS_execve),
        (Refused::Signal, libc::SYS_tgkill),
        (Refused::Tail, libc::SYS_getpid),
    ] {
        let (events, ending) = run_ending(
            &program,
            Plan {
                requests: vec![ticks],
                refused_at_event: Some(refused),
                ..Plan::default()
            },
        );
        assert_eq!(timer_clocks(&events).len(), 1, "{refused:?}: {events:?}");
        // The Tool ignored the error and returned; the run fails anyway, and
        // says why.
        let failure = ending.expect_err("the run must fail");
        assert!(
            failure.contains(&format!(
                "KVM timer-event callback injected syscall {syscall}, which a timer event cannot perform"
            )),
            "{refused:?}: {failure}"
        );
    }
}

#[test]
fn a_refused_timer_injection_ends_the_run_even_when_dropped() {
    if bounded("a_refused_timer_injection_ends_the_run_even_when_dropped") {
        return;
    }
    let ticks = 1_000_000;
    let program = Program::new(&(branches(ticks + TAIL) + EXIT));
    // A refusal at each of the three places that make one: an injection the
    // backend refuses (fork), one the provider refuses (tgkill of SIGUSR1 to
    // the thread), and a tail injection (getpid). The Tool polls it once,
    // drops it, and then exits, retires, or injects again.
    for (refused, syscall) in [
        (Refused::Fork, libc::SYS_fork),
        (Refused::Signal, libc::SYS_tgkill),
        (Refused::Tail, libc::SYS_getpid),
    ] {
        for after in [
            AfterDropped::Exit,
            AfterDropped::Retire,
            AfterDropped::Inject,
        ] {
            let (events, ending) = run_ending(
                &program,
                Plan {
                    requests: vec![ticks],
                    refused_at_event: Some(refused),
                    after_dropped: Some(after),
                    ..Plan::default()
                },
            );
            let context = format!("{refused:?}, then {after:?}: {events:?}");
            assert_eq!(timer_clocks(&events).len(), 1, "{context}");
            if after == AfterDropped::Inject {
                // The later injection never ran: polled once, it had not
                // completed.
                assert!(
                    events.contains(&Event::Injected("None".to_owned())),
                    "{context}"
                );
            }
            let failure = ending.expect_err(&format!("the run must fail: {context}"));
            assert!(
                failure.contains(&format!(
                    "KVM timer-event callback injected syscall {syscall}, which a timer event cannot perform"
                )),
                "{context}: {failure}"
            );
        }
    }
}

#[test]
fn a_timer_callback_cannot_make_a_pending_signal_deliverable() {
    if bounded("a_timer_callback_cannot_make_a_pending_signal_deliverable") {
        return;
    }
    let ticks = 1_000_000;
    // The guest blocks SIGUSR1 (r14 keeps the address of its mask, SIGUSR2's
    // follows), sends it to itself with a tgkill the Tool does not see, so
    // that it stays pending, then reaches the marker, where the request is
    // made, and the tail. It exits with its signal mask shifted right by 8
    // bits: 2 with SIGUSR1 blocked, 10 with SIGUSR2 blocked as well.
    let program = Program::new(&format!(
        "lea masks(%rip),%r14\n\
         xor %edi,%edi\nmov %r14,%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $14,%eax\nsyscall\n\
         mov $39,%eax\nsyscall\nmov %rax,%r12\nmov $186,%eax\nsyscall\nmov %rax,%r13\n\
         mov %r12,%rdi\nmov %r13,%rsi\nmov $10,%edx\nmov $234,%eax\nsyscall\n\
         {}{GETPPID}{}\
         xor %edi,%edi\nxor %esi,%esi\nlea 16(%r14),%rdx\nmov $8,%r10d\nmov $14,%eax\nsyscall\n\
         mov 16(%r14),%rdi\nshr $8,%rdi\nmov $60,%eax\nsyscall\nud2\n\
         .data\n.balign 8\nmasks: .quad 0x200, 0x800, 0\n",
        branches(1_000),
        branches(ticks + TAIL)
    ));
    for (change, expected) in [
        // Refused: the run fails, though the Tool ignores the error.
        (MaskChange::UnblockPending, None),
        // The controls run, and return their own results.
        (MaskChange::BlockAnother, Some(("Ok(0)", 10))),
        (MaskChange::BadSize, Some(("Err(EINVAL)", 2))),
        (MaskChange::BadHow, Some(("Err(EINVAL)", 2))),
        (MaskChange::BadSet, Some(("Err(EFAULT)", 2))),
    ] {
        let (events, ending) = run_ending(
            &program,
            Plan {
                arm: Arm::Marker,
                requests: vec![ticks],
                quiet_tgkill: true,
                mask_change: Some(change),
                ..Plan::default()
            },
        );
        let context = format!("{change:?}: {events:?}");
        assert_eq!(timer_clocks(&events).len(), 1, "{context}");
        let injected: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Injected(result) => Some(result.as_str()),
                _ => None,
            })
            .collect();
        match expected {
            None => {
                assert_eq!(injected, [""; 0], "{context}");
                let failure = ending.expect_err(&format!("the run must fail: {context}"));
                assert!(
                    failure.contains(&format!(
                        "KVM timer-event callback injected syscall {}, which a timer event cannot perform",
                        libc::SYS_rt_sigprocmask
                    )),
                    "{context}: {failure}"
                );
            }
            Some((result, code)) => {
                assert_eq!(injected, [result], "{context}");
                assert_eq!(ending, Ok((code, vec![], vec![])), "{context}");
            }
        }
    }
}

#[test]
fn a_parked_signal_cancels_an_outstanding_request() {
    if bounded("a_parked_signal_cancels_an_outstanding_request") {
        return;
    }
    let ticks = 1_000_000;
    let program = Program::new(&(branches(1_000) + GETPPID + &branches(ticks + TAIL) + EXIT));
    // The marker's callback makes the request, then queues a SIGALRM and
    // observes it while parked: the signal's callback cancels the request.
    let events = run(
        &program,
        Plan {
            arm: Arm::Marker,
            requests: vec![ticks],
            parked_signal: true,
            ..Plan::default()
        },
    );
    assert!(events.contains(&Event::Signal(libc::SIGALRM)), "{events:?}");
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    // The control: without the signal, the request fires.
    let events = run(
        &program,
        Plan {
            arm: Arm::Marker,
            requests: vec![ticks],
            ..Plan::default()
        },
    );
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    let (requested_at, _) = last_request(&events);
    assert_delivered(clocks[0], requested_at, ticks, &format!("{events:?}"));
}

#[test]
fn a_fault_signal_cancels_an_outstanding_request() {
    if bounded("a_fault_signal_cancels_an_outstanding_request") {
        return;
    }
    let ticks = 1_000_000;
    // A SIGILL handler that steps over the faulting ud2, installed first;
    // then the marker, where the request is made, then the fault, whose
    // signal's callback cancels the request, then the tail.
    let program = |fault: &str| {
        Program::new(&format!(
            "mov ${}, %edi\nlea action(%rip), %rsi\nxor %edx, %edx\nmov $8, %r10d\nmov $13, %eax\nsyscall\n\
             test %rax, %rax\njnz failed\n{}{GETPPID}{fault}{}{EXIT}\
             failed: mov $1, %edi\nmov $60, %eax\nsyscall\nud2\n\
             handler: addq $2, 168(%rdx)\nret\nrestorer: mov $15, %eax\nsyscall\nud2\n\
             .data\n.balign 8\naction: .quad handler, 0x4000004, restorer, 0\n",
            libc::SIGILL,
            branches(1_000),
            branches(ticks + TAIL)
        ))
    };
    let plan = Plan {
        arm: Arm::Marker,
        requests: vec![ticks],
        ..Plan::default()
    };
    let events = run(&program("ud2\n"), plan.clone());
    assert!(events.contains(&Event::Signal(libc::SIGILL)), "{events:?}");
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    // The control: without the fault, the request fires.
    let events = run(&program(""), plan);
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    let (requested_at, _) = last_request(&events);
    assert_delivered(clocks[0], requested_at, ticks, &format!("{events:?}"));
}

#[test]
fn a_restarted_callback_cancels_the_request_of_the_attempt_before() {
    if bounded("a_restarted_callback_cancels_the_request_of_the_attempt_before") {
        return;
    }
    let ticks = 1_000_000;
    let program = Program::new(&(branches(1_000) + GETPPID + &branches(ticks + TAIL) + EXIT));
    // The marker's first attempt makes the request and returns ERESTARTSYS,
    // with no signal pending, so the callback runs again at once: that
    // attempt is the syscall's event again, and cancels the request.
    let events = run(
        &program,
        Plan {
            arm: Arm::Marker,
            requests: vec![ticks],
            restart: Some(false),
            ..Plan::default()
        },
    );
    assert!(
        events.contains(&Event::Attempt(0)) && events.contains(&Event::Attempt(1)),
        "{events:?}"
    );
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    // The control: the second attempt requests again, and that request fires.
    let events = run(
        &program,
        Plan {
            arm: Arm::Marker,
            requests: vec![ticks],
            restart: Some(true),
            ..Plan::default()
        },
    );
    let clocks = timer_clocks(&events);
    assert_eq!(clocks.len(), 1, "{events:?}");
    let (requested_at, _) = last_request(&events);
    assert_delivered(clocks[0], requested_at, ticks, &format!("{events:?}"));
}

/// How many times each precise test runs its guest.
const REPEATS: usize = 20;

/// RCBs a precise test's guest runs past its target: an event delivered
/// late, by a kick, still comes before the guest exits.
const PRECISE_TAIL: u64 = 1_000_000;

/// Exits with bit 8 of r15, where a guest collects the trap flag of the
/// flags images it reads, as its status.
const EXIT_WITH_TRAP_FLAG_SEEN: &str = "mov %r15d,%edi\nshr $8,%edi\nmov $60,%eax\nsyscall\nud2\n";

/// Ten RCBs, with a label `branches` does not use.
const TEN_RCBS: &str = "mov $10,%ecx\n7: dec %ecx\njnz 7b\n";

/// A guest that keeps the addresses of `branches`' labels in r12 (label 1,
/// where a loop's third branch leaves RIP), r13 (label 2, after its first)
/// and r14 (label 3, after its second), runs `before`, which must not use
/// those labels, then a loop of `rcbs` RCBs, then `exit`.
fn labelled(before: &str, rcbs: u64, exit: &str) -> Program {
    Program::new(&format!(
        "lea 1f(%rip),%r12\nlea 2f(%rip),%r13\nlea 3f(%rip),%r14\n{before}{}{exit}",
        branches(rcbs)
    ))
}

/// Where the loop's `branch`-th branch, counted from 1, leaves RIP.
fn after_branch(registers: &Registers, branch: u64) -> u64 {
    match branch % 3 {
        1 => registers.r13,
        2 => registers.r14,
        _ => registers.r12,
    }
}

/// The most guests [`lands_exactly`] runs for one target: as many again as
/// the exact runs it needs, for kicks proven late. On this host about one
/// kick in a hundred came past the 2,000 RCB margin, and one in seven at a
/// load average of 43.
const ATTEMPTS: usize = 2 * REPEATS;

/// A plan that requests one precise event of `ticks` RCBs at the start of
/// each image.
fn precise(ticks: u64) -> Plan {
    Plan {
        requests: vec![ticks],
        precise: true,
        ..Plan::default()
    }
}

/// What one guest of [`lands_exactly`] showed: an exact event, or a kick
/// proven late before any entry of its stepping window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landing {
    Exact,
    ProvenLate,
}

/// Runs guests through `land`, given each one's attempt number, until
/// [`REPEATS`] land exactly, running a proven-late one again, but never more
/// than [`ATTEMPTS`] guests in all. Returns how many came late, or the
/// refusal of a guest past that budget.
fn within_attempts(mut land: impl FnMut(usize) -> Landing) -> Result<usize, String> {
    let (mut exact, mut late) = (0, 0);
    while exact < REPEATS {
        if exact + late == ATTEMPTS {
            return Err(format!(
                "{late} proven-late guests left {ATTEMPTS} too few for {REPEATS} exact ones"
            ));
        }
        match land(exact + late + 1) {
            Landing::Exact => exact += 1,
            Landing::ProvenLate => late += 1,
        }
    }
    Ok(late)
}

#[test]
fn the_guest_budget_of_a_precise_target_is_a_total() {
    // The first `late` guests come late, proven so, and the rest exactly:
    // what `within_attempts` returns, and how many guests it ran.
    let run = |late: usize| {
        let mut guests = 0;
        let result = within_attempts(|_| {
            guests += 1;
            if guests <= late {
                Landing::ProvenLate
            } else {
                Landing::Exact
            }
        });
        (result, guests)
    };
    assert_eq!(run(0), (Ok(0), REPEATS));
    assert_eq!(run(REPEATS), (Ok(REPEATS), ATTEMPTS));
    assert!(matches!(run(REPEATS + 1), (Err(_), ATTEMPTS)));
}

/// Runs `program` under `plan`, with registers reported, until `REPEATS`
/// runs have exited with status 0 and got one timer event exactly at
/// `target`, with RIP where `rip` says, and the same registers every time.
///
/// A request whose last notification is a kick (`kicked`: its target lies
/// more than the skid margin plus the single-step allowance past the clock
/// where it is requested) can come late: the PMU interrupt behind about one
/// kick in a thousand on an idle host, and one in a hundred on a busy one,
/// arrives past the margin. Such a run gets its one event past the target,
/// recorded as late, before any entry of the window began (see
/// [`WINDOW_ENTRIES`]), and is run again, within [`ATTEMPTS`] guests in all
/// (see [`within_attempts`]). Every other run must land exactly and record
/// no skid overshoot (the process-global witness count), so a miss after the
/// window's entries began fails.
fn lands_exactly(
    program: &Program,
    plan: Plan,
    target: u64,
    rip: impl Fn(&Registers) -> u64,
    kicked: bool,
) -> Registers {
    let mut first: Option<Registers> = None;
    let _ = reverie::take_skid_overshoot_count();
    within_attempts(|attempt| {
        let events = run(
            program,
            Plan {
                registers: true,
                ..plan.clone()
            },
        );
        let witnessed = reverie::take_skid_overshoot_count();
        let entries = WINDOW_ENTRIES.load(Ordering::Relaxed);
        let context = format!(
            "target {target}, attempt {attempt}, {witnessed} recorded, {entries} window entries: \
             {events:?}"
        );
        let clocks = timer_clocks(&events);
        if kicked && clocks.len() == 1 && clocks[0] > target && entries == 0 {
            assert!(witnessed > 0, "a late event is recorded: {context}");
            println!(
                "late run: target {target}, attempt {attempt}, delivered at {} before any \
                 window entry",
                clocks[0]
            );
            return Landing::ProvenLate;
        }
        assert_eq!(clocks, [target], "{context}");
        assert_eq!(witnessed, 0, "{context}");
        let registers = registers_of(&events);
        assert_eq!(registers.rip, rip(&registers), "{context}");
        match &first {
            None => first = Some(registers),
            Some(first) => assert_eq!(&registers, first, "{context}"),
        }
        Landing::Exact
    })
    .unwrap_or_else(|refusal| panic!("target {target}: {refusal}"));
    first.unwrap()
}

/// The registers the run's one timer event saw.
fn registers_of(events: &[Event]) -> Registers {
    events
        .iter()
        .find_map(|event| match event {
            Event::Registers(registers) => Some(registers.clone()),
            _ => None,
        })
        .unwrap()
}

/// A precise event `k` RCBs from where the image starts, for each `k`.
fn precise_events_land_exactly(ticks: &[u64]) {
    for &k in ticks {
        let program = labelled("", k + PRECISE_TAIL, EXIT);
        let plan = precise(k);
        let kicked = k > skid_margin() + reverie::pmu_config::SINGLESTEP_TIMEOUT_RCBS;
        let registers = lands_exactly(&program, plan, k, |r| after_branch(r, k), kicked);
        // The loop counts RCX down once per iteration of three branches,
        // before the third: after branch k it has done floor(k / 3) of them.
        assert_eq!(
            registers.rcx,
            (k + PRECISE_TAIL).div_ceil(3) - k / 3,
            "{registers:x?}"
        );
        println!("precise {k} RCBs: {REPEATS} runs at clock {k}, {registers:x?}");
    }
}

#[test]
fn a_precise_event_lands_exactly_one_to_six_rcbs_on() {
    if bounded("a_precise_event_lands_exactly_one_to_six_rcbs_on") {
        return;
    }
    precise_events_land_exactly(&[1, 2, 3, 4, 5, 6]);
}

#[test]
fn a_precise_event_lands_exactly_around_the_skid_margin() {
    if bounded("a_precise_event_lands_exactly_around_the_skid_margin") {
        return;
    }
    let margin = skid_margin();
    precise_events_land_exactly(&[margin - 1, margin, margin + 5, margin + 6]);
}

#[test]
fn a_precise_event_lands_exactly_around_the_staging_edge() {
    if bounded("a_precise_event_lands_exactly_around_the_staging_edge") {
        return;
    }
    // The longest request that is not staged, and the shortest that is.
    let edge = reverie::pmu_config::try_get_pmu_config()
        .unwrap()
        .first_stage_margin()
        + reverie::pmu_config::SINGLESTEP_TIMEOUT_RCBS;
    precise_events_land_exactly(&[edge, edge + 1]);
}

#[test]
fn a_precise_event_lands_exactly_far_off() {
    if bounded("a_precise_event_lands_exactly_far_off") {
        return;
    }
    precise_events_land_exactly(&[1_000_000, 100_000_000]);
}

/// A hand-built IRETQ to the next instruction, in the same mode.
const IRETQ_TO_NEXT: &str = "mov %ss,%eax\npush %rax\nlea 8(%rsp),%rax\npush %rax\npushfq\n\
     mov %cs,%eax\npush %rax\nlea 9f(%rip),%rax\npush %rax\niretq\n9:\n";

#[test]
fn a_stepped_guest_never_sees_the_trap_flag() {
    if bounded("a_stepped_guest_never_sees_the_trap_flag") {
        return;
    }
    // In the window, the guest ORs into r15 the TF of the images it reads
    // through PUSHF of both widths, also after RDTSC, CPUID and a getpid,
    // then loads CF, PF, AF, ZF, SF and OF with POPF, reads them back into
    // rbx, and keeps rsp in rbp.
    let read = "pushfq\npop %rax\nand $0x100,%eax\nor %eax,%r15d\n";
    let read16 = "pushfw\nmovzwl (%rsp),%eax\nadd $2,%rsp\nand $0x100,%eax\nor %eax,%r15d\n";
    let before = format!(
        "xor %r15d,%r15d\n{read}{read16}pushfw\npopfw\nrdtsc\n{read}xor %eax,%eax\ncpuid\n{read}\
         mov $39,%eax\nsyscall\n{read16}mov %rsp,%rbp\npush $0x8d7\npopfq\npushfq\npop %rbx\n"
    );
    let k = 40;
    let program = labelled(&before, k + PRECISE_TAIL, EXIT_WITH_TRAP_FLAG_SEEN);
    let plan = Plan {
        no_syscalls: true,
        ..precise(k)
    };
    let r = lands_exactly(&program, plan.clone(), k, |r| after_branch(r, k), false);
    // As a CPL 3 POPF loads them: IF stays as it was.
    assert_eq!(
        (r.rbx, r.rsp),
        (0x8d7 | (r.eflags & 0x200), r.rbp),
        "{r:x?}"
    );
    // Refused by name inside the window: a POPF of either width that sets
    // TF, a load of SS, and IRETQ. The latter two run as usual outside it.
    for (before, refusal) in [
        (
            "pushfq\norq $0x100,(%rsp)\npopfq\n",
            "sets its own trap flag",
        ),
        (
            "pushfw\norw $0x100,(%rsp)\npopfw\n",
            "sets its own trap flag",
        ),
        ("mov %ss,%eax\nmov %eax,%ss\n", "loads SS"),
        (IRETQ_TO_NEXT, "runs IRETQ"),
    ] {
        let program = labelled(before, k + PRECISE_TAIL, EXIT);
        let (events, ending) = run_ending(&program, plan.clone());
        let failure = ending.expect_err(&format!("{refusal}: the run must fail: {events:?}"));
        assert!(failure.contains(refusal), "{failure}");
        if !refusal.contains("trap flag") {
            let mut outside = plan.clone();
            outside.precise = false;
            run(&program, outside);
        }
    }
}

/// After `lead` (`base` RCBs), 100-byte REP MOVSB and REP STOSB, five LOOP
/// iterations, a JRCXZ and a JNZ to a REP MOVSB: a precise request for each
/// of their branches lands exactly. Adopted from a review's probes.
fn loop_jrcxz_and_rep_land_exactly(lead: &str, base: u64) {
    let program = Program::new(&format!(
        "lea 8f(%rip),%r12\nlea 5f(%rip),%r13\nlea 6f(%rip),%r14\nlea 4f(%rip),%r8\n{lead}\
         lea srcbuf(%rip),%rsi\nlea dstbuf(%rip),%rdi\nmov $100,%ecx\nrep movsb\n\
         mov $100,%ecx\nrep stosb\nmov $5,%ecx\n8: loop 8b\n5: xor %ecx,%ecx\njrcxz 6f\nnop\n\
         6: mov $1,%eax\nmov $64,%ecx\nlea srcbuf(%rip),%rsi\nlea dstbuf(%rip),%rdi\n\
         test %eax,%eax\njnz 4f\nnop\n4: rep movsb\n{}{EXIT}\
         .data\nsrcbuf: .fill 256,1,0x5a\ndstbuf: .fill 256,1,0\n",
        branches(PRECISE_TAIL)
    ));
    for (offset, rip, rcx) in [
        (1, (|r: &Registers| r.r12) as fn(&Registers) -> u64, 4),
        (3, |r| r.r12, 2),
        (4, |r| r.r12, 1),
        (5, |r| r.r13, 0),
        (6, |r| r.r14, 0),
        (7, |r| r.r8, 64),
    ] {
        let k = base + offset;
        let plan = precise(k);
        let kicked = k > skid_margin() + reverie::pmu_config::SINGLESTEP_TIMEOUT_RCBS;
        let registers = lands_exactly(&program, plan, k, rip, kicked);
        assert_eq!(registers.rcx, rcx, "{k}: {registers:x?}");
    }
}

#[test]
fn loop_jrcxz_and_rep_land_exactly_inside_the_window() {
    if bounded("loop_jrcxz_and_rep_land_exactly_inside_the_window") {
        return;
    }
    loop_jrcxz_and_rep_land_exactly("", 0);
}

#[test]
fn loop_jrcxz_and_rep_land_exactly_after_a_kick() {
    if bounded("loop_jrcxz_and_rep_land_exactly_after_a_kick") {
        return;
    }
    loop_jrcxz_and_rep_land_exactly(&branches(999_993), 999_993);
}

/// A getpid right after branch 999, the last of its loop, then the tail;
/// r8 holds the getpid's first instruction.
fn getpid_after_branch_999() -> Program {
    labelled(
        "lea 4f(%rip),%r8\n",
        999,
        &format!("4: mov $39,%eax\nsyscall\n{}{EXIT}", branches(PRECISE_TAIL)),
    )
}

/// A guest whose SIGSEGV handler steps over the 8-byte load that faults,
/// retires `rcbs` RCBs and, right after the last, makes the rt_sigreturn
/// itself, to return to a loop that makes no syscall. r12 holds the
/// instruction it returns to.
fn signal_return_program(rcbs: usize) -> Program {
    Program::new(&format!(
        "lea 9f(%rip),%r12\nmov ${}, %edi\nlea action(%rip), %rsi\nxor %edx, %edx\n\
         mov $8, %r10d\nmov $13, %eax\nsyscall\nmovq 0x10,%rax\n9:\n{}{EXIT}\
         handler: addq $8,168(%rdx)\nadd $8,%rsp\nmov $15,%eax\n{}syscall\nrestorer: ud2\n\
         .data\n.balign 8\naction: .quad handler, 0x4000004, restorer, 0\n",
        libc::SIGSEGV,
        branches(PRECISE_TAIL),
        "xor %ecx,%ecx\njz 7f\n7:\n".repeat(rcbs),
    ))
}

#[test]
fn a_request_due_at_rt_sigreturn_is_delivered_with_the_registers_it_restores() {
    if bounded("a_request_due_at_rt_sigreturn_is_delivered_with_the_registers_it_restores") {
        return;
    }
    // The signal's callback requests 1 RCB. The handler's one branch reaches
    // the target and the earliest overflow, 2 RCBs on, comes later, so
    // rt_sigreturn's exit is the first at the target.
    let plan = Plan {
        requests: vec![],
        rearm_at_signal: Some(1),
        registers: true,
        ..Plan::default()
    };
    let events = run(&signal_return_program(1), plan);
    let (requested_at, _) = last_request(&events);
    assert_eq!(timer_clocks(&events), [requested_at + 1], "{events:?}");
    let r = registers_of(&events);
    assert_eq!(r.rip, r.r12, "{r:x?}");
}

#[test]
fn with_no_skid_margin_a_precise_request_is_late_and_still_delivered() {
    // With no skid margin, a request is programmed at its target.
    if bounded_with(
        "with_no_skid_margin_a_precise_request_is_late_and_still_delivered",
        &[(reverie::pmu_config::SKID_MARGIN_OVERRIDE_ENV, "0")],
    ) {
        return;
    }
    assert_eq!(skid_margin(), 0);
    // An unstepped exit exactly at the target, the getpid right after branch
    // 999 or the kick before it, is late.
    let program = getpid_after_branch_999();
    let plan = Plan {
        no_syscalls: true,
        ..precise(999)
    };
    for _ in 0..REPEATS {
        let _ = reverie::take_skid_overshoot_count();
        let events = run(&program, plan.clone());
        assert_eq!(timer_clocks(&events), [999], "{events:?}");
        assert_eq!(reverie::take_skid_overshoot_count(), 1, "{events:?}");
        assert_eq!(WINDOW_ENTRIES.load(Ordering::Relaxed), 0, "{events:?}");
    }
    // A request for 6 RCBs from the signal's callback passes its target in
    // the handler, unstepped, and is late at rt_sigreturn (or at an earlier
    // kick), and delivered once to a guest that makes no syscall after it.
    let program = signal_return_program(6);
    let plan = Plan {
        requests: vec![],
        rearm_at_signal: Some(6),
        precise: true,
        registers: true,
        ..Plan::default()
    };
    let mut at_return = 0;
    for _ in 0..REPEATS {
        let _ = reverie::take_skid_overshoot_count();
        let events = run(&program, plan.clone());
        let (requested_at, _) = last_request(&events);
        assert_eq!(timer_clocks(&events), [requested_at + 6], "{events:?}");
        assert_eq!(reverie::take_skid_overshoot_count(), 1, "{events:?}");
        let r = registers_of(&events);
        at_return += usize::from(r.rip == r.r12);
    }
    assert!(at_return > 0, "never delivered at rt_sigreturn");
    println!("late at its target, delivered at rt_sigreturn in {at_return} of {REPEATS} runs");
}

#[test]
fn a_syscall_inside_the_stepping_window() {
    if bounded("a_syscall_inside_the_stepping_window") {
        return;
    }
    // A request for 100 RCBs, then a getpid at clock 10.
    let program = labelled(
        &format!("{TEN_RCBS}mov $39,%eax\nsyscall\n"),
        1_000 + PRECISE_TAIL,
        EXIT,
    );
    let plan = precise(100);
    // Subscribed, the getpid cancels the request; one made in its callback
    // lands exactly, and without one nothing comes.
    let rearmed = Plan {
        rearm_at_getpid: Some(50),
        ..plan.clone()
    };
    lands_exactly(&program, rearmed, 60, |r| after_branch(r, 50), false);
    let events = run(&program, plan.clone());
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    // Unsubscribed, the backend answers it and the steps go on after it.
    let quiet = Plan {
        no_syscalls: true,
        ..plan
    };
    lands_exactly(&program, quiet.clone(), 100, |r| after_branch(r, 90), false);
    // The guest of the unstepped arrival control (see
    // `with_no_skid_margin_a_precise_request_is_late_and_still_delivered`):
    // stepped from the start, its request lands exactly at the getpid.
    let quiet = Plan {
        requests: vec![999],
        ..quiet
    };
    lands_exactly(&getpid_after_branch_999(), quiet, 999, |r| r.r8, false);
}

#[test]
fn rdtsc_and_cpuid_inside_the_stepping_window() {
    if bounded("rdtsc_and_cpuid_inside_the_stepping_window") {
        return;
    }
    let program = labelled(
        &format!("{TEN_RCBS}rdtsc\nxor %eax,%eax\ncpuid\n"),
        1_000 + PRECISE_TAIL,
        EXIT,
    );
    let plan = precise(100);
    // Unsubscribed, each runs unstepped to the next instruction, and the
    // steps go on from there.
    lands_exactly(&program, plan.clone(), 100, |r| after_branch(r, 90), false);
    // Subscribed, each is an event of its own, which cancels the request.
    for (name, plan, witness) in [
        (
            "RDTSC",
            Plan {
                rdtsc: true,
                ..plan.clone()
            },
            Event::Rdtsc,
        ),
        (
            "CPUID",
            Plan {
                cpuid: true,
                ..plan
            },
            Event::Cpuid,
        ),
    ] {
        let events = run(&program, plan);
        assert!(events.contains(&witness), "{name}: {events:?}");
        assert_eq!(timer_clocks(&events), [0; 0], "{name}: {events:?}");
    }
}

#[test]
fn a_fault_inside_the_stepping_window_leaves_no_trap_flag_in_its_frame() {
    if bounded("a_fault_inside_the_stepping_window_leaves_no_trap_flag_in_its_frame") {
        return;
    }
    // A SIGSEGV handler that collects the trap flag of the frame's saved
    // RFLAGS into the frame's r15, and steps over the 8-byte load that
    // faults; installed first. Then the marker, where 300 RCBs are
    // requested, ten RCBs, the load at clock 10, and the loop.
    let program = |fault: &str| {
        labelled(
            &format!(
                "mov ${}, %edi\nlea action(%rip), %rsi\nxor %edx, %edx\nmov $8, %r10d\n\
                 mov $13, %eax\nsyscall\nxor %r15d,%r15d\n{GETPPID}{TEN_RCBS}{fault}",
                libc::SIGSEGV
            ),
            1_000 + PRECISE_TAIL,
            &format!(
                "{EXIT_WITH_TRAP_FLAG_SEEN}\
                 handler: mov 176(%rdx),%rax\nand $0x100,%eax\nor %rax,96(%rdx)\n\
                 addq $8,168(%rdx)\nret\nrestorer: mov $15,%eax\nsyscall\nud2\n\
                 .data\n.balign 8\naction: .quad handler, 0x4000004, restorer, 0\n"
            ),
        )
    };
    let faulting = program("movq 0x10,%rax\n");
    let plan = Plan {
        arm: Arm::Marker,
        ..precise(300)
    };
    // The fault's signal is an event of its own, which cancels the request.
    let events = run(&faulting, plan.clone());
    assert!(events.contains(&Event::Signal(libc::SIGSEGV)), "{events:?}");
    assert_eq!(timer_clocks(&events), [0; 0], "{events:?}");
    // One made in the signal's callback lands exactly: the steps go on
    // through the handler and after its rt_sigreturn.
    let rearmed = Plan {
        rearm_at_signal: Some(200),
        ..plan.clone()
    };
    lands_exactly(&faulting, rearmed, 210, |r| after_branch(r, 200), false);
    // The control: without the fault, the request lands exactly.
    lands_exactly(&program(""), plan, 300, |r| after_branch(r, 290), false);
}

#[test]
fn a_replaced_or_stale_precise_request_still_lands_exactly() {
    if bounded("a_replaced_or_stale_precise_request_still_lands_exactly") {
        return;
    }
    let program = labelled("", 600 + PRECISE_TAIL, EXIT);
    for (requests, target) in [(vec![500, 300], 300), (vec![300, 500], 500)] {
        let plan = Plan {
            requests,
            precise: true,
            ..Plan::default()
        };
        lands_exactly(&program, plan, target, |r| after_branch(r, target), false);
    }
    // A request for 3,000 RCBs, programmed 1,000 RCBs on, cancelled by a
    // getpid at clock 10 whose callback requests 100,000: the event keeps
    // its pending overflow at 1,000, now stale, which changes nothing.
    let plan = precise(3_000);
    let program = |call: &str| labelled(&format!("{TEN_RCBS}{call}"), 200_000 + PRECISE_TAIL, EXIT);
    let rearmed = Plan {
        rearm_at_getpid: Some(100_000),
        ..plan.clone()
    };
    lands_exactly(
        &program("mov $39,%eax\nsyscall\n"),
        rearmed,
        100_010,
        |r| after_branch(r, 100_000),
        true,
    );
    // The control: without the getpid, the first request lands exactly.
    lands_exactly(&program(""), plan, 3_000, |r| after_branch(r, 2_990), true);
}

/// Where the guests of the signal tests below keep their SIGUSR1 handler's
/// record, which they write to stdout before they exit: the interrupted
/// context in the handler's frame (rip, rax, rbx, rcx, eflags, r12, r14 and
/// r15), then how many times the handler ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record {
    rip: u64,
    rax: u64,
    rbx: u64,
    rcx: u64,
    eflags: u64,
    r12: u64,
    r14: u64,
    r15: u64,
    count: u64,
}

impl Record {
    fn read(stdout: &[u8]) -> Self {
        assert_eq!(stdout.len(), 72, "{stdout:?}");
        let quad = |i: usize| u64::from_ne_bytes(stdout[8 * i..8 * i + 8].try_into().unwrap());
        Self {
            rip: quad(0),
            rax: quad(1),
            rbx: quad(2),
            rcx: quad(3),
            eflags: quad(4),
            r12: quad(5),
            r14: quad(6),
            r15: quad(7),
            count: quad(8),
        }
    }
}

/// SA_RESTORER and SA_SIGINFO.
const HANDLER_FLAGS: u64 = 0x0400_0004;

/// The SIGUSR1 handler that fills the [`Record`] from its frame's ucontext
/// (in rdx) and sets `flag`, its restorer, and the data both use. `actions`
/// adds sigaction records before `usr1_action`.
fn signal_record(flags: u64, actions: &str) -> String {
    let save: String = [168, 144, 128, 152, 176, 72, 88, 96]
        .iter()
        .enumerate()
        .map(|(i, at)| format!("mov {at}(%rdx),%rax\nmov %rax,record+{}(%rip)\n", 8 * i))
        .collect();
    format!(
        "usr1:\nincq record+64(%rip)\n{save}movb $1,flag(%rip)\nret\n\
         restorer: mov $15,%eax\nsyscall\nud2\n\
         .data\n.balign 8\n{actions}usr1_action: .quad usr1, {flags:#x}, restorer, 0\n\
         record: .quad 0,0,0,0,0,0,0,0,0\npid: .quad 0\ntid: .quad 0\nmask: .quad 0x200\n\
         flag: .byte 0\n.bss\n.balign 16\nstack: .skip 65536\nstack_end:\n"
    )
}

/// Writes the [`Record`] and exits with status 0.
const WRITE_RECORD: &str = "mov $1,%edi\nlea record(%rip),%rsi\nmov $72,%edx\nmov $1,%eax\nsyscall\n\
     xor %edi,%edi\nmov $231,%eax\nsyscall\nud2\n";

/// Installs the SIGUSR1 handler of [`signal_record`] and starts a sibling
/// thread that sends SIGUSR1 to this one, with a `tgkill`, and exits.
const SIBLING: &str = "mov $10,%edi\nlea usr1_action(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\n\
     mov $13,%eax\nsyscall\nmov $39,%eax\nsyscall\nmov %rax,pid(%rip)\n\
     mov $186,%eax\nsyscall\nmov %rax,tid(%rip)\n\
     mov $0x50f00,%edi\nlea stack_end(%rip),%rsi\nxor %edx,%edx\nxor %r10d,%r10d\n\
     xor %r8d,%r8d\nmov $56,%eax\nsyscall\ntest %rax,%rax\njnz 8f\n\
     mov pid(%rip),%rdi\nmov tid(%rip),%rsi\nmov $10,%edx\nmov $234,%eax\nsyscall\n\
     xor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n8:\n";

/// How many iterations the spinner of [`spinner`] runs at most.
const SPINS: u64 = 1_000_000;

/// A guest whose sibling sends it SIGUSR1 (see [`SIBLING`]), which then
/// reaches the marker and spins without a syscall, 2 RCBs an iteration, with
/// rax at -ERESTARTSYS and r12 at the loop's head, until its handler has run
/// or for [`SPINS`] iterations. With `blocked`, SIGUSR1 is blocked before the
/// marker and unblocked after the loop, with r12 at that call's return.
fn spinner(flags: u64, blocked: bool) -> Program {
    spinner_after(flags, blocked, "")
}

/// [`spinner`], running `prefix` right before the marker.
fn spinner_after(flags: u64, blocked: bool, prefix: &str) -> Program {
    let mask = |how: u32| {
        format!(
            "mov ${how},%edi\nlea mask(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $14,%eax\nsyscall\n"
        )
    };
    let (block, unblock) = if blocked {
        (mask(0), format!("lea 3f(%rip),%r12\n{}3:\n", mask(1)))
    } else {
        (String::new(), String::new())
    };
    Program::new(&format!(
        "{SIBLING}{block}{prefix}{GETPPID}lea 1f(%rip),%r12\nmov $-512,%rax\nxor %ebx,%ebx\nmov ${SPINS},%rcx\n\
         1: inc %rbx\ncmpb $0,flag(%rip)\njne 2f\ndec %rcx\njnz 1b\n2:\n{unblock}{WRITE_RECORD}{}",
        signal_record(flags, "")
    ))
}

/// A precise request for this many RCBs at the marker: stepped from the
/// marker on, so it is never late.
const SPIN_TICKS: u64 = 1_000;

fn spinner_plan() -> Plan {
    Plan {
        arm: Arm::Marker,
        requests: vec![SPIN_TICKS],
        precise: true,
        registers: true,
        rendezvous: true,
        deliver: true,
        ..Plan::default()
    }
}

/// Runs `program`, checks that it exits with status 0, and returns the
/// Tool's events and the guest's [`Record`].
fn run_recorded(program: &Program, plan: Plan) -> (Vec<Event>, Record) {
    let (events, ending) = run_ending(program, plan);
    let (code, stdout, stderr) = ending.unwrap_or_else(|failure| panic!("{failure}: {events:?}"));
    assert_eq!((code, stderr), (0, vec![]), "{events:?}");
    (events, Record::read(&stdout))
}

/// The clock at the callback of each delivered SIGUSR1, and the index of
/// its event.
fn sigusr1_clocks(events: &[Event]) -> Vec<(usize, u64)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            Event::Signalled { signal, clock } if *signal == libc::SIGUSR1 => Some((at, *clock)),
            _ => None,
        })
        .collect()
}

/// The index of the first timer event, and its clock.
fn first_timer(events: &[Event]) -> (usize, u64) {
    events
        .iter()
        .enumerate()
        .find_map(|(at, event)| match event {
            Event::Timer { clock } => Some((at, *clock)),
            _ => None,
        })
        .expect("a timer event")
}

#[test]
fn a_spinner_signalled_by_a_sibling_gets_its_handler_at_the_next_timer_boundary() {
    if bounded("a_spinner_signalled_by_a_sibling_gets_its_handler_at_the_next_timer_boundary") {
        return;
    }
    // The sibling's tgkill runs while the timer event's callback waits for
    // it, so SIGUSR1 is pending when the callback returns, at the boundary
    // right after branch 1,000. It is delivered there, before the guest runs
    // on: the handler's frame holds the registers the timer event saw, and
    // the loop's head, its 500 iterations and rax as the loop left it. That
    // rax is -ERESTARTSYS changes nothing, with SA_RESTART or without: no
    // syscall is interrupted there, so nothing restarts.
    let mut first = None;
    for flags in [HANDLER_FLAGS, HANDLER_FLAGS | libc::SA_RESTART as u64] {
        let program = spinner(flags, false);
        for _ in 0..REPEATS {
            let (events, record) = run_recorded(&program, spinner_plan());
            let context = format!("{flags:#x}: {record:x?} {events:?}");
            let (requested_at, _) = last_request(&events);
            let target = requested_at + SPIN_TICKS;
            let (timer_at, timer_clock) = first_timer(&events);
            assert_eq!(timer_clocks(&events), [target], "{context}");
            let delivered = sigusr1_clocks(&events);
            assert_eq!(delivered.len(), 1, "{context}");
            assert!(delivered[0].0 > timer_at, "{context}");
            assert_eq!(delivered[0].1, timer_clock, "{context}");
            let r = registers_of(&events);
            assert_eq!(record.count, 1, "{context}");
            assert_eq!(
                (
                    record.rip,
                    record.rax,
                    record.rbx,
                    record.rcx,
                    record.eflags
                ),
                (r.rip, r.rax, r.rbx, r.rcx, r.eflags),
                "{context}"
            );
            assert_eq!(record.rip, record.r12, "{context}");
            assert_eq!(record.rax, -512_i64 as u64, "{context}");
            assert_eq!(record.rbx, SPIN_TICKS / 2, "{context}");
            assert_eq!(record.rcx, SPINS - SPIN_TICKS / 2, "{context}");
            assert_eq!(*first.get_or_insert(record), record, "{context}");
        }
    }
    println!("delivered at the timer boundary with {first:x?}");
}

/// Sets the FS base to the address of `record`, kept in r13, and the GS base
/// to that of `pid`, kept in r15 (arch_prctl ARCH_SET_FS and ARCH_SET_GS).
const SET_BASES: &str = "lea record(%rip),%r13\nmov $0x1002,%edi\nmov %r13,%rsi\nmov $158,%eax\nsyscall\n\
     lea pid(%rip),%r15\nmov $0x1001,%edi\nmov %r15,%rsi\nmov $158,%eax\nsyscall\n";

#[test]
fn a_signal_hook_at_a_timer_boundary_sees_the_timer_events_registers() {
    if bounded("a_signal_hook_at_a_timer_boundary_sees_the_timer_events_registers") {
        return;
    }
    // With the FS and GS bases set, the signal's callback at the timer
    // boundary sees the complete user registers the timer event's callback
    // saw there: the general registers, orig_rax -1, the segment selectors
    // and both bases.
    let plan = Plan {
        signal_registers: true,
        ..spinner_plan()
    };
    let (events, record) = run_recorded(&spinner_after(HANDLER_FLAGS, false, SET_BASES), plan);
    let context = format!("{record:x?} {events:?}");
    assert_eq!(record.count, 1, "{context}");
    let seen: Vec<&Registers> = events
        .iter()
        .filter_map(|event| match event {
            Event::Registers(registers) => Some(registers),
            _ => None,
        })
        .collect();
    assert_eq!(seen.len(), 2, "{context}");
    assert_eq!(seen[0], seen[1], "{context}");
    let r = seen[1];
    assert_ne!((r.r13, r.r15), (0, 0), "{context}");
    assert_eq!((r.fs_base, r.gs_base), (r.r13, r.r15), "{context}");
    assert_eq!(
        (r.orig_rax, r.cs & 3, r.ss & 3),
        (u64::MAX, 3, 3),
        "{context}"
    );
}

#[test]
fn a_signal_hook_before_a_new_threads_first_instruction_sees_its_registers() {
    if bounded("a_signal_hook_before_a_new_threads_first_instruction_sees_its_registers") {
        return;
    }
    // The user boundary of a new thread shares the timer boundary's signal
    // hook context. The guest clones a thread whose TLS (the FS base) is
    // `record`, and sends it SIGUSR1 while the Tool holds its start; the
    // thread takes it before its first instruction, and the signal's
    // callback sees its complete registers: the FS base it was given (also
    // in r8, the clone's TLS argument), user segment selectors, orig_rax -1.
    let program = Program::new(&format!(
        "mov $10,%edi\nlea usr1_action(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $13,%eax\nsyscall\n\
         mov $39,%eax\nsyscall\nmov %rax,pid(%rip)\n\
         mov $0xd0f00,%edi\nlea stack_end(%rip),%rsi\nxor %edx,%edx\nxor %r10d,%r10d\n\
         lea record(%rip),%r8\nmov $56,%eax\nsyscall\ntest %rax,%rax\njz 8f\n\
         mov pid(%rip),%rdi\nmov %rax,%rsi\nmov $10,%edx\nmov $234,%eax\nsyscall\n\
         mov $2000000000,%ecx\n9: cmpb $0,flag(%rip)\njne 7f\ndec %rcx\njnz 9b\n7:\n{WRITE_RECORD}\
         8: xor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n{}",
        signal_record(HANDLER_FLAGS, "")
    ));
    let plan = Plan {
        hold_thread_start: true,
        signal_registers: true,
        deliver: true,
        ..Plan::default()
    };
    let (events, record) = run_recorded(&program, plan);
    let context = format!("{record:x?} {events:?}");
    assert_eq!(record.count, 1, "{context}");
    let r = registers_of(&events);
    assert_ne!(r.r8, 0, "{context}");
    assert_eq!(r.fs_base, r.r8, "{context}");
    assert_eq!(
        (r.orig_rax, r.rax, r.cs & 3, r.ss & 3),
        (u64::MAX, 0, 3, 3),
        "{context}"
    );
    assert_eq!(record.rip, r.rip, "{context}");
}

#[test]
fn a_blocked_signal_waits_at_the_timer_boundary_for_its_unblock() {
    if bounded("a_blocked_signal_waits_at_the_timer_boundary_for_its_unblock") {
        return;
    }
    // SIGUSR1 is blocked, so the timer boundary leaves it pending: the loop
    // runs to its end, and the unblock's return delivers it, with the
    // handler's frame at that call's return (where SYSCALL left rcx).
    let (events, record) = run_recorded(&spinner(HANDLER_FLAGS, true), spinner_plan());
    let context = format!("{record:x?} {events:?}");
    let (requested_at, _) = last_request(&events);
    assert_eq!(
        timer_clocks(&events),
        [requested_at + SPIN_TICKS],
        "{context}"
    );
    assert_eq!(record.count, 1, "{context}");
    assert_eq!((record.rip, record.rax), (record.r12, 0), "{context}");
    assert_eq!((record.rbx, record.rcx), (SPINS, record.rip), "{context}");
    let delivered = sigusr1_clocks(&events);
    assert_eq!(delivered.len(), 1, "{context}");
    assert_eq!(delivered[0].1, requested_at + 2 * SPINS, "{context}");
}

#[test]
fn a_signal_pending_at_a_syscall_return_timer_event_follows_it() {
    if bounded("a_signal_pending_at_a_syscall_return_timer_event_follows_it") {
        return;
    }
    // The guest of
    // `a_timer_event_reaches_a_guest_making_syscalls_the_tool_does_not_see`,
    // with the sibling: its tgkill is the only syscall the Tool sees, and it
    // runs while the timer event's callback, at the return of a getpid,
    // waits for it. The signal is delivered at that same return, after the
    // callback, as the getpid's own: the handler's frame holds the registers
    // the timer event saw, with getpid's result and the loop's count.
    let program = Program::new(&format!(
        "{SIBLING}lea 7f(%rip),%r14\nmov $50000,%r15\n4: mov $3,%rbx\n5: dec %rbx\njnz 5b\n\
         mov $39,%eax\nsyscall\n7: dec %r15\njnz 4b\n{WRITE_RECORD}{}",
        signal_record(HANDLER_FLAGS, "")
    ));
    let plan = Plan {
        requests: vec![100_000],
        only_tgkill: true,
        registers: true,
        rendezvous: true,
        deliver: true,
        ..Plan::default()
    };
    let (events, record) = run_recorded(&program, plan);
    let context = format!("{record:x?} {events:?}");
    let (timer_at, timer_clock) = first_timer(&events);
    assert_eq!(timer_clocks(&events).len(), 1, "{context}");
    let delivered = sigusr1_clocks(&events);
    assert_eq!(delivered.len(), 1, "{context}");
    assert!(delivered[0].0 > timer_at, "{context}");
    assert_eq!(delivered[0].1, timer_clock, "{context}");
    let r = registers_of(&events);
    assert_eq!(
        r.rip, r.r14,
        "the timer event at a getpid's return: {context}"
    );
    assert_eq!(record.count, 1, "{context}");
    assert_eq!(
        (record.rip, record.rax, record.rbx, record.r14, record.r15),
        (r.rip, r.rax, r.rbx, r.r14, r.r15),
        "{context}"
    );
}

#[test]
fn a_signal_pending_at_an_rt_sigreturn_timer_event_follows_it() {
    if bounded("a_signal_pending_at_an_rt_sigreturn_timer_event_follows_it") {
        return;
    }
    // The guest of
    // `a_request_due_at_rt_sigreturn_is_delivered_with_the_registers_it_restores`,
    // with SIGUSR1 blocked by the SIGSEGV handler's mask. SIGSEGV's callback
    // sends SIGUSR1 to the thread, which stays pending through the handler,
    // and requests 1 RCB, which comes due at the handler's rt_sigreturn. The
    // mask that call restores unblocks SIGUSR1, which is delivered at its
    // return, after the timer event, with the registers it restores.
    let program = Program::new(&format!(
        "lea 9f(%rip),%r12\nmov $11,%edi\nlea segv_action(%rip),%rsi\nxor %edx,%edx\n\
         mov $8,%r10d\nmov $13,%eax\nsyscall\nmov $10,%edi\nlea usr1_action(%rip),%rsi\n\
         xor %edx,%edx\nmov $8,%r10d\nmov $13,%eax\nsyscall\nmovq 0x10,%rax\n\
         9: mov ${SPINS},%rcx\n1: cmpb $0,flag(%rip)\njne 2f\ndec %rcx\njnz 1b\n2:\n{WRITE_RECORD}\
         segv: addq $8,168(%rdx)\nadd $8,%rsp\nmov $15,%eax\nxor %ecx,%ecx\njz 7f\n7: syscall\nud2\n\
         {}",
        signal_record(
            HANDLER_FLAGS,
            &format!("segv_action: .quad segv, {HANDLER_FLAGS:#x}, restorer, 0x200\n")
        )
    ));
    let plan = Plan {
        requests: vec![],
        rearm_at_signal: Some(1),
        registers: true,
        deliver: true,
        self_signal_at_fault: true,
        ..Plan::default()
    };
    let (events, record) = run_recorded(&program, plan);
    let context = format!("{record:x?} {events:?}");
    let (timer_at, timer_clock) = first_timer(&events);
    let delivered = sigusr1_clocks(&events);
    assert_eq!(delivered.len(), 1, "{context}");
    assert!(delivered[0].0 > timer_at, "{context}");
    assert_eq!(delivered[0].1, timer_clock, "{context}");
    let r = registers_of(&events);
    assert_eq!(r.rip, r.r12, "the timer event at rt_sigreturn: {context}");
    assert_eq!(record.count, 1, "{context}");
    assert_eq!((record.rip, record.rax), (r.rip, r.rax), "{context}");
}

/// The x87, SSE and AVX state a guest of the FPU tests below reads: MXCSR
/// (STMXCSR), the x87 control word (FNSTCW), the x87 status and tag words
/// (FNSTENV, whose instruction and operand pointers are left out), and YMM5,
/// whose low half is XMM5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FpuState {
    mxcsr: u32,
    control: u16,
    status: u16,
    tags: u16,
    ymm5: [u64; 4],
}

impl FpuState {
    /// Linux's initial state, which `handle_signal` gives a handler
    /// (`fpu__clear_user_states`): MXCSR 0x1f80, x87 control word 0x37f, an
    /// empty x87 stack (TOP 0, every register tagged empty) and zero vector
    /// registers.
    const INITIAL: Self = Self {
        mxcsr: 0x1f80,
        control: 0x37f,
        status: 0,
        tags: 0xffff,
        ymm5: [0; 4],
    };

    /// The state [`FPU_LOAD`] leaves, with TOP 6 (bits 11 to 13) and no
    /// other bit of the status word, whose condition codes FLD leaves
    /// undefined.
    const LOADED: Self = Self {
        mxcsr: 0x7f80,
        control: 0x027f,
        status: 6 << 11,
        tags: 0x0fff,
        ymm5: FPU_YMM5,
    };

    /// The block [`fpu_save`] writes at the start of `bytes`.
    fn read(bytes: &[u8]) -> Self {
        let half = |at: usize| u16::from_ne_bytes(bytes[at..at + 2].try_into().unwrap());
        let quad = |at: usize| u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap());
        Self {
            mxcsr: u32::from_ne_bytes(bytes[..4].try_into().unwrap()),
            control: half(8),
            // FNSTENV's 28-byte image: the control, status and tag words
            // each fill four bytes.
            status: half(20),
            tags: half(24),
            ymm5: [quad(48), quad(56), quad(64), quad(72)],
        }
    }
}

/// What a guest of the FPU tests below writes to stdout before it exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FpuRecord {
    /// The state at the handler's first instruction.
    handler: FpuState,
    /// How many times the handler ran.
    entries: u64,
    /// The interrupted rip in the handler's frame, and the frame's r12, where
    /// the guest keeps the instruction at which the signal should come.
    rip: u64,
    r12: u64,
    /// The state after the handler returned.
    after: FpuState,
    /// ST0 and ST1 after the handler returned, as FSTP m80 stores them.
    stack: [[u8; 10]; 2],
}

impl FpuRecord {
    fn read(stdout: &[u8]) -> Self {
        assert_eq!(stdout.len(), FPU_RECORD_SIZE, "{stdout:?}");
        let quad = |at: usize| u64::from_ne_bytes(stdout[at..at + 8].try_into().unwrap());
        Self {
            handler: FpuState::read(&stdout[..80]),
            entries: quad(80),
            rip: quad(88),
            r12: quad(96),
            after: FpuState::read(&stdout[104..184]),
            stack: [
                stdout[184..194].try_into().unwrap(),
                stdout[200..210].try_into().unwrap(),
            ],
        }
    }

    /// Everything but the frame's rip and r12, which tell where the signal
    /// came: natively, a sibling's signal comes wherever the guest then is.
    fn fpu(&self) -> (FpuState, u64, FpuState, [[u8; 10]; 2]) {
        (self.handler, self.entries, self.after, self.stack)
    }
}

const FPU_RECORD_SIZE: usize = 216;

/// MXCSR 0x7f80 (round toward zero), x87 control word 0x027f (double
/// precision), 1.5 and then -3.25 on the x87 stack, and a nonzero YMM5: the
/// interrupted state, which a handler must not see and which must be intact
/// after it returns.
const FPU_LOAD: &str = "ldmxcsr fpu_mxcsr(%rip)\nfldcw fpu_control(%rip)\nfldt fpu_first(%rip)\n\
     fldt fpu_second(%rip)\nvmovdqu fpu_ymm5(%rip),%ymm5\n";

/// The bytes of -3.25 and 1.5 as FSTP m80 stores them: ST0 and ST1 after
/// [`FPU_LOAD`].
const FPU_STACK: [[u8; 10]; 2] = [
    [0, 0, 0, 0, 0, 0, 0, 0xd0, 0x00, 0xc0],
    [0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0x3f],
];

const FPU_YMM5: [u64; 4] = [
    0x1111_1111_1111_1111,
    0x2222_2222_2222_2222,
    0x3333_3333_3333_3333,
    0x4444_4444_4444_4444,
];

/// Saves an [`FpuState`] at `offset` in `fpu_record`.
fn fpu_save(offset: usize) -> String {
    format!(
        "stmxcsr fpu_record+{offset}(%rip)\nfnstcw fpu_record+{}(%rip)\n\
         fnstenv fpu_record+{}(%rip)\nvmovdqu %ymm5,fpu_record+{}(%rip)\n",
        offset + 8,
        offset + 16,
        offset + 48
    )
}

/// After the handler returned: saves the state, pops ST0 and ST1 into the
/// record, writes the record and exits with status 0.
fn fpu_after() -> String {
    format!(
        "{}fstpt fpu_record+184(%rip)\nfstpt fpu_record+200(%rip)\n\
         mov $1,%edi\nlea fpu_record(%rip),%rsi\nmov ${FPU_RECORD_SIZE},%edx\nmov $1,%eax\nsyscall\n\
         xor %edi,%edi\nmov $231,%eax\nsyscall\nud2\n",
        fpu_save(104)
    )
}

/// Installs the handler of [`fpu_handler`] for `signal`, with the sigaction
/// record `action`.
fn fpu_install(signal: i32, action: &str) -> String {
    format!(
        "mov ${signal},%edi\nlea {action}(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $13,%eax\nsyscall\n"
    )
}

/// The handler, its restorer and the data. The handler saves the state it
/// starts with, counts its entries, keeps its frame's rip and r12, runs
/// `extra` and sets `flag`; `action` names its sigaction record. The data
/// includes what [`SIBLING`] uses.
fn fpu_handler(action: &str, extra: &str) -> String {
    fpu_handler_loading(action, extra, 0x7f80)
}

/// [`fpu_handler`], with [`FPU_LOAD`] loading `mxcsr` as MXCSR.
fn fpu_handler_loading(action: &str, extra: &str, mxcsr: u32) -> String {
    format!(
        "fpu_handler:\n{}incq fpu_record+80(%rip)\nmov 168(%rdx),%rax\nmov %rax,fpu_record+88(%rip)\n\
         mov 72(%rdx),%rax\nmov %rax,fpu_record+96(%rip)\n{extra}movb $1,flag(%rip)\nret\n\
         restorer: mov $15,%eax\nsyscall\nud2\n\
         .data\n.balign 8\n{action}: .quad fpu_handler, {HANDLER_FLAGS:#x}, restorer, 0\n\
         pid: .quad 0\ntid: .quad 0\nflag: .byte 0\n\
         fpu_mxcsr: .long {mxcsr:#x}\nfpu_control: .short 0x027f\n\
         .balign 16\nfpu_first: .quad 0xc000000000000000\n.short 0x3fff\n\
         .balign 16\nfpu_second: .quad 0xd000000000000000\n.short 0xc000\n\
         .balign 32\nfpu_ymm5: .quad {:#x}, {:#x}, {:#x}, {:#x}\n\
         .balign 64\nfpu_record: .zero {FPU_RECORD_SIZE}\n\
         .bss\n.balign 16\nstack: .skip 65536\nstack_end:\n",
        fpu_save(0),
        FPU_YMM5[0],
        FPU_YMM5[1],
        FPU_YMM5[2],
        FPU_YMM5[3],
    )
}

/// Checks a record of a guest that loaded [`FPU_LOAD`] and then took one
/// signal: its handler started in Linux's initial state, and the interrupted
/// code found its own state again after the handler returned.
fn check_fpu_record(record: &FpuRecord, context: &str) {
    check_loaded_fpu_record(record, context, 0x7f80);
}

/// [`check_fpu_record`], where the interrupted code finds MXCSR `mxcsr`
/// after the handler returned.
fn check_loaded_fpu_record(record: &FpuRecord, context: &str, mxcsr: u32) {
    let context = format!("{context}: {record:x?}");
    assert_eq!(record.entries, 1, "{context}");
    assert_eq!(record.handler, FpuState::INITIAL, "{context}");
    let after = FpuState {
        status: record.after.status & 7 << 11,
        ..record.after
    };
    let loaded = FpuState {
        mxcsr,
        ..FpuState::LOADED
    };
    assert_eq!(after, loaded, "{context}");
    assert_eq!(record.stack, FPU_STACK, "{context}");
}

/// Runs `program` natively, the Linux reference, checks that it exits with
/// status 0 and writes nothing to stderr, and returns its stdout.
fn native_stdout(program: &Program) -> Vec<u8> {
    let output = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "30s"])
        .arg(&program.executable)
        .current_dir(&program.directory)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "native: {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Runs `program` natively and returns its record.
fn native_fpu_record(program: &Program) -> FpuRecord {
    FpuRecord::read(&native_stdout(program))
}

/// Runs `program` under the backend's own runtime, with no Tool, checks that
/// it exits with status 0 and writes nothing to stderr, and returns its
/// stdout.
fn direct_stdout(program: &Program) -> Vec<u8> {
    let mut backend = KvmBackend::new(program.memory).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&program.executable).unwrap(),
            &[program.executable.to_str().unwrap()],
            &[],
            &program.directory,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!((code, stderr), (0, vec![]));
    stdout
}

/// Runs `program` under the backend's own runtime and returns its record.
fn direct_fpu_record(program: &Program) -> FpuRecord {
    FpuRecord::read(&direct_stdout(program))
}

/// Runs `program` under a [`TimerTool`] following `plan`, checks that it
/// exits with status 0 and writes nothing to stderr, and returns the Tool's
/// events and its stdout.
fn tool_stdout(program: &Program, plan: Plan) -> (Vec<Event>, Vec<u8>) {
    let (events, ending) = run_ending(program, plan);
    let (code, stdout, stderr) = ending.unwrap_or_else(|failure| panic!("{failure}: {events:?}"));
    assert_eq!((code, stderr), (0, vec![]), "{events:?}");
    (events, stdout)
}

/// Runs `program` under a [`TimerTool`] following `plan` and returns the
/// Tool's events and the record.
fn tool_fpu_record(program: &Program, plan: Plan) -> (Vec<Event>, FpuRecord) {
    let (events, stdout) = tool_stdout(program, plan);
    (events, FpuRecord::read(&stdout))
}

/// The stdout of `program` under the backend's own runtime and under a Tool
/// that delivers every signal.
fn kvm_stdouts(program: &Program) -> [(&'static str, Vec<u8>); 2] {
    let plan = Plan {
        deliver: true,
        ..Plan::default()
    };
    [
        ("runtime", direct_stdout(program)),
        ("tool", tool_stdout(program, plan).1),
    ]
}

/// Checks a guest whose signal comes at the instruction it marks in r12, in
/// every run: natively, under the backend's runtime and under a Tool. Each
/// run's handler starts in Linux's initial FPU state, the interrupted code's
/// state is intact after it, and KVM's records equal the native one.
fn fpu_state_matches_native_at_mark(program: &Program) {
    fpu_matches_native_at_mark(program, check_fpu_record);
}

/// [`fpu_state_matches_native_at_mark`], with `check` for each record.
fn fpu_matches_native_at_mark(program: &Program, check: impl Fn(&FpuRecord, &str)) {
    let native = native_fpu_record(program);
    check(&native, "native");
    assert_eq!(native.rip, native.r12, "native: {native:x?}");
    let plan = Plan {
        deliver: true,
        ..Plan::default()
    };
    for (name, record) in [
        ("runtime", direct_fpu_record(program)),
        ("tool", tool_fpu_record(program, plan).1),
    ] {
        check(&record, name);
        assert_eq!(record.rip, record.r12, "{name}: {record:x?}");
        assert_eq!(
            record.fpu(),
            native.fpu(),
            "{name} {record:x?}, native {native:x?}"
        );
    }
}

#[test]
fn a_handler_at_a_syscall_return_starts_with_linuxs_initial_fpu_state() {
    if bounded("a_handler_at_a_syscall_return_starts_with_linuxs_initial_fpu_state") {
        return;
    }
    // The guest loads its FPU state and sends SIGUSR1 to itself with tgkill,
    // which is delivered at that call's return.
    let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", ""));
    fpu_state_matches_native_at_mark(&program);
}

/// A guest that runs `load` and sends SIGUSR1 to itself with tgkill, marking
/// that call's return in r12, and installs `handler` (an [`fpu_handler`])
/// for it.
fn fpu_at_syscall_return(load: &str, handler: &str) -> Program {
    Program::new(&format!(
        "{}mov $39,%eax\nsyscall\nmov %rax,pid(%rip)\nmov $186,%eax\nsyscall\nmov %rax,tid(%rip)\n\
         {load}mov pid(%rip),%rdi\nmov tid(%rip),%rsi\nmov $10,%edx\nlea 1f(%rip),%r12\n\
         mov $234,%eax\nsyscall\n1:\n{}{handler}",
        fpu_install(libc::SIGUSR1, "usr1_action"),
        fpu_after(),
    ))
}

#[test]
fn a_handler_at_a_user_trap_starts_with_linuxs_initial_fpu_state() {
    if bounded("a_handler_at_a_user_trap_starts_with_linuxs_initial_fpu_state") {
        return;
    }
    // The guest loads its FPU state and runs a ud2, whose SIGILL handler
    // steps over it.
    let program = Program::new(&format!(
        "{}{FPU_LOAD}lea 1f(%rip),%r12\n1: ud2\n{}{}",
        fpu_install(libc::SIGILL, "ill_action"),
        fpu_after(),
        fpu_handler("ill_action", "addq $2,168(%rdx)\n")
    ));
    fpu_state_matches_native_at_mark(&program);
}

#[test]
fn a_handler_at_a_timer_boundary_starts_with_linuxs_initial_fpu_state() {
    if bounded("a_handler_at_a_timer_boundary_starts_with_linuxs_initial_fpu_state") {
        return;
    }
    // The spinner of `a_spinner_signalled_by_a_sibling_gets_its_handler_at_the_next_timer_boundary`,
    // which loads its FPU state before it starts the sibling and spins until
    // its handler has run. Natively, the sibling's SIGUSR1 comes wherever
    // the guest then is; under the Tool, at the timer boundary at the loop's
    // head.
    let program = Program::new(&format!(
        "{FPU_LOAD}{SIBLING}{GETPPID}lea 1f(%rip),%r12\nmov $2000000000,%rcx\n\
         1: cmpb $0,flag(%rip)\njne 2f\ndec %rcx\njnz 1b\n2:\n{}{}",
        fpu_after(),
        fpu_handler("usr1_action", "")
    ));
    let native = native_fpu_record(&program);
    check_fpu_record(&native, "native");
    for _ in 0..REPEATS {
        let (events, record) = tool_fpu_record(&program, spinner_plan());
        let context = format!("{record:x?} {events:?}");
        let (timer_at, timer_clock) = first_timer(&events);
        let delivered = sigusr1_clocks(&events);
        assert_eq!(delivered.len(), 1, "{context}");
        assert!(delivered[0].0 > timer_at, "{context}");
        assert_eq!(delivered[0].1, timer_clock, "{context}");
        let r = registers_of(&events);
        assert_eq!((record.rip, record.r12), (r.rip, r.rip), "{context}");
        check_fpu_record(&record, "tool");
        assert_eq!(record.fpu(), native.fpu(), "{context}, native {native:x?}");
    }
}

/// The MXCSR_MASK field of an FXSAVE image taken on this CPU, which Linux
/// writes into every signal frame.
fn cpu_mxcsr_mask() -> u32 {
    #[repr(C, align(16))]
    struct FxsaveArea([u8; 512]);
    let mut area = FxsaveArea([0; 512]);
    // SAFETY: FXSAVE writes exactly this 512-byte, 16-byte-aligned area.
    unsafe { std::arch::x86_64::_fxsave64(area.0.as_mut_ptr()) };
    u32::from_ne_bytes(area.0[28..32].try_into().unwrap())
}

/// DAZ, MXCSR bit 6: denormal operands are read as zero.
const DAZ: u32 = 1 << 6;

#[test]
fn an_interrupted_mxcsr_with_daz_survives_a_signal_handler() {
    if bounded("an_interrupted_mxcsr_with_daz_survives_a_signal_handler") {
        return;
    }
    assert_ne!(cpu_mxcsr_mask() & DAZ, 0, "this CPU has no DAZ");
    // The guest loads MXCSR 0x1fc0, with DAZ, as -ffast-math startup code
    // does; Linux saves it in the frame and restores it at rt_sigreturn.
    let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler_loading("usr1_action", "", 0x1fc0));
    fpu_matches_native_at_mark(&program, |record, context| {
        check_loaded_fpu_record(record, context, 0x1fc0)
    });
}

#[test]
fn a_handler_may_set_daz_in_its_frames_mxcsr() {
    if bounded("a_handler_may_set_daz_in_its_frames_mxcsr") {
        return;
    }
    assert_ne!(cpu_mxcsr_mask() & DAZ, 0, "this CPU has no DAZ");
    // The handler writes MXCSR 0x5fc0 (DAZ, rounding up) into its frame, and
    // the interrupted code resumes with it.
    let edit = "mov 224(%rdx),%rax\nmovl $0x5fc0,24(%rax)\n";
    let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", edit));
    fpu_matches_native_at_mark(&program, |record, context| {
        check_loaded_fpu_record(record, context, 0x5fc0)
    });
}

/// A C program built with -ffast-math, whose startup code sets DAZ and FTZ,
/// that takes a signal and returns from its handler.
const FAST_MATH_GUEST: &str = r#"#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <xmmintrin.h>

static volatile sig_atomic_t entries;
static volatile unsigned int handler_mxcsr;

static void handler(int signal) {
  (void)signal;
  handler_mxcsr = _mm_getcsr();
  entries++;
}

int main(void) {
  struct sigaction action;
  memset(&action, 0, sizeof action);
  action.sa_handler = handler;
  sigemptyset(&action.sa_mask);
  if (sigaction(SIGUSR1, &action, NULL) != 0)
    return 1;
  unsigned int before = _mm_getcsr();
  if (raise(SIGUSR1) != 0)
    return 2;
  unsigned int after = _mm_getcsr();
  volatile float tiny = 1e-40f;
  volatile float sum = tiny + 0.0f;
  printf("mxcsr %#x before, %#x in the handler, %#x after; %d handler entries; subnormal + 0 is %s\n",
         before, handler_mxcsr, after, (int)entries, sum == 0.0f ? "zero" : "subnormal");
  return 0;
}
"#;

#[test]
fn a_fast_math_program_returns_from_a_signal_handler() {
    if bounded("a_fast_math_program_returns_from_a_signal_handler") {
        return;
    }
    let program = Program::c(FAST_MATH_GUEST, &["-ffast-math"]);
    let native = native_stdout(&program);
    assert_eq!(
        String::from_utf8_lossy(&native),
        "mxcsr 0x9fc0 before, 0x1f80 in the handler, 0x9fc0 after; 1 handler entries; \
         subnormal + 0 is zero\n",
        "native"
    );
    for (name, stdout) in kvm_stdouts(&program) {
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&native),
            "{name}"
        );
    }
}

#[test]
fn a_handlers_edits_to_fpu_state_the_interrupted_code_never_used_take_effect() {
    if bounded("a_handlers_edits_to_fpu_state_the_interrupted_code_never_used_take_effect") {
        return;
    }
    // The guest uses no x87, SSE or AVX instruction before its signal, so
    // that state is initial. Its handler sets the frame's x87 control word to
    // 0x027f, MXCSR to 0x5f80 and XMM5 to a pattern; Linux marks x87 and SSE
    // present in every frame, so rt_sigreturn applies the three edits.
    let edit = "mov 224(%rdx),%rax\nmovw $0x027f,(%rax)\nmovl $0x5f80,24(%rax)\n\
                movabs $0x5555555555555555,%rcx\nmov %rcx,240(%rax)\nmov %rcx,248(%rax)\n";
    let program = fpu_at_syscall_return("", &fpu_handler("usr1_action", edit));
    fpu_matches_native_at_mark(&program, |record, context| {
        let context = format!("{context}: {record:x?}");
        assert_eq!(record.entries, 1, "{context}");
        assert_eq!(record.handler, FpuState::INITIAL, "{context}");
        let pattern = 0x5555_5555_5555_5555;
        assert_eq!(
            (record.after.control, record.after.mxcsr, record.after.ymm5),
            (0x027f, 0x5f80, [pattern, pattern, 0, 0]),
            "{context}"
        );
    });
}

#[test]
fn rt_sigreturn_takes_mxcsr_from_a_frame_whose_header_leaves_sse_initial() {
    if bounded("rt_sigreturn_takes_mxcsr_from_a_frame_whose_header_leaves_sse_initial") {
        return;
    }
    // The handler sets its frame's MXCSR to 0x5f80 and clears the SSE and YMM
    // bits of its XSTATE_BV, keeping the x87 bit or clearing it too. Linux's
    // XRSTOR then zeroes XMM5 and YMM5 but still loads MXCSR from the frame.
    for header in [1, 0] {
        let edit = format!("mov 224(%rdx),%rax\nmovl $0x5f80,24(%rax)\nmovq ${header},512(%rax)\n");
        let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", &edit));
        fpu_matches_native_at_mark(&program, |record, context| {
            let context = format!("{context}, XSTATE_BV {header}: {record:x?}");
            assert_eq!(record.entries, 1, "{context}");
            assert_eq!(
                (record.after.mxcsr, record.after.ymm5),
                (0x5f80, [0; 4]),
                "{context}"
            );
        });
    }
}

/// Runs `program` natively, with no core dump, and returns how it ended.
fn native_output(program: &Program) -> std::process::Output {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("timeout");
    command
        .args(["--kill-after=2s", "30s"])
        .arg(&program.executable)
        .current_dir(&program.directory);
    // SAFETY: setrlimit is async-signal-safe and touches no memory the
    // parent shares.
    unsafe {
        command.pre_exec(|| {
            // A core_pattern that pipes core dumps to a program ignores
            // RLIMIT_CORE except for the value 1, with which the kernel
            // skips the dump.
            let one = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            match libc::setrlimit(libc::RLIMIT_CORE, &one) {
                0 => Ok(()),
                _ => Err(std::io::Error::last_os_error()),
            }
        });
    }
    command.output().unwrap()
}

/// Checks that `program` ends with SIGSEGV and writes nothing, natively,
/// under the backend's own runtime and under a Tool that delivers every
/// signal.
fn segfaults_natively_and_on_kvm(program: &Program) {
    use std::os::unix::process::ExitStatusExt;
    let output = native_output(program);
    // timeout ends itself with the signal that ended its child.
    assert_eq!(
        (output.status.signal(), output.stdout, output.stderr),
        (Some(libc::SIGSEGV), vec![], vec![]),
        "native: {}",
        output.status
    );
    let mut backend = KvmBackend::new(program.memory).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&program.executable).unwrap(),
            &[program.executable.to_str().unwrap()],
            &[],
            &program.directory,
        )
        .unwrap();
    assert_eq!(
        backend.run_static_elf_captured().unwrap(),
        (128 + libc::SIGSEGV, vec![], vec![]),
        "runtime"
    );
    let plan = Plan {
        deliver: true,
        ..Plan::default()
    };
    let (events, ending) = run_ending(program, plan);
    assert_eq!(
        ending,
        Ok((128 + libc::SIGSEGV, vec![], vec![])),
        "tool: {events:?}"
    );
}

#[test]
fn rt_sigreturn_ignores_mxcsr_where_linux_does_not_load_it() {
    if bounded("rt_sigreturn_ignores_mxcsr_where_linux_does_not_load_it") {
        return;
    }
    // The handler sets a reserved MXCSR bit in its frame, and the frame's
    // software xfeatures and XSTATE_BV to x87 alone or to nothing. XRSTOR
    // loads, and so checks, MXCSR only for requested SSE or YMM state, and
    // Linux initializes SSE and YMM state afterwards: MXCSR 0x1f80 and zero
    // YMM5. x87 state comes from the frame when it is requested, and is
    // initialized otherwise, which leaves FSTP an empty stack to store.
    for bit in [16, 31] {
        let reserved = 1u32 << bit;
        for features in [1, 0] {
            let edit = format!(
                "mov 224(%rdx),%rax\norl ${reserved:#x},24(%rax)\nmovq ${features},472(%rax)\n\
                 movq ${features},512(%rax)\n"
            );
            let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", &edit));
            fpu_matches_native_at_mark(&program, |record, context| {
                let context =
                    format!("{context}, MXCSR bit {bit}, features {features}: {record:x?}");
                assert_eq!(record.entries, 1, "{context}");
                assert_eq!(record.handler, FpuState::INITIAL, "{context}");
                assert_eq!(
                    (record.after.mxcsr, record.after.ymm5),
                    (0x1f80, [0; 4]),
                    "{context}"
                );
                if features == 1 {
                    assert_eq!(
                        (record.after.control, record.after.tags, record.stack),
                        (0x027f, FpuState::LOADED.tags, FPU_STACK),
                        "{context}"
                    );
                } else {
                    assert_eq!(
                        (record.after.control, record.after.status, record.after.tags),
                        (0x037f, 0, 0xffff),
                        "{context}"
                    );
                }
            });
        }
        // With SSE requested, as in the frame as built, the bit faults.
        let edit = format!("mov 224(%rdx),%rax\norl ${reserved:#x},24(%rax)\n");
        let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", &edit));
        segfaults_natively_and_on_kvm(&program);
    }
}

#[test]
fn rt_sigreturn_initializes_ymm_state_the_frames_features_leave_out() {
    if bounded("rt_sigreturn_initializes_ymm_state_the_frames_features_leave_out") {
        return;
    }
    // The handler sets its frame's software xfeatures to x87 and SSE,
    // leaving XSTATE_BV marking YMM present. XRSTOR ignores that bit, and
    // Linux initializes YMM state afterwards: YMM5's upper half is zero,
    // and the rest comes from the frame, MXCSR 0x7f80 included where the
    // host's format keeps it.
    let edit = "mov 224(%rdx),%rax\nmovq $3,472(%rax)\n";
    let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", edit));
    fpu_matches_native_but_mxcsr_at_mark(&program, 0x7f80, |record, context| {
        let context = format!("{context}: {record:x?}");
        assert_eq!(record.entries, 1, "{context}");
        assert_eq!(
            (record.after.control, record.stack),
            (0x027f, FPU_STACK),
            "{context}"
        );
        assert_eq!(
            record.after.ymm5,
            [FPU_YMM5[0], FPU_YMM5[1], 0, 0],
            "{context}"
        );
    });
}

#[test]
fn rt_sigreturn_restores_a_frame_without_magic2_as_legacy_state() {
    if bounded("rt_sigreturn_restores_a_frame_without_magic2_as_legacy_state") {
        return;
    }
    // The handler sets its frame's MXCSR to 0x5f80 and XMM5 to a pattern,
    // and clears FP_XSTATE_MAGIC2 after the frame's xstate_size bytes.
    // Linux then restores the frame's legacy FXSAVE image, the edits
    // included (MXCSR where the host's format keeps it), and initializes
    // YMM state.
    let edit = "mov 224(%rdx),%rax\nmovl $0x5f80,24(%rax)\nmovabs $0x5555555555555555,%rcx\n\
                mov %rcx,240(%rax)\nmov %rcx,248(%rax)\nmov 480(%rax),%ecx\nmovl $0,(%rax,%rcx)\n";
    let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", edit));
    fpu_matches_native_but_mxcsr_at_mark(&program, 0x5f80, |record, context| {
        let context = format!("{context}: {record:x?}");
        assert_eq!(record.entries, 1, "{context}");
        let pattern = 0x5555_5555_5555_5555;
        assert_eq!(
            (record.after.control, record.stack),
            (0x027f, FPU_STACK),
            "{context}"
        );
        assert_eq!(record.after.ymm5, [pattern, pattern, 0, 0], "{context}");
    });
}

/// Whether this host's Linux keeps FPU state in the compacted format, as it
/// does when it uses XSAVEC or XSAVES (`fpu__init_system_xstate`); the
/// kernel lists the features it uses in /proc/cpuinfo.
fn linux_uses_compacted_xstate() -> bool {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap();
    let flags = cpuinfo
        .lines()
        .find(|line| line.starts_with("flags"))
        .expect("a flags line in /proc/cpuinfo");
    flags
        .split_whitespace()
        .any(|flag| flag == "xsavec" || flag == "xsaves")
}

/// [`fpu_matches_native_at_mark`] for a frame whose features include SSE but
/// not YMM, or that Linux restores as a legacy FXSAVE image. Linux then
/// initializes YMM state with an XRSTOR of its initial state, which on a
/// host whose kernel keeps FPU state in the standard format also loads that
/// state's MXCSR, 0x1f80; with the compacted format MXCSR stays `restored`,
/// the frame's. So natively MXCSR must be Linux's answer for this host;
/// under KVM it must be `restored`, the backend's answer on every host; and
/// every other field must equal the native one.
fn fpu_matches_native_but_mxcsr_at_mark(
    program: &Program,
    restored: u32,
    check: impl Fn(&FpuRecord, &str),
) {
    let native = native_fpu_record(program);
    let native_mxcsr = if linux_uses_compacted_xstate() {
        restored
    } else {
        0x1f80
    };
    assert_eq!(native.after.mxcsr, native_mxcsr, "native: {native:x?}");
    check(&native, "native");
    assert_eq!(native.rip, native.r12, "native: {native:x?}");
    // Everything but the MXCSR the guest finds after the return.
    let rest = |record: &FpuRecord| {
        let mut fpu = record.fpu();
        fpu.2.mxcsr = 0;
        fpu
    };
    let plan = Plan {
        deliver: true,
        ..Plan::default()
    };
    for (name, record) in [
        ("runtime", direct_fpu_record(program)),
        ("tool", tool_fpu_record(program, plan).1),
    ] {
        assert_eq!(record.after.mxcsr, restored, "{name}: {record:x?}");
        check(&record, name);
        assert_eq!(record.rip, record.r12, "{name}: {record:x?}");
        assert_eq!(
            rest(&record),
            rest(&native),
            "{name} {record:x?}, native {native:x?}"
        );
    }
}

#[test]
fn rt_sigreturn_ignores_the_software_reserved_padding() {
    if bounded("rt_sigreturn_ignores_the_software_reserved_padding") {
        return;
    }
    // The handler sets its frame's MXCSR to 0x5f80 and a padding byte of
    // the software-reserved bytes, which Linux never reads.
    let edit = "mov 224(%rdx),%rax\nmovl $0x5f80,24(%rax)\nmovb $0xa5,490(%rax)\n";
    let program = fpu_at_syscall_return(FPU_LOAD, &fpu_handler("usr1_action", edit));
    fpu_matches_native_at_mark(&program, |record, context| {
        check_loaded_fpu_record(record, context, 0x5f80)
    });
}

/// What the guest of
/// `a_frame_after_a_null_fpregs_return_carries_the_cpus_mxcsr_mask` writes:
/// how many times each handler ran; the MXCSR_MASK and MXCSR in SIGUSR2's
/// frame; MXCSR as SIGUSR2's handler starts; the bits of a subnormal plus
/// zero there, computed with DAZ when that frame's mask offers it; the
/// frame's rip and r12; MXCSR once both handlers have returned; the frame's
/// XSTATE_BV; and YMM5 once both handlers have returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueuedRecord {
    first_entries: u64,
    second_entries: u64,
    frame_mask: u32,
    frame_mxcsr: u32,
    handler_mxcsr: u32,
    sum: u32,
    rip: u64,
    r12: u64,
    after_mxcsr: u32,
    frame_xstate_bv: u64,
    after_ymm5: [u64; 4],
}

impl QueuedRecord {
    fn read(stdout: &[u8]) -> Self {
        assert_eq!(stdout.len(), 112, "{stdout:?}");
        let quad = |i: usize| u64::from_ne_bytes(stdout[8 * i..8 * i + 8].try_into().unwrap());
        Self {
            first_entries: quad(0),
            second_entries: quad(1),
            frame_mask: quad(2) as u32,
            frame_mxcsr: quad(3) as u32,
            handler_mxcsr: quad(4) as u32,
            sum: quad(5) as u32,
            rip: quad(6),
            r12: quad(7),
            after_mxcsr: quad(8) as u32,
            frame_xstate_bv: quad(9),
            after_ymm5: [quad(10), quad(11), quad(12), quad(13)],
        }
    }

    /// The record without what may differ from the native one: the frame's
    /// rip and r12, and its XSTATE_BV bit for PKRU, which the guest lacks.
    fn comparable(self) -> Self {
        const PKRU: u64 = 1 << 9;
        Self {
            rip: 0,
            r12: 0,
            frame_xstate_bv: self.frame_xstate_bv & !PKRU,
            ..self
        }
    }
}

#[test]
fn a_frame_after_a_null_fpregs_return_carries_the_cpus_mxcsr_mask() {
    if bounded("a_frame_after_a_null_fpregs_return_carries_the_cpus_mxcsr_mask") {
        return;
    }
    // The guest loads MXCSR 0x7f80 and a nonzero YMM5 and sends itself
    // SIGUSR1, whose handler blocks SIGUSR2 (its sa_mask), sends it, sets
    // uc_mcontext.fpregs to NULL and returns. rt_sigreturn installs the
    // initial FPU state and unblocks SIGUSR2, which is delivered at once, at
    // the same return; its frame records that state with the CPU's
    // MXCSR_MASK, and with x87 and SSE state present and YMM state absent.
    // Its handler sets DAZ when that mask offers it, as a program probing
    // for DAZ does, and adds zero to a subnormal; it also writes the upper
    // half of YMM5 in its frame without marking YMM present, an edit
    // rt_sigreturn ignores.
    let program = Program::new(
        "mov $10,%edi\nlea usr1_action(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $13,%eax\nsyscall\n\
         mov $12,%edi\nlea usr2_action(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $13,%eax\nsyscall\n\
         mov $39,%eax\nsyscall\nmov %rax,pid(%rip)\nmov $186,%eax\nsyscall\nmov %rax,tid(%rip)\n\
         ldmxcsr loaded(%rip)\nvmovdqu loaded_ymm5(%rip),%ymm5\n\
         mov pid(%rip),%rdi\nmov tid(%rip),%rsi\nmov $10,%edx\nlea 1f(%rip),%r12\n\
         mov $234,%eax\nsyscall\n1: stmxcsr rec+64(%rip)\nvmovdqu %ymm5,rec+80(%rip)\n\
         mov $1,%edi\nlea rec(%rip),%rsi\nmov $112,%edx\nmov $1,%eax\nsyscall\n\
         xor %edi,%edi\nmov $231,%eax\nsyscall\nud2\n\
         first: mov %rdx,%r9\nmov pid(%rip),%rdi\nmov tid(%rip),%rsi\nmov $12,%edx\nmov $234,%eax\nsyscall\n\
         movq $0,224(%r9)\nincq rec(%rip)\nret\n\
         second: mov 224(%rdx),%r8\nmov 28(%r8),%eax\nmov %rax,rec+16(%rip)\n\
         mov 24(%r8),%eax\nmov %rax,rec+24(%rip)\nstmxcsr rec+32(%rip)\n\
         mov 168(%rdx),%rax\nmov %rax,rec+48(%rip)\nmov 72(%rdx),%rax\nmov %rax,rec+56(%rip)\n\
         mov 512(%r8),%rax\nmov %rax,rec+72(%rip)\n\
         movabs $0x6666666666666666,%rax\nmov %rax,656(%r8)\nmov %rax,664(%r8)\n\
         testl $0x40,28(%r8)\njz 2f\nldmxcsr daz(%rip)\n\
         2: movss subnormal(%rip),%xmm0\naddss zero(%rip),%xmm0\nmovd %xmm0,%eax\nmov %rax,rec+40(%rip)\n\
         incq rec+8(%rip)\nret\n\
         restorer: mov $15,%eax\nsyscall\nud2\n\
         .data\n.balign 8\nusr1_action: .quad first, 0x4000004, restorer, 0x800\n\
         usr2_action: .quad second, 0x4000004, restorer, 0\npid: .quad 0\ntid: .quad 0\n\
         loaded: .long 0x7f80\ndaz: .long 0x1fc0\nsubnormal: .long 1\nzero: .long 0\n\
         .balign 32\nloaded_ymm5: .quad 0x1111111111111111, 0x2222222222222222, \
         0x3333333333333333, 0x4444444444444444\n\
         .balign 8\nrec: .zero 112\n",
    );
    let native = QueuedRecord::read(&native_stdout(&program));
    let mask = cpu_mxcsr_mask();
    let expected = QueuedRecord {
        first_entries: 1,
        second_entries: 1,
        frame_mask: mask,
        frame_mxcsr: 0x1f80,
        handler_mxcsr: 0x1f80,
        sum: if mask & DAZ != 0 { 0 } else { 1 },
        after_mxcsr: 0x1f80,
        after_ymm5: [0; 4],
        ..native
    };
    assert_eq!(native, expected, "native");
    assert_eq!(native.rip, native.r12, "native: {native:x?}");
    assert_eq!(
        native.comparable().frame_xstate_bv,
        0b011,
        "native: {native:x?}"
    );
    for (name, stdout) in kvm_stdouts(&program) {
        let record = QueuedRecord::read(&stdout);
        assert_eq!(record.rip, record.r12, "{name}: {record:x?}");
        assert_eq!(
            record.comparable(),
            native.comparable(),
            "{name}: {record:x?}, native {native:x?}"
        );
    }
}

// Every eligible signal at one return to user mode
// (<https://github.com/rrnewton/reverie/issues/1188>).
//
// Linux's `exit_to_user_mode_loop` calls `arch_do_signal_or_restart` again
// while a signal is pending that the mask leaves unblocked: each frame goes on
// top of the last, so the handler of the last signal selected runs first, and
// its frame saves the handler of the frame below at its first instruction. The
// guests below make two or more signals eligible at one return, at each of the
// backend's delivery points, and log what every handler finds at its first
// instruction. Each log is compared with the same binary's natively, run under
// a minimal ptrace tracer (see [`traced_native`]), which stops the guest where a
// Tool's signal hook runs.

/// Quads in one entry of a stacked-signal guest's log (see [`record_entry`]);
/// [`q`] names them.
const ENTRY_QUADS: usize = 40;
/// Entries a stacked-signal guest's log holds.
const MAX_ENTRIES: usize = 8;
/// Quads before the entries: their count, the signal mask and the alternate
/// stack (sp, flags, size) once the handlers have returned, the alternate
/// stack as [`QUERY_STACK`] found it, if the guest ran it (zeros otherwise),
/// and [`RESULTS`] syscall results the guest kept with [`keep_result`].
const HEADER_QUADS: usize = 8 + RESULTS;
/// The syscall results a stacked-signal guest can keep in its header.
const RESULTS: usize = 4;
/// The bytes a stacked-signal guest writes to stdout before it exits.
const LOG_BYTES: usize = 8 * (HEADER_QUADS + MAX_ENTRIES * ENTRY_QUADS);

/// The quads of a log entry: what the handler's registers held at its first
/// instruction (rdi, rsp, rsi, rdx, rflags and its own address), and then what
/// its siginfo and ucontext hold.
mod q {
    pub const SIGNAL: usize = 0;
    pub const ENTRY_RSP: usize = 1;
    pub const ENTRY_RSI: usize = 2;
    pub const ENTRY_RDX: usize = 3;
    pub const ENTRY_RFLAGS: usize = 4;
    pub const HANDLER: usize = 5;
    pub const SI_SIGNO: usize = 6;
    pub const SI_CODE: usize = 7;
    pub const UC_FLAGS: usize = 8;
    pub const UC_LINK: usize = 9;
    pub const SS_SP: usize = 10;
    pub const SS_FLAGS: usize = 11;
    pub const SS_SIZE: usize = 12;
    pub const RDI: usize = 13;
    pub const RSI: usize = 14;
    pub const RBP: usize = 15;
    pub const RBX: usize = 16;
    pub const RDX: usize = 17;
    pub const RAX: usize = 18;
    pub const RCX: usize = 19;
    pub const RSP: usize = 20;
    pub const RIP: usize = 21;
    pub const EFLAGS: usize = 22;
    pub const ERR: usize = 23;
    pub const TRAPNO: usize = 24;
    pub const OLDMASK: usize = 25;
    pub const CR2: usize = 26;
    pub const SIGMASK: usize = 27;
    pub const R12: usize = 28;
    pub const MXCSR: usize = 29;
    /// The x87 control word, then the status word at bit 16 and the
    /// abridged tag word at bit 32.
    pub const X87: usize = 30;
    pub const XSTATE_BV: usize = 31;
    /// The low quad of XMM5.
    pub const XMM5: usize = 32;
}

/// Every logged quad a stacked-signal test compares between runs, by name.
/// The handler's rsp, rsi and rdx at its first instruction are addresses that
/// the run's signal-frame size decides, so they are compared only through the
/// frames that saved them (see [`comparable`]).
const COMPARED: [(&str, usize); 30] = [
    ("signal", q::SIGNAL),
    ("entry rflags", q::ENTRY_RFLAGS),
    ("handler", q::HANDLER),
    ("si_signo", q::SI_SIGNO),
    ("si_code", q::SI_CODE),
    ("uc_flags", q::UC_FLAGS),
    ("uc_link", q::UC_LINK),
    ("ss_sp", q::SS_SP),
    ("ss_flags", q::SS_FLAGS),
    ("ss_size", q::SS_SIZE),
    ("rdi", q::RDI),
    ("rsi", q::RSI),
    ("rbp", q::RBP),
    ("rbx", q::RBX),
    ("rdx", q::RDX),
    ("rax", q::RAX),
    ("rcx", q::RCX),
    ("rsp", q::RSP),
    ("rip", q::RIP),
    ("eflags", q::EFLAGS),
    ("err", q::ERR),
    ("trapno", q::TRAPNO),
    ("oldmask", q::OLDMASK),
    ("cr2", q::CR2),
    ("uc_sigmask", q::SIGMASK),
    ("r12", q::R12),
    ("mxcsr", q::MXCSR),
    ("x87", q::X87),
    ("xstate_bv", q::XSTATE_BV),
    ("xmm5", q::XMM5),
];

/// Assembly, at the first instruction of the handler for `signal`, that
/// appends an entry to `log` and counts it in the first header quad. It reads
/// rflags first, before any instruction that sets flags, and uses rax, rcx,
/// r8 to r11 and the stack below rsp, all free in a handler that returns
/// through rt_sigreturn.
fn record_entry(signal: i32) -> String {
    let at = |quad: usize| 8 * quad;
    let mut code = format!(
        "pushfq\npop %r8\n\
         mov header(%rip),%r11\ncmp ${MAX_ENTRIES},%r11\njae 8f\nlea 1(%r11),%r10\nmov %r10,header(%rip)\n\
         imul ${},%r11,%r9\nlea log(%rip),%r10\nadd %r10,%r9\nmov %r8,{}(%r9)\n\
         mov %rdi,{}(%r9)\nmov %rsp,{}(%r9)\nmov %rsi,{}(%r9)\nmov %rdx,{}(%r9)\n\
         lea handler_{signal}(%rip),%rax\nmov %rax,{}(%r9)\n",
        8 * ENTRY_QUADS,
        at(q::ENTRY_RFLAGS),
        at(q::SIGNAL),
        at(q::ENTRY_RSP),
        at(q::ENTRY_RSI),
        at(q::ENTRY_RDX),
        at(q::HANDLER),
    );
    // Quads of the siginfo (rsi) and the ucontext (rdx). The 32-bit fields
    // are read alone: Linux leaves the padding after them unwritten.
    let quads = [
        ("0(%rsi)", q::SI_SIGNO),
        ("0(%rdx)", q::UC_FLAGS),
        ("8(%rdx)", q::UC_LINK),
        ("16(%rdx)", q::SS_SP),
        ("32(%rdx)", q::SS_SIZE),
        ("104(%rdx)", q::RDI),
        ("112(%rdx)", q::RSI),
        ("120(%rdx)", q::RBP),
        ("128(%rdx)", q::RBX),
        ("136(%rdx)", q::RDX),
        ("144(%rdx)", q::RAX),
        ("152(%rdx)", q::RCX),
        ("160(%rdx)", q::RSP),
        ("168(%rdx)", q::RIP),
        ("176(%rdx)", q::EFLAGS),
        ("192(%rdx)", q::ERR),
        ("200(%rdx)", q::TRAPNO),
        ("208(%rdx)", q::OLDMASK),
        ("216(%rdx)", q::CR2),
        ("296(%rdx)", q::SIGMASK),
        ("72(%rdx)", q::R12),
    ];
    for (source, quad) in quads {
        code += &format!("mov {source},%rax\nmov %rax,{}(%r9)\n", at(quad));
    }
    for (source, quad) in [("8(%rsi)", q::SI_CODE), ("24(%rdx)", q::SS_FLAGS)] {
        code += &format!("movl {source},%eax\nmov %rax,{}(%r9)\n", at(quad));
    }
    // The frame's FPU state, which uc_mcontext.fpregs points to.
    code += &format!(
        "mov 224(%rdx),%r8\nmovl 24(%r8),%eax\nmov %rax,{}(%r9)\n\
         movzwl 0(%r8),%eax\nmovzwl 2(%r8),%ecx\nshl $16,%rcx\nor %rcx,%rax\n\
         movzbl 4(%r8),%ecx\nshl $32,%rcx\nor %rcx,%rax\nmov %rax,{}(%r9)\n\
         mov 512(%r8),%rax\nmov %rax,{}(%r9)\nmov 240(%r8),%rax\nmov %rax,{}(%r9)\n8:\n",
        at(q::MXCSR),
        at(q::X87),
        at(q::XSTATE_BV),
        at(q::XMM5),
    );
    code
}

/// A signal handler of a stacked-signal guest.
struct Handler {
    signal: i32,
    /// sigaction flags besides SA_SIGINFO and SA_RESTORER.
    flags: u64,
    /// sa_mask.
    mask: u64,
    /// What the handler runs after it logs its entry, before it returns.
    actions: String,
}

impl Handler {
    fn new(signal: i32) -> Self {
        Self {
            signal,
            flags: 0,
            mask: 0,
            actions: String::new(),
        }
    }

    fn flags(self, flags: u64) -> Self {
        Self { flags, ..self }
    }

    fn mask(self, mask: u64) -> Self {
        Self { mask, ..self }
    }

    fn actions(self, actions: &str) -> Self {
        Self {
            actions: actions.to_owned(),
            ..self
        }
    }
}

const SA_ONSTACK: u64 = 0x0800_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SS_AUTODISARM: u64 = 1 << 31;
const SS_DISABLE: u64 = 2;

/// The bit of `signal` in a signal mask.
const fn bit(signal: i32) -> u64 {
    1 << (signal - 1)
}

/// Values the stacked-signal guests keep in rbx and rbp, which their frames
/// then save.
const RBX: u64 = 0x0123_4567_89ab_cdef;
const RBP: u64 = 0x1122_3344_5566_7788;

/// The MXCSR and XMM5 low quad a stacked-signal guest loads with
/// [`LOAD_FPU`]: the interrupted state that the first frame saves.
const LOADED_MXCSR: u64 = 0x7f80;
const LOADED_XMM5: u64 = 0x1111_1111_1111_1111;
const LOAD_FPU: &str = "ldmxcsr loaded_mxcsr(%rip)\nmovdqu loaded_xmm5(%rip),%xmm5\n";

/// A static guest that keeps its argument count in `argc`, switches to its own
/// stack, sets rbx and rbp to [`RBX`] and [`RBP`], disables the alternate stack
/// (unless it has arguments: an image the guest itself executed), installs
/// `handlers` (exiting with status 1 if that or an install fails), keeps its
/// pid and tid in `pid` and `tid`, runs `body`, writes its log ([`StackedLog`])
/// to stdout and exits with status 0. `data` adds to its data, and `bss` to its
/// zeroed memory.
///
/// The guest disables the alternate stack itself because a frame's uc_stack
/// saves the stack's flags as the task holds them, and exec clears only the
/// stack's address and size (`begin_new_exec` in fs/exec.c): natively a guest
/// would otherwise keep its launcher's flags (0 from a Rust test thread, which
/// has an alternate stack of its own), while KVM starts every guest with
/// Linux's initial SS_DISABLE.
fn stacked_program(handlers: &[Handler], body: &str, data: &str, bss: &str) -> Program {
    let install: String = handlers
        .iter()
        .map(|handler| {
            format!(
                "mov ${s},%edi\nlea action_{s}(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $13,%eax\n\
                 syscall\ntest %rax,%rax\njnz 99f\n",
                s = handler.signal
            )
        })
        .collect();
    let code: String = handlers
        .iter()
        .map(|handler| {
            format!(
                "handler_{}:\n{}{}ret\n",
                handler.signal,
                record_entry(handler.signal),
                handler.actions
            )
        })
        .collect();
    let actions: String = handlers
        .iter()
        .map(|handler| {
            format!(
                "action_{s}: .quad handler_{s}, {:#x}, restorer, {:#x}\n",
                HANDLER_FLAGS | handler.flags,
                handler.mask,
                s = handler.signal
            )
        })
        .collect();
    Program::new(&format!(
        "mov (%rsp),%rax\nmov %rax,argc(%rip)\n\
         lea stack_end(%rip),%rsp\nmovabs ${RBX:#x},%rbx\nmovabs ${RBP:#x},%rbp\n\
         cmpq $1,argc(%rip)\njne 97f\n\
         lea no_altstack(%rip),%rdi\nxor %esi,%esi\nmov $131,%eax\nsyscall\ntest %rax,%rax\njnz 99f\n\
         97:\n{install}\
         mov $39,%eax\nsyscall\nmov %rax,pid(%rip)\nmov $186,%eax\nsyscall\nmov %rax,tid(%rip)\n\
         {body}\
         xor %edi,%edi\nxor %esi,%esi\nlea header+8(%rip),%rdx\nmov $8,%r10d\nmov $14,%eax\nsyscall\n\
         xor %edi,%edi\nlea header+16(%rip),%rsi\nmov $131,%eax\nsyscall\n\
         mov $1,%edi\nlea header(%rip),%rsi\nmov ${LOG_BYTES},%edx\nmov $1,%eax\nsyscall\n\
         xor %edi,%edi\nmov $231,%eax\nsyscall\nud2\n\
         99: mov $1,%edi\nmov $231,%eax\nsyscall\nud2\n\
         {code}\
         restorer: mov $15,%eax\nsyscall\nud2\n\
         .data\n.balign 8\n{actions}pid: .quad 0\ntid: .quad 0\nargc: .quad 0\n\
         no_altstack: .quad 0\n.long {SS_DISABLE}, 0\n.quad 0\n\
         loaded_mxcsr: .long {LOADED_MXCSR:#x}\n.balign 16\nloaded_xmm5: .quad {LOADED_XMM5:#x}, 0\n\
         {data}\n.balign 64\nheader: .zero {}\nlog: .zero {}\n\
         .bss\n.balign 64\nstack: .skip 65536\nstack_end:\n{bss}\n",
        8 * HEADER_QUADS,
        8 * MAX_ENTRIES * ENTRY_QUADS,
    ))
}

/// rt_sigprocmask(`how`, the mask at label `mask`, NULL, 8).
fn sigprocmask(how: i32, mask: &str) -> String {
    format!(
        "mov ${how},%edi\nlea {mask}(%rip),%rsi\nxor %edx,%edx\nmov $8,%r10d\nmov $14,%eax\nsyscall\n"
    )
}

/// tgkill of `signal` to the guest's main thread.
fn tgkill_self(signal: i32) -> String {
    format!("mov pid(%rip),%rdi\nmov tid(%rip),%rsi\nmov ${signal},%edx\nmov $234,%eax\nsyscall\n")
}

/// kill of `signal` to the guest's process.
fn kill_self(signal: i32) -> String {
    format!("mov pid(%rip),%rdi\nmov ${signal},%esi\nmov $62,%eax\nsyscall\n")
}

/// Loads [`LOAD_FPU`], blocks the signals at label `pending`, makes them
/// pending with `sends`, and unblocks them with an rt_sigprocmask whose return
/// r12 marks.
fn unblocked_at_syscall_return(sends: &str) -> String {
    format!(
        "{LOAD_FPU}{}{sends}lea 1f(%rip),%r12\n{}1:\n",
        sigprocmask(libc::SIG_BLOCK, "pending"),
        sigprocmask(libc::SIG_UNBLOCK, "pending")
    )
}

/// sigaltstack(NULL, the header's last three quads): the alternate stack as
/// the guest finds it there, which [`StackedLog::queried_stack`] reads.
const QUERY_STACK: &str = "xor %edi,%edi\nlea header+40(%rip),%rsi\nmov $131,%eax\nsyscall\n";

/// Keeps rax, a syscall's result, as result `index` of the guest's header
/// ([`StackedLog::results`]).
fn keep_result(index: usize) -> String {
    assert!(index < RESULTS);
    format!("mov %rax,header+{}(%rip)\n", 8 * (8 + index))
}

/// sigaltstack(the stack_t at `label`, NULL), its result kept as result
/// `index`.
fn set_altstack(label: &str, index: usize) -> String {
    format!(
        "lea {label}(%rip),%rdi\nxor %esi,%esi\nmov $131,%eax\nsyscall\n{}",
        keep_result(index)
    )
}

/// A stack_t at `label`: `sp` (an address or label), `flags` and `size`.
fn stack_t(label: &str, sp: &str, flags: u64, size: u64) -> String {
    format!("{label}: .quad {sp}\n.long {flags:#x}, 0\n.quad {size}\n")
}

/// Installs the alternate stack that [`altstack_data`] describes, exiting with
/// status 1 if that fails.
const ARM_ALTSTACK: &str = "lea altstack_desc(%rip),%rdi\nxor %esi,%esi\nmov $131,%eax\nsyscall\n\
     test %rax,%rax\njnz 99f\n";

/// The stack_t of a 64 KiB alternate stack at [`ALTSTACK`] with `flags`.
fn altstack_data(flags: u64) -> String {
    format!("altstack_desc: .quad altstack\n.long {flags:#x}, 0\n.quad 65536\n")
}

/// The memory of the alternate stack of [`altstack_data`].
const ALTSTACK: &str = ".balign 64\naltstack: .skip 65536\n";

/// One handler's entry in a stacked-signal guest's log.
#[derive(Clone, Copy, PartialEq, Eq)]
struct LoggedEntry([u64; ENTRY_QUADS]);

impl std::fmt::Debug for LoggedEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = f.debug_map();
        for (name, quad) in [
            ("entry rsp", q::ENTRY_RSP),
            ("entry rsi", q::ENTRY_RSI),
            ("entry rdx", q::ENTRY_RDX),
        ]
        .into_iter()
        .chain(COMPARED)
        {
            map.entry(&name, &format_args!("{:#x}", self.0[quad]));
        }
        map.finish()
    }
}

/// What a stacked-signal guest writes to stdout before it exits.
#[derive(Debug)]
struct StackedLog {
    entries: Vec<LoggedEntry>,
    /// The signal mask once every handler has returned.
    final_mask: u64,
    /// The alternate stack then: sp, flags and size.
    final_stack: [u64; 3],
    /// The alternate stack where the guest ran [`QUERY_STACK`]: sp, flags
    /// and size, or zeros.
    queried_stack: [u64; 3],
    /// The syscall results the guest kept with [`keep_result`], or zeros.
    results: [u64; RESULTS],
}

impl StackedLog {
    fn read(stdout: &[u8]) -> Self {
        assert_eq!(stdout.len(), LOG_BYTES, "{stdout:?}");
        let quad = |i: usize| u64::from_ne_bytes(stdout[8 * i..8 * i + 8].try_into().unwrap());
        let count = usize::try_from(quad(0)).unwrap();
        assert!(count <= MAX_ENTRIES, "{count} entries");
        let entries = (0..count)
            .map(|entry| {
                let first = HEADER_QUADS + entry * ENTRY_QUADS;
                LoggedEntry(std::array::from_fn(|i| quad(first + i)))
            })
            .collect();
        Self {
            entries,
            final_mask: quad(1),
            final_stack: [quad(2), quad(3) & u64::from(u32::MAX), quad(4)],
            queried_stack: [quad(5), quad(6) & u64::from(u32::MAX), quad(7)],
            results: std::array::from_fn(|i| quad(8 + i)),
        }
    }

    /// The signal of each entry, in the order the handlers ran.
    fn signals(&self) -> Vec<i32> {
        self.entries
            .iter()
            .map(|entry| entry.0[q::SIGNAL] as i32)
            .collect()
    }

    fn entry(&self, index: usize) -> &[u64; ENTRY_QUADS] {
        &self.entries[index].0
    }
}

/// A logged quad, as the runs compare it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    Value(u64),
    /// The value the handler of this entry found in this register at its
    /// first instruction: an address that the run's frame size decides.
    EntryOf(usize, &'static str),
    /// Left out of the comparison: where an interrupt landed.
    Varies,
}

/// The entries of `log` as the runs compare them: every [`COMPARED`] quad,
/// with a saved rsp, rsi or rdx that some handler found in that register at
/// its first instruction written as that entry, XSTATE_BV cut to the x87, SSE
/// and AVX bits (the native frame adds what the host has, such as PKRU), and
/// the quads `varies` names left out. Every interrupted context the guests
/// compare is a call that takes no pid or tid, which differ between runs.
fn comparable(
    log: &StackedLog,
    varies: &dyn Fn(usize, usize) -> bool,
) -> Vec<Vec<(&'static str, Seen)>> {
    let entry_of = |value: u64, register: usize| {
        let (name, at) = match register {
            q::RSP => ("rsp", q::ENTRY_RSP),
            q::RSI => ("rsi", q::ENTRY_RSI),
            q::RDX => ("rdx", q::ENTRY_RDX),
            _ => return None,
        };
        log.entries
            .iter()
            .position(|entry| entry.0[at] == value)
            .map(|index| Seen::EntryOf(index, name))
    };
    log.entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            COMPARED
                .iter()
                .map(|&(name, quad)| {
                    let value = entry.0[quad];
                    let seen = if varies(index, quad) {
                        Seen::Varies
                    } else if quad == q::XSTATE_BV {
                        Seen::Value(value & 0b111)
                    } else {
                        entry_of(value, quad).unwrap_or(Seen::Value(value))
                    };
                    (name, seen)
                })
                .collect()
        })
        .collect()
}

/// Checks that the frame of entry `top` went on top of entry `below`'s at the
/// same return, as Linux builds it: it saved `below`'s handler at its first
/// instruction (rip the handler, rsp its frame, rdi, rsi and rdx its
/// arguments, rax 0, its rflags) and the FPU state the delivery below
/// installed, Linux's initial state, and it lies below that frame and its red
/// zone.
fn assert_stacked_on(log: &StackedLog, top: usize, below: usize, context: &str) {
    let (t, b) = (log.entry(top), log.entry(below));
    let context = format!("{context}: entry {top} on entry {below}: {log:#x?}");
    assert_eq!(
        [
            t[q::RIP],
            t[q::RSP],
            t[q::RDI],
            t[q::RSI],
            t[q::RDX],
            t[q::RAX],
            t[q::EFLAGS]
        ],
        [
            b[q::HANDLER],
            b[q::ENTRY_RSP],
            b[q::SIGNAL],
            b[q::ENTRY_RSI],
            b[q::ENTRY_RDX],
            0,
            b[q::ENTRY_RFLAGS]
        ],
        "{context}"
    );
    assert_eq!(
        [t[q::MXCSR], t[q::X87], t[q::XSTATE_BV] & 0b111, t[q::XMM5]],
        [0x1f80, 0x037f, 0b011, 0],
        "{context}"
    );
    assert!(t[q::ENTRY_RSP] + 128 < b[q::ENTRY_RSP], "{context}");
}

/// Checks that entry `index` saved the interrupted code that loaded
/// [`LOAD_FPU`], at the instruction r12 marks, with rbx and rbp intact.
fn assert_interrupted_at_mark(log: &StackedLog, index: usize, context: &str) {
    let e = log.entry(index);
    let context = format!("{context}: entry {index}: {log:#x?}");
    assert_eq!(e[q::RIP], e[q::R12], "{context}");
    assert_eq!([e[q::RBX], e[q::RBP]], [RBX, RBP], "{context}");
    assert_eq!(
        [e[q::MXCSR], e[q::XMM5], e[q::XSTATE_BV] & 0b111],
        [LOADED_MXCSR, LOADED_XMM5, 0b011],
        "{context}"
    );
}

/// How [`traced_native`] answers a stop of the native guest. A Tool's signal
/// hook runs where ptrace stops a guest for a signal, so the tracer can put a
/// native run in the state a Tool's callbacks or the backend's own delivery
/// points put a KVM run in, without depending on the host scheduler.
#[derive(Clone, Copy, Debug)]
enum Tracing {
    /// Delivers each signal at its stop.
    Deliver,
    /// At the main thread's first stop for `first`, waits until `then` is
    /// pending for it as well, and delivers `first`: as a Tool's timer
    /// callback holds a thread while a sibling sends it both.
    HoldUntilPending { first: i32, then: i32 },
    /// Suppresses `deferred` at the main thread's stops until it stops for
    /// `fault`, then sends `deferred` to it again and delivers `fault`, so
    /// that `deferred` is pending at the fault's return: as when a sibling
    /// sends it to a thread that runs no syscall until it faults, under a
    /// backend with no preemption.
    DeferUntil { deferred: i32, fault: i32 },
    /// Holds each new thread at its first stop, before its first
    /// instruction, until both `signals` have reached it (pending, or the
    /// one that stop dequeued): as a Tool that holds a new thread's start
    /// until another thread has sent them.
    HoldNewThread { signals: [i32; 2] },
}

/// A native run under [`traced_native`]: its stdout, and the signals the
/// tracer delivered, in the order of their stops.
struct TracedRun {
    stdout: Vec<u8>,
    delivered: Vec<i32>,
    /// The synthetic SIGSTOPs of new threads that the tracer suppressed.
    synthetic_stops: usize,
}

/// What PTRACE_GETSIGINFO shows for a stop that is not a ptrace event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopInfo {
    /// A signal-delivery stop: the signal, its si_code and its si_pid.
    Signal { signo: i32, code: i32, sender: i32 },
    /// A group-stop report, which has no siginfo (EINVAL).
    GroupStop,
}

/// Reads the siginfo of thread `tid`'s stop.
fn stop_info(tid: libc::pid_t) -> StopInfo {
    // SAFETY: an all-zero siginfo_t is valid, and PTRACE_GETSIGINFO writes
    // one siginfo_t into it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let got = unsafe {
        libc::ptrace(
            libc::PTRACE_GETSIGINFO,
            tid,
            0,
            &mut info as *mut libc::siginfo_t,
        )
    };
    if got == 0 {
        StopInfo::Signal {
            signo: info.si_signo,
            code: info.si_code,
            // SAFETY: the kill, tkill and queue layouts, the only ones
            // classify_sigstop accepts a sender from, hold si_pid here.
            sender: unsafe { info.si_pid() },
        }
    } else {
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EINVAL),
            "PTRACE_GETSIGINFO of thread {tid}: {error}"
        );
        StopInfo::GroupStop
    }
}

/// What the native tracer does with a SIGSTOP stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SigstopAction {
    /// Suppresses the SIGSTOP the tracer's own PTRACE_O_TRACECLONE queued for
    /// a new thread, which no guest sent.
    SuppressSynthetic,
    /// Forwards a SIGSTOP that a task sent, as any other signal.
    Forward,
}

/// Classifies a SIGSTOP stop of thread `tid` of process `pid` by where the
/// signal came from, its si_code and si_pid. PTRACE_O_TRACECLONE adds SIGSTOP
/// to a new thread's pending set with no siginfo (`ptrace_init_task`,
/// <https://github.com/gregkh/linux/blob/v7.1.3/include/linux/ptrace.h#L200-L216>),
/// and `collect_signal` reports a signal with no siginfo as SI_USER from pid
/// 0 (<https://github.com/gregkh/linux/blob/v7.1.3/kernel/signal.c#L588-L600>),
/// whenever the thread dequeues it; a signal that a task sent with kill,
/// tkill, tgkill or rt_sigqueueinfo carries the sender's pid. So the
/// synthetic stop is known by its provenance, not by being a thread's first
/// SIGSTOP, and a sent SIGSTOP is forwarded even when it is. Anything else is
/// refused, as is a second synthetic stop or one for the main thread, which
/// the tracer never gets one for. A SIGSTOP a task sends while a new thread's
/// synthetic one is still pending merges into it (`legacy_queue`,
/// <https://github.com/gregkh/linux/blob/v7.1.3/kernel/signal.c#L1035-L1038>),
/// so the kernel itself keeps no trace of that send.
fn classify_sigstop(
    tid: libc::pid_t,
    pid: libc::pid_t,
    code: i32,
    sender: libc::pid_t,
    synthetic: &[libc::pid_t],
) -> Result<SigstopAction, String> {
    match (code, sender) {
        (libc::SI_USER, 0) if tid != pid && !synthetic.contains(&tid) => {
            Ok(SigstopAction::SuppressSynthetic)
        }
        (libc::SI_USER, 0) if tid == pid => {
            Err("SI_USER from pid 0 for the main thread".to_owned())
        }
        (libc::SI_USER, 0) => Err("a second SI_USER stop from pid 0".to_owned()),
        (libc::SI_USER | libc::SI_TKILL | libc::SI_QUEUE, sender) if sender > 0 => {
            Ok(SigstopAction::Forward)
        }
        (code, sender) => Err(format!("si_code {code} from pid {sender}")),
    }
}

/// Waits until every one of `signals` is pending for thread `tid` of process
/// `pid`, thread-directed or process-wide.
fn wait_pending(pid: libc::pid_t, tid: libc::pid_t, signals: &[i32]) {
    let path = format!("/proc/{pid}/task/{tid}/status");
    let wanted = signals.iter().fold(0, |mask, &signal| mask | bit(signal));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let status = std::fs::read_to_string(&path).unwrap();
        let pending = ["SigPnd:", "ShdPnd:"]
            .iter()
            .map(|key| {
                let line = status.lines().find(|line| line.starts_with(key)).unwrap();
                u64::from_str_radix(line[key.len()..].trim(), 16).unwrap()
            })
            .fold(0, |all, set| all | set);
        if pending & wanted == wanted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{signals:?} never became pending: {pending:#x}"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Runs `program` natively, the Linux reference, under a ptrace tracer that
/// answers its stops as `tracing` says, follows its threads, and checks that it
/// exits with status 0 and writes nothing to stderr. A tracer changes no frame
/// Linux builds: it only decides, at each signal's stop, whether and when the
/// signal is delivered.
fn traced_native(program: &Program, tracing: Tracing) -> TracedRun {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(&program.executable);
    command
        .current_dir(&program.directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SAFETY: ptrace(PTRACE_TRACEME) is async-signal-safe and touches no
    // memory the parent shares.
    unsafe {
        command.pre_exec(|| match libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        });
    }
    let mut child = command.spawn().unwrap();
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    // SAFETY: plain wait and ptrace calls on this test's own tracee.
    let resume = |tid: libc::pid_t, signal: i32| {
        let resumed = unsafe { libc::ptrace(libc::PTRACE_CONT, tid, 0, signal as libc::c_long) };
        assert_eq!(resumed, 0, "{}", std::io::Error::last_os_error());
    };
    // The stop at exec.
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::__WALL) },
        pid
    );
    assert!(
        libc::WIFSTOPPED(status) && libc::WSTOPSIG(status) == libc::SIGTRAP,
        "{status:#x}"
    );
    // An exec of the guest's own stops as an event, not as a SIGTRAP to
    // deliver.
    let options = libc::PTRACE_O_TRACECLONE | libc::PTRACE_O_TRACEEXEC | libc::PTRACE_O_EXITKILL;
    assert_eq!(
        unsafe { libc::ptrace(libc::PTRACE_SETOPTIONS, pid, 0, options) },
        0
    );
    resume(pid, 0);
    let mut threads = vec![pid];
    // The cloned threads whose synthetic SIGSTOP the tracer has suppressed.
    let mut synthetic = Vec::new();
    // Whether the tracer has forwarded a stop signal, which group-stop
    // reports then follow.
    let mut stop_forwarded = false;
    let mut delivered = Vec::new();
    let (mut held, mut faulted) = (false, false);
    let ending = loop {
        let tid = unsafe { libc::waitpid(-1, &mut status, libc::__WALL) };
        assert!(tid > 0, "waitpid: {}", std::io::Error::last_os_error());
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            if tid == pid {
                break status;
            }
            continue;
        }
        assert!(libc::WIFSTOPPED(status), "{status:#x}");
        let signal = libc::WSTOPSIG(status);
        if !threads.contains(&tid) {
            threads.push(tid);
            // A new thread's first stop, before its first instruction.
            if let Tracing::HoldNewThread { signals } = tracing {
                // This stop dequeued its signal; the others must be pending.
                let others: Vec<i32> = signals.into_iter().filter(|&s| s != signal).collect();
                wait_pending(pid, tid, &others);
            }
        }
        // A clone or exec event stop has no siginfo of its own to read.
        let info = (status >> 16 == 0).then(|| stop_info(tid));
        let deliver = if info.is_none() {
            // A clone or exec event stop.
            0
        } else if let Some(StopInfo::GroupStop) = info {
            // A group-stop report: it delivers nothing, and resuming from it
            // continues the thread.
            assert!(
                stop_forwarded,
                "a group-stop of thread {tid} with no stop signal forwarded"
            );
            0
        } else if let Some(StopInfo::Signal {
            signo,
            code,
            sender,
        }) = info
            && signal == libc::SIGSTOP
        {
            assert_eq!(signo, signal, "thread {tid}'s siginfo");
            match classify_sigstop(tid, pid, code, sender, &synthetic) {
                Ok(SigstopAction::SuppressSynthetic) => {
                    synthetic.push(tid);
                    0
                }
                Ok(SigstopAction::Forward) => {
                    stop_forwarded = true;
                    signal
                }
                Err(why) => panic!("an unclassifiable SIGSTOP stop of thread {tid}: {why}"),
            }
        } else {
            if [libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU].contains(&signal) {
                stop_forwarded = true;
            }
            match tracing {
                Tracing::HoldUntilPending { first, then }
                    if tid == pid && signal == first && !held =>
                {
                    held = true;
                    wait_pending(pid, tid, &[then]);
                    signal
                }
                Tracing::DeferUntil { deferred, .. }
                    if tid == pid && signal == deferred && !faulted =>
                {
                    0
                }
                Tracing::DeferUntil { deferred, fault } if tid == pid && signal == fault => {
                    faulted = true;
                    let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, deferred) };
                    assert_eq!(sent, 0, "{}", std::io::Error::last_os_error());
                    signal
                }
                _ => signal,
            }
        };
        if deliver != 0 {
            delivered.push(deliver);
        }
        resume(tid, deliver);
    };
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    // The tracer has reaped the child: its handle must not wait for it.
    std::mem::forget(child);
    assert!(
        libc::WIFEXITED(ending) && libc::WEXITSTATUS(ending) == 0 && stderr.is_empty(),
        "native: status {ending:#x}: {}",
        String::from_utf8_lossy(&stderr)
    );
    TracedRun {
        stdout,
        delivered,
        synthetic_stops: synthetic.len(),
    }
}

/// The order of the Tool's signal hooks in a run's events.
fn signal_hooks(events: &[Event]) -> Vec<i32> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Signalled { signal, .. } => Some(*signal),
            _ => None,
        })
        .collect()
}

/// The runs [`stacked_matches_native`] compared.
struct StackedRuns {
    /// The native log, for the caller's checks of Linux's frames.
    native: StackedLog,
    /// The log under the Tool, and the Tool's events.
    tool: StackedLog,
    events: Vec<Event>,
}

/// Runs `program` natively under `tracing` and on KVM, under the backend's
/// own runtime when `direct` and under a Tool following `plan` (whose
/// `deliver` delivers every signal), and checks every KVM log against the
/// native one ([`comparable`], with `varies`), and the order of the Tool's
/// signal hooks against the order of the tracer's deliveries.
fn stacked_matches_native(
    program: &Program,
    tracing: Tracing,
    direct: bool,
    plan: Plan,
    varies: &dyn Fn(usize, usize) -> bool,
) -> StackedRuns {
    let native = traced_native(program, tracing);
    let native_log = StackedLog::read(&native.stdout);
    let expected = comparable(&native_log, varies);
    if direct {
        let log = StackedLog::read(&direct_stdout(program));
        assert_eq!(
            comparable(&log, varies),
            expected,
            "runtime {log:#x?}, native {native_log:#x?}"
        );
        assert_eq!(
            (
                log.final_mask,
                log.final_stack,
                log.queried_stack,
                log.results
            ),
            (
                native_log.final_mask,
                native_log.final_stack,
                native_log.queried_stack,
                native_log.results
            ),
            "runtime"
        );
    }
    let plan = Plan {
        deliver: true,
        ..plan
    };
    let (events, stdout) = tool_stdout(program, plan);
    let log = StackedLog::read(&stdout);
    assert_eq!(
        comparable(&log, varies),
        expected,
        "tool {log:#x?}, native {native_log:#x?}, {events:?}"
    );
    assert_eq!(
        (
            log.final_mask,
            log.final_stack,
            log.queried_stack,
            log.results
        ),
        (
            native_log.final_mask,
            native_log.final_stack,
            native_log.queried_stack,
            native_log.results
        ),
        "tool"
    );
    assert_eq!(
        signal_hooks(&events),
        native.delivered,
        "tool hooks against the native tracer's deliveries: {events:?}"
    );
    StackedRuns {
        native: native_log,
        tool: log,
        events,
    }
}

/// [`stacked_matches_native`] for a guest whose signals all come at syscall
/// returns: natively, its tracer delivers each signal as it comes, and the
/// run without a tracer writes the same log; on KVM, under both runtimes.
/// Returns the native log.
fn stacked_at_syscall_returns_matches_native(program: &Program) -> StackedLog {
    let untraced = native_stdout(program);
    let runs = stacked_matches_native(program, Tracing::Deliver, true, Plan::default(), &|_, _| {
        false
    });
    assert_eq!(
        StackedLog::read(&untraced).entries,
        runs.native.entries,
        "native runs with and without the tracer"
    );
    runs.native
}

const SIGUSR1: i32 = libc::SIGUSR1;
const SIGUSR2: i32 = libc::SIGUSR2;
const SIGWINCH: i32 = libc::SIGWINCH;

/// A guest whose SIGUSR1 and SIGUSR2 are pending and blocked when it unblocks
/// both, with an rt_sigprocmask whose return r12 marks.
fn pending_pair_program() -> Program {
    stacked_program(
        &[Handler::new(SIGUSR1), Handler::new(SIGUSR2)],
        &unblocked_at_syscall_return(&(tgkill_self(SIGUSR1) + &tgkill_self(SIGUSR2))),
        &format!("pending: .quad {:#x}\n", bit(SIGUSR1) | bit(SIGUSR2)),
        "",
    )
}

#[test]
fn two_signals_pending_at_a_syscall_return_stack_their_frames() {
    if bounded("two_signals_pending_at_a_syscall_return_stack_their_frames") {
        return;
    }
    // SIGUSR1 and SIGUSR2 are pending and blocked; the guest unblocks both.
    // Linux builds SIGUSR1's frame at that call's return, then SIGUSR2's on
    // top, under the mask SIGUSR1's delivery left: SIGUSR2's handler runs
    // first and returns to SIGUSR1's, which returns to the guest.
    let log = stacked_at_syscall_returns_matches_native(&pending_pair_program());
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{log:#x?}");
    assert_stacked_on(&log, 0, 1, context);
    assert_interrupted_at_mark(&log, 1, context);
    let (top, below) = (log.entry(0), log.entry(1));
    // SIGUSR1's frame saves the mask its call left, none; SIGUSR2's, the
    // mask SIGUSR1's handler runs under.
    assert_eq!(
        [below[q::SIGMASK], top[q::SIGMASK], below[q::RAX]],
        [0, bit(SIGUSR1), 0],
        "{log:#x?}"
    );
    assert_eq!(log.final_mask, 0, "{log:#x?}");
}

#[test]
fn each_stacked_signal_has_its_own_permit_hook_and_receipt() {
    if bounded("each_stacked_signal_has_its_own_permit_hook_and_receipt") {
        return;
    }
    // The guest of `two_signals_pending_at_a_syscall_return_stack_their_frames`,
    // under a Tool that controls signal delivery with permits and lets the
    // backend skip empty boundaries, as a scheduler that runs one thread at a
    // time does. Each signal at the unblocking call's return is a boundary of
    // its own, as each signal has its own stop under ptrace: a permit, the
    // signal's hook, which sees that call as orig_rax, and the permit's
    // Caught receipt, SIGUSR1's before SIGUSR2's.
    let plan = Plan {
        permits: true,
        signal_registers: true,
        ..Plan::default()
    };
    let runs = stacked_matches_native(
        &pending_pair_program(),
        Tracing::Deliver,
        false,
        plan,
        &|_, _| false,
    );
    assert_eq!(runs.native.signals(), [SIGUSR2, SIGUSR1]);
    let boundaries: Vec<String> = runs
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Permit(sequence) => Some(format!("permit {sequence}")),
            Event::Signalled { signal, .. } => Some(format!("hook {signal}")),
            Event::Registers(r) => Some(format!("orig_rax {}", r.orig_rax)),
            Event::Receipt(sequence, outcome) => Some(format!("receipt {sequence} {outcome}")),
            _ => None,
        })
        .collect();
    let unblock = libc::SYS_rt_sigprocmask;
    assert_eq!(
        boundaries,
        [
            "permit 1".to_owned(),
            format!("hook {SIGUSR1}"),
            format!("orig_rax {unblock}"),
            "receipt 1 Caught".to_owned(),
            "permit 2".to_owned(),
            format!("hook {SIGUSR2}"),
            format!("orig_rax {unblock}"),
            "receipt 2 Caught".to_owned(),
        ],
        "{:?}",
        runs.events
    );
}

#[test]
fn two_signals_pending_at_rt_sigsuspend_stack_their_frames() {
    if bounded("two_signals_pending_at_rt_sigsuspend_stack_their_frames") {
        return;
    }
    // The same pending pair, released by an rt_sigsuspend with an empty
    // mask. SIGUSR1's frame saves the mask rt_sigsuspend saved, which blocks
    // both; SIGUSR2's frame saves the temporary mask with SIGUSR1 added, so
    // SIGUSR2 is delivered at the same return. Delivering one frame per
    // return would leave SIGUSR2 pending, and blocked again once SIGUSR1's
    // handler returned.
    let program = stacked_program(
        &[Handler::new(SIGUSR1), Handler::new(SIGUSR2)],
        &format!(
            "{LOAD_FPU}{}{}{}lea 1f(%rip),%r12\nlea empty(%rip),%rdi\nmov $8,%esi\nmov $130,%eax\nsyscall\n1:\n",
            sigprocmask(libc::SIG_BLOCK, "pending"),
            tgkill_self(SIGUSR1),
            tgkill_self(SIGUSR2)
        ),
        &format!(
            "pending: .quad {:#x}\nempty: .quad 0\n",
            bit(SIGUSR1) | bit(SIGUSR2)
        ),
        "",
    );
    let log = stacked_at_syscall_returns_matches_native(&program);
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{log:#x?}");
    assert_stacked_on(&log, 0, 1, context);
    assert_interrupted_at_mark(&log, 1, context);
    let (top, below) = (log.entry(0), log.entry(1));
    let both = bit(SIGUSR1) | bit(SIGUSR2);
    assert_eq!(
        [below[q::SIGMASK], top[q::SIGMASK], below[q::RAX]],
        [both, bit(SIGUSR1), -i64::from(libc::EINTR) as u64],
        "{log:#x?}"
    );
    assert_eq!(log.final_mask, both, "{log:#x?}");
}

#[test]
fn a_signal_the_first_handlers_mask_blocks_waits_for_its_rt_sigreturn() {
    if bounded("a_signal_the_first_handlers_mask_blocks_waits_for_its_rt_sigreturn") {
        return;
    }
    // SIGUSR1, SIGUSR2 and SIGWINCH are released at one return. SIGUSR1's
    // sa_mask blocks SIGUSR2, so SIGWINCH's frame goes on top of SIGUSR1's,
    // and SIGUSR2 waits for SIGUSR1's rt_sigreturn, whose return delivers it
    // with the guest's context restored.
    let program = stacked_program(
        &[
            Handler::new(SIGUSR1).mask(bit(SIGUSR2)),
            Handler::new(SIGUSR2),
            Handler::new(SIGWINCH),
        ],
        &unblocked_at_syscall_return(
            &(tgkill_self(SIGUSR1) + &tgkill_self(SIGUSR2) + &tgkill_self(SIGWINCH)),
        ),
        &format!(
            "pending: .quad {:#x}\n",
            bit(SIGUSR1) | bit(SIGUSR2) | bit(SIGWINCH)
        ),
        "",
    );
    let log = stacked_at_syscall_returns_matches_native(&program);
    let context = "native";
    assert_eq!(log.signals(), [SIGWINCH, SIGUSR1, SIGUSR2], "{log:#x?}");
    assert_stacked_on(&log, 0, 1, context);
    assert_interrupted_at_mark(&log, 1, context);
    assert_interrupted_at_mark(&log, 2, context);
    assert_eq!(
        log.entry(0)[q::SIGMASK],
        bit(SIGUSR1) | bit(SIGUSR2),
        "{log:#x?}"
    );
}

#[test]
fn sa_nodefer_stacks_a_second_instance_of_the_same_signal() {
    if bounded("sa_nodefer_stacks_a_second_instance_of_the_same_signal") {
        return;
    }
    // SIGUSR1 is pending twice, for the thread (tgkill, SI_TKILL) and for
    // the process (kill, SI_USER). With SA_NODEFER its delivery does not
    // block it, so the process instance's frame goes on top of the thread
    // instance's at the same return. Without SA_NODEFER the second instance
    // waits for the first handler's rt_sigreturn.
    for nodefer in [true, false] {
        let program = stacked_program(
            &[Handler::new(SIGUSR1).flags(if nodefer { SA_NODEFER } else { 0 })],
            &unblocked_at_syscall_return(&(tgkill_self(SIGUSR1) + &kill_self(SIGUSR1))),
            &format!("pending: .quad {:#x}\n", bit(SIGUSR1)),
            "",
        );
        let log = stacked_at_syscall_returns_matches_native(&program);
        let context = format!("native, SA_NODEFER {nodefer}");
        assert_eq!(log.signals(), [SIGUSR1, SIGUSR1], "{context}: {log:#x?}");
        let codes = [log.entry(0)[q::SI_CODE], log.entry(1)[q::SI_CODE]];
        let (tkill, user) = (libc::SI_TKILL as u32 as u64, libc::SI_USER as u32 as u64);
        if nodefer {
            assert_eq!(codes, [user, tkill], "{context}: {log:#x?}");
            assert_stacked_on(&log, 0, 1, &context);
            assert_interrupted_at_mark(&log, 1, &context);
            assert_eq!(log.entry(0)[q::SIGMASK], 0, "{context}: {log:#x?}");
        } else {
            assert_eq!(codes, [tkill, user], "{context}: {log:#x?}");
            assert_interrupted_at_mark(&log, 0, &context);
            assert_interrupted_at_mark(&log, 1, &context);
        }
    }
}

#[test]
fn frames_stacked_on_an_alternate_stack_nest_on_it() {
    if bounded("frames_stacked_on_an_alternate_stack_nest_on_it") {
        return;
    }
    // Both handlers are SA_ONSTACK. SIGUSR1's frame switches to the top of
    // the alternate stack; SIGUSR2's goes below it, past its red zone, on
    // the same stack. Each frame's uc_stack holds the stack's own flags:
    // with SS_AUTODISARM, SIGUSR1's delivery disarms the stack, so SIGUSR2's
    // frame saves none, and SIGUSR1's rt_sigreturn arms it again.
    for (flags, top_saved) in [(0, None), (SS_AUTODISARM, Some([0, SS_DISABLE, 0]))] {
        let program = stacked_program(
            &[
                Handler::new(SIGUSR1).flags(SA_ONSTACK),
                Handler::new(SIGUSR2).flags(SA_ONSTACK),
            ],
            &format!(
                "{ARM_ALTSTACK}{}",
                unblocked_at_syscall_return(&(tgkill_self(SIGUSR1) + &tgkill_self(SIGUSR2)))
            ),
            &format!(
                "pending: .quad {:#x}\n{}",
                bit(SIGUSR1) | bit(SIGUSR2),
                altstack_data(flags)
            ),
            ALTSTACK,
        );
        let log = stacked_at_syscall_returns_matches_native(&program);
        let context = format!("native, ss_flags {flags:#x}");
        assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{context}: {log:#x?}");
        assert_stacked_on(&log, 0, 1, &context);
        assert_interrupted_at_mark(&log, 1, &context);
        let (top, below) = (log.entry(0), log.entry(1));
        let armed = [below[q::SS_SP], flags, 65536];
        assert_eq!(
            [below[q::SS_SP], below[q::SS_FLAGS], below[q::SS_SIZE]],
            armed,
            "{context}: {log:#x?}"
        );
        assert_eq!(
            [top[q::SS_SP], top[q::SS_FLAGS], top[q::SS_SIZE]],
            top_saved.unwrap_or(armed),
            "{context}: {log:#x?}"
        );
        // Both frames lie on the alternate stack.
        let stack = armed[0]..armed[0] + 65536;
        assert!(
            stack.contains(&below[q::ENTRY_RSP]) && stack.contains(&top[q::ENTRY_RSP]),
            "{context}: {log:#x?}"
        );
        assert_eq!(log.final_stack, armed, "{context}: {log:#x?}");
    }
}

#[test]
fn two_signals_pending_at_rt_sigreturn_stack_their_frames() {
    if bounded("two_signals_pending_at_rt_sigreturn_stack_their_frames") {
        return;
    }
    // SIGUSR1's handler, whose sa_mask blocks SIGUSR2 and SIGWINCH, sends
    // both to the thread and returns. Its rt_sigreturn restores the guest's
    // mask, which blocks neither: SIGUSR2's frame, with the guest's restored
    // context, and SIGWINCH's on top of it, at that call's return. SIGUSR1
    // itself comes at the return of the call that unblocks it.
    let program = stacked_program(
        &[
            Handler::new(SIGUSR1)
                .mask(bit(SIGUSR2) | bit(SIGWINCH))
                .actions(&(tgkill_self(SIGUSR2) + &tgkill_self(SIGWINCH))),
            Handler::new(SIGUSR2),
            Handler::new(SIGWINCH),
        ],
        &unblocked_at_syscall_return(&tgkill_self(SIGUSR1)),
        &format!("pending: .quad {:#x}\n", bit(SIGUSR1)),
        "",
    );
    let log = stacked_at_syscall_returns_matches_native(&program);
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR1, SIGWINCH, SIGUSR2], "{log:#x?}");
    assert_interrupted_at_mark(&log, 0, context);
    assert_stacked_on(&log, 1, 2, context);
    assert_interrupted_at_mark(&log, 2, context);
}

#[test]
fn a_fatal_signal_selected_after_a_frame_ends_the_group_before_its_handler_runs() {
    if bounded("a_fatal_signal_selected_after_a_frame_ends_the_group_before_its_handler_runs") {
        return;
    }
    // SIGUSR1 has a handler, which writes a byte; SIGUSR2 has none. Linux
    // builds SIGUSR1's frame, then selects SIGUSR2 at the same return, whose
    // default action ends the thread group before any handler runs: no byte.
    let program = stacked_program(
        &[Handler::new(SIGUSR1)
            .actions("mov $1,%edi\nlea byte(%rip),%rsi\nmov $1,%edx\nmov $1,%eax\nsyscall\n")],
        &unblocked_at_syscall_return(&(tgkill_self(SIGUSR1) + &tgkill_self(SIGUSR2))),
        &format!(
            "pending: .quad {:#x}\nbyte: .ascii \"x\"\n",
            bit(SIGUSR1) | bit(SIGUSR2)
        ),
        "",
    );
    let killed = (128 + SIGUSR2, vec![], vec![]);
    use std::os::unix::process::ExitStatusExt;
    let native = native_output(&program);
    assert_eq!(
        (native.status.signal(), native.stdout, native.stderr),
        (Some(SIGUSR2), vec![], vec![]),
        "native: {}",
        native.status
    );
    let mut backend = KvmBackend::new(program.memory).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&program.executable).unwrap(),
            &[program.executable.to_str().unwrap()],
            &[],
            &program.directory,
        )
        .unwrap();
    assert_eq!(
        backend.run_static_elf_captured().unwrap(),
        killed,
        "runtime"
    );
    let plan = Plan {
        deliver: true,
        ..Plan::default()
    };
    let (events, ending) = run_ending(&program, plan);
    assert_eq!(ending, Ok(killed), "tool: {events:?}");
    assert_eq!(signal_hooks(&events), [SIGUSR1, SIGUSR2], "{events:?}");
}

#[test]
fn a_frame_that_overflows_the_alternate_stack_ends_the_group_with_sigsegv() {
    if bounded("a_frame_that_overflows_the_alternate_stack_ends_the_group_with_sigsegv") {
        return;
    }
    // The guest measures one SA_ONSTACK frame on a large alternate stack
    // (SIGUSR1 sent to itself), then installs an alternate stack with room
    // for one such frame and not two, at least MINSIGSTKSZ (2048) bytes, and
    // releases SIGUSR1 and SIGUSR2 at one return. SIGUSR1's frame fits, 384
    // bytes or more above the stack's base, so that SIGUSR2's goes below it
    // past its red zone (Linux switches to the top of the stack again when
    // the red zone reaches below the base); SIGUSR2's frame does not fit, and
    // Linux forces SIGSEGV, whose default action ends the group before either
    // handler runs.
    let size = "mov log+8(%rip),%rax\nlea altstack_end(%rip),%rcx\nsub %rax,%rcx\nadd $384,%rcx\n\
                cmp $2048,%rcx\njae 2f\nmov $2048,%ecx\n2: mov %rcx,small_desc+16(%rip)\n\
                lea small_end(%rip),%rax\nsub %rcx,%rax\nmov %rax,small_desc(%rip)\n";
    let program = stacked_program(
        &[
            Handler::new(SIGUSR1).flags(SA_ONSTACK),
            Handler::new(SIGUSR2).flags(SA_ONSTACK),
        ],
        &format!(
            "lea altstack_desc(%rip),%rdi\nxor %esi,%esi\nmov $131,%eax\nsyscall\ntest %rax,%rax\njnz 99f\n\
             {}{size}lea small_desc(%rip),%rdi\nxor %esi,%esi\nmov $131,%eax\nsyscall\ntest %rax,%rax\njnz 99f\n{}",
            tgkill_self(SIGUSR1),
            unblocked_at_syscall_return(&(tgkill_self(SIGUSR1) + &tgkill_self(SIGUSR2)))
        ),
        &format!(
            "pending: .quad {:#x}\naltstack_desc: .quad altstack\n.long 0, 0\n.quad 65536\n\
             small_desc: .quad 0\n.long 0, 0\n.quad 0\n",
            bit(SIGUSR1) | bit(SIGUSR2)
        ),
        ".balign 64\naltstack: .skip 65536\naltstack_end:\n.balign 64\nsmall: .skip 65536\nsmall_end:\n",
    );
    segfaults_natively_and_on_kvm(&program);
}

/// Creates a sibling thread, on the stack ending at `sibling_stack_end`, that
/// runs `body` and exits.
fn sibling(body: &str) -> String {
    format!(
        "mov $0x50f00,%edi\nlea sibling_stack_end(%rip),%rsi\nxor %edx,%edx\nxor %r10d,%r10d\n\
         xor %r8d,%r8d\nmov $56,%eax\nsyscall\ntest %rax,%rax\njnz 6f\n{body}\
         xor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n6:\n"
    )
}

const SIBLING_STACK: &str = ".balign 64\nsibling_stack: .skip 65536\nsibling_stack_end:\n";

#[test]
fn a_user_trap_and_a_pending_signal_stack_their_frames() {
    if bounded("a_user_trap_and_a_pending_signal_stack_their_frames") {
        return;
    }
    // A sibling sends SIGUSR1 to the main thread and then sets `flag`; the
    // main thread spins without a syscall until it sees the flag, and then
    // reads page zero. With no preemption, KVM has SIGUSR1 pending at the
    // fault, as the native tracer arranges by deferring it to the fault's
    // stop. Linux selects the fault's SIGSEGV first, as a synchronous
    // signal, then SIGUSR1, whose frame goes on top: its handler runs first,
    // and the frame saves the thread's last trap, the page fault, as every
    // frame does. SIGSEGV's handler steps over the read.
    let program = stacked_program(
        &[
            Handler::new(libc::SIGSEGV).actions("addq $8,168(%rdx)\n"),
            Handler::new(SIGUSR1),
        ],
        &format!(
            "{LOAD_FPU}{}5: cmpb $0,flag(%rip)\nje 5b\nmov $0x77,%eax\nxor %ecx,%ecx\nlea 2f(%rip),%r12\n\
             2: movq 0x10,%rax\n",
            sibling(&(tgkill_self(SIGUSR1) + "movb $1,flag(%rip)\n"))
        ),
        "flag: .byte 0\n",
        SIBLING_STACK,
    );
    let log = stacked_matches_native(
        &program,
        Tracing::DeferUntil {
            deferred: SIGUSR1,
            fault: libc::SIGSEGV,
        },
        true,
        Plan::default(),
        &|_, _| false,
    )
    .native;
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR1, libc::SIGSEGV], "{log:#x?}");
    assert_stacked_on(&log, 0, 1, context);
    assert_interrupted_at_mark(&log, 1, context);
    // Read, user mode, page not present, at address 0x10, in both frames.
    let trap = [14, 4, 0x10];
    for index in [0, 1] {
        let e = log.entry(index);
        assert_eq!([e[q::TRAPNO], e[q::ERR], e[q::CR2]], trap, "{log:#x?}");
    }
}

#[test]
fn two_signals_pending_at_a_new_threads_first_instruction_stack_their_frames() {
    if bounded("two_signals_pending_at_a_new_threads_first_instruction_stack_their_frames") {
        return;
    }
    // The guest clones a thread and sends it SIGUSR1 and SIGUSR2, which the
    // Tool (holding the thread's start) and the native tracer (holding its
    // first stop) make pending before its first instruction. Both frames
    // are built there, SIGUSR2's on top; SIGUSR1's saves the thread's first
    // instruction, with rax 0 and its stack. The thread then sets `done`.
    let program = stacked_program(
        &[Handler::new(SIGUSR1), Handler::new(SIGUSR2)],
        "lea 9f(%rip),%r12\nmov $0x50f00,%edi\nlea sibling_stack_end(%rip),%rsi\nxor %edx,%edx\n\
         xor %r10d,%r10d\nxor %r8d,%r8d\nmov $56,%eax\nsyscall\n\
         9: test %rax,%rax\njz 7f\nmov %rax,child(%rip)\n\
         mov pid(%rip),%rdi\nmov child(%rip),%rsi\nmov $10,%edx\nmov $234,%eax\nsyscall\n\
         mov pid(%rip),%rdi\nmov child(%rip),%rsi\nmov $12,%edx\nmov $234,%eax\nsyscall\n\
         5: cmpb $0,done(%rip)\nje 5b\njmp 4f\n\
         7: movb $1,done(%rip)\nxor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n4:\n",
        "child: .quad 0\ndone: .byte 0\n",
        SIBLING_STACK,
    );
    let plan = Plan {
        hold_thread_start: true,
        sends: 2,
        ..Plan::default()
    };
    let log = stacked_matches_native(
        &program,
        Tracing::HoldNewThread {
            signals: [SIGUSR1, SIGUSR2],
        },
        false,
        plan,
        &|_, _| false,
    )
    .native;
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{log:#x?}");
    assert_stacked_on(&log, 0, 1, context);
    let first = log.entry(1);
    assert_eq!(first[q::RIP], first[q::R12], "{log:#x?}");
    assert_eq!(first[q::RAX], 0, "{log:#x?}");
}

#[test]
fn two_signals_pending_at_a_timer_boundary_stack_their_frames() {
    if bounded("two_signals_pending_at_a_timer_boundary_stack_their_frames") {
        return;
    }
    // A sibling waits until the main thread spins, without a syscall, and
    // sends it SIGUSR1 and then SIGUSR2. The Tool's precise timer event
    // lands in the spin and holds the thread until both are sent; the native
    // tracer holds SIGUSR1's stop, at the interrupt that brought it, until
    // SIGUSR2 is pending too. Both frames are built at that boundary,
    // SIGUSR2's on top. Where the interrupt landed differs between the runs:
    // the first frame's rip, rcx and flags, and so the flags and rcx its
    // handler starts with, which the second frame saves.
    let program = stacked_program(
        &[Handler::new(SIGUSR1), Handler::new(SIGUSR2)],
        &format!(
            "{LOAD_FPU}{}{GETPPID}movb $1,spinning(%rip)\nxor %eax,%eax\nlea 1f(%rip),%r12\n\
             mov $2000000000,%rcx\n\
             1: cmpq $2,header(%rip)\njae 2f\ndec %rcx\njnz 1b\n2:\n",
            sibling(&format!(
                "5: cmpb $0,spinning(%rip)\nje 5b\n{}{}",
                tgkill_self(SIGUSR1),
                tgkill_self(SIGUSR2)
            )),
        ),
        "spinning: .byte 0\n",
        SIBLING_STACK,
    );
    let plan = Plan {
        sends: 2,
        ..spinner_plan()
    };
    let varies =
        |_: usize, quad: usize| [q::RIP, q::RCX, q::EFLAGS, q::ENTRY_RFLAGS].contains(&quad);
    let runs = stacked_matches_native(
        &program,
        Tracing::HoldUntilPending {
            first: SIGUSR1,
            then: SIGUSR2,
        },
        false,
        plan,
        &varies,
    );
    let (log, events) = (&runs.native, &runs.events);
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{log:#x?}");
    assert_stacked_on(log, 0, 1, context);
    assert_stacked_on(&runs.tool, 0, 1, "tool");
    // On KVM both signals come at the timer event's boundary, whose
    // registers the first frame saves.
    let first = runs.tool.entry(1);
    let r = registers_of(events);
    assert_eq!(
        [
            first[q::RIP],
            first[q::RCX],
            first[q::EFLAGS],
            first[q::R12]
        ],
        [r.rip, r.rcx, r.eflags, r.r12],
        "{:#x?}",
        runs.tool
    );
    let (timer_at, timer_clock) = first_timer(events);
    let delivered: Vec<(usize, u64)> = events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            Event::Signalled { clock, .. } => Some((at, *clock)),
            _ => None,
        })
        .collect();
    assert_eq!(delivered.len(), 2, "{events:?}");
    for (at, clock) in delivered {
        assert!(at > timer_at && clock == timer_clock, "{events:?}");
    }
}

/// What the thread of
/// [`hooks_before_a_new_threads_first_instruction_set_both_segment_bases`]
/// can read through its FS and GS bases: the targets that the two signal
/// callbacks set, and the bases it inherits.
const FIRST_TARGET: u64 = 0x5151_5151_5151_5151;
const SECOND_TARGET: u64 = 0x5252_5252_5252_5252;
const OLD_FS: u64 = 0x0f0f_0f0f_0f0f_0f0f;
const OLD_GS: u64 = 0x6767_6767_6767_6767;

#[test]
fn hooks_before_a_new_threads_first_instruction_set_both_segment_bases() {
    if bounded("hooks_before_a_new_threads_first_instruction_set_both_segment_bases") {
        return;
    }
    // The guest points its own FS and GS bases at `old_fs` and `old_gs`,
    // keeps the addresses of `first` and `second` in r13 and r14, and clones
    // a thread, which inherits all four. It sends the thread SIGUSR1 and
    // SIGUSR2 while the Tool holds its start, so both frames are built before
    // its first instruction, SIGUSR2's on top. SIGUSR1's callback sets one
    // base to `first` with arch_prctl, and SIGUSR2's sets the other to
    // `second`. Once both handlers have returned, the thread reads through
    // each base and keeps what it read in the header's last quads. The
    // backend stages one base at a time for the vCPU, and SIGUSR2's callback
    // reads the vCPU's registers: SIGUSR1's base must reach the vCPU first.
    for bases in [
        [SegmentBase::Gs, SegmentBase::Fs],
        [SegmentBase::Fs, SegmentBase::Gs],
    ] {
        let program = stacked_program(
            &[Handler::new(SIGUSR1), Handler::new(SIGUSR2)],
            "lea old_fs(%rip),%rsi\nmov $0x1002,%edi\nmov $158,%eax\nsyscall\ntest %rax,%rax\njnz 99f\n\
             lea old_gs(%rip),%rsi\nmov $0x1001,%edi\nmov $158,%eax\nsyscall\ntest %rax,%rax\njnz 99f\n\
             lea first(%rip),%r13\nlea second(%rip),%r14\n\
             mov $0x50f00,%edi\nlea sibling_stack_end(%rip),%rsi\nxor %edx,%edx\n\
             xor %r10d,%r10d\nxor %r8d,%r8d\nmov $56,%eax\nsyscall\n\
             test %rax,%rax\njz 7f\nmov %rax,child(%rip)\n\
             mov pid(%rip),%rdi\nmov child(%rip),%rsi\nmov $10,%edx\nmov $234,%eax\nsyscall\n\
             mov pid(%rip),%rdi\nmov child(%rip),%rsi\nmov $12,%edx\nmov $234,%eax\nsyscall\n\
             5: cmpb $0,done(%rip)\nje 5b\njmp 4f\n\
             7: mov %fs:0,%rax\nmov %rax,header+40(%rip)\nmov %gs:0,%rax\nmov %rax,header+48(%rip)\n\
             movb $1,done(%rip)\nxor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n4:\n",
            &format!(
                "child: .quad 0\ndone: .byte 0\n.balign 8\nfirst: .quad {FIRST_TARGET:#x}\n\
                 second: .quad {SECOND_TARGET:#x}\nold_fs: .quad {OLD_FS:#x}\nold_gs: .quad {OLD_GS:#x}\n"
            ),
            SIBLING_STACK,
        );
        let plan = Plan {
            hold_thread_start: true,
            sends: 2,
            deliver: true,
            bases_at_signals: Some(bases),
            ..Plan::default()
        };
        let (events, stdout) = tool_stdout(&program, plan);
        let log = StackedLog::read(&stdout);
        let context = format!("{bases:?}: {log:#x?} {events:?}");
        assert_eq!(signal_hooks(&events), [SIGUSR1, SIGUSR2], "{context}");
        assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{context}");
        assert_stacked_on(&log, 0, 1, &context);
        let quad = |i: usize| u64::from_ne_bytes(stdout[8 * i..8 * i + 8].try_into().unwrap());
        // What the thread read through FS, then through GS.
        let expected = match bases {
            [SegmentBase::Gs, SegmentBase::Fs] => [SECOND_TARGET, FIRST_TARGET],
            _ => [FIRST_TARGET, SECOND_TARGET],
        };
        assert_eq!([quad(5), quad(6)], expected, "{context}");
    }
}

#[test]
fn a_delivery_off_an_autodisarm_stack_disarms_it_for_the_next_frame() {
    if bounded("a_delivery_off_an_autodisarm_stack_disarms_it_for_the_next_frame") {
        return;
    }
    // The alternate stack is armed with SS_AUTODISARM; SIGUSR1's handler is
    // not SA_ONSTACK, and SIGUSR2's is. Linux's signal_delivered disarms such
    // a stack after every delivery: SIGUSR1's frame goes on the ordinary
    // stack and saves the stack armed, and SIGUSR2's frame then finds no
    // stack, goes below SIGUSR1's on the ordinary stack and saves none. Each
    // rt_sigreturn restores the stack its frame saved: armed at the end.
    let program = stacked_program(
        &[
            Handler::new(SIGUSR1),
            Handler::new(SIGUSR2).flags(SA_ONSTACK),
        ],
        &format!(
            "{ARM_ALTSTACK}{}",
            unblocked_at_syscall_return(&(tgkill_self(SIGUSR1) + &tgkill_self(SIGUSR2)))
        ),
        &format!(
            "pending: .quad {:#x}\n{}",
            bit(SIGUSR1) | bit(SIGUSR2),
            altstack_data(SS_AUTODISARM)
        ),
        ALTSTACK,
    );
    let log = stacked_at_syscall_returns_matches_native(&program);
    let context = "native";
    assert_eq!(log.signals(), [SIGUSR2, SIGUSR1], "{log:#x?}");
    assert_stacked_on(&log, 0, 1, context);
    assert_interrupted_at_mark(&log, 1, context);
    let (top, below) = (log.entry(0), log.entry(1));
    let armed = [below[q::SS_SP], SS_AUTODISARM, 65536];
    assert_ne!(armed[0], 0, "{log:#x?}");
    assert_eq!(
        [below[q::SS_SP], below[q::SS_FLAGS], below[q::SS_SIZE]],
        armed,
        "{log:#x?}"
    );
    assert_eq!(
        [top[q::SS_SP], top[q::SS_FLAGS], top[q::SS_SIZE]],
        [0, SS_DISABLE, 0],
        "{log:#x?}"
    );
    // Neither frame lies on the alternate stack.
    let stack = armed[0]..armed[0] + 65536;
    assert!(
        !stack.contains(&below[q::ENTRY_RSP]) && !stack.contains(&top[q::ENTRY_RSP]),
        "{log:#x?}"
    );
    assert_eq!(log.final_stack, armed, "{log:#x?}");
}

#[test]
fn a_handler_off_an_autodisarm_stack_finds_it_disarmed() {
    if bounded("a_handler_off_an_autodisarm_stack_finds_it_disarmed") {
        return;
    }
    // The alternate stack is armed with SS_AUTODISARM, and SIGUSR1's handler,
    // which is not SA_ONSTACK, queries it. Linux disarms the stack at the
    // delivery, so the handler finds none; its frame saved the stack armed,
    // and its rt_sigreturn arms it again.
    let program = stacked_program(
        &[Handler::new(SIGUSR1).actions(QUERY_STACK)],
        &format!(
            "{ARM_ALTSTACK}{}",
            unblocked_at_syscall_return(&tgkill_self(SIGUSR1))
        ),
        &format!(
            "pending: .quad {:#x}\n{}",
            bit(SIGUSR1),
            altstack_data(SS_AUTODISARM)
        ),
        ALTSTACK,
    );
    let log = stacked_at_syscall_returns_matches_native(&program);
    assert_eq!(log.signals(), [SIGUSR1], "{log:#x?}");
    assert_interrupted_at_mark(&log, 0, "native");
    let entry = log.entry(0);
    let armed = [entry[q::SS_SP], SS_AUTODISARM, 65536];
    assert_ne!(armed[0], 0, "{log:#x?}");
    assert_eq!(
        [entry[q::SS_SP], entry[q::SS_FLAGS], entry[q::SS_SIZE]],
        armed,
        "{log:#x?}"
    );
    assert!(
        !(armed[0]..armed[0] + 65536).contains(&entry[q::ENTRY_RSP]),
        "{log:#x?}"
    );
    assert_eq!(log.queried_stack, [0, SS_DISABLE, 0], "{log:#x?}");
    assert_eq!(log.final_stack, armed, "{log:#x?}");
}

#[test]
fn exec_keeps_the_alternate_stacks_flags_for_later_frames() {
    if bounded("exec_keeps_the_alternate_stacks_flags_for_later_frames") {
        return;
    }
    // The guest arms an alternate stack with `flags` and executes itself; the
    // new image (argc 2) keeps what exec left of the stack. Linux's
    // begin_new_exec clears the stack's address and size and keeps its flags:
    // sigaltstack then reports SS_DISABLE with the stored SS_AUTODISARM, and
    // a frame's uc_stack holds no stack and the flags themselves. With
    // SS_AUTODISARM, the delivery resets the stack to SS_DISABLE.
    for flags in [0, SS_AUTODISARM] {
        let program = stacked_program(
            &[Handler::new(SIGUSR1)],
            &format!(
                "cmpq $1,argc(%rip)\njne 3f\n{ARM_ALTSTACK}\
                 lea self_exe(%rip),%rdi\nlea exec_argv(%rip),%rsi\nlea exec_envp(%rip),%rdx\n\
                 mov $59,%eax\nsyscall\njmp 99f\n3:\n{QUERY_STACK}{}",
                unblocked_at_syscall_return(&tgkill_self(SIGUSR1))
            ),
            &format!(
                "pending: .quad {:#x}\n{}self_exe: .asciz \"/proc/self/exe\"\nexec_arg: .asciz \"x\"\n\
                 .balign 8\nexec_argv: .quad self_exe, exec_arg, 0\nexec_envp: .quad 0\n",
                bit(SIGUSR1),
                altstack_data(flags)
            ),
            ALTSTACK,
        );
        let log = stacked_at_syscall_returns_matches_native(&program);
        let context = format!("native, ss_flags {flags:#x}: {log:#x?}");
        assert_eq!(log.signals(), [SIGUSR1], "{context}");
        let entry = log.entry(0);
        assert_eq!(
            [entry[q::SS_SP], entry[q::SS_FLAGS], entry[q::SS_SIZE]],
            [0, flags, 0],
            "{context}"
        );
        assert_eq!(log.queried_stack, [0, SS_DISABLE | flags, 0], "{context}");
        assert_eq!(log.final_stack, [0, SS_DISABLE, 0], "{context}");
    }
}

#[test]
fn sigaltstack_takes_back_what_it_reports_for_the_stack_exec_leaves() {
    if bounded("sigaltstack_takes_back_what_it_reports_for_the_stack_exec_leaves") {
        return;
    }
    // The guest arms an alternate stack with `flags` and executes itself;
    // exec keeps the flags and clears the address and size. The new image
    // then, comparing each result with Linux's do_sigaltstack:
    // - asks for that stack unchanged, which returns 0 before any size check;
    // - queries it (SS_DISABLE, with any SS_AUTODISARM) and gives back what
    //   the query reported, which a disabling mode keeps as given;
    // - asks for a stack with an invalid mode (EINVAL) and queries again;
    // - takes SIGUSR1, whose frame saves the stack as held.
    let einval = (-i64::from(libc::EINVAL)) as u64;
    for flags in [0, SS_AUTODISARM] {
        let program = stacked_program(
            &[Handler::new(SIGUSR1)],
            &format!(
                "cmpq $1,argc(%rip)\njne 3f\n{ARM_ALTSTACK}\
                 lea self_exe(%rip),%rdi\nlea exec_argv(%rip),%rsi\nlea exec_envp(%rip),%rdx\n\
                 mov $59,%eax\nsyscall\njmp 99f\n3:\n{}{QUERY_STACK}{}{}\
                 xor %edi,%edi\nlea second_query(%rip),%rsi\nmov $131,%eax\nsyscall\n\
                 mov second_query+8(%rip),%eax\n{}{}",
                set_altstack("unchanged", 0),
                set_altstack("header+40", 1),
                set_altstack("bad_mode", 2),
                keep_result(3),
                unblocked_at_syscall_return(&tgkill_self(SIGUSR1))
            ),
            &format!(
                "pending: .quad {:#x}\n{}{}{}self_exe: .asciz \"/proc/self/exe\"\n\
                 exec_arg: .asciz \"x\"\n.balign 8\nexec_argv: .quad self_exe, exec_arg, 0\n\
                 exec_envp: .quad 0\nsecond_query: .zero 24\n",
                bit(SIGUSR1),
                altstack_data(flags),
                stack_t("unchanged", "0", flags, 0),
                stack_t("bad_mode", "0", 4, 0),
            ),
            ALTSTACK,
        );
        let log = stacked_at_syscall_returns_matches_native(&program);
        let context = format!("native, ss_flags {flags:#x}: {log:#x?}");
        let reported = [0, SS_DISABLE | flags, 0];
        assert_eq!(log.signals(), [SIGUSR1], "{context}");
        assert_eq!(log.queried_stack, reported, "{context}");
        assert_eq!(log.results, [0, 0, einval, SS_DISABLE | flags], "{context}");
        let entry = log.entry(0);
        assert_eq!(
            [entry[q::SS_SP], entry[q::SS_FLAGS], entry[q::SS_SIZE]],
            reported,
            "{context}"
        );
        assert_eq!(log.final_stack, reported, "{context}");
    }
}

#[test]
fn sigaltstack_takes_ss_onstack_as_an_input_mode() {
    if bounded("sigaltstack_takes_ss_onstack_as_an_input_mode") {
        return;
    }
    // The guest asks for an alternate stack with SS_ONSTACK as its mode,
    // which Linux's do_sigaltstack takes as it takes 0 and stores as given,
    // then for one with an invalid mode (EINVAL) and for one below
    // MINSIGSTKSZ (ENOMEM), which leave the stack as it is. SIGUSR1's handler
    // is SA_ONSTACK: its frame goes on the stack and saves the flags as
    // given; on the stack, the handler queries it (SS_ONSTACK, which the
    // report shows while the caller runs on it) and asks for another (EPERM).
    let errno = |value: i32| (-i64::from(value)) as u64;
    let ss_onstack = libc::SS_ONSTACK as u64;
    let program = stacked_program(
        &[Handler::new(SIGUSR1)
            .flags(SA_ONSTACK)
            .actions(&format!("{QUERY_STACK}{}", set_altstack("plain", 3)))],
        &format!(
            "{}{}{}{}",
            set_altstack("onstack_mode", 0),
            set_altstack("bad_mode", 1),
            set_altstack("too_small", 2),
            unblocked_at_syscall_return(&tgkill_self(SIGUSR1))
        ),
        &format!(
            "pending: .quad {:#x}\n{}{}{}{}",
            bit(SIGUSR1),
            stack_t("onstack_mode", "altstack", ss_onstack, 65536),
            stack_t("bad_mode", "altstack", 4, 65536),
            stack_t("too_small", "altstack", 0, 1024),
            stack_t("plain", "altstack", 0, 65536),
        ),
        ALTSTACK,
    );
    let log = stacked_at_syscall_returns_matches_native(&program);
    assert_eq!(log.signals(), [SIGUSR1], "{log:#x?}");
    assert_interrupted_at_mark(&log, 0, "native");
    let entry = log.entry(0);
    let sp = entry[q::SS_SP];
    assert_ne!(sp, 0, "{log:#x?}");
    assert_eq!(
        [entry[q::SS_SP], entry[q::SS_FLAGS], entry[q::SS_SIZE]],
        [sp, ss_onstack, 65536],
        "{log:#x?}"
    );
    assert!((sp..sp + 65536).contains(&entry[q::ENTRY_RSP]), "{log:#x?}");
    assert_eq!(log.queried_stack, [sp, ss_onstack, 65536], "{log:#x?}");
    assert_eq!(
        log.results,
        [
            0,
            errno(libc::EINVAL),
            errno(libc::ENOMEM),
            errno(libc::EPERM)
        ],
        "{log:#x?}"
    );
    assert_eq!(log.final_stack, [sp, 0, 65536], "{log:#x?}");
}

#[test]
fn the_native_tracer_classifies_sigstop_by_provenance() {
    // What PTRACE_GETSIGINFO shows decides, not the order of the stops: the
    // synthetic SIGSTOP of a new thread is suppressed wherever it comes, once,
    // and a sent SIGSTOP is forwarded, also as a thread's first SIGSTOP (as
    // when SIGCONT cleared the synthetic one before the thread dequeued it).
    let (pid, child) = (100, 101);
    use SigstopAction::*;
    assert_eq!(
        classify_sigstop(child, pid, libc::SI_USER, 0, &[]),
        Ok(SuppressSynthetic)
    );
    for (code, synthetic) in [
        (libc::SI_TKILL, &[][..]),
        (libc::SI_TKILL, &[child][..]),
        (libc::SI_USER, &[][..]),
        (libc::SI_QUEUE, &[child][..]),
    ] {
        assert_eq!(
            classify_sigstop(child, pid, code, pid, synthetic),
            Ok(Forward),
            "si_code {code}"
        );
    }
    assert_eq!(
        classify_sigstop(pid, pid, libc::SI_TKILL, pid, &[]),
        Ok(Forward)
    );
    // Refused: a second synthetic stop, one for the main thread, a send with
    // no sender, and any other provenance.
    for (tid, code, sender, synthetic) in [
        (child, libc::SI_USER, 0, &[child][..]),
        (pid, libc::SI_USER, 0, &[][..]),
        (child, libc::SI_TKILL, 0, &[][..]),
        (child, libc::SI_KERNEL, 0, &[][..]),
        (child, libc::SI_KERNEL, pid, &[][..]),
    ] {
        assert!(
            classify_sigstop(tid, pid, code, sender, synthetic).is_err(),
            "tid {tid} si_code {code} from {sender}"
        );
    }
}

#[test]
fn the_native_tracer_forwards_a_sent_sigstop_and_suppresses_only_the_synthetic_one() {
    if bounded("the_native_tracer_forwards_a_sent_sigstop_and_suppresses_only_the_synthetic_one") {
        return;
    }
    // Native only: KVM supports no stopped state, so a stop signal a guest
    // sends ends a KVM run. The guest clones a thread, which sends itself
    // SIGSTOP with tgkill and then lets the main thread finish. The tracer
    // suppresses the thread's synthetic SIGSTOP (SI_USER from pid 0), forwards
    // the sent one (SI_TKILL from the guest), which stops the whole process,
    // and resumes each thread from the group-stop that follows, which
    // delivers nothing.
    let program = stacked_program(
        &[],
        "mov $0x50f00,%edi\nlea sibling_stack_end(%rip),%rsi\nxor %edx,%edx\n\
         xor %r10d,%r10d\nxor %r8d,%r8d\nmov $56,%eax\nsyscall\n\
         test %rax,%rax\njz 7f\n\
         5: cmpb $0,done(%rip)\nje 5b\njmp 4f\n\
         7: mov $186,%eax\nsyscall\nmov pid(%rip),%rdi\nmov %rax,%rsi\nmov $19,%edx\n\
         mov $234,%eax\nsyscall\nmovb $1,done(%rip)\nxor %edi,%edi\nmov $60,%eax\nsyscall\nud2\n\
         4:\n",
        "done: .byte 0\n",
        SIBLING_STACK,
    );
    let native = traced_native(&program, Tracing::Deliver);
    assert_eq!(native.delivered, [libc::SIGSTOP]);
    assert_eq!(native.synthetic_stops, 1);
}
