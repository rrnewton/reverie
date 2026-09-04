use std::sync::OnceLock;

use tracing::debug;

use super::perf::Event;
use crate::SKID_OVERSHOOT_MARKER;

#[cfg(all(test, target_arch = "x86_64"))]
mod logging_tests;

/// We refuse to schedule a "perf timeout" for this or fewer RCBs, instead
/// choosing to directly single step. This is because I am somewhat paranoid
/// about perf event throttling, which isn't well-documented.
pub const SINGLESTEP_TIMEOUT_RCBS: u64 = 5;

#[cfg(target_arch = "x86_64")]
const AMD_RCB_EVENT: u64 = 0x5100d1;
#[cfg(target_arch = "x86_64")]
const AMD_DEFAULT_SKID_MARGIN: u64 = 10_000;
#[cfg(target_arch = "x86_64")]
const AMD_EPYC_9D85_SKID_MARGIN: u64 = 1_000;

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

pub fn get_pmu_config() -> &'static PmuConfig {
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
    pub fn from_family_model(family_id: u8, model_id: u8) -> Self {
        Self::try_from_family_model(family_id, model_id).unwrap_or_else(|| match family_id {
            0x06 => panic!("Unsupported Intel processor model: {:#x}", model_id),
            family => panic!(
                "Unsupported processor family, model: ({:#x},{:#x})",
                family, model_id
            ),
        })
    }

    #[cfg(target_arch = "x86_64")]
    pub fn try_from_family_model(family_id: u8, model_id: u8) -> Option<Self> {
        // based on rr's PerfCounters_x86.h and PerfCounters.cc
        let (rcb_event, skid_margin) = match family_id {
            // Intel
            0x06 => match model_id {
                0x1A | 0x1E | 0x2E => (0x5101c4, 100),        // Intel Nehalem
                0x25 | 0x2C | 0x2F => (0x5101c4, 100),        // Intel Westmere
                0x2A | 0x2D | 0x3E => (0x5101c4, 100),        // Intel Sandy Bridge
                0x3A => (0x5101c4, 100),                      // Intel Ivy Bridge
                0x3C | 0x3F | 0x45 | 0x46 => (0x5101c4, 100), // Intel Haswell
                0x3D | 0x47 | 0x4F | 0x56 => (0x5101c4, 100), // Intel Broadwell
                0x4E | 0x55 | 0x5E => (0x5101c4, 100),        // Intel Skylake
                0x8E | 0x9E => (0x5101c4, 100),               // Intel Kabylake
                0xA5 | 0xA6 => (0x5101c4, 100),               // Intel Cometlake
                0x8D => (0x5101c4, 100),                      // Intel Tiger Lake
                0x9A => (0x5101c4, 125),                      // Intel Alder Lake
                0x8F => (0x5101c4, 125),                      // Intel Sapphire Rapids
                0x86 => (0x5101c4, 100),                      // Intel Icelake
                _ => return None,
            },
            // Turin EPYC family 1Ah model 11h has p99 skid of 384 RCBs. A 1K
            // performance margin avoids excessive single stepping. Rare larger
            // overshoots are reported and delivered at the observed counter.
            0x1A if model_id == 0x11 => (AMD_RCB_EVENT, AMD_EPYC_9D85_SKID_MARGIN),
            // Other Zen CPUs keep rr's 10K guard because they have exhibited rare large skid.
            0x17 | 0x19 | 0x1A => (AMD_RCB_EVENT, AMD_DEFAULT_SKID_MARGIN),
            _ => return None,
        };

        Some(Self {
            rcb_event,
            skid_margin,
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
    /// process-global witness counter via [`crate::record_skid_overshoot`],
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
            crate::record_skid_overshoot();
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
/// configuration. Callers should install overrides before spawning a tracer.
pub fn set_pmu_config(config: PmuConfig) -> Result<(), PmuConfig> {
    PMU_CONFIG.set(config)
}

/// Returns true if the current CPU supports precise_ip.
#[cfg(target_arch = "x86_64")]
pub fn has_precise_ip() -> bool {
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
        target: "reverie_ptrace::timer",
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
pub fn has_precise_ip() -> bool {
    // Assume, for now, that aarch64 can use precise_ip.
    true
}
