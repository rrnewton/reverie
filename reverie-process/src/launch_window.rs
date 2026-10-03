/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Keeps the tracer's transient descriptors out of guest launches on other
//! host threads of the same process.
//!
//! A *transient open* is a descriptor a tracer thread opens and closes inside
//! one synchronous call, such as a procfs read. A *launch* is the parent-side
//! sequence that allocates a new child's descriptors (pipes) and forks it. If
//! a transient open overlaps another thread's launch, the descriptor takes a
//! number from the launch's descriptor budget (its pipe can fail with EMFILE
//! under a tight `RLIMIT_NOFILE`), and the forked child inherits it. A no-exec
//! guest (`spawn_fn_with_config`) closes only descriptors 3 to 255, so it keeps
//! a numbered-256-or-above copy for its whole run
//! (<https://github.com/rrnewton/reverie/issues/912>, rows 25 and 26 of
//! <https://github.com/rrnewton/reverie/issues/845>).
//!
//! One process-wide lock excludes the two: [`Launch`] is held exclusively from
//! a launch's first descriptor allocation until `fork` or `clone` returns in
//! the parent, and [`TransientOpen`] is held shared from a transient open to
//! its close. Transient opens never wait for each other, only for a launch in
//! progress, so a thread may nest them. A launch waits until no transient open
//! is held. Neither guard may be held across an `.await`, and a thread holding
//! a [`TransientOpen`] must not launch (that would wait for itself; it panics
//! instead).
//!
//! A launch must not hold [`Launch`] while it waits for the new child to start
//! (an exec-error pipe, a seccomp-notification handoff): the child's setup can
//! wait on another tracer thread, and that thread may need a transient open.
//! Descriptors the parent creates after `clone` returns are therefore outside
//! the lock; the child already exists and cannot inherit them.
//!
//! A child forked through libc runs a `pthread_atfork` handler, and a child of
//! this crate's raw `clone` resets the lock at entry, so neither inherits a
//! guard another thread held. A child made by a raw `clone`/`clone3` elsewhere
//! must not take either guard.
//!
//! The lock lives here, below `reverie-core`, `safeptrace` and
//! `reverie-ptrace`, so that the transient opens in all three take the same
//! lock. It is not part of Reverie's tool API.
//!
//! Descriptors held beyond one call (pidfds, perf counters, a gdb client's
//! files) and forks outside Reverie are not covered.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Once;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

/// Set while a launch holds the lock. The other bits count transient opens.
const LAUNCHING: u32 = 1 << 31;

static STATE: AtomicU32 = AtomicU32::new(0);

static CHILD_RESET: Once = Once::new();

thread_local! {
    /// Transient opens this thread holds.
    static OPENS_HELD: Cell<u32> = const { Cell::new(0) };
    /// Whether this thread holds the launch.
    static LAUNCHING_HERE: Cell<bool> = const { Cell::new(false) };
}

/// Runs in the child of every `fork` that goes through libc, where only the
/// forking thread survives: the lock keeps that thread's transient opens and
/// nothing else, so a guard the child inherited from another thread cannot
/// block it forever.
extern "C" fn reset_in_child() {
    let held = OPENS_HELD.try_with(Cell::get).unwrap_or(0);
    STATE.store(held, Ordering::Relaxed);
    let _ = LAUNCHING_HERE.try_with(|here| here.set(false));
}

fn register_child_reset() {
    CHILD_RESET.call_once(|| {
        // SAFETY: `reset_in_child` only touches an atomic and two
        // constant-initialized thread-locals without destructors, which is
        // async-signal-safe.
        let rc = unsafe { libc::pthread_atfork(None, None, Some(reset_in_child)) };
        assert_eq!(rc, 0, "pthread_atfork failed: {rc}");
    });
}

/// Resets the lock in a child of a raw `clone` without `CLONE_VM`, which runs
/// no `pthread_atfork` handler. Allocation-free and async-signal-safe.
pub(crate) fn reset_after_raw_clone() {
    reset_in_child();
}

/// How long a waiter sleeps before it rechecks the lock without a wake.
///
/// A release's `FUTEX_WAKE` can fail: a host seccomp filter on the releasing
/// thread may deny it, and the tracer cannot see or fix that. A bounded wait
/// makes every waiter recheck on its own, so a lost wake costs at most this
/// long instead of blocking the waiter forever.
const RECHECK_AFTER: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 10_000_000,
};

fn wait_while(observed: u32) {
    // SAFETY: STATE is an aligned u32 that lives for the whole process, and
    // the timeout is a valid relative timespec.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            STATE.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            observed,
            &RECHECK_AFTER as *const libc::timespec,
        )
    };
}

fn wake_all() {
    // SAFETY: as in `wait_while`.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            STATE.as_ptr(),
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            i32::MAX,
        )
    };
}

/// Exclusive hold for one guest launch's descriptor allocation and fork.
#[must_use]
pub struct Launch {
    _not_send: PhantomData<*const ()>,
}

impl Launch {
    /// Waits until no transient open is held and no other launch runs.
    pub fn begin() -> Self {
        register_child_reset();
        assert_eq!(
            OPENS_HELD.with(Cell::get),
            0,
            "a guest launch on a thread holding a transient open would wait for itself"
        );
        loop {
            match STATE.compare_exchange_weak(0, LAUNCHING, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => break,
                Err(0) => {}
                Err(observed) => wait_while(observed),
            }
        }
        LAUNCHING_HERE.with(|here| here.set(true));
        Launch {
            _not_send: PhantomData,
        }
    }
}

impl Drop for Launch {
    /// Ends the launch. In a forked child this releases the child's own copy,
    /// which the fork handler has already cleared.
    fn drop(&mut self) {
        LAUNCHING_HERE.with(|here| here.set(false));
        STATE.fetch_and(!LAUNCHING, Ordering::Release);
        wake_all();
    }
}

/// Shared hold for one transient descriptor, from its open to its close.
#[must_use]
pub struct TransientOpen {
    _not_send: PhantomData<*const ()>,
}

impl TransientOpen {
    /// Waits only while another thread's launch is in progress.
    pub fn begin() -> Self {
        register_child_reset();
        assert!(
            !LAUNCHING_HERE.with(Cell::get),
            "a transient open inside this thread's own launch would wait for itself"
        );
        let mut observed = STATE.load(Ordering::Relaxed);
        loop {
            if observed & LAUNCHING != 0 {
                wait_while(observed);
                observed = STATE.load(Ordering::Relaxed);
                continue;
            }
            match STATE.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(now) => observed = now,
            }
        }
        OPENS_HELD.with(|held| held.set(held.get() + 1));
        TransientOpen {
            _not_send: PhantomData,
        }
    }
}

impl Drop for TransientOpen {
    fn drop(&mut self) {
        OPENS_HELD.with(|held| held.set(held.get() - 1));
        if STATE.fetch_sub(1, Ordering::Release) == 1 {
            wake_all();
        }
    }
}

/// [`std::fs::read_to_string`] as one transient open.
pub fn read_to_string<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<String> {
    let _open = TransientOpen::begin();
    std::fs::read_to_string(path)
}

/// [`std::fs::read`] as one transient open.
pub fn read<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<Vec<u8>> {
    let _open = TransientOpen::begin();
    std::fs::read(path)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::*;

    // These tests share the process-wide lock with every other test in this
    // binary, so they only assert orderings that hold whatever else runs.

    #[test]
    fn transient_opens_nest_on_one_thread() {
        let outer = TransientOpen::begin();
        let inner = TransientOpen::begin();
        assert_eq!(OPENS_HELD.with(Cell::get), 2);
        drop(inner);
        drop(outer);
        assert_eq!(OPENS_HELD.with(Cell::get), 0);
    }

    #[test]
    fn a_launch_waits_for_a_transient_open_to_close() {
        let open = TransientOpen::begin();
        let launched = Arc::new(AtomicBool::new(false));
        let launcher = {
            let launched = Arc::clone(&launched);
            std::thread::spawn(move || {
                let _launch = Launch::begin();
                launched.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !launched.load(Ordering::SeqCst),
            "launch ran during a transient open"
        );
        drop(open);
        launcher.join().unwrap();
        assert!(launched.load(Ordering::SeqCst));
    }

    #[test]
    fn a_transient_open_waits_for_a_launch_to_finish() {
        let launch = Launch::begin();
        let opened = Arc::new(AtomicBool::new(false));
        let reader = {
            let opened = Arc::clone(&opened);
            std::thread::spawn(move || {
                let _open = TransientOpen::begin();
                opened.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !opened.load(Ordering::SeqCst),
            "transient open ran during a launch"
        );
        drop(launch);
        reader.join().unwrap();
        assert!(opened.load(Ordering::SeqCst));
    }

    /// Installs a filter on the calling thread only that fails this module's
    /// `FUTEX_WAKE` with EPERM, as a host sandbox might.
    fn deny_wake_on_this_thread() {
        const fn stmt(code: u32, k: u32) -> libc::sock_filter {
            libc::sock_filter {
                code: code as u16,
                jt: 0,
                jf: 0,
                k,
            }
        }
        const fn jeq_else_allow(k: u32, to_allow: u8) -> libc::sock_filter {
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: to_allow,
                k,
            }
        }
        let load = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
        // struct seccomp_data: nr at 0, args[1] at 24, args[2] at 32; the low
        // halves come first on little-endian hosts.
        let filter = [
            stmt(load, 0),
            jeq_else_allow(libc::SYS_futex as u32, 5),
            stmt(load, 24),
            jeq_else_allow((libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG) as u32, 3),
            stmt(load, 32),
            jeq_else_allow(i32::MAX as u32, 1),
            stmt(
                libc::BPF_RET | libc::BPF_K,
                libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
            ),
            stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW),
        ];
        let prog = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr() as *mut libc::sock_filter,
        };
        // SAFETY: plain prctl calls; the program outlives the install call.
        unsafe {
            assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
            assert_eq!(
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &prog as *const libc::sock_fprog,
                ),
                0,
                "installing the test filter failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    #[test]
    fn a_waiting_launch_proceeds_when_the_release_wake_is_denied() {
        let opened = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let holder = {
            let (opened, release) = (Arc::clone(&opened), Arc::clone(&release));
            std::thread::spawn(move || {
                let open = TransientOpen::begin();
                opened.store(true, Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                deny_wake_on_this_thread();
                // SAFETY: the same wake the release issues, to check the
                // filter denies it.
                let rc = unsafe {
                    libc::syscall(
                        libc::SYS_futex,
                        STATE.as_ptr(),
                        libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                        i32::MAX,
                    )
                };
                let denied =
                    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
                drop(open);
                denied
            })
        };
        while !opened.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let launched = Arc::new(AtomicBool::new(false));
        let launcher = {
            let launched = Arc::clone(&launched);
            std::thread::spawn(move || {
                let _launch = Launch::begin();
                launched.store(true, Ordering::SeqCst);
            })
        };
        // Let the launcher reach its wait before the release.
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !launched.load(Ordering::SeqCst),
            "launch ran during a transient open"
        );
        release.store(true, Ordering::SeqCst);
        assert!(
            holder.join().unwrap(),
            "the test filter did not deny the wake"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !launched.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            launched.load(Ordering::SeqCst),
            "the launch was still waiting 5 s after a release whose wake was denied"
        );
        launcher.join().unwrap();
    }

    #[test]
    #[should_panic(expected = "would wait for itself")]
    fn a_launch_while_holding_a_transient_open_panics() {
        let _open = TransientOpen::begin();
        let _launch = Launch::begin();
    }
}
