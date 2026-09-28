Prepared parent captures before asynchronous action dispatch

This increment removes the synchronous admission wait from both Host and Tool thread-parent captures and supplies asynchronous completed-syscall-boundary capture for the three runtime caller sites. Closed admission invokes no capture closure. The parent action remains owned while waiting, and each successful capture returns owned registers/frame bytes exactly once before the existing parking step.

GuestMemory::try_read_with binds one short CopyAccess to its exact memory handle through a private RawMemoryRead. Nested raw reads reuse that token. No token or accessor escapes the synchronous closure, and the actual closures acquire no allocation lock. A retained poison is returned as the original typed Error before or after copying. No guest EFAULT conversion is introduced.

KvmBackend::prepare_action_read registers gate and cancellation notification before checking poison, stop and existing cancellation predicates. A completed stop is consumed immediately, including when selected from the wait; an unfused completed future is never repolled. Closed admission is retried internally. Public Option::None means terminal cancellation, not an unfinished attempt. Existing cancellation status and worker/root distinction are unchanged.

The boundary adapter retains the original accepted transport RIP forms, configured-opcode check, register normalization and frame bytes. Existing synchronous boundary helpers and the complete parking state machine remain unchanged. The now uncalled blocking bootstrap park-byte helper is removed; the actual nonblocking park helper and its byte/address calculation remain. No warning suppression was added.

The separately bound runtime proposal covers signal-boundary callback setup, subscribed syscall setup and pending process-action dispatch. It preserves the owned action across waiting and uses existing cancellation returns; the two already exposed Tool scratch paths hide it on cancellation. The author did not write runtime.rs. The coordinator reported reading and applying all three proposal hunks successfully.

Two new declarations are prepared, without execution. The memory control proves closed admission has no closure effect, reopening runs once, a close during two nested reads does not cause reentry, the token retires before close completion, and poison after copied bytes or before capture retains its cause. The VM control exercises both actual capture adapters against closed admission, completed unfused stop, cancellation status 7, poison and reopening; it also retains unsupported RIP/opcode checks. It requires /dev/kvm and register ioctls but performs no KVM_RUN. Exact selected names are in TEST-PLAN.json.

PRESERVATION.json proves every original test-suffix byte remains in order with only one inserted test in each of memory.rs and vm.rs; bootstrap and proposed runtime tests are unchanged. Existing assertions, ignored populations, comparisons and failure classifications are not weakened. The source census found no caller of the removed blocking helper. Formatting succeeded; there is no compile, test, guest, full-gate, independent approval or parity claim for this increment.

Scope remains the asynchronous prepared-action paths. Other synchronous MemoryAccess calls, subsequent continuation restore/staging, sparse snapshot behavior, retained kernel operands and root-owned callback/failure ownership are not converted or qualified by this patch. The full composed source and actual results must be bound separately. These three source paths are released after this packet is frozen.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it
