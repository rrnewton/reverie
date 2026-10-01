# Flaky-failure attribution

A flake **rate** tells you nothing about the **cause**. "1/10 failed" is three
completely different bugs wearing the same costume, and they demand *opposite*
responses:

| Cause | What it looks like | Right response | Wrong response |
| --- | --- | --- | --- |
| **INFRASTRUCTURE** | a reverie check wedged 2h44m vs a 2-min baseline; a PMU-sensitive test on a contended runner; hermit-kvm unable to even measure at ~470 concurrent `hermit` procs | fix the runner / shed load; **do not touch product code** | "harden" a real product test to make the box green |
| **HERMIT_NONDETERMINISM** | the `detcore_misc` vfork-reap race (16–23%); a schedule that diverges run-to-run | the highest-value determinism fix there is | dismiss a real bug as "just flaky infra" |
| **ENVIRONMENT** | the guest read `/sys/module/<m>/refcnt` (or time/rand/meminfo) that varies under load | determinize / virtualize that one read | rerun until green and move on |

This directory is the capability that turns a rate into an attributed cause with
evidence. It has two halves:

1. **`capture-run.sh`** / `attribution.py capture` — the *harness prerequisite*.
   To attribute a flake you must still **have** the failing run's artifacts.
   Most of our harnesses run `( timeout … cmd >/dev/null 2>&1; echo $? )` — by
   the time a flake is noticed, everything but the exit code is gone. These
   wrappers run the command and, **on failure only**, preserve a *bundle*:
   `stdout`, `stderr`, exit code, wall time, and the **host conditions at that
   instant** (load, concurrent-proc count, CPU/mem PSI). On success they discard,
   matching the old `>/dev/null` footprint.

2. **`attribution.py attribute` / `report`** — the *classifier*. A pure decision
   procedure over the evidence in a bundle that emits one of
   `INFRASTRUCTURE | HERMIT_NONDETERMINISM | ENVIRONMENT | HARNESS_ERROR |
   INDETERMINATE`, a confidence, the reasons, and the **next step** (which for an
   honest `INDETERMINATE` is the *decisive probe to run next*, not a shrug).

## Quick start

```bash
# Capture: wrap any command; a failing run leaves a bundle under ./bundles/.
python3 attribution.py capture --label mytest --timeout 20 \
    --bundle-root ./bundles -- hermit run --strict --verify -- ./prog

# ...or from bash harnesses (no per-instance Python; safe under BpfJailer):
ec=$(./capture-run.sh ./bundles mytest 20 -- hermit run --strict --verify -- ./prog)

# Attribute one bundle. The single most decisive signal is reproducibility at
# low load: --low-load-control K re-runs the SAME command K times quietly.
python3 attribution.py attribute ./bundles/mytest-<stamp> --low-load-control 10

# If you have two trace logs (a passing and a failing --log info run), feed them
# in and the classifier localizes the divergence via `hermit log-diff`:
python3 attribution.py attribute ./bundles/mytest-<stamp> \
    --log-a /tmp/pass.log --log-b /tmp/fail.log

# Roll a whole capture dir up into a table of verdicts.
python3 attribution.py report ./bundles
```

## The decision procedure (what `attribute` encodes)

The classifier is deterministic and testable (`tests/test_attribution.py` encodes
the three real examples above). In order:

1. **Harness token** (`BUILD_FAIL`, `NOBIN`, …) → `HARNESS_ERROR`. Not a flake at
   all; the harness broke.
2. **Schedule divergence** — first differing trace line is a `COMMIT`
   (`(turn, dettid)` reordered) → `HERMIT_NONDETERMINISM` (high). Only a product
   bug reorders the schedule. This fires *even under load* — a load-dependent
   hermit race and an infra hang both need load, but only the race leaves a
   localizable schedule divergence.
3. **Data divergence** — `COMMIT`s match but a `DETLOG` value differs. If that
   value **looks like a live host reading** (matches `/sys/`, `/proc` (not
   `self/maps`), `clock_gettime`, `getrandom`, `rdtsc`, `cpuid`, `meminfo`,
   `loadavg`, `refcnt`, …) → `ENVIRONMENT` (high). Otherwise → hermit (medium).
4. **Hang** (timeout, no divergence) — first the free **cpu/wall kill signature**
   (`ci-hub/lib/kill_signature.py`, the one table shared with
   `ci-hub/history/query.py kill-taxonomy`), then the low-load control:
   * `cpu/wall >= 0.8` at the budget → **LIVELOCK** → `HERMIT_NONDETERMINISM`
     (high). A spin to budget is retry-futile, so **this red is REAL** — the
     single most useful thing to know before condemning a PR.
   * **OOM is excluded before the ratio test**, and that ordering is load-bearing:
     OOM rows reach `cpu/wall` of 127 (parallel build vs a memory ceiling), so
     without the exclusion 17 of the 27 kills in the live store would be
     mislabelled livelocks. → `INFRASTRUCTURE` (medium).
   * `cpu/wall < 0.3` → wait-bound. This **rules out a livelock but picks no
     cause**: starvation and a futex/deadlock wedge are indistinguishable at
     `cpu ≈ 0`, so it falls through to the control rather than guessing.
   * then, decided by the **low-load control**:
   * clean at low load **and** host was under pressure at failure →
     `INFRASTRUCTURE` (high).
   * clean at low load, no measured pressure → `INFRASTRUCTURE` (medium).
   * still fails at low load → `HERMIT_NONDETERMINISM` (a real wedge).
   * **no control run** → honest `INDETERMINATE` that *names the decisive test*.
5. **Crash / mismatch / nonzero** — external-host reads in the trace lean
   `ENVIRONMENT`; deterministic-at-low-load leans hermit; else `INDETERMINATE`
   prescribing `hermit log-diff` / `--log info`.

`INDETERMINATE` is a feature, not a cop-out: every `INDETERMINATE` carries the
next probe that would resolve it. The tool never guesses a cause it cannot
support with evidence.

## Wiring into a harness

`STRESS_CAPTURE_DIR` is the standard env knob. When set, a burst primitive such
as the multisect experiment's `matched.sh` routes each instance through
`capture-run.sh`. Off by default; the hot loop stays byte-identical when unset.
The former `ci-hub/stress/` nightly that folded reports into its alarm was
deleted on 2026-10-01; the flake census now runs through Hermit's
`ci/compat-envelope/pressure-test.rs --no-retry` and `scripts/stress-test.sh`
(see `ai_docs/stress/2026-10-01-e2e-10x-flake-census.md`).

`ATTR_CAPTURE_MAX` caps preserved bundles (default 200); overflow is **counted**
in a `.dropped-over-cap` file, never silently dropped. `ATTR_PROC_PATTERN`
(default `hermit`) is the `comm` substring counted for the concurrent-proc
stampede signal.

## The full written procedure

The human-facing decision tree, the five attribution signals, and how to gather
each piece of evidence live in
`ai_docs/flaky-failure-attribution-procedure_20260803.md`.
