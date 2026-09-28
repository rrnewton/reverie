# Parking continuation review

Findings are bound to the root's source-1 vm.rs/runtime.rs on base `91110d249ffd8957267d71fab8c83d9636105efe`, which still matched the live paths when the read began. Root's later source-2 changes concern entry/clock controls, not these reviewed methods. This is a bounded source review; no model, compiler, test, ioctl or guest was invoked here.

1. **Blocking: synchronous admission before the async wait.** `park_process_action` calls `set_syscall_return_park(true)` before `try_park_process_action`. The bootstrap helper computes one trampoline address/byte, then calls `write_raw`, which now waits in `copy_blocking`. `gate.admit_operation` checks poison, not open admission. An already closed gate therefore blocks the async worker before it can poll its registered gate/cancellation/failure futures. A closer that needs that executor to reopen can make this a wait cycle. The unpark write has the same issue after a real returned exit. Merely awaiting an open observation and then calling write_raw is insufficient: another close can win between the observation and admission.

2. **Blocking: a completed stop future is polled again.** The outer `select(..., stop.as_mut()).await` result is discarded. If stop wins, the async future created by `run_process_action_with_tool_inner` completes. The next loop iteration calls `stop.as_mut().now_or_never()` on that same completed async future and can panic. Consume the stop winner immediately, or latch/fuse completion and never poll it again. When an actual parking error is already owned, a later ordinary cancellation must not overwrite it.

The direct preparatory-read scope remains separately open: Host and Tool Thread branches read the parent syscall frame synchronously before calling park_process_action, and CompletedSyscallBoundary::capture reads both the frame and configured hypercall bytes before action dispatch. Fixing only the park/unpark byte does not establish that all async process-action preparation yields while closed. These preexisting copy callers became waits with copy admission. Their bounded conversion needs its own continuation decision, without mechanically replaying the whole action or moving the original capture after the actual parking run. The fault and injected-boundary paths also stage frame bytes before dispatch. Root explicitly limits the immediate helper fix and retains these predecessors as separate scope; they are not waived or declared safe.

## Minimal local split

Keep the actual action future and its owned locals. Add a nonblocking typed `try_write_raw` that performs `gate.try_copy(handle_origin)`, returns None without acquiring state/backing locks or touching bytes when closed, and otherwise reuses the admitted raw writer followed by the retained-poison check. Bootstrap shares its pure address/byte computation between the existing blocking helper and `try_set_syscall_return_park`.

The parking future needs three distinguishable stages. During preparation it subscribes before checking its existing stop predicates, tries the park byte once admitted, and awaits on None. During entry it repeats those stop checks, invokes CountedVcpu once per admitted attempt, and awaits only its no-ioctl None result. A real returned VcpuExit is recorded and reduced to an owned parking result before any await. During unparking it retains that result and tries the restore byte without rerunning KVM. It must distinguish an actual error from an ordinary terminal cancellation if poison/stop arrives while restoring; preserve the real cause and any cleanup cause. Never reinterpret real KVM EINTR as the no-entry branch. Completion returns true once; terminal cancellation returns false without claiming successful parking or a guest syscall value. Do not keep an admission token or borrowed exit over an await.

The unchanged copy helper must stay blocking for its existing callers; the narrow new try helper provides the async parking adapter's split. This is not a proposal to make all synchronous Tool memory APIs asynchronous or to grant mapping-publication rights.

## Ownership and status findings

`prepare_forked_process_admitted` constructs one child ElfExecutor outside the retry loop, retains it across parking, and snapshots/writes TIDs/constructs a backend only after successful parking. On early cancellation its local executor drops; ElfExecutor::drop retires its signal-registry binding, removes the exact task generation and forgets its process exit. No worker/start gate has yet been spawned. I found no repeated fork clone, snapshot, TID store or child spawn in this route.

Both Thread branches capture parent state once before parking, then perform TID writes, child-executor/backend setup and spawn only afterward. Exec preserves the existing worker-exec rejection before parking and delays sibling teardown/rearm/image replacement until successful parking. The proposed retained stages must keep these ordering points and the existing unexpected-exit rejection.

`ProcessActionContinuation::finish` explicitly skips restoration and result staging for cancelled outcomes. The ordinary Host loop converts the terminal outcome to its existing group status or worker-success cancellation; the Tool loop uses cancelled_tool_thread_status, retaining established executor/group status. Injection converts it to HandlerSignal::ThreadCancelled and suspends the callback for its owning driver to consume. In these inspected production consumers, the cancelled outcome's placeholder syscall_result zero is not installed as a guest result. Pending children from earlier injections continue through the preexisting handler cancellation policy; no new child has been created by the cancelled action before its parking point. I found no additional concrete status/ownership defect in this bounded trace.

## Opposing controls needed for the fix

- An already closed gate yields Pending from async preparation with zero byte changes and zero KVM dispatch; reopen permits exactly one byte update and one retained entry attempt.
- Closure after a successful preparation retains that preparation; repeated wakes do not repeat action construction, snapshots, TID stores or child creation.
- A stop future that becomes ready while the wait is pending is consumed once; use a future that explicitly rejects any poll after Ready.
- Retain an actual unexpected parking exit while unparking is closed, then deliver cancellation/poison: no second KVM run and no conversion to ordinary cancelled success.
- For the helper alone, prove closed None/no effect, reopen/actual write, typed poison/cause and unchanged healthy out-of-bounds behavior. Existing assertions remain active.

The reported passing source-1 memory/entry/clock controls do not exercise these process-action futures. No result is transferred from those controls to this review.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

In the inspected delta, old ProcessActionOutcome test literals add cancelled=false while retaining their assertions; the new cancelled result is checked before old result/image handling. No inspected comparator, tolerance, skip or failure classification was weakened. The two findings above require correction rather than a relaxed test. This report changes no source or evidence result. **Disposition: changes requested for the current parking mechanism; no independent approval of the memory author's own code or the complete gate.**
