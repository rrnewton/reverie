Private panic consumer composition evidence

All phases accepted: True. Exact selected declarations: 52; declarations in accepted test phases: 52. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.577 |
| compile-1 | 0 | True | 12.282 |
| check-1 | 0 | True | 1.150 |
| inventory-1 | 0 | True | 0.017 |
| test-owned-future-1 | 0 | True | 0.018 |
| test-panic-drain-1 | 0 | True | 0.047 |
| test-handler-scratch-1 | 0 | True | 0.043 |
| test-injected-boundary-1 | 0 | True | 0.093 |
| test-worker-panic-1 | 0 | True | 0.308 |
| test-normal-cleanup-1 | 0 | True | 0.058 |
| test-handler-destruction-1 | 0 | True | 0.019 |
| test-caught-worker-1 | 0 | True | 0.047 |
| test-action-preparation-1 | 0 | True | 0.282 |
| test-boundary-1 | 0 | True | 0.017 |
| test-boundary-cleanup-1 | 0 | True | 0.057 |
| test-fork-cleanup-1 | 0 | True | 0.100 |
| test-thread-cleanup-1 | 0 | True | 0.102 |
| test-partial-start-1 | 0 | True | 0.071 |
| test-cancelled-cleanup-1 | 0 | True | 0.352 |
| test-exec-cleanup-1 | 0 | True | 0.046 |
| test-cleanup-storage-1 | 0 | True | 0.036 |
| test-cancellation-locks-1 | 0 | True | 0.018 |
| test-callback-completion-1 | 0 | True | 0.045 |
| test-consuming-panics-1 | 0 | True | 0.036 |
| test-tool-panics-1 | 0 | True | 0.034 |
| test-retained-panics-1 | 0 | True | 0.047 |
| test-public-panics-1 | 0 | True | 0.103 |
| test-fork-join-panics-1 | 0 | True | 0.048 |
| test-instruction-panics-1 | 0 | True | 0.109 |
| test-instruction-constructor-1 | 0 | True | 0.109 |

Source manifest SHA256 08a451d4eb6483050c28ad06acc51d751b5d969ec9409b47b53b47bdef9b58fd; plan SHA256 2e66342573ba892fe5c2aa9b7b7e7f9321d042327ffc974239ae564db9217d45. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet records selected callback, consuming-hook, concrete-owner and child-join controls. It does not certify actual host-spawn refusal, every unexpected setup/report-hook unwind, externally cancelled pending futures, recursively panicking final payload destructors, or the external caller's global/scheduler cleanup after a propagated root panic. Production activation of the earlier entry-owner preparation and the remaining determinism/parity work are separate. Native and external code review still apply before landing.
