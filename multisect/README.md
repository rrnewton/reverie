# multisect

**Configured-probe front end for rate-aware Git-range search.** You configure a
probe *once*; then the entire search — checkout, build, run, classify, recurse —
runs as **automatic code with zero tokens per probe**. Per-probe token burn is a
design failure; that automation boundary is the whole point of the tool.

multisect is deliberately thin. It wraps the existing rate-aware search engine
(`debug/multisect`) and adds only the layer that engine omits:

- a **probe harness** (`multisect-probe`): checkout → targeted-minimal-build →
  run-one-test → PASS/HANG/FAIL verdict, per commit-rep;
- a **registered validation checkout per probe**, removed through `wrkslots`;
- a known-green **anchor** provider that consumes ci-hub history (`lib/anchors.py`);
- an up-front **ETA-to-blame estimator** (`lib/estimate.py`), measured not guessed;
- the **tool-cost convention** (`ci-hub/TOOL-COST-CONVENTION.md`): a `COST
  ESTIMATE` line before the work and a `COST ACTUAL` (wall **and** CPU) line on
  every exit path, including failure and abort.

It does **not** reimplement commit selection, green-rate classification, or
recursion — that is the engine's job, and `hermit-multisect` depends on it live.

## Quick start

```bash
# 1. Configure a probe once (see probes/detcore_misc.env for the reference).
#    Then run the search — this is the only agent action; everything after is code.
./multisect/multisect run --probe detcore_misc \
    --good <known-green sha> --bad <known-broken sha> -k 3 -n 120 -j 8

# See the cost before committing to it (calibrates, prints ETA, stops):
./multisect/multisect estimate --probe detcore_misc --good <sha> --bad <sha> -n 120

# Measure one probe's build/test cost on this host:
./multisect/multisect calibrate --probe detcore_misc --good <sha>

# Clear probe caches; retain and report any historical unregistered worktrees:
./multisect/multisect gc
```

`--good` may be omitted; multisect then asks ci-hub for the newest known-green
ancestor of `--bad`. If none is known it **fails safe** and requires an explicit
`--good` — it never fabricates a lower bound.

## Cost awareness (mandatory, built in)

Two guarantees, both from `ci-hub/TOOL-COST-CONVENTION.md` so any tool can reuse
them:

1. **Estimated time-to-blame, printed up front.** `run`/`estimate` first runs a
   single *calibration* probe (measures real cold-build, warm-build, and test
   cost on *this* host — no guessing), then prints the expected WALL and CPU to
   converge, the basis, and — if the ETA is large — the **levers** to shrink it
   (narrow the interval, lower N with the confidence cost stated, raise
   parallelism, or use a lighter build config). Canonical line:

   ```
   # multisect/search tool COST ESTIMATE wall=<s>s cpu=<s>s basis='<...>'
   ```

2. **Actual wall and CPU on completion — always.** The whole search is wrapped in
   `ci-hub/bin/tool-cost`, and the orchestrator additionally prints its own line
   in a `finally` block, so the numbers appear even on failure/abort:

   ```
   # multisect/search tool COST ACTUAL       wall=<s>s cpu=<s>s ... exit=<code>
   # multisect/orchestrator tool COST ACTUAL wall=<s>s cpu=<s>s exit=<code>
   ```

   **CPU vs wall is diagnostic:** CPU ≫ wall means real parallel work; CPU ≈ 0
   with wall climbing means a spin/hang, not progress.

## How a probe is configured (`probes/<name>.env`)

A probe is a small env file (see `probes/detcore_misc.env`, the first customer).
Key knobs:

| var | meaning |
|---|---|
| `PROBE_REPO` | checkout that owns the `.git` objects to search (worktrees share them) |
| `PROBE_POOL_DIR` | where per-commit worktrees live (under `ignored/`, auto-cleaned) |
| `PROBE_BUILD_ARGV` | build command that compiles the test **without running it** |
| `PROBE_BUILD_ENV` | build env — the **C3** cost-floor profile lives here (below) |
| `PROBE_TEST_BIN_GLOB` | glob under `target/` matching the compiled test binary |
| `PROBE_TEST_ARGV` | args to the test binary (filter / `--exact` / `--test-threads`) |
| `PROBE_TEST_TIMEOUT` | inner hang-detector timeout — bounds the **test only** |
| `PROBE_BOX_CORES`, `PROBE_BOX_MEM` | cgroup caps for the box |

**Build-vs-hang separation (correctness-critical):** only the *test* is wrapped
in `PROBE_TEST_TIMEOUT`. The build runs outside it, and the box's outer wall
timeout is a generous safety net (`ceil(cold_build×2.5 + hang + 30)`). A slow
cold build therefore can never be misread as a test hang.

**Probe exit → engine verdict:** `0`→PASS/green · `124`→hang→WEDGED ·
other-nonzero→WEDGED · `125`→build-failed (WEDGED, but a `build-failed-<sha>`
marker makes the orchestrator **warn** that the blame may be a compile break, not
the target bug).

## Checkout strategy: registered validation slots

Each repetition asks `wrkslots` for a validation checkout at the exact commit.
The per-commit Cargo target directory stays under `multisect/ignored/`, outside
the checkout, so warm repetitions retain build artifacts while every Git
worktree remains registered and is removed through the same lifecycle record.
The existing per-commit `flock` still prevents two repetitions from sharing a
writable build directory. Full data behind the original linked-worktree choice,
alternatives considered, and the build-weight table:
[`experiments/multisect-build-weight_20260803/`](../experiments/multisect-build-weight_20260803/).

## Build weight: the C3 profile (recommended)

```
CARGO_INCREMENTAL=0
CARGO_PROFILE_DEV_DEBUG=line-tables-only
CARGO_PROFILE_DEV_SPLIT_DEBUGINFO=unpacked
```

Measured on the reference host (`detcore_misc`, `cargo test -p hermit-detcore --test
tests_misc --no-run`):

| config | cold wall | cold CPU | `target/` | test binary | backtraces |
|---|---|---|---|---|---|
| default dev `debug=2` | 24.0 s | 162 CPU-s | 1.6 GB | 210 MB | full |
| **C3 (recommended)** | **18.1 s** | **129 CPU-s** | 910 MB | **68.7 MB** | **file:line** |

C3 is strictly cheaper while **keeping usable backtraces** (this bug class needs
them). Warm reps on the same commit are ~0.4 s cargo no-ops, so a high rep count
(needed to amplify a rare hang) is cheap after rep 1. ~0.92 GiB disk per cold
probe; peak pool ≈ `(k+2)` worktrees, cleaned at the end — far under the 200 GB
cap. The naive full-workspace `target/` (65 GiB/probe) would blow the cap at 3
probes; the C3 + per-worktree-`target/` decision is what makes multisect fit the
disk budget.

## Dependency: the `box` subcommand (coordinate with hermit-multisect)

The probe runs inside `safe-ci-dag-runner box` (cgroup mem + wall timeout + core
cap). **`box` is currently unmerged** — it exists only in the
`scidr-rtc-agent-utils` WIP source. Until it lands, point multisect at a
box-capable build:

```bash
SAFE_CI_DAG_RUNNER_BIN=/path/to/box-capable/safe-ci-dag-runner \
  ./multisect/multisect run --probe detcore_misc --good <sha> --bad <sha>
# or:  ./multisect/multisect run ... --box-runner /path/to/safe-ci-dag-runner
```

`resolve_runner()` checks (in order) `--box-runner`, `SAFE_CI_DAG_RUNNER_BIN`,
`PATH`, then known `agent-utils/rs` build locations.

## Layout

```
multisect/
├── multisect            # orchestrator: run | estimate | calibrate | gc
├── multisect-probe      # the token-free probe unit (checkout+build+run)
├── lib/
│   ├── estimate.py      # ETA-to-blame; ProbeCost/SearchShape/Estimate; cost lines
│   └── anchors.py       # ci-hub known-green anchor provider (fail-safe)
├── probes/
│   └── detcore_misc.env # first customer: the reap-hang search
├── ignored/             # build caches + run outputs (gitignored)
├── README.md
└── DESIGN.md            # rationale: automation boundary, checkout, cost model
```

## Exit codes (from the engine)

`0` converged (blame localized) · `2` error · `3` infra · `4` ambiguous (no
adjacent high→low green-rate step met the thresholds — often *correct* when the
range has no reproducible regression).
