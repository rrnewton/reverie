/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Compile-only until separate native admission. No tracer or cleanup owner.
use std::os::unix::process::CommandExt;

fn marker(bytes: &[u8]) {
    // Deliberately intercepted by the fixture Tool; no host descriptor opened.
    let result = unsafe { libc::syscall(libc::SYS_write, 631, bytes.as_ptr(), bytes.len()) };
    assert_eq!(result, bytes.len() as libc::c_long);
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("pthread") => {
            marker(b"root");
            let child = std::thread::spawn(|| marker(b"child"));
            child.join().unwrap();
            marker(b"joined");
        }
        Some("exec") => {
            marker(b"before-exec");
            let error = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("terminal")
                .exec();
            panic!("exec unexpectedly returned: {error}");
        }
        Some("terminal") => marker(b"terminal"),
        Some("operations") => {
            marker(b"operations-start");
            for _ in 0..256 {
                assert!(unsafe { libc::syscall(libc::SYS_getpid) } > 0);
                assert_eq!(unsafe { libc::syscall(libc::SYS_close, -1) }, -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EBADF)
                );
            }
            let mut pipe = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            let bytes = *b"short";
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_write, pipe[1], bytes.as_ptr(), bytes.len()) },
                5
            );
            let mut buffer = [0u8; 64];
            assert_eq!(
                unsafe {
                    libc::syscall(libc::SYS_read, pipe[0], buffer.as_mut_ptr(), buffer.len())
                },
                5
            );
            assert_eq!(&buffer[..5], &bytes);
            for fd in pipe {
                assert_eq!(unsafe { libc::syscall(libc::SYS_close, fd) }, 0);
            }
            marker(b"operations-done");
        }
        Some("lifetimes") => sequential_threads(),
        Some("startup-deaths") => sequential_startup_deaths(),
        Some("startup-final") => sequential_startup_final(),
        Some("source-hold-688") => followed_source_hold(),
        Some("executable-source-754") => executable_source(),
        Some("peer-sendto-root") => followed_peer_sendto(false, false),
        Some("peer-sendto-child") => followed_peer_sendto(true, false),
        Some("peer-sendto-blocked-root") => followed_peer_sendto(false, true),
        Some("peer-sendto-blocked-child") => followed_peer_sendto(true, true),
        Some("source-retirement-703") => retirement_source(),
        Some("store-read-root") => followed_destination_store(false, false),
        Some("store-read-child") => followed_destination_store(true, false),
        Some("store-recv-root") => followed_destination_store(false, true),
        Some("store-recv-child") => followed_destination_store(true, true),
        Some("timer-join-sendto") => followed_timer_join(0),
        Some("timer-join-poll") => followed_timer_join(1),
        Some("timer-join-read") => followed_timer_join(2),
        Some("original-poll-join") => followed_original_poll_join(false),
        Some("original-poll-mixed") => followed_original_poll_join(true),
        Some("original-poll") => {
            let child = std::env::args().nth(2).unwrap().parse::<u8>().unwrap() != 0;
            let kind = std::env::args().nth(3).unwrap().parse::<u8>().unwrap();
            followed_original_poll(child, kind);
        }
        mode => panic!("unadmitted fixture mode: {mode:?}"),
    }
}

fn retirement_source() {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Source([u8; 8]);
    extern "C" fn child(_: *mut libc::c_void) -> libc::c_int {
        let source = Source(*b"child703");
        let count =
            unsafe { libc::syscall(libc::SYS_write, 703, source.0.as_ptr(), source.0.len()) };
        unsafe {
            libc::syscall(libc::SYS_exit, if count == 8 { 0 } else { 83 });
        }
        unreachable!("SYS_exit returned");
    }
    let mut stack = vec![0u128; 16384];
    let tid = AtomicI32::new(0);
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) }.cast();
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    assert!(
        unsafe {
            libc::clone(
                child,
                top,
                flags,
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
            )
        } > 0
    );
    let source = Source(*b"root-703");
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_write, 703, source.0.as_ptr(), source.0.len()) },
        8
    );
    loop {
        let current = tid.load(Ordering::Acquire);
        if current == 0 {
            break;
        }
        unsafe {
            libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
        }
    }
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_write, 704, source.0.as_ptr(), source.0.len()) },
        8
    );
}

fn followed_source_hold() {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Source([u8; 8]);
    extern "C" fn child(_: *mut libc::c_void) -> libc::c_int {
        let source = Source(*b"child688");
        let count =
            unsafe { libc::syscall(libc::SYS_write, 688, source.0.as_ptr(), source.0.len()) };
        unsafe {
            libc::syscall(libc::SYS_exit, if count == 8 { 0 } else { 82 });
        }
        unreachable!("SYS_exit returned");
    }
    // Reuse the existing raw followed-thread fixture's finite stack and
    // original clear-child-tid/futex join. No TLS destructor/madvise shortcut.
    let mut stack = vec![0u128; 16384];
    let tid = AtomicI32::new(0);
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) }.cast();
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    assert!(
        unsafe {
            libc::clone(
                child,
                top,
                flags,
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
            )
        } > 0
    );
    let source = Source(*b"root-688");
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_write, 688, source.0.as_ptr(), source.0.len()) },
        8
    );
    loop {
        let current = tid.load(Ordering::Acquire);
        if current == 0 {
            break;
        }
        unsafe {
            libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
        }
    }
}

fn sequential_startup_deaths() {
    // One long-lived parent and eight sequential process children. The Tool
    // signals each original child generation before its saved context restore.
    for _ in 0..8 {
        let child = unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0) };
        assert!(child >= 0);
        if child == 0 {
            unsafe { libc::_exit(81) };
        }
        let mut status = 0;
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_wait4, child, &mut status, 0, 0) },
            child
        );
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
        marker(b"startup-joined");
    }
}

fn sequential_startup_final() {
    // Observe actual disposition; do not install a replacement disposition.
    let mut disposition = unsafe { std::mem::zeroed::<libc::sigaction>() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut disposition) },
        0
    );
    assert_eq!(disposition.sa_sigaction, libc::SIG_DFL);
    assert_eq!(disposition.sa_flags & libc::SA_NOCLDWAIT, 0);
    marker(b"startup-final-sigchld-default");
    for _ in 0..8 {
        let child = unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0) };
        assert!(child >= 0);
        if child == 0 {
            unsafe { libc::_exit(81) };
        }
        let mut status = 0;
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_wait4, child, &mut status, 0, 0) },
            child
        );
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
        marker(b"startup-final-reaped");
    }
}

fn sequential_threads() {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    extern "C" fn child(_: *mut libc::c_void) -> libc::c_int {
        // No TLS, allocator, libc teardown or unbounded child loop.
        unsafe {
            libc::syscall(libc::SYS_exit, 0);
        }
        unreachable!("SYS_exit returned");
    }
    let mut stack = vec![0u128; 16384]; // 256 KiB; one reused, aligned stack.
    let tid = AtomicI32::new(0);
    for _ in 0..64 {
        let top = unsafe { stack.as_mut_ptr().add(stack.len()) }.cast();
        let flags = libc::CLONE_VM
            | libc::CLONE_FS
            | libc::CLONE_FILES
            | libc::CLONE_SIGHAND
            | libc::CLONE_THREAD
            | libc::CLONE_SYSVSEM
            | libc::CLONE_PARENT_SETTID
            | libc::CLONE_CHILD_CLEARTID;
        let result = unsafe {
            libc::clone(
                child,
                top,
                flags,
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
            )
        };
        assert!(result > 0);
        loop {
            let current = tid.load(Ordering::Acquire);
            if current == 0 {
                break;
            }
            unsafe {
                libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
            }
        }
        // The Tool additionally awaits its original terminal owner before
        // acknowledging this marker; no second kernel waiter is introduced.
        marker(b"joined-raw");
    }
}

/// The marker is Tool-parked, but the selected Sendto really copies its eight
/// stack bytes into a native Unix stream. The blocked variant fills that same
/// socket before the rendezvous, so cancellation must freeze a running sender.
fn followed_peer_sendto(child_sender: bool, blocked: bool) {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    let (sender, mut receiver) = UnixStream::pair().unwrap();
    let fd = sender.as_raw_fd();
    if blocked {
        sender.set_nonblocking(true).unwrap();
        let fill = [0u8; 4096];
        loop {
            let raw = unsafe { libc::write(fd, fill.as_ptr().cast(), fill.len()) };
            if raw < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EAGAIN)
                );
                break;
            }
            assert!(raw > 0);
        }
        sender.set_nonblocking(false).unwrap();
    }
    let act = move |selected: bool| {
        let bytes = *b"peerSend";
        let raw = unsafe {
            if selected {
                libc::syscall(
                    libc::SYS_sendto,
                    fd,
                    bytes.as_ptr(),
                    bytes.len(),
                    libc::MSG_NOSIGNAL,
                    0usize,
                    0usize,
                )
            } else {
                libc::syscall(libc::SYS_write, 688, bytes.as_ptr(), bytes.len())
            }
        };
        assert_eq!(raw, 8);
    };
    let child = std::thread::spawn(move || act(child_sender));
    act(!child_sender);
    child.join().unwrap();
    let mut bytes = [0u8; 8];
    receiver.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"peerSend");
}

fn followed_destination_store(child_receives: bool, recvfrom: bool) {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Destination([u8; 8]);
    fn operation(receives: bool, recvfrom: bool) {
        let mut buffer = Destination([0xa5; 8]);
        let raw = if receives {
            unsafe {
                libc::syscall(
                    if recvfrom {
                        libc::SYS_recvfrom
                    } else {
                        libc::SYS_read
                    },
                    744,
                    buffer.0.as_mut_ptr().add(1),
                    4usize,
                    0usize,
                    0usize,
                    0usize,
                )
            }
        } else {
            unsafe { libc::syscall(libc::SYS_write, 688, buffer.0.as_ptr(), 8usize) }
        };
        if receives {
            assert_eq!(raw, 4);
            assert_eq!(buffer.0, [0xa5, b'a', b'b', b'c', b'd', 0xa5, 0xa5, 0xa5]);
        } else {
            assert_eq!(raw, 8);
        }
    }
    extern "C" fn child(argument: *mut libc::c_void) -> libc::c_int {
        let &(receives, recvfrom) = unsafe { &*argument.cast::<(bool, bool)>() };
        operation(receives, recvfrom);
        unsafe {
            libc::syscall(libc::SYS_exit, 0);
        }
        unreachable!("original child exit returned")
    }
    let mut argument = (child_receives, recvfrom);
    let mut stack = vec![0u128; 16384];
    let tid = AtomicI32::new(0);
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    assert!(
        unsafe {
            libc::clone(
                child,
                stack.as_mut_ptr().add(stack.len()).cast(),
                flags,
                (&mut argument as *mut (bool, bool)).cast(),
                tid.as_ptr(),
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
            )
        } > 0
    );
    operation(!child_receives, recvfrom);
    loop {
        let current = tid.load(Ordering::Acquire);
        if current == 0 {
            break;
        }
        unsafe {
            libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
        }
    }
}

fn followed_timer_join(parent_kind: u8) {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Buffer([u8; 8]);
    extern "C" fn child(argument: *mut libc::c_void) -> libc::c_int {
        let fd = argument as usize;
        let mut buffer = Buffer([0xa5; 8]);
        let raw = unsafe { libc::syscall(libc::SYS_read, fd, buffer.0.as_mut_ptr(), 4usize) };
        assert_eq!(raw, 0);
        assert_eq!(buffer.0, [0xa5; 8]);
        unsafe {
            libc::syscall(libc::SYS_exit, 0);
        }
        unreachable!("original child exit returned")
    }
    let mut stacks = [vec![0u128; 16384], vec![0u128; 16384]];
    let tids = [AtomicI32::new(0), AtomicI32::new(0)];
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    for index in 0..2 {
        assert!(
            unsafe {
                libc::clone(
                    child,
                    stacks[index].as_mut_ptr().add(stacks[index].len()).cast(),
                    flags,
                    (744 + index) as *mut libc::c_void,
                    tids[index].as_ptr(),
                    std::ptr::null_mut::<libc::c_void>(),
                    tids[index].as_ptr(),
                )
            } > 0
        );
    }
    let mut buffer = Buffer([0xa5; 8]);
    let raw = unsafe {
        match parent_kind {
            0 => libc::syscall(
                libc::SYS_sendto,
                746,
                buffer.0.as_ptr(),
                8usize,
                libc::MSG_NOSIGNAL,
                0usize,
                0usize,
            ),
            1 => libc::syscall(libc::SYS_poll, 0usize, 0usize, 0usize),
            2 => libc::syscall(libc::SYS_read, 746, buffer.0.as_mut_ptr(), 4usize),
            _ => unreachable!(),
        }
    };
    assert_eq!(raw, if parent_kind == 0 { 8 } else { 0 });
    assert_eq!(buffer.0, [0xa5; 8]);
    for tid in &tids {
        loop {
            let current = tid.load(Ordering::Acquire);
            if current == 0 {
                break;
            }
            unsafe {
                libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
            }
        }
    }
}

fn followed_original_poll(child_receives: bool, kind: u8) {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Destination([u8; 8]);
    fn operation(receives: bool, kind: u8) {
        let mut buffer = Destination([0xa5; 8]);
        if receives {
            let page = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(page, libc::MAP_FAILED);
            let bytes = page.cast::<u8>();
            unsafe {
                std::ptr::write_bytes(bytes, 0xa5, 4096);
            }
            let row = unsafe { bytes.add(8).cast::<libc::pollfd>() };
            unsafe {
                row.write(libc::pollfd {
                    fd: 744,
                    events: libc::POLLIN,
                    revents: 0x5a5a,
                });
            }
            if kind == 6 {
                assert_eq!(unsafe { libc::mprotect(page, 4096, libc::PROT_READ) }, 0);
            }
            let pointer = if kind == 13 {
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
                1usize as *mut libc::pollfd
            } else {
                row
            };
            let timeout = if kind == 1 { 0usize } else { 5000 };
            let nfds = if kind == 14 { (1usize << 32) | 1 } else { 1 };
            let timeout = if kind == 14 {
                (1usize << 32) | timeout
            } else {
                timeout
            };
            let raw = unsafe {
                libc::syscall(
                    libc::SYS_poll,
                    pointer,
                    nfds,
                    timeout,
                    0x123usize,
                    0x456usize,
                    0x789usize,
                )
            };
            assert_eq!(raw, if kind == 1 { 0 } else { 1 });
            let actual = unsafe { row.read() };
            assert_eq!((actual.fd, actual.events), (744, libc::POLLIN));
            assert_eq!(actual.revents, if kind == 1 { 0 } else { libc::POLLIN });
            for index in 0..8 {
                assert_eq!(unsafe { *bytes.add(index) }, 0xa5);
            }
            for index in 16..24 {
                assert_eq!(unsafe { *bytes.add(index) }, 0xa5);
            }
            assert_eq!(unsafe { libc::munmap(page, 4096) }, 0);
        } else {
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_write, 688, buffer.0.as_mut_ptr(), 8usize) },
                8
            );
        }
    }

    extern "C" fn child(argument: *mut libc::c_void) -> libc::c_int {
        let &(receives, kind) = unsafe { &*argument.cast::<(bool, u8)>() };
        operation(receives, kind);
        unsafe {
            libc::syscall(libc::SYS_exit, 0);
        }
        unreachable!("original child exit returned")
    }
    let mut argument = (child_receives, kind);
    let mut stack = vec![0u128; 16384];
    let tid = AtomicI32::new(0);
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    assert!(
        unsafe {
            libc::clone(
                child,
                stack.as_mut_ptr().add(stack.len()).cast(),
                flags,
                (&mut argument as *mut (bool, u8)).cast(),
                tid.as_ptr(),
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
            )
        } > 0
    );
    operation(!child_receives, kind);
    loop {
        let current = tid.load(Ordering::Acquire);
        if current == 0 {
            break;
        }
        unsafe {
            libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
        }
    }
}

fn followed_original_poll_join(mixed: bool) {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Buffer([u8; 8]);
    extern "C" fn child(argument: *mut libc::c_void) -> libc::c_int {
        let encoded = argument as usize;
        let fd = encoded & 0xffff;
        let mixed = encoded >> 16 != 0;
        let mut buffer = Buffer([0xa5; 8]);
        if mixed && fd == 745 {
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_read, fd, buffer.0.as_mut_ptr(), 4usize) },
                0
            );
            assert_eq!(buffer.0, [0xa5; 8]);
        } else {
            buffer.0[..4].copy_from_slice(&(fd as i32).to_ne_bytes());
            buffer.0[4..6].copy_from_slice(&libc::POLLIN.to_ne_bytes());
            buffer.0[6..8].copy_from_slice(&0x5a5ai16.to_ne_bytes());
            let raw = unsafe {
                libc::syscall(
                    libc::SYS_poll,
                    buffer.0.as_mut_ptr(),
                    1usize,
                    5000usize,
                    fd,
                    0usize,
                    0usize,
                )
            };
            assert_eq!(raw, 1);
            assert_eq!(&buffer.0[..4], &(fd as i32).to_ne_bytes());
            assert_eq!(&buffer.0[4..6], &libc::POLLIN.to_ne_bytes());
            assert_eq!(&buffer.0[6..8], &libc::POLLIN.to_ne_bytes());
        }
        unsafe {
            libc::syscall(libc::SYS_exit, 0);
        }
        unreachable!("original child exit returned")
    }
    let mut stacks = [vec![0u128; 16384], vec![0u128; 16384]];
    let tids = [AtomicI32::new(0), AtomicI32::new(0)];
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    for index in 0..2 {
        assert!(
            unsafe {
                libc::clone(
                    child,
                    stacks[index].as_mut_ptr().add(stacks[index].len()).cast(),
                    flags,
                    ((744 + index) | ((mixed as usize) << 16)) as *mut libc::c_void,
                    tids[index].as_ptr(),
                    std::ptr::null_mut::<libc::c_void>(),
                    tids[index].as_ptr(),
                )
            } > 0
        );
    }
    let buffer = Buffer([0xa5; 8]);
    let raw = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            746,
            buffer.0.as_ptr(),
            8usize,
            libc::MSG_NOSIGNAL,
            0usize,
            0usize,
        )
    };
    assert_eq!(raw, 8);
    assert_eq!(buffer.0, [0xa5; 8]);
    for tid in &tids {
        loop {
            let current = tid.load(Ordering::Acquire);
            if current == 0 {
                break;
            }
            unsafe {
                libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
            }
        }
    }
}

fn executable_source() {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering;
    #[repr(align(4096))]
    struct Source([u8; 8]);
    static ROOT: Source = Source(*b"root-754");
    static CHILD: Source = Source(*b"child754");
    extern "C" fn child(_: *mut libc::c_void) -> libc::c_int {
        let source = &CHILD;
        let count =
            unsafe { libc::syscall(libc::SYS_write, 754, source.0.as_ptr(), source.0.len()) };
        unsafe {
            libc::syscall(libc::SYS_exit, if count == 8 { 0 } else { 82 });
        }
        unreachable!("SYS_exit returned");
    }
    // Reuse the existing raw followed-thread fixture's finite stack and
    // original clear-child-tid/futex join. No TLS destructor/madvise shortcut.
    let mut stack = vec![0u128; 16384];
    let tid = AtomicI32::new(0);
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) }.cast();
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    assert!(
        unsafe {
            libc::clone(
                child,
                top,
                flags,
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
                std::ptr::null_mut::<libc::c_void>(),
                tid.as_ptr(),
            )
        } > 0
    );
    let source = &ROOT;
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_write, 754, source.0.as_ptr(), source.0.len()) },
        8
    );
    loop {
        let current = tid.load(Ordering::Acquire);
        if current == 0 {
            break;
        }
        unsafe {
            libc::syscall(libc::SYS_futex, tid.as_ptr(), libc::FUTEX_WAIT, current, 0);
        }
    }
}
