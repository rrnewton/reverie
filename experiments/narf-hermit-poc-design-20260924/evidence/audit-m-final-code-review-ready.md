# Audit M — final adversarial evidence/code review

## Verdict

**CHANGES REQUIRED.** The expanded F-1–F-38 set closes most of Audit J, and the
design/test gates remain strict. Two bounded evidence-integrity defects prevent
an exact-head **READY** verdict: one previously identified current-state input
claim is still outside the actionable denominator, and one canonical file
reference is knowingly outside its declared inclusive range while the trust
gate still counts it as verified.

## Exact review target

- Repository/worktree:
  `/home/newton/work/dev-hermit/worktrees/slots/narf-hermit-design`
- Git base and current `HEAD`:
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`
- `design.md` SHA-256:
  `8f28be03df3c61507254d27607a04e3757dbe2c38e379aeb2280fc9c5d44c7e6`
- `findings.jsonl` SHA-256:
  `3cedb606506c2d846d91a0b500af46fc979dcddefab68c78815978db64b06fc1`
- `verification.jsonl` SHA-256:
  `d6622977ed9d4770e1c046c08e052318f1d40bb633fa1326a6d3af8835aeafd7`
- `actionable-claim-inventory.md` SHA-256:
  `a72d2c1d22ce3203fc016fcd85186568967197c55e7bdbcb9cbe50ac4fae0b4b`
- `trust-gate-result.json` SHA-256:
  `8c4e2629030333ab1cf19057bdee285b7cad9a1285244f57a3430d31061fb792`
- `trust-gate-verifier-report.md` SHA-256:
  `d564b043219e84e54b230bfedd034b4ee5661e5eb07f8c77bd95be4725bf9a19`
- `audit-resolution.md` SHA-256:
  `17cf2342aadc669931e7ed1489a262c20659724388ce0f0420b408ef2cada73c`
- Exact-head deterministic Audit L SHA-256:
  `1b3bf01751589fedea3049d97f1847bcedf59303f15f46856adb5d3a77317e95`
- Before this report was added, the sorted path-plus-content SHA-256 manifest of
  the 37-file, 694,446-byte package hashed to
  `25d6d4ad8ce1f9dff00c84699413596a08ab54252758a2108717cf2e97f717b2`.

All reviewed files were untracked additions relative to `HEAD`; no report commit
existed at review time.

## Findings

### Severity 1 — mutable VFS and asynchronous network remain outside the trust denominator

`design.md:644` states that current Narf has a mutable VFS and asynchronous
network, and uses that fact to require virtualization, capture, or exclusion at
P3. This is a material current-state input/scope assertion under the inventory's
own method (`evidence/actionable-claim-inventory.md:5-11`). F-32 and its inventory
row establish hardware-derived time and CSPRNG output only; none of F-34–F-38 or
the other inventory rows establishes the VFS/network assertion. Exact-commit
aarch64 execution, uaccess, BPF authority, ptrace access, and provider helpers
were added, but this specific Audit J denominator example was not.

The claim is plausible and preliminary source evidence exists, but the canonical
gate is intentionally separate from that research lane. Consequently the
verifier's statement that every decision-changing current-state risk row maps to
one of 28 families (`evidence/trust-gate-verifier-report.md:123-129`) is still
false at the recorded hashes.

Required change: either remove/narrow “plus mutable VFS and asynchronous
network” from the current-state cell, or add literal operational canonical
evidence and a corresponding inventory row, independently verify it, and rerun
the gate. Do not treat F-32's time/randomness evidence as proof of unrelated
filesystem/network state.

### Severity 1 — F-37 is counted as verified despite an invalid declared range

F-37 evidence item 3 declares `userspace/src/ptrace.rs` lines 816–825, but its
four-line literal is actually lines 823–826 at the pinned object. Line 826 is
outside the declared inclusive range. The verifier records
`status="VERIFIED_RELOCATED"`, `resolved_line_end=826`, while the enclosing F-37
verdict remains `[VERIFIED]`. The verifier report explicitly admits the one-line
drift at lines 31–38 and nevertheless says no evidence reference is unverified,
hallucinated, or stale. The shell trust gate examines only the top-level tag, so
it reports 38/38 and `blocked=false`.

The ptrace-access claim itself is true and the public source-index link already
covers lines 788–834. The defect is the canonical receipt: the declared evidence
range does not contain its literal, contrary to the exact-range guarantee.

Required change: change F-37 evidence item 3's `line_end` to 826, independently
reverify it as exact/substring within the declared range rather than relocated,
update the verifier report and all bound hashes, and rerun the fail-closed gate.
Do not make relocated evidence silently qualify as exact verification.

## Closed prior findings and consistency checks

- The aarch64 executed result is now F-36 and has exact-head job evidence.
- Guarded user-copy and BPF load authority are now F-34/F-35 and are cited in the
  security table. Ptrace attach access is F-37; helper-provider existence is
  F-38.
- The BPF decision row was narrowed to the current properties its cited findings
  actually establish; the synchronous-logger assertion was removed.
- A2 promotion requires a characterized subcase and exact `matched` result;
  unresolved/uncharacterized cases cannot promote.
- A16a and A16b require the exact coverage and comparison-refused subclasses.
  Generic `no_result`, divergence, or match cannot satisfy them.
- A15 retains six independent mutations, A17 retains distinct-address-space
  32-byte movement, and A18 requires effective instruction interception and
  Tool-selected observations.
- Audit history and the completion checklist remain truthful. Historical failed
  reports are preserved, corrections are mapped, and staging/commit/clean-state
  checks remain open until those operations occur.
- Exact-head deterministic Audit L returns **READY** for the current design hash.

## Goalpost-moving assessment

- Assertions weakened: **no** — no acceptance assertion was weakened; A2 is
  stricter than the originally reviewed draft.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no** — A15–A18 and the exact no-result subclasses remain intact.
- Failure renamed or relabelled as a pass: **no** — the semantic and
  infrastructure outcome taxonomy remains fail closed.
- Check deleted instead of satisfied: **no** — the new findings/inventory and
  historical audits remain. The blockers are an omitted claim and malformed
  evidence metadata, not removed checks or lowered runtime gates.

## Verification

- Read the complete current design, 28-row actionable inventory, F-1–F-38
  canonical stream, 38-record verifier stream, trust result/report, resolution
  map, completion checklist, historical adversarial findings, and exact-head
  deterministic review.
- Parsed all five JSONL files. The canonical stream has 38 unique IDs, 67
  evidence items, and no unresolved dependency. The verifier has 38 matching
  IDs and top-level `[VERIFIED]` tags, but one evidence item is
  `VERIFIED_RELOCATED` as reported above.
- Confirmed 48 design anchor occurrences cover all 38 unique IDs and cite no
  unknown ID.
- Re-ran the trust-gate script. It reported `verified_pct=1.0000`,
  `coverage=1.0000`, `blocked=false`, and no failed finding; that result is
  insufficient because the gate neither inventories uncited claims nor rejects
  a relocated sub-verdict under a top-level verified tag.
- Read the pinned F-37 source: the access-check literal begins at line 823 and
  ends at line 826, confirming the declared line-825 endpoint is wrong.
- Resolved every local Markdown link and found no broken target. Public source
  links remain commit-pinned where applicable.
- Ran whitespace/error checks over the complete pre-Audit-M package with no
  diagnostic.
- No source/runtime classification regression, product runtime, Hermit command,
  Narf/QEMU guest, commit, push, or publication occurred during this review.

## Limits

This is a documentation/evidence review, not runtime validation. I did not rerun
Narf CI or raw discovery queries. Any correction to the design, findings,
verification, inventory, or trust receipt changes the bound hashes and requires
a fresh exact-head adversarial review.

**Final verdict: CHANGES REQUIRED.**
