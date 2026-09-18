//! Probe identity and lifecycle state.

use core::fmt;

/// Stable identity assigned to a registered probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProbeId(u64);

impl ProbeId {
    /// Creates a probe identity from its numeric representation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Published lifecycle state of a probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeState {
    /// The probe is planned but has not been published into executable code.
    Planned,
    /// The probe is installed and currently bypasses its instrumentation.
    InstalledInactive,
    /// The installed probe currently routes through its instrumentation.
    Active,
}

impl ProbeState {
    /// Moves to `next` when that transition is valid.
    ///
    /// Repeating the current state is intentionally idempotent. This prevents
    /// duplicate deactivate requests from corrupting shared activation counts.
    pub fn transition(&mut self, next: Self) -> Result<(), InvalidProbeTransition> {
        let allowed = *self == next
            || matches!(
                (*self, next),
                (Self::Planned, Self::InstalledInactive)
                    | (Self::InstalledInactive, Self::Active)
                    | (Self::Active, Self::InstalledInactive)
            );

        if allowed {
            *self = next;
            Ok(())
        } else {
            Err(InvalidProbeTransition {
                from: *self,
                to: next,
            })
        }
    }
}

/// Error returned for a probe lifecycle transition that cannot be published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProbeTransition {
    from: ProbeState,
    to: ProbeState,
}

impl InvalidProbeTransition {
    /// Returns the current state.
    pub const fn from(self) -> ProbeState {
        self.from
    }

    /// Returns the rejected destination state.
    pub const fn to(self) -> ProbeState {
        self.to
    }
}

impl fmt::Display for InvalidProbeTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid probe transition from {:?} to {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidProbeTransition {}

#[cfg(test)]
mod tests {
    use super::ProbeState;

    #[test]
    fn installed_probe_can_toggle_repeatedly() {
        let mut state = ProbeState::Planned;
        state.transition(ProbeState::InstalledInactive).unwrap();
        state.transition(ProbeState::Active).unwrap();
        state.transition(ProbeState::InstalledInactive).unwrap();
        state.transition(ProbeState::InstalledInactive).unwrap();
        assert_eq!(state, ProbeState::InstalledInactive);
    }

    #[test]
    fn planned_probe_cannot_be_activated_before_installation() {
        let mut state = ProbeState::Planned;
        let error = state.transition(ProbeState::Active).unwrap_err();
        assert_eq!(error.from(), ProbeState::Planned);
        assert_eq!(error.to(), ProbeState::Active);
        assert_eq!(state, ProbeState::Planned);
    }
}
