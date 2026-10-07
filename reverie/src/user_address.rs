/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The user address limit: the bound that Linux's `access_ok` places on a
//! range of user memory before a syscall reads or writes it.

use std::sync::Mutex;
use std::sync::PoisonError;

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

/// The running kernel's [`UserAddressLimit`], measured once per process; a
/// failed measurement is not remembered, and concurrent first callers share
/// one measurement.
///
/// On Linux both facts belong to the kernel, the same for every native
/// process (on x86-64 the limit is fixed when the kernel boots, by whether it
/// uses five-level paging), so measuring them in this process gives those of
/// a guest that runs as a native process of the same kernel.
pub(crate) fn host_user_address_limit() -> Result<UserAddressLimit, Error> {
    static LIMIT: Mutex<Option<UserAddressLimit>> = Mutex::new(None);
    measured_once(&LIMIT, || measure_user_address_limit(kernel_accepts_range))
}

/// The value in `cache`, measuring and storing it first if there is none.
///
/// The lock is held while measuring, so callers that arrive during a
/// measurement wait for it and use its value instead of measuring again. A
/// failed measurement leaves the cache empty for the next caller. The cache
/// holds a value only after a measurement completed, so a panic during one
/// leaves nothing partial behind and the poisoned lock is still usable.
fn measured_once<T: Copy>(
    cache: &Mutex<Option<T>>,
    measure: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    let mut cached = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(value) = *cached {
        return Ok(value);
    }
    let value = measure()?;
    *cached = Some(value);
    Ok(value)
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

    #[test]
    fn concurrent_first_callers_share_one_measurement_and_failures_are_not_cached() {
        use std::sync::Barrier;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;

        const THREADS: usize = 8;
        let cache = Mutex::new(None);
        let measurements = AtomicUsize::new(0);
        let failing = || {
            measurements.fetch_add(1, Ordering::SeqCst);
            Err(Error::Errno(Errno::EIO))
        };
        // A failed measurement leaves the cache empty.
        assert!(matches!(
            measured_once::<u64>(&cache, failing),
            Err(Error::Errno(Errno::EIO))
        ));
        assert_eq!(*cache.lock().unwrap(), None);

        let barrier = Barrier::new(THREADS);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        measured_once(&cache, || {
                            let call = measurements.fetch_add(1, Ordering::SeqCst);
                            // Slow enough that every other thread arrives
                            // while this measurement runs.
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            Ok(call as u64 + 1000)
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
        assert_eq!(results, vec![1001; THREADS]);
        assert_eq!(measured_once(&cache, failing).unwrap(), 1001);
        assert_eq!(measurements.load(Ordering::SeqCst), 2);
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
