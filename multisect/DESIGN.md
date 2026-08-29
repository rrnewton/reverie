# multisect — design rationale

This records *why* multisect is shaped the way it is. For usage see
[`README.md`](README.md); for the measurements see
[`experiments/multisect-build-weight_20260803/`](../experiments/multisect-build-weight_20260803/).

## Why a separate tool (not ci-hub)

ci-hub *studies* CI health (passive: it records and queries what happened).
multisect *drives* a search (active: it decides which commits to build and run
next, to localize a regression). Different verb, different tool. multisect
*consumes* ci-hub — its known-green history makes trustworthy search bounds
(`lib/anchors.py`) — but does not live inside it.

## Why reuse `debug/multisect` (the engine)

The hard part — rate-aware selection over a commit range, green-rate
classification per commit, and recursion to a localized blame — already exists and
`hermit-multisect` depends on it live. Forking it would create a second,
drifting classifier. multisect instead *composes* with it and supplies only the
parts the engine deliberately leaves to the caller:

| concern | owner |
|---|---|
| commit selection, green-rate model, recursion, verdict thresholds | `debug/multisect` engine |
| checkout + build + run one probe | `multisect-probe` |
| registered validation checkout + cleanup + cache gc | probe + orchestrator |
| known-green anchor from ci-hub | `lib/anchors.py` |
| up-front ETA + cost lines | `lib/estimate.py` + orchestrator |

## The automation boundary (the whole point)

The investigating agent spends tokens **once**, writing a `probes/<name>.env`.
After `multisect run`, every probe — possibly hundreds — is pure code. If a design
choice would put a token in the per-probe loop, it is wrong. This is why the probe
is a self-contained bash script driven entirely by env vars, why verdicts are exit
codes the engine already understands, and why calibration is a *measurement*, not
a question asked of the agent.

## Checkout strategy: `git worktree` (decided with evidence)

Considered: full clone (slow, duplicates the object store), `cp -a`/reflink copy
(duplicates or COW-then-diverges; risks stale `target/`), and registered linked
worktrees. `wrkslots` now creates each short-lived validation checkout, while a
per-commit target directory outside the checkout preserves warm build artifacts.
Parallelism remains bounded by cores and the existing per-commit lock rather
than by agent-slot ownership.

Two findings shaped the implementation:

- **Concurrent checkout creation must remain registered.** `wrkslots` owns the
  repository-global mutation and append-only lifecycle record; builds run
  concurrently after creation.
- **Cold build is the safe default; reflink warm-seed is opt-in.** A seeded
  `target/` risks stale reuse and pays a ~10 s absolute-path fingerprint relink
  tax, so the default is a clean per-worktree cold build. Reps 2..N on the same
  commit are naturally warm (~0.4 s cargo no-op).

## Build weight: measure, keep backtraces

The build dominates probe cost, so it was measured, not assumed. The **C3**
profile (`CARGO_INCREMENTAL=0` + `debug=line-tables-only` +
`split-debuginfo=unpacked`) is the cost floor that still yields **usable file:line
backtraces** — the requirement that rules out `debug=0`/strip. C3 is strictly
cheaper than the default dev profile (18.1 s vs 24.0 s cold; 68.7 MB vs 210 MB
binary) and its ~0.92 GiB/probe disk keeps a whole search three orders under the
200 GB cap. The build-weight table doubles as the estimator's calibration data.

## Build-vs-hang separation (correctness-critical)

A regression search for a *hang* must never confuse a slow cold build with a test
that wedged. So only the test binary is wrapped in `PROBE_TEST_TIMEOUT`; the build
runs outside it; and the box's outer wall timeout is a generous cover
(`ceil(cold_build×2.5 + hang + 30)`). A build *failure* is treated as infra, not a
wedge — main commits normally compile — and leaves a `build-failed-<sha>` marker
so the orchestrator warns that a blame may be a compile break (e.g. nightly
toolchain drift on an old commit), not the target bug.

## Cost model (`lib/estimate.py`)

The estimator is a small, explicit model, not magic:

- `rounds = ceil(log(interval) / log(k+1))` — the engine narrows by ~(k+1)× per
  round.
- **Wall** ≈ critical path = `rounds × batches_per_round × commit_serial_reps`,
  where `batches = ceil(probes_per_round / jobs)` and rep 1 is cold, reps 2..N
  warm; wedged reps cost `hang_timeout`, not `test_s` (via `wedge_fraction`).
- **CPU** = `total_probes × commit_cpu` — **never divided by parallelism**, so it
  reflects true machine cost regardless of `jobs`.

If the estimate is large it emits levers (narrow interval, lower N with the
confidence cost stated, more parallelism, lighter build). This is the
"estimated-time-to-blame, printed up front" requirement, made honest by
calibrating on a real green commit first.

## Cost-awareness convention (reusable)

Both cost lines follow `ci-hub/TOOL-COST-CONVENTION.md` and are emitted via
`ci-hub/bin/tool-cost`, so `multisect/search` is just one consumer of a
machine-readable convention any tool can adopt. The orchestrator also prints its
own `COST ACTUAL` in a `finally` block, guaranteeing wall+CPU even if the wrapper
is absent or the run aborts.

## Pool lifecycle

Validation worktrees live under `worktrees/validate/`; build caches and probe
output live under `multisect/ignored/`. Each probe removes its slot through
`wrkslots` on exit. `gc` clears non-worktree cache directories and deliberately
retains historical unregistered worktrees, naming the inspection and migration
remedy instead of deleting evidence it cannot classify.
