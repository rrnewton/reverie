# Nested fbsource checkout is misidentified by plain `sl` (2026-09-25)

Measured:
- cd <slot>/ignored/fbsource && /usr/local/bin/sl root
  -> /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus   (the OUTER git worktree)
- /usr/local/bin/sl log -r . -T '{node}\n' in the same cwd -> 0000000000000000000000000000000000000000
- Same result under `env -i HOME PATH` (not an environment artifact).
- Control: cd /tmp/tfa-phase2-controller-followup-20260924 && sl root -> that checkout (correct).

Cause (by design): fbcode/eden/scm/lib/identity/src/lib.rs SL_GIT identity,
`sniff_root_priority: 0` with comment "Highest priority: outer .git repo wins over
inner .sl/.hg". The slot root has a `.git` gitlink file ("gitdir: ..."), which the
sniffer accepts (lib.rs ~L656-661).

Impact: fbcode/testinfra/tfa/pipeline.py `_checkout_state` runs
`/usr/local/bin/sl log -r .` and `sl status` with cwd=checkout. From this nested
location they read the git worktree, not fbsource, so production cases run from
here cannot produce valid evidence. Any location under /home/newton/work/dev-hermit
(itself a git repo) has the same defect.

Not used as a workaround: SL_REPO_IDENTITY env override (lib.rs ~L580). It would alter
the environment the production gates run in.
