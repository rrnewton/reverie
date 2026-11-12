#![deny(warnings)]
//! Minimal consumer-owned LD_PRELOAD entry point.
//!
//! Build this example as a cdylib and put the resulting shared object in
//! LD_PRELOAD. Real consumers add their own site discovery and policy; the
//! standalone liteinst2 dependency only prepares patching machinery.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static PREPARED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn initialize() {
    PREPARED.store(
        liteinst2::patcher::prepare_live_patching().is_ok(),
        Ordering::Release,
    );
}

#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static INITIALIZER: unsafe extern "C" fn() = initialize;

/// Returns whether the preload constructor prepared live patching.
#[unsafe(no_mangle)]
pub extern "C" fn liteinst2_preload_is_prepared() -> bool {
    PREPARED.load(Ordering::Acquire)
}
