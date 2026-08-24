# Dagrun profile-based step modeling: current design and proposed plan

**Task:** `plan-doc-dagrun-profile-modeling-current-and-proposed`
**Date:** 2026-08-24
**Status:** review and plan; no product behavior changed
**Bound sources:** dev-hermit `c4aa00b6761a5fddf0901e5767148d53328dd34e`, Hermit `31fecc48d88501591d2d9ce2a241b622ebaf0568`, Reverie `f2e9839e5316e31c377089ae7f789515ba9d2550`, agent-utils `e6d9d77064036ef16e83a5807d2e754303d3f98f`. Hermit fork `origin/main` at `ec82805424b32981cf37943e6885a69b2673b61a` was also checked and pins the same agent-utils revision.

This builds on `design-audit-dagrun-outer-and-inner-width` and does not duplicate
the separate `hermit-004` tactical GitHub-lane repair.

## Executive decision

The current profile system is **useful instrumentation with a thin decision
model and a mostly disconnected feedback loop**. Raw collection is substantial,
and the CPA planner contains a real measured speedup-curve prototype. It is not
yet authoritative enough to choose production widths because actuation,
profiling, identity, freshness, memory modeling, and live admission are not one
coherent contract.

The 32-versus-8 fat build is the decisive test. Dagrun can label a successful
row width 32 even when a launch path configures Cargo for 8; the current row
cannot attest the Cargo value because no field records final actuation. The DAG
declaration, cgroup allowance, tool environment, and profile label come from
different paths, so disagreement is structurally invisible.

The fix starts with truthful telemetry. One immutable resolved-resource record
must drive command/environment actuation, cgroups, scheduler admission, and the
profile row. Profiles then store width-specific observations with workload and
machine provenance, derive the scalability plateau, and feed a CPU-and-memory
allocator.

The owner's risk preference is asymmetric: avoid over-parallelization, but avoid
under-parallelization much more strongly. Therefore uncertain evidence must not
silently narrow moldable work to 4 or 8 workers. Without a trustworthy curve,
use the largest width supported by live CPU and memory capacity. If memory is
uncertain, reduce overlap before reducing the width of the one running build.

## Part I: current design and assessment

### What is collected and where

`hermit/agent-utils/py/safe_ci_dag_runner/perflog.py` defines append-only
whole-run and per-step CSV schemas. The per-step rows include timestamp,
machine/container identity, Git and profile-base SHAs, enforcement and runner,
step/classification, `inner_jobs`, elapsed/outcome fields, OOM and timeout
events, `peak_bytes`, thread peak, CPU enrichment, host/step pressure, overlap,
memory cap/events, and raw cgroup CPU counters (`perflog.py:99-150,330-378`).

Per-step files are named:

```text
step_profiles_<machine_id>_<container_class>.csv
```

`machine_id` is a normalized CPU-model string (`perflog.py:159-171`).
`container_class` is affinity count plus binding CPU quota
(`perflog.py:257-276`). This is a useful coarse partition, but it omits exact
placement, socket/NUMA topology, memory capacity/bandwidth, cache state, and
cpuset identity.

Several field names overstate the instrument:

| Field | Actual meaning |
| --- | --- |
| `inner_jobs` | declared/resolved width or boxed CPU count, not necessarily tool workers |
| `peak_bytes` / `rss_estimate_bytes` | cgroup `memory.peak`, including descendants, not literal process RSS (`cgroup.py:2108-2115`) |
| `outer_jobs` | active-step ceiling, not run CPU capacity |
| `effective_cores` | cgroup CPU-seconds divided by wall time, not a worker acknowledgement (`profile_enrich.py:98-119`) |
| `quota_utilization_pct` | `effective_cores / inner_jobs`; meaningful only when the width label is true |

### A real profile row

This audit found the following successful e6d-schema row in a local Hermit
worktree at `30be65f`. It is not a bound-current, committed, or durable artifact.

```text
timestamp=2026-08-24T09:04:46
git_sha=30be65f97d64eb04fb4f4e82d9c06c25d3ab1a1d
machine_id=AMD_EPYC_9D85_158-Core_Processor
container_class=affinity316_cpu-max-3200000_100000
outer_jobs=32
enforcement_kind=unverified
runner_name=local
step=build.runtime_release
classification=cpu-bound
inner_jobs=32
elapsed_s=91.792
peak_bytes=5922201600
thread_peak=194
effective_cores=12.0087
quota_utilization_pct=37.53
memory_max_bytes=68719476736
```

The artifact was under
`worktrees/mainval-f972/hermit/ignored/ci/perf/run-node/portable/step_profiles_AMD_EPYC_9D85_158-Core_Processor_affinity316_cpu-max-3200000_100000.csv`;
its full-file SHA-256 at review time was
`a1179887af265e40f43667e86e77c05f580ba2bc885798a8f5f982420e399378`.
At that source SHA, the matching `run-node.sh` code would export
`CARGO_BUILD_JOBS=8` and the DAG declared `preferred_inner_jobs=32` with empty
`jobs_flag`, but the row itself cannot prove which entrypoint or environment
produced it. `effective_cores=12.0087` cannot identify Cargo's configured pool
because it covers the complete cgroup, including build scripts and native children.

### How rows become a model

The ordinary estimator reduces a rich row to six values: elapsed time,
contention, CPU time, effective cores, throttled time, and peak bytes
(`estimates.py:137-165,378-417`). It buckets by `(step, inner_jobs)` after
outcome filtering (`estimates.py:435-488`). Timestamp, source/base SHA,
enforcement, runner, command identity, controller, cap events, sibling set, and
most ambient data do not participate in compatibility selection.

Two paths consume those buckets:

- Ordinary duration and memory estimation collapses all widths into one step
  estimate (`estimates.py:491-518`) and accepts one sample by default
  (`estimates.py:69-77`).
- The speedup model retains width buckets and derives a knee while marginal wall
  gain is at least 1.15 and CPU-work growth at most 1.5. It requires only two
  distinct widths (`estimates.py:541-556,682-831`).

Summary v1 keeps bounded reservoirs per `(step, inner_jobs)` but serializes only
the reduced six-value sample (`summary.py:46-60,111-124,253-290`). It drops
recorded revision, timestamp, enforcement, and censoring fields; controller and
command identity were never collected. A synced summary cannot later prove
freshness or actuation.

### What consumes it

| Consumer | Current behavior | Assessment |
| --- | --- | --- |
| Default planner | Uses learned duration/memory when present | Useful for ordering; identity is too weak for authority |
| CPA | Opt-in live planner whose chosen order and runner-controlled widths are applied; it also computes a no-overcommit reference schedule (`estimates.py:1115-1494,1565-1578`) | Substantive prototype, but its reference schedule is not live dispatch |
| `apply_plan_to_config` | Copies estimates/CPA width back into `ResourceHint` (`estimates.py:1565-1578`) | Mixes authored work, learned evidence, and runtime result |
| Scheduler | Re-derives command width, CPU cap, memory, and profile width (`scheduler.py:1083-1139`) | Does not consume one resolved contract |
| Cgroup preparation | Independently derives or preserves `CARGO_BUILD_JOBS` (`cgroup.py:1974-2005`; `sizing.py:384-472`) | Can disagree with the profile label |
| Memory sizing | Collapses widths, assumes a width-4 base, and lets hard caps win (`sizing.py:58-129`) | Not width-valid |
| Censoring-aware memory feedback | Requires five uncensored samples (`memory_feedback.py:64-70`) | Better foundation, but opt-in; CLI does not pass the available SHA filter (`cli.py:1028`) |
| `analyze.py` | Compatibility summarizer | Reads `memory_peak_bytes` while writer emits `peak_bytes`, silently blanking its peak result (`analyze.py:18-33,63-82`) |

CPA is an opt-in live planner whose chosen order and runner-controlled widths are applied. Its
no-overcommit makespan is only a reference model: live dispatch still checks
dependencies, `max_steps`, and named resources rather than summed CPU or memory
tokens (`estimates.py:1322-1336`; `scheduler.py:503-516,912-932`).

### Why 32 versus 8 was not caught

1. The two fat builds declare `preferred_inner_jobs=32` and `jobs_flag=""`
   (`hermit/ci/dag/portable.json:77-98`).
2. `ci/run-dag.sh` and `ci/run-node.sh` source launcher mode, which defaults and
   exports `CARGO_BUILD_JOBS=8`
   (`hermit/ci/configure-build-jobs.sh:17-18,49-53`).
3. Dagrun preserves ambient Cargo width as operator intent, while empty
   `jobs_flag` prevents argv rewriting (`sizing.py:384-472`).
4. The scheduler stamps the profile from `preferred_inner_jobs` and calls it the
   actual width (`scheduler.py:1133-1139`).

No row records requested tool width, configured tool width, actuator kind,
final env/argv, or acknowledgement. There is nothing to compare.

The same declaration has three entrypoint-dependent outcomes:

| Entrypoint | Outcome for a fat build declared at 32 |
| --- | --- |
| sufficiently wide, boxed `ci/run-dag.sh` | Cargo 8; cgroup/profile 32 |
| direct `scripts/validate.rs` without ambient override | Cargo 32; profile 32 |
| default `ci/run-node.sh` | `-j 1` means `--max-cpus 1`; fixed-width validation refuses before spawn |

The row may therefore be consistent, mislabeled, or absent depending on the
entrypoint. The work does not have one resolved resource meaning.

### Memory quality

At the coordinated Hermit pin, all 61 production nodes—52 portable and 9
privileged—declare positive `hard_mem_max_bytes`; all 11 preferred-width nodes
use empty `jobs_flag`. Explicit hard caps override learned baselines
(`sizing.py:65-77`), so learned memory cannot change current cap selection,
admission, or enforcement.

Separately, memory peaks from all widths become one percentile, then CPU-bound
sizing interprets that width-agnostic scalar as a width-4 base and scales
`cap * inner_jobs / 4` (`estimates.py:491-518`; `sizing.py:58-129`). The result
may correspond to no observed width. The fat-build 64-GiB values are explicitly
generous containment ceilings, while their descriptions cite only one measured
width-32 peak. The historical width-32 curve in
`ai_docs/cargo-build-parallelism-derived-speedup-knee-20260805.md` belongs to a
different, older workload and explicitly requires remeasurement.

### Hermit and Reverie do not close the loop

Hermit hosted shards write and upload `run-node-perf-*` artifacts, but no
workflow restores them through `--profile-sync`
(`hermit/.github/workflows/ci-portable.yml:289-300,357-368`;
`agent-utils/py/safe_ci_dag_runner/cli.py:529-545`). Direct
`scripts/validate.rs` writes rows only when `RUN_NODE_PERF_DIR` is present and
marks them `enforcement_kind="unverified"`. The important width-declared steps
cannot be swept because all 11 are self-managed/empty-flag.

Current coordinated Reverie validation has correct split outer controls but the
wrong modeling unit. `ci-hub/validate/reverie_safe_ci.py` wraps the complete run
as one step declared, cgroup-capped, and profiled at width 8, with
`--max-steps 1`, `--max-cpus 8`, a 16-GiB baseline, a 32-GiB hard cap, and
`--no-profile-feedback` (`reverie_safe_ci.py:39-86`). An ambient operator
`CARGO_BUILD_JOBS` may still change the actual Cargo width, which is not acknowledged.
That step contains six
serial gates—skill discovery, workspace build, regular tests, doc tests,
Clippy, and rustfmt (`reverie/validate.sh:268-290,360-366`). One aggregate
`T(p)` or `M(p)` curve cannot represent six commands with different scaling and
cache behavior. Its explicit profile directory is not read by the parent
history aggregator.

### Honest grade

| Dimension | Grade |
| --- | --- |
| Raw collection | Good foundation |
| Naming and measurement semantics | Thin |
| Work identity and freshness | Nominal |
| Width actuation truth | Not production-safe |
| Speedup modeling | Promising prototype |
| Memory modeling | Not width-valid |
| Scheduler integration | Disconnected |
| CI learning loop | Mostly output-only |

Overall: **thin, not fake**. Dagrun has enough instrumentation and algorithmic
scaffolding to evolve rather than restart. Until actuation, identity, freshness,
and admission are unified, profile values are diagnostics and planner
experiments rather than trusted production allocations.

## Part II-A: proposed improvements to dagrun

### One immutable resolution

Add a pre-scheduler resolver:

```text
resolve_run(WorkSpec, MachineProfile, RunPolicy, live_limits, overrides)
    -> ResolvedRun[ResolvedStepResources]
```

Each resolved step carries workload digest; controller kind/version; authored
minimum/semantic maximum; profile plateau and contributing evidence; requested
and resolved width; CPU admission charge and child `cpu.max`; width-specific
memory reservation and hard `memory.max`; duration; source, confidence,
freshness, and fallback reason.

The planner, scheduler, command renderer, environment builder, and cgroup manager
consume this immutable value. A separate post-actuation result records the final
configured/acknowledged tool width and cgroup read-back; telemetry links both by
receipt ID. Stop mutating `ResourceHint` and stop independently recomputing width.

### Separate the four ownership layers

**WorkSpec** keeps command, dependencies, semantic classification, logical
resources, and a typed parallelism contract: `serial`, `moldable`, `fixed`, or
`opaque`. A moldable controller names an argv or environment adapter, not a
host-tuned count. `fixed` is explicit and exceptional; `opaque` cannot produce
width-curve evidence.

**MachineProfile v2** is the only persisted tuning layer. Static seed and
dynamic profiles share one schema keyed by workload digest, machine class,
enforcement/state class, and a named controller pool plus its contract version
and exact configured width. This supports distinct Cargo, native-build, and
test-execution widths. The profile stores raw observations, dispersion,
censoring, provenance, and derived values that cite contributing observation IDs.

**RunPolicy** owns machine-independent safety defaults, headroom/floor,
`max_active_steps` policy, explicit overcommit policy, and ephemeral operator
ceilings. A shared host may assign a protected slice, but once that slice is the
run capacity the allocator should fill it; do not apply a second silent 90%
modeling haircut.

**ResolvedRun** combines the selected profile, live cgroup/affinity/memory
limits, shared-host assignment, and operator ceilings. Authored data is never
rewritten to look like this runtime decision.

### Profile v2 and the measured plateau

Key observations by:

```text
(workload_digest,
 machine_class,
 enforcement_class,
 state_class,
 controller_pool_name,
 controller_contract_version,
 configured_tool_width)
```

The workload digest covers normalized command, relevant environment,
dependency/input identity, lockfile, toolchain/backend/image, and adapter
version. Machine class covers CPU/microarchitecture, topology/NUMA, actual
cpuset/placement, memory class, and binding quotas.

Record observation ID, receipt digest, timestamp/source SHAs, schema versions,
per-pool requested/resolved/configured widths, applied/read-back limits, wall/CPU
time, effective cores, throttling, threads, cgroup memory peak/events, censoring,
active siblings, and aggregate reservations.

`plateau_jobs` is derived, never handwritten. Initially retain the existing
1.15 marginal-gain and 1.5 CPU-work-growth rules for compatibility, but require
at least three valid repetitions per compared width for promotion. Randomize
order, separate cold/warm state, exclude failed timing samples, preserve OOM
and cap hits as censored memory evidence, and collect more data when confidence
crosses a decision boundary.

Use geometric widths (`1,2,4,8,...`) plus one or two refinement points around
the knee. Exhaustive 1..32 at three repetitions is 96 full runs per step and
1,056 runs across the 11 currently width-declared Hermit steps. Prioritize
critical expensive work, reuse passive observations, and resweep only changed,
stale, or low-confidence identities.

Only fresh compatible evidence may narrow. Sparse, old, cross-hardware, or
controller-mismatched observations may inform display or priors but cannot
reduce execution width.

### Width-specific memory and token admission

Derive memory distributions per exact configured tool width. Keep OOM/cap-hit
samples as censored lower bounds. Separate:

- `memory_reservation_bytes`, the learned admission charge; and
- `memory_limit_bytes`, the policy/enforcement ceiling.

Resolve live CPU and memory budgets `P` and `M` from binding live limits,
assigned host capacity, and explicit operator ceilings. Keep
`max_active_steps` independent. For each launch require:

```text
sum(running CPU charges) + candidate CPU charge <= P * overcommit_ratio
sum(running memory reservations) + candidate reservation <= M
```

This is an intentional behavioral change from current live CPU overcommit, not
merely activation of the CPA reference schedule. Initially charge CPU-bound work
its allocated width and default `overcommit_ratio` to 1.0; later measured policy
may discount latency-bound demand using co-run evidence. The allocator remains
work-conserving: one ready expensive build gets all feasible capacity rather
than fixed j4/j8. If memory is unknown, reserve the lesser of an explicit hard
limit and all currently free `M`; if no hard limit exists, reserve all currently
free `M`. Admit at most one unknown-memory step and run it at the largest
CPU-feasible width. This conservative reservation makes "memory-feasible"
defined while favoring width over overlap. Cgroups remain hard backstops, not
the allocator.

### Truthful telemetry

Every row references both the immutable pre-launch resolution receipt and the
post-actuation result. Together they record:

- authored/requested and profile-recommended widths;
- resolved width and its source;
- final argv/env controller mutation and configured/acknowledged tool width;
- requested/read-back CPU and memory limits;
- CPU/memory reservations and active sibling totals;
- achieved cores, thread peak, throttling, and cgroup memory peak; and
- enforcement, source, confidence, freshness, and fallback reason.

Configured width is not achieved parallelism. If the adapter does not
acknowledge configuration, record unknown; never substitute a quota, affinity,
or DAG hint and call it actual width.

### Eventual deletions

After a compatibility window, remove authored `preferred_inner_jobs`,
`jobs_flag`, `default_jobs_flag`, DAG `rss_baseline_bytes`, the width-4 memory
heuristic, mutable `apply_plan_to_config`, global launcher
`CARGO_BUILD_JOBS=8`, and non-consumed measured fields in `ResourceHint`. Rename
a retained semantic hard cap to `memory_limit_bytes`. Fix or retire
`analyze.py`; retire ambiguous `--jobs`/`cpu_jobs` aliases; rename `outer_jobs`
to `max_active_steps` and record `run_cpu_budget` separately. Profile v1 remains
diagnostics-only and may not narrow widths or reservations.

## Part II-B: how Hermit and Reverie validation should use it

### Repository-owned width contract

Dagrun sets one neutral value such as:

```text
DAGRUN_RESOLVED_INNER_JOBS=<N>
```

Repositories translate it once. Cargo exports `CARGO_BUILD_JOBS=N`; nextest and
libtest receive their real flags; native builds report a separately named pool
when correctness caps differ. Each adapter acknowledges its configured value
and internal clamp. Moldable steps receive the largest feasible resolution;
only a proven fixed-width contract violation refuses.

### Hermit migration

This convergence depends on the separate `hermit-004` repair making the hosted
single-node path express one active step without imposing a one-CPU inner ceiling.
This plan consumes that result and does not duplicate its tactical implementation.

1. Make `ci/configure-build-jobs.sh` an adapter, not a scheduling authority.
   Preserve a temporary direct-invocation fallback, but stop global 8 from
   overriding a resolved per-step width.
2. Convert width-sensitive wrappers, including
   `ci/run-with-reverie-dbt-budget.sh`, to consume the neutral contract and
   acknowledge Cargo and independently clamped native widths.
3. Replace all 11 empty-flag declarations with typed controllers and remove
   literal width fragments from commands.
4. Update manifest validation, `audit_budget_ordering`, and exact-command tests
   atomically so the repository proves there is one actuator.
5. Route `ci/run-dag.sh`, `ci/run-node.sh`, and `scripts/validate.rs` through the
   same resolver. Separate `max_active_steps` from `run_cpu_budget`; `--only`
   must not force inner width to one.
6. Give canonical validation a durable profile store. Hosted jobs restore a
   compatible summary before planning and publish afterward; local validation
   emits the same applied-plan receipt.
7. Re-measure `build.workspace` and `build.runtime_release` at their current
   workload fingerprints. Until then, do not silently fall back to 8; use the
   largest CPU-feasible width and the conservative unknown-memory reservation
   above, and label both fallback decisions.

### Reverie migration

1. Commit a Reverie-owned validation DAG with one node per current gate; retain
   `validate.sh` as a thin compatibility entrypoint. There must be one scheduler:
   dev-hermit invokes the product DAG directly rather than wrapping a nested
   dagrun inside `validate.reverie-full`.
2. Model skill discovery, build, regular tests, doc tests, Clippy, and rustfmt
   independently. Keep an aggregate whole-run record only for accounting; it is
   ineligible for plateau or per-gate memory learning.
3. Give each Cargo gate the neutral adapter. Keep regular-test
   `--test-threads=1` as a separately named test-execution policy, not Cargo
   compile width.
4. Preserve native DBT `clamp(parsed NUM_JOBS, 1, 16)` as a distinct native-build
   ceiling (`reverie/reverie-dbt/build.rs:572-595`) and acknowledge its value.
5. Keep parent ownership of checkout identity, locks, KVM requirements,
   exact-head receipts, and merge policy around that one scheduler boundary.
   Connect per-gate profile output to the durable store.
6. Sweep material gates only after decomposition. A whole-suite sweep would
   produce a false aggregate plateau.

## Independently valuable rollout

### Phase 0 — resolution receipt, no behavior change

Introduce immutable resolution while reproducing current choices. Make
scheduler, cgroups, command/env rendering, and writer consume it.

**Verify:** Python/Rust golden receipts match; final argv/env and cgroup readback
match the receipt; a declared-32/environment-8 fixture reports mismatch.

### Phase 1 — profile v2 dual-write

Add workload/machine/controller identities, observation IDs, provenance and
censoring to raw and summary forms; deduplicate merges; fix the analyzer column.

**Verify:** stale command, changed lockfile, duplicate merge, unboxed,
OOM-censored, and cross-machine fixtures. V1 and v2 write together; only v2 may
narrow.

### Phase 2 — typed controllers and Hermit convergence

After the separate `hermit-004` prerequisite lands, add neutral and Cargo/native
adapters; migrate all Hermit entrypoints and width declarations under shadow
comparison.

**Verify:** the same node resolves to identical tool width, caps, and profile
label through `run-dag.sh`, `run-node.sh`, and `scripts/validate.rs`.

### Phase 3 — Reverie gate decomposition

Commit six product-owned DAG nodes, preferably, or an equivalent structured
child-gate protocol.

**Verify:** first retain sequential gate execution and require aggregate wall to
match the interval union plus wrapper overhead, with per-gate CPU totals accounted
separately. If gates later overlap, reconcile by interval union rather than sum;
aggregate rows remain rejected as curve evidence.

### Phase 4 — learn in shadow mode

Run geometric repeated sweeps, derive plateau and width-specific memory, and
emit counterfactual schedules without changing execution.

**Verify:** source/controller changes invalidate; uncertainty never narrows;
promoted knees reproduce within the chosen confidence policy.

### Phase 5 — enforce resource allocation

Enable memory-token admission, then replace implicit live CPU overcommit with
explicit CPU-token allocation and an operator-visible overcommit ratio. Preserve
rollback and outer cgroup containment. Compare makespan, idle CPU, throttling,
OOM/cap events, and false refusals with legacy behavior.

**Verify:** deterministic scheduler fixtures, Python/Rust differential, live
cgroup readback, and representative CI. Promotion requires no systematic
throughput loss.

### Phase 6 — remove legacy surfaces

Stop reading v1 for decisions and remove old fields, aliases, global width
literals, width-4 scaling, and aggregate-only Reverie modeling after telemetry
shows no legacy consumers.

## Explicit uncertainties

1. Three repetitions per width is a recommendation; the promotion interval and
   noise model need calibration.
2. Workload digest invalidation is mandatory, but time-based expiry for compiler,
   kernel, and hardware drift remains open.
3. The portability boundary across same-model hosts is unproven; topology,
   cpuset, NUMA, bandwidth, and state may require distinct classes.
4. Cargo, native CMake/make, and test execution can be separate pools. The first
   schema must support named pools or explicitly limit itself to one primary
   controller.
5. The initial unknown-memory reservation is uncalibrated. The policy is to
   prefer one wide uncertain step over many narrow ones; the byte estimate needs
   measurement.
6. Latency-bound CPU discounts require sibling/co-run evidence. Default
   overcommit remains 1.0 until then.
7. A six-node Reverie DAG is preferred. A structured child adapter is acceptable
   only if it produces equivalent independent receipts and cgroup measurements.
8. The real rows cited here are ignored local artifacts. They prove schema
   behavior, not a reusable current profile.
9. The non-inferiority tolerance, sample count, and confidence interval for the
   "no systematic throughput loss" rollout gate remain to be defined before
   enforcement can be promoted.

## Conclusion

Dagrun already has the beginnings of the desired system: rich raw cgroup
telemetry, width buckets, a knee rule, and CPA. The pieces are not yet joined
tightly enough to trust: the model can fit a curve to the wrong width, forget
why a sample was compatible, and leave the result unused by production
entrypoints.

Do not hand-author a better `preferred_inner_jobs`. Make the plateau a
provenance-bearing derived work property, resolve one runtime allocation from
that evidence and live capacity, and make every actuator and measurement attest
to the same decision. When evidence is weak, fill the assigned machine with
controlled work and reduce overlap before silently shrinking a compile. That
directly addresses the historical failure mode: drastic under-parallelization
on very large hosts.
