# Concrete owner panic wiring: author handoff

This is an implementation handoff, not independent review or execution evidence. No compiler, formatter, tests, payload/model execution, network or SCM operation was run. The write scope was the assigned `vm.rs` owner wrappers/helpers, concrete child-handle storage and four fork join routes in `executor.rs`, and additive controls in `vm/worker_panic_tests.rs` and `executor.rs`.

## Exact snapshot

`TARGET.json` binds the three source files at author release. `SOURCE.patch` is the isolated author increment against the corresponding `BASE-*` files. `PROPOSED-vm.rs` excludes two concurrently added root-owned include lines; `COMPOSED-vm.rs` includes those lines and is the author-release live source snapshot. Those includes are `vm/public_tool_panic_tests.rs` and `vm/instruction_callback_panic_tests.rs`. Neither root-owned test body is included in this author's patch.

Author-release full source SHA-256:

- `reverie-kvm/src/vm.rs`: `c98fcbdaea105d91a774ccb7a293ada1bcad434a86139c6aac8274ae412ccb61`
- `reverie-kvm/src/executor.rs`: `499177f2237054de27a417e399ed998f0fceeb6849a64a813f248f93f1d91888`
- `reverie-kvm/src/vm/worker_panic_tests.rs`: `870b89a9b51e20466385a4ca7a0df676dd20b718f2ca7590f0de40efc9bf9cd7`
- `SOURCE.patch`: `d2291755e0390bda17822f2728d7efc2afeeb059e9efc773322ebdcb4e760281`
- `TARGET.json`: `7ef3dd952cf56a30e3179d408725db8d7e23071885e5bcf95083a69e49e3644c`

Later root formatting/composition must bind its own successor snapshot; these hashes describe the released author snapshot only.

## Production changes and static trace

The successful Tool fork wrapper creates one `ChildProcessPanicOwner` and shares it only between that child closure and its concrete registered handle. The child wait hook uses `catch_owned_future_from(|| lifecycle_state.on_backend_child_wait_event(...))`, including synchronous future construction in the catch. Its returned Reverie error remains typed. After normal process finish, every returned error sets `ChildCompletion::Failed` and sends the completion notification before `finish_deferred_process_panic` can resume the earliest original payload. The prior `?` on the wait hook no longer bypasses this bookkeeping. A normal successful completion retains its existing waitable/auto-reaped status behavior.

The successful Tool thread wrapper calls `finish_deferred_worker_panic` outside its existing outer `catch_unwind`, after normal retirement/reporting. Resuming a deferred callback panic therefore cannot enter the unexpected-panic cleanup path and repeat cleanup.

Both failed-spawn closures own `ChildToolPanicTransfer` before the retained future is polled. They await `finish_unstarted_tool`, then transfer the child owner's pending payloads to the parent owner before the child backend is destroyed. The guard also transfers pending ownership if an unexpected unwind destroys the retained future. It does not claim that externally dropping a future completes its consuming hooks.

The unexpected caught-worker path appends the new outer payload after any earlier deferred payload, publishes its worker failure, then calls the root-owned `finish_unstarted_tool_cleanups_with_panics`. That helper catches each complete owned consumer before progressing to the next. A child guard's transfer and raw consumer poll/drop payloads therefore remain in their chronological owner order. The resulting typed cleanup aggregate and payloads are recorded before retirement and original propagation. The prior raw-completion helper remains test-only through `#[cfg(test)] resume_caught_worker_panic`.

`ChildProcessHandle` carries the actual handle and optional per-child owner through the pending map, nonblocking completed list, and worker-to-leader transfer. Each of these physical join routes calls that wrapper: `discard_unstarted_child_process`, blocking `collect_child_process`, pending `finish_child_processes`, and completed `finish_child_processes`. For a Tool-owned panic it transfers the exact joined `Box<dyn Any + Send>` to `RunFailure::retain_panic_cleanup`, with the same typed diagnostic Arc already recorded by the child. The earlier retained secondary record and later original-payload record consequently use one exact Arc identity; RunFailure completion deduplicates that Arc without comparing text. An unexpected Tool fork panic with no recorded diagnostic receives a typed `GuestWorkerPanic` fallback, while retaining its exact joined payload.

`CompletedToolPanic` now carries `_run_failure: Option<Arc<crate::failure::RunFailure>>`. `finish_public_tool_panic(result, retained_failure)` stores it only when resuming a pending panic. Root owns the runtime callers (direct `None`, root `Some(failure.clone())`). This anchors fork payload records across the root's panic propagation after `tool_failure` is cleared.

Record extraction/cloning occurs under short mutex scopes. No user payload is intentionally destroyed under a record mutex. Guard transfer takes the child vector before acquiring the parent mutex.

## Added controls, not run

1. `vm::worker_panic_tests::unexpected_worker_preserves_prior_and_cross_consumer_payload_order`: actual caught constructed parent-future destructor panic through production worker finish and physical join; optional pre-existing owner payload; raw child poll panic, child-owned destructor panic transferred by the production guard, then another raw child poll panic. It checks exact original and secondary allocation order, a distinct typed child error, publication and parent lifecycle state at each consumer poll, retirement, one poll/drop per consumer, and no payload destruction until final group release. It requires `/dev/kvm` setup with no silent skip, but performs no `KVM_RUN`.
2. `vm::worker_panic_tests::failed_spawn_transfer_guard_preserves_pending_payloads_on_future_drop`: production guard in both unpolled and Pending constructed future destruction; verifies exact allocation order and exactly-once ownership transfer. Host futures only. It does not force an actual host spawn failure or claim consuming-hook completion on dropped futures.
3. `executor::child_panic_owner_tests::fork_join_routes_retain_exact_payload_and_typed_owner`: actual gated registered host thread handles through discard, blocking collect, pending join, nonblocking collection followed by completed join, and worker-to-leader transfer. Send/non-Sync payloads and the exact typed cause survive physical join and RunFailure completion; original and secondary allocations drop only at final RunFailure release. It constructs terminal fork outcomes and does not exercise a guest Tool callback inside the successful fork loop. Host threads only.

Coordination waits are finite (five seconds). Worker controls use the existing unwind rescue; fork assertions follow physical joining, including completed-handle rescue after nonblocking collection.

## Preservation and goalpost accounting

No prior assertion, tolerance, comparator, skip, exemption, result label or test declaration was removed or weakened. The old worker-control source is a byte-identical prefix of its new file. Existing `vm.rs` test bodies are unchanged by this author. `executor.rs` has six required fixture adapters and no changed assertions:

- `exiting_workers_transfer_every_child_handle_in_virtual_order`: one completed raw handle gains `.into()`.
- `child_process_cleanup_joins_every_handle_and_preserves_each_error`: two completed raw handles gain `.into()`; two cleanup-rescue joins access the wrapper's actual `.handle`.
- `discarding_named_fork_joins_only_that_unstarted_child`: the final sibling join accesses the wrapper's actual `.handle`.

The mechanical manifest also records every old assertion macro start line preserved in order (510 in vm, 2944 in executor, 61 in old worker controls). This textual check is narrower than a compiler or behavioral result. Error/failure test sources were not modified by this increment.

## Explicit limits

An unexpected outer host panic can still unwind a selected but unpublished typed outcome before reaching the worker catch; a payload-only owner cannot reconstruct that lost outcome. This increment preserves pending payload order and any previously published RunFailure cause, and does not claim recovery of an already destroyed error. Root's deferred callback outcome implementation is a separate increment.

An arbitrary unexpected fork unwind outside the caught Tool hook/driver boundaries can destroy pending child backend state before normal fork bookkeeping; the concrete join owner preserves the joined original payload and a fallback typed panic but does not reconstruct destroyed secondary state. Unowned Host fork handles preserve their previous text-diagnostic behavior and are outside Tool payload-retention claims.

Eventual destruction of the last backend/group/RunFailure owner remains the payload destruction boundary. Arbitrarily panicking payload destructors at that final boundary are not made safe here. There is no global supervisor, no completion guarantee for externally dropped public futures, no actual failed-spawn injection in these controls, and no full scheduler, determinism, Linux/POSIX, or root/fork routing approval.
