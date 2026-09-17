# Prepared common capture

`unsafe { prepare_capture_unstarted(options) }` returns a caller-owned,
noncloneable `CaptureOwner`, one `LogSink`, and a cloneable `HostProducer`.
Preparation accepts no destination and creates no worker threads. A host prefix
can commit real records before `owner.start_workers(destination)` starts the
shared-ring collector and the sole destination worker. Call the latter after any
root task-ID allocation that must precede those workers. The compatibility
wrapper `unsafe { prepared_capture(options, destination) }` performs both stages.
Keep the owner outside Tokio, through GlobalState cleanup/Drop and runtime
destruction.

The lifecycle is Prepared, Starting, Running, Finalizing, then Terminal. Starting
is one-shot, including failed attempts; duplicate starts return the supplied
destination. Running requires actual readiness of both workers and the serialized
Offered-to-Taken destination handoff. A worker's NeverCreated, Created, Ready,
Ended and Joined states report different facts: a missing join handle does not
establish collector completion. Terminal means finalization settled its report,
not that an arbitrary external operation has stopped.

`CaptureDestination` requires Send, not Sync. Neither worker spawn closure owns
the destination. Before Take, an explicit startup failure recovers the original
object in `CaptureStartError`; `take_destination()` takes it once. The error is
Send + Sync and supports propagation through `Box<dyn Error + Send + Sync>`.
Recovery uses exclusive access, without calling destination methods or Drop
under an internal lock. This recovery covers explicit startup errors, including
worker failures; it does not promise general caller-unwind recovery or reuse of
state poisoned by an earlier caught panic. After Take, only the original destination worker owns
and destroys the object; failure can return while that worker is still blocked.
Dropping a recovered object or an error that still contains it is a caller-owned
operation and can itself block. Public report fields and private error fields
have changed; this API makes no universal source-compatibility promise for
external struct literals or exhaustive matches.

This library port does not connect the prepared sink to a current LiteInst
backend adapter or a runtime bootstrap. V3 retained capture and V4 ordered
capture remain distinct socket-lifetime protocols. A prepared sink cannot be
attached to another collector or transferred twice. No default backend capability
is added, and closing a future setup socket must not satisfy lifetime closure.

The host formatter must buffer one complete original event and call
`HostProducer::write_record`; the guest formatter uses
the ordered `Writer::write_record`. V4 rejects arbitrary `io::Write` fragments.
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

Before Running, host record emission does not wait for a writer lock, ring space
or record/byte credit: exhaustion fails the capture. Consumers must serialize
prestart host emission and provision enough capacity for the complete prefix,
including BEGIN, DATA and END frames. Concurrent prestart emitters can otherwise
fail according to host timing; source ordering does not remove that requirement.
Successful prefix records retain their original END/order commits and drain after
startup; preparation acknowledges zero destination bytes. A partial BEGIN or a
missing END remains an incomplete stream, never a fabricated successful record.

While Running, ordinary guest cancellation/init/spawn/RPC failure closes only guest admission.
`guest_finished()` is a guest transport/run report, not a whole-capture verdict.
Root reap, endpoint closure, registration/FINISH, RPC outcomes and capture
publication remain separate facts. Host cleanup can still emit after that cancellation.
Cancellation before Running instead closes the whole prestart capability.
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
Finishing without startup settles report waiters as Incomplete without claiming
collector work, guest FINISH, EOF or root reap. If collector creation fails or a
delayed worker misses startup, the original handle retains the unique cancelled
collector reservation and its unread stream diagnostics; a delayed worker cannot
take it later, and no second collector can attach. Only library-owned socket
endpoints are closed; caller endpoints and their aliases remain caller-owned.
Finalization bounds its wait for already active host calls by the original
deadline. An active call or admission at cutoff is reported, not presumed absent.
The returned report is a bounded observation, not a promise to contain every
future source commit. Later `capture_snapshot()` calls can refresh retained
source diagnostics and activity counters, including after owner Drop; the
original report and failed publication outcome remain unchanged.
Finalization can settle an incomplete guest once its lifetime endpoint is closed,
the rings are drained, all committed source orders are observed, and no record
remains pending publication. Missing FINISH still means Incomplete, not Complete;
it does not require waiting for a cancellation cutoff after these facts hold.
An open lifetime endpoint, unresolved order or pending record retains the existing
cutoff/deadline rules, and a deadline failure remains sticky.
Late host writes reject and remain observable. Owner Drop requests bounded closure;
it does not synchronously join. Arbitrary destination progress, Write, flush or
Drop cannot be interrupted. The external-operation clock starts before the first
progress callback, separately from write attempts and acknowledged bytes.
Deadlines revoke queued output, return `MayAppend` while the worker is unsettled,
and retain any uncertain attempted range/acknowledged cursor. No second writer retries that range. Late
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
blocked finalization and fresh native subprocess lifetimes. The two-stage API
control proves qualification with a real spawned guest, FINISH, EOF and reap.
Its formatted-byte transport inputs do not rerun Hermit's original tracing
formatter/RPC fixture and do not qualify a SaBre runtime. Native backend
qualification, H shared-subscriber dispatch, exact existing H clipping adapter,
actual Detcore CLI execution and concurrent-emitter parity remain separate work.

Mapping creation/import, collector attachment and producer activation carry
explicit unsafe contracts. All peers, descriptor duplicates, inherited state and
writable mappings must cooperate for the full lifetime of every worker/producer.
Initialized layout fields remain immutable; frame and credit access obeys the
exclusive-incarnation/single-collector publication protocol. Size seals prevent
resizing, not arbitrary writes. The unsafe API does not establish a memory
sandbox, descriptor isolation, process ownership or an F1/F3 runtime repair.
