/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The Narf execution core with `std` off.
//!
//! `reverie_narf_core` is the code the Narf kernel links: it hosts an
//! unmodified `reverie::Tool` over the kernel's services. The positive build
//! compiles it, and the fake kernel its own tests use, for
//! `x86_64-unknown-none`. The execution step drives reverie-examples'
//! counter1 through it: new syscalls, a thread, a fork, a parked read and the
//! exits that tear the tasks down. It drives counter2 the same way, with the
//! thread-exit reporter a backend without `std` sets through the host's Tool
//! constructor, and reverie-narf-tools' strace and chaos, whose `eprintln!`
//! lines reach the sink a backend sets. chaos also changes what runs: it
//! fails reads with `EINTR` and cuts the others, keeping one flag per thread,
//! and reads its options through the host's configuration.

#[path = "../../reverie-narf-core/tests/support/fake_kernel.rs"]
pub mod fake_kernel;

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use reverie::Pid;
    use reverie::Tool;
    use reverie::syscalls::libc;
    use reverie_narf_core::Disposition;
    use reverie_narf_core::TaskExit;
    use reverie_narf_tools::chaos::ChaosOpts;
    use reverie_narf_tools::chaos::ChaosTool;
    use reverie_narf_tools::strace::Config;
    use reverie_narf_tools::strace::Strace;
    use syscalls::Sysno;

    use super::fake_kernel::FakeHost;
    use super::fake_kernel::FakeKernel;
    use super::fake_kernel::PIPE_FD;
    use super::fake_kernel::Via;
    use super::fake_kernel::request;
    use crate::tool_contract::counter1_tool::CounterLocal;
    use crate::tool_contract::counter2_tool;

    const BASE: usize = 0x10_0000;
    const NONE: [u64; 6] = [0; 6];

    #[test]
    fn counter1_runs_on_the_narf_core() {
        let host = FakeHost::<CounterLocal>::new(()).ok().expect("host");
        let kernel = FakeKernel::new();
        let root = kernel.spawn_root(&host, BASE);
        let complete = |tid: Pid, sysno: Sysno, args: [u64; 6]| match kernel.syscall(
            &host,
            tid,
            request(sysno, args),
        ) {
            Ok(Disposition::Complete(value)) => value,
            _ => panic!("{sysno} did not complete"),
        };
        let managed = |tid: Pid, sysno: Sysno, args: [u64; 6]| {
            let result = kernel.syscall(&host, tid, request(sysno, args));
            assert!(matches!(result, Ok(Disposition::ContextManaged)), "{sysno}");
        };

        kernel.poke(root, BASE, b"narf");
        assert_eq!(complete(root, Sysno::getpid, NONE), 1000);
        assert_eq!(
            complete(root, Sysno::write, [1, BASE as u64, 4, 0, 0, 0]),
            4
        );
        let thread = (libc::CLONE_THREAD | libc::CLONE_VM) as u64;
        let thread = Pid::from_raw(complete(root, Sysno::clone, [thread, 0, 0, 0, 0, 0]) as i32);
        assert_eq!(complete(thread, Sysno::getpid, NONE), 1000);
        let child = Pid::from_raw(complete(root, Sysno::fork, NONE) as i32);
        assert_eq!(complete(child, Sysno::getppid, NONE), 1000);

        managed(child, Sysno::read, [PIPE_FD, BASE as u64, 4, 0, 0, 0]);
        kernel.push_pipe(b"ok");
        let reexecuted = kernel.reexecute(&host, child);
        assert!(matches!(reexecuted, Ok(Disposition::Complete(2))));
        assert_eq!(kernel.peek(child, BASE, 4), b"okrf");
        assert_eq!(kernel.peek(root, BASE, 4), b"narf");

        managed(thread, Sysno::exit, NONE);
        managed(child, Sysno::exit_group, NONE);
        managed(root, Sysno::exit_group, NONE);

        assert_eq!(host.global().total(), 10);
        assert_eq!(kernel.output(), b"narf");
        let natives = kernel.natives();
        assert_eq!(
            natives.len(),
            11,
            "the parked read ran twice, the Tool saw it once"
        );
        assert!(natives.iter().all(|native| native.via == Via::Original));
        assert!(kernel.violations().is_empty());
        let teardowns: Vec<(i32, bool)> = kernel
            .teardowns()
            .into_iter()
            .map(|(tid, result)| {
                (
                    tid,
                    matches!(
                        result,
                        Ok(TaskExit {
                            process_exited: true
                        })
                    ),
                )
            })
            .collect();
        assert_eq!(teardowns, [(1001, false), (1002, true), (1000, true)]);
        assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
    }

    /// The thread-exit lines [`record_thread_exit`] received, in order.
    static THREAD_EXITS: std::sync::Mutex<Vec<(i32, u64)>> = std::sync::Mutex::new(Vec::new());

    fn record_thread_exit(tid: Pid, syscalls: u64) {
        THREAD_EXITS.lock().unwrap().push((tid.as_raw(), syscalls));
    }

    /// counter2 counts each thread's syscalls in its thread state, adds them
    /// to its process's Tool at the thread's exit, and sends the process's
    /// totals to the global state at the process's exit.
    #[test]
    fn counter2_runs_on_the_narf_core() {
        let host = FakeHost::<counter2_tool::CounterLocal>::new(())
            .expect("host")
            .with_tool_constructor(|pid, config| {
                <counter2_tool::CounterLocal as Tool>::new(pid, config)
                    .with_thread_exit_reporter(record_thread_exit)
            });
        let kernel = FakeKernel::new();
        let root = kernel.spawn_root(&host, BASE);
        let complete = |tid: Pid, sysno: Sysno, args: [u64; 6]| match kernel.syscall(
            &host,
            tid,
            request(sysno, args),
        ) {
            Ok(Disposition::Complete(value)) => value,
            _ => panic!("{sysno} did not complete"),
        };
        let managed = |tid: Pid, sysno: Sysno, args: [u64; 6]| {
            let result = kernel.syscall(&host, tid, request(sysno, args));
            assert!(matches!(result, Ok(Disposition::ContextManaged)), "{sysno}");
        };

        complete(root, Sysno::getpid, NONE);
        let thread = (libc::CLONE_THREAD | libc::CLONE_VM) as u64;
        let thread = Pid::from_raw(complete(root, Sysno::clone, [thread, 0, 0, 0, 0, 0]) as i32);
        complete(thread, Sysno::getpid, NONE);
        complete(thread, Sysno::getpid, NONE);
        let child = Pid::from_raw(complete(root, Sysno::fork, NONE) as i32);
        managed(child, Sysno::read, [PIPE_FD, BASE as u64, 4, 0, 0, 0]);
        kernel.push_pipe(b"ok");
        let reexecuted = kernel.reexecute(&host, child);
        assert!(matches!(reexecuted, Ok(Disposition::Complete(2))));
        managed(thread, Sysno::exit, NONE);
        managed(child, Sysno::exit_group, NONE);
        managed(root, Sysno::exit_group, NONE);

        // The child's parked read counts once, as the Tool saw it once.
        assert_eq!(
            *THREAD_EXITS.lock().unwrap(),
            [(1001, 3), (1002, 2), (1000, 4)]
        );
        assert_eq!(host.global().totals(), (9, 2, 3));
        assert!(kernel.violations().is_empty());
        assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
    }

    /// The lines [`record_line`] received, in order, each with the thread
    /// that printed it. The sink is global and the tests run in parallel,
    /// each on its own thread; the fake kernel runs the Tool on the thread
    /// that delivers the syscall.
    static LINES: std::sync::Mutex<Vec<(std::thread::ThreadId, std::string::String)>> =
        std::sync::Mutex::new(Vec::new());

    fn record_line(line: &str) {
        let printer = std::thread::current().id();
        LINES.lock().unwrap().push((printer, line.into()));
    }

    /// Removes and returns the lines the current thread printed, in order.
    fn take_lines() -> Vec<std::string::String> {
        let me = std::thread::current().id();
        let mut lines = LINES.lock().unwrap();
        let (mine, others): (Vec<_>, Vec<_>) = core::mem::take(&mut *lines)
            .into_iter()
            .partition(|(printer, _)| *printer == me);
        *lines = others;
        mine.into_iter().map(|(_, line)| line).collect()
    }

    /// Each native execution: the task, the syscall, its third argument
    /// (a read's or receive's length) and how it was requested.
    fn natives(kernel: &FakeKernel) -> Vec<(i32, Option<Sysno>, u64, Via)> {
        kernel
            .natives()
            .into_iter()
            .map(|native| {
                let sysno = Sysno::new(native.request.linux_number() as usize);
                (native.tid, sysno, native.request.args[2], native.via)
            })
            .collect()
    }

    /// strace prints a syscall that returns once, with its value. exit_group
    /// and an execve that replaces the program print before they run and
    /// never return to it; a failed execve prints its errno on a second line.
    /// A parked read prints once, at re-execution, with the value it returned
    /// then. Each thread's and each process's exit follow its last syscall.
    #[test]
    fn strace_runs_on_the_narf_core() {
        reverie_narf_tools::set_eprintln_sink(record_line);
        let host = FakeHost::<Strace>::new(Config::default()).expect("host");
        let kernel = FakeKernel::new();
        let root = kernel.spawn_root(&host, BASE);
        let complete = |tid: Pid, sysno: Sysno, args: [u64; 6]| match kernel.syscall(
            &host,
            tid,
            request(sysno, args),
        ) {
            Ok(Disposition::Complete(value)) => value,
            _ => panic!("{sysno} did not complete"),
        };
        let managed = |tid: Pid, sysno: Sysno, args: [u64; 6]| {
            let result = kernel.syscall(&host, tid, request(sysno, args));
            assert!(matches!(result, Ok(Disposition::ContextManaged)), "{sysno}");
        };

        kernel.poke(root, BASE, b"/bin/narf\0");
        assert_eq!(complete(root, Sysno::getpid, NONE), 1000);
        // The fake kernel has no getuid.
        assert_eq!(complete(root, Sysno::getuid, NONE), -38);
        assert_eq!(
            complete(root, Sysno::write, [1, BASE as u64, 4, 0, 0, 0]),
            4
        );
        assert_eq!(complete(root, Sysno::execve, NONE), -14);
        managed(root, Sysno::execve, [BASE as u64, 0, 0, 0, 0, 0]);
        let child = Pid::from_raw(complete(root, Sysno::fork, NONE) as i32);
        managed(child, Sysno::read, [PIPE_FD, BASE as u64, 4, 0, 0, 0]);
        kernel.push_pipe(b"ok");
        let reexecuted = kernel.reexecute(&host, child);
        assert!(matches!(reexecuted, Ok(Disposition::Complete(2))));
        managed(child, Sysno::exit_group, [3, 0, 0, 0, 0, 0]);
        managed(root, Sysno::exit_group, NONE);

        assert_eq!(
            take_lines(),
            [
                "[pid 1000] getpid() = 1000",
                "[pid 1000] getuid() = -38",
                "[pid 1000] write(1, 0x100000, 4) = 4",
                "[pid 1000] execve(NULL, NULL, NULL)",
                "[pid 1000] (execve) = EFAULT",
                "[pid 1000] execve(0x100000 -> \"/bin/narf\", NULL, NULL)",
                "[pid 1000] fork() = 1001",
                "[pid 1001] read(3, 0x100000, 4) = 2",
                "[pid 1001] exit_group(3) = ?",
                "Thread 1001 exited with status Exited(3)",
                "Process 1001 exited with status Exited(3)",
                "[pid 1000] exit_group(0) = ?",
                "Thread 1000 exited with status Exited(0)",
                "Process 1000 exited with status Exited(0)",
            ]
        );
        assert_eq!(kernel.output(), b"/bin");
        assert_eq!(kernel.peek(child, BASE, 4), b"okin");
        assert!(kernel.violations().is_empty());
        assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
    }

    /// chaos fails every other read of each thread with `EINTR` without
    /// running it, and runs the reads in between, and every receive, cut to
    /// one byte. It prints every other syscall before running it unchanged.
    /// Each thread's reads alternate on their own: a new thread and a forked
    /// child start with a read that fails, though each is created while its
    /// parent's next read would run. The count is the process's: a thread
    /// continues it and a forked child, with a Tool of its own, starts again
    /// at 0. A parked read prints once, at re-execution, where the one-byte
    /// read runs again, not the guest's.
    #[test]
    fn chaos_runs_on_the_narf_core() {
        reverie_narf_tools::set_eprintln_sink(record_line);
        let host = FakeHost::<ChaosTool>::new(ChaosOpts::default()).expect("host");
        let kernel = FakeKernel::new();
        let root = kernel.spawn_root(&host, BASE);
        let complete = |tid: Pid, sysno: Sysno, args: [u64; 6]| match kernel.syscall(
            &host,
            tid,
            request(sysno, args),
        ) {
            Ok(Disposition::Complete(value)) => value,
            _ => panic!("{sysno} did not complete"),
        };
        let managed = |tid: Pid, sysno: Sysno, args: [u64; 6]| {
            let result = kernel.syscall(&host, tid, request(sysno, args));
            assert!(matches!(result, Ok(Disposition::ContextManaged)), "{sysno}");
        };
        let read_at = |at: usize| [PIPE_FD, at as u64, 8, 0, 0, 0];

        // A reader retries a read that failed with EINTR and moves its buffer
        // on after a short one.
        kernel.push_pipe(b"hi");
        assert_eq!(complete(root, Sysno::read, read_at(BASE)), -4);
        assert_eq!(complete(root, Sysno::read, read_at(BASE)), 1);
        assert_eq!(complete(root, Sysno::read, read_at(BASE + 1)), -4);
        assert_eq!(complete(root, Sysno::read, read_at(BASE + 1)), 1);
        assert_eq!(complete(root, Sysno::getpid, NONE), 1000);
        kernel.push_pipe(b"ab");
        assert_eq!(complete(root, Sysno::read, read_at(BASE + 2)), -4);
        let thread = (libc::CLONE_THREAD | libc::CLONE_VM) as u64;
        let thread = Pid::from_raw(complete(root, Sysno::clone, [thread, 0, 0, 0, 0, 0]) as i32);
        assert_eq!(complete(thread, Sysno::read, read_at(BASE + 3)), -4);
        assert_eq!(complete(root, Sysno::read, read_at(BASE + 2)), 1);
        assert_eq!(complete(thread, Sysno::read, read_at(BASE + 3)), 1);
        // The fake kernel has no recvfrom.
        let recv = [4, BASE as u64, 8, 0, 0, 0];
        assert_eq!(complete(root, Sysno::recvfrom, recv), -38);
        assert_eq!(complete(root, Sysno::read, read_at(BASE + 4)), -4);
        let child = Pid::from_raw(complete(root, Sysno::fork, NONE) as i32);
        assert_eq!(complete(child, Sysno::getppid, NONE), 1000);
        assert_eq!(complete(child, Sysno::read, read_at(BASE + 4)), -4);
        managed(child, Sysno::read, read_at(BASE + 4));
        kernel.push_pipe(b"ok");
        let reexecuted = kernel.reexecute(&host, child);
        assert!(matches!(reexecuted, Ok(Disposition::Complete(1))));
        assert_eq!(complete(child, Sysno::read, read_at(BASE + 5)), -4);
        assert_eq!(complete(child, Sysno::read, read_at(BASE + 5)), 1);
        managed(thread, Sysno::exit, NONE);
        managed(child, Sysno::exit_group, [3, 0, 0, 0, 0, 0]);
        managed(root, Sysno::exit_group, NONE);

        assert_eq!(
            take_lines(),
            [
                "[pid=1000, n=0] read(3, 0x100000, 8) = -4",
                "[pid=1000, n=1] read(3, 0x100000, 1) = 1",
                "[pid=1000, n=2] read(3, 0x100001, 8) = -4",
                "[pid=1000, n=3] read(3, 0x100001, 1) = 1",
                "[pid=1000, n=4] getpid()",
                "[pid=1000, n=5] read(3, 0x100002, 8) = -4",
                "[pid=1000, n=6] clone(CloneFlags(CLONE_VM | CLONE_THREAD), NULL, NULL, NULL, 0)",
                "[pid=1000, n=7] read(3, 0x100003, 8) = -4",
                "[pid=1000, n=8] read(3, 0x100002, 1) = 1",
                "[pid=1000, n=9] read(3, 0x100003, 1) = 1",
                "[pid=1000, n=10] recvfrom(4, 0x100000, 1, 0, NULL, NULL) = -38",
                "[pid=1000, n=11] read(3, 0x100004, 8) = -4",
                "[pid=1000, n=12] fork()",
                "[pid=1002, n=0] getppid()",
                "[pid=1002, n=1] read(3, 0x100004, 8) = -4",
                "[pid=1002, n=2] read(3, 0x100004, 1) = 1",
                "[pid=1002, n=3] read(3, 0x100005, 8) = -4",
                "[pid=1002, n=4] read(3, 0x100005, 1) = 1",
                "[pid=1000, n=13] exit(0)",
                "[pid=1002, n=5] exit_group(3)",
                "[pid=1000, n=14] exit_group(0)",
            ]
        );
        assert_eq!(kernel.peek(root, BASE, 6), b"hiab\0\0");
        assert_eq!(kernel.peek(child, BASE, 6), b"hiabok");
        // No read that failed with EINTR ran.
        let (read, original, injected) = (Some(Sysno::read), Via::Original, Via::Injected);
        assert_eq!(
            natives(&kernel),
            [
                (1000, read, 1, injected),
                (1000, read, 1, injected),
                (1000, Some(Sysno::getpid), 0, original),
                (1000, Some(Sysno::clone), 0, original),
                (1000, read, 1, injected),
                (1001, read, 1, injected),
                (1000, Some(Sysno::recvfrom), 1, injected),
                (1000, Some(Sysno::fork), 0, original),
                (1002, Some(Sysno::getppid), 0, original),
                (1002, read, 1, injected),
                (1002, read, 1, injected),
                (1002, read, 1, injected),
                (1001, Some(Sysno::exit), 0, original),
                (1002, Some(Sysno::exit_group), 0, original),
                (1000, Some(Sysno::exit_group), 0, original),
            ]
        );
        assert!(kernel.violations().is_empty());
        assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
    }

    /// chaos reads its options through `guest.config()`, which is the
    /// configuration the host was built with: here its first syscall runs
    /// unchanged, a read is cut without an `EINTR` first and a receive runs
    /// as it came.
    #[test]
    fn chaos_takes_its_options_from_the_host() {
        reverie_narf_tools::set_eprintln_sink(record_line);
        let options = ChaosOpts {
            skip: 1,
            no_read: false,
            no_recv: true,
            no_interrupt: true,
        };
        let host = FakeHost::<ChaosTool>::new(options).expect("host");
        let kernel = FakeKernel::new();
        let root = kernel.spawn_root(&host, BASE);
        let complete = |tid: Pid, sysno: Sysno, args: [u64; 6]| match kernel.syscall(
            &host,
            tid,
            request(sysno, args),
        ) {
            Ok(Disposition::Complete(value)) => value,
            _ => panic!("{sysno} did not complete"),
        };

        kernel.push_pipe(b"narf");
        let read = [PIPE_FD, BASE as u64, 2, 0, 0, 0];
        assert_eq!(complete(root, Sysno::read, read), 2);
        let read = [PIPE_FD, BASE as u64 + 2, 2, 0, 0, 0];
        assert_eq!(complete(root, Sysno::read, read), 1);
        let recv = [4, BASE as u64, 8, 0, 0, 0];
        assert_eq!(complete(root, Sysno::recvfrom, recv), -38);
        let result = kernel.syscall(&host, root, request(Sysno::exit_group, NONE));
        assert!(matches!(result, Ok(Disposition::ContextManaged)));

        assert_eq!(
            take_lines(),
            [
                "SKIPPED [pid=1000, n=0] read(3, 0x100000, 2)",
                "[pid=1000, n=1] read(3, 0x100002, 1) = 1",
                "[pid=1000, n=2] recvfrom(4, 0x100000, 8, 0, NULL, NULL)",
                "[pid=1000, n=3] exit_group(0)",
            ]
        );
        assert_eq!(kernel.peek(root, BASE, 4), b"nar\0");
        let (original, injected) = (Via::Original, Via::Injected);
        assert_eq!(
            natives(&kernel),
            [
                (1000, Some(Sysno::read), 2, original),
                (1000, Some(Sysno::read), 1, injected),
                (1000, Some(Sysno::recvfrom), 8, original),
                (1000, Some(Sysno::exit_group), 0, original),
            ]
        );
        assert!(kernel.violations().is_empty());
        assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
    }
}
