# Prepared common capture

`prepared_capture(options, destination)` returns a caller-owned `CaptureOwner`,
one `LogSink`, and a cloneable `HostProducer`. It starts the existing shared-ring
collector and sole destination worker on dedicated host threads, and waits for
both before returning. Prepare it before GlobalTool initialization. Keep the
owner outside Tokio, through GlobalState cleanup/Drop and runtime destruction.

The same two LiteInst logged backend adapters accept the prepared sink. They
transfer its single guest endpoint through sealed V4 bootstrap and return after
guest/RPC completion, not after host publication shutdown. V2/V3 bootstrap and
legacy retained capture remain distinct. A prepared sink cannot be attached to
another collector or transferred twice. No default backend capability is added.

The host formatter must buffer one complete original event and call
`HostProducer::write_record`; the guest formatter uses
`GuestLogWriter::write_record`. V4 rejects arbitrary `io::Write` fragments.
Formatting failure must call `record_failed`, not emit a partial event. Neither an unwinding formatter
Drop nor a detlog RPC forwarder should duplicate the record. H subscriber/filter
and formatter integration is not provided by this crate.

Complete-record credits precede BEGIN. The short END commit assigns a checked
capture-wide order before release-publication and before the emission returns.
The collector emits only contiguous source order, never receive/scan order.
Source order preserves established causal edges but does **not** establish
determinism of concurrently enabled emitters or cross-backend INFO parity.
Host and guest credits are separate reservations in one bounded transport;
consumption/discard recycles them. Unfinished guest reservations remain charged.

Ordinary guest cancellation/init/spawn/RPC failure closes only guest admission.
`guest_finished()` is a guest transport/run report, not a whole-capture verdict.
Root reap, endpoint closure, registration/FINISH, RPC outcomes and capture
publication remain separate facts. Host cleanup can still emit after cancellation.
An unresolved guest commit at cutoff prevents ordered continuation; it is not
cleared or skipped. Host publication/order faults are fatal evidence failures.

`CaptureDestination::progress` reports cumulative **actual** data acknowledgments,
deliberate discards and marker bytes separately, including acknowledgments before
a returned error. Short writes and interrupted writes resume only the observed
unconsumed suffix. The caller owns the one existing output ceiling/marker policy:
transport implements no second clipping algorithm. Output clipping continues
transport consumption without stopping the guest, but cannot qualify a capture.
Fatal transport limits never produce a marker claiming execution was unaffected.

After all enabled host emitters are quiescent, call `finish_until` once (repeated
calls preserve the earlier deadline). Clones do not keep admission open.
Finalization can settle an incomplete guest once its lifetime endpoint is closed,
the rings are drained, all committed source orders are observed, and no record
remains pending publication. Missing FINISH still means Incomplete, not Complete;
it does not require waiting for a cancellation cutoff after these facts hold.
An open lifetime endpoint, unresolved order or pending record retains the existing
cutoff/deadline rules, and a deadline failure remains sticky.
Late host writes reject and remain observable. Owner Drop requests bounded closure;
it does not synchronously join. Blocking Write/flush cannot be interrupted:
deadlines revoke queued output, return `MayAppend`, and retain the uncertain
attempted range/acknowledged cursor. No second writer retries that range. Late
completion cannot promote a frozen failure to `Stable` or success. `Stable`
requires the exclusive destination worker to have actually ended.

Publication registers each accepted mailbox record and its bounded diagnostic
prefix before the receiver can begin it. Revocation accounts all registered,
definitely unattempted bytes immediately, without needing the destination thread
to wake. `unpublished_bytes` and `first_unpublished_order` cover these bytes and
direct collector discards with checked, once-only accounting; the earliest order
is retained even if a later direct discard precedes queued revocation. They do
retain the order of a discarded empty record without inventing payload bytes.
They do
not include deliberate output-ceiling clipping (`progress.discarded_bytes`).
An outstanding/failed `attempt` separately identifies the uncertain suffix after
its observed consumed cursor; that cursor includes data acknowledgments and
deliberate clipping, whose actual totals remain separate in `progress`. A failed
write with a fully consumed cursor has no uncertain bytes left. A successful short
write or resumable Interrupted result returns its unconsumed suffix to the
unattempted reservation until the next invocation begins. Fatal errors never
retry an uncertain suffix. Diagnostic retention/omissions are decided on enqueue
or direct discard, not on delayed receiver execution. Deadline snapshots freeze
all this evidence; late progress cannot change the returned report or its
`MayAppend` status. Source credit remains charged until the owning record is
actually released, independently of publication accounting.

Closing admissions does not turn validated pending records into order holes.
The collector delivers each contiguous next order before reporting completion;
an absent next order after closure/entrant/ring drain still refuses rather than
skipping to a later record.

`capture_snapshot()` returns `Some(CaptureReport)` only for a prepared session.
Its `qualifies()` additionally requires successful guest execution, actual root
reap/lifetime closure, no RPC/transport/order/destination issue, closed/drained
admissions, no late host write, full untruncated publication and stable shutdown.
It is not a deterministic-execution or comparator result. Legacy `snapshot()` /
`finished()` cannot qualify prepared capture: their per-producer byte format is
not the common canonical destination. They become terminal but incomplete at
publication finalization; callers must inspect `capture_snapshot()` instead.

Pure tests cover framing/order/credit, cancellation, generic destination errors,
blocked finalization and both adapters' pre-exec lifetimes. Native V4 guest
qualification, H shared-subscriber dispatch, exact existing H clipping adapter,
actual Detcore CLI execution and concurrent-emitter parity remain separate work.
