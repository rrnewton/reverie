/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Imprecise timer events end to end: `Guest::set_timer` through the
//! production CPL3 SYSCALL transport to `Tool::handle_timer_event`, under the
//! Reverie contract. These tests require KVM and the raw PMU event;
//! unavailable hardware is not a pass.
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
    if std::env::var_os("REVERIE_KVM_TIMER_TEST_CHILD").is_some() {
        return false;
    }
    let output = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "60s"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("REVERIE_KVM_TIMER_TEST_CHILD", "1")
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

/// A static guest assembled from `body`, as in read_clock.rs.
struct Program {
    directory: PathBuf,
    executable: PathBuf,
}

impl Program {
    fn new(body: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "reverie-kvm-timer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let source = directory.join("guest.S");
        let executable = directory.join("guest");
        std::fs::write(&source, format!(".global _start\n.text\n_start:\n{body}\n")).unwrap();
        let output = std::process::Command::new("timeout")
            .args([
                "--kill-after=2s",
                "10s",
                "/usr/bin/gcc",
                "-nostdlib",
                "-static",
                "-Wl,--build-id=none",
            ])
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
    /// Ask for a precise event and for 0 RCBs after the requests.
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
}

#[derive(Default)]
struct Log(Mutex<Vec<Event>>);

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = Plan;
    type Request = Event;
    type Response = ();

    async fn receive_rpc(&self, _from: Pid, event: Event) {
        self.0.lock().unwrap().push(event);
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

async fn request<G: Guest<TimerTool>>(guest: &mut G) {
    let plan = guest.config().clone();
    for ticks in plan.requests {
        let clock = guest.read_clock().unwrap();
        guest.set_timer(TimerSchedule::Rcbs(ticks)).unwrap();
        guest.send_rpc(Event::Requested { clock, ticks }).await;
    }
    if plan.refusals {
        let refused = match guest.set_timer_precise(TimerSchedule::Rcbs(1_000)) {
            Err(reverie::Error::Io(error)) => format!("{:?}: {error}", error.kind()),
            other => format!("{other:?}"),
        };
        guest.send_rpc(Event::Refused(refused)).await;
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
        subscription
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        if guest.config().arm == Arm::ThreadStart {
            request(guest).await;
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
            _ => {}
        }
        guest.tail_inject(syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Event::Signal(signal as i32)).await;
        Ok((signal != Signal::SIGUSR1 && signal != Signal::SIGALRM).then_some(signal))
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
            };
            guest.send_rpc(Event::Registers(registers)).await;
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
fn run_ending(program: &Program, plan: Plan) -> (Vec<Event>, Ending) {
    let mut backend = KvmBackend::new(MEMORY).unwrap();
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
    let events = completion.global_state.0.into_inner().unwrap();
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
fn precise_and_zero_requests_are_refused() {
    if bounded("precise_and_zero_requests_are_refused") {
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
    assert_eq!(refusals.len(), 2, "{events:?}");
    assert!(
        refusals[0].starts_with("Unsupported: ")
            && refusals[0].contains("does not deliver precise timer events"),
        "{events:?}"
    );
    assert_eq!(refusals[1], "Err(Errno(EINVAL))", "{events:?}");
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
