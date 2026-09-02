# `cells.json` and cell-result series: what changes when

Measured 2026-09-02 against Hermit `origin/main`
`8f0607348e252372b0286c0f3b590e6654822289` and dev-hermit
`origin/main` `5bfa5d91f56e380631a549bf8dcf58631c97dabd`.

## The number that explains the churn

One ordinary validate, run
`validate-current-tip-full-validate-07c10eb80ac4-1788143718039901059-4036218-e4cf26b8`,
wrote **353 cell-result series rows / 458,409 bytes**. The same run's direct
scorecard writeback added **350 `ObservedInvocation` records and 505
`ObservedAttemptInvocation` records** to the tracked
`ci/compat-envelope/cells.json`. Three series cells no longer existed in the
scorecard population and were not projected.

That one run changed `cells.json` from **15,127,455 to 19,901,322 bytes**:
**+4,773,867 bytes**, with **82,654 insertions and 21,174 deletions**. This is
not a hypothetical extrapolation: the working-tree delta contains exactly one
new `run_id`, the one named above.

Commands and abbreviated output:

```text
$ wc -lc ignored/ci-hub/series-spool/published/validate-current-tip-full-validate-07c10eb80ac4-1788143718039901059-4036218-e4cf26b8.jsonl
353 458409 ...jsonl

$ git diff --numstat -- ci/compat-envelope/cells.json
82654  21174  ci/compat-envelope/cells.json

$ git cat-file -s HEAD:ci/compat-envelope/cells.json; wc -c ci/compat-envelope/cells.json
15127455
19901322 ci/compat-envelope/cells.json
```

The result answers the lifetime question directly: the committed scorecard is
currently carrying per-run process records. The line churn is a consequence of
that placement, not a separate formatting problem.

## The two stores as they exist now

| Store | Current location and persistence | Unit | Current role |
|---|---|---|---|
| Compatibility scorecard | Hermit `ci/compat-envelope/cells.json`; tracked in Git | One row per manifest cell, with nested accumulated observations | Cell identity and selection state **plus** a projection and direct writeback of run evidence |
| Cell-result series | dev-hermit `series/<team>/<host>/<YYYY>-<MM>.jsonl`; tracked in Git after publication. Before publication, `ignored/ci-hub/series-spool/` is outside Git | One append-only row per cell per repetition, with exact duplicates optionally represented by `num_runs` | Per-run measurement history |

The second row is an important correction to the premise: the live spool is
outside version control, but the published series is currently committed to
the parent repository. Therefore neither durable store currently satisfies a
strict requirement that per-run results remain outside version control.

The Rust schema roots are:

- [`TrackedCells`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/compat-envelope/scorecard.rs#L141-L149), with `TrackedCell` at lines 209-270, `Observation` at lines 461-523, `ObservedInvocation` at lines 537-561, and `ObservedAttemptInvocation` at lines 563-575.
- [`SeriesRow`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L265-L278), with `SeriesPayload` at lines 208-262, `SeriesRuntime` at lines 149-160, and `SeriesAttemptDisposition` at lines 178-202.

The scorecard schema is private to the `scorecard.rs` executable; there is no
single reusable public type for it. The series type is shared and is the schema
authority used by the parent writer.

| Serialized type | Definition |
|---|---|
| `TrackedCells`, `ObservationProjection` | [`scorecard.rs:141`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/compat-envelope/scorecard.rs#L141-L207) |
| `TrackedCell`, `CiDisabledReasonData`, `CellStatus` | [`scorecard.rs:209`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/compat-envelope/scorecard.rs#L209-L304) |
| `ObservationProvenance`, `MeasurementState` | [`scorecard.rs:316`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/compat-envelope/scorecard.rs#L316-L411) |
| `Observation`, `CanonicalComparison`, `ObservedInvocation`, `ObservedAttemptInvocation` | [`scorecard.rs:461`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/compat-envelope/scorecard.rs#L461-L575) |
| `LastTested`, `ObservedRange`, `ObservedPositions` | [`scorecard.rs:608`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/compat-envelope/scorecard.rs#L608-L747) |
| `SeriesSchema`, `SeriesProducer`, `SeriesOutcome` | [`stress_series.rs:29`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L29-L91) |
| `SourceDepth`, `SeriesCoordinates`, `FirstDivergentMessages` | [`stress_series.rs:93`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L93-L129) |
| `SeriesRuntimeMeasurement`, `SeriesRuntime` | [`stress_series.rs:138`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L138-L160) |
| `SeriesNoVerdictKind`, `SeriesAttemptDisposition`, `SeriesNoVerdictEvidence` | [`stress_series.rs:162`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L162-L202) |
| `SeriesPayload`, `SeriesRow` | [`stress_series.rs:208`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L208-L278) |
| `ObservedResult`, `FailureClass` | [`runner.rs:810`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/runner.rs#L810-L900) |
| `HostCapability`, `CapabilityVerdict`, `HostCapabilities` | [`host_capability.rs:19`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/detcore-model/src/host_capability.rs#L19-L52) |

## Field-by-field lifetime: `cells.json`

In the table, **yes** means that axis can change the value. “Every run” means a
normal top-level validate can add or replace the value without any test
definition or green/red transition. A blank means that axis does not move it.

| JSON field | Rust owner | Every run | Green/red or enabled/disabled | Test definition | What it records |
|---|---|:---:|:---:|:---:|---|
| `schema` | `TrackedCells` |  |  |  | Scorecard format version; changes only with the schema |
| `projection.source` | `ObservationProjection` |  |  |  | Repository-relative series source; changes only with storage configuration |
| `projection.source_commit` | `ObservationProjection` | on explicit projection |  |  | Parent commit projected |
| `projection.source_tree` | `ObservationProjection` | on explicit projection |  |  | Exact source subtree identity |
| `projection.refreshed_at` | `ObservationProjection` | on explicit projection |  |  | Projection publication time |
| `projection.rows_read` | `ObservationProjection` | on explicit projection |  |  | Series rows read by that projection |
| `projection.pre_series_corpus` | `ObservationProjection` | on migration/projection |  |  | Whether older scorecard-only evidence remains |
| `cells[].lane` | `CellId` |  |  | yes | Manifest cell identity |
| `cells[].category` | `CellId` |  |  | yes | Manifest cell identity |
| `cells[].test` | `CellId` |  |  | yes | Manifest test identity |
| `cells[].mode` | `CellId` |  |  | yes | Manifest execution mode |
| `cells[].backend` | `CellId` |  |  | yes | Manifest backend |
| `cells[].enabled` | `TrackedCell` |  | yes | yes | Whether that backend applies to the cell |
| `cells[].status` | `TrackedCell` / `CellStatus` |  | yes | yes | `green`, `red`, or derived `not-applicable`. A failed run does **not** demote green to red. |
| `cells[].ci_disabled_reason.result` | `CiDisabledReasonData` |  | yes | yes | Result attached to a CI-disable decision |
| `cells[].ci_disabled_reason.evidence` | `CiDisabledReasonData` |  | yes | yes | Evidence attached to that decision |
| `cells[].ci_disabled_reason.reason` | `CiDisabledReasonData` |  | yes | yes | Human reason for the decision |
| `cells[].green_removal_reason` | `TrackedCell` |  | yes |  | Recorded override for green-to-red removal |
| `cells[].not_applicable_reason` | `TrackedCell` |  | yes | yes | Manifest reason the backend/mode does not apply |
| `cells[].last_tested.hermit_sha` | `LastTested` | yes |  |  | Latest admitted tested Hermit revision |
| `cells[].last_tested.detcore_tree` | `LastTested` | yes |  |  | Content identity used for staleness |
| `cells[].last_tested.depth.<repo>.commits` | `LastTested` / `SourceDepth` | yes |  |  | Total reachable source depth at that run |
| `cells[].last_tested.depth.<repo>.first_parent` | `LastTested` / `SourceDepth` | yes |  |  | Mainline source depth at that run |
| `cells[].observations[].detcore_tree` | `Observation` | yes |  |  | Code identity for a measured observation |
| `cells[].observations[].event_ids[]` | `Observation` | yes |  |  | Exact series events folded into the projection |
| `cells[].observations[].provenance` | `Observation` / `ObservationProvenance` | yes |  |  | `validate`, `pressure-test`, or `hermit-repeat` |
| `cells[].observations[].depth.<repo>.commits` | `Observation` / `SourceDepth` | yes |  |  | Total reachable source depth |
| `cells[].observations[].depth.<repo>.first_parent` | `Observation` / `SourceDepth` | yes |  |  | Mainline source depth |
| `cells[].observations[].hermit_shas[]` | `Observation` | yes |  |  | Hermit revisions represented by the observation |
| `cells[].observations[].results[]` | `Observation` / `ObservedResult` | yes |  |  | Pass/divergence/crash/timeout/OOM result set |
| `cells[].observations[].canonical_comparisons[].hermit_sha` | `CanonicalComparison` | yes |  |  | Exact tested revision |
| `...canonical_comparisons[].hermit_commits` | `CanonicalComparison` | yes |  |  | Reachable-history depth |
| `...canonical_comparisons[].hermit_first_parent` | `CanonicalComparison` | yes |  |  | First-parent depth |
| `...canonical_comparisons[].run_id` | `CanonicalComparison` | yes |  |  | Validation invocation identity |
| `...canonical_comparisons[].evidence_sha256` | `CanonicalComparison` | yes |  |  | Exact retained evidence identity |
| `...canonical_comparisons[].result` | `CanonicalComparison` | yes |  |  | Framework result |
| `...canonical_comparisons[].left_info_messages[]` | `CanonicalComparison` | yes |  |  | INFO-message counts from the first comparison side |
| `...canonical_comparisons[].right_info_messages[]` | `CanonicalComparison` | yes |  |  | INFO-message counts from the second comparison side |
| `cells[].observations[].invocations[].hermit_sha` | `ObservedInvocation` | yes |  |  | Per-run tested revision |
| `...invocations[].run_id` | `ObservedInvocation` | yes |  |  | Per-run identity |
| `...invocations[].attempt` | `ObservedInvocation` | yes |  |  | Outer framework attempt |
| `...invocations[].evidence_sha256` | `ObservedInvocation` | yes |  |  | Complete result-row identity |
| `...invocations[].result` | `ObservedInvocation` | yes |  |  | Per-run result or measured no-verdict |
| `...invocations[].argv[]` | `ObservedInvocation` | yes |  | yes | Per-run command; definition changes affect later copies |
| `...invocations[].guest_argv[]` | `ObservedInvocation` | yes |  | yes | Per-run guest command |
| `...invocations[].env{}` | `ObservedInvocation` | yes |  | yes | Per-run environment |
| `...invocations[].cwd` | `ObservedInvocation` | yes |  |  | Per-run working directory |
| `...invocations[].shell_command` | `ObservedInvocation` | yes |  | yes | Per-run rendered command |
| `...invocations[].attempts[].index` | `ObservedAttemptInvocation` | yes |  |  | Inner invocation index |
| `...attempts[].outcome` | `ObservedAttemptInvocation` | yes |  |  | Inner invocation outcome |
| `...attempts[].status` | `ObservedAttemptInvocation` | yes |  |  | Process exit status |
| `...attempts[].signal` | `ObservedAttemptInvocation` | yes |  |  | Terminating signal |
| `...attempts[].timed_out` | `ObservedAttemptInvocation` | yes |  |  | Timeout fact |
| `...attempts[].argv[]` | `ObservedAttemptInvocation` | yes |  | yes | Inner invocation command |
| `...attempts[].guest_argv[]` | `ObservedAttemptInvocation` | yes |  | yes | Inner guest command |
| `...attempts[].env{}` | `ObservedAttemptInvocation` | yes |  | yes | Inner invocation environment |
| `...attempts[].cwd` | `ObservedAttemptInvocation` | yes |  |  | Inner working directory |
| `...attempts[].shell_command` | `ObservedAttemptInvocation` | yes |  | yes | Inner rendered command |
| `cells[].observations[].first_divergent_{scheduler_turn,virtual_nanoseconds,record,syscall}.positions[]` | `ObservedPositions` | yes |  |  | One measured position per locating run |
| `...first_divergent_*.legacy_bounds.earliest` | `ObservedPositions` / `ObservedRange` | migration only |  |  | Pre-series lower bound |
| `...first_divergent_*.legacy_bounds.latest` | `ObservedPositions` / `ObservedRange` | migration only |  |  | Pre-series upper bound |
| `...first_divergent_*.legacy_bounds.samples` | `ObservedPositions` / `ObservedRange` | migration only |  |  | Pre-series located-sample count |
| `cells[].measurement` | `TrackedCell` / `MeasurementState` | yes, when new evidence changes the derived value |  |  | Derived `never-measured`, pass, no-verdict, or divergence state |

The large fields are measurable. At current `origin/main`, `cells.json` is
20,244,741 bytes. Its 7,466 observation values occupy 12,669,833 bytes in
compact JSON; 1,025 invocation values alone occupy 9,854,346 bytes, including
1,490 nested attempt values occupying 5,803,114 bytes. A mechanically
serialized file retaining all 5,744 identities and state but no run data is
about 1,888,194 bytes. Retaining only the 676 enabled cells and their identity
and state is about **241,686 bytes**. Those two latter figures use Python JSON
serialization to compare content populations; they are not claimed as exact
future Rust-writer output sizes.

## Field-by-field lifetime: cell-result series

The complete serialized type is
[`SeriesRow`](https://github.com/rrnewton/hermit/blob/8f0607348e252372b0286c0f3b590e6654822289/ci/manifest-plan/src/stress_series.rs#L265-L278).
Every table row below is appended per run (or represents identical runs through
`num_runs`); no field in this store controls green/red or enabled/disabled.

| JSON field | Rust owner | Every run | Green/red or enabled/disabled | Test definition | What it records |
|---|---|:---:|:---:|:---:|---|
| `schema` | `SeriesRow` / `SeriesSchema` | repeated |  | schema only | `stress-series/v1`, `v2`, or current `v3` |
| `event_id` | `SeriesRow` | yes |  |  | Immutable event identity |
| `event_type` | `SeriesRow` | repeated |  | schema only | `series.observation` discriminator |
| `emitted_at` | `SeriesRow` | yes |  |  | Publication timestamp |
| `team` | `SeriesRow` | repeated |  | organizational | Owning team |
| `host` | `SeriesRow` | yes |  |  | Measurement host; must equal `machine_shortname` |
| `producer` | `SeriesRow` / `SeriesProducer` | yes |  |  | `validate`, `pressure-test`, or `hermit-repeat` |
| `run_id` | `SeriesRow` | yes |  |  | Physical invocation/campaign identity |
| `series.cell` | `SeriesPayload` | repeated |  | yes | Durable cell identity encoded as test/mode/backend |
| `series.tree` | `SeriesPayload` | yes |  |  | Hermit source revision measured |
| `series.detcore_tree` | `SeriesPayload` | yes |  |  | Optional exact Detcore tree |
| `series.outcome` | `SeriesPayload` / `SeriesOutcome` | yes |  |  | Passed, diverged, no-result, timeout, errored, or skipped |
| `series.result` | `SeriesPayload` / `ObservedResult` | yes |  |  | Exact framework result |
| `series.failure_class` | `SeriesPayload` / `FailureClass` | yes |  |  | Product, understood infrastructure, understood prerequisite, or no-result attribution |
| `series.no_verdict_evidence.evidence_sha256` | `SeriesNoVerdictEvidence` | yes when applicable |  |  | Complete retained source-row identity |
| `...no_verdict_evidence.attempts[].index` | `SeriesAttemptDisposition` | yes when applicable |  |  | Inner invocation index |
| `...attempts[].kind` | `SeriesAttemptDisposition` | yes when applicable |  |  | Typed no-verdict reason |
| `...attempts[].attempt_outcome` | `SeriesAttemptDisposition` | yes when applicable |  |  | Producer's attempt outcome |
| `...attempts[].disposition` | `SeriesAttemptDisposition` | yes when applicable |  |  | Series outcome for the attempt |
| `...attempts[].error_kind` | `SeriesAttemptDisposition` | yes when applicable |  |  | Optional process error kind |
| `...attempts[].status` | `SeriesAttemptDisposition` | yes when applicable |  |  | Exit status |
| `...attempts[].signal` | `SeriesAttemptDisposition` | yes when applicable |  |  | Terminating signal |
| `...attempts[].timed_out` | `SeriesAttemptDisposition` | yes when applicable |  |  | Timeout fact |
| `...attempts[].verification_report_sha256` | `SeriesAttemptDisposition` | yes when applicable |  |  | Verification-report identity |
| `series.run_index` | `SeriesPayload` | yes |  |  | Repetition/attempt ordinal |
| `series.attempt` | `SeriesPayload` | yes when a retry is explicit |  |  | Outer framework attempt; prevents compression |
| `series.num_runs` | `SeriesPayload` | yes |  |  | Number of exactly equal observations represented |
| `series.last_run_index` | `SeriesPayload` | yes when compressed |  |  | End of a compressed equal-result span |
| `series.main_ancestry` | `SeriesPayload` | yes |  |  | Whether `tree` was on main when written |
| `series.runtime.run1.scheduler_turns` | `SeriesRuntimeMeasurement` | yes |  |  | First execution's scheduler turns |
| `series.runtime.run1.virtual_nanoseconds` | `SeriesRuntimeMeasurement` | yes |  |  | First execution's virtual time |
| `series.runtime.run1.syscalls` | `SeriesRuntimeMeasurement` | yes |  |  | First execution's syscall count |
| `series.runtime.run2.scheduler_turns` | `SeriesRuntimeMeasurement` | yes |  |  | Second execution's scheduler turns |
| `series.runtime.run2.virtual_nanoseconds` | `SeriesRuntimeMeasurement` | yes |  |  | Second execution's virtual time |
| `series.runtime.run2.syscalls` | `SeriesRuntimeMeasurement` | yes |  |  | Second execution's syscall count |
| `series.runtime.wall_time_min_ms` | `SeriesRuntime` | yes |  |  | Minimum wall time represented |
| `series.runtime.wall_time_max_ms` | `SeriesRuntime` | yes |  |  | Maximum wall time represented |
| `series.source_tree_dirty` | `SeriesPayload` | yes |  |  | Whether source had uncommitted changes |
| `series.depth.<repo>.commits` | `SeriesPayload` / `SourceDepth` | yes |  |  | Total reachable source depth |
| `series.depth.<repo>.first_parent` | `SeriesPayload` / `SourceDepth` | yes |  |  | First-parent source depth |
| `series.coordinates.first_divergent_scheduler_turn` | `SeriesCoordinates` | yes when divergent |  |  | Raw measured scheduler position |
| `series.coordinates.first_divergent_virtual_nanoseconds` | `SeriesCoordinates` | yes when divergent |  |  | Raw measured virtual-time position |
| `series.coordinates.first_divergent_record` | `SeriesCoordinates` | yes when divergent |  |  | Raw compared-record position |
| `series.coordinates.first_divergent_syscall` | `SeriesCoordinates` | yes when divergent |  |  | Raw syscall position |
| `series.first_divergent_messages.left` | `FirstDivergentMessages` | yes when divergent |  |  | First differing message, first side |
| `series.first_divergent_messages.right` | `FirstDivergentMessages` | yes when divergent |  |  | First differing message, second side |
| `series.machine_shortname` | `SeriesPayload` | yes |  |  | Machine identity |
| `series.kernel_version` | `SeriesPayload` | yes |  |  | Kernel identity |
| `series.host_capabilities.<cpuid-faulting|kvm>.present` | `HostCapabilities` / `CapabilityVerdict` | yes |  |  | Capability result |
| `series.host_capabilities.<cpuid-faulting|kvm>.evidence` | `HostCapabilities` / `CapabilityVerdict` | yes |  |  | Evidence supporting that result |

The `source` member of the Rust `SeriesRow` is explicitly `#[serde(skip)]`; it
is reader provenance and is not a stored JSON field.

## The split by lifetime

The measured boundary is:

| Lifetime | Belongs in sparse `cells.json` | Belongs with per-run evidence |
|---|---|---|
| Test definition changes | Cell identity (`lane`, `category`, `test`, `mode`, `backend`) for cells that the scorecard actually needs; schema version | The `cell` identity repeated on each immutable result row |
| Test-state changes | `enabled`, `status`, `ci_disabled_reason`, `green_removal_reason`, `not_applicable_reason` | Nothing; a run result must not silently change admission state |
| Every run | At most a small, deliberately refreshed summary such as `last_tested`/`measurement`, if an in-repository view is required | Event/run identity, exact source and host identity, outcomes, failure attribution, runtime measurements, divergence positions, and process disposition |

The current implementation crosses that boundary at
`ObservedInvocation` and `ObservedAttemptInvocation`: complete command lines,
environment maps, working directories, exit statuses, signals, timeout flags,
and repeated command copies are accumulated inside the tracked Hermit file.
Those are run facts. The parent series has the typed result, runtime, host, and
divergence data, but it does **not** retain successful-run `argv`, `guest_argv`,
`env`, `cwd`, or `shell_command`; those remain in raw ignored result artifacts.
Moving the scorecard copies out therefore requires naming which outside-Git
raw evidence is retained, rather than pretending the current series already
contains every field.

Sparseness is independent of the manifest. Current `origin/main` has 5,744
scorecard rows: 676 enabled and 5,068 `not-applicable`. Omitting those 5,068
rows from `cells.json` does not delete or alter the manifest; it only stops
materializing its negative space in this derived file.

## Parse cost and multiplicity

The production consumer measured was:

```text
$ /usr/bin/time -f 'elapsed=%e user=%U sys=%S maxrss_kb=%M' \
    ./ci/compat-envelope/scorecard.rs show >/dev/null
warm compile: 33.74 s
first post-compile execution: 9.35 s
next four executions: 0.74, 0.74, 0.75, 0.73 s
resident set on those four: 65,464-67,264 KiB
```

This is the real `scorecard.rs` consumer over the 19,901,322-byte working file,
not a hand-written `json.load`. `show` calls `load_existing` once. The steady
**0.73-0.75 seconds is end-to-end** derive, parse, validate, and render time; I
could not isolate serde parse time without instrumenting the production source,
so it is an upper bound on one parse rather than a mislabeled parse-only number.

A top-level receipt-producing ordinary validate deserializes the file **four
times in the direct result path**:

1. `verify-results` calls `check_tracked`, which calls `load_existing` once.
2. `verify-results` then calls `load_existing` again to render measurement state.
3. Post-run `observe-results` calls `check_tracked`, which calls `load_existing` once.
4. `observe-results` then deserializes the previously read bytes again before folding results.

Nested and off-the-record validates skip the post-run writeback and therefore
skip the latter two. This count excludes self-tests that deliberately exercise
the reader on fixtures.

## Git history and object cost

`btrfs filesystem du --raw -s` measured the complete primary Git directories:

| Repository | Btrfs total bytes | Btrfs exclusive bytes |
|---|---:|---:|
| dev-hermit parent `.git` | 26,197,016,576 | 26,197,016,576 |
| Hermit `.git` | 5,139,369,984 | 5,139,369,984 |
| Reverie `.git` | 261,046,272 | 261,046,272 |

These are this long-lived checkout's complete administrative directories, not
fresh-clone sizes. In particular, parent `.git` includes linked-worktree and
submodule administration. `git count-objects -vH` reported Hermit itself as
1.57 GiB loose plus 215.98 MiB packed; the discrepancy is why neither number is
silently substituted for the other.

For history reachable from Hermit `origin/main`, Git's own object accounting
(`git cat-file --batch-check='%(objectsize) %(objectsize:disk)'`) found:

- `cells.json`: **68 revisions**, **3** revisions changing more than 100,000
  lines, **593,608** lines changed in total, **271,418,979 logical blob bytes**,
  and **7,236,971 on-disk object bytes**.
- Across all local refs, including rescue and review refs: 266 revisions, 14
  over 100,000 changed lines, 706,362,485 logical blob bytes, and 26,695,477
  on-disk object bytes. This is not the main-history number.

No other path reachable from `origin/main` has a change over 100,000 lines.
The next largest single-file change is 10,048 lines in
`tests/e2e/manifests/c-programs.yaml`. By accumulated on-disk historical blob
cost, the next path after `cells.json` is `scripts/validate.rs` at 2,091,526
bytes across 167 blobs. Thus `cells.json` is the unique repeated giant-diff
file in main history, although Git delta-compresses its 271.4 MB of logical
versions to 7.24 MB in this object store. It is a major review/diff/parse churn
source; it is not, by itself, the explanation for the multi-gigabyte live
`.git` directory.

## What this report does not establish

- It does not choose the external non-Git retention service or retention period
  for raw per-run evidence. The current ignored result roots contain fields the
  published series deliberately omits.
- It does not claim the 0.73-0.75 second command time is serde alone; it is the
  measured production consumer's full steady-state time.
- It does not propose deleting manifest cells. A sparse scorecard and the
  manifest population are independent decisions.
- It does not propose rewriting history. All Git figures above describe the
  existing history only.
