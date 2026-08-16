---
name: base-feature-branches-on-frontier
description: "Hermit and Reverie are main-only; coordinators allocate registered slots and branch task work from freshly fetched origin/main."
---

# Main-only feature branches

The obsolete `frontier` branches were removed in July 2026. New product work is
based on freshly fetched `origin/main` and targets `main`.

Do not create or switch a branch in a primary or an arbitrary parked worktree.
The coordinator first allocates the agent's canonical slot with
`scripts/allocate-worktree.rs`, registers task/branch/path ownership, verifies
the nested products are clean, fetches without changing checked-out files, and
then creates the task's descriptive feature branch in each product that changes.
Historical frontier tips are archival facts, not instructions to recreate or
push the branch.
