Injected captured-boundary errors now return before pending-child cleanup

Implemented only the assigned vm.rs action-boundary region and one adjacent additive control. No branch operation, commit, compiler, formatter, test, payload, KVM_RUN, network or model invocation was performed. Runtime caller integration remains root-owned.

The new run_injected_process_action_with_tool_at_boundary runs the same process-action implementation and calls finish_injected_tool_process_action_at_boundary. That finisher delegates directly to ProcessActionContinuation::finish. It returns a failed action or failed boundary restoration as the complete typed error without publishing failure, canceling start gates, or joining pending children in the still-active parent callback.

The existing run_process_action_with_tool_at_boundary retains its original outer-owner cleanup behavior. Its finisher uses the same continuation-only helper, then calls cleanup_unstarted_tool_children_after_error on error as before. It still publishes, preserves ExecWorkerTeardown, cancels gates and joins exact registered children. No failed-spawn arm, executor storage, runtime, failure code or other VM implementation area was changed by this author.

Continuation semantics remain delegated to the existing code: action errors return without restoration; cancellation returns its existing terminal disposition without restoration; successful returning actions restore the original captured transport and stage the same actual result; successful exec keeps the replaced image. This change creates no success return or guest errno for fatal failure.

Required runtime caller edit

RUNTIME-CALLER.patch contains the sole method rename in StaticElfSyscallExecutor::complete_injection's captured SignalBoundary/SyscallBoundary arm:

    run_process_action_with_tool_at_boundary
    -> run_injected_process_action_with_tool_at_boundary

Apply only that injected call. The outer pending-action dispatch in run_static_elf_process_with_tool must keep run_process_action_with_tool_at_boundary. Runtime already retains pending_child_starts in an Arc outside the callback. KvmGuest::inject places complete_injection's Err into HandlerSignal::RuntimeError and remains pending; drive_handler selects that owned error and destroys its owned callback future before the outer driver runs cleanup.

Caller integration constraint discovered during this change: the regular syscall and signal-hook drivers currently call hide_tool_scratch(...)? before matching HandlerOutcome (SOURCE-1 runtime.rs:3985 and :3195). If hiding fails, that '?' can drop an owned RuntimeError. Previously this captured-boundary action helper had already published its error; after this separation it intentionally has not. Root must preserve/publish the owned RuntimeError after callback destruction and retain a simultaneous scratch-teardown error as cleanup, rather than replacing the original cause. complete_injection itself already preserves action error over expose_tool_scratch failure because it checks action_result first. This author did not edit runtime to settle that caller obligation.

From-fault trace

run_process_action_with_tool_from_fault does not call the captured-boundary error cleanup helper. It stages the fault continuation, runs the action, returns existing cancellation directly, then attempts fault restoration; an action Err remains the returned error and a restoration error is returned when the action itself succeeded. It already leaves pending children for the outer fault/signal driver on action error. That behavior, including its existing treatment of a secondary restore error, is unchanged. No new publishing/canceling/joining path was added there.

This change does not address caught-panic ownership of failed-spawn futures, earlier/later global teardown policy, or arbitrary external cancellation of public futures. Those remain root's separate composition work. Once callback destruction occurs, the existing outer helper still synchronously joins prior pending children; any stronger ordering among those joins and the new failed-spawn cleanup queue must be decided by the outer owner.

Prepared control

vm::tests::injected_boundary_error_defers_each_child_until_callback_destruction

The control registers a real host fork-handle fixture and a real host thread-handle fixture behind the same ChildStartGate types and registries used by production. It keeps a callback Drop guard alive while invoking the production injected finisher with a typed HostIo(EAGAIN) aggregate, testing both the direct error and ExecWorkerTeardown wrapper. An invalid restoration address ensures an action error cannot accidentally be replaced by a restore error.

Before dropping the guard it requires exact error Arc identity, no backend publication, pending gates, zero child consumption, and both registered handles retained. After guard destruction it invokes the existing outer cleanup helper. Each child asserts publication and callback destruction have happened, receives CancelAfterFailure exactly once, and returns either the bare marker or a typed real cleanup error. The parent must preserve the original cause, original aggregate and exec wrapper, retain the child's real error, remove both handles and gates, and avoid a second publication or second child consumption on repeated outer cleanup.

The control uses KVM setup and requires /dev/kvm without a new skip, but performs no guest KVM_RUN. It is a finalization/ownership control; it does not force an actual failed host spawn or drive complete_injection end-to-end. No execution is claimed. Root must perform composed compilation and bounded control execution after applying its caller changes. Existing outer-boundary tests remain byte-for-byte intact and continue to target restoration/cleanup behavior.

Author structural checks

PRESERVATION.json records that the complete old test suffix is unchanged except the new control: 74 to 75 direct #[test] declarations, with no deletions or replacements. No assertion, tolerance, comparator, skip, classification or existing gate was weakened. SOURCE.patch is the complete author diff relative to BASE-vm.rs. At freeze time the live vm.rs equaled PROPOSED-vm.rs and there were no foreign changes to exclude; the frozen proposal, not later concurrent live bytes, binds this handoff. No self-review or approval verdict is claimed.

Exact identities

BASE-vm.rs: 371966 bytes; SHA256 b6eb3ffc679ec1044dff9c57711769350734820a77f7ca9267cb518d22189494.
PROPOSED-vm.rs: 380752 bytes; SHA256 1301e31879ac21073c8a7d00f7dea2d382734f66f99097b0f00bbeccc74575ef.
SOURCE.patch: 10028 bytes; SHA256 772c2b1f3d09c430193717a615a80fe485dacf90cf5d2923918c6f65ef2b9468.
RUNTIME-CALLER.patch: 551 bytes; SHA256 746d76b78779cfce69715af2f71d70e0f9c4960bf1b51490152895a3fd9f412f.

Source ownership in the assigned vm.rs region is released with this packet.
