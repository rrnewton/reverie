/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A launch whose `pipe2` fails with EMFILE because another thread's
//! transient open, which began during the launch after
//! `launch_window::OPEN_WAIT_LIMIT`, still holds the last free descriptor,
//! waits for that open to close and completes
//! (<https://github.com/rrnewton/reverie/issues/930>, round-7 R7-1 of
//! <https://github.com/rrnewton/reverie/issues/912>).
//!
//! A user-notification supervisor in this process holds the launcher's
//! `pipe2` until another thread has opened a descriptor late and is closing
//! it; a second supervisor holds that close until the launcher waits for it.
//! `RLIMIT_NOFILE` leaves exactly the two descriptors the launch needs, so
//! the first `pipe2` fails, and only a retry after the close succeeds. No
//! privilege is needed.

// The filters match x86_64 system call numbers only.
#![cfg(target_arch = "x86_64")]

use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use reverie_process::Command;
use reverie_process::ExitStatus;
use reverie_process::launch_window;

/// `_IOWR('!', 0, struct seccomp_notif)` and `_IOWR('!', 1, struct
/// seccomp_notif_resp)` from `<linux/seccomp.h>`.
const SECCOMP_IOCTL_NOTIF_RECV: libc::c_ulong = 0xc050_2100;
const SECCOMP_IOCTL_NOTIF_SEND: libc::c_ulong = 0xc018_2101;
/// `AUDIT_ARCH_X86_64` from `<linux/audit.h>`.
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

/// How long the test waits for any one step before it reports the step.
const BOUND: Duration = Duration::from_secs(30);

static STOP: AtomicBool = AtomicBool::new(false);
/// Set when the helper may make its late open.
static HELPER_GO: AtomicBool = AtomicBool::new(false);
/// Set when the launch may begin.
static LAUNCHER_GO: AtomicBool = AtomicBool::new(false);
/// Set once the helper's open began while a launch held the lock.
static OPENED_DURING_LAUNCH: AtomicBool = AtomicBool::new(false);
/// Set once the close supervisor holds the helper's close.
static CLOSE_HELD: AtomicBool = AtomicBool::new(false);
/// Set once the launcher's spawn returned.
static SPAWN_RETURNED: AtomicBool = AtomicBool::new(false);
/// `pipe2` notifications the launcher made.
static PIPE2_NOTIFIED: AtomicU32 = AtomicU32::new(0);
/// Whether the close supervisor saw the launcher wait on the lock's futex
/// while it held the close.
static LAUNCHER_WAITED: AtomicBool = AtomicBool::new(false);

/// Installs, on the calling thread only, a filter that hands every call of
/// system call `nr` to a user-notification listener, and returns the
/// listener.
fn notify_on_this_thread(nr: libc::c_long) -> libc::c_int {
    let arch = std::mem::offset_of!(libc::seccomp_data, arch) as u32;
    let nr_offset = std::mem::offset_of!(libc::seccomp_data, nr) as u32;
    let stmt = |code: u32, k: u32| libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    };
    let jeq = |k: u32, jt: u8, jf: u8| libc::sock_filter {
        code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
        jt,
        jf,
        k,
    };
    let filter = [
        stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, arch),
        jeq(AUDIT_ARCH_X86_64, 0, 3),
        stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, nr_offset),
        jeq(nr as u32, 0, 1),
        stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_USER_NOTIF),
        stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW),
    ];
    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };
    // SAFETY: `prog` points at `filter`, which outlives both calls; neither
    // flag synchronizes other threads, so only this thread is filtered.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        let listener = libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &prog as *const libc::sock_fprog,
        );
        assert!(
            listener >= 0,
            "seccomp listener: {}",
            std::io::Error::last_os_error()
        );
        listener as libc::c_int
    }
}

/// Receives the next notification on `listener`, or `None` once `STOP` is
/// set. Opens no descriptor.
fn receive(listener: libc::c_int) -> Option<libc::seccomp_notif> {
    while !STOP.load(Ordering::SeqCst) {
        let mut ready = libc::pollfd {
            fd: listener,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: polls one descriptor this test owns.
        if unsafe { libc::poll(&mut ready, 1, 10) } != 1 {
            continue;
        }
        // SAFETY: zeroed is a valid `seccomp_notif`, which RECV fills.
        let mut request: libc::seccomp_notif = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::ioctl(listener, SECCOMP_IOCTL_NOTIF_RECV, &mut request) } == 0 {
            return Some(request);
        }
    }
    None
}

/// Lets the notified system call `id` run.
fn continue_call(listener: libc::c_int, id: u64) {
    let response = libc::seccomp_notif_resp {
        id,
        val: 0,
        error: 0,
        flags: libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32,
    };
    // SAFETY: answers a notification just received. It fails only if the
    // notifying thread is gone, which the test reports elsewhere.
    unsafe { libc::ioctl(listener, SECCOMP_IOCTL_NOTIF_SEND, &response) };
}

/// Waits until `flag` is set, for at most [`BOUND`]. Returns whether it was.
fn wait_for(flag: &AtomicBool) -> bool {
    let deadline = Instant::now() + BOUND;
    while !flag.load(Ordering::SeqCst) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// Holds the launcher's first `pipe2` until the helper has opened a
/// descriptor during the launch and its close is held, then lets it run.
/// Every later `pipe2` runs at once.
fn supervise_pipe2(listener: libc::c_int) {
    while let Some(request) = receive(listener) {
        if PIPE2_NOTIFIED.fetch_add(1, Ordering::SeqCst) == 0 {
            HELPER_GO.store(true, Ordering::SeqCst);
            wait_for(&CLOSE_HELD);
        }
        continue_call(listener, request.id);
    }
}

/// Holds the helper's close until the launcher is seen waiting on the lock's
/// futex, or its spawn has returned, then lets it run. `launcher_syscall` is
/// the launcher's `/proc/.../syscall`, opened before the descriptors ran out.
fn supervise_close(listener: libc::c_int, launcher_syscall: std::fs::File) {
    // `/proc/.../syscall` reads "<nr> <arg0> <arg1> ...": a futex call whose
    // operation, its second argument, is a private wait.
    let waiting = format!("{} ", libc::SYS_futex);
    let wait_op = format!("{:#x}", libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG);
    let mut buf = [0u8; 256];
    while let Some(request) = receive(listener) {
        if !CLOSE_HELD.swap(true, Ordering::SeqCst) {
            let deadline = Instant::now() + BOUND;
            while !SPAWN_RETURNED.load(Ordering::SeqCst) && Instant::now() < deadline {
                // SAFETY: reads into a buffer this function owns.
                let n = unsafe {
                    libc::pread(
                        launcher_syscall.as_raw_fd(),
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                        0,
                    )
                };
                let line = std::str::from_utf8(&buf[..n.max(0) as usize]).unwrap_or("");
                if line.starts_with(&waiting) && line.split(' ').nth(2) == Some(wait_op.as_str()) {
                    LAUNCHER_WAITED.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        continue_call(listener, request.id);
    }
}

/// Lowers the soft `RLIMIT_NOFILE` and fills every free descriptor below it
/// but two, and returns the fillers and the old limit.
fn leave_two_descriptors() -> (Vec<std::fs::File>, libc::rlimit) {
    let highest = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u64>().ok())
        .max()
        .unwrap();
    let mut old = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain getrlimit/setrlimit on valid structs.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut old), 0);
        let limit = libc::rlimit {
            rlim_cur: (highest + 64).min(old.rlim_max),
            rlim_max: old.rlim_max,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
    let mut fillers = Vec::new();
    loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => fillers.push(file),
            Err(err) if err.raw_os_error() == Some(libc::EMFILE) => break,
            Err(err) => panic!("filling the descriptor table: {err}"),
        }
    }
    assert!(fillers.len() >= 2, "no descriptor was free to fill");
    fillers.truncate(fillers.len() - 2);
    (fillers, old)
}

#[test]
fn a_launch_retries_a_pipe_after_a_late_open_releases_its_descriptor() {
    let (launcher_tx, launcher_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    // Detached: if the launch hangs, its thread can never be joined.
    std::thread::spawn(move || {
        launcher_tx
            .send((
                notify_on_this_thread(libc::SYS_pipe2),
                // SAFETY: gettid has no preconditions.
                unsafe { libc::gettid() },
            ))
            .unwrap();
        wait_for(&LAUNCHER_GO);
        let status = Command::new("/bin/true")
            .spawn()
            .map_err(|e| e.to_string())
            .and_then(|mut child| child.wait_blocking().map_err(|e| e.to_string()));
        SPAWN_RETURNED.store(true, Ordering::SeqCst);
        done_tx.send(status).unwrap();
    });
    let (helper_tx, helper_rx) = mpsc::channel();
    std::thread::spawn(move || {
        helper_tx
            .send(notify_on_this_thread(libc::SYS_close))
            .unwrap();
        wait_for(&HELPER_GO);
        let open = launch_window::TransientOpen::begin();
        OPENED_DURING_LAUNCH.store(launch_window::launching(), Ordering::SeqCst);
        // Takes one of the launch's two descriptors; its close is held.
        drop(std::fs::File::open("/dev/null").unwrap());
        drop(open);
    });
    let (pipe2_listener, launcher_tid) = launcher_rx.recv().unwrap();
    let close_listener = helper_rx.recv().unwrap();
    let launcher_syscall =
        std::fs::File::open(format!("/proc/self/task/{launcher_tid}/syscall")).unwrap();
    std::thread::spawn(move || supervise_pipe2(pipe2_listener));
    std::thread::spawn(move || supervise_close(close_listener, launcher_syscall));

    let (fillers, old_limit) = leave_two_descriptors();
    LAUNCHER_GO.store(true, Ordering::SeqCst);
    let status = done_rx.recv_timeout(BOUND);
    STOP.store(true, Ordering::SeqCst);
    drop(fillers);
    // SAFETY: restores the limit read above.
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &old_limit) };

    let status = status.unwrap_or_else(|_| panic!("the launch did not return within {BOUND:?}"));
    eprintln!(
        "launch_late_open_descriptor: {} pipe2 notification(s); open during launch {}; \
         launcher waited on the lock {}",
        PIPE2_NOTIFIED.load(Ordering::SeqCst),
        OPENED_DURING_LAUNCH.load(Ordering::SeqCst),
        LAUNCHER_WAITED.load(Ordering::SeqCst),
    );
    assert!(
        OPENED_DURING_LAUNCH.load(Ordering::SeqCst) && CLOSE_HELD.load(Ordering::SeqCst),
        "the helper's open did not begin during the launch with its close held; the case was \
         not exercised"
    );
    assert_eq!(
        status,
        Ok(ExitStatus::Exited(0)),
        "the launch failed while an open that began during it held a descriptor it needed"
    );
    assert!(
        PIPE2_NOTIFIED.load(Ordering::SeqCst) >= 2,
        "the launch's pipe2 was not retried"
    );
}
