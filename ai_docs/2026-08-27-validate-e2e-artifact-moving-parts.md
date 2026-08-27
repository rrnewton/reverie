# Validate E2E artifact moving parts

## Scope

This report explains the retained E2E artifacts from the full validate at Hermit
commit `26f5bc2de3d328b64c41c42ef093ecb9d89e98ab`:

`ignored/validate/artifacts/validate-hermit-147-green-bar-baseline-26f5bc2de3d3/e2e`

The directory is organized at three different levels: validate node, cell, and
attempt. Those levels are easy to confuse because their files overlap and the
word `attempt` appears at more than one layer.

## The three main parts

### `portable/` and `privileged/`: results grouped by validate node

Each `manifest_<category>/` directory corresponds to one E2E validate node. For
example, `portable/manifest_applications/` is the retained report from
`e2e.manifest_applications`.

The manifest runner in `hermit/ci/manifest-plan/src/runner.rs` writes three
files there:

- `results.jsonl` is the row-level record. Each line records one scheduled cell
  attempt, including the test ID, mode, backend, outer attempt number, outcome,
  reason, command, duration, and exact `artifact_dir`.
- `summary.json` aggregates the rows into node totals. It cannot identify which
  individual cell failed or prove that a missing cell was scheduled.
- `junit.xml` is the reporting projection for CI consumers. It is not the best
  source for diagnosing a cell or counting retries.

For a failure, `results.jsonl` is the first file to open.

### `runs/<run-id>/<cell>/`: evidence from an executed cell

Each cell is identified by test ID, mode, and backend. Its directory name is a
filesystem-safe rendering such as:

`applications-timed-progress-bar-verify-ptrace`

An outer retry uses a separate sibling directory:

`applications-timed-progress-bar-verify-ptrace-attempt-2`

The manifest runner creates the directory and writes the process captures:

- `captures/verify-1.stdout`
- `captures/verify-1.stderr`

It invokes Hermit with `--verify-json`; Hermit writes `verify-1.json`. That file
contains the comparator-level result: `matched`, `diverged`, or `no_result`,
along with comparison counts and first-divergence information when available.
It does not replace the row in `results.jsonl`: the row records the scheduled
cell attempt and process outcome, while `verify-1.json` records what Hermit's
two-run comparison was able to conclude.

The same cell directory can also contain its isolated home, XDG configuration,
fixtures, or working directory. Those reproduce the execution environment; they
are not additional result records.

### `audit-compile/`: preparation rather than execution results

This contains compiled guests and scratch state produced by the manifest
runner's audit/compile preparation. Its presence proves that preparation ran;
it does not prove that the corresponding runtime cell executed or passed.

## Where run1 and run2 comparison logs live

When `E2E_KEEP_VERIFY_LOGS=1` is set, the manifest runner creates and passes a
durable `--verify-log-dir` to Hermit. For the first process execution in a cell,
the paths are:

`runs/<run-id>/<cell>/verify-logs/verify-1/run1_log_<random>`

`runs/<run-id>/<cell>/verify-logs/verify-1/run2_log_<random>`

For an outer retry, `<cell>` is the sibling directory ending in `-attempt-2`.
The manifest runner creates `verify-logs/verify-1/`; Hermit's
`temp_log_files_in` function writes the two files using the exact prefixes
`run1_log_` and `run2_log_`.

This baseline artifact directory contains no `verify-logs/` directories and no
files with either prefix. `E2E_KEEP_VERIFY_LOGS=1` was not enabled, so the raw
records actually compared by Hermit are missing from this retained tree. The
captures and `verify-1.json` retain the reported outcome, but not the complete
inputs to the comparison.

The DBT defect was a producer/consumer naming mismatch. DBT supplied names that
produced `dbt-run1_log_*` and `dbt-run2_log_*`, while the manifest scanner only
accepted filenames beginning exactly with `run1_log_` and `run2_log_`. A real,
repeatable divergence could therefore be relabeled as an infrastructure error
because the scanner found zero matching logs. The landed correction makes DBT
use the same prefixes as the other backends.

## The per-attempt record

The authoritative E2E attempt record is the cell's line in the enclosing
node's `results.jsonl`.

- Top-level `"attempt": 1` is the first scheduled attempt.
- Top-level `"attempt": 2` is the retry, and its `artifact_dir` normally ends
  in `-attempt-2`.
- An embedded `attempts[].index` and filenames such as `verify-1.json` describe
  process executions inside that cell attempt. They are not a second record of
  the outer retry.

In this baseline, retry was at the enclosing node level. Consequently a node
retry produced `-attempt-2` directories and second rows even for cells that had
already passed. The existence of an `-attempt-2` directory does not by itself
mean that cell failed attempt one.

Nextest's console output is not an attempt ledger. It prints a failing test once
when it happens and again in the end-of-run recap. Counting those lines doubles
failures and retries. For these E2E cells, count `results.jsonl` rows by their
top-level attempt field. For nextest tests outside this E2E runner, partition by
the validate node's `START` events so the recap is not treated as another
execution.

## Failed versus never ran

An executed failing cell has all of the following:

- a `results.jsonl` row with `FAIL` or `ERROR`;
- a top-level attempt number;
- an `artifact_dir`, normally containing captures and possibly a verify JSON.

`HOST-INAPPLICABLE` is also represented by a row and reason. It is distinct
from absence.

A `verify-1.json` value of `verdict: "no_result"` with
`no_result_reason.kind: "not_run"` does not mean the cell was never attempted.
It means the runner invoked the cell, but Hermit failed before the guest
execution produced a comparison. For example, the timed-progress-bar row in
this run records exit status 127 and points to captures even though its verify
JSON says `not_run`.

A cell that never entered this runner has no row and no cell artifact directory.
That absence is not self-explanatory. This E2E tree alone cannot distinguish:

- a cell excluded from the selected population;
- a selected cell whose validate node was skipped because a prerequisite failed;
- a cell absent from the manifest at this commit.

That distinction requires comparison with the manifest plan and the full
validate DAG/ledger. This is the main representational gap in the directory.

## What to open first

For one failing cell:

1. Find its line or lines in the node's `results.jsonl`.
2. Read `attempt`, `outcome`, `reason`, and `artifact_dir` there.
3. Open `captures/<mode>-1.stderr` and `.stdout` in that artifact directory.
4. For verify mode, open `verify-1.json` for Hermit's comparison verdict.
5. If `verify-logs/` exists, inspect `run1_log_*` and `run2_log_*` for the actual
   compared content. In this baseline, step 5 is unavailable.

## Missing or confusing evidence

- Raw run1/run2 logs are not retained by default, including in this baseline.
  The durable artifact directory therefore cannot independently reproduce many
  comparison conclusions.
- `results.jsonl` and `verify-1.json` overlap but report different layers. A
  reader who opens only one can confuse process failure with comparator verdict.
- `attempt` is overloaded between the outer scheduled retry and inner process
  executions.
- Whole-node retries create second-attempt evidence for already-passing cells.
- No row means absence, but does not encode why the cell is absent.
- Some row provenance, including command paths and working directories, points
  into the disposable fresh validate checkout. The retained artifact directory
  survives checkout removal, but those referenced inputs do not.
- The nextest node log and end-of-run recap live outside this E2E subtree. The
  subtree is sufficient for executed E2E cell rows, but not for the full DAG
  explanation of skipped or never-started nodes.
