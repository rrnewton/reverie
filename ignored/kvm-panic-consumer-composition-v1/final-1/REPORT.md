Private panic consumer composition evidence

All phases accepted: True. Exact selected declarations: 26; declarations in accepted test phases: 26. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.259 |
| compile-1 | 0 | True | 12.529 |
| check-1 | 0 | True | 1.601 |
| inventory-1 | 0 | True | 0.039 |
| test-owned-future-1 | 0 | True | 0.021 |
| test-panic-drain-1 | 0 | True | 0.132 |
| test-handler-scratch-1 | 0 | True | 0.017 |
| test-injected-boundary-1 | 0 | True | 0.534 |
| test-worker-panic-1 | 0 | True | 0.303 |
| test-normal-cleanup-1 | 0 | True | 0.050 |
| test-handler-destruction-1 | 0 | True | 0.167 |
| test-caught-worker-1 | 0 | True | 0.209 |
| test-action-preparation-1 | 0 | True | 0.756 |
| test-boundary-1 | 0 | True | 0.045 |
| test-boundary-cleanup-1 | 0 | True | 0.078 |
| test-fork-cleanup-1 | 0 | True | 0.142 |
| test-thread-cleanup-1 | 0 | True | 0.091 |
| test-partial-start-1 | 0 | True | 0.223 |
| test-cancelled-cleanup-1 | 0 | True | 1.416 |
| test-exec-cleanup-1 | 0 | True | 0.189 |
| test-cleanup-storage-1 | 0 | True | 0.044 |
| test-cancellation-locks-1 | 0 | True | 0.019 |

Source manifest SHA256 b15981b925b6bf1bac0ddd49660c570512ed39d82bebb1831ab455af255c17b1; plan SHA256 ef78867c4acb0276504c55d1be3d34940a6a7dbe83dd3a1c9d4e249fc6bc5973. The manifest binds the 14 prior source/lock records, failure/owned_future.rs, vm/worker_panic_tests.rs, and the selected runtime/failure_tests.rs. It is an explicit 17-file private composition record, not a full transitive source/tool closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. No SCM, network, model, Hermit guest or intentional KVM_RUN phase is part of this plan. Some controls require actual KVM setup ioctls and host threads. No parity or landing approval follows from these host controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

The current integration remains incomplete: an unpublished RuntimeError can still be lost if the actual callback destructor panics before drive_handler returns. The worker controls do not test that originating failure path or an actual host-spawn failure. Root/fork panic recovery and ordinary consumer panic routing remain outside this increment. These limitations must remain attached to any interpretation of the results.
