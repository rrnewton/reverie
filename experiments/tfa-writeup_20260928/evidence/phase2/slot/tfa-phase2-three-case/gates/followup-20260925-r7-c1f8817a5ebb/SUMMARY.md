# Exact-head gates for c1f8817a5ebba01e752d295baad1bd9a8fe9dee3

Parent 3eb9d118337a (gates in `../followup-20260925-r6-3eb9d118337a/`, whose
phase-swap gap is described in its `GAP.md`). Each gate was its own unpiped
command with `.out` and `.rc`.

- commit-identity: head and parent correct, tag line present. sl-status-pre and
  sl-status-post: empty.
- arc-lint (5 stack files), pyre: rc 0. tfa_phase2_test: rc 0, 187/187.
- build-cli: rc 0. cli-par.sha256 equals the r6 value because the par is a
  1154-byte lpar launcher script, not an archive. Its link tree resolves
  followup.py and main.py to the checkout (cli-linktree-sources.sha256), and
  followup.py matches negctl-followup.head.sha256. negctl-real's new refusal
  message proves the CLI ran the r7 code.
- render-{c}, render-followup-{c}: rc 0, all three SUPPORTED. render-cmp: all 12
  files byte-identical to `stages/<c>/stage1-2-relocated/` and
  `stages/<c>/stage3-followup/`.
- negctl-*: nine mutations of committed followup.py, each rc 1 (caught), each
  restore `sha256sum -c` OK; negctl-restored-baseline rc 0 (9 tests). New
  `no-artifact-binding` fails the "unfixed phase is not the manifest's test ELF"
  subtest.
- negctl-real: the same (semantically equal) swap-phases and patched-unfixed-sha
  ledgers as r6. Both are now refused (rc 1, no output directory).
  swap-phases was accepted at r6.
- containment_test (run 1): rc 32, 34/40. The four signed-Codex 0.156.0
  preflight failures plus two host failures: `hg_monitor` (pid 3361621, host
  Sapling monitor) still referenced the removed disposable worktree, and a
  reverie-ptrace perf-counter skid panic ("Consider increasing skid margin for
  this CPU") under host load. containment_rerun-env-flakes: those two tests
  alone, 2/2 pass.
- containment_test-run2 (full suite): rc 32, 36/40, the four signed-Codex 0.156.0
  preflight failures only (BLOCKED, not green), with the known secondary
  chataccd "checkout services did not retire" cleanup symptom, as at r5.
