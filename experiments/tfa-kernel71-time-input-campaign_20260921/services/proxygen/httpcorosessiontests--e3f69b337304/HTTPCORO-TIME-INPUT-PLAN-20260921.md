# HTTPCoro controlled time-input plan

## Causal boundary

The selected test queues 1,001 reset events on one EventBase OS thread and
expects the 1,001st event to breach a 1,000-events-per-100-ms rate limiter.  The
limiter is driven by `CLOCK_MONOTONIC`; initial realtime epoch and ordinary file
timestamps are additive/noncausal for this relative window.  Native overhead
that stops at every rate-limiter increment reproduces the fatal CHECK at
`HTTPDownstreamCoroSessionTests.cpp:1685` before `HTTPCoroSession::onError`.

The guest has one relevant thread (`dettid=3`).  Fuzz-seed and happens-before
edges cannot reorder its callbacks.  Hermit's per-thread chaos slowdown is
still causal: it deterministically changes virtual nanoseconds per retired
conditional branch as a pure function of scheduler seed, deterministic thread
ID, and slowdown epoch.

## Frozen input vector

The one cell fixes `--clock-multiplier=0.125`, a single slowdown epoch, maximum
slowdown factor 10, and all non-scheduler seeds.  It sweeps only these eight
prospectively selected scheduler seeds, alternating low/high source-derived
Q32 factors:

| order | scheduler seed | Q32 units | slowdown factor | factor x 0.125 |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 7 | 463942141 | 0.108019947307184 | 0.013502493413398 |
| 1 | 5 | 31621789471 | 7.362521596020088 | 0.920315199502511 |
| 2 | 4 | 1027996744 | 0.239349143579602 | 0.029918642947450 |
| 3 | 2 | 18326736791 | 4.267025923123583 | 0.533378240390448 |
| 4 | 15 | 1161677030 | 0.270474010612816 | 0.033809251326602 |
| 5 | 0 | 12528499331 | 2.917018563253805 | 0.364627320406726 |
| 6 | 3 | 1950430690 | 0.454120032954961 | 0.056765004119370 |
| 7 | 1 | 4919407278 | 1.145388762932271 | 0.143173595366534 |

These values come from the sealed Hermit algorithm, not observed test outcomes.
The population is finite and makes no probabilistic independence claim.  Stop
after a repeatable PASS and the exact repeatable fatal-CHECK FAIL occur at two
distinct seeds in this same cell; otherwise exhaust all eight.

## Epoch and current product gap

The epoch `2026-09-21T18:14:41Z` was captured once at the pre-run planning
boundary and is passed literally in every invocation.  This is a harness-level
workaround for the current CLI gap: Hermit defaults to fixed
`2026-01-01T00:00:00Z`, has no `now` resolver, omits the default from rendered
commands, and does not persist a reloadable resolved epoch.  No Hermit source is
changed by this experiment.

A production fix should sample host UTC once before backend launch, emit the
resolved RFC3339 value, persist it in the run/record manifest, and make verify
and replay reuse that exact value.  Programmatic Detcore defaults must remain
non-ambient, and continuous virtual-time progression must not be frozen,
rounded, reset, or otherwise weakened.

## Oracle integrity

PASS requires the exact selected `[ OK ]` line, a one-passed-test summary, exit
0, and no signal.  The sole FAIL class requires the exact line-1685
`Check failed: bodyEvent.hasException()` text, the selected-test stack frame,
SIGABRT/134, and no Hermit, wrapper, dependency, sanitizer, infrastructure, or
outer-timeout failure.  Any other abnormal result is censored.  The older
fuzz-seed attempts used a different predeclared class and are not imported.
