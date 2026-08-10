# Schedule Search Usage Guide

Status: event-level `hermit analyze` is operational on current Hermit. It is an
experimental diagnostic, not a proof that only one machine instruction or
memory access causes a race.

## What it does

`hermit analyze` needs two executions of the same command:

- a target execution that matches an exit-code, stdout, or stderr predicate;
- a baseline execution that does not match.

It records normalized schedule-event blocks, constructs an adjacent-swap path
between the opposite-outcome schedules, executes a midpoint, and replaces one
endpoint. It repeats until the event-order boundary is one adjacent swap apart,
then reruns the final endpoints with stack capture and emits a report.

This searches one linear path through schedule orderings. It does not enumerate
all schedules, model weak memory, or directly observe racing loads/stores.

## Prerequisites

- x86-64 Linux.
- A debuggable guest binary with symbols for useful stack/source output.
- User-space PMU RCB access for precise replay.
- A stable, machine-checkable target predicate.
- Frozen filesystem/network inputs or a workload that does not depend on them.
- An external timeout; automatic target search has no built-in run/time bound.

Preflight the PMU and build the CLI plus the example race:

```bash
perf stat -e branches:u -- /bin/true
cargo build -p hermit --bin hermit
cargo build -p hermetic_infra_hermit_flaky-tests --bin hello_race
```

## Recommended workflow

### 1. Establish a deterministic baseline

```bash
target/debug/hermit run --base-env=empty -- target/debug/hello_race
```

Record the command, environment, output, and exit status. A useful baseline
must not already satisfy the target predicate.

### 2. Find and replay a failing chaos seed

For the validated `hello_race` fixture, seed 1 failed and seed 0 passed on the
audited host:

```bash
target/debug/hermit run \
  --base-env=empty \
  --chaos \
  --seed=1 \
  --preemption-timeout=400000 \
  -- target/debug/hello_race
```

Seeds are workload and environment evidence, not universal constants. Repeat
the seed and verify that output/status is stable before diagnosis. For a
failing guest, `hermit-verify --allow-nonzero-exit run ...` has more useful
automation exit semantics than relying on `hermit run --verify`, which can
report successful verification but still return the guest's nonzero status.

### 3. Analyze explicit endpoints

Explicit passing/failing seeds are preferred over an unbounded automatic
search:

```bash
mkdir -p "$PWD/analyze-artifacts"

timeout 300 target/debug/hermit analyze \
  --analyze-seed=0 \
  --run1-seed=1 \
  --run2-seed=0 \
  --target-exit-code=nonzero \
  --report-file="$PWD/analyze-artifacts/report.json" \
  --tmp-dir="$PWD/analyze-artifacts/work" \
  -- \
  --base-env=empty \
  --chaos \
  --summary \
  --preemption-timeout=400000 \
  -- target/debug/hello_race
```

`--analyze-seed` makes the analyzer's own random decisions reproducible.
`--run1-seed` must match the target predicate; `--run2-seed` must not. If the
baseline seed is omitted, analyze uses the non-chaos run as the baseline.

Target options are:

- `--target-exit-code=<number|nonzero|any>`;
- `--target-stdout=<regex>`;
- `--target-stderr=<regex>`.

The default target is a nonzero exit. Use output predicates for logical bugs
that return success.

### 4. Use bounded automatic search only when needed

```bash
timeout 600 target/debug/hermit analyze \
  --analyze-seed=0 \
  --search \
  --target-stdout='unexpected value' \
  --report-file="$PWD/analyze-artifacts/report.json" \
  -- \
  --chaos \
  --summary \
  --preemption-timeout=400000 \
  -- /absolute/path/to/program arg1 arg2
```

The internal `--search` loop is currently unbounded. Always impose a process
timeout and retain the printed failing seed. `--imprecise-search` can discover
some failures much faster, but final schedule replay still needs precise PMU
behavior.

### 5. Interpret the result conservatively

A successful report contains:

- target and baseline schedule artifacts;
- the two critical event indexes and event context;
- a stack for each side when symbols/unwinding succeed;
- a JSON report when `--report-file` is supplied.

The validated E2E cases were:

| Fixture | Result |
| --- | --- |
| `hello_race` | 6,088,241-swap endpoints converged in 22 passes; both stacks reached `flaky-tests/hello_race.rs:37` |
| `racewrite_nostdlib` | Converged in six passes with zero desyncs; adjacent write posthooks reached `tests/c/simple/racewrite_nostdlib.c:35` |

The conclusion is "this event ordering distinguishes these observed
outcomes," not "these are the exact racing memory accesses." Re-run both final
schedules and confirm the predicate before acting on the stacks.

## Existing test driver

`tests/util/hermit_analyze_test.sh` accepts an absolute Hermit path and guest
path. It uses `--analyze-seed=0`, automatic search, chaos mode, a 400,000-RCB
preemption timeout, a temporary report, and verifies that stacks are printed.

```bash
timeout 300 tests/util/hermit_analyze_test.sh \
  "$PWD/target/debug/hermit" \
  "$PWD/target/debug/hello_race"
```

The broader two-fixture CI wrapper landed on `main` via
[Hermit PR #7](https://github.com/rrnewton/hermit/pull/7) (merged
2026-07-22); do not treat that as settled evidence that its hardware-lane
CI is currently green — reverify the self-hosted PMU-dependent run's live
status separately before relying on it.

## Known limitations

- Replay jitter can cause a midpoint request and realized schedule to differ.
  Current search can continue optimistically and attach an outcome to the
  requested route. Large desync/jitter warnings weaken the diagnosis.
- `swap_distance == 1` does not prove edit distance 1. Inserted or deleted
  events can remain, especially in loops or split RCB blocks.
- Exact block matching and greedy duplicate matching align repeated events
  poorly.
- Partial branch-count consumption, blocked-thread behavior, and switching at
  syscall prehooks need stronger rules.
- `--run1-schedule` and `--run2-schedule` are exposed but unimplemented.
- `sub_event_search` has remained disabled. Its bounds, applicability, jitter,
  and final A/B validation are not ready.
- There is no repeated-predicate stability sampling.
- Branch-free instruction regions cannot be subdivided with current events.
- The analyzer retains substantial temporary state and can produce noisy logs.

## Operational recommendations

- Prefer explicit known seeds or preemption endpoints.
- Use `--analyze-seed` and an external timeout every time.
- Put artifacts under a host-visible absolute path; guest `/tmp` is normally
  isolated.
- Run one PMU-heavy analyzer per physical host unless hardware measurements
  show safe concurrency.
- Capture Hermit SHA, kernel, CPU model, perf policy, guest binary digest,
  command, environment, seeds, and final schedules with the report.
- Reject a diagnosis when final endpoint replay is unstable.
- Use KCSAN/TSAN/assertions/output/exit status as the target oracle; Hermit
  supplies schedule control, not a memory-race detector.

## Completion roadmap

The next validity milestone is realized-schedule checking, stable event
identities/alignment, preserved opposite-outcome endpoints, bounded target
search, and repeated predicate verification. Only then should branch-level
refinement be repaired and enabled. The research estimate is 4-6 weeks for a
credible PMU-backed MVP and 8-12 weeks for a supported feature.
