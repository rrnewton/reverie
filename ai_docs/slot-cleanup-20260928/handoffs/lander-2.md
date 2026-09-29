# lander-2 handoff — 2026-09-02

## Slot snapshot

- Before this handoff file was created, `git status --short --branch` reported no
  staged, unstaged, or untracked paths.
- `HEAD` is `9c4ae3e02c5ddd4f9d72ac43971ac26305ae48f3` on
  `lander-2/pr2910-audit`. The configured tracking ref is gone, but the commit is
  remotely durable as the current head of
  <https://github.com/rrnewton/hermit/pull/2910>, branch
  `demo08review/pr2176-scalar-rebased`.
- This slot has no slot-specific stash. `git stash list` is repository-global and
  contains 26 older entries naming other branches/tasks; none belongs to this
  slot or may be applied, dropped, or treated as handoff state.
- Other local and remote branches containing this HEAD are shared Git state, not
  work owned by this slot. In particular `_land_2910` and the retained validation
  checkout branch are unrelated to the handoff file and must not be modified on
  this record's authority.
- `HANDOFF.md` is intentionally untracked slot state. Do not add it to Hermit.

## Published work awaiting the serialized landing lane

All five pull requests below were open and non-draft when re-read for this
handoff. Re-read the exact PR head, review record, checks, and merge state before
any action; evidence does not transfer across a push. Landing belongs to
mega-lander. A technical review produced by this same top-level lander-2 lane is
replication, not independent governance approval.

| Priority | Pull request and exact head | Task | State and evidence | Remaining blocker |
| --- | --- | --- | --- | --- |
| P0 | <https://github.com/rrnewton/dev-hermit/pull/274> `e42a855f3048aca0f519d1a2b1238411de5fa7e3` | `the-landing-path-accepts-validation-under-predicates-its-own-final-boundary-later-rejects` | Published from base `5bfa5d91f56e380631a549bf8dcf58631c97dabd`; two files only. Focused boundary test 1/1 passed in 35.87s; Python compile and diff check passed. The broader mutation suite was 30 passed/2 failed, and both failures reproduced on the untouched base. GitHub reported CLEAN. | Independent top-level review, then mega-lander. This is the queue-unfreezing fix and takes precedence over the other four. |
| 1 | <https://github.com/rrnewton/hermit/pull/2924> `809c975592327b27256db5d6d99c0b29de19e348` | `retain_exact_per_test_schema9_reader` | Reader-only schema-9 prerequisite; one file, no writer or qualification activation. Ledger 17/17 and full manifest-plan 211/211 passed; check, Clippy, fmt, and diff check passed. GitHub reported CLEAN. | Review at <https://github.com/rrnewton/hermit/pull/2924#issuecomment-5503205348> is technically useful but same-lane and therefore not independent. Mega-lander must obtain a distinct top-level review/attestation before landing. |
| 2 | <https://github.com/rrnewton/agent-utils/pull/108> `a6ae3da32173a7decd66f3347d1c32a9d9e930ed` | `make_the_pull_request` | Python and Rust planners require explicit hard-green or soft-green authority and preserve exact-head review binding. `scripts/validate.py` passed: Python 2,603; planner Rust 32/32; cross-language differential 97/97; full tool differential and package smoke passed. GitHub reported UNSTABLE, so its current checks must be read rather than inferred. | Technical re-review <https://github.com/rrnewton/agent-utils/pull/108#issuecomment-5503224414> is same-lane. Distinct top-level review and mega-lander decision required. |
| 3 | <https://github.com/rrnewton/hermit/pull/2923> `2939a3cd44f3f323c85a4417053f29ac485e1322` | `correct_hermit_validate_stale` | One-file help correction split from changes-requested PR2922. Validation admission is unchanged. Validate self-test passed with final status PASSED; manifest-plan 204/204, check, Clippy, fmt passed. GitHub reported CLEAN. | Same-lane technical audit is not independent; obtain distinct review, then mega-lander lands and closes the implemented-unlanded task. |
| 4 | <https://github.com/rrnewton/dev-hermit/pull/273> `8912ff3afbb38f82f91aa0f3beeb3dbc4e354445` | `audit-docs-and-skills-for-the-tip-equality-rule-that-contradicts-soft-green-landing` | Three-file documentation/diagnostic correction. `pr_status` tests passed 110 plus 35 subtests; receipt reconciliation 5/5; focused staleness test, Python compile, and diff check passed. GitHub reported CLEAN. | Technical review <https://github.com/rrnewton/dev-hermit/pull/273#issuecomment-5503246300> is same-lane. Distinct top-level review and mega-lander required. |

PR2922 remains CHANGES REQUESTED and must not land. It was split because it
combined later receipt/totals work with the independent help correction.

## Why dev-hermit PR274 is sound

The old final boundary asked the same moving-tip question twice:

1. `reverie_base_sha` had to equal live Reverie `main`.
2. `reverie_base_tree` had to equal the tree of live Reverie `main`.

Changing only the SHA comparison to ancestry is inert for a content-changing
descendant: the live-tip tree comparison still rejects it. This was measured on
the seven extant ledger rows: all recorded SHAs were resolvable ancestors, but
none of their recorded trees equalled the live tip tree.

PR274 applies the owner-approved rule already stated in Hermit's
`scripts/check-reverie-pin.rs`: a pin must be an ancestor of Reverie main; tip
equality is allowed but not required because requiring it makes the verdict a
property of **when** it is checked rather than of the tree. The boundary now:

- requires `reverie_base_sha` to be an ancestor of live Reverie main; and
- compares `reverie_base_tree` exactly with the tree of that recorded SHA.

The second clause remains a strict integrity check. It proves the receipt's
recorded tree is genuinely the tree of the commit it names. Comparing it with
the live tip instead tested whether the world had moved, duplicating the faulty
tip check rather than checking receipt consistency.

The isolated regression demonstrates both directions:

- a recorded ancestor passes even after a later content-changing commit;
- a resolvable SHA on a divergent branch returns
  `REVERIE_BASE_NOT_CURRENT`;
- an ancestor carrying a tree other than its own returns
  `REVERIE_TREE_MISMATCH`; and
- both the direct predicate and the running CLI consumer are bracketed.

Hermit authority is untouched: the recorded Hermit base must still equal the
current base, its tree must still match, and the receipt commit must still
contain that base. Tests retain `BASE_NOT_CURRENT` for a stale Hermit base and
`BASE_NOT_CONTAINED` for a head that does not contain it.

Consumers enumerated for the two-file change: `base_boundary_verdict`,
`base_boundary_report`, the receipt-digest CLI's canonical Python boundary,
`ci-hub/landing/land-pr.sh` at the final merge boundary, and the direct/CLI
regression bracket in `test_receipt_base_boundary.py`.

## Compatibility-website independent review

Verdict remains **CHANGES REQUESTED** at exact local candidate
`89017c05101e195ebbb8c505251b6d12e5b97e45`. Recon's authoritative field map is
`ai_docs/cells-json-and-results-series-lifetimes-20260902.md` at dev-hermit
`97fa27f27a64d184f698f6d9ce633da5ded4ad7f`.

### Shared Rust-owned types duplicated in Python

The eleven primary duplicated types are:

1. `TestMetadata`
2. `CellMetadata`
3. `HistoryRow`
4. `GateHistoryRow`
5. `CellResultsEvidence`
6. `SeriesRow`
7. `SeriesPayload`
8. `CellIdentity`
9. `CellVerdict`
10. `ComparisonSpec`
11. `ComparedLogCounts`

The duplication appears in `build_data.py` through `_TEST_FIELDS`,
`_CELL_FIELDS`, `_VALIDATION_FIELDS`, `_GATE_FIELDS`, `_SERIES_FIELDS`,
`_SERIES_PAYLOAD_FIELDS`, `_public_series_payload`, `_public_identity`,
`_public_verdict`, and `_public_cell_results`, including nested source types.
It also reaches private `qualifying_receipt._RFC3339_Z` and constructs an
artificial cell-results artifact only to invoke an existing predicate. Most
source projections use `exact=False`, so a new Rust field can be silently
omitted. The required shape is one Rust-owned public website
projection/validator that deserializes through the shared Rust types; Python
should package opaque validated output.

### Properly website-owned data

These five categories are local to the website rather than duplicate source
authority:

1. the Stage-1 artifact/output envelope, including filenames and the
   incomplete-data record shape;
2. compression metadata;
3. provenance, digest, and count bindings;
4. deliberate public omissions; and
5. derived current-main, product-result, and projection-decision fields.

For example, deliberately omitting `SeriesPayload.main_ancestry` and
`FirstDivergentMessages` is a valid allowlisted public projection.

### Separate vacuous-integration blocker

Production `build_data.py` invokes:

```text
cargo run --locked -p hermit-manifest-plan --bin manifest-metadata
```

The candidate's Hermit pin
`e85aaf9654983116ac26ae02beb8f95f7c46f02f` and then-current Hermit main
`8f0607348e252372b0286c0f3b590e6654822289` did not contain that binary. It
existed only at the then-unmerged PR2880 head
`bc04aa922e806e99222ee03ac81c87495be41f20`. Tests mock
`_run_manifest_metadata` or `bounded_capture`; they prove mock and error handling,
not that the required producer exists or can be reached through the website.
The producer must land, the gitlink must advance, and an exact-checkout test must
invoke the real binary through the website path.

### Producer and consumer chain

- `manifest-metadata` -> `build_data`
- Hermit `validate.rs` -> `HistoryRow` and cell evidence -> parent ledger
  validation/publication -> `export_inputs` -> `build_data`
- `series.py` -> Rust `check-series-rows`/`SeriesRow` -> `export_inputs` ->
  `build_data`
- `scorecard.rs` separately owns private `TrackedCells`; the candidate does not
  consume it
- `write_data` is the only committed production consumer of `build_data` and
  emits `build.json` plus
  `data/{cells,incomplete-data,runs,tests}.json.gz`
- the `build_data` and `write_data` test modules are direct consumers
- planned, untracked `scratch_render.py` was outside the reviewed diff and
  bypassed the public writer surface through private helpers

If the website lane returns with a revision, keep this review with lander-2: the
field-level authority map is already reconstructed here. Re-resolve the exact
candidate and changed paths before reviewing.

## Successor instructions

1. A bounce or review finding on dev-hermit PR274 takes priority over all other
   work. It unfreezes receipt-backed landing.
2. Before acting on any listed PR, re-read its live head and all verdicts. Do not
   reuse exact-head evidence after a push.
3. Do not count any lander-2-authored or lander-2-process-bound review as
   independent. Arrange a distinct top-level reviewer and truthful attestation.
4. Do not land these changes from this lane. Hand final landing to mega-lander,
   which holds serialized landing authority.
5. Do not revive or land PR2922. Preserve its live CHANGES REQUESTED record and
   use the split PRs above.
6. If a published PR lands, verify it from a fresh fetch by content, then let the
   landing lane apply the landed tag and closure evidence. Published-but-unlanded
   work must remain visible as such.
