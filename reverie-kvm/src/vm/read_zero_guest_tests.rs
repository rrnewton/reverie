/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included inside vm::tests to reuse its private minimal ELF builder.
mod read_zero_guest_tests {
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::sync::mpsc;
    use std::time::Duration;

    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::Tool;
    use reverie::syscalls::Syscall;

    use super::*;

    const STOP: i32 = 37;
    const RETURNED_READ: u32 = 99;
    const HIGH_FD: u64 = 0x5a5a_5a5a_0000_0000;
    const ADDRESS: u64 = 0x100;
    const WAIT: Duration = Duration::from_secs(30);

    #[derive(Default)]
    struct ForwardLog {
        requests: Mutex<Vec<SyscallRequest>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for ForwardLog {
        type Request = (u64, [u64; 6]);
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _: Pid, (number, arguments): Self::Request) {
            self.requests
                .lock()
                .unwrap()
                .push(SyscallRequest::new(number, arguments));
        }
    }

    #[derive(Default)]
    struct ForwardTool;

    #[reverie::tool]
    impl Tool for ForwardTool {
        type GlobalState = ForwardLog;
        type ThreadState = ();

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> std::result::Result<i64, reverie::Error> {
            let request = SyscallRequest::from_syscall(syscall);
            // Record before injection: the terminal read must never return to
            // this callback or the guest. Recording only afterwards misses it.
            guest.send_rpc((request.number(), *request.args())).await;
            if matches!(syscall, Syscall::Exit(_) | Syscall::ExitGroup(_)) {
                guest.tail_inject(syscall).await
            }
            Ok(guest.inject(syscall).await?)
        }
    }

    fn guest(rebound: bool) -> (Vec<u8>, SyscallRequest) {
        let mut code = Vec::new();
        let mut failures = Vec::new();
        // Clear all six argument registers, then dup(0). The guest verifies
        // the actual descriptor result before reaching the selected read.
        code.extend_from_slice(&[
            0x31, 0xff, // xor edi,edi
            0x31, 0xf6, // xor esi,esi
            0x31, 0xd2, // xor edx,edx
            0x45, 0x31, 0xd2, // xor r10d,r10d
            0x45, 0x31, 0xc0, // xor r8d,r8d
            0x45, 0x31, 0xc9, // xor r9d,r9d
            0xb8, 32, 0, 0, 0, // mov eax,SYS_dup
            0x0f, 0x05, // syscall
            0x83, 0xf8, 3, // cmp eax,3
            0x0f, 0x85, 0, 0, 0, 0, // jne failure
        ]);
        failures.push(code.len() - 4);
        if rebound {
            code.extend_from_slice(&[
                0xb8, 33, 0, 0, 0, // mov eax,SYS_dup2
                0xbf, 3, 0, 0, 0, // mov edi,3
                0x31, 0xf6, // xor esi,esi
                0x0f, 0x05, // syscall
                0x85, 0xc0, // test eax,eax
                0x0f, 0x85, 0, 0, 0, 0, // jne failure
            ]);
            failures.push(code.len() - 4);
            code.extend_from_slice(&[
                0xb8, 3, 0, 0, 0, // mov eax,SYS_close
                0xbf, 3, 0, 0, 0, // mov edi,3
                0x0f, 0x05, // syscall
                0x85, 0xc0, // test eax,eax
                0x0f, 0x85, 0, 0, 0, 0, // jne failure
            ]);
            failures.push(code.len() - 4);
        }
        let fd = HIGH_FD | if rebound { 0 } else { 3 };
        code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi,fd
        code.extend_from_slice(&fd.to_le_bytes());
        code.push(0xbe); // mov esi,address
        code.extend_from_slice(&(ADDRESS as u32).to_le_bytes());
        code.extend_from_slice(&[
            0x31, 0xd2, // xor edx,edx: literal zero count
            0x45, 0x31, 0xd2, // xor r10d,r10d
            0x45, 0x31, 0xc0, // xor r8d,r8d
            0x45, 0x31, 0xc9, // xor r9d,r9d
            0x31, 0xc0, // xor eax,eax: SYS_read
            0x0f, 0x05, // syscall
        ]);
        // Every returned read, even a fabricated zero/EINTR, is a failure.
        let failure = code.len();
        code.extend_from_slice(&[0xb8, 231, 0, 0, 0, 0xbf]); // exit_group(99)
        code.extend_from_slice(&RETURNED_READ.to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x0f, 0x0b]); // syscall; ud2
        for operand in failures {
            let displacement = i32::try_from(failure - (operand + 4)).unwrap();
            code[operand..operand + 4].copy_from_slice(&displacement.to_le_bytes());
        }
        (
            minimal_test_elf(&code),
            SyscallRequest::new(libc::SYS_read as u64, [fd, ADDRESS, 0, 0, 0, 0]),
        )
    }

    type Outcome = Result<(i32, Vec<SyscallRequest>)>;

    struct GuestRun {
        group: Arc<GuestThreadGroup>,
        thread: Option<std::thread::JoinHandle<(KvmBackend, Outcome)>>,
    }

    impl Drop for GuestRun {
        fn drop(&mut self) {
            if let Some(thread) = self.thread.take() {
                // A failed observation must not detach the running guest. This
                // requests real cleanup, then keeps its owner through join.
                // If that cleanup itself regresses, the outer bounded test
                // process remains the containment boundary; no timeout here
                // is relabelled as proof of retirement.
                self.group.request_exit_group(ExitStatus::Exited(255));
                let joined = thread.join();
                if !std::thread::panicking() {
                    assert!(joined.is_ok(), "guest cleanup thread panicked");
                }
            }
        }
    }

    fn run_case(tool: bool, rebound: bool) {
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        assert!(raw >= 0, "create blocking inherited inotify");
        // SAFETY: inotify_init1 returned this test's owned descriptor.
        let endpoint = unsafe { File::from_raw_fd(raw) };
        let status_flags = unsafe { libc::fcntl(endpoint.as_raw_fd(), libc::F_GETFL) };
        assert!(status_flags >= 0);
        assert_eq!(status_flags & libc::O_NONBLOCK, 0);
        let mut backend = KvmBackend::new_with_stdin(16 * 1024 * 1024, Some(endpoint))
            .expect("real zero-read guest requires /dev/kvm");
        let (image, request) = guest(rebound);
        backend
            .install_static_elf(&image, "/bin/read-zero-alias")
            .unwrap();
        let loaded = backend.static_elf.as_ref().unwrap();
        let lifecycle = loaded
            .task_lifecycle
            .lock()
            .unwrap()
            .get(loaded.tid)
            .expect("installed root has its exact task generation");
        let task = reverie::SignalTaskIdentity {
            process: reverie::SignalProcessId {
                tgid: Pid::from_raw(loaded.pid),
                generation: lifecycle.process_generation,
            },
            tid: Pid::from_raw(loaded.tid),
            task_generation: lifecycle.generation,
        };
        backend.set_backend_stats_request(BackendStatsRequest::new(true));
        let exits = backend.exit_collector.as_ref().unwrap().clone();
        let group = backend.thread_group.clone();
        let registry = group.terminal_reads.clone();
        let (finished, completion) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = if tool {
                futures::executor::block_on(
                    backend.run_static_elf_with_tool::<ForwardTool>((), true),
                )
                .map(|(global, status, stdout, stderr)| {
                    assert!(stdout.is_empty() && stderr.is_empty());
                    (status, global.requests.into_inner().unwrap())
                })
            } else {
                backend.run_static_elf().map(|status| (status, Vec::new()))
            };
            let _ = finished.send(());
            (backend, result)
        });
        let mut run = GuestRun {
            group: group.clone(),
            thread: Some(thread),
        };
        let observed = registry.observe_registered_test_read(task, request, 0, status_flags);
        assert!(matches!(
            completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        // This is deliberately after the exact kernel read witness, not after
        // a delay or merely after registration/handle publication.
        group.request_exit_group(ExitStatus::Exited(STOP));
        completion
            .recv_timeout(WAIT)
            .expect("terminal group exit did not return the real guest run");
        let (backend, result) = run.thread.take().unwrap().join().unwrap();
        let (status, forwarded) = result.unwrap();
        assert_eq!(status, STOP, "the guest read returned to its failure exit");
        observed.assert_retired(&registry);
        assert!(!group.has_worker_handles());
        backend.guest_worker_teardown_result().unwrap();
        assert_eq!(
            exits
                .snapshot()
                .count(crate::stats::KvmExitReason::Hypercall),
            if rebound { 4 } else { 2 },
            "guest must execute dup[/dup2/close]/read and never the exit(99)"
        );
        if tool {
            let expected = if rebound {
                vec![
                    libc::SYS_dup,
                    libc::SYS_dup2,
                    libc::SYS_close,
                    libc::SYS_read,
                ]
            } else {
                vec![libc::SYS_dup, libc::SYS_read]
            };
            assert_eq!(
                forwarded
                    .iter()
                    .map(|call| call.number() as i64)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(forwarded.last(), Some(&request));
        } else {
            assert!(forwarded.is_empty());
        }
        println!("ZERO_READ_GUEST_RETIRED tool={tool} rebound={rebound} status={status}");
        drop(backend);
    }

    #[test]
    fn host_dup_zero_read_blocks_before_terminal_retirement() {
        run_case(false, false);
    }

    #[test]
    fn host_rebound_zero_read_blocks_before_terminal_retirement() {
        run_case(false, true);
    }

    #[test]
    fn tool_dup_zero_read_blocks_before_terminal_retirement() {
        run_case(true, false);
    }

    #[test]
    fn tool_rebound_zero_read_blocks_before_terminal_retirement() {
        run_case(true, true);
    }
}
