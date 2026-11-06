/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use core::mem;

use perf_event_open_sys::bindings as perf;
use reverie::Errno;
use tracing::warn;

use crate::perf::PerfCounter;
use crate::perf::do_branches;
use crate::timer::get_pmu_config;
use crate::timer::has_precise_ip;

const IN_TXCP: u64 = 1 << 33;
const NUM_BRANCHES: u64 = 500;

pub use reverie::pmu_config::PmuValidationError;
fn init_perf_event_attr(
    perf_attr_type: u32,
    config: u64,
    precise_ip: bool,
) -> perf::perf_event_attr {
    let mut result = perf::perf_event_attr {
        type_: perf_attr_type,
        config,
        ..Default::default()
    };
    result.size = mem::size_of_val(&result) as u32;
    result.set_exclude_guest(1);
    result.set_exclude_kernel(1);

    if precise_ip && has_precise_ip() {
        result.set_precise_ip(1);

        // This prevents EINVAL when creating a counter with precise_ip enabled
        result.__bindgen_anon_1.sample_period = PerfCounter::DISABLE_SAMPLE_PERIOD;
    } else {
        // This is the value used for the bug checks which are not originally designed to
        // work with precise_ip
        result.__bindgen_anon_1.sample_period = 0;
    }

    result
}

/// Create a template perf_event_attr for ticks
fn ticks_attr(precise_ip: bool) -> perf::perf_event_attr {
    init_perf_event_attr(
        perf::PERF_TYPE_RAW,
        get_pmu_config().raw_rcb_event(),
        precise_ip,
    )
}

/// Create a template perf_event_attr for cycles
fn cycles_attr(precise_ip: bool) -> perf::perf_event_attr {
    init_perf_event_attr(
        perf::PERF_TYPE_HARDWARE,
        perf::PERF_COUNT_HW_CPU_CYCLES.into(),
        precise_ip,
    )
}

/// A counter's descriptor, a transient open from [`start_counter`] to its
/// close here. Each check builds its counters' attributes before it opens the
/// first one, so only system calls on the counters and the check's own
/// counted work run under the guard; the longest is the SpecLockMap check's
/// locked-instruction loop, about a millisecond.
struct ScopedFd(i32, crate::launch_window::TransientOpen);

impl Drop for ScopedFd {
    fn drop(&mut self) {
        if let Err(errno) = Errno::result(unsafe { libc::close(self.0) }) {
            warn_after_guards(format!("Error while closing file descriptor - {:?}", errno));
        }
    }
}

/// Logs `message` once this thread holds no transient open: a log subscriber
/// may block on a thread that is launching a guest, which waits for the open
/// to close.
fn warn_after_guards(message: String) {
    crate::launch_window::run_after_guards(move || warn!("{message}"));
}

/// This function is a transcription of the function `check_for_bugs` from
/// [Mozilla-RR](https://github.com/rr-debugger/rr/blob/master/src/PerfCounters.cc#L308)
/// It checks for a collection of processor features that ensure that the pmu features
/// required from Reverie to function correctly are available and trustworthy
pub(crate) fn check_for_pmu_bugs() -> Result<(), PmuValidationError> {
    require_pmu_config(crate::timer::try_get_pmu_config().is_some())?;
    check_for_ioc_period_bug(false)?;
    check_working_counters(false)?;
    check_for_ioc_period_bug(true)?;
    check_working_counters(true)?;
    // The architecture checks build their own counters and ignore precise_ip,
    // so one pass covers both settings.
    check_for_arch_bugs(false)
}

/// Refuses a CPU with no PMU configuration, which the checks below would
/// otherwise reach through [`get_pmu_config`] and panic on.
#[cfg(target_arch = "x86_64")]
fn require_pmu_config(configured: bool) -> Result<(), PmuValidationError> {
    if configured {
        return Ok(());
    }
    let features = raw_cpuid::CpuId::new()
        .get_feature_info()
        .ok_or(PmuValidationError::CouldNotReadCpuInfo)?;
    Err(PmuValidationError::UnsupportedCpu {
        family: features.family_id(),
        model: features.model_id(),
    })
}

/// Every aarch64 CPU gets a configuration.
#[cfg(target_arch = "aarch64")]
fn require_pmu_config(_configured: bool) -> Result<(), PmuValidationError> {
    Ok(())
}

/// This function is transcribed from the function with the same name in
/// [Mozilla-RR](https://github.com/rr-debugger/rr/blob/master/src/PerfCounters.cc#L227)
/// Checks for a bug in (supposedly) Linux Kernel < 3.7 where period changes
/// do not happen until after the _next_ rollover.
fn check_for_ioc_period_bug(precise_ip: bool) -> Result<(), PmuValidationError> {
    // Start a cycles counter
    let mut attr = ticks_attr(precise_ip);
    attr.__bindgen_anon_1.sample_period = 0xffffffff;
    attr.set_exclude_callchain_kernel(1);
    let bug_fd = start_counter(0, -1, &mut attr, None)?;

    let mut new_period = 1_u64;

    let _ioctl = ioctl(&bug_fd, perf::PERIOD.into(), &mut new_period)?;

    let mut poll_bug_fd = libc::pollfd {
        fd: bug_fd.0,
        events: libc::POLLIN,
        revents: 0,
    };

    let _poll = Errno::result(unsafe { libc::poll(&mut poll_bug_fd as *mut libc::pollfd, 1, 0) })
        .map_err(|errno| PmuValidationError::UnexpectedTestingErrnoError {
        errno,
        msg: "poll syscall failed  in ioc period bug check",
    })?;

    if poll_bug_fd.revents == 0 {
        Err(PmuValidationError::IocPeriodBugDetected)
    } else {
        Ok(())
    }
}

fn start_counter(
    tid: libc::pid_t,
    group_fd: libc::c_int,
    attr: &mut perf::perf_event_attr,
    mut disabled_txcp: Option<&mut bool>,
) -> Result<ScopedFd, PmuValidationError> {
    attr.set_pinned((group_fd == -1) as u64);

    if let Some(disabled) = disabled_txcp.as_mut() {
        **disabled = false
    }

    let open = crate::launch_window::TransientOpen::begin();
    let fd_result = Errno::result(unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            attr as *mut perf::perf_event_attr,
            tid,
            -1,
            group_fd,
            perf::PERF_FLAG_FD_CLOEXEC,
        )
    });

    match &fd_result {
        Err(Errno::EINVAL) if attr.config & IN_TXCP > 0 => {
            // The kernel might not support IN_TXCP, so try again without it.
            let mut tmp_attr = *attr;
            tmp_attr.config &= !IN_TXCP;

            let no_txcp_fd = Errno::result(unsafe {
                libc::syscall(
                    libc::SYS_perf_event_open,
                    &tmp_attr,
                    tid,
                    -1,
                    group_fd,
                    perf::PERF_FLAG_FD_CLOEXEC,
                )
            });

            if no_txcp_fd.is_ok() {
                if let Some(disabled) = disabled_txcp.as_mut() {
                    **disabled = true
                }
                warn_after_guards("kernel does not support IN_TXCP".to_owned());
            }

            no_txcp_fd
        }
        _ => fd_result,
    }
    .map(|raw_fd| ScopedFd(raw_fd as i32, open))
    .map_err(|errno| match errno {
        Errno::EACCES => PmuValidationError::CouldNotCreateTimer {
            errno,
            msg: "Permission denied to use 'perf_event_open'; are hardware perf events \
                available? See https://github.com/rr-debugger/rr/wiki/Will-rr-work-on-my-system",
        },
        Errno::ENOENT => PmuValidationError::CouldNotCreateTimer {
            errno,
            msg: "Unable to open performance counter with 'perf_event_open'; \
                are hardware perf events available? See \
                https://github.com/rr-debugger/rr/wiki/Will-rr-work-on-my-system",
        },
        _ => PmuValidationError::CouldNotCreateTimer {
            errno,
            msg: "See - https://man7.org/linux/man-pages/man3/errno.3.html",
        },
    })
}

fn ioctl(
    fd: &ScopedFd,
    request: libc::c_ulong,
    argument: &mut u64,
) -> Result<libc::c_int, PmuValidationError> {
    Errno::result(unsafe { libc::ioctl(fd.0, request, argument as *mut u64) }).map_err(|errno| {
        PmuValidationError::UnexpectedTestingErrnoError {
            errno,
            msg: "ioctl syscall failed",
        }
    })
}

/// One measured perf-counter reading, with the scheduling evidence
/// `PERF_FORMAT_TOTAL_TIME_ENABLED`/`PERF_FORMAT_TOTAL_TIME_RUNNING`
/// provide. A delta between two samples is a *measurement* only when
/// the counter ran for the whole window; a multiplexed or starved
/// counter that never ran reads its stale value (usually 0), which
/// is not a measurement of anything and must never be handed to a
/// verdict as a count.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Default)]
struct CounterSample {
    value: i64,
    time_enabled: u64,
    time_running: u64,
}

/// The value delta between two samples, or `None` when the counter
/// was not scheduled for the whole window between them (including
/// not at all): an unmeasured window yields no count, never a zero.
#[cfg(target_arch = "x86_64")]
fn measured_delta(before: CounterSample, after: CounterSample) -> Option<i64> {
    // Checked, not wrapping: a sample pair whose timestamps run
    // backwards (or would overflow the signed count) is not a
    // measurement either, and must yield no count rather than a
    // manufactured one. Fresh short-lived counters cannot ordinarily
    // wrap a u64, so this only refuses inconsistent samples.
    let enabled = after.time_enabled.checked_sub(before.time_enabled)?;
    let running = after.time_running.checked_sub(before.time_running);
    let running = running?;
    if enabled == 0 || running < enabled {
        return None;
    }
    Some(after.value.wrapping_sub(before.value))
}

/// Reads one counter whose attr requested the total-time formats:
/// the read returns value, time_enabled, and time_running, in that
/// order.
#[cfg(target_arch = "x86_64")]
fn read_counter_sample(fd: &ScopedFd) -> Result<CounterSample, PmuValidationError> {
    let mut words = [0_u64; 3];
    let size = mem::size_of_val(&words);
    let nread =
        Errno::result(unsafe { libc::read(fd.0, words.as_mut_ptr() as *mut libc::c_void, size) })
            .map_err(|errno| PmuValidationError::UnexpectedTestingErrnoError {
            errno,
            msg: "Failed to read from a counter",
        })?;
    if nread == 0 {
        // A pinned event the kernel could not schedule is put in an
        // error state whose read returns EOF. That is the extreme of
        // multiplexing: the counter never ran in this window at all,
        // so the sample is unmeasured, exactly like a
        // time_running < time_enabled delta - never a hard error and
        // never a zero count. Encode it as a never-ran sample so
        // measured_delta rejects every delta it participates in.
        return Ok(CounterSample {
            value: 0,
            time_enabled: 1,
            time_running: 0,
        });
    }
    if nread != size as isize {
        return Err(PmuValidationError::UnexpectedTestingError(format!(
            "Expected to read {} bytes from counter, but read {}",
            size, nread
        )));
    }
    Ok(CounterSample {
        value: words[0] as i64,
        time_enabled: words[1],
        time_running: words[2],
    })
}

/// read from the given file descriptor assuming it is a counter
fn read_counter(fd: &ScopedFd) -> Result<i64, PmuValidationError> {
    let mut val: i64 = 0;
    let val_size = mem::size_of_val(&val);
    let nread = Errno::result(unsafe {
        libc::read(fd.0, &mut val as *mut _ as *mut libc::c_void, val_size)
    })
    .map_err(|errno| PmuValidationError::UnexpectedTestingErrnoError {
        errno,
        msg: "Failed to read from a counter",
    })?;

    if nread != val_size as isize {
        Err(PmuValidationError::UnexpectedTestingError(format!(
            "Expected to read {} bytes from counter, but read {}",
            val_size, nread
        )))
    } else {
        Ok(val)
    }
}

/// Transcription of the function with the same name in mozilla-rr to check
/// for the bug where hardware counters simply don't work or only one hardware
/// counter works
fn check_working_counters(precise_ip: bool) -> Result<(), PmuValidationError> {
    let mut attr = ticks_attr(precise_ip);
    let mut attr2 = cycles_attr(precise_ip);

    let fd = start_counter(0, -1, &mut attr, None)?;
    let fd2 = start_counter(0, -1, &mut attr2, None)?;

    do_branches(NUM_BRANCHES);

    let events = read_counter(&fd)?;
    let events2 = read_counter(&fd2)?;

    if events < NUM_BRANCHES as i64 {
        Err(PmuValidationError::HardwareCountersNotWorking {
            actual_events: events,
            expected_min_events: NUM_BRANCHES,
            config: attr.config,
        })
    } else if events2 == 0 {
        Err(PmuValidationError::OnlyOnePerformanceCounter)
    } else {
        Ok(())
    }
}

/// check the cpu feature id to determine if it is a AMD-Zen vs AmdF15R30
#[cfg(target_arch = "x86_64")]
fn is_amd_zen(fi: raw_cpuid::FeatureInfo) -> bool {
    match fi.family_id() {
        0x17 => true, // Zen 1 / Zen 2
        0x19 => true, // Zen 3 / Zen 4
        0x1A => true, // Zen 5
        _ => false,
    }
}

/// This is a transcription of the function with the same name in Mozilla-RR it will
/// check for bugs specific to cpu architectures
#[cfg(target_arch = "x86_64")]
fn check_for_arch_bugs(_precise_ip: bool) -> Result<(), PmuValidationError> {
    let c = raw_cpuid::CpuId::new();
    let vendor = c
        .get_vendor_info()
        .ok_or(PmuValidationError::CouldNotReadCpuInfo)?;
    let feature_info = c
        .get_feature_info()
        .ok_or(PmuValidationError::CouldNotReadCpuInfo)?;
    let vendor_str = vendor.as_str();

    match vendor_str {
        "AuthenticAMD" if is_amd_zen(feature_info) => check_for_zen_speclockmap(),
        "GenuineIntel" => {
            check_for_kvm_in_txcp_bug()?;
            Ok(())
        }
        s => Err(PmuValidationError::UnknownCpuVendor(s.to_owned())),
    }
}

#[cfg(target_arch = "aarch64")]
fn check_for_arch_bugs(_precise_ip: bool) -> Result<(), PmuValidationError> {
    // TODO: Do some aarch64-specific testing?
    Ok(())
}

/// How many locked instructions [`check_for_zen_speclockmap`] executes. On an
/// AMD EPYC 9D64 (family 0x19, model 0xA0) with SpecLockMap enabled, a single
/// locked instruction left the SpecLockMap counter unchanged in 15 of 16 runs,
/// while 100,000 moved it by 91 to 2,374 in 16 of 16, and the retired-branch
/// counter over-counted by 2 to 4 branches around locked instructions. Where
/// SpecLockMap is disabled the counter stayed at 0 over 100,000. See
/// https://github.com/rrnewton/hermit/issues/3794. The loop takes about a
/// millisecond and runs once per process.
#[cfg(target_arch = "x86_64")]
const SPECLOCKMAP_LOCKED_INSTRUCTIONS: usize = 100_000;
#[cfg(target_arch = "x86_64")]
const _: () = assert!(
    SPECLOCKMAP_LOCKED_INSTRUCTIONS >= 100_000,
    "fewer locked instructions miss SpecLockMap on some Zen CPUs"
);

/// AMD PMCx025, retired locked instructions, with every unit mask. User
/// mode only, as rr's `0x5100xx` events are. With SpecLockMap disabled it
/// counted exactly 100,000 of 100,000 locked instructions on AMD EPYC 9D25
/// (family 0x1A, model 0x11; 186 runs) and 9D85 hosts, but only 3 to 466
/// on an EPYC 9D64 (family 0x19, model 0xA0) — the part where, with
/// SpecLockMap enabled, it counted 59 to 56,833 (51 runs). The event
/// therefore decides nothing by under-counting on any part; the check
/// requires only that it counted at all, and uses its commits as
/// refusal evidence (see [`speclockmap_verdict`]).
#[cfg(target_arch = "x86_64")]
use reverie::pmu_config::RETIRED_LOCK_INSTRUCTIONS_EVENT;

/// AMD PMCx025 with unit mask 0x08, SpecLockMapCommit: the locked
/// instructions that retired through SpecLockMap. rr's Zen check uses the
/// same event.
#[cfg(target_arch = "x86_64")]
const SPECLOCKMAP_COMMIT_EVENT: u64 = 0x510825;

/// What [`count_speclockmap_commits`] counted over its loops.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpecLockMapCounts {
    /// Retired locked instructions, [`RETIRED_LOCK_INSTRUCTIONS_EVENT`].
    locked_instructions: i64,
    /// SpecLockMap commits among them, [`SPECLOCKMAP_COMMIT_EVENT`].
    speclockmap_commits: i64,
    /// Retired conditional branches over a branches-only loop of
    /// [`SPECLOCKMAP_LOCKED_INSTRUCTIONS`] iterations.
    branches_rcb: i64,
    /// The same branches-only loop counted again: an exact counter
    /// reads the identical delta, which fixes the loop's overhead.
    branches_rcb_repeat: i64,
    /// Retired conditional branches over a branch-matched loop of the
    /// same iteration count that also executes one locked instruction
    /// per iteration.
    locks_rcb: i64,
}

#[cfg(target_arch = "x86_64")]
fn check_for_zen_speclockmap() -> Result<(), PmuValidationError> {
    speclockmap_verdict(count_speclockmap_commits()?)
}

/// How many times [`count_speclockmap_commits`] re-measures when a
/// counter was not scheduled for the whole window (multiplexed or
/// starved under load) before refusing as unmeasured. Bounded and
/// counted: the refusal names this many attempts.
#[cfg(target_arch = "x86_64")]
const SPECLOCKMAP_MEASURE_ATTEMPTS: usize = 5;

/// What one measurement attempt established. Positive refusal
/// evidence is terminal the moment it is measured: a later attempt
/// in which some other counter happens to be scheduled can never
/// erase a SpecLockMap commit or a branch overcount that a fully
/// scheduled window already counted.
#[cfg(target_arch = "x86_64")]
enum SpecLockMapAttempt {
    Measured(SpecLockMapCounts),
    Unmeasured,
    Refuse(PmuValidationError),
}

#[cfg(target_arch = "x86_64")]
fn count_speclockmap_commits() -> Result<SpecLockMapCounts, PmuValidationError> {
    measure_speclockmap_with_retries(measure_speclockmap_once)
}

/// The bounded re-measurement loop over [`measure_speclockmap_once`]:
/// unmeasured attempts retry, a `Refuse` is terminal the moment it is
/// measured, and a commits reading recorded by an attempt that
/// otherwise went unmeasured (`sticky_commits`) refuses even if a
/// later attempt measures clean - positive refusal evidence is
/// never discarded. Factored over the attempt function so the
/// exhaustion and late-success rows are unit-testable without perf.
#[cfg(target_arch = "x86_64")]
fn measure_speclockmap_with_retries(
    mut attempt: impl FnMut(
        &mut Vec<&'static str>,
        &mut Option<i64>,
    ) -> Result<SpecLockMapAttempt, PmuValidationError>,
) -> Result<SpecLockMapCounts, PmuValidationError> {
    let mut unmeasured: Vec<&'static str> = Vec::new();
    // A fully-scheduled commits reading above zero from an attempt
    // whose retired-lock window was unmeasured: the commits happened
    // even though that attempt cannot produce the error payload's
    // locked count, so no later clean attempt may accept over it.
    let mut sticky_commits: Option<i64> = None;
    for _ in 0..SPECLOCKMAP_MEASURE_ATTEMPTS {
        match attempt(&mut unmeasured, &mut sticky_commits)? {
            SpecLockMapAttempt::Measured(counts) => {
                if let Some(commits) = sticky_commits {
                    return Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled {
                        speclockmap_commits: commits,
                        locked_instructions: counts.locked_instructions,
                    });
                }
                return Ok(counts);
            }
            SpecLockMapAttempt::Unmeasured => {}
            SpecLockMapAttempt::Refuse(error) => return Err(error),
        }
    }
    if let Some(commits) = sticky_commits {
        // Every attempt was partly unmeasured, but one fully
        // scheduled commit window counted SpecLockMap commits: the
        // host is refused on that evidence, with the executed loop
        // size as the locked-instruction payload (each commit is by
        // construction one of the executed locks; the exact
        // retired-lock delta was never measured).
        return Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled {
            speclockmap_commits: commits,
            locked_instructions: SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64,
        });
    }
    Err(PmuValidationError::SpecLockMapCounterUnmeasured {
        counter: unmeasured.join(", "),
        attempts: SPECLOCKMAP_MEASURE_ATTEMPTS,
    })
}

/// One measurement attempt. `Unmeasured` means at least one counter
/// was not scheduled for the whole window (its reading is not a
/// count), and its name has been added to `unmeasured`; a positive
/// commits reading from such an attempt is recorded in
/// `sticky_commits` so no later attempt can accept over it.
#[cfg(target_arch = "x86_64")]
fn measure_speclockmap_once(
    unmeasured: &mut Vec<&'static str>,
    sticky_commits: &mut Option<i64>,
) -> Result<SpecLockMapAttempt, PmuValidationError> {
    // When the SpecLockMap optimization is not disabled, rr will not work
    // reliably (e.g. it would work fine on a single process with a single
    // thread, but not more). When the optimization is disabled, the
    // perf counter for retired lock instructions of type SpecLockMapCommit
    // (on PMC 0x25) stays at 0.
    // See more details at https://github.com/rr-debugger/rr/issues/2034.
    //
    // A locked instruction is not always committed through SpecLockMap, so
    // one locked instruction can leave the counter at 0 on a CPU where the
    // optimization is enabled. Execute many, and count them: a zero is
    // never believed on this counter alone (the direct branch test
    // decides acceptance), but a commit it does count happened. Every
    // counter here requests its enabled/running times, and a window in
    // which it was not scheduled throughout is unmeasured - it yields
    // no count at all, for any of the three counters, branch counter
    // included - and the attempt is retried.
    let read_format =
        (perf::PERF_FORMAT_TOTAL_TIME_ENABLED | perf::PERF_FORMAT_TOTAL_TIME_RUNNING) as u64;
    let mut locked_attr =
        init_perf_event_attr(perf::PERF_TYPE_RAW, RETIRED_LOCK_INSTRUCTIONS_EVENT, false);
    locked_attr.read_format = read_format;
    let mut commit_attr =
        init_perf_event_attr(perf::PERF_TYPE_RAW, SPECLOCKMAP_COMMIT_EVENT, false);
    commit_attr.read_format = read_format;

    let mut rcb_attr = ticks_attr(false);
    rcb_attr.read_format = read_format;
    let locked_fd = start_counter(0, -1, &mut locked_attr, None)?;
    let commit_fd = start_counter(0, -1, &mut commit_attr, None)?;
    let rcb_fd = start_counter(0, -1, &mut rcb_attr, None)?;

    // The direct branch-count test: the same conditional-branch loop
    // twice, then the same loop with one locked instruction per
    // iteration, each bracketed by identical counter reads so any
    // fixed overhead lands in every delta alike. An exact counter
    // reads the identical delta three times; SpecLockMap's harm is
    // precisely that locked instructions inflate the third reading.
    let iterations = SPECLOCKMAP_LOCKED_INSTRUCTIONS;
    let mut note_unmeasured = |name: &'static str| {
        if !unmeasured.contains(&name) {
            unmeasured.push(name);
        }
    };
    let branch_phase = |run: &dyn Fn()| -> Result<Option<i64>, PmuValidationError> {
        let before = read_counter_sample(&rcb_fd)?;
        run();
        Ok(measured_delta(before, read_counter_sample(&rcb_fd)?))
    };
    let Some(branches_rcb) = branch_phase(&|| do_branches(iterations as u64))? else {
        note_unmeasured("retired-branch");
        return Ok(SpecLockMapAttempt::Unmeasured);
    };
    let Some(branches_rcb_repeat) = branch_phase(&|| do_branches(iterations as u64))? else {
        note_unmeasured("retired-branch");
        return Ok(SpecLockMapAttempt::Unmeasured);
    };
    // The lock word is prepared before the locked phase is counted,
    // so that counted region contains only its loop body.
    let lock_word = core::sync::atomic::AtomicUsize::new(0);
    let Some(locks_rcb) =
        branch_phase(&|| execute_locked_instructions_branch_matched(iterations, &lock_word))?
    else {
        note_unmeasured("retired-branch");
        return Ok(SpecLockMapAttempt::Unmeasured);
    };
    core::hint::black_box(lock_word.load(core::sync::atomic::Ordering::Relaxed));
    // A measured branch overcount is complete refusal evidence on
    // its own (its payload needs only the branch readings): a later
    // counter in this same attempt going unmeasured must not discard
    // it into a retry that could accept.
    let executed = SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64;
    if branches_rcb == branches_rcb_repeat
        && (executed..=executed + SPECLOCKMAP_BRANCH_OVERHEAD_MAX).contains(&branches_rcb)
        && locks_rcb > branches_rcb
    {
        return Ok(SpecLockMapAttempt::Refuse(
            PmuValidationError::AmdSpecLockMapBranchOvercount {
                branches: branches_rcb,
                branches_with_locks: locks_rcb,
            },
        ));
    }

    let locked_before = read_counter_sample(&locked_fd)?;
    let commits_before = read_counter_sample(&commit_fd)?;
    execute_locked_instructions(SPECLOCKMAP_LOCKED_INSTRUCTIONS);
    let speclockmap_commits = measured_delta(commits_before, read_counter_sample(&commit_fd)?);
    let locked_instructions = measured_delta(locked_before, read_counter_sample(&locked_fd)?);
    if let Some(commits) = speclockmap_commits
        && commits > 0
    {
        // A fully scheduled commits window counted real SpecLockMap
        // commits: that evidence survives this attempt even if the
        // retired-lock window beside it went unmeasured.
        *sticky_commits = Some(commits);
        if let Some(locked_instructions) = locked_instructions {
            return Ok(SpecLockMapAttempt::Refuse(
                PmuValidationError::AmdSpecLockMapShouldBeDisabled {
                    speclockmap_commits: commits,
                    locked_instructions,
                },
            ));
        }
    }
    let (Some(speclockmap_commits), Some(locked_instructions)) =
        (speclockmap_commits, locked_instructions)
    else {
        if speclockmap_commits.is_none() {
            note_unmeasured("speclockmap-commit");
        }
        if locked_instructions.is_none() {
            note_unmeasured("retired-lock");
        }
        return Ok(SpecLockMapAttempt::Unmeasured);
    };
    Ok(SpecLockMapAttempt::Measured(SpecLockMapCounts {
        locked_instructions,
        speclockmap_commits,
        branches_rcb,
        branches_rcb_repeat,
        locks_rcb,
    }))
}

/// The largest loop overhead (counted branches beyond the iterations
/// executed: call/return and counter-read paths between the samples)
/// the direct branch test accepts. This is a wrong-event guard, not
/// a tolerance on the lock differential, which is exact: the
/// measured overhead is 9 through a C probe and 17 through this
/// file's bracket paths (RCB deltas 100,009 and 100,017 for 100,000
/// iterations on EPYC 9D64/9D85 hosts), while an event counting more
/// than conditional branches reads multiples of the iteration count.
/// 256 is over 15x the measured overhead and three orders of
/// magnitude below the 200,000+ inflation SpecLockMap enabled
/// produces at 2 to 4 extra branches per locked instruction.
#[cfg(target_arch = "x86_64")]
const SPECLOCKMAP_BRANCH_OVERHEAD_MAX: i64 = 256;

/// Accepts the host only on direct branch-counter evidence: a loop of
/// exactly [`SPECLOCKMAP_LOCKED_INSTRUCTIONS`] conditional branches
/// must read the identical retired-branch count twice in a row
/// (exactly the iterations plus a fixed measured overhead), the same
/// loop with one locked instruction per iteration must read that
/// identical count again, and no locked instruction may have
/// committed through SpecLockMap. The retired-lock and commit counts
/// over a separate locked loop are diagnostic, and conclusive only
/// as refusal evidence: PMCx025 does not count every locked
/// instruction on every Zen part (it counted 3 to 466 of 100,000 on
/// an EPYC 9D64 with SpecLockMap disabled), so its under-count can
/// never accept a host, but a commit it does count happened. A
/// fully-scheduled lock count of exactly zero is likewise only a
/// measurement of an event that decides nothing; an unscheduled
/// window is not a count at all and never reaches this verdict (the
/// measurement layer re-measures it and refuses as unmeasured). No
/// SpecLockMap-enabled host has yet measured this exact loop's
/// differential: the enabled-state refusal rests on the retained
/// commit counter (measured at 91 to 2,374 commits per 100,000 when
/// enabled) plus the documented 2-to-4 extra branches per locked
/// instruction, and is named here as unproven rather than implied.
#[cfg(target_arch = "x86_64")]
fn speclockmap_verdict(counts: SpecLockMapCounts) -> Result<(), PmuValidationError> {
    let SpecLockMapCounts {
        locked_instructions,
        speclockmap_commits,
        branches_rcb,
        branches_rcb_repeat,
        locks_rcb,
    } = counts;
    let executed = SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64;
    if speclockmap_commits > executed {
        return Err(PmuValidationError::UnexpectedTestingError(format!(
            "counted {speclockmap_commits} SpecLockMap commits over only \
             {executed} locked instructions"
        )));
    }
    if speclockmap_commits != 0 {
        return Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled {
            speclockmap_commits,
            locked_instructions,
        });
    }
    // A negative retired-lock delta is a counter reading nothing can
    // be concluded from. A fully-scheduled PMCx025 reading of exactly
    // zero is different: it is a real measurement of an event that
    // decides nothing by under-counting (see the module history: 3 to
    // 466 of 100,000 on this part), and acceptance rests on the direct
    // branch evidence below, per the design that replaced this proxy.
    // Unscheduled windows never reach this verdict at all: the
    // measurement layer treats them as unmeasured and re-measures,
    // so a zero here was counted, not starved into existence.
    if locked_instructions < 0 {
        return Err(PmuValidationError::SpecLockMapCheckInconclusive {
            counted: locked_instructions,
            executed: SPECLOCKMAP_LOCKED_INSTRUCTIONS,
        });
    }
    if branches_rcb != branches_rcb_repeat
        || branches_rcb < executed
        || branches_rcb - executed > SPECLOCKMAP_BRANCH_OVERHEAD_MAX
        || locks_rcb < branches_rcb
    {
        return Err(PmuValidationError::BranchCounterCheckInconclusive {
            branches: branches_rcb,
            branches_repeat: branches_rcb_repeat,
            branches_with_locks: locks_rcb,
        });
    }
    if locks_rcb > branches_rcb {
        return Err(PmuValidationError::AmdSpecLockMapBranchOvercount {
            branches: branches_rcb,
            branches_with_locks: locks_rcb,
        });
    }
    Ok(())
}

/// Executes `count` iterations of one locked instruction plus the one
/// conditional branch `do_branches` executes per iteration, so a
/// retired-branch counter reading over this loop differs from one over
/// `do_branches(count)` only by what the locked instructions add.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn execute_locked_instructions_branch_matched(
    count: usize,
    word: &core::sync::atomic::AtomicUsize,
) {
    let mut remaining = count as u64;
    // `lock xadd` is the instruction `fetch_add` compiles to (and the
    // pattern the commit phase measures), so the differential and the
    // commit counter witness the same locked pattern. The reload of
    // the addend register is not a conditional branch, so the loop
    // branches exactly as `do_branches` does.
    //
    // SAFETY: the loop only reads and writes `word` through the pointer
    // operand and decrements the register counter to zero.
    unsafe {
        core::arch::asm!(
            "2:",
            "mov {addend:e}, 1",
            "lock xadd qword ptr [{word}], {addend}",
            "sub {remaining}, 1",
            "jnz 2b",
            word = in(reg) word.as_ptr(),
            remaining = inout(reg) remaining,
            addend = out(reg) _,
        )
    }
    assert_eq!(remaining, 0);
}

/// Executes `count` locked instructions.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn execute_locked_instructions(count: usize) {
    let word = core::sync::atomic::AtomicUsize::new(0);
    for _ in 0..count {
        // `fetch_add` is a `lock xadd` on x86_64; `black_box` keeps the
        // result, so the compiler can neither drop nor merge the additions.
        core::hint::black_box(word.fetch_add(1, core::sync::atomic::Ordering::SeqCst));
    }
}

#[cfg(target_arch = "x86_64")]
fn check_for_kvm_in_txcp_bug() -> Result<(), PmuValidationError> {
    let mut count: i64 = 0;
    let mut attr = ticks_attr(false);
    attr.config |= IN_TXCP;
    attr.__bindgen_anon_1.sample_period = 0;
    let mut disabled_txcp = false;
    let fd = start_counter(0, -1, &mut attr, Some(&mut disabled_txcp))?;

    let mut arg = 0_u64;

    if !disabled_txcp {
        ioctl(&fd, perf::DISABLE.into(), &mut arg)?;
        ioctl(&fd, perf::ENABLE.into(), &mut arg)?;
        do_branches(NUM_BRANCHES);
        count = read_counter(&fd)?;
    }

    let supports_txcp = count > 0;
    if supports_txcp && count < NUM_BRANCHES as i64 {
        Err(PmuValidationError::IntelKvmInTxcpBugDetected)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::perf::is_perf_supported;

    /// The SpecLockMap check must execute its loops at full count; a single
    /// locked instruction misses the bug on CPUs where only some locked
    /// instructions commit through SpecLockMap
    /// (https://github.com/rrnewton/hermit/issues/3794).
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn speclockmap_check_measures_its_branch_and_lock_loops() {
        let c = raw_cpuid::CpuId::new();
        let amd_zen = c.get_vendor_info().map(|v| v.as_str() == "AuthenticAMD") == Some(true)
            && c.get_feature_info().is_some_and(is_amd_zen);
        if !is_perf_supported() || !amd_zen {
            return;
        }
        let counts = count_speclockmap_commits().unwrap();
        // Commits are counted over the same executed locks, so they can
        // never exceed the executed count (the retired-lock count may
        // legitimately read lower where PMCx025 under-counts).
        assert!(
            (0..=SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64).contains(&counts.speclockmap_commits),
            "{counts:?}"
        );
        assert!(
            counts.locked_instructions >= 0,
            "{counts:?}: the retired-lock pipeline counted backwards"
        );
        // The verdict over this test's own counts must carry those same
        // counts in its payload, whatever this host's setting is. (A
        // second fresh measurement could legitimately read different
        // counts, so it is not cross-checked here; the end-to-end check
        // is exercised by test_check_for_arch_bugs on this host.) The
        // exactness assertions below run only when the verdict accepts
        // this host: on a host the check exists to refuse, they would
        // panic on a firmware condition instead of reporting it.
        let verdict = speclockmap_verdict(counts);
        if verdict.is_ok() {
            let executed = SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64;
            assert_eq!(counts.speclockmap_commits, 0, "{counts:?}");
            assert_eq!(
                counts.branches_rcb, counts.branches_rcb_repeat,
                "{counts:?}: the branch counter did not repeat its own count"
            );
            assert!(
                (executed..=executed + SPECLOCKMAP_BRANCH_OVERHEAD_MAX)
                    .contains(&counts.branches_rcb),
                "{counts:?}: baseline outside the exact overhead bound"
            );
            assert_eq!(
                counts.locks_rcb, counts.branches_rcb,
                "{counts:?}: locked instructions changed the branch count"
            );
        }
        match verdict {
            Ok(()) => assert_eq!(
                counts.speclockmap_commits, 0,
                "{counts:?}: the verdict passed counts that saw SpecLockMap commits"
            ),
            Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled {
                speclockmap_commits,
                locked_instructions,
            }) => assert_eq!(
                (speclockmap_commits, locked_instructions),
                (counts.speclockmap_commits, counts.locked_instructions),
                "{counts:?}"
            ),
            Err(PmuValidationError::AmdSpecLockMapBranchOvercount {
                branches,
                branches_with_locks,
            }) => assert_eq!(
                (branches, branches_with_locks),
                (counts.branches_rcb, counts.locks_rcb),
                "{counts:?}"
            ),
            Err(PmuValidationError::SpecLockMapCheckInconclusive { counted, .. }) => {
                assert_eq!(counted, counts.locked_instructions, "{counts:?}")
            }
            Err(PmuValidationError::BranchCounterCheckInconclusive {
                branches,
                branches_repeat,
                branches_with_locks,
            }) => assert_eq!(
                (branches, branches_repeat, branches_with_locks),
                (
                    counts.branches_rcb,
                    counts.branches_rcb_repeat,
                    counts.locks_rcb
                ),
                "{counts:?}"
            ),
            Err(error) => panic!("SpecLockMap verdict failed - {error}"),
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn measured_delta_requires_the_counter_ran_the_whole_window() {
        let sample = |value: i64, time_enabled: u64, time_running: u64| CounterSample {
            value,
            time_enabled,
            time_running,
        };
        // Fully scheduled: the delta is a measurement, including a
        // genuine counted zero (multiplexed-to-zero is the row below,
        // not this one).
        assert_eq!(
            measured_delta(sample(7, 100, 100), sample(7, 200, 200)),
            Some(0)
        );
        assert_eq!(
            measured_delta(sample(7, 100, 100), sample(107, 200, 200)),
            Some(100)
        );
        // Multiplexed part of the window: unmeasured, never a count.
        assert_eq!(
            measured_delta(sample(7, 100, 100), sample(7, 200, 150)),
            None
        );
        // Never scheduled at all: unmeasured even though it reads 0.
        assert_eq!(measured_delta(sample(0, 100, 0), sample(0, 200, 0)), None);
        // An empty window shows nothing either way.
        assert_eq!(
            measured_delta(sample(7, 100, 100), sample(7, 100, 100)),
            None
        );
        // Backwards timestamps are not a window at all: unmeasured,
        // never a wrapped-around delta presented as a count.
        assert_eq!(
            measured_delta(sample(7, 200, 200), sample(9, 100, 200)),
            None
        );
        assert_eq!(
            measured_delta(sample(7, 100, 200), sample(9, 200, 100)),
            None
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn measured_read_retries_only_the_unmeasured() {
        let counts = SpecLockMapCounts {
            locked_instructions: 3,
            speclockmap_commits: 0,
            branches_rcb: 100_009,
            branches_rcb_repeat: 100_009,
            locks_rcb: 100_009,
        };
        // Late success: unmeasured attempts, then a measured one.
        let mut late = 0;
        let measured = measure_speclockmap_with_retries(|names, _sticky| {
            late += 1;
            if late < 3 {
                names.push("retired-branch");
                Ok(SpecLockMapAttempt::Unmeasured)
            } else {
                Ok(SpecLockMapAttempt::Measured(counts))
            }
        })
        .unwrap();
        assert_eq!(measured, counts);
        assert_eq!(late, 3);
        // Exhaustion: every attempt unmeasured -> the named refusal
        // carries the bounded attempt count and the counter names.
        let mut exhausted = 0;
        let error = measure_speclockmap_with_retries(|names, _sticky| {
            exhausted += 1;
            names.push("retired-lock");
            Ok(SpecLockMapAttempt::Unmeasured)
        })
        .unwrap_err();
        assert_eq!(exhausted, SPECLOCKMAP_MEASURE_ATTEMPTS);
        assert!(
            matches!(
                error,
                PmuValidationError::SpecLockMapCounterUnmeasured {
                    counter: _,
                    attempts: SPECLOCKMAP_MEASURE_ATTEMPTS
                }
            ),
            "{error:?}"
        );
        // A measured positive commit count is sticky: a later clean
        // attempt must not accept over it.
        let mut sticky_calls = 0;
        let error = measure_speclockmap_with_retries(|_names, sticky| {
            sticky_calls += 1;
            if sticky_calls == 1 {
                *sticky = Some(91);
                Ok(SpecLockMapAttempt::Unmeasured)
            } else {
                Ok(SpecLockMapAttempt::Measured(counts))
            }
        })
        .unwrap_err();
        assert!(matches!(
            error,
            PmuValidationError::AmdSpecLockMapShouldBeDisabled {
                speclockmap_commits: 91,
                locked_instructions: 3
            }
        ));
        // And if every attempt then stays unmeasured, the sticky
        // commits still refuse instead of degrading to unmeasured.
        let error = measure_speclockmap_with_retries(|names, sticky| {
            names.push("retired-lock");
            *sticky = Some(91);
            Ok(SpecLockMapAttempt::Unmeasured)
        })
        .unwrap_err();
        assert!(matches!(
            error,
            PmuValidationError::AmdSpecLockMapShouldBeDisabled {
                speclockmap_commits: 91,
                ..
            }
        ));
        // A terminal refusal from an attempt stops the loop at once.
        let mut refused_calls = 0;
        let error = measure_speclockmap_with_retries(|_names, _sticky| {
            refused_calls += 1;
            Ok(SpecLockMapAttempt::Refuse(
                PmuValidationError::AmdSpecLockMapBranchOvercount {
                    branches: 100_009,
                    branches_with_locks: 300_009,
                },
            ))
        })
        .unwrap_err();
        assert_eq!(refused_calls, 1);
        assert!(matches!(
            error,
            PmuValidationError::AmdSpecLockMapBranchOvercount { .. }
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn speclockmap_verdict_refuses_any_commit() {
        let n = SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64;
        let exact = SpecLockMapCounts {
            locked_instructions: n,
            speclockmap_commits: 0,
            branches_rcb: n + 9,
            branches_rcb_repeat: n + 9,
            locks_rcb: n + 9,
        };
        assert!(speclockmap_verdict(exact).is_ok());
        // Any commit count at all refuses, at any locked count: the
        // commit event is diagnostic, but a commit it counted happened.
        // Pairs are counts seen together on an AMD EPYC 9D64 with
        // SpecLockMap enabled, whose retired-lock counter under-counts.
        for (locked, commits) in [(n, 1), (n, 91), (n, n), (9, 8), (230, 89), (18886, 15570)] {
            match speclockmap_verdict(SpecLockMapCounts {
                locked_instructions: locked,
                speclockmap_commits: commits,
                ..exact
            }) {
                Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled {
                    speclockmap_commits,
                    locked_instructions,
                }) => assert_eq!(
                    (speclockmap_commits, locked_instructions),
                    (commits, locked)
                ),
                other => panic!("{locked} locked, {commits} commits: {other:?}"),
            }
        }
        // More commits than the check executed cannot come from its loop.
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                speclockmap_commits: n + 1,
                ..exact
            }),
            Err(PmuValidationError::UnexpectedTestingError(_))
        ));
        // A fully-scheduled retired-lock count of exactly zero is a
        // measurement of an event that decides nothing: with exact
        // branch evidence and zero commits it accepts (the direct
        // test decides, per the design that replaced the proxy), and
        // with inexact branches it refuses on the branch evidence.
        assert!(
            speclockmap_verdict(SpecLockMapCounts {
                locked_instructions: 0,
                ..exact
            })
            .is_ok()
        );
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                locked_instructions: 0,
                branches_rcb: n + 25_000,
                branches_rcb_repeat: n + 25_000,
                locks_rcb: n + 25_000,
                ..exact
            }),
            Err(PmuValidationError::BranchCounterCheckInconclusive { .. })
        ));
        // A negative delta is not a count at all: inconclusive.
        for dead in [-5] {
            assert!(
                matches!(
                    speclockmap_verdict(SpecLockMapCounts {
                        locked_instructions: dead,
                        ..exact
                    }),
                    Err(PmuValidationError::SpecLockMapCheckInconclusive {
                        counted,
                        ..
                    }) if counted == dead
                ),
                "locked={dead}"
            );
        }
        // The overhead ceiling is inclusive at the bound and refuses
        // one past it, with all three counts identical.
        assert!(
            speclockmap_verdict(SpecLockMapCounts {
                branches_rcb: n + SPECLOCKMAP_BRANCH_OVERHEAD_MAX,
                branches_rcb_repeat: n + SPECLOCKMAP_BRANCH_OVERHEAD_MAX,
                locks_rcb: n + SPECLOCKMAP_BRANCH_OVERHEAD_MAX,
                ..exact
            })
            .is_ok()
        );
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                branches_rcb: n + SPECLOCKMAP_BRANCH_OVERHEAD_MAX + 1,
                branches_rcb_repeat: n + SPECLOCKMAP_BRANCH_OVERHEAD_MAX + 1,
                locks_rcb: n + SPECLOCKMAP_BRANCH_OVERHEAD_MAX + 1,
                ..exact
            }),
            Err(PmuValidationError::BranchCounterCheckInconclusive { .. })
        ));
        // The retired-lock count itself neither accepts nor refuses:
        // the same exact branches pass at both measured extremes.
        for locked in [3, 466, n] {
            assert!(
                speclockmap_verdict(SpecLockMapCounts {
                    locked_instructions: locked,
                    ..exact
                })
                .is_ok(),
                "{locked} locked counted"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn speclockmap_direct_branch_count_decides() {
        // The deciding test is direct: a loop of exactly N conditional
        // branches must read N plus a fixed overhead twice in a row,
        // and the same loop with a locked instruction per iteration
        // must read the identical count (owner direction 2026-11-05;
        // measured overhead 9 with RCB delta 100,009 for 100,000 on
        // EPYC 9D64, 9D85 and 9D25-class hosts). The retired-lock
        // counter is diagnostic only in this classification: its
        // under-count on Zen4c neither accepts nor refuses.
        let n = SPECLOCKMAP_LOCKED_INSTRUCTIONS as i64;
        let exact = SpecLockMapCounts {
            locked_instructions: 18,
            speclockmap_commits: 0,
            branches_rcb: n + 9,
            branches_rcb_repeat: n + 9,
            locks_rcb: n + 9,
        };
        assert!(speclockmap_verdict(exact).is_ok(), "{exact:?}");
        // A counter that cannot repeat its own count is not exact.
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                branches_rcb_repeat: n + 10,
                ..exact
            }),
            Err(PmuValidationError::BranchCounterCheckInconclusive { .. })
        ));
        // Locks changing the count at all — by one — fails exactness.
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                locks_rcb: n + 10,
                ..exact
            }),
            Err(PmuValidationError::AmdSpecLockMapBranchOvercount { .. })
        ));
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                locks_rcb: n + 8,
                ..exact
            }),
            Err(PmuValidationError::BranchCounterCheckInconclusive { .. })
        ));
        // A baseline that did not count the loop proves nothing.
        assert!(matches!(
            speclockmap_verdict(SpecLockMapCounts {
                branches_rcb: n - 1,
                branches_rcb_repeat: n - 1,
                locks_rcb: n - 1,
                ..exact
            }),
            Err(PmuValidationError::BranchCounterCheckInconclusive { .. })
        ));
    }

    /// A CPU with no profile is refused with its family and model, where the
    /// checks would otherwise panic in `get_pmu_config`.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn unconfigured_cpu_is_refused_not_panicked_on() {
        assert!(require_pmu_config(true).is_ok());
        let features = raw_cpuid::CpuId::new().get_feature_info().unwrap();
        match require_pmu_config(false) {
            Err(PmuValidationError::UnsupportedCpu { family, model }) => {
                assert_eq!((family, model), (features.family_id(), features.model_id()))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_check_for_ioc_period_bug() {
        if !is_perf_supported() {
            return;
        }

        // This assumes the machine running the test will not have this bug
        if let Err(pmu_err) = check_for_ioc_period_bug(false) {
            panic!("Ioc period bug check failed - {}", pmu_err);
        }
    }

    #[test]
    fn test_check_working_counters() {
        if !is_perf_supported() {
            return;
        }

        // This assumes the machine running the test will have working counters
        if let Err(pmu_err) = check_working_counters(false) {
            panic!("Working counters check failed - {}", pmu_err);
        }
    }

    #[test]
    fn test_check_for_arch_bugs() {
        if !is_perf_supported() {
            return;
        }

        // This assumes the machine running the test will not have arch bugs.
        match check_for_arch_bugs(false) {
            Ok(()) => {}
            // Whether the AMD Zen SpecLockMap optimization is disabled is a host
            // BIOS/firmware setting, not a defect in this code. Self-hosted CI
            // runners may leave it enabled, so treat that specific condition as a
            // skip while still failing on any other (unexpected) validation error.
            #[cfg(target_arch = "x86_64")]
            Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled { .. })
            | Err(PmuValidationError::AmdSpecLockMapBranchOvercount { .. }) => {
                eprintln!(
                    "skipping arch-bug check: host has AMD Zen SpecLockMap enabled \
                     (a firmware setting, not a code bug); see \
                     https://github.com/rr-debugger/rr/wiki/Zen"
                );
            }
            // BranchCounterCheckInconclusive is deliberately not skipped:
            // at parity with the retired lock counter's inconclusive
            // refusal it replaced, a counter that cannot repeat its own
            // count on a CI host is news the suite should fail on.
            Err(pmu_err) => panic!("Architecture-specific bug check failed - {}", pmu_err),
        }
    }

    #[test]
    fn test_check_for_ioc_period_bug_precise_ip() {
        // This assumes the machine running the test will not have this bug and only runs
        // if precise_ip will be enabled
        if has_precise_ip()
            && let Err(pmu_err) = check_for_ioc_period_bug(true)
        {
            panic!(
                "Ioc period bug check failed when precise_ip was enabled - {}",
                pmu_err
            );
        }
    }

    #[test]
    fn test_check_working_counters_precise_ip() {
        // This assumes the machine running the test will have working counters and only runs
        // if precise_ip will be enabled
        if has_precise_ip()
            && let Err(pmu_err) = check_working_counters(true)
        {
            panic!(
                "Working counters check failed when precise_ip was enabled - {}",
                pmu_err
            );
        }
    }

    #[test]
    fn test_check_for_arch_bugs_precise_ip() {
        // This assumes the machine running the test will not have arch bugs and only runs
        // if precise_ip will be enabled
        if has_precise_ip() {
            match check_for_arch_bugs(true) {
                Ok(()) => {}
                // See test_check_for_arch_bugs: SpecLockMap is a host firmware
                // setting, so tolerate that specific condition here as well.
                #[cfg(target_arch = "x86_64")]
                Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled { .. })
                | Err(PmuValidationError::AmdSpecLockMapBranchOvercount { .. }) => {
                    eprintln!(
                        "skipping arch-bug check (precise_ip): host has AMD Zen \
                         SpecLockMap enabled (a firmware setting, not a code bug)"
                    );
                }
                Err(pmu_err) => panic!(
                    "Architecture-specific bug check failed when precise_ip was enabled - {}",
                    pmu_err
                ),
            }
        }
    }
}
