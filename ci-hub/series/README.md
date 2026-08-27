# Stress / flakiness series store

Append-only, Git-backed history of **repeated** cell measurements. Each row
represents one run, or a consecutive range of equal runs when `num_runs` is
greater than one.

```
series/<team>/<short-host>/<YYYY>-<MM>.jsonl        tracked, published
ignored/ci-hub/series-spool/                        live, unpublished
```

The scheduled `ledger_publish_sweep` drains both validation-ledger and per-test
series batches through the same append-only publisher and serialized parent-main
writer. Until that succeeds, the producing checkout reads its own spool. After
it succeeds, another checkout reads the tracked `series/` shard after fetching
parent main. `series_spool_backlog` reports batches that remain unpublished.

Step 3 of the https://github.com/rrnewton/hermit/pull/2447 storage plan created
this store. The validate and pressure-test writers and the history command below
complete step 4.

## Why a second store rather than a column on the ledger

The validation ledger records one event per validation **run of a whole tree**.
This represents one observation per **cell per repetition**: a pressure campaign
over 40 cells at N=20 represents 800 runs against a single tree, potentially in
fewer rows after compression. Different cardinality, different lifetime.
Folding them into the validate shards would make a month's shard unreadable for
the purpose it already serves.

## What is reused, deliberately, and what is new

`series.py` **imports** the shard/spool/union model from `ci-hub/ledger/ledger.py`
rather than copying it:

| reused | why it must not be re-implemented |
| --- | --- |
| `union_events` | same ordering + `event_id` dedup, so a row read from spool, shard, or both is one logical row |
| `append_events` | one `os.write` per line in `O_APPEND`; a campaign is exactly the concurrent-writer case that would otherwise tear a line |
| `verify_append_only` | a rewritten or deleted line stays a **detectable violation**, not an ordinary update |
| `parse_line`, `read_shard` | one definition of what a line is |

New here, because they genuinely differ: the path root, the envelope version
(`stress-series/v1`), the row identity, and the linter.

## The row

Envelope keys are **the same set** the validate ledger uses, so `union_events`
orders and dedups these rows with no series-specific branch. The payload lives
under `series`:

```json
{"schema":"stress-series/v1","event_id":"…","event_type":"series.observation",
 "emitted_at":"…","team":"hermit","host":"devbig014","run_id":"campaign-1",
 "producer":"pressure-test",
 "series":{"cell":"backend-parity-c/flock-lifecycle/verify/ptrace",
           "tree":"<40-hex>","run_index":0,"outcome":"diverged",
           "coordinates":{"first_divergent_record":98,
                          "first_divergent_syscall":37,
                          "first_divergent_scheduler_turn":4,
                          "first_divergent_virtual_nanoseconds":null}}}
```

`run_id` identifies one validate invocation, pressure-test campaign, or
`hermit-repeat` sweep.
`run_index` is copied from the validate result's positive `attempt` or supplied
from the pressure-test repetition directory or `hermit-repeat` run number. The
writer reads the producers' retained result rows; it does not maintain another
cell-result file. New rows may carry `detcore_tree`, the object id required to
create a scorecard observation keyed by Detcore tree. It is optional only so
the already-published rows remain readable; a projection cannot invent it for
an older row that omitted it.
`num_runs` defaults to one; a value greater than one represents that many
consecutive equal executions starting at `run_index`. Only wall time is folded
as a minimum/maximum. Outcome, coordinates, source depth, main ancestry, and
the two Hermit executions' scheduler turns, virtual nanoseconds, and syscall
counts must all match before rows can be combined. A failed retry followed by a
successful retry therefore remains two ordered rows.

The payload also carries the scorecard's existing source-depth shape:
`depth.<repository>.{commits,first_parent}`. `main_ancestry` records whether the
Hermit tree was on `origin/main` when written. The history command checks the
current `origin/main` again, because a measured branch can land later.

## Recurring pressure sample

`python3 ci-hub/series/pressure_schedule.py launch` is the daily tick-hub entry
point for one bounded sample of ten red cells with executable commands. It
launches detached, checks out current Hermit main under `ignored/`, acquires the existing
`validate-lock` as a `bench` run, retains the pressure-test result directory,
and invokes the existing `emit-series` command. The fixed systemd unit prevents
two samples from overlapping, and the lock keeps the sample from running beside
a validate.

## One test's complete history

From the dev-hermit root:

```sh
python3 ci-hub/series/series.py history \
  --test applications/git-repository-workflow
```

This freshly fetches parent `origin/main`, reads its published shards plus the
unpublished and retained local spool, prints the number of stored rows and
represented runs, starts with each cell's all-history
`wall_time_min_ms` and `wall_time_max_ms`, then shows each row's source SHA,
current/on-write main ancestry, depth, outcome, run span, `run1`/`run2`
scheduler turns, virtual nanoseconds, syscall counts, wall-time range, producer,
and run id. An empty result prints `no runs recorded` and exits nonzero.

## Derived flaky cells

Regenerate the measured-flake registry from the published series with:

```sh
python3 ci-hub/validate/flaky_cells.py derive --parent .
```

The command groups rows by exact `cell` and exact Hermit `tree`, requires both
`passed` and `diverged` in that population, expands `num_runs`, and records
`observed_pass`, `observed_fail`, and `sample_size`. `no_result`, `timeout`,
`errored`, `skipped`, and dirty-source rows contribute nothing. A small sample
records observed instability only; it is not presented as a failure-rate
estimate.

## Raw positions, not bounds

Coordinates are stored as **raw positions, one value per run**. `{earliest,
latest}` is a **derived view** (`observed_range`), never a stored field — that
is step 5, and storing the bound instead would discard the distribution the
store exists to hold.

⚠️ **The four coordinates are four different keyspaces and must never be
compared across axes.** One real divergence was record 98, syscall 37, scheduler
turn 4 — three numbers for one event. `observed_range` therefore takes exactly
one coordinate; its signature does not permit a `min()` across axes.

## Four states, readable from the rows alone

`measurement_state` is the single definition every consumer should use, so step
6 does not need a second opinion:

* `never-measured` — no rows
* `measured-and-passed` — rows, none diverged
* `diverged-unlocated` — diverged, no coordinate located
* `diverged` — diverged with at least one located coordinate

`diverged-unlocated` is a **legitimate distinct answer**, not an error. The
linter refuses coordinates on a row that did **not** diverge, but deliberately
permits divergence with no coordinate: refusing it would force a producer to
invent a position it does not have.

## Linter and reader are separate, as in the ledger

`validate_row` refuses at the **write** boundary. Readers apply whatever is in
the log without re-litigating it — a reader that enforced the lint could not
replay already-written history. `observed_range` therefore filters on `outcome`
itself, so a row written before a rule existed cannot leak a position into a
derived range.

## Running the tests

```sh
python3 ci-hub/series/tests/test_series_invariants.py
```

Every guard is demonstrated **refusing**, not merely passing.
