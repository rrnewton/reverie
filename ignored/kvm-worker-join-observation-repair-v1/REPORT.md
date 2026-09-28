# Exact RPC worker transfer observation: author repair

This is a bounded author delta, not independent approval. No compilation, formatter, test, guest, network, model or SCM operation was run. Only vm/worker_join.rs and runtime/failure_tests.rs changed; vm.rs and production behavior are unchanged. Both edits compile only under cfg(test).

## Authenticated failure and cause

The live baseline matches ignored/kvm-exec-physical-join-v2/source-1 and its manifest for all three authorized files. The retained v2 test-caught-worker-1 logs report the actual caught-worker control failing at wait_until after 2.00 seconds, raw status 101, 0 passed/1 failed/0 ignored; result.json records accepted=false and the service collected/cgroup absent. This report preserves that failed result.

The unchanged fixture was also read against ignored/kvm-owner-activation-v3/source-1. Its runtime/failure_tests.rs is byte-identical to the v2 baseline (52,820 bytes, SHA256 18d7d156bd5ecc3599c100a106698d55ff5aa9229c4b31713fb8312d1b9b7995). In old v3, has_worker_handles at vm.rs:573 observed only registry contents, and join_workers at :866 moved whole batches. In v2, has_worker_handles at vm.rs:577 includes both registered and active ownership, and join_workers claims one worker at a time. Thus the old wait for !has_worker_handles cannot witness transfer while the RPC worker is physically blocked. Merely reverting the helper would also be insufficient: the caught-panic variant's worker 3 remains registered while the joiner waits on worker 2.

## Exact repair and unchanged requirements

New cfg(test) worker_join_owns_target at worker_join.rs:208 checks the guest TID, the original JoinHandle's captured host ThreadId, the known physical joiner's ThreadId, and absence of that TID from worker_handles under the same ledger-to-registry lock order as transfer. It performs no mutation, join, publication or callback.

The fixture captures worker 2's host identity before registration and the cleanup joiner's identity before waiting. At runtime/failure_tests.rs:527 it now waits for that exact active ownership transfer. The existing joined_receiver.try_recv()==Empty assertion remains immediately afterward, before any release/publication. Added assertions require group ownership still to exist and zero published failures at that point. After the existing bounded join succeeds, a new assertion requires the exact active target to be absent. The final !has_worker_handles assertion remains and still includes both registry and active ledger ownership.

All existing publication-before-retirement, exact panic payload, typed primary/cleanup error, report-count, consuming-hook ordering, completion timeout and rescue assertions are unchanged. The two-second wait/receive bounds are unchanged. The four fixture variants still execute: fatal pending RPC, separate consuming-hook error, normal RPC/status and caught worker panic.

## Complete has_worker_handles caller audit

The source search found five pre-existing calls, not three: runtime/failure_tests.rs baseline :524 (transfer wait) and :614 (final empty assertion), plus vm.rs :7855 (retained child before callback destruction), :7880 (completed outer cleanup), and :8139 (completed child consuming cleanup). Only the transfer wait required a different observation. The other four remain byte-identical with v2's all-owned semantics. has_owned_worker_joins has only the vm.rs wrapper caller; it is unchanged. The repair adds one positive all-owned assertion and two exact-target observations in the shared RPC fixture.

## Goalpost-moving assessment

A predicate is deliberately replaced: whole-registry/all-owned emptiness is no longer the witness of the pending RPC worker's transfer. That aggregate predicate encoded the former batch implementation and cannot hold at this valid pending physical join in v2. The replacement requires the exact original worker, exact joiner and removal from the registry; it retains the actual pending-join check and strengthens pre-publication and post-join observations. Root explicitly accepted this target-bound requirement and retaining the all-owned final checks before this edit.

No assertion is removed or weakened; no timeout/tolerance is widened, case skipped, exemption introduced, failure relabelled, or result gate relaxed. The changed wait must be reviewed as a changed observation rather than described as byte preservation. It does not waive final ownership cleanup: the original all-owned-empty assertion still runs after physical join and consuming hooks. An exact inverse of the declared fixture edits reconstructs every byte of the old failure_tests.rs, proving its other checks unchanged.

## Bound delta

- reverie-kvm/src/vm/worker_join.rs: 19841 -> 20480 bytes; before SHA256 ff326477247930f1e5098a095801be9d12fc118b010321339492815efc6a805b; after SHA256 eeeb36521f0ab630ed21f95ac96462608ead0961e23b8da4046fb26be671b9da.
- reverie-kvm/src/runtime/failure_tests.rs: 52820 -> 53242 bytes; before SHA256 18d7d156bd5ecc3599c100a106698d55ff5aa9229c4b31713fb8312d1b9b7995; after SHA256 efb1f468656f04276111c14f3ac4874251b1e7ba97a3796ccc7f81a42969e762.

Unchanged vm.rs: SHA256 0518949aea3444c430360a613f2d6c6f8d34edbd8fff016aebc1c829d20b05a8. AUTHOR.patch contains the full two-file delta; BASELINE.json and source/ bind the input/output. Required next evidence is root's rerun of the caught-worker control and the other three shared-fixture variants, followed by its normal qualification policy. No passing result is claimed here.
