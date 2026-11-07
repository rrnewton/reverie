# integration/literoot-sprint — branch record

## Partition-coverage gate form (coord7 21:38, recorded 2026-11-06)

`test_partition_coverage` compares the **sum of node discovery
universes** (the `binary(=...)` / `package()` selectors in
`ci/dag/validate.json`, evaluated as nextest discovery sets)
against `target/validate/list-full.json`
(`cargo nextest list --list-type full`). It is a set-coverage
comparison, not a text/binary grep: a test binary is covered
iff some node's selector discovers it.

Consequences for this branch:

- The only real partition gap today is `raw_mseal` (1312
  segment). Museport's fix head adds `binary(=raw_mseal)` to
  exactly the `liteinst_b` node; the integration swap inherits
  that line — **add nothing for it here** (a duplicate line in
  a second node would double-discover, not fix coverage).
- 1308's `c1_mapped_issuance` needs **no** `binary(=...)` line:
  it is already covered by an existing `package()` selector.
- Pre-flight for any future test binary added on this branch:
  it must be discovered by exactly the intended node selector
  set; verify against a locally generated `list-full.json`,
  not by grepping `validate.json` for the name.

## Museport evidence note (relayed 2026-11-06 via museli-lt)

`test_partition_coverage` exists only on this integration branch
(not on hermit main `9bd5c239`, not on reverie trees). Museport
evidence for the 1312 `raw_mseal` gap: arithmetic 8431 + 232 =
8663 (the museinteg 15d17486 shortfall is exactly the 232
`raw_mseal` cases), exactly one `binary(=raw_mseal)` line in the
`liteinst_b` node, binary lists 232. The authoritative gate run
needs this integration tree (or museinteg); fold this reading
into the next museinteg verdict. No additional line is added
here — the fix head carries the single line and the swap
inherits it.
