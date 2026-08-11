# LiteInst ENOTSUPP cluster — compat sweep evidence (2026-08-10)

## Why this exists

This is the producer evidence behind **reverie PR #430** ("Let LiteInst guests create threads
and child processes"), which fixes `handle_new_task` returning `ENOTSUPP` unconditionally and
thereby blocking every task-creating syscall on the LiteInst backend.

It was rescued from `worktrees/fix-liteinst/hermit/ignored/sweep/`, a **gitignored directory
inside an agent worktree**. For four of the six fixes in that cluster it was the only evidence
in existence, and it would have been destroyed the moment that slot was reclaimed. Copied here
by the coordinator, since the producing agent's task named no parent paths it owned.

## Result

| measure | value |
|---|---|
| denominator (total cells) | 235 |
| LiteInst passing, before | 127 |
| LiteInst passing, after | **171** |
| gain | **+44** |
| ENOTSUPP cohort | exactly 51 of 78 |

An independent reviewer recomputed the +44 from these CSVs rather than from the PR's claim, and
confirmed **all 44 gained cells satisfy the full L2 predicate with zero violations**. The
production ptrace path was separately walked hunk by hunk — all 17 — and none of the change is
observable by a non-LiteInst session. The change **does not cross** the Reverie core-abstraction
line: no file under `reverie/` is touched and every added symbol is private or `pub(crate)`.

**PR #430 is NOT landed and is currently blocked.** Two reviewers working from source, on disjoint
axes, both returned CHANGES-REQUESTED — a false-green hole (`record_liteinst_failure` is the only
writer of the session-failure slot and four refusal paths bypass it), an ordering hole (the root
reads that slot before `tool_exit` joins its children), and three `/// REGRESSION COVERAGE:`
comments claiming coverage the PR's own ablation table says does not exist. The +44 measurement
below is sound; the implementation needs those fixes before it lands.

## Contents

| file | rows | what it is |
|---|---|---|
| `baseline.csv` | 471 | pre-fix sweep, all cells |
| `fixed.csv` | 236 | post-fix sweep, LiteInst |
| `fixed-ptrace.csv` | 236 | post-fix sweep, ptrace — the no-regression control |
| `final.csv` | 471 | final combined sweep |
| `sweep.py` | — | the producer that generated the CSVs |
| `summarize.py` | — | the reducer that turns CSVs into the scorecard |

The four matching `*.log` run logs were **not** committed: `.gitignore:94` excludes `*.log`
repository-wide. They were short (600–1300 bytes each) and carried only invocation banners and
timing already recoverable from the CSVs, so they were left rather than force-added against a
standing ignore rule.

## What was deliberately NOT preserved

The source directory's `build/` subtree — **13 MB, containing compiled `guest` executables** plus
per-cell `.out`, `.err` and `.json` artifacts (2481 files). Compiled binaries may not be committed
to any repository under the Binary And Large-File Policy, and the per-cell artifacts are
regenerable by re-running `sweep.py`.

The four CSVs are the reduced form that every downstream claim was computed from, and `sweep.py`
plus `summarize.py` make the reduction reproducible. If a per-cell stdout is ever needed, it must
be regenerated; it no longer exists once `worktrees/fix-liteinst/` is reclaimed.

## Integrity

```
7e85061fff7b62cf1f0083b96220d2ba12d81ce2d05ea21272d882217427f656  baseline.csv
03ecc1da065f13c292f868276a0eb862d36ae25a72cb1764ded8520d91e6d1f5  final.csv
b4b47af813709342389a4c9c4812e665f0421e0ec66d77baba7544f5dc88539a  fixed.csv
59eb10039be0c5a8e9e70e631a882736d119eb7f7744bd38b17a4e1421cfb485  fixed-ptrace.csv
a0adbb20ef01137d9b91d6aa2502d668f58f74f1d373f39cf0f437784c3f12f7  summarize.py
1496319e5c800cd27ad40223d87ab12320173a78856ec21b9dad68359dd22023  sweep.py
```

## Reproducing

Run `sweep.py` against a LiteInst-capable checkout, then `summarize.py` over the resulting CSV.
The sweep requires a built Hermit and the LiteInst backend staged; it is not hermetic against a
busy multi-tenant box — see hermit #2111 for the flake classes that affect corpus sweeps, and
#2109 / #2107 for the two host-state surfaces that break bitwise parity.
