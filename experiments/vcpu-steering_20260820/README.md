# Does choosing which vCPU host thread runs choose which guest CPU advances?

Task: `test-indirect-vcpu-steering-under-mttcg`. **Yes, and close to one-for-one.**
Across 31 runs the share of host CPU a vCPU thread received and the share of guest
work its guest CPU did track each other with `r = 0.991` on a slope of `1.07`; on the
cleanest subset the two agree to within 2.2 percentage points.

No hermit, no SCX, no new tooling in any repository. Everything here is bare QEMU
plus unprivileged `taskset`, `renice`, `chrt` and busy loops.

| item | value |
| --- | --- |
| QEMU | 10.1.2, `-machine q35 -cpu max -m 512M -accel tcg,thread=multi -smp 2` |
| guest kernel | 6.17.13 (`hermit/target/qemu-busybox/bzImage`), `tsc=nowatchdog no_timer_check` |
| host | 316 CPUs, load average 16-27 during the runs |

## What is measured, and why it reads no clock

`guest/advance.c`: two processes, one pinned to each guest CPU with
`sched_setaffinity`, each spinning on its own counter in its own page of shared
memory. The first to reach `K` reads the other's counter and prints
`ADVANCE R<i> me=K other=<v> other_started=<0|1>`. `other/K` is how far the other
guest CPU got in the same interval.

No clock is read on either side. Guest timers under MTTCG without icount are the
subject of several retired claims, so a detector that depended on them would inherit
the argument it exists to settle.

`other_started` is a flag each child sets on reaching its spin loop. Without it a
reported `other=0` cannot be told apart from a child that never ran, and those are
different sentences.

On the host, `sampler.py` records each vCPU thread's `utime+stime` every 20 ms and
brackets the workload between `ADVANCE-START` and the first `ADVANCE R`. The vCPU
threads are identified by asking QEMU — `query-cpus-fast` returns `cpu-index` ->
`thread-id`. This build does not name its threads; all four report
`comm = qemu-system-x86`.

## Why the correlation is not a tautology

An idle guest CPU consumes no host CPU, which would produce the same correlation with
the causality reversed. That is ruled out by the workload: **guest demand is constant
by construction.** Both guest processes are infinite spin loops, runnable at every
instant until they hit `K`, identical in every arm. Nothing on the guest side varies
between runs, so the variation in guest advancement comes from the host side.

## Result

`host cpu0` is vCPU0's thread's share of the two threads' host CPU time in the
workload window. `guest cpu0` is guest CPU0's share of the two guest counters.

| run | actuator | host cpu0 | guest cpu0 | host ticks v0/v1 | guest counts (M) |
| --- | --- | --- | --- | --- | --- |
| A-1 | none | 49.8% | 49.9% | 117/118 | 99.7 / 100.0 |
| C-1 | SCHED_IDLE on vCPU1 | 100.0% | 100.0% | 30/0 | 100.0 / **0.0** |
| C-2 | SCHED_IDLE on vCPU1 | 27.8% | 24.6% | 10/26 | 32.7 / 100.0 |
| D-2 | SCHED_IDLE on vCPU0 | 0.5% | 7.4% | 4/878 | 8.0 / 100.0 |
| N10-1 | nice 10 on vCPU1 | 90.0% | 86.7% | 45/5 | 100.0 / 15.3 |
| E-pad | contention, favour vCPU0 | 86.2% | 84.0% | 50/8 | 200.0 / 38.2 |
| F-pad | contention, favour vCPU1 | 11.9% | 10.9% | 7/52 | 24.5 / 200.0 |
| G-pad | contention, alternating | 55.2% | 54.6% | 53/43 | 200.0 / 166.4 |

Full table: `sweep2.tsv` and the `log-*.txt` files.

| subset | n | r | slope | max abs difference |
| --- | --- | --- | --- | --- |
| all runs | 31 | 0.991 | 1.07 | 10.6 pp |
| priority actuator | 21 | 0.992 | 0.99 | 6.9 pp |
| contention actuator | 10 | 0.994 | 1.14 | 10.6 pp |
| separate counter pages | 4 | 1.000 | 0.98 | **2.2 pp** |

The strongest single run is C-1: vCPU1's host thread received **0** host ticks and the
guest process pinned to guest CPU1 executed **zero** loop iterations —
`other=0 other_started=0` — while being runnable throughout and while guest CPU1 was
online (`ADVANCE-START ncpu=2`).

## The transmission is strong; the actuator is the part that matters

`nice` and `SCHED_IDLE` are advisory. A deprioritised vCPU thread runs freely whenever
the other blocks on the BQL or halts, and **5 of the 21 priority-actuator runs went the
opposite way to the request** — C-2 asked for vCPU1 to be `SCHED_IDLE` and vCPU1 got 72%
of the host CPU; D-3 asked for vCPU0 and vCPU0 got 69%.

In every one of those the guest followed the host CPU **actually delivered**, not the
bias requested. That is a natural experiment on the independent variable and it is
stronger evidence than a clean arm design would have been.

Contention is not advisory. Putting the disfavoured vCPU thread on a host CPU with 7
busy loops and the favoured one on a free host CPU gives a reliable, mirror-symmetric
result: `E` 84-98% and `F` 2.6-10.9% guest CPU0 share, reproducible across replicates.

## The bias can be changed during the run

Arm `G` alternates which vCPU thread is favoured while the workload runs.

| period | flips | host cpu0 | guest cpu0 |
| --- | --- | --- | --- |
| 0.2 s | 6 | 54.6% | 54.0% |
| 0.05 s | 16 | 51.5% | 50.7% |
| 0.01 s | 46 | 52.0% | 50.9% |

Against `E` (84-98%) and `F` (2.6-10.9%) on the same actuator, alternation lands near
even. The handle is live, not launch-time only.

**The 0.01 s and 0.002 s rows have the same flip count because the driver's poll loop
sleeps 20 ms.** The floor measured here is the driver's, not the mechanism's; the finest
achievable change interval is not established.

## Two instrument errors, recorded because each one produced a plausible wrong answer

1. `/proc/<tid>/stat` returns the **thread group's** `utime+stime`, not the thread's.
   Per-thread reads gave `216/216`, `60/59`, `211/211` — and 211 ticks is 2.11 CPU-seconds
   inside a 1.05 s wall window, impossible for one thread. Two threads reporting the same
   number every run was the tell. The correct path is `/proc/<pid>/task/<tid>/stat`.
2. Bracketing the workload by grepping the serial log every 0.1 s cannot bracket a ~1 s
   workload: it reported 100,000,000 guest iterations in 0.33 s, or 300 M/s from an
   emulator measured at ~95 M/s. Replaced by a 20 ms sampler that records the series and
   picks the bracket afterwards.

Neither touched the guest-side counter ratio, which reads no clock and needs no host
accounting.

A third effect, not an error: both counters initially shared one page, so the two guest
CPUs wrote the same page constantly and an unbiased run was ~4x slower than one with a
starved CPU. It cancels out of a ratio, and it is removed in the `pad-*` runs, which are
the cleanest subset in the table above.

## What this does not establish

- **Cross-CPU only.** This is control over which guest CPU advances. It says nothing
  about two guest threads on ONE vCPU, which is a single host thread and remains outside
  this handle. The original "wrong layer" objection still stands for that case.
- **Not a race yet.** A biasable advancement ratio is not a driven interleaving. Whether
  a change interval of tens of milliseconds is fine enough to place a specific guest
  kernel race is untested.
- **Granularity floor unknown**, per the note above.
- The actuator used here is host contention, chosen because it is not advisory. Whether
  hermit's own handle over traced threads gives the same or better control is the obvious
  next question and was not tested.

## Reproduce

```bash
./build.sh                                        # initramfs with the detector
./run.sh <tag> A|B|C|D <host-cpu> <K>             # priority actuator
python3 steer.py E|F|G <tag> --hogs 7 --k 200000000 --period 0.05   # contention actuator
```

---

# Putting hermit in the loop: does its hold on a traced thread reproduce this?

Task: `put-hermit-in-the-loop-on-vcpu-advancement-bias`. **The acceptance test does not
transfer.** Not a clean yes and not a clean no: hermit visibly biases which guest CPU
advances — more strongly than host contention did — but the recorded test's independent
variable, host CPU time per vCPU thread, stops measuring guest execution once hermit is
the scheduler, so the identity line cannot be evaluated in that currency.

Detector, sampler and the two measured quantities are unchanged. Only the actuator
changes. hermit `target/release`, `run --strict`, resumed from a snapshot because
`experiments/cc-repro_20260818/README.md` records that a sequentialized 2-vCPU
no-icount *boot* under chaos never completed in 7200 s.

## The table, against the recorded one

| actuator | n | r | slope |
| --- | --- | --- | --- |
| host contention / priority (recorded above) | 31 | **+0.991** | +1.07 |
| hermit, all arms | 14 | **-0.863** | -0.48 |
| hermit `--strict` only | 10 | **-0.960** | -0.43 |

It does not weaken. It inverts.

## The mapping is exact, and the orientation test is validated on bare QEMU

A consistent swap of the two host threads would turn `-0.96` into `+0.96`, so it had to be
excluded before the inversion could be reported. Hermit runs the guest in a PID namespace,
so QMP reports namespace-local thread ids:

```
  NSPID host_tid=3412590 NSpid:	3412590	3
  NSPID host_tid=3412611 NSpid:	3412611	5
  NSPID host_tid=3412613 NSpid:	3412613	7
  NSPID host_tid=3412616 NSpid:	3412616	9
  NSPID host_tid=3412618 NSpid:	3412618	11
  NSPID host_tid=3412620 NSpid:	3412620	13
  ... qmp said [11, 13]
```

Exactly two namespace levels, and the six inner ids are the same set
`experiments/cc-repro_20260818/MECHANISM.md` reports (3 = main loop, 5/7/9 = helpers,
11/13 = vCPUs). The resolution is exact by construction, not inferred.

The orientation test itself — load exactly one guest CPU, see which host thread burns the
CPU — is clean on bare QEMU:

| bare QEMU, only guest CPU loaded | v0 ticks | v1 ticks |
| --- | --- | --- |
| CPU0 | **36** | 8 |
| CPU1 | 10 | **33** |

## Under hermit the same test inverts, reproducibly

| hermit, only guest CPU0 loaded, K = 10,000,000 | v0 ticks (working) | v1 ticks (idle) |
| --- | --- | --- |
| run 1 | 114 | **996** |
| run 2 | 98 | **1308** |
| run 3 | 107 | **1165** |

Three of three: **the IDLE guest CPU's host thread burns 9-13x the host CPU of the thread
doing all the work.** (A fourth run at K = 2,000,000 gave 1728/426, the other way; K
differs and it is not comparable, so the claim rests on the three matched runs.)

Guest loop iterations per host CPU-second, per vCPU thread, across the measurement runs:

| | median between-CPU ratio | max |
| --- | --- | --- |
| bare QEMU | **1.1x** (n=20) | 17.5x |
| hermit | **11.7x** (n=14) | 36.2x |

Under bare QEMU the two threads convert host CPU into guest work at the same rate, which
is exactly why the identity line held. Under hermit they do not, so the meter is gone.

## Hermit's handle is not weak — it is strong enough to hang the guest kernel

Guest CPU0's share of the work spans **20.0%** (`--sched-heuristic Random --sched-seed 1`)
to **80.7%** (seed 3). With
`--sched-heuristic StickyRandom --sched-sticky-random-param 0.9`, all 3 seeds produced
guest-kernel CSD-lock stalls and 2 of the 3 never finished the workload. The guest says so
itself:

```
[   19.273216] smp: csd: Detected non-responsive CSD lock (#1) on CPU#1, waiting 5000000032 ns for CPU#00 do_sync_core+0x0/0x40(0x0).
[   19.300754] Sending NMI from CPU 1 to CPUs 0:
[   31.326515] smp: csd: Detected non-responsive CSD lock (#2) on CPU#1, waiting 5000001081 ns for CPU#00 ...
[   41.187216] nmi_backtrace_stall_check: CPU 0: NMIs are not reaching exc_nmi() handler
```

Guest CPU1 waited five seconds of guest time for guest CPU0 to answer an IPI — the guest
kernel's own diagnosis of exactly the phenomenon under test.

## An observation, not a claim

Ten `--strict` runs produced nine distinct guest counter values; `15462539` appeared twice
(b1 and b9), to the digit. That is a hint of hermit's determinism showing through, and it
is **not** evidence of a reproducible interleaving: the workload is injected over a serial
pipe at a host-timing-dependent moment, which is a nondeterministic input to an otherwise
deterministic run. Reproducibility remains untouched by this route.

## What would settle it

Host CPU time is the host scheduler's currency. Hermit schedules in its own — scheduler
turns per stable deterministic thread id, which its `--log info` `COMMIT` records already
expose and which `experiments/cc-repro_20260818/MECHANISM.md` already read once
(`turns on vCPU threads: 150 (11: 108, 13: 42)`). Re-running this table with turns on the
x-axis instead of host CPU seconds is the measurement that would say whether hermit's hold
transmits as faithfully as the host scheduler's. **It was not done here** — it is a
different independent variable, and this task was a like-for-like comparison.

## Reproduce

```bash
python3 vs_qemu.py boot --smp 2 --append-extra "tsc=nowatchdog no_timer_check" --disk snap.qcow2
python3 hermit_run.py <tag> --k 30000000 --settle 25 [--hermit-extra "..."] [--no-hermit]
python3 hermit_run.py <tag> --orient 0|1 --k 10000000     # orientation check
```

---

# Third x-axis: hermit's own scheduler turns

Task: `re-measure-steering-with-scheduler-turns-on-the-x-axis`. **Turns do not work as the
independent variable either**, and they fail for a more fundamental reason than host CPU
seconds did: a scheduler turn is a *scheduling event*, and the guest CPU that is advancing
generates the fewest of them.

Detector, arms and identity line unchanged. Only the x-axis changed, from host CPU seconds
to `COMMIT` records per stable deterministic thread id.

## The record, and three costs the instrument imposes

A `COMMIT` line is a continuation of the `[sched-step5]` record above it, and that record
carries a host wall-clock timestamp — which is what makes bracketing possible at all:

```
2026-08-20T10:03:26.903575Z  INFO detcore::scheduler: [sched-step5] >>>>>>>
 COMMIT turn 5, dettid 3 using resources {Path("/lib64/libpixman-1.so.0"): R}, on previously committed 1_767_225_600.003_412_930s
```

dettid 11 and 13 are the vCPU threads: last section's `NSpid` dump resolved QMP's
`cpu-index` 0 and 1 to inner pids 11 and 13, and `cc-repro`'s `MECHANISM.md` reached the
same pair independently by retired-branch count.

1. **No per-module log filter.** `--log detcore::scheduler=info` is refused —
   `expected one of "off", "error", "warn", "info", "debug", "trace", or a number 0-5`.
   Turns can only be read by turning everything on.
2. **The instrument costs at least 4x.** A single-guest-CPU run at K = 2,000,000 takes 48 s
   unlogged and did not finish in 300 s with `--log info`, producing a 215 MB log. Every
   number below is from a logged run and is not comparable to an unlogged one.
3. **No in-log marker for the guest's own output** — `grep -c ADVANCE` over the log is `0`.

## A whole-run turn count cannot see the workload — measured, not assumed

Two idle resumes with **no workload at all**:

```
IDLE wall=40.00s   turns:  11247 dettid3  ...   36 dettid11   39 dettid13
IDLE wall=40.00s   turns:  23595 dettid3  ...  175 dettid11   80 dettid13
```

36-175 turns on dettid 11 with nothing running, against 66-201 in runs that ran a workload.
An early whole-run comparison here (62.0% vs 61.2%) was therefore measuring the settle
phase in both arms and is **withdrawn**; it survives only as the reason the bracket was
needed. The harness now stamps the workload window in UTC and `turns.py` counts only the
turns inside it.

## Bracketed, the counts are single digits

Guest work placed entirely on one guest CPU, five runs each way:

| guest work entirely on | dettid 11 turns | dettid 13 turns | CPU0 turn share | turns total |
| --- | --- | --- | --- | --- |
| CPU0 | 24 | 23 | 51.1% | 47 |
| CPU1 | 8 | 12 | 40.0% | 20 |

The direction is right and the size is not usable: 100% of the guest work moved from one
CPU to the other, and the turn share moved about 11 points on pooled counts of 47 and 20
events. With 20 events the second estimate cannot be separated from the first by counting
alone.

Per run the counts are 1 to 7 turns per vCPU thread across windows of 3.6 to 31.7 s — about
one turn every 3 seconds. In the two-CPU acceptance-table runs, properly bracketed, the
in-window counts are **0 or 1**:

```
both-strict  wb1 win=0.02s  t11_in=0 t13_in=0     both-random-s1 wr1 win=0.04s  t11_in=0 t13_in=0
both-strict  wb3 win=4.94s  t11_in=1 t13_in=1     both-random-s2 wr2 win=0.00s  t11_in=0 t13_in=0
```

(The sub-second windows are the serial-output buffering collapse seen earlier in this
directory, not a claim that the workload lasted 0.02 s.) A quantity with single-digit
counts over the measured interval cannot express a graded advancement ratio, so the
identity line cannot be drawn against it. Computing an `r` on this would be arithmetic on
a coin.

## Why it fails, and it is the same reason host CPU seconds failed

A turn is granted when a thread blocks or wakes. An idle guest CPU halts and wakes on every
timer interrupt; a spinning one just runs. Within the **same run**, comparing the workload
window against the rest of that resume:

| run | guest work on | window | turns in window | turns outside | in-rate /s | out-rate /s | ratio |
| --- | --- | --- | --- | --- | --- | --- | --- |
| rt0a | CPU0 | 18.4 s | 6 | 89 | 0.33 | 0.51 | 1.6x |
| rt0b | CPU0 | 25.8 s | 13 | 145 | 0.50 | 1.42 | 2.8x |
| rt1a | CPU1 | 4.7 s | 2 | 142 | 0.42 | 1.59 | 3.7x |
| rt1b | CPU1 | 4.8 s | 2 | 139 | 0.42 | 1.48 | 3.5x |

Four of four: the **idle** part of the resume generates 1.6-3.7x more vCPU scheduler turns
per second than the CPU-bound workload does.

That is the same shape as the host-CPU-seconds result, where the idle guest CPU's host
thread burned 9-13x the host CPU of the thread doing all the work. Both meters count
hermit's *reaction to a guest CPU being idle* — a ptrace stop in one currency, a scheduler
turn in the other — rather than the execution it delivered to a guest CPU that is busy.

For scale, the vCPU threads are a rounding error in hermit's scheduling either way: dettid
3, the QEMU main loop, takes about 84% of every turn (19,484 of 28,931 in one run), and the
two vCPU threads together take under 1%.

## What this does and does not change

Unchanged: hermit visibly biases which guest CPU advances — that is not in question and was
established with the guest kernel's own CSD-lock diagnosis. What is still missing is a
quantity in which to express *how faithfully*, and two candidates have now been tried and
rejected on measurement.

The four limits carried forward are **not** lifted by anything here: still cross-CPU only;
still a lever on how much rather than a choice of which interleaving; still no race
exposed; granularity floor still unmeasured. And the two-identical-values-in-ten
observation is still not reproducibility, for the reason given before — the workload is
injected over a serial pipe at a host-timing-dependent moment.

**A third candidate exists in the same record and was not tried here:** each `COMMIT`
carries a virtual timestamp (`on previously committed 1_767_225_600.003_412_930s`), so the
gap between a thread's consecutive commits is the virtual time it was granted. That is
continuous where a turn count is not, and it is hermit's own logical-time currency. Whether
it tracks guest-CPU advancement is a separate measurement and substituting it here would
have been the same goalpost move this task exists to avoid.

## Reproduce

```bash
python3 hermit_run.py <tag> --orient 0|1 --k 500000 --hermit-log info --hermit-log-file X.log
python3 turns.py X.log <start-iso> <end-iso>       # start/end printed as WINDOW_UTC
python3 hermit_run.py <tag> --idle-only --idle-secs 40 --hermit-log info --hermit-log-file X.log
```

---

# Does a smaller timeslice buy guest preemptions — and enough turns to measure against?

Task: `measure-timeslice-to-switches-with-the-guest-preemption-detector`.
**Turns: yes, about ten times as many, monotonically. Guest preemptions: not answered —
my workload was under-powered and I say so rather than read the flat result as a
falsification.**

Configuration for every arm below: hermit `run` with **default sequentialization, not
`--strict`**, plus `--no-virtualize-time --no-virtualize-metadata`; QEMU 10.1.2,
`-smp 2 -accel tcg,thread=multi`, no icount; snapshot resume; K = 1,000,000 spins per
guest process. Admission gate is the predeclared one, sha256
`51069739f831599b14a437ac5a93bc4b1d63f5227fe05913a6b4b9498ce82cbb`, census before and
after every arm.

## Turns granted to the vCPU threads, per timeslice

Bracketed to the workload window by UTC stamps and `turns.py`, `--log info`, both guest
processes on guest CPU0:

| timeslice | window | turns dettid 11 + 13 | turns / s |
| --- | --- | --- | --- |
| default (200 ms) | 24.66 s | 13 + 7 = **20** | **0.81** |
| 20 ms | 2.78 s | 16 + 7 = 23 | 8.27 |
| 5 ms | 21.80 s | 98 + 88 = **186** | 8.53 |
| 5 ms (replicate) | 24.23 s | 106 + 109 = **215** | 8.87 |
| 2 ms | 60.17 s | 397 + 274 = **671** | **11.15** |

Monotone, and the 5 ms replicate agrees to 4%. Whole-run counts move the same way:
149/118 at default, 245/255 at 20 ms, 552/487 and 451/586 at 5 ms, 1171/1000 at 2 ms.

**This is what the task was blocking.** The steering work rejected two candidate
independent variables for resolution: the vCPU threads were receiving roughly **one turn
every three seconds**, so a run offered a handful of grants to correlate against. At 2 ms
the same threads receive 11 turns per second — 671 grants inside one window. That is
enough events for a turns-based or virtual-time-based x-axis to be worth attempting.

Caveat kept in view: the 20 ms window is 2.78 s because of the serial-output buffering
collapse documented earlier in this directory, so its rate rests on a short window. The
5 ms and 2 ms points carry 21-60 s windows and are the solid ones.

## Guest preemptions: under-powered, not falsified

Involuntary context switches of the workload processes, from
`nonvoluntary_ctxt_switches` in `/proc/self/status` either side of a fixed spin:

| timeslice | admissible arms | rejected arms |
| --- | --- | --- |
| default | *(none — both rejected)* | 0, 0 |
| 20 ms | **10, 8** | 0 |
| 5 ms | **0, 9** | 26 |
| 2 ms | **0, 12** | 2 (split placement) |

Counts of 0 to 12 with no timeslice dependence the data can distinguish. Under the 07:35
predeclaration that is the "flat within repeat spread" outcome, which would falsify the
lever — **and I do not think that reading is safe.** K = 1,000,000 is very likely below
resolution: the runner recorded that 10,000,000 was already below resolution for their
detector (native 24, default 2, 20 ms 2), and mine is ten times smaller than that. I chose
it because a 10,000,000 workload at 2 ms did not finish and produced guest soft lockups
and workqueue stalls.

Rejections were `new scan errors: 0 -> 1`, a transient `/proc` read race from my own run
churn, not identity substitution. Reported as rejected rather than quietly kept.

## Three things that would have silently invalidated the measurement

1. **`--strict` and `--no-virtualize-time` cannot be combined.** With time virtualization
   off the guest's `gettimeofday` becomes an unsupported syscall and `--strict` rejects it:
   `ERROR detcore: ... inbound syscall: gettimeofday(...)` / `Error: Sandbox container
   exited unexpectedly`. The predeclared configuration says novirt "with default
   sequentialization" for this reason. Two runs died silently before I read the stderr.
2. **One process per guest CPU cannot measure guest preemption.** With one runnable
   process per guest CPU the guest kernel has no reason to preempt it, so the count is ~0
   by construction — measured: 0 involuntary at a 2 ms timeslice. Preemption needs two
   runnable processes on **one** guest CPU, which is also the case the steering work has
   been unable to reach. The detector takes a placement argument; `split` is kept as the
   control and still reads ~0.
3. **The harness was leaking every run.** `SIGTERM` to the hermit process does not tear
   down the tree: the guest runs as a PID-namespace init so the kernel discards `SIGTERM`
   there, and `pkill -P` reaches only direct children. 61 of my own hermit and QEMU
   processes were alive before this was found. Under the admission gate my own unreaped
   orphan rejects the arm, so every subsequent arm would have been thrown away. Fixed by
   starting hermit in its own process group and `SIGKILL`ing the group; verified 7 before,
   7 after.

## Two tables that must not be merged

A timeslice-to-switches curve already exists on branch `timeslice-law_20260820`:
200 ms → 1, 20 ms → 19, 10 ms → 40, 5 ms → 85, 2 ms → 174, native → 1144. That is the
`order` detector on an `order; racer` workload under `--strict` with time virtualization
**on**. This section is a preemption detector on a fixed spin under novirt flags with
default sequentialization. Different instrument, workload and flags.

Note the coincidence to be careful of: that curve has 5 ms → 85 and this task's one
inherited arm is 50 ms → 83. Nearly equal counts a decade apart in timeslice, from two
different instruments. Treating them as one series would invent a flat curve neither
measured.

## What is not claimed

The detector named in the 07:35 predeclaration (sha256 `e0d7c8fa...`) is not in the
working tree, on any branch, or in a worktree, so mine is an equivalent I wrote and its
numbers are **not** claimed to be comparable with the inherited 50 ms → 83 arm.

Nothing here lifts the four standing limits: still cross-CPU only for the steering result;
still a lever on how much rather than a choice of which interleaving; no race exposed
here; granularity floor still unmeasured. Absolute counts are measured under the recorded
host state with eight other-agent background processes present; the comparative turn
rates are the claim.

## Reproduce

```bash
./arms.sh <tag>:<timeslice-ns|default>:<same|split>          # switches, with census + gate
./turnarms.sh <tag>:<timeslice-ns|default>:<same|split>      # turns, --log info
```

---

# A workload that gives hermit many more scheduling decisions

Task: `find-a-workload-long-in-scheduler-terms-short-in-execution`. **Route (a) works and
beats the timeslice route at the default timeslice** — 54.7 scheduler turns per second
against 0.81 for the spin workload at default and 11.15 for the spin workload at 2 ms,
with the workload finishing, which the small-timeslice route could not manage.

The same four arms also measure the standing "wrong layer" limit directly instead of
arguing it.

## The design, in one sentence

Hermit grants a scheduler turn when a **host** thread blocks or wakes; a spin loop never
blocks, so hermit had nothing to decide. Two guest processes handing a byte back and forth
over a pair of pipes put one guest CPU to sleep and wake the other on every handoff, and an
idle guest CPU is what makes QEMU's vCPU thread block.

`split` = one process per guest CPU (every handoff crosses guest CPUs).
`same` = both on guest CPU0 (the guest kernel switches between them and neither guest CPU
ever idles) — the control that separates "the guest scheduler ran" from "a guest CPU slept
and woke".

## Result

All arms: 2-vCPU MTTCG, no icount, snapshot resume, hermit `run` with default
sequentialization plus `--no-virtualize-time --no-virtualize-metadata`, **default
timeslice**, census before and after each arm with `admit.py`.

| tag | placement | handoffs | turns 11+13 | turns / handoff | turns / s | involuntary guest switches | admit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| v3 | split | 2000 | 4085 | 2.042 | 54.75 | 1 | ADMIT |
| v6 | split | 2000 | 4086 | 2.043 | 54.73 | 1 | ADMIT |
| v5 | split | 4000 | 8218 | 2.054 | 43.16 | 3 | REJECT |
| v4 | same | 2000 | 27 | 0.013 | 1.49 | 1809 | REJECT |
| v7 | same | 2000 | 33 | 0.017 | 1.10 | 1657 | ADMIT |

**The mechanism is confirmed by the count itself.** Roughly one turn per vCPU per handoff
(2.04), and doubling the handoffs doubles the turns: 4086 → 8218, a ratio of 2.011 against
a handoff ratio of 2.000. A number that lands within 3% of the handoff count on both
threads, and scales with it, is not a coincidence that needs defending.

**And the two replicates agree to one turn out of 4085 — 0.024%.** v3 gave 2041/2044 and
v6 gave 2042/2044. That is reproducibility of a *count*, not of an interleaving, and it is
not offered as the latter.

## The part that matters more than the resolution fix

The two placements are **anti-correlated**:

| | hermit turns | involuntary guest switches |
| --- | --- | --- |
| split — guest CPUs hand off to each other | 4085, 4086, 8218 | 1, 1, 3 |
| same — two processes contend on guest CPU0 | 27, 33 | 1809, 1657 |

When the guest CPUs hand off, hermit has thousands of decisions and the guest kernel
preempts nobody. When two guest processes contend on one guest CPU, the guest kernel
preempts them ~1700 times and hermit has essentially nothing to decide — because neither
guest CPU ever goes idle, so neither vCPU host thread ever blocks.

That is the standing cross-CPU-only limit, measured. **The scheduling hermit can see is not
the scheduling a same-guest-CPU race needs, and across these arms the two move in opposite
directions.** Both placements are replicated and both same-placement arms agree (1.49 and
1.10 turns/s; 1809 and 1657 involuntary), so this is not resting on one run.

## What the title claims and what was actually achieved

The workload is **not** shorter in guest execution. Using the unlogged wall window as a
proxy, `split` at 2000 handoffs takes 28.08 s against 24.66 s for the spin workload — the
same order. What changed is the **density of scheduling decisions per unit of wall time**:
54.75 turns/s against 0.81, a factor of 68. That satisfies the intent — attack the
resolution problem through the workload rather than the timeslice — and I am not claiming
the literal wording.

## Two bugs of mine, recorded because each produced a wrong-looking result first

1. **`same` and `split` both begin with `s`.** The guest parsed `argv[2][0] == 's'`, so the
   first run asked for `split` and silently ran `same`; both sides reported `oncpu=0`. The
   preempt detector had the identical bug. Both now compare the whole word.
2. **`--log info` writes at a measured ~25.5 GiB/h** and reached 1.0 GiB during the 221 s
   `v5` arm, which tripped the headroom monitor. The arm completed and its log was deleted,
   so it was bounded — but only by the run finishing. `pparms.sh` now watches its own log
   and kills the arm past 4 GB, writing `SIZE-CAP` into the output so a capped arm is
   visible rather than silently short.

## Not merged with the other curve

Branch `timeslice-law_20260820` has 200 ms:1, 20 ms:19, 10 ms:40, 5 ms:85, 2 ms:174,
native:1144 — the `order` detector on an `order; racer` workload under `--strict` with time
virtualization **on**. Nothing here is plotted against it.

## Reproduce

```bash
./pparms.sh <tag>:plain|turns:split|same:default|<ns>:<handoffs>
```

---

# Third x-axis: virtual time granted, and the faithfulness question

Task: `measure-steering-faithfulness-with-virtual-timestamps`. **The meter works — the
resolution objection is gone — but the question is still not answered, and the reason is
the same one that sank the first two x-axes.** Where hermit leaves the two guest CPUs
roughly balanced, virtual-time share predicts guest advancement to about 1 percentage
point. Where hermit actually biases, the guest outcome saturates and the relationship
inverts.

## The meter

Every `COMMIT` carries hermit's logical clock. The virtual time a thread was granted is the
advance of that clock across the commits where it held the turn (`vtime.py`). From one
21.25 s window at a 2 ms timeslice:

```
granted_s={3: 0.113629, 9: 0.005, 11: 0.216223, 13: 0.177974}  turns={3: 65, 9: 2, 11: 104, 13: 84}
```

188 grants to the two vCPU threads in one window, and a continuous quantity to divide.
That is what the workload change bought: the turn-count x-axis had 0-7 events per window.

## Result

x = vCPU0's share of granted virtual time; y = guest CPU0's share of the guest counters.
All arms: 2-vCPU MTTCG, no icount, novirt flags, default sequentialization, 2 ms timeslice,
K = 1,000,000, census before and after.

| tag | arm | x: virtual-time share | y: guest share | \|x−y\| | outcome | admit |
| --- | --- | --- | --- | --- | --- | --- |
| cal2 | calibration | 54.9% | 44.0% | 10.9 pp | graded | no census |
| g1 | plain 2 ms | 48.9% | 47.3% | **1.6 pp** | graded | REJECT |
| g2 | plain 2 ms | 50.6% | 50.1% | **0.5 pp** | graded | ADMIT |
| f0a | StickyRandom p=0.0 | 57.2% | 100.0% | 42.8 pp | saturated | ADMIT |
| f9a | StickyRandom p=0.9 | 41.7% | 100.0% | 58.3 pp | saturated | REJECT |

**Where the outcome is graded and near balance, the meter is good**: 1.6 and 0.5 pp. The
third graded point, 10.9 pp off, sits further from balance.

**Where hermit biases hard, it fails, and it fails in the familiar direction.** Both
StickyRandom arms drove the losing guest CPU to exactly zero. Worse than saturation: in
`f9a` vCPU1's thread was granted **more** virtual time (0.7047 s against 0.5042 s) and its
guest CPU did **nothing**. Across the two arms the virtual-time share moved 57.2% → 41.7%,
crossing 50, while the guest outcome did not move at all.

That is the same defect as host CPU seconds: an idle guest CPU's host thread keeps
consuming the resource — a ptrace stop there, a scheduler turn there, and now virtual time
here — without executing guest code. All three meters count what hermit *gave* a vCPU
thread, and a thread spinning in QEMU's idle path is given plenty.

## Scoreboard across all three x-axes

| x-axis | actuator | result |
| --- | --- | --- |
| host CPU seconds | bare-QEMU contention | r = +0.991, slope +1.07 — the meter works |
| host CPU seconds | hermit | r = −0.960 — inverts |
| scheduler turns | hermit | unusable — 0-7 events per window |
| **virtual time granted** | **hermit** | **≈1 pp near balance; inverts and saturates under real bias** |

## What went wrong on the way, recorded

- **`--sched-sticky-random-param` 0.5 and 0.99 crash hermit**:
  `thread caused non-unwinding panic. aborting.` /
  `Error: Sandbox container exited unexpectedly > Process exited with code: Signaled(SIGSEGV, true)`.
  0.0 and 0.9 ran. Observed on 2 of 4 values at one seed each; not characterised further.
- **The driver broke on the prefix `ADVANCE R`** and tore the run down mid-line, capturing
  `ADVANCE R1 me=20`. The counters are the entire measurement. It now requires a complete
  line with the trailing field.
- **One arm wedged before the workload** — capture 0 bytes, log stalled at 16 MB, stuck
  between finding the QEMU process and the QMP handshake, with no timeout covering that
  phase. Killed; 6 processes of mine reaped; directory back to 319 M.

## What is and is not answered

**Not answered: how faithfully hermit's steering transmits.** The regime where the question
matters — hermit actually biasing one guest CPU over the other — is exactly the regime
where the dependent variable goes to 0/100 and the meter inverts. Three x-axes have now
failed for one shared reason, which is itself the finding: *resource granted to a vCPU host
thread is not a measure of guest execution, in any currency hermit exposes, because an idle
guest CPU consumes the resource without running guest code.*

The falsification stated before running was: if requested and achieved bias do not track,
hermit's handle is coarse and the mission needs a different control path. **That is the
outcome here**, with the qualification that it is the *meter* that fails, not necessarily
the handle — the same distinction drawn in the host-CPU-seconds section, and it still
stands.

What would measure it: a quantity that counts guest instructions executed per vCPU rather
than resource granted to its host thread. Nothing hermit currently logs does that. It is
the same question the QEMU shared-memory channel would have to answer for itself.

n is small — 3 graded points, 2 saturated, one graded point without a census — and the
window ended with a wedged arm rather than a full sweep. The near-balance agreement rests
on two runs.
