# Final deterministic-scheduling review

Verdict: **CHANGES REQUIRED**

## Review target and required grounding

- Target: `design.md`, SHA-256
  `3c43221354848fdcc3df17fdbc6b56b80228057dd94615c1a7a776bf529f5a61`.
- Base: `dev-hermit`
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`; Hermit/Detcore
  `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`.
- Review scope: the proposed P2 lifecycle/signal path and P3 scheduling,
  external-I/O, preemption, and virtual-time path. P0 remains explicitly
  single-task and excludes these claims.
- Claimed validation remains specified rather than executed; no runtime result
  was produced or treated as evidence in this review.

The prescribed sources were read in order before the revised design: the
complete 16-page ASPLOS 2020 *Reproducible Containers* paper, including its
artifact appendix (local author-hosted copy SHA-256
`6346dfa0e5ea193fb975d5330da80d030f2a7641d95c7063069c8f8d7d875539`);
the complete 8,436-line `detcore/src/scheduler.rs`, including tests, at Hermit
`ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`; `PROJECT_VISION.md`; and
`ai_docs/hermit-v2-roadmap.md`. I then read the complete prior
`audit-determinism.md` and the complete revised `design.md`.

## Blocking findings

### C1/C2. The named causal epoch does not yet fix drain membership

The revision correctly states that sorting an already host-selected set is
insufficient and introduces pending admissions, causal epochs, and a drain
cutoff (`design.md:287-296`). It also gives external-I/O ordering and liveness
rules (`design.md:315-322`) and acceptance permutations (`design.md:552-555`,
`580-583`). The remaining problem is that the stated mechanism is still:

> at the start of a scheduler turn, close the epoch; arrivals before the cutoff
> enter this epoch and arrivals after it enter the next.

That makes transport or callback arrival time decide membership. A completion
that is physically the same guest event can reach the controller immediately
in one run and after the cutoff in another, yielding different first eligible
turns. A stable producer identity orders events already in a cohort; the words
“causal epoch” do not establish that every event belonging to the epoch has
arrived before it is closed.

The same hole is visible in the external-I/O row. “Completion enters the next
open drain” (`design.md:315`) does not identify a scheduler-controlled readiness
snapshot, producer watermark, or end-of-epoch acknowledgement. The liveness
case “wait boundedly” (`design.md:321`) also does not say whether the bound is a
host wall-clock timeout used only to classify infrastructure failure or a
semantic cutoff that can change which completions join the cohort. With several
outstanding operations, waking on the first callback and then sorting what has
arrived still leaves cohort membership host-timed.

This is the precise distinction enforced by the pinned scheduler. Deferred
admissions drain at one location, but each production site separately needs a
causal handoff or explicit barrier that fixes which admissions reach that drain
(`detcore/src/scheduler.rs@ba9c4f4:4810-4867`, `5046-5075`). External I/O is
handled as a special liveness and readiness problem rather than as an ordinary
sorted callback list (`detcore/src/scheduler.rs@ba9c4f4:3271-3422`). The design's
100-run and shifted-drain tests can detect one implementation defect; they
cannot supply the missing production rule.

Required change:

1. Define who assigns an event's epoch and the evidence that closes the epoch.
   Acceptable shapes include a kernel-owned per-producer sequence plus an
   explicit watermark/fence from every producer, or a scheduler-issued
   synchronous snapshot/probe of every outstanding operation. Controller
   arrival before a wall-clock cutoff is not acceptable.
2. For external I/O, define the exact readiness-snapshot/cohort rule when there
   is ordinary work, only pollers, multiple completions, and completion-only
   progress. State that a host timeout can terminate with infrastructure error
   but cannot select canonical membership or advance virtual time.
3. Apply the same rule to physical signal arrival: identify guest-causal and
   timer signals versus uncontrolled external signals, and either capture the
   latter as declared input or exclude them. Do not assign their semantic epoch
   from controller arrival timing.
4. Make the state table normative per target state. In particular, resolve the
   current ambiguity where syscall completion permits a task to be “running or
   blocked” but unconditionally says to remove “that blocker”
   (`design.md:308`). For running, tentatively selected, queued, polling, every
   relevant blocked pool, exiting, and already-completed states, name exactly
   one transition or a justified no-op/error, including the one-resource
   request, queue/pool membership, tentative-pop disposition, response,
   re-enqueue, and time effect.

Until this is specified, C1's normative transition state machine and C2's
deterministic external-I/O membership proof are not closed. The bounded
liveness test plan itself is good and should remain.

### C4. P3 requires a future versioned preemption/time profile but does not define one

The revision materially improves the bar: it selects retired conditional
branches rather than “RCB or replacement,” records task/generation/phase,
target, overflow and skid fields, rejects counter aliases, compares the full
trajectory, and forbids rounding, freezing, resetting, and filtering
(`design.md:335-345`, `552-553`, `580-581`). It also correctly makes skipped,
aborted, polling, transport, and teardown work noncharging and preserves exact
finite-deadline fast-forward (`design.md:298-304`, `340-345`).

However, the text says the exact profile and charge function will be versioned
before execution; it does not define them now. Missing normative choices
include:

- which conditional branches count (guest user mode only, injected/backend
  code, syscall transitions, handlers, and kernel execution);
- the counter origin/reset rule and how exec generation changes it;
- the programmed threshold, overflow attribution, skid-correction algorithm,
  exact delivery boundary, and fail-closed unsupported-host conditions;
- the record/replay equality identity for a preemption point; and
- the actual virtual-time delta/function for each committed syscall,
  preemption, signal delivery, lifecycle transition, and ordinary guest turn.

“Advances monotonically on committed guest progress” is not an exact charge
function. Two providers could agree on a coarse but different-from-Hermit
charge and pass a cross-provider equality test. The negative controls reject
freezing and obvious rounding but do not establish the intended continuous,
fine-grained trajectory. The pinned scheduler's rules are operationally exact:
skips and internal turns suppress scheduler charging, polling has special
timeout-preserving treatment, committed time cannot go backward, and timer
fast-forward is a distinct transition (`detcore/src/scheduler.rs@ba9c4f4:4417-4506`,
`3697-3855`).

Required change: either define one complete P3 profile in this document (or a
versioned normative appendix) with the counter domain, origins, attribution,
skid/delivery algorithm, unsupported behavior, trace identity, and explicit
virtual-time transition/function table, or label P3's preemption/time mechanism
**Deferred** and remove it from the promotion-ready claim. A manifest that
merely records an implementation-chosen version is provenance, not the missing
contract.

## C1-C7 closure ledger

| Prior item | Result | Evidence and residual |
|---|---|---|
| C1 normative event-to-commit state machine | **Not closed** | The transition table, one-resource rule, tentative/commit distinction, and skip-time rule are useful additions (`design.md:287-316`), but epoch closure is arrival-timed and the per-target-state mapping is incomplete/ambiguous. See finding C1/C2. |
| C2 external-I/O membership and liveness | **Not closed** | The four liveness cases and A12 positive/negative tests are present (`design.md:318-322`, `555`, `583`), but no producer closure/watermark or scheduler-controlled readiness snapshot fixes cohort membership. |
| C3 signal and Linux/POSIX matrix | **Closed subject to C1's common eligibility fix** | The design now covers directedness, recipient selection, ignored/blocked/caught/default disposition, standard coalescing, realtime ordering, temporary-mask atomicity, all requested arrival edges, mask/disposition changes, pending sets, multiplicity, partial transfer, `EINTR`, and per-syscall restart behavior (`design.md:313`, `324-333`, `550-551`, `578-579`). It explicitly refuses to use the paper as validation. Uncontrolled physical signal arrival still needs the C1 epoch/input rule. |
| C4 exact preemption and virtual-time contract | **Not closed** | The comparison and anti-normalization bar is closed, but the actual profile and charge function are deferred to a future version rather than specified. See finding C4. |
| C5 record/replay disposition | **Closed** | Narf replay is explicitly **Deferred**, excluded from P0-P3 promotion, and given a correct future same-provider replay/refusal gate before cross-kernel comparison (`design.md:347-353`). |
| C6 lifecycle barriers and parallelism | **Closed** | Clone, `vfork`, exec, logical/physical exit barriers and reversed-callback tests are specified (`design.md:309-312`, `547-549`, `575-577`). Independent-process parallelism is explicitly **Deferred** with the necessary future conflict/commit/effects/replay contract and paired tests (`design.md:355-359`). |
| C7 repetitions, assurance metadata, and negative controls | **Closed** | P2/P3 receipts name literal commands, exact heads, strictness/log level, relaxations, comparator, counts and hashes; timing gates require exhaustive synthetic permutations and at least 100 strict repetitions (`design.md:505-567`). A9/A12 contain producer-boundary controls and A15/A16 remain six semantic mutations plus two fail-closed coverage faults (`design.md:552-559`, `586-594`). |

## Axis conclusions

### Determinism guarantee

**Not established for P2/P3.** The revised acceptance plan is substantially
stronger, but host callback arrival can still move an event across the proposed
epoch cutoff, and P3's preemption/time function is not yet normative. P0's
single-task observation claim is unaffected and remains honestly constrained.

### Linux and POSIX semantics

**The signal/interruption test contract is now adequate as a proposed gate.**
It preserves partial transfers, per-syscall restart behavior, temporary-mask
atomicity, target selection, pending state, coalescing/queueing, dispositions,
and delivery multiplicity. Its determinism still depends on fixing the common
eligibility rule; the paper does not validate this extension.

### Evidence and goalpost-moving

**No goalpost weakening found.** A15/A16 retain every prior mutation and
coverage refusal, P2/P3 add repetitions and timing-edge controls, the comparator
continues to preserve virtual time and semantic counts, and unsupported or
missing evidence cannot become a match. Repetition must not be used as a
substitute for the missing membership and exact-profile arguments.

### Paper scope and residual risk

The design now states the paper boundary correctly. The paper validates the
older syscall-serialization/nonblocking-retry model, not guest-to-guest signals,
sockets, current continuous virtual time, PMU preemption, asynchronous-backend
callbacks, Narf, record/replay parity, or parallel execution. Residual untested
areas remain every Narf scheduling path, every asynchronous callback boundary,
the final signal-input/epoch rule, external-I/O cohort closure, the exact RCB
profile, and the exact virtual-time charge trajectory.

The two blocking changes above are design changes only. They do not require
claiming unexecuted runtime support, weakening the comparator, or expanding P0.
