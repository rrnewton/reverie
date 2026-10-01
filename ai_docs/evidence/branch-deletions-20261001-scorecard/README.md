# Deletion record: hermit branch claude/scorecard-site-pin-on-5d93ff54

Background and terms:
- The compatibility site (https://rrnewton.github.io/hermit/compatibility/latest/) is built by hermit's Docs workflow (`.github/workflows/docs.yml`). The workflow runs the dev-hermit site builder (`ci-hub/compatibility-website/`) at the commit that hermit's `.github/compatibility-site-builder.json` pins (the "builder pin").
- This branch was hermit main 5d93ff543facec83fe9ae002014293fb53815137 plus one commit, fb4a2541, that moved the builder pin from dev-hermit 8ac25fc1 to 5671486a (pin file blob 6b6bce2a). Its tip commit is dated 2026-09-30T16:32Z. It was made so the Docs workflow could be dispatched on it. That dispatch was never made: docs.yml skips every job unless the ref is main, so only main can publish (https://github.com/rrnewton/dev-hermit/issues/458).
- The same pin change, with the same blob 6b6bce2a, landed on hermit main as 5407956c32e78d726b0805f81c3415b115bfc5bb ("Pin the compatibility site builder to the no-verdict renderer fix"), in the series that became hermit main 5ad890a79b4669bdb8171b9de8d6fdb3b83b03ad (2026-10-01 07:21Z). The branch tip itself is not in main. After that landing the branch has no purpose; nothing else is on it.

The branch is deleted with an expected-SHA lease on the tip below, so the deletion fails if the branch has moved:

    git push --force-with-lease=refs/heads/claude/scorecard-site-pin-on-5d93ff54:fb4a25410dbd52993456595ae88d4f01b74f085c origin :refs/heads/claude/scorecard-site-pin-on-5d93ff54

Recorded by: scorecard (claude, devbig014), on the instruction of its coordinator.
