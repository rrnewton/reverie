# Dagrun tier labels and oversubscription research

Date: 2026-09-03

Source revision: agent-utils `07394fb0b9e3328146b67f78cadc0aae193b796e`

## Verdict

The fastest observed median was 12.53 s at a sampled average load of 12.48 effective cores and a 13.535 GiB peak under the 14 GiB cap; aligned 6.75 GiB allocations succeeded, while aligned 7.0 GiB allocations reached the cap and triggered an OOM kill.

## Conclusions

1. Tier-shaped selection already exists. Commit `c1fdca7b5249cde7b04c0bdba693e0d351f224ff` added step `labels` and `dagrun run --labels`; `d18d94ffb317c1688e67cb7e90e70a7f5c53ec69` exposed the Rust selector. A `normal` label run can exclude a valid `nightly` node before planning and execution. It does not bypass validation of the complete input DAG.
2. `--profile-memory-feedback` does not put an average in `rss_baseline_bytes`. Each input is one run's cgroup-v2 `memory.peak`; the derived value is nearest-rank P90 of uncensored per-run peaks plus 20%, floored at the largest observed peak. Five uncensored samples are required.
3. Current CPU scheduling already permits the requested behavior: `--max-steps` controls overlap independently from `--max-cpus`, so two ready steps may each receive 16 workers under one 16-core outer CPU quota.
4. In the bounded two-step experiment, allowing overlap and giving both steps 16 workers reduced median wall time from 23.69 s under strict memory packing to 12.83 s. Tuning the phase offset and memory size at factor 0.9 produced a 12.53 s median, a 12.48-effective-core sampled average, and a 13.535 GiB outer peak under a 14 GiB cap. Repeating the same tuned workload at the owner's 25% setting (factor 0.75) produced 12.82 s; with only two runnable nodes and `--max-steps 2`, it necessarily admitted the same concurrency.
5. A blanket memory factor is unsafe. In single boundary trials, two aligned 6.75 GiB allocations succeeded at a 13.535 GiB outer peak; two aligned 7.0 GiB allocations reached the 14 GiB cap and the kernel killed the scope. This brackets the last observed success and first observed failure within 0.25 GiB per step for this synthetic workload; it is not a general threshold.

## What `rss_baseline_bytes` means

When readable, the runner writes one `peak_bytes` observation per step run from the step cgroup's `memory.peak` after descendants are quiescent and before cleanup (`rs/dagrun/src/cgroup.rs`, `peak_bytes`; `rs/dagrun/src/scheduler.rs`, end-of-step profile construction). An unavailable peak is retained as unknown rather than changed to zero. This is a whole-cgroup high-water value, not sampled average RSS.

`memory_admission_from_rows` in `rs/dagrun/src/memory_feedback.rs`:

- groups rows by step tag;
- excludes failed, timed-out, cap-reaching, or nonzero `memory.events.max`/`high`/`oom`/`oom_kill` observations from the central estimate; `memory.events.low` does not censor;
- requires at least five uncensored observations;
- takes nearest-rank P90, adds the default 20% margin, and floors at the largest uncensored or censored peak;
- for a known, non-skipped step without enough evidence, keeps the larger of the authored hint and the largest proven peak, without adding the 20% margin; an absent store row leaves the step untouched;
- never rewrites `hard_mem_max_bytes`.

Rows with a missing/unparseable peak, applied cap, or `memory_events_max` are unknown and contribute neither to the central estimate nor a floor. The resulting profile-derived baseline is then multiplied by `mem_cap_factor` in `rs/dagrun/src/sizing.rs`; its default is 1.25 unless a positive hard cap wins. When the percentile term dominates, the default modeled/enforced cap is approximately 1.20 x 1.25 = 1.50 times P90 before CPU-bound width scaling. An `rss_baseline_bytes=8 GiB` reported specifically as the flag's profile-derived estimate is already conservative; an arbitrary 8 GiB field may instead be authored, a proven floor, or ordinary planner feedback.

Two caveats matter for future design. The opt-in estimator pools all `inner_jobs` widths and all source revisions/workload digests within the machine/container store, then clears `rss_baseline_inner_jobs`; a CPU-bound step above width four can therefore scale that pooled value again unless a hard cap wins. Optional source-revision filtering exists in the library, but the CLI currently passes no revision. CPA's separate width-specific model normally retains width provenance, but combining CPA with `--profile-memory-feedback` overwrites/restores the scalar baseline and clears that exact-width provenance before runtime. Any future memory-overlap model should operate on width-specific evidence directly.

## Tier-label behavior

`Step.labels` is accepted and round-tripped by the JSON/YAML loader. `run --labels a,b` selects the OR-union of matching steps plus their complete transitive dependency ancestry. Unknown labels fail, and `--labels` cannot be combined with `--selected`. The filtered configuration is used by argument substitution, profile feedback, planning, memory sizing, per-step cgroup creation, and execution.

The full file is loaded and graph-validated before selection. Therefore a valid `nightly` node is quietly absent from a `--labels normal` run, but a malformed, cyclic, or missing-dependency nightly node still refuses the document. A live bounded check's retained journal records successful `tier.setup` and `tier.normal` steps and no `tier.nightly` event.

Label closure follows `deps`, not `explains`; diagnostic steps must be labeled intentionally. The option exists only on `run`, not the inspection, `plan`, or `sweep` commands. Labels are exact and case-sensitive; consumers should use simple comma-free strings because the loader permits whitespace/commas that the CLI request parser trims or splits.

There is no ordered tier type. Consumers should use the existing labels rather than add another mechanism, and explicitly choose one of these policies:

- disjoint labels, with callers requesting the union they want; or
- cumulative labels, where quick work carries `[quick, normal, nightly]`, normal-only work carries `[normal, nightly]`, and nightly-only work carries `[nightly]`.

The first policy makes membership clearer. Dependency closure already includes any unlabeled prerequisite, so prerequisites do not need every consumer label.

## Experimental method

Every result-table run used the Rust dagrun at the source revision above and passed:

```text
--cores 16 --max-cpus 16 --max-mem 15032385536 --max-steps 2
--run-timeout 180 --no-profile-feedback --profile-timeseries 250ms
```

Every result-table run's live outer cgroup readback proved `CPUQuota=1600%`, a final 16-CPU `cpuset.cpus.effective`, `memory.max=15032385536` (14 GiB), `memory.swap.max=0`, and `memory.oom.group=1`. No unboxed fallback was allowed. Logs were limited to 1 MiB per step and captures to 256 KiB. All retained experiment artifacts occupied 2.44 MB at completion.

Three preliminary harness smoke runs are excluded from the table. `smoke` used the documented `14Gi` spelling, which this `run --max-mem` parser did not accept, so it fell back to an 805.7 GiB ambient cap; its still-malformed synthetic command exited before doing work. `smoke2` used the exact byte count and proved the outer cap, but also exited before work because an inherited `jobs_flag` added two unintended arguments. The corrected `smoke3` proved all intended cgroup settings and passed before substantive runs began. Separately, an initial build-box attempt without elevated cgroup access refused before executing and was retried inside the required bounds.

The synthetic DAG had two independent steps. Each performed 32 billion fixed integer iterations and touched a fixed anonymous-memory allocation. One performed CPU then memory; the other memory then CPU. The ordinary workload touched 6.5 GiB per step and held it for five seconds. Each step kept an actual 7.5 GiB `hard_mem_max_bytes`; `outer_mem_safety_factor=1.0` made strict packing sum the two caps to 15 GiB and admit one step, while `0.9` modeled the pair as 13.5 GiB and admitted both without lowering either step's real cap. Eight versus sixteen workers changed only each step's requested inner width; outer CPU capacity stayed 16.

Average load below is the outer-cgroup CPU-usage delta divided by the matching interval between its first and last 100 ms samples, in effective cores. It omits roughly 100 ms at each process edge rather than mixing a sampled CPU numerator with full wrapper wall time. Peak memory is the exact outer-cgroup `memory.peak`, not a sum of step peaks. Paging is reported from the outer cgroup's `memory.swap.current`, `memory.swap.peak`, and `memory.stat pgmajfault`; `memory.swap.max=0` made swapping impossible, while major faults remained independently observable.

## Results

Main rows are medians of three sequential runs; ranges are shown in parentheses. The boundary rows are single runs.

| Setting | Active steps | Workers per step | Wall time | Average load (effective cores / 16) | Peak / 14 GiB cap | Paging | Outcome |
|---|---:|---:|---:|---:|---:|---|---|
| Strict packing, factor 1.0 | 1 | 8 | 32.55 s (32.53-32.59) | 4.74 / 29.6% (4.74-4.74) | 6.520 GiB / 46.6% (6.520-6.521) | 0 swap, 0 major faults | 3/3 pass |
| Strict packing, factor 1.0 | 1 | 16 | 23.69 s (23.58-23.79) | 6.55 / 41.0% (6.52-6.58) | 6.521 GiB / 46.6% (6.520-6.521) | 0 swap, 0 major faults | 3/3 pass |
| Overlap, factor 0.9 | 2 | 8 | 16.73 s (16.73-16.87) | 9.30 / 58.1% (9.21-9.31) | 6.525 GiB / 46.6% (6.524-6.525) | 0 swap, 0 major faults | 3/3 pass |
| Overlap, factor 0.9 | 2 | 16 | 12.83 s (12.83-12.95) | 12.20 / 76.3% (12.19-12.27) | 10.708 GiB / 76.5% (10.422-10.776) | 0 swap, 0 major faults | 3/3 pass |
| Tuned phase offset, factor 0.9 | 2 | 16 | 12.53 s (12.51-13.02) | 12.48 / 78.0% (12.01-12.49) | 13.535 GiB / 96.7% (13.535-13.538) | 0 swap, 0 major faults | 3/3 pass |
| Same tuned workload, factor 0.75 | 2 | 16 | 12.82 s (12.72-12.92) | 12.24 / 76.5% (12.14-12.31) | 13.535 GiB / 96.7% (13.534-13.535) | 0 swap, 0 major faults | 3/3 pass |
| Aligned peaks, 6.5 GiB each, factor 0.9 | 2 | 16 | 18.20 s | 8.58 / 53.6% | 13.033 GiB / 93.1% | 0 swap, 0 major faults | pass |
| Aligned peaks, 6.75 GiB each, factor 0.9 | 2 | 16 | 17.27 s | 9.01 / 56.3% | 13.535 GiB / 96.7% | 0 swap, 0 major faults | pass |
| Aligned peaks, 7.0 GiB each, factor 0.9 | 2 | 16 | 5.07 s to kill | 2.22 / 13.9% before kill | 14.000 GiB / 100% | 0 swap, 0 major faults | outer OOM: `oom=3`, `oom_kill=2`, process killed |

For this synthetic workload, the tuned setting answered the central question: it found the shortest observed median by allowing both independent steps to start, allowing each to use the full 16-worker width, and offsetting their CPU-heavy and memory-heavy phases so one used CPU while the other held memory. Compared with strict packing at 16 workers, median time fell 47.1% and sampled average load rose from 6.55 to 12.48 effective cores. Compared with strict packing at eight workers, time fell 61.5%. The 13.535 GiB peak used 96.7% of the cap without swap, major faults, outer `memory.max` events, or OOM. Representative 250 ms traces showed the active CPU phase sustaining approximately 15-16 effective cores.

## Interpretation and handoff

CPU overlap does not need a new scheduler feature. The present separation of `--max-steps` and `--max-cpus` already allows two nominally 16-worker steps inside one 16-core quota. The measured result supports retaining that behavior and selecting widths from measured scaling curves rather than partitioning 16 into fixed eight-core shares.

Memory overlap is different. A single global factor cannot distinguish the successful complementary phases from the failed aligned peaks: both have the same 7.5 GiB per-step hard caps and the same factor 0.9. The factor only decides admission; the outer cgroup supplies the safety boundary. In this two-node graph, once both steps were admitted, reducing the factor from 0.9 to 0.75 could not increase concurrency; the timing ranges overlap. The failure case confirms that neither 10% nor 25% reduction is generally safe merely because one staggered case succeeded. A graph with three or more simultaneously ready nodes is still required to measure whether factor 0.75 unlocks useful additional overlap.

These runs are workload-local evidence, not a production policy benchmark. They used one synthetic fork-and-anonymous-memory workload, conditions were grouped rather than randomized, the chosen 16-CPU sets differed between runs, and all 40 successful substantive step-profile rows classified ambient host load as `busy`. The tight within-setting ranges and cgroup-local counters make the comparisons useful, but a production choice should be repeated on representative Cargo/validation nodes.

Recommended production direction for the owner-designated scheduling lane:

1. Keep strict summed-cap packing as the default.
2. Reuse current labels for quick/normal/nightly selection; do not add a second tier mechanism. Add consumer-level tests for exact selected tags, dependency closure, and diagnostic-step membership.
3. If memory overlap becomes opt-in, base it on time-series overlap or explicit per-pair/group policy, not one global discount. Continue enforcing the undiscounted per-step cap and a hard outer `memory.max`; treat any OOM/reclaim observation as censored feedback, never as a usable peak.
4. Do not use pooled `rss_baseline_bytes` as if it were a mean. Either use CPA's width-specific memory observations or first fix the profile-memory-feedback provenance gaps (width, source revision, workload digest) and avoid compounding hidden margins.
5. Add whole-run telemetry to the product path. Per-step `peak_bytes` cannot recover the concurrent outer peak. The experiment had to sample the live outer cgroup to obtain the value that decides whether memory overlap is safe.

No production scheduler code was changed or landed. The raw workload, configurations, outer-cgroup samples, dagrun profiles, and logs remain workspace-local; this report preserves the source findings, method, aggregate results, and failure bracket needed for later review.
