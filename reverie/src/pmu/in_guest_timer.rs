//! Logical RCB deadlines, independent of PMU notification and guest delivery.

/// A single boundary sample: no retry loop, syscall fallback, or clock repair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InGuestRcbSample {
    /// Unscaled, full-width cumulative RCB count.
    Value(u64),
    /// No usable user-space mapping, RDPMC capability, or counter width.
    Unavailable,
    /// Counter is not currently scheduled or has lost counting time.
    Descheduled,
    /// Metadata writer was active or changed during this single attempt.
    Retry,
}

pub(crate) fn sampled_count(offset: i64, raw: u64, width: u16) -> InGuestRcbSample {
    if !(1..=64).contains(&width) {
        return InGuestRcbSample::Unavailable;
    }
    let shift = 64 - u32::from(width);
    let signed = ((raw << shift) as i64) >> shift;
    InGuestRcbSample::Value((offset as u64).wrapping_add(signed as u64))
}

/// The position of an accounted guest clock relative to a one-shot deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InGuestRcbDeadlineStatus {
    /// Number of guest RCBs still required to reach the unchanged target.
    Remaining(u64),
    /// The sampled guest clock equals the target exactly.
    Reached,
    /// Guest RCBs past the target; never a precise-delivery success.
    Overshot(u64),
    /// No active request remains.
    Cancelled,
}

/// An absolute deadline in the caller's continuous, guest-only RCB clock.
///
/// This helper neither reads nor modifies that clock. Cancelling the logical
/// deadline does not disable a PMU event or drain an already pending signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InGuestRcbDeadline {
    target: Option<u64>,
}

impl InGuestRcbDeadline {
    /// Reject a zero interval or an absolute target that would overflow.
    pub fn new(clock: u64, after: u64) -> Option<Self> {
        if after == 0 {
            return None;
        }
        Some(Self {
            target: Some(clock.checked_add(after)?),
        })
    }

    /// The absolute target, or None after cancellation.
    pub fn target(&self) -> Option<u64> {
        self.target
    }

    /// Cancel the logical request without changing the caller's clock.
    pub fn cancel(&mut self) {
        self.target = None;
    }

    /// Report overshoot explicitly; it is not successful precise delivery.
    pub fn status(&self, clock: u64) -> InGuestRcbDeadlineStatus {
        match self.target {
            None => InGuestRcbDeadlineStatus::Cancelled,
            Some(target) => match clock.cmp(&target) {
                core::cmp::Ordering::Less => InGuestRcbDeadlineStatus::Remaining(target - clock),
                core::cmp::Ordering::Equal => InGuestRcbDeadlineStatus::Reached,
                core::cmp::Ordering::Greater => InGuestRcbDeadlineStatus::Overshot(clock - target),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_width_and_signed_offset_preserve_counter_arithmetic() {
        assert_eq!(sampled_count(0, 0, 0), InGuestRcbSample::Unavailable);
        assert_eq!(sampled_count(0, 0, 65), InGuestRcbSample::Unavailable);
        assert_eq!(sampled_count(10, 255, 8), InGuestRcbSample::Value(9));
        assert_eq!(sampled_count(10, u64::MAX, 64), InGuestRcbSample::Value(9));
        assert_eq!(sampled_count(-1, 2, 64), InGuestRcbSample::Value(1));
        for count in 0..100 {
            assert_eq!(
                sampled_count(100, count, 48),
                InGuestRcbSample::Value(100 + count)
            );
        }
    }

    #[test]
    fn rejects_zero_interval_and_overflow() {
        assert_eq!(InGuestRcbDeadline::new(0, 0), None);
        assert_eq!(InGuestRcbDeadline::new(u64::MAX, 1), None);
        assert_eq!(InGuestRcbDeadline::new(1, u64::MAX), None);
    }

    #[test]
    fn preserves_every_rcb_and_distinguishes_overshoot() {
        let deadline = InGuestRcbDeadline::new(123, 3).unwrap();
        assert_eq!(deadline.target(), Some(126));
        assert_eq!(deadline.status(123), InGuestRcbDeadlineStatus::Remaining(3));
        assert_eq!(deadline.status(124), InGuestRcbDeadlineStatus::Remaining(2));
        assert_eq!(deadline.status(125), InGuestRcbDeadlineStatus::Remaining(1));
        assert_eq!(deadline.status(126), InGuestRcbDeadlineStatus::Reached);
        assert_eq!(deadline.status(127), InGuestRcbDeadlineStatus::Overshot(1));
        assert_eq!(deadline.status(128), InGuestRcbDeadlineStatus::Overshot(2));
    }

    #[test]
    fn cancellation_is_idempotent_and_never_becomes_ready() {
        let mut deadline = InGuestRcbDeadline::new(10, 5).unwrap();
        deadline.cancel();
        deadline.cancel();
        assert_eq!(deadline.target(), None);
        for clock in [0, 10, 14, 15, 16, u64::MAX] {
            assert_eq!(deadline.status(clock), InGuestRcbDeadlineStatus::Cancelled);
        }
    }

    #[test]
    fn replacement_uses_the_current_clock_without_resetting_it() {
        let mut deadline = InGuestRcbDeadline::new(10, 5).unwrap();
        assert_eq!(deadline.target(), Some(15));
        deadline = InGuestRcbDeadline::new(13, 7).unwrap();
        assert_eq!(deadline.target(), Some(20));
        assert_eq!(deadline.status(15), InGuestRcbDeadlineStatus::Remaining(5));
    }

    #[test]
    fn supports_the_last_representable_clock_tick() {
        let deadline = InGuestRcbDeadline::new(u64::MAX - 1, 1).unwrap();
        assert_eq!(deadline.target(), Some(u64::MAX));
        assert_eq!(deadline.status(u64::MAX), InGuestRcbDeadlineStatus::Reached);
    }
}
