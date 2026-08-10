# Hermit CI runner / status tooling

On-demand visibility into the Hermit repos' GitHub Actions state, sized to
Hermit's reality: Hermit does **not** run a dynamic cloud fleet - it uses a
small, fixed set of host-local self-hosted runners. This directory provides the
non-mutating status reporter and the scripts/images used to manage those fixed
runners.

> Scope note: Dynamic fleet provisioning and a CI shepherd/reconciler are out of
> scope. Hermit's test runners are host-local PMU machines (they need `perf` RCB
> counters), so cloud-fleet scale-out does not apply.

## Runner identities

`hermit-gate-newton` is the dedicated `rrnewton/hermit` self-hosted GitHub
Actions runner registered with the `gate` label and **without** `pmu`. It runs
the control-plane-only Merge Gate jobs from the slim image defined by
[`Containerfile.gate`](Containerfile.gate), keeping those jobs off the PMU
build/test queue; it is not a test runner or a separate CI service. The name is
GitHub's runner registration identity, while `gate` is the workflow scheduling
label. Use `./ci-status.py` for current online/busy state rather than inferring
state from the name.

## Quick start

```sh
cd ~/work/dev-hermit/ci-hub/runners
./ci-status.py                 # rrnewton/hermit (default)
./ci-status.py --all           # all three Hermit repos
./ci-status.py --repo rrnewton/reverie --limit 60
```

`gh` is invoked through `$GH` (default `with-proxy gh`) so it works behind the
devserver proxy without changing the machine-global `gh` account. Override with
`GH='gh' ./ci-status.py` if you are already off-proxy/authed.

The report shows, per repo (analysis lives in the importable `queue_health.py`;
`ci-status.py` is the stable entrypoint that renders it):

- **runner health — CONFIGURED vs LIVE** — the configured runner count next to
  the live (online) count, so a **registered-but-offline runner shows up as
  silently-dead capacity** instead of vanishing; plus per-runner
  status/busy/labels, a `pmu` / `pmu-serial` breakdown, and a flag when there is
  no idle runner.
- **named in-flight runs** — each queued/running run by id, workflow, branch, and
  title ("run X on branch Y"), not one opaque "something is running" count.
- **queue depth, per workflow** — queued vs running counts split out for each
  workflow (not one aggregate number), plus the **current queue age**
  (`now − createdAt`) median/max, an honest *lower bound* on each run's final
  wait.
- **capacity signal** — observed missing/offline capacity or a fully busy
  self-hosted pool, reported alongside queued depth. The cheap run-list query
  does **not** expose queued-job labels, so the signal explicitly refuses to
  name `pmu`, `gate`, or `pmu-serial` as binding. A runner label is inventory;
  it is not evidence that a queued workflow selected that label.
- **time since last green, per workflow** — both the elapsed wall time and **how
  many runs back** the last success was, so a green buried pages deep surfaces
  here instead of forcing a manual page-through.
- **time-in-queue vs run duration** — a historical distribution from the jobs
  API over a bounded sample of recent completed runs, keeping wait
  (`job.started_at − job.created_at`, the runner-pickup latency) **strictly
  separate** from run duration. Run-level `startedAt` equals `createdAt`, so it
  cannot supply this; the jobs API is the only faithful source. Control the
  sample with `--sample N` (`0` disables it; the current-queue-age above is the
  free lower-bound signal).
- **last-window run aggregates** — over a fixed window (`--window-hours`, default
  24) the counts of runs started / completed / success / failure / cancelled,
  with **merge-gate runs counted SEPARATELY** (a `Merge Gate` workflow or a
  `merge_group` event) so high-frequency gate churn never swamps the real
  test-failure count. A short run list that does not span the window prints a
  **COVERAGE WARNING** and the counts are labelled a lower bound.
- **self-hosted utilization + peak concurrency** — the owner's "can we delete a
  runner?" signal. **Utilization** is busy-runner-time as a *percentage of
  capacity* (`self-hosted busy-job-seconds ÷ (runner count × window)`), and
  **peak** is the maximum observed concurrent self-hosted jobs vs the runner
  count, both over the same fixed window. Only jobs whose runner is a known
  self-hosted runner contribute, so GitHub-hosted work never inflates them. Both
  derive from the bounded jobs-API sample over a *fixed* window, which makes the
  error one-directional: a truncated sample can only omit busy time / overlaps,
  so each figure is a strict **LOWER BOUND** that prints the **direction of its
  own error** ("TRUE utilization is ≥ this", "TRUE peak is ≥ this") and its
  sampling basis — never a bare percentage. Raise `--sample` for a tighter bound.

### Ops-tick integration

`queue_health` is wired into the operational tick as the `ci_queue_health`
reminder (`ci-hub/health/tick-hub.yaml`, cadence 900s → gate
`operational_health.py queue-health`). The gate is **fail-loud**: it returns
non-zero (hard-warning the coordinator) when any workflow's queued depth reaches
`CI_QUEUE_DEPTH_WARN` (10), the max current queue age reaches
`CI_QUEUE_AGE_WARN_SECS` (1800), a workflow with at least
`CI_GREEN_GATE_MIN_RUNS` (4) runs in the window has no green within
`CI_GREEN_RUNS_BACK_WARN` (15) runs / `CI_GREEN_AGE_WARN_SECS` (6h), or the
self-hosted capacity signal is active. All thresholds are
environment-overridable; `CI_GREEN_GATE_EXCLUDE` (comma-separated substrings,
default empty) silences the no-green gate for a structurally non-green workflow
without a code change. The gate does no jobs-API sampling, so a tick stays cheap.

Standalone: `./ci-status.py --gate` (or `--gate --all`) emits the same
`key=value` fields.

### Relationship to prior "CI stats" tooling

A sibling project surfaced comparable self-hosted + GitHub CI reporting through a
Makefile `stats` target. That target was **not** ported here — `runners/Makefile`
carries only the single-runner lifecycle targets (`build`/`init`/`start`/
`verify`/`stop`/`drain`/`status`). What existed before this change was a partial
reimplementation (`ci-status.py`: runner counts + one aggregate in-flight number
+ last-green), off the tick and missing per-workflow depth, wait-time
distribution, runs-back, and the eligibility-honest capacity signal. This directory now
ports the **capability** (all of the items above) into ci-hub and wires the cheap
core to the tick, rather than porting the target.

The reference `stats` output's defining discipline was that it stated the
direction of its own error — "job sample truncated at N jobs — utilization is a
LOWER BOUND", "merge_group run sample reached 100; count may be low". The
utilization, peak-concurrency, and last-window figures here follow that rule
exactly: every one prints its sampling basis and, when the sample or run list is
truncated, whether the true value is higher or lower. A number that knows its
limits beats one that only looks authoritative.

Data-source reuse (no third accumulator): live snapshots come from the `gh` API
directly, exactly as the existing per-workflow/queue/runner sections already did;
the durable multi-day history lives in the ci-hub history store
(`ci-hub/history/query.py runs` / `green-time`, over `ignored/ci-hub/gha-runs.csv`)
and open-PR health in `ci-hub/health/pr_status.py`. Use `--window-hours` here for
same-session capacity questions; use the history store for windows longer than the
live run list reaches.

## The Hermit CI situation (2026-07-24)

This is a **capacity** problem, not a broken-tests problem.

- The `pmu` label is required because the determinism suite reads hardware
  retired-branch counters, so these jobs **cannot fall back to GitHub-hosted
  runners**. (Update — basis: `gh api .../actions/runners`, 2026-08-03:
  `rrnewton/hermit` now has **three** PMU runners `hermit-ci-newton{,-2,-3}` plus
  a gate-only [`hermit-gate-newton`](#runner-identities); exactly one,
  `hermit-ci-newton`, carries the residual `pmu-serial` inventory label. Current
  workflows do not select that label, so it must not be described as a binding
  lane. `runner-health` reports the live count, so prefer it over this prose.)
- **reverie** drains fine: its Rust job runs ~2–3 min, so one runner stays idle
  and every push goes green quickly.
- **hermit** is jammed: the Rust ("Regular tests") job is much heavier and PR/push
  volume is high, so a single runner cannot keep up. Result: dozens of queued Rust
  runs (observed 23–59 in-flight), frequent supersession cancellations, and **zero
  green Rust runs**. The GitHub-hosted **Docs** workflow stays green and is the
  practical hosted gate.
- **facebookexperimental/hermit** (the fbcode-sync mirror) has a high Rust failure
  rate (~50% of recent runs) from fbcode/folly-fmt sync breakage — a separate
  issue from the rrnewton runner bottleneck.

### Landing discipline

Because self-hosted Rust CI cannot reliably go green here, changes land under the
**post-facto-review** discipline: run the affected checks locally, prove any
residual failure is baseline/environmental, apply the **`locally-validated`**
label, and merge on GitHub-hosted (Docs) green. Merged PRs should carry
`locally-validated` + `post-facto-review`. See
`hermit/.llms/skills/post-facto-review/SKILL.md`.

### Remediation options (for the human)

1. **Add PMU self-hosted runner(s)** for `rrnewton/hermit` — reverie proves one
   runner drains a light load; hermit's heavy load needs N>1.
2. **Split the Rust job** so non-PMU parts (build, clippy, fmt, unit tests) run on
   GitHub-hosted runners in parallel, leaving only the RCB/determinism tests on
   the PMU runner.
3. **Throttle redundant triggers** (cancel-in-progress per branch is already
   happening as supersession cancels; also consider path filters / fewer
   pull_request event types).
4. **Formally accept `locally-validated`** as the gate (current de-facto practice).

### Starting additional runner slots

```sh
# Hermit slots 2 and 3
make start-hermit SLOT=2 START_ARGS=--detach
make start-hermit SLOT=3 START_ARGS=--detach

# Reverie slot 2
make start-reverie SLOT=2 START_ARGS=--detach
```

## Files

- `ci-status.py` — self-contained, non-mutating status reporter (Python 3, stdlib
  only; shells out to `gh`).
