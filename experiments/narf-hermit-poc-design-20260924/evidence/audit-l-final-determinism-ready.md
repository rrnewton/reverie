# Exact-head final deterministic-scheduling review

Verdict: **READY**

## Target and grounding

- Exact reviewed `design.md` SHA-256:
  `8f28be03df3c61507254d27607a04e3757dbe2c38e379aeb2280fc9c5d44c7e6`.
- Base: `dev-hermit`
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`; Hermit/Detcore
  `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`.
- Scope: readiness of the design for owner review, not implementation or
  runtime approval. Every acceptance case remains `specified`; zero paired
  Linux/Narf cells have executed.

The complete exact-head design was reread under the required
deterministic-scheduling grounding: the complete ASPLOS 2020 *Reproducible
Containers* paper including its artifact appendix; the complete 8,436-line
`detcore/src/scheduler.rs`, including tests, at the pinned Hermit revision;
`PROJECT_VISION.md`; and `ai_docs/hermit-v2-roadmap.md`. The intervening
evidence-anchor, security, platform, provider, and diagram edits do not change
the scheduler design or its gates.

## Findings

No blocking determinism, Linux/POSIX, evidence, or goalpost-moving finding is
present at this exact head.

### Causal membership and producer watermarks remain sound

The scheduler, never callback arrival, assigns causal epochs. Its snapshot
names the producer set and outstanding operation IDs; every producer has a
monotonic sequence and must acknowledge a watermark proving all events owned
by that epoch are enqueued; the scheduler waits for every named watermark
before closing the cohort (`design.md:288-295`). Stable guest and operation
identities order only that already-fixed cohort.

Lifecycle events retain committed-operation tokens; guest signals retain
`kill`/`tgkill` or synchronous-fault provenance; timers retain logical
deadlines; and controlled-loopback packets retain committed-send tokens
(`design.md:297-300`). Uncaused signals, packets, and completions are excluded
or produce `no_result: input_mismatch`. A watchdog can produce only
`infrastructure_error`; it cannot choose membership, manufacture readiness,
advance virtual time, or become a semantic timeout (`design.md:300-306`).
Thus callback and transport timing cannot move an event across epochs.

### State transitions and external-I/O progress remain complete

The one-resource invariant, tentative-versus-committed distinction, and zero
time charge for skipped, vanished, cancelled, and transport-only work remain
normative (`design.md:308-314`). The transition table and state rule cover
running, tentative, queued, polling, matching and nonmatching blocked pools,
exiting, exited, and completed-operation states. Each transition records
request shape, queue/pool membership before and after, tentative action,
response, re-enqueue, and time effect (`design.md:316-336`).

External I/O still freezes the outstanding set at request publication, uses a
scheduler snapshot and per-ID causal state, excludes uncaused host I/O, runs
ordinary work first, handles only-poller and completion-only progress with a
stable cursor, and admits multiple completions in stable-ID order only after
all watermarks (`design.md:325`, `338-343`). Probe/wait wall time remains
nonsemantic. A12 still exercises arrival around drains, ordinary work, only
pollers, reversed simultaneous completions, completion-only progress, and a
nearby not-ready operation (`design.md:589`, `618`). This preserves liveness
without allowing host timing to choose guest order.

### Signal and Linux/POSIX gates remain intact

Signal eligibility remains watermarked and based on committed mask and
disposition. Running/queued targets gain pending state rather than an
immediate host-timed turn; only matching interruptible blockers wake; and
exiting/exited targets follow the declared Linux disposition without
re-admission (`design.md:323`). The matrix still covers directedness, recipient
selection, ignored/blocked/caught/default actions, standard coalescing,
realtime ordering, atomic temporary masks, every request/selection/block/
completion/exit arrival edge, disposition changes, pending state, partial
transfer, exact `EINTR`/restart behavior, and multiplicity
(`design.md:345-354`, `584-585`, `613-614`).

### `poc-coop-v1` remains an exact bounded profile

P3 still disables asynchronous PMU preemption and records
`preemption=none`; competing CPU-bound threads without a cooperative boundary
are explicitly unsupported. One committed turn is one selected task consuming
one published request and receiving one response. The manifest clock starts at
a fixed epoch and adds exactly 1 ns per committed turn; skips, aborts, drains,
probes, transport, and teardown add 0 ns; exec preserves time; finite timer
fast-forward targets exactly the smallest deadline; indefinite timers never
fire; per-task CPU time charges only the selected task; and clock/RDTSC values
derive from this state (`design.md:356-368`).

A9/A10 still compare the complete trajectory and negatively test missing
watermarks, callback movement, skipped-turn ticks, wrong 1 ns charges, reset,
rounding, freezing, filtering, and native instruction escape
(`design.md:370-378`, `586-587`, `615-616`). Precise RCB remains genuinely
**Deferred** behind a separate reviewed counter/origin/skid/delivery/trace/
replay specification, and cannot inherit P3 status through an alias
(`design.md:372-378`). This explicit limited profile is not a weakened claim
of continuous fine-grained product time.

### Lifecycle, replay, parallelism, and evidence gates remain intact

- Clone/fork registration precedes parent continuation; `vfork` retains its
  registration and exec/exit barriers; exec replaces identity exactly once;
  physical exit remains PID-barriered; reversed-callback tests remain A4-A6
  (`design.md:319-322`, `581-583`, `610-612`).
- Narf record/replay remains **Deferred** until exact same-provider replay and
  missing/exhausted/desynchronized refusal precede cross-kernel comparison
  (`design.md:380-386`). Linux ptrace replay cannot substitute.
- Independent-process parallelism remains **Deferred** behind a separate
  conflict, independence, commit, visibility, rollback, and replay design with
  conflicting/nonconflicting tests (`design.md:388-392`).
- Receipts still require literal commands, exact heads, strictness/log level,
  relaxations, comparator policy, duration, disposition, counts, and artifact
  hashes. P2/P3 timing gates still require exhaustive state/arrival
  permutations and at least 100 strict repetitions (`design.md:598-602`).
- A15 remains six semantic mutations. A16 still distinguishes missing coverage
  from malformed/truncated comparison refusal and forbids a generic result,
  match, or semantic divergence. A17/A18 retain their non-weakening clauses
  (`design.md:592-596`, `621-628`).

## Axis conclusions

### Determinism guarantee

**Adequately specified for the claimed bounded profiles.** Watermarks fix
membership, stable identities fix order, the state machine is fail-closed,
external progress cannot use host time as semantic input, and cooperative time
has an exact trajectory. Implementation and execution remain required before
any capability can be promoted.

### Linux and POSIX semantics

**Adequately specified as falsifiable gates.** Signal selection and pending
state, interruption/restart, partial I/O, temporary masks, lifecycle,
waitability, and timer behavior are tested as guest-visible semantics rather
than resemblance to one native schedule.

### Evidence and goalpost-moving

**No weakening found.** The new exact head strengthens source anchors and
security/platform wording without changing scheduler expectations,
comparators, repetitions, negative controls, refusal classes, or phase scope.
No source declaration or test name is promoted to runtime support.

### Paper scope and residual risk

The design still states that the paper's prototype did not support
guest-to-guest signals or sockets and is background rather than validation for
the proposed paths (`design.md:352-354`). It does not use the paper to validate
Narf, the watermark protocol, PMU behavior, replay, or parallel execution.

Residual risks remain explicitly unresolved or deferred: transport choice,
SMP task lifetime, uncharacterized fault boundaries, Narf input
virtualization, instruction interception, record/replay, independent-process
parallelism, and RCB preemption. These are implementation and future-profile
risks, not hidden support claims in the owner-review-ready design.
