# TLS terminator scheduler/NOW plan

## Candidate and causal basis

The one selected case is
`TlsTerminatorTest/TlsTerminatorTest.terminateTls/h1_innerTls_http_connect`
from canonical Buck target
`fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test`.
Its native screen completed in 0.24 seconds and observed 28 TIDs, 27 `clone3`
calls, 1,157 futex calls, and nine eventfd/epoll instances.  It is therefore a
short, genuinely multithreaded scheduling candidate.

The previous fixed-epoch cell used Hermit's `2026-01-01T00:00:00Z` default and
produced certificate-not-yet-valid aborts.  Those results were setup/time
failures, are not imported, and cannot satisfy this cell's target-owned FAIL
oracle.  This prospective cell instead passes the realistic epoch captured at
its planning boundary, `2026-09-21T18:54:35Z`, literally on every invocation.

## Frozen cell and search

The cell uses the ptrace backend with `--strict --chaos`, random scheduling,
`--max-timeslice 200000000`, `--clock-multiplier=1.0`, and strict
BitwiseInfoV1 verification with no relaxations.  It fixes `--seed 0`,
`--rng-seed 0`, and `--fuzz-seed 0`; only `--sched-seed` varies.

The finite population and execution order are the sixteen scheduler seeds 0
through 15 inclusive, ascending, without replacement.  The order was sealed
before any run and uses no observed outcome.  No probabilistic independence
claim is made.  Stop after distinct seeds provide a repeatable PASS and a
repeatable predeclared target-owned FAIL in this one cell; otherwise exhaust
all sixteen tuples.

This cell deliberately has no `--chaos-target-races`, `--fuzz-futexes`,
per-thread slowdown, or happens-before flags.  There is no hidden coordinate:
the full literal argv, including epoch and all four seed roots, is stored by the
plan and each admitted event.

The runner drains stdout in constant memory and retains at most 67,108,864
bytes.  On the first overflow it reads safehermit's already-emitted transient
unit identity, SIGKILLs that complete cgroup, continues draining without
writing, and records the byte counts and kill result.  Any such event is
CENSORED, never PASS or target-owned FAIL.  The prospective plan receipt also
seals the exact create/run/admit helper blobs; every run and admission refuses
helper drift from that commit.

## Prospective oracle

PASS requires exactly one selected `[ RUN ]` line, the exact selected `[ OK ]`
line, a one-passed-test summary, exit 0 without a signal, and a successful
strict BitwiseInfoV1 match.

The target-owned FAIL classes are fixed before execution:

- exact selected GoogleTest failure: one selected timed `[ FAILED ]` line, the
  one-failed-test summary, a stdout/stderr failure block at a pinned
  selected-case assertion line in `TlsTerminatorTest.cpp`, and exit 1 without
  a signal;
- uncaught `folly::AsyncSocketException`: the exact exception termination
  marker after the selected `[ RUN ]`, a matching selected `TestBody` lambda
  frame/source-line pair, the exact guest Signal 6/SIGABRT stderr marker, and
  authenticated safehermit `unit_result=core-dump`, `process_signal=6`, and
  `exit_code=134`;
- uncaught `folly::FutureTimeout`: the corresponding exact exception marker,
  selected `TestBody` lambda frame/source-line pair, exact guest SIGABRT stderr
  marker, and the same authenticated safehermit 134/SIGABRT envelope.

Every class excludes certificate verification, certificate-not-yet-valid,
certificate-expiry/expired, sanitizer, Hermit, wrapper, dependency,
infrastructure, OOM, truncation, and outer-timeout failures.  Anything else is
conservatively CENSORED.

## Product epoch gap

The captured-NOW epoch is a harness-level workaround, not a Hermit source
change.  Current Hermit has no run-start `NOW` resolver that emits and persists
the resolved value for verify/replay.  Production behavior should sample host
UTC once before launch, emit the explicit RFC3339 value, persist it in an
immutable run/record manifest, and reuse exactly that value for all verify and
replay executions.  Continuous virtual-time progression must remain intact.
