//! Narrow PMU clock primitive shared with in-guest Reverie backends.

use std::os::fd::OwnedFd;

use reverie::Errno;
use reverie::Tid;

use crate::perf::Builder;
use crate::perf::Event;
use crate::perf::PerfCounter;
use crate::timer::PmuConfig;

/// Immutable identity of an externally created, disabled RCB event.
/// It binds a transferred file to its trusted creator record, not to a task by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RcbEventDescription {
    /// Wire/API version.
    pub version: u32,
    /// Kernel PERF_EVENT_IOC_ID value.
    pub event_id: u64,
    /// The selected perf event type.
    pub event_type: u32,
    /// The exact shared PMU event configuration.
    pub config: u64,
}

/// Locally discovered native PMU type/configuration, without an event ID.
/// Capture it before enabling guest CPUID interception and carry the value
/// across fork instead of rediscovering it in a reconstructed child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RcbPmuProfile {
    event_type: u32,
    config: u64,
}

impl RcbPmuProfile {
    /// Reconstruct a profile carried by an authenticated setup channel.
    /// Only the raw event type used by this module is admitted.
    pub fn from_parts(event_type: u32, config: u64) -> Option<Self> {
        let event = Event::Raw(config);
        (event.attr_type() == event_type).then_some(Self { event_type, config })
    }

    /// Linux perf event type selected by the native profile.
    pub fn event_type(self) -> u32 {
        self.event_type
    }

    /// Raw event configuration selected by the native profile.
    pub fn config(self) -> u64 {
        self.config
    }

    /// Whether an authenticated creator description names this exact native
    /// event. The event ID remains independently authenticated by the kernel.
    pub fn matches(self, description: &RcbEventDescription) -> bool {
        description.version == 1
            && description.event_type == self.event_type
            && description.config == self.config
    }
}

/// An initially disabled counter file created by an external supervisor.
/// No metadata mapping or extra timer event is created in that supervisor.
#[derive(Debug)]
pub struct DisabledRcbEvent {
    fd: OwnedFd,
    description: RcbEventDescription,
}

impl DisabledRcbEvent {
    /// Capture the native CPU's exact shared event profile.
    ///
    /// This executes CPUID and therefore must run before a backend enables
    /// guest CPUID interception. A fork child must inherit the captured value;
    /// it must not call this while reconstructing an installed Tool.
    pub fn native_profile() -> Option<RcbPmuProfile> {
        PmuConfig::try_new().map(|config| {
            let event = config.rcb_event();
            RcbPmuProfile {
                event_type: event.attr_type(),
                config: event.attr_config(),
            }
        })
    }

    /// Whether the shared PMU table has a known event for this CPU.
    /// This does not test cross-task permissions or turn open failures into availability.
    pub fn cpu_supported() -> bool {
        Self::native_profile().is_some()
    }

    /// Create for a positive TID in the creator's PID namespace.
    /// The caller must authenticate and bracket target lifetime; perf_event_open
    /// accepts a numeric TID and this function does not make targeting atomic.
    pub fn for_thread(target: Tid) -> Result<Self, Errno> {
        if target.as_raw() <= 0 {
            return Err(Errno::EINVAL);
        }
        let config = PmuConfig::try_new().ok_or(Errno::ENODEV)?;
        let event = config.rcb_event();
        let counter = Builder::new(target.as_raw(), -1)
            .event(event)
            .sample_period(0)
            .fast_reads(false)
            .create()?;
        let description = RcbEventDescription {
            version: 1,
            event_id: counter.id()?,
            event_type: event.attr_type(),
            config: event.attr_config(),
        };
        Ok(Self {
            fd: counter.into_owned_fd(),
            description,
        })
    }

    /// Create an event for `target` only while it runs on `cpu`, using the
    /// profile captured by that already singleton-pinned target.
    ///
    /// The caller must validate the target's singleton affinity immediately
    /// before and after this operation. This function deliberately performs no
    /// CPUID discovery on the creator thread: a supervisor CPU is not authority
    /// for a target on a heterogeneous machine.
    pub fn for_thread_on_cpu(
        target: Tid,
        cpu: u32,
        profile: RcbPmuProfile,
    ) -> Result<Self, Errno> {
        if target.as_raw() <= 0 || cpu > i32::MAX as u32 {
            return Err(Errno::EINVAL);
        }
        let event = Event::Raw(profile.config());
        if event.attr_type() != profile.event_type() {
            return Err(Errno::EINVAL);
        }
        let counter = Builder::new(target.as_raw(), cpu as i32)
            .event(event)
            .sample_period(0)
            .fast_reads(false)
            .create()?;
        let description = RcbEventDescription {
            version: 1,
            event_id: counter.id()?,
            event_type: profile.event_type(),
            config: profile.config(),
        };
        Ok(Self {
            fd: counter.into_owned_fd(),
            description,
        })
    }

    /// Consume the creator's ownership for a real descriptor transfer.
    pub fn into_parts(self) -> (OwnedFd, RcbEventDescription) {
        (self.fd, self.description)
    }
}

/// A retired-conditional-branch counter owned and read by the current thread.
///
/// The enabled constructors support ordinary current-thread sampling through
/// `read()`, using RDPMC when available and the existing syscall fallback
/// otherwise. In-guest users of that mode account for handler branches
/// separately.
///
/// The disabled constructor supports trusted enable/disable boundaries that
/// exclude runtime work. `read_paused_once()` reads the cumulative count with
/// one trusted syscall while the event is disabled, without resetting it or
/// subtracting handler branches. This counter does not deliver timer signals.
#[derive(Debug)]
pub struct InGuestRcbCounter {
    counter: PerfCounter,
}

impl InGuestRcbCounter {
    /// Import an authenticated, initially disabled counter file on its target.
    ///
    /// # Safety
    /// The file must come from the trusted external creator, target this actual
    /// thread, have the advertised configuration, and never have been enabled.
    /// Before calling, the importer must match `event_type` and `config` both
    /// to a native profile captured locally before guest CPUID interception and
    /// to the authenticated creator description, then retain that binding
    /// across a fork rebind.
    /// This import deliberately executes no CPUID: a fork child may already
    /// fault guest CPUID into the Tool that this call is reconstructing. All
    /// aliases must obey exclusive runtime control for the counter lifetime.
    /// The gate must remain valid, preserve raw syscall semantics and satisfy
    /// the signal-context requirements of `read_paused_once` when used there.
    /// Event ID validation does not replace the creator/target provenance proof.
    pub unsafe fn from_disabled_owned_fd_with_syscall_gate(
        fd: OwnedFd,
        expected: &RcbEventDescription,
        gate: unsafe fn(i64, [u64; 6]) -> i64,
    ) -> Result<Self, Errno> {
        if expected.version != 1 || expected.event_id == 0 {
            return Err(Errno::EINVAL);
        }
        // Consume through a gate-aware owner even on an invalid description.
        let counter = unsafe { PerfCounter::import_disabled(fd, expected.event_id, gate) }?;
        Ok(Self { counter })
    }

    /// Create an initially disabled clock using the ordinary shared PMU builder.
    /// Enable it only at an accounted guest boundary; never reset it thereafter.
    /// Existing enabled constructors and ordinary reads are unaffected.
    ///
    /// # Safety
    ///
    /// The gate must preserve raw Linux syscall semantics and remain callable
    /// for the counter's lifetime. Boundary controls and reads must run on the
    /// owning thread with exclusive control of the event.
    /// Asynchronous reads additionally require a gate that does not allocate,
    /// lock, initialize TLS, panic or retry, and is safe in that signal context.
    pub unsafe fn current_thread_disabled_with_syscall_gate(
        raw_syscall: unsafe fn(i64, [u64; 6]) -> i64,
    ) -> Result<Self, Errno> {
        Self::create_with_config(PmuConfig::try_new(), Some(raw_syscall))
    }

    /// Borrow this clock's descriptor for trusted assembly boundary controls.
    ///
    /// # Safety
    ///
    /// Use only on the owning thread. The caller must protect the descriptor
    /// from guest access, never close, duplicate, reset or reconfigure it, and
    /// exclusively coordinate disable/read/enable with every guest transition.
    /// Keeping the descriptor does not make a Rust control path branch-free.
    pub unsafe fn boundary_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        unsafe { std::os::fd::BorrowedFd::borrow_raw(self.counter.raw_fd()) }
    }

    /// Read a disabled clock once through its trusted gate, without retrying,
    /// allocating, panicking or changing its cumulative value in this wrapper.
    /// The supplied gate must independently satisfy these requirements.
    ///
    /// Returns EBUSY for a scheduled event, ENODEV for lost PMU availability,
    /// EAGAIN for changing metadata, EIO for a short read, or the syscall error.
    /// A missing mapping or trusted gate returns EOPNOTSUPP.
    ///
    /// # Safety
    ///
    /// The owning thread must have successfully disabled this event and must
    /// exclude concurrent or reentrant controls until this read completes.
    pub unsafe fn read_paused_once(&self) -> Result<u64, Errno> {
        self.counter.ctr_value_paused_once()
    }

    /// Create and enable an RCB clock for the calling thread.
    pub fn current_thread() -> Result<Self, Errno> {
        Self::current_thread_with_optional_syscall_gate(None)
    }

    /// Create the same current-thread RCB clock through a caller-supplied raw
    /// syscall gate. In-guest backends use this after installing seccomp so the
    /// counter's perf-event, mmap, and ioctl setup cannot recursively enter the
    /// Tool that is currently rebuilding fork-child state.
    ///
    /// # Safety
    ///
    /// The gate must preserve Linux x86-64 syscall argument/result semantics
    /// and remain callable for the lifetime of the returned counter.
    pub unsafe fn current_thread_with_syscall_gate(
        raw_syscall: unsafe fn(i64, [u64; 6]) -> i64,
    ) -> Result<Self, Errno> {
        Self::current_thread_with_optional_syscall_gate(Some(raw_syscall))
    }

    fn current_thread_with_optional_syscall_gate(
        raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
    ) -> Result<Self, Errno> {
        Self::current_thread_with_config(PmuConfig::try_new(), raw_syscall)
    }

    fn current_thread_with_config(
        config: Option<PmuConfig>,
        raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
    ) -> Result<Self, Errno> {
        let clock = Self::create_with_config(config, raw_syscall)?;
        clock.counter.reset()?;
        clock.counter.enable()?;
        Ok(clock)
    }

    fn create_with_config(
        config: Option<PmuConfig>,
        raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
    ) -> Result<Self, Errno> {
        let config = config.ok_or(Errno::ENODEV)?;
        let mut builder = Builder::new(0, -1);
        builder
            .sample_period(0)
            .event(config.rcb_event())
            .fast_reads(true);
        let counter = if let Some(raw_syscall) = raw_syscall {
            builder.create_with_raw_syscall(raw_syscall)?
        } else {
            builder.create()?
        };
        Ok(Self { counter })
    }

    /// Read the calling thread's current RCB count without a syscall whenever
    /// the kernel exposes the live PMU counter to user space.
    #[inline(always)]
    pub fn read(&self) -> Result<u64, Errno> {
        self.counter.ctr_value_rdpmc()
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "in_guest/tests.rs"]
mod boundary_tests;

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn model_cf_is_refused_before_perf_event_open() {
        let config = PmuConfig::try_from_family_model(0x06, 0xcf);
        let error = InGuestRcbCounter::current_thread_with_config(config, None).unwrap_err();
        assert_eq!(error, Errno::ENODEV);
    }
}
