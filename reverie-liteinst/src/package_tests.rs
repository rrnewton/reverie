use std::any::TypeId;

#[test]
fn legacy_paths_keep_runtime_type_identity() {
    macro_rules! same_type {
        ($name:ident) => {
            assert_eq!(
                TypeId::of::<crate::$name>(),
                TypeId::of::<reverie_liteinst_runtime::$name>()
            );
        };
    }
    same_type!(PreloadBootstrap);
    same_type!(GuestLog);
    same_type!(GuestLogWriter);
    same_type!(CapturedGuestLog);
    same_type!(LiteinstInstrumentationStats);
    same_type!(LiteinstBackendStatsSnapshot);
    same_type!(LiteinstBackendStatsSource);
    same_type!(LiteinstDispatchPath);
    same_type!(LiteinstPatchDecision);
    same_type!(SyscallMode);
    same_type!(SyscallModeStats);
    same_type!(BuiltinTool);
    same_type!(PosixTimerInventory);
    assert_eq!(
        TypeId::of::<crate::startup::AuxvSnapshot>(),
        TypeId::of::<reverie_liteinst_runtime::startup::AuxvSnapshot>()
    );
}

#[test]
fn facade_still_owns_backend_and_observer_types() {
    assert_eq!(
        std::any::type_name::<crate::PreloadTool>(),
        "reverie_liteinst::PreloadTool"
    );
    assert_eq!(
        std::any::type_name::<crate::LiteinstBackend>(),
        "reverie_liteinst::backend::LiteinstBackend"
    );
    assert!(
        std::any::type_name::<crate::run_evidence::RunObserver>()
            .starts_with("reverie_liteinst::backend::")
    );
    let _: fn(&mut std::process::Command, bool) = crate::set_guest_alt_stack;
    let _: fn(&mut std::process::Command, bool) = crate::set_guest_process_forks;
    let _: unsafe extern "C" fn() = crate::reverie_liteinst_initialize;
}
