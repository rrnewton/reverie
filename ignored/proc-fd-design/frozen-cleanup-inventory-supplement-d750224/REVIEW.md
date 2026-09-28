# Supplemental adversarial source review — AU inventory-only delta

**Review target:** agent-utils `1145835fc804f47ae48b29b89009e6937184175a` → **`d750224cbf10c5a3b35eec40fa4d57d0e98493cb`**, tree `8a41f1130199bc2dde1f60e6d053f1817375c94a`, sole parent `1145835f`. Patch SHA256 `cd9bf13efb96c3cafa2c12e2c88cef1190fd8843b9138ccd0ffe682af37739b0`, 1,830 bytes, two hunks, one file: `py/tests/test_packaging_infrastructure.py`. Parent dev-hermit stays `fba5d50c96756889bc5c2d3e6caffb9adc8bcb7a`. Read/Grep/Glob only, against the frozen copies under `frozen-cleanup-inventory-supplement-d750224/`. I did not treat the prior APPROVE (`b4bb147c…`) as authority, and I did not read live source.

## Findings

**No blocking defect.** Two nonblocking observations, both pre-existing and neither introduced by this delta:

1. **`py/tests/test_packaging_infrastructure.py:405-464` — the name assertion is per *method*, not per *case*.** It compares `{node.split("::",1)[1].split("[",1)[0] for node in ordinary}` against 56 names, so the only guard on per-method parameterization counts is the three integers at lines 402-404. A future change that moved cases between two already-listed methods would be caught only by the totals, not attributed. Design property of the inventory, unchanged here; no correction required.
2. **`py/tests/test_packaging_infrastructure.py:18-50` — the three constants are bound to the collecting interpreter and plugin set.** The retained receipts used `/usr/local/fbcode/platform010/bin/python3.12`; the test itself uses `sys.executable`, and `-p no:cacheprovider` disables only the cache plugin. A plugin that adds or removes parameterizations would move 797/241/556. Pre-existing; the before and after collections used the identical helper command, so the *delta* is not affected.

### The delta does exactly what it claims, verified from source

- Hunk 1 raises `len(all_tests)` 654→797 and `len(ordinary)` 98→241 (head lines 402-403). `len(mapped) == 556` (line 404) is untouched.
- Hunk 2 adds eight names, all `test_current_incomplete_frozen_*`, to the expected ordinary name set. **No name removed** — I read base lines 394-505 and head lines 394-513 side by side; the 48 base names are all present and in place, the comparator is still `==` (head line 407, base line 407), not `<=`.
- **Byte arithmetic independently confirms nothing else changed.** Head file 21,532 bytes vs base 20,896 = +636. The eight name literals total 539 characters; each added line costs 8 indent + 2 quotes + 1 comma + 1 newline = 12, so 539 + 96 = 635, plus exactly 1 byte for `98`→`241` (`654`→`797` is length-neutral) = 636. The two hunks account for the whole file delta with no slack.
- The collection helper is unchanged and has **no** hidden selector: no `-k`, no `--deselect`, no allowlist, `--strict-markers` still present, and `assert completed.returncode == 0` still fails closed on a broken collection rather than returning an empty set (lines 19-50).
- Structural checks all survive: `ordinary.isdisjoint(mapped)` (400), `ordinary | mapped == all_tests` (401), `mapped_root == {negative, exclusion_root}` exact two-element equality (474), the four `in mapped` / `not in ordinary` membership checks (475-494), and the Makefile assertions (501-513).
- **The Makefile assertions are true of the actual frozen Makefile.** After `\<newline>`→space and whitespace collapse, `Makefile:97-101` yields the asserted `unshare --user --map-root-user --pid --fork --mount-proc "$$test_python" ../scripts/pid_namespace_init.py -- python3 -m pytest … -m 'not ordinary_environment'` string, and `Makefile:102-103` yields `… wrkslots/tests/test_lifecycle.py -m ordinary_environment`. `wrkslots/tests/test_lifecycle.py::` appears nowhere in the Makefile, so no stage cherry-picks node ids. The marker expressions in the Makefile are **character-identical** to the ones the inventory test collects with, so the counted population is the executed population.

### The 143 new cases have a real selected population

Derived independently from `py/wrkslots/tests/test_lifecycle.py` at head, each method carrying `@pytest.mark.ordinary_environment` and `@pytest.mark.parametrize("action", ("recover","classify"))`:

| method (line) | combos | cases |
|---|---:|---:|
| `requires_exact_current_shape` (22680) | 15 field/replacement rows (22672-22678) | 30 |
| `requires_pristine_recursive_checkout` (22700) | 9 mutations × 3 depths | 54 |
| `rechecks_after_fresh_census` (22737) | 10 mutations | 20 |
| `refuses_untrusted_or_live_evidence` (22779) | 11 mutations | 22 |
| `binds_checkout_and_gitlink_identity` (22840) | 4 mutations | 8 |
| `binds_initial_facts_to_recursive_snapshot` (22893) | 2 mutations | 4 |
| `retains_evidence_and_allows_stable_source_work` (22632) | 2 `advanced_source` | 4 |
| `retention_does_not_authorize_direct_recovery` (22820) | unparameterized | 1 |
| | | **143** |

This matches the raw receipts, not just the summary: `evidence/collections/after-ordinary.stdout.txt` contains **143** node ids naming those eight methods and `after-mapped.stdout.txt`/`after-mapped_root.stdout.txt` contain **zero**. All eight carry `ordinary_environment` and none carries `mapped_root_namespace`.

**No skip, xfail or deletion in the new block.** The only `pytest.skip` calls in the entire 807,373-byte lifecycle file are at lines 21945 and 21959, both inside the pre-existing namespace-capability tests, both in the region that is byte-identical to the 872 base. There is no `mark.skip`, `mark.skipif`, `mark.xfail`, `collect_ignore`, `addopts` or `__test__` anywhere in the file.

### Composition and identity preservation

- Raw stdout node counts, counted by me from the eight retained receipts: before 654 / 98 / 556 / 2; after 797 / 241 / 556 / 2. `98 + 556 = 654` and `241 + 556 = 797` — the partition is exhaustive and disjoint in the receipts themselves, and pytest's own trailers agree (`241/797 tests collected (556 deselected)`, `556/797 (241 deselected)`, `2/797 (795 deselected)`). All eight exited 0 with empty stderr (`e3b0c442…` is the SHA256 of the empty string).
- **All 98 prior ordinary identities survive, by method-count identity, not by total.** Splitting the 48 base method names into two alternations, before-ordinary matches 60 and 38; after-ordinary matches the same 60 and 38. 60 + 38 + 143 = 241 exactly, so the after-ordinary set is the prior 98 plus the new 143 with no substitution.
- `after-SOURCE.json` binds the after collection to lifecycle-file SHA256 `0e7d4c1c…`, which is the same hash `candidate-binding.json` and `AU-INVENTORY-COMMIT-READBACK.json` record for the blob committed at d750 (`e743489d…`, mode 100644). The counted tree is the committed tree.
- **Prefix claim spot-verified at the junction:** the 872 lifecycle file ends at line 22558; head line 22557 is byte-for-byte the same `assert "RETAINED" in held.stderr` and the appended material begins at 22559. A pure append cannot rename or remove an existing node id.
- Production is untouched: `py/wrkslots/cli.py` `384963f6…` / mode 0o755 and the lifecycle test `0e7d4c1c…` are identical to 114; the changed-path set is the single file; `tree_entry_count` 828 with all paths, types and modes otherwise identical.

### Correction to the prior report's numbers (the literal report is preserved unedited)

The prior review's "~145 new AU parameterizations" and "16 shape mutations" are both wrong, and the error is traceable: `(16 + 27 + 10 + 11 + 4 + 2 + 2) × 2 + 1 = 145`, whereas the source has **15** shape rows (lines 22672-22678), giving `71 × 2 + 1 = 143`. The supplement's 143/15 are correct; I derived both from the decorators before reading the claim. The prior coverage limit therefore reads **43 of 143 ran**, not 43 of ~145. The prior report file itself is unmodified (`b4bb147c…` as recorded).

## Goalpost-moving assessment

- **Assertions weakened: no.** The two integers moved from a stale value to the independently measured population, and equality remains equality. A larger exact count is not a weaker one. Nothing became `>=`, `<=`, `in`, or approximate.
- **Tolerance widened / exemption added / case skipped / comparator relaxed: no.** No `-k`, `--deselect`, marker exemption, skip, xfail or fallback was introduced; `assert completed.returncode == 0` still fails closed. `len(mapped) == 556`, the exact two-element `mapped_root` equality, the four membership checks and all three Makefile assertions are byte-identical to base.
- **Failure renamed or relabelled as a pass: no.** The 114 failure is retained verbatim in `evidence/failed-114-validation/output.txt:175-176` — `assert len(all_tests) == 654` / `AssertionError: assert 797 == 654`, `1 failed, 2967 passed, 2 warnings in 521.47s`, `make validate` exit 2 after 587.7301162581425 s. Lines 173-174 show the disjointness and union assertions passed before it, so the stale constant was the sole failure. The commit fixes the constant to the value the run itself measured; it does not reinterpret the failure.
- **Check deleted instead of satisfied: no.** The delta is strictly additive on the name set — it adds eight required names to a set compared with `==`, which also forbids any *other* new ordinary method. Net effect is a stricter inventory, not a looser one.

## Evidence limits

- **No full-validation receipt exists at d750 in these inputs.** The only execution evidence for the corrected bytes is the single direct control: 1 passed, 1.98 pytest s / 2.2889055791310966 outer wall s, exit 0, whose `INPUT.json` binds base 114 plus source SHA256 `281efe49…` — the exact bytes later committed at d750. It ran *before* the commit and is one test method, not a post-commit suite. Root's report of 2,968 first-partition passes at d750 is arithmetically coherent with 2,967 + the one repaired test, but no receipt is supplied and I do not count it.
- Collection executes no test bodies. These receipts prove the 143 cases are **selected**, not that they **pass**. The prior review's coverage limit (43 focused AU cases, strict mypy 13 files; parent 28 + 4 corrected = 32 distinct methods across two attempts) is unchanged and unsatisfied by this delta.
- The mapped 556-identity set equality before/after is supported by count, by pytest's own deselect arithmetic, and by aligned line positions in both raw outputs (`test_lsof_warning_must_be_proven_unrelated_to_slot` at line 355 of both mapped files, line 452 of both all files); I could not compare the two 556-line files element-by-element without Bash, so that one rests partly on the preparer's retained per-identity comparison.
- I did not re-audit the unchanged retention implementation; this delta creates no interaction with it. Nothing here asserts real-1839 eligibility, liveness, cleanup, deletion, scorecard handoff, test qualification, or activation of the AU/parent pair. The prior semantic conclusion stands: preservation only, direct recovery still requires four proof artifacts. The later parent landing at `55d1426b` is outside this review; the prior reference remains the literal `fba5` verdict.
- No mutation of any kind: no Bash, no tests, no collection, no Git, no source, ref, registry or public action.

## Verdict

**APPROVE** the exact inventory-only delta `1145835fc804f47ae48b29b89009e6937184175a..d750224cbf10c5a3b35eec40fa4d57d0e98493cb`, tree `8a41f1130199bc2dde1f60e6d053f1817375c94a`, and its composition with the already-approved AU `114` / parent `fba5` pair — bound to source review only. This approval does not assert that `make validate` has passed at d750, does not discharge the AU whole-suite or parent full-suite obligations, and does not activate or qualify anything.