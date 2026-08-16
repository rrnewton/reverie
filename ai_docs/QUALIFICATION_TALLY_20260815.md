# Running qualification tally — 380 rows, six subsets

Maintained by `hermit-coord`. **Read from task notes; nothing here was re-scored by me.** Every
number is attributed to the agent that produced it.

## What "qualified" means here — say this with every figure

> **Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false` on
> every accept arm — NOT PARITY AND NOT L2.**

`hermit-revents`' wording, adopted verbatim across all slices. A bare "QUALIFIED" invites the reader
to assume bitwise parity, and **no subset is measuring that.** None of these runs is a validate
profile and **nothing here is a qualifying receipt.**

## Stamp — what this reading saw

**Read 2026-08-16T00:12Z.** Note counts at read time, because the corpus moves while it is being
read and that is not fixable by care:

| subset task | notes read |
|---|---|
| `qualify-queue-rows-01-20` | 5 |
| `qualify-queue-rows-21-40` | 2 |
| `qualify-queue-rows-41-60` | 4 |
| `qualify-queue-rows-61-79` | 2 |
| `qualify-ptrace-replay-81` | 0 |
| `qualify-native-naked-73` | 0 |
| `qualify-kvm-verify-71` | 0 |
| `qualify-dbt-verify-47` | 1 |
| `qualify-sabre-verify-29` | 2 |

## Population — MEASURED HERE from the queue, not taken

Recounted from `ai_docs/QUALIFIABLE_FIXTURE_QUEUE_20260815.tsv`:

| subset | rows | owner | state |
|---|---:|---|---|
| liteinst + verify | **79** | kvm2 1-20 · sabre 21-40 · revents 41-60 · liteinst 61-79 | 54 of 79 scored |
| ptrace + replay | **81** | hermit-shell | no notes |
| native + naked | **73** | hermit-sabre | no notes |
| kvm + verify | **71** | hermit-kvm2 | no notes |
| dbt + verify | **47** | hermit-dbt | claimed; backend probe first |
| sabre + verify | **29** | hermit-codexrev | backend runnability established; 0 cells scored |
| **TOTAL** | **380** | | **54 of 380 scored (14%)** |

## liteinst + verify (79) — the only subset with verdicts

**Backend is named on every verdict, by three different encodings.** They do not aggregate; see the
warning below.

| slice | rows | result | backend measured |
|---|---:|---|---|
| kvm2 1-20 | 20 | **14** qualified on the **ptrace reference arm**; **6** not-exit-gated on ptrace; 0 not-determinable | **liteinst 0 of 20 — runtime unavailable on its binary** |
| sabre 21-40 | 20 | **17** qualified; **2** not-exit-gated; **1** message-bearing | ptrace **and** liteinst; **3 accept under ptrace and FAIL under liteinst** (rows 21, 27, 30) |
| revents 41-60 | 20 | **14** `QUALIFIED-LITEINST`; **1** `QUALIFIED-PTRACE-ONLY` (row 50); **1** `NOT-QUALIFIED` (row 58); 4 not-determinable | ptrace and liteinst |
| liteinst 61-79 | 19 | **0** — runtime build interrupted at exit 130 | none |

### Capability-blocked, kept separate from every other class

- **kvm2:** of its 14, **2 are structurally barred for liteinst by `-524`** (`fork-exec-pipeline`,
  `short-io-split-identity`). **14 → 12 eligible. A falling count, and the honest direction** —
  nothing moved into qualified to compensate. Its ptrace verdicts for those two remain true; the
  cells are re-scoped, not retracted.
- **revents row 50** (`ppoll-simulation`): qualified on the ptrace reference **only**, structurally
  barred on liteinst by `-524 ENOTSUPP`.
- **revents row 58:** barred on liteinst **and** accept fails on ptrace — a distinct class again.
- **`-524` structurally bars 21 of the 79.** **ELIGIBLE IS NOT QUALIFIED.**

### Verdict classes are NOT collapsed

`QUALIFIED` · `NOT-DETERMINABLE` · **capability-blocked** · **accept-only-outstanding** are distinct,
and within not-determinable the slices further distinguish *timeout with a passing accept arm* from
*the mutation family did not apply*. **Merging any of these destroys the information.**

## DO NOT SUM THE "QUALIFIED" COLUMN

Three encodings for one fact, and one is substring-hostile:

- kvm2 — a governing `PROFILE` line per note (*"on the PTRACE backend. NOT liteinst"*)
- sabre — per-row `pt=` / `li=` columns
- revents — the backend in the **verdict name**: `QUALIFIED-LITEINST` vs `QUALIFIED-PTRACE-ONLY`

**`QUALIFIED-PTRACE-ONLY` and `NOT-QUALIFIED` both contain the string `QUALIFIED`.** Any total
matching on that string counts a ptrace-only row and a not-qualified row as qualified. Key on each
slice's own encoding.

## Invocations differ across subsets — state yours

| agent | invocation |
|---|---|
| revents | `hermit run --strict --verify --verify-json` per `ci/test_harness.sh:3467` — the harness's actual cell command |
| kvm2 | `hermit run --strict --verify`, ptrace backend |
| sabre (21-40) | `hermit run --strict` — **no `--verify`** |
| codexrev (sabre+verify) | portable-only flags, focused SaBRe verify probe |

All measure exit-status refusability and each says so; only revents' arm is the command the cell
will actually face.

## Misclassification modes checked, where stated

**Five are now known — the fifth was found by sabre after the brief named four.**

| mode | who checked / found it |
|---|---|
| refuse arm without an accept arm | kvm2 (all 20 accept arms passed) · sabre (rows 23/36/39 caught by it) |
| ran natively / without `--strict` | sabre correction A · revents, before filing |
| stale fixture reads as cannot-fail | kvm2 (found it, row 18) · revents (**graded negative**: all 20 byte-identical to main) · sabre (structurally excluded — reads fixtures via `git show origin/main:`) |
| usage line matched as a cell defect | kvm2 (`usage` inside `getRUSAGE`) · sabre (matched stdout while the line goes to **stderr**) |
| **mutation breaks the argument contract, not the property** | **sabre — the fifth mode.** Rows 36/39 gave clean-looking exit-2 refusals while the property never ran. Only appears on argument-taking cells. |
| per-cell `build.cflags` omitted | kvm2 (8 false "will not compile" → 0) · revents (2 rows need `-pthread`) · sabre (5 rows affected) |

## Durability — where the evidence actually lives

**`revents`' artifacts are in gitignored `ignored/liteinst-qualify-20260815/`.** That survives context
loss and **not host loss**, and devbig014 cannot read it. Same for `kvm2`'s `ignored/kvm2-q0120/`.
**Promotion should be scheduled rather than discovered.**

## Sources are still being written

Every slice above except kvm2's is `IN_PROGRESS`, and kvm2's amended its closed slice after closing.
**This file is a snapshot at the stamp above, not a live mirror.** Re-read the notes rather than
trusting this total, and re-promote wholesale rather than patching it.
