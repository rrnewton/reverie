# Fresh exact-head adversarial review: TFA follow-up hypothesis wording and phase-two RESULTS.md (2c30cbed)

You are a read-only adversarial reviewer. Do not edit, commit, amend, publish,
submit, post comments or assign reviewers anywhere. Do not run `jf`, `arc diff`
or anything outward-facing. Do not run O(repo) `sl` commands. Do not use
grep/find/rg over fbsource; direct reads under `fbcode/testinfra/tfa` are fine.
Pass `--reason "<intent> - sl help <cmd>"` to every `sl` command. Keep any
scratch output under
`/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/reviews/`
(written `$E3/reviews/` below) and never under `/tmp`. Do not start Hermit
replays, builds of the case checkouts, or investigator agents; the host is under
memory pressure.

## Target

- Repository: `/home/newton/work/tfa/fbsource` (Sapling)
- Exact head: `2c30cbedb0baddb84eceb14fc76d8ff1a7fdef29`
- Parent: `c1f8817a5ebba01e752d295baad1bd9a8fe9dee3`, which the previous fresh
  review approved (`reviews/review-c1f8817a.md`).
- Review `sl diff --reason "review" -r c1f8817a -r 2c30cbed` in full. A copy is
  at `reviews/diff-2c30cbed.txt`.
- Read the new `fbcode/testinfra/tfa/followup.py` in full, plus `FollowupTest`
  and its fixtures in `fbcode/testinfra/tfa/tests/test_prototype.py`, and the
  changed README paragraph.
- Also review the uncommitted evidence-tree document `$E3/RESULTS.md` (the
  `tfa-phase2-three-case` directory).

What was asked: a scientific-clarity review
(`reviews/review-phase1-phase2-scientific-clarity.md`) requested the following:

- A context-free phase-two summary.
- Follow-up comments that present the root causes as investigator hypotheses,
  with the caveats from the reports:
  - thrift: not instrumented;
  - asyncssl: ACK-queued ordering is inferred, and the production-drain
    alternative was rejected on precedent, not disproof;
  - httpcoro: no DBG drain log.
- Definitions of the run roles and the SUPPORTED rule, and a rerun command.
- Honest labelling of the containment 36/40 as BLOCKED by the four Codex
  0.156.0 preflight cases. It is not a pass rate and not validation of any fix.

The judging, verdict and binding code approved at c1f8817a must be unchanged.

Check the following:

1. **Behaviour preservation.**
   - Are `judge_run`, `_verdict`, `_bound_run` and the phase-binding checks in
     `validated_followup` byte-for-byte unchanged in behaviour?
   - Do the new helpers (`_hypotheses`, `_measured_command`, `_commands`,
     `_fence`) alter any verdict or refusal? The only new refusals should be a
     changed report and a receipt without `cwd`.
   - Are the real `followup-runs.json` files still byte-identical to
     `stages/<case>/stage3-followup/`?
2. **Hypothesis honesty.**
   - Does every rendered comment
     (`stages/<case>/stage4-followup-hypothesis-r2/followup-comment.md`) call
     the root cause an unproven investigator hypothesis?
   - Does it quote the report's contradicting evidence verbatim (after
     whitespace collapse) and list the other ranked hypotheses?
   - Does the verdict-rule text avoid claiming that the patch fixed the bug or
     that the root cause is proven?
   - Is the same-seed caveat accurate?
3. **Measured commands.**
   - Are the printed commands exactly the receipt `cwd` plus `argv`?
   - Is the seed selection (lowest seed that FAILed unfixed, its unfixed FAIL,
     and its first patched replay) correct?
   - Is shell quoting sound?
   - Is the warning about recorded run directories and ELF sha adequate?
4. **Tests.**
   - Are the three new `FollowupTest` methods meaningful?
   - Was any pre-existing assertion weakened, removed or retargeted?
   - The fixture gained `receipt["cwd"]`. Does that mask any existing check?
5. **RESULTS.md accuracy.**
   - Check every count, seed table, sha, path, role definition, verdict and
     limitation against the files: ledgers, `followup-runs.json`,
     `stress/<case>/`, `stress/postfix_20260925.sh`, `witness/`, the
     investigator reports, and the r7 gates.
   - Are the confidence levels calibrated to the evidence?
   - Is anything overclaimed, or are any of the review's caveats missing?
   - Are the reproducer commands runnable as written? Check `witness.sh` and
     `run_witness.py` argument rules, allowed `--out` roots, and the ELF paths
     and shas. Do not run them.
6. **36/40.** Is it labelled BLOCKED by the four named Codex 0.156.0 preflight
   cases everywhere it appears, including RESULTS.md, the commit message and
   the gate SUMMARY? Is it never used as a pass rate or as fix validation?

## Evidence (exact head)

The gate directory is `gates/followup-20260925-r8-2c30cbedb0ba/`, with
`SUMMARY.md` and the manifest `followup-20260925-r8-2c30cbedb0ba.sha256`. Each
gate was its own unpiped command with `.out` and `.rc`. `run-gates.sh` is the
exact script that was run. SUMMARY.md states the results; verify each claim
against the `.out` and `.rc` files. The gates cover:

- commit identity and clean status;
- lint and pyre;
- the full `tfa_phase2_test`;
- the CLI build;
- initial and follow-up renders with cmp;
- the 9 negative-control mutations with restores and the restored baseline;
- parent-renderer sensitivity;
- the 2 real-ledger refusals;
- containment.

The pre-commit sensitivity evidence is in `gates/followup-r8-sensitivity/`.

> GOALPOST-MOVING REVIEW RULE
>
> Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
>
> Treat each of these as an explicit review target:
> - weakening an assertion so a test passes
> - widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
> - renaming or relabelling so a failure reads as a pass
> - deleting a check rather than satisfying it

Compare every changed test, threshold, label, classification and gate with the
parent revision. Say explicitly whether the bar was lowered.

## Report

Write your report to `$E3/reviews/review-2c30cbed.md` in this form, and also
print it:

```text
Review target: fbsource c1f8817a..2c30cbedb0baddb84eceb14fc76d8ff1a7fdef29 (+ uncommitted tfa-phase2-three-case/RESULTS.md)

Findings:
- <severity>: <file:line> — <problem, impact, and required change>

Goalpost-moving assessment:
- Assertions weakened: yes/no — <evidence>
- Tolerance widened, exemption added, case skipped, or comparator relaxed: yes/no — <evidence>
- Failure renamed or relabelled as a pass: yes/no — <evidence>
- Check deleted instead of satisfied: yes/no — <evidence>

Verification: <commands or evidence inspected, with limitations>
Verdict: approve / changes requested
```
