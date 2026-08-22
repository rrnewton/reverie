# Reproducing a Crash Cortex candidate under Hermit + QEMU — machinery and gate

Task: `cc-repro-methodology`. **No candidate was attempted**, because the gate
that everything downstream depends on does not pass. The snapshot/resume
machinery does work and is the reusable part.

**No demo file was modified.** `demos/lib/qemu_controller.py` hardcodes `-smp 1`
and `-icount shift=0,sleep=off`; this experiment needs the opposite of both, so
per the owner ruling the machinery is a copy, productionised here.

| item | value |
| --- | --- |
| hermit | `770b95c505`, binary sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5` |
| QEMU | 10.1.2 |
| guest kernel | 6.17.13 (pinned demo `bzImage`) |
| guest cmdline | `console=ttyS0 panic=-1 rdinit=/init tsc=nowatchdog no_timer_check` |

## The configuration in the plan does not exist

`--chaos` silently forces `sequentialize_threads = true`
(`detcore-model/src/config.rs:624-626`), overriding `--no-sequentialize-threads`.
No warning. So "compatibility profile under `--chaos`" is not a thing.

| flags (1 vCPU, no icount, no guest workarounds) | rc | wall | bytes | result |
| --- | --- | --- | --- | --- |
| `--no-sequentialize-threads --max-timeslice disabled` | 0 | 19.0 s | 26627 | PASS |
| … **+ `--chaos --seed 1`** | 137 | 200.0 s | **0** | no marker |
| `--strict` | 0 | 58.6 s | 9233 | guest panic |

The 19.3 s figure quoted in the plan is a *non-chaos* figure and does not
survive adding chaos.

## The snapshot route works

Because chaos needs sequentialized threads, and a sequentialized 2-vCPU
no-icount *boot* under chaos never completed in 7200 s, boot must happen without
chaos. That is what the task intends by "runtime only, boot does not need it".

```bash
./build-initramfs.sh
qemu-img create -f qcow2 snap.qcow2 256M
./cc_qemu.py boot --smp 2 --append-extra "tsc=nowatchdog no_timer_check"
./cc_qemu.py resume --tag t1 --seed 1 --command order --expect ORDER \
             --append-extra "tsc=nowatchdog no_timer_check"
```

| step | cost |
| --- | --- |
| boot + `savevm` (native) | 8–11 s, **one time** |
| resume + workload, native | 3.1–3.3 s |
| resume + workload, `--strict` | 15.8 s |
| resume + workload, `--chaos` | **21.6 s – 229.0 s**, seed-dependent |

Median chaos run ≈ 25 s. Seed 7 took 229.0 s (≈9× median); on an earlier
workload seed 999 took 349.3 s vs seed 1's 17.7 s (≈20×). Budget per seed and
drop overruns, or the tail defines the sweep's wall clock.

### The snapshot disk serializes runs

A qcow2 takes an **exclusive write lock**. Seven parallel resumes against one
`snap.qcow2` produced seven empty results and this in stderr:

```
Failed to get "write" lock
Is another process using the image [.../snap.qcow2]?
```

`cc_qemu.py` now copies the snapshot to `disk-<tag>.qcow2` with
`cp --reflink=auto` per run. Without this a sweep runs zero-wide and looks like
a timeout. Orphaned QEMU processes also keep holding locks, so a sweep must reap
its children.

### `tsc=nowatchdog` is required, and it is hiding a real defect

Without it, the resumed guest under Hermit never reaches the workload:

```
[ 6.16] clocksource: Marking clocksource 'tsc' as unstable because the skew is too large
[ 6.16] Clocksource 'tsc' skewed 57476130 ns over watchdog 'hpet' interval of 633375520 ns
[ 6.17] tsc: Marking TSC unstable due to clocksource watchdog
[10.66] clocksource: Checking clocksource tsc synchronization from CPU 1 to CPUs 0.
[15.77] smp: csd: Detected non-responsive CSD lock (#1) on CPU#1, waiting 5000013770 ns for CPU#00
[26.99] nmi_backtrace_stall_check: CPU 0: NMIs are not reaching exc_nmi() handler
```

425 s spent producing only that. The chain is the `cc-qemu-coherent-timebase`
defect: Hermit's `rdtsc` is per-thread while `clock_gettime` is the sum across
threads → the guest's watchdog reads that as TSC skew → marking TSC unstable
triggers a **cross-CPU TSC sync** → the IPI stalls under sequentialized 2-vCPU
scheduling.

## The gate fails

Detector (`guest/order.c`): two processes pinned to the two vCPUs each append
their id to a shared trace via atomic fetch-and-add, 1000 entries each.
`switches` counts alternations — this registers fine-grained interleaving even
without wall-clock overlap.

| configuration | switches | wall |
| --- | --- | --- |
| native, no Hermit | **1456** | 3.2 s |
| native, no Hermit | **1249** | 3.2 s |
| `--strict`, no chaos | 3 | 32.1 s |
| `--strict`, no chaos | 1 | 15.8 s |
| `--chaos --seed 1` | 1 | 26.1 s |
| `--chaos --seed 1` (repeat) | 1 | 26.3 s |
| `--chaos --seed 2` | 1 | 39.6 s |
| `--chaos --seed 3` | 1 | 48.7 s |
| `--chaos --seed 7` | 1 | 229.0 s |
| `--chaos --seed 999` | 1 | 21.6 s |

**Every seed gives `switches=1` and the identical head pattern**: one guest
process runs all 1000 iterations, then the other. Native gives 1249–1456 and
varies. Seeds change wall time 15×; they do not change guest order.

Preemption granularity does not help:

| flag | switches | wall |
| --- | --- | --- |
| `--max-timeslice=200000000` (200 ms, default) | 1 | 26.2 s |
| `--max-timeslice=20000000` (20 ms) | 1 | 185.6 s |
| `--max-timeslice=2000000` (2 ms) | — | no output in 700 s |

Control, since this is a negative claim — the guest really has two CPUs:

```
native          cpus=2 online=0-1
hermit chaos s1 cpus=2 online=0-1
```

An earlier lost-update detector agreed: native `lost=3,173,663`, `--strict`
`lost=23,631`, every chaos seed `lost=0`.

**Reasoning (not measurement):** Hermit's determinism comes from sequentializing
host threads, and QEMU's two guest vCPUs *are* two host threads. One guest CPU
runs while the other is stopped, in timeslice-length blocks. The workload fits
inside about one block, so the guest sees a nearly serial execution. Chaos
changes which thread wins and for how long — hence the 15× wall spread — but
does not add switch points. Why a finer timeslice did not add them either is
not explained here.

## Why no candidate was attempted

Every shortlisted candidate is an SMP race needing ≥2 guest CPUs. Searching
seeds over a configuration that yields one guest interleaving is 316 identical
runs, and any "reproduction" that surfaced could not be attributed to steering.
Anti-candidates (`perf-event-exit-quadratic`, `bpf-arena-64k-misalign`,
`fuse-zero-size-ptr`) were excluded by `cc-triage` and not considered.

## Files

| file | what |
| --- | --- |
| `cc_qemu.py` | boot / snapshot / resume; per-run disk, QMP client, serial-pipe command injection |
| `build-initramfs.sh` | busybox + detectors initramfs |
| `sweep.sh`, `sweep2.sh` | one run per invocation; `sweep2` forwards extra hermit flags |
| `guest/order.c` | order-recording interleaving detector |
| `guest/racer.c` | lost-update detector (blunter; kept for the record) |
| `qemu_controller_copy.py`, `qemu-snapshot-copy.sh` | verbatim copies of the demo machinery this was built from |
