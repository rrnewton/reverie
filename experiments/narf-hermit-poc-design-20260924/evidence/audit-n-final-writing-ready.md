# Audit N: final exact-head writing and structure audit

## Verdict

**CHANGES REQUIRED.** The reader-facing design at the exact SHA below is
structurally and technically ready for owner review: its recommendation is
direct, its current/proposed/runtime labels are sound, the corrected diagram
preserves both execution-cell boundaries, and its acceptance contract remains
falsifiable. One release-blocking provenance defect remains outside the design
text: `audit-resolution.md` still names the immediately superseded design SHA
as its resolution target while claiming to map the current design.

This verdict does not reopen Audit A and does not request a change to the
architecture or canonical fact set.

## Exact review target

- `design.md` SHA-256:
  `2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`
- `findings.jsonl` SHA-256:
  `b9a00bca41937b9e0f9770fdf04042a5dae2d36495d59adae035da57e67bde38`
- `verification.jsonl` SHA-256:
  `899c545fad45b83dcb978a4926a80066da2b8e7bb697902697797179e750be49`
- `trust-gate-verifier-report.md` SHA-256:
  `d813296c8f3adce6666fef94804f9f73b20479ebc2cbabc28e5e88ebed6b14e8`
- `actionable-claim-inventory.md` SHA-256:
  `a72d2c1d22ce3203fc016fcd85186568967197c55e7bdbcb9cbe50ac4fae0b4b`
- reviewed `audit-resolution.md` SHA-256:
  `17cf2342aadc669931e7ed1489a262c20659724388ce0f0420b408ef2cada73c`

The refreshed canonical gate reports `verified_pct=1.0000`,
`coverage=1.0000`, `blocked=false`, with all 38 findings verified. Its 67
evidence items include 58 file references: 33 exact-range literals and 25
contiguous in-range literal substrings. The corrected F-37 access-check snippet
is exact source text at lines 823--826 and is wholly within its declared
816--826 range.

No Hermit command, Narf/QEMU guest, Reverie provider, external tracer, commit,
push, or publication occurred in this audit.

## Finding

### N-1 — Severity 1: the resolution map is not bound to the audited exact head

`audit-resolution.md:3-4` declares design SHA
`8f28be03df3c61507254d27607a04e3757dbe2c38e379aeb2280fc9c5d44c7e6`
as its resolution target. The current design is instead
`2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`.
The later SHA changes the uncontrolled-input risk wording so it no longer
asserts uncatalogued mutable-VFS or asynchronous-network implementation, and
the canonical evidence change corrects F-37's literal range. Those are exactly
the kinds of evidence/prose corrections for which an exact-target resolution
map exists.

The mismatch matters because the map repeatedly says “current design,” claims
G-2 denominator closure, and says final exact-head verdicts are recorded
separately (`audit-resolution.md:35-49`). The design appendix in turn tells the
owner that every failed finding's current disposition is mapped there
(`design.md:802-807`). A map explicitly pinned to a different SHA cannot prove
that statement for this exact head, even though direct inspection shows the
new design text fixes the outstanding prose issue.

Required change: update the resolution map's target to the exact current design
SHA and record the two post-8f28 corrections: narrowing the VFS/network wording
to uncontrolled POC input classes, and correcting F-37's evidence range with a
fresh 38/38 verification and gate. Preserve the historical H/I/J target hashes
and verdicts; do not silently relabel them as reviews of 2c443f. Name the new
exact-head writing, deterministic, and adversarial reports only after each has
actually audited this SHA.

## Writing and structure checks that pass

- **Reader question and answer:** the first paragraph recommends one smallest
  POC—an x86_64, single-task, trace-producing dispatcher-hook prototype—and
  immediately explains what it proves, what it does not prove, and why stock
  ptrace, USER_NOTIF, ptrace emulation, and BPF isolation do not replace it.
- **Claim labels and evidence boundary:** current source, bounded execution,
  proposed interfaces, software-helper status, above-API policy, deferred work,
  and unresolved choices remain visibly distinct. The prominent no-runtime
  disclosure prevents source declarations, tests, or job names from becoming
  claimed Hermit-on-Narf support.
- **Authoritative structure:** current forcing facts precede the five-mechanism
  decision table; the proposed interface, trace contract, outcomes, trust
  boundary, phases, acceptance gates, risks, and source appendix each have a
  clear home. The eleven-row provider inventory remains an appendix and is not
  confused with the five architecture candidates or three public `Backend`
  implementations.
- **Diagram:** the Linux controller and proposed Narf adapter now have separate
  event/data outputs that meet only at the proposed canonical trace. Guest,
  kernel, external-controller, and comparator regions are distinct; the prose
  separately defines outward event/checked-data and inward disposition/bounded-
  copy control.
- **Trace and outcome semantics:** match requires input and coverage
  admissibility plus exact records, output, disposition, and cleanup. Guest
  signal/core disposition stays semantic; provider/kernel/controller/harness
  crashes remain infrastructure failures. Unsupported and every `no_result`
  subclass remain non-successes.
- **Acceptance executability:** `narf-hermit-acceptance/v1` fixes the command
  shape, exact revisions, statuses, repetition/timeout constraints, counts,
  artifact hashes, comparator expectations, and negative controls. Literal
  `${POC_DRIVER}` placeholders cannot advance beyond `specified`; A2 research
  cases cannot promote; A15 retains all six mutations; A16 has two exact
  refusal subclasses; A17 requires distinct-address-space 32-byte movement;
  and A18 requires observed Tool-selected CPUID/RDTSC values.
- **Current-claim completeness:** the revised risk row now anchors only the
  verified hardware-time/CSPRNG fact and describes filesystem/network as
  uncontrolled POC input classes, not current implementation claims. The
  actionable inventory's 28 families and the design's 48 finding citations
  have the exact F-1 through F-38 union with no unknown ID.
- **Density:** the 9,670-word document exceeds Audit B's 6,000--7,500-word
  target, but the excess is predominantly the normative scheduler transition
  table, executable command matrix, and full A0--A18 evidence matrix required
  by later correctness audits. The two acceptance tables serve different
  command-level and semantic-review purposes and do not state conflicting
  gates. This is not a release blocker for the requested detailed design.

## Goalpost-moving assessment

- Assertions weakened: **no**. The VFS/network correction removes an
  unsupported current-state assertion while retaining both input classes as
  P3 risks and required controls.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. Exact trace comparison, 100-run P2/P3 requirements, all six A15
  mutations, both A16 refusals, A17, and A18 remain intact.
- Failure renamed or relabeled as a pass: **no**. Only `matched` is success.
- Check deleted instead of satisfied: **no**. F-37 was made in-range and
  independently reverified; the trust gate still covers all 38 canonical IDs.

## Limits and disposition

This was a writing, structure, visualization, provenance, and overclaim audit
against the exact hashes above. I read the complete current design, Audit B
criteria, resolution map, actionable inventory, refreshed verifier report, and
the prior post-draft writing/determinism/adversarial reports. I did not rerun
product runtime or source discovery and did not treat the old exact-head audits
as verdicts on the new SHA.

The design prose itself needs no writing correction found by this pass. The
package becomes **READY** for the writing/structure axis once N-1's exact-head
provenance is corrected and this verdict is rechecked against the resulting
resolution-map hash.
