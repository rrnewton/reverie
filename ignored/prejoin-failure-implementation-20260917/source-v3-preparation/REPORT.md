# Reverie producer corrections and native ownership seam

Source preparation only, repository Reverie, branch `codex/kvm-proc-fd-identity-20260917`, base `114b309413612fafc2657c74e83811c71aac7b19` (tree `16b49eb8c6e85b6b63ac8d2b43179de663726083`). No source is committed. Execution remains withheld for the parent coordinator's complete bound plan.

The independent report was read completely and its SHA256 matched `d559f0ff60d0c38a5e5cbf32e8918a946de2a31d2fd9be380a220c6d888e69fe`. Its prior paper/scheduler grounding was not repeated. Existing source-v2 copies, patch/binding, and all failed/passing execution evidence were retained untouched.

## Corrections

1. `vm::finish_host_worker_outcome` is called by the production host-owned worker return path before its retirement/transport/CLEARTID closure. It retargets the inherited Tool failure context to the actual worker TID while preserving its process identity. With no Tool context it leaves the ordinary result and cleanup unchanged.
2. `runtime::finish_tool_process_after_workers` was extracted from the production finisher. Cached worker errors and missing final task status now pass through the reporter before consuming owner hooks or descendant joins. The existing exec-worker-cache suppression is preserved. Re-reporting the already-retained typed cause does not create another global failure event.
3. `RunFailure` stores the first typed cause and BackendFailure event together. A publication mutex serializes cause selection, the synchronous Tool terminal hook, and local notification. It is never held across a guest RPC or physical join. Later returned errors retain their distinct typed causes.
4. The post-worker finisher polls the real terminal subscription after owner hooks when selecting failed versus ordinary descendant joins, including when local execution was successful. This check handles terminal state observable at that choice; it does not introduce an atomic transaction spanning later gate resolutions and newly arriving failures.

The adjacent scratch-hide/callback dual-error limitation was not changed. Normal guest errno conversion, normal process status, and existing start/hook ordering logic were not relaxed.

## Native seam

`reverie-kvm/Cargo.toml` adds empty, explicitly nondefault `native-test-support`. The public module is exported only with that feature; unit tests also compile the same module. It introduces no dependency and changes no manifest revision pin.

`NativeToolCallback<T>` is generic over the abstract Guest interface and returns BoxFuture. `NativeToolOwner<T>` retains actual Tool/ThreadState ownership, a real ElfExecutor lifecycle, run-failure context, child start gates, and OS handles. It provides owned state access, real fork lifecycle construction, child attachment, host worker attachment, the production KvmGuest/drive_handler callback path, and the shared post-worker finisher.

Child attachment uses the existing `spawn_owned` helper. Both gate-receive/driver errors and refused spawning enter consuming cleanup. A fatal owner Drop rescue publishes before retirement, cancels retained gates, reaps owned handles, and invokes consuming hooks; explicit finishing removes owned fields first to prevent double consumption. Returned preparation errors consume supplied Tool state before returning. Host work closures must themselves remain bounded or have a caller-owned channel rescue; a native thread cannot forcibly interrupt arbitrary Rust code.

The existing LoadedStaticElf test fixture was moved into one cfg-gated constructor and its original test helper now delegates to it. Its data fields were preserved. No fake KvmBackend was constructed. Clock reads and guest instruction/syscall execution panic in the native callback seam. Actual Detcore startup therefore needs the coordinator's max_timeslice=None native fixture, with sequentialization still enabled.

The seam begins after native task retirement and calls the actual production post-worker finisher. It does not establish initialized-VM setup, backend transport release, physical guest execution, or determinism/parity qualification.

## Source-derived native selectors

Seven new controls are listed exactly in selected-tests.json:

- Two controlled publishers with distinct typed causes and pid/tid/phase metadata; first cause selection pauses before terminal event recording, and local publication remains pending.
- A real root KvmGuest RPC interrupted by an actual owned host worker's returned error, with exact worker identity and typed cause checked after the actual OS joins.
- Missing final status from an actual live executor task preparation, with a started descendant parked in its actual KvmGuest RPC.
- The same missing-status producer with an unresolved descendant gate, requiring Cancel.
- A returned error retained by the actual worker cache without an earlier report, requiring one publication before a started descendant RPC join.
- Terminal publication in the owner hook after a successful local outcome, requiring the pending descendant gate to receive Cancel.
- Ordinary completion preserving parent status 37, child status 73, Start, and owner-before-child hook ordering.

Existing 27 selected names are retained, for 34 source-derived selectors total. This is not a test-listing or execution count. Controls install response/release/join rescue before ordering preconditions; rescue responses cannot satisfy fatal-outcome assertions, and a missed completion bound remains a failure after reaping.

## Goalpost-moving assessment and verification limits

Existing assertions were not weakened. No tolerance, exemption, skip, comparator, label, or success classification was relaxed. No check was removed instead of satisfied. Existing native fixture observations gain publication-before-hook assertions only for newly selected scenarios.

Manual source reading and source-copy/hash readback were performed. No rustfmt, diff-check command, compilation, test listing, test execution, guest, network, commit, TaskGraph/public write, or pin update was performed. Formatting and compiler/type validation remain for the parent coordinator. This report is implementation evidence, not independent approval.

The complete 11-file source copies are under source/. candidate.patch is the complete delta against the recorded base; source-v2-increment.patch is the incremental delta from the preserved source-v2 copies (or base for newly touched files). binding.json records exact bytes and hashes.
