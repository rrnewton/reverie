# Fresh exact-head adversarial review: TFA follow-up hypothesis wording, agent-text policy and phase-two RESULTS.md (b8b85d00)

You are a read-only adversarial reviewer. Do not edit, commit, amend, publish,
submit, post comments or assign reviewers anywhere. Do not run `jf`, `arc diff`
or anything outward-facing. Do not run O(repo) `sl` commands. Do not use
grep/find/rg over fbsource; direct reads under `fbcode/testinfra/tfa` are fine.
Pass `--reason "<intent> - sl help <cmd>"` to every `sl` command. Keep any
scratch output under
`/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/reviews/`
(written `$E3/reviews/` below) and never under `/tmp`. Do not start Hermit
replays, builds of the case checkouts, or investigator agents; the host is under
memory pressure. You may run the `FollowupTest` suite read-only from the Buck
link tree, but only with a fresh `PYTHONPYCACHEPREFIX` under `$E3/reviews/`:

```sh
cd '/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/c62dfa3398b37794/testinfra/tfa/tests/__tfa_phase2_test__/tfa_phase2_test#link-tree' && PYTHONPYCACHEPREFIX=$E3/reviews/pyc-review /usr/local/fbcode/platform010/bin/python3.12 -m unittest -v testinfra.tfa.tests.test_prototype.FollowupTest
```

## Target

- Repository: `/home/newton/work/tfa/fbsource` (Sapling)
- Exact head: `b8b85d00462b6f159641aee73a49321ca863609d`
- Parent: `c1f8817a5ebba01e752d295baad1bd9a8fe9dee3`, which the previous fresh
  review approved (`reviews/review-c1f8817a.md`).
- `2c30cbedb0ba` was an earlier, never-reviewed version of this same commit,
  amended into `b8b85d00`. It is superseded.
- Review `sl diff --reason "review" -r c1f8817a -r b8b85d00` in full. A copy is
  at `reviews/diff-b8b85d00.txt`.
- Read in full:
  - the new `fbcode/testinfra/tfa/followup.py`;
  - the `CandidateSummary`, `Hypothesis`, `_validate_hypotheses` and
    `validate_candidate_report` parts of `fbcode/testinfra/tfa/report.py`;
  - `FollowupTest` and its fixtures in
    `fbcode/testinfra/tfa/tests/test_prototype.py`, including `_valid_report`
    and `_followup_ledger`;
  - the changed README paragraph.
- Also review the uncommitted evidence-tree document `$E3/RESULTS.md` (the
  `tfa-phase2-three-case` directory).

## What was asked

### 1. The scientific-clarity review

The review is `reviews/review-phase1-phase2-scientific-clarity.md`. It asked
for:

- a context-free phase-two summary;
- follow-up comments that present the root causes as investigator hypotheses,
  with the caveats from the reports:
  - thrift is not instrumented;
  - asyncssl's ACK-queued ordering is inferred, and the production-drain
    alternative was rejected on precedent, not disproof;
  - httpcoro has no DBG drain log;
- definitions of the run roles and the SUPPORTED rule;
- a rerun command;
- honest labelling of the containment 36/40 as BLOCKED by the four Codex
  0.156.0 preflight cases. It is not a pass rate and not validation of any fix.

The judging, verdict and binding code approved at c1f8817a must be unchanged.

### 2. An adversarial finding on 2c30cbed, quoted in full

> followup.py now inserts investigator-controlled hypotheses[].claim and
> .contradicting directly into a task comment. report._validate_hypotheses only
> requires non-empty strings, and _one_line only collapses whitespace, so an
> untrusted investigator can inject Markdown/HTML/@mentions or unbounded text
> into the eventual posting surface. Also _hypotheses rereads report.json after
> validate_candidate_report, creating a TOCTOU window. Before final review, make
> the renderer consume one validated snapshot, apply a bounded plain-text policy
> (or explicit safe Markdown escaping) to every newly rendered agent field, and
> add positive plus malicious-injection/overlength tests without weakening
> existing validation.

### The claimed resolution

- `validate_candidate_report` now returns `selected_hypothesis_rank` and a
  `hypotheses` tuple of `(rank, claim, contradicting)` in `CandidateSummary`,
  taken from the same parse it validated.
- `followup.py` uses only that snapshot.
- `_agent_text`:
  - collapses whitespace;
  - drops Unicode category C characters;
  - maps backticks to `'`;
  - cuts at `AGENT_TEXT_LIMIT` = 1000 with a marker;
  - refuses a field that is empty after filtering.
- The two fields are rendered inside a fixed three-backtick `text` fence.

## Check the following

1. **Security of the agent-text path.**
   - Is every newly rendered, investigator-controlled field routed through
     `_agent_text` and kept inside the fence?
   - Can any input close the fence, start a new Markdown or Remarkup block,
     produce a live @mention, HTML or link outside the fence, or smuggle
     bidi, zero-width or ANSI characters?
   - Consider Unicode edge cases: line and paragraph separators, NEL,
     characters in other categories that render invisibly or as line breaks,
     and combining marks. Consider also whether truncation can split a
     surrogate pair or grapheme in a harmful way.
   - Is the refusal of text that is empty after filtering sound?
   - Is 1000 a reasonable bound? The longest real field is 751 characters.
   - Is anything else newly rendered and agent-controlled that is not bounded?
     State the pre-existing surfaces as out of scope: `root_cause` has strict
     regex validation, and the patch diff has an adaptive fence approved at
     c1f8817a.
2. **Snapshot and TOCTOU.**
   - Is there any remaining read of the report file after validation in the
     follow-up path?
   - Are the new `CandidateSummary` fields populated only from the validated
     parse?
   - The fields default to `0` and `()`. Could a caller build a
     `CandidateSummary` without them and reach the renderer? Is the resulting
     refusal correct?
   - Are other `CandidateSummary` users (`orchestrator.py`, `pipeline.py`,
     `seal_candidate_report`, `test_production_containment.py`) unaffected?
3. **Behaviour preservation.**
   - Are `judge_run`, `_verdict`, `_bound_run` and the phase-binding checks
     unchanged in behaviour?
   - Are the only new refusals these three: a receipt without `cwd`, a
     missing selected hypothesis, and a field that is empty after filtering?
   - Are the real `followup-runs.json` files still byte-identical to
     `stages/<case>/stage3-followup/`?
4. **Hypothesis honesty.**
   - Does every rendered comment
     (`stages/<case>/stage4-followup-hypothesis-r3/followup-comment.md`) call
     the root cause an unproven investigator hypothesis?
   - Does it quote the report's contradicting evidence (whitespace collapsed,
     backticks mapped) and list the other ranked hypotheses?
   - Does the verdict-rule text avoid claiming that the patch fixed the bug or
     that the root cause is proven?
   - Is the same-seed caveat accurate?
5. **Measured commands.**
   - Are the printed commands exactly the receipt `cwd` plus `argv`?
   - Is the seed selection correct?
   - Is shell quoting sound?
6. **Tests.**
   - Are the seven new `FollowupTest` methods meaningful?
   - Is the malicious-input test's exact expected block correct, and does it
     cover the finding?
   - Was any pre-existing assertion weakened, removed or retargeted? The
     exact-block assertion in
     `test_root_cause_is_rendered_as_a_caveated_hypothesis` changed layout.
     Confirm that it still asserts the rank, the caveat, the contradicting
     text and the alternatives.
   - The fixture gained `receipt["cwd"]`. Does that mask an existing check?
7. **RESULTS.md accuracy.**
   - Check every count, seed table, sha, path, role definition, verdict and
     limitation against the files: ledgers, `followup-runs.json`,
     `stress/<case>/`, `stress/postfix_20260925.sh`, `witness/`, the
     investigator reports, and the r7 and r8 gates.
   - Are the confidence levels calibrated?
   - Is anything overclaimed, or are any of the review's caveats missing?
   - Are the reproducer commands runnable as written? Do not run them.
8. **36/40.** Is it labelled BLOCKED by the four named Codex 0.156.0 preflight
   cases everywhere it appears, including RESULTS.md, the commit message and
   the gate SUMMARY? Is it never used as a pass rate or as fix validation?

## Evidence (exact head)

The gate directory is `gates/followup-20260925-r8-b8b85d00462b/`, with
`SUMMARY.md` and the manifest `followup-20260925-r8-b8b85d00462b.sha256`.
`run-gates.sh` is the exact script that was run. Each gate was its own
unpiped command with `.out` and `.rc`. Verify each SUMMARY claim against the
`.out` and `.rc` files.

The gates cover:

- commit identity, and clean status before and after;
- lint on four files, and pyre;
- the full `tfa_phase2_test`;
- the CLI build;
- initial and follow-up renders, with cmp against stage 1-2, stage4-r3 and
  stage 3;
- 14 negative-control mutations, each with a restore check:
  - the 9 approved at c1f8817a;
  - 5 new plain-text and snapshot mutations;
- the restored baseline;
- parent-renderer sensitivity against `c1f8817a` and `2c30cbed`;
- the 2 real-ledger refusals;
- containment.

Every Python run used a fresh `PYTHONPYCACHEPREFIX`. Two equal-size mutants
written in the same second otherwise share `.pyc` validation keys. The
pre-commit sensitivity run hit exactly that, and it is recorded in
`gates/followup-r8b-sensitivity/`. Assess whether earlier gate runs without a
fresh prefix, including r7, could have been affected, and whether this run
closes that gap.

> GOALPOST-MOVING REVIEW RULE
>
> Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
>
> Treat each of these as an explicit review target:
> - weakening an assertion so a test passes
> - widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
> - renaming or relabelling so a failure reads as a pass
> - deleting a check rather than satisfying it

Compare every changed test, threshold, allowlist, skip, label, classification
and gate with the parent revision. Determine whether the implementation now
meets the original requirement, or whether the requirement was changed to
admit the implementation. Say explicitly whether the bar was lowered.

## Report

Write your report to `$E3/reviews/review-b8b85d00.md` in this form, and also
print it:

```text
Review target: fbsource c1f8817a..b8b85d00462b6f159641aee73a49321ca863609d (+ uncommitted tfa-phase2-three-case/RESULTS.md)

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

Put findings first. If there are none, say so explicitly, and still include the
goalpost-moving assessment, residual risks and verification limits.
