# Concurrent validate cap from the measured knee

Task: `cap-concurrent-validates-at-6-measured-knee`

## Decision

Use this box-level full-validate admission envelope:

```text
per_validate = { jobs: 1, bytes: 2_147_483_648 }
box_cap     = { jobs: 6, bytes: 12_884_901_888 }
```

The count is six **live validate leases**, including the run being admitted. The
byte value is a 2 GiB admission reservation per warm full-profile validate, so
the pair must travel together; `6` alone is not a complete resource limit.

This is a throughput cap, not authority for a contended negative result. A
failure or no-result observed with another validate or incompatible box-heavy
work running must be confirmed under the solo envelope
`{ jobs: 1, bytes: 2_147_483_648 }` before it can block or diagnose a change.

## Evidence and provenance

No new validate was run for this analysis. It recomputes existing local
measurements.

The wall/CPU knee comes from the first 378 records of
`ignored/validate-run-ledger.jsonl`, whose SHA-256 is
`71a52e7895514f0deaead0d6173ea69dcb855ff8dd979e5b644f4f5b6924125b`.
That immutable prefix ends at `2026-08-04T16:47:11Z`. The selected population
is all 101 records satisfying `profile=full && result=pass`; failed and partial
runs are not silently treated as durations. For each selected run, the
concurrency proxy is the number of *other* validate ledger intervals that
overlap its interval, deduplicated by log file. This matches the existing
ledger derivation but cannot see non-validate Cargo or benchmark load.

| Other overlapping validates | n | Median wall (s) | Runs at or below 600 s | Median CPU (s) |
|---:|---:|---:|---:|---:|
| 0-3 | 47 | 492 | 36/47 (76.6%) | 1,275.6 |
| 4-6 | 24 | 535 | 17/24 (70.8%) | 1,258.8 |
| 7-9 | 5 | 697 | 0/5 (0%) | 3,043.6 |
| 10-13 | 9 | 500 | 6/9 (66.7%) | 1,382.8 |
| 14+ | 16 | 834 | 5/16 (31.3%) | 4,621.0 |

The owner-specified budget is a median wall time below 600 seconds, not a
promise that every run finishes within 600 seconds. The 4-6 band remains below
that budget at 535 seconds. The next band is 697 seconds, 30% slower, with CPU
time 2.42 times the 4-6 median. The 14+ band supplies the larger sample after
the knee: 834 seconds wall and 4,621 seconds CPU, respectively 1.56 and 3.67
times the 4-6 medians.

The 10-13 dip is not evidence for raising the cap. It is a small, noisy,
cache-confounded band between two degraded bands; all five 7-9 observations
miss the budget, while the 16-observation 14+ band shows both wall degradation
and manufactured CPU work. Choosing the last pre-knee band is therefore more
defensible than fitting a monotone curve through the dip.

The count proxy has a useful conservative off-by-one relationship to live
admission. It counts *peers*, whereas `{ jobs: 6, ... }` counts the admitted run
too. Six live leases expose a run to at most five peers, keeping it within the
measured pre-knee region rather than allowing six peers plus the subject run.

Memory comes from
`experiments/validate-resource-footprint_20260803/`: one isolated warm
full-profile validate at Hermit
`9ebe1608303c66bfaa4b9c7d0521a30d9519c182` on `devbig014` completed in
527.7 seconds and reached an RSS-sum proxy of 2,130,870,272 bytes (1.98 GiB).
Rounding that observation upward gives the per-job reservation of
2,147,483,648 bytes; multiplying by six gives 12,884,901,888 bytes (12 GiB).
This is an admission reservation derived from a warm RSS proxy, not a measured
cold-build peak or a proposed `memory.max` hard limit.

## Why this connects to `detcore_misc`

The concurrency measurements and the `detcore_misc` stress experiment are
independent but point in the same direction. The existing multisect evidence
shows solo/clean executions at 0-0.6% hang incidence and high ambient-load,
32-way stress at 16-23%, with the bad-pin mechanism spinning after notifier
`ESRCH` before `reservation.commit`. Separately, the validate ledger shows the
wall/CPU knee above six and observed fast false-red/no-result clusters in the
9-15 overlap regime. The evidence does **not** establish a controlled hang-rate
plateau exactly at six, so the cap is risk containment, not a proof that six
concurrent jobs cannot trigger the bug.

That distinction requires a two-level rule:

1. Up to `{ jobs: 6, bytes: 12_884_901_888 }` may be used for throughput.
2. A contended failure or no-result is `NeedsRerun`, not an authoritative red.
3. Negative authority requires a solo rerun with
   `{ jobs: 1, bytes: 2_147_483_648 }` (and the existing solo-confirmation
   settings, including `dag_jobs=4`).

This preserves useful parallelism without letting the known concurrency
failure mode manufacture a blocking verdict.

## Mechanical admission predicate

For a new full validate, admission should require both dimensions:

```text
live_validate_jobs + 1 <= 6
reserved_validate_bytes + 2_147_483_648 <= validate_memory_budget_bytes
```

The admission source must be live leases, not reconstructed ledger overlap.
Benchmarks and other known box-heavy jobs must share the admission class (or
reduce the available validate jobs), because the ledger proxy cannot observe
them. An implementation that enforces only the integer `6` drops the measured
memory condition and is incomplete.

The current durable `validate_lock.rs` remains an exclusive cap of one; this
document derives the bounded-throughput policy but does not silently replace
that lock. A future semaphore implementation must preserve lease cleanup and
the solo-negative authority rule.

## Re-measurement triggers

Recompute this cap rather than treating it as permanent after either the
Reverie notifier fix is on the pinned main pair or the one-fat-build validate
shape is deployed. Also re-measure for a materially different host, cold-build
mix, per-job memory footprint, or competing heavy-workload policy. Until then,
six is the largest supported live-job setting and one is the only supported
negative-confirmation setting.

## Implementation update, 2026-08-30

The admission controller now implements the two-dimensional envelope above.
It admits at most six live validates, reserves 2 GiB for each live validate,
and requires the resulting reservation to fit both the 12 GiB validation
budget and current Linux `MemAvailable`. A missing or malformed memory reading
is an explicit refusal; it cannot silently fall back to the count alone.

The implementation uses the live slot anchors as the reservation source under
the same box-global queue guard that assigns a slot. Benchmarks retain every
slot and therefore remain exclusive against the complete validation budget.
The qualifying-receipt predicate accepts at most five peers, matching six live
validates including the run producing the receipt.

A later five-run launch on `devbig014` sampled system memory every five seconds
throughout the overlap. Memory in use ranged from 109.08 to 125.34 GiB while
`MemAvailable` ranged from 901.71 to 885.45 GiB. The range demonstrates ample
headroom on that host; it does not replace the per-validate reservation or turn
the count into a machine-independent tuning constant.
