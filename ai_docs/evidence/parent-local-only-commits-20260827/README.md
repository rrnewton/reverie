# Parent local-only commit inventory, 2026-08-27

Task: `the-1515-local-only-parent-commits-are-uncompared-not-unpublished`

The parent monitor reported 1,515 commits reachable from local refs or worktree
HEADs and from no locally known remote-tracking ref. Its own output correctly
said that content had not been compared. That count was an upper bound, not an
unpublished-work count.

The exact census was reproduced before changing the parent's remote-tracking
state:

- all-ref count: 1,515
- HEAD-scoped count: 0
- scan time: 0.526 seconds
- classified: false
- unpublished: not classified
- superseded: not classified

## Preservation

All 1,515 commit objects were preserved before classification as distinct
`refs/heads/rescue/auto-<short-sha>` branches on
`https://github.com/rrnewton/dev-hermit`. They were pushed in eight atomic
batches and then read back with `git ls-remote`: 1,515 of 1,515 refs contained
the expected SHA; 0 failed. Nothing was deleted.

`commit-inventory.tsv` records each commit, the repository whose tree it belongs
to, disposition, evidence, exact rescue ref, original local refs when visible,
author, date, and subject.

## Result

The 1,515 commits divide into:

- 1,506 content-present on an appropriate current main;
- 7 superseded by named landed replacements; and
- 2 unpublished commits whose content is absent from current main.

The source trees identify 544 Hermit commits, 948 Reverie commits, and 23
dev-hermit commits. There are no agent-utils or LiteInst2 commits in this
population. Comparing every object only to dev-hermit `origin/main` would
therefore ask the wrong question for 1,492 of 1,515 commits.

### What content-present means

The 1,506 positive results are supported as follows:

- 1,442 exact commit objects are ancestors of the matching repository's current
  main: 495 Hermit and 947 Reverie.
- 48 non-ancestor commits have an equivalent single-commit stable patch ID on
  the matching repository's current main.
- 9 commits belong to three merged Hermit pull-request stacks whose complete
  aggregate stable patch ID equals the corresponding merged commit on main:
  https://github.com/rrnewton/hermit/pull/2562,
  https://github.com/rrnewton/hermit/pull/2622, and
  https://github.com/rrnewton/hermit/pull/2628.
- 4 commit deltas reverse-apply cleanly to current Hermit main.
- 1 commit has both a reverse-applicable delta and byte-identical current
  content at every touched path.
- 2 commits have no net content delta from their first parent.

Thus “content-present” is not shorthand for ancestry. Exact ancestry proves
1,442 rows. The other 64 use content comparison, including stable patch
identity, current path identity, reverse application, and complete-stack patch
identity.

Stable patch ID is correct for an ordinary rebase or commit-message rewrite,
but it can report false missing when a change is reapplied by hand or folded
into a larger restack. That blind spot did occur here, so a patch-ID miss was
never treated as proof of absence. The three complete-stack comparisons and
the seven named replacements below are the additional checks that resolved it.

### Superseded by landed replacements

Seven commits were not byte-identical to main, but direct comparison found the
same intended work in a named landed change, usually with corrections added:

- `198fa68adf326c909f44d9b03fab2bc25d1eb0fd` was the environment-dependent
  test-fixture attempt replaced by `66283f8c59308d769ae4f44c41ee964f375fb493`
  in https://github.com/rrnewton/hermit/pull/2641.
- `e5c4e2b77d4adc5923445d6fdec5b12a9080fb56` maps by range-diff to
  `26cde85f9752ba80bc460e21967f984004d84100` in
  https://github.com/rrnewton/hermit/pull/2651; current main contains the audit
  target and all ten recorded rows.
- `b221e3e0e2174cc8a858c3b6cf50dd1b0ff1144c` was replaced by the versioned
  SaBRe backend evidence and three-state reach check in `4ad60dc476`.
- `9f6b9f08af5b0b87fe62c649b00e4c7ecca78a30` was reauthored as
  `c5b4a3cb8a`, retaining the pull-request hold analysis while removing two
  falsified claims.
- `0634708c34198613e4079e535246d54062b53629` maps by range-diff to
  `7ada52f5f`, which adds launch locking and tests to the pressure-test scheduler.
- `a79e20d8acdc8fdedf7b95ff1783503da00dfd2d` maps by range-diff to
  `23d6c2a64`, which retains the lead-in grammar and broadens the tests.
- `b7aadc28d7dcec951979ac0b6989a1a7aaa21754` is stash WIP superseded by
  `3a3c18d86`, the landed per-test repeat implementation with workspace
  selection and additional checks.

These seven are not called content-identical. They are recorded separately so
the hand-reapplied-restack limitation is visible rather than hidden in the
1,506 count.

### Unpublished content

Two commits contain content absent from current main:

- `23aa04f3e051849b75d4cfe4ac79e86c33963866` adds `HANDOFF.md` and five
  `sb/*.c` SaBRe probe sources. All six paths are absent from Hermit main, and
  the commit delta applies forward cleanly.
- `0b274b859ce3163ed70182fc6e90c4d5f6a4cf91` is a mixed dev-hermit recovery
  merge. Its `sys.dont_write_bytecode` controller guard and archived instruction
  edits are absent, while its removal of the `safehermit` wrapper is contradicted
  by current main. Other hunks landed separately. The merge must not be applied
  wholesale merely because it is in this two-commit count.

Both are now remotely preserved by the exact rescue refs recorded in the TSV.
This inventory makes no recommendation to land either commit and deletes
nothing.

## Main commits used

- Hermit: `ff125248383e7d4ed04eb7e93b5fa8354d972483`
- Reverie: `ab07a89239150df3726a036bee9f5e897893dfc1`
- dev-hermit: `a82eba216c4b232fd71f7c0f9f83926aac7c3af7`
- agent-utils: `ff3b5e28c7e58d81a1d0e5f772b3c84765a6dbd6`
- LiteInst2: `ac6df266c76dcd5c724df958321ff4f9c0b23f63`
