# Validate test prerequisite and selection audit

Date: 2026-08-27

Audited Hermit commit: `7ddb3559a09f2709a67ce2b605426b43c33f070f`

This was a read-only audit. The Hermit repository was not modified. Commands were run from the repository root unless a path says otherwise. Host probes were run outside the restricted tool sandbox so they describe `devbig014`, not the sandbox.

## Bottom line

Hermit does not currently have one machine capability result that drives all test selection.

The current stack has several independent mechanisms:

1. workflow runner labels and shell preflight checks;
2. the `portable` / `privileged` DAG split;
3. `scripts/validate.rs` host probes for only `cpuid-faulting` and `kvm`;
4. E2E manifest `requires` metadata, of which only `cpuid` currently drives a host probe;
5. Cargo/nextest filters and Rust `#[ignore]` attributes;
6. Rust tests that return early when a device, feature, artifact, tool, or sysfs file is absent;
7. shell tests that print `SKIP` and exit 0; and
8. Hermit/Reverie runtime checks that can disable PMU preemption after a weaker probe.

Those layers overlap but do not share one complete answer. KVM, CPUID faulting, PMU access, mount/user namespaces, optional backends, tools, and host files are each checked in more than one place, with different tests and different meanings.

The owner's desired rule, “a commit is green only after a FULL validate with nothing skipped,” is not the current rule. A qualifying FULL receipt can presently coexist with:

- Rust tests excluded by command-line filters;
- Rust `#[ignore]` tests;
- tests that return early and are counted as passing;
- nextest tests reported as skipped and rewritten as `filtered out`;
- 371 enabled E2E cells outside the ordinary Required population;
- a mixed E2E bucket in which some selected cells are `HOST-INAPPLICABLE`; and
- successful test nodes that emit no parseable test-count banner.

The immediate PMU finding is also clear: the eight `chaos_buck_*` tests have been statically ignored since 2026-07-21, including on capable machines. On `devbig014` today, six pass, one fails for a real `nanosleep`/`EINTR` assertion, and one does not finish within five minutes under Hermit even though its guest finishes natively in 3.62 seconds. The right change is dynamic selection from a real PMU capability result, followed by fixing the two failing cases; it is not simply deleting all eight ignores and declaring victory.

## Terms used in this report

These states must remain distinct because they imply different evidence:

| State | What happened | Current example |
| --- | --- | --- |
| Test ignored | libtest knows the test but does not run its body without `--ignored` | `#[ignore]` on the eight `chaos_buck_*` tests |
| Test filtered | the test ID is excluded by a Cargo/nextest selector | `test.hermit_modes` passes `--skip default_ --skip chaos_buck_ ...` |
| Test returned early | libtest ran the test function and reports success, but the intended work did not execute | many `/dev/kvm` guards in `cli.rs` |
| Cell omitted | an enabled E2E cell is not in the Required population, usually because `ci=false` or `occasional=true` | 371 of 679 enabled cells |
| Cell host-inapplicable | the harness selected the cell but withheld execution after a capability probe | `HOST-INAPPLICABLE` |
| Node omitted by profile/selection | the DAG node is absent from the plan | `portable-only`, `--only`, or `--selective` |
| Node dependency-skipped | the node was planned but never ran because a dependency failed | recorded separately by validate |
| Run refused | validate produced no product verdict because admission or execution conditions were not met | dirty tree, full local off-record run, malformed plan, stale base |

Only the last item is reliably a non-verdict. Several of the other states can currently sit under a green node or qualifying receipt.

## Quantitative inventory

### Checked-in DAGs and dynamically built plans

There are two checked-in DAG JSON files:

- `ci/dag/portable.json`: 59 nodes
- `ci/dag/privileged.json`: 10 nodes

Together they contain 69 declared nodes: 20 `test.*` nodes and 15 E2E manifest bucket nodes. No current node has an explicit `skip_reason`. FULL selects both lanes (`scripts/validate.rs:5213-5218`) and marks only an unfocused FULL plan as `suite_complete` (`scripts/validate.rs:5424-5433`).

Command:

```bash
jq -s '{portable_total:(.[0].steps|length),privileged_total:(.[1].steps|length),total:([.[].steps[]]|length),portable_test:(.[0].steps|map(select(.group=="test"))|length),privileged_test:(.[1].steps|map(select(.group=="test"))|length),manifest_nodes:([.[].steps[]|select(.group=="e2e" and (.job|startswith("manifest_")))]|length),skip_reason_total:([.[].steps[]|select(.skip_reason!=null)]|length)}' ci/dag/portable.json ci/dag/privileged.json
```

Result:

```text
portable_total=59 privileged_total=10 total=69
portable_test=19 privileged_test=1 manifest_nodes=15 skip_reason_total=0
```

The files are not the whole runtime graph. `scripts/validate.rs` adds preflight and scorecard nodes, fuses the two lanes, rewrites dependencies, and deduplicates identical work (`scripts/validate.rs:4854-4920`, `:5279-5435`). The ordinary fused FULL plan observed during this audit had 75 runtime nodes after four duplicate lane nodes were removed.

Command:

```bash
./scripts/validate.rs --show-plan \
  --skip-inner-dirty-working-tree-and-rebase-freshness-checks
```

The shared checkout advanced to `92b7fc3823826961eadaa4bbcbc75e0a65b6a0fc` before this last display command. `git diff --name-only 7ddb3559... 92b7fc382...` showed changes only to `AGENTS.md` and two skill files, so the DAG and planner content producing the 75-node result was byte-identical to the audited commit.

There is also one nested validate invocation in FULL: `test.strict_compat` runs `./scripts/validate.rs --portable-strict-compat-only` (`ci/dag/portable.json:619-627`). That inner invocation loads the compatibility corpus and creates one `compat.*` DAG node per selected program (`scripts/lib/validate_plan.rs:934-1020`). The current generated inner graph is 193 nodes. `ci/super/gates.json` is another checked-in table from which validate builds super gates, but it is not a third `ci/dag/*.json` file.

### E2E manifest population

There are 14 YAML files in `tests/e2e/manifests/`: 13 bucket documents plus `defaults.yaml`, defining 359 test recipes.

Commands:

```bash
find tests/e2e/manifests -maxdepth 1 -type f -name '*.yaml' -printf '%f\n' | sort
rg -n '^\s*- id:' tests/e2e/manifests/*.yaml | wc -l
target/debug/hermit-manifest-plan --format matrix-json | jq '{total:length,enabled:([.[]|select(.enabled)]|length),ci:([.[]|select(.enabled and .ci)]|length),disabled:([.[]|select(.enabled|not)]|length)}'
target/debug/test-harness plan --format json | jq '{selected:length,portable:([.[]|select(.lane=="portable")]|length),privileged:([.[]|select(.lane=="privileged")]|length),modes:([.[].mode]|group_by(.)|map({key:.[0],value:length})|from_entries),backends:([.[].backend]|group_by(.)|map({key:.[0],value:length})|from_entries)}'
target/debug/test-harness plan --include-occasional --format json | jq length
jq '.cells|length' ci/expected-e2e-plan.json
```

Results:

| Population | Count | Share |
| --- | ---: | ---: |
| All declared test × mode × backend combinations | 7,539 | 100% |
| Enabled combinations | 679 | 9.0% of all |
| Enabled and marked `ci=true` | 309 | 45.5% of enabled |
| Ordinary Required plan used by FULL | 308 | 45.4% of enabled; 4.1% of all |
| Enabled but absent from ordinary FULL | 371 | 54.6% of enabled |
| Disabled / not applicable | 6,860 | 91.0% of all |

The 371 enabled omissions are 370 `ci=false` cells plus the one `occasional: true` KVM shell cell. Required selection accepts `ci && enabled` and omits occasional tests unless explicitly requested (`ci/manifest-plan/src/runner.rs:360-479`; `applications/kvm-shell-environment` at `tests/e2e/manifests/applications.yaml:188-196`). All 15 DAG manifest nodes invoke the harness with `--ci-only --allow-empty` (`ci/dag/portable.json:218-376`, `ci/dag/privileged.json:72-95`).

The 308 ordinary Required cells are:

| Axis | Counts |
| --- | --- |
| Lane | 306 portable, 2 privileged |
| Mode | 299 verify, 5 chaos, 3 custom, 1 replay |
| Backend | 244 ptrace, 57 SaBRe, 6 LiteInst, 1 KVM |

Mode totals over all 7,539 combinations:

| Mode | Total | Enabled | CI-selected | Disabled/not applicable |
| --- | ---: | ---: | ---: | ---: |
| verify | 1,795 | 635 | 300 | 1,160 |
| chaos | 1,795 | 7 | 5 | 1,788 |
| replay | 1,795 | 1 | 1 | 1,794 |
| custom | 1,795 | 3 | 3 | 1,792 |
| naked | 359 | 33 | 0 | 326 |

`ci/ci-reason-baseline.json` contains 927 grandfathered `ci=false` mode rows with no enabled backend and no reason. `ci/matrix-symmetry-baseline.json` records two asymmetric manifest tests and 28 backend-private guest files.

Commands:

```bash
jq '.unreasoned_ci_false_cells|length' ci/ci-reason-baseline.json
jq '{asymmetric_manifest_tests:(.asymmetric_manifest_tests|length),backend_private_guest_files:(.backend_private_guest_files|length)}' ci/matrix-symmetry-baseline.json
```

### Rust test binaries and ignored test IDs

The Hermit package currently lists 1,198 Rust tests; 279 are ignored, or 23.3%. There are only 61 literal `#[ignore]` source sites because macros generate many IDs, especially 213 `rr_suite` IDs and eight `chaos_buck_*` IDs.

Commands:

```bash
cargo nextest list -p hermit --message-format json 2>/dev/null \
  | jq '{test_count:."test-count",listed:([."rust-suites"[].testcases[]]|length),ignored:([."rust-suites"[].testcases[]|select(.ignored)]|length)}'
git ls-files '*.rs' | xargs rg -n '^\s*#\[ignore(?:\s*=|\])' | wc -l
```

Result:

```text
test_count=1198 listed=1198 ignored=279
literal source sites=61
```

Ignored IDs by test binary:

| Binary | Total IDs | Ignored IDs |
| --- | ---: | ---: |
| `rr_suite` | 214 | 213 |
| `app_strict_verify` | 15 | 15 |
| `command_strict_verify` | 10 | 10 |
| `hermit_modes` | 82 | 10 |
| `language_runtime_determinism` | 7 | 7 |
| `frontier_app_benchmarks` | 5 | 5 |
| `stress_suite` | 6 | 4 |
| `analyze` | 3 | 3 |
| `leveldb` | 3 | 2 |
| eight other binaries | 22 combined | 8 combined |

The 61 literal sites occur in 19 files. The largest counts are 15 in `app_strict_verify.rs`, 10 in `command_strict_verify.rs`, 7 in `language_runtime_determinism.rs`, 5 in `frontier_app_benchmarks.rs`, 4 in `stress_suite.rs`, and 3 each in `hermit_modes.rs` and `analyze.rs`.

The allowlist contains 263 ignored IDs, 16 fewer than the 279 IDs Cargo reports. The missing entries are six `app_strict_verify`, seven `command_strict_verify`, and one each from `io_uring_ring_determinism`, `mmap_stress_determinism`, and `prodcons_determinism`.

Command:

```bash
cargo nextest list -p hermit --message-format json 2>/dev/null \
  | jq -r '."rust-suites"|to_entries[]|.value."binary-name" as $b|.value.testcases|to_entries[]|select(.value.ignored)|[$b,.key]|@tsv' \
  | sort > /tmp/actual-ignored.tsv
awk 'NF && $1 !~ /^#/ {print $1 "\t" $2}' hermit-cli/tests/fail_closed_allowed_ignores.tsv \
  | sort > /tmp/allowlisted-ignored.tsv
comm -23 /tmp/actual-ignored.tsv /tmp/allowlisted-ignored.tsv
```

The intended ratchet is not protecting main now:

- `scripts/test-fail-closed.sh:118-162` compares live IDs with the allowlist and would reject an unlisted ignore.
- The script is explicitly allowed to remain unscheduled at `scripts/check-checker-scheduling.rs:47-60`.
- It currently exits before inventory because the allowlist is not sorted.

Command and result:

```bash
timeout --kill-after=5s 30s ./scripts/test-fail-closed.sh
# exit 1: fail_closed_allowed_ignores.tsv must remain sorted
```

### Test-binary registration

The binary-level audit is scheduled, but it permits unresolved omissions:

```bash
python3 ci/audit-test-binary-registration.py
```

Result, exit 0:

```text
present=130 ci-registered=49 reason-recorded=10 none-recorded=71 undeclared=0
ACCOUNTED-WITH-UNKNOWN
```

Only 37.7% of tracked top-level integration-test binaries are registered in a DAG command; 54.6% are `none-recorded`. The audit fails only for `undeclared`, while `none-recorded` prints debt and returns success (`ci/audit-test-binary-registration.py:174-246`). Registration is also only binary-granular: it extracts `--test NAME` tokens (`:80-93`, `:127-148`) and cannot tell which test IDs inside that binary were selected or executed.

## Current prerequisite and selection mechanisms

### 1. Workflow placement and preflight

The portable workflow uses `ubuntu-latest`; the privileged workflow uses `[Linux, X64, hermit, pmu]` (`.github/workflows/ci-privileged.yml:19-46`). The privileged preflight only checks that `perf_event_paranoid` is readable, `/dev/kvm` is readable/writable, and `cc` resolves (`:79-85`). Other workflows separately modify AppArmor's user-namespace setting and run an `unshare` probe (`.github/workflows/ci-portable.yml:635-639`, `:760-765`).

This is placement/configuration, not one authoritative capability result. A runner label called `pmu` is trusted independently of the later direct PMU test.

### 2. Static portable/privileged partition

The two DAG files are the broad partition. It is useful but too coarse for independent axes. The project documentation correctly says architecture, CPU-model support, PMU availability, perf permission, CPUID interception, and namespaces are orthogonal (`docs/TESTING_ENVIRONMENTS.md:111-147`). KVM, optional backend artifacts, and tools add more axes.

The partition does not itself establish those capabilities. For example, `pmu.preemption` is always in the privileged DAG and runs `tests/util/pmu_skid.c`; it has no `requires_host_capability` field (`ci/dag/privileged.json:38-46`). That is a fail-by-execution policy, unlike CPUID/KVM pre-spawn withholding.

### 3. Validate host capability probes

`scripts/lib/validate_plan.rs:572-607` has a closed `HostCapability` enum with exactly two entries:

- `cpuid-faulting`
- `kvm`

Only two of 69 checked-in DAG nodes declare one:

```bash
jq -r '.steps[]|select(has("requires_host_capability"))|[.group,.job,.requires_host_capability]|@tsv' ci/dag/portable.json ci/dag/privileged.json
```

```text
cpuid  faulting  cpuid-faulting
test   cli_kvm   kvm
```

The CPUID probe combines `arch_prctl(ARCH_SET_CPUID, 0)` with `/proc/cpuinfo`; the KVM probe combines `open(/dev/kvm, O_RDWR)` with `vmx`/`svm` flags. Both require two observations to agree before declaring absence, and uncertainty runs the node (`scripts/lib/validate_plan.rs:705-881`). Validate probes each capability needed by the plan, withholds nodes before spawn, and records `host-inapplicable` (`scripts/validate.rs:4339-4401`).

This is the most principled existing mechanism, but it is not comprehensive and its result is not exported as one machine-wide input to every child.

### 4. E2E manifest `requires`

Every one of the 359 recipes declares `linux` and `x86_64`; 357 declare `userns` and `ptrace`. The complete token counts are:

| Token | Recipes |
| --- | ---: |
| linux | 359 |
| x86_64 | 359 |
| userns | 357 |
| ptrace | 357 |
| cc | 302 |
| python3 | 9 |
| bash | 7 |
| perl | 3 |
| openssl | 3 |
| kvm | 2 |
| ar, cpuid, cxx, date, du, find, gawk, git, hexdump, jq, lua5.4, m4, node, ruby, rustc, sqlite3, tclsh, zstd | 1 each |

Command:

```bash
awk 'BEGIN{inreq=0} /^    requires:$/ {inreq=1; next} inreq && /^      - / {x=$0; sub(/^      - /,"",x); c[x]++; next} inreq {inreq=0} END{for(k in c) print c[k], k}' tests/e2e/manifests/*.yaml | sort -nr
```

The vocabulary is closed, but only `cpuid` maps to a host capability; every other token maps to `None`, including `kvm`, `userns`, and every tool (`ci/manifest-plan/src/runner.rs:42-90`). Therefore:

```bash
target/debug/hermit-manifest-plan --format host-requirements
```

prints only:

```text
cpuid-faulting  backend-parity-c/cpuid-probe
```

The harness shells back into `scripts/validate.rs --probe-host-capability` and resolves the needed cell capabilities separately in each harness process (`ci/manifest-plan/src/bin/test-harness.rs:134-240`, `:1290-1338`). That shares probe code, but not one captured result for the validate run.

Missing compilers/interpreters and shell prerequisites are generally discovered later by each program's `--prepare` command. Preparation errors become node/cell errors, not pre-selection (`ci/manifest-plan/src/runner.rs:1077-1187`; `test-harness.rs:1136-1169`). Some suite drivers instead skip individual subcases, so the same kind of missing tool can either fail or disappear depending on the path.

### 5. Cargo/nextest command filters

DAG commands select binaries and test-ID subsets directly. The clearest example is `test.hermit_modes`:

```text
./ci/run-nextest-counted.sh ... --test hermit_modes ... -- --skip default_ --skip chaos_buck_ --skip hello_race_chaos_verify
```

Source: `ci/dag/portable.json:541-549`.

This means a normal FULL validate does not run the eight PMU chaos tests or the `default_*` tests. A currently failing `default_shell_parallel_work` can therefore coexist with a FULL green.

`ci/select-tests.rs` adds a separate changed-file-based selection layer. It maps paths to portable DAG nodes and E2E backend affinity; unknown changes conservatively select full, while proven inert changes select nothing (`ci/select-tests.rs:8-29`, `:460-540`, `:919-956`). This selects by code impact, not machine capability.

Validate's focused modes add more node/cell selection: `--only` selects named DAG nodes, while `--requalify-cell` selects one E2E test/mode/backend cell (`scripts/validate.rs:245-316`). There is no validate selector for an arbitrary exact Rust test ID today.

### 6. Static Rust ignores

Static ignores encode prerequisites, known failures, slowness, and occasional coverage in the same mechanism. The allowlist reasons include PMU, mount namespaces, optional language toolchains, long-running suites, and known blocking defects (`hermit-cli/tests/fail_closed_allowed_ignores.tsv`). Because the ordinary DAG often invokes binaries without `--ignored`, those tests are excluded regardless of actual host capability.

This is exactly what happened to the eight PMU chaos tests: capable hardware did not activate them.

### 7. Runtime early returns in Rust tests

Representative mechanisms:

- KVM: `cli.rs` contains 24 `if !Path::new("/dev/kvm").exists()` guards for the 24 `run_kvm_*` tests; the function returns and libtest reports success. The privileged node compensates by opening `/dev/kvm` first and asserting that 24 tests were selected (`ci/dag/privileged.json:103-112`). Other KVM tests separately check only path existence and return (`kvm_harder.rs:56-59`, `terminal_child_selectors.rs:83-88`, `kvm_lifecycle_timing_log_level.rs:95-100`, `kvm_info_log_determinism.rs:252-257,349-356,533-538`).
- DBT: `cli.rs:800-835` checks the compile-time `dbt` feature and permits skipping unless `HERMIT_REQUIRE_DBT` is set; 20 tests call it. `waitid_signal_interrupt.rs:383-394` repeats the helper and 10 tests call it. FULL builds with `third-party-backends`, but plain Cargo testing does not.
- SaBRe: helpers return `None`/return when default loader/plugin artifacts are absent, but panic if an explicit configured path is missing (`sabre_examples.rs:27-73`; `hermit_modes.rs:528-553`).
- PMU: `reverie_ptrace::ret_without_perf!()` returns from a test when a basic perf probe fails; it is used by `detcore/tests/misc/mod.rs:1840-1843`.
- CPUID: `detcore/tests/misc/mod.rs:88-101` has a separate `arch_prctl` probe; `rdrand_rdseed_is_masked` asserts it at `:1888-1900` rather than consuming validate's result.
- Host sysfs: Btrfs accounting, cpuidle, cpufreq, CPPC feedback, transparent huge pages, RTC, and `/proc/key-users` tests return early or skip subcases based on host paths/errors. Examples: `cpuidle_determinism.rs:70-101`, `cpufreq_avg_determinism.rs:68-73`, `cppc_feedback_determinism.rs:100-105`, `thp_stats_determinism.rs:73-103`, `sysfs_rtc_determinism.rs:115-183`, and the three Btrfs `*_determinism.rs` files.
- Tools: `record_replay.rs` skips portions when bash, sh, yes/head, Node, or sqlite3 are missing; `hashseed_determinism.rs` skips when Python is absent. Other tests panic on missing tools through helpers such as `required_program`, so missing-tool semantics are inconsistent.

Command counts for two large duplicated helpers:

```bash
rg -n 'if dbt_unavailable\(' hermit-cli/tests/cli.rs | wc -l
# 20
rg -n 'if dbt_unavailable\(' hermit-cli/tests/waitid_signal_interrupt.rs | wc -l
# 10
rg -n 'if !Path::new\("/dev/kvm"\)\.exists\(\)' hermit-cli/tests/cli.rs | wc -l
# 24
```

### 8. Shell-level skips and partial passes

Examples:

- `tests/util/chaos_stress_wrapper.sh:17-25` calls `perf_supported.sh`; failure prints that chaos will be skipped and exits 0.
- `tests/e2e/lib/language-runtimes/run.sh:85-131` independently probes Python, Ruby, Node, and Java, skips missing runtimes, and passes when at least one is available.
- `tests/e2e/lib/system-utils/_common.sh:17-22` defines `skip()` as a printed message followed by exit 0; backend allowlisting can call it (`:33-70`).
- `tests/standalone/strict_timer_create.sh:34-37` and `strict_setitimer.sh:52-55` exit 0 when no C compiler exists.
- individual E2E programs repeat `command -v` in their `--prepare` arms.

These are not represented as a single typed prerequisite outcome. Whether the outer layer notices the word `SKIP` depends on the caller; exit 0 alone looks green.

### 9. Hermit/Reverie PMU fallback

Hermit's ptrace-backed argument validation calls `reverie_ptrace::is_perf_supported()` (`hermit-cli/src/bin/hermit/run.rs:2758-2768`). If `max_timeslice` is requested but that function returns false, Hermit warns and changes `max_timeslice` to disabled (`:2887-2898`). Record/replay metadata has a similar fallback (`hermit-cli/src/metadata.rs:349-354`).

The probe is weaker than the runtime requirement. At pinned Reverie `1f226acd5bd0a942279e2acbf22c7e75c6527af7`, `is_perf_supported()` opens and closes one generic `PERF_COUNT_HW_INSTRUCTIONS` counter (`reverie-ptrace/src/perf.rs:807-842`). Actual deterministic preemption needs:

- a CPU-supported raw retired-conditional-branch event (`reverie-ptrace/src/timer.rs:161-205`);
- two simultaneous pinned per-thread counters (`timer.rs:753-779`; `perf.rs:216-227`);
- asynchronous overflow signal delivery and period reprogramming (`perf.rs:395-406` and the timer implementation);
- accurate nonzero counts and architecture-specific bug checks (`reverie-ptrace/src/validation.rs:139-150`, `:292-317`, `:360-420`);
- acceptable skid with single-step completion (`timer.rs:9-30`); and
- CPU affinity because RCB counts are not stable across migration (`hermit-cli/src/bin/hermit/run.rs:250-255`, `:2918-2922`).

Reverie's full PMU validation can report a problem but the counter builder only logs it and continues (`reverie-ptrace/src/perf.rs:307-314`). Thus neither the basic `is_perf_supported()` result nor successful `perf stat` alone proves Hermit-ready deterministic preemption.

`tests/util/pmu_skid.c` is the strongest current standalone probe. It opens the raw event twice with pinned/exclusion settings, configures signal delivery, reprograms the period, pins to a CPU, and measures skid (`tests/util/pmu_skid.c:195-240`, `:281-302`, `:341-410`). It should contribute to the one PMU capability result rather than remain a disconnected gate.

## PMU ignored tests: age and direct results

The eight tests are generated by `buck_chaos_tests!` and all carry:

```rust
#[ignore = "requires PMU branch counters and working mount namespaces"]
```

Source: `hermit-cli/tests/hermit_modes.rs:419-464`.

History command:

```bash
git log --format='%H %ad %s' --date=short \
  -S 'requires PMU branch counters and working mount namespaces' -- \
  hermit-cli/tests/hermit_modes.rs
```

Result:

```text
d8aa7ae9d7f31fb1d9d6e9a79f116e917d05d983 2026-07-21 Port chaos and record replay Buck tests (#40)
```

They were introduced already ignored and have therefore been disabled for 37 days as of this report. The other two ignored `hermit_modes` tests are known default-mode hangs (`hermit_modes.rs:721-730`) added on 2026-07-22.

The eight PMU cases were run individually at the audited clean commit with this command shape:

```bash
/usr/bin/time -f 'MEASURE elapsed_s=%e exit=%x' \
  timeout --kill-after=5s <bound>s \
  cargo test -p hermit --test hermit_modes <test> -- \
  --exact --ignored --nocapture
```

Results:

| Test | Result | Test/wall time | Finding |
| --- | --- | ---: | --- |
| `chaos_buck_getpid` | PASS | 1.74/1.88 s | executes on this host |
| `chaos_buck_uname` | PASS | 1.79/1.92 s | executes on this host |
| `chaos_buck_sysinfo` | PASS | 7.68/7.82 s | executes on this host |
| `chaos_buck_wait_on_child` | PASS | 1.83/1.97 s | executes on this host |
| `chaos_buck_clone` | PASS | 1.86/2.01 s | executes on this host |
| `chaos_buck_hello_alarm` | PASS | 1.76/1.89 s | executes on this host |
| `chaos_buck_nanosleep_parallel` | FAIL | 36.04/36.18 s | guest assertion saw `nanosleep` return `EINTR`; first verify run aborted before run 2 |
| `chaos_buck_mem_race` | no verdict | >300 s | 90 s and 300 s outer bounds expired; direct native guest finishes in 3.62 s, while Hermit remained in `Run1` beyond 60 s |

These are also scheduled only in the `super` gate `pmu_buck_chaos_cases` (`ci/super/gates.json:486-503`). Its literal timeout 0 resolves to the 600-second default, not infinity (`scripts/lib/validate_super.rs:124-129`).

## What `devbig014` can do today

Commands and observed results:

```bash
uname -r
# 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf

cat /proc/sys/kernel/perf_event_paranoid
# 1

perf stat -e instructions:u -- /bin/true
# exit 0; numeric instructions:u count

perf stat -e branches:u -- /bin/true
# exit 0; 54,361 branches:u in this sample

tests/util/perf_supported.sh
# exit 0

./scripts/validate.rs --probe-host-capability cpuid-faulting
# PRESENT: arch_prctl(ARCH_SET_CPUID, 0)=0; cpuinfo advertises cpuid_fault

./scripts/validate.rs --probe-host-capability kvm
# PRESENT: open(/dev/kvm,O_RDWR)=ok; cpuinfo advertises vmx or svm

target/ci-pmu-skid --iterations 50 --period 100000
# AMD EPYC 9D85; raw event 0x5100d1; precise_ip=0;
# skid min=60 max=260 mean=69.82 p99=260; recommended margin=520 RCB
```

This host has the measured facilities needed to attempt the PMU tests. The six passes confirm that the blanket static ignore is over-broad. The two non-passes show why host capability and test correctness must remain separate: satisfying the prerequisite means the test must run; it does not mean the test must pass.

## Where basic RCB access is not available

A basic branch counter may be unavailable in any of these environments:

- a VM that does not expose a virtual PMU or does not expose the needed event;
- a container or sandbox whose seccomp policy, LSM, or other security policy blocks `perf_event_open`;
- a host whose `perf_event_paranoid` setting requires privileges the process lacks (`CAP_PERFMON` or, on relevant kernels, `CAP_SYS_ADMIN`);
- an unrecognized CPU family/model for which Reverie has no raw conditional-branch event;
- a configuration with insufficient simultaneous physical counters or competing PMU users, causing a pinned event to be descheduled;
- a PMU/kernel/virtualization bug that yields zero or inaccurate raw counts, breaks `PERF_EVENT_IOC_PERIOD`, or violates architecture-specific checks; or
- execution that cannot maintain the CPU affinity needed for consistent RCB counts.

`perf stat -e branches:u -- /bin/true` and `tests/util/perf_supported.sh` prove that one generic branch event opens and returns a number. They do not prove the two pinned raw counters, signal delivery, period reprogramming, CPU-model mapping, accuracy, or skid properties Hermit uses. A machine capability result should therefore record both basic access and the stronger Hermit-ready PMU result.

## Why FULL green does not mean “nothing skipped”

There are five independent gaps.

### Filtered and ignored Rust tests are allowed

The nextest wrapper parses nextest's `skipped` count but, on success, rewrites it as libtest `filtered out` (`ci/run-nextest-counted.sh:46-71`). Its self-test explicitly accepts “8 passed, 7 skipped” and emits “7 filtered out” (`:100-108`).

The receipt predicate has `"gate_filtered_tests": false` (`../ci-hub/validate/qualifying-receipt.json:18`). `row_qualification` only rejects a positive filtered count when that switch is true (`../ci-hub/lib/qualifying_receipt.rs:849-875`).

For direct libtest output, dagrun treats `running N tests` as the executed count; its unit test explicitly counts one ignored test inside an executed total of five (`agent-utils/rs/dagrun/src/scheduler.rs:2021-2075`, `:3593-3606`). Therefore ignored tests can increase the reported executed count.

### Coverage is node-level, not exact-test-ID-level

The receipt finalizer derives only `test.<job>` node names from the DAG (`../ci-hub/validate/finalize_receipt.py:233-258`). A planned node with a terminal status and no parseable banner is treated as executed; a node is inert only when it emits a parseable zero count (`:261-314`). There is no source-derived expected set of Rust test IDs and no comparison with the IDs actually run.

The registration audit is only binary-level. A single exact test invocation can make an entire binary “registered” while leaving its other IDs unselected.

### FULL's E2E contract is the Required population, not every enabled cell

The scorecard can prove exact execution of the 308 cells selected for Required, but the declared enabled population is 679. The remaining 371 enabled cells are outside that contract by policy.

### Mixed host-inapplicable E2E buckets can exit successfully

The harness emits `HOST-INAPPLICABLE` rather than running such a cell and only forces failure when every selected cell is host-inapplicable (`ci/manifest-plan/src/bin/test-harness.rs:1288-1338`, `:1453-1462`). A mixed bucket can therefore pass with one or more selected cells unrun.

The receipt's cell-result schema treats `unavailable-with-reason` with tier `declared-but-unverifiable` as a valid closed state, then checks that every selected cell has some valid state; it does not require every selected cell to be compared-and-matched (`../ci-hub/qualifying_receipt.py:579-588`, `:653-678`, `:724-828`).

### Planned-gate accounting is not applied to a PASS row

The Rust receipt library defines `gate_accounting_complete`, requiring executed plus admitted intentional skips to cover the expected gates (`../ci-hub/lib/qualifying_receipt.rs:789-844`). The PASS `row_qualification` path begins at `:849` and does not call it. `validate-status` calls `row_qualifies` directly for a pass (`../ci-hub/lib/validate_status.rs:225-233`, `:491-529`); gate accounting is used only while classifying red rows (`:339-407`). The Python qualification path has the same omission (`../ci-hub/qualifying_receipt.py:1217-1274`).

The producer already records `gates_expected`, `intentional_skipped_nodes`, dependency skips, and unaccounted nodes (`scripts/validate.rs:12782-12934`). The data needed to reject an incomplete green exists; the green predicate does not currently consume all of it.

## Duplicated or inconsistent capability knowledge

| Capability/prerequisite | Current independent authorities | Consequence |
| --- | --- | --- |
| PMU/RCB | workflow `pmu` label; readable `perf_event_paranoid`; `perf_supported.sh`; validate `pmu.preemption`; Reverie generic-instructions probe; Reverie full validation; static ignores; individual shell skips | a basic probe can pass while the real timer cannot; capable hosts still retain static ignores; some failures become weaker execution rather than refusal |
| CPUID faulting | validate `HostCapability`; manifest `cpuid`; detcore test-local `arch_prctl` probe; runner label/assumption | repeated probing and different fail/skip behavior |
| KVM | workflow label and file permission check; validate open+cpuinfo probe; manifest `requires: kvm` that maps to no capability; 24 CLI early returns; several other test-local path checks; shell programs | existence, openability, and usable KVM are conflated; early returns can report pass |
| user/mount namespaces | portable/privileged descriptions; 357 manifest tokens mapped to no probe; workflow sysctl/unshare; Hermit container setup; test ignore reasons | requirement is declared almost everywhere but drives no shared selection |
| DBT | Cargo feature; `HERMIT_REQUIRE_DBT`; DAG feature flags; artifact staging; runtime availability checks | plain Cargo and validate exercise different populations; helper duplicated in two binaries |
| SaBRe/LiteInst | build features, environment paths, staged artifacts, test helpers, product checks | missing default artifacts can skip tests, while configured absence fails |
| compilers/interpreters/tools | manifest `requires`; workflow package installs; per-program `--prepare`; Rust helper checks; suite-level partial skips | same missing tool may omit a subcase, fail preparation, panic, or be ignored statically |
| sysfs/procfs features | test-local path checks only | tests pass after returning early; no receipt-level record of lost coverage |

This is the central DRY failure: vocabulary exists in several places, and observed capability state exists only transiently inside the component that asked.

## Concrete design using current project vocabulary

### 1. Produce one machine capability result at validate admission

Before plan construction, enumerate every capability needed by the selected profile and run each probe once. Write one versioned JSON result containing, at minimum:

- exact Hermit source SHA;
- hostname/machine short name;
- kernel release;
- boot ID, so a reboot on the same machine cannot silently mix results;
- CPU vendor/family/model;
- capability name, present/absent/unknown, evidence, probe command/version, and timestamp.

Pass the path to every planner, `test-harness`, Rust test launcher, and shell suite. Children must consume it, not probe again. Refuse a missing, stale, wrong-SHA, wrong-kernel, or wrong-boot result.

This uses the project's existing `HostCapability`, `requires`, `host-inapplicable`, and receipt vocabulary; it does not require a new label for the mechanism.

### 2. Use one closed requirement vocabulary

Move the currently split mappings into one shared Rust library used by validate and manifest-plan. Cover at least the requirements already present in source:

- `linux`, `x86_64`;
- `userns` and mount namespaces;
- `ptrace`;
- `pmu` at both basic-access and Hermit-ready functional levels;
- `cpuid-faulting`;
- `kvm`;
- DBT, SaBRe, and LiteInst artifact availability; and
- the existing tool names in manifest `requires`.

Do not treat a token as merely validated metadata. Every requirement should either map to a probe or be explicitly classified as a static build/platform requirement.

### 3. Attach requirements outside test bodies

Requirements should be declared on:

- DAG nodes;
- E2E test/cell recipes; and
- exact Rust test IDs in a version-controlled table checked against `cargo nextest list`.

Generate portable/privileged/full selections from those requirements. Replace hard-coded `--skip` lists, blanket `#[ignore]` attributes used only for host prerequisites, and in-test early returns. Once a test is selected, its body must either perform the asserted work or fail.

Known product failures, expensive/occasional tests, and host prerequisites are different facts and must stay separate. A test may be known red and still selected on a capable host; it should not be hidden behind the same mechanism used for unavailable hardware.

### 4. Make FULL require the complete declared population

For a qualifying FULL run:

- admission must require every machine capability needed by the FULL population;
- an absent capability must make the run `COULD_NOT_RUN`, not redefine FULL to a smaller set;
- the expected exact Rust test-ID set must equal the union of executed test IDs across nodes;
- ignored, skipped, or unexplained filtered IDs must be zero;
- every required E2E cell must have a terminal result that performed its declared evidence level;
- `HOST-INAPPLICABLE`, dependency-skipped, unaccounted, and successful bannerless test nodes must disqualify the receipt; and
- the binary audit must have zero `none-recorded` entries.

Portable and focused runs remain useful but non-qualifying. If the project later wants exact-SHA partitioned receipts from more than one machine, the union must cover one canonical exact-ID population, with disjoint/declared partitions and no missing IDs; no individual partial receipt should read as FULL.

### 5. Repair the existing ratchets before relying on them

1. Sort and update `fail_closed_allowed_ignores.tsv`, then schedule `scripts/test-fail-closed.sh` in a real DAG node.
2. Make `none-recorded > 0` fail the binary registration gate for FULL.
3. Add an exact test-ID inventory and execution result, not only aggregate counts.
4. Apply `gate_accounting_complete` to PASS qualification.
5. Reject positive skipped/ignored counts directly; do not infer completeness from the current aggregate `filtered_tests` number, because ordinary sharding also produces legitimate filtering.
6. Require a mixed E2E bucket to propagate its host-inapplicable cells to a non-qualifying run.

### 6. Suggested migration order

1. Generate and check in the exact Rust test-ID inventory with current requirements and current omissions; initially report differences without changing execution.
2. Expand the shared requirement vocabulary and produce the one machine capability result.
3. Convert the eight PMU chaos tests first: run six normally on a capable host, retain the two real failures as failures to fix, and stop using static ignore as the capability switch.
4. Convert KVM and DBT early returns to pre-selection from the shared result.
5. Convert tool and sysfs/procfs skips category by category.
6. Tighten the FULL receipt only after the reported unknown/skip debt reaches zero, so the policy change starts green for honest reasons.

## Direct answers

### Do we already dynamically down-select based on host tests?

Yes, narrowly. Validate dynamically withholds DAG nodes for `cpuid-faulting` and `kvm`, and the E2E harness dynamically withholds cells whose `requires` token maps to a host capability. In the current manifests, only `cpuid` maps that way. PMU, namespaces, KVM manifest tokens, tools, backend artifacts, and sysfs/procfs prerequisites are handled elsewhere or not dynamically selected.

### Is portable/privileged enough?

No. It is a useful broad partition, but the project already documents independent capability axes. A privileged machine can have KVM but no CPUID faulting, PMU access but unusable raw RCB timing, namespaces but no optional runtime, and so on. Keep the lanes, but derive each test's inclusion from its declared requirements and the one measured machine result.

### Does FULL currently mean nothing was skipped?

No. FULL currently means both lane plans were selected and the receipt's node-level and cell-population predicates passed. It does not prove all Rust test IDs ran, all enabled E2E cells ran, or all selected cells produced comparable evidence.

### Does `devbig014` have usable PMU/RCB support?

Yes for the directly measured prerequisites: generic instructions and branch events work, `perf_supported.sh` passes, the raw two-counter overflow/skid probe passes, CPUID faulting is present, `/dev/kvm` opens, and six of eight previously ignored PMU chaos tests pass. That does not excuse the two remaining test defects; it proves they should be run and investigated on this machine.

## Audit limitations

- The source audit is pinned to `7ddb3559a09f2709a67ce2b605426b43c33f070f`. The shared checkout and `origin/main` advanced during the audit to `92b7fc3823826961eadaa4bbcbc75e0a65b6a0fc`; `git diff --name-only` showed that only `AGENTS.md` and two skill files changed, so no audited test, DAG, manifest, planner, or receipt path changed.
- The 1,198/279 Rust-ID inventory is for package `hermit`; other workspace packages have their own tests, but the owner question and the disabled PMU cases were in the Hermit integration suite.
- Static searches can count explicit mechanisms but cannot prove that every arbitrary early return is a prerequisite skip. This report cites the known prerequisite helpers and representative path/tool checks rather than classifying every `return` statement.
- The eight ignored PMU tests were measured individually. No FULL validate was run for this audit.
