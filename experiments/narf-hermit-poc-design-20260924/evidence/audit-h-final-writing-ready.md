# Audit H: exact-head final writing-readiness audit

## Verdict

**CHANGES REQUIRED**

The corrected design closes every substantive Audit C, D, E, F, and G text,
contract, acceptance, and evidence finding except one remaining error in the
architecture diagram: the Linux controller's event arrow is drawn into the
Narf-only transport/adapter instead of into the common trace Tool. The prose
and component boundaries are correct, but the sole visual still depicts an
incorrect data path. This is a narrow presentation correction; it does not
reopen Audit A or require changing the proposed architecture.

Review target:

- `design.md` SHA-256:
  `eaa877e81b067d63d0792771c6421c2bb3c4c1368a5a6757cc54b8071b818efd`;
- `audit-resolution.md` SHA-256:
  `1688ac8ca3295a8955a722ece94b7a0df69fdacb207590b60706eb8144c2b179`;
- `findings.jsonl` SHA-256:
  `8b3df1b38a8cceca4be54f1f29f4853ae72a7e5c9cf313876956cbd6b2b22bab`;
- `verification.jsonl` SHA-256:
  `61a954e5bc5770abafec3cf64fe3e3a2cb1e5ba8da0574ab5b6ae885de73f6ef`;
- `trust-gate-verifier-report.md` SHA-256:
  `6ac744430c7629ac98b22a350035ecb7dbd77032bb3929e3fc75ad51aed85b67`.

No Hermit command, Narf guest, QEMU guest, provider runtime, commit, push, or
publication occurred in this audit.

## Finding

### H-1 — Severity 2: the diagram connects the Linux controller to the Narf adapter

The corrected visual now separates Linux guest-user, Linux-kernel, external-
controller, Narf guest-user, Narf-kernel, and harness regions
(`design.md:237-259`). However, its only outgoing Linux-controller line descends
from `Reverie ptrace + Detcore` at `design.md:239-240` and terminates in the top
border of `[P] transport/reverie-narf` at `design.md:241-242`. The Narf adapter
then has the sole depicted path to the canonical trace Tool
(`design.md:243-246`). Read literally, Linux event/data enters the Narf-only
transport/adapter.

Impact: the design prose correctly specifies two independent cells feeding a
common canonicalizer, but the document's sole architecture visual contradicts
that design and assigns the Linux reference path to the wrong controller.

Required change: give `Reverie ptrace + Detcore` and `[P]
transport/reverie-narf` separate event/checked-data outputs that converge only
at `[P] canonical trace Tool`. Keep their control/disposition paths separate and
bidirectional with their respective kernels. Preserve the already-correct
guest/kernel/controller/harness boundaries and `[P]` labels.

## Requested re-checks

### Producer watermarks and snapshot membership — closed

The former host-arrival cutoff is gone. The scheduler now names the producer
and outstanding-operation set, issues a synchronous snapshot, requires a
monotonic watermark from every named producer, and closes the epoch only after
those acknowledgements (`design.md:294-312`). Lifecycle, guest-generated
signals, timers, and controlled loopback bind to guest-causal tokens; uncaused
physical inputs are excluded or classified `no_result: input_mismatch`, and a
host watchdog can produce only `infrastructure_error`. The target-state table
specifies running, tentative, queued, polling, blocked, exiting, and completed
dispositions and preserves the one-resource, tentative-pop, queue/pool,
response, re-enqueue, and time accounting (`design.md:314-349`). This closes
Audit D/F C1-C2 and Audit E E-1 without relying on repetitions to create the
membership proof.

### Exact P3 time profile — closed

P3 now fixes `poc-coop-v1`: no asynchronous PMU preemption; unsupported
CPU-bound competitors; exactly one selected task consuming one one-resource
request per semantic turn; a fixed epoch; exactly 1 ns per committed turn and
per selected task; zero time for skipped, polling, snapshot, transport, and
teardown work; exact finite-deadline fast-forward; no indefinite firing; and
clock preservation across exec (`design.md:362-384`). The precise RCB profile is
explicitly **Deferred** behind a separate reviewed specification. This is a
bounded POC profile, not a claim that the product's later precise-preemption or
continuous-time goal is already met.

### Exact A2 and A16 outcomes — closed

A2 promotion accepts only pre-characterized subcases with a frozen byte/fault
expectation and `matched`; unresolved research subcases stay `specified` and
cannot promote (`design.md:525-526`, `584`, `613`). A16a requires
`no_result:coverage` and the missing family/location, while A16b requires
`no_result:comparison_refused` and the malformed record location
(`design.md:598-599`, `627`). Generic `no_result`, semantic divergence, and
post-result characterization are forbidden. Audit E E-2/E-3 and Audit G G-1
are closed.

### Labels, terminology, platform, and disclosures — closed

- The three-public-`Backend` denominator and LiteInst/e9patch interpretations
  are explicitly **Verified current (source-only)** (`design.md:72-85`).
- Stock ptrace, USER_NOTIF, ptrace-compatible emulation, first-class in-kernel
  interposition, and framekernel/BPF-like isolation remain one comparable
  five-row decision table with the locked dispositions (`design.md:162-168`).
- `RET_TRACE` and USER_NOTIF remain distinct; BPF load/test-run is not called an
  attach point (`design.md:170-173`).
- The no-runtime statement, failed exact-commit Narf job, absence of external
  tracing/provider runs, browser-test limit, deterministic/time/replay absence,
  and SMP-stop uncertainty remain prominent (`design.md:33-44`).
- Linux x86_64, Narf x86_64, and Narf aarch64 remain separately classified by
  source versus bounded execution evidence (`design.md:681-690`).

### Interfaces, trace, trust, phases, and executable manifest — closed

The design has authoritative homes for lifecycle ownership, checked memory and
actual lengths, task lifetime, transport loss/backpressure, teardown, the
adapter conformance boundary, external-world digests, trace fields,
normalization, exact outcome classes, and security/TCB (`design.md:175-227`,
`400-519`). P0-P4 have explicit exclusions, gates, promotion/stop rules, and
deferred capabilities (`design.md:521-535`). The versioned manifest records
literal commands, exact revisions, status, repetitions, timeout, count
constraints, exact comparator result, exit disposition, durations, and
content-addressed artifacts; all A0-A18 primitives are present with qualifying
and refusing controls (`design.md:537-634`). A15 still has six mutations, A16
two exact refusal injections, A17 distinct-address-space 32-byte movement, and
A18 effective CPUID/RDTSC delivery rather than setup-call success.

### F-1 through F-33 denominator and evidence inventory — closed

The canonical stream contains 33 unique findings and the verifier contains the
same 33 unique IDs, all `[VERIFIED]`. The design has 38 citation occurrences
covering every F-1 through F-33 ID and no unknown ID. The 23-row actionable-
claim inventory covers the same complete ID union and declares the rule for any
new material current assertion. The expanded records cover the formerly missing
exact-commit runtime evidence, ptrace request inventory, generic-interposition
inverse check, BPF attach/domain breadth, I/O-buffer setting,
deterministic/replay inverse search, time/randomness inputs, and stock selective
seccomp mechanism. The independent report records 52 checked evidence items and
the exact input hashes (`trust-gate-verifier-report.md:5-40`, `88-113`).

I reran the prescribed gate against this exact design and verification stream:
`verified_pct=1.0000`, `coverage=1.0000`, `blocked=false`, no failed finding.
All local Markdown links resolve, all five JSONL evidence files parse, and the
complete experiment tree passes `git diff --check`.

### C-1 through C-8 closure — complete apart from H-1's visual flow

| Prior writing item | Exact-head result |
|---|---|
| C-1 P0 lifecycle and cleanup source | **Closed:** authoritative root start/image/exit/output/teardown, no comparator synthesis. |
| C-2 P0-compatible A1/A15 | **Closed:** deterministic P0 subcases; PID/RNG at P3; six golden-schema mutations retained. |
| C-3 executable manifest | **Closed:** schema, commands, prerequisites, exact classes/counts, timeouts, statuses, revisions, and hashes are specified. |
| C-4 deterministic P3 contract | **Closed:** watermarked causal membership, exhaustive state transitions, cooperative clock profile, liveness, signals, and assurance controls are normative. |
| C-5 diagram | **Partially closed:** proposed labels and trust/address-space boundaries are correct, but H-1 leaves the cross-cell event flow wrong. |
| C-6 crash classification | **Closed:** guest signal/core disposition is semantic; provider/kernel/controller/harness failure is infrastructure. |
| C-7 labels | **Closed:** mixed evidence is split and proposed/deferred/current labels are explicit. |
| C-8 disclosures/platform matrix | **Closed:** all mandatory uncertainties and three platform cells remain visible. |

## Goalpost-moving assessment

- Assertions weakened: **no**. A2's exemption was removed, not renamed.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. Exact A16 subclasses, A15 mutations, repetitions, state permutations,
  and semantic fields remain mandatory.
- Failure renamed or relabelled as a pass: **no**. Unsupported, all `no_result`
  subclasses, four divergence classes, and `infrastructure_error` stay distinct
  from `matched`.
- Check deleted instead of satisfied: **no**. The narrower cooperative P3
  profile is an explicit POC scope with precise RCB and independent-process
  parallelism visibly deferred; it does not claim those product capabilities
  passed or remove their future gates.

## Verification limits

This was a documentation/evidence audit. I read the complete corrected design,
resolution map, expanded actionable-claim inventory, updated trust-verifier
report, prior Audit E/G findings, and the required deterministic grounding. I
did not rerun product runtime, remote CI, public-browser checks, or the
independent literal verification. The remaining commit, staging, clean-state,
and post-commit content checks are correctly still open in the completion
checklist and are not evidence against this pre-commit design verdict.

Verdict: **CHANGES REQUIRED** at the exact hashes above, solely for H-1.
