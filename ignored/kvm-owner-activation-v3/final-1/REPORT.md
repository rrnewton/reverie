Private entry driver activation evidence

All phases accepted: True. Exact selected declarations: 99; declarations in accepted test phases: 99. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.650 |
| compile-1 | 0 | True | 0.129 |
| check-1 | 0 | True | 0.235 |
| inventory-1 | 0 | True | 0.066 |
| test-owned-future-1 | 0 | True | 0.037 |
| test-panic-drain-1 | 0 | True | 0.052 |
| test-handler-scratch-1 | 0 | True | 0.054 |
| test-injected-boundary-1 | 0 | True | 0.120 |
| test-worker-panic-1 | 0 | True | 0.651 |
| test-normal-cleanup-1 | 0 | True | 0.147 |
| test-handler-destruction-1 | 0 | True | 0.128 |
| test-caught-worker-1 | 0 | True | 0.168 |
| test-action-preparation-1 | 0 | True | 0.450 |
| test-boundary-1 | 0 | True | 0.045 |
| test-boundary-cleanup-1 | 0 | True | 0.083 |
| test-fork-cleanup-1 | 0 | True | 0.117 |
| test-thread-cleanup-1 | 0 | True | 0.391 |
| test-partial-start-1 | 0 | True | 0.138 |
| test-cancelled-cleanup-1 | 0 | True | 1.097 |
| test-exec-cleanup-1 | 0 | True | 0.075 |
| test-cleanup-storage-1 | 0 | True | 0.040 |
| test-cancellation-locks-1 | 0 | True | 0.046 |
| test-callback-completion-1 | 0 | True | 0.046 |
| test-consuming-panics-1 | 0 | True | 0.065 |
| test-tool-panics-1 | 0 | True | 0.047 |
| test-retained-panics-1 | 0 | True | 0.045 |
| test-public-panics-1 | 0 | True | 0.112 |
| test-fork-join-panics-1 | 0 | True | 0.047 |
| test-instruction-panics-1 | 0 | True | 0.460 |
| test-instruction-constructor-1 | 0 | True | 0.448 |
| test-entry-gate-1 | 0 | True | 0.086 |
| test-entry-owner-1 | 0 | True | 0.054 |
| test-entry-routing-1 | 0 | True | 0.042 |
| test-failure-completion-1 | 0 | True | 0.149 |
| test-runtime-entry-1 | 0 | True | 0.152 |
| test-vm-entry-1 | 0 | True | 0.149 |
| test-exec-private-wait-1 | 0 | True | 0.079 |
| test-entry-capture-wake-1 | 0 | True | 0.046 |

Source manifest SHA256 047930881d84b518c15b4afed4e86e89a776abaf8dc944e3b3f22795de290362; plan SHA256 d8886f8202876a35a2320b19614af54b5cd554e08dd4435be6b87240b32fe266. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet records selected callback, consuming-hook, concrete-owner and child-join controls. It does not certify actual host-spawn refusal, every unexpected setup/report-hook unwind, externally cancelled pending futures, recursively panicking final payload destructors, or the external caller's global/scheduler cleanup after a propagated root panic. The selected controls do not certify every production owner path or backend parity. The remaining determinism/parity work is separate. Native and external code review still apply before landing.
