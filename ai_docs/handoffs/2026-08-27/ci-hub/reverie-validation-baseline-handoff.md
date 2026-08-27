# Reverie validation baseline repair

Status: parent authority published; Reverie branch and PR published; no merge.
No plain `gh` bypass was used.

## Reverie

- Base: `bfbe3b14d5d4095c8d23bdb0c4ea278beca7b9c7`
- Candidate: `92a2d4bbf58542800f1dab930a3ef4a3d17d2e77`
- Branch: `fix/reverie-validation-baseline`
- Checkout: `/home/newton/work/dev-hermit/worktrees/reverie-validation-baseline/reverie`
- Result: remove dangling retired root discovery adapters; validate the five
  quarantined root packages and the three still-active LiteInst packages.
- Evidence: base checker exit 1; repaired checker pass; reintroduced dangling
  adapter refused; restored layout pass; workspace fmt and clippy pass; diff
  and repository-cleanliness checks pass.
- Pull request: `https://github.com/rrnewton/reverie/pull/470`
- Independent review: approved at reviewed tree `835937ea`; the published
  attribution-only rewrite `92a2d4bb` was independently verified tree-identical
  and approved.

## dev-hermit authority

- Base: `a3deaa1b3e1773f9e87a446a55a62dcd3c4637fc`
- Candidate: `7fc7e214af6906053ce1f5d5d617f5190ae7a1ad`
- Branch: `ci-hub/repair-reverie-validation-baseline`
- Clone: `/home/newton/temp/dev-hermit-reverie-validation-baseline`
- Seal digest: `a578a11b40cde128d83d3560566f2b2fb22f85c004c9d67fff41705809ecc8d6`
- Result: emit the current six-gate contract as
  `reverie-local-validation/v2`; preserve exact historical seven-gate v1
  receipts; refuse extra, mixed-version, missing, reordered, failed, and
  unknown-policy v2 evidence.
- Evidence: Python 6/6; Python compile; Rust verifier 281/281 serial; stale
  seven-gate wording absent; diff check pass.
- Publication: confirmed on `rrnewton/dev-hermit:main` at exact candidate.
- Activation obligation:
  `20260824-032442-7fc7e214af69-12f8c7`; activation was safely refused because
  the primary checkout contains two local commits absent from remote main.
- Independent review: APPROVE, no findings. The final attribution-only rewrite
  was independently verified tree-identical to the reviewed/sealed change.

## Deployment order

Activate the already-published dev-hermit authority change once the two local
primary commits are preserved or published. Then run exact-head Reverie full
validation for PR #470. Do not merge #470 before that authority is active, and
do not fold this change into Reverie PR #468.
