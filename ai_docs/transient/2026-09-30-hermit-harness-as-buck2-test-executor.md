# Hermit e2e harness as a buck2 test executor: analysis (2026-09-30)

Owner question: buck2's test runner can be replaced, so could the e2e compat
matrix run through our own buck2-compliant runner? What would it take?

Sources. buck2 is `facebook/buck2` at `main`, read on 2026-09-30. Hermit pins
the **2026-08-01** buck2 release (`bootstrap/buck2`), so the protocol details
below need rechecking against that tag. Hermit is `gh/main` `de130c044`.
Citations marked **[P]** are proven from source. **[I]** marks inference.

## 1. How buck2 launches a test executor

- **[P]** There is no `--test-executor` CLI flag. `buck2 test` reads buckconfig
  `test.v2_test_executor` from the root cell. `$BUCK2_BINARY_DIR/` is expanded.
  If the key is unset, buck2 runs its own `buck2 internal-test-runner`
  (`app/buck2_test/src/command.rs`, around line 406; `post_process_test_executor`).
  A per-invocation `-c test.v2_test_executor=...` override therefore works.
- **[P]** The executor is spawned with the configured args followed by
  `--buck-trace-id <id> --config-entry host=linux ...
  --executor-fd N --orchestrator-fd M -- <args after buck2 test -- ...>`.
  The two fds are Unix socketpairs carrying gRPC. `BUCK2_TEST_TPX_USE_TCP`
  switches to TCP (`--executor-addr/--orchestrator-addr`). Stdin is `/dev/null`
  (`app/buck2_test/src/unix/executor.rs`, `executor_launcher.rs`,
  `app/buck2_test_runner/src/{unix,tcp}.rs`).
- **[P]** Tests go to RE only with `--unstable-allow-compatible-tests-on-re`
  or `--unstable-allow-all-tests-on-re`. Without them buck2 prefers local
  execution, but an explicit `default_executor` or `executor_overrides` entry
  can still route a test to RE (`orchestrator.rs` `executor_preference`;
  `docs/rule_authors/test_execution.md`).

## 2. The protocol (`app/buck2_test_proto/test.proto`) [P]

| Service | Implemented by | RPCs |
|---|---|---|
| `TestExecutor` | **us** | `ExternalRunnerSpec(spec)` is called once per test target as its build finishes. `EndOfTestRequests()` means no more specs. `Unstable_HeapDump`. |
| `TestOrchestrator` | **buck2** | `Execute2`, `ReportTestResult`, `ReportTestsDiscovered`, `ReportTestSession`, `EndOfTestResults(exit_code)`, `PrepareForLocalExecution`, `AttachInfoMessage`, `UploadFileToCas` |

`ExecuteRequest2` carries these fields:
- `test_executable`: stage (`Listing` or `Testing`), target handle, `cmd`/`env`
  as `ArgValue`s, which are verbatim strings or opaque artifact handles from
  the spec, and `pre_create_dirs` as `DeclaredOutput`s.
- `timeout` (per request).
- `host_sharing_requirements`: `ExclusiveAccess`, `Shared` with a weight, or
  `OnePerToken`.
- `executor_override` (by name).
- `required_local_resources`.
- `disable_test_execution_caching`.

`ExecutionResult2` returns the status (exit code or `timed_out`),
stdout/stderr, output entries, timings, max RSS, and the execution kind.

**What buck2 provides:** building every artifact and expanding argument handles
into paths. It also picks the local or RE executor from `default_executor` or
the override, then runs the command, enforces the timeout, and materializes
declared outputs locally even after RE execution (`orchestrator.rs` around 494-556,
1532-1700). **What the executor must do:** choose what to run, how often, and
with what timeout; decide pass or fail; report results; and send
`EndOfTestResults` with the run's exit code. `TestStatus` has PASS, FAIL, SKIP,
OMITTED, FATAL, TIMEOUT, UNKNOWN, RERUN, LISTING_*, and INFRA_FAILURE.

## 3. The OSS reference runner [P]

`app/buck2_test_runner` is 7 files, about **560 lines**, and `runner.rs` is 253 of
them. The `TestExecutor` implementation pushes specs onto a channel. The runner
calls `execute2` once per spec, 10,000 at a time, with the spec's command and
env plus `--test-arg`/`--env`. Exit 0 is PASS, any other exit is FAIL, and a
timeout is TIMEOUT. It ends with exit 0 or 32. It **ignores labels,
executor_overrides, local resources, host sharing and declared outputs**, and
uses **one global `--timeout` (default 600 s)** for every test. It never
reports SKIP. This is the fork base. Meta's internal runner is Tpx, which adds
translation, retries, listing and Test Infra reporting
(`test_execution.md`).

## 4. How a rule describes a test: `ExternalRunnerTestInfo` [P]

Fields (`app/buck2_build_api/.../external_runner_test_info.rs`):
- `type`
- `command`
- `env`
- `labels`
- `contacts`
- `use_project_relative_paths`
- `run_from_project_root`. In OSS both default to true; both are required for
  RE, otherwise `supports_re=false` (`orchestrator.rs` around line 1718).
- `default_executor`, a `CommandExecutorConfig` with RE `capabilities`,
  `use_case` and `local_enabled`
- `executor_overrides` (a map of named executors)
- `local_resources`, `required_local_resources`
- `worker`
- `supports_test_execution_caching`. Leave this false: a cached PASS would
  hide nondeterminism.

The spec the executor receives contains the target, `test_type`, command,
env, labels, contacts and oncall. The prelude's `prelude/tests/re_utils.bzl`
shows how `remote_execution = {capabilities, use_case, local_enabled, ...}`
props become `default_executor` and a `listing` override.

## 5. Mapping our harness onto it

Facts on our side **[P]**:
- `test-harness run` already selects one cell with
  `--test/--mode/--backend`, filters with `--lane portable|privileged`, and
  writes `--results` JSONL and `--junit`.
- Parity is a separate cross-cell post-pass
  (`test-harness parity compare --artifacts`).
- The manifest defines retry-once with the failed row kept, per-cell CPU and
  wall timeouts, `HOST-INAPPLICABLE` ("never a pass"), and typed verify reports.
- The ledger's `CellVerdict` has four states (compared-and-matched,
  compared-and-diverged, no-comparison-by-design, unavailable). It is combined
  with 4 failure classes (`failure-class-schema.json`).
- `ci/expected-e2e-plan.json` lists **900 CI cells**; the envelope has 5,984.
- Hermit already builds `//hermit-cli:hermit` under OSS buck2 (shadow-only,
  `docs/BUCK2_OSS.md`), and its BUCK files are the internal fbcode ones.

**Important:** `lane` is set per test, not per backend. **241 KVM cells are
`lane: portable`**, and only 7 CI cells are privileged. Lane alone cannot select
an RE platform. The mapping has to use lane, backend and `requires` together.

### Design A: our harness becomes the executor

A forked `hermit-test-executor` receives one spec per cell target. The cell
identity is carried in labels. For each spec it calls `execute2` with:
- the manifest's timeout,
- a `DeclaredOutput` for the cell's results and verify logs,
- `ExclusiveAccess` for PMU- or timing-sensitive cells,
- the executor override for its platform.

It retries once and maps `CellVerdict` to `TestStatus`, keeping the full
verdict in `details` or attached info. It reports inapplicable cells as SKIP,
runs the parity post-pass after `EndOfTestRequests`, checks the received
population against the manifest, and writes ledger rows.

| Pros | Cons |
|---|---|
| Faithful semantics: SKIP, INFRA_FAILURE, retries, per-cell timeouts | Depends on an unstable gRPC API (the `2` suffixes and `reserved` fields show churn). We must track buck2 versions or vendor protos. |
| The evidence files come back from RE through declared outputs | `buck2_test_api` appears not to be on crates.io [I], so we need a git dependency or tonic-generated protos |
| Parity and the completeness check live in one process | Internally Tpx is the executor; swapping it at Meta is unproven **[I]** |
| The executor can refuse a subset run itself (label filters, patterns) | A new long-lived component to maintain |

### Design B: stock executor, one plain test per cell

Each cell becomes an `ExternalRunnerTestInfo` target whose command is
`test-harness run --prebuilt --test X --mode M --backend B --results <dir>`.
It may later be a thinner single-cell binary. The verdict and retry stay
inside the wrapper, and a post-run ingester reads the results into the ledger.

| Pros | Cons |
|---|---|
| Almost no new protocol code; `test-harness` already runs a single cell | The stock runner has **no SKIP**, so `HOST-INAPPLICABLE` must become PASS (forbidden) or FAIL |
| Also works under internal Tpx as a `custom`-type test **[I]** | **No declared outputs**, so evidence cannot come back from RE; only stdout/stderr return, inside `details` |
| Low version-coupling risk | One global timeout; the parity post-pass and the completeness check move to the ingester |
| | Locally, results must go to an out-of-tree directory passed with `-- --env E2E_RESULT_ROOT=...` (non-hermetic) |

## 6. Work needed in hermit, estimates and risks

| Piece | A | B | Estimate |
|---|---|---|---|
| Target generator: manifest to generated `tests/e2e/BUCK` with one target per enabled cell, plus an audit that it equals `expected-plan` | yes | yes | 1-2 person-days |
| `hermit_e2e_cell` `.bzl` rule producing `ExternalRunnerTestInfo`: labels `lane:`, `backend:`, `mode:`, `requires:`, `cell:`; both RE flags; caching off | yes | yes | 1 day |
| Guests, fixtures and the hermit binary as Buck deps. Buck already builds C probes (`tests/c/BUCK`); `.sh` guests use host tools, which differ on RE images | yes | yes | **3-5 days** (largest) |
| Platform map from (lane, backend, requires) to executor: ptrace to RE only if the RE worker allows ptrace and hardware `perf_event`; KVM and privileged to a local executor | yes | yes | 1 day, plus RE qualification (unknown) |
| Ledger adapter from per-cell results JSONL to existing validate rows, with a population check | yes | yes | 1-2 days |
| Executor fork: fds and gRPC, execute2 with timeouts and outputs, retry, SKIP, parity pass, completeness check | yes | no | 4-7 days (about 1.5-3 k lines) |
| Single-cell wrapper hardening | no | yes | 1-2 days |

Totals: **Design B is about 8-13 person-days** and **Design A is about 11-18**. The
shared rows (about 7-11 days) carry over from B to A.

Risks:
1. **Protocol stability [I]:** the protocol is an internal gRPC API with no
   compatibility promise. Pin one buck2 release, vendor its three protos, and
   test the executor against every buck2 bump.
2. **OSS vs internal:** it is the same source tree **[P]**, but internal
   `buck2 test` always goes through Tpx, and the `fbcode_build`-only code
   passes Tpx extra args (`command.rs`). Replacing Tpx inside fbsource is
   probably not allowed by policy **[I]**. Internally, B under Tpx is the
   realistic route.
3. **RE capabilities for our workloads [I]:** ptrace inside RE containers
   (seccomp, YAMA), PMU counters for detcore's RCB timer (often missing in
   VMs), `/dev/kvm`, cgroup delegation for the harness's CPU caps (otherwise
   `--unsafe-no-cgroups`), and golden stdout that depends on host tool
   versions. Expect "portable" (GitHub-runner-able) to be a superset of what
   RE can run.
4. **Subset risk:** `buck2 test --include/--exclude` and target patterns can
   silently shrink the run. The executor (A) or the ingester (B) must compare
   against the manifest population and refuse.

## 7. Recommendation

Build the shared pieces first and run **Design B locally**: stock
`internal-test-runner` with no RE. That proves the generator, rule, deps and
ledger adapter while Cargo stays the default. Then build **Design A** once RE is
a real goal, because only a custom executor can return evidence from RE,
report SKIP honestly and run parity. Before any RE work, spend half a day
qualifying one RE worker for ptrace plus `perf_event_open` of a hardware
counter. If that fails, RE covers only the DBT and liteinst backends at most,
and the case for A becomes much weaker.

PROVEN items cite buck2 or hermit source above. INFERRED items are marked
[I]: internal Tpx swappability, protocol churn risk, RE ptrace/PMU
availability, and Tpx `custom`-type handling.
