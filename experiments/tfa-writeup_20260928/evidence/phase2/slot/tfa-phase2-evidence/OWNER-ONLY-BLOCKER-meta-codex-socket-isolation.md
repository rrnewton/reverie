# Owner-only blocker: no signed Meta Codex with the Btrfs socket-isolation fix exists locally

Recorded 2026-09-25 by tfa-phase2-cyber-opus (claude-opus-5-5, devbig030). Mechanism and
evidence: diagnostic/ROOT-CAUSE-meta0156-v1.md ("Precise cause" section onward).

## Production result (unchanged, the only production claim)

`buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test` at fbsource
ef4885b60dc2: 36 pass, 4 fail, 0 skip (gates-tfa-checkout/production.*, 7m04s). The four
real-Codex cases fail closed because the installed signed Codex cannot start its sandbox on
this host:
- test_real_codex_native_workspace_sandbox_and_tfa_cleanup
- test_real_codex_candidate_handoff_uses_controller_buck
- test_real_codex_final_message_cannot_be_redirected_by_tool
- test_real_codex_reviewer_final_message_cannot_be_redirected

## What fails, precisely

Signed Codex 0.156.0 (stable) and 0.156.1 (latest) contain upstream's pre-fix
`linux-sandbox/src/daemon_mounts.rs::check_mounts`. It demands `mountinfo major:minor ==
st_dev` for the fixed socket directory `/tmp/codex-daemon-<euid>`. On devbig030, `/tmp` is Btrfs
subvolume `/tmpvol`: mountinfo reports device 0:42 and st_dev is 0:49. The check therefore
returns "cannot establish app-server socket mount isolation" before bwrap is launched.
There is no path, namespace, capability, environment or launcher fix: the directory is
hard-coded, and the Meta launcher only adds `-c` config flags. Aliases `/var/tmp` and `/data`
also expose that backing directory, so the fix must include alias masking as well.

Required upstream fixes, both needed, neither in upstream 0.156.1 (b412ff32):
- 7cec3e110ce6dc5c14704dfa06f6db9c23a530fc (mask alias paths instead of rejecting them)
- a708fc683934d7d7ea7560354968fedd2be28581 (accept the Btrfs subvolume device mismatch)

## Every locally available signed payload was checked; none carries the fix

Signed packages in ~/.codex/packages/meta-dotslash/releases, plus the dotslash extract:
| release dir | version | isolation check present | 7cec marker string | bin/codex sha256 (prefix) |
|---|---|---|---|---|
| 11dc4931 (latest; GC'd by the launcher after run A, identical to the extract) | 0.156.1 | yes | 0 | fdd42a7f |
| abcaf363 | 0.156.0 | yes | 0 | 76f060b9 |
| d6b5e2ce | 0.156.0 | yes | 0 | 258360ae |
| a73d81b5 | 0.155.1 | no (predates it) | 0 | af131cfb |
| ac6903f7 | 0.154.0 | no (predates it) | 0 | 4c5461a8 |
0.155.1 and 0.154.0 are selected by neither channel manifest. They would drop the daemon-socket
isolation property instead of satisfying it, so they are not a fix.

Installed identities:
- launcher /usr/local/bin/codex_cli/codex 71b8f5bde99ce8b4cff6ce7c7cbe79a0fe8d6bd36a4427e5d308bda4d065ebf0
- stable manifest codex.real 66494455a90b6f45f1fb93c73847d610337a12252a8f10a8a393f20ca213ae72
- latest manifest codex_latest.real 4b776a3e1d72706ab648db6fb24f5baa1a1ceaec2d4e3f07bd408db4dc0ecab5
  (linux/x86_64 digest ebf433a1...:98630430). Package: bin/codex fdd42a7f... (313250424 B),
  bin/codex-code-mode-host 28da077e..., codex-path/bwrap 10567ddd..., codex-package.json ca3d163b...

## Evidence that those two fixes are sufficient (diagnostic only, never production)

- Replaying the launcher argv and environment through `codex sandbox`: signed bin/codex rc=1 with
  the isolation error; unsigned candidate f9228bec... (0.156.1 + both fixes) rc=0 `inside-sandbox`
  (v5-sandbox-discriminator/D2.json, C4.json).
- v6: the candidate in a package layout whose other three files are byte-identical to the
  signed 0.156.1 package passes the four failing real-Codex TFA cases 4/4 (207.8s, receipt
  codex-meta0156-v6-diagnostic-receipt.json, production_eligible=false).
- v8/v8b, run from the Buck-built link tree with its fixtures: the valid 39-test population (40 minus the
  fail-closed gate test, whose assertion the override inverts) was 38/39 in v8 (581.8s), plus a successful
  single-test rerun v8b (1/1, 23.3s). There is no single clean 39-case run. v8's sole failure was a missing
  child PYTHONPATH in a non-Codex test; v8b reran that one test with PYTHONPATH exported.
  v7 (fbpython from source, no Buck fixtures, 25/40) is non-authoritative: 14 of its 15 failures were the
  missing `hermit-test-bin`, and 1 was the inverted gate test.
- The unsigned candidate is not in ALLOWED_EXECUTABLES, _CODEX_COMPATIBILITY or any manifest.
  It was injected only by an in-process test patch that restores the map afterwards.

## What only the owner / Codex-at-Meta release process can do

1. Import or cherry-pick 7cec3e110ce6 and a708fc683934 into fbsource `third-party/codex`
   (baseline f3319051bb78, 0.156.1). The patches apply cleanly: meta-source/patches/.
2. Build and MSDK-sign a complete package (bin/codex, bin/codex-code-mode-host,
   codex-path/bwrap, codex-package.json). The host binary cannot be built here because
   rusty_v8 150.4.0 prebuilts come from github.com, and this session's proxy returns
   CONNECT 403.
3. Publish it to a launcher channel (stable or latest manifest under /usr/local/bin/codex_cli).
4. Owner approval to seal its (version, launcher sha, manifest sha, channel) key as
   `eligible` in `_CODEX_COMPATIBILITY`, then rerun the 40-case production suite.
Until then, stable and latest stay fail-closed as sealed, and 36/40 is the accurate production result.

## Local commit for owner review (not submitted, not published)

/home/newton/work/tfa/fbsource head e324ed1880a44a731deec80271a84bb0ccf5ec3b (draft), which is ef4885b60dc2 with a
message-only change (`sl diff -r ef4885b -r e324ed1880a4` is empty): the who-am-i line is now
`[hermit2, tfa-phase2-cyber-opus, claude-opus-5-5, devbig030, role=impl]`, and the unit Test Plan line gives the
measured fresh-checkout result. Gates in this checkout: production 36/4/0 (7m04s), pyre rc=0, unit 131/132
(see gates-tfa-checkout/UNIT-131-OF-132-FINDING.md), validate-manifest rc=0 with 3 cases, arc lint rc=0,
arc f clean, `sl status` clean.

## Signed-identity watcher (started 2026-09-25)

- Script: `watch/watch_signed_codex_identity.py` (sha256 in `watch/watcher.sha256`). It is read-only: it lstats and hashes files, runs no Codex and writes only under `watch/`.
- Baseline (sealed in the script):
  - `/usr/local/bin/codex_cli/codex` 71b8f5bd…
  - `codex.real` (stable manifest) 66494455…
  - `codex_latest.real` (latest manifest) 4b776a3e…
  - `/usr/local/bin/codex` must resolve to `/usr/local/bin/codex_cli/codex`.
  - A change in either manifest's sha covers any change to the package digests it pins.
- Bound: it checks every 1800 s for 86400 s (48 checks), under `timeout 87000`. It re-hashes a file only when its inode, size, mtime or ctime moves, so there is no busy polling.
- Outcomes:
  - exit 3 and `watch/change-<time>.json`: identity changed.
  - exit 0: no change before the deadline.
  - Evidence files: `watch/heartbeat.log`, `watch/watcher.{out,rc}`, `watch/{started,finished}.txt`.
- On a change:
  1. Re-run the sealed preflight: `_codex_environment_preflight` against the new identity. It must report `state` and `blocker` from the unmodified `_CODEX_COMPATIBILITY`.
  2. Re-run the authoritative 40-case production suite: `buck test fbcode//testinfra/tfa/tests:tfa_production_containment_test`, with rc captured.
  3. A new signed package that is not in `_CODEX_COMPATIBILITY` stays fail-closed. Sealing it in the map needs owner approval. The unsigned candidate is never made eligible.
- On no change: stay idle. The blocker above stands.
- Slot heartbeat at 07:08Z was REFUSED (rc 3). The cause is an interrupted mutation journal that belongs to another agent: `worktrees/CREATE.9.devbig030.44.validate-fresh-3f464d9a6395-3892369-72df096d.journal`. That agent was a hermit validate-fresh checkout whose `git submodule update` hook failed; its owner pid 3892369 is gone and its `failure_policy` is leave-for-inspection. I did not run `wrkslots recover`, because it is not this slot's mutation.

## Exact-head review of e324ed1880a4 (2026-09-25): CHANGES REQUESTED

- Report: `review/REVIEW-e324ed1880a4.md`. The head was e324ed1880a4 at both the start and the end of the review. The review was read-only; the reviewer built and ran nothing. The tree is identical to ef4885b, which the owner reports as approved.
- Invariants held:
  - `ALLOWED_EXECUTABLES` is unchanged.
  - `_CODEX_COMPATIBILITY` has two keys, both `blocked`, and their hashes match the live host files.
  - The four real-Codex tests are unmodified and fail on the sealed blocker.
  - No cgroup-probe check was removed.
- **Precondition added to owner step 4 (medium finding, verified by reading the code):**
  - The compatibility key binds version, launcher, manifest and channel, but not the payload the launcher runs from `~/.codex/packages/meta-dotslash/releases/<id>/`. That directory is owned by newton with mode 755, so the user can write it.
  - `payload_wrapper.py` main() passes `argv[1:]` on to the supervisor without checking its identity.
  - Before any key is marked `eligible`, the payload bytes must be hash-bound to the key, or the approval covers whatever sits in that cache.
- **Low-medium finding, verified:**
  - `run_process` with `readiness_path` sets `payload_deadline` at readiness, so the total bound becomes startup + timeout. Only tests pass `readiness_path`.
  - Without it, the deadline starts at `started`, which is before spawn and the launcher re-hash.
  - Metadata records only `timeout_seconds`. The commit message does not disclose this change.
- Low findings, not yet verified by me:
  - The new launcher and manifest guards have no refusal tests.
  - The `/proc/<pid>/exe` check after launch can race the launcher's exec. It fails closed, but it will make runs flaky.
  - The blocker-text tests compare the constant with itself.
  - Two "latest" banners in the version-stderr allowlist are accepted for stable.
  - Nit: the README omits `/data`.
- Commit NOT amended. The two reviews of the same tree disagree: the owner-reported approval at ef4885b versus this CHANGES REQUESTED. Amending would invalidate the approved head, so whether to fix these before owner review is the owner's decision.

## Hardening commit 795572d12246 atop e324ed1880a4 (2026-09-25)

- A new local commit, not an amend and not published: `795572d12246 [testinfra][tfa] Hash-bind the Codex payload and unify deadlines`. After it, `sl status` is clean. Message: `commit-msg-hardening.txt`. The first body line is the live who-am-i tag, resolved from the slot with `DG_AGENT_NAME=tfa-phase2-cyber-opus`, because the bare resolver returned UNRESOLVED.
- Each e324ed review finding and how this commit addresses it:
  - **Payload.** The compatibility entry is now a 3-tuple with a payload sha256, and an eligible entry without a 64-hex digest is demoted to blocked. The wrapper hashes the payload through one O_NOFOLLOW descriptor, checks its identity, type, owner and write bits, and execs `/proc/self/fd/<fd>`. The sentinel records `payload-sha256`.
  - **The /proc/<pid>/exe race.** The post-spawn check is removed. In its place: a root-owned directory chain, a launcher descriptor re-check immediately before Popen, and the payload binding.
  - **Deadlines.** There is one origin, `payload-launcher-spawned`. The execution deadline is origin + timeout, and readiness waits until min(origin + startup, origin + timeout).
  - **Refusal tests.** 25 new tests cover owner, link-count, mode, symlink, directory-chain, manifest and channel refusals.
  - **Blocker text.** The tests now assert both upstream hashes literally.
  - **Channel.** It must be exact, and the banner is checked per channel.
  - **README.** It now documents `/data/tmpvol`.
- Mandatory correction (owner): `_codex_entrypoint_identity` now classifies the entry by lstat type and owner before readlink. The regular-file case asserts exactly `ownership or type is unsafe`, with no broadened alternation.
  - Mutation check: putting readlink back first makes the test fail with `does not match "Codex entrypoint is unavailable"`. `process.py` was then restored, and its sha was verified.
- `test_real_packaged_hermit_is_bound_to_blocked_provenance` now requires PROVENANCE_MISMATCH.
  - It asserts that the rebuilt resource digest differs from `PACKAGED_HERMIT_RESOURCE_SHA256`, the exact error prefix with the case witness, that nothing is pinned, and every recorded identity field.
  - Caveat: if the resource is ever re-pinned to a byte-identical build, this test must change with it.
- Two timeout tests moved from 0.3s/0.5s to 5s. The budget now includes launcher startup, and the guests still sleep 30s.
- Gates, run on the corrected tree (logs in `gates-hardening/`):
  - unit `tfa_phase2_test`: 157/157 in 15s (`unit2.out`);
  - pyre: no errors, 3 targets (`pyre2.out`);
  - arc lint and arc f: clean;
  - validate-manifest: 3 cases;
  - production `tfa_production_containment_test`: 36/40 in 5m28s, 07:53:25Z to 07:58:53Z (`production.out`). All 4 failures are the real-Codex agent cases, each failing closed on the Btrfs socket-isolation blocker; the "mount aliases" text appears 4 times. The real signed Codex version bootstrap passes through the descriptor-bound path.
  - A pre-correction run on the same tree, minus the lstat reorder, gave the same 36/4/0 in 5m34s (`production-pre-lstat.out`).
- Both production compatibility keys are still `blocked` with no payload digest. `ALLOWED_EXECUTABLES` is unchanged. The unsigned candidate is still diagnostic-only.
- The signed-package watcher is still running, and its heartbeat shows `changed=[]`.
