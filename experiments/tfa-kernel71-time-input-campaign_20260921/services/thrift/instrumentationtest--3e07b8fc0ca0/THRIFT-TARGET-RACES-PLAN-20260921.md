# Thrift Instrumentation target-races plan

## Scheduler-sensitive causal boundary

The exact selected test submits three RPCs, blocks and releases callbacks with
Folly batons, and snapshots request state while worker/event-base threads can
still change eligibility.  Its bounded CPU-0 native screen passed three times
in 0.55--0.59 seconds and once reached the exact target assertion at
`InstrumentationTest.cpp:836` in 1.57 seconds.  The strace control recorded 16
TIDs, 15 `clone3`, 307 futex, 6 eventfd, 4 epoll instances, 45 `epoll_ctl`, and
43 `epoll_wait` calls.  These are planning diagnostics, not formal evidence.

## Frozen input vector

The single cell uses strict ptrace chaos with `--chaos-target-races`, random
scheduling, and a 200,000,000-RCB maximum timeslice.  Seed, RNG seed, and
scheduler seed are fixed to zero.  Only `--fuzz-seed` varies, in the explicit
without-replacement order 0 through 35.  The 36-attempt cap and 29-valid-sample
rate-bound requirement were chosen prospectively for a 10% minimum detectable
Hermit FAIL probability and at most 5% miss probability.  Native outcomes do
not parameterize or count toward that rate.

The explicit epoch `2026-09-21T19:01:04Z` was captured at the planning boundary
and is replayed literally.  Normal continuous virtual-time progression remains
enabled; no clock multiplier, frozen time, or metadata-time override is used.

The launcher retains stdout through a constant-memory streaming reader with a
lethal 67,108,864-byte cap.  On overflow it kills the safehermit transient
cgroup, continues draining without retaining further bytes, and records the
cap and kill result; incomplete stdout is always censored.  The plan receipt
also seals the exact Git blobs of the Thrift create, run, and admission helpers,
which every later run and admission must match.

## Oracle integrity

PASS requires the exact selected `[ OK ]` line, one-passed-test summary, clean
exit 0, strict BitwiseInfoV1 parity, and no sanitizer verdict.  The sole FAIL
class is `gtest-snapshot-size-2`: after the exact selected RUN line, stdout must
contain exactly one contiguous line-836 block saying `reqSnapshots.size()` is
2 rather than 3, with target nonzero exit or selected-test SIGABRT context.

A later TSan report or Baton teardown abort is fallout, not an additional FAIL
class.  TSan/Baton output without the exact line-836 block, sanitizer-only,
Hermit, wrapper, loader, dependency, infrastructure, OOM, truncation, and outer
timeout results are censored.  The older launch-boundary attempt is not
imported into this prospective cell.
