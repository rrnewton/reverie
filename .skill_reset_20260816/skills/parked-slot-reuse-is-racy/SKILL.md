---
name: parked-slot-reuse-is-racy
description: "A detached clean slot is not free; only a coordinator allocation recorded by the registry authorizes reuse."
---

# Parked-looking slots are not assignments

A slot's branch and checkout are shared mutable state. Detached, clean, absent
from `ACTIVE.md`, or main-reachable does not prove that no agent or process owns
it; old incidents put commits on another agent's branch after an uncoordinated
reuse.

Work only in the canonical slot allocated to the agent by
`scripts/allocate-worktree.rs`. Before the first edit, verify the registry row,
task, branch, owned paths, cleanliness, and current head. If any fact disagrees,
stop editing and report it to the coordinator. Never fall back to another slot,
run the manual `slot-init.sh` fallback, reset the checkout, stash, or copy work
aside to manufacture availability.
