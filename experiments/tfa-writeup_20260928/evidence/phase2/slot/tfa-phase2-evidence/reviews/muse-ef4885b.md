# Adversarial review — TFA phase-two Codex provenance seal

Review target: Sapling `fbsource` `/home/newton/work/tfa/fbsource` · Base `435d79639fc043c472d31a87e5a4a4b6f658ed32` · Exact reviewed head `ef4885b60dc26a1a73321b83d834c97179e0ec64` (single commit `[testinfra][tfa] Seal Codex incompatibility provenance`, parent = base; `sl id` = head, `sl status` clean before/after; Sapling hashes are content ids — no GPG field applicable). Read-only: no source edit, no publish, no tests run by reviewer.

Intended behaviour: seal exact signed Codex executable/package/config/environment provenance; fail closed on unsupported signed builds; preserve containment/cleanup/recovery; document signed 0.156.0/0.156.1 blocked vs unsigned diagnostic candidate (fixes `7cec3e110ce6…`, `a708fc683934…`) not production-eligible.

## Findings (first)

No blocking finding in the commit. Two minor residuals:

- **MINOR F1 — launch exec-by-path micro-window (mitigated, owner-only).** `fbcode/testinfra/tfa/process.py` `_open_verified_codex_launcher` fstat-pins dev/ino/size/mode on an `O_NOFOLLOW` fd, but `TrustedSubprocessWithList.Popen(executable=launch_executable=path)` re-resolves the path; `_verify_launched_codex` (`/proc/<pid>/exe` dev/ino) + kill-on-mismatch in `_spawn_contained` (except-path kills `process` before `runtime.close()`) bounds it to a brief start of a wrong binary. Reachable only by root replacing a root-owned non-group/world-writable file mid-launch (owner trust domain). Required: none for approval; `/proc/self/fd/<fd>` exec would close it fully — follow-up.
- **MINOR F2 — evidence labelling uneven (docs cure it).** v6/v7 receipts lack the explicit `authority` string that v8/v8b carry (`diagnostic-only; unsigned candidate; not production evidence`); they do carry `production_compatibility_map_restored` and the blocker doc + README label all diagnostic evidence candidate-only. Required: none; harmonize receipt schema on next diagnostic run.

### Confirmed (implementation meets intent)

- **Sealed identity (4-tuple, fail closed).** `process.py:61-107` `_CODEX_RELEASE_MANIFESTS{stable,latest}` (manifest path+sha256), schema `v1→v2`, `_CODEX_COMPATIBILITY[(version, launcher_sha, manifest_sha, channel)]` — both signed keys `blocked` with `_CODEX_SOCKET_ISOLATION_BLOCKER` naming both upstream hashes; map contains **zero** `eligible` entries, default `unsupported`. `_codex_environment_preflight:410-597`: entrypoint (`lstat` symlink, root uid, target+resolved) and root-owned single-link non-writable records (`_root_owned_regular_record`: uid 0, nlink 1, mode `&022==0`, sha256) before/after `--version` (run via resolved path), release identity before/after; any drift → `identity-changed` + raise; `state != eligible or blocker` → raise (`:591-594`). Version stderr closed tuple extended with exact latest-channel strings only.
- **Post-preflight / spawn binding.** `run_process` snapshots environment once (`:1121-1135`), verifies preflight record file, passes `codex_identity`; `_validated_codex_launch_record` re-derives entrypoint/channel/manifest/resolved and refuses drift (incl. channel); `_spawn_contained` opens verified launcher, appends fd to `_PinnedRuntime.descriptors` (tuple generalized, closed on all paths), verifies launched `/proc/exe`, kills on failure. Channel env `META_CODEX_RELEASE` unknown value → `release channel is not reviewed` (`:397-402`); it is the only provenance-relevant env read — no compatibility/config override exists in production code.
- **Containment/cleanup preserved + probe strengthened.** Readiness barrier resets `payload_deadline = monotonic()+timeout` after `_wait_for_readiness` (startup no longer consumes execution budget; non-readiness path keeps `started+timeout`) — documented intent, execution timeout unchanged. `tests/test_process_migration_helper.py`: polling/sleep replaced by stdin pipe barrier (descendant asserts descendant-cgroup before signalling), stdout `ready` line, and lifetime `flock` — `survived` now determined by lock acquisition after cleanup, race-free; `valid` logic unchanged (`not (… or survived)`).
- **Tests strengthened, exact hashes updated truthfully.** Launcher sha `8ec5b757…→71b8f5bd…` matches installed identity in blocker doc/gates (base key was stale: it would have misclassified current payload `unsupported` instead of precise `blocked`); new tests: latest blocked classification, manifest race, entrypoint retarget, post-preflight channel drift, unreviewed channel, plus real-host latest preflight in `test_production_containment.py` asserting channel/version/launcher+manifest sha. Mock retargets (`regular_file_record→_root_owned_regular_record`) follow the production rename.

### Evidence (inspected under `ignored/tfa-phase2-evidence/`)

- **Production-signed (only production claim):** `gates-tfa-checkout/production.*`: 36/40, four real-Codex cases fail with exact `v2; codex-cli 0.156.0; state=blocked` + upstream hashes — fail-closed is the production result, honestly reported (commit Test Plan `36/40`; `132/132` phase2 unit; manifest validate 3 cases).
- **Signed baseline:** blocker doc census — every signed payload checked, none contains `7cec` marker; device mismatch mechanism (`mountinfo 0:42 vs st_dev 0:49` on Btrfs `/tmpvol`, socket dir hard-coded) consistent with signed `rc=1` vs candidate `rc=0` sandbox discriminator.
- **Unsigned diagnostic (never production):** candidate `f9228bec…` 0.156.1+both fixes. v6 four-case `4/4 pass` (candidate in signed-identical package layout); v7 40-case `25/40` correctly treated **invalid/non-authoritative** (missing Buck `hermit-test-bin`: 14 failures fixture-caused + 1 inverted gate test); v8 Buck-fixture `38/39` sole failure `test_killed_worktree_body…` traced to missing child `PYTHONPATH` (environment, non-Codex), v8b rerun of that single case `1/1 pass rc0` → valid population 39/39 (40 minus production-refusal test, exclusion explicitly reasoned in runner: override inverts its assertion).
- **No override escape:** diagnostic runners live only under `ignored/…/diagnostic/` (not in `fbcode`); they force `eligible` in-process for existing keys only, substitute candidate via sealed memfd wrapper at `_pinned_runtime` for Codex spawns only, annotate identity records `production_eligible=False/eligibility_scope=unsigned-local-diagnostic-only`, restore map in `finally` and hard-fail if `_CODEX_COMPATIBILITY != original` (receipt `production_compatibility_map_restored: true`). Candidate absent from `ALLOWED_EXECUTABLES`, `_CODEX_COMPATIBILITY`, manifests. Production binary/config cannot import or be reached by this code.

## Goalpost-moving (four explicit answers, base→head)

- Assertions weakened: **No** — new refusals added; helper `valid` semantics identical; sha updates reflect measured installed payload with evidence.
- Tolerance/exemption/skip/comparator relaxed: **No** — compatibility key widened (2→4 fields, stricter); deadline change excludes startup from execution budget by design (barrier explicit, budget value unchanged); diagnostic 39-population exclusion is diagnostic-only, reasoned, and the excluded test is the production gate itself.
- Renamed/relabelled as pass: **No** — production suite stays red `36/40` and is reported red; diagnostic passes are labelled diagnostic-only at receipt/runner/blocker-doc levels (F2 unevenness noted).
- Check deleted vs satisfied: **No** — stale-key imprecision fixed by sealing exact current identities; blocked state satisfied by real-host preflight tests, not by reclassification (both entries remain `blocked`, none `eligible`).

## Verification limits

Reviewer ran no `buck`/tests (read-only scope); pass/fail counts are from commit Test Plan + gate logs at exact head (production log traces show `process.py` from Buck link-tree at this commit). Candidate fix sufficiency rests on evidence census/patches/sandbox discriminator, not reviewer rebuild (host binary unbuildable here per blocker doc: `rusty_v8` proxy 403 — owner step). Hashes in code cross-checked against blocker doc + gate output text, not independently re-hashed from `/usr/local/bin` (host state could have drifted since gates).

## Residual risks

F1 micro-window; future signed fixed build stays `unsupported` until owner seals an `eligible` key (intended, step 4 in blocker doc); host Btrfs alias topology is host-specific (blocker text scoped to "this host").

## Verdict

**Approve** commit `ef4885b60dc26a1a73321b83d834c97179e0ec64`. Production Codex remains blocked — that is the sealed, accurate state pending owner-signed remediation, not a defect of this change.
