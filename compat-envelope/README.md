# compat-envelope: automated cross-backend compatibility measurement

This directory is the **canonical compat-envelope scorecard system** for the
dev-hermit workspace. It measures, in machine-readable form, how each Hermit /
Reverie execution backend compares to the golden **ptrace** reference across the
e2e test corpus, and renders the owner's scorecard table from that data.

> **Looking for the numbers, not the machinery?** Read
> [`SCORECARD-CURRENT.md`](SCORECARD-CURRENT.md) — the current rendering of all
> four CSVs with every column, denominator, and certification limit stated
> inline. It is the entry point; this file documents the system that produces
> it. `SCORECARD.md` and `REPORT.md` are earlier (2026-08-04) narrative
> analyses, superseded for current numbers.

It replaces the retired free-text
[`ai_docs/deprecated/progress_report_template.md`](../ai_docs/deprecated/progress_report_template.md)
(fake-cell risk): every number here is produced by actually running a cell, and
a cell exists **only** for a B1.5+ backend running the canonical shared tool
(anti-fakery gate #152). No fake cells for incomplete backends.

All CSVs live **in this outer dev-hermit repo**, never inside the inner
`hermit/` or `reverie/` checkouts (those stay low-noise).

## The scorecard, in one picture

Rows are e2e manifest buckets plus a `TOTAL`. The leftmost column is the golden
**ptrace** integer count — tests passing `--strict` + replay — which is the B4
denominator. Every other backend column is `stdout-parity%, determinism%` as a
fraction of that ptrace denominator:

- **stdout-parity%** — piped guest stdout has a SHA-256 match with the ptrace
  reference. This is an **upper bound**, not full cross-backend parity: it does
  not compare the INFO log, stack detlog, or heap detlog. The optional stack and
  heap fields record only whether stronger-mode flags were passed, not whether
  those outputs matched. TTY behavior is also outside this scorecard.
- **determinism%** — the backend is self-deterministic (run1 == run2), whether or
  not it matches ptrace. Determinism and stdout parity are independent signals;
  neither implies the other.

A cell a backend never ran counts as `0` in both (honest 0/0, never blank-as-green).

## Components

| File | Role |
| --- | --- |
| `lib/scorecard.rs` | Owns the complete `ScorecardRow`, every column, and every published scorecard variant imported by the Rust producers and renderer. |
| `check-scorecard-schema.rs` | Checks the Rust type against `scorecard-schema.json`, every producer import, and every published CSV header and row width. `check_emitter_schema.py` remains as a compatibility entry point. |
| `collect-envelope.rs` | **Hermit** collector. Drives `hermit/ci/test_harness.sh`, consumes its JSONL, and writes an ignored per-run observation. `publish-scorecard.py` is the explicit history/current-view publication boundary. Two modes (below). `--assert-green` turns it into the regression gate. |
| `collect-reverie-compat.rs` | **Reverie** collector (owner directive #1, run first). Runs the shared Reverie counter Tool through the ptrace and KVM launchers and records ptrace-vs-kvm parity into `reverie-scorecard.csv`. |
| `collect-e9patch-compat.rs` | **e9patch** collector. Runs the freestanding raw-syscall corpus through the ptrace backend both un-rewritten (golden) and e9tool-rewritten, and records preprocessing-invariance (golden-vs-e9 parity) with honest L1/L2 levels into `e9patch-scorecard.csv`. e9patch is NOT a Detcore backend (see below), so it lives in its own CSV like reverie. |
| `hermit/tests/backend-parity/run_matrix.py` | Runs the focused ptrace/DBI/KVM contracts and writes an ignored per-run observation; it never dirties the tracked current scorecard as a validation side effect. |
| `expansion-dag.rs` | Generates the **expansion-mode** safe-ci-dag-runner DAG: one boxed step per cell with per-cell wall-time + memory budgets, plus a dated evidence run dir. |
| `render-scorecard.rs` | Reads a scorecard CSV and renders the owner's table (`--json` / `--tsv` also). Shared by all collectors. |
| `render-current-scorecard.sh` | Renders **all four** CSVs in one pass, in the order published in `SCORECARD-CURRENT.md`, plus the provenance block and the `verify_compare` certification-tier distribution. Re-run and diff to detect doc drift. |
| `SCORECARD-CURRENT.md` | The published, human-readable rendering: current tables + schema + interpretation + provenance + known limitations. Start here. |
| `scorecard.csv` | Current Hermit Detcore-envelope view: newest row per `(test_id, backend)` (schema below). |
| `history/scorecard-observations.csv` | Append-only Hermit observation history from which the current view is derived. |
| `reverie-scorecard.csv` | Reverie B1.5 Guest/Tool-boundary results (same schema). |
| `e9patch-scorecard.csv` | e9patch preprocessing-invariance results over the ptrace backend (same schema). |
| `pre-tightening-baseline-20260806/` | **Frozen, self-contained HISTORICAL snapshot**, not part of the live pipeline: one `collect-fullcorpus.sh` sweep at Hermit `4c70658e` / Reverie `dd3c178e`, captured as the matched *before* state for an upcoming strictness tightening of the comparison contract. Carries its own `scorecard.csv` (the 19-column schema plus a `verify_compare` tier column), `metadata.json`, `no-results.csv`, `sweep-transcript.txt`, `loadavg.tsv`, and a `generate.py --check` that asserts the README still matches its inputs. Nothing else reads it; do not append to it. |

All `.rs` files are [`rust-script`](https://rust-script.org) executables
(`chmod +x`, run directly). They resolve their own directory via
`RUST_SCRIPT_BASE_PATH`, so default CSV paths land next to the script.

## Data flow: `validate` → intermediate → CSV → scorecard

The compat data is **not** produced by `cargo nextest`. It comes from Hermit's
own e2e runner, `hermit/ci/test_harness.sh` — a bash + `jq` harness that runs each
manifest cell and emits results. `safe-ci-dag-runner` is still used, but **only in
GitHub CI**, where it *schedules* `test_harness.sh run` steps in cgroup-boxed DAG
nodes (`hermit/ci/dag/{portable,privileged}.json` via `hermit/ci/run-dag.sh`); it
does not replace the harness or introduce nextest.

The intermediate format is **JSONL** (one JSON record per cell). The harness's
`append_result` writes `results.jsonl`; it *also* emits a parallel `junit.xml`
(for CI test reporting), but the collectors consume the **JSONL**, never the
JUnit.

Two concrete pipelines share that runner:

**CI split lanes** (portable / privileged), driven by `.github/workflows/compat-envelope.yml`:

```
make compat-envelope            (Makefile; builds release hermit --features dbi)
  └─ compat-envelope/validate-envelope.sh --lane portable
      └─ compat-envelope/collect-envelope.rs --mode regression --with-parity --assert-green
          ├─ bash hermit/ci/test_harness.sh plan --lane L --format json      → cell list
          ├─ bash hermit/ci/test_harness.sh run  --lane L --category B --backend BE
          │      --results hermit/ignored/e2e/compat-envelope/L/B/BE.jsonl    ← JSONL intermediate
          │      (bash+jq runner — NOT nextest; also emits …/junit.xml)
          ├─ reads that JSONL (outcome, duration_ms, reason)
          ├─ --with-parity: re-run guest under ptrace + backend, SHA-256 stdout compare → stdout parity
          └─ writes per-run rows → compat-envelope/ignored/envelope/
                                  └─ explicit publish-scorecard.py
                                      ├─ appends history/scorecard-observations.csv
                                      └─ replaces scorecard.csv current view
      └─ collect-reverie-compat.rs → reverie-scorecard.csv
      └─ render-scorecard.rs --csv scorecard.csv --latest            → rendered table
```

**Local definition-of-done** (`make validate`, this repo, a box with `/dev/kvm`):

```
make validate → compat-envelope-fullcorpus
  └─ builds release hermit --features third-party-backends
  └─ compat-envelope/collect-fullcorpus.sh
      (enumerates the FULL 235-cell corpus, auto-detects every runnable backend,
       ptrace FIRST to write the plain --strict parity reference, then each backend)
      → compat-envelope/fullcorpus-scorecard.csv  → render-scorecard.rs
```

**Storage paths.**

| Artifact | Path |
| --- | --- |
| Harness result root | `hermit/ignored/e2e/` (`E2E_RESULT_ROOT` override) |
| CI DAG JSONL + JUnit | `hermit/ignored/e2e/<lane>/<category>/{results.jsonl,junit.xml}` |
| Collector JSONL | `hermit/ignored/e2e/compat-envelope/<lane>/<bucket>/<backend>.jsonl` |
| CSV compat-logs | `compat-envelope/{scorecard,fullcorpus-scorecard,reverie-scorecard}.csv` (this outer repo) |
| Backend-parity observations | per-run artifact from Hermit's `run_matrix.py`, then explicit publication through `publish-scorecard.py` |
| Raw logs / scratch | `compat-envelope/ignored/` (gitignored) |

## Local full-corpus gate (`collect-fullcorpus.sh`)

`collect-envelope.rs` measures the portable, ci=true subset — the right scope for
a GitHub runner that may lack `/dev/kvm` or the feature build. On a fully
provisioned local box the definition-of-done should instead be the **union of
both lanes = the full 235-cell verify corpus across every runnable backend**.
`collect-fullcorpus.sh` does exactly that and is what `make validate` runs locally
(the split targets stay CI-only):

- Enumerates the full corpus from `corpus/corpus-c.tsv` (214 compiled C guests) +
  `corpus/corpus-nonc.tsv` (21 shell/interpreter cells) — the same denominator as
  `corpus-manifest.csv`.
- **Auto-detects** backends from the binary's `--backend` enum + host
  (`/dev/kvm` for KVM; `--features third-party-backends` for dbi/sabre/e9patch);
  a missing backend is recorded `n/a`, never a false red.
- Runs **ptrace first** to write the stdout reference, then each other backend:
  `det` = `--strict --verify` exits 0; `stdout_parity` = backend stdout SHA-256
  == reference. It is not the four-signal parity standard.
- **Parity reference gotcha (important):** the reference is captured with plain
  `hermit run --strict`, *not* `--strict --verify`. `--verify` does an internal
  double-run and emits **no** guest stdout to the parent, so a `--verify` capture
  is 0 bytes and every backend's parity-vs-reference collapses to ~0. Cells where
  ptrace itself fails under plain `--strict` are marked (`ptv.fail`) so downstream
  backends record `stdout_parity=""` (unmeasured), never a false
  empty-vs-empty match.
- **Ratchet-asserts** each backend's det count against a measured floor
  (ptrace 214, e9patch 214, sabre 199, dbi 190, kvm 160, liteinst 118); a drop
  below the floor fails the gate. The existing 205-cell floors and the thirty new
  performance cells were measured with Hermit `82a8e853` and uniform lane flags.

## CSV schema (shared contract)

`lib/scorecard.rs` owns the complete `ScorecardRow`, its columns, and every
published file variant. Rust collectors and the renderer import that type
directly. `scorecard-schema.json` is its checked projection for the Python and
shell producers. `check-scorecard-schema.rs` refuses a mismatched projection,
an untyped producer, an undeclared scorecard, a header difference, or a row-width
difference before the compatibility run starts.

```
run_id,run_utc,hermit_sha,reverie_sha,dirty,run_mode,lane,bucket,
test_id,test_mode,backend,cell_state,outcome,deterministic,<observable>_parity,
output_hash,duration_ms,max_rss_kb,reason,verify_compare,bitwise_parity,
compared_log_messages,tier,legacy_parity_unqualified,ref_output_hash,
parity_comparator,parity_tier,profile_flags,population_id,selected_count,
executed_count,evidence_count,comparison_tier,exit_code_parity,detlog_parity,
oracle_verdict,stack_parity,heap_parity
```

- `run_mode` — `regression` | `expansion` (hermit) or `reverie`.
- `bucket` — e2e manifest bucket, or `reverie-examples`.
- `test_mode` — hermit: `verify` | `replay` | `chaos` | `naked` | `custom`;
  reverie: `counter`.
- `backend` — `ptrace` | `dbi` | `kvm` | `sabre` | `liteinst`.
- `cell_state` — `enabled` (in the regression envelope) | `disabled` (expansion
  candidate).
- `outcome` — `pass` | `diverge` | `fail` | `error` | `timeout` | `oom` |
  `skip` | `unavailable` | `not-exercised` | `gap`. `error` is an understood
  infrastructure failure and is not product-regression evidence; its `reason`
  names the cause. `tier=gap` only describes missing comparison strength and
  does not replace the outcome.
- `deterministic` / `<observable>_parity` — `1` | `0` | blank (unknown).
  Hermit and e9patch stdout comparisons write `stdout_parity`; Reverie counter
  comparisons write `tool_count_parity`. Scorecards written before this rename
  use the ambiguous `parity` spelling. A parity boolean is qualified only when
  the same row carries both SHA-256 operands, exact product SHAs, comparator,
  tier, full profile, population receipt, and selected/executed/evidence counts.
  Historical booleans missing those conditions live in
  `legacy_parity_unqualified` and are never counted by the renderer.
  `deterministic` records run1==run2 independently of either parity observable.
- `output_hash` / `ref_output_hash` — SHA-256 operands for the candidate and
  reference observables (guest output or the decimal syscall-count text).
- `parity_comparator` / `parity_tier` / `profile_flags` — the comparison
  contract and exact argv/profile that condition a qualified verdict.
- `population_id` — SHA-256 receipt over the sorted selected row identities;
  the verifier re-derives it. The three count columns state the selected,
  executed, and evidence-bearing denominators for that run.
- `max_rss_kb` — filled by the expansion cgroup path; blank in the fast lanes.
- `comparison_tier` — the cross-backend certification standard carried on
  every row. `full-stdout-info-stack-heap` and
  `stdout-info-stack-heap-spot-check` are the only values that qualify a raw
  pass as green. `legacy-unqualified`, `unqualified-stdout-only`, and
  `unqualified-tool-count-only` preserve weaker cross-backend evidence
  explicitly. `unqualified-self-verify-only` records only repeated-run or
  within-backend self-consistency; it makes no cross-backend stdout, Tool,
  stack, or heap parity claim. `unqualified-no-comparison` records a fresh row
  for which no comparison witness exists, such as an unexecuted gap; it asserts
  neither self-consistency nor cross-backend parity. Every `unqualified-*`
  value is non-green. This is deliberately separate from `tier`, which records
  the self-determinism comparator used within one backend.
- `stack_parity` / `heap_parity` — optional invocation modes, despite the
  historical names: `1` means `--detlog-stack` / `--detlog-heap` was passed,
  `0` means it was not, and historical blank means unknown. They are not
  comparison outcomes. Highest-grade green is existing base green plus both
  stronger modes true.

The current scorecard keys logical cells on `(test_id, backend)`.
`--all` is accepted only for a single run identity; mixed-run aggregation is
refused. Use `--run-id` (or `--latest`) so a table has one code/population state.

## Two modes (hermit envelope)

### Regression — every `validate` + CI run

Runs only the **known-green** (`enabled`) cells, asserts they stay green, and
writes the green-cells CSV as a side effect. This is fast and safe.

```bash
./collect-envelope.rs --mode regression --lane portable \
    --repo ../hermit --with-parity --assert-green
```

`--assert-green` exits non-zero and lists every enabled cell that no longer
passes, so it is the drop-in check for the outer-repo `validate` entry point and
CI. It leaves the CSV updated regardless, so the scorecard always reflects the
last run.

### Expansion — periodic, full superset, hard-bounded

Runs the **full superset** including currently-failing / `disabled` cells to
catch failing→passing flips (compat growth). Because broken cells infinite-loop
or OOM, every cell is boxed by the `safe-ci-dag-runner` (229) with a per-cell
timeout + cgroup memory cap. `expansion-dag.rs` generates that DAG:

```bash
./expansion-dag.rs --csv scorecard.csv --repo ../hermit --lane portable \
    --backends dbi,kvm,sabre,liteinst --headroom 1.5 --dag-out /tmp/expand.dag.json
# validate then run with 229's runner:
python3 -m safe_ci_dag_runner.cli ascii --dag /tmp/expand.dag.json
```

**Per-cell budget:** for each backend, the geo-mean wall-time and max-mem ratio
vs ptrace across the green envelope is computed; a red frontier cell's budget is
`ptrace_baseline(test) × backend_geomean × headroom` (default 1.5×) for both
time and memory. Fallback ratios are used only when a backend has no green
overlap yet.

**Evidence retention:** each run writes a dated dir under
`ignored/compat-envelope/<run-id>/` (gitignored) with one subdir per cell
containing INFO logs (the ptrace reference and the backend run), machine-readable
exec stats, and captured stdout/stderr. The last `--keep` (default 5) runs are
retained; older ones rotate out. This feeds the `debug/` framework.

## Reverie compat (run this first)

`collect-reverie-compat.rs` measures the Reverie **B1.5 Guest/Tool boundary**:
the same shared counter Tool run through both the ptrace launcher
(`counter1`/`counter2`) and the KVM launcher
(`reverie-kvm-counter1`/`reverie-kvm-counter2`). The KVM launchers need a
statically-linked guest ELF + `/dev/kvm`, so the default corpus is static
busybox applets.

```bash
# build the launchers once (in the reverie checkout):
#   cargo build -p reverie-examples \
#     --bin counter1 --bin counter2 \
#     --bin reverie-kvm-counter1 --bin reverie-kvm-counter2
./collect-reverie-compat.rs --repo ../hermit --csv reverie-scorecard.csv
./render-scorecard.rs --csv reverie-scorecard.csv --denominator counter \
    --backends kvm --observable tool-count --latest
```

Only tools that have both launchers become tool-count-parity cells; a ptrace-only example
records its KVM cell as not-runnable (0/0), never faked.

**Current finding** (reverie `a4f33d69`, hermit `2f3689bd`, this host with
`/dev/kvm`): KVM is `0% tool-count-parity, 100% determinism` — fully self-deterministic but
surfaces a **constant 4 fewer syscalls** to the shared Tool callback than ptrace
(`true` 12→8, `echo hi` 15→11, `pwd` 16→12). That is a real Guest-contract
interception-surface gap, not a determinism defect, and is exactly the honest
B1.5 signal the scorecard is meant to expose.

## e9patch compat (preprocessing over ptrace, NOT a backend)

`collect-e9patch-compat.rs` measures **preprocessing-invariance**, not a
cross-backend parity. `hermit/AGENTS.md` is explicit that e9patch is not a
Detcore backend — it is binary-rewriting preprocessing used *with* the ptrace
backend. So this collector runs the freestanding raw-syscall corpus
(`hermit/tests/backend-parity/e9patch_corpus/*.c`) through the ptrace backend in
two arms and asks: is the e9tool-rewritten ELF's output bitwise-identical (L2) to
the same guest run **without** rewriting (the golden reference)?

- The `ptrace` column is the golden, un-rewritten reference arm (the denominator).
- The `e9patch` column is the e9tool-rewritten variant arm; its
  `stdout_parity` field means e9 stdout == golden stdout, both under ptrace.

It lives in its own `e9patch-scorecard.csv` (like reverie), never as a column in
the backend `scorecard.csv`, because a literal `e9patch` token in a backend field
would misread as a Detcore backend and violate the #152 anti-fakery gate.

**Honest L1 vs L2.** `deterministic=1` means the arm reached **L2** (`hermit run
--strict --verify` printed "Determinism verified" and exited cleanly). An arm
that ran under `--strict` (**L1**) but whose `--verify` leg did not confirm a
bitwise repeat is recorded with `deterministic=0` and a `reason` that says whether
L2 was missed because the verify leg **wedged** (PMU contention — an environment
limitation, `outcome=l1`, not a regression) or genuinely **diverged**
(`outcome=diverge` — a real finding). L1 is never reported as L2.

Both the strict and verify legs are retried up to `--verify-retries` (default 3)
whenever they wedge or skid-panic under fleet PMU load. If even the strict leg
never clears on any retry, the arm is unmeasurable environment noise, not a
confirmed red: it is recorded `outcome=skip` with **blank** determinism (never
`0`), so an env wedge is never rendered as a failing cell. A real non-124 strict
exit is the only `outcome=fail`.

```bash
export HERMIT_E9TOOL=../worktrees/e9patch/reverie/third-party/e9patch/e9tool
export HERMIT_E9PATCH_BACKEND=../worktrees/e9patch/reverie/third-party/e9patch/e9patch
./collect-e9patch-compat.rs --csv e9patch-scorecard.csv --assert-green
./render-scorecard.rs --csv e9patch-scorecard.csv --backends e9patch --latest
```

`--assert-green` treats only real defects (parity divergence or run failure) as
regressions; an environment verify-wedge (L1-only) is reported, not failed. The
e9 arm requires a hermit built `--features e9patch` plus `e9tool`/`e9patch` on
disk (defaults resolve to the `worktrees/e9patch` checkout).

## Rendering

```bash
./render-scorecard.rs --csv fullcorpus-scorecard.csv --observable stdout --run-id RUN_ID
./render-scorecard.rs --csv scorecard.csv --observable stdout --latest
./render-scorecard.rs --csv reverie-scorecard.csv --denominator counter \
    --backends kvm --observable tool-count --latest
./render-scorecard.rs --csv e9patch-scorecard.csv --backends e9patch \
    --observable stdout --latest
./render-scorecard.rs --csv fullcorpus-scorecard.csv --observable stdout --run-id RUN_ID --json
```

`--csv` is required. A bare invocation exits 2 rather than silently choosing
`scorecard.csv`, whose CI/regression population is much smaller than the full
corpus. `--observable` defaults to `stdout`; the Reverie counter scorecard must
select `tool-count`. The input path and observable are repeated in machine and
human output.

## Table markers (never let a `0` be ambiguous)

The examples below use the default `stdout-parity` label. With `--observable
tool-count`, the renderer uses the same markers under `tool-count-parity`.

- `X%?` — stdout parity **never measured** for that bucket → UNKNOWN, not a confirmed 0.
- `X%~` — **partial** stdout-parity coverage (some denom cells measured, some not).
- `n/a` — backend **ran zero** denom cells here (binary absent / not
  manifest-enabled) → not measurable, **not** a confirmed fail.

A real red (ran + failed) is `0%, 0%` with no marker; the markers keep it
visually distinct from "not measured" and "not runnable", which is what the
phase-2 bar (*every red a CONFIRMED red*) requires. Machine-readable
`--json`/`--tsv` carry observable-qualified fields such as
`stdout_parity_measured_count` or `tool_count_parity_measured_count`, plus
`ran_count` per cell.

## Stdout-parity measurement

`collect-envelope.rs --with-parity` runs the same guest under ptrace and the
backend and compares stdout SHA-256. `.sh` fixtures run via `--run`, `direct`
commands via `bash -c`, and **compiled `.c`/`.rs` fixtures are built the way the
harness builds them** (`build_compiled_fixture()`) into
`repo/ignored/compat-envelope-parity/<id>/` — NOT `/tmp`, which hermit isolates
and refuses to launch a guest from. Backend availability is probed once up front
(`backend_available()`); an absent backend binary is recorded `unavailable`
(blank determinism), never a fabricated red.

## Anti-fakery invariants

1. A cell exists **only** for a B1.5+ backend running the canonical shared tool.
   Pre-B1.5 work is "progress toward", not a cell.
2. Not-run / not-runnable cells are `n/a` or `0/0`, never blank-rendered-as-green.
3. `determinism%` never implies full parity; a deterministic backend whose
   stdout diverges from ptrace reads `0% stdout-parity, N% determinism`.
4. Every number is produced by an actual run recorded in the CSV with its SHAs.
5. An **unmeasured** stdout parity (`?`) or **unavailable** backend (`n/a`) is never
   presented as a confirmed 0 — measured-and-failed is the only real red.
