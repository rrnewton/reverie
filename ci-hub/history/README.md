# ci-hub history store

History stores observations; it does not select validation authority. Follow
skill `hermit-validation-authority` for CI mode, evidence qualification, and the
local-versus-GitHub boundary.

The **single** local accumulator of commit/CI knowledge. Do not build a parallel
one — two independently-maintained stores drift (exactly how the hand-maintained
test-footprint map rotted). Everything joins by the documented file-contract
columns below; never import another store module's internals.

## Producers (`ingest.py`)

`ci-hub/history/ingest.py` — incremental and idempotent. Re-running never
duplicates a row and resumes from the last cursor. All GitHub access goes through
`with-proxy gh` (override with `CI_HUB_GH`).

```bash
ci-hub/history/ingest.py                     # incremental, both repos
ci-hub/history/ingest.py --full              # full backfill (recursive windows)
ci-hub/history/ingest.py --since 2026-07-01  # bound the window
ci-hub/history/ingest.py --repo rrnewton/hermit --no-profiles --no-local
```

Dispatched by the front controller: `ci-hub refresh-history [--full] [args…]`
(`ci-hub.rs` `RefreshHistory` seam runs this file when present).

Two ingesters run per invocation:

**(A) GitHub Actions runs → `ignored/ci-hub/gha-runs.csv`.**
Keyed by `(repo, run_id, run_attempt)`; UPSERT by newest `updated_at`, so an
in-progress row is promoted to its terminal conclusion on re-run with **zero
duplicate rows**. Timing is split because the hosted pool is queue-starved and a
single wall figure lies:

- `queue_s = run_started_at − created_at`  (time spent queued before the run starts)
- `run_s   = updated_at   − run_started_at` (end-to-end run wall time; blank until terminal)

`run_s` is wall time, not execution cost. Jobs can wait for runners after the
run has started, so the run and job rows also carry one attempt-scoped
measurement:

- `runner_s` — sum of each executed job's own elapsed interval;
- `jobs_n` — number of job intervals in that sum;
- `jobs_src` — `jobs` when the values came from job rows, `absent` only when a
  fetched attempt had no executable job interval, and blank when the attempt was
  never measured.

`runner_s` may exceed `run_s` because parallel jobs contribute overlapping
runner time. It may also be much smaller when jobs wait inside an already-started
workflow run. Both absence and never-measured leave `runner_s` and `jobs_n`
blank, never zero; the source field keeps those two states distinct.

Both CSVs use the shared definitions in `gha_rows.py`. The run key is
`(repo, run_id, run_attempt)`; job evidence joins on that same three-part key.
The jobs API is requested with `filter=all`, because its default returns only the
latest attempt and cannot populate attempt-bearing history correctly. The
default job scope remains cancelled authoritative-main runs; use
`--jobs-scope all` to populate execution cost for every terminal authoritative
run.

Incrementality: the cursor `ignored/ci-hub/gha-cursor.json` records
`{last_created_at, max_run_id}` per repo. Every mode fetches through one
**recursive time-window fetcher**: a `created=start..end` window that saturates
(1000 rows — GitHub's hard per-query cap) is bisected in time until it fits, so a
busy drain day (>1000 runs) is never silently truncated. Non-terminal rows older
than the window are additionally re-fetched by id so queued/in_progress runs get
promoted once they finish.

**(B) Per-node CI profiling artifacts → `ignored/ci-hub/gha-profiles/<repo>/…`.**
Downloads GitHub artifacts named `ci-perf-*` (producer: hermit PR #1548, the
portable DAG runner) and unzips their `step_profiles_*.csv` onto local disk.
Idempotent via `gha-profiles/downloaded.json`. Until #1548 lands there are zero
such artifacts and this is a graceful no-op. Also refreshes the local
validate-run history via `ci-hub/validate/aggregate.py --write-global`.

## Consumer (`query.py`)

`ci-hub/history/query.py` — read-only, no GitHub access. Dispatched by
`ci-hub history …`.

```bash
# Per-DAG-node CPU-second budgets for cpu_timeout derivation.
ci-hub/history/query.py node-cpu-budgets --repo rrnewton/hermit --format csv
# → node, n_samples, max_cpu_s, p95_cpu_s, p50_cpu_s, max_wall_s,
#   suggested_cpu_timeout = round(max_cpu_s * 1.5), thin
# n_samples < 5 → suggested null + thin=true (never derive a timeout from noise).
# CPU seconds = step_profiles user_s + sys_s; node id = the `step` column
# ({group}.{job}). Reads local .safe-ci-dag-runner/profiles/ AND downloaded
# ci-perf step_profiles together.

# % of main-branch wall-clock time green (owner headline metric; DERIVED, never
# estimated).
ci-hub/history/query.py green-time --repo rrnewton/hermit
# state timeline over authoritative main runs; fraction of time conclusion==success.

# Default view: queue/run summary (the shape) PLUS the K most-recent individual
# runs behind it. `--since` works here (previously only on subparsers).
ci-hub history --repo rrnewton/hermit --limit 20
# columns: TIME(UTC) REPO RUN_ID WORKFLOW BRANCH/PR CONCL QUEUE(s) RUN(s)
#          RUNNER(s) JOBS JOBS_SRC URL
# `!` marks a queue outlier: waited > max(300s floor, window p95) for a runner.

# Surface the handful of runs stuck for hours (the tail behind a p95=0 median):
ci-hub history --repo rrnewton/hermit --slowest --limit 10
# View the current queued backlog individually (agrees with the summary's
# queued=N count — same gha-runs.csv store, so no divergent numbers):
ci-hub history --status queued
# JSON for tooling (mirrors the local-history flag surface: --since/--json/--limit):
ci-hub history --repo rrnewton/hermit --slowest --limit 10 --json
```

The `--since` / `--repo` / `--branch` / `--limit` / `--json` flag names match
`ci-hub local-history` on purpose so the two history subcommands do not diverge.
The recent-runs listing and every other store consumer read the SAME
`ignored/ci-hub/gha-runs.csv`, so counts agree by construction.

## Local main-history queries (`ci-hub`)

The typed front door uses one Rust history index for both directions; neither
command reruns validation:

```bash
# Most recent main commit whose latest clean, anchored local run passed.
ci-hub/ci-hub newest-green

# Newest retained PASS -> FAIL transition for an outer gate, DAG node, or test.
ci-hub/ci-hub first-bad liteinst_detcore_strict_verify_micro_suite
```

`newest-green` defaults to `--branch main` and returns only a clean, anchored
`full`-profile/`full`-selection pass at or after the effective rebase-base floor.
It derives that floor at query time from
`ci-hub/validate/gate_floors.py --json` and the versioned
`rebase-base-floors.json` registry; no floor SHA is embedded in the Rust query.
A green commit before the effective floor is not a usable rebase base. The
command refuses if the registry cannot prove every floor is on the fetched
branch rather than falling back to older green evidence.

The report prints the exact profile, selection mode, floor policy and effective
floor, plus
the count of distinct qualifying full-green commits in the floor-to-tip
first-parent window. It states the oldest commit, tip, total commit denominator,
and trustworthy-record count; duplicate receipts for one SHA count once. The
ledger is append-ordered, not time-ordered: branch recency comes from Git's
first-parent order, and receipt recency from `finished_at`. JSONL position is
never treated as time. PR-only heads are excluded because they are not rebase
bases. It also counts newer commits with no ledger record. The cache is keyed by
the fetched branch tip, gate floor, and ledger length + modification time, so any
of those changing invalidates it. Use `--no-fetch` only for an intentionally
offline snapshot.

`first-bad` reports both endpoints, unobserved commits between them, the files
touched by the candidate, and a conservative plausibility statement. Inner DAG
nodes, Rust test functions, and their error excerpt are recovered through the
log path stored in the same ledger row. Once observed, that detail is retained
without time-based expiry in `ignored/ci-hub/local-cell-evidence-cache.json`,
keyed to the source ledger row; it cannot create a verdict for a missing row.
If a historical log vanished before its first indexing, the command says the
cell detail was not retained and never turns absence into a pass. Host load was
not stored by schema 3, so only the measured run CPU/wall ratio can currently be
reported.

Both commands follow `validate-status` exit codes: `0` found the requested
evidence, `3` found failure evidence without the required green boundary, and
`4` means no qualifying record/transition. Tool-cost output reports estimated
and actual wall and CPU time on every completion path.

**Queued-run wait (`>=N`).** A still-queued run has `run_started_at == created_at`
(a GitHub placeholder), so its stored `queue_s` is `0` even after hours in the
queue — a silent wrong reading. The listing therefore shows `>=N` for a queued
run, where `N = snapshot_mtime - created_at` ("still queued as of our last
refresh, so it waited at least this long"). This is an OFFLINE lower bound
anchored to the snapshot, not a live `now - created_at` (which would trust a
possibly-stale status). It can only understate, never overstate, the current
wait. `queue_s` and its percentiles stay measured-terminal-only; the lower bound
is a separate `queue_lower_bound_s` field in `--json`. The `!` outlier flag uses
the effective wait (measured for terminal runs, lower bound for queued), so a
run stuck in the queue is flagged instead of reading `0`. See
`ai_docs/ci-hub-history-queued-wait-lower-bound_20260803.md` for the reasoning.

`green-time` implements a dated four-state definition (see the `GREEN-TIME
DEFINITION` block at the top of `query.py`, `GREEN_TIME_DEFINITION_DATE`). Main's
wall-clock is partitioned into **green / red / no_result / gap**, where **green
is a positive success record, never the absence of red**. The headline is:

```
green-time = green wall-hours / authoritative-run wall-hours
           = green hours / (green hours + genuine-red hours)
```

`success` -> green; a genuine failing verdict -> red;
`cancelled`/`skipped`/`neutral`/`stale`/unknown -> no_result (a destroyed or
withheld answer, re-dispatch not revert); pending or no-record -> gap. NO-RESULT
and gap hours are measured and printed beside every figure but are excluded from
the authoritative denominator. Green requires ALL authoritative workflows to
succeed (precedence red > gap > no_result > green).

The definition names a **seven-case taxonomy** so "not red" is not collapsed into
one bucket: cancelled-below-cap (supersede/manual) is no_result, cancelled-at-cap
(self-timeout kill) is red, environmental/harness-caused failure is no_result,
and the **seventh case** — a run-level `cancelled` that masks a *job* which
failed first — is red, discriminated by **ordering** (the job's red conclusion
completed at/before the run's cancel moment). Cases that the run-level store
cannot yet discriminate (self-timeout annotation; per-job failure) stay
conservatively in no_result, so an offline blind spot can only UNDER-count red,
never inflate green. The seventh-case discriminator (`_resolve_cancelled_run`) is
LIVE: `ingest.py` produces the per-job store `gha-jobs.csv`, scoped to cancelled
authoritative-main runs (the only runs where the case can fire), so a masked job
failure is now recovered as red. Being non-green, the promotion moves the
red<->no_result split (which drives action) without ever changing green_pct. A
run cancelled while still queued has zero jobs and correctly stays no_result; a
fetched-set sidecar (`gha-jobs-fetched.json`) records exact
`run_id:run_attempt` values and makes the default ingest O(new cancelled runs).
Legacy bare run IDs are accepted only when the run store contains exactly one
attempt; otherwise the run is fetched again rather than assigning evidence to
the wrong attempt. Skip job ingestion with `ingest.py --no-jobs`, force a
re-fetch with `--refetch-jobs`, or use `--jobs-scope all` to measure every
terminal authoritative run.

The regular `ci_queue_health` tick appends the complete snapshot to
`ignored/ci-hub/green-time-log.jsonl`: percentage, numerator, authoritative-run
denominator, exact window, excluded NO-RESULT/gap hours, workflow set, and sample
count travel together. The lane-health JSONL carries the same window and
denominator with its copy of the figure; a bare percentage is not a valid record.

The store preserves every conclusion and timestamp, so a refined definition can
be recomputed later without re-ingesting. Authoritative workflow defaults: hermit
`CI (GitHub-managed portable)`, reverie `Rust`; override with `--workflow`
(repeatable). Use `--trend {day,week}` for the trend and `--append-log` to
persist a durable JSONL snapshot per run.

## Join keys (file contract — join ONLY via these)

| concept            | gha-runs.csv | local (`validate-run-global.jsonl`) |
|--------------------|--------------|-------------------------------------|
| commit             | `head_sha`   | `commit`                            |
| repo               | `repo`       | (same string)                       |

All commit values are the same 40-hex SHA; `repo` is the same `OWNER/REPO` string
everywhere. The `submodule-bumps` stream stays separate and joins by these
columns only.

## Live consumers

1. **cpu_timeout derivation** (`ci-timeout-audit-cpu-time-manifest-node`,
   `enable-cgroups-and-cpu-timeouts-across-dag-nodes`) consumes
   `node-cpu-budgets`. Nodes with ≥5 samples get a suggested timeout; the rest
   stay UNSET (thin) rather than guessed. Real per-node CPU rows arrive once
   #1548 lands and profiling artifacts are ingested.
2. **Owner's "% time green on main"** consumes `green-time`, derived from
   `gha-runs.csv`.

## Tests

`python3 -m unittest test_history` (in `tests/`). Offline: synthetic CSVs cover
the queue/run split, UPSERT idempotency, node-budget aggregation + thin flag, and
the green-time interval computation.
