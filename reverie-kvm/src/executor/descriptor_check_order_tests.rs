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

    /// A scalar read whose destination is writable skips the descriptor
    /// checks and lets the host read report the descriptor's error. For every
    /// descriptor the checks refuse, every destination (the writable one
    /// that takes the unchecked path, and each invalid one that keeps the
    /// checks) must give the error the checks give.
    #[test]
    fn scalar_read_descriptor_errors_do_not_depend_on_the_destination() {
        let directory = open_fixture(c".", libc::O_RDONLY | libc::O_DIRECTORY);
        assert_read_zero_fixture_filesystem(&directory);
        for (name, file) in [
            ("directory", directory),
            ("path", open_fixture(c".", libc::O_PATH)),
            ("write-only", open_fixture(c"/dev/null", libc::O_WRONLY)),
            ("epoll", epoll_fixture()),
        ] {
            let expected = ensure_read_capable(&file)
                .and_then(|()| ensure_readable(&file))
                .expect_err("each fixture is refused by the descriptor checks");
            let mut state = test_state(&std::env::current_dir().unwrap());
            state.files.insert(3, file);
            let mut memory = read_zero_memory();
            for address in READ_ZERO_ADDRESSES {
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

    /// A scalar write to a plain host descriptor whose source is readable
    /// skips the access-mode check and lets the host write report EBADF. For
    /// every descriptor that check refuses, every source gives its error.
    #[test]
    fn scalar_write_descriptor_errors_do_not_depend_on_the_source() {
        for (name, file) in [
            ("read-only", open_fixture(c"/dev/null", libc::O_RDONLY)),
            ("path", open_fixture(c".", libc::O_PATH)),
            (
                "directory",
                open_fixture(c".", libc::O_RDONLY | libc::O_DIRECTORY),
            ),
        ] {
            let expected =
                ensure_writable(&file).expect_err("each fixture is refused by the access-mode check");
            let mut state = test_state(&std::env::current_dir().unwrap());
            state.files.insert(3, file);
            let mut memory = read_zero_memory();
            for address in READ_ZERO_ADDRESSES {
                assert_eq!(
                    syscall_result(
                        &mut memory,
                        &mut state,
                        libc::SYS_write,
                        [3, address, 16, 0, 0, 0],
                    ),
                    expected,
                    "{name} address={address:#x}"
                );
                assert_read_zero_canaries(&memory);
            }
        }
        // A descriptor the access-mode check admits but the kernel cannot
        // write (no FMODE_CAN_WRITE) gets the host's EINVAL from a readable
        // source, as before.
        let mut state = test_state(&std::env::current_dir().unwrap());
        state.files.insert(3, epoll_fixture());
        let mut memory = read_zero_memory();
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                libc::SYS_write,
                [3, READ_ZERO_BUFFER, 16, 0, 0, 0],
            ),
            negative_errno(libc::EINVAL)
        );
    }
}
