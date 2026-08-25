# LiteInst zero-ptracer `strace -f` litmus

Date: 2026-08-07  
Task: `verify-and-land-liteinst-zero-ptracer-prerequisites`  
Slot: `worktrees/sol-box`

## Result

The owner acceptance gate fails at the tested source: **0/2 guests executed,
0/2 commands succeeded, and 2/2 tracees attempted
`ptrace(PTRACE_TRACEME)` and received `EPERM`** while Hermit itself was under
`strace -f`. This is direct evidence that the tested LiteInst path still needs
the ptrace tracer slot. It is not architecturally zero-ptracer and is not ready
for performance work under the owner's rule.

This is focused direct-litmus evidence only. It is not a product change, no PR
was landed, and no validation authority is claimed.

## Exact source and build provenance

- Hermit checkout: `303113c10522360de9051e21ed2d777c3436e17b`
- Reverie locked dependency and slot checkout:
  `038e993926e45514264d30367b70df9b6ac3b9b8`
- LiteInst2 locked dependency actually consumed by both Hermit lockfiles:
  `95ee5e6917fa33191eb41c3f1606ea8b03c1b78c`
- LiteInst2 detached slot checkout:
  `8bf704feb06a62e7a05bee3b237d70793e4e2689` (not consumed by this Cargo
  build; it must not be cited as binary provenance)
- Host: Linux x86-64
- LiteInst feature selection: LiteInst is part of Hermit's default/core build;
  `hermit-cli` has no separate `liteinst` Cargo feature.

The runtime and CLI were built sequentially with one Cargo job:

```bash
cd /home/newton/work/dev-hermit/worktrees/sol-box/hermit
CARGO_BUILD_JOBS=1 ./scripts/stage-liteinst-runtime.sh release \
  "$PWD/target/release/libreverie_liteinst.so" \
  "$PWD/target/liteinst-runtime-build"
CARGO_BUILD_JOBS=1 cargo build --release --locked -p hermit --bin hermit
```

The runtime staging command exited 0 after 3m42s. The locked CLI build exited
0. A predecessor process was already finishing the same one-job release build;
the locked command waited on Cargo's directory lock and then verified the
result rather than compiling concurrently.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `target/release/hermit` | 58,542,240 | `ea54d5fab65a0c948ece60ac6adde29d03507f9281d1147aaefdbad794fd183c` |
| `target/release/libreverie_liteinst.so` | 35,045,272 | `86ef13b22c1c8686f9e7d5b5522115c04e3f348c9cbff2931ccfc25c72a21b8c` |

The CLI identified itself as:

```text
hermit 0.2.0 (2026-08-07, g303113c10522)
```

## Exact litmus commands

The complete traces below use a transparent process/ptrace filter so the full
evidence fits in this artifact. The filter includes Hermit's `execve`, tracee
creation and reaping, every `ptrace` call, and any guest `execve`.

```bash
/usr/bin/timeout --signal=TERM --kill-after=5s 45s \
  /usr/bin/strace -f -s 4096 -e trace=%process,ptrace \
  -o /home/newton/work/dev-hermit/worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/true.focused.strace \
  /home/newton/work/dev-hermit/worktrees/sol-box/hermit/target/release/hermit \
  --log info run --backend liteinst --strict --verify -- /bin/true \
  > /home/newton/work/dev-hermit/worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/true.focused.stdout \
  2> /home/newton/work/dev-hermit/worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/true.focused.stderr

/usr/bin/timeout --signal=TERM --kill-after=5s 45s \
  /usr/bin/strace -f -s 4096 -e trace=%process,ptrace \
  -o /home/newton/work/dev-hermit/worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/echo.focused.strace \
  /home/newton/work/dev-hermit/worktrees/sol-box/hermit/target/release/hermit \
  --log info run --backend liteinst --strict --verify -- \
  /bin/echo liteinst-zero-ptracer \
  > /home/newton/work/dev-hermit/worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/echo.focused.stdout \
  2> /home/newton/work/dev-hermit/worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/echo.focused.stderr
```

Both commands completed before the timeout and returned 1. Both stdout files
are exactly 0 bytes. Their complete stderr is:

```text
Error: failed to open pidfd for LiteInst tracee 3282825: -110 ETIMEDOUT (Connection timed out)
```

and:

```text
Error: failed to open pidfd for LiteInst tracee 3328602: -110 ETIMEDOUT (Connection timed out)
```

## Complete focused raw trace: `/bin/true`

This is the complete 10-line contents of `true.focused.strace`, SHA-256
`acb45b267a4cc3b6a11c0c00cf8bf64cfd31b7d3ba6dcaf26b5e7469982d75bb`:

```text
3282637 execve("/home/newton/work/dev-hermit/worktrees/sol-box/hermit/target/release/hermit", ["/home/newton/work/dev-hermit/worktrees/sol-box/hermit/target/release/hermit", "--log", "info", "run", "--backend", "liteinst", "--strict", "--verify", "--", "/bin/true"], 0x7ffff3d3ea50 /* 153 vars */) = 0
3282637 clone(child_stack=0x7fff16e6fea0, flags=SIGCHLD) = 3282825
3282825 ptrace(PTRACE_TRACEME)          = -1 EPERM (Operation not permitted)
3282825 exit_group(1)                   = ?
3282825 +++ exited with 1 +++
3282637 --- SIGCHLD {si_signo=SIGCHLD, si_code=CLD_EXITED, si_pid=3282825, si_uid=212630, si_status=1, si_utime=0, si_stime=0} ---
3282637 kill(3282825, SIGKILL)          = 0
3282637 wait4(3282825, [{WIFEXITED(s) && WEXITSTATUS(s) == 1}], WNOHANG|__WALL, NULL) = 3282825
3282637 exit_group(1)                   = ?
3282637 +++ exited with 1 +++
```

The trace contains one `ptrace` call and one `PTRACE_TRACEME`/`EPERM` result.
It contains zero `execve("/bin/true"...)` calls: the guest did not execute.
PIDs 3282637 and 3282825 were checked after completion and were absent.

## Complete focused raw trace: `/bin/echo`

This is the complete 10-line contents of `echo.focused.strace`, SHA-256
`bf2d43dbde2a367b1301bb3436b6fdf1c5927ab23710c514e45b062f9f52147b`:

```text
3328473 execve("/home/newton/work/dev-hermit/worktrees/sol-box/hermit/target/release/hermit", ["/home/newton/work/dev-hermit/worktrees/sol-box/hermit/target/release/hermit", "--log", "info", "run", "--backend", "liteinst", "--strict", "--verify", "--", "/bin/echo", "liteinst-zero-ptracer"], 0x7ffdc9c3b268 /* 153 vars */) = 0
3328473 clone(child_stack=0x7ffdbb1b4940, flags=SIGCHLD) = 3328602
3328602 ptrace(PTRACE_TRACEME)          = -1 EPERM (Operation not permitted)
3328602 exit_group(1)                   = ?
3328602 +++ exited with 1 +++
3328473 --- SIGCHLD {si_signo=SIGCHLD, si_code=CLD_EXITED, si_pid=3328602, si_uid=212630, si_status=1, si_utime=0, si_stime=0} ---
3328473 kill(3328602, SIGKILL)          = 0
3328473 wait4(3328602, [{WIFEXITED(s) && WEXITSTATUS(s) == 1}], WNOHANG|__WALL, NULL) = 3328602
3328473 exit_group(1)                   = ?
3328473 +++ exited with 1 +++
```

The trace contains one `ptrace` call and one `PTRACE_TRACEME`/`EPERM` result.
It contains zero `execve("/bin/echo"...)` calls: the guest did not execute and
the expected output was absent. PIDs 3328473 and 3328602 were checked after
completion and were absent.

## Full unfiltered local traces

The same two litmus commands were also run with unfiltered
`strace -f -s 4096 -yy`. The 16 MiB logs are ignored local evidence and are not
committed:

| Guest | Local trace path | Lines | Bytes | SHA-256 |
| --- | --- | ---: | ---: | --- |
| `/bin/true` | `worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/true.host.strace` | 124,781 | 16,429,525 | `cfb89aa8148cf43263329398a075b67c9080288f7d49ca08628ea8488484159e` |
| `/bin/echo` | `worktrees/sol-box/evidence/zero-ptracer-20260807T1920PT/echo.host.strace` | 124,782 | 16,429,577 | `d7cfcc15e04047c1300dc92dce4e44a86173738b6e07cf5f6606d0f71e3a8556` |

The unfiltered runs also returned 1, produced zero stdout bytes, recorded the
same pidfd timeout on stderr, contained exactly one
`ptrace(PTRACE_TRACEME) = -1 EPERM` per trace, and never executed either guest.

## Owner gate and qualification

- `rrnewton/reverie#392` remains open with the owner architecture decision
  unresolved.
- `rrnewton/hermit#1783` still treats #392 as a hard precondition rather than
  parallel work.
- The advisory landing plan remained `land_now=[]`; PR #405 was draft with a
  genuine named-job red, PR #389 remained review-blocked, PR #391 carried only
  L0 host-dispatch evidence, and no Hermit consumer existed.
- No PR was mutated or landed by this task.
- ci-hub evidence is on probation for this task: no ci-hub result, label,
  receipt, or copied status was used as authority. The direct litmus is the
  only measured result here.

Repository/base qualification:

- This PR is in `rrnewton/dev-hermit`. Its branch base is parent `origin/main` at
  `6cda835ebd0d6145c1c720291c835a823ab926ef`. The dev-hermit parent main has
  no verified-green boundary and is **UNVERIFIED**.
- The runtime evidence tested Hermit
  `303113c10522360de9051e21ed2d777c3436e17b`, and that Hermit main is
  unverified.
- `d53550510d1e7d13e84cc8af9bb90269e90b3f07` is the Hermit repository's
  verified-green boundary. This task did not build or test it, and it is not a
  valid dev-hermit parent branch base.

No full validate, focused repository test suite, or performance benchmark was
run. The observed failures establish the zero-ptracer architecture defect; they
do not establish any Hermit L1/L2 determinism assurance for LiteInst.

