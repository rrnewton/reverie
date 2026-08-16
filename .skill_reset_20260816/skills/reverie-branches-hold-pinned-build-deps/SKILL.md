---
name: reverie-branches-hold-pinned-build-deps
description: "Deleting rrnewton/reverie branches can orphan a hermit-pinned build dep; check containment first"
---

Branch deletion requires an explicit task authorization; an audit request alone
does not authorize mutation. Before any authorized deletion on
`rrnewton/reverie`, check every Reverie `?rev=SHA` pinned by Hermit against
fresh remote branch containment. Hermit pins by exact SHA, so a seemingly stale
branch can be the only ref keeping a build dependency reachable.

**Why:** During the 2026-07-22 reverie branch cleanup, `impl-rcx-r11-canonicalization` was the sole holder of rev `f6bcc06e498...`, pinned by 5 active worktrees (slot84/89/90/94/96). "Delete all but main + open PRs" would have broken those builds.

**How to apply:** Report every uniquely held pin and keep that branch. A separate
coordinated change may first repin dependents to a reviewed, reachable main SHA.
Never infer deletion authority from recoverability, age, a closed PR, or an old
frontier note.
