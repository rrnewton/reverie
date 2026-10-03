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
//! One process-wide lock excludes the two across threads: [`Launch`] is held
//! exclusively from a launch's first descriptor allocation until `fork` or
//! `clone` returns in the parent, and [`TransientOpen`] is held shared from a
//! transient open to its close. Transient opens of different threads never
//! wait for each other, only for another thread's launch in progress. A launch
//! waits until no other thread holds a transient open. Neither guard may be
//! held across an `.await`.
//!
//! A thread never waits for its own guards. An open inside the thread's own
//! launch, or a launch nested inside it (a `pthread_atfork` prepare handler
//! that launches, for example), proceeds and leaves the outer launch held. So
//! does a launch inside the thread's own open, which then becomes part of the
//! launch until the launch ends; that open's descriptor reaches the child, as
//! it would without the lock. Reverie's own guarded scopes therefore run no
//! callbacks (no logging, no locks another thread may hold while launching).
//!
//! A launch must not hold [`Launch`] while it waits for the new child to start
//! (an exec-error pipe, a seccomp-notification handoff): the child's setup can
//! wait on another tracer thread, and that thread may need a transient open.
//! Descriptors a launch keeps open after `clone` returns, such as its
//! exec-error reader, are therefore not excluded from other threads' launches
//! while the child starts.
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
    /// Transient opens this thread holds that `STATE` counts.
    static OPENS_HELD: Cell<u32> = const { Cell::new(0) };
    /// How many [`Launch`] guards this thread holds, nested.
    static LAUNCH_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Transient opens this thread holds while it holds the launch. `STATE`
    /// does not count them; they move to `OPENS_HELD` when the launch ends.
    static OPENS_IN_LAUNCH: Cell<u32> = const { Cell::new(0) };
}

/// Runs in the child of every `fork` that goes through libc, where only the
/// forking thread survives: the lock keeps that thread's transient opens and
/// nothing else, so a guard the child inherited from another thread cannot
/// block it forever. A launch the forking thread held is over in the child;
/// the child's copy of its guard releases nothing.
extern "C" fn reset_in_child() {
    let held = OPENS_HELD.try_with(Cell::get).unwrap_or(0)
        + OPENS_IN_LAUNCH
            .try_with(|opens| opens.replace(0))
            .unwrap_or(0);
    let _ = OPENS_HELD.try_with(|opens| opens.set(held));
    let _ = LAUNCH_DEPTH.try_with(|depth| depth.set(0));
    STATE.store(held, Ordering::Relaxed);
}

fn register_child_reset() {
    CHILD_RESET.call_once(|| {
        // SAFETY: `reset_in_child` only touches an atomic and three
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
    /// Waits until no other thread holds a transient open or a launch.
    pub fn begin() -> Self {
        register_child_reset();
        let depth = LAUNCH_DEPTH.with(Cell::get);
        if depth == 0 {
            // This thread's own opens stay counted until the launch takes
            // them over; only other threads' opens are waited for.
            let own = OPENS_HELD.with(Cell::get);
            loop {
                match STATE.compare_exchange_weak(
                    own,
                    LAUNCHING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) if observed == own => {}
                    Err(observed) => wait_while(observed),
                }
            }
            OPENS_HELD.with(|opens| opens.set(0));
            OPENS_IN_LAUNCH.with(|opens| opens.set(own));
        }
        LAUNCH_DEPTH.with(|launches| launches.set(depth + 1));
        Launch {
            _not_send: PhantomData,
        }
    }
}

impl Drop for Launch {
    /// Ends the launch when the thread's outermost guard drops. In a forked
    /// child the fork handler has already ended it, so this does nothing.
    fn drop(&mut self) {
        let depth = LAUNCH_DEPTH.with(Cell::get);
        if depth == 0 {
            return;
        }
        LAUNCH_DEPTH.with(|launches| launches.set(depth - 1));
        if depth > 1 {
            return;
        }
        let own = OPENS_IN_LAUNCH.with(|opens| opens.replace(0));
        OPENS_HELD.with(|opens| opens.set(own));
        STATE.store(own, Ordering::Release);
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
        if LAUNCH_DEPTH.with(Cell::get) > 0 {
            OPENS_IN_LAUNCH.with(|opens| opens.set(opens.get() + 1));
            return TransientOpen {
                _not_send: PhantomData,
            };
        }
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
        // While this thread holds the launch, every open it holds is one the
        // launch took over or one opened inside it.
        if LAUNCH_DEPTH.with(Cell::get) > 0 {
            OPENS_IN_LAUNCH.with(|opens| opens.set(opens.get() - 1));
            return;
        }
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
    use std::sync::mpsc;
    use std::time::Duration;
    use std::time::Instant;

    use super::*;

    // These tests share the process-wide lock with every other test in this
    // binary, so they only assert orderings that hold whatever else runs.

    fn gettid() -> libc::pid_t {
        // SAFETY: gettid has no preconditions.
        unsafe { libc::gettid() }
    }

    /// Returns once thread `tid` is seen asleep in this lock's futex wait
    /// (`FUTEX_WAIT_PRIVATE` on `STATE`), and fails after 10 s. A contender
    /// that has not reached the wait yet, or waits on anything else, does not
    /// count.
    fn wait_until_asleep_on_the_lock(tid: libc::pid_t) {
        let expected = format!(
            "{} {:#x} {:#x} ",
            libc::SYS_futex,
            STATE.as_ptr() as usize,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let syscall = std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall"))
                .unwrap_or_default();
            if syscall.starts_with(&expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "thread {tid} was not seen waiting on the lock within 10 s (last: {syscall:?})"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

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
        let (tid_tx, tid_rx) = mpsc::channel();
        let launcher = {
            let launched = Arc::clone(&launched);
            std::thread::spawn(move || {
                tid_tx.send(gettid()).unwrap();
                let _launch = Launch::begin();
                launched.store(true, Ordering::SeqCst);
            })
        };
        wait_until_asleep_on_the_lock(tid_rx.recv().unwrap());
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
        let (tid_tx, tid_rx) = mpsc::channel();
        let reader = {
            let opened = Arc::clone(&opened);
            std::thread::spawn(move || {
                tid_tx.send(gettid()).unwrap();
                let _open = TransientOpen::begin();
                opened.store(true, Ordering::SeqCst);
            })
        };
        wait_until_asleep_on_the_lock(tid_rx.recv().unwrap());
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
        let (tid_tx, tid_rx) = mpsc::channel();
        let launcher = {
            let launched = Arc::clone(&launched);
            std::thread::spawn(move || {
                tid_tx.send(gettid()).unwrap();
                let _launch = Launch::begin();
                launched.store(true, Ordering::SeqCst);
            })
        };
        // The release below must find the launcher already asleep on the
        // lock, so that only a timed-out wait can let it proceed.
        wait_until_asleep_on_the_lock(tid_rx.recv().unwrap());
        assert!(
            !launched.load(Ordering::SeqCst),
            "launch ran during a transient open"
        );
        release.store(true, Ordering::SeqCst);
        assert!(
            holder.join().unwrap(),
            "the test filter did not deny the wake"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !launched.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            launched.load(Ordering::SeqCst),
            "the launch was still waiting 5 s after a release whose wake was denied"
        );
        launcher.join().unwrap();
    }

    #[test]
    fn a_thread_never_waits_for_its_own_guards() {
        let open = TransientOpen::begin();
        // A launch inside the thread's own open, an open inside its launch,
        // and a launch nested in that, as from a `pthread_atfork` prepare
        // handler.
        let launch = Launch::begin();
        let inner_open = TransientOpen::begin();
        let inner_launch = Launch::begin();
        drop(inner_launch);
        drop(inner_open);
        assert_eq!(
            STATE.load(Ordering::SeqCst),
            LAUNCHING,
            "the outer launch must stay held after the nested one ends"
        );

        // Another thread's open still waits for the outer launch.
        let opened = Arc::new(AtomicBool::new(false));
        let (tid_tx, tid_rx) = mpsc::channel();
        let reader = {
            let opened = Arc::clone(&opened);
            std::thread::spawn(move || {
                tid_tx.send(gettid()).unwrap();
                let _open = TransientOpen::begin();
                opened.store(true, Ordering::SeqCst);
            })
        };
        wait_until_asleep_on_the_lock(tid_rx.recv().unwrap());
        assert!(
            !opened.load(Ordering::SeqCst),
            "another thread's open ran during the outer launch"
        );
        drop(launch);
        reader.join().unwrap();
        assert!(opened.load(Ordering::SeqCst));
        // The first open outlived the launch and is counted again.
        assert_eq!(OPENS_HELD.with(Cell::get), 1);
        drop(open);
        assert_eq!(OPENS_HELD.with(Cell::get), 0);
    }
}
