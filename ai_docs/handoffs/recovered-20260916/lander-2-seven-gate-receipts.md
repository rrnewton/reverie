# dev-hermit restart continuity

Last refreshed: 2026-08-03 07:25 PDT.

This tracked file is the restart manifest for the development fleet. A hard
restart loses agent conversations, so recovery must come from TaskGraph notes,
registered worktrees, and committed branches. Refresh this snapshot before a
planned restart; never assume an old slot row is still correct.

## Hard-restart procedure

1. Restore the parent checkout on `main`, fetch through `with-proxy`, and verify
   TaskGraph reads and writes before dispatching work.
2. Reconcile `worktree-state.json` and `worktrees/ACTIVE.md` against the Git
   worktree registries. Git is physical truth; the machine-managed registry is
   currently stale for several branches.
3. Verify `hermit/` and `reverie/` are clean on `main`. Do not develop or check
   out feature branches in a primary checkout.
4. For an existing slot, inspect status before resuming. Never reset, clean,
   stash, or overwrite dirty state. For a missing slot, allocate a canonical
   slot with `scripts/allocate-worktree.rs`, then check out the recorded remote
   branch in that slot.
5. Recreate the canonical fixed agents: `hermit-coord`, `hermit-kvm`,
   `hermit-liteinst`, `hermit-e9patch`, `hermit-dbi`, `hermit-sabre`,
   `hermit-lander`, and `hermit-ci`. Recreate dynamic agents only for the
   still-active TaskGraph work below and stay within the 15-agent cap.
6. Each agent reads its TaskGraph task and notes before continuing. A local-only
   branch or dirty worktree is not a durable handoff; commit and push coherent
   work before any planned restart.

## Live fleet snapshot

Evidence: live `orc-hermit` tmux panes, TaskGraph ownership, Git worktree
registries, and direct `git status` checks at the timestamp above. There are 17
live agent panes, which exceeds the 15-agent policy cap; the coordinator must
retire idle/no-task dynamic panes before adding or respawning the full set.

| Agent | Slot or checkout | Current task / lane | Feature branch or resume state |
| --- | --- | --- | --- |
| `hermit-coord` | `worktrees/reap/{hermit,reverie}` | `detcore-misc-timeout-hang-on-main` | Hermit `test/reap-drive-stopped-tracee-validation` @ `4a97ec36`; Reverie `fix/reap-drive-stopped-tracee-to-exit` @ `d2fb9a05`. **Both are dirty; current fix is not restart-durable.** |
| `hermit-dbi` | `worktrees/dbi/hermit` | Temporary GROUP-A `#1256-#1290` drain; `#1535` landed | Detached at Hermit `6505f0b5`; no current feature branch. |
| `hermit-231b` | `worktrees/vtime/hermit` | `vtime-global-vs-local-audit-overnight` | Research checkout detached at `4274144d`; evidence under `scratch/vtime-audit/`. |
| `hermit-lander` | `worktrees/lander/hermit` | `e2e_union_rebase_batch_textutil` / serialized landing | Local `_union_wip` @ `473fb379` (ahead 10, behind 1 from its configured upstream); verify/push before restart. |
| `hermit-kvm` | **No registered slot; `worktrees/kvm` is absent** | `fix_kvm_python_examples`; pane most recently audited KVM executor reachability | Reverie remote `codex/kvm-credential-noop-parity` @ `fef77963`; allocate a slot before resuming. |
| `hermit-e9patch` | **No registered slot; `worktrees/e9patch` is absent** | E9Patch corpus work-ahead (`e9patch-corpus-round-3` lane; no in-progress task currently owned) | Hermit remote `codex/e9patch-corpus-relocation-stress` @ `37f221e1`; allocate a slot before resuming. |
| `hermit-sabre` | `worktrees/sabre/{hermit,reverie}` | `sabre_non_gated_parity` | Hermit `codex/sabre-nongated-parity-frontier` @ `1932d70e` (pushed); Reverie `codex/sabre-stats-env-lazy` @ `1d64f6b3`. |
| `hermit-238b` | **No registered slot** | `tick-hub-usage-audit` is its newest in-progress task; several older research tasks remain in progress | Parent/read-only work, no product feature branch recorded; allocate before mutation. |
| `hermit-ci` | `worktrees/ci/hermit` | Temporary GROUP-A `#1326-#1356` drain, then `ci-hub-validate-commit-anchoring` | `codex/groupA-ci-combined` @ `f779f6b7` (ahead 1, behind 1). |
| `hermit-220` | `worktrees/landuuid/hermit` | `kick-the-tires-robustness-audit` | `codex/ci-lint-scripts-help-flag` @ `3a262383` (PR `#1515`). Registry ownership is stale. |
| `hermit-243` | **No registered slot** | `backend-rb-readiness-assessment-overnight` | Research-only, no product feature branch recorded. |
| `hermit-226` | `worktrees/union/hermit` | `e2e_manifest_union_rebase` / GROUP-A `#1221-#1255` | `codex/backend-parity-multiprocess-fork-exec` @ `4df1b2bd`. Registry ownership is stale. |
| `hermit-250` | `worktrees/250/hermit` | Temporary GROUP-A `#1360-#1477` drain, then `ci-hub-local-ci-history-store` | `codex/e2e-manifest-union-driver` @ `f356148f`. |
| `hermit-ptw` | **No slot assigned to this agent in the registry** | GROUP-A `#1291-#1325`, then `e9patch_ptw_promote_agreed_subset` | Per-PR branches are pushed; do not claim `slot04`/`slot05` without coordinator allocation. |
| `hermit-251` | Parent checkout `~/work/dev-hermit` | Parent `HANDOFF.md` continuity refresh and gitlink audit; prior `ci-hub-consolidate-scattered-infra` is closed | Parent `main`; no product feature branch. |
| `hermit-227b` | `worktrees/227b/hermit` | `wire-affected-test-selection-and-measure` | `codex/verify-selection-unknown-path` @ `062bd8ad` (probe PR `#1539`; companion probes `#1536-#1538`). |
| `hermit-247` | `worktrees/247/hermit` | `ci-cancellation-masking-let-red-land`; currently blocked by the main `test.detcore_misc` hang | `codex/fail-closed-required-checks` @ `5f96cdbc`. |

## Fixed-lane gap

`hermit-liteinst` is part of the canonical fixed inventory but has no live
`orc-hermit` pane and no registered `worktrees/liteinst` slot in this snapshot.
On restart, the coordinator must either allocate its named slot and dispatch its
current LiteInst task, or explicitly record why the lane remains paused.

## Hermit gitlink hold

At this snapshot the parent pins Hermit
`3e4367ec206c756c9eca5b5427826e30d5a42074`, while fetched
`rrnewton/hermit:main` is
`0ac1c1d7cd9380cae76bb596788e8ba6e35e2c2d`: the pin is 85 commits behind and
is an ancestor (0 commits divergent). Do **not** re-pin while
`test.detcore_misc` is the known hanging cell; advance the gitlink deliberately
after that main failure is fixed and the intended validation is green.

## Durable recovery sources

- Current work assignment and decisions: `tg <task> -v` and task notes.
- Physical checkout/branch truth: `git worktree list --porcelain` in the parent,
  Hermit, Reverie, and LiteInst2 repositories.
- Machine-local slot ownership: `worktree-state.json` and
  `worktrees/ACTIVE.md` (advisory until reconciled with Git).
- Completed slot history: `worktrees/ARCHIVED.md`.
- Parent coordination policy: `AGENTS.md` (`CLAUDE.md` symlinks to it).
