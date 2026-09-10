#![cfg(feature = "private-crt")]

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

static CALLS: AtomicUsize = AtomicUsize::new(0);

#[unsafe(export_name = "pl_take_initial")]
extern "C" fn take_initial() -> *const std::ffi::c_void {
    CALLS.fetch_add(1, Ordering::SeqCst);
    std::ptr::null()
}

#[test]
fn partial_private_provider_cannot_consume_initial_record() {
    for _ in 0..2 {
        let error = unsafe { reverie_liteinst_runtime::__prepare_private_startup() }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(error.to_string(), "private CRT provider unavailable");
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    }
}
