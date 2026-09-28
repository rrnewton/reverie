Review target: `rrnewton/reverie` `60f2d369b49e6ffbc2b2d9d0f0e55fead0ba6b09..7d1f29f973978e3ea4bbda8768b855a91fe5c6ed`, tree `4f66c7d04e92640b3ddb883d703f3da59a79de42`.

## Findings

No code findings.

F1 is resolved:

- The child-start vector is fresh, retained across callback destruction, and asserted empty before finalization or every outcome arm at `parked_signal_runtime.rs:304` and `parked_signal_runtime.rs:351`. This covers `Returned`, `RuntimeError`, `ParkedCancelled`, `RunFailed`, finalizer errors, and the catch-all.
- Callback and failure futures are actually destroyed and their panics retained before the assertion (`runtime.rs:2224-2257`).
- `DequeueNotification` is installed before `handle_signal_dequeue` (`parked_signal_runtime.rs:44-69`) and unconditionally rejects injection (`:3-7`). `KvmGuest::inject` returns `ENOSYS` at `runtime.rs:1440-1442`, before execution at `:1461` or publication through `complete_injection` at `:1479`.
- The only production publishers are fork at `vm.rs:3026-3039` and Tool `CLONE_THREAD` at `vm.rs:3349-3359`; neither is reachable under that guard. Tail injection is independently rejected at `runtime.rs:933-936,1517-1527`.
- The real test exercises both `SYS_fork` and `SYS_clone` with `CLONE_THREAD|CLONE_VM|CLONE_SIGHAND` through production flush (`signal_cleanup_completion_tests.rs:352-523`).
- The preseeded negative control proves cancellation returns without starting the gate, proves the production invariant rejects it, then explicitly cancels and observes `ChildStartCommand::Cancel` (`:201-273`).

Ordinary callback failure ordering remains `BeforeCallback`; only consuming signal cleanup uses `AfterReadyCleanup`. No scheduler request, run-queue, signal-targeting, virtual-time, record/replay, or Linux-visible syscall semantics changed.

## Goalpost-moving assessment

- Assertion weakening: none. Existing assertions now additionally check caught output, empty panic sets, and exact `EFAULT`/success values.
- Tolerance, exemption, skip, or comparator widening: none. No thresholds, ignores, allowlists, or comparators changed. The cfg on `register_child_process_with_gate` matches its test/native-support-only callers; production still uses `register_child_process_with_panic_owner`.
- Relabelling failure as success: none.
- Deleted checks: none. Eight new focused tests were added; no old case was removed.

## Verification and limitations

- Complete four-file diff reviewed; `git diff --check` passed. Patch identity matches the exact head, and tracked index/worktree remain clean.
- Preserved author evidence claims focused 8/8, parallel full library 744/744 in 7.61s, formatting, default check, strict Clippy, and native-test-support check. The parent separately reported serialized 744/744 in 32.59s.
- Those execution results lack preserved raw command/status transcripts, so I could confirm source identity and internal count consistency, not independently authenticate the runs. I did not rerun broad tests or Hermit.
- The native-support check closes prior F2 and full 744/744 coverage closes F3 if the reported executions are accepted.

## Residual risks

No exact-head Hermit consumer run or cross-backend parity comparison is evidenced. This approval is scoped to this Reverie component, not general KVM parity. Future child injection during dequeue must replace the hard invariant with explicit ownership cleanup, not remove or weaken it.

Verdict: **APPROVE** exact head `7d1f29f973978e3ea4bbda8768b855a91fe5c6ed`.
