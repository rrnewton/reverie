# Reverie timer component: review-response successor

Author implementation report, **not independent approval**. Source is frozen for native and properly grounded Claude review. No commit, push, PR, merge, scheduler implementation, or setitimer/parity success is claimed.

Repository: `/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918`, branch `codex/kvm-setitimer-20260918`, unchanged base/HEAD `f97b7be1de4e2ef10ecc24cee5d8cc47f2fd254f`, base tree `fea1d0744b6eb77bd9d2643f5d9b2af7e3854e3f`.

- Full 14-path `SOURCE.patch`: **09efb96b9fda442f36cb22f772b94db50cf42ecbee125738063752cd57917979**, 159,872 bytes.
- Eight-path delta from preserved frozen-v1: `DELTA.patch` **fe292a6f01ae755b80fc9c2771dec035090aadf909d208622b5f860f8d37eea3**, 64,979 bytes.
- Core API inventory: **4a8460a798af17db988c480d8e0d5f784bd9c860c18b6bddafbb2e90bb3d8557**. `reverie/src/signal_observation.rs`: **83361a50d825b11df24649349f45d1297b7edf0c851f78b9ae04335e65b7132a**.
- Predecessor full patch remains **2376c4f9405d551cb5ffd4edf0314ef8e2dcefde1d2c327c22d34da831a1e804**. Its source snapshots, reports, retained ELFs, failed receipts and cleanup-only recovery were reauthenticated without changes.

`SOURCE_INPUTS.json` binds exact before/after files, 17 closure snapshots, the 2,617-record final product manifest, design inputs, both independent reviews, final qualification, and SCM state. Both patches pass reverse application checks against the actual current source. `READBACK.json` binds this report and all final evidence.

## Review closures and measured controls

| Finding | Final implementation and actual control |
|---|---|
| Native R1: equal local ordinals accepted across lifetimes | Both public handles now carry the complete `CallbackSignalSite` (`signal_observation.rs:120,203`). Executor validation checks process generation/tgid, tid/task generation, current callback and saved boundary before creation, acceptance or consuming a prepared selection (`executor.rs:3346,3449,3500,3531`). The adapter and private driver also check their retained site. Real fork/executor controls preserve the whole ledger on stale/reused-TID refusal; VM modes 4/5 reject all six changed site fields and wrong selection/ledger ordinals, then exercise the valid fatal/cancel action. The native negative-before control actually observed parent and child tokens both equal to `{callback_nonce:1, selection_nonce:1}`. |
| Native R2: replay of a completed observation | `admit_signal_observation` (`executor.rs:3466`) records each admitted lease in the original callback ledger before removal; reused leases are refused without altering pending, sequence, ack, or ledger. Fresh nonmonotonic leases are legal. Actual ignored and suppressed VM cycles reject the previous lease before each next observation, prove pending remains coalescible, then complete all three fresh cycles. The negative-before VM returned a second ignored removal instead of the required refusal. |
| Native R3: posthook changes a committed caught action | The existing injection guard now rejects `rt_sigaction` writes for the reserved signal before execution (`parked_signal_runtime.rs:4`). Queries, writes for another signal, ordinary getpid/RPC, and preselection action changes remain supported. The real frame fixture still requires its handler exactly once, with the original mask/alt-stack/siginfo checks. Independent mode-3 control measured the old write returning `Ok(0)`; final writes of both SIG_IGN and SIG_DFL return ENOSYS. Added mode 14 proves a preselection change to ignored still succeeds. |
| Claude F1: a sibling flushes another task's journal | The process FIFO remains globally sequenced. Each entry now retains the removing task's full admitted identity and per-task wakeup (`signal.rs:373`; `executor.rs:7849`). `signal_dequeue_front` exposes only caller-owned entries. `poll_signal_dequeue` (`executor.rs:3721`) waits until that owner reaches the process FIFO front; registration and predicate share one short process lock. No lock survives the Tool await. Acknowledgment checks exact owner and record, then wakes the next owner. Only the current owner's last acknowledgment is idempotent; wrong-owner and stale earlier acknowledgments refuse. A real two-thread opted-in guest holds T1's notification while T2 injects unrelated getpid and then removes SIGUSR2. The old source announced T1's private SIGUSR1 through T2. Final source emits exactly the two owned contiguous records and returns genuine post-removal EFAULT to each injection. |
| Claude F1: terminal transfer and later owners | `with_signal_effects` (`executor.rs:3574`) marks the process notification stream terminal and wakes waiters; it transfers only the caller's journal entries. It never acknowledges, rolls back, steals a sibling record, or notifies a later sequence across a failed predecessor. The negative VM path fails T1's notification while T2 is already pending after its own real removal: both owner ledgers survive, only T1 was notified, and neither abandoned syscall resumes. A focused unit test also pins the pending/wakeup/ack transition and exact journal contents. |
| Claude F2: capacity becomes an unrelated syscall errno | The raw `execute` implementations no longer fabricate an errno for bookkeeping reservation. `KvmGuest::inject` preflights via a fallible internal method before execution and signals a private RuntimeError with retained effects (`runtime.rs:1177`). The unsubscribed production path has the same fallible preflight (`runtime.rs:3403`). A real VM first acknowledges one removal, then fills the unchanged 4,096-publication receipt limit using accepted/coalesced publications. Old getpid returned `Err(EOVERFLOW)` to the Tool; final execution terminates with the retained one dequeue, ack 1, all 4,096 receipts and no fabricated raw syscall result. |
| Claude F3/F4: creation self-check and nonce documentation | `retain_parked_effects` now checks the full live site even when no ledger exists. Six independent foreign-site controls require no allocation/ledger adoption and unchanged pending snapshot. Documentation describes callback nonces as local to an executor, with lifetime uniqueness supplied by the full site. |

The handles' larger lifetime context made an intermediate Clippy run reject an oversized `Error` variant. The context is now boxed **when the ledger is created**, before publication, and moved into `Error::SignalEffects`; the public error field is `Option<Box<ParkedSignalFailureContext>>`. No Clippy suppression was added to bypass that diagnostic. This backend error representation change accompanies the core handle changes in the inventory.

The Hermit author explicitly confirmed its existing process-global contiguous acknowledgment contract and full `selection.site == retained_site` validation. No new public dequeue field, speculative buffering, sequence reorder, or blanket sole-thread journaling restriction was introduced. Ordinary multithreaded notifications remain enabled. Parked alarm admission retains its separately documented sole-live-leader restriction.

## Final exact-source qualification

Owned `qualification-v6` is the final source binding. Every phase has raw status 0, accepted=true, terminal_authenticated=true and inputs_unchanged=true. All phase inputs were reauthenticated again during freeze. The approved observer/private cache and original resource limits were retained.

| Check | Actual result |
|---|---|
| `cargo test --offline --locked -p reverie-kvm --lib --test static_elf --no-run --message-format=json` | Successful; 20.059 seconds payload, no compiler diagnostics. |
| Corresponding Clippy targets with `-D warnings` | Successful; 4.168 seconds, no diagnostics. |
| Rustfmt check on all 13 changed Rust paths | Successful. |
| Focused domain / alarm / child library groups | **15 + 12 + 9 = 36 passed**, zero failed/ignored. |
| Actual initialized KVM tests | **6 + 1 = 7 passed**, zero failed/ignored; group libtest times 3.064 and 2.407 seconds. |
| Actual ELF inventories | 478 library / 294 static declarations; these are not executed-pass counts. |

The seven VM declarations are the original parked contract (now all modes 0 through 14), caught-action refusal, capacity terminal failure (mode 15), two-thread owner success, two-thread failed-predecessor retention, prior process-alarm boundary contract, and prior child-exit contract. Each retains the official 30-second self-exec bound and `REVERIE_REQUIRE_KVM=1`; missing KVM is a failure. No timer clock rounding, comparator relaxation, or wider timeout was used.

Retained final ELFs:

- Library: `artifacts/lib`, SHA256 **4829ea491225216618c89bdc320dd17b2f7ead44b777e264874f92009c4f0ab2**.
- Static VM test harness: `artifacts/static`, SHA256 **ad8c7aa4599845f4f51b7f0af0e6d3252dfffce1a51abd06bdd3a5c0e0a768a0**.

`QUALIFICATION.json` binds each plan/result, stdout/stderr, original and retained executable, Cargo artifact identity, exact selectors and inventory. Build limits remain 600 aggregate CPU seconds / 900 wall seconds; runtime observer limits 30 CPU / 60 wall seconds, plus each VM's existing 30-second child bound. The 16-GiB memory cap, zero swap, disk floor and output guards are unchanged.

## Preserved failures and report corrections

Successor qualification-v1 contains the three authenticated negative-before native controls (cross-lifetime token collision, replay removal, caught-action mutation). Qualification-v5 contains the authenticated sibling-owner unit failure and both additional real VM failures (wrong sibling and getpid EOVERFLOW). They remain failed receipts, not later rewritten passes. Their plans bind the source and executable hashes used at that time; final and predecessor ELFs are retained separately. Intermediate v2 Clippy size failure and v4 fixture compile errors are also preserved; the latter were an omitted `.await`, incorrect helper spelling and unused mutability, corrected before v5 actually ran. Intermediate v3 green results are history, not substituted for v6.

The predecessor's original raw-0/ENODEV observer failure remains unqualified; its separate authenticated lifecycle recovery is cleanup-only. Earlier stale-TID and notification-cancellation negative receipts remain unchanged. `PREDECESSOR_PRESERVATION.json` authenticates these records and both predecessor ELFs.

Correct two overclaims in the old author report: **mode 12 is a Tool-supplied EFAULT**, not a measured remainder-copyout EFAULT. The read/signalfd/rt_sigtimedwait controls (and new sibling controls) exercise real post-removal copyout faults. Also, `parked_signal_runtime.rs:216` contains the local `#[allow(clippy::too_many_arguments)]` for the existing driver-context flush helper, introduced in v1. It is a style exemption and was incorrectly omitted by the earlier blanket statement. It is unchanged in this successor. No recursion-limit change or recursion-warning suppression was made.

Claude's dismissal of equal-ordinal authorization and its assertion of exactly one caught frame are not adopted: the native controls measured the former collision and latter invalidating write, and both are fixed. Claude's v1 static inventory of 295 was a transcription error; the authenticated predecessor value was 290. Root has identified a grounding deficiency in that review and will obtain a properly grounded successor review. Its concrete F1/F2 observations were independently reproduced here and are not discarded because of that process defect.

## Remaining scope and invariants

This is component qualification. Explicit fixture publications are not proof of Hermit timer expiry, periodic rearm, timed-wait continuation, Linux/ptrace parity, record/replay parity, or a production scheduler determinism guarantee. Process sequence reflects actual backend removals admitted under the process lock; making those guest actions deterministic remains Detcore's scheduling responsibility. The FIFO fixes wrong-owner/duplicate notifications and contiguous delivery; it does not canonically reorder concurrent guest actions. No scheduler, virtual-clock trajectory, INFO/replay representation, or ptrace source was changed.

The caller must answer consuming dequeue notifications, including during terminal cleanup; the component cannot make progress if an arbitrary Tool never answers its RPC. It preserves committed effects instead of canceling those notifications into an ordinary syscall result. The two-thread failure control exercises the actual predecessor-failure wakeup and retained error chain, not every possible hung Tool. Notification injection and nested observation remain structurally refused; no generic depth framework was added.

The flush allocation concern was traced through the actual callers: injected and unsubscribed syscall paths reserve 64 slots before execution; return-to-user/observation selection reserves before taking; each syscall journal is capped before its next removal. Owner filtering prevents a caller's buffer from absorbing arbitrary sibling entries. Therefore final production flushes use the owning caller's prior reservations; adding a fresh allocation after removal is not the remedy. These reservations and their limits remain explicit review targets.

Allocation failure was not fault-injected: the actual capacity negative/positive pair exercised EOVERFLOW at the original limit. Full cross-lifetime handle refusal uses real executor fork/lifecycle controls plus forged-site negative VM calls, not a new multigeneration fork VM workload. No stress count, whole workspace suite, generic Stop-signal observation support, or arbitrary Tool future-cancellation guarantee is inferred from these finite controls.

The reported ptrace recursion-depth diagnostic in the separate combined build is not suppressed or attributed by this component run. Hermit owns the same-toolchain baseline comparison and integrated guest diagnosis. Its first v1 integrated guest failure is not converted into timer-delivery credit here. The historical container SIGALRM victim remains unknown.

Startup identity admission and retired-owner consuming cleanup remain distinct: new removals validate the live admission stamp; already committed cleanup uses the recorded full owner even after lifecycle retirement. Original pending domain and complete event bytes survive replacement, reblocking, post-copyout failures and terminal aggregation. Acknowledgments never imply rollback.

The core site getter still exposes the original site during a live structured observation hook: it denies dequeue-notification and checked-out stack states, while recursive observation is refused separately. An earlier coordination message conflated those checks; source was not changed to suppress genuine hook RPC capability.

## Goalpost-moving assessment and ownership

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

Author assessment: no existing case, assertion, comparator or timeout was weakened. Original modes 0–13 remain; mode 14 and separate mode 15 add obligations. Handle comparison is strengthened from one nonce to the complete site. The explicit existing helper style exemption is disclosed above; no new lint exemption was introduced by the successor. Failed runs stay failed. The raw-execute reservation check was moved to the fallible production boundary, not deleted as a requirement. Independent review must verify these claims.

SCM remains at the assigned branch/base with staged entries identical to base. Raw index metadata drift caused by root's earlier status read was preserved, not restored; START.json and final readback bind the same successor index bytes. All writes stayed in the assigned product slot and its owned evidence directory. Sole source/SCM ownership transfers back to root upon this packet's handoff; further product edits require explicit reassignment. Source review approval and integrated runtime qualification remain pending.
