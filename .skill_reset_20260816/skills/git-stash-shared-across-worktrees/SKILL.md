---
name: git-stash-shared-across-worktrees
description: "Never use git stash in dev-hermit worktrees; the stack is shared and an intended handoff must be committed on its task branch."
---

# Never stash shared-worktree changes

All worktrees of one repository share `refs/stash`. An unqualified stash or pop
can consume another agent's entry and mix foreign changes into the current slot.

Do not create, apply, pop, drop, or recover a stash. Before switching context,
commit every coherent task-owned change on the task feature branch and record
its exact SHA; incomplete intended work remains in its registered dirty slot
until it can be committed or explicitly handed off. For read-only comparison,
use `git diff`, `git show`, or another already-allocated clean checkout. Never
check out a base over task files, copy changes aside, or create an ad hoc
worktree as a stash substitute.
