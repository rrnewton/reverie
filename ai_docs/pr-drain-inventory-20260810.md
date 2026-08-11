# Open-PR drain inventory — rrnewton/hermit, rrnewton/reverie, rrnewton/agent-utils

- **Survey instant:** 2026-08-11T03:06Z (local date 2026-08-10 PDT)
- **Machine token:** `devbig014` (hostname `devbig014.atn7.facebook.com`)
- **Parent SHA at authoring:** `3b4be9d965abfe1c30cfc0ced0cc1fab99594dca`
- **Task:** `survey-inventory-open-prs-and-claims` — read-only reconnaissance. Nothing was claimed, labelled, closed, or pushed.
- **Author:** claude agent, neutral id `codex-1` (the id is neutral per *Conventions*; the model is Claude).

---

## 1. Totals

| Repo | Open PRs |
|---|---|
| rrnewton/hermit | 117 |
| rrnewton/reverie | 15 |
| rrnewton/agent-utils | 5 |
| **TOTAL** | **137** |

Drain target is **below 10**, so this is a **127-PR deficit**.

### Disposition totals (every open PR classified, no gaps)

| Disposition | Count |
|---|---|
| needs-rebase | 60 |
| needs-review | 42 |
| aggregate | 28 |
| blocked | 7 |
| **land-now** | **0** |
| **obsolete** | **0** |

---

## 2. Headline findings

### 2.1 The `obsolete` bucket is empty — there is no free close

Tested three independent ways, all negative:

1. **History containment.** GitHub three-dot `compare/main...<head>` for all 137. `ahead_by == 0` for **none**; all 137 are diverged (2 reverie report `ahead`, the rest `diverged`).
2. **Content residue vs `main` (the authority, not a patch-id guess).** Private `--bare --shared` clones of all three repos, every `refs/pull/N/head` fetched, and for each PR the **two-dot** diff `origin/main..head` **restricted to the files that PR touches**. A PR whose content already landed has zero residue. Result: **137/137 LIVE, 0 SUBSUMED.** Closest to empty: reverie #399 (1 file, +7/-25) and reverie #429 (1 file, +10/-5) — both still live.
3. **Pairwise containment between open PRs.** 218 subset-candidate pairs (same repo, one PR's file set a subset of the other's) tested with the same residue method. **Zero** PRs fully contained in another open PR.

Reference `main` heads used: hermit `a4c8aedaf`, reverie `34946092`, agent-utils `b495100`.

**Consequence:** drain must come from **landing and aggregation**, not closure. Any agent closing a PR as "subsumed" today destroys unique work. This independently corroborates the `hermit-drain-landed@devbig030` audits on #1730, #1864, #1918, #1933, all of which concluded *NOT landed*.

### 2.2 Zero PRs hold exact-head landing authority

Only three hermit PRs carry `locally-validated`: **#2108, #2008, #1666**. Dereferenced against the LEDGER at their **current** heads with `ci-hub validate-status --sha`:

| PR | head | LEDGER verdict |
|---|---|---|
| #2108 | `d3e90b9826891829b7b64ef2b803ba09f3553c67` | NOT-VALIDATED — no clean full-coverage PASS, 0 records |
| #2008 | `7e1bfee0f2786bc2f38625f2847b7134878f7a98` | NOT-VALIDATED — no clean full-coverage PASS, 0 records |
| #1666 | `26d47f007b54d2a60d31a8d22f2ce30245c0e95f` | NOT-VALIDATED — no clean full-coverage PASS, 0 records |

The label is a **cache that has drifted off its source** — a textbook Proxy Binding failure. `land-now` is therefore empty: **nothing lands without first producing a fresh exact-head receipt.** The drain rate is bounded by **validate throughput**, not by reviewers or agent count.

### 2.3 Merge state had to be forced

GitHub reported `mergeable: UNKNOWN` for **97 of 117** hermit PRs — it computes lazily and nothing had asked. Each PR was re-polled until GitHub resolved it. Anyone reading `gh pr list --json mergeable` once will see `UNKNOWN` and must not read that as "fine".

| Repo | dirty (conflict) | unstable | behind | clean | blocked |
|---|---|---|---|---|---|
| hermit | 53 | 56 | 0 | 8 | 0 |
| reverie | 2 | 0 | 11 | 1 | 1 |
| agent-utils | 5 | 0 | 0 | 0 | 0 |

**58 of 137 carry real conflicts.** Note `unstable` is *not* disqualifying for Hermit: GH Actions is advisory P2 and the LEDGER is the authority.

### 2.4 Draft and age

- **Draft:** hermit 99 draft / 18 ready; reverie 12 draft / 3 ready; agent-utils 5 draft.
- **Age:** nothing exceeds 7 days. hermit 76 in the 3–7d band, 13 at 1–3d, 28 under a day. The pool is being **actively regenerated**, not merely rotting — draining it requires stopping new work, which the owner directive already does.

### 2.5 Revert risk — stale branches that would delete landed work

`residue -N` = lines present on `main` but **absent from the PR head, inside that PR's own files`. Merging such a branch without a rebase silently reverts landed work.

| Repo | PR | would delete from main | adds | files |
|---|---|---|---|---|
| hermit | #1665 | 5720 | 2602 | 12 |
| hermit | #1927 | 5386 | 929 | 4 |
| hermit | #1735 | 5359 | 818 | 2 |
| agent-utils | #23 | 4374 | 3493 | 15 |
| agent-utils | #22 | 3096 | 1308 | 8 |
| hermit | #1757 | 2971 | 2335 | 62 |
| hermit | #1863 | 2809 | 538 | 4 |
| hermit | #1975 | 2779 | 917 | 14 |
| hermit | #2000 | 2304 | 2176 | 16 |
| hermit | #1727 | 2130 | 3628 | 43 |

---

## 3. Team-claim labels and the claim mechanism

### 3.1 Labels already in use on open PRs — do not collide

| Label | hermit | reverie | agent-utils |
|---|---|---|---|
| `codex-coord` / `codex-coord-030` | 19 / 19 | 3 / 3 | 0 |
| `team:claude-coord-fable` | 13 | 1 | 0 |
| `orc-coord` / `orc-coord-014` | 7 / 2 | 1 / 1 | 0 |
| `claude-coord` / `claude-coord-176` | 6 / 6 | 1 / 1 | 0 |

The numeric suffix is the **devbig box number**: `-030` = devbig030, `-176` = devbig176, `-014` = **this box**.

Additional team slugs appearing **only as comment prefixes, not yet as labels**: `hermit-drain-corpus@devbig030`, `hermit-drain-budget@devbig030`, `hermit-drain-landed@devbig030`, `codex-slots@devbig014`, `codex-hostload@devbig014`, `codex-mirror@devbig014`.

**Non-colliding slug recommendation for this box:** `team:hermit-drain-014`. Avoid `orc-coord-014`, `codex-slots`, `codex-hostload`, `codex-mirror` — all already in use on devbig014.

### 3.2 The claim mechanism (derived empirically — there is no tooling for it)

`ci-hub` has **no claim subcommand** and no script in the repo implements claim TTL. A claim is a **PR comment** with:

- a bracketed `[<team-slug>, devbigNNN]` prefix,
- a bold `**CLAIM**` / `**CLAIMING**` marker,
- an explicit **TTL**, either an absolute lapse instant or *N hours from this comment*,
- and, verbatim, the statement that on lapse the PR is **fair game for any other team**.

Release is an explicit `**CLAIM RELEASED**` comment. Because it is convention-only, nothing enforces it and nothing expires it automatically.

### 3.3 Claim state at 2026-08-11T03:06Z

**ACTIVE — do not touch (6):**

| PR | Team | Expires |
|---|---|---|
| hermit #1719 | hermit-drain-corpus@devbig030 | 2026-08-11T04:00Z |
| hermit #2066 | hermit-drain-corpus@devbig030 | 2026-08-11T04:00Z |
| hermit #1865 | claude-coord-fable@devbig030 | 2026-08-11T06:00Z |
| hermit #1892 | claude-coord-fable@devbig030 | 2026-08-11T06:00Z |
| hermit #2004 | claude-coord-fable@devbig030 | 2026-08-11T06:00Z |
| hermit #2057 | claude-coord-176@devbig176 | 2026-08-11T05:44Z |

**EXPIRED — fair game now (9):**

| PR | Team | Expired |
|---|---|---|
| hermit #1774 | hermit-drain-corpus@devbig030 | 21:47Z |
| hermit #2084 | hermit-drain-corpus@devbig030 | 22:40Z |
| hermit #2053 | claude-coord-fable@devbig030 | ~21:28Z |
| hermit #2062 | claude-coord-fable@devbig030 | ~21:26Z |
| hermit #2069 | claude-coord-fable@devbig030 | ~21:26Z |
| hermit #2078 | claude-coord-fable@devbig030 | ~21:26Z |
| hermit #2083 | claude-coord-fable@devbig030 | ~21:26Z |
| hermit #2088 | claude-coord-fable@devbig030 | ~21:26Z |
| reverie #430 | claude-coord-fable@devbig030 | ~22:50Z |

reverie #430 then took **two independent CHANGES-REQUESTED** at head `9b17ec02`, was returned to **draft** by its own shepherd, and was declared **ORPHANED and adoptable** at 02:27Z (authoring agent `hermit-fix-liteinst` verified dead). Adoptable, but **not** land-ready.

**Explicitly RELEASED (9):** hermit #1691 #1717 #1721 #1730 #1734 #1737 #1756 #1899 #1978. Several were released *with a stated defect* — #1717's fixture found a real Hermit bug, #1721 has two failing promoted cells. **Released does not mean healthy.**

**Stale pre-convention claims:** `codex-coord-030` posted *"Claiming for repair"* on hermit #1673 #1677 #1684 #1691 #1692 #1695 on 2026-08-07 with **no TTL**. Four days old, predating the TTL convention. Flagged as lapsed-by-age rather than assumed.

---

## 4. Aggregation clusters

### Cluster 1 — test-corpus registry (15 PRs) — largest lever, ALREADY OWNED

#1691 #1717 #1719 #1721 #1730 #1734 #1737 #1756 #1774 #1899 #1978 #2066 #2084 #2108 #2114

These touch **only** `tests/**` and `ci/expected-e2e-plan.json`, and all append to the same registry files (`tests/e2e/manifests/inventory/test-files.json`, `ci/expected-e2e-plan.json`, `tests/e2e/manifests/*.toml`), so landing any one conflicts the rest. `hermit-drain-corpus@devbig030` has already landed batches **#2098** and **#2100** and holds **#2108** open as batch 4. **Do not start a parallel corpus batch.**

Verified: #2108 does **not** contain #1719 (16 residue files, +298) or #2066 (8 files, +831). Batch 4 is partial.

### Cluster 2 — getrusage (5 PRs) — second largest lever

Children #1680 #1682 #1686 #1687, declared container **#2064** (OPEN, draft, unmerged).

**Correction to the record:** #2064 does **not** textually contain them — residue is #1680 +1, #1682 +75, #1686 +94, #1687 +74. Inspecting #1686 shows those 94 lines are a **rewrite of the same function** (`timeval_from_logical` in `detcore/src/syscalls/sysinfo.rs`), not orphaned work, so #2064 *is* a genuine semantic coalescer. The four were **correctly reopened** on 2026-08-10 because containment is not discharged until the container merges. **Landing #2064 discharges 5 PRs at once.**

### Cluster 3 — `scripts/validate.rs` producer (15 PRs) — MUST SERIALIZE

#1735 #1927 #1999 #2000 #2007 #2012 #2050 #2052 #2056 #2067 #2070 #2076 #2079 #2089 #2105

Every PR here modifies the **registered validate producer**, so it is intentionally **local-non-authoritative** and needs the registered exact-head **GH Actions** result rather than a LEDGER receipt (*Sole Hermit exception*). These cannot self-vouch and cannot be batched.

### Clusters 4 and 5 — pure serialization hazards, not semantic duplicates

- `ci/dag/portable.json` (18): #1727 #1742 #1757 #1778 #1851 #1864 #1865 #1901 #1908 #1926 #1959 #2000 #2008 #2053 #2079 #2088 #2093 #2105
- `ci/test_harness.sh` (25): #1665 #1727 #1729 #1757 #1779 #1863 #1892 #1926 #1959 #1975 #2000 #2004 #2008 #2041 #2048 #2050 #2053 #2069 #2074 #2077 #2079 #2088 #2093 #2097 #2105

### Mechanism-label twins — all checked for containment in **both** directions; none subsumes the other

`backend-parity-c-registration` {#1665 #1691 #1730 #1757 #2108} · `fixture-oracle-distinguishability` {#1719 #1734 #1737 #1756} · `getrusage-logical-cpu-accounting` {#1680 #1682 #1686 #1687} · `reverie-pin-gate` {#1863 #1975 #2041} · `validate-admission` {#1927 #2007 #2089} · `compat-scorecard` {#2067 #2084} · `dbi-build-budget` {#1863 #1975} · `full-validate-gate-plan` {#2000 #2008} · `merge-gate` {#1666 #2008} · `rust-validate-production` {#1999 #2070} · `strict-compat-budget-nesting` {#1901 #2000} · `strict-compat-denominator` {#1717 #1727} · `validate-ledger-authority` {#2012 #2052} · `validate-shell-entrypoint-retired` {#1749 #1926} · reverie `ptrace-sigtrap-provenance` {#388 #395}

### agent-utils — all 5 collide on one subsystem

All five are `dirty` and all edit `safe_ci_dag_runner`: `py/safe_ci_dag_runner/scheduler.py` (#19 #22 #23 #25), `model.py` (#22 #23 #24 #25), `rs/safe-ci-dag-runner/src/{scheduler,model}.rs` (#22 #23 #25). #22/#23/#24 are `rescue/*` branches; their content is **not** on main (residue LIVE), so the rescue purpose is **not** discharged — but #22 and #23 would delete 3096 and 4374 lines of landed work if merged unrebased. Publication here is serialized through `scripts/main_write.py publish`; a bare push to `main` is refused.

---

## 5. Prior triage that already exists — do not redo it

- `claude-coord-176` posted **97 per-PR `Disposition:` comments** across hermit and reverie at ~2026-08-10T05:30Z.
- `hermit-drain-landed@devbig030` posted already-landed audits on #1730 #1864 #1918 #1933 — all *NOT landed*, agreeing with §2.1.
- `hermit-drain-corpus@devbig030` landed batches #2098 and #2100 and holds #2108.

---

## 6. Recommended drain order (advisory — this task authorizes nothing)

1. **#2064** — discharges 5 PRs (itself + #1680 #1682 #1686 #1687). Needs a fresh exact-head receipt.
2. **Let `hermit-drain-corpus@devbig030` finish Cluster 1** — up to 15 PRs, already in flight. Coordinate; do not duplicate.
3. **Serialize Cluster 3** (validate.rs, 15 PRs) through registered exact-head GH Actions, one at a time.
4. **Rebase-first sweep** of the 58 conflicted PRs, highest revert-risk first (§2.5) — those are the ones most likely to silently revert landed work.
5. **Adopt the 9 expired-claim PRs** — explicitly fair game by their own claim text.

**Hard blocker on all of it:** §2.2. No PR has exact-head validate authority, and per prior measurement `main`'s median inter-commit gap (656 s) is below the validate wall (520–900 s), so roughly half of all validate runs cannot survive a moving `main`. Freezing `main` is a precondition for a sustained drain, not an optimisation.

---

## 7. Per-PR table — all 137 open PRs

Legend — **residue f/+/-**: files / lines in the PR head absent from `main` / lines on `main` absent from the PR head, restricted to the PR's own files. **merge**: GitHub `mergeable_state` after forced computation.

### rrnewton/agent-utils

| PR | head12 | age(d) | draft | merge | claim | residue f/+/- | cluster | disposition | note | title |
|---|---|---|---|---|---|---|---|---|---|---|
| #19 | `13b268f0a3ff` | 4.2 | draft | dirty | none | 30/+4172/-1842 | - | **needs-rebase** | conflicts; rebase would drop 1842 stale lines vs main | Correct v0.3.0: drop refuted reap-on-teardown; keep pids axis + namesp |
| #22 | `7dc20de8f8b8` | 1.1 | draft | dirty | none | 8/+1308/-3096 | - | **needs-rebase** | conflicts; rebase would drop 3096 stale lines vs main | [rescue] preserve per-platform CPU-budget multiplier |
| #23 | `3c641506e609` | 1.1 | draft | dirty | none | 15/+3493/-4374 | - | **needs-rebase** | conflicts; rebase would drop 4374 stale lines vs main | [rescue] preserve small default cgroup cap |
| #24 | `33a6eb9047a3` | 1.0 | draft | dirty | none | 1/+27/-57 | - | **needs-rebase** | conflicts; rebase would drop 57 stale lines vs main | [rescue] preserve runner known-failure allowlist work |
| #25 | `27a34ca4c19b` | 0.7 | draft | dirty | none | 17/+954/-868 | - | **needs-rebase** | conflicts; rebase would drop 868 stale lines vs main | Fail closed on undeclared DAG write domains |

### rrnewton/hermit

| PR | head12 | age(d) | draft | merge | claim | residue f/+/- | cluster | disposition | note | title |
|---|---|---|---|---|---|---|---|---|---|---|
| #1665 | `dfbafe36eb4d` | 4.6 | ready | dirty | none | 12/+2602/-5720 | backend-parity-c-registration{#1665,#1691,#1730,#1757,#2108} | **needs-rebase** | conflicts; rebase would drop 5720 stale lines vs main | ci: shard backend parity manifests |
| #1666 | `26d47f007b54` | 4.6 | draft | unstable | none | 2/+161/-71 | merge-gate{#1666,#2008} | **aggregate** | serialize: merge-gate{#1666,#2008} | ci(merge-gate): stop NO_RESULT retry amplification |
| #1673 | `a50f5eb1c917` | 4.6 | draft | dirty | none | 6/+834/-259 | - | **needs-rebase** | conflicts; rebase would drop 259 stale lines vs main | Add hermit oci to discover and run images from the local store |
| #1677 | `4a744f168e93` | 4.6 | draft | unstable | none | 3/+89/-66 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Make fail-closed unsupported-syscall handling the default (#71) |
| #1680 | `9cfd59b5b454` | 4.6 | draft | unstable | none | 2/+113/-20 | getrusage/#2064; getrusage-logical-cpu-accounting{#1680,#1682,#1686,#1687} | **aggregate** | survivor #2064 (open, unmerged); NOT textually contained (+113 lines) but semantically superseded | getrusage: derive CPU times from logical time instead of reporting zer |
| #1682 | `4a3536f0ccce` | 4.6 | draft | unstable | none | 2/+90/-20 | getrusage/#2064; getrusage-logical-cpu-accounting{#1680,#1682,#1686,#1687} | **aggregate** | survivor #2064 (open, unmerged); NOT textually contained (+90 lines) but semantically superseded | detcore: derive getrusage CPU accounting from virtual time instead of  |
| #1684 | `52581d68a40b` | 4.6 | draft | clean | none | 1/+28/-115 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | detcore: fail closed instead of panicking when a thread exits before d |
| #1686 | `27757cd23b90` | 4.6 | draft | unstable | none | 1/+113/-8 | getrusage/#2064; getrusage-logical-cpu-accounting{#1680,#1682,#1686,#1687} | **aggregate** | survivor #2064 (open, unmerged); NOT textually contained (+113 lines) but semantically superseded | Derive getrusage CPU times from logical accounting |
| #1687 | `9dc5f889a1d9` | 4.6 | draft | unstable | none | 1/+91/-4 | getrusage/#2064; getrusage-logical-cpu-accounting{#1680,#1682,#1686,#1687} | **aggregate** | survivor #2064 (open, unmerged); NOT textually contained (+91 lines) but semantically superseded | [impl agent, opus-5] detcore: derive getrusage CPU times from logical  |
| #1689 | `ece949c5ba8b` | 4.5 | draft | dirty | none | 3/+2324/-176 | - | **needs-rebase** | conflicts; rebase would drop 176 stale lines vs main | Honour --log-file on the DBI backend |
| #1691 | `c17b61b89f2b` | 4.5 | draft | unstable | none | 2/+455/-878 | test-corpus-registry; backend-parity-c-registration{#1665,#1691,#1730,#1757,#2108} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | tests: multi-writer pipe ordering parity fixture (M11 gap) |
| #1692 | `c01d24740569` | 4.5 | draft | unstable | none | 1/+75/-65 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | detcore: compare the guest INFO stream, not the tracer and CLI streams |
| #1695 | `9001bb3c8e2d` | 4.5 | draft | dirty | none | 3/+158/-114 | - | **needs-rebase** | conflicts; rebase would drop 114 stale lines vs main | Emit the heap DETLOG record from the observed program break |
| #1697 | `d93d512826c5` | 4.5 | draft | unstable | none | 1/+214/-158 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | hermit: preserve guest wait status through the DBI backend |
| #1704 | `c25549b5e034` | 4.5 | draft | dirty | none | 3/+350/-1269 | - | **needs-rebase** | conflicts; rebase would drop 1269 stale lines vs main | detcore: contract fixtures for fork/exec/wait ordering and the vfork h |
| #1708 | `dd9d8569c5b6` | 4.5 | draft | dirty | none | 3/+431/-1269 | - | **needs-rebase** | conflicts; rebase would drop 1269 stale lines vs main | detcore: contract fixtures for signal mask / disposition inheritance a |
| #1709 | `bc461a2608e2` | 4.5 | draft | clean | none | 1/+624/-0 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | scripts: cross-backend DETLOG diff reporting the first divergence |
| #1713 | `f706d3dc3d6f` | 4.5 | draft | unstable | none | 2/+627/-0 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Pin the whole clock family to one continuous virtual clock |
| #1717 | `878a7ead77c5` | 4.5 | draft | dirty | none | 4/+544/-946 | test-corpus-registry; strict-compat-denominator{#1717,#1727} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | tests: pin that syscall failure is as deterministic as success |
| #1719 | `8f4a5cfb14c3` | 4.5 | draft | dirty | ACTIVE hermit-drain-corpus@devbig030 exp 04:00Z | 30/+1265/-768 | test-corpus-registry; fixture-oracle-distinguishability{#1719,#1734,#1737,#1756} | **blocked** | ACTIVE foreign claim hermit-drain-corpus@devbig030 exp 04:00Z | backend-parity: emit the observed VALUE, not just a check sum |
| #1721 | `9bece24c394f` | 4.5 | draft | dirty | none | 7/+604/-938 | test-corpus-registry | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | Corpus: add three real-program entries on ptrace (auxv, /proc walk, pa |
| #1722 | `151d4c032e42` | 4.5 | draft | dirty | none | 1/+132/-143 | - | **needs-rebase** | conflicts; rebase would drop 143 stale lines vs main | hermit-cli: mark backends --help lists but this binary cannot run |
| #1725 | `09cb6c982903` | 4.5 | draft | clean | none | 1/+129/-140 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | Make SaBRe unobserved dynamic-loading phase loud |
| #1727 | `49321e67e635` | 4.5 | draft | dirty | none | 43/+3628/-2130 | strict-compat-denominator{#1717,#1727} | **needs-rebase** | conflicts; rebase would drop 2130 stale lines vs main | [strict-compat] Restore full-corpus performance fixtures |
| #1729 | `22475a53d6d9` | 4.5 | draft | unstable | none | 4/+494/-1444 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Break the ambiguous zero in SaBRe path evidence with a routed signal |
| #1730 | `addcfab237ea` | 4.5 | draft | unstable | none | 6/+1016/-752 | test-corpus-registry; backend-parity-c-registration{#1665,#1691,#1730,#1757,#2108} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | tests: add inline-syscall guests so a main-ELF patcher has sites to pa |
| #1731 | `63ec41017562` | 4.5 | draft | unstable | none | 1/+66/-8 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Put the not-measured state in the e9patch banner fields, not the prose |
| #1734 | `1acefc75f375` | 4.5 | draft | dirty | none | 3/+413/-1076 | test-corpus-registry; fixture-oracle-distinguishability{#1719,#1734,#1737,#1756} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | getcpu_identity: emit observed values, bracket it, and turn it on |
| #1735 | `3678a3b7b7f5` | 4.5 | draft | dirty | none | 2/+818/-5359 | validate.rs-producer | **needs-rebase** | conflicts; rebase would drop 5359 stale lines vs main | Refuse a bare validate inside dev-hermit and name the command to run |
| #1736 | `97a5d6be758e` | 4.5 | draft | unstable | none | 1/+132/-0 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | ci: give the Linux boot its own lane instead of riding the hot-path ga |
| #1737 | `56ca566fc2b3` | 4.4 | draft | dirty | none | 3/+392/-1076 | test-corpus-registry; fixture-oracle-distinguishability{#1719,#1734,#1737,#1756} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | tests: run the getpriority identity contract in CI and report observed |
| #1739 | `d1d30349d7fc` | 4.4 | draft | unstable | none | 2/+95/-2 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Make demo 5 find its pinned kernel without the network |
| #1742 | `162fbffcdb31` | 4.4 | draft | dirty | none | 11/+1519/-777 | - | **needs-rebase** | conflicts; rebase would drop 777 stale lines vs main | Make flock(2) actually exclude, instead of a no-op success |
| #1744 | `780f4cd40d9f` | 4.4 | draft | dirty | none | 14/+101/-27 | - | **needs-rebase** | conflicts; rebase would drop 27 stale lines vs main | Deny unused_must_use across the workspace |
| #1747 | `01c6f6aec678` | 4.4 | draft | unstable | none | 5/+249/-286 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Name a SaBRe plugin/coordinator mismatch instead of failing as a codec |
| #1749 | `d756169e2671` | 4.4 | draft | dirty | none | 13/+7714/-286 | validate-shell-entrypoint-retired{#1749,#1926} | **needs-rebase** | conflicts; rebase would drop 286 stale lines vs main | Add versioned strict verification receipts |
| #1753 | `95c8c9026f8b` | 4.3 | draft | clean | none | 5/+531/-0 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | compat-scorecard: version-controlled, idempotent, monotonicity-linted |
| #1756 | `059df5cd2c35` | 4.3 | draft | dirty | none | 3/+396/-1076 | test-corpus-registry; fixture-oracle-distinguishability{#1719,#1734,#1737,#1756} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | backend-parity: emit the observed affinity mask and run the contract i |
| #1757 | `f792cf1c7155` | 4.3 | draft | dirty | none | 62/+2335/-2971 | backend-parity-c-registration{#1665,#1691,#1730,#1757,#2108} | **needs-rebase** | conflicts; rebase would drop 2971 stale lines vs main | Make backend-parity-c actually run: 1 -> 72 enabled cells, all 85 fixt |
| #1759 | `1e9e80f92e54` | 4.2 | draft | clean | none | 20/+3395/-1405 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | Fence RDRAND determinization off LiteInst, SaBRe and e9patch |
| #1760 | `b221e3e0e217` | 4.2 | draft | clean | none | 1/+134/-140 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | Make the SaBRe ptrace fallback loud instead of silent |
| #1766 | `0fdcfa9eba25` | 4.2 | draft | dirty | none | 2/+1829/-1057 | - | **needs-rebase** | conflicts; rebase would drop 1057 stale lines vs main | Qualify LiteInst socket timestamp determinism (REFILE of abandoned bra |
| #1767 | `7c7838fb3516` | 4.2 | draft | dirty | none | 46/+7278/-1433 | - | **needs-rebase** | conflicts; rebase would drop 1433 stale lines vs main | Document locked host installation (REFILE of abandoned branch) |
| #1772 | `6a6f41c9777b` | 4.2 | draft | dirty | none | 6/+3353/-1325 | - | **needs-rebase** | conflicts; rebase would drop 1325 stale lines vs main | hermit-cli: gate DBI-only run helpers behind the dbi feature (REFILE o |
| #1774 | `5a7ce2e0a1eb` | 4.2 | draft | dirty | EXPIRED hermit-drain-corpus@devbig030 exp 21:47Z | 1/+124/-128 | test-corpus-registry | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | manifests: dated power/weight audit of the three costliest test cells |
| #1778 | `e58457a303c6` | 4.2 | draft | dirty | none | 9/+2357/-906 | - | **needs-rebase** | conflicts; rebase would drop 906 stale lines vs main | parity: compare stderr and the INFO log, with a recorded strictness ti |
| #1779 | `63ec12d3b9b3` | 4.2 | draft | dirty | none | 6/+1015/-1875 | - | **needs-rebase** | conflicts; rebase would drop 1875 stale lines vs main | Make the chaos diversity oracle sensitive to partial narrowing, not ju |
| #1813 | `f140ed26a6c1` | 4.0 | draft | dirty | none | 2/+430/-189 | - | **needs-rebase** | conflicts; rebase would drop 189 stale lines vs main | Add exec-boundary virtual-clock continuity regression coverage |
| #1828 | `2c16d95d76ee` | 4.0 | draft | unstable | none | 1/+181/-141 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Fail closed when a SaBRe run never reaches the Detcore coordinator |
| #1847 | `077833ad6595` | 4.0 | draft | unstable | none | 7/+737/-240 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Determinize proc maps inodes for LiteInst memory hashes |
| #1851 | `ef1392494c11` | 4.0 | ready | dirty | none | 7/+1031/-756 | - | **needs-rebase** | conflicts; rebase would drop 756 stale lines vs main | detcore: determinize the chown family to complete the virtual-root ide |
| #1853 | `05331f698272` | 4.0 | draft | unstable | none | 1/+61/-11 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Name `run` in the SaBRe scope message, which the predicate already adm |
| #1863 | `072ad486795f` | 3.9 | draft | dirty | none | 4/+538/-2809 | reverie-pin-gate{#1863,#1975,#2041}; dbi-build-budget{#1863,#1975} | **needs-rebase** | conflicts; rebase would drop 2809 stale lines vs main | check-reverie-pin: refuse a bump whose tree does not compile |
| #1864 | `b2e44120e984` | 3.9 | ready | dirty | none | 6/+692/-256 | - | **needs-rebase** | conflicts; rebase would drop 256 stale lines vs main | detcore: route epoll_pwait through the epoll_wait logic instead of blo |
| #1865 | `0707319d958e` | 3.9 | draft | unstable | ACTIVE claude-coord-fable@devbig030 exp 06:00Z | 4/+414/-211 | - | **blocked** | ACTIVE foreign claim claude-coord-fable@devbig030 exp 06:00Z | scripts: fail when a determinism-relaxation flag is unclassified |
| #1875 | `8eb032382922` | 3.8 | draft | unstable | none | 3/+518/-210 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Record the hash domain with each memory digest so parity compares like |
| #1878 | `13344dcd10ff` | 3.8 | draft | unstable | none | 1/+83/-100 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | ci: keep prebuilt artifacts when a re-run may still need them |
| #1884 | `5b09632b5f56` | 3.8 | draft | unstable | none | 1/+144/-0 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | ci: exercise and classify cold Demo 8 calibration |
| #1892 | `195614a4c208` | 3.8 | draft | dirty | ACTIVE claude-coord-fable@devbig030 exp 06:00Z | 1/+243/-1248 | - | **blocked** | ACTIVE foreign claim claude-coord-fable@devbig030 exp 06:00Z | ci: record canonical E2E verification verdicts |
| #1899 | `e70eeb120644` | 3.7 | draft | unstable | none | 1/+253/-0 | test-corpus-registry | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | tests/qemu: parent<->child pipe IPC fixture for the guest kernel (M11) |
| #1901 | `c15610b0d471` | 3.7 | draft | dirty | none | 2/+156/-147 | strict-compat-budget-nesting{#1901,#2000} | **needs-rebase** | conflicts; rebase would drop 147 stale lines vs main | Derive per-node DAG CPU timeouts |
| #1902 | `9ace4fe6566f` | 3.6 | draft | dirty | none | 3/+2499/-123 | - | **needs-rebase** | conflicts; rebase would drop 123 stale lines vs main | Close the Sysno::iter() off-by-one in the passthru-opt sweep and pin t |
| #1904 | `bb427c8d2944` | 3.6 | draft | unstable | none | 4/+269/-148 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Virtualize PR_SET/GET_TIMERSLACK in per-thread Detcore state |
| #1906 | `b24221876fff` | 3.6 | draft | unstable | none | 6/+224/-206 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Make ioprio updates observable and target-aware |
| #1907 | `49a40f446117` | 3.6 | draft | unstable | none | 1/+15/-43 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | detcore: log nondeterministic realtime elapsed at trace, not debug |
| #1908 | `08cd63d57bd2` | 3.6 | draft | dirty | none | 2/+183/-138 | - | **needs-rebase** | conflicts; rebase would drop 138 stale lines vs main | tests: make the #964 thread-self regression able to fail, and put it i |
| #1918 | `5f39f398bc2b` | 3.4 | draft | clean | none | 7/+3191/-1684 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | detcore-dbi: determinize ownership syscalls through the virtual-root m |
| #1919 | `ff11f9864695` | 3.4 | draft | dirty | none | 11/+3177/-713 | - | **needs-rebase** | conflicts; rebase would drop 713 stale lines vs main | detcore: canonicalize backend DETLOG and add environment identity |
| #1926 | `9d1f817f3d37` | 3.4 | draft | dirty | none | 9/+5806/-1554 | validate-shell-entrypoint-retired{#1749,#1926} | **needs-rebase** | conflicts; rebase would drop 1554 stale lines vs main | Enforce current agent-utils pin across validation |
| #1927 | `88e238f95ed7` | 3.4 | draft | dirty | none | 4/+929/-5386 | validate.rs-producer; validate-admission{#1927,#2007,#2089} | **needs-rebase** | conflicts; rebase would drop 5386 stale lines vs main | Add typed non-qualifying historical validation |
| #1932 | `951fac5627d8` | 3.4 | draft | dirty | none | 5/+603/-159 | - | **needs-rebase** | conflicts; rebase would drop 159 stale lines vs main | Determinize anonymous mmap placement across backends |
| #1933 | `54167bf9478a` | 3.3 | draft | dirty | none | 8/+358/-974 | - | **needs-rebase** | conflicts; rebase would drop 974 stale lines vs main | Determinize process-group identity |
| #1957 | `b83840f850d1` | 3.3 | draft | unstable | none | 4/+228/-326 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Refuse direct vvar reads across execution backends |
| #1959 | `6797cb7fb9b8` | 3.3 | draft | dirty | none | 4/+263/-2056 | - | **needs-rebase** | conflicts; rebase would drop 2056 stale lines vs main | Wire stop fixture into portable validation DAG |
| #1960 | `44327af3031b` | 3.3 | draft | dirty | none | 7/+330/-563 | - | **needs-rebase** | conflicts; rebase would drop 563 stale lines vs main | Polish release-facing build and CLI guidance |
| #1963 | `238e160cd231` | 3.1 | draft | unstable | none | 3/+90/-39 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Enumerate repo content through git, not bare filesystem walks |
| #1975 | `9d50e89908ee` | 2.8 | draft | dirty | none | 14/+917/-2779 | reverie-pin-gate{#1863,#1975,#2041}; dbi-build-budget{#1863,#1975} | **needs-rebase** | conflicts; rebase would drop 2779 stale lines vs main | reverie pin: repair every live binding, or refuse |
| #1978 | `2fcbfdd0d85a` | 2.5 | draft | dirty | none | 5/+814/-316 | test-corpus-registry | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | backend-parity: isolate scorecard observations |
| #1991 | `6a71bd3d52be` | 2.2 | ready | unstable | none | 3/+256/-0 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Refuse merge drivers that no reviewer can see |
| #1999 | `ae75552120c2` | 2.0 | draft | unstable | none | 2/+353/-199 | validate.rs-producer; rust-validate-production{#1999,#2070} | **aggregate** | serialize: validate.rs-producer | validate: settle an environmental classification with the retry it alr |
| #2000 | `47705cd6b6fb` | 2.0 | ready | dirty | none | 16/+2176/-2304 | validate.rs-producer; full-validate-gate-plan{#2000,#2008}; strict-compat-budget-nesting{#1901,#2000} | **needs-rebase** | conflicts; rebase would drop 2304 stale lines vs main | ci: resize the metadata validation budget |
| #2004 | `703968fdf88b` | 1.9 | draft | dirty | ACTIVE claude-coord-fable@devbig030 exp 06:00Z | 3/+236/-1192 | - | **blocked** | ACTIVE foreign claim claude-coord-fable@devbig030 exp 06:00Z | ci: refuse a hermit-cli test binary CI does not know exists |
| #2007 | `6a626da4b96b` | 1.4 | ready | dirty | none | 3/+1939/-1141 | validate.rs-producer; validate-admission{#1927,#2007,#2089} | **needs-rebase** | conflicts; rebase would drop 1141 stale lines vs main | ci: port peer-snapshot authority into Rust validation |
| #2008 | `7e1bfee0f278` | 1.4 | ready | unstable | none | 2/+148/-12 | merge-gate{#1666,#2008}; full-validate-gate-plan{#2000,#2008} | **aggregate** | serialize: merge-gate{#1666,#2008} | ci: resize the metadata validation budget |
| #2012 | `d01369e8e992` | 1.2 | ready | dirty | none | 4/+414/-1132 | validate.rs-producer; validate-ledger-authority{#2012,#2052} | **needs-rebase** | conflicts; rebase would drop 1132 stale lines vs main | Use one canonical validate ledger |
| #2041 | `e23ed5186abf` | 1.0 | draft | unstable | none | 10/+225/-1385 | reverie-pin-gate{#1863,#1975,#2041} | **aggregate** | serialize: reverie-pin-gate{#1863,#1975,#2041} | docs: align Reverie pin policy surfaces |
| #2042 | `a158ef7cf952` | 1.0 | ready | unstable | none | 2/+112/-20 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | compat: add the guest-program classes the corpus never had (parallel,  |
| #2048 | `915ea9425e62` | 1.0 | draft | unstable | none | 4/+4261/-976 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | compat: one backend-agnostic corpus x Backend::ALL with four-state cel |
| #2050 | `8374db4423c0` | 1.0 | draft | dirty | none | 8/+1163/-1805 | validate.rs-producer | **needs-rebase** | conflicts; rebase would drop 1805 stale lines vs main | Gate validation on per-node wall-time regressions |
| #2052 | `4d2ccb9ac1fd` | 1.0 | ready | unstable | none | 3/+442/-841 | validate.rs-producer; validate-ledger-authority{#2012,#2052} | **aggregate** | serialize: validate.rs-producer | Account for every planned validate node |
| #2053 | `5165ca03d67d` | 1.0 | draft | unstable | EXPIRED claude-coord-fable@devbig030 exp 21:28Z | 2/+167/-1067 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Parallelize Hermit guest validation safely |
| #2056 | `66a0cc3905b3` | 1.0 | draft | unstable | none | 1/+421/-833 | validate.rs-producer | **aggregate** | serialize: validate.rs-producer | validate: make the known fail-closed table speak on the lane that gate |
| #2057 | `2771f28aad39` | 1.0 | ready | unstable | ACTIVE claude-coord-176@devbig176 exp 05:44Z | 1/+65/-10 | - | **blocked** | ACTIVE foreign claim claude-coord-176@devbig176 exp 05:44Z | make: split submodule verification from initialisation so the gate can |
| #2059 | `fbe3e139b047` | 1.0 | draft | unstable | none | 2/+231/-14 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Fix intermittent compat.yes determinism under pipe pressure |
| #2062 | `26ce4c39c213` | 0.9 | draft | unstable | EXPIRED claude-coord-fable@devbig030 exp 21:26Z | 3/+504/-0 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Add code-elegance skill for correctness-driven cleanup sweeps |
| #2064 | `ff1f6c6f6587` | 0.9 | draft | unstable | none | 3/+142/-20 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Coalesce getrusage logical CPU accounting drafts |
| #2066 | `281dd3472ead` | 0.9 | draft | dirty | ACTIVE hermit-drain-corpus@devbig030 exp 04:00Z | 14/+1594/-719 | test-corpus-registry | **blocked** | ACTIVE foreign claim hermit-drain-corpus@devbig030 exp 04:00Z | tests: batch 12 backend-parity/contract fixtures into one manifest rec |
| #2067 | `2048ebd909b8` | 0.9 | draft | unstable | none | 4/+210/-823 | validate.rs-producer; compat-scorecard{#2067,#2084} | **aggregate** | serialize: validate.rs-producer | Classify localhost replay coverage by measured state |
| #2069 | `4b5c13bd6d64` | 0.8 | draft | dirty | EXPIRED claude-coord-fable@devbig030 exp 21:26Z | 9/+1081/-859 | - | **needs-rebase** | conflicts; rebase would drop 859 stale lines vs main | Let an e2e verify cell declare its guest's deterministic terminal stat |
| #2070 | `5bf16e145861` | 0.8 | ready | unstable | none | 2/+255/-5 | validate.rs-producer; rust-validate-production{#1999,#2070} | **aggregate** | serialize: validate.rs-producer | Require evidence-bearing record/replay verdicts |
| #2074 | `10833f176e04` | 0.8 | draft | dirty | none | 5/+601/-1188 | - | **needs-rebase** | conflicts; rebase would drop 1188 stale lines vs main | Enforce review and timeout-cap integrity |
| #2076 | `a7378f175e6b` | 0.8 | ready | unstable | none | 11/+245/-1109 | validate.rs-producer | **aggregate** | serialize: validate.rs-producer | Bind rust-script validation cache to every included module (current-ma |
| #2077 | `85649c96b053` | 0.8 | ready | unstable | none | 7/+168/-963 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Name the Reverie pin gate ancestor-and-monotonic |
| #2078 | `367fa13859ef` | 0.8 | draft | unstable | EXPIRED claude-coord-fable@devbig030 exp 21:26Z | 8/+1386/-37 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Wake robust-futex waiters when their owner thread dies |
| #2079 | `bb5a9b1066c7` | 0.7 | draft | dirty | none | 6/+337/-1509 | validate.rs-producer | **needs-rebase** | conflicts; rebase would drop 1509 stale lines vs main | Fail closed on undeclared validation write domains |
| #2083 | `66138d34910d` | 0.5 | draft | unstable | EXPIRED claude-coord-fable@devbig030 exp 21:26Z | 2/+34/-20 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Expect LiteInst guests to create threads and child processes |
| #2084 | `79b8984f2cdf` | 0.5 | ready | clean | EXPIRED hermit-drain-corpus@devbig030 exp 22:40Z | 2/+546/-56 | test-corpus-registry; compat-scorecard{#2067,#2084} | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | Record real stdout parity operands |
| #2085 | `3c540dcd27f9` | 0.5 | draft | unstable | none | 4/+462/-3 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Report progress before verification divergence |
| #2088 | `bbf817168f61` | 0.5 | ready | dirty | EXPIRED claude-coord-fable@devbig030 exp 21:26Z | 6/+669/-657 | - | **needs-rebase** | conflicts; rebase would drop 657 stale lines vs main | ci: stop serializing e2e manifest consumers behind the Cargo build loc |
| #2089 | `a9955de54f93` | 0.5 | draft | unstable | none | 1/+233/-60 | validate.rs-producer; validate-admission{#1927,#2007,#2089} | **aggregate** | serialize: validate.rs-producer | Guard managed-slot validation behind ci-hub admission |
| #2090 | `5336bf93ab14` | 0.5 | draft | unstable | none | 1/+36/-11 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Add hosted local-validation label report |
| #2093 | `ebd81e9011b7` | 0.4 | draft | unstable | none | 3/+217/-676 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | Use the prebuilt manifest planner in validation DAGs |
| #2097 | `47236384bd4e` | 0.4 | ready | dirty | none | 13/+136/-728 | - | **needs-rebase** | conflicts; rebase would drop 728 stale lines vs main | Advance the Reverie pin to 4664b5e5 |
| #2105 | `a5d13355e414` | 0.3 | draft | unstable | none | 5/+448/-671 | validate.rs-producer | **aggregate** | serialize: validate.rs-producer | Represent empty manifest buckets as typed skips |
| #2108 | `d3e90b982689` | 0.1 | draft | unstable | none | 41/+2120/-305 | test-corpus-registry; backend-parity-c-registration{#1665,#1691,#1730,#1757,#2108} | **aggregate** | serialize: test-corpus-registry | tests: fold #1719 and #2066's new registrations (batch 4) — NOT ready  |
| #2114 | `0e3b1d23dad9` | 0.1 | ready | unstable | none | 6/+47/-6 | test-corpus-registry | **aggregate** | test-corpus registry; survivor = a batch PR; owned by hermit-drain-corpus@devbig030 | Make application e2e guests hermetic against shared-directory mutation |
| #2118 | `2bca7efdf95e` | 0.1 | draft | unstable | none | 1/+543/-18 | - | **needs-review** | mergeable(unstable); no exact-head LEDGER receipt | detcore: replace the classification census triple with per-class membe |

### rrnewton/reverie

| PR | head12 | age(d) | draft | merge | claim | residue f/+/- | cluster | disposition | note | title |
|---|---|---|---|---|---|---|---|---|---|---|
| #388 | `bf8ee2dd8ebc` | 4.6 | draft | behind | none | 1/+73/-13 | ptrace-sigtrap-provenance{#388,#395} | **needs-rebase** | behind base; 13 lines of main missing from head | reverie-ptrace: deliver a guest-raised SIGTRAP instead of swallowing i |
| #389 | `6c1deab9b96a` | 4.5 | draft | behind | none | 5/+399/-347 | - | **needs-rebase** | behind base; 347 lines of main missing from head | liteinst: move instrumentation counters out of the ptrace crate |
| #391 | `af3fda4b672a` | 4.2 | draft | behind | none | 1/+38/-13 | - | **needs-rebase** | behind base; 13 lines of main missing from head | liteinst: assert the host dispatch paths are quiet, not just that in-g |
| #395 | `28f84b13437e` | 4.1 | draft | behind | none | 2/+290/-13 | ptrace-sigtrap-provenance{#388,#395} | **needs-rebase** | behind base; 13 lines of main missing from head | reverie-ptrace: deliver a guest's own int3 instead of swallowing it |
| #399 | `09ba5db5615b` | 4.0 | draft | behind | none | 1/+7/-25 | - | **needs-rebase** | behind base; 25 lines of main missing from head | docs: add Codex coordinator output discipline |
| #400 | `446298a155e5` | 3.9 | draft | behind | none | 1/+18/-15 | - | **needs-rebase** | behind base; 15 lines of main missing from head | Test KVM received-rights rollback by endpoint identity |
| #402 | `867fce5b6cd6` | 3.8 | ready | behind | none | 1/+87/-3 | - | **needs-rebase** | behind base; 3 lines of main missing from head | kvm: give a static image's heap its own arena instead of capping brk a |
| #403 | `96306604d780` | 3.8 | ready | blocked | none | 5/+614/-5 | - | **blocked** | GitHub state=blocked (required review/check unmet) | reverie-kvm: route timestamp reads through Tool |
| #404 | `e8352bdca84f` | 3.8 | draft | behind | none | 1/+50/-0 | - | **needs-rebase** | behind base; 0 lines of main missing from head | merge-gate: make a NO_RESULT cancellation say that it is not a failure |
| #407 | `9646804e8370` | 3.4 | draft | behind | none | 1/+63/-1 | - | **needs-rebase** | behind base; 1 lines of main missing from head | reverie-process: contain a cloned child's panic instead of aborting it |
| #410 | `b53b9f005baa` | 3.3 | draft | dirty | none | 2/+423/-223 | - | **needs-rebase** | conflicts; rebase would drop 223 stale lines vs main | Fail closed on incomplete vDSO patches |
| #411 | `9c9b61ef9288` | 3.3 | draft | dirty | none | 3/+3336/-0 | - | **needs-rebase** | conflicts; rebase would drop 0 stale lines vs main | Share DBI virtual time across copied processes |
| #413 | `6fa1be8b93b2` | 3.3 | draft | behind | none | 1/+57/-2 | - | **needs-rebase** | behind base; 2 lines of main missing from head | safeptrace: resume Died tracees before terminal rewait |
| #429 | `7eefbd82a0ab` | 0.9 | draft | behind | none | 1/+10/-5 | - | **needs-rebase** | behind base; 5 lines of main missing from head | docs: fix commit-trailer example to match enforced attribution format |
| #430 | `9b17ec0230ab` | 0.5 | draft | clean | EXPIRED claude-coord-fable@devbig030 exp 22:50Z | 7/+499/-76 | - | **needs-review** | mergeable(clean); no exact-head LEDGER receipt | Let LiteInst guests create threads and child processes |
