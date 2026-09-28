# dev-hermit rescue/auto branch cleanup, 2026-09-28

Owner: dev-hermit must be main-only, and rescue snapshots of inner-repo work do not belong
in it. The 1,515 `rescue/auto-<sha>` heads were created on 2026-08-27 by preserving every
commit that was local-only in the shared parent checkout
(`ai_docs/evidence/parent-local-only-commits-20260827/`). Of those commits, 544 were
Hermit, 948 Reverie and 23 dev-hermit. That inventory classified 1,506 as content-present
on the appropriate current main, 7 as superseded and 2 as unpublished.

This step handles only the 1,506 content-present heads whose remote tip still equalled the
inventoried SHA. Each tip was first pushed to the non-branch ref `refs/archive/rescue/<name>`
and read back (1,506/1,506 matched). `deleted-heads.tsv` records the branch, SHA and
preserved ref for each. The heads are then deleted with expected-SHA leases.

Not touched here: the 7 superseded and 2 unpublished heads (to be moved into their own
repositories), the other rescue families (dropped-stash, unreachable, local-only and
dated snapshots), `refs/rescue/*`, `archive/*`, and `validation-receipts`.
