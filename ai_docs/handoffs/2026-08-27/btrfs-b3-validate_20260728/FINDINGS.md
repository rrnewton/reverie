# B3 validation — does a hermit chaos/seed sweep permute in-guest btrfs interleavings under icount?

Task: `btrfs_repro_blocker_validate` (blocker B3 of `goal-repro-btrfs-f6a6c280`)
Date: 2026-07-28
Host: dev-hermit devserver. All work under `ignored/` (gitignored). No tracked source changed. No process kills issued (all runs exited 0 on their own).

## Verdict

**POSITIVE / FEASIBLE.** Under QEMU `-icount shift=0,sleep=off -smp 2` (the config
required to boot under hermit), a hermit **`--chaos --sched-seed` sweep DOES permute
the in-guest btrfs kernel interleaving.** hermit's scheduler control reaches the
guest vCPU switch points even though icount makes QEMU single-threaded round-robin
TCG. `--rng-seed` is an independent, orthogonal lever (works in `--strict` too).

Caveat that nearly caused a wrong conclusion: **adjacent chaos sched-seeds often
converge** to the same guest signature (sched 1≈2, sched 9≈17), so a sweep must span
a wide seed range — judging from two near seeds falsely reads as "no effect."

## Method

Built an interleaving-SENSITIVE in-guest oracle (the existing harness prints a fixed
transcript, so it cannot reveal interleaving). Two independent race signals over
btrfs on `/dev/ram0` (brd ramdisk, pre-baked image in initramfs):

1. `RACE_COUNTER` — lost-update read-modify-write: two shell processes each do
   N=150 `v=$(cat ctr); echo $((v+1)) > ctr`; final value in [1..300] measures how
   many updates were lost to interleaving. Resolution 1/300.
2. `INTERLEAVE_SHA` — sha256 of a file two processes append to concurrently
   (`emit A`/`emit B`), capturing the exact interleave order.

Probe init: `ignored/btrfs-b3-validate_20260728/root/init`. Scripts:
`run-matrix.sh`, `run-attribution.sh`, `run-disambig.sh`, `run-confirm.sh`.
Common hermit flags: `run <mode/seeds> --no-rcb-time --target-timeslice 100000
--max-timeslice 1000000000`. QEMU: `-machine q35 -accel tcg -cpu max -smp 2 -m 512M
-icount shift=0,sleep=off -rtc base=...,clock=vm -kernel ignored/qemu-linux/bzImage`.

## Data

### Oracle sensitivity controls (proves the oracle is not saturated/degenerate)
| config (strict, seed 1)      | RACE_COUNTER |
|------------------------------|--------------|
| icount shift=0, -smp 2       | 66  |
| icount shift=1, -smp 2       | 86  |
| icount shift=3, -smp 2       | 62  |
| icount shift=0, **-smp 1**   | 160 (far fewer lost updates — less true concurrency) |

### Attribution: single-variable sweeps (smp2, icount shift=0)
| mode   | held        | swept          | result            | conclusion |
|--------|-------------|----------------|-------------------|------------|
| strict | rng=5       | sched 1 vs 2   | 49 == 49          | sched-seed NO effect in strict |
| strict | sched=5     | rng 1 vs 2     | 66 vs 43          | rng-seed CHANGES guest |
| chaos  | rng=5       | sched 1 vs 2   | 42 == 42          | (adjacent seeds converge — misleading) |
| chaos  | sched=5     | rng 1 vs 2     | 66 vs 43          | rng-seed CHANGES guest |

### Disambiguation: chaos, wide sched spread, rng FIXED
| config                      | RACE_COUNTER | INTERLEAVE_SHA |
|-----------------------------|--------------|----------------|
| chaos rng=1 sched=1         | 84  | 6424cd10… |
| chaos rng=1 sched=9         | 37  | 58d7cfc6… |
| chaos rng=1 sched=17        | 37  | 58d7cfc6… |
| chaos rng=5 sched=1         | 42  | 23b14a19… |
| chaos rng=5 sched=2         | 42  | 23b14a19… |
| chaos rng=5 sched=9         | 65  | f10914af… |
| chaos rng=5 sched=17        | 65  | f10914af… |

At BOTH rng values, varying the chaos sched-seed yields ≥2 distinct guest
signatures: rng=1 → {84, 37}; rng=5 → {42, 65}. **Scheduler seed reaches the guest.**

## Mechanism (inferred, not proven at TCG level)

Under `-icount shift=0,sleep=off` QEMU runs single-threaded round-robin TCG, but it
still has multiple host threads (main loop, vCPU, iothread/RCU) and signal/timer-IRQ
delivery. hermit's chaos scheduler perturbs when those host threads run, and that
perturbation shifts guest vCPU switch points / timer-IRQ landing → different
guest-kernel interleaving. Empirically the coupling exists regardless of the exact
path; icount does NOT fully seal the guest from hermit scheduling under `--chaos`.

## Recommendation for goal-repro-btrfs-f6a6c280

1. **Primary explorer:** `hermit run --chaos --sched-seed <N>` swept over a WIDE
   range (≥16 seeds; adjacent seeds converge, so don't sample only neighbors).
2. **Orthogonal lever:** sweep `--rng-seed` too — exercises data-dependent kernel
   paths (hash buckets, etc.); works even in `--strict`.
3. Optionally add `--chaos-target-races` / `--fuzz-futexes` to bias toward
   lock-ordering / wakeup race patterns.
4. `-smp ≥2` is required for guest concurrency; `icount shift` is a secondary knob.
5. On a hang-triggering seed, use `--record-preemptions-to` /
   `--replay-preemptions-from` for deterministic replay + localization.
