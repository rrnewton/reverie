/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A Tool decides the signals it names (`Tool::injection_stop_signals`) when
//! one stops a call the backend injected for it before the call ran
//! (`Tool::decide_injection_stop`), and the backend does what the Tool
//! answers. Refused, the injection fails with the refusal's errno,
//! `Guest::take_unsupported_refusal` reports the refusal, and the signal is
//! gone: the guest's handler never runs. Held, the injection reports that the
//! call did not run (`ERESTARTSYS`), and the signal is delivered when the
//! guest resumes. A signal the Tool does not name is held without asking. The
//! Tool is told the injected call with its arguments and the signal, and
//! reads the thread's facts while it stays stopped
//! (`crate::injection_stop_facts::Reader`).
//!
//! The tracer sends the signal to the guest thread (`tgkill`), where it stays
//! pending, and the Tool then injects a call, which the pending signal stops
//! before the call runs.

use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::DescriptorFile;
use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::FactError;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectionStop;
use reverie::InjectionStopFacts;
use reverie::InjectionStopOutcome;
use reverie::Pid;
use reverie::SignalSets;
use reverie::Subscription;
use reverie::Tool;
use reverie::UnsupportedOperation;
use reverie::UnsupportedRefusal;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use serde::Deserialize;
use serde::Serialize;

/// The guest's probe, `getppid(signal, descriptor)`, whose arguments Linux
/// ignores: the Tool sends `signal` to the guest thread, then injects a call
/// that carries `descriptor`, an open pipe of the guest's.
const PROBE: Sysno = Sysno::getppid;

/// The second argument of the call the Tool injects (`getpid`, which ignores
/// its arguments), so that the Tool can tell its call was the one stopped.
const MARKER: usize = 0x5157_0be7;

/// The refusal this Tool returns. The backend records whatever refusal the
/// Tool returns; this one borrows the kind the hook exists for.
const REFUSAL: UnsupportedRefusal = UnsupportedRefusal::new(
    UnsupportedOperation::JobControlStop,
    Errno::EIO,
    "refused by the test Tool",
);

/// What the Tool decides.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Policy {
    /// The signals it decides (`Tool::injection_stop_signals`).
    decides: u64,
    /// Whether it refuses them; otherwise it holds them.
    refuses: bool,
}

/// What `Tool::decide_injection_stop` was told and read.
#[derive(Debug, Serialize, Deserialize)]
struct Asked {
    tid: i32,
    /// The injected call's number and first two arguments.
    call: Option<(usize, usize, usize)>,
    signal: i32,
    /// Whether the hook's thread may be under a seccomp filter, where the
    /// reader does not ask for the siginfo.
    tracer_may_be_filtered: bool,
    /// The siginfo's `si_code`.
    si_code: Result<i32, FactError>,
    signal_sets: Result<SignalSets, FactError>,
    /// The descriptor the injected call carries.
    descriptor: Result<DescriptorFile, FactError>,
    /// A descriptor the guest does not have open.
    closed: Result<DescriptorFile, FactError>,
    terminal: Result<u32, FactError>,
}

#[derive(Debug, Serialize, Deserialize)]
enum Event {
    Asked(Asked),
    /// The injection's result, as a negative errno for a failure, and the
    /// errno of the refusal it left, if any.
    Injected {
        tid: i32,
        result: i64,
        refusal: Option<i32>,
    },
}

#[derive(Debug, Default)]
struct Seen(Mutex<Vec<Event>>);

#[reverie::global_tool]
impl GlobalTool for Seen {
    type Request = Event;
    type Response = ();
    type Config = Policy;

    async fn receive_rpc(&self, _from: Pid, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

#[derive(Debug, Default)]
struct Decider;

#[reverie::tool]
impl Tool for Decider {
    type GlobalState = Seen;
    type ThreadState = ();

    fn subscriptions(_: &Policy) -> Subscription {
        let mut events = Subscription::none();
        events.syscalls([PROBE]);
        events
    }

    fn injection_stop_signals(policy: &Policy) -> u64 {
        policy.decides
    }

    async fn decide_injection_stop<R: GlobalRPC<Seen>>(
        &self,
        rpc: &R,
        stop: InjectionStop,
        facts: &dyn InjectionStopFacts,
    ) -> InjectionStopOutcome {
        let descriptor = stop.call.map_or(-1, |(_, args)| args.arg0 as i32);
        let asked = Asked {
            tid: stop.tid.as_raw(),
            call: stop
                .call
                .map(|(nr, args)| (nr.id() as usize, args.arg0, args.arg1)),
            signal: stop.signal,
            tracer_may_be_filtered: crate::task::thread_may_be_seccomp_filtered(),
            si_code: facts
                .signal_info()
                .map(|info| i32::from_ne_bytes([info[8], info[9], info[10], info[11]])),
            signal_sets: facts.signal_sets(),
            descriptor: facts.descriptor_file(descriptor),
            closed: facts.descriptor_file(1_000_000),
            terminal: facts.controlling_terminal(),
        };
        rpc.send_rpc(Event::Asked(asked)).await;
        if rpc.config().refuses {
            InjectionStopOutcome::Refuse(REFUSAL)
        } else {
            InjectionStopOutcome::Hold
        }
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != PROBE {
            guest.tail_inject(syscall).await;
        }
        let (_, args) = syscall.into_parts();
        let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
        // SAFETY: tgkill takes three integers and touches no memory.
        let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, args.arg0 as libc::c_int) };
        assert_eq!(sent, 0, "tgkill from the tracer");
        let call = Syscall::from_raw(
            Sysno::getpid,
            SyscallArgs::new(args.arg1, MARKER, 0, 0, 0, 0),
        );
        let result = match guest.inject(call).await {
            Ok(value) => value,
            Err(errno) => -i64::from(errno.into_raw()),
        };
        let refusal = guest.take_unsupported_refusal();
        assert!(
            refusal.is_none_or(|refusal| refusal == REFUSAL),
            "{refusal:?}"
        );
        guest
            .send_rpc(Event::Injected {
                tid,
                result,
                refusal: refusal.map(|refusal| refusal.errno.into_raw()),
            })
            .await;
        // The injection's errno as a positive number, never a restart code,
        // so that the probe returns it as it is.
        Ok(-result)
    }
}

/// Signal `signal`'s bit in a signal set.
fn bit(signal: i32) -> u64 {
    1 << (signal - 1)
}

/// The guest's handler runs.
static HANDLED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_signal(_: libc::c_int) {
    HANDLED.fetch_add(1, Ordering::SeqCst);
}

/// Runs the guest under `policy`: it handles SIGUSR1 and SIGUSR2, probes with
/// `signal` and a pipe of its own, and asserts that the probe returned
/// `returns` and that its handler then ran `runs` times. Returns what the
/// Tool saw.
fn run(policy: Policy, signal: i32, returns: i64, runs: usize) -> Vec<Event> {
    let (output, seen) = crate::testing::test_fn_with_config::<Decider, _>(
        move || {
            for handled in [libc::SIGUSR1, libc::SIGUSR2] {
                // SAFETY: the handler only increments an atomic.
                let installed = unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction =
                        on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    libc::sigaction(handled, &action, std::ptr::null_mut())
                };
                assert_eq!(installed, 0);
            }
            let (reader, _writer) = std::io::pipe().unwrap();
            // SAFETY: the probe takes integers and touches no memory.
            let ret = unsafe {
                libc::syscall(
                    libc::SYS_getppid,
                    signal as libc::c_long,
                    reader.as_raw_fd() as libc::c_long,
                )
            };
            assert_eq!(
                (ret, HANDLED.load(Ordering::SeqCst)),
                (returns, runs),
                "(probe result, handler runs)"
            );
        },
        policy,
        true,
    )
    .unwrap();
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "guest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    seen.0.into_inner().unwrap()
}

/// The facts the Tool read at a stop of `signal`: the injected call with the
/// arguments it carried, the signal and the thread, the siginfo of the
/// tracer's `tgkill` (unless the hook's thread may be under a seccomp filter,
/// where the reader does not ask for it), the guest's handler, its pipe, a
/// descriptor it does not have open, and the controlling terminal it shares
/// with this test's process.
fn assert_facts(asked: &Asked, signal: i32, tid: i32) {
    assert_eq!(asked.tid, tid, "{asked:?}");
    assert_eq!(asked.signal, signal, "{asked:?}");
    let (nr, _, marker) = asked.call.expect("the backend knows the injected call");
    assert_eq!(
        (nr, marker),
        (Sysno::getpid.id() as usize, MARKER),
        "{asked:?}"
    );
    if asked.tracer_may_be_filtered {
        assert_eq!(asked.si_code, Err(FactError::NotProvided), "{asked:?}");
    } else {
        assert_eq!(asked.si_code, Ok(libc::SI_TKILL), "{asked:?}");
    }
    let sets = asked.signal_sets.expect("the guest's signal sets");
    assert_ne!(sets.caught & bit(signal), 0, "{asked:?}");
    let pipe = asked.descriptor.expect("the guest's pipe");
    assert_eq!(pipe.mode & libc::S_IFMT, libc::S_IFIFO, "{asked:?}");
    assert_eq!(
        asked.closed,
        Err(FactError::Errno(Errno::EBADF)),
        "{asked:?}"
    );
    let own = std::fs::File::open("/proc/thread-self").unwrap();
    let terminal =
        crate::injection_stop_facts::controlling_terminal_in(std::os::fd::AsFd::as_fd(&own))
            .unwrap();
    assert_eq!(asked.terminal, Ok(terminal), "{asked:?}");
}

/// A refused stop fails the injection with the refusal's errno, which the
/// probe returns, and the backend records the refusal; the signal is gone,
/// so the guest's handler never runs.
#[test]
fn a_refused_stop_fails_the_injection_with_the_refusal() {
    let policy = Policy {
        decides: bit(libc::SIGUSR2),
        refuses: true,
    };
    let eio = libc::EIO as i64;
    let events = run(policy, libc::SIGUSR2, eio, 0);
    let [
        Event::Asked(asked),
        Event::Injected {
            tid,
            result,
            refusal,
        },
    ] = &events[..]
    else {
        panic!("one stop asked, then the injection: {events:?}");
    };
    assert_facts(asked, libc::SIGUSR2, *tid);
    assert_eq!((*result, *refusal), (-eio, Some(libc::EIO)), "{events:?}");
}

/// A held stop leaves the injection's report that the call did not run
/// (`ERESTARTSYS`), and no refusal; the signal is delivered when the guest
/// resumes, so its handler runs once.
#[test]
fn a_held_stop_is_delivered_when_the_guest_resumes() {
    let policy = Policy {
        decides: bit(libc::SIGUSR2),
        refuses: false,
    };
    const ERESTARTSYS: i64 = 512;
    let events = run(policy, libc::SIGUSR2, ERESTARTSYS, 1);
    let [
        Event::Asked(asked),
        Event::Injected {
            tid,
            result,
            refusal,
        },
    ] = &events[..]
    else {
        panic!("one stop asked, then the injection: {events:?}");
    };
    assert_facts(asked, libc::SIGUSR2, *tid);
    assert_eq!((*result, *refusal), (-ERESTARTSYS, None), "{events:?}");
}

/// A signal the Tool does not decide is held without asking, even by a Tool
/// that refuses every signal it decides.
#[test]
fn a_signal_the_tool_does_not_decide_is_held_unasked() {
    let policy = Policy {
        decides: bit(libc::SIGUSR2),
        refuses: true,
    };
    const ERESTARTSYS: i64 = 512;
    let events = run(policy, libc::SIGUSR1, ERESTARTSYS, 1);
    let [
        Event::Injected {
            result, refusal, ..
        },
    ] = &events[..]
    else {
        panic!("no stop asked, only the injection: {events:?}");
    };
    assert_eq!((*result, *refusal), (-ERESTARTSYS, None), "{events:?}");
}
