# TFA prototype: what it is and the way forward

*Paths are under `fbcode/testinfra/tfa/` at stack tip a729ddf3707f unless noted. "Refuses" means it deliberately stops.*

## TL;DR
1. **Is Hermit ready to replicate TFA results?** Open-source Hermit 4630ee2a: yes, for all three cases on the original host (D121978137). Internal Hermit a919cce: not yet (sanitizer crashes, a CPU-counter panic, trace drift; hermit-import-compatibility.v1.json). Sandcastle: no; Hermit's own test fails there on a mount error, 53 runs straight (`meta testinfra.test metadata --id=562950400403548`).
2. The stack is a 32.7k-line AI-written prototype for three C++ tests. Only asyncssl is still flaky (60%, disabled); httpcoro and thrift are marked OK (`meta testinfra.test metadata`). It shares an idea with old TFA, not code.
3. Little runs end to end: internal Hermit and Codex are blocked; comments are dry-run only.
4. **Recommendation: do not land it.** Share the asyncssl candidate patch with its owners, then wrap Hermit's existing `analyze --search`.
5. Later, add a Hermit step to existing flaky-test agents (not a new service) once Hermit runs on Sandcastle.

## 1. What does the big diff stack represent?
22 unpublished, unreviewed diffs (D121244476 to D121978137) adding 33 files. It "does not discover tests or schedule production work" (README.md:3-4). Half (16.1k lines) is tests. About 5 diffs add features; 17 add pinning, tamper checks and refusals (inference from STACK_SUMMARIES.md titles).

## 2. How does it connect to old TFA infra?
Only by idea. Old TFA (2021-22) re-ran flaky tests on Sandcastle, including Hermit chaos seeds (wiki TFA/; T106084722). It is dead: automatic runs are off (SV_TEST_FLAKINESS_ANALYZER_RATE_LIMIT per_day_limit 0) and most of its code is deleted (D82956704, D118696067).

The stack reuses the technique, `hermit run --chaos` with fixed PASS/FAIL seeds (cases.v1.json:55,72); its "historical" records are your own 2026-09-21 runs (manifest.py:15-35). It borrows another team's bot (comments.py:21; AutoTestRemediationScheduler.php:41-46) and puts its code under the testinfra oncall (BUCK:16).

## 3. What functionality does it actually implement?
Intended flow (state.py:21-28): check recorded runs, dry-run a comment, Codex proposes and reviews a patch, replay and test, draft a diff. Only offline pieces work. No test plan shows a real `run-case`; the full flow is tested only with mocks (tests/test_prototype.py RunCaseTest).

| Component | Status | What it would take |
|---|---|---|
| Manifest, prompt and report validators | works offline | Allow more than 3 hard-coded cases (manifest.py:249-277) |
| PASS/FAIL log-diff hints (`render-evidence`, hints.py) | works offline on records made elsewhere | Probably superseded by `hermit analyze` (inference) |
| Follow-up verdict (`render-followup`) | works offline (followup.py:199-338) | Something in the package to produce its inputs |
| `dry-run` | writes prompts; for asyncssl also reads the live task's comments and dry-runs a comment as the borrowed bot (orchestrator.py:48-80; cases.v1.json:288) | Low value |
| Replay with internal Hermit | refuses (a919cce BLOCKED, hermit-import-compatibility.v1.json) | Newer import, re-recorded pins |
| Replay with open-source Hermit (`run-witness`) | works on the original host only (D121978137) | Buck-built binaries instead of /home/newton paths |
| `run-seed`, `run-case` | refuse: internal Hermit blocked (main.py:282,374) | Unblock Hermit |
| `run-agent` and Codex review | refuse: all Codex pins blocked (process.py:104-121). Real Codex ran in D121244213's tests (2026-09-22); blocking began at D121848885 (D121848886: codex-cli 0.156.0) | An approved Codex version, or Claude |
| Task comments | always refused (comments.py:287) | A bot identity TFA owns |
| Draft diff | unreachable | Everything above |
| Test discovery and scheduling | missing | `meta testinfra` queries |
| Containment tests in Sandcastle CI | 15 of 43 fail per build mode; land-blocking warnings that also fail on the parent revision (`meta phabricator.diff.signals list --number=D121978137 --details`) | VM tests or local-only marking |

## 4. Should we make this a committed pipeline?
No, not in this form:
- It is hard-wired to three hash-pinned cases and /home/newton paths (manifest.py:193, :249-277).
- Internal Hermit and Codex are blocked.
- It borrows a bot and an oncall (§2).
- 30 land-blocking CI warnings fail (15 tests × 2 modes).
- It would be a fixer outside Test Infra's resolver, which their wiki recommends against (wiki Test_Infra_For_Users/Getting_Started/Test_Issues/Agentic_Test_Fixers); that resolver doesn't take flaky tests today (DefaultTestFixerStrategy.php:9-11).

Worth keeping: the asyncssl case (others as fixtures), candidate patches, and ~0.6k lines of hint and verdict logic (hints.py; pipeline.py:119-342; followup.py:199-338).

## 5. Dev server, or Sandcastle / cloud?
Devserver only. Tests run on any devserver with passwordless sudo and quota-enabled Btrfs (resources/bounded-run-space:95-99,191-224); devvm895 passes 215/215 unit and 37/43 containment tests. Hermit replays also need /home/newton/work files (manifest.py:193), missing on devvm895. Those files are on devbig030 (devbig030.atn3.facebook.com), where 13 of the 22 diffs were developed.

On Sandcastle, 9 tests fail on denied `sudo mount`, 1 on a missing devserver service, 5 on Codex hash mismatches (D121978137 signals). None reached Hermit.

## 6. Could we revive a small TFA as we test?
Yes, small (inference):
- **Hermit already does the core.** `hermit analyze --search` finds a failing chaos seed, pairs it with a passing run, and prints the racing instructions' stack traces (hermetic_infra/hermit/hermit-cli/src/bin/hermit/analyze/types.rs:22-43).
- **Existing flaky-test agents:** a fix-flaky-test skill (fbcode/claude-templates/components/skills/fix-flaky-test/SKILL.md), used by AutoTestRemediation (AutoTestRemediationCommand.php:199-201). Devmate's asyncssl attempt errored (T255721720). None appear to use Hermit (inference).
- **Where to run:** devserver for now. Sandcastle is tightening containers (user-namespace prerequisites targeted mid-October; Workplace sandcastle.eng post 4477862635875268) and moving mount-heavy tests into VMs (T288822939), a possible Hermit route (inference).
- **Scope:** Hermit is x86-only (commit 4620432e135b); native fbcode tests only.
- **Posting:** inside an existing agent, no new identity (inference); standalone needs its own service user (wiki Tasks/Task_Bots), not the "trusted issuer" the stack awaits (README.md:13-14; inference).

## 7. How much needs repair or rebuild?
Most of it (inference). Keep §4's list. Rebuild Hermit packaging, test binaries and discovery. Drop Codex and most of the 10-12k lines (60-75% of product code) of pinning and sandboxing.

## Recommended next steps
1. Mark the stack do-not-land; remove the borrowed bot ID and oncall. *Hours.*
2. On devbig030, retrieve the asyncssl candidate patch (D121848896: failed 5× unfixed, passed 3× patched; small sample); optionally share it with T255721720's owners. *Hours.*
3. Rerun the 6 open-source Hermit runs there (D121978137; one earlier attempt timed out). Seeds depend on paths and timing (D121848893); re-check on new hosts. *Hours.*
4. Finish your import D120908510 (rrnewton/hermit via facebookexperimental/hermit), confirm it includes 4630ee2a, re-record pins. May unblock internal Hermit (inference). *Days.*
5. Build a thin wrapper: TestID → Buck build → `hermit analyze --search`. Start with asyncssl. *Days (inference).*
6. Probe Sandcastle: `hermit run --namespace-only -- /bin/true`, then one chaos seed. *1-2 days.*
7. If step 5 works, offer a Hermit step to the fix-flaky-test owners. *Weeks.*
8. Delete /tmp/tfa-phase2 on devvm895. *Minutes.*

## Open questions
- What produced the D121848896 patches? A "controller" wrote the ledgers (README.md:63-74), probably on the original host, perhaps before Codex was blocked (inference).
- Is 4630ee2a in public facebookexperimental/hermit? (GitHub is unreachable here.)
- When did old TFA's automatic runs stop (sitevar audit log)?