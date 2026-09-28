# Public Tool memory-failure controls

Four new test declarations and narrowly scoped cfg(test) observers are implemented and released. No compiler, formatter, test, guest, SCM, or network execution was launched by this author. Compilation and execution remain unverified. Root will add the include inside vm::tests and run the qualification.

Write destination: Reverie `/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918`, branch `codex/kvm-parity-land-20260918`, underlying base `91110d249ffd8957267d71fab8c83d9636105efe`. Root's preceding frozen qualification was source manifest `6ce3110dbbd6dac16b9a9c640c5a0d47d40f1ecc2264fbb69336252e68cb3dd1`; its stated183 passes are parent evidence, not execution by this author. Product source was left untouched while that qualification ran. Root then explicitly released the source freeze and authorized the executor observer.

Changed paths are exactly:

- reverie-kvm/src/memory.rs: cfg(test) per-handle observers and invocation points.
- reverie-kvm/src/executor.rs: cfg(test) dispatch observation at the real ElfExecutor::execute entry, before its effects.
- reverie-kvm/src/vm/entry_public_tests.rs: new controls, wrapped in entry_public_tests for inclusion inside vm::tests.

No runtime.rs, vm.rs, clock.rs, entry.rs, lib.rs, README, or existing test was edited. The existing usize-only after_vector_copy callback remains present and unchanged. Source copies, prechange copies, and an exact additive patch are retained. The root can integrate the module by adding `include!("vm/entry_public_tests.rs");` inside vm::tests.

## Actual paths and cases

Each of these four independently selectable declarations has six subcases: vector read and vector write, each with poison before the first byte, poison after the first real two-byte portion, and a healthy counterpart.

- vm::tests::entry_public_tests::public_direct_memory_failure_refuses_ready_rpc
- vm::tests::entry_public_tests::public_direct_memory_failure_refuses_injection
- vm::tests::entry_public_tests::public_elf_memory_failure_refuses_ready_rpc
- vm::tests::entry_public_tests::public_elf_memory_failure_refuses_injection

If all complete, that is24 subcases:16 fatal cases and eight healthy cases. These are planned coverage counts, not executed results. A failed subcase short-circuits the remaining subcases of its declaration; no report should count unexecuted later cases as passes.

Direct tests invoke actual KvmBackend::run_with_tool with an installed getpid hypercall followed by HLT. ELF tests invoke actual run_static_elf_with_tool_completion with minimal_test_elf containing exit(0). The actual Tool's handle_thread_start obtains Guest::memory and performs the real vector MemoryAccess operation. It catches the required EIO and immediately attempts either an ordinary ready RPC or injected ftruncate. No synthetic KvmGuest, handler driver, fake failure publisher, or substituted ELF executor is used.

Fatal cases require zero ordinary RPC constructors and polls, zero target syscall dispatches, zero target file-length effect, no ordinary completion, zero observed guest exits, and retention of the original shared typed cause. Copy effects are exact: a read before poison leaves both buffers untouched; a read after the first portion preserves AB and the untouched second buffer. The corresponding writes leave ABCD or WXCD in guest backing. Healthy cases transfer all four bytes and successfully perform the ordinary operation, return normally from thread start, then enter and complete the installed direct/ELF guest. They require at least one actual guest exit, no failure publication, and WXYZ for a completed write.

The injection target is a valid writable owned memfd at guest descriptor9; ftruncate to7 bytes does not depend on another guest-memory copy. A test observer counts the actual ElfExecutor dispatch independently of observing that file's length. The direct path counts at its real caller-supplied SyscallExecutor, then performs File::set_len. Ordinary RPC construction is counted outside the returned async future, and polling is counted separately.

Both paths require the actual thread-start future's Drop before backend reporting. The hook's issuing OperationOrigin supplies the real callback destruction receipt, which must also be complete at reporting. Direct's by-value GlobalTool path requires no FailureContext; ELF requires its actual FailureContext. The observer records and asserts that distinction rather than treating direct reporting as RunFailure publication. Actual on_exit_thread and consuming on_exit_process each send a consuming KvmGlobal RPC exactly once, after failure publication for fatal cases or without publication for healthy cases. Status, thread-state ownership, and event ordering are asserted.

## Observer scope

The copy observer is stored on a GuestMemory handle under cfg(test). At the first nonempty vector admission it receives total0 and the issuing handle's current EntryOrigin; it receives the same issuing attribution after each actual completed portion. This avoids binding the observer to an ownerless setup-time origin. It is armed only for the Tool's intended copy, invokes the real EntryGate::poison, and preserves existing production copy admission and byte-transfer code.

The observer stores only the weak OperationOrigin receipt and whether a FailureContext was present. It does not retain a FailureContext or RunFailure in GlobalTool observation state. There is no publisher or owner-registration shortcut. Observer calls occur before copy admission or after the short backing/access locks have returned. The existing usize-only copy hook is preserved exactly.

The syscall observer is also per handle and cfg(test), carried by the real memory handle passed to ElfExecutor::execute. It runs before syscall effects and filters the armed ftruncate request. It uses no global dispatch override and cannot affect a different backend's memory handle. The static ACTIVE slot is solely for this fixture's ordinary Tool construction and is serialized across its four declarations, matching the existing public owner-test pattern.

After the entire public run returns, the test inspects the retained backing bytes at the fixture's low identity-backed address. No guest or callback is concurrent at that point. This observation checks real prefix effects even when the production mapping gate is poisoned; the test does not bypass admission during the run or claim general mapping-mutation safety.

## Review and limits

Code-review skill was read and applied to the bounded source comparison. No assertion was weakened; no tolerance, exemption, skip, comparator, status classification, or existing control was changed or removed. Existing production code is unchanged in non-test builds. These are additional requirements rather than replacements for the earlier adapter controls.

The controls exercise the actual public driver cleanup/reporting paths but fail in thread start, before guest entry in fatal cases. Healthy counterparts intentionally run actual guest instructions. They do not exercise a failure after a guest syscall callback, cross-worker A/B/C publication, failed host spawn, a closed entry gate, outstanding hypercall completion, arbitrary retained views, or changed mappings. Those matrix rows remain separate obligations.

No test was executed, so compilation, runtime behavior, exact exit counts, and all four declarations remain unqualified. Root's include change and any formatting/compiler corrections require their own source identity. No backend parity or determinism percentage follows from these controls.
