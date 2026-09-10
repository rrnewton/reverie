# dev-hermit pull-request branch disposition, 2026-09-10

This directory records the exact remote tips of the 26 branches that backed
the final open pull-request queue in `rrnewton/dev-hermit`. Every pull request
was closed with a public substantive disposition before this record was
published. The exact remote tips were read again immediately before this
record was written and all 26 matched.

The pre-record `rrnewton/dev-hermit` main tip was
`66f8c496cf4818de303173f43aa657fd2e70e96e`. The TSV has 26 data rows and its
pre-deletion SHA-256 is
`02e6131b280d9ade3099d7d802d9ead7181c0d54477666a32f15423af7529127`.

`deleted-branches.tsv` names the repository, original branch, exact tip,
pull-request URL, disposition, durable preservation location, and public
closure comment. A `main:` value means the reviewed useful content was landed
or the older change was proved superseded there. An `archive:` or `rescue:`
value is a protected remote ref that was created and read back at the exact
tip before deletion. A rejected-tip entry identifies a branch whose only
unpreserved delta was unsafe or invalid; its useful portion is separately
named in the preservation field.

This record itself must be committed to `main` and read back from the remote
before any listed branch is deleted. Deletion is then permitted only with an
expected-SHA lease for the exact `tip_sha` in the table. A moved branch must
survive for a new review. `main`, `integration/*`, `archive/*`, and
`refs/rescue/*` are outside the deletion set.

## Recovery

For an archived or rescued row, recreate the original name from the recorded
protected ref. For a landed or superseded row, consult the pull-request URL,
public closure comment, and named `main` commit before deciding whether a
historical branch name should be recreated.
