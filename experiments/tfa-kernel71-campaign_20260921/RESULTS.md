# Reproducing flaky-test outcomes with Hermit: a kernel 7.1 TFA campaign

## Abstract

Flaky concurrent tests are difficult to diagnose because a native rerun says
that a failure happened, but usually does not preserve the schedule that made
it happen. This campaign asked whether Hermit could select schedules that make
both the passing and the original failing outcome of real Buck test targets
repeatable. The prospectively fixed acceptance rule required each successful
target to exhibit, in one frozen strict-ptrace cell, a repeated PASS at one
seed and the same repeated, predeclared target-owned FAIL at another seed.

At evidence revision `d74d1dab5dc865d8854df9649dbd9b36b1f9496a`
(2026-09-21 15:49:45 -07:00), 3/10 distinct attempted Buck targets were
WORKING, 0/10 were BLOCKED-WITH-NAMED-CAUSE, and 7/10 were
NOT-DETERMINABLE. The 25 material records contained 201 distinct valid
discovery tuples, 93/294 censored/attempted discovery tuples, and 512
guest-started Hermit executions; none of those run-level quantities is the
target denominator. The three successful targets were Proxygen
`HTTPCoroSessionTests`, Thrift `InstrumentationTest`, and Folly
`async_ssl_socket_test`. Thus the campaign conclusion is **SUCCESS** under
the fixed rule `W >= 3` distinct canonical targets. This is an existence and
reproducibility result for the recorded targets and cells, not an estimate of
flakiness prevalence or a claim that Hermit found the underlying software
defects.

## 1. Question and motivation

The Test Flakiness Analyzer (TFA) question was:

> Can Hermit's deterministic scheduler reproduce both outcomes of distinct,
> real flaky-test targets while holding the test, binary, environment, and all
> non-swept randomness fixed?

Hermit runs a Linux program under a controlled scheduler. In this experiment
its ptrace backend intercepted the guest and `--chaos` used seeded scheduling
choices. A useful result requires more than making a test fail: a harness that
always fails would do that. TFA therefore sought a two-directional bracket.
One seed had to make the selected test pass twice, and a different seed had to
make the same test reach the same predeclared, target-owned failure twice.
Hermit's strict canonical schedule comparison then had to match within each
seed. This design tests schedule localization: a seed is a compact handle for
a repeatable outcome under a fully identified execution cell.

“TFA” here names this experimental method and its records; it is not a claim
that the campaign automatically diagnosed or repaired the races.

## 2. Fixed protocol and target selection

### 2.1 Acceptance contract

The classification authority was fixed before the campaign at dev-hermit
commit `9d7f603c43618715b9245c74acb3e164c17d2ae8`, path
`ai_docs/tfa-acceptance-bracket-2026-09-17.md`, blob
`4a53f0c25623ad6371ee86b4556adada1218745d`. The campaign did not amend it.
Its units and terminal rules are:

- `A`: distinct materially attempted canonical Buck targets.
- `W`: targets with a complete repeated PASS/repeated predeclared-FAIL bracket
  in one frozen cell.
- `B`: targets for which every authorized route is stopped by a causally
  demonstrated named boundary with the required owner disposition.
- `U`: every other material target, reported as NOT-DETERMINABLE.
- `A = W + B + U`; seeds, cells, TestInfra IDs, retries, and executions never
  add targets.
- SUCCESS requires `W >= 3`. The much stronger negative stopping rule requires
  at least 30 attempted targets, `W=0`, terminal records, adjudicated blockers,
  and exhausted NOT-DETERMINABLE budgets.

For one target to be WORKING, PASS and FAIL had to use distinct values of the
one predeclared schedule-affecting seed, but otherwise the exact same frozen
cell. Each arm required a discovery execution and a confirmation execution.
The selected test had to execute exactly once. PASS meant its unmodified oracle
succeeded. FAIL meant one predeclared target-owned class matched. Hermit,
wrapper, loader, dependency, sanitizer-only, OOM, output-truncation, and
outer-timeout failures were censored rather than relabelled as target failures.
Each arm also required a complete built-in verifier result with
`verified=true`, `bitwise_parity=true`, comparison `BitwiseInfoV1`, and an
empty relaxation list.

### 2.2 Candidate population and admission

The population was the byte-identical 30-target registry published by commit
`ee6562142e3796bb56a1dd0e28b77c1c16a78cc2`. Its source document is
`ai_docs/tfa-candidate-sourcing-2026-09-17.md`, blob
`ff01b534352e15a5685b8a41032973d46c089274`; the canonical target-array digest
is `49e1952b2bd857229646842f6a6da845e5b7ecea14e14cd9c077190a699750c0`.
The registry contains three targets from each of ten service families.
Candidates were deduplicated by canonical Buck label, required a current
Probabilistic Flakiness Score (PFS) of at least 5%, and were ranked within a
service by PFS before curation toward Linux-native tests. PFS was used only to
rank candidates. It was never treated as an outcome probability.

The registry also froze each TestInfra ID, selected case, build mode,
standalone Buck command, PFS, and trailing-week native failure numerator and
denominator. Native failure rates were planning priors only: the records
explicitly state that a native rate is not a Hermit seed-outcome rate.

Ten of the 30 targets became material attempts. Diagnostic native screening
accounted for all 30 and favored selected cases that completed within ten
seconds, demonstrated competing-thread ordering, and had a thread footprint
for which ptrace overhead was not expected to dominate. Diagnostics did not
increment `A` and were not copied into formal samples. Later continuation
cells were informed by earlier observations, but every new plan, oracle,
configuration, finite seed population, helper identity, and stop rule was
committed before that cell's results. The 13 schema roots retain all 25
material records; repeated roots for one canonical target do not increase
`A` or `W`.

## 3. Experimental method

### 3.1 Build boundary: Buck outside Hermit

The target source revision was
`7f239d5b37dfbecf32fed144468da461f998add5` for every cell. The test ELFs were
existing Buck outputs under `/home/newton/work/tfa/fbsource`. Each sealed route
records the external `build_argv` used to resolve or produce that output, for
example:

```text
buck test @fbcode//mode/dev-asan fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests -- --exact fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2
buck test @fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:InstrumentationTest -- --exact fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest
buck test @fbcode//mode/libcxx fbcode//folly/io/async/test:async_ssl_socket_test -- --exact fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --run-disabled
```

The formal records contain no `BUILD` event, so they do not attest a
campaign-time Buck build execution. They bind the already produced Buck ELFs
by source revision, absolute path, byte size, and SHA-256. This is build
provenance, not a claim that the build itself was reproduced under Hermit. The
three successful ELF identities were:

| Target | Mode | Bytes | Test ELF SHA-256 |
| --- | --- | ---: | --- |
| `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` | `@fbcode//mode/dev-asan` | 48,908,168 | `00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc` |
| `fbcode//thrift/lib/cpp2/test:InstrumentationTest` | `@fbcode//mode/dev-tsan` | 11,514,064 | `2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc` |
| `fbcode//folly/io/async/test:async_ssl_socket_test` | `@fbcode//mode/libcxx` | 17,651,096 | `47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46` |

For measurement, `bin/safehermit` invoked the absolute ELF directly after
Hermit's `--` boundary with an exact GoogleTest filter. The common shape was:

```text
bin/safehermit --sh-report EVIDENCE --sh-deadline 120 \
  --sh-max-log-bytes 67108864 --sh-disk-limit 8G HERMIT_BINARY \
  run --backend ptrace --strict --chaos ... \
  --seed 0 --rng-seed 0 --sched-seed SWEEP --fuzz-seed 0 \
  --sched-heuristic random --verify --verify-strict \
  --verify-allow both --keep-logs ... -- ABSOLUTE_BUCK_ELF GTEST_FILTER
```

This separation was checked directly, not inferred from prose: all 540
`HERMIT_EXECUTION` event argvs begin with `bin/safehermit`, all 540 name an
absolute Buck output binary immediately after `--`, and none contains a `buck`
token. The 452 separately retained `*.invocation.json` files have the same
property. The difference between 540 execution events and 512 guest-started
executions is expected: pre-guest failures remain events but do not enter `R`.

### 3.2 Exact execution environment

All 25 cells recorded the same base environment:

| Property | Recorded value |
| --- | --- |
| Host | `devbig030.atn3.facebook.com` |
| Kernel | `Linux 7.1.3-0_fbk0_0_g295ad0959f34` |
| Architecture / CPU | `x86_64`; `AMD EPYC 9D85 158-Core Processor` |
| Hermit source | `4630ee2a61f93221a43e2baec0a15f34102a2c61` |
| Hermit binary | 415,330,552 bytes; SHA-256 `5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a` |
| Backend and policy | ptrace; `strict=true`; `chaos=true`; `relaxations=[]` |
| Scheduler | `random`; RCB PMU available and effective; no fallback |
| Guest environment | minimal base; `LC_ALL=C`; `TZ=UTC` |
| Mounted build tree | `/home/newton/work/tfa/fbsource/buck-out` bind-mounted at the identical guest path |
| Per-invocation bounds | 120 s wall; 17,179,869,184 bytes memory; zero swap; 67,108,864-byte stderr and retained-log caps; 8 GiB disk guard |

Later schema roots also made the 67,108,864-byte stdout cap explicit. Earlier
records lack that optional field; this is a recorded schema difference, not a
retroactive assertion. All twelve successful witness executions report an
unfired stdout cap, no timeout, exactly one selected test, authenticated
safehermit and verifier qgroups, and `BitwiseInfoV1=matched`.

The base cells swept `--fuzz-seed` while fixing `--seed`, `--rng-seed`, and
`--sched-seed` at zero and enabling `--chaos-target-races`. Continuation cells
instead swept `--sched-seed`, fixed the other three roots at zero, and used
prospectively chosen scheduler/time controls. The three successful cells were:

| Target | Frozen cell and SHA-256 | Time/schedule controls |
| --- | --- | --- |
| HTTPCoro | `httpcoro-kernel71-ptrace-strict-midpoint-slowdown-v1`; `c2628d6421a726f0f9dd497cfbed4bc952234c35df621c16e030c2d1ae93988e` | clock multiplier 0.5; per-thread slowdown factor 10.0; zero-length slowdown epoch; no target timeslice; 200,000,000-ns maximum timeslice |
| Thrift | `thrift-instrumentation-kernel71-ptrace-strict-chaos-sched-slowdown-now-v1`; `e882c837b3fee09e5779325938b2c7090cb09bec01210e5b5e65e5f8be6a6b5e` | clock multiplier 1.0; per-thread slowdown factor 10.0; zero-length slowdown epoch; 1,000,000-ns target timeslice; 200,000,000-ns maximum timeslice |
| AsyncSSL | `asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-factor8-slice5000000-now-v1`; `3ef3715f71a7349258292ee5fe3095ef701f1fb8096a17a9975376231504dd11` | clock multiplier 1.0; per-thread slowdown factor 8.0; zero-length slowdown epoch; 5,000,000-ns target timeslice; 200,000,000-ns maximum timeslice |

Their PLANNED receipts, respectively, are commit/blob
`910f2ccb4e4341cd9eac8707e93ab4194e49fd7f` /
`20f2d07ab2453b24d753c46401a02da02dc87465`,
`000fb6342ac55f9807ed0f927d776cfe94315a70` /
`3827afb144681c6580aaefbd81a8f9b1227afcd9`, and
`2909759234b6cea4152b488ff3a1cf42a6634b55` /
`b3900ab9e9ca4b70ad0929610694bde4d1248396`.

### 3.3 Seeds, replicas, and stopping

A discovery sample is one unique `(frozen cell, swept flag, seed value)` tuple.
Seeds followed the committed numeric order without replacement. Initial
rate-bound cells used the smallest required valid-tuple count satisfying
`p^N + (1-p)^N <= 0.05`, where `p` was explicitly only a planning prior; a
separate attempted-tuple cap allowed replacement of censored observations.
Continuation cells used complete finite populations, generally seeds 0–15,
and made no independence or miss-rate claim.

The stop rule was `BOTH_ARMS_THEN_REPEAT`: stop after two distinct seeds supply
the two outcomes and both exact tuples satisfy their confirmation requirement;
otherwise exhaust the sealed budget. Within each qualifying seed, replica 1 is
DISCOVERY and replica 2 is CONFIRMATION. They are the two logical guest
executions created and checked by the same outer `--verify` measurement. A
confirmation does not add to `S`, and no majority vote can repair a disagreeing
tuple.

The counters therefore have deliberately different denominators:

- `S=201`: valid discovery tuples with an unambiguous target PASS or
  predeclared target FAIL.
- `C=93`, `T=294`: censored and total attempted discovery tuples, so
  `T=S+C`.
- `R=512`: Hermit executions for which the guest started, including
  confirmations.
- `A=10`: distinct canonical targets. Only `A` is the denominator for
  `W`, `B`, and `U`.

### 3.4 Pass/fail oracles for the successful targets

The oracles were source- and output-specific, fixed before their cells ran:

- **HTTPCoro:** PASS required the single selected ResetRateLimit case to emit
  its `[ OK ]` line and one-test summary and exit zero. FAIL class
  `reset-rate-limit-line1685-body-event-fatal` required the selected-test
  context, exactly one
  `HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()`
  line, the named selected-test stack, and SIGABRT/exit 134.
- **Thrift:** PASS required the selected instrumentation case and one-test
  summary with no snapshot failure. FAIL class `gtest-snapshot-size-2`
  required the contiguous `InstrumentationTest.cpp:836` block in which
  `reqSnapshots.size()` was 2 rather than 3. Later TSan or teardown output
  counted only as fallout after that exact block.
- **AsyncSSL:** PASS required the selected byte-event test, its `[ OK ]` line,
  one-test summary, and absence of both source-pinned failure blocks. FAIL
  class `gtest-byte-events-three-missing-ack` required both the line-3797
  byte-event size-three block and the line-3812 expected-offset 501 versus
  all-zero ACK block, followed by the selected test's timed failure and
  one-failed-test summary. A line-3592 failure, either required block alone,
  or signal-only behavior was not this class.

## 4. Results

### 4.1 Campaign accounting

Across the 25 material records at the evidence cutoff, canonical-target
deduplication gives:

```text
A/W/B/U = 10/3/0/7
S = 201
C/T = 93/294
R = 512
W/A = 3/10; B/A = 0/10; U/A = 7/10
Campaign conclusion: SUCCESS, because W=3 distinct targets.
```

The per-target accounting shows why summing per-root `A` or `W` would be wrong:

| Canonical target | Effective outcome | Material records | S | C/T | R |
| --- | --- | ---: | ---: | ---: | ---: |
| `fbcode//cachelib/common/tests:event-tracker-test` | NOT-DETERMINABLE | 1 | 0 | 1/1 | 1 |
| `fbcode//folly/io/async/test:async_ssl_socket_test` | WORKING | 10 | 122 | 38/160 | 295 |
| `fbcode//folly/io/async/test:timerfd_timeout_manager_test` | NOT-DETERMINABLE | 1 | 9 | 0/9 | 18 |
| `fbcode//logdevice/logservice/server/log_store/test:log_store_test` | NOT-DETERMINABLE | 1 | 0 | 1/1 | 2 |
| `fbcode//logdevice/admin/health_monitor:tests` | NOT-DETERMINABLE | 1 | 0 | 1/1 | 1 |
| `fbcode//mcrouter/facebook/test/toaster_tests:mc_refill_route_wrapper_test` | NOT-DETERMINABLE | 1 | 0 | 1/1 | 2 |
| `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` | WORKING | 3 | 10 | 18/28 | 52 |
| `fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test` | NOT-DETERMINABLE | 1 | 0 | 1/1 | 2 |
| `fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test` | NOT-DETERMINABLE | 2 | 0 | 31/31 | 19 |
| `fbcode//thrift/lib/cpp2/test:InstrumentationTest` | WORKING | 4 | 60 | 1/61 | 120 |
| **Total, with targets deduplicated** | **3 WORKING, 0 BLOCKED, 7 NOT-DETERMINABLE** | **25** | **201** | **93/294** | **512** |

Every one of the 13 root validators passed. The three qualifying roots report:

```text
HTTPCoro: A=1 W=1 B=0 U=0 S=2 C/T=2/4 R=4
Thrift:   A=1 W=1 B=0 U=0 S=2 C/T=0/2 R=4
AsyncSSL: A=1 W=1 B=0 U=0 S=6 C/T=6/12 R=22
```

The complete per-root validator accounting is:

| Root suffix after `experiments/tfa-kernel71-` | Records | A/W/B/U | S | C/T | R |
| --- | ---: | --- | ---: | ---: | ---: |
| `campaign_20260921` | 10 | 10/0/0/10 | 16 | 36/52 | 89 |
| `asyncssl-factor2-continuation_20260921` | 1 | 1/0/0/1 | 14 | 2/16 | 28 |
| `asyncssl-factor5-continuation_20260921` | 1 | 1/0/0/1 | 14 | 2/16 | 29 |
| `asyncssl-factor8-continuation_20260921` | 1 | 1/0/0/1 | 16 | 0/16 | 32 |
| `asyncssl-slice10-continuation_20260921` | 1 | 1/0/0/1 | 10 | 6/16 | 28 |
| `asyncssl-slice20-continuation_20260921` | 1 | 1/0/0/1 | 10 | 6/16 | 24 |
| `asyncssl-slice5-continuation_20260921` | 1 | 1/1/0/0 | 6 | 6/12 | 22 |
| `asyncssl-slice50-continuation_20260921` | 1 | 1/0/0/1 | 12 | 4/16 | 30 |
| `httpcoro-midpoint-continuation_20260921` | 1 | 1/1/0/0 | 2 | 2/4 | 4 |
| `sched-continuation_20260921` | 1 | 1/0/0/1 | 4 | 12/16 | 30 |
| `sched-input-campaign_20260921` | 1 | 1/0/0/1 | 29 | 0/29 | 58 |
| `thrift-slowdown-continuation_20260921` | 1 | 1/1/0/0 | 2 | 0/2 | 4 |
| `time-input-campaign_20260921` | 4 | 4/0/0/4 | 66 | 17/83 | 134 |

Each root by itself reports campaign conclusion `neither`, because every root
has at most one WORKING target and SUCCESS requires three. SUCCESS is the
contract-defined, canonical-target-deduplicated
union across the 25 records, not the output of the ten-record base root alone.
For reference, the base root validates as
`A/W/B/U=10/0/0/10, S=16, C/T=36/52, R=89`; the later prospectively sealed
roots supply the successful cells without overwriting those earlier results.

### 4.2 Repeated witnesses

| Target | PASS seed | FAIL seed and exact class | Qualifying executions |
| --- | ---: | --- | ---: |
| `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` | 3 | 0 — `reset-rate-limit-line1685-body-event-fatal` | 4 |
| `fbcode//thrift/lib/cpp2/test:InstrumentationTest` | 0 | 1 — `gtest-snapshot-size-2` | 4 |
| `fbcode//folly/io/async/test:async_ssl_socket_test` | 1 | 11 — `gtest-byte-events-three-missing-ack` | 4 witness executions; 22 guest-started executions in the root |

All twelve witness executions selected one test, avoided timeout and output-cap
truncation, and matched their same-seed partner under strict
`BitwiseInfoV1`. The additional valid AsyncSSL PASS seeds 4, 5, 6, and 8 are
discovery evidence but are not additional targets or required witnesses.

### 4.3 Per-target observations and interpretation

The following first sentence in each item is confirmed by retained records.
Any mechanism statement explicitly labelled “hypothesis” goes beyond what the
acceptance result proves.

- **Cachelib EventTracker:** seed 0 was censored after the selected native case
  exceeded 13 seconds and the formal run reached the 120-second outer timeout;
  seeds 1–93 were not run. No target verdict was observed. The data do not
  distinguish target runtime from ptrace amplification or another stall.
- **AsyncSSL:** the original target-races cell produced seven valid PASS tuples,
  and several scheduler-slowdown/timeslice continuations produced PASS and
  censored outcomes without the predeclared FAIL. In the factor-8,
  5,000,000-ns-timeslice cell, seed 1 repeatedly passed and seed 11 repeatedly
  produced the exact paired line-3797/3812 failure. Confirmed interpretation:
  the recorded cell and scheduler seed select both repeatable outcomes.
  Hypothesis: the slowdown/timeslice combination changes relative byte-event
  and ACK ordering; these experiments do not identify the responsible source
  race.
- **Folly TimerFD:** all nine required valid fuzz-seed tuples passed and the
  planned budget ended without a predeclared FAIL. This is
  NOT-DETERMINABLE—not evidence that the target is nonflaky.
- **LogDevice log store:** native screening emitted no selected-test marker by
  13 seconds, and formal seed 0 failed in the loader before the target oracle.
  The tuple is censored; the target's race behavior was not measured.
- **LogDevice health monitor:** native profiling observed 657 threads. Formal
  seed 0 stopped before the oracle in a two-thread scheduler deadlock followed
  by an internal container failure. Hypothesis: ptrace fanout makes this route
  impractical, but the record does not satisfy the stronger causal and
  all-routes requirements for `B`, so the classification remains `U`.
- **Mcrouter McRefill:** the native selected case emitted no marker by 25
  seconds, and formal seed 0 failed in the loader before the target oracle.
  The result is censored and causally insufficient for a named blocker.
- **HTTPCoro:** the original fuzz-seed cell produced a repeatable target-owned
  line-1685 fatal CHECK for seeds 0–15, but that CHECK was not a predeclared
  failure class, so all sixteen tuples correctly remained censored. A separate
  clock-0.125 cell yielded eight valid PASS tuples. The prospectively sealed
  midpoint cell then fixed the clock multiplier at 0.5; scheduler seed 0
  repeated the exact predeclared line-1685 FAIL and seed 3 repeated PASS.
  Confirmed interpretation: the selected phase is single-threaded, and
  seed-derived slowdown plus virtual-time progression makes both outcomes
  selectable. The retained analysis attributes the threshold crossing to a
  100-ms rate-limiter reset interval before 1,001 resets; it is a controlled
  time/order result, not evidence of a competing-thread race in this target.
- **Proxygen transport monitor:** formal execution stopped before the selected
  test when Hermit's post-open `inject_fstat` path converted the enclosing
  `openat` to `EFAULT`; native inspection also found the selected case
  synchronous. Because the full blocker contract was not satisfied, this is
  `U`, not `B`.
- **Proxygen TLS terminator:** target-races seeds 11 and 12 repeated a
  certificate-not-yet-valid abort under the fixed January epoch, but retained
  output did not satisfy the predeclared GoogleTest failure class. The other 13
  tuples in that cell and all 16 scheduler-input tuples were censored; only 19
  of the 31 attempted tuples started a guest execution. Hypothesis: fixed time
  and certificate validity explain the two aborts, but they are not an accepted
  flaky-test arm.
- **Thrift Instrumentation:** one early otherwise passing observation was
  permanently rejected because its intended invocation lacked the sealed 8
  GiB wrapper-log disk guard. A scheduler-only continuation produced 29 valid
  PASS tuples, and a target-races continuation produced 29 PASS plus one
  censored tuple. In the separate slowdown/1,000,000-ns-timeslice cell, seed 0
  repeatedly passed and seed 1 repeatedly produced the exact size-2 rather
  than size-3 snapshot assertion. Confirmed interpretation: the frozen cell and
  seed select both outcomes. Hypothesis: the controls expose the callback/baton
  snapshot ordering seen in native profiling; the campaign does not prove a
  source-level cause.

## 5. Evidence integrity and validation boundary

The dependency-free schema-v2 validator checks registry identity, plan/route/
cell digests, argv and seed substitution, sample ownership, oracle results,
matcher payloads, tracked artifact bytes, strict plan ancestry, and derived
counters. It deliberately prints `evidence_authentication=NOT_CHECKED`: an
offline consistency checker is not execution attestation and cannot establish
the identity of the author or machine that produced bytes.

Direct review at the evidence head established:

- all 25 plan receipt commits are strict ancestors and each recorded PLANNED
  blob is the exact blob at the receipt commit and path;
- all plan, route, and cell SHA-256 values recompute exactly;
- every tracked artifact in the qualifying records matches its committed size
  and SHA-256: HTTPCoro 38/38, Thrift 26/26, and AsyncSSL 149/149, with no
  missing, mismatched, truncated, staged-only, or uncommitted artifact;
- the live local Hermit binary remains 415,330,552 bytes with the recorded
  SHA-256, and the three live test ELF hashes match their cells; and
- at the original evidence audit, the retained winner verifier qgroups were
  readable and below the exact 8,000,000,000-byte limits: 53,035,008,
  36,397,056, and 75,313,152 referenced bytes for HTTPCoro, Thrift, and
  AsyncSSL, respectively.

The repaired historical HTTPCoro attempts absent from their record remain
uncounted. Diagnostic fixed-binary and native sweeps remain non-formal and
uncounted. Review found no weakened assertion, widened tolerance, exemption,
skip, comparator relaxation, failure relabelling, or deleted check. In
particular, the many AsyncSSL line-3592 outcomes remain censored rather than
being retroactively accepted as the line-3797/3812 class.

## 6. Limitations and threats to validity

1. **Selection and adaptivity.** Only 10/30 registry targets were materially
   attempted. Native admission and later cell design intentionally pursued
   tractable, scheduler-sensitive cases. Although every continuation was
   sealed prospectively, the sequence was informed by earlier roots. The 3/10
   ratio is campaign accounting, not a population success-rate estimate.
2. **One environment.** Every result uses one host, CPU, kernel, target source
   revision, and Hermit binary. Portability to another kernel, CPU, backend, or
   revision was not tested.
3. **Ptrace effects.** Ptrace overhead can alter timing and made high-fanout
   targets costly. This experiment asks whether Hermit can create stable
   witnesses under its recorded backend, not whether their native frequency is
   preserved.
4. **Existence, not rates.** Successful finite sweeps stopped when both arms
   and confirmations existed. Confirmation replicas check a fixed property;
   they are not independent race samples. No PASS/FAIL probability or
   confidence interval is estimated.
5. **Oracle scope.** The result accepts only original, predeclared target-owned
   symptoms. Censoring real but undeclared target behavior protects validity,
   but can understate other reproducible failures.
6. **Build boundary.** Buck and its build graph were outside Hermit. The result
   covers execution of identified Buck artifacts, not deterministic builds.
7. **Authentication boundary.** Git history, hashes, raw output, safehermit
   reports, and qgroups make alteration detectable within the stated model,
   but the generic validator does not cryptographically attest execution.
8. **Mechanism.** Scheduler-seed association demonstrates control and
   repeatability. It does not by itself prove which source interleaving caused
   the bug; the causal explanations above remain hypotheses unless the record
   contains a direct boundary diagnosis.

## 7. Reproduction and audit procedure

The complete evidence history, including every PLANNED receipt, must be
present; checking out only a final tree is insufficient for ancestry checks.
From the repository root:

```sh
# Validate all 13 kernel-7.1 roots. Each command must exit zero.
for root in experiments/tfa-kernel71*20260921; do
  python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
    --root "$root" || exit 1
done

# Exercise the validator's refusal cases as well as its accepting cases.
python3 -m unittest discover \
  -s experiments/tfa-flaky-tests_20260917/tools -p 'test_*.py'
```

The base-root command alone is:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-campaign_20260921
```

It should print `A=10 W=0 B=0 U=10 S=16 C=36 T=52 R=89`; this is not a
contradiction. Reproduce the published aggregate by summing `S`, `C`, `T`, and
`R` over all 25 records, grouping outcomes by
`identity.canonical_target`, and treating a target as effectively WORKING when
one of its immutable roots has a valid WORKING bracket. The resulting target
partition must be exactly the ten rows in Section 4.1.

For an execution-level audit, inspect each `events[].argv` and the tracked
`*.invocation.json` files. The command must begin with `bin/safehermit`; its
Hermit segment must include ptrace, strict, chaos, all four seed roots, bounds,
and strict verification; the argv after `--` must begin with the recorded
absolute test ELF rather than `buck`. Rehash tracked artifacts and compare
their byte counts to each artifact object. Finally, verify each
`plan_receipt.commit` is a strict ancestor and that
`git rev-parse COMMIT:PATH` equals the recorded receipt blob.

## 8. Conclusions and next work

The fixed campaign question has a positive answer for three distinct real Buck
targets: under identified strict-ptrace cells, Hermit scheduler seeds select a
repeatable original PASS and a repeatable predeclared target FAIL. The result
is strongest as a schedule-localization primitive, not as a prevalence or
root-cause claim. The strict handling of undeclared failures, timeouts,
pre-guest errors, and an under-bounded Thrift run is part of the result: SUCCESS
was reached without weakening the acceptance bar.

The next useful experiments are to replay the three brackets on another host
and kernel, repeat them with a production backend that does not impose ptrace
overhead, and turn each seed contrast into a source-level causal explanation.
Campaign infrastructure should also gain one aggregate validator for multiple
immutable roots, so the canonical-target union is machine-produced rather than
reconstructed beside otherwise per-root validation. For the seven unresolved
targets, work should focus on loader compatibility and censor reduction before
spending more seeds; broader claims require prospectively attempting more of
the fixed 30-target cohort.
