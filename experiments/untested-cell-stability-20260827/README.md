# Stability of red compatibility cells under ten repetitions

Status: **complete for the fixed 338-cell pressure-test population.** Four
disjoint batches cover every cell in that population once, with ten final
repetition results per cell.

## Abstract

`red` means **not selected into the regression plan**, not measured and failing.
The headline is that **296 of 338 red cells passed all ten final repetitions**.
That does not mean 296 cells were fixed: red is a plan-selection status, and
this is primarily a first measurement of cells that had not been run before.

The 338 cells produced 3,380 final repetition results: 2,984 pass, 178
infrastructure-error, 169 crash-error, and 49 determinism-failure. At cell
level, 296 passed all ten final repetitions, 31 had one non-pass result class in
all ten, and 11 varied across repetitions. Four of the varied cells sometimes
passed and sometimes did not. The other seven always failed or produced no
trustworthy result, but the reported class varied.

The 11 varied cells are the result repetition was intended to expose. Four are
flakes that sometimes pass and sometimes do not, behavior for which the
scorecard had no prior evidence. The remaining seven vary among failure or
no-result classes rather than between pass and non-pass.

Of the 338 cells, 329 were `never-measured` before this run. Among those 329,
293 passed all ten final repetitions, four had both pass and non-pass results,
and 32 never passed. Those are first observations, not improvements.

## Population and the corrected missed-cell accounting

The source table is `ci/compat-envelope/cells.json` at Hermit
`7ddb3559a09f2709a67ce2b605426b43c33f070f`. Its content is byte-identical at
both experiment commits and has SHA-256
`727e0185974e520a721a46ed0c23ce5eb745f92176311f18f7f7f7eb81e3d05c`.

| population | cells |
|---|---:|
| red, all modes | 371 |
| red `verify` | 336 |
| red `chaos` | 2 |
| red `naked` | 33 |
| admitted by the all-mode `--sample` policy | **338** |
| enabled and `never-measured`, regardless of status | 363 |

The fixed study population is the 338 admitted cells: 336 `verify` plus two
`chaos`. It contains 329 `never-measured` cells, eight previously `diverged`
cells, and one previously `measured-and-passed` cell. By backend it is 122
ptrace, 86 sabre, 61 dbt, 48 liteinst, and 21 kvm cells.

This corrects the **58 unmeasurable cells** figure that was relayed to the owner:
it was invalid subtraction, not a set. A discarded selector bug reported 305
eligible cells because it selected
the enabled **green** population. The independently counted 363 cells were the
enabled **never-measured** population. Those sets overlap in only one identity;
`363 - 305 = 58` does not identify 58 cells.

The actual difference used here is 371 red minus 338 admitted by sampling: 33
`naked/native` cells. They do **not** lack runnable commands. `manifest-plan`
reports executable attempts for all 33 (31 declare three runs and two declare
five), and `tests/manifest-cli.rs get` rendered a command for all 33 with zero
failures. They are excluded by the `pressure-test` sampling predicate, which
admits only `verify`, `replay`, and `chaos` when `--sample` is present without an
explicit mode. Their exact identities are in `population-red-excluded-33.tsv`.
They were not added after batch 1 because doing so would change the
pre-established 338-cell population mid-study.

The exact lists are:

- `population.tsv`: the 338 cells measured here, in `test`, `mode`, `backend`
  columns.
- `population-red-371.tsv`: every red cell and its prior measurement state.
- `population-enabled-never-measured-363.tsv`: the differently defined
  population named in the original task.
- `population-red-excluded-33.tsv`: the 33 naked cells outside this run, with
  prior status, measurement, and exclusion reason.
- `test-ids.txt`: all 187 test IDs represented by the 371 red cells.
- `test-ids-pressure-test-eligible-155.txt`: the 155 test IDs represented by
  the 338-cell study population.
- `never-measured-uniform-pass-cell-ids-293.txt`: the exact cell IDs for the
  293 `never-measured` cells that passed all ten final repetitions.
- `never-measured-uniform-pass-test-ids-139.txt`: the 139 unique test IDs
  represented by those 293 cells.
- `never-measured-remaining-36.tsv`: the other 36 `never-measured` cells, with
  their cell-level classification and final-result counts.
- `never-measured-reconciliation.txt`: row counts and set checks reconciling
  those files against the 329 `never-measured` cells and all 338 study cells.

## Method

Batch 1 was the original seeded draw. Batches 2–4 came from the exact sorted
complement, not new random samples:

| batch | selection | cells |
|---|---|---:|
| 1 | `--sample 85 --seed 1` | 85 |
| 2 | first 85 rows of the explicit complement | 85 |
| 3 | next 85 rows of the explicit complement | 85 |
| 4 | remaining rows of the explicit complement | 83 |

The four membership files have 338 rows in total, 338 unique identities, zero
pairwise overlap, and a union exactly equal to `population.tsv`. Each input was
also compared to the corresponding plan and retained `run.json` before its
batch was accepted. `--cells-from` silently drops an individually unmatched row
when other rows remain valid, so this equality check is required.

The effective commands, run from the Hermit checkout, were:

```bash
study=/path/to/dev-hermit/experiments/untested-cell-stability-20260827

with-proxy env -u DEV_HERMIT_PARENT \
  ./ci/compat-envelope/pressure-test.rs run \
  --results ignored/compat-envelope/red-batch1-seed1 \
  --sample 85 --seed 1 --repetitions 10 \
  --cell-timeout 30 --run-timeout 7200 --jobs 16

with-proxy env -u DEV_HERMIT_PARENT \
  ./ci/compat-envelope/pressure-test.rs run \
  --results ignored/compat-envelope/red-batch2-complement \
  --cells-from "$study/batch2-cells-85.tsv" \
  --repetitions 10 --cell-timeout 30 --run-timeout 7200 --jobs 16

with-proxy env -u DEV_HERMIT_PARENT \
  ./ci/compat-envelope/pressure-test.rs run \
  --results ignored/compat-envelope/red-batch3-complement-restart1 \
  --cells-from "$study/batch3-cells-85.tsv" \
  --repetitions 10 --cell-timeout 30 --run-timeout 7200 --jobs 16

with-proxy env -u DEV_HERMIT_PARENT \
  ./ci/compat-envelope/pressure-test.rs run \
  --results ignored/compat-envelope/red-batch4-complement \
  --cells-from "$study/batch4-cells-83.tsv" \
  --repetitions 10 --cell-timeout 30 --run-timeout 7200 --jobs 16
```

The executed inputs were byte-identical copies of the published membership
files.

### Provenance

| item | value |
|---|---|
| batch 1 Hermit commit | `ae137114fdf1a670e4a31f74bdfc78a675037deb` |
| batches 2–4 Hermit commit | `8f78fc6620c75bafde1aed758020aed6783a84c2` |
| difference between those commits | only `ci/compat-envelope/pressure-test.rs`, adding and hardening explicit `--cells-from` selection |
| common base | Hermit main `b7f97008972669fd60acf3b0b579273a40d8ee1e` |
| pull request | https://github.com/rrnewton/hermit/pull/2798 |
| DetCore tree | `c6423f8e4c6f8190c6cfaa3687cf9916d22e5a0a` |
| scheduler width | 16 jobs |
| raw artifacts | `/home/newton/temp/hermit-coord-helper-pressure-repetitions/ignored/compat-envelope/` |

Both source trees were clean. No Hermit product code changed between batch 1
and batches 2–4.

### Timeout and containment checks

Before execution, each retained DAG was checked for exactly one node per cell
and repetition. Every one of the 3,380 cell nodes has a 30-second wall timeout
and a 60-second CPU timeout.

The in-process whole-run wall budget was 7,200 seconds. The outer systemd scope
added 720 seconds of cleanup grace and read back `RuntimeMaxSec=7920s` while
live. The individual scope had `cpu.max=max`; CPU containment came from the
shared `dagrun.slice` value `28440000 100000`, or 284.4 cores on this 316-core
host. `--jobs 16` separately bounded scheduler concurrency. Thus the CPU quota
was shared aggregate containment, not a 16-core per-run quota.

The 30-second bound fired 32 times. The whole-run wall bound fired zero times,
and no OOM was recorded. Without the inner bound, the 32 hung repetitions would
have consumed their node allowance instead of terminating with runner evidence.

### Result normalization

The report does not use `summary.json.repeated_cells`: the current tool labels
that section as green-cell output and aggregates a repeated red batch under its
first cell. Instead, `summary.rows` is grouped by exact
`(test, mode, backend, lane, category, repetition)` and the maximum attempt is
the final result. `normalize-pressure.jq`, `final-results-tsv.jq`, and
`cell-results-tsv.jq` contain the exact transformations.

`summary_rows` is also not an execution count. Across the study, 3,380 final
repetitions required 3,764 executions because 384 repetitions retried once.
Framework `results.jsonl` rows plus runner-proven timeout attempts independently
reproduce every batch's execution count.

Batch 4 exposed a post-processing defect after all 830 cell jobs completed. An
earlier `chaos` retry had divergence coordinates but, correctly for chaos mode,
no single `verify-1.json`; the summarizer incorrectly required that verify-mode
file and stopped before writing `summary.json`. The one-line guard in
`batch4-summarizer-recovery.patch` limited earlier-attempt verification-history
processing to `verify` and `replay`, then `summarize` was rerun without executing
any cell. The patch was removed afterward and the Hermit checkout is clean. The
raw two-attempt chaos record remains retained; only the invalid verify-file
lookup was skipped.

### Retained retry failure classification

`retry-failure-classification.tsv` classifies the 23 batch-one result groups
that contain a second attempt. Its source is the retained
`red-batch1-seed1/summary.json` named in `raw-artifacts-sha256.txt` at SHA-256
`7a504faf0f07544f1cd6cd8c701a6744ce3b97d3d14fa4f10994f494669773a3`.
`retry-failure-classification-tsv.jq` requires that exact measured population:
873 attempt rows, 850 result groups, and 23 two-attempt groups whose first
attempt is the retained canonical `determinism-failure` result.

All 23 triggering first attempts are `product_failure` under the framework's
current [`ObservedResult` to `FailureClass` contract](https://github.com/rrnewton/hermit/blob/7f050cc4c9bc778022bbc661c6c53b1603d16710/ci/manifest-plan/src/runner.rs).
Sixteen second attempts
remain `determinism-failure`, four change exact result to `crash-error`, and all
twenty remain `product_failure`. Three second attempts pass; their
`failure_class` is `null` because a pass has no failure attribution, and those
three repetitions are recovered green results. Thus seven exact results change
between attempts, while the four failure-to-failure changes stay in the same
broader product-failure category.

The retained summary has `error_kind=null` on every one of these attempts and
contains no `retry_class` field. Neither absence is used as the classification
source. `failure_class` comes from the retained structured `result`;
`retry_class` separately answers why another attempt was allowed and its newer
class/detail contract is not reconstructed into this older artifact. The raw
summary is unchanged. `retry-failure-classification-verification.txt` records
the source and projection digests and the exact result/class counts.

## Results

### By batch

| batch | cells | final results | executions | retries | pass | infrastructure-error | crash-error | determinism-failure | 30s timeouts | uniform pass cells | varied cells |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 85 | 850 | 938 | 88 | 755 | 42 | 35 | 18 | 12 | 74 | 5 |
| 2 | 85 | 850 | 908 | 58 | 773 | 57 | 18 | 2 | 20 | 77 | 2 |
| 3 | 85 | 850 | 912 | 62 | 790 | 20 | 40 | 0 | 0 | 79 | 0 |
| 4 | 83 | 830 | 1,006 | 176 | 666 | 59 | 76 | 29 | 0 | 66 | 4 |
| **total** | **338** | **3,380** | **3,764** | **384** | **2,984** | **178** | **169** | **49** | **32** | **296** | **11** |

Final-result shares are 88.3% pass, 5.3% infrastructure-error, 5.0%
crash-error, and 1.4% determinism-failure. The 178 infrastructure-error rows
mean the overall result is incomplete as compatibility evidence. Among the
3,202 final repetitions with a product result, 2,984 passed (93.2%).

At cell level:

| final ten-result pattern | cells |
|---|---:|
| uniform pass | **296** |
| uniform infrastructure-error | 16 |
| uniform crash-error | 12 |
| uniform determinism-failure | 3 |
| varied | **11** |

Six of the 296 uniform-pass cells needed at least one retry; 290 passed all ten
on their first attempts. A 10/10 final result therefore does not always mean
10/10 first-attempt stability.

### Cell-level pattern by backend

| backend | cells | uniform pass | uniform infrastructure-error | uniform crash-error | uniform determinism-failure | varied |
|---|---:|---:|---:|---:|---:|---:|
| dbt | 61 | 58 | 2 | 0 | 0 | 1 |
| kvm | 21 | 21 | 0 | 0 | 0 | 0 |
| liteinst | 48 | 46 | 2 | 0 | 0 | 0 |
| ptrace | 122 | 106 | 12 | 0 | 1 | 3 |
| sabre | 86 | 65 | 0 | 12 | 2 | 7 |

This table describes this fixed population. The batches after batch 1 were
ordered slices of the complement, so batch-to-batch rates are confounded by
which categories and backends occur in each slice.

### The 11 varied cells

| batch | cell | final results |
|---:|---|---|
| 1 | `c-programs/ppoll-simulation verify/sabre` | crash-error 9, determinism-failure 1 |
| 1 | `c-programs/printf-with-threads verify/sabre` | crash-error 9, determinism-failure 1 |
| 1 | `c-programs/threadexhaustion verify/sabre` | crash-error 7, determinism-failure 3 |
| 1 | `determinism-stress-c/pipe-chain verify/sabre` | pass 7, determinism-failure 3 |
| 1 | `language-runtimes/example-python-random verify/dbt` | pass 8, infrastructure-error 2 |
| 2 | `backend-parity-c/pthread-lifecycle verify/sabre` | crash-error 8, determinism-failure 2 |
| 2 | `c-programs/dbt-pid-virtualization verify/ptrace` | pass 3, infrastructure-error 7 |
| 4 | `c-programs/thread-self-procfs-handoff verify/sabre` | crash-error 8, infrastructure-error 2 |
| 4 | `chaos-c/lock-granularity verify/sabre` | crash-error 4, determinism-failure 6 |
| 4 | `determinism-stress/order-violation chaos/ptrace` | pass 6, crash-error 4 |
| 4 | `system-utils/procfs-sanitized-paths verify/ptrace` | infrastructure-error 7, determinism-failure 3 |

Four cells alternate between pass and a non-pass result. The other seven do not
pass, but their failure or no-result class changes. Both are defects worth
retaining; neither is discounted because another job was running on the host.

### Runner-proven 30-second timeouts

| cell | timed-out final repetitions |
|---|---:|
| `applications/example-timed-progress-bar verify/liteinst` | 10 |
| `applications/example-timed-progress-bar verify/ptrace` | 10 |
| `backend-parity-c/signal-waitstatus-identity verify/dbt` | 10 |
| `language-runtimes/example-python-random verify/dbt` | 2 |

All 32 are classified as infrastructure-error because no trustworthy
compatibility result was produced. That classification does not make the hangs
an environmental artifact; they are named failures of the tested path.

### Recorded reasons for all 396 non-pass final repetitions

| result | retained reason | repetitions |
|---|---|---:|
| crash-error | SaBRe execution path incomplete or used fallback/native sites | 165 |
| infrastructure-error | verify exited 125 before a terminal comparison | 127 |
| determinism-failure | canonical verification diverged | 49 |
| infrastructure-error | runner-proven 30-second timeout; no row reason | 32 |
| infrastructure-error | verify exited 122 before a terminal comparison | 10 |
| infrastructure-error | verification said matched but did not prove bitwise parity | 7 |
| crash-error | chaos comparison mismatch, returned nonzero | 4 |
| infrastructure-error | cell's 15-second manifest deadline expired before attempt 1 | 2 |

`non-pass-cells.tsv` names all 42 cells that did not pass all ten final
repetitions. `final-results.tsv` retains the exact per-repetition result, attempt
number, reason, and timeout/OOM evidence.

### Previously measured red cells

The nine previously measured cells prevent interpreting every pass as a first
measurement. Three of the eight previously `diverged` cells passed 10/10 here;
five still did not. The single previously `measured-and-passed` cell,
`applications/example-timed-progress-bar verify/ptrace`, timed out in all ten
repetitions. These are observations of current behavior, not evidence that a
code change fixed or regressed a cell during this study.

## Box conditions

| batch | execution window, local time | condition at launch | validation slots |
|---:|---|---|---|
| 1 | 17:21:16–17:36:14 | recorded as quiet | both free |
| 2 | 18:00:26–18:15:43 | 15.31 executing cores of 316 (4.85%); 88.13% memory available | slot 0 held, slot 1 free |
| 3 | 18:27:51–18:41:23 | 17.31 executing cores of 316 (5.48%); 88.35% memory available | slot 0 held, slot 1 free |
| 4 | 18:43:45–18:58:14 | 15.92 executing cores of 316 (5.04%); 88.42% memory available | slot 0 held, slot 1 free |

An initial batch-3 process was interrupted when an owner hold arrived, after 451
run directories and before any summary. Its partial directory is retained as
`red-batch3-complement` and contributes no result here. Once the hold was
lifted, the complete 85-cell batch was restarted from the same input as
`red-batch3-complement-restart1`.

The later batches shared the host with a validation, but the measured CPU and
memory load was low relative to the 316-core host. More importantly, the owner
has ruled that these runs should tolerate load: a failure under contention is a
real defect to fix, not a reason to remove or discount a result. The conditions
above are context only.

## What the results establish—and what they do not

They establish the ten-repetition behavior of every cell in the fixed 338-cell
pressure-test population at the stated commits, under the stated bounds. They
name every final non-pass, every varied cell, every retry, and every timeout.
There are no missed identities inside that population.

They do not establish the behavior of the 33 naked cells excluded by the
sampling predicate. They also do not directly predict the validation gate.
`pressure-test` ran the host/release/quiet path, while validate runs the
pinned-root/debug/contended path. That configuration difference limits transfer
from this result to the gate. It does not excuse a failure observed here, and
host load was not used to discard any row.

The ten repetitions are one contiguous measurement session on one host. They
expose observed instability but do not bound the long-run probability of a
failure that did not occur.

## Published data

- `batch-summary.tsv`: batch-level counts used in the tables above.
- `cell-results.tsv`: all 338 cell summaries.
- `non-pass-cells.tsv`: the 42 cells that were not final-attempt pass 10/10.
- `final-results.tsv`: all 3,380 final repetition results and retained reasons.
- `batch1-cells-85.tsv`, `batch2-cells-85.tsv`, `batch3-cells-85.tsv`, and
  `batch4-cells-83.tsv`: exact disjoint inputs.
- `normalize-pressure.jq`, `final-results-tsv.jq`, and `cell-results-tsv.jq`:
  normalization programs.
- `retry-failure-classification.tsv` and
  `retry-failure-classification-tsv.jq`: the durable 23-retry projection and
  its exact derivation from the retained batch-one summary.
- `retry-failure-classification-verification.txt`: source hash, projection
  hash, and result/class totals for that projection.
- `verification.txt`: exact membership, runner, timeout, and command-rendering
  checks.
- `batch4-summarizer-recovery.patch`: the exact one-line post-processing guard
  used after batch 4; it did not rerun a cell.
- `raw-artifacts-sha256.txt`: hashes for retained raw metadata, runner evidence,
  summaries, logs, and condition records.
- `batch1-seed1-selected-76.tsv`: explicitly marked discarded pilot output from
  the selector bug; it is not part of this study.

The multi-gigabyte raw run directories remain in the preserved Hermit checkout
at the path in the provenance table. The tracked tables are the compact,
reviewable extraction from those raw artifacts.
