Implemented the private ownership primitive in reverie-kvm/src/entry/owner.rs only. Base is Reverie 91110d249ffd8957267d71fab8c83d9636105efe on codex/kvm-parity-land-20260918. No module declaration or other product source was edited by this author; root is composing entry/memory integration separately. This is source preparation, not independent approval or executed evidence.

DriverScope uniquely owns one registration lifetime. DriverOwner is cloneable and contains no GlobalState or new public generic bound. Each OperationOrigin holds a Weak driver reference and a separate Arc retirement witness; the witness contains lifecycle only, never a pending cause. This avoids a strong ownership cycle when a PendingFailure retains its origin and the driver retains that pending failure.

The driver mutex serializes capture, checked u64 callback identities, active scopes and retirement. Capture inserts the exact Arc<PendingFailure> before returning a separate OwnerNotification. Duplicate captures of the same Arc already pending are coalesced; distinct Arcs retain their order. take_pending transfers typed Arcs to the existing owner, without publication. subscribe must precede the caller's pending/lifecycle recheck. Readiness, including channel disconnection, means only that a recheck is required.

OwnerNotification contains the replaced sender and receiver, so even the old shared receiver is dropped outside the owner lock. Move the notification out of the entry gate lock before notifying OR dropping it: dropping a oneshot sender also wakes its receiver. Root's EntryGate composition owns that sequencing. Capture calls no Tool and does not inspect or reserve RunFailure.primary.

begin_callback(None) creates a unique lexical CallbackScope and immutable generation. begin_callback(Some(origin)) requires that origin and all enclosing live generations belong to this driver. Foreign, missing, destroyed or closed-registration parents, and exhausted callback identity, return Error::EntryControl. IDs never wrap. Nested receipts retain their enclosing generation dependency; ending an inner scope cannot acknowledge the outer scope. Retained origins continue to identify their original generation after a later callback begins. callback_id distinguishes an operation without a callback receipt; its number is meaningful only with the driver identity.

The caller must keep CallbackScope outside its actual owned future and drop that future, including inline/nested futures, before dropping the scope. The primitive deliberately does not pretend to own or destroy an arbitrary callback future. Scope Drop records its generation's destruction, removes its active registration and wakes outside the owner lock, including during unwind. It does not publish a failure or settle executor effects. wait_callback_drop awaits the exact generation and its enclosing dependencies asynchronously, checks the destruction bits after wake, and refuses disconnected notification without destruction.

DriverScope::retire consumes the unique scope, atomically closes capture registration, and returns DriverRetirement { lifecycle, pending, notification, result }. With no active callback it marks Retired and returns Ok; that certifies only registration closure. With an active callback it marks Abandoned, returns a typed protocol error and every pending Arc, and leaves the callback receipt undestroyed. A racing capture either precedes closure and is returned in retirement.pending, or is rejected with the original Arc. CaptureRejected also records the observed lifecycle and whether an owner could still be upgraded. No missing Weak owner is interpreted as successful cleanup.

Dropping DriverScope without retire marks Abandoned and wakes after releasing its mutex. A surviving DriverOwner can still take pending causes. If no owner survives, retained origins still observe the abandonment witness; they cannot call an obsolete publisher through this API. Neither retirement case permits delegated publication. Root retains the existing origin-owned executor/effect/publication route, and must preserve the separately reviewed nested failed-spawn cleanup ownership before activating this primitive in runtime callbacks.

Actual private API:

- DriverScope::{new, owner, retire}
- DriverOwner::{lifecycle, subscribe, pending, take_pending, origin, begin_callback}
- CallbackScope::origin; its unique Drop supplies the destruction acknowledgement
- OperationOrigin::{lifecycle, callback_id, same_driver, same_callback, callback_dropped, wait_callback_drop, capture}
- OwnerNotification::notify
- DriverRetirement fields lifecycle, pending, notification, result
- CaptureRejected fields failure, lifecycle, owner_available

Seven unexecuted test declarations cover:

1. Pending Arc retention before deferred notification, duplicate capture, and synchronous wake checks that both actual gate and owner mutexes are unlocked.
2. Inner/outer destruction dependency and immutable retained identity across a subsequent callback.
3. A real owned future retaining an RAII probe across Pending, with destruction observed before the caller ends its CallbackScope; acknowledgement is not inferred from a poll.
4. Both deterministic capture/retirement orderings and 32 finite concurrent capture/retirement races, requiring every Arc be returned on exactly the appropriate side.
5. Rejected retirement with a live callback, preserved pending cause, explicit abandonment, and no false callback acknowledgement.
6. Callback/driver unwind, retained pending cause and private wake, without Tool publication.
7. Weak origin ownership, unavailable-owner rejection, typed foreign/missing/stale-generation errors, and callback ID exhaustion without wrap.

Tests construct an opaque typed PendingFailure through the existing EntryGate::new().poison(None, Error::EntryControl { ... }); they require no assumed PendingFailure fields and make no fake Tool publication claim. The new declarations add assertions; none replace existing tests, widen tolerances, skip cases, alter comparators or relabel failure as success. No existing check was edited.

Validation status: only source inspection, static hashing, declaration inventory and read-only base/branch checks. No formatting tool, compiler, test, guest, network, external model, commit or other SCM mutation was run. The source is not self-approved, and none of these seven declarations is reported as passed. Root must compose the module/API, compile and execute the controls, then obtain independent review and full integration evidence separately. TARGET.json seals this source and its frozen copy; later root changes are successors, not this preparation's tested result.
