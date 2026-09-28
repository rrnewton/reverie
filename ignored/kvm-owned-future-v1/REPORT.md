Private owned-future catcher, source preparation only

Implemented the agreed private API in the new reverie-kvm/src/failure/owned_future.rs. This author changed no existing product file. Root owns the failure.rs module declaration, runtime/vm/error integration, and subsequent compilation and execution. The dispatched base is 91110d249ffd8957267d71fab8c83d9636105efe on codex/kvm-parity-land-20260918; no SCM command was run to make a new source identity claim.

Exact new source: SHA256 043a38bfb7af1138ec66962f892e904c69320dab9c0c672b75ed7f64898b202d, 10,917 bytes. The adjacent source copy and SOURCE.patch preserve the submitted bytes. The existing failed-spawn panic-successor proposal was read and is copied into this packet as PROPOSAL.md. This report describes an implementation, not an independent approval or a passing test result.

The API is PanicPayload = Box<dyn Any + Send + 'static>, CaughtFuture<R> with output: Option<R> and panics: Vec<PanicPayload>, and async catch_owned_future<F: Future>(future: F) -> CaughtFuture<F::Output>. All are pub(crate). The helper imposes no additional Send, Sync, Unpin, UnwindSafe, or lifetime bound on F.

At owned_future.rs:38 the owned Pin<Box<F>> is stored outside the closures that catch polling. Pending returns with that allocation retained; Ready moves the output out, and a poll panic moves its original Box into the ordered payload vector. Either terminal outcome stops polling. At :56 the helper takes the same allocation, then explicitly destroys it inside a separate catch after the polling phase has returned. Any destruction panic is appended, leaving a returned typed error or earlier panic payload intact. The helper returns only after this destruction attempt has completed. It performs no publication, callback acknowledgement, scheduler action, hook invocation, or effect settlement.

Four finite host test declarations were added, with no executions:

| Declaration and line | Exact intended check |
| --- | --- |
| pending_then_ready_keeps_allocation_and_drops_once, :180 | A deliberately !Unpin future is Pending until a controlled oneshot is released; address equality is checked in both polls and destruction. Poll counts are exactly one then two, destruction is zero then one, and destroying the completed helper does not destroy its input again. |
| poll_panic_is_not_repolled_and_drop_panic_is_retained_separately, :208 | Poll and Drop each resume a distinct original Box payload. The output is absent, poll and destruction counts are exactly one, and both payload addresses and labels survive in polling-then-destruction order. |
| returned_typed_error_survives_a_destructor_panic, :233 | A ready Err containing Arc<Error::GuestClock> survives Drop panic with identical Arc identity and typed contents; the separate original destruction payload survives too. |
| send_non_sync_state_can_cross_a_pending_await, :262 | A Cell is held across an actual pending oneshot await. A compile-time Send requirement and moving the pending helper to a second thread check that Sync is not required; the second poll must return the expected value without panic. |

The test future uses interior mutability and PhantomPinned; there is no unsafe projection. Preallocated marker payloads use resume_unwind so the checks can compare the original payload allocation rather than compare only panic text. The tests do not stand in for a production Tool callback, retained consumer drain, owner publication order, physical joins, or the actual failed-spawn producer.

No existing assertion, comparator, tolerance, case selection, failure label, or check was edited. No test was skipped, ignored, widened, or removed. No prior test result is relabelled by this source preparation. The previously failing owner keepalive control and all of its source/evidence remain untouched by this task.

Limitations are explicit at owned_future.rs:33-36. Dropping the helper while Pending follows ordinary future destruction; it does not promise this awaited completion protocol or asynchronous cleanup. A destructor that panics during already-unwinding destruction may abort the process and cannot be recovered by catch_unwind. Payload destruction after the caller receives CaughtFuture remains caller-owned. Compiler and integration results remain unknown until root composes the module and runs the admitted checks.

No compiler, formatter, tests, product payload, model, network, or SCM operation was performed. All non-source writes are contained in ignored/kvm-owned-future-v1. The new file is released to root for composition after this packet is sealed.
