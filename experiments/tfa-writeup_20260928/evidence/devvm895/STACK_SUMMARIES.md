# D121244476 [testinfra][tfa] Add bounded phase-two prototype (lines 5221)
## Summary
Context:
The phase-two TFA study needs a deterministic three-case wrapper before any diagnostic agent or external mutation is allowed.

Motivation:
Preserve the established strict-ptrace witnesses while enforcing exact-base isolation, resumable execution, bot-only task updates, and unpublished reviewer-free experiment diffs.

This diff:
- adds the versioned HTTPCoro, Thrift, and AsyncSSL manifest plus fixed Hermit and diagnosis contracts;
- adds durable sealed working directories and exact pinned dev-hermit evidence provenance;
- adds atomic leases, an append-only FSM, argv-safe timeout/resume, independent wrapper validation, and complete artifact capture;
- adds exact-base/TFA-stack rejection, list-first task-comment gates, and draft-only Phabricator gates with readback;
- adds focused tests proving validation failure blocks draft/final actions and resumes idempotently.
## Test plan
`arc f`
`arc lint`
`arc pyre check-changed-targets`
`buck test fbcode//testinfra/tfa/tests:tfa_phase2_test` (54 passed)
`buck test fbcode//testinfra/tfa:tfa_phase2_lib-type-checking fbcode//testinfra/tfa/tests:tfa_phase2_test-library-type-checking` (2 passed)
`buck test fbcode//testinfra/tfa:cli -- --run-slow-tests` (1 passed)
`buck run fbcode//hub_infra/cli:hub_cli -- check fbcode/testinfra/tfa`
`buck run fbcode//testinfra/tfa:tfa_phase2 -- dry-run --manifest fbcode/testinfra/tfa/cases.v1.json --artifact-root /tmp/tfa-phase2/actual-runs-20260922/framework-dry-run.gxtbL1 --checkout /home/newton/work/tfa/fbsource`
Exact-head adversarial review of the full local diff; no remaining findings.

# D121244213 [testinfra][tfa] Use native Codex sandbox with TFA lifecycle control (lines 20138)
## Summary
Run the trusted Codex bootstrap and upstream payload as the invoking host user so the supported native `workspace-write` sandbox can launch without a nested map-root or mount namespace. Keep sealed TFA wrapper/supervisor admission, cgroup-v2 hierarchy cleanup, subreaping, process-group backstop, bounded timeouts, and safehermit. Record that TFA supplies no independent outer filesystem or network isolation. Require Codex-native network denial and exclude `/tmp` and `$TMPDIR` from implicit writable roots.

Fail closed before artifact creation unless the complete argv exactly matches the canonical fresh-agent, resumed-agent, or reviewer grammar. Keep the trusted final response outside every tool-writable root and reject pre-existing multi-link outputs before launch. Publish only after cgroup cleanup, only between the exact same-case source and destination, through pinned no-follow parent and file descriptors that reject symlink and hard-link sources or destinations. Real production regressions exercise fresh, resume, and reviewer tools, bind each tool-produced nonce to allowed bytes, reject final-message redirection, prove successful publication of real Codex bytes, and verify native sandbox denial plus per-run TFA cgroup cleanup. An audited privileged helper remains deferred production hardening.
## Test plan
`arc f`
`arc lint -a`
`arc lint`
`arc pyre check-owning-targets fbcode/testinfra/tfa/process.py fbcode/testinfra/tfa/orchestrator.py fbcode/testinfra/tfa/pipeline.py fbcode/testinfra/tfa/prompts.py fbcode/testinfra/tfa/tests/test_production_containment.py fbcode/testinfra/tfa/tests/test_prototype.py`
`arc pyre check-changed-targets`
`buck test --no-remote-cache fbcode//testinfra/tfa/tests:tfa_phase2_test` (76 passed; session 30962247475440273)
`buck test --no-remote-cache fbcode//testinfra/tfa/tests:tfa_production_containment_test` (11 passed; session 16044073876580293)
`buck test --no-remote-cache fbcode//testinfra/tfa/tests:tfa_phase2_test fbcode//testinfra/tfa/tests:tfa_production_containment_test fbcode//testinfra/tfa:tfa_phase2_lib-type-checking fbcode//testinfra/tfa/tests:tfa_phase2_test-library-type-checking fbcode//testinfra/tfa/tests:tfa_production_containment_test-library-type-checking` (90 passed; session 26177172884231650)
The same full matrix with `-j 1` passed 90/90; session 8162774705161542.
`buck test --no-remote-cache -j 1 fbcode//testinfra/tfa:cli -- --run-slow-tests` (1 passed; session 35747322079529589)
`buck build fbcode//testinfra/tfa:tfa_phase2`
`fbcode/hub_infra/cli/hub check fbcode/testinfra/tfa` (PASS 605/1000)
`buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json` (3 cases)
`buck run fbcode//testinfra/tfa:tfa_phase2 -- dry-run --manifest fbcode/testinfra/tfa/cases.v1.json --artifact-root /tmp/tfa-phase2/review-dry-run-hardlink.SbRmBb/artifacts --checkout /home/newton/work/tfa/fbsource`
Independent exact-head approval is required before any case execution.

# D121649569 [testinfra][tfa] Close controller follow-up contracts (lines 1157)
## Summary
[hermit2, unknown-agentname, codex-family-by-ancestry, devbig030, role=impl]

# Context
The phase-two controller still had three fail-closed gaps at the boundary between a qualified repair and a draft: Buck executable-relative resources were not copied into the sealed runtime image, the HTTPCoro fatal witness accepted an under-specified transcript, and the generated Thrift xplat mirror was outside the final commit and review seal. Draft resume also treated a missing final mirror seal as an invitation to create one with an empty review-schema path.

# Mitigation
Pin and copy declared runtime resources under the attempt-specific image and revalidate their manifest, bytes, modes, paths, and byte budget around every validation run. Require the exact HTTPCoro fatal header, normal and async frames, SIGABRT metadata, and unrelaxed deterministic comparison transcript. For Thrift, require the declared canonical/mirror pair to be byte-identical and delta-identical, bind both paths to a fresh independent review, and persist the final commit evidence across interruption and draft readback. Resume now requires that final seal at both the caller and helper boundary, so neither diff generation nor review can run without it.

# This Diff
Adds these controller contracts and negative tests for malformed resources, transcript mutations, ambiguous mirror output, divergent mirror bytes, and a missing final seal during draft resume. No validation assertion, tolerance, comparator, skip, label, or gate is weakened.
## Test plan
- `arc f` (format clean)
- `buck2 test --no-remote-cache -j 1 fbcode//testinfra/tfa/tests:tfa_phase2_test` (114 passed, 0 failed; 45.19 seconds; session 2533275190228708)
- `arc lint -a` (no lint issues; 25.41 seconds)
- `arc pyre check-changed-targets` (2 of 2 affected targets; no type errors; 3.65 seconds)
- `buck2 run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json` (3 cases; 0.57 seconds)

# D121848885 [testinfra][tfa] Make controller dependencies self-contained

[hermit2, unknown-agentname, codex-family-by-ancestry, devbig030, role=impl]

Context:
The phase-two controller still depended on mutable machine-local SafeHermit paths and a historical... (lines 736)
## Summary
...Hermit executable outside fbsource. The current imported Hermit and Codex binaries also have known host compatibility failures that must not be mistaken for qualified execution.

Package and lazily pin the SafeHermit helpers, consume the current fbsource Hermit resource, and preserve the historical 4630ee2a witness while recording the a919cce qualification blockers. Scope verification allocation state to each attempt and use disposable quota-backed production fixtures. Record versioned Codex executable identity and reject known-blocked, unknown, malformed, or unstartable local versions before agent launch.
## Test plan
- `arc f` (clean)
- `arc lint -a` (passed; 32 advisory ShellCheck findings in the byte-identical packaged safehermit script)
- `arc pyre check-changed-targets` (4 targets, no type errors)
- `buck2 test --no-remote-cache -j 1 fbcode//testinfra/tfa/tests:tfa_phase2_test` (124 passed)
- `buck2 run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json` (3 cases)
- production containment run 25051272978218003 (28/35 passed; retained seven explicit environmental failures)

# D121848886 [testinfra][tfa] Close phase-two review findings (lines 1569)
## Summary
[hermit2, unknown-agentname, gpt-5.6-sol, devbig030, role=impl]

Restore the strict current-Hermit FullTrace contract without changing the
historical witness, bind packaged and qualification artifacts to their exact
configured identities, and fail aggregate readiness closed for unqualified
Hermit and Codex binaries.

Seal independently verifiable import receipts, render self-contained
reproducers, make production-fixture recovery durable before cleanup, and
replace timing-based containment checks with direct liveness evidence.
## Test plan
- arc f
- arc lint -a
- arc pyre
- buck test fbcode//testinfra/tfa/tests:tfa_phase2_test (127/127)
- buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json (3 cases)
- buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test (36/40; packaged Hermit minimal FullTrace blocker passes as a fail-closed assertion, and four real Codex tests fail closed on unsupported codex-cli 0.156.0)

# D121848888 [testinfra][tfa] Seal Codex incompatibility provenance (lines 626)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

Bind the owner-managed Codex entrypoint, opened launcher inode, selected signed
release manifest, channel, and version into one before/after preflight identity.
Snapshot launch environment state and reject any post-preflight drift. Record stable
0.156.0 and latest 0.156.1 as blocked by the exact Btrfs daemon-socket
incompatibility and name the two upstream commits required for remediation.

Replace the production cgroup probe's polling and delayed absence check with
pipe barriers and a lifetime lock. Start its unchanged execution timeout at
the explicit readiness barrier so variable process startup cannot consume the
execution budget.
## Test plan
- arc f
- arc lint -a
- arc pyre
- buck test fbcode//testinfra/tfa/tests:tfa_phase2_test (132/132 in the original checkout; 131/132 in a fresh checkout, where test_real_packaged_hermit_is_bound_to_blocked_provenance fails closed as PROVENANCE_MISMATCH because the pinned hermit-test-bin sha covers a build whose fb_build_info stamps revision, path and build time, a pre-existing pin not changed here)
- buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json (3 cases)
- buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test (36/40; all non-Codex cases pass, a real signed-latest preflight is blocked with exact provenance, and four real Codex readiness cases fail closed on the sealed external runtime blocker)

# D121848889 [testinfra][tfa] Hash-bind the Codex payload and unify deadlines (lines 1128)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

Harden the Codex launch identity and the managed-process deadlines after
the exact-head review of the sealed-provenance change. Both reviewed
compatibility keys (stable 0.156.0 and latest 0.156.1) stay blocked on the
Btrfs daemon-socket incompatibility; nothing here makes any key eligible.

- Payload binding: a compatibility entry is now (state, blocker, payload
  sha256). An entry marked eligible without a 64-hex payload digest is
  demoted to blocked. The payload wrapper opens the owner-managed payload
  with O_NOFOLLOW, hashes it through that single descriptor, checks the
  file identity, type, owner (root or the effective uid) and group/other
  write bits, compares the digest with the bound value, and execs the
  supervisor through /proc/self/fd/<fd> instead of the path. The sentinel
  records "payload-sha256 <digest>" and run metadata records
  payload_sha256 and payload_binding; the parent refuses a sentinel digest
  that differs from the bound one. The version bootstrap runs unbound but
  still records the observed digest.
- /proc/<pid>/exe race: remove the post-spawn /proc/<pid>/exe check. The
  launcher is instead covered by a root-owned, non-writable directory
  chain from / to the entrypoint and launcher, a descriptor re-check of
  the launcher's mode, size, inode and type immediately before Popen, and
  the payload binding above.
- Entrypoint: classify the entrypoint by its own lstat type and owner
  before calling readlink, so a regular file or foreign-owned link is
  reported as "ownership or type is unsafe" rather than "unavailable".
- Deadlines: one origin, taken when the payload launcher is spawned and
  recorded as deadline_origin=payload-launcher-spawned. The execution
  deadline is origin + timeout; readiness waits until
  min(origin + startup, origin + timeout). A readiness miss before an
  earlier startup deadline is a readiness error; otherwise it is the
  execution timeout. Metadata records startup_timeout_seconds.
- Channel: META_CODEX_RELEASE unset or empty selects stable and exactly
  "latest" selects latest; every other spelling, including "stable",
  "LATEST", surrounding whitespace and version strings, is refused. The
  version banner must match the selected channel's reviewed banner.
- The blocker text names the /tmp, /var/tmp and /data/tmpvol aliases of
  the same Btrfs subvolume; README documents /data, the payload binding,
  its residual risks (a same-uid writer or tracer, the preflight version
  read outside the cgroup, the unhashed codex-code-mode-host sibling) and
  the deadline origin. Identity schema is v3.
- Tests: blocker-text tests assert both upstream fix hashes
  (7cec3e110ce6dc5c14704dfa06f6db9c23a530fc,
  a708fc683934d7d7ea7560354968fedd2be28581) literally. 25 new tests cover
  owner, link-count, mode, symlink, directory-chain, manifest, channel,
  banner, launcher-descriptor, payload-binding, sentinel and fake-clock
  deadline rejection. Two timeout tests move from 0.3s/0.5s to 5s because
  the budget now includes launcher startup, so the shorter value could
  expire before the guest creates the descendant the test must kill; the
  guests still sleep 30s so the timeout path is exercised.
- test_real_packaged_hermit_is_bound_to_blocked_provenance now requires
  PROVENANCE_MISMATCH: the packaged hermit-test-bin is rebuilt with
  fb_build_info stamps, so its digest differs from the reviewed
  provenance digest. The test asserts that difference, the exact error
  prefix with the case witness, that nothing is pinned, and the recorded
  identity fields (state, witness, packaged revision and digest,
  configured targets, blockers, receipts digest). If the resource is ever
  re-pinned to a byte-identical build, this test must change with it.
## Test plan
- arc f; arc lint -a (both clean)
- arc pyre check-changed-targets (no type errors, 3 targets)
- buck test fbcode//testinfra/tfa/tests:tfa_phase2_test (157/157, 15s)
- mutation check: restoring readlink before the lstat type check makes
  test_entrypoint_requires_trusted_symlink fail with "Codex entrypoint is
  unavailable"
- buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json (3 cases)
- buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test (36/40 in 5m28s; all non-Codex cases pass, the real signed Codex version bootstrap passes through the descriptor-bound payload path, and the four real Codex agent cases fail closed on the sealed Btrfs socket-isolation blocker)

# D121848890 [testinfra][tfa] Refuse empty wrapper argv; byte-bind Hermit test (lines 148)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

Address the two minors from the review of 795572d12246.

- test_real_packaged_hermit_is_bound_to_blocked_provenance no longer
  assumes the ambient hermit-test-bin digest differs from the reviewed
  digest. It hashes the observed resource bytes and requires exactly one
  state: BLOCKED when they equal the reviewed provenance digest (the
  reviewed packaged state is BLOCKED, which the test also pins), and
  PROVENANCE_MISMATCH otherwise. Either way it asserts the exact error
  prefix including the observed digest, that nothing is pinned, and every
  recorded identity field (schema, state, witness, target, import commit,
  packaged revision, digest, state and configured target, packaged and
  qualification blockers, the qualification record, receipts digest).
- New test_packaged_hermit_mutated_bytes_are_provenance_mismatch
  substitutes only the packaged executable with synthetic bytes, keeping
  the real compatibility record, receipts and pinned digests, and
  requires PROVENANCE_MISMATCH with the same identity assertions, so the
  mismatch path is proven independently of how the resource was built.
- payload_wrapper.py: _upstream_command() refuses a missing or empty
  upstream executable with SystemExit("payload wrapper received no
  upstream executable") as the first step of main(), before cgroup
  admission, the sentinel and payload binding, so no later index can
  raise IndexError. The existing len(argv) guard is folded into it and
  also refuses an empty-string executable. The wrapper pin in process.py
  moves to 160c7455e2a44d3ffa26bb8a78265e6f66a52023bcb7dc011a4fdbc411b8c824
  (6819 bytes). New test_wrapper_refuses_empty_upstream_argv covers
  [], [wrapper] and [wrapper, ""] and runs main() with live control
  paths, asserting the diagnostic and that cgroup.procs and the sentinel
  are untouched.

Both production Codex compatibility keys remain blocked with no payload
digest; nothing here changes eligibility or containment.
## Test plan
All on the exact committed bytes, exit status captured before filtering:
- `buck test fbcode//testinfra/tfa/tests:tfa_phase2_test -- -r 'packaged_hermit|wrapper_refuses_empty|test_wrapper_|entrypoint_requires'`: 9/9 (5.8s)
- `buck test fbcode//testinfra/tfa/tests:tfa_phase2_test`: 159/159 (13.1s; 157 at the parent plus the 2 new tests)
- `buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json`: 3 cases
- `arc pyre check-changed-targets`: no type errors
- `arc f`; `arc lint -a`: clean, no bytes changed
- Negative controls, each restored and sha-verified: deleting the
  empty-argv refusal fails test_wrapper_refuses_empty_upstream_argv
  ("SystemExit not raised"); evaluating the reviewed packaged state before
  the provenance digest in hermit.py fails both packaged-Hermit tests
  (BLOCKED instead of PROVENANCE_MISMATCH)
- In this checkout the observed resource digest differs from the reviewed
  one, so the real test exercised its PROVENANCE_MISMATCH arm; its BLOCKED
  arm on real bytes needs a byte-identical build and was not exercised
  (the synthetic exact-match BLOCKED path is covered by
  test_packaged_hermit_records_exact_blocked_compatibility)
- `buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test`:
  36 pass / 4 fail (rc=32, 406s). This is a blocked result, not a pass:
  the four real-Codex agent cases fail closed in preflight because signed
  codex-cli 0.156.0 lacks 7cec3e110ce6dc5c14704dfa06f6db9c23a530fc and
  a708fc683934d7d7ea7560354968fedd2be28581; every non-Codex case passes.

# D121848892 [testinfra][tfa] Render deterministic difference hints and unposted initial comments (lines 435)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

Phase-two acceptance stages 1 and 2 need, for each of the three cases, the
PASS/FAIL Hermit reproducer pair with deterministic difference hints and the
initial flaky-test comment text, without posting. Before this change
the only renderer was `initial_text`, which refuses thrift (no IssueID),
and nothing derived hints from the authoritative records.

- New `hints.py`: `difference_hints(case, record_dir)` reads the replica-1
  `result.json`, stdout and stderr for the manifest's
  `historical_pass_seed` and `historical_fail_seed` from an authoritative
  dev-hermit record. It fails closed with `HintError` unless each record
  names exactly that `--sched-seed`, the expected `target_oracle`, the
  expected failure class (none for PASS, the manifest `failure_class` for
  FAIL) and a `status=VALID;` diagnostic, and unless exactly one
  replica-1 selection matches. The output is a JSON-serialisable dict
  containing:
  - every input's path, SHA-256 and size
  - per-run exit, signal, gtest verdict, gtest `Failure` blocks and glog
    fatal lines, the top non-abort stack frames, and the verifier message
    counts
  - a bounded unified diff of guest stderr after the verifier block, with
    glog timestamps/pids and hex addresses normalised
  - a same-lines-reordered flag
  - `qualification: "deterministic-text-difference; not a root cause"`
- `initial_text(case, artifact_only=False)`: the existing behaviour is
  unchanged (it still refuses a missing IssueID). `artifact_only=True`
  labels the text "(artifact-only preview; not posted)" and renders a
  missing IssueID as "none (no exact FLAKY issue)"; both reproducers and
  the OSS Hermit link are unchanged.
- New CLI `render-evidence --manifest --case-id --record-dir --output-dir`
  writes `difference-hints.json` and the artifact-only
  `initial-comment.md` with exclusive create (never overwrites). It
  launches no process and calls no external service; the README says so.

No change to the process, Hermit, Codex, state or pipeline code; both Codex
compatibility keys remain blocked and the packaged Hermit gate is
untouched.
## Test plan
- `buck test fbcode//testinfra/tfa/tests:tfa_phase2_test`: 166/166 (159 at
  the parent, plus 6 `HintTest` and 1 comment test)
- `buck run fbcode//testinfra/tfa:tfa_phase2 -- validate-manifest --manifest fbcode/testinfra/tfa/cases.v1.json`: 3 cases
- `arc pyre check-changed-targets`: no type errors; `arc f`; `arc lint -a`: clean
- Negative controls, each restored and sha-verified: disabling the seed
  binding fails `test_refuses_record_not_bound_to_manifest_seed`; disabling
  the failure-class check fails `test_refuses_record_with_wrong_failure_class`
- Real run on the three authoritative records extracted from dev-hermit
  edd968317a09 (git archive; outputs outside /tmp, never posted): all three
  render. asyncssl flags the server `HandshakeCallback::connectionAccepted`
  reordered after `writeSuccess`; thrift isolates
  `InstrumentationTest.cpp:836` `reqSnapshots.size()` 2 vs 3; httpcoro isolates
  the `HTTPDownstreamCoroSessionTests.cpp:1685` fatal in the `ResetRateLimit`
  coroutine lambda.
- The production containment suite was not rerun: no containment code
  changed; its last result (36/40, the four signed-Codex preflight
  blockers) remains a blocked result.

# D121848893 [testinfra][tfa] Lead unposted initial comments with measured non-/tmp reproducers (lines 647)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

The owner forbids running from `/tmp`, but every phase-one PASS/FAIL command
advertises a `/tmp/tfa-phase2` cwd, same-path mount, safehermit report and
verify-log dir. Changing paths and argv can change virtual scheduling, so
relocated commands are reproducers only once measured. This change lets the
unposted initial comment lead with measured relocated commands while keeping
the historical record labelled and hash-bound.

- New `relocation.py`: `validated_relocation(case, summary_path)` accepts a
  `tfa.relocated-reproducer-summary.v2` summary only if all of these hold:
  - Both witnesses are `REPRODUCED` with at least two decided attempts, at a
    seed that is not a fresh stress seed.
  - The attempt directories on disk at that seed are exactly the decided plus
    censored labels, with no omissions or inventions.
  - Every censored attempt really is `CENSORED` (outer timeout or
    rc 124/125/126), and it is counted as neither PASS nor FAIL.
  - Every decided attempt's receipt binds the manifest Hermit and ELF
    SHA-256s and its own stdout/stderr bytes. The pipeline's gtest oracles
    then re-judge it from those bytes, so a receipt's recorded label is never
    trusted.
  - Each argv differs from the manifest argv only in the `/tmp`-bound tokens
    (report path, verify-log dir, and the old mount, replaced by a same-path
    cwd mount plus an owned `--tmp=`) and `--sched-seed`, and no token names
    `/tmp`.
- `initial_text(..., relocation, record_sha256)` leads with the measured
  commands, each prefixed with `mkdir -p <cwd> && cd <cwd>`. It reports a
  changed witness seed and any censored attempts, then demotes the `/tmp`
  commands to a "Historical phase-one record" section citing the
  `experiment.json` SHA-256. Without a relocation the text is unchanged.
  `comments.py` imports the relocation types under `TYPE_CHECKING` to avoid a
  pipeline → comments → relocation import cycle.
- `render-evidence --relocation-summary` validates the summary and hashes the
  record's `experiment.json`. It still writes with exclusive create.
- Eight `RelocationTest` cases cover the leading layout, changed-seed and
  censored reporting, the missing-hash refusal, a censored attempt relabelled
  as decided, an omitted censored attempt, receipt bytes that contradict the
  oracle, argv drift beyond `/tmp` paths, and fresh stress seeds or
  unreproduced state.

No process, Hermit, Codex, state or pipeline code changed. Both Codex
compatibility keys remain blocked.
## Test plan
Every gate below was run as its own unpiped command. Each log and exit status
is under the registered evidence tree `tfa-phase2-three-case/gates/relocation-20260925-r2/`.
An earlier combined attempt is kept but marked invalid in
`gates/relocation-20260925/INVALID.md`.
- `buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test`: rc 0, 174/174
  (166 at the parent plus 8 `RelocationTest`)
- `buck2 test fbcode//testinfra/tfa/tests:tfa_production_containment_test`:
  rc 32, 36 pass / 4 fail. All four `test_real_codex_*` failures are the
  signed Codex 0.156.0 `_codex_environment_preflight` blocker (missing
  7cec3e110ce6 and a708fc683934). This remains BLOCKED, not green.
- `arc pyre check-changed-targets`, `arc f`, `arc lint`: rc 0, clean
- Negative control: replacing the `_check_oracle` call with `pass` fails
  `test_refuses_receipt_whose_bytes_contradict_its_oracle` and
  `test_refuses_relabelled_censored_attempt` (rc 1). The file was restored
  and SHA-verified.
- Real three-case measurement (`relocated-v2`). Each attempt ran in a
  newly created, uid-owned, verified-empty cwd under
  `/home/newton/work/tfa/phase2-cases/<case>/tfa-phase2-repro/attempts/`.
  Every witness stayed at its manifest seed, with two decided attempts, zero
  censored, and identical stdout:
  - thrift: FAIL seed 1, PASS seed 0
  - asyncssl: FAIL seed 11, PASS seed 1
  - httpcoro: FAIL seed 0 (rc 134, 114 s and 113 s against the 120 s bound),
    PASS seed 1
- The v1 run's httpcoro seed-0 attempt was CENSORED at the bound. Its receipt
  and the leftover verify-summary file in the shared v1 cwd are preserved,
  with an incident record and a live-process scan (0 hits).
- `render-evidence --relocation-summary`: rc 0 for all three cases. The
  output is byte-identical to `stages/<case>/stage1-2-relocated/`. Nothing was
  posted.

# D121848894 [testinfra][tfa] Bind censored relocation attempts and resolve relocated paths (lines 250)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

The exact-head review of the relocation change found three gaps.

- A receipt labelled `CENSORED` was trusted as-is. Now every attempt, decided
  or censored, goes through the same binding:
  - `_bound_receipt` checks the receipt's schema, case, seed and argv.
  - It checks the SHA-256 of its stdout and stderr, and of its safehermit report.
  - It requires the report's `exit_code` to match the receipt.
- `_check_censored` also requires wall-bound evidence:
  - rc 124 and `unit_result=timeout`;
  - `bound.wall=APPLIED:<deadline>s` for the argv's `--sh-deadline`;
  - `elapsed_secs` and `duration_seconds` both at least that deadline.
  It then refuses the attempt if its bytes satisfy either the PASS or the FAIL
  gtest oracle.
- `_check_argv` rejected literal `/tmp` strings but accepted `..` and symlinks
  into `/tmp`. The cwd mount, safehermit report, verify-log dir and guest
  `--tmp` paths must now be absolute and `normpath`-canonical. Neither the path
  nor its `realpath` may be under `/tmp` or its `realpath`.
- `initial_text` requires the historical `experiment.json` digest to be exactly
  64 lowercase hex characters.

Three new `RelocationTest` cases cover:
- decided bytes relabelled as censored;
- forged censored receipts (wrong schema, wrong seed, short duration, exit code
  contradicting the report, bare report without timeout evidence);
- `..` and symlink escapes for each path role, against a patched forbidden root.
The historical-hash test gains uppercase, short, newline-terminated and non-hex
digests. No existing assertion changed. The fixture now writes a bound safehermit
report for every attempt.

No process, Hermit, Codex, state or pipeline code changed. Both Codex
compatibility keys remain blocked.
## Test plan
The pre-commit checks are under the registered evidence tree
`tfa-phase2-three-case/gates/relocation-20260925-r4-precommit/`:
- `arc f` and `arc lint` on the four files: rc 0.
- `arc pyre check-changed-targets`: rc 0 (`pyre-after-fix`). The first commit of
  this change, `335ed55b`, failed the exact-head pyre gate on a test lambda type.
  That red run is preserved in `gates/relocation-20260925-r4-335ed55b/`.
- `RelocationTest` via the link tree: 11/11.
- All three real `relocated-v2` summaries still validate with unchanged summary
  hashes and witness seeds.

The exact-head gates for this commit are run afterwards, each as its own unpiped
command, under `gates/relocation-20260925-r4-<head>/`.

Earlier evidence is preserved as recorded:
- r2 (`bb4ff9f3`'s pre-commit tree): 174/174. Containment 36/40, where the four
  failures are the signed Codex 0.156.0 preflight blocker.
- r3 (exact head `bb4ff9f3`): 174/174. Containment rc 32 with 35/40. Four
  failures are the same signed-Codex blocker. The fifth,
  `test_real_codex_bootstraps_with_bound_agent_identity`, failed with "checkout
  services did not retire": a host `chataccd` (not started by this work) was
  watching the disposable `/tmp/tfa-phase2/tfa-production-source-*` checkout.
  It is recorded in `chataccd-ps.out` and has not been relabelled.
The signed Codex gate remains BLOCKED, not green.

# D121848895 [testinfra][tfa] Bind relocated safehermit reports to unique keys and the manifest Hermit (lines 49)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

The exact-head review of `76901578` found an evidence-binding gap in
`_bound_receipt`. It parsed the safehermit report with `setdefault`, so a
repeated key was silently resolved to its first value. It also never checked
which Hermit binary safehermit reports it ran.

This change:
- rejects a report that names any key more than once;
- requires the report's `binary_sha256` to equal the manifest `hermit_sha256`,
  alongside the existing receipt input check.

The new `test_refuses_report_with_foreign_hermit_or_repeated_key` covers three
cases on a decided attempt:
- a wrong `binary_sha256`;
- a second, conflicting `binary_sha256`;
- a second, conflicting `exit_code`.

The fixture now writes the manifest Hermit hash. Two existing forged-report
subtests gained that line, so each still reaches the property it targets. Their
assertions are unchanged.

The README now notes a limitation. The offline renderer resolves relocated
paths only when it runs, so a symlink swapped afterwards is not detected (a
TOCTOU gap). This validates recorded evidence; it is not runtime containment.

No process, Hermit, Codex, state or pipeline code changed. Both Codex
compatibility keys remain blocked.
## Test plan
Pre-commit checks, under the registered evidence tree
`tfa-phase2-three-case/gates/relocation-20260925-r5-precommit/`:
- `arc f`, `arc lint`, `arc pyre check-changed-targets`: rc 0
- `RelocationTest` via the link tree: 12/12
- All 12 real `relocated-v2` safehermit reports have unique keys and the
  manifest Hermit hash. All three real summaries still validate with unchanged
  summary hashes and witness seeds.

The exact-head gates for this commit are run afterwards, each as its own unpiped
command, under `gates/relocation-20260925-r5-<head>/`.

Parent `76901578` exact-head gates (`gates/relocation-20260925-r4-76901578/`):
- `arc lint` and pyre: rc 0
- `tfa_phase2_test`: 177/177
- Containment: rc 32, 36/40. All four failures are the signed Codex 0.156.0
  preflight blocker. The earlier `chataccd` "did not retire" failure did not
  recur.
- The relocation renders were not run at that head before this follow-up.

The signed Codex gate remains BLOCKED, not green.

# D121848896 [testinfra][tfa] Render unposted candidate-fix follow-ups from re-judged witness replays (lines 666)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

Phase two needs a follow-up comment for each investigator candidate patch. It
must show what the patch did to the FAIL and PASS witnesses and to bounded
stress seeds, and it must never trust a recorded label. This adds
`followup.py` and a `render-followup` command.

The command reads a `tfa.followup-ledger.v1` ledger that names:
- a validated candidate report;
- an `unfixed` and a `patched` phase, each with its test ELF SHA-256;
- the `tfa.witness-replay-run.v1` receipts from one controller stress checkout.

Every receipt must bind:
- the manifest Hermit, both in its inputs and in the safehermit report;
- its phase's ELF;
- the ELF that its argv actually runs;
- its scheduler seed;
- its own stdout, stderr and safehermit report bytes.

Each run is then re-judged from those bytes with the manifest oracles:
- CENSORED requires rc 124 plus the safehermit `unit_result=timeout` wall
  evidence;
- NONDETERMINISTIC and OTHER count as neither PASS nor FAIL;
- interrupted attempts must lack a receipt.

The two phases must test different ELFs.

The verdict is:
- NOT SUPPORTED if any patched replay FAILs;
- INCONCLUSIVE unless some unfixed replay FAILed at this checkout path, every
  such seed PASSed on every patched replay, and the patched PASS witness always
  PASSed;
- otherwise SUPPORTED, with caveats for any FAIL witness seed that did not FAIL
  unfixed at this path and for any patched run that did not PASS.

The comment embeds the local patch in a fence that is longer than any backtick
run in the patch. It states that it is not posted, that no Diff was created and
that no reviewers were assigned. Output files are opened exclusively.

`relocation.py` only extracts `_safehermit_fields` for reuse; its behaviour is
unchanged. No process, Hermit, Codex, state or pipeline code changed. Both
Codex compatibility keys remain blocked.
## Test plan
Pre-commit checks, under `tfa-phase2-three-case/gates/followup-20260925-r6-precommit/`:
- `arc f`, `arc lint`, `arc pyre check-changed-targets`: rc 0
- `tfa_phase2_test`: 187/187 (178 existing plus 9 new `FollowupTest` cases)

The new tests cover:
- a SUPPORTED render;
- a relabelled patched FAIL;
- no unfixed FAIL at the path;
- CENSORED and NONDETERMINISTIC counted as neither;
- a CENSORED replay of a failing seed;
- rc 124 without wall evidence;
- the witness-seed caveat;
- seven binding refusals;
- interrupted attempts.

The real ledgers under `followup/<case>/ledger.json` preview as follows.

thrift: SUPPORTED.
- Seeds 3 and 8 FAILed unfixed 5 times, and PASSed on all 6 patched replays.
- The manifest FAIL seed 1 PASSes unfixed at the stress path; this is carried
  as a caveat.

asyncssl: SUPPORTED.
- FAIL seed 11 was FAIL×5 and NONDETERMINISTIC×2 unfixed, and PASS×3 patched.
- Fresh seed 15 is OTHER in both phases. It is the unrelated
  `AsyncSSLSocketTest.cpp:3592` connect failure.

httpcoro: SUPPORTED.
- FAIL seed 0 was FAIL×3 unfixed and PASS×2 patched.
- Unfixed fresh seeds 5 and 6 were CENSORED.
- One unfixed attempt was interrupted.

The exact-head gates run afterwards under
`gates/followup-20260925-r6-<head>/`.

The signed Codex gate remains BLOCKED at 36/40, not green.

# D121848898 [testinfra][tfa] Bind the follow-up unfixed phase to the manifest test ELF (lines 27)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

A real-evidence negative control at `3eb9d118` found a gap. When the two
phases of the real thrift ledger were swapped, `render-followup` rendered the
ledger instead of refusing it. The verdict came out NOT SUPPORTED, because
the unfixed FAILs became "patched" FAILs. However, nothing tied the `unfixed`
phase to the manifest. A ledger could therefore offer any failing ELF as the
baseline and make an ineffective patch look discriminating.

`validated_followup` now requires the unfixed phase's `guest_sha256` to equal
the manifest `artifact_sha256`. It checks this before the existing check that
the two phases test different ELFs. The fixture now uses the case's
`artifact_sha256` for the unfixed ELF. A new binding subtest swaps the ledger
phases and expects the refusal. The README states the rule.
## Test plan
Pre-commit checks, under `tfa-phase2-three-case/gates/followup-20260925-r7-precommit/`:
- `arc f`, `arc lint`, `arc pyre check-changed-targets`: rc 0
- `tfa_phase2_test`: 187/187. The new check is a subtest of
  `test_refuses_runs_not_bound_to_their_phase_or_bytes`.
- All three real ledgers still validate, and their unfixed ELF equals the
  manifest artifact:
  - thrift: SUPPORTED, with the seed-1 path caveat;
  - asyncssl: SUPPORTED, with fresh seed 15 OTHER;
  - httpcoro: SUPPORTED.

Parent `3eb9d118` exact-head gates
(`gates/followup-20260925-r6-3eb9d118337a/`):
- `arc lint` and pyre: rc 0
- `tfa_phase2_test`: 187/187
- CLI built.
- The initial renders are byte-identical to `stage1-2-relocated`, and the
  follow-up renders are SUPPORTED for all three cases.
- All 8 `followup.py` mutations are caught by `FollowupTest`.
- The phase-swap real-evidence control was not refused, which is the gap fixed
  here.
- Containment was not run at that head.

The exact-head gates for this commit run afterwards under
`gates/followup-20260925-r7-<head>/`.

The signed Codex gate remains BLOCKED at 36/40, not green.

# D121848525 [testinfra][tfa] Render follow-up root causes as caveated hypotheses (lines 363)
## Summary
[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]

A scientific-clarity review of the phase-two evidence found that the
follow-up comments stated each investigator's root cause as a mechanism,
although the underlying reports qualify it:
- thrift is not instrumented;
- asyncssl's ACK ordering is inferred, and its production-drain alternative
  was rejected on precedent;
- httpcoro has no DBG drain log.

The comments also did not define run roles or the verdict rule, and they
gave no rerun command.

`render-followup` now:
- labels the root cause "Root-cause hypothesis (investigator candidate,
  rank r of n; not proven)";
- quotes the selected hypothesis's `contradicting` field;
- lists the other ranked hypotheses as not excluded;
- defines the `fail-witness`, `pass-witness`, `discriminator` and `fresh`
  roles;
- states the verdict rule. SUPPORTED means only that patched replays did
  not reproduce the FAIL. A same-seed patched replay is a different
  schedule. SUPPORTED is not native, CI, landed or production-containment
  validation.

The quoted `contradicting` and `claim` fields are investigator-controlled,
and `_validate_hypotheses` only requires non-empty strings. An adversarial
review found two problems with an earlier version of this change:
- It pasted those fields into the comment with only whitespace collapsed,
  so an investigator could inject Markdown, HTML, @mentions or unbounded
  text.
- It re-read `report.json` after `validate_candidate_report`, which left a
  TOCTOU window.

Now `validate_candidate_report` returns the ranked hypotheses and the
selected rank in `CandidateSummary`, built from the same parse it
validated, and the renderer reads only that snapshot. Each rendered agent
field goes through `_agent_text`:
- collapse to one line;
- drop Unicode control and format characters, including bidi overrides and
  zero-width characters;
- turn backticks into `'`;
- cut fields longer than `AGENT_TEXT_LIMIT` (1000) characters, with a
  marker. The longest real field is 751 characters.

A field that is empty after filtering is refused. The fields are rendered
inside a `text` code fence, which they cannot close. `root_cause` keeps its
existing strict one-line validation.

The comment also prints the exact receipt `cwd` and `argv` for the unfixed
FAIL and the patched replay of the lowest seed that FAILed unfixed. A
receipt without `cwd` is refused.

Judging and verdicts are unchanged. The `judge_run`, `_verdict` and
binding checks are untouched, and the three real `followup-runs.json` files
are byte-identical to stage 3.

The context-free phase-two summary is `tfa-phase2-three-case/RESULTS.md`,
in the evidence tree and not in this commit. The comments do not cite
containment, because it is environment-level. The signed Codex containment
gate remains BLOCKED at 36/40 by the four Codex 0.156.0 preflight cases.
That count is not a pass rate, and it validates no candidate fix.
## Test plan
Pre-commit:
- `arc f` and `arc lint` on the four files: no issues.
- `FollowupTest`: 16/16. It adds seven tests:
  - `test_root_cause_is_rendered_as_a_caveated_hypothesis`
  - `test_comment_gives_measured_commands_for_lowest_failing_seed`, with
    fail-witness and fresh-seed subtests
  - `test_refuses_receipt_without_cwd_for_its_command`
  - `test_hypothesis_text_keeps_ordinary_punctuation_as_plain_text`
  - `test_hypothesis_text_cannot_inject_markup_mentions_or_hidden_text`,
    covering fence breaks, HTML, @mentions, links, CR, bidi, zero-width,
    ANSI escapes and an all-invisible refusal
  - `test_overlong_hypothesis_text_is_cut_at_the_limit`
  - `test_renders_the_validated_snapshot_not_a_reread_report`
- No existing assertion was removed or relaxed.
- Sensitivity, in `gates/followup-r8b-sensitivity/` with a fresh pycache
  per run:
  - Against the previous `followup.py` (2c30cbed plus the constant), five
    tests fail, and it renders `@swapped` text that was written after
    validation.
  - Each of five mutations of the plain-text policy fails its own tests:
    `no-control-filter`, `no-backtick-escape`, `no-length-cap`,
    `no-text-fence` and `reread-report`.

The exact-head gates run after this amend under
`gates/followup-20260925-r8-<head>/`: lint, pyre, the full
`tfa_phase2_test`, renders, negative controls and containment.

# D121877477 [testinfra][tfa] Hash-bind every follow-up artifact in the ledger (lines 568)
## Summary
`render-followup` validated the candidate report and replay receipts from files
it reopened on each read, while the ledger named only their paths. A report,
patch, receipt, stdout, stderr or safehermit report changed after the ledger was
written was therefore read and trusted, and a file changed between validation
and rendering could reach the comment.

Ledger schema `tfa.followup-ledger.v2` records the kind, mode, path, size and
SHA-256 of exactly the candidate report, `hypotheses.json`, `patch.diff` and
`validation.json`, and of each run's `receipt.json`, `stdout.txt`, `stderr.txt`
and `safehermit.txt`. `followup.py` opens each file once with `O_NOFOLLOW`,
refuses it unless it is a regular file matching its record, and validates,
judges and renders only from those bytes. The report validator, oracle and
safehermit parser gain byte-input entry points (`validate_candidate_report_bytes`,
`check_oracle_bytes`, `_safehermit_text_fields`, `_gtest_execution_lines`,
`_gtest_fatal_lines`) so the path-reading callers are unchanged.

The ledger itself is not authenticated: it is a caller-supplied trust root, so
whoever writes it chooses what is bound. Its SHA-256 is printed in the comment.
This diff does not bind replay roles to manifest seeds.
## Test plan
New `FollowupTest` counterexamples, each leaving the ledger bytes unchanged:
appending a byte to any of the eight bound files, `chmod` of the patch, a
symlink in place of stdout, a FIFO in place of the patch and of an empty stderr,
and another run's receipt copied in, all raise `FollowupError`. A ledger that
omits, adds or misroutes a file record is refused, and resealing another run's
receipt is refused as not the attempt's own file. Overwriting every file right
after it is read leaves the verdict, commands and rendered comment unchanged.

Sensitivity mutants (each run against its focused test, source restored after):
dropping the SHA-256/size, mode, kind or record-path checks, dropping
`O_NOFOLLOW`, accepting extra candidate or run keys, rereading the patch or
stdout after binding, and dropping the own-file receipt check: 10/10 KILLED.

`arc lint`: no issues. `arc pyre check-changed-targets`: no type errors.
`buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test`: rc=0, Pass 199,
Fail 0, 12s.

# D121877478 [testinfra][tfa] Bind follow-up replay roles to manifest seeds (lines 168)
## Summary
The follow-up ledger labelled each replay with a role, and the verdict trusted
the label: a fresh-seed replay relabelled `pass-witness` satisfied the patched
PASS witness requirement, and a phase could omit either witness or replay
different seeds from the other phase.

Before any run is judged, `followup.py` now requires each `fail-witness` and
`pass-witness` run to be at the manifest FAIL or PASS seed, each `fresh` run at
a manifest fresh stress seed, and each `discriminator` run at a seed outside the
manifest. Both phases must replay both witnesses, and the patched phase must
replay exactly the unfixed roles and seeds. The comment's role legend now says
this.

A relocation that found a different witness seed than the manifest cannot be
followed up under this rule; that is deliberate, because the comment calls the
witnesses the manifest seeds.
## Test plan
New `FollowupTest` cases: every role relabelled onto a seed outside its role
(fresh as `pass-witness` or `fail-witness`, each witness seed as the other
witness, a witness as `fresh`, and a fresh or witness seed as `discriminator`) is refused before `judge_run` is called; a
`discriminator` at a seed outside the manifest is accepted and rendered; a
phase without either witness, or a patched phase that drops or adds a seed, is
refused.

Sensitivity mutants: accepting any seed for each of the four roles, refusing
every discriminator, dropping the witness-coverage or same-seeds check, not
calling the binding, and judging before binding: 9/9 KILLED. Letting
`pass-witness` accept the FAIL seed, `fail-witness` the PASS seed, or
`discriminator` the FAIL seed: 3/3 KILLED by
`test_each_role_is_bound_to_its_manifest_seed`.

`arc lint`: no issues. `arc pyre check-changed-targets`: no type errors.
`buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test`: rc=0, Pass 202,
Fail 0, 13s.

# D121877479 [testinfra][tfa] Keep task comments dry-run-only (lines 150)
## Summary
`assert_bot_authorization` accepted any caller-supplied JSON that claimed the
service user, `act_as_authorized: true` and a service principal, so a forged
`--authorization-proof` file could authorize a real `tasks.comment create`.

There is no trusted issuer of task/action-bound receipts yet, so this fails
closed:
- `assert_bot_authorization(proof, task_id, text)` keeps the three existing
  refusals, then refuses a proof not bound to the task, `tasks.comment create`
  and the comment text's sha256, and finally refuses every proof with
  `UNTRUSTED_AUTHORIZATION`.
- `_comment_stage` never posts. It lists, dedupes and runs only the
  `--dry-run` payload, recording an `authorization-blocked` status with the
  refusal reason.
- `post_comment` stays as the eventual bot-comment path, gated by the same
  check, so it cannot create until a trusted issuer replaces the final refusal.
## Test plan
- New tests: a perfectly shaped, task/text-bound caller proof is refused;
  `post_comment` with it never issues `create`; `_comment_stage` issues exactly
  one `create`, containing `--dry-run`, for no proof, a perfect proof and an
  unproven proof.
- Sensitivity mutants, all KILLED: accept the bound proof, drop the task
  binding, stop catching the refusal in `_comment_stage`, drop the refusal
  reason, drop `--dry-run` from the payload.
- `arc f`, `arc lint`, `arc pyre check-changed-targets`: clean.
- `buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test`: rc=0, Pass 205,
  13s.

# D121877480 [testinfra][tfa] Present the final comment's cause as an unproven hypothesis (lines 72)
## Summary
`final_text` rendered the investigator's `root_cause` as "- Root cause: ..."
under "TFA phase-two verified fix update". Nothing the controller runs (Buck,
lint, the original PASS/FAIL seeds, the fresh stress grid) proves the cause;
it only supports the patch.

- `ReportSummary` now carries `selected_hypothesis_rank` and
  `hypothesis_count`, taken from the validated (and, for qualified reports,
  sealed) candidate report.
- `final_text` refuses a rank outside 1..count or a count outside 2..5, titles
  the update "candidate fix", renders "Unproven root-cause hypothesis
  (investigator's selected rank r of n)", and states that passing controller
  checks do not prove it.
## Test plan
- New `test_final_comment_presents_an_unproven_ranked_hypothesis` checks the
  wording, the caveat and the absence of "Root cause" and "verified", and
  refuses invalid rank/count pairs. `validate_report` and
  `validate_qualified_report` tests assert the propagated rank and count (2 of
  3 for the qualified report).
- Sensitivity mutants, all KILLED: "Root cause" label, caveat removed, rank
  check removed, rank upper bound removed, qualified rank or count hardcoded,
  rank and count swapped.
- `arc f`, `arc lint`, `arc pyre check-changed-targets`: clean.
- `buck2 test fbcode//testinfra/tfa/tests:tfa_phase2_test`: rc=0, Pass 206,
  12s.

# D121877481 [testinfra][tfa] Exercise the eligible Codex launch with a hash-bound fixture (lines 588)
## Summary
Every production Codex compatibility entry is blocked, so the positive
real-agent containment tests could never reach an eligible launch: each one
asserted an "eligible" preflight that the pinned table forbids, and failed at
preflight. The eligible `run_process` path (preflight, payload binding, the
sealed wrapper's digest check, cgroup admission, supervisor and cleanup) had
no end-to-end coverage.

- `process.py` gains `_CODEX_ENTRYPOINT` (default `/usr/local/bin/codex`),
  read by `_codex_entrypoint_identity`, in the same style as `_TRUSTED_ROOT`
  and `_TRUSTED_OWNER_UID`. The production value and every gate are unchanged.
- `_eligible_codex_fixture` installs a test-owned launcher, release manifest
  and payload under a trusted test root. It adds one eligible compatibility
  entry, bound to the payload's SHA-256, with `patch.dict`, and leaves the
  production entries untouched. The payload runs one shell tool from the
  prompt and writes its stdout as the final message; it has no native sandbox.
- New `test_eligible_codex_fixture_runs_bound_agent_in_tfa_containment` runs
  fresh, resume and reviewer launches through the real `run_process`. It
  asserts:
  - an eligible preflight, with `payload_binding` and `payload_sha256` equal
    to the fixture digest;
  - that the tool ran in TFA's `payload` cgroup;
  - `containment_empty` and removal of the cgroup;
  - that `publish_codex_report` refuses a tool-planted symlink.
  It then refuses a blocked entry before launch, and refuses a changed payload
  behind an unchanged launcher before exec: the wrapper reports "payload
  identity differs from its reviewed digest" and the tool never runs.
- The four owner-managed real-release tests are kept, and their assertions
  are unchanged. `_owner_codex_is_eligible` runs the real preflight and
  accepts a refusal only when `identity.json` records `state=blocked` with the
  reviewed socket-isolation blocker. Any other refusal, such as a drifted
  release manifest or a missing executable, raises `AssertionError`. For that
  blocked release, three tests check the refusal
  ("native workspace sandbox preflight failed.*state=blocked") and that
  nothing launched: no process metadata, tool marker or final message. All
  four then call `skipTest`, so they report Skip, not Pass.
  `_preserving_production_directory` now lets `unittest.SkipTest` through and
  removes its root, instead of converting the skip into a preserved-evidence
  failure as it does for every other exception. Their native
  sandbox assertions (network EPERM, forbidden writes, reviewer symlink
  denial, controller handoff) run only once a reviewed release is eligible.
  The fixture does not stand in for them.
- New `test_owner_codex_eligibility_admits_only_the_reviewed_blocked_release`
  drives the helper through the fixture. It expects eligible to give True and
  the socket-blocked entry to give False. It expects `AssertionError` for
  another blocker, a missing (unsupported) entry and a drifted manifest.
- New `test_real_codex_tests_skip_only_on_the_reviewed_blocked_release` runs
  each of the four real-release test methods under the fixture. With the
  socket-blocked entry, each must report exactly one Skip with no failure,
  after exactly the refused launches its blocked branch asserts (one each,
  none for the handoff test), and nothing may be preserved. With another
  blocker, each must fail as "not the reviewed blocked release" without
  launching.
- The README says the fixture proves only the TFA-owned launch path, not
  Codex's network or filesystem restrictions.
## Test plan
- Sensitivity mutants, all KILLED by the intended assertion:
  - the entrypoint ignores `_CODEX_ENTRYPOINT`;
  - the spawn downgrades the binding to `unbound`;
  - the spawn omits the binding;
  - preflight admits `blocked`;
  - `_codex_payload_binding` ignores state and blocker;
  - the sentinel digest comparison is removed;
  - an eligible entry without a payload hash is accepted;
  - the helper admits any refusal, ignores the blocker, admits
    `unsupported`, or never reports eligible (each fails the helper test);
  - `_preserving_production_directory` swallows `SkipTest`, the helper skips
    on any blocker, the reviewer test returns instead of skipping, or the
    by-tool test skips before asserting the refused launch (each fails the
    call-site skip test).
- `arc f`, `arc lint`, `arc pyre check-changed-targets`: clean.
- `buck2 test fbcode//testinfra/tfa/tests:tfa_production_containment_test --
  --regex 'owner_codex_eligibility|eligible_codex_fixture|test_real_codex'`:
  rc=32, Pass 3, Fail 5. The fixture, helper and call-site skip tests pass. The five
  real-release tests fail on this host, as they did before this change:
  its owner-managed Codex release manifest no longer matches the pinned
  digest, so preflight refuses it with "release manifest identity differs"
  rather than the socket blocker, and the helper will not count that as a
  blocked-release control.

# D121978134 [testinfra][tfa] Pin the rolled-over owner-managed Codex releases as blocked (lines 47)
## Summary
The host's owner-managed Codex rolled over, so every real-release preflight
failed locally with "release manifest identity differs" instead of reaching the
reviewed blocked entry.

Observed identities (`codex --version`, rc 0, banners already reviewed):
- launcher `/usr/local/bin/codex_cli/codex`: `d22c52fa…` (was `71b8f5bd…`)
- stable `codex.real`: `4b776a3e…`, `codex-cli 0.156.1`. This is byte-identical
  to the manifest already recorded as the blocked 0.156.1 `latest` entry.
- latest `codex_latest.real`: `d291f08a…`, `codex-cli 0.156.1`, built by MSDK
  from fbsource `6be63a60915d` (2026-09-24).

Both stay blocked with the unchanged socket-isolation blocker. The owning lane
(devinfra_3p_ai, `third-party/codex/main/codex-rs`) has now landed the fixes:
`de089a75d6a1` imports upstream 0.157.0 (D121728319) and `18cdc3ab303f` accepts
verified Btrfs mount-device mismatches (D121708813). Both landed on 2026-09-25,
after the `6be63a60` build. `linux-sandbox/src/daemon_mounts.rs` at `6be63a60`
still rejects ancestor aliases and requires the mountinfo device to equal
`st_dev`. Master's latest MSDK `codex_latest` is built from `24281a08`, also
before both fixes. No installed or published release contains them, so no
entry becomes eligible.

- `_CODEX_RELEASE_MANIFESTS` pins the current stable and latest digests.
- `_CODEX_COMPATIBILITY` replaces the two unreachable 0.156.0/0.156.1 entries
  (old launcher) with the two current ones. Both are blocked, unbound, and use
  `_CODEX_SOCKET_ISOLATION_BLOCKER`.
- The unit and real-host tests pin the new digests. Their blocked, unbound and
  blocker-text assertions are unchanged.
## Test plan
Evidence: `/home/newton/work/tfa/codex-drift/AUDIT.md` (version output,
manifest `build_info`, source snapshots of `daemon_mounts.rs` at `6be63a60` and
master) and `/home/newton/work/tfa/codex-rollover/`.
- `buck test fbcode//testinfra/tfa/tests:tfa_phase2_test -- -r 'codex|release_manifest|compatibility'`:
  rc 0, Pass 20, 11.2 s.
- `buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test -- -r 'real_codex|owner_codex|eligible_codex'`:
  Pass 4, Skip 4, Fail 0, 2m42s (rc 64 is the skip exit). The real bootstrap
  test asserts the latest channel is refused as `blocked` with the new digests
  and blocker text. The four real-release tests assert the refusal and that
  nothing launched, then skip.
- `arc f`, `arc lint`: no issues. `arc pyre check-changed-targets`: no errors.
- Full `buck build fbcode//testinfra/tfa/...`: rc 0. Full `buck test
  fbcode//testinfra/tfa/...`: Pass 249, Fail 0, Skip 4, Omit 1 (rc 64), 494 s.
- Mutants (`codex-rollover/mutants*.out`): 5/5 killed. The stable-manifest digest
  flip survives the unit regex and is killed by the real-host containment tests.

# D121978137 [testinfra][tfa] Add a separately labelled external OSS Hermit prototype mode for witnesses (lines 1663)
## Summary
The packaged `a919cce` Hermit import cannot replace the preserved `4630ee2a` witness, so `packaged_hermit_argv` fails closed and no witness can run. The owner authorized the initial prototype to use the open-source Hermit before the internal package is ready.

This diff adds `external_oss_hermit_argv` and the `run-witness --external-oss-hermit-record PATH --external-oss-hermit-record-sha256 HEX` option pair. The mode:
- accepts only a record whose bytes match the given digest, with schema `tfa.external-oss-hermit.v1`, evidence class `external-oss-prototype`, and `packaged` and `production_qualified` false
- requires the record's executable path, bytes and source revision to equal the manifest's `hermit_*` witness, from `github.com/rrnewton/hermit` on a named branch, with a clean version stamp
- requires the reviewed safehermit and bounded-run-space digests and the manifest bounds
- requires `QUALIFIED` with no blockers, plus hash-bound passing strict-verify probes for a `/bin/true` control and for each case at its pass seed. Each case probe's argv must match the case `pass_argv` except for its report and verify-log paths.
- pins the same bytes at mode 0500 under `.tfa-external-oss-runtime/`, writes an `identity.json` with that label, and reports `hermit_evidence_class` in the witness output

The packaged path, its fail-closed states, and every downstream `hermit_sha256` comparator are unchanged. The mode is wired only into `run-witness`, not `run-case`, so no comment or ledger can present prototype evidence.
## Test plan
Exact-head evaluation of this commit's source tree (evaluated as `4fb8c412073909fad266c15eae8edc18373084bb`; the submitted commit differs only in its message, with an empty `sl diff` against it). Run by the hash-reviewed, frozen runner `resume-4fb8c4120739.sh` on 2026-09-26, 09:06:40-09:15:28 PDT, rc 0, with a clean `sl status` and the pinned `hermit.py` hash checked before containment, after containment and after the witnesses.

- `buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test`: rc 64 (skip warnings only), 316 s, trace `b4a124e7-3eaa-40b8-a30f-eb55c90b5b06`.
  `Tests finished: Pass 39. Fail 0. Timeout 0. Fatal 0. Skip 4. Omit 0. Infra Failure 0. Build failure 0`.
  The Buck command report (`exit_code` 64), `buck log what-ran` (43 unique `test.run` records bound to this trace) and the per-test TPX results agree: 39 pass. The only skips are the four policy-blocked owner-managed Codex tests, which assert the `blocked` refusal and that nothing launched: `test_real_codex_candidate_handoff_uses_controller_buck`, `test_real_codex_final_message_cannot_be_redirected_by_tool`, `test_real_codex_native_workspace_sandbox_and_tfa_cleanup`, `test_real_codex_reviewer_final_message_cannot_be_redirected`.
- Witnesses through `buck run fbcode//testinfra/tfa:tfa_phase2` on a disposable quota-enabled Btrfs fixture, with `--external-oss-hermit-record` `qual-4/external-oss-hermit.v1.json` (sha256 `5041afd1885026e5af5b441f6456b7a0c5122a3a042e7a1ff815ad32e999a3d2`): 8/8 rc 0 in 210 s. The steps were `validate-manifest`, `dry-run`, and `run-witness` for asyncssl, httpcoro and thrift, each at `--outcome fail` and `--outcome pass`. The six hermit-evidence reports show the expected fail/pass outcomes, `timed_out=false`, `truncated=false` and finalized bounded output; for example, httpcoro fail core-dumped (134) at 108 s, under the 120 s wall bound.
- Previous attempt (rerun 2, same frozen runner): containment qualified identically, but the httpcoro fail witness reached safehermit's 120 s wall bound (`unit_result=timeout`, exit 124), so the runner failed closed without a PASS. That evidence is preserved, and the retry used the unchanged bound, scripts and gates.
- Runner PASS marker sha256 `a25299b4a4da19bd163e720fea1fdab1da6ccfed83556957474792c2e89f507f`. Evidence: `/home/newton/work/tfa/external-oss-mode/` (`rerun3-4fb8c4120739/`, `rerun2-failed-4fb8c4120739/`, `containment-20260926T090640/`, `RUNNER-EVIDENCE-4fb8c4120739.txt`).
- `arc lint`: no lint issues.

