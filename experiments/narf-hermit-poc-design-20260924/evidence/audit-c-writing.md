# Audit C: independent post-draft writing audit

## Verdict

**CHANGES REQUIRED**

The draft gives the owner a clear recommendation, keeps the five required
mechanisms in one comparison, and preserves the locked current-state facts.
It is not ready for owner review because P0 cannot satisfy several of its own
mandatory gates as written, the acceptance matrix omits the execution fields
required by Audit B, and P3 does not yet state a falsifiable determinism
contract. The remaining findings are presentation and disclosure corrections;
none requires reopening a locked current-state fact.

Review target:

- draft SHA-256:
  `ab75ee74b384d5681cf8a1b68bdcc3d53317c04f990f5bc2523fe73aadceb1fa`;
- locked Audit A SHA-256:
  `f7d7396f098949f7f2b790ec5c59b4b9dc7507bc6fab4eb11eef7ebdce908c3b`;
- Audit B SHA-256:
  `9c7ad966ea3b5c3cf0fca39d2729207c7a06b3ead2dedec6f20f5641c1868ceb`.

No runtime, Hermit command, QEMU guest, commit, push, or publication occurred
in this audit.

## Findings

### C-1 — Severity 1: P0 has no authoritative source for its mandatory lifecycle and cleanup records

The component table says the task/lifecycle source will emit authoritative
create/exec/exit and cleanup records “later” (`design.md:177`), while P0 is
defined as dispatcher enter/exit plus output, exit, and a coverage receipt
(`design.md:397-400`). A0 nevertheless requires task start, an initial exec
boundary, exact exit, and proof of no leaked task (`design.md:420`). The event
sequence also requires start/exec before the first semantic event and teardown
after the last one (`design.md:256-268`), but P0 is explicitly only a subset
(`design.md:270-274`).

Impact: a dispatcher-only P0 can emit syscall entry/exit but cannot produce the
complete A0 receipt it uses to justify `matched`. Synthesizing lifecycle events
in the comparator would weaken the evidence bar.

Required change: put a minimal authoritative task-start/initial-exec/exit and
teardown source in P0, name its ownership and ordering relative to the
dispatcher, and state how stdout/stderr and no-leak evidence are collected.
Keep the richer fork/thread/signal lifecycle in P2; do not weaken A0.

### C-2 — Severity 1: A1 and A15 require capabilities excluded from P0

P0 permits observation/`Execute` only (`design.md:217-222`) and excludes
mutation, scheduling, and signals (`design.md:270-274`, `design.md:397-400`).
Yet A1 requires exact `getrandom` bytes and a “virtual PID” (`design.md:421`).
The locked fact set says current Narf's randomness is an uncontrolled CSPRNG
input, while the draft leaves PID renaming unresolved and exact in P0
(`design.md:327-335`, `audit-a-lock3.md:163-167`). P0 therefore has neither a
declared seeded-random path nor an identity rule that makes A1's expected values
obtainable. Separately, P0 requires all six A15 mutations, including schedule
turn and signal payload (`design.md:399`, `design.md:435`), even though a valid
P0 trace contains neither family.

Impact: P0's mandatory positive pair and comparator gate are not executable as
specified. An implementation could only pass by adding undeclared
normalization, skipping fields, or fabricating event families.

Required change: add a P0 random-input/emulation contract and freeze the PID
identity rule before execution, or explicitly move the affected positive case
to the first phase that supplies those contracts while retaining an equivalent
P0 buffer test. Preserve all six A15 mutations by defining a phase-independent,
full-schema golden comparator corpus; also run live mutations for every family
the current phase claims. Do not reduce A15 to the conveniently observable
fields.

### C-3 — Severity 1: the acceptance matrix is specified but not executable

The draft says a future execution record “must add” commands, hashes, duration,
status, counts, and artifact locations (`design.md:411-416`). Its A0–A18 table
contains only phase, guest/action, evidence, and outcome/control
(`design.md:418-438`). Audit B required Linux and Narf command/artifact
placeholders, prerequisites, expected prefix/count, evidence status, duration,
exit status, revisions, and artifact hashes in the acceptance appendix
(`audit-b.md:262-277`).

Impact: the nineteen rows are good test intentions, but they are not yet an
executable acceptance manifest. Different implementers could choose different
drivers, artifact identities, repeat counts, or comparators and still claim the
same row passed.

Required change: add a versioned per-row manifest or table with concrete driver
command templates for both cells, prerequisite capability/phase, expected
record prefix and count constraints, comparator invocation and expected class,
negative-control invocation, evidence status, timeout, and required artifact
metadata. Unknown commands may remain explicit placeholders tied to the phase
that must fill them, but the schema and refusal behavior must be executable and
machine-checkable.

### C-4 — Severity 1: P3's “deterministic world” gate checks an outcome, not the determinism mechanism

The proposal says later event families join one total order (`design.md:264-265`),
P3 requires identical realized schedules (`design.md:402`), and A9 compares one
runnable-set/selection/block/wake trajectory (`design.md:429`). The unresolved
row asks for explicit control/observation (`design.md:456`). It does not state
where host-timed readiness, transport arrival, physical exit, signals, or
external I/O stop influencing membership in a scheduling decision; how
membership is fixed before ordering; how skipped versus committed turns and
virtual time behave; or how repeatability and liveness are tested.

Impact: two paired runs could happen to realize the same schedule without Narf
being deterministic, and a sorted set of already-arrived events could still
have host-timing-dependent membership. That is insufficient for the P3 label
and for later multithreaded/time-sensitive equivalence claims.

Required change: define the deterministic eligibility/commit barrier, every
host-timed input's handoff into guest-causal state, thread transitions among
running/queued/polling/blocked/exiting states, fairness/liveness, continuous
virtual-time advancement, and record/replay representation. Extend A9/A10/A12
with repeated same-input runs, arrival-before/after boundary cases, only-poller
and external-completion progress cases, and full virtual-time trajectory
comparison. Keep P3 explicitly **Proposed** until those gates execute.

### C-5 — Severity 2: the diagram visually promotes proposed artifacts to current

The caption says solid boxes are current and only `[proposed]` boxes are absent
(`design.md:224-227`). The diagram then draws `canonical trace Tool` and
`fail-closed comparator` without `[proposed]` (`design.md:228-242`), even though
the cross-kernel trace and comparator are proposed. It also does not show the
guest/kernel/controller/harness address-space and trust boundaries required by
Audit B (`audit-b.md:296-309`).

Impact: this is stronger than the locked fact that current Hermit supplies
trace discipline, not an existing cross-provider format. A reader can
reasonably read the central POC artifacts as implemented.

Required change: mark the canonicalizer, Narf adapter, transport, coverage
receipt, and comparator as proposed; retain the Linux reference path as current;
and draw the address-space/trust boundaries plus separate event/data and
control/disposition directions.

### C-6 — Severity 2: the crash sentence collapses guest disposition and infrastructure failure

The trace table correctly includes exit-by-signal/core as a final guest
observation (`design.md:310`), and the outcome table reserves
`infrastructure_error` for provider/kernel/harness failures
(`design.md:354-365`). “A crash is not a semantic mismatch” at
`design.md:367-369` is therefore ambiguous: a guest crash is a disposition that
can match or diverge, while a provider, kernel, controller, or harness crash is
an infrastructure error.

Required change: name those two cases explicitly and keep guest signal/core
disposition in the canonical trace.

### C-7 — Severity 2: claim labels are not applied consistently

The label definition reserves **Optional capability** for a valid opt-in
facility (`design.md:42-53`), but the not-yet-existing framekernel/BPF path is
called an “Optional P4 isolation follow-on” and P4 is labeled “Optional
capability / Deferred” (`design.md:163`, `design.md:403`). The locked current
classification is existing ingredients plus a **Proposed** extension
(`audit-a-lock3.md:204-212`). The stock-ptrace table row also mixes bounded
executed ptrace evidence with source-only gaps under one unqualified “Verified
current” cell (`design.md:157-160`). Material current assertions such as the
three-implementation count (`design.md:68-71`) and the inference from loader
support to the recommended static P0 (`design.md:131-136`) do not visibly split
**Verified current** from **Proposed**.

Impact: none of the underlying current facts is false, but the labels blur
source versus execution and can make a proposed extension look like an
advertised current capability.

Required change: label P4's extension **Proposed / Deferred** until it exists;
reserve **Optional capability** for a current advertised facility. Split mixed
cells into `(executed, bounded)` and `(source-only)` clauses, and label the
loader-to-P0 recommendation **Proposed**.

### C-8 — Severity 2: required uncertainty disclosures and the platform matrix are incomplete

The evidence boundary is strong, and the risk table preserves most locked
unknowns. The draft does not state that public links were not interactively
browser-tested; it never explicitly says no Narf record/replay input-capture
implementation was found; and it does not preserve SMP stopped-task
synchronization as an unproved Narf ptrace behavior. These were mandatory Audit
B disclosures (`audit-b.md:404-439`). The appendix gives a complete provider
inventory (`design.md:487-505`) but no explicit Linux/Narf by x86_64/aarch64
platform matrix, despite the frozen research contract's provider/platform
matrix requirement. The x86_64-only recommendation and aarch64 risk appear in
prose, but the platform denominator is not auditable in one place.

Required change: add the three missing disclosures and a compact platform
matrix covering boot/loader evidence, syscall-stop architecture, instruction
interception prerequisites, executed evidence, and POC disposition for Linux
x86_64, Narf x86_64, and Narf aarch64. Do not infer aarch64 runtime support from
source declarations.

## Checks that passed

- The opening gives a direct, bounded owner decision and says P0 is a
  nonconforming software helper (`design.md:7-31`).
- The five required mechanisms appear once under a consistent criterion set
  (`design.md:151-168`); stock ptrace is not rejected merely for absent SEIZE or
  LISTEN.
- LiteInst direct/hybrid and e9patch Reverie/Hermit terminology follow the
  immutable facts (`design.md:68-79`).
- Proposed interface ownership, checked data movement, teardown, trace
  normalization, refusal/divergence classes, security/TCB, controlled inputs,
  and P0–P4 staging all have authoritative homes.
- All nineteen A0–A18 rows are present. A2 retains the unresolved exact native
  boundary, A15 retains six mutations, A16 retains both refusal injections, A17
  retains distinct-address-space 32-byte movement, and A18 requires effective
  CPUID/RDTSC interception rather than successful setup calls.
- Exact parent, Hermit, nested/standalone Reverie, Narf, Linux man-pages, and
  vendored-provider revisions are recorded (`design.md:470-485`). Query-pair,
  document-class, exclusion, inverse-check, provider, and preliminary-finding
  denominators are present (`design.md:507-534`).
- The no-runtime boundary, failed exact-commit x86_64 QEMU job, source-only
  provider conclusions, unresolved process-vm boundary, boot identity, domain
  enforcement, and deferred scope are visible.
- No duplicate section silently resolves a locked uncertainty. No current fact
  correction was found that requires reopening Audit A.

## Goalpost-moving assessment

- Assertions weakened: **no textual weakening found**. The P0 inconsistencies
  above make gates non-executable, but the stated A15–A18 bars themselves remain
  strict.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. The required fix for C-2 must preserve all six A15 controls rather
  than deleting the schedule/signal cases.
- Failure renamed or relabeled as a pass: **no**, subject to correcting the
  ambiguous crash sentence in C-6.
- Check deleted instead of satisfied: **no**, but Audit B's executable
  acceptance columns have not yet been instantiated, as described in C-3.

## Verification and limits

This audit read the complete locked Audit A, Audit B, design-document protocol,
and reader-facing draft. It checked all A0–A18 rows, all P0–P4 rows, the five
mechanism rows, exact revision and coverage tables, required disclosures, and
the goalpost controls. It did not rerun primary-source discovery or product
runtime and does not substitute for the separate final trust-gate verifier or a
full implementation-level deterministic-scheduler review.

Verdict: **CHANGES REQUIRED** at the reviewed draft hash.
