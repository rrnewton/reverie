#![cfg(feature = "private-crt")]

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[unsafe(export_name = "pl_tls")]
static CONTROL: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

#[unsafe(export_name = "pl_guest_arch_prctl")]
extern "C" fn arch_prctl(_: u64, _: u64, _: *mut std::ffi::c_void) {
    std::process::abort();
}

#[test]
fn missing_initial_record_function_refuses_before_startup() {
    for _ in 0..2 {
        let error = unsafe { reverie_liteinst_runtime::__prepare_private_startup() }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(error.to_string(), "private CRT provider unavailable");
        assert_eq!(CONTROL[4].load(Ordering::Acquire), 0);
    }
}
