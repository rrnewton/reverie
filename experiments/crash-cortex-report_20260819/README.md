# Reproducing Crash Cortex kernel bugs under Hermit — investigation report

**Written 2026-08-19.** Assumes no prior context.

This report is written from the task notes of nine tasks and the artifact
directories they produced. No experiments were run to write it; every number
below is quoted from the run that produced it and attributed to that run.

Landing state was re-verified today rather than quoted: `origin/main` in
`rrnewton/hermit` is `770b95c505fa2794e2937b9245bb72b5f10ec340`, and every
change this investigation produced is on a pushed branch, none of it merged.
Details in the last section.

## Summary

The goal was to reproduce real Linux kernel bugs by running Linux under QEMU
under Hermit, and using Hermit's deterministic scheduler to steer which order the
guest kernel's threads ran in.

- **The inspection half works.** Linux under Hermit can be stopped at a chosen,
  reproducible point, read live with `drgn`, and resumed, and the pause costs the
  guest under ~50 µs per 60 seconds held.
- **The search half does not.** Every chaos seed produces the identical guest
  interleaving — `switches=1` across seeds 1, 2, 3, 7 and 999, where the same
  detector run natively gives 1249–1456. Reproducible, but with one point in the
  space, so there is nothing to search.
- **The reason is not Hermit's scheduler.** Hermit alternates between QEMU's two
  vCPU host threads 42 times in a run whose guest sees 1 switch. The guest kernel
  cannot act on that, because **it receives about 4 timer interrupts per 229
  seconds of its own uptime**, against ~1,029 per second natively. Without the
  scheduler tick it never preempts a running task.
- **The obvious cause was fixed and was not the cause.** Hermit's two clock
  domains (`rdtsc` per-thread versus `clock_gettime` summed across threads) were
  a real defect — an 8-thread rate disagreement of 21.9x, up to 17.05 seconds of
  backwards guest time — and are now coherent at 1.001x in PR #2312. The guest
  ticks did **not** return. Two defects, not one.
- **No shortlisted bug was ever attempted**, deliberately: a seed search over a
  one-point space cannot find a race, and any apparent hit could not be
  attributed to steering.

## 1. What Crash Cortex is, and why reproducing its bugs under Hermit looked attractive

Crash Cortex is an automated kernel-crash investigation pipeline. When a machine
in the fleet crashes, it takes the vmcore, investigates it with an agent, and
posts a report to the Workplace group "Kernel AI Investigations"
(`4508446156040829`). Each report has a fixed four-section shape — Summary,
Detailed Analysis, Affected Functions, Recommendations — plus a link to a
Phabricator paste holding the full report and often a proposed kernel patch.

Hermit is this project's deterministic container for Linux userspace. It runs a
guest program under the Reverie instrumentation layer, intercepts syscalls,
`rdtsc`, CPUID and PMU preemption events, and replaces the nondeterministic ones
with values it controls. Its scheduler serializes guest threads onto one logical
CPU and picks the next runnable thread deterministically, so the same program
run twice takes the same schedule. Its `--chaos` mode uses that same machinery in
reverse: instead of one canonical schedule, it draws a randomized schedule from a
seed, so that running many seeds explores many thread interleavings.

The attraction was direct. Crash Cortex reports races. A race is a bug that
depends on which order two threads happened to run in. Chaos-mode Hermit is a
machine for choosing that order. If a Crash Cortex bug could be made to happen
inside a Linux kernel running under QEMU, and QEMU itself were running under
Hermit, then Hermit's control over QEMU's host threads would translate into
control over the guest kernel's thread interleaving. Two things would follow:

- **Search.** Sweep chaos seeds until one produces the crash. Because a seed
  names a schedule, the hit is then reproducible on demand rather than
  1-in-3715-boots.
- **Inspection.** Stop the guest at the moment of interest and read live kernel
  state with `drgn`, the kernel debugger this project already uses, without the
  act of stopping changing what the guest does next.

This is the split the rest of the report uses. **The inspection half works. The
search half does not.** The reason is not where anyone expected it to be, and
finding out where it actually is consumed most of the investigation.

## 2. What was attempted

The work ran as nine tasks over 2026-08-18 and 2026-08-19. In dependency order:

| task | question it asked | outcome |
| --- | --- | --- |
| `cc-triage-reports` | Which reported crashes are even candidates? | 246 reports triaged; 13 genuine race families; a ranked shortlist |
| `cc-exec-control` | Can we stop Linux under Hermit mid-run and read it with `drgn`? | **Yes**, at a chosen reproducible stop point. Required fixing two defects |
| `cc-guest-interleaving-steerable` | Does Hermit's chaos reach the guest kernel's interleaving? | No, in the demos' mode. Its headline reason was later contradicted |
| `cc-qemu-coherent-timebase` | Is Hermit's split time base what blocks the steerable configuration? | The split is real and measured; it is not what blocks booting |
| `chaos-wedges-noicount-boot` | Why does `--chaos` wedge the steerable boot? | It does not wedge it. ~6–7x slower with a long tail |
| `chaos-fixed-seed-reproducibility` | Does a fixed chaos seed name a schedule, or does PMU skid move it? | It names one. 21 recordings, byte-identical |
| `cc-repro-methodology` | Reproduce a shortlisted bug | **Stopped before picking a candidate.** Every seed gives one interleaving |
| `does-sequentialization-serialize-guest-vcpus` | Is that a structural property of Hermit's scheduler? | No. The guest gets almost no timer interrupts |
| `cc-coherent-timebase-implement` | Fix the time base; does interleaving return? | Time base fixed and coherent. Interleaving did **not** return |

### The three configurations that matter

Almost every disagreement in this investigation turns out to be two people
measuring different rows of this table, so it is worth fixing the vocabulary up
front. All three run QEMU with TCG (software emulation of the guest CPU),
because that is the only mode where the guest executes inside QEMU's userspace
where Hermit can see it.

| name used below | flags | what it gives |
| --- | --- | --- |
| **compatibility profile** | `--no-sequentialize-threads --max-timeslice disabled` | QEMU's threads run concurrently. Fast, but not deterministic and not steerable |
| **strict profile** | `--strict` | Hermit's determinism. One thread runs at a time |
| **`-icount`** | a QEMU flag, not a Hermit one | QEMU counts guest instructions for a deterministic guest clock |

Two constraints bind these together, and both were established by measurement
rather than from documentation:

1. **`-icount` and steerability are mutually exclusive, and that is QEMU's rule,
   not Hermit's.** With `-icount` and `-smp 2`, QMP `query-cpus-fast` reports both
   guest CPUs on host thread-id `1930720` — one thread for two vCPUs. Without it,
   thread-ids `1931121` and `1931122` — two real threads. Asking QEMU for both at
   once fails outright in 0.1 s: `qemu-system-x86_64: -accel tcg,thread=multi: No
   MTTCG when icount is enabled`. If both guest CPUs share one host thread, there
   is no host-thread scheduling decision for Hermit to make.
2. **`--chaos` silently forces the strict profile.** `detcore-model/src/config.rs:624-626`
   sets `sequentialize_threads = true` whenever `chaos` is set, with no warning.
   So "compatibility profile under `--chaos`" is not a configuration that exists,
   and a plan built on the compatibility profile's timings does not survive
   adding chaos. Measured at 1 vCPU, no `-icount`, no guest workarounds:
   `--no-sequentialize-threads --max-timeslice disabled` boots in 19.0 s and
   26,627 console bytes; the identical flags plus `--chaos --seed 1` produce
   **0 bytes in 200 s** (`experiments/cc-repro_20260818/README.md`).

## 3. What works today

### 3.1 A chosen, reproducible stop point in a running Linux guest, with `drgn` reading kernel state there

This is the inspection half, and it is a working capability on the real target:
a Linux kernel booted under QEMU, with QEMU running under `hermit run
--gdbserver` in Hermit's **default** mode. Driver:
`experiments/drgn-exec-control_20260818/session.py`.

The stop point is a breakpoint on `writev`. That choice came from a syscall
profile of QEMU (`strace -f -c`, 30 s, standalone restore of the demo-5
snapshot):

| syscall | calls in 30 s |
| --- | --- |
| `futex` | 2,950,650 |
| `write` | 1,437,920 |
| `read` | 482,895 |
| `ppoll` | 482,793 |
| `writev` | **243** |

The serial log over that window was exactly 243 bytes. QEMU emits guest console
output one byte per `writev` and uses `writev` for nothing else here, so
"break on `writev`, continue N−1 times" means "stop the instant the guest kernel
emits its Nth console byte" — a semantically meaningful point in the guest's own
execution, at 243 round trips rather than `write`'s 1.4 million.

Four independent sessions, two at each of two stop points:

| stop point | tasks in guest | tasks named `sleep` | state digest | runs |
| --- | --- | --- | --- | --- |
| 1st guest console byte | 83 | 0 | `2b2855580df35cf6` | 2 |
| 100th guest console byte | 85 | 2 | `441f64d5e27fa3d7` | 2 |

Two runs at the same index give byte-identical transcripts once the QEMU pid,
wall timings and port are normalized out. A different index selects a different
and meaningful state: byte 1 lands before the guest command has forked its two
`sleep` processes, byte 100 after. An earlier driver in the same task
(`gdbserver_hangs_after_a`) reproduced the same structure with its own digest
scheme — N=1 → 83 tasks, digest `81779c19a80f9b5f`, 6,919 physical reads; N=100
→ 85 tasks, digest `18b058891bccacc5`, 6,999 reads, each twice — so the result
holds across two independently written harnesses.

At the stop, `drgn` walks `task_struct.tasks` live out of `/proc/<qemu-pid>/mem`
and reports per-task pid, `comm` and decoded state (e.g. *"by state: I (idle
kthread) x58, R (running) x2, S (sleeping) x19"*). That costs 7,392 physical
reads / 23,043,630 bytes. QEMU sits in ptrace-stop `t` throughout and emits zero
guest console bytes during the read, so the guest executes nothing while being
inspected. The whole loop, including snapshot restore, is 3.3–3.8 s per session.

**The pause is free, and the bound is measured rather than asserted.** The
observable is the guest kernel's own `printk` timestamps at 1 µs resolution,
from two `/dev/kmsg` writes bracketing the pause — the kernel's clock, not a
userspace read of it. Three repetitions per configuration
(`experiments/drgn-exec-control_20260818/`, `pause-midrun.py`):

| configuration | wall clock held | guest seconds across the pause |
| --- | --- | --- |
| no pause | — | 0.002797 / 0.002772 / 0.002796 |
| pause, 0.03 s held | 0.03 s | 0.002788 / 0.002796 / 0.002797 |
| pause, 60.03 s held | 60.03 s | 0.002824 / 0.002789 / 0.002797 |

All nine values lie in [0.002772, 0.002824]: total spread 52 µs, and the same
spread appears in the unpaused baseline. Baseline mean 2788 µs versus 60-second
dwell mean 2803 µs is a 15 µs difference, smaller than the baseline's own 25 µs
spread. So 60,000,000 µs of wall clock held with the guest stopped cost the
guest under ~50 µs of its own time — under one part per million, and not
distinguishable from zero at this resolution.

**Three things this capability is not**, stated because each is an easy
overclaim:

- It is **not full-run bitwise determinism.** The whole-serial-log digest still
  varies run to run. What is reproducible is the guest state *at the chosen stop*.
- "Free" is a **measurement bound**, not a proof of zero: under ~50 µs per 60 s
  pause, not exactly 0.
- It is **not a unified `drgn` control plane.** This is `gdb` driving control and
  `drgn` driving inspection, glued together. See §7 for what would change that.

### 3.2 A 2-vCPU snapshot/resume harness that runs a workload under arbitrary Hermit flags in ~25 s

`experiments/cc-repro_20260818/cc_qemu.py` is a productionised copy of
`demos/lib/qemu_controller.py` and `demos/lib/qemu-snapshot.sh`. It exists as a
copy, not a parameterisation, because the demos hardcode `-smp 1` and `-icount
shift=0,sleep=off` and this needs the opposite of both; the owner's ruling is
that the demos stay clean, and no demo file was touched at any point in this
investigation.

It matters because a *boot* under the steerable configuration is not affordable:
at 2 vCPUs with no `-icount`, a full boot under `--chaos` never completed in
7200 s across six attempts. So the harness boots once **without** chaos, takes a
QEMU `savevm` snapshot, and then resumes **with** chaos to run only the workload.
Measured per-run costs — these are the numbers a parallel sweep would be planned
on:

| step | cost |
| --- | --- |
| boot + `savevm`, 2 vCPU, no `-icount`, native | 8–11 s, **one time**, shared by every run |
| resume + run workload, native | 3.1–3.3 s |
| resume + run workload, `hermit --strict`, no chaos | 15.8–18.1 s |
| resume + run workload, `hermit --strict --chaos --seed 1` | 17.6 s / 17.7 s (two runs) |
| resume + run workload, `--chaos --seed 999` | 349.3 s |
| resume + run workload, `--chaos --seed 7` | 229.0 s |

The median chaos run is roughly 25 s; the tail is the planning risk, and §4.3
explains where it comes from.

Two hazards a sweep must design around were found the hard way and are fixed or
documented in the harness:

- **A qcow2 image takes an exclusive write lock.** Launching 7 resumes against
  one `snap.qcow2` produced seven empty result files and no obvious error until
  a stderr was read: `Failed to get 'write' lock`. The sweep did not run slowly,
  it ran zero-wide and looked like a timeout. The fix is three lines: copy the
  snapshot per run with `cp --reflink=auto`. Eight orphaned QEMU processes from
  those failed runs kept holding locks afterwards, so a sweep must also reap its
  own children.
- **Three guest kernel command-line flags are currently required**:
  `no_timer_check`, `lpj=999964`, `tsc=nowatchdog`. All three suppress a guest
  check that compares the TSC against a `clock_gettime`-derived clock. See §6.

### 3.3 A triaged corpus: which reported crashes are candidates at all

`experiments/crash_cortex/` (branch `cc-triage-reports_20260818`, commit
`3193a7d6`): `reports.csv` with 246 rows and 24 columns, `shortlist.csv` with 13
ranked candidate families plus 3 named anti-candidates, and the scripts that
regenerate both.

The group holds 251 posts, of which 247 are CrashCortex-authored and 246 are
crash reports, spanning 2026-03-06 to 2026-08-16. They carry 193 distinct machine
ids, so roughly 46 posts are re-investigations of a signature already reported.
Deduplicated by defect rather than by machine, they collapse to **33 defect
families**. 169 of the 246 rows were classified by hand-reading the report's own
summary; the 77 rule-classified rows are all ones the report itself calls a
driver bug or a hardware fault. A pure-regex pass over the full analysis text was
tried and discarded: these reports enumerate and then *eliminate* hardware and
race theories, so negated mentions ("no MCE/ECC", "T4 hardware fault:
eliminated") read as positives and misfiled 48 reports.

Mechanism split over the 246:

| mechanism | reports |
| --- | --- |
| driver-specific | 73 |
| host-environment (hypervisor) | 43 |
| **timing-dependent** | **43** |
| input-or-config | 20 |
| contention-at-scale | 19 |
| missing-yield | 18 |
| memory-pressure | 15 |
| hardware-specific | 11 |
| not-a-bug | 4 |

Four families account for 91 of the 246 and **none is a Hermit target**: 42
reports of one bug where `csd_lock_wait_toolong` emits `pr_alert` + `dump_stack`
synchronously into a 115200-baud serial console from inside an IPI busy-wait, so
a host-side vCPU deschedule becomes a 20–30 s stall (one report names 4124
hosts); 18 of one in-kernel loop with no `cond_resched` on `PREEMPT_NONE`; 16 of
unfair `virt_spin_lock` starvation with no `CONFIG_PARAVIRT_SPINLOCKS`; 15 of the
btrfs qgroup `GFP_ATOMIC` `BUG_ON`, which needs an allocation to fail, not an
interleaving. **The genuine race pool is 13 families, not 247 reports.**

Two findings from the triage bear directly on the plan:

- **No reproducer in the corpus has ever been run to a crash.** Of 246 reports,
  177 full-report pastes carry a `### REPRODUCER` section; 42 of those say N/A
  and 135 give something, but not one was run to a crash in a lab. Zero reports
  reproduced a panic in a VM. Exactly 1 of 248 pastes contains inline C source;
  everything else is prose or a path on an investigating agent's ephemeral
  sandbox. Two systemic blockers recur verbatim across reports: a blocked
  `mkosi-kernel` build, and "no compiler available on this devvm". So a
  reproduction under Hermit would be the *first* reproduction, not a port of one.
- **The obvious candidate is the wrong one, and is named as such.**
  `perf-event-exit-quadratic` (paste `P2404636945`) is the only report in the
  corpus with a real QEMU A/B and real numbers — 192x at 20,000 events; the
  unpatched kernel could not finish 40,000. That makes it the tempting pick and
  the worst one: it reproduces deterministically once the event count is large
  enough, so reproducing it under Hermit would demonstrate nothing about
  deterministic scheduling. `shortlist.csv` names it, `bpf-arena-64k-misalign`
  and `fuse-zero-size-ptr` as anti-candidates so they are not picked up by
  accident.

The rank-1 candidate is `scx-tasks-lock-AA`, 12 independent reports:
`scx_enable()`'s class-switch loop holds `scx_tasks_lock` and calls
`put_task_struct(p)`; when that drops the last reference of a concurrently
exiting task, `__put_task_struct → sched_ext_free` re-acquires the held lock.
One lock, one recursive acquire, one file. It is the pick because its rarity is
*measured*: paste `P2380192538` records 37 days of uptime and 3,715
scheduler-enable cycles for one hit, and states it was never reproduced in a VM
across 5 fleet instances. Its oracle is crisp — `drgn` read `scx_tasks_lock` at
`0x101` (a pure self-deadlock) on the real vmcore, so a `drgn` probe can score a
schedule without waiting for a panic.

### 3.4 A fixed chaos seed does name a schedule

This is the property the whole search idea depends on, and it was challenged and
then confirmed. Chaos timeslices are counted in retired conditional branches
(RCBs), and PMU counter delivery on this project is known to skid; if skid
reached the schedule, a seed would not name a fixed point and seed search would
be searching a space whose points move.

The falsification condition was stated before any run: if repeated runs at one
fixed seed produce byte-identical `--record-preemptions-to` recordings, the
hypothesis is wrong. Result — three full QEMU boots at chaos seed 1, 1 vCPU,
`-accel tcg,thread=single`, the exact workload where the anomaly was seen:

```
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qfull-s1-r1.json
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qfull-s1-r2.json
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qfull-s1-r3.json
```

583,561,705 bytes each, byte-identical. The comparison is not shallow: that hash
covers 6,448 preemption points across 3 threads, each carrying the **realized**
retired-branch count at that point, and a global trace of **5,564,357 scheduler
events**. A single skidded branch count would have broken the hash. Zero
`HERMIT_SKID_OVERSHOOT` markers appeared in any run.

Total evidence: **21 recordings at chaos seed 1, three workloads, host load
average spanning 12 to 220, one hash per workload, zero differences** — including
two full QEMU boots run while the 316-CPU host carried ~180 synthetic spinners
(load average 219.79 / 220.90 / 195.73), whose recordings are byte-identical to
the low-load ones. Load dependence was the specific claim in the hypothesis and
it does not appear.

The mechanism, from source rather than from the absence of a difference:
`reverie/reverie-ptrace/src/timer.rs:15-17` states the contract outright — the
performance-counter interrupt is deliberately scheduled *early* by a per-processor
skid margin (default 1000 RCBs, `timer.rs:155`) and Hermit then single-steps to
the exact target branch count. Skid is absorbed before it can reach the schedule.

One honest gap: the QEMU recordings are all at 1 vCPU with
`-accel tcg,thread=single`, because 2-vCPU chaos boots do not complete within any
affordable budget and a recording is only written when the guest exits normally
(`--stop-after-turn` exits rc=1 and writes nothing; a SIGTERM-terminated run also
writes nothing).

## 4. What does not work, and why

### 4.1 Every chaos seed produces the same guest interleaving

This is the gate the whole search idea rests on, and it fails. Measured on the
2-vCPU MTTCG no-`icount` snapshot from §3.2, guest cmdline `tsc=nowatchdog
no_timer_check`.

The first detector was a lost-update counter: two guest processes pinned to the
two guest vCPUs hammer an unsynchronised shared counter 5,000,000 times each,
and lost increments measure overlap.

| configuration | lost increments |
| --- | --- |
| native, no Hermit | 3,173,663 (varies run to run) |
| `hermit --strict`, no chaos | 23,631 |
| `hermit --chaos --seed 1` | 0 |
| `hermit --chaos --seed 1` (repeat) | 0 |
| `hermit --chaos --seed 999` | 0 |

`lost = 0` is also what an insensitive instrument returns, so this was not
treated as an answer. The detector was replaced with one that records the
**order** of guest events: the two pinned processes each append their own id to a
shared trace via an atomic fetch-and-add, 1000 entries each. The trace *is* the
interleaving, and `switches` counts alternations. This registers fine-grained
interleaving even when the two never overlap in wall-clock terms.

| configuration | switches | wall |
| --- | --- | --- |
| native, no Hermit | 1456 | — |
| native, no Hermit | 1249 | — |
| `hermit --strict`, no chaos | 3 | — |
| `hermit --strict`, no chaos | 1 | 15.8 s |
| `hermit --chaos --seed 1` | 1 | 26.1 s |
| `hermit --chaos --seed 1` (repeat) | 1 | 26.3 s |
| `hermit --chaos --seed 2` | 1 | 39.6 s |
| `hermit --chaos --seed 3` | 1 | 48.7 s |
| `hermit --chaos --seed 7` | 1 | 229.0 s |
| `hermit --chaos --seed 999` | 1 | 21.6 s |

Every seed gives `switches=1` and the identical head pattern: one guest process
runs all 1000 of its iterations, then the other runs all of its. Native gives
1249–1456 alternations and varies between runs. **Two chaos seeds do not produce
different guest interleavings; they produce the same one, at wall costs spanning
15.8 s to 229.0 s.** That is reproducibility without variation — precisely the
useless case for finding races. A 316-way sweep over that configuration is 316
identical runs.

The obvious lever does not work either. If the problem were Hermit preempting
QEMU's vCPU threads too coarsely, a finer timeslice should buy interleaving:

| `--max-timeslice` | switches | wall |
| --- | --- | --- |
| 200,000,000 ns (200 ms, default) | 1 | 26.2 s |
| 20,000,000 ns (20 ms) | 1 | 185.6 s |
| 2,000,000 ns (2 ms) | — | no output within 700 s |

A 10x finer preemption budget bought zero additional guest interleaving at 7x the
wall cost, and 100x finer produced nothing at all.

Control, because this is a negative claim: the guest really does have two CPUs
under Hermit — the detector reports `cpus=2 online=0-1` both natively and under
`hermit --chaos --seed 1`, and `sched_setaffinity` succeeds (`AFF rc=0
parent_cpu=0`). This is not a one-CPU guest trivially explaining `switches=1`.

Because of this result, **no shortlisted candidate was ever attempted.** Every
candidate on the shortlist is an SMP race needing two or more guest CPUs; a seed
search over a space with one point in it cannot find one, and any apparent hit
could not be attributed to steering.

### 4.2 The reason is not Hermit's scheduler — the guest receives almost no timer interrupts

The natural explanation for §4.1 is structural: Hermit's determinism comes from
sequentializing host threads, QEMU's two guest vCPUs *are* two host threads, so
sequentializing them serializes the guest. If that were true, chaos over QEMU's
threads would be the wrong mechanism outright and the approach would be dead.

**It is not true, and there are three independent lines of evidence.**

**(a) Hermit alternates between the two vCPU threads 42 times in a run whose guest
sees 1 switch.** Read from Hermit's own scheduler record, not inferred: a resume
under `hermit --log info --strict --chaos --seed 1` produced a 379,326,519-byte
info log; each `COMMIT` record is one scheduler turn, so the sequence of thread
ids across COMMIT records is the realized host-thread schedule.

| quantity | value |
| --- | --- |
| total scheduler turns | 14,245 |
| turns per thread | dtid 3 = 9,510; dtid 7 = 3,300; dtid 9 = 1,266; dtid 11 = 108; dtid 13 = 42; dtid 5 = 19 |
| total host-thread switches | 8,952 |

The two vCPU threads are identified by final retired-branch count, since a thread
executing translated guest code retires vastly more conditional branches than a
helper: dtid 11 at 852,203,454 RCBs and dtid 13 at 170,665,967 RCBs are the
vCPUs; dtid 3 at 37,463,390 RCBs is QEMU's main loop (52,520 `clock_gettime`,
5,913 `ppoll`, 3,708 `read`); dtids 5, 7 and 9 retire 0 RCBs and are helpers.
Restricting the COMMIT sequence to the two vCPU threads gives 150 turns and
**42 alternations between them**, in a run whose guest-visible outcome was
`switches=1`. The mechanism demonstrably produces guest-CPU interleaving.

**(b) Removing host-thread alternation from the experiment does not change the
outcome.** Pinning *both* guest processes to the *same* guest CPU means any
interleaving must come from the guest kernel timeslicing them, with Hermit's
vCPU alternation out of the picture:

| configuration | switches |
| --- | --- |
| native, both processes on guest CPU0 | 3 |
| Hermit, both processes on guest CPU0 | 1 |
| native, one process per guest CPU | 1080–1522 |
| Hermit, one process per guest CPU | 1 |

The same-CPU row is what redirected the investigation. If Hermit's
sequentialization of vCPU threads were the cause, the same-CPU case would be
unaffected. It is not unaffected, so the cause lies below that.

**(c) The guest takes essentially no local timer interrupts.** Measured by
reading `/proc/interrupts` inside the guest immediately before and after the
workload, same snapshot, same command, only Hermit added. The 20,000-slot
workload (10x the previous one):

| | native | `hermit --strict --chaos --seed 1` |
| --- | --- | --- |
| LOC before (CPU0 / CPU1) | 1843 / 1553 | 1776 / 1485 |
| LOC after (CPU0 / CPU1) | 2543 / 2223 | 1780 / 1490 |
| **timer interrupts during workload** | **+700 / +670** | **+4 / +5** |
| guest uptime across workload | 0.68 s | 229.19 s |
| implied tick rate | ~1,029 /s (HZ=1000, as expected) | ~0.017 /s |
| guest-visible switches | 19,370 | 2 |

**Over 229 seconds of the guest's own elapsed time the guest took four local
timer interrupts on CPU0 and five on CPU1. At the HZ=1000 the same kernel
achieves natively it should have taken roughly 229,000 — a deficit of about
57,000x.** Without the scheduler tick the guest kernel never preempts, so two
runnable processes cannot alternate no matter what Hermit does with QEMU's host
threads.

And the guest-visible interleaving tracks the *ticks*, not the work: the
2,000-slot workload got 0 ticks and 1 switch; the 20,000-slot workload got 4–5
ticks and 2 switches. Ten times the work bought one extra switch, because it
bought a handful of extra ticks. Natively the same tenfold increase took switches
from ~1,470 to 19,370, scaling with the work.

This also explains §4.1's dead lever. `--max-timeslice` governs how often
**Hermit** preempts **QEMU's host threads**. It has no bearing on how often
QEMU's emulated APIC timer interrupts the **guest**, and the guest kernel cannot
preempt a running task without that interrupt. The earlier note recording the
knob as ineffective was right about the observation and silent about the reason.

### 4.3 A cost that is real but is not the blocker: chaos makes busy-waits ~6–7x more expensive, with a long tail

Worth carrying because it dominates the wall-clock budget of any sweep, and
because it was initially mistaken for a wedge (see §5.3).

The mechanism reproduces without QEMU at all, in a 45-line C guest
(`experiments/chaos-wedge_20260818/tickwait.c`): one thread busy-waits for a
counter that a second thread bumps every 1 ms via `nanosleep`, 20 times. It
reports spins per delivered tick — how much wasted work the waiter does per event.

| configuration | spins per delivered tick | wall |
| --- | --- | --- |
| native, no Hermit | 47,782 | — |
| `hermit --strict`, **no chaos** | 19,993,386 | 10.37 s (3 reps identical) |
| same, `--max-timeslice=20000000` (20 ms) | 1,993,386 | 2.38 s |

That is a **418x amplification with no chaos involved**, and the number is not
arbitrary: 19,993,386 ≈ `max_timeslice / NANOS_PER_RCB` = 200,000,000 / 10 =
20,000,000. A thread busy-waiting on another thread runs a *full preemption
timeslice* before the scheduler hands over. This is Hermit's documented
preemption of last resort (`detcore/src/tool_local.rs:2041-2043`) working as
designed, and it is the dominant cost in the non-chaos baseline too. Ten times
smaller slice, exactly ten times fewer spins.

What chaos adds is a heavy tail, not a deadlock. In chaos the slice is not the
fixed budget but a draw from an **unbounded exponential** with that budget as its
mean (`detcore/src/tool_local.rs:2070-2076`: `lambda = 1/target_timeout_rcbs;
rcbs = 1 + Exp(lambda).sample()`). Eleven seeds on the same 20-tick guest:

| seed | wall | spins/tick | vs. non-chaos |
| --- | --- | --- | --- |
| 4 | 6.14 s | 10,200,393 | 0.51x |
| 5 | 6.24 s | 10,685,272 | 0.53x |
| 3 | 7.87 s | 13,807,792 | 0.69x |
| 2 | 8.42 s | 14,376,405 | 0.72x |
| 1 | 8.47 s | 15,205,491 | 0.76x |
| 6 | 8.49 s | 15,418,699 | 0.77x |
| 42 | 9.17 s | 16,076,456 | 0.80x |
| 999 | 16.46 s | 29,183,673 | 1.46x |
| 11 | 30.41 s | 55,369,503 | 2.77x |
| 8 | 38.24 s | 69,640,654 | 3.48x |
| 7 | 235.21 s | 415,373,102 | 20.78x |

A 41x spread in spins/tick and a 38x spread in wall time across eleven seeds,
from 2x *better* than the fixed slice to 21x worse. The same distribution shows
through the full QEMU boot: at 1 vCPU, no `-icount`, non-chaos reaches the
`HERMIT-QEMU-BUSYBOX-PASS` marker in about 190 s; chaos seed 1 in ~1190 s
(guest time 15.284575), seed 3 in ~1270 s, seed 999 in ~1340 s, and seed 2 was
still unfinished at a 3000 s budget with guest time 0.063834.

The one candidate fix — bounding that exponential draw, about 2–3 lines at
`tool_local.rs:2070-2076`, or roughly 3 files and 25 lines with a tunable knob
and a test — **was measured, costed, and deliberately not made**, for three
reasons recorded at the time: (1) the long draws are exactly the schedules where
one thread runs far ahead of another, which is the schedule shape chaos exists to
produce, so truncating the tail trades race-finding power for wall time and
nobody has measured that cost; (2) it invalidates every recorded-preemption
artifact, since the draw sequence changes; (3) there is a zero-code mitigation
that is probably better — the schedule is reproducible per seed, so a campaign
can give each seed a wall budget, drop the seeds that blow it, and record how
many were dropped. `hermit/CLAUDE.md` makes this post-facto-review trigger 4,
"a core DetCore scheduling change ... especially race search", so it is a labeled
PR with dual adversarial review.

Tuning the knob in the other direction was tried and the prediction was wrong.
Full QEMU boot, chaos seed 1, 1 vCPU, no `-icount`:

| knob | result |
| --- | --- |
| `--max-timeslice=200000000` (200 ms, default) | PASS ~1190 s |
| `--max-timeslice=20000000` (20 ms) | PASS ~2320 s — **2x worse** |
| `--max-timeslice=2000000` (2 ms) | rc=137, **zero** console bytes after 3000 s |
| `--target-timeslice=2000000` (2 ms) | PASS ~1000 s — 1.2x better |
| no chaos | PASS ~190 s |

Shrinking the PMU preemption budget buys back the busy-wait region and loses far
more on the other ~99% of the boot, because every preemption costs a PMU trap
plus a scheduler RPC. `--target-timeslice` preempts at syscall boundaries
instead, which is nearly free for a thread issuing 46,637 syscalls per 200,000
log lines — but 1.2x is not a fix and was not presented as one.

## 5. The premises that were refuted along the way

Four premises about the approach were tested and did not survive. Two further
hypotheses the investigation raised *against itself* were also killed; they are
in §5.5 and §5.6 because a reader who only sees the first four will mistake the
shape of the evidence.

### 5.1 "The demos run under KVM, so the guest executes where Hermit cannot see it" — refuted

This was the scenario that would have killed the approach outright. If the guest
runs on the hardware via KVM, its interleaving happens in a place Hermit has no
visibility into.

It was settled from the live process rather than the command line, which
mattered: demo 7 pins `-accel tcg` explicitly, but demos 5 and 6 pass **no
`-accel` at all**. Measured via QMP `query-kvm` plus `/proc/<qemu>/fd` on a host
with working KVM (`/dev/kvm` openable, `kvm_amd` loaded, svm): `query-kvm` returns
`{enabled: false, present: true}` and there are **zero** `/dev/kvm` file
descriptors open. QEMU's built-in default is TCG; KVM must be asked for. Control
proving the probe can see KVM when it is there: `-accel kvm` on the same host
gives `query-kvm {enabled: true}`, a `kvm-nx-lpage-re` kernel thread appears, and
26,567 serial bytes arrive in 3 s versus 0 bytes in 6 s under TCG.

**All three demos run TCG. Guest execution is inside QEMU userspace, where
Hermit can see it.** This premise being false is what kept the approach alive.

### 5.2 "`-icount` is load-bearing for booting QEMU under Hermit, so the steerable configuration is unavailable" — refuted

This is the one that carries an explicit contradiction annotation on its task,
and it is the most instructive disagreement in the investigation. Both agents'
numbers reproduce. See §5.7 for exactly how far the reconciliation goes and where
it stops.

The refutation is the boot matrix, all at hermit `770b95c505`, release binary
sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5`, QEMU
10.1.2, guest kernel 6.17.13, busybox initramfs
(`experiments/qemu-coherent-timebase_20260818/`):

| profile | icount | vCPUs | rc | wall | result |
| --- | --- | --- | --- | --- | --- |
| compat | no | 1 | 0 | 19.3 s | PASS |
| compat | no | 2 (MTTCG) | 0 | 23.4 s | PASS |
| compat | yes | 1 | 0 | 31.8 s | PASS (**`-icount` is slower**) |
| strict | yes | 1 | 0 | 151.8 s | PASS |
| strict | yes | 2 | 1 | 0.1 s | QEMU refuses: `No MTTCG when icount is enabled` |
| strict | no | 1 | 0 | 59.4 s | guest **panic** in `check_timer()`, 9,233 bytes |
| strict | no | 1 + `no_timer_check` | 0 | 103.5 s | PASS, TSC calibrated 999.964 MHz, zero printk rewinds |
| strict | no | 2 + `no_timer_check` | 137 | 900.0 s | stalls at CPU#1 `calibrate_delay_direct()` |
| strict | no | 2 + `no_timer_check lpj=999964` | 0 | **382.0 s** | **PASS** |

The last row is the point: **a 2-vCPU, no-`icount`, strict-profile boot works
today on current `main` with no Hermit change at all.** It needed two guest
kernel command-line flags and nothing else. So `-icount` is not load-bearing for
booting. What `-icount` *is* load-bearing for is clock coherence (§6.1), and the
mechanical reason it destroys steerability is QEMU's own constraint, quoted in
the table above.

The `check_timer()` panic in the 1-vCPU strict no-`icount` row is a real and
distinct failure — `..MP-BIOS bug: 8254 timer not connected to IO-APIC` →
`Kernel panic - not syncing: IO-APIC + timer doesn't work!` at
`check_timer+0x225/0x390` — and it is **not** a stall and **not** a clock-rate
failure. Bypassing exactly that check with `no_timer_check` produces a clean boot
that calibrates a 999.964 MHz TSC, within 0.005% of the `-icount` run's
1000.012 MHz.

A related documentation claim was also refuted by measurement:
`hermit/docs/QEMU_BOOT.md` said `--no-virtualize-time --no-virtualize-metadata`
"calibrates normally and reaches the expected boot outcome". Measured, that
configuration boots 9,396 bytes and then panics in timer setup, preceded by
`tsc: Fast TSC calibration failed`. A correction is on hermit branch
`qemu-boot-doc-correction_20260818` @ `19d6d706ffc85da52bb7be56c39782cdbcc9c4fc`.

### 5.3 "`--chaos` wedges the 2-vCPU no-`icount` boot" — refuted by the agent who claimed it

The task title asserting the wedge was written by the same agent who then
disproved it, which is the right outcome and worth recording as such.

Given a 2400 s budget instead of 600 s, chaos seed 1 at 1 vCPU **completed**:
rc=0, 25,713 console bytes, `pass=1`, last guest time 15.284575 — same seed, same
flags, same binary that "wedged" at 600 s and again at 1800 s. It is not a
wedge; it is roughly 6–7x slower at the median with the long right tail
quantified in §4.3.

What made this look like a wedge is worth knowing, because the same trap is
available to anyone re-running it. The guest's `printk` timestamp only moves when
the guest prints, and inside `raid6_choose_gen()`'s calibration
(`lib/raid6/algos.c`, which busy-waits on `jiffies` with preemption disabled)
there are no messages for one algorithm's worth of work. A frozen printk
timestamp and a frozen console byte count look identical to a frozen guest.
Progress samples across the slow region show the truth — about 0.0166 s of guest
time per 240 wall seconds:

```
t=120   bytes=12643  guest_t=0.013919
t=360   bytes=12643  guest_t=0.013919
t=480   bytes=12693  guest_t=0.030500
t=720   bytes=12743  guest_t=0.047068
t=960   bytes=12857  guest_t=0.064919
t=1180  bytes=25713  guest_t=15.284575   <- PASS
```

Two intermediate diagnoses were also collected and refuted before the budget
turned out to be the answer, and both are recorded so nobody re-runs them:

- **Thread starvation from chaos's uniformly random thread priorities**
  (`detcore/src/tool_global.rs:2246`, with strict priority ordering at
  `detcore/src/scheduler/runqueue.rs:112-116` — a starvation-shaped design).
  Refuted by counting thread activity in the last 200,000 log lines of a wedged
  run versus the first 200,000: QEMU's main-loop thread dtid 3 gets **18,521**
  lines during the slow region versus **2,725** before it — six times *more*
  turns, not fewer.
- **QEMU kicking the TCG thread by signal, with chaos delaying delivery.**
  Refuted: `handling inbound signal` appears **zero** times in both a chaos and a
  non-chaos run of this configuration. QEMU's round-robin TCG kick is a
  shared-memory store, not a signal, so there is nothing for Hermit's signal path
  to delay.

It also is not a deadlock: during the slow region QEMU burns 5.4 CPU-seconds per
10 wall-seconds, steadily; a directly sampled 2-vCPU run consumed 17.08
CPU-seconds in 60 wall-seconds while its console did not change. And it is not
MTTCG-specific: the whole effect reproduces at **one** vCPU with
`-accel tcg,thread=single`, and then in a 45-line C guest with no emulator at all.

**Status of this task's own verify line, stated honestly at the time:** a 2-vCPU
no-`icount` boot completing under `--chaos` was **not demonstrated**. Best result
69,926 of 76,444 console bytes (91%) at a 7200 s budget; five further attempts ran
3120–5590 s more without completing. It completes at 1 vCPU for 3 of 4 seeds.

### 5.4 "Host-thread sequentialization inherently serializes guest vCPUs" — refuted

This is §4.2 and is not repeated here. Summary: Hermit alternated between QEMU's
two vCPU threads **42 times** in a run whose guest saw 1 switch, and the absence
of guest interleaving survives removing vCPU alternation from the experiment
entirely. Chaos over QEMU's host threads is not structurally the wrong mechanism.
This was explicitly *not* the design question for the owner that it was framed as
being.

### 5.5 A self-raised hypothesis, killed: "PMU skid means a fixed seed does not name a schedule"

Raised by the chaos-wedge investigation on the strength of an anomaly it could
not explain, and flagged as mattering well beyond that task. It is wrong; §3.4
has the evidence. The author's own note: *"My hypothesis was wrong and I am glad
it was."*

### 5.6 A self-raised prediction, falsified: "fix the time base and the guest ticks return"

The prediction was written into the task before the work, and stated as
falsifiable: *if a sane guest tick rate is restored, guest-visible interleaving
reappears and scales with workload length.* The time base was then fixed and
measured coherent. The prediction failed. §6 is that result.

### 5.7 Where two agents measured contradictory things, and how far it resolved

**The conflict.** One agent measured, on 2026-08-18, that QEMU under Hermit
without `-icount` "stalls at zero output": 0 bytes in 750 s at 2 vCPUs with
`--strict`, 0 bytes in 520 s with `--no-virtualize-time` and no strict, and 0
bytes in **360 s** at 1 vCPU with `--strict`. On that basis `-icount` was
declared load-bearing for booting at all. A second agent measured, the same day,
no-`icount` reaching PASS in 19.3 s at 1 vCPU and 23.4 s at 2 vCPUs, with
`-icount` *slower* at 31.8 s.

**The reconciliation, and what it does establish.** Two axes were tested
(`experiments/chaos-wedge_20260818/`, same binary sha256 `4d8e8924…`):

- *Profile.* This is the axis that changes the outcome. The compatibility profile
  passes in 17.9–19.3 s; the strict profile panics after 52.5–95.1 s. Neither the
  machine type (`q35 -cpu max` versus QEMU defaults) nor the kernel version
  (pinned 6.17.13 demo `bzImage` versus this host's 6.19.2 `/boot/vmlinuz`)
  changes pass-versus-panic. So the two agents were never measuring the same
  configuration: 19.3 s is a compatibility-profile number, and the zero-output
  observations are strict-profile ones.
- *Observation window.* Strict + no-`icount` + 1 vCPU emits **nothing** on the
  serial console for its first ~57 seconds. Timeouts of 15/30/45/48/52/56 s all
  return rc=137 with exactly 0 bytes; at 60 s, 8,186 bytes; at 90 s it runs to
  completion with 9,233 bytes and the `check_timer()` panic. Any run bounded at
  56 s or less reports exactly zero bytes. `hermit/docs/QEMU_BOOT.md` already
  warns that the strict profile "first emitted serial data after 85 seconds ...
  so use at least a 180-second bound"; that warning is easy to miss.

**Where the reconciliation stops, and I am not going to round this away.** The
observation-window axis explains a zero-byte report from a *short* window. The
first agent's 1-vCPU strict row ran **360 seconds**, six times the ~57 s
first-byte latency, and its 2-vCPU strict row ran 750 s. Those two rows are not
accounted for by either axis as stated. A third difference was never tested as a
variable: the zero-output runs restored a demo-5 **snapshot** under Hermit, while
the reconciliation booted a fresh kernel from `-kernel`/`-initrd`. That is a
plausible candidate and it is untested.

**Why it did not matter in the end.** The disputed claim was "`-icount` is
required to boot", and that question was settled decisively in the direction the
second agent measured, by a run neither original measurement covers: strict,
no-`icount`, 2 vCPUs, plus `no_timer_check lpj=999964` reaches PASS in **382.0 s**
on unmodified `main`. Independently, a second harness later reproduced that
figure at ~400 s wall with 76,444 console bytes. Both agents also agree to the
byte on the strict no-`icount` panic size — 9,233 bytes from one harness and
8,953/9,233 from the other's matrix rows. So the premise fell regardless, and the
residual disagreement is about how two specific timed-out runs behaved, not about
the conclusion.

### 5.8 Three smaller corrections the record should carry

- **"11 rdtsc events in a 10-second run"** was used early in the time-base task
  to argue that routing `rdtsc` through the coordinator would be free. An
  instrumented full boot (strict, no-`icount`, 1 vCPU, `no_timer_check`,
  `--log info`, 133,092,107-byte log, 109 s to PASS) counts **286,199** inbound
  `rdtsc` events — 286,187 from the TCG vCPU thread, 12 from the main loop — plus
  113,038 `clock_gettime` and 61,422 `gettimeofday`. Four orders of magnitude
  more. The cost conclusion survived, but for a different reason: 286,199 ×
  19.1 µs upper bound ≈ 5.5 s on a 109 s boot, about +5%. **Do not reuse the "11
  events" figure.**
- **"Two runs of chaos seed 1 produced identical consoles"**, asserted from equal
  byte counts at one sampling instant, was corrected by its own author to
  **prefix-identical**: `cmp -n 17033` shows every byte both runs produced is
  identical, but the runs later reached 17,033 versus 17,083 bytes. Byte-count
  equality at one instant is not evidence of determinism. The underlying
  determinism claim was subsequently established properly, by recording diff
  (§3.4).
- **The `gdb`-on-QEMU hang was first diagnosed as a Reverie core defect** in the
  gdbstub resume/stop plumbing (`vcont_resume` breaking out of its collect loop
  on a `NewTask` event). That code-level account was superseded by an
  instrumented trace, which located the hang inside `freeze_all` and traced it to
  a one-line omission in **hermit**, not Reverie. §7.2 has the real cause.

## 6. The single remaining blocker

**A Linux guest running under QEMU under Hermit receives about one timer
interrupt per QEMU main-loop timer service, instead of one per millisecond of
elapsed guest time. Measured: 4 interrupts on CPU0 and 5 on CPU1 across 229.19
seconds of the guest's own uptime, against ~1,029 per second natively. Without
the scheduler tick the guest kernel never preempts a running task, so no amount
of control over QEMU's host threads produces guest-kernel thread interleaving,
and there is nothing for a chaos seed search to search.**

Everything else on the path is either working (§3) or measured and understood
(§4.3, §5). This is the one thing that has to change.

### 6.1 What the two-clock defect was, and that it is now fixed

The obvious suspect was Hermit's split time base, and it took real work to
establish that it is a genuine defect *and* that it is not this blocker.

Hermit had two guest-visible clock domains that could not agree by construction:

- `handle_rdtsc_event` (`detcore/src/lib.rs:1210-1215`) returned the calling
  thread's **private** logical time, and carried a standing
  `TODO: use global time for rdtsc`.
- `clock_gettime`, `gettimeofday` and `time`
  (`detcore/src/syscalls/time.rs:96-107`) go through `thread_observe_time` →
  `GlobalTime::as_nanos()`, plus a process-tree shared monotonic floor.
  `detcore-model/src/time.rs:738-760` defines `GlobalTime.total` as
  `starting_nanos + the SUM over all threads of each thread's own duration +
  extra_time`.

One is a **sum over threads**; the other is **one addend**. Three structurally
distinct disagreements follow, all measured with
`experiments/qemu-coherent-timebase_20260818/twodomainN.c` (N threads, each
sample reads `rdtsc` then `clock_gettime` back to back, 40 samples/thread, every
thread doing identical work):

1. **Rate.** In the concurrent profile every thread's `rdtsc` span is
   bit-identical at 390,975 ns because `rdtsc` reports only the calling thread's
   own work, while `clock_gettime`'s span grows with concurrency. The ratio
   tracks the number of threads making progress: 1.000 at 1 thread, up to
   4.802 at 2, 15.890 at 4, **22.229 at 8**.
2. **Per-thread offset.** Each thread's `rdtsc` omits every other thread's
   contribution, and the omission differs per thread — 3,331,455 ns on thread 0
   versus 7,389,440 ns on thread 1 in a 2-thread strict run — so `rdtsc` jumps
   backwards at thread switches even under `--strict`, where the rate ratio is
   already 1.000. Measured: 1 backwards step of 79, by 901,335 ns, exactly at the
   thread boundary.
3. **Scheduler time.** `GlobalTime.extra_time` charges `NANOS_PER_SCHED` =
   500,000 ns per scheduling decision to the global clock and to no thread's
   local clock, which is why even a single-threaded strict run shows a
   3,331,435 ns standing offset between the domains.

This is visible in the guest's own console, because the guest kernel's `printk`
timestamps ride `sched_clock`, which rides the TSC once `tsc-early` is selected.
Counting backwards jumps per configuration
(`experiments/qemu-coherent-timebase_20260818/printk_rewind.py`):

| configuration | printk lines | backwards jumps | largest rewind |
| --- | --- | --- | --- |
| icount, 1 vCPU, strict | 357 | 0 | 0 ms |
| no-icount, 1 vCPU, strict + `no_timer_check` | 359 | 0 | 0 ms |
| no-icount, 2 vCPU, strict + `no_timer_check` | 154 | 1 | −15.000 ms |
| no-icount, 1 vCPU, compat | 370 | 1 | −15.978 ms |
| **no-icount, 2 vCPU, compat** | 960 | 4 | **−17,052.637 ms** |
| icount, 1 vCPU, compat | 357 | 0 | 0 ms |

Up to **17.05 seconds of backwards guest time** in the concurrent 2-vCPU run.
This is also what the three guest kernel workaround flags are hiding: all of
`no_timer_check`, `lpj=999964` and `tsc=nowatchdog` suppress a guest-side check
that compares the TSC against a `clock_gettime`-derived clock. Without
`tsc=nowatchdog`, a resumed 2-vCPU guest under Hermit spends its entire run in
TSC-skew detection and never reaches the workload at all — 425 seconds producing
only these lines:

```
[ 6.16] clocksource: timekeeping watchdog on CPU0: Marking clocksource 'tsc' as unstable because the skew is too large
[ 6.16] Clocksource 'tsc' skewed 57476130 ns (57 ms) over watchdog 'hpet' interval of 633375520 ns (633 ms)
[ 6.17] tsc: Marking TSC unstable due to clocksource watchdog
[10.66] clocksource: Checking clocksource tsc synchronization from CPU 1 to CPUs 0.
[15.77] smp: csd: Detected non-responsive CSD lock (#1) on CPU#1, waiting 5000013770 ns for CPU#00 do_sync_core
[26.99] nmi_backtrace_stall_check: CPU 0: NMIs are not reaching exc_nmi() handler
```

With the flag, the same resume completes in 15.8 s. So every measurement in §4.1
is taken on a guest that has been told not to look at its own clock.

**The fix is implemented and the domains are now coherent.** hermit branch
`cc-coherent-timebase`, base `origin/main 770b95c505fa2794e2937b9245bb72b5f10ec340`,
two commits: `26ccdf84be`, a cherry-pick of `7dfed8addf` "Keep SaBRe guest clocks
on coordinator time" (3 files, +10/−9 — finished work that had been sitting on a
branch with no pull request since 2026-08-17), and `fc1ed441792d0d80572085a4226b9121832d9906`,
the `rdtsc` half (4 files, +70/−8 including a new regression test). Both halves
replace the same expression, `thread_state().thread_logical_time.as_nanos()`,
which is why they were landed as one review rather than two.

Before/after, same probe, same host, old binary sha256 `4d8e8924…` versus new
`a1abce832d2c28b7e406f58b387e83d592f79792fc17c7af1ecc122e3180e978`:

| measurement | before | after |
| --- | --- | --- |
| strict, 2 threads: backwards `rdtsc` steps | 1 of 79 | **0 of 79** |
| strict, 2 threads: standing offset per thread | 3,332,775 ns / 7,392,040 ns | **10,010 ns on both** |
| concurrent, 8 threads: worst rate ratio | 21.946 | **1.001** |
| concurrent: backwards steps, 2 / 4 / 8 threads | 34/79, 90/159, 148/319 | 0/79, 13/159, 2/319 |

The residual 10,010 ns is one `clock_gettime` syscall charge
(`NANOS_PER_SYSCALL` = 10,000), which is correct: the probe reads `rdtsc` and
then `clock_gettime`, one syscall apart. The residual backwards steps are in the
concurrent profile only, which is not deterministic to begin with; the strict
profile is at zero. Tests at the branch head: `cargo test -p hermit-detcore
--test tests_time` 17 passed / 0 failed (including `rdtsc_deltas` and the new
`rdtsc_agrees_with_clock_gettime`, which samples eight times rather than once
because agreeing on a first read and drifting afterwards is the failure it exists
to catch); `-p hermit-detcore --lib` 415 passed; `-p hermit --lib` 121 passed.

**This is open as [PR #2312](https://github.com/rrnewton/hermit/pull/2312) and is
NOT merged** — re-verified 2026-08-19: `state: OPEN`, `mergedAt: null`, head
`fc1ed441792d0d80572085a4226b9121832d9906`, and `fc1ed441` is not an ancestor of
`origin/main`. It is labeled for post-facto human review under trigger 3, a new
determinization strategy, because the value returned for a guest-visible
instruction changes domain and therefore every recorded schedule, replay log and
comparison baseline containing an `rdtsc` value shifts.

### 6.2 Fixing it did not restore the ticks — the two defects are separate

The acceptance test was designed before the fix and run after it. Same snapshot,
same detector, same guest flags, only the Hermit binary changed:

| workload | binary | timer interrupts (CPU0 / CPU1) | guest uptime across workload | switches |
| --- | --- | --- | --- | --- |
| `order` (2,000 slots) | native, no Hermit | +77 / +56 | 0.05 s | **1471** |
| `order` | before the fix | +0 / +0 | 2.00 s | 1 |
| `order` | **after the fix** | +1 / +0 | 0.79 s | **1** |
| `order_long` (20,000 slots) | native, no Hermit | +700 / +670 | 0.68 s | **19,370** |
| `order_long` | before the fix | +4 / +5 | 229.19 s | 2 |
| `order_long` | **after the fix** | killed at the 800 s budget | — | **1** |

Ticks did not recover: 0 ticks per 2.00 guest-seconds became 1 tick per 0.79
guest-seconds, against ~1,029 per second natively — still about **800x short**.
Interleaving did not reappear: switches stayed at 1.

It would have been easy to read the partial movement as success. Guest uptime for
the same workload dropped from 2.00 s to 0.79 s, and that *is* the TSC no longer
racing, and it *is* the coherence working. The thing that was supposed to follow
from it did not follow.

The other stated expectation also failed. Strict, no `-icount`, 1 vCPU, no guest
flags, new binary: rc=0, 61.3 s wall, **9,235** console bytes, panic — character
for character the same `IO-APIC + timer doesn't work!` failure as before the fix
(9,233 bytes then, 9,235 now). All three guest kernel flags are still required
and `tsc=nowatchdog` is still hiding the 425-second failure mode.

A suspected regression was checked and cleared rather than asserted: the long
workload took 320.9 s before the fix and was still running at 800 s after it, but
running the **old** binary against the **same** snapshot gives wall 805.1 s,
`found=False`, the guest stuck at 56.72 s uptime with CSD lock #6 unresponsive
and no ORDER line at all — while the new binary at least produced the ORDER line.
Both fail on that snapshot generation; the 320.9 s figure came from a different
snapshot and was not a controlled comparison. **There is no regression.**

### 6.3 What the blocker most likely is, labeled as reasoning rather than measurement

This is the best available reading and it is **not** established. It is offered
because the next person needs a first hypothesis, not because it has been shown.

Coherence fixed the *comparison* between the guest TSC and QEMU's device clock.
It did not change — and could not change — how much virtual time Hermit charges
per unit of emulated guest work, which is a different quantity. Both clocks now
advance together, but they advance in the large jumps Hermit's scheduler
produces. A QEMU periodic timer that finds itself late fires **once** and re-arms
rather than delivering the backlog. On that reading the guest can receive at most
about one tick per QEMU main-loop timer service no matter how much virtual time
elapsed, which is roughly what the numbers show: the main-loop thread's `ppoll`
calls return `Ok(0)` about every 500 µs of wall time, and the guest gets
single-digit ticks over hundreds of guest-seconds.

Confirming or killing that needs instrumentation inside QEMU's timer path
(`timerlist` / `qemu_clock_run_timers` and the emulated APIC/PIT), which nobody
has done. It should not be treated as established, and no further budget should
go to the outer search loop until someone does it.

### 6.4 What would have to change for the reproduction approach to work

In order:

1. **A nested guest must receive timer interrupts at something near its
   configured HZ.** The concrete target is the native row: ~1,029 local timer
   interrupts per second of guest time, versus today's 0.017/s before the time-base
   fix and ~1.27/s after it. Whether that is fixed in Hermit's charging of virtual
   time, in how QEMU's main loop services a late periodic timer under a
   virtualized clock, or by a coalescing/backlog policy, is the open design
   question. Nothing else on this list is worth doing first.
2. **Then re-run the acceptance test, which already exists and is falsifiable.**
   Using `experiments/cc-repro_20260818/cc_qemu.py` and the order detector:
   resume the snapshot, run `order` and `order_long`, read `/proc/interrupts`
   either side, and compare tick deltas and switch counts against the native rows
   in `experiments/cc-repro_20260818/MECHANISM.md`. The prediction to test is that
   guest-visible switches reappear **and scale with workload length** the way they
   do natively — native goes from ~1,470 to 19,370 switches for a tenfold workload
   increase, while Hermit currently goes from 1 to 2. **If ticks recover but
   interleaving still does not, that is the structural question the investigation
   was looking for, and it is then the owner's call.**
3. **Then remove the guest kernel workarounds, or account for them.** Today the
   configuration needs `no_timer_check`, `lpj=999964` and `tsc=nowatchdog`, all
   three suppressing a guest check that compares the TSC to a
   `clock_gettime`-derived clock. Any reproduction claim made with those flags on
   is a claim about a guest that has been told not to look at its own clock.
4. **Then, and only then, build the sweep.** The machinery is ready and the costs
   are known (§3.2): ~25 s median per resume, one 8–11 s snapshot shared by all
   runs, per-run disk copies mandatory, per-seed wall budget mandatory with the
   number of dropped seeds recorded rather than silently absorbed. A sweep run
   before step 2 passes is N identical runs.
5. **Independently of all of the above**, land PR #2312. The two-clock defect it
   fixes is real and measured (§6.1) and is not made less real by failing to be
   this blocker; leaving it open leaves the SaBRe half stranded again.

## 7. What was built that survives regardless

None of the following depends on the steering result.

### 7.1 Working capabilities and machinery

| what | where | state |
| --- | --- | --- |
| Stop Linux under Hermit at a **chosen** guest console byte, read kernel state with `drgn`, resume; pause costs the guest <~50 µs per 60 s held | `experiments/drgn-exec-control_20260818/session.py` | works, ~3.5 s/session |
| Mid-execution stop via the tracer-freeze primitive (the earlier route, stop point wall-clock dependent) | same dir, `pause-midrun.py` | works |
| 2-vCPU MTTCG no-`icount` boot → snapshot → resume under arbitrary Hermit flags | `experiments/cc-repro_20260818/cc_qemu.py` + `build-initramfs.sh` | works, ~25 s/run |
| Two guest-side interleaving detectors (lost-update, and event-order with `switches`) | `experiments/cc-repro_20260818/guest/` | works, native-sensitive |
| A busy-wait cost model with no emulator in it | `experiments/chaos-wedge_20260818/tickwait.c` | 45 lines, reproduces the 418x |
| Two-domain clock probe (N threads, `rdtsc` vs `clock_gettime` back to back) | `experiments/qemu-coherent-timebase_20260818/twodomainN.c` | works; produced the before/after in §6.1 |
| `printk` rewind counter over a guest console | `experiments/qemu-coherent-timebase_20260818/printk_rewind.py` | works |
| Triaged corpus: 246 reports, 24 columns, 13 ranked candidates, 3 named anti-candidates, regeneration scripts | `experiments/crash_cortex/` | landed on branch `cc-triage-reports_20260818` @ `3193a7d6` |

No demo file was modified at any point. The demo-review gate confirms "no
runnable demo files touched" on every commit in this investigation. The harness
in `cc_qemu.py` is a copy rather than a parameterisation because
`demos/lib/qemu_controller.py` hardcodes `-smp 1` and `-icount shift=0,sleep=off`
and this work needs the opposite of both.

### 7.2 Three defects found and fixed, and one documentation correction

All three were found by measurement, and all three are filed separately rather
than absorbed into the investigation.

1. **Hermit's `run` path never told Reverie the guest was sequentialized.**
   `TracerBuilder::sequentialized_guest()` was called in exactly one place in
   hermit, `hermit-cli/src/replay.rs:123`. The run path
   (`hermit-cli/src/lib.rs:1698` and `:1817`) called `.gdbserver(...)` and never
   called it. So `freeze_all` did not early-return; it `tgkill`ed the sibling
   threads with SIGSTOP and waited for each to report, but under detcore those
   siblings are not really blocked in the kernel — they are parked in detcore's
   scheduler polling `InternalIOPolling`, and cannot answer until the scheduler
   runs them, which it cannot because the breakpointing thread holds its turn.
   `freeze_all` waits forever. Since sequentialize-threads is hermit's **default**,
   breakpoints under `hermit run --gdbserver` were unusable on any multithreaded
   guest. Before/after with reproducer `livethreads.c`: `gdb rc=124`, 60 s
   timeout, no output → `gdb rc=0`, 1 s, `done 43`.
   Fix: 20 insertions in one file, both run-path `TracerBuilder` sites, guarded
   exactly as `replay.rs:123` already does. **This is the fix that unblocked QEMU
   under the gdbserver**, which previously never reached its main loop (QMP
   `connect()` returned EAGAIN for 520 s).
   hermit branch `gdbserver-sequentialized-guest-on-run-path_20260818` @
   `d29ed7144c5b9d2edfcd76efa41f5595c73a6727`, off `main 770b95c505`. **Pushed,
   no PR, not on `main`** (re-verified 2026-08-19).
2. **Reverie released frozen sibling threads one event too late after a
   breakpoint.** A separate, genuinely independent defect on the
   non-sequentialized path; the hermit fix above was measured against the current
   pin with no Reverie change, so it can land alone. Reverie branch
   `gdbserver-thaw-siblings-on-resume_20260818` @
   `b0cb3f6d2cef6c207435cc2e645b9bc5b447ae0c`, plus fixture
   `tests/gdbserver-integration/test-src/blockedSiblings.c`. **Pushed, no PR.**
3. **`hermit/docs/QEMU_BOOT.md` asserted behaviour that does not reproduce** —
   both the `--no-virtualize-time --no-virtualize-metadata` claim (§5.2) and the
   "fixed QEMU icount is required" section. The doc also never states QEMU's own
   `No MTTCG when icount is enabled` constraint, which is the actual mechanical
   reason `-icount` and steerability are exclusive. hermit branch
   `qemu-boot-doc-correction_20260818` @ `19d6d706ffc85da52bb7be56c39782cdbcc9c4fc`.
   **Pushed, no PR.**

Plus PR #2312 (§6.1), the coherent time base, **open and not merged**.

### 7.3 Operational knowledge that cost real time to acquire

- **`gdb` on PATH here is a trap.** `/usr/local/bin/gdb` is a Meta build of GDB
  9.1. Against a stripped binary carrying `.gnu_debugdata` (MiniDebugInfo) —
  which `qemu-system-x86_64` is — it dies on `continue` with
  `gdb/stap-probe.c:1293: internal-error: sect_index_data not initialized` and
  dumps core. That is a `gdb` client bug, not a Hermit bug. Use `/usr/bin/gdb`
  (16.3) explicitly, never bare `gdb`. Cost to discover: ~30 minutes.
- **Every breakpoint must be armed at first attach.** Reverie's gdbstub server
  does a single `accept()` (`server.rs:125`) with no reconnect loop, and the
  async-interrupt packet (0x03) is parsed but its handler is an empty match arm
  (`reverie/reverie-ptrace/src/gdbstub/session.rs:757-758`). So `gdb` attaches
  exactly once, before the guest starts, `Ctrl-C` against a running guest is
  silently dropped, and you cannot re-attach later. A breakpoint is arguably the
  right primitive anyway — an async interrupt lands at a wall-clock-dependent
  point, which would inject nondeterminism into a run whose whole value is
  reproducibility — but silently ignoring the packet is a defect, because `gdb`
  waits forever for a stop reply.
- **The stop must not use demo 7's tracer freeze when the gdbserver is in play**:
  SIGSTOPping the tracer thread group would also freeze the thread serving the
  gdbserver.
- **`--chaos` silently discards `--no-sequentialize-threads`**
  (`detcore-model/src/config.rs:624-626`), with no warning or error.
- **A qcow2 snapshot takes an exclusive write lock**, so a parallel sweep must
  give each run its own image and reap its own children (§3.2).
- **Wall time and realized schedule are decoupled by construction.** Skid
  absorption schedules the counter interrupt early and single-steps to the exact
  target, and how much single-stepping that takes varies with host conditions.
  Directly demonstrated: `--skid-margin` 1000 / 10000 / 50000 gives wall times
  8.32 s / 22.42 s / 85.40 s — a 10.3x swing — with a **byte-identical** recording
  (`sha256 1fc00ad70ba672f6…`) throughout. So a wall-time difference between two
  runs of the same seed is not a determinism smell, and the 4x progress
  difference that started the whole skid investigation (17,033 versus 69,926
  console bytes in the same 7200 s budget, prefix-identical consoles) needs no
  scheduling explanation at all.

### 7.4 The upstream dependency for a unified debugger, located and sized

The stretch goal was a `drgn` execution-control backend driven by Hermit. It was
found rather than left as "could not locate", and the answer is *not yet, for a
known reason*.

Omar Sandoval's kernel status posts describe the work directly. *March 30 –
June 12, 2026*: "I've spent most of this time working on **breakpoint support for
drgn**. The ultimate goal is to be able to debug kernels using breakpoints both
in QEMU and live in prod." The listed prep work is precisely an extension
interface: exposing `RegisterState` publicly with pluggable register-state
finders, pluggable thread finders, and "adding the concept of a pluggable
**execution controller** that manages stopping/resuming threads and setting
breakpoints". *June 15 – July 10, 2026*: "Continued working on
breakpoints/execution control", tracked at
`https://github.com/osandov/drgn/issues/626`, and "Ran into a couple of complex
cases (stepping over software breakpoints, integrating with REPL) that made me go
back to the drawing board and **rework everything I had so far to be fully
async**."

What is available was checked directly rather than assumed:

| checked | result |
| --- | --- |
| `drgn` 0.2.0 installed here | `Program` exposes `set_qemu_qmp`, `set_pid`, `set_core_dump`, `read`/`search_memory`, `stack_trace`, `threads`. **No** continue, step, breakpoint, or controller registration |
| `osandov/drgn` main @ `232a621` | grep for `ExecutionController` / `execution_controller`: nothing |
| branch `thread-finder` @ `77c1dd6` | "Introduce thread and register state finder APIs" — the prep work, the only piece published |
| all 14 published branches | no breakpoints/execution-control branch exists |

So the controller lives in its author's working tree and its shape was still
moving as of July 2026. Implementing against it now would mean guessing at an API
its author is actively redesigning.

**This is a good position rather than a dead end.** The controller is defined as
"a pluggable execution controller that manages stopping/resuming threads and
setting breakpoints", and §3.1 shows Hermit already supplies exactly those
primitives: set a breakpoint, stop at a chosen point, read memory there, resume,
reproducibly. When #626 lands, a Hermit-backed controller is a small, well-defined
adapter over the gdbstub — a known dependency of known size.

One half-step is available now and was deliberately not taken: `drgn` already
ships a QEMU target, `Program.set_qemu_qmp` (osandov/drgn commit
`f679eb18f63447489bcdd5f0e7698e5c65f630b7`). Switching the *inspection* half from
the hand-rolled `/proc/<qemu-pid>/mem` physical-memory callback to that supported
target would delete harness code. Two unresolved caveats: it wants
`-device vmcoreinfo`, which the current QEMU command does not pass, and it would
contend for the QMP socket the harness already owns. It does **not** reduce the
`gdb` glue for *control*, which is the half blocked on #626.

## 8. Source tasks and artifact directories

### Artifact directories

Each has its own `README.md` with the full tables and the exact commands. This
report references them rather than duplicating them.

| directory | contents |
| --- | --- |
| `experiments/crash_cortex/` | `reports.csv` (246 rows), `shortlist.csv`, `triage.py`, `classification.py`, `collect.sh`, derivation of every column |
| `experiments/drgn-exec-control_20260818/` | `session.py` (the chosen-stop loop), `pause-midrun.py`, `exec-control.py`, `startup-timing.py`, recorded transcripts under `sessions/`, measurement JSON under `results/`, copies of demos 5/6/7 and their `lib/`, and `steerability/` (the TCG-vs-KVM answer with `accel_probe.py`, `boot_timer.py`) |
| `experiments/reverie-gdbstub-swbreak_20260818/` | `livethreads.c` reproducer, `lt.gdb`, diagnosis |
| `experiments/qemu-coherent-timebase_20260818/` | the boot matrix, `twodomainN.c`, `printk_rewind.py`, `rdtsc_cost.c`, `logs/` and `logs-fixed/` (before/after two-domain probe output). 612 KB, 37 log files |
| `experiments/chaos-wedge_20260818/` | `tickwait.c`, the seed sweep, the tuning sweep, the profile/observation-window reconciliation. 1.6 MB, 119 log files |
| `experiments/chaos-seed-repro_20260818/` | `recording-hashes.txt` (all 37 hashes), `qemu-recording-summary.json`, `tickwait.c`, `spin6.c`, `load.sh`. 1.8 MB |
| `experiments/cc-repro_20260818/` | `cc_qemu.py`, `build-initramfs.sh`, `sweep.sh`/`sweep2.sh`, `guest/` detectors, `MECHANISM.md` (the vCPU-alternation and `/proc/interrupts` tables, `commit-seq.txt`, `vcpu-seq.txt`), acceptance runs `res-acc-o1`/`res-acc-ol1`/`res-ab-old`. 1.3 MB |

Large intermediates were deleted after their counts were extracted and are named
here so nobody looks for them: the five 583 MB preemption recordings (hashes kept
in `recording-hashes.txt`), a 379 MB and a 298 MB and a 127 MB `--log info`
trace, and 2.9 GB of per-run qcow2 copies. The snapshot itself is regenerated in
about 11 seconds by `build-initramfs.sh` + `cc_qemu.py boot`.

### Source tasks

`cc-triage-reports`, `cc-exec-control` (and its subtask
`gdbserver_hangs_after_a`), `cc-guest-interleaving-steerable`,
`cc-qemu-coherent-timebase`, `chaos-wedges-noicount-boot`,
`chaos-fixed-seed-reproducibility`, `cc-repro-methodology`,
`does-sequentialization-serialize-guest-vcpus`, `cc-coherent-timebase-implement`.
Read them with `tg <task> -v`.

**One caution when reading them directly.**
`cc-guest-interleaving-steerable` carries an annotation saying its own headline
measurement is contradicted, and it is right to. Its conclusions that **still
hold** are: all three demos run TCG, established from the live process; the KVM
scenario is refuted; and QEMU's `No MTTCG when icount is enabled` is the
mechanical reason `-icount` and steerability are exclusive. Its conclusion that
does **not** hold is that `-icount` is required to boot at all. §5.2 and §5.7
have the reconciliation, including the part of it that is still open.

### Reproduction anchors

| item | value |
| --- | --- |
| hermit `main` at the time, and today | `770b95c505fa2794e2937b9245bb72b5f10ec340` |
| hermit release binary used for nearly every measurement | sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5` |
| hermit binary with the coherent time base | sha256 `a1abce832d2c28b7e406f58b387e83d592f79792fc17c7af1ecc122e3180e978` |
| QEMU | 10.1.2 |
| guest kernel | 6.17.13 (pinned demo `bzImage`); 6.19.2 `/boot/vmlinuz` tested as a control and made no difference to pass-versus-panic |
| guest cmdline, steerable configuration | `console=ttyS0 panic=-1 rdinit=/init tsc=nowatchdog no_timer_check` (+ `lpj=999964` for the 2-vCPU boot) |
| host | 316 CPUs; load average is recorded per experiment where it matters |

### Landing state, re-verified 2026-08-19

| change | ref | state |
| --- | --- | --- |
| coherent time base | PR [#2312](https://github.com/rrnewton/hermit/pull/2312), branch `cc-coherent-timebase` @ `fc1ed441792d0d80572085a4226b9121832d9906` | **OPEN, not merged** (`mergedAt: null`); head is not an ancestor of `origin/main` |
| hermit gdbserver sequentialized-guest fix | `gdbserver-sequentialized-guest-on-run-path_20260818` @ `d29ed7144c5b9d2edfcd76efa41f5595c73a6727` | pushed, **no PR**, not on `main` |
| reverie gdbserver thaw fix | `gdbserver-thaw-siblings-on-resume_20260818` @ `b0cb3f6d2cef6c207435cc2e645b9bc5b447ae0c` | pushed, **no PR** |
| `QEMU_BOOT.md` correction | `qemu-boot-doc-correction_20260818` @ `19d6d706ffc85da52bb7be56c39782cdbcc9c4fc` | pushed, **no PR** |
| dev-hermit experiment work | branch `cc-exec-control_20260818` @ `523028b8`; triage on `cc-triage-reports_20260818` @ `3193a7d6` | committed in the parent workspace |

Nothing from this investigation is on `hermit:main`.

---

## Is QEMU TCG + `-icount` alone bitwise deterministic, with no hermit?

Measured 2026-08-20, devbig014. QEMU 10.1.2 (`qemu-10.1.2-1.4.hs+fb.el9`), kernel
`6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79`, `ignored/qemu-linux/{bzImage,initramfs.cpio.gz}`.
Raw runs under `experiments/cc-icount-determinism_20260820/`.

**Answer: no. Plain QEMU TCG with `-icount shift=0,sleep=off` is not bitwise
deterministic here — 0 of 5 repeats matched on either half. Hermit converts the
OUTPUT half to fully deterministic and the SNAPSHOT half to mostly deterministic.**

### Method

The demos' exact QEMU command shape (`demos/lib/qemu_controller.py`,
`-machine q35 -cpu max -smp 1 -m 512M -icount shift=0,sleep=off
-rtc base=2022-01-01T00:00:00,clock=vm`), driven directly. The two arms differ
only by the presence of the `hermit run` prefix that `demos/05-qemu-boot.py` and
`demos/06-qemu-resume.py` use:

```
boot    hermit run --strict --target-timeslice 100000 --max-timeslice 2000000000 -- <controller>
resume  hermit run --strict --no-rcb-time --target-timeslice 100000 --max-timeslice disabled -- <controller>
```

Snapshots are compared on **guest payload**: the whole qcow2 with the snapshot
table's `date_sec`, `date_nsec` and `vm_clock_nsec` zeroed. That is stricter than
the demos, which canonicalize `date_nsec` only
(`demos/lib/demo_common.py:61`, writing 4 zero bytes at entry+20). Masking the
other two changes no conclusion below; it only removes a wall-clock field that is
not guest state.

### Boot: kernel boot -> snapshot + serial output

| arm | repeats | distinct serial outputs | distinct snapshot payloads |
| --- | --- | --- | --- |
| hermit ABSENT | 5 | **5** | **5** |
| hermit PRESENT | 10 | **1** | 4 (7 of 10 identical) |

### Resume: snapshot -> post-snapshot + output (`uname -a`, from one identical base snapshot)

| arm | repeats | distinct outputs | distinct post-snapshot payloads |
| --- | --- | --- | --- |
| hermit ABSENT | 5 | 3 | **5** |
| hermit PRESENT | 5 | 2 (4 of 5 identical) | 2 (4 of 5 identical) |

### What differs without hermit, and it is not just metadata

Diffing two hermit-absent boot transcripts shows three distinct kinds of
difference, so this is real execution divergence rather than a timestamp field:

* printk timestamps differ by ~1 us (`0.403683` vs `0.403684`);
* `sched_clock: Marking stable (1490001750, ...)` vs `(1490001754, ...)`;
* **device probe messages arrive in different orders** — `ata5/ata2/ata1` in one
  run, `ata2/ata1/ata5` in another;
* transcripts end at different points (26209 / 26451 / 26452 bytes).

`vm_clock_nsec` at snapshot time was distinct in all 5 hermit-absent boots
(7.45e9 to 8.90e9). With `-icount shift=0` the VM clock is instruction-driven, so
distinct values mean distinct instruction counts.

### Where hermit's residual snapshot nondeterminism comes from

Across 10 hermit boots the serial output was **one** hash, while the snapshot
payload was four. The `vm_clock_nsec` values were 7403393368 (x7), 7402393368
(x2) and 7398393368 (x1) — spreads of 1 ms and 5 ms of VM clock.

Identical output with a differing snapshot means the guest executed identically
and the snapshot was *taken at a different instant*. This is a stop-point
difference, not an execution difference: the controller tails the serial
transcript for a marker and then issues QMP `savevm`, and the delay between those
two is host-scheduled. It is not a first-run effect — the outliers were runs 1, 7
and 10 of 10.

### Cost

| | repeats | min | median | max |
| --- | --- | --- | --- | --- |
| boot, no hermit | 5 | 18.48 s | 19.07 s | 22.64 s |
| boot, hermit | 10 | 43.27 s | 44.36 s | 58.93 s |
| resume, no hermit | 5 | 0.96 s | 0.99 s | 1.14 s |
| resume, hermit | 5 | 6.38 s | 6.46 s | 6.99 s |

Hermit costs ~2.3x on boot and ~6.5x on resume.

### What has NOT been shown

* **This is single-vCPU by construction.** `-smp 1`, and QEMU forbids MTTCG when
  icount is enabled. These numbers say nothing about thread interleaving; do not
  read them as an interleaving result.
* I ran the demos' **command shape**, not `demos/05-qemu-boot.py` /
  `06-qemu-resume.py` themselves. Those scripts carry additional scaffolding (a
  `boot-anchor` directory, repeat auto-verification) that may pin the snapshot
  stop point further. A 7-of-10 snapshot result here is not a measurement of the
  demos' own pass rate.
* One kernel, one initramfs, one host, one guest command (`uname -a`).
* I did not isolate the QMP `savevm` delay as the cause of the snapshot spread;
  that is the leading hypothesis, supported by identical output alongside
  differing `vm_clock_nsec`, not a proven mechanism.
* The hermit-absent resume arm produced 3 distinct outputs from 5 runs, two of
  which had a longer transcript; I did not classify those separately from the
  266-byte majority.
