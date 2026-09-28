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

Source manifest SHA256 b15981b925b6bf1bac0ddd49660c570512ed39d82bebb1831ab455af255c17b1; plan SHA256 ef78867c4acb0276504c55d1be3d34940a6a7dbe83dd3a1c9d4e249fc6bc5973. The manifest binds the 14 prior source/lock records, failure/owned_future.rs, vm/worker_panic_tests.rs, and the selected runtime/failure_tests.rs. It is an explicit 17-file private composition record, not a full transitive source/tool closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. No SCM, network, external model, Hermit, Detcore, or parity execution is part of this plan. The complete batch does include KVM guest execution in existing boundary and parking paths. Four selected declarations call backend_at_completed_tool_boundary: production_wrapper_cleans_fork_after_boundary_restore_failure, production_wrapper_cleans_tool_thread_after_boundary_restore_failure, cancelled_child_cleanup_retains_real_errors_and_ordinary_cancellation, and exec_teardown_disposition_survives_unstarted_child_cleanup. Their fixture multiplicities are one, one, eight, and one respectively. Each fixture explicitly enters the vCPU to obtain a getpid syscall hypercall; the production wrappers also use parking paths, so eleven fixture calls are not an exact total KVM_RUN ioctl count. No exact ioctl count was measured. The three new vm::worker_panic_tests controls use KVM setup ioctls and host futures/threads without entering KVM_RUN. No full integration, parity, or landing approval follows from this batch.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

The current integration remains incomplete: an unpublished RuntimeError can still be lost if the actual callback destructor panics before drive_handler returns. The worker controls do not test that originating failure path or an actual host-spawn failure. Root/fork panic recovery and ordinary consumer panic routing remain outside this increment. These limitations must remain attached to any interpretation of the results.

Scope correction: this report supersedes the execution-scope paragraph in final-1/REPORT.md, whose no-intentional-KVM_RUN claim was incorrect. The old report and its target remain preserved. All source identities, compiler outputs, phase commands, raw statuses, exact selected counts and retained executable bytes are unchanged. No phase was rerun to make this correction. The old packet.py report template contains the same incorrect scope sentence and must not be reused unchanged.
