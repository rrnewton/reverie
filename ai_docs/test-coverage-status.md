# Hermit Cargo Test Coverage Status

Date: 2026-07-21

## Scope And Baseline

This report compares the tests reachable from the maintained fork's current
`main` branch with the expanded Buck test graph in fbsource.

| Input | Revision |
| --- | --- |
| `rrnewton/hermit` | `fbb6771a8c05f27f06f85219448ef7acb04810c1` |
| fbsource | `851564d2da7cccc1af085fa6d65cead1d0f489a0` |
| Buck package | `fbcode//hermetic_infra/hermit/...` |

The audit used the live Buck expansion, not a count of macro calls. This
matters because `tests/BUCK` expands workload lists into hundreds of
mode-by-workload `sh_test` targets.

The main local commands were:

```bash
cargo test --workspace
cargo test --workspace -- --list --format terse
buck2 targets fbcode//hermetic_infra/hermit/... --json \
  --output-attribute '^buck.type$' \
  --output-attribute '^buck.package$' \
  --output-attribute '^name$' \
  --output-attribute '^test$'
```

The full Cargo workspace passed on the audit host at `8ec63a5`, which has
working PMU and CPUID support. Main advanced during the audit: `74324ff` added
one passing no-hardware test and `fbb6771` changed only CI. The updated counts
below include that test. Another agent's concurrent uncommitted stress and
vfork files were explicitly excluded.

## Counting Rules

Cargo and Buck expose different units and must not be compared as if they were
the same:

- A **Cargo case** is one named `#[test]` or doctest reported by `cargo test
  -- --list`. One Cargo integration case may loop over several workloads.
- A **Buck target** is one expanded `rust_test`, `sh_test`, `cxx_test`, or
  `python_test`. A Buck `rust_test` may contain dozens of Rust test functions.
- A Cargo guest binary with a zero-test harness is compile coverage, not proof
  that its `main` function ran.
- A semantic replacement test is useful coverage, but it is not counted as a
  direct Buck target port unless it runs the same workload in the same mode.

This distinction explains why both of these statements are true:

- Cargo currently reports 176 named cases.
- Only a small fraction of the 744 Buck executable targets has a direct
  equivalent on reachable `main`.

## Executive Summary

- `cargo test --workspace` exposes **176 named cases**: **174 passed** and **2
  Hermit doctests were ignored**. The ignored cases are documentation examples,
  not PMU tests.
- Buck expands to **745 test rules**: **744 executable test targets** plus one
  `test_suite`.
- The executable Buck targets are **714 shell**, **24 Rust**, **3 C++**, and
  **3 Python** targets.
- All **24 Buck Rust test targets** have a Cargo build/test-harness equivalent
  on main.
- Only **11 of 714 Buck shell targets** have a direct workload/mode equivalent
  on main: five default-mode workload runs and six record/verify workloads.
- Therefore **35 of 744 Buck executable targets (4.7%)** have a direct main
  representation. This target-level percentage understates the Rust unit
  logic covered inside the 24 Rust harnesses, but accurately reflects the
  missing end-to-end matrix.
- Current main has **no PMU tests marked ignored**. Its hardware tests ran and
  passed locally. The proposed ignore split exists only in open PRs.
- Batch B is an open green upstream PR with two regular Cargo cases and one
  ignored PMU case. Batch C is an open, now-conflicting upstream PR containing
  all 78 lit targets and PMU annotations. Neither is on `rrnewton/hermit:main`.
- The claimed wave-two 56-test port was never committed to its branch. Its
  removed worktree survives only as unreachable pre-cleanup commit
  `f6d5fb9c8319100291e59579b9475548ae54ccf4`; it is not current coverage and is
  at risk of garbage collection.
- The largest gaps are the 219-test rr suite, 149 strict/chaos/chaos-replay
  targets, 79 trace/chaos-trace targets, 55 raw workload runs, 39 unported
  record workloads, and all three `hermit analyze` targets.

The earlier informal "300+ tests ported" figure is not supported by any
reachable branch. It combined planned generated targets, semantic replacement
tests, open PRs, and uncommitted wave-two work. Planning should use the
reachable-main and pending-work counts in this report instead.

## Current Cargo Baseline

The 176 named cases break down as follows:

| Cargo harness or family | Named cases | Result |
| --- | ---: | --- |
| Detcore library unit tests | 43 | 43 passed |
| Detcore misc integration | 3 | 3 passed |
| Detcore parallelism integration | 16 | 16 passed |
| Detcore time integration | 12 | 12 passed |
| Detcore model unit tests | 13 | 13 passed |
| Detcore testutils unit tests | 1 | 1 passed |
| Digest unit tests | 1 | 1 passed |
| Edit-distance unit tests | 22 | 22 passed |
| Flaky guest native unit test | 1 | 1 passed |
| `libhermit` unit tests | 7 | 7 passed |
| Hermit CLI binary unit tests | 14 | 14 passed |
| Hermit black-box CLI tests | 6 | 6 passed |
| Hermit mode integration tests | 6 | 6 passed |
| Record/replay matrix | 1 case / 6 workloads | passed |
| Hermit-verify unit tests | 21 | 21 passed |
| Hermit-verify CLI/trace fixture tests | 4 | 4 passed |
| Test allocator unit tests | 2 | 2 passed |
| Test allocator doctest | 1 | passed |
| Hermit doctests | 2 | 2 ignored |
| **Total** | **176** | **174 passed, 2 ignored** |

Several generated Cargo harnesses report zero tests. They prove that guest
sources compile but do not execute the guest `main`; they are not counted as
runtime coverage.

### Hardware Classification

Updating the earlier 160-case PMU audit for the 16 subsequently added
no-hardware CLI and regression cases gives:

| Cargo requirement | Cases | Share |
| --- | ---: | ---: |
| No hardware dependency | 150 | 85.2% |
| PMU only | 25 | 14.2% |
| CPUID faulting only | 1 | 0.6% |
| Both PMU and CPUID | 0 | 0% |

The PMU cases are the Detcore getrandom case, 11 generated Detcore
parallelism variants, all 12 Detcore time cases, and the record/replay matrix.
The CPUID-only case is `rdrand_rdseed_is_masked`. Capability probes may skip or
degrade on an unsupported host even though the intended behavior is hardware
sensitive.

### What Green CI Currently Means

The current GitHub workflow still runs less than the local workspace:

- The regular job excludes the `detcore`, `hermit`, and intentionally flaky
  packages from the general workspace command. It separately runs Hermit
  library/binary tests, Detcore library/binary tests, and one Detcore misc case.
- The hardware workflow at `fbb6771` was expanded to request all ignored
  Detcore PMU groups and the pending lit/chaos targets, but the first run is
  red: the self-hosted runner failed before testing because apt could not find
  `golang-go`. The regular job passed.
- The workflow and source are temporarily out of sync. Main has no ignored
  Detcore PMU cases yet, so the new `--ignored` commands select zero tests.
  Main also lacks the `detcore_lit` and `chaos_verify` targets referenced by
  later steps. Those steps would fail on a host where mount namespaces are
  available.
- The preceding green main run at `8ec63a5` reported that mount namespaces
  were unavailable, so it ran Hermit unit tests only. Hermit mode,
  record/replay, and black-box CLI integrations were not exercised.

Consequently, `./scripts/validate.rs` or `cargo test --workspace` on a capable host is
still the only complete main-branch run. Current main CI is not green.

## Original Buck Inventory

### By Package

| Buck package | Rust | Shell | C++ | Python | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| `common` | 2 | 0 | 0 | 0 | 2 |
| `common/test-allocator` | 2 | 1 | 0 | 0 | 3 |
| `detcore` | 6 | 2 | 0 | 0 | 8 |
| `detcore-model` | 1 | 0 | 0 | 0 | 1 |
| `detcore/tests/lit` | 0 | 78 | 0 | 0 | 78 |
| `flaky-tests` | 3 | 4 | 3 | 0 | 10 |
| `hermit-cli` | 2 | 3 | 0 | 0 | 5 |
| `hermit-verify` | 1 | 0 | 0 | 0 | 1 |
| `tests` | 7 | 626 | 0 | 3 | 636 |
| **Executable total** | **24** | **714** | **3** | **3** | **744** |

There is also one non-executable `test_suite` for the rr wrapping.

### By Hardware Requirement

| Buck requirement | Targets | Share |
| --- | ---: | ---: |
| No hardware dependency | 205 | 27.6% |
| PMU only | 538 | 72.3% |
| CPUID faulting only | 0 | 0% |
| Both PMU and CPUID | 1 | 0.1% |

At the shell/integration level, 536 of 714 targets (75.1%) need PMU for their
intended semantics. The single combined target is `detcore:tests_misc`, which
aggregates a PMU-sensitive case and the CPUID masking case.

## Port Status Categories

### Ported And Passing On Main

1. **All 24 Buck Rust test rules.** Cargo builds the same Rust libraries,
   binaries, and integration sources. These targets account for most of the
   174 Cargo logical cases.
2. **Five default-mode Buck workload targets:** `c_getpid`, `c_uname`,
   `c_sysinfo`, `c_wait_on_child`, and `c_nanosleep-par`. The Cargo runner adds
   portable-runner flags (`--no-virtualize-cpuid` and
   `--preemption-timeout=disabled`), so this is workload behavior parity rather
   than byte-for-byte command parity.
3. **Six record/verify Buck workload targets:** `c_getpid`, `c_uname`,
   `c_sysinfo`, `c_wait_on_child`, `c_nanosleep-par`, and
   `rs_clock_gettime`. One Cargo case loops over all six and asserts
   `Success: replay matched recording.`
4. **Net-new coverage:** ten black-box CLI/verifier tests, strict-option
   regressions, and verify tmp/environment regressions improve public behavior
   coverage but do not correspond to additional Buck target names.

The five-workload strict and chaos mode loops are useful smoke coverage, but
they explicitly disable PMU preemption. They are not counted as ports of the
Buck strict/chaos targets, which assert execution with branch-count
preemption.

### Ported But Ignored For PMU

Reachable main has **zero ignored PMU cases**. The only two ignored cases are
doctests.

The intended PMU split exists in unlanded work:

| Pending work | Regular | PMU ignored | Validation |
| --- | ---: | ---: | --- |
| Batch B, upstream PR #72 / `0a62ce5` | 2 | 1 | regular tests and PR CI passed; hardware job skipped |
| Batch C, upstream PR #75 / `ea53f08` | 51 lit | 27 lit + 11 parallel + 12 time | all lit and time PMU cases passed locally; 3/11 parallel PMU cases completed |

Batch C's eight memory-race PMU cases were blocked by unfiltered timer traces.
The fix landed on main as `74324ff`. The full audit run at the preceding commit
still produced extremely large instruction-trace output; the landed fix makes
future full hardware validation practical, but does not by itself add the
missing ignored test selection.

### Ported But Not Integrated

| Work | State | What is recoverable |
| --- | --- | --- |
| Batch B chaos/verify | Upstream PR #72 is open, clean, and regular-CI green | Two no-PMU semantic tests and one ignored PMU branch-preemption test |
| Batch C Detcore/lit | Upstream PR #75 is open but conflicts with its base and has no current code CI | Exact 78-target lit harness, fixture dispatcher, and PMU annotations |
| Wave-two no-PMU port | No branch or worktree contains the changes | Unreachable pre-cleanup commit `f6d5fb9` contains a 489-line diff adding 56 named Cargo tests |

The wave-two artifact covered 46 of 55 default-mode workload targets in total
(including the five already on main), several no-PMU lit cases, environment and
tmpfs behavior, network-bind behavior, and two stacktrace scenarios. It must be
attached to a ref immediately if it is to be preserved, then rebased and
reviewed against current main. It must not be counted until that happens.

### Not Directly Portable Because Of fbsource Dependencies

1. **219 rr suite targets.** `wrap_test_suite` imports `RR_TEST_TARGETS` from
   `//hermetic_infra/common`, and the test binaries live outside the public
   Hermit tree. Porting requires a licensed/exported rr test corpus, a
   standalone build for those binaries, a maintained exclusion list, and a
   compatible namespace/PMU runner. These cannot be reproduced from the
   current public checkout alone.
2. **Three C++ unit targets:** `bind_same`, `bind_random`, and
   `use_configurable_flaky_service`. They depend on fbsource GoogleTest;
   `bind_random` also uses Folly random, while the service test uses Folly
   formatting and libcurl. Their sources are exported, but Cargo cannot build
   the original dependency graph. Replace Folly with standard facilities and
   add a small CMake/cc test harness, or rewrite the assertions as Rust/Cargo
   integration tests.
3. **Three Python type-check targets:** `pythonbin_hello`, `pythonbin_rand`, and
   `pythonbin_timed_progress_bar`. Buck supplies fbsource Python packaging and
   library type-checking. The scripts are exported, but Cargo has no equivalent
   target. Add an explicit Python syntax/typecheck job and run the scripts with
   system Python if these checks remain valuable.

There are no executable test targets under `fb-only/BUCK`; the blockers above
come from dependencies outside the exported tree rather than test sources in
that directory.

## Generated Matrix Crosswalk

The core `tests/BUCK` matrix accounts for 557 of the 626 targets in that
package:

| Buck family | Targets | Main status | What remains |
| --- | ---: | --- | --- |
| `raw_run__*` | 55 | 0 direct runs | Add a Cargo guest registry and execute every guest `main` directly; compiling a zero-test harness is insufficient. |
| `hermit_run_default__*` | 55 | 5 direct workload runs | Recover wave two to reach 46/55, then add `vforkExec`, five shell cases, and three Python cases. |
| `hermit_run_strict__*` | 51 | 5 PMU-disabled smoke workloads; 0 full equivalents | Reuse the guest registry on the hardware runner with PMU enabled and isolate each case. |
| `hermit_run_chaos__*` | 51 | 5 PMU-disabled smoke workloads; 0 full equivalents | Add PMU-enabled bounded chaos runs and deterministic seeds. Batch B supplies a better semantic starting point. |
| `hermit_run_chaosreplay__*` | 47 | none | Record and replay schedules for the supported guest subset on the hardware lane. |
| `hermit_run_tracereplay__*` | 43 | no end-to-end Cargo run | Hermit-verify fixture tests cover parsing/comparison only. Add guest trace recording and replay. |
| `hermit_run_tracereplay_chaos__*` | 36 | none | Add chaos trace record/replay after the regular trace runner is stable. |
| `hermit_record_*` | 45 | 6 direct workload runs | Expand the existing record matrix across the remaining 39 workloads. |
| `test_hermit_strict__rr_*` | 219 | none | Requires the unavailable rr corpus and build graph. |

### Workload Inventory For Raw/Default Modes

The 55 common workload names are the complete per-target inventory for raw and
default mode:

- **C (19):** `clone`, `getCpu`, `getpid`, `hello_alarm`, `hello_signals`,
  `just_spin`, `memoryPress`, `nanosleep-par`, `print_memaddrs`,
  `printf_with_threads`, `sigtimedwait-no-timeout`,
  `sigtimedwait-timeout-0s`, `sigtimedwait-timeout-1s`, `sysinfo`,
  `sysinfo_uptime`, `threadExhaustion`, `uname`, `vforkExec`, and
  `wait_on_child`.
- **Rust (25):** `bind_connect_race`, `clock_gettime`, `clock_total_order`,
  `exit_group`, `futex_and_print`, `futex_timeout`, `futex_wait_child`,
  `futex_wake_some`, `heap_ptrs`, `interrogate_tty`, `mem_race`, `nanosleep`,
  `network_hello_world`, `pipe_basics`, `poll`, `poll_spin`,
  `print_clock_nanosleep_monotonic_abs_race`,
  `print_clock_nanosleep_monotonic_race`,
  `print_clock_nanosleep_realtime_abs_race`, `print_nanosleep_race`, `rdtsc`,
  `sched_yield`, `socketpair`, `stack_ptr`, and `thread_random`.
- **Shell (7):** `curl`, `date`, `devrand`, `par_work`, `py_hello`, `race`, and
  `taskset`.
- **Python (3):** `hello`, `rand`, and `timed_progress_bar`.
- **Custom binary (1):** `minimal_hello`.

Wave two covered all 25 Rust workloads, 18 of 19 C workloads, two shell
workloads, and the custom binary in default mode. It did not add raw-mode
execution.

### Remaining Record Workloads

Main covers five C workloads and `rs_clock_gettime`. The other 39 are:

- **C (14):** `clone`, `getCpu`, `hello_alarm`, `hello_signals`, `just_spin`,
  `memoryPress`, `print_memaddrs`, `printf_with_threads`,
  `sigtimedwait-no-timeout`, `sigtimedwait-timeout-0s`,
  `sigtimedwait-timeout-1s`, `sysinfo_uptime`, `threadExhaustion`, and
  `vforkExec`.
- **Rust (24):** every Rust workload in the preceding inventory except
  `clock_gettime`.
- **Custom (1):** `minimal_hello`.

The existing record matrix can be extended rather than replaced. The guest
build registry from the wave-two artifact would remove most of the fixture
work.

## Special Shell Targets

### Inside `tests` (69)

| Family | Count | Main status | Port requirement |
| --- | ---: | --- | --- |
| Record workloads | 45 | 6 covered | Expand the current record matrix; use hardware for reliable branch scheduling. |
| Analyze | 3 | none | Run `hermit analyze` for `hello_race`, `nanosleep-threads-nocrash`, and `racewrite_nostdlib`; assert backtraces/blame. Needs bounded chaos and symbolization. |
| Chaos stress | 5 | none exact | Add bounded hardware tests for `cas_sequence`, `hello_chaos`, `keyvalue`, `lock_granularity`, and `order_violation`. Batch B partially covers the first two semantics. |
| Trace-replay split | 1 | none | Port `cbin_just_spin_trace_replay_split` with its 10M timeout. |
| Standalone scripts | 9 | none on main | Port env/tmp/backtrace/replay cases. Replace `curl_client_only` external networking with a local fixture. Wave two recovers three of these. |
| No-network bind | 2 | none on main | Compile both network-bind guests and run the shared script; wave two recovers the smaller case only. |
| Stacktrace-events matrix | 3 | none | Compile the stacktrace tool and three guests; the `cas_sequence` case needs PMU. |
| Verify-replay | 1 | none | Port `nanosleep-threads-simple` schedule recording/replay and normalized comparison. |

### Outside `tests` (88)

| Target family | Count | Main status | Port requirement |
| --- | ---: | --- | --- |
| Detcore lit | 78 | absent; exact port in conflicting PR #75 | Rebase/retarget the PR, resolve generated-manifest and fixture changes, then run 51 regular and 27 ignored PMU cases. |
| Allocator `run_test_bin` | 1 | binary compiles but `main` does not run | Add a Cargo integration test that launches `CARGO_BIN_EXE_test_bin`. |
| Detcore musl build | 1 | absent | Wrap the exported archive/script in an ignored slow integration test with `make`, cc, and sufficient timeout. |
| Detcore syscall-support reporter | 1 | absent, intentionally invisible to Buck testpilot | Treat as a tool, not parity debt, or add a smoke test for its output schema. |
| Flaky shell runners | 4 | only `hello_race` native unit logic runs | Add an opt-in flaky target; keep nondeterministic exits out of regular CI and provide a local-only server fixture. |
| Hermit CLI record smoke | 3 | absent | Add uptime and urandom recording cases; put the GDB case in an ignored/self-hosted lane with explicit GDB dependency. |

## Feature Coverage

| Feature | Current Cargo status | Assessment |
| --- | --- | --- |
| `hermit run` default | Five C workloads plus CLI parsing | Partial. Broad source inventory exists but wave-two execution is not landed. |
| Strict execution | Five PMU-disabled workloads; Detcore PMU tests cover lower layers | Partial. No PMU-backed Hermit workload matrix on main. |
| Record | Six representative workloads run `record start --verify` | Useful but narrow: 6/45 Buck record workloads. |
| Replay | Exercised indirectly by record verification; replay CLI help is tested | Partial. No direct recording selection/lifecycle matrix and no standalone trace replay. |
| Chaos | Five PMU-disabled workloads and one race/verify smoke | Partial. No branch-preemption stress on main; Batch B is unlanded. |
| Verify | Five workload comparisons plus tmp/environment regression | Good smoke coverage, but not the full strict/chaos matrix. |
| Trace replay | Parser/diff fixture tests only | Major gap. No end-to-end guest trace replay on main. |
| Chaos replay | Help/fixture-level exposure only | Major gap. No end-to-end chaos replay on main. |
| Analyze | Command appears in help tests only | Critical gap. All three Buck analyze targets are missing. |
| CLI contracts | Help, option conflicts, JSON listing, strict flags, and replay parsing | Strong relative to Buck; much is net-new coverage. |
| Detcore logic | Unit, misc, parallelism, and time suites all pass locally | Strong locally, weak in CI because only one PMU smoke case runs there. |
| Syscall/file behavior | Mostly lower-layer units and five C workloads | Weak until lit PR #75 lands. |
| Networking/isolation | No end-to-end Cargo case on main | Gap; wave two and the lit port contain recoverable coverage. |
| Stacktraces/signals | Unit logic only on main | Gap; wave two contains two recoverable scenarios. |

## Priority Gaps And Recommended Order

1. **Preserve wave two immediately.** Create a durable branch/ref for
   `f6d5fb9` before Git garbage collection, then rebase its two-file diff onto
   current main and rerun all 56 cases. This is the cheapest large coverage
   recovery.
2. **Repair main CI and align it with reachable tests.** Fix the self-hosted Go
   package installation. Until Batch B/C land, do not present zero selected
   `--ignored` runs or absent Cargo targets as coverage.
3. **Retarget and integrate Batch C.** Resolve PR #75 against the maintained
   fork. It supplies exact parity for all 78 lit targets and the intended
   regular/hardware split.
4. **Retarget and integrate Batch B.** Its three semantic tests cover chaos
   seed behavior, deterministic verify outcomes, schedule replay, and a PMU
   branch-preemption assertion.
5. **Build one reusable guest/mode matrix.** Use a Cargo-managed guest registry
   for the 55 workloads and parameterize raw, default, strict, chaos,
   chaos-replay, trace-replay, and record modes. Keep PMU modes ignored by
   default and explicit on the hardware lane.
6. **Make hardware CI representative.** Run all ignored Detcore PMU cases and
   the Hermit integration targets on a runner with PMU, CPUID faulting, and
   working mount namespaces. A green unit-only fallback should remain visible
   as reduced coverage, not be mistaken for the full lane.
7. **Add analyze and trace-replay end-to-end tests.** These are user-facing
   debugging features with essentially no current runtime coverage.
8. **Decide whether rr parity is a product goal.** Importing 219 targets is a
   separate dependency/export project. If it is not a goal, explicitly remove
   them from the parity denominator and document the supported public matrix.
9. **Port the small special targets.** Allocator execution, network bind,
   uptime/urandom record smoke, and stacktrace scripts are inexpensive once the
   common runner exists.
10. **Handle non-Rust tests deliberately.** Replace fbsource-only Folly,
    GoogleTest, and Python build dependencies or classify those six checks as
    intentionally internal-only.

## Bottom Line

Main has a healthy and passing Rust unit-test base plus useful smoke coverage
for run, record/replay, verify, and CLI behavior. It does **not** yet approach
the original Buck end-to-end matrix. Most of the gap is concentrated in a few
repeatable families rather than hundreds of unrelated implementations:

- recover and land the existing wave-two, Batch B, and Batch C work;
- add a single reusable guest/mode runner;
- restore practical PMU output and expand the self-hosted lane;
- make an explicit decision about the external rr corpus.

Those steps would convert the current fragmented work into measurable Cargo
parity without pretending that Cargo logical case counts and Buck target counts
are interchangeable.
