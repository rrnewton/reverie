# Branch deletion record, 2026-08-27

This directory records the exact remote branch tips approved for deletion from
`rrnewton/hermit`, `rrnewton/reverie`, `rrnewton/dev-hermit`,
`rrnewton/agent-utils`, and `rrnewton/liteinst2`. It was committed before any
branch was deleted. `refs/rescue/*`, `archive/*`, `main`, and `integration/*`
are excluded.

`deleted-branches.tsv` contains 353 data rows. Each row gives the repository,
branch name, exact tip SHA, content proof, and associated pull-request URLs.
Its SHA-256 before deletion was
`abaa246fd4d7ecd31b6cb5d7b4c06c9694052746995b02dc89ec78c661400381`.

The recorded tips were read from the remotes again immediately before this
record was created. All 353 names still resolved to the recorded SHA. The main
commits at that check were:

- `rrnewton/hermit`: `e2d89097f1baa8af260c68994201121ac4c557da`
- `rrnewton/reverie`: `0cc049fa68c0628024d1329b168edf9d8398d267`
- `rrnewton/dev-hermit`: `75dc69b67590d59acf870543fa3344ac22a7c3a8`
- `rrnewton/agent-utils`: `ff3b5e28c7e58d81a1d0e5f772b3c84765a6dbd6`
- `rrnewton/liteinst2`: `ac6df266c76dcd5c724df958321ff4f9c0b23f63`

## What the proof means

Branches were selected by content, not by a merged label and not by branch-tip
ancestry alone.

- 317 branch deltas have the same stable aggregate patch ID as the associated
  merged pull-request commit contained in main.
- 20 have that patch identity and also have identical content on every path
  changed by the branch.
- 3 have identical content on every path changed by the branch.
- 8 branch deltas reverse-apply cleanly to current main.
- 4 have no net content delta from their merge base with main.
- 1 has a complete tree identical to main.

The ancestry check used for the patch-ID cases establishes that the matching
merge commit is contained in main; ancestry alone did not admit a branch to
this file.

Stable patch ID can report a false missing result when work was reapplied by
hand in a restack. That limitation is conservative here: a failed patch-ID
comparison did not place a branch in this file. In particular, 51 branch tips
associated with pull requests labelled merged did not have sufficient content
proof and remain excluded from deletion. Every patch-ID entry in this file is
a positive match against the merge commit recorded for that pull request.

## Recovery

To recreate one deleted name from a clone that has the recorded commit object:

```sh
with-proxy git push origin <sha>:refs/heads/<branch>
```

Use the repository, SHA, and branch columns from `deleted-branches.tsv`. The
deletion commands use an expected-SHA lease, so a remote branch that advances
after this record was written is not deleted.
