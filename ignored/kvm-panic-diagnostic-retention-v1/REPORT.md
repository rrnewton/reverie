# Fork panic diagnostic retention: author report

Scope: failure.rs only, adding RunFailure's private retained panic-cleanup records, retain_panic_cleanup, complete folding, and a separate additive test module. Root's concurrent `pub(crate) mod tool_panics;` declaration is present in BASE and preserved. Backend/runtime callers, worker-group records, payload catching, and propagation are root-owned and untouched by this task.

A retained record contains Option<Arc<Error>> plus Vec<PanicPayload>. retain_panic_cleanup constructs the complete record before locking and appends it without publication or notification. An incoming SharedFailure reuses its exact Arc; any other complete Error gets its own Arc without discarding wrappers or cleanup. RunFailure's mutex supplies Sync for Send-only panic payloads without changing Error or GlobalState bounds.

complete snapshots and clears only diagnostic Option markers while holding the record mutex. It releases that guard before traversing errors, dropping ordinary return values, combining results, or dropping superseded Arc references. Every entire record and every secondary payload stays owned until final RunFailure destruction. Neither retention nor completion invokes a Tool hook; publication behavior stays in the existing publish method. The whole record vector is never drained, replaced, or cleared under a mutex guard.

Each pending diagnostic is folded exactly once. If its exact Arc already occurs anywhere in the result's SharedFailure, WorkerFailure, Cleanup, WithCleanup, SignalEffects cause, or ExecWorkerTeardown tree, folding skips that record while retaining its payloads. Matching text or matching only a primary cause never suppresses a distinct aggregate. The final result still uses existing complete_after_failure selection so the first-published Arc and event retain authority. With no pending records, the prior result path is unchanged.

The diagnostic marker is consuming: passing a previously completed error back through complete preserves its existing causes without adding records again. An independent later complete call does not replay diagnostics already transferred to a prior result. The method's existing contract remains completion after all owned workers/processes have returned; concurrent or premature completion is not newly promised.

Five additive controls cover no-record success/error/cancellation behavior; exact first cause plus typed cleanup and opaque signal-effect ledger identity with cancellation and exec/phase wrappers; two same-text distinct records across repeated completion; exact shared-Arc suppression through six wrapper positions and shared-primary/different-cleanup retention; and a Send-but-not-Sync payload whose allocation survives publication, completion, and repeated completion before dropping only at final RunFailure destruction. A Drop-bearing successful result checks that conversion to failure destroys it outside all three relevant mutexes, and a recording global checks that publication does not hold the record mutex.

All original seven tests and their supporting test module are byte-identical (19,642 bytes, SHA256 3c9a0131395bce39dfc2f5d7c612c07cf8278d9603abbbd047af62bf4bf2eae4). No old assertion, tolerance, comparator, label, gate, or case was changed. The new ExecWorkerTeardown assertion checks the typed wrapper because its existing Display is transparent; no nonexistent display prefix is assumed.

Validation performed: source reads, byte comparison, hashing, and patch generation only. No compiler, formatter, tests, product execution, network, model, or SCM command. This author report is not independent review or passing runtime evidence.

Limitations: this is narrow fork diagnostic storage. The caller must publish, complete owned cleanup, notify ChildCompletion::Failed, store exactly one record for that fork, and only then resume the original payload. It does not repair root/direct/fork/thread routing by itself, capture cleanup panic boundaries, or preserve original join payloads by itself. Arbitrarily panicking secondary-payload destructors at eventual final RunFailure destruction remain outside this storage guarantee.

Exact new selectors:

- failure::panic_cleanup_tests::no_retained_panic_cleanup_preserves_existing_completion
- failure::panic_cleanup_tests::retained_panic_cleanup_keeps_first_cause_wrappers_and_signal_effect_identity
- failure::panic_cleanup_tests::retained_panic_cleanup_folds_two_distinct_records_once
- failure::panic_cleanup_tests::retained_panic_cleanup_skips_only_exact_already_returned_arcs
- failure::panic_cleanup_tests::retained_send_only_payload_outlives_publication_and_completion
