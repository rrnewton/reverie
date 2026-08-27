# Hermit review and clustering evidence, 2026-08-27

This document preserves the evidence that would be expensive to reproduce from
temporary build logs. The public review comments and TaskGraph notes remain the
authoritative verdict records.

## Hermit pull request 2427

Pull request: https://github.com/rrnewton/hermit/pull/2427

Reviewed head: `d254c5e6babe27b65b0364c78aa39d04807b3684`

Public verdict: https://github.com/rrnewton/hermit/pull/2427#issuecomment-5439377770

The pressure-test and scorecard self-tests passed on the reviewed head. Two
independent mutations proved the checks could refuse:

```text
mutation: remove only the explicit Enforcer FS/EXEC/NET classifier clauses
result: exit 1
diagnostic: compatibility pressure test: schema-2 runner evidence collapsed its three output observations

mutation: accept sandbox-denied as CrashError
result: exit 2
diagnostic: compatibility scorecard: sandbox-denied pressure output was stored as product behavior
```

`scripts/validate.rs --self-test` passed in 2m55s with
`FINAL_VALIDATE_STATUS: PASSED`; `check-script-sigpipe.sh` checked all 20
Rust-script entrypoints; `check-portable-paths.sh` and `git diff --check` passed.

The required full validate did run at the exact head, but stopped before product
work. `pre.reverie_pin` failed both attempts because Hermit's then-current pin
`1393db6279331feb4af2533220e4786ab104a1b9` was not reachable from Reverie main
`ab07a89239150df3726a036bee9f5e897893dfc1`. Only `pre.submodules` and
`pre.reverie_pin` executed; 64 nodes were dependency-skipped, two `cpuid_fault`
nodes were host-inapplicable, `executed_tests` was unknown, and the final status
was `FAILED`. The pin has since moved, so this is historical evidence rather
than a current blocker.

## Hermit pull request 2733

Pull request: https://github.com/rrnewton/hermit/pull/2733

Reviewed head: `91442c38c97dd158604c1b1dcd294671038e8cf8`

Pinned Reverie: `ab07a89239150df3726a036bee9f5e897893dfc1`

Public verdict: https://github.com/rrnewton/hermit/pull/2733#issuecomment-5439765673

The composed behavior was checked at both boundaries. A Detcore overshoot
records the structural count. Hermit consumes that count after every
ptrace-family run and refuses an otherwise successful result.

Clean controls:

```text
cargo test -p hermit --lib
test result: ok. 210 passed; 0 failed

cargo test -p hermit-detcore --lib
test result: ok. 564 passed; 0 failed

cargo clippy -p hermit-detcore -p hermit --lib -- -D warnings
exit 0

cargo fmt --all -- --check
exit 0

# Run in a Reverie worktree at the pinned revision.
cargo test -p reverie-ptrace amd_epyc_9d85_uses_reduced_skid_margin --lib -- --nocapture
test timer::tests::amd_epyc_9d85_uses_reduced_skid_margin ... ok
```

Independent mutations:

```text
mutation: change Hermit's successful-result branch from Err(overshoot) to Ok(value)
test: tests::skid_overshoot_report_covers_success_error_and_disabled_backends
result: FAILED, exit 101
diagnostic: called Result::unwrap_err() on an Ok value: 7

mutation: remove only Detcore's reverie::record_skid_overshoot() call
test: rcb_overshoot_tests::default_overshoot_policy_emits_error_and_returns
result: FAILED, exit 101
diagnostic: the log-and-continue path must feed the supervisor's structural count; left 0, right 1

mutation: change AMD_EPYC_9D85_SKID_MARGIN from 1_000 to 3_141
test: timer::tests::amd_epyc_9d85_uses_reduced_skid_margin
result: FAILED, exit 101
diagnostic: left 3141, right 1000
```

The exact predicate remains `rcb_actual > rcb_target`. The affected AMD EPYC
9D85 margin remains 1,000; it was not widened to accommodate the observed 3,141
overshoot.

## Stress-series divergence points

Landed dev-hermit commit:
`2c4eec3a8193fbddd0c0434c26bb7cf1c544851c`

The Hermit producers are already landed through
https://github.com/rrnewton/hermit/pull/2748 and
https://github.com/rrnewton/hermit/pull/2751. They write both compared messages
and all four measured coordinates, and retain an earlier divergent attempt when
a retry passes.

The parent series writer now combines rows only when all stored evidence is
exactly equal apart from wall time and run position. It records `num_runs`, the
first and last observed run indices, all four exact coordinates, and both
compared messages. It does not store coordinate ranges or a distance threshold.

Verification:

```text
python3 -m unittest discover -s ci-hub/series/tests -p 'test_*.py'
Ran 72 tests
OK

200 interleaved observations at two exact divergence points plus passes:
3 rows, with counts 30, 50, and 120; serialized output below 10,000 bytes.

200 observations differing only in first_divergent_record:
200 rows; serialized output below 300,000 bytes.

mutation: remove coordinates from row equality
test: test_scattered_positions_remain_distinct_with_the_same_messages
result: FAILED; expected 200 rows, got 1

mutation: retain wall time in row equality
test: test_two_hundred_runs_keep_two_exact_divergence_points_and_counts
result: FAILED; expected 3 rows, got 200
```

Distance-based collapsing remains a future tunable reader policy if scatter
proves common. It is not part of the stored schema, so changing that policy will
not rewrite measured divergence points.
