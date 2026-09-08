# TaskGraph backlog partition, 2026-09-07

This directory preserves the complete reviewed partition of the non-project
TaskGraph backlog captured on 2026-09-07/08 UTC. It exists because the original
artifacts were under an ignored worktree directory and therefore had no Git
ref or publication path.

The classified population was 502 rows:

- 139 `ACTIONABLE`
- 112 `BLOCKED`
- 85 `STALE-PREMISE`
- 166 `DUPLICATE`

`partition.tsv` contains one classification and evidence clause for every row.
Its SHA-256 is
`73e0c9b0520a37e60f02a00f8bbb0bf5dbee87a630df8e6e0edee91c96fcf859`.
The same classification was written to each TaskGraph row with the
`complete-backlog-partition` note prefix and verified byte-for-byte.

`actionable-ranked.tsv` contains all 139 rows classified `ACTIONABLE`. Rows
with current nonterminal tasks behind them sort first by the total number of
such tasks, then direct edges, priority, impact, and task ID. The other rows
sort by priority, impact, and task ID. Its SHA-256 is
`52acbb6a5083f5c3af5ef0c787a4306eac6839c2daacc7a6c617d6c2d1523581`.

Two actionable rows moved from `BACKLOG` to `IN_PROGRESS` while the notes were
being written. Their status is retained in the ranked file rather than hidden:

- `owner-checkout-dirty-set-blocks-periodic-validate-for-23-hours`
- `the-operational-tick-files-a-new-task-per-cycle-instead-of-updating-one`

The 166 duplicate rows include 110 repeated operational-health alert rows.
They were not closed: preserving them keeps the evidence for
`the-operational-tick-files-a-new-task-per-cycle-instead-of-updating-one`
until that repeated-filing defect lands. Duplicate reasons name the surviving
row or landed work. The whole-tick timeout row is explicitly marked as a
symptom-level grouping because its component causes remain separately tracked.

No task was claimed or closed during the partition pass. A named TaskGraph
snapshot, `before-backlog-partition-notes-20260907-01a07`, preserves the state
immediately before the classification notes were added.
