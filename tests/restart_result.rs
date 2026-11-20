/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A Tool that answers a guest syscall with a restart code (`-ERESTART*`)
//! while a signal is pending gets Linux's restart semantics: `-EINTR` or a
//! restart once the signal is delivered, never the raw code.
//!
//! The outcomes are the same on every architecture, though Linux decides
//! them at different points: x86-64 after the tracer's last stop
//! (`handle_signal` and `arch_do_signal_or_restart`,
//! arch/x86/kernel/signal.c:263-281 and :344-361 at v6.12.9), arm64 before
//! its signal-delivery stops (`do_signal`, arch/arm64/kernel/signal.c:1395-1464).
//! A signal the Tool drops is one the tracer suppresses: no handler runs, and
//! the call restarts, `-ERESTART_RESTARTBLOCK` through `restart_syscall`.
//! The same holds for a number the syscall table cannot name, which a Tool
//! answers under `UnknownSyscallPolicy::TrapToTool`, and for a code returned
//! with no signal at all, which restarts as a call whose signal is gone by its
//! delivery. `ENOIOCTLCMD` (515), internal to the kernel, and a restart code
//! for a number Linux reads as no syscall are refused by name.

#[cfg(all(not(sanitized), test))]
use std::sync::Arc;
#[cfg(all(not(sanitized), test))]
use std::sync::Mutex;
#[cfg(all(not(sanitized), test))]
use std::sync::atomic::AtomicI32;
#[cfg(all(not(sanitized), test))]
use std::sync::atomic::Ordering as AtomicOrdering;

#[cfg(all(not(sanitized), test))]
use reverie::Error;
#[cfg(all(not(sanitized), test))]
use reverie::GlobalRPC;
#[cfg(all(not(sanitized), test))]
use reverie::GlobalTool;
#[cfg(all(not(sanitized), test))]
use reverie::Guest;
#[cfg(all(not(sanitized), test))]
use reverie::Pid;
#[cfg(all(not(sanitized), test))]
use reverie::Subscription;
#[cfg(all(not(sanitized), test))]
use reverie::Tool;
#[cfg(all(not(sanitized), test))]
use reverie::UnknownSyscallPolicy;
#[cfg(all(not(sanitized), test))]
use reverie::UnreportedSignal;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::Errno;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::Getpid;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::RawSyscall;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::Syscall;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::SyscallArgs;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::SyscallInfo;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::Sysno;
#[cfg(all(not(sanitized), test))]
use reverie::syscalls::Tgkill;
#[cfg(all(not(sanitized), test))]
use serde::Deserialize;
#[cfg(all(not(sanitized), test))]
use serde::Serialize;

/// What the Tool returns for the probe, or for `restart_syscall` or a
/// `ppoll`, once the guest restarted it.
#[cfg(all(not(sanitized), test))]
const RESTARTED: i64 = 4242;

/// The probe: `getppid`, whose arguments Linux ignores, carries the restart
/// code to return (a positive errno), the signal to send first, and how the
/// Tool sends it (`SENT_BY_*`, `HELD_*`), or `CONFIGURE_PPOLL`.
#[cfg(all(not(sanitized), test))]
const PROBE: Sysno = Sysno::getppid;

/// The probe as a number with no `Sysno` in either architecture's table (both
/// end at 469), which `UnknownNumberRestart` answers with the probe's
/// arguments under `UnknownSyscallPolicy::TrapToTool`.
#[cfg(all(not(sanitized), test))]
const UNKNOWN_PROBE: u64 = 1000;

/// Unknown probes whose low 32 bits are all ones, which Linux reads as -1,
/// no syscall, wherever it holds the number as an `int`.
#[cfg(all(not(sanitized), test))]
const NO_SYSCALL_PROBES: [u64; 3] = [0xffff_ffff, u64::MAX, 0x1_ffff_ffff];

/// Unknown probes with the high word set or a negative low word other than
/// -1: Linux reads the low word, which names a call, and restarts it with the
/// whole register, so the same number reaches the Tool again.
#[cfg(all(not(sanitized), test))]
const WIDE_PROBES: [u64; 3] = [0x1_0000_03e8, 0xffff_ffff_0000_03e8, 0x8000_0000];

/// The tracer sends the signal, which stays pending in the kernel.
#[cfg(all(not(sanitized), test))]
const SENT_BY_TRACER: i64 = 0;

/// An injected `tgkill` sends the signal.
#[cfg(all(not(sanitized), test))]
const SENT_BY_INJECTION: i64 = 1;

/// The tracer sends the signal, then the Tool injects a syscall, which the
/// pending signal stops: the signal is then held for the guest's resume.
#[cfg(all(not(sanitized), test))]
const HELD_OVER_INJECTION: i64 = 2;

/// As `HELD_OVER_INJECTION`, and then the tracer sends the signal again to
/// the whole process, where it stays pending: the guest takes both.
#[cfg(all(not(sanitized), test))]
const HELD_AND_PENDING_FOR_THE_PROCESS: i64 = 3;

/// As `HELD_OVER_INJECTION`, and the Tool drops the held signal
/// (`Tool::handle_signal_event` or `Tool::filter_unreported_signal`).
#[cfg(all(not(sanitized), test))]
const HELD_AND_DROPPED: i64 = 4;

/// For `ppoll`: as `HELD_OVER_INJECTION`, and then the tracer sends SIGUSR1,
/// which the guest handles, so that it is pending too.
#[cfg(all(not(sanitized), test))]
const HELD_WITH_SIGUSR1_PENDING: i64 = 5;

/// For `ppoll` or `rt_sigsuspend`: the Tool returns the interrupted
/// injection's own result, with the signal where the injection left it.
#[cfg(all(not(sanitized), test))]
const PLAIN: i64 = 6;

/// For `ppoll`: as `HELD_OVER_INJECTION`, and then an injected
/// `rt_sigprocmask` blocks the held signal (the probe's fifth argument points
/// at the set).
#[cfg(all(not(sanitized), test))]
const HELD_THEN_BLOCKED: i64 = 7;

/// For `ppoll`: the Tool runs the guest's call with a first argument of its
/// own (no descriptors), and returns the interrupted call's result as for
/// `PLAIN`. A restart must then run the guest's call with the guest's first
/// argument.
#[cfg(all(not(sanitized), test))]
const PLAIN_WITH_ANOTHER_FIRST_ARGUMENT: i64 = 8;

/// For the probe: as `HELD_OVER_INJECTION`, and then the tracer sends every
/// realtime signal from 34 to 64, which the guest blocks, so that each stays
/// pending for the thread.
#[cfg(all(not(sanitized), test))]
const HELD_WITH_EVERY_REALTIME_SIGNAL_PENDING: i64 = 9;

/// For `ppoll` of no descriptors: as `PLAIN`, with the Tool's call given a
/// first argument the call never reads (the timeout's address), so the
/// guest sees no difference unless a restart runs with it.
#[cfg(all(not(sanitized), test))]
const PLAIN_WITH_AN_UNREAD_FIRST_ARGUMENT: i64 = 10;

/// As `PLAIN_WITH_AN_UNREAD_FIRST_ARGUMENT`, with the signal sent to the
/// process, and the Tool's answer held until another thread of the guest has
/// taken it (`kill` and the process's shared pending set).
#[cfg(all(not(sanitized), test))]
const TAKEN_BY_ANOTHER_THREAD: i64 = 11;

/// The Tool sends no signal: it returns the code alone.
#[cfg(all(not(sanitized), test))]
const NO_SIGNAL: i64 = 12;

/// The Tool injects a `ppoll` of no descriptors with the zero timeout and the
/// temporary mask the probe's fourth and fifth arguments point at, which
/// returns at once, and then returns the code alone. A Tool that declares
/// serialized execution runs that mask-swapping injection between syscall
/// stops (`syscall_stops_admitted`), so the guest resumes from the
/// injection's exit stop rather than a signal-delivery stop.
#[cfg(all(not(sanitized), test))]
const INJECTED_PPOLL_THEN_NO_SIGNAL: i64 = 13;

/// The Tool runs the injected `ppoll` of `INJECTED_PPOLL_THEN_NO_SIGNAL`, then
/// sends the signal from the tracer, which stays pending for the thread, and
/// returns the code: the backend's carrier carries the resume into
/// `do_signal` with every other signal blocked, and the pending signal is
/// taken at a signal-delivery stop after the carrier's, before Linux converts
/// the code.
#[cfg(all(not(sanitized), test))]
const INJECTED_PPOLL_THEN_SIGNAL_PENDING: i64 = 14;

/// As `INJECTED_PPOLL_THEN_NO_SIGNAL`, and the Tool then makes the tracer's
/// own procfs status unreadable before it returns the code
/// (`StatusUnreadable`): no descriptor can be opened (`RLIMIT_NOFILE` 0), so
/// the tracer cannot rule out a seccomp filter on its own thread.
#[cfg(all(not(sanitized), test))]
const STATUS_UNREADABLE: i64 = 20;

/// Why this Tool refuses a restart code under a pending temporary mask's
/// restore: the two cases whose Linux outcome it cannot decide.
#[cfg(all(not(sanitized), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Undecidable {
    /// `-ERESTARTSYS` with a handler: the outcome hangs on the handler's
    /// `SA_RESTART`, which procfs does not show.
    ErestartsysWithHandler,
    /// A delivered signal the mask in effect blocks: the kernel would queue
    /// it again rather than deliver it.
    DeliveredSignalBlocked,
}

/// This Tool's refusal of a restart code (`Undecidable`), which ends the run.
#[cfg(all(not(sanitized), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("restart code refused by the test Tool ({reason:?}): {syscall} was answered with {code}")]
struct RestartRefused {
    syscall: Sysno,
    code: Errno,
    reason: Undecidable,
}

/// The probe's arguments are the code, the signal, the mode (fourth
/// argument) and a signal set (fifth) for the thread's next `ppoll` or
/// `rt_sigsuspend`, which the Tool runs itself while the tracer sends the
/// signal, and answers with the code (or, for `PLAIN`, the call's result).
#[cfg(all(not(sanitized), test))]
const CONFIGURE_PPOLL: i64 = 100;

/// The Tool's global state, which holds nothing. Its configuration says
/// whether the Tool declares serialized execution
/// (`Tool::enforces_serialized_execution`), under which the backend runs a
/// mask-swapping injection between syscall stops on aarch64.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default)]
struct Global;

#[cfg(all(not(sanitized), test))]
#[reverie::global_tool]
impl GlobalTool for Global {
    type Request = ();
    type Response = ();
    type Config = bool;

    async fn receive_rpc(&self, _from: Pid, _request: ()) {}
}

#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct RestartResult {
    /// A signal the Tool drops once, or 0.
    drop_next: Arc<AtomicI32>,
}

#[cfg(all(not(sanitized), test))]
impl RestartResult {
    fn drops(&self, signal: i32) -> bool {
        self.drop_next
            .compare_exchange(signal, 0, AtomicOrdering::SeqCst, AtomicOrdering::SeqCst)
            .is_ok()
    }
}

#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Serialize, Deserialize)]
struct ThreadState {
    interrupted: bool,
    /// The first argument of the guest's `ppoll` or `rt_sigsuspend` the Tool
    /// interrupted: a call with it is that call restarted, any other a new
    /// one (such as a handler's), which is run.
    interrupted_arg0: Option<usize>,
    /// The code, signal, mode and set for the next `ppoll` or
    /// `rt_sigsuspend` (`CONFIGURE_PPOLL`).
    ppoll: Option<(i32, i32, i64, usize)>,
}

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for RestartResult {
    type GlobalState = Global;
    type ThreadState = ThreadState;

    /// As configured: only the tests of the syscall-stop transport declare
    /// it. The one with two guest threads breaks the contract on purpose.
    fn enforces_serialized_execution(serialized: &bool) -> bool {
        *serialized
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let interrupted = guest.thread_state().interrupted;
        match syscall.number() {
            // The kernel's own restart block describes no call of this Tool's.
            Sysno::restart_syscall => {
                if interrupted {
                    return Ok(RESTARTED);
                }
            }
            Sysno::ppoll | Sysno::rt_sigsuspend
                if interrupted
                    && guest.thread_state().interrupted_arg0.is_none_or(|arg0| {
                        arg0 == syscall.into_parts().1.arg0
                    }) =>
            {
                return Ok(RESTARTED);
            }
            Sysno::ppoll | Sysno::rt_sigsuspend if interrupted => {
                return guest.tail_inject(syscall).await;
            }
            Sysno::ppoll | Sysno::rt_sigsuspend => {
                if let Some((code, sig, mode, set)) = guest.thread_state_mut().ppoll.take() {
                    guest.thread_state_mut().interrupted = true;
                    guest.thread_state_mut().interrupted_arg0 = Some(syscall.into_parts().1.arg0);
                    return self
                        .interrupt_ppoll(guest, syscall, code, sig, mode, set)
                        .await;
                }
                return guest.tail_inject(syscall).await;
            }
            nr if nr != PROBE => return guest.tail_inject(syscall).await,
            _ => {}
        }
        let (_, args) = syscall.into_parts();
        if args.arg2 as i64 == CONFIGURE_PPOLL {
            guest.thread_state_mut().ppoll = Some((
                args.arg0 as i32,
                args.arg1 as i32,
                args.arg3 as i64,
                args.arg4,
            ));
            return Ok(0);
        }
        answer_probe(self, guest, args).await
    }

    /// The outcome Linux's `do_signal` takes for a restart code under a
    /// pending temporary mask's restore, decided here because the backend
    /// and Reverie's default decide none (arch/arm64/kernel/signal.c:1395-1464
    /// at v6.12.9): `get_signal` takes the delivered signal, then, while none
    /// has a handler, the pending signals the mask in effect lets through.
    /// With a handler `ERESTARTNOHAND` and `ERESTART_RESTARTBLOCK` give
    /// `-EINTR` and `ERESTARTNOINTR` restarts; with none every code restarts.
    /// `ERESTARTSYS` with a handler (it hangs on `SA_RESTART`, which procfs
    /// does not show) and a delivered signal the mask in effect blocks are
    /// refused, with this Tool's own reason (`RestartRefused`).
    async fn resolve_restart_under_pending_mask_restore<R: reverie::GlobalRPC<Global>>(
        &self,
        _rpc: &R,
        facts: reverie::PendingMaskRestore,
    ) -> Result<reverie::MaskRestoreOutcome, Error> {
        let bit = |signal: i32| 1u64 << (signal - 1);
        let refuse = |reason| {
            Err(Error::Tool(anyhow::Error::new(RestartRefused {
                syscall: facts.syscall,
                code: facts.code,
                reason,
            })))
        };
        let runs_a_handler = |signal: i32| {
            signal != libc::SIGKILL && signal != libc::SIGSTOP && facts.caught & bit(signal) != 0
        };
        if facts
            .delivered
            .is_some_and(|signal| facts.mask_in_effect & bit(signal) != 0)
        {
            return refuse(Undecidable::DeliveredSignalBlocked);
        }
        let handler_runs = facts.delivered.is_some_and(runs_a_handler)
            || (1..=64)
                .filter(|&signal| facts.pending & !facts.mask_in_effect & bit(signal) != 0)
                .any(runs_a_handler);
        match facts.code {
            Errno::ERESTARTNOHAND | Errno::ERESTART_RESTARTBLOCK if handler_runs => {
                Ok(reverie::MaskRestoreOutcome::Interrupted { handler_runs })
            }
            Errno::ERESTARTSYS if handler_runs => refuse(Undecidable::ErestartsysWithHandler),
            _ => Ok(reverie::MaskRestoreOutcome::Restart { handler_runs }),
        }
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        _guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        Ok((!self.drops(signal as i32)).then_some(signal))
    }

    async fn filter_unreported_signal<R: GlobalRPC<Self::GlobalState>>(
        &self,
        _rpc: &R,
        signal: UnreportedSignal,
    ) -> bool {
        !self.drops(signal.number())
    }
}

#[cfg(all(not(sanitized), test))]
impl RestartResult {
    /// Runs the guest's `ppoll` or `rt_sigsuspend` while the tracer sends
    /// `sig` 200 ms in, which the guest blocks and the call's temporary mask
    /// lets through: the call returns `-ERESTARTNOHAND` with that mask's
    /// restore pending. For `PLAIN` the Tool returns that result. Otherwise
    /// the next injection takes the still pending signal, which is held for
    /// the guest's resume (dropped for `HELD_AND_DROPPED`), and the Tool
    /// answers with `code`.
    async fn interrupt_ppoll<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
        code: i32,
        sig: i32,
        mode: i64,
        set: usize,
    ) -> Result<i64, Error> {
        let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
        let to_the_process = mode == TAKEN_BY_ANOTHER_THREAD;
        let sender = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if to_the_process {
                // SAFETY: kill takes two integers and touches no memory.
                unsafe { libc::kill(pid, sig) as libc::c_long }
            } else {
                // SAFETY: tgkill takes three integers and touches no memory.
                unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, sig) }
            }
        });
        let (nr, args) = syscall.into_parts();
        let syscall = match mode {
            PLAIN_WITH_ANOTHER_FIRST_ARGUMENT => Syscall::from_raw(
                nr,
                SyscallArgs::new(0, 0, args.arg2, args.arg3, args.arg4, args.arg5),
            ),
            PLAIN_WITH_AN_UNREAD_FIRST_ARGUMENT | TAKEN_BY_ANOTHER_THREAD => Syscall::from_raw(
                nr,
                SyscallArgs::new(args.arg2, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5),
            ),
            _ => syscall,
        };
        let injected = guest.inject(syscall).await;
        assert_eq!(sender.join().unwrap(), 0, "the signal from the tracer");
        if to_the_process {
            wait_until_taken_from_the_process(pid, sig).await;
        }
        if matches!(
            mode,
            PLAIN
                | PLAIN_WITH_ANOTHER_FIRST_ARGUMENT
                | PLAIN_WITH_AN_UNREAD_FIRST_ARGUMENT
                | TAKEN_BY_ANOTHER_THREAD
        ) {
            return injected.map_err(Into::into);
        }
        assert!(injected.is_err(), "the signal interrupts the injected call");
        assert!(
            guest.inject(Getpid::new()).await.is_err(),
            "the pending signal interrupts the next injection, and is held"
        );
        if mode == HELD_AND_DROPPED {
            self.drop_next.store(sig, AtomicOrdering::SeqCst);
        }
        if mode == HELD_WITH_SIGUSR1_PENDING {
            // SAFETY: tgkill takes three integers and touches no memory.
            let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1) };
            assert_eq!(sent, 0, "tgkill from the tracer");
        }
        if mode == HELD_THEN_BLOCKED {
            let block = Syscall::from_raw(
                Sysno::rt_sigprocmask,
                SyscallArgs::new(libc::SIG_BLOCK as usize, set, 0, 8, 0, 0),
            );
            assert_eq!(guest.inject(block).await, Ok(0), "block the held signal");
        }
        Err(Errno::new(code).into())
    }
}

/// Answers the probe's arguments `args` (`PROBE`, or `UNKNOWN_PROBE`) for
/// either Tool of this file, which drops a signal as `tool` does: `RESTARTED`
/// once the thread was interrupted, and otherwise the code, after sending the
/// signal as the arguments say.
#[cfg(all(not(sanitized), test))]
async fn answer_probe<T, G>(
    tool: &RestartResult,
    guest: &mut G,
    args: SyscallArgs,
) -> Result<i64, Error>
where
    T: Tool<ThreadState = ThreadState>,
    G: Guest<T>,
{
    let (code, sig, sent_by) = (args.arg0 as i32, args.arg1 as i32, args.arg2 as i64);
    if guest.thread_state().interrupted {
        return Ok(RESTARTED);
    }
    guest.thread_state_mut().interrupted = true;
    if matches!(
        sent_by,
        INJECTED_PPOLL_THEN_NO_SIGNAL | INJECTED_PPOLL_THEN_SIGNAL_PENDING
    ) {
        let ppoll = Syscall::from_raw(
            Sysno::ppoll,
            SyscallArgs::new(0, 0, args.arg3, args.arg4, 8, 0),
        );
        assert_eq!(
            guest.inject(ppoll).await,
            Ok(0),
            "the injected ppoll returns at once"
        );
    }
    if matches!(sent_by, NO_SIGNAL | INJECTED_PPOLL_THEN_NO_SIGNAL) {
        return Err(Errno::new(code).into());
    }
    let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
    if sent_by == SENT_BY_INJECTION {
        guest
            .inject(Tgkill::new().with_tgid(pid).with_tid(tid).with_sig(sig))
            .await?;
    } else {
        // SAFETY: tgkill takes three integers and touches no memory.
        let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, sig) };
        assert_eq!(sent, 0, "tgkill from the tracer");
    }
    if matches!(
        sent_by,
        HELD_OVER_INJECTION
            | HELD_AND_PENDING_FOR_THE_PROCESS
            | HELD_AND_DROPPED
            | HELD_WITH_EVERY_REALTIME_SIGNAL_PENDING
    ) {
        assert!(
            guest.inject(Getpid::new()).await.is_err(),
            "the pending signal interrupts the injection, and is held"
        );
    }
    if sent_by == HELD_AND_PENDING_FOR_THE_PROCESS {
        // SAFETY: kill takes two integers and touches no memory.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "kill from the tracer");
    }
    if sent_by == HELD_AND_DROPPED {
        tool.drop_next.store(sig, AtomicOrdering::SeqCst);
    }
    if sent_by == HELD_WITH_EVERY_REALTIME_SIGNAL_PENDING {
        for realtime in 34..=64 {
            // SAFETY: tgkill takes three integers and touches no memory.
            let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, realtime) };
            assert_eq!(sent, 0, "tgkill {realtime} from the tracer");
        }
    }
    Err(Errno::new(code).into())
}

/// `RestartResult` for a number the syscall table cannot name: the Tool
/// answers `UNKNOWN_PROBE`, `NO_SYSCALL_PROBES` and `WIDE_PROBES` under
/// `UnknownSyscallPolicy::TrapToTool` (`Tool::handle_unknown_syscall`) as
/// `RestartResult` answers the probe, with the code taken from the fourth
/// argument: arm64 replaces the first with `-ENOSYS` for a number it reads as
/// -1 (`el0_svc_common`, arch/arm64/kernel/syscall.c:114-115 at v7.1.3). A
/// restart runs the number again, which reaches the Tool again, and
/// `-ERESTART_RESTARTBLOCK` runs `restart_syscall`.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct UnknownNumberRestart(RestartResult);

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for UnknownNumberRestart {
    type GlobalState = Global;
    type ThreadState = ThreadState;

    fn subscriptions(_serialized: &bool) -> Subscription {
        let mut subscriptions = Subscription::all_syscalls();
        subscriptions.set_unknown_syscall_policy(UnknownSyscallPolicy::TrapToTool);
        subscriptions
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() == Sysno::restart_syscall && guest.thread_state().interrupted {
            return Ok(RESTARTED);
        }
        guest.tail_inject(syscall).await
    }

    async fn handle_unknown_syscall<T: Guest<Self>>(
        &self,
        guest: &mut T,
        raw: RawSyscall,
    ) -> Result<i64, Error> {
        let probe = raw.nr == UNKNOWN_PROBE
            || NO_SYSCALL_PROBES.contains(&raw.nr)
            || WIDE_PROBES.contains(&raw.nr);
        if !probe {
            return Err(Errno::ENOSYS.into());
        }
        let args = raw.args;
        let args = SyscallArgs::new(
            args.arg3, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5,
        );
        answer_probe(&self.0, guest, args).await
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        _guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        Ok((!self.0.drops(signal as i32)).then_some(signal))
    }

    async fn filter_unreported_signal<R: GlobalRPC<Self::GlobalState>>(
        &self,
        _rpc: &R,
        signal: UnreportedSignal,
    ) -> bool {
        !self.0.drops(signal.number())
    }
}

/// The global state of `StatusUnreadable` and `StatusUnreadableAnswering`.
/// Its configuration says whether the Tool declares serialized execution,
/// and the errno `StatusUnreadableAnswering`'s unresolved-restart hook answers.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default)]
struct HookGlobal;

#[cfg(all(not(sanitized), test))]
#[reverie::global_tool]
impl GlobalTool for HookGlobal {
    type Request = ();
    type Response = ();
    type Config = (bool, i32);

    async fn receive_rpc(&self, _from: Pid, _request: ()) {}
}

/// The tracer's open-file limit (soft, hard) before a Tool lowered it, which
/// that Tool's hook puts back.
#[cfg(all(not(sanitized), test))]
type SavedLimit = Arc<Mutex<Option<(libc::rlim_t, libc::rlim_t)>>>;

/// Lowers the tracer's soft open-file limit to 0, so that no descriptor can be
/// opened, and returns the limit it replaced.
#[cfg(all(not(sanitized), test))]
fn close_the_tracers_files() -> (libc::rlim_t, libc::rlim_t) {
    let mut old = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes the one rlimit it is passed.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut old) }, 0);
    let closed = libc::rlimit {
        rlim_cur: 0,
        rlim_max: old.rlim_max,
    };
    // SAFETY: setrlimit reads the one rlimit it is passed.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &closed) }, 0);
    (old.rlim_cur, old.rlim_max)
}

/// Puts back the tracer's open-file limit `limit`.
#[cfg(all(not(sanitized), test))]
fn reopen_the_tracers_files((rlim_cur, rlim_max): (libc::rlim_t, libc::rlim_t)) {
    let limit = libc::rlimit { rlim_cur, rlim_max };
    // SAFETY: setrlimit reads the one rlimit it is passed.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
}

/// Answers the probe's arguments `args` for either Tool below: for
/// `STATUS_UNREADABLE`, an injected `ppoll` that returns at once, then the
/// tracer's procfs status made unreadable (`close_the_tracers_files`, the
/// limit kept in `saved`), then the code alone; otherwise as `RestartResult`.
#[cfg(all(not(sanitized), test))]
async fn answer_with_the_status_unreadable<T, G>(
    saved: &SavedLimit,
    guest: &mut G,
    syscall: Syscall,
) -> Result<i64, Error>
where
    T: Tool<ThreadState = ThreadState>,
    G: Guest<T>,
{
    if syscall.number() != PROBE {
        return guest.tail_inject(syscall).await;
    }
    let (_, args) = syscall.into_parts();
    if args.arg2 as i64 != STATUS_UNREADABLE {
        return answer_probe(&RestartResult::default(), guest, args).await;
    }
    if guest.thread_state().interrupted {
        return Ok(RESTARTED);
    }
    guest.thread_state_mut().interrupted = true;
    let ppoll = Syscall::from_raw(
        Sysno::ppoll,
        SyscallArgs::new(0, 0, args.arg3, args.arg4, 8, 0),
    );
    assert_eq!(
        guest.inject(ppoll).await,
        Ok(0),
        "the injected ppoll returns at once"
    );
    *saved.lock().unwrap() = Some(close_the_tracers_files());
    Err(Errno::new(args.arg0 as i32).into())
}

/// Answers the probe with `answer_with_the_status_unreadable`, and leaves
/// `Tool::handle_unresolved_restart` at its default, which answers the code
/// itself. The tracer's open-file limit stays closed; the test puts it back.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct StatusUnreadable(SavedLimit);

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for StatusUnreadable {
    type GlobalState = HookGlobal;
    type ThreadState = ThreadState;

    fn enforces_serialized_execution(config: &(bool, i32)) -> bool {
        config.0
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        answer_with_the_status_unreadable(&self.0, guest, syscall).await
    }
}

/// `StatusUnreadable`, whose `Tool::handle_unresolved_restart` puts the
/// tracer's open-file limit back and answers the configured errno.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct StatusUnreadableAnswering(SavedLimit);

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for StatusUnreadableAnswering {
    type GlobalState = HookGlobal;
    type ThreadState = ThreadState;

    fn enforces_serialized_execution(config: &(bool, i32)) -> bool {
        config.0
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        answer_with_the_status_unreadable(&self.0, guest, syscall).await
    }

    async fn handle_unresolved_restart<R: GlobalRPC<HookGlobal>>(
        &self,
        rpc: &R,
        _tid: Pid,
        _syscall: Sysno,
        _code: Errno,
        _reason: reverie::UnresolvedRestart,
    ) -> Result<i64, Error> {
        if let Some(limit) = self.0.lock().unwrap().take() {
            reopen_the_tracers_files(limit);
        }
        Err(Errno::new(rpc.config().1).into())
    }
}

/// Injects the guest's own `rt_sigsuspend`, `ppoll` or `pselect6` unchanged,
/// which the backend runs in place, from its seccomp stop to its exit stop
/// (`Guest::inject` of the pending call), and returns that call's result, a
/// restart code included; every other call is tail-injected.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct InjectsTheGuestsOwnCall;

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for InjectsTheGuestsOwnCall {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::rt_sigsuspend | Sysno::ppoll | Sysno::pselect6 => {
                Ok(guest.inject(syscall).await?)
            }
            _ => guest.tail_inject(syscall).await,
        }
    }
}

/// Injects the guest's own `rt_sigsuspend` unchanged, as
/// `InjectsTheGuestsOwnCall` does, and answers the kernel's `-ERESTARTNOHAND`
/// for it with another restart code, `-ERESTARTSYS`.
#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[derive(Debug, Default, Clone)]
struct AnswersAnotherRestartCode;

#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[reverie::tool]
impl Tool for AnswersAnotherRestartCode {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != Sysno::rt_sigsuspend {
            return guest.tail_inject(syscall).await;
        }
        match guest.inject(syscall).await {
            Err(Errno::ERESTARTNOHAND) => Err(Errno::ERESTARTSYS.into()),
            result => Ok(result?),
        }
    }
}

/// On the probe, runs the guest's own call in place, which completes
/// normally, and answers with `-ERESTARTNOHAND` of its own; with the probe's
/// first argument 1 it first sends SIGUSR1 to the guest's process from the
/// tracer and waits until another thread of the guest has taken it. A
/// restarted probe is answered with `RESTARTED`. Every other call is
/// tail-injected.
#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[derive(Debug, Default, Clone)]
struct SynthesizesARestartCode;

#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[reverie::tool]
impl Tool for SynthesizesARestartCode {
    type GlobalState = ();
    type ThreadState = ThreadState;

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != PROBE {
            return guest.tail_inject(syscall).await;
        }
        if guest.thread_state().interrupted {
            return Ok(RESTARTED);
        }
        guest.thread_state_mut().interrupted = true;
        let sends = syscall.into_parts().1.arg0 == 1;
        assert!(
            guest.inject(syscall).await? > 0,
            "the guest's own getppid completes normally"
        );
        if sends {
            let pid = guest.pid().as_raw();
            // SAFETY: kill takes two integers and touches no memory.
            assert_eq!(
                unsafe { libc::kill(pid, libc::SIGUSR1) },
                0,
                "kill from the tracer"
            );
            wait_until_taken_from_the_process(pid, libc::SIGUSR1).await;
        }
        Err(Errno::ERESTARTNOHAND.into())
    }
}

/// How `EditsTheInterruptedWait` edits the guest's registers before it
/// returns the interrupted call's result.
#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
enum RegisterEdit {
    /// The syscall number Linux restarts from (`orig_rax`) set to -1.
    #[default]
    NoSyscallNumber,
    /// `rcx` and `r11`, which the `syscall` instruction clobbers, set to 0.
    ClobberedRegisters,
    /// `orig_rax` set to another syscall's number (`getpid`).
    AnotherSyscallNumber,
    /// No edit at the exit stop; `orig_rax` set to -1 at the signal-delivery
    /// stop of the signal that interrupted the call, which Linux makes inside
    /// `get_signal` before it converts the code
    /// (arch/x86/kernel/signal.c:337 at v7.1.3).
    NoSyscallNumberAtTheDeliveryStop,
    /// No edit at the exit stop; at SIGUSR1's delivery stop `rip` moved by 8
    /// and the signal suppressed, so `get_signal` goes on to SIGUSR2; at
    /// SIGUSR2's, still before Linux converts the code, `rip` put back and
    /// `orig_rax` set to -1.
    IpEditedThenNoSyscallNumberAtTheNextStop,
}

/// The global state of `EditsTheInterruptedWait`, whose configuration is the
/// edit.
#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[derive(Debug, Default)]
struct EditGlobal;

#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[reverie::global_tool]
impl GlobalTool for EditGlobal {
    type Request = ();
    type Response = ();
    type Config = RegisterEdit;

    async fn receive_rpc(&self, _from: Pid, _request: ()) {}
}

/// Injects the guest's own `rt_sigsuspend` unchanged, as
/// `InjectsTheGuestsOwnCall` does, then edits the guest's registers as
/// configured (`Guest::set_regs`) and returns the call's result.
#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[derive(Debug, Default, Clone)]
struct EditsTheInterruptedWait;

#[cfg(all(not(sanitized), test, target_arch = "x86_64"))]
#[reverie::tool]
impl Tool for EditsTheInterruptedWait {
    type GlobalState = EditGlobal;
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != Sysno::rt_sigsuspend {
            return guest.tail_inject(syscall).await;
        }
        let result = guest.inject(syscall).await;
        let mut regs = guest.regs().await;
        match *guest.config() {
            RegisterEdit::NoSyscallNumber => regs.orig_rax = u64::MAX,
            RegisterEdit::ClobberedRegisters => {
                regs.rcx = 0;
                regs.r11 = 0;
            }
            RegisterEdit::AnotherSyscallNumber => regs.orig_rax = libc::SYS_getpid as u64,
            RegisterEdit::NoSyscallNumberAtTheDeliveryStop
            | RegisterEdit::IpEditedThenNoSyscallNumberAtTheNextStop => return Ok(result?),
        }
        guest.set_regs(regs).await?;
        Ok(result?)
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        match (*guest.config(), signal) {
            (RegisterEdit::NoSyscallNumberAtTheDeliveryStop, reverie::Signal::SIGUSR1) => {
                let mut regs = guest.regs().await;
                regs.orig_rax = u64::MAX;
                guest.set_regs(regs).await.map_err(|_| Errno::EIO)?;
            }
            (RegisterEdit::IpEditedThenNoSyscallNumberAtTheNextStop, reverie::Signal::SIGUSR1) => {
                let mut regs = guest.regs().await;
                regs.rip += 8;
                guest.set_regs(regs).await.map_err(|_| Errno::EIO)?;
                return Ok(None);
            }
            (RegisterEdit::IpEditedThenNoSyscallNumberAtTheNextStop, reverie::Signal::SIGUSR2) => {
                let mut regs = guest.regs().await;
                regs.rip -= 8;
                regs.orig_rax = u64::MAX;
                guest.set_regs(regs).await.map_err(|_| Errno::EIO)?;
            }
            _ => {}
        }
        Ok(Some(signal))
    }
}

/// Runs the guest's own `pselect6` with its second argument, the read set,
/// replaced by an address the call never reads (the guest watches no
/// descriptors), so the backend runs a call of the Tool's own rather than the
/// guest's in place, and skips the guest's call; returns the call's result,
/// a restart code included. The first argument is the guest's, so a restart
/// by the kernel runs the guest's call. Every other call is tail-injected.
/// Declares serialized execution as configured, under which the backend runs
/// the mask-swapping call between syscall stops on aarch64.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct RunsTheGuestsPselect6WithAnUnreadSet;

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for RunsTheGuestsPselect6WithAnUnreadSet {
    type GlobalState = Global;
    type ThreadState = ();

    fn enforces_serialized_execution(serialized: &bool) -> bool {
        *serialized
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != Sysno::pselect6 {
            return guest.tail_inject(syscall).await;
        }
        let (nr, args) = syscall.into_parts();
        let unread_set = args.arg4;
        let call = Syscall::from_raw(
            nr,
            SyscallArgs::new(
                args.arg0, unread_set, args.arg2, args.arg3, args.arg4, args.arg5,
            ),
        );
        Ok(guest.inject(call).await?)
    }
}

/// The flags `RunsTheCallAfterASiblingsFilter` and a guest's sibling thread
/// hand over a seccomp filter's installation with at `rt_sigsuspend`, and the
/// stopped thread's `Seccomp_filters` the Tool reads before and after it.
#[cfg(all(not(sanitized), test))]
static SIBLING_GO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(all(not(sanitized), test))]
static SIBLING_DONE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(all(not(sanitized), test))]
static SIBLING_FILTERS: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Thread `tid`'s `Seccomp_filters`, from its procfs status.
#[cfg(all(not(sanitized), test))]
fn seccomp_filters_of(tid: i32) -> u64 {
    std::fs::read_to_string(format!("/proc/{tid}/status"))
        .expect("read the thread's status")
        .lines()
        .find_map(|line| line.strip_prefix("Seccomp_filters:"))
        .map(|count| count.trim().parse().expect("Seccomp_filters is a number"))
        .expect("the status has Seccomp_filters")
}

/// On the probe, whose first three arguments are the addresses of two flags
/// and of two counts in the guest, or on `rt_sigsuspend`, with
/// `SIBLING_GO`, `SIBLING_DONE` and `SIBLING_FILTERS` (the guest is a fork of
/// this process, so they are at the same addresses there): reads the stopped
/// thread's `Seccomp_filters`, sets the first flag, waits until a sibling
/// thread of the guest has set the second (it installs, with
/// `SECCOMP_FILTER_FLAG_TSYNC`, a seccomp filter answering the call), reads
/// the count again and writes both to the guest, and panics unless the count
/// rose by one during the stop; then injects the guest's own call unchanged,
/// which the backend runs in place, and returns its result. Every other call
/// is tail-injected.
#[cfg(all(not(sanitized), test))]
async fn run_after_a_siblings_filter<T: Tool, G: Guest<T>>(
    guest: &mut G,
    syscall: Syscall,
) -> Result<i64, Error> {
    use reverie::syscalls::MemoryAccess;
    let (go, done, filters) = match syscall.number() {
        PROBE => {
            let (_, args) = syscall.into_parts();
            (args.arg0, args.arg1, args.arg2)
        }
        Sysno::rt_sigsuspend => (
            SIBLING_GO.as_ptr() as usize,
            SIBLING_DONE.as_ptr() as usize,
            SIBLING_FILTERS.as_ptr() as usize,
        ),
        _ => return guest.tail_inject(syscall).await,
    };
    let tid = guest.tid().as_raw();
    let before = seccomp_filters_of(tid);
    let go = reverie::syscalls::AddrMut::<u32>::from_raw(go).expect("the go flag");
    let done = reverie::syscalls::Addr::<u32>::from_raw(done).expect("the done flag");
    guest.memory().write_value(go, &1)?;
    for _ in 0..500 {
        if guest.memory().read_value(done)? == 1 {
            let after = seccomp_filters_of(tid);
            for (offset, count) in [(0, before), (8, after)] {
                let at = reverie::syscalls::AddrMut::<u64>::from_raw(filters + offset)
                    .expect("the filter counts");
                guest.memory().write_value(at, &count)?;
            }
            assert_eq!(
                after,
                before + 1,
                "the stopped thread's Seccomp_filters did not rise by one during the stop"
            );
            return Ok(guest.inject(syscall).await?);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the sibling did not install its filter in 5 s");
}

/// Runs the guest's call after a sibling's filter (`run_after_a_siblings_filter`).
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct RunsTheCallAfterASiblingsFilter;

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for RunsTheCallAfterASiblingsFilter {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        run_after_a_siblings_filter(guest, syscall).await
    }
}

/// `RunsTheCallAfterASiblingsFilter`, whose `Tool::handle_unresolved_restart`
/// answers every unresolved code with `EINTR`, an ordinary error.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct RunsTheCallAfterASiblingsFilterAnsweringEintr;

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for RunsTheCallAfterASiblingsFilterAnsweringEintr {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        run_after_a_siblings_filter(guest, syscall).await
    }

    async fn handle_unresolved_restart<R: GlobalRPC<()>>(
        &self,
        _rpc: &R,
        _tid: Pid,
        _syscall: Sysno,
        _code: Errno,
        _reason: reverie::UnresolvedRestart,
    ) -> Result<i64, Error> {
        Err(Errno::EINTR.into())
    }
}

/// Completes the guest's own probe normally in place, as
/// `SynthesizesARestartCode` does, and answers `-ERESTART_RESTARTBLOCK` of its
/// own; its unresolved-restart hook (`Tool::handle_unresolved_restart`, which
/// the backend awaits while it routes that code under a tracer filter, after
/// it checked the code) first lets a sibling thread of the guest install a seccomp
/// filter on every thread, through the flags `SIBLING_GO` and `SIBLING_DONE`
/// in the guest's memory (`/proc/<tid>/mem`, which the tracer may write),
/// waits for it, and then answers `EINTR`, an ordinary error.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct HookAwaitsASiblingsFilter;

/// The guest's `u32` at `addr` (`/proc/<tid>/mem`).
#[cfg(all(not(sanitized), test))]
fn read_guest_u32(tid: Pid, addr: usize) -> u32 {
    use std::os::unix::fs::FileExt;
    let mem = std::fs::File::open(format!("/proc/{tid}/mem")).expect("open the guest's memory");
    let mut bytes = [0u8; 4];
    mem.read_exact_at(&mut bytes, addr as u64)
        .expect("read the guest's memory");
    u32::from_ne_bytes(bytes)
}

/// Writes `value` to the guest's `u32` at `addr` (`/proc/<tid>/mem`).
#[cfg(all(not(sanitized), test))]
fn write_guest_u32(tid: Pid, addr: usize, value: u32) {
    use std::os::unix::fs::FileExt;
    let mem = std::fs::OpenOptions::new()
        .write(true)
        .open(format!("/proc/{tid}/mem"))
        .expect("open the guest's memory");
    mem.write_all_at(&value.to_ne_bytes(), addr as u64)
        .expect("write the guest's memory");
}

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for HookAwaitsASiblingsFilter {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != PROBE {
            return guest.tail_inject(syscall).await;
        }
        assert!(
            guest.inject(syscall).await? > 0,
            "the guest's own getppid completes normally"
        );
        Err(Errno::ERESTART_RESTARTBLOCK.into())
    }

    async fn handle_unresolved_restart<R: GlobalRPC<()>>(
        &self,
        _rpc: &R,
        tid: Pid,
        _syscall: Sysno,
        _code: Errno,
        _reason: reverie::UnresolvedRestart,
    ) -> Result<i64, Error> {
        write_guest_u32(tid, SIBLING_GO.as_ptr() as usize, 1);
        for _ in 0..500 {
            if read_guest_u32(tid, SIBLING_DONE.as_ptr() as usize) == 1 {
                return Err(Errno::EINTR.into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the sibling did not install its filter in 5 s");
    }
}

/// The path the guest of the inherited-filter row opens to read its own
/// status (`ReadsTheTrueStatusAtItsOpen`); the guest is a fork of this
/// process, so the bytes are at the same address there.
#[cfg(all(not(sanitized), test))]
static TRUE_STATUS_PATH: &[u8] = b"/proc/self/status\0";

/// What `ReadsTheTrueStatusAtItsOpen` read of the guest thread's true status:
/// its `Seccomp` mode and its `Seccomp_filters`, `u64::MAX` until read.
#[cfg(all(not(sanitized), test))]
static TRUE_STATUS: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(u64::MAX),
    std::sync::atomic::AtomicU64::new(u64::MAX),
];

/// At the guest's `openat` of `TRUE_STATUS_PATH`, a tracer stop, reads the
/// guest thread's true `/proc/<tid>/status` from the tracer's side and
/// writes its `Seccomp` mode and `Seccomp_filters` to `TRUE_STATUS` in the
/// guest; every call is tail-injected.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct ReadsTheTrueStatusAtItsOpen;

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for ReadsTheTrueStatusAtItsOpen {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        use reverie::syscalls::MemoryAccess;
        if syscall.number() == Sysno::openat
            && syscall.into_parts().1.arg1 == TRUE_STATUS_PATH.as_ptr() as usize
        {
            let status = std::fs::read_to_string(format!("/proc/{}/status", guest.tid()))
                .expect("read the guest's true status");
            let field = |name: &str| -> u64 {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix(name))
                    .expect("the status has the line")
                    .trim()
                    .parse()
                    .expect("a number")
            };
            for (slot, value) in TRUE_STATUS
                .iter()
                .zip([field("Seccomp:"), field("Seccomp_filters:")])
            {
                let at = reverie::syscalls::AddrMut::<u64>::from_raw(slot.as_ptr() as usize)
                    .expect("the true status's slot");
                guest.memory().write_value(at, &value)?;
            }
        }
        guest.tail_inject(syscall).await
    }
}

/// `RestartResult`, which answers the probe as configured, declaring
/// serialized execution as configured, whose report of SIGUSR1
/// (`Tool::handle_signal_event`) first lets a sibling thread of the guest
/// install a seccomp filter on every thread (`SIBLING_GO`, `SIBLING_DONE`)
/// and waits for it.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct SignalStopAwaitsASiblingsFilter(RestartResult);

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for SignalStopAwaitsASiblingsFilter {
    type GlobalState = Global;
    type ThreadState = ThreadState;

    fn enforces_serialized_execution(serialized: &bool) -> bool {
        *serialized
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != PROBE {
            return guest.tail_inject(syscall).await;
        }
        let (_, args) = syscall.into_parts();
        answer_probe(&self.0, guest, args).await
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        if signal == reverie::Signal::SIGUSR1 {
            await_a_siblings_filter(guest).await?;
        }
        Ok(Some(signal))
    }
}

/// Lets a sibling thread of the guest install a seccomp filter on every
/// thread (`SIBLING_GO`), and waits until it has (`SIBLING_DONE`).
#[cfg(all(not(sanitized), test))]
async fn await_a_siblings_filter<T: Tool, G: Guest<T>>(guest: &mut G) -> Result<(), Errno> {
    use reverie::syscalls::MemoryAccess;
    let go = reverie::syscalls::AddrMut::<u32>::from_raw(SIBLING_GO.as_ptr() as usize)
        .expect("the go flag");
    let done = reverie::syscalls::Addr::<u32>::from_raw(SIBLING_DONE.as_ptr() as usize)
        .expect("the done flag");
    guest.memory().write_value(go, &1).map_err(|_| Errno::EIO)?;
    for _ in 0..500 {
        if guest.memory().read_value(done).map_err(|_| Errno::EIO)? == 1 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the sibling did not install its filter in 5 s");
}

/// `RestartResult`, which answers the probe as configured, declaring
/// serialized execution as configured, whose report of SIGUSR1 first lets a
/// sibling thread of the guest exit the whole guest (`SIBLING_GO`) and waits
/// until the stopped thread is a zombie or gone.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct SignalStopAwaitsTheGuestsExit(RestartResult);

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for SignalStopAwaitsTheGuestsExit {
    type GlobalState = Global;
    type ThreadState = ThreadState;

    fn enforces_serialized_execution(serialized: &bool) -> bool {
        *serialized
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != PROBE {
            return guest.tail_inject(syscall).await;
        }
        let (_, args) = syscall.into_parts();
        answer_probe(&self.0, guest, args).await
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        use reverie::syscalls::MemoryAccess;
        if signal != reverie::Signal::SIGUSR1 {
            return Ok(Some(signal));
        }
        let go = reverie::syscalls::AddrMut::<u32>::from_raw(SIBLING_GO.as_ptr() as usize)
            .expect("the go flag");
        guest.memory().write_value(go, &1).map_err(|_| Errno::EIO)?;
        let tid = guest.tid().as_raw();
        for _ in 0..500 {
            let exiting = match std::fs::read_to_string(format!("/proc/{tid}/status")) {
                Ok(status) => status
                    .lines()
                    .find_map(|line| line.strip_prefix("State:"))
                    .is_some_and(|state| {
                        matches!(state.trim_start().as_bytes().first(), Some(b'Z' | b'X'))
                    }),
                Err(_) => true,
            };
            if exiting {
                return Ok(Some(signal));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the guest did not exit in 5 s");
    }
}

/// `RestartResult`, which answers the probe as configured, declaring
/// serialized execution as configured, whose report of SIGUSR1 lowers the
/// tracer's open-file limit to 0 (`close_the_tracers_files`), so that no
/// procfs status can be read before the resume; the test puts it back.
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct SignalStopClosesTheTracersFiles(RestartResult);

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for SignalStopClosesTheTracersFiles {
    type GlobalState = Global;
    type ThreadState = ThreadState;

    fn enforces_serialized_execution(serialized: &bool) -> bool {
        *serialized
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != PROBE {
            return guest.tail_inject(syscall).await;
        }
        let (_, args) = syscall.into_parts();
        answer_probe(&self.0, guest, args).await
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        _guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        if signal == reverie::Signal::SIGUSR1 {
            close_the_tracers_files();
        }
        Ok(Some(signal))
    }
}

/// `RunsTheGuestsPselect6WithAnUnreadSet`, which returns
/// `-ERESTART_RESTARTBLOCK` where the call returns `-ERESTARTNOHAND`, with
/// the temporary mask's restore pending and no signal held, so the backend
/// leaves the code to the kernel from the call's exit stop
/// (`defer_restart_under_pending_mask_restore` on aarch64); its report of
/// SIGUSR1 first lets a sibling thread of the guest install a seccomp filter
/// on every thread and waits for it (`await_a_siblings_filter`).
#[cfg(all(not(sanitized), test))]
#[derive(Debug, Default, Clone)]
struct ADeferredRestartBlockAwaitsASiblingsFilter;

#[cfg(all(not(sanitized), test))]
#[reverie::tool]
impl Tool for ADeferredRestartBlockAwaitsASiblingsFilter {
    type GlobalState = Global;
    type ThreadState = ();

    fn enforces_serialized_execution(serialized: &bool) -> bool {
        *serialized
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != Sysno::pselect6 {
            return guest.tail_inject(syscall).await;
        }
        let (nr, args) = syscall.into_parts();
        let unread_set = args.arg4;
        let call = Syscall::from_raw(
            nr,
            SyscallArgs::new(
                args.arg0, unread_set, args.arg2, args.arg3, args.arg4, args.arg5,
            ),
        );
        match guest.inject(call).await {
            Err(Errno::ERESTARTNOHAND) => Err(Errno::ERESTART_RESTARTBLOCK.into()),
            result => Ok(result?),
        }
    }

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        if signal == reverie::Signal::SIGUSR1 {
            await_a_siblings_filter(guest).await?;
        }
        Ok(Some(signal))
    }
}

/// Waits until `sig` has left the shared pending set of process `pid`
/// (`ShdPnd` in its procfs status), taken by one of its threads.
#[cfg(all(not(sanitized), test))]
async fn wait_until_taken_from_the_process(pid: i32, sig: i32) {
    let bit = 1u64 << (sig - 1);
    for _ in 0..500 {
        let status =
            std::fs::read_to_string(format!("/proc/{pid}/status")).expect("read the status");
        let shared = status
            .lines()
            .find_map(|line| line.strip_prefix("ShdPnd:"))
            .map(|hex| u64::from_str_radix(hex.trim(), 16).expect("ShdPnd is hexadecimal"))
            .expect("the status has ShdPnd");
        if shared & bit == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("signal {sig} still pending for process {pid} after 5 s");
}

#[cfg(all(not(sanitized), test))]
mod tests {
    use std::sync::atomic::AtomicI64;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use nix::sys::signal::SaFlags;
    use nix::sys::signal::SigAction;
    use nix::sys::signal::SigHandler;
    use nix::sys::signal::SigSet;
    use nix::sys::signal::Signal;
    use nix::sys::signal::sigaction;
    use reverie_ptrace::testing::check_fn;
    use reverie_ptrace::testing::check_fn_with_config;
    use test_case::test_case;

    use super::*;

    // The restart codes (include/linux/errno.h), which libc does not name.
    const ERESTARTSYS: i32 = 512;
    const ERESTARTNOINTR: i32 = 513;
    const ERESTARTNOHAND: i32 = 514;
    const ENOIOCTLCMD: i32 = 515;
    const ERESTART_RESTARTBLOCK: i32 = 516;

    static HANDLED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn on_signal(_: libc::c_int) {
        HANDLED.fetch_add(1, Ordering::SeqCst);
    }

    /// What the handler's own `ppoll` returned (`on_signal_polling`).
    static HANDLER_POLL: AtomicI64 = AtomicI64::new(i64::MIN);

    /// Runs a `ppoll` of its own through the same `syscall` wrapper, so the
    /// same `svc`, as the guest's interrupted call: one ignored descriptor and
    /// a zero timeout, which returns 0 at once.
    extern "C" fn on_signal_polling(_: libc::c_int) {
        HANDLED.fetch_add(1, Ordering::SeqCst);
        let mut ignored = libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        };
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ppoll reads one descriptor entry and the timeout.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_ppoll,
                &mut ignored as *mut libc::pollfd,
                1,
                &zero as *const libc::timespec,
                std::ptr::null::<libc::sigset_t>(),
                std::mem::size_of::<u64>(),
            )
        };
        HANDLER_POLL.store(ret, Ordering::SeqCst);
    }

    /// Handles `sig` under `flags`, or leaves it at its default action
    /// without `flags`.
    fn handle(sig: Signal, flags: Option<SaFlags>) {
        if let Some(flags) = flags {
            let action = SigAction::new(SigHandler::Handler(on_signal), flags, SigSet::empty());
            // SAFETY: the handler only increments an atomic.
            unsafe { sigaction(sig, &action) }.expect("install the handler");
        }
    }

    /// Runs the probe in the guest with `sig` handled under `flags` (or left
    /// at its default action without `flags`), and returns its result and
    /// errno and the number of handler runs.
    fn probe(code: i32, sig: Signal, flags: Option<SaFlags>, sent_by: i64) -> (i64, i32, usize) {
        probe_number(libc::SYS_getppid, code, sig, flags, sent_by)
    }

    /// `probe`, made with the syscall number `nr`, with the code as the fourth
    /// argument too (`UnknownNumberRestart`).
    fn probe_number(
        nr: libc::c_long,
        code: i32,
        sig: Signal,
        flags: Option<SaFlags>,
        sent_by: i64,
    ) -> (i64, i32, usize) {
        handle(sig, flags);
        // SAFETY: the probe syscall takes integers and touches no memory.
        let ret = unsafe {
            libc::syscall(
                nr,
                code as libc::c_long,
                sig as libc::c_long,
                sent_by as libc::c_long,
                code as libc::c_long,
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
        )
    }

    /// `probe_number` for a number whose whole register matters
    /// (`NO_SYSCALL_PROBES`, `WIDE_PROBES`): made with `raw_syscall`, since
    /// the C library's `syscall` passes only the low 32 bits of the number on
    /// aarch64.
    fn probe_raw_number(
        nr: u64,
        code: i32,
        sig: Signal,
        flags: Option<SaFlags>,
        sent_by: i64,
    ) -> (i64, i32, usize) {
        handle(sig, flags);
        let args = [code as u64, sig as i32 as u64, sent_by as u64, code as u64];
        // SAFETY: the probe takes integers and touches no memory.
        let ret = unsafe { raw_syscall(nr, args) };
        let handled = HANDLED.load(Ordering::SeqCst);
        if (-4095..0).contains(&ret) {
            (-1, -ret as i32, handled)
        } else {
            (ret, 0, handled)
        }
    }

    /// Makes the syscall `nr`, the whole register as given, with the first
    /// four arguments `args`, and returns its raw result.
    ///
    /// # Safety
    ///
    /// As for the call `nr` with those arguments.
    unsafe fn raw_syscall(nr: u64, args: [u64; 4]) -> i64 {
        let ret: i64;
        // SAFETY: the caller's; `syscall` clobbers rcx and r11 and no memory
        // of this function.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::asm!(
                "syscall",
                inlateout("rax") nr as i64 => ret,
                in("rdi") args[0],
                in("rsi") args[1],
                in("rdx") args[2],
                in("r10") args[3],
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack),
            );
        }
        // SAFETY: the caller's; `svc` writes only `x0`.
        #[cfg(target_arch = "aarch64")]
        unsafe {
            std::arch::asm!(
                "svc #0",
                inlateout("x0") args[0] as i64 => ret,
                in("x1") args[1],
                in("x2") args[2],
                in("x3") args[3],
                in("x8") nr,
                options(nostack),
            );
        }
        ret
    }

    fn check(
        code: i32,
        sig: Signal,
        flags: Option<SaFlags>,
        sent_by: i64,
        want: (i64, i32, usize),
    ) {
        check_fn::<RestartResult, _>(move || {
            let got = probe(code, sig, flags, sent_by);
            assert_eq!(got, want, "(result, errno, handler runs)");
        });
    }

    #[test_case(SENT_BY_TRACER, 1; "sent by the tracer")]
    #[test_case(SENT_BY_INJECTION, 1; "sent by an injection")]
    #[test_case(HELD_OVER_INJECTION, 1; "held over an injection")]
    #[test_case(HELD_AND_PENDING_FOR_THE_PROCESS, 2; "held and pending for the process")]
    fn erestartnohand_with_a_handler_is_eintr(sent_by: i64, runs: usize) {
        let flags = Some(SaFlags::SA_RESTART);
        check(
            ERESTARTNOHAND,
            Signal::SIGUSR1,
            flags,
            sent_by,
            (-1, libc::EINTR, runs),
        );
    }

    #[test_case(SENT_BY_TRACER, 1; "sent by the tracer")]
    #[test_case(SENT_BY_INJECTION, 1; "sent by an injection")]
    #[test_case(HELD_OVER_INJECTION, 1; "held over an injection")]
    #[test_case(HELD_AND_PENDING_FOR_THE_PROCESS, 2; "held and pending for the process")]
    fn erestart_restartblock_with_a_handler_is_eintr(sent_by: i64, runs: usize) {
        let flags = Some(SaFlags::SA_RESTART);
        check(
            ERESTART_RESTARTBLOCK,
            Signal::SIGUSR1,
            flags,
            sent_by,
            (-1, libc::EINTR, runs),
        );
    }

    #[test_case(SENT_BY_TRACER, 1; "sent by the tracer")]
    #[test_case(SENT_BY_INJECTION, 1; "sent by an injection")]
    #[test_case(HELD_OVER_INJECTION, 1; "held over an injection")]
    #[test_case(HELD_AND_PENDING_FOR_THE_PROCESS, 2; "held and pending for the process")]
    fn erestartsys_without_sa_restart_is_eintr(sent_by: i64, runs: usize) {
        let flags = Some(SaFlags::empty());
        check(
            ERESTARTSYS,
            Signal::SIGUSR1,
            flags,
            sent_by,
            (-1, libc::EINTR, runs),
        );
    }

    #[test_case(SENT_BY_TRACER, 1; "sent by the tracer")]
    #[test_case(SENT_BY_INJECTION, 1; "sent by an injection")]
    #[test_case(HELD_OVER_INJECTION, 1; "held over an injection")]
    #[test_case(HELD_AND_PENDING_FOR_THE_PROCESS, 2; "held and pending for the process")]
    fn erestartsys_with_sa_restart_restarts(sent_by: i64, runs: usize) {
        let flags = Some(SaFlags::SA_RESTART);
        check(
            ERESTARTSYS,
            Signal::SIGUSR1,
            flags,
            sent_by,
            (RESTARTED, 0, runs),
        );
    }

    #[test_case(SENT_BY_TRACER, 1; "sent by the tracer")]
    #[test_case(SENT_BY_INJECTION, 1; "sent by an injection")]
    #[test_case(HELD_OVER_INJECTION, 1; "held over an injection")]
    #[test_case(HELD_AND_PENDING_FOR_THE_PROCESS, 2; "held and pending for the process")]
    fn erestartnointr_with_a_handler_restarts(sent_by: i64, runs: usize) {
        let flags = Some(SaFlags::empty());
        check(
            ERESTARTNOINTR,
            Signal::SIGUSR1,
            flags,
            sent_by,
            (RESTARTED, 0, runs),
        );
    }

    /// SIGWINCH's default action ignores it; a traced thread still queues it
    /// and takes it at the resume, with no handler, so every code restarts,
    /// `-ERESTART_RESTARTBLOCK` through `restart_syscall`.
    #[test_case(ERESTARTSYS, SENT_BY_TRACER; "erestartsys sent by the tracer")]
    #[test_case(ERESTARTSYS, SENT_BY_INJECTION; "erestartsys sent by an injection")]
    #[test_case(ERESTARTSYS, HELD_OVER_INJECTION; "erestartsys held over an injection")]
    #[test_case(ERESTARTSYS, HELD_AND_PENDING_FOR_THE_PROCESS; "erestartsys held and pending for the process")]
    #[test_case(ERESTARTNOINTR, SENT_BY_TRACER; "erestartnointr sent by the tracer")]
    #[test_case(ERESTARTNOINTR, SENT_BY_INJECTION; "erestartnointr sent by an injection")]
    #[test_case(ERESTARTNOINTR, HELD_OVER_INJECTION; "erestartnointr held over an injection")]
    #[test_case(ERESTARTNOINTR, HELD_AND_PENDING_FOR_THE_PROCESS; "erestartnointr held and pending for the process")]
    #[test_case(ERESTARTNOHAND, SENT_BY_TRACER; "erestartnohand sent by the tracer")]
    #[test_case(ERESTARTNOHAND, SENT_BY_INJECTION; "erestartnohand sent by an injection")]
    #[test_case(ERESTARTNOHAND, HELD_OVER_INJECTION; "erestartnohand held over an injection")]
    #[test_case(ERESTARTNOHAND, HELD_AND_PENDING_FOR_THE_PROCESS; "erestartnohand held and pending for the process")]
    #[test_case(ERESTART_RESTARTBLOCK, SENT_BY_TRACER; "erestart_restartblock sent by the tracer")]
    #[test_case(ERESTART_RESTARTBLOCK, SENT_BY_INJECTION; "erestart_restartblock sent by an injection")]
    #[test_case(ERESTART_RESTARTBLOCK, HELD_OVER_INJECTION; "erestart_restartblock held over an injection")]
    #[test_case(ERESTART_RESTARTBLOCK, HELD_AND_PENDING_FOR_THE_PROCESS; "erestart_restartblock held and pending for the process")]
    fn without_a_handler_restarts(code: i32, sent_by: i64) {
        check(code, Signal::SIGWINCH, None, sent_by, (RESTARTED, 0, 0));
    }

    /// A held signal the Tool drops runs no handler, as one a tracer
    /// suppresses at its signal-delivery stop, so every code restarts.
    #[test_case(ERESTARTSYS; "erestartsys")]
    #[test_case(ERESTARTNOINTR; "erestartnointr")]
    #[test_case(ERESTARTNOHAND; "erestartnohand")]
    #[test_case(ERESTART_RESTARTBLOCK; "erestart_restartblock")]
    fn a_dropped_signal_restarts(code: i32) {
        let flags = Some(SaFlags::empty());
        check(
            code,
            Signal::SIGUSR1,
            flags,
            HELD_AND_DROPPED,
            (RESTARTED, 0, 0),
        );
    }

    /// A held SIGCONT is delivered as any other signal: its handler runs, and
    /// `-ERESTARTSYS` without `SA_RESTART` is `-EINTR`.
    #[test_case(SENT_BY_TRACER; "sent by the tracer")]
    #[test_case(HELD_OVER_INJECTION; "held over an injection")]
    fn a_sigcont_with_a_handler_is_eintr(sent_by: i64) {
        let flags = Some(SaFlags::empty());
        check(
            ERESTARTSYS,
            Signal::SIGCONT,
            flags,
            sent_by,
            (-1, libc::EINTR, 1),
        );
    }

    /// A SIGSTOP, which no mask blocks, stops the thread on its way back to
    /// the call and runs no handler, so the call restarts once the thread
    /// goes on (`do_signal_stop`, kernel/signal.c).
    #[test_case(ERESTARTSYS, SENT_BY_TRACER; "erestartsys sent by the tracer")]
    #[test_case(ERESTARTNOINTR, SENT_BY_TRACER; "erestartnointr sent by the tracer")]
    #[test_case(ERESTARTNOHAND, SENT_BY_TRACER; "erestartnohand sent by the tracer")]
    #[test_case(ERESTART_RESTARTBLOCK, SENT_BY_TRACER; "erestart_restartblock sent by the tracer")]
    #[test_case(ERESTARTSYS, SENT_BY_INJECTION; "erestartsys sent by an injection")]
    #[test_case(ERESTARTSYS, HELD_OVER_INJECTION; "erestartsys held over an injection")]
    fn a_stop_signal_restarts(code: i32, sent_by: i64) {
        check(code, Signal::SIGSTOP, None, sent_by, (RESTARTED, 0, 0));
    }

    /// Negative: an ordinary error is returned as it is, whatever is
    /// pending; the handler of the pending signal runs.
    #[test_case(libc::EINTR, SENT_BY_TRACER; "eintr sent by the tracer")]
    #[test_case(libc::EINTR, HELD_OVER_INJECTION; "eintr held over an injection")]
    #[test_case(libc::EAGAIN, SENT_BY_TRACER; "eagain sent by the tracer")]
    #[test_case(libc::EAGAIN, HELD_OVER_INJECTION; "eagain held over an injection")]
    fn an_ordinary_error_is_returned_as_it_is(code: i32, sent_by: i64) {
        let flags = Some(SaFlags::SA_RESTART);
        check(code, Signal::SIGUSR1, flags, sent_by, (-1, code, 1));
    }

    /// A code the Tool returns with no signal pending at all restarts, as
    /// Linux restarts a call whose signal is gone by its delivery:
    /// `-ERESTART_RESTARTBLOCK` through `restart_syscall`. On aarch64 the
    /// backend's carrier signal runs the kernel's restart processing; nothing
    /// else would.
    #[test_case(ERESTARTSYS; "erestartsys")]
    #[test_case(ERESTARTNOINTR; "erestartnointr")]
    #[test_case(ERESTARTNOHAND; "erestartnohand")]
    #[test_case(ERESTART_RESTARTBLOCK; "erestart_restartblock")]
    fn a_code_returned_with_no_signal_restarts(code: i32) {
        let flags = Some(SaFlags::SA_RESTART);
        check(code, Signal::SIGUSR1, flags, NO_SIGNAL, (RESTARTED, 0, 0));
    }

    /// A code returned alone after an injection of a mask-swapping `ppoll`
    /// that returned at once restarts too, stepped or, under a Tool that
    /// declares serialized execution, run between syscall stops: from the
    /// injection's exit stop the backend's carrier signal runs the kernel's
    /// restart processing on both architectures; nothing else would.
    #[test_case(false, ERESTARTNOHAND; "stepped erestartnohand")]
    #[test_case(true, ERESTARTSYS; "syscall stops erestartsys")]
    #[test_case(true, ERESTARTNOINTR; "syscall stops erestartnointr")]
    #[test_case(true, ERESTARTNOHAND; "syscall stops erestartnohand")]
    #[test_case(true, ERESTART_RESTARTBLOCK; "syscall stops erestart_restartblock")]
    fn a_code_returned_with_no_signal_after_a_mask_swapping_injection_restarts(
        serialized: bool,
        code: i32,
    ) {
        check_fn_with_config::<RestartResult, _>(
            move || {
                let got = probe_after_an_injected_ppoll(libc::SYS_getppid, code);
                assert_eq!(got, (RESTARTED, 0), "(result, errno)");
            },
            serialized,
            true,
        );
    }

    /// Makes the probe `nr` with `code` for `INJECTED_PPOLL_THEN_NO_SIGNAL`,
    /// with a zero timeout and an empty mask for the Tool's `ppoll`, and
    /// returns its result and errno.
    fn probe_after_an_injected_ppoll(nr: libc::c_long, code: i32) -> (i64, i32) {
        probe_after_an_injected_ppoll_with(nr, code, INJECTED_PPOLL_THEN_NO_SIGNAL)
    }

    /// `probe_after_an_injected_ppoll`, for `mode`.
    fn probe_after_an_injected_ppoll_with(nr: libc::c_long, code: i32, mode: i64) -> (i64, i32) {
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let mask = SigSet::empty();
        // SAFETY: the probe takes integers and two pointers, which the Tool's
        // `ppoll` reads and which outlive the call.
        let ret = unsafe {
            libc::syscall(
                nr,
                code as libc::c_long,
                libc::SIGUSR1 as libc::c_long,
                mode as libc::c_long,
                &zero as *const libc::timespec,
                mask.as_ref() as *const libc::sigset_t,
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        (ret, if ret == -1 { errno } else { 0 })
    }

    /// `ENOIOCTLCMD` (515), which `ioctl` turns into `ENOTTY` inside the
    /// kernel, is neither returned nor restarted by Linux: refused by name,
    /// with a signal pending or none.
    #[test_case(SENT_BY_TRACER; "sent by the tracer")]
    #[test_case(NO_SIGNAL; "no signal")]
    fn enoioctlcmd_is_refused(sent_by: i64) {
        assert_refused::<RestartResult>(
            reverie_ptrace::RestartRefusalReason::KernelInternalCode,
            move || {
                probe(
                    ENOIOCTLCMD,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    sent_by,
                );
            },
        );
    }

    /// `check` for the unknown probe (`UNKNOWN_PROBE`, `UnknownNumberRestart`).
    fn check_unknown(
        code: i32,
        sig: Signal,
        flags: Option<SaFlags>,
        sent_by: i64,
        want: (i64, i32, usize),
    ) {
        check_fn::<UnknownNumberRestart, _>(move || {
            let got = probe_number(UNKNOWN_PROBE as libc::c_long, code, sig, flags, sent_by);
            assert_eq!(got, want, "(result, errno, handler runs)");
        });
    }

    /// A number the syscall table cannot name, which the Tool answers under
    /// `UnknownSyscallPolicy::TrapToTool`, gets the same outcomes. With a
    /// handler, `-ERESTARTNOHAND`, `-ERESTART_RESTARTBLOCK` and `-ERESTARTSYS`
    /// without `SA_RESTART` are `-EINTR`; `-ERESTARTSYS` with `SA_RESTART`
    /// and `-ERESTARTNOINTR` run the number again.
    #[test_case(ERESTARTSYS, SaFlags::SA_RESTART, (RESTARTED, 0, 1); "erestartsys with sa_restart")]
    #[test_case(ERESTARTSYS, SaFlags::empty(), (-1, libc::EINTR, 1); "erestartsys without sa_restart")]
    #[test_case(ERESTARTNOINTR, SaFlags::empty(), (RESTARTED, 0, 1); "erestartnointr")]
    #[test_case(ERESTARTNOHAND, SaFlags::SA_RESTART, (-1, libc::EINTR, 1); "erestartnohand")]
    #[test_case(ERESTART_RESTARTBLOCK, SaFlags::SA_RESTART, (-1, libc::EINTR, 1); "erestart_restartblock")]
    fn an_unknown_numbers_restart_code_with_a_handler(
        code: i32,
        flags: SaFlags,
        want: (i64, i32, usize),
    ) {
        check_unknown(code, Signal::SIGUSR1, Some(flags), SENT_BY_TRACER, want);
    }

    /// With no handler (SIGWINCH) every code runs the number again,
    /// `-ERESTART_RESTARTBLOCK` as `restart_syscall`.
    #[test_case(ERESTARTSYS; "erestartsys")]
    #[test_case(ERESTARTNOINTR; "erestartnointr")]
    #[test_case(ERESTARTNOHAND; "erestartnohand")]
    #[test_case(ERESTART_RESTARTBLOCK; "erestart_restartblock")]
    fn an_unknown_number_without_a_handler_runs_again(code: i32) {
        check_unknown(
            code,
            Signal::SIGWINCH,
            None,
            SENT_BY_TRACER,
            (RESTARTED, 0, 0),
        );
    }

    /// Negative: an unknown number's ordinary error is returned as it is.
    #[test_case(libc::EINTR; "eintr")]
    #[test_case(libc::EAGAIN; "eagain")]
    fn an_unknown_numbers_ordinary_error_is_returned_as_it_is(code: i32) {
        let flags = Some(SaFlags::SA_RESTART);
        check_unknown(code, Signal::SIGUSR1, flags, SENT_BY_TRACER, (-1, code, 1));
    }

    /// An unknown number's code returned with no signal runs the number
    /// again, `-ERESTART_RESTARTBLOCK` as `restart_syscall`.
    #[test_case(ERESTARTSYS; "erestartsys")]
    #[test_case(ERESTARTNOINTR; "erestartnointr")]
    #[test_case(ERESTARTNOHAND; "erestartnohand")]
    #[test_case(ERESTART_RESTARTBLOCK; "erestart_restartblock")]
    fn an_unknown_numbers_code_returned_with_no_signal_runs_again(code: i32) {
        let flags = Some(SaFlags::SA_RESTART);
        check_unknown(code, Signal::SIGUSR1, flags, NO_SIGNAL, (RESTARTED, 0, 0));
    }

    /// `ENOIOCTLCMD` for an unknown number is refused by name too.
    #[test]
    fn an_unknown_numbers_enoioctlcmd_is_refused() {
        assert_unknown_refused::<UnknownNumberRestart>(
            reverie_ptrace::RestartRefusalReason::KernelInternalCode,
            UNKNOWN_PROBE,
            || {
                probe_number(
                    UNKNOWN_PROBE as libc::c_long,
                    ENOIOCTLCMD,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    SENT_BY_TRACER,
                );
            },
        );
    }

    /// A restart code for a number whose low 32 bits are all ones is refused
    /// by name on every architecture, before the result is written: Linux
    /// reads the number as -1, no syscall, and its restart processing would
    /// skip the call and leave the code to the guest.
    #[test_case(0, ERESTARTSYS; "0xffffffff erestartsys")]
    #[test_case(0, ERESTARTNOINTR; "0xffffffff erestartnointr")]
    #[test_case(0, ERESTARTNOHAND; "0xffffffff erestartnohand")]
    #[test_case(0, ERESTART_RESTARTBLOCK; "0xffffffff erestart_restartblock")]
    #[test_case(1, ERESTARTSYS; "u64 max erestartsys")]
    #[test_case(1, ERESTARTNOHAND; "u64 max erestartnohand")]
    #[test_case(2, ERESTARTSYS; "high word 1 low word all ones erestartsys")]
    #[test_case(2, ERESTARTNOHAND; "high word 1 low word all ones erestartnohand")]
    fn a_restart_code_for_a_number_read_as_no_syscall_is_refused(probe: usize, code: i32) {
        let nr = NO_SYSCALL_PROBES[probe];
        assert_unknown_refused::<UnknownNumberRestart>(
            reverie_ptrace::RestartRefusalReason::NoKernelSyscallNumber,
            nr,
            move || {
                probe_raw_number(
                    nr,
                    code,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    SENT_BY_TRACER,
                );
            },
        );
    }

    /// Negative: the same numbers' ordinary errors are returned as they are.
    #[test_case(0; "0xffffffff")]
    #[test_case(1; "u64 max")]
    #[test_case(2; "high word 1 low word all ones")]
    fn a_number_read_as_no_syscall_returns_an_ordinary_error(probe: usize) {
        let nr = NO_SYSCALL_PROBES[probe];
        check_fn::<UnknownNumberRestart, _>(move || {
            let got = probe_raw_number(
                nr,
                libc::EAGAIN,
                Signal::SIGUSR1,
                Some(SaFlags::SA_RESTART),
                SENT_BY_TRACER,
            );
            assert_eq!(got, (-1, libc::EAGAIN, 1), "(result, errno, handler runs)");
        });
    }

    /// A number with the high word set, or a negative low word other than
    /// -1, names a call to Linux by its low word: the kernel's outcomes, the
    /// number run again with the whole register.
    #[test_case(0, ERESTARTNOHAND, Signal::SIGUSR1, (-1, libc::EINTR, 1); "high word 1 erestartnohand with a handler")]
    #[test_case(0, ERESTARTSYS, Signal::SIGWINCH, (RESTARTED, 0, 0); "high word 1 erestartsys without a handler")]
    #[test_case(0, ERESTART_RESTARTBLOCK, Signal::SIGWINCH, (RESTARTED, 0, 0); "high word 1 erestart_restartblock without a handler")]
    #[test_case(1, ERESTARTNOHAND, Signal::SIGUSR1, (-1, libc::EINTR, 1); "high word all ones erestartnohand with a handler")]
    #[test_case(1, ERESTARTSYS, Signal::SIGWINCH, (RESTARTED, 0, 0); "high word all ones erestartsys without a handler")]
    #[test_case(2, ERESTARTNOHAND, Signal::SIGUSR1, (-1, libc::EINTR, 1); "negative low word erestartnohand with a handler")]
    #[test_case(2, ERESTARTNOINTR, Signal::SIGWINCH, (RESTARTED, 0, 0); "negative low word erestartnointr without a handler")]
    fn a_wide_numbers_restart_code(probe: usize, code: i32, sig: Signal, want: (i64, i32, usize)) {
        let nr = WIDE_PROBES[probe];
        let flags = (sig == Signal::SIGUSR1).then_some(SaFlags::SA_RESTART);
        check_fn::<UnknownNumberRestart, _>(move || {
            let got = probe_raw_number(nr, code, sig, flags, SENT_BY_TRACER);
            assert_eq!(got, want, "(result, errno, handler runs)");
        });
    }

    /// Puts the tracer's open-file limit back when dropped, after a Tool
    /// closed it (`close_the_tracers_files`) and nothing reopened it.
    struct OpenFileLimit(libc::rlimit);

    impl OpenFileLimit {
        fn kept() -> Self {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit writes the one rlimit it is passed.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
                0
            );
            Self(limit)
        }
    }

    impl Drop for OpenFileLimit {
        fn drop(&mut self) {
            // SAFETY: setrlimit reads the one rlimit it is passed.
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.0) };
        }
    }

    /// The tracer cannot read its own procfs status (no descriptor can be
    /// opened), so it cannot rule out a seccomp filter on its own thread, and
    /// a code returned alone goes to `Tool::handle_unresolved_restart`, whose
    /// default answers the code itself. Nothing hands that answer to the
    /// kernel either: it is refused by name, on both architectures (on
    /// x86-64 after an injection run between syscall stops, the route that
    /// reaches the hook there).
    #[test_case(ERESTARTSYS; "erestartsys")]
    #[test_case(ERESTARTNOHAND; "erestartnohand")]
    fn an_unreadable_status_with_the_default_hook_is_refused(code: i32) {
        let _limit = OpenFileLimit::kept();
        assert_refused_with_config::<StatusUnreadable>(
            reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                answer: Errno::new(code),
            },
            (true, 0),
            move || {
                probe_after_an_injected_ppoll_with(libc::SYS_getppid, code, STATUS_UNREADABLE);
            },
        );
    }

    /// A hook that answers a code internal to Linux, `ENOIOCTLCMD` or a
    /// restart code, is refused by name too.
    #[test_case(ENOIOCTLCMD; "enoioctlcmd")]
    #[test_case(ERESTARTNOINTR; "erestartnointr")]
    fn an_unresolved_restart_answered_with_an_internal_code_is_refused(answer: i32) {
        let _limit = OpenFileLimit::kept();
        assert_refused_with_config::<StatusUnreadableAnswering>(
            reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                answer: Errno::new(answer),
            },
            (true, answer),
            || {
                probe_after_an_injected_ppoll_with(
                    libc::SYS_getppid,
                    ERESTARTNOHAND,
                    STATUS_UNREADABLE,
                );
            },
        );
    }

    /// Negative: a hook that answers an ordinary error gives the guest that
    /// error.
    #[test]
    fn an_unresolved_restart_answered_with_an_ordinary_error_is_returned() {
        let _limit = OpenFileLimit::kept();
        check_fn_with_config::<StatusUnreadableAnswering, _>(
            || {
                let got = probe_after_an_injected_ppoll_with(
                    libc::SYS_getppid,
                    ERESTARTNOHAND,
                    STATUS_UNREADABLE,
                );
                assert_eq!(got, (-1, libc::EINTR), "(result, errno)");
            },
            (true, libc::EINTR),
            true,
        );
    }

    /// The same with the signal sent by an injected `tgkill`, held over an
    /// injection, or held with a second instance pending for the process:
    /// the backend carries a held signal into the kernel's restart processing
    /// as for a named syscall (`return_restart_through_syscall_exit`).
    #[test_case(ERESTARTSYS, SaFlags::SA_RESTART, SENT_BY_INJECTION, (RESTARTED, 0, 1); "erestartsys with sa_restart sent by an injection")]
    #[test_case(ERESTARTSYS, SaFlags::empty(), SENT_BY_INJECTION, (-1, libc::EINTR, 1); "erestartsys without sa_restart sent by an injection")]
    #[test_case(ERESTARTNOINTR, SaFlags::empty(), SENT_BY_INJECTION, (RESTARTED, 0, 1); "erestartnointr sent by an injection")]
    #[test_case(ERESTARTNOHAND, SaFlags::SA_RESTART, SENT_BY_INJECTION, (-1, libc::EINTR, 1); "erestartnohand sent by an injection")]
    #[test_case(ERESTART_RESTARTBLOCK, SaFlags::SA_RESTART, SENT_BY_INJECTION, (-1, libc::EINTR, 1); "erestart_restartblock sent by an injection")]
    #[test_case(ERESTARTSYS, SaFlags::SA_RESTART, HELD_OVER_INJECTION, (RESTARTED, 0, 1); "erestartsys with sa_restart held over an injection")]
    #[test_case(ERESTARTSYS, SaFlags::empty(), HELD_OVER_INJECTION, (-1, libc::EINTR, 1); "erestartsys without sa_restart held over an injection")]
    #[test_case(ERESTARTNOINTR, SaFlags::empty(), HELD_OVER_INJECTION, (RESTARTED, 0, 1); "erestartnointr held over an injection")]
    #[test_case(ERESTARTNOHAND, SaFlags::SA_RESTART, HELD_OVER_INJECTION, (-1, libc::EINTR, 1); "erestartnohand held over an injection")]
    #[test_case(ERESTART_RESTARTBLOCK, SaFlags::SA_RESTART, HELD_OVER_INJECTION, (-1, libc::EINTR, 1); "erestart_restartblock held over an injection")]
    #[test_case(ERESTARTSYS, SaFlags::SA_RESTART, HELD_AND_PENDING_FOR_THE_PROCESS, (RESTARTED, 0, 2); "erestartsys with sa_restart held and pending for the process")]
    #[test_case(ERESTARTSYS, SaFlags::empty(), HELD_AND_PENDING_FOR_THE_PROCESS, (-1, libc::EINTR, 2); "erestartsys without sa_restart held and pending for the process")]
    #[test_case(ERESTARTNOINTR, SaFlags::empty(), HELD_AND_PENDING_FOR_THE_PROCESS, (RESTARTED, 0, 2); "erestartnointr held and pending for the process")]
    #[test_case(ERESTARTNOHAND, SaFlags::SA_RESTART, HELD_AND_PENDING_FOR_THE_PROCESS, (-1, libc::EINTR, 2); "erestartnohand held and pending for the process")]
    #[test_case(ERESTART_RESTARTBLOCK, SaFlags::SA_RESTART, HELD_AND_PENDING_FOR_THE_PROCESS, (-1, libc::EINTR, 2); "erestart_restartblock held and pending for the process")]
    fn an_unknown_numbers_restart_code_with_a_handler_after_an_injection(
        code: i32,
        flags: SaFlags,
        sent_by: i64,
        want: (i64, i32, usize),
    ) {
        check_unknown(code, Signal::SIGUSR1, Some(flags), sent_by, want);
    }

    /// With no handler (SIGWINCH), or with the held signal dropped, which runs
    /// no handler, every code runs the number again after an injection too.
    #[test_case(ERESTARTSYS, Signal::SIGWINCH, SENT_BY_INJECTION; "erestartsys without a handler sent by an injection")]
    #[test_case(ERESTARTSYS, Signal::SIGWINCH, HELD_OVER_INJECTION; "erestartsys without a handler held over an injection")]
    #[test_case(ERESTARTSYS, Signal::SIGUSR1, HELD_AND_DROPPED; "erestartsys with the held signal dropped")]
    #[test_case(ERESTARTNOINTR, Signal::SIGWINCH, SENT_BY_INJECTION; "erestartnointr without a handler sent by an injection")]
    #[test_case(ERESTARTNOINTR, Signal::SIGWINCH, HELD_OVER_INJECTION; "erestartnointr without a handler held over an injection")]
    #[test_case(ERESTARTNOINTR, Signal::SIGUSR1, HELD_AND_DROPPED; "erestartnointr with the held signal dropped")]
    #[test_case(ERESTARTNOHAND, Signal::SIGWINCH, SENT_BY_INJECTION; "erestartnohand without a handler sent by an injection")]
    #[test_case(ERESTARTNOHAND, Signal::SIGWINCH, HELD_OVER_INJECTION; "erestartnohand without a handler held over an injection")]
    #[test_case(ERESTARTNOHAND, Signal::SIGUSR1, HELD_AND_DROPPED; "erestartnohand with the held signal dropped")]
    #[test_case(ERESTART_RESTARTBLOCK, Signal::SIGWINCH, SENT_BY_INJECTION; "erestart_restartblock without a handler sent by an injection")]
    #[test_case(ERESTART_RESTARTBLOCK, Signal::SIGWINCH, HELD_OVER_INJECTION; "erestart_restartblock without a handler held over an injection")]
    #[test_case(ERESTART_RESTARTBLOCK, Signal::SIGUSR1, HELD_AND_DROPPED; "erestart_restartblock with the held signal dropped")]
    fn an_unknown_number_without_a_handler_runs_again_after_an_injection(
        code: i32,
        sig: Signal,
        sent_by: i64,
    ) {
        let flags = (sig == Signal::SIGUSR1).then_some(SaFlags::empty());
        check_unknown(code, sig, flags, sent_by, (RESTARTED, 0, 0));
    }

    /// Installs a seccomp filter that allows every call on the calling thread:
    /// the guest's own, or the tracer's (`under_a_tracer_filter`).
    fn install_a_seccomp_filter_allowing_every_call() {
        let allow = libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        };
        let program = libc::sock_fprog {
            len: 1,
            filter: &allow as *const libc::sock_filter as *mut libc::sock_filter,
        };
        // SAFETY: prctl takes integers here and touches no memory.
        let no_new_privs = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        assert_eq!(no_new_privs, 0, "set no_new_privs");
        // SAFETY: seccomp reads the one-instruction program, which outlives the call.
        let installed = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0,
                &program as *const libc::sock_fprog,
            )
        };
        assert_eq!(installed, 0, "install the filter");
    }

    /// Under a seccomp filter of the guest's own, x86-64's kernel resolves an
    /// unknown number's restart code as any other: `-EINTR` with a handler.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_unknown_numbers_restart_code_under_a_guest_seccomp_filter() {
        check_fn::<UnknownNumberRestart, _>(|| {
            install_a_seccomp_filter_allowing_every_call();
            let got = probe_number(
                UNKNOWN_PROBE as libc::c_long,
                ERESTARTNOHAND,
                Signal::SIGUSR1,
                Some(SaFlags::SA_RESTART),
                SENT_BY_TRACER,
            );
            assert_eq!(got, (-1, libc::EINTR, 1), "(result, errno, handler runs)");
        });
    }

    /// The same on aarch64 is refused by name. The backend does not carry the
    /// code to the kernel past the guest's filter, and the Tool hook that
    /// decides the result then (`Tool::handle_unresolved_restart`) names the
    /// syscall with a `Sysno`, which this number has none of.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn an_unknown_numbers_restart_code_under_a_guest_seccomp_filter() {
        assert_unknown_refused::<UnknownNumberRestart>(
            reverie_ptrace::RestartRefusalReason::UnknownSyscallUndecidable,
            UNKNOWN_PROBE,
            || {
                install_a_seccomp_filter_allowing_every_call();
                probe_number(
                    UNKNOWN_PROBE as libc::c_long,
                    ERESTARTNOHAND,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    SENT_BY_TRACER,
                );
            },
        );
    }

    /// On aarch64 a seccomp filter of the guest's own closes the kernel's
    /// route too, and the hook decides: its default, which answers the code
    /// itself, and an answer of `ENOIOCTLCMD`, are refused by name; an
    /// ordinary error is the guest's result.
    #[cfg(target_arch = "aarch64")]
    #[test_case(0, ERESTARTNOHAND; "the default hook")]
    #[test_case(ENOIOCTLCMD, ENOIOCTLCMD; "a hook answering enoioctlcmd")]
    fn a_restart_code_under_a_guest_seccomp_filter_answered_internally_is_refused(
        answer: i32,
        refused: i32,
    ) {
        let reason = reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
            answer: Errno::new(refused),
        };
        let guest = || {
            install_a_seccomp_filter_allowing_every_call();
            probe(
                ERESTARTNOHAND,
                Signal::SIGUSR1,
                Some(SaFlags::SA_RESTART),
                SENT_BY_TRACER,
            );
        };
        if answer == 0 {
            assert_refused::<RestartResult>(reason, guest);
        } else {
            assert_refused_with_config::<StatusUnreadableAnswering>(reason, (false, answer), guest);
        }
    }

    /// Negative, as above: a hook answering `EINTR` under a guest filter.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a_restart_code_under_a_guest_seccomp_filter_answered_with_eintr() {
        check_fn_with_config::<StatusUnreadableAnswering, _>(
            || {
                install_a_seccomp_filter_allowing_every_call();
                let got = probe(
                    ERESTARTNOHAND,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    SENT_BY_TRACER,
                );
                assert_eq!(got, (-1, libc::EINTR, 1), "(result, errno, handler runs)");
            },
            (false, libc::EINTR),
            true,
        );
    }

    /// The calling thread's seccomp mode, from its procfs status.
    fn seccomp_mode_of_this_thread() -> String {
        std::fs::read_to_string("/proc/thread-self/status")
            .expect("read the thread's status")
            .lines()
            .find_map(|line| line.strip_prefix("Seccomp:"))
            .expect("the status has Seccomp")
            .trim()
            .to_owned()
    }

    /// Runs `run` on a thread of its own under a seccomp filter that allows
    /// every call, as a tracer under an inherited filter of its host runs:
    /// the tracer's threads, started from that thread, and the guest it
    /// starts inherit the filter. The filter goes with the thread.
    fn under_a_tracer_filter(run: impl FnOnce() + Send + 'static) {
        let thread = std::thread::spawn(move || {
            install_a_seccomp_filter_allowing_every_call();
            assert_eq!(seccomp_mode_of_this_thread(), "2", "the tracer is filtered");
            run();
        });
        if let Err(panic) = thread.join() {
            std::panic::resume_unwind(panic);
        }
    }

    /// Runs `guest` in a child process with no tracer, and checks that it
    /// returns without a panic.
    fn run_natively(guest: impl FnOnce()) {
        // SAFETY: the child runs only `guest` and then exits, as the traced
        // guests of this file do.
        match unsafe { libc::fork() } {
            0 => {
                let passed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(guest)).is_ok();
                // SAFETY: _exit ends the child without running the parent's
                // exit handlers.
                unsafe { libc::_exit(if passed { 0 } else { 1 }) }
            }
            -1 => panic!("fork: {}", std::io::Error::last_os_error()),
            child => {
                let mut status = 0;
                // SAFETY: waitpid writes the one status it is passed.
                assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
                assert!(
                    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                    "the native guest failed (wait status {status:#x})"
                );
            }
        }
    }

    /// The guest's raw `rt_sigsuspend`, or `ppoll` or `pselect6` of no
    /// descriptors with a 10 s timeout, under a temporary mask that lets SIGUSR1 through while
    /// the thread blocks it, with a handler, and a sibling thread that sends
    /// SIGUSR1 to the waiting thread 200 ms in. Returns the result, its
    /// errno, the handler runs and whether SIGUSR1 is blocked again.
    fn wait_for_a_siblings_signal(call: libc::c_long) -> (i64, i32, usize, bool) {
        wait_for_a_siblings_signal_with(call, SaFlags::SA_RESTART)
    }

    /// `wait_for_a_siblings_signal`, with SIGUSR1 handled under `flags`.
    fn wait_for_a_siblings_signal_with(
        call: libc::c_long,
        flags: SaFlags,
    ) -> (i64, i32, usize, bool) {
        handle(Signal::SIGUSR1, Some(flags));
        SigSet::from(Signal::SIGUSR1)
            .thread_block()
            .expect("block the signal");
        // SAFETY: getpid and gettid take no arguments.
        let (pid, tid) = unsafe { (libc::getpid(), libc::syscall(libc::SYS_gettid)) };
        let sibling = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            // SAFETY: tgkill takes three integers and touches no memory.
            unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1) }
        });
        let lets_it_through = SigSet::empty();
        let mask = lets_it_through.as_ref() as *const libc::sigset_t;
        let timeout = libc::timespec {
            tv_sec: 10,
            tv_nsec: 0,
        };
        // The sixth argument of `pselect6`: the mask and its size.
        let mask_and_size: [usize; 2] = [mask as usize, std::mem::size_of::<u64>()];
        // SAFETY: rt_sigsuspend reads the mask; ppoll reads the timeout and
        // the mask; pselect6 reads the timeout, the mask and its size, and no
        // descriptor set.
        let ret = unsafe {
            if call == libc::SYS_rt_sigsuspend {
                libc::syscall(call, mask, std::mem::size_of::<u64>())
            } else if call == libc::SYS_pselect6 {
                libc::syscall(
                    call,
                    0,
                    std::ptr::null_mut::<libc::fd_set>(),
                    std::ptr::null_mut::<libc::fd_set>(),
                    std::ptr::null_mut::<libc::fd_set>(),
                    &timeout as *const libc::timespec,
                    mask_and_size.as_ptr(),
                )
            } else {
                libc::syscall(
                    call,
                    std::ptr::null_mut::<libc::pollfd>(),
                    0,
                    &timeout as *const libc::timespec,
                    mask,
                    std::mem::size_of::<u64>(),
                )
            }
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert_eq!(sibling.join().unwrap(), 0, "tgkill from the sibling");
        let blocked_after = SigSet::thread_get_mask()
            .expect("read the mask")
            .contains(Signal::SIGUSR1);
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
            blocked_after,
        )
    }

    /// What Linux gives `wait_for_a_siblings_signal`: `-ERESTARTNOHAND`
    /// with a handler is `-EINTR` (`handle_signal`,
    /// arch/x86/kernel/signal.c:263-281 at v7.1.3), the handler runs once,
    /// and its return puts the thread's own mask back.
    const INTERRUPTED_BY_THE_SIBLING: (i64, i32, usize, bool) = (-1, libc::EINTR, 1, true);

    /// Native control: Linux's outcome for the waits below.
    #[test_case(libc::SYS_rt_sigsuspend; "rt_sigsuspend")]
    #[test_case(libc::SYS_ppoll; "masked ppoll")]
    #[test_case(libc::SYS_pselect6; "masked pselect6")]
    fn a_wait_a_siblings_signal_interrupts_natively(call: libc::c_long) {
        run_natively(move || {
            let got = wait_for_a_siblings_signal(call);
            assert_eq!(
                got, INTERRUPTED_BY_THE_SIBLING,
                "(result, errno, handler runs, blocked again)"
            );
        });
    }

    /// A Tool that injects the guest's own interrupted `rt_sigsuspend`,
    /// masked `ppoll` or masked `pselect6` unchanged gets `-ERESTARTNOHAND` at the call's exit
    /// stop, with the temporary mask's restore pending and the sibling's
    /// signal pending, which that mask lets through, and returns it. Under a
    /// tracer with no filter of its own, x86-64's kernel resolves the code:
    /// `-EINTR`, as natively.
    #[cfg(target_arch = "x86_64")]
    #[test_case(libc::SYS_rt_sigsuspend; "rt_sigsuspend")]
    #[test_case(libc::SYS_ppoll; "masked ppoll")]
    #[test_case(libc::SYS_pselect6; "masked pselect6")]
    fn the_guests_own_interrupted_wait_returned_by_a_tool_is_resolved_by_the_kernel(
        call: libc::c_long,
    ) {
        assert_eq!(
            seccomp_mode_of_this_thread(),
            "0",
            "the tracer is not filtered"
        );
        check_fn::<InjectsTheGuestsOwnCall, _>(move || {
            let got = wait_for_a_siblings_signal(call);
            assert_eq!(
                got, INTERRUPTED_BY_THE_SIBLING,
                "(result, errno, handler runs, blocked again)"
            );
        });
    }

    /// The same under a tracer that a seccomp filter of its host may
    /// restrict, which an inherited filter gives the tracer and the guest
    /// alike. The kernel runs `do_signal` for the pending signal, which
    /// resolves the code, so it is left to the kernel, as without the filter:
    /// the unresolved-restart hook (`Tool::handle_unresolved_restart`), whose
    /// default ends the run, is not asked.
    #[cfg(target_arch = "x86_64")]
    #[test_case(libc::SYS_rt_sigsuspend; "rt_sigsuspend")]
    #[test_case(libc::SYS_ppoll; "masked ppoll")]
    #[test_case(libc::SYS_pselect6; "masked pselect6")]
    fn the_guests_own_interrupted_wait_under_a_tracer_filter_is_resolved_by_the_kernel(
        call: libc::c_long,
    ) {
        under_a_tracer_filter(move || {
            check_fn::<InjectsTheGuestsOwnCall, _>(move || {
                let got = wait_for_a_siblings_signal(call);
                assert_eq!(
                    got, INTERRUPTED_BY_THE_SIBLING,
                    "(result, errno, handler runs, blocked again)"
                );
            });
        });
    }

    /// The Tool's answer must be the kernel's own result for the call: a Tool
    /// that answers the interrupted `rt_sigsuspend`'s `-ERESTARTNOHAND` with
    /// `-ERESTARTSYS` under a tracer filter is not left to the kernel, and
    /// the default unresolved-restart hook's answer, that code, is refused by
    /// name. (SIGUSR1's handler has no `SA_RESTART`, so the kernel would give
    /// `-EINTR`.)
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn another_restart_code_than_the_kernels_under_a_tracer_filter_is_refused() {
        under_a_tracer_filter(|| {
            assert_refused::<AnswersAnotherRestartCode>(
                reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                    answer: Errno::ERESTARTSYS,
                },
                || {
                    wait_for_a_siblings_signal_with(libc::SYS_rt_sigsuspend, SaFlags::empty());
                },
            );
        });
    }

    /// The probe of `SynthesizesARestartCode`, with SIGUSR1 handled: its
    /// result, errno and handler runs. With `sends`, a sibling thread waits
    /// for SIGUSR1 in `rt_sigtimedwait` while this thread blocks it, so the
    /// signal the Tool sends to the process can only be the sibling's, and
    /// the guest checks that the sibling took it.
    #[cfg(target_arch = "x86_64")]
    fn probe_after_a_completed_call(sends: bool) -> (i64, i32, usize) {
        handle(Signal::SIGUSR1, Some(SaFlags::SA_RESTART));
        let taker = sends.then(|| {
            SigSet::from(Signal::SIGUSR1)
                .thread_block()
                .expect("block the signal in this thread");
            let waiting = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let taker = {
                let waiting = waiting.clone();
                std::thread::spawn(move || {
                    let set = SigSet::from(Signal::SIGUSR1);
                    let timeout = libc::timespec {
                        tv_sec: 10,
                        tv_nsec: 0,
                    };
                    waiting.store(true, Ordering::SeqCst);
                    // SAFETY: rt_sigtimedwait reads the set and the timeout.
                    unsafe {
                        libc::syscall(
                            libc::SYS_rt_sigtimedwait,
                            set.as_ref() as *const libc::sigset_t,
                            std::ptr::null_mut::<libc::siginfo_t>(),
                            &timeout as *const libc::timespec,
                            std::mem::size_of::<u64>(),
                        )
                    }
                })
            };
            while !waiting.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            taker
        });
        // SAFETY: the probe syscall takes integers and touches no memory.
        let ret = unsafe { libc::syscall(libc::SYS_getppid, libc::c_long::from(sends)) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if let Some(taker) = taker {
            assert_eq!(
                taker.join().unwrap(),
                libc::SIGUSR1 as libc::c_long,
                "the sibling takes the signal"
            );
        }
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
        )
    }

    /// A restart code a Tool makes up after the guest's own call completed
    /// normally in place is not the kernel's: nothing set this thread's
    /// `TIF_SIGPENDING`, with no signal sent or with the signal sent to the
    /// process taken by a sibling, and a code left to the kernel would reach
    /// the guest raw. Without a tracer filter the backend's carrier runs the
    /// kernel's restart processing from the call's exit stop: the probe
    /// restarts and gets `RESTARTED`.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no signal")]
    #[test_case(true; "the signal taken by a sibling")]
    fn a_made_up_restart_code_after_a_completed_call_restarts(sends: bool) {
        assert_eq!(
            seccomp_mode_of_this_thread(),
            "0",
            "the tracer is not filtered"
        );
        check_fn::<SynthesizesARestartCode, _>(move || {
            let got = probe_after_a_completed_call(sends);
            assert_eq!(got, (RESTARTED, 0, 0), "(result, errno, handler runs)");
        });
    }

    /// The same under a tracer filter, where the carrier cannot run: the
    /// default unresolved-restart hook's answer, the code itself, is refused
    /// by name.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no signal")]
    #[test_case(true; "the signal taken by a sibling")]
    fn a_made_up_restart_code_after_a_completed_call_under_a_tracer_filter_is_refused(sends: bool) {
        under_a_tracer_filter(move || {
            assert_refused::<SynthesizesARestartCode>(
                reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                    answer: Errno::ERESTARTNOHAND,
                },
                move || {
                    probe_after_a_completed_call(sends);
                },
            );
        });
    }

    /// Runs `run` under a tracer filter when `filtered`, and otherwise checks
    /// that the tracer is not filtered and runs it.
    fn with_or_without_a_tracer_filter(filtered: bool, run: impl FnOnce() + Send + 'static) {
        if filtered {
            under_a_tracer_filter(run);
        } else {
            assert_eq!(
                seccomp_mode_of_this_thread(),
                "0",
                "the tracer is not filtered"
            );
            run();
        }
    }

    /// The guest's masked `pselect6`, run by the Tool as a call of its own
    /// (`RunsTheGuestsPselect6WithAnUnreadSet`) between syscall stops, so the
    /// guest's call was skipped, which leaves no syscall number for Linux's
    /// restart processing (`orig_ax` -1 on x86-64, `syscallno` NO_SYSCALL on
    /// aarch64). The sibling's signal interrupts the Tool's call with the
    /// temporary mask's restore pending, and the Tool returns
    /// `-ERESTARTNOHAND`. The backend leaves the code to the kernel with the
    /// guest's syscall number put back, so Linux converts it: `-EINTR`, as
    /// natively, on both architectures.
    #[test]
    fn a_skipped_waits_restart_code_left_to_the_kernel_is_converted() {
        check_fn_with_config::<RunsTheGuestsPselect6WithAnUnreadSet, _>(
            || {
                let got = wait_for_a_siblings_signal(libc::SYS_pselect6);
                assert_eq!(
                    got, INTERRUPTED_BY_THE_SIBLING,
                    "(result, errno, handler runs, blocked again)"
                );
            },
            true,
            true,
        );
    }

    /// A Tool that sets the interrupted `rt_sigsuspend`'s `orig_rax` to -1
    /// before it returns the kernel's `-ERESTARTNOHAND`: Linux would read no
    /// syscall and leave the code to the guest, so the code is refused by
    /// name (`NoKernelSyscallNumber`) before any route, under a tracer filter
    /// or none, and the Tool's write is not undone.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn the_interrupted_waits_code_after_orig_rax_set_to_minus_one_is_refused(filtered: bool) {
        with_or_without_a_tracer_filter(filtered, || {
            assert_refused_with_config::<EditsTheInterruptedWait>(
                reverie_ptrace::RestartRefusalReason::NoKernelSyscallNumber,
                RegisterEdit::NoSyscallNumber,
                || {
                    wait_for_a_siblings_signal(libc::SYS_rt_sigsuspend);
                },
            );
        });
    }

    /// Set by `on_sigusr2`.
    static SIGUSR2_HANDLED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn on_sigusr2(_: libc::c_int) {
        SIGUSR2_HANDLED.fetch_add(1, Ordering::SeqCst);
    }

    /// The guest's own `rt_sigsuspend`, interrupted by a sibling's SIGUSR1
    /// with a handler, returns `EINTR`; then, with no syscall between, the
    /// probing thread spins in user mode with `-ERESTARTNOHAND` in `rax`
    /// until a SIGUSR2 the sibling then sends has run its handler. That
    /// signal's delivery stop interrupts user mode, where `orig_rax` reads -1
    /// (an interrupt's entry, arch/x86/entry/entry_64.S at v7.1.3). Returns
    /// the wait's result, errno and SIGUSR1 handler runs, `rax` after the
    /// spin, and the SIGUSR2 handler runs.
    #[cfg(target_arch = "x86_64")]
    fn a_wait_then_a_user_mode_signal_with_the_code_in_rax() -> (i64, i32, usize, i64, usize) {
        use std::sync::atomic::AtomicU32;
        static READY: AtomicU32 = AtomicU32::new(0);
        handle(Signal::SIGUSR1, Some(SaFlags::SA_RESTART));
        let action = SigAction::new(
            SigHandler::Handler(on_sigusr2),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: the handler only increments an atomic.
        unsafe { sigaction(Signal::SIGUSR2, &action) }.expect("install the SIGUSR2 handler");
        SigSet::from(Signal::SIGUSR1)
            .thread_block()
            .expect("block the signal");
        // SAFETY: getpid and gettid take no arguments.
        let (pid, tid) = unsafe { (libc::getpid(), libc::syscall(libc::SYS_gettid)) };
        let sibling = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            // SAFETY: tgkill takes three integers and touches no memory.
            unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1) };
            while READY.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            // SAFETY: tgkill takes three integers and touches no memory.
            unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR2) };
        });
        let lets_it_through = SigSet::empty();
        // SAFETY: rt_sigsuspend reads the mask.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_rt_sigsuspend,
                lets_it_through.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let handled = HANDLED.load(Ordering::SeqCst);
        READY.store(1, Ordering::SeqCst);
        let rax: i64;
        // SAFETY: the loop only reads the counter, which the SIGUSR2 handler
        // sets; it touches no stack, and a handler's frame skips the red zone.
        unsafe {
            std::arch::asm!(
                "2:",
                "cmp qword ptr [{done}], 0",
                "je 2b",
                done = in(reg) SIGUSR2_HANDLED.as_ptr(),
                inout("rax") -(ERESTARTNOHAND as i64) => rax,
                options(nostack),
            );
        }
        sibling.join().expect("the sibling");
        (
            ret,
            errno,
            handled,
            rax,
            SIGUSR2_HANDLED.load(Ordering::SeqCst),
        )
    }

    /// The guest's own interrupted `rt_sigsuspend` has its `-ERESTARTNOHAND`
    /// left to the kernel, which converts it to `EINTR` with the handler;
    /// the thread has left the call. A later signal-delivery stop in user
    /// mode, where `orig_rax` is -1 and `rax` happens to hold the same code,
    /// is not that call's exit, and the guest runs on as natively, under a
    /// tracer filter or none.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn a_later_user_mode_signal_after_the_code_was_converted_runs_natively(filtered: bool) {
        with_or_without_a_tracer_filter(filtered, || {
            check_fn::<InjectsTheGuestsOwnCall, _>(|| {
                let got = a_wait_then_a_user_mode_signal_with_the_code_in_rax();
                assert_eq!(
                    got,
                    (-1, libc::EINTR, 1, -(ERESTARTNOHAND as i64), 1),
                    "(result, errno, SIGUSR1 runs, rax after the spin, SIGUSR2 runs)"
                );
            });
        });
    }

    /// Native control of the above.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_wait_then_a_user_mode_signal_with_the_code_in_rax_natively() {
        run_natively(|| {
            let got = a_wait_then_a_user_mode_signal_with_the_code_in_rax();
            assert_eq!(got, (-1, libc::EINTR, 1, -(ERESTARTNOHAND as i64), 1));
        });
    }

    /// A Tool completes the guest's own probe and answers
    /// `-ERESTART_RESTARTBLOCK` of its own, under a tracer filter, so the
    /// backend asks its unresolved-restart hook after it checked the code;
    /// while it awaits the hook, a sibling thread installs a seccomp filter
    /// on every thread (`HookAwaitsASiblingsFilter`), and the hook answers
    /// `EINTR`. The guest's filters are read again before the resume, after
    /// the route's last await, and the code is refused by name, on both
    /// architectures.
    #[test]
    fn a_guest_filter_installed_while_a_516_is_routed_is_refused_by_name() {
        under_a_tracer_filter(|| {
            assert_refused::<HookAwaitsASiblingsFilter>(
                reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
                || {
                    let sibling = std::thread::spawn(|| {
                        while SIBLING_GO.load(Ordering::SeqCst) == 0 {
                            std::thread::yield_now();
                        }
                        install_a_filter_on_every_thread(
                            libc::SYS_getppid,
                            libc::SECCOMP_RET_ALLOW,
                            0,
                        );
                        SIBLING_DONE.store(1, Ordering::SeqCst);
                    });
                    // SAFETY: getppid takes no arguments.
                    let ret = unsafe { libc::syscall(libc::SYS_getppid) };
                    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    sibling.join().expect("the sibling");
                    panic!("the guest got ({ret}, {errno})");
                },
            );
        });
    }

    /// The test runner's own `Seccomp_filters` plus one for the tracer's
    /// launch filter, which every traced guest carries: the filters a guest
    /// has with no inherited filter of the host's (the runner-side baseline).
    fn runner_side_baseline() -> u64 {
        std::fs::read_to_string("/proc/thread-self/status")
            .expect("read the runner's status")
            .lines()
            .find_map(|line| line.strip_prefix("Seccomp_filters:"))
            .expect("the status has Seccomp_filters")
            .trim()
            .parse::<u64>()
            .expect("a number")
            + 1
    }

    /// The inherited-filter row under raw Reverie: a seccomp filter answering
    /// `rt_sigsuspend` with errno 514 is installed on the test's thread
    /// before the tracer starts, so the tracer and the guest inherit it, as a
    /// guest inherits its host's filter. The guest's true status, read from
    /// the tracer side (an embedding tool may blank the guest's own view by
    /// policy), shows the filter: at the guest's `openat` of its own status,
    /// a tracer stop, the Tool reads `/proc/<tid>/status`
    /// (`ReadsTheTrueStatusAtItsOpen`), which must show `Seccomp: 2` and at
    /// least one filter more than the runner-side baseline
    /// (`runner_side_baseline`); both numbers are printed. Then the wait returns errno 514 at once, which Linux gives
    /// at the call's entry, as natively. Both architectures.
    #[test_case(true; "under the inherited filter")]
    fn the_guests_true_status_shows_an_inherited_filter_at_its_status_open(inherited: bool) {
        let baseline = runner_side_baseline();
        // The guest's report of what it read, printed by the test.
        let report =
            std::env::temp_dir().join(format!("restart-result-true-status-{}", std::process::id()));
        let guest_report = report.clone();
        let thread =
            std::thread::spawn(move || {
                if inherited {
                    install_a_filter(
                        libc::SYS_rt_sigsuspend,
                        libc::SECCOMP_RET_ERRNO | ERESTARTNOHAND as u32,
                        0,
                    );
                }
                check_fn::<ReadsTheTrueStatusAtItsOpen, _>(move || {
                    // SAFETY: openat reads the path, which outlives the call;
                    // close takes the descriptor it returned.
                    let fd = unsafe {
                        libc::syscall(
                            libc::SYS_openat,
                            libc::AT_FDCWD,
                            TRUE_STATUS_PATH.as_ptr(),
                            libc::O_RDONLY,
                        )
                    };
                    assert!(fd >= 0, "open the guest's own status");
                    // SAFETY: close takes the descriptor openat returned.
                    unsafe { libc::close(fd as i32) };
                    let (mode, filters) = (
                        TRUE_STATUS[0].load(Ordering::SeqCst),
                        TRUE_STATUS[1].load(Ordering::SeqCst),
                    );
                    std::fs::write(&guest_report, format!(
                    "A1 raw Reverie: the guest's true status, read from the tracer side at its \
                     status open: Seccomp: {mode}; runner Seccomp_filters {runner}; baseline0 \
                     {baseline}; guest Seccomp_filters {filters} (needs ≥ baseline0+1)",
                    runner = baseline - 1,
                ))
                .expect("write the guest's report");
                    assert_eq!(mode, 2, "the guest's true seccomp mode");
                    assert!(
                        filters > baseline,
                        "guest Seccomp_filters {filters}, not ≥ baseline0+1 (runner Seccomp_filters \
                     {runner}; baseline0 {baseline}): no inherited filter",
                        runner = baseline - 1,
                    );
                    let lets_it_through = SigSet::empty();
                    // SAFETY: alarm takes an integer; rt_sigsuspend reads the mask.
                    let ret = unsafe {
                        libc::alarm(10);
                        libc::syscall(
                            libc::SYS_rt_sigsuspend,
                            lets_it_through.as_ref() as *const libc::sigset_t,
                            std::mem::size_of::<u64>(),
                        )
                    };
                    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    // SAFETY: alarm takes an integer.
                    unsafe { libc::alarm(0) };
                    assert_eq!((ret, errno), (-1, ERESTARTNOHAND), "(result, errno)");
                });
            });
        let joined = thread.join();
        if let Ok(line) = std::fs::read_to_string(&report) {
            println!("{line}");
            let _ = std::fs::remove_file(&report);
        }
        if let Err(panic) = joined {
            std::panic::resume_unwind(panic);
        }
    }

    /// The guest's own `rt_sigsuspend`, with SIGUSR1 and SIGUSR2 both pending
    /// and blocked until the call's empty mask lets them through, so the call
    /// returns `-ERESTARTNOHAND` at once and `get_signal` takes SIGUSR1, then
    /// SIGUSR2, before Linux converts the code. Returns the result, its errno
    /// and the SIGUSR2 handler runs.
    #[cfg(target_arch = "x86_64")]
    fn a_wait_with_two_signals_pending() -> (i64, i32, usize) {
        let action = SigAction::new(
            SigHandler::Handler(on_sigusr2),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: the handler only increments an atomic.
        unsafe { sigaction(Signal::SIGUSR2, &action) }.expect("install the SIGUSR2 handler");
        let mut both = SigSet::empty();
        both.add(Signal::SIGUSR1);
        both.add(Signal::SIGUSR2);
        both.thread_block().expect("block both signals");
        // SAFETY: getpid and gettid take no arguments; tgkill takes three
        // integers and touches no memory.
        unsafe {
            let (pid, tid) = (libc::getpid(), libc::syscall(libc::SYS_gettid));
            libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1);
            libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR2);
        }
        let lets_them_through = SigSet::empty();
        // SAFETY: rt_sigsuspend reads the mask.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_rt_sigsuspend,
                lets_them_through.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        (ret, errno, SIGUSR2_HANDLED.load(Ordering::SeqCst))
    }

    /// A Tool's register write at one signal-delivery stop does not hide the
    /// code from the next: the guest's own interrupted `rt_sigsuspend` has its
    /// `-ERESTARTNOHAND` left to the kernel; at SIGUSR1's delivery stop the
    /// Tool moves `rip` and suppresses the signal, and at SIGUSR2's, still
    /// inside the same `get_signal`, it puts `rip` back and sets `orig_rax` to
    /// -1. That stop's own registers, read as it begins, hold the code with
    /// the call's number, so the resume is refused by name.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_register_write_at_one_delivery_stop_does_not_hide_the_code_from_the_next() {
        assert_refused_with_config::<EditsTheInterruptedWait>(
            reverie_ptrace::RestartRefusalReason::NoKernelSyscallNumber,
            RegisterEdit::IpEditedThenNoSyscallNumberAtTheNextStop,
            || {
                let got = a_wait_with_two_signals_pending();
                panic!("the guest got {got:?}");
            },
        );
    }

    /// A Tool's `-ERESTART_RESTARTBLOCK`, with no filter of the guest's own
    /// when it is checked, goes through the backend's carrier
    /// (`INJECTED_PPOLL_THEN_SIGNAL_PENDING`); after the carrier's stop,
    /// before Linux converts the code, SIGUSR1's delivery stop reports the
    /// signal, and while the Tool's report awaits, a sibling thread installs a
    /// seccomp filter on every thread (`SignalStopAwaitsASiblingsFilter`). The
    /// answer lives on to that stop, its filters are read again at its
    /// resume, and the code is refused by name, on both architectures.
    #[test]
    fn a_guest_filter_installed_after_the_carrier_is_refused_by_name() {
        assert_refused_with_config::<SignalStopAwaitsASiblingsFilter>(
            reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
            true,
            || {
                handle(Signal::SIGUSR1, Some(SaFlags::SA_RESTART));
                let sibling = std::thread::spawn(|| {
                    SigSet::from(Signal::SIGUSR1)
                        .thread_block()
                        .expect("block the signal in the sibling");
                    while SIBLING_GO.load(Ordering::SeqCst) == 0 {
                        std::thread::yield_now();
                    }
                    install_a_filter_on_every_thread(libc::SYS_getppid, libc::SECCOMP_RET_ALLOW, 0);
                    SIBLING_DONE.store(1, Ordering::SeqCst);
                });
                let got = probe_after_an_injected_ppoll_with(
                    libc::SYS_getppid,
                    ERESTART_RESTARTBLOCK,
                    INJECTED_PPOLL_THEN_SIGNAL_PENDING,
                );
                sibling.join().expect("the sibling");
                panic!("the guest got {got:?}");
            },
        );
    }

    /// A sibling thread of the guest that waits for `SIBLING_GO` and then
    /// installs a seccomp filter allowing every call on every thread
    /// (`SECCOMP_FILTER_FLAG_TSYNC`) and sets `SIBLING_DONE`.
    fn a_sibling_that_installs_a_filter() -> std::thread::JoinHandle<()> {
        std::thread::spawn(|| {
            SigSet::from(Signal::SIGUSR1)
                .thread_block()
                .expect("block the signal in the sibling");
            while SIBLING_GO.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            install_a_filter_on_every_thread(libc::SYS_getppid, libc::SECCOMP_RET_ALLOW, 0);
            SIBLING_DONE.store(1, Ordering::SeqCst);
        })
    }

    /// A Tool's `-ERESTART_RESTARTBLOCK`, with no filter of the guest's own
    /// when it is checked, left to the kernel from the call's exit stop with
    /// the temporary mask's restore pending
    /// (`ADeferredRestartBlockAwaitsASiblingsFilter`). On aarch64 Linux's
    /// `do_signal` rewinds the registers before the interrupting signal's
    /// delivery stop and converts the code only after it; on x86-64 it
    /// converts after it too. While that stop is reported a sibling installs
    /// a filter: the answer lives on to the stop, its filters are read again
    /// at its resume, and the code is refused by name, on both architectures.
    #[test]
    fn a_guest_filter_installed_after_a_deferred_516_is_refused_by_name() {
        assert_refused_with_config::<ADeferredRestartBlockAwaitsASiblingsFilter>(
            reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
            true,
            || {
                let _sibling = a_sibling_that_installs_a_filter();
                let got = wait_for_a_siblings_signal(libc::SYS_pselect6);
                panic!("the guest got {got:?}");
            },
        );
    }

    /// A Tool's `-ERESTART_RESTARTBLOCK` on its way to the kernel through the
    /// carrier, and while a signal-delivery stop after the carrier's is
    /// reported, a sibling thread exits the whole guest
    /// (`SignalStopAwaitsTheGuestsExit`). The stopped thread is then exiting:
    /// its procfs status is gone or shows a zombie, which is no seccomp filter
    /// of the guest's own and no unreadable one, so nothing is refused and
    /// the guest exits with the sibling's status, as natively, on both
    /// architectures.
    #[test]
    fn a_guest_that_exits_while_a_516_is_pending_exits() {
        let run = reverie_ptrace::testing::test_fn_with_config::<SignalStopAwaitsTheGuestsExit, _>(
            || {
                handle(Signal::SIGUSR1, Some(SaFlags::SA_RESTART));
                let _sibling = std::thread::spawn(|| {
                    SigSet::from(Signal::SIGUSR1)
                        .thread_block()
                        .expect("block the signal in the sibling");
                    while SIBLING_GO.load(Ordering::SeqCst) == 0 {
                        std::thread::yield_now();
                    }
                    // SAFETY: exit_group takes one integer and does not return.
                    unsafe { libc::syscall(libc::SYS_exit_group, 42) };
                });
                let got = probe_after_an_injected_ppoll_with(
                    libc::SYS_getppid,
                    ERESTART_RESTARTBLOCK,
                    INJECTED_PPOLL_THEN_SIGNAL_PENDING,
                );
                panic!("the guest got {got:?}");
            },
            true,
            true,
        );
        let (output, _) =
            run.unwrap_or_else(|error| panic!("expected the guest's exit, got {error:?}"));
        assert_eq!(output.status, reverie::ExitStatus::Exited(42));
    }

    /// A Tool's `-ERESTART_RESTARTBLOCK` on its way to the kernel through the
    /// carrier, and while a signal-delivery stop after the carrier's is
    /// reported, the tracer can no longer open a descriptor
    /// (`SignalStopClosesTheTracersFiles`), so the live guest's seccomp count
    /// cannot be read at the resume: refused by name, on both architectures.
    #[test]
    fn an_unreadable_seccomp_count_at_a_516_resume_is_refused_by_name() {
        let _limit = OpenFileLimit::kept();
        assert_refused_with_config::<SignalStopClosesTheTracersFiles>(
            reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
            true,
            || {
                handle(Signal::SIGUSR1, Some(SaFlags::SA_RESTART));
                let got = probe_after_an_injected_ppoll_with(
                    libc::SYS_getppid,
                    ERESTART_RESTARTBLOCK,
                    INJECTED_PPOLL_THEN_SIGNAL_PENDING,
                );
                panic!("the guest got {got:?}");
            },
        );
    }

    /// A Tool that leaves the interrupted `rt_sigsuspend`'s `-ERESTARTNOHAND`
    /// to the kernel and then sets `orig_rax` to -1 at the signal-delivery
    /// stop of the sibling's signal, which Linux makes inside `get_signal`
    /// before it converts the code (arch/x86/kernel/signal.c:337 at v7.1.3):
    /// Linux would save the raw code in the handler's frame and the guest
    /// would get it after the handler, so the resume from that stop refuses
    /// it by name, under a tracer filter or none.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn the_interrupted_waits_code_after_orig_rax_set_to_minus_one_at_the_delivery_stop_is_refused(
        filtered: bool,
    ) {
        with_or_without_a_tracer_filter(filtered, || {
            assert_refused_with_config::<EditsTheInterruptedWait>(
                reverie_ptrace::RestartRefusalReason::NoKernelSyscallNumber,
                RegisterEdit::NoSyscallNumberAtTheDeliveryStop,
                || {
                    let got = wait_for_a_siblings_signal(libc::SYS_rt_sigsuspend);
                    assert_eq!(got.1, libc::EINTR, "the guest got {got:?}");
                },
            );
        });
    }

    /// A Tool that sets `rcx` and `r11`, which the `syscall` instruction
    /// clobbers, before it returns the kernel's code: the call's control
    /// context is the one the kernel left, so the code is left to the kernel,
    /// as without the write: `-EINTR`, under a tracer filter or none.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn the_interrupted_waits_code_after_the_clobbered_registers_are_set_is_resolved_by_the_kernel(
        filtered: bool,
    ) {
        with_or_without_a_tracer_filter(filtered, || {
            check_fn_with_config::<EditsTheInterruptedWait, _>(
                || {
                    let got = wait_for_a_siblings_signal(libc::SYS_rt_sigsuspend);
                    assert_eq!(
                        got, INTERRUPTED_BY_THE_SIBLING,
                        "(result, errno, handler runs, blocked again)"
                    );
                },
                RegisterEdit::ClobberedRegisters,
                true,
            );
        });
    }

    /// A Tool that sets `orig_rax` to another syscall's number before it
    /// returns the kernel's code changes the call's control context, so the
    /// code is no longer the kernel's own: without a tracer filter the
    /// pending signal's `do_signal` resolves it (`-EINTR`, the handler run);
    /// under one the default unresolved-restart hook's answer is refused by
    /// name.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_interrupted_waits_code_after_orig_rax_changed_is_resolved_by_the_kernel() {
        assert_eq!(
            seccomp_mode_of_this_thread(),
            "0",
            "the tracer is not filtered"
        );
        check_fn_with_config::<EditsTheInterruptedWait, _>(
            || {
                let got = wait_for_a_siblings_signal(libc::SYS_rt_sigsuspend);
                assert_eq!(
                    got, INTERRUPTED_BY_THE_SIBLING,
                    "(result, errno, handler runs, blocked again)"
                );
            },
            RegisterEdit::AnotherSyscallNumber,
            true,
        );
    }

    /// The same under a tracer filter (see above).
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_interrupted_waits_code_after_orig_rax_changed_under_a_tracer_filter_is_refused() {
        under_a_tracer_filter(|| {
            assert_refused_with_config::<EditsTheInterruptedWait>(
                reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                    answer: Errno::ERESTARTNOHAND,
                },
                RegisterEdit::AnotherSyscallNumber,
                || {
                    wait_for_a_siblings_signal(libc::SYS_rt_sigsuspend);
                },
            );
        });
    }

    /// Installs on every thread of the process (`SECCOMP_FILTER_FLAG_TSYNC`,
    /// plus `flags`) a seccomp filter answering `call` with `action` and
    /// allowing every other call; returns what `seccomp` returned (a listener
    /// descriptor with `SECCOMP_FILTER_FLAG_NEW_LISTENER`).
    fn install_a_filter_on_every_thread(
        call: libc::c_long,
        action: u32,
        flags: libc::c_ulong,
    ) -> i64 {
        install_a_filter(call, action, libc::SECCOMP_FILTER_FLAG_TSYNC | flags)
    }

    /// Installs, with `flags`, a seccomp filter answering `call` with
    /// `action` and allowing every other call; returns what `seccomp`
    /// returned.
    fn install_a_filter(call: libc::c_long, action: u32, flags: libc::c_ulong) -> i64 {
        let program = [
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 0, // offsetof(seccomp_data, nr)
            },
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: call as u32,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: action,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
        ];
        let fprog = libc::sock_fprog {
            len: program.len() as u16,
            filter: program.as_ptr() as *mut libc::sock_filter,
        };
        // SAFETY: prctl takes integers here and touches no memory.
        let no_new_privs = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        assert_eq!(no_new_privs, 0, "set no_new_privs");
        // SAFETY: seccomp reads the program, which outlives the call.
        let installed = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                flags,
                &fprog as *const libc::sock_fprog,
            )
        };
        assert!(
            installed >= 0,
            "install the filter: {}",
            std::io::Error::last_os_error()
        );
        installed
    }

    /// A guest whose sibling thread installs, on every thread, a seccomp
    /// filter answering `getppid` with `SECCOMP_RET_ERRNO` `errno` once a
    /// flag lets it go, and whose probing thread then makes the probe;
    /// returns the probe's result and errno. Traced, the Tool sets the flag,
    /// waits for the filter while the probing thread is stopped in the call,
    /// and writes the thread's `Seccomp_filters` before and after
    /// (`run_after_a_siblings_filter`), which the guest checks rose by one
    /// during the stop; with `first` the probing thread sets the flag and
    /// waits for the filter before the call, which the filter then answers at
    /// its entry. Linux returns the filter's errno itself, with no signal
    /// pending.
    fn getppid_after_a_siblings_errno_filter(first: bool, errno: i32) -> (i64, i32) {
        use std::sync::atomic::AtomicU32;
        use std::sync::atomic::AtomicU64;
        static GO: AtomicU32 = AtomicU32::new(0);
        static DONE: AtomicU32 = AtomicU32::new(0);
        static FILTERS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
        let sibling = std::thread::spawn(move || {
            while GO.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            install_a_filter_on_every_thread(
                libc::SYS_getppid,
                libc::SECCOMP_RET_ERRNO | errno as u32,
                0,
            );
            DONE.store(1, Ordering::SeqCst);
        });
        if first {
            GO.store(1, Ordering::SeqCst);
            while DONE.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
        }
        // SAFETY: the probe takes three addresses the Tool reads and writes,
        // which live for the whole program; Linux ignores them.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_getppid,
                GO.as_ptr() as libc::c_long,
                DONE.as_ptr() as libc::c_long,
                FILTERS.as_ptr() as libc::c_long,
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        sibling.join().expect("the sibling");
        if !first {
            let (before, after) = (
                FILTERS[0].load(Ordering::SeqCst),
                FILTERS[1].load(Ordering::SeqCst),
            );
            assert_eq!(after, before + 1, "Seccomp_filters rose during the stop");
        }
        (ret, if ret == -1 { errno } else { 0 })
    }

    /// The guest's raw `rt_sigsuspend` with an empty mask, after a sibling
    /// thread installs, on every thread, a filter answering `rt_sigsuspend`
    /// with `SECCOMP_RET_ERRNO` `errno`: traced, while the call is stopped at
    /// its seccomp stop (`SIBLING_GO`, `SIBLING_DONE`; the guest checks that
    /// `Seccomp_filters` rose by one during the stop, `SIBLING_FILTERS`);
    /// with `first`, before the call. A 10 s alarm with a handler is the
    /// watchdog: a wait the filter did not answer would end with `EINTR`.
    /// Returns the result and its errno.
    fn rt_sigsuspend_after_a_siblings_errno_filter(first: bool, errno: i32) -> (i64, i32) {
        handle(Signal::SIGALRM, Some(SaFlags::empty()));
        let sibling = std::thread::spawn(move || {
            SigSet::from(Signal::SIGALRM)
                .thread_block()
                .expect("block the watchdog in the sibling");
            while SIBLING_GO.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            install_a_filter_on_every_thread(
                libc::SYS_rt_sigsuspend,
                libc::SECCOMP_RET_ERRNO | errno as u32,
                0,
            );
            SIBLING_DONE.store(1, Ordering::SeqCst);
        });
        if first {
            SIBLING_GO.store(1, Ordering::SeqCst);
            while SIBLING_DONE.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
        }
        let mask = SigSet::empty();
        // SAFETY: alarm takes an integer; rt_sigsuspend reads the mask.
        let ret = unsafe {
            libc::alarm(10);
            libc::syscall(
                libc::SYS_rt_sigsuspend,
                mask.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        // SAFETY: alarm takes an integer.
        unsafe { libc::alarm(0) };
        sibling.join().expect("the sibling");
        if !first {
            let (before, after) = (
                SIBLING_FILTERS[0].load(Ordering::SeqCst),
                SIBLING_FILTERS[1].load(Ordering::SeqCst),
            );
            assert_eq!(after, before + 1, "Seccomp_filters rose during the stop");
        }
        (ret, if ret == -1 { errno } else { 0 })
    }

    /// Native control: with no tracer, the guest gets the filter's errno as
    /// Linux returns it, for `getppid` and for `rt_sigsuspend`, which returns
    /// at once.
    #[test_case(ERESTARTSYS; "errno 512")]
    #[test_case(ERESTARTNOINTR; "errno 513")]
    #[test_case(ERESTARTNOHAND; "errno 514")]
    #[test_case(ERESTART_RESTARTBLOCK; "errno 516")]
    fn a_siblings_errno_filter_answers_natively(errno: i32) {
        run_natively(move || {
            assert_eq!(
                getppid_after_a_siblings_errno_filter(true, errno),
                (-1, errno)
            );
            assert_eq!(
                rt_sigsuspend_after_a_siblings_errno_filter(true, errno),
                (-1, errno)
            );
        });
    }

    /// A filter present at the call's entry answers it with
    /// `SECCOMP_RET_ERRNO`, which wins over the backend's
    /// `SECCOMP_RET_TRACE` (kernel/seccomp.c:426 at v7.1.3): the call never
    /// reaches the Tool, and the guest gets the filter's errno from Linux,
    /// as natively, under a tracer filter or none. Linux alone decides this
    /// result; it is no evidence of a backend route.
    #[test_case(false, ERESTARTNOHAND; "no tracer filter, errno 514")]
    #[test_case(false, ERESTART_RESTARTBLOCK; "no tracer filter, errno 516")]
    #[test_case(true, ERESTARTNOHAND; "under a tracer filter, errno 514")]
    #[test_case(true, ERESTART_RESTARTBLOCK; "under a tracer filter, errno 516")]
    fn an_errno_filter_present_at_the_calls_entry_answers_it_before_the_tracer(
        filtered: bool,
        errno: i32,
    ) {
        with_or_without_a_tracer_filter(filtered, move || {
            check_fn::<RunsTheCallAfterASiblingsFilter, _>(move || {
                let got = getppid_after_a_siblings_errno_filter(true, errno);
                assert_eq!(got, (-1, errno), "(result, errno)");
            });
        });
    }

    /// The carrier rows: the filter installed while `getppid` is stopped at
    /// its seccomp stop, with no tracer filter, on x86-64. The backend runs
    /// the call in place, Linux's second run of the filters answers it with
    /// the errno, and the Tool returns that result. `getppid` is not one of
    /// the three waits whose native result is left to the kernel, so the
    /// carrier restarts the call (512 and 514 restart with no handler, 513
    /// always), and the filter, now present at its entry, answers it there:
    /// the errno, as natively, with nothing raw from the backend.
    #[cfg(target_arch = "x86_64")]
    #[test_case(ERESTARTSYS; "errno 512")]
    #[test_case(ERESTARTNOINTR; "errno 513")]
    #[test_case(ERESTARTNOHAND; "errno 514")]
    fn a_siblings_errno_filter_installed_while_the_call_is_stopped_answers_its_restart(errno: i32) {
        with_or_without_a_tracer_filter(false, move || {
            check_fn::<RunsTheCallAfterASiblingsFilter, _>(move || {
                let got = getppid_after_a_siblings_errno_filter(false, errno);
                assert_eq!(got, (-1, errno), "(result, errno)");
            });
        });
    }

    /// The same filter installed while the call is stopped, where no carrier
    /// may run: under a tracer filter on x86-64, and on aarch64, whose backend
    /// sends every restart code of a thread with a seccomp filter of the
    /// guest's own to the Tool's hook. The default hook answers the code
    /// itself, which is refused by name.
    #[test_case(true, ERESTARTSYS; "under a tracer filter, errno 512")]
    #[test_case(true, ERESTARTNOINTR; "under a tracer filter, errno 513")]
    #[test_case(true, ERESTARTNOHAND; "under a tracer filter, errno 514")]
    #[cfg_attr(target_arch = "aarch64", test_case(false, ERESTARTSYS; "no tracer filter, errno 512"))]
    #[cfg_attr(target_arch = "aarch64", test_case(false, ERESTARTNOINTR; "no tracer filter, errno 513"))]
    #[cfg_attr(target_arch = "aarch64", test_case(false, ERESTARTNOHAND; "no tracer filter, errno 514"))]
    fn a_siblings_errno_filter_installed_while_the_call_is_stopped_is_left_to_the_hook(
        filtered: bool,
        errno: i32,
    ) {
        with_or_without_a_tracer_filter(filtered, move || {
            assert_refused::<RunsTheCallAfterASiblingsFilter>(
                reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                    answer: Errno::new(errno),
                },
                move || {
                    getppid_after_a_siblings_errno_filter(false, errno);
                },
            );
        });
    }

    /// Errno 516 from the filter installed while `getppid` is stopped is
    /// refused by name on every backend route, on both architectures, with no tracer
    /// filter (the carrier's route on x86-64) or under one (the hook's):
    /// Linux would restart it as `restart_syscall`, which the filter does not
    /// answer.
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn a_siblings_errno_516_installed_while_the_call_is_stopped_is_refused_by_name(filtered: bool) {
        with_or_without_a_tracer_filter(filtered, move || {
            assert_refused::<RunsTheCallAfterASiblingsFilter>(
                reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
                move || {
                    getppid_after_a_siblings_errno_filter(false, ERESTART_RESTARTBLOCK);
                },
            );
        });
    }

    /// A Tool whose unresolved-restart hook answers `EINTR`: where the hook
    /// decides (under a tracer filter on x86-64; on aarch64 also without
    /// one), errno 514 from the filter installed while the call is stopped
    /// gets the hook's `EINTR`, which shows the hook is consulted, while
    /// errno 516 is refused by name before the hook is asked; on x86-64 with
    /// no tracer filter errno 516 is refused too (the carrier's route).
    #[cfg_attr(target_arch = "x86_64", test_case(true; "under a tracer filter"))]
    #[cfg_attr(target_arch = "aarch64", test_case(false; "no tracer filter"))]
    #[cfg_attr(target_arch = "aarch64", test_case(true; "under a tracer filter"))]
    fn a_hooks_ordinary_answer_replaces_514_but_never_516(filtered: bool) {
        with_or_without_a_tracer_filter(filtered, || {
            check_fn::<RunsTheCallAfterASiblingsFilterAnsweringEintr, _>(|| {
                let got = getppid_after_a_siblings_errno_filter(false, ERESTARTNOHAND);
                assert_eq!(got, (-1, libc::EINTR), "(result, errno)");
            });
        });
        with_or_without_a_tracer_filter(filtered, || {
            assert_refused::<RunsTheCallAfterASiblingsFilterAnsweringEintr>(
                reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
                || {
                    getppid_after_a_siblings_errno_filter(false, ERESTART_RESTARTBLOCK);
                },
            );
        });
    }

    /// With no tracer filter on x86-64, the hook is not consulted for the
    /// carrier's errno 514, and errno 516 is still refused by name.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_hooks_ordinary_answer_does_not_reach_the_carrier_or_516() {
        with_or_without_a_tracer_filter(false, || {
            check_fn::<RunsTheCallAfterASiblingsFilterAnsweringEintr, _>(|| {
                let got = getppid_after_a_siblings_errno_filter(false, ERESTARTNOHAND);
                assert_eq!(got, (-1, ERESTARTNOHAND), "(result, errno)");
            });
        });
        with_or_without_a_tracer_filter(false, || {
            assert_refused::<RunsTheCallAfterASiblingsFilterAnsweringEintr>(
                reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
                || {
                    getppid_after_a_siblings_errno_filter(false, ERESTART_RESTARTBLOCK);
                },
            );
        });
    }

    /// The native-result route, x86-64: the filter installed while the
    /// guest's own `rt_sigsuspend` is stopped answers the call's rerun with
    /// errno 512, 513 or 514, the kernel's own result for one of the three
    /// waits, which the Tool returns unchanged and the backend leaves to the
    /// kernel: the guest gets the filter's errno at once, as natively, under
    /// a tracer filter or none.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false, ERESTARTSYS; "no tracer filter, errno 512")]
    #[test_case(false, ERESTARTNOINTR; "no tracer filter, errno 513")]
    #[test_case(false, ERESTARTNOHAND; "no tracer filter, errno 514")]
    #[test_case(true, ERESTARTSYS; "under a tracer filter, errno 512")]
    #[test_case(true, ERESTARTNOINTR; "under a tracer filter, errno 513")]
    #[test_case(true, ERESTARTNOHAND; "under a tracer filter, errno 514")]
    fn a_siblings_errno_filter_on_the_guests_own_wait_reaches_the_guest_as_natively(
        filtered: bool,
        errno: i32,
    ) {
        with_or_without_a_tracer_filter(filtered, move || {
            check_fn::<RunsTheCallAfterASiblingsFilter, _>(move || {
                let got = rt_sigsuspend_after_a_siblings_errno_filter(false, errno);
                assert_eq!(got, (-1, errno), "(result, errno)");
            });
        });
    }

    /// Errno 516 on the native-result route is refused by name, under a
    /// tracer filter or none, on both architectures (aarch64 has no
    /// native-result route; the same refusal comes first).
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn a_siblings_errno_516_on_the_guests_own_wait_is_refused_by_name(filtered: bool) {
        with_or_without_a_tracer_filter(filtered, move || {
            assert_refused::<RunsTheCallAfterASiblingsFilter>(
                reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
                move || {
                    rt_sigsuspend_after_a_siblings_errno_filter(false, ERESTART_RESTARTBLOCK);
                },
            );
        });
    }

    /// On aarch64, which has no native-result route, errno 514 on the
    /// guest's own wait goes to the hook, whose default answer is refused by
    /// name.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a_siblings_errno_514_on_the_guests_own_wait_is_left_to_the_hook() {
        with_or_without_a_tracer_filter(false, || {
            assert_refused::<RunsTheCallAfterASiblingsFilter>(
                reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                    answer: Errno::ERESTARTNOHAND,
                },
                || {
                    rt_sigsuspend_after_a_siblings_errno_filter(false, ERESTARTNOHAND);
                },
            );
        });
    }

    /// A restart code 516 the Tool makes up, for the probe or for a number
    /// the table cannot name, while the guest has a seccomp filter of its own
    /// (one allowing every call), is refused by name, on both architectures.
    #[test]
    fn a_made_up_516_under_a_guest_seccomp_filter_is_refused_by_name() {
        assert_refused::<RestartResult>(
            reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
            || {
                install_a_seccomp_filter_allowing_every_call();
                probe(
                    ERESTART_RESTARTBLOCK,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    NO_SIGNAL,
                );
            },
        );
        assert_unknown_refused::<UnknownNumberRestart>(
            reverie_ptrace::RestartRefusalReason::RestartBlockUnderGuestSeccompFilter,
            UNKNOWN_PROBE,
            || {
                install_a_seccomp_filter_allowing_every_call();
                probe_number(
                    UNKNOWN_PROBE as libc::c_long,
                    ERESTART_RESTARTBLOCK,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    NO_SIGNAL,
                );
            },
        );
    }

    /// A guest whose sibling thread installs, on every thread, a filter
    /// answering `rt_sigsuspend` with `SECCOMP_RET_USER_NOTIF`, whose
    /// notifications nobody reads, and sends SIGUSR1, handled without
    /// `SA_RESTART` and unblocked, to the waiting thread; that thread then
    /// waits in `rt_sigsuspend`. The interrupted notification wait returns
    /// `-ERESTARTSYS` (kernel/seccomp.c:1199-1211 at v7.1.3), which Linux turns
    /// into `-EINTR` as the handler runs. Traced, the Tool lets the sibling go
    /// while the thread is stopped in the call (`SIBLING_GO`), and the
    /// sibling sends the signal before it reports the filter
    /// (`SIBLING_DONE`); with `first` the thread lets it go and waits for
    /// the filter before the call, and the sibling sends the signal 200 ms
    /// later. Returns the result, its errno and the handler runs.
    fn rt_sigsuspend_after_a_siblings_notification_filter(first: bool) -> (i64, i32, usize) {
        handle(Signal::SIGUSR1, Some(SaFlags::empty()));
        SigSet::from(Signal::SIGUSR1)
            .thread_unblock()
            .expect("unblock the signal");
        // SAFETY: getpid and gettid take no arguments.
        let (pid, tid) = unsafe { (libc::getpid(), libc::syscall(libc::SYS_gettid)) };
        let sibling = std::thread::spawn(move || {
            SigSet::from(Signal::SIGUSR1)
                .thread_block()
                .expect("block the signal in the sibling");
            while SIBLING_GO.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            let listener = install_a_filter_on_every_thread(
                libc::SYS_rt_sigsuspend,
                libc::SECCOMP_RET_USER_NOTIF,
                libc::SECCOMP_FILTER_FLAG_NEW_LISTENER | libc::SECCOMP_FILTER_FLAG_TSYNC_ESRCH,
            );
            let signal = || {
                // SAFETY: tgkill takes three integers and touches no memory.
                let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1) };
                assert_eq!(sent, 0, "tgkill from the sibling");
            };
            if !first {
                signal();
            }
            SIBLING_DONE.store(1, Ordering::SeqCst);
            if first {
                std::thread::sleep(std::time::Duration::from_millis(200));
                signal();
            }
            listener
        });
        if first {
            SIBLING_GO.store(1, Ordering::SeqCst);
            while SIBLING_DONE.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
        }
        let mask = SigSet::empty();
        // SAFETY: rt_sigsuspend reads the mask.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_rt_sigsuspend,
                mask.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let listener = sibling.join().expect("the sibling");
        // SAFETY: the listener is the sibling's open descriptor, closed once.
        unsafe { libc::close(listener as i32) };
        if !first {
            let (before, after) = (
                SIBLING_FILTERS[0].load(Ordering::SeqCst),
                SIBLING_FILTERS[1].load(Ordering::SeqCst),
            );
            assert_eq!(after, before + 1, "Seccomp_filters rose during the stop");
        }
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
        )
    }

    /// What Linux gives `rt_sigsuspend_after_a_siblings_notification_filter`:
    /// `-ERESTARTSYS` with a handler that has no `SA_RESTART` is `-EINTR`
    /// (`handle_signal`, arch/x86/kernel/signal.c:272-276 at v7.1.3), and the
    /// handler runs once.
    const THE_NOTIFICATION_WAIT_INTERRUPTED: (i64, i32, usize) = (-1, libc::EINTR, 1);

    /// Native control for the interrupted notification wait.
    #[test]
    fn a_siblings_notification_filter_wait_is_interrupted_natively() {
        run_natively(|| {
            let got = rt_sigsuspend_after_a_siblings_notification_filter(true);
            assert_eq!(
                got, THE_NOTIFICATION_WAIT_INTERRUPTED,
                "(result, errno, handler runs)"
            );
        });
    }

    /// A `SECCOMP_RET_USER_NOTIF` filter present at the call's entry wins
    /// over the backend's `SECCOMP_RET_TRACE` (kernel/seccomp.c:426 at
    /// v7.1.3): the call never reaches the Tool, its interrupted notification
    /// wait returns `-ERESTARTSYS`, and Linux resolves it, as natively,
    /// under a tracer filter or none.
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn a_notification_filter_present_at_the_calls_entry_answers_it_before_the_tracer(
        filtered: bool,
    ) {
        with_or_without_a_tracer_filter(filtered, || {
            check_fn::<RunsTheCallAfterASiblingsFilter, _>(|| {
                let got = rt_sigsuspend_after_a_siblings_notification_filter(true);
                assert_eq!(
                    got, THE_NOTIFICATION_WAIT_INTERRUPTED,
                    "(result, errno, handler runs)"
                );
            });
        });
    }

    /// The notification filter installed, and the signal sent, while the
    /// guest's own `rt_sigsuspend` is stopped at its seccomp stop: the
    /// backend runs the call in place, Linux's second run of the filters
    /// waits for a notification answer, the pending signal interrupts the
    /// wait, and the call's exit stop shows `-ERESTARTSYS`, which the Tool
    /// returns. The code came from a `signal_pending` check, so this thread's
    /// `TIF_SIGPENDING` is set: on x86-64 the native result of one of the
    /// three waits is left to the kernel, which converts it as natively,
    /// under a tracer filter or none.
    #[cfg(target_arch = "x86_64")]
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn an_interrupted_notification_wait_of_the_guests_own_wait_is_resolved_by_the_kernel(
        filtered: bool,
    ) {
        with_or_without_a_tracer_filter(filtered, || {
            check_fn::<RunsTheCallAfterASiblingsFilter, _>(|| {
                let got = rt_sigsuspend_after_a_siblings_notification_filter(false);
                assert_eq!(
                    got, THE_NOTIFICATION_WAIT_INTERRUPTED,
                    "(result, errno, handler runs)"
                );
            });
        });
    }

    /// On aarch64, whose backend sends every restart code of a thread with a
    /// seccomp filter of the guest's own to the Tool's hook, the interrupted
    /// notification wait's `-ERESTARTSYS` is refused by name.
    #[cfg(target_arch = "aarch64")]
    #[test_case(false; "no tracer filter")]
    #[test_case(true; "under a tracer filter")]
    fn an_interrupted_notification_wait_of_the_guests_own_wait_is_refused_by_name(filtered: bool) {
        with_or_without_a_tracer_filter(filtered, || {
            assert_refused::<RunsTheCallAfterASiblingsFilter>(
                reverie_ptrace::RestartRefusalReason::HookAnsweredInternalCode {
                    answer: Errno::ERESTARTSYS,
                },
                || {
                    rt_sigsuspend_after_a_siblings_notification_filter(false);
                },
            );
        });
    }

    /// The guest's raw `ppoll` with no descriptors and a 10 s timeout, under a
    /// temporary mask that lets `sig` through while the thread blocks it, with
    /// `sig` handled under `flags` (default action without). Returns the
    /// result, its errno, the handler runs, and whether `sig` is blocked
    /// again afterwards.
    fn masked_ppoll(
        code: i32,
        sig: Signal,
        flags: Option<SaFlags>,
        mode: i64,
    ) -> (i64, i32, usize, bool) {
        handle(sig, flags);
        SigSet::from(sig).thread_block().expect("block the signal");
        if mode == HELD_WITH_SIGUSR1_PENDING {
            handle(Signal::SIGUSR1, Some(SaFlags::SA_RESTART));
            SigSet::from(Signal::SIGUSR1)
                .thread_block()
                .expect("block SIGUSR1");
        }
        let blocks_it = SigSet::from(sig);
        configure(code, sig, mode, &blocks_it);
        let lets_it_through = SigSet::empty();
        let timeout = libc::timespec {
            tv_sec: 10,
            tv_nsec: 0,
        };
        // SAFETY: ppoll reads no descriptors, the timeout and the mask.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_ppoll,
                std::ptr::null_mut::<libc::pollfd>(),
                0,
                &timeout as *const libc::timespec,
                lets_it_through.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let blocked_after = SigSet::thread_get_mask()
            .expect("read the mask")
            .contains(sig);
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
            blocked_after,
        )
    }

    /// Configures the thread's next `ppoll` or `rt_sigsuspend`
    /// (`CONFIGURE_PPOLL`), with `set` for `HELD_THEN_BLOCKED`.
    fn configure(code: i32, sig: Signal, mode: i64, set: &SigSet) {
        // SAFETY: the probe syscall takes integers; Linux ignores them.
        unsafe {
            libc::syscall(
                libc::SYS_getppid,
                code as libc::c_long,
                sig as libc::c_long,
                CONFIGURE_PPOLL as libc::c_long,
                mode as libc::c_long,
                set.as_ref() as *const libc::sigset_t,
            )
        };
    }

    /// The guest's raw `rt_sigsuspend` with an empty mask while the thread
    /// blocks `sig`, handled under `flags`: as `masked_ppoll`.
    fn masked_sigsuspend(sig: Signal, flags: Option<SaFlags>, mode: i64) -> (i64, i32, usize, bool) {
        handle(sig, flags);
        SigSet::from(sig).thread_block().expect("block the signal");
        configure(0, sig, mode, &SigSet::empty());
        let lets_it_through = SigSet::empty();
        // SAFETY: rt_sigsuspend reads the mask.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_rt_sigsuspend,
                lets_it_through.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let blocked_after = SigSet::thread_get_mask()
            .expect("read the mask")
            .contains(sig);
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
            blocked_after,
        )
    }

    /// The guest's raw `ppoll` of one ignored descriptor (so its first argument
    /// is a pointer) with a 10 s timeout, under a temporary mask that lets
    /// `sig` through while the thread blocks it, interrupted with `mode`.
    /// With `handler_polls`, `sig`'s handler runs a `ppoll` of its own through
    /// the same wrapper (`on_signal_polling`). Returns the result, its errno,
    /// the handler runs, whether `sig` is blocked again, and what the
    /// handler's `ppoll` returned.
    fn masked_ppoll_of_a_descriptor(
        sig: Signal,
        handler_polls: bool,
        mode: i64,
    ) -> (i64, i32, usize, bool, i64) {
        if handler_polls {
            let action = SigAction::new(
                SigHandler::Handler(on_signal_polling),
                SaFlags::SA_RESTART,
                SigSet::empty(),
            );
            // SAFETY: the handler increments an atomic and makes a syscall.
            unsafe { sigaction(sig, &action) }.expect("install the handler");
        }
        SigSet::from(sig).thread_block().expect("block the signal");
        configure(0, sig, mode, &SigSet::empty());
        let mut ignored = libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        };
        let lets_it_through = SigSet::empty();
        let timeout = libc::timespec {
            tv_sec: 10,
            tv_nsec: 0,
        };
        // SAFETY: ppoll reads one descriptor entry, the timeout and the mask.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_ppoll,
                &mut ignored as *mut libc::pollfd,
                1,
                &timeout as *const libc::timespec,
                lets_it_through.as_ref() as *const libc::sigset_t,
                std::mem::size_of::<u64>(),
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let blocked_after = SigSet::thread_get_mask()
            .expect("read the mask")
            .contains(sig);
        (
            ret,
            if ret == -1 { errno } else { 0 },
            HANDLED.load(Ordering::SeqCst),
            blocked_after,
            HANDLER_POLL.load(Ordering::SeqCst),
        )
    }

    fn check_masked_ppoll(
        code: i32,
        sig: Signal,
        flags: Option<SaFlags>,
        mode: i64,
        want: (i64, i32, usize, bool),
    ) {
        check_fn::<RestartResult, _>(move || {
            let got = masked_ppoll(code, sig, flags, mode);
            assert_eq!(
                got, want,
                "(result, errno, handler runs, the signal blocked again)"
            );
        });
    }

    /// `ppoll` leaves its temporary mask's restore pending with
    /// `-ERESTARTNOHAND` (`poll_select_finish`, fs/select.c). With a handler
    /// that is `-EINTR`, even under `SA_RESTART`, and the handler's return
    /// puts the guest's own mask back (`sigmask_to_save`).
    #[test]
    fn erestartnohand_under_a_temporary_mask_is_eintr_and_restores_the_mask() {
        let flags = Some(SaFlags::SA_RESTART);
        let want = (-1, libc::EINTR, 1, true);
        check_masked_ppoll(ERESTARTNOHAND, Signal::SIGUSR1, flags, HELD_OVER_INJECTION, want);
    }

    /// With no handler (SIGWINCH at its default action), or with the held
    /// signal dropped, `ppoll` restarts with the guest's own mask back
    /// (`restore_saved_sigmask`), and the restarted call is answered.
    #[test_case(Signal::SIGWINCH, None, HELD_OVER_INJECTION; "sigwinch at its default action")]
    #[test_case(Signal::SIGUSR1, Some(SaFlags::SA_RESTART), HELD_AND_DROPPED; "sigusr1 dropped")]
    fn erestartnohand_under_a_temporary_mask_without_a_handler_restarts(
        sig: Signal,
        flags: Option<SaFlags>,
        mode: i64,
    ) {
        check_masked_ppoll(ERESTARTNOHAND, sig, flags, mode, (RESTARTED, 0, 0, true));
    }

    /// A held signal without a handler (SIGWINCH) with SIGUSR1, which has one,
    /// pending too: `get_signal` goes on to SIGUSR1 in the same `do_signal`,
    /// whose handler makes the call `-EINTR`.
    #[test]
    fn a_handled_signal_pending_behind_one_without_a_handler_is_eintr() {
        let want = (-1, libc::EINTR, 1, true);
        check_masked_ppoll(ERESTARTNOHAND, Signal::SIGWINCH, None, HELD_WITH_SIGUSR1_PENDING, want);
    }

    /// `-ERESTARTSYS` with a handler under a pending mask restore, which Linux
    /// never returns: x86-64's kernel resolves it after the tracer's last stop
    /// (`SA_RESTART`: the handler runs and the call restarts).
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn erestartsys_with_a_handler_under_a_temporary_mask() {
        let flags = Some(SaFlags::SA_RESTART);
        let want = (RESTARTED, 0, 1, true);
        check_masked_ppoll(ERESTARTSYS, Signal::SIGUSR1, flags, HELD_OVER_INJECTION, want);
    }

    /// The same on aarch64 is refused by name, with an error that ends the
    /// run: its outcome hangs on `SA_RESTART`, which procfs does not show, so
    /// this Tool refuses it (`resolve_restart_under_pending_mask_restore`,
    /// `Undecidable::ErestartsysWithHandler`).
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn erestartsys_with_a_handler_under_a_temporary_mask() {
        assert_undecidable(Undecidable::ErestartsysWithHandler, || {
            masked_ppoll(
                ERESTARTSYS,
                Signal::SIGUSR1,
                Some(SaFlags::SA_RESTART),
                HELD_OVER_INJECTION,
            );
        });
    }

    /// Asserts that the backend ends the run `guest` makes under Tool `T` with
    /// the refusal `reason` (`reverie_ptrace::RestartRefusal`).
    fn assert_refused<T: Tool + 'static>(
        reason: reverie_ptrace::RestartRefusalReason,
        guest: impl FnOnce(),
    ) {
        assert_refused_with_config::<T>(reason, Default::default(), guest);
    }

    /// `assert_refused`, with the configuration `config`.
    fn assert_refused_with_config<T: Tool + 'static>(
        reason: reverie_ptrace::RestartRefusalReason,
        config: <T::GlobalState as GlobalTool>::Config,
        guest: impl FnOnce(),
    ) {
        let run = reverie_ptrace::testing::test_fn_with_config::<T, _>(guest, config, true);
        let error = match run {
            Ok((output, _)) => panic!("expected {reason:?}; the guest exited {:?}", output.status),
            Err(error) => error,
        };
        let refusal = match &error {
            Error::Tool(inner) => inner.downcast_ref::<reverie_ptrace::RestartRefusal>(),
            _ => None,
        };
        let refusal = refusal.unwrap_or_else(|| panic!("expected RestartRefusal, got {error:?}"));
        assert_eq!(refusal.reason, reason, "{refusal}");
    }

    /// Asserts that the backend ends the run `guest` makes under Tool `T` with
    /// the refusal `reason` of a code for the unknown number `nr`
    /// (`reverie_ptrace::UnknownSyscallRestartRefusal`).
    fn assert_unknown_refused<T: Tool + 'static>(
        reason: reverie_ptrace::RestartRefusalReason,
        nr: u64,
        guest: impl FnOnce(),
    ) {
        let error = match reverie_ptrace::testing::test_fn::<T, _>(guest) {
            Ok((output, _)) => panic!("expected {reason:?}; the guest exited {:?}", output.status),
            Err(error) => error,
        };
        let refusal = match &error {
            Error::Tool(inner) => {
                inner.downcast_ref::<reverie_ptrace::UnknownSyscallRestartRefusal>()
            }
            _ => None,
        };
        let refusal = refusal
            .unwrap_or_else(|| panic!("expected UnknownSyscallRestartRefusal, got {error:?}"));
        assert_eq!((refusal.reason, refusal.nr), (reason, nr), "{refusal}");
    }

    /// Asserts that the run `guest` makes ends with this Tool's refusal of
    /// its `-ERESTARTSYS` for `ppoll`, for `reason` (`RestartRefused`).
    #[cfg(target_arch = "aarch64")]
    fn assert_undecidable(reason: Undecidable, guest: impl FnOnce()) {
        let error = match reverie_ptrace::testing::test_fn::<RestartResult, _>(guest) {
            Ok((output, _)) => panic!("expected {reason:?}; the guest exited {:?}", output.status),
            Err(error) => error,
        };
        let refusal = match &error {
            Error::Tool(inner) => inner.downcast_ref::<RestartRefused>(),
            _ => None,
        };
        let refusal = refusal.unwrap_or_else(|| panic!("expected RestartRefused, got {error:?}"));
        assert_eq!(
            *refusal,
            RestartRefused {
                syscall: Sysno::ppoll,
                code: Errno::ERESTARTSYS,
                reason,
            },
            "{refusal}"
        );
    }

    /// A held signal that an injected `rt_sigprocmask` then blocks: the kernel
    /// queues it again rather than deliver it, no handler runs, and the call
    /// restarts with the guest's own mask, where the signal stays pending.
    #[test]
    fn a_held_signal_the_mask_in_effect_then_blocks_restarts() {
        let flags = Some(SaFlags::SA_RESTART);
        let want = (RESTARTED, 0, 0, true);
        check_masked_ppoll(ERESTARTNOHAND, Signal::SIGUSR1, flags, HELD_THEN_BLOCKED, want);
    }

    /// The plain pattern: the Tool injects the guest's own `ppoll` or
    /// `rt_sigsuspend`, a signal interrupts it, and the Tool returns the
    /// injection's result as the guest's, the signal left pending. Linux gives
    /// `-EINTR` for a handler, which runs once, and restarts the call without
    /// one (SIGWINCH), the guest's own mask restored either way.
    #[test_case(false, Signal::SIGUSR1, (-1, libc::EINTR, 1, true); "ppoll with a handler")]
    #[test_case(true, Signal::SIGUSR1, (-1, libc::EINTR, 1, true); "rt_sigsuspend with a handler")]
    #[test_case(false, Signal::SIGWINCH, (RESTARTED, 0, 0, true); "ppoll without a handler")]
    #[test_case(true, Signal::SIGWINCH, (RESTARTED, 0, 0, true); "rt_sigsuspend without a handler")]
    fn an_interrupted_injections_own_result_is_resolved_as_on_linux(
        sigsuspend: bool,
        sig: Signal,
        want: (i64, i32, usize, bool),
    ) {
        let flags = (sig == Signal::SIGUSR1).then_some(SaFlags::SA_RESTART);
        check_fn::<RestartResult, _>(move || {
            let got = if sigsuspend {
                masked_sigsuspend(sig, flags, PLAIN)
            } else {
                masked_ppoll(0, sig, flags, PLAIN)
            };
            assert_eq!(
                got, want,
                "(result, errno, handler runs, the signal blocked again)"
            );
        });
    }

    /// The Tool runs the guest's `ppoll` with a first argument of its own,
    /// a signal with no handler (SIGWINCH) interrupts it, and the Tool returns
    /// the call's result: the restarted call runs with the guest's own first
    /// argument, which the Tool checks. This Tool does not declare serialized
    /// execution, so on aarch64 the injection is single-stepped from the
    /// private page and ends at a SIGTRAP stop, after which the backend
    /// writes the restart itself, with the guest's first argument. The
    /// syscall-stop transport, after which the kernel restarts the call, is
    /// tested below.
    #[test]
    fn a_restarted_call_runs_with_the_guests_own_first_argument() {
        check_fn::<RestartResult, _>(|| {
            let got = masked_ppoll_of_a_descriptor(
                Signal::SIGWINCH,
                false,
                PLAIN_WITH_ANOTHER_FIRST_ARGUMENT,
            );
            assert_eq!(
                got,
                (RESTARTED, 0, 0, true, i64::MIN),
                "(result, errno, handler runs, the signal blocked again, the handler's ppoll)"
            );
        });
    }

    /// "ppoll without a handler" under a Tool that declares serialized
    /// execution, with the Tool's call given a first argument the call never
    /// reads (no descriptors; the same temporary mask and timeout). On aarch64
    /// the backend runs that mask-swapping injection between syscall stops
    /// (`syscall_stops_admitted`), which loads the injected arguments before
    /// the private `svc`, so the kernel saves the Tool's first argument as
    /// `orig_x0`, and a kernel restart would put it back in `x0`. The backend
    /// therefore writes the restart itself, with the guest's first argument
    /// (`defer_restart_under_pending_mask_restore`), and the Tool answers a
    /// restarted call only with the guest's first argument.
    #[test]
    fn ppoll_without_a_handler_restarts_with_the_guests_first_argument_after_a_serialized_injection()
     {
        check_fn_with_config::<RestartResult, _>(
            || {
                let got = masked_ppoll(0, Signal::SIGWINCH, None, PLAIN_WITH_AN_UNREAD_FIRST_ARGUMENT);
                assert_eq!(
                    got,
                    (RESTARTED, 0, 0, true),
                    "(result, errno, handler runs, the signal blocked again)"
                );
            },
            true,
            true,
        );
    }

    /// As above, with the signal sent to the process, and another guest thread
    /// waiting for it in `rt_sigtimedwait` until after the interruption: that
    /// thread takes it from the shared pending set while the tracer holds the
    /// first, so no signal is left to deliver when the first resumes. A kernel
    /// restart would then come from a `do_signal` that finds no signal and
    /// restarts the call with no ptrace stop, where no tracer could put the
    /// guest's first argument back; the backend writes the restart itself.
    /// The second thread running guest code meanwhile breaks the
    /// serialized-execution contract the Tool declares; it is the only way a
    /// guest can take the signal away from the first thread.
    #[test]
    fn a_restart_with_the_signal_taken_by_another_thread_keeps_the_guests_first_argument() {
        check_fn_with_config::<RestartResult, _>(
            || {
                SigSet::from(Signal::SIGWINCH)
                    .thread_block()
                    .expect("block the signal");
                let waiting = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let taker = {
                    let waiting = waiting.clone();
                    std::thread::spawn(move || {
                        let set = SigSet::from(Signal::SIGWINCH);
                        let timeout = libc::timespec {
                            tv_sec: 1,
                            tv_nsec: 0,
                        };
                        waiting.store(true, Ordering::SeqCst);
                        // SAFETY: rt_sigtimedwait reads the set and the timeout.
                        unsafe {
                            libc::syscall(
                                libc::SYS_rt_sigtimedwait,
                                set.as_ref() as *const libc::sigset_t,
                                std::ptr::null_mut::<libc::siginfo_t>(),
                                &timeout as *const libc::timespec,
                                std::mem::size_of::<u64>(),
                            )
                        }
                    })
                };
                while !waiting.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                let got = masked_ppoll(0, Signal::SIGWINCH, None, TAKEN_BY_ANOTHER_THREAD);
                assert_eq!(
                    taker.join().unwrap(),
                    libc::SIGWINCH as libc::c_long,
                    "the other thread takes the signal"
                );
                assert_eq!(
                    got,
                    (RESTARTED, 0, 0, true),
                    "(result, errno, handler runs, the signal blocked again)"
                );
            },
            true,
            true,
        );
    }

    /// The Tool runs the guest's own `ppoll` of a descriptor, a signal whose
    /// handler runs a `ppoll` of its own through the same wrapper (so from the
    /// same `svc`, with the same syscall number and another first argument)
    /// interrupts it, and the Tool returns the call's result, which the kernel
    /// resolves: the guest's call is `-EINTR`, and the handler's call keeps its
    /// own first argument and returns 0.
    #[test]
    fn a_handler_calling_the_same_wrapper_keeps_its_own_first_argument() {
        check_fn::<RestartResult, _>(|| {
            let got = masked_ppoll_of_a_descriptor(Signal::SIGUSR1, true, PLAIN);
            assert_eq!(
                got,
                (-1, libc::EINTR, 1, true, 0),
                "(result, errno, handler runs, the signal blocked again, the handler's ppoll)"
            );
        });
    }

    /// Blocks every realtime signal from 34 to 64 for the thread, so those
    /// the tracer sends stay pending (`HELD_WITH_EVERY_REALTIME_SIGNAL_PENDING`).
    fn block_every_realtime_signal() {
        let every_realtime: u64 = (34..=64).map(|signal| 1u64 << (signal - 1)).sum();
        // SAFETY: rt_sigprocmask reads the 8-byte set.
        let blocked = unsafe {
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_BLOCK,
                &every_realtime as *const u64,
                std::ptr::null_mut::<u64>(),
                std::mem::size_of::<u64>(),
            )
        };
        assert_eq!(blocked, 0, "block every realtime signal");
    }

    /// A held signal with a handler while every realtime signal is pending:
    /// x86-64's kernel resolves the code, `-EINTR`.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_held_signal_with_every_realtime_signal_pending() {
        check_fn::<RestartResult, _>(|| {
            block_every_realtime_signal();
            let flags = Some(SaFlags::SA_RESTART);
            let got = probe(
                ERESTARTNOHAND,
                Signal::SIGUSR1,
                flags,
                HELD_WITH_EVERY_REALTIME_SIGNAL_PENDING,
            );
            assert_eq!(got, (-1, libc::EINTR, 1), "(result, errno, handler runs)");
        });
    }

    /// The same on aarch64 is refused by name: no realtime signal is free to
    /// carry the resume into `do_signal`.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a_held_signal_with_every_realtime_signal_pending() {
        assert_refused::<RestartResult>(
            reverie_ptrace::RestartRefusalReason::NoFreeCarrierSignal,
            || {
                block_every_realtime_signal();
                probe(
                    ERESTARTNOHAND,
                    Signal::SIGUSR1,
                    Some(SaFlags::SA_RESTART),
                    HELD_WITH_EVERY_REALTIME_SIGNAL_PENDING,
                );
            },
        );
    }
}
