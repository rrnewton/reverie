# LiteInst: blockers to wider determinism coverage, parity experiments, strategies tried, and how the backend works (2026-09-29)

Author: liteinst-lane-claude (claude, devbig014), written 2026-09-29 around 15:50Z at the owner's request.

Refs used throughout, unless a line says otherwise:
- hermit main 350fa554b472aed0e1504453f4ecc8e2e0a4d8d1, read at about 15:12Z;
- reverie main ed7851f319a11b59f88c1abde200df46346e2083;
- Hermit's reverie pin 6297f7154299e30bed97e6aead6ae7f5e1fc45ed, which is 46 commits behind reverie main;
- dev-hermit main c3b6d1990b.

Labels: **[MEASURED]** is a number read from a receipt, log or command output. **[READ]** comes from code or documents. **[INFERRED]** is my reasoning and has not been measured.

## TL;DR

**Scorecard of record.**
- s22: receipt `validate-coord-s22-356dfd3ede03-1790677291871670711-3535309-c9910280`, hermit 356dfd3e, run 10:24:34Z to 11:32:21Z (68 minutes).
- File: `ai_docs/liteinst-lane/scorecard.md` on dev-hermit branch `liteinst-lane/scorecard-20260927` @ 16073ef0.

| Cells where ptrace is green (369) | LiteInst |
|---|---|
| Determinism: the same test run twice under LiteInst produces byte-identical determinism logs | **146/369 = 39.6%** [MEASURED] |
| Parity: LiteInst's determinism log equals ptrace's | **0/369 = 0.0%** [MEASURED]. Of the 99 cells with a parity record, 98 diverged, 1 had no LiteInst log and 0 matched. The other 270 have no parity record: 49 are outside the parity selection and 221 have no LiteInst cell |

1. **Most of the missing determinism coverage is held up by landing, not by the product.**
   - On 2026-09-27 I screened all 215 unselected LiteInst verify cells once at hermit 19553a64 (212 disabled, 3 enabled but not selected). The 182 that passed the screen then ran 10 times each, and 178 passed all ten [MEASURED].
   - 162 of them are selected, with 1,620 clean strict runs, on an unlanded branch, https://github.com/rrnewton/hermit/pull/3254.
   - It has not landed for three reasons:
     - hermit main has been red;
     - every hermit landing is blocked on an expired validation trust root (https://github.com/rrnewton/dev-hermit/issues/414);
     - the coordinator is holding it behind the parity-measurement overhaul.
   - If those cells re-qualify at the landing head, determinism rises to roughly **308/369, about 83%** [INFERRED upper bound; not measured at the current head].
2. **Behind the landing queue, the census found 37 non-passing cells in 15 families.** 28 cells in 8 families (F1, F2, F4-F9) need LiteInst work; one of those families (F5, 5 cells) is speed against the time budget. The other 9 are by-design refusals or test, gate or host issues (6), cells with no ptrace reference (2), and a host-PMU refusal (1). The biggest LiteInst families:
   - vfork refused, 6 cells;
   - an exec'd child reaching its entry point before the runtime has checked in, 8 cells (7 static or no-libc images, 1 PIE child);
   - a kernel-internal restart code (ERESTARTSYS, -512) reaching the guest, 5 cells. The fix is on an unmerged, unpushed reverie branch, stalled on an open slowness defect (section 3.2); it is **not** on reverie main.
   - over the 22 CPU-second budget, 5 cells. The budget may not be widened; only a faster LiteInst fixes this.
   - LiteInst refuses record/replay and chaos by design. Only 8 of the 369 cells are in those modes (chaos 6, replay 2).
3. **Parity is 0 for one structural reason.**
   - Today's LiteInst loads a helper library into the guest with `LD_PRELOAD`. Loading it changes what the guest sees before `main` runs: an extra environment variable, extra libraries, about 316 extra syscalls, shifted heap, libc and TLS addresses, and extra `rdtsc` reads.
   - So every compared log first differs at record 16, and the LiteInst log runs about 916 records longer.
   - No local fix inside the preload design changes this, and my experiments show it (section 5).
4. **Yes, I ran parity experiments.** Seven are listed in section 5. They produced the root cause, the eight measured divergence sources (D1-D8), and one partial fix: https://github.com/rrnewton/reverie/pull/777 made the heap tail identical to ptrace, but the first divergence stayed at record 16.
5. **The chosen parity route is "TrapOnly": the tracer patches syscall sites itself and nothing is loaded into the guest.**
   - Phase 1, an inert launch that patches nothing, is on reverie main but not in Hermit's pin.
   - Phase 2, the actual patching, is https://github.com/rrnewton/reverie/pull/680. It is approved and waiting for exact-head validation. It compiles only for tests until the timer rules are written and the P2e safety gates are built (seven main gates in https://github.com/rrnewton/reverie/issues/776, plus test-only and P2d items named in the module docs).
   - Hermit cannot select TrapOnly yet.
6. **Honest caveat on speed.** Nothing in the current LiteInst or TrapOnly work removes the ptrace stop from the syscall path.
   - Shipped LiteInst still makes one ptrace round trip per hot syscall.
   - TrapOnly costs 3 tracer stops per patched syscall against ptrace's 2.
   - The parity work is a bridge; the ptrace-free goal (vision item 5) still needs an in-guest runtime later.

---

## 1. What "determinism" and "parity" mean on the scorecard

- **Cell**: one test in one mode (verify, chaos, custom or replay).
- **Denominator**: the 369 cells whose ptrace run passed in the same receipt: verify 358, chaos 6, custom 3, replay 2 [MEASURED, s22]. Non-verify cells stay in the denominator. LiteInst cannot run them today, and dropping them would raise the percentage without any LiteInst change.
- **Determinism matched**: under strict verify, LiteInst's run and its repeat produce identical Hermit determinism logs (every Detcore record, bit for bit). This is a same-backend check; it says nothing about agreement with ptrace.
- **Parity matched**: LiteInst's determinism log equals ptrace's for the same test.
  - The comparison is `hermit log-diff` with the `cross-backend-detcore-v1` filter. It keeps only Detcore's own records and does not round or reset virtual time or branch counts [READ, hermit `ci/manifest-plan/src/parity.rs:1213`, `hermit-cli/src/bin/hermit/record_envelope.rs:112-140`].
  - A post-pass writes one verdict per cell to `parity.jsonl`.
  - The owner rule for this lane: the comparator, the filter and the manifests are **not** changed to gain cells.

### Where the 369 cells stand (s22)

| Group | Cells | Meaning |
|---|---|---|
| A | 98 | LiteInst determinism matched; parity diverged. All `c-programs/*` verify cells; all diverge at record 16 |
| B | 48 | LiteInst determinism matched; no parity record (not in the parity selection) |
| R | 1 | `system-utils/clock-determinism` custom: runs, but by design compares nothing, so it is not counted |
| C | 221 | No LiteInst cell: the manifest lists LiteInst under `backends_disabled`. By mode: verify 211, chaos 6, custom 2, replay 2 |
| D | 1 | `c-programs/getrusage-self-accounting` verify: `ci.liteinst: false` after a determinism failure |

Group C's disable reasons [MEASURED, s22 scorecard]:
- 121 "Initial C-corpus migration preserves the established ptrace baseline". This placeholder was written when the tests were migrated; it is not a measured failure.
- 23 "LiteInst coverage is owned by its backend compatibility partition".
- 12 the DBT-contract reason on newly added cells.
- The rest are single-digit reasons.

All 147 LiteInst cells in s22 passed on their first attempt.

---

## 2. How the backend works: the main moving parts

### 2.1 The layers

```
            Hermit CLI  (hermit run --backend liteinst ...)
                 |
        Detcore  = the determinism "Tool": decides what every syscall returns,
                   keeps virtual time, schedules threads, writes the determinism log
                 |
        Reverie  = backends that deliver guest events to the Tool
                   ptrace | LiteInst | KVM | SaBRe | DBT | e9patch
                 |
              guest program
```

Detcore is the same Tool for every backend. A backend's job is to stop the guest at each syscall (and at `rdtsc`, `cpuid`, signals and timers), hand the event to Detcore, and apply Detcore's answer.

### 2.2 Plain ptrace, the reference

```
guest executes `syscall`  ->  kernel seccomp stop  ->  tracer runs Detcore
                          ->  tracer resumes guest (syscall runs or is emulated)  ->  exit stop
```

This costs two tracer stops per syscall (entry and exit) [READ, reverie `reverie-ptrace/src/task.rs:5222-5378`]. It is slow, but the guest's memory, environment and address layout are exactly those of an unmodified run.

### 2.3 LiteInst as Hermit ships it: the "host hybrid"

Hermit calls exactly one family of LiteInst entry points, `LiteinstBackend::run_host_with_preload::<Detcore>` [MEASURED by `git grep` at hermit 350fa554; `hermit-cli/src/lib.rs:2600-2615`]. The parts:

1. **A helper library in the guest.** `libreverie_liteinst.so` is prepended to `LD_PRELOAD`, and `REVERIE_LITEINST_HOST_RUNTIME=1` is set in the guest's environment [READ, reverie `reverie-liteinst/src/backend.rs:217-247`].
   - Hermit refuses to start unless the library's embedded revision equals Hermit's reverie pin, and unless it exports the expected symbols [READ, `hermit-cli/src/lib.rs:805,848,956`].
2. **A startup handshake.** The library's constructor signals the tracer with `int3` breakpoints carrying marker values in `rax`, and prepares for patching.
   - It also makes `cpuid` and `rdtsc` fault, through `ARCH_SET_CPUID` and `PR_SET_TSC`, so the tracer can virtualize them [READ, `reverie-liteinst/src/runtime.rs:834-968`].
3. **First-touch patching.** The first time a `syscall` instruction runs, it is stopped exactly as under ptrace. The tracer then calls an install helper inside the guest, which overwrites the instruction with a 5-byte jump to a trampoline [READ, `reverie-ptrace/src/task.rs:5050-5120`].
   - Patching stops for good as soon as the guest has a second task; later threads run under ordinary ptrace stops. Only vfork is refused (family F1) [READ, reverie `task.rs:5072` for the stop, pin 6297f715 `task.rs:5384-5394` for the vfork refusal].
   - Multi-threaded programs do pass under LiteInst (printf-with-threads, thread-sync-determinism, zstd-multithread and thread-interleaving in the census). The `bin-c.yaml:88` disable reason, which says clone3 is refused, is stale: that cell ran in the census and deadlocked (F11).
4. **The hot path.** Later executions of a patched site run: trampoline, hook, `int3`, a breakpoint (SIGTRAP) stop in the tracer.
   - The tracer reads the syscall's arguments from guest memory, runs Detcore, executes the real syscall from a private code page, writes the result back, and resumes the guest [READ, `task.rs:3226-3324`].
   - **So every hot syscall is still a ptrace round trip. A breakpoint stop replaces a seccomp stop.**
5. **An activation probe.** Before each run, Hermit re-executes itself and makes 32 `getpid` calls through one site. It requires exactly `traps=1 hooks=31`, and fails closed otherwise [READ, `hermit-cli/src/bin/hermit/main.rs:62-140`].
6. **Refusals.** LiteInst is refused for every subcommand except `run`, so record/replay are not available, and chaos is disabled in every manifest [READ, `main.rs:359-362`; MEASURED manifest tally].

### 2.4 The in-guest Tool mode (built in August, never shipped)

- **What it is.** `run_with_preload*` runs Detcore inside the guest and reaches the shared state over an RPC channel to the host.
- **Speed.** It was the fast design: 583 ns marginal per syscall with a counting Tool (ptrace: 17,101 ns with the same Tool), and about 14.2 µs with Detcore `--strict`, which the same measurement put at 2.84x faster than ptrace under that Tool [READ, `ai_docs/transient/liteinst-perf-attribution-fastpath-is-leader_20260804.md`].
- **Limits.** One thread per process, exec refused, no guest signal handlers, timers that silently do nothing, and a vDSO clock that leaked host time [READ, reverie `reverie-liteinst/README.md`].
- **Hermit never calls it.**

### 2.5 TrapOnly, the parity route under construction

- **Idea.** Do the patching from the tracer, with **nothing loaded into the guest**.
  - The tracer rewrites a `syscall` it has already seen (bytes `0f 05`) into `int 0x80` (bytes `cd 80`). Later executions of that site then arrive as 32-bit seccomp stops.
  - The tracer runs the real 64-bit syscall through a "masked hop": a traced stub on Hermit's private page, run with all signals blocked. It then shows the guest and Detcore exactly the registers a plain `syscall` would have produced [READ, reverie `reverie-ptrace/src/liteinst_trap_only.rs:28-34,204-218` at https://github.com/rrnewton/reverie/pull/680 head 5139c80a].
- **Why `int 0x80` and not `int3`.** An `int3` forces a SIGTRAP, which clears `SIG_IGN` and unblocks it, and the timer's single-step loop swallows it [READ, `ignored/liteinst-lane-claude/tracer-side-reconciliation.md`].
- **Keeping patches valid.** There is one site table per address space. Patched bytes are restored before any mapping change (mmap `MAP_FIXED`, munmap, mremap, mprotect, madvise, and similar). The table is disabled permanently when a second task starts, and when the guest installs its own seccomp filter or Syscall User Dispatch.
- **Phases:**
  - **P1**, landed on reverie main (3ac26b7e4, 2026-09-28; https://github.com/rrnewton/reverie/pull/669). `SitePatching::Off`: patches nothing, uses a seccomp filter byte-identical to ptrace's, and is observably the ptrace run. It is **not in Hermit's pin** [MEASURED, merge-base check].
  - **P2**: https://github.com/rrnewton/reverie/pull/680, 17 commits, approved at 5139c80a and waiting for its exact-head validation.
    - `SitePatching::On` is compiled only under `#[cfg(test)]` [READ, `liteinst_trap_only.rs:169-177`].
    - Two things keep it there: the timer rules are not written, and the P2e gates are not built (the seven below, plus end-to-end tests of the `pkey_mprotect`, `remap_file_pages`, `shmat(SHM_REMAP)` and `PROT_GROWSDOWN` rules, a failed mapping syscall, and a new child when `kcmp(KCMP_VM)` cannot answer) [READ, `liteinst_trap_only.rs:59-116`].
  - **P2e**: seven safety gates (https://github.com/rrnewton/reverie/issues/776). Each covers a way guest code or mappings can change without a stop the tracer sees:
    - io_uring `MADVISE` and `brk`;
    - a write to the file behind a patched mapping;
    - another process writing guest text;
    - `/proc/self/mem` self-writes;
    - an inherited seccomp filter or Syscall User Dispatch;
    - `process_madvise` naming another process;
    - Tool-injected syscalls that bypass the site table.
  - **Hermit P3**: bump the pin, add a hidden `--liteinst-mode {preload,trap-only}`, and add a second parity arm.
    - Reported metrics: M1 = cells at parity, M2 = fraction of syscalls that went through patched sites, M3 = syscalls that needed no ptrace stop.
    - M2 matters because TrapOnly with patching off *is* ptrace. A parity number with M2 = 0 would say nothing about LiteInst [READ, `ignored/liteinst-lane-claude/traponly/PLAN-v2.md`; the M2 reasoning is INFERRED].
- **Cost.** 3 tracer stops per patched syscall against ptrace's 2 [READ, `ignored/liteinst-lane-claude/parity-understand/PLAN.md` section 7]. It is a parity bridge, not a speedup.

### 2.6 The self-contained runtime (the later speed route)

A no-libc code image that the tracer places into the guest, invisibly, at the post-exec stop, with its setup also run by the tracer. It is the intended way to reach syscalls that need no ptrace stop (M3 > 0).

The first design was reviewed and rejected on 2026-09-28. Its fixed addresses were in the kernel half of the address space, and it would have changed the ptrace reference or the comparator. Only the tracer-placed, no-libc core survives [READ, `ignored/liteinst-lane-claude/self-contained-runtime-design-review.md`].

### 2.7 How tests opt in to LiteInst

- Each test in `tests/e2e/manifests/*.yaml` has five modes. Each mode carries:
  - `backends_enabled`;
  - `backends_disabled`, with a written reason per backend;
  - a `ci` flag per backend.
- At hermit 350fa554, across 374 tests [MEASURED, yaml tally]:
  - LiteInst verify is enabled in 149 tests (146 run in CI) and disabled in 225.
  - It is disabled in every replay and every chaos cell.
  - The top verify disable reason is the 127-test migration placeholder.

---

## 3. Blockers to expanding determinism coverage

### 3.1 Landing and process: the largest block by cell count

**Census.**
- **When and where**: 2026-09-27, hermit 19553a64, reverie pin b0ede531.
- **What ran**: all 215 unselected LiteInst verify cells (212 disabled, 3 enabled but not selected), screened once each through the repeated-run qualification harness (strict verify, CPU and wall timeouts). The 182 that passed the screen then ran 10 times each.
- **Result**: 178 passed all ten, 16 failed, 6 hung, 15 were refused [MEASURED, `ignored/validate/liteinst-groupc-19553a64/REPORT.md`].
- **Check**: an independent serial rerun agreed on all 178 passes.

**Promotion branch.**
- `claude/liteinst-promote-census-r2-20260928` @ 15c02893, carried by https://github.com/rrnewton/hermit/pull/3254.
- It selects **162** of those cells, each with 10 clean first-attempt strict runs: 1,620 runs; an independent audit qualified 166.
- Screen at b280bc4807: 162/162 PASS in 1,770.6 s [READ, hermit `15c02893:docs/LITEINST_QUALIFICATION_20260927.md`].
- The branch takes LiteInst verify from 146 selected to 308.

**Why it has not landed:**
- **hermit main has been red.** The landing package of 2026-09-28 recorded no full-validate PASS since 2026-09-11, at c7520a6c3873. Landing on a red main needs an owner judgement. The coordinator is now fixing main's s22 reds directly.
- **Every hermit landing through `land-pr.sh` is blocked** on https://github.com/rrnewton/dev-hermit/issues/414. The registered producer definition of the validation receipt, which is a trust root, no longer matches hermit main.
  - I own the fix. The rotation is implemented at hermit 350fa554's map (dev-hermit a03a89aa, not pushed) and is under two adversarial reviews.
  - The final map has to be derived after the parity-overhaul commits (`runner.rs`, `scripts/validate.rs`) land on hermit main, and pushing needs the coordinator's explicit OK.
- **The coordinator is holding https://github.com/rrnewton/hermit/pull/3254** until the shared parity-measurement overhaul lands, because the branch touches manifests and `ci/dag/validate.json`.
- **Validation throughput.** One full validate takes about an hour (s22 took 68 minutes). The fleet runs one at a time, and I may hold only one queue slot.
  - Today admission refused my run twice with "read-only entry classification budget expired". The 30 s budget was counting time spent waiting for locks. It is fixed on dev-hermit main by ed07766df4, and I moved my validation chain onto a tool checkout that has the fix.

**Caveat.** The 162 qualified at older hermit heads. They must pass again in the exact-head validation of the landing commit; old evidence is not carried over.

### 3.2 Non-passing cells in the census: 37 cells, 15 families

| Family | Cells | What happens | State |
|---|---|---|---|
| F1 vfork refused | 6 | "exec cannot preserve the preload runtime" | Built into the preload design; expected to go away only with TrapOnly [INFERRED] |
| F2 entry before handshake | 8 | An exec'd child reaches its entry point before the runtime has checked in (7 static or no-libc images, 1 PIE child) | Built into the preload design [INFERRED] |
| F4 ERESTARTSYS -512 reaches the guest | 5 (+ perl `close()`) | The LiteInst stop is a breakpoint, not a syscall stop, so the kernel never restarts an interrupted injected syscall and the raw restart code leaks into the guest | **Not fixed on reverie main.** The fix is on the unpushed branch `claude/liteinst-hybrid-restart-20260927`. Its round 3 stalled on 2026-09-27: a new test passes but takes 113-119 s under LiteInst against 0.17 s under ptrace, because the tracer single-steps most of a spin loop after a timer step window; the cause is not yet found [MEASURED, `ignored/liteinst-lane-claude/f4-restart/restart-r3-blocked-state.md`] |
| F5 over the 22 CPU-s budget | 5 | LiteInst is slower than ptrace on these cells (example-timed-progress-bar, kvm-python-examples, timed-progress-bar, writev-determinism, dd-partial-transfers) | The bound may not be widened |
| F6 hang after exit | 1 | Hang after the injected `exit_group` | Open |
| F7 `times()` clock does not advance | 1 | resource-determinism | https://github.com/rrnewton/hermit/pull/3261, validation queued |
| F8 `arch_prctl` GS value lost | 1 | | Open |
| F9 aliased `recvmsg` EFAULT | 1 | | Open |
| F10, F11 | 2 | No ptrace reference. F10: nondeterministic epoll polling. F11: a deterministic robust-futex deadlock (owner death not released) | Open |
| F12 PMU skid refusal | 1 | This is group D (getrusage) | Host PMU |
| F3, F13, F14, F15 | 6 | By design, test defects, or a gate defect | F14: https://github.com/rrnewton/hermit/pull/3263, held behind the parity overhaul |

Source: `ignored/validate/liteinst-groupc-19553a64/REPORT.md` [READ].

Beyond the 37:
- **Six passing cells** are held for an owner decision on design grounds, for example kvm-shell-environment and thread-interleaving.
- **Six cells** are excluded because the timeout bound derived from their times (1.5 x p90 CPU, 4 x p90 wall, or the maximum-derived form) exceeds the unchanged 22 CPU-s / 57 wall-s bound: proc-locks, shell-pipeline, sqlite-query-determinism, zstd-multithread, bash-loop-pipe-time, node-v8-jit. Their measured times themselves are under the bound (zstd-multithread's highest was 15.3 CPU-s).
- **Four qualified cells** are held:
  - sort-random diverged in 1 of 10 runs on a `/proc/self/maps` read. That class is the target of https://github.com/rrnewton/hermit/pull/3255, which is approved and validating.
  - perl hit the F4 restart leak in exact-head validation.
  - two cells await a separate decision.

### 3.3 Correctness fixes in flight

These are the timing, signal and child-tracking defects that LiteInst runs expose. All are open PRs on my lane.

- https://github.com/rrnewton/reverie/pull/649: keeps a precise timer event that another stop overtakes. Its exact-head validation failed only on the unrelated flake https://github.com/rrnewton/reverie/issues/724. The rerun at 9515fecc is queued behind https://github.com/rrnewton/hermit/pull/3261; a 15:47Z attempt died before admission (my tool checkout was incomplete) and produced no receipt.
- https://github.com/rrnewton/reverie/pull/665: keeps precise timer events across LiteInst hook traps that the Tool does not see. Round 7 was approved by both review lenses at cac9afde. Round 8 carries it onto the rebased 649 head, which conflicts with main's new x86_64 gates, and gets fresh exact-head reviews.
- https://github.com/rrnewton/reverie/pull/763: registers a new child decoded during a timer step before its stop can be dropped.
- https://github.com/rrnewton/reverie/pull/771 plus the hermit half of https://github.com/rrnewton/hermit/issues/3338: stop charging the runtime's startup to guest virtual time.
- https://github.com/rrnewton/reverie/pull/775 (for https://github.com/rrnewton/reverie/issues/686): retire stale stops left before the exit stop. A fix round is in progress.
- https://github.com/rrnewton/hermit/pull/3361: lets a pending signal interrupt a blocked futex wait (EINTR). It also grows `test.liteinst_strict` from 38 to 56 tests.
- https://github.com/rrnewton/reverie/pull/782: vfork/clone3 in LiteInst's strace/compat modes. This is not the host hybrid, and it does not address F1.

### 3.4 The pin lag

Hermit pins reverie 6297f715, which is 46 commits behind reverie main [MEASURED]. Every reverie fix, including TrapOnly P1 and the signal-requeue fix db8890989 (https://github.com/rrnewton/reverie/pull/678), reaches Hermit only through a pin-bump PR. That PR also updates the expected test counts recorded in the CI plan and validation DAG files.

The planned bump carries the fix for https://github.com/rrnewton/reverie/issues/686 (https://github.com/rrnewton/reverie/pull/775), plus https://github.com/rrnewton/reverie/pull/761 and https://github.com/rrnewton/reverie/pull/771, with the detcore half of https://github.com/rrnewton/hermit/issues/3338 as its own commit. It waits for those reverie PRs to land. Even a bump to current reverie main would not bring in the F4 restart fix, because that fix is not on main (section 3.2).

### 3.5 Modes LiteInst cannot run

LiteInst refuses record/replay and chaos. Only 8 of the 369 denominator cells are in those modes (chaos 6, replay 2), so this is a small share of the gap today. It becomes the whole remaining gap once verify is closed. LiteInst does run custom mode (group R); the 2 group-C custom cells are disabled because no custom arguments have been calibrated for them, not because of a mode refusal.

### 3.6 Sample size

Ten runs do not rule out rare divergence: sort-random failed once in ten. Two runs miss a failure that occurs in 13 of 30 runs about 55% of the time [READ, memory note on per-cell self-determinism]. This is why promotions require 10 clean strict runs plus exact-head validation, and why that bar is not lowered.

---

## 4. Blockers to parity

**The single structural cause.** Under ptrace, record 16 of the log is the loader's `access("/etc/ld-nix.so.preload")`. Under LiteInst it is an extra `rdtsc` from the loader, followed by the open and a 68 MB mmap of the runtime library.

This holds in all 98 group A cells (s22, hermit 356dfd3e), and the LiteInst log is 916 records longer in 96 of them (914 and 1028 in the other two) [MEASURED, s22 parity-shape join: first divergence {16: 98}, length delta {916: 96, 914: 1, 1028: 1}; `ignored/liteinst-lane-claude/s22-scorecard/parity-shape-s17-s22.txt`]. The same roughly 916-record overhead is described in https://github.com/rrnewton/reverie/issues/750. For example, aio-refusal makes 35 guest syscalls under ptrace and 379 under LiteInst in the KVM lane (Nix glibc 2.42); on the host (glibc 2.34) it is 41 vs 374.

**The eight divergence sources** (2026-09-27, reverie 7a505332, hermit 19553a64) [MEASURED, except where a row says otherwise]:

| # | Source | Plain meaning |
|---|---|---|
| D1 | `LD_PRELOAD` and `REVERIE_LITEINST_HOST_RUNTIME` stay in the guest environment | Stack shifted by 0x70 bytes, so every stack address in the log differs |
| D2 | The loader loads the runtime and libgcc_s | +22,771 counted branches, 164 vs 7 hashed I/O-buffer log records, 36 vs 2 `fstat` calls injected by Detcore |
| D3 | The constructor makes 316 syscalls, mallocs early, installs a SIGTRAP handler | Extra log records; heap moved |
| D4 | 32 MiB Tool heap plus 32 MiB patch heap | libc mapped 0x4200000 lower |
| D5 | An extra thread-local-storage module | `fs_base` differs |
| D6 | Hook code runs inside the guest | Its branches are counted as guest branches, so the branch clock drifts |
| D7 | The install helper runs with the virtual clock on | A timer signal inside the helper is a hard error; virtual-clock drift is inferred, not measured |
| D8 | A signal can arrive while the guest is inside a trampoline | The signal frame's instruction pointer differs |

**Why local fixes cannot close it:**
- Fixing only the `rdtsc` surplus leaves all 97 LiteInst parity reds of run a9cf3263 red. In 97 of 97, ptrace's syscall sequence is not even a subsequence of LiteInst's [READ, comment on https://github.com/rrnewton/reverie/issues/627].
- Loading the runtime later with a guest `dlopen` cannot fix D2-D5.
- https://github.com/rrnewton/reverie/pull/777 fixed the heap tail, but record 16 did not move (section 5).
- Filtering the extra records, changing the comparator, or dropping cells are not allowed.

**The route is TrapOnly** (section 2.5). It removes the library behind D1-D5 and D7, and by construction the in-guest hook (D6); D8 is handled by re-dispatching a signal that arrives during the hop from the hop's slot.

What stands between here and a parity number:
- P2 validation and landing (https://github.com/rrnewton/reverie/pull/680);
- the timer rules and the seven P2e gates (https://github.com/rrnewton/reverie/issues/776);
- the Hermit pin bump and `--liteinst-mode` (P3);
- a measured run.

Known open P2 issues:
- https://github.com/rrnewton/reverie/issues/727: syscalls 335/336 are refused where ptrace returns ENOSYS.
- https://github.com/rrnewton/reverie/issues/728: the timer-step refusal fires at a rate that depends on host timing.
- https://github.com/rrnewton/reverie/issues/730: test gaps.
- https://github.com/rrnewton/reverie/issues/787: other P2 gaps.

**Not yet known [INFERRED risk]:**
- TrapOnly with patching on has never run under Hermit, so whether it actually closes record 16 is unmeasured.
- The hop's wall-time cost on real workloads is unmeasured.
- Intel's branch-counter behaviour is unmeasured; only AMD has been measured.

---

## 5. Parity experiments: yes, seven

| Date | Experiment | Finding |
|---|---|---|
| 2026-09-16 to 09-27 | Cross-backend comparison enabled (hermit e385d5f72a9b), then 31 runs | 6,010 of 6,010 LiteInst log comparisons diverged, all at record 16 [READ, `ai_docs/research/2026-09-27-liteinst-backend-parity-phase1.md`] |
| 2026-09-18 | `rdtsc` study across backends | LiteInst makes 12 `rdtsc` against ptrace's 10, both extra ones from the loader's preload handling. KVM and SaBRe have separate `rdtsc` defects [READ, `ai_docs/research/2026-09-18-rdtsc-backend-parity.md`] |
| 2026-09-27 | Phase-1 controls | 41 vs 374 syscalls on aio-refusal (host, glibc 2.34). **A ptrace run given the same environment bytes (`LD_PRELOAX=<runtime path>`, which the loader ignores, plus `REVERIE_LITEINST_HOST_RUNTIME=1`) reproduces LiteInst's stack addresses exactly, which proves the stack shift comes from the environment.** Heap 0x4052a0 moved to 0x40cae0; virtual time 4.04 ms vs 29.89 ms [READ, same phase-1 doc] |
| 2026-09-27 | Byte-level breakdown of the divergence | The eight sources D1-D8 (section 4) |
| 2026-09-28 | `rdtsc`-only counterfactual | Removing the `rdtsc` surplus leaves 97/97 red (run a9cf3263); aio-refusal has 106 vs 1,022 records [READ, https://github.com/rrnewton/reverie/issues/627] |
| 2026-09-27 / 09-28 | Parity reds across all backends (run a9cf3263 at hermit a9cf326342f9 on 2026-09-27; analysed 09-28, code read at 81ca8822) | 173 parity reds with 3 causes: LiteInst 97 (preload), KVM 75 (brk layout), SaBRe 1 (a pid-probe fallback). Guest stdout differed from ptrace in only 4 of 344 rows (2 cells x 2 attempts: LiteInst stat-metadata-identity and one KVM test) [READ, memory note] |
| 2026-09-29 | Guest experiment for https://github.com/rrnewton/reverie/pull/777: the constructor gets its own heap instead of the guest's | The 4 runtime `brk` calls disappeared and the guest's heap tail became identical to ptrace, **but the first divergence stayed at record 16**. This confirmed the heap is one source but not the first one [READ, PR 777 description] |

Earlier, looser work:
- **August parity numbers.** About 118/205 in the August sweeps compared only stdout and exit code, not the determinism log [READ, memory note].
- **e9patch, 2026-08-06.** A full determinism-log comparison for e9patch found the same class of divergence: 9 extra runtime syscalls and a relocated heap base [READ, `ai_docs/cross-backend-detlog-parity-sweep-20260806.md`].

---

## 6. Strategies tried, in order

| When | Strategy | What it achieved / why it stalled |
|---|---|---|
| 2026-07-21 | Roadmap | Bet on SaBRe (about 13x faster than ptrace on a getpid probe), time-box a DynamoRIO proof of concept; KVM was a 310-line skeleton. LiteInst was not yet in the plan |
| 07-24 | LiteInst prototype | An `LD_PRELOAD` runtime with its own patcher (`pun.rs`); a seccomp trap catches each site the first time it runs. On 07-26 it switched to the `liteinst2` patching core (https://github.com/rrnewton/reverie/pull/127). Judged on 08-01 to be a patching core, not a faithful port of the PLDI'17 LiteInst system |
| 07-26 / 07-30 | Two ways to host Detcore | In-guest Tool plus RPC (fast), and the ptrace-owned host hybrid (what ships) |
| 08-02 | Flagship in-guest attempt | 19/20 advanced tests (https://github.com/rrnewton/hermit/pull/1466, reverie 456b628); fast path 2.84-2.96x faster than ptrace (https://github.com/rrnewton/hermit/pull/1443). Stalled on multithreading (spinlocks held across RPC), exec, the vDSO clock leak, nested-fork deadlock, and no PMU timer |
| 08-04 | Performance attribution | In-guest fast path 583 ns per syscall vs ptrace 17,101 ns; in-guest with Detcore `--strict` 14.2 µs. The legacy host hybrid (over 450 µs per syscall) was labelled broken and retired; the in-guest path had replaced it |
| 08-04 | **Owner suspension** of LiteInst performance and compatibility work | "If a backend depends on a PTRACE ROUND TRIP on the syscall path it is NOT architecturally correct yet and NOT ready for optimization at all. ALL perf work on sabre/e9patch/liteinst is SUSPENDED until the ptracer is out of the syscall path." [READ, `ai_docs/patching-backends-ptrace-on-syscall-path-audit-20260804.md`] Plan: one shared in-guest Tool host for LiteInst, e9patch and SaBRe. Only the shared syscall driver landed; the e9patch lifecycle owner is still a stub |
| 08-06 / 08-07 | Zero-ptrace cost gate | Per guest syscall: ptrace 34.13 µs, e9patch 34.31, SaBRe 39.88. Only DBT (DynamoRIO) passed, at 1.43 µs. LiteInst could not be measured: it failed its startup handshake, and on 08-07 made 9,430 ptrace calls for `/bin/true`. KVM hung |
| 08-06 | Activation regression | LiteInst 0/28 on the runner. The 220 older LiteInst scorecard rows turned out to be unverified |
| 08-17 to 08-26 | Self-determinism fixes | 30/336 cells enabled with an 8.6% failure rate, mostly one defect at turn 10 (the runtime reading `/proc/self/maps`). Trampoline inode fix (08-07): https://github.com/rrnewton/hermit/pull/1847, stack nondeterminism 302/412 to 0/412 |
| 09-14 to 09-26 | Host-hybrid recovery in reverie | Unpatchable syscalls through the Tool, kernel-mediated guest memory, re-activation after exec, the loader artifact, trap and relocation fixes, timer-signal discard |
| 09-17 | Qualification | 96 cells promoted (50 to 146 selected), each with 10 strict same-backend repeats |
| 09-27 | **Owner redirect for this lane** | Ratchet toward 100% determinism and 100% parity on ptrace-green cells, with no exemptions or comparator changes, preferring a tracer-side / self-contained runtime. The lane then ran the census, the D1-D8 breakdown and the TrapOnly design, and landed P1 |
| 09-28 | Self-contained runtime design | Rejected as written; the tracer-placed no-libc core survives. P2 stack assembled; 173 reds = 3 causes |
| 09-29 | Constructor heap experiment (https://github.com/rrnewton/reverie/pull/777) | Partial: heap tail fixed, record 16 remains. P2 approved at 5139c80a |

**Other backends** (state from August documents and not re-verified at today's refs):
- e9patch's in-guest path is dormant, so it is fully ptrace-serviced.
- SaBRe keeps a ptrace safety net, which costs 2 stops per syscall.
- DBT was the only backend to pass the cost gate.

**KVM today** [MEASURED]: it runs, with 75 parity reds in run a9cf3263 (2026-09-27) and 76 in s22 at hermit 356dfd3e, attributed to its brk layout.

---

## 7. What happens next, and what needs an owner decision

**Next on my lane, in order:**
1. Finish the validations that are running or queued: https://github.com/rrnewton/reverie/pull/649 (rerun queued), https://github.com/rrnewton/hermit/pull/3261, https://github.com/rrnewton/hermit/pull/3255, https://github.com/rrnewton/reverie/pull/680.
2. Push the https://github.com/rrnewton/dev-hermit/issues/414 trust-root rotation once parity-green's commits land and the coordinator OKs the exact commit.
3. Land the correctness PRs, then the combined Hermit pin bump. That bump carries TrapOnly P1 into Hermit.
4. Resume the F4 restart fix (5 cells plus perl): find why the tracer single-steps the spin loop after a timer step window, then review and land it.
5. Build the timer rules and P2e gates (https://github.com/rrnewton/reverie/issues/776), then Hermit P3, then the first real TrapOnly parity measurement.
6. Refresh `scorecard.md` after each new qualifying validation.

**Decisions only the owner can make:**
- Whether https://github.com/rrnewton/hermit/pull/3254 (162 cells) may land on a red main, or must wait for a green one.
- The six cells held on design grounds.
- Which residual guest-visible state TrapOnly may leave. TrapOnly's residual is the 2 patched bytes per site and the dirty-page fields in `smaps`/`pagemap` (P2-SPEC item O5); 0 of the 339 program-backed ptrace-green rows read these. (Phase-1 item (e), about an extra mapping entry or a SIGTRAP disposition, applies to the preload route.)
- Whether the ptrace-free goal (an in-guest hook) is still the target after parity. Every current route keeps a ptrace stop per syscall.

## Appendix: open questions found while writing this

- Hermit's `prepare_backend_config` says LiteInst "re-invokes the Tool callback on ERESTARTSYS" (hermit `hermit-cli/src/lib.rs:2438-2443`). At Hermit's current pin the lane measured the opposite: the raw -512 reaches the guest (F4). The comment is wrong until the F4 restart fix lands and is pinned.
- The tracer-side design's baseline recorded LiteInst virtual time differing between two invocations (32,423,150 vs 32,435,620 ns) while ptrace was stable. That would be a LiteInst self-determinism defect separate from parity. It needs a controlled re-measurement before it is filed.
- The census universe (215 cells at 19553a64) is not the same set as s22's group C (221 cells at 356dfd3e, including 13 later-joined cells: 12 from the DBT parity matrix, plus io-uring-fallback|custom). The exact number of s22 cells the 162 cover has not been computed, which is why 308/369 is an upper bound.
