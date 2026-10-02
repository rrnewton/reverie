/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included in executor::tests so the native oracle and memory canaries remain
// identical to the admitted zero-read tests. Refusal is a backend error, not
// claimed equivalence to the native result of the unsupported operation.
mod read_zero_refusal_tests {
    use super::*;

    fn external_cases(nonblocking: bool) {
        for socket in [false, true] {
            for inherited in [false, true] {
                for with_context in [false, true] {
                    let (file, _peer) = if socket {
                        let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
                        stream.set_nonblocking(nonblocking).unwrap();
                        let owned: std::os::fd::OwnedFd = stream.into();
                        (std::fs::File::from(owned), Some(peer))
                    } else {
                        let raw = unsafe {
                            libc::inotify_init1(
                                libc::IN_CLOEXEC
                                    | if nonblocking { libc::IN_NONBLOCK } else { 0 },
                            )
                        };
                        assert!(raw >= 0);
                        (unsafe { std::fs::File::from_raw_fd(raw) }, None)
                    };
                    let raw = file.as_raw_fd();
                    let flags = file_status_flags(&file).unwrap();
                    assert_eq!(flags & libc::O_NONBLOCK != 0, nonblocking);
                    let mut state = test_state(&std::env::current_dir().unwrap());
                    let fd = if inherited {
                        state.stdin = Some(file);
                        0
                    } else {
                        state.files.insert(3, file);
                        3
                    };
                    let entries = state.fd_entry_ids.clone();
                    let mut memory = read_zero_memory();
                    let registry = Arc::new(crate::terminal_read::ReadRegistry::default());
                    for high in [0, READ_ZERO_HIGH_FD] {
                        for address in [
                            0,
                            READ_ZERO_BUFFER,
                            READ_ZERO_PROTECTED,
                            2 * PAGE_SIZE,
                            X86_64_GUEST_USER_LIMIT,
                        ] {
                            let request = SyscallRequest::new(
                                libc::SYS_read as u64,
                                [high | fd as u64, address, 0, 0, 0, 0],
                            );
                            let mut context = read_zero_terminal_context(&memory, registry.clone());
                            let action = execute_basic_syscall_with_read_context(
                                &mut memory,
                                &mut state,
                                &request,
                                None,
                                None,
                                with_context.then_some((&mut context, read_zero_task())),
                            );
                            assert!(
                                matches!(
                                    action,
                                    SyscallAction::Failure(
                                        crate::Error::PotentiallyBlockingZeroRead { fd: actual }
                                    ) if actual == fd
                                ),
                                "expected named refusal: socket={socket} inherited={inherited} context={with_context} nonblocking={nonblocking} address={address:#x}"
                            );
                            registry.assert_no_test_read_started();
                            registry.teardown_result().unwrap();
                            let retained = if inherited {
                                state.stdin.as_ref().unwrap()
                            } else {
                                state.files.get(&3).unwrap()
                            };
                            assert_eq!(retained.as_raw_fd(), raw);
                            assert_eq!(file_status_flags(retained).unwrap(), flags);
                            assert_eq!(state.fd_entry_ids, entries);
                            assert_read_zero_canaries(&memory);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn read_zero_blocking_external_endpoints_are_refused_before_reader_creation() {
        external_cases(false);
    }

    #[test]
    fn read_zero_nonblocking_flag_does_not_admit_external_endpoint() {
        // The flag is mutable through outside aliases. A guard-removal mutant
        // can use this exact test without starting a deliberately blocking read.
        external_cases(true);
    }

    #[test]
    fn read_zero_external_invalid_address_retains_native_efault() {
        for inherited in [false, true] {
            let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
            assert!(raw >= 0);
            let file = unsafe { std::fs::File::from_raw_fd(raw) };
            let mut state = test_state(&std::env::current_dir().unwrap());
            let fd = if inherited {
                state.stdin = Some(file);
                0
            } else {
                state.files.insert(3, file);
                3
            };
            let mut memory = read_zero_memory();
            for address in [X86_64_GUEST_USER_LIMIT + 1, u64::MAX] {
                // The host's architecture may allow addresses above the guest
                // ceiling. UINTPTR_MAX is rejected on both; the guest boundary
                // expectation remains the existing four-level address policy.
                assert_eq!(native_read_zero(raw as u64, u64::MAX), negative_errno(libc::EFAULT));
                assert_eq!(
                    syscall_result(
                        &mut memory,
                        &mut state,
                        libc::SYS_read,
                        [fd, address, 0, 0, 0, 0],
                    ),
                    negative_errno(libc::EFAULT)
                );
                assert_read_zero_canaries(&memory);
            }
        }
    }

    #[test]
    fn read_zero_intrinsic_nonwaiting_endpoints_keep_native_results_with_blocking_flags() {
        for kind in ["pipe", "eventfd", "timerfd", "signalfd", "null", "zero"] {
            let (file, _writer) = match kind {
                "pipe" => {
                    let mut fds = [-1; 2];
                    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
                    unsafe {
                        (std::fs::File::from_raw_fd(fds[0]), Some(std::fs::File::from_raw_fd(fds[1])))
                    }
                }
                "null" | "zero" => (std::fs::File::open(format!("/dev/{kind}")).unwrap(), None),
                _ => {
                    let raw = match kind {
                        "eventfd" => unsafe { libc::eventfd(9, libc::EFD_CLOEXEC) },
                        "timerfd" => unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC) },
                        "signalfd" => {
                            let mut mask = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
                            assert_eq!(unsafe { libc::sigemptyset(mask.as_mut_ptr()) }, 0);
                            unsafe { libc::signalfd(-1, mask.as_ptr(), libc::SFD_CLOEXEC) }
                        }
                        _ => unreachable!(),
                    };
                    assert!(raw >= 0, "create {kind}");
                    (unsafe { std::fs::File::from_raw_fd(raw) }, None)
                }
            };
            let raw = file.as_raw_fd();
            assert_eq!(file_status_flags(&file).unwrap() & libc::O_NONBLOCK, 0);
            let mut state = test_state(&std::env::current_dir().unwrap());
            state.files.insert(3, file);
            let mut memory = read_zero_memory();
            for address in [0, READ_ZERO_PROTECTED, u64::MAX] {
                let native = native_read_zero(raw as u64, address);
                let expected = if address == u64::MAX {
                    negative_errno(libc::EFAULT)
                } else if matches!(kind, "eventfd" | "timerfd" | "signalfd") {
                    negative_errno(libc::EINVAL)
                } else {
                    0
                };
                assert_eq!(native, expected, "native {kind} address={address:#x}");
                assert_eq!(
                    syscall_result(&mut memory, &mut state, libc::SYS_read, [3, address, 0, 0, 0, 0]),
                    native,
                    "guest {kind} address={address:#x}"
                );
                assert_read_zero_canaries(&memory);
            }
            assert_eq!(fd_status_flags(raw).unwrap() & libc::O_NONBLOCK, 0);
            if kind == "eventfd" {
                let mut value = 0_u64;
                assert_eq!(unsafe { libc::read(raw, (&mut value as *mut u64).cast(), 8) }, 8);
                assert_eq!(value, 9, "zero-count calls consumed the counter");
            }
        }
    }

    #[test]
    fn read_zero_unknown_regular_filesystem_is_not_admitted_by_mode_or_bad_address() {
        // Procfs presents regular modes too. It is deliberately outside the
        // finite local-file admission set; a generic S_IFREG classification or
        // an EFAULT prediction cannot grant an unknown file-operation contract.
        let file = std::fs::File::open("/proc/self/status").unwrap();
        assert_eq!(file_mode(&file).unwrap() & libc::S_IFMT, libc::S_IFREG);
        let mut state = test_state(&std::env::current_dir().unwrap());
        state.files.insert(3, file);
        let mut memory = read_zero_memory();
        let registry = Arc::new(crate::terminal_read::ReadRegistry::default());
        for address in [0, READ_ZERO_BUFFER, u64::MAX] {
            let request = SyscallRequest::new(libc::SYS_read as u64, [3, address, 0, 0, 0, 0]);
            let mut context = read_zero_terminal_context(&memory, registry.clone());
            assert!(matches!(
                execute_basic_syscall_with_read_context(
                    &mut memory,
                    &mut state,
                    &request,
                    None,
                    None,
                    Some((&mut context, read_zero_task())),
                ),
                SyscallAction::Failure(crate::Error::PotentiallyBlockingZeroRead { fd: 3 })
            ));
            registry.assert_no_test_read_started();
            assert_read_zero_canaries(&memory);
        }
    }
}
