# Handoff: pressure sample classification

Pause reason: the owner redirected all lanes to the emergency pull-request drain. Do not resume this task until `review-zero-pr-linear-state-and-green-main` is closed.

Task: `define-pressure-sample-classification-and-promotion-checks`

## Preserved implementation

- Parent branch `codex/hermit-002`, clean at commit `06986220ef33b4c2bc686e3f6eb5e1b420df78f4` (`Classify accepted pressure sample campaigns`).
- Inner Hermit branch `codex/pressure-sample-classification`, clean at commit `b96defb72053e70d7599a585868622aa27b9afc0` (`Define pressure sample outcome classification`).
- Nothing was pushed or landed.
- The selected cohort was not changed. `hermit/ci/expected-e2e-plan.json` remains SHA-256 `bc48a92f8d3981619bb9211eee12c42b5cfd2af202080293556c0f57c5824e3f`; `hermit/ci/compat-envelope/cells.json` remains SHA-256 `04e623083bb264338b49fcd83aa6e5acf629a259fa370c7322cbe01561f42a5a`.

The implementation defines a fixed ten-repetition classification in the Hermit pressure summary and accepted-series projection. Only 10/10 clean first-attempt product passes are a promotion candidate. Complete samples with a terminal pass but fewer than ten qualifying passes are intermittent. Only ten terminal product failures are confirmed failing. Infrastructure, prerequisite, and no-result samples remain distinct. Missing, duplicate, gapped, malformed, and mixed evidence is incomplete.

Canonical `HOST-INAPPLICABLE` evidence is accepted only from a prepared empty result file plus an exact one-cell harness summary plus a completed nonzero scheduler node. Prepared empty files with proven timeout or OOM are no-result. Nonempty malformed result histories remain missing. The accepted-series reducer uses the highest attempt ordinal as terminal and exact event IDs only for replay deduplication.

## Test evidence

- `python3 ci-hub/series/tests/test_series_invariants.py`: 92 tests passed in 46.898 seconds.
- `./ci/compat-envelope/pressure-test.rs self-test`: passed at the committed Hermit diff, including physical empty-file prerequisite, timeout, and OOM production fixtures.
- `./ci/compat-envelope/scorecard.rs self-test`: passed.
- `cargo fmt --all -- --check`: passed.
- Both repository diff checks passed before commit; both worktrees were clean after commit.

Logs are preserved under `ignored/handoffs/define-pressure-sample-classification-and-promotion-checks/`:

- `series-tests.log`
- `pressure-selftest.log`
- `scorecard-selftest.log`

Initial sandboxed self-test attempts failed because rust-script could not modify `/home/newton/.cache/rust-script`; the same commands passed outside the filesystem sandbox. These were environment failures, not test failures.

## Independent review

The adversarial reviewer approved with no findings at exact pre-commit diff hashes:

- parent: `25bd1aa47ba1dc9ffb4c1676e516c3ed2a1bb248cf46e547eca1976693406664`
- inner Hermit: `a622694fd17873d544723bc6fd4d0dc3df3ceb279a493d9a90d45fb617a61ace`

The review explicitly found no weakened assertions, tolerances, labels, gates, or deleted checks. Earlier review findings about withheld prerequisite rows and prepared-empty timeout/OOM files were fixed and re-reviewed.

## Resume

After `review-zero-pr-linear-state-and-green-main` closes, verify both branch tips and clean worktrees, reread this handoff and the TaskGraph notes, then decide the landing path. Do not recreate the changes. The direct task close was refused by the closure verifier because these commits are local and unlanded; leave this task open until the owner-directed dependency is satisfied and the work can be landed or explicitly dispositioned.

---

# Handoff: check.lint_checks authority refusal correction

Task: `a-gate-reports-a-network-error-as-red-turning-an-outage-into-fleet-wide-failure`

The reviewed correction is committed locally at `de1b1de0b3b517661e1bc271a65e54443aff0744` on branch `codex/network-gate-refusal-fix` in `reconstructions/network-gate-refusal`, based on Hermit main `c7520a6c38731128ec3e0bf802d80b0d1cca80c0`. The worktree is clean and one commit ahead of `origin/main`.

The branch was subsequently published under the explicit `publish-the-network-gate-refusal-fix` task. Origin resolves `codex/network-gate-refusal-fix` to the exact reviewed commit, and the open pull request is https://github.com/rrnewton/hermit/pull/2989. It is implemented and unlanded; landing belongs to the drain lane.

The change makes `scripts/test-authority-obtained-once.sh` emit `NO-RESULT-CASE` only for the shared authority helper's reserved exit 3. Reachable HTTP 403, digest mismatch, and ordinary errors remain nonzero. Two added tests exercise the real script startup path under simulated HTTP 504 and HTTP 403.

Verification at the committed content: authority suite 13/13 passed with no skips in 2.391 seconds; full oracle passed in 28.949 seconds; Bash syntax, ShellCheck at error severity, and `git diff --check` passed. A full oracle run with a dedicated `TMPDIR` left zero residue. Independent adversarial review approved the final diff with no findings and explicitly found no weakened assertion, tolerance, exemption, label, or gate.

Before landing, verify the pull request remains at the recorded exact head and use the drain lane. Do not recreate the patch or land it from this slot.
