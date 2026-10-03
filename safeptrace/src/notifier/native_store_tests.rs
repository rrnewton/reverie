/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license found in the LICENSE file. */
mod native_store_tests {
    use reverie_memory::Addr;
    use reverie_memory::MemoryAccess;
    use reverie_memory::NativeUserReadRefusal as E;
    use reverie_memory::NativeUserStoreOutcome as O;
    use reverie_memory::NativeUserStoreRefusal as R;

    use super::*;

    struct Mapping(*mut libc::c_void);
    impl Mapping {
        fn new(shared: bool, protection: i32) -> Self {
            let flags = libc::MAP_ANONYMOUS
                | if shared {
                    libc::MAP_SHARED
                } else {
                    libc::MAP_PRIVATE
                };
            let address = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    8192,
                    libc::PROT_READ | libc::PROT_WRITE,
                    flags,
                    -1,
                    0,
                )
            };
            assert_ne!(address, libc::MAP_FAILED);
            unsafe {
                std::ptr::write_bytes(address.cast::<u8>(), 0xa5, 8192);
            }
            assert_eq!(unsafe { libc::mprotect(address, 8192, protection) }, 0);
            Self(address)
        }
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            assert_eq!(unsafe { libc::munmap(self.0, 8192) }, 0);
        }
    }
    struct HookReset;
    impl Drop for HookReset {
        fn drop(&mut self) {
            NativeStorePermit::store_hook_for_test(None);
        }
    }

    #[test]
    fn actual_store_writes_once_preserves_canaries_and_keeps_old_controls_blocked() {
        let mapping = Mapping::new(false, libc::PROT_READ | libc::PROT_WRITE);
        let (_cleanup, stopped) = child_stop();
        let control = stopped.control_stop().unwrap();
        let hold = control.hold().unwrap();
        NativeStorePermit::store_count_for_test();
        let permit = hold.begin_native_store().unwrap();
        assert!(matches!(hold.begin_native_store(), Err(Errno::EBUSY)));
        assert!(matches!(hold.begin_register_capture(), Err(Errno::EBUSY)));
        assert_eq!(
            probe_held_controls_for_store(&stopped, mapping.0 as usize),
            [true; 3]
        );
        let result = permit.write(mapping.0 as usize + 1, b"abcd");
        let duplicate = permit.write(mapping.0 as usize + 1, b"WXYZ");
        assert_eq!(
            result,
            O::Attempted {
                raw: Ok(4),
                postcheck: Ok(())
            }
        );
        assert_eq!(
            duplicate,
            O::Refused(R::Evidence(E::TargetState(Errno::EALREADY)))
        );
        assert_eq!(NativeStorePermit::store_count_for_test(), 1);
        let mut bytes = [0; 6];
        stopped
            .read_exact(Addr::from_raw(mapping.0 as usize).unwrap(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, [0xa5, b'a', b'b', b'c', b'd', 0xa5]);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(mapping.0.cast::<u8>(), 6) },
            [0xa5; 6]
        );
        permit.validate().unwrap();
        drop(permit);
        assert!(hold.begin_register_capture().is_ok());
        drop(hold);
        finish(stopped);
    }

    // Test-only ordinary aliases, using the same original Event and requiring
    // real old control admissions to refuse, not a new ticket Boolean.
    fn probe_held_controls_for_store(original: &Stopped, address: usize) -> [bool; 3] {
        let mut stopped = Stopped::from_token(
            original.pid(),
            TraceeToken::from_event(original.1.event().clone()),
        );
        let registers = stopped.getregs().unwrap();
        [
            matches!(
                stopped.setregs(&registers),
                Err(crate::Error::Errno(Errno::EBUSY))
            ),
            stopped.write_exact(reverie_memory::AddrMut::from_raw(address).unwrap(), b"x")
                == Err(Errno::EBUSY),
            stopped.terminal_cleanup().continue_for_cleanup() == Err(Errno::EBUSY),
        ]
    }

    #[test]
    fn actual_store_refuses_readonly_shared_empty_and_cross_page_before_transfer() {
        for (shared, protection, offset, length, expected) in [
            (false, libc::PROT_READ, 0, 4, R::WriteDenied),
            (
                true,
                libc::PROT_READ | libc::PROT_WRITE,
                0,
                4,
                R::Evidence(E::UnsupportedBacking),
            ),
            (
                false,
                libc::PROT_READ | libc::PROT_WRITE,
                0,
                0,
                R::Evidence(E::UnsupportedRange),
            ),
            (
                false,
                libc::PROT_READ | libc::PROT_WRITE,
                4094,
                4,
                R::Evidence(E::UnsupportedRange),
            ),
        ] {
            let mapping = Mapping::new(shared, protection);
            let (_cleanup, stopped) = child_stop();
            let control = stopped.control_stop().unwrap();
            let hold = control.hold().unwrap();
            let permit = hold.begin_native_store().unwrap();
            NativeStorePermit::store_count_for_test();
            assert_eq!(
                permit.write(mapping.0 as usize + offset, &b"abcd"[..length]),
                O::Refused(expected)
            );
            assert_eq!(NativeStorePermit::store_count_for_test(), 0);
            let mut bytes = [0; 4];
            stopped
                .read_exact(
                    Addr::from_raw(mapping.0 as usize + offset).unwrap(),
                    &mut bytes,
                )
                .unwrap();
            assert_eq!(bytes, [0xa5; 4]);
            drop(permit);
            drop(hold);
            finish(stopped);
        }
    }

    #[test]
    fn actual_store_keeps_raw_result_when_external_terminal_postcheck_fails() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mapping = Mapping::new(false, libc::PROT_READ | libc::PROT_WRITE);
        let (cleanup, stopped) = child_stop();
        let control = stopped.control_stop().unwrap();
        let hold = control.hold().unwrap();
        let permit = hold.begin_native_store().unwrap();
        let event = Arc::clone(stopped.1.event().event());
        let (send, receive) = mpsc::channel();
        *event.capture_consume_observer.lock() = Some(send);
        let original = cleanup.0.duplicate_bound_thread_pidfd().unwrap();
        NativeStorePermit::store_count_for_test();
        NativeStorePermit::store_hook_for_test(Some(Box::new(move || {
            Errno::result(unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    original.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            })
            .unwrap();
            let observed = receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            assert!(
                matches!(
                    observed,
                    CaptureConsumeObservation::Waiting { mutation: false }
                ),
                "actual notifier must wait on the distinct held-store ticket: {observed:?}"
            );
        })));
        let reset = HookReset;
        let result = permit.write(mapping.0 as usize, b"abcd");
        drop(reset);
        assert_eq!(
            result,
            O::Attempted {
                raw: Ok(4),
                postcheck: Err(Errno::ESTALE)
            }
        );
        assert_eq!(NativeStorePermit::store_count_for_test(), 1);
        assert_eq!(permit.validate(), Err(Errno::ESTALE));
        drop(permit);
        drop(hold);
        *event.capture_consume_observer.lock() = None;
        cleanup.cleanup().unwrap();
        assert!(Instant::now() < deadline);
    }
    #[test]
    fn actual_store_checks_target_nonzero_pkey_write_disable_and_access_disable() {
        struct Key(i32);
        impl Drop for Key {
            fn drop(&mut self) {
                assert_eq!(unsafe { libc::syscall(libc::SYS_pkey_free, self.0) }, 0);
            }
        }
        struct Rights(u32);
        impl Drop for Rights {
            fn drop(&mut self) {
                unsafe {
                    core::arch::asm!("wrpkru", in("eax") self.0,
                    in("ecx") 0u32, in("edx") 0u32, options(nostack, preserves_flags));
                }
            }
        }
        for bits in [2u32, 1, 3] {
            let key = unsafe { libc::syscall(libc::SYS_pkey_alloc, 0usize, 0usize) } as i32;
            assert!(
                (1..16).contains(&key),
                "actual nonzero protection key required"
            );
            let key = Key(key);
            let mapping = Mapping::new(false, libc::PROT_READ | libc::PROT_WRITE);
            assert_eq!(
                unsafe {
                    libc::syscall(
                        libc::SYS_pkey_mprotect,
                        mapping.0,
                        8192usize,
                        libc::PROT_READ | libc::PROT_WRITE,
                        key.0,
                    )
                },
                0
            );
            let saved: u32;
            unsafe {
                core::arch::asm!("rdpkru", out("eax") saved, in("ecx") 0u32,
                out("edx") _, options(nostack, preserves_flags));
            }
            let reset = Rights(saved);
            let target = (saved & !(3 << (2 * key.0))) | (bits << (2 * key.0));
            unsafe {
                core::arch::asm!("wrpkru", in("eax") target, in("ecx") 0u32,
                in("edx") 0u32, options(nostack, preserves_flags));
            }
            // fork inherits this real thread's PKRU; the tracer restores its
            // own rights before observing the still-stopped original child.
            let (_cleanup, stopped) = child_stop();
            drop(reset);
            let control = stopped.control_stop().unwrap();
            let hold = control.hold().unwrap();
            let permit = hold.begin_native_store().unwrap();
            NativeStorePermit::store_count_for_test();
            assert_eq!(
                permit.write(mapping.0 as usize, b"abcd"),
                O::Refused(R::ProtectionKey(key.0 as u8))
            );
            assert_eq!(NativeStorePermit::store_count_for_test(), 0);
            let mut bytes = [0; 4];
            stopped
                .read_exact(Addr::from_raw(mapping.0 as usize).unwrap(), &mut bytes)
                .unwrap();
            assert_eq!(bytes, [0xa5; 4]);
            drop(permit);
            drop(hold);
            finish(stopped);
        }
    }
}
