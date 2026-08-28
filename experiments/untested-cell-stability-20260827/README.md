# Stability of never-measured compatibility cells under repetition — batch 1

Status: **one batch of a planned four.** Batch 1 complete; batches 2–4 not run.
Author: agent(coord-helper), 2026-08-27. Written at the end of the author's
context, so read the "What is missing" section before relying on this.

---

## ⚠️ Read this before quoting any number from this document

**72 cells that the scorecard calls RED passed 10 out of 10. That is not 72
things being fixed, and it is not 72 flakes being caught.**

In this scorecard, **red means the cell is NOT SELECTED into the regression
plan** — not that it was run and failed. At the study commit, **362 of the 371
red cells had never been measured at all** (`measurement: never-measured`). So a
red cell passing ten times has not turned green and was never failing. **The
study found out, for the first time, what those cells do.**

Quoted without that sentence, "72 red cells passed" reads as a 72-cell
improvement that never happened. Nothing in this study fixed anything.

---

## 1. Method

One batch: a seeded random draw of 85 cells from the red population, each run 10
times, under a 30-second per-repetition cap.

    ./ci/compat-envelope/pressure-test.rs run \
      --results ignored/compat-envelope/red-batch1-seed1 \
      --sample 85 --seed 1 --repetitions 10 --cell-timeout 30

### Provenance

| what | value |
|---|---|
| hermit commit under test | `ae137114fdf1a670e4a31f74bdfc78a675037deb` |
| branch | `coord-helper/pressure-test-batch-repetitions` |
| base | hermit main `b7f970089726` |
| pull request | https://github.com/rrnewton/hermit/pull/2798 |
| detcore tree | `c6423f8e4c6f8190c6cfaa3687cf9916d22e5a0a` |
| population source | `ci/compat-envelope/cells.json` at `7ddb3559a09f2709a67ce2b605426b43c33f070f` |
| scheduler | `-j 16`, 30s per repetition, 7200s whole-run wall bound |
| box | quiet; both validation slots free at launch |

The tool did not exist in this form before the study: repeating a BATCH of red
cells was refused, so covering a population meant one invocation per cell. The
flag that makes this study expressible — and repeatable by anyone else — is the
pull request above. Results here were produced from the branch, not from main.

### Two bounds, both verified before launching, both fired

* inner, per repetition: `--cell-timeout 30`. **Fired 12 times.**
* outer, whole run: cgroup `RuntimeMaxSec=7920s`, read back from the live unit.
  Did not fire. Its job is to catch a livelock that the inner bound missed, so
  its firing would mean the inner bound failed.

---

## 2. Population — three different numbers are in circulation and they are NOT
   the same set

| figure | what it actually counts | value |
|---|---|---:|
| red cells, all modes | what `pressure-test` draws from | **371** |
| red AND `verify` | quoted elsewhere as "336" | **336** |
| red AND never-measured | quoted elsewhere as "362" | **362** |
| enabled AND never-measured | the population the task named | **363** |
| **eligible to run** | red cells with an executable manifest command | **338** |

**336 and 362 are not nested** — nine of the 336 red verify cells have been
measured. **363 and 371 are not nested either** — 363 includes one green cell and
excludes nine measured red ones.

**What this study sampled is the 338**, and 85 of them were drawn. The 33 red
cells excluded from the 338 are the red `naked` cells, whose manifests provide no
guest command. The tool's own message attributes exclusion to "a red chaos cell
whose manifest declares no seeds", which describes only 2 cells — so the message
is incomplete, though the arithmetic (371 − 338 = 33 = the naked count) closes.

Membership files in this directory, so every count can be checked rather than
trusted:

* `population-red-371.tsv` — every red cell at the study commit
* `population-enabled-never-measured-363.tsv` — the population the task named
* `batch1-seed1-selected-76.tsv` — **MISNAMED AND WRONG, see section 6**

---

## 3. Results — batch 1, 85 cells, 850 (cell, repetition) results

873 attempts collapsed to 850 distinct results; the extra 23 are retries
(section 5).

| result | count | share |
|---|---:|---:|
| pass | **755** | 88.8% |
| infrastructure-error | **42** | 4.9% |
| crash-error | **35** | 4.1% |
| determinism-failure | **18** | 2.1% |

**The tool's own verdict on the batch is `repeated_result: incomplete`**, because
42 runs produced no trustworthy result. Under the owner's taxonomy this batch is
a PARTIAL ANSWER, not a red. The denominator for any compatibility claim is 808,
not 850.

All 12 timeouts are `timeout_proven_by_runner_and_attempt_marker` — identified by
the runner, not by matching log text — and all 12 are classed
`infrastructure-error`, not product failures. No OOM kills.

### Per-cell stability (85 cells, final attempt of each repetition)

| | cells |
|---|---:|
| uniform **pass** 10/10 | **74** |
| uniform infrastructure-error | 4 |
| uniform determinism-failure | 1 |
| uniform crash-error | 1 |
| **varied across repetitions** | **5** |

---

## 4. ⚠️ The five varied cells — and why they must not be averaged into "flaky"

    c-programs/ppoll-simulation              verify/sabre   crash 9,  determinism-failure 1
    c-programs/printf-with-threads           verify/sabre   crash 9,  determinism-failure 1
    c-programs/threadexhaustion              verify/sabre   crash 7,  determinism-failure 3
    determinism-stress-c/pipe-chain          verify/sabre   pass 7,   determinism-failure 3
    language-runtimes/example-python-random  verify/dbt     pass 8,   infrastructure-error 2

**These are three different phenomena, and collapsing them loses the only
structural information ten repetitions bought.**

1. **The first three never pass.** They vary only in HOW they fail — crash versus
   determinism-failure. That is an unstable CLASSIFICATION of a consistent
   failure, not a flake. A single run of any of them would report a failure; ten
   runs report a failure whose *kind* is not reproducible.
2. **`determinism-stress-c/pipe-chain` sometimes passes and sometimes produces a
   determinism failure.** This is a genuine flake: a nondeterministic WRONG
   ANSWER. It is the one cell here that a single run would misreport in either
   direction.
3. **`language-runtimes/example-python-random` sometimes passes and otherwise
   produces an infrastructure error.** This is NOT the same defect. Its
   non-passes are harness faults, not wrong answers, and they belong to a
   different owner. A cell that sometimes hangs or errors in the harness and a
   cell that sometimes computes the wrong thing are different problems that look
   identical in any "N% flaky" summary.

**Four of the five varied cells are `sabre`.** Whether that is a backend property
or an artifact of this draw is NOT established here — one batch of 85 cannot
distinguish them.

### The single deterministic hang

One cell is uniform `infrastructure-error` across all ten repetitions with the
runner-proven timeout marker: it **never completes**. Under the owner's taxonomy
that is a **no-result, not a red** — there is no compatibility verdict for it in
either direction, and it has been sitting in the red population unmeasured. It is
not a flake and it is not a test failure.

---

## 5. ⚠️ The 23 retries carry no classification

Twenty-three (cell, repetition) pairs needed more than one attempt. Every one:

    result = determinism-failure    error_kind = None    23 of 23

**Not one of the 23 is attributed to an understood infrastructure failure, a
prerequisite failure, or a genuine no-result.** Twenty-three retry events
produced zero classification, which is precisely the signal the taxonomy work
exists to generate. This is a small, countable sample of that gap.

What the retries achieved:

| first attempt | final | count |
|---|---|---:|
| determinism-failure | → determinism-failure | 16 |
| determinism-failure | → crash-error | **4** |
| determinism-failure | → pass | **3** |

**The four that changed class on retry are a third kind of instability**: the same
repetition produced two different failure kinds on two runs. Not "sometimes
passes", not "always fails" — *fails differently each time*.

All 23 retries fall in only 5 cells. `backend-parity-c/timer-family-identity
verify/ptrace` retried **all ten** of its repetitions.

**Importantly: none of the 74 passing cells needed a retry.** All 74 are clean
first-attempt 10/10. The retry mechanism did not manufacture any of the green.

One consequence: `c-programs/nanosleep-threads-nocrash verify/sabre` had a retry
but is uniform on FINAL attempts, so it is stable by the final-attempt view and
not by the first-attempt view. Both views are reported because they differ.

---

## 6. ⚠️ What is wrong or missing in this directory

* **`batch1-seed1-selected-76.tsv` IS NOT THIS STUDY'S BATCH.** It records a
  first launch that selected 76 **green** cells because of a defect in the flag
  being added (a predicate named for green that tested only "repeats"). That run
  was killed and its results discarded. The file is kept, misnamed, rather than
  deleted silently — but do not read it as batch 1's membership. **Batch 1's real
  membership is `ignored/compat-envelope/red-batch1-seed1/run.json`,** which
  records the seed and all 85 identities.
* **A per-cell TSV of batch 1's 85 selected cells was never written.** Recover it
  from `run.json`.
* **Batches 2–4 were not run.** The population is 338 and this batch covered 85.
* **The remaining coverage plan:** batches 2–4 must be driven from the explicit
  COMPLEMENT using `--cells-from` (added in the same pull request), NOT by
  resampling. Four random draws of 85 would overlap and miss roughly 98 cells
  while appearing to cover the population. Batch 1 was a random draw and batches
  2–4 will not be, which is a real asymmetry in method: batch 1's cells had a
  different selection probability from the rest, and any result correlating with
  selection order must be read with that in mind.
* **Cells the study never reached must be named** in the final document, by
  differencing the population file against the union of batch memberships.

### Configuration caveat — this is not the configuration the gate runs

`pressure-test` runs **host / release / quiet**: it hard-wires
`HERMIT_BIN="$PWD/target/release/hermit"` for an exact cell and contains no
reference to the pinned root. Validate runs **pinned-root / debug / contended**.

**So these results describe a configuration the gate never executes.** This is
not theoretical: three cells that failed twice each under validate at one commit
passed 20/20 under `pressure-test` at the same commit earlier the same day. A
number from this study is not a prediction of what validate will do.

### Not established

* What the 33 excluded red `naked` cells are, and whether they are disabled by
  design or broken. Not investigated.
* Whether the sabre concentration among varied cells is real.
* Whether the 42 infrastructure errors share one cause.
