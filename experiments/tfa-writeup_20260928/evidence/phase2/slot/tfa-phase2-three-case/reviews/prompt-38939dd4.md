# Fresh exact-head adversarial review: TFA relocation evidence binding (38939dd4)

You are a read-only adversarial reviewer. Do not edit, commit, amend, publish,
submit, post comments or assign reviewers anywhere. Do not run `jf`, `arc diff`
or anything outward-facing. Do not run O(repo) `sl` commands. Do not use
grep/find/rg over fbsource; direct reads under `fbcode/testinfra/tfa` are fine.
Pass `--reason "<intent> - sl help <cmd>"` to every `sl` command. Keep any
scratch output under
`/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/reviews/`
and never under `/tmp`.

## Target

- Repository: `/home/newton/work/tfa/fbsource` (Sapling)
- Exact head: `38939dd4ac125a90ec0115031a571a593c8910cd`
- Parent: `7690157882fb674f65a62ebfd091ad1dfe4bf1b4`, which the previous fresh
  review approved. That review found one gap: `_bound_receipt` should reject
  duplicate safehermit report keys and require the report's `binary_sha256` to
  equal `case.hermit_sha256`. It also required wrong-hash and
  conflicting-duplicate-key negative tests and no weakening of any gate.
- Review `sl diff --reason "review" -r 76901578 -r 38939dd4` in full. Read the
  surrounding code in `fbcode/testinfra/tfa/relocation.py` and
  `fbcode/testinfra/tfa/tests/test_prototype.py` (`RelocationTest` and the
  `_relocated_attempt` fixture). For context, you may also inspect
  `bb4ff9f3..76901578`.

Check the following:
1. Is every report key parsed only once, with any repeat refused?
2. Is the Hermit hash from the report bound to the manifest for every decided
   and censored attempt?
3. Do the new tests exercise a wrong hash and conflicting duplicates?
4. Were the two existing forged-report subtests changed only to keep reaching
   their intended property?
5. Is the README's new offline-renderer symlink TOCTOU limitation accurate, and
   does anything claim runtime containment?

## Evidence (exact head)

Gate directory: `.../tfa-phase2-three-case/gates/relocation-20260925-r5-38939dd4/`.
Each gate was run as its own unpiped command, with its `.out` and `.rc` saved.
The directory's manifest is `relocation-20260925-r5-38939dd4.sha256`.
- `commit-identity`: the head carries the tag line.
- `sl-status-pre` and `sl-status-post`: empty.
- `arc-f` and `arc-lint` on the five stack files: rc 0.
- `pyre` (`arc pyre check-changed-targets`): rc 0.
- `tfa_phase2_test`: rc 0, 178/178.
- `build-cli`: rc 0.
- `render-{thrift,asyncssl,httpcoro}`: rc 0. `render-cmp` shows byte identity
  with `stages/<case>/stage1-2-relocated/`.
- `containment_test`: rc 32, 36/40. All four failures are the signed Codex
  0.156.0 preflight blocker, which stays BLOCKED and is not green. One of them
  also reports a secondary fixture-cleanup failure: a host `chataccd` subscribed
  to the disposable checkout (`chataccd-ps.out`, `fixture-recovery.sha256`).
- `negctl-*`: five mutations of committed `relocation.py`, each restored and
  checked with `sha256sum -c` against `negctl-relocation.head.sha256`:
  `no-oracle`, `no-repeat-check`, `no-binary-check`,
  `no-censored-oracle-refusal`, `no-realpath`. Each fails its intended tests
  (rc 1). `negctl-restored-baseline` is 12/12.

Earlier red or invalid evidence, preserved:
- `gates/relocation-20260925-r4-335ed55b/SUPERSEDED.md`: pyre was red on the
  first commit of the prior change.
- `gates/relocation-20260925-r3-bb4ff9f3/`: containment 35/40, with a chataccd
  fifth failure.
- `gates/relocation-20260925/INVALID.md`.

Verify that the claims above match the files.

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

Write your report to
`.../tfa-phase2-three-case/reviews/review-38939dd4.md` in this form, and also
print it:

```text
Review target: fbsource 76901578..38939dd4ac125a90ec0115031a571a593c8910cd

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
