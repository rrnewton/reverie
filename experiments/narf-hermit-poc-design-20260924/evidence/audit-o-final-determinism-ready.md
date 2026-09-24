# Exact-head deterministic-scheduling re-review

Verdict: **READY**

## Target

- Exact reviewed `design.md` SHA-256:
  `2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`.
- Base: `dev-hermit`
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`; Hermit/Detcore
  `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2`.
- Scope: design readiness only. No implementation or paired runtime support is
  approved; all acceptance cases remain `specified`.

The exact-head design was re-reviewed under the complete previously grounded
deterministic-scheduling sources: the ASPLOS 2020 *Reproducible Containers*
paper including its artifact appendix; the full pinned
`detcore/src/scheduler.rs`, including tests; `PROJECT_VISION.md`; and
`ai_docs/hermit-v2-roadmap.md`.

## Findings

No blocking finding is present. The only change since Audit L is sound: the
risk table now states that filesystem and network are uncontrolled semantic
input classes for this POC, rather than making an unsupported current-behavior
assertion (`design.md:644`). It retains the requirement to virtualize, capture,
or exclude each class and prove that disposition in the receipt. External
network remains excluded from P0 (`design.md:644`, `652-655`). This wording
change neither admits an input nor alters scheduler behavior.

The scheduler guarantees remain unchanged:

- Scheduler-issued snapshots and monotonic producer watermarks close causal
  cohort membership before stable ordering; controller arrival time is never
  an epoch input (`design.md:288-295`). Uncaused physical signals, packets, and
  completions are excluded or produce `no_result: input_mismatch`; watchdogs
  can produce only infrastructure failure (`design.md:297-306`).
- One-resource requests, tentative-versus-committed selection, zero time for
  skipped/aborted/transport-only work, and complete running/queued/polling/
  blocked/exiting/completed state dispositions remain normative
  (`design.md:308-336`).
- External I/O still freezes the outstanding set, uses watermarked causal
  state, runs ordinary work first, orders simultaneous completions by stable
  ID, supplies completion-only progress without a busy loop, and leaves a
  nearby not-ready operation blocked (`design.md:325`, `338-343`). A12 retains
  the arrival-edge, reversed-callback, only-poller, completion-only, and
  not-ready controls (`design.md:589`, `618`).
- Signal/POSIX coverage still includes directedness, selection, pending sets,
  dispositions, standard coalescing, realtime ordering, temporary-mask
  atomicity, arrival boundaries, partial transfer, exact `EINTR`/restart, and
  multiplicity (`design.md:323`, `345-354`, `584-585`, `613-614`).
- `poc-coop-v1` remains exact: `preemption=none`, one published request per
  semantic turn, exactly 1 ns per commit, zero charge for skips/drains/probes/
  transport/teardown, preserved time across exec, exact finite-deadline
  fast-forward, and no firing of indefinite deadlines (`design.md:356-368`).
  A9/A10 retain full trajectory comparison and wrong-delta, skipped-tick,
  reset, rounding, freezing, filtering, watermark, and native-escape negative
  controls (`design.md:370-378`, `586-587`, `615-616`). Precise RCB remains
  explicitly deferred behind a separate reviewed specification.
- Clone/fork, `vfork`, exec, and physical-exit barriers remain intact
  (`design.md:319-322`, `581-583`, `610-612`). Narf replay and independent-
  process parallelism remain explicitly deferred with their future proof
  obligations (`design.md:380-392`).
- Exact commands, heads, strictness, log level, relaxations, comparator,
  counts, hashes, exhaustive boundary permutations, and at least 100 strict
  repetitions remain mandatory (`design.md:598-602`). A15 remains six semantic
  mutations; A16 still distinguishes missing coverage from truncation; A17 and
  A18 retain their anti-weakening clauses (`design.md:592-596`, `621-628`).

## Axis conclusions

### Determinism guarantee

**Sound as a proposed bounded design.** Membership, ordering, state
transitions, I/O progress, and cooperative virtual time remain independent of
host callback timing. Runtime proof remains a future promotion requirement.

### Linux and POSIX semantics

**Still adequately specified as falsifiable gates.** The risk-table correction
does not change signal, interruption, partial-I/O, lifecycle, timer, or
temporary-mask semantics.

### Evidence and goalpost-moving

**No weakening found.** No comparator, assertion, tolerance, repetition,
negative control, phase boundary, or refusal condition changed. Calling VFS
and network uncontrolled POC input classes is more precise and preserves the
fail-closed gate.

### Paper scope and residual risk

The paper remains background rather than evidence for guest-to-guest signals,
sockets, Narf, producer watermarks, PMU preemption, replay, or parallel
execution (`design.md:352-354`). Transport, SMP lifetime, input
virtualization, Narf replay, process parallelism, and RCB remain honestly
unimplemented, unresolved, or deferred.
