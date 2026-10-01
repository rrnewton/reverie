### Per-variant summary

| variant | runs | passes | failures | hangs | parsed runs | total markers | markers with 1 in flight | markers with >=2 in flight | markers with >=1 in flight | runs with >=1 in-flight marker | exec_time s min / median / max | invocation wall s min / median / max | 1-min loadavg range |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A | 40 | 40 | 0 | 0 | 40 | 3840 | 147 | 0 | 147 | 38 of 40 | 0.098 / 0.106 / 0.171 | 0.102 / 0.111 / 0.178 | 24.67-26.02 |
| B | 40 | 40 | 0 | 0 | 40 | 3840 | 123 | 0 | 123 | 39 of 40 | 0.099 / 0.107 / 0.156 | 0.104 / 0.112 / 0.163 | 24.67-26.02 |
| C | 40 | 40 | 0 | 0 | 40 | 3840 | 6 | 0 | 6 | 6 of 40 | 0.135 / 0.157 / 0.509 | 0.140 / 0.162 / 0.515 | 24.35-26.02 |

- A: non-pass runs: none; unparsed runs: none; baseline cross-check failures: none; RPC samples differing from marker fds: none
- B: non-pass runs: none; unparsed runs: none; baseline cross-check failures: none; RPC samples differing from marker fds: none
- C: non-pass runs: none; unparsed runs: none; baseline cross-check failures: none; RPC samples differing from marker fds: none

### Baseline, excess and in-flight detail (parsed runs)

Excess = marker fds minus the run's baseline (iteration 0's fds, the test's `captured[0].1`).

| variant | baseline values (runs) | modal fds minus baseline (runs) | runs whose marker 0 had a child in flight | excess histogram at markers with >=1 in flight | excess histogram at markers with exactly 1 in flight | excess histogram at markers with >=2 in flight | excess histogram at markers with 0 in flight |
|---|---|---|---|---|---|---|---|
| A | 18 (40) | +0 (40) | 0 | +5: 143, +6: 1, +7: 3 | +5: 143, +6: 1, +7: 3 | (none) | +0: 3693 |
| B | 18 (40) | +0 (40) | 0 | +5: 118, +7: 5 | +5: 118, +7: 5 | (none) | +0: 3717 |
| C | 18 (40) | +0 (40) | 0 | +5: 5, +7: 1 | +5: 5, +7: 1 | (none) | +0: 3834 |

| variant | largest excess per run: histogram (value: runs) | largest excess per run, by k |
|---|---|---|
| A | +0: 2, +5: 34, +6: 1, +7: 3 | 1:+0 2:+5 3:+5 4:+5 5:+5 6:+5 7:+5 8:+5 9:+5 10:+5 11:+5 12:+5 13:+5 14:+7 15:+6 16:+5 17:+5 18:+5 19:+7 20:+7 21:+0 22:+5 23:+5 24:+5 25:+5 26:+5 27:+5 28:+5 29:+5 30:+5 31:+5 32:+5 33:+5 34:+5 35:+5 36:+5 37:+5 38:+5 39:+5 40:+5 |
| B | +0: 1, +5: 34, +7: 5 | 1:+5 2:+5 3:+5 4:+5 5:+0 6:+5 7:+7 8:+5 9:+5 10:+5 11:+5 12:+7 13:+7 14:+5 15:+5 16:+7 17:+5 18:+5 19:+5 20:+5 21:+5 22:+5 23:+5 24:+5 25:+5 26:+5 27:+5 28:+5 29:+5 30:+5 31:+5 32:+5 33:+5 34:+5 35:+5 36:+5 37:+5 38:+5 39:+5 40:+7 |
| C | +0: 34, +5: 5, +7: 1 | 1:+5 2:+0 3:+0 4:+0 5:+0 6:+0 7:+5 8:+0 9:+0 10:+0 11:+7 12:+0 13:+0 14:+0 15:+0 16:+0 17:+0 18:+0 19:+0 20:+0 21:+5 22:+0 23:+0 24:+0 25:+0 26:+0 27:+0 28:+5 29:+0 30:+5 31:+0 32:+0 33:+0 34:+0 35:+0 36:+0 37:+0 38:+0 39:+0 40:+0 |

| variant | runs by number of in-flight markers (count: runs) | in-flight markers by iteration (iteration x count) |
|---|---|---|
| A | 0: 2, 1: 5, 2: 3, 3: 12, 4: 5, 5: 4, 6: 6, 8: 3 | 1 x5, 2 x1, 3 x2, 4 x5, 5 x2, 6 x5, 7 x4, 8 x1, 9 x1, 10 x1, 11 x1, 13 x4, 14 x1, 15 x4, 16 x2, 17 x1, 21 x2, 22 x5, 23 x1, 24 x2, 25 x3, 26 x4, 27 x2, 28 x3, 29 x1, 30 x1, 32 x1, 33 x2, 36 x1, 37 x1, 38 x1, 39 x2, 42 x2, 43 x1, 44 x4, 47 x1, 48 x2, 49 x1, 50 x4, 51 x2, 53 x4, 54 x2, 57 x2, 58 x1, 59 x1, 60 x1, 62 x3, 63 x2, 64 x2, 65 x1, 66 x3, 68 x2, 71 x1, 72 x1, 74 x3, 75 x2, 76 x1, 77 x1, 78 x3, 80 x1, 81 x4, 82 x2, 83 x1, 84 x2, 86 x1, 87 x2, 88 x3, 89 x2, 90 x1, 93 x2, 95 x1 |
| B | 0: 1, 1: 11, 2: 8, 3: 6, 4: 4, 5: 5, 6: 1, 7: 2, 8: 1, 9: 1 | 1 x1, 2 x5, 3 x1, 4 x1, 5 x2, 7 x3, 9 x1, 11 x1, 12 x1, 13 x2, 14 x4, 16 x4, 18 x1, 22 x1, 23 x1, 24 x2, 26 x1, 27 x3, 29 x2, 30 x3, 31 x4, 32 x2, 33 x2, 35 x3, 36 x2, 38 x2, 39 x1, 41 x4, 45 x1, 46 x2, 47 x2, 48 x1, 49 x2, 50 x1, 51 x1, 52 x1, 53 x3, 54 x1, 55 x1, 56 x3, 57 x2, 58 x3, 60 x2, 61 x1, 63 x1, 64 x1, 65 x1, 68 x2, 69 x1, 70 x1, 71 x2, 72 x2, 73 x3, 75 x1, 77 x2, 80 x2, 82 x1, 84 x1, 85 x1, 86 x3, 87 x3, 89 x1, 90 x1, 91 x1, 92 x2, 94 x2, 95 x1 |
| C | 0: 34, 1: 6 | 7 x1, 23 x1, 68 x1, 79 x1, 88 x1, 91 x1 |

### Cross-check: one or more children in flight versus excess >= +5 (all markers of parsed runs)

| variant | in flight >=1 and excess >=5 | in flight >=1 and excess <5 | in flight 0 and excess >=5 | in flight 0 and excess <5 | agreement |
|---|---|---|---|---|---|
| A | 147 | 0 | 0 | 3693 | 3840 of 3840 |
| B | 123 | 0 | 0 | 3717 | 3840 of 3840 |
| C (supplementary) | 6 | 0 | 0 | 3834 | 3840 of 3840 |

### Final steady sample (A and B print it; C has no such line)

| variant | steady fds minus baseline (runs) |
|---|---|
| A | +0: 40 |
| B | +0: 40 |
| C | (none) |
