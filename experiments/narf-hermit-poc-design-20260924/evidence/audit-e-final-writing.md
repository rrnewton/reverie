# Audit E: final post-revision writing audit

## Verdict

**CHANGES REQUIRED**

The revision closes most of Audit C and Audit D, and it preserves the locked
current-state facts. It is not ready for owner review because the proposed
P2/P3 admission rule still lets host callback timing choose drain membership,
two acceptance commands admit weaker or non-taxonomic outcomes, and the sole
architecture diagram places host and kernel components inside guest address
spaces. These are design/evidence defects, not new current-state fact disputes;
Audit A need not reopen if they are corrected without changing a locked fact.

Review target:

- `design.md` SHA-256:
  `3c43221354848fdcc3df17fdbc6b56b80228057dd94615c1a7a776bf529f5a61`;
- locked Audit A SHA-256:
  `f7d7396f098949f7f2b790ec5c59b4b9dc7507bc6fab4eb11eef7ebdce908c3b`;
- Audit B SHA-256:
  `9c7ad966ea3b5c3cf0fca39d2729207c7a06b3ead2dedec6f20f5641c1868ceb`;
- Audit C SHA-256:
  `8d0707a534bbc55f8f2b34f54a60f384d38e3b022a72b4d5f281aff72fc857b0`;
- deterministic-scheduling Audit D SHA-256:
  `4d47aeb220aabd9cdfd18b5af2e279d04fdf199192b99ef21812fb2894842e5e`;
- independent verification SHA-256:
  `1e97cca8f06e298d90108f5c8a7c35539dbfea7f74c400dcaa00dfe8b60c38c1`.

No Hermit command, Narf guest, QEMU guest, provider runtime, commit, push, or
publication occurred in this audit.

## Findings

### E-1 — Severity 1: the new epoch cutoff does not determinize epoch membership

The new scheduling section correctly says that sorting a host-selected set is
insufficient (`design.md:289-296`), but the mechanism immediately does that at
an epoch boundary: an event is admitted to the current epoch if it arrives
before the drain cutoff and to the next epoch if it arrives afterward
(`design.md:291-294`). Neither a stable producer identity nor a causal-epoch tag
defines how an asynchronous physical completion is assigned to an epoch
independently of callback/transport timing.

The same defect appears in the normative rows. Signal eligibility starts from a
physical arrival pending at a drain (`design.md:313`), while external I/O enters
the "next open drain" after request publication (`design.md:315`). Ordering by a
stable operation ID only orders the members that happened to reach one frozen
set; it cannot make membership identical when the same completion straddles a
cutoff. The four liveness cases (`design.md:318-322`) and 100 repetitions
(`design.md:489`, `552-567`) may expose this race, but testing cannot supply the
missing rule. Record/replay is explicitly deferred (`design.md:347-353`), so it
does not bound the nondeterminism either.

Impact: P2/P3 can still produce different committed schedules from identical
declared inputs. The prose's claim that the barrier prevents host timing from
deciding membership is unsupported by the mechanism it specifies. This leaves
Audit C C-4 and deterministic Audit D C1-C3 only partially closed.

Required change: define epoch assignment from guest-causal state, not physical
arrival time. For example, bind guest lifecycle and guest-generated signals to
the committed source operation, timers to logical deadlines, and external I/O
to a versioned deterministic polling/harvest point over an already fixed set of
published operations; reject or explicitly capture external signals/packets.
For every asynchronous family, state what happens when physical arrival occurs
on either side of publication, tentative selection, the eligibility barrier,
completion, and exit, while requiring the same committed epoch. Retain the
arrival-edge and barrier-removal negative controls.

### E-2 — Severity 1: A16 relaxes two exact refusal classes into generic `no_result`

The outcome taxonomy deliberately distinguishes `no_result: coverage` from
`no_result: comparison_refused` (`design.md:446-450`). The executable A16 row
instead invokes `compare --expect=no_result` for both removing an event family
and truncating a trace (`design.md:559`), and the descriptive row likewise asks
only that both trials "refuse" (`design.md:587`). This accepts the wrong refusal
reason and therefore does not prove that missing coverage and malformed or
truncated evidence travel through their intended gates.

Impact: a comparator that misclassifies every A16 failure could satisfy the
manifest. That is a comparator relaxation and leaves Audit C C-3 partially
open.

Required change: split A16's command/control into two exact invocations. Event-
family removal must expect `no_result: coverage`; truncation must expect
`no_result: comparison_refused`. Require the first missing family or malformed
record location in the artifact. Use the same exact-class rule for the generic
"refusal" shorthand in A0 (`design.md:543`).

### E-3 — Severity 1: A2 admits `declared unresolved` as if it were a comparator outcome

The executable A2 row permits `compare --expect=matched` **or declared
unresolved** (`design.md:545`). `declared unresolved` is not one of the outcome
classes at `design.md:439-450`, and the fuller A2 row still says the pair must
agree while acknowledging that the native Linux boundary is unresolved
(`design.md:573`). P0 and P1 nevertheless list applicable A2 work as a promotion
gate (`design.md:486-487`).

Impact: an implementation can claim the gate without a frozen expected outcome
or an exact comparator verdict. This is an exemption in an otherwise executable
manifest.

Required change: split A2 into characterized subcases whose positive pair must
be `matched` and research subcases that remain `specified` and cannot satisfy a
promotion gate. If an unresolved subcase is executed for discovery, give it an
exact refusal/research-artifact status outside promotion; never accept
"unresolved" as a comparator result after observing the output.

### E-4 — Severity 2: the diagram's address-space and trust boundaries are wrong

The caption says the horizontal boxes mark guest address spaces
(`design.md:255-256`), but the Linux guest box contains `Detcore -> Reverie
ptrace`, and the Narf guest box contains the Narf dispatcher
(`design.md:236-238`). Detcore/Reverie ptrace are external controller/tracer
components; the Narf dispatcher is kernel code. Neither belongs inside a guest
user address space. This leaves Audit C C-5 open despite correctly marking the
canonicalizer, transport, adapter, and comparator as proposed.

Impact: the only architecture visual gives the owner the wrong fault,
authority, and memory-access boundaries.

Required change: redraw the two cells with separate guest-user, kernel, and
external-controller regions: Linux guest -> Linux kernel ptrace/seccomp ->
external Reverie/Detcore; Narf guest -> Narf kernel dispatcher/uaccess ->
proposed transport/`reverie-narf`. Keep the harness/comparator outside both,
and retain separate event/data versus control/disposition arrows.

### E-5 — Severity 2: one material current assertion still lacks its frozen label

The statement that the standalone Reverie pin has exactly three production
public `Backend` implementations is a load-bearing current fact, but it is
unlabelled at `design.md:72-75`. Audit C C-7 specifically required this count to
be visibly separated as **Verified current (source-only)**. The adjacent
LiteInst/e9patch interpretation at `design.md:77-83` is also current-source
material and should remain under that explicit label.

Required change: prefix that paragraph (or its two factual paragraphs) with
**Verified current (source-only)**. Do not change the denominator or the
LiteInst/e9patch terminology.

## Audit C closure, C-1 through C-8

| Prior item | Result at reviewed hash | Evidence |
|---|---|---|
| C-1: P0 lifecycle/cleanup source | **Closed** | P0 has an authoritative root start/initial-image/exit/teardown owner, ordered around dispatcher events, including output closure and no-leak evidence (`design.md:181`, `265-285`, `486`). |
| C-2: phase-incompatible A1/A15 | **Closed** | P0 A1 is now invalid-syscall plus deterministic-buffer evidence; PID/RNG move to P3, while all six A15 mutations remain in a phase-independent golden corpus and live claimed-family tests (`design.md:486`, `544`, `572`, `586`). |
| C-3: executable acceptance manifest | **Partially closed** | The versioned schema, commands, artifacts, counts, timeouts, revisions, and per-case table exist (`design.md:498-567`), but E-2/E-3 leave exact comparator outcomes weaker or undefined. |
| C-4: deterministic P3 contract | **Partially closed** | Transition, signal, preemption, virtual-time, liveness, repetition, replay, and parallelism prose was added (`design.md:287-359`), but E-1 leaves membership host-timed. |
| C-5: diagram labels/boundaries | **Open** | Proposed components are marked, but E-4 shows incorrect address-space placement. |
| C-6: guest versus infrastructure crash | **Closed** | Guest signal/core disposition may match or diverge; provider/kernel/controller/harness crashes are `infrastructure_error` (`design.md:452-456`). |
| C-7: frozen labels | **Partially closed** | P4 is now **Proposed / Deferred**, mixed stock-ptrace evidence is split, and the loader inference is **Proposed**; E-5 remains. |
| C-8: disclosures/platform matrix | **Closed** | Browser-testing, Narf record/replay absence, SMP-stop uncertainty, and Linux/Narf x86_64/aarch64 platform rows are explicit (`design.md:33-44`, `641-650`). |

## Required-scope checks

- **Five mechanisms:** complete in one table: stock ptrace, Linux seccomp user
  notification, ptrace-compatible emulation, first-class in-kernel Rust
  interposition, and framekernel/BPF-like isolation (`design.md:161-167`). The
  viability judgments match the locked fact set.
- **Terminology:** LiteInst trait/direct versus ptrace-host hybrid and e9patch's
  Reverie-trait versus Hermit-product meaning are correct (`design.md:77-83`).
  `RET_TRACE`, USER_NOTIF, and `BPF_PROG_TYPE_SYSCALL` attachment are not
  conflated (`design.md:169-172`).
- **No-runtime/platform boundary:** complete and prominent (`design.md:33-44`,
  `641-650`). No unexecuted Narf capability is presented as runtime-proven.
- **Interfaces, data movement, lifecycle, trace, inputs, outcomes, and
  security:** all have authoritative sections and retain checked byte counts,
  task lifetimes, coverage refusal, external-world digests, and the TCB
  (`design.md:174-224`, `361-480`).
- **P0-P4 and A0-A18:** all five phases and all nineteen primitives are present.
  A15 still contains six mutations, A16 two fault injections, A17 exact
  distinct-address-space 32-byte movement, and A18 effective CPUID/RDTSC
  interception (`design.md:482-594`). E-2/E-3 concern the exact executable
  verdicts, not missing rows.
- **Appendix:** exact parent/Hermit/nested and standalone Reverie/Narf/man-pages
  revisions, commit-pinned public links, the eleven-row provider inventory,
  platform matrix, 24 independently verified canonical findings, query pairs,
  document classes, exclusions, inverse checks, audit history, and runtime/source
  reconciliation are present (`design.md:622-760`). Local links resolved during
  this audit, and every F-1 through F-24 anchor appears in the design.
- **New scheduling prose:** it is explicitly **Proposed** and does not introduce
  an unsupported current Narf capability claim. E-1 is a soundness defect in the
  proposed rule, not claim-label drift.

## Goalpost-moving assessment

- Assertions weakened: **yes** — A2's `matched`-or-`declared unresolved`
  alternative permits a promotion input with no exact comparator outcome
  (`design.md:545`).
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **yes** — A16's generic `--expect=no_result` accepts the wrong refusal subclass
  (`design.md:559`). No numeric tolerance was widened and no A15 mutation was
  skipped.
- Failure renamed or relabelled as a pass: **no**. Guest versus infrastructure
  failure is now explicit. The A2/A16 problems weaken gate precision but do not
  literally rename a failure `matched`.
- Check deleted instead of satisfied: **no**. All P0-P4 phases, A0-A18 rows,
  six A15 mutations, and two A16 injections remain.

## Verification and limitations

This audit read the complete locked Audit A, Audit B, Audit C, deterministic
Audit D, independent trust-gate verification, and revised draft. Scheduler
grounding followed the required order: the complete 16-page ASPLOS 2020
*Reproducible Containers* paper including its artifact appendix (local copy
SHA-256 `6346dfa0e5ea193fb975d5330da80d030f2a7641d95c7063069c8f8d7d875539`),
the complete pinned Hermit `detcore/src/scheduler.rs` including tests,
`PROJECT_VISION.md`, and `ai_docs/hermit-v2-roadmap.md`, before the proposal.
It also checked all local Markdown links, all F-1-F-24 anchors, all A0-A18 IDs,
and `git diff --check`. It did not rerun source discovery, remote CI, product
runtime, or the already independent literal verifier.

Verdict: **CHANGES REQUIRED** at the reviewed design hash.
