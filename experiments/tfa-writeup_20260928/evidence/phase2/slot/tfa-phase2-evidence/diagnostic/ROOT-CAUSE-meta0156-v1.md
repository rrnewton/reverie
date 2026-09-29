# Root cause: unsigned Meta Codex 0.156.1 diagnostic run v1 (2026-09-25T05:35:47Z)

Runner `run_codex_meta0156_diagnostic.py`, candidate sha256
f9228becc28fe4414ba4026db00818d7af48be83590cb6c9e857e64371abb88d (diagnostic only,
owner_approved=false, production_eligible=false). Checkout
/home/newton/work/tfa/fbsource at ef4885b60dc26a1a73321b83d834c97179e0ec64.
Result `codex-meta0156-four-cases.out`: 4/4 FAIL in 97.9s, rc=1.

## 1. Empty 48-hex nonce (three real-Codex cases)

Retained evidence (`retained-v1/{codex-report,reviewer-report}/.../control/diagnostic-agent/`):

- stdout item_0: `Code Mode is unavailable because failed to spawn code-mode host
  .../meta-build/target/release/codex-code-mode-host: host executable was not found.
  Code mode will fail closed`
- stderr: `ERROR codex_core::tools::router: error=failed to spawn code-mode host ...
  No such file or directory (os error 2)`
- final messages: `{"nonce":"","report_link":"tool-error: shell host unavailable"}` and
  `{"nonce":"","report_link":"shell-tool-error: code-mode host not found"}`; payload
  status exit 0.

Pinned source (fbsource f3319051bb78 third-party/codex, 0.156.1):

- `install-context/src/lib.rs:176-206` `code_mode_host_program()` resolves
  `codex-code-mode-host` as a sibling of `current_exe()` (no env override).
- `models-manager/models.json`: every catalog model except gpt-5.5 and gpt-5.4 is
  `"tool_mode": "code_mode_only"`.
- `core/src/tools/mod.rs:84-95` `effective_tool_mode` falls back to Direct only for
  `CodeMode`, never for `CodeModeOnly`; `core/src/tools/code_mode/mod.rs:108-121`
  emits the fail-closed warning seen above.

So the model had no working shell tool and reported empty strings. The candidate was
built with `-p codex-cli --bin codex` only; `codex-code-mode-host` was not built because
the MSDK recipe (`configure_rusty_v8_artifacts`, dotslash_build:150-200) downloads
rusty_v8 150.4.0 archives from github.com release assets, which this agent's proxy
refuses (CONNECT 403). The two fix commits touch only `codex-rs/linux-sandbox/*`.

## 2. "case checkout preflight failed" (controller-handoff case)

Retained `retained-v1/controller-handoff/.../pre-agent-checkout/{base,status}/`:
argv `/usr/local/bin/sl log -r . ...` / `/usr/local/bin/sl status --reason ...`, rc=2,
stderr `error: unexpected argument '-r' found ... Usage: codex`.

Cause is the diagnostic harness, not TFA: `process._spawn_contained` obtains the payload
wrapper from `_pinned_runtime()` for every contained launch; for non-Codex executables
the wrapper receives the real executable as argv[1]. The v1 chain wrapper replaced
argv[1] with the candidate unconditionally, so `sl` ran Codex. Only `/usr/local/bin/codex`
launches route their payload through META_EXEC_WRAPPER.

Fix (diagnostic runner only, no product change): `run_codex_meta0156_v2_diagnostic.py`
patches `_spawn_contained` to mark Codex launches and substitutes the candidate only for
those; non-Codex launches receive the unchanged sealed runtime. Launch counts are
recorded in the v2 receipt.

## v2 rerun (Codex-only substitution), 2026-09-25T05:42Z-05:45:30Z

`codex-meta0156-v2-four-cases.out`, rc=1, 4 tests in 201.2s, 4 failures, 0 errors.
Receipt `codex-meta0156-v2-diagnostic-receipt.json`: launches
`{"codex_substituted": 5, "non_codex_unchanged": 6}`, compatibility map restored,
candidate production_eligible=false.

- controller-handoff: checkout preflight now PASSES (pre/post-agent `sl` base/status
  retained); fails later with `ReportError: required candidate artifacts are missing:
  hypotheses.json, patch.diff, validation.json` across attempts 001-003.
- native-workspace, codex-report, reviewer-report: empty nonce, as in v1.
- All Codex runs in `retained-v2/` show `Code Mode is unavailable because failed to spawn
  code-mode host .../release/codex-code-mode-host` (9 occurrences). One remaining
  cause: the diagnostic candidate lacks `codex-code-mode-host`, which 0.156.1's
  `code_mode_only` catalog models require for any tool call.

## Production rerun preserved (2026-09-25T05:48:02Z-05:55:06Z)

`gates-tfa-checkout/production.out`, rc=32: Pass 36, Fail 4, Timeout 0, Fatal 0, Skip 0,
Infra Failure 0. The four failures are the real-Codex cases stopped by the signed
0.156.0 stable native-sandbox preflight (`_CODEX_SOCKET_ISOLATION_BLOCKER`). This is
the production result; nothing below changes it.

## Direct-mode model diagnostic: NOT RUN (condition false)

The production harness has no model override: `codex_arguments` (process.py:1420)
passes no `-m/--model`, `_CODEX_AGENT_CONFIG` (process.py:52) has no `model=` entry,
and `--ignore-user-config` excludes user config. None was added.

## v3: unsigned candidate + copied code-mode host (06:00:10Z-06:04:10Z)

`run_codex_meta0156_v3_diagnostic.py`, rc=1, 4 tests in 239.0s, 4 failures. Host
sha256 28da077e... copied from the local signed-latest extract. Code mode now worked
(no host warning); every agent reported `bubblewrap-unavailable`
(`retained-v3/*/final-message.json`, `review.json`, handoff `report.json`: "linux-sandbox
launcher panicked before executing the command").

Cause (pinned source): no system bwrap on this host (`/usr/bin/bwrap`,
`/usr/local/bin/bwrap` absent). `linux-sandbox/src/launcher.rs:50-54` panics when
neither PATH nor a bundled `codex-resources/bwrap` supplies one. The signed payload
ships `codex-path/bwrap` (bubblewrap 0.6.3, sha256 10567ddd...), and
`arg0/src/lib.rs:461-469` prepends `<package>/codex-path` to PATH only when the
executable is `<package>/bin/codex` beside `<package>/codex-package.json`
(`install-context/src/lib.rs:246-297`). `target/release/codex` is not in a package
layout. A hand-assembled layout (`meta-build/package/`) was built and then abandoned
unexecuted per owner instruction to run the complete signed payload as one unit.

## v4: complete signed latest 0.156.1 payload as one unit (06:08:44Z-06:12:25Z)

`run_codex_signed_latest_v4_diagnostic.py`, rc=1, 4 tests in 219.4s, 4 failures, 0 errors.
No binary copied or substituted: the runner sets `META_CODEX_RELEASE=latest` so the Meta
launcher selects `/usr/local/bin/codex_cli/codex_latest.real` (sha256 4b776a3e...,
SignedSource + msdk signature, package fbid 1608704314601842, commit 4d654ec9d26c), and
overrides only the compatibility verdict for that exact key inside the runner process
(restored in `finally`; receipt `production_eligible: false`, sealed state `blocked`).
A recording wrapper logs every payload exec and then execs the original argv unchanged.

- Payload identity (`v4-installed-package-identity.txt`): all 4 launches executed
  `~/.codex/packages/meta-dotslash/releases/11dc4931.../bin/codex` (the launcher's
  materialized package). Its bin/codex (fdd42a7f..., 313250424 B), bin/codex-code-mode-host
  (28da077e...), codex-path/bwrap (10567ddd...) and codex-package.json (ca3d163b...) are
  byte-identical to the dotslash extract of manifest digest ebf433a1...:98630430.
  The receipt field `payload_launches_all_signed_latest: false` compared against the
  extract path, not the materialized path; by content the launches are the signed payload.
- Result: code mode and bwrap both worked. The three cases with retained evidence
  (controller-handoff, codex-report, reviewer-report) failed with
  `error building bubblewrap command: cannot establish app-server socket mount isolation`
  (`retained-v4/*/final-message.json`, `review.json`, `stdout.txt`), raised at
  `linux-sandbox/src/daemon_mounts.rs:71` `check_mounts`. This host's /tmp and /var/tmp
  are Btrfs mount aliases (per-subvolume st_dev differs from the mountinfo superblock
  device); the Btrfs exemption is added by a708fc683934 (on top of 7cec3e110ce6), neither
  of which is an ancestor of upstream 0.156.1 b412ff32. This is exactly the sealed
  `_CODEX_SOCKET_ISOLATION_BLOCKER`, now observed on the signed latest package.
- native-workspace failed with an empty nonce (`four-cases.out:15`); that test retains no
  agent evidence, so its cause is inferred from the three retained cases, not observed.
- The 40-case signed-latest run was NOT executed: the owner's condition was that the four
  cases pass first. Production remains 36 pass / 4 fail / 0 infra.

## Precise cause of `cannot establish app-server socket mount isolation` (v5 discriminator)

Evidence in `v5-sandbox-discriminator/` (`host-mount-facts.txt`, `A*`/`B*`/`D2`/`C4` files).

Launcher vs payload: `A2-launcher-exec-record.txt` records exactly what
`/usr/local/bin/codex` (channel latest) execs: `<package>/bin/codex` with only `-c`
config flags (gateway provider, headers, otel certificates) prepended. There is no
namespace, unshare or PATH/TMPDIR rewrite (env values redacted except PATH, HOME,
XDG_RUNTIME_DIR, META_CODEX_RELEASE, META_EXEC_WRAPPER). The failure needs neither TFA
nor a model: `codex sandbox -- /bin/sh -c 'echo inside-sandbox'` fails immediately via
the launcher on both signed channels: A (latest 0.156.1) and B (stable 0.156.0), rc=1.

Controlled comparison (`replay_launcher_argv.py`): replay the recorded launcher argv and
environment, varying only argv[0]:
- D2 signed extract `bin/codex` fdd42a7f... -> rc=1, the isolation error.
- C4 unsigned candidate f9228bec... (0.156.1 + 7cec3e110ce6 + a708fc683934), with the
  signed package's own `codex-path/bwrap` on PATH -> rc=0, stdout `inside-sandbox`.

Mechanism (source `uds/src/daemon_directory.rs`, `linux-sandbox/src/daemon_mounts.rs`
at baseline 1b5c36b vs patched 9c5c733):
- The socket directory is fixed at `canonicalize("/tmp")/codex-daemon-<euid>` and
  deliberately ignores HOME, TMPDIR, CODEX_HOME and config. No env, wrapper or launcher
  setting can move it.
- On this host `/tmp` is Btrfs subvolume `/tmpvol` (mount 31979, device 0:42), and the
  opened directory reports `st_dev` 0:49 (the per-subvolume anonymous device).
  Baseline `check_mounts` selects mount 31979 by `mnt_id`, then returns `invalid()` because
  `mount_device (0:42) != st_dev (0:49)`. That `invalid()` is exactly this message. The check
  runs in the Codex process while it builds the bwrap argv, before bwrap is launched, so no
  capability or user namespace is involved.
- Fix a708fc683934 permits that mismatch only when the fd and its unique mount are both
  verified Btrfs. The host also exposes the socket directory through ancestor aliases
  `/var/tmp` (32001, root `/tmpvol`) and `/data` (31868, root `/`), which baseline code
  rejects outright. Fix 7cec3e110ce6 masks those aliases instead, and a708 is built on it.
  Both are required.
- Binary corroboration: the 7cec-only string `app-server socket mask path must be valid
  UTF-8` occurs 0 times in every materialized signed payload (0.154.0, 0.155.1, 0.156.0 x2,
  0.156.1) and once in the candidate. 0.154.0 and 0.155.1 lack the daemon-socket
  isolation code entirely (the error string occurs 0 times). They are older, are not
  selected by either channel manifest, and would remove a sandbox property, so they
  were not used.

## v5: candidate + signed bwrap on PATH only (06:22Z-06:26Z) - instrument failure

rc=1, 4 failures in 240.8s. TFA's `_CODEX_AGENT_CONFIG` sets
`shell_environment_policy.set.PATH="/usr/local/bin:/usr/bin:/bin"`, so the sandbox helper
never saw the PATH prefix: codex-report and reviewer reported `bwrap-unavailable`. In the
handoff case, on retry the model wrote the artifacts with apply_patch and invented the
probe values (nonce `6f8e2d4c...`, `errno:13,succeeded`) without running the command
(`retained-v5/controller-handoff/.../stdout.txt`: the first turn reports bwrap
unavailable, the second contains only a file_change item). TFA rejected this at
test_production_containment.py:3460 (network must be `errno:1`), so the evidence gate
caught the fabrication.

## v6: candidate executed through a package layout (06:27Z-06:31Z) - 4/4 PASS (diagnostic)

`run_codex_meta0156_v6_diagnostic.py`, rc=0, 4 tests OK in 207.8s. Receipt
`codex-meta0156-v6-diagnostic-receipt.json`: launches {codex_substituted 7,
non_codex_unchanged 18}, compatibility map restored, production_eligible false. The
candidate (same inode as target/release/codex) runs as `meta-build/package/bin/codex`
beside `codex-package.json` (ca3d163b...), `codex-path/bwrap` (10567ddd...) and
`bin/codex-code-mode-host` (28da077e...). Each of these is byte-identical to the signed
0.156.1 package, so the only difference from signed latest is the Codex binary. The two
fixes are therefore sufficient for the four real-Codex cases on this host.

## v7: the same substitution over the whole 40-test class via fbpython (06:32:55Z-06:41Z) - instrument-limited

rc=1. Ran 40 tests in 499.9s: 25 ok, 12 failures, 3 errors
(`codex-meta0156-v7-40case.out`, receipt `codex-meta0156-v7-40case-diagnostic-receipt.json`: launches
{codex_substituted 7, non_codex_unchanged 164}, compatibility map restored). Every failure was classified:
- 14 of 15: `FileNotFoundError ... tests/hermit-test-bin`. Buck places `hermit-test-bin` (and `safehermit`, etc.)
  next to the test as resources (tests/BUCK `resources`), and the test finds them with
  `Path(__file__).with_name(...)`. Running from source with fbpython has no such files. These tests never launch
  Codex, and all of them pass in the Buck production run (36/4/0). This is a runner artifact, not a Codex result.
- 1 of 15: `test_real_codex_bootstraps_with_bound_agent_identity`, line 3201 `ProcessError not raised`. This test
  asserts that production preflight REFUSES signed latest ("owner-managed Codex lacks daemon-socket fixes").
  The diagnostic forces every compatibility key to eligible, so this failure is required. It shows that the
  fail-closed gate is live, and that a diagnostic override cannot pass it.
- All four target real-Codex cases passed again inside the full-class run (handoff, final-message,
  native-workspace, reviewer).
A 40/40 number cannot come from any diagnostic: the gate test at line 3201 blocks it by design until an
owner-approved signed build is sealed. v7 is kept only as NON-AUTHORITATIVE diagnostic evidence; the valid
rerun is v8/v8b below.

## v8/v8b: the valid 39-test diagnostic population, run from the Buck link tree (diagnostic only)

Fixtures: `buck2 build fbcode//testinfra/tfa/tests:tfa_production_containment_test` rc=0 (v8-buck-build.out).
The runner runs from the link tree's own `_bootstrap.sh` and bundled Python 3.12, so `hermit-test-bin`
(sha256 4e8e438148c7...) sits next to the test module. The population is 40 minus one:
`test_real_codex_bootstraps_with_bound_agent_identity` is excluded because it asserts production REFUSES
signed latest, and forcing the ineligible same-version key eligible inverts that assertion. The runner
checks the population and records the exclusion reason in the receipt. The candidate substitution
(package layout, f9228bec..., production_eligible=false) is identical to v6.
- v8 (06:46:05Z-06:55:49Z): Ran 39 tests in 581.8s, 38 ok, 1 failure, 0 errors; launches {codex_substituted 7,
  non_codex_unchanged 164}; map restored. All 4 real-Codex blocker cases ok. The sole failure,
  `test_killed_worktree_body_is_reconciled_by_independent_owner`, does not involve Codex. Its
  `/usr/local/bin/fbpython -c 'import testinfra...'` child hit `ModuleNotFoundError: No module named 'testinfra'`
  because the link-tree launch exported no PYTHONPATH to children (v7 had set the source PYTHONPATH).
  Bundle retained at retained-v8/.
- v8b: that single test, same link tree, with `PYTHONPATH=<link tree>` exported: Ran 1 test in 23.3s, OK, rc=0
  (receipt codex-meta0156-v8b-worktree-crash-diagnostic-receipt.json).
Diagnostic candidate result: 4/4 blocker cases (v6, repeated in v8); the valid 39-test population was 38/39 in v8
(581.8s) plus a successful single-test rerun v8b (1/1, 23.3s). There is no single clean 39-case run. This is NOT
production-signed 40/40. The 40th test is the fail-closed gate that must refuse every
unsealed Codex, and the candidate is unsigned and not owner-approved.
