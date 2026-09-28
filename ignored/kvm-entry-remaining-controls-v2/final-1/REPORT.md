Private entry ownership and physical join evidence

All phases accepted: False. Exact selected declarations: 230; declarations in accepted test phases: 226. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.787 |
| compile-1 | 0 | True | 26.517 |
| check-1 | 0 | True | 0.856 |
| inventory-1 | 0 | True | 0.044 |
| test-owned-future-1 | 0 | True | 0.034 |
| test-panic-drain-1 | 0 | True | 0.046 |
| test-handler-scratch-1 | 0 | True | 0.040 |
| test-injected-boundary-1 | 0 | True | 0.105 |
| test-worker-panic-1 | 0 | True | 0.294 |
| test-normal-cleanup-1 | 0 | True | 0.033 |
| test-handler-destruction-1 | 0 | True | 0.042 |
| test-caught-worker-1 | 0 | True | 0.048 |
| test-action-preparation-1 | 0 | True | 0.307 |
| test-boundary-1 | 0 | True | 0.045 |
| test-boundary-cleanup-1 | 0 | True | 0.083 |
| test-fork-cleanup-1 | 0 | True | 0.126 |
| test-thread-cleanup-1 | 0 | True | 0.091 |
| test-partial-start-1 | 0 | True | 0.072 |
| test-cancelled-cleanup-1 | 0 | True | 0.368 |
| test-exec-cleanup-1 | 0 | True | 0.109 |
| test-cleanup-storage-1 | 0 | True | 0.041 |
| test-cancellation-locks-1 | 0 | True | 0.038 |
| test-callback-completion-1 | 0 | True | 0.040 |
| test-consuming-panics-1 | 0 | True | 0.054 |
| test-tool-panics-1 | 0 | True | 0.061 |
| test-retained-panics-1 | 0 | True | 0.051 |
| test-public-panics-1 | 0 | True | 0.110 |
| test-fork-join-panics-1 | 0 | True | 0.033 |
| test-instruction-panics-1 | 0 | True | 0.118 |
| test-instruction-constructor-1 | 0 | True | 0.105 |
| test-entry-gate-1 | 0 | True | 0.082 |
| test-entry-owner-1 | 0 | True | 0.047 |
| test-entry-routing-1 | 0 | True | 0.043 |
| test-failure-completion-1 | 0 | True | 0.150 |
| test-runtime-entry-1 | 0 | True | 0.156 |
| test-vm-entry-1 | 0 | True | 0.132 |
| test-exec-private-wait-1 | 0 | True | 0.217 |
| test-entry-capture-wake-1 | 0 | True | 0.050 |
| test-existing-join-1 | 0 | True | 0.045 |
| test-existing-join-2 | 0 | True | 0.055 |
| test-existing-join-3 | 0 | True | 0.044 |
| test-existing-join-4 | 0 | True | 0.042 |
| test-existing-join-5 | 0 | True | 0.043 |
| test-existing-join-6 | 0 | True | 0.049 |
| test-existing-join-7 | 0 | True | 0.085 |
| test-existing-join-8 | 0 | True | 0.050 |
| test-existing-join-9 | 0 | True | 0.046 |
| test-existing-join-10 | 0 | True | 0.040 |
| test-clock-controls-1 | 0 | True | 0.091 |
| test-memory-controls-1 | 0 | True | 0.055 |
| test-host-mask-controls-1 | 0 | True | 0.052 |
| test-entry-operations-1 | 0 | True | 0.047 |
| test-rpc-join-observation-1 | 0 | True | 0.044 |
| test-rpc-join-observation-2 | 0 | True | 0.041 |
| test-rpc-join-observation-3 | 0 | True | 0.049 |
| test-public-entry-1 | 0 | True | 0.889 |
| test-main-entry-1 | 0 | True | 0.436 |
| test-spawn-entry-1 | 0 | True | 0.190 |
| test-hypercall-entry-1 | 0 | True | 0.236 |
| test-race-entry-1 | 0 | True | 0.163 |
| test-multi-owner-entry-1 | 101 | False | 5.134 |
| test-action-entry-1 | 0 | True | 0.688 |
| test-wait-entry-1 | 0 | True | 0.417 |
| test-eintr-entry-1 | 0 | True | 0.077 |
| test-cleanup-entry-1 | 0 | True | 1.788 |
| test-existing-parking-wait-1 | 0 | True | 0.148 |

Source manifest SHA256 0439fc5c36aaa59fe805e29cc20ac5d3b00b59cfd452764ffb598fbeb0a456ed; plan SHA256 5e1993087262fdac04fb7e9ee641c330a5383fb4646ed84de0865f94b4f9eb2b. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet retains all 198 prior selected controls and adds 32 declarations for pending hypercall responses, final activation, real A/B/C owner publication, four prepared action callers, wait registration, real foreign SIGURG interruption, signal cleanup and the existing parking helper. Their author reports distinguish actual public owners from constructed action dispatch and injected cleanup errors from kernel-generated failures. Exact pass counts are declaration counts, not guest comparison counts or total ioctl counts. These controls do not certify the complete production caller, all scheduler/Linux semantics, Hermit/Detcore parity, changed address mappings or a global fork snapshot. Native and external source/evidence review and consumer qualification remain required before landing.

