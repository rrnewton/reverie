# Exact inventory followup review

**APPROVE the test-only delta** `1145835fc804f47ae48b29b89009e6937184175a..d750224cbf10c5a3b35eec40fa4d57d0e98493cb`, tree `8a41f1130199bc2dde1f60e6d053f1817375c94a`. No blocking finding in this delta. This is separate from the still-running external preservation-semantics review at114 and does not predict its verdict.

Only `py/tests/test_packaging_infrastructure.py` changes: exact total654→797, exact ordinary98→241, and the eight appended preservation method names are added to the exact expected set. Removing precisely those eight names and restoring precisely those two integers makes the complete file AST identical to base. All previous assertions and names, disjointness/complete union, mapped556 and mapped-root2 checks survive. The actual collection helper still requires successful strict-marker pytest collection and has no new skip, filter, fallback or comparator relaxation.

I independently authenticated all eight retained before/after collection receipts/raw outputs against their recorded hashes and recomputed the sets. All654 old identities survive; all143 additions are ordinary cases from exactly those eight methods. Ordinary becomes241; mapped556 and the exact two mapped-root identities are unchanged. Both partitions remain disjoint/exhaustive. The before/after full tracked-source archive hashes were independently recomputed from their immutable Git objects and match the retained source bindings. Neither collection nor tests were rerun by me.

The directly affected control's retained actual output is one pass in1.98s (outer2.28wall,1.67user+0.56system CPU), exit0. That is one control, not successful normal validation. The original normal114 failure remains actual exit2/587.73wall with2967pass/1fail in its first partition; later stages were not reached. The new normald750 run is pending, and no full-suite pass is claimed.

Both retention implementation and lifecycle test bytes/modes atd750 are identical to114, and parentfba5 is untouched. The active external review's80 frozen inputs remain unchanged; its verdict must remain scoped to114+fba5. This additional source-bound assessment covers only the count/method-set repair, not new retention semantics, real1839 liveness, admission, deletion or result qualification.

Goalpost assessment: no assertion was weakened, no tolerance/comparator was relaxed, no old case/name was skipped or deleted, and no failure was relabelled as a pass. The exact expected inventory is increased to the independently collected population while preserving the old population and substantive partition checks. The old failure is retained.

Patch SHA256 `cd9bf13efb96c3cafa2c12e2c88cef1190fd8843b9138ccd0ffe682af37739b0`; proof SHA256 `cec98ff6fc0a04d57241feff5f456de54787c979dd3685dc843902e2a59205b3`. READBACK.json contains the exact source and all observed collection/input bindings.

GOALPOST-MOVING REVIEW RULE
Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it
