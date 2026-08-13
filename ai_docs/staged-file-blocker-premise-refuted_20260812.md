# The staged file blocking activation: no staged file, and the blocker was already gone

2026-08-12, ~21:30 PDT. Task `preserve-and-clear-staged-file-blocking-primary-activation`.
**Nothing was committed, cleared, or discarded.** The tree is exactly as I found it.

## The instruction, and why it was not executed

The ruling was: a staged, unowned file is blocking the primary from advancing to landed main
`80e50e9e`; preserve it to a task branch, push it, then clear the index so activation
proceeds. The mechanism is right. It has no target here.

| claim | measured |
| --- | --- |
| a file is **staged** | `git diff --cached --name-status` is **empty**. The only dirty path in the tree is one **unstaged** modification. |
| the primary is stuck at `80e50e9e` | `80e50e9e` is an **ancestor** of `HEAD`, three commits back. `HEAD == origin/main == d6000de25005`. Activation has run three times since — twice by me, `474ef361` and `d6000de2`, both through the clone protocol. |
| the dirty file is the artifact 904 reviewed | The described defect is a combined write + flag-restore error classified by `write_error.kind()` — Rust `io::Error` API. The dirty path is `.claude/skills/worktree-provisioning/SKILL.md`, 42 added lines about not reviewing an agent's live slot working tree, with **zero** occurrences of `write_error` or `.kind()`. |

## Why proceeding anyway would have been the worse error

Committing that file to a branch under the directed commit message would durably assert two
false things: that this markdown edit is the artifact 904 reviewed, and that it carries that
retry-path defect. It would attribute both to an author who made neither claim. That is
manufacturing provenance — the defect class this session exists to remove — and unlike a
misread it would be permanent and look authoritative.

It would also take live work. The file grew from +15 lines when I first reported it at ~19:40
to +42 at mtime 21:24. Someone is editing it right now.

## It is not a general blocker either

`clone-change activate` refuses local staged, unstaged, or untracked state **in the candidate
paths**, not tree-wide. This file is under `.claude/skills/`, so it blocks only a change whose
claims include that path. Both of my activations, claiming `ci-hub/closure`, succeeded with it
present — which is the direct evidence that it was not what stopped anything.

## Where the reviewed artifact is not

No staged or modified file exists in any worktree under `worktrees/`, any ci-hub clone under
`ignored/ci-hub/`, or any clone under `~/temp/`. `grep -rl write_error --include=*.rs` finds
only committed, clean code — `scripts/single-submodule-bump.rs`,
`hermit/scripts/validate.rs`, `agent-utils/rs/herdr-run/src/cli.rs` — none of which is a
write-plus-flag-restore retry path. So the reviewed artifact has either already been resolved,
possibly by 904's superseding fix, or lives in a checkout not visible from here.

**To re-issue:** name the exact path and the checkout holding it. The preserve-to-branch
mechanism is correct and I will run it immediately against a real target.

## The reusable part

This is the fourth time today a single dirty parent path has been described as blocking the
tree. Three of those were real. The lesson from this one is narrower and worth keeping: a
preserve-and-clear instruction carries a *description* of the content, and the description is
part of the artifact it creates. When the description and the target disagree, executing the
mechanism does not preserve evidence — it fabricates it. Verify that the path in hand is the
path the instruction describes before committing anything on someone else's behalf.

## Commands

```sh
git status --porcelain=v1
git diff --cached --name-status                      # empty == nothing staged
git merge-base --is-ancestor 80e50e9e HEAD           # already advanced past it
git diff <path> | grep -c 'write_error\|\.kind()'    # 0 == not the reviewed artifact
stat -c '%y %n' <path>                               # is an author still editing it
```
