# Two clocks, one silence: when a guest sees a time step its watchdogs do not

**Question.** A guest was observed taking a 30-second time step with no watchdog
firing. Does that transfer to hermit? Which clock does the guest's `sched_clock`
end up on under hermit's time virtualization, and can hermit move guest-visible
time without the watchdogs objecting?

**Short answers.**

1. The original observation is real and much larger than reported. Measured with
   both clocks sampled on the same line, a guest saw a **300-second** step while
   the watchdogs' clock advanced **17 milliseconds**, and nothing complained.
2. It requires KVM's paravirtual clock, and a single vCPU. **Under hermit this
   particular split cannot happen** — hermit runs QEMU under TCG, the guest boots
   "on bare hardware" with no paravirtual clock in existence, and both clocks are
   driven by the same QEMU virtual clock. Measured across five hermit
   configurations: the two clocks never separate.
3. **But the conclusion the task drew still stands, by a different route.** On
   hermit's own product surface — a program run directly under `hermit run` —
   `clock_gettime` and `rdtsc` disagree about how much time passed, by a factor
   that grows with `--clock-multiplier` and reaches **3.5×** at 1000. Hermit can
   move one guest-visible clock without another following. So watchdog silence,
   or any check reading a single clock, is not evidence that hermit's time
   manipulation is safe.

---

## 1. Methodology

### 1.1 Naming the two clocks

The claim is about two clocks disagreeing, so both must be named precisely and
read at the same instant.

| clock | what reads it | how this experiment samples it |
|---|---|---|
| **`sched_clock`** | the soft-lockup detector's "stuck for Ns" arithmetic, via `local_clock()` | the bracketed timestamp printk puts on every console line |
| **current clocksource** | userspace `CLOCK_MONOTONIC`, via timekeeping | `/proc/uptime`, read by the guest shell |

The instrument is one guest line carrying both:

```sh
echo "T $(cut -d' ' -f1 /proc/uptime)" > /dev/kmsg
```

which arrives on the console as

```
[   42.123456] T 42.12
```

The bracketed stamp is applied by printk **inside the kernel**; the value after
`T` was read from `/proc/uptime` by userspace immediately before the write. Every
line is therefore a simultaneous reading of the watchdog's clock and the guest's
own clock, and divergence is visible directly rather than inferred by comparing
separate runs.

### 1.2 Host, guest, tooling

Same host and guest as `experiments/drgn-execution-control-pause_20260821`:
`devbig014`, AMD EPYC 9D85, QEMU 10.1.2, guest kernel
6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79 from the hermit QEMU demo assets,
cold-booted. hermit 0.2.0 (2026-08-21, g3b6cd503ccd8) at
`hermit/target/release/hermit`. Host load 8–15 of 316 throughout.

Guest topology varies by experiment and is stated per result, because **it
changes the answer** (§2.2).

### 1.3 Locating the pause

An earlier version of this harness took the boundary as "last sample before the
drain, first sample after the resume". That is wrong when the console drain
misses a sample already in flight: the boundary then compares two *pre-pause*
samples and reports an ordinary sampling interval as though it were the pause. It
reported 0.021 s for a run whose log plainly shows both clocks stepping 30.76 s.

The measurement instead **locates the pause by its size** — the largest step in
either clock over the whole run — and reports how many large steps exist, which
must be exactly one. `gapscan.py` re-derives every number below from the raw logs
by that method, independent of the harness.

### 1.4 Controls

- **Positive control**: the same guest, same clocksource, paused with `SIGSTOP`
  instead of QMP. If the detector is live in that exact configuration it must
  fire — otherwise a silent QMP row means nothing.
- **Negative control**: the same guest on `kvm-clock` instead of `tsc`, where
  both clocks should freeze together and nothing should be observable.
- Every trial records console bytes emitted while paused; a guest that can still
  talk was not paused.

---

## 2. The guest result

### 2.1 A guest sees five minutes pass; the watchdogs see 17 milliseconds

1 vCPU, `-accel kvm`, `clocksource=tsc`, QMP `stop`/`cont`:

| pause held | **clocksource** (what the guest sees) | **`sched_clock`** (what watchdogs use) | complained |
|---|---|---|---|
| 30 s | **+30.52 s** | +0.019 s | – |
| 125 s | **+125.52 s** | +0.020 s | – |
| 300 s | **+300.52 s** | +0.017 s | – |

Exactly one large step per run. The guest's own clock tracks the pause to within
0.52 s at every duration; the clock the soft-lockup detector computes from does
not move at all.

**Positive control** — same guest, same `tsc` clocksource, `SIGSTOP` instead of
QMP:

| pause | clocksource | `sched_clock` | complained |
|---|---|---|---|
| 30 s | +30.56 s | **+30.56 s** | **RCU stall detector** |

Both clocks move, and the detector fires. So the detector is armed and working in
this configuration; the silence in the table above is the clock split, not a dead
watchdog.

**Negative control** — same guest, `kvm-clock` instead of `tsc`, QMP stop 30 s:
clocksource +0.030 s, `sched_clock` +0.022 s, no large step, no complaint. Both
frozen together, nothing to observe.

### 2.2 Why it needs one vCPU — and the loose end that closes

The previous experiment left an unexplained result: `clocksource=tsc` is honoured
at 1 vCPU but refused at 4. The guest says why in one line:

```
[    0.292685] tsc: Marking TSC unstable due to TSCs unsynchronized
[    0.848671] clocksource: Switched to clocksource kvm-clock
```

At 4 vCPUs the guest checks the vCPUs' TSCs against each other, finds them
unsynchronized, marks the TSC unstable and falls back to `kvm-clock` — so the
command-line override is itself overridden. Userspace confirms it:
`CS=kvm-clock` despite `clocksource=tsc` on the command line. At 1 vCPU there is
no second TSC to disagree with, the check has nothing to fail, and the override
takes.

This narrows the finding rather than broadening it: **the split reproduces only on
a single-vCPU guest on this host**, because a multi-vCPU guest here cannot stay on
the TSC. On 4 vCPUs, both `kvm` and `tcg` show both clocks moving together under
`SIGSTOP` (+30.76 / +30.53) and the detectors fire.

### 2.3 The mechanism

`sched_clock` and the clocksource are separate kernel facilities. Under
`CONFIG_KVM_GUEST` with a paravirtual clock present, `sched_clock` is the
paravirtual clock, which KVM corrects across a hypervisor-level pause. The TSC
clocksource is not corrected — it keeps counting with the host. Select `tsc` as
the clocksource and the two are reading different things, so a QMP `stop` moves
one and not the other. Under `SIGSTOP` the correction never happens, both move,
and the detector fires — which is what the positive control shows.

---

## 3. Under hermit, this split cannot occur

Five configurations, hermit wrapping QEMU, flags taken from the demo path in
`demos/lib/drgn_hermit.py` rather than invented here. The whole hermit process
group is `SIGSTOP`ped for 30 s — hermit is the tracer and QEMU its tracee, so
stopping QEMU alone would leave the tracer spinning.

| hermit profile | vCPUs | clocksource | clocksource step | `sched_clock` step | complained |
|---|---|---|---|---|---|
| demo (`--strict --no-rcb-time`, icount) | 1 | tsc-early | +0.030 s | +0.023 s | – |
| demo | 4 | tsc-early | +0.040 s | +0.033 s | – |
| demo + `--clock-multiplier 1000` | 1 | tsc-early | +0.030 s | +0.022 s | – |
| `--no-virtualize-time --no-virtualize-metadata` | 1 | tsc-early | +0.030 s | +0.022 s | – |
| demo | 1 (repeat) | tsc-early | +0.030 s | +0.023 s | – |

Zero large steps in every run: the two clocks never separate, and both are frozen
across the host pause.

**Which clock does the guest's `sched_clock` end up on under hermit?** The guest's
own boot log answers it:

```
Booting paravirtualized kernel on bare hardware
```

and zero occurrences of `kvm-clock` anywhere in the log. Hermit runs QEMU under
TCG, so the guest has no hypervisor paravirtual clock *in existence*. Its
`sched_clock` is therefore `native_sched_clock` reading the QEMU-emulated TSC, and
its clocksource is another QEMU-emulated device. Both are driven by QEMU's virtual
clock — by instructions retired when `-icount` is on. **There is no second,
independently-corrected clock for them to disagree about**, which is precisely the
ingredient the §2 split requires.

This is a structural argument backed by measurement, not by reasoning alone: the
absence of `kvm-clock` is read out of the guest, and the five rows above are
measured.

---

## 4. But hermit can move one clock without another — on its own product surface

The guest experiment asks about a guest inside QEMU. Hermit's actual product
surface is a program run directly under `hermit run`, and the same question
applies there: can the clocks hermit exposes disagree with each other?

A C probe (`clockprobe.c`) reads `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_RAW`,
`CLOCK_REALTIME`, `CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID`,
`getrusage`, and `rdtsc` — the last being a direct instruction, not a syscall —
with a fixed amount of deterministic work between samples.

**Under default virtual time every clock agrees**, and the run is bit-reproducible
(three runs, identical sha256): `CLOCK_MONOTONIC` 0.1518 s against `rdtsc`
0.1503 s over the same interval.

**Under `--clock-multiplier` they come apart, systematically:**

| multiplier | `CLOCK_MONOTONIC` | `rdtsc` (ns→s) | `getrusage` | monotonic ÷ rdtsc |
|---|---|---|---|---|
| none | 0.151800 | 0.150301 | 0.150301 | 1.010 |
| 1 | 0.151800 | 0.150301 | 0.150301 | 1.010 |
| 2 | 0.304600 | 0.300603 | 0.300601 | 1.013 |
| 10 | 1.552993 | 1.503008 | 1.503001 | 1.033 |
| 100 | 18.930063 | 15.030216 | 15.030141 | 1.259 |
| **1000** | **527.302155** | **150.303685** | 150.302935 | **3.508** |

`rdtsc` and `getrusage` scale exactly linearly with the multiplier — 0.1503 × m to
six digits. `CLOCK_MONOTONIC` does not: it runs ahead, and the excess grows
roughly as the square of the multiplier (0.0015, 0.004, 0.05, 3.9, 377.0).

At multiplier 1000, one deterministic execution doing identical work is told **527
seconds elapsed by `clock_gettime` and 150 seconds by its own `rdtsc`**. The
values are reproducible: two complete runs produced byte-identical output.

**No cause is claimed here.** The mechanism worth testing next is that a larger
multiplier produces proportionally more preemptions, each costing virtual time
that is itself multiplied — which would give the observed square term — but that
is a hypothesis, not a measurement, and is labelled as one.

### 4.1 An intermittent crash, reported without a diagnosis

Once, `hermit run --no-virtualize-time --no-virtualize-metadata
--clock-multiplier 1000 -- ./clockprobe` died with:

```
core::panicking::panic_cannot_unwind
reverie_process::clone::clone_with_stack::callback
thread caused non-unwinding panic. aborting.
Error: Sandbox container exited unexpectedly
     > Process exited with code: Signaled(SIGSEGV, true)
```

It did **not** reproduce: three identical retries succeeded, as did multipliers
10, 50, 100, 200 and 500 with the same flags, and multiplier 100000 with virtual
time on. So this is an intermittent crash with a captured stack, not a
flag-combination bug — recorded so the stack is not lost, not diagnosed.

Separately, `--no-virtualize-time` alone is refused with a clear message
(`also requires --no-virtualize-metadata`), which is the good behaviour.

---

## 5. Threats to validity

- **The §2 split is single-vCPU on this host only**, for the reason in §2.2. It
  should not be quoted as a general property of Linux guests.
- **The harness boundary bug in §1.3 was real** and would have inverted one row;
  every number in this report is re-derived from the raw logs by `gapscan.py`
  using the size-based method, and every run is checked to contain exactly one
  large step.
- **§3 is a negative result**, and negative results from a small set of
  configurations are weak. It is five configurations, one host, one guest kernel,
  one QEMU. What raises it above "we did not see it" is the structural reason —
  the guest has no paravirtual clock at all under hermit, read from its own boot
  log — but a configuration where hermit exposes KVM to the guest would need
  re-testing.
- **§4 measures hermit's clocks against each other, not against truth.** The
  claim is that they disagree, which needs no reference clock. Which of them is
  "right" is not established.
- Two of the hermit-plus-QEMU cells (`demo-no-icount`,
  `no-virtualize-time-no-icount`) did not produce 40 samples within 900 s and are
  absent from the table rather than reported as zeros.

---

## 6. Evaluation

The task's premise was that watchdog silence is not evidence of safety. That
premise survives, but the specific mechanism behind it does not transfer to
hermit, and the distinction is worth keeping sharp.

**The guest-level split is a KVM artifact.** It needs two independently-maintained
clocks where a hypervisor corrects one and not the other. Hermit runs its guests
under TCG on emulated hardware with no paravirtual clock in existence, so the
guest has only one time base and its two clock *facilities* both read it. That is
a real structural difference, not a lucky configuration — but it also means the
protection is incidental. It comes from hermit not being KVM, not from anything
hermit does to keep its clocks consistent.

**And hermit does not keep its clocks consistent.** The multiplier result shows two
clocks hermit itself virtualizes, disagreeing by 3.5× on the same execution, with
nothing in the system objecting. A program that cross-checks `clock_gettime`
against `rdtsc` — which is exactly what a TSC calibration loop, a watchdog, or a
benchmark harness does — will read those as inconsistent. The guest kernel in §2
is one instance of a general pattern: **a check reading one clock is blind to a
step in another, and hermit has more than one clock.**

The practical conclusion for anything built on hermit's time virtualization is
that "no watchdog fired" and "no timeout tripped" are not safety evidence. They
are evidence about one clock. A check that matters should compare at least two of
the clocks hermit exposes and assert they agree — which, per §4, they currently do
not under a multiplier.

**What would sharpen this**: establishing the cause of the `CLOCK_MONOTONIC`
excess (the preemption-cost hypothesis is testable by counting preemptions across
multipliers); testing whether `clock_multiplier` divergence also reaches a QEMU
guest's *own* clocks when hermit wraps QEMU with icount off, which is the one cell
this experiment could not complete; and deciding whether the multiplier's
behaviour is a defect at all, which is an owner call, not a measurement.

---

## 7. Reproducing

```sh
# The guest split, both clocks on one line
python3 two-clocks.py --modes kvm --clocksources tsc --holds 30,125,300 \
        --pause-mode qmp --cpus 1 --numa 1 --out results/twoclocks-tsc-big.json
# Positive control: same guest, SIGSTOP instead of QMP
python3 two-clocks.py --modes kvm --clocksources tsc --holds 30 \
        --pause-mode sigstop --cpus 1 --numa 1 --out results/twoclocks-tsc-sigstop.json
# hermit wrapping QEMU
python3 hermit-two-clocks.py --profiles demo,demo-multiplier-1000,no-virtualize-time \
        --holds 30 --cpus 1 --out results/hermit-tc-profiles.json
# hermit's own clocks against each other
gcc -O2 -o clockprobe clockprobe.c
python3 ratio.py none 1 2 10 100 1000
# Re-derive every number above from the raw logs, independent of the harness
python3 gapscan.py runs/*/*/serial.log
```

Committed: the harness, the per-trial JSON, `clockprobe.c`, and `GAPSCAN.txt` —
the size-based re-derivation of every paired log, which is the evidence the tables
rest on. Not committed: the raw serial logs, for the reason given in the
neighbouring experiment — they are overwhelmingly identical boot output, and
`gapscan.py` regenerates the result from any rerun.
