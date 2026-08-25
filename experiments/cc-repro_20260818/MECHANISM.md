# Does host-thread sequentialization serialize guest vCPUs?

Task: `does-sequentialization-serialize-guest-vcpus`. Investigation only; no
repository files changed.

**No.** Hermit alternates between QEMU's two vCPU threads freely. The absence of
guest-visible interleaving has a different cause: **the guest receives almost no
timer interrupts, so its kernel never preempts.**

| item | value |
| --- | --- |
| hermit | `770b95c505`, binary sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5` |
| QEMU | 10.1.2, `-smp 2 -accel tcg,thread=multi`, no `-icount` |
| guest | 6.17.13, cmdline adds `tsc=nowatchdog no_timer_check` |
| harness | `cc_qemu.py` (snapshot + resume), `guest/order*.c` detectors |

## 1. Hermit does alternate between the vCPU threads

Read from Hermit's own `COMMIT` records in a 379,326,519-byte `--log info`
capture of one resume (`--strict --chaos --seed 1`). A `COMMIT` is one scheduler
turn, so the dettid sequence across them *is* the realized host-thread schedule.

```
total scheduler turns   14,245
total host-thread switches  8,952
turns   dtid 3 = 9,510   dtid 7 = 3,300   dtid 9 = 1,266
        dtid 11 = 108    dtid 13 = 42     dtid 5 = 19
```

Identifying the vCPU threads by final retired-branch count (a thread executing
translated guest code retires vastly more than a helper):

| dtid | final rcbs | role |
| --- | --- | --- |
| 11 | 852,203,454 | **vCPU thread** |
| 13 | 170,665,967 | **vCPU thread** |
| 3 | 37,463,390 | QEMU main loop (52,520 `clock_gettime`, 5,913 `ppoll`) |
| 5, 7, 9 | 0 | helpers (futex/write/madvise) |

Restricting the turn sequence to `{11, 13}`:

```
turns on vCPU threads: 150   (11: 108, 13: 42)
alternations between them: 42
pattern: 11x2 13x1 11x2 13x2 11x2 13x3 11x1 13x2 ... 11x7 13x16 11x28 ... 11x22
```

Each guest CPU got ~21 separate execution blocks — in the very run whose
guest-visible result was `switches=1`.

## 2. The absence survives removing vCPU alternation

Pinning both guest processes to **one** guest CPU takes Hermit's vCPU
alternation out of the picture: any interleaving must then come from the guest
kernel timeslicing two runnable processes.

| detector | native | hermit `--chaos` |
| --- | --- | --- |
| one process per guest CPU | 1080–1522 switches | **1** |
| **both on guest CPU0** | 3 switches | **1** |

`AFF rc=0 parent_cpu=0` under Hermit, so `sched_setaffinity` succeeds and the
pinning is real. Since the same-CPU case is unaffected by vCPU alternation and
still shows no interleaving, vCPU alternation cannot be the cause.

## 3. The cause: the guest gets essentially no timer interrupts

`/proc/interrupts` read in-guest immediately before and after the workload.

Short workload (2,000 slots):

| | LOC before | LOC after | delta | uptime delta | switches |
| --- | --- | --- | --- | --- | --- |
| native | 1824 / 1541 | 1901 / 1597 | **+77 / +56** | 0.05 s | 1471 |
| hermit | 1778 / 1485 | 1778 / 1485 | **0 / 0** | **2.0 s** | 1 |

Long workload (20,000 slots):

| | LOC before | LOC after | delta | uptime delta | switches |
| --- | --- | --- | --- | --- | --- |
| native | 1843 / 1553 | 2543 / 2223 | **+700 / +670** | 0.68 s | 19,370 |
| hermit | 1776 / 1485 | 1780 / 1490 | **+4 / +5** | **229.19 s** | 2 |

Native tick rate: 700 / 0.68 s ≈ **1,029/s** (HZ=1000, as expected).
Hermit tick rate: 4 / 229.19 s ≈ **0.017/s**. A deficit of roughly **57,000×**.

**Guest interleaving tracks ticks, not work.** Ten times the workload bought one
extra switch (1 → 2), because it bought a handful of extra ticks (0 → 4).
Natively the same tenfold increase took switches from ~1,470 to 19,370.

This is also why `--max-timeslice` did nothing: that knob changes how often
*Hermit* preempts QEMU's host threads; it has no effect on how often QEMU's
emulated APIC timer interrupts the *guest*, and the guest kernel cannot preempt
a running task without that interrupt.

## Where the fix belongs

QEMU derives the guest's APIC/PIT from host `clock_gettime` — under Hermit the
coordinator clock — while the guest's own elapsed time comes from `rdtsc`, the
per-thread synthetic clock. Those are the two domains from
`cc-qemu-coherent-timebase`. The guest's clock races while the device meant to
interrupt it barely fires.

This is the fourth consequence of that defect on this configuration, after
`no_timer_check`, `lpj=999964` and `tsc=nowatchdog` — and the worst, because it
removes the guest scheduler's ability to preempt at all.

**Not shown:** that fixing timer delivery will produce steerable guest
interleaving. That is the obvious next prediction and it is untested. What is
shown is that the current absence has a known cause with a filed fix, rather
than being a structural property of Hermit's scheduler.

## Files

| file | what |
| --- | --- |
| `cc_qemu.py` | boot / snapshot / resume; `--hermit-log`, `--hermit-log-file` added for this task |
| `guest/order.c` | order-recording detector (one process per guest CPU) |
| `guest/order_long.c` | 10× longer variant |
| `guest/order_same.c` | both processes pinned to guest CPU0 — the control that redirected this investigation |
| `guest/order_cpu.c` | reports `sched_setaffinity` return value and the parent's CPU |
| `tick.sh` | reads `/proc/interrupts` either side of the workload |
| `commit-seq.txt`, `vcpu-seq.txt` | extracted scheduler turn sequences |
