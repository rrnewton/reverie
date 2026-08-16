# Qualification tally — 380 rows, six subsets, 2026-08-15

> **Every number here that survived contact with a second measurement got smaller or grew a caveat.
> None got bigger.**

Maintained by `hermit-coord` from task notes and the artifacts named below. **Nothing was re-scored by
me.** Re-promoted wholesale at each revision, never patched.

**The disagreements come first, deliberately.** They are the part of this record that a summary would
destroy, and the counts further down are the part a reader would have trusted anyway.

## What "qualified" means — say it with every figure

> **Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false` on
> every accept arm — NOT PARITY AND NOT L2.**

`hermit-revents`' wording, adopted verbatim across all subsets. **Nothing here is a qualifying
receipt.**

---

# 1 · FIVE OPEN DISAGREEMENTS — carried unnetted

**These are competing readings, not defects to close.** None is subtracted from any count below.
Collapsing any of them would destroy the fact that two careful agents read the same thing differently.

| # | disagreement | the competing readings |
|---|---|---|
| 1 | **cluster E** | **143 vs 150 vs `hermit-shell`'s independent read** — three values, not two |
| 2 | **six triples** | **330 vs three** |
| 3 | **product total** | a **14-cell difference** with devbig014 |
| 4 | **`c-programs/clone`, row 27** | `hermit-fixtures` disagreed with `hermit-sabre`, **did not reconcile**, and said so: *"their reading and mine both stand on the record until someone adjudicates."* |
| 5 | **CLI-gate exclusion** | **1,344 declarations** (dbt) vs **at most 1,306 cells** (revents) — **DIFFERENT UNITS.** Not subtracted, because *"subtracting a figure I can't reproduce is worse than overlapping."* |

# 2 · ONE RETIRED FIGURE — recorded, not deleted

**`4,750` is retired: not reconstructible in any unit.** It was quoted as the largest unexplained
population **and relayed to devbig014**, so it is in circulation and must be retired visibly.

- **Provenance:** `hermit-revents`' bound — *the CLI gate accounts for at most 1,306 and none of the
  other 4,750.*
- **Retired by** `hermit-dbt`: *not reconstructible from my counts as a pool containing replay and
  chaos.* `6429 − 1306 = 5123`; the only clean route to 4750 is `6429 − 1679`, which subtracts
  **replay** and so contradicts replay being a slice of it.
- **The unit hypothesis was tested and REFUTED:** `1,306 + 4,750 = 6,056`, which matches no derivable
  total — not 7,056 triples, 6,429 disabled declarations, 5,376 cells, 4,751 disabled cells, or 5,206
  red. It cannot be two cell counts either: 6,056 exceeds the whole scorecard.
- **Likely origin, hypothesis not finding:** `6429 − 1678 (custom) = 4751` closes three ways;
  `6429 − 1679 (replay) = 4750` does not. **`custom` and `replay` differ by exactly one.**

**A figure that vanishes silently is worse than one carried with a correction** — someone re-derives
it in a week and nobody knows it was already tested.

# 3 · VERIFICATION RESULT — four of five sets disagreed under independent check

| check | outcome |
|---|---|
| ptrace 25 | **6 disagreements** |
| KVM 43 | **6 disagreements** |
| KVM conversions | **5 disqualified of 9** |
| liteinst 26 | **1 disagreement**, unreconciled |
| — RELAYED, unsourced by me — | three false `NOT-EXIT-GATED` |
| **SaBRe 29** | **0 disagreements — HELD, and one verdict STRENGTHENED** |

**SaBRe alone held, and was strengthened rather than merely confirmed.** The `clone` cell timed out
for both agents, but codexrev had **no control**, which cannot separate a cell property from a busy
host. `hermit-li2` ran **a ptrace control on the same binary at the same moment: rc=0** — so the hang
is **SaBRe-specific**, a stronger claim than the original verdict made. li2 also **stated its prior
and reported against it**: it expected the verdict to break, having overturned five inherited timeout
verdicts an hour earlier, and reported that it did not.

**Its own bound, which belongs beside the number: 21 of the 29 rest on a structural argument and
bookkeeping, not on execution — 16 of 29 by the structural route. A CONFIRMED SET IS NOT A
FULLY-EXECUTED SET.**

# 4 · RESULTS, with the four-state check column

**Four states, not two:** *unchecked* · *checked-and-disagreed* · *checked-and-held* ·
*checked-and-strengthened-with-a-control*.

| subset | scored | result | checked by | outcome |
|---|---:|---|---|---|
| liteinst 1-20 | 20 | 14 qualified **on the ptrace reference arm**, 6 not-exit-gated | hermit-codexrev | dispatched |
| liteinst 21-40 | 20 | 17 qualified, 2 not-exit-gated, 1 message-bearing | hermit-fixtures | **1 disagreement, unreconciled** |
| liteinst 41-60 | 20 | 14 `QUALIFIED-LITEINST`, 1 ptrace-only, 1 not-qualified, 4 ND | hermit-codexrev | dispatched |
| liteinst 61-79 | 19 | 9 `REFUSES-LITEINST` — **subsume the earlier 2, do not add** | hermit-fixtures | held |
| ptrace+replay | 40 of 81 | 25 qualified, 6 not-exit-gated, 9 ND | hermit-sabre | **6 disagreements** |
| kvm+verify | 71 | 20 qualified, 16 not-qualified, 7 not-exit-gated, 28 ND | hermit-revents | **6 disagreements** |
| native+naked | 73 | accept arm fails **71 of 73**, for two opposite reasons | — | unchecked |
| dbt+verify | 47 | closed, then **amended after closing** | self | **detector fault found** |
| sabre+verify | 29 | 12 pass-refusable, 6 no-exit-gate, 8 accept-fail, 3 indeterminate | hermit-li2 | **HELD + strengthened** |

**Do not sum the QUALIFIED column.** `-524` bars 21 of the liteinst 79 — **eligible is not
qualified**. Of kvm's 28 ND, **23 are operator-side: unscored, not unqualifiable.** kvm2 measured
what a substring count costs: **36 against a true 20**, because `NOT-QUALIFIED` nests `QUALIFIED`.
**Count by exact match on the verdict token.**

`native+naked` is on a different channel entirely: its oracle is **native nondeterminism**, requiring
every run to exit 0 **and** ≥2 distinct hashes. **50 of 73 were admitted to the queue *because* they
exit non-zero natively** — the property that qualified them disqualifies them here.

# 5 · THE TWO DERIVATIONS OF THE C4 20 AGREE — and the agreement is worth less than it looks

**20 of 20 agree.** Containment holds: all 20 C4 cells are in the ptrace artifact, none missing.

**They do not share an upstream artefact** — two distinct files, independent runs with their own
timings and binary hashes. **But they share the METHOD completely:** same harness, same command
(`--backend=ptrace record start --strict --verify --verify-json`), same commit `302a1a9c0`.

> **This is a repeat measurement, not an independent derivation.** It bounds flake and says nothing
> about systematic error. Any fault in the harness, command, or oracle is present in both.

**"The 20" is li2's probe set, not the artifact's QUALIFIED set** — they differ by 5
(`file-io-roundtrip`, `fork-exec-pipeline`, `personality-domain`, `short-io-split-identity`,
`signal-delivery-sequence`). **Anyone reading "+20" should know it is 20 of 25.**

# 6 · THE MOST TRANSFERABLE THING IN THIS RECORD

**My first comparison of those 20 cells reported 20 of 20 DISAGREE.**

> **Uniformity across all 20 is the tell — that's a comparator, not twenty independent
> contradictions. It would have produced the single most alarming result available.**

The field is `PASS -` (outcome plus an empty-reason dash) and I compared against `"PASS"`. The rule
that fixed it, stated so it can be checked: **first whitespace token of each field** — `PASS - ` →
`PASS`, `QUALIFIED under ptrace replay` → `QUALIFIED`. Not a substring test, so `NOT-QUALIFIED`
tokenises to itself and cannot nest.

`hermit-fixtures` caught the same class ahead of me: the probe file has **21 lines and 20 cells** —
line 21 is a literal `DONE` sentinel. **A row count taken on faith reports 21.**

**Twenty-four instances of this family today.** The operational form is not *be careful* — vigilance
failed all day. It is: **produce a second measurement by a different route and see whether it
disagrees.**

---

# Provenance, sources, and what is still moving

**Stamp — read 2026-08-16T02:14Z.** Note counts at read time, because the corpus moves while it is
read and that is a property of the task, not something care fixes.

| task | notes | status |
|---|---:|---|
| `qualify-queue-rows-01-20` / `21-40` / `41-60` / `61-79` | 6 / 2 / 4 / 3 | all CLOSED |
| `qualify-ptrace-replay-81` | 4 | CLOSED |
| `qualify-native-naked-73` | 1 | CLOSED |
| `qualify-kvm-verify-71` | 3 | CLOSED |
| `qualify-dbt-verify-47` | 10 | CLOSED |
| `qualify-sabre-verify-29` | 14 | CLOSED |
| `cross-verify-the-liteinst-qualified-verdicts` | 4 | CLOSED |
| **`bound-the-flake-rate-on-the-20-c4-cells`** | 1 | **IN PROGRESS** |

**ARE THE SOURCES STILL BEING WRITTEN? Mostly no, and one yes.** Ten of eleven producing tasks are
closed. **`bound-the-flake-rate-on-the-20-c4-cells` is live**, and its result bears directly on §5 —
the C4 verdicts are single observations awaiting exactly that bound. **Two closed slices were amended
*after* closing** (`qualify-queue-rows-01-20`, `qualify-dbt-verify-47`), so closed does not mean
frozen. **Re-read the notes rather than trusting this file, and re-promote wholesale rather than
patching it.**

**File-backed sources, with digests, so an integrity check means something:**

| source | sha256 (first 16) |
|---|---|
| `ai_docs/QUALIFIABLE_FIXTURE_QUEUE_20260815.tsv` | `a287d2361669d00b` |
| `ai_docs/evidence/four-populations-map-20260815/reasons.json` | `6c8961bcd99c5002` |
| `ignored/xc4-enable-20260815/probe-all.tsv` | `8069b6ba9ec056a7` |
| `ignored/qualify-ptrace-replay-81-20260815/results.jsonl` | `e0a8f7424edb1953` |

**The last two are gitignored** — they survive context loss, not host loss, and devbig014 cannot read
them. **Promotion should be scheduled rather than discovered.**

**A CHECKSUM COMPARES THE FILE, NOT THE KNOWLEDGE.** These digests detect drift in the inputs; they
say nothing about whether a verdict inside them is right. That is what §3's check column is for.

**Verify this artifact by containment, not byte-identity.** Content promoted into a superset returns
a mismatch, and the naive reading is that it was lost.

**And check the tracked count against the intended count.** Promoting evidence earlier today I copied
12 files and **9 landed**; 187 and **186**. A global `*.log` in `.gitignore` applied inside
`ai_docs/evidence/` and `git add` skipped five **without a word**. The commit succeeded, ancestry
verified, and every existence check passed — **because they all asked about files that existed. Only
the count disagreed.**
