# Type-discipline census: serialized artifacts and text classification

Date: 2026-08-27

## Result

I followed 23 repository-owned producer-to-consumer artifact families that do not have one complete shared type:

| Primary finding | Artifact families |
|---|---:|
| No complete type at all; writers and readers agree by key, column, or position convention | 11 |
| Multiple definitions of the same logical type | 12 |
| **Total not standardized on one type** | **23** |

The first two rows are mutually exclusive primary classifications. Other dimensions overlap them:

| Other dimension | Count |
|---|---:|
| Definitions split across Rust and Python, or Rust/Python/shell | 11 of 23 |
| Already shared correctly, retained as control examples | 4 |
| Serious text-classification families found by manual review | 14 |
| Maintained Rust scripts compiled by the current lint but never smoke-run with `--help` | 32 |

This is a producer-to-consumer census, not a count of files or raw search hits. One artifact family can have several writers, readers, versions, and files. The text scan found 391 candidate lines in 150 production files after excluding tests, fixtures, vendored code, dated evidence, experiments, benchmarks, Markdown, and JSON data. Manual inspection, rather than the raw count, produced the 14 findings below.

## Scope and revisions

The census covers production code and committed artifacts in:

- `dev-hermit` latest main and report base at [`d5673dfb362bc3530d6095799fc8ca46b34b14e0`](https://github.com/rrnewton/dev-hermit/tree/d5673dfb362bc3530d6095799fc8ca46b34b14e0). Source citations below remain pinned to `c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6`, the exact snapshot inspected before main advanced by one deletion commit. I reviewed that commit's changed production files and recomputed the ledger, scorecard, Rust-script, retry, and pattern counts on `d5673dfb`; none of the 23 artifact-family or 14 text-classification findings changed.
- `hermit` main at [`26f5bc2de3d328b64c41c42ef093ecb9d89e98ab`](https://github.com/rrnewton/hermit/tree/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab)
- `reverie` main at [`063fa37b05e562760f3d27c80ed8a6482b97b44a`](https://github.com/rrnewton/reverie/tree/063fa37b05e562760f3d27c80ed8a6482b97b44a)
- `liteinst2` main at [`ac6df266c76dcd5c724df958321ff4f9c0b23f63`](https://github.com/rrnewton/liteinst2/tree/ac6df266c76dcd5c724df958321ff4f9c0b23f63)

Excluded: tests and fixtures, vendored/third-party source, dated evidence snapshots, experiments and benchmarks, and human-only documents. A format is included when repository code both produces and later interprets it, or when it is a live authority such as the validation ledger or worktree registry.

## Sharpest findings

### A missing input can look like a measured zero

Four later measurements exposed the same failure in different readers. The
reported value looked healthy because the reader silently omitted part or all
of the population it claimed to count:

- disk-residue reporting returned zero quarantined slots while the live
  registry contained 25 rows in the actual `lease-quarantined` state;
- slot/agent parity counted all 18 named snapshot entries as live instead of
  applying the shared status meaning, which yielded 12 live agents and exposed
  two more active slots without a live owner, including assignments to the
  departed `cloudlint` and `cloudrecover` agents;
- red attribution returned zero per-node classifications from 978 failed rows
  because it read only seven surviving `/tmp` logs and ignored the durable
  `failed_substep_classes` already stored in the rows; consuming that field
  restored 681 classifications across 226 log-missing rows; and
- a DBT cell recorded as an infrastructure error concealed a divergence that
  reproduced three times out of three because two log files used a prefix the
  scanner did not include.

A zero or green result is therefore evidence only when the reader also proves
which population it observed and names unavailable or omitted inputs. Missing
input must remain distinguishable from an observed empty result.

### The ledger does not record which host state produced a row

[`ledger/hermit/devbig014/2026-08.jsonl`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ledger/hermit/devbig014/2026-08.jsonl) contains 1,989 rows. All 1,989 omit a kernel release, boot ID, host capability set, and `host_class`:

| Field or equivalent | Rows carrying it |
|---|---:|
| `kernel` or `kernel_release` | 0 / 1,989 |
| `boot_id` | 0 / 1,989 |
| `capabilities` or `capability_set` | 0 / 1,989 |
| `host_class` | 0 / 1,989 |

The owner observed the live machine reboot to kernel 7.1.3 at 07:31 on 2026-08-27 and observed a capability change. The versioned blob audited here ends at 06:39 local time, so it does not independently preserve the later side of that transition. That is itself the point: the committed row shape has no field capable of distinguishing rows made under the two host states. A filename containing only host and month cannot do that job.

### `host_env_signature` does not record the host environment

The producer [`_host_env_signature_for`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/failure_evidence.py#L539-L563) scans failure text for missing-command, missing-file, and package-manager phrases and returns the first matched phrase. It records neither kernel, boot identity, capabilities, package inventory, nor any other host state. The same file correctly describes the stronger direction immediately above it: emit a closed `FAULT <class>: <detail>` token rather than infer a class from prose.

There are 0 populated `host_env_signature` values in the 1,989-row committed ledger, so this report does not claim an existing row was misread. The defect is in the field contract: whenever populated, a name asserting host identity carries a matched error phrase instead. A reader trusting the name cannot detect that substitution.

Hermit independently performs a larger phrase-based classification in [`environmental_block_observation`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/lib/validate_runtime.rs#L202-L490). The two repositories therefore have separate Rust and Python classifiers for the same failure classes, while the serialized row has no shared exhaustive type.

### `retry_class` contains detail text rather than a class

Across the tracked ledger, 414 `retry_class` values contain 16 distinct strings but only four leading classes:

| Leading value | Rows |
|---|---:|
| `always-eligible` | 368 |
| `bound-kill under contention` | 27 |
| `measured-unstable ...` | 17 |
| `proxy-egress` | 2 |

The writer appends the changing reason directly to the class, for example [`bound-kill under contention: {reason}` and `always-eligible: {reason}`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs#L9783-L9827). Exact grouping therefore sees near-unique values such as timeout durations and exit codes. The producer should emit the existing class as a closed value and the human reason as a separate detail field.

### The harness reconstructs nextest executions from console text

[`nextest_test_observation`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs#L12555-L12615) parses `PASS`, `FAIL`, `TIMEOUT`, `LEAK`, `ABORT`, `SIGSEGV`, `SIGABRT`, and `TRY ... FAIL` lines. It then deduplicates identical rows because nextest prints each failing test once live and again in the recap. The repository's own regression example has six terminal-looking lines but only four executions ([source](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs#L8395-L8410)). A naive consumer doubled retry counts.

We do not own nextest, but we own the wrapper and validation row. The wrapper should consume a machine-readable nextest result and write one typed row per execution. The outer harness should not infer executions from display text.

### Positional LiteInst arrays let one index mean three things

The same logical dispatch-path counts exist as three positional arrays:

- ptrace-host [`dispatch_paths: [u64; 5]`](https://github.com/rrnewton/reverie/blob/063fa37b05e562760f3d27c80ed8a6482b97b44a/reverie-ptrace/src/liteinst_stats.rs#L25-L31), where index 2 is cache-line fallback;
- in-guest [`paths: [u64; 5]`](https://github.com/rrnewton/reverie/blob/063fa37b05e562760f3d27c80ed8a6482b97b44a/reverie-liteinst/src/stats.rs#L261-L266), where index 2 is also the in-guest straddler fallback;
- public [`[u64; 7]` in `LiteinstDispatchPath` order](https://github.com/rrnewton/reverie/blob/063fa37b05e562760f3d27c80ed8a6482b97b44a/reverie-liteinst/src/stats.rs#L203-L219), where index 2 is `InGuestSigsys`.

The conversion at [`from_ptrace_host_hybrid`](https://github.com/rrnewton/reverie/blob/063fa37b05e562760f3d27c80ed8a6482b97b44a/reverie-liteinst/src/stats.rs#L147-L170) manually reads five positions after two variants were inserted into the seven-variant public order. Adding variants did not force every conversion to be updated. The representation needs named exhaustive values at every boundary, not raw arrays plus comments.

### Scorecard schema checks parse source and currently refuse main

The compatibility scorecard has six declared file variants and 38 core columns. The repository also restates the shape in hard-coded emitter `HEADER` strings and a renderer `REQUIRED_COLUMNS` list. [`check_emitter_schema.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/compat-envelope/check_emitter_schema.py#L47-L70) uses regular expressions to discover headers and default targets from source text.

At the audited revisions it discovered five emitters and exited 1:

- one emitter target was unparseable;
- `collect-reverie-compat.rs` declared 36 columns while its default `scorecard.csv` had 40;
- `reverie-scorecard.csv` was not core-prefix safe.

This is direct evidence that the JSON definition is not the type used by writers. It is a checker comparing independent definitions after the compiler has already accepted them.

## No complete type at all: 11 artifact families

These have no complete Rust struct/enum defining the serialized shape. Producers assemble dictionaries, `serde_json::Value`, `json!` objects, positional CSV rows, or shell/JQ expressions; readers repeat required keys and accepted values.

| # | Artifact family | Convention and locations |
|---:|---|---|
| 1 | Ledger event envelope | Python tuples and dictionary operations in [`ledger.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/ledger/ledger.py) and [`validate_rows.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/ledger/validate_rows.py); Rust checks only the projected `legacy_row`. |
| 2 | Failed-substep evidence | [`failure_evidence.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/failure_evidence.py) returns dictionaries; Rust `HistoryRow` receives them through its untyped `extra` map. |
| 3 | Published validation receipt | Python dictionary construction in [`publish_receipt.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validation/publish_receipt.py), partial Rust `Value` reads in [`ci-hub.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/ci-hub.rs), and shell/JQ predicates in [`verify_receipt.sh`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validation/verify_receipt.sh). |
| 4 | Validation run handle | Dictionary writes and field checks in [`run_registry.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/run_registry.py) and [`start_unit.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/start_unit.py). |
| 5 | Stress result record | Dictionary/CSV construction, padding, truncation, and reads in [`stress_store.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/stress/stress_store.py). |
| 6 | Status-log row | Rust `json!` construction and `Value` reads in [`status-log.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/status-log.rs), with separately maintained schema constants. |
| 7 | CI Hub change manifest and seal | Dictionary construction and validation in [`ci-hub-clone-change.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/ci-hub-clone-change.py). |
| 8 | CI Hub change activation obligation | Separate JSONL dictionary shape in the same [`ci-hub-clone-change.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/ci-hub-clone-change.py). |
| 9 | QEMU demo run metadata | Dictionary construction and field-by-field reads in [`demo_common.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/demos/lib/demo_common.py), consumed by demos 05 and 06. |
| 10 | GitHub Actions run row | Fixed CSV column order and dictionaries in [`history/ingest.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/history/ingest.py) and [`history/query.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/history/query.py). |
| 11 | GitHub Actions job row | A second fixed CSV row in the same ingestion/query modules; no named row type or exhaustive job outcome value. |

## Multiple definitions of the same logical type: 12 artifact families

| # | Logical type | Independent definitions and conventions |
|---:|---|---|
| 1 | Validation history row | Three: parent Rust [`HistoryRow`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/lib/records.rs#L196-L350), Hermit `json!` construction in [`validate.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs), and Python canonical-event/projection dictionaries in [`validate_rows.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/ledger/validate_rows.py). |
| 2 | Verification report | Two Rust structs with the same serialized role: Hermit CLI [`verify.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/hermit-cli/src/bin/hermit/verify.rs#L658-L754) and manifest-plan [`canonical_verdict.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/ci/manifest-plan/src/canonical_verdict.rs#L21-L168). |
| 3 | Cell-results artifact and verdict | Parent Rust structs/enums in [`records.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/lib/records.rs#L396-L535), Hermit `Value` plus hard-coded key lists in [`validate_cell_results.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/lib/validate_cell_results.rs), and Python shape logic in [`qualifying_receipt.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/qualifying_receipt.py). |
| 4 | Qualifying receipt predicate | Rust [`QualifyingPredicate`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/lib/qualifying_receipt.rs#L245-L282) and an independent Python dictionary implementation in [`qualifying_receipt.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/qualifying_receipt.py), both interpreting the JSON file. |
| 5 | Compatibility scorecard row | At least seven code definitions: `scorecard-schema.json`, five emitter headers, and the renderer's required-column list, plus six declared file variants of which four are present in the audited checkout. The current checker already reports skew. |
| 6 | Stress series row | Python dictionary schema in [`series.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/series/series.py) and Rust `SeriesRow` in Hermit's [`scorecard.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/ci/compat-envelope/scorecard.rs). |
| 7 | Worktree registry | Independent `Value` field conventions in [`allocate-worktree.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/allocate-worktree.rs) and [`release-worktree.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/release-worktree.rs), plus Python field conventions in [`slot-owner-authority.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/slot-owner-authority.py) and [`worktree_liveness.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/worktree_liveness.py). |
| 8 | Worktree owner sidecar | Rust JSON construction/reads in allocator and release, and independent Python exact-field validation in `slot-owner-authority.py`. |
| 9 | Coordinator lease | Typed [`SentinelLease`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/codex-slot-sentinel.rs) but untyped `Value` handling and repeated validation in allocator/release. |
| 10 | Agent container registry | Typed `Lifetime`, `Ownership`, and `Registry` in [`agent-podman.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/agent-podman.rs), but `release-worktree.rs` reads the same file through `Value` and a separate command result convention. |
| 11 | Agent snapshot | Bridge outer struct with untyped agent entries in [`agent-container-reconcile-bridge.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/health/agent-container-reconcile-bridge.rs), Rust `Value` interpretation in `agent-podman.rs`, and Python dictionary validation in [`operational_health.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/health/operational_health.py). |
| 12 | LiteInst dispatch-path counts | Three positional arrays with two lengths and conflicting index meanings, detailed above. |

## Definitions split across languages: 11 of 23

These are the artifact families for which one language cannot import and exhaustively match the other language's definition:

1. Ledger event envelope — Python journal code and the Rust ledger checker/reader.
2. Failed-substep evidence — Python producer, Rust untyped `extra` consumer.
3. Published validation receipt — Python, Rust, and shell/JQ.
4. Validation history row — Hermit Rust writer, parent Python journal/projection, and parent Rust reader.
5. Cell-results artifact and verdict — Rust and Python.
6. Qualifying receipt predicate — Rust and Python.
7. Compatibility scorecard row — Rust, Python, shell, JSON, and CSV.
8. Stress series row — Python and Rust.
9. Worktree registry — Rust and Python.
10. Worktree owner sidecar — Rust and Python.
11. Agent snapshot — Rust and Python.

## Already shared correctly: 4 control cases

These show that the repository already uses the desired shape where a library owns the value and consumers import it:

1. Hermit validation code imports `DagConfig`, `Step`, `StepOutcome`, and `RunResult` from [`dagrun::model`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs#L102-L105), and sibling validation modules import the same definitions.
2. `detcore-model` owns [`SchedEvent` and exhaustive `Op`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/detcore-model/src/schedule.rs); detcore, hermit-cli, and hermit-verify import them.
3. Reverie's core crate owns [`BackendStatsRequest`, `BackendStatsSnapshot`, and `BackendStatsSource`](https://github.com/rrnewton/reverie/blob/063fa37b05e562760f3d27c80ed8a6482b97b44a/reverie/src/backend_stats.rs#L26-L69); ptrace, KVM, DBT, E9Patch, LiteInst, and Sabre implement or consume the same interface.
4. dev-hermit's [`lease.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/lib/lease.rs) owns the lease-state and decision types imported by both [`landing_lock.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/lib/landing_lock.rs) and [`validate_lock.rs`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/lib/validate_lock.rs).

## Text classified by pattern matching where a typed value or parse should do the work: 14 families

Exact syntax validation such as a full SHA, safe filename, or closed command-line flag is not counted here. The table covers pattern piles that infer semantic state from text or parse owned source without its grammar.

| # | What is inferred from text | Where | Do we own the producer? | What the producer could emit instead |
|---:|---|---|---|---|
| 1 | Host-environment, infrastructure, or code failure | Python [`failure_evidence.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/failure_evidence.py) and Rust [`validate_runtime.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/lib/validate_runtime.rs) | Yes: the DAG runner/Hermit wrapper produces the record, though some underlying diagnostics come from tools | The existing closed fault class as an enum value plus a separate detail string in the step result and ledger row. |
| 2 | Retry class | Hermit [`validate.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs#L9783-L9827) | Yes | A closed class value plus detail, rather than one concatenated string. |
| 3 | Per-test nextest execution and verdict | Hermit [`validate.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs#L12555-L12615) | No for nextest; yes for the wrapper and ledger producer | Consume nextest's machine-readable result and emit one typed test-attempt row. |
| 4 | Nextest aggregate test totals | [`run-nextest-counted.sh`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/ci/run-nextest-counted.sh), parent [`preflight.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/landing/preflight.py), and [`reverie_node.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/reverie_node.py) | No for nextest; yes for wrappers | A typed summary beside the per-test rows. |
| 5 | Validation service completion, exit, node count, and test count | [`service_result.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/service_result.py) | Mixed: systemd is external; the unit and validation result are ours | Read systemd's structured properties and the validation run handle/result artifact. |
| 6 | Sabre patched-site reach and fallback | [`sabre_reach.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/validate/sabre_reach.py) | Yes | A backend statistics JSON value with counts and an exhaustive reach state. |
| 7 | Validation step identity, lane, test, and manifest meaning from shell command strings | Hermit [`validate.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/validate.rs), [`validate_super.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/lib/validate_super.rs), manifest-plan, and [`audit-test-binary-registration.py`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/ci/audit-test-binary-registration.py) | Yes | Put the existing identity, lane, package, test, and result roles on the step/manifest as typed fields; render a shell command only at execution. |
| 8 | Scorecard header and target from Rust/shell source text | [`check_emitter_schema.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/compat-envelope/check_emitter_schema.py#L47-L70) | Yes | Writers and readers use one Rust row type; generate any JSON/CSV description from it rather than regex-parsing source. |
| 9 | Parent validation admission, active nodes, failures, retries, and exit status from the live log | [`owner-checkout-validate-watch.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/owner-checkout-validate-watch.py) | Yes | Read the run handle, step outcomes, and final ledger record. |
| 10 | Union-rebase/landing result from `RESULT ...` and error prose | [`land-pr.sh`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/landing/land-pr.sh#L517) | Yes | A structured result artifact with an exhaustive result value, relevant SHAs, and exit code. |
| 11 | Task, pull request, review, cherry-pick, and ancestry facts from prose | [`task_pr_join.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/landing/task_pr_join.py), [`approval_binding.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/health/approval_binding.py), and [`cherry_pick_context.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/landing/cherry_pick_context.py) | Mixed human/tool producers, but the project owns the durable record format | Store URL, repository, number, SHA, review verdict, and disclosure fields separately; prose remains explanation. |
| 12 | Operational counts and health states from human command output such as `(N rows)` and `problems: N` | [`operational_health.py`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/ci-hub/health/operational_health.py#L1210) | Yes | Every owned probe emits JSON with a closed state and numeric fields. |
| 13 | Dependency policy from TOML and Rust source text | [`check-detcore-backend-abstraction.sh`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/scripts/check-detcore-backend-abstraction.sh#L210-L269) | Yes | Parse Cargo metadata/TOML and the Rust AST completely; inspect dependency and path nodes rather than masked source text and regexes. |
| 14 | Detcore log events, addresses, timestamps, and turns | [`logdiff.rs`](https://github.com/rrnewton/hermit/blob/26f5bc2de3d328b64c41c42ef093ecb9d89e98ab/detcore/src/logdiff.rs#L368-L452) and later regex passes in the same file | Yes | Emit structured schedule/log events and compare those; render text only for humans. |

Pattern matching remains justified where the producer is genuinely outside the project and exposes no structured form, provided the accepted grammar is closed and failures are rejected rather than guessed. Examples include exact Git output forms and third-party tool statistics. That justification does not apply when both the record producer and consumer are repository-owned, which is true for most rows above.

## Exhaustiveness and smoke-test gaps

- The LiteInst example proves that `[u64; N]` plus index conventions does not make a new enum variant break every consumer.
- `VerificationReport`, cell-result values, retry classes, fault classes, and scorecard columns all have consumers that accept strings/`Value` and therefore cannot be made exhaustive by the Rust compiler.
- [`scripts/lint-rust-scripts.sh`](https://github.com/rrnewton/dev-hermit/blob/c2bd4ca5e9682f434f263730b45cd65e4b8fd1a6/scripts/lint-rust-scripts.sh#L14-L22) intentionally compiles rather than runs scripts. Its counted production population is 32. It runs zero scripts with `--help`, so startup, inline manifest loading, and argument parsing can fail even when `cargo check` succeeds.

For later implementation verification, a new enum variant must make every relevant consumer fail to compile until it handles the variant. A deliberately broken maintained Rust script must make the script smoke gate fail. This report adds neither rule; it establishes the population first.

## Direction for fixes

The highest-value order is:

1. Put host identity on every validation row and stop using `host_env_signature` as a name for a matched error phrase.
2. Separate retry class from retry detail and make the class exhaustive.
3. Have the test framework/wrapper write test-attempt rows instead of reconstructing them from nextest display output.
4. Replace LiteInst positional arrays with named exhaustive values across every conversion.
5. Make Hermit own and export the validation report/cell-result/history types that Hermit produces; dev-hermit imports those types from the local `./hermit` main checkout.
6. Derive scorecard writers/readers from one Rust type instead of checking hard-coded headers after the fact.

For a type owned by Hermit and consumed by dev-hermit, prepare and validate both commits before landing either. Land the Hermit main commit first and the dev-hermit main commit immediately afterward, from the same task. The short interval in which the parent fails to compile is deliberate and loud; the parent half must not be deferred to another task.

## Reproduction summary

- Ledger count and absent host fields: `jq -s` over the commit-pinned JSONL file.
- Retry values: recursive `jq` extraction over tracked `ledger/**/*.jsonl`, then exact-string and prefix counts.
- Scorecard: `python3 compat-envelope/check_emitter_schema.py` exited 1 with five emitters discovered.
- Maintained Rust scripts: `scripts/lint-rust-scripts.sh --count` printed 32.
- Pattern candidates: production-only `rg` scan for Rust/Python regex construction and shell `grep`/`sed` matching; 391 lines in 150 files on the report base, followed by manual producer-to-consumer review.
