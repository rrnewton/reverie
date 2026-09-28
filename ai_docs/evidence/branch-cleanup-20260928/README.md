# dev-hermit non-protected branch cleanup, 2026-09-28

Owner directive (verbatim): "make sure we are synced up and that repo remains
main-only, should not be PRs or branches".

At inventory time `git ls-remote --heads origin` on `rrnewton/dev-hermit`
returned 2534 heads: `main` (7b205bdd3ce9df1fe312e86cf6105c3829ac8fc5),
2045 `rescue/*`, 291 `archive/*`, 0 `integration/*`, and 197 others. Open pull
requests: 0. The protected families (`main`, `integration/*`, `archive/*`,
`rescue/*`, and `refs/rescue/*`) are outside this record and were not touched;
what to do with them is an open question for the owner.

`deleted-heads.tsv` has one row per non-protected head (197 rows): repository,
branch, exact tip SHA, tip committer date, whether the tip is an ancestor of
`origin/main` at 7b205bdd3c (`git merge-base --is-ancestor`), the preserved ref,
and the action.

Preservation: before this record was written, every one of the 197 tips was
pushed to the non-branch ref `refs/archive/branches/<original-name>` on origin
and read back with `git ls-remote origin 'refs/archive/branches/*'`; all 197
name/SHA pairs matched the table exactly. 175 of the 197 tips are not contained
in `main`, so the archive ref is the only remote copy of that work.

Deletion: rows with action `delete` (196) are removed only after this record is
on `main` and read back from the remote, each with an expected-SHA lease
`--force-with-lease=refs/heads/<branch>:<tip_sha>`. A branch that moved after
this record must fail to delete and survive for review.

Retained: `validation-receipts` (action `retained-live-receipt-store`) is not a
work branch. It is the live validation-receipt store: `RECEIPT_REPO` /
`RECEIPT_BRANCH` in `ci-hub/validation/publish_receipt.py`,
`VALIDATION_RECEIPT_BRANCH` in `ci-hub/ci-hub.rs`, and `receipt_branch` in
`ci-hub/validation/verify_receipt.sh` (which checks receipt commits against it
through the compare API). Its tip advanced on 2026-09-27. Deleting it would
break receipt publication and verification for landings. Its
`refs/archive/branches/validation-receipts` ref is only a point-in-time
snapshot. Keeping the repository strictly main-only requires first moving the
receipt store to a non-branch ref or another repository; that is an owner
decision.
