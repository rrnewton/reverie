# Audit Q — final exact-head adversarial evidence/code review

## Verdict

**READY.** No blocking correctness, evidence, package-consistency, or
goalpost-moving finding remains at the exact hashes below. Both Audit M
blockers are closed: the risk table no longer asserts unverified current
filesystem/network behavior, and every F-37 literal is inside its declared
inclusive range with an ordinary verified status. The full F-1–F-38 denominator
and fail-closed acceptance gates are ready for owner review.

This is design/evidence readiness only. It does not approve an implementation or
claim an executed Linux/Narf pair.

## Exact review target

- Repository/worktree:
  `/home/newton/work/dev-hermit/worktrees/slots/narf-hermit-design`
- Git base and current `HEAD`:
  `4b4250928d52a5e1f9f7a0b258fb734f06196d8e`
- `design.md` SHA-256:
  `2c443f583faf18e2baa54345536b101f5ec1b91ed393788c3a716576c2e89e72`
- `findings.jsonl` SHA-256:
  `b9a00bca41937b9e0f9770fdf04042a5dae2d36495d59adae035da57e67bde38`
- `verification.jsonl` SHA-256:
  `899c545fad45b83dcb978a4926a80066da2b8e7bb697902697797179e750be49`
- `actionable-claim-inventory.md` SHA-256:
  `a72d2c1d22ce3203fc016fcd85186568967197c55e7bdbcb9cbe50ac4fae0b4b`
- `trust-gate-result.json` SHA-256:
  `aa24adf8786e83e2dd5966ebb9ed063dea8a24b46b3f6edd07b0219334b34613`
- `trust-gate-verifier-report.md` SHA-256:
  `d813296c8f3adce6666fef94804f9f73b20479ebc2cbabc28e5e88ebed6b14e8`
- `audit-resolution.md` SHA-256:
  `54348ede2046e5aceaff24f3d2208a94af1f3c76275921342768396c9a2083db`
- Exact-head deterministic Audit O SHA-256:
  `4424a71e12ac55848f9273bba36192b3b3ac822a8607ce73a652eb3764bad87b`
- Audit P writing/provenance report SHA-256:
  `15e9f4fcd96c11e1c7b69e5e99ad361911519eec20256c0d431e52d9674cf866`
- Before this report was added, the sorted path-plus-content SHA-256 manifest of
  the 41-file, 722,418-byte package hashed to
  `5943e83175263cbb6a9c39e649a213c5b380943de07176bdc84c5ce5fba7c27f`.

All reviewed files were untracked additions relative to `HEAD`; no report commit
existed at review time.

## Findings

No blocking or advisory finding remains.

## Audit M and Audit P closure

- `design.md:644` now says filesystem and network are uncontrolled semantic
  input classes *for this proposed POC*. It no longer attributes mutable-VFS or
  asynchronous-network behavior to current Narf. The design still requires each
  input class to be virtualized, captured, or excluded and keeps external
  networking outside P0. This narrows an unsupported source claim without
  weakening the POC input contract.
- F-37 evidence item 3 now declares lines 816–826. Its literal is exact source
  text at lines 823–826 of the pinned object, so it is entirely inside the
  declared range. The machine verdict is `VERIFIED_SUBSTRING`, not relocated;
  no relocated, stale, or unverified sub-verdict remains.
- The resolution map targets the current design hash and accurately states both
  denominators: all 67 evidence items were checked, and all 58 range-bearing
  file references are in-range. This closes Audit P's sole provenance wording
  finding without altering canonical evidence or the design.
- Historical `CHANGES REQUIRED` reports remain immutable and bound to their
  superseded hashes. Their corrections are mapped rather than silently
  relabelled.

## Current-state denominator and trust result

- The actionable inventory has 28 material current-state families. Its finding
  union is exactly F-1 through F-38, matching the design and canonical stream.
- The design has 48 anchor occurrences spanning all 38 IDs, no unknown ID, and
  no canonical ID omitted from the inventory.
- The canonical stream has 38 unique records, 67 evidence items, only the
  declared `native` source kind, and no unresolved dependency.
- The verifier has one matching `[VERIFIED]` record for every canonical ID. All
  58 `file_ref` items are exact or contiguous substrings within their stated
  pinned ranges; the remaining nine non-file items are separately classified.
- The revised risk wording is normative POC scope, not an uncited current-source
  assertion. The previously omitted uaccess, BPF authority, ptrace access,
  aarch64 CI, and provider-helper claims are covered by F-34 through F-38.
- Re-running the mandatory gate against the exact verifier stream returned
  `verified_pct=1.0000`, `coverage=1.0000`, `blocked=false`, and no failed
  finding. The durable trust result binds the exact design, findings,
  verification, and inventory hashes above.

## Acceptance and goalpost-moving assessment

- Assertions weakened: **no**. Characterized A2 promotion cases require an
  exact `matched` result; unresolved or uncharacterized cases remain
  nonpromoting research.
- Tolerance widened, exemption added, case skipped, or comparator relaxed:
  **no**. A15 retains six independent mutations; A16 retains separate exact
  `no_result: coverage` and `no_result: comparison_refused` controls; A17 still
  requires distinct-address-space 32-byte movement; A18 still requires effective
  interception and Tool-selected observations.
- Failure renamed or relabelled as a pass: **no**. Unsupported, input mismatch,
  coverage refusal, comparison refusal, semantic divergence, and infrastructure
  error remain distinct from `matched`.
- Check deleted instead of satisfied: **no**. Historical failed audits,
  contradiction ledgers, strict comparator controls, exact evidence receipts,
  and final re-reviews remain present.
- No repetition count, timeout, event-count requirement, artifact requirement,
  comparator class, trace field, or normalization rule was weakened to produce
  readiness.

## Package verification

- Read the complete current design, actionable inventory, canonical findings,
  verifier stream/report, trust result, audit-resolution map, completion
  checklist, historical adversarial reports, Audit P, and exact-head
  deterministic Audit O.
- Parsed all five JSONL files successfully; IDs are unique, dependencies resolve,
  and canonical/verifier ID sets match.
- Resolved every local Markdown link with no broken target. Public source links
  remain commit-pinned where applicable, with browser testing explicitly
  disclaimed.
- Ran whitespace/error checks over the complete pre-Audit-Q package with no
  diagnostic.
- Rechecked the exact parent, Hermit, nested and standalone Reverie, Narf, and
  Linux man-pages revisions and found no source/runtime classification drift.
- The completion checklist truthfully leaves staging, commit, clean-state, and
  exact-final-diff completion open for the coordinator's subsequent commit
  sequence.
- No product runtime, Hermit command, Narf/QEMU guest, commit, push, or
  publication occurred during this review.

## Residual limits

No Hermit-on-Narf implementation or paired execution exists. Transport choice,
SMP task lifetime, native fault-boundary characterization, input virtualization,
instruction interception, Narf replay, independent-process parallelism, and RCB
preemption remain proposed, unresolved, or deferred exactly as the design says.
Those are implementation-stage uncertainties, not evidence defects in this POC
design.

**Final verdict: READY.**
