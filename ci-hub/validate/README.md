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

`ci-hub validate-status` renders run timestamps as compact ages by default
(for example, `1h36m ago`) so stale results are visible at a glance. Use
`--times local` or `--times utc` when an absolute timestamp is needed. JSON
output retains the recorded RFC 3339 timestamps in every mode.

With no SHA, PR, or run number, the recent table includes completed ledger rows
from every branch, ordered by completion time; it is not restricted to
`origin/main`, and live in-progress runs may also appear. Use
`--commit-timeline` for the `origin/main`-only view. The
`COMMITS-SINCE-GREEN` line and its JSON object name the exact local history ref
and full tip SHA used for the first-parent depth. This query deliberately does
not fetch, and the named tip may differ from the checkout's `HEAD`.

Use `ci-hub validate-status --log-paths` when following a running or completed
validation. It adds the producer-recorded `SLOT` and full `LOG-FILE` columns;
they stay out of the default table because the path is intentionally wide.

Each run admitted by `validate-lock` receives the next RUN number for that
repository on the current machine before its child starts. The current run
handle's admitted result carries it while the run is in progress, and the
completed ledger row carries the same number. `ci-hub validate-status --run-no
NUMBER` looks up that run on the current machine. RUN is an identity, not proof
that a completed receipt exists: a run that fails before writing a ledger row
can leave a gap.

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

Disposable checkouts and private Cargo homes are created under
`worktrees/validate/`, never under `ignored/`. A normal waiter removes them at
completion; `ci-hub validate-run --sweep-completed` retries cleanup for
terminal runs whose waiter was interrupted, including its recorded private
Cargo home. Before removing a checkout, the sweep copies any untracked `*.jsonl` receipt to
`ignored/validate/orphaned-receipts/<unit>/`.

`ci-hub validate-run` prints `E2E_RESULTS` and `RUNNER_LOGS` paths for Hermit
runs and records the same paths as `e2e_result_root` and
`safe_ci_dag_runner_log_dir` in `ignored/validate/runs/<unit>.json`. They live
under `ignored/validate/artifacts/<unit>/`, outside the fresh checkout that is
removed after validation.

After a canonical Hermit receipt is readable, the waiting `validate-run`
invocation merges that run's durable per-cell results into the source
checkout's `SCORECARD.md` and `ci/compat-envelope/cells.json`, then prints the
exact `git diff` command for review. It does this for passing and failing
receipts and reports an empty per-cell result set as unchanged. The write is
refused if the source checkout moved away from the validated commit or acquired
tracked changes while the detached service was running. That refusal does not
change the receipt's verdict, but a green `validate-run` exits nonzero so the
missing update cannot read as overall success. An interrupted caller leaves the
systemd service running; `validate-run --attach <unit>` performs the same update
when it later returns the completed run to an invoker.

Current run handles retain wire schema 1 so already-deployed readers can
continue observing a run started by newer code. Their complete contract is
owned by `run_registry.py`, and the `producer` field names that file so readers
can distinguish them from historical schema-1 handles (including twelve older
manual benchmark rows that already carry `kind`). Every current row records the
existing `validate-lock` `kind`
(`validate`, `reverie-validate`, `frozen-validate`, or `bench`), so the state
`failed` used by the pressure-test path cannot silently acquire the meaning of
an ordinary validation state. Python readers validate through `read_record`;
the Rust validate-lock and validate-status consumers ask the same authority for
their typed projections. Other schema-1 handles remain readable only as
historical input so a process started before deployment can finish without being
rewritten into the current contract.

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
