/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Provides a more rustic interface to a minimal set of `perf` functionality.
//!
//! Explicitly missing (because they are unnecessary) perf features include:
//! * Grouping
//! * Sample type flags
//! * Reading any kind of sample events
//! * BPF
//! * Hardware breakpoints
//!
//! The arguments and behaviors in this module generally correspond exactly to
//! those of `perf_event_open(2)`. No attempts are made to paper over the
//! non-determinism/weirndess of `perf`. For example, counter increments are
//! dropped whenever an event fires on a running thread.
//! [`PerfCounter::DISABLE_SAMPLE_PERIOD`] can be used to avoid this for sampling.
//! events.

use core::ptr::NonNull;
#[allow(unused_imports)] // only used if we have an error
use std::compile_error;
use std::sync::LazyLock;

use nix::sys::signal::Signal;
use nix::unistd::SysconfVar;
use nix::unistd::sysconf;
use perf_event_open_sys::bindings as perf;
use perf_event_open_sys::ioctls;
use reverie::Errno;
use reverie::Tid;
use tracing::error;
use tracing::warn;

use crate::validation::PmuValidationError;
use crate::validation::check_for_pmu_bugs;

static PMU_BUG: LazyLock<Result<(), PmuValidationError>> = LazyLock::new(check_for_pmu_bugs);

// Not available in the libc crate
const F_SETOWN_EX: libc::c_int = 15;
const F_SETSIG: libc::c_int = 10;
const F_OWNER_TID: libc::c_int = 0;
#[repr(C)]
struct f_owner_ex {
    pub type_: libc::c_int,
    pub pid: libc::pid_t,
}

/// An incomplete enumeration of events perf can monitor
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Event {
    #[allow(dead_code)] // used in tests
    /// A perf-supported hardware event.
    Hardware(HardwareEvent),
    /// A perf-supported software event.
    Software(SoftwareEvent),
    /// A raw CPU event. The inner value will have a CPU-specific meaning.
    Raw(u64),
}

/// An incomplete enumeration of hardware events perf can monitor.
#[allow(dead_code)] // used in tests
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HardwareEvent {
    /// Count retired instructions. Can be affected by hardware interrupt counts.
    Instructions,
    /// Count retired branch instructions.
    BranchInstructions,
}

/// An incomplete enumeration of software events perf can monitor.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SoftwareEvent {
    /// A placeholder event that counts nothing.
    Dummy,
}

/// A perf counter with a very limited range of configurability.
/// Construct via [`Builder`].
#[derive(Debug)]
pub struct PerfCounter {
    fd: libc::c_int,
    mmap: Option<NonNull<perf::perf_event_mmap_page>>,
    raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
}

impl Event {
    fn attr_type(self) -> u32 {
        match self {
            Event::Hardware(_) => perf::PERF_TYPE_HARDWARE,
            Event::Software(_) => perf::PERF_TYPE_SOFTWARE,
            Event::Raw(_) => perf::PERF_TYPE_RAW,
        }
    }

    fn attr_config(self) -> u64 {
        match self {
            Event::Raw(x) => x,
            Event::Hardware(HardwareEvent::Instructions) => perf::PERF_COUNT_HW_INSTRUCTIONS.into(),
            Event::Hardware(HardwareEvent::BranchInstructions) => {
                perf::PERF_COUNT_HW_BRANCH_INSTRUCTIONS.into()
            }
            Event::Software(SoftwareEvent::Dummy) => perf::PERF_COUNT_SW_DUMMY.into(),
        }
    }
}

/// Builder for a PerfCounter. Contains only the subset of the attributes that
/// this API allows manipulating set to non-defaults.
#[derive(Debug, Clone)]
pub struct Builder {
    pid: libc::pid_t,
    cpu: libc::c_int,
    evt: Event,
    sample_period: u64,
    precise_ip: u32,
    fast_reads: bool,
}

impl Builder {
    /// Initialize the builder. The initial configuration is for a software
    /// counting event that never increments.
    ///
    /// `pid` accepts a *TID* from `gettid(2)`. Passing `getpid(2)` will
    /// monitor the main thread of the calling thread group. Passing `0`
    /// monitors the calling thread. Passing `-1` monitors all threads on
    /// the specified CPU.
    ///
    /// `cpu` should almost always be `-1`, which tracks the specified `pid`
    /// across all CPUs. Non-negative integers track only the specified `pid`
    /// on that CPU.
    ///
    /// Passing `-1` for both `pid` and `cpu` will result in an error.
    pub fn new(pid: libc::pid_t, cpu: libc::c_int) -> Self {
        Self {
            pid,
            cpu,
            evt: Event::Software(SoftwareEvent::Dummy),
            sample_period: 0,
            precise_ip: 0,
            fast_reads: false,
        }
    }

    /// Select the event to monitor.
    pub fn event(&mut self, evt: Event) -> &mut Self {
        self.evt = evt;
        self
    }

    /// Set the period for sample collection. Default is 0, which creates a
    /// counting event.
    ///
    /// Because this module always sets `wakeup_events` to 1, this also
    /// specifies after how many events an overflow notification should be
    /// raised. If a signal has been setup with
    /// `PerfCounter::set_signal_delivery`], this corresponds to one sent
    /// signal. Overflow notifications are sent whenever the counter reaches a
    /// multiple of `sample_period`.
    ///
    /// If you only want accurate counts, pass
    /// `DISABLE_SAMPLE_PERIOD`. Passing `0` will also work, but will create a
    /// _counting_ event that cannot become a _sampling event_ via the
    /// `PERF_EVENT_IOC_PERIOD` ioctl.
    pub fn sample_period(&mut self, period: u64) -> &mut Self {
        self.sample_period = period;
        self
    }

    /// Set `precise_ip` on the underlying perf attribute structure. Valid
    /// values are 0-3; the underlying field is 2 bits.
    ///
    /// Non-zero values will cause perf to attempt to lower the skid of *samples*
    /// (but not necessarily notifications), usually via hardware features like
    /// Intel PEBS.
    ///
    /// Use with caution: experiments have shown that counters with non-zero
    /// `precise_ip` can drop events under certain circumstances. See
    /// `experiments/test_consistency.c` for more information.
    pub fn precise_ip(&mut self, precise_ip: u32) -> &mut Self {
        self.precise_ip = precise_ip;
        self
    }

    /// Enable fast reads via shared memory with the kernel for the latest
    /// counter value.
    pub fn fast_reads(&mut self, enable: bool) -> &mut Self {
        self.fast_reads = enable;
        self
    }

    /// Render the builder into a `PerfCounter`. Created counters begin in a
    /// disabled state. Additional initialization steps should be performed,
    /// followed by a call to [`PerfCounter::enable`].
    pub fn create(&self) -> Result<PerfCounter, Errno> {
        self.create_with_optional_raw_syscall(None)
    }

    pub(crate) fn create_with_raw_syscall(
        &self,
        raw_syscall: unsafe fn(i64, [u64; 6]) -> i64,
    ) -> Result<PerfCounter, Errno> {
        self.create_with_optional_raw_syscall(Some(raw_syscall))
    }

    fn create_with_optional_raw_syscall(
        &self,
        raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
    ) -> Result<PerfCounter, Errno> {
        let mut attr = perf::perf_event_attr::default();
        attr.size = core::mem::size_of_val(&attr) as u32;
        attr.type_ = self.evt.attr_type();
        attr.config = self.evt.attr_config();
        attr.__bindgen_anon_1.sample_period = self.sample_period;
        attr.set_disabled(1); // user must enable later
        attr.set_exclude_kernel(1); // we only care about user code
        attr.set_exclude_guest(1);
        attr.set_exclude_hv(1); // unlikely this is supported, but it doesn't hurt
        attr.set_pinned(1); // error state if we are descheduled from the PMU
        attr.set_precise_ip(self.precise_ip.into());
        attr.__bindgen_anon_2.wakeup_events = 1; // generate a wakeup (overflow) after one sample event

        let pid = self.pid;
        let cpu = self.cpu;
        let group_fd: libc::c_int = -1; // always create a new group
        let flags = perf::PERF_FLAG_FD_CLOEXEC; // marginally more safe if we fork+exec

        let fd = if let Some(raw_syscall) = raw_syscall {
            Errno::from_ret(unsafe {
                raw_syscall(
                    libc::SYS_perf_event_open,
                    [
                        (&raw const attr) as u64,
                        pid as i64 as u64,
                        cpu as i64 as u64,
                        group_fd as i64 as u64,
                        flags.into(),
                        0,
                    ],
                ) as usize
            })?
        } else {
            Errno::result(unsafe {
                libc::syscall(libc::SYS_perf_event_open, &attr, pid, cpu, group_fd, flags)
            })? as usize
        };
        let fd = fd as libc::c_int;

        let mmap = if self.fast_reads {
            let res = if let Some(raw_syscall) = raw_syscall {
                Errno::from_ret(unsafe {
                    raw_syscall(
                        libc::SYS_mmap,
                        [
                            0,
                            get_mmap_size() as u64,
                            libc::PROT_READ as u64,
                            libc::MAP_SHARED as u64,
                            fd as u64,
                            0,
                        ],
                    ) as usize
                })
                .map(|address| address as *mut libc::c_void)
            } else {
                Errno::result(unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        get_mmap_size(),
                        libc::PROT_READ, // leaving PROT_WRITE unset lets us passively read
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                })
            };
            match res {
                Ok(ptr) => match NonNull::new(ptr as *mut _) {
                    Some(ptr) => Some(ptr),
                    None => {
                        close_perf_fd(fd, raw_syscall);
                        return Err(Errno::ENOMEM);
                    }
                },
                Err(e) => {
                    close_perf_fd(fd, raw_syscall);
                    return Err(e);
                }
            }
        } else {
            None
        };

        Ok(PerfCounter {
            fd,
            mmap,
            raw_syscall,
        })
    }

    pub(crate) fn check_for_pmu_bugs(&mut self) -> &mut Self {
        if let Err(pmu_error) = &*PMU_BUG {
            error!(
                error = ?pmu_error,
                "PMU validation failed; RCB timers may be unreliable"
            );
        }
        self
    }
}

impl PerfCounter {
    /// Perf counters cannot be switched from sampling to non-sampling, so
    /// setting their period to this large value effectively disables overflows
    /// and sampling.
    pub const DISABLE_SAMPLE_PERIOD: u64 = 1 << 60;

    /// Call the `PERF_EVENT_IOC_ENABLE` ioctl. Enables increments of the
    /// counter and event generation.
    pub fn enable(&self) -> Result<(), Errno> {
        if let Some(raw_syscall) = self.raw_syscall {
            Errno::from_ret(unsafe {
                raw_syscall(
                    libc::SYS_ioctl,
                    [self.fd as u64, perf::ENABLE as u64, 0, 0, 0, 0],
                ) as usize
            })
            .and(Ok(()))
        } else {
            Errno::result(unsafe { ioctls::ENABLE(self.fd, 0) }).and(Ok(()))
        }
    }

    /// Call the `PERF_EVENT_IOC_DISABLE` ioctl. Disables increments of the
    /// counter and event generation.
    pub fn disable(&self) -> Result<(), Errno> {
        if let Some(raw_syscall) = self.raw_syscall {
            Errno::from_ret(unsafe {
                raw_syscall(
                    libc::SYS_ioctl,
                    [self.fd as u64, perf::DISABLE as u64, 0, 0, 0, 0],
                ) as usize
            })
            .and(Ok(()))
        } else {
            Errno::result(unsafe { ioctls::DISABLE(self.fd, 0) }).and(Ok(()))
        }
    }

    /// Corresponds exactly to the `PERF_EVENT_IOC_REFRESH` ioctl.
    #[allow(dead_code)]
    pub fn refresh(&self, count: libc::c_int) -> Result<(), Errno> {
        assert!(count != 0); // 0 is undefined behavior
        Errno::result(unsafe { ioctls::REFRESH(self.fd, 0) }).and(Ok(()))
    }

    /// Call the `PERF_EVENT_IOC_RESET` ioctl. Resets the counter value to 0,
    /// which results in delayed overflow events.
    pub fn reset(&self) -> Result<(), Errno> {
        if let Some(raw_syscall) = self.raw_syscall {
            Errno::from_ret(unsafe {
                raw_syscall(
                    libc::SYS_ioctl,
                    [self.fd as u64, perf::RESET as u64, 0, 0, 0, 0],
                ) as usize
            })
            .and(Ok(()))
        } else {
            Errno::result(unsafe { ioctls::RESET(self.fd, 0) }).and(Ok(()))
        }
    }

    /// Call the `PERF_EVENT_IOC_PERIOD` ioctl. This causes the counter to
    /// behave as if `ticks` was the original argument to `sample_period` in
    /// the builder.
    pub fn set_period(&self, ticks: u64) -> Result<(), Errno> {
        // The bindings are wrong for this ioctl. The method signature takes a
        // u64, but the actual ioctl expects a pointer to a u64. Thus, we use
        // the constant manually.

        // This ioctl shouldn't mutate it's argument per its API. But in case it
        // does, create a mutable copy to avoid Rust UB.
        let mut ticks = ticks;
        if let Some(raw_syscall) = self.raw_syscall {
            Errno::from_ret(unsafe {
                raw_syscall(
                    libc::SYS_ioctl,
                    [
                        self.fd as u64,
                        perf::PERIOD as u64,
                        (&raw mut ticks) as u64,
                        0,
                        0,
                        0,
                    ],
                ) as usize
            })
            .and(Ok(()))
        } else {
            Errno::result(unsafe {
                libc::ioctl(self.fd, perf::PERIOD as _, &mut ticks as *mut u64)
            })
            .and(Ok(()))
        }
    }

    /// Call the `PERF_EVENT_IOC_ID` ioctl. Returns a unique identifier for this
    /// perf counter.
    #[allow(dead_code)]
    pub fn id(&self) -> Result<u64, Errno> {
        let mut res = 0u64;
        Errno::result(unsafe { ioctls::ID(self.fd, &mut res as *mut u64) })?;
        Ok(res)
    }

    /// Sets up overflow events to deliver a `SIGPOLL`-style signal, with the
    /// signal number specified in `signal`, to the specified `thread`.
    ///
    /// There is no reason this couldn't be called at any point, but typial use
    /// cases will set up signal delivery once or not at all.
    pub fn set_signal_delivery(&self, thread: Tid, signal: Signal) -> Result<(), Errno> {
        let owner = f_owner_ex {
            type_: F_OWNER_TID,
            pid: thread.as_raw(),
        };
        if let Some(raw_syscall) = self.raw_syscall {
            for (command, argument) in [
                (F_SETOWN_EX, (&raw const owner) as u64),
                (libc::F_SETFL, libc::O_ASYNC as u64),
                (F_SETSIG, signal as u64),
            ] {
                Errno::from_ret(unsafe {
                    raw_syscall(
                        libc::SYS_fcntl,
                        [self.fd as u64, command as u64, argument, 0, 0, 0],
                    ) as usize
                })?;
            }
            return Ok(());
        }
        Errno::result(unsafe { libc::fcntl(self.fd, F_SETOWN_EX, &owner as *const _) })?;
        Errno::result(unsafe { libc::fcntl(self.fd, libc::F_SETFL, libc::O_ASYNC) })?;
        Errno::result(unsafe { libc::fcntl(self.fd, F_SETSIG, signal as i32) })?;
        Ok(())
    }

    /// Read the current value of the counter.
    pub fn ctr_value(&self) -> Result<u64, Errno> {
        let mut value = 0u64;
        let expected_bytes = std::mem::size_of_val(&value);
        loop {
            let res = if let Some(raw_syscall) = self.raw_syscall {
                match Errno::from_ret(unsafe {
                    raw_syscall(
                        libc::SYS_read,
                        [
                            self.fd as u64,
                            (&raw mut value) as u64,
                            expected_bytes as u64,
                            0,
                            0,
                            0,
                        ],
                    ) as usize
                }) {
                    Ok(value) => value as isize,
                    Err(Errno::EINTR) => continue,
                    Err(error) => return Err(error),
                }
            } else {
                unsafe { libc::read(self.fd, (&raw mut value).cast(), expected_bytes) }
            };
            if res == -1 {
                let errno = Errno::last();
                if errno != Errno::EINTR {
                    return Err(errno);
                }
            }
            if res == 0 {
                // EOF: this only occurs when attr.pinned = 1 and our event was descheduled.
                // This unrecoverably gives us innacurate counts.
                panic!("pinned perf event descheduled!")
            }
            if res == expected_bytes as isize {
                break;
            }
        }
        Ok(value)
    }

    pub(crate) fn ctr_value_paused_once(&self) -> Result<u64, Errno> {
        use std::ptr::addr_of;
        use std::ptr::addr_of_mut;
        use std::ptr::read_volatile;

        let mapping = self.mmap.ok_or(Errno::EOPNOTSUPP)?;
        let gate = self.raw_syscall.ok_or(Errno::EOPNOTSUPP)?;
        let page = mapping.as_ptr();
        let sequence = unsafe { read_once(addr_of_mut!((*page).lock)) };
        if sequence & 1 != 0 {
            return Err(Errno::EAGAIN);
        }
        smp_rmb();
        let (index, enabled, running) = unsafe {
            (
                read_volatile(addr_of!((*page).index)),
                read_volatile(addr_of!((*page).time_enabled)),
                read_volatile(addr_of!((*page).time_running)),
            )
        };
        if index != 0 {
            return Err(Errno::EBUSY);
        }
        if enabled != running {
            return Err(Errno::ENODEV);
        }
        let mut value = 0u64;
        let length = std::mem::size_of_val(&value);
        let result = Errno::from_ret(unsafe {
            gate(
                libc::SYS_read,
                [
                    self.fd as u64,
                    (&raw mut value) as u64,
                    length as u64,
                    0,
                    0,
                    0,
                ],
            ) as usize
        })?;
        smp_rmb();
        if sequence != unsafe { read_once(addr_of_mut!((*page).lock)) } {
            return Err(Errno::EAGAIN);
        }
        if result == 0 {
            return Err(Errno::ENODEV);
        }
        if result != length {
            return Err(Errno::EIO);
        }
        Ok(value)
    }

    /// Perform a fast read, which doesn't involve a syscall in the fast path.
    /// This falls back to a slow syscall read where necessary, including if
    /// fast reads weren't enabled in the `Builder`.
    pub fn ctr_value_fast(&self) -> Result<u64, Errno> {
        match self.mmap {
            Some(ptr) => {
                // SAFETY: self.mmap is constructed as the correct page or not at all
                let res = unsafe { self.ctr_value_fast_loop(ptr) };
                // TODO: remove this assertion after we're confident in correctness
                debug_assert_eq!(res, self.ctr_value_fallback());
                res
            }
            None => self.ctr_value_fallback(),
        }
    }

    #[cold]
    fn ctr_value_fallback(&self) -> Result<u64, Errno> {
        self.ctr_value()
    }

    /// Read the current counter value using the `rdpmc` instruction, with no
    /// syscall on the fast path even when the counter is *currently scheduled*
    /// on the PMU (`index != 0`).
    ///
    /// This is an **additive read primitive** intended for **in-guest,
    /// same-core** use: a guest thread reading its own performance counter from
    /// user space. It changes only *how a counter value is read* — it does not
    /// change how time or ordering is observed, and it does not alter the
    /// behavior of [`ctr_value`](Self::ctr_value) or
    /// [`ctr_value_fast`](Self::ctr_value_fast).
    ///
    /// # Correctness contract
    ///
    /// `rdpmc` reads the hardware PMC of *whatever core executes the
    /// instruction*. It is therefore only correct when the calling thread is
    /// the monitored thread running on the core the counter is scheduled on —
    /// i.e. the guest reading itself in-guest. A cross-core reader (such as the
    /// ptrace supervisor reading a stopped guest on another core) must NOT use
    /// this; that is exactly why [`ctr_value_fast`](Self::ctr_value_fast)
    /// deliberately falls back to the syscall read when `index != 0`.
    ///
    /// When the counter is not currently scheduled on the PMU (`index == 0`)
    /// there is no live hardware counter to read, so this returns the mmap
    /// `offset` via the seqlock read path with no syscall — the same value
    /// [`ctr_value_fast`](Self::ctr_value_fast) returns. `cap_user_rdpmc` does
    /// not matter in that case; it is only consulted when the counter is live.
    ///
    /// Falls back to the [`ctr_value`](Self::ctr_value) syscall read only when:
    /// * fast reads were not enabled on the [`Builder`] (no mmap page), or
    /// * the counter is live (`index != 0`) but the kernel/CPU does not permit
    ///   user-space `rdpmc` (`cap_user_rdpmc` clear).
    ///
    /// On a non-x86-64 target there is no portable `rdpmc`, so the
    /// [`ctr_value`](Self::ctr_value) syscall read is always used.
    // Additive in-guest read primitive; no in-tree caller yet (the in-guest
    // patching backend that will use it is still being built).
    #[allow(dead_code)]
    pub fn ctr_value_rdpmc(&self) -> Result<u64, Errno> {
        match self.mmap {
            Some(ptr) => {
                // SAFETY: self.mmap is constructed as the correct page or not at all
                unsafe { self.ctr_value_rdpmc_loop(ptr) }
            }
            None => self.ctr_value_fallback(),
        }
    }

    /// Safety: `ptr` must refer to the metadata page corresponding to self.fd.
    #[deny(unsafe_op_in_unsafe_fn)]
    #[inline(always)]
    unsafe fn ctr_value_fast_loop(
        &self,
        ptr: NonNull<perf::perf_event_mmap_page>,
    ) -> Result<u64, Errno> {
        // This implements synchronization with the kernel via a seqlock,
        // see https://www.kernel.org/doc/html/latest/locking/seqlock.html.
        // Also see experiments/perf_fast_reads.c for more details on fast reads.
        use std::ptr::addr_of_mut;
        let ptr = ptr.as_ptr();
        let mut seq;
        let mut running;
        let mut enabled;
        let mut count;
        loop {
            // Acquire a lease on the seqlock -- even values are outside of
            // writers' critical sections.
            loop {
                // SAFETY: ptr->lock is valid and aligned
                seq = unsafe { read_once(addr_of_mut!((*ptr).lock)) };
                if seq & 1 == 0 {
                    break;
                }
            }
            smp_rmb(); // force re-reads of other data
            let index;
            // SAFETY: these reads are synchronized by the correct reads of the
            // seqlock. We don't do anything with them until after the outer
            // loop finishing has guaranteed our read was serialized.
            unsafe {
                running = (*ptr).time_running;
                enabled = (*ptr).time_enabled;
                count = (*ptr).offset;
                index = (*ptr).index;
            }
            if index != 0 {
                // `index` being non-zero indicates we need to read from the
                // hardware counter and add it to our count. Instead, we
                // fallback to the slow path for a few reasons:
                // 1. This only works if we're on the same core, which is basically
                //    never true for our usecase.
                // 2. Reads of an active PMU are racy.
                // 3. The PMU should almost never be active, because we should
                //    generally only read from stopped processes.
                return self.ctr_value_fallback();
            }
            smp_rmb();
            // SAFETY: ptr->lock is valid and aligned
            if seq == unsafe { read_once(addr_of_mut!((*ptr).lock)) } {
                // if seq is unchanged, we didn't race with writer
                break;
            }
        }
        // This check must be outside the loop to ensure our reads were actually
        // serialized with any writes.
        if running != enabled {
            // Non-equal running/enabled time indicates the event was
            // descheduled at some point, meaning our counts are inaccurate.
            // This is not recoverable. The slow-read equivalent is getting EOF
            // when attr.pinned = 1.
            panic!("fast-read perf event was probably descheduled!")
        }
        Ok(count as u64)
    }

    /// Safety: `ptr` must refer to the metadata page corresponding to self.fd,
    /// and the calling thread must be the monitored thread running on the core
    /// the counter is scheduled on (see [`ctr_value_rdpmc`](Self::ctr_value_rdpmc)).
    #[cfg(target_arch = "x86_64")]
    #[allow(dead_code)]
    #[deny(unsafe_op_in_unsafe_fn)]
    #[inline(always)]
    unsafe fn ctr_value_rdpmc_loop(
        &self,
        ptr: NonNull<perf::perf_event_mmap_page>,
    ) -> Result<u64, Errno> {
        use std::ptr::addr_of_mut;
        let ptr = ptr.as_ptr();
        // `pmc_width` and the capability bits are fixed for the lifetime of the
        // mapping, so read them once outside the seqlock loop.
        // SAFETY: ptr is a valid, aligned perf metadata page.
        let width = unsafe { (*ptr).pmc_width } as u32;
        // SAFETY: reading the raw `capabilities` word of the capability union.
        let caps = unsafe { (*ptr).__bindgen_anon_1.capabilities };
        // `cap_user_rdpmc` is bit 2 of the capability bitfield (after `cap_bit0`
        // and `cap_bit0_is_deprecated`).
        let cap_user_rdpmc = (caps >> 2) & 1 == 1;

        let mut seq;
        let mut running;
        let mut enabled;
        let mut count: i64;
        // This mirrors the seqlock synchronization in `ctr_value_fast_loop`; see
        // https://www.kernel.org/doc/html/latest/locking/seqlock.html and the
        // rdpmc self-monitoring example in perf_event_open(2).
        loop {
            loop {
                // SAFETY: ptr->lock is valid and aligned
                seq = unsafe { read_once(addr_of_mut!((*ptr).lock)) };
                if seq & 1 == 0 {
                    break;
                }
            }
            smp_rmb();
            let index;
            // SAFETY: these reads are synchronized by the seqlock; nothing is
            // acted upon until the outer loop confirms serialization.
            unsafe {
                running = (*ptr).time_running;
                enabled = (*ptr).time_enabled;
                count = (*ptr).offset;
                index = (*ptr).index;
            }
            if index != 0 {
                if !cap_user_rdpmc {
                    // Counter is live but user-space rdpmc is disabled; the only
                    // correct read is the slow syscall path.
                    return self.ctr_value_fallback();
                }
                // `index != 0` means the counter is scheduled on this core's
                // PMU; read the raw hardware counter and add it to `offset`.
                // Sign-extend the rdpmc result from `pmc_width` bits to 64 bits
                // (arithmetic shifts on a signed value), exactly as the kernel's
                // rdpmc self-monitoring example in perf_event_open(2) does: the
                // hardware counter can wrap, and `offset` is chosen so that
                // `offset + sign_extend(rdpmc)` is the current count mod 2^width.
                // SAFETY: index-1 is the currently-scheduled PMC for this core,
                // and cap_user_rdpmc confirmed user-space rdpmc is permitted.
                let raw = unsafe { rdpmc(index - 1) };
                let pmc = ((raw << (64 - width)) as i64) >> (64 - width);
                count = count.wrapping_add(pmc);
            }
            smp_rmb();
            // SAFETY: ptr->lock is valid and aligned
            if seq == unsafe { read_once(addr_of_mut!((*ptr).lock)) } {
                // Unchanged seq => our reads were not torn by a writer.
                break;
            }
        }
        if running != enabled {
            // Non-equal running/enabled time means the event was descheduled at
            // some point, making counts inaccurate and unrecoverable. Same
            // condition the slow path detects as EOF when attr.pinned = 1.
            panic!("rdpmc perf event was probably descheduled!")
        }
        Ok(count as u64)
    }

    /// Non-x86-64 fallback: there is no portable `rdpmc`, so always use the
    /// syscall read.
    #[cfg(not(target_arch = "x86_64"))]
    #[allow(dead_code)]
    #[inline(always)]
    unsafe fn ctr_value_rdpmc_loop(
        &self,
        _ptr: NonNull<perf::perf_event_mmap_page>,
    ) -> Result<u64, Errno> {
        self.ctr_value_fallback()
    }

    /// Return the underlying perf fd.
    pub fn raw_fd(&self) -> libc::c_int {
        self.fd
    }

    /// Attempt a same-thread sample once, without syscall fallback or panic.
    #[inline(always)]
    pub(crate) fn sample_rdpmc_once(&self) -> crate::InGuestRcbSample {
        #[cfg(target_arch = "x86_64")]
        {
            self.sample_rdpmc_once_using(|index| unsafe { rdpmc(index) })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            crate::InGuestRcbSample::Unavailable
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    fn sample_rdpmc_once_using(
        &self,
        read_pmc: impl FnOnce(u32) -> u64,
    ) -> crate::InGuestRcbSample {
        use std::ptr::addr_of;
        use std::ptr::addr_of_mut;
        use std::ptr::read_volatile;

        use crate::InGuestRcbSample;

        let Some(mapping) = self.mmap else {
            return InGuestRcbSample::Unavailable;
        };
        let page = mapping.as_ptr();
        let sequence = unsafe { read_once(addr_of_mut!((*page).lock)) };
        if sequence & 1 != 0 {
            return InGuestRcbSample::Retry;
        }
        smp_rmb();
        let (index, caps, width, offset, enabled, running) = unsafe {
            (
                read_volatile(addr_of!((*page).index)),
                read_volatile(addr_of!((*page).__bindgen_anon_1.capabilities)),
                read_volatile(addr_of!((*page).pmc_width)),
                read_volatile(addr_of!((*page).offset)),
                read_volatile(addr_of!((*page).time_enabled)),
                read_volatile(addr_of!((*page).time_running)),
            )
        };
        let sample = if index == 0 || enabled != running {
            InGuestRcbSample::Descheduled
        } else if caps & (1 << 2) == 0 || !(1..=64).contains(&width) {
            InGuestRcbSample::Unavailable
        } else {
            crate::in_guest_timer::sampled_count(offset, read_pmc(index - 1), width)
        };
        smp_rmb();
        if sequence != unsafe { read_once(addr_of_mut!((*page).lock)) } {
            InGuestRcbSample::Retry
        } else {
            sample
        }
    }
}

/// Execute the `rdpmc` instruction to read hardware performance counter number
/// `counter`. Returns the raw counter value (the low `pmc_width` bits are
/// meaningful; higher bits are unspecified and must be masked by the caller).
/// On CPU/kernel configurations with execution-serializing LFENCE semantics,
/// these fences bracket the RDPMC observation. LFENCE availability alone does
/// not establish that precondition. Metadata compiler fences do not order RDPMC.
/// This does not establish an exact guest-instruction boundary. Consumers that
/// require exact instruction-relative sampling must qualify the platform, use
/// a separately proven sequence, or refuse that exact mode when unqualified.
///
/// SAFETY: the caller must ensure `counter` is the currently-scheduled PMC
/// index for the calling core (i.e. `index - 1` from the perf mmap page) and
/// that user-space `rdpmc` is permitted (`cap_user_rdpmc`). Executing `rdpmc`
/// without user-space access enabled raises `#GP`.
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
#[inline(always)]
unsafe fn rdpmc(counter: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: rdpmc reads the counter selected by ecx into edx:eax and touches
    // no memory or other registers.
    unsafe {
        core::arch::asm!(
            "lfence",
            "rdpmc",
            "lfence",
            in("ecx") counter,
            out("eax") lo,
            out("edx") hi,
            options(nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

fn close_perf_fd(fd: libc::c_int, raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>) {
    if let Some(raw_syscall) = raw_syscall {
        Errno::from_ret(unsafe {
            raw_syscall(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) as usize
        })
        .expect("Could not close perf fd");
    } else {
        Errno::result(unsafe { libc::close(fd) }).expect("Could not close perf fd");
    }
}
fn close_mmap(
    ptr: *mut perf::perf_event_mmap_page,
    raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
) {
    if let Some(raw_syscall) = raw_syscall {
        Errno::from_ret(unsafe {
            raw_syscall(
                libc::SYS_munmap,
                [ptr as u64, get_mmap_size() as u64, 0, 0, 0, 0],
            ) as usize
        })
        .expect("Could not munmap ring buffer");
    } else {
        Errno::result(unsafe { libc::munmap(ptr as *mut _, get_mmap_size()) })
            .expect("Could not munmap ring buffer");
    }
}

impl Drop for PerfCounter {
    fn drop(&mut self) {
        if let Some(ptr) = self.mmap {
            close_mmap(ptr.as_ptr(), self.raw_syscall);
        }
        close_perf_fd(self.fd, self.raw_syscall);
    }
}

// Safety:
// The mmap region is never written to. Multiple readers then race with the
// kernel as any single thread would. Though the reads are racy, that is the
// intended behavior of the perf api.
unsafe impl std::marker::Send for PerfCounter {}
unsafe impl std::marker::Sync for PerfCounter {}

#[cfg(all(test, target_arch = "x86_64"))]
mod in_guest_sample_tests {
    use super::*;
    use crate::InGuestRcbSample;

    unsafe fn forbidden_syscall(_: i64, _: [u64; 6]) -> i64 {
        panic!("single-attempt sampling must never fall back to a syscall")
    }

    thread_local! {
        static PAUSED_VALUE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static PAUSED_RESULT: std::cell::Cell<i64> = const { std::cell::Cell::new(8) };
        static PAUSED_READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static CHANGE_SEQUENCE: std::cell::Cell<*mut u32> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    }

    unsafe fn paused_read_gate(number: i64, arguments: [u64; 6]) -> i64 {
        assert_eq!(number, libc::SYS_read);
        assert_eq!(arguments[0], 99);
        assert_eq!(arguments[2], 8);
        PAUSED_READS.set(PAUSED_READS.get() + 1);
        if PAUSED_RESULT.get() == 8 {
            unsafe { *(arguments[1] as *mut u64) = PAUSED_VALUE.get() };
        }
        if !CHANGE_SEQUENCE.get().is_null() {
            unsafe { *CHANGE_SEQUENCE.get() = 4 };
        }
        PAUSED_RESULT.get()
    }

    fn paused_fixture() -> (
        Box<perf::perf_event_mmap_page>,
        std::mem::ManuallyDrop<PerfCounter>,
    ) {
        PAUSED_VALUE.set(0);
        PAUSED_RESULT.set(8);
        PAUSED_READS.set(0);
        CHANGE_SEQUENCE.set(std::ptr::null_mut());
        let mut page = Box::<perf::perf_event_mmap_page>::default();
        page.lock = 2;
        let counter = std::mem::ManuallyDrop::new(PerfCounter {
            fd: 99,
            mmap: Some(NonNull::from(page.as_mut())),
            raw_syscall: Some(paused_read_gate),
        });
        (page, counter)
    }

    #[test]
    fn paused_read_preserves_exact_counts_without_retry() {
        let (_page, counter) = paused_fixture();
        for value in [0, 1, 2, (1 << 53) + 1, u64::MAX] {
            PAUSED_VALUE.set(value);
            let previous = PAUSED_READS.get();
            assert_eq!(counter.ctr_value_paused_once(), Ok(value));
            assert_eq!(PAUSED_READS.get(), previous + 1);
        }
    }

    #[test]
    fn paused_read_errors_are_explicit_and_not_retried() {
        let (_page, counter) = paused_fixture();
        for (result, error) in [
            (0, Errno::ENODEV),
            (7, Errno::EIO),
            (9, Errno::EIO),
            (-(libc::EINTR as i64), Errno::EINTR),
            (-(libc::EBADF as i64), Errno::EBADF),
        ] {
            PAUSED_RESULT.set(result);
            let previous = PAUSED_READS.get();
            assert_eq!(counter.ctr_value_paused_once(), Err(error));
            assert_eq!(PAUSED_READS.get(), previous + 1);
        }
    }

    #[test]
    fn paused_read_rejects_active_lost_or_unavailable_state_before_syscall() {
        let (mut page, mut counter) = paused_fixture();
        page.lock = 3;
        assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EAGAIN));
        page.lock = 2;
        page.index = 1;
        assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EBUSY));
        page.index = 0;
        page.time_enabled = 1;
        assert_eq!(counter.ctr_value_paused_once(), Err(Errno::ENODEV));
        page.time_enabled = 0;
        counter.raw_syscall = None;
        assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EOPNOTSUPP));
        counter.raw_syscall = Some(paused_read_gate);
        counter.mmap = None;
        assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EOPNOTSUPP));
        assert_eq!(PAUSED_READS.get(), 0);
    }

    #[test]
    fn paused_read_rejects_changed_metadata_after_one_read() {
        let (mut page, counter) = paused_fixture();
        CHANGE_SEQUENCE.set(&raw mut page.lock);
        assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EAGAIN));
        assert_eq!(PAUSED_READS.get(), 1);
        CHANGE_SEQUENCE.set(std::ptr::null_mut());
    }

    #[test]
    fn reports_unavailable_without_mapping_or_syscall() {
        let counter = std::mem::ManuallyDrop::new(PerfCounter {
            fd: -1,
            mmap: None,
            raw_syscall: Some(forbidden_syscall),
        });
        assert_eq!(counter.sample_rdpmc_once(), InGuestRcbSample::Unavailable);
    }

    #[test]
    fn reports_busy_descheduled_and_unavailable_metadata_without_retrying() {
        let mut page = Box::<perf::perf_event_mmap_page>::default();
        let counter = std::mem::ManuallyDrop::new(PerfCounter {
            fd: -1,
            mmap: Some(NonNull::from(page.as_mut())),
            raw_syscall: Some(forbidden_syscall),
        });
        page.lock = 1;
        assert_eq!(counter.sample_rdpmc_once(), InGuestRcbSample::Retry);
        page.lock = 2;
        assert_eq!(counter.sample_rdpmc_once(), InGuestRcbSample::Descheduled);
        page.index = 1;
        assert_eq!(counter.sample_rdpmc_once(), InGuestRcbSample::Unavailable);
        page.time_enabled = 1;
        assert_eq!(counter.sample_rdpmc_once(), InGuestRcbSample::Descheduled);
        page.time_running = 1;
        page.__bindgen_anon_1.capabilities = 1 << 2;
        for invalid_width in [0, 65, u16::MAX] {
            page.pmc_width = invalid_width;
            assert_eq!(counter.sample_rdpmc_once(), InGuestRcbSample::Unavailable);
        }
    }

    #[test]
    fn successful_sampling_preserves_adjacent_counts_through_metadata_path() {
        let mut page = Box::<perf::perf_event_mmap_page>::default();
        page.lock = 2;
        page.index = 7;
        page.__bindgen_anon_1.capabilities = 1 << 2;
        page.pmc_width = 48;
        let counter = std::mem::ManuallyDrop::new(PerfCounter {
            fd: -1,
            mmap: Some(NonNull::from(page.as_mut())),
            raw_syscall: Some(forbidden_syscall),
        });
        for offset in [1000, (1_i64 << 53), i64::MAX - 64] {
            page.offset = offset;
            for adjacent in 1..=33 {
                assert_eq!(
                    counter.sample_rdpmc_once_using(|index| {
                        assert_eq!(index, 6);
                        adjacent
                    }),
                    InGuestRcbSample::Value(offset as u64 + adjacent)
                );
            }
        }
    }

    #[test]
    fn changed_metadata_discards_a_successful_counter_read_without_retry() {
        let mut page = Box::<perf::perf_event_mmap_page>::default();
        page.lock = 2;
        page.index = 1;
        page.__bindgen_anon_1.capabilities = 1 << 2;
        page.pmc_width = 48;
        let pointer = NonNull::from(page.as_mut());
        let counter = std::mem::ManuallyDrop::new(PerfCounter {
            fd: -1,
            mmap: Some(pointer),
            raw_syscall: Some(forbidden_syscall),
        });
        assert_eq!(
            counter.sample_rdpmc_once_using(|index| {
                assert_eq!(index, 0);
                unsafe { (*pointer.as_ptr()).lock = 4 };
                1001
            }),
            InGuestRcbSample::Retry
        );
    }
}

fn get_mmap_size() -> usize {
    // Use a single page; we only want the perf metadata
    sysconf(SysconfVar::PAGE_SIZE)
        .expect("failed to query the system page size")
        .expect("the system did not report a page size")
        .try_into()
        .expect("the system page size must fit in usize")
}

/// Force a relaxed atomic load. Like Linux's READ_ONCE.
/// SAFETY: caller must ensure v points to valid data and is aligned
#[inline(always)]
#[deny(unsafe_op_in_unsafe_fn)]
unsafe fn read_once(v: *mut u32) -> u32 {
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering::Relaxed;
    // SAFETY: AtomicU32 is guaranteed to have the same in-memory representation
    // SAFETY: The UnsafeCell inside AtomicU32 allows aliasing with *mut
    // SAFETY: The reference doesn't escape this function, so any lifetime is ok
    let av: &AtomicU32 = unsafe { &*(v as *const AtomicU32) };
    av.load(Relaxed)
}

#[inline(always)]
fn smp_rmb() {
    use core::sync::atomic::Ordering::SeqCst;
    use core::sync::atomic::compiler_fence;
    compiler_fence(SeqCst);
}

fn handle_perf_pmu_error(errno: Errno) -> bool {
    match errno {
        Errno::ENOENT | Errno::EPERM | Errno::EACCES | Errno::ENOSYS => {
            warn!(
                %errno,
                "PMU hardware-event capability probe failed; performance counters are unavailable"
            );
        }
        _ => {
            warn!("Perf feature check failed unexpectedly due to {errno}; assuming unsupported");
        }
    }

    false
}

// Test if we have PMU access by doing a check for a basic hardware event.
fn test_perf_pmu_support() -> bool {
    // Do a raw perf_event_open because our default configuration has flags that
    // might be the actual cause of the error, which we want to catch separately.
    let evt = Event::Hardware(HardwareEvent::Instructions);
    let mut attr = perf::perf_event_attr::default();
    attr.size = core::mem::size_of_val(&attr) as u32;
    attr.type_ = evt.attr_type();
    attr.config = evt.attr_config();
    attr.__bindgen_anon_1.sample_period = PerfCounter::DISABLE_SAMPLE_PERIOD;
    attr.set_exclude_kernel(1); // lowers permission requirements

    let pid: libc::pid_t = 0; // track this thread
    let cpu: libc::c_int = -1; // across any CPU
    let group_fd: libc::c_int = -1;
    let flags = perf::PERF_FLAG_FD_CLOEXEC;
    let res = Errno::result(unsafe {
        libc::syscall(libc::SYS_perf_event_open, &attr, pid, cpu, group_fd, flags)
    });
    match res {
        Ok(fd) => {
            Errno::result(unsafe { libc::close(fd as libc::c_int) })
                .expect("perf feature check: close(fd) failed");
            true
        }
        Err(errno) => handle_perf_pmu_error(errno),
    }
}

static IS_PERF_SUPPORTED: LazyLock<bool> = LazyLock::new(test_perf_pmu_support);

/// Returns true if the current system configuration supports use of perf for
/// hardware events.
pub fn is_perf_supported() -> bool {
    *IS_PERF_SUPPORTED
}

/// Concisely return if `is_perf_supported` is `false`. Useful for guarding
/// tests.
#[macro_export]
macro_rules! ret_without_perf {
    () => {
        if !$crate::is_perf_supported() {
            return;
        }
    };
    (expr:expr) => {
        if !$crate::is_perf_supported() {
            return ($expr);
        }
    };
}

/// Perform exactly `count+1` conditional branch instructions. Useful for
/// testing timer-related code.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
pub fn do_branches(mut count: u64) {
    // Anything but assembly is unreliable between debug and release
    unsafe {
        // Loop until carry flag is set, indicating underflow
        core::arch::asm!(
            "2:",
            "sub {0}, 1",
            "jnz 2b",
            inout(reg) count,
        )
    }

    assert_eq!(count, 0);
}

/// Perform exactly `count+1` conditional branch instructions. Useful for
/// testing timer-related code.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
pub fn do_branches(mut count: u64) {
    unsafe {
        core::arch::asm!(
            "2:",
            "subs {0}, {0}, #0x1",
            "b.ne 2b",
            inout(reg) count,
        )
    }

    assert_eq!(count, 0);
}

#[cfg(test)]
mod support_test {
    use super::*;

    #[test]
    fn perf_event_open_errors_mean_pmu_is_unsupported() {
        for errno in [
            Errno::ENOENT,
            Errno::EPERM,
            Errno::EACCES,
            Errno::ENOSYS,
            Errno::EINVAL,
        ] {
            assert!(!handle_perf_pmu_error(errno));
        }
    }
}

// NOTE: aarch64 doesn't work with
// `Event::Hardware(HardwareEvent::BranchInstructions)`, so these tests are
// disabled for that architecture. Most likely, we need to use `Event::Raw`
// instead to enable these tests.
#[cfg(all(test, target_arch = "x86_64"))]
mod test {
    use nix::unistd::gettid;

    use super::*;

    #[test]
    fn test_do_branches() {
        do_branches(1000);
    }

    #[test]
    fn trace_self() {
        ret_without_perf!();
        let pc = Builder::new(gettid().as_raw(), -1)
            .sample_period(PerfCounter::DISABLE_SAMPLE_PERIOD)
            .event(Event::Hardware(HardwareEvent::BranchInstructions))
            .create()
            .expect("perf test operation should succeed");
        pc.reset().expect("perf test operation should succeed");
        pc.enable().expect("perf test operation should succeed");
        const ITERS: u64 = 10000;
        do_branches(ITERS);
        pc.disable().expect("perf test operation should succeed");
        let ctr = pc.ctr_value().expect("perf test operation should succeed");
        assert!(ctr >= ITERS);
        assert!(ctr <= ITERS + 100); // `.disable()` overhead
    }

    /// The in-guest `rdpmc` read must agree with the syscall read. Because the
    /// counter is live, we can't assert exact equality; instead we bracket the
    /// rdpmc read between two syscall reads on the same (monotonic) thread and
    /// require `before <= rdpmc <= after`.
    #[test]
    fn rdpmc_read_agrees_with_syscall_read() {
        ret_without_perf!();
        let pc = Builder::new(gettid().as_raw(), -1)
            .sample_period(PerfCounter::DISABLE_SAMPLE_PERIOD)
            .event(Event::Hardware(HardwareEvent::BranchInstructions))
            .fast_reads(true)
            .create()
            .expect("perf test operation should succeed");
        pc.reset().expect("perf test operation should succeed");
        pc.enable().expect("perf test operation should succeed");

        // Repeat so we exercise the live (`index != 0`) case, which requires the
        // counter to be scheduled on this core when we read it.
        for _ in 0..1000 {
            do_branches(1000);
            let before = pc.ctr_value().expect("syscall read");
            let via_rdpmc = pc.ctr_value_rdpmc().expect("rdpmc read");
            let after = pc.ctr_value().expect("syscall read");
            assert!(
                before <= via_rdpmc && via_rdpmc <= after,
                "rdpmc read {via_rdpmc} not in bracket [{before}, {after}]"
            );
        }
    }

    #[test]
    fn rdpmc_preserves_one_branch_intervals() {
        let config = crate::timer::PmuConfig::try_new().expect("hardware RCB PMU required");
        let counter = Builder::new(0, -1)
            .sample_period(0)
            .event(config.rcb_event())
            .fast_reads(true)
            .create()
            .expect("hardware RCB counter required");
        let page = counter.mmap.expect("perf metadata required").as_ptr();
        let mut samples = Vec::with_capacity(4096);
        counter.reset().unwrap();
        counter.enable().unwrap();
        let sequence = unsafe { std::ptr::read_volatile(&raw const (*page).lock) };
        let index = unsafe { std::ptr::read_volatile(&raw const (*page).index) };
        let capabilities = unsafe { (*page).__bindgen_anon_1.capabilities };
        assert_eq!(sequence & 1, 0);
        assert_ne!(capabilities & (1 << 2), 0, "user RDPMC required");
        assert_ne!(index, 0, "counter must be scheduled");
        let selector = index - 1;
        for _ in 0..4096 {
            let before = unsafe { rdpmc(selector) };
            unsafe {
                core::arch::asm!(
                    "test eax, eax",
                    "jz 2f",
                    "2:",
                    in("eax") 0,
                    options(nostack, nomem),
                );
            }
            let after = unsafe { rdpmc(selector) };
            samples.push([before, after]);
        }
        let final_sequence = unsafe { std::ptr::read_volatile(&raw const (*page).lock) };
        counter.disable().unwrap();
        assert_eq!(
            sequence, final_sequence,
            "PMU mapping changed during measurement"
        );
        for (iteration, [before, after]) in samples.into_iter().enumerate() {
            assert_eq!(
                after.wrapping_sub(before),
                1,
                "sample {iteration}: {before} -> {after}"
            );
        }
    }

    /// Microbenchmark: cost of a single counter read via `rdpmc` (in-guest,
    /// same-core, `index != 0` live case) versus the `read(2)` syscall fallback.
    ///
    /// Ignored by default because it prints timings rather than asserting on
    /// them (timing is host-dependent). Run with:
    ///   `cargo test -p reverie-ptrace --release perf::test::bench_rdpmc_vs_read \
    ///        -- --ignored --nocapture`
    ///
    /// The counter self-monitors this thread, so it stays scheduled on this
    /// core (`index != 0`) throughout — the exact case the ptrace fast path
    /// deliberately punts to the syscall. If rdpmc had silently fallen back to
    /// the syscall, the two timings would be equal; a large gap is itself proof
    /// the rdpmc path was taken.
    #[test]
    #[ignore]
    fn bench_rdpmc_vs_read() {
        use std::time::Instant;
        ret_without_perf!();
        const LOOP: usize = 100_000; // reads per timed sample
        const REPS: usize = 25; // independent timed samples
        const WARMUP: usize = 5;

        let pc = Builder::new(gettid().as_raw(), -1)
            .sample_period(PerfCounter::DISABLE_SAMPLE_PERIOD)
            .event(Event::Hardware(HardwareEvent::BranchInstructions))
            .fast_reads(true)
            .create()
            .expect("perf create");
        pc.reset().expect("perf reset");
        pc.enable().expect("perf enable");

        let time_loop = |read: &dyn Fn() -> u64| -> Vec<f64> {
            let mut samples = Vec::with_capacity(REPS);
            for rep in 0..(WARMUP + REPS) {
                let start = Instant::now();
                let mut acc = 0u64;
                for _ in 0..LOOP {
                    acc = acc.wrapping_add(std::hint::black_box(read()));
                }
                std::hint::black_box(acc);
                let ns_per_op = start.elapsed().as_nanos() as f64 / LOOP as f64;
                if rep >= WARMUP {
                    samples.push(ns_per_op);
                }
            }
            samples
        };

        let median = |mut v: Vec<f64>| -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let min = |v: &[f64]| v.iter().cloned().fold(f64::INFINITY, f64::min);

        let rdpmc_s = time_loop(&|| pc.ctr_value_rdpmc().expect("rdpmc"));
        let read_s = time_loop(&|| pc.ctr_value().expect("read"));

        let rdpmc_med = median(rdpmc_s.clone());
        let read_med = median(read_s.clone());
        eprintln!("=== rdpmc vs read() microbenchmark ===");
        eprintln!("host: self-monitoring thread, BranchInstructions, fast_reads=true");
        eprintln!("loop size (reads/sample): {LOOP}");
        eprintln!("reps (timed samples): {REPS} (+{WARMUP} warmup, discarded)");
        eprintln!(
            "rdpmc  (index!=0 live): min={:.1} ns  median={:.1} ns",
            min(&rdpmc_s),
            rdpmc_med
        );
        eprintln!(
            "read() (syscall fallback): min={:.1} ns  median={:.1} ns",
            min(&read_s),
            read_med
        );
        eprintln!(
            "gap (median read / median rdpmc): {:.1}x",
            read_med / rdpmc_med
        );
    }

    #[test]
    fn trace_other_thread() {
        ret_without_perf!();
        use std::sync::mpsc::sync_channel;
        let (tx1, rx1) = sync_channel(0); // send TID
        let (tx2, rx2) = sync_channel(0); // start guest spinn

        const ITERS: u64 = 100000;

        let handle = std::thread::spawn(move || {
            tx1.send(gettid())
                .expect("perf test operation should succeed");
            rx2.recv().expect("perf test operation should succeed");
            do_branches(ITERS);
        });

        let pc = Builder::new(
            rx1.recv()
                .expect("perf test operation should succeed")
                .as_raw(),
            -1,
        )
        .sample_period(PerfCounter::DISABLE_SAMPLE_PERIOD)
        .event(Event::Hardware(HardwareEvent::BranchInstructions))
        .create()
        .expect("perf test operation should succeed");

        pc.enable().expect("perf test operation should succeed");
        tx2.send(()).expect("perf test operation should succeed"); // tell thread to start
        handle.join().expect("perf test operation should succeed");
        let ctr = pc.ctr_value().expect("perf test operation should succeed");
        assert!(ctr >= ITERS);
        assert!(ctr <= ITERS * 2, "{}", ctr); // overhead from channel operations
    }

    #[test]
    fn deliver_signal() {
        ret_without_perf!();
        use std::mem::MaybeUninit;
        use std::sync::mpsc::sync_channel;
        let (tx1, rx1) = sync_channel(0); // send TID
        let (tx2, rx2) = sync_channel(0); // start guest spinn

        // SIGSTKFLT defaults to TERM, so if any thread but the traced one
        // receives the signal, the test will fail due to process exit.
        const MARKER_SIGNAL: Signal = Signal::SIGSTKFLT;
        const SPIN_BRANCHES: u64 = 50000; // big enough to "absorb" noise from debug/release
        const SPINS_PER_EVENT: u64 = 10;
        const SAMPLE_PERIOD: u64 = SPINS_PER_EVENT * SPIN_BRANCHES + (SPINS_PER_EVENT / 4);

        fn signal_is_pending() -> bool {
            unsafe {
                let mut mask = MaybeUninit::<libc::sigset_t>::zeroed();
                libc::sigemptyset(mask.as_mut_ptr());
                libc::sigpending(mask.as_mut_ptr());
                libc::sigismember(mask.as_ptr(), MARKER_SIGNAL as _) == 1
            }
        }

        let handle = std::thread::spawn(move || {
            unsafe {
                let mut mask = MaybeUninit::<libc::sigset_t>::zeroed();
                libc::sigemptyset(mask.as_mut_ptr());
                libc::sigaddset(mask.as_mut_ptr(), MARKER_SIGNAL as _);
                libc::sigprocmask(libc::SIG_BLOCK, mask.as_ptr(), std::ptr::null_mut());
            }

            tx1.send(gettid())
                .expect("perf test operation should succeed");
            rx2.recv().expect("perf test operation should succeed");

            let mut count = 0;
            loop {
                count += 1;
                do_branches(SPIN_BRANCHES);
                if signal_is_pending() {
                    break;
                }
            }
            assert_eq!(count, SPINS_PER_EVENT);
        });

        let tid = rx1.recv().expect("perf test operation should succeed");
        let pc = Builder::new(tid.as_raw(), -1)
            .sample_period(SAMPLE_PERIOD)
            .event(Event::Hardware(HardwareEvent::BranchInstructions))
            .create()
            .expect("perf test operation should succeed");
        pc.set_signal_delivery(tid.into(), MARKER_SIGNAL)
            .expect("perf test operation should succeed");
        pc.enable().expect("perf test operation should succeed");

        tx2.send(()).expect("perf test operation should succeed"); // tell thread to start
        handle.join().expect("perf test operation should succeed"); // propagate panics
    }
}
