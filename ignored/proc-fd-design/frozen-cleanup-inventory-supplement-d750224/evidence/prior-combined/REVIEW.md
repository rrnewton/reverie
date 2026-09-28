I have read both complete patches, every changed and new function, the unchanged callers they depend on, the committed producer and schema context, and all terminal receipts.

---

# Review target

- **Agent Utils** `8720799f78f8ef56d796723dbc473a9171a4f4eb` → **`1145835fc804f47ae48b29b89009e6937184175a`**, tree `e470a86697cbb9590c23d45ee5fab9ade12aab01` (patch SHA256 `c7b39cad…f60940`, 37,299 bytes, two files).
- **Parent dev-hermit** `0510ee72b3c94f164b62d6a0d24be912b23b490b` → **`fba5d50c96756889bc5c2d3e6caffb9adc8bcb7a`**, tree `f127f6563d771eb172980af74e398fed8b6e0edd` (patch SHA256 `2fed7382…3bd12`, 27,842 bytes, two files).

Both patches reconstruct against the copied bases and match the committed blob hashes recorded in `candidate-binding.json`. I bound findings to these exact heads independently; I did not take the author self-review or the prior parent-only review as evidence.

---

# Findings

**No blocking defect remains.** I could not construct a path through either change that deletes storage, creates a journal or removal proof, mutates a record or sidecar, synthesizes handoff/writeback, promotes the incomplete result, relabels schema 5 as historical, or admits a generic current no-result record. The items below are all nonblocking and are kept separate from the verdict.

## 1. Removal, proof-creation and mutation are unreachable from the new disposition

`_frozen_no_proof_disposition` (cli.py:17765) is the only caller of the new helper, and it is itself reachable only from `_cmd_recover_ownerless_validate_batch` (cli.py:24176) and `_cmd_classify_ownerless_validate_batch` (cli.py:24430). The new branch sits at cli.py:17822, strictly inside the arm that previously returned `BLOCKS_ENTRY`, and only *after* the unchanged `_terminal_validation_record` call at cli.py:17815 already accepted the handle. The legacy (`schema ∈ {1,2,3}` + `validation-service-result`) arm at 17802 and the historical `schema ∈ {2,3}` + `historical-validation-service-result` arm at 17830 are untouched, including the completed-writeback requirement at cli.py:17897 (`projection.get("scorecard_writeback") != {"status": "completed"}` → `Refusal`). That rule is byte-identical to base.

In `recover`, the new disposition appends `blocks_entry: False` and `continue`s at cli.py:24183-24189 — before the `LEGACY_RECOVERY` check, before `_cmd_recover`, and before `removed.append` (which only runs in the loop's `else:`). In `classify`, the same at cli.py:24437-24444, before `_ownerless_validation_journal`. Every `Refusal`/`StateError` raised anywhere inside `_current_incomplete_frozen_retention` propagates to the callers' `except (Refusal, StateError)` and becomes `blocks_entry: True`. The new code is fail-closed by construction.

Direct `recover --frozen-validate-checkout` is unchanged: `_terminal_validation_record` with `frozen_removal_proof_supplied=False` (cli.py:17138-17151) still refuses any non-`{1,2,3}` schema, and the new test asserts rc 3 with `requires --validation-proof-manifest`.

## 2. Eligibility predicate and authority binding

`_current_incomplete_frozen_result` (cli.py:17528) requires all fourteen fields via `_json_equal`, which uses `json.dumps` and therefore distinguishes `True` from `1` and `False` from `0` — the `("service_result_schema", True)` and `("executed_nodes", False)` controls are real. Presence-only refusals for `scorecard_handoff` and `scorecard_writeback_files` mean even `None`/`[]` block. `_validation_record_has_typed_admission` plus the explicit `admission.get("state") == "admitted"` closes the refused-admission case.

I verified the actual run-1839 record in `evidence/actual-1839/` matches this predicate exactly, and that the committed producer (`run_registry.py:897-921`) independently forces `qualifying_receipt is False` and forbids `materialized_target=True`/`scorecard_handoff`/`scorecard_writeback_files` on any `frozen-validate` handle. So three of the fourteen conditions are structurally implied by the producer and carry no discriminating power; the discriminating set is schema 5 + completed/no-result/COULD_NOT_RUN/75 + `executed_nodes == 0` + null tests/selection_mode/writeback + typed admitted admission. That is a *class* rule, not an identity rule for run 1839 — correctly so, and its only effect is to not block an unrelated validation while the clone is preserved.

Authority binding is intact and unweakened: `_strict_json_object` (cli.py:1128) refuses duplicate keys at every depth; `_terminal_validation_record` runs the sealed canonical parser (`_canonical_frozen_validation_record`, cli.py:16974, with `_authenticated_frozen_parser_blobs` re-checked after execution, root-owned `/usr/bin/python3`, 0o500 parser root, and a post-parse record-digest recheck) and binds checkout/`temporary_checkout`/`source_checkout`/`target` through `_frozen_validation_record_binding`; the sidecar is projected through the real `_validation_service_result_schema`/`_validation_service_result_projection` against the clone's committed producer schema, and every projected field must match the record (cli.py:17733-17735).

**Interface coherence with the unchanged consumer is exact.** AU's `_VALIDATION_SERVICE_RESULT_FIELDS[5]` (cli.py:99-111) is an order-identical list to `fields` in hermit `98d58b9`'s committed `ci/manifest-plan/validation-service-result-schema.json`, and the `outcomes`/`scorecard_writeback` blocks match `_VALIDATION_SCORECARD_WRITEBACK_SCHEMA`. `_validation_service_result_schema` compares `fields` as an ordered list, so this is a real constraint that currently holds.

## 3. Clone/source observation and the initial-window race

The two-snapshot design is sound *because* the anchors are the record's own `target` and the origin digest captured before the first recursive snapshot:

- `target` comes from the digest-bound record (cli.py:17698-17701) and is passed as `expected_head` into **both** `_pristine_frozen_validation_state` calls (17713 and 17744), which refuse at cli.py:17640-17641 if the root HEAD differs.
- `remote` is captured by `_validation_checkout_facts` at cli.py:17707 — before any `_retained_validation_git_state` call — and the *original* value is compared against both the source and the clone at the end (17748-17749). Two origins moved in lockstep therefore still refuse.

This is exactly the defect the four retained `au-initial-binding-before-fix` failures name, and the failure text shows the assertion (`row["blocks_entry"] is True`) is identical to the committed test. **The fix was in production code; the test was not weakened.**

Observation depth is real: root and every initialized submodule get `repository_root` identity, `operation_paths`, `assert_ordinary_history`, `assert_ordinary_index` (assume-unchanged and skip-worktree), absolute `--git-dir`/`--git-path index`, an inode+SHA256 index identity, and a status run with `core.fsmonitor=false`, `core.untrackedCache=false`, `GIT_OPTIONAL_LOCKS=0`, `--untracked-files=all --ignored=matching --ignore-submodules=none`. Declared vs. initialized submodules must agree; every committed gitlink must match the child HEAD; uninitialized gitlinks must be empty directories; submodule Git storage must live inside the clone (`_path_is_within(state.common, checkout)`). Directory identities are `(st_dev, st_ino, mnt_id)`, which is what catches the `replaced-clone` rename+copytree control.

Notably, the clone is observed with **no** cache-glob exemption (`cache_globs` defaults to `()`), so an ignored artifact in the clone refuses — *stricter* than `_ownerless_validation_blocking_status`, which exempts `ignored` and the cache globs. The source is observed with `pristine=False` and cache-glob exclusions, which is the documented and intended asymmetry; its HEAD, index, history, ordinariness and origin are still bound. That asymmetry is safe here because the outcome is retention, not removal.

## 4. Atomicity, budget and census

I did not find a double-read used as if it were atomicity. Concretely: `_read_regular_file_identity` (cli.py:1063) validates `st_dev/st_ino/st_mode/st_size/st_mtime_ns/st_ctime_ns` across the read and against a fresh `lstat`, so `sidecar-replace` (same bytes, new inode) refuses. The single fresh same-UID census (`assert_unused(..., fresh_same_uid=True)`, cli.py:17736 → 22871-22884) is followed by re-reads of all three evidence files, both recursive Git snapshots, both origin digests, a second canonical-parser pass with `expected_digest`/`expected_target`, a second liveness check, and a registry-row comparison (17758-17761). The `rechecks_after_fresh_census` test proves each of ten mutation classes injected *during* that census blocks, with `calls == 1`.

Residual windows that remain (and are inherent, not introduced): nothing is atomic across the whole classification, so the guarantee is "no observed change," not "no change." The outcome of any detected change is retention plus blocked entry. `unknown-census` (a `Refusal` from `_capture_lsof_process_path_census`) correctly blocks rather than becoming nonblocking.

## 5. Parent: state root, record threading, acceptance paths

`state_root` is now an explicit keyword on `prepare_fresh_checkout` (start_unit.py:1247, 1281) and `remove_fresh_checkout` (1533, 1541), replacing `parent.parent.parent` and `source.parent`. This is a genuine bug fix, not cosmetics: the real run-1839 record's `source_checkout` is `worktrees/slots/kvm-parity-recovery-20260916`, so the base computed `frozen_checkout_parent(worktrees/slots)`, never matched the frozen branch, and fell to the `else` refusal — the frozen cleanup route was unreachable for any worker-launched run. `main()` takes `state_root` from `(args.state_root or tool_root).resolve()` (6246), so the canonical root is explicit, not inferred from a worker or tooling slot.

`cleanup_record_path` (6437, set at 6712) is assigned only after `run_registry.create_current_record` succeeds; `create_record` (run_registry.py:1218) refuses under the exclusive lock if the path exists. Before that point both cleanup closures pass `None`, and the frozen branch raises `frozen checkout cleanup requires its completed validation record` → `return False` → storage preserved.

I checked that the reworded comment at 6804-6806 is an honest correction rather than a behavioral loosening: under the base, the pre-acceptance frozen path *already* retained, in both layouts (worker-slot source → `else` refusal; classic source → `completed_record is None` refusal). The base comment ("the temp checkout holds no evidence and is removed") was false. Post-record, the provider is now actually asked — and for a current-schema record it still answers rc 3, so nothing new is deleted.

Preservation on every named path: missing record → `RuntimeError` → `False`; wrong root → `outside the managed or frozen validation roots`; outside project root → `ValueError` → `RuntimeError`; provider nonzero → 1639-1648 `False`; surviving path after provider zero → 1649-1656 `False`; archive failure → `cleanup_fresh_checkout_preserving_receipts` returns before `remove_fresh_checkout` (3941-3944); post-accept → 6786-6803 returns `could_not_determine` without touching storage.

Report acceptance is typed and fail-closed: `state_blocks_entry["current-incomplete-retained"] = False` is added **only when `frozen`** (2008-2009); `type(blocks_entry) is not bool` rejects `0`/`1`; `blocks_entry is not state_blocks_entry[state]` requires the exact pair; unknown `state` raises; row key set must be exactly `{blocks_entry, checkout, reason, state}`; duplicate/unrequested checkouts raise; coverage must be total; and any nonblocking row requires `process_censuses == 1` (2034-2039). Every `ValueError` converts all outcomes to `blocks_entry True` with `invalid ownerless batch report` (2102-2107). **No reason-text inference anywhere** — the existing `test_entry_cleanup_does_not_infer_nonblocking_from_reason_text` passes unchanged.

I also cross-checked the recover-path arithmetic the AU change must satisfy: `nonblocking_count and not frozen` → reject; `counts[2] < removed_count + nonblocking_count` → AU's `same_uid_census_count` increments once per retained current-incomplete item; `counts[2] > 0 and counts[1] != 1` → AU sets `shared_process_censuses = 1`. Coherent in both directions.

## 6. Tests

Both AU entrypoints are exercised as real CLI invocations via `wrkslots.main([...])` with `--format json`, under both `recover` and `classify`. The AU test diff is a **single append-only hunk**; zero existing AU tests or fixtures were modified. Negative controls are dense and meaningful: 16 shape mutations, 27 pristine mutations across three repository depths including a `submodule.component.ignore=all` attempt to hide a nested violation, 10 post-census mutations, 11 untrusted/live mutations (including a duplicate-JSON-key sidecar and a live `process_identity` taken from the running PID), 4 gitlink/substitution mutations, 4 initial-window mutations, and the direct-recovery refusal.

On the parent side, the only modifications to existing tests are: a `FakeRun.write_frozen_result = True` default that preserves prior behavior; mandatory `state_root=` arguments; three rows added to a mutation tuple; and `test_immediate_frozen_cleanup_uses_typed_wrkslots_recovery`, which was **strengthened** by four new argv assertions and a worker-slot source. Nothing was removed or loosened.

I diffed the two retained parent attempts directly. `start_unit.py` is SHA256 `4db675e7…f3be73` in **both** attempts and equals the committed blob — production bytes are provably identical. The test file grew 416,997 → 417,442 bytes, and reading both copies side by side, the four changes are: `"schema_version": 1` added to three `write_record` fixtures, `"state"/"result"` added to one, and the tool-root schema file copied in the fifth. **Every assertion is textually identical between attempts.** The claim "no assertion was relaxed" holds.

---

# Goalpost-moving assessment

- **Assertions weakened:** no. AU tests are append-only. The four corrected parent methods carry byte-identical assertions across both attempts, with production bytes identical. `test_immediate_frozen_cleanup_uses_typed_wrkslots_recovery` gained four assertions.
- **Tolerance widened / exemption added / case skipped / comparator relaxed:** no. The one behavioral widening is the intended new classification, and it is gated by strictly more checks than the `BLOCKS_ENTRY` arm it carves out of. The clone is held to a *stricter* cleanliness standard than the existing ownerless status filter (no `ignored`/cache-glob exemption). The source's relaxed cleanliness is the explicitly authorized retention-vs-deletion distinction, and the source's root/history/index/origin bindings are re-imposed in `_retained_validation_git_state`.
- **Failure renamed or relabelled as a pass:** no. `result` stays `no-result`, `state` stays `completed`, `qualifying_receipt` stays `false`, `blocks_entry: False` is not a verdict, and the parent maps state→Boolean without reading reason text. The record and sidecar are never written.
- **Check deleted instead of satisfied:** no. The historical completed-writeback rule, the legacy-recovery arm, the direct-recovery proof requirement, and every parent preservation path survive unmodified. The four pre-fix failures were fixed in production, and the failure output is retained rather than discarded.

---

# Nonblocking follow-ups (not part of the verdict)

1. **The 22-second census budget may make this a no-op on the real clone.** `_VALIDATE_REMOVE_BATCH_CENSUS_SECONDS = 22.0` (cli.py:341) is operation-wide for the whole batch, and `remaining_seconds()` (cli.py:21906) raises once it is spent. The retention path adds, per checkout, three full-tree `git status --untracked-files=all --ignored=matching` walks (one in `_validation_checkout_facts`, two recursive snapshots) plus a per-repository index read and SHA256, each done twice. The fixture repositories are a few files; the real run-1839 clone is a hermit tree with initialized submodules. Nothing in the evidence measures wall time on a hermit-sized clone, and `_GitVcs._run` itself has no timeout. The failure mode is fail-closed and diagnosable (`read-only census exceeded its operation-wide time bound` → `blocks_entry: True`), so it is not a safety issue — but it may mean the change does not actually unblock entry in production. Measure this at activation time.
2. **Index size bound.** `_read_regular_file_identity(index, …, 16 MiB)` (cli.py:17591) is a hard refusal that scales with file count and is read twice per repository. Diagnosable, fail-closed, worth knowing.
3. **Three of the eight rows added to `test_ownerless_classification_requires_state_disposition_pair_and_census` (test_start_unit.py:5919-5921) are vacuous.** That test calls `remove_ownerless_checkouts_batch` without `frozen=True`, so all three are rejected by the unknown-state rule, not by the Boolean-pair or census rule the first two appear to probe. The real frozen-request coverage exists in the new `test_entry_cleanup_preserves_current_incomplete_frozen_result` subTests, so nothing is uncovered — only the labelling overstates what those three rows discriminate.
4. **Source Git-storage containment is checked by equality only.** `vcs.common_directory(repository) == vcs.common_directory(checkout)` (cli.py:17717) omits the historical path's `_path_is_within(observer_common, checkout)`. I could not build an exploit: a linked worktree of the clone is caught by the equality, and an alternates link cannot hide clone content. Retention-only outcome. Noted for completeness, not for action.

---

# Source-versus-runtime evidence limits

- **Process censuses are fully stubbed.** `stub_validate_batch_censuses` (test_lifecycle.py:1548) replaces `_capture_process_path_census`, `_capture_lsof_process_path_census` and `_capture_same_uid_process_path_census`. These tests prove control flow around liveness, not host liveness. **They are not a host-1839 liveness proof and not a removal proof.**
- **The sealed canonical parser is stubbed.** `stub_canonical_frozen_record_parser` (test_lifecycle.py:2060) bypasses `_authenticated_frozen_parser_blobs`, the root-owned interpreter, the bounded command, and the post-hoc blob recheck, and projects from the same bytes — making the projection-agreement check trivially true in tests. The fixture also uses `repo: "consumer-a/project"`, which the real producer would reject for a schema-5 marker. The sealed chain is unmodified by this patch, but **no test in this package exercises it against the new shape.**
- **Real Git operations are real.** Clones, submodules, `update-index`, `MERGE_HEAD`, `info/grafts`, `info/exclude`, origin rewrites and the rename+copytree substitution are genuine filesystem/Git work, so the pristine, gitlink, index-state and substitution controls are not stubbed.
- **Only 43 of ~145 new AU parameterizations ran at this exact head** (43 passed, 754 deselected of 797 collected; 84.77 s pytest, 85.10 s wall, 34.12 user + 47.16 system CPU, exit 0). The unrerun cases are negative controls asserting `blocks_entry True`, and the binding correction only *adds* refusal conditions, so a regression to a false accept is implausible — but it is unproven at this head. The earlier 165-pass belongs to the pre-correction source. Strict mypy: 13 files, exit 0, 2.32 s.
- **No whole-suite result.** Normal AU `make validate` was running separately and is explicitly not claimed; parent has no full 269-method run. 32 distinct parent methods have passing observations across two retained attempts (28 + 4), not one fresh 32-method pass.
- **Deployment.** Parent `fba5` still records hermit `98d58b9bd6ea722c6d7087d45d5fd81792abfa16`, which records AU `2781b1054efc3a9c561dbed35584a6fca1ed8676`. This pair is **not activated**. Concrete activation constraint: the parent's `current-incomplete-retained` acceptance (start_unit.py:2008) is inert until a deployed AU can emit it, and an AU emitting it against a parent at `0510ee72` would be rejected as an unknown state and would block entry — so the AU side must not be activated ahead of the parent side. Both pins must be bound through the normal deployment path; nothing here bypasses it.
- I performed no source, ref, registry, record, service, public or filesystem mutation, ran no Bash, builds, tests, guest runs or cleanup, and classified/admitted/deleted no actual retained run.

---

# Verdict

- **Agent Utils `1145835fc804f47ae48b29b89009e6937184175a` (tree `e470a86697cbb9590c23d45ee5fab9ade12aab01`): APPROVE.**
- **Parent dev-hermit `fba5d50c96756889bc5c2d3e6caffb9adc8bcb7a` (tree `f127f6563d771eb172980af74e398fed8b6e0edd`): APPROVE.**
- **Combined interface: APPROVE**, bound to source review only. The producer-schema field list, the report schema and field sets, the census-cardinality arithmetic, and the typed state↔Boolean pair are mutually consistent at these two heads. Approval does **not** assert that `fba5` activates AU `114`, does not clear the AU `make validate` obligation or the parent full-suite obligation, does not constitute a live-host or removal proof for run 1839, and does not clear the CLI test obligation or guest/parity evidence for https://github.com/rrnewton/hermit/pull/3047.