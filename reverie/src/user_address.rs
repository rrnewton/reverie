/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The user address limit: the bound that Linux's `access_ok` places on a
//! range of user memory before a syscall reads or writes it.

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie_syscalls::Errno;

use crate::Error;

/// The user address limit of an x86-64 guest with four-level paging: Linux's
/// `TASK_SIZE_MAX`, one page below 2^47. See
/// [`Guest::user_address_limit`](crate::Guest::user_address_limit).
pub const X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT: u64 = (1 << 47) - 4096;

/// Linux's `MAX_RW_COUNT` with four-KiB pages, `INT_MAX & PAGE_MASK`: the
/// most bytes one read or write transfers.
const MAX_RW_COUNT: u64 = (i32::MAX as u64) & !4095;

/// How a guest's kernel checks the user ranges a syscall names, as reported
/// by [`Guest::user_address_limit`](crate::Guest::user_address_limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UserAddressLimit {
    /// The user address limit: `access_ok` accepts `[base, base + len)`
    /// exactly when `base + len` does not overflow and is at most `max_end`.
    /// An empty range at the limit is therefore valid, and any byte at or
    /// above it is not. On x86-64 Linux it is `TASK_SIZE_MAX`, which is
    /// [`X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT`] under four-level paging.
    pub max_end: u64,
    /// Whether a vectored request with exactly one vector has only that
    /// vector's first `MAX_RW_COUNT` bytes (`0x7ffff000`) checked. Linux 6.4
    /// and later import a lone vector with `import_ubuf`, which shortens it
    /// to `MAX_RW_COUNT` before `access_ok`. Earlier kernels check its whole
    /// length, as every kernel checks each vector of a request with several.
    pub caps_single_vector: bool,
}

/// The running kernel's [`UserAddressLimit`], cached for the process after
/// its first successful measurement.
///
/// On Linux both facts belong to the kernel, the same for every native
/// process (on x86-64 the limit is fixed when the kernel boots, by whether it
/// uses five-level paging), so measuring them in this process gives those of
/// a guest that runs as a native process of the same kernel.
///
/// A successful measurement makes at most 69 range checks. A failed one is
/// not remembered, so the next query measures again. A query that arrives
/// while another running thread of this process is measuring waits for that
/// measurement instead of making its own. A process created by `fork` or
/// `clone` while a measurement was under way in its parent does not inherit
/// the thread making it, so its first query measures for itself rather than
/// waiting for a measurement that will never finish there.
pub(crate) fn host_user_address_limit() -> Result<UserAddressLimit, Error> {
    static CACHE: AtomicU64 = AtomicU64::new(IDLE);
    cached_measurement(&CACHE, Thread::current, is_running_thread, || {
        measure_user_address_limit(kernel_accepts_range)
    })
}

/// A thread, as the cache of [`host_user_address_limit`] names the one
/// measuring: its process ID and thread ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Thread {
    pid: u32,
    tid: u32,
}

impl Thread {
    /// The calling thread. Both IDs come from the kernel on every call, so a
    /// child made by a raw `fork` or `clone`, which no library hook sees,
    /// still reads its own.
    fn current() -> Thread {
        // SAFETY: getpid and gettid take no arguments and cannot fail.
        let (pid, tid) = unsafe {
            (
                libc::syscall(libc::SYS_getpid),
                libc::syscall(libc::SYS_gettid),
            )
        };
        Thread {
            pid: pid as u32,
            tid: tid as u32,
        }
    }
}

/// Whether `tid` is a running thread of the process `pid`. `tgkill` with
/// signal 0 sends nothing; it succeeds exactly when the thread exists in that
/// thread group. Any failure counts as "not running", which costs at most a
/// second measurement, never a wait that cannot end.
fn is_running_thread(pid: u32, tid: u32) -> bool {
    // SAFETY: signal 0 only checks that the thread exists.
    unsafe { libc::syscall(libc::SYS_tgkill, pid as libc::pid_t, tid as libc::pid_t, 0) == 0 }
}

/// The cache word holds one of three states, each whole in a single word, so
/// a reader never sees part of a state, and a child process inherits exactly
/// the state its parent's word held when it was copied:
///
/// - [`IDLE`]: nothing is cached and no measurement is under way.
/// - Measuring: bits 1..0 are `10` and the bits above hold the measuring
///   thread's process ID (high [`ID_BITS`]) and thread ID (low [`ID_BITS`]).
/// - Measured: bit 0 is `1`, bit 1 is
///   [`caps_single_vector`](UserAddressLimit::caps_single_vector) and the
///   bits above hold [`max_end`](UserAddressLimit::max_end).
const IDLE: u64 = 0;

/// The width of each ID in a measuring word. Linux IDs are below 2^22
/// (`PID_MAX_LIMIT`), so every real thread fits.
const ID_BITS: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheState {
    Idle,
    Measuring(Thread),
    Measured(UserAddressLimit),
}

fn decode(word: u64) -> CacheState {
    if word & 1 == 1 {
        CacheState::Measured(UserAddressLimit {
            max_end: word >> 2,
            caps_single_vector: word & 0b10 != 0,
        })
    } else if word == IDLE {
        CacheState::Idle
    } else {
        let ids = word >> 2;
        CacheState::Measuring(Thread {
            pid: (ids >> ID_BITS) as u32,
            tid: (ids & ((1 << ID_BITS) - 1)) as u32,
        })
    }
}

/// The measured word for `limit`, if its `max_end` fits in 62 bits (every
/// real limit does: x86-64's is below 2^57).
fn measured_word(limit: UserAddressLimit) -> Option<u64> {
    (limit.max_end < 1 << 62)
        .then(|| limit.max_end << 2 | u64::from(limit.caps_single_vector) << 1 | 1)
}

/// The measuring word naming `thread`, if both its IDs fit in [`ID_BITS`].
fn measuring_word(thread: Thread) -> Option<u64> {
    (thread.pid < 1 << ID_BITS && thread.tid < 1 << ID_BITS)
        .then(|| (u64::from(thread.pid) << ID_BITS | u64::from(thread.tid)) << 2 | 0b10)
}

/// The value cached in `cache`, measuring it with `measure` and caching it
/// first if there is none.
///
/// `current` names the calling thread, and `is_running(pid, tid)` answers
/// whether `tid` is a running thread of the process `pid`. A caller that
/// finds a measurement under way by another running thread of its own
/// process waits for it. A measurement claimed by anyone else (a thread of
/// another process, which is what a `fork` or `clone` child sees when its
/// parent was measuring; a thread no longer running; or the caller itself,
/// interrupted by a signal handler that queries again) is taken over, so no
/// caller waits on a measurement that cannot finish. A failed or panicking
/// measurement releases its claim and caches nothing. A caller whose IDs, or
/// a value whose `max_end`, the word cannot hold measures without caching.
fn cached_measurement(
    cache: &AtomicU64,
    current: impl FnOnce() -> Thread,
    mut is_running: impl FnMut(u32, u32) -> bool,
    measure: impl FnOnce() -> Result<UserAddressLimit, Error>,
) -> Result<UserAddressLimit, Error> {
    if let CacheState::Measured(limit) = decode(cache.load(Ordering::Acquire)) {
        return Ok(limit);
    }
    let me = current();
    let Some(mine) = measuring_word(me) else {
        return measure();
    };
    loop {
        let word = cache.load(Ordering::Acquire);
        match decode(word) {
            CacheState::Measured(limit) => return Ok(limit),
            CacheState::Measuring(owner)
                if owner.pid == me.pid && owner.tid != me.tid && is_running(me.pid, owner.tid) =>
            {
                std::thread::yield_now();
            }
            CacheState::Idle | CacheState::Measuring(_) => {
                if cache
                    .compare_exchange(word, mine, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }
        }
    }
    let claim = Claim { cache, mine };
    let limit = measure()?;
    claim.publish(limit);
    Ok(limit)
}

/// A caller's claim on the cache while it measures. Dropping the claim, on a
/// failed or panicking measurement, returns the cache to [`IDLE`] unless
/// another caller has since taken the measurement over.
struct Claim<'a> {
    cache: &'a AtomicU64,
    mine: u64,
}

impl Claim<'_> {
    /// Caches `limit`, unless another caller has taken the measurement over
    /// (then its own result is cached) or the word cannot hold `limit`.
    fn publish(self, limit: UserAddressLimit) {
        let next = measured_word(limit).unwrap_or(IDLE);
        let _ = self
            .cache
            .compare_exchange(self.mine, next, Ordering::AcqRel, Ordering::Relaxed);
        std::mem::forget(self);
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let _ = self
            .cache
            .compare_exchange(self.mine, IDLE, Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// The [`UserAddressLimit`] that `accepts` implements, where `accepts(base,
/// len)` answers whether a syscall accepts the range `[base, base + len)`
/// named by a lone vector.
///
/// After [`search_user_address_limit`] finds the limit, one more check names
/// the lone vector `[limit - MAX_RW_COUNT, limit + 1)`: a kernel that
/// shortens it to `MAX_RW_COUNT` bytes sees a range ending exactly at the
/// limit and accepts it; a kernel that checks it whole sees one byte past
/// the limit and refuses it.
fn measure_user_address_limit(
    mut accepts: impl FnMut(u64, u64) -> Result<bool, Error>,
) -> Result<UserAddressLimit, Error> {
    let max_end = search_user_address_limit(&mut accepts)?;
    let caps_single_vector = match max_end.checked_sub(MAX_RW_COUNT) {
        Some(base) => accepts(base, MAX_RW_COUNT + 1)?,
        None => {
            return Err(Error::Tool(anyhow::anyhow!(
                "the user address limit {max_end:#x} is below MAX_RW_COUNT"
            )));
        }
    };
    Ok(UserAddressLimit {
        max_end,
        caps_single_vector,
    })
}

/// Whether the running kernel's `access_ok` accepts the range
/// `[base, base + len)`.
///
/// `process_vm_readv` imports its local vectors first, checking each with
/// `access_ok` as `readv` does. With no remote vectors it then returns 0
/// before it looks up the process, pins a page or copies a byte
/// (`mm/process_vm_access.c`), so the call asks only the range question. The
/// one local vector is a lone vector: when `len` exceeds Linux's
/// `MAX_RW_COUNT`, kernels since 6.4 check only its first `MAX_RW_COUNT`
/// bytes, which [`measure_user_address_limit`] relies on.
fn kernel_accepts_range(base: u64, len: u64) -> Result<bool, Error> {
    let local = libc::iovec {
        iov_base: std::ptr::without_provenance_mut(base as usize),
        iov_len: len as usize,
    };
    // SAFETY: with no remote vectors the kernel neither reads nor writes the
    // memory that `local` names.
    let result =
        unsafe { libc::process_vm_readv(libc::getpid(), &local, 1, std::ptr::null(), 0, 0) };
    range_check_result(result, Errno::last())
}

/// Classifies the result of [`kernel_accepts_range`]'s `process_vm_readv`:
/// 0 accepts the range and `EFAULT` refuses it. Anything else means the call
/// did not answer the range question, which is the tool's failure, never a
/// verdict about the range.
fn range_check_result(result: isize, error: Errno) -> Result<bool, Error> {
    match (result, error) {
        (0, _) => Ok(true),
        (-1, Errno::EFAULT) => Ok(false),
        _ => Err(Error::Tool(anyhow::anyhow!(
            "process_vm_readv user range check failed: result={result}, errno={error}"
        ))),
    }
}

/// The user address limit that `accepts` (`accepts(base, len)` answers
/// whether a syscall accepts `[base, base + len)`) implements: the largest
/// address at which an empty range is accepted.
///
/// Fails unless `accepts` behaves as a limit on the range's end does at both
/// ends of the address space and at the limit itself.
fn search_user_address_limit(
    mut accepts: impl FnMut(u64, u64) -> Result<bool, Error>,
) -> Result<u64, Error> {
    let not_a_limit = |what: &str| {
        Error::Tool(anyhow::anyhow!(
            "the kernel's user range check is not a limit on the range's end: {what}"
        ))
    };
    // An empty range at address 0 is valid, and one at the top of the address
    // space, in the kernel's half, is not.
    let (mut accepted, mut refused) = (0_u64, u64::MAX);
    if !accepts(accepted, 0)? {
        return Err(not_a_limit("an empty range at 0 is refused"));
    }
    if accepts(refused, 0)? {
        return Err(not_a_limit(
            "an empty range at the last address is accepted",
        ));
    }
    while refused - accepted > 1 {
        let middle = accepted + (refused - accepted) / 2;
        if accepts(middle, 0)? {
            accepted = middle;
        } else {
            refused = middle;
        }
    }
    let limit = accepted;
    // The bound is on where the range ends: the byte just below the limit is
    // valid and the byte at it is not.
    if limit == 0 || !accepts(limit - 1, 1)? {
        return Err(not_a_limit("the byte below the limit is refused"));
    }
    if accepts(limit, 1)? {
        return Err(not_a_limit("the byte at the limit is accepted"));
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_level_user_limit_is_one_page_below_2_pow_47() {
        assert_eq!(X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT, 0x7fff_ffff_f000);
    }

    #[test]
    fn search_finds_a_limit_on_the_range_end_wherever_it_lies() {
        for limit in [
            1,
            4096,
            X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT,
            (1 << 56) - 4096,
            (1 << 63) - 1,
            u64::MAX - 1,
        ] {
            let mut calls = 0;
            let found = search_user_address_limit(|base, len| {
                calls += 1;
                Ok(base.checked_add(len).is_some_and(|end| end <= limit))
            })
            .unwrap();
            assert_eq!(found, limit);
            // Two end checks, at most 64 halvings and two boundary checks.
            assert!(calls <= 68, "{calls} checks for {limit:#x}");
        }
    }

    #[test]
    fn search_refuses_a_check_that_is_not_a_limit_on_the_range_end() {
        type Check = fn(u64, u64) -> bool;
        const LIMIT: u64 = X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT;
        let checks: [(&str, Check); 4] = [
            ("refuses everything", |_, _| false),
            ("accepts everything", |_, _| true),
            // Bounds where a range starts, not where it ends.
            ("bounds the start", |base, _| base <= LIMIT),
            // Refuses every nonempty range.
            ("refuses every byte", |base, len| len == 0 && base <= LIMIT),
        ];
        for (name, check) in checks {
            let result = search_user_address_limit(|base, len| Ok(check(base, len)));
            assert!(matches!(result, Err(Error::Tool(_))), "{name}: {result:?}");
        }
    }

    #[test]
    fn search_stops_at_the_first_failed_check_and_returns_its_error() {
        let mut calls = 0;
        let result = search_user_address_limit(|_, _| {
            calls += 1;
            if calls == 3 {
                Err(Error::Errno(Errno::EPERM))
            } else {
                Ok(calls == 1)
            }
        });
        assert!(
            matches!(result, Err(Error::Errno(Errno::EPERM))),
            "{result:?}"
        );
        assert_eq!(calls, 3);
    }

    #[test]
    fn range_check_failures_are_tool_errors_not_verdicts() {
        assert!(range_check_result(0, Errno::EPERM).unwrap());
        assert!(!range_check_result(-1, Errno::EFAULT).unwrap());
        for error in [
            Errno::ENOSYS,
            Errno::EPERM,
            Errno::EINVAL,
            Errno::ENOMEM,
            Errno::ESRCH,
            Errno::EIO,
        ] {
            assert!(matches!(range_check_result(-1, error), Err(Error::Tool(_))));
        }
        assert!(matches!(
            range_check_result(1, Errno::EFAULT),
            Err(Error::Tool(_))
        ));
    }

    #[test]
    fn measurement_reports_whether_a_lone_vector_is_shortened_before_its_check() {
        const LIMIT: u64 = X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT;
        for caps in [false, true] {
            let mut calls = Vec::new();
            let measured = measure_user_address_limit(|base, len| {
                calls.push((base, len));
                let checked = if caps { len.min(MAX_RW_COUNT) } else { len };
                Ok(base.checked_add(checked).is_some_and(|end| end <= LIMIT))
            })
            .unwrap();
            assert_eq!(
                measured,
                UserAddressLimit {
                    max_end: LIMIT,
                    caps_single_vector: caps,
                }
            );
            // The last check is the oversized lone vector ending one byte
            // past the limit.
            assert_eq!(
                calls.last(),
                Some(&(LIMIT - MAX_RW_COUNT, MAX_RW_COUNT + 1)),
                "caps {caps}"
            );
        }
    }

    #[test]
    fn measurement_fails_on_a_failed_check_or_a_limit_below_max_rw_count() {
        let mut calls = 0;
        let result = measure_user_address_limit(|base, len| {
            calls += 1;
            if len > MAX_RW_COUNT {
                return Err(Error::Errno(Errno::EPERM));
            }
            Ok(base
                .checked_add(len)
                .is_some_and(|end| end <= X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT))
        });
        assert!(
            matches!(result, Err(Error::Errno(Errno::EPERM))),
            "{result:?}"
        );
        assert!(calls > 1);
        let result = measure_user_address_limit(|base, len| {
            Ok(base.checked_add(len).is_some_and(|end| end <= 4096))
        });
        assert!(matches!(result, Err(Error::Tool(_))), "{result:?}");
    }

    fn limit(max_end: u64) -> UserAddressLimit {
        UserAddressLimit {
            max_end,
            caps_single_vector: max_end.is_multiple_of(2),
        }
    }

    #[test]
    fn each_cache_state_is_held_whole_in_one_word() {
        assert_eq!(decode(IDLE), CacheState::Idle);
        for max_end in [1, 4096, X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT, (1 << 62) - 1] {
            for caps_single_vector in [false, true] {
                let value = UserAddressLimit {
                    max_end,
                    caps_single_vector,
                };
                assert_eq!(
                    decode(measured_word(value).unwrap()),
                    CacheState::Measured(value)
                );
            }
        }
        assert_eq!(measured_word(limit(1 << 62)), None);
        let max_id = (1 << ID_BITS) - 1;
        for (pid, tid) in [
            (1, 1),
            (4_194_303, 4_194_303),
            (max_id, max_id),
            (7, max_id),
        ] {
            let thread = Thread { pid, tid };
            assert_eq!(
                decode(measuring_word(thread).unwrap()),
                CacheState::Measuring(thread)
            );
        }
        assert_eq!(
            measuring_word(Thread {
                pid: 1 << ID_BITS,
                tid: 1
            }),
            None
        );
        assert_eq!(
            measuring_word(Thread {
                pid: 1,
                tid: 1 << ID_BITS
            }),
            None
        );
    }

    #[test]
    fn concurrent_first_callers_share_one_measurement_and_failures_are_not_cached() {
        use std::sync::Barrier;
        use std::sync::atomic::AtomicUsize;

        const THREADS: usize = 8;
        let cache = AtomicU64::new(IDLE);
        let measurements = AtomicUsize::new(0);
        let failing = || {
            measurements.fetch_add(1, Ordering::SeqCst);
            Err(Error::Errno(Errno::EIO))
        };
        // A failed measurement leaves the cache empty.
        assert!(matches!(
            cached_measurement(&cache, Thread::current, is_running_thread, failing),
            Err(Error::Errno(Errno::EIO))
        ));
        assert_eq!(cache.load(Ordering::SeqCst), IDLE);

        let barrier = Barrier::new(THREADS);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        cached_measurement(&cache, Thread::current, is_running_thread, || {
                            let call = measurements.fetch_add(1, Ordering::SeqCst);
                            // Slow enough that every other thread arrives
                            // while this measurement runs.
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            Ok(limit(call as u64 + 1000))
                        })
                        .unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        // One failed and exactly one successful measurement ran, and every
        // caller got the successful one's value.
        assert_eq!(measurements.load(Ordering::SeqCst), 2);
        assert_eq!(results, vec![limit(1001); THREADS]);
        assert_eq!(
            cached_measurement(&cache, Thread::current, is_running_thread, failing).unwrap(),
            limit(1001)
        );
        assert_eq!(measurements.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_measurement_no_running_thread_of_this_process_is_making_is_taken_over() {
        let me = Thread { pid: 100, tid: 101 };
        // A thread of the parent process, as a fork or clone child sees it;
        // a thread of this process that is no longer running; and this
        // thread itself, as a signal handler that queries again sees it.
        for (owner, running) in [
            (Thread { pid: 99, tid: 99 }, true),
            (Thread { pid: 99, tid: 101 }, true),
            (Thread { pid: 100, tid: 102 }, false),
            (me, true),
        ] {
            let cache = AtomicU64::new(measuring_word(owner).unwrap());
            let mut measurements = 0;
            let result = cached_measurement(
                &cache,
                || me,
                |pid, tid| {
                    assert_eq!((pid, tid), (100, 102), "{owner:?}");
                    running
                },
                || {
                    measurements += 1;
                    Ok(limit(4096))
                },
            );
            assert_eq!(result.unwrap(), limit(4096), "{owner:?}");
            assert_eq!(measurements, 1, "{owner:?}");
            assert_eq!(
                decode(cache.load(Ordering::SeqCst)),
                CacheState::Measured(limit(4096)),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn a_caller_waits_for_a_running_thread_of_its_process() {
        let me = Thread { pid: 100, tid: 101 };
        let owner = Thread { pid: 100, tid: 102 };
        // The owner succeeds: the waiter takes its value and does not
        // measure.
        let cache = AtomicU64::new(measuring_word(owner).unwrap());
        let mut checks = 0;
        let result = cached_measurement(
            &cache,
            || me,
            |pid, tid| {
                assert_eq!((pid, tid), (100, 102));
                checks += 1;
                if checks == 3 {
                    cache.store(measured_word(limit(4096)).unwrap(), Ordering::SeqCst);
                }
                true
            },
            || panic!("a waiter measured while the owner was running"),
        );
        assert_eq!(result.unwrap(), limit(4096));
        assert_eq!(checks, 3);

        // The owner fails: the waiter then measures itself.
        let cache = AtomicU64::new(measuring_word(owner).unwrap());
        let mut checks = 0;
        let result = cached_measurement(
            &cache,
            || me,
            |_, _| {
                checks += 1;
                if checks == 2 {
                    cache.store(IDLE, Ordering::SeqCst);
                }
                true
            },
            || Ok(limit(8192)),
        );
        assert_eq!(result.unwrap(), limit(8192));
        assert_eq!(checks, 2);
        assert_eq!(
            decode(cache.load(Ordering::SeqCst)),
            CacheState::Measured(limit(8192))
        );
    }

    #[test]
    fn a_failed_or_panicking_measurement_releases_its_claim() {
        let me = Thread { pid: 100, tid: 101 };
        let cache = AtomicU64::new(IDLE);
        let result = cached_measurement(
            &cache,
            || me,
            |_, _| true,
            || {
                assert_eq!(cache.load(Ordering::SeqCst), measuring_word(me).unwrap());
                Err(Error::Errno(Errno::EIO))
            },
        );
        assert!(
            matches!(result, Err(Error::Errno(Errno::EIO))),
            "{result:?}"
        );
        assert_eq!(cache.load(Ordering::SeqCst), IDLE);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cached_measurement(
                &cache,
                || me,
                |_, _| true,
                || panic!("measurement panicked"),
            )
        }));
        assert!(panicked.is_err());
        assert_eq!(cache.load(Ordering::SeqCst), IDLE);
    }

    #[test]
    fn a_value_or_thread_the_cache_cannot_hold_is_measured_without_it() {
        let me = Thread { pid: 100, tid: 101 };
        let cache = AtomicU64::new(IDLE);
        let mut measurements = 0;
        for _ in 0..2 {
            let result = cached_measurement(
                &cache,
                || me,
                |_, _| true,
                || {
                    measurements += 1;
                    Ok(limit(1 << 62))
                },
            );
            assert_eq!(result.unwrap(), limit(1 << 62));
            assert_eq!(cache.load(Ordering::SeqCst), IDLE);
        }
        assert_eq!(measurements, 2);

        // A caller that cannot be named measures without claiming the cache,
        // even while another thread holds it.
        let owner = measuring_word(Thread { pid: 100, tid: 102 }).unwrap();
        let cache = AtomicU64::new(owner);
        let unnamed = Thread {
            pid: 1 << ID_BITS,
            tid: 101,
        };
        let result = cached_measurement(
            &cache,
            || unnamed,
            |_, _| panic!("no liveness check is needed"),
            || Ok(limit(4096)),
        );
        assert_eq!(result.unwrap(), limit(4096));
        assert_eq!(cache.load(Ordering::SeqCst), owner);
    }

    #[test]
    fn a_child_forked_during_a_measurement_measures_for_itself() {
        use std::sync::mpsc;
        use std::time::Duration;
        use std::time::Instant;

        static CACHE: AtomicU64 = AtomicU64::new(IDLE);
        const PARENT: UserAddressLimit = UserAddressLimit {
            max_end: 0x1000_0000,
            caps_single_vector: true,
        };
        const CHILD: UserAddressLimit = UserAddressLimit {
            max_end: 0x2000_0000,
            caps_single_vector: false,
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let measurer = std::thread::spawn(move || {
            cached_measurement(&CACHE, Thread::current, is_running_thread, || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(PARENT)
            })
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            decode(CACHE.load(Ordering::SeqCst)),
            CacheState::Measuring(_)
        ));

        // SAFETY: the child runs only atomics, raw syscalls and _exit, all of
        // which are safe after forking a multithreaded process.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            let result =
                cached_measurement(&CACHE, Thread::current, is_running_thread, || Ok(CHILD));
            let cached = CACHE.load(Ordering::SeqCst);
            let ok = matches!(result, Ok(value) if value == CHILD)
                && Some(cached) == measured_word(CHILD);
            // SAFETY: _exit ends the child without running the parent's
            // destructors or atexit handlers.
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }

        // The child must finish while the parent's measurement is still under
        // way; with the cache it inherited held by a thread it does not have,
        // waiting would never end.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut status = 0;
        let finished = loop {
            // SAFETY: waitpid writes only `status`.
            let waited = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
            if waited == child {
                break true;
            }
            assert_eq!(waited, 0, "waitpid failed");
            if Instant::now() > deadline {
                // SAFETY: the child is ours and not yet reaped.
                unsafe {
                    libc::kill(child, libc::SIGKILL);
                    libc::waitpid(child, &mut status, 0);
                }
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        release_tx.send(()).unwrap();
        assert_eq!(measurer.join().unwrap().unwrap(), PARENT);
        assert_eq!(Some(CACHE.load(Ordering::SeqCst)), measured_word(PARENT));
        assert!(
            finished,
            "the forked child waited on its parent's measurement"
        );
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "the forked child's query failed: status {status:#x}"
        );
    }

    #[test]
    fn host_limit_is_the_running_kernels_range_check_boundary() {
        let measured = host_user_address_limit().unwrap();
        assert_eq!(host_user_address_limit().unwrap(), measured);
        let limit = measured.max_end;
        #[cfg(target_arch = "x86_64")]
        assert!(limit >= X86_64_FOUR_LEVEL_USER_ADDRESS_LIMIT, "{limit:#x}");
        // Linux 6.4 and later shorten a lone vector before checking it.
        // SAFETY: utsname is plain data, and uname fills it.
        let mut name: libc::utsname = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::uname(&mut name) }, 0);
        // SAFETY: uname stores a NUL-terminated release string.
        let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
        let release = release.to_string_lossy();
        let mut version = release
            .split(|c: char| !c.is_ascii_digit())
            .map(|part| part.parse::<u32>().unwrap_or(0));
        let version = (version.next().unwrap_or(0), version.next().unwrap_or(0));
        if version >= (6, 4) {
            assert!(measured.caps_single_vector, "{release}");
        }
        assert_eq!(
            kernel_accepts_range(limit - MAX_RW_COUNT, MAX_RW_COUNT + 1).unwrap(),
            measured.caps_single_vector
        );
        for (base, len, accepted) in [
            (0, 0, true),
            (limit, 0, true),
            (limit + 1, 0, false),
            (limit - 1, 1, true),
            (limit, 1, false),
            (limit - 4096, 4096, true),
            (limit - 4095, 4096, false),
            // The end wraps past the top of the address space.
            (u64::MAX - 1, 2, false),
        ] {
            assert_eq!(
                kernel_accepts_range(base, len).unwrap(),
                accepted,
                "[{base:#x}, +{len:#x}) with limit {limit:#x}"
            );
        }
    }
}
