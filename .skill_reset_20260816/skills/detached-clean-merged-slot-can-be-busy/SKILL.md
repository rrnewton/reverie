---
name: detached-clean-merged-slot-can-be-busy
description: "Detached, clean, and main-reachable does not prove a slot is idle; coordinator release requires registry, ownership, process, and recovery checks."
---

# Git state alone cannot release a slot

Agents can run from detached heads and temporary guest roots while the worktree
looks clean. Therefore detached + clean + merged is necessary but not sufficient
evidence that a slot is idle.

Only the coordinator releases a slot, using `scripts/release-worktree.rs` after
reconciling the registry, filesystem, every product's worktree registry, branch
reachability, documented recovery SHAs, and exact process ownership. Never use
raw `git worktree remove`, prune, reset, or manual directory deletion. Never kill
by command pattern, executable name, user, or substring; only an agent's own
captured child PID/PGID may be stopped. If ownership is uncertain, keep the slot
active and record the discrepancy.
