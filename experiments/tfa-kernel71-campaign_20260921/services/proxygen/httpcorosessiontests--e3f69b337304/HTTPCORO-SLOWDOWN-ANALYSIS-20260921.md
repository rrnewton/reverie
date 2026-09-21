# HTTPCoro ResetRateLimit slowdown analysis

The original fuzz-seed cell is no longer being expanded.  Owner correction on
2026-09-21 requires a prospective controlled-time continuation, now planned in
`experiments/tfa-kernel71-time-input-campaign_20260921`; the single-thread
diagnosis rules out a thread/HB explanation but does not make virtual-time
inputs inert.  Seeds 0 through 15 in the original cell all produced a
repeatable strict-ptrace `SIGABRT` at
`HTTPDownstreamCoroSessionTests.cpp:1685`, but the mechanism is a
single-thread time-budget assumption rather than a scheduler-sensitive race.
The record remains nonterminal `NOT-DETERMINABLE`: it has no PASS arm, and the
predeclared 100-seed budget was not exhausted.

## Evidence

- Frozen target:
  `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests`, exact case
  `HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`, ELF
  SHA-256 `00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc`.
- Five bounded ordinary native executions passed the unchanged selected test
  in 303, 330, 318, 325, and 314 ms (outer wall times 0.48-0.69 s).
- Three bounded `strace` executions also passed after setting only
  `ASAN_OPTIONS=detect_leaks=0`, required because LeakSanitizer refuses ptrace.
  Each trace showed one eventfd, one epoll instance, eight epoll waits, and no
  competing application thread during the selected test. The sole `clone3`
  occurred during sanitizer teardown.
- Three bounded `perf stat` executions passed. They recorded 10-14 context
  switches over 347-390 ms of task-clock time, not evidence of competing
  guest callbacks.
- A strict Hermit run contains only deterministic thread id 3 and 6,168
  scheduler selections from a one-element `[0,1)` run queue. The retained INFO
  log contains no happens-before/target-race eligibility decision.
- Independent breakpoint analysis forced overhead on every
  `RateLimiter::incrementNumEventsInCurrentInterval` call in an otherwise
  native execution. It reproduced the same line-1685 abort before
  `HTTPCoroSession::onError`, establishing the same causal ordering without
  Hermit.

Raw native diagnostics are retained at
`/home/newton/work/dev-hermit/ignored/tfa-hb-analysis-20260921`.

## Causal chain

`RateLimiter::incrementNumEventsInCurrentInterval` in
`fbcode/proxygen/lib/http/codec/RateLimitFilter.cpp` schedules a reset timer on
the first event and clears the count when the interval expires. The H2 session
sets the RST limit to 1,000 events per 100 ms in
`fbcode/proxygen/lib/http/coro/HTTPCoroSession.cpp`. The test sends 1,001
resets, assumes they all land inside that interval, turns the event loop twice,
then requires the original request body read to carry `HTTPErrorCode::CANCEL`.

Native execution normally processes the reset burst before the interval
expires. Sufficient instrumentation overhead, including Hermit's virtual-time
and syscall cost, lets the reset timer clear the counter before the 1,001st
event. The rate-limit error path never calls `HTTPCoroSession::onError`, the
original stream is not cancelled, and the assertion at line 1685 aborts. No
alternative thread schedule is required or available.

## Concrete target-side fix

Do not change Hermit's scheduler for this failure. Make the test's counting
invariant independent of host or virtual-time throughput: in
`HTTPDownstreamCoroSessionTests.cpp::ResetRateLimit`, call
`session_->setRateLimitParams(RateLimiter::Type::RSTS, 1'000, <long bounded
test interval>)` before sending resets, using an interval long enough that the
test cannot cross it under supported instrumentation. Then continue to assert
that the 1,001st reset invokes `onIngressError` and cancels the original body.
An event/predicate-driven assertion is preferable to adding more `loopN`
iterations, because additional loop turns cannot restore a counter that the
100-ms timer already reset.
