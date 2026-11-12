# Proposed interface boundary

Source-only author sketch against Reverie000c. Names below are proposed, not existing callable APIs or approved implementation. The report defines lifecycle, locking and remaining recipient/continuation requirements.

```rust
// Non-RPC setup, called exactly once before any guest instruction/start hook.
// None means this backend cannot provide the capability.
fn GlobalTool::install_backend_signal_control(
    &self,
    control: Option<BackendSignalControl>,
) -> Result<BackendSignalControlMode, Error>;

struct BackendSignalControl {
    publication: Arc<dyn ProcessSignalControl>,
    selection: Arc<dyn SignalSelectionControl>,
}

enum BackendSignalControlMode {
    Unchanged,      // default; no scheduler-controlled selection is enabled
    ToolControlled,
}

trait ProcessSignalControl: Send + Sync {
    fn publish_alarm(
        &self,
        target: SignalProcessId,
        event: SignalEvent,
    ) -> ProcessPublication;

    fn publish_child_exit(
        &self,
        parent: SignalProcessId,
        completion: CommittedChildExit,
    ) -> ProcessPublication;
}

enum ProcessPublication {
    Rejected(PublicationRejection),       // no effect occurred
    Committed(ProcessPublicationReceipt),
    FailedAfterCommit {
        receipt: ProcessPublicationReceipt,
        error: PublicationFailure,
    },
}
```

The default hook ignores the controls and returns Unchanged. An interested Tool rejects unavailable setup, retains the controls and returns ToolControlled. KVM activates the corresponding removal/phase policy only after successful installation, before startup releases. One installation keeps publication and selection from becoming half-enabled between separate hooks. `SignalSelectionControl` is an explicitly new companion defined by the paired scheduler design, not an existing facility or a method hidden inside the publication trait. That policy must be implemented as part of the composed design; the endpoint alone is not an excuse to activate an incomplete mode. Existing Tools/backends retain their current behavior. A new explicit mode is necessary: merely enabling existing dequeue observation does not say that a Tool owns recipient selection. Installation failure uses the existing retained-G terminal cleanup path. No serialized Request/Response format gains an Arc.

`publish_alarm` validates exact SIGALRM/SI_KERNEL info and process target, without a callback nonce or publisher thread mask. The scheduler already owns the timer arm and determines the causal expiry; the endpoint does not read wall time or replace timer version checks. `publish_child_exit` validates full parent/child generations and constructs the siginfo from an immutable committed completion. Its fields must include child identity, actual cause/status, guest UID, guest user/system times and the committed parent relation. These fields need authoritative producers; the existing numeric child event is insufficient. Normal exited versus killed/core causes must remain typed and correctly encoded. No arbitrary SignalEvent parameter lets the child route bypass that provenance boundary.

The process receipt contains full target identity, internal binding revision, signal/pending generation, publication result (queued, coalesced, or explicitly suppressed), and actual disposition at publication. It contains no `blocked` scalar. Suppressed SIGCHLD does not claim a queued pending entry. The result records publication only, not recipient eligibility, delivery, hook completion, periodic rearm or wait readiness. Accepted coalescence retains the existing first siginfo. A snapshot/recipient selection needs a separate current member/mask/phase view.

Before-effect rejections distinguish stale/absent process, closed run, invalid event/provenance and a conflicting or stale binding. Internal broken carrier/state invariants must not be exposed as ordinary guest errno success. FailedAfterCommit retains the complete receipt and first typed terminal cause. The caller must not repeat it as an uncommitted send or clear the pending state. These synchronous calls have no await cancellation boundary: the scheduler must validate the intent, call publication, record its receipt and consume that exact causal intent under one lock section, with no await or unlock between them. A genuinely before-effect stale lookup can be resolved again; a post-effect failure or panic is terminal, never replayed as a fresh send. If retries across an await/unlock are needed, the API needs a generation-bound publication ID plus retained result before implementation; standard-signal coalescing is not sufficient after an intervening dequeue.

Because the caller holds the scheduler lock, the endpoint must not recursively invoke `GlobalTool::report_backend_failure` while returning an outcome. It can poison the backend process/run control synchronously to prevent further effects, retain the first typed cause and return FailedAfterCommit. The caller performs its terminal transition using the lock-held scheduler helper, then wakes failure subscribers after releasing the lock. Cleanup errors remain secondary. A receipt is not authorization for an ordinary grant.

The control object owns only the run registry; registry process bindings are weak and generation checked. The registry holds a stable per-process transaction guard and an exec binding revision, while executors/driver owners hold the actual live state. Lookup upgrades then revalidates after acquiring the authoritative file table and transaction guard. Process exit closes the entry even if a Tool retained the endpoint; run completion closes all entries before recovering G. No numeric descriptor/PID reopening and no strong GlobalState cycle.

The **required companion** to ToolControlled is a snapshot and single-use shared-removal authorization at actual backend owner phases. Its concrete public shape remains for the paired scheduler/backend review, not silently supplied by the two publication methods. It must bind full task/process generations, current image and phase revision, signal/pending generation and actual consumer; preserve private-first removal and all three consumer domains; invalidate on mask/disposition/exec/exit/phase changes; retain real journal/ack effects; and release scheduler fences before Tool hooks. Publication can be independently implemented/tested as a prerequisite, but the complete H1/H2 correction cannot be declared done until that companion and wait service exist.
