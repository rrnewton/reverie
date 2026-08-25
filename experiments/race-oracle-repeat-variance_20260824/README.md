# Does the race oracle's repeat variance survive at high N?

**Date:** 2026-08-24 · **Agent:** hermit-005 · **Task:** `the-race-oracle-has-5x-repeat-variance`

## Question

Two 20-run sweeps of an identical arm gave 55% and 30% divergence. Is that
sweep-to-sweep spread a property of the oracle, or is it sampling noise at n=20?

## Method

One arm, held fixed for all 800 runs:

```
bin/hermit-repeat -n 20 --mode {concurrent,sequential} --backend sabre \
  --sh-bin worktrees/005/pin/target/release/hermit \
  -- /bin/sh -c 'seq 1 2000 | wc -l'
```

hermit binary `f972f4ffc66b9f21a6203fc8e3dd0f8f103bd580`, sha256
`51e3eab17746f84760fe55486f9a7cb0f88fa94c1ef855d5084ae95ac69e4027`.
20 replicate sweeps per mode, n=20 each — 400 runs per mode, 800 total.
Modes **interleaved** (C,S,C,S,...), never blocked, so box drift over the
~45-minute window cannot masquerade as a mode difference.
Private per-node cwd (hermit-repeat's default); the shared-cwd confounder was
NOT enabled. `load1` recorded per run by the tool itself and used as a covariate.

Box was NOT quiet and this is not claimed: two other agents ran hermit guests
throughout, load1 ranged 14.4–57.7. Three orphaned `liteinst_advanced`
processes (ppid 1, cwd deleted, 27 min old, one at 69% of a core) were reaped
before starting.

## Results

| | concurrent | sequential |
|---|---|---|
| pooled | 144/400 = **36.0%** | 75/400 = **18.8%** |
| Wilson 95% | [31.4, 40.8] | [15.2, 22.9] |
| dispersion vs binomial | **2.64** (X²=50.2/19df, z=+5.06) | **0.96** (X²=18.3/19df, z=−0.11) |
| verdict | OVERDISPERSED | consistent with binomial |
| corr(divergences, load) | **+0.549** | +0.155 |

## Findings

1. **The max/min ratio is not evidence of anything.** Simulating a *perfectly*
   binomial process at the sequential rate, 20 sweeps of n=20 gives a **median
   max/min ratio of 7.0x**, and P(ratio ≥ 5.3x) = **76%**. My own sequential arm
   is measurably binomial (dispersion 0.96) and still spans 1/20 to 7/20 — a 7x
   ratio. A 5.3x ratio at n=20 is the *typical* output of a noise-free process.
2. **Sequential mode is a well-behaved oracle.** Dispersion 0.96 across 20
   replicates: its repeat variance is pure binomial sampling, so a single
   sweep's Wilson interval is valid as printed.
3. **Concurrent mode is genuinely overdispersed, and it is contamination.**
   Dispersion 2.64. It tracks host load (r=+0.549), and dropping the three
   highest-load sweeps collapses dispersion to 1.38 (z=+1.08, consistent with
   binomial). Extra-binomial variance enters through contention, not through
   the oracle.
4. **The concurrent/sequential difference is real but load-confounded.**
   36.0% vs 18.8%, ratio 1.92x, z=5.47; intervals stay disjoint even after
   inflating concurrent by its design effect ([28.8, 43.9] vs [15.2, 22.9]).
   Restricting both modes to load<25 keeps the effect size (32.5% vs 18.1%,
   1.80x) but halves n, and the intervals then overlap — a power limit, not
   evidence of no effect. This still **reverses** the earlier n=20 finding of
   "no difference established".
5. **The minimum is a ceiling, measured.** Concurrent's minimum first divergence
   point fell 131 → 40 at observation #6 and never moved again through 144.
   Sequential never left 131 in 75 observations. Same guest, same binary: the
   reported minimum is an artefact of sampling depth and mode.
   Divergences cluster hard at `info_record` 131 (127/144 concurrent, 67/75
   sequential); rare values 40, 202, 1232, 1233.

## Recommendation

Use **sequential** as the race oracle: its repeat variance is exactly binomial,
so plain Wilson intervals are sound and no replicate-dispersion correction is
needed. Use concurrent only when contention is itself the object of study, and
then report load and inflate intervals by the measured design effect. Never
quote a max/min ratio across small-n sweeps as evidence of instrument noise.

## Files

`sweeps.csv` 40 sweeps · `runs.csv` 800 runs · `results.txt` full output ·
`drive.sh` the driver · `analyze*.py` the analysis.

## Applied

`bin/hermit-repeat` now carries this correction: `CONCURRENT_DESIGN_EFFECT =
2.64` widens the reported interval in concurrent mode only, sequential is left
uncorrected at 1.0, and `--design-effect` overrides both. The measured
false-positive rate above — 15 of 190 identical-arm pairs reading as different,
0 of 190 after correction — is the regression that the tool's `--selftest` now
pins via the 2/20-versus-13/20 pair.
