# Uncommitted working-tree edits captured from /tmp worktrees, 2026-08-27

These are edits that **no push could reach**: on no branch, in no commit, in no
object store. `/tmp` does not survive a reboot.

## What is here

- `patches/` — one `git diff HEAD --binary` per worktree, named
  `<repo>__<worktree>__base-<sha12>.patch`. **The baseline is in the filename**
  so each patch stays applicable after origin/main moves.
- `untracked/` — authored untracked files copied verbatim.
- `manifest.json` — repo, worktree, baseline sha, byte count, sha256 per entry.

## Recovering one

    git -C <repo> worktree add --detach /tmp/recover <baseline-sha-from-filename>
    git -C /tmp/recover apply --binary <absolute path to the .patch>

## What was deliberately NOT captured

`/tmp/hermit-134-known-failures/hermit-src/` — 18,086,968 bytes across 1,567
files, a copied source tree, not authored here.

## These are preserved copies, not proposals

Nothing was resolved, cleaned or committed. Several worktrees belong to agents
that are still running and still editing; their owners own that work.
