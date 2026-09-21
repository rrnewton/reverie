# Thrift Instrumentation scheduler-seed plan

This cell is materially different from the exhausted target-races cell:
`--chaos-target-races` is absent, seed, RNG seed, and fuzz seed are fixed at
zero, and only `--sched-seed` varies in the explicit order 0 through 35. Random
scheduling and the 200,000,000-RCB maximum timeslice remain fixed.

The explicit epoch `2026-09-21T20:56:38Z` was captured at this planning
boundary and is replayed literally with normal clock progression. The 36
attempt cap provides seven slots of slack beyond the 29-valid-tuple requirement
for a prospective 10% minimum detectable FAIL rate and 5% maximum miss rate.

The CPU-0 native diagnostic's three PASS executions and exact line-836 failure,
and the separate target-races continuation's 29 PASS tuples plus one censored
attempt, are planning context only. They are not imported evidence and do not
assert rate equivalence to this scheduler-seed population.

PASS requires the exact selected OK line, one-pass summary, exit zero, and
strict BitwiseInfoV1 parity. The sole target-owned FAIL is the exact contiguous
`InstrumentationTest.cpp:836` block in which `reqSnapshots.size()` is two
rather than three after the selected RUN line. All other sanitizer-only,
loader, wrapper, infrastructure, timeout, truncation, or unmatched results are
censored. The inherited live stdout cap, helper-blob receipt, qgroup, memory,
swap, wall, stderr, and disk gates remain mandatory.
