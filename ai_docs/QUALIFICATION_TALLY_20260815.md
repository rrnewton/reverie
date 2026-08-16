# Running qualification tally — 380 rows, six subsets

Maintained by `hermit-coord`. **Read from task notes; nothing here was re-scored by me.**
**Re-promoted wholesale, not patched.**

## What "qualified" means here — say this with every figure

> **Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false` on
> every accept arm — NOT PARITY AND NOT L2.**

`hermit-revents`' wording, adopted verbatim. **Nothing here is a qualifying receipt.**

## Stamp

**Read 2026-08-16T00:57Z.** Note counts and status, because the corpus moves while it is read.

| subset task | notes | status |
|---|---:|---|
| `qualify-queue-rows-01-20` | 6 | CLOSED |
| `qualify-queue-rows-21-40` | 2 | in progress |
| `qualify-queue-rows-41-60` | 4 | in progress |
| `qualify-queue-rows-61-79` | 2 | in progress (`hermit-li2`) |
| `qualify-ptrace-replay-81` | 4 | in progress |
| `qualify-native-naked-73` | 1 | CLOSED |
| `qualify-kvm-verify-71` | 3 | CLOSED |
| `qualify-dbt-verify-47` | 6 | in progress |
| `qualify-sabre-verify-29` | 14 | CLOSED |

## Results, with **who checked what** — an unchecked verdict and a checked one are not the same

**This column is the point.** A verdict nobody has re-derived and one that survived an independent
run should never look alike in a tally.

| subset | scored | result | independently checked by | outcome of the check |
|---|---:|---|---|---|
| liteinst+verify 1-20 | 20 | 14 qualified **on the ptrace reference arm**, 6 not-exit-gated | — | **unchecked** |
| liteinst+verify 21-40 | 20 | 17 qualified, 2 not-exit-gated, 1 message-bearing | — | **unchecked** |
| liteinst+verify 41-60 | 20 | 14 `QUALIFIED-LITEINST`, 1 ptrace-only, 1 not-qualified, 4 ND | — | **unchecked** |
| liteinst+verify 61-79 | 0 | — | — | not started |
| **ptrace+replay** | 40 | 25 qualified, 6 not-exit-gated, 9 ND | **hermit-sabre**, on the 25 | **6 disagreements — SOFT** |
| **kvm+verify** | 71 | 20 qualified, 16 not-qualified, 7 not-exit-gated, 28 ND | **hermit-revents**, on 43 | **6 disagreements — SOFT** |
| native+naked | 73 | accept arm fails 71 of 73 | — | unchecked |
| dbt+verify | partial | in progress | self re-run | **1 flake found** |
| **sabre+verify** | 29 | 12 pass-refusable, 6 no-exit-gate, 8 accept-fail, 3 indeterminate | **hermit-li2**, on the 12 | **in progress** |

**The 2 liteinst-enabling verdicts have no check at all.** That is not a criticism of anyone — it is
the gap the column exists to show.

## The two cross-verifications — SOFT, not revised, and NOT netted

**Both reviewers found disagreements in verdicts they did not derive. The direction is downward.**

- **`hermit-revents` on KVM, 43 cells: 6 disagreements.** It caught **two faults in its own checker
  first** — a comparator that named the file, so a comment-only edit read as *differs*; and a
  mutation that hit prose at line 20 instead of the call at line 77. **Both would have manufactured a
  false disagreement. Both were fixed, and the disagreement survived on real evidence.**
- **`hermit-sabre` on ptrace, 25 cells: 6 disagreements, each backed by a run** — and it **declared a
  conflict of interest**, being the author of the same defect earlier today.

**THREE THINGS THIS TALLY DELIBERATELY DOES NOT DO:**

1. **No disagreement rate.** revents was explicit: *6 of 43 is not a rate, and I'm not offering one.*
   **Two samples are not a population**, and dividing them would invent one.
2. **The disagreements are NOT netted against the qualified counts.** Neither reviewer has reconciled
   with the original author. **There are currently two unreconciled readings of each set, and that is
   the honest state** — both are recorded, neither is subtracted.
3. **The affected sets are marked SOFT, not revised.** `hermit-kvm2` is sweeping the ptrace 25 and
   `revents` the remaining 37 KVM cells for the shared shape. **The corrected figures come after
   those sweeps, not now.**

## Also downward: a reproducibility re-run caught a flake

`hermit-dbt` re-ran both arms on 5 refusable cells **purely to check its own numbers.** Four
reproduced exactly. **`backend-parity-c/sendfile-copy` did not** — accept arm 0 on the first pass, 1
on the re-run. **A stable green it would otherwise have shipped.**

That cell keeps recurring: it is the row `hermit-teardown` is disqualified from reviewing, kvm2 scored
it NOT-EXIT-GATED, and sabre+verify has it as pass-refusable. **Three subsets, three readings, and now
a demonstrated flake.**

## DO NOT SUM THE "QUALIFIED" COLUMN

Verdict classes stay distinct: **QUALIFIED · NOT-DETERMINABLE · capability-blocked ·
accept-only-outstanding · unscored-operator-side**. `-524` bars 21 of the liteinst 79; **eligible is
not qualified.** Of kvm's 28 ND, **23 are operator-side — unscored, not unqualifiable.**

Encodings differ per slice, and kvm2 measured what a substring count costs: **36 against a true 20**,
because `NOT-QUALIFIED` nests `QUALIFIED`. **Count by exact match on the verdict token.**

## Provenance — the failure this artifact is most exposed to

**An existence check cannot see a file that was never written.** Promoting evidence earlier today I
copied 12 files and **9 landed**; 187 and **186**. A global `*.log` in `.gitignore` applied inside
`ai_docs/evidence/` and `git add` skipped five **without a word**. Every check passed, because they
all asked about files that existed. **Only the count disagreed.**

**Check the tracked count against the intended count.** And treat every figure here as provisional
against the stamp above — four subsets are in progress, two closed sets are SOFT pending sweeps, and
two closed slices were amended after closing.

**Evidence still in gitignored paths:** `ignored/qualify-sabre-verify-29-20260815/results.jsonl`.
Survives context loss, not host loss; devbig014 cannot read it. **Promotion should be scheduled.**
