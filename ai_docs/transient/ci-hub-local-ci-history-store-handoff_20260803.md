# ci-hub local CI-history store — implementation handoff

- **Date:** 2026-08-03
- **Task:** `ci-hub-local-ci-history-store` (P1)
- **Landed:** parent `main` @ `32bc690703aae204222f68889811b22c3b83fc49`
- **Files:** `ci-hub/history/{ingest.py, query.py, tests/test_history.py, README.md}`
- **Type:** parent-only tooling (no PR; task explicitly names these parent files)

## What it is

The **single** local accumulator of commit/CI knowledge. Two independently
maintained stores drift (that is how the hand-maintained test-footprint map
rotted), so everything joins to this store by documented file-contract columns
and never imports another store module's internals.

Front-controller seam already existed in `ci-hub/ci-hub.rs`:
`ci-hub refresh-history [--full] [args…]` → `ingest.py`;
`ci-hub history …` → `query.py`.

## Producers — `ingest.py` (incremental, idempotent)

**(A) GitHub Actions runs → `ignored/ci-hub/gha-runs.csv`.**
- Keyed by `(repo, run_id, run_attempt)`; UPSERT by newest `updated_at` so an
  in-progress row is promoted to its terminal conclusion on re-run with **zero
  duplicate rows**.
- Timing is **split** — the hosted pool is queue-starved, so one wall figure
  lies:
  - `queue_s = run_started_at − created_at`
  - `run_s   = updated_at   − run_started_at` (blank until terminal)
- **Recursive time-window fetcher**: a `created=start..end` window that saturates
  (1000 rows — GitHub's hard per-query cap) is bisected in time until it fits, so
  a busy drain day (>1000 runs) is **never silently truncated**. This directly
  honors the "No silent caps" guidance — the first flat-paging version silently
  dropped rows past 1000; the recursive version fetched 3690 hermit runs in 101
  API calls.
- Cursor `ignored/ci-hub/gha-cursor.json` (`{last_created_at, max_run_id}` per
  repo) makes reruns resume. Non-terminal rows **older than the window** are
  re-fetched by id (bounded) so queued/in_progress runs get promoted once they
  finish, without re-fetching every open row each run.
- All GitHub access via `with-proxy gh` (`CI_HUB_GH` override). Deliberately does
  **not** inherit the BpfJailer-blocked `pr_status.py` per-PR git-fetch fan-out;
  the gh REST API over the proxy works.

**(B) ci-perf profiling artifacts → `ignored/ci-hub/gha-profiles/<repo>/…`.**
- Downloads GitHub artifacts named `ci-perf-*` (producer: hermit **PR #1548**,
  the portable DAG runner — currently UNLANDED) and unzips their
  `step_profiles_*.csv`. Idempotent via `gha-profiles/downloaded.json`; bounded
  to the newest `--profiles-max-pages` (default 30) pages. Graceful no-op until
  #1548 lands.
- Also refreshes local validate-run history via
  `ci-hub/validate/aggregate.py --write-global`.

## Consumer — `query.py` (read-only, no network)

- **`node-cpu-budgets`** — per-DAG-node CPU-second budgets for `cpu_timeout`
  derivation. `suggested_cpu_timeout = round(max_cpu_s × 1.5)` on the
  distribution **max**; `n < 5 → null + thin=true` (never derive a timeout from
  noise). CPU seconds = `step_profiles.user_s + sys_s`; node id = the `step`
  column (`{group}.{job}`). Reads local `.safe-ci-dag-runner/profiles/` AND
  ingested ci-perf `step_profiles` together. `--since` accepts a SHA prefix or a
  date.
- **`green-time`** — owner's **"% time green on main"**, DERIVED (never
  estimated): a state timeline over authoritative main-branch terminal runs;
  fraction of wall-clock where `conclusion == success`. Authoritative defaults:
  hermit `CI (GitHub-managed portable)`, reverie `Rust` (`--workflow`
  overridable). Every conclusion + timestamp is preserved, so an alternative
  definition (e.g. carry-forward across `cancelled`) is computable later without
  re-ingesting.
- **`runs`** — `gha-runs.csv` summary with the queue/run split.

## Join keys (file contract — join ONLY via these)

| concept    | gha-runs.csv | local (`validate-run-global.jsonl`) |
|------------|--------------|-------------------------------------|
| commit     | `head_sha`   | `commit`                            |
| repo       | `repo`       | (same string)                       |

The `submodule-bumps` stream stays separate and joins by these columns only.

## Live verification (2026-08-03)

- Store: **7280 rows** (3690 hermit + 3590 reverie).
- `green-time` hermit **6.67%** (drain: 98 cancelled / 22 failure / 3 success);
  reverie **86.77%** (260 success / 37 failure / 2 cancelled).
- `node-cpu-budgets` returns **thin/UNSET** today because #1548 is unlanded AND
  this host lacks cpu-cgroup delegation (local `step_profiles` carry empty
  `user_s`/`sys_s`). This is correct behavior, not a bug.

## Tests

`cd ci-hub/history/tests && python3 -m unittest test_history` — 8 offline tests
(synthetic CSVs, `DEV_HERMIT_PARENT` temp override): queue/run split, `run_s`
blank until terminal, UPSERT idempotency + newest-wins, node-budget thin flag +
`round(25×1.5)=38`, skip rows missing cpu, green-time interval fraction,
green-time ignores non-main/non-authoritative, percentile.

## Downstream / next

1. When **#1548** lands: `ci-hub/history/ingest.py` folds ci-perf artifacts in;
   re-run `node-cpu-budgets` — the ~48 UNSET DAG nodes get real per-node CPU rows
   with no code change. Consumer:
   `enable-cgroups-and-cpu-timeouts-across-dag-nodes` /
   `ci-timeout-audit-cpu-time-manifest-node`.
2. Coordinator closes `ci-hub-local-ci-history-store` after landing confirmation
   (working agent stays `in_progress` per CLAUDE.md task lifecycle).
