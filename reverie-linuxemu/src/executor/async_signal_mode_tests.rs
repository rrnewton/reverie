/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included in executor::tests. Asynchronous signal mode (async_signal_mode.rs):
// these calls reach the checked syscall dispatcher, which turns a refusal into
// the typed `UnsupportedOperation::AsyncSignalMode`. Every expected value is
// this backend's actual one; where Linux answers from the host file, the
// native answer on a native descriptor of the same kind is the expectation.

const ASYNC_INT: u64 = 0x80;
const ASYNC_PATH: u64 = 0x100;
const ASYNC_PAIR: u64 = 0x180;
const ASYNC_MESSAGE: u64 = 0x500;
const ASYNC_IOV: u64 = 0x600;
const ASYNC_CONTROL: u64 = 0x700;
const ASYNC_DATA: u64 = 0x800;

struct AsyncFixture {
    executor: ElfExecutor,
    memory: GuestMemory,
    _root: TestDir,
}

impl AsyncFixture {
    /// A checked executor over a test state that `prepare` may give
    /// descriptors the run did not create (inherited or received ones) and
    /// a standard input, before the executor's table is made from it.
    fn with(prepare: impl FnOnce(&mut LoadedStaticElf)) -> Self {
        let root = TestDir::new();
        let mut state = test_state(&root.0);
        prepare(&mut state);
        Self {
            executor: ElfExecutor::new(state, false),
            memory: GuestMemory::new(0, PAGE_SIZE as usize).unwrap(),
            _root: root,
        }
    }

    fn new() -> Self {
        Self::with(|_| {})
    }

    fn call(&mut self, number: libc::c_long, args: [u64; 6]) -> i64 {
        self.executor
            .execute(&SyscallRequest::new(number as u64, args), &self.memory)
    }

    fn fioasync_with(&mut self, fd: i32, request: u64, value: i32) -> i64 {
        self.memory.write(ASYNC_INT, &value.to_ne_bytes()).unwrap();
        self.call(libc::SYS_ioctl, [fd as u64, request, ASYNC_INT, 0, 0, 0])
    }

    fn fioasync(&mut self, fd: i32, value: i32) -> i64 {
        self.fioasync_with(fd, libc::FIOASYNC as u64, value)
    }

    fn setfl(&mut self, fd: i32, flags: i32) -> i64 {
        self.call(
            libc::SYS_fcntl,
            [fd as u64, libc::F_SETFL as u64, flags as u64, 0, 0, 0],
        )
    }

    fn fcntl(&mut self, fd: i32, command: i32, argument: u64) -> i64 {
        self.call(
            libc::SYS_fcntl,
            [fd as u64, command as u64, argument, 0, 0, 0],
        )
    }

    fn getfl(&mut self, fd: i32) -> i32 {
        let flags = self.fcntl(fd, libc::F_GETFL, 0);
        assert!(flags >= 0, "F_GETFL({fd}): {flags}");
        flags as i32
    }

    fn host(&self, fd: i32) -> RawFd {
        host_fd(&self.executor.state, fd).unwrap()
    }

    fn host_flags(&self, fd: i32) -> i32 {
        fd_status_flags(self.host(fd)).unwrap()
    }

    /// The typed refusal of the last call, which must be this one's kind.
    fn refusal(&mut self) -> Option<&'static str> {
        self.executor.take_unsupported_refusal().map(|refusal| {
            assert_eq!(
                refusal.operation,
                reverie::UnsupportedOperation::AsyncSignalMode
            );
            assert_eq!(refusal.errno, reverie::Errno::ENOSYS);
            refusal.diagnostic
        })
    }

    fn pipe(&mut self) -> [i32; 2] {
        assert_eq!(
            self.call(libc::SYS_pipe2, [ASYNC_PAIR, 0, 0, 0, 0, 0]),
            0
        );
        read_struct(&self.memory, ASYNC_PAIR)
    }

    fn open(&mut self, path: &str, flags: i32) -> i32 {
        let mut bytes = path.as_bytes().to_vec();
        bytes.push(0);
        self.memory.write(ASYNC_PATH, &bytes).unwrap();
        let fd = self.call(
            libc::SYS_openat,
            [libc::AT_FDCWD as u64, ASYNC_PATH, flags as u64, 0o600, 0, 0],
        );
        assert!(fd >= 0, "open {path}: {fd}");
        fd as i32
    }

    fn dup(&mut self, fd: i32) -> i32 {
        let alias = self.call(libc::SYS_dup, [fd as u64, 0, 0, 0, 0, 0]);
        assert!(alias >= 0, "dup({fd}): {alias}");
        alias as i32
    }

    /// Sends one byte with `rights` in an SCM_RIGHTS message on `socket`.
    fn send_rights(&mut self, socket: i32, rights: &[i32]) -> i64 {
        let control = rights_control(rights);
        self.memory.write(ASYNC_CONTROL, &control).unwrap();
        self.memory.write(ASYNC_DATA, b"X").unwrap();
        let iov = libc::iovec {
            iov_base: ASYNC_DATA as usize as *mut libc::c_void,
            iov_len: 1,
        };
        assert_eq!(write_struct(&mut ProviderMemory(&self.memory), ASYNC_IOV, &iov), 0);
        // SAFETY: all-zero is a valid empty Linux msghdr. Guest pointers below
        // are translated by sendmsg; Rust never dereferences them.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = ASYNC_IOV as usize as *mut libc::iovec;
        message.msg_iovlen = 1;
        message.msg_control = ASYNC_CONTROL as usize as *mut libc::c_void;
        message.msg_controllen = control.len();
        let header = GuestMsghdr::from(&message);
        assert_eq!(write_struct(&mut ProviderMemory(&self.memory), ASYNC_MESSAGE, &header), 0);
        self.call(
            libc::SYS_sendmsg,
            [socket as u64, ASYNC_MESSAGE, libc::MSG_NOSIGNAL as u64, 0, 0, 0],
        )
    }
}

fn socket_file(socket: UnixStream) -> std::fs::File {
    std::fs::File::from(std::os::fd::OwnedFd::from(socket))
}

/// Linux's answer to `FIOASYNC(1)` on the native descriptor `fd`.
fn native_fioasync_on(fd: RawFd) -> i64 {
    let one: libc::c_int = 1;
    // SAFETY: fd is a live native descriptor and one is readable for one int.
    zero_or_errno(unsafe { libc::ioctl(fd, libc::FIOASYNC, &one) })
}

/// Sets the status flags of a native descriptor, as a process outside the run
/// sharing the description would.
fn outside_setfl(fd: RawFd, flags: i32) {
    // SAFETY: fd is a live native descriptor; F_SETFL takes one int.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);
}

/// A native pseudo-terminal pair: (controller, terminal).
fn native_pty() -> (std::fs::File, std::fs::File) {
    let (mut controller, mut terminal) = (-1, -1);
    // SAFETY: both out-parameters are writable; the name, termios and winsize
    // pointers may be null.
    let result = unsafe {
        libc::openpty(
            &mut controller,
            &mut terminal,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: openpty returned two new owned descriptors.
    unsafe {
        (
            std::fs::File::from_raw_fd(controller),
            std::fs::File::from_raw_fd(terminal),
        )
    }
}

/// A terminal's host owner, as `F_GETOWN_EX` reads it: (type, pid).
fn native_owner(fd: RawFd) -> (i32, i32) {
    let mut owner = [0_i32; 2];
    // SAFETY: owner is writable for one `struct f_owner_ex`.
    let result = unsafe { libc::fcntl(fd, LINUX_F_GETOWN_EX, owner.as_mut_ptr()) };
    assert_eq!(result, 0);
    (owner[0], owner[1])
}

#[test]
fn fioasync_on_and_off_track_the_host_flag_on_a_guest_pipe() {
    let mut f = AsyncFixture::new();
    let native = owner_native_pipe();
    let [read, write] = f.pipe();
    for (fd, native) in [(read, &native[0]), (write, &native[1])] {
        assert_eq!(native_fioasync_on(native.as_raw_fd()), 0);
        assert_eq!(f.fioasync(fd, 1), 0);
        assert_ne!(f.getfl(fd) & libc::O_ASYNC, 0);
        assert_ne!(f.host_flags(fd) & libc::O_ASYNC, 0);
        assert_eq!(f.fioasync(fd, 0), 0);
        assert_eq!(f.getfl(fd) & libc::O_ASYNC, 0);
        assert_eq!(f.refusal(), None);
    }
}

#[test]
fn setfl_async_round_trip_reads_the_host_flag() {
    let mut f = AsyncFixture::new();
    let [read, _] = f.pipe();
    let flags = f.getfl(read);
    assert_eq!(f.setfl(read, flags | libc::O_ASYNC), 0);
    assert_eq!(f.getfl(read), flags | libc::O_ASYNC);
    assert_eq!(f.setfl(read, flags), 0);
    assert_eq!(f.getfl(read), flags);
    assert_eq!(f.refusal(), None);
}

#[test]
fn fioasync_bad_fd_o_path_and_pointer_errors_come_in_linux_order() {
    let mut f = AsyncFixture::new();
    let [read, _] = f.pipe();
    let unmapped = 2 * PAGE_SIZE;
    // fdget first, then O_PATH, then the int's copy (fs/ioctl.c:369).
    let ioctl = |f: &mut AsyncFixture, fd: i32| {
        f.call(
            libc::SYS_ioctl,
            [fd as u64, libc::FIOASYNC as u64, unmapped, 0, 0, 0],
        )
    };
    assert_eq!(ioctl(&mut f, 999), negative_errno(libc::EBADF));
    let path = f.open(".", libc::O_PATH | libc::O_DIRECTORY);
    assert_eq!(ioctl(&mut f, path), negative_errno(libc::EBADF));
    assert_eq!(ioctl(&mut f, read), negative_errno(libc::EFAULT));
    assert_eq!(f.refusal(), None);
}

#[test]
fn fioasync_high_word_command_is_fioasync() {
    let [import, _peer] = owner_native_pipe();
    let mut inherited = -1;
    let mut f = AsyncFixture::with(|state| {
        inherited = insert_file_with_flags(state, import, false, None) as i32;
    });
    let high = (1_u64 << 32) | libc::FIOASYNC as u64;
    let [read, _] = f.pipe();
    assert_eq!(f.fioasync_with(read, high, 1), 0);
    assert_ne!(f.host_flags(read) & libc::O_ASYNC, 0);
    assert_eq!(f.fioasync_with(inherited, high, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
}

#[test]
fn managed_pipe_async_on_is_a_typed_refusal_and_async_blocks_owner_admission() {
    let mut f = AsyncFixture::new();
    let [read, write] = f.pipe();
    // After FIOASYNC on, owner admission refuses, as after F_SETFL O_ASYNC
    // (pipe_owner.rs keeps that bare ENOSYS).
    assert_eq!(f.fioasync(write, 1), 0);
    assert_eq!(f.fcntl(write, libc::F_SETOWN, 0), negative_errno(libc::ENOSYS));
    assert_eq!(f.executor.take_unsupported_refusal(), None);
    // A recorded owner refuses the mode by name, on both paths.
    assert_eq!(f.fcntl(read, libc::F_SETOWN, 0), 0);
    assert_eq!(f.fioasync(read, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(pipe_owner::ASYNC_OWNER_RECORDED_REFUSAL));
    let flags = f.getfl(read);
    assert_eq!(f.setfl(read, flags | libc::O_ASYNC), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(pipe_owner::ASYNC_OWNER_RECORDED_REFUSAL));
    assert_eq!(f.host_flags(read), flags);
}

#[test]
fn inherited_pipe_async_on_is_refused_on_both_paths_and_off_is_forwarded() {
    let [import, _peer] = owner_native_pipe();
    let mut fd = -1;
    let mut f = AsyncFixture::with(|state| {
        fd = insert_file_with_flags(state, import, false, None) as i32;
    });
    let flags = f.host_flags(fd);
    assert_eq!(f.fioasync(fd, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
    assert_eq!(f.setfl(fd, flags | libc::O_ASYNC), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
    assert_eq!(f.host_flags(fd), flags);
    assert_eq!(f.fioasync(fd, 0), 0);
    assert_eq!(f.setfl(fd, flags & !libc::O_NONBLOCK), 0);
    assert_eq!(f.host_flags(fd), flags & !libc::O_NONBLOCK);
    assert_eq!(f.refusal(), None);
}

#[test]
fn outside_the_domain_only_a_no_op_passes_compared_over_settable_flags() {
    let [import, _peer] = owner_native_pipe();
    let holder = import.try_clone().unwrap();
    outside_setfl(holder.as_raw_fd(), libc::O_ASYNC);
    let mut fd = -1;
    let mut f = AsyncFixture::with(|state| {
        fd = insert_file_with_flags(state, import, false, None) as i32;
    });
    let flags = f.host_flags(fd);
    assert_eq!(flags & (libc::O_ASYNC | libc::O_NONBLOCK), libc::O_ASYNC);
    // No-ops: FIOASYNC(on) with FASYNC set, and an F_SETFL carrying O_ASYNC
    // that changes no settable flag. The access mode and O_LARGEFILE in the
    // fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_ASYNC) idiom are no change.
    assert_eq!(f.fioasync(fd, 1), 0);
    assert_eq!(f.setfl(fd, flags), 0);
    assert_eq!(f.setfl(fd, flags | libc::O_LARGEFILE | libc::O_RDWR), 0);
    assert_eq!(f.refusal(), None);
    // A carry-over that changes another settable flag is refused, an
    // O_NONBLOCK-only change included, and changes nothing.
    for change in [libc::O_NONBLOCK, libc::O_APPEND] {
        assert_eq!(f.setfl(fd, flags | change), negative_errno(libc::ENOSYS));
        assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
        assert_eq!(f.host_flags(fd), flags);
    }
}

#[test]
fn two_setter_seam_keeps_the_outside_word() {
    let [import, _peer] = owner_native_pipe();
    let holder = import.try_clone().unwrap();
    let outside = holder.as_raw_fd();
    outside_setfl(outside, libc::O_ASYNC);
    let mut fd = -1;
    let mut f = AsyncFixture::with(|state| {
        fd = insert_file_with_flags(state, import, false, None) as i32;
    });
    let start = f.host_flags(fd);
    let bits = libc::O_ASYNC | libc::O_APPEND | libc::O_NONBLOCK;
    assert_eq!(start & bits, libc::O_ASYNC);
    // The guest asks for async 1, append 0, nonblock 1; once its flags are
    // read, the outside holder writes async 1, append 1, nonblock 0.
    ASYNC_OBSERVED_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            outside_setfl(outside, libc::O_ASYNC | libc::O_APPEND)
        }))
    });
    assert_eq!(
        f.setfl(fd, libc::O_ASYNC | libc::O_NONBLOCK),
        negative_errno(libc::ENOSYS)
    );
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
    // The whole word is the outside holder's: never append 1 with nonblock 1.
    let after = f.host_flags(fd);
    assert_eq!(after, fd_status_flags(outside).unwrap());
    assert_eq!(after & bits, libc::O_ASYNC | libc::O_APPEND);
    // A no-op answered while the holder writes lets the holder's word stand.
    ASYNC_OBSERVED_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || outside_setfl(outside, libc::O_ASYNC)))
    });
    assert_eq!(f.setfl(fd, libc::O_ASYNC | libc::O_APPEND), 0);
    assert_eq!(f.host_flags(fd) & bits, libc::O_ASYNC);
}

#[test]
fn memory_devices_forward_and_random_devices_are_refused() {
    let mut f = AsyncFixture::new();
    for path in ["/dev/null", "/dev/zero", "/dev/full"] {
        let native = std::fs::File::open(path).unwrap();
        let fd = f.open(path, libc::O_RDONLY);
        // A requested transition on operations without fasync: ENOTTY.
        assert_eq!(f.fioasync(fd, 1), native_fioasync_on(native.as_raw_fd()), "{path}");
        assert_eq!(f.fioasync(fd, 1), negative_errno(libc::ENOTTY), "{path}");
        let flags = f.getfl(fd);
        assert_eq!(f.setfl(fd, flags | libc::O_ASYNC), 0, "{path}");
        assert_eq!(f.getfl(fd), flags, "{path}: F_SETFL cannot set FASYNC here");
        assert_eq!(f.refusal(), None, "{path}");
    }
    // Whatever their provenance: the test state's inherited stdin is /dev/null.
    assert_eq!(f.fioasync(0, 1), negative_errno(libc::ENOTTY));
    for path in ["/dev/random", "/dev/urandom"] {
        let fd = f.open(path, libc::O_RDONLY);
        assert_eq!(f.fioasync(fd, 1), negative_errno(libc::ENOSYS), "{path}");
        assert_eq!(f.refusal(), Some(RANDOM_ASYNC_REFUSAL), "{path}");
        let flags = f.getfl(fd);
        assert_eq!(f.setfl(fd, flags | libc::O_ASYNC), negative_errno(libc::ENOSYS));
        assert_eq!(f.refusal(), Some(RANDOM_ASYNC_REFUSAL), "{path}");
        assert_eq!(f.host_flags(fd) & libc::O_ASYNC, 0, "{path}");
    }
}

#[test]
fn memory_device_minors_admit_only_the_audited_no_fasync_set() {
    // A table proof, not a live control: /dev/mem, /dev/port and /dev/kmsg
    // need privileges a test may lack. Minors outside drivers/char/mem.c's
    // devlist cannot be opened.
    for minor in 0..=16 {
        let admitted = matches!(memory_device_admission(1, minor), AsyncAdmission::Device);
        assert_eq!(admitted, [1, 3, 4, 5, 7].contains(&minor), "minor {minor}");
    }
    let reason = |major, minor| match memory_device_admission(major, minor) {
        AsyncAdmission::Outside(reason) => reason,
        _ => panic!("{major}:{minor} admitted"),
    };
    assert_eq!(reason(1, 8), RANDOM_ASYNC_REFUSAL);
    assert_eq!(reason(1, 9), RANDOM_ASYNC_REFUSAL);
    // kmsg stays refused until its operations are audited on the supported
    // host kernels, and keeps its refusal row.
    assert_eq!(reason(1, 11), KMSG_ASYNC_REFUSAL);
    assert_eq!(reason(1, 2), CHARACTER_DEVICE_ASYNC_REFUSAL);
    // /dev/tty and a pseudo-terminal are other majors.
    assert_eq!(reason(5, 0), CHARACTER_DEVICE_ASYNC_REFUSAL);
    assert_eq!(reason(136, 0), CHARACTER_DEVICE_ASYNC_REFUSAL);
}

#[test]
fn rule_b_reads_the_device_from_the_descriptor() {
    let (_controller, terminal) = native_pty();
    let terminal_path = std::fs::read_link(format!("/proc/self/fd/{}", terminal.as_raw_fd()))
        .unwrap();
    let mut f = AsyncFixture::new();
    // A terminal opened through a link named null: a path check would admit it.
    std::os::unix::fs::symlink(&terminal_path, f._root.0.join("null")).unwrap();
    let fd = f.open("null", libc::O_RDWR | libc::O_NOCTTY);
    assert_eq!(f.fioasync(fd, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(CHARACTER_DEVICE_ASYNC_REFUSAL));
    assert_eq!(native_owner(f.host(fd)), (0, 0));
    // /dev/null through a link that is then removed: a path check would fail.
    std::os::unix::fs::symlink("/dev/null", f._root.0.join("gone")).unwrap();
    let null = f.open("gone", libc::O_RDONLY);
    std::fs::remove_file(f._root.0.join("gone")).unwrap();
    assert_eq!(f.fioasync(null, 1), negative_errno(libc::ENOTTY));
    assert_eq!(f.refusal(), None);
}

#[test]
fn no_change_control_answers_zero_when_fasync_is_already_set() {
    use std::os::unix::fs::OpenOptionsExt as _;
    // open(2) keeps FASYNC in f_flags without calling fasync
    // (fs/open.c:963), so FIOASYNC(on) changes nothing and Linux answers 0
    // (fs/ioctl.c:375-382), where a requested transition answers ENOTTY.
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_ASYNC)
            .open("/dev/null")
            .unwrap()
    };
    let native = open();
    let mut fd = -1;
    let mut f = AsyncFixture::with(|state| {
        fd = insert_file_with_flags(state, open(), false, None) as i32;
    });
    assert_ne!(f.getfl(fd) & libc::O_ASYNC, 0);
    assert_eq!(native_fioasync_on(native.as_raw_fd()), 0);
    assert_eq!(f.fioasync(fd, 1), 0);
    assert_eq!(f.refusal(), None);
}

#[test]
fn created_descriptions_and_keepers_answer_as_the_host_does() {
    let mut f = AsyncFixture::new();
    let natives: Vec<(&str, std::fs::File)> = {
        let root = &f._root.0;
        let regular = std::fs::File::create(root.join("native-regular")).unwrap();
        let directory = std::fs::File::open(root).unwrap();
        let (socket, _) = UnixStream::pair().unwrap();
        let anonymous = |fd: RawFd| {
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            // SAFETY: the creating call returned a new owned descriptor.
            unsafe { std::fs::File::from_raw_fd(fd) }
        };
        // SAFETY (each block): the creating calls take only flags and
        // return a new owned descriptor or fail.
        vec![
            ("regular", regular),
            ("directory", directory),
            ("socket", socket_file(socket)),
            ("eventfd", anonymous(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) })),
            (
                "timerfd",
                anonymous(unsafe {
                    libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC)
                }),
            ),
            ("inotify", anonymous(unsafe { libc::inotify_init1(libc::IN_CLOEXEC) })),
            ("epoll", anonymous(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) })),
        ]
    };
    let guest = |f: &mut AsyncFixture, kind: &str| -> i32 {
        let fd = match kind {
            "regular" => i64::from(f.open("guest-regular", libc::O_CREAT | libc::O_RDWR)),
            "directory" => i64::from(f.open(".", libc::O_RDONLY | libc::O_DIRECTORY)),
            "socket" => {
                let created = f.call(
                    libc::SYS_socketpair,
                    [libc::AF_UNIX as u64, libc::SOCK_STREAM as u64, 0, ASYNC_PAIR, 0, 0],
                );
                assert_eq!(created, 0);
                i64::from(read_struct::<[i32; 2]>(&f.memory, ASYNC_PAIR)[0])
            }
            "eventfd" => f.call(libc::SYS_eventfd2, [0; 6]),
            "timerfd" => f.call(
                libc::SYS_timerfd_create,
                [libc::CLOCK_MONOTONIC as u64, 0, 0, 0, 0, 0],
            ),
            "inotify" => f.call(libc::SYS_inotify_init1, [0; 6]),
            "epoll" => f.call(libc::SYS_epoll_create1, [0; 6]),
            _ => unreachable!(),
        };
        assert!(fd >= 0, "{kind}: {fd}");
        fd as i32
    };
    for (kind, native) in &natives {
        let fd = guest(&mut f, kind);
        assert_eq!(f.fioasync(fd, 1), native_fioasync_on(native.as_raw_fd()), "{kind}");
        assert_eq!(f.refusal(), None, "{kind}");
        assert_eq!(f.fioasync(fd, 0), 0, "{kind}");
    }
}

#[test]
fn unrecorded_anonymous_inode_and_terminal_are_refused() {
    use std::os::unix::fs::OpenOptionsExt as _;
    // SAFETY: eventfd returns a new owned descriptor or fails.
    let eventfd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    assert!(eventfd >= 0);
    // SAFETY: eventfd returned a new owned descriptor.
    let eventfd = unsafe { std::fs::File::from_raw_fd(eventfd) };
    let (_controller, terminal) = native_pty();
    let path = std::fs::read_link(format!("/proc/self/fd/{}", terminal.as_raw_fd())).unwrap();
    // A terminal description with FASYNC set and no owner (open keeps the
    // flag without calling fasync): the stale-flag seam's NULL-owner tty.
    let flagged = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_ASYNC)
        .open(&path)
        .unwrap();
    let outside = flagged.try_clone().unwrap();
    let (mut anonymous, mut tty, mut stale) = (-1, -1, -1);
    let mut f = AsyncFixture::with(|state| {
        anonymous = insert_file_with_flags(state, eventfd, false, None) as i32;
        tty = insert_file_with_flags(state, terminal.try_clone().unwrap(), false, None) as i32;
        stale = insert_file_with_flags(state, flagged, false, None) as i32;
    });
    assert_eq!(f.fioasync(anonymous, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
    assert_eq!(f.fioasync(tty, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(CHARACTER_DEVICE_ASYNC_REFUSAL));
    let flags = f.getfl(tty);
    assert_eq!(f.setfl(tty, flags | libc::O_ASYNC), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(CHARACTER_DEVICE_ASYNC_REFUSAL));
    assert_eq!(native_owner(terminal.as_raw_fd()), (0, 0));
    // The outside holder clears async between the guest's read of the flags
    // and its answer: the no-op is answered with no host call, so no
    // terminal fasync runs and the description still has no owner.
    let outside_fd = outside.as_raw_fd();
    let cleared = fd_status_flags(outside_fd).unwrap() & !libc::O_ASYNC;
    ASYNC_OBSERVED_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || outside_setfl(outside_fd, cleared)))
    });
    assert_eq!(f.fioasync(stale, 1), 0);
    assert_eq!(f.refusal(), None);
    assert_eq!(native_owner(outside_fd), (0, 0));
}

#[test]
fn export_revokes_admission_through_an_alias_and_a_fork() {
    let (socket, peer) = UnixStream::pair().unwrap();
    let mut sender = -1;
    let mut f = AsyncFixture::with(|state| {
        sender = insert_file_with_flags(state, socket_file(socket), false, None) as i32;
    });
    let [read, _] = f.pipe();
    let alias = f.dup(read);
    assert_eq!(f.send_rights(sender, &[read]), 1);
    for fd in [read, alias] {
        assert_eq!(f.fioasync(fd, 1), negative_errno(libc::ENOSYS));
        assert_eq!(f.refusal(), Some(pipe_owner::ASYNC_EXPORTED_REFUSAL));
    }
    let mut child = f.executor.state.try_clone_for_fork(2).unwrap();
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    memory.write(ASYNC_INT, &1_i32.to_ne_bytes()).unwrap();
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut child,
            libc::SYS_ioctl,
            [alias as u64, libc::FIOASYNC as u64, ASYNC_INT, 0, 0, 0]
        ),
        negative_errno(libc::ENOSYS)
    );
    assert_eq!(child.async_signal_refusal, Some(pipe_owner::ASYNC_EXPORTED_REFUSAL));
    drop(peer);
}

#[test]
fn export_with_async_on_is_refused_and_marks_nothing() {
    let (socket, peer) = UnixStream::pair().unwrap();
    let mut sender = -1;
    let mut f = AsyncFixture::with(|state| {
        sender = insert_file_with_flags(state, socket_file(socket), false, None) as i32;
    });
    let [read, _] = f.pipe();
    assert_eq!(f.fioasync(read, 1), 0);
    assert_eq!(f.send_rights(sender, &[read]), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(pipe_owner::ASYNC_EXPORT_REFUSAL));
    let mut byte = 0_u8;
    // SAFETY: the peer is open and byte is writable; MSG_DONTWAIT never blocks.
    let received = unsafe {
        libc::recv(peer.as_raw_fd(), (&mut byte as *mut u8).cast(), 1, libc::MSG_DONTWAIT)
    };
    assert_eq!(received, -1, "the refused send sent nothing");
    // Nothing was marked: the pipe still takes the mode and an owner.
    assert_eq!(f.fioasync(read, 0), 0);
    assert_eq!(f.fioasync(read, 1), 0);
    assert_eq!(f.fioasync(read, 0), 0);
    assert_eq!(f.fcntl(read, libc::F_SETOWN, 0), 0);
}

#[test]
fn export_race_after_the_scan_is_refused_and_marks_nothing() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    let mut sender = -1;
    let mut f = AsyncFixture::with(|state| {
        sender = insert_file_with_flags(state, socket_file(socket), false, None) as i32;
    });
    let [read, _] = f.pipe();
    let host = f.host(read);
    let flags = f.host_flags(read);
    // Another thread turns the mode on after the early scan.
    RIGHTS_SCANNED_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || outside_setfl(host, flags | libc::O_ASYNC)))
    });
    assert_eq!(f.send_rights(sender, &[read]), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(pipe_owner::ASYNC_EXPORT_REFUSAL));
    outside_setfl(host, flags);
    assert_eq!(f.fioasync(read, 1), 0, "a refused send marked the pipe");
}

#[test]
fn export_of_one_description_twice_locks_it_once() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    let mut sender = -1;
    let mut f = AsyncFixture::with(|state| {
        sender = insert_file_with_flags(state, socket_file(socket), false, None) as i32;
    });
    let [read, _] = f.pipe();
    let alias = f.dup(read);
    assert_eq!(f.send_rights(sender, &[read, read, alias]), 1);
    assert_eq!(f.fioasync(alias, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(pipe_owner::ASYNC_EXPORTED_REFUSAL));
}

#[test]
fn declared_stdin_admits_a_pipe_refuses_a_terminal_and_lapses_on_replacement() {
    let [reader, _writer] = owner_native_pipe();
    let [other, _other_writer] = owner_native_pipe();
    let mut inherited = -1;
    let mut f = AsyncFixture::with(|state| {
        state.stdin = Some(reader);
        state.declare_owned_stdin();
        inherited = insert_file_with_flags(state, other, false, None) as i32;
    });
    assert_eq!(f.fioasync(0, 1), 0);
    assert_ne!(f.host_flags(0) & libc::O_ASYNC, 0);
    assert_eq!(f.fioasync(0, 0), 0);
    let alias = f.dup(0);
    assert_eq!(f.fioasync(alias, 1), 0);
    assert_eq!(f.fioasync(alias, 0), 0);
    // Descriptor 0 replaced by an inherited description: the declaration no
    // longer names it, while the alias keeps the declared record.
    assert_eq!(f.call(libc::SYS_dup2, [inherited as u64, 0, 0, 0, 0, 0]), 0);
    assert_eq!(f.fioasync(0, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
    assert_eq!(f.fioasync(alias, 1), 0);

    // A declared terminal fails the type check.
    let (_controller, terminal) = native_pty();
    let mut f = AsyncFixture::with(|state| {
        state.stdin = Some(terminal.try_clone().unwrap());
        state.declare_owned_stdin();
    });
    assert_eq!(f.fioasync(0, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(CHARACTER_DEVICE_ASYNC_REFUSAL));
    assert_eq!(native_owner(terminal.as_raw_fd()), (0, 0));

    // An undeclared pipe stdin is inherited.
    let [reader, _writer] = owner_native_pipe();
    let mut f = AsyncFixture::with(|state| state.stdin = Some(reader));
    assert_eq!(f.fioasync(0, 1), negative_errno(libc::ENOSYS));
    assert_eq!(f.refusal(), Some(INHERITED_ASYNC_REFUSAL));
}

#[test]
fn forged_creation_result_keeps_the_held_descriptor() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(
        syscall_result(&mut memory, &mut state, libc::SYS_pipe2, [ASYNC_PAIR, 0, 0, 0, 0, 0]),
        0
    );
    let [read, _] = read_struct::<[i32; 2]>(&memory, ASYNC_PAIR);
    let held = host_fd(&state, read).unwrap();
    let identity = |fd: RawFd| {
        let stat = fstat_descriptor(fd);
        (stat.st_dev, stat.st_ino)
    };
    let before = (identity(held), fd_status_flags(held).unwrap());
    let stdin_before = identity(0);
    // A creating call that reports a descriptor the executor holds, as a
    // filter answering it with SECCOMP_RET_ERRNO and data 0 does: here its
    // pipe end's host descriptor, and the supervisor's descriptor 0.
    let mut copies = Vec::new();
    for (forged, expected) in [(held, before.0), (0, stdin_before)] {
        let fd = insert_created_file(&mut state, forged, false, pipe_owner::Provenance::Created);
        assert!(fd >= 0, "{forged}: {fd}");
        let fd = fd as i32;
        assert_eq!(identity(host_fd(&state, fd).unwrap()), expected);
        assert!(state.pipe_owners.get(&fd).is_none(), "{forged}: no creation record");
        copies.push(fd);
    }
    // The copy of the pipe end is inherited: the mode is refused on it.
    memory.write(ASYNC_INT, &1_i32.to_ne_bytes()).unwrap();
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_ioctl,
            [copies[0] as u64, libc::FIOASYNC as u64, ASYNC_INT, 0, 0, 0]
        ),
        negative_errno(libc::ENOSYS)
    );
    assert_eq!(state.async_signal_refusal, Some(INHERITED_ASYNC_REFUSAL));
    // The held descriptors stay open and unchanged.
    assert_eq!((identity(held), fd_status_flags(held).unwrap()), before);
    assert_eq!(identity(0), stdin_before);
}

fn fstat_descriptor(fd: RawFd) -> libc::stat {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: stat is writable and fd is live.
    assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
    // SAFETY: fstat initialized stat on success.
    unsafe { stat.assume_init() }
}
