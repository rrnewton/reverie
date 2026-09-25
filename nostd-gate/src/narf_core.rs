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
//! exits that tear the tasks down.

#[path = "../../reverie-narf-core/tests/support/fake_kernel.rs"]
pub mod fake_kernel;

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use reverie::Pid;
    use reverie::syscalls::libc;
    use reverie_narf_core::Disposition;
    use reverie_narf_core::TaskExit;
    use syscalls::Sysno;

    use super::fake_kernel::FakeHost;
    use super::fake_kernel::FakeKernel;
    use super::fake_kernel::PIPE_FD;
    use super::fake_kernel::Via;
    use super::fake_kernel::request;
    use crate::tool_contract::counter1_tool::CounterLocal;

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
}
