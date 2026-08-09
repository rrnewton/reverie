# Fresh validate profiling: per-step parallel speedup and whether the thread caps are derived

Question: how fast is `validate` wall-clock right now, how good is its parallel speedup and what
actually bounds it, what is each step's individual speedup curve, and is each step's thread cap
derived from that curve or guessed?

Every number here was produced on 2026-08-07/08 in a dedicated slot. Nothing is inherited. See
`metadata.json` for full provenance and `results.csv` for the per-step table (all 47 nodes).

## Provenance

hermit `61edbef424ad83e0434730ec24eb83389a196243` (main tip; includes `702daa824` one-process
artifact scan and `0cc055ae4` scan disable), box `devbig014` (AMD EPYC 9D85, 316 cores),
`CI_DAG_JOBS=16`, portable profile (47 nodes), warm target unless stated, load1 32-47 at start,
slot `worktrees/soltiming/hermit`. The SHA is read from each run's own ledger `commit=` field and
cross-checked against `git rev-parse HEAD` of the checkout that ran.

## Methods

Whole-DAG runs: `scripts/validate.rs portable [--dag-file V] --perf-dir D`, launched in a transient
`systemd --user` service carrying the same env `ci-hub/validate/start_unit.py` builds, wrapped in
`/usr/bin/time -v`. Per-step sweeps: the step is emitted as a ONE-step DAG file with its
`--test-threads` rewritten and its deps stripped, then run through the same runner with the same
boxing at `CI_DAG_JOBS=1`, so the step has the box to itself. Pass/fail is recorded at every thread
point, because a cap justified by a failure at T>1 is a correctness cap and not a guess.

Accounting is cgroup-recorded, not sampled: `elapsed_s`, `cpu.usage_usec`, `effective_cores`,
`peak_bytes` come from the runner's own `--perf-dir` CSV. The whole-run figure from
`/usr/bin/time -v` was validated against `validate.sh`'s own `times` builtin on the same run and
agreed to the second.

## (1) Total wall clock, fresh

| configuration | cache | wall | CPU | CPU/wall | result |
| --- | --- | --- | --- | --- | --- |
| `validate.rs portable`, shipped config | warm | 323.8 s | 819.9 s | 2.53x | PASS 47/47 |
| `validate.sh --portable-only`, shipped | warm | 353.7 s | 907.8 s | 2.57x | PASS |
| `validate.rs portable`, shipped config | cold | 514.3 s | 3426.8 s | 6.7x | PASS 47/47 |
| `validate.sh --portable-only`, shipped | cold | 514.5 s | 3538.0 s | 6.9x | PASS |
| `validate.sh full` (7 gates, ledger row, contended) | warm | 566 s | 2975.6 s | 5.26x | PASS |

## (2) Overall parallel speedup, and what actually bounds it

Computed from the run's own per-node walls plus the dependency graph:

| configuration | total work | graph critical path | binding bound | ceiling | makespan | achieved | efficiency |
| --- | --- | --- | --- | --- | --- | --- | --- |
| shipped (`hermit_guest: 1`) | 666.2 s | 105.6 s | **282.5 s (hermit_guest serial queue)** | 2.36x | 306.8 s | 2.17x | 92% |
| `hermit_guest: 4` | 920.0 s | 131.1 s | 131.1 s (graph) | 7.02x | 152.9 s | 6.02x | 86% |
| `hermit_guest: 4` + per-step knees | 855.8 s | 123.5 s | 123.5 s (graph) | 6.93x | 138.9 s | 6.16x | 89% |

**I do not reproduce 4.24x, and the graph-shape ceiling is not the binding one.** In the shipped
configuration the graph-shape ceiling is 6.36-7.02x, but sixteen test nodes declare
`hint.resources = {hermit_guest: 1}` against `resource_caps.hermit_guest = 1`, so they run strictly
one at a time whatever `CI_DAG_JOBS` says. Their walls sum to 282-300 s against a 307-324 s
makespan: **the hermit_guest serial queue is 92-93% of the entire wall clock.** The real ceiling is
2.36x and the scheduler already achieves 92% of it. Neither the graph, nor the width, nor the
scheduler is the problem. One constant is.

Total work RISES when the cap is lifted (666 -> 920 s) because co-running nodes contend; the wall
still halves. That is a real cost of the change and is reported, not hidden.

## (3) Per-step speedup curves

Full table in `results.csv` (all 47 nodes: shipped wall, shipped CPU, shipped effective cores, peak
RSS, the knob each step exposes, the curve where one exists, knee, and pass/fail at every point).

Twelve steps expose `--test-threads`. Measured wall (s) at T = 1/2/4/8/16/32, boxed, alone on the
box; **n=3 replicated rows marked R**:

| step | shipped cap | T=1 | T=2 | T=4 | T=8 | T=16 | T=32 | speedup | knee | passes at every T |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| test.hermit_unit **R** | 1 | 8.37 | 2.06 | **1.22** | 1.20 | 1.25 | 1.20 | **6.9x** | 4 | yes |
| test.liteinst_strict **R** | 1 | 33.4 | 19.5 | 12.2 | **5.38** | 7.12 | 7.07 | **6.2x** | 8 | yes |
| test.cli **R** | 1 | 13.4 | 4.81 | 3.56 | 2.86 | **2.45** | 2.46 | **5.5x** | 16 | yes |
| test.detcore_misc | 1 | 0.70 | 0.55 | **0.45** | 0.50 | 0.85 | 0.85 | 1.6x | 4 | yes |
| test.app_strict_verify | 1 | 21.1 | 16.6 | **14.9** | 14.6 | 15.0 | 15.0 | 1.45x | 4 | yes |
| test.ignored_syscall_regressions | 1 | 1.35 | 1.05 | **0.95** | 1.01 | 1.00 | 1.00 | 1.4x | 4 | yes |
| test.arbitrary_binaries | 1 | 1.25 | **1.00** | 1.05 | 1.05 | 1.10 | 1.00 | 1.25x | 2 | yes |
| test.hermit_integration | 1 | 48.6 | **41.3** | 41.5 | 41.1 | 43.5 | 44.5 | 1.18x | 2 | yes |
| test.sabre_examples | 1 | 15.4 | 14.3 | 15.4 | 13.1 | 13.6 | 16.9 | ~1.0x (noise) | - | yes |
| test.hermit_modes **R** | 1 | 21.8 | 22.0 | 21.5 | 22.1 | 21.8 | 22.2 | **1.00x** | - | yes |
| test.command_strict_verify | 1 | 38.0 | 38.0 | 38.4 | 38.4 | 39.6 | 38.5 | **1.00x** | - | yes |
| test.detcore_parallel | 4 | 1.70 | 1.75 | 1.70 | 1.75 | 1.70 | 1.75 | **1.00x** | - | yes |

An unreplicated single-sample sweep initially showed `test.hermit_modes` at 8.3 s for T=16 (an
apparent 3.9x). Replication at n=3 refutes it: the step is flat at 21.5-22.2 s across T=1..32. That
point is retracted; do not use it.

The remaining 35 steps have **no test-thread knob at all**, and that is itself a finding:

- **13 `e2e.manifest_*` steps run their cells STRICTLY SERIALLY.** `run_required` in
  `ci/test_harness.sh` is a bare `while IFS=$'\t' read ...; do run_cell ...; done` loop with no
  jobs flag, no `&`, no `xargs -P`. Measured `effective_cores` is 0.7-1.1 for every one of them.
  They sum to 245-329 s of wall and only 4 may co-run. There is no cap to derive here because there
  is no knob to set; the parallelism is 1 by construction of the harness.
- Build/lint/doc steps expose `CARGO_BUILD_JOBS` / cargo inner jobs, which is a BUILD width, not a
  test width.
- Four steps are shell or python (`test.envelope_levels` a serial 20-rep probe loop,
  `test.applications_e2e` a shell script, `test.dbi_parity` a python matrix,
  `test.strict_compat` a re-entrant `validate.sh --portable-strict-compat-only`).

A separate measured artifact of the shipped run: `lint.clippy` (61.6 s wall, 0.03 effective cores),
`test.regular_crates` (62.7 s, 0.08), `doc.rustdoc` (60.9 s, 0.48) and `doc.doctests` (51.8 s, 0.32)
burn long walls at near-zero CPU — they are blocked, not slow, most plausibly on cargo's package
lock behind the concurrent build nodes. Worth its own investigation.

## (4) Is each cap derived from its curve, or guessed?

**Guessed, and the tree says so in its own words.** `ci/dag/README.md`:

> "The `rss_baseline_bytes` / `hard_mem_max_bytes` / `est_duration_s` values are **hand-estimated,
> not measured**."

> "The `hermit_guest: 1` serialization faithfully reproduces `validate.sh`, which ran these gates
> strictly one-after-another. It is **intentionally conservative**."

So the single setting that costs 92-93% of the wall clock was inherited from what the bash script
happened to do. It is the conservative-constant defect exactly.

**Nine of the twelve capped steps are capped BELOW their measured knee**, and every one of the
twelve passed at every thread count from 1 to 32 — no cap in this set is justified by an observed
correctness failure:

| step | shipped cap | measured knee | wall saved by moving to the knee |
| --- | --- | --- | --- |
| test.liteinst_strict | 1 | 8 | 28.0 s |
| test.cli | 1 | 16 | 11.0 s |
| test.hermit_unit | 1 | 4 | 7.2 s |
| test.hermit_integration | 1 | 2 | 7.3 s |
| test.app_strict_verify | 1 | 4 | 6.2 s |
| test.ignored_syscall_regressions | 1 | 4 | 0.4 s |
| test.detcore_misc | 1 | 4 | 0.25 s |
| test.arbitrary_binaries | 1 | 2 | 0.25 s |
| test.sabre_examples | 1 | 8 | ~2 s (within noise) |

Three caps are harmless because the curve is flat (`test.hermit_modes`,
`test.command_strict_verify`, `test.detcore_parallel` at 4) — they are still underived, they just
cost nothing. `test.detcore_parallel`'s cap of 4 is inert: the step is 1.00x flat.

Genuine derivations do exist in this file and are the model for how the rest should look:
`build.workspace` carries `preferred_inner_jobs=32 measured on a cold empty target at
hermit@846baeca: 130.35 s, cgroup-recorded peak=11663998976 bytes`; `test.hermit_unit` carries a
MEM-CAP DERIVATION at j=8 from a cgroup-RECORDED peak; and the former `pmu: 1` cap was RETIRED on
measurement (`experiments/pmu-concurrency-ceiling-measured_20260803`). All three are memory or
build-width derivations. **None of them is a parallel-speedup curve.** Before this experiment, no
step's thread setting had one.

## End-to-end confirmation

Each configuration is the same 47-node DAG, same SHA, same box, `CI_DAG_JOBS=16`, warm:

| configuration | makespan | vs shipped | result |
| --- | --- | --- | --- |
| shipped | 323.8 s | 1.00x | PASS 47/47 |
| per-step knees only, `hermit_guest: 1` | 292.7 s | 1.11x | PASS 47/47 |
| `hermit_guest: 4` only, shipped threads | 143.8 s / 152.9 s (n=2) | **2.19x** | PASS 47/47 both |
| `hermit_guest: 4` + per-step knees | 138.9 s | **2.33x** | PASS 47/47 |

**Raising one conservative constant from 1 to 4 cuts portable validate from 324 s to ~148 s with
all 47 nodes passing.** The per-step knees add ~9 s more on top of that, because once nodes overlap
the serial queue is no longer what binds. Lifting the caps without lifting `hermit_guest` buys only
11%: the two levers are not additive, and the cap is the one that matters.

## Limitations

- The whole-DAG configurations are n=1 or n=2, on a shared box whose background load was not held
  constant (load1 32-47). The 2.19x and 2.33x are large enough to survive that, the 11% is not
  comfortably so.
- Nine of the twelve per-step curves are n=1 sweeps; only `test.hermit_unit`, `test.cli`,
  `test.liteinst_strict` and `test.hermit_modes` are replicated at n=3. The n=1 points are marked
  `samples=1` in `results.csv`. One n=1 point (hermit_modes T=16) was already refuted by
  replication, so treat the other unreplicated points as provisional.
- "Passes at every T" is one observation per point, not a flakiness bound. Two genuine flakes were
  seen at the SHIPPED configuration during this session, unrelated to any change:
  `test.sabre_examples` (`sabre_non_racy_examples_verify_current_envelope`, rand.py "Failure:
  nondeterministic") once in five runs, and `e2e.manifest_system_utils`
  (`system-utils/clock-determinism`, liteinst, runs=5 failed_runs=1 distinct=2) once. Raising
  concurrency plausibly raises flake rates; that needs its own repeated-run study before any cap
  change lands.
- Every measurement is on the `portable` profile. `privileged.json` (which caps `{kvm: 1}`) was not
  measured.
- Reproduce with the runners in `ignored/soltiming/` (`sweep.py`, `confirm.py`, `launch.sh`,
  `run.sh`) against the SHA above.
