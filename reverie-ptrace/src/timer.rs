/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Timers monitor a specified thread using the PMU and deliver a signal
//! after a specified number of events occur. The signal is then identified
//! and transformed into a reverie timer event. This is intended to allow
//! tools to break busywaits or other spins in a reliable manner. Timers
//! are ideally deterministic so that `detcore` can use them.
//!
//! Due to PMU skid, precise timer events normally must be driven to completion
//! via single stepping. This means the PMI is scheduled early, and events with
//! very short timeouts require immediate single stepping. Immediate stepping is
//! acheived by artificially generating a signal that will then be delivered
//! immediately upon resumption of the guest. If delivery is already past the
//! target, the overshoot is recorded and the event is delivered at the observed
//! counter because single stepping cannot move the guest backward.
//!
//! Proper use of timers requires that all delivered signals of type
//! `Timer::signal_type()` be passed through `Timer::handle_signal`, and that
//! `Timer::observe_event()` be called whenever a Tool-observable reverie event
//! occurs. Additionally, `Timer::finalize_requests()` must be called
//!  - after the end of the tool callback in which the user could have
//!    requested a timer event, i.e. those with `&mut guest` access.
//!  - after any reverie-critical single-stepping occurs (e.g. in syscall
//!    injections),
//!  - before resumption of the guest,
//!    which _usually_ means immediately after the tool callback returns.

use std::cmp::Ordering::Equal;
use std::cmp::Ordering::Greater;
use std::cmp::Ordering::Less;
use std::sync::OnceLock;

use reverie::Errno;
use reverie::Pid;
use reverie::RegDisplay;
use reverie::RegDisplayOptions;
use reverie::Signal;
use reverie::Tid;
use safeptrace::Error as TraceError;
use safeptrace::Event as TraceEvent;
use safeptrace::Running;
use safeptrace::Stopped;
use safeptrace::Wait;
use thiserror::Error;
use tracing::debug;
use tracing::trace;
use tracing::warn;

use crate::perf::*;

// This signal is unused, in that the kernel will never send it to a process.
const MARKER_SIGNAL: Signal = reverie::PERF_EVENT_SIGNAL;

/// We refuse to schedule a "perf timeout" for this or fewer RCBs, instead
/// choosing to directly single step. This is because I am somewhat paranoid
/// about perf event throttling, which isn't well-documented.
const SINGLESTEP_TIMEOUT_RCBS: u64 = 5;

/// The single, greppable marker emitted to stderr whenever this backend detects
/// the RCB fallback overshooting its target. Re-exported from the
/// backend-agnostic `reverie` crate so that this precise single-step guard and
/// hermit's detcore log-and-continue path emit the *same* token — one marker,
/// one source. See [`reverie::SKID_OVERSHOOT_MARKER`] for the full contract and
/// the retry-harness safety property.
pub use reverie::SKID_OVERSHOOT_MARKER;

/// Fault-injection knob: when set to a parseable `u64`, this env var overrides
/// the processor-detected skid margin for the whole process. Setting it to `0`
/// programs the precise timer's overflow interrupt *at* the target RCB instead
/// of `skid_margin` RCBs early, so any natural positive skid pushes the fallback
/// single-step past the target and deterministically triggers the
/// [`SKID_OVERSHOOT_MARKER`] path. This exists to exercise the skid overshoot +
/// retry harness on demand; it is not a tuning knob for normal runs. A value
/// that does not parse as `u64` is ignored (processor default retained).
pub const SKID_MARGIN_OVERRIDE_ENV: &str = "REVERIE_SKID_MARGIN_OVERRIDE";

/// Supervisor-only witness nonce. When the consuming harness (the hermit
/// *supervisor*) sets this env var, its exact value is stamped into every
/// [`SKID_OVERSHOOT_MARKER`] line as a ` witness=<value>` field. A downstream
/// retry gate uses it to distinguish a genuine supervisor-emitted overshoot from
/// guest-printed marker text: hermit strips this var from the guest environment,
/// so the guest can neither read nor forge the value. Unset or empty leaves the
/// marker in its plain (unauthenticated) shape.
pub const WITNESS_TOKEN_ENV: &str = "HERMIT_SKID_WITNESS_TOKEN";

static PMU_CONFIG: OnceLock<PmuConfig> = OnceLock::new();

pub(crate) fn get_pmu_config() -> &'static PmuConfig {
    PMU_CONFIG.get_or_init(PmuConfig::new)
}

/// Processor-specific PMU event settings used by precise ptrace timers.
// TODO-HUMAN-REVIEW(PR-186): Review the programmatic PMU skid-margin override API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PmuConfig {
    rcb_event: u64,
    skid_margin: u64,
    skid_margin_override: Option<u64>,
}

impl PmuConfig {
    /// Attempts to configure the PMU without assuming that this CPU has a
    /// measured deterministic-timer profile.
    ///
    /// In-guest clocks can treat an unknown CPU as an unavailable optional
    /// capability. Precise ptrace timers retain [`Self::new`]'s fail-fast
    /// contract because they also require a measured skid margin.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn try_new() -> Option<Self> {
        let features = raw_cpuid::CpuId::new().get_feature_info()?;
        Self::try_from_family_model(features.family_id(), features.model_id())
            .map(Self::with_env_overrides)
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn try_new() -> Option<Self> {
        Some(Self::new())
    }

    /// Creates / initializes the PMU config.
    #[cfg(target_arch = "x86_64")]
    pub fn new() -> Self {
        let c = raw_cpuid::CpuId::new();
        let fi = c
            .get_feature_info()
            .expect("CPUID feature information is required to configure the PMU");
        Self::from_cpuid_features(fi).with_env_overrides()
    }

    #[cfg(target_arch = "aarch64")]
    pub fn new() -> Self {
        // TODO:
        //  1. Compute the microarchitecture from
        //     `/sys/devices/system/cpu/cpu*/regs/identification/midr_el1`
        //  2. Look up the microarchitecture in a table to determine what features
        //     we can enable.
        // References:
        //  - https://github.com/rr-debugger/rr/blob/master/src/PerfCounters.cc#L156
        const BR_RETIRED: u64 = 0x21;

        // For now, always assume that we can get retired branch events.
        Self {
            rcb_event: BR_RETIRED,
            skid_margin: 1000,
            skid_margin_override: None,
        }
        .with_env_overrides()
    }

    #[cfg(target_arch = "x86_64")]
    fn from_cpuid_features(fi: raw_cpuid::FeatureInfo) -> Self {
        Self::from_family_model(fi.family_id(), fi.model_id())
    }

    #[cfg(target_arch = "x86_64")]
    fn from_family_model(family_id: u8, model_id: u8) -> Self {
        Self::try_from_family_model(family_id, model_id).unwrap_or_else(|| match family_id {
            0x06 => panic!("Unsupported Intel processor model: {:#x}", model_id),
            family => panic!(
                "Unsupported processor family, model: ({:#x},{:#x})",
                family, model_id
            ),
        })
    }

    #[cfg(target_arch = "x86_64")]
    pub(crate) fn try_from_family_model(family_id: u8, model_id: u8) -> Option<Self> {
        reverie::pmu::PmuProfile::for_family_model(family_id, model_id).map(|profile| Self {
            rcb_event: profile.raw_rcb_event(),
            skid_margin: profile.default_skid_margin(),
            skid_margin_override: None,
        })
    }

    /// Overrides the processor-specific skid margin while preserving the detected PMU event.
    pub fn with_skid_margin_override(mut self, skid_margin: u64) -> Self {
        self.skid_margin_override = Some(skid_margin);
        self
    }

    /// Applies the [`SKID_MARGIN_OVERRIDE_ENV`] fault-injection override, if the
    /// env var is present and parses as a `u64`. A missing var leaves the
    /// processor default untouched; a present-but-unparseable value is ignored
    /// with a warning rather than aborting startup. When the override is applied
    /// it is announced loudly on stderr, because it deliberately degrades timer
    /// precision to force the overshoot path.
    fn with_env_overrides(self) -> Self {
        match std::env::var(SKID_MARGIN_OVERRIDE_ENV) {
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(v) => {
                    eprintln!(
                        "[reverie-ptrace] {}={} active: skid margin forced to {} RCBs \
                         (fault injection; not for production runs)",
                        SKID_MARGIN_OVERRIDE_ENV, raw, v
                    );
                    self.with_skid_margin_override(v)
                }
                Err(_) => {
                    eprintln!(
                        "[reverie-ptrace] ignoring {}={:?}: not a u64; using processor default",
                        SKID_MARGIN_OVERRIDE_ENV, raw
                    );
                    self
                }
            },
            Err(_) => self,
        }
    }

    /// This is the experimentally determined maximum number of RCBs an overflow
    /// interrupt is delivered after the originating RCB.
    ///
    /// If this number is too small, timer event delivery can pass its target and
    /// will be reported at the observed counter. If this number is too big, we
    /// degrade performance from excessive single stepping.
    pub fn skid_margin(&self) -> u64 {
        self.skid_margin_override.unwrap_or(self.skid_margin)
    }

    /// The maximum single step count we expect can occur when a precise timer
    /// event is requested that leaves less than the minimum perf timeout
    /// remaining.
    pub fn max_single_step_count(&self) -> u64 {
        self.skid_margin().saturating_add(SINGLESTEP_TIMEOUT_RCBS)
    }

    /// Emits the single canonical [`SKID_OVERSHOOT_MARKER`] line to stderr.
    ///
    /// This is called at every site that detects the RCB-fallback preemption
    /// landing past its programmed target, so the overshoot signal has exactly
    /// one greppable shape regardless of which layer noticed it. It reports the
    /// counter value actually observed, the intended target, the skid margin in
    /// effect (the CPU-specific constant whose tuning determines how often this
    /// fires), and the resulting overshoot.
    pub fn emit_skid_overshoot_marker(&self, rcb_actual: u64, rcb_target: u64) {
        eprintln!(
            "{}",
            self.format_skid_overshoot_marker(rcb_actual, rcb_target)
        );
    }

    /// Single decision-and-record site for a skid overshoot: iff the interrupt
    /// was delivered *past* the programmed target (`rcb_actual > rcb_target`),
    /// emit the canonical [`SKID_OVERSHOOT_MARKER`] line **and** increment the
    /// process-global witness counter via [`reverie::record_skid_overshoot`],
    /// then return `true`. A non-overshoot (`rcb_actual <= rcb_target`) records
    /// nothing and returns `false`.
    ///
    /// [`Self::attempt_single_step`]'s late-delivery guard is the sole runtime
    /// caller, so a unit test that drives real `(actual, target)` pairs through
    /// this method exercises exactly the behaviour the supervisor runs — a
    /// genuine overshoot causes exactly one witness record — without needing a
    /// live guest or PMU. The boolean return is what makes that behaviour
    /// (not merely the marker arithmetic) observable in a test.
    pub fn record_overshoot_if_past_target(&self, rcb_actual: u64, rcb_target: u64) -> bool {
        if rcb_actual > rcb_target {
            self.emit_skid_overshoot_marker(rcb_actual, rcb_target);
            // Structural, in-process signal (a guest cannot forge it) so an
            // in-process classifier can causally attribute a divergence to skid.
            reverie::record_skid_overshoot();
            true
        } else {
            false
        }
    }

    /// Formats the canonical [`SKID_OVERSHOOT_MARKER`] line. Split out from
    /// [`Self::emit_skid_overshoot_marker`] so the exact shape is unit-testable
    /// without capturing process stderr.
    pub fn format_skid_overshoot_marker(&self, rcb_actual: u64, rcb_target: u64) -> String {
        let overshoot = rcb_actual.saturating_sub(rcb_target);
        let mut line = format!(
            "{} rcb_actual={} rcb_target={} skid_margin={} overshoot={}",
            SKID_OVERSHOOT_MARKER,
            rcb_actual,
            rcb_target,
            self.skid_margin(),
            overshoot,
        );
        // Supervisor-only witness nonce (see [`WITNESS_TOKEN_ENV`]): stamps the
        // marker so a downstream retry gate can authenticate it as genuinely
        // supervisor-emitted rather than guest-printed. Read from the env at emit
        // time; the emit path is rare, so the lookup cost is irrelevant.
        if let Ok(token) = std::env::var(WITNESS_TOKEN_ENV)
            && !token.is_empty()
        {
            line.push_str(" witness=");
            line.push_str(&token);
        }
        line
    }

    /// The event needed to configure the PMU and observe RCBs.
    pub fn rcb_event(&self) -> Event {
        Event::Raw(self.rcb_event)
    }

    /// Returns the raw perf event selector for retired conditional branches.
    pub fn raw_rcb_event(&self) -> u64 {
        self.rcb_event
    }
}

impl Default for PmuConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Installs the PMU configuration used by subsequently created ptrace timers.
///
/// Returns the supplied configuration if a timer has already initialized the process-wide
/// configuration. Callers should install overrides before spawning a [`crate::Tracer`].
pub fn set_pmu_config(config: PmuConfig) -> Result<(), PmuConfig> {
    PMU_CONFIG.set(config)
}

/// Returns true if the current CPU supports precise_ip.
#[cfg(target_arch = "x86_64")]
pub(crate) fn has_precise_ip() -> bool {
    let cpu = raw_cpuid::CpuId::new();
    let feature_info = cpu.get_feature_info();
    let has_debug_store = feature_info.as_ref().is_some_and(|info| info.has_ds());

    // Identify the CPU by vendor and model only. Debug-printing the whole
    // `CpuId` (or a whole `FeatureInfo`) also dumps per-core topology --
    // `initial_local_apic_id`, `x2apic_id`, `core_id` -- which reflects
    // whichever core this thread happens to be scheduled on, not any property
    // of the machine. That made this line differ between otherwise identical
    // runs even though the decision below is a pure function of `has_ds()`.
    debug!(
        "Setting precise_ip to {} for cpu vendor {} family {:?} model {:?} stepping {:?}",
        has_debug_store,
        cpu.get_vendor_info()
            .map_or_else(|| "unknown".to_string(), |v| v.as_str().to_string()),
        feature_info.as_ref().map(|i| i.family_id()),
        feature_info.as_ref().map(|i| i.model_id()),
        feature_info.as_ref().map(|i| i.stepping_id()),
    );

    has_debug_store
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn has_precise_ip() -> bool {
    // Assume, for now, that aarch64 can use precise_ip.
    true
}

/// A timer monitoring a single thread. The underlying implementation is eagerly
/// initialized, but left empty if perf is not supported. In that case, any
/// methods with semantics that require a functioning clock or timer will panic.
#[derive(Debug)]
pub struct Timer {
    inner: Option<TimerImpl>,
}

/// Data requires to request a timer event
#[derive(Debug, Copy, Clone)]
pub enum TimerEventRequest {
    /// Event should fire after precisely this many RCBs.
    Precise(u64),

    /// Event should fire after at least this many RCBs.
    Imprecise(u64),

    /// Event should fire after precisely this many RCBS and this many instructions
    PreciseInstruction(u64, u64),
}

/// The possible results of handling a timer signal.
#[derive(Error, Debug, Eq, PartialEq)]
pub enum HandleFailure {
    #[error(transparent)]
    TraceError(#[from] TraceError),

    #[error("Unexpected event while single stepping")]
    Event(Wait),

    /// The timer signal was for a timer event that was otherwise cancelled. The
    /// task is returned unchanged.
    #[error("Timer event was cancelled and should not fire")]
    Cancelled(Stopped),

    /// The signal causing the signal-delivery stop was not actually meant for
    /// this timer. The task is returned unchanged.
    #[error("Pending signal was not for this timer")]
    ImproperSignal(Stopped),
}

/// Failure to suspend or restore the deterministic timer around private
/// controller execution.
///
/// Once a counter transition has begun, any error leaves the timer latched in
/// a fail-closed private-execution state. The tracee must remain stopped and the
/// caller must terminate that execution rather than resume it with counter
/// ownership in doubt.
#[derive(Error, Debug, Eq, PartialEq)]
pub(crate) enum PrivateExecutionTimerError {
    #[error("perf support is required to suspend the deterministic timer")]
    Unavailable,

    #[error("the deterministic timer is already in private-execution phase {phase}")]
    Busy { phase: &'static str },

    #[error("an artificial timer signal is pending")]
    ArtificialSignalPending,

    #[error("sampling counter {observed} has reached hardware notification threshold {threshold}")]
    NotificationThresholdReached { observed: u64, threshold: u64 },

    #[error("deterministic timer state is inconsistent: {0}")]
    InconsistentState(&'static str),

    #[error("private-execution suspension token does not match the active timer suspension")]
    TokenMismatch,

    #[error("{operation} failed while transitioning deterministic timer counters: {source}")]
    CounterOperation {
        operation: &'static str,
        source: Errno,
    },

    #[error(
        "{operation} failed while transitioning deterministic timer counters: {source}; \
         fail-closed recovery also failed at {recovery_operation}: {recovery_source}"
    )]
    CounterOperationAndRecovery {
        operation: &'static str,
        source: Errno,
        recovery_operation: &'static str,
        recovery_source: Errno,
    },
}

/// One-use authority to restore a [`Timer`] after private controller execution.
///
/// The token is deliberately neither `Clone` nor `Copy`. Dropping it leaves the
/// timer suspended and all ordinary timer operations fail closed.
#[must_use = "the deterministic timer remains suspended until this token is restored"]
#[derive(Debug)]
pub(crate) struct PrivateExecutionTimerSuspension {
    snapshot: PrivateExecutionSnapshot,
}

impl PrivateExecutionTimerSuspension {
    pub(crate) fn frozen_clock(&self) -> u64 {
        self.snapshot.frozen_clock
    }
}

impl Timer {
    /// Create a new timer monitoring the specified thread.
    pub fn new(guest_pid: Pid, guest_tid: Tid) -> Self {
        // No errors are exposed here, as the construction should be
        // bullet-proof, and if it wasn't, consumers wouldn't be able to
        // meaningfully handle the error anyway.
        Self {
            inner: if is_perf_supported() {
                Some(TimerImpl::new(guest_pid, guest_tid).unwrap_or_else(|err| {
                    panic!(
                        "failed to initialize perf timer for tracee {guest_tid} \
                         in process {guest_pid}: {err}"
                    )
                }))
            } else {
                None
            },
        }
    }

    fn inner(&self) -> &TimerImpl {
        self.inner.as_ref().expect("Perf support required")
    }

    fn inner_noinit(&self) -> Option<&TimerImpl> {
        self.inner.as_ref()
    }

    fn inner_mut_noinit(&mut self) -> Option<&mut TimerImpl> {
        self.inner.as_mut()
    }

    /// Read the thread-local deterministic clock. Represents total elapsed RCBs
    /// on this thread since the timer was constructed, which should be at or
    /// near thread creation time.
    pub fn read_clock(&self) -> u64 {
        self.inner().read_clock()
    }

    pub(crate) fn diagnostic_clock(&self) -> Option<u64> {
        self.inner_noinit().map(TimerImpl::diagnostic_clock)
    }

    /// Stop both deterministic counters while a stopped tracee executes
    /// controller-owned code.
    ///
    /// The sampling counter is disabled before the non-resetting clock. No
    /// counter is reset and no period is changed. The returned token is the
    /// only authority accepted by [`Self::restore_after_private_execution`].
    /// The caller must hold the tracee in an exact ptrace stop from before this
    /// call until restoration succeeds.
    pub(crate) fn begin_suspend_for_private_execution(
        &mut self,
    ) -> Result<PrivateExecutionTimerSuspension, PrivateExecutionTimerError> {
        self.inner_mut_noinit()
            .ok_or(PrivateExecutionTimerError::Unavailable)?
            .begin_suspend_for_private_execution()
    }

    /// Complete the counter transition after the returned token has been
    /// installed in durable task state.  No mutating perf operation occurs in
    /// the begin half, so every partial transition has an external owner.
    pub(crate) fn complete_suspend_for_private_execution(
        &mut self,
        suspension: &PrivateExecutionTimerSuspension,
    ) -> Result<(), PrivateExecutionTimerError> {
        self.inner_mut_noinit()
            .ok_or(PrivateExecutionTimerError::Unavailable)?
            .complete_suspend_for_private_execution(suspension)
    }

    /// Restore counters from an exact private-execution suspension snapshot.
    ///
    /// The non-resetting clock is enabled first. The sampling counter is then
    /// enabled only when it was enabled at suspension. Any transition failure
    /// leaves the timer latched fail closed; the tracee must remain stopped.
    pub(crate) fn restore_after_private_execution(
        &mut self,
        suspension: &PrivateExecutionTimerSuspension,
    ) -> Result<(), PrivateExecutionTimerError> {
        self.inner_mut_noinit()
            .ok_or(PrivateExecutionTimerError::Unavailable)?
            .restore_after_private_execution(suspension)
    }

    /// Retire an exact private-execution suspension after the tracee reached a
    /// terminal generation. This consumes the token without touching either
    /// perf fd and leaves every ordinary timer operation fail-closed.
    pub(crate) fn retire_private_execution_on_terminal(
        &mut self,
        suspension: &PrivateExecutionTimerSuspension,
    ) -> Result<(), PrivateExecutionTimerError> {
        self.inner_mut_noinit()
            .ok_or(PrivateExecutionTimerError::Unavailable)?
            .retire_private_execution_on_terminal(suspension)
    }

    /// Approximately convert a duration to the internal notion of timer ticks.
    pub fn as_ticks(dur: core::time::Duration) -> u64 {
        // assumptions: 10% conditional branches, 3 GHz, avg 2 IPC
        // this gives: 0.6B branch / sec = 0.6 branch / ns
        (dur.as_secs() * 600_000_000) + (u64::from(dur.subsec_nanos()) * 6 / 10)
    }

    /// Return the signal type sent by the timer. This is intended to allow
    /// pre-filtering signals without the full overhead of gathering signal info
    /// to pass to ['Timer::generated_signal`].
    pub fn signal_type() -> Signal {
        MARKER_SIGNAL
    }

    /// Request a timer event to occur in the future at a time specified by
    /// `evt`.
    ///
    /// This is *not* idempotent and will replace the outstanding request. If it
    /// is called repeatedly no events will be delivered.
    pub fn request_event(&mut self, evt: TimerEventRequest) -> Result<(), Errno> {
        self.inner_mut_noinit()
            .ok_or(Errno::ENODEV)?
            .request_event(evt)
    }

    /// Must be called whenever a Tool-observable reverie event occurs. This
    /// ensures proper cancellation semantics are observed. See the internal
    /// `timer::EventStatus` type for details.
    pub fn observe_event(&mut self) {
        if let Some(t) = self.inner_mut_noinit() {
            t.observe_event();
        }
    }

    /// Cancel pending timer notifications. This is idempotent.
    ///
    /// If there was a previous call to [`Timer::enable_interval'], this
    /// will prevent the delivery of that notification. This also has the effect
    /// of reseting the "elapsed ticks." That is, if the current notification
    /// duration is `N` ticks, then a full `N` ticks must elapse after the next
    /// call to [`enable_interval`](Timer::enable_interval) before a
    /// notification is delivered.
    ///
    /// While [`Timer::cancel`] actually disables the counting of RCBs, this
    /// method simply sets a flag to subsequent delivered signals until
    /// [`Timer::request_event`] is called again. Thus, this method is lighter
    /// if called multiple times, but still results in a signal delivery, while
    /// [`Timer::cancel`] must perform a syscall, but will actually cancel the
    /// signal.
    #[allow(dead_code)]
    pub fn schedule_cancellation(&mut self) {
        if let Some(t) = self.inner_mut_noinit() {
            t.schedule_cancellation();
        }
    }

    /// Cancel pending timer notifications. This is idempotent.
    ///
    /// If there was a previous call to [`Timer::enable_interval'], this
    /// will prevent the delivery of that notification. This also has the effect
    /// of reseting the "elapsed ticks." That is, if the current notification
    /// duration is `N` ticks, then a full `N` ticks must elapse after the next
    /// call to [`enable_interval`](Timer::enable_interval) before a
    /// notification is delivered.
    ///
    /// See [`Timer::schedule_cancellation`] for a comparison with this
    /// method.
    #[allow(dead_code)]
    pub fn cancel(&mut self) -> Result<(), Errno> {
        self.inner_mut_noinit()
            .map(|t| t.cancel())
            .unwrap_or(Ok(()))
    }

    /// Perform finalization actions on requests for timer events before guest
    /// resumption. See the module-level documentation for rules about when this can and
    /// should be called.
    ///
    /// Currently, this will, if necessary, `tgkill` a timer signal to the guest
    /// thread.
    pub fn finalize_requests(&self) {
        if let Some(t) = self.inner_noinit() {
            t.finalize_requests();
        }
    }

    /// When a signal is received, this method drives the timer event to
    /// completion via single stepping, after checking that the signal was meant
    /// for this specific timer. This *must* be called when a timer signal is
    /// received for correctness.
    ///
    /// Preconditions: task is in signal-delivery-stop.
    /// Postconditions: if a signal meant for this timer was the cause of the
    /// stop, the tracee will be at the precise instruction the timer event
    /// should fire at, unless signal delivery was already past the target. In
    /// that case the overshoot is recorded and the event fires at the observed
    /// counter.
    /// Drives a timer signal using caller-owned stopped-state transitions.
    ///
    /// LiteInst uses this hook to keep its exact-generation root-stop lease
    /// synchronized across the precise timer's internal single steps. The
    /// non-LiteInst caller supplies the historical raw transition.
    pub(crate) async fn handle_signal(
        &mut self,
        task: Stopped,
        step: &mut (dyn FnMut(Stopped) -> Result<Running, TraceError> + Send),
        observe: &mut (dyn FnMut(&Wait) -> Result<(), TraceError> + Send),
    ) -> Result<Stopped, HandleFailure> {
        match self.inner_mut_noinit() {
            Some(t) => t.handle_signal(task, step, observe).await,
            None => {
                warn!("Stray SIGSTKFLT indicates a bug!");
                Err(HandleFailure::ImproperSignal(task))
            }
        }
    }
}

/// The lazy-initialized part of a `Timer` that holds the functionality.
#[derive(Debug)]
struct TimerImpl {
    /// A non-resetting counter functioning as a thread-local clock.
    clock: PerfCounter,

    /// A separate counter used to generate signals for timer events
    timer: PerfCounter,

    /// Last successfully established physical enable state of `clock`.
    clock_enabled: CounterEnableState,

    /// Last successfully established physical enable state of `timer`.
    timer_enabled: CounterEnableState,

    /// Hardware notification threshold programmed by the active request.
    /// `None` means that the request uses an artificial signal or that no
    /// hardware notification is armed.
    timer_notification_threshold: Option<u64>,

    /// Information about the active timer event, including expected counter
    /// values.
    event: ActiveEvent,

    /// The cancellation status of the active timer event.
    timer_status: EventStatus,

    /// Whether or not the active timer event requires an artificial signal
    send_artificial_signal: bool,

    /// Pid (tgid) of the monitored thread
    guest_pid: Pid,

    /// Tid of the monitored thread
    guest_tid: Tid,

    /// A private-execution transition remains present until its exact token has
    /// restored both counters. `Failed` is intentionally terminal for this
    /// timer so callers cannot accidentally resume after a partial ioctl.
    private_execution: Option<PrivateExecutionState>,

    /// Monotonic token identity local to this timer.
    next_private_execution_id: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum CounterEnableState {
    Enabled,
    Disabled,
    Unknown,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct RetainedTimerState {
    event: ActiveEvent,
    timer_status: EventStatus,
    send_artificial_signal: bool,
    timer_enabled: CounterEnableState,
    clock_enabled: CounterEnableState,
    timer_notification_threshold: Option<u64>,
    guest_pid: Pid,
    guest_tid: Tid,
}

impl RetainedTimerState {
    fn changed_field(&self, current: &Self) -> Option<&'static str> {
        if self.event != current.event {
            Some("active event")
        } else if self.timer_status != current.timer_status {
            Some("event status")
        } else if self.send_artificial_signal != current.send_artificial_signal {
            Some("artificial-signal state")
        } else if self.timer_enabled != current.timer_enabled {
            Some("sampling-counter enable state")
        } else if self.clock_enabled != current.clock_enabled {
            Some("clock-counter enable state")
        } else if self.timer_notification_threshold != current.timer_notification_threshold {
            Some("hardware notification threshold")
        } else if self.guest_pid != current.guest_pid {
            Some("guest pid")
        } else if self.guest_tid != current.guest_tid {
            Some("guest tid")
        } else {
            None
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct PrivateExecutionSnapshot {
    id: u64,
    retained: RetainedTimerState,
    frozen_clock: u64,
    frozen_timer: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum PrivateExecutionState {
    Suspending(PrivateExecutionSnapshot),
    Suspended(PrivateExecutionSnapshot),
    Restoring(PrivateExecutionSnapshot),
    Failed(PrivateExecutionSnapshot),
    Terminal(PrivateExecutionSnapshot),
}

impl PrivateExecutionState {
    fn phase(self) -> &'static str {
        let (phase, snapshot) = match self {
            Self::Suspending(snapshot) => ("suspending", snapshot),
            Self::Suspended(snapshot) => ("suspended", snapshot),
            Self::Restoring(snapshot) => ("restoring", snapshot),
            Self::Failed(snapshot) => ("failed", snapshot),
            Self::Terminal(snapshot) => ("terminal", snapshot),
        };
        let _ = snapshot;
        phase
    }

    fn snapshot(self) -> PrivateExecutionSnapshot {
        match self {
            Self::Suspending(snapshot)
            | Self::Suspended(snapshot)
            | Self::Restoring(snapshot)
            | Self::Failed(snapshot)
            | Self::Terminal(snapshot) => snapshot,
        }
    }
}

fn terminal_private_execution_snapshot(
    state: Option<PrivateExecutionState>,
    suspension: &PrivateExecutionTimerSuspension,
    current: RetainedTimerState,
) -> Result<PrivateExecutionSnapshot, PrivateExecutionTimerError> {
    let snapshot = match state {
        Some(
            PrivateExecutionState::Suspending(snapshot)
            | PrivateExecutionState::Suspended(snapshot)
            | PrivateExecutionState::Restoring(snapshot)
            | PrivateExecutionState::Failed(snapshot),
        ) => snapshot,
        Some(PrivateExecutionState::Terminal(_)) => {
            return Err(PrivateExecutionTimerError::Busy { phase: "terminal" });
        }
        None => {
            return Err(PrivateExecutionTimerError::InconsistentState(
                "no private-execution suspension was active",
            ));
        }
    };
    if suspension.snapshot != snapshot {
        return Err(PrivateExecutionTimerError::TokenMismatch);
    }
    let mut expected = snapshot.retained;
    expected.timer_enabled = current.timer_enabled;
    expected.clock_enabled = current.clock_enabled;
    if let Some(field) = expected.changed_field(&current) {
        return Err(PrivateExecutionTimerError::InconsistentState(field));
    }
    Ok(snapshot)
}

/// Tracks cancellation status of a timer event in response to other reverie
/// events.
///
/// Whenever a reverie event occurs, this should tick "forward" once. If the
/// timer signal is first to occur, then the cancellation will be pending, and
/// the event will fire. If instead some other event occured, the tick will
/// result in `Cancelled` and the event will not fire.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum EventStatus {
    Scheduled,
    Armed,
    Cancelled,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ActiveEvent {
    Precise {
        /// Expected clock value when event fires.
        clock_target: u64,
        /// Instruction offset from clock target
        offset: u64,
    },
    Imprecise {
        /// Expected minimum clock value when event fires.
        clock_min: u64,
    },
}

impl ActiveEvent {
    /// Given the current clock, determine if another event is required to get the
    /// clock to its expected state
    fn reschedule_if_spurious_wakeup(&self, curr_clock: u64) -> Option<TimerEventRequest> {
        match self {
            ActiveEvent::Precise {
                clock_target,
                offset: _,
            } => {
                if clock_target.saturating_sub(curr_clock)
                    > get_pmu_config().max_single_step_count()
                {
                    Some(TimerEventRequest::Precise(*clock_target - curr_clock))
                } else {
                    None
                }
            }
            ActiveEvent::Imprecise { clock_min } => {
                if *clock_min > curr_clock {
                    Some(TimerEventRequest::Imprecise(*clock_min - curr_clock))
                } else {
                    None
                }
            }
        }
    }
}

impl EventStatus {
    pub fn next(self) -> Self {
        match self {
            EventStatus::Scheduled => EventStatus::Armed,
            EventStatus::Armed => EventStatus::Cancelled,
            EventStatus::Cancelled => EventStatus::Cancelled,
        }
    }

    pub fn tick(&mut self) {
        *self = self.next()
    }
}

/// This ClockCounter represents a pair in a form of (rcb, instr) that gets increased
/// while single-stepping to reach target (target_rcb, target_instr)
#[derive(Debug, Eq, PartialEq)]
struct ClockCounter {
    rcbs: u64,
    instr: u64,
    target_rcb: u64,
}

impl std::fmt::Display for ClockCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rcb: {}, instr: {}", self.rcbs, self.instr)
    }
}

impl ClockCounter {
    pub fn new(rcb: u64, instr: u64, target_rcb: u64) -> Self {
        Self {
            rcbs: rcb,
            instr,
            target_rcb,
        }
    }

    /// This method counts instructions & rcbs together in an attempt to reach target_rcb
    ///
    /// With each attempt we either increment rcb or instruction counter based on the read clock value.
    /// If we reach target_rcb we no longer increase rcb counter and allow to meet at the target instruction counter
    pub fn single_step_with_clock(&mut self, rcbs: u64) {
        match (self.rcbs.cmp(&self.target_rcb), self.rcbs.cmp(&rcbs)) {
            (Less, Less) => {
                self.instr = 0;
                self.rcbs = rcbs;
            }

            (Less | Equal, Equal) => {
                self.instr += 1;
            }

            (Equal, Less) => {
                self.instr += 1;
            }

            (_, Greater) => panic!(
                "current counter rcb value {} is greater than privided rcb value {}",
                self.rcbs, rcbs
            ),
            (Greater, _) => panic!(
                "current counter rcb value {} is greater than target rcb value {}",
                self.rcbs, self.target_rcb
            ),
        }
    }

    /// If a counter behind a given (rcb, instr) pair.
    ///
    /// Note: this is not always comparable. [None] will be returned in this case
    fn is_behind(&self, rcbs: u64, instr: u64) -> Option<bool> {
        match self.target_rcb.cmp(&rcbs) {
            Less => None,
            Greater | Equal => match self.rcbs.cmp(&rcbs) {
                Less => Some(true),
                Equal => Some(self.instr < instr),
                Greater => Some(false),
            },
        }
    }

    fn rcbs(&self) -> u64 {
        self.rcbs
    }
}

impl TimerImpl {
    pub fn new(guest_pid: Pid, guest_tid: Tid) -> Result<Self, Errno> {
        let evt = get_pmu_config().rcb_event();

        // measure the target tid irrespective of CPU
        let mut builder = Builder::new(guest_tid.as_raw(), -1);
        builder
            .sample_period(PerfCounter::DISABLE_SAMPLE_PERIOD)
            .event(evt);

        if has_precise_ip() {
            // set precise_ip to lowest value to enable PEBS (TODO: AMD?)
            builder.precise_ip(1);
        }

        let timer = builder.check_for_pmu_bugs().create()?;
        timer.set_signal_delivery(guest_tid, MARKER_SIGNAL)?;
        timer.reset()?;
        // measure the target tid irrespective of CPU
        let clock = Builder::new(guest_tid.as_raw(), -1)
            // counting event
            .sample_period(0)
            .event(evt)
            .fast_reads(true)
            .create()?;
        clock.reset()?;
        clock.enable()?;

        Ok(Self {
            timer,
            clock,
            clock_enabled: CounterEnableState::Enabled,
            timer_enabled: CounterEnableState::Disabled,
            timer_notification_threshold: None,
            event: ActiveEvent::Precise {
                clock_target: 0,
                offset: 0,
            },
            timer_status: EventStatus::Cancelled,
            send_artificial_signal: false,
            guest_pid,
            guest_tid,
            private_execution: None,
            next_private_execution_id: 1,
        })
    }

    fn retained_state(&self) -> RetainedTimerState {
        RetainedTimerState {
            event: self.event,
            timer_status: self.timer_status,
            send_artificial_signal: self.send_artificial_signal,
            timer_enabled: self.timer_enabled,
            clock_enabled: self.clock_enabled,
            timer_notification_threshold: self.timer_notification_threshold,
            guest_pid: self.guest_pid,
            guest_tid: self.guest_tid,
        }
    }

    fn private_execution_phase(&self) -> Option<&'static str> {
        self.private_execution.map(PrivateExecutionState::phase)
    }

    fn ensure_timer_operation_allowed(&self) -> Result<(), Errno> {
        if self.private_execution.is_some() {
            Err(Errno::EBUSY)
        } else {
            Ok(())
        }
    }

    fn assert_timer_operation_allowed(&self, operation: &'static str) {
        if let Some(phase) = self.private_execution_phase() {
            panic!(
                "Timer::{operation} is forbidden while private execution is {phase}; \
                 the tracee must remain stopped"
            );
        }
    }

    fn disable_sampling_counter(&mut self) -> Result<(), Errno> {
        match self.timer.disable() {
            Ok(()) => {
                self.timer_enabled = CounterEnableState::Disabled;
                Ok(())
            }
            Err(error) => {
                self.timer_enabled = CounterEnableState::Unknown;
                Err(error)
            }
        }
    }

    fn enable_sampling_counter(&mut self) -> Result<(), Errno> {
        match self.timer.enable() {
            Ok(()) => {
                self.timer_enabled = CounterEnableState::Enabled;
                Ok(())
            }
            Err(error) => {
                self.timer_enabled = CounterEnableState::Unknown;
                Err(error)
            }
        }
    }

    fn disable_clock_counter(&mut self) -> Result<(), Errno> {
        match self.clock.disable() {
            Ok(()) => {
                self.clock_enabled = CounterEnableState::Disabled;
                Ok(())
            }
            Err(error) => {
                self.clock_enabled = CounterEnableState::Unknown;
                Err(error)
            }
        }
    }

    fn enable_clock_counter(&mut self) -> Result<(), Errno> {
        match self.clock.enable() {
            Ok(()) => {
                self.clock_enabled = CounterEnableState::Enabled;
                Ok(())
            }
            Err(error) => {
                self.clock_enabled = CounterEnableState::Unknown;
                Err(error)
            }
        }
    }

    fn latch_private_execution_failure(
        &mut self,
        snapshot: PrivateExecutionSnapshot,
        operation: &'static str,
        source: Errno,
    ) -> PrivateExecutionTimerError {
        self.private_execution = Some(PrivateExecutionState::Failed(snapshot));
        PrivateExecutionTimerError::CounterOperation { operation, source }
    }

    fn latch_private_execution_inconsistency(
        &mut self,
        snapshot: PrivateExecutionSnapshot,
        detail: &'static str,
    ) -> PrivateExecutionTimerError {
        self.private_execution = Some(PrivateExecutionState::Failed(snapshot));
        PrivateExecutionTimerError::InconsistentState(detail)
    }

    fn fail_private_execution_and_disable_both(
        &mut self,
        snapshot: PrivateExecutionSnapshot,
        operation: &'static str,
        source: Errno,
    ) -> PrivateExecutionTimerError {
        self.private_execution = Some(PrivateExecutionState::Failed(snapshot));
        match self.disable_both_after_private_failure() {
            Ok(()) => PrivateExecutionTimerError::CounterOperation { operation, source },
            Err((recovery_operation, recovery_source)) => {
                PrivateExecutionTimerError::CounterOperationAndRecovery {
                    operation,
                    source,
                    recovery_operation,
                    recovery_source,
                }
            }
        }
    }

    /// Best-effort transition back to the only state safe for failed private
    /// execution: both counters disabled. Sampling must be disabled first so a
    /// notification cannot race a later clock disable.
    fn disable_both_after_private_failure(&mut self) -> Result<(), (&'static str, Errno)> {
        if let Err(error) = self.disable_sampling_counter() {
            self.timer_enabled = CounterEnableState::Unknown;
            return Err(("disable sampling counter during failure recovery", error));
        }
        if let Err(error) = self.disable_clock_counter() {
            self.clock_enabled = CounterEnableState::Unknown;
            return Err(("disable clock counter during failure recovery", error));
        }
        Ok(())
    }

    pub fn begin_suspend_for_private_execution(
        &mut self,
    ) -> Result<PrivateExecutionTimerSuspension, PrivateExecutionTimerError> {
        if let Some(state) = self.private_execution {
            return Err(PrivateExecutionTimerError::Busy {
                phase: state.phase(),
            });
        }

        let retained = self.retained_state();
        if retained.send_artificial_signal {
            return Err(PrivateExecutionTimerError::ArtificialSignalPending);
        }
        if retained.clock_enabled != CounterEnableState::Enabled {
            return Err(PrivateExecutionTimerError::InconsistentState(
                "non-resetting clock was not known enabled",
            ));
        }
        if retained.timer_enabled == CounterEnableState::Unknown {
            return Err(PrivateExecutionTimerError::InconsistentState(
                "sampling-counter enable state was unknown",
            ));
        }

        // Both reads occur while the tracee is stopped. They are repeated after
        // disabling the counters to prove that suspension did not alter either
        // count.
        let frozen_timer = self.timer.ctr_value().map_err(|source| {
            PrivateExecutionTimerError::CounterOperation {
                operation: "read sampling counter before suspension",
                source,
            }
        })?;
        if let Some(threshold) = retained.timer_notification_threshold {
            if frozen_timer >= threshold {
                return Err(PrivateExecutionTimerError::NotificationThresholdReached {
                    observed: frozen_timer,
                    threshold,
                });
            }
        } else if retained.timer_enabled == CounterEnableState::Enabled {
            return Err(PrivateExecutionTimerError::InconsistentState(
                "enabled sampling counter had no notification threshold",
            ));
        }
        let frozen_clock = self.clock.ctr_value_fast().map_err(|source| {
            PrivateExecutionTimerError::CounterOperation {
                operation: "read clock before suspension",
                source,
            }
        })?;

        let id = self.next_private_execution_id;
        self.next_private_execution_id =
            id.checked_add(1)
                .ok_or(PrivateExecutionTimerError::InconsistentState(
                    "private-execution token identity overflowed",
                ))?;
        let snapshot = PrivateExecutionSnapshot {
            id,
            retained,
            frozen_clock,
            frozen_timer,
        };
        self.private_execution = Some(PrivateExecutionState::Suspending(snapshot));

        Ok(PrivateExecutionTimerSuspension { snapshot })
    }

    pub fn complete_suspend_for_private_execution(
        &mut self,
        suspension: &PrivateExecutionTimerSuspension,
    ) -> Result<(), PrivateExecutionTimerError> {
        let snapshot = match self.private_execution {
            Some(PrivateExecutionState::Suspending(snapshot)) => snapshot,
            Some(state) => {
                return Err(PrivateExecutionTimerError::Busy {
                    phase: state.phase(),
                });
            }
            None => {
                return Err(PrivateExecutionTimerError::InconsistentState(
                    "no private-execution suspension was being prepared",
                ));
            }
        };
        if suspension.snapshot != snapshot {
            return Err(PrivateExecutionTimerError::TokenMismatch);
        }

        if let Err(source) = self.disable_sampling_counter() {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                "disable sampling counter for private execution",
                source,
            ));
        }
        if let Err(source) = self.disable_clock_counter() {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                "disable clock counter for private execution",
                source,
            ));
        }

        let stopped_clock = match self.clock.ctr_value_fast() {
            Ok(value) => value,
            Err(source) => {
                return Err(self.latch_private_execution_failure(
                    snapshot,
                    "read clock after suspension",
                    source,
                ));
            }
        };
        if stopped_clock != snapshot.frozen_clock {
            return Err(self.latch_private_execution_failure(
                snapshot,
                "verify frozen clock after suspension",
                Errno::EIO,
            ));
        }
        let stopped_timer = match self.timer.ctr_value() {
            Ok(value) => value,
            Err(source) => {
                return Err(self.latch_private_execution_failure(
                    snapshot,
                    "read sampling counter after suspension",
                    source,
                ));
            }
        };
        if stopped_timer != snapshot.frozen_timer {
            return Err(self.latch_private_execution_failure(
                snapshot,
                "verify frozen sampling counter after suspension",
                Errno::EIO,
            ));
        }

        self.private_execution = Some(PrivateExecutionState::Suspended(snapshot));
        Ok(())
    }

    pub fn restore_after_private_execution(
        &mut self,
        suspension: &PrivateExecutionTimerSuspension,
    ) -> Result<(), PrivateExecutionTimerError> {
        let snapshot = match self.private_execution {
            Some(PrivateExecutionState::Suspended(snapshot)) => snapshot,
            Some(state) => {
                return Err(PrivateExecutionTimerError::Busy {
                    phase: state.phase(),
                });
            }
            None => {
                return Err(PrivateExecutionTimerError::InconsistentState(
                    "no private-execution suspension was active",
                ));
            }
        };
        if suspension.snapshot != snapshot {
            return Err(PrivateExecutionTimerError::TokenMismatch);
        }

        let mut expected_suspended = snapshot.retained;
        expected_suspended.timer_enabled = CounterEnableState::Disabled;
        expected_suspended.clock_enabled = CounterEnableState::Disabled;
        if let Some(field) = expected_suspended.changed_field(&self.retained_state()) {
            return Err(self.latch_private_execution_inconsistency(snapshot, field));
        }
        let frozen_clock = match self.clock.ctr_value_fast() {
            Ok(value) => value,
            Err(source) => {
                return Err(self.latch_private_execution_failure(
                    snapshot,
                    "read frozen clock before restore",
                    source,
                ));
            }
        };
        if frozen_clock != snapshot.frozen_clock {
            return Err(self.latch_private_execution_inconsistency(
                snapshot,
                "non-resetting clock changed during private execution",
            ));
        }
        let frozen_timer = match self.timer.ctr_value() {
            Ok(value) => value,
            Err(source) => {
                return Err(self.latch_private_execution_failure(
                    snapshot,
                    "read frozen sampling counter before restore",
                    source,
                ));
            }
        };
        if frozen_timer != snapshot.frozen_timer {
            return Err(self.latch_private_execution_inconsistency(
                snapshot,
                "sampling counter changed during private execution",
            ));
        }

        self.private_execution = Some(PrivateExecutionState::Restoring(snapshot));
        if let Err(source) = self.enable_clock_counter() {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                "enable clock counter after private execution",
                source,
            ));
        }

        if snapshot.retained.timer_enabled == CounterEnableState::Enabled
            && let Err(source) = self.enable_sampling_counter()
        {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                "enable sampling counter after private execution",
                source,
            ));
        }

        // Enabling counters while the tracee remains stopped must not alter
        // either retained value. Verify that before making ordinary Timer APIs
        // available again.
        let restored_clock = match self.clock.ctr_value_fast() {
            Ok(value) => value,
            Err(source) => {
                return Err(self.fail_private_execution_and_disable_both(
                    snapshot,
                    "read clock after restore",
                    source,
                ));
            }
        };
        if restored_clock != snapshot.frozen_clock {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                "verify clock after restore",
                Errno::EIO,
            ));
        }
        let restored_timer = match self.timer.ctr_value() {
            Ok(value) => value,
            Err(source) => {
                return Err(self.fail_private_execution_and_disable_both(
                    snapshot,
                    "read sampling counter after restore",
                    source,
                ));
            }
        };
        if restored_timer != snapshot.frozen_timer {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                "verify sampling counter after restore",
                Errno::EIO,
            ));
        }

        if let Some(field) = snapshot.retained.changed_field(&self.retained_state()) {
            return Err(self.fail_private_execution_and_disable_both(
                snapshot,
                field,
                Errno::EPROTO,
            ));
        }

        self.private_execution = None;
        Ok(())
    }

    pub fn retire_private_execution_on_terminal(
        &mut self,
        suspension: &PrivateExecutionTimerSuspension,
    ) -> Result<(), PrivateExecutionTimerError> {
        let current = self.retained_state();
        let snapshot = terminal_private_execution_snapshot(
            self.private_execution,
            suspension,
            current,
        )?;

        // The tracee generation is terminal: reading, enabling, disabling, or
        // otherwise touching either perf fd is both unnecessary and unsafe.
        // Retain the frozen software snapshot solely for diagnostics and keep
        // the private-execution latch permanently closed.
        self.timer_enabled = CounterEnableState::Disabled;
        self.clock_enabled = CounterEnableState::Disabled;
        self.private_execution = Some(PrivateExecutionState::Terminal(snapshot));
        Ok(())
    }

    pub fn request_event(&mut self, evt: TimerEventRequest) -> Result<(), Errno> {
        self.ensure_timer_operation_allowed()?;
        let (delivery, notification) = match evt {
            TimerEventRequest::Precise(ticks) | TimerEventRequest::PreciseInstruction(ticks, _) => {
                (ticks, ticks.saturating_sub(get_pmu_config().skid_margin()))
            }
            TimerEventRequest::Imprecise(ticks) => (ticks, ticks),
        };
        if delivery == 0 {
            return Err(Errno::EINVAL); // bail before setting timer
        }
        self.send_artificial_signal = if notification <= SINGLESTEP_TIMEOUT_RCBS {
            // If there's an existing event making use of the timer counter,
            // we need to "overwrite" it the same way setting an actual RCB
            // notification does.
            self.disable_sampling_counter()?;
            self.timer_notification_threshold = None;
            true
        } else {
            self.timer.reset()?;
            self.timer.set_period(notification)?;
            self.enable_sampling_counter()?;
            self.timer_notification_threshold = Some(notification);
            false
        };
        let clock = self.read_clock() + delivery;
        self.event = match evt {
            TimerEventRequest::Precise(_) => ActiveEvent::Precise {
                clock_target: clock,
                offset: 0,
            },
            TimerEventRequest::PreciseInstruction(_, instr_offset) => ActiveEvent::Precise {
                clock_target: clock,
                offset: instr_offset,
            },
            TimerEventRequest::Imprecise(_) => ActiveEvent::Imprecise { clock_min: clock },
        };
        self.timer_status = EventStatus::Scheduled;
        Ok(())
    }

    pub fn observe_event(&mut self) {
        self.assert_timer_operation_allowed("observe_event");
        self.timer_status.tick()
    }

    pub fn schedule_cancellation(&mut self) {
        self.assert_timer_operation_allowed("schedule_cancellation");
        self.timer_status = EventStatus::Cancelled;
    }

    pub fn cancel(&mut self) -> Result<(), Errno> {
        self.ensure_timer_operation_allowed()?;
        self.disable_sampling_counter()
    }

    fn is_timer_generated_signal(signal: &libc::siginfo_t) -> bool {
        // The signal that gets sent is SIGPOLL. We reconfigured the signal
        // number, but the struct info is the same. Per the perf manpage, signal
        // notifications will come indicating either POLL_IN or POLL_HUP.
        signal.si_signo == MARKER_SIGNAL as i32
            && (signal.si_code == i32::from(libc::POLLIN)
                || signal.si_code == i32::from(libc::POLLHUP))
    }

    fn generated_signal(&self, signal: &libc::siginfo_t) -> bool {
        signal.si_signo == MARKER_SIGNAL as i32
            // If we sent an artificial signal, it doesn't have any siginfo
            && (self.send_artificial_signal
            // If not, the fd should match. This could possibly lead to a
            // collision, because an fd comparing-equal to this one in another
            // process could also send a signal. However, that it would also do so
            // as SIGSTKFLT is effectively not going to happen.
                || (Self::is_timer_generated_signal(signal)
                    && get_si_fd(signal) == self.timer.raw_fd()))
    }

    pub fn read_clock(&self) -> u64 {
        self.assert_timer_operation_allowed("read_clock");
        self.clock.ctr_value_fast().expect("Failed to read clock")
    }

    fn diagnostic_clock(&self) -> u64 {
        match self.private_execution {
            Some(state) => state.snapshot().frozen_clock,
            _ => self.clock.ctr_value_fast().expect("Failed to read clock"),
        }
    }

    pub fn finalize_requests(&self) {
        self.assert_timer_operation_allowed("finalize_requests");
        if self.send_artificial_signal {
            debug!("Sending artificial timer signal");

            // Give the guest a kick via an "artificial signal".  This gives us something
            // to handle in `handle_signal` and thus drives single-stepping.
            Errno::result(unsafe {
                libc::syscall(
                    libc::SYS_tgkill,
                    self.guest_pid.as_raw(),
                    self.guest_tid.as_raw(),
                    MARKER_SIGNAL as i32,
                )
            })
            .expect("Timer tgkill error indicates a bug");
        }
    }

    async fn handle_signal(
        &mut self,
        task: Stopped,
        step: &mut (dyn FnMut(Stopped) -> Result<Running, TraceError> + Send),
        observe: &mut (dyn FnMut(&Wait) -> Result<(), TraceError> + Send),
    ) -> Result<Stopped, HandleFailure> {
        self.assert_timer_operation_allowed("handle_signal");
        let signal = task.getsiginfo()?;
        if !self.generated_signal(&signal) {
            warn!(
                ?signal,
                "Passed a signal that wasn't for this timer, likely indicating a bug!",
            );
            return Err(HandleFailure::ImproperSignal(task));
        }

        match self.timer_status {
            EventStatus::Scheduled => panic!(
                "Timer event status should tick at least once before the signal \
                is handled. This is a bug!"
            ),
            EventStatus::Armed => {}
            EventStatus::Cancelled => {
                debug!("Delivered timer signal cancelled due to status");
                self.disable_timer_before_stepping();
                return Err(HandleFailure::Cancelled(task));
            }
        };

        // At this point, we've decided that a timer event is to be delivered.

        // Ensure any new timer signals don't mess with us while single-stepping
        self.disable_timer_before_stepping();

        // Last check to see if this an unexpected wakeup (a signal before the minimum expected)
        let ctr = self.read_clock();

        if let Some(additional_timer_request) = self.event.reschedule_if_spurious_wakeup(ctr) {
            debug!("Spurious wakeup - rescheduling new timer event");
            if let Err(errno) = self.request_event(additional_timer_request) {
                warn!(
                    "Attempted to reschedule a timer signal after an early wakeup, but failed with - {:?}. A panic will likely follow",
                    errno
                );
            } else {
                return Err(HandleFailure::Cancelled(task));
            };
        }

        // Before we drive the event to completion, clear `send_artificial_signal` flag so that:
        // - another signal isn't generated anytime Timer::finalize_requests() is called
        // - spurious SIGSTKFLTs aren't let errantly let through
        // Cancellations should prevent spurious timer events in any case.
        self.send_artificial_signal = false;

        match self.event {
            ActiveEvent::Precise {
                clock_target,
                offset,
            } => {
                self.attempt_single_step(task, ctr, clock_target, offset, step, observe)
                    .await
            }
            ActiveEvent::Imprecise { clock_min } => {
                debug!(
                    "Imprecise timer event delivered. Ctr val: {}, min val: {}",
                    ctr, clock_min
                );
                assert!(ctr >= clock_min, "ctr = {}, clock_min = {}", ctr, clock_min);
                Ok(task)
            }
        }
    }

    async fn attempt_single_step(
        &self,
        task: Stopped,
        ctr_initial: u64,
        target_rcb: u64,
        target_instr: u64,
        step: &mut (dyn FnMut(Stopped) -> Result<Running, TraceError> + Send),
        observe: &mut (dyn FnMut(&Wait) -> Result<(), TraceError> + Send),
    ) -> Result<Stopped, HandleFailure> {
        // The perf interrupt can arrive *past* the target when descheduling or
        // migration delays signal handling long enough for the actual skid to
        // exceed the margin. Single stepping cannot move the guest backward, so
        // record the overshoot and deliver the timer event at the observed
        // counter. The Tool can then account for the late event and either end
        // the timeslice or re-arm the next timer through its normal callback.
        if get_pmu_config().record_overshoot_if_past_target(ctr_initial, target_rcb) {
            warn!(
                "Precise timer interrupt arrived after target: actual {} > target {}; \
                 delivering timer event at the observed counter",
                ctr_initial, target_rcb
            );
            return Ok(task);
        }
        let mut current = ClockCounter::new(ctr_initial, 0, target_rcb);
        let max_single_step_count = get_pmu_config().max_single_step_count();
        assert!(
            target_rcb - current.rcbs() <= max_single_step_count,
            "Single steps from {} to {} requested ({} steps), but that exceeds the skid margin + minimum perf timer steps ({}). \
                This probably indicates a bug",
            current.rcbs(),
            target_rcb,
            (target_rcb - current.rcbs()),
            max_single_step_count
        );
        debug!(
            "Timer will single-step from ctr {} to {}",
            current, target_rcb
        );
        let mut task = task;
        loop {
            if !current
                .is_behind(target_rcb, target_instr)
                .expect("counter should increase monotonically and stay at target_rcb until equal. This is most likely a BUG with counter tracking")
            {
                break;
            }
            #[cfg(target_arch = "x86_64")]
            trace!(
                "[instruction]\n{}\n{}",
                crate::decoder::decode_instruction(&task)?,
                task.getregs()?
                    .display_with_options(RegDisplayOptions { multiline: true })
            );
            let wait = step(task)?.next_state().await?;
            observe(&wait)?;
            task = match wait {
                // a successful single step results in SIGTRAP stop
                Wait::Stopped(new_task, TraceEvent::Signal(Signal::SIGTRAP)) => new_task,
                wait => return Err(HandleFailure::Event(wait)),
            };
            current.single_step_with_clock(self.read_clock());
        }
        Ok(task)
    }

    /// Imagine our skid margin is 50 RCBs, and we set the timer for 5 RCBs.
    /// Since we step for 50, the timer will trigger multiple times unless we
    /// disable it before stepping. This would count as a state machine
    /// transition and errantly cancel the delivery of the timer event.
    fn disable_timer_before_stepping(&mut self) {
        self.disable_sampling_counter()
            .expect("Must be able to disable timer before stepping");
    }
}

#[cfg(target_os = "linux")]
fn get_si_fd(signal: &libc::siginfo_t) -> libc::c_int {
    // This almost certainly broken for anything other than linux (glibc?).
    //
    // The `libc` crate doesn't expose these fields properly, because the
    // current version was released before union support, and `siginfo_t` is a
    // messy enum/union, making this super fragile.
    //
    // `libc` has an accessor system in place, but only for a few particular
    // signal types as of right now. We could submit a PR for SIGPOLL/SIGIO, but
    // until then, this is copies the currently used accessor idea.

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct sifields_sigpoll {
        si_band: libc::c_long,
        si_fd: libc::c_int,
    }
    #[repr(C)]
    union sifields {
        _align_pointer: *mut libc::c_void,
        sigpoll: sifields_sigpoll,
    }
    #[repr(C)]
    struct siginfo_f {
        _siginfo_base: [libc::c_int; 3],
        sifields: sifields,
        padding: [libc::c_int; 24],
    }

    // These compile to no-op or unconditional runtime panic, which is good,
    // because code not using timers continues to work.
    assert_eq!(
        core::mem::size_of::<siginfo_f>(),
        core::mem::size_of_val(signal),
    );
    assert_eq!(
        core::mem::align_of::<siginfo_f>(),
        core::mem::align_of_val(signal),
    );

    unsafe {
        (*(signal as *const _ as *const siginfo_f))
            .sifields
            .sigpoll
            .si_fd
    }
}

#[cfg(test)]
mod tests {
    use reverie::Pid;
    use test_case::test_case;

    use super::ActiveEvent;
    use super::ClockCounter;
    use super::CounterEnableState;
    use super::EventStatus;
    #[cfg(target_arch = "x86_64")]
    use super::PmuConfig;
    use super::RetainedTimerState;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn amd_epyc_9d85_uses_reduced_skid_margin() {
        let config = PmuConfig::from_family_model(0x1A, 0x11);
        assert_eq!(config.raw_rcb_event(), 0x5100d1);
        assert_eq!(config.skid_margin(), 1_000);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn unknown_cpu_is_unavailable_to_fallible_in_guest_clock() {
        assert_eq!(PmuConfig::try_from_family_model(0x06, 0xCF), None);
        assert_eq!(PmuConfig::try_from_family_model(0xFF, 0x01), None);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn other_amd_cpus_keep_default_skid_margin() {
        for (family, model) in [(0x17, 0x71), (0x19, 0x61), (0x1A, 0x20)] {
            let config = PmuConfig::from_family_model(family, model);
            assert_eq!(config.raw_rcb_event(), 0x5100d1);
            assert_eq!(config.skid_margin(), 10_000);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn explicit_skid_margin_overrides_processor_default() {
        let config = PmuConfig::from_family_model(0x1A, 0x11).with_skid_margin_override(500);

        assert_eq!(config.raw_rcb_event(), 0x5100d1);
        assert_eq!(config.skid_margin(), 500);
        assert_eq!(config.max_single_step_count(), 505);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zero_skid_margin_forces_interrupt_at_target() {
        // The force-skid witness sets REVERIE_SKID_MARGIN_OVERRIDE=0. That path
        // runs through `with_skid_margin_override(0)`; assert the invariant it
        // relies on: a zero margin schedules the overflow interrupt *at* the
        // target RCB (request_event uses `ticks - skid_margin()`), so any
        // natural positive skid overshoots and trips the marker. Kept
        // env-free because process env is not thread-safe under the parallel
        // test runner.
        let config = PmuConfig::from_family_model(0x1A, 0x11).with_skid_margin_override(0);
        assert_eq!(config.skid_margin(), 0);
        // max_single_step_count() == skid_margin() + SINGLESTEP_TIMEOUT_RCBS (5).
        assert_eq!(config.max_single_step_count(), 5);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn skid_overshoot_marker_has_canonical_shape() {
        use super::SKID_OVERSHOOT_MARKER;
        // EPYC 9D85: skid margin 1000. Overshoot of 33_500 (the measured
        // baseline outlier) landing 500 RCB past a target of 33_000.
        let config = PmuConfig::from_family_model(0x1A, 0x11);
        let line = config.format_skid_overshoot_marker(33_500, 33_000);
        assert_eq!(
            line,
            format!(
                "{} rcb_actual=33500 rcb_target=33000 skid_margin=1000 overshoot=500",
                SKID_OVERSHOOT_MARKER
            )
        );
        // The token is what a retry harness greps for; keep it stable.
        assert!(line.starts_with(SKID_OVERSHOOT_MARKER));
        assert_eq!(SKID_OVERSHOOT_MARKER, "HERMIT_SKID_OVERSHOOT");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn skid_overshoot_marker_overshoot_is_saturating() {
        // A non-overshoot call (actual <= target) must never underflow.
        let config = PmuConfig::from_family_model(0x1A, 0x11);
        let line = config.format_skid_overshoot_marker(100, 200);
        assert!(line.contains("overshoot=0"));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn overshoot_decision_records_witness_counts_and_attributes_per_run() {
        // Drives real (rcb_actual, rcb_target) pairs through the *same*
        // decision-and-record method the supervisor calls in
        // `attempt_single_step`, and observes the process-global witness
        // counter — proving the behaviour (a genuine overshoot is recorded),
        // not merely the marker arithmetic. This test is the only writer of the
        // witness counter in this test binary, so draining residue first makes
        // it order-independent; env is untouched, so it is parallel-safe.
        let _ = reverie::take_skid_overshoot_count();

        // The exact CPU is irrelevant to the decision, which keys only on
        // `actual > target`; use EPYC 9D85 (default margin 1000).
        let config = PmuConfig::from_family_model(0x1A, 0x11);

        // --- Negative bracket: a non-overshoot must record nothing. ---
        // Interrupt delivered *at* the target, and *before* it.
        assert!(!config.record_overshoot_if_past_target(33_000, 33_000));
        assert!(!config.record_overshoot_if_past_target(32_500, 33_000));
        assert_eq!(
            reverie::take_skid_overshoot_count(),
            0,
            "no overshoot occurred, so the witness must be empty"
        );

        // --- Positive bracket: a genuine overshoot records exactly once. ---
        // 500 RCB past target: a real skid past the margin.
        assert!(config.record_overshoot_if_past_target(33_500, 33_000));
        assert_eq!(
            reverie::take_skid_overshoot_count(),
            1,
            "one real overshoot must record exactly one witness event"
        );

        // --- Counting: N genuine overshoots in one run count to N, and only
        // genuine ones are counted. Models the heavy-tailed skid (p99 < 1000
        // RCB, rare outliers into the tens of thousands). ---
        let overshoots = [1u64, 500, 10_000, 47_311];
        for &past in &overshoots {
            assert!(config.record_overshoot_if_past_target(33_000 + past, 33_000));
        }
        // Interleave a non-overshoot to prove it is not counted.
        assert!(!config.record_overshoot_if_past_target(33_000, 33_000));
        assert_eq!(
            reverie::take_skid_overshoot_count(),
            overshoots.len() as u64,
            "every genuine overshoot in run A must be counted, and only those"
        );

        // --- Per-run attribution: `take` reset makes runs disjoint, so a
        // second verify run sees only its own overshoots and run A's count does
        // not leak in. ---
        assert!(config.record_overshoot_if_past_target(33_001, 33_000));
        assert!(config.record_overshoot_if_past_target(90_000, 33_000));
        assert_eq!(
            reverie::take_skid_overshoot_count(),
            2,
            "run B is attributed only its own two overshoots"
        );
        // A run with zero overshoots (the common case) is attributed zero.
        assert_eq!(reverie::take_skid_overshoot_count(), 0);
    }

    #[test]
    fn private_execution_retains_every_timer_state_field_exactly() {
        let retained = RetainedTimerState {
            event: ActiveEvent::Precise {
                clock_target: 41,
                offset: 7,
            },
            timer_status: EventStatus::Armed,
            send_artificial_signal: false,
            timer_enabled: CounterEnableState::Enabled,
            clock_enabled: CounterEnableState::Enabled,
            timer_notification_threshold: Some(31),
            guest_pid: Pid::from_raw(101),
            guest_tid: Pid::from_raw(102),
        };
        assert_eq!(retained.changed_field(&retained), None);

        let changed = [
            (
                RetainedTimerState {
                    event: ActiveEvent::Imprecise { clock_min: 41 },
                    ..retained
                },
                "active event",
            ),
            (
                RetainedTimerState {
                    timer_status: EventStatus::Cancelled,
                    ..retained
                },
                "event status",
            ),
            (
                RetainedTimerState {
                    send_artificial_signal: true,
                    ..retained
                },
                "artificial-signal state",
            ),
            (
                RetainedTimerState {
                    timer_enabled: CounterEnableState::Disabled,
                    ..retained
                },
                "sampling-counter enable state",
            ),
            (
                RetainedTimerState {
                    clock_enabled: CounterEnableState::Unknown,
                    ..retained
                },
                "clock-counter enable state",
            ),
            (
                RetainedTimerState {
                    timer_notification_threshold: Some(32),
                    ..retained
                },
                "hardware notification threshold",
            ),
            (
                RetainedTimerState {
                    guest_pid: Pid::from_raw(103),
                    ..retained
                },
                "guest pid",
            ),
            (
                RetainedTimerState {
                    guest_tid: Pid::from_raw(104),
                    ..retained
                },
                "guest tid",
            ),
        ];

        for (actual, expected_field) in changed {
            assert_eq!(retained.changed_field(&actual), Some(expected_field));
        }
    }

    fn private_execution_fixture() -> (PrivateExecutionSnapshot, RetainedTimerState) {
        let retained = RetainedTimerState {
            event: ActiveEvent::Precise {
                clock_target: 400,
                offset: 3,
            },
            timer_status: EventStatus::Armed,
            send_artificial_signal: false,
            timer_enabled: CounterEnableState::Enabled,
            clock_enabled: CounterEnableState::Enabled,
            timer_notification_threshold: Some(397),
            guest_pid: Pid::from_raw(201),
            guest_tid: Pid::from_raw(202),
        };
        (
            PrivateExecutionSnapshot {
                id: 17,
                retained,
                frozen_clock: 1_337,
                frozen_timer: 211,
            },
            retained,
        )
    }

    #[test]
    fn terminal_retirement_accepts_every_owned_transition_phase_without_perf_access() {
        let (snapshot, retained) = private_execution_fixture();
        let token = PrivateExecutionTimerSuspension { snapshot };
        let phases = [
            PrivateExecutionState::Suspending(snapshot),
            PrivateExecutionState::Suspended(snapshot),
            PrivateExecutionState::Restoring(snapshot),
            PrivateExecutionState::Failed(snapshot),
        ];
        for phase in phases {
            let mut current = retained;
            // Hardware transition bookkeeping may be anywhere between enabled,
            // disabled, or unknown at terminal proof. No fd operation is
            // needed; all non-enable deterministic fields remain exact.
            current.timer_enabled = CounterEnableState::Unknown;
            current.clock_enabled = CounterEnableState::Disabled;
            assert_eq!(
                terminal_private_execution_snapshot(Some(phase), &token, current),
                Ok(snapshot),
                "terminal retirement rejected owned {} phase",
                phase.phase(),
            );
            assert_eq!(phase.snapshot().frozen_clock, 1_337);
        }
    }

    #[test]
    fn terminal_retirement_rejects_wrong_token_repeat_and_retained_state_drift() {
        let (snapshot, retained) = private_execution_fixture();
        let token = PrivateExecutionTimerSuspension { snapshot };
        let wrong = PrivateExecutionTimerSuspension {
            snapshot: PrivateExecutionSnapshot { id: 18, ..snapshot },
        };
        assert_eq!(
            terminal_private_execution_snapshot(
                Some(PrivateExecutionState::Suspended(snapshot)),
                &wrong,
                retained,
            ),
            Err(PrivateExecutionTimerError::TokenMismatch),
        );
        assert_eq!(
            terminal_private_execution_snapshot(
                Some(PrivateExecutionState::Terminal(snapshot)),
                &token,
                retained,
            ),
            Err(PrivateExecutionTimerError::Busy { phase: "terminal" }),
        );
        let drifted = RetainedTimerState {
            timer_notification_threshold: Some(398),
            ..retained
        };
        assert_eq!(
            terminal_private_execution_snapshot(
                Some(PrivateExecutionState::Failed(snapshot)),
                &token,
                drifted,
            ),
            Err(PrivateExecutionTimerError::InconsistentState(
                "hardware notification threshold"
            )),
        );
    }

    #[test_case(ClockCounter::new(0, 0, 10), 0, 1, Some(true))]
    #[test_case(ClockCounter::new(2, 100, 200), 3, 0, Some(true))]
    #[test_case(ClockCounter::new(1, 10, 200), 1, 11, Some(true))]
    #[test_case(ClockCounter::new(2, 100, 2), 3, 0, None)]
    #[test_case(ClockCounter::new(4, 4, 4), 4, 5, Some(true))]
    #[test_case(ClockCounter::new(4, 4, 4), 4, 3, Some(false))]
    #[test_case(ClockCounter::new(4, 4, 4), 4, 4, Some(false))]
    fn test_clock_counter_is_behind(
        counter: ClockCounter,
        target_rcb: u64,
        target_instr: u64,
        expected: Option<bool>,
    ) {
        assert_eq!(counter.is_behind(target_rcb, target_instr), expected);
    }

    #[test_case(ClockCounter::new(0, 0, 0), 0, (0, 1))]
    #[test_case(ClockCounter::new(0, 1, 0), 1, (0, 2))]
    #[test_case(ClockCounter::new(0, 1, 0), 2, (0, 2))]
    #[test_case(ClockCounter::new(0, 1, 1), 0, (0, 2))]
    #[test_case(ClockCounter::new(0, 1, 1), 1, (1, 0))]
    #[test_case(ClockCounter::new(0, 1, 1), 2, (2, 0))]
    #[test_case(ClockCounter::new(0, 1, 1), 3, (3, 0))]
    #[test_case(ClockCounter::new(10, 0, 11), 10, (10, 1))]
    #[test_case(ClockCounter::new(10, 1, 11), 10, (10, 2))]
    #[test_case(ClockCounter::new(10, 1, 11), 11, (11, 0))]
    #[test_case(ClockCounter::new(10, 1, 11), 12, (12, 0))]
    fn test_increment_counter_with_clock(
        mut counter: ClockCounter,
        new_clock: u64,
        expected: (u64, u64),
    ) {
        counter.single_step_with_clock(new_clock);
        assert_eq!((counter.rcbs, counter.instr), expected);
    }
}
