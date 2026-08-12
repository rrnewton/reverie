# Append-only per-team/machine validation-ledger event schema

Task `define-team-machine-ledger-schema` (hermit-w10), 2026-08-07.
**Schema only. No producer, writer, or consumer code is changed by this document.**

Every design choice below is justified against the *measured* current dataset, not against
an imagined one. The measurements are from `measure-current-validate-ledger-dataset` plus
the field-level inventory taken for this task; both are reproduced inline where they bind a
decision.

---

## 0. What the existing data actually is (the constraints, measured)

| fact | value | why it constrains the schema |
|---|---|---|
| canonical ledger | `ignored/validate-run-ledger.jsonl`, 654 rows | the thing that must map without loss |
| second ledger | `ignored/validate-run-global.jsonl`, 326 rows, **disjoint schema** | the union is not one file today |
| distinct keys | 51 (canonical) + 34 (global), union ~57 | only 20 are present on 100% of canonical rows |
| machines | 1 (`devbig014`), short form, 0 FQDNs | sharding is unexercised; design must not assume it works |
| **no natural unique key** | `host+started_at+finished_at+commit` → **608 distinct / 654 rows** | a `run_id` must be **minted**, not derived |
| **the 46 "duplicates" are enrichments** | 608 groups = 571 singletons + 36 pairs + **one 11-row chain**; **0 byte-identical**; differing fields are exactly `schema_version`, `executed_tests`, `filtered_tests`, `coverage` | the event model is **required by the data**, not a nicety |
| `cwd` | starts `/home/<owner>/` on **654/654** rows | tracked shards would publish owner paths — the class `scripts/check-portable-paths.sh` rejects |
| outcome vocabularies differ | canonical `pass/fail/killed/no_result`; global `pass/fail/timeout/incomplete/killed` | the union needs one normalized enum |
| provenance | global `source`: `reconstructed` 145 / `ledger` 181 | 145 rows were **never observed directly**; that must be visible |
| growth | 41.7–70.8 MiB/yr raw per machine, ~2.3–4.0 MiB zstd-19 | monthly shards are ~3–6 MiB: small enough to commit, large enough not to churn |

**The single most important finding:** the 37 colliding keys are the *same run* emitted more
than once — 36 twice, one eleven times — each later emission carrying fields the earlier
lacked. The current ledger already performs
enrichment — it just expresses it as an unlinked duplicate row, which is why the row count
(654) and the run count (608) disagree and why no consumer can dedupe correctly. Modelling
enrichment as a first-class event with a reference is what makes the legacy data
representable *without loss*.

---

## 1. Shard path convention

```
ledger/<team>/<short-host>/<YYYY>-<MM>.jsonl
```

Example: `ledger/hermit/devbig014/2026-08.jsonl`

- **`<team>`** — stable lowercase-hyphenated slug (`hermit`, `reverie-infra`). Names the owning
  team, never a person.
- **`<short-host>`** — hostname up to the first dot, e.g. `devbig014`. **A dot in this path
  component is a hard error** (§7).
- **`<YYYY>-<MM>`** — UTC month of the event's `emitted_at`.

**Why month-sharded, not one file per machine.** Only the current month's file is ever
appended to; every prior file is frozen. Two machines never write the same path, and the same
machine never rewrites a closed month, so **the git merge-conflict surface is one file per
active machine** rather than one shared file. At the measured 41.7–70.8 MiB/yr per machine, a
month is ~3–6 MiB raw — comfortably committable, and ~0.2–0.35 MiB at zstd-19.

**Why not shard by commit or by day.** Per-commit is unbounded in count (279 commits in 4
days) and defeats packing; per-day multiplies file count by 30 for no conflict benefit, since
same-day writes already only collide on one machine's own file.

---

## 2. Event envelope (every line, every type)

Every line is one JSON object with this envelope. **Events are immutable: a written line is
never edited or deleted.**

```jsonc
{
  "schema": "validate-ledger/v1",   // envelope version; see §8
  "event_id": "01J...",             // ULID, unique across the union. MINTED by the producer.
  "event_type": "run.result",       // §3
  "emitted_at": "2026-08-07T02:32:02Z",  // RFC3339 UTC, when the EVENT was written
  "team": "hermit",
  "host": "devbig014",              // SHORT name. Must equal the shard path component.
  "run_id": "01J...",               // the run this event is about; == event_id on run.start
  "producer": {                     // provenance -- see §6
    "source": "observed",           // observed | reconstructed | imported
    "tool": "ci-hub/validate-run",
    "tool_version": "schema_version=5"
  }
}
```

- **`event_id` is minted (ULID), not derived from content.** This is forced by measurement:
  no combination of existing fields is unique (608/654), so a content-derived id would
  collide on exactly the 37 enrichment groups the schema most needs to distinguish.
- **`run_id` is the join key.** All events about one validation run share it. It is minted
  once, by the first event of the run.
- **ULID, not UUIDv4**, because ULIDs sort lexicographically by mint time, which gives §5 a
  total order with no extra field.

---

## 3. Event types

| type | meaning | required refs |
|---|---|---|
| `run.start` | a validation run began; mints `run_id` | — |
| `run.result` | terminal outcome of a run | `run_id` |
| `run.enrich` | **adds** fields not known at result time | `run_id`, `enriches: <event_id>` |
| `run.correct` | **supersedes** a previously-emitted field value | `run_id`, `supersedes: <event_id>`, `reason` |
| `run.annotate` | human/agent note; never affects derived state | `run_id` |

`run.start` is **optional** — the legacy data has no start event and must not be forced to
invent one. A run is well-formed with `run.result` alone.

**Corrections never mutate.** A wrong value is fixed by appending a `run.correct` that names
the `event_id` it supersedes and carries a `reason`. The original stays on disk forever. This
is what makes the shards safe to commit: a rewrite of history is detectable as a git diff to a
frozen file, and is by definition invalid.

**`run.enrich` vs `run.correct` is not stylistic.** Enrich may only *add* keys that were absent
or `null`; correct may *change* a key that had a value. A producer that changes a value via
`enrich` is emitting an invalid event (§7 lint). All 37 measured legacy multi-row groups are enrichments under this rule — verified
exhaustively: **0 of them change an already-set value**, so none needs `run.correct`. One of
them is an **11-row chain**, so an enrichment chain is not assumed to be depth 1.

---

## 4. `run.result` payload

Core fields, all required (these are exactly the 20 present on 100% of canonical rows, minus
the ones the envelope now owns):

```jsonc
{
  "outcome": "pass",            // §4.1 -- normalized enum
  "raw_outcome": "fail",        // producer's pre-reclassification verdict, when it differed
  "exit_code": 1,
  "started_at": "2026-08-07T02:32:02Z",
  "finished_at": "2026-08-07T02:35:46Z",
  "real_seconds": 224, "user_seconds": 51.4, "sys_seconds": 12.9,
  "commit": "8f4a5cfb...",      // 40-hex, the SUT commit
  "tree": "81988f9a...",        // tree oid; distinguishes dirty rebuilds of one commit
  "profile": "full",
  "checks": 7, "failures": 1,
  "gates": [...],
  "git_depth": 1568, "git_ahead": 10, "git_behind": 0,

  "workspace": {                // §4.2 -- REPLACES raw `cwd`
    "slot": "standalone",
    "repo_relative": "scratch/w4-1719-reconcile",
    "kind": "scratch"           // primary | worktree | scratch | standalone
  },
  "log_ref": "hermit-validate.Thy8Un.log",   // basename only; never an absolute path

  "host_class": {               // §6
    "cores": 316, "arch": "x86_64", "kernel": "6.18.39-...",
    "privileged": true, "kvm": true, "pmu": true
  }
}
```

Optional fields (present on a subset today, all preserved verbatim under `detail`):
`cache_state`, `selection_mode`, `commit_anchored`, `tree_dirty`, `executed_tests`,
`filtered_tests`, `gates_run`, `gates_expected`, `dag_jobs`, `concurrency_proof`,
`concurrent_validates`, `known_flaky_failure`, `flaky_failed_substeps`,
`solo_rerun_confirmation`, `solo_rerun_of`, `toolchain`, `interruption_signal`,
`zero_byte_purged`, `failed_substep_classes`, `first_error_line`, `reverie_pin_current`,
`coverage`, `full_coverage`, `repo`, `reclassified_reason`, `incomplete_gates`,
`killed_by_bound`, `killed_by_signal`, `product_failures`, `banner_lines`,
`gate_classification`, `skipped_gates`, `profiling_linked`.

### 4.1 Normalized outcome enum

One enum spanning **both** legacy vocabularies, so the union is answerable:

| `outcome` | meaning | legacy sources |
|---|---|---|
| `pass` | ran to completion, zero failures | canonical `pass` (357), global `pass` (160) |
| `fail` | ran to completion, ≥1 genuine failure | canonical `fail` (286), global `fail` (138) |
| `timeout` | exceeded a wall/CPU bound | global `timeout` (11) |
| `killed` | terminated by signal | canonical `killed` (1), global `killed` (1) |
| `incomplete` | started, did not reach a verdict | global `incomplete` (16) |
| `no_result` | **no verdict was produced** — not a failure | canonical `no_result` (10) |

**`no_result` is not `fail`.** A cancelled run, a lost runner, an OOM kill, a network error
reaching GitHub — these are the *absence* of an answer. Collapsing them into `fail` is the
misread that once put an automated revert of a healthy main one step away. `raw_outcome`
preserves the producer's original verdict whenever reclassification changed it (99 legacy rows
carry `raw_result`; measured split `fail` 76 / `pass` 23), and `interruption_signal` (`TERM` on
8 rows) is retained.

### 4.2 `workspace` replaces `cwd` — and this is mandatory, not cosmetic

**Measured: `cwd` begins `/home/<owner>/` on 654 of 654 rows.** Shards are *tracked* in
`rrnewton/dev-hermit`, and `scripts/check-portable-paths.sh` rejects owner-specific literal
paths in tracked files. Committing raw `cwd` would (a) publish an owner's home layout into
git history forever and (b) be the same defect class that produced a 15-run red streak and
that `portable_path_fix_ci` was filed to fix in a shell script. The path is therefore split
into a repo-relative remainder plus a typed `kind`, which is strictly more useful for querying
anyway ("all scratch runs", "all worktree runs"). Same rule for `log_file` → `log_ref`
basename.

---

## 5. Deterministic union, dedup, and order

Given any set of shard files:

1. **Union** = concatenation of all lines from all shards. No file is privileged.
2. **Dedup** on `event_id`. Identical `event_id` with differing bodies is a **hard error**, not
   a last-writer-wins — it means two producers minted the same ULID, and silently picking one
   would hide it.
3. **Order** by `(emitted_at, event_id)`. `event_id` is a ULID so it breaks ties
   deterministically and monotonically; the pair is a total order independent of file order,
   read order, or filesystem enumeration.
4. **Fold to run state**: group by `run_id`, apply events in the order from (3):
   - `run.result` establishes the base record.
   - `run.enrich` adds only keys currently absent/`null`; an enrich touching a set key is
     **dropped and reported**, never applied.
   - `run.correct` overwrites the named keys and records `superseded_by` on the prior event.
   - `run.annotate` never changes derived state.
5. The fold is **pure and order-independent given (3)** — two machines folding the same shard
   set produce byte-identical output. That is the property that lets green/bisect answers be
   compared across hosts.

---

## 6. Provenance and host class

`producer.source` is required and typed:

- `observed` — the producer watched the run.
- `reconstructed` — derived after the fact from logs/artifacts. **145 of 326 rows in the
  existing global ledger are this.** A consumer computing "green time" must be able to exclude
  reconstructed rows, and today it cannot.
- `imported` — carried in from a legacy file by the migration in §9.

`host_class` carries the capability facts that make two hosts non-comparable — cores, arch,
kernel, and the three that actually gate what can run here (`privileged`, `kvm`, `pmu`). A
green on a host without `/dev/kvm` does not license a KVM claim, and the schema must let a
query say so rather than leaving it to prose.

---

## 7. Lints (invalid states excluded by construction)

A shard line is **rejected** if any hold:

1. `host` contains `.` — FQDN or domain suffix. Short names only, in both the field and the path.
2. `host` ≠ the shard path's host component, or `team` ≠ its team component.
3. Any string field matches `/home/[^/]+/` or an internal domain suffix.
4. `event_id` is absent, malformed, or duplicated within the shard.
5. `event_type` ∈ {`run.enrich`,`run.correct`} without a resolvable `enriches`/`supersedes`.
6. `run.enrich` changes a key that already has a non-null value (that is a correction).
7. `commit` is not 40-hex; `outcome` is outside the §4.1 enum.
8. A line is appended to a shard whose month ≠ `emitted_at`'s month (frozen-file violation).
9. **Any modification to a line that already exists in `HEAD`** — detected as a git diff to a
   non-appending region. Append-only is enforced against the *committed* file, not on trust.

---

## 8. Schema evolution

- `schema` in the envelope is `validate-ledger/vN`; **N changes only for envelope-breaking
  changes**, which are expected to be rare.
- **Adding an optional payload field does not bump N.** The measured history shows five
  payload generations (`schema_version` 1→5) with mean row size growing 781→1628 bytes; a
  scheme that bumped the envelope for each would have five incompatible readers by now.
- Readers **must ignore unknown keys** and must not fail on absent optional keys.
- The producer's own generation stays visible as `producer.tool_version`, so a query can
  still say "only rows emitted by generation ≥5 carry `coverage`" without the envelope
  pretending those are different schemas.

---

## 9. Legacy mapping — 654 + 326 rows, no loss

**Canonical `validate-run-ledger.jsonl` (654 rows → 608 runs):**

1. Group rows by `(host, started_at, finished_at, commit)` → 608 groups.
2. **Singleton group (571)** → one `run.result`. `run_id` = `event_id` = ULID minted from
   `started_at` for stable replay.
3. **Multi-row group (37 groups, 83 rows: 36 pairs + one 11-row chain)** → order by
   `schema_version` ascending; the lowest becomes `run.result`, each subsequent row becomes a
   `run.enrich` carrying only the keys it adds (`executed_tests`, `filtered_tests`,
   `coverage`), with `enriches` pointing at the immediately preceding event so the chain is
   explicit. Verified exhaustively against the data: **no row in any group changes a value the
   earlier row had set**, so every one is a legal enrichment and `run.correct` is unused by the
   migration.
4. `cwd` → `workspace` (§4.2); `log_file` → `log_ref` basename; `result`/`raw_result` →
   `outcome`/`raw_outcome`; all remaining keys pass through under `detail` unchanged.
5. `producer.source` = `imported`; `producer.tool_version` = the row's `schema_version`.

**Global `validate-run-global.jsonl` (326 rows):** same procedure; `source` maps to
`producer.source` (`ledger`→`imported`, `reconstructed`→`reconstructed`), and its unique keys
(`banner_lines`, `gate_classification`, `skipped_gates`, `profiling_linked`) pass through under
`detail`.

**Loss check, with the arithmetic closed.** Group sizes are exactly `571×1 + 36×2 + 1×11 =
654` rows over `571 + 36 + 1 = 608` runs. Every input key lands in exactly one of: envelope,
typed payload, or `detail`; no key is dropped and no row is discarded, because each row in a
multi-row group becomes its own event. The migration is therefore **reversible** — replaying
events per `run_id` in `schema_version` order reconstructs the original 654 rows. Canonical
contributes 654 events over 608 runs; global contributes 326 events; **the union is 980 events
over 934 runs — not "980 rows", and not 934 events**.

---

## 10. Queries the schema must answer

**Green** — newest commit whose latest run passed, on a comparable host:
```
fold events → runs
  where outcome == 'pass'
    and producer.source == 'observed'      -- exclude reconstructed
    and host_class matches the claim        -- e.g. kvm == true for a KVM claim
  group by commit, take the LATEST run per commit (not any run)
  → newest by git ancestry
```
"Latest run per commit" matters: measured 2.33 runs per commit, so *some* run passing is not
the same as the commit being green.

**Timeline** — every event for a commit or host in order: filter, then order by
`(emitted_at, event_id)` (§5). Because corrections are events rather than edits, the timeline
shows *what was believed when*, which a mutable row cannot.

**Bisect** — for an ordered commit list, the per-commit verdict is the fold's latest run
outcome, with `no_result`/`incomplete`/`timeout` returned as **"unknown, re-run"** rather than
as `fail`. A bisect that treats a missing answer as a failure walks to the wrong commit.

---

## 11. Worked examples

**Two teams, two machines** — four shards, no shared file, no write contention:
```
ledger/hermit/devbig014/2026-08.jsonl
ledger/hermit/devbig030/2026-08.jsonl
ledger/reverie-infra/devbig014/2026-08.jsonl
ledger/reverie-infra/devbig077/2026-08.jsonl
```

**A run, then an enrichment** (the shape all 37 legacy multi-row groups have):
```jsonc
{"schema":"validate-ledger/v1","event_id":"01JA…R1","event_type":"run.result","run_id":"01JA…R1",
 "emitted_at":"2026-08-03T18:38:32Z","team":"hermit","host":"devbig014",
 "producer":{"source":"imported","tool":"ci-hub/validate-run","tool_version":"schema_version=3"},
 "outcome":"pass","commit":"469a0f92…","started_at":"2026-08-03T18:30:22Z","checks":7,"failures":0}

{"schema":"validate-ledger/v1","event_id":"01JA…E9","event_type":"run.enrich","run_id":"01JA…R1",
 "enriches":"01JA…R1","emitted_at":"2026-08-03T19:02:10Z","team":"hermit","host":"devbig014",
 "producer":{"source":"imported","tool":"ci-hub/validate-run","tool_version":"schema_version=5"},
 "executed_tests":740,"filtered_tests":350,
 "coverage":{"planned_test_nodes":19,"executed_test_nodes":19}}
```

**A correction** (outcome was reclassified; the original is not touched):
```jsonc
{"schema":"validate-ledger/v1","event_id":"01JA…C2","event_type":"run.correct","run_id":"01JA…R1",
 "supersedes":"01JA…R1","emitted_at":"2026-08-03T20:11:00Z","team":"hermit","host":"devbig014",
 "producer":{"source":"observed","tool":"ci-hub/reclassify","tool_version":"1"},
 "outcome":"no_result","reason":"runner lost; banner verdict was not a product answer"}
```

**Failed / interrupted / no-result** are three distinct records, not one:
```jsonc
{"…":"…","event_type":"run.result","outcome":"fail","exit_code":1,"failures":1,
 "first_error_line":"FAIL portable chaos ptrace determinism-stress/order-violation …"}
{"…":"…","event_type":"run.result","outcome":"killed","interruption_signal":"TERM","exit_code":143}
{"…":"…","event_type":"run.result","outcome":"no_result","raw_outcome":"fail",
 "reclassified_reason":"cancelled hosted run"}
```

**A duplicate** — same `event_id` twice with differing bodies is a hard error (§5.2), not a
silent pick. Same *run* twice is representable only as result + enrich/correct, which is why
the 37 legacy multi-row groups stop being ambiguous.

---

## 12. Explicitly out of scope

No producer, writer, publisher, or consumer code is modified by this document. In particular
`ci-hub/validate/aggregate.py`, `ci-hub/lib/validate_status.rs` (`LEDGER_REL`) and
`ci-hub/health/*` are untouched. The migration in §9 is *specified*, not implemented.

**One caveat that bounds every sharding claim here:** the measured dataset has exactly **one**
machine and **one** team. The shard/union/conflict design is therefore reasoned from the
mechanism, not validated against real multi-machine data, because none exists on this box. The
first real second machine is the test that matters, and it should be treated as such rather
than as a rollout.

---

# What a reader may rely on (measured 2026-08-12, n=1491)

## The version string is not a contract, and must not be read as one

Every record ever written carries the same envelope value:

```
"schema": "validate-ledger/v1"      1491 of 1491 records
```

**Under that one string there are FIVE distinct envelope key sets.** Eleven envelope keys
appear in every record; four (`enriches`, `legacy_index`, `started_at`, `workspace`) appear in
some only. So `validate-ledger/v1` constrains nothing about the record's shape. A consumer that
checks the version and then assumes a field is reasoning from a string that permits five layouts.

The payload version is real but weak. `legacy_row.schema_version` genuinely spans 1..5
(94 / 22 / 549 / 262 / 534 in the largest shard), yet **`schema_version == 5` guarantees only
seven payload fields across 550 records** — barely more than the all-history six. Neither version
is a useful predictor of shape.

**What predicts shape is ROLE AND RECENCY, not version.** See the table.

## GUARANTEE YOUR OWN DENOMINATOR

There is no single answer to "which fields are always present". The honest answer is per scope,
and the spread is large enough that a global answer would mislead every reader:

| scope | n | guaranteed `legacy_row` fields |
|---|---|---|
| ALL HISTORY | 1491 | **6** |
| `schema_version == 5` | 550 | **7** |
| CURRENT validate receipts (`finished_at >= 2026-08-10` AND carries `gates[]`) | 182 | **27** |

**Guaranteed across ALL HISTORY (6):**
`commit`, `finished_at`, `host`, `result`, `schema_version`, `started_at`

**`schema_version == 5` adds exactly one:** `cwd`

**CURRENT validate receipts add 21 more**, and these are the ones that decide validation
authority: `cache_state`, `checks`, `commit_anchored`, `cwd`, `executed_tests`, `exit_code`,
`failures`, `filtered_tests`, `gates`, `git_ahead`, `git_behind`, `log_file`, `producer`,
`profile`, `real_seconds`, `repo`, `selection_mode`, `slot`, `sys_seconds`, `tree_dirty`,
`user_seconds`.

Read that table as the rule it encodes: **a consumer restricted to current receipts may rely on
27 fields; one that scans all history may rely on 6.** State which scope you are reading before
claiming a field is present, and never generalise a guarantee measured on one scope to another.
`profile`, `executed_tests`, `gates` and `coverage` are all in the second group — absent from the
all-history guarantee — so an all-history scan must treat every authority-deciding field as
optional.

Envelope keys guaranteed in all 1491: `commit`, `emitted_at`, `event_id`, `event_type`, `host`,
`legacy_row`, `outcome`, `producer`, `run_id`, `schema`, `team`.

## `git_depth`: the bash producer is gone

`git_depth` was written by the BASH producer (`validate.sh`) and is not written by the Rust
producer (`scripts/validate.rs`). Its disappearance is a producer retirement, not a conditional
field, and the daily curve shows the cutover cleanly:

```
08-03..08-06  100%      08-08   23.6%   <- bash -> Rust cutover
08-07          96.2%    08-10..  0.0%
```

Presence also varies WITHIN a single `schema_version` (v3: 437/549, v5: 58/534), so
`schema_version` does not predict it either.

**Do not backfill it.** No consumer reads `git_depth` — a whole-tree grep finds zero readers, so
nothing refuses on it and nothing silently defaults. Writing values for it now would manufacture
data no producer ever measured, which is strictly worse than an absent field with no readers.

## What would make the version mean something

Two options; either is honest, and the current state is neither:

1. **Bind it.** `validate-ledger/v1` asserts an exact envelope key set, the writer refuses to emit
   a record that does not match, and any shape change takes a new version string.
2. **Rename it.** If the envelope is intended to be extensible, call the field something that does
   not read as a contract (`format_family`, say) so no consumer treats it as a guarantee.

Leaving a field named `schema` that permits five shapes is the defect — not the shape variance
itself. This matters more since GitHub Actions was turned off: the ledger is now the sole
validation authority, and a version string that constrains nothing is the weakest link in the
thing everything else trusts.
