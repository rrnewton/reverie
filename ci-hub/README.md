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
./ci-hub/ci-hub runner-health --all

# Launch one detached full validate through the sole admission point. The user
# service runs ci-hub validate-lock before scripts/validate.rs and writes a durable log.
# A ci-hub-owned observer tab appears in the validate-hermit Herdr workspace.
./ci-hub/ci-hub validate-run --checkout worktrees/slots/slot01/hermit \
  --agent hermit-example --target aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --pr 123 --dry-run -- full

# A live launch prints its validate-* handle before blocking. A replacement
# caller can resume the wait without relaunching or changing the run:
./ci-hub/ci-hub validate-run --attach validate-hermit-example-aaaaaaaaaaaa
```

## Compatibility website

The website command derives its data from the canonical ledger, series, and
manifest inputs. It does not read the generated scorecard as authority. A build
prints one canonical JSON result naming the immutable digest directory. Serving
validates that complete directory before opening its already-validated directory
descriptor in the loopback server. With no subcommand, the command builds or
reuses the exact output, reports its identities on stderr, and then serves it.

```bash
./ci-hub/ci-hub compatibility-website build
./ci-hub/ci-hub compatibility-website serve --root /absolute/path/to/dev-hermit-compatibility-website/FRESHNESS_SHA256
./ci-hub/ci-hub compatibility-website

# Build one immutable historical snapshot from published ledger and series bytes.
./ci-hub/ci-hub compatibility-website build-historical \
  --parent /absolute/path/to/dev-hermit \
  --parent-commit 5bfa5d91f56e380631a549bf8dcf58631c97dabd \
  --record-id devbig014-1788153725-3415639 \
  --run-id validate-mega-lander-69b08cd09a60-1788152123591813608-3265480-d6eafa4c \
  --manifest-hermit /absolute/path/to/hermit-pr2880 \
  --manifest-commit bc04aa922e806e99222ee03ac81c87495be41f20
```

The `build` result's `path` field is the absolute value to pass to
`serve --root`; the placeholder above is not a relative path.

The default output root is the deterministic sibling
`../CHECKOUT-NAME-compatibility-website`. Use `--output-root` to select another
directory outside every source checkout. The output root and its immediate
parent must be owned by the current user and must not be group- or
other-writable; use a user-owned directory beneath `/tmp`, not `/tmp` itself.

A build refusal writes no stdout and never serves an older directory. Published
digest directories use read-only files and non-writable directories and are not
deleted when later inputs produce a different digest. Reuse and serving reject
mode or content changes. The same local user or root can deliberately change
permissions; that is a host trust limit, not an accepted artifact. Builds also
trust the local executable search path, Rust/Python toolchains, and their caches;
ordinary tracked-source changes are detected, but this is not a hermetic defense
against a malicious same-user process changing those dependencies between checks.
Failed or interrupted builds and same-digest publication losers deliberately
retain their private `.DIGEST.build-*` directories at mode 0700 and print a
warning. They are not published artifacts; this command performs no automatic
deletion because it cannot safely prove ownership through every failure race.
When invoked through ci-hub, explicit `build` reports its standard cost on
stderr. Long-running `serve` and default `build-and-serve` do not add wrapper
cost lines to their stream contracts. The generated pages keep cells in the manifest distinct
from cells selected by each named validate path; no selection aggregate without
its validate path is introduced.

`build-historical` is a separate, fail-closed path. Its `--parent-commit` must
be a full commit, not a moving branch or date. It copies only the tracked
published ledger, ledger-recovery, and series bytes at that commit; it does not
read live or retained spools. One bounded
`compatibility-website-projection` invocation deserializes the exact captured
manifest, HistoryRow JSONL, and SeriesRow JSONL through their shared Rust types.
That Rust boundary applies correction semantics, requires the named
`--record-id` and `--run-id` to identify one unique terminal row, decides
projection and product results, and emits the deliberately public shape.
Python packages that opaque validated result; it does not define or select any
of those source schemas. The manifest comes from the separate clean checkout
and exact `--manifest-commit`; that commit is not represented as the commit
measured by the named run.

The historical reader records the clean Hermit checkout that actually ran the
export separately from the Hermit gitlink recorded by the generator's own base
commit. Those commits may differ only on this historical path; the ordinary
live build still requires them to match. The historical build binds the actual
checkout to the manifest-source checkout, binds the gitlink to the exact
generator commit in the source provenance, and retains the older ledger commit
and its Hermit gitlink separately. It publishes both generator-side Hermit
commits plus the exact reader-file and source-identity digests. It refuses a
missing, dirty, or differently identified actual checkout.

Every generated historical page carries a collapsed disclosure titled **Full
snapshot provenance (YYYY-MM-DD HH:MM ET)**. Its timestamp is the selected
validation row's required UTC RFC3339 `finished_at`, converted with the IANA
`America/New_York` time zone; it is not the website generation time. The
disclosure names the ledger ref and resolved commit, source row and run,
validate path, exact cell count selected by full, measured Hermit commit,
manifest-source commit, actual generator Hermit checkout, and
generator-base-recorded Hermit gitlink, including which commits differ. The
visible full ledger commit links to `rrnewton/dev-hermit`; the visible full
measured, manifest-source, and generator-base-recorded Hermit commits link to
`rrnewton/hermit`. It also displays whether the generator parent sources were
committed or uncommitted/dirty and their exact source-byte digests. Cells in the
manifest at the manifest-source commit remain distinct from cells selected by
the named validate path and run. The retained-run count is labeled with that
path and remains separate from the manifest-source selected-by-full count and
manifest denominator. Use
`build-historical-and-serve` with the same source arguments plus `--port` to
serve the validated result immediately. The ordinary `build` and
`build-and-serve` commands keep their live checkout and current-main guards.

Older validation rows may omit `profile` or `selection_mode`. The Rust
projection records each such absence as an incomplete-data entry bound to the
exact `history_row_sha256`; Stage 1 retains that binding through reduction. A
run page displays **Unavailable** only when the matching row-and-field entry is
present, and links that display to the entry on the Incomplete data page. It
does not infer `full` or any other validate path from a neighboring field.

`validate-run` and the inner `validate-lock` call the same admission mechanism.
To deliberately repeat an older Hermit SHA after main advances, add
`--frozen-validate` before `--`. That path still checks the immutable validation
floors and box-exclusive lock, forces a real uncached run, and records the result
outside the canonical receipt ledger with `qualifying_receipt=false` and the
freshly observed current-main SHA. It runs the current validation driver against
the historical checkout, so a target that predates frozen-holder admission can
still be measured without changing the target tree. Ordinary validation remains
freshness-gated.
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
authority and enforces the configured validation-slot capacity.

Stop the unit, not the watcher: `./ci-hub/ci-hub validate-stop --unit
validate-NAME.service --reason 'why this run must stop'` targets one run, while
`./ci-hub/ci-hub validate-stop --all --reason 'why these runs must stop'`
enumerates every active `validate-*` user service or scope. Before it signals
anything, the command requires each active unit to have a current durable run
handle and records the resolved agent identity and stated reason there. A
legacy scope without that handle therefore refuses the whole selection before
any unit is stopped. The command then
uses `systemctl --user stop` on each exact unit, confirms the unit became
inactive, and marks that request confirmed. `validate-status` renders the
retained record as `terminated-by`; a missing or unreadable handle refuses the
stop rather than producing another unattributed termination. The signal-aware
Rust driver records an operator stop that reaches its own handler as
`no_result`, never as a product failure.

To see current and recent validation runs together, including the exact head,
slot, log path, and start time, run:

```bash
./ci-hub/ci-hub validate-status --current-main
```

This fetches main before comparing exact SHAs. An older commit on main does not
count as a validation of the current main tip. The report reads ci-hub's durable
run records twice before it says no current-main validation is in progress. A
`preparing`, `launching`, or `running` handle without a process/cgroup identity
that can still be verified is reported with its exact state and
`process_verified=false`; the current-main answer is `unknown`, never `yes` or
`no`. Unreadable run records also keep the answer `unknown`. The default output
reports their count without printing every path and parse error; use
`validate-status --current-main --show-unreadable-handles` for those individual
details. When `DG_AGENT_NAME` is a valid agent identity, the unknown result also
prints a complete read-only `validate-lock admission-status` command for an
unverified handle. Without that identity, it prints the agent-free
`validate-lock status` command and keeps the handle's state, unit, head, slot,
log, and start time in the report. When unreadable records and an unverified
handle both prevent a yes/no answer, the output gives both applicable commands.

The periodic integration-tip
check uses the same two run-record reads with the exact-target validate-lock
observation between them. It suppresses a duplicate while an unverified handle
remains, and reports `in-progress` only for a verified process or an exact target
held by the validate-lock queue or slot.

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
When that record says `landing=landed`, the same operation preserves the task's
existing tags, adds `landed`, verifies the tag readback, and only then closes
the task. `landing=implemented-unlanded`, `landing=n/a`, and
`landing=superseded` do not add it. This makes the tag a derived index of the
verified marker rather than a second fact an agent must remember to record.

If a TaskGraph write interrupts that sequence after the closure record is
stored, rerun the exact same `close-task` command. It re-verifies the evidence
and gates, reuses only a byte-identical existing record, and retries the missing
tag or status update without duplicating the record. `--check-only` previews
that recovery without writing. A raw `tg update --status closed` is not a
supported recovery path and remains refused by the closure guard.

The gateway closes only a landed code reference, a URL that resolves, a local
artifact tracked on freshly fetched parent `main`, or a GitHub Actions run ID
that resolves. Local artifact evidence is recorded as the typed tuple
`rrnewton/dev-hermit:path@last-content-commit;target=main@tip`; the gateway
verifies that content commit is ancestral to the fetched target. This proves
publication, not that the artifact answers the task's goal, which the
coordinator checks separately. `REFUSED` exits 1 for a reference known not to satisfy the criterion;
`UNVERIFIABLE` exits 2 when no answer can be obtained.
Neither nonzero state calls `tg`. Use `--check-only` to validate evidence without
mutating a live task. The upstream `tg` binary has no project hook, so project
policy requires this gateway and forbids raw terminal-status updates.

## Recording completed reviews

`ci-hub review-attest` records an exact-head verdict comment. Pass the full
comment URL and the outcome; the command verifies that the comment belongs to
the selected pull request, that its immutable GitHub actor still has the
required repository permission, and that it contains exactly one canonical
verdict for the selected family and current head:

```text
ci-hub review-attest --repo rrnewton/hermit --pr 1234 \
  --head 40_HEX_HEAD --family codex --outcome approval \
  --comment-url https://github.com/rrnewton/hermit/pull/1234#issuecomment-5678 \
  --task review-task-id --round 1
```

`--reviewer`, `--who`, and `--team` are optional audit metadata. When supplied,
they are recorded as unverified metadata and do not grant or deny authority.
Absent, stale, mismatched, or same-process fleet names therefore do not prevent
recording an otherwise valid review. A moved head still requires a fresh review,
and an unauthorized GitHub actor, malformed marker, or unresolved substantive
objection still blocks.
Agent-utils PR #117 owns normalization of that optional metadata in its callers;
this repository's authority depends only on the canonical review artifact and
the immutable GitHub actor's current repository permission.

The canonical exact-head comment is the authority. Review labels are a cache
that `review-attest` reconciles after verifying the comment; a label without the
comment does not establish approval.

Approval and refusal both apply the numbered `adversarial-review-<family><N>`
activity label. Approval additionally applies `passed-review-<family>`; refusal
removes that approval label if it is present. The accepted families, rounds,
and exact label spellings live in `ci-hub/review_contract.py`, which both the
writer and the status reader import. Before any mutation, the writer also
requires every contract label to exist in both supported repositories.

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

The outer ORC workflow calls `bin/health-tick` every thirty minutes. The pinned
tick engine reads `health/tick-hub.yaml`; dev-hermit probes live beside it.

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

## Why this exists

The [2026-08-03 skills audit](../ai_docs/transient/2026-08-03-skills-audit.md#where-are-the-ci-health-skills)
found validation and GH Actions knowledge fragmented across the coordinator `hermit-ci` charter,
Hermit's debugging workflow, five dated state skills, standalone scripts, and
an ORC-only poll registration. Skills now point here for live commands and
ownership. Skills describe when/how an agent should act; this hub owns the
actual code, current query paths, state schema, and operator entrypoints.
