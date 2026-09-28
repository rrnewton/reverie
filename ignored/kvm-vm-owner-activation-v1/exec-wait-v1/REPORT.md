# Interruptible Exec worker wait: author handoff

This narrow successor implements the injected/action Exec join dependency identified in the parent packet. It is not independent review or execution evidence. Only vm.rs, new vm/exec_wait_tests.rs, and this new artifact directory were written; the preceding frozen activation packet remains unchanged. No compiler, formatter, test, guest, network, model, or SCM operation ran.

The source was released to root for composed formatting/build after the exact snapshots below.

- Baseline vm.rs: `42b3df8175f728659dcfc2378646e4bdfb078729fbac8aa4dbda30d44bed5536`.
- Author vm.rs: 410522 bytes, `bf042a9c05113b38ad9c2bd939548424234aaecefda2855baa8d765fb1b3fcc0`.
- New vm/exec_wait_tests.rs: 6562 bytes, `0c1d8de95a14276fb9ac9e7a93a16f23deb6d95df0d3f3bdb59051e45994bd41`.
- SOURCE.patch: `0b30a06f8541f91c1f80fb550bda000b0807252fa723f41a70f47f096abd02d3`.
- TARGET.json: `cd9604dbd4c8b5cee423c1ee4d712a2bdf9890392f2f8a67c6b89d59b825abb4`.

## Implemented path

The Exec action requests the existing group cancellation, then awaits `cancel_guest_threads_for_exec` rather than synchronously joining live workers inside a callback. The group owns every unfinished handle throughout that wait. A private entry failure or the existing stop future can return its typed error before the callback has been destroyed; the caller's existing outer route/publication remains responsible for releasing dependent peers.

`WorkerCompletionNotice` supplies a real completion generation and a returning flag for both production Host and Tool thread wrappers. An outer guard encloses an inner move closure containing the actual worker state. Return and unwind therefore destroy the inner closure's child backend, executor, and other owned state before dropping the completion notice. Notice delivery happens outside registry locks. A failed host spawn can produce a harmless change notification without a registered handle; every consumer still rechecks actual state.

The async wait subscribes to group completion before registry rechecks and retains that subscription across Pending. It checks EntryDriverWatch and the existing stop before and after each finished-handle collection; the private watch registers its own owner/gate wake. A real notice requests a recheck, not success. If notification precedes JoinHandle::is_finished during final host bookkeeping, the future requests one cooperative recheck per poll and rechecks owner/stop each time. No timer or background joiner was added.

`join_finished_workers_for_exec` extracts only handles whose JoinHandle::is_finished is already true, under the registry lock. It releases that lock before physical join. Other handles stay registered if the callback is interrupted. Normal blocking joins and finished-only joins use one `join_worker` result collector, preserving typed worker errors, panic records, start-gate removal, and the existing error cache. Raw-handle registration entry points remain test-only for old fixtures; these can be polled cooperatively and are not accepted as completed without is_finished. Every production worker registers its real completion flag.

Exec's existing cancellation, worker teardown error, image installation, and successful rearm sequence remains. Interruption returns an error and leaves cancellation/owned handles for terminal outer cleanup; it does not install a guest result or pretend Exec succeeded.

## Physical join ownership scope

Empty registry alone is not a general proof that another thread has no in-flight join batch. This implementation relies on the concrete production Exec owner rather than extending group supervision: the Exec action and exec_process reject guest-thread execution; guest-thread backends' cancellation/join methods do not physically join. The single non-thread backend's mutable invocation owns the actual Exec path, its normal completion, and its eventual Drop. Separate fork backends have separate groups. Those production paths cannot concurrently move the same group's batches out during this leader's Exec wait. Existing private controls that intentionally run concurrent join_workers calls are not a new production join owner.

This source argument is specific to current call sites. Introducing another concurrent physical join owner into active leader Exec would require tracking its in-flight ownership, not treating the registry as sufficient.

## One added control, not run

`vm::exec_wait_tests::exec_wait_private_failure_leaves_worker_owned_until_callback_destruction` uses the actual Exec wait and notification-aware group registration with a real gated host worker and a constructed owned callback. It first polls Pending, checks cancellation was delivered while the unfinished handle remains registered, then captures a private typed cause. It requires a wake and a typed interrupted outcome while the callback guard is still live, publication is absent, and the handle/error cache have not been consumed. Only after actual callback destruction and outer route/publication does the test release the worker, observe its real completion notice, and physically join it. The exact cause survives final driver retirement.

The worker's receives are bounded to five seconds, including the release that makes a regressed blocking join finite. An unwind rescue releases and joins the owned worker after a bounded departure wait. The control requires /dev/kvm setup with no skip, but performs no KVM_RUN and claims no guest execution or failed-spawn injection.

All 75 old vm test declarations remain in the byte-identical old test module, SHA256 `a8c98f5b564d8c4a47e13752a42dd0d847fd9bb66502ce6957ad211dbb5cc918`. No old assertion, tolerance, comparator, exemption, skip, result label, gate, or denominator was relaxed or removed. Earlier new owner controls were not changed. This packet supplies no test-pass, whole-scheduler, determinism, Linux/POSIX parity, or landing claim.
