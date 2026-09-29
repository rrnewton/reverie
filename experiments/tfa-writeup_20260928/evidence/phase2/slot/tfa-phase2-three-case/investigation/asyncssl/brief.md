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
  "authoritative_record": "https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/experiments/tfa-kernel71-asyncssl-slice5-continuation_20260921/services/folly/async-ssl-socket-test--cefc173a09b4/experiment.json",
  "build_argv": [
    "buck",
    "build",
    "@fbcode//mode/libcxx",
    "fbcode//folly/io/async/test:async_ssl_socket_test",
    "--show-full-json-output"
  ],
  "case_id": "asyncssl",
  "checkout": "/home/newton/work/tfa/phase2-cases/investigators/asyncssl",
  "controller_fail_command": "buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest fbcode/testinfra/tfa/cases.v1.json --case-id asyncssl --outcome fail --artifact-root /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output",
  "controller_pass_command": "buck run fbcode//testinfra/tfa:tfa_phase2 -- run-witness --manifest fbcode/testinfra/tfa/cases.v1.json --case-id asyncssl --outcome pass --artifact-root /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output",
  "controller_working_directory": "create a fresh <attempt-artifact-dir>/controller-cwd per invocation",
  "evidence_paths": {
    "hypotheses": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/hypotheses.json",
    "patch": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/patch.diff",
    "report": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/report.json",
    "validation": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/validation.json"
  },
  "fail_seed": 11,
  "failure_class": "gtest-byte-events-three-missing-ack",
  "fresh_stress_seeds": [
    12,
    13,
    14,
    15
  ],
  "hermit_revision": "4630ee2a61f93221a43e2baec0a15f34102a2c61",
  "historical_fail_seed": 11,
  "historical_pass_seed": 1,
  "issue_id": "224226669",
  "pass_seed": 1,
  "selector": "fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect",
  "source_revision": "7f239d5b37dfbecf32fed144468da461f998add5",
  "test_argv": [
    "buck",
    "test",
    "@fbcode//mode/libcxx",
    "fbcode//folly/io/async/test:async_ssl_socket_test",
    "--",
    "--exact",
    "fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect",
    "--run-disabled"
  ],
  "test_id": "562950211052906",
  "timeout_layers_seconds": {
    "hermit": 110,
    "outer_minimum": 130,
    "safehermit": 120
  },
  "working_directory": "/tmp/tfa-phase2/cwd/asyncssl"
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
- You are the only investigator for case `asyncssl`. Two other Claude Opus 5.5
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
    sched-seed 11. Each was confirmed on 2 decided attempts, with 0
    censored.
  - Relocation summary: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/asyncssl/summary.json` (sha256 `e4f43e4534eeb764774ce1be5b29719c5c622c80a06a326f7a4cba9278936404`).
- Do not re-run those commands verbatim. Their report and verify-log paths are
  preserved evidence and must not be overwritten.
- To reproduce, use the controller replay runner. It replays the measured
  witness argv. It rewrites only the attempt paths (the cwd same-path mount,
  `--tmp`, `--sh-report` and `--verify-log-dir`) into a new directory you name,
  creates a fresh owned empty `controller-cwd` there, verifies the pinned
  safehermit, Hermit and ELF hashes, classifies the run, and writes
  `receipt.json`:

      /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/witness.sh --case asyncssl --template pass --out /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/witness/<new-label>
      /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/witness/witness.sh --case asyncssl --template fail --out /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/witness/<new-label>

  Use a new label for every attempt; the runner refuses an existing directory.
  Optional flags:
  - `--seed N` replays the same argv with a different `--sched-seed`. Do not
    use the fresh stress seeds `[12, 13, 14, 15]`. They are reserved for the
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
- Your own writable checkout is `/home/newton/work/tfa/phase2-cases/investigators/asyncssl`. It is an Eden clone at
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
- When your fix is final, write it to `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/patch.diff` with
  `sl diff --reason "export TFA asyncssl draft fix - sl help diff" > /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output/patch.diff`
  run in your checkout. It must match your checkout's changes exactly. The
  controller will apply `patch.diff` in its own separate checkout.
- Do not invoke Buck or Arc, as the fixed prompt says. The controller builds,
  then stress-tests the fixed ELF against both witness schedules and the fresh
  stress seeds after you finish.

## S4. Deterministic inputs
- The difference hints and the unposted initial comment are reproduced
  verbatim below. Their files are:
  - `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stages/asyncssl/stage1-2-relocated/difference-hints.json` (sha256 `8936689c859bb0a18100b7b7ea7730ab0e1f99dc0874bc1110e865bac7ae7ac5`)
  - `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stages/asyncssl/stage1-2-relocated/initial-comment.md` (sha256 `ce8cf33db4a20a70a5971f181aad43bdc6c44d058a3bdafb468fcfcdaa9f99d5`)
- Historical phase-one evidence, read-only: `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl`.
- Report schema: `/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/report.schema.v1.json`.

## S5. Outputs
- Write these into `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output`:
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
- Set `TMPDIR=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/tmpdir` on every shell command. Never write under `/tmp`.
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
- Do not modify anything under `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case` except inside `/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/investigation/asyncssl/agent-output`.
- If EdenFS shows an infosec or protected-folder warning, answer no. Stop and
  say so in your final report.
- Do not kill processes you did not start.

## S7. Unposted initial comment (verbatim)

<!-- tfa-phase2:02e283e14aff4e440db1adfba5feda6e7a1969b76cc75dc79981bc1577038673:initial -->
TFA phase-two reproducibility update (artifact-only preview; not posted)
- Selector: `fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect`
- TestID: `562950211052906`; IssueID: `224226669`
- Source revision: `7f239d5b37dfbecf32fed144468da461f998add5`
- Hermit revision: `4630ee2a61f93221a43e2baec0a15f34102a2c61`
- Buck build argv (runs outside Hermit): `buck build @fbcode//mode/libcxx fbcode//folly/io/async/test:async_ssl_socket_test --show-full-json-output`
  Its `--show-full-json-output` result gives the exact target ELF path.
Current non-`/tmp` reproducers (each command is exactly what was measured; relocation summary sha256 `e4f43e4534eeb764774ce1be5b29719c5c622c80a06a326f7a4cba9278936404`):
- PASS reproducer (seed 1; observed PASS on all 2 decided attempts, each in a newly created empty cwd): `mkdir -p /home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd && cd /home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/asyncssl/seed-001-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=8.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd,target=/home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-001-attempt-1/cwd --tmp=/home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-001-attempt-1/guest-tmp --epoch=2026-09-21T22:13:10Z --seed 0 --rng-seed 0 --sched-seed 1 --fuzz-seed 0 --sched-heuristic random --target-timeslice 5000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/asyncssl/seed-001-attempt-1/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test --gtest_filter=AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --gtest_also_run_disabled_tests`
- FAIL reproducer (seed 11; observed `gtest-byte-events-three-missing-ack` on all 2 decided attempts, each in a newly created empty cwd): `mkdir -p /home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-011-attempt-1/cwd && cd /home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-011-attempt-1/cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/asyncssl/seed-011-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=8.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-011-attempt-1/cwd,target=/home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-011-attempt-1/cwd --tmp=/home/newton/work/tfa/phase2-cases/asyncssl/tfa-phase2-repro/attempts/seed-011-attempt-1/guest-tmp --epoch=2026-09-21T22:13:10Z --seed 0 --rng-seed 0 --sched-seed 11 --fuzz-seed 0 --sched-heuristic random --target-timeslice 5000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/relocated-v2/asyncssl/seed-011-attempt-1/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test --gtest_filter=AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --gtest_also_run_disabled_tests`
Historical phase-one record (not re-run verbatim; its paths are under `/tmp` and are kept only for provenance):
- Historical working directory: `/tmp/tfa-phase2/cwd/asyncssl`
- Historical PASS command: `/home/newton/work/dev-hermit/bin/safehermit --sh-report /tmp/tfa-phase2/repro/asyncssl/pass/safehermit.json --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=8.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/tmp/tfa-phase2/cwd/asyncssl,target=/tmp/tfa-phase2/cwd/asyncssl --epoch=2026-09-21T22:13:10Z --seed 0 --rng-seed 0 --sched-seed 1 --fuzz-seed 0 --sched-heuristic random --target-timeslice 5000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/tmp/tfa-phase2/repro/asyncssl/pass/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test --gtest_filter=AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --gtest_also_run_disabled_tests`
- Historical FAIL command: `/home/newton/work/dev-hermit/bin/safehermit --sh-report /tmp/tfa-phase2/repro/asyncssl/fail/safehermit.json --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=8.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/fbsource/buck-out,target=/home/newton/work/tfa/fbsource/buck-out --mount=type=bind,source=/tmp/tfa-phase2/cwd/asyncssl,target=/tmp/tfa-phase2/cwd/asyncssl --epoch=2026-09-21T22:13:10Z --seed 0 --rng-seed 0 --sched-seed 11 --fuzz-seed 0 --sched-heuristic random --target-timeslice 5000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/tmp/tfa-phase2/repro/asyncssl/fail/verify -- /home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test --gtest_filter=AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --gtest_also_run_disabled_tests`
- Evidence: https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/experiments/tfa-kernel71-asyncssl-slice5-continuation_20260921/services/folly/async-ssl-socket-test--cefc173a09b4/experiment.json (`experiment.json` sha256 `b1138c6651ff2d2f6989f2f934307b08c81b87d6c94ca65f030fa8195589d93e`)
- OSS Hermit install: https://github.com/rrnewton/hermit/tree/4630ee2a61f93221a43e2baec0a15f34102a2c61
- Required bounded launcher: https://github.com/rrnewton/dev-hermit/blob/edd968317a0986ccfff0bb29ef924d7c5435ce8c/bin/safehermit
The recorded `/home/newton/...` prefixes are local evidence paths. On another host, substitute your fbsource and dev-hermit roots, use the ELF path returned by Buck, and preserve each same-path mount's identical source and target.
Reproducibility is proven, but root cause is not.


## S8. Deterministic difference hints (verbatim JSON)

```json
{
  "case_id": "asyncssl",
  "fail": {
    "determinism_verified": true,
    "exit_code": 1,
    "failure_class_id": "gtest-byte-events-three-missing-ack",
    "failures": [
      "fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp:3797: Failure\nValue of: observer->byteEvents\nExpected: has a size that is >= 4\n  Actual: { 96-byte object <00-00 00-00 00-00 00-00 F5-01 00-00 00-00 00-00 CB-70 88-7B B3-75 D7-18 00-BE BE-BE BE-BE BE-BE 00-BE BE-BE BE-BE BE-BE 00-BE BE-BE BE-BE BE-BE 00-BE BE-BE BE-BE BE-BE 17-00 00-00 00-00 00-00 01-BE BE-BE BE-BE...",
      "fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp:3812: Failure\nExpected equality of these values:\n  offsetExpected\n    Which is: 501\n  observer->maxOffsetForByteEventReceived(ByteEventType::ACK)\n    Which is: 16-byte object <00-00 00-00 00-00 00-00 00-00 00-00 00-00 00-00>"
    ],
    "gtest_verdict": "FAILED",
    "seed": 11,
    "signal": null,
    "top_frames": [],
    "verifier_counts": {
      "DETLOG & scheduler COMMIT messages": 8667,
      "INFO messages": 9123,
      "detcore-specific messages": 9121,
      "messages total": 9123,
      "scheduler COMMIT records reading /proc/self/maps (run 1 first at turn 221, committed virtual time 1790028790214849200ns, run 2 first at turn 221, committed virtual time 1790028790214849200ns)": 42,
      "scheduler empty-run-queue kick messages": 1
    }
  },
  "failure_class": "gtest-byte-events-three-missing-ack",
  "guest_stderr_diff": [
    "--- pass",
    "+++ fail",
    "@@ -3,4 +3,4 @@",
    " SSLServerAcceptCallback::connAccepted",
    "+HandshakeCallback::connectionAccepted",
    " writeSuccess",
    "-HandshakeCallback::connectionAccepted",
    " readDataAvailable, len 1"
  ],
  "guest_stderr_diff_truncated": false,
  "guest_stderr_same_lines_reordered": true,
  "inputs": {
    "fail.result": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl/evidence/selection-011-sched-seed-00000000000000000011-replica-1.result.json",
      "sha256": "c04e71b80ea09b14b5858c766bd0c40199e426fba801705d3f3cf01896a3d1ca",
      "size": 2263
    },
    "fail.stderr": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl/evidence/selection-011-sched-seed-00000000000000000011-replica-1.stderr.txt",
      "sha256": "10b4598e6e3458238767e592e2e1d98e1880d3b67d272b589b74298c4fbe932b",
      "size": 1765
    },
    "fail.stdout": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl/evidence/selection-011-sched-seed-00000000000000000011-replica-1.stdout.txt",
      "sha256": "25a6177327a3766309b2caa7ce33ddab0b6a709f5217ff1a7e07508f6bd3eab9",
      "size": 2064
    },
    "pass.result": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl/evidence/selection-001-sched-seed-00000000000000000001-replica-1.result.json",
      "sha256": "9d7de447d1dcfa5d5a393ab4b065eab298391e3d55be7d76338acd95f01c7a21",
      "size": 2226
    },
    "pass.stderr": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl/evidence/selection-001-sched-seed-00000000000000000001-replica-1.stderr.txt",
      "sha256": "71fa37b9487c5719b906d7855b0f7c442b9698b975d36cd4116dae3b3c139e65",
      "size": 1765
    },
    "pass.stdout": {
      "path": "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/authoritative/asyncssl/evidence/selection-001-sched-seed-00000000000000000001-replica-1.stdout.txt",
      "sha256": "ee0966d97628864f983a2a4773f00b4de6d1dc6f38e2a4518bed56b86a24877f",
      "size": 576
    }
  },
  "pass": {
    "determinism_verified": true,
    "exit_code": 0,
    "failure_class_id": null,
    "failures": [],
    "gtest_verdict": "OK",
    "seed": 1,
    "signal": null,
    "top_frames": [],
    "verifier_counts": {
      "DETLOG & scheduler COMMIT messages": 8704,
      "INFO messages": 9167,
      "detcore-specific messages": 9165,
      "messages total": 9167,
      "scheduler COMMIT records reading /proc/self/maps (run 1 first at turn 229, committed virtual time 1790028790207421713ns, run 2 first at turn 229, committed virtual time 1790028790207421713ns)": 42,
      "scheduler empty-run-queue kick messages": 1
    }
  },
  "qualification": "deterministic-text-difference; not a root cause",
  "schema_version": 1,
  "verifier_count_delta": {
    "DETLOG & scheduler COMMIT messages": -37,
    "INFO messages": -44,
    "detcore-specific messages": -44,
    "messages total": -44,
    "scheduler empty-run-queue kick messages": 0
  }
}
```
