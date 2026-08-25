# Does a fixed chaos seed produce an identical realized schedule?

Task: `chaos-fixed-seed-reproducibility`. Investigation only; **no repository
files were changed**.

**Answer: yes. A fixed chaos seed names a schedule.** The PMU-skid hypothesis
that prompted this task — mine, from the `chaos-wedge` investigation — is
**wrong**. Seed search is sound and `cc-repro-methodology` is not blocked by
this.

Falsification condition, stated before any run: *if repeated runs at one fixed
seed produce byte-identical `--record-preemptions-to` recordings, the hypothesis
is wrong.* They do.

| item | value |
| --- | --- |
| hermit revision | `770b95c505` (tree clean) |
| hermit binary | sha256 `4d8e89249ec1f0fb4bc4c5f4ee24f7727c4913964ad06594b22d55d0c0443db5` |
| QEMU | 10.1.2 |
| guest kernel | 6.17.13 (pinned demo `bzImage`) |
| host | 316 CPUs, Linux 6.19.2 |

## Result

All at chaos seed 1. Every group produced exactly one distinct recording hash.

| workload | host load | runs | outcome |
| --- | --- | --- | --- |
| 2-thread busy-wait (`tickwait`) | ~12 (idle) | 10 | 1 hash |
| 2-thread busy-wait | 79 → 173 | 10 | **same hash as idle** |
| 7-thread CPU-bound (`spin6`) | ~220 | 5 | 1 hash |
| **full QEMU boot, 1 vCPU** | ~12 | **3** | **1 hash** |
| **full QEMU boot, 1 vCPU** | **~220** | **1** | **same hash as low load** |

19 recordings, 3 workloads, host load spanning 12 to 220, zero differences.
Zero `HERMIT_SKID_OVERSHOOT` markers anywhere.

The decisive command:

```bash
hermit --log error run --strict --chaos --seed 1 \
  --record-preemptions-to rec/qfull-s1-rN.json -- \
  qemu-system-x86_64 -nodefaults -nic none -machine q35 -cpu max -m 256M \
    -accel tcg,thread=single -smp 1 -rtc base=utc,clock=vm \
    -kernel target/qemu-busybox/bzImage \
    -initrd target/qemu-busybox/initramfs-busybox.cpio.gz \
    -display none -serial stdio -monitor none -no-reboot \
    -append 'console=ttyS0 panic=-1 rdinit=/init no_timer_check lpj=999964'
```

```
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qfull-s1-r1.json
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qfull-s1-r2.json
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qfull-s1-r3.json
91b65696dba8ee5cffcf1556547977b70a5266e46f30cbce6047a0cf9bf51434  rec/qload-s1-r2.json   (load ~220)
```

583,561,705 bytes each.

## The comparison is not shallow

The recording has two parts, both covered by that hash:

- **`per_thread`** — 6,448 preemption points across 3 threads, each with a
  parallel `preemption_rcbs` entry holding the *realized* retired-conditional-
  branch count (sample: `12741169, 32586469, 36422998, 56296698, 76282298,
  82574629`). Skid, if it propagated into the schedule, lands here.
- **`global`** — the full schedule trace, **5,564,357 scheduler events**, each
  with `dettid`, `op`, `count`, `end_time`, e.g.
  `{"dettid":3,"op":"Branch","count":49,"end_time":490}`.

A single skidded branch count anywhere would have broken the hash.

### The instrument was checked before trusting a negative

`detcore/src/preemptions.rs:909-928` (`insert_reprioritization`) stores its
`rcbs` argument into `preemption_rcbs`. The caller chain —
`detcore/src/scheduler.rs:3136` → `perform_priority_changepoint` →
`ResourceID::PriorityChangePoint` built at `detcore/src/syscalls/time.rs:167`
from `thread_state().committed_clock_value` — supplies the **realized** committed
branch clock, not a target deadline. An instrument recording only intended
deadlines would have produced identical files regardless, making the negative
result worthless.

## Why it is identical — a mechanism, not luck

`reverie/reverie-ptrace/src/timer.rs:15-17`:

> Due to PMU skid, precise timer events must be driven to completion via single
> stepping. This means the PMI is scheduled early…

The counter interrupt is deliberately scheduled **early** by a per-processor skid
margin (`timer.rs:155`, default 1000 RCBs; family-specific values at
`timer.rs:180-209`), and Hermit then **single-steps** to the exact target branch
count. Skid is absorbed before it can reach the schedule. The residual overshoot
case is itself instrumented and counted (`reverie/reverie/src/timer.rs:43-62`,
`SKID_OVERSHOOT_MARKER` plus a process-global counter used to attribute
divergence during `--verify`).

## This also explains the anomaly that started it

How *much* single-stepping skid absorption takes depends on how much skid
actually occurred, which varies with host conditions. So the work to land on a
schedule point varies; the point does not. Forcing the margin:

| `--skid-margin` | wall | recording sha256 |
| --- | --- | --- |
| 1000 | 8.32 s | `1fc00ad70ba672f6…` |
| 10000 | 22.42 s | `1fc00ad70ba672f6…` |
| 50000 | 85.40 s | `1fc00ad70ba672f6…` |
| (no override) | — | `1fc00ad70ba672f6…` |

A **10.3× wall-time swing with a byte-identical recording**. Wall time and
realized schedule are decoupled by construction.

So the 4× progress difference between two 2-vCPU runs of chaos seed 1 (17,033 vs
69,926 console bytes in the same 7200 s budget, prefix-identical consoles) needs
no scheduling explanation. Prefix-identical consoles were never evidence of a
problem — they were evidence the schedule was fine and only the traversal rate
differed. A normal property of the skid-absorption design was read as a
determinism smell.

## Scope limit

The original anomaly was at **2 vCPUs**; these QEMU recordings are 1 vCPU with
`-accel tcg,thread=single`. A 2-vCPU chaos boot could not be recorded: those do
not complete within any affordable budget, and a recording is written only on
normal guest exit. Verified: `--stop-after-turn` exits `rc=1` with
"Sandbox container exited unexpectedly" and writes nothing; a SIGTERM-terminated
run also writes nothing. So 2 vCPUs is covered only indirectly — by the 3-thread
1-vCPU QEMU case and the 7-thread synthetic. Skid absorption is per-thread and
thread count is not a variable in it, so this is unlikely to change the
conclusion, but it is a genuine gap.

## Files here

| file | what |
| --- | --- |
| `qrun.sh` | full QEMU boot with `--record-preemptions-to` |
| `tickwait.c` | 2-thread busy-wait guest (from `chaos-wedge_20260818`) |
| `spin6.c` | 7-thread CPU-bound guest |
| `load.sh` | synthetic host load generator |
| `rec/` | recordings, consoles, stderr |
