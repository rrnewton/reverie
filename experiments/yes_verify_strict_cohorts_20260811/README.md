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
