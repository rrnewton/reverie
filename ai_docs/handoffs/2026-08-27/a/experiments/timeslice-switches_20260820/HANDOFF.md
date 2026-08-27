# Safereview handoff — 2026-08-20

## CURRENT ADDENDUM — 2026-08-20 09:28 PDT

This addendum supersedes stale state statements later in this document. It was
written immediately before the requested session restart.

### Repositories, paths written, branches, and commits

- Parent repository `/home/newton/work/dev-hermit` remains on branch
  `cc-triage-reports_20260818` at `02e75abe52f9b852986001f9d2f05263373c868e`.
  I did not edit parent working-tree files directly. The branch is already
  durable as `origin/detcore-partial-determinism_20260819`; it has no commit of
  mine. The shared parent is still dirty with other agents' files and untracked
  experiment/check-out directories. Do not commit, clean, reset, or switch it
  without re-reading the active TaskGraph cleanup notes and the demo owner's
  task.
- Parent cleanup worktree
  `/home/newton/work/dev-hermit/worktrees/cleanup-parent`, branch
  `cleanup/dev-hermit-status-20260820`, is clean. I wrote and committed:
  `ci-hub/directives/{README.md,check.py,tests/test_check.py}` at
  `95dccbdb146c8a380657627a7a7d8492aba635ca`;
  `ci-hub/health/{btrfs_headroom.py,tests/test_btrfs_headroom.py}` at
  `0a164580d5ef926254c0b099f01a88530f46bb35`;
  `ci-hub/health/tick-hub.yaml` at
  `52874ccbf148c106e6418d757b7b8840efab68d5`;
  `compat-envelope/tests/test_collect_reverie_backends.sh` at
  `1f488253a95b0f6cec239d553194b96890aa21d5`; and `.gitignore` at
  `ffa5c0f5dbd3f73238e175d0385a90024d771ead`. All five are published on
  `rrnewton/dev-hermit:main`; nothing in this worktree is uncommitted or
  unpushed.
- Hermit worktree
  `/home/newton/work/dev-hermit/worktrees/safereview/hermit`, branch
  `fix/inherited-git-env-cache-guard`, is clean at
  `85331f49c33f33482fdf814aae09846b13a58d59`, pushed to the same remote branch.
  I wrote `scripts/check-reverie-pin.rs`; PR is
  `https://github.com/rrnewton/hermit/pull/2345`. Exact-head evidence: 32 tests
  passed, `cargo fmt --all -- --check` passed, and `git diff --check` passed.
- Hermit primary `/home/newton/work/dev-hermit/hermit` is repaired and clean on
  `main` at `fda2b0b2874fead40171ab7ffa4aa7cfd638c8ea`, equal to its
  `origin/main`. Before repairing the ref I preserved the two pre-existing
  uncommitted files `detcore/src/logdiff.rs` and
  `hermit-cli/src/bin/hermit/logdiff.rs` unchanged in pushed branch
  `wip/log-diff-follow-20260820`, commit
  `c2805020ae36f239801cc3962200b89f8a4c304f`. That commit is explicitly an
  unreviewed WIP durability snapshot, not a completion claim. The corrupt local
  `main` ref was CAS-moved from Reverie SHA
  `af82f1b92a2762840dc5894984be5e1f5bfc5cb9` to Hermit `origin/main`; the old
  value remains in the reflog.
- Existing pushed dev-hermit experiment branches remain unchanged:
  `ticks-vs-alternation_20260820` at
  `59d7f948794e94c5de7717e9fbeb03feb803a1bb` and
  `slots/mttcg-vcpu-concurrency-20260819` at
  `5560321785b7b0ebbf4521216fd31d7f8b272e00`.
- Existing scratch paths I wrote remain exactly those listed later in this
  document: `/tmp/timeslice_census_watch.py`, `/tmp/run_timeslice_arm.sh`,
  `/tmp/__pycache__/timeslice_census_watch.cpython-312.pyc`, and the named
  detached review worktrees. I also created
  `/tmp/check-reverie-pin-tests-safereview`, a disposable compiled test binary.

### Uncommitted files at risk now

I have no uncommitted product or parent source changes. My uncommitted research
data is still:

- every file under
  `/home/newton/work/dev-hermit/ignored/a/experiments/sched-switch-detector_20260820/`;
- the untracked run outputs under
  `/home/newton/work/dev-hermit/ignored/a/experiments/ticks-vs-alternation_20260820/`;
- every file under
  `/home/newton/work/dev-hermit/ignored/a/experiments/timeslice-switches_20260820/`,
  including this handoff;
- `/tmp/timeslice_census_watch.py`, `/tmp/run_timeslice_arm.sh`, the Python
  bytecode cache above, `/tmp/check-reverie-pin-tests-safereview`, and
  `/var/tmp/pdeath-probe.3505340/tgt/` (generated build output).

The shared parent still contains extensive uncommitted work owned by others.
The highest-risk live item is the disk-input demo change in
`demos/{05-qemu-boot.py,06-qemu-resume.py,lib/qemu-assets.sh,lib/qemu_controller.py}`;
TaskGraph task `land-the-disk-input-demos-change-and-rebuild-the-snapshot` is
still in progress under agent `directives`. Do not duplicate or discard it.

### Committed but unpushed

None of mine. Every commit named above has been pushed, and every parent commit
named above is reachable from `rrnewton/dev-hermit:main`.

### Work in progress and exact next step

Task `get-dev-hermit-back-to-a-clean-git-status` remains open. A fresh agent in
a clean worktree should first fetch and re-read `origin/main` because it moved
repeatedly during cleanup, then:

1. wait for or coordinate with the active demo owner; do not touch its four
   demo files;
2. inspect the latest cleanup-task notes for the per-item inventory;
3. preserve or deliberately remove the remaining unique parent reports and
   experiment sources, while leaving multi-gigabyte generated outputs ignored;
4. restore `.skill_reset_20260816/{AGENTS.md,CLAUDE.md}` only after respecting
   the open owner-only `skills-dir-missing` decision;
5. switch the shared parent from the stale divergent branch to current `main`
   only after all genuine working bytes are durable; and
6. verify the literal parent `git status --short --branch` is clean and record
   every disposition before closing the task.

The `.gitignore` commit at `ffa5c0f5` already prevents accidental staging of
the measured multi-gigabyte first-divergence/KVM run directories and the three
compiled vCPU-steering guests while leaving source and compact evidence visible.

### Running processes

No QEMU, Hermit guest, detector, compiler, test, or experiment process owned by
this agent remains running. Only the Codex session harness itself is active.

Do not clean, reset, checkout, or commit the shared parent checkout as part of
recovering this work. The paths below were inspected in place and must be
salvaged into newly assigned worktree slots deliberately.

## 1. Where I worked and wrote files

### dev-hermit repository

- `/home/newton/work/dev-hermit/ignored/a`
  - Detached worktree at `59d7f948794e94c5de7717e9fbeb03feb803a1bb`.
  - The commit is also named by local/remote branch
    `ticks-vs-alternation_20260820` /
    `origin/ticks-vs-alternation_20260820`.
  - I wrote the tracked harness and evidence under
    `/home/newton/work/dev-hermit/ignored/a/experiments/ticks-vs-alternation_20260820/`.
    Commit `59d7f948` contains `README.md`, `confirm.sh`, `sweep.sh`,
    `tickprobe.py`, and the committed `r-ch20`, `r-ts2`, `r-ts20`, `r-ts200`,
    `r-ts200b`, and `r-ts2b` evidence files.
  - I also wrote untracked run outputs in that directory. Their tags are:
    `a1`, `a2`, `b1`, `b2`, `c1`, `c2`, `d10`, `d100`, `d100b`, `f20a`,
    `f40a`, `f40b`, `f50a`, `final10a`, `final50a`, `m10`, `m100`, `m100b`,
    `mts200a`, `mts20a`, `mts50a`, `mts50r1`, `mts50r2`, `s1000000`,
    `s10000000`, `s100000000`, `s1b`, `ts50a`, `ts50b`, `ts50c`, `y100`,
    `y1000`, and `y10000`. Each tag may have `.capture`, `.err`, and `.int`
    files. The whole directory is about 31 MiB.
  - I wrote the entire untracked detector tree
    `/home/newton/work/dev-hermit/ignored/a/experiments/sched-switch-detector_20260820/`
    (about 92 MiB). Important files include `RESULTS.md`,
    `build-initramfs.sh`, `guest/init`, `guest/sched_switch_detector.c`,
    `guest/sched_switch_detector`, `initramfs-cc.cpio.gz`,
    `snap-detector.qcow2`, and every file under `results/`.
    Detector SHA-256:
    `e0d7c8fa4dc6fa63de69e5c8302f8d18c52d652f858ce4c02f5fead955413cc1`.
    Initramfs SHA-256:
    `cc583ed0f48368bc29527e7447566da629dadad76c6821140d0dbdfe4ea6ac34`.
  - I wrote the entire untracked measurement tree
    `/home/newton/work/dev-hermit/ignored/a/experiments/timeslice-switches_20260820/`
    (about 228 KiB before this handoff), including every JSON and `.stop` file
    under `results/` and this `HANDOFF.md`.

- `/home/newton/work/dev-hermit/worktrees/slots-mttcg-probe`
  - dev-hermit branch `slots/mttcg-vcpu-concurrency-20260819` at
    `5560321785b7b0ebbf4521216fd31d7f8b272e00`.
  - I wrote and committed
    `/home/newton/work/dev-hermit/worktrees/slots-mttcg-probe/experiments/mttcg-vcpu-concurrency_20260819/admit.py`.
  - Worktree is clean.

### Hermit repository

- `/home/newton/work/dev-hermit/worktrees/safereview/hermit`
  - Hermit branch `fix/inherited-git-env-cache-guard`, based on
    `origin/main` / `269b1e4f72e93d4c8e32f00891a5a1ae15d42bff`.
  - I edited only
    `/home/newton/work/dev-hermit/worktrees/safereview/hermit/scripts/check-reverie-pin.rs`.
  - The change is deliberately uncommitted. Diff SHA-256:
    `75f692f684e05696ce2e09489d7a5ded1150dfc96550470674147d1beb4c958f`.

### Scratch/review paths

- `/tmp/timeslice_census_watch.py`
  - SHA-256 `8526d28faf6d70da6eedbbf615662b3059ba2fbb923ecdaedd8eefcdf1fd0d5b`.
- `/tmp/run_timeslice_arm.sh`
  - SHA-256 `44832ed16a63ac9f33b17c9ed4289ee44257119842ae1be2cc349a552e89c841`.
- `/tmp/__pycache__/timeslice_census_watch.cpython-312.pyc`.
- `/tmp/cap-log-review-17be` — clean detached review worktree at
  `17be54121674c14807d41c290b9804fab094000f`.
- `/tmp/cap-log-review-57df` — clean detached review worktree at
  `57df8c5f0c5457e25cff10a7ad93c9cd7f67d78a`.
- `/tmp/emitter-review-ab66` — clean detached review worktree at
  `ab66ed2a1db5d417694b653fd3f0ed3ad50f362d`.
- `/var/tmp/emitter-rebase.1434731` — clean detached review worktree at
  `8f21fd4cfeacef5c48f37376407223859af659fd`.
- `/var/tmp/pdeath-probe.3505340` — detached review worktree at
  `d1798c8e11064381e035fc7e404fa236f65512e8`; it has the untracked build
  directory `/var/tmp/pdeath-probe.3505340/tgt/` (about 893 MiB).
- Historical, no longer present after relocation:
  `/tmp/alternation-vs-clock.bPTVYX` and
  `/home/newton/work/dev-hermit/ignored/alternation-vs-clock-safereview-20260820`.
  Their durable replacement is `/home/newton/work/dev-hermit/ignored/a`.
- Task state and notes were written through `tg` to
  `/home/newton/.tg/hermit2.db`; do not treat that database as source code.

I did not intentionally edit the shared parent checkout at
`/home/newton/work/dev-hermit` itself. It contains extensive unrelated shared
changes belonging to other agents.

## 2. What is uncommitted now

The source change most at risk is exactly:

- `/home/newton/work/dev-hermit/worktrees/safereview/hermit/scripts/check-reverie-pin.rs`

It adds both protections against inherited Git environment redirecting a
cache fetch: clearing the complete Git repository-selection environment on
every cache command, and refusing when the canonical cache git-dir equals the
product common git-dir. The incident-shaped `/tmp` regression proved the raw
command moved a victim `main`, the isolated command updated only the cache, and
the identity guard refused a symlinked cache before any ref update. Final local
evidence was 32 checker self-tests passed twice (3.07 s and 2.78 s),
`cargo fmt --all -- --check`, and `git diff --check`. No test, tolerance,
comparator, label, or skip was weakened.

Other uncommitted data is experimental rather than product source:

- Every file under
  `/home/newton/work/dev-hermit/ignored/a/experiments/sched-switch-detector_20260820/`.
- The untracked run outputs listed above under
  `/home/newton/work/dev-hermit/ignored/a/experiments/ticks-vs-alternation_20260820/`.
- Every file under
  `/home/newton/work/dev-hermit/ignored/a/experiments/timeslice-switches_20260820/`.
- `/tmp/timeslice_census_watch.py`, `/tmp/run_timeslice_arm.sh`, and the Python
  bytecode cache listed above.
- `/var/tmp/pdeath-probe.3505340/tgt/` is untracked build output, not source.

The detached `/home/newton/work/dev-hermit/ignored/a` worktree has no modified
tracked files; its status consists of the untracked experiment outputs above.
The four other detached review worktrees listed above are clean.

## 3. Committed but unpushed

None found.

Committed and already pushed, for provenance:

- dev-hermit branch `ticks-vs-alternation_20260820`, commit
  `59d7f948794e94c5de7717e9fbeb03feb803a1bb`; remote branch points to the same
  commit.
- dev-hermit branch `slots/mttcg-vcpu-concurrency-20260819`, commit
  `5560321785b7b0ebbf4521216fd31d7f8b272e00`; remote branch
  `origin/slots/mttcg-vcpu-concurrency-20260819` points to the same commit.

The Hermit branch `fix/inherited-git-env-cache-guard` has no new commit; its
HEAD is still base commit `269b1e4f72e93d4c8e32f00891a5a1ae15d42bff`.

## 4. What I was doing and the exact next step

### Immediate salvage priority

First preserve the uncommitted
`/home/newton/work/dev-hermit/worktrees/safereview/hermit/scripts/check-reverie-pin.rs`
diff into the newly assigned Hermit worktree. Do not commit it from the shared
parent. Re-run the 32 checker self-tests, formatting, and `git diff --check`
from the clean slot, then review and commit/push only under the new task's
authorization.

### Guest-preemption measurement

My original question was whether reducing Hermit's maximum timeslice increases
the guest detector's involuntary switches. Exact scope:

- Hermit binary SHA-256
  `52204423138bb2633de007afd826467fc1adb3055a46f5fbe86e28fce85faa81`
  (`hermit 0.2.0`, `g770b95c505fa`).
- QEMU 10.1.2, 2-vCPU MTTCG `thread=multi`, no icount, `det-ready` snapshot
  resume.
- `--no-virtualize-time --no-virtualize-metadata`, default sequentialization.
- Original detector SHA-256
  `e0d7c8fa4dc6fa63de69e5c8302f8d18c52d652f858ce4c02f5fead955413cc1`,
  workload `sched_switch_detector spin 100000000`.
- Admission uses commit `55603217` plus continuous 0.2-second process-census
  sampling from `/tmp/timeslice_census_watch.py`.

Only one arm was admissible: `f50a`, maximum timeslice 50 ms, wall 53 s,
`total=84 voluntary=1 involuntary=83`, clean guest output, unchanged endpoint
identities, and zero new processes in the continuous census. Do not extrapolate
a curve from it.

If the owner wants this exact switch question resumed, obtain an exclusive
approximately seven-minute QEMU/Hermit window and run the remaining balanced
order: **40 ms, 30 ms, 30 ms, 40 ms, 50 ms**. Reject and repeat an arm on any
process identity change, continuous-census hit, detector failure, or guest
CSD/RCU/soft-lockup warning.

The TaskGraph task was later claimed and closed by `directives`, but its final
note explicitly says the switch question remains under-powered: it used a
different detector and a 1M workload, while the preserved detector's own 10M
calibration was below resolution. Its useful result is about Hermit scheduler
turns, not the original 100M involuntary-switch curve. Do not treat task status
`closed` as proof that this switch question was answered.

Twenty milliseconds must not be silently retried as a normal point. In the
preserved detector configuration, one 20 ms arm printed 111 involuntary
switches and then a CPU0 soft lockup before `FIN23`; another emitted zero serial
and Hermit reported all guest threads terminating with `SIGSTKFLT`. Existing
captures also establish that there is no universal timeslice-only boundary:
20 ms was clean twice with the `order_same` snapshot, while `-smp 1 + icount`
produced zero serial at both 2 ms and 20 ms. The safe conclusion is that the
failure depends on timeslice interacting with configuration/workload; a single
shared kernel cause is unproved.

### Already completed research

- The separating experiment and its exact configuration/results are preserved
  by pushed commit `59d7f948` and the untracked raw captures.
- The guest detector was calibrated against known yields and native fixed work;
  details are in
  `/home/newton/work/dev-hermit/ignored/a/experiments/sched-switch-detector_20260820/RESULTS.md`.
- The stable process-census gate is preserved by pushed commit `55603217`.

## 5. Processes still running

No QEMU, Hermit guest, detector, tickprobe, census sampler, or experiment shell
owned by `DG_AGENT_NAME=safereview` remains running.

The only processes observed with that agent identity immediately before this
handoff were the session harness itself:

- PID `3375334` — outer `codex` process.
- PID `3377820` — `codex.real` process.
- PID `3396811` — `codex-code-mode-host`.

These are not experiment workloads and are expected to disappear when this
session is restarted.
