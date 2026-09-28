Private entry ownership and physical join evidence

All phases accepted: True. Exact selected declarations: 231; declarations in accepted test phases: 231. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.741 |
| compile-1 | 0 | True | 7.427 |
| check-1 | 0 | True | 0.120 |
| inventory-1 | 0 | True | 0.038 |
| test-multi-owner-entry-1 | 0 | True | 0.140 |
| test-multi-owner-entry-2 | 0 | True | 0.130 |
| test-multi-owner-entry-3 | 0 | True | 0.150 |
| test-multi-owner-entry-4 | 0 | True | 0.145 |
| test-owned-future-1 | 0 | True | 0.048 |
| test-panic-drain-1 | 0 | True | 0.044 |
| test-handler-scratch-1 | 0 | True | 0.039 |
| test-injected-boundary-1 | 0 | True | 0.120 |
| test-worker-panic-1 | 0 | True | 0.304 |
| test-normal-cleanup-1 | 0 | True | 0.035 |
| test-handler-destruction-1 | 0 | True | 0.041 |
| test-caught-worker-1 | 0 | True | 0.043 |
| test-action-preparation-1 | 0 | True | 0.313 |
| test-boundary-1 | 0 | True | 0.042 |
| test-boundary-cleanup-1 | 0 | True | 0.076 |
| test-fork-cleanup-1 | 0 | True | 0.117 |
| test-thread-cleanup-1 | 0 | True | 0.120 |
| test-partial-start-1 | 0 | True | 0.080 |
| test-cancelled-cleanup-1 | 0 | True | 0.336 |
| test-exec-cleanup-1 | 0 | True | 0.092 |
| test-cleanup-storage-1 | 0 | True | 0.042 |
| test-cancellation-locks-1 | 0 | True | 0.048 |
| test-callback-completion-1 | 0 | True | 0.044 |
| test-consuming-panics-1 | 0 | True | 0.045 |
| test-tool-panics-1 | 0 | True | 0.045 |
| test-retained-panics-1 | 0 | True | 0.046 |
| test-public-panics-1 | 0 | True | 0.107 |
| test-fork-join-panics-1 | 0 | True | 0.052 |
| test-instruction-panics-1 | 0 | True | 0.103 |
| test-instruction-constructor-1 | 0 | True | 0.115 |
| test-entry-gate-1 | 0 | True | 0.081 |
| test-entry-owner-1 | 0 | True | 0.050 |
| test-entry-routing-1 | 0 | True | 0.043 |
| test-failure-completion-1 | 0 | True | 0.144 |
| test-runtime-entry-1 | 0 | True | 0.138 |
| test-vm-entry-1 | 0 | True | 0.133 |
| test-exec-private-wait-1 | 0 | True | 0.208 |
| test-entry-capture-wake-1 | 0 | True | 0.049 |
| test-existing-join-1 | 0 | True | 0.046 |
| test-existing-join-2 | 0 | True | 0.054 |
| test-existing-join-3 | 0 | True | 0.038 |
| test-existing-join-4 | 0 | True | 0.045 |
| test-existing-join-5 | 0 | True | 0.038 |
| test-existing-join-6 | 0 | True | 0.046 |
| test-existing-join-7 | 0 | True | 0.083 |
| test-existing-join-8 | 0 | True | 0.041 |
| test-existing-join-9 | 0 | True | 0.039 |
| test-existing-join-10 | 0 | True | 0.036 |
| test-clock-controls-1 | 0 | True | 0.084 |
| test-memory-controls-1 | 0 | True | 0.046 |
| test-host-mask-controls-1 | 0 | True | 0.054 |
| test-entry-operations-1 | 0 | True | 0.049 |
| test-rpc-join-observation-1 | 0 | True | 0.042 |
| test-rpc-join-observation-2 | 0 | True | 0.042 |
| test-rpc-join-observation-3 | 0 | True | 0.047 |
| test-public-entry-1 | 0 | True | 0.834 |
| test-main-entry-1 | 0 | True | 0.438 |
| test-spawn-entry-1 | 0 | True | 0.186 |
| test-hypercall-entry-1 | 0 | True | 0.225 |
| test-race-entry-1 | 0 | True | 0.160 |
| test-action-entry-1 | 0 | True | 0.680 |
| test-wait-entry-1 | 0 | True | 0.378 |
| test-eintr-entry-1 | 0 | True | 0.073 |
| test-cleanup-entry-1 | 0 | True | 1.797 |
| test-existing-parking-wait-1 | 0 | True | 0.147 |
| test-existing-peer-cancellation-1 | 0 | True | 0.033 |

Source manifest SHA256 69cce12356eb02bd7f7b0de7c529347ab3cf5cd6b1340a42551d227effe72ebf; plan SHA256 bab8e139ea8a6634917fab686425d5e53d1113aac5c8d518a4354e8c688719cc. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet retains all 198 prior selected controls, adds 32 declarations and also selects the existing peer-cancellation status regression for pending hypercall responses, final activation, real A/B/C owner publication, four prepared action callers, wait registration, real foreign SIGURG interruption, signal cleanup and the existing parking helper. Their author reports distinguish actual public owners from constructed action dispatch and injected cleanup errors from kernel-generated failures. Exact pass counts are declaration counts, not guest comparison counts or total ioctl counts. These controls do not certify the complete production caller, all scheduler/Linux semantics, Hermit/Detcore parity, changed address mappings or a global fork snapshot. Native and external source/evidence review and consumer qualification remain required before landing.

