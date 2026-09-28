Private entry ownership and physical join evidence

All phases accepted: True. Exact selected declarations: 183; declarations in accepted test phases: 183. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.661 |
| compile-1 | 0 | True | 0.104 |
| check-1 | 0 | True | 0.094 |
| inventory-1 | 0 | True | 0.016 |
| test-owned-future-1 | 0 | True | 0.050 |
| test-panic-drain-1 | 0 | True | 0.020 |
| test-handler-scratch-1 | 0 | True | 0.050 |
| test-injected-boundary-1 | 0 | True | 0.120 |
| test-worker-panic-1 | 0 | True | 0.330 |
| test-normal-cleanup-1 | 0 | True | 0.028 |
| test-handler-destruction-1 | 0 | True | 0.045 |
| test-caught-worker-1 | 0 | True | 0.018 |
| test-action-preparation-1 | 0 | True | 0.304 |
| test-boundary-1 | 0 | True | 0.051 |
| test-boundary-cleanup-1 | 0 | True | 0.082 |
| test-fork-cleanup-1 | 0 | True | 0.138 |
| test-thread-cleanup-1 | 0 | True | 0.096 |
| test-partial-start-1 | 0 | True | 0.046 |
| test-cancelled-cleanup-1 | 0 | True | 0.279 |
| test-exec-cleanup-1 | 0 | True | 0.053 |
| test-cleanup-storage-1 | 0 | True | 0.023 |
| test-cancellation-locks-1 | 0 | True | 0.057 |
| test-callback-completion-1 | 0 | True | 0.027 |
| test-consuming-panics-1 | 0 | True | 0.038 |
| test-tool-panics-1 | 0 | True | 0.024 |
| test-retained-panics-1 | 0 | True | 0.023 |
| test-public-panics-1 | 0 | True | 0.079 |
| test-fork-join-panics-1 | 0 | True | 0.035 |
| test-instruction-panics-1 | 0 | True | 0.101 |
| test-instruction-constructor-1 | 0 | True | 0.111 |
| test-entry-gate-1 | 0 | True | 0.077 |
| test-entry-owner-1 | 0 | True | 0.027 |
| test-entry-routing-1 | 0 | True | 0.045 |
| test-failure-completion-1 | 0 | True | 0.126 |
| test-runtime-entry-1 | 0 | True | 0.143 |
| test-vm-entry-1 | 0 | True | 0.136 |
| test-exec-private-wait-1 | 0 | True | 0.218 |
| test-entry-capture-wake-1 | 0 | True | 0.018 |
| test-existing-join-1 | 0 | True | 0.034 |
| test-existing-join-2 | 0 | True | 0.048 |
| test-existing-join-3 | 0 | True | 0.043 |
| test-existing-join-4 | 0 | True | 0.018 |
| test-existing-join-5 | 0 | True | 0.045 |
| test-existing-join-6 | 0 | True | 0.018 |
| test-existing-join-7 | 0 | True | 0.051 |
| test-existing-join-8 | 0 | True | 0.018 |
| test-existing-join-9 | 0 | True | 0.017 |
| test-existing-join-10 | 0 | True | 0.036 |
| test-clock-controls-1 | 0 | True | 0.086 |
| test-memory-controls-1 | 0 | True | 0.025 |
| test-host-mask-controls-1 | 0 | True | 0.027 |
| test-entry-operations-1 | 0 | True | 0.046 |
| test-rpc-join-observation-1 | 0 | True | 0.019 |
| test-rpc-join-observation-2 | 0 | True | 0.037 |
| test-rpc-join-observation-3 | 0 | True | 0.023 |

Source manifest SHA256 6ce3110dbbd6dac16b9a9c640c5a0d47d40f1ecc2264fbb69336252e68cb3dd1; plan SHA256 fc138b771c9593502e3ca17659b35f64c8852d45d2b9343747aa67789fa931e1. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet records selected callback, consuming-hook, concrete-owner and child-join controls. It does not certify actual guest-worker host-spawn refusal, every unexpected setup/report-hook unwind, externally cancelled pending futures, recursively panicking final payload destructors, or the external caller's global/scheduler cleanup after a propagated root panic. The selected controls do not certify every production owner path or backend parity. The remaining determinism/parity work is separate. Native and external code review still apply before landing.
