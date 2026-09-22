# HTTPCoro oracle-integrity repair

The seed 0 through 15 ledger is complete: it contains 16 attempted tuples and
32 strict-verification executions.  This repair concerns their interpretation,
not missing executions.

The prospectively sealed `gtest-selected-test-failure` class required both a
GoogleTest `[  FAILED  ]` line and a one-failed-test summary.  Every retained
stdout artifact instead ends after the selected test's `[ RUN ]` line.  Every
stderr artifact contains the selected test's fatal
`HTTPDownstreamCoroSessionTests.cpp:1685] Check failed:
bodyEvent.hasException()` and SIGABRT.  All 32 stdout files have SHA-256
`64701a7c4b67e570f3333c5c0da2fbdcd7a53921ed1c5c69e47750f1f5f78058`.

The fatal CHECK is target-owned behavior, but it was not a predeclared failure
class.  It therefore cannot be retrofitted into a formal FAIL arm.  The raw
stdout, stderr, safehermit reports, matcher artifacts, commands, sealed plan,
and plan receipt remain unchanged.  Only the derived event RESULT payloads and
record interpretations are corrected:

- each event becomes `target_oracle=AMBIGUOUS` with no failure class;
- each seed tuple becomes `CENSORED` with the exact oracle-mismatch reason;
- the strict `BitwiseInfoV1` matchers remain truthful evidence that the two
  executions repeated, but no longer make an undeclared outcome qualifying;
- a new append-only NOT-DETERMINABLE outcome supersedes the earlier pending
  classification.

The generic validator checks structural result consistency but cannot execute
free-text oracle predicates.  A continuation plan must declare the exact
line-1685 fatal-CHECK/SIGABRT class before any new HTTPCoro execution.
