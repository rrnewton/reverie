# Portable CI and the GitHub-hosted account concurrency ceiling — measured

> **Historical measurement, not landing policy.** Parent `AGENTS.md` and the
> owner's 2026-08-10 decision supersede this document's references to a
> "required merge-gate": normal Hermit landing uses exact-head VALIDATE in the
> LEDGER, while GH Actions is advisory P2 except for a PR modifying
> `scripts/validate.rs`. The queue measurements below remain historical data.

**Date:** 2026-08-04
**Agent:** CI-health research (opus-4.8), task `portable_ci_hits_github`
**Status:** research-only; no code/workflow changes. All numbers measured via `with-proxy gh`.

## Question

The owner flagged a claim as possibly *arithmetic, not measured*:

> "Portable CI hits the GitHub-HOSTED ACCOUNT-concurrency ceiling (~90 PR fan-outs x ~30 jobs),
> starving the TRAILING AGGREGATION jobs — the fan-out consumes the account quota and the job that
> reports the verdict never runs, so a run can be substantially complete yet report nothing."

Verify the actual triplet (fan-out width per run x concurrent runs vs the real account ceiling),
confirm/refute the trailing-aggregation starvation, re-measure the hosted side after the Demo Gate
land (#1562), and decide whether this is the same finding as `is-portable-ci-usable-at-all-evidence`.

## Method (exact commands)

```
# repo / plan visibility
with-proxy gh api users/rrnewton --jq '{login,plan}'          # -> {"login":"rrnewton","plan":null}
with-proxy gh api repos/rrnewton/hermit --jq '{visibility}'   # -> public

# fan-out width + reporter DAG position: hermit/.github/workflows/ci-portable.yml (all runs-on: ubuntu-latest)

# live congestion + timeline
with-proxy gh run list -R rrnewton/hermit --workflow "CI (GitHub-managed portable)" --limit 200 \
  --json databaseId,status,conclusion,createdAt,event,headBranch

# observed ceiling: max concurrent jobs per run via sweep over job startedAt/completedAt
with-proxy gh run view <id> -R rrnewton/hermit --json jobs,createdAt,updatedAt,status,conclusion,event
```

Reproduction scripts: `scratch/portable-ceiling/{conc.py,jobs.py}` (scratch is ignored; re-derive fresh).

## Measured numbers

### Fan-out width per run — 34 hosted jobs, ~19 parallel peak
`ci-portable.yml` ("CI (GitHub-managed portable)") has **12 top-level jobs**, 3 of them matrices
(`test-debug`, `test-release`, `e2e`) that expand to **34 total jobs** in a full run. **Every job is
`runs-on: ubuntu-latest` — 100% GitHub-hosted, zero self-hosted.** The natural parallel *width* of one
run (jobs eligible at once after `build-*` completes) is ~19-22 (the e2e/test cells).

So the claim's "~30 jobs/run" is **CONFIRMED** (actually 34).

### Concurrent runs at peak — ~91 (confirmed from sibling's snapshot)
Sibling task `is-portable-ci-usable-at-all-evidence` (and `ai_docs/ci-queued-180-distinct-vs-superseded_20260803.md`)
measured **91 CI (portable) runs queued of 168 hermit runs** during the 2026-08-03 congestion window.

So the claim's "~90 fan-outs" is **CONFIRMED** (91 concurrent runs).

### The actual account ceiling — OBSERVED ~20 concurrent jobs (NOT 90x30)
This is the arithmetic error the owner suspected. **90 x 30 ≈ 2700 is the DEMAND, not the ceiling.**
The ceiling is the account-wide concurrent-*job* cap. Plan is not exposed via the API (`plan:null`,
public repo), so it was measured directly by "verify the running thing":

| run | window | event | jobs | **max concurrent** | wall | billed | realized parallelism |
|-----|--------|-------|------|--------------------|------|--------|----------------------|
| 30930063816 | quiet 08-04 | push | 34 | **19** | 0.69h | 2.30h | 3.3x |
| 30932980033 | quiet 08-04 | push | 34 | **18** | 0.46h | 2.10h | 4.6x |
| 30919378129 | quiet 08-04 | dispatch | 34 | **17** | 0.58h | 2.24h | 3.9x |
| 30819597014 | congested 08-03 (owner's ref) | PR | 34 | **8** | 8.02h | 1.93h | 0.24x |
| 30818220504 | congested 08-03 (trailing-agg ex.) | PR | 34 | **6** | 9.90h | 2.12h | 0.21x |

A **single unconstrained run caps at ~17-19 concurrent even though ~22 e2e/test cells are eligible at
once** — it never reaches 22. That ceiling sitting just under 20 is consistent with GitHub's documented
**Free-plan standard-hosted-runner limit of 20 total concurrent jobs** per account (Pro 40 / Team 60 /
Enterprise 180; macOS 5). Direct fetch of `docs.github.com` is network-blocked for this agent (same
limitation the sibling reported), so the doc is cited for the number and the ~19 observed cap is the
load-bearing measurement regardless of the exact tier.

**Measured triplet: 34 hosted jobs/run x ~91 concurrent runs vs an account ceiling of ~20 concurrent
jobs.** The account has room for **essentially ONE portable run at full speed.** A single run wants ~19
of the ~20 slots; a *second* concurrent run already halves both. At 91 runs the oversubscription is ~85x.

### Consequence, measured: the same run is 12x slower under congestion
Quiet-window runs get 17-19 concurrent and finish in **~30-40 min wall** (~2.1-2.3h billed compressed
3-5x). The identical workload under 08-03 congestion got only 6-8 concurrent and took **8-10h wall for
the same ~2h billed** — 0.2-0.24x realized parallelism, i.e. mostly sitting in the shared queue.

## Starvation mechanism — CONFIRMED (structural), with a precision correction

The verdict job is **`regular` = "Regular tests (GitHub-managed portable)"** (the required merge-gate
check). Its DAG position:

```
regular  needs: [select, plan, preflight, build-debug, build-release, test-debug, test-release, e2e]
```

It depends on the **entire fan-out** and is structurally the last real job (only `cleanup` follows). The
former standalone `reduce-e2e` aggregation was **folded into `regular`**, so the verdict + e2e reduction
are one trailing node. Under a ~20 cap shared across ~91 runs, the fan-out cells consume the slots and
`regular`, being last, waits for every cell to finish *and* a free slot — a multi-hour wait. This is the
"fan-out eats quota, the reporter runs last" cruelty, and it is **CONFIRMED structurally**.

**Precision correction to the claim's "the reporter never runs":** in the cited example run
`30818220504`, `regular` and the folded reducer *did* eventually run — they **FAILED** because an
upstream `e2e` cell (`data-handling__verify__ptrace`) was **cancelled by supersession**, so the reducer
hit "no files found" on the missing archive. So the durable failure mode is not "reporter permanently
denied a slot" but "**run substantially complete (31/34 green) yet the trailing verdict emits a
failure/no-verdict**" — real, and caused by (a) the verdict being gated behind everything under the cap
(8-10h critical path) plus (b) supersession cancelling an upstream cell that breaks the reducer's input.
"Substantially complete yet reports nothing useful" is **REAL and observed**; "never runs" is only the
transient snapshot (31/32 with the reducer queued) that a prior agent caught.

## Post-#1562 re-measurement — hosted side is unchanged BY #1562 (orthogonal), currently latent

PR #1562 (merge `9e85f02f`, landed 2026-08-03T20:02Z) re-keyed the **P0 Demo Gate** concurrency and
removed its `pmu-serial` pin — a **self-hosted** fix. Portable CI is **100% ubuntu-latest hosted**, so
#1562 **cannot** have touched hosted congestion by construction (orthogonal, confirmed).

Live hosted state now (2026-08-04 ~21:41Z): of the last 200 portable runs, **0 queued, 2 in_progress**;
65 success / 17 failure / 116 cancelled over ~31h. Arming-rate timeline (runs created per hour):
08-03 14-18Z ran hot at 20-28 runs/hr with 12-19 cancellations/hr (the supersession storm); after 18Z
it fell to 3-13/hr and by 08-04 to **1-8/hr, mostly successes**. **The hosted recovery tracks the
arming-rate collapse, not #1562** (which landed at 20:02Z, after the descent began). Note: the sibling's
"zero completions for 8h" was a real-time snapshot — those 14-18Z runs *did* complete later once the
storm subsided and the queue drained; binning their eventual outcome by creation hour shows the
successes that were still queued at snapshot time.

**Net:** the hosted account ceiling is a **standing structural limit (~20 concurrent jobs), currently
latent only because load is low.** Any renewed arming spike (mass drain, mass re-push) will re-saturate
it exactly as on 08-03. It is measured cleanly on its own and is not conflated with the (now-fixed)
self-hosted Demo Gate problem.

## Same finding as `is-portable-ci-usable-at-all-evidence`? — YES for the throughput half

The sibling decomposed that task into three mechanisms:
1. **SLOW (throughput)** — 8h wall / 2h billed, max ~7 concurrent, 2.6h queue wait. **This IS the account
   concurrency ceiling.** SAME FINDING: the 8h wall = queue wait for slots under the ~20 cap while ~91
   runs compete; the 2h billed = the real CPU, barely parallelized. My measurement (single run wants ~19,
   cap ~20, throttled to 6-8 under contention) is the same phenomenon, quantified at the cap.
2. **BROKEN-failures** — the `e2e.metadata` node rc=2 cascade (stale test inventory, first-bad
   `0ffa2fb3`, already fixed on main by `a034f39c`). **DISTINCT** — a product/test bug, not the ceiling.
3. **BROKEN-recent** — the supersession/cancellation storm from mass re-arming ~25 branches. This is the
   **demand-side amplifier of the same ceiling**: the storm only starves *because* the ceiling is ~20;
   without the cap, 91 runs would simply run. The cancellations are what turn "slow" into "0 completions."

**Verdict:** the account ceiling and the sibling's "SLOW / 8h-vs-2h" finding are **ONE finding** — cross-link
both tasks. The "failures/skips" the owner saw are a **separate** product bug (e2e.metadata, fixed) plus
the supersession amplifier of this same ceiling. So: throughput half = same finding; failure half = distinct.

## Candidate levers (analysis only — do NOT implement), ranked by ROI vs measured demand

The design target is the **measured** shape: ceiling ≈ 20 concurrent jobs; one run's peak width ≈ 19;
peak ≈ 91 concurrent runs. The account holds ~one portable run at a time, so *demand control beats supply*.

1. **Admission control — cap concurrent portable PR runs (serialize arming).** HIGHEST ROI. Since one run
   ≈ the whole cap, permitting >~1-2 concurrent portable runs is self-defeating (they time-slice and
   supersede each other into 8-10h thrash + cancellation). Capping concurrent portable runs to ~1-2 and
   queueing the rest converts the storm's ~0 completions/8h into steady serial ~35-min runs that actually
   finish (~1.5 completions/hr, matching billed capacity). Directly attacks the 91-vs-20 oversubscription;
   this is the owner's noted sequenced-behind lever. **RISK/SAFETY:** must PRIORITIZE merge-gate and main
   runs and be FIFO-with-priority, never blanket-cancel; never starve or cancel a run serving as a landing
   gate, and never cancel a main run a pin bump is actively citing (same discipline as #1562).
2. **Reduce hosted fan-out width per run (shard-coalescing).** MEDIUM-HIGH. One run's ~19 peak nearly
   equals the cap; coalescing the ~22 e2e/test cells into ~6-8 heavier jobs drops per-run peak from ~19 to
   ~8, so two runs fit within the cap concurrently. Expected delta: ~2 concurrent runs instead of ~1;
   halves oversubscription. **RISK:** longer per-job wall (less intra-run parallelism), coarser failure
   attribution per cell.
3. **Emit the verdict independent of the fan-out / reserve a slot for it.** MEDIUM. `regular` is
   structurally last and dies on cancelled-cell input. Making the verdict emit a definite result even when
   upstream cells are throttled/cancelled (or reserving a slot / cheap lane for the reducer) converts
   "substantially complete but no verdict" into an emitted verdict. Fixes the reporting cruelty, not
   throughput. **RISK:** the reducer needs the e2e archive artifacts, so it cannot fully leave the DAG;
   supersession-cancelled cells still break its input unless combined with lever 1.
4. **Larger-runner consolidation (safe-ci-dag-runner as the in-runner scheduler).** LOW-MEDIUM ROI, higher
   effort. Portable currently uses **GitHub Actions as the outer scheduler** (`ci/run-node.sh` jq-extracts
   and bash-runs nodes); a larger hosted runner running the whole DAG in-process counts as ~1-3 hosted
   jobs, so the ~20 cap would fit ~10-20 runs. Big structural win but a real architecture change; larger
   runners are cost/tier-limited and lose per-cell Actions retry/visibility.
5. **Buy concurrency (upgrade plan).** LOWEST ROI. 20->40 (Team) still only fits ~2 runs vs 91 demand — it
   does not fix structural oversubscription and costs money; demand control (lever 1) is far cheaper. Note
   CLAUDE.md security constraint against privileged self-hosted runners; a hosted-equivalent upgrade is the
   only clean supply path and it is still dominated by demand control.

## Bottom line

The demand numbers in the claim are **CONFIRMED** (34 hosted jobs/run x ~91 concurrent runs). The
correction the owner suspected is real: **"~90 x 30" is the demand, not the ceiling** — the measured
ceiling is **~20 concurrent jobs**, so the account can run essentially **one portable run at a time**.
The trailing-aggregation starvation is **confirmed structurally** (`regular` needs the whole fan-out),
with the precise durable failure being "31/34 green yet the trailing verdict fails/cancels on
supersession-broken input," not a permanent no-run. #1562 is orthogonal (self-hosted); the hosted side
is a standing ~20-job ceiling, currently latent because arming dropped. This is **the same finding** as
the SLOW/8h-vs-2h half of `is-portable-ci-usable-at-all-evidence`. Top lever: **admission control that
caps concurrent portable runs to ~1-2 with merge-gate/main priority.**
