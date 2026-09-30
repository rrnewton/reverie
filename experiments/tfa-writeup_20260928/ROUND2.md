# TFA round 2: evidence copy, loader-bug fix, seeds on current Hermit main

[hermit2, sub-tfa2-030, opus, devbig030, role=impl]

This round was dispatched by coord2 on 2026-09-29 at 15:39Z, working from the owner decisions in [WAY_FORWARD.md](WAY_FORWARD.md). The rules were a 4-hour cap, the 3 known tests only, 6 seeds per test, and a 30-minute wall cap per test. Where the owner has not decided, nothing was done: no diff was published, abandoned or given reviewers, and nothing was sent to the AsyncSSL owners.

## Summary

| Goal | Status | Result |
|---|---|---|
| 1. Curated phase-two evidence copy | done | dev-hermit main `30bb84e0f1e3d7343b6b2f7fa7b83cbf864768f1`: 9,327 text files (28.1 MB) in [evidence/phase2/](evidence/phase2/), plus a 60.9 MB archive of 214 large logs outside git |
| 2. Fix the loader bug (https://github.com/rrnewton/hermit/issues/3328) | fixed in an approved PR; not landed during round 2. **Update, 2026-09-30:** coord2 landed it as hermit main `3b5f7cdc43247abad79585feae1469e23b69f241`. | https://github.com/rrnewton/hermit/pull/3374, APPROVE at exact head `558df1a7f`; the new test fails without the fix (EFAULT, exit 101) and passes with it, and I re-ran both myself; the PR head is now `c6867781` (rebased onto main `98a621f42`, re-approved); the required full local validation could not be started from this session (section 2), so it was handed to coord2 |
| 3. Re-find seeds on current Hermit main | done | a passing and a failing seed for all 3 tests on main `4101426a`, 18 of 18 trials deterministic, 4 min 25 s in all (section 3) |

## 1. Curated phase-two evidence copy

- **What was copied:** reports, logs, JSON results, verdicts, patches and small scripts from:
  - `worktrees/slots/tfa-phase2-cyber-opus/ignored/{tfa-phase2-evidence,tfa-phase2-three-case}`;
  - coord2's preserved copy of `/tmp/tfa-phase2*`.

  Only text files of at most 1 MiB are tracked. [evidence/phase2/MANIFEST.md](evidence/phase2/MANIFEST.md) and `MANIFEST.tsv` give each file's original path, size, sha256 and redactions.
- **Large logs:** the 214 text files over 1 MiB (1,268,882,584 bytes, mostly raw Hermit run logs) are in `ignored/tfa-phase2-archive/tfa-phase2-large-text-20260929.tar.zst` on devbig030, outside git:
  - 60,864,481 bytes, sha256 `92b68b4d7456bac5dae12b1a63747e97ad98e265810cf6444a17fe1f4a664c7b`;
  - `zstd -t` passes and `tar -t` lists all 214 files;
  - the output went through `head -c 524288000` as a 500 MB cap, and at 60.9 MB nothing was cut off.
- **Not copied:** 11.7 GB of Codex build output, a 1.39 GB unsigned Codex binary, two source-tree copies, 243 MB of Python bytecode and caches, and the slot's EdenFS fbsource checkout, which was never walked. Nothing in the source slot was moved or deleted.
- **Changes made while copying:** 40 files had expired Devmate signed-link signatures replaced with `REDACTED`. 89 `*.log` files are stored as `*.log.txt`, because of `.gitignore` line 94.
- **Disk:** `df -h /home/newton` read 253G free before the copy and 251G free after.
- **Landing, done differently from the dispatch in two places:**
  - I built the commit with a temporary git index (`GIT_INDEX_FILE`, `read-tree origin/main`, `add`, `write-tree`, `commit-tree -p origin/main`) instead of `git worktree add`. A full dev-hermit worktree is over 18 GB, on the disk that filled twice this week. The effect is the same: a clean base at origin/main, and only these paths.
  - I pushed with `git push origin <sha>:refs/heads/main` through `hermit/agent-utils/bin/herdr-run`, capturing the exit status without a pipe. Then I confirmed with `git ls-remote` that main points at `30bb84e0…`. `ci-hub/bin/git-push-verified` cannot run through `herdr-run`, which only allows bare `git`, `gh`, `cargo` and `ci-hub-cli`.
- **Read back by content:** after a fresh fetch, `origin/main:experiments/tfa-writeup_20260928/evidence/phase2` has tree `b88bb42c760a3252d42e6bd29770da6ddfa2f253`, identical to the tree I built, and all 9,330 paths are present.

## 2. Loader-bug fix (https://github.com/rrnewton/hermit/issues/3328)

**Pull request:** https://github.com/rrnewton/hermit/pull/3374, "Keep openat results when the stack below rsp is unwritable". It is open against hermit `main`, and its head is `f3f1e34bfaf7a244976da4569ddf7f42fd1de055`. The subagent sub-tfa2-loader wrote it in slot `worktrees/slots/sub-tfa2-030-loader`, as two commits:

- `532416618`: tests only;
- `f3f1e34bf`: the fix.

The pull request's `Owner:` line names sub-tfa2-030.

**What changed** (`detcore/src/syscalls/files.rs`):

- `inject_fstat` still puts its `struct stat` buffer below `rsp` first, so guests whose stack has room behave exactly as before.
- If that fails with EFAULT, it repeats the same `fstat` into a temporary private anonymous page, then reads the buffer back and unmaps the page. The page is created with an injected `mmap`, and Detcore already uses this pattern for its atomic-writev scratch in `helpers.rs`. The page's address never reaches the guest.
- If no scratch memory can be obtained at all, `add_fd` closes the descriptor instead of leaking it.
- A second bug is fixed too: the old `stack.commit()?;` dropped the guard that keeps the stack scratch alive before the injected `fstat` ran. On the DBT and SaBRe backends that guard owns the buffer's heap memory, so the kernel wrote into freed memory. The guard is now held until the buffer is no longer used.

**Tests fail before the fix and pass after.** I re-ran them myself, separately from the author, in slot `worktrees/slots/sub-tfa2-030-seeds`:

| Commit | `cargo test -p hermit-detcore --test tests_misc tight_stack_openat -- --nocapture` | `cargo test -p hermit-detcore --lib inject_fstat_scratch` |
|---|---|---|
| `532416618` (tests only) | **fails**, exit 101: `openat(... "/dev/null" ...) = Err(Errno(EFAULT))`, then "raw openat with 192 writable bytes above a GuardPage region returned -14; natively it returns the lowest free descriptor 3" | the author reports 0 of 3 passing; I did not re-run this one |
| `f3f1e34bf` (fix) | **passes**, 1 test, 9 s including the build. Each tight-stack case logs "Guest stack scratch cannot hold the fstat buffer for fd 3; using a transient page instead." | **passes**, 3 of 3 |

The integration test covers four cases: 512 writable bytes below `rsp` (the control), 192 and 0 bytes above a `PROT_NONE` page, and 192 bytes above an unmapped page. Each case must return the lowest free descriptor with the same mode, device and inode as an ordinary open, read EOF, and leak nothing. The author also checked each guarded line by deliberately breaking it in turn; every break failed its intended test.

**The original reproducer**, `experiments/tfa-openat-stack-size-20260917`, run under `hermit run --strict` through `bin/safehermit`:

- 192 bytes: `openat raw_rc=-14 errno=14` on the base, `raw_rc=3 errno=0` with the fix;
- 512 bytes: `raw_rc=3` on both.

With the fix, the 192- and 0-byte cases also pass `--verify --verify-strict`: `bitwise_parity: true`, 125 of 125 INFO messages matched, ptrace backend, default log level, no relaxations. This is the author's measurement; the logs are in `/tmp/sub-tfa2-loader/probe-*` on devbig030.

**Other checks the author ran at the fix head:**

- `hermit-detcore --lib`: 848 passed.
- `tests_misc`: 28 passed, plus the 5 tests the CI skip list excludes, run separately.
- `tests_time`: 19 passed, run serially.
- Five hermit-cli file and fd tests passed.
- `cargo fmt --check` and `clippy -D warnings` are clean.
- Pinned test counts in `ci/`: `test.detcore_unit` goes from 845 to 848 and `test.detcore_misc` from 27 to 28; `generate-validation-dag --check` passes.

**Not done:**

- The full validation driver. Local full validation of hermit main has not been green since run 1777 on 2026-09-11 (`ci-hub validate-status`: 588 commits since green).
- DBT, SaBRe and KVM runs.
- Retrying the three Buck targets this bug stopped. Their test binaries were evicted from buck-out, and rebuilding them needs an fbsource build on an OnDemand node.

**Review:** one independent adversarial review by sub-tfa2-review, a fresh subagent that did not write the change.

- **Round 1**, at `f3f1e34bf`: https://github.com/rrnewton/hermit/pull/3374#issuecomment-5895062164, **CHANGES-REQUESTED**. It found no code defect. Its two blocking items were about the evidence:
  - a false "tcache abort" claim in the commit message and the pull request description;
  - checks labelled as run at the head that had run on the uncommitted tree.
- **Fixed by:**
  - a message-only amend to `558df1a7f6030f8e9f04def079a12a617fabb51a`, with the same tree `6d57ac81f705ee9459c69ebbcceec8a66a11941a` (force-pushed with a lease);
  - re-running the checks at the exact head tree: 848 lib tests, 33 misc tests run serially, 19 time tests, fmt, clippy, the DAG check, and the probe with strict verify (125 of 125 INFO messages matched).
- **Round 2**, at `558df1a7f`: https://github.com/rrnewton/hermit/pull/3374#issuecomment-5895241478, **APPROVE**.
- **Non-blocking follow-ups:**
  - N1: when munmap fails, fail closed instead of warning.
  - N2: remove the duplicated scratch-page setup.
  - N3: three test gaps.
  - N4: the same use-after-free at four other sites, filed as https://github.com/rrnewton/hermit/issues/3376.

The change meets none of the four `post-facto-human-review` triggers (hermit `.llms/skills/post-facto-review/SKILL.md`), so one review suffices.

**Landing: not done. The exact-head validation could not be started from this session.** Landing needs a full local validation run at the PR's exact head, and each attempt below was refused before the validation DAG ran:

| Time (UTC) | Target | Launch | Refusal |
|---|---|---|---|
| ≈17:32 | `eebc63f33` (rebased onto `82e24cc0e`) | `ci-hub validate-run --materialize-target --dry-run` | "read-only entry classification budget expired while reading ignored/validate/runs/hermit-pressure-test.json". The dry-run retry was admitted (830 run handles, 408 s). |
| 17:43 | `eebc63f33` | `ci-hub validate-run` | "validation target … does not contain freshly fetched origin/main. Rebase before validating." Hermit main gained 5 commits while the run waited about 3 minutes for admission. |
| 17:51 | `c68677817` (rebased onto `98a621f42`) | `ci-hub validate-run` | "explicit DEV_HERMIT_TOOL_ROOT /home/newton/work/dev-hermit is dirty: M hermit". The shared parent checkout's `hermit` gitlink was already modified when this session started. |
| 17:58 | `c68677817` | `scripts/hermit-operational-tick --run-current-tool --tool-relative ci-hub/ci-hub -- validate-run …` | "cannot resolve current origin/main". Fetching dev-hermit returned GitHub CONNECT 403 from this session. |
| 18:01 | `c68677817` | same, with `--tool-parent-sha addc63df…` (newest cached tool tree) | "fd-backed DEV_HERMIT_TOOL_ROOT lacks the installed-bootstrap, cache, or exact-parent handoff metadata" |

The PR head is now `c6867781717df87837b268a0310d901be4c5bb4c`:

- commits `9ddad7428` (tests) and `c68677817` (fix);
- `git range-diff` shows both commits as `=` against the approved `558df1a7f`;
- `generate-validation-dag --check` is fresh, and the new test passes at the rebased heads.

The reviewer re-approved after each rebase:

- https://github.com/rrnewton/hermit/pull/3374#issuecomment-5895574046 at `eebc63f33`;
- https://github.com/rrnewton/hermit/pull/3374#issuecomment-5895629743 at `c6867781`.

The PR's `Owner:` line was released at 18:06Z, and coord2 was asked to run the validation and land.

Two refused runs left temporary checkouts, which the cleanup fence kept:

- `worktrees/validate/validate-fresh-eebc63f33646-1731885-676ae801`
- `worktrees/validate/validate-fresh-c6867781717d-2362802-9068cd11`

**Related exposure, not fixed (listed in the pull request):** 23 other Detcore call sites also stage scratch memory below `rsp`. The worst two:

- `helpers.rs` `zero_timespec`, used by ppoll, FUTEX_WAIT and rt_sigtimedwait, calls `.expect()` on the scratch commit, so Hermit would panic.
- `observe_timer_slack_identity` runs after a successful open of `/proc/<tid>/timerslack_ns`, so it can still turn that open into an error.

## 3. Seeds re-found on current Hermit main

**Result:** every one of the three known tests has a passing seed and a failing seed on current Hermit main, and all 18 trials passed Hermit's strict run-to-run check (verify).

- AsyncSSL and HTTPCoro reproduced their campaign seed pairs exactly.
- Thrift kept its passing seed 0. Its old failing seed 1 now passes, and seeds 3 and 4 fail with the declared assertion.

**Setup:**

- **Hermit:** main `4101426afa068e87048ef4cda2f182f87d32eb4a`, which includes `350fa554b`. Release build: `cargo build --release -p hermit --bin hermit`, 1 min 27 s. The binary is 69,063,272 bytes, sha256 `b4b30c7d7e9d9569184e9bc05cd7f8945640342c5ad61230f4765e81ed147537`, and reports `hermit 0.2.0 (2026-09-29, g4101426afa06)`.
- **Configuration:** each test used the campaign's winning configuration, taken verbatim from the tracked `experiment.json` (`cells[0].hermit.argv`). Only four things changed:
  - the Hermit binary;
  - the swept `--sched-seed`;
  - the verify log directory;
  - the `bin/safehermit` deadline, 300 s instead of 120 s. That deadline belongs to the wrapper and does not change Hermit's behavior.
- **Test binaries:** the campaign's Buck test binaries, still present with the recorded sha256:

  | Test | sha256 |
  |---|---|
  | HTTPCoro | `00066235…` |
  | Thrift | `2ec5d212…` |
  | AsyncSSL | `47b6c727…` |

- **Budget and machine rules (coord2 decisions 8 and 3):** 6 seeds per test, run one at a time under `nice -n 10 ionice -c3`, every run through `bin/safehermit`. Each trial is one `--verify --verify-strict` invocation, which runs the test twice and compares the runs.
- **Wall time:** 2026-09-29 15:46:25Z to 15:50:50Z, 4 min 25 s in all. Per test: Thrift 29.1 s, AsyncSSL 38.2 s, HTTPCoro 198.1 s.

| Test | Seeds (campaign pair first) | PASS | Declared FAIL | Other | Strict verify | Campaign pair |
|---|---|---|---|---|---|---|
| AsyncSSL `ObserverAttachedBeforeConnect` (slowdown up to 8x, 5 ms timeslice) | 1, 11, 0, 2, 3, 4 | 1, 3, 4 | **11** | 0, 2 | 6 of 6 deterministic | pass 1, fail 11: **reproduced** |
| Thrift `snapshotStartedProcessingTest` (slowdown up to 10x, 1 ms timeslice) | 0, 1, 2, 3, 4, 5 | 0, 1, 2, 5 | **3, 4** | none | 6 of 6 | pass 0 reproduced; fail seed 1 now **passes** |
| HTTPCoro `ResetRateLimit/h2` (clock multiplier 0.5, slowdown up to 10x) | 3, 0, 1, 2, 4, 5 | 1, 3, 4, 5 | **0, 2** | none | 6 of 6 | pass 3, fail 0: **reproduced** |

**How each failure was checked** (from `stdout.txt` and `stderr.txt` under `round2/runs/`):

- **AsyncSSL seed 11:** both declared blocks appear:
  - `AsyncSSLSocketTest.cpp:3797` ("has a size that is >= 4");
  - `:3812` (expected ACK offset 501, got all zeros).

  Exactly 1 test ran, and it is the one listed as failed. Seeds 0 and 2 failed differently, at `AsyncSSLSocketTest.cpp:3592`. The campaign also saw this failure and did not count it, so it is "Other" here.
- **Thrift seeds 3 and 4:** `InstrumentationTest.cpp:836`, where `reqSnapshots.size()` is 2 instead of 3. Seed 4 exits 66 because ThreadSanitizer also reports a data race in `ThriftServer::stop()` after the assertion. The campaign's rule counts output after the assertion block only as fallout, so seed 4 still counts as the declared failure.
- **HTTPCoro seeds 0 and 2:** `HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()`, then SIGABRT (exit 134).

**Compared with the campaign (2026-09-21, campaign Hermit `4630ee2a`):**

- The HTTPCoro failing seed takes 70.4 s per trial on the release build, against about 110 s on the campaign binary. That is no longer close to the 120 s limit.
- HTTPCoro seed 2 timed out at 121 s in the campaign; now it is a declared failure at 99.7 s.
- HTTPCoro seed 1 had differing runs in the campaign; now it is a deterministic pass.

**Control run:** in the campaign, AsyncSSL seed 3 in this same configuration diverged at `clock_gettime(CLOCK_MONOTONIC)`. That is the example in https://github.com/rrnewton/hermit/issues/3329. Today the campaign binary itself gave a deterministic pass for seed 3 in 2 of 2 trials (4.0 s and 4.1 s). So that divergence is intermittent, not fixed on main. I added this to the issue: https://github.com/rrnewton/hermit/issues/3329#issuecomment-5893770870.

**Artifacts** (tracked, 178 KB):

- `round2/runs/<test>/seed-NN/`: `argv.json`, `result.json`, `stdout.txt`, `stderr.txt` and `safehermit.txt` for each trial.
- `round2/control-4630ee2a-rep{1,2}/`: the control runs.
- `round2/refind_seeds.py`: the runner.
- `round2/runs-attempt1-missing-keep-logs/`: a first attempt in which all 18 invocations exited 2 at once. Current `--verify-log-dir` requires `--keep-logs`, and the runner had dropped that flag.

Verify logs were deleted after each deterministic trial. `safehermit.txt` records `bound.disk=NOT_APPLIED` (per-run subvolumes are disabled), so deleting them is what bounded disk use.

## Other GitHub records from this round

Each body was read back and matched what was sent byte for byte.

- https://github.com/rrnewton/hermit/issues/3329#issuecomment-5893770870: the few-nanosecond clock divergence under per-thread slowdown is intermittent, not fixed on main (the control run in section 3).
- https://github.com/rrnewton/hermit/pull/3374#issuecomment-5895062164: the independent review of the loader fix, posted verbatim.
- https://github.com/rrnewton/hermit/issues/3376: the reviewer's N4 finding. Four more Detcore sites drop the stack guard before an injected syscall uses the scratch buffer, a use-after-free on DBT and SaBRe. The two `utime` sites may also be hit by the known write-before-commit hazard.

## Owner decisions still open (untouched)

#1 abandon or keep the 22 fbsource drafts; #4 send the AsyncSSL patch; #5 posting channel; #7 review D121815443.

## What next (suggestions, not actions)

1. Land https://github.com/rrnewton/hermit/pull/3374 (coord2): run the exact-head local validation for `c6867781` from a clean tool root, then `ci-hub/landing/land-pr.sh 3374 tfa2/inject-fstat-efault`. The most recent hermit full validation on this box passed 272 of 272 in 2,691 s (`ci-hub local-history`, 2026-09-29 08:09Z). Then retry the three targets the loader bug stopped. That needs their Buck test binaries rebuilt on an OnDemand node: LogStore and McRefill were evicted from buck-out, and the transport monitor's state is unchecked.
2. Record the new seed sets as the campaign's "current" witnesses. Thrift is now pass 0 / fail 3 on main; AsyncSSL (1/11) and HTTPCoro (3/0) are unchanged. A later loop should re-find seeds on every Hermit bump rather than trust old ones.
3. Fix https://github.com/rrnewton/hermit/issues/3376 and the review's N1 to N3 follow-ups on https://github.com/rrnewton/hermit/pull/3374.

## Resources

- **Wall time:** the round ran from 15:39Z to 18:10Z (2 h 31 min).
  - Seed re-find: 4 min 25 s.
  - Control runs: 8 s.
  - Evidence curation and archive: about 3 min of compute.
  - Hermit builds: release 1 min 27 s; debug at the PR head 47 s.
  - Exact-head test re-runs: about 3 min.
  - Subagents: the loader-fix author ran about 71 min, and the reviewer about 19 min plus three short re-reviews.
  - Validation attempts: 5 launches, all refused before the DAG ran (about 20 min of admission time in all).
- **Disk** (`df -h /home/newton`): 253G free at the start, 251G after the archive, 233G after the builds in the two slots, and 237G at the end. The seeds slot's `target/` was then cleaned (10.6 GiB). The loader slot's `target/` (16G apparent) is kept for follow-up on the pull request.
- **Slots:** `sub-tfa2-030-loader` (branch `tfa2/inject-fstat-efault`) and `sub-tfa2-030-seeds` (branch `tfa2/seed-refind-build`, no commits of its own). Both are registered with `wrkslots --machine devbig030`.
