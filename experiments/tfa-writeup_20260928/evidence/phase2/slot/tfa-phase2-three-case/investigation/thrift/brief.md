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
  "authoritative_record": "https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/experiments/tfa-kernel71-thrift-slowdown-continuation_20260921/services/thrift/instrumentationtest--3e07b8fc0ca0/experiment.json",
  "build_argv": [
    "buck",
    "build",
    "@fbcode//mode/dev-tsan",
    "fbcode//thrift/lib/cpp2/test:InstrumentationTest",
    "--show-full-json-output"
  ],
  "case_id": "thrift",
  "checkout": "/home/newton/work/tfa/phase2-cases/investigators/thrift",
  "controller_fail_command": "buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest fbcode/testinfra/tfa/cases.v1.json --case-id thrift --outcome fail --artifact-root /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output",
  "controller_pass_command": "buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest fbcode/testinfra/tfa/cases.v1.json --case-id thrift --outcome pass --artifact-root /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output",
  "controller_working_directory": "create a fresh <attempt-artifact-dir>/controller-cwd per invocation",
  "evidence_paths": {
    "hypotheses": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/hypotheses.json",
    "patch": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/patch.diff",
    "report": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/report.json",
    "validation": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/validation.json"
  },
  "fail_seed": 1,
  "failure_class": "gtest-snapshot-size-2",
  "fresh_stress_seeds": [
    2,
    3,
    4,
    5
  ],
  "hermit_revision": "4630ee2a61f93221a43e2baec0a15f34102a2c61",
  "historical_fail_seed": 1,
  "historical_pass_seed": 0,
  "issue_id": null,
  "pass_seed": 0,
  "selector": "fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest",
  "source_revision": "7f239d5b37dfbecf32fed144468da461f998add5",
  "test_argv": [
    "buck",
    "test",
    "@fbcode//mode/dev-tsan",
    "fbcode//thrift/lib/cpp2/test:InstrumentationTest",
    "--",
    "--exact",
    "fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest"
  ],
  "test_id": "281475235299286",
  "timeout_layers_seconds": {
    "hermit": 110,
    "outer_minimum": 130,
    "safehermit": 120
  },
  "working_directory": "/tmp/tfa-phase2/cwd/thrift"
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
- You are the only investigator for case `thrift`. Two other Claude Opus 5.5
  sessions investigate the other cases in their own checkouts. Do not read,
  write, or run anything for them.

## S2. Witness commands (overrides `controller_pass_command`, `controller_fail_command`, `working_directory`)
- Do not use `controller_*_command` from the fixed facts. It routes through the
  packaged-Hermit `buck run ... run-witness` path, which is blocked here.
- Do not use the fixed `working_directory` or any other path under `/tmp`. The
  owner has forbidden all execution under `/tmp`.
- The measured non-`/tmp` PASS and FAIL reproducers for this case are in the
  unposted initial comment below.
  - The PASS witness is sched-seed 0 and the FAIL witness is
    sched-seed 1. Each was confirmed on 2 decided attempts, with 0
    censored.
  - Relocation summary: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/thrift/summary.json` (sha256 `00e9a2045a44246579ddaa322408f42c8e105c3a73c132e58b69a73d45663aa2`).
- Do not re-run those commands verbatim. Their report and verify-log paths are
  preserved evidence and must not be overwritten.
- To reproduce, use the controller replay runner. It replays the measured
  witness argv. It rewrites only the attempt paths (the cwd same-path mount,
  `--tmp`, `--sh-report` and `--verify-log-dir`) into a new directory you name,
  creates a fresh owned empty `controller-cwd` there, verifies the pinned
  safehermit, Hermit and ELF hashes, classifies the run, and writes
  `receipt.json`:

      /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/witness.sh --case thrift --template pass --out /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/witness/<new-label>
      /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/witness.sh --case thrift --template fail --out /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/witness/<new-label>

  Use a new label for every attempt; the runner refuses an existing directory.
  Optional flags:
  - `--seed N` replays the same argv with a different `--sched-seed`. Do not
    use the fresh stress seeds `[2, 3, 4, 5]`. They are reserved for the
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
- Your own writable checkout is `/home/newton/work/tfa/phase2-cases/investigators/thrift`. It is an Eden clone at
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
- When your fix is final, write it to `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/patch.diff` with
  `sl diff --reason "export TFA thrift draft fix - sl help diff" > /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output/patch.diff`
  run in your checkout. It must match your checkout's changes exactly. The
  controller will apply `patch.diff` in its own separate checkout.
- Do not invoke Buck or Arc, as the fixed prompt says. The controller builds,
  then stress-tests the fixed ELF against both witness schedules and the fresh
  stress seeds after you finish.

## S4. Deterministic inputs
- The difference hints and the unposted initial comment are reproduced
  verbatim below. Their files are:
  - `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stages/thrift/stage1-2-relocated/difference-hints.json` (sha256 `76f1e5f0c6c77af4d80d3ca43ff857f4422499ebd30ef961dcd92f1819b0c4dc`)
  - `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stages/thrift/stage1-2-relocated/initial-comment.md` (sha256 `72c5ee9c133e025cfb8fd55452d632e3b37b08f521136e94660053420186a858`)
- Historical phase-one evidence, read-only: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift`.
- Report schema: `/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/report.schema.v1.json`.

## S5. Outputs
- Write these into `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output`:
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
- Set `TMPDIR=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/tmpdir` on every shell command. Never write under `/tmp`.
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
- Do not modify anything under `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case` except inside `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/thrift/agent-output`.
- If EdenFS shows an infosec or protected-folder warning, answer no. Stop and
  say so in your final report.
- Do not kill processes you did not start.

## S7. Unposted initial comment (verbatim)

<!-- tfa-phase2:cd46934a017713ee49c4ec4b019ee1122d1564133462abec6c8744c21e9d8044:initial -->
TFA phase-two reproducibility update (artifact-only preview; not posted)
- Selector: `fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- TestID: `281475235299286`; IssueID: `none (no exact FLAKY issue)`
- Source revision: `7f239d5b37dfbecf32fed144468da461f998add5`
- Hermit revision: `4630ee2a61f93221a43e2baec0a15f34102a2c61`
- Buck build argv (runs outside Hermit): `buck build @fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:InstrumentationTest --show-full-json-output`
  Its `--show-full-json-output` result gives the exact target ELF path.
Current non-`/tmp` reproducers (each command is exactly what was measured; relocation summary sha256 `00e9a2045a44246579ddaa322408f42c8e105c3a73c132e58b69a73d45663aa2`):
- PASS reproducer (seed 0; observed PASS on all 2 decided attempts, each in a newly created empty cwd): `mkdir -p /home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd && cd /home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/thrift/seed-000-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd,target=/home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-000-attempt-1/cwd --tmp=/home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-000-attempt-1/guest-tmp --epoch=2026-09-21T21:44:37Z --seed 0 --rng-seed 0 --sched-seed 0 --fuzz-seed 0 --sched-heuristic random --target-timeslice 1000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/thrift/seed-000-attempt-1/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest --gtest_filter=RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- FAIL reproducer (seed 1; observed `gtest-snapshot-size-2` on all 2 decided attempts, each in a newly created empty cwd): `mkdir -p /home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd && cd /home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/thrift/seed-001-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd,target=/home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd --tmp=/home/newton/work/tfa/phase2-cases/thrift/tfa-phase2-repro/attempts/seed-001-attempt-1/guest-tmp --epoch=2026-09-21T21:44:37Z --seed 0 --rng-seed 0 --sched-seed 1 --fuzz-seed 0 --sched-heuristic random --target-timeslice 1000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/thrift/seed-001-attempt-1/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest --gtest_filter=RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
Historical phase-one record (not re-run verbatim; its paths are under `/tmp` and are kept only for provenance):
- Historical working directory: `/tmp/tfa-phase2/cwd/thrift`
- Historical PASS command: `/home/newton/work/dev-hermit/bin/safehermit --sh-report /tmp/tfa-phase2/repro/thrift/pass/safehermit.json --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/tmp/tfa-phase2/cwd/thrift,target=/tmp/tfa-phase2/cwd/thrift --epoch=2026-09-21T21:44:37Z --seed 0 --rng-seed 0 --sched-seed 0 --fuzz-seed 0 --sched-heuristic random --target-timeslice 1000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/tmp/tfa-phase2/repro/thrift/pass/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest --gtest_filter=RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- Historical FAIL command: `/home/newton/work/dev-hermit/bin/safehermit --sh-report /tmp/tfa-phase2/repro/thrift/fail/safehermit.json --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/tmp/tfa-phase2/cwd/thrift,target=/tmp/tfa-phase2/cwd/thrift --epoch=2026-09-21T21:44:37Z --seed 0 --rng-seed 0 --sched-seed 1 --fuzz-seed 0 --sched-heuristic random --target-timeslice 1000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/tmp/tfa-phase2/repro/thrift/fail/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest --gtest_filter=RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- Evidence: https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/experiments/tfa-kernel71-thrift-slowdown-continuation_20260921/services/thrift/instrumentationtest--3e07b8fc0ca0/experiment.json (`experiment.json` sha256 `96d82e9103df3ea0d30b2eeade1e591b87c15d5b8a1dd032ada02e59274a6162`)
- OSS Hermit install: https://github.com/rrnewton/hermit/tree/4630ee2a61f93221a43e2baec0a15f34102a2c61
- Required bounded launcher: https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/bin/safehermit
The recorded `/home/newton/...` prefixes are local evidence paths. On another host, substitute your fbsource and dev-hermit roots, use the ELF path returned by Buck, and preserve each same-path mount's identical source and target.
Reproducibility is proven, but root cause is not.


## S8. Deterministic difference hints (verbatim JSON)

```json
{
  "case_id": "thrift",
  "fail": {
    "determinism_verified": true,
    "exit_code": 1,
    "failure_class_id": "gtest-snapshot-size-2",
    "failures": [
      "fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp:836: Failure\nExpected equality of these values:\n  reqSnapshots.size()\n    Which is: 2\n  3"
    ],
    "gtest_verdict": "FAILED",
    "seed": 1,
    "signal": null,
    "top_frames": [],
    "verifier_counts": {
      "DETLOG & scheduler COMMIT messages": 30452,
      "INFO messages": 38937,
      "detcore-specific messages": 38923,
      "messages total": 38937,
      "scheduler COMMIT records reading /proc/self/maps (run 1 first at turn 800, committed virtual time 1790027077571351040ns, run 2 first at turn 800, committed virtual time 1790027077571351040ns)": 82,
      "scheduler empty-run-queue kick messages": 1
    }
  },
  "failure_class": "gtest-snapshot-size-2",
  "guest_stderr_diff": [],
  "guest_stderr_diff_truncated": false,
  "guest_stderr_same_lines_reordered": false,
  "inputs": {
    "fail.result": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift/evidence/selection-001-sched-seed-00000000000000000001-replica-1.result.json",
      "sha256": "bc7df483b6f8957c33a0f919f71961514c3a837527351efcf11897fe887afc32",
      "size": 2195
    },
    "fail.stderr": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift/evidence/selection-001-sched-seed-00000000000000000001-replica-1.stderr.txt",
      "sha256": "85f03e9d5b46c519120cc647a866feeba19cf2d12af443e4bf1269766ca3e296",
      "size": 1660
    },
    "fail.stdout": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift/evidence/selection-001-sched-seed-00000000000000000001-replica-1.stdout.txt",
      "sha256": "c8a68dbd3d417f0259f338d71bac761d7fe54682fbb8103e0180b0b22fdb7d8d",
      "size": 983
    },
    "pass.result": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift/evidence/selection-000-sched-seed-00000000000000000000-replica-1.result.json",
      "sha256": "ea034d80afb978c68c2ad649a383c1aa685346140b45cbae1db6bce6ea622e23",
      "size": 2174
    },
    "pass.stderr": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift/evidence/selection-000-sched-seed-00000000000000000000-replica-1.stderr.txt",
      "sha256": "305f7db233675d0f477f2bd73cd7a9d917f681f791a25d7296ec84c587b0ff5d",
      "size": 1660
    },
    "pass.stdout": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/thrift/evidence/selection-000-sched-seed-00000000000000000000-replica-1.stdout.txt",
      "sha256": "a403e806173b58c452100babc08ccccadfdaffbc04fbee77dde192d907cfece9",
      "size": 694
    }
  },
  "pass": {
    "determinism_verified": true,
    "exit_code": 0,
    "failure_class_id": null,
    "failures": [],
    "gtest_verdict": "OK",
    "seed": 0,
    "signal": null,
    "top_frames": [],
    "verifier_counts": {
      "DETLOG & scheduler COMMIT messages": 29189,
      "INFO messages": 34876,
      "detcore-specific messages": 34862,
      "messages total": 34876,
      "scheduler COMMIT records reading /proc/self/maps (run 1 first at turn 912, committed virtual time 1790027077877067741ns, run 2 first at turn 912, committed virtual time 1790027077877067741ns)": 82,
      "scheduler empty-run-queue kick messages": 1
    }
  },
  "qualification": "deterministic-text-difference; not a root cause",
  "schema_version": 1,
  "verifier_count_delta": {
    "DETLOG & scheduler COMMIT messages": 1263,
    "INFO messages": 4061,
    "detcore-specific messages": 4061,
    "messages total": 4061,
    "scheduler empty-run-queue kick messages": 0
  }
}
```
