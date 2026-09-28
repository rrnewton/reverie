# Consolidated PR planning and landing process

**Status:** canonical workspace procedure

**Owner:** coordinator

**Task:** `pr-planning-process-consolidation-drain-as-testcase`

**Last reconciled:** 2026-08-05

This document is the single end-to-end procedure for turning an open-PR backlog into an
evidence-bound landing plan and then executing that plan. It replaces the competing operational
procedures formerly spread across landing skills and the July planning synthesis. It does not copy
the planner's CLI reference or the lander's implementation details.

## Authority boundaries

| Authority | Owns | Does not own |
| --- | --- | --- |
| `agent-utils/skills/pr-landing-planner/SKILL.md` | Agent trigger and planner evidence contract | Workspace dispatch or landing mutations |
| `pr-landing-planner --userguide` | CLI flags and emitted JSON schema | Authorization to merge |
| This document | Fetch-to-closure sequence, lane selection, required evidence, and metrics | Planner implementation or merge mechanics |
| `ci-hub/landing/land-pr.sh` | Locking, fresh-head checks, receipt verification, `--rebase` merge, ancestry verification | Batch selection |
| `AGENTS.md` | Review, publication, task closure, and repository policy | Tool-specific CLI syntax |

The discoverable `hermit-lander` role and the historical landing-mechanics alias are pointers only.
Do not add substantive planning rules to them. When a rule changes, change its owning authority and
then reconcile this document.

## Non-negotiable predicates

1. **Live planning requires live egress.** If the host cannot fetch GitHub, stop, record the exact
   transport failure on the task, and do not present cached refs as a current plan. Offline fixture
   runs validate the process shape only.
2. **Fetch once per planning snapshot.** Use one planner invocation, which bulk-fetches the base and
   selected PR heads in one round trip. Do not build a per-PR fetch loop. All merge-tree, ancestry,
   changed-file, and freshness probes run locally after that fetch.
3. **Bind the plan to identity.** The fetched head must equal the API-reported head. The planner's
   content-identity guard aborts on drift; rerun instead of mixing snapshots.
4. **Use both conflict authorities.** `conflict_edges` records real merge-tree conflicts.
   `mechanism_overlap_edges` records semantic overlap after normalization to stable mechanism slugs.
   Neither substitutes for the other.
5. **Keep the look-ahead surface.** `file_overlap_edges` is the conservative surface for pairs that
   merge today but may interact after an earlier landing. `unclassified_mechanism_candidates` is the
   analogous signal that the mechanism enum needs extension. They request review; they are not
   proof of a conflict.
6. **Use the canonical validation policy.** Skill `hermit-validation-authority` alone defines
   validation evidence, label derivation, hard/soft green, landability, and the
   local-versus-GitHub boundary. This process must not retype those predicates.
7. **Separate validation from policy approval.** Routine `ci-hygiene` may proceed autonomously.
   `gate-policy` always escalates, even when tests are green. For every policy-changing PR, compare
   its rationale date with current policy; a stale rationale requires coordinator review.
8. **Adversarial review is the default.** Missing review evidence is not approval. Resolve findings
   before landing unless a documented policy exemption applies.
9. **Execute only through the tracked lander.** Merge with `--rebase`, never `--admin`. Serialize
   landings under the landing lock.
10. **Landing is an ancestry fact.** After a fresh destination fetch, prove the merge commit is an
    ancestor of the named target branch. An API `MERGED` flag, a successful merge command, a label,
    or a PR-head SHA is not landing evidence.

## The one pipeline

```text
SNAPSHOT ONCE
  -> DERIVE mechanism candidates
  -> CLASSIFY into stable mechanisms + CI/evidence/policy classes
  -> CLUSTER on real conflicts and mechanism overlap
  -> EMIT ordered JSON with agents, actions, both conflict maps, and look-ahead
  -> ARCHIVE the exact plan
  -> SELECT fresh-flow or stale-drain execution
  -> DISPATCH with exact heads and archive path
  -> REVIEW + VALIDATE/REBASE as required
  -> LAND through the serialized executor
  -> FETCH + ANCESTRY-VERIFY
  -> CLOSE through the verified task gateway
```

### 1. Prepare caller-owned context

Before the live run, construct a landing-context JSON/YAML file. Each entry carries facts GitHub
cannot derive:

```json
{
  "prs": [
    {
      "pr": 123,
      "head_sha": "<exact-40-hex>",
      "validation_evidence": "clean-validate-record",
      "policy_class": "ci-hygiene",
      "assigned_agent": "hermit-ci",
      "review_objections_resolved": false,
      "review_evidence_digest": null
    }
  ]
}
```

Allowed evidence classes and their semantics come from the canonical skill/user guide. Never copy
an old branch name or label into `head_sha`. Mechanism labels use the same stable
`mechanism:<lowercase-hyphenated-slug>` on the task and PR. Diff-derived candidates without a known
mapping remain explicitly unclassified.

Set `review_objections_resolved` to `true` only after the repository review authority accepts a
canonical `CHANGES-REQUESTED-WITHDRAWN-AT` record carrying `RETIRES <exact refusing comment id>`
from a GitHub actor whose current repository permission is `triage`, `write`, `maintain`, or
`admin`. The immutable event author must match the disclosure identity and any `BY` identity;
the disclosure's role text is audit metadata only. The withdrawal must use the refusal's lane,
bind the current exact `head_sha`, and be strictly later than the current version of the refusal;
naming the old refused head is stale and does not resolve the current head.

Obtain `review_evidence_digest` mechanically from `nodes[].review_evidence_digest` in an
uncontexted exact-head `plan --format json`, then copy that exact digest beside the same
`head_sha` into the caller-owned context. The review authority decides whether the objection was
resolved; the digest only binds that decision to the complete native-review, issue-comment, and
inline-comment snapshot it evaluated, including the exact retirement actor's current repository
permission. Any review event, body, state, author, head, or permission change makes the context
stale; unavailable permission evidence makes the snapshot unavailable rather than preserving an
older authorization. This fact suppresses only GitHub's aggregate `CHANGES_REQUESTED` hold, which
can remain stale after the exact objection is resolved. It does not supply an approval, satisfy a
required review, or weaken any validation, conflict, or exact-head check.

### 2. Produce and archive one plan

Run from the parent workspace, after verifying the relevant primary is clean and on `main`:

```bash
with-proxy agent-utils/py/bin/pr-landing-planner plan \
  --repo rrnewton/hermit \
  --base main \
  --git-dir "$PWD/hermit" \
  --net-wrapper with-proxy \
  --landing-context ignored/landing-context.json \
  --archive-dir ignored/pr-landing-plans \
  --format json > ignored/pr-landing-plan.json
```

Use the same invocation for Reverie with its repository and primary. The planner performs one light
PR-list query, bounded rollup enrichment, and one bulk Git fetch for the base plus all selected heads.
Everything expensive after that is local. A host error, identity mismatch, missing archive, or
nonzero planner exit means **no plan**.

The current JSON schema carries the base branch name but not the fetched base object ID. Until that
schema gap is closed, record the planner's deterministic local base ref beside the archive:

```bash
planner_base_ref="refs/pr-landing-planner/base-$(printf %s main | sha256sum | cut -c1-16)"
git -C hermit rev-parse "$planner_base_ref" > ignored/pr-landing-plan.base-sha
```

Treat the JSON plus this adjacent exact base-SHA record as one plan artifact. Do not substitute the
possibly different `origin/main` ref after collection.

Fixture-only validation is deliberately network-free:

```bash
agent-utils/py/bin/pr-landing-planner quickstart --emit-demo > /tmp/pr-landing-demo.yaml
agent-utils/py/bin/pr-landing-planner plan \
  --fixture /tmp/pr-landing-demo.yaml --format json > /tmp/pr-landing-demo.json
```

### 3. Audit the emitted record before dispatch

The archived JSON plus its adjacent base-SHA record is the handoff authority. Confirm it contains:

- the repository/base branch, adjacent exact fetched base SHA, and exact fetched `head` on every
  node (a live head is 40 hex; symbolic fixture heads are test data only);
- ordered `per_pr_actions`, `assigned_agent`, evidence class, policy class, held reasons, and stacks;
- `conflict_edges` (real merge-tree conflict map);
- `mechanism_overlap_edges` (normalized semantic conflict map);
- `file_overlap_edges` plus `unclassified_mechanism_candidates` (look-ahead surface);
- `parallel_safe_groups`, conflict clusters, and rebase/validation economics;
- CI diagnostics, including real red, flaky, stale-gate, evaluate-once race, and outage classes.

Reject a plan if a required field is inferred from prose rather than carried in the JSON. A
mechanism overlap requires coordinator review even when the pair has no file conflict. A file overlap
is look-ahead, not a reason by itself to claim a merge-tree conflict.

### 4. Select one execution mode from the same plan

The planner is shared; only dispatch differs.

Before any receipt-producing validation in either mode, rebase onto the freshly
fetched target tip and pass `ci-hub/validate/preflight_validate.py`. The
admission command refreshes `origin/main` itself and mechanically requires:

```text
git merge-base --is-ancestor <fresh origin/main tip> <exact validation head>
```

It also retains the fixed producer/merge-gate floor checks. A stale head is
refused before a validation slot or ledger write; a fetch/identity error fails
closed. The sole historical exception is differential debugging after the
rebased head fails. That comparison is not qualifying validation and must not
mint, copy, or reuse landing evidence.

#### `fresh-flow`

Use for the moving, newly-created pool. Optimize for landing rate at least equal to production rate.

1. Take the front eligible PR from the ordered plan.
2. Rebase it onto fetched current `main`, then classify the result under skill
   `hermit-validation-authority`; this process does not redefine soft inheritance.
3. Resolve adversarial review, qualify validation under skill
   `hermit-validation-authority`, and land serially.
4. Fetch and ancestry-verify before advancing to the next PR.

Do not hold fresh work merely to form a future cluster; the population moves while waiting.

#### `stale-drain`

Use for a finite stale snapshot whose conflict components are stable. Optimize for shrinkage of that
exact population.

1. Freeze membership by PR number and exact head SHA in the archived plan.
2. Treat real conflict connected-components as candidate stack/staging units. Mechanism edges can
   impose additional ordering or coordinator review even across distinct components.
3. Land conflict-free singletons and components smaller than three serially unless the plan shows a
   stronger dependency.
4. For a component of three or more, the coordinator may approve a staging/stack landing when every
   member is individually eligible. Resolve shared append-only registry files once with the tracked
   union mechanism; do not hand-union derived JSON.
5. Validate the exact staging head. A red staging result is not automatically attributable to one
   member; use the DAG's named failing cell, then bounded bisect when attribution is ambiguous.
6. Prove each constituent independently. A staging merge does not prove a non-ancestral PR head was
   included, and it does not authorize closing that PR.
7. **Land clusters as they ripen.** As soon as one independently authorized component has a current
   validation green accepted by `hermit-validation-authority` and an executable landing path, land it; do not wait for sibling components or
   the entire frozen backlog. Fetch the resulting main, invalidate the old plan frontier, and rebuild
   the remaining components against that new base. Preserve explicit ordering boundaries such as
   patching-backend-last, but never turn them into a barrier for unrelated ready clusters.

The typed branch-construction, shared-evidence, and constituent-verification contract is specified in
https://github.com/rrnewton/dev-hermit/blob/main/ai_docs/staging-batch-o1-drain-design-20260805.md.
It requires a batch manifest and an atomic topology-preserving landing (or an equivalent typed replay
verifier). The current single-PR rebase-only executor does not supply that constituent proof; do not
execute a staging landing until the batch executor and closure authority are explicitly approved.

Known common-cause infrastructure failures gate both modes. Fix or explicitly hold the common cause
before spending repeated validation runs; do not classify a zero-test/no-result row as a product red.

### 5. Dispatch from the archived plan

For every assigned PR, post a dispatch comment on the pull request, and set its `Owner:` line to the
assigned agent (see the parent `AGENTS.md`, "Task lists, pull-request ownership, and durable records").
The comment contains:

```text
FROM <planning-task>: plan=<durable archive path> repo=<owner/repo>
base=<40-hex> pr=<N> head=<40-hex> action=<per_pr_action>
cluster=<stable descriptive slug> assigned_agent=<agent>
conflicts=<edge ids/paths> mechanisms=<slugs> lookahead=<paths/candidates>
next=<concrete review/rebase/validate/land action>
```

The pull request is the durable channel. Under ORC, the dispatch task may simply point at that
comment. A chat/send attempt is not delivery acknowledgement. Re-plan when any head or the base
changes; do not patch the old JSON by hand.

### 6. Execute and verify

Use `ci-hub/landing/land-pr.sh` for an approved Hermit landing. The executor owns lock acquisition,
fresh-head verification, receipt dereferencing, rebase merge, and ancestry proof. Do not reproduce
those steps in an ad-hoc shell sequence.

After the executor reports success:

1. fetch the named target branch freshly;
2. obtain the actual merge/replay commit OID;
3. run `git merge-base --is-ancestor <merge-oid> origin/main` and require rc 0;
4. record the merge OID and target tip in a comment on the pull request;
5. under ORC, close the ORC task through `ci-hub/bin/close-task`, which re-verifies the landing.
   Agents that do not use TaskGraph skip this step.

## Required outcome metrics

Report measurements from the archived plan and verified target, never impressions:

| Metric | Binding definition |
| --- | --- |
| `prs_landed` | Count of merge/replay OIDs proven ancestors of freshly fetched target main |
| `conflicts_caught_before_landing` | Count of `conflict_edges` emitted before any mutation |
| `mechanism_overlaps_reviewed` | Count of normalized mechanism edges with a recorded disposition |
| `lookahead_pairs_reviewed` | File-overlap/unclassified pairs reviewed; never relabel as real conflicts |
| `rebases_avoided_by_clustering` | Planner's `sum(component_size - 1)` counterfactual, labeled as such |
| `validate_runs_avoided_by_clustering` | Same SHA-keyed counterfactual; do not present as observed runtime savings |
| `no_result` | Rows with zero execution, missing coverage, cancellation, or admission refusal |

Separate **observed** outcomes from planner counterfactuals. `land_now=[]` can be a correct result when
all candidates are red, pending, held, or lack qualifying evidence under
`hermit-validation-authority`.

## Stop conditions

Stop and write a task note when any of these occurs:

- egress/fetch fails;
- the primary is dirty or not on `main` and the dirt is not attributed;
- fetched/API head identity differs;
- the validation authority selected by `hermit-validation-authority` refuses or is unknown;
- a `gate-policy` PR lacks current rationale/approval;
- adversarial findings are unresolved;
- mechanism candidates are unclassified or a mechanism overlap lacks disposition;
- a merge conflict changes reviewed content without renewed review/validation;
- the plan cannot be archived;
- the adjacent fetched base-SHA record is missing or ambiguous;
- the tracked executor still requires stale merge-gate state despite qualifying validation evidence
  (file an implementation mismatch; never bypass it with `--admin`);
- ancestry proof fails.

## Live-drain acceptance status

The 2026-08-04 historical run is preserved at
https://github.com/rrnewton/dev-hermit/blob/main/experiments/pr-drain-planning_20260804/ACCEPTANCE.md.
It demonstrated the planner pipeline and recorded 310 pre-landing conflict edges and a 44-rebase
counterfactual, but it did not prove clustered landings (`prs_landed=0`). Those figures are historical
evidence, not the current frontier.

The required current live-drain rerun is **deferred while GitHub egress returns box-wide HTTP 403**.
When egress returns, rerun the complete pipeline from a fresh snapshot and report all metrics above,
including ancestry-verified landings. Until then, the process document is implemented but the live
acceptance half remains open.
