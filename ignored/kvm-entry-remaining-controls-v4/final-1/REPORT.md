Private entry ownership and physical join evidence

All phases accepted: True. Exact selected declarations: 231; declarations in accepted test phases: 231. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.765 |
| compile-1 | 0 | True | 25.689 |
| check-1 | 0 | True | 1.513 |
| format-check-1 | 0 | True | 1.995 |
| clippy-1 | 0 | True | 10.519 |
| inventory-1 | 0 | True | 0.042 |
| test-multi-owner-entry-1 | 0 | True | 0.138 |
| test-multi-owner-entry-2 | 0 | True | 0.137 |
| test-multi-owner-entry-3 | 0 | True | 0.148 |
| test-multi-owner-entry-4 | 0 | True | 0.160 |
| test-owned-future-1 | 0 | True | 0.031 |
| test-panic-drain-1 | 0 | True | 0.048 |
| test-handler-scratch-1 | 0 | True | 0.040 |
| test-injected-boundary-1 | 0 | True | 0.102 |
| test-worker-panic-1 | 0 | True | 0.268 |
| test-normal-cleanup-1 | 0 | True | 0.044 |
| test-handler-destruction-1 | 0 | True | 0.039 |
| test-caught-worker-1 | 0 | True | 0.050 |
| test-action-preparation-1 | 0 | True | 0.315 |
| test-boundary-1 | 0 | True | 0.045 |
| test-boundary-cleanup-1 | 0 | True | 0.090 |
| test-fork-cleanup-1 | 0 | True | 0.119 |
| test-thread-cleanup-1 | 0 | True | 0.114 |
| test-partial-start-1 | 0 | True | 0.093 |
| test-cancelled-cleanup-1 | 0 | True | 0.370 |
| test-exec-cleanup-1 | 0 | True | 0.087 |
| test-cleanup-storage-1 | 0 | True | 0.046 |
| test-cancellation-locks-1 | 0 | True | 0.048 |
| test-callback-completion-1 | 0 | True | 0.047 |
| test-consuming-panics-1 | 0 | True | 0.048 |
| test-tool-panics-1 | 0 | True | 0.051 |
| test-retained-panics-1 | 0 | True | 0.053 |
| test-public-panics-1 | 0 | True | 0.108 |
| test-fork-join-panics-1 | 0 | True | 0.047 |
| test-instruction-panics-1 | 0 | True | 0.113 |
| test-instruction-constructor-1 | 0 | True | 0.107 |
| test-entry-gate-1 | 0 | True | 0.085 |
| test-entry-owner-1 | 0 | True | 0.051 |
| test-entry-routing-1 | 0 | True | 0.041 |
| test-failure-completion-1 | 0 | True | 0.139 |
| test-runtime-entry-1 | 0 | True | 0.141 |
| test-vm-entry-1 | 0 | True | 0.133 |
| test-exec-private-wait-1 | 0 | True | 0.164 |
| test-entry-capture-wake-1 | 0 | True | 0.038 |
| test-existing-join-1 | 0 | True | 0.038 |
| test-existing-join-2 | 0 | True | 0.043 |
| test-existing-join-3 | 0 | True | 0.044 |
| test-existing-join-4 | 0 | True | 0.048 |
| test-existing-join-5 | 0 | True | 0.050 |
| test-existing-join-6 | 0 | True | 0.044 |
| test-existing-join-7 | 0 | True | 0.072 |
| test-existing-join-8 | 0 | True | 0.033 |
| test-existing-join-9 | 0 | True | 0.042 |
| test-existing-join-10 | 0 | True | 0.048 |
| test-clock-controls-1 | 0 | True | 0.089 |
| test-memory-controls-1 | 0 | True | 0.055 |
| test-host-mask-controls-1 | 0 | True | 0.046 |
| test-entry-operations-1 | 0 | True | 0.041 |
| test-rpc-join-observation-1 | 0 | True | 0.043 |
| test-rpc-join-observation-2 | 0 | True | 0.046 |
| test-rpc-join-observation-3 | 0 | True | 0.045 |
| test-public-entry-1 | 0 | True | 0.837 |
| test-main-entry-1 | 0 | True | 0.447 |
| test-spawn-entry-1 | 0 | True | 0.193 |
| test-hypercall-entry-1 | 0 | True | 0.246 |
| test-race-entry-1 | 0 | True | 0.175 |
| test-action-entry-1 | 0 | True | 0.720 |
| test-wait-entry-1 | 0 | True | 0.442 |
| test-eintr-entry-1 | 0 | True | 0.142 |
| test-cleanup-entry-1 | 0 | True | 1.723 |
| test-existing-parking-wait-1 | 0 | True | 0.142 |
| test-existing-peer-cancellation-1 | 0 | True | 0.041 |

Source manifest SHA256 600d428e6f8df8cbe7b3ec333ac8c05949e9fd26ee526f6363c46dcc1d023e39; plan SHA256 147d203fa28162d759908cf72850315afb762bde77bf50d827901659de475d87. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet retains all 198 prior selected controls, adds 32 declarations and also selects the existing peer-cancellation status regression for pending hypercall responses, final activation, real A/B/C owner publication, four prepared action callers, wait registration, real foreign SIGURG interruption, signal cleanup and the existing parking helper. Their author reports distinguish actual public owners from constructed action dispatch and injected cleanup errors from kernel-generated failures. Exact pass counts are declaration counts, not guest comparison counts or total ioctl counts. These controls do not certify the complete production caller, all scheduler/Linux semantics, Hermit/Detcore parity, changed address mappings or a global fork snapshot. Native and external source/evidence review and consumer qualification remain required before landing.

