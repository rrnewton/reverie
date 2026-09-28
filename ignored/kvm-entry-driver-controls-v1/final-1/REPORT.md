Private entry ownership and physical join evidence

All phases accepted: True. Exact selected declarations: 198; declarations in accepted test phases: 198. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
| format-1 | 0 | True | 0.664 |
| compile-1 | 0 | True | 27.316 |
| check-1 | 0 | True | 1.277 |
| inventory-1 | 0 | True | 0.039 |
| test-owned-future-1 | 0 | True | 0.043 |
| test-panic-drain-1 | 0 | True | 0.040 |
| test-handler-scratch-1 | 0 | True | 0.037 |
| test-injected-boundary-1 | 0 | True | 0.100 |
| test-worker-panic-1 | 0 | True | 0.278 |
| test-normal-cleanup-1 | 0 | True | 0.040 |
| test-handler-destruction-1 | 0 | True | 0.036 |
| test-caught-worker-1 | 0 | True | 0.043 |
| test-action-preparation-1 | 0 | True | 0.315 |
| test-boundary-1 | 0 | True | 0.044 |
| test-boundary-cleanup-1 | 0 | True | 0.076 |
| test-fork-cleanup-1 | 0 | True | 0.110 |
| test-thread-cleanup-1 | 0 | True | 0.100 |
| test-partial-start-1 | 0 | True | 0.075 |
| test-cancelled-cleanup-1 | 0 | True | 0.302 |
| test-exec-cleanup-1 | 0 | True | 0.079 |
| test-cleanup-storage-1 | 0 | True | 0.049 |
| test-cancellation-locks-1 | 0 | True | 0.039 |
| test-callback-completion-1 | 0 | True | 0.028 |
| test-consuming-panics-1 | 0 | True | 0.020 |
| test-tool-panics-1 | 0 | True | 0.039 |
| test-retained-panics-1 | 0 | True | 0.039 |
| test-public-panics-1 | 0 | True | 0.110 |
| test-fork-join-panics-1 | 0 | True | 0.044 |
| test-instruction-panics-1 | 0 | True | 0.103 |
| test-instruction-constructor-1 | 0 | True | 0.098 |
| test-entry-gate-1 | 0 | True | 0.078 |
| test-entry-owner-1 | 0 | True | 0.045 |
| test-entry-routing-1 | 0 | True | 0.048 |
| test-failure-completion-1 | 0 | True | 0.147 |
| test-runtime-entry-1 | 0 | True | 0.146 |
| test-vm-entry-1 | 0 | True | 0.148 |
| test-exec-private-wait-1 | 0 | True | 0.209 |
| test-entry-capture-wake-1 | 0 | True | 0.046 |
| test-existing-join-1 | 0 | True | 0.043 |
| test-existing-join-2 | 0 | True | 0.046 |
| test-existing-join-3 | 0 | True | 0.047 |
| test-existing-join-4 | 0 | True | 0.036 |
| test-existing-join-5 | 0 | True | 0.039 |
| test-existing-join-6 | 0 | True | 0.059 |
| test-existing-join-7 | 0 | True | 0.089 |
| test-existing-join-8 | 0 | True | 0.044 |
| test-existing-join-9 | 0 | True | 0.042 |
| test-existing-join-10 | 0 | True | 0.043 |
| test-clock-controls-1 | 0 | True | 0.092 |
| test-memory-controls-1 | 0 | True | 0.055 |
| test-host-mask-controls-1 | 0 | True | 0.058 |
| test-entry-operations-1 | 0 | True | 0.026 |
| test-rpc-join-observation-1 | 0 | True | 0.048 |
| test-rpc-join-observation-2 | 0 | True | 0.037 |
| test-rpc-join-observation-3 | 0 | True | 0.045 |
| test-public-entry-1 | 0 | True | 0.795 |
| test-main-entry-1 | 0 | True | 0.392 |
| test-spawn-entry-1 | 0 | True | 0.261 |

Source manifest SHA256 f74effb8842eaa3d9a8178c96fdde293cf210bfc36fe824a1f38f8c3f096dd55; plan SHA256 2878729a4f9926cc570a6a9a1f9406cf2e703f69cef8c8940eccedf81e77d846. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet records selected callback, consuming-hook, concrete-owner and child-join controls, plus four public Tool memory-failure declarations (24 subcases), eight closed-main declarations (eleven lifetimes) and three guest-spawn declarations (two public ELF refusal routes and one healthy host-spawn neighbor). The main controls separately observe shared descriptor access, RUN/MASK sites and clock begin/interval creation; descriptor access counts are not numeric ioctl counts. These selected paths do not certify every unexpected setup/report-hook unwind, externally cancelled pending futures, recursively panicking final payload destructors, all multi-owner/publication and pending-hypercall paths, or the external caller's global/scheduler cleanup after a propagated root panic. The selected controls do not certify every production owner path or backend parity. The remaining determinism/parity work is separate. Native and external code review still apply before landing.
