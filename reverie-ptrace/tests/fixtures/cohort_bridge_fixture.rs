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
        Some("peer-sendto-root") => followed_peer_sendto(false, false),
        Some("peer-sendto-child") => followed_peer_sendto(true, false),
        Some("peer-sendto-blocked-root") => followed_peer_sendto(false, true),
        Some("peer-sendto-blocked-child") => followed_peer_sendto(true, true),
        Some("source-retirement-703") => retirement_source(),
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
