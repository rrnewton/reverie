---
name: code-review
description: "Review code changes adversarially for correctness and evidence, with an explicit check that tests, assertions, tolerances, comparators, labels, and gates were not weakened to make the change appear successful. Use whenever reviewing a diff, commit, pull request, test change, validation change, or dispatching an adversarial code review."
---

# Code review

## Purpose

Try to refute the change's correctness claims. Inspect the implementation, its
tests, and the evidence together. A passing suite is not sufficient when the
change also weakens what the suite requires.

This skill owns the substance of adversarial code review. Separate skills may
set reviewer counts, exact-head attestation, validation authority, and landing
policy.

## Establish the review target

Before reviewing, identify the repository, base revision, exact head revision,
and intended behavior. Review the complete diff and the relevant surrounding
code and tests. Bind the verdict to the exact head; a changed head requires a
fresh review.

## Mandatory goalpost-moving check

The coordinator applies this check before dispatching an adversarial review and
includes this complete block in every adversarial-review prompt, not a pointer
to it:

> GOALPOST-MOVING REVIEW RULE
>
> Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
>
> Treat each of these as an explicit review target:
> - weakening an assertion so a test passes
> - widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
> - renaming or relabelling so a failure reads as a pass
> - deleting a check rather than satisfying it

Compare every changed test, threshold, allowlist, skip, label, classification,
and gate with the base revision. Determine whether the implementation now meets
the original requirement or whether the requirement was changed to admit the
implementation. Call out either result explicitly, including when no lowering
of the bar is present.

## Review method

1. Trace the changed behavior through the real execution path. Do not infer
   behavior from names, configuration, or successful exit status alone.
2. Check the claimed evidence against the exact behavior it is supposed to
   prove. Identify missing identity, causation, coverage, or provenance links.
3. Inspect production and test changes together. Require a qualifying case to
   pass and a violating case to be refused when the change introduces or alters
   a guard.
4. Report concrete findings with file and line evidence, impact, and the change
   required to resolve them. Do not substitute a vague procedural objection for
   a technical finding.
5. State a verdict only after resolving the goalpost-moving check and the
   correctness findings against the exact reviewed head.

## Report structure

```text
Review target: <repository> <base>..<exact head>

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

Put findings first. If there are no findings, say so explicitly and still
include the goalpost-moving assessment, residual risks, and verification limits.
