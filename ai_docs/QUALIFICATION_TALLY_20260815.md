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

**This column is the point, and it now has FOUR states rather than two:** *unchecked* ·
*checked-and-disagreed* · *checked-and-held* · *checked-and-strengthened-with-a-control*. A verdict
nobody has re-derived, one that survived a check, and one a check made **stronger** are three
different things.

| subset | scored | result | independently checked by | outcome of the check |
|---|---:|---|---|---|
| liteinst+verify 1-20 | 20 | 14 qualified **on the ptrace reference arm**, 6 not-exit-gated | **hermit-codexrev** | dispatched, not started |
| liteinst+verify 21-40 | 20 | 17 qualified, 2 not-exit-gated, 1 message-bearing | **hermit-fixtures**, 26 cells | **1 disagreement — row 27, UNRECONCILED** |
| liteinst+verify 41-60 | 20 | 14 `QUALIFIED-LITEINST`, 1 ptrace-only, 1 not-qualified, 4 ND | **hermit-codexrev** | dispatched, not started |
| liteinst+verify 61-79 | 19 | **9 `REFUSES-LITEINST`** (rows 61-67, 69, 70) — RELAYED, see below | **hermit-fixtures**, in the same 26 | **held** |
| **ptrace+replay** | 40 | 25 qualified, 6 not-exit-gated, 9 ND | **hermit-sabre**, on the 25 | **6 disagreements — SOFT** |
| **kvm+verify** | 71 | 20 qualified, 16 not-qualified, 7 not-exit-gated, 28 ND | **hermit-revents**, on 43 | **6 disagreements — SOFT** |
| native+naked | 73 | accept arm fails 71 of 73 | — | unchecked |
| dbt+verify | 47 | CLOSED, then **amended after closing** | self re-check | **detector fault found — counts changed** |
| **sabre+verify** | 29 | 12 pass-refusable, 6 no-exit-gate, 8 accept-fail, 3 indeterminate | **hermit-li2**, all 29 | **HELD — 0 disagreements; 1 verdict STRENGTHENED** |

**CORRECTION to the previous revision of this file, which said the 2 liteinst-enabling verdicts had
no check at all.** They do. `hermit-fixtures` holds `cross-verify-the-liteinst-qualified-verdicts`,
in progress, and has enumerated **26 cells to re-derive** — `hermit-sabre`'s 17 (rows 21-40) plus
`hermit-li2`'s 9 (rows 61-79). It has derived none of them yet and says so.

**AND THE 2 ARE SUBSUMED, NOT SEPARATE — DO NOT ADD THEM.** `hermit-liteinst`'s original 2,
`tee-enosys` and `ustat-enosys`, **are rows 67 and 69 inside li2's 9.** li2's figure **replaces** the
earlier 2-of-19 tally rather than adding to it. A tally that carried both would double-count.

**Attribution, because it is not where you would look for it:** li2's 9 is recorded in
`hermit-fixtures`' enumeration note, **not** in `qualify-queue-rows-61-79`, whose two notes are the
reassignment and carry no result. **RELAYED, not read from the producing task.**

**Slices still with no check: liteinst 1-20 and 41-60.**

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

## The SaBRe 29 HELD — the first set tonight to survive an independent check

**`hermit-li2` cross-verified `hermit-codexrev`'s 29 and found ZERO disagreements**, against 6 each in
the ptrace 25 and the KVM 43. Recorded as its own state because a set that holds and a set nobody has
looked at are not the same evidence.

**One verdict was STRENGTHENED rather than confirmed.** The `clone` cell timed out for both agents —
but **codexrev had no control, which cannot separate a cell property from a busy host.** li2 ran **a
ptrace control on the same binary at the same moment: rc=0.** So the hang is **SaBRe-specific**, which
is a stronger claim than the original verdict made.

**It stated its prior and reported against it.** li2 **expected the verdict to break**, having
overturned five inherited timeout verdicts an hour earlier, and reported that it did not. A negative
from someone who expected a positive is worth more than one from someone who expected nothing.

**Arithmetic checked two ways, neither of them file order.** The 37→29 reduction was re-derived
against codexrev's **independently written class lists** — zero disagreements — and the four lists
**partition the population with an empty set difference in both directions**.

**MEASURED HERE, third route:** the four class lists hold **29 cells, 29 distinct, 0 duplicates
across classes**, and set-difference against the queue's own `sabre`/`verify` population is **empty in
both directions**. `12+3+8+6 = 29` closes.

### The bound li2 put on its own result, which belongs beside the number

> **21 of the 29 rest on a structural argument and bookkeeping, not on execution — 16 of 29 by the
> structural route.**

**A CONFIRMED SET IS NOT A FULLY-EXECUTED SET.** "Held under independent check" and "ran end to end"
are different claims, and this set is the first and not the second. The two figures — 21 non-executed,
of which 16 structural — are kept distinct rather than collapsed into one.

## Also downward: a reproducibility re-run caught a flake

`hermit-dbt` re-ran both arms on 5 refusable cells **purely to check its own numbers.** Four
reproduced exactly. **`backend-parity-c/sendfile-copy` did not** — accept arm 0 on the first pass, 1
on the re-run. **A stable green it would otherwise have shipped.**

That cell keeps recurring: it is the row `hermit-teardown` is disqualified from reviewing, kvm2 scored
it NOT-EXIT-GATED, and sabre+verify has it as pass-refusable. **Three subsets, three readings, and now
a demonstrated flake.**

## Every checked set changed — with one distinction the summary flattens

**No verdict set has come back with nothing to say.** Running total across the checks:

| check | outcome |
|---|---|
| ptrace 25 | **6 disagreements** |
| KVM 43 | **6 disagreements** |
| KVM conversions | **5 disqualified of 9** |
| liteinst 26 | **1 disagreement**, unreconciled |
| — RELAYED, unsourced by me — | **three false `NOT-EXIT-GATED`** |
| SaBRe 29 | **0 disagreements** — and 1 verdict **strengthened** |

**The distinction worth preserving: SaBRe HELD.** Zero disagreements is not the same result as six,
and "every set was touched" is true only if you count a *strengthening* as a touch. It was touched in
the sense that a control was added that codexrev did not have — **the verdict got stronger, not
weaker.** Recording it in the same row as six disagreements would erase the only clean check of the
night.

**The last two unchecked sets are now dispatched** — `hermit-codexrev` has liteinst 1-20 and 41-60.
**No tg task existed for it at this stamp**, so that row records a dispatch rather than a task id, and
it should be re-read once one exists.

## ONE RETIRED FIGURE — recorded with its provenance, NOT deleted

**`4,750` is retired.** It was quoted as the largest unexplained population and **relayed onward to
devbig014**, so it is in circulation and must be retired visibly rather than dropped.

**Provenance:** it came from `hermit-revents`' bound — *the CLI gate accounts for at most 1,306 and
none of the other 4,750.* **Retired by `hermit-dbt`'s independent measurement:** *4,750 isn't
reconstructible from my counts as a pool containing replay and chaos.*

**A figure that vanishes silently is worse than one carried with a correction** — someone re-derives
it in a week and nobody knows it was already tested.

### An adjacency the next reader will hit, flagged and NOT adjudicated

`ai_docs/evidence/four-populations-map-20260815/MAP.txt` carries **4,751** — disabled-red scorecard
cells — and reconciles it three ways: `disabled-red 4751 + enabled-red 455 = red 5206`, and
`3853 + 213 + 685 = 4751`. **That is one off the retired 4,750.**

**I am not asserting they are the same population.** The map is internally consistent at 4,751 and the
retired figure is 4,750, and nothing I can read settles whether that is the same set off by one or two
different sets. **The map cannot adjudicate the other half either — it carries no CLI-gate figure at
all.** Recorded so a re-derivation starts from the discrepancy rather than rediscovering it.

### Why the overlap was NOT netted, which is the core of it

**RELAYED, `hermit-dbt`:** the CLI-gate exclusion is **1,344 declarations by its measurement** versus
**at most 1,306 cells** by revents'. **DIFFERENT UNITS — declarations are not cells.** It did not
subtract, and said why:

> *subtracting a figure I can't reproduce is worse than overlapping.*

**The overlap stays visible.** This is the same discipline as the four open disagreements above: an
unreconciled overlap that a reader can see beats a clean number that hides a unit mismatch.

### What survives from that slice

- **669 with no mechanism found.**
- **Three timeout populations, uncharacterised** — and the bound is the agent's own: *I never moved
  the deadline, so none can be called a hang.* **A timeout at an unchanged deadline is not evidence
  of a hang**, and none of the three is described as one.

## FIVE OPEN DISAGREEMENTS — recorded to SURVIVE, not to be reconciled

**These are not defects to close. They are competing readings that must both stay on the record until
someone adjudicates them.** Collapsing any of them into a single number would destroy the fact that
two careful agents read the same thing differently.

| disagreement | the competing readings |
|---|---|
| **cluster E** | **143 vs 150 vs `hermit-shell`'s independent read** — three values, not two |
| **six triples** | **330 vs three** |
| **product total** | a **14-cell difference with devbig014** |

Plus the per-cell one from the liteinst cross-check: **`c-programs/clone`, row 27.** `hermit-fixtures`
recorded a disagreement with `hermit-sabre`, **did not reconcile**, and said so: *"their reading and
mine both stand on the record until someone adjudicates."* That is the correct handling and the reason
this section exists.

**None of the four is netted into any count above.**

## Two more results, both downward

**`hermit-codexrev`: 89 of 89 RED.** A whole population, no survivors.

**`hermit-revents`: 5 disqualified of 9 KVM conversions.** More than half of a conversion batch
removed on inspection — again the honest direction.

## The dbt slice amended itself AFTER closing, and the caveat is precise

`hermit-dbt` re-examined its own detector when kvm2's ternary correction landed, and found **the
symbolic blind spot in its own guard picker — worse than the old detector's.** Measured against
literal strings: `return EXIT_FAILURE;` → not matched; `return violations == 0 ? 0 : 1;` → not
matched, **and the old detector caught that one.** **14 of its 47 fixtures use `return EXIT_FAILURE`
as a guard error path.**

**It bounded the blast radius rather than retracting everything:** the regex only chose *which guard
to invert*; it never decided whether a fixture is exit-gated. **So it cannot have manufactured a
refusal** — every refusal in its table is a measured non-zero exit. **A closed slice that amends
itself is the system working.**

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
