A/B/C control correction — source author packet, unexecuted

Only reverie-kvm/src/vm/entry_multi_owner_tests.rs changed. The before snapshot is byte-identical to assembled v1 source a2a2ce43b9a2ee0c08311aaaceb8c244164ca3dc6e59a24972c319c79dad4631. Original author, assembled source and raw failure evidence are preserved. No compiler, formatter, test/guest, network, model or SCM operation was performed by this author. Root owns independent review and qualification.

Measured failure and cause

Root's assembled test-multi-owner-entry-1 process aborted after approximately 5.68 seconds in the first C-first declaration; the other three declarations never ran. Its event list already contained Report(C, "Tool callback"), while the controller waited for Report(C, "execution"). During that wait A's finite synchronous barrier and C's report barrier also reached their bounds. The controller's wait assertion held the observation-state mutex and poisoned it; CallbackDrop then unwrapped that poisoned mutex while another assertion was unwinding, producing a destructor panic and process abort. These are retained failures, not passing evidence.

The expected phase was wrong. In frozen runtime.rs:5587 the real execution outcome first routes entry failure, then :5589 invokes cleanup_unstarted_tool_children_after_error. That helper publishes "Tool callback" at vm.rs:2703. The later finish_tool_process "execution" report is not the first report for C's callback error. RunFailure::publish at failure.rs:130 invokes the synchronous report hook before its published flag and notification at :177 onward. Therefore the earlier actual callback report is exactly the causal pre-publication barrier the test needs.

Correction

The one exact C phase expectation changes from "execution" to "Tool callback". The controller still holds C's real report hook, requires A's callback alive, B's consuming hook absent, the actual run notification Pending and published_primary absent, then releases the hook and requires C's original typed cause at the real publication receipt. It still waits for B's actual outer thread-exit-hook report before releasing A. No alternate phase, substring or allowed set was added.

Observation-state reads used by assertions now take owned snapshots, and mutation sites return their previous values before applying the identical assertions. The wait timeout copies its observed events and drops the mutex before assertion/formatting. Release::blocking also drops its bool guard before its timeout assertion. CallbackDrop retains both original assertions but reads an owned snapshot. Thus a controller assertion cannot poison the shared observation mutex needed by callback destruction/rescue. No poisoned-result fallback or ignored assertion was introduced. Initial role assignment still occurs under the same mutex, preserving A/B allocation; the same parent identity and duplicate-state requirements are checked after releasing it.

The snapshots retain only the test's already-observed Arc identities, Weak ThreadState identities and copied event/signal observations. They do not consume, deduplicate or rewrite the product's owned Error::SignalEffects ledgers. Exact cause addresses, effect counts/watermarks, callback/owner-dropped checks, publication-before-physical-join requirements and actual join identities are unchanged. The final typed-error verifier is unchanged apart from reading an owned snapshot rather than holding the observer mutex.

Goalpost assessment

- Assertions weakened: no. All existing conditions remain, evaluated outside shared observer/release guards. C's exact expected phase is corrected from the documented real control flow, not broadened.
- Tolerance widened, exemption added, case skipped or comparator relaxed: no. The five-second bound, all four declarations, expected statuses, exact causes/effects and exact report-phase comparator remain.
- Failure renamed or relabelled as pass: no. Assembled v1's abort and three unrun declarations remain failures/unrun. The successor has no execution verdict. Report-hook entry is still explicitly not actual publication.
- Check deleted instead of satisfied: no. All ordering, lifetime, count and identity requirements remain. Rescue remains release-before-completion/join; no product change was made to accommodate the fixture.

Limits

This is authorship of a test correction, not independent approval. The exact successor must compile and run under root's bounded evidence harness. It does not discharge the separate prepared-action or waiter failures, or establish backend parity, full Linux semantics, scheduler determinism or landing readiness.
