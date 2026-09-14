# Stress / flakiness series store

Append-only, Git-backed history of **repeated** cell measurements. Each row
represents one run, or a set of exactly equal observations when `num_runs` is
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

New here, because they genuinely differ: the path root and the row identity.
Hermit's `hermit_manifest_plan::stress_series::SeriesRow` owns the serialized
row type and write checks; the parent invokes that type before appending.

## The row

Envelope keys are **the same set** the validate ledger uses, so `union_events`
orders and dedups these rows with no series-specific branch. The payload lives
under `series`:

```json
{"schema":"stress-series/v3","event_id":"…","event_type":"series.observation",
 "emitted_at":"…","team":"hermit","host":"devbig014","run_id":"campaign-1",
 "producer":"pressure-test",
 "series":{"cell":"backend-parity-c/flock-lifecycle/verify/ptrace",
           "tree":"<40-hex>","run_index":0,"outcome":"diverged",
           "result":"determinism-failure","failure_class":"product_failure",
           "machine_shortname":"devbig014","kernel_version":"7.1.3-…",
           "host_capabilities":{
             "cpuid-faulting":{"present":false,"evidence":"…"},
             "kvm":{"present":true,"evidence":"…"}},
           "coordinates":{"first_divergent_record":98,
                          "first_divergent_syscall":37,
                          "first_divergent_scheduler_turn":4,
                          "first_divergent_virtual_nanoseconds":null},
           "first_divergent_messages":{"left":"inbound read",
                                       "right":"finish read"}}}
```

`run_id` identifies one validate invocation, pressure-test campaign, or
`hermit-repeat` sweep.
`machine_shortname`, `kernel_version`, and `host_capabilities` identify the
machine population in which the measurement was taken. The capability field is
the complete closed set of verdicts used by the harness, including the evidence
behind each verdict; it is not inferred from the hostname or kernel. The
envelope `host` must equal `machine_shortname`. The
`series/<team>/<host>/<month>.jsonl` path is a storage index derived from that
field, not a second source of measurement identity; copying or republishing a
row does not change the machine it describes. Retained `stress-series/v1` rows
remain readable, but the scorecard does not admit them as measurement evidence
because they do not record the kernel or capability set.
Retained `stress-series/v2` rows remain readable with their former coarse
`outcome`; `stress-series/v3` requires the framework-written exact `result` and
`failure_class`. New projection therefore preserves `crash-error`, timeout,
out-of-memory, understood infrastructure failure, understood prerequisite
failure, and genuine no-result observations instead of reconstructing a result
from the broad outcome after execution.
`run_index` is copied from the validate result's positive `attempt` or supplied
from the pressure-test repetition directory or `hermit-repeat` run number. The
writer reads the producers' retained result rows; it does not maintain another
cell-result file. New rows may carry `detcore_tree`, the object id required to
create a scorecard observation keyed by Detcore tree. It is optional only so
the already-published rows remain readable; a projection cannot invent it for
an older row that omitted it.

A new comparison-mode row that reached no canonical verdict carries
`no_verdict_evidence`: the SHA-256 identity of the complete retained result row
and the typed reason plus process disposition of each inner invocation that did
not compare. Such rows also carry an explicit positive outer `attempt` and are
never compressed.
This is what lets the scorecard distinguish a producer-recorded timeout from an
unavailable result such as KVM exit 125 without inferring from backend, duration,
or exit code. Historical v3 rows predate the optional field and remain readable,
but they cannot be projected as measured no-verdict evidence.
`num_runs` defaults to one; a value greater than one represents that many
exactly equal observations between `run_index` and `last_run_index`. Only wall
time is folded as a minimum/maximum. Outcome, all four measured divergence
coordinates, the two compared messages, source depth, main ancestry, and the two
Hermit executions' scheduler turns, virtual nanoseconds, and syscall counts must
all match before rows can be combined. Rows that carry an explicit retry
`attempt` are never combined, so a failed retry followed by a successful retry
remains two ordered rows.

New pressure rows also retain `pressure_evidence`: a digest identifying the
original CellResult and the complete inner invocation history, including each
report digest, comparison verdict and process disposition. These rows keep an
explicit outer attempt and remain separate. Ordinary repeat rows still use
the compression rules above. Historical pressure rows without this field stay
readable, but cannot prove a complete history for promotion.

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

`classify_pressure_sample` reduces one fixed campaign from the accepted series
projection, never from the website's display state. Its denominator is exactly
ten distinct repetition numbers for every cell in the frozen selection:

* ten clean, first-attempt product passes with complete inner process evidence
  and canonical reports in comparison modes is a `promotion-candidate`;
* one through nine passes, a pass recovered by retry, or a terminal pass with
  an adverse inner observation is `intermittent`;
* zero passes and ten complete histories containing only product failures and
  successful declared subruns is `confirmed-failing`;
* ten infrastructure failures, prerequisite failures, or no-results retain
  those three separate classifications; and
* missing or duplicate repetitions, gapped retries, mixed non-product results,
  and every other incomplete shape are `incomplete`.

The result reports every count and a separate `promotion_candidate` boolean.
It does not edit `ci/expected-e2e-plan.json`; changing the selected cohort is a
later reviewed operation. Exact duplicate events produced by publication replay
are deduplicated by event ID, while distinct events claiming the same
cell/repetition/attempt make that cell incomplete.
Several successful declared subruns are valid and do not count as outer
retries. Missing history and dirty source remain incomplete. The classifier
keeps infrastructure, prerequisite and unavailable-result observations distinct
when a failed outer result contains several inner observations; conflicting
failure categories remain incomplete. It preserves the framework's original
result and never turns absent host-inapplicable rows into measurements.

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
current/on-write main ancestry, depth, outcome, exact result, failure class, run
span, `run1`/`run2`
scheduler turns, virtual nanoseconds, syscall counts, wall-time range, all four
divergence coordinates, the two compared messages, producer, and run id. An
empty result prints `no runs recorded` and exits nonzero.

## Immutable scorecard input snapshot

The dormant scorecard transaction can obtain one run-scoped input without
reading a moving checkout or falling back to checked-in observations:

```sh
python3 ci-hub/series/series.py snapshot \
  --parent . \
  --output ignored/validate/<run>/scorecard-series-snapshot.json
```

The command refreshes parent `origin/main` before and after local capture,
requires both reads to name the same exact commit and `series/` tree, and
combines that immutable published snapshot with one stable generation of the
local live and published-retained spool. It validates and orders every row
through the canonical series union and records the row count and digest. Each
local input is opened through its parent directory with symlinks forbidden,
named by its relative path, device, inode, byte size, and digest, and held open
through publication and readback.

Local capture uses only short bounded shared-lock phases; both network refreshes,
source reads, parsing, validation, hashing, and output publication run without
the lock. Each lock phase has an explicit source-count limit and monotonic
deadline and measures only names and file metadata. That exact identity is
checked before and after the outside-lock content capture and again after the
second refresh. The held descriptors and content digests are then verified
through output readback. A publisher transition, append, changed source name,
changed inode, changed byte sequence, or changed source set makes the snapshot
unavailable instead of combining a stale published commit with an incomplete
local spool. This command does not alter publisher retention or reaping.

Each shared-lock identity phase streams directory entries and refuses after
one second or after 4096 total entries across the live and published-retained
directories. The count includes non-JSONL entries, and both limits are checked
before sorting or materializing the complete source list. Source contents,
network refreshes, validation, and output publication remain outside the lock.

The output is fsynced in an unnamed temporary inode where supported, linked
into place without replacing an existing file, then read back through a held
no-follow descriptor before success is reported. The named fallback uses an
exclusive unpredictable staging name and an atomic no-replace move. A failure
after publication leaves the final evidence in place for inspection; no error
path unlinks a pathname that another process could have replaced. If the
kernel lacks the no-replace move and a hardlink fallback leaves two names, the
operation preserves both and reports non-success instead of claiming a clean
publication with staging residue.

This command is an additive input provider only. It does not change selection
or observation authority, does not write or separate the tracked
`SCORECARD.md` and `ci/compat-envelope/cells.json` pair, and does not change
their lifetime. If the published source or any local input cannot be read and
validated, the command reports failure without claiming whether the final
output exists. Inspect and preserve the named output and any sibling staging
file before retrying with a new run-scoped output path. It never substitutes
stale tracked output.

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

Coordinates are stored as **raw measured positions**. Rows combine only when
all four positions and the compared messages are exactly equal, so `num_runs`
is the count at that exact point. `{earliest, latest}` is a **derived view**
(`observed_range`), never a stored field — storing a bound instead would discard
the distribution the store exists to hold.

Distance-based collapsing is deliberately not part of the stored schema. If a
large sample shows scatter to be the common case, a reader may later apply a
tunable distance along one timeline dimension. The stored rows retain the
measured points so changing that policy does not rewrite evidence.

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

## Write and read validation

`validate_row` refuses at the **write** boundary. Stored rows use the shared
type's separate read check: retained v1 rows remain readable, while every v2
row must satisfy the complete v2 shape, including every named host capability.
That distinction preserves history without letting a current-schema row omit a
fact and still count as evidence.

Every production consumer of the row shape reaches one of these checks:

* the publisher validates every proposed new row with `validate_row` before it
  can enter a tracked shard;
* `union` and `published_rows` call `validate_read_rows` before the history
  command or flaky-cell derivation receives stored rows;
* `flaky_cells.derive` also calls `validate_read_rows`, so direct callers cannot
  bypass the stored-row check; and
* Hermit's scorecard deserializes the shared `SeriesRow` and calls
  `validate_for_projection`, which additionally refuses v1 and dirty-source
  rows as scorecard evidence.

`append-cells` keeps the same whole-batch refusal for malformed provenance,
duplicate identities, changed replays, and every other invalid row. One narrow
historical case is recoverable: in a pressure-test campaign, a comparison cell
with no typed attempts is rejected as non-qualifying while independent typed
cells are appended together. The command prints a structured identity and
SHA-256 of each rejected retained CellResult and exits nonzero. Re-emitting the
campaign is idempotent, and the rejected raw result is never synthesized into a
series observation.

`parse_line` and `read_shard` remain raw line-format plumbing inherited from the
ledger; they do not interpret a row as measurement evidence. `observed_range`
still filters on `outcome` itself so a retained row cannot leak a position into
a derived range.

## Running the tests

```sh
python3 ci-hub/series/tests/test_series_invariants.py
```

Every guard is demonstrated **refusing**, not merely passing.
