# Validate concurrency characterization on the 316-core box

Date: 2026-08-24

Trust Gate: PASS — 15/15 findings independently verified; no stale, unverified, or hallucinated findings.

## Conclusion

The global validate lock is not a correctness boundary. It is a per-UID, host-wide flock shared by cooperating `validate`, Reverie-validate, and benchmark callers across clones; repository lease and cleanup records remain repository-local. It does not exclude ordinary agents or arbitrary host filesystem activity. [F-1] [F-2]

Concurrency should therefore be treated as an instrument that increases the chance of observing latent nondeterminism. The evidence currently identifies three distinct flake mechanisms:

1. **Host-visible directory metadata enters strict traces.** Verified cells run with the checkout root as their cwd, while Hermit overlays only `/tmp`; the checkout and its ancestors remain visible. A directory entry being created or removed can change `st_size` between the two verify executions and produce a canonical-log divergence even though both guest executions exit zero. [F-3]
2. **PMU skid has a rare heavy tail.** One full validate with a peer active failed `backend-parity-c/getrusage-self-accounting` after `HERMIT_SKID_OVERSHOOT ... overshoot=1169` exceeded the fixed 1000-instruction margin. The same cell passed at the same SHA in another overlapping validate. [F-6]
3. **Resource contention can starve subprocesses.** A previous exact-SHA six-run investigation recorded a scorecard Cargo subprocess consuming only 0.050 CPU seconds over 93.813 wall seconds while another checkout rebuilt dependencies. That is a resource-sharing failure, distinct from trace contamination.

The existing evidence does **not** measure the failure rate of controlled concurrent full validates. The earlier `--jobs 24` experiment increased cell concurrency inside one harness and one worktree; it was not a C=2 full-validate experiment. [F-5] Only four retry-aware full ledger rows currently show one peer validate, and they span three SHAs and uncontrolled conditions. They establish examples, not a treatment effect.

No lock change is made or recommended from this evidence alone. The next useful step is a matched, bounded C=1/C=2 characterization after the serial exact-main baseline and resource isolation preconditions are repaired.

## What the lock actually serializes

The live exclusion anchor is derived from the Unix UID, so linked worktrees and independent clones owned by the same user contend on one kernel flock. The priority queue and claim sidecars are also host-global. The human-readable lease, owner, and cleanup files are rooted in each repository's main worktree. [F-1]

All production validate launchers converge on `ci-hub validate-run`, which invokes `validate-lock run` and holds the anchor across child execution, residual-process census, and lease release. The participating job kinds are validate, Reverie-validate, and benchmark. Ordinary build agents, system services, and unrelated filesystem writers do not participate. [F-2]

Fresh worktrees do isolate each validate's checkout descendants, `target/`, ordinary artifact tree, and normally its Cargo home. They do not isolate `/home/newton`, `/home/newton/work`, the parent workspace, or the repository's common Git metadata. More importantly, every manifest cell starts in the host checkout root, and Hermit makes host files outside its private `/tmp` visible to the guest. [F-3]

That means the lock can reduce load and remove one class of cooperating writer, but cannot make the strict-verification input stable. A serial exact-main run demonstrated this directly: with `Peer validates: 0`, `applications/git-repository-workflow` diverged because `newfstatat("/home/newton", ...)` returned directory sizes 5788 and 5810 in its two executions. [F-15]

## Previous write-ups, re-read

### `jobs-24-manufactures-six-spurious-reds-per-sweep`

The controlled system-utils experiment remains strong evidence that concurrency exposes failures:

- `--jobs 1`: 0 of 29 failed in each of two runs, taking 470 and 471 seconds.
- `--jobs 24`: 6 of 29 and 8 of 29 failed, taking 34 and 37 seconds.
- Three 305-cell width-24 sweeps reported 5, 7, and 9 real failures.

The old interpretation was incomplete. Inspection of the preserved captures found that 28 of the 31 divergence instances across the five width-24 runs differed first on `newfstatat` metadata for the shared checkout cwd; the remaining three were the already-stable `node-v8-jit` failure. A representative difference is `newfstatat(".", ...)` with `st_size=1008` versus `946`. Thus concurrency selected a rotating victim set, but the proximate wrong answer was shared mutable directory metadata, not CPU scarcity alone. [F-4]

This study used one runner and one worktree. It characterizes intra-validate cell concurrency and cannot be used as a measured cross-validate C=2 result. [F-5]

### `skid-margin-under-load_20260803`

The PMU study on this AMD EPYC box found a heavy-tailed retired-conditional-branch skid distribution. Its p99 stayed below the fixed margin of 1000, while maxima reached 15,244 and 33,500. It recommended retrying only when the failed run emits a positive `HERMIT_SKID_OVERSHOOT` marker, recording every retry, and capping retries at two or three.

The observed cross-validate `getrusage-self-accounting` failure is exactly this known signature: target 19,945,122, actual 19,946,291, overshoot 1,169. It is a positively identified retryable flake, but one event does not prove that peer load caused the tail event; the earlier distribution also had outliers at baseline. [F-6]

### `flakiness-investigation-measure-the-rate-and-find-the-mechanism`

At exact SHA `485a0ad4`, five of six full validates passed. The failure was `scorecard.compatibility`; its nested Cargo process spent 93.813 seconds wall time but only 0.050 seconds of CPU while another checkout rebuilt dependencies. All 1,632 E2E results passed. This is the cleanest prior example of resource contention manufacturing a top-level red.

### `design-the-c2-concurrency-experiment-the-one-unmeasured-lever`

That note correctly states that a controlled, resource-bounded C=2 full-validate trial remained unmeasured. Its matched C=1 -> simultaneous C=2 pair -> C=1 blocks, exact SHA, fresh worktrees, and first-attempt evidence are still the right basic design. The current host does not yet meet its preconditions. [F-14]

## Actual cross-validate observations

There are four unique full ledger rows in the retry-aware schema with `concurrent_validates=1`. All used `dag_jobs=16`; all have retry count zero.

| SHA | Result | Observation |
|---|---|---|
| `5ad34aad` | pass | 1,194 tests passed |
| `5ad34aad` | fail | `getrusage-self-accounting` PMU-skid overshoot 1169; no timeout |
| `5f34dba3` | fail | deterministic Clippy unused-import error |
| `8adf6198` | pass | 1,203 tests passed |

The same-SHA pass/fail pair isolates a genuine flaky outcome seen during overlap. The inner failing result is preserved at:

`worktrees/dap/hermit/ignored/validate/e2e/validate-full-5ad34aad32af-20260824T042235Z/local-1787546226-2571762/results.jsonl`

The sample is too small and uncontrolled to estimate a C=2 rate. `concurrent_validates=1` proves overlap, but not identical peer load, synchronized start, or a matched serial control.

## Retry-count-per-validate

The exact available metric is:

```text
retry_count_per_validate = sum(gate.retries for gate in gates)
```

Each gate retains all outer attempts plus `first_attempt_result` and `first_attempt_reason`; summing `gates[].retries` counts repeated DAG-node executions without losing a second retry on the same node. [F-7] Do not use `env_block_retries`: it counts retry rounds and can undercount when several nodes retry in one round.

Current baseline:

- 34 local ledger rows carry the new attempt schema.
- Aggregate `retry_count_per_validate`: **0**.
- Rows with a nonzero retry count: **0**.

This zero is not evidence that the flake rate is zero. The retry policy currently recognizes environmental signatures, exact wall/CPU budget kills, and registry membership. The registry contains only `command_strict_verify`. Ordinary strict-verify divergence and the PMU-skid failure are therefore ineligible, and the PMU marker is present only in the inner cell result rather than the outer node detail examined by the retry classifier. [F-8]

The current metric is exact at the mechanism's real unit, the outer DAG node, but not at individual-test granularity. Retrying `e2e.manifest_system_utils` once increments the metric by one even though its whole category reruns. A future cell-level metric requires binding each retained E2E run/result to its outer gate attempt ordinal.

A retry-recovered terminal pass currently still qualifies as green: receipt qualification checks the terminal result and failures, but not retry count or first-attempt result. [F-9] Reporting should therefore distinguish:

- clean first-attempt pass;
- pass recovered by one or more retries;
- terminal failure;
- missing retry data in legacy rows.

Missing legacy retry fields must remain unknown, not be coerced to zero.

## Why serial validation is so slow

The latest clean full pass took 1,786.474 seconds at `-j16` while averaging only 4.7 busy cores across the effective 284-core slice. [F-10] The scheduler was not leaving meaningful parallelism unused under its declared constraints: `hermit_guest=1` serialized 18 nodes totaling 1,273.152 seconds after a 510.364-second prefix, producing a measured lower bound of 1,783.516 seconds. That is within 2.958 seconds of the observed run. [F-11]

Raising outer scheduler width alone cannot produce the ten-minute goal. Even with resource caps removed and infinite outer width, the measured dependency-only critical path is 1,068.835 seconds. Its two dominant nodes are the 398.663-second manifest gate and the 496.418-second system-utils bucket. [F-12]

Fleet concurrency and node width also need one admission model. One observed validate selected up to 75 CPU-width units concurrently. Ten synchronized validates could request roughly 750 units while all safe-ci scopes share an aggregate quota of about 284 cores; current scheduling admits the work first and lets the shared cgroup throttle the excess. [F-13]

The implication is not "keep K=1 forever." It is that throughput policy must jointly account for fleet validate count, active DAG nodes, and each node's inner width. Reaching ten minutes additionally requires changing the work shape: overlap or reduce the manifest gate, split or parallelize system-utils, and relax or shard the long `hermit_guest` lane.

## Preconditions and next experiment

A new concurrent sweep was deliberately not launched. Exact-main `ec82805424b3` has no clean serial green baseline: the first run hit the `/home/newton` metadata divergence with no peer validates, and the retry failed cgroup readback before tests and left a residual cleanup quarantine. The HTTP fixture still derives a host port from PID, and validate currently supplies no explicit per-run CPU allocation to its outer scope. [F-14] [F-15]

Before measuring C=2:

1. Establish repeated clean first-attempt C=1 passes at one exact SHA.
2. Remove known cross-run collision candidates, especially the fixed derived HTTP port, and ensure residual-process cleanup is clean.
3. Apply and verify explicit per-run CPU, memory, PID, and wall-time envelopes.
4. Preserve inner cell signatures in the outer attempt record, including `HERMIT_SKID_OVERSHOOT`.
5. Record `retry_count_per_validate`, but disable retries for the causal characterization or analyze first attempts separately.

Then run matched blocks at one SHA:

```text
C=1 control -> synchronized C=2 pair -> C=1 control
```

For every validate record the first-attempt outcome, `sum(gates[].retries)`, failed gate and cell, first divergent syscall/event, wall and CPU time, cgroup throttling/pressure, peer identity, and cleanup result. Classify failures into shared-visible-input divergence, PMU skid, resource starvation/budget kill, stable product defect, or infrastructure/setup failure. Only after that rate and mechanism are measured should the global lock be replaced by a bounded admission cap.

## Decision

- Treat concurrency as a flake detector and throughput lever, not as a condition that invalidates evidence.
- Keep first-attempt evidence immutable and expose retried passes as a separate outcome.
- Fix or isolate the known deterministic inputs before assigning causal weight to peer load.
- Do not infer a safe fleet cap from the old intra-worktree `--jobs 24` study or the four uncontrolled overlapping ledger rows.
- Do not expect outer `-j` tuning alone to reach ten minutes; the current critical path is already about 17m49s.
