# PR #2095 undisclosed semantic rider: process correction record

## Disposition

**Code correction: no. Process and disclosure correction: yes. Do not
revert.**

Hermit PR [#2095](https://github.com/rrnewton/hermit/pull/2095) merged with an
undisclosed semantic unit and an incomplete exact-head semantic review. The
defect is in the process and its record, not in the landed behavior.

The controlling scope statement is exact:

> validation diagnostics currently depend on this behavior, but application runtime semantics do not.

Reverting would restore silent audit failure evidence loss. This record does
not propose a code change.

## Exact merge and review timeline

- Exact PR head: `51821486ea488907b100713c1c8048b455c1ff7c`.
- Merge commit: `b4cc27da6aa02bd2126234b9cb71a4f80c9d143b`.
- Merged: `2026-08-10T19:59:14Z`.
- The pre-merge exact-head review approved at `2026-08-10T19:36:38Z`, but
  certified the CPU-width work without semantically auditing the rider:
  [comment 5245055273](https://github.com/rrnewton/hermit/pull/2095#issuecomment-5245055273).
- The post-merge review identified the rider at `2026-08-10T20:56:22Z`,
  exactly `+57m08s` after merge:
  [comment 5245869411](https://github.com/rrnewton/hermit/pull/2095#issuecomment-5245869411).

The empirical process finding is exact:

> The +57m08s review found a real scope defect after it could prevent landing, demonstrating that post-close adversarial review is detection without prevention.

This is measured evidence relevant to the disputed post-close-review policy in
Reverie PR #437. It does not itself decide that policy dispute.

## Undisclosed semantic unit

The rider is independent cherry-pick
`9b6245b1b5208d58cd3ee266281b46d0d77aaabd`, titled `Preserve failing audit
probe evidence`, in `ci/test_harness.sh`. PR #2095 described its CPU-width
mechanism and named its structural relationship to #2092, but did not disclose
this separate behavior in its own Summary. Naming two changed paths is not
semantic disclosure, and historical review of a source PR does not transfer
across a cherry-pick.

The rider:

- adds `run_audit_probe_expect_status`;
- captures each probe's status and combined output before `set -e` can discard
  the evidence;
- accepts only the declared status;
- reports the probe name, actual and expected statuses, and captured output on
  mismatch;
- preserves an unexpected nonzero status and maps unexpected success to rc 1;
- routes hosted rc 0, boxed rc 0, and wrong-pin rc 2 probes through the helper;
- adds a planted rc 101 panic bracket whose identity and stderr must remain
  visible.

## Current dependence and evidence

At Hermit main `796565d21d123cf177b54e26e2176b695752aef3`, the helper is invoked
at four sites from `audit_ci_correspondence`. That function is reached by
`audit-ci` and full validation. The originating read-only characterization task
`hermit-2095-undisclosed-semantic-rider-found-post-merge` records:

- current-main `./ci/test_harness.sh audit-ci`: rc 0;
- the planted rc 101 identity-and-stderr bracket executed and passed;
- expected rc 0 probes continued;
- expected wrong-pin rc 2 continued.

The behavior is intentional and load-bearing for validation diagnostics. Its
source PR had explicit evidence and review, but #2095's own disclosure and
exact-head semantic review were incomplete. The corrective action is this
durable record and future pre-close semantic review, not a revert.

## Closure discipline

This record does not treat red, missing, or stale validation as evidence. It
does not authorize closing blocked PR work or deferring adversarial review
until after close. Evidence, adversarial confirmation, and only then closure
remain the operative order while the disputed Reverie AGENTS.md clauses are on
hold.
