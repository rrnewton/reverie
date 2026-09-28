Proposed correction: retain failed-spawn consumers through owner panic cleanup

This is a source-only correction proposal against frozen SOURCE-1, not an applied patch, a completed implementation, or approval of successor source. The smallest sound correction has two parts: catch each retained consumer independently, and give the owner that survives callback destruction an explicit panic-cleanup outcome. Adding one drain call to the existing worker catch does not by itself cover fork/root owners or a consumer that panics while the drain owns the remaining vector.

Exact source target

L=/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918. All product references below refer to L/ignored/kvm-entry-owner-composition-v1/source-1/reverie-kvm/src, never mutable live source.

- runtime.rs SHA256 c40d3e8608622f5938932926c6faf0d9bd7f31d56456f196adb16cb5c0eb1a04.
- vm.rs SHA256 b6eb3ffc679ec1044dff9c57711769350734820a77f7ca9267cb518d22189494.
- executor.rs SHA256 bad4de60e4016f3e445c6fe484cce433b25f95758c8ca0272fdf72e46b183b9d.
- failure.rs SHA256 848ef6a5f74fdc26cd9109af44f016b08ba5b3c3f8267e53978bd49909695aa9.
- SOURCE-1.json SHA256 391d0cca2ac81afb7c82c670bf5fd971c48e98f7043ee7f76bca4a38f756d0e9.

Source-2 runtime differs only by the third-after-real-error control: dafcdbde310260492fd0bc9637e863df736fd51e79586560cab19ccf7ef55a5c. Its production prefix equals source-1. The proposal applies to that production code too, without deleting that stronger control.

Actual ownership boundaries

| Path | Existing boundary and ownership | Necessary correction |
| --- | --- | --- |
| Initial static-ELF Tool owner | runtime.rs:2879 owns executor and global state locally; call at :2942 awaits run_static_elf_process_with_tool. There is no catch around that await. | Put an owner-local catch around the actual owned run future while executor and backend remain outside it. After panic publication, finish retained consumers and terminal resource/worker/child ownership before resuming the same payload. This is an awaited cleanup phase in the existing caller, not a detached task. |
| Fork Tool process | vm.rs:2491 spawn_owned returns child state on spawn failure. On success, closure :2502 owns child backend/executor outside the run call, executes the cancellation/run branches at :2518, then child-wait publication at :2544. There is no catch around the fork closure's Tool work. | Catch the child operation while the child backend/executor still live outside the caught closure. Publish before cleanup/joins, complete retained consumers, clean physical descendants, publish ChildCompletion::Failed and completion notification, retain typed cleanup causes, then resume the original payload. Include child-wait hook failure/panic in this owner boundary. |
| Tool CLONE_THREAD | vm.rs:2785 already catches the whole block_on operation. child and child_executor survive outside its closure. Err(payload) calls finish_panicked_guest_worker at :2847. | Extend this existing boundary rather than nesting block_on inside an already-entered executor. The catch has returned and the old block_on invocation has unwound, so this synchronous worker may drive one awaited recovery operation with block_on before resuming the original payload. Keep parent callback ThreadState unwound; recover only separately retained, untouched child consumers. |
| Host-owned CLONE_THREAD under a Tool run | vm.rs:2138 catches direct run_static_elf_process. It also calls finish_panicked_guest_worker. Its direct backend loop does not invoke Tool callbacks and cannot populate a Tool failed-spawn queue. Constructors start empty. | Keep publication/retirement behavior. Shared panic helper may safely drain an empty queue. Do not fabricate Tool hooks or ThreadState for Host workers. |
| Bare run_with_tool transport | runtime.rs run_with_tool does not construct this static-ELF executor/failed-spawn ownership chain. | Do not broaden this correction into a different runner. |
| A successfully spawned child whose start gate is cancelled | vm.rs:2519/:2796 call finish_unstarted_tool, not the ordinary run loop. Its constructed executor starts with an empty retained queue; consuming hooks can still panic. | Include these calls in the same fork/Tool-worker owner catch. Failed-spawn children are handled by the per-consumer catch below. |
| Public future dropped by its caller | Public async methods borrow KvmBackend; their executor and in-progress cleanup future can be dropped at an await. There is no existing asynchronous destructor or supervisor. | Explicitly separate this from caught panic. This proposal does not promise asynchronous consuming hooks after arbitrary external cancellation. Do not spawn a background cleanup supervisor, block in Drop, claim that dropping an unpolled future consumed its hooks, or manufacture a guest success status. |

Minimal API shape

Keep the existing failed-spawn producer arms and the original typed Error::HostIo return. Keep executor-local storage and independent constructor queues. No process-wide queue registry is needed if each root/fork/thread owner catches and completes its own retained consumers before propagating its panic.

Add these private/internal types and functions, with names adjusted only if the owner has already introduced equivalents:

```rust
type PanicPayload = Box<dyn std::any::Any + Send + 'static>;

struct CaughtFuture<R> {
    output: Option<R>,
    // poll panic first; a separate destruction panic second, if any.
    panics: Vec<PanicPayload>,
}

async fn catch_owned_future<F: Future>(future: F) -> CaughtFuture<F::Output>;

struct OwnerPanic {
    payload: PanicPayload,          // the original Box, never downcast/reboxed
    published: Error,              // exact published typed cause / signal ledger
    additional: Vec<Error>,        // every real error from later cleanup
    secondary_payloads: Vec<PanicPayload>,
}

struct CleanupCompletion {
    errors: Vec<Error>,
    panic: Option<OwnerPanic>,
}

async fn drain_unstarted_tool_cleanups(
    executor: &mut ElfExecutor,
    failure: Option<&FailureContext>,
    panic: Option<OwnerPanic>,
) -> CleanupCompletion;
```

A real production fatal path always has FailureContext installed by the static-ELF owner. The Option preserves existing direct/internal controls; no new success or fallback admission is inferred from None.

catch_owned_future must own a pinned allocation outside its caught poll closure. Catching an entire vector drain, or only applying FutureExt::catch_unwind to a borrowed future, is insufficient. The following algorithm is the exact required behavior; use it as a small reusable primitive rather than duplicating it in each owner:

1. Store Some(Box::pin(future)) in a local variable owned by the helper.
2. poll_fn invokes catch_unwind(AssertUnwindSafe(|| owned.as_mut().unwrap().as_mut().poll(cx))). Pending retains the same allocation. Ready moves the output into CaughtFuture.output. A poll panic moves its original payload into CaughtFuture.panics and never polls that future again.
3. After the poll operation has returned or panicked, take the owned allocation and explicitly drop it inside a separate catch_unwind. Append a destruction panic without replacing the original payload or any already-returned Result::Err output.
4. Return only after that allocation has been destroyed. No callback receipt or next consumer can run before this point.

This catches an actual callback/cleanup destructor panic on an otherwise normal drop. Rust abort on a second panic during an already-unwinding destructor is not a recoverable catch_unwind outcome; do not claim otherwise. A future that already panicked must never be re-polled.

Drain algorithm

- Take the whole local queue once, outside every registry guard. Its iterator and result accumulation remain outside each individual catch_owned_future invocation. Each stored future crosses the caught poll/destruction boundary separately. Do not put the iterator inside the operation being caught.
- Retain every output error except the exact bare Error::RunAborted marker. In particular, preserve WithCleanup/SharedFailure/SignalEffects/WorkerFailure wrappers even when primary() is RunAborted. If a future returned Err before its destructor panicked, preserve both that Err and the panic.
- If a parent OwnerPanic already exists, never replace its payload. Publish each newly caught cleanup panic as a typed cleanup cause before moving to another consumer or a physical join. Attach a phase using the existing Error::cleanup facility; no stringification of an existing Error tree.
- Without a parent panic, the first cleanup panic becomes the payload to propagate after all retained consumers complete. Every later panic remains a separate typed error; retain its payload until safe final owner handling rather than allowing payload disposal to overwrite the selected original panic.
- A pending consuming hook keeps the drain Pending, with that future and all later futures still owned. No retry or spawning a new hook invocation. An earlier returned error or caught panic never prevents a later consumer from being polled.
- Return CleanupCompletion; do not resume_unwind inside the drain. That would bypass later consumers or lose its accumulated real errors.

Owner recovery and typed error retention

Factor the terminal resource ownership currently in vm.rs:3979-3999 into an owner-local recovery operation that can await the drain. Split the existing finish_caught_worker_panic publication from its final resume so publication occurs before the first cleanup poll, not after the drain.

For an already-caught callback panic:

1. Callback future has been destroyed by catch_owned_future; executor and backend remain live outside it.
2. Capture the original signal/effect ledger with executor.with_signal_effects(Error::GuestWorkerPanic, None) and publish through this exact FailureContext. Keep the returned shared typed error as OwnerPanic.published. Do not rewrap the original parent HostIo or change RunFailure's first-cause choice.
3. Await all retained consumers with that OwnerPanic. Consumer panics are absorbed into the owned report, not resumed early.
4. Retire the actual parent generation, release slot/TID/file/stdin resources, and perform the existing worker/process ownership protocol. A Tool-thread owner transfers its independent process handles to its process leader as today; a process leader/root cancels and joins its workers and finishes its process handles after terminal publication. Do not transfer consuming futures into a registry that a parent has already drained. No callback or queue/registry guard spans the await/join.
5. Preserve all errors from those operations in the same typed aggregate, then store it at the correct surviving owner and resume the exact original payload.

The worker path can retain its aggregate in the existing reported_worker_panics/worker_errors protocol: preserve the matched panic error at the physical join rather than publishing a fresh duplicate GuestWorkerPanic. Existing finish_caught_worker_panic tests must continue checking publication-before-retirement and original payload propagation.

Root/fork errors need a surviving typed owner too. RunFailure currently retains only its first published cause; repeatedly calling publish does NOT retain every later error. Do not mistake publication for a cleanup-error ledger. The minimal shared place already present in every Tool run is RunFailure, not a new supervisor. Add a narrowly scoped retained panic-cleanup record and fold it once into complete after owned joins:

```rust
impl RunFailure {
    // Consuming insert, used only where resuming panic would otherwise drop
    // the typed diagnostics. Do not insert the same aggregate in both this
    // ledger and worker_errors.
    fn retain_panic_cleanup(
        &self,
        error: Error,
        secondary_payloads: Vec<PanicPayload>,
    );
}
```

Use a mutex-owned record to retain Send panic payloads; never hold that mutex while executing their destructors or any hook. The primary payload is NOT placed in this ledger: it remains the original Box resumed by the owner. Root panic leaves the backend's FailureContext retaining this record while the caller receives its original payload; provide a narrow diagnostic read/take path on that existing backend/failure owner if caller inspection is required. A fork's parent eventually calls RunFailure::complete, which must attach these real cleanup causes once, without replacing its original typed cause or treating a panic as guest success. Worker aggregates that are already guaranteed to return through worker_errors must not be duplicated in the new ledger. If implementing the ledger is outside the next increment, explicitly limit that increment to the already-caught Tool worker; do not claim all root/fork ownership is solved.

For a cleanup-future panic during otherwise ordinary finish_tool_process, do not discard still-owned parent Tool/ThreadState. Keep the CleanupCompletion.panic alongside the failure result while normal consuming parent cleanup proceeds; preserve both the returned errors and the pending panic. At the final owner boundary record diagnostics and resume the selected payload. If a later parent hook itself panics, the previously selected payload still wins and that later panic becomes an additional cause. This is why a structured owner completion (Result plus optional OwnerPanic) must flow to the outer run boundary instead of resuming inside finish_tool_process.

Suggested internal completion API for this plumbing:

```rust
struct ToolOwnerCompletion<R> {
    result: Result<R>,
    panic: Option<OwnerPanic>,
}
```

Keep the public API unchanged. Change the private finish_tool_process/run_static_elf_process_with_tool completion plumbing and the root/fork/Tool-thread callers together; do not silently convert panic into a Result success or lose panic state through map(|_| ()). Failed-spawn stored consumers may keep their current Result<()> output and have their panics captured by catch_owned_future, provided no internal cleanup helper resumes before its own remaining consumers and diagnostics are transferred to that outcome.

Internal direct-call obligations

The production direct callers visible in this frozen source set are runtime.rs:939 (fault injection), :956 (captured-boundary injection), and :4181 (outer pending action). Their ordinary error path ultimately reaches finish_tool_process. They are not independently complete owners. Keep the retained consumer on their exact executor and document that returning HostIo may leave this queue nonempty until outer completion.

Direct vm.rs controls at :5920, :6444, :6854, :7260, and :7780 call the internal action APIs without the full outer runtime owner. If an actual host spawn fails, these calls can return with the queue nonempty. Their current normal fixtures do not deliberately force that path. Add an explicit test helper that awaits the same production retained-consumer drain after publication and callback destruction, and use it in any forced-spawn-failure direct-call control. Do not claim an internal Result return alone completes Tool ownership. No additional production bypass was found in the provided snapshot; it omits other modules and is not a repository-wide proof.

Earlier synchronous joins are a separate required follow-up

vm.rs:2378-2380 still publishes and physically joins prior successfully spawned pending children from inside complete_injection. A callback can inject a returning clone and then hit failed spawn without suspending. This must be deferred to the same outer owner if the intended guarantee is that no consuming hook/join runs while the parent callback is borrowed. The panic correction does not fix that ordering by itself. Do not work around it by skipping the second clone, weakening supported injection, or merely broadening the tests' success label.

Targeted controls required before implementation approval

1. Actual parent destructor panic plus a pending consuming hook. Use the production drive_handler and the production owner-recovery helper. Create a callback-local Drop guard that calls panic_any with a unique owned marker; publish the synthetic original HostIo(EAGAIN), then force handler cancellation so actual guard destruction panics. The executor already owns at least two retained child consumers. The first invokes an actual Tool on_exit_thread hook that waits on a controlled oneshot; its state destructor must not run while pending. A following consumer returns a typed real error. Poll the recovery to Pending, assert publication already completed and callback was destroyed, and assert both child states remain owned as appropriate. Release the hook; require exactly-once hook/state consumption, all later consumers attempted, exact typed error Arc retained, queue empty, and the same original panic payload at the final catch. This must fail on source-1's panic bypass. It is a lifecycle control, not an actual failed-spawn producer test.
2. Cleanup future panic followed by more work. Retain, in order: a future returning a typed real error; a future whose poll panics; a future returning a typed real error whose destructor panics; a real pending consuming hook; a final successful/marker consumer. Require all later consumers to run once, both returned real errors to survive, both panic events to be retained, and the selected original parent payload to survive unchanged. Also run without a pre-existing parent panic, where the first cleanup panic payload is propagated. No consumer is re-polled after panic/Ready.
3. Boundary matrix. Exercise the factored owner helper in leader/fork/Tool-worker dispositions. Tool worker retains matched panic diagnostic until join and transfers physical process handles; fork publishes Failed completion; root retains secondary diagnostics while propagating its payload. Host-only worker has an empty Tool queue and never fabricates hooks. Assert no physical join occurs before publication. Use finite controls with explicit releases, not a watchdog result counted as success.
4. Actual failed-spawn producer control remains separate. Add an injectable spawn failure at the spawn_owned boundary for tests (same state-return contract, exact EAGAIN), then cover both fork and CLONE_THREAD arms and a successful-then-failed injection sequence. The existing three-future queue control does not replace this test. Do not use global process limits or a flaky exhaustion loop as the fault mechanism.
5. Preserve source-2's third-after-real-error test and every old assertion. No tolerance, comparator, skip, failure label or supported-case exemption changes are part of this proposal.

Unexecuted work and limits

No compiler, formatter, tests, payload, KVM, Hermit, model, network or SCM invocation. Only frozen-source reading/hash parsing and this proposal artifact were performed. An independent audit agent could not be started because the agent thread limit was reached. No product source or skill file was edited. This proposal must be implemented and reviewed as exact successor source before it is treated as a fix. No full scheduler, Linux/POSIX, determinism or parity approval is implied.
