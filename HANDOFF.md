# HANDOFF

Coordinator identity: `hermit-124`.

The coordinator process was started in the shared parent checkout `/home/newton/work/dev-hermit`, not in a slot. The active product slot for this task is `/home/newton/work/dev-hermit/worktrees/hermit-124-pmu-skid-correction`.

## Current task: PMU skid overshoot composition and landing

The exact defect is the Ptrace PMU late-delivery path formerly panicking at `reverie-ptrace/src/timer.rs:982`, observed as `actual=3141`, `target=1000` in `e2e.manifest_backend_parity_c` under contention.

### Reverie PR 503

- Pull request: https://github.com/rrnewton/reverie/pull/503
- Remote PR branch: `hermit-124/pmu-skid-recovery`
- Settled exact head: `d0ab063cfdff92dcc607efcde370ed96c5a22eb1`
- Exact base used for the final rebase: Reverie main `d3cd29e2fef334108a4e99739a6a38a744628702`
- Local slot: `/home/newton/work/dev-hermit/worktrees/hermit-124-pmu-skid-correction`
- Local branch: `hermit-124/pmu-skid-recovery-fix` at the same head. Its configured upstream is the older preservation branch at `6185e7690a700d8be8a1954b06b059b007dd7d5c`; the commit itself is safe because remote PR branch `hermit-124/pmu-skid-recovery` and `refs/pull/503/head` both resolve to `d0ab063cfdff92dcc607efcde370ed96c5a22eb1`.
- Public settled declaration: https://github.com/rrnewton/reverie/pull/503#issuecomment-5439634496
- Rebase proof: `git range-diff ab07a89239150df3726a036bee9f5e897893dfc1..6185e7690a700d8be8a1954b06b059b007dd7d5c d3cd29e2fef334108a4e99739a6a38a744628702..d0ab063cfdff92dcc607efcde370ed96c5a22eb1` reports exact `=` for all three commits. The rebase had no conflicts. Fresh remote branch and pull-ref trees matched the clean local tree.
- Current state: NOT MERGED. The attempted landing at prior head `e3ccb71500f5cd13d45eba849100f2aae4265bc0` was correctly stopped before invoking merge because Codex refusal https://github.com/rrnewton/reverie/pull/503#issuecomment-5438402110 was current and the Claude label was stale. The new third commit preserves the no-perf-host skip while retaining the automatic forced-overshoot child on capable hosts.
- Review tasks: `codex_re_review_of` and `claude_re_review_of` are OPEN and each contains the exact `d0ab063c...` declaration. No more pushes are planned until both exact-head lanes finish.
- Next action: after both lanes approve this exact head and canonical exact-head validation exists, run `ci-hub/bin/gh-merge-verified 503 --repo rrnewton/reverie -- --rebase` from a clean detached Reverie worktree. Then fetch `origin/main`, derive every changed path from the PR merge base, and require byte-identity against the landed tree before reporting success.

### Hermit PR 2733

- Pull request: https://github.com/rrnewton/hermit/pull/2733
- Branch: `hermit-124/pmu-skid-reader`
- Settled exact head: `91442c38c97dd158604c1b1dcd294671038e8cf8`
- Exact base used for the final rebase: Hermit main `d4d9fe5effe31a90c5a64238ce99fc5ddeea4710`
- Local slot: `/home/newton/work/dev-hermit/worktrees/hermit-124-pmu-reader`
- Public settled declaration: https://github.com/rrnewton/hermit/pull/2733#issuecomment-5439467883
- Rebase proof: all four commits replayed as exact `=` entries, no conflicts; remote branch and pull-request trees were byte-identical to the clean worktree.
- Current state: NOT MERGED. Review tasks `codex_re_review_of_2` and `claude_re_review_of_2` are OPEN after the final rebase invalidated the prior exact-head evidence.
- Next action: obtain both exact-head reviews and canonical validation, then land and verify changed-path content from freshly fetched `origin/main`.

The two pull requests compose: Reverie PR 503 stops the child panic and records the exact overshoot condition; Hermit PR 2733 consumes the count and refuses an otherwise successful result. Neither is sufficient alone. Static census at Hermit `7d5bb038a41d70ded0fd8ae2739002739cb77b3a` found 17 validate DAG nodes with a configured path to this timer and 246 of 306 manifest executions on Ptrace or LiteInst with PMU-backed preemption enabled. The durable task is `pmu-skid-overshoot-panics-at-timer-rs-982-under-contention`.

## PR 2696

- Pull request: https://github.com/rrnewton/hermit/pull/2696
- Current reviewed head known to this agent: `a6c514d9122b043e63da4275d5c785df66eae96a`
- Local branch and slot: `hermit-124/2696-final-nontmp` in `/home/newton/work/dev-hermit/worktrees/hermit-124-2696-final`
- The pull ref resolved to that SHA during the last remote preservation check. Independent Codex review approved it at https://github.com/rrnewton/hermit/pull/2696#issuecomment-5435611488.
- Task: `hermit_2696_the_brackets`.
- Next action: re-read the remote head and current Hermit main; if still exact-head approved, obtain current exact-head validation and land from a clean detached worktree. The required behavior is that every validate schedules `bisect-probe --self-test`, with a broken bracket failing by name and the restored harmless control passing.

## Running validation handles

No validation was started or is owned by `hermit-124` at checkpoint time. Two other agents hold the box; record only, do not attempt to save or stop them:

- `validate-hermit-141-4944fb5b3cc0-1787834650301238559-798721-c47ac7c9.service`
  log: `/home/newton/work/dev-hermit/ignored/validate/validate-hermit-141-4944fb5b3cc0-1787834650301238559-798721-c47ac7c9.log`
- `validate-hermit-139-d4d9fe5effe3-1787834846589488461-1094101-3efbe98f.service`
  log: `/home/newton/work/dev-hermit/ignored/validate/validate-hermit-139-d4d9fe5effe3-1787834846589488461-1094101-3efbe98f.log`

Nothing owned by `hermit-124` is unsafe to interrupt as a running process. No rebase, merge, push, or validate is in flight.

## Uncommitted and local-only state found during checkpoint

These must be inspected before cleanup; they are not part of either settled PR head:

- `/home/newton/work/dev-hermit/worktrees/hermit-124-pmu-skid-hermit`, branch `hermit-124/pmu-skid-integration`, head `1540f91a0539e0cec8923d33220cdc316c910a0b`: ten staged Cargo manifest/lockfile edits pinning Reverie from `86d9003a...` to old PR503 head `c73b7e4e...`. The branch head is an ancestor of pushed branch `hermit-124/pmu-skid-integration-v2` at `f6706739f9374e54af36bed6587011d9a1a5673e`; the staged edits themselves are uncommitted and obsolete relative to the newer integration branch, but remain present locally.
- `/tmp/orc-hermit-2696`, branch `hermit-124/pr-2696-takeover`, head `5fea32e55ebc7e2409ad37169d7adbd498788f64`: uncommitted edit to `scripts/bisect-probe.rs`. It contains suite-only category probing and fail-closed midpoint handling. It is not an ancestor of reviewed PR2696 head `a6c514d...`; inspect whether the final PR carries equivalent content before discarding or preserving it.
- Clean local-only historical branches: `hermit-124/2696-brackets` at `b5fdb953f69ef9d322afa4d486f9ea2dea9512c1` and `hermit-124/2696-final` at `c1f462b619d983879b1c6abfe5d0217d34c41287`. Neither is an ancestor of `a6c514d...`; they were earlier rebased forms and are not the landing head.
- Pushed integration branch: `hermit-124/pmu-skid-integration-v2` at `f6706739f9374e54af36bed6587011d9a1a5673e`. It pins the older settled Reverie head `e3ccb715...`; after PR503 lands it must be rebuilt from current Hermit main using the landed Reverie SHA rather than advanced as-is.
- Pushed budget branch: `hermit-124/reverie-budget-926d` at `fe95da9af2b7dc3762c4b143535e6dc5fc9d9278`; agent(hermit-139) owns the active replacement work, so do not redo it.

## Publication records

The previously flagged closed drain tasks now carry structured `PUBLISHED-COMMENT v1` notes and pass the publication gate:

- `drain-open-pr-hermit-2733` records https://github.com/rrnewton/hermit/pull/2733#issuecomment-5439467883
- `drain-open-pr-reverie-503` records https://github.com/rrnewton/reverie/pull/503#issuecomment-5439634496 and the earlier declaration https://github.com/rrnewton/reverie/pull/503#issuecomment-5438225780

## First commands after restart

From `/home/newton/work/dev-hermit`:

1. Read this file.
2. Run `git -C /home/newton/work/dev-hermit/worktrees/hermit-124-pmu-skid-correction status --short --branch`.
3. Read `tg codex_re_review_of -v` and `tg claude_re_review_of -v` for fresh PR503 verdicts at `d0ab063c...`.
4. Read `tg codex_re_review_of_2 -v` and `tg claude_re_review_of_2 -v` for fresh PR2733 verdicts at `91442c38...`.
5. Read `ci-hub/ci-hub validate-lock status`; do not infer the two handles above survived reboot.

Unverified: no fresh exact-head review verdict or canonical validation exists yet for PR503 `d0ab063c...` or PR2733 `91442c38...`. PR503 has not landed. PR2733 has not landed. PR2696 landing status has not been rechecked after this checkpoint.
