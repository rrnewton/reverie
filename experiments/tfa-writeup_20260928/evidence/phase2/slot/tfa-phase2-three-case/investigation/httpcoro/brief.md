Hermit quick-start
- Build with Buck outside Hermit.
- Run witnesses only through the controller command shown below. It resolves,
  verifies, and pins the packaged safehermit, bounded-run-space, and Hermit
  resources; no machine-local dev-hermit checkout is required.
- Preserve argv tokens exactly; never invoke through a shell.
- For every Hermit invocation, create a fresh `controller-cwd` under that
  attempt's artifact directory. Add a same-path bind for it, add
  `--workdir=<fixed case working_directory>`, add Hermit's `--timeout=110`
  inside safehermit's 120-second deadline, and invoke safehermit with the tool
  working directory set to `controller-cwd`. Keep the outer timeout at least
  130 seconds. Never run Hermit from the fixed guest working directory.
- Preserve and hash any `.hermit-verify-summary-*` timeout residue in the
  controller directory; never delete or allowlist it. The fixed guest working
  directory must remain sealed and unchanged.
- Keep the same-path buck-out bind mount because the ELF resolves runtime
  dependencies there.
- For post-fix replay, use the ELF built in the isolated case checkout and
  rewrite both the guest ELF path and same-path buck-out bind mount to that
  checkout.
- Never copy, replace, or link an ELF or dependency into
  /home/newton/work/tfa/fbsource/buck-out.
- Treat `/tmp/tfa-phase2/repro` and the sealed historical manifest as immutable
  supplied evidence. Use only the controller commands below; the controller maps
  the historical contract to the current packaged CLI only after its
  compatibility gate passes.
- The current strict comparison contract is `--verify --verify-verbose
  --verify-allow both`: verbose selects FullTrace and disables line stripping.
  Inspect its evidence instead of relaxing comparison.

Investigation contract
1. Reproduce both the supplied PASS and FAIL before editing. Inspect the
   selected test and production code.
2. Separate the trigger/fatal symptom from the originating invariant
   violation.
3. Emit 2-5 ranked falsifiable hypotheses. Each needs supporting evidence,
   contradicting evidence, and the next discriminating experiment. Select
   exactly one top hypothesis.
4. Propose the smallest principled fix in the exact patch artifact. The source
   checkout is read-only to your tools: do not edit it, invoke source-control
   mutations, or attempt to apply the patch yourself. Do not quarantine or
   disable tests, weaken assertions/comparators/tolerances/timeouts, add
   sleeps/retries, reduce stress counts, or change the expected verdict.
5. Do not invoke Buck, Arc, or the post-fix stress grid. Native network access
   is disabled, and only the trusted controller may apply the returned patch
   and run those commands after your process and cgroup are gone. You may run
   the supplied pre-fix safehermit witnesses with outputs redirected to the
   dedicated artifact directory.
6. Record proposed validation argv and any observations honestly, including
   failures or checks you could not run. They are candidate-only and never
   qualify the patch. The controller will run Buck, lint, exact PASS/FAIL and
   fresh stress seeds, then a separate hash-bound exact-diff review.
7. You must not post comments, submit/create/publish/land diffs, assign
   reviewers, commit/amend/rebase source control, or invoke identity-sensitive
   actions. The supplied isolated checkout may only be read and inspected.
   Write candidate artifacts only to the dedicated agent-output paths; the
   deterministic controller owns every source mutation and build action.
8. Do not create, replace, link, or write the report path. Return the compact
   machine-readable JSON report as the final response with
   `qualification="candidate-only"`; the trusted controller publishes and
   seals it after the tool process is gone. The report must satisfy
   report.schema.v1.json. Write hypotheses, patch, and proposed validation only
   to their dedicated artifact paths.

Fixed case facts (JSON; do not reinterpret):
{
  "authoritative_record": "https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/experiments/tfa-kernel71-httpcoro-midpoint-continuation_20260921/services/proxygen/httpcorosessiontests--e3f69b337304/experiment.json",
  "build_argv": [
    "buck",
    "build",
    "@fbcode//mode/dev-asan",
    "fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests",
    "--show-full-json-output"
  ],
  "case_id": "httpcoro",
  "checkout": "/home/newton/work/tfa/phase2-cases/investigators/httpcoro",
  "controller_fail_command": "buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest fbcode/testinfra/tfa/cases.v1.json --case-id httpcoro --outcome fail --artifact-root /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output",
  "controller_pass_command": "buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest fbcode/testinfra/tfa/cases.v1.json --case-id httpcoro --outcome pass --artifact-root /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output",
  "controller_working_directory": "create a fresh <attempt-artifact-dir>/controller-cwd per invocation",
  "evidence_paths": {
    "hypotheses": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/hypotheses.json",
    "patch": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/patch.diff",
    "report": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/report.json",
    "validation": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/validation.json"
  },
  "fail_seed": 0,
  "failure_class": "reset-rate-limit-line1685-body-event-fatal",
  "fresh_stress_seeds": [
    4,
    5,
    6,
    7
  ],
  "hermit_revision": "4630ee2a61f93221a43e2baec0a15f34102a2c61",
  "historical_fail_seed": 0,
  "historical_pass_seed": 3,
  "issue_id": "224798678",
  "pass_seed": 1,
  "selector": "fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2",
  "source_revision": "7f239d5b37dfbecf32fed144468da461f998add5",
  "test_argv": [
    "buck",
    "test",
    "@fbcode//mode/dev-asan",
    "fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests",
    "--",
    "--exact",
    "fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2"
  ],
  "test_id": "562950215319283",
  "timeout_layers_seconds": {
    "hermit": 110,
    "outer_minimum": 130,
    "safehermit": 120
  },
  "working_directory": "/tmp/tfa-phase2/cwd/httpcoro"
}

===== END OF EXACT FIXED PROMPT =====

# Controller supplement for this diagnostic run

This supplement is from the controller (the operator running TFA phase two). It
overrides the fixed prompt above only where a point below says so. Everything
else in the fixed prompt stands.

## S1. Status of this run
- This is a diagnostic proof-of-concept investigation for owner study. The
  production gate, signed Codex 0.156.0, is BLOCKED (36/40 containment tests).
  Nothing you produce qualifies a patch or makes that gate green.
- You are the only investigator for case `httpcoro`. Two other Claude Opus 5.5
  sessions investigate the other cases in their own checkouts. Do not read,
  write, or run anything for them.

## S2. Witness commands (overrides `controller_pass_command`, `controller_fail_command`, `working_directory`)
- Do not use `controller_*_command` from the fixed facts. It routes through the
  packaged-Hermit `buck run ... run-witness` path, which is blocked here.
- Do not use the fixed `working_directory` or any other path under `/tmp`. The
  owner has forbidden all execution under `/tmp`.
- The measured non-`/tmp` PASS and FAIL reproducers for this case are in the
  unposted initial comment below.
  - The PASS witness is sched-seed 1 and the FAIL witness is
    sched-seed 0. Each was confirmed on 2 decided attempts, with 0
    censored.
  - Relocation summary: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/httpcoro/summary.json` (sha256 `fca9f308f0dec565fe48cfb9d8148ded7ae58fa90f310493b7ecd829850efa79`).
- Do not re-run those commands verbatim. Their report and verify-log paths are
  preserved evidence and must not be overwritten.
- To reproduce, use the controller replay runner. It replays the measured
  witness argv. It rewrites only the attempt paths (the cwd same-path mount,
  `--tmp`, `--sh-report` and `--verify-log-dir`) into a new directory you name,
  creates a fresh owned empty `controller-cwd` there, verifies the pinned
  safehermit, Hermit and ELF hashes, classifies the run, and writes
  `receipt.json`:

      /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/witness.sh --case httpcoro --template pass --out /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/witness/<new-label>
      /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/witness.sh --case httpcoro --template fail --out /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/witness/<new-label>

  Use a new label for every attempt; the runner refuses an existing directory.
  Optional flags:
  - `--seed N` replays the same argv with a different `--sched-seed`. Do not
    use the fresh stress seeds `[4, 5, 6, 7]`. They are reserved for the
    controller's post-fix stress.
  - `--note TEXT` records your intent in the receipt.
- A replay's path differs from the measured command, so each replay is a new
  measurement. Report what it actually shows.
  - `CENSORED` is a bound firing. It is neither PASS nor FAIL.
  - If a replay does not reproduce a witness, record that honestly. Do not
    re-run until the result looks right.
- Runs take up to about 4 minutes, with a 240-second outer bound. Never run
  more than one Hermit replay at a time.
- The runner script is `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/run_witness.py`, and its helpers are
  `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated/run_relocated.py`. Read them but do not modify them.

## S3. Your checkout (overrides "read-only" in contract items 4 and 7)
- Your own writable checkout is `/home/newton/work/tfa/phase2-cases/investigators/httpcoro`. It is an Eden clone at
  `7f239d5b37dfbecf32fed144468da461f998add5`, and `sl status` was empty at
  start.
- The owner authorizes you to draft the fix by editing source files in this
  checkout only.
- Everything else in items 4 and 7 still applies:
  - Do not commit, amend, rebase, shelve, goto, revert, purge, or run any other
    source-control mutation.
  - Do not touch any other checkout, especially
    `/home/newton/work/tfa/fbsource` and `/home/newton/work/tfa/phase2-cases/<case>`.
  - Do not quarantine tests, weaken assertions, add sleeps or retries, or
    change the expected verdict.
- When your fix is final, write it to `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/patch.diff` with
  `sl diff --reason "export TFA httpcoro draft fix - sl help diff" > /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output/patch.diff`
  run in your checkout. It must match your checkout's changes exactly. The
  controller will apply `patch.diff` in its own separate checkout.
- Do not invoke Buck or Arc, as the fixed prompt says. The controller builds,
  then stress-tests the fixed ELF against both witness schedules and the fresh
  stress seeds after you finish.

## S4. Deterministic inputs
- The difference hints and the unposted initial comment are reproduced
  verbatim below. Their files are:
  - `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stages/httpcoro/stage1-2-relocated/difference-hints.json` (sha256 `bc1fd9e66e5482d790c6f39be1aa9190ff575d4e71718c731f74f2861a39b5d7`)
  - `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stages/httpcoro/stage1-2-relocated/initial-comment.md` (sha256 `c91059c477362f91af0bc6c67cdea9697d5a23bb25be1ff3a34f0df3e40aa593`)
- Historical phase-one evidence, read-only: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro`.
- Report schema: `/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/report.schema.v1.json`.

## S5. Outputs
- Write these into `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output`:
  - `hypotheses.json`: 2 to 5 ranked hypotheses, each with supporting and
    contradicting evidence and a next experiment, and exactly one top
    hypothesis.
  - `patch.diff`
  - `validation.json`
  Also write a short human-readable `notes.md` there, covering the root cause,
  the trigger versus the originating invariant, and what you ran.
- Do not write `report.json`. Return the report as your final message, as JSON
  that satisfies the schema, with `qualification="candidate-only"`.
- You cannot build, so you cannot measure the fixed test.
  - Set `original_fail_now_passes` and `original_pass_still_passes` to `false`,
    meaning "not demonstrated by the investigator".
  - For each validation you could not run, give its proposed argv, status
    `FAIL`, passed 0, total 0, and an artifact string beginning `NOT RUN:` that
    says why.
  - The controller's stress results are authoritative.

## S6. Hard limits
- Set `TMPDIR=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/tmpdir` on every shell command. Never write under `/tmp`.
- Do not use grep, find or rg over fbsource; they time out on this monorepo.
  Instead:
  - read files directly by path;
  - use the `search_files` MCP tool;
  - use `sl log <file>` or `sl blame`, each with `--reason`.
- Do not spawn subagents, agentctl sessions, Codex, or any other model or
  harness.
- Publish nothing:
  - no comments, diffs, `jf`, `arc diff`, or Phabricator/GitHub writes;
  - no reviewers;
  - no network posting.
- Do not modify anything under `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case` except inside `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/httpcoro/agent-output`.
- If EdenFS shows an infosec or protected-folder warning, answer no. Stop and
  say so in your final report.
- Do not kill processes you did not start.

## S7. Unposted initial comment (verbatim)

<!-- tfa-phase2:abd4f5d24af2f6750cda9d7237aa52024119d556dface89b47fd55f658ed20b0:initial -->
TFA phase-two reproducibility update (artifact-only preview; not posted)
- Selector: `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
- TestID: `562950215319283`; IssueID: `224798678`
- Source revision: `7f239d5b37dfbecf32fed144468da461f998add5`
- Hermit revision: `4630ee2a61f93221a43e2baec0a15f34102a2c61`
- Buck build argv (runs outside Hermit): `buck build @fbcode//mode/dev-asan fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests --show-full-json-output`
  Its `--show-full-json-output` result gives the exact target ELF path.
Current non-`/tmp` reproducers (each command is exactly what was measured; relocation summary sha256 `fca9f308f0dec565fe48cfb9d8148ded7ae58fa90f310493b7ecd829850efa79`):
- PASS reproducer (seed 1; observed PASS on all 2 decided attempts, each in a newly created empty cwd): `mkdir -p /home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd && cd /home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/httpcoro/seed-001-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd,target=/home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd --tmp=/home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-001-attempt-1/guest-tmp --epoch=2026-09-21T21:01:52Z --clock-multiplier=0.5 --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --seed 0 --rng-seed 0 --sched-seed 1 --fuzz-seed 0 --sched-heuristic random --max-timeslice 200000000 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/httpcoro/seed-001-attempt-1/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests --gtest_filter=HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
- FAIL reproducer (seed 0; observed `reset-rate-limit-line1685-body-event-fatal` on all 2 decided attempts, each in a newly created empty cwd): `mkdir -p /home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd && cd /home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/httpcoro/seed-000-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd,target=/home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd --tmp=/home/newton/work/tfa/phase2-cases/httpcoro/tfa-phase2-repro/attempts/seed-000-attempt-1/guest-tmp --epoch=2026-09-21T21:01:52Z --clock-multiplier=0.5 --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --seed 0 --rng-seed 0 --sched-seed 0 --fuzz-seed 0 --sched-heuristic random --max-timeslice 200000000 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/httpcoro/seed-000-attempt-1/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests --gtest_filter=HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
Historical phase-one record (not re-run verbatim; its paths are under `/tmp` and are kept only for provenance):
- Historical working directory: `/tmp/tfa-phase2/cwd/httpcoro`
- Historical PASS command: `/home/newton/work/dev-hermit/bin/safehermit --sh-report /tmp/tfa-phase2/repro/httpcoro/pass/safehermit.json --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/tmp/tfa-phase2/cwd/httpcoro,target=/tmp/tfa-phase2/cwd/httpcoro --epoch=2026-09-21T21:01:52Z --clock-multiplier=0.5 --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --seed 0 --rng-seed 0 --sched-seed 1 --fuzz-seed 0 --sched-heuristic random --max-timeslice 200000000 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/tmp/tfa-phase2/repro/httpcoro/pass/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests --gtest_filter=HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
- Historical FAIL command: `/home/newton/work/dev-hermit/bin/safehermit --sh-report /tmp/tfa-phase2/repro/httpcoro/fail/safehermit.json --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/tmp/tfa-phase2/cwd/httpcoro,target=/tmp/tfa-phase2/cwd/httpcoro --epoch=2026-09-21T21:01:52Z --clock-multiplier=0.5 --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --seed 0 --rng-seed 0 --sched-seed 0 --fuzz-seed 0 --sched-heuristic random --max-timeslice 200000000 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/tmp/tfa-phase2/repro/httpcoro/fail/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests --gtest_filter=HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
- The authoritative record witnessed PASS at seed 3, not the manifest's seed 1.
- Evidence: https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/experiments/tfa-kernel71-httpcoro-midpoint-continuation_20260921/services/proxygen/httpcorosessiontests--e3f69b337304/experiment.json (`experiment.json` sha256 `1d215d2a7a91df380affcfb3b22120b13073fc848290f0a4014c8649724d3f97`)
- OSS Hermit install: https://github.com/rrnewton/hermit/tree/4630ee2a61f93221a43e2baec0a15f34102a2c61
- Required bounded launcher: https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/bin/safehermit
The recorded `/home/newton/...` prefixes are local evidence paths. On another host, substitute your fbsource and dev-hermit roots, use the ELF path returned by Buck, and preserve each same-path mount's identical source and target.
Reproducibility is proven, but root cause is not.


## S8. Deterministic difference hints (verbatim JSON)

```json
{
  "case_id": "httpcoro",
  "fail": {
    "determinism_verified": true,
    "exit_code": 134,
    "failure_class_id": "reset-rate-limit-line1685-body-event-fatal",
    "failures": [
      "F HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException() "
    ],
    "gtest_verdict": "NO_VERDICT_AFTER_RUN",
    "seed": 0,
    "signal": "SIGABRT",
    "top_frames": [
      "proxygen::coro::test::H2DownstreamSessionTest_ResetRateLimit_Test::co_TestBody()::$_1::operator()(proxygen::coro::HTTPSourceHolder) const [clone .resume]",
      "std::__n4861::coroutine_handle<void>::resume() const",
      "folly::resumeCoroutineWithNewAsyncStackRoot(std::__n4861::coroutine_handle<void>, folly::AsyncStackFrame&)",
      "void folly::resumeCoroutineWithNewAsyncStackRoot<folly::coro::detail::TaskPromise<void> >(std::__n4861::coroutine_handle<folly::coro::detail::TaskPromise<void> >)",
      "folly::coro::TaskWithExecutor<void>::Awaiter::await_suspend<folly::coro::detail::InlineTaskDetached::promise_type>(std::__n4861::coroutine_handle<folly::coro::detail::InlineTaskDetached::promise_type>)::{lambda()#1}::operator()()",
      "void folly::detail::function::call_<folly::coro::TaskWithExecutor<void>::Awaiter::await_suspend<folly::coro::detail::InlineTaskDetached::promise_type>(std::__n4861::coroutine_handle<folly::coro::detail::InlineTaskDetached::promise_type>)...",
      "folly::detail::function::FunctionTraits<void ()>::operator()()",
      "folly::EventBase::FuncRunner::operator()(folly::Function<void ()>&&)"
    ],
    "verifier_counts": {
      "DETLOG & scheduler COMMIT messages": 33400,
      "INFO messages": 42909,
      "detcore-specific messages": 42909,
      "messages total": 42910,
      "scheduler COMMIT records reading /proc/self/maps (run 1 first at turn 258, committed virtual time 1790024512159897058ns, run 2 first at turn 258, committed virtual time 1790024512159897058ns)": 42,
      "scheduler empty-run-queue kick messages": 1
    }
  },
  "failure_class": "reset-rate-limit-line1685-body-event-fatal",
  "guest_stderr_diff": [
    "--- pass",
    "+++ fail",
    "@@ -1 +1,161 @@",
    " E HTTP2Codec.cpp:1480] ignore unknown settingsId=4294967926 value=0",
    "+F HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException() ",
    "+*** Check failure stack trace: ***",
    "+*** Aborted at 1790024514 (Unix time, try 'date -d @1790024514') ***",
    "+*** Signal 6 (SIGABRT) (0x?) received by PID 3 (pthread TID 0x?) (linux TID 3) (maybe from PID 3, UID 0) (code: sent by tkill or tgkill), stack trace: ***",
    "+==3==WARNING: ASan doesn't fully support makecontext/swapcontext functions and may produce false positives in some cases!",
    "+    @ 000000000002cf2b folly::symbolizer::(anonymous namespace)::innerSignalHandler(int, siginfo_t*, void*)",
    "+                       ./xplat/folly/debugging/symbolizer/SignalHandler.cpp:598",
    "+    @ 000000000002cbc2 folly::symbolizer::(anonymous namespace)::signalHandler(int, siginfo_t*, void*)",
    "+                       ./xplat/folly/debugging/symbolizer/SignalHandler.cpp:615",
    "+    @ 000000000004455f (unknown)",
    "+                       /home/engshare/third-party2/glibc/2.34/src/glibc-2.34/signal/../sysdeps/unix/sysv/linux/libc_sigaction.c:8",
    "+                       -> /home/engshare/third-party2/glibc/2.34/src/glibc-2.34/signal/../sysdeps/unix/sysv/linux/x86_64/libc_sigaction.c",
    "+    @ 000000000009ca13 pthread_kill",
    "+                       /home/engshare/third-party2/glibc/2.34/src/glibc-2.34/nptl/pthread_kill.c:46",
    "+    @ 00000000000444ac raise",
    "+                       /home/engshare/third-party2/glibc/2.34/src/glibc-2.34/signal/../sysdeps/posix/raise.c:26",
    "+    @ 000000000002c432 abort",
    "+                       /home/engshare/third-party2/glibc/2.34/src/glibc-2.34/stdlib/abort.c:79",
    "+    @ 0000000000008cad folly::(anonymous namespace)::wrapped_abort()",
    "+                       ./xplat/folly/init/Init.cpp:53",
    "+    @ 000000000000c28c google::LogMessage::Fail()",
    "+                       /home/engshare/third-party2/glog/0.3.2_fb/src/glog-0.3.2/src/logging.cc:1519",
    "+    @ 00000000000103b2 google::LogMessage::SendToLog()",
    "+                       /home/engshare/third-party2/glog/0.3.2_fb/src/glog-0.3.2/src/logging.cc:1473",
    "+    @ 000000000000bece google::LogMessage::Flush()",
    "+                       /home/engshare/third-party2/glog/0.3.2_fb/src/glog-0.3.2/src/logging.cc:1346",
    "+    @ 000000000000cbb8 google::LogMessageFatal::~LogMessageFatal()",
    "+                       /home/engshare/third-party2/glog/0.3.2_fb/src/glog-0.3.2/src/logging.cc:2013",
    "+    @ 0000000000eb36c2 proxygen::coro::test::H2DownstreamSessionTest_ResetRateLimit_Test::co_TestBody()::$_1::operator()(proxygen::coro::HTTPSourceHolder) const [clone .resume]",
    "+                       ./fbcode/proxygen/lib/http/coro/test/HTTPDownstreamCoroSessionTests.cpp:1685",
    "+    @ 000000000000b2d3 std::__n4861::coroutine_handle<void>::resume() const",
    "+                       fbcode/third-party-buck/platform010/build/libgcc/include/c++/trunk/coroutine:127",
    "+    @ 000000000000b1bd folly::resumeCoroutineWithNewAsyncStackRoot(std::__n4861::coroutine_handle<void>, folly::AsyncStackFrame&)",
    "+                       ./xplat/folly/tracing/AsyncStack.cpp:218",
    "+    @ 0000000000c2c642 void folly::resumeCoroutineWithNewAsyncStackRoot<folly::coro::detail::TaskPromise<void> >(std::__n4861::coroutine_handle<folly::coro::detail::TaskPromise<void> >)",
    "+                       fbsource/folly/tracing/AsyncStack-inl.h:75",
    "+    @ 0000000000c2c4f9 folly::coro::TaskWithExecutor<void>::Awaiter::await_suspend<folly::coro::detail::InlineTaskDetached::promise_type>(std::__n4861::coroutine_handle<folly::coro::detail::InlineTaskDetached::promise_type>)::{lambda()#...",
    "+                       fbsource/folly/coro/Task.h:517",
    "+    @ 0000000000c2c37c void folly::detail::function::call_<folly::coro::TaskWithExecutor<void>::Awaiter::await_suspend<folly::coro::detail::InlineTaskDetached::promise_type>(std::__n4861::coroutine_handle<folly::coro::detail::InlineTask...",
    "+                       fbsource/folly/Function.h:341",
    "+    @ 000000000009c252 folly::detail::function::FunctionTraits<void ()>::operator()()",
    "+                       fbsource/folly/Function.h:370",
    "+    @ 000000000009bed4 folly::EventBase::FuncRunner::operator()(folly::Function<void ()>&&)",
    "+                       ./xplat/folly/io/async/EventBase.cpp:233",
    "+    @ 000000000009aecb folly::AtomicNotificationQueueTaskStatus folly::detail::invokeConsumerWithTask<folly::Function<void ()>, folly::EventBase::FuncRunner&>(folly::EventBase::FuncRunner&, folly::Function<void ()>&&, std::shared_ptr<fo...",
    "+                       fbsource/folly/io/async/AtomicNotificationQueue-inl.h:258",
    "+    @ 000000000009aab5 bool folly::AtomicNotificationQueue<folly::Function<void ()> >::drive<folly::EventBase::FuncRunner&>(folly::EventBase::FuncRunner&)",
    "+                       fbsource/folly/io/async/AtomicNotificationQueue-inl.h:328",
    "+    @ 000000000009a047 bool folly::EventBaseAtomicNotificationQueue<folly::Function<void ()>, folly::EventBase::FuncRunner>::drive<folly::EventBase::FuncRunner&>(folly::EventBase::FuncRunner&)",
    "+                       fbsource/folly/io/async/EventBaseAtomicNotificationQueue-inl.h:275",
    "+    @ 00000000000c0e99 folly::EventBaseAtomicNotificationQueue<folly::Function<void ()>, folly::EventBase::FuncRunner>::execute()",
    "+                       fbsource/folly/io/async/EventBaseAtomicNotificationQueue-inl.h:294",
    "+    @ 00000000000da08b folly::EventBaseAtomicNotificationQueue<folly::Function<void ()>, folly::EventBase::FuncRunner>::handlerReady(unsigned short)",
    "+                       fbsource/folly/io/async/EventBaseAtomicNotificationQueue-inl.h:286",
    "+    @ 00000000001062b9 folly::EventHandler::libeventCallback(int, short, void*)",
    "+                       ./xplat/folly/io/async/EventHandler.cpp:165"
  ],
  "guest_stderr_diff_truncated": true,
  "guest_stderr_same_lines_reordered": false,
  "inputs": {
    "fail.result": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro/evidence/selection-000-sched-seed-00000000000000000000-replica-1.result.json",
      "sha256": "e4fadc3b8d88a19a32c101d09b4476d63041f411a5c039dd534ae99c876158f9",
      "size": 2180
    },
    "fail.stderr": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro/evidence/selection-000-sched-seed-00000000000000000000-replica-1.stderr.txt",
      "sha256": "d2d9d0c2a1bd9132b6b8084e931de8afdcff06bc1a61a81bbc61bcb430e8b4cb",
      "size": 17546
    },
    "fail.stdout": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro/evidence/selection-000-sched-seed-00000000000000000000-replica-1.stdout.txt",
      "sha256": "64701a7c4b67e570f3333c5c0da2fbdcd7a53921ed1c5c69e47750f1f5f78058",
      "size": 343
    },
    "pass.result": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro/evidence/selection-003-sched-seed-00000000000000000003-replica-1.result.json",
      "sha256": "5724d454fe834b6925ec51b0b9a8cb9355970ab0ad39c389e0be1e6f36e49b17",
      "size": 2129
    },
    "pass.stderr": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro/evidence/selection-003-sched-seed-00000000000000000003-replica-1.stderr.txt",
      "sha256": "b7e6265efac07bad5cb46cedcf013c0bb8a0355a6bcb658f04a75481467b2246",
      "size": 1335
    },
    "pass.stdout": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/httpcoro/evidence/selection-003-sched-seed-00000000000000000003-replica-1.stdout.txt",
      "sha256": "2c7a2dc130ff736194e6f6e3074c809a7a1920a5da9af3f7aef74d6081847576",
      "size": 650
    }
  },
  "pass": {
    "determinism_verified": true,
    "exit_code": 0,
    "failure_class_id": null,
    "failures": [],
    "gtest_verdict": "OK",
    "seed": 3,
    "signal": null,
    "top_frames": [],
    "verifier_counts": {
      "DETLOG & scheduler COMMIT messages": 13612,
      "INFO messages": 14798,
      "detcore-specific messages": 14797,
      "messages total": 14798,
      "scheduler COMMIT records reading /proc/self/maps (run 1 first at turn 258, committed virtual time 1790024512094600840ns, run 2 first at turn 258, committed virtual time 1790024512094600840ns)": 42,
      "scheduler empty-run-queue kick messages": 1
    }
  },
  "qualification": "deterministic-text-difference; not a root cause",
  "schema_version": 1,
  "verifier_count_delta": {
    "DETLOG & scheduler COMMIT messages": 19788,
    "INFO messages": 28111,
    "detcore-specific messages": 28112,
    "messages total": 28112,
    "scheduler empty-run-queue kick messages": 0
  }
}
```
