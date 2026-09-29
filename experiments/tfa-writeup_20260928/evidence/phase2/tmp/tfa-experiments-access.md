coord2 -> sub-tfa: how to reach the TFA experiment records from devvm895. (The owner asked for the writeup to go in dev-hermit/experiments.)

- All 15 TFA experiment directories (39 MB, about 3,700 files) are committed to the dev-hermit repo. Its main is at 82050eff3fb66d6a854f212b014e5cc627416874 on https://github.com/rrnewton/dev-hermit. The authoritative checkout is /home/newton/work/dev-hermit on devbig030, not devbig014.
- Directories: experiments/tfa-flaky-tests_20260917, experiments/tfa-openat-stack-size-20260917, experiments/tfa-kernel71-campaign_20260921, experiments/tfa-kernel71-{sched,time}-input-campaign_20260921, and nine experiments/tfa-kernel71-*-continuation_20260921. Also ai_docs/tfa-acceptance-bracket-2026-09-17.md and ai_docs/tfa-candidate-sourcing-2026-09-17.md.
- Fetch them WITHOUT the rest of the repo, which is 18 GB of experiments alone:
    with-proxy git clone --filter=blob:none --no-checkout --depth 1 https://github.com/rrnewton/dev-hermit ~/dev-hermit
    cd ~/dev-hermit && git sparse-checkout set --no-cone '/experiments/tfa-*' '/ai_docs/tfa-*' '/AGENTS.md' && git checkout main
  If with-proxy or GitHub access fails on the VM, tell me the exact error and I will rsync the same directories to you.
- Writeup: put it in a NEW directory, experiments/tfa-writeup_20260928/, with a README.md in your clone. Do NOT push to dev-hermit: it is main-only and shared, and I land it from a clean worktree on devbig030. When it is ready, tell me the absolute path on devvm895. I will copy it back with agentcloudctl, review it, commit it and push it, then report the commit SHA to you and the owner.
- The owner's instructions to you take priority over mine.
