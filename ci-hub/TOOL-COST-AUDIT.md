# Tool cost compliance audit

Audit date: 2026-08-03. Scope: executable operator/developer tools under parent
`scripts/`, `ci-hub/`, `hermit/{ci,scripts}/`, and `reverie/scripts/`. Tests and
small generated artifacts are not separate operator entrypoints.

## Compliant in this change

| Surface | Estimate | Final wall + CPU | Notes |
| --- | --- | --- | --- |
| Substantive `ci-hub/ci-hub` commands | explicit unknown | yes | Typed dispatch arms cost reporting only after argument parsing; help/version/usage and instant local status reads are intentionally silent. |
| `ci-hub/bin/main-health` | explicit unknown | yes | Repository count is stated, but no per-repository cost is invented. |
| `ci-hub/bin/pr-status` | explicit unknown | yes | Repository count is stated, but no runtime is invented; the default engine makes one rollup query plus at most 32 cached Actions-authority dereferences per repo (exact jobs and, for the one transitive contract, exact run/workflow binding), each bounded to 5s and 256 KiB. |
| `ci-hub/bin/health-tick` | explicit unknown | yes | Due gates vary and no tick history exists. |
| Any command launched through `ci-hub/bin/tool-cost` | derived or explicit unknown | yes | Shared `wait4` child-tree measurement and exit preservation; unknown persists as JSON `null`. |

## Numeric-claim sweep

| Violation | Replacement or disposition |
| --- | --- |
| `ci-hub` front door and direct wrappers printed invented wall/CPU constants, including a one-second `land-lock` estimate even though acquisition may queue and `run` includes an arbitrary child command. | Substantive operations use `wall=unknown cpu=unknown`, with an operation-specific `not measured:` basis, and measured actuals. Trivial control/status paths print no cost lines. |
| Speculative-land local validation used invented 30-minute wall / 2-hour CPU floors, including when no ledger rows existed. | With history: p90 of the last at most 50 usable successful full-profile ledger rows, with `n` in the basis. Without history: explicit unknown and JSON `null`, never a floor. |
| Full history refresh described “approximately 19000” runs and converted that constant into a four-hour estimate without counting or profiling the query. | Explicit unknown; the GitHub result count and runtime history do not exist before the query. |
| `hermit/scripts/validate.rs` prints a per-profile ETA. | Owned by `hermit-227b`: same-profile/cache/host ledger estimate with sample size/range, or explicit insufficient history. |
| `hermit/ci/power-to-weight.rs` displayed unmeasured DAG hints as `dur_s` and used a `>=120s` flag. | Hermit PR: unitless `declared_unmeasured_weight`; measured selection rate names exact commit window and `n`; the candidate rule is labeled a configured heuristic. |

The Hermit sweep also confirmed that selector/harness counts, stress progress,
compatibility and coverage percentages, and final validation durations are
computed from live inputs or results. `RR_COMPAT_EXPECTED=139` has recorded
provenance. Four other legitimate compatibility ratchets still need equivalent
source comments in the Rust validation driver:
`STRICT_COMPAT_TOTAL=191`, `SABRE_COMPAT_EXPECTED=207`,
`SABRE_COMPAT_TOTAL=212`, and `E9PATCH_COMPAT_TOTAL=155`.

## Assigned concurrently

| Tool | Owner/task | Required completion |
| --- | --- | --- |
| `hermit/scripts/validate.rs` | `hermit-227b` / `wire-affected-test-selection-and-measure` | History-derived warm/cold estimate; final wall + CPU on every exit. |
| Multisect tool | `hermit-231b` / `multisect-tool-design` | `(build + run) x repetitions x ceil(log2(commits)) / parallelism` wall estimate, summed CPU estimate, final wall + CPU. |

## Follow-up list

Every row below lacks both required outputs unless noted.

| Priority | Tool(s) | Missing | Concrete estimate basis |
| --- | --- | --- | --- |
| P0 | `scripts/super-validate.sh` | estimate + actual | Selected scope, stress repetitions, historical per-demo/per-gate cost, parallelism. |
| P0 | `hermit/ci/run-dag.sh`, `hermit/ci/run-node.sh`, `hermit/ci/test_harness.sh` | estimate + actual | Selected DAG critical path and summed node baselines; warm/cold artifact state. |
| P0 | `scripts/e2e-union-rebase.sh`, `scripts/e2e-union-resolve.py` | estimate + actual | PR/manifest row count x measured conversion/rebase cost; network retry allowance. |
| P0 | `hermit/scripts/stress-test.sh`, `hermit/scripts/hermit-code-coverage.rs` | estimate + actual | Iterations/program count x measured run/build cost divided by jobs. |
| P1 | `scripts/detached-verify.rs` | estimate + **CPU actual** | It reports wall duration today; add caller estimate and `wait4` CPU. |
| P1 | `scripts/run_experiment.sh`, `scripts/prepare-demo08-assets.sh` | estimate + actual | Repetitions/assets/download bytes and build/run history. |
| P1 | `scripts/install-deps.sh`, `scripts/checkout-optional-submodules.rs`, `scripts/submodules.sh` | estimate + actual | Missing submodule/dependency count, expected download bytes, warm/cold state. |
| P1 | `scripts/allocate-worktree.rs`, `scripts/release-worktree.rs`, `scripts/worktree-gc.sh`, `scripts/slot-init.sh` | estimate + actual | Product count, checkout/cache size, removal count. |
| P1 | `scripts/pr_conflict_graph.py`, `hermit/scripts/pr-dag-health.sh`, `hermit/scripts/pr_status.py` | estimate + actual | Open PR count, pair count, and GitHub API request count. |
| P1 | `hermit/ci/power-to-weight.rs`, `hermit/ci/select-tests.rs` | estimate + actual | Candidate/path/node counts; history scan size. |
| P1 | `hermit/scripts/progress-report.sh`, `hermit/scripts/compat-map.sh`, `hermit/scripts/manifest-to-commands.rs` | estimate + actual | Manifest/test count and invoked probe count. |
| P1 | `hermit/scripts/stage-liteinst-runtime.sh` | estimate + actual | Build profile, cache state, artifact size. |
| P2 | `scripts/primary_checkout.py`, `scripts/doctor.sh`, `scripts/resource_audit.sh`, `scripts/verify-slot-pushed.sh` | estimate + actual | Checkout/slot/process counts and remote queries. |
| P2 | `scripts/lint-memory-skill-sync.rs`, `scripts/sync-memory-skill.rs`, `scripts/check-demo-review.sh`, `scripts/check-parent-gitmodules.sh` | estimate + actual | Skill/file/diff counts. |
| P2 | `ci-hub/bin/agent-tool` and direct `ci-hub/runners/*` substantive commands | estimate + actual | Wrapped through the front door where available; direct invocation still lacks the contract. `landing-lock.sh` now execs the typed front door. |
| P2 | `reverie/scripts/backend-submodule.sh`, `reverie/scripts/dump-vdso.py` | estimate + actual | Backend/submodule count or input image size. |

Fast syntax-only checkers need cost reporting only when they scan enough input
to affect a caller's plan. Help/version/usage and instant local status reads are
outside the convention by design; substantive failures and early exits still
require the final actual line.
