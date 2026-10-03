/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Regression coverage for a tracer thread's transient descriptor leaking into
//! a guest launch on another host thread
//! (https://github.com/rrnewton/reverie/issues/912, rows 25 and 26 of
//! https://github.com/rrnewton/reverie/issues/845).
//!
//! Tracer A runs a guest that takes a signal. At its signal-delivery stop the
//! tracer reads `/proc/thread-self/status` (`thread_may_be_seccomp_filtered`).
//! This binary interposes `open64` and holds that one descriptor open while
//! tracer B launches a guest with `spawn_fn_with_config`. Descriptors 3 to 255
//! are filled first, so the status descriptor is numbered 256 or above, which
//! the no-exec guest does not close. Without mutual exclusion between the two:
//!
//! - F-1: B's guest inherits the status descriptor and keeps it for its run.
//! - F-2: under a soft `RLIMIT_NOFILE` that fits exactly the launch's four pipe
//!   descriptors, the status descriptor takes one of their numbers and the
//!   launch fails with EMFILE.
//!
//! With the exclusion, B's launch waits until A closes the descriptor. A is
//! released either when B's launch returns (which, if it returned while A was
//! held, is the defect) or after `HOLD_AFTER_ENTRY`.

#![cfg(all(target_os = "linux", target_env = "gnu"))]

use std::ffi::CStr;
use std::ffi::CString;
use std::os::raw::c_char;
use std::os::raw::c_int;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::ExitStatus;
use reverie_ptrace::spawn_fn_with_config;
use reverie_ptrace::testing::run_tokio_test;

/// The path tracer A opens at a signal-delivery stop.
const STATUS_PATH: &CStr = c"/proc/thread-self/status";

/// How long A stays held after B entered its launch, if B's launch does not
/// return first. Long enough for an unguarded launch to fork.
const HOLD_AFTER_ENTRY: Duration = Duration::from_secs(5);

/// Bound on every wait in this test, so a hang reports as a failure.
const BOUND: Duration = Duration::from_secs(120);

/// Exit code of B's guest when it finds A's status descriptor open.
const INHERITED: i32 = 73;

/// The tests share the process's descriptor table and limits.
static SERIAL: Mutex<()> = Mutex::new(());

/// Set to hold the next open of `STATUS_PATH`.
static HOLD_NEXT_STATUS_OPEN: AtomicBool = AtomicBool::new(false);
/// The held status descriptor, or -1.
static HELD_FD: AtomicI32 = AtomicI32::new(-1);
/// What the held descriptor resolves to (`/proc/self/fd/N`).
static HELD_TARGET: Mutex<Option<CString>> = Mutex::new(None);

#[derive(Default)]
struct Hold {
    opened: bool,
    released: bool,
}

static HOLD: Mutex<Hold> = Mutex::new(Hold {
    opened: false,
    released: false,
});
static HOLD_CHANGED: Condvar = Condvar::new();

thread_local! {
    /// Set on B's launching thread to tighten `RLIMIT_NOFILE` at its first
    /// pipe (F-2).
    static TIGHTEN_AT_PIPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Pipes seen since tightening, and the limit to restore.
    static TIGHTENED: std::cell::Cell<Option<(u32, libc::rlimit)>> =
        const { std::cell::Cell::new(None) };
}

static REAL_OPEN64: AtomicUsize = AtomicUsize::new(0);
static REAL_PIPE: AtomicUsize = AtomicUsize::new(0);

fn real(cache: &AtomicUsize, name: &CStr) -> usize {
    let mut addr = cache.load(Ordering::Relaxed);
    if addr == 0 {
        // SAFETY: `name` is a valid C string; RTLD_NEXT finds libc's symbol.
        addr = unsafe { libc::dlsym(libc::RTLD_NEXT, name.as_ptr()) } as usize;
        assert_ne!(addr, 0, "dlsym({name:?}) failed");
        cache.store(addr, Ordering::Relaxed);
    }
    addr
}

/// Interposes libc's `open64`, which `std::fs::File::open` calls. The mode is
/// taken as a fixed third argument, which the x86-64 and AArch64 Linux calling
/// conventions pass where a variadic one goes.
///
/// # Safety
///
/// As libc's `open64`: `path` is a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open64(path: *const c_char, flags: c_int, mode: libc::mode_t) -> c_int {
    let real_open64: unsafe extern "C" fn(*const c_char, c_int, libc::mode_t) -> c_int =
        // SAFETY: `real` returns libc's `open64`.
        unsafe { std::mem::transmute(real(&REAL_OPEN64, c"open64")) };
    // SAFETY: the caller's arguments, passed through.
    let fd = unsafe { real_open64(path, flags, mode) };
    // SAFETY: `path` is the caller's C string.
    if fd >= 0
        && unsafe { CStr::from_ptr(path) } == STATUS_PATH
        && HOLD_NEXT_STATUS_OPEN.swap(false, Ordering::SeqCst)
    {
        hold(fd);
    }
    fd
}

/// Interposes libc's `pipe`, which `nix::unistd::pipe` calls.
///
/// # Safety
///
/// As libc's `pipe`: `fds` is writable for two descriptors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe(fds: *mut c_int) -> c_int {
    let real_pipe: unsafe extern "C" fn(*mut c_int) -> c_int =
        // SAFETY: `real` returns libc's `pipe`.
        unsafe { std::mem::transmute(real(&REAL_PIPE, c"pipe")) };
    if TIGHTEN_AT_PIPE
        .try_with(|t| t.replace(false))
        .unwrap_or(false)
    {
        TIGHTENED.with(|t| t.set(Some((0, tighten_nofile()))));
    }
    // SAFETY: the caller's argument, passed through.
    let rc = unsafe { real_pipe(fds) };
    if let Ok(Some((seen, saved))) = TIGHTENED.try_with(|t| t.get()) {
        if rc != 0 || seen + 1 == 2 {
            TIGHTENED.with(|t| t.set(None));
            // SAFETY: `saved` is the limit read before tightening.
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &saved) }, 0);
        } else {
            TIGHTENED.with(|t| t.set(Some((seen + 1, saved))));
        }
    }
    rc
}

/// Records `fd` as held and blocks until the test releases it.
fn hold(fd: c_int) {
    HELD_FD.store(fd, Ordering::SeqCst);
    *HELD_TARGET.lock().unwrap() = Some(fd_target(fd).expect("held descriptor has no target"));
    let mut hold = HOLD.lock().unwrap();
    hold.opened = true;
    HOLD_CHANGED.notify_all();
    let (_hold, timeout) = HOLD_CHANGED
        .wait_timeout_while(hold, BOUND, |h| !h.released)
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "the status descriptor was never released"
    );
}

/// What `/proc/self/fd/<fd>` links to, `None` if `fd` is not open.
fn fd_target(fd: c_int) -> Option<CString> {
    let link = CString::new(format!("/proc/self/fd/{fd}")).unwrap();
    let mut buf = [0u8; 4096];
    // SAFETY: `link` is a C string and `buf` is writable for its length.
    let n = unsafe { libc::readlink(link.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    (n > 0).then(|| CString::new(&buf[..n as usize]).unwrap())
}

/// Whether A's held status descriptor is open in this process.
fn held_fd_open() -> bool {
    let fd = HELD_FD.load(Ordering::SeqCst);
    let held = HELD_TARGET
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    fd >= 0 && held.is_some() && fd_target(fd) == held
}

fn fd_free(fd: c_int) -> bool {
    // SAFETY: F_GETFD only reads the descriptor table.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags == -1
}

/// Sets the soft `RLIMIT_NOFILE` to fit exactly the four lowest descriptor
/// numbers that would be free if A's status descriptor were closed, and
/// returns the previous limit.
fn tighten_nofile() -> libc::rlimit {
    let mut saved = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `saved` is writable.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut saved) },
        0
    );
    let held = held_fd_open().then(|| HELD_FD.load(Ordering::SeqCst));
    let mut available = 0;
    let mut fd = 0;
    loop {
        if fd_free(fd) || Some(fd) == held {
            available += 1;
            if available == 4 {
                break;
            }
        }
        fd += 1;
    }
    let tight = libc::rlimit {
        rlim_cur: fd as libc::rlim_t + 1,
        rlim_max: saved.rlim_max,
    };
    // SAFETY: `tight` is a valid limit no higher than the hard limit.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &tight) }, 0);
    saved
}

/// Fills every free descriptor from 3 to 255 and closes them on drop.
struct LowDescriptorsFilled(Vec<c_int>);

impl LowDescriptorsFilled {
    fn new() -> Self {
        // SAFETY: plain open of a constant path.
        let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(null >= 0);
        let mut filled = Vec::new();
        for fd in 3..256 {
            if fd != null && fd_free(fd) {
                // SAFETY: `fd` is free and `null` is open.
                assert_eq!(unsafe { libc::dup3(null, fd, libc::O_CLOEXEC) }, fd);
                filled.push(fd);
            }
        }
        if null >= 256 {
            // SAFETY: our own descriptor.
            unsafe { libc::close(null) };
        } else {
            filled.push(null);
        }
        Self(filled)
    }
}

impl Drop for LowDescriptorsFilled {
    fn drop(&mut self) {
        for &fd in &self.0 {
            // SAFETY: descriptors this guard opened.
            unsafe { libc::close(fd) };
        }
    }
}

/// Guest of tracer A: takes one handled SIGUSR1.
fn take_a_signal() {
    extern "C" fn on_usr1(_: c_int) {}
    // SAFETY: installs a no-op handler, then raises the signal at itself.
    unsafe {
        libc::signal(libc::SIGUSR1, on_usr1 as *const () as libc::sighandler_t);
        libc::raise(libc::SIGUSR1);
    }
}

/// Guest of tracer B for F-1: exits `INHERITED` if A's status descriptor is
/// open in it. The guest is a fork of this process, so it reads the held
/// descriptor's number and target from the copied statics.
fn check_not_inherited() {
    if held_fd_open() {
        // SAFETY: ends the guest without running the test process's exit
        // handlers.
        unsafe { libc::_exit(INHERITED) };
    }
}

/// How B's launch ended, and whether A was still held when it returned.
struct Outcome {
    launch: Result<ExitStatus, String>,
    returned_while_held: bool,
}

/// Holds A's next status read open, runs B's launch of `guest` meanwhile, and
/// releases A when B's launch returns or `HOLD_AFTER_ENTRY` after it began.
fn launch_during_status_read(guest: fn(), tighten_nofile: bool) -> Outcome {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _filled = LowDescriptorsFilled::new();
    *HOLD.lock().unwrap() = Hold::default();
    HELD_FD.store(-1, Ordering::SeqCst);
    *HELD_TARGET.lock().unwrap() = None;
    HOLD_NEXT_STATUS_OPEN.store(true, Ordering::SeqCst);

    let tracer_a = std::thread::spawn(|| {
        run_tokio_test(async {
            let tracer = spawn_fn_with_config::<(), _>(take_a_signal, (), false).await?;
            tracer.wait().await
        })
    });

    {
        let hold = HOLD.lock().unwrap();
        let (_hold, timeout) = HOLD_CHANGED
            .wait_timeout_while(hold, BOUND, |h| !h.opened)
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "tracer A never opened {STATUS_PATH:?}; the test reached no overlap"
        );
    }
    assert!(held_fd_open());
    assert!(
        HELD_FD.load(Ordering::SeqCst) >= 256,
        "the held descriptor must be one the guest does not close"
    );

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (returned_tx, returned_rx) = std::sync::mpsc::channel();
    let tracer_b = std::thread::spawn(move || {
        run_tokio_test(async move {
            entered_tx.send(()).unwrap();
            TIGHTEN_AT_PIPE.with(|t| t.set(tighten_nofile));
            let launched = spawn_fn_with_config::<(), _>(guest, (), false).await;
            TIGHTEN_AT_PIPE.with(|t| t.set(false));
            returned_tx.send(held_fd_open()).unwrap();
            match launched {
                Ok(tracer) => tracer
                    .wait()
                    .await
                    .map(|(status, ())| status)
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        })
    });

    entered_rx.recv_timeout(BOUND).unwrap();
    let entered = Instant::now();
    let returned_while_held = returned_rx.recv_timeout(HOLD_AFTER_ENTRY).unwrap_or(false);
    eprintln!(
        "launch_descriptor_window: A held descriptor {} ({:?}), released {:?} after B \
         entered its launch ({})",
        HELD_FD.load(Ordering::SeqCst),
        HELD_TARGET.lock().unwrap().as_deref().unwrap_or_default(),
        entered.elapsed(),
        if returned_while_held {
            "B's launch returned while A held the descriptor"
        } else {
            "B's launch had not returned while A held the descriptor"
        }
    );
    {
        let mut hold = HOLD.lock().unwrap();
        hold.released = true;
        HOLD_CHANGED.notify_all();
    }

    let launch = tracer_b.join().unwrap();
    let a = tracer_a.join().unwrap();
    assert_eq!(a.map(|(status, ())| status).unwrap(), ExitStatus::Exited(0));
    Outcome {
        launch,
        returned_while_held,
    }
}

/// F-1: B's guest must not inherit A's transient descriptor.
#[test]
fn a_launch_does_not_inherit_another_threads_transient_descriptor() {
    let outcome = launch_during_status_read(check_not_inherited, false);
    assert_eq!(
        outcome.launch,
        Ok(ExitStatus::Exited(0)),
        "B's guest exited {INHERITED} if it inherited A's status descriptor"
    );
    assert!(!outcome.returned_while_held);
}

/// F-2: B's launch must not lose a pipe descriptor to A's transient one.
#[test]
fn a_launch_does_not_run_out_of_descriptors_to_another_threads_transient_one() {
    let outcome = launch_during_status_read(|| {}, true);
    assert_eq!(outcome.launch, Ok(ExitStatus::Exited(0)));
    assert!(!outcome.returned_while_held);
}
