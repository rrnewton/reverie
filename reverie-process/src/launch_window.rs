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
//! a launch's first descriptor allocation until `fork` returns in the parent,
//! and [`TransientOpen`] is held shared from a transient open to its close.
//! Transient opens never wait for each other, only for a launch in progress,
//! so a thread may nest them. A launch waits until no transient open is held.
//! Neither guard may be held across an `.await`, and a thread holding a
//! [`TransientOpen`] must not launch (that would wait for itself; it panics
//! instead).
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

fn wait_while(observed: u32) {
    // SAFETY: STATE is an aligned u32 that lives for the whole process.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            STATE.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            observed,
            std::ptr::null::<libc::timespec>(),
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

    #[test]
    #[should_panic(expected = "would wait for itself")]
    fn a_launch_while_holding_a_transient_open_panics() {
        let _open = TransientOpen::begin();
        let _launch = Launch::begin();
    }
}
