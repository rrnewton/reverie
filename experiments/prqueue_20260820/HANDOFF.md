# HANDOFF — agent `prqueue`, 2026-08-20

`[hermit2, prqueue, unresolved, devbig014, role=impl]`

Session: `daabe828-8698-419f-bb73-3d51bc6701a2`. Three tg tasks, all worked and all
**closed**. Everything I did was **read-only against git**; I created no commit and no branch
in any repository.

---

## 1. Where I was working

**I was spawned into the shared parent checkout — I had no slot.** Confirmed by the owner as a
spawn error affecting every agent this session.

| absolute path | repo | what I did there |
|---|---|---|
| `/home/newton/work/dev-hermit` | dev-hermit (parent) | cwd for the whole session. **Wrote ONE untracked file** (§2). No commit, no branch, no index change. |
| `/home/newton/work/dev-hermit/hermit` | hermit | **read-only** + one `git fetch` into a private ref namespace (below). Never checked out, never committed. |
| `/home/newton/work/dev-hermit/reverie` | reverie | **read-only** + the same one fetch. Never checked out, never committed. |
| `/home/newton/work/dev-hermit/experiments/prqueue_20260820` | dev-hermit (parent) | this directory, created now, holds only this file. Untracked. |
| `/home/newton/.claude/projects/-home-newton-work-dev-hermit/memory/` | not a repo | 5 memory files + `MEMORY.md` (§2c). Outside the repo; survives a repo restart. |
| `/tmp/*` | — | scratch audit inputs (§2d). |

### Branches created or committed on: **NONE.**

```
$ git -C .        log -1 --format='%h %ci'   -> 9cf41a49  2026-08-19 23:31:05 -0700   (untouched)
$ git -C hermit   log -1 --format='%h %ci'   -> af82f1b92a 2026-08-17 17:49:05 -0400  (untouched)
$ git -C reverie  log -1 --format='%h %ci'   -> af82f1b9  2026-08-17 17:49:05 -0400   (untouched)
```

### The only writes I made to any `.git`

Two remote-tracking refs in a private namespace, created with
`git fetch --no-tags origin +refs/heads/main:refs/audit-20260820/main`. **No existing ref was
moved.** Delete them if you want, or reuse them — they are just a stable, dated pin of what I
measured against:

```
hermit   refs/audit-20260820/main  269b1e4f72     (origin/main, 2026-08-19 18:53:12 -0700)
reverie  refs/audit-20260820/main  af82f1b9       (origin/main, 2026-08-17 17:49:05 -0400)
```

---

## 2. Uncommitted right now — **this is the part that gets lost**

### (a) THE ONE THING AT RISK

```
/home/newton/work/dev-hermit/PR-AUDIT-66-READY-20260820.md      25,414 bytes, untracked (`??`)
```

The full audit of the 66 ready hermit PRs — 9 sections, grouped inventory, recommended landing
order, and every claim paired with the command that produced it. **It exists nowhere else.**
It is not in git, not pushed, not in `/tmp`, and only summarised in tg notes. If a restart
sweeps untracked files from the parent root, it is gone and costs several hours to rebuild.

**Per instruction I did not commit it and did not move it.** Copy it somewhere durable before
any clean. Its contents are ~85% reconstructible from the tg notes on
`audit-the-66-ready-prs-before-any-draining`, but the per-PR tables are not.

### (b) Files I edited that are **not** in a repo

```
/home/newton/.claude/projects/-home-newton-work-dev-hermit/memory/MEMORY.md
```
Two edits: compacted hook text 31,955 → 22,656 bytes because it was over its 24.4 KB read limit
and entries at the end were being silently dropped (all 242 links and 197 lines preserved,
verified by count); then added one `phantom-coverage:` index line. Another agent restructured the
file between my two edits — the current state is theirs plus my line. Still ~20.2 KB, above the
17.1 KB target; closing that gap needs entries merged into subindex files, which I deliberately
did not do to other agents' pointers.

### (c) New memory files (outside the repo — **not** at risk)

```
branch-protection-no-longer-freezes-reverie.md                       2,140 B
pr-supersession-here-is-pr-vs-pr-not-pr-vs-main.md                   3,077 B
debugger-suite-was-wired-twice-then-cut-in-1000.md                   3,357 B
an-allow-empty-ci-only-node-is-green-while-selecting-nothing.md      2,431 B
skid-margin-override-witness-was-never-built.md                      3,059 B
```

### (d) `/tmp` scratch — regenerable, but ~40 GitHub API calls to rebuild

```
/tmp/hermit_prs_full.json   207 KB   all 116 open hermit PRs, full metadata
/tmp/audit_out.json          70 KB   THE analysis output: per-PR merge-tree/subsumption/conflict results
/tmp/bodies_nd.json         413 KB   all 66 non-draft PR bodies
/tmp/bodies_short.txt        67 KB   condensed bodies, the readable form
/tmp/hermit_issues.json      68 KB   200 open issues
/tmp/reviews.json            16 KB   review state for all 116
/tmp/rev_prs.json           5.4 KB   reverie's 21 open PRs
/tmp/audit_prs.py           3.1 KB   the analysis script (regenerates audit_out.json in ~6 min)
/tmp/revcheck.py            1.2 KB   APPROVED-AT-binds-current-head checker
/tmp/{hermit_prs,heads,inv,h2,hermit_hist}.*        small
```

---

## 3. Committed but unpushed

**Nothing.** No commit was made in any repository. Nothing to push, nothing to rescue from a
local branch.

---

## 4. What I was doing, and the exact next step

All three tasks are **closed**. There is no half-finished work. What follows is what a fresh
agent needs so the conclusions are not re-derived.

### Task 1 — `audit-the-66-ready-prs-before-any-draining` (closed)

Audit only; no PR was touched. Headline: **0 of 66 are superseded by main** (tree equality via
`git merge-tree --write-tree`), because main's cadence collapsed from 46 commits/day a month ago
to 2/day now. 27 CONFLICTING, 39 CLEAN, of which **25 are landable as-is**. Supersession here is
PR-vs-PR: six mutually-exclusive rival pairs, all CLEAN against main so nothing warns you
(#2304 is byte-identical to a commit inside #2305; #2315 vs #2304/#2305 fix the same line in
opposite directions; #2250 vs #2311; #2305 vs #2314; #2268 vs #2274; #2276/#2277 vs #2286/#2287).

**Next step is the owner's, not an agent's:** four rulings in Tier 0 of the report, land nothing
until they are made. Full order in `PR-AUDIT-66-READY-20260820.md` §6.

### Task 2 — `timer_rs_cites_a` (closed)

Verdict **NEVER BUILT**. `reverie-ptrace/src/timer.rs:1137`'s "force-skid witness" enters the
tree in the same commit that adds the knob (`b91a7ed3`, one file, +57/−1) and hermit's whole main
history has zero occurrences of the string. A second phantom sits at `timer.rs:88-95`
(`HERMIT_SKID_WITNESS_TOKEN`): its reverie half is real, its hermit-supervisor half, retry gate,
and guest-env stripping never existed. **The knob itself is real and in live manual use — do not
delete it**; it is the deterministic injector in `a-panicking-replay-never-exits`'s pre-registered
arms.

**Next step:** an owner ruling on correct-the-comment vs build-the-witness. My recommendation is
correct the comment, covering **both** constants, and only after the panic-exit fix lands is there
a stable behaviour worth gating. I edited nothing.

### Task 3 — `the_whole_debugger_suite` (closed)

Verdict **BUILT AND REMOVED** — the opposite of task 2 despite the identical symptom.
`README.md:18` was true on 2026-07-22. Wired in `.github/workflows/ci.yml` (`1a0a66d294`), removed
(`3f73aa9774`), re-wired as a `ci/dag/hardware.json` node (`a33bbab7f4`), **cut 2026-07-27 by
`68ada78fe4` (#1000)** which took the hardware lane from 26 nodes to 4 to fit the PMU runner pool.
The deleted node's own budget is the starting point for any re-wiring: `timeout 1800,
est_duration_s 300, resources {pmu:1}, 2 GiB/4 GiB, deps ["build.workspace"]`.

Two things a resumer must not miss: **every precondition in `harness.py` raises `SkipTest` and
`unittest` exits 0 on all-skips**, so wiring it as-is registers a node that cannot go red; and
`e2e.manifest_debugger_c` is already a live node in `ci/dag/portable.json` that passes in ~5 s
having selected zero cells (`--ci-only --allow-empty` against a manifest with `ci: false` in all
five modes).

**Next step, four ordered steps, none taken:** correct `README.md:18` citing #1000; run the suite
by hand and record the result; close the skip surface; only then add the node in the privileged
lane with an explicit kill bound. Steps 2–4 need the box.

### Two hazards I found and did **not** repair

1. **`/home/newton/work/dev-hermit/hermit` is checked out on reverie's lineage.**
   `git -C hermit rev-parse main` and `git -C reverie rev-parse origin/main` both return
   `af82f1b92a2762840dc5894984be5e1f5bfc5cb9`; `merge-base` against hermit's own origin/main is
   empty ("refusing to merge unrelated histories"); `status -sb` reads
   `## main...origin/main [ahead 927, behind 1791]` with **3,615 staged paths**. A commit made
   there becomes a child of a reverie commit. The parent's committed gitlink (`e2f9a36b`) is fine
   and is on hermit's origin/main. **This is very likely a symptom of the shared-checkout spawn
   error and should be resolved before anyone is respawned into it.**
2. **`MEMORY.md` is still above its size target** (§2b).

---

## 5. Processes still running

**None.** Both background jobs I started have exited:

| pid | what it was | state |
|---|---|---|
| 1840937 | `git grep` for the skid knob across hermit's 16,426 refs | killed by me at ~2 min (too slow); confirmed gone |
| 2035822 | `git log -S` pickaxe over hermit main's history, 3 patterns | completed, output in `/tmp/h2.txt`, `ALLDONE` |

`ps -p 1840937 2035822` returns nothing for either. No hermit run, no build, no validate, and no
box time was ever consumed by me — all three tasks were read-only by instruction.

*(Note for whoever checks: `pgrep -f 'hermit_allrefs|audit_prs.py'` returns a hit that is the
grep's own command line, not a live job — the probe matches itself.)*
