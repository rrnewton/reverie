Private entry ownership and physical join evidence

All phases accepted: False. Exact selected declarations: 230; declarations in accepted test phases: 212. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.755 |
| compile-1 | 0 | True | 30.729 |
| check-1 | 0 | True | 2.407 |
| inventory-1 | 0 | True | 0.039 |
| test-owned-future-1 | 0 | True | 0.041 |
| test-panic-drain-1 | 0 | True | 0.048 |
| test-handler-scratch-1 | 0 | True | 0.044 |
| test-injected-boundary-1 | 0 | True | 0.107 |
| test-worker-panic-1 | 0 | True | 0.297 |
| test-normal-cleanup-1 | 0 | True | 0.040 |
| test-handler-destruction-1 | 0 | True | 0.042 |
| test-caught-worker-1 | 0 | True | 0.056 |
| test-action-preparation-1 | 0 | True | 0.332 |
| test-boundary-1 | 0 | True | 0.048 |
| test-boundary-cleanup-1 | 0 | True | 0.074 |
| test-fork-cleanup-1 | 0 | True | 0.127 |
| test-thread-cleanup-1 | 0 | True | 0.114 |
| test-partial-start-1 | 0 | True | 0.080 |
| test-cancelled-cleanup-1 | 0 | True | 0.318 |
| test-exec-cleanup-1 | 0 | True | 0.079 |
| test-cleanup-storage-1 | 0 | True | 0.047 |
| test-cancellation-locks-1 | 0 | True | 0.041 |
| test-callback-completion-1 | 0 | True | 0.054 |
| test-consuming-panics-1 | 0 | True | 0.041 |
| test-tool-panics-1 | 0 | True | 0.050 |
| test-retained-panics-1 | 0 | True | 0.041 |
| test-public-panics-1 | 0 | True | 0.108 |
| test-fork-join-panics-1 | 0 | True | 0.038 |
| test-instruction-panics-1 | 0 | True | 0.111 |
| test-instruction-constructor-1 | 0 | True | 0.110 |
| test-entry-gate-1 | 0 | True | 0.089 |
| test-entry-owner-1 | 0 | True | 0.047 |
| test-entry-routing-1 | 0 | True | 0.047 |
| test-failure-completion-1 | 0 | True | 0.143 |
| test-runtime-entry-1 | 0 | True | 0.136 |
| test-vm-entry-1 | 0 | True | 0.135 |
| test-exec-private-wait-1 | 0 | True | 0.207 |
| test-entry-capture-wake-1 | 0 | True | 0.040 |
| test-existing-join-1 | 0 | True | 0.061 |
| test-existing-join-2 | 0 | True | 0.042 |
| test-existing-join-3 | 0 | True | 0.032 |
| test-existing-join-4 | 0 | True | 0.046 |
| test-existing-join-5 | 0 | True | 0.047 |
| test-existing-join-6 | 0 | True | 0.037 |
| test-existing-join-7 | 0 | True | 0.079 |
| test-existing-join-8 | 0 | True | 0.037 |
| test-existing-join-9 | 0 | True | 0.041 |
| test-existing-join-10 | 0 | True | 0.064 |
| test-clock-controls-1 | 0 | True | 0.090 |
| test-memory-controls-1 | 0 | True | 0.039 |
| test-host-mask-controls-1 | 0 | True | 0.049 |
| test-entry-operations-1 | 0 | True | 0.040 |
| test-rpc-join-observation-1 | 0 | True | 0.036 |
| test-rpc-join-observation-2 | 0 | True | 0.041 |
| test-rpc-join-observation-3 | 0 | True | 0.046 |
| test-public-entry-1 | 0 | True | 0.954 |
| test-main-entry-1 | 0 | True | 0.381 |
| test-spawn-entry-1 | 0 | True | 0.182 |
| test-hypercall-entry-1 | 0 | True | 0.250 |
| test-race-entry-1 | 0 | True | 0.178 |
| test-multi-owner-entry-1 | 1 | False | 5.677 |
| test-action-entry-1 | 101 | False | 0.655 |
| test-wait-entry-1 | 101 | False | 0.429 |
| test-eintr-entry-1 | 0 | True | 0.076 |
| test-cleanup-entry-1 | 0 | True | 1.730 |
| test-existing-parking-wait-1 | 0 | True | 0.133 |

Source manifest SHA256 2fba215af555a69ad6f26c75d154f91b5ea262a40138485de490629dcc2d6aea; plan SHA256 867fad6f225240a42a4f9f68b258f3a24835ae552f5415f5a71bb4b1af517c96. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet retains all 198 prior selected controls and adds 32 declarations for pending hypercall responses, final activation, real A/B/C owner publication, four prepared action callers, wait registration, real foreign SIGURG interruption, signal cleanup and the existing parking helper. Their author reports distinguish actual public owners from constructed action dispatch and injected cleanup errors from kernel-generated failures. Exact pass counts are declaration counts, not guest comparison counts or total ioctl counts. These controls do not certify the complete production caller, all scheduler/Linux semantics, Hermit/Detcore parity, changed address mappings or a global fork snapshot. Native and external source/evidence review and consumer qualification remain required before landing.

