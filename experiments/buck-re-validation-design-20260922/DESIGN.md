# Buck2 remote build and validation design

Date: 2026-09-22

Base: `dev-hermit` `ace5e53f43a5a5ca09ef3ed16e20823a02b0f7a8`; Hermit evidence gitlink `6988c184d8ff245907ddfaf53744eb27b03669e5`; fbsource evidence revision `a60b3b9cd9f7a644f8d0f73c007a3548b5791b02`

Status: design and pre-experiment evidence. Remote execution has not yet been attempted, and a fresh complete current-main validation profile is intentionally pending the authoritative manifest-validation lane. The measurements below are useful historical and prefix evidence, not that missing baseline.

## Decision

Adopt Buck2 in validation in three independently reversible boundaries:

1. make the existing OSS Hermit target remotely buildable and measure it;
2. let Buck2 supply the release Hermit binary to the existing validation DAG while Cargo continues to supply the install/resource bundle;
3. move manifest-defined E2E buckets behind separately schedulable Buck test targets, first locally and then on capability-qualified RE workers.

Do **not** replace the E2E harness with one opaque `buck test` action, raise the DAG width globally, or switch the current Buck binary into validation yet. The present Buck target is not feature- or provenance-equivalent to the Cargo validation binary, the OSS toolchain is host-dependent, and there is no checked-in RE endpoint/platform configuration.

The best first performance target is the build path. Build nodes were 2,574 CPU-seconds, 60% of process CPU, in the latest retained complete run. In the near-current partial run, four Cargo build nodes consumed 4,957 CPU-seconds, 87% of recorded node CPU, and the observed critical chain was build-bound. E2E migration is still valuable for ownership, cacheability and eventual remote work, but the complete run's portable E2E cells accounted for 380 CPU-seconds and occupied a 177-second window.

## Evidence and limits

The two useful profiles answer different questions. They must not be merged as if they were one benchmark.

| Profile | Coverage | Wall | CPU denominator | What it supports |
|---|---:|---:|---:|---|
| Complete, 2026-09-05, Hermit `991174b029b7` | 267/267 nodes, 2,755 tests, no retries, PASS | 998 s | 4,297 process CPU-s; 4,177.678 CPU-s attributed to outer-node cgroups | Whole-run proportions and a valid E2E inventory |
| Near-current, 2026-09-22, Hermit `e123b2e34df8` | 14 attempts, 13 succeeded, 1 failed; stopped before most tests | 969 s | 5,711 recorded node cgroup CPU-s | Current build topology and observed build critical chain only |

The complete SHA is not an ancestor of current Hermit main. The partial SHA is current main plus four unlanded commits from [facebookexperimental/hermit pull request 3171](https://github.com/facebookexperimental/hermit/pull/3171). The retained current constructed plan has `path == "full"` and selects 270 unique `group.job` steps versus 267 nodes in the complete run; the manifest itself contains definitions for more than one profile. Accordingly, the complete profile is the denominator for whole-run claims, while the partial profile is evidence about the present build prefix—not a validation baseline.

The complete run reports 4,587 node-wall-seconds over a 998-second makespan and 4,297 CPU-seconds, or 4.3 CPU-seconds per wall second. The journal serializes populated `cpu_usage_usec` fields as decimal JSON strings, not JSON numbers. Validating and converting those strings from outer `step_end` rows attributes 4,177.677642 CPU-seconds after excluding one deliberately failed nested `pre.submodules` fixture (4.204602 CPU-seconds) that wrote into the same journal during a manifest self-test. Nested pinned-root cgroups under-report some children, so individual cgroup rows are directional. Aggregate process CPU and producer-owned E2E summaries are the stronger denominators.

The reproducible journal filter is: select `event == "step_end"`; require string-valued `cpu_usage_usec` matching `^[0-9]+$`; exclude the single row with `step == "pre.submodules"`, `ok == "false"`, and `cpu_usage_usec == "4204602"`; convert each remaining value with `tonumber`, sum and divide by one million. This prevents nested negative-test work from being presented as outer validation CPU. The checked-in extractor implements this exact rule and fails if its results differ from the table.

### Complete-run slices

| Slice | Recorded CPU-s | Share of 4,297 process CPU | Recorded node-wall-s | Interpretation |
|---|---:|---:|---:|---|
| Build group | 2,574 | 59.9% | 931 | Largest aggregate CPU opportunity |
| Test group | 615 | 14.3% | 2,225 | Broad, parallel work; wall sum is not critical path |
| Portable E2E cells, from producer summaries | 380 | 8.8% | 177 s observed first-to-last window | Actionable remote-test ceiling before guest-build work is counted |
| Check group | 180 | 4.2% | 234 | Secondary opportunity |
| Doc group | 176 | 4.1% | 38 | High parallel CPU, low wall leverage |

Producer summaries contain 680 cells and 380.955137 CPU-seconds in total: 677 portable cells with 379.885215 CPU-seconds and three privileged cells with 1.069922 CPU-seconds. The largest portable E2E bucket was `c_programs`: 404 cells and 197 CPU-seconds, 52% of measured portable E2E CPU. `backend_parity` was next by cell count (179 cells, 43 CPU-seconds); `applications` used 41 CPU-seconds across only four cells. This makes `c_programs` both the first meaningful Buck-test migration and a good sharding test.

### Near-current build prefix

| Node | CPU-s | Share of 5,711 partial CPU | Wall-s |
|---|---:|---:|---:|
| `build.workspace_in_pinned_root` | 1,778 | 31.1% | 463 |
| `build.runtime_release_in_pinned_root` | 1,309 | 22.9% | 156 |
| `build.rust_scripts` | 968 | 16.9% | 176 |
| `build.rust_scripts_in_pinned_root` | 903 | 15.8% | 164 |

Together these four nodes consumed 4,957 CPU-seconds, 86.8% of the incomplete run's recorded CPU. The observed dependency chain accounted for 968.841 of 968.874 seconds of makespan:

```text
pre.submodules -> pre.reverie -> pinned fetch -> pinned rust_scripts
 -> pinned workspace -> e2e artifact -> pinned liteinst release
```

`runtime_release_in_pinned_root` finished about 308 seconds before `workspace_in_pinned_root`. If Buck supplies the selected binary and artifact publication waits only for the resource-producing release node, that particular chain falls from 969 to an idealized 661 seconds. The 308 seconds (31.8%) is a structural upper bound for this failed prefix, not a forecast for a complete validation: Buck readiness, resource work, other tests, cache state and a different critical path can erase some or all of it.

Machine-readable values and exact retained paths are in [measurements.tsv](measurements.tsv).

### Auditing the measurements

The experiment tracks bounded extracts rather than the 550,470 logical bytes (about 538 KiB) of source journal and log. `source-extracts/cold-build-commit.txt` retains the scope-qualified cold timing evidence; `complete-step-ends.jsonl` retains all 304 raw `step_end` measurement tuples, including the nested negative fixtures; `complete-e2e-boundaries.jsonl` retains the 13 portable E2E start/end pairs used for the observed window; `e2e-summaries.jsonl` retains all 15 producer summaries; `partial-profile.csv` retains the complete 9-KiB partial profile; `constructed-plan-steps.json` retains the full-plan identity and all 270 `group.job` pairs; and `complete-final-summary.txt` retains the validation verdict and process totals. `UPSTREAM_SHA256SUMS` binds those extracts to the original ignored artifacts, while `EXTRACT_SHA256SUMS` protects the tracked extracts themselves.

From the repository root, independently recompute and compare every derived CPU, wall, inventory, critical-chain and upper-bound value:

```bash
experiments/buck-re-validation-design-20260922/extract-measurements.sh verify
```

The script also has a bounded `capture` mode for recreating these extracts from their cited original artifacts. It rejects oversized inputs, more than 128 E2E summaries, missing inputs and non-decimal CPU fields rather than silently treating them as zero. Incorporating the future authoritative current-main run remains a deliberate report update: its nested fixtures and profile topology must be reviewed instead of being assumed identical to this run.

## Current gaps

### The OSS Buck target is not a validation release binary

`hermit-cli/BUCK` supplies `detcore`, ptrace, KVM, LiteInst and RPC dependencies, but omits the DBT dependencies selected by Cargo's `third-party-backends` feature. Its build metadata is hard-coded to version `0.2.0`, date `unknown`, and SHA `unknown`. No explicit Buck release configuration demonstrates parity with Cargo's optimization, panic, debug, or link behavior.

Validation currently does more than build a binary. `build.runtime_release` builds Cargo `--release --features third-party-backends`, creates resources and an install package, and copies a strict binary. The E2E artifact publisher requires a complete bundle including DBT, SaBRe, LiteInst, DynamoRIO and e9patch artifacts. E2E consumers export both `HERMIT_BIN` and `HERMIT_INSTALL_DIR`. A binary-only switch that bypasses this contract would silently reduce backend coverage.

### The OSS toolchain is not remote-hermetic

The OSS shim selects `system_cxx_toolchain` and `system_rust_toolchain`. The nightly workflow builds a wrapper around an absolute `rustup which rustc` path and injects `-llzma`. This works on the hosted runner but is not a declared RE toolchain. A remote worker needs either:

- a versioned container image with exactly the pinned Rust/C++ tools and system libraries, included in execution-platform identity; or
- fully declared compiler/sysroot/linker artifacts in the action inputs.

The container is the shorter prototype path. Declared toolchains are the stronger long-term path.

### Validation has no build-system contract

The validation CLI has no Cargo/Buck selector. Cache-state reporting only recognizes `target/debug/hermit` and `target/release/hermit`. The E2E artifact is published from mutable Cargo output, while several compatibility nodes refer directly to `target/ci/hermit-strict`. A switch needs one stable selected-binary contract and needs to record its producer in the plan and receipt.

### Old Buck tests are not authoritative E2E policy

`tests/BUCK` and `tests/helpers.bzl` generate per-test shell targets from globs and hard-wire a Buck Hermit binary. The current YAML manifest is load-bearing policy for lane, mode, backend, CPU and wall limits, build protocol, exclusions and host applicability. The Rust harness publishes typed per-cell results before deciding the aggregate outcome, produces JUnit, retries individual cells, and checks completeness. Reimplementing those semantics in Starlark or accepting one batch result would weaken evidence.

The current OSS test configuration also does not express remote eligibility. `shim/BUCK` registers a remote-test-execution toolchain and a no-op test toolchain, while the OSS `tpx_labels` shim emits empty strings. Generated tests carry neither a use case nor platform properties; the configured no-property path therefore stays local. Remote builds working is not evidence that `buck2 test` actions went remote.

## Target architecture

### Stable artifact boundary

Add two orthogonal, explicit settings:

```text
--build-system cargo|buck2
--buck-execution local|prefer-remote|remote-only
```

They must appear in the constructed plan, cache key, artifact metadata and final receipt. Environment variables may be conveniences, but must resolve to these recorded values before execution.

Split today's release node conceptually into:

```text
selected binary producer ─┐
                          ├─> publish content-addressed E2E artifact
resource bundle producer ─┘       ├─ hermit-strict
                                  ├─ install resources
                                  └─ provenance.json
```

Initially, Cargo remains the resource producer and Buck becomes only the binary producer. The publisher copies the selected binary to `target/ci/hermit-strict`, verifies the complete resource inventory, hashes all components, and records:

- source and Reverie SHAs;
- build system and execution mode;
- target platform and feature set;
- Buck configuration and producer-action digest, or Cargo profile and producer invocation;
- binary hash and resource-manifest hash;
- compiler, linker, sysroot/toolchain digest and RE worker-image identity.

The publisher computes `bundle_id = SHA-256(canonical attestation JSON || canonical sorted file-tree manifest)`. The Buck binary and Cargo resource producers must attest the same source SHA, Reverie SHA, target platform and sorted feature contract. Toolchain and producer-action identities may differ only where the contract names distinct producers; their digests remain in the canonical attestation. Guest-producer identity is checked later when forming each test input ID. The final receipt binds the exact bundle ID, not merely a mutable path.

Publication fails before any consumer runs when an attestation is missing, a file hash differs, source or Reverie SHAs disagree, the platform/features do not equal the selected plan, or an input's producer identity does not equal the requested identity. Negative tests must substitute a stale binary, stale resource bundle and mismatched Reverie/toolchain/platform attestation and prove refusal. Cache keys include the canonical attestation and every declared input digest, so a stale bundle cannot become a cache hit for the current plan.

All downstream nodes must resolve the published artifact or the stable strict path. No consumer may reach back to mutable `target/debug/hermit`, `target/release/hermit`, or a Buck output path.

### Test target shape

Use one Buck command selecting many separately schedulable targets. The command is the batch; targets are the scheduling and reporting units. Initial targets should align with manifest buckets, with deterministic sub-shards for the large `c_programs` bucket. Each target invokes the existing harness with an explicit selection and emits the existing typed result stream and JUnit.

Generate target/shard declarations from the parsed manifest into a checked-in file, and add a drift check. The manifest remains the single policy source. Shards must keep coupled record/replay or backend-parity cells together and use stable cell IDs, not timing-dependent partitioning.

The validation adapter waits for the Buck invocation, imports every typed result into the existing ledger, checks expected versus observed cell IDs, and then applies the existing aggregate gate. A Buck pass cannot compensate for a missing result.

### Hermetic Buck-test artifact bridge

The Phase 2 hybrid artifact under `target/ci` is not directly usable as a hermetic Buck input: Buck cannot action-key an ambient Cargo output. Before Phase 4, add Buck-owned targets for the validation `test-harness`, every required backend resource and the install layout. A `validation_e2e_bundle` packaging rule consumes those Buck artifacts, verifies the same inventory/attestation contract as the Phase 2 publisher, and emits one directory artifact plus its bundle ID. If Buck does not yet own a required resource, the Buck-test phase remains blocked; it must not import that resource from `target/`.

Generate a custom `hermit_e2e_shard_test` rule invocation per shard. Its attributes name the harness executable target, `validation_e2e_bundle`, generated shard-plan target, guest/fixture targets, execution profile and result schema. Its implementation returns `ExternalRunnerTestInfo` with those artifacts embedded in `command`/`env`; it does not assign execution or artifact collection to `TestToolchainInfo` (whose relevant Prelude contract is only sanitizer configuration). Buck therefore includes the artifacts in the action key and sandbox. Each generated shard target has a closed input set:

- the built `test-harness` executable and wrapper launcher;
- the Buck-produced content-addressed Hermit/install bundle, including `hermit-strict`, the complete backend resource tree and `provenance.json`;
- the generated shard plan, which contains exact manifest digest, cell IDs, modes, backend, budgets and expected result count;
- every guest binary, script, fixture and data file selected by that shard; guest binaries are Buck outputs or a content-addressed guest bundle, never ambient `target/` files;
- the worker capability-profile descriptor and immutable toolchain/container identity.

The action runs in a Buck sandbox with only those read-only inputs plus a fresh writable work/results directory. It does not read the repository checkout, `$HOME`, a Cargo target directory, or a previously published pointer. The wrapper passes artifact paths explicitly to the harness as `HERMIT_BIN`, `HERMIT_INSTALL_DIR`, shard-plan path and guest root. A Starlark analysis test enumerates the expected target inputs, and an execution test with ambient outputs removed proves the declaration is complete.

Run `buck2 test` with a Hermit-aware OSS external runner. From each `ExternalRunnerTestInfo`, that runner creates a local or RE action with a declared output directory named `test-out`, sets `HERMIT_TEST_OUTPUT_DIR` to it, and includes the directory in the REAPI output-directory list. The worker wrapper writes `results.jsonl`, `junit.xml` and per-cell attempt logs through temporary names, renames them into place, and writes `result-manifest.json` last as the completion marker. Logical test failure and harness-enforced cell timeout are encoded in that manifest while the wrapper exits successfully; the external runner first downloads and validates `test-out`, then reports the logical pass/fail status to Buck. Thus a failing test retains declared outputs instead of relying on side effects from a failed action.

The immutable result manifest binds every output hash to `test_input_id`, the selected cell IDs and their logical outcomes. It does **not** contain worker identity, action attempt, or local/remote/cache disposition: those are known only after execution and cannot vary inside a byte-identical cached output.

After every execution or cache lookup, the external runner writes a separate, non-cacheable `attempt-envelope.json` beside the materialized output. It records the SHA-256 of `result-manifest.json` and the materialized output-tree digest, action/operation ID, attempt ordinal, resolved execution profile and use case, executor disposition (`LOCAL`, `REMOTE`, or `ACTION_CACHE`), queue/execution/materialization timings, and worker/platform identity when the executor provides it. This hash-binds post-action facts to immutable test output without changing that output. A cache hit therefore reuses byte-identical `test-out` but gets a new envelope stating `ACTION_CACHE`.

An action-level crash, timeout or worker loss remains an infrastructure failure and may have no output directory. The external runner records an attempt envelope with no output hash and must not invent per-cell results. For completed actions, it materializes `test-out` to a caller-owned validation artifact directory and refuses absent, partial, duplicate, or hash-mismatched results. Only after successful materialization does validation import rows into its canonical ledger. Contract tests deliberately produce logical failure and cell timeout, assert that Buck reports failure, and byte-check the retained JSONL/JUnit/log artifacts; a separate killed-action test asserts infrastructure classification rather than a false test result.

Guest compilation and the Hermit/install bundle remain ordinary cacheable build actions. Runtime test actions are initially non-cacheable. A later test-result cache may be enabled only after a cold/warm negative test proves its key includes the complete bundle ID, shard-plan digest, requested execution profile/capabilities and toolchain platform, and that a cache hit materializes byte-identical typed artifacts. Cache disposition belongs only in the attempt envelope. Harness retries remain per-cell and are recorded inside one action. Buck/RE retries are allowed only for classified infrastructure loss, rerun the entire shard with a new attempt envelope, and never erase the first attempt; assertion failures and per-cell timeouts are not action-retryable.

For each shard, require the guest/harness attestations to match the runtime bundle's source SHA, Reverie SHA, platform and feature contract, then compute `test_input_id = SHA-256(canonical JSON(runtime bundle ID, guest/fixture tree digest, harness digest, shard-plan digest, execution-profile digest))`. The Buck action key includes the same artifacts; `result-manifest.json` and the final validation receipt record `test_input_id`. This binds guests and test policy to the runtime bundle without pretending they are files inside the install bundle.

## Phased plan

### Phase 0: establish an RE baseline (not yet executed)

Phase 0A is a prerequisite, not a later acceptance task: run one complete, clean current-main Cargo validation with node profiles, outer-journal CPU derivation, typed cell summaries and the constructed plan retained. Do not size phases, claim a speedup, or begin the Cargo/Buck A/B from the divergent September 5 run or failed PR prefix. Freeze its source/Reverie SHAs, host class, toolchain and cache state as the local comparison baseline.

Create an optional, non-secret RE config and Linux execution platform based on the standard REAPI fields: execution, action-cache and CAS endpoints; instance/use case; TLS or headers supplied only at runtime; digest algorithm; container image; and output-path behavior. Keep local and remote execution platforms selectable.

First make the toolchain portable. Pin the worker image by immutable digest and remove assumptions that an absolute host `rustc`, host `gcc/g++`, `liblzma`, `libunwind` or `zstd` happens to exist.

Run the same Hermit SHA and target under:

1. local-only, empty Buck action cache;
2. prefer-remote, empty action cache;
3. remote-only, empty action cache;
4. prefer-remote, warm action cache on a fresh client output tree.

Retain Buck event logs and report action counts by `LOCAL`, `REMOTE`, `ACTION_CACHE`, failure/fallback and materialization. Report RE queue, execution, upload/download and materialization separately from local cgroup CPU and end-to-end wall. `buck2 log critical-path`, `whatran`, `what-materialized` and `what-uploaded` are the required evidence views.

Gate: the fresh current-main Cargo profile is complete first. Then remote-only builds `//hermit-cli:hermit` with no compile/link fallback to local execution, the binary hash is reproducible under identical inputs, secrets are absent from logs/config, and failure injection proves remote cancellation and cleanup.

Expected upside: unknown until action logs exist. The landed nightly commit records a cold Cargo release at 104.81 seconds and the narrower Buck Hermit target at 59.89 seconds on the same host; its commit message explicitly says the workload scope differs. That shows local Buck is competitive, not that Buck or RE is 43% faster for validation. Confidence: low.

### Phase 1: make a feature-equivalent Buck release target

Define an explicit release configuration and bring `third-party-backends` to parity, including DBT artifacts. Generate exact source/version/date metadata rather than `unknown`. Compare Cargo and Buck products for:

- backend inventory and CLI capability output;
- required dynamic libraries and install resources;
- optimization/panic/link configuration;
- source and Reverie provenance;
- representative run, record, replay and verify behavior through the official validation path.

Run it in shadow mode: build both binaries, publish the Cargo binary, and execute a bounded parity matrix against both. Any behavioral or selected-test divergence is a hard failure.

Gate: explicit parity matrix passes locally and through RE, and output provenance is sufficient to reproduce the action. Confidence in a safe switch after the gate: high; performance upside remains unmeasured.

### Phase 2: select the Buck binary at the DAG boundary

Implement the two switches and the stable artifact contract. In Buck mode:

- Buck produces the selected release binary;
- Cargo initially continues building the third-party resource bundle;
- the publisher combines them and verifies hashes/inventory;
- `build.e2e_artifact` no longer depends on the entire Cargo workspace solely to obtain `target/debug/hermit`;
- all downstream direct binary references move to the selected artifact contract.

Before publication, the mixed-producer attestation gate verifies equal source SHA, Reverie SHA, target platform and requested feature contract across the Buck binary and Cargo resource bundle. It recomputes the bundle ID from the canonical attestation and file hashes. Stale or mismatched producer negative fixtures must fail before `build.e2e_artifact` exposes a pointer; Phase 4's separate `test_input_id` gate adds guest and harness identity.

Keep the full plan structure and test inventory invariant across modes. A plan diff should show only producer/resources and their dependency edges, never deleted validation consumers.

Gate: at least three cold and three warm complete A/B validations at the same SHA and host class, with identical expected/observed node and cell inventories, no retries hidden, and retained receipts/event logs. Report median, range, critical path and cache hit rate—not just fastest elapsed time.

Likely upside: this attacks the dominant build slice. On the partial near-current graph, removing the workspace prerequisite exposes an upper bound of 308 seconds (31.8%) on that observed prefix. The complete-run improvement will be smaller or zero if another branch is critical. Confidence: medium that dependency decoupling helps; low on magnitude until full A/B.

### Phase 3: remove duplicate Cargo binary work

After Phase 2 is authoritative, separate Cargo workspace outputs actually needed by nextest, proc-macros and tests from Cargo's duplicate Hermit CLI/backend build. Delete only work proven redundant by Buck action and Cargo timing data. Keep the Cargo producer as a selectable rollback mode for at least one release cycle.

As a separate prerequisite for Phase 4, add Buck targets for the manifest harness and the full E2E install/resource inventory, then package them as `validation_e2e_bundle`. This may land after the binary default switch, but Buck tests cannot begin without it. Compare its file inventory, hashes, capabilities and source/Reverie/platform/features attestations against the Phase 2 hybrid artifact; missing DBT, SaBRe, LiteInst, DynamoRIO or e9patch content is a hard failure.

Gate: full A/B remains inventory-identical after removal and the Cargo rollback mode still passes. Report both local CPU saved and RE CPU consumed; lower local CPU alone is not lower total cost.

Expected upside: potentially the largest aggregate CPU reduction, because builds were 60% of complete-run CPU and 87% of the current partial prefix. Those figures are opportunity bounds, not attributable savings: much of the workspace is still needed for Rust tests. Confidence: low until the build graph is separated and measured.

### Phase 4: Buck-test bridge, local execution

Prove the adapter on a small calibrated shard, then migrate `c_programs`, the largest measured bucket, before broadening. Each Buck target calls the current harness and must preserve:

- exact selected-cell count and host-inapplicable accounting;
- per-cell CPU and wall budgets and multiplier semantics;
- result-before-outcome ordering;
- individual-cell retry history and failure classification;
- JSONL/JUnit/artifact paths and completeness checks.

Run a single `buck2 test` request over all selected targets. Avoid one Buck process per DAG cell or bucket: concurrent commands share daemon state, obscure one graph-level critical path, and have test-result serialization caveats. Avoid one monolithic test target: Buck and RE could schedule only one action and validation would lose bucket attribution.

Gate: local Cargo-harness mode and local Buck-test mode select the same cell IDs and yield equivalent typed ledgers on repeated full runs. Deliberate fail, timeout, retry and missing-result probes must fail identically.

The gate also runs each shard from a checkout with ambient Cargo outputs removed, proves every required input appears in Buck's action declaration, and verifies materialization of JSONL/JUnit/log artifacts on pass, assertion failure and cell timeout. An infrastructure-cancellation probe must remain an incomplete outer attempt rather than a pass or fabricated cell timeout. A stale-bundle substitution and an unlisted-guest access must be refused. Test-result caching remains disabled at this phase.

Expected performance: neutral initially. This is an ownership/scheduling change. The directly measured portable E2E ceiling was 380 CPU-seconds and a 177-second execution window in the complete run; migration overhead can make it slower. Confidence: high that bucket/shard targets preserve policy if the ledger is authoritative.

### Phase 5: execute eligible E2E actions remotely

Keep pure build actions on the build execution platform. For tests, define named profiles in the existing `remote_test_execution_toolchain(profiles = ...)` rather than labeling whole buckets remote-capable:

- `e2e-ptrace`: separate test use case; pinned image plus proved user namespaces, `/proc`, signal, ptrace, seccomp and cgroup accounting capabilities;
- `e2e-perf`: dedicated test use case/profile with stable PMU/perf capability and calibrated worker class;
- `e2e-kvm`: dedicated isolated `/dev/kvm` pool only; otherwise generated as local;
- `e2e-local`: explicit local executor for host-specific, unsupported or privileged cells, never a silent skip.

Prelude's actual profile schema accepts `capabilities`, `local_enabled` and `use_case`; it does not accept the previously proposed `platform_properties` key. Configure the toolchain with no default profile and no implicit bundling:

```python
remote_test_execution_toolchain(
    name = "remote_test_execution",
    default_profile = None,
    default_run_as_bundle = False,
    profiles = {
        "e2e-ptrace": {
            "capabilities": {
                "OSFamily": "Linux",
                "container-image": "<immutable digest>",
                "hermit.ptrace": "true",
                "hermit.userns": "true",
                "hermit.cgroup-accounting": "true",
            },
            "local_enabled": False,
            "remote_cache_enabled": False,
            "use_case": "hermit-e2e-ptrace",
        },
        # e2e-perf and e2e-kvm have distinct use cases/capabilities.
    },
)
```

The manifest-to-Buck generator maps every shard to exactly one profile from its lane, backend, program type and typed host requirements, and generated remote targets set `remote_execution = "e2e-ptrace"` (or the corresponding named profile) on `hermit_e2e_shard_test`. Local shards use Prelude's valid sentinel `remote_execution = "disabled"`; an inline dictionary missing `capabilities` or `use_case` is invalid. Generation fails if a requirement has no mapping, a remote profile is absent, or its capabilities/use case are empty; this is necessary because the current no-property path executes locally. The checked-in generated file and a drift audit make the mapping reviewable.

Before admitting a worker pool, run capability probes under the same use case and exact properties. The external runner's attempt envelope attests the resolved execution profile, worker image/platform and whether execution was local, remote or cached, while hash-binding those facts to the immutable result manifest. A claimed remote run with local execution, property fallback, or an unexpected use case is an infrastructure failure.

Remote test results must return the typed ledger plus logs/JUnit. Worker platform and image identity belong in each attempt. Retry may change worker, but history must remain visible.

Gate: capability probes run as tests on every RE pool; deliberately absent properties and misrouted privileged tests fail as infrastructure errors; action logs prove each remote shard used its generated test use case/profile; remote and local modes have identical non-vacuous inventory. Run cold/warm A/B as in Phase 2.

Maximum directly evidenced opportunity is bounded by the old run's 380 portable E2E CPU-seconds and 177-second portable E2E window before accounting for remotely built guest binaries or privileged cells. Only work on the critical path reduces wall time, and queue/materialization can reverse the gain. Confidence: low until the pool is characterized.

## Timeout and resource semantics

The current local CPU clock does not include CPU burned by an RE worker. Reusing it as a total CPU budget would make remote mode look artificially cheap. Store at least:

```text
local_cpu_usec
remote_queue_usec
remote_execution_usec
remote_upload_usec / remote_download_usec
materialization_usec
remote action counts and cache-hit counts
end_to_end_wall_usec
```

Keep the manifest's per-cell CPU and wall limits inside the worker harness. For each cell the harness creates the bounded process/cgroup, measures worker CPU and monotonic wall, kills that cell's process tree at either limit, and publishes the typed result before deciding shard outcome. The coordinator never substitutes an outer timeout for per-cell enforcement. Missing worker CPU accounting is an infrastructure failure for calibrated tests, not zero CPU.

Four deadlines have different owners:

| Deadline | Owner and derivation | On expiry |
|---|---|---|
| Per-cell CPU and wall | Worker harness; unchanged manifest budgets and multipliers | Kill cell cgroup/process tree; emit typed timeout result; continue or stop according to existing harness policy |
| Buck test-action wall | Buck test rule; `ceil(sum(cell wall limit × maximum attempts) / worker_count) + max(cell wall limit) + setup/result-upload grace` | Cancel action; wrapper trap kills the whole shard tree and finalizes an infrastructure-attempt manifest if possible |
| Validation DAG-node wall | DAG runner; conservative bound from shard-action limits and configured remote in-flight width, plus bounded RE queue, download/materialization and cancellation grace | Terminate the Buck client and require remote-operation cancellation acknowledgement; missing acknowledgement is infrastructure failure |
| Whole validation run | Validation driver/operator policy; greater than the constructed DAG's critical-path deadline plus receipt-finalization grace | Cancel DAG, then Buck client, then RE operations; receipt records unfinished actions and no success is publishable |

The normal cancellation chain is whole run → DAG node → Buck client → RE operation → worker wrapper → cell/shard cgroup. Every layer gets a shorter deadline than its parent so it can return evidence before the parent expires. SIGTERM/interrupt and forced-timeout probes must show no live Buck command, remote operation, Hermit descendant or writable result session afterward. A Buck action timeout does not fabricate per-cell timeouts for cells that never produced results; those remain missing/infrastructure outcomes.

The DAG runner should classify a Buck RE request as `remote-dispatch`: reserve realistic local Buck daemon/materialization CPU and memory plus a separate remote-inflight quota. It should not reserve the legacy 32 CPUs/64 GiB merely because the equivalent Cargo node did. It also must not claim zero resources: digesting, uploads and materialization can saturate CPU, RAM and disk.

Recalibrate only after at least three cold and three warm observations per mode on a fixed worker class. Use distribution-aware limits (at minimum median/range and p90/p95 where sample size permits), preserve a safety margin, and version limits with platform/toolchain. Recalibration may change a budget only through an explicit manifest review; it may not silently widen timeouts to make RE pass.

## DAG width

Do not raise the global default above 16 as the first move. One broad Buck request exposes all target/action parallelism to Buck and the RE scheduler without increasing DAG width. Buck's local thread count governs local action pressure, not remote concurrency, so it is not a substitute for an explicit remote-inflight policy.

There is a later opportunity to overlap one `remote-dispatch` request with unrelated local checks. Instrument queue delay, client CPU/RSS, CAS bandwidth and materialization first; then sweep widths 16, 24 and 32 with fixed remote limits. Adopt a higher Buck-mode width only if repeated full runs improve p95 wall time without increasing failures, memory pressure or materialization contention. If Phase 4 temporarily retains multiple per-bucket DAG nodes, the existing width already queues the roughly fourteen E2E nodes quickly; measure scheduler idle time before changing it.

## Alternatives considered

| Design | Benefit | Cost | Decision |
|---|---|---|---|
| One opaque harness invocation as one Buck test | Small integration diff | One schedulable action; poor attribution/cache/retry; mostly wraps existing harness | Smoke test only |
| One target per E2E cell | Maximum scheduling/cache granularity | Huge generated graph; parity coupling and result management become fragile | Defer until bucket-shard data justifies it |
| One target per bucket, with stable shards for large buckets | Matches present ownership and ledger; exposes useful parallelism | Requires generator and adapter | Recommended first migration |
| Remote-build/local-run hybrid | Captures hermetic compilation/cache benefits without privileged worker assumptions | Does not offload Hermit runtime CPU | Recommended durable baseline |
| Replace resource bundle with Buck immediately | Single build system | Current Buck graph does not own/verify all backends/resources | Reject for initial switch |

## Rollout and rollback

Each phase lands behind an explicit selector. The default remains Cargo/local until its phase gate passes. Receipts must make mixed artifacts impossible to mistake for one build. A rollback changes the selector, not files in `target/`.

Suggested change sequence:

1. fresh complete current-main Cargo profile with retained plan, journal and cell evidence;
2. metrics/event-log collector and RE platform/toolchain prototype;
3. feature/provenance-equivalent release target and parity test;
4. stable selected-artifact producer, attestation negative tests and Cargo/Buck DAG modes;
5. full A/B receipts and default switch;
6. hermetic Buck-test bridge, manifest-to-target generator and local canary;
7. `c_programs` shards, then remaining buckets by measured CPU;
8. test-specific worker profiles, capability targets and selective remote execution;
9. width/resource sweep and optional tuning.

## Blocking inputs and next experiment

RE feasibility remains unexecuted. The experiment cannot demonstrate a remote build or measure its speed until an endpoint, credentials/use case, CAS/action-cache policy and worker image are selected and available. These are unresolved experimental inputs, not evidence that RE will or will not work; credentials must be injected privately and do not belong in the repository.

Before starting performance implementation, obtain the Phase 0A fresh complete current-main local profile. The newest complete retained run is divergent and the near-current run failed after its build prefix. The minimum decision package is:

- one complete current-main Cargo run with node profiles and E2E summaries;
- the four Phase 0 Buck build cases;
- three cold and three warm complete Cargo-versus-Buck-selected validation pairs;
- Buck critical-path/action-kind/materialization reports;
- exact node/cell inventory comparison and artifact provenance.

## Source ledger

Repository evidence, all read at the SHAs named above:

- `hermit/.buckconfig:29-45` — default execution platform and daemon settings; no RE endpoints.
- `hermit/shim/BUCK:20-90` — host system C++/Rust toolchains and test toolchain declarations.
- `hermit/shim/tools/build_defs/testpilot_defs.bzl:9-16` — OSS test labels are empty and convey no remote profile.
- `hermit/hermit-cli/BUCK:14-106` — Buck metadata, dependencies and binary target.
- `hermit/hermit-cli/Cargo.toml:14-28` — Cargo `third-party-backends` includes DBT.
- `hermit/docs/BUCK2_OSS.md:24-42,95-115,132-150` — supported targets, pinned tools, system-toolchain constraint, reproduction status and absence of shared-cache measurements.
- Hermit commit `105ea982641f3a119fb82cb8af30417e9dbdc7f5` — cold 104.81-second Cargo release and 59.89-second narrower Buck target, explicitly scope-qualified.
- `hermit/.github/workflows/buck2-oss-nightly.yml:22-150` — one hosted job, isolated caches, host packages, absolute-rustc wrapper, diagnostic third-party build and Hermit gate.
- `hermit/scripts/validate.rs:588-724,5280-5305,7899-7908` — CLI, default DAG width and Cargo-only cache state.
- `hermit/ci/dag/validate.json` — current node graph and resource declarations.
- `hermit/ci/publish-hermit-e2e-artifact.sh:8-103` and `hermit/ci/run-with-hermit-e2e-artifact.sh:18-30` — content-addressed binary/resource contract.
- `hermit/tests/e2e/manifests/README.md:9-135` — manifest policy, partitions, budgets and host gating.
- `hermit/ci/manifest-plan/src/bin/test-harness.rs:2074-2337` — selection, typed results, retries, completeness and JUnit.
- `hermit/tests/BUCK` and `hermit/tests/helpers.bzl:15-167` — legacy generated Buck tests.
- Complete receipt/log/journal and partial CSV named in [measurements.tsv](measurements.tsv).
- [`extract-measurements.sh`](extract-measurements.sh) and [`source-extracts/`](source-extracts/) — bounded tracked evidence, original-artifact hashes and an executable comparison against every derived table value.

RE configuration examples inspected at fbsource revision `a60b3b9cd9f7a644f8d0f73c007a3548b5791b02`:

- `fbcode/buck2/prelude/tests/test_toolchain.bzl:11-25` — `TestToolchainInfo` contains only sanitizer metadata; the no-op toolchain does not run or collect tests.
- `fbcode/buck2/prelude/test/inject_test_run_info.bzl:14-38` — tests execute through `ExternalRunnerTestInfo`/`RunInfo`.
- `fbcode/buck2/prelude/toolchains/remote_test_execution.bzl:13-39` — named profiles and default profile/bundling selection.
- `fbcode/buck2/prelude/decls/re_test_common.bzl:11-62` — valid test RE fields, including `capabilities`, `local_enabled`, `remote_cache_enabled` and `use_case`.
- `fbcode/buck2/prelude/tests/re_utils.bzl:59-89,145-174` — `"disabled"` local sentinel, named-profile resolution, explicit local fallback and required `capabilities`/`use_case` validation.
- `fbcode/buck2/examples/remote_execution/internal/.buckconfig:4-24` — REAPI endpoints, digest, TLS, metadata and platform selection.
- `fbcode/buck2/examples/remote_execution/internal/platforms/defs.bzl:15-30` — hybrid executor, use case, strict output paths.
- `fbcode/buck2/examples/remote_execution/buildbuddy/.buckconfig:4-14` and `platforms/defs.bzl:15-34` — header-based authentication and container execution platform.

The earlier coordinator observed an 11m03s successful nightly Buck workflow. A fresh GitHub Actions API read was blocked by the host's destination filter, so that number is context, not an independently verified benchmark and is not used in any estimate above.
