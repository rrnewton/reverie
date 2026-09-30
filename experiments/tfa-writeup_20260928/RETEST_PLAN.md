# Plan: retest the three loader-blocked targets on fixed Hermit

*Written 2026-09-30 by sub-tfa2-030. **Not started.** coord2 is confirming with the owner first. This is
next step 1 of [README.md](README.md) section 5.*

## Why

In the kernel-7.1 campaign, a Hermit loader bug stopped three of the ten attempted targets before their
tests ran. The bug turned a successful `openat` of a shared library into EFAULT
(https://github.com/rrnewton/hermit/issues/3328).

The fix, https://github.com/rrnewton/hermit/pull/3374, landed on hermit main as
`3b5f7cdc43247abad79585feae1469e23b69f241` on 2026-09-30. Its three `detcore` files are byte-identical
to the reviewed head `c6867781`. coord2 reported the exact-head validation of `502b1f4c` as passing
278 of 278 nodes and 3,840 of 3,840 tests in 34 min 36 s.

Nobody has yet re-run these three targets on the fixed Hermit. The question is the paper's: does Hermit
now produce a passing seed and a failing seed for any of them?

## The three targets

| Target | Build mode | Selected case | Native evidence | Expectation (inference) |
|---|---|---|---|---|
| `fbcode//logdevice/logservice/server/log_store/test:log_store_test` | `@fbcode//mode/dev` | `LogStoreTest.StoreRecordCondWriteFailedSameEpoch_FallbackReadFailsAndRetries` | no test output natively by 13 s; CI failed 3 of 32 in the sampled week, but no failure text in the last week | gets past the loader; may need a long limit before its test runs |
| `fbcode//mcrouter/facebook/test/toaster_tests:mc_refill_route_wrapper_test` | `@fbcode//mode/dev-tsan` | `McRefillWrapperBucketized/0.refillOnMiss` | no test output natively by 25 s; starts 6 server processes; CI failures are `McToasterUtils.h:161` `mc_res_timeout`, 2 of 3 of them at CI's 600 s limit | gets past the loader; probably too slow under ptrace to reach a verdict |
| `fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test` | `@fbcode//mode/dev-asan` | `PopulateEdgeConnectionsLoggerTests/PopulateEdgeConnectionsLoggerTest.PopulateEdgeConnectionsLoggerEvent/without_response` | passes natively in 36 ms (4.25 s wall); the selected case is synchronous, with 5 threads; no CI failure in the last week | reaches its test and passes; a failing seed is unlikely |

The native evidence is from `experiments/tfa-kernel71-campaign_20260921/NATIVE-CANDIDATE-SCREEN-20260921.md`
and [TARGETS.md](TARGETS.md) rows 9, 13 and 19.

## Where it runs

The work runs on an **OnDemand agentcloud node**, never on devbig030. Under the owner's 2026-09-28
policy, devbig030 gets no fbsource checkouts, no large Buck builds and no heavy experiments. The two
LogStore and McRefill test binaries were evicted from devbig030's buck-out anyway.

## Steps

1. **Start a node.** Run `agentcloudctl create --harness claude-code --provision --envspec fbsource ...` and
   give the child this plan. Every output goes into a new dev-hermit experiment root,
   `experiments/tfa-loader-retest_<date>/`, pushed from the child through its own route.
2. **Build the three test binaries.** Check out fbsource at the campaign's revision
   `7f239d5b37dfbecf32fed144468da461f998add5` if it can still be checked out. If not, use current master and
   record it. Build each target in its campaign mode with `buck2 build`. Record each test binary's path,
   size and sha256.
3. **Build Hermit.** Use hermit main at or after `3b5f7cdc` in release mode
   (`cargo build --release -p hermit --bin hermit`). Record the commit and the binary's sha256.
4. **Run each selected case natively, three times.** Record pass or fail and the wall time. This sets the
   baseline and each target's time limit: for trials under Hermit, 2 runs × 13 × the native time, rounded
   up, with a floor of 120 s and a cap of 900 s.
5. **Check the loader.** Run one `hermit run --strict` per target at seed 0. Confirm that the loader no
   longer exits 127 and that the selected test starts.
6. **Declare the failures, and commit them before any sweep.** For each target, the only thing that counts
   as FAIL is a failure printed by the target itself:
   - **McRefill:** the `McToasterUtils.h:161` `mc_res_timeout` assertion that CI shows;
   - **LogStore and transport monitor:** a GoogleTest assertion failure in the selected case, with no CI
     text to match, which is weaker evidence.

   Outer timeouts, sanitizer-only reports and Hermit errors are discarded, as in the campaign.
7. **Sweep seeds.** Every run goes through `bin/safehermit`, one at a time.
   - First, the campaign's base configuration for that target: fuzzing seeds with target races. Use 6
     seeds.
   - If that gives only one outcome, one follow-up: per-thread slowdown up to 8x with a 5 ms timeslice.
     This is the setting shape that produced the campaign's successes. Use 6 more seeds.
   - Stop a target as soon as it has a repeatable passing seed and a repeatable failing seed.
8. **Record the results.**
   - Put the per-trial argv, result, stdout, stderr and safehermit report in the experiment root.
   - Update [TARGETS.md](TARGETS.md), [targets.csv](targets.csv) and Figure 1 (re-run
     `figures/make_cause_buckets.py`), and add one results paragraph to README section 3.2.

## Budget and estimate

These are estimates, not measurements.

| Stage | Estimate |
|---|---|
| Node provisioning and fbsource checkout | 0.5 to 1 h |
| Three Buck builds (dev, dev-tsan, dev-asan service tests, cold cache) | 1 to 2 h |
| Hermit release build | about 10 min |
| Native baselines and loader checks | about 30 min |
| Sweeps: at most 12 trials per target | transport monitor under 30 min; LogStore and McRefill up to about 3 h each at the 900 s cap |
| Analysis and write-up | 1 to 2 h |
| **Total** | **about half a day to a day of agent time** |

The owner's cap for this kind of round was 6 seeds per test with 30 minutes of wall time per test. That
cap would stop LogStore and McRefill early if each trial needs many minutes. The owner should either
accept the larger per-target budget above or cap each target at a stated wall time.

## Possible outcomes

- A target with both seeds adds a fourth success, and its bucket in Figure 1 changes.
- A target that now reaches its test but only passes moves to "ran, no failing seed".
- A target that times out before its test moves to "stopped by another Hermit limit" (ptrace speed).
- A new Hermit defect gets a GitHub issue before anything else is recorded.
