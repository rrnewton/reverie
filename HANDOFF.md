# HANDOFF — agent `cortex` (claude coordinator seat), 2026-08-20

Written at the coordinator's instruction before a session restart. Every agent
this session was spawned into the shared parent checkout; this records my state
so a fresh agent in a clean worktree can resume without rediscovering it.

**AT RISK IN ONE LINE:** the panic-exit fix — 125 lines in one reverie file,
built and verified working — is **UNCOMMITTED on a branch with zero commits**,
living under `ignored/`, which is a plausible thing for a cleanup to delete.

---

## 1. WHERE I HAVE WRITTEN — absolute paths, repos, branches

### Repo: reverie (worktree) — **the fix lives here**
- Path: `/home/newton/work/dev-hermit/ignored/panic-task-boundary-reverie`
- Branch: `cc-panic-task-boundary-fatal_20260820`, created by me, based on `origin/main af82f1b9`
- Commits on it: **ZERO.** The work is a working-tree edit only.
- Created via `git -C reverie worktree add`, so it is registered in the reverie
  submodule's worktree list and `git worktree remove`/`prune` would take it.

### Repo: hermit (worktree) — build only, no source changes
- Path: `/home/newton/work/dev-hermit/ignored/panic-fix-hermit`
- Branch: `cc-panic-fix-build_20260820`, created by me, based on `origin/main 269b1e4f72`
- Commits on it: **ZERO.**
- Modified: `Cargo.lock` (a side effect of the cargo `[patch]`, not an intended change)
- Untracked: `.cargo/config.toml` (the patch table — see §2), `target/` (build output,
  contains the verified binary)

### Repo: dev-hermit (parent, SHARED) — two branches, both **pushed**
- `zen5-pmu-evidence_20260820` @ `9b8bf897956e96a45b946599672e69ef42435186`
- `cc-crash-cortex-report_20260819` @ `4a609910e9f3a297d61e1bd6e14131635e63835c`
- Both confirmed present on origin by `git ls-remote` at handoff time. Nothing to lose here.
- I also wrote one untracked file in the shared tree:
  `/home/newton/work/dev-hermit/experiments/orphan-baseline-20260820.txt`
- I made **no commit** in the shared parent checkout and never staged anything there.
  Both parent commits above were built with a temporary `GIT_INDEX_FILE` and an
  explicit path list, so the shared index and working tree were never touched.

### Outside the repos
- `/home/newton/.claude/projects/-home-newton-work-dev-hermit/memory/` — added
  `a-panicking-tracer-task-hangs-because-ivar-cannot-report-a-dead-writer.md`,
  edited `amd-rcb-event-is-correct-on-zen5-the-skid-is-cpu-migration.md` and
  `test-inventory-binds-fixture-to-runner-not-runner-to-ci.md`, and compacted
  `MEMORY.md` from 197 to 83 lines (all pointers preserved, zero broken links).
- `/tmp/armA.err`, `/tmp/armC{1,2,3}.err`, `/tmp/census_{before,after}.txt`,
  `/tmp/panicfix_build.log` — measurement output, **ephemeral, will not survive**.
  Their contents are already quoted verbatim in the tg task notes, so nothing is
  lost if they go.

---

## 2. UNCOMMITTED RIGHT NOW — the part that gets lost on a restart

**ONE FILE MATTERS:**

```
/home/newton/work/dev-hermit/ignored/panic-task-boundary-reverie/reverie-ptrace/src/task.rs
    M, +125 / -1, on branch cc-panic-task-boundary-fatal_20260820 (0 commits)
```

That is the whole fix: a `catch_unwind` at the guest-thread task boundary, the
`HERMIT_TASK_PANIC` marker with the tid, `TASK_PANIC_EXIT_CODE = 101`, and three
unit tests. It **compiles** and it is **verified working** (§4).

**I DID NOT COMMIT IT TO THE PARENT TO SAVE IT**, per instruction. Instead I
copied it outside every repo, where a checkout clean cannot reach it:

```
/home/newton/handoff-cortex-20260820/reverie-task-boundary-panic-fatal.patch   (7,147 B, sha256 75e4c7c482206487…)
/home/newton/handoff-cortex-20260820/task.rs.full-copy                          (233,377 B, whole file)
/home/newton/handoff-cortex-20260820/hermit-cargo-patch-config.toml             (the [patch] table)
/home/newton/handoff-cortex-20260820/orphan-baseline-20260820.txt
```

To restore in a clean reverie worktree based on `af82f1b9`:
`git apply /home/newton/handoff-cortex-20260820/reverie-task-boundary-panic-fatal.patch`

**Also uncommitted, lower value:**
- `/home/newton/work/dev-hermit/ignored/panic-fix-hermit/.cargo/config.toml` — the
  cargo `[patch]` table. Cheap to regenerate but easy to get wrong; copy saved above.
- `/home/newton/work/dev-hermit/ignored/panic-fix-hermit/Cargo.lock` — modified by
  the patch build. **Do not carry this anywhere**; it is a build artifact of the override.
- `/home/newton/work/dev-hermit/experiments/orphan-baseline-20260820.txt` — untracked
  in the shared parent; copy saved above; contents also in the tg notes.

---

## 3. COMMITTED BUT UNPUSHED

**None.** Both parent-repo commits are pushed and verified on origin (§1). The
reverie and hermit worktree branches have zero commits, so there is nothing
committed-and-unpushed anywhere.

---

## 4. WHAT I WAS DOING, AND THE EXACT NEXT STEP

### Finished and verified (tg task `a-panicking-replay-never-exits`, still OPEN)
A panicking hermit run never exited. Root cause, established by reading:
tokio's task harness swallows a panic in a `spawn_local` guest-thread task
(`reverie-ptrace/src/task.rs:4140`); detcore's scheduler then parks forever on an
`Ivar<SchedRequest>` the dead task owed (`detcore/src/scheduler.rs:1042`,
`:1406-1416`); and `detcore/src/ivar.rs` has **no writer-dropped path**, so the
wait is unkillable by construction of the primitive.

Measured proof, one 6-minute box window, injector `REVERIE_SKID_MARGIN_OVERRIDE=0`,
workload `raceprobe 2 2000 500000`:

| arm | binary | rc | wall | SKID_OVERSHOOT | TASK_PANIC |
|---|---|---|---|---|---|
| A | unpatched | 137 | **30.0 s** | 1 | 0 |
| C1 | patched | 1 | **0.2 s** | 1 | 1 |
| C2 | patched | 1 | **0.0 s** | 1 | 1 |
| C3 | patched | 1 | **0.0 s** | 1 | 1 |

`rc=137` in arm A is my own SIGKILL — the hang. Arm C: 3/3 self-exit, backstop
never fired. Unit tests: 3 passed, compiled in 35.67 s. The verified binary is at
`/home/newton/work/dev-hermit/ignored/panic-fix-hermit/target/release/hermit`,
sha256 `bc917361d969dc56…` (goes away if `ignored/` is cleaned; rebuildable in
**2m 13s** at `CARGO_BUILD_JOBS=4`).

**EXACT NEXT STEP FOR THIS ONE:** the branch is unlanded and has no commit. A
fresh agent should, in a proper slot: create a reverie worktree on
`cc-panic-task-boundary-fatal_20260820` (or a new branch off `origin/main`), apply
the saved patch, commit it, and decide with the owner whether to open a PR — the
coordinator held two other reverie/hermit PRs tonight on queue-depth grounds
(137 open, no qualifying receipts), and that hold was a coordinator decision, **not
an owner ruling**.

### In progress, reading only (tg task `a_tracer_panic_is`, CLAIMED BY ME, OPEN)
Why the fix's exit code is invisible. Established, no code changed:
- reverie **preserves** the child status, typed: `RunError::ExitStatus(ExitStatus)`,
  `reverie-process/src/container.rs:947-957`. **Not the culprit.**
- hermit `with_container` folds it into an opaque `anyhow::Error`:
  `hermit-cli/src/bin/hermit/container.rs:423`.
- hermit `main` maps **every** CLI error to `ExitStatus::Exited(1)`:
  `hermit-cli/src/bin/hermit/main.rs:348-355`. One hardcoded line.
- And exit-code-alone discrimination is **not achievable in general**: hermit's exit
  status *is* the guest's by contract (`raise_or_exit`,
  `reverie-process/src/exit_status.rs:86-111`), so every value 0..=255 is a legal
  guest exit. I withdrew my own earlier claim that 101 would deliver it.

**EXACT NEXT STEP:** an owner decision on hermit's exit-code contract, not code.
The recommendation on the task is (c) an out-of-band verdict channel — hermit
already has one, `write_pending_verification_json` at `main.rs:320-322`, built for
exactly "exited without telling you why" — plus (a) fixing "every CLI error is
exit 1", which is a defect independent of this task.

### Filed for others, do not lose
`timer_rs_cites_a` (phantom "force-skid witness" that was never built),
`the_whole_debugger_suite` (four gdb/lldb test files that no validate node runs),
`held_two_gdbstub_fixes` (two written, pushed, un-PR'd gdbserver fixes),
and the coordinator's P0 on the census protocol that cannot tell an orphan exit
from contamination.

---

## 5. PROCESSES I STILL HAVE RUNNING

**None.** Verified at handoff:
`pgrep -a -u $USER -f "panic-fix-hermit|panic-task-boundary|raceprobe|cargo"` → no
matches. My background build completed (exit 0) and every measurement run exited
on its own or was killed by its own backstop within the window.

**Not mine, still alive** — the eight orphans I baselined at 12:32:48Z, unchanged
at 12:34:56Z. Owners `writeup` (4 qemu, ~1% of a core each), `ops` (2 qemu started
with `-S`, never resumed), `ratchet-kvm` (1 hermit, `__futex_wait`, 2.5 days),
`timeouts` (1 hermit, `epoll_wait`, 13.4 h). Full table with pids and start ticks
in `orphan-baseline-20260820.txt` (copy saved outside the repo) and in the tg notes.
I did not kill any of them and did not clean, reset, or check out anything.
