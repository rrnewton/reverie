# Does `--chaos` wedge a no-icount QEMU boot?

Task: `chaos-wedges-noicount-boot`. Investigation only; **no repository files were
changed**.

**Answer: no. It does not wedge. It costs about 6× on the median seed, with a
long right tail that makes some seeds far worse.** The premise in the task title
— including my own earlier report of a "wedge" — came from observation windows
that were too short.

| item | value |
| --- | --- |
| hermit revision | `770b95c505` (tree clean) |
| hermit binary | sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5` |
| QEMU | 10.1.2 |
| guest kernel | 6.17.13 (pinned demo `bzImage`); 6.19.2 `/boot/vmlinuz` where noted |
| guest cmdline | `console=ttyS0 panic=-1 rdinit=/init no_timer_check lpj=999964` |
| host | 316 CPUs, Linux 6.19.2 |

## It is not a wedge

Same seed, same flags, same binary — only the budget changed:

| budget | outcome |
| --- | --- |
| 600 s | no marker — reported as "wedged" |
| 1800 s | no marker — reported as "wedged" |
| 2400 s | **`rc=0`, `pass=1`, `last_guest_t=15.284575`** |

Guest time creeps in steps longer than a short observation window:

```
t=120   bytes=12643  guest_t=0.013919
t=360   bytes=12643  guest_t=0.013919
t=480   bytes=12693  guest_t=0.030500
t=720   bytes=12743  guest_t=0.047068
t=960   bytes=12857  guest_t=0.064919
t=1180  bytes=25713  guest_t=15.284575   <- PASS
```

The printk timestamp only moves when the guest prints. Inside the slow region it
does not print, so a frozen timestamp and a frozen guest look identical. The
correct discriminator was a longer budget, not closer inspection.

## Neither dispatch failure shape applies

- **Not a deadlock.** QEMU burns 5.4 CPU-seconds per 10 wall-seconds throughout,
  steadily. A directly sampled 2-vCPU run consumed 17.08 CPU-seconds in 60 wall
  seconds while its console was unchanged.
- **Not thread starvation**, though the design is starvation-shaped: `--chaos`
  gives each new thread a uniformly random priority
  (`detcore/src/tool_global.rs:2246`) and the runqueue orders strictly by
  priority with round-robin only as a tie-break
  (`detcore/src/scheduler/runqueue.rs:112-116`). Refuted by measurement —
  QEMU's main-loop thread `dtid 3` gets **18,521** log lines during the slow
  region versus **2,725** before it, six times *more*.
- **Not signal-delivery delay.** `handling inbound signal` appears **zero**
  times in both chaos and non-chaos runs; QEMU's round-robin TCG kick is a
  shared-memory store, not `SIG_IPI`.
- **Not MTTCG.** The whole effect reproduces at **1 vCPU** with
  `-accel tcg,thread=single`, and then in a 45-line C guest with no emulator.

## Root cause, isolated without QEMU

`tickwait.c`: one thread busy-waits for a counter that a second thread bumps
every 1 ms, twenty times. Same shape as `raid6_choose_gen()` waiting on
`jiffies`, and as QEMU's vCPU thread waiting on the PIT that QEMU's main loop
drives. It reports spins per delivered tick.

| configuration | spins/tick | wall |
| --- | --- | --- |
| native, no Hermit | 47,782 | — |
| `--strict`, no chaos | 19,993,386 | 10.37 s |
| `--strict`, no chaos, `--max-timeslice=20000000` | 1,993,386 | 2.38 s |

A **418× amplification with no chaos at all**, and the number is not arbitrary:
`max_timeslice` / `NANOS_PER_RCB` = 200,000,000 / 10 = 20,000,000. **A thread
busy-waiting on another thread runs one full preemption timeslice per event.**
That is Hermit's documented "preemption of last resort"
(`detcore/src/tool_local.rs:2041-2043`) behaving as designed.

What chaos adds is a heavy tail. The slice becomes a draw from an **unbounded
exponential** with that budget as its mean (`detcore/src/tool_local.rs:2070-2076`:
`lambda = 1/target_timeout_rcbs; rcbs = 1 + Exp(lambda).sample()`):

| seed | wall | spins/tick | vs non-chaos |
| --- | --- | --- | --- |
| 4 | 6.14 s | 10,200,393 | 0.51× |
| 5 | 6.24 s | 10,685,272 | 0.53× |
| 3 | 7.87 s | 13,807,792 | 0.69× |
| 2 | 8.42 s | 14,376,405 | 0.72× |
| 1 | 8.47 s | 15,205,491 | 0.76× |
| 6 | 8.49 s | 15,418,699 | 0.77× |
| 42 | 9.17 s | 16,076,456 | 0.80× |
| 999 | 16.46 s | 29,183,673 | 1.46× |
| 11 | 30.41 s | 55,369,503 | 2.77× |
| 8 | 38.24 s | 69,640,654 | 3.48× |
| 7 | 235.21 s | 415,373,102 | 20.78× |

41× spread in spins/tick, 38× in wall time — from 2× *better* than the fixed
slice to 21× worse. The cost is linear in the knob (seed 7):

| `--max-timeslice` | spins/tick | wall |
| --- | --- | --- |
| 200,000,000 | 415,373,102 | 235.32 s |
| 50,000,000 | 103,838,324 | 95.03 s |
| 20,000,000 | 41,531,368 | 68.50 s |
| 5,000,000 | 10,377,891 | 53.82 s |

## Why the knob does not rescue QEMU

A Linux boot is mostly *not* busy-waiting, and every preemption costs a PMU trap
plus a scheduler RPC. Real boot, chaos seed 1, 1 vCPU, no icount:

| configuration | result |
| --- | --- |
| no chaos | PASS ~190 s |
| chaos, default `--max-timeslice=200000000` | PASS ~1190 s |
| chaos, `--max-timeslice=20000000` (20 ms) | PASS ~2320 s — **2× worse** |
| chaos, `--max-timeslice=2000000` (2 ms) | `rc=137`, **zero console bytes** after 3000 s |
| chaos, `--target-timeslice=2000000` (2 ms) | PASS ~1000 s — 1.2× better |

`--target-timeslice` preempts at syscall boundaries, nearly free for a thread
issuing 46,637 syscalls per 200,000 log lines. It is the only knob that helped,
and 1.2× is not a fix.

## Boot results

1 vCPU, no icount, `--strict`:

| run | result |
| --- | --- |
| no chaos | PASS ~190 s |
| chaos seed 1 | PASS ~1190 s (`guest_t` 15.284575) |
| chaos seed 3 | PASS ~1270 s (`guest_t` 13.987864) |
| chaos seed 999 | PASS ~1340 s (`guest_t` 13.877203) |
| chaos seed 2 | `rc=137` at 3000 s — tail outlier, unfinished |

2 vCPUs (MTTCG), no icount, `--strict`:

| run | budget | result |
| --- | --- | --- |
| no chaos | 1800 s | **PASS ~400 s** (`guest_t` 2186.244007, 76444 bytes) |
| chaos seed 1 | 7200 s | `rc=137`, 17033 bytes (22% of the console) |
| chaos seed 1, repeat | 7200 s | `rc=137`, **69926 bytes (91%)**, `guest_t` 4691.587222 |
| chaos seed 999 | 7200 s | `rc=137`, 13165 bytes |
| chaos seeds 4 / 5 | killed early to free the box | — |

**No 2-vCPU chaos boot completed.** The best result reached 91% of the
non-chaos console.

### Determinism, stated correctly

Same seed gives **prefix-identical** consoles — not equal byte counts, which is
only true at a lucky sampling instant:

```
cmp -n 17033 v2-s1.console v2-s1rep.console   -> identical
cmp -n 13165 v2-s4.console v2-s4rep.console   -> identical
cmp          v2-s1.console v2-s999.console    -> differ at byte 8095, line 115
```

Line 115 is the guest's own clock calibration: seed 1 `999.966 MHz`, seed 999
`990.002 MHz`, seed 4 `999.965 MHz`, no chaos `999.965 MHz`.

### An unexplained 4× same-seed wall-time spread

`v2-s1` and `v2-s1rep` are the same seed and flags yet differ 4× in progress per
unit wall time. Host contention does **not** explain it: re-running seed 1 alone
(load average 11.27 on 316 CPUs, QEMU the only significant consumer) was *slower*
still — 3700 s parked at the same byte 17033. Candidate worth checking: chaos
timeslices are denominated in retired conditional branches, and PMU RCB delivery
is known to skid under load; if skid moves preemption points, the realized
schedule is not identical at a fixed seed. **Not tested** — these runs did not
use `--record-preemptions-to`. Re-run with it on both and diff.

## What the seed actually changes in the guest

Four *completed* 1-vCPU boots, printk timestamps stripped, out of ~382 lines:

| pair | differing lines |
| --- | --- |
| seed 1 vs seed 3 | 29 |
| seed 1 vs seed 999 | 29 |
| seed 3 vs seed 999 | 28 |
| seed 1 vs no chaos | 27 |
| seed 3 vs no chaos | 30 |
| seed 999 vs no chaos | 28 |

**Changing the seed changes the boot no more than turning chaos off does.** The
differences sort into:

- *Measurement noise* (the large majority): detected TSC MHz, `tsc-early`
  `max_cycles`, presence of an APIC-vs-PM-Timer calibration warning, raid6
  throughputs — and consequently a different raid6 algorithm selected —
  `rtc_cmos` read succeeding or failing, `sched_clock` stabilization values.
- *Genuine ordering*: the `ata1`/`ata3`/`ata4` "SATA link down" lines appear in
  different orders, and "Freeing initrd memory" moves relative to them.

So the seed does perturb guest-visible ordering, but mostly it perturbs the
guest's own *timing measurements*, which are noisy anyway because of the
rdtsc-vs-`clock_gettime` split. At 1 vCPU a largely serial boot offers little
concurrency to steer; the 2-vCPU comparison that would actually settle this does
not exist yet.

## Reconciliation with the "stalls at zero output" report

Two axes, both real. **Profile** — no icount, 1 vCPU, no `no_timer_check`:

| profile | machine | kernel | rc | wall | bytes | pass | panic |
| --- | --- | --- | --- | --- | --- | --- | --- |
| compat | q35 `-cpu max` | 6.17.13 | 0 | 19.3 s | 26578 | 1 | 0 |
| compat | q35 `-cpu max` | 6.19.2 | 0 | 18.0 s | 26670 | 1 | 0 |
| compat | QEMU defaults | 6.17.13 | 0 | 17.9 s | 23232 | 1 | 0 |
| compat | QEMU defaults | 6.19.2 | 0 | 18.2 s | 23249 | 1 | 0 |
| strict | q35 `-cpu max` | 6.17.13 | 0 | 60.5 s | 9233 | 0 | 1 |
| strict | q35 `-cpu max` | 6.19.2 | 0 | 54.9 s | 9441 | 0 | 1 |
| strict | QEMU defaults | 6.17.13 | 0 | 95.1 s | 8745 | 0 | 1 |
| strict | QEMU defaults | 6.19.2 | 0 | 52.5 s | 8953 | 0 | 1 |

Machine type and kernel change nothing; the profile decides. **Observation
window** — strict emits *zero* bytes for its first ~57 s:

| timeout | 15 s | 30 s | 45 s | 48 s | 52 s | 56 s | 60 s | 90 s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| bytes | 0 | 0 | 0 | 0 | 0 | 0 | 8186 | 9233 |

Any bound at 56 s or less reports exactly zero. Both reports were correct; they
were never the same configuration.

## Files here

| file | what |
| --- | --- |
| `run.sh` | boot with progress sampling every 10 s |
| `reconcile.sh` | the profile × machine × kernel matrix |
| `tickwait.c` | the 45-line busy-wait-for-a-tick model |
| `logs/` | consoles, stderr, progress traces |
