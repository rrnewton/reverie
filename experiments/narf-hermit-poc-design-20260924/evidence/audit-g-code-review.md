# Audit G — final adversarial code review

## Verdict

**CHANGES REQUIRED.** The design recommendation is technically coherent and the
locked fact set remains intact, but the owner-review package is not complete at
this exact working-tree state. One acceptance row lowers its own promotion bar,
the mandatory trust gate verifies only the cited subset rather than every
load-bearing current-state claim, and the completion/audit records still report
required work as open.

Review target: `/home/newton/work/dev-hermit/worktrees/slots/narf-hermit-design`,
base and current Git `HEAD`
`4b4250928d52a5e1f9f7a0b258fb734f06196d8e`; all 27 reviewed package files were
untracked additions. The exact reviewed `design.md` SHA-256 was
`3c43221354848fdcc3df17fdbc6b56b80228057dd94615c1a7a776bf529f5a61`.
Before this audit file was added, the sorted path-plus-content SHA-256 manifest
of the 27-file, 570,388-byte package hashed to
`c844de8e50b4c5e8a91189914393188c84d7d208851f9607c5f214fe0610a9cf`.

## Findings

### Severity 1 — A2 can pass without the equality result its detailed contract requires

`design.md:545` permits “`compare --expect=matched` or declared unresolved” for
A2. That conflicts with `design.md:573`, which says the Linux/Narf pair must
agree, and with the P0/P1 promotion table at `design.md:486-487`, which names
the applicable A2 subset/full case as an executable gate. The manifest schema
at `design.md:509` also has no `unresolved` evidence status. As written, an
implementer can avoid the cross-kernel page-boundary assertion by preserving the
research uncertainty rather than resolving it through the test.

Required change: A2 may remain **specified** and its native boundary may remain
**Unresolved** before implementation, but promotion must require
`compare --expect=matched`. An unresolved, unsupported, or unavailable result
must block promotion or produce the appropriate non-success class. Remove the
alternative pass path; do not weaken the detailed A2 byte-count/fault-boundary
requirements.

### Severity 1 — the trust-gate denominator omits load-bearing claims

The canonical stream and verifier are internally valid: `findings.jsonl` and
`verification.jsonl` each have 24 unique matching IDs, all 24 verdicts are
`[VERIFIED]`, and the prescribed gate reports 100%. That percentage covers only
the 24 `[F-N]` anchors found in the draft, however. It does not establish the
protocol's requirement that every actionable claim carries a machine-verified
tag.

Examples of material current-state claims outside that denominator include:

- `design.md:87-92` reports fourteen executed ptrace fixtures, two named smokes,
  and their limited meaning without any canonical finding anchor. Preliminary
  `N-F-18`/`N-F-19` and the coverage ledger contain the evidence, but those
  namespaced records are explicitly ineligible for the final gate.
- `design.md:101-108` enumerates Narf's implemented and missing ptrace requests
  under `[F-8]`, while canonical F-8 claims only option/event behavior and its
  sole cited literal is a source comment. The operational TRACESYSGOOD path is at
  `ptrace.rs:387-395`, option storage at `ptrace.rs:1134-1139`, and the exhaustive
  request inventory exists only in the preliminary evidence.
- `design.md:135-153` makes broader loader, BPF-runtime, and domain-backend
  assertions than canonical F-14, F-11/F-24, and F-12 respectively verify.
- `design.md:397-403` attributes complete structured records and I/O-buffer hash
  requirements to F-15, whose canonical claim and cited range establish the
  log-comparison policy but not that entire sentence.

This is a denominator error: a 24/24 pass can coexist with uncited actionable
claims. It also conflicts with the explicit rule not to infer behavior from
comments. Required change: either narrow each sentence to the corresponding
canonical claim or add literal, operational canonical findings for every
load-bearing extension (including exact-commit runtime evidence), independently
verify them, cite them in the design, and rerun the trust gate. The final gate
record must state both cited-anchor coverage and the method used to establish
that the actionable-claim inventory itself is complete.

### Severity 1 — the completion control still declares completed work open

`evidence/completion-checklist.md:3-5` says an item remains open until its
evidence exists and has been checked. Despite the locked Audit A, ready Audit B,
complete design, canonical verification, and link checks, lines 18-75 still
leave every Narf reproduction, source/runtime separation, coverage control,
technical decision, sequential audit, and final-artifact item unchecked. Lines
11-13 are legitimately pre-commit, but the rest make the durable package state
that the user-requested design and audits are incomplete.

Required change: reconcile each substantive item with its authoritative
evidence and check it only after verification. Keep commit/clean-state items open
until the corresponding operation actually happens, then update them through a
truthful commit/amend-and-reread sequence or remove those future-state boxes from
the committed checklist. Re-run this exact-head review after substantive edits.

### Severity 2 — the audit appendix does not reconcile post-draft failures

`design.md:742-750` records Audit A and Audit B, but does not mention the
independent post-draft writing audit or deterministic-scheduling audit. The
durable files `evidence/audit-c-writing.md:3-12` and
`evidence/audit-determinism.md:1-4` both say **CHANGES REQUIRED** and are bound to
the older design hash
`ab75ee74b384d5681cf8a1b68bdcc3d53317c04f990f5bc2523fe73aadceb1fa`.
The current design appears to address their substantive findings, but no durable
resolution map says so. An owner opening the evidence directory therefore sees
two unqualified failed audit verdicts beside an “owner-review draft.”

Required change: add a concise resolution record mapping every Audit C and
determinism finding to the corrected current lines, clearly state that those
audits targeted the superseded hash, and obtain a final exact-head verdict after
the remaining findings in this review are fixed. Do not delete the historical
failed audits.

## Goalpost-moving assessment

- Assertions weakened: **yes** — A2's required paired equality can be replaced
  by “declared unresolved” in `design.md:545` even though the detailed row
  requires agreement.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **yes** — the A2 alternative is an exemption/skip path, and the 24-anchor trust
  denominator excludes uncited actionable claims while still reporting 100%.
- Failure renamed or relabelled as a pass: **no** — the outcome taxonomy itself
  keeps unsupported, no-result, divergence, and infrastructure failure distinct.
- Check deleted instead of satisfied: **no** — historical failed audits and the
  strict A15/A16/A17/A18 controls remain present. The issue is incomplete
  denominator coverage and one inconsistent A2 pass condition, not deletion of
  those controls.

## Checks that passed

- Audit A's locked inputs still match every SHA-256 recorded in
  `evidence/audit-a-lock3.md:373-392`; no fact file drifted after the lock.
- Audit ordering is valid: the final Audit A says **LOCKED** before Audit B says
  **READY TO DRAFT**. Historical failed Audit A reports remain visible.
- All five required interposition mechanisms, every public Reverie `Backend`
  implementation/provider path, the P0-P4 stages, A0-A18, trace normalization,
  outcome taxonomy, security boundaries, exact revisions, and remaining
  uncertainties are present.
- A15 retains six independent mutations, A16 retains two refusal injections,
  A17 retains distinct-address-space 32-byte movement, and A18 requires effective
  instruction interception and Tool-selected observations. Those bars were not
  lowered.
- All five JSONL files parse. Canonical finding and verification IDs are unique,
  dependencies resolve, and their ID sets match exactly.
- Every local Markdown link resolves. All public source URLs in `design.md` are
  commit-pinned where applicable; the document correctly discloses that they
  were not interactively browser-tested.
- All 27 pre-audit files passed whitespace/error checks with no diagnostic.
- Running the mandated gate with the current `verification.jsonl` produced
  `verified_pct=1.0000`, `coverage=1.0000`, `blocked=false`, and no failed
  finding. This validates the cited subset, subject to the denominator finding
  above.
- No product runtime, Hermit command, QEMU guest, commit, push, or publication
  occurred during this review.

## Verification limits

This was a documentation/evidence review. I inspected the complete reader-facing
design, audit sequence, completion state, canonical findings and verifier
records, locked hashes, source links used by the load-bearing examples, and the
complete Git working-tree inventory. I did not rerun Narf CI, execute a provider,
or independently replay all raw query receipts; Audit A and the verifier record
those checks at exact revisions. Because the target has no report commit yet,
the verdict is bound to the working-tree hashes above; any correction requires a
fresh exact-head review.

**Final verdict: CHANGES REQUIRED.**
