/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A Tool that answers a number the syscall table cannot name under
//! `UnknownSyscallPolicy::TrapToTool` injects syscalls from
//! `Tool::handle_unknown_syscall` as from `Tool::handle_syscall_event`: the
//! guest's own call never reaches the kernel, each injection runs once and
//! returns its own result, and the value the Tool returns is the call's.

#[cfg(all(not(sanitized), test))]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use nix::sys::signal::SaFlags;
    use nix::sys::signal::SigAction;
    use nix::sys::signal::SigHandler;
    use nix::sys::signal::SigSet;
    use nix::sys::signal::Signal;
    use nix::sys::signal::sigaction;
    use reverie::Error;
    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::Pid;
    use reverie::Subscription;
    use reverie::Tool;
    use reverie::UnknownSyscallPolicy;
    use reverie::syscalls::Errno;
    use reverie::syscalls::Lseek;
    use reverie::syscalls::RawSyscall;
    use reverie::syscalls::Whence;
    use reverie_ptrace::testing::check_fn;
    use test_case::test_case;

    /// A number with no `Sysno` in either architecture's table (both end at
    /// 469), which Linux implements on neither.
    const UNKNOWN: libc::c_long = 1000;

    /// The tracer sends the guest SIGUSR1 before the Tool's first injection.
    const SIGNAL_FIRST: usize = 1;

    #[derive(Debug, Default)]
    struct Global;

    #[reverie::global_tool]
    impl GlobalTool for Global {
        type Request = ();
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, _request: ()) {}
    }

    /// Answers `UNKNOWN` with the result of an injected `lseek` that moves the
    /// descriptor in the call's first argument one byte on. With
    /// `SIGNAL_FIRST` in the second argument, the tracer first sends SIGUSR1,
    /// which interrupts the first injection before it runs and is held for
    /// the guest's resume, and the Tool injects the call again.
    #[derive(Debug, Default, Clone)]
    struct InjectingTool;

    #[reverie::tool]
    impl Tool for InjectingTool {
        type GlobalState = Global;
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            let mut subscriptions = Subscription::none();
            subscriptions.set_unknown_syscall_policy(UnknownSyscallPolicy::TrapToTool);
            subscriptions
        }

        async fn handle_unknown_syscall<G: Guest<Self>>(
            &self,
            guest: &mut G,
            raw: RawSyscall,
        ) -> Result<i64, Error> {
            if raw.nr != UNKNOWN as u64 {
                return Err(Errno::ENOSYS.into());
            }
            let step = Lseek::new()
                .with_fd(raw.args.arg0 as i32)
                .with_offset(1)
                .with_whence(Whence::SEEK_CUR);
            if raw.args.arg1 == SIGNAL_FIRST {
                let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
                // SAFETY: tgkill takes three integers and touches no memory.
                let sent = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1) };
                assert_eq!(sent, 0, "tgkill from the tracer");
                assert!(
                    guest.inject(step).await.is_err(),
                    "the pending signal interrupts the first injection, and is held"
                );
            }
            Ok(guest.inject(step).await?)
        }
    }

    static HANDLED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn on_signal(_: libc::c_int) {
        HANDLED.fetch_add(1, Ordering::SeqCst);
    }

    /// Makes the call `UNKNOWN` with `mode` on a fresh file at offset 0, with
    /// SIGUSR1 handled, and returns its result, its errno, the file's offset
    /// after it, and the handler runs.
    fn call_unknown(mode: usize) -> (i64, i32, i64, usize) {
        let action = SigAction::new(
            SigHandler::Handler(on_signal),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: the handler only increments an atomic.
        unsafe { sigaction(Signal::SIGUSR1, &action) }.expect("install the handler");
        // SAFETY: memfd_create reads the name, which outlives the call.
        let fd = unsafe { libc::memfd_create(c"unknown-syscall-injection".as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create");
        // SAFETY: the unknown call takes integers; the Tool answers it.
        let ret = unsafe { libc::syscall(UNKNOWN, fd as libc::c_long, mode as libc::c_long) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        // SAFETY: lseek takes integers and touches no memory.
        let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
        (
            ret,
            if ret == -1 { errno } else { 0 },
            offset,
            HANDLED.load(Ordering::SeqCst),
        )
    }

    /// The injection runs once, and its result is the call's: offset 1. With a
    /// pending signal the first injection is interrupted before it runs, the
    /// second runs once, and the handler runs once when the guest resumes.
    #[test_case(0, 0; "without a pending signal")]
    #[test_case(SIGNAL_FIRST, 1; "with a pending signal")]
    fn an_injection_from_the_unknown_syscall_hook_runs_once(mode: usize, runs: usize) {
        check_fn::<InjectingTool, _>(move || {
            assert_eq!(
                call_unknown(mode),
                (1, 0, 1, runs),
                "(result, errno, offset after, handler runs)"
            );
        });
    }
}
