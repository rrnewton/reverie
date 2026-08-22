# Coherent time base for rdtsc and clock_gettime — size investigation

Task: `cc-qemu-coherent-timebase`. Investigation only; **no repository files were
changed**.

Basis for every number below:

| item | value |
| --- | --- |
| hermit revision | `770b95c505` (tree clean) |
| hermit binary | `target/release/hermit`, sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5` |
| QEMU | 10.1.2 (`qemu-10.1.2-1.4.hs+fb.el9`) |
| guest kernel | 6.17.13 (`target/qemu-busybox/bzImage`, the pinned demo kernel) |
| initramfs | `target/qemu-busybox/initramfs-busybox.cpio.gz`, from `demos/qemu-busybox/build-initramfs.sh` |
| host | 316 CPUs, Linux 6.19.2 |

## The defect, in one sentence

One expression — `guest.thread_state().thread_logical_time.as_nanos()` — is
returned to the guest from exactly two places, and it names a different clock
than the one every other guest-visible time read uses.

| site | who reads it |
| --- | --- |
| `detcore/src/lib.rs:1211-1215` | `rdtsc` / `rdtscp`, every backend. Carries a standing `TODO: use global time for rdtsc`. |
| `detcore/src/syscalls/time.rs:101-102` | `clock_gettime` / `gettimeofday` / `time`, SaBRe backend only. |

The other domain is `thread_observe_time()` → `GlobalTime::as_nanos()`
(`detcore-model/src/time.rs:738-760`), which is `starting_nanos` + **the sum of
every thread's own duration** + `extra_time`, then passed through the shared
`GuestClock` floor (`detcore/src/tool_local.rs:1198`).

About 60 other reads of `thread_logical_time` exist (timeslices, CPU accounting,
sched-event enrichment, priority changepoints). None are guest-visible.

## Three ways the two domains disagree

Measured with `twodomainN.c` (N threads; each sample reads `rdtsc` then
`clock_gettime(CLOCK_MONOTONIC)` back to back; 40 samples/thread; every thread
does an identical amount of its own work).

`hermit run --no-sequentialize-threads --max-timeslice disabled`:

| threads | rdtsc span, each thread | clock_gettime span | rate ratio | rdtsc backwards |
| --- | --- | --- | --- | --- |
| 1 | 390,975 ns | 390,975 ns | 1.000 | 0 of 39 |
| 2 | 390,975 ns (both) | 1,877,400 / 751,875 | 4.802 / 1.923 | 36 of 79 |
| 4 | 390,975 ns (all) | 6,212,600 … 1,152,875 | 15.890 … 2.949 | 64 of 159 |
| 8 | 390,975 ns (all) | 8,690,800 … 932,325 | 22.229 … 2.385 | 144 of 319 |

Every thread's rdtsc span is bit-identical because rdtsc reports only the
caller's own work. `clock_gettime`'s span grows with concurrency because it
reports the sum.

1. **Rate.** Global is a sum; per-thread rdtsc is one addend. The ratio tracks
   the number of threads making progress.
2. **Per-thread offset.** Each thread's rdtsc omits every other thread's
   contribution, and the omission differs per thread — 3.33 ms vs 7.39 ms in a
   two-thread `--strict` run. rdtsc jumps backwards at thread switches even in
   strict mode, where the rate ratio is already 1.000.
3. **Scheduler time.** `GlobalTime.extra_time` (`NANOS_PER_SCHED` = 500,000 ns
   per scheduling decision) is in the global clock and in no thread's local
   clock. A *single*-thread strict run still shows a 3,331,435 ns standing
   offset.

Only defect 1 corrupts calibration, because calibration measures a rate. Defects
2 and 3 are what break a guest comparing TSC readings taken on different vCPUs.

## It is visible in the guest's own console

The guest's printk timestamps ride `sched_clock`, which rides the TSC once
`tsc-early` is selected. `printk_rewind.py` counts backwards jumps:

| config | printk lines | backwards | largest rewind |
| --- | --- | --- | --- |
| icount, smp1, strict | 357 | 0 | — |
| no-icount, smp1, strict, `no_timer_check` | 359 | 0 | — |
| no-icount, smp2, strict, `no_timer_check` | 154 | 1 | −15.000 ms |
| no-icount, smp1, compat | 370 | 1 | −15.978 ms |
| no-icount, smp2, compat | 960 | 4 | **−17,052.637 ms** |
| icount, smp1, compat | 357 | 0 | — |

Zero rewinds in every `-icount` run and in the single-vCPU strict run; up to
17.05 seconds of backwards guest time in the concurrent two-vCPU run.

## The boot matrix — the task's premise does not reproduce

"QEMU needs `-icount` to boot at all; without it, zero output even at 1 CPU":

| profile | icount | vCPUs | rc | wall | outcome |
| --- | --- | --- | --- | --- | --- |
| compat | off | 1 | 0 | 19.3 s | PASS |
| compat | off | 2 | 0 | 23.4 s | PASS |
| compat | on | 1 | 0 | 31.8 s | PASS (icount is *slower*) |
| strict | on | 1 | 0 | 151.8 s | PASS |
| strict | on | 2 | 1 | 0.1 s | QEMU refuses: `No MTTCG when icount is enabled` |
| strict | off | 1 | 0 | 59.4 s | guest panic in `check_timer()` |
| strict | off + `no_timer_check` | 1 | 0 | 103.5 s | PASS, TSC 999.964 MHz, 0 rewinds |
| strict | off + `no_timer_check` | 2 | 137 | 900.0 s | stall at CPU#1 `calibrate_delay_direct()` |
| strict | off + `no_timer_check lpj=999964` | 2 | 0 | **382.0 s** | **PASS** |

The last row is the task's own first acceptance clause, met on current `main`
with no Hermit change — using two guest kernel cmdline flags. Both flags disable
a guest-side check that compares the TSC against a PIT/jiffies-derived clock:
`check_timer()`/`timer_irq_works()` and `calibrate_delay_direct()`. Those are the
same rdtsc-versus-`clock_gettime` comparison, so a coherent base is the
principled way to remove the flags.

`qemu-system-x86_64: -accel tcg,thread=multi: No MTTCG when icount is enabled`
is QEMU's own constraint, and it is the mechanical reason `-icount` destroys
steerability: it forces `thread=single`, collapsing both guest vCPUs onto one
host thread.

## Chaos seeds

`--strict --chaos --seed N`, no icount, 2 MTTCG vCPUs, 1800 s budget:

| run | rc | console bytes | sha256 (16) |
| --- | --- | --- | --- |
| seed 1 | 137 | 17033 | `e8c873ebd518c6f9` |
| seed 1, repeated | 137 | 17033 | `e8c873ebd518c6f9` (byte-identical) |
| seed 999 | 137 | 11546 | `17b015afb10cdc23` |

Deterministic under a fixed seed; different across seeds. The first divergence
is inside the guest's clock calibration, console line 115:

```
seed 1:   [    0.000000] tsc: Detected 999.966 MHz processor
seed 999: [    0.000000] tsc: Detected 990.002 MHz processor
```

Neither chaos run finishes the boot; the same configuration without `--chaos`
reaches the marker in 382.0 s. So chaos steering at 2 vCPUs is real and
reproducible, and it currently wedges the boot.

## Cost

`rdtsc_cost.c`, 20,000 iterations, 3 reps, `hermit run --strict`:

| workload | reps | per call |
| --- | --- | --- |
| no interception | 0.020 / 0.017 / 0.017 s | — |
| 20k `rdtsc` | 0.362 / 0.362 / 0.358 s | 17.2 µs |
| 20k `clock_gettime` | 0.742 / 0.747 / 0.743 s | 36.3 µs |

`clock_gettime` is the existing coordinator-RPC-plus-shared-floor path, so
+19.1 µs/call is an **upper** bound for rdtsc (it also includes syscall-trap
versus instruction-trap overhead rdtsc would not pay).

An instrumented full boot (strict, no-icount, smp1, `no_timer_check`,
`--log info`, 133,092,107-byte log, 109 s to PASS) issued **286,199** `rdtsc`
events — 286,187 from dtid 7, the TCG vCPU thread — against 113,038
`clock_gettime` and 61,422 `gettimeofday`. Upper bound on the added cost:
286,199 × 19.1 µs = 5.5 s on a 109 s boot, about +5%.

At 1 vCPU, 99.996% of rdtscs come from one thread, which is why the per-thread
offset defect is invisible there and structural at 2.

## Size of the change

| file | edit | lines |
| --- | --- | --- |
| `detcore/src/syscalls.rs:21` | `mod time` → `pub(crate) mod time` | 1 |
| `detcore/src/syscalls/time.rs:96` | `guest_clock_time` → `pub(crate)` | 1 |
| `detcore/src/lib.rs:1209-1217` | return `guest_clock_time(guest).await`, drop the TODO | ~9 |

About 11 lines in 3 files. **The other half already exists**: commit
`7dfed8addf` on branch `codex/sabre-clock-coordinator-time` (2026-08-17, 3 files,
+10/−9) deletes the SaBRe branch of the same expression. It is not an ancestor of
`origin/main` and has no pull request.

Continuity is provable from existing code, not only testable:
`tool_global.rs:694-702` folds `time_from_guest` into global time *before* the
request match, and `handle_rdtsc_event` charges `add_rdtsc()` at `lib.rs:1189`
before returning. The just-charged `NANOS_PER_NONDET_INSTR` is therefore already
in the lower bound the same call reads back — consecutive rdtscs differ by at
least 25 ns × multiplier, so no plateau, and `detcore/tests/time/mod.rs:372`
`rdtsc_deltas` still clears its `> 12345` bound by 25× at 308,625.

Disturbed test surface: `detcore/tests/time/mod.rs:372`,
`hermit-cli/tests/hermit_modes.rs:258,696`,
`hermit-cli/tests/record_replay.rs:51,963`, `detcore-sabre/src/lib.rs:348-350`,
plus every recorded schedule / replay log / L2 baseline containing rdtsc values.

Governance: post-facto-human-review trigger 3 (new determinization strategy) and
arguably 4 (core DetCore scheduling change) — labeled PR, dual adversarial
review, `core-review-protocol` merge gate.

## Files here

| file | what |
| --- | --- |
| `boot_matrix.sh`, `boot_one.sh` | boot the icount × vCPU × profile matrix |
| `chaos_seed.sh` | strict, no-icount, 2 vCPU chaos run at a fixed seed |
| `twodomain.c`, `twodomainN.c`, `analyze.py` | the two-domain probe and its summary |
| `rdtsc_cost.c` | interception-cost microbenchmark |
| `printk_rewind.py` | counts backwards jumps in guest printk timestamps |
| `logs/` | every console, stderr, and the instrumented info log |
