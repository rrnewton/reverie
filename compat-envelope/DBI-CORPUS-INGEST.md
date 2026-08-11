# DBI full-corpus L1-parity / L2-det → scorecard ingest (coordination handoff)

**From:** dbt ratchet lane (`dbt-corpus-round-nongated-3`, impl agent opus-4.8)
**To:** compat-scorecard owner (agent hermit-235)
**Producer:** `collect-dbi-corpus.rs` (additive; does NOT touch the four existing
collectors or `scorecard.csv`; its current default is the tracked
`compat-envelope/dbi-scorecard.csv`)
**Historical data described below:** `ignored/dbi-corpus-scorecard.csv` (404 rows: 202 `dbi` + 202
`ptrace-ref`), run_id `dbi-corpus-verify`, hermit
`13f3ee680d81353a53e019fc2c92101a010a2357` (branch `dbt/pidfd-open-self`,
origin/main `82a8e853` + pidfd fixture; dirty=true).

> This document preserves the original 202-test measurement. The producer now
> emits the shared 38-column schema with exact Hermit/locked-Reverie provenance,
> counted population receipts, and whole-cgroup CPU/wall containment. Reproduce
> new runs to the tracked default; do not treat the dirty historical artifact as
> current qualifying evidence.

## Why this exists

`collect-envelope.rs` only drives the manifest cells that a bucket marks
`backends_enabled` (plus manual). It therefore measures DBI on the *subset* of
the corpus that already opted DBI in — it cannot answer "how does DBI do across
the **whole** 202-test corpus, including tests that never enabled it?" This
producer reconstructs **every** corpus guest (184 `.c` compiled into a stable
per-test binary under the repo target tree, 18 `.sh` run with `--run`) and runs
it under both backends directly, independent of the manifest's enable flags.

## Methodology (READ THIS — it governs how to read the numbers)

Two separate measurements per test, deliberately at different assurance levels:

- **Parity = L1 (`--strict`, NO `--verify`).** Both backends pass guest stdout
  through unchanged, so a byte hash of ptrace stdout vs DBI stdout (plus exit
  code) is a real cross-backend comparison. **Parity is NOT measured under
  `--verify`**: under `--verify` the ptrace backend consumes guest stdout
  internally (0 bytes reach the parent) while DBI passes it through, so a
  cross-backend `--verify` stdout diff is a plumbing artifact, not divergence.
  This mirrors why `collect-envelope.rs::run_and_hash` uses `--strict` without
  `--verify`.
- **Determinism (det) = L2 (`--strict --verify`), measured per-backend.** DBI's
  own `--verify` double-run returning rc==0 is hermit's self-check that the run
  is bitwise-repeatable. This is a same-backend property, never cross-backend.

Portable profile on every run (matches `run_and_hash` for comparability):
`--strict --no-virtualize-cpuid --max-timeslice=disabled`, `LC_ALL=C TZ=UTC`,
150s timeout, child in its own process group and SIGKILL-reaped after `timeout`
returns (hermit forks a background supervisor that outlives the foreground; the
group kill prevents a surviving supervisor from wedging the sweep).

### Outcome taxonomy (the `outcome` column)

| outcome | meaning | det | parity |
| --- | --- | --- | --- |
| `pass` | **B4**: L1 byte-identical to ptrace AND L2 `--verify` clean | 1 | 1 |
| `parity-gap` | DBI L2-deterministic on its own but L1 output **diverges from ptrace** | 1 | 0 |
| `parity-not-det` | L1 byte-identical to ptrace, but L2 `--verify` did not return clean | 0 | 1 |
| `gap` | DBI L1 hung/errored, or diverged **in error** from ptrace | 0 | 0/blank |

## Results @ `13f3ee68` (202 tests)

| metric | value |
| --- | --- |
| **L1 parity (byte-identical to ptrace golden)** | **155/202 = 76.7%** |
| **B4 (L1 parity + L2 det)** | **135/202 = 66.8%** |
| parity-gap (det, diverges) | 23 |
| parity-not-det (L1 parity, L2 unclean) | 20 |
| gap (L1 hang/error) | 24 |

Per bucket (dbi rows), `parityL1 / B4pass / n`:

| bucket | parityL1 | B4 | n |
| --- | --- | --- | --- |
| applications | 2 | 2 | 3 |
| backend-parity-c | 2 | 2 | 3 |
| bin-c | 1 | 1 | 2 |
| c-programs | 129 | 118 | 159 |
| chaos-c | 1 | 1 | 1 |
| data-handling | 2 | 0 | 2 |
| debugger-c | 1 | 1 | 1 |
| determinism-stress | 3 | 1 | 4 |
| determinism-stress-c | 7 | 6 | 10 |
| language-runtimes | 1 | 1 | 6 |
| shared-futex-c | 3 | 0 | 4 |
| system-utils | 2 | 2 | 6 |
| util-c | 1 | 0 | 1 |

## IMPORTANT caveat on the L2/det numbers (host-sensitivity)

The sweep ran on the shared, heavily-loaded host (devbig014) alongside ~10
other agents' runaway 20–30h-old `rustbin_*` spinners. `--verify` is a
double-run and is **timing-sensitive under load**: after the sweep,
`bin-c/posix-timer-test` — recorded `pass`/det=1 during the sweep — took >120s
to `--verify` (it completed fast enough at sweep time). So:

- **L1 parity (155/202) is the robust primary signal** (single run, load-tolerant).
- **`parity-not-det` (20) is NOT a parity failure** — L1 is byte-identical to
  ptrace. Its L2 is inconclusive: a mix of (a) tests that structurally can't
  `--verify` under DBI (threaded / no-preemption: `thread-contention`,
  `process-chains`, `qemu-*`, `pmu-skid`) and (b) possible sweep-time
  verify-timeouts under load. Do **not** present these as non-deterministic
  without a re-run on a quiet host.

## How to fold (owner action, hermit-235)

The renderer keys logical cells on `(bucket, test_id, test_mode, backend)`. Every
producer row uses `test_mode = verify` and the **real corpus buckets**
(`c-programs`, `system-utils`, …), so DBI cells here **can collide** with
`collect-envelope.rs` DBI cells for manifest-enabled tests. Choose one:

1. **Separate view (recommended):** ingest under the distinct `run_id`
   `dbi-corpus-verify` and render it as its own full-corpus DBI column, not
   merged into the manifest-driven cells.
2. **Merge:** if you want these in the master grid, relabel `test_mode` to a
   distinct value (e.g. `dbi-corpus`) before concat so there is zero collision:
   ```bash
   tail -n +2 ignored/dbi-corpus-scorecard.csv \
     | awk -F, 'BEGIN{OFS=","}{$10="dbi-corpus"; print}' >> scorecard.csv
   ./render-scorecard.rs --csv scorecard.csv
   ```

The `ptrace-ref` rows (backend `ptrace-ref`) are the golden L1 baseline this
sweep compared against; fold them only if you want the reference column, else
drop them (`grep ',dbi,'`). They will otherwise create a new `ptrace-ref`
backend column distinct from the real `ptrace` column.

## Reproduce

```bash
set -a; source ../worktrees/dbt-compat/hermit/.env.dbt.slot; set +a   # DBI toolchain (gitignored)
./collect-dbi-corpus.rs --manifest ignored/manifest-harness.json \
  --timeout 150 --run-id dbi-corpus-verify --emit-ptrace-ref \
  --csv compat-envelope/dbi-scorecard.csv
```

`ignored/manifest-harness.json` = `hermit-manifest-plan --format harness-json`.
