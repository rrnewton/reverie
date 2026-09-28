You are the independent external Claude-family adversarial reviewer for one frozen Reverie pull request.

Review target: https://github.com/rrnewton/reverie/pull/628
Repository checkout: /home/newton/work/dev-hermit/worktrees/slots/kvm-sendfile-lseek-lowword-20260923
Base: 7bc49f4c4d63018adba61d49246e847569518e33
Exact head: 730123acb922cfa30fa1bb9281391776088fb35d
Expected SHA-256 of `git diff --binary BASE..HEAD`: 460ccbcaa5d5262c977179e2a8391ac59d05db76042168fcc6af05a9875caa8a
Frozen artifact: ignored/recovery/sendfile-lseek-lowword-20260923/sendfile-lseek-frozen-post-rebase.diff

This is a read-only exact-artifact review. Do not edit source or tests, commit, push, use GitHub, or change the checkout. Build/test outputs under existing ignored or target directories are permitted. Use only exact named files or focused source subtrees; never recursively search `/tmp`, the slot root, or a parent workspace. Do not inspect or change other worktrees.

The claimed change is deliberately narrow: Linux consumes only the low 32-bit descriptor word for sendfile's output fd, sendfile's input fd, and lseek's fd. Existing fdinfo, captured-output, ordinary-file, error-ordering, and offset behavior must remain unchanged. This does not change `lseek` whence decoding or CLOSE_RANGE_UNSHARE. Tests claim exact native/KVM stdout, stderr, and exit-status parity, not L2/log/replay parity.

First verify HEAD, parent/base, clean tracked state, frozen-artifact byte identity, and expected diff hash. Inspect the complete diff and relevant surrounding implementation. Trace all three decoder sites and downstream routes. Adversarially check signed bit 31, high-word aliases, sendfile explicit-offset side effects, source positions, captured stdout, lseek routing, and whether each old decoder is independently discriminated. Call out pre-existing behavior separately from regressions.

If practical, independently run the exact focused unit and required real-KVM test; do not broaden to unrelated suites. Review these preserved full-suite facts without erasing any: serial exact head passed 817/817; a later parallel exact-head run passed 817/817; two earlier parallel exact-head runs each passed 816/817 and failed unrelated existing SIGPIPE and accept/EAGAIN tests. Determine whether the clean-run alternative gate is satisfied, while preserving causality limitations.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

Compare every changed test, threshold, allowlist, skip, label, classification, and gate with the base revision. Determine whether the implementation now meets the original requirement or whether the requirement was changed to admit the implementation. Call out either result explicitly, including when no lowering of the bar is present.

Return this structure with concrete file:line evidence:

Review target: <repository> <base>..<exact head>

Artifact identity: <verified/not verified, actual hash>

Findings:
- <severity>: <file:line> — <problem, impact, and required change>

Goalpost-moving assessment:
- Assertions weakened: yes/no — <evidence>
- Tolerance widened, exemption added, case skipped, or comparator relaxed: yes/no — <evidence>
- Failure renamed or relabelled as a pass: yes/no — <evidence>
- Check deleted instead of satisfied: yes/no — <evidence>

Residual risks: <explicit list>
Verification: <commands/results and limitations>
Verdict: approve / changes requested, bound to exact head
