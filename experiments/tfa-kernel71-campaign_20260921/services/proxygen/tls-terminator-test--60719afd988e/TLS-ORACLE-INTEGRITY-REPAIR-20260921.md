# TLS terminator oracle-integrity repair

The original schema-v2 record classified scheduler seeds 11 and 12 as the
predeclared `gtest-selected-test-failure` class.  That interpretation was not
supported by the retained bytes: all four strict replicas stop stdout after the
exact selected `[ RUN ]` line, with neither the required selected `[ FAILED ]`
line nor a one-failed-test summary.

The strict replicas instead terminate with an uncaught
`folly::AsyncSocketException`, `SIGABRT`, and the exact message `certificate is
not yet valid`.  Their virtual realtime is the old fixed
`2026-01-01T00:00:00Z` default.  Certificate/dependency validity was expressly
excluded by the target-owned class and cannot be retroactively relabelled.

The repair therefore changes only the derived interpretation:

- four execution results become `AMBIGUOUS` with no failure class;
- seed 11 and 12 samples become `CENSORED`;
- exact raw stdout, stderr, safehermit reports, strict matchers, plan, route,
  cell, and immutable receipt remain unchanged;
- one append-only exhausted `NOT-DETERMINABLE` outcome supersedes the old one.

No old event is imported into a future cell.  A prospective retry must pass a
literal realistic epoch, exclude all certificate validity/verification errors,
and predeclare any target-owned uncaught exception by exact message/stack/signal
before execution.
