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

#[test]
fn active_control_without_arch_prctl_cannot_prepare_private_startup() {
    CONTROL[4].store(2, Ordering::Release);
    for _ in 0..2 {
        let error = unsafe { reverie_liteinst_runtime::__prepare_private_startup() }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(error.to_string(), "private CRT provider unavailable");
        assert_eq!(
            CONTROL
                .each_ref()
                .map(|field| field.load(Ordering::Acquire)),
            [0, 0, 0, 0, 2, 0, 0]
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    }
}
