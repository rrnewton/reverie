# TFA phase-two prototype lane: what was built and every experiment it ran

Gatherer: sub-tfa2-030 "phase2", devbig030, 2026-09-28. Read-only; nothing was run except read-only
`meta phabricator.*` queries, `sha256sum`, `wc`, `grep`, `sl log`/`sl cat` in `/home/newton/fbsource`, and
three `gh` queries through `herdr-run`.

Path shorthands used below (all on devbig030 unless noted):

- `$T` = `/home/newton/work/tfa` (top level and the named subdirectories only)
- `$E3` = `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case`.
  This is the lane's main phase-two evidence tree, with `RESULTS.md`. Neither the previous agent nor the
  way-forward draft found it.
- `$EV` = `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-evidence`,
  the Codex diagnostic evidence tree. The previous agent and the way-forward also missed this one.
- `$SRC` = `/tmp/sub-tfa2-030/devvm895/tfa-src/fbcode/testinfra/tfa`, the stack-tip source tree copied from devvm895.
- `$DIAG` = `/tmp/sub-tfa2-030/devvm895/tfa-diag` (devvm895 scratch: `way-forward.md`, `containment-local.log`,
  `results-dev.json`).
- `$G` = `/tmp/sub-tfa2-030/gather`. It holds my raw query outputs: `files-D*.json` and `batch-describe.json`.
- "PDT" times come from file mtimes or in-file timestamps. Phabricator `created` values are EDT (-04:00),
  and I converted them to PDT.

---------------------------------------------------------------------------------------------------

## 0. Headline findings

1. **The stack.** It has 22 diffs, all Unpublished, with no reviewers, 0 comments and none landed. They
   touch 33 files: +34,549/-1,867 lines, net 32,682. The net count equals `wc -l` of the stack-tip tree
   exactly. Tests are net 16,181 lines (49.5%) and non-test net 16,501, of which 13,555 are product
   Python. D121244213 alone is 20,138 of the 36,416 changed lines (55%). Sources: `$G/files-*.json`,
   `$G/batch-describe.json`, `wc -l` on `$SRC`.
2. **The candidate patches did not come from the prototype.** The three candidate test fixes behind the
   SUPPORTED verdicts came from three **Claude** investigator agents (`claude-opus-5-5`, sessions
   `tfa-inv-<case>`). They ran on 2026-09-25 03:27-03:51 PDT, after Codex had been recorded as blocked.
   A lane-written shell controller built and replayed the patches (`$E3/stress/postfix_20260925.sh`,
   `$E3/witness/witness.sh`). This was not the prototype's `run-agent` or `run-case`. The prototype only
   re-judged the results offline (`render-followup`). Sources: `$E3/RESULTS.md` §2.2-2.3,
   `$E3/investigation/goal-text.txt`, `$E3/stress/postfix_20260925.sh`.
3. **asyncssl.** Seed 11 without the patch gave FAIL×5 and NONDETERMINISTIC×2. Two of the five FAILs are
   revert-control repeats, added after the first revert control came back NONDETERMINISTIC. With the patch,
   seed 11 gave PASS×3. Over the whole case, the unfixed ELF FAILed 5/14 runs; the patched ELF PASSed 8/9,
   with 1 OTHER at fresh seed 15, a SEGV in both phases. Source: re-counted from
   `$E3/stages/asyncssl/stage4-followup-hypothesis-r3/followup-runs.json`. Thrift: unfixed FAIL 5/17,
   patched PASS 13/13. httpcoro: unfixed FAIL 3/9 plus 2 CENSORED, patched PASS 8/8. All three verdicts
   are SUPPORTED, from 1-4 replays per seed. The patch files still exist at
   `$E3/investigation/<case>/agent-output/patch.diff`.
4. **Open-source Hermit 4630ee2a reproduced all 6 PASS/FAIL witnesses (3 cases × pass/fail).**
   - It did so in 5 complete witness batches under the prototype's `run-witness` (2026-09-25/26), plus the
     relocated re-measurement (12/12) and the stress replays.
   - The httpcoro FAIL witness runs 109-118 s against a 120 s wall bound. I counted 2 time-outs among 12
     seed-0 FAIL attempts: relocated v1 and the approved rerun 2 at the submitted head.
   - The final submitted runs still used `/tmp/tfa-phase2/...` for the guest cwd and the verify logs.
5. **The packaged (internal) Hermit never ran a witness.** All 18 witness calls in the three "three-case"
   runs refused at `PROVENANCE_MISMATCH` in 0-5 s. Eight packaged a919 re-qualification probes on 09-24
   all failed: RLIMIT_CORE with rc 1, 2 and 132, and a PMU skid panic with rc 1. Sources:
   `$T/three-case*/SUMMARY.txt`, `/tmp/tfa-phase2/requal-a919-*/result.json`.
6. **Missed Codex evidence.** An unsigned Codex 0.156.1 build with the two upstream fixes (sha256
   `f9228bec…`) **passed the four blocked real-Codex containment cases 4/4** in the v6 diagnostic
   (2026-09-24 23:30 PDT, 207.8 s). A 39-test run then gave 38/39, and the one failure passed on a rerun.
   Earlier attempts v1-v5 failed 4/4. The previous agent's reports and the way-forward omit this. Sources:
   `$EV/diagnostic/codex-meta0156-v6-diagnostic-receipt.json` and
   `$EV/OWNER-ONLY-BLOCKER-meta-codex-socket-isolation.md`.
7. **The Codex pins are stale again on devbig030.** D121978134 pinned launcher `d22c52fa…`, stable
   `4b776a3e…` and latest `d291f08a…`. Today devbig030 has launcher `2d4083cf…` (installed 2026-09-27
   23:30), stable `d291f08a…` and latest `7e520ac7…` (my `sha256sum`). The latest build is still from
   fbsource `24281a08` (2026-09-25 01:18 PDT). That is before the Btrfs fix `18cdc3ab303f`, so no
   installed or master-pointed Codex contains the fix. Sources: `sha256sum /usr/local/bin/codex_cli/*`,
   `sl cat -r master fbcode/3pai_tooling/codex/msdk/bin/codex_latest` in `/home/newton/fbsource` at master
   `9e6ec042287d` (2026-09-28 11:14).
8. **Mutation testing.**
   - The five review fixes on 09-25 got 49 mutants in total, and all 49 were killed (`$T/mutant-*.log`,
     `$T/mut-*.log`).
   - At the submitted head of D121978137, 67 mutants were killed and **5 survived**
     (`$T/external-oss-mode/mutants-head6/SUMMARY.txt`).
   - The lane's own note calls 2 of the 5 survivors a "real test gap". D121978137's test plan does not
     mention mutation at all.
9. **The RESULTS.md §10 reproducers ("runnable now") no longer run as written.**
   - The stress and investigator checkouts they use are empty. The directory mtimes are 2026-09-26 20:45
     and 21:15.
   - The witness runner pins `/home/newton/work/dev-hermit/bin/safehermit` at sha `a8a2965e…`. That file is
     now `fa2a29d7…`, changed on 2026-09-27; since commit `7b205bdd3`, safehermit no longer creates
     per-run Btrfs subvolumes.

---------------------------------------------------------------------------------------------------

## 1. The 22 diffs

Method:

- **Order.** Stack order comes from each diff's `depends_on` field in `$G/batch-describe.json`. I produced
  that file with `meta phabricator.diff batch-get --numbers=<22 diffs> --include=describe,ci-status -o json`.
- **Per-file counts.** These come from `meta phabricator.diff files --number D… -o json`, saved as
  `$G/files-D….json`.
- **"lines N".** The "lines N" in `/tmp/sub-tfa2-030/devvm895/tfa-src/STACK_SUMMARIES.md` equals + plus −
  for every diff.
- **Diff state.** All 22 are `Unpublished`, have no reviewers and 0 comments, and `is_landed=false`.

Paths are relative to `fbcode/testinfra/tfa/`. "T" = test lines, meaning files under `tests/`.

| # | Diff | Title | Files (count: names) | +/- (T) | What it adds |
|---|---|---|---|---|---|
| 1 | D121244476 (created 09-22 11:53 PDT) | Add bounded phase-two prototype | 19 new: BUCK, PACKAGE, hub.toml, README, cases.v1.json, report.schema.v1.json, case_checkout, comments, drafts, main, manifest, orchestrator, pipeline, process, prompts, report, state, tests/BUCK, tests/test_prototype | +5221/-0 (T +1494) | The base package: a manifest of the 3 cases, a 7-stage append-only FSM with leases, and draft-only Phabricator and dry-run task-comment gates. 54 unit tests. |
| 2 | D121244213 (09-22 11:52) | Use native Codex sandbox with TFA lifecycle control (title in the 09-25 jf log: "Bind fatal witnesses and SafeHermit evidence") | 24: new hermit.py, payload_wrapper, process_supervisor, review.py, review.schema, resources/safehermit, resources/bounded-run-space, tests/test_production_containment, tests/test_process_migration_helper; edits to 15 others | +18935/-1203 (T +9516/-636) | Runs Codex agents in TFA cgroup containment and adds the independent reviewer, safehermit packaging, Hermit witness replay and the first production-containment suite. Test plan: 76 unit tests, 11 containment tests. 55% of the stack. |
| 3 | D121649569 (09-24 11:45) | Close controller follow-up contracts | 2: pipeline, test_prototype | +1108/-49 (T +378/-4) | Fixes three gaps a real 09-23 run hit: Buck resources missing from the staged runtime (asyncssl), an under-specified httpcoro fatal transcript, and the thrift xplat mirror outside the commit seal. 114 unit tests. |
| 4 | D121848885 (09-25 12:13) | Make controller dependencies self-contained | 8: process, main, hermit, README, BUCK, test_prototype, test_production_containment, new hermit-import-compatibility.v1.json | +730/-6 (T +347) | Switches replay to the packaged fbsource Hermit (a919cce, recorded BLOCKED) and adds a Codex version-identity check that rejects blocked versions. 124 unit tests; containment 28/35. |
| 5 | D121848886 (09-25 12:13) | Close phase-two review findings | 12, incl. new hermit-import-receipts.v1.json | +1378/-191 (T +891/-139) | Adds import receipts, strict FullTrace for current Hermit, fail-closed readiness for unqualified Hermit and Codex, and durable fixture recovery. 127 unit; containment 36/40. |
| 6 | D121848888 (09-25 12:13) | Seal Codex incompatibility provenance | 5: process, README, 3 test files | +567/-59 (T +295/-28) | Binds the Codex launcher, manifest and channel into one identity. Records 0.156.0 and 0.156.1 as blocked on the Btrfs daemon-socket defect. 132 unit. |
| 7 | D121848889 (09-25 12:13) | Hash-bind the Codex payload and unify deadlines | 6: process, payload_wrapper, README, 3 tests | +1036/-92 (T +727/-15) | Adds a payload sha256 to the Codex compatibility key, O_NOFOLLOW exec through `/proc/self/fd`, and a single deadline origin. 157 unit; containment 36/40. |
| 8 | D121848890 (09-25 12:13) | Refuse empty wrapper argv; byte-bind Hermit test | 3: process, payload_wrapper, test_prototype | +127/-21 (T +115/-16) | Two small review fixes. 159 unit. |
| 9 | D121848892 (09-25 12:13) | Render deterministic difference hints and unposted initial comments | 5 incl. new hints.py | +431/-4 (T +192) | `render-evidence`: a PASS-vs-FAIL log text diff plus an unposted initial comment. 166 unit. |
| 10 | D121848893 (09-25 12:13) | Lead unposted initial comments with measured non-/tmp reproducers | 5 incl. new relocation.py | +624/-23 (T +257/-1) | The comment leads with re-measured commands outside /tmp (relocated-v2). 174 unit. |
| 11 | D121848894 (09-25 12:13) | Bind censored relocation attempts and resolve relocated paths | 4 | +222/-28 (T +136/-2) | Hardens the relocation checks (censored attempts, `..` and symlink escapes). |
| 12 | D121848895 (09-25 12:13) | Bind relocated safehermit reports to unique keys and the manifest Hermit | 3 | +44/-5 (T +34/-2) | Refuses repeated report keys and a foreign Hermit sha. 177 unit (parent gate). |
| 13 | D121848896 (09-25 12:13) | Render unposted candidate-fix follow-ups from re-judged witness replays | 5 incl. new followup.py | +658/-8 (T +286) | `render-followup`: re-judges the unfixed and patched replays and prints SUPPORTED, NOT SUPPORTED or INCONCLUSIVE. 187 unit. |
| 14 | D121848898 (09-25 12:13) | Bind the follow-up unfixed phase to the manifest test ELF | 3 | +21/-6 (T +17/-5) | Closes the phase-swap gap that a real-evidence negative control found. |
| 15 | D121848525 (09-25 12:11) | Render follow-up root causes as caveated hypotheses | 4 | +356/-7 (T +209/-1) | Presents the root cause as a ranked hypothesis and makes agent text inert. |
| 16 | D121877477 (09-25 15:23) | Hash-bind every follow-up artifact in the ledger | 6 | +472/-96 (T +192/-5) | Ledger v2 binds 8 artifact kinds by sha. 199 unit; 10/10 mutants killed. |
| 17 | D121877478 (09-25 15:23) | Bind follow-up replay roles to manifest seeds | 3 | +155/-13 (T +99) | Binds each role to its manifest seed. 202 unit; 9/9 and 3/3 mutants killed. |
| 18 | D121877479 (09-25 15:23) | Keep task comments dry-run-only | 5 | +139/-11 (T +98/-2) | Refuses every authorization proof (`UNTRUSTED_AUTHORIZATION`). 205 unit; 5/5 mutants killed. |
| 19 | D121877480 (09-25 15:23) | Present the final comment's cause as an unproven hypothesis | 4 | +68/-4 (T +48/-1) | Wording change. 206 unit; 7/7 mutants killed. |
| 20 | D121877481 (09-25 15:23) | Exercise the eligible Codex launch with a hash-bound fixture | 4 | +576/-12 (T +562/-8) | Adds a test-owned "eligible" Codex fixture. The real-release tests now skip on the reviewed blocked release. |
| 21 | D121978134 (09-26 09:20) | Pin the rolled-over owner-managed Codex releases as blocked | 4 | +24/-23 (T +11/-11) | Re-pins the new launcher and manifest digests, still blocked. |
| 22 | D121978137 (09-26 09:20) | Add a separately labelled external OSS Hermit prototype mode for witnesses | 4: hermit.py +406/-4, main.py +65/-2, README +33, test_prototype +1153 | +1657/-6 (T +1153) | `run-witness --external-oss-hermit-record`: replays the witnesses with the preserved open-source 4630ee2a binary. Unit 215; containment 39 pass + 4 skip. |

**Totals** (my sums over `$G/files-*.json`):

- **File touches.** 138 file-touches across 33 distinct paths.
- **Lines.** +34,549 / −1,867 = 36,416 changed lines, net 32,682. `wc -l` over every file in `$SRC` also
  gives 32,682, so the tree copy matches the stack tip.
- **Tests.** Paths under `tests/`: +17,057 / −876, net 16,181. That equals `wc -l $SRC/tests/*.py` (16,147)
  plus `tests/BUCK` (34).
- **Non-test.** +17,492 / −991, net 16,501. `wc -l` breaks it down as:
  - Python: 13,555. The largest files are pipeline.py 4,575, hermit.py 2,815 and process.py 1,631.
  - `resources/`: 1,766 (safehermit 1,452 and bounded-run-space 314). These are copies of dev-hermit
    scripts. The packaged safehermit is sha `a8a2965e…`; dev-hermit's current `bin/safehermit` is
    `fa2a29d7…`.
  - JSON: 688 (cases 427, compatibility 54, receipts 61, two schemas 146).
  - README: 357.
  - BUCK, PACKAGE and hub.toml: 135.
- **Test functions.** `grep -c 'def test_'`: test_prototype.py 215, test_production_containment.py 43,
  test_process_migration_helper.py 0 (a helper module).

**Feature versus hardening** (my classification from the summaries above):

- 6 diffs add user-visible capability: #1, #2 (mixed), #9, #10, #13 and #22.
- 1 is a test fixture (#20) and 1 is a pin update (#21).
- The other 14 are hardening, binding, refusal or wording.

The way-forward's "about 5 feature / 17 hardening" is close.

**CI** (from `$G/batch-describe.json`, `ci_status`):

- Every diff shows WARNING except two ancestors, which show **FAILED**: D121244213 (28 failed of 300
  test signals) and D121848886 (32 failed of 342).
- The tip D121978137 has 559 signals, 0 failed and 30 warnings.
- 2026-09-25 snapshots are in `$T/stack-ci-1..8.txt`, taken 15:24-16:26 PDT. In them, D121244213 is
  FAILED 28, D121848886 FAILED 16 with 17 pending, the rest are WARNING with 28-34 warnings, and
  D121877481 briefly showed FAILED on `fbsource-target-determinator`.

---------------------------------------------------------------------------------------------------

## 2. What the prototype is designed to do, and what actually works

**Designed flow** (`$SRC/state.py:21-28`, `Stage` enum; driven by `$SRC/pipeline.py:4464-4575` `run_case`):

1. VALIDATED_REPRO: `validate_witness_artifacts` checks and seals the recorded PASS and FAIL witnesses
   (pipeline.py:4486-4492).
2. INITIAL_COMMENT: `_comment_stage` posts, or dry-runs, the initial task comment (pipeline.py:4496-4504).
3. INVESTIGATING: a checkout preflight, then a Codex diagnostic agent proposes hypotheses and a patch, and
   an independent Codex reviewer checks it (`_diagnose_candidate_locked`, pipeline.py:4505-4524).
4. FIX_DRAFTED: `independent_validate` rebuilds, replays the PASS/FAIL seeds and fresh seeds under Hermit,
   then produces a qualified report (pipeline.py:4525-4538).
5. VERIFIED: stress counts are recorded (pipeline.py:4539-4547).
6. DIFF_DRAFTED: `_draft` creates a draft Phabricator diff with readback (pipeline.py:4548-4557; drafts.py
   uses `jf submit --draft`, drafts.py:74, 404).
7. FINAL_COMMENT: the final comment (pipeline.py:4558-4571).

**Other CLI commands** (`$SRC/main.py`): `validate-manifest` (:89), `render-prompt` (:96),
`render-evidence` (:108), `render-followup` (:139), `validate-report` (:166), `dry-run` (:182),
`run-witness` (:191), `run-seed` (:266), `run-agent` (:305) and `run-case` (:345).

**Hard-wiring:**

- `$SRC/manifest.py:249-277` requires exactly three cases, with IDs {httpcoro, thrift, asyncssl}. Only
  asyncssl may have a live task, `T255721720` (cases.v1.json:287-288).
- It also requires canonical per-case sha256 values (manifest.py:36+, 270-276).
- The expected guest working directories are `/tmp/tfa-phase2/cwd/<case>` (manifest.py:18-22).
- The authoritative record URLs point at dev-hermit commit `edd96831…` experiments (manifest.py:14-35).
- `argv[0]` must be `/home/newton/work/dev-hermit/bin/safehermit` (manifest.py:193).
- Lock roots live under `/tmp/tfa-phase2/` (state.py:32-33).

**Status of each piece at the stack tip:**

| Piece | Status at tip | Evidence |
|---|---|---|
| validate-manifest | Works (3 cases), in every recorded run | `$T/three-case*/validate.log` |
| dry-run | Works. For asyncssl it runs `meta tasks.comment list` on T255721720, then `tasks.comment create --dry-run` as service user `4533519350210414` (orchestrator.py:48-80; comments.py:21). One run failed on a transient token.service `APP_QUEUE_TIMEOUT` and passed on retry. | `$T/external-hermit/witnesses-2b5edff7bfde-fixture/SUMMARY.txt` |
| render-evidence (hints.py:172 `difference_hints`) | Works offline on records produced elsewhere | D121848892 test plan; `$E3/stages/<case>/stage1-2*` |
| render-followup (followup.py:426 `validated_followup`, :301 `_verdict`) | Works offline on ledgers written by the lane's own controller scripts | `$E3/stages/*/stage4-followup-hypothesis-r3/render.out` (SUPPORTED ×3) |
| run-witness, packaged Hermit | **Refuses.** `packaged_hermit_argv` (hermit.py:1683) raises `HermitError` at PROVENANCE_MISMATCH, BLOCKED or UNASSESSED_VARIANT (hermit.py:1769-1828). The packaged a919 digest recorded is `9bf7064a…`, but the built one is `4e8e4381…`. | `$T/three-case*/SUMMARY.txt`; `$SRC/hermit-import-compatibility.v1.json` |
| run-witness, external OSS mode (hermit.py:1925 `external_oss_hermit_argv`) | **Works on devbig030 only.** It needs the preserved binary at `/home/newton/work/dev-hermit/ignored/…/hermit-4630ee2a` (still present, sha `5c422b85…` re-verified today), the ELFs in `/home/newton/work/tfa/fbsource/buck-out`, and a sudo loop-mounted Btrfs fixture. | §3.11 |
| run-seed, run-case | **Refuse.** Both go through `_execution_manifest`, which calls `packaged_hermit_argv` (main.py:41-64, 282, 374). | code |
| run-agent, Codex review | **Refuses.** `_CODEX_COMPATIBILITY` (process.py:104-121) has 2 entries, both `blocked`, with no payload digest. On devbig030 today the launcher and manifests no longer match those keys, so the refusal would be "release manifest identity differs" (inference; I did not run it). | code; §4 |
| Task comments | Never post. `assert_bot_authorization` always ends in `raise CommentError(UNTRUSTED_AUTHORIZATION)` (comments.py:287). `run-case` only runs `--dry-run` (D121877479). | code |
| Draft diff | Unreachable at the tip, because Hermit and Codex are both blocked upstream of it. | — |
| Test discovery and scheduling | Absent (README.md:3-4). | code |

**Correction to "the full flow ran only against mocks":**

- `/tmp/tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md` (mtime 09-24 05:55) audits a real
  controller run at fbsource `50a9ce52…`. That run preceded D121848885's switch to packaged Hermit.
- For **thrift** it reached a validation seal, a qualified report and a local commit `2f755a3be882…` of a
  candidate fix, with the xplat mirror swept in by the commit hook.
- For **asyncssl** it reached a staged validation attempt at seed 11, which failed because the staged
  runtime lacked Buck resource files.
- For **httpcoro** it stopped at a transcript check that was too strict.
- The audit lists sha256 values for these artifacts, but the artifacts themselves are gone. Only the
  audit file survives.
- Conclusion: the real pipeline did run past INVESTIGATING at least once, on 2026-09-23. The audit does
  not name the agent; Codex was the only agent path wired in then (inference). D121649569's summary
  confirms it: "gaps at the boundary between a qualified repair and a draft".

---------------------------------------------------------------------------------------------------

## 3. Every experiment the lane ran (chronological)

### 3.1 2026-09-23: first real controller run (audit only)
- **Source:** `/tmp/tfa-phase2/goal-20260923.CQi0pX/controller-followup-audit.md`.
- **Result:** the audit lists three defects, which D121649569 then fixed:
  - httpcoro's fatal transcript check counted a bare substring;
  - thrift's commit swept in the xplat mirror;
  - asyncssl's staged image lacked the `.resources.json` file and certificates, so the TLS setup failed
    and the candidate was never exercised.
- **Evidence:** the audit records sha256 values only. The underlying artifacts were not found.

### 3.2 2026-09-24 06:41-06:46 PDT: controller follow-up real checks (D121649569 era)
- **Commands:** driver scripts; logs are in `/tmp/tfa-phase2-controller-followup-*.log` and the evidence in
  `/tmp/tfa-phase2-controller-followup-evidence-20260924/asyncssl-real/`.
- **asyncssl seed 11:** rc 1, `[ FAILED ]`, 4.53 s (`…-asyncssl-seed11-driver.log`,
  `…-real-asyncssl-verify.log`).
- **asyncssl seed 1:** rc 0, `[ OK ]`, 6.75 s.
- **asyncssl stress seeds 12, 13, 14:** rc 0 (4.5-5.1 s).
- **asyncssl seed 15:** rc 1 (8.46 s). The stress verify step then raised "selected gtest did not execute
  exactly once" (`…-asyncssl-stress-verify.log`).
- **httpcoro:** roles `['fatal-header','normal-frame','async-frame']`, rc 134, not timed out
  (`…-real-httpcoro.log`).

### 3.3 2026-09-24 13:40-13:46 PDT: packaged a919 Hermit re-qualification
- **Source:** 8 probe directories, `/tmp/tfa-phase2/requal-a919-*/result.json` and `stderr.txt`.
- **asyncssl pass seed 1, 6 variants:** rc 2 once and rc 1 five times, in 1.1-1.4 s. `setrlimit(RLIMIT_CORE`
  appears in stderr.
- **httpcoro pass seed 1:** rc 1 in 2.9 s, "Clock perf counter exceeds target value".
- **thrift pass seed 0:** rc 132 in 3.8 s, RLIMIT_CORE.
- **Conclusion:** 0 of 8 probes passed. This backs `hermit-import-compatibility.v1.json` (qualification
  probe `d24fe3a2…` BLOCKED; packaged `9bf7064a…` BLOCKED by `--verify-verbose /bin/true` TRACE drift).

### 3.4 2026-09-24 15:09-18:05 PDT: unit and production-containment runs
- **Source:** `/tmp/tfa-phase2/*.out`.
- **Unit:** 125/127 twice (`unit-test.out`, `unit-exact-final.out`), then 132/132 at `68cf2bac`
  (`unit-68cf2bac-recheck.out`, 18:05).
- **Containment:** 34/39, 35/39, 34/39, 35/39, then 36/40 at `68cf2bac` (18:04). The failures were real-Codex
  preflight: "Codex native workspace sandbox preflight failed (… codex-cli 0.156.0; state=unsupported)"
  (`/tmp/tfa-phase2-production-final.out`).

### 3.5 2026-09-24: Codex diagnostics (see also §4)

**Codex 71b8f5 review** (`/tmp/tfa-phase2/codex-71b8f5-qualification-review.md`, 16:03):

- The launcher sha `71b8f5bd…` (codex-cli 0.156.0) is NOT QUALIFIED.
- The native sandbox probe reported "cannot establish app-server socket mount isolation" and the tool never
  ran.
- The review names the prior identity `8ec5b757…` as already blocked with the same message.

**Signed-Codex qualification runs** (`/tmp/tfa-phase2-codex-qualification.out` and the
`-latest-qualification*.out` files, 16:55-17:04):

- Three runs of the 4 real-Codex cases; none passed. Run 1: 4 failures in 206.7 s. Run 2: 3 failures and
  1 error in 51.5 s. Run 3: 4 failures in 172.6 s.
- Six assertions report an empty 48-hex nonce, so no tool executed.

**Local upstream Codex build at `a708fc68`** (`/tmp/tfa-phase2/codex-a708-*`, also copied to `$EV/diagnostic/`):

- Built with cargo: `codex-cli 0.0.0`, sha `4378ab0c…`, 1.39 GB.
- The four real-Codex cases **failed 4/4** in 142.1 s at 21:35. Codex exited 1 with "Reading additional
  input from stdin…", and one case failed its checkout preflight.
- Receipt: `codex-a708-diagnostic-receipt.json` (`successful: false`, `production_eligible: false`).

**Unsigned Meta-downstream candidate**, v1-v8b (`$EV/diagnostic/codex-meta0156*-receipt.json`; the
candidate is described in `$EV/meta-build/CANDIDATE.md`):

- It is fbsource `third-party/codex` at `f3319051bb78` (0.156.1) with upstream `7cec3e110ce6` and
  `a708fc683934` applied. sha `f9228bec…`, not signed, never marked eligible.

| Run | Time (PDT) | Result |
|---|---|---|
| v1, v2, v3, v5 | 22:37, 22:45, 23:04, 23:24 | 4/4 fail (99.4 s, 201.2 s, 239.0 s, 240.8 s). The v1 cause was a missing `codex-code-mode-host` sibling (`$EV/diagnostic/ROOT-CAUSE-meta0156-v1.md`). |
| v4 (signed latest, control) | 23:12 | 4/4 fail |
| **v6** | 23:30 | **4/4 pass**, 207.8 s |
| v7 (40-case, fbpython from source) | 23:41 | 25/40; ruled non-authoritative: 14 failures were the missing `hermit-test-bin` fixture and 1 was the inverted gate test |
| v8 (Buck link tree) | 23:55 | 38/39 |
| v8b | 23:58 | 1/1 rerun of v8's single failure (a missing PYTHONPATH in a non-Codex test) |

- **Root cause** (per `$EV/OWNER-ONLY-BLOCKER-…md`): devbig030's `/tmp` is the Btrfs subvolume `/tmpvol`.
  mountinfo reports device 0:42 while `st_dev` is 0:49. The pre-fix `daemon_mounts.rs::check_mounts`
  requires the two to be equal.
- **Coordinator's reading:** `/home/newton/work/dev-hermit/ignored/coord-tfa-v7-interpretation-20260925.md`
  says "the V6 four-case discriminator remains the clean proof that the two Codex fixes remove the four
  signed-package containment failures".

### 3.6 2026-09-25 01:38-07:28 PDT: the three-case evidence tree `$E3` (RESULTS.md, reviewed "approve")

All runs used Hermit `4630ee2a` (sha `5c422b85…`) under safehermit with `--sh-deadline 120` and flags
`--backend ptrace --strict --chaos --chaos-per-thread-slowdown --verify --verify-strict --verify-allow both`
(RESULTS.md §2.3).

**Relocated reproducers** (runner `$E3/relocated*/run_relocated*.py`):

- **v1** (01:58-02:01, `$E3/relocated/run.out`):
  - thrift: 4/4 matched the oracle (seed 1 FAIL 15.8 s and 15.0 s; seed 0 PASS 14.8 s and 23.6 s).
  - asyncssl: 4/4 (seed 11 FAIL 4.9 s and 4.1 s; seed 1 PASS 4.0 s ×2).
  - httpcoro: seed 0 rc 124 **CENSORED at 121.1 s**. The runner then crashed with "guest cwd is not empty".
- **v2** (02:07-02:13, `$E3/relocated-v2/run.out`): **12/12**, every witness at its manifest seed with
  identical stdout.
  - thrift: FAIL seed 1 (15.7 s, 15.1 s), PASS seed 0 (14.6 s, 14.0 s).
  - asyncssl: FAIL seed 11 (4.4 s, 4.1 s), PASS seed 1 (4.2 s ×2).
  - httpcoro: FAIL seed 0, rc 134 (114.4 s, 112.7 s); PASS seed 1 (10.4 s ×2).
  - The cwds sit under `/home/newton/work/tfa/phase2-cases/<case>/tfa-phase2-repro/`. Only thrift's still
    exists.

**Investigators** (03:27-03:51):

- Three Claude agents (`claude-opus-5-5`) were launched through agentctl (`$E3/investigation/<case>/start.out`,
  `goal.out`). For all three cases, both the start and the goal deliveries were recorded as
  `possibly_submitted` (ambiguous).
- Goal text: "…never use /tmp…". The agents were not allowed to build.
- Each wrote 4 ranked hypotheses and selected rank 1.
- Candidate patches, all test-only (sha256 from `sha256sum $E3/investigation/*/agent-output/patch.diff`):
  - asyncssl: `dc77e591…`, 831 bytes, 6 added lines (an untimestamped `writeAndReflect` drain).
  - thrift: `61dd5a10…`, 851 bytes.
  - httpcoro: `3806affc…`, 929 bytes.
- The reports encode NOT RUN validations as "FAIL 0/0" (RESULTS.md §2.2).

**Controller stress** (03:39-04:14):

- `$E3/stress/postfix_20260925.sh` does `sl import` → buck build → replay → native `buck test` + `arc lint`
  → revert → rebuild → revert-control replay.
- On-disk receipts: thrift 31 (one of them an excluded smoke run), asyncssl 23, httpcoro 17 (plus 1
  interrupted attempt without a receipt). That is 71 Hermit replays, ~1,379 s of summed guest wall time
  (507 + 118 + 754 s), per the `duration_seconds` fields in `$E3/stress/*/*/*/receipt.json`.
- I cross-checked the ledger run list against the receipts: every ledger run has a receipt. The only extras
  are the thrift smoke run and the httpcoro interrupted directory.

**Follow-up verdicts.** I re-counted these from the r3 `followup-runs.json` files; they match RESULTS.md §4.

| Case | Unfixed runs | Patched runs | Verdict |
|---|---|---|---|
| thrift | 17: FAIL 5 (seed 3 ×4, seed 8 ×1), PASS 12. The manifest FAIL seed 1 **PASSed** at the stress path, so FAIL seeds had to be searched for (1 FAIL in seeds 6-11). | 13, all PASS | SUPPORTED |
| asyncssl | 14: seed 11 FAIL 5 + NONDET 2 (3 FAILs from prefix/prefix-extra, 2 from the revert-control repeats); PASS 5; OTHER 2 (seed 15) | 9: PASS 8 (seed 11 ×3, seed 1 ×2, seeds 12-14), OTHER 1 (seed 15) | SUPPORTED |
| httpcoro | 9: FAIL 3 (seed 0: 117.6 s, 117.5 s, and the revert control at 117.9 s), PASS 4, CENSORED 2 (fresh seeds 5 and 6 at 121.1 s); plus 1 interrupted | 8, all PASS | SUPPORTED |

**Other outcomes:**

- **Native patched `buck test`:** rc 0, Pass 1, for each case (e.g. `$E3/stress/asyncssl/postfix/unit.out`).
  n = 1, with no unfixed native comparison (RESULTS.md §7).
- **arc lint:** rc 0.
- **Validator negative controls** (`$E3/gates/followup-20260925-r7-c1f8817a5ebb/SUMMARY.md`):
  - 9 mutations of followup.py were each caught (rc 1).
  - Two real-ledger negative controls (swapped phases; patched sha equal to unfixed) were refused at r7.
    The swap had been **accepted** at r6, which is why D121848898 exists.
- **Containment during these gates:**
  - r7 run 1: 34/40 (the 4 Codex failures plus `hg_monitor` and a reverie PMU skid, which passed 2/2 on
    rerun).
  - r7 run 2: 36/40.
  - r8 at `b8b85d00`: 36/40.

### 3.7 2026-09-25 13:43-15:07 PDT: mutation testing of the five exact-head review fixes

- **Command:** `$T/mutants.py SPEC.json PREFIX`. It applies one textual mutation, runs the focused
  `buck2 test … -- --regex`, and calls the mutant KILLED on a nonzero rc.
- **Specs:** `$T/mutants-*.json`. Logs: `$T/mutant-*.log` and `$T/mut-*.log`, 49 in total. I classified
  each log from its `Tests finished` line.
- **Result by finding:**
  - f1 (ledger hash-binding): 10
  - f2 (role/seed binding): 9. The extra `judge-before-bind` is not in mutants-f2.json.
  - f2b: 3
  - f3 (dry-run comments): 5
  - f4 (hypothesis wording): 7
  - f5 (eligible Codex fixture): 7
  - f5b: 4
  - f5c: 4
  - **49/49 KILLED**, 0 survived.
- **Suite progression over this window:**
  - `tfa_phase2_test`: 111 → 199 → 202 → 205 → 206 (`$T/d213-phase2-test.log` and
    `$T/finding{1..4}-phase2-test.log`, 12-16 s each).
  - Full `buck test testinfra/tfa/...` at 14:25: Pass 250, Fail 1 (`test_real_codex_bootstraps…`: "Codex
    latest release manifest identity differs"), Omit 1 (`$T/full-test.log`). That failure is the first
    sign of the host Codex rollover, at 14:18 PDT (`$T/f5-containment.log`).
  - `$T/head2/full-test.log` (head 0a413f6c): Pass 247, Fail 5. `$T/head3/full-test.log` (head c9ef1aa9):
    Pass 248, Fail 5. All 5 are real-Codex failures.
- `$T/d569-phase2-test.log` was killed by SIGTERM (no result).

**codex-rollover mutants** (`$T/codex-rollover/mutants.out`, `mutants-real.out`, 20:00-20:09):

- 4 distinct mutants. 3 were killed by the unit regex.
- `stable-manifest-digest-flip` SURVIVED the unit regex (Pass 20) and was killed only by the 308 s
  real-host containment run.
- D121978134's test plan counts this as "5/5 killed" (5 runs); it does disclose the survival.

**external-oss-mode mutants** (`$T/external-oss-mode/mutants*/SUMMARY.txt`). The mapping of directories to
heads is my inference from mtimes and the gate scripts.

| Directory | Time (09-25/26) | Killed | Survived |
|---|---|---|---|
| mutants | 21:36 | 17 | 4 |
| mutants-133076a2d046 | 21:50 | 21 | 7 |
| mutants-head2 | 22:02 | 29 | 6 |
| mutants-head4 | 23:32 | 50 | 6 |
| mutants-head5 | 23:59 | 65 | 2 |
| **mutants-head6 = submitted head 4fb8c4120739** | 00:16 | **67** | **5** |

- The head6 survivors are `safehermit-failure-no-signal`, `unit-core-dump-any-failure`,
  `attempt-safehermit-digest`, `reseal-metadata-file` and `reseal-identity`.
- The lane's own note: `$T/external-oss-mode/DISK-HOLD-4fb8c4120739.txt` says the first three are "outer-gate
  relaxations still refused by safehermit_evidence" and the last two are a "real test gap: no subtest mutates
  metadata or pin identity during the comparator".
- D121978137's test plan does not mention mutation testing.

### 3.8 2026-09-25: "three-case" wrapper runs (the prototype's own witness path)

| Head | Time (PDT) | validate | dry-run | Witnesses |
|---|---|---|---|---|
| 5bc4f555dd93 | 14:22 | rc 0 | rc 0 | 6/6 rc 1 in ~1 s: `HermitError: packaged Hermit compatibility is PROVENANCE_MISMATCH` |
| 0a413f6c | 14:45 | rc 0 | rc 0 | same, 0-1 s |
| b6b8f7a16a8a | 19:44 | rc 0 | rc 0 | same, 2-5 s |

- Sources: `$T/three-case/SUMMARY.txt`, `$T/three-case-0a413f6c/SUMMARY.txt`,
  `$T/three-case-b6b8f7a16a8a/SUMMARY.txt`. The command is recorded in
  `$T/three-case-b6b8f7a16a8a/run.sh`, e.g.
  `buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest … --case-id C --outcome O --artifact-root …`.
- The message in every case: expected rev `4630ee2a…` sha `5c422b85…`; packaged rev `a919cce…` sha
  `4e8e4381…`; qualification `d24fe3a2…` BLOCKED.
- That makes 18 refusals in total. **None ran Hermit.**

### 3.9 2026-09-25 evening: audits
- **`$T/codex-drift/AUDIT.md` (19:27).**
  - Rollover: launcher `71b8f5bd…` → `d22c52fa…`; stable `4b776a3e…` (0.156.1); latest `d291f08a…`
    (0.156.1, built from fbsource `6be63a60915d`, 2026-09-24 10:17).
  - Fix status: `de089a75d6a1` (0.157.0 import, D121728319) landed 09-25 10:26 and `18cdc3ab303f` (Btrfs
    mismatch, D121708813) landed 16:57. Neither is in any installed build.
  - The first `codex --version` attempt failed on a DotSlash CAS fetch with "tlsv13 alert certificate
    required" (`version-latest.err`). The retry gave rc 0.
- **`$T/codex-rollover/`** (19:29-20:10) is D121978134's evidence:
  - focused unit tests: Pass 20;
  - real-host containment: Pass 4, Skip 4;
  - full `testinfra/tfa/...`: Pass 249, Fail 0, Skip 4, Omit 1, rc 64, 494 s (`full-test.log`).
- **`$T/hermit-provenance/AUDIT.md` (20:47).** Verdict: an owner-only blocker. Re-pinning would only turn
  PROVENANCE_MISMATCH into BLOCKED or WITNESS_MISMATCH.
  - fbsource Hermit still returns EPERM for `prlimit64` RLIMIT_CORE (sysinfo.rs:76-78). I re-checked
    today: `validate_resource_limit_mutation` has 0 occurrences in master's sysinfo.rs.
  - The fixes are in rrnewton/hermit: `ccbe37e91`, on hermit origin/main (see
    https://github.com/rrnewton/hermit/pull/3142), plus `b38586bd6` and `02d978736`.
  - `4630ee2a` sits only on `origin/salvage/devbig014/hermit-tfa-prlimit-fix/hermit-1-4630ee2a61f9` and does
    not contain `ccbe37e91` (`$T/external-hermit/source-provenance.txt`).
- **`$T/sudo-fixture/AUDIT.md` (19:50).** The containment tests need a non-root user who has passwordless
  sudo. `bounded-run-space` itself calls `sudo -n btrfs …`, and exit 10 means UNBOUNDED. Sandcastle's
  default user has no sudo, and its sudo lane runs as root, which the supervisor refuses. The verdict is
  that no TFA-side fix exists.

### 3.10 2026-09-25 20:53 → 2026-09-26 04:55: import watcher
- `$T/import-watcher/check.py` runs `sl pull -B master`, `sl cat` and an ancestry revset every ~30 min.
- `watch.jsonl` has **15 rows, all `ready: false`**:
  - `codex_latest_commit_hash` is always `24281a08…` (`codex_contains_18cdc3ab: false`);
  - `hermit_rlimit_core_fix_imported: false`.
- Recording stops at 04:55. The PID file says `watcher pid 2847162`.

### 3.11 2026-09-25 20:13 → 2026-09-26 09:16: open-source Hermit mode (D121978137)

**Protocol.** `$T/external-hermit/PROTOCOL.md` was declared before any probe. It allows one attempt per
probe, with no retry and no seed replacement. Amendments 1-3 record harness defects:

- the bounded-run-space digest (qual-1);
- inherited stdin, which left every guest stuck behind `--verify`'s stdin buffering (qual-2);
- missing Buck shared-libs symlink trees; the fix was a rebuild at `7f239d5b` with byte-identical ELFs
  (qual-3).

**Qualification** (`$T/external-hermit/qual-{1..4}.out`):

| Attempt | Time (09-25) | State | Detail |
|---|---|---|---|
| qual-1 | 20:16 | BLOCKED | 0 probes; bounded-run-space digest |
| qual-2 | 20:27 | BLOCKED | all 4 probes rc 124 at 121.7-123.6 s (harness stdin defect) |
| qual-3 | 20:30 | BLOCKED | `/bin/true` rc 0 in 1.3 s; the 3 cases rc 127 in 2.7-3.5 s (missing runtime), although verify had reported deterministic |
| **qual-4** | 20:37 | **QUALIFIED** | `/bin/true` 1.7 s, httpcoro 37.5 s, thrift 15.7 s, asyncssl 6.1 s. Record sha `5041afd1…` |

**Witness batches.** Each has 6 `run-witness` calls plus validate and dry-run. Sources:
`$T/external-hermit/witnesses-*/SUMMARY.txt` and `NOTES.md`. The command is in each `run.sh`:
`buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness … --external-oss-hermit-record $R --external-oss-hermit-record-sha256 $RS`.
The Btrfs fixture is created by `with_fixture.py`.

| Batch (head) | Time (PDT) | Outcome |
|---|---|---|
| 1667ce0d7600, no fixture | 09-25 20:46-20:49 | 6/6 rc 1, "bounded verification space was not applied" |
| 1667ce0d7600-fixture ("batch 2") | 20:52-21:00 | **Invalid.** The code translated the argv to a919's `--verify-verbose` (FullTrace), so all 5 completed runs were judged nondeterministic, and httpcoro-fail hit safehermit exit 124 (137 s). Fixed in the next head. |
| 55f6e37a4a63 (batch 3) | 21:05-21:09 | 6/6 expected outcomes: asyncssl 1/0, httpcoro 134/0, thrift 1/0; BitwiseInfoV1 deterministic. httpcoro-fail CLI 125 s. |
| 418be8592ff2 (batch 4) | 21:28-21:32 | 6/6. Hermit time for httpcoro-fail 114.7 s; the other 5 took 5.2-18.4 s. |
| dc3528a0d1ad (batch 5) | 22:15-22:20 | 6/6. httpcoro-fail CLI 132 s. |
| 2b5edff7bfde (batch 6) | 22:47-22:52 | 6/6 under a stricter evidence gate. httpcoro-fail CLI 126 s. dry-run rc 1 (transient token.service APP_QUEUE_TIMEOUT); retry rc 0. |
| 134b055a02c3, 8cdf04340a0e | 23:10, 23:49 | Prepared (`run.sh` only) and never run. At 8cdf the gates stopped as superseded. |
| 4fb8c4120739, resume run 1 | 09-26 08:16 | Containment rc 64, Pass 39, Skip 4, 478 s. The runner did not yet accept rc 64, so it stopped fail-closed with no witnesses (`DISK-HOLD-4fb8c4120739.txt`). |
| 4fb8c4120739, rerun 2 | 08:51-08:59 | Containment rc 64 verified (39/4, 313 s). Witnesses: asyncssl fail/pass OK, then **httpcoro-fail hit the 120 s safehermit wall bound** (`unit_result=timeout`, exit 124, CLI 128 s). The batch stopped with 3 of 6 witnesses run (`rerun2-failed-4fb8c4120739/README.txt`). |
| **4fb8c4120739, rerun 3** | 09:06-09:15 | Containment rc 64 verified (Pass 39, Skip 4, 316 s, trace `b4a124e7…`). Witnesses 8/8 rc 0 in 210 s. Per-witness rc/duration, from the JSON line in each `rerun3-4fb8c4120739/witness/witness-*.log`: asyncssl fail 1/4.5 s (seed 11), pass 0/4.4 s (seed 1); httpcoro fail 134/109.2 s (seed 0), pass 0/11.1 s (seed 1); thrift fail 1/14.9 s (seed 1), pass 0/14.5 s (seed 0). PASS marker sha `a25299b4…`. |

Then D121978134 and D121978137 were created with `jf submit --stack --draft` at 09:21
(`$T/external-oss-mode/submit.out`, `RUNNER-EVIDENCE-4fb8c4120739.txt`).

**Answer to "did open-source Hermit reproduce all 6 witnesses?"** Yes.

- It produced the expected FAIL and PASS for all 3 cases in 5 complete batches (30/30 witness runs).
- It also did so in relocated-v2 (12/12) and in the stress unfixed-phase witnesses.
- Caveats:
  - The runs use the manifest's `/tmp/tfa-phase2/cwd/<case>` guest cwd and a verify-log dir inside a
    `/tmp/tfa-phase2/tfa-production-btrfs-*` fixture (argv in the rerun 3 logs).
  - The httpcoro FAIL witness sits 2-11 s under the 120 s bound. My tally of seed-0 FAIL attempts under
    4630ee2a with the valid contract comes to 12: relocated v1 ×1, relocated v2 ×2, stress ×3,
    batches 3-6 ×4, rerun 2, rerun 3. Two of them timed out (relocated v1 at 121.1 s and rerun 2), 10
    FAILed with rc 134, and 1 more attempt was interrupted.

**Disk incident** (`$T/external-oss-mode/DISK-HOLD-4fb8c4120739.txt`):

- `/dev/md0` fell to 583 MB and then 378 MB free at 01:19-01:20 PDT on 09-26.
- The coordinator SIGSTOPped the TFA gate pipeline.
- The concurrent root devbackup `tar` (pid 3612085) was verified as not TFA.
- The launch resumed at 08:16 with a 100 GiB free-space floor.

### 3.12 2026-09-28 11:15 PDT: devvm895 re-check by the previous agent
- **Containment on devvm895:** Pass 37, Fail 6 (`$DIAG/containment-local.log`):
  - 5 real-Codex tests failed with "release manifest identity differs";
  - `test_current_full_trace_refuses_trace_drift_and_cleans_up` hit ENOSPC inside the quota fixture (per
    the seq 1885 report).
- **Sandcastle CI** (run `1125900439774673`, `$DIAG/results-dev.json`): 249 passed, 15 failed. I classified
  the 15 failures from `test_details.details`:
  - 9 × `sudo -n mount` denied;
  - 5 × Codex "release manifest identity differs";
  - 1 × `chataccd-mount@….service not found` (`test_killed_worktree_body_is_reconciled_by_independent_owner`).

---------------------------------------------------------------------------------------------------

## 4. The Codex dependency

**Identities seen and pinned.** Sources: `$SRC/process.py:60-121`, `/tmp/tfa-phase2/codex-71b8f5-qualification-review.md`,
`$EV/OWNER-ONLY-BLOCKER-…md`, `$T/codex-drift/AUDIT.md`, and my `sha256sum` today.

| When | Launcher | stable | latest | Status in TFA |
|---|---|---|---|---|
| ≤ 09-24 | `8ec5b757…` (0.156.0) | — | — | blocked: "cannot establish app-server socket mount isolation" |
| 09-24 | `71b8f5bd…` (0.156.0) | `66494455…` (0.156.0) | `4b776a3e…` (0.156.1) | NOT QUALIFIED (same blocker); later pinned blocked by D121848888/D121848889 |
| 09-25 ~14:18 → D121978134 | `d22c52fa…` | `4b776a3e…` (0.156.1) | `d291f08a…` (0.156.1, fbsource `6be63a60`) | Pinned blocked (process.py:104-121, current tip) |
| 09-28, devvm895 (seq 1885) | `c37d7ac7…` | `d291f08a…` | `7e520ac7…` | Not pinned → "release manifest identity differs" |
| 09-28, devbig030 (my `sha256sum`) | `2d4083cf…` (file dated 09-27 23:30) | `d291f08a…` | `7e520ac7…` (MSDK build from fbsource `24281a08`, 09-25 01:18) | Not pinned (inference: same refusal) |

**Why they are blocked:**

- Every signed build contains the pre-fix `linux-sandbox/src/daemon_mounts.rs`. It rejects mount aliases
  and requires mountinfo `major:minor == st_dev` for `/tmp/codex-daemon-<euid>`.
- On devbig030, `/tmp`, `/var/tmp` and `/data/tmpvol` are aliases of one Btrfs subvolume (0:42 vs 0:49).
- The fixes are upstream `7cec3e110ce6` and `a708fc683934`. In fbsource they are `18cdc3ab303f`
  (D121708813, 2026-09-25 16:57) and the 0.157.0 import `de089a75d6a1` (D121728319).
- `codex_latest` on master is still built from `24281a08`. `sl log -r 'ancestor(18cdc3ab303f, 24281a08a62b)'`
  in `/home/newton/fbsource` returns `24281a08a62b`, so the fix is not contained. Stable is built from
  `6be63a60`. Both are from local master `9e6ec042287d` (2026-09-28 11:14).

**Proof that the fixes suffice (diagnostic only):**

- The unsigned candidate `f9228bec…` passed 4/4 in v6 and 38/39 + 1/1 in v8/v8b (§3.5).
- An earlier plain-upstream build at `a708fc68` failed 4/4, for different reasons.

**The digest-pin rollover problem:**

- The compatibility key is (version, launcher sha256, release-manifest sha256, channel), plus the payload
  sha256 since D121848889.
- The owner-managed launcher and manifests change under the lane without notice. devbig030 saw 4 distinct
  launcher identities (3 changes) between 09-24 and 09-28: `8ec5b757` → `71b8f5bd` → `d22c52fa` →
  `2d4083cf`. devvm895 has a fifth, `c37d7ac7`.
- Each change turns the "reviewed blocked" skip into a hard failure ("release manifest identity differs").
  That happened on 09-25 at 14:18 (`$T/f5-containment.log`); D121978134's re-pin fixed it on 09-26; it
  is stale again as of 09-27 23:30.
- The owner's standing instruction (`/home/newton/temp/tfa-phase2-goal-20260924.txt`) forbids marking any
  unsigned or unreviewed release eligible. So until devinfra_3p_ai ships a signed build that contains
  `18cdc3ab303f`, `run-agent` cannot run.

---------------------------------------------------------------------------------------------------

## 5. Contradictions with the previous agent's reports (seq 1885, 156826) or the way-forward draft

1. **Who produced the patches.**
   - Way-forward open question: "What produced the D121848896 patches? … probably on the original host,
     perhaps before Codex was blocked (inference)."
   - Records: three Claude investigators (`claude-opus-5-5`) produced them on devbig030 on 2026-09-25
     03:27-03:51 PDT, **after** Codex was blocked. A lane-written controller built and replayed them, not
     the prototype (`$E3/RESULTS.md` §2.2-2.3). I believe the records.
2. **The asyncssl "5× unfixed / 3× patched" summary.**
   - Way-forward and seq 156826: "failed 5× before the patch and passed 3× after".
   - Records: that is seed 11 only, which was also NONDETERMINISTIC 2 of 7 times unfixed. Two of the five
     FAILs are revert-control repeats. Seed 15 ended OTHER (SEGV) in both phases. Whole-case numbers are
     5/14 unfixed FAIL and 8/9 patched PASS.
   - The patch does not need "retrieving": it is at `$E3/investigation/asyncssl/agent-output/patch.diff`.
3. **"Open-source Hermit: yes, for all three cases."** Correct on outcomes. Omitted:
   - the `/tmp` guest cwd and verify paths, which contradict the owner's no-/tmp rule quoted in
     D121848893's summary and the investigator goal text;
   - the httpcoro time-out margin (2 of 12 seed-0 FAIL attempts timed out);
   - the 5 surviving mutants at the submitted head.
   - Also, "one earlier attempt timed out" undercounts. Besides rerun 2, relocated v1 was CENSORED, and one
     batch was invalid because of the FullTrace contract.
4. **"The full flow is tested only with mocks" / "No test plan shows a real run-case" / "Draft diff:
   unreachable".**
   - At the tip, true.
   - But a real controller run on 2026-09-23 reached a thrift qualified report and a local commit
     `2f755a3be882`, and an asyncssl staged validation. Evidence is the audit in
     `/tmp/tfa-phase2/goal-20260923.CQi0pX/`; the artifacts are gone. D121649569 exists because of that run.
5. **"Real Codex ran in D121244213's tests (2026-09-22)"** (way-forward §3 and the review in
   `$DIAG/way-forward-full.json`). **Not verified.**
   - By 09-24 the prior identity `8ec5b757` was already recorded blocked. All 4-5 real-Codex cases failed
     from 09-24 onward. The 09-24 qualification runs show no tool ever executed (empty nonce).
   - The only support is D121244213's own test plan (11 containment tests passed, session
     16044073876580293).
6. **"Only the offline pieces work".** This misses that the lane *did* prove its Codex containment
   architecture works with a fixed Codex: v6 4/4 and v8 38/39 + v8b 1/1 (§3.5). Neither report mentions
   `$EV`.
7. **"10 tests (CI only) fail on sudo mount"** (seq 1885). The CI record shows 9 sudo-mount failures plus 1
   missing `chataccd-mount@…` unit (`$DIAG/results-dev.json`). The way-forward's 9 + 1 + 5 is right.
8. **"The pinned digests only match the machine they were recorded on"** (seq 1885). They also drift on the
   recording machine: devbig030's launcher changed on 09-27 23:30 to `2d4083cf…` (§4).
9. **"The mini-paper is still blocked: coord2 hasn't copied the experiment records"** (seq 156826). The
   phase-two records exist on devbig030 at `$E3` and `$EV`, with a reviewed `RESULTS.md`.
10. **RESULTS.md §10 "Unfixed FAIL and PASS witnesses, runnable now"** is no longer true:
    - `/home/newton/work/tfa/phase2-cases/stress/` and `…/investigators/` are empty (mtimes 2026-09-26
      20:45 and 21:15).
    - `$E3/relocated/run_relocated.py:36-37` pins `/home/newton/work/dev-hermit/bin/safehermit` at
      `a8a2965e…`, but that file is now `fa2a29d7…`. It changed with commits `eca41414a` and `7b205bdd3`
      on 2026-09-27. The latter, "by owner direction", stops calling `bounded-run-space`, so reports say
      `bound.disk NOT_APPLIED`.
    - The prototype's external mode requires `bound.disk=APPLIED` (PROTOCOL.md). That now diverges from
      owner policy for dev-hermit's safehermit, although the prototype runs its own packaged copy.
11. **Mutant counts.** D121978134's test plan says "Mutants: 5/5 killed". That counts runs: 4 distinct
    mutants, one of which survived the unit regex. It is disclosed, but the headline number is inflated.
    D121978137 was submitted with a known 67/5 mutant result that its test plan does not mention.
12. **Way-forward §5, "Tests run on any devserver with passwordless sudo and quota-enabled Btrfs;
    devvm895 passes 215/215 unit and 37/43 containment".** Consistent with `$DIAG`. But on devvm895 one
    non-Codex test fails with ENOSPC inside its quota fixture (seq 1885), so "any devserver" is not
    demonstrated.

---------------------------------------------------------------------------------------------------

## 6. Open questions

- Did real Codex ever execute a tool inside TFA containment on 2026-09-22, as D121244213's test plan
  implies? No retained artifact from 09-22 or 09-23 was found. `/tmp/tfa-phase2/qualified-runs-0eb3028f-20260922/`
  is empty.
- Which agent produced the 09-23 thrift candidate committed as `2f755a3be882`? Where did its artifacts go,
  given the audit's `repro/` and `artifacts/` paths are missing?
- Who emptied `/home/newton/work/tfa/phase2-cases/{stress,investigators}` (2026-09-26 20:45 and 21:15) and
  the two other case checkouts, and was anything preserved?
- Is `4630ee2a` on a facebookexperimental/hermit branch?
  `gh api repos/facebookexperimental/hermit/commits/4630ee2a…` returned the sha
  (`/tmp/sub-tfa2-030/gh-phase2-3.json`), but GitHub resolves commits across a fork network, so this does
  not prove it is on a branch there. Locally it is only on rrnewton/hermit's `salvage/devbig014/...` branch.
- The devvm895 latest Codex `7e520ac7…` equals devbig030's current latest (fbsource `24281a08` build). Is
  any MSDK build containing `18cdc3ab303f` scheduled?
- Why does `test_current_full_trace_refuses_trace_drift_and_cleans_up` hit ENOSPC in its quota fixture on
  devvm895 but not on devbig030 (seq 1885)?
- I did not re-verify the way-forward's claims about current test health ("only asyncssl still flaky, 60%,
  disabled").
