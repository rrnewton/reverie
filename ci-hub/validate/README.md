# `validate/` — local validation run visibility

**Follow skill `hermit-validation-authority` for Hermit authority and Reverie's
`reverie-validation-authority` skill for Reverie rows.**

This object turns the records that `hermit/scripts/validate.rs` writes into readable,
machine-wide views. It owns the linkage from local run records to retained
profiles; it does **not** own the generic DAG runner or the profile format
(those belong to the pinned `safe-ci-dag-runner`).

Two consumers live here:

| Command | Reads | Answers |
| --- | --- | --- |
| `validate/aggregate.py` | Every validation run on this machine (parent JSONL ledger + raw per-run logs + `safe-ci-dag-runner` profiling CSVs). | "What has every worktree/agent/slot validated, and where is the profiling?" |
| `validate/worktrees.py` | This hub's two report artifacts under parent `ignored/ci-hub/`. | "Which worktrees are registered with this hub, how fresh is each, and how did their recent runs go?" |

Both are read-only. The front door exposes them as `ci-hub local-history` and
`ci-hub validate-worktrees`.

Running `ci-hub validate-run` again for an unchanged commit starts a fresh
measurement. This is intentional: repeated runs expose intermittent failures
and provide the independent observations used by the basic-sanity scorecard.
The newest run does not erase older receipts; `validate-status` and
`local-history` retain the run history.

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
`ci-hub-reverie-validate-run` row that carries exact repository, seven-gate,
test-coverage, lock, and safe-ci evidence. `validate-status` filters by
repository before resolving a SHA, so a same-text Hermit SHA cannot satisfy a
Reverie query.

## Hub-report artifacts

`hermit/scripts/validate.rs` walks up from its checkout root through at most three parent
directories looking for `ci-hub/ci-hub`. It fails before validation when the hub
or its required store dependencies are missing, so a run cannot silently lose
its receipt. A standalone product checkout may pass `--allow-no-ci-hub` (or set
`VALIDATE_ALLOW_NO_CI_HUB=1`), which emits a loud warning; dev-hermit agents must
not opt out. Both artifacts land under parent `ignored/ci-hub/`, gitignored by
the repo-wide `**/ignored/` rule.

### `ignored/validate-runs.jsonl`

Append-only JSONL, one record per run, byte-identical to the line the same run
writes to the parent validate-run ledger (`build_validation_record`). Runs are
events and are never deduplicated. Records are `schema_version` >= 3; the fields
that matter to `worktrees.py` are:

The current JSONL record written by VALIDATE includes the exact HEAD commit,
whether the tree matched that commit, and what selection actually ran. Staged
work or the explicit `--run-on-dirty-tree` escape can still run, but its record
says `commit_anchored=false`. Skill `hermit-validation-authority` defines the evidentiary
consequence; this README only documents what the producer stores.

| Field | Meaning |
| --- | --- |
| `finished_at` | UTC ISO-8601 completion time. |
| `slot` | Worktree slot (e.g. `ci`, `kvm`, `slot03`). |
| `profile` | `quick` / `portable-only` / `full` / `super`. |
| `selection_mode` | Actual selection outcome: `full`, `smart`, `selective`, `skip`, `only`. Records the outcome, not the intent. |
| `result` / `exit_code` | `pass`, product `fail`, bounded `timeout`, signal `killed`, or harness `incomplete`, plus the process exit code. |
| `checks` / `failures` | Gate counts. |
| `real_seconds` / `user_seconds` / `sys_seconds` | Wall and CPU time, all in seconds. |
| `product_failures` / `killed_by_bound` / `killed_by_signal` / `incomplete_gates` | Explicit terminal attribution; resource kills never collapse into product failure or incomplete. |
| `commit` / `commit_anchored` / `tree_dirty` | Validated commit and its anchoring state. |

`selection_mode` never says `full` unless the complete suite actually ran.
Qualification and label derivation follow skill `hermit-validation-authority`; the field
definition here is not another policy predicate.

### `ignored/worktree-registry.json`

A JSON object keyed by absolute worktree path, exactly one entry per worktree.
It is upserted to `running` before gates begin and finalized from the EXIT trap,
so a detached run remains attributable after its launching agent exits. Missing
`jq`, `flock`, or a writable store refuses the run rather than skipping the
receipt. Each entry:

```json
{
  "/abs/path/to/worktrees/ci/hermit": {
    "path": "/abs/path/to/worktrees/ci/hermit",
    "repo": "hermit",
    "branch": "codex/validate-smart-selection",
    "slot": "ci",
    "first_seen": "2026-08-01T09:00:00Z",
    "last_seen": "2026-08-03T17:00:00Z",
    "last_seen_epoch": 1785000000,
    "state": "pass",
    "active_pid": null,
    "current_started_at": "2026-08-03T16:45:00Z",
    "last_commit": "3370d9c8c1caa85012ae10199293543c8871a5b9",
    "last_result": "pass",
    "last_exit_code": 0,
    "last_profile": "full",
    "last_selection_mode": "smart",
    "commit_anchored": true,
    "tree_dirty": false
  }
}
```

`first_seen` is preserved across runs; identity includes absolute path, slot,
branch, exact SHA, full profile, selection mode, and dirty/anchoring state.

## Consuming the reports

```bash
# Registered-worktree table (newest last; worktrees unseen > 24h flagged STALE).
./ci-hub/ci-hub validate-worktrees

# Add the 10 most-recent hub-reported runs.
./ci-hub/ci-hub validate-worktrees --runs 10

# Tighten the staleness threshold to 6 hours.
./ci-hub/ci-hub validate-worktrees --stale-hours 6

# One machine-readable report (worktrees + runs + counts).
./ci-hub/ci-hub validate-worktrees --json

# Point at a non-default artifact directory (also honors $CI_HUB_IGNORED_DIR).
./ci-hub/ci-hub validate-worktrees --data-dir /path/to/ignored/ci-hub
```

The reader tolerates a missing historical artifact or truncated JSONL line and
prints only what is present, so it is always safe to run. New producers fail
closed before creating those gaps.

## Why a consumer matters

Without a reader, `validate-runs.jsonl` and `worktree-registry.json` are
write-only: they accumulate but nothing surfaces them, so the capability exists
and nobody can use it. `worktrees.py` closes that loop, turning the artifacts
into a live map of which worktrees exist, how fresh each is, and how their most
recent validate runs went.
