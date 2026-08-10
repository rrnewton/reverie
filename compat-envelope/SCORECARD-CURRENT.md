# Compat-Envelope Scorecard — Current Rendered Output

> ## 🕐 HISTORICAL — SUPERSEDED FOR "WHAT DOES MAIN DO TODAY?"
>
> **Its Tables 1, 3 and 4 are 2026-08-01 measurements at Hermit `82a8e853`, and
> Hermit `main` has advanced by dozens of commits since** (this is Limitation L5
> below, promoted here because the numbers are quoted more often than the
> limitation is read). Nothing here was re-measured after 2026-08-05.
>
> For a **same-corpus, single-SHA measurement on current `main`**, read
> [`pre-tightening-baseline-20260806/README.md`](pre-tightening-baseline-20260806/README.md)
> — Hermit `4c70658e`, Reverie `dd3c178e`, measured 2026-08-07T02:17Z–03:47Z on
> the same host. That file is itself explicitly labelled **PRE-TIGHTENING**: it
> is the matched before-state for an upcoming strictness tightening of the
> comparison contract, not a replacement status page.
>
> **Do not diff this file's Table 1 against that one row-for-row.** The corpus
> is not the same population: this file's 200-cell/backend sweep predates the
> 30-cell `performance` bucket that the parent corpus now lists, and 30 of those
> cells are unbuildable on current Hermit `main` anyway. Denominators differ
> (179 here vs 182 there) and the backend sets differ (`kvm` was measurable on
> 2026-08-01 and is not measurable today).
>
> This file stays in place as the standing rendering of the four committed CSVs
> and as the reference for the certification-tier and denominator caveats in §6,
> which all still apply. Replace it when the collectors are re-run into those
> CSVs.

> ## Strict comparison-tier gate added 2026-08-06
>
> The four live CSVs now carry `comparison_tier` on every one of **2,284/2,284
> rows**. Their distribution is **2,284 `legacy-unqualified`**, **0
> `full-stdout-info-stack-heap`**, and **0
> `stdout-info-stack-heap-spot-check`**. The 1,837 historical raw
> `outcome=pass` rows therefore yield **0/1,837 qualified greens**. The tables
> below remain the historical stdout/stripped rendering; the current renderer
> refuses to emit them as strict green until a clean re-baseline records one of
> the two qualifying tiers. Reproduce the distribution with
> `compat-envelope/check-scorecard-tier.py`.

> ## Current-view rewrite and optional stronger-mode grade (2026-08-10)
>
> `scorecard.csv` is now the **current view**, newest physical observation per
> `(test_id, backend)`: **541/541 current cells**. Its source ledger is
> `history/scorecard-observations.csv`, which retains **624/624 historical
> observations**, including **83 superseded rows**. The change from 624 stored
> rows to 541 current cells is a **counting correction — neither progress nor
> regression**. Coverage history was not deleted.
>
> All four live scorecards now have optional `stack_parity` and `heap_parity`
> mode columns: true means the corresponding `--detlog-stack` or
> `--detlog-heap` flag was passed, false means it was not, and historical blank
> means unknown. They are inputs, not comparison verdicts.
> For the selected current CI/regression scope, base green is **0/541 current
> cells** and the higher-grade query is **0/0 highest-grade greens among
> qualified greens**. There is no base-green population from which a
> highest-grade numerator can yet be earned. This does not narrow the existing
> qualification standard; it adds a queryable stronger-mode grade on top of
> the existing green verdict.

### Fresh strict-green snapshot for owner review (2026-08-10)

Generated `2026-08-10T15:09:11Z` on `devbig014` from the append-only history,
using the strict-green decision procedure committed at
`8ae767cacd23571f2f5c15ff2e283d466f835298`.

- Artifact: `compat-envelope/scorecard.csv`, SHA-256
  `a49bc29189c992700da4b79f8cc70f6153009036ab9bdc7656911647fd33d0d3`.
- Selection scope: newest physical observation per `(test_id, backend)` in
  `compat-envelope/history/scorecard-observations.csv`.
- Stored/current/superseded: **624/624 stored observations**, **541/624 current
  cells**, and **83/624 superseded observations**. The 624-to-541 change is the
  same coverage counted correctly, **neither progress nor regression**.
- Strict verdict: **0/541 base-green current cells** and **541/541 red current
  cells**. Green means two complete runs on the declared determinism-or-parity
  axis with identical output and real comparison evidence; incomplete,
  absent, crashed, divergent, or errored comparisons are red.
- Higher grade: **0/0 highest-grade green among base-green cells** (no
  base-green population). `stack_parity` and `heap_parity` are mode inputs,
  not outcomes, and both historical modes are unknown on **541/541 current
  cells**.
- Evidence blind spots: stdout equality is operand-bound on **0/541 current
  cells**; nonempty stack digests exist on **6/541 current cells** and nonempty
  heap digests on **6/541 current cells**; exact clean Hermit+Reverie product
  provenance exists on **446/541 current cells**; hosted-authority evidence is
  bound to **0/541 current cells**. All **541/541 current cells** retain the
  `legacy-unqualified` comparison tier.
- State separation: product=`RED under the strict-green application standard`;
  gate/producer=`NO_RESULT in this artifact`; reporting=`NORMAL`.

The separate `run_matrix.py --check` compatibility-contract catalog results
(ptrace **28/28**, DBT **26/28**, KVM **27/28**) count declared compatibility
contracts, not strict-green scorecard cells, and do not contribute to either
scorecard numerator above.

**This file is the clear, human-readable rendering of the four scorecard CSVs
that sit beside it.** It is the entry point: `README.md` documents the *system*,
`SCORECARD.md` and `REPORT.md` are older narrative analyses written on 2026-08-04
against an earlier corpus, and this file is the *current numbers* with every
column, denominator, and certification limit stated inline.

- **Rendered:** 2026-08-07T01:13Z
- **Rendered by:** `compat-envelope/render-current-scorecard.sh` (wraps
  `render-scorecard.rs`; re-run it and diff to detect drift)
- **Rendered from:** the four CSVs listed in §1, at the exact blob hashes given
  there — not from a branch name, not from a live run.
- **Nothing here was re-measured for this document.** Publishing is a rendering
  step. The measurements are whatever the collectors last wrote into the CSVs.

> ## ⚠️ READ THIS BEFORE QUOTING ANY NUMBER
>
> **No cell in any table below is a bitwise-parity certification.** Every
> "verified"/"deterministic" cell in these CSVs was produced by a **stripped**
> comparator, and a stripped comparator has been measured to miss real
> divergence. See **§6 Limitation L1**, which is not a caveat on the margins —
> it is the tier of the whole scorecard. Read a green cell as
> *"agreed after normalization"*, never as *"byte-identical"*.

---

## 1. Provenance — what these numbers were computed from

Evidence binds to content, not to a branch. Quote the blob hash.

| CSV | blob (content id) | rows (excl. header) | cols | last content commit |
| --- | --- | ---: | ---: | --- |
| `fullcorpus-scorecard.csv` | `d25e784e28b544240203fc02af522793297eb9cf` | 1200 | 33 | `0c38fb3773d0300c46b68fa48561c932b9dc524f` |
| `scorecard.csv` | `1ef205fa71d1180aafb4720afd81d5c69aff23e7` | 618 | 33 | `0c38fb3773d0300c46b68fa48561c932b9dc524f` |
| `reverie-scorecard.csv` | `0f46d77a991cc3c458f684551ccc1b09dfa6e24a` | 12 | 34 | `0c38fb3773d0300c46b68fa48561c932b9dc524f` |
| `e9patch-scorecard.csv` | `0f94dff27fa394734d90a04fae12bcb519ccd031` | 454 | 36 | `0c38fb3773d0300c46b68fa48561c932b9dc524f` |

`20b4a7d` = *"compat-envelope: name parity observables in CSV schema"*,
2026-08-06 17:15:25 -0700.

### Source runs (the `run_id` → SHA/time mapping the CSVs carry per row)

**`fullcorpus-scorecard.csv`** — one 200-cell sweep per backend, all six at the
same product SHAs, all `dirty=false`:

| run_id | cells | hermit SHA | reverie SHA | run UTC |
| --- | ---: | --- | --- | --- |
| `ptrace-fullcorpus-scorecard` | 200 | `82a8e853357584a3a567fd80812e015572a607c7` | `a4f33d69a56ed4233a53b218c39d93807ffc8cd0` | 2026-08-01T21:56:49Z |
| `kvm-fullcorpus-scorecard` | 200 | `82a8e853…` | `a4f33d69…` | 2026-08-01T21:48:44Z–21:58:54Z |
| `liteinst-fullcorpus-scorecard` | 200 | `82a8e853…` | `a4f33d69…` | 2026-08-01T22:01:37Z |
| `dbi-fullcorpus-scorecard` | 200 | `82a8e853…` | `a4f33d69…` | 2026-08-01T22:25:21Z |
| `sabre-fullcorpus-scorecard` | 200 | `82a8e853…` | `a4f33d69…` | 2026-08-01T22:32:24Z |
| `e9patch-fullcorpus-scorecard` | 200 | `82a8e853…` | `a4f33d69…` | 2026-08-01T22:39:36Z |

**`scorecard.csv`** — **mixed provenance across nine runs at seven different
Hermit SHAs spanning 2026-08-01 → 2026-08-05.** This matters: the `--all`
rendering is last-writer-wins per cell, so Table 2 is a *composite*, not a
snapshot of any single commit. See Limitation L4.

| run_id | rows | hermit SHA | run UTC |
| --- | ---: | --- | --- |
| `kvm-fullcorpus-scorecard` | 200 | `82a8e853…` | 2026-08-01T21:48Z |
| `liteinst-fullcorpus-1785621912` | 200 | `464cbd9f9bb43d5505c914783819e1d349630283` | 2026-08-01T22:05Z |
| `canonical-release-ptrace-dbi` | 46 | `9429005ca04b6ae0b3d0aedbdc18969f3b770603` | 2026-08-01T18:25Z |
| `liteinst-spst-1785620995` | 40 | `464cbd9f…` | 2026-08-01T21:49Z |
| `backend-parity-75edd7455dc9-…` | 28 | `75edd7455dc99f26953c06d8b2c8fb757c580c04` | 2026-08-05T05:50Z |
| `backend-parity-52d56e5ceb38-…` | 28 | `52d56e5ceb386d24ec809edbfdb6920e8484271e` | 2026-08-05T06:45Z |
| `backend-parity-fc49593ac21c-…` | 28 | `fc49593ac21c7655e841a3de825ef86692ad117c` | 2026-08-05T07:24Z |
| `backend-parity-09d7bd0c6f98-…1561902` | 24 | `09d7bd0c6f9833a51e4681357c552d24b71b6cf1` | 2026-08-03T01:34Z |
| `backend-parity-09d7bd0c6f98-…1586797` | 24 | `09d7bd0c6f98…` | 2026-08-03T01:35Z |

**`reverie-scorecard.csv`** — `reverie-20260801`, 12 rows, hermit
`2f3689bd8830ab6b59dacea6cb72951f4d0d899e`, reverie `a4f33d69…`,
2026-08-01T15:30:40Z.

**`e9patch-scorecard.csv`** — `e9patch-20260801`, 454 rows, hermit
`b1fdeaf6d7bcda0799a7a5c4f116bbe1ed55a43d`, reverie
`2112c0045f25f895388257caed43b7b5abb9b50a`, 2026-08-01T21:55:26Z.

**Host:** all sweeps ran on the shared 316-core devbig with `/dev/kvm`. Wall and
memory figures in the CSVs are load-contended and are not benchmark-grade; the
pass/parity/determinism fields are what this scorecard reports.

---

## 2. Table 1 — full corpus (the definition-of-done denominator)

`render-scorecard.rs --csv fullcorpus-scorecard.csv --observable stdout --all`

```
bucket                  ptrace               dbi               kvm             sabre          liteinst
------------------------------------------------------------------------------------------------------
applications                 1        100%, 100%        100%, 100%        100%, 100%            0%, 0%
backend-parity-c             3        100%, 100%         67%, 100%          67%, 67%          67%, 67%
bin-c                        1        100%, 100%            0%, 0%        100%, 100%        100%, 100%
c-programs                 149          80%, 89%          66%, 74%          82%, 92%          69%, 75%
chaos-c                      1        100%, 100%        100%, 100%        100%, 100%            0%, 0%
data-handling                0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
debugger-c                   1        100%, 100%          0%, 100%        100%, 100%        100%, 100%
determinism-stress           2          50%, 50%        100%, 100%        100%, 100%            0%, 0%
determinism-stress-c         9          67%, 78%          33%, 33%         89%, 100%            0%, 0%
language-runtimes            6          17%, 50%          33%, 50%          17%, 67%            0%, 0%
shared-futex-c               0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
system-utils                 6          33%, 83%          33%, 83%         33%, 100%          17%, 33%
util-c                       0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
------------------------------------------------------------------------------------------------------
TOTAL                      179          76%, 87%          63%, 72%          79%, 92%          60%, 66%
```

Exact fractions from the `--tsv` projection (`X/Y` = measured/ran):

| backend | stdout-parity% *(unqualified)* | determinism% | qualified measured | ran | raw outcomes over 200 cells |
| --- | ---: | ---: | ---: | ---: | --- |
| ptrace *(reference)* | — | — | 0/179 *(unqualified)* | 200/200 | 179 pass, 20 diverge, 1 timeout |
| dbi | 76.0 | 86.6 | **0**/179 *(unqualified)* | 179/179 | 156 pass, 36 diverge, 8 timeout |
| kvm | 62.6 | 72.1 | **0**/179 *(unqualified)* | 179/179 | 130 pass, 70 fail |
| sabre | 78.8 | 91.6 | **0**/179 *(unqualified)* | 179/179 | 164 pass, 30 diverge, 6 timeout |
| liteinst | 60.3 | 65.9 | **0**/179 *(unqualified)* | 179/179 | 118 pass, 82 diverge |

**Denominator: 179.** The corpus is 200 ptrace-verify cells (13 buckets, static
parse of `hermit/tests/e2e/manifests/*.toml`); **179 of those 200 passed the
golden ptrace `--strict --verify` leg**, and only those 179 are eligible to be a
denominator for another backend. The 21 ptrace non-passes (20 diverge, 1
timeout) are excluded from every percentage in this table — they are a ptrace
gap, not a backend gap, and they are not counted against dbi/kvm/sabre/liteinst.

**Coverage is NOT complete here, and the previous wording of this paragraph was
wrong.** It read "Coverage is complete here: every backend column reads
`179/179` measured", which was false. `179/179` counts cells that RAN. The
QUALIFIED observable `stdout_parity` is **blank on 1000 of 1000 rows** in
`fullcorpus-scorecard.csv` — all five backends, 200 rows each — and
`comparison_tier` is `legacy-unqualified` on all 1000. Every percentage in this
table is therefore derived from `legacy_parity_unqualified`, a column whose name
states that it does not qualify (sabre: 142/180 = 78.9%, the published 78.8).
**Qualified measured is 0/179 for every backend**, which is why that column now
reads `0` rather than `179`. The numbers are retained because the underlying
comparison did happen and is real history; they are marked unqualified because
they were not produced under the current comparison tier. `render-scorecard.rs`
REFUSES this data outright today (exit 3, "qualified green=0/179 raw passes"), so
this table is a stale cache of a superseded computation and cannot be reproduced
from its own inputs by its own renderer. Guarded by
`tests/test_headline_measured_count_is_real.sh`. The three
buckets showing `0` ptrace (`data-handling`, `shared-futex-c`, `util-c`) have an
**empty denominator** — their `0%, 0%` is `0/0`, i.e. *no information*, not a
confirmed failure (Limitation L3).

`e9patch` is measured in this CSV too (179 pass / 20 diverge / 1 timeout —
identical to ptrace) but is deliberately not a column here: it is preprocessing
over the ptrace backend, not a Detcore backend. It gets its own table (§5).

---

## 3. Table 2 — regression / CI envelope

`render-scorecard.rs --csv scorecard.csv --observable stdout --all`

```
bucket                  ptrace               dbi               kvm             sabre          liteinst
------------------------------------------------------------------------------------------------------
applications                 1               n/a        100%, 100%               n/a            0%, 0%
backend-parity              24               n/a               n/a               n/a               n/a
backend-parity-c             0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
backend-parity-spst         20               n/a               n/a               n/a          90%, 95%
bin-c                        0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
c-programs                   8        100%, 100%        100%, 100%               n/a        100%, 100%
chaos-c                      0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
data-handling                2               n/a            0%, 0%               n/a               n/a
debugger-c                   0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
determinism-stress           4               n/a          50%, 50%               n/a               n/a
determinism-stress-c         1               n/a        100%, 100%               n/a               n/a
language-runtimes            6               n/a          33%, 50%               n/a           0%~, 0%
shared-futex-c               0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
system-utils                 6               n/a         17%~, 67%               n/a         17%~, 33%
util-c                       0            0%, 0%            0%, 0%            0%, 0%            0%, 0%
------------------------------------------------------------------------------------------------------
TOTAL                       72         11%~, 11%         21%~, 26%               n/a         38%~, 40%
```

**Do not read this table as "the backends got worse."** It is a *different and
much smaller population* than Table 1 (72-cell denominator vs 179), assembled
from nine runs at seven Hermit SHAs (§1). Its low totals are dominated by
**coverage**, not by failure: `~` marks partial measurement and `n/a` marks a
backend that ran zero cells in that bucket. Table 1 is the compatibility
statement; Table 2 is the CI-envelope statement.

The `backend-parity` (24) and `backend-parity-spst` (20) buckets are the
focused contract matrix appended directly by Hermit's
`tests/backend-parity/run_matrix.py`; they have no counterpart in Table 1.

---

## 4. Table 3 — Reverie B1.5 Guest/Tool boundary

`render-scorecard.rs --csv reverie-scorecard.csv --denominator counter
--backends kvm --observable tool-count --all`

```
bucket                  ptrace               kvm
------------------------------------------------
reverie-examples             6          0%, 100%
------------------------------------------------
TOTAL                        6          0%, 100%
```

Different observable: **`tool-count-parity%`**, not stdout. It compares the
total number of syscall events the shared Reverie counter Tool observes through
the ptrace launcher vs the KVM launcher. `0%, 100%` means KVM is fully
self-deterministic but surfaces a **constant 4 fewer syscalls** to the Tool
callback than ptrace (`true` 12→8, `echo hi` 15→11, `pwd` 16→12). That is an
interception-surface gap in the Guest contract, not a determinism defect. The
two observables are never merged into one number.

---

## 5. Table 4 — e9patch preprocessing-invariance (not a backend)

`render-scorecard.rs --csv e9patch-scorecard.csv --backends e9patch
--observable stdout --all`

```
bucket                  ptrace           e9patch
------------------------------------------------
e9patch-corpus             227        100%, 100%
------------------------------------------------
TOTAL                      227        100%, 100%
```

Both arms run under **ptrace**. The `ptrace` column is the un-rewritten golden
reference; the `e9patch` column is the same guest after `e9tool` rewriting. The
question is *"does binary rewriting change observable behavior?"* — 227/227 say
no on this corpus. This is an invariance result, not a cross-backend parity
result, and it does not transfer to Table 1.

---

## 6. Known limitations

### L1 — **Stripped, not bitwise. This is the headline limitation.**

The scorecard's `deterministic` field means *"`hermit run --strict --verify`
exited 0"*. That comparator **normalizes before comparing**. It is not the
all-observable-channels equality (exit code + stdout bytes + stderr bytes +
complete INFO-log bytes, no masking of addresses, branch counts, virtual-time
values, or durations) that the repository's own progress rubric requires before
the word *verified* may be used.

Two independent facts, both checkable from this directory:

1. **Every comparator token recorded in the published CSVs says `stripped`.**
   `scorecard.csv` is the only CSV with a `verify_compare` column at all, and
   its 618 rows are `{stripped: 346, blank: 272}` — **zero `bitwise`**. The
   other three CSVs, including `fullcorpus-scorecard.csv` which produces the
   headline 179-denominator table, **have no `verify_compare` column at all**:
   1200 + 12 + 454 rows record no comparator. So there is *no* published cell
   anywhere in this directory that carries a bitwise certification, and for
   Table 1 there is not even a field in which one could be recorded.
   Reproduce: `compat-envelope/render-current-scorecard.sh` prints this
   distribution as its last section.

2. **A stripped comparator was measured missing 3 of 5 planted defects.** A
   mutation sweep on the ptrace backend planted five distinguishable
   divergences and ran each through both comparators:

   | planted defect | stripped comparator | strict comparator |
   | --- | --- | --- |
   | `mut_stdout` (stdout bytes) | **caught** (diverged) | diverged |
   | `mut_exit` (exit status) | **caught** (diverged) | diverged |
   | `mut_detlog_only` (DETLOG only) | **MISSED** — `:: Success: deterministic. Determinism verified.` | diverged |
   | `mut_addr` (address values) | **MISSED** — reported verified | diverged |
   | `mut_path` (path strings) | **MISSED** — reported verified | diverged |
   | `clean_ctrl` (no defect) | reported verified | **diverged (false positive)** |

   Every row of that sweep also recorded `bitwise_parity=False`, including the
   rows the stripped comparator called verified — so `verified:true` with
   `bitwise_parity:false` is the *normal* state today, not an edge case.

   **Consequence:** the DETLOG / address / path defect classes are exactly the
   ones the compat scorecard exists to detect, and the current comparator is
   blind to all three. A green cell in Tables 1–4 rules out stdout and
   exit-status divergence. It does **not** rule out detlog, address-layout, or
   path divergence.

   **Nor is the strict comparator a drop-in replacement:** it diverged on the
   *clean control*, so switching to it today would turn the whole scorecard red
   for reasons unrelated to the guests. Neither comparator can currently
   certify bitwise parity. That gap is open work, tracked separately from this
   publication.

   *Provenance caveat:* this sweep is
   `experiments/strict-certification-mutation-sweep_20260806/` and was
   **untracked in the parent repository when this file was published**, so the
   table above is transcribed from its `results.csv` rather than linked to a
   committed artifact. Treat the six rows as reported-not-yet-published
   evidence, and re-derive before building a decision on them.

**Corollary for `REPORT.md`:** that file describes its full-corpus sweep as
"L2, DETLOG-bitwise self-verify". By L1 that phrasing over-states the tier —
the same `--strict --verify` leg is the stripped comparator. Read `REPORT.md`'s
L2 as stripped-L2.

### L2 — the parity observable is stdout only

`stdout-parity%` compares the SHA-256 of piped guest stdout against the ptrace
reference. It is an **upper bound** on cross-backend parity: INFO logs, stack
detlogs, and heap detlogs are not compared, and TTY behavior is outside the
scorecard entirely. A backend can score 100% stdout-parity and still diverge on
three of the four signals in the parity standard. Compounding L1, a 100% cell
means "same stdout hash, and the stripped self-verify agreed" — nothing more.

### L3 — a `0%, 0%` over an empty denominator is not a red

Buckets where the ptrace column reads `0` (Table 1: `data-handling`,
`shared-futex-c`, `util-c`) have `0/0` measured cells. The renderer prints
`0%, 0%` because 0/0 formats as 0%, but the number carries no information. Read
the `--tsv` projection, which prints the `X/Y` measured and ran counts beside
every percentage, before treating any 0% as a failure. Genuine reds are `0%, 0%`
over a **non-zero** denominator.

### L4 — Table 2 mixes seven Hermit SHAs

`scorecard.csv` accumulates across runs, and `--all` resolves each logical cell
`(bucket, test_id, test_mode, backend)` last-writer-wins. Table 2 is therefore a
composite across 2026-08-01 → 2026-08-05 and seven product SHAs (§1); no single
commit ever produced it. Table 1 does not have this problem — all six of its
sweeps ran at hermit `82a8e853` / reverie `a4f33d69`.

### L5 — measurements are from 2026-08-01, on one host

Tables 1, 3, and 4 are 2026-08-01 sweeps at hermit `82a8e853…`. Hermit `main`
has advanced since. **These numbers are a dated measurement, not a live status
of current `main`,** and the deltas are unmeasured until the collectors are
re-run. All sweeps ran on one shared 316-core devbig under contention; the
`duration_ms` / `max_rss_kb` columns are not benchmark-grade.

### L6 — the common schema is tiered; producer-specific tails remain

All four files now share the **33-column** core through `comparison_tier`.
`reverie-scorecard.csv` adds `absence_reason` (34 columns), while
`e9patch-scorecard.csv` adds `candidate_sites`, `mapped_sites`, and
`reach_state` (36 columns). Consumers must bind fields by name, not ordinal;
the producers project by target header for the same reason.

---

## 7. Column and cell schema

### CSV columns (shared contract; see `README.md` for the full system)

| # | column | class | meaning |
| ---: | --- | --- | --- |
| 1 | `run_id` | identity | producing run identifier |
| 2 | `run_utc` | identity | `@<epoch-seconds>` run timestamp |
| 3 | `hermit_sha` | identity | exact Hermit revision |
| 4 | `reverie_sha` | identity | exact Reverie revision |
| 5 | `dirty` | identity | whether the producing checkout was dirty |
| 6 | `run_mode` | mode | collector execution mode |
| 7 | `lane` | mode | portable or privileged lane |
| 8 | `bucket` | identity | corpus/category identity |
| 9 | `test_id` | identity | program/test identity |
| 10 | `test_mode` | mode | requested test operation |
| 11 | `backend` | mode | selected backend |
| 12 | `cell_state` | mode | enabled/disabled/expansion state |
| 13 | `outcome` | outcome | pass/fail/diverge/gap/skip result |
| 14 | `deterministic` | outcome | repeat-run equality verdict |
| 15 | `stdout_parity` | outcome | candidate/reference stdout equality verdict |
| 16 | `output_hash` | outcome/evidence | candidate output SHA-256 |
| 17 | `duration_ms` | outcome/evidence | measured wall duration |
| 18 | `max_rss_kb` | outcome/evidence | measured peak RSS |
| 19 | `reason` | outcome/evidence | result diagnostic |
| 20 | `verify_compare` | mode | comparison strictness/mode requested |
| 21 | `bitwise_parity` | outcome | bitwise comparison verdict |
| 22 | `compared_log_messages` | outcome/evidence | compared-message counts |
| 23 | `tier` | outcome/evidence | determinism assurance tier earned |
| 24 | `legacy_parity_unqualified` | outcome/evidence | legacy unqualified parity record |
| 25 | `ref_output_hash` | outcome/evidence | reference output SHA-256 |
| 26 | `parity_comparator` | mode | comparator used |
| 27 | `parity_tier` | outcome/evidence | parity assurance tier earned |
| 28 | `profile_flags` | mode | exact run/comparison flags |
| 29 | `population_id` | identity | selected-population identity |
| 30 | `selected_count` | outcome/evidence | selected cell count |
| 31 | `executed_count` | outcome/evidence | executed cell count |
| 32 | `evidence_count` | outcome/evidence | cells carrying parity evidence |
| 33 | `comparison_tier` | outcome/evidence | cross-backend standard earned |
| 34 | `stack_hash` | outcome/evidence | recorded stack digest |
| 35 | `heap_hash` | outcome/evidence | recorded heap digest |
| 36 | `exit_code_parity` | outcome | exit-code equality verdict |
| 37 | `detlog_parity` | outcome | DETLOG equality verdict |
| 38 | `oracle_verdict` | outcome | oracle comparison verdict |
| 39 | `stack_parity` | mode | whether `--detlog-stack` was passed (`1`/`0`; historical blank unknown) |
| 40 | `heap_parity` | mode | whether `--detlog-heap` was passed (`1`/`0`; historical blank unknown) |

### Rendered cell format

Each backend cell is **`stdout-parity%, determinism%`**, both as a fraction of
the ptrace denominator in that row:

- **`stdout-parity%`** — piped guest stdout SHA-256 matches the ptrace
  reference. Upper bound (L2).
- **`determinism%`** — the backend is self-deterministic under the stripped
  comparator (L1), whether or not it matches ptrace.

The two are **independent signals**; neither implies the other. A backend can be
100% self-deterministic and 0% parity (it reproduces its own wrong answer), or
match ptrace on a run that is not reproducible.

### Markers — never let a `0` be ambiguous

| marker | meaning |
| --- | --- |
| `X%?` | the observable was **never measured** for that bucket → UNKNOWN, not a confirmed 0 |
| `X%~` | **partial** coverage — some denominator cells measured, some not |
| `n/a` | the backend **ran zero** denominator cells here (binary absent / not enabled) → not measurable, **not** a confirmed fail |
| `0%, 0%` *(no marker, non-zero denominator)* | a **confirmed red**: it ran and it failed |
| `0%, 0%` *(no marker, zero denominator)* | no information — see L3 |

The `--tsv` / `--json` projections carry `ran_count` and
`<observable>_parity_measured_count` per cell so a machine consumer never has to
disambiguate a percentage by eye. Downstream tooling must read those, never
scrape the ASCII table.

---

## 8. Regenerating this file

```bash
# human tables, in the order they appear above, plus provenance and the
# certification-tier distribution behind Limitation L1:
compat-envelope/render-current-scorecard.sh

# machine projection of the same four tables:
compat-envelope/render-current-scorecard.sh --tsv
```

Re-running the *measurements* (as opposed to the rendering) is a different and
far more expensive operation — `make validate` →
`compat-envelope/collect-fullcorpus.sh` for Table 1, the CI lanes for Table 2,
`collect-reverie-compat.rs` for Table 3, `collect-e9patch-compat.rs` for
Table 4. See `README.md`. Refresh the CSVs first, then re-render, then update §1
and §6-L5 with the new SHAs and dates.
