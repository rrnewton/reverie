# Proxy Binding — Authority Registry, Worked Examples, Enforcement Taxonomy

Reference companion to the **Proxy Binding Review Axis** section of `AGENTS.md`. The
behaviour-changing rules live in `AGENTS.md`; this file holds the maintained registry, the
worked-example catalogue, and the mechanical-enforcement taxonomy that agents consult when
touching a gate, verifier, labeler, lander, status view, or closure path. Read it before
introducing a load-bearing authority or a new consumer of one.

## Load-Bearing Authority Registry

This registry covers authorities that currently decide validation, review, landing, dependency currency, or
workflow-policy outcomes. Add a row before introducing another load-bearing authority or consumer; update a
row only from exact code and tests, never from a PR description.

| Authority and qualified value | Canonical dereferencing verifier | Coverage and remaining hole |
| --- | --- | --- |
| **Local validation ledger:** exact Hermit SHA plus clean/full/counted local ledger row and satisfied per-node coverage. | `ci-hub validate-status` / `ci-hub/lib/validate_status.rs`; the lander calls it through `ci-hub/landing/local-validation-eligibility.sh`. | **COVERED on canonical producer and lander paths.** Hermit's Rust driver appends the ledger row first, then delegates label publication to `ci-hub apply-local-label --ledger`; that consumer re-derives the receipt from the ledger. Labels remain caches, not ledger evidence. |
| **Published validation receipt:** immutable receipt commit and path on the receipt branch, branch ancestry, recomputed receipt and log digests, exact repo/head, and a counted qualifying ledger row. | Producer: `ci-hub/validation/publish_receipt.py`. Consumer: the parent-pinned `ci-hub/validation/verify_receipt.sh`, never PR-controlled verifier code. | **PARTIAL.** The consumer fixture accepts 2/2 legitimate counted receipts and refuses a well-shaped nonexistent receipt, tampering, zero execution, and incomplete coverage. The Hermit merge gate and landing planner still accept a bare `locally-validated` label, and #1593 adds another shape-only parser. Replace every such consumer with the parent verifier. |
| **Historical Git provenance:** the claimed commit exists in the named repository and the measurement/artifact is bound to that commit, not merely written beside a 40-hex string. | **None.** The minimum identity primitive is a fresh fetch plus `git cat-file -e <sha>^{commit}` in the intended repository; measurement causation additionally needs a commit-bound artifact/receipt. | **MISSING.** Hermit #1546 validates provenance fields and full-SHA syntax without dereferencing the object or a measurement artifact. Commit existence alone proves identity, not that the measurement came from it. |
| **Adversarial/human review:** reviewer lane, verdict, PR number, and exact reviewed head in a durable receipt produced by that lane. | **None.** `scripts/core-review-protocol-lint.sh` checks cache labels only. | **MISSING.** Push-time label invalidation narrows staleness but does not prove who reviewed what; `passed-review-*`, numbered review labels, and `human-approved` are assertions until a lane-specific exact-head receipt is dereferenced. |
| **Landing authorization:** a durable coordinator/owner decision names the exact repository, PR, and head allowed to land, with no unresolved hold. | **None.** `land-pr.sh` accepts command-line PR/branch arguments and does not dereference a task authorization record. | **MISSING.** Agent assignment, a ready label, or invoking the lander is not authorization. A future verifier must consume an exact-head authorization receipt; keep this human/coordinator gate explicit until then. |
| **Landed PR identity and task closure:** GitHub's `mergeCommit.oid` is reachable from a freshly fetched target branch. | `ci-hub verify-landing`; `ci-hub/closure/verified_close.py` calls it. | **PARTIAL.** `ci-hub/landing/land-pr.sh` reimplements the proof, and `ci-hub/remediation/land_and_arm.py` can arm from `state == MERGED` plus a shaped `mergeCommit.oid` without proving ancestry. Route both through `verify-landing`; never substitute PR-head ancestry, `MERGED`, or `mergeStateStatus`. |
| **Hosted CI result:** an authoritative workflow/job run for the exact head, with terminal success distinct from failure and `NO_RESULT`. | Exact-head GitHub API run/job lookup plus the canonical result classifier in `ci-hub/check_outcome.py`. | **PARTIAL.** The lander, remediation, health, and history paths call the classifier, but other consumers still hand-roll lookup/classification; `hermit/scripts/pr_status.py` maintains separate conclusion sets and #1593 adds another implementation. Cancelled/skipped/missing/queued/stale are `NO_RESULT`, not red or green. |
| **Workflow-policy version:** the required context was emitted by the trusted current workflow definition, not an older PR-branch YAML with weaker rules. | Hermit #1579's versioned context/blob check and ruleset reconciler. | **MISSING on main.** Until the versioned gate lands and every required-context consumer switches, stale branch YAML can emit a current-looking green. |
| **Live dependency currency:** every tracked manifest and lockfile pin equals the freshly resolved canonical remote ref. | `scripts/check-reverie-pin.rs` from Hermit #1591: live `git ls-remote`, tracked `Cargo.toml` + `Cargo.lock` scan, exact equality. | **PARTIAL until #1591 lands and all paths call it.** The reviewed verifier has an exact-tip positive and real ancestor-behind negative; local validate, both DAGs, hosted aggregate, merge gate, and receipt production are the required consumers. |
| **Workspace ownership:** `worktree-state.json`, the managed `ACTIVE.md` row, registered worktrees, and actual checked-out branches/detached SHAs agree. | `scripts/check-worktree-registry.rs`; its fixture accepts 2/2 correct rows and refuses planted content drift while preserving a 1/1 correct control. | **PARTIAL.** `allocate-worktree.rs --check-only` calls it, but normal allocation, `release-worktree.rs`, and `worktree-gc.sh` do not all run the verifier before acting. They must call it rather than trusting or independently parsing one surface. |
| **Mechanism overlap:** every open PR changing the same load-bearing mechanism is in the coordinator's exact-head review set, including mechanisms an author failed to tag. | **None for semantic completeness.** `ci-hub pr-status` groups declared `mechanism:<slug>` labels; it cannot prove that the declarations cover the code's actual mechanisms or that two intentions are compatible. | **MISSING; human semantic authority required.** The label grouper is a discovery aid, not a verifier. Record coordinator review against the exact PR/head set, and retain file/symbol review for untagged overlap; do not describe label absence as proof of independence. |
| **Merge conflicts and final mergeability:** fetched PR/base objects are the objects analyzed, pairwise conflicts come from real Git merges, and the final merge command succeeds against the current target. | Planning: `scripts/pr_conflict_graph.py` fetches base/head refs, checks API-head equality, and runs `git merge-tree`. Landing: the actual `gh pr merge --rebase` command is the final arbiter. | **COVERED on canonical planning/landing paths.** `mergeable` and `mergeStateStatus` remain hints only. New planners must call the real merge-tree path; new landers must not promote those hints to authority. |

## The Generative Cure — Carry The Condition With The Value

The generative cure is: **carry the condition with the value.** A value measured under conditions it does
not record is a proxy, whether that value is a string, flag, status, hash, or number. Store `{ jobs: 32,
bytes: N }`, not a bare memory cap `N`; bind green to an exact-SHA run with a nonzero executed-test count;
bind landing to `mergeCommit.oid` ancestry on freshly fetched main, not a PR head or `MERGED` flag. A bare
value and a qualified value often read identically as facts, so inspection cannot reveal that the
qualification is missing. Reviewers must ask what conditions made the value true, whether those conditions
travel with it, and whether they are still current at the decision point.

For test and validation results, **a green must carry what it verified** in one result record: exact SHA,
profile, discovered count, selected count, executed count, filtered/skipped count, failure count, and the
declared per-node coverage obligations. A full green requires the full profile, nonzero execution, satisfied
coverage obligations, and zero failures. A bare `filtered == 0` predicate is not completeness: legitimate
suites can filter tests (693 in one measured full run), while an incomplete discovery set can report zero
filtered. A partial-profile `PASS` row is not a full green, and `test result: ok` with zero executed tests is a
no-result, not success. Keep these qualifications together at the ledger-write point so no downstream reader
can pair a bare `PASS` with inferred coverage.

## Both-Sides Bracketing

Verification must bracket guarded behavior from both sides. **Negative:** plant the violating case and
confirm **refusal** (proves the mechanism is not permissive). **Positive:** plant the genuine qualifying
case and confirm it **fires** (proves the mechanism is not inert). Neither alone is verification: a guard
that refuses everything passes every negative test. State the counts on both sides. PR #1468 is the model:
9 cells / 18 executions remained eligible with zero fallback and zero trusted-native sites, while the
`random-device` negative was rejected with 66 trusted-native sites.

Do **not** plant an artifact that is itself an authorization. Hand-adding a merge/review/validation label,
dispatching a workflow that can auto-merge, or arming another live gate tests by creating the hazard. Exercise
the consumer with an inert fixture, a dry-run/read-only mode, or an isolated test repository; the negative
control must be incapable of authorizing the action whose refusal it tests.

## The Twelve Worked Examples

A check fails when it keys on a correlated proxy without an observable identity, causal, coverage, or
provenance link to the claimed condition. Reviewers name the claimed fact, the observed evidence, the
conditions under which it was measured, and the binding between them; passing tests do not supply a missing
binding. The current twelve worked examples are:

1. A `locally-validated` label with no exact-head ledger record: the label is a cache, not the source of truth.
2. A merge gate that authorizes on bare label presence without reading the ledger.
3. `workflow_dispatch` running the PR branch's older YAML, allowing a weaker historical gate to emit the same green.
4. `is-ancestor <PR head>` encoding a merge-commit model under rebase-merge, where the PR head is never ancestral to replayed main (it undercounted landings by 33); use `mergeCommit.oid` after a fresh fetch.
5. A pin checker walking `Cargo.toml` but not tracked `Cargo.lock`, reporting consistency over an incomplete file set.
6. A green result with no executed count: success is not bound to any work having run.
7. `filtered == 0` used as completeness although 693 filters are legitimate and an incomplete discovery set can also report zero.
8. A `parity%` derived from piped-stdout SHA-256 but presented as full INFO + detlog-stack + detlog-heap parity.
9. `ACTIVE.md` naming a branch the slot does not hold, while reconciliation passes by comparing row counts rather than row contents.
10. `--cgroups` accepted by a CLI but producing no cgroup behavior or typed acknowledgement.
11. A cancelled run classified as red: a no-result is rendered as a result.
12. Dispatch boilerplate listing `commit` as destructive, causing agents to withhold the durable handoff the protocol requires.

Earlier marker-substring, error-string, partial-backend, rendered-SIGPIPE, and unqualified-memory-cap cases are
the same class. In each case ask: **what binds this signal to the fact it claims, and can I observe that
binding rather than infer it?**

## Mechanical Enforcement, Split By Layer

Mechanical enforcement is deliberately split by layer:

- **Source/config lint:** reject representations whose missing qualification is syntactically observable.
  Of the twelve examples above, only **3/12** are source/config-lintable without pretending to understand
  runtime semantics: #2 can forbid a label-presence authorization branch, #4 can forbid PR-head ancestry at
  the typed landed-identity boundary, and #12 can reject `commit` in the destructive-operation list of the
  dispatch template that owns it. The existing Rust error-string proxy lint covers **0/12** of this new
  catalogue; it correctly covers an earlier syntactic instance and must not claim more. These checks prove
  only that a known bad representation is absent, not that the replacement binding is truthful or sufficient.
- **Runtime/result checks:** require one ledger record carrying run ID, exact SHA, durable log, profile,
  discovered/selected/executed/filtered counts, failures, and declared coverage obligations; mechanically
  reject full green unless the profile is full, execution is nonzero, coverage obligations are satisfied,
  and failures are zero. This layer catches #1, #3, #6, #9, #10, and #11 with ledger/provenance/content/
  behavioral/classifier checks. Require `mergeCommit.oid` ancestry after a fresh fetch behind landed. A
  planted stale `Cargo.lock` fixture can regression-test the known half of #5, but it cannot prove that every
  future relevant file is in the checker's universe. These are evidence validators and contract tests, not
  source lint.
- **Semantic review:** determine whether a marker is causally bound, a file/backend/gate registry is
  complete (#5), coverage obligations actually define the intended suite (#7), and a parity artifact covers
  the full claimed trace (#8). It must also establish workflow/registry freshness, behavioral currency, and
  causal validity even where a mechanical detector exists. Perfect counts over an incomplete discovery set
  remain a proxy. No general lint can infer these facts. Do not stretch a syntactic lint to claim coverage of
  them; a lint claiming all twelve would itself fail Proxy Binding.
