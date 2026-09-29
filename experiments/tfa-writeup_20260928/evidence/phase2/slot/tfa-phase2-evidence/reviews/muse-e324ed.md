# Adversarial review — TFA phase-two amended head (metadata-only claim)

Review target: Sapling `fbsource` `/home/newton/work/tfa/fbsource` · Base `435d79639fc043c472d31a87e5a4a4b6f658ed32` · Exact reviewed head `e324ed1880a44a731deec80271a84bb0ccf5ec3b` · Prior reviewed head `ef4885b60dc26a1a73321b83d834c97179e0ec64` · Prior review `reviews/muse-ef4885b.md` (Approve, findings F1/F2 minor — both inherited unchanged, see below).
Attestation: `sl id` = `e324ed1880a4`, `sl status` clean before/after. Read-only; no source edit, no publish, no tests run by reviewer. **Verdict bound to `e324ed1880a44a731deec80271a84bb0ccf5ec3b` only.**

## Findings (first)

No blocking finding. The metadata-only amendment claim is **CONFIRMED** on every checkable axis:

- **F0 (claim confirmed) Same parent, same manifest, empty inter-head diff.** Both heads: `parent:435d79639fc0` (= base, `sl log -T {parents}`). Both manifests: `1af2264809c65fc4738b70b5cdab9efa02cea690` (`sl log -T {manifest}`). `sl diff -r ef4885b60dc2 -r e324ed1880a4 --stat` prints nothing. `sl diff base..e324ed18 --stat` = prior reviewed stat exactly (5 files, `+567/−59`: `README.md`, `process.py`, `test_process_migration_helper.py`, `test_production_containment.py`, `test_prototype.py`). Source behaviour is therefore byte-identical to the head approved in `muse-ef4885b.md` — sealed provenance, fail-closed containment, diagnostic-only candidate treatment all carry over without re-litigation; no required action.
- **F1 (message delta = exactly two lines, both truthful).** `diff` of `{desc}` prior vs current: (1) who-am-i tag `[hermit2, tfa-phase2-cyber, gpt-5.6-sol, …] → [hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]` — corrects attribution to the implementing agent (prior tag named a different model family; tag stays in body line 1, title unchanged); (2) Test Plan `tfa_phase2_test (132/132)` → `(132/132 in the original checkout; 131/132 in a fresh checkout, where test_real_packaged_hermit_is_bound_to_blocked_provenance fails closed as PROVENANCE_MISMATCH because the pinned hermit-test-bin sha covers a build whose fb_build_info stamps revision, path and build time, a pre-existing pin not changed here)`. No other line differs (title, summary, remaining Test Plan bullets incl. production `36/40` unchanged).
- **F2 (131/132 wording accurate against source + evidence).** Test `fbcode/testinfra/tfa/tests/test_prototype.py:1192-1224` expects `HermitError "packaged Hermit compatibility is BLOCKED"` + identity `state==BLOCKED`; `fbcode/testinfra/tfa/hermit.py:1716-1717` sets `PROVENANCE_MISMATCH` when `packaged_sha256 != packaged["sha256"]`, checked *before* the BLOCKED arm (`:1718`), raising `"packaged Hermit compatibility is PROVENANCE_MISMATCH…"` (`:1762-1775` region) — a fresh-link binary (fb_build_info stamps revision/path/time, saved section `gates-tfa-checkout/hermit-test-bin.fb_build_info.bin`) therefore fails the test **closed** (stricter refusal, wrong expected string/state). Evidence `gates-tfa-checkout/UNIT-131-OF-132-FINDING.md` + `unit.out` (`Pass 131 Fail 1`, rc file `unit.rc=32` — buck test failure exit, recorded, not hidden) documents measured shas (`9bf7064a…` pin vs `4e8e4381…` fresh build). "Pre-existing pin not changed here" verified: `hermit.py` and `hermit-import-compatibility.v1.json` are absent from base..head file list. The finding doc correctly refuses to re-pin/normalize (owner decision) — the amendment records, not repairs.
- **Inherited minors (unchanged, non-blocking):** prior F1 launch exec-by-path micro-window and F2 diagnostic receipt labelling — tree-identical code/evidence, status unchanged.

## Goalpost-moving (four explicit answers)

- Assertions weakened: **No** — zero source/test bytes changed inter-head; base..head test changes are those already reviewed at `ef4885b`.
- Tolerance widened / exemption / skip / comparator relaxed: **No** — `PROVENANCE_MISMATCH` is a pre-existing stricter arm, not a new exemption; the disclosure narrows the *claimed* result (131 fresh vs 132 original), the opposite direction.
- Renamed/relabelled failure as pass: **No** — the amendment does the reverse: a previously unconditional `132/132` claim is corrected to name the failing test, its fail-closed state, and cause. Production `36/40` red remains stated red.
- Check deleted vs satisfied: **No** — no check touched; the pin defect is disclosed for owner decision, explicitly not fixed by re-pinning in this commit.

## Verification limits

Reviewer ran no `buck`/`sl` mutations; 131/132 and 132/132 figures rest on gate logs at the identical tree (`unit.*` in this checkout at `ef4885b` — same manifest, so equally binding on `e324ed18` content; note any future build stamps the new revision, which is itself the disclosed defect mechanism). Manifest/parent/message comparisons are direct `sl` outputs quoted above. Prior full-diff/evidence review (`muse-ef4885b.md`) incorporated by tree identity, not re-executed.

## Verdict

**Approve** `e324ed1880a44a731deec80271a84bb0ccf5ec3b`. Amendment is metadata-only as claimed; disclosures are accurate and more conservative than the prior head's.
