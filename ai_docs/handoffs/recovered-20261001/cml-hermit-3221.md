# Handoff: cml-hermit-3221 (released by claude-coord-mega-lander, 2026-09-29T13:45Z)

Owner: claude-coord-mega-lander (claude, devbig014), live session pid 518539, recorded agent
claude-cml-w-3221, generation 1.

## State of the checkout

- Branch cml/land-3221-20260928 at ba3643d1463c50737ec341fb3bb26aaaa1810449.
- Pushed to https://github.com/rrnewton/hermit as branch cml/3221-wip-ba3643d1463c; the remote
  tip was read back with git ls-remote at 13:40Z and equals the slot head.
- git status --porcelain: 0 lines. No rebase or merge in progress. The stash list the slot shows
  belongs to the shared hermit repository, not to this worktree; removing the worktree does not
  touch it.
- Nothing in this worktree exists only here.

## What the branch holds

A rebase of https://github.com/rrnewton/hermit/pull/3221 (PR head fc326e4022b91a91d888edbea4b7d344f8a4678f)
onto hermit main 7ce5e3b6bc63718250deed58007e093deae27668, 3 commits:

1. 8182f2c148d Test that the SaBRe config fingerprint ignores the environment
2. 226c5c9ddc4 Log SaBRe Detcore records where the ptrace backend logs them
3. ba3643d1463 Send SaBRe Detcore records before each handler returns

## What remains (not started; the slot is not needed for it)

It was held for https://github.com/rrnewton/hermit/issues/3360, whose fix is on hermit main
(cfa56cd605af84cf6caccc332e057003478c1860). hermit main is now 21 commits ahead of the branch
(350fa554b472aed0e1504453f4ecc8e2e0a4d8d1 at 13:40Z). The next owner, in a fresh slot:

1. Rebase onto current main and re-derive every count node the branch touches; regenerate
   ci/dag/validate.json with generate-validation-dag --write, never hand-merge it.
2. Run the gates, open a replacement PR with an Owner: line, and get exact-head reviews from one
   Claude-family and one Codex-family reviewer plus an exact-head local validation receipt.
3. Land it, then close https://github.com/rrnewton/hermit/pull/3221 naming its head SHA and the
   replacement PR, keeping its branch.

https://github.com/rrnewton/hermit/pull/3221 keeps its Owner: line for claude-coord-mega-lander
until then.
