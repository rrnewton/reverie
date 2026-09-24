# Deterministic-scheduling review

Verdict: **CHANGES REQUIRED**

## Review target and grounding

- Target: `design.md`, SHA-256
  `ab75ee74b384d5681cf8a1b68bdcc3d53317c04f990f5bc2523fe73aadceb1fa`,
  based on `dev-hermit` `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`.
- Affected execution paths: current Linux Hermit/Detcore over Reverie ptrace;
  proposed Narf P0 observation helper; proposed later `reverie-narf` path.
- Claimed behavior: P0 compares a deliberately single-task syscall trace;
  P2 adds lifecycle and signals; P3 adds deterministic scheduling, time,
  instruction interception, filesystem, and controlled networking.
- Claimed validation: specified A0-A18 only; no paired cross-kernel cell and no
  Hermit-on-Narf execution has run (`design.md:413-416`, `556-562`).
- Sources read in the mandated order: the complete 16-page ASPLOS 2020
  *Reproducible Containers* paper, including its artifact appendix (local copy
  SHA-256 `6346dfa0e5ea193fb975d5330da80d030f2a7641d95c7063069c8f8d7d875539`);
  complete `detcore/src/scheduler.rs`, including tests, at Hermit
  `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`; `PROJECT_VISION.md`; and
  `ai_docs/hermit-v2-roadmap.md`; then the complete design.

P0's narrow claim is not the problem: it explicitly excludes threads,
blocking external I/O, signals, instruction events, and deterministic
scheduling (`design.md:270-274`, `397-400`). The findings below apply to the
later promotion gates and to the claim that the document completely specifies
how those gates would prove equivalence.

## Findings

### C1. P3 names scheduler concepts but does not define the event-to-commit state machine

The component table says a later lifecycle source will emit block/wake,
signal, timer, and cleanup records (`design.md:174-182`), the lifecycle sketch
says those events join a total order (`264-268`), and P3 promises a scheduler
event/control contract (`402-403`). A9, however, checks only a runnable set,
selection, block/wake reason/order, an RCB-or-replacement position, and virtual
time (`429-430`). The design never specifies:

- the complete path from a Narf event through `reverie-narf`, Detcore request,
  blocked pool, wake, response, syscall completion, and trace/replay record;
- the deterministic eligibility point for each event;
- every state in which its target can reside (running, tentatively selected,
  queued with a request, polling, each blocked pool, external operation,
  exiting, or already signaled);
- resource request shape before and after each transition;
- run-queue/blocked-pool membership before and after it;
- whether the turn skips, commits, re-enqueues, or advances virtual time; and
- the backend capability that implements each transition.

This is load-bearing, not extra implementation detail. The pinned scheduler
enforces a one-resource request invariant, separates tentative selection from
commit, holds non-runnable threads in several distinct pools, and defers
asynchronous admissions/removals to a deterministic drain. Its own comments
explicitly warn that sorting items present at a drain is insufficient unless a
causal handoff or barrier also fixes drain membership
(`detcore/src/scheduler.rs@ba9c4f4:4478-4810`). A9 can currently pass after a
single coincidental matching run without proving that host callback timing
cannot select a different membership/order on repetition.

Required change: add a normative P2/P3 transition table for every proposed
event class. It must name the allowed input, eligibility barrier, target states,
one-resource request (or a fully audited replacement model), queue/pool
mutations, skip/commit/reenqueue behavior, virtual-time effect, trace/replay
record, and required behavior on Linux ptrace and Narf. Add a promotion test
that deliberately varies callback arrival around request publication,
tentative selection, drain, completion, and exit, while requiring an identical
committed schedule.

### C2. External-I/O readiness has no deterministic membership rule or liveness proof

The trace requires event/readiness order (`design.md:341-343`) and A12 compares
packet bytes, readiness, partial I/O, timeout, and peer close (`432-433`), but
the design never says when a host/kernel completion becomes eligible to affect
the guest schedule. “Join the same total order” (`264-265`) describes the
output, not how host timing stops selecting that order.

The current scheduler treats this as a special hazard: it separates externally
blocked operations, does deterministic-work-first harvesting, distinguishes
ordinary runnable work from pollers, and has explicit behavior when external
completion is the only possible progress
(`detcore/src/scheduler.rs@ba9c4f4:3264-3422`). A Narf callback/ring/RPC can
arrive before or after a drain and thereby change membership even if records
within each drain are sorted.

Required change: define the causal handoff or explicit barrier that fixes which
I/O completions enter each scheduler drain. Specify fairness and progress for
(a) ordinary guest work plus a ready completion, (b) only pollers, (c) multiple
simultaneous completions, and (d) completion as the only possible progress.
Extend A12 with arrival-before/after-drain tests, a nearby not-ready negative
case, multiple ready operations in reversed host callback order, and a bounded
liveness assertion. Exact final packets are not sufficient evidence.

### C3. Signal and interruption gates omit required Linux/POSIX state transitions

A7 is strong about siginfo, real-time ordering, standard-signal coalescing,
target, mask, disposition, and handler boundary; A8 is strong about partial
bytes, `EINTR`, restart, and logical-operation identity (`design.md:427-428`).
They still do not test or normatively define:

- process-directed versus thread-directed target selection from the eligible
  unblocked set;
- ignored, blocked, caught, and default-action outcomes at the wake point;
- atomic temporary-mask semantics for `pselect6`, `ppoll`, and
  `rt_sigsuspend`;
- arrival before/after request publication and tentative selection, while in
  every applicable blocked pool, and after syscall completion or target exit;
- target mask/disposition changing between physical arrival and deterministic
  commitment; or
- proof that one physical signal is neither delivered twice nor consumed while
  remaining pending.

These cases are precisely where an asynchronous backend can be repeatable but
not Linux-compatible. The paper cannot fill the gap: its prototype explicitly
did not support guest-to-guest signals, so it validates neither the proposed
P2 signal path nor Narf delivery semantics.

Required change: split A7/A8 into a signal-state matrix covering the cases
above, with wake and nearby-must-not-wake controls. Preserve partial byte counts
when transfer already occurred; for zero transferred bytes assert the exact
per-syscall Linux restart result under `SA_RESTART`. Record pending-set state
before and after delivery, not only the handler/result trace.

### C4. Virtual-time and preemption acceptance permits incompatible mechanisms

P3 asks for “PMU or replacement preemption” (`design.md:402`), A9 accepts “RCB
or replacement position” (`429`), and A10 asks for fixed clock progression and
expiration order (`430`). “Or replacement” is not a contract: it leaves the
counter domain, boundary, skid handling, async trap ordering, and replay
identity unspecified. “Fixed progression” can also be satisfied by coarse
rounding or freezing, contrary to the product requirement for continuous,
fine-grained virtual time.

The pinned scheduler makes skipped turns noncommitting, advances time only at
defined quiescent boundaries, and has special handling for host-timed polling
retries (`detcore/src/scheduler.rs@ba9c4f4:3956-4009`, `4410-4506`). The design's
normalization correctly says virtual time must be preserved (`design.md:320-346`),
but no test compares the full trajectory or ensures a skipped/aborted turn did
not add a tick.

Required change: freeze an exact preemption-position abstraction before P3:
counter definition, task attribution, overflow/skid behavior, delivery point,
unsupported hosts, record format, and replay equality. Define virtual-time
increments for committed, skipped, polling, blocked, timer-fast-forward,
exec, and teardown transitions. Extend A9/A10 to compare the complete virtual
time/preemption trajectory across repeated reads, multiple threads, and exec;
include positive and negative skipped-turn controls. Explicitly prohibit
rounding, freezing, resetting, or log filtering as a way to obtain parity.

### C5. Record/replay is disclaimed but never designed or gated

The only record/replay text says current Hermit replay is not proof of equality
between independent Linux and Narf runs (`design.md:312-318`). That statement is
correct, but P2/P3 never specify how Narf block/wake, signals, timers, external
completion, lifecycle, and preemption enter a recording or drive replay. No
acceptance row executes replay, checks schedule desynchronization, or compares
replayed trajectories per backend.

Required change: either label Narf record/replay **Deferred** and remove it from
the promotion claim, or specify a provider-neutral recorded-event schema and
add per-backend gates. A valid gate records one realized execution, replays it
on the same exact backend/revision, requires exact schedule and virtual-time
trajectory plus output/state parity, refuses missing/exhausted/desynchronized
events, and then separately performs the Linux-vs-Narf canonical comparison.
Ptrace replay success cannot establish asynchronous Narf replay, and filtering
different event counts is not acceptable.

### C6. Lifecycle tests do not bind asynchronous create/exec/exit membership, and independent-process parallelism is absent

A4-A6 cover important guest outcomes—parentage, `vfork` suspension, nonleader
exec identity, robust-list and clear-TID wake (`design.md:424-426`)—and A14
covers containment (`434`). They do not state the causal/barrier rule that
prevents Narf create/exec/physical-exit callbacks from straddling scheduler
drains, nor do they test callback-order perturbation. The pinned scheduler
requires explicit ordinary-clone, `vfork`, exec-reconnect, and physical-exit
handoffs for this reason (`detcore/src/scheduler.rs@ba9c4f4:4636-4810`,
`7578-7644`).

The product contract also requires eventual deterministic parallel execution
for independent processes. P3's only scheduler case is “two runnable threads”
(`design.md:429`), which neither specifies serialization nor proves safe
parallel commitment.

Required change: for each create/exec/exit path, name the barrier that fixes
first-drain membership and add reversed-callback-order tests. Either mark
independent-process parallel execution explicitly **Deferred**, or define its
independence/conflict rule, deterministic commit order, external-observation
boundary, and record/replay representation and add a conflicting/nonconflicting
pair of tests. Do not infer parallel safety from a two-thread serialized test.

### C7. The evidence plan lacks repetition, exact assurance metadata, and scheduler-specific negative controls

The execution receipt requires commands, revisions, hashes, capabilities,
duration, exit, counts, and artifacts (`design.md:413-416`). A15's six comparator
mutations and A16's missing/truncated evidence tests are excellent anti-goalpost
controls (`435-443`). They do not establish repeatability of a scheduler path,
and “schedule turn” mutation tests the comparator rather than the producer's
host-timing boundaries.

Required change: every P2/P3 scheduler gate must declare backend, exact head,
strictness/log level, relaxations, repetition count, comparator policy, and
nonzero event counts. Require repeated strict verification and the timing-edge
tests from C1-C6. Add a negative control that deliberately removes each
eligibility barrier or shifts an event across its drain and is detected; keep
the existing A15/A16 controls unchanged. Native Linux output is a semantics
reference, not determinism evidence.

## Axis conclusions

### Determinism guarantee

**Not established for P2/P3.** P0 is honestly constrained and may proceed as a
single-task observation POC after its own locked gates. The later design lacks
deterministic membership rules for asynchronous lifecycle, signals, I/O,
timers, and preemption; output ordering and one matching pair cannot substitute
for those rules.

### Linux and POSIX semantics

**Partially specified, not sufficient for promotion.** A2, A7, and A8 correctly
preserve partial effects, signal payload/order, coalescing, and restart identity,
and the unresolved Linux fault boundary is properly labeled. Process/thread
signal selection, temporary-mask atomicity, disposition transitions, delivery
multiplicity, and arrival-state races remain missing.

### Evidence and goalpost-moving

**No weakening found in A15/A16.** They explicitly require six independent
semantic mutations and two independent coverage failures, and forbid tolerance
widening. The evidence bar is nevertheless incomplete because it lacks repeated
strict scheduling runs, producer-boundary negative controls, per-backend replay,
and full virtual-time trajectories. The design must add those checks rather
than normalize or filter the affected fields.

### Paper scope and residual risk

No statement in the design appears to claim that the ASPLOS paper validates
Narf or the proposed extension. That boundary must remain explicit: the paper's
prototype serialized threads, did not support guest-to-guest signals, did not
support sockets, and used a much narrower timer model. It does not prove later
Hermit signal, external-I/O, continuous-time, PMU, replay, backend-parity, or
parallel-execution behavior.

Untested/residual paths include every Narf scheduler path, every asynchronous
Narf callback order, PMU/RCB equivalence, signal restart and temporary-mask
semantics, external-I/O liveness, process parallelism, and Narf record/replay.
Until C1-C7 are resolved, P2/P3 must remain proposed interfaces rather than
promotion-ready proof gates.
