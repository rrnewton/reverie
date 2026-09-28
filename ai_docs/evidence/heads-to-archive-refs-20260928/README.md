# dev-hermit: remaining rescue/* and archive/* heads moved to non-branch refs, 2026-09-28

Owner directive: dev-hermit is main-only, with no PRs or branches. After the rescue/auto
cleanup (`../rescue-auto-cleanup-20260928/`) and the devbig014 cleanup of 196 work heads
(`../branch-cleanup-20260928/`), 830 heads remained besides `main`: 539 `rescue/*` and
291 `archive/*`. Nothing was classified or discarded. Each tip was pushed unchanged to a
non-branch ref and read back (830/830 matched):

- `refs/heads/rescue/<x>`  -> `refs/archive/rescue/<x>`
- `refs/heads/archive/<x>` -> `refs/archive/heads-archive/<x>`

`moved-heads.tsv` records each branch, SHA and preserved ref. The heads are then deleted
with expected-SHA leases. Every commit stays reachable on GitHub through `refs/archive/*`;
fetch it with `git fetch origin 'refs/archive/*:refs/archive/*'`.

Retained as a branch: `validation-receipts`, the live ci-hub receipt store
(see `../branch-cleanup-20260928/README.md`). Moving it to a non-branch ref requires a
ci-hub change first.
