# Every workflow gate in the repository — a map

Measured 2026-08-26 on `devbig014` by `agent(hermit-106)`, against `dev-hermit` `origin/main`
`bae1ec70e386257434b4da57afcffa56d57509d3` and `rrnewton/hermit` `main`. Task:
`map-every-workflow-gate-in-the-repository-report-in-ai-docs`.

Counts drift. Every number below has the command that re-derives it.

## The short answer

**79 gate-shaped things. Three of them can stop you.**

| layer | count | can it be walked past? |
| --- | --- | --- |
| `rrnewton/hermit` ruleset `main history protection` | **3 rules** | **NO** — server-side, 0 bypass actors |
| `rrnewton/hermit` ruleset `main check gating (admin-bypassable)` | 1 ruleset, **0 rules** | n/a — active and requires nothing |
| GitHub Actions workflows | 5 | never fires: all are `workflow_dispatch:` only |
| local git hooks | 4 active | yes — `--no-verify`, and only where `core.hooksPath` is set |
| operational-health tick gates | 35 | yes — every one emits text and blocks nothing |
| `ci-hub/bin` refusing wrappers | 17 | yes — bypassed by not calling the wrapper |
| `scripts/check*` checkers | 18 | yes — nothing invokes most of them automatically |

The only mechanism in this repository that a determined agent cannot walk past is three history
rules on `hermit` main. Everything else is a convention with a helpful reminder attached.

## Layer 1 — server-side, and the only enforcement that holds

    gh api repos/rrnewton/hermit/rulesets

**`main history protection`** (id 20307165), `enforcement=active`, **0 bypass actors**,
rules: `deletion`, `non_fast_forward`, `required_linear_history`.
Cannot be walked past. Stops branch deletion, force-push, and merge commits on `hermit` main.
This is why every landing squashes or rebases and rewrites the SHA.

**`main check gating (admin-bypassable)`** (id 20244443), `enforcement=active`, **`rules: []`**.
Active and empty: it requires no check at all. The name states an intent the object does not
implement. Anyone reading the ruleset list sees "check gating … active" and concludes checks are
gated.

**`repos/rrnewton/hermit/branches/main/protection` returns 404 Branch not protected.**

**`rrnewton/dev-hermit` has no server-side enforcement available at all** — both the protection
and ruleset endpoints return HTTP 403, *"Upgrade to GitHub Pro or make this repository public"*.
The parent is main-only with direct pushes and nothing server-side can check them. Worth stating
plainly because several parent-side gates below are written as if something downstream will catch
a mistake, and nothing will.

## Layer 2 — GitHub Actions: five workflows, none of which fires

    for f in .github/workflows/*.yml; do grep -A3 '^on:' "$f"; done

| workflow | trigger |
| --- | --- |
| `compat-envelope.yml` | `workflow_dispatch:` |
| `demo-review-gate.yml` | `workflow_dispatch:` |
| `dev-hermit-ci.yml` | `workflow_dispatch:` |
| `nightly-demo-sweep.yml` | `workflow_dispatch:` |
| `portability.yml` | `workflow_dispatch:` |

**All five are manual-only.** None has `push:` or `pull_request:`. This is the owner directive
that CI does not run automatically, and it is correctly implemented — but see Layer 5, because
two gates still demand Actions evidence that this guarantees will not exist.

## Layer 3 — local git hooks: the only thing that blocks a commit, and it is opt-in

`core.hooksPath` on the coordinator's checkout is `/home/newton/work/dev-hermit/.git/dev-hermit-hooks`,
a directory of symlinks to `.githooks/dev-hermit-hook-dispatch`, installed by
`scripts/setup-hooks`.

| hook | invokes | blocks |
| --- | --- | --- |
| `pre-commit` | `scripts/primary_checkout*`, `scripts/setup-hooks` | yes |
| `commit-msg` | `scripts/check_commit_attribution.py`, `scripts/check-demo-review.sh` | yes |
| `pre-push` | (dispatch only) | yes |
| `reference-transaction` | (dispatch only) | yes |

Two ways past, both ordinary:

1. **`git commit --no-verify`** skips every one.
2. **`core.hooksPath` is local config, not repository content.** A fresh clone, or any checkout
   where `scripts/setup-hooks` was never run, has no hooks at all. Linked worktrees inherit it
   (shared config); separate clones do not — and this workspace has several separate clones under
   `/home/newton/temp`.

The commit-attribution trailer requirement (`[<role>, <model>] [<team-slug>, devbigNNN]`,
`scripts/check_commit_attribution.py:33-38`) is enforced *only* here. Its role vocabulary is a
closed set of four — `impl agent`, `adversarial-reviewer agent`, `coordinator`, `Human` — and
`role=impl` from the subject disclosure tag is **not** one of them.

## Layer 4 — the 35 operational-health tick gates: none of them blocks anything

`ci-hub/health/tick-hub.yaml`, one entry per gate. Re-derive with
`grep -c '^  - name:' ci-hub/health/tick-hub.yaml`.

**26 emit `kind: action` with `skill: hard-warn-coordinator`. 9 emit `kind: note`.** Both are
text on stdout for a coordinator or automation to read. A tick gate cannot stop a commit, a push,
a merge, or a validate. Its entire power is that somebody reads the line.

| line | gate | emit |
| --- | --- | --- |
| 57 | `merge_gate_refire_due` | action |
| 111 | `slot_agent_parity` | action |
| 140 | `deleted_tool_absent` | action |
| 152 | `worktree_disk_residue` | action |
| 208 | `pr_tip_reachability` | action |
| 222 | `ledger_spool_backlog` | action |
| 255 | `ledger_publish_sweep` | note |
| 280 | `ledger_referenced_objects` | action |
| 298 | `validate_wall_regression` | action |
| 314 | `validate_wall_series_readiness` | action |
| 327 | `owner_tooling_directives` | action |
| 357 | `tick_code_currency` | action |
| 397 | `unpushed_change_clone_commits` | action |
| 414 | `unpushed_parent_commits` | action |
| 433 | `activation_obligations` | action |
| 447 | `speculative_land_obligations` (removed 2026-08-26) | action |
| 484 | `speculative_land_obligation_decision_waits` (removed 2026-08-26) | note |
| 522 | `primary_checkout_snapshot` | action |
| 550 | `reverie_pin_lag_note` | note |
| 562 | `reverie_pin_lag_escalation` | action |
| 575 | `github_main_health` | note |
| 595 | `hermit_merge_gate_authority` | action |
| 608 | `pull_request_health` | note |
| 679 | `drain_verdict_published` | action |
| 699 | `ci_queue_health` | note |
| 721 | `ci_lane_saturation` | action |
| 781 | `agent_container_lifecycle` | action |
| 832 | `pr_head_never_evaluated` | action |
| 860 | `approval_no_longer_binds_to_head` | action |
| 887 | `approvals_unverifiable_against_head` | action |
| 920 | `landing_observability` | note |
| 955 | `gate_runner_presence` | action |
| 1006 | `btrfs_headroom` | action |
| 1039 | `gate_set_integrity` | action |
| 1067 | `task_reference_hygiene` | note |

⚠️ **The whole layer sits behind one line that was broken.** `ci-hub/bin/health-tick:16` invoked
`operational_health.py cache-agents`, a subcommand deleted at `e5864b405` on 2026-08-25. The
script runs under `set -euo pipefail`, so `argparse`'s exit 2 **aborted the tick before any gate
ran** — measured `exit 2, 0 stdout lines, first gate reached 0 times`. Fixed in `79b1c57a8`. It
was invisible because the call sits behind `if [[ -n ${HERMIT_AGENT_SNAPSHOT_JSON:-} ]]`: a bare
`./ci-hub/bin/health-tick` skips it and looks healthy, while the ORC production path
(`.orc/plugins/hermit-dev/index.ts:112`) sets the variable. The tick that ran on a timer was the
one that died; the tick you would run by hand to check was the one that could not fail.

## Layer 5 — what these gates demand that we deliberately turned off

**Two gates require GitHub Actions runs that the owner directive guarantees will never exist.**

`approval_binding.py` (tick lines 860, 887) prints `actions_enabled=true` and flags every
approval with `runs=0` as `*** APPROVED + UNEVALUATED ***`. With all five workflows on
`workflow_dispatch:`, `runs=0` is the expected steady state, not a defect. Measured this
afternoon: **7 of 8 labels flagged, all 7 carrying that marker.**

The same file already contains the other branch — `zero workflow runs are not classified as
unevaluated`, reached when Actions is disabled. The lever exists; `actions_enabled` is simply
true.

`unevaluated_head.py` (tick line 832, `pr_head_never_evaluated`) rests on the same premise.

**Separately, and not caused by `runs=0`:** the same gate reports `REFUSED_STALE` for approvals
that bind the exact head, because an older un-withdrawn refusal exists. Of 7 refused rows: 1
genuinely stale (`#2694`), 1 correctly blocking (`#2683` codex, outstanding changes-requested),
**5 false**. Filed as `approval-gate-cannot-be-cleared-by-approving-correctly`. Two independent
problems under one alarm; turning `actions_enabled` off clears all 7 markers and zero
`REFUSED_STALE`.

## Layer 6 — `ci-hub/bin` wrappers that refuse (17)

`agent-tool`, `changes-requested-gate`, `claim-edit`, `close-task`, `gh`, `gh-api`,
`gh-merge-verified`, `git-push-verified`, `publication-exempt`, `review-attest`, `tg-dup-scan`,
`tg-guard-shim`, `tg-note-verified`, `who-am-i`, plus three `*-test` harnesses.

These refuse at the point of use, which means **they are bypassed by not calling them**. At this
report's 2026-08-26 measurement, two were installed as PATH shims and therefore harder to miss:
`tg-guard-shim` was copied to `/home/newton/orc-bin/tg`, position 3, ahead of `/usr/local/bin` at
position 20. That dated observation is not a continuing deployment guarantee; resolve the live
command and compare the copy before claiming enforcement, and note that explicit
`/usr/local/bin/tg` bypasses any PATH shim. The rest are opt-in by typing the wrapper name instead
of the bare tool. `gh-merge-verified` in particular cannot intercept a plain `gh pr merge`.

## Layer 7 — `scripts/check*` (18)

`check-agent-slot.sh`, `check-agent-utils-pin.rs`, `check-append-only-prefix.py`,
`check-codex-setup.py`, `check_commit_attribution.py`, `check-demo-review.sh`,
`check-gitlink-currency.sh`, `check-harness-help.py`, `check_hermit_agent_utils_pin.py`,
`check-ignored-experiment-artifacts.sh`, `check-parent-gitmodules.sh`, `check-portable-paths.sh`,
`check_reverie_pin_invariant.py`, `check-worktree-registry.rs`, and four test/setup companions.

Only two have an automatic caller: `check_commit_attribution.py` and `check-demo-review.sh`, both
from `commit-msg` — and therefore both subject to `--no-verify` and to `core.hooksPath` being set.
The rest are run by hand or not at all. `hermit`'s own
`scripts/core-review-protocol-lint.sh` is the documented example of this shape: `AGENTS.md` was
corrected on 2026-08-26 to say plainly that nothing runs it and nothing blocks a landing that
skips it.

## Whether anyone asked for it

Provenance was read from each tick gate's own comment block — the only durable record attached to
the gate itself.

| stated provenance | gates |
| --- | --- |
| names an owner directive | 9 |
| names a task or pull request | 3 |
| cites a measurement only | 9 |
| **no stated provenance** | **14** |

The 14 with nothing recorded: `ledger_spool_backlog`, `validate_wall_regression`,
`validate_wall_series_readiness`, `unpushed_change_clone_commits`, `unpushed_parent_commits`,
`activation_obligations`, `reverie_pin_lag_note`,
`reverie_pin_lag_escalation`, `github_main_health`, `hermit_merge_gate_authority`,
`ci_queue_health`, `approval_no_longer_binds_to_head`, `btrfs_headroom`.

This is a measure of what the file records, not proof nobody asked. A gate may have been
requested in a conversation that left no trace here — which is itself the point: for 14 of 35
there is no way to answer the owner's question from the repository.

## What the categorisation shows

1. **The enforcement is three history rules, and everything else is advisory.** 76 of 79
   gate-shaped things can be walked past by an agent who does not want to be stopped. That is not
   necessarily wrong — this fleet runs on agents choosing to hold a line — but it should be a
   decision rather than a discovery.
2. **The one ruleset whose name promises gating implements nothing.** `main check gating` is
   `active` with `rules: []`.
3. **Two gates demand evidence the owner directive guarantees cannot exist**, and the lever to
   stop that is already in the code.
4. **The 35-gate layer had a single point of failure at `health-tick:16`** that silently disabled
   all of it, and the guard on that line made the failure invisible to a manual check.
5. **For 14 of 35 gates the repository cannot say who asked for them.**

Nothing above is a recommendation. The owner asked for the map.

## Re-derivation

    R=<a clean detached worktree at dev-hermit origin/main>
    ls $R/.github/workflows/*.yml                                   # Actions
    grep -A3 '^on:' $R/.github/workflows/*.yml                      # triggers
    grep -c '^  - name:' $R/ci-hub/health/tick-hub.yaml             # tick gates
    grep -n '^  - name:\|kind:\|skill:' $R/ci-hub/health/tick-hub.yaml
    ls $R/.githooks/ ; git config core.hooksPath                    # hooks + install
    ls $R/scripts/check*                                            # checkers
    with-proxy gh api repos/rrnewton/hermit/rulesets                # server-side
    with-proxy gh api repos/rrnewton/hermit/rulesets/<id>
    with-proxy gh api repos/rrnewton/hermit/branches/main/protection

⚠️ Read a ruleset's `rules` array, not its name. `main check gating (admin-bypassable)` reads as
enforcement in every listing and enforces nothing.
