use std::cell::Cell;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::io;

use reverie::Errno;
use reverie::Error;
use reverie::TimerSchedule;
use reverie::pmu::InGuestRcbTimer;
use reverie_preload::precise_timer::Controller;
use reverie_preload::precise_timer::Decision;
use reverie_preload::precise_timer::Generation;
use reverie_preload::precise_timer::Observation;
use reverie_preload::trap::raw_syscall6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnedTimerPosition {
    pub generation: u64,
    pub sequence: u64,
    pub rip: u64,
    pub clock: u64,
    pub target: u64,
    pub suffix: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ticket {
    pub(crate) generation: Generation,
    pub(crate) sequence: u64,
}

#[derive(Debug)]
struct OwnedRequest {
    ticket: Ticket,
    target: u64,
    suffix: u64,
    immediate: bool,
    clock: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ModeledInterruption {
    owner: i64,
    sequence: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum InterruptionResolution {
    Preserved(Option<OwnedTimerPosition>),
    Replaced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeferredRequest {
    clock: u64,
    target: u64,
    rcbs: u64,
    suffix: u64,
}

struct InterruptedTimer {
    identity: ModeledInterruption,
    pc: u64,
    clock: u64,
    replacement: Option<DeferredRequest>,
    failed: bool,
}

struct OwnedTimer {
    owner: i64,
    controller: Controller,
    request: Option<OwnedRequest>,
    window: Option<(u64, u64)>,
    delivered: Option<OwnedTimerPosition>,
    interruption_sequence: u64,
    interrupted: Option<InterruptedTimer>,
}

thread_local! {
    static OWNED: RefCell<Option<OwnedTimer>> = const { RefCell::new(None) };
}

pub(crate) fn initialize_owned() -> io::Result<()> {
    if !crate::clock_control::active()
        || !crate::clock_control::paused()
        || !crate::clock_control::notification_free()
    {
        return Err(io::Error::other(
            "precise controller needs an owned paused counter",
        ));
    }
    OWNED.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(io::Error::other("precise controller already installed"));
        }
        *slot = Some(OwnedTimer {
            owner: unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) },
            controller: Controller::default(),
            request: None,
            window: None,
            delivered: None,
            interruption_sequence: 0,
            interrupted: None,
        });
        Ok(())
    })
}

fn with_owned<Output>(
    operation: impl FnOnce(&mut OwnedTimer) -> Result<Output, Errno>,
) -> Result<Output, Errno> {
    OWNED.with(|slot| {
        let mut slot = slot.try_borrow_mut().map_err(|_| Errno::EBUSY)?;
        let state = slot.as_mut().ok_or(Errno::EOPNOTSUPP)?;
        if unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } != state.owner {
            return Err(Errno::EXDEV);
        }
        if !crate::clock_control::paused() {
            return Err(Errno::EBUSY);
        }
        operation(state)
    })
}

pub(crate) fn open_window(pc: u64, clock: u64) -> Result<(), Errno> {
    with_owned(|state| state.open_window(pc, clock))
}

pub(crate) fn begin_modeled_interruption(
    ticket: Option<Ticket>,
    pc: u64,
    clock: u64,
) -> Result<ModeledInterruption, Errno> {
    with_owned(|state| state.begin_interruption(ticket, pc, clock))
}

pub(crate) fn fail_modeled_interruption(identity: &ModeledInterruption) -> Result<(), Errno> {
    with_owned(|state| state.fail_interruption(identity))
}

pub(crate) fn resolve_modeled_interruption(
    identity: &ModeledInterruption,
    completed: Option<Observation>,
) -> Result<InterruptionResolution, Errno> {
    with_owned(|state| state.resolve_interruption(identity, completed))
}

impl OwnedTimer {
    fn open_window(&mut self, pc: u64, clock: u64) -> Result<(), Errno> {
        let state = self;
        if state.window.is_some() {
            return Err(Errno::EBUSY);
        }
        if let Some(interrupted) = &state.interrupted {
            if interrupted.failed {
                return Err(Errno::ECANCELED);
            }
            if (pc, clock) != (interrupted.pc, interrupted.clock) {
                return Err(Errno::EINVAL);
            }
        }
        state.window = Some((pc, clock));
        Ok(())
    }
}

pub(crate) fn close_window() -> Result<(), Errno> {
    with_owned(|state| {
        state.window = None;
        state.delivered = None;
        Ok(())
    })
}

pub(crate) fn cancel_owned() -> Result<(), Errno> {
    with_owned(OwnedTimer::cancel)
}

impl OwnedTimer {
    fn uninterrupted(&self) -> Result<(), Errno> {
        if self.interrupted.is_some() {
            return Err(Errno::EBUSY);
        }
        Ok(())
    }

    fn begin_interruption(
        &mut self,
        ticket: Option<Ticket>,
        pc: u64,
        clock: u64,
    ) -> Result<ModeledInterruption, Errno> {
        self.uninterrupted()?;
        if self.window.is_some() || self.delivered.is_some() {
            return Err(Errno::EBUSY);
        }
        if ticket != self.next_ticket()?
            || self
                .request
                .as_ref()
                .is_some_and(|request| clock < request.clock)
        {
            return Err(Errno::EINVAL);
        }
        let sequence = self
            .interruption_sequence
            .checked_add(1)
            .ok_or(Errno::EOVERFLOW)?;
        self.interrupted = Some(InterruptedTimer {
            identity: ModeledInterruption {
                owner: self.owner,
                sequence,
            },
            pc,
            clock,
            replacement: None,
            failed: false,
        });
        self.interruption_sequence = sequence;
        Ok(ModeledInterruption {
            owner: self.owner,
            sequence,
        })
    }

    fn fail_interruption(&mut self, identity: &ModeledInterruption) -> Result<(), Errno> {
        let interrupted = self.interrupted.as_mut().ok_or(Errno::EINVAL)?;
        if interrupted.identity != *identity {
            return Err(Errno::EINVAL);
        }
        interrupted.failed = true;
        Ok(())
    }

    fn resolve_interruption(
        &mut self,
        identity: &ModeledInterruption,
        completed: Option<Observation>,
    ) -> Result<InterruptionResolution, Errno> {
        let interrupted = self.interrupted.as_ref().ok_or(Errno::EINVAL)?;
        if interrupted.identity != *identity {
            return Err(Errno::EINVAL);
        }
        if interrupted.failed {
            return Err(Errno::ECANCELED);
        }
        let clock = interrupted.clock;
        let replacement = interrupted.replacement;
        let result = (|| {
            if self.window.is_some() {
                return Err(Errno::EBUSY);
            }
            match (&self.request, completed) {
                (None, None) => (),
                (Some(request), Some(observation))
                    if observation.generation == request.ticket.generation
                        && Some(observation.sequence) == request.ticket.sequence.checked_add(1)
                        && observation.clock == clock =>
                {
                    ()
                }
                _ => return Err(Errno::EINVAL),
            }
            if let Some(replacement) = replacement {
                let (generation, decision) = self
                    .controller
                    .replace(replacement.clock, replacement.rcbs, replacement.suffix)
                    .map_err(|_| Errno::EOVERFLOW)?;
                self.request = Some(OwnedRequest {
                    ticket: Ticket {
                        generation,
                        sequence: 0,
                    },
                    target: replacement.target,
                    suffix: replacement.suffix,
                    immediate: decision == Decision::Deliver,
                    clock: replacement.clock,
                });
                Ok(InterruptionResolution::Replaced)
            } else {
                completed
                    .map(|observation| self.advance_current(observation))
                    .transpose()
                    .map(|delivery| InterruptionResolution::Preserved(delivery.flatten()))
            }
        })();
        match result {
            Ok(resolution) => {
                self.interrupted = None;
                Ok(resolution)
            }
            Err(error) => {
                self.interrupted.as_mut().ok_or(Errno::EINVAL)?.failed = true;
                Err(error)
            }
        }
    }

    fn cancel(&mut self) -> Result<(), Errno> {
        self.uninterrupted()?;
        if let Some(request) = self.request.take()
            && !request.immediate
        {
            self.controller
                .cancel(request.ticket.generation)
                .map_err(|_| Errno::EINVAL)?;
        }
        Ok(())
    }

    fn stage(&mut self, schedule: TimerSchedule, precise: bool) -> Result<(), Errno> {
        if let Some(interrupted) = &mut self.interrupted {
            if interrupted.failed {
                return Err(Errno::ECANCELED);
            }
            let replacement = (|| {
                let (_, clock) = self.window.ok_or(Errno::EOPNOTSUPP)?;
                if !precise {
                    return Err(Errno::EOPNOTSUPP);
                }
                let (rcbs, suffix) = match schedule {
                    TimerSchedule::Rcbs(rcbs) => (rcbs, 0),
                    TimerSchedule::RcbsAndInstructions(rcbs, suffix) => (rcbs, suffix),
                    TimerSchedule::Time(_) => return Err(Errno::EOPNOTSUPP),
                };
                Ok(DeferredRequest {
                    clock,
                    target: clock.checked_add(rcbs).ok_or(Errno::EOVERFLOW)?,
                    rcbs,
                    suffix,
                })
            })();
            match replacement {
                Ok(replacement) => interrupted.replacement = Some(replacement),
                Err(error) => {
                    interrupted.failed = true;
                    return Err(error);
                }
            }
            return Ok(());
        }
        let (_, clock) = self.window.ok_or(Errno::EOPNOTSUPP)?;
        self.cancel()?;
        if !precise {
            return Err(Errno::EOPNOTSUPP);
        }
        let (rcbs, suffix) = match schedule {
            TimerSchedule::Rcbs(rcbs) => (rcbs, 0),
            TimerSchedule::RcbsAndInstructions(rcbs, suffix) => (rcbs, suffix),
            TimerSchedule::Time(_) => return Err(Errno::EOPNOTSUPP),
        };
        let target = clock.checked_add(rcbs).ok_or(Errno::EOVERFLOW)?;
        let (generation, decision) = self
            .controller
            .replace(clock, rcbs, suffix)
            .map_err(|_| Errno::EOVERFLOW)?;
        self.request = Some(OwnedRequest {
            ticket: Ticket {
                generation,
                sequence: 0,
            },
            target,
            suffix,
            immediate: decision == Decision::Deliver,
            clock,
        });
        Ok(())
    }
}

pub(crate) fn request_owned(schedule: TimerSchedule, precise: bool) -> Result<(), Error> {
    with_owned(|state| state.stage(schedule, precise)).map_err(Error::from)
}

pub(crate) fn next_step() -> Result<Option<Ticket>, Errno> {
    with_owned(OwnedTimer::next_ticket)
}

impl OwnedTimer {
    fn next_ticket(&mut self) -> Result<Option<Ticket>, Errno> {
        self.uninterrupted()?;
        self.request
            .as_ref()
            .map(|request| {
                if request.immediate {
                    return Err(Errno::EINVAL);
                }
                Ok(Ticket {
                    generation: request.ticket.generation,
                    sequence: request
                        .ticket
                        .sequence
                        .checked_add(1)
                        .ok_or(Errno::EOVERFLOW)?,
                })
            })
            .transpose()
    }
}

pub(crate) fn advance(observation: Observation) -> Result<Option<OwnedTimerPosition>, Errno> {
    with_owned(|state| state.advance(observation))
}

impl OwnedTimer {
    fn advance(&mut self, observation: Observation) -> Result<Option<OwnedTimerPosition>, Errno> {
        self.uninterrupted()?;
        self.advance_current(observation)
    }

    fn advance_current(
        &mut self,
        observation: Observation,
    ) -> Result<Option<OwnedTimerPosition>, Errno> {
        let request = self.request.as_mut().ok_or(Errno::EINVAL)?;
        let decision = self
            .controller
            .observe(observation)
            .map_err(|_| Errno::EINVAL)?;
        request.ticket.sequence = observation.sequence;
        request.clock = observation.clock;
        if decision == Decision::Step {
            return Ok(None);
        }
        let position = OwnedTimerPosition {
            generation: observation.generation.value(),
            sequence: observation.sequence,
            rip: observation.rip,
            clock: observation.clock,
            target: request.target,
            suffix: request.suffix,
        };
        self.request = None;
        Ok(Some(position))
    }
}

pub(crate) fn immediate(pc: u64, clock: u64) -> Result<Option<OwnedTimerPosition>, Errno> {
    with_owned(|state| state.immediate(pc, clock))
}

impl OwnedTimer {
    fn immediate(&mut self, pc: u64, clock: u64) -> Result<Option<OwnedTimerPosition>, Errno> {
        self.uninterrupted()?;
        let Some(request) = self.request.as_ref().filter(|request| request.immediate) else {
            return Ok(None);
        };
        if clock != request.target {
            return Err(Errno::EINVAL);
        }
        let position = OwnedTimerPosition {
            generation: request.ticket.generation.value(),
            sequence: 0,
            rip: pc,
            clock,
            target: request.target,
            suffix: request.suffix,
        };
        self.request = None;
        Ok(Some(position))
    }
}

pub(crate) fn publish_delivery(position: OwnedTimerPosition) -> Result<(), Errno> {
    with_owned(|state| {
        state.uninterrupted()?;
        state.delivered = Some(position);
        Ok(())
    })
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub fn __owned_timer_position() -> Option<OwnedTimerPosition> {
    with_owned(|state| Ok(state.delivered)).ok().flatten()
}

/// Whether this thread actually holds an installed precise owned controller.
/// Host-test observation only; absent from every shipped build.
#[cfg(test)]
pub(crate) fn owned_controller_installed() -> bool {
    OWNED.with(|slot| slot.borrow().is_some())
}

#[cfg(test)]
pub(crate) mod owned_tests {
    use super::*;

    pub(crate) fn with_paused_model<Output>(operation: impl FnOnce() -> Output) -> Output {
        struct Restore([u64; 4]);
        impl Drop for Restore {
            fn drop(&mut self) {
                OWNED.with(|slot| {
                    slot.borrow_mut().take();
                });
                crate::clock_control::test_control::restore(self.0);
            }
        }
        OWNED.with(|slot| assert!(slot.borrow().is_none()));
        let _restore = Restore(crate::clock_control::test_control::snapshot());
        crate::clock_control::test_control::set_ready(1);
        crate::clock_control::test_control::set_running(0);
        initialize_owned().unwrap();
        operation()
    }

    fn state() -> OwnedTimer {
        OwnedTimer {
            owner: 1,
            controller: Controller::default(),
            request: None,
            window: None,
            delivered: None,
            interruption_sequence: 0,
            interrupted: None,
        }
    }

    #[test]
    fn default_and_unsupported_return_windows_never_stage() {
        assert!(request_owned(TimerSchedule::Rcbs(1), true).is_err());
        let mut state = state();
        assert_eq!(
            state.stage(TimerSchedule::Rcbs(1), true),
            Err(Errno::EOPNOTSUPP)
        );
        assert!(state.request.is_none());
    }

    #[test]
    fn closed_callback_preserves_request_and_replacement_rejects_old_generation() {
        let mut state = state();
        state.window = Some((0x4000, 40));
        state
            .stage(TimerSchedule::RcbsAndInstructions(2, 3), true)
            .unwrap();
        let old = state.request.as_ref().unwrap().ticket.generation;
        state.window = None;
        assert!(state.request.is_some());
        state.cancel().unwrap();
        state.window = Some((0x5000, 41));
        state
            .stage(TimerSchedule::RcbsAndInstructions(0, 1), true)
            .unwrap();
        let current = state.request.as_ref().unwrap();
        assert_ne!(old, current.ticket.generation);
        assert_eq!(
            (current.target, current.suffix, current.ticket.sequence),
            (41, 1, 0)
        );
        assert!(
            state
                .controller
                .observe(Observation {
                    generation: old,
                    sequence: 1,
                    clock: 41,
                    rip: 0x5001
                })
                .is_err()
        );
        state.cancel().unwrap();
        assert!(state.request.is_none());
    }

    #[test]
    fn unsupported_or_overflow_replacement_cancels_without_success() {
        let mut state = state();
        state.window = Some((0x4000, 40));
        state.stage(TimerSchedule::Rcbs(1), true).unwrap();
        assert_eq!(
            state.stage(TimerSchedule::Rcbs(2), false),
            Err(Errno::EOPNOTSUPP)
        );
        assert!(state.request.is_none());
        state.stage(TimerSchedule::Rcbs(1), true).unwrap();
        assert_eq!(
            state.stage(TimerSchedule::Rcbs(u64::MAX), true),
            Err(Errno::EOVERFLOW)
        );
        assert!(state.request.is_none());
    }

    fn armed() -> OwnedTimer {
        let mut state = state();
        state.open_window(0x4000, 40).unwrap();
        state
            .stage(TimerSchedule::RcbsAndInstructions(2, 3), true)
            .unwrap();
        state.window = None;
        state
    }

    fn retained(state: &OwnedTimer) -> String {
        format!("{:?} {:?}", state.controller, state.request)
    }

    #[test]
    fn interruption_preserves_before_equal_and_overshot_targets_without_observation() {
        for clock in [41, 42, 43] {
            let mut state = armed();
            let ticket = state.next_ticket().unwrap();
            let original = retained(&state);
            state.begin_interruption(ticket, 0x4010, clock).unwrap();
            assert_eq!(state.request.as_ref().unwrap().target, 42);
            assert_eq!(retained(&state), original);
            assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
            let ticket = ticket.unwrap();
            assert_eq!(
                state.advance(Observation {
                    generation: ticket.generation,
                    sequence: ticket.sequence,
                    clock,
                    rip: 0x4017,
                }),
                Err(Errno::EBUSY)
            );
            assert_eq!(state.cancel(), Err(Errno::EBUSY));
            assert_eq!(retained(&state), original);
        }
    }

    #[test]
    fn interruption_keeps_active_suffix_and_defers_callback_replacement() {
        let mut state = armed();
        for clock in [42, 42] {
            let ticket = state.next_ticket().unwrap().unwrap();
            assert_eq!(
                state.advance(Observation {
                    generation: ticket.generation,
                    sequence: ticket.sequence,
                    clock,
                    rip: 0x4008,
                }),
                Ok(None)
            );
        }
        let original = retained(&state);
        let ticket = state.next_ticket().unwrap();
        let identity = state.begin_interruption(ticket, 0x4010, 42).unwrap();
        state.open_window(0x4010, 42).unwrap();
        state.stage(TimerSchedule::Rcbs(8), true).unwrap();
        assert_eq!(
            state.interrupted.as_ref().unwrap().replacement,
            Some(DeferredRequest {
                clock: 42,
                target: 50,
                rcbs: 8,
                suffix: 0,
            })
        );
        assert_eq!(retained(&state), original);
        state.window = None;
        assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
        state.fail_interruption(&identity).unwrap();
        assert_eq!(retained(&state), original);
        assert!(state.interrupted.as_ref().unwrap().replacement.is_some());
    }

    #[test]
    fn interruption_without_replacement_retains_pending_request_or_absence() {
        for has_ticket in [false, true] {
            let mut state = if has_ticket { armed() } else { state() };
            let ticket = state.next_ticket().unwrap();
            let original = retained(&state);
            state.begin_interruption(ticket, 0x4010, 41).unwrap();
            state.open_window(0x4010, 41).unwrap();
            state.window = None;
            assert!(state.interrupted.as_ref().unwrap().replacement.is_none());
            assert_eq!(retained(&state), original);
            assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
        }
    }

    #[test]
    fn interruption_deferred_immediate_cannot_deliver_or_erase_old_request() {
        for has_ticket in [false, true] {
            let mut state = if has_ticket { armed() } else { state() };
            let ticket = state.next_ticket().unwrap();
            let original = retained(&state);
            state.begin_interruption(ticket, 0x4010, 41).unwrap();
            state.open_window(0x4010, 41).unwrap();
            state.stage(TimerSchedule::Rcbs(0), true).unwrap();
            assert_eq!(
                state.interrupted.as_ref().unwrap().replacement,
                Some(DeferredRequest {
                    clock: 41,
                    target: 41,
                    rcbs: 0,
                    suffix: 0,
                })
            );
            assert_eq!(state.immediate(0x4010, 41), Err(Errno::EBUSY));
            assert_eq!(state.cancel(), Err(Errno::EBUSY));
            assert_eq!(retained(&state), original);
        }
    }

    #[test]
    fn interruption_failure_preserves_last_deferred_request_and_blocks_reentry() {
        for invalid_schedule in [false, true] {
            let mut state = armed();
            let ticket = state.next_ticket().unwrap();
            let original = retained(&state);
            let identity = state.begin_interruption(ticket, 0x4010, 41).unwrap();
            state.open_window(0x4010, 41).unwrap();
            state
                .stage(TimerSchedule::RcbsAndInstructions(3, 2), true)
                .unwrap();
            if invalid_schedule {
                assert_eq!(
                    state.stage(TimerSchedule::Rcbs(u64::MAX), true),
                    Err(Errno::EOVERFLOW)
                );
            } else {
                state.fail_interruption(&identity).unwrap();
            }
            assert_eq!(
                state.interrupted.as_ref().unwrap().replacement,
                Some(DeferredRequest {
                    clock: 41,
                    target: 44,
                    rcbs: 3,
                    suffix: 2,
                })
            );
            assert_eq!(
                state.stage(TimerSchedule::Rcbs(1), true),
                Err(Errno::ECANCELED)
            );
            state.window = None;
            assert_eq!(state.open_window(0x4010, 41), Err(Errno::ECANCELED));
            assert_eq!(
                state.begin_interruption(ticket, 0x4010, 41),
                Err(Errno::EBUSY)
            );
            assert_eq!(retained(&state), original);
        }
    }

    #[test]
    fn interruption_rejects_stale_ticket_owner_count_and_callback_window() {
        let mut state = armed();
        let ticket = state.next_ticket().unwrap().unwrap();
        let original = retained(&state);
        assert_eq!(
            state.begin_interruption(None, 0x4010, 41),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            state.begin_interruption(
                Some(Ticket {
                    sequence: 0,
                    ..ticket
                }),
                0x4010,
                41
            ),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            state.begin_interruption(Some(ticket), 0x4010, 39),
            Err(Errno::EINVAL)
        );
        let identity = state.begin_interruption(Some(ticket), 0x4010, 41).unwrap();
        assert_eq!(
            state.fail_interruption(&ModeledInterruption {
                owner: 2,
                sequence: identity.sequence
            }),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            state.fail_interruption(&ModeledInterruption {
                owner: 1,
                sequence: identity.sequence + 1
            }),
            Err(Errno::EINVAL)
        );
        assert_eq!(state.open_window(0x4017, 41), Err(Errno::EINVAL));
        assert_eq!(state.open_window(0x4010, 42), Err(Errno::EINVAL));
        assert!(!state.interrupted.as_ref().unwrap().failed);
        assert_eq!(retained(&state), original);
    }

    #[test]
    fn uninterrupted_suffix_and_immediate_still_use_current_generation() {
        let mut state = armed();
        for (index, clock) in [41, 42, 42, 42, 42].into_iter().enumerate() {
            let ticket = state.next_ticket().unwrap().unwrap();
            let result = state
                .advance(Observation {
                    generation: ticket.generation,
                    sequence: ticket.sequence,
                    clock,
                    rip: 0x4010,
                })
                .unwrap();
            assert_eq!(result.is_some(), index == 4);
        }
        assert!(state.request.is_none());
        state.open_window(0x4017, 42).unwrap();
        state.stage(TimerSchedule::Rcbs(0), true).unwrap();
        assert_eq!(state.immediate(0x4017, 42).unwrap().unwrap().clock, 42);
        assert_eq!(state.immediate(0x4017, 42), Ok(None));
    }

    fn assert_missing_window_poison(prior_intent: bool) {
        let mut state = armed();
        let ticket = state.next_ticket().unwrap();
        let original = retained(&state);
        assert_eq!(
            state.stage(TimerSchedule::Rcbs(5), true),
            Err(Errno::EOPNOTSUPP)
        );
        assert_eq!(retained(&state), original);
        assert!(state.interrupted.is_none());
        state.begin_interruption(ticket, 0x4010, 41).unwrap();
        if prior_intent {
            state.open_window(0x4010, 41).unwrap();
            state
                .stage(TimerSchedule::RcbsAndInstructions(3, 2), true)
                .unwrap();
            state.window = None;
        }
        let expected = prior_intent.then_some(DeferredRequest {
            clock: 41,
            target: 44,
            rcbs: 3,
            suffix: 2,
        });
        assert_eq!(
            state.stage(TimerSchedule::Rcbs(5), true),
            Err(Errno::EOPNOTSUPP)
        );
        assert!(state.interrupted.as_ref().unwrap().failed);
        assert_eq!(state.interrupted.as_ref().unwrap().replacement, expected);
        assert_eq!(retained(&state), original);
        assert_eq!(state.open_window(0x4010, 41), Err(Errno::ECANCELED));
        assert_eq!(
            state.stage(TimerSchedule::Rcbs(1), true),
            Err(Errno::ECANCELED)
        );
        assert_eq!(
            state.begin_interruption(ticket, 0x4010, 41),
            Err(Errno::EBUSY)
        );
        assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
        assert_eq!(state.cancel(), Err(Errno::EBUSY));
        assert_eq!(state.interrupted.as_ref().unwrap().replacement, expected);
        assert_eq!(retained(&state), original);
    }

    #[test]
    fn missing_window_poisons_interruption_with_prior_intent() {
        assert_missing_window_poison(true);
    }

    #[test]
    fn missing_window_poisons_interruption_without_prior_intent() {
        assert_missing_window_poison(false);
    }

    fn completion(ticket: Ticket, clock: u64) -> Observation {
        Observation {
            generation: ticket.generation,
            sequence: ticket.sequence,
            clock,
            rip: 0x4017,
        }
    }

    fn active_suffix() -> OwnedTimer {
        let mut state = armed();
        for _ in 0..2 {
            let ticket = state.next_ticket().unwrap().unwrap();
            assert_eq!(state.advance(completion(ticket, 42)), Ok(None));
        }
        state
    }

    #[test]
    fn explicit_rearm_supersedes_active_suffix_without_old_observation() {
        let mut state = active_suffix();
        let old = state.next_ticket().unwrap().unwrap();
        let identity = state.begin_interruption(Some(old), 0x4010, 42).unwrap();
        state.open_window(0x4010, 42).unwrap();
        state.stage(TimerSchedule::Rcbs(8), true).unwrap();
        state.window = None;
        assert_eq!(
            state.resolve_interruption(&identity, Some(completion(old, 42))),
            Ok(InterruptionResolution::Replaced)
        );
        let current = state.next_ticket().unwrap().unwrap();
        assert_ne!(current.generation, old.generation);
        assert_eq!(current.sequence, 1);
        let request = state.request.as_ref().unwrap();
        assert_eq!((request.target, request.suffix, request.clock), (50, 0, 42));
        let replacement = retained(&state);
        assert_eq!(state.advance(completion(old, 42)), Err(Errno::EINVAL));
        assert_eq!(retained(&state), replacement);
        assert_eq!(
            state.resolve_interruption(&identity, Some(completion(old, 42))),
            Err(Errno::EINVAL)
        );
        assert_eq!(retained(&state), replacement);
        assert_eq!(state.advance(completion(current, 49)), Ok(None));
        let current = state.next_ticket().unwrap().unwrap();
        assert!(state.advance(completion(current, 50)).unwrap().is_some());
    }

    #[test]
    fn private_interruption_without_arm_preserves_and_completes_existing_suffix_once() {
        let mut state = active_suffix();
        let old = state.next_ticket().unwrap().unwrap();
        let identity = state.begin_interruption(Some(old), 0x4010, 42).unwrap();
        state.open_window(0x4010, 42).unwrap();
        state.window = None;
        assert_eq!(
            state.resolve_interruption(&identity, Some(completion(old, 42))),
            Ok(InterruptionResolution::Preserved(None))
        );
        let current = state.next_ticket().unwrap().unwrap();
        assert_eq!(current.generation, old.generation);
        assert_eq!(current.sequence, old.sequence + 1);
        assert!(state.advance(completion(current, 42)).unwrap().is_some());
        assert_eq!(state.next_ticket(), Ok(None));
        assert_eq!(
            state.resolve_interruption(&identity, Some(completion(old, 42))),
            Err(Errno::EINVAL)
        );
    }

    #[test]
    fn retained_target_before_equal_and_overshoot_obey_original_controller() {
        for clock in [41, 42, 43] {
            let mut state = armed();
            let old = state.next_ticket().unwrap().unwrap();
            let original = retained(&state);
            let identity = state.begin_interruption(Some(old), 0x4010, clock).unwrap();
            let result = state.resolve_interruption(&identity, Some(completion(old, clock)));
            if clock > 42 {
                assert_eq!(result, Err(Errno::EINVAL));
                assert_eq!(retained(&state), original);
                assert!(state.interrupted.as_ref().unwrap().failed);
                assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
            } else {
                assert_eq!(result, Ok(InterruptionResolution::Preserved(None)));
                assert_eq!(
                    state.next_ticket().unwrap().unwrap().generation,
                    old.generation
                );
            }
        }
    }

    #[test]
    fn explicit_arm_replaces_before_equal_and_overshot_old_target() {
        for clock in [41, 42, 43] {
            let mut state = armed();
            let old = state.next_ticket().unwrap().unwrap();
            let identity = state.begin_interruption(Some(old), 0x4010, clock).unwrap();
            state.open_window(0x4010, clock).unwrap();
            state.stage(TimerSchedule::Rcbs(5), true).unwrap();
            state.window = None;
            assert_eq!(
                state.resolve_interruption(&identity, Some(completion(old, clock))),
                Ok(InterruptionResolution::Replaced)
            );
            assert_eq!(state.request.as_ref().unwrap().target, clock + 5);
            assert_ne!(
                state.next_ticket().unwrap().unwrap().generation,
                old.generation
            );
        }
    }

    #[test]
    fn no_ticket_and_explicit_immediate_use_only_new_continuation() {
        let mut empty = state();
        let identity = empty.begin_interruption(None, 0x4010, 42).unwrap();
        assert_eq!(
            empty.resolve_interruption(&identity, None),
            Ok(InterruptionResolution::Preserved(None))
        );
        assert_eq!(empty.next_ticket(), Ok(None));
        for has_old in [false, true] {
            let mut state = if has_old { active_suffix() } else { state() };
            let old = state.next_ticket().unwrap();
            let identity = state.begin_interruption(old, 0x4010, 42).unwrap();
            state.open_window(0x4010, 42).unwrap();
            state.stage(TimerSchedule::Rcbs(0), true).unwrap();
            state.window = None;
            assert_eq!(
                state.resolve_interruption(&identity, old.map(|ticket| completion(ticket, 42))),
                Ok(InterruptionResolution::Replaced)
            );
            let position = state.immediate(0x4017, 42).unwrap().unwrap();
            assert_eq!(
                (
                    position.rip,
                    position.clock,
                    position.sequence,
                    position.target,
                    position.suffix
                ),
                (0x4017, 42, 0, 42, 0)
            );
            if let Some(old) = old {
                assert_ne!(position.generation, old.generation.value());
            }
            assert_eq!(state.immediate(0x4017, 42), Ok(None));
        }
    }

    #[test]
    fn callback_failure_after_rearm_cannot_resolve_or_resume() {
        let mut state = active_suffix();
        let old = state.next_ticket().unwrap().unwrap();
        let original = retained(&state);
        let identity = state.begin_interruption(Some(old), 0x4010, 42).unwrap();
        state.open_window(0x4010, 42).unwrap();
        state.stage(TimerSchedule::Rcbs(8), true).unwrap();
        state.fail_interruption(&identity).unwrap();
        state.window = None;
        assert_eq!(
            state.resolve_interruption(&identity, Some(completion(old, 42))),
            Err(Errno::ECANCELED)
        );
        assert_eq!(retained(&state), original);
        assert_eq!(
            state
                .interrupted
                .as_ref()
                .unwrap()
                .replacement
                .as_ref()
                .unwrap()
                .target,
            50
        );
        assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
        assert_eq!(state.immediate(0x4017, 42), Err(Errno::EBUSY));
    }

    #[test]
    fn resolver_binds_transaction_observation_and_closed_callback() {
        for defect in 0..4 {
            let mut state = armed();
            let old = state.next_ticket().unwrap().unwrap();
            let original = retained(&state);
            let identity = state.begin_interruption(Some(old), 0x4010, 41).unwrap();
            state.open_window(0x4010, 41).unwrap();
            state.stage(TimerSchedule::Rcbs(5), true).unwrap();
            let wrong = ModeledInterruption {
                owner: identity.owner + 1,
                sequence: identity.sequence,
            };
            assert_eq!(
                state.resolve_interruption(&wrong, Some(completion(old, 41))),
                Err(Errno::EINVAL)
            );
            assert!(!state.interrupted.as_ref().unwrap().failed);
            if defect != 3 {
                state.window = None;
            }
            let observed = match defect {
                0 => None,
                1 => Some(Observation {
                    clock: 42,
                    ..completion(old, 41)
                }),
                2 => Some(Observation {
                    sequence: old.sequence + 1,
                    ..completion(old, 41)
                }),
                _ => Some(completion(old, 41)),
            };
            assert_eq!(
                state.resolve_interruption(&identity, observed),
                Err(if defect == 3 {
                    Errno::EBUSY
                } else {
                    Errno::EINVAL
                })
            );
            assert!(state.interrupted.as_ref().unwrap().failed);
            assert_eq!(retained(&state), original);
            assert_eq!(state.next_ticket(), Err(Errno::EBUSY));
        }
    }
}

pub(crate) struct TimerOwner {
    timer: UnsafeCell<InGuestRcbTimer>,
    owner: i64,
    borrowed: Cell<bool>,
}

struct TimerBorrow<'owner> {
    borrowed: &'owner Cell<bool>,
    revision: u64,
}

impl Drop for TimerBorrow<'_> {
    fn drop(&mut self) {
        crate::clock_control::finish_notification_update(self.revision);
        self.borrowed.set(false);
    }
}

impl TimerOwner {
    pub(crate) fn descriptor(&self) -> i32 {
        unsafe { &*self.timer.get() }.raw_fd()
    }

    pub(crate) fn with_timer<Output>(
        &self,
        operation: impl FnOnce(&mut InGuestRcbTimer) -> Result<Output, Errno>,
    ) -> Result<Output, Errno> {
        if unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } != self.owner {
            return Err(Errno::EXDEV);
        }
        if !crate::clock_control::paused() || self.borrowed.replace(true) {
            return Err(Errno::EBUSY);
        }
        let Some(revision) = crate::clock_control::notification_revision().checked_add(1) else {
            self.borrowed.set(false);
            return Err(Errno::EOVERFLOW);
        };
        let _borrow = TimerBorrow {
            borrowed: &self.borrowed,
            revision,
        };
        operation(unsafe { &mut *self.timer.get() })
    }

    fn cancel(&self) -> Result<(), Errno> {
        self.with_timer(|timer| {
            crate::clock_control::cancel_notification_resume();
            timer.disarm()
        })
    }
}

pub(crate) fn initialize() -> io::Result<()> {
    if !crate::clock_control::active() {
        return Ok(());
    }
    if !crate::clock_control::notification_owner().is_null() {
        return Err(io::Error::other("notification timer already installed"));
    }
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if owner <= 0 {
        return Err(io::Error::other("notification timer thread unavailable"));
    }
    let timer = unsafe {
        InGuestRcbTimer::current_thread_with_syscall_gate(raw_syscall6, reverie::PERF_EVENT_SIGNAL)
    }
    .map_err(io::Error::other)?;
    crate::clock_control::publish_notification(Box::new(TimerOwner {
        timer: UnsafeCell::new(timer),
        owner,
        borrowed: Cell::new(false),
    }))
}

pub(crate) fn request(_schedule: TimerSchedule, precise: bool) -> Result<(), Error> {
    let owner = crate::clock_control::notification_owner();
    if !owner.is_null() {
        unsafe { &*owner }.cancel()?;
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        if precise {
            "LiteInst precise timer delivery requires an exact position witness and safe Tool entry"
        } else {
            "LiteInst timer delivery requires a safe Tool entry"
        },
    )
    .into())
}
