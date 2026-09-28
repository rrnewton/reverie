# VM driver-owner activation: author handoff

This is an implementation handoff, not independent approval or execution evidence. Only `reverie-kvm/src/vm.rs`, new `reverie-kvm/src/vm/entry_owner_tests.rs`, and this ignored artifact directory were written. The existing isolated checkout and all preceding work were preserved; no frozen v3 packet was edited. No compiler, formatter, test, guest, network, model, or SCM operation ran.

The source map at S/ignored/kvm-private-owner-next-activation-v1/REPORT.md and its cited api-successor report were read before implementation. Root owns `entry/driver.rs` classification/retirement and the runtime author owns ordinary callback generations and direct/root lifetimes. This packet does not approve either increment.

## Frozen author increment

Baseline `BASE-vm.rs`: 390011 bytes, SHA256 `30e4287a9bd5b9686862f13800a663bbc02852419ce714fe96f8a9ec1f557e47`.

- `PROPOSED-vm.rs`: 404441 bytes, SHA256 `42b3df8175f728659dcfc2378646e4bdfb078729fbac8aa4dbda30d44bed5536`.
- `PROPOSED-entry_owner_tests.rs`: 6972 bytes, SHA256 `5f0f13738b06b0ae38582daf0b892e57ba1a07f47ed79ae789bb1cc33f2ec827`.
- `SOURCE.patch`: SHA256 `1429cf46a148764a825b7e038585a17d3ef6ebc684e5d90d0de6dd9bc2282030`.
- `TARGET.json`: SHA256 `d65fbbaa61ddd8ae6c4ea46dec2aedd9258101e8b864e9c822ef7151472acce6`.

Source ownership was released to root after these snapshots. Later formatting, corrections, and composition must bind a successor independently.

## Ownership and boundaries

`KvmBackend` now has an initially absent private driver owner and the requested `start_entry_driver`, `entry_driver_owner`, `set_operation_origin`, `restore_entry_origin`, and `begin_entry_callback` APIs. Starting creates a unique lexical DriverScope, stores its cloneable owner, and binds both backend memory and CountedVcpu to its ordinary origin. Callback entry checks pending state first and binds the new guard's generation; callers must keep that guard outside the complete owned-future/borrow scope.

The actual spawned Tool fork, Tool thread, and Host thread each start an independent scope outside their outer panic catch and explicitly bind the executor's retained memory before execution. They do not inherit the parent's driver identity. Their ordinary origin is restored before post-loop memory cleanup. Scopes survive clear-TID operations, child-wait hooks, error reporting, and final diagnostic composition. They route again after those operations and close before the existing intentional deferred panic resume. Host early publication now uses its child-specific FailureContext TID. Its route waits via private yielding machinery outside locks before publication or physical cleanup.

The successful fork wrapper now has a narrow outer catch. Normal deferred callback panic propagation stays outside that catch. An unexpected fork unwind retains the original payload after any earlier deferred payload, routes/publishes its typed panic and owned executor effects, drains separately retained consumers, retires local resources, publishes before worker/child cancellation and joins, then returns its aggregate to the wrapper's final route/close. The wrapper marks ChildCompletion::Failed and notifies before the existing exact-payload process propagation. No consuming hook is invented for Tool/thread state already destroyed by an unexpected unwind.

The caught-worker helper similarly restores ordinary views, routes before initial publication, drains retained consumers, completes clear-TID/resource retirement, folds/retire-closes its DriverScope, and records the complete typed error before original payload resume. Its old test entry delegates without a scope; the production wrappers use the scope-aware method. Existing fixture assertions were not changed.

Failed-spawn retained consumers create their independent driver only when consumed, never in the still-active parent's callback. The entire child cleanup future is caught with the existing construction/poll/drop catcher, its typed result and exact payloads go through the child ToolPanics owner, and final route/retirement precedes payload transfer and backend destruction. External abandonment of that entire retained future is not claimed to complete its hooks.

Fork snapshot preparation now explicitly binds the parent's current OperationOrigin and failure context before scratch normalization and long-mode preparation. The new child retains that operation through child TID/segment/return-frame preparation and its executor memory binding. Rebinding to an independent owner happens at actual spawned execution or retained cleanup consumption. Thread preparation already clones the parent's immutable memory view.

## Inline Host fork dependency

An inline Host fork is nested work on the parent's call stack, not a separate worker. It therefore inherits the parent DriverOwner and exact current OperationOrigin. Creating a separate owner and waiting for parent publication there would prevent that same parent call stack from reaching publication.

On a returned inline child error, if there is an enclosing owner, its backend/executor are moved into the parent's retained-consumer queue. This keeps KvmBackend::drop's cancellation/joins outside the live callback. The outer drain restores ordinary views, consumes nested retained children, cancels/joins the native children, preserves every returned real error, and transfers pending panic ownership with the existing guard. The Host worker outer path now drains this queue after routing/publication and before its normal retirement. With no enclosing owner the standalone native typed-return behavior is preserved.

`finish_static_elf_thread` checks captured private/gate causes after best-effort TID/resource cleanup and before natural or cancellation joins. The inline finishing operation has a by-reference helper so a failing child is still available for retention; old test call signatures are preserved by the existing wrapper. `start_pending_tool_children` checks before releasing any gates and returns a captured typed error directly, without invoking its synchronous cleanup publisher for that check. The Exec arm also checks private pending causes immediately before its existing cancellation/join.

These checks return typed failures and do not invoke a foreign PendingFailure publisher. Root's PendingFailure now stores notification-only observation; this author calls only each concrete backend's existing publisher at its valid outer boundary.

## Added controls, not executed

- `vm::entry_owner_tests::child_driver_rebinds_inherited_callback_view_and_preserves_generation` uses actual backends sharing memory to verify independent child identity, immutable retained callback generation, separate subsequent generation, parent callback lifetime, and retirement/clearing.
- `vm::entry_owner_tests::caught_worker_driver_retires_after_pending_consumer_and_before_payload_resume` uses an actual constructed Future::Drop panic, captured typed gate cause, and retained consumer that polls Pending then completes. Through the production caught-worker helper it checks publication and live owner/parent state during consumption, typed child error retention, parent retirement, driver closure, and exact original payload allocation on resume.

Both require `/dev/kvm` setup with no skip and issue no KVM_RUN. These are constructed callback/consumer controls, not actual guest callback or failed host-spawn injection. The new selectors alone do not establish routing through every production wrapper.

## Preservation and limits

The complete old 213713-byte vm test module, containing 75 existing test declarations and both prior root-owned includes, is byte-identical (SHA256 `a8c98f5b564d8c4a47e13752a42dd0d847fd9bb66502ce6957ad211dbb5cc918`). No assertions, tolerances, comparators, labels, skips, exemptions, gates, or acceptance denominators were weakened. Existing worker/public/instruction/terminal control files were not edited. New failure paths remain errors.

The immediate Exec pre-join private check detects an already captured obligation. It does not by itself prove that a concurrent capture immediately after the check cannot interact with the pre-existing physical join inside an injected callback. This concrete remaining dependency was sent to root before release and requires its own ownership argument; the author does not claim that all injected-exec joins are solved.

Inline Host returned errors are retained; arbitrary unexpected unwind before an inline child returns can still trigger backend destruction during stack unwinding. The new real-worker/fork catches do not constitute a general supervisor for that nested case. Preparation errors before a child has run still have no child Tool consumer or runnable child-worker set; their typed cause remains with the preparing parent operation.

Eventual arbitrary payload-destructor panic, externally dropped public futures, unrelated lifecycle invariants, and already destroyed unpublished outcomes remain outside this packet's completion claim. No full scheduler, determinism, Linux/POSIX parity, test-pass, or landing approval is asserted.
