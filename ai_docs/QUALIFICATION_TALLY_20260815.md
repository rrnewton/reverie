# Running qualification tally — 380 rows, six subsets

Maintained by `hermit-coord`. **Read from task notes; nothing here was re-scored by me.** Every
number is attributed to the agent that produced it. **Re-promoted wholesale, not patched.**

## What "qualified" means here — say this with every figure

> **Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false` on
> every accept arm — NOT PARITY AND NOT L2.**

`hermit-revents`' wording, adopted verbatim across all slices. No subset is measuring bitwise
parity. **Nothing here is a qualifying receipt.**

## Stamp — what this reading saw

**Read 2026-08-16T00:27Z.** Note counts and status at read time, because the corpus moves while it
is being read and that is a property of the task, not something care fixes.

| subset task | notes | status |
|---|---:|---|
| `qualify-queue-rows-01-20` | 6 | CLOSED |
| `qualify-queue-rows-21-40` | 2 | in progress |
| `qualify-queue-rows-41-60` | 4 | in progress |
| `qualify-queue-rows-61-79` | 2 | in progress — **reassigned to `hermit-li2`**, `hermit-liteinst` hit 100% context |
| `qualify-ptrace-replay-81` | 4 | in progress |
| `qualify-native-naked-73` | 1 | CLOSED |
| `qualify-kvm-verify-71` | 3 | CLOSED |
| `qualify-dbt-verify-47` | 4 | in progress |
| `qualify-sabre-verify-29` | 11 | in progress |

## Population — MEASURED HERE from the queue

| subset | rows | owner | scored |
|---|---:|---|---:|
| liteinst + verify | 79 | kvm2 · sabre · revents · **li2** | 60 |
| ptrace + replay | 81 | hermit-shell | 40 |
| native + naked | 73 | hermit-sabre | 73 |
| kvm + verify | 71 | hermit-kvm2 | 71 |
| dbt + verify | 47 | hermit-dbt | 0 |
| sabre + verify | 29 | hermit-codexrev | 0 |
| **TOTAL** | **380** | | **244 (64%)** |

## Results by subset — **backend named, never summed across backends**

### liteinst + verify (79) — 60 scored

| slice | result | backend measured |
|---|---|---|
| kvm2 1-20 | 14 qualified **on the ptrace reference arm**; 6 not-exit-gated | **liteinst 0 of 20 — runtime unavailable** |
| sabre 21-40 | 17 qualified; 2 not-exit-gated; 1 message-bearing | ptrace **and** liteinst — **3 accept on ptrace, FAIL on liteinst** |
| revents 41-60 | 14 `QUALIFIED-LITEINST`; 1 `QUALIFIED-PTRACE-ONLY`; 1 `NOT-QUALIFIED`; 4 ND | ptrace and liteinst |
| li2 61-79 | 0 of 19 | none yet |

**`-524` structurally bars 21 of the 79.** kvm2's 14 → **12 eligible**; revents' row 50 ptrace-only;
row 58 barred *and* failing accept on ptrace — three distinct classes. **Eligible is not qualified.**

### ptrace + replay (81) — 40 scored, RELAYED from hermit-shell

**QUALIFIED 25 · NOT-EXIT-GATED 6 · NOT-DETERMINABLE 9 = 40.** Counted by **exact equality on the
classification field**. **These 25 are ptrace. The queue exists for liteinst.**

### kvm + verify (71) — 71 of 71 scored, CLOSED

**QUALIFIED 20 · NOT-QUALIFIED 16 · NOT-EXIT-GATED 7 · NOT-DETERMINABLE 28 = 71.** Backend KVM, both
arms. Verdict read from exit status, not `--verify-json` — weaker than revents' JSON read, and named
rather than glossed.

**23 of the 28 NOT-DETERMINABLE are OPERATOR-SIDE:** the enosys/guard mutation patterns did not match
those fixtures' shape, **so no mutant existed and no refuse arm was possible.**

> **UNSCORED IS NOT UNQUALIFIABLE.** This distinction is load-bearing and must not be collapsed. A
> second-family task exists to convert them.

**A correction to a figure in circulation:** *22 of 24 operator-side* is the **interim 63-row**
number. The final, superseding count is **23 of 28**.

**kvm2 measured the substring hazard directly: counting `QUALIFIED` by substring gives 36 against a
true 20 — an 80% overstatement, because `NOT-QUALIFIED` (16) nests the string.** Count by exact
match on the verdict token.

### native + naked (73) — 73 scored, CLOSED, and the mode is on a different channel

**The accept arm fails for 71 of 73, for two OPPOSITE reasons.** `naked` is not an exit-status
oracle: per `ci/test_harness.sh:3570-3602` it is a conjunction — every native run must exit 0 **and**
the runs must produce ≥2 distinct output hashes. Its oracle is **native nondeterminism**.

- **50 of 73** were admitted to the queue *because they exit non-zero natively* — precisely what
  `failed_runs > 0` fails on. **The property that qualified them for the queue disqualifies them in
  this mode.** Not a defect in either artifact.
- **21 of the remaining 23** are natively deterministic → fail the `min_distinct ≥ 2` conjunct.
- **2 pass both conjuncts** (3 distinct outcomes each). Whole sub-population measured, 0 unmeasurable.

**Not fixable by fixture gating:** naked wants nondeterminism, and these are determinism witnesses
built to be deterministic. Gating them harder makes them worse candidates.

### dbt + verify (47) and sabre + verify (29) — 0 scored

dbt: backend probe first. codexrev: runnability established; cells 22/23/25 **unscored** pending a
rerun with ptrace-calibrated `guest_args` (a manifest-calibration gap, not a cell property); cell 21
timeout and cell 24 accept failure retained separately.

## Fixture repair — RELAYED from hermit-fixtures, a different axis from qualification

**16 fixtures made exit-gated, each at 80 runs with zero variation.** **Nine deliberately left
ungated and named:** `cachestat-refusal`, `fallocate-extents`, `kcmp-refusal`, `mempolicy-default`,
`mknod-special`, `no-new-privs-refusal`, `openat2-refusal`, `readdir-entries`,
`statfs-free-determinism` — **because gating at a merely-observed number would make them flaky, and
flaky is worse than unable to fail.**

## DO NOT SUM THE "QUALIFIED" COLUMN

Verdict classes are distinct and must not be collapsed: **QUALIFIED · NOT-DETERMINABLE ·
capability-blocked · accept-only-outstanding · unscored-operator-side**.

Encodings differ per slice — a governing `PROFILE` line (kvm2), per-row `pt=`/`li=` columns (sabre),
the backend in the verdict name (revents). **`QUALIFIED-PTRACE-ONLY` and `NOT-QUALIFIED` both contain
`QUALIFIED`**, and kvm2 measured what that costs: **36 against a true 20.** Key on each slice's own
encoding, by exact match.

## Provenance of this file — and the failure it is most exposed to

**An existence check cannot see a file that was never written.** Promoting the two evidence
directories earlier today, I copied 12 files and **9 landed tracked**; 187 and **186**. A global
`*.log` in `.gitignore` applied inside `ai_docs/evidence/` and `git add` skipped five files **without
a word**. The commit succeeded, ancestry verified, and read-back matched on every file that was
there — **every check passed, because they all asked about files that existed. Only the count
disagreed.**

**This tally is exactly the kind of artifact that failure produces silently:** a total that looks
complete because everything it lists is real. **Check the tracked count against the intended count**,
and treat any figure here as provisional against the note counts in the stamp above.

**Sources are still being written.** Four subsets are in progress and two closed slices were amended
after closing. Re-read the notes; re-promote wholesale rather than patching.
