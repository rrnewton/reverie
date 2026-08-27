# ci-hub general audit and validate optimization plan

- **Date:** 2026-08-08
- **Task:** `ci-hub-general-audit-and-validate-optimization`
- **Scope:** read-only source, GitHub, ledger, and retained-log audit; no validation dispatch
- **Audited parent:** `2e19f864a88fc61df936f982d34221eba71627dc`
- **Audited Hermit main:** `592f7abd0ba29b77209d112c480711de3e8a766c`
- **Companion:** [ci-hub ledger health and deferred-log audit](2026-08-08-ci-hub-ledger-audit.md)

## Executive summary

Two premises are refuted.

1. The full Rust validation migration is **not landed**. Only its Phase 1 typed
   lane wrapper is on Hermit main. Production still enters a 5,140-line
   `validate.sh`, and `safe-ci-dag-runner` still defaults to its tracked Python
   engine. The full migration remains open and conflicting in Hermit PR #1635.
2. Current full validation does **not** have approximately 58 gates outside the
   DAG runner. It has 55 boxed DAG nodes (47 portable and 8 privileged) plus
   seven outer shell gates. The migration branch's “58/58” count means three
   preflight nodes plus those 55 lane nodes.

The latest successful cold validation on the current-main ancestry took 627
seconds for 862 executed tests and zero failures. Comparable main-line samples
were 591, 611, and 627 seconds: median 611 seconds, with only one of three below
ten minutes. That is borderline green, not attestably optimized. The measured
bottleneck is the `hermit_guest: 1` resource path: 16 nodes serialize for 439
node-seconds, while the dependency-only critical path is 260 seconds. Full
validation also performs the same manifest validation four times. Removing the
two outer duplicates saves about 59 measured seconds; fusing the portable and
privileged lanes can overlap up to another 52 seconds. These are projections,
not a new benchmark result.

The recommended destination is:

- six stable ci-hub namespaces (`health`, `validate`, `history`, `land`,
  `obligation`, and `admin`) with 15–20 documented human-facing operations;
- one typed, deduplicated full-validation DAG containing every test and build;
- measured per-node inner widths and safely ratcheted guest concurrency;
- a “solid” performance attestation requiring repeated cold exact-SHA samples,
  not one sub-ten-minute green;
- durable producer-stamped ledger events, automatic multi-machine union, a
  TTL-cached GitHub union, `commit-health`, and recomputable compressed raw-log
  bundles.

## 1. ci-hub command-surface audit

### Inventory

The Rust front door declares **26 top-level commands**
([`ci-hub/ci-hub.rs:76`](../../ci-hub/ci-hub.rs#L76),
[`ci-hub/ci-hub.rs:195`](../../ci-hub/ci-hub.rs#L195)). Six are namespaces with
26 typed nested leaves. The other 20 are standalone commands. `history` adds a
default form and four Python subcommands, so there are **50 callable leaf
forms**, one of which (`history runs`) duplicates the default renderer.

| Disposition | All top-level commands |
| --- | --- |
| Keep as stable user workflows | `quickstart`, `health` (after JSON repair), `active-work`, `main-health`, `pr-status`, `verify-landing`, `refresh-history`, `history`, `runner-health`, `load-probe`, `validate-status`, `hosted-status`, `validate-run`, `validate-stop`, `newest-green`, `first-bad`, `land-lock` (`status`/`run` emphasis), `validate-lock` (`status`/`run` emphasis) |
| Keep only as a clearly labeled diagnostic | `local-history`, `ci-timeout scan` |
| Rename or finish wiring | `ci-mode status/set` (currently only an autoretry-mode projection), `tick` (must execute the canonical `health-tick` wrapper) |
| Hide or deprecate from the public surface | top-level `green-time`, `ci-mode fire`, `batch`, `ci-timeout reap` |
| Internal ABI/protocol/admin only | `receipt-digest`, `ledger`, `apply-local-label`, low-level lock acquire/renew/release/reclaim/census verbs |

Nested inventories are:

- `ledger`: `qualified-rows`, `attribute-reds`
  ([`ci-hub/ci-hub.rs:603`](../../ci-hub/ci-hub.rs#L603));
- `ci-mode`: `status`, `set`, `fire`
  ([`ci-hub/ci-hub.rs:745`](../../ci-hub/ci-hub.rs#L745));
- `batch`: `show`, `set`, `add`, `remove`, `clear`
  ([`ci-hub/ci-hub.rs:852`](../../ci-hub/ci-hub.rs#L852));
- `ci-timeout`: `scan`, `reap`
  ([`ci-hub/ci-hub.rs:953`](../../ci-hub/ci-hub.rs#L953));
- both locks: `acquire`, `renew`, `release`, `status`, `reclaim-dead`,
  `census-orphaned-domain`, `run`
  ([`ci-hub/lib/landing_lock.rs:54`](../../ci-hub/lib/landing_lock.rs#L54),
  [`ci-hub/lib/validate_lock.rs:128`](../../ci-hub/lib/validate_lock.rs#L128));
- `history`: default/`runs`, `node-cpu-budgets`, `green-time`, `kill-taxonomy`
  ([`ci-hub/history/query.py:1426`](../../ci-hub/history/query.py#L1426),
  [`ci-hub/history/query.py:1471`](../../ci-hub/history/query.py#L1471)).

### Commands not fit for the current public surface

1. **`health --json` is not one JSON document.** It prints separate main/PR JSON values. Keep the
   command, but give it a versioned composite schema
   ([`ci-hub/ci-hub.rs:1430`](../../ci-hub/ci-hub.rs#L1430)).
2. **Top-level `green-time` is explicitly incomplete.** Its signal adapter and
   validate-driven densification are not wired. The separate
   `history green-time` has a different meaning: conclusive Actions-run wall
   time. Keep the library internal until it consumes the shared history/ledger
   authority, and retain one unambiguous public name.
3. **`ci-mode fire` is unsafe under merge-gate-v4.** It manually dispatches a
   comparison/debug workflow rather than the gate-owned authority workflow.
   Manual exact-head dispatch can race the gate and poison the authority with a
   cancellation. Rename `status/set` to their actual autoretry-mode behavior or
   fully wire admission before exposing a general CI mode.
4. **`batch` has no workflow consumer.** Its five mutators edit parent state and
   GitHub labels, but no measured workflow consumes `ci-batch`. Hide it until a
   consumer and end-to-end test exist.
5. **`ci-timeout reap` cancels authority runs and only prints a local enqueue
   command.** A cancellation can make a genuinely good exact head read red. Keep
   `scan` as diagnostics; disable `reap` until merge-gate owns the transition.
6. **Top-level `tick` is a weaker duplicate.** It invokes `tick-hub` directly,
   while `bin/health-tick` first refreshes the optional ORC snapshot. Make the
   top-level command execute the canonical wrapper or hide it.
7. **`ledger` is mislabeled as read-only.** `attribute-reds --persist/--refill`
   mutates local evidence
   ([`ci-hub/ci-hub.rs:638`](../../ci-hub/ci-hub.rs#L638)). Separate query and
   maintenance namespaces.
8. **`validate-run` and `validate-stop` are untyped passthroughs.** Their real
   argument contracts live in Python. Either type them at the Rust boundary or
   visibly declare them external adapters
   ([`ci-hub/ci-hub.rs:246`](../../ci-hub/ci-hub.rs#L246)).

### Rationalized target

| Namespace | Human-facing operations |
| --- | --- |
| `health` | summary, main, PRs, runners, active work, load |
| `validate` | run, stop, status, hosted status, worktrees, newest-green, first-bad, logs |
| `history` | runs, refresh, node budgets, green-time |
| `land` | verify, lock status/run, apply validated evidence through the lander |
| `admin` | hidden protocol/ABI operations, ledger maintenance, recovery-only commands |

The speculative-land obligation commands were removed on 2026-08-27 without
compatibility aliases. Test that every remaining documented leaf reaches a real
consumer, then delete dead Python fallback branches after that assertion is
covered.

## 2. Rust validation migration: landed status

### What landed

Current remote Hermit main is
`592f7abd0ba29b77209d112c480711de3e8a766c`. Phase 1 landed through
[PR #1586](https://github.com/rrnewton/hermit/pull/1586) at
`a793335895207f5ed6c22221d4abee3f34bcd3dc`, followed by
[PR #1967](https://github.com/rrnewton/hermit/pull/1967) at
`f65f74462931c10ce822a2d46fbb8a9ea9d86305`. Both commits are ancestors of the
current main tip.

The landed file describes itself as a **Phase 1 additive typed wrapper** and
says that deleting `validate.sh`, porting the approximately 60 non-DAG call
sites, and repointing `make validate` are Phase 2
([`hermit/scripts/validate.rs:8`](../../hermit/scripts/validate.rs#L8)). Its
schema deliberately cannot mint the final counted full receipt without the
separate finalizer.

### What did not land

The full migration remains
[PR #1635](https://github.com/rrnewton/hermit/pull/1635), open at
`40241d7f6e9870ee81b54728c3f2cf5569c2def0` and reported
`CONFLICTING`/`DIRTY`. Its single-Rust-path commit
`6b67eba9076bde591dbdd8c46409520a8d6af73c` and shell-removal commit
`1ad1a7ff58c5882578f765908181f1bd613949aa` are not ancestors of current main.
A later backup line is preserved at
`origin/backup/validate-rs-phase2-rebase-20260808`, tip
`a77d386935aafc813c96f6cee09fdecfbd3983d7`, but that branch is not a merge.

Production still follows this path:

1. the Rust front controller dispatches Python `start_unit.py`
   ([`ci-hub/ci-hub.rs:1745`](../../ci-hub/ci-hub.rs#L1745));
2. Python requires `validate.sh` and constructs
   `with-proxy ./validate.sh full`
   ([`ci-hub/validate/start_unit.py:252`](../../ci-hub/validate/start_unit.py#L252),
   [`ci-hub/validate/start_unit.py:374`](../../ci-hub/validate/start_unit.py#L374));
3. `make validate` also invokes the shell directly
   ([`hermit/Makefile:71`](../../hermit/Makefile#L71));
4. the shell invokes `ci/run-dag.sh` for its two lanes
   ([`hermit/validate.sh:4555`](../../hermit/validate.sh#L4555)); and
5. the DAG resolver defaults to the tracked Python engine; Rust is opt-in
   ([`hermit/agent-utils/common/bin/engine-resolver:15`](../../hermit/agent-utils/common/bin/engine-resolver#L15)).

The hosted workflow comment claiming `validate.sh` is already an exec shim is
stale and contradicted by the executable tree
([`hermit/.github/workflows/ci-portable.yml:531`](../../hermit/.github/workflows/ci-portable.yml#L531)).

**Conclusion:** reconcile the existing migration and its stacked correctness
fixes against current main; do not reimplement it, and do not merge the stale
conflicting head merely to satisfy a migration checkbox.

## 3. Gate triage and DAG destination

### Count reconciliation

Current main has:

- 47 nodes in `ci/dag/portable.json`;
- 8 nodes in `ci/dag/privileged.json`;
- three outer preflights: live Reverie-pin equality, submodule initialization,
  and Reverie-pin/lockfile consistency; and
- for each lane, an outer manifest audit followed by one aggregate DAG call.

Therefore the shell reports seven outer gates, while two of those gates contain
55 boxed nodes. The migration branch's measured **58/58** is three synthesized
preflight nodes plus 47 portable plus 8 privileged nodes. The approximately 60
shell `run_check*` call sites span mutually exclusive quick, focused,
compatibility, envelope, and super/stress profiles; they are not 60 additional
gates in a normal full run.

### What belongs in the DAG

Every test and build should become a typed DAG node or an explicitly sharded set
of nodes:

- the existing 55 portable/privileged nodes;
- the quick profile's build, metadata, ptrace E2E, core tests, run, verify, and
  record/replay smoke gates;
- focused build plus SaBRe, e9patch, LiteInst, record/replay, QEMU, envelope,
  and strict-compatibility gates;
- the mechanically extractable super/stress diagnostic gates; and
- large aggregate nodes such as the 150-second Hermit integration batch and
  strict-compatibility program loop, split into deterministic shards after one
  shared build barrier.

Keep only producer/admission work outside the DAG:

- argument parsing and profile selection;
- exact-SHA/tree/admission checks and the invocation lock;
- cache decision and durable-log setup;
- final completeness/count aggregation;
- durable ledger/finalizer writes and optional publication.

Submodule materialization may be a `pre.submodules` node only if the producer
already materialized the runner dependency. Otherwise it remains producer setup
before the DAG can exist.

### Existing parallelism constraints

The portable lane has 16 nodes tagged `hermit_guest` under cap 1 and 13 manifest
nodes under cap 4. The privileged lane has two KVM nodes under cap 1
([`hermit/ci/dag/README.md:140`](../../hermit/ci/dag/README.md#L140)). Only two
of the 55 lane nodes declare `preferred_inner_jobs`; eight commands hardcode
`CARGO_BUILD_JOBS=8`. The two fat build nodes carry a measured width of 32, but
the Reverie DBI wrapper correctly clamps its native width to 16.

“Maximum inner parallelism” must mean the **measured work-conservation knee for
each node under its memory and CPU box**, not `nproc` on a 316-CPU host. Keep
intentional `--test-threads=1` for deterministic guest cases and gain speed by
running isolated outer nodes concurrently.

## 4. Current wall time and critical path

No new validation was launched for this audit. Measurements come from exact-SHA
ledger rows and retained full logs.

| Commit | Relationship to current main | Cold wall | Result | Tests | DAG width |
| --- | --- | ---: | --- | ---: | ---: |
| `35d76a5859d31b532ffba303688aebecc6844e9e` | ancestor | 627 s | pass | 862/862 | 16 |
| `b22e0f30700602f0f8fa92ff2895ad0d307f7542` | ancestor | 611 s | pass | 862/862 | 16 |
| `b6051b1cd1402526c76ea768167c875188144328` | ancestor | 591 s | pass | 862/862 | 16 |
| `8790cece3192e2f284ae5c7edcf37ccfef32367a` | not current-main ancestry | 582 s | pass | 862/862 | 16 |
| current `592f7abd0ba29b77209d112c480711de3e8a766c` | exact tip | 3 s | preflight fail | none | 16 |

The exact 627-second run is the most useful current-main-line decomposition:

- preflight gates: about 2 seconds;
- first outer manifest audit: 32 seconds;
- portable DAG: 47/47 in 511.7 seconds;
- second outer manifest audit: 27 seconds; and
- privileged DAG: 8/8 in 51.3 seconds.

Evidence is in the canonical ignored ledger row and retained log
([`ignored/validate-run-ledger.jsonl:148`](../../ignored/validate-run-ledger.jsonl#L148),
`/tmp/hermit-validate.E7UOt6.log`, lines 6827 and 6986). The current exact tip's
three-second row is a pin-preflight red and is not a wall-time measurement of
the suite.

The dependency-only portable path is 260 seconds:

`e2e.metadata 28 → build.workspace 48 → test.regular_crates 129 → test.strict_compat 55`.

The observed resource path is longer: the 16 cap-1 `hermit_guest` nodes total
439 node-seconds, led by `test.hermit_integration` at 150 seconds. The manifest
pool totals 496 node-seconds; cap 4 gives a theoretical lower bound near 124
seconds. Thus the runtime is constrained by safe resource serialization and
aggregate inner loops, not merely DAG dependency shape.

Full validation performs manifest validation four times: the two outer shell
audits plus the `e2e.metadata` node inside each DAG. Removing only the outer
duplicates projects the 627-second run to roughly 568 seconds. A fused full DAG
can deduplicate the shared pin/metadata nodes and overlap the approximately
52-second privileged lane with portable work. These projections are sufficient
to justify implementation, but only fresh measurements can establish success.

### Attestable “solid” criterion

A single 582-second green is insufficient. Require:

1. one exact Hermit SHA and declared Reverie SHA;
2. at least five cold full samples on a named comparable host class;
3. complete coverage/counts, nonzero execution, zero failures, and no
   timeout/OOM/cgroup fallback;
4. retained per-node profiles with runner version, manifest digests, outer and
   inner widths, resource caps, cache state, and concurrent-validate count;
5. reported dependency and resource critical paths; and
6. p90 wall time at or below **540 seconds**, leaving 60 seconds of operational
   headroom below the ten-minute objective.

Use the p90 criterion as a proposed acceptance threshold, not as a claim that
the current system meets it.

## 5. Prioritized implementation plan

Implementation should be filed as separate tasks. Stable slugs below name the
observable outcome rather than an ordinal.

### P0 — authority correctness and immediate safety

1. **`rust-validate-phase2-reconcile-current-main`**
   - Rebase/reconcile PR #1635 and its stacked correctness fixes onto current
     main.
   - Preserve every current profile and receipt denominator.
   - Bracket old-vs-new profile parity before making `validate.sh` an exec shim.
   - Prove the final migration commit and shell removal are main ancestors.
2. **`full-validate-single-deduplicated-dag`**
   - Construct one typed full plan from three preflights plus the lane nodes.
   - Remove the two outer manifest duplicates, deduplicate shared metadata/pin
     nodes, and schedule portable and privileged nodes together.
   - Preserve fail-closed behavior and exact node/test counts.
3. **`validate-ledger-durable-producer-events`**
   - Stamp truthful producer/version provenance at the original writer.
   - Make an otherwise-green run fail closed if its evidence cannot be durably
     recorded.
   - Use locked complete writes plus `fsync` and a recoverable local spool.
   - Only after all deployed writers emit the new shape, activate producer-floor
     enforcement in `qualifying-receipt.json`.
4. **`ci-hub-quarantine-misleading-mutators`**
   - Fix `health --json`.
   - Hide `ci-mode fire`, batch mutators, `ci-timeout reap`,
     and incomplete top-level `green-time` from normal help.
   - Make `tick` execute the canonical health wrapper.

### P1 — measured parallelism, usable authority, and surface consolidation

5. **`validate-guest-isolation-and-cpa-widths`**
   - Give guest nodes disjoint scratch/output paths or immutable prebuilt
     artifacts.
   - Split integration and compatibility aggregate loops into explicit shards.
   - Plant negative/positive concurrent-execution brackets, then ratchet
     `hermit_guest` cap 1→2 and only then 2→4.
   - Sweep heavy-node inner widths under hard memory caps and commit the measured
     CPA recommendations. Consider manifest cap 4→8 only after memory evidence.
6. **`validate-solid-attestation`**
   - Retain fresh-checkout per-node profiles instead of deleting them at cleanup.
   - Emit a versioned exact-SHA performance attestation implementing the cold
     N≥5/p90≤540s criterion and all correctness denominators.
7. **`ci-hub-six-namespace-surface`**
   - Introduce the six stable namespaces and 15–20 human-facing leaves.
   - Type Python passthrough contracts, separate query from maintenance, and add
     hidden compatibility aliases for one release.
   - Remove dead fallbacks after end-to-end command coverage exists.
8. **`fleet-ledger-union-and-commit-health`**
   - Automatically import/spool/publish qualifying events into per-host tracked
     shards and prove union on a real second machine.
   - Reuse the Actions history store as a TTL/ETag cache keyed by repository,
     exact SHA, and policy version; keep short TTLs for pending/no-result.
   - Add `ci-hub commit-health` showing local-live, fleet-ledger,
     GitHub-validation-index, GitHub-Actions, freshness, denominators,
     disagreements, and the versioned combined verdict.

### P2 — deferred determinism/parity evidence

9. **`validate-log-bundles-parity-recompute`**
   - Store atomic compressed bundles under
     `ignored/validate-logs/v1/<host>/<sha>/<run-id>/`.
   - Preserve full run A/B info streams, ptrace and backend operands, verify
     JSON, exact commands, manifest digests, and checksums.
   - Add a pure `ci-hub parity recompute --bundle PATH` reader and cache only
     compact digest-bound results in the ledger.
   - Monitor allocated bytes, pin receipt/obligation/certification evidence, and
     rotate only finalized unpinned bundles under a scoped lock.

The ledger and log details, including current failure modes and the full bundle
schema, are in the
[companion ledger audit](2026-08-08-ci-hub-ledger-audit.md#recommended-implementation-order).

## Disposition

This audit changes no product code and launches no CI. The coordinator recovery
commit `eb032fa0f5f489e7436b74bd1d41387a243cd5e6` on
`recovery/hermit-coord-owner-five-path-main-red-20260808` remains local,
untouched, and unpublished. Implementation belongs in the separate P0–P2 tasks
above.
