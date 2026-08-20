# Does `-icount` alone permit multiple vCPUs?

Agent: icountsmp (claude coordinator subagent) · task
`prove-whether-icount-alone-allows-multiple-vcpus` · 2026-08-20 · host devbig014

**VERDICT: instruction counting permits multi-vCPU guests. `-icount` alone imposes no
CPU-count limit; the only two restrictions are on record/replay (which forbids >1 CPU)
and on MTTCG (which is refused with icount, forcing round-robin — round-robin runs as
many vCPUs as asked). A guest booted with `-icount shift=0 -smp 2` sees two CPUs, brings
both online, spawns CPU1's per-CPU kernel threads, runs guest code on both, and completes
cross-CPU IPIs and TLB shootdowns between them.**

Cost, measured below: **1.73x on boot, 4.18x on the compute-dominated part**, both against
the same round-robin accelerator with icount switched off.

## What was run

Everything below is bare QEMU 10.1.2 (no hermit), kernel
`/home/newton/work/dev-hermit/ignored/qemu-linux/bzImage`, a busybox initramfs whose
`init` is the evidence collector, `-m 512M -display none -serial file:... -monitor none
-no-reboot`.

Every arm was run on **both** binaries, and they agree:

- stock `/usr/bin/qemu-system-x86_64` — `QEMU emulator version 10.1.2 (qemu-10.1.2-1.4.hs+fb.el9)`
- source-built `/home/newton/work/qemu-vcpu-channel_20260820/build/qemu-system-x86_64` — `10.1.2`

The source-built one carries the local vCPU-scheduling-channel patch. That patch only
diverges from stock when `QEMU_VCPU_SCHED_CHANNEL` is set, and it was never set here — but
running both anyway is what rules the patch out as an explanation.

The guest load is `spin`, a freestanding (`-nostdlib`) fixed-iteration program built for a
generic x86-64. It is new because the neighbouring experiment's prebuilt `worker2` was
compiled for this host's Zen 5 and dies inside the guest with
`traps: worker2[94] trap invalid opcode ip:427398` — a silently zero-load run. `spin CPU N`
pins with `sched_setaffinity`; `spin -1 N` does not pin, and both report `sched_getcpu()`
on exit.

## 1. `-icount shift=0 -smp 2`, no `rr=` — does it boot, does the guest see two CPUs?

```
/usr/bin/qemu-system-x86_64 -accel tcg,thread=single -smp 2 -icount shift=0,sleep=off \
  -m 512M -kernel bzImage -initrd initramfs.cpio.gz \
  -append 'console=ttyS0 panic=-1 rdinit=/init iters=100000000' \
  -display none -serial file:... -monitor none -no-reboot
```

`exit=0 wall=20.04s`, **QEMU's stderr is empty — no warning, no refusal, nothing.**

Guest's own output:

```
NPROC=2
CPUINFO-PROCESSOR-COUNT=2
processor	: 0
model name	: QEMU Virtual CPU version 2.5+
physical id	: 0
core id		: 0
apicid		: 0
processor	: 1
model name	: QEMU Virtual CPU version 2.5+
physical id	: 0
core id		: 1
apicid		: 1
SYS-CPU-ONLINE=0-1
SYS-CPU-POSSIBLE=0-1
```

Same on the source-built binary: `exit=0 wall=20.01s`, `NPROC=2`, `SYS-CPU-ONLINE=0-1`.

`-smp 4` with the same icount options also works: `exit=0`, `NPROC=4`,
`SYS-CPU-ONLINE=0-3`.

## 2. `-icount shift=0 -smp 2 rr=record` — the exact refusal

```
/usr/bin/qemu-system-x86_64 -accel tcg,thread=single -smp 2 \
  -icount shift=0,rr=record,rrfile=runs/rr.bin ...
```

```
exit=1 wall=0.042s
qemu-system-x86_64: Record/replay: Record/replay is not supported with multiple CPUs
```

Identical message and exit from the source-built binary (`wall=0.032s`).

**The same record command with `-smp 1` succeeds** — `exit=0 wall=20.82s`, `NPROC=1`,
`SYS-CPU-ONLINE=0`. So the rejection is about the CPU count, not about record mode being
otherwise unusable here.

The third restriction, kept separate because it has been conflated with the other two:

```
/usr/bin/qemu-system-x86_64 -accel tcg,thread=multi -smp 2 -icount shift=0,sleep=off ...
exit=1 wall=0.027s
qemu-system-x86_64: -accel tcg,thread=multi: No MTTCG when icount is enabled
```

That one rejects the *accelerator*, not the CPU count. Dropping back to
`thread=single` with the same `-smp 2` is arm 1 above, and it boots.

## 3. Does the guest kernel actually use the second CPU?

Under `-icount shift=0 -smp 2`, all of the following are the guest's own counters.

**The kernel brought CPU1 online and spawned its per-CPU kernel threads:**

```
cpuhp/0 cpuhp/1 idle_inject/0 idle_inject/1 ksoftirqd/0 ksoftirqd/1 migration/0 migration/1
```

**Guest user time accumulates on both CPUs.** `/proc/stat` user column, three snapshots
(before load / after two pinned workers / after four unpinned workers), 1e9 iterations each:

| | before | after pinned | after unpinned |
|---|---|---|---|
| cpu0 | 0 | 1178 | 3924 |
| cpu1 | 0 | 1404 | 4217 |

**Work ran on CPU1 both when told to and when not told to.** `sched_getcpu()` at exit:

```
spin pinned_to=0    finished_on_cpu=0 iters=1000000000
spin pinned_to=1    finished_on_cpu=1 iters=1000000000
spin pinned_to=none finished_on_cpu=0 iters=1000000000
spin pinned_to=none finished_on_cpu=0 iters=1000000000
spin pinned_to=none finished_on_cpu=0 iters=1000000000
spin pinned_to=none finished_on_cpu=1 iters=1000000000
```

With `-smp 4` the kernel's own placement used the CPUs it was never told about: the four
unpinned workers finished on CPU2, CPU3, CPU2, CPU3.

**Both CPUs have a live local timer, and cross-CPU kernel machinery actually completes.**
`/proc/interrupts`, before and after the load, columns CPU0 / CPU1:

```
before   LOC:  1239  1286     RES:  21 287     CAL: 147 167     TLB: 1 2
after    LOC: 40626 43594     RES:  33 304     CAL: 200 200     TLB: 2 4
```

`CAL` is a function-call IPI and `TLB` a TLB shootdown: the sending CPU blocks until the
target acknowledges. 200 of them completing on each side means QEMU's round-robin loop
switched vCPUs while one was waiting on the other. This is the property the route needs —
the kernel's own cross-CPU concurrency mechanisms are live, not just two CPUs listed in
`/proc/cpuinfo`.

## Wall time, and what icount costs

Three repeats per cell, stock binary, host load average ~18-22 on 316 cores. `iters=1` is
boot-and-poweroff only; `iters=1e9` runs 6 workers of 1e9 iterations each (2 pinned, then
4 unpinned).

| arm | accel | smp | icount | boot only (3 reps) | median | with 6x1e9 load | median |
|---|---|---|---|---|---|---|---|
| rr1  | round-robin | 2 | no  | 9.36 / 9.50 / 8.66 s | **9.36** | 17.17 / 17.17 s | **17.17** |
| rr1m | MTTCG       | 2 | no  | 7.88 / 7.93 / 7.98 s | **7.93** | — | — |
| ic1  | round-robin | 2 | yes | 16.35 / 16.21 / 16.13 s | **16.21** | 48.94 / 48.84 s | **48.89** |
| ic4  | round-robin | 1 | yes | 15.13 / 15.16 / 15.34 s | **15.16** | — | — |

- **Boot: 16.21 / 9.36 = 1.73x** for turning icount on at the same accelerator and CPU count.
- **Whole run under load: 48.89 / 17.17 = 2.85x.**
- **Compute-dominated part: (48.89 - 16.21) / (17.17 - 9.36) = 32.68 / 7.81 = 4.18x.**
  This is the figure comparable to the owner's "roughly 4x" (5.28x native vs 1.37x
  round-robin ≈ 3.85x). It is measured against round-robin *without* icount, not against
  native execution.
- **The second vCPU is nearly free under icount**: 16.21 s at `-smp 2` against 15.16 s at
  `-smp 1`, +7%. Both vCPUs share one host thread, so a second CPU adds emulation
  bookkeeping, not a second core's worth of work.
- Guest throughput without icount, from the 1e9 arm: 6e9 iterations in 7.81 s ≈ 768 M
  iterations/s; with icount ≈ 184 M/s.

Wall times are not repeat-stable to better than ~0.9 s on this shared box (rr1 boot spanned
8.66-9.50 s), so the ratios above should be read to two significant figures, not three.

## What this settles and what it does not

Settled: a route that needs instruction counting is **not** forced to a uniprocessor guest.
`-icount` and multiple vCPUs coexist. Only `rr=record`/`rr=replay` forbids >1 CPU, and
only MTTCG is refused outright.

Not settled here, and it matters for the route:

- **The record/replay plug-in point named on the umbrella task (`replay_get_instructions`,
  feeding `icount_get_limit`) sits inside the mode that refuses multiple CPUs.** So
  "hijack QEMU's replay mechanism" and "have more than one vCPU" are, as the code stands,
  mutually exclusive — not because icount forbids it, but because the *replay* blocker
  does. Whether that blocker is load-bearing or merely conservative is a separate question
  from the one this task asked, and it was not tested.
- Round-robin's vCPU switch granularity under icount was not measured here. The completed
  IPIs prove switching happens; they do not say at what instruction granularity, nor
  whether it can be commanded.
- Nothing here touches reproducibility. Bitwise determinism of plain TCG+icount was
  already measured elsewhere on this project at 0 of 5 repeats matching.

## Files

- `run.sh` — the arms, both binaries
- `spin.c`, `rootfs/bin/spin` — the freestanding fixed-work guest load
- `rootfs/init` — the guest evidence collector
- `initramfs.cpio.gz`
- `runs/*.serial.log` — guest output, `runs/*.stderr.log` — QEMU's own output verbatim
- process census before and after in
  `/home/newton/work/qemu-vcpu-channel_20260820/runs/census.icountsmp-{before,after}.txt`:
  identical qemu/hermit process counts (6 qemu, belonging to the other agent's determinism
  boots), so these runs leaked nothing and disturbed nothing.
