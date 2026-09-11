//! Shared Linux PMU primitives for in-guest clocks and stopped-task timers.
//!
//! Enabling this module does not initialize counters or select a backend.

#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod in_guest;
#[doc(hidden)]
pub mod in_guest_timer;
#[cfg(test)]
mod logging_tests;
#[doc(hidden)]
pub mod perf;
mod validation;

pub use config::PmuConfig;
pub use config::set_pmu_config;
pub use in_guest::InGuestRcbCounter;
pub use in_guest::InGuestRcbTimer;
pub use in_guest_timer::InGuestRcbDeadline;
pub use in_guest_timer::InGuestRcbDeadlineStatus;
pub use in_guest_timer::InGuestRcbSample;
pub use perf::is_perf_supported;
