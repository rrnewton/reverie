# Fact-check of the TFA way-forward draft, plus inputs for the owner's three questions

Gatherer: sub-tfa2-030 "wayforward". Host: devbig030.atn3.facebook.com (`hostname -f`). Work done 2026-09-28, about 21:00-22:30 UTC.
Target: `/tmp/sub-tfa2-030/devvm895/tfa-diag/way-forward.md`, identical to `/tmp/sub-tfa2-030/devvm895/way-forward.md` (`diff` printed SAME). The previous lane's research and reviews are in `/tmp/sub-tfa2-030/devvm895/tfa-diag/way-forward-full.json`.
Sources read:
- Stack-tip source: `/tmp/sub-tfa2-030/devvm895/tfa-src/fbcode/testinfra/tfa/`. Below, "stack:" means a path in that directory.
- The stack's diff summaries: `/tmp/sub-tfa2-030/devvm895/tfa-src/STACK_SUMMARIES.md`.
- fbsource at `/home/newton/fbsource`, working copy `e79d544a25ca`. That is one draft commit on public `b06cf54186b9` (2026-09-27), found with `sl log -r 'last(::. & public())'`.
- Open-source Hermit at `/home/newton/work/dev-hermit/hermit`. HEAD is `e6323658` (2026-09-27); `origin/main` is `fdd8f55a` (2026-09-28).
Every `meta` command used was checked first with `meta metacli.command annotations`; all were `mutation_risk_level NONE`, `AUTONOMOUS`.
Labels: **PROVEN** means the record shows it. **INFERENCE** means it is my reasoning.

---

## 0. Most important corrections

1. **The Hermit import step is wrong.** D120908510 cannot carry 4630ee2a:
   - D120908510 is an unpublished stub: 16 lines, 1 file (`meta phabricator.diff describe --number=D120908510`). The import attempt produced only that stub after an HTTP 500 (summary of D120908815).
   - Its source is https://github.com/facebookexperimental/hermit/pull/93. That pull request is **OPEN**, with head `4dcb34b4` (`/tmp/sub-tfa2-030/gh-wayforward-1.json`).
   - `4dcb34b4` is the **parent** of 4630ee2a (`git log --oneline -3 4630ee2a` in `dev-hermit/hermit`). 4630ee2a is not an ancestor of it: `git merge-base --is-ancestor` returned 1.
   - The same patch (stable patch-id `c9843d1e…` for both commits) landed on rrnewton/hermit main as `ccbe37e9` through https://github.com/rrnewton/hermit/pull/3142, merged 2026-09-21T14:23Z (`/tmp/sub-tfa2-030/gh-wayforward-8.json`).
   - facebookexperimental/hermit main (`6b3f7ef0`, 2026-09-28) does **not** contain `ccbe37e9`: the GitHub compare returned `"diverged"`, `ahead_by` 26, `behind_by` 2675 (`/tmp/sub-tfa2-030/gh-wayforward-3.json`).
   - So the import would have to be refreshed to current rrnewton main, not "finished".
2. **The "existing flaky-test agents" named in the draft are the wrong audience.**
   - `fbcode/claude-templates/components/skills/fix-flaky-test/SKILL.md` covers iOS IAB tests. It searches `fbobjc/Libraries/IAB/...` (SKILL.md "Step 1").
   - AutoTestRemediation installs a different thing, the **plugin** `fix-flaky-test` (`www/flib/intern/sandcastle/commands/AutoTestRemediationCommand.php:200-201`). The plugin is for Jest/E2E/WWW tests (`fbcode/claude-templates/components/plugins/fix-flaky-test/.claude-plugin/plugin.json`: keywords jest, e2e, wwwtest; command `fix-flaky-e2e.md`).
   - AutoTestRemediation belongs to `billing_experiences_customer_success` (`AutoTestRemediationCommand.php:42`).
   - Neither targets fbcode C++ gtests.
3. **Sandcastle is "not yet", not a flat "no".**
   - A candidate fix for Hermit's Sandcastle failure exists: D121815443, "Fix hermit_run_default__custombin_minimal_hello: proc mount EPERM in CI sandbox". It is **Needs Review**, reviewers jasonwhite and newton, published 2026-09-25 (`meta phabricator.diff describe --number=D121815443`).
   - Its root cause: the kernel rejects a writable `/proc` mount in CI with "VFS: Mount too revealing". The fix retries the mount once with `MS_RDONLY`.
   - Its test plan reports 35 pass / 0 fail / 1 timeout for the 36 `hermit_run_default__*` targets, 29/32 for the `hermit_run_strict__*` targets, and the named test passing.
   - The failing test is attached to T290084710 (owner newton, created 2026-09-24).
4. **devbig030 is the only host where the witness replays can run today, but the owner does not own it.**
   - All pinned inputs exist here with matching SHA-256 (§1, row 26).
   - The devserver registry lists the host as owner **davidai**, purpose "sched_ext development", `others_can_sudo false` (`meta devserver.server describe --hostname=devbig030.atn3.facebook.com`).
   - The earlier reviewer's "devbig030 not found" is wrong: the fully qualified name resolves.
5. **The open question "who produced the candidate patches" is answered.**
   - They came from Claude investigator agents, not Codex and not the stack's `run-agent`: "One agent per case (Claude harness, model `claude-opus-5-5`, session `tfa-inv-<case>`)". They worked in `/home/newton/work/tfa/phase2-cases/investigators/<case>`.
   - Source: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/RESULTS.md` §2.2, file dated 2026-09-25T07:28.
   - The asyncssl patch is 16 lines and changes only the test: `.../investigation/asyncssl/agent-output/patch.diff`.
6. **Old TFA's automatic runs cannot happen even if the rate limit were raised.**
   - The trigger itself was deleted: `0f046cdf3542` 2025-10-01 nikk "[ti] remove TestFlakinessAnalyzerFlakyTestTailer" (`sl log www/flib/intern/hermetic_infra`).
   - The live sitevar is still 0 (row 5).
   - The last human edit to it was 2023-06-13 by vmakaev (`meta sitevar.sv history --name=TEST_FLAKINESS_ANALYZER_RATE_LIMIT`).

---

## 1. Claim table (every factual claim in way-forward.md)

Verdicts: CONFIRMED / PARTLY / WRONG / UNVERIFIABLE.

| # | Draft claim (section) | Verdict | Evidence | Corrected statement |
|---|---|---|---|---|
| 1 | Paths are under `fbcode/testinfra/tfa/` at stack tip a729ddf3707f (header) | UNVERIFIABLE (the hash only) | The snapshot contains those paths. I did not re-derive the tip hash. | none |
| 2 | Old TFA (2021-22) re-ran flaky tests on Sandcastle, including Hermit chaos seeds (§2) | CONFIRMED | T106084722 "TFA MVP", created 2021-11-17, owner Ryan Newton, CLOSED, items "[X] Launch on sandcastle", "[X] hermit chaos / stress". Wiki https://www.internalfb.com/wiki/TFA/Architecture/ shows WWW plus Sandcastle/Skycastle jobs. `tools/skycastle/workflows2/hermetic_infra/tfa/tfa_run_experiment_workflow.sky:152` has an example `--hermit-experimental-tool-flags="run ... --chaos ..."`. | Add the dates. Public in Nov 2022, and hermit analyze stack traces shown in H2 2022 (post https://fb.workplace.com/groups/hermit.fyi/permalink/865638134483633/, 2022-12-16). Moved to Test Infra when TIV stopped funding Hermetic Infra (post https://fb.workplace.com/groups/hermit.fyi/permalink/858293301884783/, 2022-12-07: "Test Flakiness Analysis (TFA) tool will also move to the Test Infra team"). |
| 3 | Old TFA is dead: automatic runs off, per_day_limit 0 (§2) | CONFIRMED | Live value, 2026-09-28: `meta sitevar.sv read --name=TEST_FLAKINESS_ANALYZER_RATE_LIMIT` returned `{"flaky_test_tailer":{"per_day_limit":0,"test_target_per_day_limit":0}}`. The same value is in `www/flib/intern/__db_generated__/SitevarAutoDefaultDBCigarUpdateScript/it/SV_TEST_FLAKINESS_ANALYZER_RATE_LIMITAutoDefault.php`, generated 2026-04-10 (`sl log`, commit 567e01ba1532). | Add that the trigger ("Flaky Test Tailer") was deleted on 2025-10-01 (`0f046cdf3542`), so no automatic run can start whatever the sitevar says. |
| 4 | When did automatic runs stop? (open question) | PARTLY answerable | `meta sitevar.sv history --name=TEST_FLAKINESS_ANALYZER_RATE_LIMIT`: created 2022-06-07 by willhughes; willhughes edits 2022-06-09, 07-25, 10-05, 10-31, 11-23; **2023-06-13 vmakaev**; 2025-09-26 automatic resync by config_infra. The history does not show values. | INFERENCE: set to 0 on 2023-06-13 by vmakaev, the only human edit after the team dissolved. The trigger was deleted on 2025-10-01. |
| 5 | "rate-limited to 0/day" | CONFIRMED | Row 3. The wiki https://www.internalfb.com/wiki/TFA/Development/ names this sitevar as the killswitch: "Set per_day_limit ... to 0". | none |
| 6 | Most of its code is deleted (D82956704, D118696067) (§2) | CONFIRMED, with details added | D82956704 "[ti] remove TFA tests", Nikolay Kondratyev, 2025-09-22, quoting the hermit oncall: "Yes, that is all dead code. Fine to delete". D118696067 is a dead-code codemod that deleted TFAReport, TFADetailReport and TFADiagnoses; created 2026-09-03, closed 2026-09-10. `sl log www/flib/intern/hermetic_infra` shows nikk's `[ti]` series from 2025-09-22 to 2025-11-12: tests, tailer, skycastle workflows, experiment definitions, diagnosis definitions, ents moved to DeletionEntSchema, "remove more TFA code". Codemods followed on 2026-01-02 and 2026-09-10. | Still present: `www/flib/intern/hermetic_infra/tfa/experiments/TFAExperimentName.php`, `www/flib/intern/hermetic_infra/HermeticInfraTests.php`, the Skycastle stubs under `tools/skycastle/workflows2/hermetic_infra/tfa/` (2 workflows and 2 `__tests__`), and the sitevars. tpx no longer has a Hermit hook: `grep -rIl -i hermit --include=*.rs --include=*.toml --include=BUCK` in `fbcode/testinfra/tpx` returned rc 1 (no matches). The `buck test -- --hermit-experimental-tool-bin` path old TFA used is gone. |
| 7 | The stack reuses the technique: `hermit run --chaos` with fixed PASS/FAIL seeds (cases.v1.json:55,72) (§2) | CONFIRMED | stack: `cases.v1.json:55` `"--chaos"`, `:72` `"--sched-seed"`. The argv is `bin/safehermit ... hermit-4630ee2a run --backend ptrace --strict --chaos ...` (`cases.v1.json:48-76`). | none |
| 8 | Its "historical" records are the owner's 2026-09-21 runs (manifest.py:15-35) (§2) | CONFIRMED | stack: `manifest.py:13-35` has `DEV_HERMIT_COMMIT` `edd96831…` and URLs under `experiments/tfa-kernel71-*_20260921`. `edd96831` is on dev-hermit `origin/main` (2026-09-22, "Land kernel 7.1 TFA campaign history"). | none |
| 9 | It borrows another team's bot (comments.py:21; AutoTestRemediationScheduler.php:41-46) (§2) | CONFIRMED | stack: `comments.py:21` `SERVICE_USER_FBID = "4533519350210414"`, also `pipeline.py:2104`. `www/flib/intern/scripts/auto_test_remediation/AutoTestRemediationScheduler.php:41-46`: "AutoTestRemediation Bot — dedicated Phabricator service user … ACL: INTERN_AUTOMATION:4533519350210414 — act_as granted to SANDCASTLE_CMD:AutoTestRemediationCommand"; `<<Oncalls('billing_experiences_customer_success')>>` at :34. | none |
| 10 | …and puts its code under the testinfra oncall (BUCK:16) | CONFIRMED | stack: `BUCK:16` `oncall("testinfra")`, `tests/BUCK:5` the same. The line came from `hub init --oncall` (comment at BUCK:15). | none |
| 11 | It shares an idea with old TFA, not code (TL;DR 2) | CONFIRMED | `grep -rli 'skycastle\|hermetic_infra/tfa\|TestFlakinessAnalyzer\|internalfb.com/tfa\|flakytestanalyzer'` over the stack: 0 files. | none |
| 12 | 22 unpublished, unreviewed diffs D121244476 to D121978137, adding 33 files (§1) | CONFIRMED | `meta phabricator.diff describe` for each of the 22: every one "Unpublished", no reviewers, created 2026-09-22 to 2026-09-26. `find fbcode/testinfra/tfa -type f \| wc -l` = 33. | The diffs' `line_count` values sum to 36,416 changed lines. D121244213 alone is 20,138 (55%). |
| 13 | 32.7k-line AI-written prototype; half (16.1k) is tests (TL;DR 2, §1) | CONFIRMED (sizes), PARTLY ("AI-written") | Total 32,682 lines; the `tests/` directory has 16,181 (49.5%) (`xargs cat \| wc -l`). The STACK_SUMMARIES tags name agents on 13 of 22 diffs: 10 `tfa-phase2-cyber-opus, claude-opus-5-5`, 2 `codex-family-by-ancestry`, 1 `gpt-5.6-sol`. | "AI-written" is proven for 13 diffs. For the other 9 it is INFERENCE. |
| 14 | "does not discover tests or schedule production work" (README.md:3-4) | CONFIRMED | stack: `README.md:3-4`. | none |
| 15 | About 5 diffs add features and 17 add pinning, tamper checks and refusals (§1) | PARTLY (INFERENCE) | Titles in STACK_SUMMARIES.md fit: capability in D121244476, D121848892, D121848893, D121848896, D121978137; most others say "Bind", "Hash-bind", "Seal", "Pin", "Refuse", "Keep … dry-run-only". | Fine as a labelled estimate. |
| 16 | Intended flow (state.py:21-28) | CONFIRMED | stack: `state.py:21-28`: VALIDATED_REPRO → INITIAL_COMMENT → INVESTIGATING → FIX_DRAFTED → VERIFIED → DIFF_DRAFTED → FINAL_COMMENT. | none |
| 17 | No test plan shows a real `run-case`; the full flow is tested only with mocks (§3) | CONFIRMED | The only mention of `run-case` in STACK_SUMMARIES.md is at :989: "wired only into `run-witness`, not `run-case`". | none |
| 18 | Only 3 hard-coded cases (manifest.py:249-277); /home/newton paths (manifest.py:193) (§3, §4) | CONFIRMED | stack: `manifest.py:249-250` "phase two requires exactly three cases"; `:253-254` the IDs must be exactly httpcoro, thrift, asyncssl; `:256-260` only asyncssl may have a live task; `:271-276` per-case canonical SHA-256; `:193` argv[0] must be `/home/newton/work/dev-hermit/bin/safehermit`. | none |
| 19 | dry-run reads the live task's comments and dry-runs a comment as the borrowed bot for asyncssl (orchestrator.py:48-80; cases.v1.json:288) | CONFIRMED | stack: `orchestrator.py:47-66` (`if case.live_task` → `list_comments` → `dry_run_payload`); `comments.py:201-231` (`meta tasks.comment ... --service-user=4533519350210414 --dry-run`); `cases.v1.json:288` `"live_task": true`. | none |
| 20 | Replay with internal Hermit refuses (a919cce BLOCKED) (§3, TL;DR 1) | CONFIRMED | stack: `hermit-import-compatibility.v1.json`: `packaged_executable.state` BLOCKED ("--verify-verbose rejects /bin/true … CPU identity, elapsed timing, and async-resource ordering differ"); `qualification_probe.state` BLOCKED; short candidates: asyncssl "AddressSanitizer startup CHECK failed in setrlimit(RLIMIT_CORE)", thrift "ThreadSanitizer … SIGILL", httpcoro "Hermit PMU single-step skid panic". Three live runs on devbig030 on 2026-09-25 all ended `HermitError: packaged Hermit compatibility is PROVENANCE_MISMATCH` (`/home/newton/work/tfa/three-case*/SUMMARY.txt`). Internal Hermit is still the a919cce import: `sl log fbcode/hermetic_infra/hermit/hermit-cli/src/bin/hermit/main.rs` shows 5a90c83fbd90, 2026-07-25, "Source: `rrnewton:stable` at `a919cce…`", D113553943. | The RLIMIT_CORE blocker is fixed upstream in `ccbe37e9` (https://github.com/rrnewton/hermit/pull/3142). The PMU-skid blocker matches the open issue https://github.com/rrnewton/hermit/issues/1845 (INFERENCE that they are the same problem). |
| 21 | Open-source Hermit 4630ee2a: yes, for all three cases on the original host (D121978137) (TL;DR 1, §3) | CONFIRMED | `/home/newton/work/tfa/external-oss-mode/D121978137-test-plan.txt`: on 2026-09-26, 09:06-09:15 PDT, 8/8 steps rc 0 in 210 s: validate-manifest, dry-run, and 6 `run-witness` runs (3 cases, fail and pass), "expected fail/pass outcomes"; httpcoro fail core-dumped at 108 s against the 120 s bound. The previous attempt (rerun 2) failed closed when the httpcoro fail witness hit the 120 s wall. | Say "6 Hermit witness runs reproduced the recorded PASS/FAIL seeds, on the third attempt". "TFA results" here means the owner's September 2026 phase-one seeds, not old-TFA reports. |
| 22 | Sandcastle: Hermit's own test fails there on a mount error, 53 runs straight (`meta testinfra.test metadata --id=562950400403548`) (TL;DR 1) | CONFIRMED, context missing | `meta testinfra.test metadata --id=562950400403548 --output=json`, run 2026-09-28 about 21:40Z: `hermit_run_default__custombin_minimal_hello - unmanaged`, status PROBLEMATIC, `trunk_state` DISABLED_FAILING, "number of consecutive trunk failures: 53.0". The last 10 CI results are FAILED with `container.rs:842:26 … EPERM, context: Mount`; the latest ended 2026-09-28T21:38Z. `meta testinfra.run get --id=28710447663535899.562950400403548.1790585925` gives `execution_type` SANDCASTLE_JOB. The journal shows `SignalInfraTestSeenInMaster` and `SeenFirstFailure` at 2026-09-24T22:14Z. | The test first appeared on trunk on 2026-09-24 and has never passed there. The fix D121815443 is awaiting review (§0.3). Correct wording: "not yet; a fix awaits the owner's review". |
| 23 | Little runs end to end: internal Hermit and Codex blocked; comments dry-run only (TL;DR 3) | CONFIRMED | stack: `process.py:104-121` (both `_CODEX_COMPATIBILITY` keys `"blocked"`); `comments.py:287` unconditional `raise CommentError(UNTRUSTED_AUTHORIZATION)`; `main.py:282,374` (`run_seed` and `run_case` go through `_execution_manifest`, which uses the packaged Hermit). | none |
| 24 | Real Codex ran in D121244213's tests (2026-09-22); blocking began at D121848885 (D121848886: codex-cli 0.156.0) | PARTLY | The D121244213 summary claims real Codex runs; its test plan lists only "tfa_production_containment_test (11 passed)", with no per-test names. The D121848885 summary: "Record versioned Codex executable identity and reject known-blocked…". The D121848886 test plan: "four real Codex tests fail closed on unsupported codex-cli 0.156.0". The current pins say 0.156.1 (`process.py:106-120`). | Say "D121244213's summary claims real Codex runs". The version pinned today is 0.156.1. |
| 25 | Containment tests in Sandcastle CI: 15 of 43 fail per build mode; land-blocking warnings that also fail on the parent revision (§3) | CONFIRMED | `meta phabricator.diff.signals list --number=D121978137 -l 1000`: 559 signals, 30 Test/Warning, all in `tfa_production_containment_test`, 15 distinct tests × 2. `--status=warning --details`: all 30 are `LAND_BLOCKING`, and each says "Test failed on base-rev (8969df1b…)". | none |
| 26 | On Sandcastle, 9 fail on denied sudo mount, 1 on a missing devserver service, 5 on Codex hash mismatches (§5) | CONFIRMED | `/tmp/sub-tfa2-030/devvm895/tfa-diag/results-dev.json` (run 1125900439774673, mode/dev): 15 Failed; 9 contain "not allowed to execute", 5 "identity differs", 1 "chataccd". The failing test names match (5 `test_real_codex_*`, 1 `test_killed_worktree_body…`). | none |
| 27 | Tests run on any devserver with passwordless sudo and quota-enabled Btrfs (resources/bounded-run-space:95-99,191-224) (§5) | PARTLY | stack: `resources/bounded-run-space:95-96,191-192` needs Btrfs plus `sudo -n`. However, the tests create their own loop-mounted, quota-enabled Btrfs image (`tests/test_production_containment.py:351-409`, per the previous lane). devvm895's `/tmp` has quotas off and still passed 37 (`containment-local.log`: "Pass 37. Fail 6"). | Say "passwordless sudo plus Btrfs; the tests build their own quota-enabled loop fixture". On devbig030 the same suite passed 39 with 4 skips (the 4 blocked real-Codex tests), 0 fail, in 316 s (`D121978137-test-plan.txt`). |
| 28 | devvm895 passes 215/215 unit tests and 37/43 containment tests (§5) | PARTLY | 37 pass / 6 fail CONFIRMED (`/tmp/sub-tfa2-030/devvm895/tfa-diag/containment-local.log`, 2026-09-28 11:15-11:20 PDT). No log for 215/215 is in the copied evidence. | 215/215 is UNVERIFIABLE from these records. |
| 29 | Hermit replays need /home/newton/work files, missing on devvm895; they are on devbig030, where 13 of the 22 diffs were developed (§5) | CONFIRMED, with stronger evidence | On devbig030 every manifest path exists. `sha256sum` matches each pin: Hermit `5c422b85…` (= `cases.v1.json` `hermit_sha256`); the ELFs match `artifact_sha256` (httpcoro `00066235…`, thrift `2ec5d212…`, asyncssl `47b6c727…`). The packaged `resources/safehermit` matches `PINNED_SAFEHERMIT_SHA256` `a8a2965e…` (`process.py:42-44`). The live `bin/safehermit` has changed (`fa2a29d7…`, modified 2026-09-27). The runtime copies the packaged one (`hermit.py:1441-1448`); the manifest only checks argv[0]'s string. 13 of 22 diffs carry the devbig030 tag; D121244213's test plan also uses `/home/newton/work/tfa/fbsource`. | Add: the host is registered to davidai (§0.4), and the witnesses were recorded for the "kernel71" campaign. devbig030 runs 7.1.3 (`uname -r`). INFERENCE: moving hosts may change seeds. |
| 30 | It is devserver-only (§5) | CONFIRMED | stack: BUCK has only `hub_python_binary` plus `cli.metadata(binary_name="tfa-phase2")`. README "Runtime output must stay under /tmp/tfa-phase2 …". There is no fbpkg, Tupperware, Chronos or Sandcastle job definition. | none |
| 31 | Codex row: all pins blocked (process.py:104-121) | CONFIRMED | Row 23. | none |
| 32 | Task comments always refused (comments.py:287) | CONFIRMED | Row 23. README.md:13-14: "Posting needs a trusted issuer of task/action-bound receipts, which does not exist yet." | none |
| 33 | It would be a fixer outside Test Infra's resolver, which their wiki recommends against; that resolver doesn't take flaky tests today (DefaultTestFixerStrategy.php:9-11) (§4) | CONFIRMED | Wiki https://www.internalfb.com/wiki/Test_Infra_For_Users/Getting_Started/Test_Issues/Agentic_Test_Fixers/: "We recommend against implementing your own fixer outside of our resolver". `www/flib/intern/tasks/devmate_assignment_config/task_types/test_fixer_strategies/DefaultTestFixerStrategy.php:9-11`: `SUPPORTED_ISSUE_TYPES = keyset[FAILURE]`. `TTestFixerStrategyTrait.php:167` filters on it. | none |
| 34 | Worth keeping: about 0.6k lines of hint and verdict logic (hints.py; pipeline.py:119-342; followup.py:199-338) (§4) | CONFIRMED (arithmetic) | 204 + 224 + 140 = 568 lines. | none |
| 35 | Hermit already does the core: `hermit analyze --search` finds a failing chaos seed, pairs it with a passing run, and prints stack traces (types.rs:22-43) (§6) | PARTLY | Internal: `fbcode/hermetic_infra/hermit/hermit-cli/src/bin/hermit/analyze/types.rs:22-43` (doc) and `:69-71` (`--search`). `main.rs:131-138` has LogDiff, Analyze ("Analyze Pass and failing runs") and Bisect. Open-source: the same file, lines 22 and 69-71. `analyze/phases.rs:893-930` (`do_search`) is `loop {}` with **no round cap**; each round picks a random `--sched-seed`. The doc says: "If a run2 is not provided, analyze assumes that a hermit run without chaos is the baseline." `docs/USER_GUIDE.md:417-428`. | The default baseline is a no-chaos run, not a searched PASS seed. `--search` has no bound, so a wrapper needs its own timeout. The stack never calls analyze: `grep analyze` over its `*.py` finds nothing. No 2026 record shows analyze working on these three large gtest binaries. Old TFA used it in 2022. (INFERENCE: it may work.) |
| 36 | A fix-flaky-test skill, used by AutoTestRemediation (AutoTestRemediationCommand.php:199-201) (§6) | WRONG (conflation) | See §0.2. | "AutoTestRemediation, a Chronos-launched Claude-on-Sandcastle fixer owned by billing_experiences_customer_success, installs a Jest/E2E fix-flaky-test *plugin*. A separate fix-flaky-test *skill* targets iOS IAB tests. Neither covers fbcode C++." |
| 37 | Devmate's asyncssl attempt errored (T255721720) (§6) | CONFIRMED | `meta tasks.task describe --task=T255721720`: `latest_agent_assignment` agent_type DEVMATE, agent claude, status **ERROR**; tag `task_delegated_to_devmate`; owner gouthamarukonda, Service Encryption, progress NO_PROGRESS. | none |
| 38 | None appear to use Hermit (inference) (§6) | UNVERIFIABLE (INFERENCE) | The previous lane searched only a sample of sources. | Keep it labelled as inference. |
| 39 | Sandcastle userns prerequisites targeted mid-October (sandcastle.eng post 4477862635875268) (§6) | CONFIRMED | https://fb.workplace.com/groups/sandcastle.eng/permalink/4477862635875268/ (2026-09-17): "We are targeting to complete the container prerequisites by mid-October." The items include "Fix /proc bind mount path clash under a shifted userns". | Add: that item touches the same `/proc` mount Hermit fails on (INFERENCE that the two interact). |
| 40 | Sandcastle is moving mount-heavy tests into VMs (T288822939), a possible Hermit route (inference) (§6) | PARTLY | T288822939 is "[userns prereq] Move sudo twagent tests to vmtest", owner yuangaobk, IN_PROGRESS, created 2026-09-16. | It covers twagent sudo tests only, not mount-heavy tests in general, and does not mention Hermit. The VM route for Hermit remains INFERENCE. |
| 41 | Hermit is x86-only (commit 4620432e135b) (§6) | CONFIRMED | `sl log -r 4620432e135b`: 2026-09-21, meyering, "Gate the hermit family x86-only": detcore interposes x86 `cpuid`, and 554 targets are incompatible on aarch64. Open-source `docs/TESTING_ENVIRONMENTS.md:118`: "Hermit targets x86-64 Linux. AArch64 is incomplete". | none |
| 42 | …native fbcode tests only (§6) | PARTLY (INFERENCE) | Hermit runs any Linux x86-64 process. The a919cce strict-compat probe passed `/usr/bin/python3` (D113553943 message). Old TFA supported C++ and Rust (wiki TFA/). | Say "Linux x86-64 fbcode tests (C++/Rust, possibly Python); not WWW/Hack/Jest". |
| 43 | A standalone service needs its own service user (wiki Tasks/Task_Bots), not the "trusted issuer" (README.md:13-14; inference) (§6) | PARTLY (the citation) | The procedure I found is https://www.internalfb.com/wiki/Service-users/creating-bots/: "Create a Service User … check the 'Limited Tasks Access' box". An act_as ACL for Sandcastle automation is shown at `AutoTestRemediationScheduler.php:43-44`. | Cite Service-users/creating-bots. "Trusted issuer not needed" is still INFERENCE. |
| 44 | Most of it needs repair or rebuild; drop Codex and 10-12k lines (60-75%) of pinning and sandboxing (§7) | PARTLY (INFERENCE) | Product code = 32,682 − 16,181 = 16,501 lines; the previous lane's AST tally gives about 10.1k lines of binding and containment. | Reasonable as a labelled estimate. |
| 45 | Step 2: the asyncssl candidate patch (D121848896: failed 5× unfixed, passed 3× patched; small sample) | CONFIRMED, with details added | STACK_SUMMARIES.md:562-575, and RESULTS.md §4: seed 11 was FAIL×5 and NONDETERMINISTIC×2 unfixed, PASS×3 patched. Over all seeds asyncssl FAILed 5 of 14 unfixed runs; patched runs gave 8 PASS and 1 OTHER. RESULTS.md §5 lists a rank-3 alternative: "a production defect: `handleRead` should drain the error queue … If it holds, the candidate patch hides a production defect." | Patch path: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/patch.diff`, 16 lines, changing only `folly/io/async/test/AsyncSSLSocketTest.cpp`. All evidence comes from Hermit runs: RESULTS.md §8 says "None of it is native, CI or landed evidence." |
| 46 | Step 3: seeds depend on paths and timing (D121848893) | CONFIRMED | RESULTS.md §4: "The manifest FAIL seed 1 PASSed at the stress path … the Hermit schedule depends on them." | none |
| 47 | Step 4: finish import D120908510 (rrnewton/hermit via facebookexperimental/hermit) and confirm it includes 4630ee2a | WRONG | §0.1. | "Refresh https://github.com/facebookexperimental/hermit/pull/93 (head `4dcb34b4`, which lacks the fix) to current rrnewton/hermit main, which contains `ccbe37e9`. Get it merged, re-run the CodeSync import, then re-qualify and re-pin. That may unblock internal Hermit (INFERENCE); the PMU-skid blocker is separate." |
| 48 | Step 6: probe Sandcastle with `hermit run --namespace-only -- /bin/true` | PARTLY | D121815443 already ran this probe inside CI (the diff reports `unshare … --mount --pid` reproduces the EPERM), and it touches `run_with_namespace_only()`. | Replace with "review D121815443 (owner is a reviewer), then check whether the 36 hermit_run_default tests pass on trunk". |
| 49 | Step 7: offer a Hermit step to the fix-flaky-test owners | WRONG target | Rows 33 and 36. | The only central path is testing_core's TestFixerStrategyResolver, which handles FAILURE only. Adding FLAKY would need testing_core to agree (owner decision). |
| 50 | Step 8: delete /tmp/tfa-phase2 on devvm895 | UNVERIFIABLE from devbig030 | none | Note: devbig030's `/home/newton/temp/tfa-phase2/` holds the external-OSS fixtures behind D121978137. Do not delete it without the owner's approval. |
| 51 | Is 4630ee2a in public facebookexperimental/hermit? (open question) | ANSWERED: no | §0.1. It is public on rrnewton/hermit: `gh api repos/rrnewton/hermit/commits/4630ee2a…` returned the commit, "Accept virtual RLIMIT_CORE updates", 2026-09-20T14:33Z (`/tmp/sub-tfa2-030/gh-wayforward-7.json`). Locally it is on `origin/salvage/devbig014/hermit-tfa-prlimit-fix/hermit-1-4630ee2a61f9`. | none |
| 52 | What produced the D121848896 patches? (open question) | ANSWERED | §0.5. | none |
| 53 | Only asyncssl is still flaky (60%, disabled); httpcoro and thrift are marked OK (TL;DR 2) | PARTLY | See §2. Both are OK/PASSING, but thrift's flakiness score is **10.8%**, below the 15% disable threshold (TestX hint `clean_window_but_scored_flaky`). httpcoro's score is 0.26%. | "Only asyncssl is disabled (DISABLED_FLAKY at a 60.0% score; 52 of 186 stress runs failed from 2026-09-26 to 09-28). thrift is OK but still scores 10.8% flaky. httpcoro is OK at 0.26%." |

---

## 2. Current flakiness of the three working tests (and Hermit's own test)

All commands ran on devbig030 on 2026-09-28, about 21:40-22:00 UTC.
- `meta testinfra.test metadata --id=<id> --output=json`
- `meta testinfra.result history --test=<id> --since=ONE_WEEK --final-only --limit=200 --output=json`. With `--since=ONE_MONTH` it timed out after 120 s (srproxy OVERALL_TIMEOUT).
- `meta testinfra.issue list --test-id=<id> --output=json`

| Test (id) | status / trunk_state | flakiness score | Mode | Last week of final results | Issues / tasks |
|---|---|---|---|---|---|
| asyncssl `fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect` (562950211052906) | FLAKY / **DISABLED_FLAKY** ("simple flakiness score: 60.0% (threshold: 15.0%)"); diff_eligibility INELIGIBLE | 0.6002; brokenness 0.009 | @fbcode//mode/libcxx | 200 results (the limit) from 2026-09-26T08:32Z to 09-28T21:40Z. STRESS_RUN 27 FAILED / 63 PASSED; STRESS_RUN_NEW_TEST 25 FAILED / 71 PASSED; 14 OMITTED because disabled. **52/186 = 28.0% of stress runs failed.** | FLAKY issue 224226669 **OPEN**, task T255721720 (Service Encryption); three older FAILURE issues closed. Journal: failed and passed stress tests alternating from 2026-09-22 to 09-28. |
| httpcoro `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - …ResetRateLimit/h2` (562950215319283) | OK / PASSING; ELIGIBLE | 0.0026 | @fbcode//mode/dev-asan | 200 results from 2026-09-22T14:42Z to 09-28T21:12Z: **all PASSED** (21 CONTINUOUS, 179 DIFF) | FLAKY issue 224798678 CLOSED (T278900793, completed 2026-07-10). FAILURE issue 244462068 CLOSED (T288811856, a "Widespread test failure" task, 2026-09-15/16, i.e. infrastructure). Journal: one failure on 2026-09-16, then StartedConsistentlyPassing on 2026-09-17. |
| thrift `fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest` (281475235299286) | OK / PASSING; ELIGIBLE | **0.1083** (hint: "None of the last 10 CI results failed, yet health metadata marks this test 10.8% flaky") | @fbcode//mode/dev-tsan | 194 results from 2026-09-21T23:17Z to 09-28T21:19Z: **all PASSED** (24 CONTINUOUS, 166 DIFF, 4 OTHER) | No TestX issues ("⚠ No test issues found"). Journal: consistently passing since 2025-11-18. |
| Hermit `fbcode//hermetic_infra/hermit/tests:hermit_run_default__custombin_minimal_hello - unmanaged` (562950400403548) | PROBLEMATIC / **DISABLED_FAILING** (53 consecutive trunk failures) | 0; brokenness 0.737 | @fbcode//mode/opt | last 10 CI results FAILED (2026-09-27T20:53Z to 09-28T21:38Z), all EPERM on Mount at `reverie-process/src/container.rs:842:26`; SANDCASTLE_JOB | FAILURE issue 246587564 OPEN (T290084710, owner newton; T290025391 widespread, closed). open_diffs D121815443. First seen on trunk 2026-09-24T22:14Z. |

Caveat on the asyncssl numbers: the 60% score and the 28% observed failure rate measure different things. The score is TestX's metric; 28% is the raw stress-run fraction in about 2.5 days of results. Both are reported. INFERENCE: the score likely weights some window or purposes differently.

---

## 3. My answers to the owner's questions

### 3.1 What does the big diff stack represent, how does it connect to old TFA, and what does it implement?

- **What it is (PROVEN):**
  - 22 unpublished, reviewer-free diffs, D121244476 to D121978137, created 2026-09-22 to 09-26. Together 36,416 changed lines (sum of `line_count`); the final tree is 33 files and 32,682 lines, 16,181 of them tests.
  - They add a local CLI, `tfa-phase2`, under `fbcode/testinfra/tfa` (BUCK `hub_python_binary`; `cli.metadata`).
  - It is a harness for exactly three C++ gtests that the owner's September 2026 dev-hermit campaign (`experiments/tfa-kernel71-*_20260921`) had already reproduced as PASS/FAIL under open-source Hermit chaos.
  - D121244213 alone (Codex sandbox and containment) is 20,138 changed lines, 55%.
- **Connection to old TFA (PROVEN):**
  - It shares the name and the technique: Hermit chaos seed pairs (PASS and FAIL) for flaky TestInfra TestIDs.
  - It shares no code, infrastructure, identity or data. The grep of the stack for old-TFA names found 0 files.
  - Old TFA (T106084722, from 2021-11) was a WWW Hack service with Skycastle workflows on Sandcastle, a web report UI and task comments.
  - Old TFA went to Test Infra in Dec 2022 (post 858293301884783). Its trigger was deleted on 2025-10-01 and its reports on 2026-09-10 (D118696067). Its rate-limit sitevar is 0.
  - The new stack posts under another team's bot ID, 4533519350210414 (AutoTestRemediation), not old TFA's own.
- **What it implements (PROVEN):**
  - Working offline: manifest, prompt and report validators; PASS/FAIL text-diff hints (`render-evidence`); a follow-up verdict re-judged from replay receipts (`render-followup`); `dry-run`, which for asyncssl lists the live task's comments and runs `--dry-run` comment creation.
  - Working on devbig030 only: `run-witness` with open-source Hermit 4630ee2a (D121978137).
  - Refuses: `run-seed` and `run-case` (internal Hermit BLOCKED), `run-agent` (Codex blocked), posting comments (always refused).
  - Present but unreachable: draft-diff creation (`jf submit --draft`).
  - The actual candidate fixes came from **outside** the stack: Claude investigator agents plus controller stress runs (RESULTS.md).

### 3.2 Should it become a committed pipeline? Where is it designed to run?

- **Where it is designed to run (PROVEN):**
  - A devserver: one operator, a local CLI, passwordless sudo, Btrfs, `systemd --user`, and chataccd for one test.
  - There is no Sandcastle, Chronos, Tupperware or fbpkg design.
  - In Sandcastle CI, 15 of 43 containment tests fail per mode (9 sudo mount, 5 Codex identity, 1 chataccd). All 30 of those signals are land-blocking and also fail on the base revision.
- **My answer: do not commit this code as a pipeline (INFERENCE, based on the proven facts).**
  - It is hard-wired to 3 hash-pinned cases and /home/newton paths.
  - It places its code and CI failures under the `testinfra` oncall without their agreement.
  - It uses another team's service user.
  - It depends on Codex versions that are blocked and on an internal Hermit that is BLOCKED.
  - 30 land-blocking warnings are open.
  - About 55% of its diff lines are Codex containment.
- **What would justify a committed pipeline:** a much smaller tool (§3.3 option B or C) owned by the hermit oncall. The hermit rotation is newton and jasonwhite (previous lane: `meta oncall.rotation schedule --rotation=hermit`; not re-verified).

### 3.3 Could a small TFA be revived as we test? How much needs repair or rebuild?

**What already exists (PROVEN):**
- (a) **Test lookup.** `meta testinfra.test metadata` returns `run_cmd` and `stress_run_cmd` for each TestID (mode, target and `--exact` filter). §2 shows these for all three tests.
- (b) **Hermit.** Open-source Hermit reproduces the three witnesses on devbig030 (row 21). rrnewton/hermit main contains the RLIMIT_CORE fix `ccbe37e9`.
- (c) **Race localization.** `hermit analyze --search`, `bisect` and `log-diff` exist in both trees (row 35).
- (d) **Evidence records.** The phase-one records are on dev-hermit main, and phase-two RESULTS.md is on devbig030.
- (e) **Scheduling pattern.** A Chronos job launches Sandcastle Claude jobs (`AutoTestRemediationScheduler.php:4-12`).
- (f) **Posting identity process.** The Service-users/creating-bots wiki describes it.

**What is missing:**
- Choosing test targets, beyond a hard-coded list.
- Hermit starting on Sandcastle: D121815443 is pending, and the userns changes are due mid-October.
- A qualified internal Hermit import (§0.1; PMU-skid blocker).
- An identity the owner's team owns for posting.
- Anyone who has agreed to host a FLAKY fixer strategy (testing_core).

Costed options. Every duration below is INFERENCE.

| Option | What it does | Must build | Where | Rough cost | Main risk |
|---|---|---|---|---|---|
| A. Re-verify the witnesses | Re-run the existing `tfa_phase2 run-witness --external-oss-hermit-record …` for 3 cases × PASS/FAIL | nothing | devbig030 only | hours; last run took 210 s for 8 steps (D121978137) | Host registered to davidai; httpcoro FAIL takes 108-114 s against a 120 s bound (RESULTS.md, D121978137) |
| B. Thin devserver wrapper, no posting | TestID → `meta testinfra.test metadata` → `buck build <mode> <target>` → `bin/safehermit <hermit> run --chaos --sched-seed k` over N seeds with the gtest filter → PASS/FAIL seed table → optional `hermit analyze` / `bisect` → report to `ai_docs/` or an issue | about 200-500 lines of script in dev-hermit; per-test mount and env recipe (copied from the `cases.v1.json` argv); an outer timeout around `analyze --search` (no internal cap) | any x86-64 devserver with ptrace, user namespaces and PMU; **no sudo needed** unless Btrfs quotas are used | days | Seeds move with paths and host (RESULTS.md §4). N seeds × up to about 2 min per run (httpcoro): 20 seeds ≈ 40 min for that test. |
| C. Scheduled service | B, plus Chronos → Sandcastle job, plus a hermit-owned service user with Limited Tasks Access and an act_as ACL | B, plus D121815443 landed, plus a refreshed and qualified internal Hermit import, plus a Sandcastle command and service user | Sandcastle | weeks | Userns rollout from mid-October; the PMU skid blocker (https://github.com/rrnewton/hermit/issues/1845); Reverie's timer test gets a zero count on some Sandcastle hosts (previous lane, test 562950403344809) |
| D. Plug into the central fixer | A FLAKY strategy with a Hermit reproduction step in TestFixerStrategyResolver | C's prerequisites, plus testing_core's agreement | Sandcastle | weeks or more | Every strategy today is FAILURE-only (row 33) |
| E. OnDemand | Same as B | same as B | Classic OnDemand has limited sudo (post https://fb.workplace.com/groups/devfeature/permalink/4319555484986547/). DSV2 OnDemand machines are devvms with sudo for Tier-1 FTEs (post https://fb.workplace.com/groups/1392872742159150/permalink/1685436742902747/). | same as B | Not verified here: whether DSV2 VMs expose PMU counters |

**Repair versus rebuild of the stack (INFERENCE):**
- Keep: hints.py (204 lines), the gtest oracles (`pipeline.py:119-342`, 224 lines), the follow-up verdict rules (`followup.py:199-338`, 140 lines), the three case records, and RESULTS.md.
- Rebuild: discovery, the build step, the Hermit invocation without byte pins, and the posting identity.
- Drop: the Codex containment (the 20,138-line D121244213 and its successors) and the sudo Btrfs-quota requirement.
- The stack already routes Hermit through `bin/safehermit` (argv[0] is required at `manifest.py:193`), which matches dev-hermit policy.

---

## 4. Decisions only the owner can make

1. Abandon the 22-diff stack (or mark it do-not-land), or keep it as an archive. Remove the `oncall("testinfra")` lines and the AutoTestRemediation bot ID 4533519350210414 either way.
2. Whether to send the asyncssl candidate patch to the owners of T255721720 (Service Encryption, gouthamarukonda). The evidence comes only from Hermit, and RESULTS.md's rank-3 hypothesis says the patch may **hide a production defect** in `AsyncSocket::handleRead`.
3. Whether any tool should post on tasks, and as whom. Options: a new hermit-owned service user, the central fixer's identity, or no posting (reports go to GitHub issues or `ai_docs/`).
4. Where any kept code lives: `fbcode/testinfra` (needs Test Infra's agreement), `fbcode/hermetic_infra`, or only dev-hermit on GitHub.
5. Whether to review and land D121815443, where the owner is a reviewer. It is the precondition for any Sandcastle option.
6. Which Hermit goes into fbsource. PR https://github.com/facebookexperimental/hermit/pull/93 at `4dcb34b4` lacks the RLIMIT_CORE fix; refreshing to current rrnewton main needs that pull request merged and a new CodeSync import.
7. Which host to use: keep relying on devbig030 (registered owner davidai), or move the witnesses to a host the owner controls and accept re-measuring the seeds.
8. Whether to ask testing_core for a FLAKY strategy (option D), or stay standalone (options B/C, against Test Infra's "recommend against").
9. Which agent, if any, does diagnosis: Codex (blocked), Claude (produced the current patches), or none (Hermit analyze output only).
10. How big the revival is: how many tests, seeds per test, and a wall-time budget.

## 5. Still open

- Does `hermit analyze --search` finish on these three gtest binaries under current open-source Hermit, and how long does it take? There is no 2026 record.
- Did D121815443's test-plan runs execute on Sandcastle hosts? The diff was generated by an automated test-fixer job run on Sandcastle (job 18014401479994438), so probably yes (INFERENCE). Trunk results will show it once it lands.
- The value TEST_FLAKINESS_ANALYZER_RATE_LIMIT held between 2022-11-23 and 2023-06-13; the history does not show values.
- 215/215 unit tests on devvm895: no log in the copied evidence.
