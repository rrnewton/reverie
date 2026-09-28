# Exec physical join repair: author release

This packet binds an implementation increment, not a self-review or behavioral approval. Only vm.rs, new vm/worker_join.rs, and append-only vm/exec_wait_tests.rs were edited by this author. Root separately added the test-only thread_spawn_hook feature in lib.rs. No compiler, formatter, test, guest, network, model or SCM command was run. Root must qualify this exact source after formatting and obtain independent review.

## Resulting ownership

vm.rs:313–315 loads the private helper module; :413 gives each group its ledger; :691 serializes registration with direct/helper transfers and rejects an active duplicate TID in either location. vm.rs:4479 retains request-cancellation first, subscribe-before-recheck, EntryDriverWatch and stop checks before/after the new asynchronous progress operation. The progress operation never calls JoinHandle::join. is_finished is only eligibility to transfer one original handle to the owned helper.

worker_join.rs:211 retains initialized bootstrap state through spawn_owned. Launch::drop (:130) rolls back the parent-side Launching transaction on synchronous hook unwind. Bootstrap::drop (:104), captured before spawn, records and wakes failure when an inherited child hook prevents body entry. Originals remain in the registry until body readiness and the exact successful helper handle are both stored. Failed spawn returns a typed retained error; successful spawn followed by child startup panic retains its exact handle for outer physical collection. Parent-hook panic resumes through the existing caller's panic machinery with the original payload; the added control directly catches its unchanged allocation.

The helper (:146) has at most one Queued/Joining/Ready/Collecting job. It takes one main-closure-finished original, physically joins outside every registry lock, then stores the complete std result before notification. It never owns the whole worker batch. Exec interruption drops only the wait subscriptions; handles, in-flight identities, gates and results remain group-owned. Result collection (:260) preserves the pre-existing direct WorkerFailure Arc identity for caught worker panics, the exact joined payload, returned typed errors, and direct-discard bare RunAborted behavior. It records each full-join cause once before retiring the active entry and gate. Helper failures/payloads have separate group-lifetime storage and no invented guest TID.

advance_worker_joins_for_exec (:323) checks startup errors, in-flight work and terminal drain ownership as well as the registry. Empty worker_handles cannot imply completion while another direct join, helper job, collection or startup owns work. Returned-main notification can request a cooperative repoll only to observe is_finished eligibility; there is no physical join on that poll.

discard_unstarted_worker (:371) claims only its own registered cancelled target, with an explicit self-join refusal. As before, an already-taken child returns false; its caller verifies the previously observed exact gate registration. It never waits a whole helper or unrelated job. Enclosing A can be in the registry OR owned by an outer direct join while cleaning B; the required invariant is that an unfinished A cannot be assigned to the helper and targeted B cleanup cannot be queued behind an unrelated C.

join_workers (:395) serializes terminal drain, keeps direct joins in the same in-flight ledger, and takes remaining registered workers one at a time. It recovers an untouched queued original if the helper unwinds, collects committed results, waits with a condition variable under the state predicate, then requests helper stop and physically joins the exact helper outside locks. A result-ready state is rechecked before any condvar wait, avoiding a missed result notification. Helper TLS is joined only here, after the caller's callback/publication boundary. Repeated cleanup uses existing error caches and cannot rejoin consumed handles. A normal completed helper may be replaced only after physical reaping.

## Actual final owner

An idle helper retains the ledger that owns its handle; Arc structure by itself is not a shutdown protocol. Production helper creation is reachable only through leader cancel_guest_threads_for_exec, after rejecting is_guest_thread. That future borrows the leader backend. The leader KvmBackend::Drop always cancels and calls join_workers. The only production transition to is_guest_thread is from_thread_state (vm.rs:1839–1843), before the child executes, replacing a fresh never-used group. Constructor-created leaders remain leaders. Therefore the production owner drains on normal destruction, error/unwind destruction, or destruction after an externally dropped borrowed future. The eighth control exercises backend Drop with an idle live helper and a retained group observer, without an explicit group join. Standalone group controls require their explicit GroupDrain rescue.

This does not claim progress for arbitrary synchronously blocking spawn hooks, arbitrary TLS dependencies that are never released, process abort, OOM, or recursively panicking payload destruction at final group destruction. It is not a global supervisor, complete scheduler/determinism review, or approval of unrelated owner routing. External future abandonment retains the previously documented broader semantics.

## Controls and preserved requirements

All original 12,582 bytes of exec_wait_tests.rs remain an exact prefix, SHA256 da8bbab493f1d609c1113db8418ba469cc3299ce1dffc94fd21304c7017120f0. This includes the unchanged measured real-TLS negative control. The complete old vm.rs test/module suffix is byte-identical; PRESERVATION.json binds its hash. No existing assertion, tolerance, skip, exemption, comparator, label or gate was changed. No failed result was relabelled a pass.

The new controls use five-second finite coordination inherited from the existing file. TLS destructors record timeout/order observations and never assert; release/drop guards permit cleanup after parent assertion failure. Actual spawn hooks run on isolated host test threads, filter the production helper name, and do not leak process-wide hook configuration. Parent panic, pre-body child panic, and helper TLS use the pinned std hook API. No guest instructions or KVM_RUN are issued by the new controls. Three controls require KVM setup: normal physical completion, helper TLS/reuse, and leader backend Drop. The other five use host groups/threads only.

New exact selectors:
- vm::exec_wait_tests::exec_wait_success_requires_worker_tls_physical_completion
- vm::exec_wait_tests::refused_exec_join_helper_preserves_original_worker_and_gate
- vm::exec_wait_tests::exec_join_helper_retains_exact_worker_panic_through_repeated_teardown
- vm::exec_wait_tests::exec_join_helper_does_not_capture_nested_discard_behind_another_worker
- vm::exec_wait_tests::exec_join_helper_parent_spawn_hook_panic_restores_original_ownership
- vm::exec_wait_tests::exec_join_helper_child_spawn_hook_panic_retains_handle_payload_and_originals
- vm::exec_wait_tests::exec_join_helper_reuses_thread_and_defers_inherited_tls_to_outer_drain
- vm::exec_wait_tests::leader_backend_drop_physically_reaps_idle_exec_join_helper

## Bound source

- reverie-kvm/src/vm.rs: 411300 bytes, SHA256 a6b2fa6a0a8ec73d98f2e0df3becc6e18aef52ad1035f96f41370a352984cacf
- reverie-kvm/src/vm/worker_join.rs: 18314 bytes, SHA256 31e45fb0afee2b16efdc91292afd2599084a22153be64a49deab000582045614
- reverie-kvm/src/vm/exec_wait_tests.rs: 33515 bytes, SHA256 5d43aa1cd924ea0954c403f3f00a5c4b0e61fbd1c7bfe86d8b4e02c6c6d7e8c0

AUTHOR.patch is the isolated three-file author delta; SOURCE.json and source/ bind the released bytes. baseline/ preserves the input vm and both original controls. No claim of compilation or passing tests is made.
