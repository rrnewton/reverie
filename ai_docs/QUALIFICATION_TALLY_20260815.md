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

# 1 · SEVEN OPEN DISAGREEMENTS — carried unnetted

**These are competing readings, not defects to close.** None is subtracted from any count below.
Collapsing any of them would destroy the fact that two careful agents read the same thing differently.

| # | disagreement | the competing readings |
|---|---|---|
| 1 | **cluster E** | **143 vs 150 vs `hermit-shell`'s independent read** — three values, not two |
| 2 | **six triples** | **330 vs three** |
| 3 | **product total** | a **14-cell difference** with devbig014 |
| 4 | **`c-programs/clone`, row 27** | `hermit-fixtures` disagreed with `hermit-sabre`, **did not reconcile**, and said so: *"their reading and mine both stand on the record until someone adjudicates."* |
| 5 | **CLI-gate exclusion** | **1,344 declarations** (dbt) vs **at most 1,306 cells** (revents) — **DIFFERENT UNITS.** Not subtracted, because *"subtracting a figure I can't reproduce is worse than overlapping."* |
| 6 | **`backend-parity-c/sendfile-copy` scaffolding-only flag** | `hermit-fixtures` relayed the flag and asked for a check; **`hermit-revents` derived it and REFUTED it** — *"the flag as stated doesn't block it."* **Did not reconcile.** Added 2026-08-16; see §7 |
| 7 | **what fault produces the `getrusage-self-accounting` flake** | the measuring agent's note says `reverie_process::clone::clone_with_stack::callback`; the owner relay says **`timer.rs:982`, five captures, hermit #1845**, with `clone_with_stack` a SEPARATE fault held by hermit-panic and **an explicit instruction not to merge them.** Added 2026-08-16; see §7 |

**Two disagreements were added on 2026-08-16 and NONE was closed.** The count moved 5 → 7 because
new readings arrived, not because old ones were adjudicated.

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

> **The bound §5 was waiting for has arrived — see §7. It did not confirm the 20; it split them
> 19-and-1 and found two more that cannot fail.**

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

> **§7 carries the strongest instance of all, and it is not a matcher — it is an ORACLE.** A
> `(char)i → (char)(i+1)` mutation changes `.text` and leaves a checksum invariant, because over
> `0..255` it is a permutation. **The instrument that reads the binary says "bites"; the instrument
> that reads the observable says "nothing happened."** Same rule, one layer down: the second
> measurement has to come from a different ROUTE, and a different section of the same binary is not
> a different route.

# 7 · THE LAST LIVE INPUT IS IN — AND IT MADE §5 SMALLER, NOT SURER

**Recorded 2026-08-16, re-derivation PENDING. This section is not settled and must not be read as
settled.** It is here because the input arrived, not because the question closed.

**RELAYED, NOT MEASURED HERE.** Everything in this section was produced by other agents. I ran none
of it. The flake numbers are read from `bound-the-flake-rate-on-the-20-c4-cells`'s own closeout note;
the re-derivation status is read from the owner's relay and from
`which-cannot-fail-verdicts-rest-on-a-binary-keyed-bite-proof`.

## The bound: the +20 is 19 bounded cells and one measured flake

| what | figure | denominator |
|---|---:|---|
| `backend-parity-c/getrusage-self-accounting` | **0.83%** | 5 failures / 600 runs |
| each of the other 19, individually | **≤ 2.95%** | 100 runs per cell, upper bound |
| the other 19, pooled | **≤ 0.158%** | 1,900 runs |

**Mechanism: NOT SETTLED. Two attributions are on record and they name different faults.** Recorded
unnetted; do not merge them, and note that an instruction not to merge them has been given
explicitly.

| source | fault named for getrusage's failures | disposition |
|---|---|---|
| `bound-the-flake-rate-on-the-20-c4-cells` closeout note | non-unwinding panic in `reverie_process::clone::clone_with_stack::callback`, surfacing as SIGSEGV | the measuring agent's own reading |
| owner relay, 2026-08-16 | **`timer.rs:982`, five captures** — filed as **hermit #1845** | **`clone_with_stack` is a SEPARATE fault, held by hermit-panic** |

**The counts line up with the second reading** — five captures against 5 failures / 600 runs — but
**that is agreement in a count, not in a mechanism**, and it is not treated here as settling which
fault produced the flake. **Whichever it is, it is not the fixture's fault:** the fixture creates no
child at all — no `fork`, `clone`, `vfork`, `posix_spawn` or `pthread_create`.

**Both faults are now filed** (#1845 for the timer panic; hermit-panic holds the `clone_with_stack`
SIGSEGV), which supersedes this section's earlier note that no task had been opened.

**A UNIFORM RATE IS EXCLUDED, A SHARED LOWER RATE IS NOT.** If every cell had getrusage's rate,
P(0 failures across the other 19 cells' 1,900 runs) = 1.2e-07. The author states plainly that the
per-cell 2.95% bounds do **not** individually exclude a lower shared rate and does not claim they do.
**100 runs cannot call a cell clean against a 0.83% rate** — P(0 failures in 100 runs) = 0.43.

**SCOPE, because the profile decides what the number means:** devbig030, **PORTABLE LANE**, backend
ptrace, mode replay, debug build at `302a1a9c0fde564db0292d1ea5cabc91343e79bc`, via
`ci/test_harness.sh run --probe-disabled`, read-only, nothing enabled. **A PASS HERE IS NOT A
RECEIPT.** Runs were 8-way concurrent at load 42–76, which biases against a clean result rather than
toward one. Evidence at `ignored/evidence/c4-flake-bound-20260815/` — **gitignored, so it survives
context loss but not host loss, and devbig014 cannot read it.**

## Two of the 20 QUALIFIED cannot fail — and they are inside the 20, not the 6

Under replay the harness verdict is **exit status and nothing else**, so a fixture whose only
non-zero exits are scaffolding books a green no wrong answer can turn red.

| cell | basis | status |
|---|---|---|
| `c-programs/clone` | derived by the author with a discriminating mutant pair: a PROPERTY mutation (the `CLONE_VM` child writes the wrong string through shared memory) is visible in stdout and **still exits 0**; only inverting the scaffolding guard `if (pid == -1)` moves the exit | **derived** |
| `backend-parity-c/sendfile-copy` | ~~relayed by three agents, never derived~~ → **DERIVED ONCE AND REFUTED** | **the flag does NOT hold — see below** |

**`sendfile-copy`'s flag was REFUTED, and the refutation is not reconciled.** hermit-revents derived
it independently and reports: *"on my evidence the plan would not be enabling a cell that cannot
express failure — the flag as stated doesn't block it."* **It did not reconcile with
hermit-fixtures**, which relayed the flag and explicitly asked for the check. So the state is
**derived once and refuted, with the disagreement standing** — not "relayed three times and never
derived", which is what an earlier reading of this row said. **Both positions are recorded; neither
is netted into a count.**

**This changes what "+20" buys and it is not netted into any count above.**

## The instrument finding the refutation produced — a permutation defeats a `.text`-keyed proof

**This is the sharpest one in the record and it generalises past this cell.** hermit-revents' first
ungated mutation was `(char)i → (char)(i+1)`.

- **The `.text` section DIFFERED, so a binary-keyed bite proof PASSED.**
- **The checksum stayed identical at 32640** — over `0..255` that mutation is a **permutation**, and
  a **sum is invariant under a permutation**.

In the author's words: *scoring on `.text` alone would have let me report "bites and still exits 0"
from a mutation that changed nothing observable.* **A binary that differs is not a behaviour that
differs.** This is the successor to the earlier finding that a whole-binary diff is defeated by the
ELF build ID: comparing `.text` fixes that one and does **not** fix this one.

**hermit-sabre bounded the damage, and the bound matters as much as the finding.** A `.text`-keyed
proof gives **SOUND POSITIVES AND UNTRUSTWORTHY NEGATIVES.** It does **not** ungrade every verdict —
**if the exit status moved, that IS the evidence.** Exposure is confined to the **NOT-EXIT-GATED**
calls. An earlier reading that it ungraded everything was too broad and is corrected here.

## The six-cell re-derivation: RESOLVED — all six are exit-gated

**hermit-observable re-derived all six on the observable and all six hold.** Closed through
`./ci-hub/bin/close-task`, code ref `5c5effde`, VERIFIED.

| what | figure |
|---|---:|
| mutations moving the replay exit status `0 → 1` | **12 of 12** |
| independent mutation families | **2** |
| accept arms | replay rc=0 / verify rc=0 |
| mutant arms | replay rc=1 / verify rc=1 |

**Two families, not two runs of one:** **M1 is BEHAVIOUR** — a different call issued at the site.
**M2 is ORACLE** — the same call with a changed expectation.

**THIS IS GENUINE INDEPENDENT CONFIRMATION AND IT PASSED THE SHARED-ARTIFACT TEST EXPLICITLY.** In
the agent's own words: it agrees with hermit-fixtures' verdict on **different mutations, a different
binary, and a different observable record**; it did **not** reconcile with hermit-fixtures, and read
that artifact **only to learn which six and which mutations to avoid duplicating.** **The agent
stated the distinction rather than leaving us to infer it** — and confusing two observations with one
observation reported twice is what produced the most confident wrong answer of the day (§5, §6).

**RECORD BOTH HALVES.** hermit-fixtures restored the six on a `.text`-keyed proof and then **disowned
its own result**, flagging it as resting on the wrong instrument. The re-derivation on the correct
observable confirms all six. **The self-flag is resolved in hermit-fixtures' favour, and the
disowning is why the confirmation is worth anything** — a tally carrying only the confirmation would
be quoting a number whose author had withdrawn it, without the withdrawal or the repair.

**This closes the tally's last live input.** What remains open are the seven disagreements in §1,
which are carried, not pending.

## How the two re-derivation claims resolved

**Two claims arrived about re-derivation and I am recording them separately because I could not
reconcile them and netting them would invent a number nobody measured.**

- **From the owner's relay:** six cells restored, all six flagged by **hermit-fixtures** as resting on
  a `.text`-keyed bite proof and needing re-derivation; `c-programs/clone` reconfirmed
  scaffolding-only through the replay path; the six being re-derived on the observable.
  **RESOLVED — see the section above: all six re-derive as exit-gated, 12 of 12.** (When first
  recorded at 02:55 I could not verify this: `tg search "hermit-fixtures"` returned no results and
  there was no durable note to cite. The evidence landed as
  `ai_docs/evidence/observable-rederive-six-restored-cells-20260815/` at `5c5effde`.)
- **From `which-cannot-fail-verdicts-rest-on-a-binary-keyed-bite-proof` (CLOSED):** two binary-keyed
  sets cover **8 distinct cells**; **six of the eight have already been re-derived as QUALIFIED by an
  observable-keyed method** (hermit-sabre, via name-to-handle-refusal, seccomp-refusal and
  sysv-ipc-refusal). The remaining deliverable is **two cells nobody has challenged.** That audit
  declared **no verdict false** — its output is which verdicts need re-derivation, not a claim that
  any is wrong.

**Whether these two sixes are the same six was UNRESOLVED at 02:55 and is now moot for the tally:**
both routes end with the six confirmed on an observable-keyed method — hermit-sabre's earlier
six-of-eight, and hermit-observable's 12-of-12 across two families. **They are still not proven to be
the same set, and they are still not added together.** What changed is that the tally no longer waits
on either.

**AND THE AUDIT CAUGHT ITSELF MAKING THE DAY'S SIGNATURE MISTAKE:** its first pass grepped whole
`tg -v` output **including each task's brief — and the briefs discuss `.text`** — so it attributed
bite-proof language to tasks that had none, including one with no notes at all. **The instrument was
reading the instructions rather than the evidence.** That is the same matcher family as §6.

---

# Provenance, sources, and what is still moving

**Stamp — read 2026-08-16T02:14Z; §7 added 02:55Z; retracted-and-extended 03:05Z; COMPLETED 03:12Z
when the last live input landed.** Note counts at read time, because the corpus moves while it is
read and that is a property of the task, not something care fixes.

**§7 needed a retraction ten minutes after it was written** — the `sendfile-copy` flag (derived and
refuted) and "no task has been opened" (both faults filed) — and §1 moved from five open
disagreements to seven. **A section that young needing retraction is the argument for re-reading
sources rather than trusting this file**, and it is the reason the closing instruction below says
what it says.

**THIS FILE IS NOW COMPLETE AND IS NOT THE PRIMARY SOURCE FOR ANY FIGURE IN IT.** Every producing
task is closed; nothing is pending. **Verify against the artifacts that produced the numbers, not
against the notes quoting them, and not against this file.**

| task | notes | status |
|---|---:|---|
| `qualify-queue-rows-01-20` / `21-40` / `41-60` / `61-79` | 6 / 2 / 4 / 3 | all CLOSED |
| `qualify-ptrace-replay-81` | 4 | CLOSED |
| `qualify-native-naked-73` | 1 | CLOSED |
| `qualify-kvm-verify-71` | 3 | CLOSED |
| `qualify-dbt-verify-47` | 10 | CLOSED |
| `qualify-sabre-verify-29` | 14 | CLOSED |
| `cross-verify-the-liteinst-qualified-verdicts` | 4 | CLOSED |
| **`bound-the-flake-rate-on-the-20-c4-cells`** | **3** | **CLOSED 2026-08-16** — result in §7 |
| `which-cannot-fail-verdicts-rest-on-a-binary-keyed-bite-proof` | — | CLOSED — bears on §7 |

**ARE THE SOURCES STILL BEING WRITTEN? No. Every producing task is closed and the last live input is
in.** `bound-the-flake-rate-on-the-20-c4-cells` closed and its result is in §7 — it did not confirm
§5's 20, it split them into 19 bounded cells plus one measured flake. The six-cell re-derivation
closed at `5c5effde` with all six confirmed exit-gated, 12 of 12 across two families.

**COMPLETE IS NOT SETTLED.** Seven disagreements in §1 are **carried, not pending** — nobody has
adjudicated them and this file does not. **The mechanism behind the getrusage flake is contested
(§1 row 7) and the `sendfile-copy` flag is refuted-but-unreconciled (§1 row 6).** A reader who wants
one number from this file will not find one, and that is the result, not a gap in it.
**Two closed slices were amended
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
| `ai_docs/evidence/observable-rederive-six-restored-cells-20260815/REPORT.md` | `158b652baf1650ef` |
| `ai_docs/evidence/observable-rederive-six-restored-cells-20260815/results-pass1.json` | `2c81bc31647d0fcd` |

**The last two are TRACKED, landed at `5c5effde`** — unlike the gitignored sources above, they are
readable from a fresh clone and by devbig014. That is the promotion this file kept asking for,
performed by their author rather than discovered later.

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
