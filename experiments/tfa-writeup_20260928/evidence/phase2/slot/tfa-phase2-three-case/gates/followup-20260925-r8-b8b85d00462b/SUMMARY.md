# Exact-head gates for fbsource b8b85d00462b6f159641aee73a49321ca863609d

Parent: c1f8817a5ebba01e752d295baad1bd9a8fe9dee3. The run date is 2026-09-25.
`run-gates.sh` is the exact script that ran. Each gate wrote its own `.out` and
`.rc`.

Every Python run used a fresh `PYTHONPYCACHEPREFIX` under `pyc/`. Two
equal-size mutants written in the same second share `.pyc` validation keys, and
a pre-commit run in `../followup-r8b-sensitivity/` reused a stale mutant this
way.

`run1-aborted/` holds an earlier invocation of this script. Its render-cmp step
used `{ ...; exit $rc; }`, and the `exit` ended the whole script. All of its
recorded rcs were 0. It stopped before the negative controls, and the full gate
list was rerun here.

## Results

- **commit-identity**: rc 0. `.` is b8b85d00462b and `.^` is c1f8817a5ebb.
- **sl-status-pre and sl-status-post**: rc 0, empty output, so the working
  copy was clean.
- **arc-lint**: rc 0, "No lint issues" on `followup.py`, `report.py`,
  `tests/test_prototype.py` and `README.md`.
- **pyre**: rc 0, "No type errors found" (`arc pyre check-changed-targets`).
- **build-cli**: rc 0.
  - `cli-par.sha256` records the built PAR.
  - `cli-linktree-sources.sha256` shows that the link-tree `followup.py` and
    `report.py` resolve to the checkout files, with identical hashes.
- **tfa_phase2_test**: rc 0, Pass 194, Fail 0. That is the 187 tests at
  c1f8817a plus the 7 new `FollowupTest` methods.
- **Renders**: render-{thrift,asyncssl,httpcoro} and
  render-followup-{thrift,asyncssl,httpcoro} all returned rc 0.
  - All three follow-ups have `Verdict: SUPPORTED`.
- **render-cmp**: rc 0, with 15 cmp 0 lines:
  - the initial `difference-hints.json` and `initial-comment.md` match
    `stages/<c>/stage1-2-relocated/`;
  - `followup-comment.md` and `followup-runs.json` match
    `stages/<c>/stage4-followup-hypothesis-r3/`;
  - `followup-runs.json` matches `stages/<c>/stage3-followup/`, so the
    judgements are unchanged.
- **Negative controls**: 14 mutations of `followup.py`.
  - Every `.mutate` returned rc 0, meaning each anchor matched exactly once.
  - Every `.restore` returned rc 0: `sha256sum -c` against the head
    `followup.py`.
  - The 9 mutations approved at c1f8817a are no-artifact-binding,
    no-patched-fail, no-wall-evidence, no-phase-distinct, no-guest-check,
    no-stdout-binding, trust-recorded-label, no-failing-seed-rule and
    no-witness-caveat. Each makes `FollowupTest` fail with rc 1, in the same
    tests as at r7.
  - The 5 new mutations, applied by `negctl-mutate-plaintext.py`, each make
    `FollowupTest` fail with rc 1:
    - no-control-filter fails the injection test.
    - no-backtick-escape fails the injection and punctuation tests.
    - no-length-cap fails the overlength test.
    - no-text-fence makes the exact-block test fail and the three block tests
      error.
    - reread-report fails the snapshot/TOCTOU test.
- **negctl-restored-baseline**: rc 0, `FollowupTest` OK (16 tests).
- **Sensitivity**: the earlier `followup.py`, with only
  `AGENT_TEXT_LIMIT: int = 1000` appended so that the tests import, was run
  against the head tests.
  - c1f8817a5ebb: rc 1, with 4 failures and 3 errors.
  - 2c30cbedb0ba: rc 1, with 2 failures and 3 errors. The failures include
    the snapshot test: that renderer printed the `@swapped` text written after
    validation.
  - Both restores returned rc 0.
- **negctl-real**: rc 1 for both, and both are refusals.
  - `swap-phases` gives "unfixed phase is not the manifest's test ELF".
  - `patched-unfixed-sha` gives "patched phase reran the unfixed test ELF".
- **containment_test**: rc 32, with Pass 36 and Fail 4. The result is
  **BLOCKED**.
  - `buck2 test fbcode//testinfra/tfa/tests:tfa_production_containment_test`.
    `free-pre-containment.out` records host memory before the run.
  - The four failures are the signed-Codex 0.156.0 preflight cases:
    - `test_real_codex_reviewer_final_message_cannot_be_redirected`
    - `test_real_codex_final_message_cannot_be_redirected_by_tool`
    - `test_real_codex_candidate_handoff_uses_controller_buck`
    - `test_real_codex_native_workspace_sandbox_and_tfa_cleanup`
  - 36/40 is the suite's test count. It is not a pass rate, and it validates
    no candidate fix. The suite remains BLOCKED on these four cases.

## Residual scope

The patch diff and `root_cause` are agent-controlled surfaces that predate this
commit:

- The patch diff is rendered in an adaptive-length fence.
- `root_cause` is limited to one line of at most 500 characters, matching
  `[A-Za-z0-9][A-Za-z0-9 .,;()'/_-]*`.

Both were approved at c1f8817a and are unchanged here.
