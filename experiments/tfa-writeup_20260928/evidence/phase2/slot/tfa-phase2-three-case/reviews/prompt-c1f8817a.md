# Fresh exact-head adversarial review: TFA candidate-fix follow-ups (c1f8817a)

You are a read-only adversarial reviewer. Do not edit, commit, amend, publish,
submit, post comments or assign reviewers anywhere. Do not run `jf`, `arc diff`
or anything outward-facing. Do not run O(repo) `sl` commands. Do not use
grep/find/rg over fbsource; direct reads under `fbcode/testinfra/tfa` are fine.
Pass `--reason "<intent> - sl help <cmd>"` to every `sl` command. Keep any
scratch output under
`/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/reviews/`
and never under `/tmp`. Do not start Hermit replays, builds of the case
checkouts, or investigator agents; the host is under memory pressure.

## Target

- Repository: `/home/newton/work/tfa/fbsource` (Sapling)
- Exact head: `c1f8817a5ebba01e752d295baad1bd9a8fe9dee3`
- Its parent: `3eb9d118337a63f1bfe487d811c32caff06de1dd`. Both are new.
- Base: `38939dd4ac125a90ec0115031a571a593c8910cd`, which the previous fresh
  review approved (`reviews/review-38939dd4.md`).
- Review `sl diff --reason "review" -r 38939dd4 -r c1f8817a` in full. A copy is
  at `reviews/diff-c1f8817a.txt`, and the r7-only delta is at
  `reviews/diff-r7-delta.txt`. Read the new `fbcode/testinfra/tfa/followup.py`
  in full, plus `FollowupTest` and its fixtures in
  `fbcode/testinfra/tfa/tests/test_prototype.py`, the `render-followup` command
  in `main.py`, and the new README paragraph.

What the change does: `render-followup` takes a `tfa.followup-ledger.v1` that
lists witness-replay run directories for an unfixed phase and a patched phase.
It re-judges every run from its own receipt bytes, ignoring recorded labels, and
renders an unposted follow-up comment plus `followup-runs.json`. The verdict is
SUPPORTED, NOT SUPPORTED or INCONCLUSIVE. The r7 commit binds the unfixed
phase's guest ELF to the manifest `artifact_sha256`. It fixes a gap that the
r6 real-evidence negative control found: a swapped-phase ledger was rendered
`NOT SUPPORTED` instead of being refused (`gates/followup-20260925-r6-3eb9d118337a/GAP.md`).

Check the following:
1. Is each run bound to its own bytes?
   - receipt schema
   - stdout, stderr and safehermit files under the run directory
   - guest ELF sha equal to the phase's `guest_sha256`
   - the manifest Hermit hash
   Can a relabelled, copied or foreign run get through?
2. Classification:
   - Does FAIL/PASS come only from the manifest oracle on the run's own output?
   - Is CENSORED only rc 124 with safehermit `unit_result=timeout`?
   - Is NONDETERMINISTIC only the Hermit two-run mismatch?
   - Does anything else end up OTHER, never PASS?
3. Verdict rules:
   - Can SUPPORTED happen with any patched FAIL?
   - Can it happen without an unfixed FAIL for some seed?
   - Can it happen with a CENSORED or NONDETERMINISTIC patched replay of a failing seed?
   - Are the caveats (a witness seed not failing unfixed at this path; a patched non-PASS) surfaced rather than dropped?
4. Phase binding:
   - Are the unfixed phase's manifest ELF and the patched phase's different ELF both enforced?
   - Is any other forgery of the phase structure accepted? Examples: duplicate runs, runs in both phases, empty phases, unknown roles, interrupted attempts that silently vanish.
5. Is the `relocation.py` `_safehermit_fields` extraction behaviour-preserving?
6. Output honesty:
   - Are the outputs opened exclusively and never posted?
   - Does the comment call the patch candidate-only?
   - Does it report CENSORED, NONDETERMINISTIC, OTHER and interrupted runs?
   - Is the README accurate, and does anything overclaim?
7. The evidence-tree ledger builder `followup/build_ledger.py` is not
   committed. Does it list every replay directory without filtering on outcome?
   Are its role assignments defensible? It puts reverted-checkout controls in
   the unfixed phase and assigns `revert-control-seed-N` a fail-witness role
   only for the manifest FAIL seed.

## Evidence (exact head)

Gate directory: `gates/followup-20260925-r7-c1f8817a5ebb/`, with `SUMMARY.md`
and manifest `followup-20260925-r7-c1f8817a5ebb.sha256`. Each gate was its own
unpiped command with `.out` and `.rc`.
- commit-identity: shows the tag line.
- sl-status-pre and sl-status-post: empty.
- arc-lint and pyre: rc 0.
- tfa_phase2_test: 187/187.
- build-cli: rc 0. The par is an lpar launcher; see SUMMARY.md.
- render-*: all SUPPORTED. render-cmp has 12 byte-identical files.
- negctl-*: 9 mutations, each caught and each restored.
- negctl-real: swap-phases and patched-unfixed-sha are both refused.

Containment was run twice at this head:
- Run 1 (`containment_test.*`): 34/40. The four Codex 0.156.0 preflight failures plus two host failures: an `hg_monitor` reference to the removed worktree, and a reverie perf-counter skid panic.
- Those two tests were rerun alone (`containment_rerun-env-flakes.*`): 2/2 pass.
- Run 2 (`containment_test-run2.*`): 36/40, the four Codex failures only, still BLOCKED.

Judge explicitly whether reporting run 2 as the result, with run 1 preserved and disclosed, is legitimate or goalpost moving.

Real stress evidence is under `stress/<case>/{prefix,prefix-fresh,prefix-extra,postfix}/`, the ledgers are under `followup/<case>/ledger.json`, and the rendered outputs are under `stages/<case>/stage3-followup/`. Spot-check at least one CENSORED, one NONDETERMINISTIC and one OTHER run against its rendered classification.

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
base revision. Say explicitly whether the bar was lowered.

## Report

Write your report to
`.../tfa-phase2-three-case/reviews/review-c1f8817a.md` in this form, and also
print it:

```text
Review target: fbsource 38939dd4..c1f8817a5ebba01e752d295baad1bd9a8fe9dee3

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
