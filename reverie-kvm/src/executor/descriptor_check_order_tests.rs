/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included in executor::tests so the guest memory fixture and canaries are the
// ones the zero-read tests use.
mod descriptor_check_order_tests {
    use std::ffi::CStr;

    use super::*;

    fn open_fixture(path: &CStr, flags: libc::c_int) -> std::fs::File {
        // SAFETY: path is terminated and a successful open returns a new fd.
        let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC) };
        assert!(fd >= 0, "open {path:?}");
        // SAFETY: the descriptor was just opened and is owned here.
        unsafe { std::fs::File::from_raw_fd(fd) }
    }

    fn epoll_fixture() -> std::fs::File {
        // SAFETY: epoll_create1 returns a new descriptor on success.
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        assert!(fd >= 0);
        // SAFETY: the descriptor was just created and is owned here.
        unsafe { std::fs::File::from_raw_fd(fd) }
    }

    /// An O_PATH handle to an inotify instance, reopened through its proc
    /// link: the access check must refuse it before any inotify handling.
    fn inotify_path_fixture() -> std::fs::File {
        // SAFETY: inotify_init1 returns a new descriptor on success.
        let inotify = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        assert!(inotify >= 0);
        // SAFETY: the descriptor was just created and is owned here.
        let inotify = unsafe { std::fs::File::from_raw_fd(inotify) };
        let link = std::ffi::CString::new(format!("/proc/self/fd/{}", inotify.as_raw_fd())).unwrap();
        open_fixture(&link, libc::O_PATH)
    }

    /// A scalar read whose destination is writable skips the descriptor
    /// checks after the access mode and lets the host read report the
    /// descriptor's error; an O_PATH or write-only descriptor and a
    /// directory keep the checks. For every
    /// descriptor the checks refuse, every destination (the writable one that
    /// takes the unchecked path, and each invalid one that keeps the checks)
    /// must give the error the checks give.
    #[test]
    fn scalar_read_descriptor_errors_do_not_depend_on_the_destination() {
        let directory = open_fixture(c".", libc::O_RDONLY | libc::O_DIRECTORY);
        assert_read_zero_fixture_filesystem(&directory);
        for (name, file) in [
            ("directory", directory),
            ("path", open_fixture(c".", libc::O_PATH)),
            ("write-only", open_fixture(c"/dev/null", libc::O_WRONLY)),
            ("epoll", epoll_fixture()),
            ("inotify-path", inotify_path_fixture()),
        ] {
            let expected = ensure_read_capable(&file)
                .and_then(|()| ensure_readable(&file))
                .expect_err("each fixture is refused by the descriptor checks");
            let mut state = test_state(&std::env::current_dir().unwrap());
            state.files.insert(3, file);
            let mut memory = read_zero_memory();
            for address in READ_ZERO_ADDRESSES {
                // Linux checks the address (access_ok) before a directory's
                // read op, so it answers EFAULT for these two noncanonical
                // addresses where the checked path answers EISDIR. That
                // existing difference is not this test's subject, and the
                // test does not pin either answer.
                if name == "directory" && address >= 0x8000_0000_0000_0000 {
                    continue;
                }
                assert_eq!(
                    syscall_result(
                        &mut memory,
                        &mut state,
                        libc::SYS_read,
                        [3, address, 16, 0, 0, 0],
                    ),
                    expected,
                    "{name} address={address:#x}"
                );
                assert_read_zero_canaries(&memory);
            }
        }
    }

    /// The skipped zero-iovec readv probe was itself an access: Linux raises
    /// IN_ACCESS for it. A read with a valid destination now raises only what
    /// a Linux read raises: nothing at end of file, one IN_ACCESS for data.
    #[test]
    fn a_valid_destination_read_raises_only_linuxs_inotify_events() {
        let root = TestDir::new();
        let path = root.0.join("watched");
        std::fs::write(&path, b"").unwrap();
        // SAFETY: inotify_init1 returns a new descriptor on success.
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(inotify >= 0);
        // SAFETY: the descriptor was just created and is owned here.
        let inotify = unsafe { std::fs::File::from_raw_fd(inotify) };
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: inotify is live and cpath is terminated.
        assert!(
            unsafe { libc::inotify_add_watch(inotify.as_raw_fd(), cpath.as_ptr(), libc::IN_ACCESS) }
                >= 0
        );
        let events = || {
            let mut buffer = [0u8; 4096];
            // SAFETY: buffer is writable for its length.
            let count = unsafe {
                libc::read(
                    inotify.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if count < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EAGAIN)
                );
                return 0;
            }
            count as usize / std::mem::size_of::<libc::inotify_event>()
        };
        let mut state = test_state(&root.0);
        state.files.insert(3, std::fs::File::open(&path).unwrap());
        let mut memory = read_zero_memory();
        let read = |memory: &mut GuestMemory, state: &mut LoadedStaticElf| {
            syscall_result(memory, state, libc::SYS_read, [3, READ_ZERO_BUFFER, 16, 0, 0, 0])
        };
        assert_eq!(read(&mut memory, &mut state), 0);
        assert_eq!(events(), 0, "a read at end of file raises no IN_ACCESS");
        std::fs::write(&path, b"data").unwrap();
        assert_eq!(read(&mut memory, &mut state), 4);
        assert_eq!(events(), 1, "a read that returns data raises one IN_ACCESS");
    }
}
