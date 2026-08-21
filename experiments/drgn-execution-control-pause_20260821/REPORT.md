# Pausing a Linux guest under QEMU: what the guest can perceive, and what complains

**Question.** Can a running Linux VM be paused at an arbitrary point, have real
kernel state read out of it, and be resumed — and what duration of pause makes
Linux break, by which mechanism?

**Short answer.** A QEMU-level pause is not something the guest can perceive, at
any duration up to the 300 s tested, because QEMU freezes the guest's clocks
across it. Break it with a pause QEMU cannot compensate for and the guest's
watchdogs do fire — but *not monotonically in the pause length*, because the
guest's clocksource wraps. On a 4-vCPU TCG guest using HPET, a **86-second pause
is invisible while a 21-second pause is not**.

This supersedes the single-vCPU experiment on branch
`cloudlint/qemu-pause-duration-sweep` (`165c3496`), whose qualitative conclusions
hold but which contained two measurement defects, both documented in
[Threats to validity](#7-threats-to-validity).

---

## 1. Methodology

### 1.1 Host

| | |
|---|---|
| Machine | `devbig014.atn7.facebook.com`, AMD EPYC 9D85, 316 logical CPUs, 1010 GiB RAM |
| Host kernel | Linux 6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79 |
| QEMU | 10.1.2 (`qemu-10.1.2-1.4.hs+fb.el9`), `/usr/bin/qemu-system-x86_64`, accelerators `tcg` and `kvm` |
| drgn | 0.2.0 (Python 3.12.13, elfutils 0.191, with libkdumpfile, lzma, pcre2) |
| Host load | 7–15 of 316 throughout; concurrent agents held ≤ 4 unrelated 1-vCPU QEMUs and one serial test harness |

Trials ran up to nine guests concurrently. Each guest is 1–4 vCPUs against 316
host CPUs, so no trial was competing for a core.

### 1.2 Guest

Same kernel image and initramfs as the hermit QEMU demos
(`ignored/qemu-linux/bzImage`, `initramfs.cpio.gz`), booted cold — **not**
restored from the demo snapshot.

| | |
|---|---|
| Guest kernel | 6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79 |
| Topology | 4 vCPUs, 2 sockets × 2 cores, 2 guest NUMA nodes, 1024 MiB (512 MiB per node) |
| Machine type | `q35` |
| Userspace | busybox initramfs; the workload arrives on a virtio-blk disk, which is what this `/init` expects |

**The `-smp 1` constraint from the slowdown work did not apply here, and the
reason matters.** That constraint came from the demo *snapshot's* saved device
model: a snapshot can only be restored into the machine it was taken from. This
experiment cold-boots, so there is no saved device state to be incompatible
with, and 4 vCPUs are available. Independently, a peer experiment
(`experiments/icount-smp_20260820`, commit `1cf0bc87`) established that `-icount`
alone permits multiple vCPUs; only `rr=record` and `thread=multi` refuse. Our
icount arm running 4 vCPUs is consistent with that.

**The vCPU count is confirmed from inside the guest, not from the flag:**

```
smpboot: x86: Booting SMP configuration:
smp: Brought up 2 nodes, 4 CPUs
smpboot: Total of 4 processors activated (12381.39 BogoMIPS)
futex hash table entries: 512 (32768 bytes on 2 NUMA nodes, total 64 KiB, linear)
```

and from userspace: `online=0-3`, `present=0-3`, `nproc=4`, four `cpuN` rows in
`/proc/stat`, two entries under `/sys/devices/system/node/`.

### 1.3 Watchdog thresholds, read from the guest

Not assumed from a host config — read out of the running guest:

```
watchdog_thresh         10     -> soft-lockup detector warns at 2x, i.e. 20 s
rcu_cpu_stall_timeout   21
hung_task_timeout_secs  120
nmi_watchdog            0      -> the HARD-lockup detector is not armed
softlockup_panic        0      -> soft lockup warns; it does not panic
```

### 1.4 The two pause mechanisms

**QMP `stop`/`cont`** is the mechanism the owner asked about: QEMU is running and
deliberately suspends the guest.

**`SIGSTOP`/`SIGCONT` of QEMU's process group** is the negative control, and a
fair second mechanism in its own right — it is what an OOM-throttled,
descheduled, or migrated host does to a VM. QEMU cannot freeze the guest's clock
if QEMU itself is not executing. Without this arm, "no watchdog complained" in
every QMP row would be indistinguishable from a detector that cannot see
anything.

### 1.5 Workloads

An idle NO_HZ CPU touches the watchdog on idle exit, so an idle guest cannot
exercise the soft-lockup detector at all. Two workloads:

- **spin** — one busy loop per vCPU (`nproc` of them), plus a 1 Hz heartbeat.
- **rcu** — the same, plus two syscall-heavy loops. An RCU stall can only be
  *reported* if a grace period is in flight across the pause, so a guest that
  never enters the kernel cannot exercise that detector.

Every heartbeat carries the guest's own load average and run-queue length, so
the load **at the instant of the pause** is recorded per trial rather than
asserted once at startup. Measured across 104 trials: 5 runnable tasks median
(4 spinners + the heartbeat shell), min 5, max 9.

### 1.6 Observables and their resolution

This is where the previous report went wrong, so it is stated explicitly.

| observable | how time is stamped | sampling period |
|---|---|---|
| A — 1 Hz heartbeat | shell reads `/proc/uptime`, `sleep 1` | 1.0 s |
| B — tight loop | same, no sleep | ~10 ms (`/proc/uptime` granularity) |
| C — kernel printk | guest writes `/dev/kmsg`; **printk stamps it inside the kernel** | 0.4 ms unthrottled, 1 ms throttled |

Observable C is the trustworthy one: the timestamp is applied by the kernel, so
it does not depend on the shell, on scheduling, or on userspace reading a clock.
**A measurement cannot resolve a gap smaller than its own sampling period** —
the trap that produced the "1.01 s" figure in the previous report.

---

## 2. Why 1.01 s?

The owner's question: a 30-second QMP `stop` moved kvm-clock guest uptime by
1.01 s. Not zero, not thirty. Why the middle?

**Because 1.01 s was the measurement floor, not anything the guest did.** The
previous metric was "largest step in guest uptime between consecutive
heartbeats", and the heartbeat loop was `while :; do echo ...; sleep 1; done`.
A 1 Hz sampler cannot report a gap smaller than 1 s. Zero was unreachable by
construction, and "1.01" is one ordinary heartbeat interval carrying no
information about the pause at all.

The same 30 s pause, same guest, three resolutions:

| observable | sampling period | guest time across the pause |
|---|---|---|
| A — 1 Hz heartbeat | 1.0 s | **1.01 s** |
| B — tight loop | ~10 ms | **0.0 s** |
| C — kernel printk | 406 µs | **430 µs** |

A collapses to B collapses to C, exactly as it must if the figure is the
sampling interval.

**And the residual is a fixed cost, not a rate — which settles it.** If any clock
were still running during the pause, the residual would scale with the hold. It
does not:

| hold | guest time across pause | ratio to hold |
|---|---|---|
| 5 s | 393 µs | 7.9 × 10⁻⁵ |
| 30 s | 425 µs | 1.4 × 10⁻⁵ |
| 120 s | 412 µs | 3.4 × 10⁻⁶ |
| 300 s | 404 µs | 1.4 × 10⁻⁶ |

Flat at ~0.4 ms across a 60× range, and equal to the loop's own free-running
period (404–432 µs). **The guest-visible cost of a QMP `stop` is one ordinary
loop iteration**, at every duration from 5 seconds to 5 minutes. What little
remains is the fixed cost of stopping and restarting the vCPUs.

---

## 3. Pause duration versus what complained

4 vCPU, 2 NUMA, spin workload unless noted. `guest saw` is the guest's own
uptime step at 1 Hz, so any value ≤ ~1.05 s is at the sampling floor.

### 3.1 QMP `stop` — nothing complains, in any mode, at any duration

| backend | 3 s | 10 s | 21 s | 30 s | 60 s | 125 s | 300 s |
|---|---|---|---|---|---|---|---|
| kvm | – | – | – | – | – | – | – |
| tcg | – | – | – | – | – | – | – |
| icount | – | – | – | – | – | – | – |

Guest saw ≤ 1.3 s in all 21 trials. Not one complaint of any kind.

### 3.2 `SIGSTOP` — the curve exists

| pause | kvm (kvm-clock) | tcg (hpet) |
|---|---|---|
| ≤ 18 s | – | – |
| 19 s | – (0/3) | soft-lockup (1/3) |
| 20 s | soft-lockup (2/4) | soft-lockup (2/4) |
| 21 s | soft-lockup (2/3) | soft-lockup (3/3) |
| 22 s | soft-lockup (4/4) | soft-lockup (4/4) |
| 23 s | soft-lockup (3/3) | soft-lockup (3/3) |
| 25–125 s | soft-lockup | **see §4 — not monotonic** |

The first-fire boundary straddles the 20 s that `watchdog_thresh=10` predicts. It
is deliberately **not** quoted as a sharp edge: the watchdog hrtimer samples every
`2/5 × watchdog_thresh` = 4 s, so whether a given pause crosses depends on where
it lands relative to the last watchdog touch.

### 3.3 Which mechanism complains depends on the workload

With one spinner per vCPU, only the **soft-lockup detector** ever spoke. With the
syscall-heavy workload the **RCU stall detector** joins it from 22 s:

```
rcu: INFO: rcu_sched self-detected stall on CPU
rcu: rcu_sched kthread timer wakeup didn't happen for 5251 jiffies! ...
rcu: Unless rcu_sched kthread gets sufficient CPU time, OOM is now expected behavior.
```

and at 125 s the **workqueue watchdog** as well:

```
BUG: workqueue lockup - pool cpus=0 node=0 flags=0x0 nice=0 stuck for 32s!
```

**The hung-task detector never fired**, including past its 120 s threshold, and
the paused drgn read says why rather than leaving it a guess: the guest holds
**zero tasks in `TASK_UNINTERRUPTIBLE`** (77 `TASK_IDLE`, 44 interruptible, 5
runnable of 126). `check_hung_task` counts only the first state and explicitly
skips `TASK_IDLE`. That is a property of the workload, not a bound on the pause.

**The hard-lockup detector cannot be the mechanism here**: the guest reports
`nmi_watchdog=0` and the kernel logs `NMI watchdog: Perf NMI watchdog permanently
disabled` under TCG.

---

## 4. The main SMP finding: the answer is a sawtooth, not a threshold

Under TCG the sweep produced a curve that made no sense — 20–30 s fire, 45 s and
60 s say nothing, 125 s fires again. Reproduced with the observation window
widened to 90 s in case the message was merely late. It is not late; it is
absent, and the guest's own uptime step says why.

**With 4 vCPUs the TCG guest selects HPET, where the 1-vCPU guest selected TSC.**
From the guest's boot log:

```
hpet0: 3 comparators, 64-bit 100.000000 MHz counter
clocksource: hpet: mask: 0xffffffff max_cycles: 0xffffffff, max_idle_ns: 19112604467 ns
clocksource: Switched to clocksource hpet
```

The counter is 64-bit in hardware but Linux drives it through a **32-bit mask at
100 MHz**, so it wraps every 2³²/10⁸ = **42.9497 s**. A pause longer than that
wraps, and the guest computes `pause mod 42.95`:

| pause | predicted | measured | complained? |
|---|---|---|---|
| 30 s | 30.00 s | 30.44 s | soft-lockup |
| **43 s** | 0.05 s | 1.05 s, 1.05 s *(at the 1 Hz floor)* | **no** |
| 44 s | 1.05 s | 1.44 s | no |
| 45 s | 2.05 s | 2.11 s, 2.11 s, 3.07 s | no |
| 60 s | 17.05 s | 17.40 s, 17.73 s, 17.84 s | no |
| **86 s** | 0.10 s | 1.05 s, 1.05 s *(at the floor)* | **no** |
| 88 s | 2.10 s | 2.80 s | no |
| 125 s | 39.10 s | 39.56 s | soft-lockup |

Every row within ~0.5 s, with the same small positive offset already present in
the 30 s row. This is not a fit with free parameters: the wrap period is read off
the guest's own dmesg and the modulus is arithmetic.

**Confirmed independently on a second clocksource.** `acpi_pm` is 24-bit at
3.579545 MHz and wraps every **4.687 s**:

| pause | predicted | measured | complained? |
|---|---|---|---|
| 5 s | 0.31 s | 1.05 s *(floor)* | no |
| 10 s | 0.63 s | 1.05 s *(floor)* | no |
| 21 s | 2.25 s | 2.34 s | no |
| 30 s | 1.88 s | 2.63 s | no |

**On `acpi_pm` no pause of any length can trip the soft-lockup detector**, because
the guest can never compute more than 4.687 s of elapsed time.

**kvm is not affected**: kvm-clock is a 64-bit paravirtual counter with nothing to
wrap, and its SIGSTOP rows track the pause 1:1 monotonically to 125 s.

The practical consequence: **"how long can I pause before Linux notices" is not
monotonic in the pause length.** It is aliased by the clocksource's wrap period.

---

## 5. Clock modes by backend

### 5.1 What QEMU 10.1.2 offers, and what each backend accepts

| flag | values | kvm | tcg |
|---|---|---|---|
| `-accel` | `tcg`, `kvm` | ✔ | ✔ |
| `-accel tcg,thread=` | `single`, `multi` | n/a | ✔ (`multi` refuses with icount) |
| `-icount shift=` | `0`…`N`, `auto` | **rejected** | ✔ |
| `-icount sleep=` | `on`, `off` | rejected | ✔ |
| `-icount align=` | `on`, `off` | rejected | ✔, but **incompatible with `sleep=off`** |
| `-icount rr=` | `record`, `replay` | rejected | ✔ at `-smp 1` only |
| `-rtc clock=` | `host`, `rt`, `vm` | ✔ | ✔ |
| `-rtc base=` | `utc`, `localtime`, datetime | ✔ | ✔ |
| `-rtc driftfix=` | `none`, `slew` | ✔ | ✔ |

Refusals are hard errors, quoted verbatim:

```
-icount is not allowed with hardware virtualization
-accel tcg,thread=multi: No MTTCG when icount is enabled
-icount shift=3,sleep=off,align=on: align=on and sleep=off are incompatible
Record/replay is not supported with multiple CPUs      (from experiments/icount-smp_20260820)
```

**So the icount rows are TCG-only by construction, not by choice.** This is the
central cross-product fact: the one mode that decouples guest time from host time
is exactly the one hardware virtualization cannot have.

### 5.2 Guest-visible time across a 30 s `stop`/`cont`

Measured with observable C at 1 kHz; the sampling period is quoted because it is
the floor for each row.

| mode | clocksource in use | across a 30 s QMP pause | sample period |
|---|---|---|---|
| `kvm` default | kvm-clock | 2.0 ms | 2.5 ms |
| `kvm -rtc clock=rt` | kvm-clock | 4.4 ms | 2.4 ms |
| `kvm -rtc clock=vm` | kvm-clock | 2.9 ms | 2.5 ms |
| `kvm -rtc driftfix=slew` | kvm-clock | 3.8 ms | 2.5 ms |
| `tcg` default | hpet | 10.1 ms | 6.0 ms |
| `tcg,thread=multi` | hpet | 15.9 ms | 7.0 ms |
| `tcg -rtc clock=vm` | hpet | 9.2 ms | 6.8 ms |
| `tcg -icount shift=3,sleep=off` | hpet | 58.9 ms | 56.6 ms |
| `tcg -icount shift=3,sleep=on` | hpet | 39.7 ms | 39.0 ms |
| `tcg -icount shift=auto` | hpet | 20.6 ms | 19.4 ms |
| `tcg -icount + -rtc clock=host` | hpet | 42.8 ms | 41.5 ms |
| `tcg -icount ...,align=on` | — | **refused by QEMU** | — |

Every row is at its own sampling floor. **No QEMU clock mode lets a guest see a
QMP pause**, and none produced a complaint.

### 5.3 The same modes under `SIGSTOP`, which separates them completely

| mode | across a 30 s SIGSTOP | complained |
|---|---|---|
| `kvm` default | **30.50 s** | RCU stall, soft-lockup |
| `tcg` default | **30.55 s** | RCU stall, soft-lockup |
| `tcg,thread=multi` | **30.52 s** | soft-lockup |
| `tcg -icount sleep=off` | 43.5 ms | – |
| `tcg -icount sleep=on` | 37.0 ms | – |
| `tcg -icount shift=auto` | 74.9 ms | hrtimer overrun |

**This is the table that answers the confirm.** Under `-icount` the guest cannot
see a pause even when QEMU is not running to hide it, because guest time is a
function of instructions retired and a stopped QEMU retires none. Every other
mode leaks the full 30 s.

### 5.4 The guest's clocksource is a third axis, and it matters

A table indexed only by QEMU flags would be wrong. Forcing the clocksource:

| backend | requested | actually in use | across a 30 s QMP pause |
|---|---|---|---|
| kvm | *(default)* | kvm-clock | 2.0 ms |
| kvm | `tsc` | **kvm-clock** — override refused | 6.9 ms |
| kvm | `hpet` | hpet | 2.4 ms |
| kvm | `acpi_pm` | acpi_pm | 4.0 ms |
| tcg | *(default)* | hpet | 10.8 ms |
| tcg | `acpi_pm` | acpi_pm | 9.6 ms |
| tcg + icount | `acpi_pm` | acpi_pm | 38.9 ms |

Note row 2: on the 4-vCPU guest, `clocksource=tsc` does **not** take under kvm —
the kernel stays on kvm-clock. On the 1-vCPU guest it did take, and a 30 s pause
then moved guest uptime by 30.02 s with no watchdog firing (because the watchdogs
run on `sched_clock`, still the paravirtual clock). That single-vCPU result stands
but does not reproduce at 4 vCPUs, and the reason the override is refused is not
established here.

---

## 6. The capability: pause at a random point, read with drgn, resume

`pause-and-read-smp.py`, 4 vCPU / 2 NUMA kvm guest. Five pauses at random points:
QMP `stop` → drgn walks `init_task.tasks` → QMP `cont`.

```
pause 1: held 2.45s  tasks=125  reads=11027 (30.0 MiB)  quiet=0  cpu{0:28, 1:17, 2:42, 3:38}
pause 2: held 2.04s  tasks=126  reads=11093 (27.2 MiB)  quiet=0  cpu{0:27, 1:19, 2:43, 3:37}
pause 3: held 2.04s  tasks=126  reads=11093 (27.2 MiB)  quiet=0  cpu{0:26, 1:19, 2:44, 3:37}
pause 4: held 2.03s  tasks=126  reads=11093 (27.2 MiB)  quiet=0  cpu{0:27, 1:18, 2:44, 3:37}
pause 5: held 2.04s  tasks=126  reads=11103 (27.2 MiB)  quiet=0  cpu{0:27, 1:17, 2:43, 3:39}
```

Each read walks 125–126 tasks with pid, comm, decoded state, **and the vCPU each
last ran on** — a column that is uniformly 0 on a single-vCPU guest and says
nothing there. Tasks are spread across all four vCPUs. `quiet=0` is the guard:
zero console bytes emitted while paused. The guest's own uptime never stepped by
more than 1.01 s (the 1 Hz floor) across any of them, and no watchdog complained.

No drgn execution-control API is involved anywhere — the pause and resume are
QEMU's, as the owner specified.

### 6.1 One thing had to be fixed to read a NUMA guest at all

`demos/lib/drgn_hermit.py` supplies the whole inspection path (vmlinux
extraction, VMCOREINFO scan, kallsyms lookup, `init_task` registration) and is
imported unmodified. But its `_ram_region` assumes **one** host mapping for guest
RAM, and a NUMA guest has **one per node** — each `memory-backend-ram` is its own
mmap. Guest physical memory is split across them.

Worse, the obvious repair is wrong. Verified on a live 2-node guest:

```
region 0 0x7f3423c00000: bytes@phys0x1000000=a48e7d84ffffffff  kallsyms_num_syms=0
region 1 0x7f3443e00000: bytes@phys0x1000000=66900faee8e90141  kallsyms_num_syms=166293
```

**QEMU backs guest-physical node 0 with the *higher* host mapping**, so ordering
the mappings by address gets it exactly backwards. And a VMCOREINFO-shaped blob
is present in *both* mappings, so "did VMCOREINFO parse" does not discriminate —
the first version of this code accepted the wrong mapping on that basis and every
symbol read faulted. The check now reads `kallsyms_num_syms` through the candidate
and requires a plausible count; the wrong ordering returns 0 and is rejected.

---

## 7. Threats to validity

**Two measurement defects were found and fixed during this work.** Both were
caught by internal inconsistency, and both are the same shape: an instrument
reporting its own artifact as a result.

1. **The 1 Hz sampling floor** (§2). Reported as a property of the guest in the
   previous experiment. It was the sampler's period.

2. **Console backlog read as post-resume output.** "Guest time across the pause"
   compared the last console sample before the pause with the first after the
   resume — but the console socket holds a backlog, so the "first sample after
   resume" was often written *before* the pause and merely queued. It reported
   0.0004 s for a case where the guest had plainly jumped 30 s, which is how it
   was caught. The fix: stop the guest, drain the socket completely (safe and
   bounded — a stopped guest writes nothing more), and only then take the
   boundary. The same case then reads 30.68 s. **The §2 conclusion was re-run
   with the drain in place and is unchanged**, so it never depended on the bug.

   A related trap: the unthrottled kernel-log probe emitted 1.7 MB during the
   settle window, overflowed the console badly enough to truncate a line
   mid-word, and outran the drain. It is throttled to 1 kHz for the clock-mode
   matrix, where the question is "0 or 30 s?" and 1 ms resolves it by four
   orders of magnitude.

**Guards carried forward from the previous experiment**, which are why that result
was believable:

- **The SIGSTOP negative control.** Every QMP arm reports "nothing complained";
  without a control that is indistinguishable from a blind detector. It fires.
- **"Nothing complained" established by exhaustive diff, not a pattern list.**
  All 104 serial logs were normalised and pooled; the 318 messages common to
  every run form the boot set; every message outside it was listed and
  attributed. The only complaint-class messages in the whole corpus are the
  soft-lockup, RCU-stall, workqueue and hrtimer reports already classified above,
  and **no QMP-pause run produced any of them**. Nothing was missed by a regex.
- **Every trial demonstrably stopped the guest**: zero console bytes emitted
  while paused, in all 104.

**Remaining limits, stated rather than papered over:**

- One kernel, one machine type, one initramfs. 4 vCPUs and 2 NUMA nodes, not 64.
- 300 s is the longest QMP pause tried. "Never breaks" means "not up to 300 s".
- The 1 Hz heartbeat is still the observable in the duration sweeps (§3, §4), so
  values ≤ 1.05 s there mean "at or below the floor", not "zero". The clock-mode
  matrix and §2 use the microsecond observable.
- Why `clocksource=tsc` is refused under kvm at 4 vCPUs but accepted at 1 vCPU is
  not established.
- **hermit is deliberately absent from this experiment.** The variable under test
  is the guest's time source, and hermit virtualizes time for QEMU as well, which
  would confound the axis. Hermit can only decouple guest time from host time
  further, never re-couple it, so the icount immunity measured here is a *lower
  bound* on the hermit-wrapped case. That last step is reasoning, not measurement.

---

## 8. Evaluation

The experiment set out to find a duration at which Linux breaks. What it found is
that the duration is the wrong independent variable twice over.

**First, the pause mechanism dominates it.** A QMP `stop` of five minutes costs
the guest 0.4 ms of perceived time — one loop iteration — while a `SIGSTOP` of 21
seconds trips a watchdog. Between the two the ratio of perceived to real elapsed
time differs by six orders of magnitude, and no amount of varying the duration
moves the QMP arm.

**Second, where the mechanism does let time through, the clocksource aliases it.**
A 32-bit HPET at 100 MHz wraps every 42.95 s, so guest-perceived time is
`pause mod 42.95` and the visibility curve is a sawtooth. An 86-second pause is
invisible to a guest that a 21-second pause upsets. On a 24-bit `acpi_pm` the wrap
is 4.687 s and *no pause of any length* can trip the soft-lockup detector. Any
statement of the form "Linux tolerates pauses up to N seconds" is therefore
incomplete without naming the clocksource — and this was invisible to the
single-vCPU experiment, because 4 vCPUs is what moved the guest off TSC and onto
HPET.

**For the drgn execution-control work the practical conclusion is favourable.**
The inspect-pause-resume loop is free at the timescales a debugger needs: a
human-scale or search-loop-scale pause costs the guest well under a millisecond
under `kvm`, and under `-icount` it costs nothing measurable even when QEMU is not
running to hide it. The 20 s soft-lockup threshold is not a budget the debugger
has to stay inside, because a QMP pause never spends against it.

**What would sharpen this further**, in order of value: a guest with 8+ vCPUs and
a real distro userspace, where the hung-task detector actually has
`TASK_UNINTERRUPTIBLE` work to observe; the same sweep with hermit wrapping QEMU,
to convert the lower-bound reasoning in §7 into a measurement; and pinning down
why the `tsc` clocksource override is refused at 4 vCPUs under kvm, which is a
loose end this experiment created rather than resolved.

---

## 9. Reproducing

```sh
python3 smoke_smp.py                 # confirm 4 vCPU / 2 NUMA and read watchdog settings
python3 sweep_smp.py --arm kvm --mode sigstop --durations 19,20,21,22,23 \
        --observe 45 --label fine --out results/fine-kvm.json
python3 clock-modes.py --hold 30 --pause-mode qmp --out results/clock-modes-qmp.json
python3 why-redo.py kvm 30           # the three-resolution comparison
python3 why-scaling.py kvm 5,30,120,300
drgn -q -p $$ pause-and-read-smp.py --arm kvm --numa 2 --pauses 5
python3 summarize_smp.py             # every table above, derived from the JSON
python3 bootset_diff.py              # the exhaustive-diff guard
python3 extract_evidence.py          # EVIDENCE.md from the raw logs
```

### What is committed, and what is not

Committed: the harness, the per-trial JSON (200 KiB, 104 trials), the derived
tables (`results/SUMMARY.txt`), the exhaustive-diff output
(`results/BOOTSET-DIFF.txt`), and `EVIDENCE.md` — 161 excerpts giving the exact
kernel wording of every named complaint with the run it came from.

Not committed: the raw serial logs, **271 MB** across 104 runs, nearly all of it
identical boot output and heartbeat lines. `EVIDENCE.md` carries the lines that
actually support the result, at 64 KB. Any log can be regenerated by rerunning
the command above.
