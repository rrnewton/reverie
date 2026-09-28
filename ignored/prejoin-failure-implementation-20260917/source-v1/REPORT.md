# Reverie fatal Tool/RPC failure candidate

This is an uncompiled Reverie implementation candidate for publication before physical join. It is not an integration completion or a scheduler approval. The Hermit half remains read-only pending the root coordinator's release, and both halves must be reviewed and land together.

Repository: Reverie. Write destination: `/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917`, branch `codex/kvm-proc-fd-identity-20260917`.
Base: `114b309413612fafc2657c74e83811c71aac7b19`, tree `16b49eb8c6e85b6b63ac8d2b43179de663726083`.

The former head `696f0476aa46cf29e31b947a89379d80b4542ce3` is preserved by an absent-only rescue ref and `../prior-head.bundle`. All six old authored source files were byte-identical to origin/main before the normal empty-suffix rebase. Existing HANDOFF and ignored evidence remain intact. No shared-checkout edit, destructive SCM operation, commit, stage, push, pull-request creation or merge occurred.

## Frozen identity

`binding.json` binds all 9 changed or new source files. SHA256: `f09d15c72953ed8c4d6f007b4f6a6314801e5206cd891f84a1635bec818c16c2`.

`candidate.patch`: 100238 bytes, SHA256 `2544ff62007ce6e1ad47d83d61d5b1aa7b2fd01dddacbc51d6025d5d499610a0`.

`tracked-source-manifest.json`: 2585 tracked-source or Git-link entries plus the two new Rust files included in that count, SHA256 `c1524f097cdcbb6b2303d9de53621218c2fa478301a5fb5f84d2c6470170704b`. Git links are dependency pins; this manifest does not claim expanded submodule contents.

Frozen file copies are under `source/`. Source has not changed since binding. Preparation performed targeted rustfmt and `git diff --check`, both successful. There has been no compilation, test inventory, test execution, KVM guest, or Hermit invocation. The abandoned source-edit marker mismatch was retained in the preparation script before correction; it wrote no source and was not a build failure.

## Production changes

`reverie/src/tool.rs` adds defaulted `GlobalTool::report_backend_failure(BackendFailure)` and `wait_for_backend_failure()` hooks. `BackendFailure` carries actual pid, tid and phase metadata. It is neither a syscall response nor the typed error transport. Report is synchronous: a Tool must make terminal cleanup safe before returning. Each wait is an independent subscription.

`reverie-kvm/src/failure.rs` owns the first typed cause and a run-local shared oneshot notification. The global reporter retains only Weak ownership. Publication calls the synchronous Tool hook before publishing locally or exposing the primary cause to cancelled-gate cleanup. This also works for Tools that use the default global hooks. Constructed state is retained outside the OS spawn closure by `spawn_owned`; a refused spawn returns the exact state to its consuming caller.

`runtime.rs` adds a separate `HandlerOutcome::RunFailed`. The actual `drive_handler` polls the global/local failure subscription before and after polling a Tool future, before accepting a response or releasing child starts. All production calls subscribe. A fatal runtime error is reported before worker cancellation or physical join. Setup after ThreadState construction is inside the same consuming finisher as normal execution. The actual thread-exit hook reports its error before the process-exit hook can block. Existing worker-before-leader and owner-before-independent-fork hook ordering is retained.

`finish_unstarted_tool` consumes cancelled or spawn-refused initialized child state without `handle_thread_start`, guest execution, or invented scheduler admission. It feeds the same physical retirement and consuming exit hooks. A child that has not been constructed is distinguished by moving reorderable fallible work before ThreadState creation; after construction the spawn helper or worker owns that state exactly once.

`ToolRunCompletion<G>` and `run_static_elf_with_tool_completion` retain GlobalState alongside a typed runtime result, so Hermit can complete its scheduler/global cleanup even on failure. The old API remains as a wrapper. An ownership failure after physical joins retains the original execution cause plus a separate ownership diagnostic.

`vm.rs` propagates run failure context with actual identities into thread and fork backends. Cancel and disconnected-gate paths consume initialized child state. Spawn errors recover it. Cached worker errors are typed Arc values instead of strings.

`executor.rs` uses the existing normal join path with a failed-only option. `ElfExecutor::join_child_processes_after_failure` calls `finish_child_processes(true)`, which cancels every pending owned or transferred child gate before joining the first handle. Normal `join_all_child_processes` still starts children. Every handle is joined and every returned diagnostic retained. These handles are OS **threads** executing guest child-process backends; they are not OS process handles.

`error.rs` introduces `SharedFailure`, `WithCleanup`, and contextual `Cleanup` wrappers. `primary()` provides the original typed cause and standard source chains remain available. `RunAborted` is a distinct internal failure outcome. None of these become an ordinary guest errno, successful status, clock reading, output byte, or virtual signal.

## Required review corrections

1. Selected-transaction closure before cleanup wakes: the Reverie synchronous publication contract and ordered local wake are implemented. The actual Hermit mutex-protected terminal transition is still required. A default hook cannot supply that behavior.
2. Cancel after child construction: fork and thread paths now call consuming cleanup instead of dropping ThreadState or starting the guest. Hermit must accept cleanup both after registration and before its delayed detpid has been assigned. Native evidence below does not establish that real Detcore behavior.
3. Setup/preamble/spawn failures: postconstruction fallible setup feeds the consuming finisher; reordered fallible setup stays before ThreadState creation; OS spawn refusal returns its exact state owner. Real initialized backend cleanup requires the coordinated tests below.
4. Typed primary plus separate cleanup diagnostics: source implements this through worker, owner and child aggregation. The primary-plus-hook-error native control asserts typed variants. Physical worker diagnostics no longer stringify the cause.

## Native controls proposed, not executed

`selected-tests.json` contains 8 new tests and 19 unchanged nearby tests, 27 selected total. The eight new controls cover:

- Two pending independent failure subscribers and a late subscriber, first-error retention, and recovery of sole GlobalState ownership.
- A blocked synchronous Tool publication: local notification and published-primary access must remain pending until the Tool returns.
- An actual refused OS thread spawn with an impossible stack allocation, recovering the exact initialized state without running it. This uses no host limit or configuration change.
- A pending actual `KvmGuest::send_rpc` Tool future, actual `drive_handler`, and actual `GuestThreadGroup::join_workers` OS join. The worker never polls the cancellation flag. Publication must release the pending RPC and complete the join.
- The same production RPC/join path with a typed GuestClock primary and a separate consuming thread-hook EIO.
- The adjacent successful RPC response 37, exact status 37, and worker, leader, process hook ordering with no failure report.
- Failure preempting an already ready callback while its pending child-start gate remains closed; cancellation produces Cancel rather than Start.
- Two actual OS threads owned by an `ElfExecutor`. The first child cannot return until the second has consumed Cancel. The test calls the exact production `join_child_processes_after_failure` helper, proving that all gates must be resolved before its first join and that both typed errors survive once.

The RPC/join and two-child controls have a 2 second watchdog that only rescues/reaps a failed control; a watchdog response cannot satisfy the success assertion. Removing the driver subscription or moving publication behind the join must fail the RPC control. Cancelling children one at a time around joins must fail the two-child control. No fault-mutation run has been executed yet, so those are source expectations awaiting measurement.

All real owned handles in these controls are OS thread JoinHandles. Test guest memory and the KvmGuest RPC adapter do not create a KVM VM or execute an instruction. The 19 existing controls cover pending gate release/refusal, ordinary negative syscall conversion and restart handling, thread/group cancellation, child transfer ordering, and error retention. Existing assertions and selected regression bounds were not weakened.

Coverage limits: no real Detcore registered or unregistered child consuming-accounting test, full initialized VM setup-error cleanup test, selected scheduler transaction test, clear-TID waiter test, final clock test, or successful daemon shutdown has run. The native refused-spawn test proves ownership recovery, not all downstream Tool cleanup. These remain required coordinated coverage; the eight controls are not a replacement for that coverage. Native results cannot be counted as guest determinism or cross-backend parity.

Two preparation concerns are exposed for the source review: the new typed worker cache retains the original error but its display no longer adds the previous worker-TID text; and a precondition timeout before the RPC watchdog is installed can panic before the test explicitly reaps its worker. The service lifetime bound still contains the test, but source review should decide whether either needs correction before execution. No failing control has been reclassified or omitted.

## Concrete execution caller awaiting release

Caller: `../cargo-v1/launch.py`, SHA256 `fa246157f0855c83f6a40201ee1941d6393607736966daac6088d74bdc746980`.
Plan with complete argv, input identities, selectors and environment: `../cargo-v1/plan.json`, SHA256 `d94d92c619a6239964a44c0f1a1355886056a4d375ce4a25adf5f566b33e27d1`.
Reservation: `../cargo-v1/reservation.json`. The caller has only been parsed as Python source; it has not run.

Unchanged observer absolute path:
`/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916/ignored/recovery/main-increment-independent-review/closed-standard-input-20260917/measurement-observer/observer.py`
SHA256 `f10ab861f262dbbd18295d92e59e05174299b72b397f58de844ee1725266eae6`.

The exact reserved output paths are:

- `/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916/ignored/recovery/main-increment-independent-review/closed-standard-input-20260917/measurement-observer/measurement-prejoin-native-20260917/cargo-v1/compile`
- `/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916/ignored/recovery/main-increment-independent-review/closed-standard-input-20260917/measurement-observer/measurement-prejoin-native-20260917/cargo-v1/list`
- `/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916/ignored/recovery/main-increment-independent-review/closed-standard-input-20260917/measurement-observer/measurement-prejoin-native-20260917/cargo-v1/native`

Compilation: pinned nightly 2026-07-29 Cargo, `test --locked --offline -p reverie-kvm --lib --no-run --message-format=json`, 600 CPU seconds and 900 wall seconds, 16 MiB fatal stderr cap and caller read cap. The target is the new absent `target/prejoin-native-v1`. Existing Cargo.lock is 67056 bytes, SHA256 `d432b018c022722ac6a8d121a652402dd6089f0eb92e5ab6bdbc489992e00469`; it is separately bound. If it is stale against current manifests, the locked invocation must refuse and retain the refusal. That does not authorize a silent lock mutation or retry.

List: only the compiled test executable identified from Cargo JSON, 5 CPU seconds and 15 wall seconds, 1 MiB fatal stderr cap/read cap. Inventory is not execution.

Native: that same byte-checked executable, the exact 27 names and `--exact --test-threads=1 --nocapture`, 30 CPU seconds and 60 wall seconds, 1 MiB fatal stderr cap/read cap. Success requires all 27 passed, 0 failed, 0 ignored, 0 measured, plus complete observer accounting and an independently read empty inactive service. A failed or incomplete stage stops later stages and remains preserved.

The unchanged observer verifies 16 GiB memory, zero swap, and independently caps each retained stdio file at 64 MiB. Cargo and third-party jobs are 2. The caller uses the prior reviewed minimal environment, pinned toolchain paths and an owned temporary directory; existing Cargo configuration is bound and absent configurations must stay absent. It checks source and executable identity before and after every stage. No guest run is included.

## Hermit edits still required

The GlobalTool reporter must synchronously acquire the real scheduler mutex, make the one terminal transition, and close any tentative selected transaction exactly once before either the global wait or the Reverie local subscription releases consuming cleanup. It must not run a second ordinary undo/step2, grant another guest turn, advance timers, or counterfeit a guest response. The wait needs independent subscriptions; a single latest-waker Ivar is not sufficient.

The terminal path must keep consuming RPC cleanup available with correct pid/tid/MmId and distinguish an initialized child not yet registered from one already registered but held at its start gate. Cancellation must not invent registration. A missing delayed child detpid needs the reviewed identity fallback in consuming cleanup. Final errors must retain their typed cause.

Hermit's `run_kvm` must consume `run_static_elf_with_tool_completion`, finish scheduler/global cleanup through the retained GlobalState, then return the original typed failure. Neither the completion API alone nor a native mock Tool proves this implementation. Parent-reviewed primary scheduler sources and independent actual Claude plus Codex review of the final coordinated source are required before landing. Whole-DAG receipts are not a hard landing prerequisite under the current owner directive.

## Goalpost-moving assessment of this preparation

Assertions weakened: no. Existing tests only gained a permanently pending failure argument or an explicit unexpected RunFailed arm where required by the API.
Tolerance widened, exemption added, case skipped, comparator relaxed: no source-test change of this kind. The new plan selects the stated 8 plus 19 native population rather than claiming all library or guest tests ran.
Failure renamed or relabelled as a pass: no. RunFailed remains distinct from ThreadCancelled and returned guest errno/status. No test execution result exists.
Check deleted instead of satisfied: no. Typed aggregation retains separate cleanup diagnostics and existing negative-result assertions.

This is an implementation and execution-plan handoff for source review, not an independent approval verdict.
