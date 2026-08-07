//! Narrow PMU clock primitive shared with in-guest Reverie backends.

use reverie::Errno;

use crate::perf::Builder;
use crate::perf::PerfCounter;
use crate::timer::PmuConfig;

/// A retired-conditional-branch counter owned and read by the current thread.
///
/// Unlike the ptrace timer, this counter never delivers a signal and never
/// reads another thread.  The same thread that owns the PMU event reads it via
/// `rdpmc`, which is the binding required by [`PerfCounter::ctr_value_rdpmc`].
/// In-guest backends use it to sample the guest clock at an instrumentation
/// trampoline boundary and then deduct branches retired by their own handler.
#[derive(Debug)]
pub struct InGuestRcbCounter {
    counter: PerfCounter,
}

impl InGuestRcbCounter {
    /// Create and enable an RCB clock for the calling thread.
    pub fn current_thread() -> Result<Self, Errno> {
        let mut builder = Builder::new(0, -1);
        builder
            .sample_period(0)
            .event(PmuConfig::new().rcb_event())
            .fast_reads(true);
        let counter = builder.create()?;
        counter.reset()?;
        counter.enable()?;
        Ok(Self { counter })
    }

    /// Read the calling thread's current RCB count without a syscall whenever
    /// the kernel exposes the live PMU counter to user space.
    #[inline(always)]
    pub fn read(&self) -> Result<u64, Errno> {
        self.counter.ctr_value_rdpmc()
    }
}
