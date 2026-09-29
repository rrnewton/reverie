# TFA phase two: candidate test fixes for three Hermit-reproduced flaky tests

Status: artifact-only. Nothing here was posted, no Diff was created, and no
reviewer was assigned. Paths are relative to this directory
(`/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case`,
written `$E3` below) unless they are absolute. The validator and judging code
are at fbsource commit `c1f8817a5ebb`. The reporting changes described in
section 9 are in its child `b8b85d00462b`.

## Abstract

For three fbcode tests that phase one had reproduced as PASS/FAIL under
Hermit, an investigator agent proposed one ranked root-cause hypothesis
set and one test-only candidate patch per case. The controller then built
each patched test and replayed it under the same Hermit configuration on the
manifest FAIL and PASS seeds, on path-local FAIL seeds, and on fresh seeds.
In all three cases every decided patched replay PASSed (thrift 13/13,
asyncssl 8/9 with 1 OTHER, httpcoro 8/8), while the unfixed ELF FAILed on
at least one seed (thrift 5/17, asyncssl 5/14, httpcoro 3/9). By the rule in
section 2.4 all three are SUPPORTED. SUPPORTED means only that the patched
replays did not reproduce the FAIL. The root causes are investigator
hypotheses that were not instrumented. The samples are 1 to 4 replays per
seed. There is no native, CI or landed evidence beyond one
non-discriminating native `buck test` run per case. The separate production
containment suite is BLOCKED (section 7) and says nothing about these fixes.

## 1. Question

For each case, does the investigator's candidate patch remove the Hermit FAIL
without breaking the Hermit PASS? And how much does that tell us about the
proposed root cause?

| case | test (manifest selector) | build mode | manifest PASS / FAIL seed |
| --- | --- | --- | --- |
| thrift | `fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest` | `@fbcode//mode/dev-tsan` | 0 / 1 |
| asyncssl | `fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect` (`--run-disabled`) | `@fbcode//mode/libcxx` | 1 / 11 |
| httpcoro | `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2` | `@fbcode//mode/dev-asan` | 1 / 0 |

All cases use fbsource revision `7f239d5b37dfbecf32fed144468da461f998add5`.
The manifest is `fbcode/testinfra/tfa/cases.v1.json`.

## 2. Method

### 2.1 Reproduction (stages 1-2)

The phase-one PASS and FAIL witnesses were relocated to non-temporary paths
and re-measured. The initial comments are in
`stages/<case>/stage1-2-relocated/initial-comment.md`. Each one gives the exact
measured PASS and FAIL commands and states "Reproducibility is proven, but root
cause is not."

### 2.2 Investigation (stage 3 input)

One agent per case (Claude harness, model `claude-opus-5-5`, session
`tfa-inv-<case>`) worked in its own checkout,
`/home/newton/work/tfa/phase2-cases/investigators/<case>`. Each agent received
the same fixed prompt plus a case brief (`investigation/<case>/fixed-prompt.txt`,
`brief.md`). Agents were not allowed to build. They could read code and replay
the existing witnesses. Each returned four ranked hypotheses, a selected
hypothesis (rank 1 in all three cases), a root-cause statement and a patch
(`investigation/<case>/agent-output/`). The reports are candidate-only.

In `validation.json` and in each report's `validations`, every proposed
validation is recorded as `status FAIL, passed 0, total 0`, with
`original_fail_now_passes` and `original_pass_still_passes` false. This
encoding means NOT RUN, because the investigators could not build. It does not
mean that a validation failed.

### 2.3 Post-fix stress (controller)

Each case had an isolated, clean stress checkout at the same revision,
`/home/newton/work/tfa/phase2-cases/stress/<case>`. `stress/postfix_20260925.sh`
ran the following steps once per case:

1. `sl import --no-commit` the candidate patch.
2. Build with the manifest `build_argv`, then record the ELF sha256 and check
   the ELF path against the build output.
3. Replay the patched ELF with `witness/witness.sh` for each line of
   `stress/<case>/postfix-plan.txt`.
4. Run the manifest `test_argv` natively (`unit.out`) and run `arc lint` on the
   patched files (`lint.out`).
5. Run `sl revert --all --no-backup` and rebuild. Then check that the rebuilt ELF
   sha256 equals the manifest unfixed sha256, and replay one revert control
   (`revert-plan.txt`).

The unfixed replays in `stress/<case>/{prefix,prefix-fresh,prefix-extra}` were
taken earlier on the same unfixed ELF.

Hermit settings:

- Hermit binary `4630ee2a` (sha256 `5c422b85…`), under `safehermit` with
  `--sh-deadline 120`.
- Flags: `--backend ptrace --strict --chaos --chaos-per-thread-slowdown`, plus
  `--verify --verify-strict --verify-allow both`. With these flags Hermit runs
  the guest twice and reports when the two runs differ.
- Per-case chaos settings:
  - thrift: slowdown max 10.0, target timeslice 1e6, clock multiplier 1.0.
  - asyncssl: slowdown max 8.0, target timeslice 5e6, clock multiplier 1.0.
  - httpcoro: slowdown max 10.0, clock multiplier 0.5, no target timeslice.
- Only the `--sched-seed` and the per-run paths vary between runs. The complete
  argv of each run is in its `receipt.json`.

Run roles:

- `fail-witness` and `pass-witness`: the manifest FAIL and PASS seeds.
- `discriminator`: further seeds used to find or repeat a FAIL at the stress
  path. This includes `search-seed-*`, `discriminator-seed-*`, and a revert
  control whose seed is not the manifest FAIL seed.
- `fresh`: the manifest `fresh_stress_seeds`.

### 2.4 Judging and verdict rule

`tfa_phase2 render-followup` (`fbcode/testinfra/tfa/followup.py`) re-judges
every run listed in `followup/<case>/ledger.json` from its own bytes. It does
not trust recorded labels. Each run is bound to:

- the case;
- the manifest Hermit and safehermit;
- its phase's guest sha256;
- its seed;
- its stdout, stderr and exit code.

The unfixed guest must equal the manifest `artifact_sha256`, and the patched
guest must differ from it.

Each run is classified as follows:

- PASS: exit code 0 and the manifest PASS oracle.
- FAIL: a nonzero exit code (not 124–126) and the case failure marker.
- CENSORED: exit code 124 with a timeout unit result.
- NONDETERMINISTIC: Hermit's nondeterminism failure line without a success
  line.
- OTHER: anything else. It is never counted as PASS.

The verdict is decided as follows:

- Any patched FAIL makes the verdict NOT SUPPORTED.
- SUPPORTED needs all of the following:
  - at least one seed FAILed unfixed;
  - every such seed and the PASS witness have patched replays;
  - all of those patched replays PASS.
- Anything else is INCONCLUSIVE.

The rendered comments are in
`stages/<case>/stage4-followup-hypothesis-r3/followup-comment.md`. The per-run
judgements are in `followup-runs.json` in the same directories and are
byte-identical to `stage3-followup/followup-runs.json`.

## 3. Controls

- **Unfixed identity.** The unfixed ELF sha256 equals the manifest
  `artifact_sha256` for every case (`stress/prefix-elves.json`). The validator
  refuses a ledger in which this does not hold. The real negative controls with
  swapped phases and a patched sha equal to the unfixed sha were refused with
  rc 1 (`gates/followup-20260925-r7-c1f8817a5ebb/negctl-real/`).
- **Patched identity.** The patched ELF sha256 differs from the unfixed one and
  equals the ELF that `build-patched` produced. The patched sha256 values are:
  - thrift `440b9182…`;
  - asyncssl `3dd68a50…`;
  - httpcoro `30ecdc39…`.
- **Revert control.** After `sl revert`, each rebuilt ELF matched the manifest
  unfixed sha256 again, and the revert-control replay FAILed again:
  - thrift: seed 3 FAIL;
  - asyncssl: seed 11 was NONDETERMINISTIC on the first attempt, and two added
    repeats (`revert-control-seed-011-repeat-{1,2}`) both FAILed;
  - httpcoro: seed 0 FAIL.

  This shows that the build path restores the FAIL, so the patched PASSes are
  not an artifact of rebuilding.
- **PASS witness.** The manifest PASS seed PASSed in both phases for every case.
  This checks that the patch does not break the passing schedule.
- **Fresh seeds.** Fresh seeds that were not used for selection were replayed in
  both phases.
- **Validator sensitivity.** Nine targeted mutations of the validator were each
  caught (rc 1), and the restored baseline passes. This is recorded in the r7
  gate and repeated at the new head (section 9).

## 4. Sample sizes and observations

The counts are re-judged run counts per seed. `×n` is the number of replays.

**thrift** (17 unfixed and 13 patched runs, 0 interrupted)

| seed | role | unfixed | patched |
| --- | --- | --- | --- |
| 0 | pass-witness | PASS×2 | PASS×2 |
| 1 | fail-witness | PASS×2 | PASS×2 |
| 2, 4, 5 | fresh | PASS×1 each | PASS×1 each |
| 3 | fresh / discriminator | FAIL×4 | PASS×4 |
| 8 | discriminator (search) | FAIL×1 | PASS×2 |
| 6, 7, 9, 10, 11 | discriminator (search) | PASS×1 each | not run |

The manifest FAIL seed 1 PASSed at the stress path, although the ELF bytes are
the same. The ELF path and run directories enter the argv and mounts, and the
Hermit schedule depends on them. The FAIL therefore had to be found again at
this path. Seed 3 came from the fresh set, and seed 8 came from a search of
seeds 6–11 (1 FAIL in 6). All of the thrift discrimination rests on seeds 3 and
8.

**asyncssl** (14 unfixed and 9 patched runs, 0 interrupted)

| seed | role | unfixed | patched |
| --- | --- | --- | --- |
| 1 | pass-witness | PASS×2 | PASS×2 |
| 11 | fail-witness | FAIL×5, NONDETERMINISTIC×2 | PASS×3 |
| 12, 13, 14 | fresh | PASS×1 each | PASS×1 each |
| 15 | fresh | OTHER×2 | OTHER×1 |

Seed 11 is not fully deterministic. In 2 of its 7 unfixed runs, Hermit's
two-run check reported a divergence. Seed 15 ends OTHER in both phases, with a
deterministic failure at `AsyncSSLSocketTest.cpp:3592` and a SEGV. This is a
different failure class from the targeted one, and the patch does not affect
it.

**httpcoro** (9 unfixed and 8 patched runs, 1 unfixed attempt interrupted)

| seed | role | unfixed | patched |
| --- | --- | --- | --- |
| 0 | fail-witness | FAIL×3 | PASS×2 |
| 1 | pass-witness | PASS×2 | PASS×2 |
| 4, 7 | fresh | PASS×1 each | PASS×1 each |
| 5, 6 | fresh | CENSORED×1 each (120 s deadline) | PASS×1 each |

The interrupted attempt is `stress/httpcoro/prefix/fail-witness-seed-000-attempt-2`.
It has no receipt and is counted as neither PASS nor FAIL.

Totals: in the unfixed phase, thrift FAILed 5 of 17 runs, asyncssl 5 of 14 and
httpcoro 3 of 9. No patched replay FAILed in any case. All three verdicts are
SUPPORTED.

## 5. Hypotheses and calibrated confidence

"Confidence" is our judgement of the evidence for the mechanism. It is not a
probability. A patched replay at the same seed is a different schedule,
because the patched code executes differently. Patched PASSes are therefore
consistent with any change that perturbs timing enough, and they do not
discriminate between the ranked hypotheses.

### thrift: moderate-low

**Selected hypothesis (rank 1).** `ThriftServer::getActiveRequests()` still
counts req1 after its handler posts baton1. The loop
`while (getActiveRequests() < 2)` can therefore exit on req1 and req2 before
req3 reaches the server, so the snapshot has 2 entries.

**Caveat.** The investigator reported: "Not directly instrumented. No build is
allowed, so which snapshot entry is missing and the activeRequests value at loop
exit are inferred from reading the code."

**Alternatives.** Ranks 2–4 were argued against by code reading only:

- rank 2: DebugStub registration lags;
- rank 3: finished-list eviction;
- rank 4: a worker registry is missing.

**Discriminating experiment not run.** Log `activeRequests` and the snapshot
entries at loop exit in an unfixed FAIL replay.

### asyncssl: moderate

**Selected hypothesis (rank 1).** TLS 1.3 NewSessionTickets are still unread at
the first timestamped write. `AsyncSocket::handleRead` consumes the tickets and
the echo in one loop without re-draining the error queue. As a result,
`writeAndReflect` returns before the ACK ByteEvent arrives.

**Caveat.** The investigator reported: "No post-fix run exists … That the ACK
timestamp is queued when the server echoes is inferred from how kernel TCP
handles ACKs; the syscall trace does not observe it directly."

**Supporting context.** The investigator cites a sibling test in which the
maintainers applied the same test-side workaround for the same sequence.

**Alternatives.**

- Rank 3 is a production defect: `handleRead` should drain the error queue
  between reads. It was rejected on sibling precedent and hot-path cost, not
  disproved by experiment. If it holds, the candidate patch hides a production
  defect.
- Rank 4 is a Hermit artifact. It was argued against from the trace.

**Discriminating experiment not run.** Trace the error-queue ordering in a
post-fix replay.

### httpcoro: moderate

**Selected hypothesis (rank 1).** `loopN(4)` uses blocking `loopOnce`. Idle
iterations let Hermit virtual time reach the 1 s connection read timeout. The
drain's GOAWAY then suppresses `onAbort` for the 1001 burst streams, so the
reset-rate limiter never trips, and `readBodyEvent` returns SUSPEND.

**Caveat.** The investigator reported: "No DBG log directly shows 'Initiating
drain due to read timeout' or 'Suppressing onAbort'; that the +900ms deadline
was the connection read timer comes from timing arithmetic." Other 1 s timers
expire in the same window.

**Scope.** Phase one already classified this failure as a virtual-time
threshold, not a thread race, so it should not be cited as race evidence. This
case runs with clock multiplier 0.5. Its native relevance is not established.

**Alternatives.** Ranks 2–4 were each argued against from traces or code, but
not by an experiment:

- rank 2: the reset-rate limiter's window resets;
- rank 3: ordering of `loopN(2)`;
- rank 4: the stream read timeout.

**Discriminating experiment not run.** Replay the FAIL seed with
`--vmodule=HTTPCoroSession=4,HTTP2Codec=3`, and check that the post-fix epoll
timeouts are all 0.

## 6. Candidate fixes

All three patches are test-only. They are candidates, were applied only in the
isolated stress checkouts, and were not posted.

| case | file | patch sha256 | change |
| --- | --- | --- | --- |
| thrift | `thrift/lib/cpp2/test/InstrumentationTest.cpp` | `61dd5a10…` | after baton1, wait for active requests to reach 0 |
| asyncssl | `folly/io/async/test/AsyncSSLSocketTest.cpp` | `dc77e591…` | drain post-handshake data with an untimestamped `writeAndReflect` first |
| httpcoro | `proxygen/lib/http/coro/test/HTTPDownstreamCoroSessionTests.cpp` | `3806affc…` | replace blocking `loopN(4)` with a non-blocking loop |

The full diffs are in `investigation/<case>/agent-output/patch.diff` and at the
end of each follow-up comment.

## 7. Stress and gate outcomes

For every case:

- `build-patched` returned rc 0.
- `arc lint` on the patched files returned rc 0.
- The native patched manifest `buck test`, run outside Hermit, passed with
  rc 0 (Pass 1, Fail 0).

That native run is n = 1, and no unfixed native run was made for comparison, so
the run does not discriminate. It is not in the ledger.

**Production containment is BLOCKED.**
`buck2 test fbcode//testinfra/tfa/tests:tfa_production_containment_test` at
`c1f8817a` returned rc 32 in run 2, with 36 tests passing and 4 failing. The
evidence is in `gates/followup-20260925-r7-c1f8817a5ebb/containment_test-run2.*`.
At the reporting head `b8b85d00462b` it again returned rc 32, with 36 passing
and the same 4 failing
(`gates/followup-20260925-r8-b8b85d00462b/containment_test.*`).

Run 1 had 34 passes. Its two extra failures were host flakes (`hg_monitor`, and
a reverie perf-counter skid). A rerun of only those two passed 2/2. Both runs
are preserved.

The four failures are signed Codex 0.156.0 preflight cases that lack daemon
socket fixes. There is also a secondary chataccd cleanup symptom. The four
failing tests are:

- `test_real_codex_reviewer_final_message_cannot_be_redirected`
- `test_real_codex_final_message_cannot_be_redirected_by_tool`
- `test_real_codex_candidate_handoff_uses_controller_buck`
- `test_real_codex_native_workspace_sandbox_and_tfa_cleanup`

This 36/40 is the containment suite's test count, not a pass rate over cases,
seeds or fixes. It validates no candidate fix. The suite remains BLOCKED on the
four Codex 0.156.0 preflight cases.

## 8. Limitations

1. **Small n.** There are only 1–4 patched replays per seed. Thrift seeds 6, 7,
   9, 10 and 11 were never replayed patched.
2. **Adaptive selection.** The thrift FAIL seeds were found after the manifest
   FAIL seed PASSed at the stress path.
3. **Hermit only.** All discriminating evidence comes from Hermit chaos
   replays. None of it is native, CI or landed evidence.
4. **Same-seed replays are not paired.** A patched replay at the same seed does
   not repeat the unfixed schedule (section 5).
5. **Unclear outcomes.** asyncssl seed 11 is not fully deterministic.
   asyncssl seed 15 ends OTHER in both phases. httpcoro has 2 CENSORED runs and
   1 interrupted attempt.
6. **Trusted inputs.** The validator trusts the ledger's enumeration of runs and
   their role labels. Completeness was checked directory by directory against
   `stress/`, but the validator cannot prove it. The link from patched ELF to
   patch rests on the post-fix harness logs (`build-patched.*`, `applied.out`),
   not on the validator.
7. **Validation encoding.** Candidate-report validations record NOT RUN as
   FAIL 0/0 (section 2.2).
8. **Not re-executed.** The reproducer commands below were not re-executed for
   this document. They are the harness commands, and the receipts show that
   they ran.
9. **Link-tree dependency.** `witness.sh` imports from the `tfa_phase2_test`
   Buck link tree, so rebuilding that target can change what it runs.

## 9. Reporting changes in this revision

`render-followup` was changed as follows:

- It now labels the root cause "Root-cause hypothesis (investigator candidate,
  rank r of n; not proven)".
- It quotes the selected hypothesis's contradicting or unverified evidence.
- It lists the other ranked hypotheses.
- It defines the run roles and states the verdict rule, including the
  same-seed caveat.
- It prints the exact receipt commands for the unfixed FAIL and the patched
  replay of the lowest seed that FAILed unfixed.

The quoted hypothesis fields are investigator-written, and the report validator
only requires them to be non-empty strings. The renderer therefore treats them
as untrusted:

- It uses the hypotheses that `validate_candidate_report` returns from the same
  parse it validated. It does not re-read the report file afterwards.
- It collapses each field to one line and drops Unicode control and format
  characters.
- It replaces backticks with `'`.
- It cuts any field longer than 1000 characters, adding a marker. The longest
  real field is 751 characters, so no real text was cut.
- It prints the fields inside a `text` code fence, which they cannot close.
  Markdown, HTML and @mentions are therefore inert.

An earlier revision, `2c30cbed`, lacked this policy and re-read the report.
Its renders (`stage4-followup-hypothesis-r2/`) and its incomplete gate
directory are marked `SUPERSEDED.txt`. The r3 comments differ from r2 only in
the fenced layout and the backtick mapping.

Judgements and verdicts are unchanged: the `followup-runs.json` files are
byte-identical to stage 3. The exact-head gates are in
`gates/followup-20260925-r8-b8b85d00462b/`, with pre-commit sensitivity
evidence in `gates/followup-r8b-sensitivity/`.

## 10. Reproducers

Unfixed FAIL and PASS witnesses, runnable now: the stress ELF paths currently
hold the unfixed ELFs (sha256 checked on 2026-09-25). `--out` must be a new
directory under `$E3/stress/<case>/`. The script refuses a guest whose sha256
does not match `--expect-elf-sha256`.

```sh
E3=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case
S=/home/newton/work/tfa/phase2-cases/stress
# thrift: path-local FAIL seed 3 (template fail), PASS seed 0 (template pass)
$E3/witness/witness.sh --case thrift --template fail --seed 3 \
  --elf $S/thrift/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest \
  --expect-elf-sha256 2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc \
  --out $E3/stress/thrift/repro/unfixed-seed-003-$(date +%s)
# asyncssl: FAIL seed 11
$E3/witness/witness.sh --case asyncssl --template fail --seed 11 \
  --elf $S/asyncssl/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test \
  --expect-elf-sha256 47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46 \
  --out $E3/stress/asyncssl/repro/unfixed-seed-011-$(date +%s)
# httpcoro: FAIL seed 0
$E3/witness/witness.sh --case httpcoro --template fail --seed 0 \
  --elf $S/httpcoro/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests \
  --expect-elf-sha256 00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc \
  --out $E3/stress/httpcoro/repro/unfixed-seed-000-$(date +%s)
```

For the PASS witness, use `--template pass --seed <manifest PASS seed>`. The
command is judged by re-rendering or by reading `stdout.txt`, `stderr.txt` and
`receipt.json` in the new directory.

Patched replays: the patched and unfixed ELFs share one path, so patch, build,
check the sha256, replay and restore. The example below is for thrift. For the
other cases, substitute the case's build mode and target from section 1,
`investigation/<case>/agent-output/patch.diff`, and the patched sha256 from
section 3.

```sh
cd /home/newton/work/tfa/phase2-cases/stress/thrift
sl status --reason "confirm clean stress checkout - sl help status"   # must be empty
sl import --no-commit $E3/investigation/thrift/agent-output/patch.diff --reason "apply candidate patch - sl help import"
buck build @fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:InstrumentationTest --show-full-json-output
sha256sum buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest   # expect 440b91829732e378dd2a5519869f5c5bb110473218d7eefedd11cd73f50fa15d
$E3/witness/witness.sh --case thrift --template fail --seed 3 \
  --elf $PWD/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest \
  --expect-elf-sha256 440b91829732e378dd2a5519869f5c5bb110473218d7eefedd11cd73f50fa15d \
  --out $E3/stress/thrift/repro/patched-seed-003-$(date +%s)
sl revert --all --no-backup --reason "restore unfixed stress checkout - sl help revert"
buck build @fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:InstrumentationTest --show-full-json-output
sha256sum buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest   # expect 2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc
```

Re-render the comments (each output directory must be new):

```sh
PAR=/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/f131b89c4a309a34/testinfra/tfa/__tfa_phase2__/tfa_phase2.par
M=/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/cases.v1.json
$PAR render-followup --manifest $M --case-id thrift --ledger $E3/followup/thrift/ledger.json --output-dir /path/to/new/dir
```

The exact measured commands of the original runs are also printed in each
follow-up comment and recorded in each run's `receipt.json` (`cwd`, `argv`).
Those commands write to their original run directories, so point the paths at
new directories before reusing them.

## 11. Review

A fresh read-only Muse review of fbsource `c1f8817a..b8b85d00462b6f159641aee73a49321ca863609d`,
plus this document, returned `Verdict: approve` with no blocking findings:
`reviews/review-b8b85d00.md` (sha256 `c9c23538e31c5db64cdf2d55e13f317c55f3ea0d8167ab7a29ceb17b88448ba1`).
The reviewed text of this document, which is everything above this section, is kept as
`reviews/RESULTS-as-reviewed-b8b85d00.md` (sha256
`245f684df82ddebafd3e2c1434d5da855a565b32f75f05a76185ab4ac961e88a`).
The review's low findings are not addressed in this revision:
- `_agent_text` lets through a field made only of combining marks or filler characters. It renders as a blank line inside the fence.
- `_measured_command` rereads `receipt.json` instead of reusing the judged receipt.
- Three wording nits in sections 4 and 9. The totals are unaffected.
