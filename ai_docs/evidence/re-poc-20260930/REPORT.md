# RE-POC: Hermit `hermit run` tests on buck2 Remote Execution (2026-09-30)

Author: sub-re-poc (agentcloud OnDemand node twshared4569.04.vcn5, own fbsource checkout @ a11f6222bbad), for coord2 / Ryan Newton.
No diffs published; all local modifications are throwaway and listed at the end.

## TL;DR

**RE-POC: passes-on-RE.** The unmodified test
`fbcode//hermetic_infra/hermit/tests:hermit_run_default__custombin_minimal_hello`
(which runs `hermit run --no-sequentialize-threads --no-deterministic-io <nostdlib hello>` under the Reverie
ptrace backend) executed on a remote RE worker (`RemoteCommand`, `cache_hit: false`,
`platform=linux-remote-execution`, `use_case=fbcode_re_tests`) and PASSED (exit 0, stdout `Hello, World!`).
The same digest ran remotely and passed again later (run4). Strict/verify tests (`hermit_run_strict__custombin_minimal_hello`,
`test_hermit_strict__rr_hello`: PMU preemption `--preemption-timeout=80000000`, two-run determinism check) also pass on RE,
but only with throwaway harness path fixes (unmodified they fail before the guest starts), and strict determinism fails
on Sapphire Rapids workers (section 5). The same test run by the Local executor on this node FAILS (hermit container
setup `EPERM` on `Mount`), which corroborates that the passing run was not local (the proof is the event log, section 2).

RE workers allow ptrace, perf_event_open and unprivileged namespaces: probed directly on one Skylake-SP worker
(`PTRACE_SEIZE` OK, perf_event_open OK incl. raw RCB event, `perf_event_paranoid=1`, `CapEff` = `CAP_PERFMON`,
user+pid+mount namespaces OK, no seccomp, no YAMA) and indirectly on ~570 passing remote hermit test executions.
No failing test showed a ptrace/namespace/PMU-access denial. Caveats: some AMD workers lack CPUID faulting
(`ARCH_SET_CPUID returned ENODEV ... On AMD hosts, use Linux 6.17+`; workers run 6.16.1) and have the SpecLockMap
RCB bug unmitigated.

What breaks on RE is (a) **test-harness path assumptions** (RE runs tests from the project root with
project-relative paths; harnesses that `cd` or use `--isolate-workdir` then cannot find `buck-out/...`),
(b) **undeclared inputs** (standalone scripts reading repo source files), (c) a few **worker-environment
differences** (rlimits, priority; hypothesis for some), (d) **CPU-model heterogeneity** in the RE pool: strict-mode
determinism/preemption fails on Intel Sapphire Rapids workers (see section 5), and (e) the largest remaining class:
**53 trace-replay/chaos-replay tests fail a DETLOG comparison of hermit-owned execve pointers** (harness; root cause
not fully explained, section 4 item 2).

## 1. How to force a Hermit test onto RE

Hermit tests are `buck_sh_test` with `remote_execution = None`. In `fbcode/buck2/prelude/tests/re_utils.bzl`
such a test gets an explicit **local-only** executor (`local_enabled=True, remote_enabled=False`). There is no
`local_only` label; the test labels are just
`tpx-enable-artifact-reporting, buck_sh, tpx-enable-artifact-cas-store, top_of_file_oncall:hermit, tpx:platform:linux, ...`.

* `buck2 test --remote-only <t>` alone does NOT move the test: tpx skips it with
  `The desired execution strategy (RemoteOnly) is incompatible with the executor config that was selected: CommandExecutorConfig { executor: Local(...) }`.
* The config-only switch is **`-c fbcode.re_tests=true`**: `fbcode/buck2/platform/remote_test_execution_toolchain_utils.bzl`
  then makes the toolchain default profile `{"capabilities": {"platform": "linux-remote-execution"}, "use_case": "fbcode_re_tests"}`.
  No source edit is needed.
* `--remote-only` is optional (the test executor is already remote-only), and it actually breaks the three `pythonbin_*`
  targets (`generate_build_info_json` is `LocalRequired` -> `Incompatible executor preferences: RemoteRequired & LocalRequired`).

Command used (mode matches the package CI replace `fbcode//mode/dev-nosan`):

```
buck2 test @fbcode//mode/dev-nosan --remote-only -c fbcode.re_tests=true \
  fbcode//hermetic_infra/hermit/tests:hermit_run_default__custombin_minimal_hello -- --print-passing-details
```

## 2. Headline run (pristine tree, no modifications)

Pristine tree: the buck2 event log for this invocation records `VersionControlRevision has_local_changes:false` at
`a11f6222bbad` (later runs with throwaway edits record `has_local_changes:true`).

```
Buck UI: https://www.internalfb.com/buck2/de996a9a-0632-495e-b39a-e70d42662549
Test session: https://www.internalfb.com/intern/testinfra/testrun/35747322082179512
RE Session: reSessionID-532f49b4-c433-4960-a359-36ad2a76a1d7
✓ Pass: fbcode//hermetic_infra/hermit/tests:hermit_run_default__custombin_minimal_hello - unmanaged (0.6s)
stdout:
Hello, World!
Tests finished: Pass 1. Fail 0. ...   (buck2 exit 0)
```

`buck2 log what-ran --format json` (test action):
```
{"reason": "test.run", "identity": "fbcode//hermetic_infra/hermit/tests:hermit_run_default__custombin_minimal_hello",
 "reproducer": {"executor": "Re", "details": {"digest": "fd9f3b3f6cf7cb970f19d45b0adb46ff14a3b43bb7309debb4fee4f2d2099866:150",
 "platform_properties": {"platform": "linux-remote-execution"}, ...}}}
```
Across the whole invocation: 8895 actions `Cache`, 83 `Re`, **0 `Local`**.

buck2 event log TestRun command_report:
```
{"RemoteCommand": {"action_digest": "fd9f3b3f...:150", "cache_hit": false, "queue_time_us": 219831,
  "details": {"session_id": "reSessionID-532f49b4-...", "use_case": "fbcode_re_tests",
  "platform": {"properties": [{"name": "platform", "value": "linux-remote-execution"}]}}},
  "cache_hit_type": 2}   signed_exit_code=0, execution_time_us=617922
```
`cache_hit_type: 2` = `EXECUTED` ("not a cache hit", buck2_data/data.proto). The event log also has a
`ReLogStreamAvailable` from `SERVICE_IDENTITY:re-worker-job`. The worker's CPU for this particular run is not known.

### Controls
| run | executor | result |
|---|---|---|
| `-c fbcode.re_tests=true --remote-only` | `Re` (RemoteCommand, cache_hit=false) | **PASS**, `Hello, World!` |
| `--remote-only` only | none (rejected) | SKIP: `RemoteOnly is incompatible with ... executor: Local` (exit 64) |
| default (no flags; run on the tree with throwaway edits, which do not touch this target) | `Local` (what-ran: `"executor":"Local","command":["/data/atlas/boxes/full-fbsource/buck-out/.../hermit","run",...]`) | **FAIL**: `panicked at reverie-process/src/container.rs:842:26: Error { errno: EPERM, context: Mount }`, plus `WARNING: --preemption-timeout requires user-space perf counters, but perf_event_open is unavailable` |

The agentcloud node itself cannot run hermit (mount EPERM, no perf), so a pass can only come from RE.

## 3. Strict mode and the minimal cases

* `hermit_run_strict__custombin_minimal_hello` (hermit-verify: two `hermit run --base-env=empty --env=HERMIT_MODE=strict
  --preemption-timeout=80000000` runs with `--isolate-workdir`, then log-diff) **failed unmodified** on RE, but not for a
  capability reason:
  `Error: Could not resolve program "buck-out/v2/art/.../minimal_hello.bin" in the guest PATH ... No such file or directory (os error 2)`.
  Cause: on RE the guest path is project-relative and `--isolate-workdir` sets the guest cwd to `/tmp/out`.
  With a throwaway `canonicalize()` of the guest path in hermit-verify it **passes on RE** (runs match, `DESYNC events not found`, `Success!`).
* `test_hermit_strict__rr_hello` (`hermit run --base-env=minimal --verify --preemption-timeout=80000000 --workdir=/tmp -- rr test_hello`)
  **failed unmodified** on RE with exit 127:
  `test_runner.sh: line 43: buck-out/v2/art/.../hermit: No such file or directory`
  (`hermetic_infra/common/scripts/test_runner.sh` does `cd $(mktemp -d)` before exec'ing project-relative args).
  With a throwaway absolutization in test_runner.sh it **passes on RE**: `:: Success: deterministic. Determinism verified.`
* Throwaway probe test `re_poc_probe` on RE worker `twshared70618.04.vll6` (Intel Xeon Platinum 8321HC, bare metal,
  kernel 6.16.1-0_fbk5, uid fbnobody, cwd /re_cwd, cgroup `system-re-workloads...rustemd-*.service`):
  * `hermit run -- /bin/true` -> EXIT=0
  * `hermit run -- /bin/grep TracerPid /proc/self/status` -> `TracerPid:1` (guest really is ptraced), EXIT=0
  * `hermit run -- /bin/date` -> `Fri Dec 31 03:59:59 PM PST 2021` (virtualized epoch), EXIT=0
  * `hermit run --strict -- /bin/true`, `--strict -- /bin/date`, `--strict -- minimal_hello`,
    `--strict --preemption-timeout=80000000 -- minimal_hello`, `--strict --no-rcb-time -- minimal_hello` -> all EXIT=0
  * `hermit run --verify --strict -- /bin/date` -> `Success: deterministic. Determinism verified.`
  * `hermit --log=info run --strict -- /bin/true` -> full detcore DETLOG syscall trace, RCB counts advancing (`rcbs: 367 ... 27154`), `reverie_ptrace::task` tail_inject exit_group.
  * perf/ptrace C probe: `perf_event_open(HW_BRANCH_INSTRUCTIONS) OK`, `RAW intel rcb 0x5101c4 OK`, `SW_TASK_CLOCK OK`, `ptrace(PTRACE_SEIZE) OK`.
  * `unshare -Ur` OK, `unshare -Urpmf --mount-proc` OK; `perf_event_paranoid=1`; `max_user_namespaces=2147483647`;
    `/proc/sys/kernel/yama` absent; `Seccomp: 0`, `NoNewPrivs: 0`; `CapEff=0000004000000000` (CAP_PERFMON).

## 4. Bonus: whole-package sweep on RE (throwaway path fixes applied)

`buck2 test @fbcode//mode/dev-nosan --remote-only -c fbcode.re_tests=true fbcode//hermetic_infra/hermit/tests: --keep-going`
(with the test_runner.sh + hermit-verify `run` path fixes; Buck UI trace 423b3654-41e3-4127-b8d5-5f596f5ba8d8):

**Pass 385, Fail 88, Timeout 2, Build failure 3** — all 476 `test.run` actions (475 distinct tests) executor `Re`.

| category | pass | fail |
|---|---|---|
| raw_run__ (no interception) | 33 | 0 |
| hermit_run_default__ | 33 | 0 |
| hermit_record_ | 26 | 0 |
| test_hermit_strict__rr_* (hermit run --verify --preemption-timeout=80000000) | 215 | 3 (+1 timeout) |
| hermit_run_strict__ (hermit-verify run) | 29 | 3 |
| hermit_run_chaos__ | 30 | 2 |
| hermit_chaos_fail_ | 4 | 0 (+1 timeout) |
| hermit_run_tracereplay__ / _chaos__ / chaosreplay__ | 0 | 72 |
| standalone__ | 5 | 4 |
| other (analyze_*, verify_replay__, cbin_*, test_no_networking_*, test_standalone_stacktrace_events__*, re_poc_probe, 3 pythonbin type-check tests) | 10 | 4 |

Failure classification (sweep + a follow-up rerun of every failure without `--remote-only` and with the
hermit-verify fix moved to `cli_wrapper.rs::Hermit::run` so it covers trace-replay/chaos-replay too):

1. **Project-relative path + cwd change (73 in sweep)** — hermit-verify trace-replay/chaos-replay: `Could not resolve program "buck-out/..."`.
   Fixed by canonicalizing the guest path: `tracereplay_chaos__*` then pass 18/19, `cbin_just_spin_trace_replay_split` passes.
2. **Harness DETLOG comparison of hermit's own pointers (53 after fix: all `tracereplay__*` 25 + `chaosreplay__*` 28)** —
   log-diff flags `DEBUG syscall.intercept{tid=3 syscall=execve args=SyscallArgs { arg0: 140700618654208, ... }}` vs
   `arg0: 139909860229504`. These are pointers into hermit's pre-exec child image (hermit's own heap), not guest state.
   Tested with `setarch x86_64 -R` (ASLR off, personality 0x40000): delta shrinks from ~GBs to 128/32 bytes but does not
   vanish (record vs replay hermit processes have different argv, e.g. the extra `--replay-schedule-from=`), so ASLR is
   NOT the whole explanation. Chaos variants pass only because they set `verify_detlog_others = false`.
   Fix direction: normalize/drop pointer args in the `syscall.intercept{}` span prefix in `hermit log-diff`.
   (Open question: why this is green in regular CI.)
3. **Undeclared inputs / cwd assumptions (4 standalone__)** — `find: './hermetic_infra/hermit/hermit-cli/src': No such file or directory`,
   `can't open file '/re_cwd/fbcode/hermetic_infra/hermit/tests/standalone/../util/simplest_server.py'`,
   `Could not resolve program "examples/race.sh"`. RE only materializes declared inputs.
4. **Likely worker-environment differences (3 rr tests; rlimit shown, others hypothesis)** — `rr_rlimit`: `setrlimit(RLIMIT_FSIZE) errno:1 (Operation not permitted)`
   (worker hard limit); `rr_ppoll`: `!(ret == -1 && errno == EINTR) errno:38 (Function not implemented)`;
   `rr_sched_yield_to_lower_priority`: `!(!low_priority_thread_scheduled)`. `rr_multiple_pending_signals_sequential`
   times out on RE (both runs).
5. **`--remote-only` artifact (3 build failures)** — `pythonbin_*` `generate_build_info_json` is LocalRequired.
   Without `--remote-only` the 6 py tests (`raw_run__py_*`, `hermit_run_default__py_*`) pass, test action still `Re`.
6. **Consistent with CPU-model dependence (strict/chaos determinism; per-test worker CPU not logged)** — see section 5
   (e.g. a sweep log shows `Switch points: 1`, the SPR-stock signature). `hermit_run_strict__rs_mem_race`,
   `hermit_run_strict__sh_py_hello`, `hermit_run_strict__sh_par_work` failed in the sweep and passed in the rerun;
   `hermit_run_chaos__sh_{py_hello,par_work}`, `hermit_run_tracereplay_chaos__rs_mem_race`, `analyze_*` failed both times.
7. `verify_replay__nanosleep-threads-simple`: both hermit runs (record and replay) segfault + `git: command not found` in the helper script.

All 17 non-path failing tests are `OK / GOOD_QUALITY` in testinfra's bare-name entries (the "unmanaged" DISABLED
entries exist even for tests that pass; checked live via `meta testinfra.test list`), so these are RE-environment
regressions, not already-broken tests.

No failure anywhere mentions ptrace/seccomp/YAMA/namespace/PMU-access denial: grepping all RE logs for
`EPERM|Operation not permitted|EACCES|Permission denied|perf_event_open is unavailable|context: Mount|Unshare|Clone`
finds only the rr_rlimit guest's own setrlimit (note: the sweep prints output only for failing tests). Total:
**643 test.run actions, all executor `Re`** (66 of them throwaway probes, incl. tpx retries).

## 5. RE worker pool is heterogeneous; strict-mode determinism depends on CPU model

Throwaway `re_poc_probe2_{0..23}` (each prints CPU, runs `hermit run --strict --base-env=empty --verify -- bash -c "python3 -c ..."`
and `hermit run --strict --preemption-timeout=80000000 -- rustbin_mem_race`, which asserts >1 deterministic preemption):

| worker CPU | reverie RCB event | samples | mem_race switch points | py_hello --verify pass |
|---|---|---|---|---|
| Intel Skylake-SP family (fam 6 model 85: Xeon D-2191A, Platinum 8321HC) | 0x5101c4 (stock) | 11 | 9 (11/11) | 11/11 |
| AMD EPYC 9D25 (Turin) | stock | 1 | 9 | 1/1 |
| **Intel Sapphire Rapids** (fam 6 model 143, Xeon Gold 6450C) | **0x5101c4 (stock)** | 24 | **1 (24/24 -> panic)** | 8/24 |
| **Intel Sapphire Rapids** | **0x5111c4 (throwaway fix)** | 20 | **9 (20/20)** | 7/20 |

* `fbcode/hermetic_infra/reverie/reverie-ptrace/src/timer.rs` says it is "based on rr's PerfCounters", but maps
  Ice Lake (0x86), Tiger Lake (0x8D), Alder Lake (0x9A) and Sapphire Rapids (0x8F) to `0x5101c4`, while the vendored
  `third-party/rr/src/PerfCounters.cc` uses `0x5111c4` for Icelake/Tigerlake/Rocketlake/Alderlake/Raptorlake/SapphireRapid.
  On RE SPR workers (Gold 6450C), changing 0x8F to 0x5111c4 (rr's value) changed mem_race from 1 switch point
  (24/24 panic) to 9 (20/20), matching Skylake-SP/AMD. Very likely a reverie bug, not RE-specific — but not yet confirmed
  with a same-host A/B or off RE: the stock and fixed batches ran on disjoint SPR host sets, sequentially (not
  interleaved), and kernel/microcode were not recorded; also mem_race only asserts >1 switch point in one run (not
  run-to-run determinism). Candidate fix: adopt rr's 0x5111c4 for 0x86/0x8D/0x9A/0x8F.
* A second SPR issue remains: py_hello strict `--verify` passes only ~1/3 of the time on SPR with either event (8/24, 7/20)
  (divergence is an event-ordering difference, e.g. `DETLOG RNG USER RAND Generated new seed` in run 1 vs a
  `(pre) registers` DETLOG at the same position in run 2). Not root-caused.
* `WARN reverie_ptrace::perf: Pmu bugs detected: AmdSpecLockMapShouldBeDisabled` appeared in `hermit_chaos_fail_keyvalue`
  (sweep; that test timed out, then passed in the rerun) and `analyze_nanosleep-threads-nocrash` (rerun): some AMD workers
  have the SpecLockMap RCB bug unmitigated. AMD workers also log `ARCH_GET_CPUID reported a valid state, but ARCH_SET_CPUID
  returned ENODEV ... On AMD hosts, use Linux 6.17+ upstream` (12 occurrences), i.e. no CPUID faulting there.
* Pool mix varied by time: rerun batch 7/8 Skylake-SP; SPR-fix batch 20/24 SPR; stock-baseline batch 23/24 SPR.


## 6. What it would take to run Hermit CI on RE

* **Nothing on the RE worker side needs to be requested for ptrace/namespaces/PMU access** — the default
  `linux-remote-execution` workers already grant CAP_PERFMON with perf_event_paranoid=1, allow ptrace and unprivileged
  namespaces (no special RE platform needed). CPU heterogeneity (SPR, AMD without CPUID faulting/SpecLockMap mitigation)
  is the worker-side concern.
* To opt Hermit tests into RE: `-c fbcode.re_tests=true` (config only) or per-target `remote_execution = "linux"`
  / explicit RE props in `helpers.bzl` / `wrap_test_suite.bzl`.
* Harness fixes (small, local to hermit):
  1. `hermit-verify`: canonicalize the guest program path (one line in `cli_wrapper.rs::Hermit::run`).
  2. `hermetic_infra/common/scripts/test_runner.sh`: absolutize project-relative args before `cd $(mktemp -d)`.
  3. `hermit log-diff` / trace-replay verify: stop comparing hermit-owned pointer values in `syscall.intercept{}` span fields.
  4. standalone scripts: declare the repo files they read (or stop reading the source tree), don't depend on `git`.
* Determinism fidelity: either pin Hermit strict/chaos/replay tests to known-good CPUs, or fix reverie for SPR
  (adopt rr's 0x5111c4 for Ice Lake+/SPR; then investigate the residual SPR py_hello divergence) and handle AMD
  SpecLockMap. Non-strict `hermit run` default/raw/record tests and the rr strict suite are already ~all green on RE.
* A few rr tests encode host policy (rlimits, priorities, ppoll EINTR) and would need excludes or worker-policy tweaks.


## 7. Throwaway local modifications (NOT published, reverted after this report)

Note: the probe2/probe3 scripts listed are the final versions; earlier batches used variants that differed only in
grep/output lines.

### Current throwaway working-copy changes (sl diff fbcode/hermetic_infra + new files)
```diff
diff --git a/fbcode/hermetic_infra/common/scripts/test_runner.sh b/fbcode/hermetic_infra/common/scripts/test_runner.sh
--- a/fbcode/hermetic_infra/common/scripts/test_runner.sh
+++ b/fbcode/hermetic_infra/common/scripts/test_runner.sh
@@ -34,6 +34,12 @@
 }
 
 trap onexit EXIT
+# RE-POC throwaway: absolutize project-relative paths (RE runs from project root) before cd.
+abs_args=()
+for a in "$@"; do
+    if [[ "$a" != /* && "$a" == */* && -e "$a" ]]; then abs_args+=("$PWD/$a"); else abs_args+=("$a"); fi
+done
+set -- "${abs_args[@]}"
 workdir=$(mktemp -dt "test-${TEST_NAME:-unknown}-XXXXXXXXX")
 cd "$workdir"
 
diff --git a/fbcode/hermetic_infra/hermit/hermit-verify/src/cli_wrapper.rs b/fbcode/hermetic_infra/hermit/hermit-verify/src/cli_wrapper.rs
--- a/fbcode/hermetic_infra/hermit/hermit-verify/src/cli_wrapper.rs
+++ b/fbcode/hermetic_infra/hermit/hermit-verify/src/cli_wrapper.rs
@@ -43,7 +43,8 @@
         HermitRunBuilder {
             log_file: self.log_file,
             level: self.log_level,
-            guest_program,
+            // RE-POC throwaway: guest path is project-relative on RE; --isolate-workdir changes cwd.
+            guest_program: std::fs::canonicalize(&guest_program).unwrap_or(guest_program),
             guest_args: args,
             ..Default::default()
         }
diff --git a/fbcode/hermetic_infra/hermit/tests/BUCK b/fbcode/hermetic_infra/hermit/tests/BUCK
--- a/fbcode/hermetic_infra/hermit/tests/BUCK
+++ b/fbcode/hermetic_infra/hermit/tests/BUCK
@@ -655,3 +655,50 @@
     target_compatible_with = ["ovr_config//cpu:x86_64"],
     test_targets = RR_TEST_TARGETS,
 )
+
+# RE-POC throwaway (not for commit): probe the test worker environment.
+cpp_binary(
+    name = "re_poc_perf_probe",
+    srcs = ["re_poc/perf_probe.c"],
+    target_compatible_with = ["ovr_config//cpu:x86_64"],
+)
+
+buck_sh_test(
+    name = "re_poc_probe",
+    env = {
+        "HERMIT_BIN": "$(location //hermetic_infra/hermit/hermit-cli:hermit)",
+        "MINIMAL_HELLO": "$(location //hermetic_infra/hermit/tests:minimal_hello)",
+        "PERF_PROBE": "$(location :re_poc_perf_probe)",
+    },
+    target_compatible_with = ["ovr_config//cpu:x86_64"],
+    test = "re_poc/probe.sh",
+)
+
+[
+    buck_sh_test(
+        name = "re_poc_probe2_{}".format(i),
+        env = {
+            "HERMIT_BIN": "$(location //hermetic_infra/hermit/hermit-cli:hermit)",
+            "MEM_RACE": "$(location :rustbin_mem_race)",
+            "PROBE_IDX": str(i),
+        },
+        target_compatible_with = ["ovr_config//cpu:x86_64"],
+        test = "re_poc/probe2.sh",
+    )
+    for i in range(24)
+]
+
+[
+    buck_sh_test(
+        name = "re_poc_probe3_{}".format(i),
+        env = {
+            "HERMIT_BIN": "$(location //hermetic_infra/hermit/hermit-cli:hermit)",
+            "HERMIT_VERIFY": "$(location //hermetic_infra/hermit/hermit-verify:hermit-verify)",
+            "MINIMAL_HELLO": "$(location //hermetic_infra/hermit/tests:minimal_hello)",
+            "PROBE_IDX": str(i),
+        },
+        target_compatible_with = ["ovr_config//cpu:x86_64"],
+        test = "re_poc/probe3.sh",
+    )
+    for i in range(3)
+]
```

### NEW FILE (throwaway): fbcode/hermetic_infra/hermit/tests/re_poc/perf_probe.c
```
// RE-POC throwaway: report perf_event_open and ptrace availability with errno.
#include <errno.h>
#include <linux/perf_event.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static void try_perf(const char* name, __u32 type, __u64 config) {
  struct perf_event_attr attr;
  memset(&attr, 0, sizeof(attr));
  attr.size = sizeof(attr);
  attr.type = type;
  attr.config = config;
  attr.exclude_kernel = 1;
  attr.exclude_hv = 1;
  attr.disabled = 1;
  long fd = syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0);
  if (fd < 0) {
    printf("perf_event_open(%s): FAIL errno=%d (%s)\n", name, errno, strerror(errno));
  } else {
    printf("perf_event_open(%s): OK fd=%ld\n", name, fd);
    close((int)fd);
  }
}

int main(void) {
  try_perf("HW_BRANCH_INSTRUCTIONS", PERF_TYPE_HARDWARE, PERF_COUNT_HW_BRANCH_INSTRUCTIONS);
  try_perf("RAW intel rcb 0x5101c4", PERF_TYPE_RAW, 0x5101c4);
  try_perf("RAW amd rcb 0x5100d1", PERF_TYPE_RAW, 0x5100d1);
  try_perf("SW_TASK_CLOCK", PERF_TYPE_SOFTWARE, PERF_COUNT_SW_TASK_CLOCK);

  pid_t child = fork();
  if (child == 0) {
    pause();
    _exit(0);
  }
  if (ptrace(PTRACE_SEIZE, child, 0, PTRACE_O_EXITKILL) != 0) {
    printf("ptrace(PTRACE_SEIZE): FAIL errno=%d (%s)\n", errno, strerror(errno));
  } else {
    printf("ptrace(PTRACE_SEIZE): OK\n");
  }
  kill(child, SIGKILL);
  waitpid(child, NULL, 0);
  return 0;
}
```

### NEW FILE (throwaway): fbcode/hermetic_infra/hermit/tests/re_poc/probe2.sh
```
#!/bin/bash
# RE-POC throwaway probe #2: sample worker CPU vendor vs hermit strict determinism.
set -u
H="$(realpath "$HERMIT_BIN")"
MEM_RACE="$(realpath "$MEM_RACE")"
echo "== probe2 idx=$PROBE_IDX host=$(hostname)"
echo "cpu: $(grep -m1 'vendor_id' /proc/cpuinfo | cut -d: -f2) |$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2) | family=$(grep -m1 'cpu family' /proc/cpuinfo | cut -d: -f2) model=$(grep -m1 '^model' /proc/cpuinfo | cut -d: -f2) | hypervisor=$(grep -c -m1 -w hypervisor /proc/cpuinfo)"
cd "$(mktemp -d)"
echo "== py_hello strict --verify"
timeout 300 "$H" run --strict --base-env=empty --verify -- /bin/bash -c "python3 -c \"print('hello')\"" > py.out 2> py.err; echo "EXIT=$?"
grep -E 'Pmu bugs|WARN|deterministic|differ' py.err | head -3
sed 's/\x1b\[[0-9;]*m//g' py.err | grep -A3 -m1 'Mismatch at' | cut -c1-400
echo "== mem_race strict preemption-timeout=80000000"
timeout 300 env HERMIT_MODE=strict "$H" run --strict --base-env=empty --env=HERMIT_MODE=strict --preemption-timeout=80000000 -- "$MEM_RACE" > mr.out 2> mr.err; echo "EXIT=$?"
grep -E 'Switch points' mr.out; grep -E 'Pmu bugs|WARN|panicked|Expecting' mr.err | head -5
echo "== hermit --log=warn strict /bin/true (PMU bug detection)"
"$H" --log=warn run --strict -- /bin/true 2>&1 | grep -i -E 'pmu|perf' | head -3
exit 0
```

### NEW FILE (throwaway): fbcode/hermetic_infra/hermit/tests/re_poc/probe3.sh
```
#!/bin/bash
# RE-POC throwaway probe #3: does host ASLR explain trace-replay DETLOG pointer mismatches?
set -u
H="$(realpath "$HERMIT_BIN")"
HV="$(realpath "$HERMIT_VERIFY")"
HELLO="$(realpath "$MINIMAL_HELLO")"
echo "== probe3 host=$(hostname) cpu=$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2)"
echo "randomize_va_space=$(cat /proc/sys/kernel/randomize_va_space) personality=$(cat /proc/self/personality 2>&1)"
echo "setarch -R personality=$(setarch x86_64 -R cat /proc/self/personality 2>&1)"
cd "$(mktemp -d)"
tr() { "$@" "$HV" --hermit-bin="$H" --verbose trace-replay --strip-times --isolate-workdir --hermit-arg=--base-env=empty --hermit-arg=--env=HERMIT_MODE=tracereplay -- "$HELLO" > out.txt 2>&1; local rc=$?; echo "EXIT=$rc"; grep -E 'Success|failed' out.txt | head -2; sed 's/\x1b\[[0-9;]*m//g' out.txt | grep -A2 -m2 'Mismatch at' | cut -c1-600; }
echo "== trace-replay, ASLR as-is"; tr env
echo "== trace-replay, setarch -R (ASLR off)"; tr setarch x86_64 -R
echo "== trace-replay, ASLR as-is (again)"; tr env
exit 0
```

### NEW FILE (throwaway): fbcode/hermetic_infra/hermit/tests/re_poc/probe.sh
```
#!/bin/bash
# RE-POC throwaway probe: characterize the test worker and run hermit on trivial guests.
set -u
H="$(realpath "$HERMIT_BIN")"
HELLO="$(realpath "$MINIMAL_HELLO")"
PERF_PROBE="$(realpath "$PERF_PROBE")"
echo "== host"; hostname; uname -a; id; head -2 /etc/os-release
echo "== cwd: $(pwd)"
echo "== self status"; grep -E 'CapEff|CapBnd|Seccomp|NoNewPrivs|TracerPid' /proc/self/status
echo "== sysctls"
for f in /proc/sys/kernel/yama/ptrace_scope /proc/sys/kernel/perf_event_paranoid /proc/sys/user/max_user_namespaces /proc/sys/kernel/unprivileged_userns_clone /proc/sys/kernel/apparmor_restrict_unprivileged_userns; do
    echo "$f: $(cat "$f" 2>&1)"
done
echo "== cpu"; grep -m1 'model name' /proc/cpuinfo; grep -m1 -o -w hypervisor /proc/cpuinfo || echo "no hypervisor flag"; echo "nproc=$(nproc)"
echo "== cgroup"; head -5 /proc/self/cgroup
echo "== pmu devices"; ls /sys/bus/event_source/devices/ 2>&1 | tr '\n' ' '; echo
echo "== unshare"
unshare -Ur true && echo "userns: OK" || echo "userns: FAIL"
unshare -Urpmf --mount-proc true && echo "userns+pid+mnt: OK" || echo "userns+pid+mnt: FAIL"
echo "== perf/ptrace probe"; "$PERF_PROBE"
run() { echo; echo "== RUN: $*"; timeout 300 "$@"; echo "EXIT=$?"; }
cd "$(mktemp -d)"
run "$H" run -- /bin/true
run "$H" run -- /bin/grep TracerPid /proc/self/status
run "$H" run -- /bin/date
run "$H" run --strict -- /bin/true
run "$H" run --strict -- /bin/date
run "$H" run --strict -- "$HELLO"
run "$H" run --strict --preemption-timeout=80000000 -- "$HELLO"
run "$H" run --strict --no-rcb-time -- "$HELLO"
run "$H" run --verify --strict -- /bin/date
run "$H" --log=info run --strict -- /bin/true
exit 0
```

### Additional throwaway change used only for the SPR A/B experiment (already reverted)
```diff
--- a/fbcode/hermetic_infra/reverie/reverie-ptrace/src/timer.rs
+++ b/fbcode/hermetic_infra/reverie/reverie-ptrace/src/timer.rs
@@ -127,7 +127,7 @@
                 0xA5 | 0xA6 => (0x5101c4, 100),               // Intel Cometlake
                 0x8D => (0x5101c4, 100),                      // Intel Tiger Lake
                 0x9A => (0x5101c4, 125),                      // Intel Alder Lake
-                0x8F => (0x5101c4, 125),                      // Intel Sapphire Rapids
+                0x8F => (0x5111c4, 125),                      // Intel Sapphire Rapids  RE-POC throwaway: rr uses 0x5111c4
                 0x86 => (0x5101c4, 100),                      // Intel Icelake
```

Note: the first strict/rr runs (section 3, run4) used an earlier variant of the hermit-verify fix placed in run.rs::build_first_hermit_args (canonicalize(&self.guest_program)); it was moved to cli_wrapper.rs for the rerun.


## 8. Artifacts / links

* Headline run: Buck UI https://www.internalfb.com/buck2/de996a9a-0632-495e-b39a-e70d42662549 , testrun https://www.internalfb.com/intern/testinfra/testrun/35747322082179512
* Strict unmodified fail: https://www.internalfb.com/buck2/3b1f1228-9416-40f7-a8f5-09dce9e77007 ; rr_hello unmodified fail: https://www.internalfb.com/buck2/07bc1126-bf84-44c6-8e1d-542ebd633680
* Strict+rr+probe pass (with path fixes): https://www.internalfb.com/buck2/00fae99a-e2c7-4350-9d90-fe827959cda5 , testrun https://www.internalfb.com/intern/testinfra/testrun/9288674435549887
* Controls: remote-only-only https://www.internalfb.com/buck2/b82bd641-1e1f-43f5-ae44-6d643fca4676 ; local https://www.internalfb.com/intern/testinfra/testrun/1407375417933034
* Sweep: trace 423b3654-41e3-4127-b8d5-5f596f5ba8d8 ; rerun: https://www.internalfb.com/buck2/45f8b643-18b1-4753-aa66-a3ce7b35e247
* Raw logs/excerpts: companion evidence paste P2529899569 (https://www.internalfb.com/intern/paste/P2529899569/).
* GitHub (dev-hermit ai_docs) not updated: github.com unreachable from this node (direct: connection refused; fwdproxy: `CONNECT tunnel failed, response 403`) and no gh/ssh credentials.

RE-POC: passes-on-RE — unmodified hermit_run_default__custombin_minimal_hello (hermit run, Reverie ptrace) ran as RemoteCommand on linux-remote-execution and passed; RE workers allow ptrace/userns/perf, remaining failures are harness paths/inputs and CPU-model (Sapphire Rapids RCB event) issues.
