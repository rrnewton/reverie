#![cfg(feature = "private-crt")]

use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[unsafe(export_name = "pl_tls")]
static CONTROL: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static CALLS: AtomicUsize = AtomicUsize::new(0);

#[unsafe(export_name = "pl_take_initial")]
extern "C" fn take_initial() -> *const std::ffi::c_void {
    CALLS.fetch_add(1, Ordering::SeqCst);
    std::ptr::null()
}

#[unsafe(export_name = "pl_guest_arch_prctl")]
extern "C" fn arch_prctl(_: u64, _: u64, _: *mut std::ffi::c_void) {
    std::process::abort();
}

#[test]
fn available_symbols_do_not_supply_private_ownership() {
    for _ in 0..2 {
        let error = unsafe { reverie_liteinst_runtime::__prepare_private_startup() }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "owned instruction native profile is not admitted"
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
        assert_eq!(CONTROL[4].load(Ordering::Acquire), 0);
    }
}
