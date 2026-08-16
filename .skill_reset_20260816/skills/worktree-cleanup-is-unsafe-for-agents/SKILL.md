---
name: worktree-cleanup-is-unsafe-for-agents
description: "Worktree cleanup is coordinator-only; agents may audit but must not delete, prune, reset, stash, or reclaim slots."
---

# Worktree cleanup is coordinator-only

Unexpected slot state is somebody else's work until proven otherwise.
Implementation agents may perform a read-only audit of registry rows, filesystem
paths, product worktree registries, status, branch reachability, and owned live
processes, then report exact evidence. They remove nothing.

The coordinator uses `scripts/release-worktree.rs` only after intended work is
committed and handed off, recovery SHAs and validation are archived, every child
is clean, and ownership is resolved. Never use `git clean`, raw worktree removal,
manual pruning, reset, stash, directory deletion, or broad process kills to make
the inventory fit a cap. A dirty or uncertain slot stays active.
