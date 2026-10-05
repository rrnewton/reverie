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
use thiserror::Error;
use tracing::warn;

use crate::perf::PerfCounter;
use crate::perf::do_branches;
use crate::timer::get_pmu_config;
use crate::timer::has_precise_ip;

const IN_TXCP: u64 = 1 << 33;
const NUM_BRANCHES: u64 = 500;

/// Why the one-time PMU validation (see [`crate::pmu_validation`]) failed.
///
/// Each variant means Reverie could not confirm that the RCB counter its
/// precise timers are built on counts every retired conditional branch on this
/// host, either because a check found a defect or because a check could not
/// run. An event scheduled by RCB count may then fire at a different point
/// from run to run.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PmuValidationError {
    /// A test counter could not be opened, so the checks could not run.
    #[error("Failed to create timer: {errno:?} - {msg}")]
    CouldNotCreateTimer {
        /// The error `perf_event_open` returned.
        errno: Errno,
        /// What the errno means here.
        msg: &'static str,
    },

    /// A system call the checks depend on failed, so they could not finish.
    #[error("Unexpected error while checking for pmu bugs: {errno:?} - {msg}")]
    UnexpectedTestingErrnoError {
        /// The error the system call returned.
        errno: Errno,
        /// Which step failed.
        msg: &'static str,
    },

    /// The checks could not finish for a reason other than a failed system
    /// call, such as a counter read returning too few bytes.
    #[error("Unexpected error while checking for pmu bugs: {0}")]
    UnexpectedTestingError(String),

    /// The kernel did not apply a new sample period set through
    /// `PERF_EVENT_IOC_PERIOD` until the next overflow.
    #[error("The ioc-period bug was detected")]
    IocPeriodBugDetected,

    /// The processor vendor or model could not be read, so the
    /// architecture-specific checks could not run.
    #[cfg(target_arch = "x86_64")]
    #[error("Could not read cpu info")]
    CouldNotReadCpuInfo,

    /// The branch counter reported fewer branches than the check executed.
    #[error(
        "Got {actual_events} branch events, expected at least {expected_min_events}. \
        The hardware performance counter seems to not be working. Check \
        that hardware performance counters are working by running \
        \n`perf stat -e r{config:x} true`\n\
        and checking that it reports a nonzero number of events. \
        If performance counters seem to be working with 'perf', file a \
        reverie issue, otherwise check your hardware/OS/VM configuration. Also \
        check that other software is not using performance counters on \
        this CPU."
    )]
    HardwareCountersNotWorking {
        /// The branches the counter reported.
        actual_events: i64,
        /// The branches the check executed.
        expected_min_events: u64,
        /// The raw event the counter was programmed with.
        config: u64,
    },

    /// The second test counter, opened alongside the first, counted nothing.
    #[error("Your CPU only supports one performance counter in its current configuration")]
    OnlyOnePerformanceCounter,

    /// The AMD Zen SpecLockMap optimization is enabled, so a speculatively
    /// executed locked instruction can leave the retired-branch count off by
    /// a few branches. See <https://github.com/rr-debugger/rr/issues/2034>.
    #[cfg(target_arch = "x86_64")]
    #[error(
        "On AMD Zen CPUs, reverie timers will not work reliably unless you disable the \
        hardware SpecLockMap optimization. For instructions on how to \
        do this, see https://github.com/rr-debugger/rr/wiki/Zen"
    )]
    AmdSpecLockMapShouldBeDisabled,

    /// The counter accepts the `IN_TXCP` flag but undercounts with it, a
    /// defect of some KVM guests on Intel processors.
    #[cfg(target_arch = "x86_64")]
    #[error("Intel Kvm-In-Txcp bug found")]
    IntelKvmInTxcpBugDetected,
}

impl PmuValidationError {
    /// The variant's name, which is stable and carries no host-specific values.
    /// Suitable for a machine-readable marker or report field; use `Display`
    /// for the human-readable explanation.
    pub fn name(&self) -> &'static str {
        match self {
            Self::CouldNotCreateTimer { .. } => "CouldNotCreateTimer",
            Self::UnexpectedTestingErrnoError { .. } => "UnexpectedTestingErrnoError",
            Self::UnexpectedTestingError(_) => "UnexpectedTestingError",
            Self::IocPeriodBugDetected => "IocPeriodBugDetected",
            #[cfg(target_arch = "x86_64")]
            Self::CouldNotReadCpuInfo => "CouldNotReadCpuInfo",
            Self::HardwareCountersNotWorking { .. } => "HardwareCountersNotWorking",
            Self::OnlyOnePerformanceCounter => "OnlyOnePerformanceCounter",
            #[cfg(target_arch = "x86_64")]
            Self::AmdSpecLockMapShouldBeDisabled => "AmdSpecLockMapShouldBeDisabled",
            #[cfg(target_arch = "x86_64")]
            Self::IntelKvmInTxcpBugDetected => "IntelKvmInTxcpBugDetected",
        }
    }
}

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
/// first one, so nothing but system calls on the counters runs under the
/// guard.
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
    check_for_ioc_period_bug(false)?;
    check_working_counters(false)?;
    check_for_arch_bugs(false)?;
    check_for_ioc_period_bug(true)?;
    check_working_counters(true)?;
    check_for_arch_bugs(true)
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
        s => panic!("Unknown CPU vendor: {}", s),
    }
}

#[cfg(target_arch = "aarch64")]
fn check_for_arch_bugs(_precise_ip: bool) -> Result<(), PmuValidationError> {
    // TODO: Do some aarch64-specific testing?
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn check_for_zen_speclockmap() -> Result<(), PmuValidationError> {
    // When the SpecLockMap optimization is not disabled, rr will not work
    // reliably (e.g. it would work fine on a single process with a single
    // thread, but not more). When the optimization is disabled, the
    // perf counter for retired lock instructions of type SpecLockMapCommit
    // (on PMC 0x25) stays at 0.
    // See more details at https://github.com/rr-debugger/rr/issues/2034.

    // 0x25 == RETIRED_LOCK_INSTRUCTIONS - Counts the number of retired locked instructions
    // + 0x08 == SPECLOCKMAPCOMMIT
    let mut attr = init_perf_event_attr(perf::PERF_TYPE_RAW, 0x510825, false);

    let fd = start_counter(0, -1, &mut attr, None)?;

    let val = 20_usize;
    let to_add = 22_usize;
    let count = read_counter(&fd)?;

    // A lock add is known to increase the perf counter we're looking at.
    unsafe {
        let mut _prev: *mut usize;
        core::arch::asm!(
            "lock",
            "xadd [{}], {}",
            inout(reg) &to_add => _prev,
            in(reg) val,
        )
    }

    if read_counter(&fd)? != count {
        Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled)
    } else {
        Ok(())
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

    /// Every variant, so a test can cover the whole enum.
    fn every_variant() -> Vec<PmuValidationError> {
        vec![
            PmuValidationError::CouldNotCreateTimer {
                errno: Errno::EINVAL,
                msg: "m",
            },
            PmuValidationError::UnexpectedTestingErrnoError {
                errno: Errno::EINVAL,
                msg: "m",
            },
            PmuValidationError::UnexpectedTestingError("m".to_owned()),
            PmuValidationError::IocPeriodBugDetected,
            #[cfg(target_arch = "x86_64")]
            PmuValidationError::CouldNotReadCpuInfo,
            PmuValidationError::HardwareCountersNotWorking {
                actual_events: 0,
                expected_min_events: 1,
                config: 0x5101c4,
            },
            PmuValidationError::OnlyOnePerformanceCounter,
            #[cfg(target_arch = "x86_64")]
            PmuValidationError::AmdSpecLockMapShouldBeDisabled,
            #[cfg(target_arch = "x86_64")]
            PmuValidationError::IntelKvmInTxcpBugDetected,
        ]
    }

    #[test]
    fn every_variant_name_is_its_identifier_and_distinct() {
        let variants = every_variant();
        let names: Vec<&str> = variants.iter().map(PmuValidationError::name).collect();
        let distinct: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(distinct.len(), variants.len(), "names collide: {names:?}");
        for (variant, name) in variants.iter().zip(&names) {
            // The Debug form of a variant starts with its identifier, so the
            // name cannot drift from the variant it reports.
            assert!(
                format!("{variant:?}").starts_with(name),
                "{name} does not name {variant:?}"
            );
            assert!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()),
                "{name} is not a bare identifier"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_speclockmap_variant_has_a_stable_name() {
        assert_eq!(
            PmuValidationError::AmdSpecLockMapShouldBeDisabled.name(),
            "AmdSpecLockMapShouldBeDisabled"
        );
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
            Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled) => {
                eprintln!(
                    "skipping arch-bug check: host has AMD Zen SpecLockMap enabled \
                     (a firmware setting, not a code bug); see \
                     https://github.com/rr-debugger/rr/wiki/Zen"
                );
            }
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
                Err(PmuValidationError::AmdSpecLockMapShouldBeDisabled) => {
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
