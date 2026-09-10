#![cfg(feature = "private-crt")]

#[test]
fn missing_private_provider_refuses_before_startup() {
    for _ in 0..2 {
        let error = unsafe { reverie_liteinst_runtime::__prepare_private_startup() }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(error.to_string(), "private CRT provider unavailable");
    }
}
