# TFA kernel-7.1 continuations: every formal cell, the three successful paths, cost, failures, time-knob audit

Gatherer: `sub-tfa2-030-continuations` (read-only). Written 2026-09-28 on devbig030.
No Hermit, safehermit, Buck, or cargo command was run to produce this. Every number
below was read from committed records or recomputed from them with read-only Python
over the JSON; the recomputations reproduce the published aggregates exactly (Section 0).

## Conventions and source legend

All repo-relative paths are under `/home/newton/work/dev-hermit`.
`E/` = `experiments/`. Each record is `E/tfa-kernel71-<root>_20260921/services/<svc>/<dir>/experiment.json`.
JSON paths cited below are relative to that record.

| Rec | Root suffix (`tfa-kernel71-…_20260921`) | Record dir under `services/` |
|---|---|---|
| B1 | `campaign` | `cachelib/event-tracker-test--4b6700e9a3a0` |
| B2 | `campaign` | `folly/async-ssl-socket-test--cefc173a09b4` |
| B3 | `campaign` | `folly/timerfd-timeout-manager-test--246a62382461` |
| B4 | `campaign` | `logdevice/log-store-test--589f359badef` |
| B5 | `campaign` | `logdevice/tests--679930c8e36e` |
| B6 | `campaign` | `mcrouter/mc-refill-route-wrapper-test--82eb51b9426d` |
| B7 | `campaign` | `proxygen/httpcorosessiontests--e3f69b337304` |
| B8 | `campaign` | `proxygen/proxygen-transport-monitor-test--fcaf56433e7f` |
| B9 | `campaign` | `proxygen/tls-terminator-test--60719afd988e` |
| B10 | `campaign` | `thrift/instrumentationtest--3e07b8fc0ca0` |
| C1 | `time-input-campaign` | `proxygen/httpcorosessiontests--e3f69b337304` |
| C2 | `time-input-campaign` | `proxygen/tls-terminator-test--60719afd988e` |
| C3 | `time-input-campaign` | `thrift/instrumentationtest--3e07b8fc0ca0` |
| C4 | `time-input-campaign` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C5 | `sched-input-campaign` | `thrift/instrumentationtest--3e07b8fc0ca0` |
| C6 | `sched-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C7 | `httpcoro-midpoint-continuation` | `proxygen/httpcorosessiontests--e3f69b337304` |
| C8 | `asyncssl-factor2-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C9 | `thrift-slowdown-continuation` | `thrift/instrumentationtest--3e07b8fc0ca0` |
| C10 | `asyncssl-factor5-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C11 | `asyncssl-factor8-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C12 | `asyncssl-slice10-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C13 | `asyncssl-slice20-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C14 | `asyncssl-slice50-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |
| C15 | `asyncssl-slice5-continuation` | `folly/async-ssl-socket-test--cefc173a09b4` |

C-numbers are in the order the cells were frozen (`cells[0].frozen_at`), which is
also execution order, except that C15 (frozen 22:13:10Z) was executed last, after
C12–C14 (event times below).

The 12 continuation roots hold 15 records (the time-input root holds 4). With the 10
base records that is 25 records, each with exactly one formal cell (`cells` has length 1
in all 25).

## 0. Aggregate cross-check (recomputed; matches RESULTS.md)

Recomputed over all 25 records (`samples[].status`, `samples[].target_outcome`,
`events[].kind`, `events[].result.guest_started`):

- attempted seed tuples T = 294; valid S = 201 (198 PASS, 3 predeclared FAIL);
  censored C = 93. One tuple is one `--verify` invocation.
- 540 `HERMIT_EXECUTION` events, of which 512 have `result.guest_started=true` (R);
  plus 1 `LAUNCH` event (the rejected Thrift run, B10).
- These equal `E/tfa-kernel71-campaign_20260921/RESULTS.md` lines 259–264
  (`S = 201`, `C/T = 93/294`, `R = 512`) and line 150 ("all 540 `HERMIT_EXECUTION` event argvs").
- Per-target totals also match RESULTS.md lines 270–281: AsyncSSL S=122 C/T=38/160 R=295;
  HTTPCoro 10, 18/28, 52; Thrift 60, 1/61, 120; TLS 0, 31/31, 19.

Common to all 25 cells (read from `cells[0]`): ptrace backend, `strict=true`,
`chaos=true`, `relaxations=[]`, `--sched-heuristic random`, `timing.requested_max_timeslice =
effective_max_timeslice = 200000000` ns (200 ms of guest time), PMU available with RCB timer and no
fallback, host devbig030 / Linux 7.1.3-0_fbk0_0_g295ad0959f34 / AMD EPYC 9D85,
Hermit revision `4630ee2a61f93221a43e2baec0a15f34102a2c61`, binary SHA-256 `5c422b85…`,
`--verify --verify-strict --verify-allow both`, safehermit bounds of 120 s wall, 16 GiB memory,
0 swap, 64 MiB byte cap, 8G disk guard. `--seed 0 --rng-seed 0` in every cell.

## 1. Every formal cell

Columns: swept flag and the seed values actually attempted (from `samples[].seed.value`, with the
sealed population from `plan.measurement_design.seed_population.values`); time/schedule controls
parsed from `cells[0].hermit.argv`; tuple counts from `samples[]`; outcome from
`current_outcome.kind`; Hermit wall time per `--verify` invocation from the tracked safehermit report
(`events[].artifacts[kind=SAFEHERMIT_REPORT]` → `evidence/*.safehermit.txt`, field `elapsed_secs=`,
1-second resolution).

Abbreviations: CM = `--clock-multiplier` (absent means Hermit's default 1.0,
`clock_multiplier.unwrap_or(1.0)` in `hermit/detcore-model/src/config.rs` at 4630ee2a);
SD = `--chaos-per-thread-slowdown` with `--chaos-slowdown-max-factor` R; every SD cell also has
`--chaos-epoch-length-ns=0`; TS = `--target-timeslice` (ns); TR = `--chaos-target-races`;
epoch = `--epoch` (absent means the fixed default `2026-01-01T00:00:00Z`, `DEFAULT_EPOCH_STR` at
`hermit/detcore-model/src/config.rs:1263` in 4630ee2a). "div" = Hermit verify divergence
("Failure: nondeterministic" in the retained stderr); "3592" = AsyncSSL line-3592 failure that
verified deterministic but is not the predeclared class.

| Rec | Target | Cell id | Swept; seeds attempted (population) | CM | SD R | TS | TR | Epoch | PASS | FAIL | Cens. | Outcome | s/invocation median (min–max); sum |
|---|---|---|---|---|---|---|---|---|---:|---:|---:|---|---|
| B1 | cachelib event-tracker-test | `cachelib-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0 (0–93) | – | – | – | on | default | 0 | 0 | 1 (outer timeout) | NOT-DETERMINABLE, not exhausted | 120; 120 |
| B2 | AsyncSSL | `asyncssl-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0–6 (0–10) | – | – | – | on | default | 7 | 0 | 0 | NOT-DET., exhausted (7 valid required) | 4 (4–5); 29 |
| B3 | TimerFD | `timerfd-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0–8 (0–12) | – | – | – | on | default | 9 | 0 | 0 | NOT-DET., exhausted | 4 (3–4); 34 |
| B4 | LogStore retry | `logstore-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0 (0–34) | – | – | – | on | default | 0 | 0 | 1 (loader exit 127) | NOT-DET., deferred | 20; 20 |
| B5 | LogDevice health | `logdevice-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0 (0–11) | – | – | – | on | default | 0 | 0 | 1 (exit 125: scheduler deadlock + container failure) | NOT-DET., deferred | 38; 38 |
| B6 | McRefill | `mcrefill-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0 (0–16) | – | – | – | on | default | 0 | 0 | 1 (loader exit 127) | NOT-DET., deferred | 18; 18 |
| B7 | HTTPCoro | `httpcoro-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0–15 (0–99) | – | – | – | on | default | 0 | 0 | 16 (line-1685 CHECK, undeclared) | NOT-DET., owner stopped | 76 (75–95); 1234 |
| B8 | Proxygen transport monitor | `monitor-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0 (0–51) | – | – | – | on | default | 0 | 0 | 1 (exit 127, `inject_fstat` EFAULT) | NOT-DET., deferred | 20; 20 |
| B9 | TLS terminator | `tls-kernel71-ptrace-strict-chaos-target-races` | fuzz; 0–14 (0–14) | – | – | – | on | default | 0 | 0 | 15 (13 div, 2 cert-not-yet-valid) | NOT-DET., exhausted | 3 (3–4); 52 |
| B10 | Thrift Instrumentation | `thrift-instrumentation-kernel71-ptrace-strict-chaos-target-races` | fuzz; none admitted (0–35) | – | – | – | on | default | 0 | 0 | 0 (1 rejected LAUNCH) | NOT-DET., not exhausted | LAUNCH 16 s, not a tuple |
| C1 | HTTPCoro | `httpcoro-kernel71-ptrace-strict-sched-slowdown-v1` | sched; 7,5,4,2,15,0,3,1 (same 8) | 0.125 | 10 | – | off | 2026-09-21T18:14:41Z | 8 | 0 | 0 | NOT-DET., exhausted | 8 (7–15); 77 |
| C2 | TLS terminator | `tls-kernel71-ptrace-strict-sched-now-v1` | sched; 0–15 (0–15) | 1.0 | – | – | off | 2026-09-21T18:54:35Z | 0 | 0 | 16 (all div) | NOT-DET., exhausted | 33 (31–35); 525 |
| C3 | Thrift | `thrift-instrumentation-kernel71-ptrace-strict-chaos-target-races-now-v1` | fuzz; 0–29 (0–35) | – | – | – | on | 2026-09-21T19:01:04Z | 29 | 0 | 1 (skid-overshoot refusal, exit 122) | NOT-DET., exhausted | 15 (15–17); 467 |
| C4 | AsyncSSL | `asyncssl-kernel71-ptrace-strict-chaos-target-races-now-v1` | fuzz; 0–28 (0–35) | – | – | – | on | 2026-09-21T19:32:11Z | 29 | 0 | 0 | NOT-DET., exhausted | 4 (3–5); 111 |
| C5 | Thrift | `thrift-instrumentation-kernel71-ptrace-strict-chaos-sched-now-v1` | sched; 0–28 (0–35) | – | – | – | off | 2026-09-21T20:56:38Z | 29 | 0 | 0 | NOT-DET., exhausted | 16 (14–18); 458 |
| C6 | AsyncSSL | `asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-now-v1` | sched; 0–15 | 1.0 | 10 | 1,000,000 | off | 2026-09-21T20:57:56Z | 4 | 0 | 12 (11 "3592", 1 div) | NOT-DET., exhausted | 8.5 (3–56); 326 |
| C7 | HTTPCoro | `httpcoro-kernel71-ptrace-strict-midpoint-slowdown-v1` | sched; 0–3 (0–15) | 0.5 | 10 | – | off | 2026-09-21T21:01:52Z | 1 | 1 | 2 (1 div, 1 outer timeout) | **WORKING** | 60 (7–121); 248 |
| C8 | AsyncSSL | `asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-factor2-now-v1` | sched; 0–15 | 1.0 | 2 | – | off | 2026-09-21T21:44:37Z | 14 | 0 | 2 (div) | NOT-DET., exhausted | 4 (3–5); 65 |
| C9 | Thrift | `thrift-instrumentation-kernel71-ptrace-strict-chaos-sched-slowdown-now-v1` | sched; 0–1 (0–15) | 1.0 | 10 | 1,000,000 | off | 2026-09-21T21:44:37Z | 1 | 1 | 0 | **WORKING** | 13.5 (13–14); 27 |
| C10 | AsyncSSL | `…-sched-slowdown-factor5-now-v1` | sched; 0–15 | 1.0 | 5 | – | off | 2026-09-21T21:57:01Z | 14 | 0 | 2 (div) | NOT-DET., exhausted | 4 (3–8); 73 |
| C11 | AsyncSSL | `…-sched-slowdown-factor8-now-v1` | sched; 0–15 | 1.0 | 8 | – | off | 2026-09-21T21:57:05Z | 16 | 0 | 0 | NOT-DET., exhausted | 5 (3–12); 88 |
| C12 | AsyncSSL | `…-sched-slowdown-factor10-slice10m-now-v1` | sched; 0–15 | 1.0 | 10 | 10,000,000 | off | 2026-09-21T22:13:13Z | 10 | 0 | 6 (3 "3592", 3 div) | NOT-DET., exhausted | 4 (3–57); 162 |
| C13 | AsyncSSL | `…-sched-slowdown-factor10-slice20m-now-v1` | sched; 0–15 | 1.0 | 10 | 20,000,000 | off | 2026-09-21T22:29:06Z | 10 | 0 | 6 (2 "3592", 4 div) | NOT-DET., exhausted | 3 (3–52); 108 |
| C14 | AsyncSSL | `…-sched-slowdown-factor10-slice50m-now-v1` | sched; 0–15 | 1.0 | 10 | 50,000,000 | off | 2026-09-21T22:29:36Z | 12 | 0 | 4 (3 "3592", 1 div) | NOT-DET., exhausted | 4 (3–58); 123 |
| C15 | AsyncSSL | `asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-factor8-slice5000000-now-v1` | sched; 0–11 (0–15) | 1.0 | 8 | 5,000,000 | off | 2026-09-21T22:13:10Z | 5 | 1 | 6 (5 "3592", 1 div) | **WORKING** | 5 (3–43); 139 |

Notes on the table:

- "fuzz" = `--fuzz-seed` swept with `--sched-seed 0` fixed; "sched" = `--sched-seed` swept with
  `--fuzz-seed 0` fixed (`cells[0].hermit.fixed_seed_roots`, `plan.measurement_design.swept_coordinate`).
  All 10 base cells swept fuzz seeds with target-races on and no slowdown, no clock multiplier,
  and no explicit epoch. All 15 continuation cells pass an explicit epoch captured at their
  planning boundary.
- Column sums: PASS 198, FAIL 3, censored 93, total 294, as in Section 0.
- Censor-cause totals across all 93 censored tuples (classified from the tracked stderr of each
  censored tuple's first event): 44 verify divergences (AsyncSSL 14, TLS 29, HTTPCoro 1); 24 AsyncSSL
  line-3592 results that verified deterministic; 16 HTTPCoro line-1685 CHECKs (verified
  deterministic, not predeclared); 2 TLS certificate-not-yet-valid aborts; 3 loader exits with
  code 127 (B4, B6, B8); 1 exit 125 (B5); 2 outer timeouts (B1, C7 seed 2); 1 skid-overshoot policy
  refusal (C3 seed 16).
- The plan's "stop after both arms" rule (`plan.measurement_design.stop_rule = BOTH_ARMS_THEN_REPEAT`)
  is why C7, C9, and C15 did not use their whole 16-seed population.

## 2. How the three working targets were reached

The acceptance rule (RESULTS.md §2.1): within one frozen cell, one seed must give PASS on both
`--verify` replicas, a different seed must give the predeclared target-owned FAIL on both replicas,
and both must pass strict `BitwiseInfoV1` comparison with no relaxations. All three witnesses have
`matchers[].details = {verified: true, bitwise_parity: true, comparison: BitwiseInfoV1,
relaxations: []}` (`current_outcome.pass_witness` / `fail_witness` → `matchers[]`).

### What the knobs do, in plain words

Source: doc comments in `hermit/detcore-model/src/config.rs` at the campaign revision 4630ee2a
(`git show 4630ee2a…:detcore-model/src/config.rs`, lines ~196–211, 318–374, 505–525, 732–737).

- **Virtual time.** Hermit charges guest time per retired conditional branch (RCB), 10 virtual
  ns per RCB at the default rate, so the guest clock and the scheduler run on work done, not wall
  time.
- **`--clock-multiplier M`** scales that charge for all threads. Hermit's minimum-timeslice code
  computes ns per RCB as `NANOS_PER_RCB * clock_multiplier * slowdown`. M=0.5 means the guest's
  clock advances half as fast per unit of work (the program looks like it runs on a faster CPU);
  M=0.125 is eight times less charge.
- **`--chaos-per-thread-slowdown` with `--chaos-slowdown-max-factor R`** gives each guest thread
  a constant speed factor. The factor is drawn log-uniformly from [1/R, R] and depends only on the
  scheduler seed, the thread's stable id, and the epoch. A thread with factor 4 is charged 4× the
  virtual time per branch, so it looks 4× slower to the scheduler and to the guest's own clock.
  With **`--chaos-epoch-length-ns=0`** the factor is fixed for the whole run ("0 means one epoch
  for the entire run"). The scheduler seed therefore selects both the interleaving and each
  thread's relative speed.
- **`--target-timeslice T`**: at each syscall boundary, a thread that has used T virtual ns
  becomes eligible to be switched out. A smaller T gives more switch points. This is separate from
  **`--max-timeslice`** (200,000,000 ns = 200 virtual ms in every cell), the PMU-enforced cap on
  how long a thread may run without returning to Hermit.
- **`--chaos` + `--sched-heuristic random` + `--sched-seed`**: at every scheduling point, pick
  the next runnable thread with a seeded random choice.
- **`--chaos-target-races` + `--fuzz-seed`**: at the scheduler's existing nondeterminism points,
  use the fuzz seed to send process-directed signals to a randomly chosen thread and to randomize
  where a force-unblocked thread is requeued.
- **`--epoch`**: the guest's wall-clock start date (and initial file mtimes).

So "slowdown factor 8 + 5 ms timeslice" (C15) means: each AsyncSSL thread runs at a fixed speed
somewhere between 1/8× and 8× nominal, chosen by the scheduler seed, and the scheduler may switch
threads at any syscall once a thread has used 5 virtual ms.

### Thrift InstrumentationTest: 4 records, 3 cells with samples, 61 tuples, WORKING on the 2nd seed of the 3rd cell

1. **B10** (target-races, fuzz seeds, default epoch). The first invocation (seed 0, 2026-09-21T17:19:56Z)
   reached the test and printed a PASS with BitwiseInfoV1 matched. It was **rejected as a `LAUNCH`
   event** because the safehermit report said `bound.disk=NOT_APPLIED:bounded-run-space reported UNBOUNDED (rc=10)`
   (`events[0].result.diagnostic`; `B10/evidence/boundary-20260921T171956Z-seed-000.safehermit.txt`).
   It counts toward no tuple and no execution.
2. **C3** (target-races, explicit epoch 19:01:04Z, fuzz seeds 0–29): 29 PASS and 1 censored. Seed 16 exited 122,
   `HERMIT_POLICY_REFUSAL class=policy-refusal cause=skid-overshoot count=2`: Hermit refused the result
   because PMU timer delivery overshot its target (`C3/evidence/selection-016-fuzz-seed-00000000000000000016-replica-1.stderr.txt`).
   Exhausted: 29 valid tuples was the sealed requirement, set for a 10% minimum detectable FAIL rate
   with at most 5% miss probability.
3. **C5** (no target-races, no slowdown, sched seeds 0–28): 29 PASS, exhausted.
4. **C9** (per-thread slowdown R=10, epoch length 0, target timeslice 1,000,000 ns = 1 ms, CM 1.0,
   sched seeds): **seed 0 PASS twice, seed 1 FAIL twice**, then stopped.
   - FAIL signature (class `gtest-snapshot-size-2`), `C9/evidence/selection-001-sched-seed-00000000000000000001-replica-1.stdout.txt`:
     `fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp:836: Failure … reqSnapshots.size() Which is: 2 … 3`,
     `[  FAILED  ] …snapshotStartedProcessingTest (2110 ms)` (guest virtual ms), guest exit 1.
   - What changed from C5 to C9: the per-thread slowdown was turned on and a 1-ms target timeslice
     added, with the same zero seeds for everything but the scheduler seed.
   - Before this cell, 58 PASS tuples with no FAIL came from the two cells without slowdown (C3, C5). The
     FAIL then came on the second scheduler seed. **Inference:** slowdown plus a 1-ms timeslice is the
     change that exposed the ordering. With n=2 in C9, the records show that the change was sufficient
     here. They do not establish a rate.
   - Native planning context (not formal evidence): on CPU 0, three PASS runs at 0.55–0.59 s wall and
     one exact line-836 FAIL at 1.57 s. Unrestricted: 909 ms test, 1.51 s wall, 331 TIDs
     (`E/tfa-kernel71-campaign_20260921/services/thrift/instrumentationtest--3e07b8fc0ca0/THRIFT-INSTRUMENTATION-NATIVE-PROFILE-20260921.md`;
     `E/tfa-kernel71-campaign_20260921/NATIVE-CANDIDATE-SCREEN-20260921.md`). The cohort's trailing-week native
     failure rate for this target was 0/29 (`candidate_ranking.native_failure_rate`) while PFS was 17.22%.

### Folly AsyncSSL (`async_ssl_socket_test`, `AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect`): 10 cells, 160 tuples, WORKING on seed 11 of the 10th cell

Execution order (first and last `events[].started_at` / `finished_at`):

| Step | Rec | Ran (UTC 2026-09-21) | Knobs | P / F / C | What happened |
|---:|---|---|---|---|---|
| 1 | B2 | 14:27:14–14:27:53 | target-races, fuzz 0–6, default epoch, oracle v1 | 7/0/0 | PASS only; reached the 7 valid tuples required |
| 2 | C4 | 19:59:35–20:23:05 | target-races, fuzz 0–28, explicit epoch, oracle v2 | 29/0/0 | PASS only; exhausted |
| 3 | C6 | 21:22:21–21:36:16 | SD R=10, TS 1 ms | 4/0/12 | 11 deterministic line-3592 results, 1 div |
| 4 | C8 | 21:47:00–21:54:00 | SD R=2, no TS | 14/0/2 | 2 div |
| 5 | C10 | 21:58:55–22:03:13 | SD R=5, no TS | 14/0/2 | 2 div |
| 6 | C11 | 22:06:22–22:11:33 | SD R=8, no TS | 16/0/0 | PASS only |
| 7 | C12 | 22:21:28–22:27:38 | SD R=10, TS 10 ms | 10/0/6 | 3 "3592", 3 div |
| 8 | C13 | 22:32:10–22:37:17 | SD R=10, TS 20 ms | 10/0/6 | 2 "3592", 4 div |
| 9 | C14 | 22:38:07–22:43:38 | SD R=10, TS 50 ms | 12/0/4 | 3 "3592", 1 div |
| 10 | C15 | 22:44:41–22:49:33 | SD R=8, TS 5 ms | 5/1/6 | **seed 1 PASS ×2, seed 11 FAIL ×2** |

- Seeds: 36 fuzz-seed tuples (B2, C4) and 124 sched-seed tuples (16 × 7 cells + 12 in C15) = 160.
  Guest-started executions: 295 (per `result.guest_started`).
- **PASS witness: sched seed 1. FAIL witness: sched seed 11** (`C15 current_outcome.pass_witness` /
  `fail_witness`). Additional valid PASS seeds in C15: 4, 5, 6, 8.
- **FAIL signature** (class `gtest-byte-events-three-missing-ack`), `C15/evidence/selection-011-sched-seed-00000000000000000011-replica-1.stdout.txt`:
  `AsyncSSLSocketTest.cpp:3797: Failure  Value of: observer->byteEvents  Expected: has a size that is >= 4 … whose size 3 doesn't match`,
  then `AsyncSSLSocketTest.cpp:3812: Failure … offsetExpected Which is: 501 … maxOffsetForByteEventReceived(ByteEventType::ACK) Which is: 16-byte object <00-…-00>`,
  `[  FAILED  ] AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect (140 ms)`, exit 1.
  Both witness invocations took 3 s (`elapsed_secs=3`).
- **Line-3592 (censored, not the target class):** `AsyncSSLSocketTest.cpp:3592: Failure … connCb_.state Which is: 4-byte object <02-00 00-00>`
  versus `SUCCESS <01-00 00-00>`. stderr shows `writeError … socket in invalid state` and
  `handshakeError … errno = 104 (Connection reset by peer)`, then a SIGSEGV in
  `folly::test::WriteCallbackBase::writeErr` (`C6/evidence/selection-000-sched-seed-00000000000000000000-replica-1.{stdout,stderr}.txt`).
  In plain words, the client connection failed with an error before the byte-event checks. That is a
  different symptom from the ACK race.
- **Pattern (inference from the table):** the line-3592 result appeared only in cells with a target
  timeslice (1, 5, 10, 20, 50 ms): 11/16 at 1 ms, 5/12 at 5 ms, 3/16 at 10 ms, 2/16 at 20 ms,
  3/16 at 50 ms. It appeared in none of the 3 no-timeslice slowdown cells (48 tuples) and none of the
  36 target-races tuples. The predeclared 3797/3812 FAIL appeared exactly once in 160 tuples, at
  R=8 with a 5-ms timeslice. RESULTS.md §4.3 labels the mechanism as a hypothesis: slowdown and
  timeslice change the relative ordering of byte events and ACKs.
- Native planning context: an ordinary native FAIL at 17 ms selected-test time (1.29 s outer wall)
  and a traced PASS at 20 ms (0.21 s wall); 2 TIDs in the selected phase. Cohort native failure rate
  37/96 (`candidate_ranking.native_failure_rate`; `C6/ASYNCSSL-SCHED-SLOWDOWN-PLAN-20260921.md`).

### Proxygen HTTPCoro (`HTTPCoroSessionTests`, `H2DownstreamSessionTest.ResetRateLimit/h2`): 3 cells, 28 tuples, WORKING on the 4th seed of the 3rd cell

1. **B7** (target-races, fuzz 0–15, default clock and epoch): all 16 tuples (32 executions) hit a
   deterministic `HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()`,
   SIGABRT/134, with BitwiseInfoV1 matched. The sealed v1 oracle required a gtest `[  FAILED  ]` line
   and summary. stdout ends after `[ RUN ]`, so all 16 were **re-labelled from FAIL to CENSORED** by
   `B7/HTTPCORO-ORACLE-INTEGRITY-REPAIR-20260921.md`. The analysis in `B7/HTTPCORO-SLOWDOWN-ANALYSIS-20260921.md`
   found a single-thread time budget, not a race: 1,001 resets must land inside a 1,000-per-100-ms
   rate-limiter window, and Hermit's overhead and virtual time let the 100-ms reset timer fire
   first. The owner stopped further fuzz seeds (`current_outcome.pending_in_scope_measurements`).
2. **C1** (CM 0.125, SD R=10, epoch length 0, no TS, 8 prospectively chosen sched seeds
   7,5,4,2,15,0,3,1 selected from their precomputed slowdown factors): 8/8 PASS, exhausted.
3. **C7** (CM 0.5 "midpoint", SD R=10, epoch length 0, no TS, sched seeds 0–15 ascending):
   - seed 0: **FAIL ×2** (class `reset-rate-limit-line1685-body-event-fatal`; stderr
     `F0921 21:01:54.726754 3 HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()`
     plus stack frame `H2DownstreamSessionTest_ResetRateLimit_Test::co_TestBody()`; `unit_result=core-dump`, exit 134; 110 s);
   - seed 1: censored, verify divergence (10 s);
   - seed 2: censored, outer 120-s safehermit deadline (`elapsed_secs=121`, exit 124; the stderr ends
     at `:: Run2...`, so run 1 had finished);
   - seed 3: **PASS ×2** (7 s). Stopped.
   - **Witnesses: PASS seed 3, FAIL seed 0** (`C7 current_outcome`).
- **Why it works (inference; uses the factor table in `C1/HTTPCORO-TIME-INPUT-PLAN-20260921.md`).**
  That table lists the Hermit-computed slowdown factor of deterministic thread 3 for each scheduler
  seed at R=10. C7 uses the same R and epoch length, and the factor depends only on (seed, thread id,
  epoch). The effective virtual-time rate is CM × factor:
  - seed 0: 2.917 × 0.5 = 1.46 → FAIL;
  - seed 1: 1.145 × 0.5 = 0.57 → divergence;
  - seed 2: 4.267 × 0.5 = 2.13 → timeout;
  - seed 3: 0.454 × 0.5 = 0.23 → PASS.
  In C1, every effective rate was ≤ 0.920 (seed 5), and all 8 passed. In B7 the rate was 1.0 (no
  slowdown) and all 16 aborted. The records therefore fit a threshold between 0.92 and 1.0 in
  effective virtual ns per unit of work. This is an observed pattern, not a proven threshold.
- The selected phase is single-threaded. RESULTS.md §4.3 calls this "a controlled time/order result,
  not evidence of a competing-thread race."
- Native: 5 ordinary passes at 303–330 ms test time, 0.48–0.69 s outer wall. Cohort native failure rate
  1/30 (`B7/HTTPCORO-SLOWDOWN-ANALYSIS-20260921.md`; `candidate_ranking.native_failure_rate`).

## 3. Execution cost

**Source and unit.** Each tuple is one outer `bin/safehermit … hermit run --verify` invocation.
Hermit runs the guest twice, one run after the other: `:: Run1...` completes before `:: Run2...`
starts (`hermit/hermit-cli/src/bin/hermit/run.rs:4242,4404` at 4630ee2a). It then compares the logs.
The only wall-clock measurement per invocation is safehermit's `elapsed_secs` (integer seconds). The
event `started_at`/`finished_at` agree to the second and are identical for both replicas of one
invocation. No per-replica wall time is recorded. **Per guest run ≈ half an invocation** is an
inference from the sequential code and includes the comparison time.

| Scope | Invocations | Median s | Min s | Max s | Sum s |
|---|---:|---:|---:|---:|---:|
| AsyncSSL, all 10 cells | 160 | 4 | 3 | 58 | 1,224 |
| AsyncSSL PASS tuples | 121 | 4 | 3 | 12 | 495 |
| AsyncSSL censored tuples | 38 | 7.5 | 3 | 58 | 726 |
| AsyncSSL winning cell C15 | 12 | 5 | 3 | 43 | 139 |
| Thrift, 3 sampled cells | 61 | 15 | 13 | 18 | 952 (+16 s rejected LAUNCH) |
| Thrift winning cell C9 | 2 | 13.5 | 13 | 14 | 27 |
| HTTPCoro, 3 cells | 28 | 75 | 7 | 121 | 1,559 |
| HTTPCoro PASS tuples | 9 | 7 | 7 | 15 | 84 |
| HTTPCoro B7 line-1685 aborts | 16 | 76 | 75 | 95 | 1,234 |
| HTTPCoro winning cell C7 | 4 | 60 | 7 | 121 | 248 |
| TLS, 2 cells | 31 | 31 | 3 | 35 | 577 |
| All 25 cells | 294 | 5 | 3 | 121 | 4,562 (≈ 76 min) |

- Timeouts: 2, both on the outer 120-s safehermit deadline: B1 seed 0 (120 s) and C7 seed 2 (121 s).
  No target-owned timeout was declared or observed (`plan.target_oracle.timeout_policy`).
- Near-miss: the C7 FAIL witness took 110 s of the 120-s bound.
- Executions: 294 invocations; 540 execution events; 512 marked guest-started. The physical number
  of guest runs is at most 2 × 294 = 588 (inference). Every invocation runs Run1 and Run2 unless it
  dies first. Invocations that failed verification record only a replica-1 event.
- Calendar span: the first formal execution was 2026-09-21T14:27:14Z (B2) and the last finished
  22:49:33Z (C15), about 8 h 22 min. 76 min of that was Hermit invocation time; the rest was planning
  and sealing between cells.
- Time from a winning cell's first execution to its WORKING classification: C9 1.0 min (27 s of
  Hermit time); C15 4.9 min (139 s); C7 5.4 min (248 s).

**Hermit versus native (inference; wall time only, rounded; guest-reported gtest times are
virtual time and are not comparable).**

| Target | Native wall (source) | Hermit per invocation (PASS) | ≈ per guest run | ≈ slowdown vs native |
|---|---|---|---|---|
| AsyncSSL | 0.21 s traced PASS; 1.29 s ordinary FAIL (`C6/ASYNCSSL-SCHED-SLOWDOWN-PLAN-20260921.md`) | median 4 s (3–12) | ~2 s | ~10× vs the 0.21-s PASS |
| Thrift | 0.55–0.59 s CPU-0 PASS; 1.51 s unrestricted (`THRIFT-INSTRUMENTATION-NATIVE-PROFILE-20260921.md`, `NATIVE-CANDIDATE-SCREEN-20260921.md`) | median 15 s (13–18) | ~7.5 s | ~13× vs CPU-0 (the topology Hermit presents, since it virtualizes `sched_getaffinity` to CPU 0 per the profile doc); ~5× vs unrestricted |
| HTTPCoro | 0.48–0.69 s outer wall (`HTTPCORO-SLOWDOWN-ANALYSIS-20260921.md`) | median 7 s (7–15) | ~3.5 s | ~5–7× |
| HTTPCoro abort path | same | 75–110 s | ~38–55 s | ~55–110× |
| TLS | 0.24 s (`NATIVE-CANDIDATE-SCREEN-20260921.md`) | 3 s (B9) / 33 s (C2); no valid tuples | – | – |

Hypothesis, not established: the HTTPCoro abort path is slow because the folly signal handler
symbolizes a stack from the 48,908,168-byte ASan ELF under ptrace. The B7 stderr shows the CHECK at
guest time `F0101 00:00:02.614` followed by symbolized stacks (`B7/evidence/seed-000-replica-1.stderr.txt`).
The AsyncSSL censored tuples run long for a similar possible reason (the line-3592 SIGSEGV stack
trace): median 7.5 s versus 4 s for PASS.

## 4. Tried and did not work (counts)

1. **AsyncSSL line-3592 kept censored:** 24 tuples (C6 11, C15 5, C12 3, C14 3, C13 2). Each was
   BitwiseInfoV1-matched on both replicas, so the outcome was repeatable. It is not the predeclared
   3797/3812 class, and RESULTS.md §5 confirms it was not relabelled.
2. **AsyncSSL PASS-only sweeps:** 36 target-races tuples (B2 7, C4 29) and 16 in C11 (R=8, no
   timeslice) had no FAIL at all. C8 and C10 had 28 PASS and 4 divergences, and no FAIL.
3. **Hermit verify divergence ("Failure: nondeterministic"): 44 tuples.** AsyncSSL 14 (C6 1, C8 2,
   C10 2, C12 3, C13 4, C14 1, C15 1), HTTPCoro 1 (C7 seed 1), TLS 29 (B9 13, C2 16). In 3 AsyncSSL
   cases the replica-1 guest itself printed `[  PASSED  ] 1 test.` (C10 seed 13, C12 seeds 3 and 4),
   but run 2 diverged. C6 seed 1 shows the divergence at about syscall #3166 in a
   `clock_gettime`/`recvmsg`/`epoll_wait` ordering (`C6/evidence/selection-001-…-replica-1.stderr.txt`).
   TLS C2: the guest's stderr timestamps differ between runs around calls to SMC tiers, configerator,
   and Falcon (`C2/evidence/selection-000-…-replica-1.stderr.txt`, "Mismatch in stderr between run 1
   and run 2"). **Inference:** TLS reaches host services that Hermit does not isolate. I found no
   GitHub issue for these divergences; searches "TFA", "slowdown", and "epoch" in rrnewton/hermit
   (outputs `/tmp/sub-tfa2-030/gh-continuations-{1,2,4}.json`) returned nothing specific.
4. **HTTPCoro fuzz-seed CHECK not predeclared:** 16 tuples / 32 executions in B7. They were first
   recorded as FAIL, then re-labelled CENSORED by the oracle-integrity repair. HTTPCoro at clock 0.125
   (C1): 8/8 PASS, no FAIL arm. C7 also lost 1 tuple to divergence and 1 to the outer timeout.
5. **Thrift run rejected for missing disk guard:** 1 `LAUNCH` event in B10 (`bound.disk=NOT_APPLIED`). It
   was a PASS observation made permanently ineligible (`current_outcome.missing_or_ambiguous_facts`).
   Thrift PASS-only sweeps: 58 valid tuples in C3 and C5. One C3 tuple (seed 16) was refused by Hermit's
   skid-overshoot policy (exit 122). The related open issue is
   https://github.com/rrnewton/hermit/issues/1845 ("IMPLEMENT the skid-gated retry …"), found by the
   "TFA" search.
6. **TLS terminator:** 31/31 tuples censored across 2 cells, 0 valid. In B9, seeds 11 and 12 aborted
   on `certificate is not yet valid` under the 2026-01-01 default epoch. They were first recorded as
   FAIL, then re-labelled by `B9/TLS-ORACLE-INTEGRITY-REPAIR-20260921.md`. The other 13 diverged. C2 used
   a realistic epoch, and all 16 diverged.
7. **Base-root single-seed stops (5 targets, 1 tuple each):** B1 outer timeout at 120 s; B4 and B6
   loader exit 127; B8 exit 127 (Hermit's post-open `inject_fstat` turns the enclosing `openat` into
   `EFAULT`, per `NATIVE-CANDIDATE-SCREEN-20260921.md`); B5 exit 125 (two-thread scheduler deadlock then
   container failure; 657 native TIDs). B3 TimerFD passed 9/9 with no FAIL.
8. **Counter history:** the 16:26:37Z audit (`E/tfa-kernel71-campaign_20260921/TERMINAL-AND-CANDIDATE-GAP-AUDIT-20260921.md`)
   reported base-root S=34, C=18 for 9 records. The final base root is S=16, C=36. The difference is
   exactly the 18 tuples moved from FAIL to CENSORED by the two oracle repairs (HTTPCoro 16, TLS 2).

## 5. Epoch and time-knob audit in plain words

Source: `E/tfa-kernel71-time-input-campaign_20260921/HERMIT-TIME-KNOB-AUDIT-20260921.md`. I re-checked
its line references against `git show 4630ee2a…` in `hermit/`.

- Hermit already has the time controls these cells need:
  - `--epoch`, the guest start date;
  - `--clock-multiplier`, the guest clock rate;
  - RCB-time on/off, and opt-outs for virtual time and virtual file metadata;
  - timeslices, scheduler seed, heuristic, and preemption record/replay;
  - the chaos slowdown vector (per-thread slowdown, max factor, epoch length).
  Fuzz seed, futex fuzzing, and target-races control other nondeterminism. There is no separate
  file-mtime seed: virtual file times start at the epoch and advance with the virtual clock.
  `--timeout` is a wall-time kill, not guest time. The cells left it unset and relied on
  safehermit's 120-s deadline.
- **The gap.** When `--epoch` is omitted, Hermit uses the fixed date 2026-01-01T00:00:00Z
  (`DEFAULT_EPOCH_STR`, `config.rs:1263`; default value wired at `config.rs:209`). The rendered
  command line omits `--epoch` whenever it equals that default (`config.rs:888–891`). `--save-config`
  writes Rust debug text, not a reloadable manifest (`run.rs:4832–4837`). Runs do not persist the
  resolved epoch. So a default run sees an unrealistic date, and its record cannot prove which date it
  used. The concrete harm in this campaign was the TLS certificate-not-yet-valid aborts (B9 seeds 11
  and 12).
- **Workaround used.** Every continuation cell sampled host UTC once at planning time and passed it
  literally as `--epoch=…` to both verify replicas (epoch column in Section 1).
- **Proposed fix.** At the CLI boundary, resolve an omitted epoch once from host UTC, before any
  backend or replica starts. Treat it as effective configuration: print it and persist it in a
  versioned run/record manifest. Reuse it for verify, record, and replay. An explicit `--epoch` or
  `HERMIT_EPOCH` still wins. Keep `detcore::Config::default()` deterministic for library use, and do
  not freeze or quantize continuous virtual time.
- **Status after the campaign.** This has since landed on hermit main as commit
  `402ba9737798d1b3d882b3dfaa5bca8fe21c9cb3`, "Capture one reproducible virtual epoch per execution",
  dated 2026-09-22 15:44 -0700. It is an ancestor of `origin/main` `fdd8f55a…`
  (`git merge-base --is-ancestor`). Its body says it integrates the reviewed PR3151 implementation;
  https://github.com/rrnewton/hermit/pull/3151 itself is CLOSED unmerged, closed 2026-09-22T22:58:43Z
  (`/tmp/sub-tfa2-030/gh-continuations-5.json`). The campaign binary predates this commit, so the
  campaign results do not depend on it.

## 6. Provenance caveat on the Hermit binary

The campaign binary revision `4630ee2a61f93221a43e2baec0a15f34102a2c61` ("Accept virtual RLIMIT_CORE
updates", 2026-09-20) is **not an ancestor of hermit `origin/main`**. It exists on
`origin/salvage/devbig014/hermit-tfa-prlimit-fix/hermit-1-4630ee2a61f9`. It is one commit on top of
main's `4dcb34b4` ("Restore the standalone OSS Buck2 build"). Its `git patch-id --stable`
(`c9843d1e…`) equals that of landed main commit `ccbe37e91`, "Accept virtual RLIMIT_CORE updates
(https://github.com/rrnewton/hermit/pull/3142)". The patch landed, but the exact tree that produced
the TFA evidence was never a main commit. No replay on current main is recorded.

## 7. Contradictions and record-semantics caveats found

1. **B2 classified before it ran.** B2 `current_outcome.classified_at` is 2026-09-21T14:26:00Z
   (`asyncssl-kernel71-pass-only-exhausted`). All 14 of its events started 14:27:14Z–14:27:49Z, and
   the safehermit run_ids agree (`20260921T142714Z-885809` … `20260921T142749Z-928324`). The
   classification timestamp is earlier than the evidence it classifies. I believe the event times,
   because two independent sources agree, and treat the classification timestamp as mis-entered.
2. **R undercounts guest runs.** 28 of the 540 execution events have `result.guest_started=false`.
   These include verify-divergence invocations whose stderr shows Hermit ran both guest runs and
   compared them. Example: C2, where 14 of 16 events are marked false although each stderr has
   "Mismatch in stderr between run 1 and run 2". R=512 counts what the harness marked. It is not the
   number of physical guest executions (at most 588; see Section 3).
3. **Wrong unit in three plan documents.** They say "200,000,000-RCB maximum timeslice":
   `C5/THRIFT-SCHED-PLAN-20260921.md:6`, `C3/THRIFT-TARGET-RACES-PLAN-20260921.md:16`,
   `C4/ASYNCSSL-TARGET-RACES-PLAN-20260921.md:20`. Hermit documents `--max-timeslice` in virtual
   nanoseconds, enforced by RCB counting at 10 ns per RCB by default (`config.rs` doc on
   `max_timeslice` at 4630ee2a). The records themselves say ns (`cells[0].timing`). The prose unit is
   wrong; the configured value is 200,000,000 ns = 200 virtual ms.
4. **Epoch-length wording (not a conflict).** AsyncSSL READMEs say "zero-length epochs"; the C1 plan
   says "a single slowdown epoch". Per the `chaos_epoch_length_ns` doc, 0 means one epoch for the
   whole run, so both describe the same setting.
5. **Changed counters (explained).** The 16:26Z terminal audit's base-root S=34, C=18 differs from
   the final S=16, C=36. The difference is the 18 tuples re-labelled by the oracle repairs
   (Section 4, item 8).
