# Actual guest-worker spawn refusal controls

Author implementation packet, not a review verdict or test result. The parent accepted the plan before source edits. No formatter, compiler, test, guest, model, network or SCM operation was run by this author. Source ownership is released for root composition and qualification.

## Bound source and scope

Slot `/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918`, branch `codex/kvm-parity-land-20260918`, underlying Reverie base `91110d249ffd8957267d71fab8c83d9636105efe`. The concrete baseline is the ongoing private integration, with `BEFORE.json` and complete copies of the inspected failure.rs/vm.rs/runtime.rs under `before/`. It is not the landed commit alone.

Authored source snapshots are retained under `after/`:

- `reverie-kvm/src/failure.rs`: SHA256 `1e483827961f13251d8fa7f6169c6c6775a5e95214029e5f91cd98ab3350aeb8`, 52,553 bytes.
- `reverie-kvm/src/vm/entry_spawn_tests.rs`: SHA256 `5ebee1ac319e8d72e03ed4c3fcb53e1816181333333aa6a431891d8216a1e507`, 20,796 bytes.

`AUTHOR.patch` contains precisely the delta from those baseline bytes plus the new test file. Root must include `vm/entry_spawn_tests.rs` within `vm::tests`, as the neighboring public controls do; this author did not modify vm.rs or any other product file. Root formatting or composed successors do not alter these retained source copies.

## Real refusal and recovery

The new `cfg(test)` module at failure.rs:387 holds a one-shot refusal on the current host thread. The guard records its admitting thread, preserves a prior admission and restores it on Drop. Normal Drop asserts exactly one consumption and an actual recorded OS error. During an already failing assertion's unwind, restoration still happens while that original panic remains a test failure; the guard does not create a second diagnostic panic solely for incomplete setup. The public controls independently assert consumption again after callback destruction and final completion.

The seam changes only the next admitted Builder's stack size to `usize::MAX / 2`, matching the unchanged existing `failure::tests::refused_host_spawn_returns_exact_initialized_owner` control. `Builder::spawn` actually runs and returns its own error. The original state is recovered by the existing `spawn_owned` Err arm. The observer records raw errno, ErrorKind and message from that actual returned error without replacing it. There is no injected synthetic Err, state extraction shortcut, process-limit modification or global resource-exhaustion loop. A std OS error has no separately owned payload allocation to compare; the final public result must retain its exact raw errno/kind/text as primary and exactly one host-error diagnostic.

The guard is armed inside the matched parent getpid callback immediately before `guest.inject` creates the Fork or Tool-owned clone. No other spawn_owned call lies between that admission and the producer under test. The actual Fork and Tool Thread Err arms in the inspected vm.rs are unchanged: they retain a future owning the initialized child, return the original HostIo error, and consume the child later through the outer cleanup queue. This source packet does not replace those producers with constructed helper calls.

## Two public controls and one host neighbor

The two public declarations are:

- `vm::tests::entry_spawn_tests::public_elf_refused_fork_retains_child_until_published_cleanup`
- `vm::tests::entry_spawn_tests::public_elf_refused_tool_thread_retains_child_until_published_cleanup`

`run_case` at entry_spawn_tests.rs:297 installs a minimal static ELF whose real userspace getpid reaches a subscribed Tool callback; an unreachable UD2 follows it. The callback at line 193 injects the actual Fork or Clone syscall. The Thread case uses Tool ownership and ordinary CLONE_VM/FS/FILES/SIGHAND/THREAD flags, with a guest stack derived from the stopped parent registers. These cases perform real guest execution and may enter the existing action parking path. They do not claim zero KVM_RUN or an exact ioctl count; the test requires positive exit statistics and forbids a child start callback.

The ThreadState is `Arc<u64>`, using Reverie's existing serde rc contract without a new dependency. Parent and child have different values and allocations. The controller stores only Weak witnesses. The parent callback destructor at line 85 requires one initialized child, zero child consuming polls, zero child starts and a live child with exactly one production strong owner. Initialization observes the actual parent state and occurs before callback destruction (line 156).

The actual child `on_exit_thread` at line 217 must run only after that callback destructor and an outer `report_backend_failure` notification. It receives the exact original Arc and waits on an unreleased oneshot, returning Pending. While it is pending, the controller checks that the child and parent states remain live, parent hooks have not begun, no child process hook has run, and the first publication belongs to parent pid/tid 1. The controller then releases the real hook, which drops its unique child state and returns ENOSPC. The final weak witness must be dead. The Fork child additionally runs its actual process hook and returns EOWNERDEAD; the Thread child must never consume the shared process hook. Parent thread/process hooks run once afterward. Exact event ordering, per-hook counts and both retained cleanup diagnostics are asserted. Polling loops have five-second host deadlines; each individual guest operation remains subject to the root's admitted bounded runner.

The third declaration, `vm::tests::entry_spawn_tests::unarmed_spawn_owned_starts_exact_initialized_state`, is a host-only positive neighbor. An unarmed ordinary spawn really starts once and returns the exact initialized Arc. It is not a public successful clone/fork control. Existing production neighbors remain unmodified, including `vm::tests::production_wrapper_cleans_fork_after_boundary_restore_failure`, `vm::tests::production_wrapper_cleans_tool_thread_after_boundary_restore_failure` and `vm::tests::started_tool_worker_observes_cancellation_after_start_lifecycle`. No fresh execution credit is claimed for those neighbors here.

## Preservation and limits

`PRESERVATION.json` verifies that removing only the newly added cfg(test) module and two cfg(test) call sites from failure.rs yields the baseline byte-for-byte. Thus the non-test path and all pre-existing failure tests remain unchanged. The second authored path is entirely new. No existing assertion, tolerance, comparator, exemption, skip, classification or gate was relaxed or deleted. New failures are required to remain failures; the controls do not drop a backend or convert refusal into guest success.

There are three unexecuted test declarations, comprising two actual public ELF failure routes and one host-only successful helper route. This is not evidence that they pass. Root owns module inclusion, composed source freeze, formatting, compilation, exact test selection, bounded execution and review. There is no Hermit/Detcore comparison, matrix/parity result, full scheduling proof, external review, source approval or landing authorization in this packet. It also does not claim arbitrary future-cancellation or double-panic-abort recovery.
