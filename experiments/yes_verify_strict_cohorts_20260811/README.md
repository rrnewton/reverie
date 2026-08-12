# `yes | head` under `--verify-strict` — 2x20 factorial plus a discriminating legacy arm

## Question

Retained failures showed an inside-guest divergence for the strict-compat corpus row
`yes` (`bash -c 'set -eu; yes hermit | head -n 3'`) under **legacy `--verify`**, with
`--no-virtualize-cpuid --max-timeslice=disabled`. Those comparisons were normalized DETLOG
comparisons, not bitwise, so they were a repeatability failure and **not** canonical L2 proof.

Does it reproduce under `--verify-strict`, and does it depend on `max_timeslice`?

## Result: it does not reproduce at this head, under either comparator

| arm | comparator | `max_timeslice` | divergent |
|---|---|---|---|
| A | `--verify-strict` | `None` (historical config) | **0/20** |
| B | `--verify-strict` | `Some(200000000)` (default) | **0/20** |
| C | legacy `--verify` | `None` (historical config) | **0/20** |

Pooled **0/60**, 95% Clopper–Pearson upper bound **4.9%**. The rate is not asserted to be
zero — it is bounded below ~5%, which is incompatible with the historical failures being a
stable property of this SHA.

All 60 runs exited 0 with byte-identical stdout (one unique output across all 60) and
**zero** DNF/RPM activity in every sample window. Arms A and B additionally carry canonical
L2 evidence: `bitwise_parity: true` with `compared_log_messages` nonzero on both sides in
40/40, so the comparator was live rather than vacuous.

## Why arm C exists

`--verify-strict` is *stricter* than legacy `--verify`, so arms A and B alone cannot separate
"strict does not reproduce it" from "it does not reproduce at all any more" — both predict
0/40. Arm C reruns the exact historical comparator and configuration. It is also clean, so
the answer is the second: the failure does not reproduce here today. The hypothesis that the
legacy comparator is over-sensitive is **not** supported; both comparators agreed on all 60.

## The factorial is real

Verified with `--log=debug`, because a host fallback could otherwise masquerade as arm B:

```
--max-timeslice=disabled  ->  max_timeslice: None
flag omitted              ->  max_timeslice: Some(200000000)
```

## What this does and does not settle

It does **not** say the historical failures were imaginary; they are retained, and at least
three had no package activity to blame. It says whatever produced them is not a stable
property of `069b532f` reproducible on this host at n=20 per arm, and it retires the framing
that hermit fails to determinize this pipeline at this head.

Arm A vs arm B rules out `max_timeslice` as the discriminator. The historical failures ran
under contended validate conditions; these 60 ran on a quiet box (load ~11 of 316 cores).
**Matched load is the most promising next variable** — repeat arm C under load rather than idle.

## Reproduce

```sh
BIN=<hermit-strict at 069b532f> OUT=./results N=20 ./run-cohorts.sh   # arms A and B
```
Arm C is the same invocation with `--verify-strict` omitted. Count arm C from the tool's own
`":: Success: deterministic"` line and exit status — **not** a keyword grep: the success text
contains the words "no substantive differences found", and grepping for `differences` reports
a 20/20 failure rate that is entirely false.

## Addendum — comparator anatomy and the contended arm (2026-08-12)

### What legacy `--verify` erases before comparing

`detcore/src/logdiff.rs:215-248`, applied in order:

| regex | replacement | erases |
|---|---|---|
| `\b[\d][\d_.]*s\b` | `<NANOSECONDS>` | durations / seconds |
| `/proc/[\d]+/` | `/proc/<PID>/` | pids in paths |
| `\b0[xX][A-Fa-f0-9]+\b` | `<ADDR>` | **every** bare hex token |
| `\b[\d][\d_]*(?:\.[\d][\d_]*)?(?:ns\|us\|µs\|ms)?\b` | `<NUM>` | **every** number |
| `/tmp/[^"]*"` | `/tmp/<somewhere>"` | tmp paths |

The fourth is decisive: legacy cannot observe a numeric divergence of any kind — syscall
arguments and results, counts, sizes, flags, pids, turns, virtual time. It compares the
*shape* of the log. `--verify-strict` compares every captured INFO message with only the
wall-clock prefix stripped and host pointers canonicalised.

Measured on the same guest: strict compared **1452|1452** messages, legacy **1324|1324**.
Strict's set is larger and its normalisation weaker, so strict is stricter on both axes.

### Arm L — legacy comparator under calibrated contention

| | |
|---|---|
| configuration | guest + 6 spinners pinned to 2 CPUs (3:1 oversubscription, 2 of 316 cores) |
| calibrator | 0.16s unpinned → **0.64s** contended, a 4× slowdown |
| result | **0/20 divergent** |

Pooled across all four arms: **0/80**, 95% CP upper bound **3.7%**. At the historical 12.5%
rate, observing 0/80 has probability 0.875^80 ≈ 2.2e-5 — the mode is absent here, not merely rare.

### Retraction

An earlier note (mine) asserted the historical failures "ran under contended validate
conditions". **The ledger does not support that.** Across 93 validate rows running
`test.strict_compat`: cpu/wall ratio median **3.6** effective busy cores (min 1.2, max 8.3),
`concurrent_validates` median **0**, max 1. That was an inference reported as a measurement.

### Consequence for the bisect

A bisect needs an oracle that returns FAIL at a known-bad point. At the best-documented
known-bad point this oracle returns PASS 80/80 across four configurations. **Do not run the
588-point bisect** — every point would read green and it would conclude "never broke."
The productive next step is forensic: recover what the six retained failures actually
diverged on. Legacy prints the differing normalised lines; a field name is worth more than
400 samples.
