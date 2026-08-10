# dev-hermit CI hub

`ci-hub/` is the single versioned home for dev-hermit CI operations and CI
knowledge. Treat each subdirectory as an object with one responsibility and a
stable public entrypoint; do not add new CI scripts under `scripts/`, `ops/`, or
an experiment directory.

## Public entrypoints

Every fenced shell invocation in this README and `landing/README.md` is extracted
and exercised by the docs-as-tests check. Read-only commands execute; mutating
commands must pass their exact argument parser without performing the action.
`--help` is a purity contract: it must not write, touch tracked-file mtimes,
perform network access, or import heavy optional dependencies. The core workflows
are:

```bash
# Print the command list without network or filesystem writes.
./ci-hub/ci-hub help

# Read the opinionated 5-step agent workflow without touching local state.
./ci-hub/ci-hub quickstart

# Pull fresh open-PR CI state from GitHub and classify it.
./ci-hub/ci-hub fresh

# Summarize current-main plus open-PR health.
./ci-hub/ci-hub health

# Parse the canonical landing command without mutating a PR. Real lands use the
# same entrypoint without CI_HUB_DOCS_PARSE_ONLY; it verifies the final pushed
# head through the owner-authorized counted-local OR versioned-hosted authority
# before a head-matched rebase merge.
CI_HUB_DOCS_PARSE_ONLY=1 ./ci-hub/landing/land-pr.sh \
  123 example/feature-branch --foreground

# Inspect or recover polling for obligations that are still open.
./ci-hub/ci-hub obligations
./ci-hub/ci-hub obligations --actionable
./ci-hub/ci-hub inherit-obligations --agent hermit-lander --session "$HOSTNAME:$$"
./ci-hub/ci-hub watch-obligations --once

# Incrementally refresh the local GitHub/local-run history store.
./ci-hub/ci-hub refresh-history

# Query the local commit/CI history store.
./ci-hub/ci-hub history

# Inspect local validate-run history, retained profiles, or runner health.
./ci-hub/ci-hub local-history --since 2026-08-03
./ci-hub/ci-hub validate-worktrees --runs 10
./ci-hub/ci-hub runner-health --all

# Launch one detached full validate through the sole admission point. The user
# service runs ci-hub validate-lock before scripts/validate.rs and writes a durable log.
# A ci-hub-owned observer tab appears in the validate-hermit Herdr workspace.
./ci-hub/ci-hub validate-run --checkout worktrees/slot01/hermit \
  --agent hermit-example --target aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --pr 123 --dry-run -- full

# A live launch prints its validate-* handle before blocking. A replacement
# caller can resume the wait without relaunching or changing the run:
./ci-hub/ci-hub validate-run --attach validate-hermit-example-aaaaaaaaaaaa
```

`validate-run` and the inner `validate-lock` both call the same fail-closed
admission authority. It refreshes `origin/main`, requires that exact tip to be
an ancestor of the exact validation SHA, and checks every fixed receipt/merge
floor before `scripts/validate.rs` can start. A stale or unresolvable base is a refusal,
not a soft warning. Historical differential debugging on an older head is
non-qualifying and runs outside this receipt-producing path.

Networked commands use `with-proxy` internally.

`fresh` selects one latest check attempt per context and uses that same selected
set for both the rollup state and failing-check names. A selected failed GitHub
Actions check remains red unless a bounded lookup of its exact job binds the
repository, run ID, job ID, PR head SHA, check name/URL, timestamps, completed
failure, and the complete step list. Only a job whose sole step is failed
`Set up job` becomes typed `NO_RESULT`; it is reported as pending and never as
green. One versioned downstream contract is also recognized: the exact
`merge-gate-v4` job may become typed `NO_RESULT` only when the same selected
rollup/run contains an independently verified setup-only Reverie-pin source and
the complete seven-step gate receipt
shows only the prerequisite requirement failing while product validation stayed
skipped. The owner-approved rule is ancestry plus monotonicity: a candidate must
pin a `reverie/main` ancestor and must not regress from the landing base; tip
equality is explicitly not required. During the coordinated rename, the
authority accepts the historical `reverie-pin-is-latest-main` identity and the
accurate `reverie-pin-ancestry-and-monotonicity` identity only as exact reviewed
name/workflow-blob/step-receipt contracts. The authority also dereferences the
exact workflow run and exact-head workflow contents, requiring a reviewed v4
blob whose YAML declares the `needs.reverie-pin.result` link. This is not a
generic gate-failure carve-out.
Missing/malformed API
data, identity mismatches, later workflow steps, source/run mismatches, timeouts,
and lookup-budget exhaustion preserve the red verdict. JSON output exposes
`setup_only_no_result_checks`, `setup_only_evidence`,
`prerequisite_no_result_checks`, `prerequisite_evidence`, and any
`actions_job_verification_errors` on the affected PR.

The launch looks synchronous to its caller, but the run is owned by an
independent `validate-*` user service. Before that service starts, ci-hub creates
an observer-only tab in the `validate-hermit` Herdr workspace, titled with the
PR (or unit), exact head prefix, and start time. Pane control crosses the agent
jail through short-lived `systemd-run` broker calls. The pane tails the durable
log and observes the service's `/proc` descendants; it never runs `scripts/validate.rs`
and therefore cannot become an unboxed second producer. It prints every
observed `safe-ci-*` cgroup path and stores those paths in the durable handle at
`ignored/validate/runs/<unit>.json`.

Visibility is fail-closed: if ci-hub cannot create the Herdr observer, it does
not start the validation service. Once launched, recycling or interrupting the
waiting agent does not stop the run. A successor uses `--attach <unit>` to read
the same handle and wait for the same service; it must not relaunch. Multiple
queued services can each be visible, but `validate-lock` remains the execution
authority and currently admits one validate/bench at a time.

Stop the unit, not the watcher: `./ci-hub/ci-hub validate-stop --unit
validate-NAME.service` targets one run, while `./ci-hub/ci-hub validate-stop
--all` enumerates every active `validate-*` user service or scope. The command
refuses unrelated unit names, uses `systemctl --user stop` on each exact unit,
and fails if any unit remains active. The signal-aware Rust driver records that
operator stop as `no_result`, never as a product failure.

## Standing receipt reconciliation

A validate receipt is keyed to an exact commit SHA, so every rebase/push/mark-ready
orphans the receipt earned at the old head. Nothing else sweeps for the receipts
that are STILL valid, so earned ~500s evidence sits unread while the drain queues
fresh runs. `reconcile-receipts` is the standing join — run it after every drain
rebase wave:

```bash
./ci-hub/bin/reconcile-receipts          # human table
./ci-hub/bin/reconcile-receipts --json   # machine-readable
```

It joins every distinct clean-full receipt commit (enumerated from
`local-history`) against FRESHLY-FETCHED open-PR heads and classifies each,
always WITH A DENOMINATOR ("valid 1 of 59"):

- `VALID` — head matches, authoritatively `is_clean_full_pass` (via
  `validate-status`, NOT the looser `local-history` prefilter), and clears every
  rebase-base floor. Landable NOW: `apply-local-label` + merge, no new validate.
- `FLOOR-BLOCKED` — matched + certified but the head predates a merge-gate or
  producer floor; it validates green yet landing is refused. Lever is REBASE.
- `NOT-CERTIFIED` — matched an open head but the authoritative certifier refuses
  (a `local-history checks==5` match is not landability proof).
- `ORPHANED` — no current open-PR head equals this commit; the head moved or the
  PR landed. `orphaned/total` is the measured cost of push-rewrites-the-head.

**COORDINATOR post-wave step (standing, once per rebase wave).** There is no
per-wave code hook — a rebase wave is a coordinator-level batch, not a single
script's loop, and the natural home (`rebase_wrapper.py`) owns an overlapping
reconcile/`eligible` mechanism. So the coordinator runs this as the FIRST landing
step after every rebase wave, BEFORE queueing any new ~500s validate:

```bash
./ci-hub/bin/reconcile-receipts            # 1. sweep the moved frontier
# 2. for each VALID row: apply-local-label from the receipt, then land it
#    (ci-hub apply-local-label --pr <N> ; ci-hub/landing/land-pr.sh ...).
# 3. for each FLOOR-BLOCKED row: it needs REBASE, not a new validate.
```

A wave rewrites heads — some receipts revive, some die — so only a re-run after
each wave is current; the query costs seconds against a full run's ~500s. This is
the cheapest work in the drain: consume evidence already earned before spending
new compute.

Landing verification is PR-aware and has machine-stable result codes:

```bash
./ci-hub/ci-hub verify-landing PR --repo OWNER/REPO --source CHECKOUT
./ci-hub/ci-hub verify-landing COMMIT_OID --source CHECKOUT
./ci-hub/ci-hub verify-landing PR --repo OWNER/REPO --source CHECKOUT \
  --item "description" --claimed-oid REPORTED_OID
```

The command freshly fetches the target (default `origin/main`) and prints
`LANDED` with rc 0, `NOT_LANDED` with rc 1, or `UNVERIFIABLE` with rc 2. For a
PR it reads GitHub's `mergeCommit.oid`, the commit created by a rebase merge,
then checks that replay SHA's ancestry. Do not test the pre-merge PR head:
rebase merge rewrites it by design. The API's `MERGED` state is not sufficient
on its own; a non-ancestral replay SHA is `NOT_LANDED`, detecting a later
force-push orphan. A PR without `mergeCommit.oid` is `UNVERIFIABLE`, never an
inferred failure or success. `verify-landed-pr` remains a compatibility alias.

Commit abbreviations are expanded and reported as `full_oid`; never copy an
abbreviated OID into a landing report. Claim-audit mode prints the item, reported
OID, full OID, whether it resolves, whether the PR's `mergeCommit.oid` is present
on the fetched target, and the reported OID's own ancestry rc. Thus a pre-rebase
head can truthfully show `claimed_ancestry_rc=1` while
`change_present_on_main=true`; the landing is identified by the separate full
`merge_commit_oid`.

Task closure is a separate, fail-closed consumer of that verifier:

```bash
./ci-hub/bin/close-task TASK --code PR_OR_FULL_SHA --repo OWNER/REPO --source CHECKOUT
./ci-hub/bin/close-task TASK --artifact ai_docs/path.md
./ci-hub/bin/close-task TASK --run-id GITHUB_RUN_ID --repo OWNER/REPO
```

The gateway records `CLOSURE-VERIFIED` on the task before changing its status.
It closes only a landed code reference, a URL that resolves, a local artifact
tracked on freshly fetched parent `main`, or a GitHub Actions run ID that
resolves. Local artifact evidence is recorded as the typed tuple
`rrnewton/dev-hermit:path@last-content-commit;target=main@tip`; the gateway
verifies that content commit is ancestral to the fetched target. This proves
publication, not that the artifact answers the task's goal, which the
coordinator checks separately. `REFUSED` exits 1 for a reference known not to satisfy the criterion;
`UNVERIFIABLE` exits 2 when no answer can be obtained.
Neither nonzero state calls `tg`. Use `--check-only` to validate evidence without
mutating a live task. The upstream `tg` binary has no project hook, so project
policy requires this gateway and forbids raw terminal-status updates.

## Object map and ownership

| Object | Owns | Does not own |
| --- | --- | --- |
| `bin/` | Stable wrappers and pinned shared-tool materialization. | CI classification, scheduling, or history logic. |
| `directives/` | Versioned owner tooling obligations and fresh target-branch ancestry verdicts. | Implementation, review, or treating quoted instructions as completion. |
| `health/` | Dev-hermit-specific current-main, PR, primary, and agent health adapters plus tick configuration. | Generic cadence or PR-CI classification engines. |
| `history/` | Incremental/idempotent GitHub Actions and local commit/CI knowledge store, ingestion, and queries. | Current-live status presentation. |
| `remediation/` | Mandatory post-land dual verification, exact-SHA local execution, watcher, and remediation recommendation. | CI-history ingestion or automatic source-code reverts. |
| `validate/` | Local validation ledger aggregation and linkage to retained profiles. | The generic DAG runner/profile format. |
| `runners/` | Self-hosted runner image, lifecycle, and host status tooling. | Generic CI scheduling. |
| `landing/` | Shared-file **landing mutex** (`land-lock`) that serializes PR landings touching the shared manifest registries. | The land sequence itself (re-union/push/stamp/merge). |
| `closure/` | Verification and evidence recording required before TaskGraph closure. | Task implementation or coordinator judgement that a goal is complete. |

Runtime data is untracked under `ignored/ci-hub/`; versioned code and schemas
live here. Reproducible experiment producers and their frozen outputs remain
with their experiment, but new reusable CI-history queries belong in
`history/`.

The `history` and `refresh-history` front-door commands fall back to the local
validate-run aggregator until the unified store is present. Once
`history/query.py` and `history/ingest.py` exist, dispatch switches to them
automatically without changing callers.

## Shared agent-utils boundary

The `agent-utils` gitlink is a hard dependency, not a source to copy. The
single `bin/agent-tool` adapter materializes the exact pinned commit and runs:

- `pr-landing-planner`: open-PR CI collection/classification. `health/pr_status.py`
  only combines its JSON for the Hermit and Reverie forks; the retired parent
  classifier is not duplicated here.
- `tick-hub`: cadence, gates, and stable `HEALTH`/`ACTION`/`NOTE`/`ERROR`
  emission. Dev-hermit owns only `health/tick-hub.yaml` and project probes.
- `safe-ci-dag-runner`: validation DAG execution and retained profile
  summaries. `validate/aggregate.py` links local run records to that store; it
  does not reimplement the runner or profile schema.

Before adding code, audit the pinned `agent-utils` APIs. If the capability is
generic, add it there and link/use it here.

## Health meanings

`health` is deliberately fail-loud:

- current-main `red` returns 1; missing/unqueryable data returns 2;
- open-PR health is unhealthy for a real regression or systemic runner outage;
- stale/evaluate-once/flaky reds retain their agent-utils classification and
  are not silently relabeled as product failures;
- pending current-main work is displayed as pending and must not be claimed
  green.
- every unresolved speculative-land obligation makes `health` nonzero. A
  verification failure remains unresolved until an explicit fix-forward or
  revert is recorded with `resolve-obligation`.

The outer ORC workflow calls `bin/health-tick` every five minutes. The pinned
tick engine reads `health/tick-hub.yaml`; dev-hermit probes live beside it. A
detached per-obligation watcher records terminal state continuously, while the
dedicated ORC workflow `hermit-dev-speculative-land-remediation-v1` first
recovers any write-ahead intent interrupted after merge but before arm, then
polls every 15 seconds. A failure records durable remediation state and may send
an advisory wake to the live `hermit-lander` (or coordinator). The wake is
recorded as `sent_unacknowledged`; only a reader's `inherit-obligations` scan
changes it to `acknowledged`. A bare scan records receipt only. When the reader
has evaluated the recommendation, it records either `--action-outcome executed`
or `--action-outcome declined --reason '<durable rationale>'`; a decline without
a nonblank reason is refused. The resulting `remediation.dispatch.acknowledgement`
lives on the obligation, so a refusal cannot be mistaken for a click-through.
The five-minute tick and every fresh lander's startup scan recover from a lost
wake.

## Speculative-land obligation contract

The canonical `landing/land-pr.sh` entrypoint prepares the speculative-land
obligation before its bounded child can merge; raw `gh pr merge` is not the
protocol. It also dereferences the final pushed head through the owner-authorized
counted-local OR versioned-hosted authority immediately before a
`--match-head-commit` merge. Once GitHub exposes
the merged SHA it completes the prepared intent and calls `arm-land`. If the
wrapper dies in that interval, the restartable ORC workflow recovers the intent
and arms it. The write-ahead intent and initial OPEN event both carry the
versioned repository/workflow/job policy; an unsupported repository or a
`--source` checkout whose `origin` names another repository is refused before
merge instead of creating an obligation no verifier can satisfy. Omitting
`--source` selects the canonical `hermit/` or `reverie/` checkout from `--repo`;
Reverie never inherits Hermit's default. Arming appends that OPEN event to
`ignored/ci-hub/obligations.jsonl`, then concurrently:

1. clones the obligation's registered source repository into
   `ignored/ci-hub/obligations/<id>/hermit`, checks out the exact landed commit
   detached, runs full `./scripts/validate.rs --no-label-pr`, and dereferences the
   canonical counted exact-SHA receipt with `ci-hub validate-status`;
2. dereferences the policy-bound jobs from their exact workflow runs for the
   same SHA, dispatching the workflows only when GitHub `main` still equals
   that SHA; and
3. launches a detached watcher whose durable log lives beside the local
   validation log.

The supported GitHub bindings are explicit rather than inferred from a global
default:

| Repository | Workflow file | Workflow name | Required job |
|---|---|---|---|
| `rrnewton/hermit` | `.github/workflows/ci-portable.yml` | `CI (GitHub-managed portable)` | `Regular tests (GitHub-managed portable)` |
| `rrnewton/reverie` | `.github/workflows/ci.yml` | `Rust` | `Regular tests (GitHub-hosted)` |
| `rrnewton/reverie` | `.github/workflows/ci.yml` | `Rust` | `Host-dependent tests (self-hosted)` |

Current verification-policy schema v3 freezes Hermit's one portable job tuple
and exact positive count of one; Reverie remains on schema v2 with two required
job tuples and an exact count of two. Historical Hermit schema-v2 obligations
retain their frozen two-job policy, but new landing authority does not silently
add the privileged workflow. A workflow conclusion alone is not authority: the
consumer dereferences the exact-SHA jobs and refuses a green for a missing,
skipped, duplicate, truncated, or otherwise vacuous required set. The schema
does not snapshot the cross-source OR evaluator truth table; a future change to
those semantics therefore requires its own explicit migration and review.

This parent implementation precedes its Hermit consumer deployment. Until the
versioned `hermit-merge-gate-authority-deployment` obligation in
[`landing/README.md`](landing/README.md#deployment-obligation-hermit-merge-gate-authority-deployment)
lands as a Hermit PR, Hermit's required merge-gate remains stricter
(portable+privileged) and pins the older receipt verifier. Portable-only hosted
authority must not be reported as live end to end during that transition.

The event log is append-only and locked. Every transition retains timestamps,
verification scope (`total` here), GitHub run IDs, local log path, and failure
recommendation for the history and green-time consumers. The local verifier is
wrapped by `bin/tool-cost`; its history-derived estimate and atomic actual
wall/CPU JSON are copied into the obligation record. Stable cross-store joins
are `landed_sha → gha-runs.csv:head_sha`, `landed_sha → local-runs.csv:git_sha`,
and `github.run_ids → gha-runs.csv:run_id`.

This is a write-ahead, crash-recoverable protocol, **not an atomic transaction
with GitHub**. An OPEN row is not proof that its producers were armed. The
launcher claims an append-only token, each local runner and watcher registers
its own PID against a component token, and the intent becomes `armed` only after
both registrations are durable. Recovery audits that evidence even when the
intent already says `armed`; abandoned `pending`/`starting` claims are
idempotently resumed, while compare-and-append claims prevent concurrent
recoverers from starting duplicate producers. A merge can complete before
arming; the machine-local intent lets ORC recover that window when the same
workspace returns. Raw merges bypass the intent, and loss of the workspace/disk
loses its machine-local recovery state.

A local `scripts/validate.rs` exit code is never green authority by itself. Local green
requires the persisted canonical `validate-status --sha ... --repo ... --json`
report and its digest: `VALIDATED`, a nonzero qualifying count, full profile and
selection, a passing dereferenced receipt, and exact landed-SHA provenance.
Legacy bare greens are rechecked and become `no_result` when that authority
refuses them. Reverie's versioned policy does not enable the local leg because
its retained local evidence is not repository-bound; that leg remains
conservatively `no_result`, while its exact hosted 2/2 job set can satisfy the OR
policy.

Automatic local re-dispatch consults the owner-watch admission predicate before
claiming the obligation or spawning clone/submodule setup. A missing release
record admits the bootstrap case; a starved or indeterminate watch yields without
spending the retry; a recently admitted watch preserves the positive re-dispatch
path. This early check is deliberately not the acquisition authority:
`validate-lock run` repeats the same predicate at the box-anchor boundary, so a
watch arrival between the precheck and the claim still makes re-dispatch yield.

Parent `AGENTS.md` is authoritative under the owner's 2026-08-10 decision;
this operational document follows it. Authority is repository-specific and the
same selection governs landing and remediation. For a normal Hermit head, only a counted exact-head local ledger
result decides; hosted CI is P2 diagnostic evidence and cannot satisfy, reopen,
or actuate the obligation. A Hermit head that changes its registered local
producer is the narrow exception: local is intentionally non-authoritative and
the registered exact-head hosted job decides because a producer cannot vouch
for itself. Reverie's versioned hosted authority remains unchanged.

A green from the selected authority satisfies the obligation. `pending` and
`running` are reserved for dereferenced queued or executing producers; a missing
workflow/run/job is `no_result`. Results outside the selected authority remain
visible diagnostics, never an AND gate or mutation trigger. An authoritative
red remains loud and changes the obligation to `remediation_required`, but a red
is a verdict, not permission to rewrite `main`: no automatic revert is
recommended or dispatched. A revert requires a durable owner-authored record
bound to the exact candidate revert SHA, current-main SHA, and failed-land
identity; otherwise the obligation is explicitly awaiting that decision.
The owner record is an exact-main commit comment headed
`REMEDIATION-REVERT-AUTHORIZED`; consume it once with
`ci-hub/ci-hub authorize-revert OBLIGATION_ID` before any revert dispatch.
Outstanding work is enumerable from state alone with
`obligations --actionable`. A replacement lander acknowledges inherited work at
startup with `inherit-obligations`; no notification delivery is required. The
obligation remains visible and unhealthy even after acknowledgment, until the
repair lands and is closed with a full SHA. Resolution requires `--kind` to
match the durable triggered recommendation, freshly fetches the obligation's
repository-bound `main`, and refuses a nonexistent, wrong-repository, off-main,
or pre-failure repair. Its compare-and-append binds the event identity and full
remediation action, so a concurrent same-state change cannot inherit the proof.
A `revert` must be the failed land's direct child and restore its sole parent's
exact tree; other repairs are reviewed fix-forwards. The completed event records
the exact failed-land/repair/main ancestry and kind proof:

```bash
./ci-hub/ci-hub resolve-obligation OBLIGATION_ID --kind fix-forward --ref REPAIR_SHA
# or: --kind revert --ref REVERT_SHA
```

Completed remediations are terminal: later polls return the recorded repair
instead of re-evaluating the failed land's historical red producers. If a
watcher is nevertheless stuck, use the supported exact-identity cancellation
path rather than a PID-only or pattern signal:

```bash
./ci-hub/ci-hub cancel-watcher OBLIGATION_ID --reason "stuck after terminal remediation"
```

Watcher registration persists host, boot ID, PID, and `/proc` start ticks.
Cancellation opens a pidfd, rechecks those fields, records the request, and
sends only `SIGTERM`; it never escalates to a stronger signal. A legacy row
without that durable identity is refused. Cancellation is accepted only after
the obligation is terminal, and changes only watcher state—the satisfied or
remediated result remains exactly as before.

## Why this exists

The [2026-08-03 skills audit](../ai_docs/transient/2026-08-03-skills-audit.md#where-are-the-ci-health-skills)
found CI knowledge fragmented across the coordinator `hermit-ci` charter,
Hermit's debugging workflow, five dated state skills, standalone scripts, and
an ORC-only poll registration. Skills now point here for live commands and
ownership. Skills describe when/how an agent should act; this hub owns the
actual code, current query paths, state schema, and operator entrypoints.
