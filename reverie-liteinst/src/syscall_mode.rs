use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Syscall interception for an explicitly installed in-process Tool.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyscallMode {
    /// Existing seccomp interception with optional-site instrumentation.
    #[default]
    SeccompWithPatching,
    /// Native x86-64 SUD without guest rewriting; see `install_tool_with_mode`.
    UserDispatchWithoutPatching,
}

/// Always-enabled process counters, including attempted work, not only success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallModeStats {
    pub validated_sud: u64,
    pub deferred_sud: u64,
    pub planning_attempts: u64,
    pub patch_attempts: u64,
    pub installed_patches: u64,
    pub vdso_rewrite_attempts: u64,
}

static SUD_ONLY: AtomicBool = AtomicBool::new(false);
static VALIDATED_SUD: AtomicU64 = AtomicU64::new(0);
static DEFERRED_SUD: AtomicU64 = AtomicU64::new(0);
static PLANNING: AtomicU64 = AtomicU64::new(0);
static PATCHING: AtomicU64 = AtomicU64::new(0);
static INSTALLED: AtomicU64 = AtomicU64::new(0);
static VDSO: AtomicU64 = AtomicU64::new(0);

pub fn syscall_mode_stats() -> SyscallModeStats {
    SyscallModeStats {
        validated_sud: VALIDATED_SUD.load(Ordering::Relaxed),
        deferred_sud: DEFERRED_SUD.load(Ordering::Relaxed),
        planning_attempts: PLANNING.load(Ordering::Relaxed),
        patch_attempts: PATCHING.load(Ordering::Relaxed),
        installed_patches: INSTALLED.load(Ordering::Relaxed),
        vdso_rewrite_attempts: VDSO.load(Ordering::Relaxed),
    }
}

pub(crate) fn select(mode: SyscallMode) {
    SUD_ONLY.store(
        mode == SyscallMode::UserDispatchWithoutPatching,
        Ordering::Release,
    );
}

pub(crate) fn sud_only() -> bool {
    SUD_ONLY.load(Ordering::Acquire)
}

pub(crate) fn record_sud() {
    VALIDATED_SUD.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_deferred() {
    DEFERRED_SUD.fetch_add(1, Ordering::Relaxed);
}

fn attempt(counter: &AtomicU64) -> io::Result<()> {
    counter.fetch_add(1, Ordering::Relaxed);
    if sud_only() {
        Err(io::Error::other(
            "guest patching is disabled in SUD-only mode",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn planning() -> io::Result<()> {
    attempt(&PLANNING)
}

pub(crate) fn patching() -> io::Result<()> {
    attempt(&PATCHING)
}

pub(crate) fn vdso_rewrite() -> io::Result<()> {
    attempt(&VDSO)
}

pub(crate) fn installed() {
    INSTALLED.fetch_add(1, Ordering::Relaxed);
}
