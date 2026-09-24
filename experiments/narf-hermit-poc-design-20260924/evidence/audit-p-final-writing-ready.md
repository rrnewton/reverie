# Audit P: exact-head writing and provenance re-review

## Verdict

**CHANGES REQUIRED.** Audit N's sole finding is closed: the resolution map now
targets the exact current design, records the M corrections and current verifier
hashes, and preserves the superseded audit verdicts as history. The design prose
itself remains ready for owner review. One new, narrowly bounded provenance
overclaim was introduced in the resolution text: it says all 67 evidence items
are “in-range,” although only the 58 file references have line ranges.

No architecture, design, acceptance-test, or canonical-fact change is required.

## Exact review target

- `design.md` SHA-256:
  `2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`
- `audit-resolution.md` SHA-256:
  `c3458575e76ead270619cb144832167649ccccf1252bbd3b01aa1489476a17a6`
- `findings.jsonl` SHA-256:
  `b9a00bca41937b9e0f9770fdf04042a5dae2d36495d59adae035da57e67bde38`
- `verification.jsonl` SHA-256:
  `899c545fad45b83dcb978a4926a80066da2b8e7bb697902697797179e750be49`
- `trust-gate-verifier-report.md` SHA-256:
  `d813296c8f3adce6666fef94804f9f73b20479ebc2cbabc28e5e88ebed6b14e8`
- `actionable-claim-inventory.md` SHA-256:
  `a72d2c1d22ce3203fc016fcd85186568967197c55e7bdbcb9cbe50ac4fae0b4b`
- Audit N SHA-256:
  `b27c5585982f63869537c721b448ff68a8c0ec18f45c713f0d55901f82b8f9d0`
- Audit M SHA-256:
  `1452dc6ea164e8e38d22d9a7d38aeb033c5f8e94f69e03f80f62ff725e7fdc92`

The canonical gate remains a valid pass: 38/38 findings verified,
`verified_pct=1.0000`, `coverage=1.0000`, and `blocked=false`.

## Finding

### P-1 — Severity 2: “67/67 evidence items as in-range” has the wrong denominator

`audit-resolution.md:59-60` says independent reverification records “38/38
findings and 67/67 evidence items as in-range.” The verifier stream has 67
evidence items, but only 58 are `file_ref` items with declared inclusive line
ranges. The remaining nine are six tool outputs, one captured knowledge record,
and two external URLs. Their statuses are informational or verified non-code;
calling them “in-range” is not defined.

The underlying result is sound. All 58 file references are exact at their
pinned objects and inside their declared ranges: 33 are `VERIFIED_EXACT` and 25
are `VERIFIED_SUBSTRING`. All 67 evidence items were checked and accounted for.
F-37 has no remaining relocation.

Required change: replace the sentence with the two exact denominators, for
example: “Independent reverification records 38/38 findings and all 67 evidence
items checked; all 58 file references are in-range, with no relocation, stale
reference, or unknown citation.” Do not change the canonical evidence or
verification results.

## Audit N closure and package checks

- **Exact target:** closed. `audit-resolution.md:3-4` now names the current
  design SHA `2c443f…`, exactly matching the audited file.
- **M corrections:** closed. The map records the removed mutable-VFS/network
  current-state assertion, corrected F-37 range, current findings/verifier
  hashes, and the refreshed 38/38 gate (`audit-resolution.md:46-67`).
- **Historical scope:** closed. Audit L is explicitly historical because it
  targets `8f28…`; Audit M and Audit N retain their original verdicts and exact
  hashes rather than being rewritten as current-head passes.
- **Recommendation and labels:** pass. The recommended x86_64 single-task
  dispatcher-hook POC is plainly **Proposed** and nonconforming at P0. Source-
  only, bounded execution, proposed, software-helper, above-API, deferred, and
  unresolved claims remain distinct.
- **Runtime boundary:** pass. No Hermit-on-Narf implementation or paired run is
  claimed; exact-commit Narf CI is bounded and its failures remain visible.
- **Diagram and trust boundaries:** pass. Linux and Narf event/data paths remain
  separate until the proposed canonical trace, with distinct guest, kernel,
  controller, and comparator regions and bounded-copy control described in the
  accompanying text.
- **Acceptance executability:** pass. The versioned manifest, exact comparator
  classes, artifacts, timeouts, counts, repetitions, negative controls, A2
  promotion restriction, six A15 mutations, two A16 refusals, A17 remote-memory
  proof, and A18 effective instruction interception remain unchanged and
  falsifiable. Placeholders cannot promote an unexecuted case.
- **No design overclaim:** pass. Filesystem and network are now described as
  uncontrolled semantic-input classes for the POC, not as uncatalogued current
  Narf implementation facts. Provider/helper status and aarch64 evidence remain
  bounded by F-36/F-38 and source-versus-runtime labels.

## Goalpost-moving assessment

- Assertion weakened, tolerance widened, exemption added, or test skipped:
  **no**.
- Failure renamed as success: **no**; only `matched` remains success.
- Check deleted instead of satisfied: **no**. Audit N's finding was fixed by
  exact retargeting and explicit provenance, not by removing audit history.
- New issue scope: **provenance wording only**. P-1 does not dispute any verified
  fact or require another product/runtime test.

## Limits and disposition

This was a writing, structure, provenance, and overclaim re-review. I read the
complete current resolution map, the unchanged exact-head design, refreshed
verifier report and stream, actionable inventory, Audit M, Audit N, and the
historical deterministic verdict. I did not rerun product runtime, remote CI, or
source discovery.

After P-1's denominator wording is corrected and the resulting resolution-map
hash is checked, the package is **READY** on the writing/provenance axis without
another design or canonical-evidence change.
