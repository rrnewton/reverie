//! Pure exact RCB-crossing and instruction-suffix progression.
//!
//! Observations must come from independently authenticated, one-instruction
//! guest completions. This controller neither executes instructions nor treats
//! notification count equality as evidence of a crossed instruction boundary.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Generation(u64);

impl Generation {
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub generation: Generation,
    pub sequence: u64,
    pub clock: u64,
    pub rip: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Step,
    Deliver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Overflow,
    Inactive,
    Stale,
    Sequence,
    Backwards,
    Overshot,
    Position,
}

#[derive(Clone, Copy, Debug)]
struct Request {
    generation: Generation,
    target: u64,
    suffix: u64,
    progress: Option<u64>,
    sequence: u64,
    clock: u64,
}

#[derive(Debug, Default)]
pub struct Controller {
    generation: u64,
    request: Option<Request>,
}

impl Controller {
    /// Replace the old request, even if validation of its replacement fails.
    /// Zero RCB/suffix is anchored at this authenticated arming continuation;
    /// its Deliver decision does not represent a fabricated completed step.
    pub fn replace(
        &mut self,
        clock: u64,
        rcbs: u64,
        suffix: u64,
    ) -> Result<(Generation, Decision), Error> {
        self.request = None;
        self.generation = self.generation.checked_add(1).ok_or(Error::Overflow)?;
        let generation = Generation(self.generation);
        let target = clock.checked_add(rcbs).ok_or(Error::Overflow)?;
        if rcbs == 0 && suffix == 0 {
            return Ok((generation, Decision::Deliver));
        }
        self.request = Some(Request {
            generation,
            target,
            suffix,
            progress: (rcbs == 0).then_some(0),
            sequence: 0,
            clock,
        });
        Ok((generation, Decision::Step))
    }

    /// A stale cancellation must not erase a newer request.
    pub fn cancel(&mut self, generation: Generation) -> Result<(), Error> {
        let request = self.request.as_ref().ok_or(Error::Inactive)?;
        if request.generation != generation {
            return Err(Error::Stale);
        }
        self.request = None;
        Ok(())
    }

    /// Consume a validated observation once. Errors do not modify progress.
    /// Delivery consumes the request before a caller can invoke a Tool callback.
    pub fn observe(&mut self, observation: Observation) -> Result<Decision, Error> {
        let mut request = *self.request.as_ref().ok_or(Error::Inactive)?;
        if observation.generation != request.generation {
            return Err(Error::Stale);
        }
        if observation.sequence != request.sequence.checked_add(1).ok_or(Error::Overflow)? {
            return Err(Error::Sequence);
        }
        if observation.rip == 0 || observation.rip >= 1 << 47 {
            return Err(Error::Position);
        }
        if observation.clock < request.clock {
            return Err(Error::Backwards);
        }
        request.progress = match request.progress {
            Some(progress) => Some(progress.checked_add(1).ok_or(Error::Overflow)?),
            None if observation.clock > request.target => return Err(Error::Overshot),
            None if observation.clock == request.target => Some(0),
            None => None,
        };
        request.sequence = observation.sequence;
        request.clock = observation.clock;
        if request.progress == Some(request.suffix) {
            self.request = None;
            Ok(Decision::Deliver)
        } else {
            self.request = Some(request);
            Ok(Decision::Step)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(generation: Generation, sequence: u64, clock: u64, rip: u64) -> Observation {
        Observation {
            generation,
            sequence,
            clock,
            rip,
        }
    }

    #[test]
    fn suffix_counts_instructions_including_later_branches() {
        let mut controller = Controller::default();
        let (generation, decision) = controller.replace(40, 2, 3).unwrap();
        assert_eq!(decision, Decision::Step);
        for (index, clock) in [40, 41, 41, 42, 42, 43, 43].into_iter().enumerate() {
            let result = controller.observe(observation(
                generation,
                index as u64 + 1,
                clock,
                0x4000 + index as u64,
            ));
            assert_eq!(
                result,
                Ok(if index == 6 {
                    Decision::Deliver
                } else {
                    Decision::Step
                })
            );
        }
        assert_eq!(
            controller.observe(observation(generation, 8, 43, 0x4010)),
            Err(Error::Inactive)
        );
    }

    #[test]
    fn identical_rip_and_count_still_require_each_occurrence() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 0, 3).unwrap();
        for sequence in 1..=3 {
            assert_eq!(
                controller.observe(observation(generation, sequence, 40, 0x4000)),
                Ok(if sequence == 3 {
                    Decision::Deliver
                } else {
                    Decision::Step
                })
            );
        }
    }

    #[test]
    fn equal_count_at_different_pcs_does_not_skip_suffix() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(5, 1, 1).unwrap();
        assert_eq!(
            controller.observe(observation(generation, 1, 6, 0x4002)),
            Ok(Decision::Step)
        );
        assert_eq!(
            controller.observe(observation(generation, 1, 6, 0x4003)),
            Err(Error::Sequence)
        );
        assert_eq!(
            controller.observe(observation(generation, 2, 6, 0x4003)),
            Ok(Decision::Deliver)
        );
    }

    #[test]
    fn replacement_and_cancellation_do_not_consume_new_generation() {
        let mut controller = Controller::default();
        let (old, _) = controller.replace(10, 1, 0).unwrap();
        let (new, _) = controller.replace(10, 2, 0).unwrap();
        assert_eq!(controller.cancel(old), Err(Error::Stale));
        assert_eq!(
            controller.observe(observation(old, 1, 11, 0x4000)),
            Err(Error::Stale)
        );
        controller.cancel(new).unwrap();
        assert_eq!(
            controller.observe(observation(new, 1, 12, 0x4000)),
            Err(Error::Inactive)
        );
        let (next, _) = controller.replace(12, 0, 1).unwrap();
        assert_eq!(
            controller.observe(observation(next, 1, 12, 0x4000)),
            Ok(Decision::Deliver)
        );
    }

    #[test]
    fn overshoot_before_crossing_and_backwards_after_crossing_refuse() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 2, 3).unwrap();
        assert_eq!(
            controller.observe(observation(generation, 1, 43, 0x4000)),
            Err(Error::Overshot)
        );
        assert_eq!(
            controller.observe(observation(generation, 1, 42, 0x4000)),
            Ok(Decision::Step)
        );
        assert_eq!(
            controller.observe(observation(generation, 2, 43, 0x4000)),
            Ok(Decision::Step)
        );
        assert_eq!(
            controller.observe(observation(generation, 3, 42, 0x4000)),
            Err(Error::Backwards)
        );
    }

    #[test]
    fn zero_anchor_and_overflow_do_not_invent_a_step() {
        let mut controller = Controller::default();
        let (generation, decision) = controller.replace(40, 0, 0).unwrap();
        assert_eq!(decision, Decision::Deliver);
        assert_eq!(
            controller.observe(observation(generation, 1, 40, 0x4000)),
            Err(Error::Inactive)
        );
        controller.replace(40, 1, 0).unwrap();
        assert_eq!(controller.replace(u64::MAX, 1, 0), Err(Error::Overflow));
        assert!(controller.request.is_none());
        controller.generation = u64::MAX;
        assert_eq!(controller.replace(0, 1, 0), Err(Error::Overflow));
    }
}
