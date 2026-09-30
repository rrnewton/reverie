# Crash Cortex retry — 2026-09-29

Owner: newton. Worker: sub-crashcortex on agentcloud OnDemand node `devvm63809.lla0`,
fbsource checkout `/data/repos/fbsource`. Coordinator: coord2.
Session window: 2026-09-29T21:08Z to ~01:35Z (2026-09-30).

**Status: partial. Steps 1–4 are done (CI: 0 failures on all six final versions). Step 5 ran, but the run
was stopped at its 90-minute budget.**
The investigation reached the right root cause and a real QEMU A/B. The watchdog
stopped it after `report.md` was written and before the pipeline's post-agent
handler ran. The dev-hermit push is blocked on this node (github.com is refused to
the agent identity), so coord2 needs to commit this file.

## 0. Environment and provenance

| item | value |
|---|---|
| master base | `92964159b0c0` (D122395648), pulled 2026-09-29 ~21:12Z |
| upstream change since the old base | only D119982061 (`af282907d607`, landed 2026-09-14) |
| final local commits (after `jf sync`) | D116819880 `7e7056032c2e`, D116846891 `01ba3251cf8c`, D116857431 `2ad168449f39`, D116858160 `32eb5654b23c`, D116988175 `c94cfaf6e139`, D117035583 `f366fa2bcbbd` |
| final Phabricator versions | D116819880 v438693436, D116846891 v438693441, D116857431 v438693439, D116858160 v438693447, D116988175 v438693440, D117035583 v438693450 (three update rounds today: v4385991xx, v4386710xx, v4386934xx) |
| run binary | frozen copy built from `b9614b58083b` (top of stack after the first update round) |
| final binary (post-hoc manifest, final smoke test, final check-deps) | frozen copy built from `f7508c7e6302` (the final code; `jf sync` then changed only commit messages) |
| Claude Code | 2.1.285 |
| kernel tree | `~/local/linux` (= `/data/users/newton/linux`), cloned from the internal `kernel/linux.git`, branch `v6.16-fb`, 6.7 GB `.git`, 8 m 59 s |
| skill | `~/.claude/skills/kernel-investigation`, identical (`diff -rq`) to fbsource `fbcode/claude-templates/components/skills/kernel-investigation` at master |

Node setup done for the run:
- kernel-mcp and ods-data were registered at user scope with `claude-templates mcp … install --scope user`.
  kernel-mcp was pre-built in the `claude-plugin-mcp` isolation dir (2 m 48 s).
  `~/.claude.json` was backed up first.
- `qemu-system-x86-core` 10.1.2, `socat` and `mkosi` 24 were installed with dnf.

Scratch evidence on the node: `/data/users/newton/scratch/cc-retry-20260929/`. It holds the logs,
`run-artifacts/`, `run-transcripts/`, `per-commit-tests2.txt`, `review-result.json` and `meta/`.
These are node-local; the durable copies are this report and the diff fields.

## 1. Rebase

`sl rebase -s <D116819880> -d 92964159b0c0` was clean. main.py and BUCK auto-merged with **no conflicts**,
and no BUCK fix was needed. The rebased stack as it was, before any edits, passed
**285, failed 0** in 299 s on a cold daemon
(https://www.internalfb.com/intern/testinfra/testrun/1970325237339957).

## 2. Review items

- **D116819880, dcostantino's inline comment** ("remove the mcp-ssh-session setup lines from
  `docs/setup-crashcortex.sh`"). A later version of the diff had already dropped the script.
  I replied inline, saying what the docs now say about `--live-vmcore` setup (out of scope, left to the
  engineer), and marked the comment resolved.
  **dcostantino accepted D116819880 at 22:50:55Z**, on v438599176. The latest version, v438671059,
  adds one README paragraph after that (the transcript-location fix below), so the acceptance does not
  cover the latest version (`accept_covers_latest_version: false`). The final version, v438693436, has the same
  code as v438671059. Nothing was landed.
- **D116819880, other fixes:**
  - Hard-coded `main.py:926/:938/:971/:1230` and `pipeline.py:221` references are replaced by the function
    or guard they meant. They were already stale when written.
  - "10800 is about one week" is corrected to 7.5 days in the epilog, the README and the `--minutes` help.
  - The `--dry-run`/`--force` rationale was wrong. A dry run leaves a completed `meta.json`, and a later run
    without `--force` posts that report (`_post_existing_report`).
  - The transcript-location claim was wrong. Only triage runs in `/tmp/kinv-logs`; the other agents run in
    the worktree, and on this host their transcripts went to `-data-users-newton-linux-worktrees-<mid16>`.
  - `--triage-only` is no longer called "the smoke test".
  - The `\b` epilog blocks were verified to render correctly under the pinned Click.
- **D116857431, "Diff Queue Bot rejected it".** The reason is staleness, not code. On 2026-09-11 the
  diff-reaper bot (`generatedunixname1816288208882371`, shown as "Diff Queue Bot") wrote: "This diff is in
  Needs Review status but was last updated more than 7 days ago. Putting it back in the author's queue".
  The update addresses it.
- **D116857431, AI findings.**
  - All sync tests use `unittest.TestCase`. Same change in D116858160, D116988175 and D117035583.
    `//later:lib` stays in BUCK because `test_below_collector` uses it for async tests.
  - The unlisted-header swallow is fixed. A section is named by its leading token, and the owner-stage
    `### FIX_DISCREPANCY (owner stage)` banner, analyzer.py's aliases and `NVIDIA_*` are all recognised.
- **REPRODUCER vs AB_VERIFICATION contradiction, fixed in D116857431.**
  - *Root cause:* D116857431 itself caused it. It keyed "the reproducer ran" on exactly
    `baseline_result.txt`/`fix_result.txt` and rewrote only REPRODUCER. The 09-04 run wrote `ab_*.log`, so
    its reproducer was deleted and REPRODUCER said DID NOT ATTEMPT above AB_VERIFICATION saying FIX VERIFIED.
    Also, the investigation prompt never named the files; only the skill did.
  - *Now:* one on-disk verdict, from the new stdlib-only `ab_evidence.py`, decides both sections:
    - both arms present: untouched;
    - one arm: AB_VERIFICATION INCOMPLETE, naming the arm that ran; the reproducer and its section stay;
    - no arm: `reproducer/` is *moved* to `reproducer.NO_AB_EVIDENCE/`, and REPRODUCER DID NOT ATTEMPT and
      AB_VERIFICATION NOT PERFORMED carry the same evidence statement.
  - The agent's own names count (`ab_*.log`, negated fixes as the baseline, build logs excluded).
  - `report.raw_output` is refreshed, so the paste and the DB row match the file.
  - The prompt now names the two files.
- **D116858160, two measurement bugs found from the 09-04 numbers:**
  - Tokens were summed per transcript line. Claude Code repeats a message's usage on every content-block
    line, which gives 1.7x–2.9x inflation on this host's transcripts. They are now counted once per
    (message.id, requestId).
  - `wall_secs` was meta.json's `elapsed_secs`, the lead agent's last attempt only (the 09-04 34%
    undercount). It now runs from a per-MID start stamp in `_investigate_one`. A post-hoc `--write-manifest`
    measures earliest to newest artifact, and the agent's own figure is kept as `agent_secs`.
  - Remote outputs are read from meta.json, where the pipeline writes them. The `*_url.txt` files it looked
    for were written by nothing.
- **D117035583:** `--smoke-test` now passes on transcript evidence: a `tool_use` for the tool plus a non-error
  `tool_result`. The agent's self-report no longer decides.
- **D116846891:** probes get `stdin=DEVNULL`.

**Adversarial review of my own changes.** A workflow ran 3 lenses and 2 refuters per finding: 45 agents,
~2.24M subagent tokens, 78 min. 17 of 21 findings survived and 4 were refuted. The surviving ones are fixed
in the v4386710xx round, and three AI "General Reviewer" warnings on that round are fixed in v4386934xx:
bare `after`/`before` arm tokens, a `kernel_tree or ""` guard on the manifest's `finally` path, and requiring
non-empty tool-call ids. The review-round fixes include:
- a partial-evidence case that wrote false text ("no A/B run left a result file") and erased an honest
  REPRODUCER INADEQUATE;
- `without_fix` being counted as the fix arm;
- a verdict surviving in a header annotation;
- a header quoted in a fence before the real one winning;
- `--write-manifest` measuring wall time "to now";
- a smoke-test note comparing the wrong things;
- an over-broad resumption comment;
- a wrong README example path;
- test gaps: the raw_output refresh, the verdict function, an alias directly after REPRODUCER, and the real
  tool_result shape.

## 3. Tests (final versions)

`buck2 test …:test_kernel_investigator :libs-type-checking :test_kernel_investigator-library-type-checking`.
Each count includes the 2 type-check targets. `arc lint --rev 92964159b0c0`: no issues.

| diff | commit (pre-sync) | pass / fail | time | testrun |
|---|---|---|---|---|
| D116819880 | 4df8430ccbdc | 259 / 0 | 60 s | https://www.internalfb.com/intern/testinfra/testrun/7318349778233231 |
| D116846891 | 229b4c4da641 | 259 / 0 | 39 s | https://www.internalfb.com/intern/testinfra/testrun/17732923739183116 |
| D116857431 | 82cea9719964 | 290 / 0 | 64 s | https://www.internalfb.com/intern/testinfra/testrun/35184372128494519 |
| D116858160 | 4cda86eb5603 | 300 / 0 | 71 s | https://www.internalfb.com/intern/testinfra/testrun/6473924848410824 |
| D116988175 | 57ba2f26485b | 308 / 0 | 65 s | https://www.internalfb.com/intern/testinfra/testrun/12384899178822862 |
| D117035583 | 586f393d7650 | 324 / 0 | 66 s | https://www.internalfb.com/intern/testinfra/testrun/24769798003073866 |

At the top that is 322 tests plus 2 type-check targets, against 285 tests before the edits: 37 new tests.
Phabricator CI on the final v4386934xx round, complete at 01:28Z: **0 failures on all six**.

| diff | status | passed |
|---|---|---|
| D116819880 | OKAY | 320 |
| D116846891 | WARNING | 314 |
| D116857431 | WARNING | 333 |
| D116858160 | WARNING | 342 |
| D116988175 | WARNING | 352 |
| D117035583 | WARNING | 368 |

Every WARNING is `TEST_FINISHED_WITH_WARNINGS` with 0 failures, raised by info or AI-review signals
(see §6).

## 4. Resubmission

`jf submit -r <6 revs> --no-request-review --no-draft` was run three times: the initial update, the
review-round fixes, and the AI-warning fixes.
It updated the **same six diffs in place**, and review states are unchanged:
- D116857431 is still **Waiting For Author**. I used `--no-request-review` as instructed; moving it to
  Needs Review is the owner's call.
- The other four are **Needs Review**. D116819880 is **Accepted** by dcostantino, on the earlier version.

No reviewers were added. `ai4a` has been on D116819880 and D117035583 since August. Summaries and test plans
were updated with `meta phabricator.diff update` (no new version), then `jf sync`. The only contact with
dcostantino was the one inline reply.

## 5. Rerun of MID 4594a5b341d127cd2b03b68e269b9a23 (`--dry-run --force`)

- **Pin required:**
  - The newest crash for this MID is 1787461043 (2026-08-23), older than triage's 30-day window, so an
    unpinned run would SKIP.
  - I pinned `--representative-host twshared64861 --representative-time 1787312157`.
    Scuba confirms it as the second-newest crash, on kernel `6.16.1-0_fbk3_0_gd6c130b80483-hotfix9`.
    The newest crash was on a hardened kernel on fna011.
- **`--check-deps`:**
  - 13/13 passed with each build: 10.4 s, 26.1 s, and 25.4 s with the final build. It was run from the fbcode directory.
  - From outside a Buck project the `buck2` probe fails, because buck2 refuses every command there. `buck2 run`
    is unaffected.
  - mkosi 24 passes the probe, but kernel-mcp's generated config wants 26+, and the probe does not check versions.
- **`--smoke-test`:** passed with all three builds; the final one is built from `f7508c7e6302`.

  | build | tokens (4 messages) | wall | per-line sum |
  |---|---|---|---|
  | first rebased build | 134,480 | 72.5 s | 232,274 |
  | review-round build | 138,153 | 99.9 s | 274,474 |
  | final build | 135,845 | 72.1 s | 235,488 |

  Each figure equals an independent per-message re-sum.
- **Run:**
  - Started 22:40:19Z. The watchdog stopped it at 00:10:28Z (rc 143): **process wall 5409 s (90.15 min)**.
  - **First artifact to last:** `collection.json` at 22:40:35Z to the lead transcript at 00:10:27Z,
    **5392 s (89.87 min)**.
  - Collection took ~59 s; the agent was spawned at 22:41:23Z.
  - Milestones: theories.md at +18.5 min, kernel_fix.patch +21.8, baseline_result.txt +65.9,
    fix_result.txt +68.2, kreview_results.txt +83.4, report.md ~+86 (last write +90.1).
- **Two interventions (logged).** I killed two whole-filesystem `find /` crawls the agent started, which were
  walking the Eden mounts:
  - `find / -maxdepth 7 -name clang-[0-9]*`, ~14 min, killed at 23:18:35Z;
  - `find / -path /proc -prune -o -iname kverify*.md`, ~12.5 min, killed at 00:02:14Z. The prompt tells the
    agent to run `/kverify`, which is not an installed skill; it exists only as `prompts/skills/kverify.md`
    in the package.

  About 27 of the 90 minutes went to these crawls.
- **Tokens, all project dirs:**
  - **24,016,861** over 101 API messages. One project dir,
    `-data-users-newton-linux-worktrees-4594a5b341d127cd`, with two sessions:
    - lead `2aa5bcf2…`: 92 messages, 23,615,596;
    - one general-purpose subagent: 9 messages, 401,265.
  - By kind: input 202, output 195,678, cache read 23,385,880 (97.4%), cache creation 435,101.
  - No triage agent ran (pinned). Dedup, NVIDIA and the haiku extractor never ran (stopped first).
  - The per-line sum of the same transcripts is 68,822,328 (2.87x).
- **Manifest vs re-sum:**
  - The in-run manifest was not written: the orchestrator was stopped before `_handle_agent_result`'s `finally`.
  - `--write-manifest` gives 2 sessions, 1 project dir, **24,016,861 tokens**, and 0 unattributed
    transcripts. The review-round and final binaries give the same result.
  - That is an **exact match** with the independent re-sum (script `resum.py`, one count per
    (message.id, requestId)).
  - Its `wall_secs` is 5390 s. `agent_secs` is 0, because meta.json stayed `running`.
- **Result (report.md, written by the agent):**
  - Root cause: `scx_enable()` class-switch loop self-deadlock, with `put_task_struct()` running under
    `scx_tasks_lock`. It was introduced by `5a6fb3cfc3fb4` (= upstream `ddf7233fcab6`).
  - **Fix: backport upstream `b7d4b28db7da`**, absent from the crash kernel (`d6c130b80483`, tag `v6.16-fbk3`).
    Same root cause and fix as on 09-04.
  - CONFIDENCE high, SEVERITY high, PATCH_REQUIRED yes.
  - **A real A/B on this node**, with QEMU 10.1.2 + KVM and a 9p root. The agent routed around mkosi 24.
    A race-window amplifier was present in both arms.
    - Baseline: `Kernel panic - not syncing: Hard LOCKUP`, matching production.
    - Fix: 40 enable/disable cycles, no deadlock.
  - The agent wrote `baseline_result.txt` and `fix_result.txt` under the prescribed names, which the new
    prompt text asks for.
- **REPRODUCER vs AB_VERIFICATION agree.**
  - REPRODUCER describes a reproducer that ran. AB_VERIFICATION is FIX VERIFIED, backed by both files.
  - `ab_evidence` on the directory maps both arms (`baseline_result.txt`, `baseline_full_serial.log` /
    `fix_result.txt`, `fix_full_serial.log`), so the check leaves the report alone.
  - The call site (the check plus the raw_output refresh) was not reached, because of the budget stop, and is
    covered by unit tests.
- **Budget:** 90 min hit (stopped); tokens 24.0M < 60M. The Hermit+QEMU schedule-search option was not started.

**Against 09-04** (from coord2's brief; the 09-04 evidence was not readable here):
- Same root cause and fix.
- 09-04 took 38 m 18 s end to end with 31.85M tokens. Today took 89.9 min with 24.0M deduplicated tokens,
  including ~27 min of filesystem crawls and a full kernel build plus A/B on a fresh node.
- If 09-04's 31.85M was a per-line sum, as the manifest code of that time computed, the real figure was
  roughly a third to a half of it.

## 6. Found, not fixed (for the owner)

1. The prompt's `/kverify` and `/kreview` are not installed skills, so the agent searches `/` for them.
   Point the prompt at the package's `prompts/skills/*.md`, or install them.
2. Agents crawl `/` on Eden-backed hosts. This needs a guidance or deny rule.
3. A report from an incomplete run that is posted later (`_post_existing_report`, `--process-pending`) skips
   the A/B check. This is documented at the call site.
4. No manifest is written when the orchestrator itself is killed; use `--write-manifest` afterwards.
5. MIDs older than 30 days SKIP silently without a pin.
6. `--check-deps` does not check versions (mkosi 26+) and is cwd-sensitive for buck2.
7. Pre-existing, outside the stack:
   - `pipeline.should_skip_submit` has no callers;
   - sub-signature MIDs share `mid[:16]` worktrees;
   - `tests/test_collector.py` writes into the real `/tmp/kinv-logs`.

### Open AI-review warnings on the final versions (not addressed; owner's call)

The Devmate General Reviewer raises a new batch of edge-case warnings on each version. I stopped after
fixing three and did not start another round, to avoid churn in dcostantino's queue. All CI tests pass;
these are the `WARNING` signals.

- D116846891: `asyncio.gather` without `return_exceptions=True`, so an exception other than
  Timeout/OSError in one probe aborts the table. The code dates from 09-03.
- D116857431:
  - a report with neither A/B header gets its reproducer moved and nothing written. The directory
    name still carries the status;
  - negation is checked only on the adjacent word (`no_actual_fix`);
  - `baseline_notes.log` is rejected because of the `notes` token.
- D116858160: `_subagents_of` lacks an `isinstance(dict)` guard on `.meta.json`. The code dates from 09-03.
- D117035583: an `OSError` from spawning the stage-2 agent is not caught. The code dates from 09-03.
- D116988175: `# pyre-ignore[3]` on a test helper, a legacy directive. Also advisories about `# pyre-strict`
  headers across the package's pattern.

## 7. Asks for coord2

- Commit this file as `ai_docs/evidence/crash-cortex-retry-20260929/REPORT.md` in
  https://github.com/rrnewton/dev-hermit.
  github.com is denied here: "403 Forbidden … github.com has not been allowlisted in filter
  {"agent_id":"agent:claude_code"}", fwdproxy request 95469EBA004C4AFE83BE9976CF6C6782.
- Decide whether D116857431 should go back to Needs Review; it is Waiting For Author because of the reaper.
- D116819880's accept is on v438599176; v438671059 adds one README paragraph after it.

