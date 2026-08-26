# dev-hermit validation and GH Actions hub

**Follow skill `hermit-validation-authority` for validation authority.**

`ci-hub/` is the stable tool name and the single versioned home for dev-hermit
VALIDATE, LEDGER, GH Actions, and runner operations. Treat each subdirectory as
an object with one responsibility and a stable public entrypoint; do not add new automation scripts under `scripts/`, `ops/`, or
an experiment directory.

Changes to this live infrastructure are built and tested in a fresh disposable
clone, never edited in the shared primary checkout. Follow
[`../ai_docs/ci-hub-change-clone-protocol.md`](../ai_docs/ci-hub-change-clone-protocol.md);
publication is one sealed commit through `scripts/parent-main-write`, followed
by coordinator-owned activation in the primary.

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

# Pull fresh open-PR GH Actions state from GitHub and classify it.
./ci-hub/ci-hub fresh

# Summarize current-main plus open-PR health.
./ci-hub/ci-hub health

# Parse the canonical landing command without mutating a PR. Real lands use the
# same entrypoint without CI_HUB_DOCS_PARSE_ONLY; its validation-authority
# selection follows skill hermit-validation-authority.
CI_HUB_DOCS_PARSE_ONLY=1 ./ci-hub/landing/land-pr.sh \
  123 example/feature-branch --foreground

# Inspect or recover polling for obligations that are still open.
./ci-hub/ci-hub obligations
./ci-hub/ci-hub obligations --actionable
./ci-hub/ci-hub inherit-obligations --agent hermit-lander --session "$HOSTNAME:$$"
./ci-hub/ci-hub watch-obligations --once

# Start GitHub CI BY HAND on a chosen commit, and read the result. NOTHING on
# GitHub starts by itself any more: owner directive 2026-08-21 keeps CI "disabled
# in the sense that it does not run automatically", so every workflow in this
# repository is workflow_dispatch and nothing else, and this is the only way in.
# It reports; it gates nothing (owner directive 2026-08-17).
./ci-hub/bin/manual-run --list                 # triggers per workflow; names any automatic one
./ci-hub/bin/manual-run --workflow portability.yml --wait
./ci-hub/bin/manual-run --commit b540dbec9585 --workflow dev-hermit-ci.yml --wait
./ci-hub/bin/manual-run --repo rrnewton/hermit --list
# A commit that is not the branch tip is dispatched on a `manual-run/<sha12>` tag
# the tool creates, because the dispatch API takes a branch or a tag, never a SHA.
# It refuses while the repository Actions switch is off: a dispatch then returns
# SUCCESS and creates a run with ZERO JOBS that never runs and can never be
# cancelled or deleted.

# Incrementally refresh the local GitHub/local-run history store.
./ci-hub/ci-hub refresh-history

# Query the local commit, VALIDATE, and GH Actions history store.
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

`validate-run` and the inner `validate-lock` call the same admission mechanism.
To deliberately repeat an older Hermit SHA after main advances, add
`--frozen-validate` before `--`. That path still checks the immutable validation
floors and box-exclusive lock, forces a real uncached run, and records the result
outside the canonical receipt ledger with `qualifying_receipt=false` and the
freshly observed current-main SHA. Ordinary validation remains freshness-gated.
Follow skill `hermit-validation-authority` for the required base-currency check, the sole
production path, and whether any resulting evidence qualifies. This README
does not restate those decisions.

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

A validate receipt remains keyed to the exact commit it tested.
`reconcile-receipts` joins those records to current open heads; it does not
decide whether preservation across a rebase is landable. Follow skill
`hermit-validation-authority` for hard/soft-green meaning and landability.

```bash
./ci-hub/bin/reconcile-receipts          # human table
./ci-hub/bin/reconcile-receipts --json   # machine-readable
```

It joins distinct receipt commits (enumerated from `local-history`) against
freshly fetched open-PR heads and reports its exact-head match classes with a
denominator. Those classes are operational inventory, not an independent
validation policy.

- `VALID` — the current head exactly matches a receipt accepted by the semantic verifier.
- `FLOOR-BLOCKED` — matched + certified but the head predates a merge-gate or
  producer floor; it validates green yet landing is refused. Lever is REBASE.
- `NOT-CERTIFIED` — matched an open head but the authoritative certifier refuses
  (a `local-history checks==5` match is not landability proof).
- `ORPHANED` — no current open-PR head equals this commit; the head moved or the
  PR landed. `orphaned/total` is the measured cost of push-rewrites-the-head.

Run the join when an exact-head inventory is useful. Do not use its `ORPHANED`
name to conclude that hard-green evidence at X is useless after X becomes Y;
that separate landability decision belongs only to `hermit-validation-authority`.

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
| `bin/` | Stable wrappers and pinned shared-tool materialization. | Workflow classification, scheduling, or history logic. |
| `directives/` | Versioned owner tooling obligations and fresh target-branch ancestry verdicts. | Implementation, review, or treating quoted instructions as completion. |
| `health/` | Dev-hermit-specific current-main, PR, primary, and agent health adapters plus tick configuration. | Generic cadence or PR/workflow classification engines. |
| `history/` | Incremental/idempotent GitHub Actions and local VALIDATE knowledge store, ingestion, and queries. | Current-live status presentation. |
| `remediation/` | Mandatory post-land dual verification, exact-SHA local execution, watcher, and remediation recommendation. | Workflow-history ingestion or automatic source-code reverts. |
| `validate/` | Local validation ledger aggregation and linkage to retained profiles. | The generic DAG runner/profile format. |
| `runners/` | Self-hosted runner image, lifecycle, and host status tooling. | Generic workflow scheduling. |
| `landing/` | Shared-file **landing mutex** (`land-lock`) that serializes PR landings touching the shared manifest registries. | The land sequence itself (re-union/push/stamp/merge). |
| `closure/` | Verification and evidence recording required before TaskGraph closure. | Task implementation or coordinator judgement that a goal is complete. |

Runtime data is untracked under `ignored/ci-hub/`; versioned code and schemas
live here. Reproducible experiment producers and their frozen outputs remain
with their experiment, but new reusable workflow-history queries belong in
`history/`.

The `history` and `refresh-history` front-door commands fall back to the local
validate-run aggregator until the unified store is present. Once
`history/query.py` and `history/ingest.py` exist, dispatch switches to them
automatically without changing callers.

## Shared agent-utils boundary

The parent `agent-utils` path is a symlink to the nested
`hermit/agent-utils` checkout. The nested gitlink in Hermit is the authoritative
pin; the path is a hard dependency, not a source to copy or an independently
bumpable parent submodule. The single `bin/agent-tool` adapter materializes the
exact nested pin and runs:

- `pr-landing-planner`: open-PR workflow collection/classification. `health/pr_status.py`
  only combines its JSON for the Hermit and Reverie forks; the retired parent
  classifier is not duplicated here.
- `tick-hub`: cadence, gates, and stable `HEALTH`/`ACTION`/`NOTE`/`ERROR`
  emission. Dev-hermit owns only `health/tick-hub.yaml` and project probes.
- `safe-ci-dag-runner`: validation DAG execution and retained profile
  summaries. `validate/aggregate.py` links local run records to that store; it
  does not reimplement the runner or profile schema.

`validate-run` supports both `rrnewton/hermit` and `rrnewton/reverie`. Reverie
uses the same central lock and ledger format but a repository-specific
qualifier. Its full product driver is one mandatory, fail-closed safe-ci node;
there is no unboxed fallback. See `validate/README.md` and Reverie's
`reverie-validation-authority` skill for the exact row contract. The canonical
exact-head local row is Reverie's current validation authority; hosted results
remain diagnostics only under the owner's project-wide Actions-off directive.

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

## The lander restores the checkout it was handed

`landing/land-pr.sh` detaches the `--checkout` worktree at `origin/main` in
several places — to materialize an exact head for just-in-time validation, to
clean up after a conflicted rebase, and after a successful push. It now records
the caller's entry state and restores it from an `EXIT` trap, so **every** exit
puts the worktree back, including `abandon`. Restoration is best-effort and
never changes the exit code, but it warns on stderr rather than failing quietly.

⚠️ **Why this matters, and who it bit.** Before the trap existed, a failed
landing returned the worktree on `main`. The caller's next entirely ordinary
`git rebase origin/main && git push --force origin HEAD:<branch>` then rebased
main onto main, produced a no-op head, and force-pushed it **over their own
branch**. The destructive command is the agent's own, minutes later, in a
different tool — which is why three wipes in one session were each attributed to
an external actor, and why two agents and a coordinator spent time hunting a
branch-reaping automation that does not exist.

⚠️ **The exposure is the inverse of the natural guess: it only bit agents doing
everything right.** The damaging path is the just-in-time validation branch,
which runs *only* when the PR head already contains current `main`. An agent
whose branch was stale abandoned earlier, before any checkout, and was never
touched. So the agents closest to landing — freshly rebased, current, ready —
were the ones losing branches. Anyone reasoning about who was affected will
guess the opposite.

Keep a rescue ref while landing (`refs/rescue/<agent>/<name>-<sha>`): it is
append-only, costs nothing, and is what made the three wipes recoverable.

## Speculative-land obligation contract

The canonical `landing/land-pr.sh` entrypoint supports `rrnewton/hermit` by
default and explicit `--repo rrnewton/reverie`; it refuses a checkout whose
origin does not match the selected repository. It prepares the speculative-land
obligation before its bounded child can merge; raw `gh pr merge` is not the
protocol. It also dereferences the final pushed head through the repository-selected
authority immediately before a
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

1. retains local validation and hosted-observation component slots for legacy
   obligation records; current authority and whether either component may run
   are selected only by skill `hermit-validation-authority`; and
2. launches a detached watcher whose durable log lives beside the local
   validation log.

Historical obligation schemas retain their recorded workflow tuples for audit
and replay of old state. Interpret them under the current authority boundary in
the canonical skill named above.

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

Green qualification is intentionally not restated here; follow skill
`hermit-validation-authority`. Legacy obligation fields remain data, not authority.

Automatic local re-dispatch consults the owner-watch admission predicate before
claiming the obligation or spawning clone/submodule setup. A missing release
record admits the bootstrap case; a starved or indeterminate watch yields without
spending the retry; a recently admitted watch preserves the positive re-dispatch
path. This early check is deliberately not the acquisition authority:
`validate-lock run` repeats the same predicate at the box-anchor boundary, so a
watch arrival between the precheck and the claim still makes re-dispatch yield.

This operational object does not select validation authority. Follow skill
`hermit-validation-authority`; repository-specific non-Hermit policy remains separate.

A qualifying result selected by `hermit-validation-authority` satisfies the validation leg
of an obligation. Operational `pending`, `running`, `no_result`, and diagnostic
fields do not redefine that policy. A red verdict is not permission to rewrite
`main`: no automatic revert is recommended or dispatched. A revert requires a durable owner-authored record
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
found validation and GH Actions knowledge fragmented across the coordinator `hermit-ci` charter,
Hermit's debugging workflow, five dated state skills, standalone scripts, and
an ORC-only poll registration. Skills now point here for live commands and
ownership. Skills describe when/how an agent should act; this hub owns the
actual code, current query paths, state schema, and operator entrypoints.
