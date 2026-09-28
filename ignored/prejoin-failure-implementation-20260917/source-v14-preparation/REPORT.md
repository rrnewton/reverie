# Reverie source v14

The sole source increment is in the native test-support helper. The thread_state and thread_state_mut accessors now compile only with the explicit native-test-support feature used by the Hermit combined controls. The unused NativeToolOwner::tool accessor is removed. The production Guest trait, module gates, feature export, existing controls, assertions, selections and counts are unchanged.

This corrects the default-feature dead_code warning observed in Cargo structured stdout at v12 and v13. No lint suppression was added. The previous v13 library and static_elf executables and inventories remain attributed to v13; this preparation is not an execution claim. The v12 37 native passes and v13 workspace all-features Clippy/format pass remain preserved. Default-feature Clippy plus incremental compile/inventory and actual VM/static_elf qualification are pending for v14.

The cargo-v9 original no-warning claim was incorrect and its explicit correction is retained in cargo-v9/COMPILER-DIAGNOSTICS-CORRECTION.md. The previous actual Claude request for changes, limited arbitrary-unwind support, pre-existing successful-exec sibling RPC cancellation gap and virtual SIGCHLD limitation remain disclosed.
