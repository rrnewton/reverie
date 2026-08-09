# Per-step parallel scaling for the CI DAG: plateau, regression, and whether the runner can derive them

**Owner directive:** *"Rerun parallel scaling experiments and make sure the ci-dag runner captures all
the data and can estimate the plateau for each CI step — that should be its max threads on a given
machine."*

**Headline, and it inverts the task.** The capability the owner asked for is **already ~80% built
inside `safe-ci-dag-runner`** — capture, a machine-scoped store, a sweep driver, and a knee
estimator all exist. It has **never produced a single estimate**, and five defects stand between it
and working. This report measures the curves the owner asked for, and reports the five defects with
executed evidence for each.

Nothing here is modelled from core counts. Every wall time was measured by the runner's own
`sweep` subcommand and read back out of its own profile store; this experiment adds **no external
stopwatch**.

---

## Provenance

| item | value |
| --- | --- |
| UTC run window | 2026-08-09 02:30 – 03:05 |
| host (first DNS label) | `devbig014` |
| CPU | AMD EPYC 9D85 158-Core Processor — **316 hardware threads** (158 cores × SMT2) |
| kernel | `6.18.39-0_fbk0_hardened_0_ga43d5727b443` |
| hermit SHA | `b0b7c9c1f77bbbdcb4ef92eb6330bb30821c04f2` |
| agent-utils SHA | `7eb49bd4240f1ae8929dbdaa75cee1a4375b4bb3` (clean) |
| dev-hermit parent SHA | `f27ea1259140a1fe93ea3d477198a4d502a51cd9` |
| python | 3.12.13+meta |
| slot | `worktrees/budget-fix/hermit` |
| ambient load1 across all 198 samples | min 58 / median 76 / max 107 — **every sample is `ambient_bucket=busy`** |
| payload | `/tmp/scaling-payload.bin`, 201326592 B, sha256 `07748d65…94d1` |
| producers | `runners/sweep-driver.sh` sha256 `edce345c…6fb7`; `runners/parallel-payload.sh` sha256 `f1dae10c…c1f5`; `runners/analyze-curve.py` sha256 `957720ee…0bcf3` |
| raw data | `raw/`, per-sample rows written by the runner |
| derived table | `results.csv` |

---

## Methods

**Measurement instrument.** `safe-ci-dag-runner sweep --dag D --step S --jobs J..J`, which runs one
step at one width and appends a row to
`step_profiles_<machine_id>_<container_class>.csv`. `elapsed_s`, `effective_cores`, `user_s`,
`sys_s`, `peak_bytes`, `ambient_bucket`, `load1_*` are all the runner's own cgroup-recorded values.

**Why `systemd-run --user` wraps every sample.** An agent sandbox scope exposes
`cgroup.controllers = io memory pids` — **no `cpu` controller**, so `cpu.max` cannot be written.
Since a sweep's width manifests *as* `cpu.max` (below), an unwrapped sweep would vary **nothing**
and produce flat curves that are pure artifact. `systemd-run --user` yields
`cpuset cpu io memory pids`. This is the same admitted producer path `ci-hub validate-run` uses.

**Why one invocation per width rather than `--jobs 1..N`.** `_cmd_sweep` iterates
`range(lo, hi+1)` — linear, and strictly **ascending**. On a shared box whose load drifts, ascending
order aliases load drift onto the width axis and **manufactures an apparent regression at high
widths**. Every replicate here uses an independently shuffled width order, so drift cannot correlate
with width. Widths are geometric.

**Sampling.** n=3 per width (n=2 for the 16-CPU container run). Median is the primary statistic;
min, max and IQR are reported beside it. Failed samples are excluded from the derived table and
retained in `raw/`.

**Cache control.** The demonstration payload hashes one page-cached file, warmed before the first
sample — there is no cold/warm mode to draw from, which is deliberate: it removes the bimodal
confound entirely rather than trying to correct for it. The real hermit step measured
(`check.portability_paths`) is a source-scanning shell script with no build or fetch step.

**Plateau and regression, defined** (they are different points and only the second is actionable):

* **best** — width with the lowest median wall.
* **PLATEAU** — smallest width whose median is within 5% of best. Where you stop buying speed.
* **REGRESSION (confirmed)** — smallest width above best whose median exceeds best by >5% **and**
  whose [min,max] sample range is **disjoint** from best's. The disjointness test is what stops a
  noisy draw on a loaded box from being reported as a regression. A width failing only the
  percentage test is reported `suspected` and explicitly **not** actionable.

---

## Result 1 — the measured curves

### 1a. Capability demonstration, full 316-thread box

A fixed total of 576 sha256 units split into exactly *width* forked workers (forks scale with
width, not with unit count, so per-unit spawn cost cannot masquerade as a high-width regression).
Total work is constant across widths, so this is a genuine strong-scaling curve.

| width | n | median wall (s) | min | max | IQR | effective_cores | speedup (94.435s ÷ median) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 3 | 94.435 | 94.271 | 94.576 | 0.305 | 1.0 | 1.00× |
| 2 | 3 | 46.948 | 46.938 | 47.311 | 0.373 | 2.0 | 2.01× |
| 4 | 3 | 23.664 | 23.560 | 23.762 | 0.202 | 4.0 | 3.99× |
| 8 | 3 | 11.893 | 11.888 | 11.937 | 0.049 | 7.9 | 7.94× |
| 16 | 3 | 6.029 | 5.989 | 6.079 | 0.090 | 15.8 | 15.66× |
| 32 | 3 | 3.074 | 3.073 | 3.124 | 0.051 | 31.2 | 30.72× |
| 64 | 3 | 1.621 | 1.621 | 1.622 | 0.001 | 60.5 | 58.26× |
| 128 | 3 | 0.971 | 0.970 | 1.020 | 0.050 | 103.9 | 97.26× |
| 192 | 3 | 0.720 | 0.720 | 0.721 | 0.001 | 157.6 | 131.16× |
| 256 | 3 | 0.770 | 0.720 | 0.771 | 0.051 | 159.3 | 122.64× |
| **316** | 3 | **0.678** | 0.671 | 0.777 | 0.106 | **191.5** | **139.28×** ← best |
| 384 | 3 | 0.771 | 0.720 | 0.774 | 0.054 | 161.0 | 122.48× |
| 512 | 3 | 0.721 | 0.682 | 0.765 | 0.083 | 170.6 | 130.98× |

* **Plateau: width 192.** By the 5% rule the plateau lands on 316, but 192 (0.720 s, range
  0.720–0.721) and 316 (0.678 s, range 0.671–0.777) have **overlapping** ranges — they are not
  distinguishable, and everything from 192 to 512 sits inside ~0.1 s of noise. The defensible
  reading is **plateau ≈192**, and the 5% rule's answer of 316 is a tolerance artifact. Both are
  stated so the reader can pick.
* **Regression: NONE CONFIRMED up to width 512.** 384 is 13.7% slower than best but its range
  overlaps best's, so it is `suspected` only. On a box at load1 58–107 that is the correct verdict;
  do not act on it.
* Scaling is near-ideal to width 64 (58.26× on 64) and degrades thereafter: `effective_cores` tops
  out at 191.5, never approaching 316, which is the expected SMT2 throughput ceiling.

### 1b. Same workload, same physical machine, 16-CPU cpuset

`AllowedCPUs=0-15` (verified applied: `cpuset.cpus.effective = 0-15`, affinity 16).

| width | n | median wall (s) | min | max | effective_cores | speedup (94.081s ÷ median) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2 | 94.081 | 94.054 | 94.107 | 1.0 | 1.00× |
| 4 | 2 | 23.541 | 23.516 | 23.567 | 4.0 | 4.00× |
| **16** | 2 | **6.612** | 6.588 | 6.637 | **14.3** | **14.23×** ← best/plateau |
| 32 | 2 | 6.622 | 6.592 | 6.652 | 14.3 | 14.21× |
| 64 | 2 | 6.734 | 6.714 | 6.753 | 14.4 | 13.97× |

**Plateau moves 192 → 16 when the container changes.** This is the owner's verify criterion —
change the machine, observe the estimate change — and it is satisfied *by the measurement*. It is
**not** satisfied by the store key; see Defect 5.

### 1c. A real hermit DAG step

`check.portability_paths`, swept 1→256 on the full box:

| width | n | median wall (s) | min | max | effective_cores | speedup (1.821s ÷ median) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 3 | 1.821 | 1.721 | 1.934 | 0.9 | 1.00× |
| 4 | 3 | 1.721 | 1.721 | 1.823 | 1.0 | 1.06× |
| 16 | 3 | 1.671 | 1.670 | 1.673 | 1.0 | 1.09× |
| 64 | 3 | 1.671 | 1.671 | 1.771 | 1.0 | 1.09× |
| 256 | 3 | 1.721 | 1.721 | 1.771 | 1.0 | 1.06× |

**`effective_cores` is pinned at 1.0 across a 256× width range.** The step is single-threaded; the
width is meaningless to it. Max speedup 1.09× is inside the noise. This is **flat by construction,
not a measured plateau**, and `effective_cores` is the discriminator that tells them apart — which
the runner already records. Encouragingly, the estimator gets this right on its own: it returns
`recommended_inner_jobs=1`, because 1.09× never clears its 1.15× marginal-gain bar.

---

## Result 2 — five defects, each with executed evidence

### Defect 1 — the plateau estimator has never fired. Starved of data, not broken.

`estimates.load_step_speedups()` over the **real accumulated store**
(`hermit/.safe-ci-dag-runner/profiles/`, 129 rows, 52 steps) returns **0 curves**.
`_SPEEDUP_MIN_LEVELS = 2` needs ≥2 distinct `inner_jobs` per step; measured over that store,
**52/52 steps have exactly one width** (50 at 316, `build.workspace` and `build.runtime_release` at
32). Positive control: the same function on this experiment's swept store returns a 13-level curve
and `recommended_inner_jobs=192`. The code is live and correct; CI simply never varies a width, so
nothing ever reaches it.

### Defect 2 — the runner's width knob cannot reach a hermit step's parallelism.

`portable.json` sets `default_jobs_flag: null`; 47/48 steps inherit that null or set `jobs_flag: ""`
explicitly; only 2 declare `hint.preferred_inner_jobs`. So `command_with_inner_jobs` appends nothing
to any hermit command. Meanwhile **19 of 48 portable steps hardcode their parallelism inside the
`cmd` string**: 12 carry `--test-threads=N` (11 of them `=1`) and 7 carry `CARGO_BUILD_JOBS=8`.

Consequence: a sweep varies only `effective_cpu_count` → the step's cgroup `cpu.max`. A hermit sweep
therefore measures **wall vs CPU-core budget**, not wall vs thread count. For a step pinned at
`--test-threads=1` the thread count cannot move at all, so no plateau it derives could ever become
that step's max-threads. **The plateau cannot become the setting until the setting is reachable** —
i.e. until those literals are replaced by the `jobs_flag` mechanism the runner already implements.
The previous study (`validate-per-step-thread-caps_20260807`) worked around this by rewriting the
DAG file per point — an external harness, exactly what the owner ruled out.

### Defect 3 — plateau and cliff are indistinguishable in the estimator's output.

Two synthetic curves fed to the real estimator — one plateauing (40/21/11/10.6/10.5 s) and one
regressing hard (40/21/11/10.6/17.0/26.0 s) — **both return `recommended_inner_jobs=4`.**
`StepSpeedup` has no field separating "flat above the knee" from "2.4× slower at J=32".

**Correcting the task's stated worry:** the brief says a plateau-derived max-threads "will sit past
the point where throughput degrades." **That is not true of this implementation.** The walk is
greedy and stops at the first level failing the marginal-gain test, so the recommendation can never
advance past a degradation — the regressor above still got 4, not 32. The defect is **reporting**,
not safety. Nothing tells an operator that a step falls off a cliff just beyond its recommendation.

### Defect 4 — the estimator ingests FAILED samples as if they were measurements.

`bucketize_rows` / `sample_from_row` never inspect `ok`, `returncode`, `timed_out`, `cpu_timed_out`,
or `oom_kills` (verified against source). Demonstrated live: `check.script_sigpipe` (rc=2, 15/15
samples failed) and `check.skill_discovery` (rc=127, 15/15 failed) both produced full curves and a
`recommended_inner_jobs`, fitted to **crash-fast times of 0.006–0.011 s**. A step that fails fast at
every width is indistinguishable from a perfectly flat, extremely fast one.

This bit this very experiment first: the initial sweep set every width to `cpu_timed_out=True`
against the 10 CPU-second default budget (`DEFAULT_SMALL_CPU_TIMEOUT`), and those truncation times —
11.1 s at width 1, 0.57 s at width 32 — form a beautiful, entirely fictitious scaling curve. They
are retained in `raw/demo-store/` as the negative control. The runner *records* `ok=False` and
`cpu_timed_out=True` faithfully; only the estimator ignores them.

### Defect 5 — the machine/container store key collides, silently, under boxing.

`container_class()` called directly inside a 16-CPU unit correctly returns
`affinity16_cpu-max-unknown`. But on the **sweep path with cgroup boxing active**, rows measured in
that same 16-CPU container record `affinity316_cpu-max-unknown` — while their walls prove the work
really was confined (`effective_cores` 14.3 at widths 16/32/64; 6.6 s floor vs 0.68 s on the full
box). Separately, the quota half of the key is always `cpu-max-unknown` because it reads the **root**
`/sys/fs/cgroup/cpu.max` rather than the process's own cgroup.

Consequence, measured by merging the two datasets exactly as a shared store would:

| store contents | `recommended_inner_jobs` |
| --- | ---: |
| 316-CPU box only | 192 |
| 16-CPU container only | **16** |
| both, collided into one bucket | **192** |

**The estimator recommends 192 threads for a 16-CPU container** — a 12× over-allocation, produced
silently. This defeats the "per-machine" requirement at the storage layer even though the
measurement layer gets it right.

---

## What would make the owner's capability real

In dependency order. 1 and 2 are prerequisites; the estimator is otherwise ready.

1. **Make the width reachable** (Defect 2). Replace hardcoded `--test-threads=N` / `CARGO_BUILD_JOBS=8`
   in `ci/dag/portable.json` with the runner's `jobs_flag` mechanism (e.g.
   `jobs_flag: "-- --test-threads=%d"`), so the runner can set what it measures. Until then no
   derived plateau can become a setting.
2. **Fix the store key** (Defect 5): compute `container_class` from the process's own cgroup, and
   recompute it on the boxed path. Until then, samples from different containers merge.
3. **Reject non-measurements** (Defect 4): filter `ok != True` in the estimator's ingest, or carry a
   validity flag into `Sample`.
4. **Report the regression** (Defect 3): add a regression point to `StepSpeedup` — the smallest
   width above best that is slower by more than dispersion — and surface it. The recommendation is
   already safe; operators just cannot see the cliff.
5. **Fix `analyze.py`** — it keys on `duration_s`; the store writes `elapsed_s`. Executed against
   the real store it drops **100% of rows** for all 52 steps.
6. **Then populate**: a periodic sweep at a few geometric widths per step is enough to cross
   `_SPEEDUP_MIN_LEVELS`, after which the estimator produces per-machine plateaus continuously.

---

## Limitations — read before quoting any number

* **Every sample is `ambient_bucket=busy`** (load1 58–107 on a 316-thread shared box, ~14 other
  agents). Randomized width order and n=3 defend against drift *aliasing onto the width axis*; they
  do not make this an idle-box measurement. Absolute walls would be lower on a quiet host.
* **The estimator's walls are not the measured walls.** It applies a contention discount: at width 1
  it reports 71.278 s where the measured median is **94.435 s** — a 25% adjustment. That is a
  *model* on top of the measurement. Every wall in the tables above is the raw measured median; only
  the `recommended_inner_jobs` values come from the discounted path.
* **1a/1b are a synthetic CPU-bound payload, not a hermit step.** They demonstrate the capability
  and characterize *this machine*; they are not a hermit workload result.
* **Only one real hermit step was swept.** `check.script_sigpipe` (rc=2) and `check.skill_discovery`
  (rc=127, `rustc` absent from the transient unit's PATH) failed at every width — an environment
  limitation of this harness, not a product defect. The 12 `--test-threads` steps and 7 build steps
  were not swept: they need a full workspace build, and per Defect 2 their thread counts would not
  have moved anyway.
* **The privileged lane was not measured** (unchanged from the prior study).
* **No confirmed regression was found anywhere in this dataset.** The honest answer to "where does it
  regress" is: not within 512 widths on this box for CPU-bound work, and the two suspected points
  fail the dispersion test. A dataset from a quiet host could resolve them.
* n=2 for the 16-CPU container run; n=3 elsewhere.

## Reproducing

```bash
export RUNNER=.../agent-utils/py/bin/safe-ci-dag-runner CHECKOUT=$PWD \
       DAG=$PWD/runners/capability-demo.dag.json STORE=$PWD/raw/demo-store-v3 \
       STEP=demo.fixed_work_parallel WIDTHS="1 2 4 8 16 32 64 128 192 256 316 384 512" \
       REPS=3 TOTAL=576
bash runners/sweep-driver.sh
python3 runners/analyze-curve.py raw/demo-store-v3 --csv results.csv
```
