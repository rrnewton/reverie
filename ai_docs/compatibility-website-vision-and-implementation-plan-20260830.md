# Compatibility website: product vision and implementation plan

Date: 2026-08-30

## Decision summary

The complete website will be built from the current Hermit manifest, the
canonical per-cell series ledger, the validation ledger, machine-readable
syscall metadata, and retained canonical logs. The last two inputs do not exist
in sufficient form today; the first releases build from the manifest and
ledgers and show those inputs as unavailable. The output is a directory of
static HTML, CSS, JavaScript, JSON, and gzip-compressed text. A top-level
`ci-hub` command builds it when its inputs have changed and serves it from a
small Rust HTTP server bound to loopback by default.

The website is a view, never an authority. It must be safe to delete and
regenerate. Cell state is computed from ledger rows on every build. In
particular, `never measured` is the absence of qualifying rows; it is not a
stored field. Syscall status comes from reviewed machine-readable metadata in
Hermit, while each linked GitHub issue explains why that metadata has its
current value. The website does not ingest or repeat issue commentary.

The first release should be useful before every producer gap is fixed. Missing
data is rendered explicitly and collected on an incomplete-data page. A value
that was not recorded is never shown as zero, inferred from prose, or borrowed
from a stale derived file. Closing those visible gaps is part of the product:
the website is also an audit of what Hermit actually records.

## Non-negotiable rules

1. The ledger is the source of truth for observations. `SCORECARD.md`,
   `ci/compat-envelope/cells.json`, HTML, search indexes, and aggregate counts
   are derived views.
2. The current manifest defines the current cell universe and current test
   metadata. It does not assert that a cell was measured.
3. `never measured` is represented by no qualifying ledger rows. No producer or
   renderer writes a measured/not-measured flag.
4. A partial run changes only cells selected by that run. The absence of a cell
   from a partial run is not a result for that cell.
5. Retries are reduced within one run: a retry that passes makes that run pass.
   Display state is not smoothed across runs.
6. The displayed state and the 10-of-10 enablement bar are separate derived
   answers. Passing the pressure bar does not silently edit the manifest.
7. The site contains machine-generated content only. It links to GitHub issues
   for reasoning; it does not scrape their bodies or turn prose into status.
8. Missing, malformed, stale, or unsupported evidence stays visible as such.
   It is never converted to green, red, zero, or `never measured` by default.
9. Same-backend verification must not be described as cross-backend parity.
10. All generated content is local and self-contained: no external fonts,
    scripts, analytics, telemetry, or runtime API dependency.

## Current state, grounded in the repository

The following dataset measurements were supplied with the task from
`devbig014` at 2026-08-30 18:36 UTC:

- The per-cell series ledger held 14,902 canonical rows representing 21,251
  run observations across 644 distinct cells. Of those physical rows, 7,883
  used schema v1, 4,923 used v2, and 2,096 used v3; readable history is not the
  same thing as projection-grade current evidence.
- The validation ledger held 1,821 canonical events, projecting to 1,669
  validation history rows.
- The current manifest-derived scorecard contains 5,744 cells: 676 enabled
  combinations and 5,068 categorical exclusions. Its 350 green rows are
  selected regression requirements, not 350 observed passes.
- Recent full validation logs were 2.777--3.285 MiB raw. Captured per-cell
  stdout/stderr accounted for 2.002 MiB raw and 0.340 MiB gzipped. Those runs
  retained zero verify-log files, so 3 MiB is a floor, not a storage estimate
  for the requested full-log website.

The supplied snapshot did not include its exact parent/Hermit commits, an
immutable capture of the live spool, exact commands, or source digests for
every input. The numbers are therefore the planning baseline requested by the
owner, not release evidence that a later build may cite. The site builder's
`build.json` must make future measurements reproducible by recording every
input commit, shard digest, spool-batch digest, command, and raw audit output.

The code establishes the following answers to the four questions that had to be
resolved before choosing an architecture.

| Question | Finding | Consequence |
| --- | --- | --- |
| Does `hermit run` emit machine-readable run metadata? | Yes, when `--summary-json PATH` is supplied. `RunSummary` in `hermit/detcore-model/src/summary.rs` contains scheduler turns, schedule-event counts, thread/process counts, intercepted syscall events counted at Detcore's prehook, elapsed/final virtual time, wall duration, and timeslice statistics. Non-DBT `hermit run --verify` creates private summaries and includes `runtime.run1` and `runtime.run2` in `--verify-json`. The public summary has no schema/version and is written only during cleanup, so a failed invocation can leave stale prior content at the requested path. It also omits backend, argv, exit status, INFO count, and a per-syscall histogram. | Reuse and harden this producer-owned type. Do not parse the human summary or accept an unbound leftover file. |
| Can Detcore's syscall histogram be extracted after a run? | No native histogram exists. `ThreadStats` in `hermit/detcore/src/tool_local.rs` stores a scalar count and has an explicit TODO to evolve it into a histogram. Human INFO lines name completed syscalls, but the structured DETLOG suffix carries only the completed-syscall ordinal. INFO logging is not guaranteed. Default ptrace-style execution observes the full pinned subscription, but explicit `--passthru-opt` and backend execution outside Detcore can bypass observation. | This requires a real producer and schema change. Parsing retained human logs is a diagnostic fallback, not website architecture. |
| Is there an existing logical syscall grouping? | Only in dated prose audits such as `hermit/docs/SYSCALL_COVERAGE.md` and `ai_docs/syscall-coverage-map.md`. The headings are useful seeds, but there is no current exhaustive machine-readable syscall-to-group mapping. | Add one reviewed mapping, checked against the complete pinned x86_64 syscall table. Do not parse a Markdown snapshot. |
| Which requested per-test statistics are recorded? | Scheduler turns, virtual nanoseconds, and total syscalls are typed in verification results and the current series schema. At the measured snapshot, run-1 values existed on 14,504 of 14,902 rows and run-2 values on 14,445; absence is historical/producer debt, not zero. Canonical INFO-*message* counts are typed in verification reports and retained compact validation evidence, but are not copied into the durable series rows. A message may span physical lines, so a literal INFO-line count is not currently a typed field. | The site builder should consume typed fields. Extend durable series retention for INFO-message counts and missing producer paths. If the requested statistic is literally physical lines, record that alongside the retained canonical log rather than silently relabelling message count. |

Other current facts materially affect the plan:

- `hermit/SCORECARD.md` currently uses green to mean selected by
  `ci/expected-e2e-plan.json` and red to mean enabled but not selected. That is
  intentionally different from the owner-specified website state. The current
  table is a visual starting point, not a state implementation to reuse.
- `ci-hub/series/series.py::measurement_state` reduces all-history evidence and
  answers whether any divergence was ever observed. It does not implement the
  required most-recent-run display state.
- `ci/compat-envelope/cells.json` is a checked-in projection. Its projection
  metadata currently describes only a small imported subset and does not have a
  verifiable source-tree identity. It is useful as a transition cross-check,
  not as the website's input.
- The series store deliberately preserves per-cell observation history,
  including retries and represented-run counts. The validation ledger records
  whole-run selection, provenance, completion, and receipt evidence. Their
  different cardinalities are intentional; join them only by explicit run and
  tree identity.
- Current series rows carry scheduler turns, virtual nanoseconds, and total
  syscalls, but omit the exact historical invocation and canonical INFO count.
  Current commands can be regenerated from the manifest; historical commands
  cannot be reconstructed exactly after the manifest changes.
- The manifest runner retains verify logs only when
  `E2E_KEEP_VERIFY_LOGS=1`. For ptrace it can create
  `normalized-ptrace-golden.log`, but the canonical ledger does not retain a
  durable content-addressed reference to that file.
- The live x86_64 syscall classification is exhaustive over 373 pinned syscall
  names and currently classifies 294 as `Determinized`, 78 as `PassThrough`,
  and one as `Unsupported`. These are execution-policy classes, not the owner's
  requested compatibility status: `Determinized` includes modeled behavior and
  fixed-error refusals, and it carries no completeness, caveat, issue, or
  per-backend information.

## The complete user experience

### Landing page

The landing page keeps the recognizable backend-by-mode scorecard shape, but
every count uses the owner-specified ledger-derived state:

- green: the most recent qualifying run passed after its retries were reduced;
- red: the most recent qualifying run selected and ran the cell, and every
  retry ended in a product failure;
- never measured: no qualifying row exists.

Each table shows both the full manifest denominator and the enabled subset so a
percentage cannot improve merely because an unsupported backend or mode was
removed. Every number is a link to the exact filtered cell listing behind it.
The page also shows the Hermit main commit, manifest digest, ledger source
commit/tree, generation time, canonical-row count, represented-run count, and
the number of incomplete records excluded from a state calculation.

The landing page includes direct links to:

- all cells;
- all test ids;
- all syscalls;
- validation runs;
- incomplete data.

### Reusable cell listing

One component is used for every scorecard drill-down and for the all-cells
view. Its filter state is encoded in the URL so a result can be shared. It
supports text search over test id and filters for lane, category, mode, backend,
state, enabled/disabled manifest policy, result kind, current-main ancestry,
host capability, and whether required evidence is complete.

The default columns are test id, mode, backend, state, enabled status, latest
result time/tree, canonical rows, represented runs, and incomplete fields. The
count at the top must equal the landing-page link that opened it. Compressed
rows use `num_runs` for the data-point total while also showing physical row
count; otherwise 21,251 measured runs would misleadingly appear as 14,902.

The all-cells view includes all 5,744 current manifest cells. Historical ledger
cells that no longer join to the current manifest remain reachable in a
separate filtered listing and do not silently enter the current denominator.

### Test-id page

Clicking a test id opens one page that combines current manifest policy with
ledger evidence without confusing them. It contains:

- description, category, lane, program/direct command, requirements,
  observation policy, timeout policy, guest arguments, and backend exclusion
  reasons from the manifest;
- the full mode/backend cell matrix for that test id, with each cell's derived
  state and data-point count;
- the current copy-pasteable reproducer generated by a side-effect-free command
  renderer factored from the harness's manifest recipe code, not a second YAML
  interpreter. Until that refactor lands, use the stable exact-cell test-harness
  command rather than calling filesystem-mutating `build_spec` from the site;
- a path-tokenized, allowlisted historical reproduction recipe only when the
  ledger recorded it, clearly distinguished from the current reproducer. Raw
  environment and workspace paths remain protected; the public row carries
  redaction/omission metadata and may carry a digest of the raw invocation;
- recent per-cell history grouped by run, with retries shown together;
- the separate 10-of-10 pressure result used to decide whether a formerly red
  cell is ready to be enabled;
- for each green canonical verification cell, the golden execution's compared
  INFO-message count, physical INFO-line count when recorded, syscall count,
  scheduler turns, elapsed virtual nanoseconds, and full canonical log when all
  of those artifacts exist;
- links to every syscall exercised by that canonical execution.

The page never substitutes run 2 for run 1 without saying so, never adds the two
verification executions together, and never presents a retry as an independent
current cell. The canonical golden statistics use run 1. Run 2 is displayed as
comparison evidence alongside it.

### Cell history and run pages

The rough sketch stops at a test-id page, but debugging needs two further
drill-downs.

A cell history page shows every qualifying observation for one exact
`test/mode/backend` identity, including run id, tree, current-main ancestry,
producer, host/kernel/capabilities, attempts, result/failure class, wall and CPU
time, runtime counts, divergence coordinates, and links to the enclosing run.
An e9patch summary describes its ptrace runtime, and a LiteInst summary
describes its ptrace-host hybrid; these counts are execution observations, not
proof that the selected backend path was fully engaged. The four divergence
coordinates remain separate axes; the UI never compares a
record number with a syscall count or scheduler turn.

A run page shows the validation selection and whole-run receipt: profile,
partial/full selection, source and tree identity, host, timings, gates,
selected cells, recorded cells, retries, and terminal outcome. This page makes
the membership rule inspectable: a cell absent from a partial run was not
measured by that run.

### Syscall index

The syscall index lists every syscall in the pinned x86_64 table exactly once.
It is grouped under reviewed logical headings and sorted alphabetically inside
each heading. Each row shows:

- syscall name and number;
- owner-specified status;
- current Detcore execution-policy class;
- total calls across the selected current suite population;
- distinct test ids and cells that exercise it;
- whether its evidence is complete;
- a link to its syscall page.

`never exercised` is not stored metadata. It is the derived condition that the
selected population has an empty test-id list and zero recorded calls.

### Syscall page

Each syscall page shows the machine-readable status and execution policy, its
logical group, total calls, the contributing cells, and distinct test ids. The
counts default to one canonical golden execution for each current green
canonical verification cell; they do not double-count verification run 2,
retries, or repeated pressure samples. Non-verification modes are shown
separately only after their producer records which execution is the primary one
for aggregation. Filters can narrow the totals by backend, and the page states
the exact population used.

The page links to, but never embeds, the syscall's GitHub tracking issue. The
metadata is authoritative for status. The issue is authoritative for reasoning
and discussion and must not be parsed as a second status source.

### Incomplete-data page

The site build emits a machine-generated page and JSON file listing every fact
it could not render, grouped by cause and linked back to affected cells,
tests, runs, or syscalls. This is a first-class output, not a warning lost in a
build log. Known historical absence may render the rest of the site; malformed
current-schema evidence, contradictory identities, or a new omission from a
producer that claims the field is required must fail the build.

## Exact data reduction

### Inputs and authority

| Input | Authority | Not authoritative for |
| --- | --- | --- |
| Current E2E manifests parsed by `hermit-manifest-plan` | Current test ids, full cell universe, mode/backend applicability, current command recipe and policy metadata | Whether anything ran or passed |
| Canonical union of tracked series shards and unpublished spool | Per-cell attempts, results, repeated-run history, runtime metrics, divergence positions | Whole-run completeness or cells omitted from a partial selection |
| Canonical validation-ledger union | Run selection, run completion, source identity, receipt and compact comparison evidence | Per-cell repeated history not present in the receipt |
| Reviewed syscall metadata in Hermit | Syscall status, execution policy, logical group, GitHub issue URL | Which tests actually exercised the syscall |
| Content-addressed canonical log objects referenced by a ledger row | The full log bytes for that observation | Cell state or syscall status |

The builder reads the existing canonical union functions instead of opening one
shard directly. Published shards and pending spool rows may contain the same
event id during publication; deduplication must produce one logical event.

The default website describes current Hermit main. Aggregate state uses clean,
schema-valid rows whose tree is currently an ancestor of that main. Other
canonical rows remain visible in history with their ancestry label, but a
feature-branch result cannot silently change the main scorecard.

### Cell state algorithm

For each current manifest cell:

1. Select rows accepted by `SeriesRow::validate_for_projection`, then recompute
   current-main ancestry. Never infer a row from a run whose selection omitted
   the cell. Read-valid historical rows that are not projection-grade remain
   visible as excluded evidence.
2. Build a producer-specific logical execution identity. For validate rows it
   is `(producer, run_id, cell)`, with `run_index` as the retry ordinal. For
   pressure-test and `hermit-repeat` rows it is
   `(producer, run_id, cell, run_index)`, with `attempt` as the retry ordinal.
   A pressure campaign's `run_id` covers many repetitions and must not collapse
   them into one result.
3. Treat `num_runs` and `last_run_index` as compressed equal repetitions, never
   as retries. Use `num_runs` for represented-data-point counts and retain the
   physical-row count separately. Do not reconstruct exact repetition indexes
   from the current compressed form: equal outcomes can be collapsed across
   intervening different outcomes, so the stored span does not prove which
   indexes were represented.
4. Reduce retries only inside one logical execution. Any passing retry makes
   that execution pass. An execution is a product failure only when it ran and
   every retry has a recorded product failure. Infrastructure, prerequisite,
   timeout, out-of-memory, skipped, and genuine no-result outcomes are not
   silently relabelled as product failures.
5. Order logical executions by a producer-bound immutable execution timestamp
   or sequence and choose the most recent qualifying one. Current `emitted_at`
   defaults to append/publication time and cannot prove execution order after a
   backfill, so add `observed_at` or an equivalent producer-owned field. Until
   that exists, label a result only as most recently recorded and list the
   owner-specified latest-run state as unavailable where ordering is ambiguous.
6. Map a passing latest execution to green and an all-retries product failure
   to red. If there are no qualifying rows, return never measured.
7. If the newest selected execution has no product verdict, the current three-state
   contract has no honest answer. Record that cell on the incomplete-data page
   and exclude it from green/red counts until the owner specifies whether a
   prior product result carries forward. Do not invent a fourth displayed state
   or coerce it to red.

Applicability remains a separate manifest-policy axis. A currently disabled
cell is excluded from the current enabled denominator and shows its current
exclusion reason, without erasing historical observations or relabelling them
as never measured. The builder evaluates each observation against the manifest
at that observation's tree when that tree is available. It reports a
contradiction only when the exact-tree manifest excluded a cell that the run
claims it selected or executed; a cell that was enabled historically and
disabled later is ordinary policy history.

The 10-of-10 decision is computed separately from a fresh uncompressed pressure
campaign artifact or a typed promotion result at one exact tree, not ten
physical JSONL rows. The existing compressed series history may be sufficient
to disprove the bar, but it cannot always prove ten consecutive passes because
exact repetition membership can be lost. Ten proven consecutive passes with no
failure or no-result make the page say the bar is met. The site does not edit `ci`,
`ci_disabled_reason`, or `ci/expected-e2e-plan.json`.

### Per-test statistics

For a canonical green verification cell, the four headline values come from
typed evidence:

- compared INFO records/messages: `compared_log_messages.left`, after verifying that the
  comparison scope is canonical INFO and both sides are nonzero;
- syscalls: `runtime.run1.syscalls`;
- scheduler turns: `runtime.run1.scheduler_turns`;
- virtual nanoseconds: `runtime.run1.virtual_nanoseconds`.

The full log is the normalized run-1 canonical log named by a digest-bearing
ledger reference. The producer records its physical INFO-line count separately
when that is the desired display statistic. The build verifies the digest,
decompresses the log for validation, reruns the same timestamp-delimited record
parser, record-envelope predicate, and ignore policy, confirms that count agrees
with the typed comparison report, and confirms the physical-line count against
the recorded artifact metadata. It then copies the gzip bytes into the output.
A mismatch is a producer defect and fails the build. Missing historical values
render as unavailable and appear in the incomplete-data output.

### Syscall totals

Detcore should record a sparse, deterministically ordered map keyed by
`(architecture, syscall number)`, carrying the pinned display name, in
`RunSummary`. Each bucket increments at the same prehook accounting point as
the existing scalar syscall counter. Verification carries both run maps through
`canonical_verdict::RuntimeStats` and
`verification_runtime_from_summaries`; the manifest `CellResult` retains them,
and the series projection stores them. The builder validates that the sum of
run-1 histogram buckets equals `runtime.run1.syscalls` before admitting the
row. If the desired metric changes to completed calls, version and change the
scalar and histogram accounting points together.

The default syscall total sums run-1 histograms from current green canonical
verification cells. The test-id list is the distinct set of those cells' test
ids with a nonzero bucket. Default ptrace-style execution observes the full
pinned subscription. Calls not observed by Detcore under explicit
`--passthru-opt` or a backend path that executes outside Detcore cannot be
represented as zero; the run must carry its observation scope, and incomplete
scope is displayed.

## Syscall metadata

The current `Determinized | PassThrough | Unsupported` classification remains
the execution policy used by the code. Add separate reviewed metadata that
maps each pinned syscall to:

- one of the owner-specified statuses: `supported-and-determinized`,
  `passed-through-because-believed-deterministic`, or `partially-supported`;
- one existing logical group heading;
- one full `https://github.com/rrnewton/hermit/issues/...` tracking URL;
- optional machine-readable limitations such as unsupported flags, modes, or
  backends, using existing project vocabulary.

This metadata belongs beside the live syscall classification, is exported by a
small typed command, and is checked for exact coverage of the pinned table.
Adding a syscall to the pinned table without adding metadata fails the check.
The existing prose groupings are inputs to the first review, but the Markdown
files do not become parsers or authorities.

The status and the issue have deliberately different jobs. A syscall change
updates the metadata in the same work that changes its implementation. The
tracking issue records evidence and reasoning and links the implementing pull
request, but the website never treats issue prose or labels as status.

## Generated artifact and server

The generated directory has stable, inspectable paths:

```text
index.html
cells/index.html
cells/<encoded-cell-id>.html
tests/<encoded-test-id>.html
runs/<encoded-run-id>.html
syscalls/index.html
syscalls/<syscall-name>.html
incomplete-data/index.html
data/cells.json.gz
data/tests.json.gz
data/runs.json.gz
data/syscalls.json.gz
logs/<sha256>.log.gz
assets/site.css
assets/site.js
build.json
```

HTML pages contain useful headings, summaries, and links without JavaScript.
JavaScript adds fast filtering and sorting over the generated indexes. Search
state stays in the URL. Logs are fetched only when opened, never embedded in
the initial page. Colour is not the only state indicator, tables remain usable
from a keyboard, and large counts include unambiguous labels and denominators.

`build.json` records input commits, tree/directory digests, row counts, schema
versions, generator version, and every accepted incomplete-data category. It is
provenance for the derived output, not a new state store.

Add a thin `ci-hub` passthrough subcommand with separate build and serve
operations. Keep the site builder and Rust server co-located under `ci-hub/`,
rather than adding their dependencies to the monolithic `ci-hub.rs` front door.
The builder consumes canonical JSON exports from the existing Python ledger and
series authorities plus typed Hermit exports. The normal top-level user action
builds only when `build.json` no longer matches the input digests, then serves
the directory. The server:

- binds to `127.0.0.1` by default and supports port zero for tests;
- serves only the selected output root and disables directory listing;
- rejects path traversal and symlink escapes;
- sends explicit content types and `Content-Encoding: gzip` for stored text;
- adds a restrictive content-security policy;
- performs no network fetch and writes no status or judgement.

Stable automation belongs under `ci-hub/`, exposed through
`./ci-hub/ci-hub`; it should not become another unrelated script in
`scripts/`. A Make target may express the build dependency, but the builder
itself verifies input digests because ledger shards and their unpublished spool
are dynamic inputs.

The build is reproducible: sort every collection, canonicalize JSON, give gzip
a fixed timestamp, avoid absolute workspace paths, and use the source epoch for
generated timestamps. Building twice from identical inputs must produce the
same file list and SHA-256 digest for every file.

## Durable log storage

The requested log page cannot be implemented honestly from current retained
data. The implementation must first run a full validation with verify-log
retention enabled and measure:

- raw and gzipped bytes by cell and in total;
- largest and median log;
- duplicate content by digest;
- build and browser latency;
- paths or environment data that must be normalized before publication.

The existing 2.777--3.285 MiB figure excludes verify logs and must not be used
to choose a storage backend. After the measurement, retain canonical run-1 logs
as immutable gzip objects named by SHA-256. The series/ledger row records the
digest, byte sizes, media type, and storage identity. The output builder copies
only objects referenced by the selected site population. Content-addressing
deduplicates identical logs and lets the builder prove that the served bytes
are the recorded bytes.

Do not place a temporary `ignored/validate/...` path in a published row and call
that durable. The artifact policy must prove that a clean checkout can rebuild
the site after the original validation checkout has been removed. The measured
full-run size decides whether the durable object store can live in Git or needs
the repository's artifact storage; that decision is a gate at the end of the
measurement stage, not a guess in advance.

## What cannot be shown honestly today

This list is itself a deliverable of the investigation.

1. **Owner-specified green/red state.** Existing scorecard colour is selection
   policy, and existing `measurement_state` is an all-history reduction. Neither
   is the required latest-run state.
2. **A complete current series projection from `cells.json`.** Its recorded
   projection is stale and incomplete relative to the canonical series.
   Historical schema versions remain readable but not all are projection-grade;
   the site must count and explain every excluded row by validation reason.
3. **Proven latest-run order for backfilled history.** Current `emitted_at` may
   be append time rather than execution time, so an old run published later can
   look newest. The series schema needs producer-owned execution time or order.
4. **A state for a newest run with no product verdict.** The owner specified
   three states, while current producers preserve infrastructure,
   prerequisite, timeout, out-of-memory, skipped, and no-result outcomes.
5. **Canonical INFO counts for every historical series row.** INFO-message
   counts exist in verification reports and compact validation evidence but are
   not retained in the durable series schema. A literal physical-line count is
   not a typed field at all.
6. **Runtime totals for every row and backend/mode.** Hundreds of historical run
   sides lack them; DBT ignores the normal summary path, record/replay does not
   currently attach runtime summaries, `--namespace-only` ignores
   `--summary-json`, and non-verifying custom/naked manifest modes do not request
   it.
7. **Safe historical reproducers.** Raw manifest results contain argv, guest
   argv, environment, cwd, and shell command, but the durable series projection
   omits them. Publishing those raw fields would also expose workspace paths or
   credentials. Only the current high-level exact-cell harness command can be
   generated safely without first defining an allowlisted, path-tokenized
   reproduction recipe.
8. **Full canonical logs.** Retention is optional, current measured runs kept no
   verify logs, and no durable digest-bearing log reference exists.
9. **Per-syscall call counts or test coverage.** Detcore records only a total;
   structured DETLOG records omit syscall identity.
10. **An exhaustive logical syscall grouping.** Only dated prose groupings
   exist.
11. **The requested syscall support status.** Current execution-policy classes
    conflate implemented models, partial behavior, fixed-error refusals, and
    passthrough.
12. **Per-syscall GitHub links.** No structured mapping exists.
13. **Complete observation scope.** Explicit `--passthru-opt` and execution paths
    outside Detcore may let calls bypass the accounting point, so a histogram
    without an explicit scope could look complete while omitting calls.
14. **Reliable historical Detcore-tree identity.** Most retained rows predate
    the explicit field; reconstructed identity is contingent and must display as
    unknown when it cannot be proved.
15. **A clean join for every row.** The ledger has historical cell identities
    outside the current manifest, and at least one current categorical exclusion
    has recorded divergence evidence. Both require visible handling.
16. **Cross-backend parity.** Current verify cells compare a backend with itself;
    the website must not manufacture a cross-backend number.

## Staged implementation

### Stage 1: pure readers, reducer, and incomplete-data output

- Add a co-located `ci-hub` component that reuses the existing canonical ledger
  and series readers through canonical JSON export commands, plus a typed
  manifest metadata export from Hermit. Do not port their Python union and
  validation rules into a second implementation.
- Factor a side-effect-free command-plan renderer from the manifest runner. The
  site must not call `build_spec`, which creates artifact directories and
  therefore is not a pure read of manifest policy.
- Implement the owner-specified state reduction and retry grouping as a pure
  function over fixture rows.
- Join current manifest cells to canonical history and emit deterministic JSON
  for cells, tests, runs, and incomplete data.
- Count rows excluded from the current projection by schema version and exact
  validation reason; preserve links to their readable historical evidence.
- Cross-check the generated state against current derived artifacts without
  using those artifacts as inputs. Report every semantic mismatch.

Exit criterion: every cell in the current manifest is accounted for exactly
once (5,744 at the measured snapshot); deleting the unique deciding logical
execution changes the derived state or makes it incomplete; deleting any row
changes auditable history or represented-run counts; partial-run omission
changes nothing; and known unavailable fields appear in generated
incomplete-data JSON.

### Stage 2: first navigable website from existing evidence

- Generate the landing page, reusable cell listing, test-id pages, cell history
  pages, and run pages.
- Make every aggregate number link to a filtered listing whose row count matches
  it.
- Show existing runtime fields and current reproducers; show unavailable for
  missing INFO counts, historical invocations, logs, and syscall histograms.
- Add URL-preserving search/filter tests and static link validation.

Exit criterion: the site is already useful for compatibility navigation and
produces a complete, linked inventory of recording gaps without parsing logs.

### Stage 3: retain the missing producer-owned facts

- Extend the durable series row with canonical INFO counts and a safe,
  allowlisted reproduction recipe. Keep raw invocation fields protected or
  retain only their digest; do not publish arbitrary environment values.
- Preserve exact pressure repetition membership as contiguous ranges, an
  uncompressed campaign artifact, or a typed promotion result. Do not infer a
  10-of-10 sequence from today's lossy compressed span.
- Introduce a versioned, invocation-bound run-summary envelope, clear or
  pre-stamp its destination before launch, and publish it atomically so stale
  files cannot be accepted.
- Make every website-supported backend/mode write typed run summaries,
  including DBT and record/replay; either add an explicit primary-execution
  contract for namespace-only/custom/naked modes or preserve their absence and
  keep them out of aggregate runtime claims.
- Add a sparse per-syscall histogram and observation-scope field through
  `RunSummary`, `canonical_verdict::RuntimeStats`,
  `verification_runtime_from_summaries`, manifest `CellResult`, and the durable
  series projection.
- Add cross-field guards: histogram sum equals total syscalls, retry and run
  identities agree across the validation and series ledgers, and INFO count is
  checked against a retained log whenever that row already has a log reference.
- Keep old schema rows readable and visibly incomplete; do not reconstruct new
  authority from old text.

Exit criterion: one new full validation produces all four requested statistics
and per-syscall counts as typed durable fields for every selected canonical
verification cell whose observation scope is complete. Every other selected
mode/backend is enumerated explicitly as available or unavailable.

### Stage 4: syscall metadata and pages

- Add exhaustive typed status, logical group, limitations, and full GitHub issue
  URL metadata beside the live syscall classification.
- Generate or validate one tracking issue link per pinned syscall without
  ingesting issue contents.
- Add exact-set tests so a new, removed, or renamed syscall cannot leave the
  metadata or grouping silently stale.
- Generate the syscall index and per-syscall pages from the new metadata plus
  run-1 histograms.

Exit criterion: all 373 pinned x86_64 syscalls appear once, every group is
nonempty and alphabetized, histogram totals reconcile, and `never exercised`
is derived from an empty usage set.

### Stage 5: canonical log retention and storage decision

- Run and retain one full validation with verify logs enabled.
- Either generalize `normalized-ptrace-golden.log` into a backend-independent
  canonical run-1 log producer or scope the first full-log release explicitly
  to ptrace; do not imply that current normalization covers every backend.
- Publish the raw/gzip size distribution and choose the durable content-addressed
  storage path from that evidence.
- Normalize publication-sensitive paths at the producer boundary, gzip with
  deterministic metadata, record digests in ledger rows, and reject missing or
  changed objects.
- Require INFO-record, physical-line, and digest reconciliation for every
  published log.
- Add lazy full-log viewing and links from green test/cell pages.

Exit criterion: a clean checkout can regenerate the site and serve every
referenced canonical log after the producing validation checkout is deleted.

### Stage 6: top-level server, freshness, and shipping

- Add build and serve subcommands to `./ci-hub/ci-hub` and a documented Make
  target.
- Rebuild on input-digest change rather than maintaining another mutable current
  state file.
- Add deterministic-build, path-safety, gzip, MIME, no-directory-listing,
  no-external-resource, and large-log tests.
- Package the output directory and build manifest as the shippable artifact.

Exit criterion: one top-level command builds or reuses a fresh artifact, serves
it on loopback, and prints the local URL and exact input identities.

### Stage 7: iterate on the complete data

Once the site can show real logs and syscall usage, review the information
architecture with the owner. Add views only when they are projections of
existing typed data. Candidate follow-ups include change-over-time comparisons,
first-bad links, and cross-backend parity after those cells actually exist; none
should delay the core site or be inferred from current same-backend results.

## Verification contract

The implementation is complete only when automated tests establish all of the
following:

- Deleting the unique deciding/latest logical execution changes the displayed
  state or makes it incomplete, proving no stored state field is authoritative.
  Deleting any row changes auditable history or represented-run counts, while a
  redundant non-deciding row may correctly leave current state unchanged.
- An omitted cell in a partial run leaves its prior history untouched.
- Failed-first/pass-on-retry is green; all product-failing retries are red.
- No qualifying rows is never measured. Missing or malformed current-schema
  evidence is not.
- Every landing-page number links to exactly that many cell rows under the same
  denominator and filters.
- Physical row counts and represented-run counts are separately correct.
- The 10-of-10 bar is never asserted from compressed history that cannot prove
  exact consecutive repetition membership.
- Current applicability and historical observed state are displayed separately;
  a contradiction is reported only against the manifest at the observation's
  own tree.
- Current reproducers come from the shared manifest command construction.
  Historical reproducers appear only when recorded.
- INFO-message, syscall, scheduler-turn, and virtual-time values come from typed
  fields, with absent values shown as unavailable rather than zero. A displayed
  physical INFO-line count comes from separately recorded artifact metadata.
- Every admitted syscall histogram sums to the corresponding total syscall
  count.
- Syscall usage totals count one canonical golden execution per selected cell
  and do not include verification run 2, retries, or pressure repetitions.
- The pinned syscall table, status metadata, grouping, and issue mapping have
  exactly equal name sets.
- The website never reads GitHub issue bodies, labels, or comments.
- A retained log's digest and INFO count are verified before it is published.
- Two builds from byte-identical inputs produce byte-identical output trees.
- The server cannot escape the output root, list directories, fetch external
  resources, or expose an uncompressed log path.
- The generated artifact contains no absolute owner workspace path or
  credential-bearing environment value.
- The site labels all current verification evidence as same-backend until a
  real cross-backend comparison row exists.

## Explicit non-goals

- Do not make the website an editor for manifests, syscall metadata, or issues.
- Do not hand-maintain aggregate HTML, JSON, or a measured flag.
- Do not use the legacy parent compatibility CSVs as current state.
- Do not parse human log text for cell verdicts, runtime metrics, or syscall
  status.
- Do not hide missing evidence to make the first release look complete.
- Do not retain human or agent judgement in the generated directory.

The result is not merely a prettier scorecard. It is a navigable, reproducible
projection from recorded evidence: every headline number drills down to its
rows, every test can be rerun, every syscall can be traced to exercising tests,
and every blank identifies a concrete producer or schema obligation.
