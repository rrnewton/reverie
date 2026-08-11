---
name: branch-vs-pr-sweep-mostly-already-prd
description: "How to sweep today's branches for missing PRs; most 'un-PR'd' branches are superseded/stale, not orphaned work"
---

Coordinator sweep for "completed feature branches lacking PRs" (task
`impl-open-prs-today-work`, 2026-07-25). Reusable method + finding:

**Method:** enumerate local branches ahead of `origin/main` in hermit
(shared across worktrees) and reverie, filter by `committerdate` = target day,
subtract the **all-states** PR-head set (`gh pr list --state all --json headRefName`,
~388 hermit / ~96 reverie) plus helper patterns (`land-*`, `*-rebase`,
`archive/*`, `frontier*`, `*-adopt`). Then triage each survivor by its **tip
commit's own diff** (`git show <tip>`), NOT `origin/main..branch` — stale-based
branches show main's advance as thousands of phantom "deletions" (e.g. validate.sh
-884, whole dirs deleted). Check merge-base staleness (`rev-list --count mb..origin/main`).

**Finding (the premise is usually FALSE):** nearly all "un-PR'd" survivors are
one of: (a) superseded by a merged PR (verify content on freshly fetched main
and merge ancestry), (b) owned in-progress work in an active or uncertain slot
(see [parked-slot races](../parked-slot-reuse-is-racy/SKILL.md) and
[busy clean slots](../detached-clean-merged-slot-can-be-busy/SKILL.md)) — do not poach,
(c) stale rebase/automation helper variants, or (d) obsolete frontier work (see
[main-only branches](../base-feature-branches-on-frontier/SKILL.md)). Real orphaned
completed work is rare.

The one genuine case this sweep found: coordinated pair
`fix-dbi-pid-virtualization-slot113` (reverie DBI PID/TID virtualization
`native/client.c` + hermit procfs virtual-PID normalization + L2 regression) —
net-new, pushed to origin on both repos, no PR, no active owner. Published as
DRAFT reverie#106 + hermit#723 (cross-linked). It was deliberately not treated
as validated because its base was stale. Land the lower-level dependency first,
repin, rebase, and classify the result under skill `hermit-validation-authority`. Never type
a validation label by hand; label derivation also lives in that skill.
