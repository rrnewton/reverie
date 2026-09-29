# TFA phase-two stack: current state (gatherer "stackstate", devbig030, 2026-09-28 ~14:38-14:55 PDT)

Read-only. No Phabricator writes, no `sl pull`, no Hermit/buck runs. Every raw command output cited below is kept under
`/tmp/sub-tfa2-030/gather/stackstate-raw/` (abbreviated `RAW/` below). "Inference" marks anything the records do not prove.

## 0. Headline

1. **The stack is 22 unpublished drafts, all authored under `newton`, linear, rooted on public master `4c52f205ae7a` (2026-09-22 02:55 PDT).** No reviewers, no subscribers, no human comments on any of them. Only bots have commented or acted (48 bot comments; 5 AI-review transactions).
2. **Two lower diffs are RED, which the previous report did not mention.** D121244213 has 28 FAILED signals and D121848886 has 32 FAILED. The other 20 diffs are "GREEN with warnings" (`check_ci_state` verdict). The failures are hard failures because the tests were *added in that revision*, so base-rev reproduction could not reclassify them as pre-existing.
3. **Top two diffs: the previous agent's CI figures are confirmed.** D121978134 has 506/538 passed, 30 warning, 0 failed, 2 info. D121978137 has 528/559 passed, 30 warning, 0 failed, 1 info. The 30 warnings are 15 distinct `tfa_production_containment_test` tests x {mode/dev, mode/opt}. All 30 are `LAND_BLOCKING` and all "failed on base-rev".
4. **One correction to the reason breakdown.** It is 9 + 1 + 5, not 10 + 5:
   - 9 tests: `sudo -n mount -o loop` denied for Sandcastle user `twsvcscm` (Btrfs fixture).
   - 1 test: `test_killed_worktree_body_is_reconciled_by_independent_owner` fails because systemd unit `chataccd-mount@<hash>.service` is not found. No sudo is involved.
   - 5 tests: `real_codex_*`, "Codex stable|latest release manifest identity differs".
   All 10 non-Codex failures are CI-only (they passed on devvm895).
5. **One correction to "same failure on the parent".** For D121978137 (dev and opt) and D121978134 opt, the base-rev failure class matches for all 15 tests. For **D121978134 mode/dev**, the 5 `real_codex_*` base-rev failures have a *different* cause: "version command could not start", not "manifest identity differs". The similarity scores there are 75.1-85.8%, not the "98-99.9%" stated earlier.
6. **`fbcode/testinfra/tfa` is not on fbsource master.** Local `remote/master` = `9e6ec042287d` (2026-09-28 11:14 PDT) has no such path, and BigGrep finds no TFA symbols anywhere in fbsource.
7. **Phase question.** On devbig030 `/home/newton/fbsource`, all 22 stack commits are phase **draft**, including the local tip `317d325e8eb0`. That local tip is a message-only amend of Phabricator's recorded commit `a729ddf3707f`: the trees are identical (empty `sl diff`). The earlier "secret" observation applied to devvm895's separate repository and cannot be checked from here.
8. **devvm895 local validation.** "containment 37 pass / 6 fail" is confirmed by the retained log. "Build rc 0" and "tfa_phase2_test 215/215" have **no retained log** on devbig030; they are the previous agent's narrative only. CI does independently show 215 `tfa_phase2_test` cases passing on D121978137 in both modes. `results-dev.json` is a **CI** result (Sandcastle), not local validation.

## 1. Per-diff metadata

Commands (run for all 22 D numbers in `RAW/diffs.txt`, in parallel, all rc 0):
`meta phabricator.diff {describe,files,reviewers,comments,history,versions,agent-activity} --number D<N> -o json` → `RAW/<sub>-D<N>.json`;
CI counts: `/home/newton/.llms/skills/claude-templates/ci-signals/scripts/check_ci_state D<N>` → `RAW/cistate-D<N>.json` (every `version_id` matches `describe.latest_version_id`, so the counts are for the latest version).
Local commit and phase: `sl log -r '6b3a2263c23c::317d325e8eb0' -T '{node|short} p1={p1node|short} {phase} {phabdiff}\n'` in `/home/newton/fbsource` → `RAW/sl-stack-chain.txt`. The output is 22 lines, a linear chain.

| # | Diff | Title (Phabricator, first line) | Status | Author | Created (EDT) | Latest version created (EDT), versions | Files | +added / -removed | Reviewers | Human comments | Bot comments | CI (passed/warn/failed/info of total) | Local commit (devbig030 /home/newton/fbsource), phase | who-am-i tag in title/summary |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | D121244476 | [testinfra][tfa] Add bounded phase-two prototype | Unpublished | newton | 2026-09-22T14:53 | 2026-09-22T14:53, 1 | 19 | +5221 / -0 | 0 | 0 | ai_diff_reviewer 2 | 156/2/0/2 of 160 | 6b3a2263c23c, draft | none |
| 2 | D121244213 | [testinfra][tfa] Use native Codex sandbox with TFA lifecycle control | Unpublished | newton | 2026-09-22T14:52 | 2026-09-25T18:23, 6 | 24 | +18935 / -1203 | 0 | 0 | lint_root 32 | 307/0/28/5 of 340 | e10b1b4a78ad, draft | none |
| 3 | D121649569 | [testinfra][tfa] Close controller follow-up contracts | Unpublished | newton | 2026-09-24T14:45 | 2026-09-25T18:23, 4 | 2 | +1108 / -49 | 0 | 0 | 0 | 315/28/0/1 of 344 | 28c3c40ef45e, draft | unknown-agentname / codex-family-by-ancestry / devbig030 |
| 4 | D121848885 | [testinfra][tfa] Make controller dependencies self-contained | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 8 | +730 / -6 | 0 | 0 | ai_diff_reviewer 1 | 333/28/0/2 of 363 | 11f0a52263f9, draft | unknown-agentname / codex-family-by-ancestry / devbig030 |
| 5 | D121848886 | [testinfra][tfa] Close phase-two review findings | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 12 | +1378 / -191 | 0 | 0 | 0 | 344/0/32/2 of 378 | 77ca18ca1f68, draft | unknown-agentname / gpt-5.6-sol / devbig030 |
| 6 | D121848888 | [testinfra][tfa] Seal Codex incompatibility provenance | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 5 | +567 / -59 | 0 | 0 | ai_diff_reviewer 1 | 361/34/0/4 of 399 | ec6692f4acc0, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 7 | D121848889 | [testinfra][tfa] Hash-bind the Codex payload and unify deadlines | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 6 | +1036 / -92 | 0 | 0 | ai_diff_reviewer 1 | 414/32/0/4 of 450 | 77c712d2310b, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 8 | D121848890 | [testinfra][tfa] Refuse empty wrapper argv; byte-bind Hermit test | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 3 | +127 / -21 | 0 | 0 | 0 | 421/30/0/3 of 454 | 8d5c7d0ff39a, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 9 | D121848892 | [testinfra][tfa] Render deterministic difference hints and unposted initial comments | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 5 | +431 / -4 | 0 | 0 | 0 | 436/30/0/2 of 468 | 628132447c63, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 10 | D121848893 | [testinfra][tfa] Lead unposted initial comments with measured non-/tmp reproducers | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 5 | +624 / -23 | 0 | 0 | ai_diff_reviewer 1 | 450/31/0/3 of 484 | 289f8f03fee6, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 11 | D121848894 | [testinfra][tfa] Bind censored relocation attempts and resolve relocated paths | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 4 | +222 / -28 | 0 | 0 | ai_diff_reviewer 1 | 455/32/0/4 of 491 | 83d0cdc6b49d, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 12 | D121848895 | [testinfra][tfa] Bind relocated safehermit reports to unique keys and the manifest Hermit | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 3 | +44 / -5 | 0 | 0 | 0 | 455/28/0/3 of 486 | b5ddb5ce9027, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 13 | D121848896 | [testinfra][tfa] Render unposted candidate-fix follow-ups from re-judged witness replays | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 5 | +658 / -8 | 0 | 0 | ai_diff_reviewer 2 | 463/31/0/2 of 496 | 235febad0e55, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 14 | D121848898 | [testinfra][tfa] Bind the follow-up unfixed phase to the manifest test ELF | Unpublished | newton | 2026-09-25T15:13 | 2026-09-25T18:23, 2 | 3 | +21 / -6 | 0 | 0 | ai_diff_reviewer 1 | 476/31/0/3 of 510 | d30f4854c3dc, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 15 | D121848525 | [testinfra][tfa] Render follow-up root causes as caveated hypotheses | Unpublished | newton | 2026-09-25T15:11 | 2026-09-25T18:23, 3 | 4 | +356 / -7 | 0 | 0 | ai_diff_reviewer 1 | 490/31/0/3 of 524 | b996d9329e1c, draft | tfa-phase2-cyber-opus / claude-opus-5-5 / devbig030 |
| 16 | D121877477 | [testinfra][tfa] Hash-bind every follow-up artifact in the ledger | Unpublished | newton | 2026-09-25T18:23 | 2026-09-25T18:23, 1 | 6 | +472 / -96 | 0 | 0 | ai_diff_reviewer 1 | 488/31/0/2 of 521 | 6f234641d0f9, draft | none |
| 17 | D121877478 | [testinfra][tfa] Bind follow-up replay roles to manifest seeds | Unpublished | newton | 2026-09-25T18:23 | 2026-09-25T18:23, 1 | 3 | +155 / -13 | 0 | 0 | ai_diff_reviewer 2 | 493/31/0/2 of 526 | 5e5465339241, draft | none |
| 18 | D121877479 | [testinfra][tfa] Keep task comments dry-run-only | Unpublished | newton | 2026-09-25T18:23 | 2026-09-25T18:23, 1 | 5 | +139 / -11 | 0 | 0 | 0 | 501/30/0/1 of 532 | d0ccdc2e23d9, draft | none |
| 19 | D121877480 | [testinfra][tfa] Present the final comment's cause as an unproven hypothesis | Unpublished | newton | 2026-09-25T18:23 | 2026-09-25T18:23, 1 | 4 | +68 / -4 | 0 | 0 | 0 | 502/30/0/1 of 533 | 0e10fe33365c, draft | none |
| 20 | D121877481 | [testinfra][tfa] Exercise the eligible Codex launch with a hash-bound fixture | Unpublished | newton | 2026-09-25T18:23 | 2026-09-25T18:23, 1 | 4 | +576 / -12 | 0 | 0 | ai_diff_reviewer 2 | 506/33/0/1 of 540 | 7deace132822, draft | none |
| 21 | D121978134 | [testinfra][tfa] Pin the rolled-over owner-managed Codex releases as blocked | Unpublished | newton | 2026-09-26T12:20 | 2026-09-26T12:21, 1 | 4 | +24 / -23 | 0 | 0 | 0 | 506/30/0/2 of 538 | 266a65acae24, draft | none |
| 22 | D121978137 | [testinfra][tfa] Add a separately labelled external OSS Hermit prototype mode for witnesses | Unpublished | newton | 2026-09-26T12:20 | 2026-09-26T12:21, 1 | 4 | +1657 / -6 | 0 | 0 | 0 | 528/30/0/1 of 559 | 317d325e8eb0 (Phabricator commit_hash a729ddf3707f), draft | none |

Notes on the table:
- **Status**: `describe.status` = `Unpublished` for all 22. `is_landable=False`, `land_blocker=latest_phabricator_version_is_draft`, `land_job_status=NO_LAND_RUNNING`. `reviewers`, `subscribers`, `tags` and `tasks` are empty strings on all 22 (`RAW/describe-*.json`). Every diff's `depends_on` is its predecessor in the table; D121244476 has none.
- **Created/updated**: `describe.created`. `describe.updated` does NOT track later versions (D121244213 says `updated 2026-09-22T17:19:51` but its V0.6 was created 2026-09-25T18:23:59, per `RAW/versions-D121244213.json`), so the table uses the latest version's `created` instead. All versions are `NORMAL_DRAFT` and authored by `newton`. Version totals: 43 across 22 diffs; D121244213 has the most (6).
- **Size**: sum of `files[].lines_added/lines_removed`, **+34,549 / -1,867** in total (net **+32,682**, which is the previous report's "+32.7k"). There are 33 distinct paths, all under `fbcode/testinfra/tfa/`, and each is first introduced with status `A`. No path lies outside that directory (computed from `RAW/files-*.json`). `describe.line_count` = added+removed per diff (e.g., D121244213: 18,935+1,203 = 20,138).
- **Comments**: `describe.comment_count` = 0 on all 22, so there are no human comments. `meta phabricator.diff comments` returns **48 automated rows**: 32 `lint_root` ShellCheck advice on `fbcode/testinfra/tfa/resources/safehermit` in D121244213 (28 x SC2317 "command appears to be unreachable", 4 x SC2086), plus 16 `ai_diff_reviewer` ("General/Context Reviewer") warnings or advice across 12 diffs. Examples: D121244476 `pipeline.py` `_current_revision` indexes `splitlines()[-1]` without checking for empty output; D121877478 `followup.py` uses `set(...)`, which discards replay multiplicity; D121877481 asserts the global production state instead of restoring the fixture. All are `is_unresolved=True` (`RAW/comments-*.json`).
- **Title/commit-message drift**: the local commit message of D121244213 reads "[testinfra][tfa] Bind fatal witnesses and SafeHermit evidence", while the Phabricator title is "Use native Codex sandbox with TFA lifecycle control" (`RAW/sl-nonpublic-fbsource.txt` line 208 vs `RAW/describe-D121244213.json`). The Phabricator `title` field of D121848885 is multi-line: it contains the who-am-i tag `[hermit2, unknown-agentname, codex-family-by-ancestry, devbig030, role=impl]` and the start of "Context:". So the summary leaked into the title.
- **Stack-root freshness**: D121244476 carries a `LAND_BLOCKING` "Old Diff" warning: "Diff Is Old or Base Revision Is Old ... try rebasing first" (`meta phabricator.diff.signals list --number D121244476 --status=warning --columns=name,status,type,error,land_friction -o json` → `RAW/warn-D121244476.json`). Its other warning is a non-blocking General Reviewer insight.

## 2. CI signals

### 2a. Counts (whole stack, one call)
`meta phabricator.diff stack-ci --number=D121978137 --output=json` → `RAW/stack-ci-D121978137.json` (rc 0). `ci_overall` is `WARNING` for 20 diffs and `FAILED` for D121244213 and D121848886. Warning counts range from 28 to 34 per diff (table above). `check_ci_state` agrees: `TEST_FINISHED_WITH_FAILURES`/`RED` for those two, and `TEST_FINISHED_WITH_WARNINGS`/`GREEN` for the rest. Pending = 0 everywhere.

### 2b. Top two diffs: 30 warnings each
Command: `meta phabricator.diff.signals list --number D1219781{34,37} --status=warning --details --limit=500 -o json` → `RAW/warn-D121978134.json`, `RAW/warn-D121978137.json` (rc 0).
- 30 signals each. All are type `Test`, target `fbcode//testinfra/tfa/tests:tfa_production_containment_test`, 15 distinct tests x {dev, opt}, and `land_friction=LAND_BLOCKING`, `land_friction_reason=DEFAULT`, `signal_class` empty (as the skill documents for Warning rows).
- Every one says "Test failed on base-rev". For D121978134 the base rev is `08bc9982ae757e0458ac9562b96d1774bd37799e`, and for D121978137 it is `8969df1b77665c8ecf24e99cc446f2524f54516a`. Neither hash appears in the local non-public commit list. I did not resolve them with `sl`, to avoid an implicit autopull. Inference: they are the stack parent as rebased by CI.
- Similarity: D121978137 ranges 98.48-99.95% (n=30). D121978134 ranges **75.14-99.92%**; its five `real_codex_*` mode/dev rows are 75.1, 80.9, 81.2, 85.8 and 85.8%.

To get failure text, I read the per-run TestX results. The run IDs come from the "Original failure" / "Base rev failure" diagnostics links: `testx --caller ci-signals --as-json results list <run>` → `RAW/testx-<run>.json`, plus the already-fetched `/tmp/sub-tfa2-030/devvm895/tfa-diag/results-dev.json` (= run 1125900439774673).

| Run | Diff / mode / role | Sandcastle job | Duration | Summary |
|---|---|---|---|---|
| 1125900439774673 | D121978137 dev, diff | 2984739366 | 415 s | 249 passed, 15 failed |
| 17732923738240851 | D121978137 opt, diff | 4503602598707220 | 384 s | 245 passed, 15 failed |
| 1125900439775734 | D121978137 dev, base | 2984739366 | 229 s | 1 passed (listing), 15 failed |
| 17732923738241798 | D121978137 opt, base | 4503602598707220 | 222 s | 1 passed, 15 failed |
| 34058472220695225 | D121978134 dev, diff | 18014401483113555 | 381 s | 240 passed, 15 failed |
| 14073749038122421 | D121978134 opt, diff | 13510801855919709 | 374 s | 236 passed, 15 failed |
| 34058472220696158 | D121978134 dev, base | 18014401483113555 | 413 s | 1 passed, 15 failed |
| 14073749038123367 | D121978134 opt, base | 13510801855919709 | 199 s | 1 passed, 15 failed |

(`run_details.time.duration`, `run_details.result_summary`.) In the diff runs, `tfa_production_containment_test` has 44 rows (43 test cases + 1 `listing`), of which 28 cases pass and 15 fail. `tfa_phase2_test` has 216 rows (**215 cases** + listing) on D121978137 and 207 (206 + listing) on D121978134, all passed. So D121978137 adds 9 unit tests. The containment count of 43 matches D121978137's own test plan (Pass 39 + Skip 4 on devbig030, 2026-09-26).

Distinct warning tests and one-line reasons (identical in all four diff runs; `retries=4` each):

| Test | CI reason (diff runs) | Same class on base-rev? |
|---|---|---|
| test_current_full_trace_cannot_qualify_minimal_guest_and_cleans_up | `sudo -n mount -o loop .../verify.btrfs` denied: "Sorry, user twsvcscm is not allowed to execute '/bin/mount -o loop ...' as root" | yes, all 4 base runs |
| test_current_full_trace_refuses_trace_drift_and_cleans_up | same sudo mount denial | yes |
| test_forced_run2_kill_confines_and_seals_verify_residue | same sudo mount denial | yes |
| test_inflight_bounded_commands_hold_recovery_until_quiescent | same sudo mount denial | yes |
| test_killed_controller_recovers_every_verify_allocation_boundary | same sudo mount denial | yes |
| test_nested_hermit_lock_serializes_independent_processes | same sudo mount denial | yes |
| test_removed_worktree_drain_rejects_real_deleted_process_root | same sudo mount denial | yes |
| test_runtime_recovery_preserves_mounted_interrupted_staging | same sudo mount denial | yes |
| test_worker_response_timeout_keeps_command_lock_until_exit | same sudo mount denial | yes |
| test_killed_worktree_body_is_reconciled_by_independent_owner | "RuntimeError: Failed to start chataccd-mount@8b35ddbd552a08c5.service: Unit ... not found", then "worktree crash body exited 1" (a missing host systemd unit, not sudo) | yes |
| test_real_codex_bootstraps_with_bound_agent_identity | "Codex **latest** release manifest identity differs". The test expected "owner-managed Codex lacks daemon-socket fixes" | yes, except D121978134 dev base: "version command could not start" |
| test_real_codex_candidate_handoff_uses_controller_buck | "Codex **stable** release manifest identity differs", giving "owner Codex refusal is not the reviewed blocked release" | yes, except D121978134 dev base (as above) |
| test_real_codex_final_message_cannot_be_redirected_by_tool | stable manifest identity differs | yes, except D121978134 dev base (evidence-preserved assertion after the Codex preflight could not start) |
| test_real_codex_native_workspace_sandbox_and_tfa_cleanup | stable manifest identity differs | yes, except D121978134 dev base ("version command could not start") |
| test_real_codex_reviewer_final_message_cannot_be_redirected | stable manifest identity differs | yes, except D121978134 dev base (evidence-preserved assertion) |

The classification script is the inline `python3` in this session. It pattern-matches "is not allowed to execute", "Codex (\w+) release manifest identity differs" and "Unit chataccd-mount@... not found" in `test_details.details` for every non-passed row of the 8 runs above.

**Verdict on the previous agent's figures**:
- Confirmed: 15 x 2 = 30; 506/538 and 528/559; all LAND_BLOCKING; all "failed on base-rev".
- Corrected: the non-Codex group is **9 sudo + 1 missing chataccd-mount unit**, not "10 sudo".
- Corrected: "98-99.9% similarity" holds only for D121978137. D121978134 dev has 5 rows at 75-86%, with a different base-rev cause.
- Not reproduced: the seq-12 briefing's "96 passed / 4 warnings". Current records do not support it.

### 2c. The two RED lower diffs (not in the previous report)
Command: `meta phabricator.diff.signals list --number D121244213|D121848886 --status=failed --columns=name,status,type,signal_class,land_friction,land_friction_reason,error,sandcastle_job,repro --limit=500 -o json` → `RAW/failed-D121244213.json`, `RAW/failed-D121848886.json`.
- **D121244213: 28 failed**, 14 containment tests x {dev, opt}. All have `signal_class=real`, `LAND_BLOCKING`, and the error "Test was added in target revision (d9b5fabaeb66...) hence is not present in base rev (14f840b5b5f0...)". TestX run 8725724661015409 (dev, Sandcastle 31525200365394532: 138 passed, 14 failed) and run 35465847104050158 (dev retry, job 18014401481397461: 22 passed, 14 failed) give the causes, in `RAW/testx-8725724661015409.json` and `RAW/testx-35465847104050158.json`:
  - 9 x sudo mount denied for twsvcscm (this includes `test_normal_verify_keeps_guest_and_controller_workdirs_clean`, which is not in the top-diff set)
  - 1 x chataccd-mount unit not found
  - 4 x real_codex at this older revision: "managed launcher did not invoke the payload wrapper"; "-9 != 0 : Codex CLI at Meta"; and "production evidence preserved ..." assertions
- **D121848886: 32 failed.**
  - 26 are "Test failed on base-rev (5504457061eb...)" but still Failed. Their similarity is 96.7-99.6%, and no inline text is given.
  - 6 are "added in target revision". 4 of these are the two `current_full_trace_*` Btrfs tests x 2 modes.
  - The other 2 are the unit test `tfa_phase2_test` `test_real_packaged_hermit_is_bound_to_blocked_provenance` x {dev, opt}: "HermitError: packaged Hermit compatibility is PROVENANCE_MISMATCH: expected revision=4630ee2a... sha256=5c422b85...; packaged revision=a919cce4... sha256=4488f9e5... (dev) / 3bff5502... (opt)".
  - That unit test **passes** by D121978137 in both modes (`results-dev.json` and `RAW/testx-17732923738240851.json`). Inference: a later diff in the stack fixed it.
- Consequence: publishing the stack as-is exposes two diffs with hard FAILED CI in the middle, in addition to the 28-34 land-blocking warnings on every other diff.

## 3. Is `fbcode/testinfra/tfa` on fbsource master?
**No.** Evidence:
- `sl log -r remote/master -T '{node} {date|isodate} {phase}\n' --reason ...` in `/home/newton/fbsource` gives `9e6ec042287d978af08117f9456d043e6b424ba6 2026-09-28 11:14 -0700 public` (`RAW/sl-remote-master.txt`). This is the local remote bookmark, about 3.5 h before my query at 14:44 PDT. No pull was done or needed.
- `sl files -r remote/master fbcode/testinfra/tfa --reason ...` gives rc 1, "fbcode/testinfra/tfa: no such file in rev 9e6ec042287d" (`RAW/sl-files-tfa-master.txt`). Probe control: `sl files -r remote/master 'glob:fbcode/testinfra/*'` gives rc 0 and 2 files (`.flake8`, `PACKAGE`) (`RAW/sl-files-testinfra-top.txt`). So the parent directory resolves and the probe works.
- BigGrep over fbsource master: `fbgs -f 'testinfra/tfa/' -l tfa`, `fbgs -f 'testinfra/' -l ProductionContainmentIntegrationTest` and `fbgs -l 'tfa.codex-environment-preflight'` all return rc 1 with no hits (`RAW/fbgs-*.txt`). Positive control: `fbgs -l 'rrnewton/hermit' -f 'fbcode/'` returns hits in `fbcode/hermetic_infra/hermit/...` (`RAW/fbgs-hermit.txt`).
- The `search_files` MCP tool could not be used. It is bound to the `/home/newton/work/dev-hermit` git repository and refuses paths in `/home/newton/fbsource` ("only paths inside that root are searchable").
- The stack's fork point is `4c52f205ae7a54bd1ab23401415b29c9d551e656` (public, 2026-09-22 02:55 PDT), from `sl log -r '6b3a2263c23c^'`. That is 6 days behind current master, which is consistent with the "Old Diff" blocker on D121244476.
- dev-hermit `origin/main` (`7424b031dafd`, 2026-09-28 14:40 PDT) has no tracked file under `experiments/` or `ai_docs/` that cites any of the 22 D numbers or `testinfra/tfa` (`git grep` returned rc 1). GitHub search of rrnewton/dev-hermit for "D121978137" returned `[]` (`/tmp/sub-tfa2-030/gh-stackstate-2.json`, via herdr-run, rc 0).

## 4. The previous agent's devvm895 validation
Sources: `/tmp/sub-tfa2-030/devvm895/tfa-diag/containment-local.log` (268 lines) and the prior final report (`/tmp/sub-tfa-history.jsonl`, seq 1885).
- **Containment: 37 pass / 6 fail, confirmed.** Log line 255 reads "Tests finished: Pass 37. Fail 6. Timeout 0. Fatal 0. Skip 0. Omit 0. Infra Failure 0. Build failure 0". Buck UI `57a77611-af88-48fd-a04e-de5606bd6610`, test session 11258999271745765, cfg `dev-linux-x86_64-fbcode-platform010-clang21-asan-ubsan-dev`, 11:15:30-11:20:02 PDT 2026-09-28 (about 4.5 min). The paths are under `/data/repos/fbsource/buck-out`, i.e., devvm895. The six failures, with the final assertion text:
  1. `test_real_codex_native_workspace_sandbox_and_tfa_cleanup` (5.2 s): "owner Codex refusal is not the reviewed blocked release: Codex executable preflight failed (tfa.codex-environment-preflight.v3): Codex stable release manifest identity differs"
  2. `test_real_codex_candidate_handoff_uses_controller_buck` (5.1 s): same text (stable)
  3. `test_real_codex_bootstraps_with_bound_agent_identity` (4.5 s): '"owner-managed Codex lacks daemon-socket fixes" does not match "... Codex latest release manifest identity differs"'
  4. `test_current_full_trace_refuses_trace_drift_and_cleans_up` (23.7 s): "AssertionError: 124 != 1 : :: Run1... :: Run2... OSError: [Errno 28] No space left on device" (test_production_containment.py line 4467)
  5. `test_real_codex_reviewer_final_message_cannot_be_redirected` (4.6 s): stable manifest identity differs, then "production evidence preserved at /tmp/tfa-phase2/production-test-failures/reviewer-report/..."
  6. `test_real_codex_final_message_cannot_be_redirected_by_tool` (4.5 s): stable manifest identity differs, then "production evidence preserved at .../codex-report/..."
  The five Codex failures have the same class as CI. The ENOSPC failure is devvm895-only. The 9 sudo tests and the chataccd-mount test **passed** on devvm895, so they are CI-only.
- **"Build rc 0" and "tfa_phase2_test 215/215": not verifiable from retained evidence.** No build or unit-test log was copied to devbig030. `/tmp/sub-tfa-history.jsonl` contains only thinking and text blocks (0 tool-result records), and `way-forward.md` line 49 repeats the claim without a log. Corroboration (a different host and tree): CI shows 215 `tfa_phase2_test` cases passing in both modes on D121978137 (2026-09-26, Sandcastle), plus passing type-checking targets (`tfa_phase2-library-type-checking`, etc.).
- **`results-dev.json` is not local validation.** It is TestX run 1125900439774673 = D121978137 mode/dev **CI** (Sandcastle job 2984739366, 2026-09-26, 415 s, 249 passed / 15 failed).
- Also unverifiable from here: the claim that the stack tip was overlaid on master `8ffb424766c0`; the devvm895 Codex digests (launcher `c37d7ac7`, stable `d291f08a`, latest `7e520ac7`); and the ENOSPC count. The prior thinking at seq 1283 says 3/3 runs and the final report says 4/4, where the fourth run was on D121978134's tree (inference). Only one run is in the retained log.

## 5. Phase and who touched the diffs
- **Phase (devbig030, `/home/newton/fbsource`)**: `sl log -r 'not public()' -l 2000 -T '{node|short} {phase} {phabdiff} {date|isodate} {desc|firstline}\n'` gives 290 non-public commits (`RAW/sl-nonpublic-fbsource.txt`). All 22 stack commits are `draft`. The local tip for D121978137 is `317d325e8eb0f2b0f2f2f7d6badde84e9faeca7a` (draft, 09:22 PDT 2026-09-26, p1 = `266a65acae24` = D121978134).
  - Phabricator's `commit_hash` for D121978137 is `a729ddf3707f0ae343cbb7bdac80856afe7ccb25` (draft, hidden, 09:20). `sl log --hidden -r 'predecessors(317d325e8eb0)+successors(317d325e8eb0)'` shows a729ddf is the direct predecessor, and `4fb8c412...` (the tree the test plan says was evaluated, 00:06 PDT) comes before it.
  - `sl diff --hidden -r a729ddf3707f -r 317d325e8eb0 --stat` is empty (`RAW/sl-diff-a729-317d.txt`, 0 bytes). The two messages differ only in the Test Plan: "Pending; updated below after verification." was replaced by the full plan (`RAW/desc-a729.txt` vs `RAW/desc-317d.txt`). Inference: the Test Plan was edited on Phabricator and synced locally, and Phabricator still records the pre-sync hash.
  - The devvm895 report of `secret` came from a different repository (`devvm895:/data/repos/fbsource`) and cannot be checked read-only from devbig030. Phase is per-repository, so both can be true.
  - The working copy of `/home/newton/fbsource` is on an unrelated draft (`e79d544a25ca`, "[third-party][rust] Vendor liteinst2 at the revision Reverie pins"). Six "WIP autosave before eden rm of .../tfa-phase2/..." drafts (2026-09-26 19:57-20:44 PDT) also exist there.
- **Actors**:
  - `meta phabricator.diff history` (214 transactions over 22 diffs) has exactly two authors: `newton` (209) and `generatedunixname537391475639613` (5). The latter posts only `differential:ai_diff_review` "General Reviewer", on D121244476, D121244213, D121848525, D121877478 and D121877481.
  - All 43 versions are authored by `newton`. `agent-activity` returns 0 rows for all 22. Reviewers are 0 and subscribers empty everywhere.
  - So no human other than the `newton` account has touched any diff. **But `newton` does not identify the owner.** Thirteen diffs (D121649569 through D121848525) carry agent who-am-i tags naming devbig030 agents: `unknown-agentname`/`codex-family-by-ancestry` (2), `unknown-agentname`/`gpt-5.6-sol` (1), and `tfa-phase2-cyber-opus`/`claude-opus-5-5` (10). The other 9 have no tag. Inference: all 22 were produced by agents acting through the owner's account. The records cannot show which, if any, the owner submitted personally.

## Raw evidence index
`RAW/` = `/tmp/sub-tfa2-030/gather/stackstate-raw/`: `describe|files|reviewers|comments|history|versions|agent-activity|cistate-D*.json`, `stack-ci-D121978137.json`, `warn-D1219781{34,37}.json`, `warn-D121244476.json`, `failed[-url]-D121244213|D121848886.json`, `info-D1219781{34,37}.json`, `testx-*.json` (8 runs + 2 D121244213 runs), `sl-*.txt`, `desc-a729.txt`, `desc-317d.txt`, `fbgs-*.txt`, `table.md`.
