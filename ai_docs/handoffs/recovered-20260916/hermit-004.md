# Pressure campaign handoff

## Wrkslots multiline ExecStart parser handoff — 2026-09-11

Task `make_wrkslots_recovery_accept` remains IN_PROGRESS and owned by `hermit-004`. The owner redirected this lane to time-sensitive preservation work before the parser fix could be committed or landed.

- Clean detached agent-utils worktree: `/tmp/agent-utils-wrkslots-multiline`
- Base: `rrnewton/agent-utils:main` at `6fc882cba8af1c8f32b5273ca4f72e670e16dbba`
- Uncommitted paths: `py/wrkslots/cli.py`, `py/wrkslots/tests/test_lifecycle.py`
- Diff: parser accepts physical continuation lines only while parsing an `Exec*` property; regression preserves the complete multiline `ExecStart`; a second test proves both absent-agent and absent-validation recovery dry-run plans complete without changing registry records.
- Actual read-only systemd evidence: both stopped `run8-cgroup-podman-probe-2422360.service` and `run8-cgroup-podman-probe-2611654.service` have 15 continuation lines after `ExecStart=`. The first continuation is `mkdir -p /src/target/cgroup-probe`; later lines include equals signs.
- Before change: focused regression exited 1 with `cannot parse user-systemd properties: 'mkdir -p /src/target/cgroup-probe'`. Preserved log: `ignored/wrkslots-multiline-regression-before.log`.
- After change: two focused tests passed in 2.87s. The corrected parser also read the live snapshot successfully: 172 units, both affected services found, multiline values and path evidence preserved.
- `make validate` was interrupted at the owner's redirect. It had reached 98% of the Python suite and had displayed failures/errors, so it must be rerun and any reproducible failure investigated before commit. Preserved log: `ignored/wrkslots-multiline-make-validate-interrupted.log`.
- Resumption evidence: the restricted full run completed with socket `EPERM` failures (`ignored/wrkslots-multiline-validate-restricted.log`). The host-permission run passed 2,708 tests, then its PID-namespace lifecycle split failed 41 tests because `/usr/bin/git` appears untrusted inside that namespace (`ignored/wrkslots-multiline-validate-userns.log`). The same representative failure reproduces on untouched base `6fc882cba8af1c8f32b5273ca4f72e670e16dbba` (`ignored/wrkslots-main-baseline-unshare.log`).
- The Makefile's documented ordinary-environment fallback was run with only the `unshare` availability probe disabled. It reached 94% with one failure marker and no second failure, but the owner redirected this lane before the very slow real-`lsof` suite reached its traceback. It was stopped safely; no process from that run remains. Preserved log: `ignored/wrkslots-multiline-validate-fallback-interrupted.log`.
- No registry mutation, recovery, commit, push, or landing was performed.

Next action: resume in `/tmp/agent-utils-wrkslots-multiline`, rerun the Makefile's documented ordinary-environment fallback without a concurrent validation process, capture the late failure traceback or a green final status, then commit and land directly to agent-utils main only if the result permits. Verify the pushed main commit by a fresh origin fetch. Record that the landed fix is not active until the consuming repository advances its pinned agent-utils commit.

Paused by owner on 2026-09-10. Do not continue pressure execution or concurrency work until TaskGraph task `review-zero-pr-linear-state-and-green-main` is closed.

All relative paths below are rooted at `/home/newton/work/dev-hermit/worktrees/slots/hermit-004`.

## Task state

- `verify-pressure-dag-200x10-counts-and-timeouts` was reopened and is IN_PROGRESS, owned by `hermit-004`.
- `execute-pressure-sample-200x10` was returned to OPEN and must not be reclaimed until verification closes.
- No pressure runner, calibration runner, cgroup monitor, or guest process remains active.

## Authoritative population

- Use all 149 identities in `ignored/compat-envelope/pressure-sample-200-never-measured-verify/eligible-population.jsonl`.
- SHA-256: `136d55373312bcd1dc0c5c7587357c13743cbb498e76fd736c9a2761cc1ab7eb`.
- The elevated official KVM probe and an elevated read/write open both passed. The earlier missing-`/dev/kvm` observation came from the restricted inspection sandbox and is not execution evidence.
- Do not use `ignored/compat-envelope/pressure-sample-200-never-measured-verify/host-executable-population.jsonl`. It and `host-excluded-kvm-population.jsonl` are preserved rejected planning artifacts only.

## Preserved source state

- Hermit branch: `hermit-004/pressure-cells-file`; worktree is clean.
- Current HEAD: `68fabc2f07093d9390e403a6d5f8a63c428f64a3` (`ci: bound and measure pressure guest concurrency`).
- Parent commit: `8dbfb1d5a693ac2da786e3bb354ec9c56bd22d09` (`ci: select exact pressure cells from a file`).
- The former uncommitted concurrency diff is preserved exactly in commit `68fabc2f0`; its reviewed pre-commit blob was `492bc4ec13cc61b1c209cb119e97f14cd16c13a1`.
- Independent adversarial review approved that exact blob after one earlier lifecycle finding was fixed. Non-guest self-test, compile/help, `cargo fmt --all -- --check`, and `git diff --check` passed.
- These commits are local only. A GitHub push was denied by environment external-code export policy.

## Preserved stopped runs

Initial full-campaign stop (zero guest attempts):

- Results: `ignored/compat-envelope/pressure-sample-200-never-measured-verify/campaign-149x10`
- Generated fresh checkout: `hermit/ignored/pressure-fresh-anlPMvh6` (about 21 GiB), pinned to `8dbfb1d5a693ac2da786e3bb354ec9c56bd22d09`.
- Preserved markers: `setup-manifest_plan.ok`, `build-runtime_release.ok`, `build-workspace.ok`.
- No `cells/` or `prepare/` directory exists, so this run produced zero attempts, guest executions, and comparisons.
- DAG SHA-256: `26353822f492a4d4766cb27d18990a022abf00315700e3ee00547f4cae80c8dd`.
- run.json SHA-256: `d96f28a3d309c9e36c5471d6a8e1f0ff3e48a8c871e95aa915b5599304103fd6`.

Interrupted W4/K1 calibration (zero guest attempts):

- Results: `ignored/compat-envelope/pressure-sample-200-never-measured-verify/calibration-w004-k01-r01`
- Generated fresh checkout: `hermit/ignored/pressure-fresh-WRjzCXoy` (about 11 GiB), pinned to `68fabc2f07093d9390e403a6d5f8a63c428f64a3`.
- Only `setup_manifest_plan` completed. Journal contains containment, three build/setup starts, and the setup end; no cell start exists.
- DAG SHA-256: `2ded4afd060d141b7b3d9895161e1c04913f616fbb2ebf7fa1a75d490a946ddb`.
- run.json SHA-256: `2f7b51bb25bcc43b61b0862b511c9fc65a349f6c76db818fcf4ecdde4cd60975`.
- Outer-cgroup samples SHA-256: `6f2933c0dea422ba03debb22ee7d418f27c9fe35ff8bc48ecf556a8c4098893f`.

## Corrected all-149 plan and controls

- Candidate DAG: `ignored/compat-envelope/pressure-sample-200-never-measured-verify/dag-149x10-w128-k8/dag.json`, SHA-256 `2f9b09eea0e52916d0b1dfa8270884c86dc7e559fcd7218702d5e6f896b40804`.
- Candidate run metadata: same directory `run.json`, SHA-256 `f95e15aac9f34515669c4397058dc9e93e207225e761e8`.
- Fixed outer memory limit: 549755813888 bytes (512 GiB).
- Candidate caps: requested jobs 316, manifest guests 128, KVM guests 8.
- Declared peak: 534723428352 bytes (498 GiB), including late LiteInst build, one preparation, selected cell cgroups, and 1 GiB control-plane headroom. This leaves 14 GiB inside the fixed outer scope.
- Exact all-149 shape remains 1,490 cell wrappers, 135 preparations, five setup/build nodes, one summary, 1,631 total nodes, at most 2,980 guest executions, and 1,490 comparisons.
- Positive boundaries: W132/K8 passes at 510 GiB (`control-pass-w132-k8`); W128/K9 passes at 511 GiB (`control-pass-w128-k9`).
- Negative boundaries: W133/K8 refuses at 513 GiB (`control-reject-w133-k8`); W128/K10 refuses at 524 GiB (`control-reject-w128-k10`). The reject directories are intentionally empty.

## Calibration plan after the dependency closes

- Calibration file: `ignored/compat-envelope/pressure-sample-200-never-measured-verify/concurrency-calibration-population.jsonl`, SHA-256 `4974444eeba367b8b574568fc5cf5b9f4406de2837e514602872ddde136af9b5`.
- It contains 16 frozen identities across all five backends: DBT 3, KVM 4, LiteInst 3, ptrace 3, SaBRe 3.
- Pin every rung with `DAGRUN_OUTER_MEMORY_MAX_BYTES=549755813888`, `--jobs 316`, `--cell-timeout 60`, and no `DEV_HERMIT_PARENT` so calibration rows do not enter the series store.
- Rungs: W4/K1 with 1 repetition; W32/K2 with 3; W64/K4 with 5; W128/K8 with 10. The repetition counts provide enough queued KVM and portable nodes for each requested cap.
- Accept a rung only if the append-only `runner-profile/journal.jsonl` proves the requested cell concurrency was actually reached, all planned cell tags are terminal and unique, profile rows show zero OOM/reclaim-at-cap, there is no outer OOM event, and no new timeout/infrastructure class appears relative to the prior accepted rung. If a rung does not reach its requested concurrency, increase only its calibration repetitions and rerun in a fresh result root.
- The fixed-bound envelopes are W4/K1=133 GiB, W32/K2=133 GiB, W64/K4=254 GiB, W128/K8=498 GiB. The aggregate CPU bound at W128 is 130 cores, below the observed 284-core outer quota.
- After W128/K8 passes and is measured, regenerate and audit the all-149 x 10 DAG at the same fixed 512 GiB outer bound. Close `verify-pressure-dag-200x10-counts-and-timeouts` with the calibration and corrected DAG evidence. Only then reclaim `execute-pressure-sample-200x10` and relaunch the full campaign.
