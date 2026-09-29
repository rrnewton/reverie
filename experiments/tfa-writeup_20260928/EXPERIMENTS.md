# TFA experiment log

Written 2026-09-28. Companion to [README.md](README.md). One entry per experiment,
in time order. Nothing here was re-run for this write-up except the validator check in
section 6, which is read-only. Section 8 lists record-keeping caveats and section 9 lists every
source this write-up read.

Terms, once: a **trial** is one seed tried once under strict verify (Hermit runs the test
twice with that seed and the two detailed scheduling logs must match exactly). A
**configuration** (the records say "cell") is the fixed test binary, Hermit binary, flags
and host; only the seed changes inside it. **Censored** means the trial was discarded
because it did not end in a clean PASS or the declared FAIL (a Hermit error, a timeout, a
loader failure, the two runs differing, or a real failure other than the one declared in
advance).

Repository paths are relative to `/home/newton/work/dev-hermit`. Short names used below, with
the copy kept in this directory where one exists ([evidence/MANIFEST.md](evidence/MANIFEST.md)
maps every copy to its original):

| Short name | Location | Copy in this directory |
|---|---|---|
| `$T` | `/home/newton/work/tfa` (top level and named subdirectories only) | selected small files under [evidence/tfa-lane/](evidence/tfa-lane/), same relative paths |
| `$E3` | `worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case` (phase-two evidence, untracked) | `RESULTS.md` only, as [evidence/phase2-three-case-RESULTS.md](evidence/phase2-three-case-RESULTS.md) |
| `$EV` | `worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-evidence` (Codex diagnostics, untracked) | not copied |
| `P2` | `/tmp/sub-tfa2-030/gather/phase2.md` (evidence notes, not durable) | [evidence/gather-phase2.md](evidence/gather-phase2.md) |
| `CONT` | `/tmp/sub-tfa2-030/gather/continuations.md` (per-configuration recount, not durable) | [evidence/gather-continuations.md](evidence/gather-continuations.md) |
| `CAMP` | `/tmp/sub-tfa2-030/gather/campaign.md` (campaign recount, not durable) | [evidence/gather-campaign.md](evidence/gather-campaign.md) |
| `/tmp/tfa-phase2…` | phase-two scratch files (not durable) | selected files under [evidence/tmp-tfa-phase2/](evidence/tmp-tfa-phase2/) |

## 1. Preparation (2026-09-17)

| Date | Experiment | Counts and result | Sources |
|---|---|---|---|
| 2026-09-17 | Candidate sourcing from TestInfra: rank tests by PFS within each service, keep PFS at least 5%, deduplicate by Buck target, curate toward Linux-native tests | 30 targets, 3 in each of 10 services. PFS 17.22% to 49.47%; native failure rate 0/29 to 72/75 | `ai_docs/tfa-candidate-sourcing-2026-09-17.md` (commit `ee6562142`); `experiments/tfa-kernel71-campaign_20260921/cohort.json` |
| 2026-09-17 | Acceptance rule fixed before any run: a success needs, in one configuration, one seed passing twice and another failing twice with the declared failure, under strict verify | Rule text only | `ai_docs/tfa-acceptance-bracket-2026-09-17.md` lines 122-143 (commit `9d7f603c4`) |
| 2026-09-18 05:01Z | `openat` stack-size reproducer: a tiny program issues one raw `openat` with a chosen number of writable bytes below the stack pointer, under Hermit `79da5231` (binary sha256 `000eeb5e…`) | 192 bytes: `openat raw_rc=-14 errno=14` (EFAULT). 512 bytes: `raw_rc=3` (success). A 5,000-open loop control: `5000 opens completed`. The Hermit log shows the same three lines seen later in the loader failures (inbound `openat`, "Injecting additional fstat", `= Err(Errno(EFAULT))`) | `experiments/tfa-openat-stack-size-20260917/evidence/hermit-906-open-stack-{192,512}.stdout`, `hermit-906-open-loop.stdout`, `hermit-906-open-stack-192-info.stderr` lines 104-109, `hermit-906-open-stack-192.report` |

## 2. Pilot on kernel 6.13 (2026-09-20)

Host devbig030, `Linux 6.13.2-0_fbk17_hardened_0_g2ae417e0caa0`, Hermit `79da5231`, ptrace
backend, strict verify, scheduler seed 0 only. One record per target; 10 records, 9 trials,
0 valid (validator line in section 6).

| Start (UTC, 2026-09-20) | Target | Result | Record |
|---|---|---|---|
| 05:25:45 | Manifold Rust client | Loader exit 127 (missing `libprivacy_enforcer_fci_fci_policy_enforcer.so`) before the guest ran; disk guard not applied | `experiments/tfa-flaky-tests_20260917/services/manifold/manifold-client-tests--ffb0d3de4eb5/` |
| 05:45:53 | ZippyDB VRT usage | Both runs started the test; their output and scheduling logs differed | `…/zippydb/vrt-usage-tracker-test--5a123a3ebda4/` |
| 06:09:43 | ZippyDB DRocksDB | Hermit deadlock (1 thread in futex wait), exit 125; log reached the 1 GiB cap | `…/zippydb/drocksdbtest--af5789c4e317/` |
| 12:45:40 | Folly AsyncSSL | Sanitizer abort on `setrlimit(RLIMIT_CORE)` before the test | `…/folly/async-ssl-socket-test--cefc173a09b4/` |
| 12:56:10 | Proxygen TLS terminator | Hermit PMU skid-overshoot refusal, exit 122 | `…/proxygen/tls-terminator-test--60719afd988e/` |
| 13:03:04 | LogDevice health monitor | Hermit deadlock (1 thread in futex wait) at scheduler turn 219,044, exit 125 | `…/logdevice/tests--679930c8e36e/` |
| 13:13:28 | Folly TimerFD | Sanitizer abort on RLIMIT_CORE, exit 132 | `…/folly/timerfd-timeout-manager-test--246a62382461/` |
| 13:48:52 | Cachelib EventTracker | Sanitizer abort on RLIMIT_CORE | `…/cachelib/event-tracker-test--4b6700e9a3a0/` |
| 13:56:12 | Folly io_uring | Sanitizer abort on RLIMIT_CORE, exit 132 | `…/folly/io-uring-dynamic-provided-buffer-ring-test--2aaba8d28b70/` |
| 14:01:31 | Thrift InstrumentationTest | Sanitizer abort on RLIMIT_CORE, exit 132 | `…/thrift/instrumentationtest--3e07b8fc0ca0/` |

Result field: `$.events[0].result.diagnostic` of each `experiment.json`. The RLIMIT_CORE
refusal (5 of 10) was fixed as "Accept virtual RLIMIT_CORE updates", landed on Hermit main
as `ccbe37e91` (https://github.com/rrnewton/hermit/pull/3142). The main campaign binary
`4630ee2a` carries that patch; it is 50 commits after `79da5231`
(`git -C hermit rev-list --count 79da5231..4630ee2a`).

This one easy Hermit fix did matter (defect 7 in [HERMIT_DEFECTS.md](HERMIT_DEFECTS.md)). After it, 2 of
those 5 targets became successes (AsyncSSL, Thrift) and a third reached its test (TimerFD). The
fix was necessary for those two, but not sufficient: their first configurations after it gave
passes only, and a kernel change and 49 other Hermit commits also separate the two campaigns
(the `rev-list --count` above prints 50, which includes the fix commit `4630ee2a` itself).

## 3. Main campaign on kernel 7.1 (2026-09-21)

Common to all 25 configurations: host devbig030, `Linux 7.1.3-0_fbk0_0_g295ad0959f34`, AMD
EPYC 9D85; Hermit `4630ee2a` (binary 415,330,552 bytes, sha256 `5c422b85…`, still present with
that hash on 2026-09-28 per P2 section 2); ptrace
backend; strict mode; chaos scheduling with the random heuristic; PMU timer, no fallback;
200 ms virtual maximum timeslice; `--seed 0 --rng-seed 0`; strict verify with no
relaxations; 120 s wall limit per trial, 16 GiB memory, no swap, 64 MiB log caps
(`$.cells[0]` of every record; RESULTS.md section 3.2).

### 3.1 Native screen (diagnostic, not formal)

| When | What | Result | Source |
|---|---|---|---|
| 2026-09-21, committed 09:09-10:05 PDT | Bounded native runs of candidate test cases, checking runtime (gate: at most 10 s), thread count, and whether threads compete | 20 table rows covering 21 targets (one Cogwheel row stands for both launchers), plus a closing paragraph covering 6 more (io_uring, the 3 MySQL targets, DRocksDB, the Thrift resource-pool case). AsyncSSL, TimerFD and HTTPCoro are in neither; the GAP audit covers them. Only Thrift InstrumentationTest was promoted, in the last edit (`b819db93c`, 10:05:35 PDT), which replaced its earlier "Reject: ptrace fanout": pinned to CPU 0 it ran with 16 threads (331 unpinned) and gave 3 PASS (0.55-0.59 s) and 1 exact line-836 FAIL (1.57 s). TLS terminator qualified but its configuration was already used up | `experiments/tfa-kernel71-campaign_20260921/NATIVE-CANDIDATE-SCREEN-20260921.md` (commits `8769ecc14` 09:09:57, `21f1c3388` 09:12:49, `9e8d2dcf7` 09:35:53, `65b08810a` 09:51:15, `b819db93c` 10:05:35 PDT; `git log --format='%h %cI %s' --` that file); `…/services/thrift/instrumentationtest--3e07b8fc0ca0/THRIFT-INSTRUMENTATION-NATIVE-PROFILE-20260921.md` |
| 2026-09-21 | Per-target native profiles | HTTPCoro: 5 passes, 0.48-0.69 s wall. AsyncSSL: a native FAIL at 17 ms test time, traced PASS 0.21 s wall. LogDevice health: SIGSEGV at 4.56 s, 657 threads. TLS: PASS 0.24 s wall, 28 threads | Under `experiments/tfa-kernel71-campaign_20260921/services/`: `proxygen/httpcorosessiontests--e3f69b337304/HTTPCORO-SLOWDOWN-ANALYSIS-20260921.md`, `logdevice/tests--679930c8e36e/LOGSTORE-HEALTH-NATIVE-PROFILE-20260921.md`, `proxygen/tls-terminator-test--60719afd988e/TLS-NATIVE-PROFILE-20260921.md`; AsyncSSL: `experiments/tfa-kernel71-sched-continuation_20260921/services/folly/async-ssl-socket-test--cefc173a09b4/ASYNCSSL-SCHED-SLOWDOWN-PLAN-20260921.md` |

The native screen was committed after 9 of the 10 base configurations had already run
(their first runs were 14:27Z-15:11Z, that is 07:27-08:11 PDT). The Thrift promotion
(17:05Z) came before the 10th base configuration, Thrift's own, at 17:19Z. The screen shaped
that configuration and the later follow-ups, not the first 9.

### 3.2 The 25 formal configurations

Numbered in execution order. "Fuzz" sweeps `--fuzz-seed` with `--chaos-target-races` on
(random signals and wake-up perturbations); "sched" sweeps `--sched-seed` (thread order).
"Slowdown F" is `--chaos-per-thread-slowdown --chaos-slowdown-max-factor=F
--chaos-epoch-length-ns=0` (each thread gets one fixed speed between 1/F and F, chosen by
the seed). "Slice" is `--target-timeslice`. "Clock" is `--clock-multiplier`. "Date" is an
explicit `--epoch` (the guest's start date, a time on 2026-09-21); without it Hermit used
2026-01-01. Rows 1-10 used the default date; rows 11-25 all set an explicit date.
Hermit time is safehermit `elapsed_secs` per trial (whole seconds). "Differing runs" means
Hermit's two runs of the same seed did not match under strict verify, so the trial was
censored.

| # | Target | Root (`experiments/tfa-kernel71-…_20260921`) | Settings | Seeds tried | Pass / fail / censored | Outcome | Ran (UTC) | Hermit s per trial: median (range); total |
|---:|---|---|---|---|---|---|---|---|
| 1 | AsyncSSL | `campaign` | fuzz, default date | 0-6 | 7 / 0 / 0 | Passes only; budget met | 14:27-14:27 | 4 (4-5); 29 |
| 2 | Transport monitor | `campaign` | fuzz, default date | 0 | 0 / 0 / 1 | Loader EFAULT (Hermit), exit 127 | 14:30-14:31 | 20; 20 |
| 3 | HTTPCoro | `campaign` | fuzz, default date | 0-15 | 0 / 0 / 16 | All 16 aborted at line 1685, but the declared failure pattern expected a GoogleTest "FAILED" line, so all were censored | 14:32-15:11 | 76 (75-95); 1,234 |
| 4 | TimerFD | `campaign` | fuzz, default date | 0-8 | 9 / 0 / 0 | Passes only; budget used up | 14:42-14:43 | 4 (3-4); 34 |
| 5 | TLS terminator | `campaign` | fuzz, default date | 0-14 | 0 / 0 / 15 | 2 "certificate not yet valid" aborts (not the declared failure); 13 with differing runs | 14:50-14:51 | 3 (3-4); 52 |
| 6 | EventTracker | `campaign` | fuzz, default date | 0 | 0 / 0 / 1 | Outer 120 s timeout, no test output | 14:52-14:54 | 120; 120 |
| 7 | LogDevice health | `campaign` | fuzz, default date | 0 | 0 / 0 / 1 | "Deadlock detected" after a stray timer signal killed 96 of 98 threads; exit 125 | 14:54-14:55 | 38; 38 |
| 8 | LogStore retry | `campaign` | fuzz, default date | 0 | 0 / 0 / 1 | Loader EFAULT (Hermit), exit 127 | 14:57-14:57 | 20; 20 |
| 9 | McRefill | `campaign` | fuzz, default date | 0 | 0 / 0 / 1 | Loader EFAULT (Hermit), exit 127 | 15:11-15:11 | 18; 18 |
| 10 | Thrift InstrumentationTest | `campaign` | fuzz, default date | none admitted | 0 / 0 / 0 | One run passed but was rejected because the disk guard was not applied; no trial | 17:19-17:20 | 16 (rejected run, not a trial) |
| 11 | HTTPCoro | `time-input-campaign` | sched, clock 0.125, slowdown 10, date 18:14:41Z | 7,5,4,2,15,0,3,1 | 8 / 0 / 0 | Passes only; exhausted | 18:34-18:47 | 8 (7-15); 77 |
| 12 | Thrift InstrumentationTest | `time-input-campaign` | fuzz, date 19:01:04Z | 0-29 | 29 / 0 / 1 | Passes only; 1 PMU skid-overshoot refusal (exit 122) | 19:26-19:58 | 15 (15-17); 467 |
| 13 | AsyncSSL | `time-input-campaign` | fuzz, date 19:32:11Z | 0-28 | 29 / 0 / 0 | Passes only | 19:59-20:23 | 4 (3-5); 111 |
| 14 | TLS terminator | `time-input-campaign` | sched, real date 2026-09-21T18:54:35Z | 0-15 | 0 / 0 / 16 | All 16 aborted with `folly::FutureTimeout`; all 16 with differing runs | 20:24-20:48 | 33 (31-35); 525 |
| 15 | Thrift InstrumentationTest | `sched-input-campaign` | sched, date 20:56:38Z | 0-28 | 29 / 0 / 0 | Passes only | 21:01-21:17 | 16 (14-18); 458 |
| 16 | AsyncSSL | `sched-continuation` | sched, slowdown 10, slice 1 ms, date 20:57:56Z | 0-15 | 4 / 0 / 12 | 11 line-3592 failures (a different failure), 1 with differing runs | 21:22-21:36 | 8.5 (3-56); 326 |
| 17 | **HTTPCoro** | `httpcoro-midpoint-continuation` | sched, clock 0.5, slowdown 10, date 21:01:52Z | 0-3 of 0-15 | 1 / 1 / 2 | **Success: pass 3, fail 0**; seed 1 with differing runs, seed 2 outer timeout | 21:37-21:42 | 60 (7-121); 248 |
| 18 | AsyncSSL | `asyncssl-factor2-continuation` | sched, slowdown 2, date 21:44:37Z | 0-15 | 14 / 0 / 2 | Passes only; 2 with differing runs | 21:47-21:54 | 4 (3-5); 65 |
| 19 | **Thrift InstrumentationTest** | `thrift-slowdown-continuation` | sched, slowdown 10, slice 1 ms, date 21:44:37Z | 0-1 of 0-15 | 1 / 1 / 0 | **Success: pass 0, fail 1** | 21:54-21:55 | 13.5 (13-14); 27 |
| 20 | AsyncSSL | `asyncssl-factor5-continuation` | sched, slowdown 5, date 21:57:01Z | 0-15 | 14 / 0 / 2 | Passes only; 2 with differing runs | 21:58-22:03 | 4 (3-8); 73 |
| 21 | AsyncSSL | `asyncssl-factor8-continuation` | sched, slowdown 8, date 21:57:05Z | 0-15 | 16 / 0 / 0 | Passes only | 22:06-22:11 | 5 (3-12); 88 |
| 22 | AsyncSSL | `asyncssl-slice10-continuation` | sched, slowdown 10, slice 10 ms, date 22:13:13Z | 0-15 | 10 / 0 / 6 | 3 line-3592 failures, 3 with differing runs | 22:21-22:27 | 4 (3-57); 162 |
| 23 | AsyncSSL | `asyncssl-slice20-continuation` | sched, slowdown 10, slice 20 ms, date 22:29:06Z | 0-15 | 10 / 0 / 6 | 2 line-3592 failures, 4 with differing runs | 22:32-22:37 | 3 (3-52); 108 |
| 24 | AsyncSSL | `asyncssl-slice50-continuation` | sched, slowdown 10, slice 50 ms, date 22:29:36Z | 0-15 | 12 / 0 / 4 | 3 line-3592 failures, 1 with differing runs | 22:38-22:43 | 4 (3-58); 123 |
| 25 | **AsyncSSL** | `asyncssl-slice5-continuation` | sched, slowdown 8, slice 5 ms, date 22:13:10Z | 0-11 of 0-15 | 5 / 1 / 6 | **Success: pass 1, fail 11**; 5 line-3592 failures, 1 with differing runs | 22:44-22:49 | 5 (3-43); 139 |
| | **Total** | 13 roots | | | **198 / 3 / 93 = 294 trials** | **3 successes** | 14:27-22:49 (8 h 22 min) | **median 5 (3-121); 4,562 s = 76 min** |

Sources: each record's `experiment.json` (`samples[]`, `events[]`, `cells[0]`,
`current_outcome`) under `experiments/tfa-kernel71-<root>_20260921/services/`; per-trial
times from the tracked `evidence/*.safehermit.txt` files (`elapsed_secs=`). The table is
the recount in CONT section 1; its totals equal RESULTS.md lines 259-281. Configurations
2-9 in the "Settings" column swept fuzz seeds with target races; the cause text for the
censored trials comes from each trial's tracked stderr (CAMP section 5).

Rows 11-25 were each planned and committed before they ran (their `cells[0].frozen_at`
precedes their first run), but which settings to try next was chosen after seeing earlier
results.

## 4. Evidence re-checks for this write-up (2026-09-28)

| Check | Result | Source |
|---|---|---|
| Retained Hermit log, LogStore retry | Line 67714: loader `openat(... __log_store_test__shared_libs_symlink_tree/...so)` = `Err(Errno(EFAULT))`, right after "Injecting additional fstat ... fd 3" (line 67713); 3,015 fstat injections in the log | `ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-logstore-verify-3012349-1790002573993825987/run1_log_A6gHB` (`sed -n 67713,67714p`, `grep -c 'Injecting additional fstat'`) |
| Retained Hermit log, LogDevice health | 96 lines `guest terminated by signal ... signal=SIGSTKFLT`; 194 `clone3` log lines (97 calls, inbound plus finish) | `…/brs-tfa-kernel71-logdevice-verify-2445441-1790002126713644463/run1_log_ZbCv2` (`grep -c`) |
| Retained Hermit logs, McRefill and transport monitor | Same EFAULT sequence at line 62277 and line 62511, same guest buffer address `0x7ffffffde180` in all three | FIX section 1.1 (`/tmp/sub-tfa2-030/gather/fixability.md`; copy [evidence/gather-fixability.md](evidence/gather-fixability.md)) |
| CI failure text for all 30 targets, week before 2026-09-28 | See TARGETS.md "Likely cause" column | `/tmp/sub-tfa2-030/ti/` (29 targets) and `/tmp/sub-tfa2-030/ti-timerfd.json`; copies [evidence/ti/](evidence/ti/), [evidence/ti-timerfd.json](evidence/ti-timerfd.json) |

## 5. Phase two: prototype automation (2026-09-23 to 2026-09-28)

This phase built a draft Phabricator stack (`fbcode/testinfra/tfa`, 22 unpublished diffs)
that tries to automate the pipeline from "Hermit witnesses exist" to "draft fix". Details
are in STACK_STATE.md and WAY_FORWARD.md; this is the experiment list only.

| When (PDT) | Experiment | Counts and result | Sources |
|---|---|---|---|
| 2026-09-23 | First real controller run | Thrift reached a qualified report and a local commit `2f755a3be882`; AsyncSSL's staged validation failed (missing Buck resource files); HTTPCoro stopped at an over-strict transcript check. Only the audit survives | `/tmp/tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md` (copy under [evidence/tmp-tfa-phase2/](evidence/tmp-tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md)) |
| 2026-09-24 06:41-06:46 | Controller follow-up real checks, AsyncSSL and HTTPCoro | AsyncSSL seed 11 FAIL (4.53 s), seed 1 PASS (6.75 s), stress seeds 12-14 rc 0, seed 15 rc 1; HTTPCoro rc 134 | `/tmp/tfa-phase2-controller-followup-*.log` (P2 section 3.2); the 7 real-run logs are copied to [evidence/tmp-tfa-phase2/](evidence/tmp-tfa-phase2/) |
| 2026-09-24 13:40-13:46 | Re-qualify the packaged internal Hermit (`a919cce`) | 0 of 8 probes passed: RLIMIT_CORE failures (rc 1, 2, 132) and one PMU clock-counter panic | `/tmp/tfa-phase2/requal-a919-*/result.json` (all 8 copied to [evidence/tmp-tfa-phase2/](evidence/tmp-tfa-phase2/)) |
| 2026-09-24 15:09-18:05 | Unit and containment tests | Unit 125/127, then 132/132; containment 34-36 of 39-40, failures all real-Codex preflight | `/tmp/tfa-phase2/*.out` (P2 section 3.4); `unit-68cf2bac-recheck.out` and `/tmp/tfa-phase2-production-final.out` are copied to [evidence/tmp-tfa-phase2/](evidence/tmp-tfa-phase2/) |
| 2026-09-24 16:55-23:58 | Codex containment diagnostics | Signed Codex: 3 runs, 0 of 4 each. Plain upstream build: 0 of 4. Unsigned 0.156.1 plus two upstream socket fixes: v1, v2, v3 and v5 0 of 4 (v4 was a control run with the signed latest Codex, also 0 of 4); **v6 4 of 4** (207.8 s); v8 38 of 39, v8b 1 of 1 | `$EV/diagnostic/codex-meta0156*-receipt.json` and `codex-meta0156{,-v2,-v3,-v5,-v6}-four-cases.out`, `$EV/diagnostic/codex-signed-latest-v4-four-cases.out`, `$EV/OWNER-ONLY-BLOCKER-meta-codex-socket-isolation.md` |
| 2026-09-25 01:58-02:13 | Relocated witness replays (open-source Hermit `4630ee2a`) | v1: Thrift 4/4, AsyncSSL 4/4, HTTPCoro FAIL timed out at 121.1 s. v2: **12 of 12** as expected | `$E3/relocated/run.out`, `$E3/relocated-v2/run.out` |
| 2026-09-25 03:27-04:04 | Investigator agents (3 Claude agents, `claude-opus-5-5`; the HTTPCoro agent wrote its last output at 04:04) | 4 ranked hypotheses each; 3 test-only candidate patches (831, 851, 929 bytes) | `$E3/investigation/<case>/agent-output/patch.diff`; `$E3/RESULTS.md` section 2.2 |
| 2026-09-25 03:39-04:14 | Controller stress: build each patch, replay unfixed and patched under Hermit | 71 Hermit replays, about 1,379 s. Thrift unfixed FAIL 5 of 17, patched PASS 13 of 13. AsyncSSL unfixed FAIL 5 of 14; patched 9 runs, PASS 8, and the ninth (seed 15) failed differently, at line 3592 with a SEGV, as it did unfixed. HTTPCoro unfixed 9 runs: FAIL 3, PASS 4 and 2 timeouts (seeds 5 and 6); patched PASS 8 of 8. All three "SUPPORTED" | `$E3/RESULTS.md` sections 4 and 7; `$E3/stress/*/*/*/receipt.json` |
| 2026-09-25 13:43-15:07 | Mutation testing of 5 review fixes | 49 of 49 mutants killed | `$T/mutant-*.log`, `$T/mut-*.log` |
| 2026-09-25 14:22-19:44 | Prototype witness runs with the packaged internal Hermit | 18 of 18 refused at PROVENANCE_MISMATCH in 0-5 s; Hermit never ran | `$T/three-case*/SUMMARY.txt` (copies under [evidence/tfa-lane/](evidence/tfa-lane/)) |
| 2026-09-25 19:27-20:47 | Audits: Codex drift, Hermit provenance, sudo fixture; Codex-rollover mutants | 4 distinct mutants, 1 survived the unit tests and was killed only by the 308 s real-host test; full test run 249 pass, 0 fail | `$T/codex-drift/AUDIT.md`, `$T/hermit-provenance/AUDIT.md`, `$T/sudo-fixture/AUDIT.md`, `$T/codex-rollover/` (the three audits, `codex-rollover/full-test.log` and `summary.txt` are copied under [evidence/tfa-lane/](evidence/tfa-lane/)) |
| 2026-09-25 20:53 to 09-26 04:55 | Import watcher (is a fixed Codex or the RLIMIT_CORE fix in fbsource yet?) | 15 checks, all "not ready" | `$T/import-watcher/watch.jsonl` (copy under [evidence/tfa-lane/](evidence/tfa-lane/import-watcher/watch.jsonl)) |
| 2026-09-25 20:16 to 09-26 09:15 | Open-source Hermit mode for the prototype | Qualified on the 4th attempt. Witness batches: 5 complete batches (batches 3-6 and rerun 3 at the submitted head), **30 of 30** witness runs as expected; in rerun 3, HTTPCoro FAIL took 109.2 s against the 120 s limit. 1 batch was invalid (wrong verify mode; its HTTPCoro FAIL run also hit the limit, 137 s, exit 124), and rerun 2 at the submitted head stopped after 3 of 6 when HTTPCoro FAIL timed out. Counting only attempts with the valid verify setting across phase two (relocated v1 and v2, stress, batches 3-6, reruns 2 and 3), 2 of 12 HTTPCoro FAIL-seed attempts timed out (P2 section 3.11). External-mode mutants at the submitted head: 67 killed, 5 survived | `$T/external-hermit/`, `$T/external-oss-mode/mutants-head6/SUMMARY.txt`, `$T/external-oss-mode/DISK-HOLD-4fb8c4120739.txt` (these two files and every `external-hermit/witnesses-*/SUMMARY.txt` are copied under [evidence/tfa-lane/](evidence/tfa-lane/)) |
| 2026-09-26 09:21 | Top two diffs submitted as drafts (D121978134, D121978137) | CI for D121978137: 528 of 559 passed, 30 warnings, 0 failed; the 30 warnings are 15 tests in 2 build modes and also fail on the base revision | `$T/external-oss-mode/submit.out`; `/tmp/sub-tfa2-030/gather/stackstate.md` headlines 3-4 (copy [evidence/gather-stackstate.md](evidence/gather-stackstate.md)) |
| 2026-09-28 11:15 | Re-check on devvm895 (an agentcloud short-lease devvm) and in CI | devvm895 containment 37 pass, 6 fail (5 stale Codex pins, 1 out-of-space error); CI TestX run 1125900439774673: 249 pass, 15 fail (9 sudo mount, 5 Codex identity, 1 missing systemd unit) | `/tmp/sub-tfa2-030/devvm895/tfa-diag/containment-local.log` line 255 (copy [evidence/devvm895/containment-local.log.txt](evidence/devvm895/containment-local.log.txt)); `…/results-dev.json` (243,313 bytes, not copied) |

One of the 6 prototype witnesses differs from the campaign (summarized in [STACK_STATE.md](STACK_STATE.md)):
the prototype's HTTPCoro passing witness is seed 1, with the campaign's seed 3 kept as
`historical_pass_seed` (`fbcode/testinfra/tfa/cases.v1.json` lines 34-36 in the stack-tip copy).
In the campaign's winning configuration seed 1 gave differing runs (CONT section 4); the records
read here do not say why the seed was changed (inference: different paths and mounts).

## 6. Offline validator runs

| When | Command | Result | Source |
|---|---|---|---|
| 2026-09-22 (campaign report) | `python3 experiments/tfa-flaky-tests_20260917/tools/validate.py --root <root>` for the 13 kernel-7.1 roots | All 13 passed; per-root counts in RESULTS.md section 4.1 | `experiments/tfa-kernel71-campaign_20260921/RESULTS.md` lines 283-307 |
| 2026-09-28 | Same command, 14 roots (13 kernel-7.1 plus the pilot root) | All 14 exit 0 in 1.9-57.4 s each. Each prints "conclusion=neither" (no single root has 3 successes) and "evidence_authentication=NOT_CHECKED" (it checks consistency, not who ran what). Pilot root: 10 records, 9 trials, 0 valid | `/tmp/sub-tfa2-030/gather/validate/summary.txt` and `<root>.out` (copies in [evidence/gather-raw/validate/](evidence/gather-raw/validate/)) |
| 2026-09-28 | `python3 -m unittest discover -s experiments/tfa-flaky-tests_20260917/tools -p 'test_*.py'` | 59 tests, OK, 135.7 s | `/tmp/sub-tfa2-030/gather/validate/unittest.out` (copy in [evidence/gather-raw/validate/](evidence/gather-raw/validate/unittest.out)) |

## 7. Not examined here

- The "fixed-binary diagnostic sweep" under `ignored/tfa-kernel71-campaign` is non-formal
  and was not copied into any record (`experiments/tfa-kernel71-campaign_20260921/README.md`).
  This write-up did not read it.

## 8. Record-keeping caveats

Referenced from README.md section 4, item 6. None of these changes the success count (CAMP section 7):

- The "guest started" counter means different things in different roots.
- RESULTS.md calls 28 runs "pre-guest failures" although their stderr shows the guest ran.
- Every root's `metadata.json` still says `SCHEMA_ONLY_NO_EXPERIMENTS_RUN`.

## 9. Provenance of this write-up

Summarized in [SOURCES.md](SOURCES.md) and [metadata.json](metadata.json). Written 2026-09-28 by sub-tfa2-030, a read-only
Claude subagent, for the owner. No test or Hermit run was repeated for this write-up; only the
read-only validator check of section 6 was re-run. Repository records were read
from the local dev-hermit checkout at `1393fcda8e20c4b06f0aec5b32f071a6e9a8fd40` (branch `main`,
committed 2026-09-28T08:59:22-07:00; `git -C /home/newton/work/dev-hermit rev-parse HEAD` and
`git -C /home/newton/work/dev-hermit log -1 --format=%cI`); that checkout was not re-fetched. The
TFA records are identical in the newer local `origin/main` ref (`864ab69db945`,
2026-09-28T15:30:41-07:00): `git diff --stat HEAD origin/main -- 'experiments/tfa-kernel71-*'
experiments/tfa-flaky-tests_20260917 experiments/tfa-openat-stack-size-20260917 'ai_docs/tfa-*'`
printed nothing, and it still printed nothing against the later `origin/main` `8ec66f057997`
(2026-09-28T15:38:15-07:00) in the final completeness pass. Non-repository sources:

1. The old TFA lane `/home/newton/work/tfa` on devbig030 (only named subdirectories were read).
2. The devvm895 scratch copy `/tmp/sub-tfa2-030/devvm895` on devbig030: the stack-tip source, a
   containment-test log and the previous way-forward draft. It is **not durable**.
3. This write-up's gatherer notes and query outputs under `/tmp/sub-tfa2-030/` (not durable).
4. Phase-two scratch files under `/tmp/tfa-phase2/` and `/tmp/tfa-phase2-*` (not durable).
5. Untracked evidence in `worktrees/slots/tfa-phase2-cyber-opus/ignored/` and retained Hermit
   logs in `ignored/safehermit-loopback/` inside the dev-hermit checkout.
6. Read-only Phabricator, TestInfra, sitevar, `sl`, `agentcloudctl inspect` and
   `meta devserver.server describe` queries, and GitHub searches, on 2026-09-28. The verify pass's
   saved outputs are under `/tmp/sub-tfa2-030/verify/` (not durable); the six that the documents cite
   are copied to [evidence/verify/](evidence/verify/).
7. One TaskGraph task, `tfa-diagnose-futuretimeout-strict-hermit` (4 notes dated 2026-09-18, read
   with `tg -v`). A TaskGraph note is not a durable record; the output is copied to
   [evidence/verify/tg-futuretimeout-v.txt](evidence/verify/tg-futuretimeout-v.txt).
8. The owner's heavy-work directive of 2026-09-28, relayed by coord2 in sub-tfa2-030's launch prompt.

Supporting copies of the small cited files from sources 1-5 are in [evidence/](evidence/).
[evidence/MANIFEST.md](evidence/MANIFEST.md) maps each copy to its original path, with sizes and
SHA-256 hashes. Expired signed-link signatures were redacted in three copied logs. The retained
Hermit logs, the stack-tip source tree, the rest of the untracked phase-two evidence trees and two
files over 200 KB were not copied; the manifest lists them.

The old-TFA rate-limit check behind README section 1: `meta sitevar.sv read
--name=TEST_FLAKINESS_ANALYZER_RATE_LIMIT` (2026-09-28) returned `per_day_limit: 0`, and the
trigger was deleted on 2025-10-01 in fbsource `0f046cdf3542` (WF section 0 item 6, in
`/tmp/sub-tfa2-030/gather/wayforward.md`; copy
[evidence/gather-wayforward.md](evidence/gather-wayforward.md)).
