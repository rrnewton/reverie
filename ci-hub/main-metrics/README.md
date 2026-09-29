# main-metrics: validate WALL as a standing main-branch series

`ci/wall-budget-600s` asks *"was this run slow?"*. It catches a slow run and is
blind to the question the owner actually asked: **did a change make everything
slower?** A migration-shaped regression arrives with a commit and is invisible
without a series to compare against — which is exactly why *"did the DAG
migration make validate slower"* could not be answered.

This is the other half, and the sibling of `ci-hub/greentime`. Green-time and
wall-time are the two standing health numbers.

```
python3 ci-hub/main-metrics/wall_series.py                 # main-only, full-profile passes
python3 ci-hub/main-metrics/wall_series.py --format json
python3 ci-hub/main-metrics/wall_series.py --all-commits   # include PR heads
python3 ci-hub/main-metrics/wall_series.py --fail-over-budget
```

## What it measures, and what it refuses to

Per main commit: **wall** (primary), **CPU seconds**, **CPU/wall ratio**,
**concurrency at run time**, **per-gate wall**, and **peak memory** — reported as
`null`, because nothing produces it (see below).

Two refusals are deliberate:

- **Ancestry, not a branch label.** "Is this commit on main" is answered by
  `git merge-base --is-ancestor` at read time, not by a label the row recorded
  when it ran. A row produced on a branch that later landed *is* main history;
  a label written at run time cannot know that. Each row is checked against its
  own repository: legacy/missing and `hermit` repo values use
  `rrnewton/hermit`, while `reverie` values use `rrnewton/reverie`. An unknown
  repository, unavailable object, or non-commit object refuses the whole
  population; the refusal names the repository with every object ID.
  One exception to "unavailable refuses": a commit this checkout lacks
  and that GitHub itself reports as nonexistent ("No commit found for SHA",
  HTTP 404 or 422, for that exact SHA in that repository) cannot be on
  main. Its rows are excluded, counted, and named in the output
  (`absent_upstream_rows`, `absent_upstream_shas`). Anything short of that
  answer still refuses: a timeout, a transport, authentication or rate-limit
  failure, a 5xx, a repository-level "Not Found", HTTP 200 (the commit exists
  and just needs fetching), or no `FLEET_REAL_GH` route. Only the affirmative
  answers are cached, for 7 days, in
  `$DEV_HERMIT_PARENT/ignored/ci-hub/main-metrics/github-absent-commits.json`.
  Lookups stop at the first failure and use at most 10 s per run; nine cold
  lookups took 3.22 s on devbig014 and 0 s once cached. That does not keep the
  gate within tick-hub's 30 s kill: loading the ledger alone measured 124 s on
  a busy host. See https://github.com/rrnewton/dev-hermit/issues/400.
- **`INSUFFICIENT` instead of a guess.** A wall time without its concurrency is
  uninterpretable. Median wall measured **490s at 0–3 concurrent validates and
  852s at 14+**, so comparing two unconditioned samples can manufacture a 74%
  "regression" out of scheduling alone. `compare()` returns `INSUFFICIENT` unless
  both sides carry concurrency. `require_conditioned=False` overrides it
  explicitly, never silently.

**CPU/wall separates the two causes.** Wall up with the ratio *down* is
CONTENTION — we waited. Wall up with the ratio *held or risen* is MORE WORK — we
did more. Reporting wall alone cannot distinguish them, so the ratio travels with
the verdict and is named in `cause`.

## Measured state of the series, 2026-08-07

All counts carry their denominator.

| quantity | value |
| --- | --- |
| ledger rows | 691 |
| rows on main (ancestry-checked) | 132 / 691 (19.1%) |
| main **full-profile pass** points | 26 |
| distinct main commits with a point | **15** |
| span | 2026-08-03 → 2026-08-07 (4.38 days) |
| wall median / min / max | 659s / 396s / 995s (n=26) |
| **over the 600s budget** | **15 / 26** |
| CPU/wall ratio median (main full-pass) | 7.2× |
| **conditioned (carry concurrency)** | **4 / 26** |
| peak memory | **absent — no producer** |

Two of those numbers are the finding:

1. **The comparable series is 4 points, not 26.** `concurrent_validates` is
   present on 210/691 rows overall (30.4%) and on only 4 of the 26 main points.
   The UNKNOWN bucket is the majority, not a corner case.
2. **The median on main already exceeds the budget** (659s vs 600s), with 15 of
   26 points over. That is a standing condition, not a spike.

Note the ratio differs by population: 2.77× across all 691 rows, 7.2× across main
full-pass rows. Main points are the full DAG lane and run far more parallel. A
ratio quoted without its population is not comparable — which is the same defect
this module exists to prevent.

## Retention: how long must the series be kept?

**Two days demonstrably cannot answer the question, and neither can four.**

Derivation, not assertion. At the current rate the series accumulates ~15 distinct
main commits and **~4 conditioned points per 4.38 days ≈ 1 conditioned point per
day**. A median comparison across a change needs a usable sample on both sides;
at 20 points per side that is **~40 days of retention at today's conditioning
rate** — and the ledger is a local file with a 4.38-day history.

So retention has two independent requirements, and raising only one does not help:

- **Keep ≥ 90 days** of ledger rows, so a change three weeks old still has a
  before-side. Ninety days gives ~90 conditioned points at today's rate, enough
  for a seasonal baseline rather than a two-sample guess.
- **Raise the conditioning rate toward 100%.** Retention alone is useless if 85%
  of retained points are unconditioned; 90 days of unqualified walls is still a
  refusal. `concurrent_validates` must become mandatory on every row, not
  best-effort on 30%.

## Known gaps

- **Peak memory has no producer.** No `rss`/`mem`/`peak` key exists among the 51
  distinct keys observed across 691 rows. It is reported as `null` so the absence
  stays visible rather than being quietly dropped from the metric set.
- **Per-gate wall needed no producer** — contrary to the original task text,
  `gates[].real_seconds` is already present on 690/691 rows. This was a missing
  *view*, and that view now exists (`gates_top`).
- **Not wired to tick-hub.** The owner's phase-2 direction is that tick-hub calls
  this automatically so it stays measured. `--fail-over-budget` gives the exit
  code an alarm needs; nothing calls it yet.
- **No per-commit alarm at land time.** `compare()` is the primitive; binding it
  to "fires at the commit that caused it" requires the tick-hub wiring above.
- **Rebased commits are still matched only by object ID.** A patch can land
  under a different commit ID after rebase. Patch-identity matching is a
  separate owner decision; this repository-routing correction does not infer
  ancestry from equivalent content.
