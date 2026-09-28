Private panic consumer composition evidence

All phases accepted: True. Exact selected declarations: 50; declarations in accepted test phases: 50. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.559 |
| compile-1 | 0 | True | 21.551 |
| check-1 | 0 | True | 2.577 |
| inventory-1 | 0 | True | 0.035 |
| test-owned-future-1 | 0 | True | 0.038 |
| test-panic-drain-1 | 0 | True | 0.051 |
| test-handler-scratch-1 | 0 | True | 0.036 |
| test-injected-boundary-1 | 0 | True | 0.104 |
| test-worker-panic-1 | 0 | True | 0.286 |
| test-normal-cleanup-1 | 0 | True | 0.043 |
| test-handler-destruction-1 | 0 | True | 0.044 |
| test-caught-worker-1 | 0 | True | 0.042 |
| test-action-preparation-1 | 0 | True | 0.281 |
| test-boundary-1 | 0 | True | 0.045 |
| test-boundary-cleanup-1 | 0 | True | 0.068 |
| test-fork-cleanup-1 | 0 | True | 0.108 |
| test-thread-cleanup-1 | 0 | True | 0.104 |
| test-partial-start-1 | 0 | True | 0.076 |
| test-cancelled-cleanup-1 | 0 | True | 0.317 |
| test-exec-cleanup-1 | 0 | True | 0.075 |
| test-cleanup-storage-1 | 0 | True | 0.045 |
| test-cancellation-locks-1 | 0 | True | 0.044 |
| test-callback-completion-1 | 0 | True | 0.029 |
| test-consuming-panics-1 | 0 | True | 0.043 |
| test-tool-panics-1 | 0 | True | 0.046 |
| test-retained-panics-1 | 0 | True | 0.040 |
| test-public-panics-1 | 0 | True | 0.113 |
| test-fork-join-panics-1 | 0 | True | 0.050 |
| test-instruction-panics-1 | 0 | True | 0.113 |

Source manifest SHA256 c0e6e34bcb02d37c3a988a1bbbd2d37999f353dd0411b2a7860eb32997cd81e9; plan SHA256 2464519e12fcf5f2f6be9a39911aea135a2a60ca6c422495b705834e3f3e8bc0. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet records selected callback, consuming-hook, concrete-owner and child-join controls. It does not certify actual host-spawn refusal, every unexpected setup/report-hook unwind, externally cancelled pending futures, recursively panicking final payload destructors, or the external caller's global/scheduler cleanup after a propagated root panic. Production activation of the earlier entry-owner preparation and the remaining determinism/parity work are separate. Native and external code review still apply before landing.
