# TFA kernel-7.1 flaky-test campaign: per-target master table and recount

Gatherer: `campaign` (sub-tfa2-030), devbig030, 2026-09-28. Read-only evidence work.

Unless stated otherwise, paths are relative to `/home/newton/work/dev-hermit`. The
local checkout HEAD was `1393fcda8e20c4b06f0aec5b32f071a6e9a8fd40`. I compared it with the
local `origin/main` ref `7424b031dafdc2cc702903981458d754bba9101a` without fetching. `git diff --stat HEAD origin/main -- <campaign paths>` was
empty, and `git status --short` on the campaign directories was clean. The records read here
are therefore the committed records.

Abbreviations used below (defined once):

- **Target**: one canonical Buck test label, the only counting unit.
- **Cell**: one frozen configuration: test ELF, Hermit binary, flags, host, and the single
  swept seed flag.
- **Tuple / sample**: one cell plus one concrete value of the swept seed. It is the unit of S, C, and T.
- **Discovery / confirmation**: a single `hermit run --verify` invocation runs the guest
  twice (`:: Run1...`, `:: Run2...`) and compares the two runs. The record logs run 1 as a
  `DISCOVERY` event and run 2 as a `CONFIRMATION` event, both for the same tuple.
- **PASS**: the selected test ran exactly once and satisfied its sealed success predicate.
  **FAIL**: the selected test ran and matched one predeclared, target-owned failure class.
  **CENSORED**: anything else, such as a Hermit refusal, a timeout, a loader failure, a verify
  mismatch, or a real target failure that was not predeclared.
- **S** = VALID tuples, **C** = CENSORED tuples, **T** = S+C. **R** = `HERMIT_EXECUTION` events
  whose `result.guest_started` is true. **E** = all `HERMIT_EXECUTION` events. None of these is a target count.
- **A/W/B/U** = attempted / WORKING / BLOCKED-WITH-NAMED-CAUSE / NOT-DETERMINABLE targets.

## 0. Headline findings

1. **My recount matches RESULTS.md section 4.1 exactly.** It gives A/W/B/U = 10/3/0/7, S=201, C/T=93/294,
   R=512, 25 material records, and 540 `HERMIT_EXECUTION` events. The same holds for every per-target row and every
   per-root row, and all 14 validators exit 0. The script, its output, and the validator outputs are listed in sections 3 and 4.
2. **The WORKING result rests on three targets, each from a later "continuation" cell.** None of the
   ten original target-races cells produced a bracket. The winning cells are HTTPCoro (PASS
   sched-seed 3, FAIL sched-seed 0), Thrift InstrumentationTest (PASS 0, FAIL 1), and AsyncSSL (PASS 1,
   FAIL 11). The base root alone validates as W=0.
3. **About half of all censored tuples are Hermit-side defects, not target behavior.** This decomposition is mine:
   44 of the 93 are Hermit strict-verify "nondeterministic" verdicts, meaning run 1 and run 2 of the same seed diverged. The remaining Hermit-side causes are
   3 loader-time `openat` returning `EFAULT` in `inject_fstat`, 1 scheduler deadlock, and 1 PMU skid-overshoot
   refusal, for a Hermit-side total of 49/93. Another 42/93 are real target failures that the cell had not predeclared, and 2/93 are outer 120 s
   timeouts (section 5).
4. **Contradiction with RESULTS.md section 3.1.** RESULTS.md explains the 28 executions excluded from R (540 − 512) as
   "pre-guest failures". The tracked stderr of all 28 shows that the guest ran. 26 end in `:: Failure: nondeterministic.`
   after two compared runs, 1 is an outer timeout that fired after `:: Run2...` had started, and 1 is a
   `HERMIT_POLICY_REFUSAL cause=skid-overshoot`. In the continuation roots the field is set as
   `"guest_started": classification.selected_count > 0`
   (for example `experiments/tfa-kernel71-asyncssl-slice5-continuation_20260921/tools/admit_asyncssl_sched_slowdown_seed.py:1027`),
   where `selected_count` counts `[ RUN ]` lines in the retained stdout. The field therefore means "selected-test
   marker seen in stdout", not "guest process started". The base-root tool hard-codes it to `True`.
5. **Under-diagnosed loader failures.** RESULTS.md, the records, and the gap audit describe LogStore retry and McRefill as having
   "failed in the loader". Only the transport monitor was diagnosed as Hermit's `inject_fstat` turning `openat` into `EFAULT`.
   I read the retained raw verify logs of LogStore and McRefill. Both show the same sequence, `Injecting additional fstat ... fd 3` followed by
   `openat(... __shared_libs_symlink_tree/<lib>.so ...) = Err(Errno(EFAULT))`, in both runs. So 3 of the 7
   NOT-DETERMINABLE targets are proximately blocked by one Hermit defect. I found no GitHub issue for it (section 8).
6. **The TLS terminator is proximately blocked by Hermit nondeterminism.** 29 of its 31 tuples are strict-verify
   mismatches (first divergence at `tgkill` teardown or `epoll_wait`). The records phrase these as "did not
   complete strict BitwiseInfoV1 matching". The one closely related open issue is
   https://github.com/rrnewton/hermit/issues/3247, which covers exec-time thread teardown; TLS diverges at abort-time teardown.

## 1. Experimental setup in plain words

Every one of the 25 cells records the same base environment. Source: `campaign_recount.out`,
"cell fact" lines, counted over all 25 `cells[0]`. It also matches RESULTS.md section 3.2.

- **Host / kernel / CPU:** `devbig030`, `Linux 7.1.3-0_fbk0_0_g295ad0959f34`, x86_64,
  `AMD EPYC 9D85 158-Core Processor` (`$.cells[0].host`; 25/25).
- **Hermit:** source `4630ee2a61f93221a43e2baec0a15f34102a2c61`. The time-knob audit names this commit "Accept virtual RLIMIT_CORE updates"
  (`experiments/tfa-kernel71-time-input-campaign_20260921/HERMIT-TIME-KNOB-AUDIT-20260921.md` lines 3-4).
  The binary is
  `/home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a`,
  415,330,552 bytes, SHA-256 `5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a`
  (`$.cells[0].hermit_binary_sha256`, 25/25). The binary is still present today with the same hash
  (`sha256sum` output in `/tmp/sub-tfa2-030/gather/campaign_live_hashes.txt`).
- **Target code:** fbsource revision `7f239d5b37dfbecf32fed144468da461f998add5` (25/25). Test ELFs are
  pre-built Buck outputs under `/home/newton/work/tfa/fbsource/buck-out`. Buck ran outside Hermit, and no
  `BUILD` events exist (event kinds overall: `HERMIT_EXECUTION: 540, LAUNCH: 1`).
  Today only the 3 WORKING ELFs survive, and their hashes match the records (`campaign_live_hashes.txt`). The other 7 ELF paths no longer
  exist because buck-out evicted them (`os.path.exists` check).
- **Backend / policy:** ptrace; `strict=true`; `chaos=true`; `relaxations=[]` (25/25). Scheduler heuristic
  `random` (25/25). PMU `{"available": true, "effective_timer": "RCB", "fallback_recorded": false}` (25/25).
  `--max-timeslice 200000000` (200 ms of virtual time) in every cell.
- **Verification:** `--verify --verify-strict --verify-allow both --keep-logs`, comparison `BitwiseInfoV1`,
  relaxations none. Hermit runs the guest twice and compares the canonical INFO scheduler logs exactly.
- **Bounds (per invocation, safehermit):** 120 s wall (`--sh-deadline 120`), 17,179,869,184 B memory, 0 swap,
  67,108,864 B stderr and retained-log caps, `8G` disk guard. 14 of the 25 cells also record a 67,108,864 B stdout
  cap. The other 11 (the 10 base cells and the time-input HTTPCoro cell) do not
  (`cell fact bounds` in `campaign_recount.out`).
- **Guest:** `--base-env=minimal`, `LC_ALL=C`, `TZ=UTC`, the buck-out bind-mounted at the identical path, and argv
  = absolute ELF + `--gtest_filter=<case>`. All 540 execution argvs start with `bin/safehermit`,
  all 540 have an absolute buck-out ELF right after `--`, and 0 contain a `buck` token (`campaign_recount.out`).
- **Seeds:** `--seed`, `--rng-seed`, and one of `--sched-seed`/`--fuzz-seed` are fixed at 0, and exactly one flag is swept.
  12 cells swept `--fuzz-seed` with `--chaos-target-races` on (the base target-races cells). 13 cells swept
  `--sched-seed` (`cell fact swept: {'--sched-seed': 13, '--fuzz-seed': 12}`).
  The continuation cells add time and scheduler controls: `--epoch=<planning-time UTC>`, `--clock-multiplier`,
  `--chaos-per-thread-slowdown --chaos-slowdown-max-factor=F --chaos-epoch-length-ns=0`, and sometimes
  `--target-timeslice N` (per-record `controls=` lines in `campaign_recount.out`).
- **Seed → execution mapping:** one seed value produces one `bin/safehermit … hermit run … --verify … -- ELF filter`
  invocation, which yields 2 guest runs. When both runs complete and match, the record has 2 events for that tuple
  (DISCOVERY + CONFIRMATION). When the verifier refuses, only 1 event is kept. Overall there are 294 DISCOVERY and 246 CONFIRMATION
  events. The 48 tuples without a confirmation are 44 nondeterministic verdicts, 2 timeouts, 1 skid refusal, and 1 deadlock
  (sections 5 and 9).
- **Sampling plan:** the base cells used a rate-bound N, the smallest N with `p^N+(1-p)^N <= 0.05`, where p is the
  native failure rate used only as a planning prior. Each also had an attempted-tuple cap: for example Cachelib N=86 with cap 94, and AsyncSSL
  N=7 with cap 11. The continuation cells used finite populations, mostly seeds 0-15. The stop rule was `BOTH_ARMS_THEN_REPEAT`:
  stop once two distinct seeds give a repeated PASS and a repeated predeclared FAIL, otherwise exhaust the budget.
- **Acceptance (WORKING):** in one cell, one seed must PASS in both runs and a different seed must hit the same
  predeclared FAIL class in both runs. Each arm needs strict verify `verified=true, bitwise_parity=true,
  BitwiseInfoV1, relaxations=[]` (`ai_docs/tfa-acceptance-bracket-2026-09-17.md` lines 122-143). SUCCESS
  requires W >= 3 distinct targets (same file, line 220).
- **Time window:** HERMIT_EXECUTION `started_at`/`finished_at` span 2026-09-21 14:27:14Z to 22:49:33Z, about 8 h 22 min
  (07:27-15:49 PDT). Median per-invocation wall time by target: AsyncSSL 4 s, TimerFD 4 s, TLS 4 s, Thrift 15 s,
  HTTPCoro 75 s (max 121 s), LogStore 20 s, McRefill 18 s, monitor 20 s, LogDevice health 38 s, Cachelib 120 s
  (the timeout). Source: `campaign_recount.out` "HERMIT_EXECUTION wall window". Both replicas of one invocation share
  the same timestamps, so the summed 8,195 s double-counts.

## 2. The 30-row master table

Registry fields come from `experiments/tfa-kernel71-campaign_20260921/cohort.json` `$.targets[i]`. This file is
byte-identical to the other 13 roots' `cohort.json` (SHA-256 `202e90b3…`, 14/14) and to
`ai_docs/tfa-candidate-sourcing-2026-09-17.md` lines 85-164. Native-screen text is quoted or condensed from
`experiments/tfa-kernel71-campaign_20260921/NATIVE-CANDIDATE-SCREEN-20260921.md`. Gap-audit category comes from
`experiments/tfa-kernel71-campaign_20260921/TERMINAL-AND-CANDIDATE-GAP-AUDIT-20260921.md`, "Candidate-gap audit"
bullets. The categories are: EXH = "short scheduler-sensitive, already exhausted" (2); INS = "scheduler-insensitive" (3); FAN =
"ptrace-dominating fanout" (5); RT = "exceed native runtime/setup gate or do not execute the selected case" (13);
INC = "incompatible routes" (7). The outcome columns come from my recount (`campaign_recount.out`, "Per-target union").
"k6.13" is the earlier kernel-6.13 schema-root record in `experiments/tfa-flaky-tests_20260917`. It is context only and
not part of the kernel-7.1 union.

| # | Service | Canonical target | Selected case | Mode | PFS | Native fail n/d | Native screen (runtime; threads; disposition) | Gap cat. | k7.1 attempted | Outcome | Recs | S | C/T | R | E | PASS / FAIL seed | Proximate reason (non-WORKING) | k6.13 record |
|---|---|---|---|---|---:|---:|---|---|---|---|---:|---:|---:|---:|---:|---|---|---|
| 1 | cachelib | `…/cogwheel:cogwheel_cachelib_sanitizer_free_list_stress-test` | NAMED_TEST `cachelib_cogwheel_test` | (none; `buck run …-launcher`) | 31.04% | 21/60 | only `.par` launcher output, no local test ELF; Reject | INC | N | not attempted | 0 | – | – | – | – | – | – | none |
| 2 | cachelib | `…/cogwheel:cogwheel_cachelib_free_list_stress-test` | NAMED_TEST `cachelib_cogwheel_test` | (none; launcher) | 27.80% | 7/26 | same launcher-only row; Reject | INC | N | not attempted | 0 | – | – | – | – | – | – | none |
| 3 | cachelib | `fbcode//cachelib/common/tests:event-tracker-test` | `EventTrackerTest.NvmCacheWithEventTracker` | dev | 22.99% | 1/29 | entered case, no completion by 13 s; no profile; Reject (runtime) | RT | Y | NOT-DETERMINABLE | 1 | 0 | 1/1 | 1 | 1 | – | outer 120 s timeout in verify run 1, no stdout; seeds 1-93 deferred | ND: ASan abort on RLIMIT_CORE |
| 4 | folly | `fbcode//folly/io/async/test:async_ssl_socket_test` | `AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect` | libcxx | 36.87% | 37/96 | not in screen table (formal attempts came before the screen) | EXH | Y | **WORKING** | 10 | 122 | 38/160 | 295 | 306 | PASS sched 1 / FAIL sched 11 | – | ND: ASan abort on RLIMIT_CORE |
| 5 | folly | `fbcode//folly/io/async/test:timerfd_timeout_manager_test` | `TimerFDTimeoutManagerTest.ReschedTest` | dev-tsan | 30.54% | 28/93 | not in screen table | INS | Y | NOT-DETERMINABLE | 1 | 9 | 0/9 | 18 | 18 | – | budget exhausted: 9/9 valid tuples all PASS | ND: TSan abort on RLIMIT_CORE |
| 6 | folly | `…:io_uring_dynamic_provided_buffer_ring_test` | `IoUringDynamicProvidedBufferRingTest.BufferCountCheck` | dev-tsan | 30.29% | 22/100 | excluded: Hermit intentionally returns ENOSYS for io_uring | INC | N | not attempted | 0 | – | – | – | – | – | – | ND: TSan abort on RLIMIT_CORE |
| 7 | logdevice | `fbcode//logdevice/admin/health_monitor:tests` | `LogStoreHealthMonitorTest.DisabledRebuilding` | opt | 48.63% | 65/96 | SIGSEGV at 4.56 s; traced PASS 2.673 s; 657 TIDs, 16,739 futexes; Reject (fanout) | FAN | Y | NOT-DETERMINABLE | 1 | 0 | 1/1 | 1 | 1 | – | Hermit deadlock (2 FutexWait threads) then container-child-exit, exit 125 | ND: FutexWait deadlock, exit 125 |
| 8 | logdevice | `fbcode//logdevice/fb:tests` | `SslAuthenticationTest…VerifyServerAuthentication/WithoutSessionResumption` | dev | 31.77% | 33/94 | entered case, no completion by 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| 9 | logdevice | `…/log_store/test:log_store_test` | `LogStoreTest.StoreRecordCondWriteFailedSameEpoch_FallbackReadFailsAndRetries` | dev | 27.71% | 3/32 | no GTest marker by 13 s; Reject | RT | Y | NOT-DETERMINABLE | 1 | 0 | 1/1 | 2 | 2 | – | exit 127 in dynamic loader; my raw-log read: `openat` = EFAULT right after `inject_fstat` | none |
| 10 | manifold | `…/versioned_prod:crypto_plugin_integration_test` | `CryptoPluginIntegrationTest.EmptyPayloadAtPluginBoundary` | dev | 48.99% | 72/97 | case skipped (`CRYPTO_PLUGIN_SO_PATH` unset); 5.28 s, ~2.28 GiB RSS; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| 11 | manifold | `…/client/impl/tests:manifold_client_test` | `core/ClientTestWithBucketTypes.TestPayloadSizeBasedTimeout/sr_unified_nosidecar` | dev | 48.31% | 39/84 | entered case, no completion by 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| 12 | manifold | `fbcode//manifold/clients/rust:manifold_client_tests` | `test_real_create_then_delete_dir` | dev | 46.03% | 7/96 | no completion by 13 s; `--test-threads=1`; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | ND: LAUNCH exit 127 (missing `libprivacy_enforcer…so`) |
| 13 | mcrouter | `…/toaster_tests:mc_refill_route_wrapper_test` | `McRefillWrapperBucketized/0.refillOnMiss` | dev-tsan | 49.39% | 77/97 | no GTest marker by 25 s; Reject | RT | Y | NOT-DETERMINABLE | 1 | 0 | 1/1 | 2 | 2 | – | exit 127 in loader; my raw-log read: same `inject_fstat` EFAULT | none |
| 14 | mcrouter | `…:shadow_thread_affinity_integration_test` | `ShadowThreadAffinityIntegrationTest.DisableJumpThreads` | opt-tsan | 49.00% | 68/98 | PASS 8.641 s / 11.22 s wall; 338 TIDs; Reject (wall + fanout) | FAN | N | not attempted | 0 | – | – | – | – | – | – | none |
| 15 | mcrouter | `…:mux_io_thread_pool_executor_toaster_test` | `MuxIOThreadPoolExecutorWithThreadAffinityToasterTest/0.BasicTest` | opt-tsan | 48.57% | 50/88 | suite setup > 13 s; multiple ucache processes; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| 16 | mysql | `…/mysql-test:mtr_parallel_main_1` | `main.explain_for_connection_small_json---rocksdb_intrinsic_table` | dbgo, aarch64 | 49.42% | 79/96 | requires the AArch64 MTR route | INC | N | not attempted | 0 | – | – | – | – | – | – | none |
| 17 | mysql | `…/mysql-test:mtr_parallel_rocksdb_stress` | `rocksdb_stress.rocksdb_ha_consistent_snapshot_stress` | dbgo, aarch64 | 49.39% | 82/95 | AArch64 MTR | INC | N | not attempted | 0 | – | – | – | – | – | – | none |
| 18 | mysql | `…/mysql-test:mtr_parallel_main_no_tp_mutex_1` | `main.explain_for_connection_small_json---rocksdb_intrinsic_table` | dbgo, aarch64 | 49.30% | 21/50 | AArch64 MTR | INC | N | not attempted | 0 | – | – | – | – | – | – | none |
| 19 | proxygen | `…/transport/monitor/test:proxygen_transport_monitor_test` | `…PopulateEdgeConnectionsLoggerEvent/without_response` | dev-asan | 33.98% | 2/30 | PASS 36 ms / 4.25 s; 5 TIDs; case is synchronous; Reject | INS | Y | NOT-DETERMINABLE | 1 | 0 | 1/1 | 2 | 2 | – | exit 127 in loader; diagnosed in-record: `inject_fstat` turns `openat` into EFAULT | none |
| 20 | proxygen | `…/tlsterm/test:tls_terminator_test` | `TlsTerminatorTest/…terminateTls/h1_innerTls_http_connect` | opt | 23.44% | 23/96 | PASS 182 ms / 0.24 s; 28 TIDs; qualifies, but its cell is exhausted | EXH | Y | NOT-DETERMINABLE | 2 | 0 | 31/31 | 19 | 33 | – | 29/31 Hermit strict-verify nondeterministic; 2/31 certificate-not-yet-valid abort (class not predeclared) | ND: PMU skid-overshoot refusal, exit 122 |
| 21 | proxygen | `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` | `…H2DownstreamSessionTest.ResetRateLimit/h2` | dev-asan | 21.70% | 1/30 | not in screen table | INS | Y | **WORKING** | 3 | 10 | 18/28 | 52 | 54 | PASS sched 3 / FAIL sched 0 | – | none |
| 22 | thrift | `fbcode//thrift/lib/cpp2/test:coroutine_test` | `CoroutineTest/…/0.SumThrowsUserExPrimitive` | dev-tsan | 23.10% | 0/29 | PASS 1.619 s / 2.61 s; 331 TIDs; Reject (fanout) | FAN | N | not attempted | 0 | – | – | – | – | – | – | none |
| 23 | thrift | `…:coroutine_test_resource_pools` | `CoroutineTest/…/0.SumThrows` | opt-tsan | 20.19% | 0/22 | "same high-fanout server scaffold" | FAN | N | not attempted | 0 | – | – | – | – | – | – | none |
| 24 | thrift | `fbcode//thrift/lib/cpp2/test:InstrumentationTest` | `RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest` | dev-tsan | 17.22% | 0/29 | unrestricted PASS 909 ms, 331 TIDs; pinned to CPU 0: 16 TIDs, 3 PASS and one line-836 size-2 FAIL natively; **Promote** | FAN (audit predates promotion) | Y | **WORKING** | 4 | 60 | 1/61 | 120 | 121 | PASS sched 0 / FAIL sched 1 | – | ND: TSan abort on RLIMIT_CORE |
| 25 | warm_storage | `…/ws_durability:write_time_encoding_selection_test` | WHOLE_TARGET | opt | 49.43% | 88/100 | onebox setup/backoff > 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | empty dir only |
| 26 | warm_storage | `…/file_service:file_service_tests` | `FileCacheTest.getFilesFromNVM` | dev | 49.42% | 90/96 | no GTest marker by 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| 27 | warm_storage | `…/vnext/tests/api:bc_vnext_create_reader_retry_tests` | `CreateReaderRetryTests.testGetBlockInfoRetryBypassesCacheOnBlockNotFound` | `…/mode-branch-dev` | 49.15% | 60/93 | entered retry path, no completion by 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| 28 | zippydb | `…/common/tests:vrt_usage_tracker_test` | `VRTUsageTrackerTest.BumpWithIOTagTest` | opt | 49.47% | 72/75 | target-owned counter failure, then retry/sleep past 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | ND: verify runs diverged |
| 29 | zippydb | `…/drocksdb/tests:DRocksDBTest` | `DRocksDBTest.DestroySucceedsOnPopulatedBackend` | opt | 49.44% | 90/97 | "previously filled a 1 GiB retained log while deadlocked" | INC | N | not attempted | 0 | – | – | – | – | – | – | ND: deadlock + 1 GiB INFO log cap |
| 30 | zippydb | `…/zgateway/server/tests:admin_handler_e2e_test` | `UseCases/AdminHandlerE2ETest.CrosssShardTxnReserveWithoutConditions/0` | dev | 49.43% | 38/47 | no GTest marker by 13 s; Reject | RT | N | not attempted | 0 | – | – | – | – | – | – | none |
| | | **Totals** | | | | | | 2/3/5/13/7 | **10 Y** | **3 W, 0 B, 7 U** | **25** | **201** | **93/294** | **512** | **540** | | | 10 records, all ND, S=0 |

Notes on the table:

- The gap-audit categories sum to 2+3+5+13+7 = 30. The audit counts InstrumentationTest among "two Thrift cases with 331 TIDs"
  (FAN). It was committed at 9e8d2dcf7, 2026-09-21 09:35:53 PDT, **before** the screen's "Promote" row and the Thrift plan
  seal (b819db93c, 10:05:35 PDT). So the audit is a stale snapshot for row 24 (section 7).
- The k6.13 column reflects `experiments/tfa-flaky-tests_20260917/services/*/*/experiment.json`: Hermit `79da523137db`, kernel
  `Linux 6.13.2-0_fbk17_hardened_0_g2ae417e0caa0`, same host, events on 2026-09-20, `--sched-seed` swept. Five of those
  ten records died on sanitizer `setrlimit/prlimit64(RLIMIT_CORE)`. The kernel-7.1 campaign then used a Hermit build
  titled "Accept virtual RLIMIT_CORE updates" (inference: this was the fix for that failure mode).

## 3. Per-target detail with quoted record fields

`R0` = `experiments/tfa-kernel71-campaign_20260921/services`. Each quote gives the record path and a JSON path.

### WORKING targets

**HTTPCoro** (`fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests`), 3 records:

1. Base target-races cell, fuzz-seed 0-15 (`R0/proxygen/httpcorosessiontests--e3f69b337304/experiment.json`). All 16 CENSORED.
   `$.samples[0].censor_reason`: "The sealed gtest-selected-test-failure class requires a [  FAILED  ] line and one-failed-test summary; retained stdout ends after [ RUN ], while stderr records the selected test's line-1685 fatal CHECK/SIGABRT, a target-owned class that was not declared before these executions."
   `$.events[0].result.diagnostic`: "status=CENSORED; selected=1; oracle=predeclared-gtest-failure-class-not-satisfied; observed=line-1685-fatal-check-sigabrt; exit=134; BitwiseInfoV1=matched".
   The outcome history shows this was reclassified at 18:05:52Z (`httpcoro-kernel71-oracle-integrity-repair-seed-015`).
   The gap audit written earlier listed these tuples as "16 / 16 valid".
2. Time-input cell (clock 0.125): 8 PASS (seed order `[7,5,4,2,15,0,3,1]`), finite population, exhausted
   (`experiments/tfa-kernel71-time-input-campaign_20260921/services/proxygen/httpcorosessiontests--e3f69b337304/experiment.json`).
3. **Midpoint cell (clock 0.5)**, `httpcoro-kernel71-ptrace-strict-midpoint-slowdown-v1`, cell sha256 `c2628d64…`:
   seeds 0=FAIL, 1=CENSORED, 2=CENSORED, 3=PASS.
   `$.current_outcome.pass_witness.sample_id` = `httpcoro-midpoint-sched-00000000000000000003-sample`, and
   `$.current_outcome.fail_witness.sample_id` = `…00000000000000000000-sample`. The FAIL class is
   `reset-rate-limit-line1685-body-event-fatal`: exit 134 with SIGABRT in both replicas. Both matchers have `details` =
   `{verified: true, bitwise_parity: true, comparison: BitwiseInfoV1, relaxations: []}`.
   Seed 1 is `$.samples[1].censor_reason` "did not satisfy the exact predeclared PASS or line-1685 SIGABRT target-owned FAIL predicate", but its
   stderr ends `:: Failure: nondeterministic.` with first divergence at `clock_gettime(CLOCK_MONOTONIC)`. Seed 2 is
   `$.samples[2].censor_reason` "The outer 120-second safehermit deadline fired."

**Thrift InstrumentationTest** (`fbcode//thrift/lib/cpp2/test:InstrumentationTest`), 4 records:

1. Base record: no tuple. `$.events[0]` is kind `LAUNCH`. Its diagnostic reads: "…reached the exact selected test and emitted a strict
   BitwiseInfoV1-matched GTest PASS, but safehermit reported bound.disk=NOT_APPLIED… rejected LAUNCH boundary". S=C=T=R=0.
2. Sched-input cell (sched-seed, `--epoch` set to now): 29/29 PASS, exhausted.
3. Time-input target-races cell: 29 PASS plus 1 CENSORED. The censored one is seed 16 (`$.events[32].result.exit_code`=122), and its stderr reads
   `HERMIT_POLICY_REFUSAL class=policy-refusal cause=skid-overshoot count=2`.
4. **Slowdown cell** `thrift-instrumentation-kernel71-ptrace-strict-chaos-sched-slowdown-now-v1` (sha256 `e882c837…`), with
   factor 10, target timeslice 1,000,000 ns, and clock 1.0. Seed 0 = PASS (exit 0 in both replicas). Seed 1 = FAIL `gtest-snapshot-size-2`
   (exit 1 in both). T=2, so the stop rule fired after two seeds.

**AsyncSSL** (`fbcode//folly/io/async/test:async_ssl_socket_test`), 10 records. The per-record sample sequences
are in `campaign_recount.out` under "Compact per-record summary":

- Base target-races: 7/7 PASS, exhausted. Time-input target-races: 29/29 PASS.
- Slowdown cells, all sched-seed 0-15:
  - factor10 + 1 ms slice: 4 PASS, 12 CENSORED.
  - factor2: 14 PASS, 2 CENSORED.
  - factor5: 14 PASS, 2 CENSORED.
  - factor8: 16 PASS.
  - factor10 + 10 ms: 10 PASS, 6 CENSORED.
  - factor10 + 20 ms: 10 PASS, 6 CENSORED.
  - factor10 + 50 ms: 12 PASS, 4 CENSORED.
- **factor8 + 5 ms slice** (`asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-factor8-slice5000000-now-v1`, sha256
  `3ef3715f…`): seeds 1, 4, 5, 6, 8 = PASS; 11 = FAIL `gtest-byte-events-three-missing-ack`; 0, 2, 3, 7, 9, 10 = CENSORED.
  The witnesses are PASS seed 1 and FAIL seed 11 (`$.current_outcome.pass_witness` / `fail_witness`).
- The 38 AsyncSSL censored tuples split as follows. 24 show only `AsyncSSLSocketTest.cpp:3592` failures in stdout, a target failure outside
  the predeclared class. 14 are strict-verify nondeterministic verdicts (section 5).

### NOT-DETERMINABLE targets (proximate reason with quotes)

- **Cachelib EventTracker**, `R0/cachelib/event-tracker-test--4b6700e9a3a0/experiment.json`:
  - `$.events[0].result` = `{exit_code: 124, timed_out: true, timeout_origin: "OUTER", harness_status: "TIMEOUT", selected_test_executed_count: 0}`.
  - `$.samples[0].censor_reason` "The selected test did not produce a qualifying target verdict."
  - The tracked stderr is 11 bytes (`:: Run1...`), and safehermit reports "DEADLINE: the run reached 120 and was killed by its cgroup".
  - `$.current_outcome.pending_in_scope_measurements[0]`: "Seeds 1-93 remain in the predeclared population, but further execution is deferred after the selected native test exceeded 13 seconds and formal seed 0 reached the 120-second outer timeout."
  - `measurement_budget_exhausted=false`. Plan: N=86, cap 94, and only 1 was attempted.
- **Folly TimerFD**, `R0/folly/timerfd-timeout-manager-test--246a62382461/experiment.json`: 9/9 VALID PASS, which reaches N=9 (cap 13).
  `$.current_outcome.measurement_budget_exhausted` = true. `missing_or_ambiguous_facts[0]` = "A complete
  distinct-seed PASS/target-owned-FAIL bracket is not yet present." Proximate reason: PASS only, within budget.
- **LogStore retry**, `R0/logdevice/log-store-test--589f359badef/experiment.json`:
  - `$.events[0].result.diagnostic` "status=CENSORED; selected=0; unit_result=exit-code; exit=127; BitwiseInfoV1=matched".
  - Tracked stderr: "error while loading shared libraries: libconfigerator_structs_warm_storage_thrift_ws_request_tag-cpp2-types.so: cannot open shared object file: No such file or directory".
  - `$.current_outcome.pending_in_scope_measurements[0]` "…formal seed 0 failed in the loader before the target oracle."
  - **My raw-log reading:** in `ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-logstore-verify-3012349-1790002573993825987/run1_log_A6gHB`
    (17,813,784 B, sha256 `26676250…`), line 67713 is `Injecting additional fstat to retrieve file metadata on fd 3.`, and line
    67714 is `openat(-100, …/__log_store_test__shared_libs_symlink_tree/libconfigerator_structs_warm_storage_thrift_ws_request_tag-cpp2-types.so, O_CLOEXEC) = Err(Errno(EFAULT))`.
    Run 2 (`run2_log_N1HWb`) has the identical line number. This matches the monitor diagnosis. I cannot now check whether the DSO existed, because the ELF
    and symlink tree are gone from buck-out.
- **LogDevice health**, `R0/logdevice/tests--679930c8e36e/experiment.json`:
  - `$.events[0].result` exit 125, `BitwiseInfoV1=not-matched`, 1 replica only.
  - Stderr: "Deadlock detected: thread(s) waiting on futex, but no runnable threads left. turn 39776 … dtid 3: FutexWait … dtid 79: FutexWait", followed by
    "HERMIT_INTERNAL_FAILURE class=container-child-exit status=Exited(1)".
  - `$.current_outcome.pending_in_scope_measurements[0]` cites "native profiling found 657 threads".
- **McRefill**, `R0/mcrouter/mc-refill-route-wrapper-test--82eb51b9426d/experiment.json`:
  - Exit 127 in both replicas, matched.
  - Stderr: "libaxon_common_if_control_plane_service-cpp2-clients.so: cannot open shared object file".
  - **My raw-log reading:** in `…/brs-tfa-kernel71-mcrefill-verify-3174621-1790002707339803842/run1_log_Xc7VF` (16,585,633 B, sha256
    `2d9b718d…`), line 62277 is `openat(… __mc_refill_route_wrapper_test__shared_libs_symlink_tree/libaxon_common_if_control_plane_service-cpp2-clients.so …) = Err(Errno(EFAULT))`,
    right after `Injecting additional fstat … fd 3`. Run 2 has the same line.
- **Proxygen transport monitor**, `R0/proxygen/proxygen-transport-monitor-test--fcaf56433e7f/experiment.json`:
  - Exit 127 in both replicas, matched.
  - Stderr: "libdataswarm_if_dataswarm_thrift_dataset-rust.so: cannot open shared object file".
  - Diagnosis in `…/PROXYGEN-MONITOR-NATIVE-AND-LOADER-DIAGNOSIS-20260921.md`: "the enclosing guest `openat` completes as `Errno(EFAULT)` from that post-open path". The proven boundary is
    `detcore/src/syscalls/files.rs::inject_fstat` (lines 687-718 at 4630ee2a), and the leading hypothesis is a scratch-stack
    commit crossing an unmapped page.
  - `$.current_outcome.pending_in_scope_measurements[0]`: "Continue the committed predeclared fuzz-seed prefix without changing the cell."
- **Proxygen TLS terminator**, 2 records:
  - Base cell (`R0/proxygen/tls-terminator-test--60719afd988e/experiment.json`), 15/15 cap:
    - `$.samples[11].censor_reason` "…stderr records an uncaught certificate-not-yet-valid AsyncSocketException and SIGABRT under the fixed 2026-01-01 epoch."
      This applies to seeds 11 and 12, with `$.events[11].result.diagnostic` "…observed=certificate-not-yet-valid-asyncsocketexception-sigabrt; exit=134; BitwiseInfoV1=matched".
    - The other 13 are `$.events[0].result.diagnostic` "status=CENSORED; selected=1; unit_result=exit-code; exit=1; BitwiseInfoV1=not-matched".
      Their stderr ends `:: Failure: nondeterministic.`, and the first divergence is `logically_kill: Scheduler removing all knowledge of [det]tid 13` in one run vs `tid 11` in the other, after `tgkill(3, 9, 6)`.
    - `$.current_outcome.missing_or_ambiguous_facts[1]`: "The other 13 attempted tuples did not complete strict BitwiseInfoV1 matching; no qualifying PASS or FAIL arm exists."
  - Time-input cell (`experiments/tfa-kernel71-time-input-campaign_20260921/services/proxygen/tls-terminator-test--60719afd988e/experiment.json`), sched-seed 0-15:
    - All 16 CENSORED, all 16 stderr end `:: Failure: nondeterministic.`
    - 14 of the 16 have `guest_started=false`. Their stderr shows the guest printing "configerator proxy2+fuse unhealthy; this may lead to timeouts or fallbacks to system configs" and
      "terminate called after throwing an instance of 'folly::FutureTimeout'". That is a predeclared FAIL class in this cell (`tls-selected-uncaught-future-timeout-abort`), but it
      cannot count because the two runs diverged.
    - Inference: the test depends on an external configerator service that the guest cannot reach deterministically.

## 4. Offline validator runs

Commands were run from `/home/newton/work/dev-hermit`, one per root, as
`python3 experiments/tfa-flaky-tests_20260917/tools/validate.py --root <root> > file 2>&1; rc=$?`. Outputs are in
`/tmp/sub-tfa2-030/gather/validate/<root>.out`, and rc and seconds are in `/tmp/sub-tfa2-030/gather/validate/summary.txt`.
The validator blob is `409bd2a6232780a31369a4dd5f7e129d167f57f6` (last changed 6bdc84664, 2026-09-20).

| Root | rc | secs | Printed line (verbatim) |
|---|---:|---:|---|
| tfa-flaky-tests_20260917 (schema root, k6.13) | 0 | 5.3 | `OK: cohort=30 records=10 planned=0 A=10 W=0 B=0 U=10 S=0 C=9 T=9 R=9 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-factor2-continuation_20260921 | 0 | 12.1 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=14 C=2 T=16 R=28 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-factor5-continuation_20260921 | 0 | 11.1 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=14 C=2 T=16 R=29 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-factor8-continuation_20260921 | 0 | 11.9 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=16 C=0 T=16 R=32 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-slice10-continuation_20260921 | 0 | 11.4 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=10 C=6 T=16 R=28 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-slice20-continuation_20260921 | 0 | 10.6 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=10 C=6 T=16 R=24 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-slice50-continuation_20260921 | 0 | 12.1 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=12 C=4 T=16 R=30 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-asyncssl-slice5-continuation_20260921 | 0 | 9.0 | `OK: cohort=30 records=1 planned=0 A=1 W=1 B=0 U=0 S=6 C=6 T=12 R=22 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-campaign_20260921 | 0 | 23.2 | `OK: cohort=30 records=10 planned=0 A=10 W=0 B=0 U=10 S=16 C=36 T=52 R=89 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-httpcoro-midpoint-continuation_20260921 | 0 | 2.4 | `OK: cohort=30 records=1 planned=0 A=1 W=1 B=0 U=0 S=2 C=2 T=4 R=4 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-sched-continuation_20260921 | 0 | 11.4 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=4 C=12 T=16 R=30 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-sched-input-campaign_20260921 | 0 | 20.6 | `OK: cohort=30 records=1 planned=0 A=1 W=0 B=0 U=1 S=29 C=0 T=29 R=58 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-thrift-slowdown-continuation_20260921 | 0 | 1.9 | `OK: cohort=30 records=1 planned=0 A=1 W=1 B=0 U=0 S=2 C=0 T=2 R=4 conclusion=neither evidence_authentication=NOT_CHECKED` |
| tfa-kernel71-time-input-campaign_20260921 | 0 | 57.4 | `OK: cohort=30 records=4 planned=0 A=4 W=0 B=0 U=4 S=66 C=17 T=83 R=134 conclusion=neither evidence_authentication=NOT_CHECKED` |

All 14 exit 0 and every root prints `conclusion=neither`, because no root on its own has W >= 3. The 13 kernel-7.1 lines
match RESULTS.md section 4.1's per-root table row by row. Every root also prints `evidence_authentication=NOT_CHECKED`: the validator
checks internal consistency only and does not attest execution.

Unit tests: `python3 -m unittest discover -s experiments/tfa-flaky-tests_20260917/tools -p 'test_*.py'` gave rc=0,
136 s wall, and printed "Ran 59 tests in 135.690s / OK": 59 passed, 0 failed
(`/tmp/sub-tfa2-030/gather/validate/unittest.out`, `unittest.rc`). An earlier run already present in that directory,
from a prior instance of this gatherer, recorded the same 59/OK in 168.2 s. The campaign root's own
`experiments/tfa-kernel71-campaign_20260921/tools/test_admit_observed_target.py` is not under the discover path, and I did not run it.

## 5. Recount (script and output) and censor decomposition

The script is `/tmp/sub-tfa2-030/gather/campaign_recount.py` and its output is `/tmp/sub-tfa2-030/gather/campaign_recount.out`
(rc=0). It does not import the validator; it reimplements the counters from record fields. A second classifier,
`/tmp/sub-tfa2-030/gather/campaign_verify_classify.py` (output `campaign_verify_classify.out`), buckets every execution by
the verifier verdict printed in its tracked STDERR artifact.

Recount totals, which **exactly match RESULTS.md section 4.1** (lines 259-281) and the abstract (lines 14-19):

```
A=10 W=3 B=0 U=7
records=25 material_records=25
S=201 C=93 T=294 R(guest-started HERMIT_EXECUTION events)=512 E(all HERMIT_EXECUTION events)=540
event kinds over all records: {'HERMIT_EXECUTION': 540, 'LAUNCH': 1}
DISCOVERY=294, CONFIRMATION=246
```

The per-target union rows match RESULTS.md's table cell for cell (records/S/C/T/R). The WORKING seeds match RESULTS.md section 4.2
(HTTPCoro PASS 3 / FAIL 0; Thrift PASS 0 / FAIL 1; AsyncSSL PASS 1 / FAIL 11).

**Censor decomposition (mine; C=93).** Each bucket is derived from the fields and tracked stderr cited:

| Bucket | Tuples | Where |
|---|---:|---|
| Real target failure, but not a predeclared class in that cell | 42 | AsyncSSL `AsyncSSLSocketTest.cpp:3592` only: 24; HTTPCoro base-cell line-1685 CHECK: 16; TLS certificate-not-yet-valid abort: 2 |
| Hermit strict verify `:: Failure: nondeterministic.` (run 1 ≠ run 2 for the same seed) | 44 | TLS 29 (base 13, time-input 16), AsyncSSL 14, HTTPCoro 1 |
| Hermit `inject_fstat` → `openat` EFAULT in the dynamic loader | 3 | transport monitor (diagnosed in-record), LogStore, McRefill (my raw-log reading) |
| Hermit scheduler deadlock → container-child-exit | 1 | LogDevice health |
| Hermit `HERMIT_POLICY_REFUSAL cause=skid-overshoot` | 1 | Thrift time-input seed 16 |
| Outer 120 s safehermit deadline | 2 | Cachelib seed 0; HTTPCoro midpoint seed 2 |
| **Total** | **93** | |

The first divergent syscall in run 1 of the 44 nondeterministic verdicts (`campaign_verify_classify.out`) is:

| First divergent syscall | Verdicts |
|---|---:|
| `tgkill(3)` (abort-time teardown order) | 14 |
| `clock_gettime(CLOCK_MONOTONIC)` | 10 |
| `epoll_wait` | 13 |
| `epoll_ctl` | 2 |
| `read` | 1 |
| stderr-only mismatch | 4 |

All 10 `clock_gettime` cases are in cells using `--chaos-per-thread-slowdown`. For example, in
`experiments/tfa-kernel71-asyncssl-slice5-continuation_20260921/…/selection-003-sched-seed-00000000000000000003-replica-1.stderr.txt`,
run 1 returned tv_nsec 388175690 and run 2 returned 388175685, a 5 ns difference. The next `recvmsg` then returned
`EAGAIN` in one run and `Ok(0)` in the other. Inference: this is a candidate Hermit virtual-time determinism defect under
per-thread slowdown. By cell, the nondeterministic share was 13/15 (TLS base), 16/16 (TLS time-input), and between 0/16 and 4/16
for the AsyncSSL slowdown cells.

## 6. Timeline

All commands were run in `/home/newton/work/dev-hermit`.

- `git log --format='%h %ad %s' --date=iso -- ai_docs/tfa-candidate-sourcing-2026-09-17.md ai_docs/tfa-acceptance-bracket-2026-09-17.md`
  returns 2 commits. The cohort was published in `ee6562142 2026-09-17 20:08:01 -0700`, and the acceptance bracket in `9d7f603c4 2026-09-17 20:14:54 -0700`.
- `git log --format='%h %ad %s' --date=iso -- experiments/tfa-flaky-tests_20260917` returns **33 commits**, from
  `1356352a5 2026-09-17 23:00:06 -0700 Define the TFA experiment evidence layout` to
  `7bcee1143 2026-09-20 07:04:17 -0700 Record Thrift instrumentation readiness result`.
- `git log --format='%h %ad %s' --date=iso -- experiments/tfa-kernel71-*_20260921` (the shell expands this to the 13 roots)
  returns **303 commits**: 296 non-merge and 7 merges (`--merges`). They run from `128168e1a 2026-09-21 07:19:23 -0700 Instantiate the kernel 7.1 TFA campaign`
  to `344f54ff8 2026-09-22 02:49:18 -0700 Document kernel 7.1 TFA campaign`. 301 are dated 2026-09-21 and 2 are dated 2026-09-22.
  The list is saved at `/tmp/sub-tfa2-030/gather/campaign_gitlog_kernel71.txt`.
- The union of all the above paths is **338 commits** (`/tmp/sub-tfa2-030/gather/campaign_gitlog_all.txt`).
- Per-root commit counts (`git log --format=%h -- <root> | wc -l`) and date ranges (PDT):

| Root | Commits | First | Last |
|---|---:|---|---|
| campaign | 39 | 07:19 | 09-22 02:49 |
| time-input | 87 | 11:34 | 13:49 |
| sched-input | 30 | 14:00 | 14:18 |
| sched-continuation | 18 | 14:00 | 14:36 |
| httpcoro-midpoint | 6 | 14:05 | 14:42 |
| asyncssl-factor2 | 17 | 14:45 | 14:54 |
| thrift-slowdown | 3 | 14:46 | 14:55 |
| asyncssl-factor8 | 17 | 14:58 | 15:11 |
| asyncssl-factor5 | 17 | 14:58 | 15:03 |
| asyncssl-slice5 | 13 | 15:15 | 15:49 |
| asyncssl-slice10 | 17 | 15:16 | 15:27 |
| asyncssl-slice20 | 17 | 15:30 | 15:37 |
| asyncssl-slice50 | 17 | 15:31 | 15:43 |

- Key moments:
  - Native screen and gap audit committed at 09:09-09:35 PDT.
  - Oracle "integrity repairs" at 11:13 (HTTPCoro) and 11:58 (TLS). These moved HTTPCoro's 16 and TLS's 2 tuples from VALID to CENSORED, which explains the gap audit's
    `S=34 C=18` versus the final base root's `S=16 C=36`.
  - First WORKING classification: HTTPCoro at 21:42:30Z. Thrift at 21:55:52Z. AsyncSSL at 22:49:34Z (evidence revision `d74d1dab5 2026-09-21 15:49:45 -0700`).
  - RESULTS.md written at 779db3250 (09-22 02:14) and 344f54ff8 (02:49).

## 7. Contradictions and discrepancies

1. **"Pre-guest failures" vs tracked stderr.** RESULTS.md section 3.1 (lines 154-155) says the 540−512 = 28 gap is "pre-guest failures".
   The stderr shows the guest ran in all 28 cases: 26 nondeterministic verify verdicts, 1 timeout during `:: Run2...`, and 1 skid refusal. The field is defined as
   "a `[ RUN ]` marker is in retained stdout", and the verifier leaves stdout empty on mismatch (0-byte stdout in the slice5 seed 3 example).
   I believe the stderr; R is a lower bound on guest starts. Conversely, the base-root tool hard-codes `guest_started: True`
   (`experiments/tfa-kernel71-campaign_20260921/tools/admit_observed_target.py:597`). So R uses two definitions across roots.
2. **Loader failures.** The records, RESULTS.md section 4.3, and the gap audit say LogStore and McRefill "failed in the loader", which reads as a dependency problem.
   The retained raw logs show the same Hermit `inject_fstat` EFAULT that the monitor diagnosis proved. The gap audit even states for the monitor "not an absent DSO", but it did not
   extend that to the other two. I believe the raw logs. That this is Hermit-caused for those two is still inference, because the DSOs can no longer be checked.
3. **Gap audit vs final state.** The audit's validator line `records=9 … S=34 C=18 T=52 R=89` (TERMINAL-AND-CANDIDATE-GAP-AUDIT lines 7-8)
   and its table rows "HTTPCoro 16 / 16" valid and "Proxygen TLS terminator 2 / 15" valid predate the oracle repairs. The final base root is
   `S=16 C=36 T=52 R=89` with 10 records. The audit also puts InstrumentationTest in the fanout category. Both documents are
   dated snapshots and were not updated afterward. The final records are authoritative.
4. **TLS phrasing.** RESULTS.md section 4.3 says "only 19 of the 31 attempted tuples started a guest execution". In fact 29 of 31 are Hermit
   strict-verify nondeterministic verdicts. The 19 is an R count over events (17 base + 2 time-input), not a count of tuples.
5. **Stale metadata status.** Every root's `metadata.json` still says `"status": "SCHEMA_ONLY_NO_EXPERIMENTS_RUN"`. The 13 kernel-7.1 copies are identical
   (sha256 `4ae1c0d5…`) and differ from the schema root's (`5c4a3e9e…`). The field is stale; the records are MATERIAL.
6. **Schema-root README.** `experiments/tfa-flaky-tests_20260917/README.md` lines 4 and 10-13 still say "It contains no experiment result
   yet … 0/0". The root now holds 10 MATERIAL kernel-6.13 records, and its validator prints A=10 U=10 C/T=9/9 R=9.

## 8. GitHub issue search (sanctioned herdr-run route; raw JSON in `/tmp/sub-tfa2-030/gh-campaign-N.json`)

- `"inject_fstat"` (issues) returned one hit, which is unrelated: https://github.com/rrnewton/hermit/issues/1850 (pipe EOF hang). `"inject_fstat"`
  (PRs) returned https://github.com/rrnewton/hermit/pull/1851 (chown family), also unrelated. `"EFAULT openat fstat"` and
  `"scratch stack EFAULT"` returned 0. **No issue exists for the `inject_fstat` → `openat` EFAULT defect** that stops 3 targets.
- `"logically_kill"` found https://github.com/rrnewton/hermit/issues/3247, "ptrace: exec-time thread teardown logs logically_kill
  in host order, so strict verify diverges" (open). This is closely related to the TLS base-cell divergence, which happens at abort-time teardown rather than exec-time.
  Inference: it may be the same root cause.
- `"strict verify diverges"` also found https://github.com/rrnewton/hermit/issues/1923 (timerfd expiry host-timed; open). Inference: this is a
  possible link to the `epoll_wait` divergences, but unverified.
- `"TFA"` and `"chaos-per-thread-slowdown"` found https://github.com/rrnewton/hermit/issues/1845 (skid-gated retry and EPYC skid margin; open),
  which is relevant to the Thrift seed-16 skid refusal. `"clock_gettime nondeterministic verify"` and `"flaky test"` returned 0.

## 9. Open questions

- Are the LogStore and McRefill DSOs actually present at the symlink-tree paths? The ELFs were evicted, so this cannot be checked now. A rebuild at
  `7f239d5b` plus a native `ls -L` would settle whether `inject_fstat` is the sole cause.
- Why does `clock_gettime(CLOCK_MONOTONIC)` differ by nanoseconds between the two verify runs only under `--chaos-per-thread-slowdown`?
  This is a determinism defect candidate, and no issue was found.
- Is the TLS abort-teardown divergence (`logically_kill` order after `tgkill(...,6)`) the same defect as https://github.com/rrnewton/hermit/issues/3247?
- The Thrift native FAIL appeared only with `taskset -c 0` (16 TIDs vs 331 unrestricted). The formal cells do not pin CPUs. Does Hermit present a
  1-CPU view that reproduces the pinned topology? That is inference, not checked.
- Should R be redefined uniformly across roots, as "guest process started" or as "selected marker observed"? Otherwise per-root R values mean different things.
