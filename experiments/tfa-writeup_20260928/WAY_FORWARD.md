# TFA: what the diff stack is, and the way forward

Written 2026-09-28 from read-only records on devbig030; summarized in [README.md](README.md) section 5.
Bracketed tags point to the sources at the end; copies of non-repository sources are in [evidence/](evidence/).
"Proven" means a record shows it; "inference" means it is our reasoning.

Terms: a **seed** is a number that fixes Hermit's thread schedule, so the same seed gives the same run.
**Chaos mode** makes Hermit pick a random but seeded thread order, to expose races. **Strict verify**
runs a test twice with one seed and requires identical logs.

## Short answers

### What does the big diff stack represent?

- 22 unpublished Phabricator drafts, D121244476 to D121978137, made 2026-09-22 to 09-26 through the
  owner's account. 13 carry an AI agent's tag; the other 9 are untagged [S1].
- They add a local command-line prototype, `tfa-phase2`, in `fbcode/testinfra/tfa` (not on fbsource
  master). It handles exactly three flaky C++ tests whose PASS and FAIL seeds the owner's 2026-09-21
  Hermit campaign had already found [S1, S2].
- 36,416 changed lines; the final tree is 32,682 lines, half tests. One diff, D121244213 (running Codex
  agents in a sandbox), is 55% of the changed lines [S1].

### How does it connect to old infra?

- By name and idea only: run a flaky test under Hermit chaos and keep a PASS seed and a FAIL seed. The
  stack calls itself "the bounded three-case Test Flakiness Analyzer prototype" (stack-tip `README.md:3`),
  but no old TFA code was found in it: a search of the stack for old TFA's paths and identifiers found
  0 files [S3].
- Old TFA (2021-22) was a WWW service with Sandcastle jobs and a report page; Test Infra took it over in
  December 2022. It is dead, as the owner thought: its rate limit is 0 runs per day (last human edit
  2023-06-13, so probably off since then; inference), its trigger was deleted on 2025-10-01, most of
  its code by 2026-09, and the test runner's Hermit hook is gone [S3].
- The stack borrows another team's bot for dry-run comments and names `testinfra` as its oncall. No record
  of agreement from Test Infra or the AutoTestRemediation bot's owners was found [S3].

### What functionality does it actually implement?

- Works: input checks, a PASS-versus-FAIL log comparison, a verdict on candidate fixes, and dry-run task
  comments [S4].
- Works on devbig030 only: replaying the six recorded seeds with open-source Hermit. 30 of 30 runs gave
  the expected result in 5 batches, but HTTPCoro's FAIL run takes 109-118 s against a 120 s limit and
  timed out in 2 of 12 tries with the valid verify setting [S4].
- Blocked: the internal fbsource Hermit (an old import that failed 8 of 8 qualification runs), Codex (no
  signed build has the needed fix), posting comments, and drafting diffs. There is no test discovery or
  scheduling [S4].
- The three candidate test fixes were written by three Claude agent sessions (`tfa-inv-<case>`, launched
  with agentctl), and a separate shell script (`stress/postfix_20260925.sh`) built and replayed them; the
  stack did neither [S5].

### Should we make this a committed pipeline moving forward?

- Not this code (recommendation). It is hard-wired to 3 pinned tests and `/home/newton` paths. Two diffs
  have failing CI, and the top diff has 30 land-blocking warnings. Much of it is sandboxing and
  byte-pinning for a Codex agent that cannot run and for Hermit binaries on one host; no record measures
  the Codex-only share [S1, S4, S6].
- Yes to a small committed loop in dev-hermit, first as a Hermit test workload: the campaign exposed
  real Hermit bugs (below).

### Is it designed for a devserver, or does it work on Sandcastle or cloud?

- Designed for one devserver: it needs passwordless sudo, Btrfs loop mounts and a systemd unit, and its
  replays need a Hermit binary and test binaries that exist only on devbig030 [S4, S6].
- Sandcastle: not yet. In CI, 15 of its 43 sandbox tests fail in each build mode (9 sudo, 1 systemd, 5
  Codex). Hermit's own smoke test has failed 53 trunk runs in a row there; a bot-authored candidate
  fix, D121815443 ("[Autopilot][Test Fixer]"), awaits review, with the owner as a reviewer [S6, S7].
- OnDemand (agentcloud): partly tried. On 2026-09-28 the stack's containment suite ran on devvm895, an
  agentcloud short-lease devvm, and passed 37 of 43. The 6 failures were 5 stale Codex pins and 1
  out-of-space error; 9 of the 10 sudo, Btrfs-loop and systemd tests that fail on Sandcastle passed
  there. Not tried there: the Hermit witness replays (their inputs exist only on devbig030) and whether
  the node exposes the CPU counters Hermit chaos needs. Heavy TFA work runs there by owner directive
  [S3, S10].

### Would it be feasible to revive a small TFA as we test?

- Yes, as a script-driven loop, not a service. The old service cannot be restarted: its trigger and
  most of its code are gone.
- The campaign got both seeds for 3 of 10 tests tried, in about 76 minutes of Hermit time (294
  seed trials, each two strict-verify runs) over 8 h 22 min of calendar time [S2].
- Of the 7 misses, one Hermit loader bug stopped 3, and a stray Hermit signal probably stopped a fourth.
  A fifth, the TLS terminator, never passed: all 31 of its trials were discarded because of Hermit (29
  with the two runs differing, 2 aborted by Hermit's old fixed date), and why no run passed is not
  established. The 3 wins needed hand-picked time settings (slowing some threads plus a timeslice or
  clock change), so a loop must sweep those itself [S2, S8].
- Median Hermit time per trial (one seed, two runs): AsyncSSL 4 s, Thrift 15 s, HTTPCoro 7 s passing and
  75-110 s on its abort path [S2].

### How much needs repair or rebuild?

- Mostly rebuild, but small. Keep about 570 lines (log hints, gtest result parsing, fix verdicts), the
  three test records and the evidence [S3].
- Drop the Codex sandbox and byte-pinning: roughly 10,000 of the 16,501 non-test lines (inference) [S1, S3].
- New: test lookup (`meta testinfra.test metadata` gives each run command), build, a seed and setting
  sweep, and a report. Estimate 200-500 lines of script (inference) [S3].
- Hermit fixes that matter most:
  1. Hermit's injected `fstat` makes the loader's `openat` fail (EFAULT). It stopped 3 of 7 misses. Filed as
     https://github.com/rrnewton/hermit/issues/3328; the fix looks easy [S8].
  2. Hermit determinism gaps. 93 of the 294 trials were discarded: 42 were real test failures other than
     the declared one, and 49 were stopped by Hermit. In 44 of those 49, the two strict-verify runs
     differed. Causes: thread teardown order after an abort (added to
     https://github.com/rrnewton/hermit/issues/3247), loopback socket timing (https://github.com/rrnewton/hermit/issues/3330), and few-ns
     clock drift under slowdown (https://github.com/rrnewton/hermit/issues/3329) [S2, S8].
  3. A stray SIGSTKFLT, probably Hermit's own timer signal (strong inference), killed 96 of 98 threads in
     one test; evidence added to https://github.com/rrnewton/reverie/issues/747 [S8].
  4. Virtual-clock timers (open https://github.com/rrnewton/hermit/pull/3229). The 2026-01-01 default
     date that broke TLS certificates is fixed on main (402ba9737) [S8].

## Recommendation

Leave the stack unlanded as an archive, and preserve its evidence, which sits in an untracked directory.
Fix the loader bug. Build a small dev-hermit loop on OnDemand: first the three known tests on current
Hermit main, then the other 27 tests on the campaign's list. Consider Sandcastle only after D121815443
lands and fbsource gets a current Hermit.

## Next steps (durations are estimates)

| # | Step | Estimate |
|---|---|---|
| 1 | Leave the 22 drafts unpublished, or abandon them (owner only: decision 1) | minutes |
| 2 | Copy RESULTS.md, the 3 patches and the Codex receipts into a tracked `experiments/` directory (owner decision 9; time-sensitive, because the files sit under another agent's slot and in `/tmp/tfa-phase2`) | 1-2 hours |
| 3 | Done 2026-09-28 (project policy, no owner approval needed): https://github.com/rrnewton/hermit/issues/3328, https://github.com/rrnewton/hermit/issues/3329, https://github.com/rrnewton/hermit/issues/3330, plus evidence comments on https://github.com/rrnewton/hermit/issues/3247 and https://github.com/rrnewton/reverie/issues/747 | done |
| 4 | Fix the loader bug; `experiments/tfa-openat-stack-size-20260917/` already holds a test probe | 1-2 days |
| 5 | OnDemand check (the containment suite already ran on devvm895): tracing, namespaces, CPU counters; build Hermit main and one test | 1-2 days |
| 6 | Re-find PASS and FAIL seeds for the 3 tests on Hermit main (the campaign's Hermit commit was never on main, and a seed can change when the test's file paths change [S5 §4]) | about 1 day |
| 7 | Loop script: test ID, build, sweep, PASS/FAIL table, report | 3-5 days |
| 8 | Run the other 27 tests in small batches | 1-2 weeks |
| 9 | Sandcastle: land D121815443 (owner only: decision 7), refresh fbsource Hermit, get a bot identity | weeks |

No diff is published, landed, abandoned or given reviewers without an explicit owner instruction (the
owner's rule for this work, recorded in the journal of agentcloud session
`ef420c5e-34f0-4659-bbba-2967ac725a1a`).

## Decisions only the owner can make

**Update, 2026-09-29.** coord2, the coordinating agent, judged that items 2, 3, 6, 8 and 9 below did not need the owner. It decided them in its round-2 dispatch to sub-tfa2-030 (2026-09-29):

- 2: the loop lives in dev-hermit for now.
- 3: a six-seed replay may run on devbig030, one at a time, under nice/ionice; anything heavier goes to an OnDemand node.
- 6: use Claude, if a diagnosis agent is needed at all.
- 8: this round's budget is the 3 known tests, 6 seeds per test, and 30 minutes of wall time per test.
- 9: preserve a curated copy of the phase-two evidence, which is now in [evidence/phase2/](evidence/phase2/MANIFEST.md).

Items 1, 4, 5 and 7 remain the owner's ([ROUND2.md](ROUND2.md)).

Already decided, so not listed: heavy TFA experiments run on OnDemand agentcloud nodes, not devbig030
(owner directive of 2026-09-28, relayed by coord2 in sub-tfa2-030's launch prompt); and filing the Hermit
defects needed no decision (next step 3, done).

1. **Abandon the stack, or keep the drafts?** Abandon is clean. Keeping costs nothing, but its Codex
   pins keep going stale.
2. **Where the loop lives.** dev-hermit: fast and owner-controlled. `fbcode/hermetic_infra`:
   Sandcastle-ready later; needs a Hermit re-import. `fbcode/testinfra`: needs Test Infra's agreement,
   and their wiki advises against outside fixers.
3. **Quick replays on devbig030?** Does a quick replay of the six recorded seeds (about 210 s including
   checks) count as light enough to run on devbig030, which is registered to another user? Everything
   heavier runs on OnDemand.
4. **Send the AsyncSSL candidate patch to its owners?** Yes: it may help a test now disabled as flaky
   (TestInfra flakiness score 60%; 52 of 186 stress runs failed, 2026-09-26 to 09-28 [S9]). No: the
   evidence is Hermit-only, and the patch might hide a real `AsyncSocket` bug.
5. **Posting.** None (GitHub issues only), or a new Hermit-owned bot (weeks).
6. **Diagnosis agent.** None, Claude (wrote the current patches), or Codex (blocked for now).
7. **Review D121815443?** It is a bot-authored "[Autopilot][Test Fixer]" candidate fix, not yet proven on
   trunk. Landing it is the first of three Sandcastle prerequisites; step 9 also needs a refreshed
   fbsource Hermit and a bot identity. No keeps work on devservers and OnDemand.
8. **Budget.** Tests, seeds per test, and a wall-time cap.
9. **Preserve the untracked phase-two evidence?** Copy `worktrees/slots/tfa-phase2-cyber-opus/ignored/`
   (with the relevant `/tmp/tfa-phase2` files) into a tracked directory: yes or no. It is time-sensitive:
   it sits under another agent's slot and in `/tmp`.

## Sources

- [S1] `meta phabricator.diff describe|files --number D<N> -o json` for all 22 diffs; totals recomputed
  from `/tmp/sub-tfa2-030/gather/stackstate-raw/files-D*.json` (copies in `evidence/gather-raw/stackstate-raw/`). Tags and phase:
  `/tmp/sub-tfa2-030/gather/stackstate.md` §1, §5 (copy `evidence/gather-stackstate.md`). Master: `sl files -r remote/master fbcode/testinfra/tfa`
  in `/home/newton/fbsource` (rc 1 at `9e6ec042287d`). Details and per-diff table: `STACK_STATE.md`.
- [S2] `experiments/tfa-kernel71-campaign_20260921/RESULTS.md` §4.1 (10 tried, 3 with both seeds, 294
  runs, 93 discarded). Per-run time: safehermit `elapsed_secs`, recounted in
  `/tmp/sub-tfa2-030/gather/continuations.md` §3 (copy `evidence/gather-continuations.md`). Discard causes:
  `/tmp/sub-tfa2-030/gather/campaign.md` §5 (copy `evidence/gather-campaign.md`).
- [S3] Old TFA: `meta sitevar.sv read --name=TEST_FLAKINESS_ANALYZER_RATE_LIMIT`, `meta sitevar.sv history`
  (same name), `sl log www/flib/intern/hermetic_infra` (commit `0f046cdf3542`), D82956704, D118696067.
  Bot: `www/flib/intern/scripts/auto_test_remediation/AutoTestRemediationScheduler.php:41-46`, stack
  `comments.py:21`; oncall: stack `BUCK:16`. Kept lines: stack `hints.py`, `pipeline.py:119-342`,
  `followup.py:199-338`. OnDemand notes: `/tmp/sub-tfa2-030/gather/wayforward.md` §3.3 (copy `evidence/gather-wayforward.md`). Heavy-work
  policy: owner directive of 2026-09-28, relayed by coord2 in sub-tfa2-030's launch prompt. Name search:
  `gather-wayforward.md` §1 row 11 (it searched for `TestFlakinessAnalyzer` without spaces, so it could not
  match the stack's own name); stack-tip `README.md:3`.
- [S4] Stack tip (copy in `/tmp/sub-tfa2-030/devvm895/tfa-src/fbcode/testinfra/tfa/`, not copied here; the diff summaries are in
  `evidence/devvm895/STACK_SUMMARIES.md`): `state.py:21-28`,
  `process.py:104-121`, `comments.py:287`, `manifest.py:193,249-277`. Replays:
  `/home/newton/work/tfa/external-hermit/witnesses-*/SUMMARY.txt`,
  `/home/newton/work/tfa/external-oss-mode/D121978137-test-plan.txt` (copies under `evidence/tfa-lane/`); summary in
  `/tmp/sub-tfa2-030/gather/phase2.md` §2-3 (copy `evidence/gather-phase2.md`).
- [S5] `worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/RESULTS.md` §2.2-2.3 and §4 (untracked; copy
  `evidence/phase2-three-case-RESULTS.md`). Agent launch: `investigation/<case>/start.out` in the same tree names
  `/home/newton/work/dev-hermit/.agentctl/tfa-inv-<case>/` (not copied).
- [S6] `meta phabricator.diff stack-ci --number=D121978137 --output=json`;
  `meta phabricator.diff.signals list --number D121978137 --status=warning --details`; TestX run 1125900439774673.
- [S7] `meta testinfra.test metadata --id=562950400403548 --output=json`, run 2026-09-28 22:17 UTC
  (DISABLED_FAILING, 53 consecutive trunk failures; saved output `evidence/way/hermit-test-meta.json`);
  `meta phabricator.diff describe --number=D121815443` (Needs Review; `evidence/way/d121815443.json`).
- [S8] `/tmp/sub-tfa2-030/gather/fixability.md` §1, §4 (copy `evidence/gather-fixability.md`; retained Hermit logs under `ignored/safehermit-loopback/`;
  probe in `experiments/tfa-openat-stack-size-20260917/`).
- [S9] `/tmp/sub-tfa2-030/gather/wayforward.md` §2 (copy `evidence/gather-wayforward.md`): `meta testinfra.test metadata
  --id=562950211052906` and `meta testinfra.result history --test=562950211052906 --since=ONE_WEEK --final-only
  --limit=200`, 2026-09-28. The 60% is TestInfra's score; 52 of 186 (28%) is the raw stress-run fraction.
  The verify pass re-ran the history query: same window (2026-09-26T08:31:57Z to 09-28T21:40:37Z), 186
  stress runs, 52 FAILED; its raw output is `/tmp/sub-tfa2-030/verify/hist-asyncssl.json` (copy
  `evidence/verify/hist-asyncssl.json`).
- [S10] devvm895: `agentcloudctl inspect --session ef420c5e-34f0-4659-bbba-2967ac725a1a` (node
  devvm895 with a reservation lease) and `meta devserver.server describe
  --hostname=devvm895` (type devvm, "Short-term lease devserver", reserved 2026-09-28
  11:03-19:03 PDT); saved as `/tmp/sub-tfa2-030/verify/acl-inspect.txt` and `devvm895.json` (copies in
  `evidence/verify/`). Containment run: `evidence/devvm895/containment-local.log.txt` line 255, 11:15-11:20 PDT.
  devbig030's registration: `meta devserver.server describe --hostname=devbig030` (copy
  `evidence/verify/devbig030.json`).
