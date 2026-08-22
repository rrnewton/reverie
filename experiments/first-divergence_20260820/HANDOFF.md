# HANDOFF — find-the-first-divergence-in-hermit-boot

Agent: claude (diverge). Session 2026-08-20, 06:16–08:00. Task
`find-the-first-divergence-in-hermit-boot`, still `in_progress`.

All arms COMPLETED before this handoff was written. Nothing was abandoned
mid-flight. **No process of mine is still running** (see §5).

---

## 0. THE RESULT, per stream — this is what the two blocked lanes need

**Ten kernel boots, byte-identical pathname, `hermit log-diff` between them.**
Arm `fm` (info log via stderr redirect) and arm `b5` (info log via `--log-file`,
stdout/stderr on their own fds) are two independent replicates and agree.

| stream | result | how measured |
| --- | --- | --- |
| **info log** | **10 distinct of 10** | sha256 of the log with only the wall-clock timestamp prefix removed; **and** `hermit log-diff`, all 9 pairs vs r01, every one `differences found` |
| **snapshot** | **1 of 10 identical** | sha256 of the qcow2 file (`fm` c7ac958fc706211c, `b5` 6277d78a8801) |
| **state** | **1 of 10 identical** | `scalars.py`: vm_clock 7398393368, icount 7144684866, bias 253708502, cpu_clock_off 274351456915, cpu_ticks_off 185893385, guest RAM c065fa9b95c03e34, devices 3fc48709eda1370c |
| **stdout** | **1 of 10 identical** | 0 bytes in all ten (`b5`, captured on its own fd) |
| **stderr** | **1 of 10 identical** | 0 bytes in all ten (`b5`) |

Serial console output (not one of the five, but measured): **1 of 10 identical**,
9df4778cb8149501.

**The info log is the only stream of the five that diverges.** Every
determinism claim about this boot to date rested on the others, and they are
clean.

### FIRST DIVERGING DETLOG MESSAGE

```
hermit log-diff keep/fm/r01.log keep/fm/r02.log --no-color --syscall-history 3

Logs contain 2869805 | 2869805 messages total
(DETLOG) Mismatch at log messages 2862447 (run 1) and 2862447 (run 2):
<"DETLOG [memory][dtid 3] 0x7ffffffdb000-0x7ffffffff000 ... [stack]->5b4e5e07d908348d..."
>"DETLOG [memory][dtid 3] 0x7ffffffdb000-0x7ffffffff000 ... [stack]->7b2720c1b4c638ce..."
Divergent syscall context:
  run 1, log message 2862445: finish syscall #907: recvmsg(11, 0x7fffffff8370, 0) = Ok(1468)
  run 2, log message 2862445: finish syscall #907: recvmsg(11, 0x7fffffff8370, 0) = Ok(1468)
```

**Message 2,862,447 of 2,869,805 — 99.74% of the way through the run.** With
`--limit 0`: **132 mismatches, all between message 2,862,447 and 2,863,082** —
one 635-message window at the very end, which is simply every remaining stack
sample of dtid 3 after the buffer was dirtied.

Layered, same pair:
- `--ignore-lines 'DETLOG [memory]' --ignore-lines 'DETLOG [registers]'` →
  `no substantive differences found (1,433,139 DETLOG messages compared)`
- `--ignore-lines 'DETLOG [memory]'` alone → `no substantive differences found`

So **zero** syscall divergences, **zero** COMMIT/scheduling divergences,
**zero** register divergences. One cause, 132 downstream symptoms.

### THE CAUSE

`getifaddrs()` over **AF_NETLINK**. dtid 3 (the `/usr/bin/python3` launcher,
which is a symlink to python3.9) does, at syscalls 901–907:

```
socket(16, 524291, 0) = Ok(11)      AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_ROUTE
bind(11, ..., 12)                   sockaddr_nl
sendto(11, ..., 20, 0, ..., 12)     nlmsghdr + rtgenmsg = RTM_GETLINK, NLM_F_DUMP
recvmsg(11, ..., 0) = Ok(1468)      the interface dump
```

The return value is 1468 in **every** run, so the DETLOG syscall record is
identical and `log-diff`/`--verify` cannot see it. The 1468 bytes land in a
**stack** buffer, so `--detlog-stack` can.

Isolated standalone with `netlink_probe.py`: 3 hermit runs, 3 distinct payloads,
1488 bytes each, **exactly 4 bytes differ**, decoded to interface `lo`,
`IFLA_AF_SPEC` → `AF_INET6` → `IFLA_INET6_CACHEINFO`:

```
u32 index 1  tstamp          62,299,122 -> 62,299,159   host kernel jiffies (host uptime)
u32 index 2  reachable_time      41,219 ->     20,123   a value the host kernel RANDOMIZES
```

Neither is virtualized. Hermit isolates the network namespace (1488 bytes inside
vs 3060 native) but passes the reply contents through.

### THE REPRODUCER THAT MATTERS MOST

`netlink_silent.py` (same exchange, prints only the length — the bytes stay
internal, as in the real case):

```
hermit run --strict --verify --verify-strict --verify-json av/verify.json  -- python3 netlink_silent.py
  -> ":: Success: deterministic. Determinism verified."
  -> verified TRUE, bitwise_parity TRUE, verdict 'matched'

hermit run --strict --verify --verify-strict --detlog-stack --verify-json av/verify3.json -- python3 netlink_silent.py
  -> ":: Failure: nondeterministic."
  -> verified FALSE, bitwise_parity FALSE, verdict 'diverged'
```

Same binary, same guest, **one flag different**. **L2 as this project defines it
certifies a program whose internal state differs between the two runs.** L3
(`--detlog-stack`) catches it.

### DEMO 6 (snapshot→snapshot resume), 10 runs, separately

| stream | result |
| --- | --- |
| info log | **10 distinct of 10** |
| serial | 1 of 10 |
| post-command snapshot | 1 of 10 |

Two leaks, found in order:
1. **arm `d6`** — first divergence at DETLOG message **2,317 of 630,469** (0.37%
   in, i.e. EARLY): `finish syscall #352: poll(0x7fffffffaf10, 3, 0) = Ok(0)`
   vs `Ok(1)`. Straced natively the call is
   `poll([{fd=0,events=0},{fd=1,events=0},{fd=2,events=0}], 3, 0)` — CPython's
   check that its three standard descriptors are open. With `events=0` the only
   reportable conditions are POLLERR/POLLHUP/POLLNVAL, so a `1` means one of
   fd 0/1/2 was **hung up on the host**. Demonstrated directly:
   `stdin=/dev/null` → Ok(0); `stdin=pipe` → Ok(1); `stdin=regular file` →
   Ok(0). My arm inherited a heredoc pipe; run 1 saw it open, the launching
   shell then exited, runs 2–10 saw POLLHUP.
2. **arm `d7`** — same 10 runs with driver stdin pinned to `/dev/null`
   (verified via `/proc/PID/fd/0`). The poll leak is gone (Ok(0) 10/10) and
   `--ignore-lines 'DETLOG [memory]'` gives
   `no substantive differences found (508,693 messages)`. But info logs are
   **still 10 distinct of 10** — the netlink leak above.

**I registered the prediction "pinning stdin takes demo 6 to 1 of 10" before
running d7. It failed. Recorded as a miss.**

---

## 1. WHERE I WORKED — every absolute path written to

**Repo:** `/home/newton/work/dev-hermit` (parent checkout, shared), branch
`cc-triage-reports_20260818` @ `9cf41a49`.

**I created NO branch. I made NO commit. I ran no `git` command other than
`status`/`log`/`rev-parse`.**

**Everything I wrote is under ONE directory, untracked:**

```
/home/newton/work/dev-hermit/experiments/first-divergence_20260820/
```

It appears in `git status --short` as a single line: `?? experiments/first-divergence_20260820/`.

I verified with `find -newermt` that I wrote **nothing** elsewhere in the
dev-hermit tree. I did not touch `hermit/`, `reverie/`, `demos/`, `ci-hub/`, or
any other agent's experiment directory. I *read* from
`experiments/detcore-partial-determinism_20260819/` and executed its
`scalars.py`/`qcow2*.py` in place; its `__pycache__` entries are all dated
yesterday, so I added nothing there.

---

## 2. UNCOMMITTED RIGHT NOW — the part that gets lost

**All of it. Nothing I produced is committed anywhere.** Everything below is
untracked files in the shared parent checkout. Per the instruction I did **not**
commit to "save" it.

### Scripts I wrote (the reusable part — ~14 KB total, cheap to preserve)

```
experiments/first-divergence_20260820/boot5.sh          2485 B  boot with all FIVE streams captured separately (--log-file for info; stdout/stderr on own fds)
experiments/first-divergence_20260820/arm5.sh            260 B  N x boot5.sh
experiments/first-divergence_20260820/resume_one.sh     2270 B  demo-6-shaped snapshot resume, constant paths
experiments/first-divergence_20260820/arm6.sh            834 B  N x resume_one.sh
experiments/first-divergence_20260820/fixedpath.sh      1536 B  N boots into ONE fixed path (runs/fix), logs copied out
experiments/first-divergence_20260820/fixedpath2.sh     1530 B  same, into runs/f2, so two fixed-path arms can run at once
experiments/first-divergence_20260820/arm.sh             775 B  N boots into per-run dirs (SUPERSEDED — see the argv trap in §4)
experiments/first-divergence_20260820/compare.sh        2061 B  per-arm five-stream table + log-diff vs r1 + raw compare
experiments/first-divergence_20260820/cost_probe.sh     1131 B  syscalls completed in a fixed wall window, per instrument
experiments/first-divergence_20260820/netlink_probe.py  1625 B  ** the AF_NETLINK reproducer; prints the payload **
experiments/first-divergence_20260820/netlink_silent.py 1650 B  ** same but prints only the length -> the L2-vs-L3 demo **
experiments/first-divergence_20260820/boot_one.sh       1130 B  COPIED verbatim from detcore-partial-determinism_20260819; not mine, unmodified
```

`netlink_probe.py` and `netlink_silent.py` are the two files I would most want
preserved: they reproduce the finding in ~2 seconds without any QEMU.

### Evidence (25 GB, untracked, prunable)

```
experiments/first-divergence_20260820/keep/fm/   10 kernel boots, fixed path      (10 x 499 MB info logs + snapshots)
experiments/first-divergence_20260820/keep/b5/   10 kernel boots, 5 streams split (10 x 499 MB + stdout/stderr/snapshot/serial)
experiments/first-divergence_20260820/keep/d6/   10 demo-6 resumes                (10 x 138 MB)
experiments/first-divergence_20260820/keep/d7/   10 demo-6 resumes, stdin pinned  (10 x 138 MB)
experiments/first-divergence_20260820/keep/fh/    6 bounded boots, regs+stack+HEAP (6 x 13 MB)  -- log bodies 1 of 6 IDENTICAL
experiments/first-divergence_20260820/runs/      snapshots + serial from the early arms; the big logs here are already PRUNED
experiments/first-divergence_20260820/av/        the small controls: A1/A2/B1 argv test, P1-P4 stdin test, verify JSONs, netlink hex dumps
experiments/first-divergence_20260820/*.log      per-arm drivers' stdout (arm_m, arm_h, fix_m, fix_h, arm5_b5, arm6_d6, arm6_d7)
```

No document was written to the parent root. **This HANDOFF.md is the only
Markdown I created, and it is inside my own experiment directory.**

All substantive findings are ALSO in the TaskGraph notes on
`find-the-first-divergence-in-hermit-boot` (8 notes), which survive a restart
independently of this tree.

---

## 3. COMMITTED BUT UNPUSHED

**Nothing.** No branch created, no commit made, in any repo.

---

## 4. WHAT A FRESH AGENT CANNOT CHEAPLY REDISCOVER

1. **`--detlog-stack` is VALIDATED IN BOTH DIRECTIONS.**
   `hermit run --strict --detlog-stack -- /bin/true aaaaa` twice → first three
   stack hashes `494984bf92433d10 0d2249a4feb9abcd 55d4431f56c1793e`,
   **identical**, and `hermit log-diff` says
   `no substantive differences found (369 | 369 DETLOG messages compared)`
   over 110 stack samples. Change one character of argv
   (`/bin/true bbbbb`) → `99583f9cefa538b4 ...`, different from the FIRST
   sample. The instrument is sound and hermit passes it.

2. **Fixed-WIDTH directory naming is the WRONG control here.** Prior art
   (`detcore-partial-determinism_20260819`) proved pathname LENGTH is worth
   10 ns of logical time per character and used fixed-width names. That is not
   enough once the stack is hashed: **argv lives on the initial process stack**,
   so two same-length names differing in one character give different stack
   hashes from sample 1. Only a **byte-identical** pathname isolates the
   run-to-run question. This cost me one whole arm (`m`/`h`) which reported a
   "first divergence at message 19" that was my own input.
   Use `fixedpath.sh`, not `arm.sh`.

3. **`hermit --log-file` under `/tmp` silently produces NO FILE** — exit 0, no
   warning, no file. Owner has ruled this a filed bug (the path should resolve
   in the host namespace, not the guest's) and it is not ours. Workaround:
   either point `--log-file` outside `/tmp` (works — `boot5.sh` does this) or
   send the log to stderr and do the shell redirect yourself.

4. **The serial-pipe injection is the STANDING HYPOTHESIS and disk injection is
   the APPROVED REPLACEMENT.** My finding on it, which changes its priority:
   - The demos (`demos/lib/qemu_controller.py`) run the python controller
     **INSIDE** the hermit container, so its writes to the `-serial pipe:` FIFO
     are scheduled by hermit. Demo 5 boot uses `-serial file:` and injects
     nothing.
   - `experiments/vcpu-steering_20260820/hermit_run.py` — the harness that
     produced "nine distinct counter values" — injects from **OUTSIDE** the
     container: `time.sleep(settle)` (a host sleep, 25 s) then
     `open(base+".in","w").write(f"advance {K}\n")` at lines 166/172-173, plus a
     host `dd` reading `.out` at line 75. That one is genuinely host-timed and
     the disk-input change is needed there.
   - **But serial injection is NOT what breaks the demos.** Demo 6's divergence
     is the netlink leak, before QEMU even exists. Fixing the injection will not
     make demo 6 reproducible; fixing netlink might.

5. **`--detlog-heap` is unusable on this workload and here is why.** QEMU's
   `[heap]` grows monotonically (measured 132 KB → **18.2 MB** by syscall
   15,612, only 2.5% into the boot) and `detlog_memory_maps`
   (`detcore/src/lib.rs:2430`) re-hashes the WHOLE region after **every**
   syscall exit — so the work is quadratic in syscall count. Full boot with all
   three flags: reached 5.3% in 600 s, extrapolating to ~3.1 h per boot. Use
   `--stop-after-turn N` to bound it (arm `fh` did 8,000 turns in ~105 s).

6. **`hermit run` ignores SIGTERM** (guest is a namespace init). `pkill -TERM`
   left all 7 processes alive after 8 s; `pkill -KILL` cleared them.

7. **A wall-clock cap script must not match its own command line.**
   `pkill -KILL -f "$OUT"` killed my reporting shell along with the run.

---

## 5. PROCESSES STILL RUNNING

**None of mine.** Every arm completed:
`arm_m`, `arm_h`, `fix_h` (fh), `fix_m` (fm), `arm6_d6`, `arm6_d7`, `arm5_b5`
all printed their DONE line. Verify with:

```
ps -eo pid,args | grep -E "arm5|arm6|arm\.sh|fixedpath|boot5|boot_one|resume_one|first-divergence" | grep -v grep
```

**Not mine, present before I started, still there** (recorded so a fresh agent
does not attribute them to this task): 4 × `qemu-system-x86_64` writing
`/tmp/scxprobe/serial.log`, a `hermit-clean-48a648c8` KVM run, 2 stopped
`qemu -S`, a `hermit ... ignored/pdeathsig/...` run, and the
`vcpu-steering_20260820` `hermit_run.py g3` tree.

---

## 6. TIMING AND DISK — the three numbers asked for

### 6.1 Wall time of one boot, by instrument

All demo-5-shaped, same binary, same box:

| configuration | wall | info log | ratio |
| --- | --- | --- | --- |
| **no logging** (`RUST_LOG` unset) | **43.71 s** | 123 B | 1.00x |
| `RUST_LOG=warn,detcore=info,reverie_ptrace::task=info` | **50.6–53.1 s** | 252 MB | **1.16–1.22x** |
| + `--detlog-regs` | 56.5 s | 361 MB | 1.29x |
| + `--detlog-regs --detlog-stack` | **265–297 s** | 499 MB | **6.1–6.8x** |

**The info log itself is cheap: ~7–9 seconds, about 20%.** The 6x is
`--detlog-stack`, not the log. That matters for how this is structured: turning
the log on is nearly free; hashing memory is what costs.

One boot is 632,229 guest syscalls, 191,796 COMMIT turns, 2,001,250 log lines
without the hash flags.

### 6.2 Where the ~100 minutes went (06:16–07:57)

| phase | wall | note |
| --- | --- | --- |
| harness, calibration, cost probe | ~25 min | includes a 10-min aborted all-three-flags run that established `--detlog-heap` is quadratic |
| arm `m` — 10 boots **IN PARALLEL** | **~5 min** | 06:36–06:41 |
| arm `fh` — 6 bounded, sequential | ~10 min | |
| arms `d6`+`d7` — 20 resumes, sequential | ~18 min | |
| arm `fm` — 10 boots, **SEQUENTIAL** | **45 min** | 06:42–07:27 |
| arm `b5` — 10 boots, **SEQUENTIAL** | **45 min** | 07:12–07:57 (overlapped fm) |

**The owner's correction is right and the `m` arm proves it with my own data:
ten boots in parallel took 5 minutes; the same ten sequentially took 45.** The
two sequential 10-boot arms are ~80 minutes that parallel execution would have
made ~10. Content comparison does not care about box load — and if it did, that
would itself be the bug. **A fresh agent should run all ten at once.**

### 6.3 Disk

**25 GB right now**, in
`/home/newton/work/dev-hermit/experiments/first-divergence_20260820/`.
Filesystem `/dev/md0`: 3.5 T total, 2.0 T available, 42% used.

**My logs are UNBOUNDED. There is no size cap anywhere in my harness.** Stating
that plainly as asked. Peak rate: one boot writes 499 MB in ~270 s ≈ **6.5 GB/h
per concurrent boot**; ten in parallel would be ~65 GB/h. That is well under the
25.5 GiB/h writer that tripped the headroom monitor only because mine run for
minutes, not hours — but a long or looping arm **must** add a cap.

I did prune as I went: the superseded argv-varying `ctrl.err` logs (7 files,
~4 GB) were deleted after their findings were extracted, keeping their snapshots
and serial output as evidence.

---

## 7. EXACT NEXT STEP

The headline measurement is **done**; a fresh agent does not need to repeat it.
In priority order:

1. **Answer the prefix question — I already can, and the answer is NO for the
   boot.** The first divergence is at message **2,862,447 of 2,869,805**, i.e.
   the last 0.26% of the log. **Truncating the logs to a common prefix would
   have reported them clean and been wrong.** For demo 6 the opposite holds
   (first divergence at 0.37%), so prefix comparison is a valid cheap screen
   *there* but not for the boot. Do not adopt it as the general method.

2. **The netlink leak is the thing to fix and it is not mine to land.**
   Determinizing a new syscall payload class is trigger 3 of
   `post-facto-human-review` (new determinization strategy). The narrow fix is
   to virtualize the `NETLINK_ROUTE` reply the way time and entropy already are
   — at minimum fix or zero `IFLA_INET6_CACHEINFO`'s `tstamp` and
   `reachable_time`, which is where all 4 differing bytes were.
   **The tempting wrong fix** is to add these lines to a comparator ignore list;
   that reaches 10 of 10 by deleting the evidence and is the anti-pattern
   `continuous-virtual-time-is-sacred` names.
   Reproduce in 2 seconds with `netlink_probe.py`; no QEMU needed.

3. **The stdin/poll leak is real and separate**, and is already fixed at the
   harness level by pinning stdin. Whether hermit should virtualize the hangup
   state of its own fd 0/1/2 is a genuine design question: the *connected
   object* is a legitimate input, but *whether its peer has closed by the moment
   the guest polls* is a race with the host.

4. **Gap to report on the DAG runner** (checked, as asked). `--stress N` exists
   and does exactly the graph multiplication described:
   `_expand_stress` at `hermit/agent-utils/py/safe_ci_dag_runner/cli.py:1041`
   clones the graph N times "and run the copies IN PARALLEL". **But the copy
   index is a `#NN` suffix on the JOB NAME only** — `_expand_stress` rewrites
   `job` and `deps` via `dataclasses.replace` and leaves `command` and `env`
   untouched, and the scheduler (`scheduler.py:486-490`) exports only
   `STEP_NONCE_ENV`, a freshly minted nonce, not a stable rank.
   **So no stable per-copy identifier reaches the command.** Ten copies of one
   command write to the same output path and clobber each other. A stable
   rank-like variable (the MPI-rank analogy) does **not** exist today; that is
   the gap. The nonce would break symmetry but is not stable across reruns, so
   it cannot name a comparable artifact.

5. Nothing here needs to land in `hermit/` or `reverie/`. If the experiment
   directory is worth keeping it should be committed **from a clean worktree**,
   never by `git add` in this shared parent checkout.
