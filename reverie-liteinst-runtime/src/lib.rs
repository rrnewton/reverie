#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("reverie-liteinst requires Linux x86-64");

#[doc(hidden)]
pub mod bootstrap;
#[doc(hidden)]
pub mod guest_log;
mod instruction_event;
mod owned_context;
mod owned_step;
mod syscall_event;
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use owned_context::__owned_step_observation;
mod patch_alloc;
mod protected_fd;
pub mod startup;
#[doc(hidden)]
pub mod stats;
mod straddler;
mod syscall_fallback;
mod syscall_mode;
mod timer;

pub use bootstrap::COORDINATOR_ENV;
pub use bootstrap::PreloadBootstrap;
pub use bootstrap::STATS_COORDINATOR_ENV;
pub use bootstrap::TOOL_PRELOAD_ENV;
pub use bootstrap::take_preload_bootstrap;
pub use guest_log::CapturedGuestLog;
pub use guest_log::GuestLog;
pub use guest_log::GuestLogWriter;
pub use reverie::liteinst_stats::LiteinstInstrumentationStats;
pub use reverie_rpc_transport::guest_log as retained_guest_log;
pub use stats::LiteinstBackendStatsSnapshot;
pub use stats::LiteinstBackendStatsSource;
pub use stats::LiteinstDispatchPath;
pub use stats::LiteinstPatchDecision;
pub use syscall_mode::SyscallMode;
pub use syscall_mode::SyscallModeStats;
pub use syscall_mode::syscall_mode_stats;
mod clock_control;
#[cfg(feature = "test-guest-log")]
#[doc(hidden)]
pub mod guest_log_fixture;
pub mod mapping;
pub mod rpc;
#[doc(hidden)]
pub mod runtime;
mod runtime_domain;
pub mod vdso;
#[doc(hidden)]
pub use clock_control::defer_private_constructor as __defer_private_constructor;
#[doc(hidden)]
pub use clock_control::reverie_liteinst_clock_constructor_begin as __clock_constructor_begin;
#[doc(hidden)]
pub use clock_control::reverie_liteinst_clock_constructor_finish as __clock_constructor_finish;
#[cfg(feature = "private-crt")]
mod private_startup;
mod tool_host;
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared reverie-preload built-in re-exports.
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use owned_context::__owned_syscall_capture;
#[cfg(feature = "test-owned-cpuid")]
pub use owned_context::__owned_syscall_evidence;
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use owned_context::__read_owned_clock_after_return;
#[cfg(feature = "test-owned-cpuid")]
pub use owned_context::OwnedSyscallEvidence;
/// Observed POSIX timer inventory. `Unavailable` is not an empty inventory, and
/// `Empty` does not prove that no POSIX timer was created since exec.
pub use owned_context::PosixTimerInventory;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use owned_context::stack::StackAllocation as __PrivateStackAllocation;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::prepare as __prepare_private_startup;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::AdapterStatus as __PrivateGnuAdapterStatus;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::GnuStatus as __PrivateGnuProviderStatus;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::ProviderError as __PrivateGnuError;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::Reservation as __PrivateGnuReservation;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::ReserveError as __PrivateGnuReserveError;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::StackInfo as __PrivateGnuStackInfo;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::StackRecord as __PrivateGnuStackRecord;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::Stage as __PrivateGnuStage;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::Startup as __PrivateGnuStartup;
#[cfg(feature = "private-crt")]
#[doc(hidden)]
pub use private_startup::provider::Status as __PrivateGnuStatus;
/// Shared `reverie-preload` built-in tool enum and getpid spoof constant.
///
/// These are re-exported verbatim so LiteInst and e9patch present the same
/// built-in surface; the same [`BuiltinTool`] value installs the same dispatcher
/// in both backends via `reverie_preload::install_builtin`.
pub use reverie_preload::BuiltinTool;
pub use reverie_preload::SPOOF_PID;
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review shared RuntimeConfig alt-stack re-exports.
/// `REVERIE_LITEINST_ALT_STACK` selector and parser for the shared
/// `reverie-preload` `RuntimeConfig` alt-stack knob.
pub use runtime::ALT_STACK_ENV;
pub use runtime::IN_GUEST_STAGE_STREAM_ENV;
pub use runtime::PROCESS_FORK_ENV;
/// `REVERIE_LITEINST_TOOL` values and parser for shared built-in selection.
pub use runtime::TOOL_PASSTHROUGH;
pub use runtime::TOOL_SPOOF_GETPID;
pub use runtime::alt_stack_from_env_value;
pub use runtime::builtin_tool_from_env_value;
pub use straddler::STRADDLER_STALENESS_TICKS_ENV;
pub use straddler::straddler_staleness_from_env_value;
#[cfg(feature = "test-owned-cpuid")]
pub use timer::__owned_timer_position;
#[cfg(feature = "test-owned-cpuid")]
pub use timer::OwnedTimerPosition;
#[cfg(any(test, feature = "test-tool-host-dispatch"))]
#[doc(hidden)]
pub use tool_host::__install_dispatch_only_tool_host;
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use tool_host::__install_owned_clocked_instruction_fixture;
#[cfg(feature = "test-owned-cpuid")]
pub use tool_host::__install_owned_compiled_step_fixture;
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use tool_host::__install_owned_cpuid_fixture;
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use tool_host::__install_owned_instruction_fixture;
#[cfg(feature = "test-owned-cpuid")]
pub use tool_host::__install_owned_precise_timer_fixture;
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub use tool_host::__install_owned_single_step_fixture;
#[cfg(feature = "test-owned-cpuid")]
pub use tool_host::__install_owned_syscall_timer_fixture;
#[cfg(feature = "test-tool-host-dispatch")]
#[doc(hidden)]
pub use tool_host::dispatch_observer;
pub use tool_host::install_tool;
pub use tool_host::install_tool_from_bootstrap;
pub use tool_host::install_tool_from_bootstrap_with_mode;
pub use tool_host::install_tool_owned_native_from_bootstrap;
pub use tool_host::install_tool_quiescent;
pub use tool_host::install_tool_with_mode;

#[global_allocator]
static PATCH_ALLOCATOR: patch_alloc::PatchAllocator = patch_alloc::PatchAllocator;

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-87): Review the inherited compatibility event channel.
/// Environment variable selecting an inherited descriptor for compatibility events.
///
/// When unset, compatibility events retain their standalone behavior and use
/// standard error.
pub const COMPAT_EVENT_FD_ENV: &str = "REVERIE_LITEINST_EVENT_FD";

/// Environment variable selecting a per-launch compatibility-event cookie.
///
/// A controller that sets [`COMPAT_EVENT_FD_ENV`] must also set this to a
/// nonzero decimal `u64`. The runtime removes both variables before guest code
/// starts and includes the cookie in every dedicated-channel record.
pub const COMPAT_EVENT_COOKIE_ENV: &str = "REVERIE_LITEINST_EVENT_COOKIE";

// TODO-HUMAN-REVIEW(#61): this constructor installs process-wide signal and seccomp state.
/// Initializes the preload runtime when selected by the launcher environment.
///
/// # Safety
///
/// The dynamic loader must call this exactly once before application threads
/// start. Calling it again would stack an irreversible seccomp filter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_liteinst_initialize() {
    if let Err(error) = runtime::initialize_from_environment() {
        eprintln!("reverie-liteinst initialization failed: {error}");
        unsafe {
            libc::_exit(127);
        }
    }
}

// TODO-HUMAN-REVIEW(PR-127): Review public per-site instrumentation counters.
/// Returns the number of SIGSYS deliveries observed at one syscall instruction.
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_site_trap_count(address: u64) -> u64 {
    runtime::site_counts(address).0
}

/// Returns the number of installed-hook callbacks observed at one syscall instruction.
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_site_hook_count(address: u64) -> u64 {
    runtime::site_counts(address).1
}

// TODO-HUMAN-REVIEW(PR-249): Review public fallback-surface observability counters.
/// Total syscalls that reached LiteInst's fail-closed escape surface.
///
/// The escape surface is the dispatch path for a trapped site the runtime could
/// not route to the Tool (un-patchable `SITE_FALLBACK`, or an unclaimable site),
/// which fails closed with `EOPNOTSUPP`. For Detcore this counts syscalls that
/// bypass the determinism tool, so it is the by-syscall-number analog of the
/// per-site `reverie_liteinst_site_trap_count`/`_hook_count` exports and the
/// direct counterpart of `reverie_e9patch_fallback_dispatch_count` (round 4).
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_fallback_dispatch_count() -> u64 {
    runtime::fallback_dispatch_count()
}

// TODO-HUMAN-REVIEW(PR-249): Review public fallback-surface observability counters.
/// Number of times syscall `number` reached LiteInst's fail-closed escape surface.
///
/// The per-syscall-number analog of the per-site counters, keyed by syscall
/// number to match `reverie_e9patch_fallback_syscall_count`. Returns `0` for a
/// negative number or one outside the tracked table; those are only reflected in
/// [`reverie_liteinst_fallback_dispatch_count`].
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_fallback_syscall_count(number: i64) -> u64 {
    runtime::fallback_syscall_count(number)
}

#[cfg(feature = "preload-constructor")]
#[used]
#[unsafe(link_section = ".init_array")]
static REVERIE_LITEINST_INIT: unsafe extern "C" fn() = reverie_liteinst_initialize;
