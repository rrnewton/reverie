# Nightly Hermit determinism stress harness

Task: `nightly-stress-tests-not-actually-running` (owner: hermit-250).

> "Hermit level flakiness is our kryptonite. This is why we do the nightly
> stress tests." The criterion is **determinism of outcome, not "did it pass"**:
> run N instances concurrently, and *anything other than 0% or 100% is FLAKY, and
> FLAKY IS A RED CONDITION*. 29/30 must ALARM, not round to green. Any nightly
> failure is a P0.

This directory is the **recording + verdict + alarm + scheduling** layer. It
owns **no burst logic** — that is the shared concurrent-burst primitive (see
"Shared harness" below). One primitive, measured one way; two readings.

## Why it lives on the parent host, not a GitHub runner

The flake this catches is **load-dependent**. hermit-multisect measured ~28%
per-instance hang at 128-wide concurrency *under real fleet load* on this host;
an idle, single-tenant GitHub-hosted runner shows ~0% — a **false green**. The
existing weekly `super` GHA job (`.github/workflows/validation-levels.yml`) is
structurally unable to catch this class, and separately never completes (it hits
its 6h `timeout-minutes:360` and is cancelled — 0/2 completions as of
2026-08-03; see task notes FINDING A–C). The nightly must run where the load is.

## Components

| File | Role |
|------|------|
| `matched-burst.sh` | **CANONICAL nightly burst (what cron runs).** Calibrator-gated matched-load probe: builds subject at main HEAD, runs multisect's `matched.sh` with subject + a known-flaky calibrator interleaved every wave, emits one CSV row per **calibrator-valid** wave (else a RED `CALIB_UNDERPOWERED` row). See "Validity calibrator" below. |
| `stress_rows.py` | Producer-owned typed contract for each burst row, the derived summary, and each stored run record. |
| `stress_store.py` | flaky-is-red verdict + durable JSONL store + `summary`/`selftest` CLI |
| `nightly.sh` | driver: resolve main HEAD → run `$STRESS_BURST_CMD` per workload → record → alarm |
| `stress-burst` | first-gen concurrent-burst primitive (`--prebuilt`/`--build-at`); emits the CSV contract. **Deprecated for nightly — it has NO validity calibrator, so a quiet host can false-green.** Retained for on-demand single-burst use. |
| `burst-build.sh` | first-gen scheduled adapter (`stress-burst --prebuilt` in a persistent worktree). **Superseded by `matched-burst.sh`** (same no-reflink worktree, adds the calibrator). |
| `burst-prebuilt.sh` | cheap adapter: `stress-burst --prebuilt <primary>` (no build; on-demand) |
| `README.md` | this file |

Durable calibrator asset: `ignored/ci-hub/stress-calib/calib-9c964fce` (gitignored;
provisioned once from hermit-multisect's prebuilt binary — see `PROVENANCE.txt`
alongside it).

Store: `ignored/ci-hub/stress-runs.jsonl` (gitignored, append-only, one event
per run). Joins the rest of the ci-hub store by `(repo, git_sha)` and the same
`OWNER/REPO` string. Alarm markers: `ignored/ci-hub/stress-alarm-*.json`.

## Flaky-is-red verdict (the important part)

Fold every burst in a run through:

| Verdict | Condition | Signal |
|---------|-----------|--------|
| `CLEAN`   | every OK burst had `passes == N` (0 hangs, 0 others)     | 🟢 GREEN |
| `FLAKY`   | some OK burst had `0 < passes < N`                        | 🔴 RED / P0 |
| `FAILING` | some OK burst had `passes == 0`                          | 🔴 RED / P0 |
| `ERROR`   | a burst could not run (non-OK STATUS), no flaky/fail seen | 🔴 RED / P0 |

`CLEAN` is the *only* non-alarm. A run that could not execute its probe is RED
too (a silent-green nightly is the exact failure mode this task exists to kill).
`stress_store.py record` exits **2** on any RED, **0** on CLEAN; the driver's
overall exit is 0 (green) or 1 (P0). A malformed producer row exits 3 and the
driver records an `ERROR` alarm rather than treating the missing measurement as
green. Verified: `stress_store.py selftest`.

## Validity calibrator (the OTHER important part)

Flaky-is-red only works if the burst was **powerful enough to expose the flake**.
The vfork/reap race is load-dependent: on a quiet host every subject instance can
pass and the nightly rounds it to GREEN — a **false negative that hides a broken
subject**. `matched-burst.sh` closes that hole with a calibrator, exactly as
hermit-multisect calibrated its bisection:

1. **Matched-load waves.** `matched.sh` launches `<width>` single-shot instances
   of *every* label — the subject AND a known-flaky calibrator — interleaved in
   the SAME wave, so subject and calibrator see identical instantaneous host load
   (matters on this 316-core box).
2. **Known-flaky witness.** The calibrator is `calib-9c964fce` — the
   permanently-flaky `9c964fce` test binary (reverie pin `fb2cf7e0`, measured
   21.6% hang; the multisect flip commit). Being a fixed binary, it stays flaky
   forever.
3. **Validity gate.** A wave COUNTS only if the calibrator comes back `FLAKY`/
   `FAIL`. A wave where the calibrator stays clean was **under-powered** and is
   DISCARDED, not scored — the subject's clean result in that wave is meaningless.
4. **No-valid-wave ⇒ RED.** If *no* wave is valid (the calibrator never witnessed
   its own known bug), the run emits `CALIB_UNDERPOWERED` → `ERROR` → RED. The
   nightly cannot certify green when it could not even reproduce a known flake.

Only the subject's **valid-wave** rows reach `stress_store.py`, one row per wave,
so each wave's trinary (all-pass = CLEAN, mixed = FLAKY, zero-pass = FAILING) is
judged independently. multisect calibration: calibrator 30×@load407 → 5/5 waves
FLAKY (100% detection). Smoke-verified end-to-end 2026-08-03 @load322: subject
`cd96303e` scored 3/3 valid waves → 22 hangs / 50 passes / 72 → FLAKY → 🔴 P0
(hang_rate 0.31), matching the known ~16–22% open flake.

## Shared harness (owner rule: "do NOT write a second one")

The concurrent burst is a **single shared primitive**, `ci-hub/stress/stress-burst`
— a generalization of hermit-multisect's
`experiments/multisect_detcore_misc_20260803/probe.sh` (same burst loop, same
trinary classification, **identical CSV**, so zero measurement drift). It was
extracted as a NEW file; probe.sh is untouched. multisect may migrate probe.sh to
call it or keep probe.sh — either way the CSV is identical. nightly and multisect
read the **same k/N** two ways:

- **nightly** → flaky-is-red (above): any hang ⇒ P0.
- **multisect** → rate bands (GREENISH ≥ 0.67 / WEDGED ≤ 0.33) to *locate* the
  transition commit. Bands find the step under noise; they never bless a flaky
  commit.

### Burst CSV contract (the only coupling)

    $STRESS_BURST_CMD <sha> <width> <timeout_s> <workload>  ->  CSV on stdout
    row: sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS
    STATUS: OK | WT_FAIL | BUILD_FAIL | NOBIN | NOTEST | PROBE_FAIL
          | CALIB_MISSING | MATCHED_MISSING | CALIB_UNDERPOWERED
          | NOWIRE | NOOUTPUT   (non-OK = harness error / could-not-validate)

The row has exactly nine columns. An `OK` row must carry `burst_N`, `hangs`,
`passes`, and `other`, and the three outcome counts must sum exactly to
`burst_N`. Missing columns are not padded, extra columns are not truncated, and
an unknown status is refused. These checks are what make `CLEAN` a claim about
every promised instance rather than merely every instance that happened to be
reported.

`matched-burst.sh` may emit MULTIPLE `OK` rows (one per calibrator-valid wave);
`stress_store.py` folds them all. The three calibrator tokens are the
could-not-validate cases, all RED (`ERROR`).

`stress_rows.py` owns these columns and their validation; `stress_store.py`
imports that type for both the JSONL writer and reader. The layer therefore
cannot silently pad, truncate, or reinterpret a producer row regardless of
where the primitive lives or how it builds.

### Why the collision mattered (resolved)

`probe.sh` hardcodes its worktree to
`experiments/multisect_detcore_misc_20260803/ignored/wt/<sha>`, so invoking it
for a main-HEAD nightly would collide with multisect's live SHA-keyed bisection
worktrees. `stress-burst` resolves this with two build modes — `--prebuilt DIR`
(no build; burst DIR's existing binary) and `--build-at SHA --wt-base DIR`
(isolated, relocatable per-SHA worktree, reflink-seeded) — so nightly and
multisect share code without ever sharing a worktree. Actual signature:

    stress-burst {--prebuilt DIR | --build-at SHA [--wt-base DIR] [--hermit DIR]} \
                 --workload BIN:TESTPATH [--width W] [--timeout S] [--out CSV]

**The nightly does NOT use `--build-at`.** `--build-at` reflink-seeds `target/`
from the primary (`cp -a --reflink=auto`), which is (a) blocked by this host's
BPFJailer FS policy for the agent role and (b) a known cmake-cache poisoner. So
`burst-build.sh` instead keeps ONE persistent detached worktree
(`ignored/ci-hub/stress-wt/nightly`), checks out the recorded SHA into it, builds
in-tree (cold first night, incremental after — no reflink), and bursts it via
`--prebuilt`. `--build-at` remains for multisect's per-SHA bisection, where a
throwaway seeded worktree per commit is the right trade-off.

Coordination with hermit-multisect on placement/ownership: see task note
"(4) SHARED-HARNESS PROPOSAL" + "CONVERGENCE OFFER". The primitive is a new file;
probe.sh is untouched, so there is no drift regardless of the outcome.

## Workload set (where flakiness actually bites)

Seeded with the CONFIRMED reproducer
`tests_misc:vfork::vfork_parent_resumes_after_child_exec`; grows to the
`detcore_misc` reap/waitpid set and a scheduling/futex-contention burst. The
current wired weekly `super` probes (`/bin/echo`, `yes|head|sha256sum`) are
single-process/single-thread theater — they stress harness startup, not the
reap/vfork/scheduling paths where determinism flakes live (task note (5)).

## Running

```bash
# verify verdict logic (no build needed)
ci-hub/stress/stress_store.py selftest

# one calibrated nightly pass (what cron runs): builds subject at main HEAD,
# runs STRESS_WAVES matched waves with the known-flaky calibrator, scores only
# valid waves:
STRESS_BURST_CMD=$PWD/ci-hub/stress/matched-burst.sh \
  STRESS_WIDTH=64 STRESS_TIMEOUT=20 STRESS_WAVES=10 \
  ci-hub/stress/nightly.sh              # exit 0 = green, 1 = P0

# review recorded runs (exit 2 if latest is RED)
ci-hub/stress/stress_store.py summary
```

`matched-burst.sh` env knobs: `STRESS_WAVES` (matched waves, default 10),
`STRESS_CALIB_BIN` (calibrator, default `ignored/ci-hub/stress-calib/calib-9c964fce`),
`MATCHED_SH` (default multisect's `matched.sh`).

### Scheduling (INSTALLED)

Nightly at 04:30 local on this host (off-hours; `<width>×2`=128 concurrent
single-shots per wave are self-loading, so it does not depend on ambient fleet
load — and if a quiet host under-powers the wave, the calibrator gate turns that
into a RED `CALIB_UNDERPOWERED`, never a false green). 10 matched waves × 64 at
main HEAD, appending to `~/.local/state/nightly-stress.log`. Rewired to the
calibrated path 2026-08-03:

```cron
30 4 * * * PATH=/home/newton/.cargo/bin:/home/newton/orc-bin:/usr/local/bin:/usr/bin:/bin \
  STRESS_BURST_CMD=/home/newton/work/dev-hermit/ci-hub/stress/matched-burst.sh \
  STRESS_WIDTH=64 STRESS_TIMEOUT=20 STRESS_WAVES=10 CI_HUB_AGENT=hermit-250 \
  /home/newton/work/dev-hermit/ci-hub/stress/nightly.sh \
  >> /home/newton/.local/state/nightly-stress.log 2>&1
```

NOTE: main HEAD currently carries the open reap/vfork flake (reverie #355 not yet
pinned on main), so the first scheduled run is expected to alarm RED — correctly
flagging the known-open P0. It goes GREEN once the fix lands and the pin bumps.

### Where a RED goes (visibility)

On any non-CLEAN verdict `nightly.sh` raises a P0 through four channels, in order
of durability:

1. **Durable store record** — one JSON event in `ignored/ci-hub/stress-runs.jsonl`
   with `alarm:true` (the primary signal; never suppressed).
2. **Exit 1** from `nightly.sh` (0 = green), captured in the cron log.
3. **Alarm marker** — `ignored/ci-hub/stress-alarm-*.json`.
4. **`tg note` to the STANDING task `nightly_stress_red_triage`** (`$STRESS_ALARM_TASK`
   default). This is deliberately NOT the setup task
   `nightly-stress-tests-not-actually-running`: that one gets closed, and a `tg note`
   to a closed task drops out of active views — a silently-invisible alarm, the exact
   failure this harness exists to prevent. The standing task stays open so every
   future nightly P0 lands somewhere a coordinator watches. (`tg` slug ids use
   underscores; the hyphen form does not resolve.)

VERIFIED 2026-08-03: cron daemon live (`crond` active); a full e2e run through the
exact cron entrypoint on the calibrated path @ main `8c0aeb0d` (width=24, 3
calibrator-valid waves) produced FLAKY (5 hangs / 72 instances) → store record
(`source_tool=matched-burst.sh`) → P0 marker → auto `tg note` → `nightly.sh` exit 1.
