# Audit B: writing and structure plan

## Verdict

**READY TO DRAFT**

Audit A is **LOCKED** in `audit-a-lock3.md`. This audit treats that report,
the frozen research contract, and the locked fact, matrix, and coverage files
as immutable. It changes no technical classification and resolves none of the
preserved open decisions. Its sole question is how to present the locked
material so an owner can decide whether to authorize a POC.

No Hermit command, Narf guest, QEMU guest, provider, or comparison runtime was
executed for this audit. No existing evidence file was edited.

## One-sentence answer

**Proposed:** make the smallest POC an x86_64-only, explicitly bounded
`reverie-narf` prototype whose event source is a first-class typed hook at
Narf's live syscall dispatcher and whose controller produces the same
fail-closed canonical trace as Linux/Hermit for a single-process fixture;
describe this first trace-producing stage as nonconforming until it satisfies
the full Reverie `Backend` lifecycle, mutation, instruction-event, memory,
output, and statistics obligations, and defer ptrace-compatible emulation and
a framekernel/BPF-isolated implementation to measured follow-ons.

This is a recommendation, not a current-capability claim. The draft must make
the inference explicit: stock Reverie ptrace has several independently fatal
current Narf gaps; Narf's seccomp filter/user-notification path is absent or
inert; ptrace-compatible emulation is a larger Linux-compatibility project;
and Narf's BPF ingredients have no guest-syscall attach point or trace-context
ABI. The live dispatcher is therefore the nearest current seam, but every
interface attached to it remains **Proposed**.

## Reader and decision frame

The primary reader is an owner deciding whether the smallest POC is worth
implementing and which boundary it should commit to. The opening page must let
that reader answer four questions without reading the appendices:

1. Why can stock Hermit/Reverie not run unchanged on current Narf?
2. Why is a first-class dispatcher hook the smallest recommended architecture,
   rather than USER_NOTIF, ptrace emulation, or a BPF extension?
3. What exactly would the POC prove, and what would it not prove?
4. Which executable gates must pass before the result may be called a match?

The draft must define “functional equivalence” operationally: two runs match
only when their input/provenance digests and complete coverage receipts are
admissible, every canonical observable record matches under the predeclared
normalization, final output/disposition matches, and no refusal or
infrastructure class applies. It must not define equivalence as two zero exit
codes, equal returns without moved bytes and persistent effects, or successful
record/replay of one recording.

## Proposed reader-facing architecture

Use this section order. The “authoritative home” column is a drafting rule:
other sections may link back or give a one-line consequence, but must not
repeat the claim or introduce a second, slightly different formulation.

| Order | Reader-facing section | Purpose | Authoritative claim home |
|---:|---|---|---|
| 0 | Title, status, decision banner | State that this is a source-backed POC design, not implemented support; give exact evidence date and revisions. | Evidence boundary, claim-label legend, and no-runtime disclosure. |
| 1 | Executive decision | Give the one-sentence recommendation, smallest proof boundary, three reasons, success definition, and explicit non-goals. | Recommendation and the bounded meaning of “POC match.” |
| 2 | Current facts that force the decision | Explain the actual Reverie contract, stock ptrace dependencies, current Narf intersection, and current Narf dispatcher/BPF/domain ingredients. | Current compatibility blockers and the distinction between source-only and executed evidence. |
| 3 | Interposition choices | Compare the five required mechanisms under one fixed criterion set. | All “why viable/not viable now” judgments. No second provider comparison later. |
| 4 | Proposed `reverie-narf` architecture | Define components, boundary ownership, event/lifecycle flow, memory/data movement, blocking/reentrancy, teardown, output/stats, and the conformance boundary. | Proposed interfaces and control/data flow. |
| 5 | Equivalence experiment | Define paired Linux/Narf fixtures, boot and executable prerequisites, controlled inputs, observation envelope, normalization, and coverage receipt. | Canonical trace semantics and harness obligations. |
| 6 | Failure model and security | Define admissibility/refusal/divergence/infrastructure outcomes, authorization, memory authority, containment, and TCB. | Outcome taxonomy and trust boundaries. |
| 7 | Phases and executable acceptance | Give the smallest phase, follow-ons, entry/exit criteria, A0–A18 mapping, and negative controls. | All implementation staging and falsifiable gates. |
| 8 | Risks, unresolved decisions, and stop conditions | Preserve every locked uncertainty and state which phase it blocks. | Open questions; no other section may silently decide them. |
| 9 | Evidence appendix | Exact revisions, provider inventory, complete protocol/trace/test tables, source coverage, exclusions, contradictions, and runtime/source reconciliation. | Evidence detail and provenance. |

### Current-to-new mapping

The draft is new synthesis, not a line-by-line rewrite of an earlier report.
Map the locked artifacts as follows:

| Locked input | Reader-facing destination | Drafting treatment |
|---|---|---|
| `audit-a-lock3.md` | Sections 0–3, 8, and revision appendix | Canonical classifications and immutable cautions. Do not copy the audit process narrative. |
| `reverie-facts.md` | Sections 2 and 4; provider appendix | Explain `Backend`/`Tool`/`Guest` obligations and the exact ptrace state machine. |
| `reverie-backend-matrix.md` | Section 3 and provider appendix | Condense to the five decision mechanisms in the main body; retain the complete eleven-row provider inventory in the appendix. |
| `narf-facts.md` | Sections 2, 4, 5, 6, and 8 | Current dispatcher, ptrace/seccomp/BPF/domain, loader, scheduler/input, security, CI, and open facts. |
| `hermit-facts.md` | Sections 2, 5, 6, and 7 | Active Hermit path, strict observation discipline, input controls, verdicts, and acceptance implications. |
| `trace-input-matrix.md` | Sections 5–7 and complete appendix tables | Turn proposed rows into the trace contract and executable gates without calling them implemented. |
| Three source-coverage ledgers | Evidence appendix | Summarize query/document coverage and link to full ledgers; keep detailed heading and exclusion inventories out of the decision path. |
| Preliminary finding JSONL/schema notes | Evidence appendix only | Research provenance only. Never cite namespaced lane IDs as final trust-gate verification. |
| Historical failed audits | Evidence appendix correction history | One compact ledger showing that the current verdict supersedes them; do not narrate every correction in the main body. |

## Authoritative homes for load-bearing claims

The following claims are sufficiently important that each needs exactly one
authoritative paragraph or table row in the final design.

| Load-bearing claim | Authoritative home | Required label/control |
|---|---|---|
| Reverie's generic contract is broader than syscall callbacks. | Section 2, “Reverie conformance boundary.” | **Verified current**; name `Backend`, `Tool`, `Guest`, `GlobalTool`, `Subscription`, and `MemoryAccess`. |
| Exactly three public Reverie `Backend` implementations exist at the standalone pin. | Provider appendix. | **Verified current**; denominator is production `impl Backend for` blocks. |
| Stock ptrace is not viable unchanged on current Narf. | Section 3 decision matrix, stock-ptrace row. | **Verified current (source-only intersection)**; enumerate fatal event/message, signal, distinct-AS memory, CPUID/RDTSC, and cleanup gaps. |
| Narf does implement real ptrace and has executed x86_64 basic fixtures. | Section 2, “What current Narf does have.” | **Verified current**; distinguish executed fixture subset from the later failed QEMU job and absent external tracer. |
| Linux USER_NOTIF semantics and current Narf absence/inertness. | Section 3, USER_NOTIF row. | Linux behavior **Verified current** from pinned man-pages; Narf negative **Verified current (source-only)**; proposed future semantics remain **Unresolved**. |
| The dispatcher hook is the nearest current seam, not a current interface. | Section 3 decision rationale. | Current dispatcher **Verified current**; typed hook and controller **Proposed**. |
| BPF/domain ingredients exist but no syscall attach/context ABI does. | Section 3 BPF row and Section 6 trust boundary. | **Verified current** for ingredients/absence; isolated extension **Proposed**; domains are defense in depth. |
| The new path is not conforming merely because it emits syscall events. | Section 4 conformance boundary. | **Proposed** gate against the locked Reverie obligations; fail closed on absent lifecycle, mutation, output, stats, or subscribed events. |
| Trace equality includes bytes, effects, lifecycle, order, and coverage. | Section 5 canonical trace contract. | **Proposed**; tie each family to exact comparison/refusal rules. |
| Fixture identity and external-world equivalence are harness duties. | Section 5 inputs/prerequisites. | **Above the API**; executable, interpreter/dependencies, argv/env/cwd, filesystem, seeds, and network fixture digests. |
| Record/replay fidelity is not independent cross-kernel equivalence. | Section 5, one caution box. | **Verified current**; do not repeat elsewhere. |
| Only complete matched traces support the POC claim. | Section 6 outcome table. | **Proposed**; `unsupported`, `no_result`, and `infrastructure_error` are never matches. |
| The first stage is intentionally narrower than a conforming backend. | Section 7 phase table. | **Proposed**; state exact excluded guest features and never present that scope as generic Hermit support. |
| No Hermit-on-Narf runtime was executed. | Status banner and Section 8. | Mandatory disclosure; repeat only in the final conclusion because it directly constrains the recommendation. |

## Required mechanism comparison

The main body needs exactly one decision table with these columns:

| Column | Required content |
|---|---|
| Mechanism | Stock ptrace; Linux seccomp user notification; ptrace-compatible emulation; first-class in-kernel Rust interposition; framekernel/BPF-like isolated extension. |
| Current Narf evidence | What exists and what is absent/inert at the pinned revision; source-only versus executed must be visible in the cell. |
| Reverie/Hermit fit | Which generic obligations it naturally supplies and which remain outside it. |
| Required implementation | Kernel, Reverie, controller, memory, lifecycle, instruction-event, and containment work. |
| Smallest-POC judgment | `not viable unchanged`, `not present`, `larger compatibility project`, `recommended proposed seam`, or `follow-on proposed isolation`. |
| Falsifier | The first acceptance primitive or negative control that would reject the judgment. |

The table's five locked dispositions are:

1. **Stock ptrace — not viable unchanged.** Do not lead with absent SEIZE or
   LISTEN; lead with the actual fatal stock dependencies: seccomp/lifecycle
   events and `GETEVENTMSG`, `GETSIGINFO`, distinct-address-space
   `process_vm_*`, default CPUID/RDTSC delivery, and cleanup semantics.
2. **Linux seccomp/user notification — not present on current Narf and not a
   complete Reverie backend even on Linux.** Explain restart/cancellation,
   memory TOCTOU, listener lifetime, register, signal, instruction, and
   lifecycle gaps. Do not infer a Narf notifier from mode/action constants.
3. **Ptrace-compatible emulation — technically proposed but not the smallest
   POC.** Present it as a large compatibility project whose benefit is reuse of
   stock Reverie, not as a small wrapper around basic syscall stops.
4. **First-class in-kernel Rust interposition — recommended proposed seam.**
   Its advantage is placement at the live dispatcher; its obligations include
   ownership, authorization, event order, blocking/reentrancy, task lifetime,
   bounded data movement, mutation semantics, cleanup, and the entire
   non-syscall `Backend` surface.
5. **Framekernel/BPF-like isolated extension — proposed follow-on.** Existing
   verifier/JIT/maps/fd/domain ingredients reduce some future work, but a
   syscall attach target, typed context, bounded copy helpers, lifecycle, and
   export path do not exist. Hardware domains must not be presented as the
   primary safety proof.

Do not add KVM, DBT, preload, LiteInst, e9patch, or SaBRe as extra candidates in
this main decision table. They belong in the complete provider appendix and
may be cited in one paragraph as lessons about placement, coverage, and trust.

## Proposed architecture section design

### Components and boundaries

Use one component table with these exact rows:

| Component | Placement | Proposed responsibility | Must not be inferred |
|---|---|---|---|
| Narf dispatcher event source | Narf syscall entry/exit seam | Produce typed task/syscall phase events and accept a bounded disposition. | That a generic trait already exists. |
| Narf task/lifecycle source | Narf process/signal/scheduler seams | Produce authoritative create/exec/exit, block/wake, signal, and teardown events as phases require. | That syscall callbacks alone satisfy `Backend`. |
| Memory/data-movement API | Kernel-mediated, task-scoped | Scalar metadata plus checked bounded copy-in/copy-out and explicit transferred lengths/errors. | Raw guest pointers are safe capabilities or current `process_vm_*` suffices. |
| Controller transport | Proposed kernel/userspace boundary | Preserve per-task ordering, backpressure, cancellation, loss accounting, and teardown; export coverage receipts. | That a ring, fd, RPC, or blocking callback is already chosen. |
| `reverie-narf` adapter | Controller side | Implement the growing `Backend`/`Guest` surface and drive an arbitrary `Tool` only at the conformance phase. | That a trace-only phase is a conforming backend. |
| Cross-kernel trace tool | Above backend API | Turn events and observations into the versioned canonical trace and coverage receipt. | That generic Reverie defines canonical ordering or normalization. |
| Paired-run harness/comparator | Above the API | Stage identical inputs, run Linux and Narf cells, validate receipts, normalize only declared fields, and classify outcomes. | That the backend creates equivalent filesystems, clocks, or networks. |

Do not invent a final Rust signature in the main body. Specify a semantic
contract first: event phase, stable task/lifetime handle, ABI and six scalar
arguments, register snapshot/mutation availability, checked memory operation,
return/errno/disposition, logical operation identity for restart, sequence
number, cancellation, and teardown. Any illustrative trait must be labeled
**Proposed sketch**, must not use borrowed guest pointers across await, and
must leave transport and guest-address normalization visibly unresolved.

### Lifecycle and ordering narrative

Use one numbered event sequence, not prose repeated in several sections:

1. Harness verifies image/input digests and starts a bounded Narf guest.
2. Kernel assigns an authoritative task/lifetime identity and emits start/exec
   provenance before the first subscribed semantic event.
3. Entry event carries scalar metadata and permits only checked task-scoped
   reads; controller returns execute, emulate, replace/inject, or fail.
4. Completion event carries exact return/errno, transferred bytes, side-effect
   evidence, restart identity, and ordering position.
5. Blocking, wake, signal, fork/clone/vfork/exec, instruction, timer, and exit
   events enter as each phase claims them; missing claimed families refuse the
   comparison.
6. Teardown revokes task/memory authority, drains or marks lost records, emits
   the final coverage receipt, and proves no escaped task or zombie.

The draft must state that steps 2–6 are **Proposed**. Where the smallest phase
does not yet implement a step, its guest scope must exclude the feature and its
coverage receipt must advertise that reduced scope; it may not silently emit
zero events.

## Trace and experiment table designs

### Main-body trace contract table

Condense the locked observation envelope into these rows:

| Family | Required fields | Equality rule | Refusal condition | Acceptance IDs |
|---|---|---|---|---|
| Provenance and inputs | Revisions, architecture, image/dependency/fixture/config digests | Inputs/config exact; provider/revision retained as provenance | Digest or schema mismatch | A0, A13, A16 |
| Coverage | Subscriptions, enablement proofs, counts, memory transport, bytes/hashes, loss/truncation | Both sides cover every claimed family | Missing family, native subscribed instruction, unexplained zero work, truncation | A16–A18 |
| Task/lifecycle/order | Logical identities, parentage, exec generation, phase, event ordinal, scheduler turn | Exact provider-neutral tree and total order | Missing or ambiguous create/exec/exit mapping | A0, A4–A6, A9 |
| Syscall and memory | Number, six args, input bytes, return/errno, partial counts, mutable bytes, register effects | Semantic scalars and actual bytes exact | Pointer substituted for data; moved length unavailable | A1–A3, A17 |
| Persistent state | fd/open-file/path/map/socket/process/signal/timer transitions plus follow-up observation | Transition and later observation exact | Success inferred without observation | A3, A6, A11, A12 |
| Signals, time, hardware | Queue/provenance/mask/restart; virtual clocks/timers; CPUID/RDTSC events and selected values | Exact declared Linux personality | Provenance absent; subscribed instruction runs natively | A7–A10, A18 |
| Filesystem/network/platform | Guest namespace paths/bytes/metadata/order; controlled packet/readiness; uname/auxv/proc/vDSO | Exact within declared fixture/personality | External/uncontrolled input or unadvertised capability difference | A11–A13 |
| Final observation | stdout/stderr bytes, lengths and digests; exit/signal/core; cleanup | Exact | Output missing or task escaped | A0, A14–A16 |

The normalization rules must immediately follow this table. Keep host/controller
address ordinalization, logical task IDs, guest byte paths, actual buffer
lengths, and fail-closed ordering. Preserve guest-address equality versus a
mapping bijection and PID/fd/inode/port renaming as **Unresolved**; the smallest
POC should either require exact values or choose fixtures that avoid needing a
relaxation. The document must not silently make either choice.

### Outcome table

Use the locked classes without merging them:

| Outcome family | Required names |
|---|---|
| Success | `matched` only after complete admissible receipts and exact canonical records/final outputs. |
| Semantic divergence | `diverged_clock`, `diverged_control`, `diverged_observation`, `diverged_state`. |
| Capability/refusal | `unsupported`, `no_result: coverage`, `no_result: comparison_refused`, `no_result: input_mismatch`. |
| System failure | `infrastructure_error`, with provider and stage; includes panic, kernel fault, deadlock, lost task, escaped task, zombie, or cleanup failure. |

Guest exit status is a trace field, not the comparison verdict. Never relabel
unsupported or a crash as divergence, and never relabel divergence as an
infrastructure-only result merely because both programs exit.

### Phase table

The staging table needs these columns: phase; claimed scope; implemented
interfaces; explicit exclusions; executable gates; promotion rule; rollback or
stop condition. Use this phase shape:

| Phase | Claim and boundary | Mandatory gates before promotion |
|---|---|---|
| P0: trace plumbing | **Proposed, nonconforming:** x86_64, one process/thread, controlled static fixture, dispatcher entry/completion, checked buffers, exact output/exit and coverage receipt. No claim of generic Hermit support. | A0, A1, the applicable memory subset of A2, A15, A16; paired Linux/Narf artifacts and negative controls. |
| P1: mutation and backend skeleton | **Proposed:** suppression/emulation/injection, registers, task-scoped memory, real stats/output; begin `Guest` operations. | A2, A3, A17 plus Tool-error cleanup. |
| P2: lifecycle and signals | **Proposed:** complete fork/clone/vfork/exec/exit identities, signal provenance/restart, containment; only here approach generic process-tree obligations. | A4–A8, A14. |
| P3: deterministic inputs | **Proposed:** scheduler events/control, PMU or replacement contract, virtual time, CPUID/RDTSC, randomness, filesystem/personality; controlled loopback last. | A9–A13, A18; rerun A15/A16. |
| P4: isolation and compatibility follow-ons | **Optional capability / Deferred until measured:** BPF attach/context/copy ABI and verified isolation; or ptrace-compatible emulation for stock-backend reuse. | Full A0–A18 matrix, hostile-extension tests, actual domain-backend proof, failure injection, and no regression to receipt coverage. |

P0's exact static-loader assumptions and guest-address rule remain design
decisions to fix before implementation; the table describes a recommended
scope, not current Narf execution. If the owner requires “Hermit backend” to
mean a conforming arbitrary-Tool path from the first commit, P0 and P1 must be
renamed software helpers and the first backend milestone moves to the phase
that satisfies every generic obligation.

### Acceptance appendix

Retain A0–A18 as nineteen separate rows. Add these columns to the locked table:

- Linux command/artifact placeholder;
- Narf command/artifact placeholder;
- prerequisite capability and phase;
- expected canonical record prefix/count;
- comparator outcome on the positive pair;
- required negative mutation or failure injection;
- evidence status (`specified`, `executed Linux`, `executed Narf`, `paired`);
- duration, exit status, revisions, and artifact hashes.

Do not convert the appendix to a checklist of exit codes. A15 must remain six
separate mutations (return, errno, buffer, schedule, signal, output), A16 must
retain both event-family removal and truncation, A17 must prove actual
distinct-address-space 32-byte movement, and A18 must prove that subscribed
RDTSC and CPUID did not execute natively.

## Diagram decision

**Create one new diagram; delete/avoid all decorative alternatives.** There is
no reader-facing diagram to keep. The useful visual is a two-column execution
and comparison boundary:

```text
Linux reference                                      Narf candidate
guest X -> Hermit/Detcore -> Reverie ptrace           same guest X -> Narf syscall/lifecycle seams
             | complete event + coverage                          | proposed typed event + coverage
             +------------------- canonicalizer ------------------+
                                      |
                         fail-closed trace comparator
                                      |
             matched | semantic divergence | no-result | infrastructure error
```

Redraw it for the final document with these acceptance criteria:

1. Visually distinguish **Verified current** Linux/Reverie paths from every
   **Proposed** Narf component; a solid identical style is forbidden.
2. Show guest/kernel/controller/harness trust and address-space boundaries.
3. Draw control/disposition separately from event/data flow and show bounded
   copy operations rather than a raw memory arrow.
4. Show fixture digests entering both sides and coverage receipts entering the
   comparator.
5. End in the four outcome families, not a generic green “success” box.
6. Do not show BPF or hardware domains on the P0 path; show them only as a
   dashed optional P4 enclosure if needed.
7. Do not imply that the Narf controller transport, trait signature, scheduler
   control, or isolation backend has already been selected or implemented.

No sequence diagram is needed if the six-step lifecycle sequence is present.
No provider-zoo diagram is needed because the decision table communicates that
relationship more precisely.

## Main body versus appendix

### Main body

Keep only material that changes the architecture decision or the POC claim:

- recommendation and evidence boundary;
- generic Reverie obligations and fatal current Narf intersection;
- five-mechanism decision matrix;
- proposed components and lifecycle/data-movement boundary;
- controlled boot/executable/input prerequisites;
- canonical trace and normalization rules;
- outcomes, security/TCB, phases, promotion gates, and top unresolved decisions;
- one architecture diagram.

### Appendices

Move detail needed for auditability but not first-pass judgment:

- exact source revisions and link index;
- complete eleven-row provider inventory and execution/data/trust matrices;
- full stock ptrace request/option/event intersection;
- full trace-field/normalization matrix;
- full A0–A18 acceptance matrix and artifact schema;
- source-coverage/query-pair/document-class/heading/exclusion/inverse-check
  ledgers by reference;
- contradiction/correction ledger and runtime/source reconciliation;
- canonical trust-gate findings and verification results when produced.

Do not duplicate appendix tables in abbreviated but differently worded forms.
The main decision and trace tables may be projections only if they link to the
one complete appendix table and preserve row identifiers.

## Word and density budget

Target 6,000–7,500 words in the reader-facing design, excluding machine-readable
findings and the already separate source-coverage ledgers.

| Section | Budget | Density rule |
|---|---:|---|
| Status + executive decision | 450–650 words | One answer, three reasons, one success definition, one no-runtime disclosure. |
| Current forcing facts | 800–1,000 | At most two compact tables; no audit chronology. |
| Five-mechanism comparison | 700–900 | One six-column decision table plus at most 250 words of interpretation. |
| Proposed architecture | 1,200–1,500 | One component table, one diagram, one six-step event sequence; semantic contract before any trait sketch. |
| Experiment and trace | 1,200–1,500 | One trace projection, normalization list, boot/input prerequisites. |
| Failure/security | 600–800 | One outcome table and one trust-boundary table. |
| Phases/tests | 850–1,100 | Phase table plus promotion/stop rules; A0–A18 detail in appendix. |
| Risks/conclusion | 350–500 | Ranked open decisions, no repetition of the executive summary. |
| Evidence appendix | 1,500–2,500 | Tables and links dominate prose; reference full existing ledgers rather than reproducing their headings. |

Paragraphs should average no more than five sentences. Every main-body table
needs a preceding sentence saying the decision it supports and a following
sentence stating the conclusion; otherwise move it to the appendix. Avoid a
long “background” section: current facts belong immediately before the choice
they constrain.

## Terminology and claim-label controls

1. Prefix every material current/proposed assertion or table cell with one of
   the frozen labels: **Verified current**, **Proposed**, **Optional
   capability**, **Software helper**, **Above the API**, **Deferred**, or
   **Unresolved**. “Verified current” must add `(source-only)` or `(executed)`
   where the distinction matters.
2. Reserve “public Reverie `Backend` implementation” for trait implementation
   status. Under Hermit's narrower local terminology, e9patch is preprocessing
   plus ptrace, not a separate Hermit backend. Never write unqualified “e9patch
   backend” in a Hermit-path claim.
3. Always distinguish **LiteInst trait/direct** (`Backend::run*` ->
   `run_with_preload*`) from the separate **LiteInst ptrace-host hybrid**
   (`run_host_with_preload*`) that Hermit selects. Never imply the public trait
   path is ptrace-owned.
4. State both halves of the Narf ptrace result: it is real and has limited
   executed x86_64 fixtures; it is not compatible with stock Reverie ptrace.
5. Call seccomp `RET_TRACE` and seccomp `USER_NOTIF` different mechanisms.
   Narf's stored mode/action recognition is not filter execution.
6. A `BPF_PROG_TYPE_SYSCALL` load/test-run path is not a syscall attach point.
   Domain execution is not proof of complete sandboxing.
7. Use “first-class in-kernel Rust interposition” only for the **Proposed**
   interface. The existing concrete ptrace calls are a seam, not that trait.
8. Use “canonical observable trace” for the proposed cross-kernel artifact;
   use “strict Hermit trace discipline” as its starting evidence, not as an
   already cross-provider format.
9. Use `matched`, the four divergence classes, refusal/capability outcomes, and
   `infrastructure_error` exactly. Avoid “pass” for a guest exit or one test
   declaration.
10. Do not use provider percentages or “most backends.” Use the locked
    denominators: three public trait implementations, eleven provider/mechanism
    rows, and nineteen proposed A0–A18 primitives.

## Required uncertainty and no-runtime disclosures

The final design must have a visibly boxed evidence-boundary paragraph near the
top and an unresolved-decision table near the end. Together they must say:

- no Hermit-on-Narf backend exists or was run;
- no external Hermit, strace, or gdb tracer ran against Narf;
- no Reverie provider runtime test ran in this research;
- exact-commit Narf x86_64 QEMU fixtures demonstrated a limited ptrace subset,
  but the job later failed when QEMU exited with SIGSEGV after 457 seconds;
  other pinned jobs also failed, so the commit was not globally green;
- the evidence is source- and captured-CI-bound, and public links were not
  interactively browser-tested;
- x86_64 is the only defensible first POC target from current syscall-stop and
  instruction-event evidence; arbitrary aarch64 guest parity is unresolved;
- `PTRACE_SINGLESTEP`, restart/wait order, realtime signal reinjection,
  FP/XSTATE, ptracer-death cleanup, and SMP stopped-task synchronization remain
  unproved on Narf;
- exact paired Linux/Narf page- and segment-fault outcomes remain unresolved
  even though Narf's same-AS gather/scatter behavior and distinct-AS `EPERM`
  are known;
- controller transport, event total order, task/lifetime ownership, blocking
  and reentrancy, bounded memory authority, teardown, and production
  authorization are proposed design obligations, not selected current APIs;
- guest-address equality versus a mapping bijection and later PID/fd/inode/port
  renaming must be fixed before execution;
- the boot image has not staged and run the byte-identical executable,
  interpreter, dependencies, argv/environment, filesystem, and network fixture
  on both systems;
- the POC domain backend and actual hardware enforcement are unspecified and
  unexecuted; FRAME/domain 0 remains in the trusted boundary;
- current Narf has no Hermit-style deterministic scheduler, virtual-time, or
  record/replay capture implementation; P3 must implement or explicitly bound
  the relevant behavior before making multithreaded/time-sensitive claims;
- uncontrolled external networking, performance equivalence, arbitrary
  identity/address renaming, and production hardening are deferred.

Every unresolved item needs a “blocks phase/test” column. This prevents a broad
unknown, such as signal provenance, from blocking the trace-only P0 while also
preventing P2/P3 from silently proceeding without it.

## Security and trust presentation

Use one trust table with columns: actor/component; authority; protected assets;
current enforcement; proposed additional enforcement; failure effect; required
test. It must include the guest, Narf dispatcher/uaccess, controller transport,
`reverie-narf`/Tool, comparator/harness, BPF verifier/JIT/helpers, domain
manager/backend, and trace artifacts.

The security conclusion must be modest:

- ptrace is a privileged control surface, not a sandbox;
- an in-kernel interposer with tracee-memory authority joins uaccess/memory
  management in the TCB;
- current BPF credentials are the actual load-authority gate and the bootstrap
  capability is not proof of per-task authorization;
- the verifier/interpreter/JIT/helper/dispatcher chain is primary for a BPF
  extension, while hardware domains are conditional defense in depth;
- P0 may run trusted test fixtures, but that restriction must be an explicit
  security assumption and cannot be carried into a hostile-guest claim.

## Section review order

Review in dependency order, which differs slightly from reading order:

1. Evidence banner, exact revisions, labels, and “no runtime” statement.
2. Definition of match, refusal, divergence, and infrastructure failure.
3. Five-mechanism decision table and one-sentence recommendation.
4. `reverie-narf` semantic interface and conformance boundary.
5. Trace fields, normalization, controlled inputs, and coverage receipt.
6. P0–P4 scopes and promotion/stop rules.
7. A0–A18 tests, especially A15–A18 goalpost controls.
8. Security/TCB and unresolved-decision blockers.
9. Diagram against the acceptance checklist.
10. Provider/source/revision appendices and final claim-to-source/trust-gate
    verification.

Do not review prose polish before items 1–7 agree. Any fact correction found
during drafting reopens Audit A; Audit B may reorganize but must not repair a
technical claim by silently changing its label or meaning.

## Draft acceptance checklist

The draft is ready for its later writing/correctness review only if all of the
following are true:

- the recommendation is labeled **Proposed** and P0 is explicitly
  nonconforming;
- the five required interposition mechanisms appear in one comparable table;
- stock ptrace's fatal gaps are complete and avoidable requests are not used as
  the rationale;
- LiteInst direct/hybrid and e9patch Reverie/Hermit terminology are correct;
- interfaces, lifecycle, memory movement, boot/inputs, scheduling/time,
  trace semantics, failure classes, security, phases, and A0–A18 are covered;
- every match requires complete coverage receipts and exact output/disposition;
- A15/A16 cannot be weakened, and A17/A18 retain the known stock prerequisites;
- proposed interfaces are not described in present tense as current Narf or
  current Hermit support;
- all preserved uncertainties and the failed pinned Narf jobs remain visible;
- the complete evidence appendix names all exact revisions and uses
  commit-pinned public links where available;
- final reader-facing claims use a new canonical `F-N` stream and independent
  verification, never the preliminary `N-F`, `R-F`, or `H-F` IDs;
- no publication, push, issue, pull request, or comment occurs.

Verdict: **READY TO DRAFT** from the locked fact set under this structure.
