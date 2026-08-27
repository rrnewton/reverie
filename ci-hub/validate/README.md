# `validate/` — local validation run visibility

**Follow skill `hermit-validation-authority` for Hermit authority and Reverie's
`reverie-validation-authority` skill for Reverie rows.**

This object turns the records that `hermit/scripts/validate.rs` writes into a readable,
machine-wide history. It owns the linkage from local run records to retained
profiles; it does **not** own worktree status, the generic DAG runner, or the
profile format (the latter two belong to the pinned `safe-ci-dag-runner`).

The retained-history consumer is:

| Command | Reads | Answers |
| --- | --- | --- |
| `validate/aggregate.py` | Every validation run on this machine (parent JSONL ledger + raw per-run logs + `safe-ci-dag-runner` profiling CSVs). | "What has every worktree/agent/slot validated, and where is the profiling?" |

The front door exposes it as `ci-hub local-history`. Worktree status belongs to
the wrkslots infrastructure rather than a second report in ci-hub.

Running `ci-hub validate-run` again for an unchanged commit starts a fresh
measurement. This is intentional: repeated runs expose intermittent failures
and provide the independent observations used by the basic-sanity scorecard.
The newest run does not erase older receipts; `validate-status` and
`local-history` retain the run history.

If freshly fetched `origin/main` has moved beyond that exact Hermit SHA, use
`ci-hub validate-run --frozen-validate ...`. The ordinary path still refuses a
target that does not contain current main. The explicit frozen path forces a
real rerun (`--ignore-cache`), disables receipt-backed PR labeling, and writes
its result to `ignored/validate/frozen/<unit>.jsonl` rather than the canonical
ledger. Both the run handle and that result name the target and the current-main
SHA observed before launch, set `measured_against_superseded_tip=true`, and set
`qualifying_receipt=false`. It is repeatability evidence for that exact tree,
never a claim that the current tip passed.

## Per-run output that survives the fresh checkout

`ci-hub validate-run` prints `E2E_RESULTS` and `RUNNER_LOGS` paths for Hermit
runs and records the same paths as `e2e_result_root` and
`safe_ci_dag_runner_log_dir` in `ignored/validate/runs/<unit>.json`. They live
under `ignored/validate/artifacts/<unit>/`, outside the fresh checkout that is
removed after validation.

`E2E_RESULTS` contains the harness's per-cell JSONL and JUnit output, including
failure reasons. Compare its per-category cells with Hermit's existing
`ci/expected-e2e-plan.json`; that committed plan is the denominator that
distinguishes a complete run from a passing subset. `RUNNER_LOGS` contains the
incrementally flushed per-step logs and `journal.jsonl`, so a killed runner
still leaves the last observed step and test boundaries.

For schema-6 failed receipts, the ledger adapter validates the retained
`cell_results` artifact before classification and writes `failed_cells` as exact
`test/mode/backend` ids whose typed verdict is `compared-and-diverged`.
`flaky_failed_cells` is the subset present in the exact-tree generated
`flaky-cells.json`. A cell with `unavailable-with-reason` is not put in either
list, and its presence prevents an aggregate manifest failure from being called
known-flaky.

## Reverie centralized producer

`ci-hub validate-run --repo rrnewton/reverie --checkout <reverie> --agent
<agent> --target <exact-sha>` uses the same detached systemd service,
box-exclusive `validate-lock`, run registry, and ledger adapter as Hermit. Its
product command is always the exact full `./validate.sh --no-label-pr`, executed
as one `safe-ci-dag-runner` step. Cgroup containment is mandatory: the wrapper
passes neither `--allow-cgroup-failure` nor `--unsafe-no-cgroups`, and the node
itself proves both the safe-ci step cgroup and live validate-lock owner ancestry
before it runs the product driver.

Reverie events use the existing HistoryRow/JSONL wire format and shard to
`ledger/reverie/<host>/<month>.jsonl`. The direct schema-3 row emitted by
`validate.sh` remains diagnostic. The central wrapper emits the schema-5
`ci-hub-reverie-validate-run` row that carries exact repository, current
seven-gate v2, test-coverage, lock, and safe-ci evidence. The reader retains the
historical seven-gate v1 contract for old exact-SHA receipts. `validate-status` filters by
repository before resolving a SHA, so a same-text Hermit SHA cannot satisfy a
Reverie query.
