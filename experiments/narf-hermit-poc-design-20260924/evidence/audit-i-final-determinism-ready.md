# Final deterministic-scheduling readiness review

Verdict: **READY**

## Review target and grounding

- Exact target: `design.md`, SHA-256
  `eaa877e81b067d63d0792771c6421c2bb3c4c1368a5a6757cc54b8071b818efd`.
- Base: `dev-hermit`
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`; Hermit/Detcore
  `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`.
- Scope of this verdict: the design is ready for owner review. It is not an
  implementation approval and does not convert any `specified` acceptance case
  into executed Linux, executed Narf, or paired evidence.

This re-review uses the required grounding already read in order for the prior
exact-head review: the complete 16-page ASPLOS 2020 *Reproducible Containers*
paper including its artifact appendix (author-hosted copy SHA-256
`6346dfa0e5ea193fb975d5330da80d030f2a7641d95c7063069c8f8d7d875539`);
the complete 8,436-line `detcore/src/scheduler.rs`, including tests, at Hermit
`ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`; `PROJECT_VISION.md`; and
`ai_docs/hermit-v2-roadmap.md`. I then read the complete corrected design at
the hash above.

## Findings

No blocking deterministic-scheduling, Linux/POSIX-semantics, evidence, or
goalpost-moving finding remains in the corrected design.

### C1/C2: producer closure now fixes membership independently of callback timing

The scheduler, rather than the transport callback, assigns causal epochs. A
scheduler-issued snapshot names every producer and outstanding operation;
each producer supplies a monotonic-sequence watermark proving all events
causally owned by that epoch have been enqueued; and the scheduler waits for
all named watermarks before closing the cohort (`design.md:294-301`). This is
the missing membership proof. Ordering by guest identity and operation ID now
happens only after membership is fixed.

The design also binds each supported producer to a deterministic source:
lifecycle events to committed lifecycle operations, guest signals to committed
signal operations or synchronous faults, timers to logical deadlines, and
controlled loopback packets to a committed-send token (`design.md:303-306`).
An uncaused physical signal, packet, or completion is excluded or produces
`no_result: input_mismatch`; a host watchdog can only produce
`infrastructure_error` and cannot affect membership, readiness, or virtual time
(`design.md:306-312`). Therefore transport latency and callback order cannot
move the same semantic event between cohorts.

External-I/O behavior is now sound for the proposed bounded profile. The
outstanding set freezes when requests publish, the snapshot names every ID,
the provider reports causal-token state for every ID, ordinary work runs first,
only-poller and completion-only progress use a stable cursor, and multiple
ready completions are admitted by stable ID only after all watermarks
(`design.md:331`, `344-349`). Probe/wait wall time is nonsemantic. The nearby
not-ready control and callback-order permutations remain in A12
(`design.md:594`, `623`). This matches the pinned scheduler's distinction
between deterministic cohort membership and ordering within a cohort, while
providing a cleaner proposed producer fence for Narf.

The state dispositions are sufficiently exhaustive for a design contract.
Running, tentative, queued, polling, matching and nonmatching blockers,
exiting, exited, and completed-operation states each have a mutation,
buffer/no-op, discard-if-Linux-permits, or protocol-error disposition. Every
transition records the one-resource request, queue/pool membership, tentative
action, response, re-enqueue, and time effect (`design.md:314-342`). The earlier
running-versus-blocked completion ambiguity is resolved at `design.md:324`.

### C3: signal and Linux/POSIX obligations remain intact

Signals use the same watermarked eligibility rule without weakening the
semantic matrix. The design retains process- versus thread-directed selection,
ignored/blocked/caught/default dispositions, standard-signal coalescing,
realtime ordering, atomic temporary masks for `pselect6`, `ppoll`, and
`rt_sigsuspend`, arrival on both sides of every scheduler boundary,
mask/disposition changes, pending sets, and multiplicity
(`design.md:329`, `351-360`). A7/A8 still require exact partial transfer,
`EINTR`/restart, remainder, target, payload, and nearby-must-not-wake evidence
(`design.md:589-590`, `618-619`). Uncontrolled external signals are refused,
so host timing is not smuggled into the deterministic result.

### C4: `poc-coop-v1` is exact; RCB is genuinely deferred

P3 now names one precise, falsifiable profile rather than an undefined
“preemption or replacement” abstraction. `poc-coop-v1` has no asynchronous
preemption; CPU-bound competitors that reach no cooperative boundary are
explicitly unsupported. A committed turn is exactly one task consuming one
published one-resource request and receiving its response. The clock starts at
the manifest epoch and advances exactly 1 ns per committed turn, with a 0 ns
charge for skipped/aborted selections, admission drains, probes, transport, and
teardown. Exec preserves time; a finite timer fast-forwards exactly to the
smallest eligible deadline; indefinite deadlines never fire; per-task CPU time
charges only the selected task; every clock API and Tool-selected RDTSC value
derives from that state (`design.md:362-374`).

A9/A10 compare the complete turn/time trajectory and reject a wrong 1 ns
charge, skipped-turn tick, reset, freezing, rounding, filtering, or native
instruction escape (`design.md:376-384`, `591-592`, `620-621`). This does not
claim the product's future continuous fine-grained/precise-preemption behavior.
The proposed `poc-rcb-v1` is explicitly **Deferred** until a separate reviewed
specification supplies the full counter, origin/reset, skid, delivery,
unsupported-host, trace, and replay contract (`design.md:378-384`). No RCB
result can inherit P3 status by alias.

This scope change is not evidence goalpost weakening: the receipt must say
`preemption=none`, CPU-bound cases are `unsupported`, P3 is explicitly renamed
the cooperative profile, and precise RCB remains an unclaimed future profile.
The tests remain exact for the claim actually made.

### C5-C7 and adjacent controls remain closed

- Narf record/replay remains explicitly **Deferred**. A future gate must first
  record and exactly replay on the same backend/revision, refuse missing,
  exhausted, or desynchronized records, and only then compare kernels
  (`design.md:386-392`). Linux ptrace replay cannot stand in for Narf.
- Clone/fork, `vfork`, exec, and logical/physical exit retain causal and
  PID-specific barriers, with reversed-callback tests (`design.md:325-328`,
  `586-588`, `615-617`). Independent-process parallelism remains explicitly
  **Deferred** behind a separate conflict, commit, external-effects, rollback,
  and replay design (`design.md:394-398`).
- Completed manifests still require literal commands, exact heads,
  strictness/log level, relaxations, comparator policy, duration, disposition,
  nonzero counts, and hashes. P2/P3 timing gates retain exhaustive synthetic
  permutations and at least 100 strict end-to-end repetitions
  (`design.md:603-607`).
- A15 remains six independent semantic mutations. A16 is now more explicit:
  missing family must yield `no_result: coverage`, while truncation must yield
  `no_result: comparison_refused`; a generic result, match, or semantic
  divergence is forbidden (`design.md:597-599`, `626-633`). A17 cannot be
  weakened to same-address-space/word access, and A18 requires observed
  Tool-selected CPUID/RDTSC values rather than setup success.

## Axis conclusions

### Determinism guarantee

**Adequately specified for the claimed P0-P3 design profiles.** Producer
watermarks fix membership, stable identities fix within-cohort order,
one-resource transitions have explicit state dispositions, noncommitting work
does not charge time, and the cooperative clock is exact. Unsupported and
uncontrolled inputs fail closed. Implementation and execution evidence are
still future work and remain promotion prerequisites.

### Linux and POSIX semantics

**Adequately specified as falsifiable promotion gates.** The signal,
interruption, partial-I/O, lifecycle, waitability, timer, and temporary-mask
requirements preserve Linux-visible behavior rather than merely matching one
native schedule. The design chooses one explicitly bounded cooperative
personality and labels cases outside it unsupported instead of silently
changing their semantics.

### Evidence and goalpost-moving

**No weakening found.** The corrected text narrows claims explicitly rather
than loosening comparisons: all trace values and trajectories remain exact;
coverage and malformed-input failures are more specific; repetitions and
producer-boundary negative controls remain; unexecuted cases stay
`specified`; and current source evidence is not promoted to runtime support.

### Paper scope and residual risk

The paper boundary remains correct at `design.md:358-360`: the ASPLOS
prototype did not support guest-to-guest signals or sockets and is background,
not validation, for the proposed paths. It likewise does not validate Narf,
the watermark protocol, current continuous virtual time, PMU preemption,
record/replay parity, or parallel execution.

Residual risks are honestly classified: no Narf path or paired cell has run;
transport, SMP task lifetime, exact page-fault cases, input virtualization,
instruction interception, Narf replay, independent-process parallelism, and
RCB preemption still require implementation and exact-head evidence. None is
misrepresented as current support or silently admitted into the bounded POC
profile.
