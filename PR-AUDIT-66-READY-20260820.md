# Audit of the 66 ready hermit PRs — 2026-08-20

`[hermit2, prqueue, unresolved, devbig014, role=impl]`

**Audit only. Nothing was landed, closed, rebased, force-pushed, or commented on.**
Task: `audit-the-66-ready-prs-before-any-draining`.

All measurements are against `origin/main = 269b1e4f72e93d4c8e32f00891a5a1ae15d42bff`
(2026-08-19 18:53:12 -0700), fetched fresh into `refs/audit-20260820/main` inside
`/home/newton/work/dev-hermit/hermit` — never against that checkout's local `main`
branch, which is broken (see §0).

---

## 0. Read this first: the shared `hermit/` checkout is on reverie's lineage

Not part of the ask; found while measuring, and it blocks any drain dispatched into
`hermit/`.

```
$ git -C hermit rev-parse main               -> af82f1b92a2762840dc5894984be5e1f5bfc5cb9
$ git -C reverie rev-parse origin/main       -> af82f1b92a2762840dc5894984be5e1f5bfc5cb9   IDENTICAL
$ git -C hermit merge-base HEAD origin/main  -> (empty)
$ git -C hermit merge-tree --write-tree origin/main HEAD
  fatal: refusing to merge unrelated histories
$ git -C hermit status -sb                   -> ## main...origin/main [ahead 927, behind 1791]
$ git -C hermit diff --cached --name-only | wc -l   -> 3615
```

`hermit/`'s local `main` points at **reverie's** main tip, with the whole hermit tree
(3615 paths) staged on top. A commit made there becomes a child of a reverie commit.
The parent repo's committed gitlink (`e2f9a36b`, 2026-08-13) is fine and *is* on
hermit's `origin/main`; the damage is confined to this checkout's branch pointer and index.

---

## 1. The headline counts

| | count |
|---|---|
| open PRs on rrnewton/hermit | 116 (66 non-draft, 50 draft) |
| **superseded by main (tree equality)** | **0 of 66** |
| CONFLICTING with main | 27 |
| CLEAN against main | 39 |
| — CLEAN but in a mutually-exclusive rival pair | 12 |
| — CLEAN but carrying an explicit landing hold or a rejection at head | 2 |
| — **CLEAN, no rival, no hold → landable as-is** | **25** |

The 25: **#1851 #2150 #2216 #2224 #2226 #2227 #2228 #2229 #2230 #2231 #2233 #2234 #2236
#2237 #2241 #2251 #2264 #2271 #2272 #2273 #2282 #2283 #2288 #2307 #2312**
(+7,746 lines total, median +85).

### "Stale" is the wrong word for this pool

The authoritative subsumption test from the drain playbook — tree equality, not
patch-id — found **zero** absorbed PRs:

```
$ git merge-tree --write-tree refs/audit-20260820/main <head>   # for all 66
  0 produced a tree equal to main's tree (c5853fa496b72e424885f305daa6fb3e301ab0d4)
```

Per-file byte-identity found exactly one PR with any absorbed files: **#2108, 62 of its
100 files already byte-identical in main.**

Why nothing went stale: **main stopped moving.**

```
$ git rev-list --count --since='N days ago' refs/audit-20260820/main
  30d 1380 | 14d 238 | 7d 37 | 3d 9 | 2d 4 | 1d 4
```

46 commits/day a month ago → 34/day two weeks ago → **5.3/day this week → 2/day now.**
The deepest branch point among the 66 is 82 commits behind (#2059); the median is ~20.
No PR head is older than 8 days (22 heads dated 2026-08-12, newest 2026-08-19).

**Supersession here is PR-vs-PR, not PR-vs-main.** That is the real finding, and it is
in §3.

---

## 2. Two facts that change how you read every row

**Review state is not in GitHub.** All 66 report `reviewDecision: NONE`; across all 66
there is exactly **one** GitHub review object (a `COMMENTED` on #2108). Every review
verdict lives in labels and in `APPROVED-AT` / `CHANGES-REQUESTED-AT` comments.

Checked those: **17 PRs carry a `passed-review-*` label, and all 17 have at least one
`APPROVED-AT` comment naming their current head.** The labels are not stale. But only
**one PR (#2281) carries both `passed-review-claude` and `passed-review-codex`**, which
is what the core-review merge gate wants; 16 have exactly one lane; **41 have no review
label at all.**

One PR is rejected at its current head: **#2176**, `CHANGES-REQUESTED-AT: claude
b3ca4e155098ad6e6f32f507ed72855da0638010` (2026-08-13, two separate reviewer comments).

**Authorship gives no ownership signal.** All 66 have `author: rrnewton` (one fork
credential). Ownership must come from the commit trailer lane, which is per-task and
ephemeral — 35 distinct lanes across 66 PRs, largest `hermit-deslop` with 7.
`grep -l 'DG_AGENT_NAME=' /proc/*/environ | wc -l` returns **0 processes on this box**,
so that liveness probe is uninformative here — it cannot show any lane alive *or* dead.
The honest statement is: no PR is abandoned by age, and none has a live author.

---

## 3. Six collisions — the real "close, not land" list

Every PR below is CLEAN against main, so nothing warns you. Verified by reading the
hunks, not the titles.

### 3.1 #2304 is already inside #2305 — byte-identical

```
$ git diff-tree -p e2b18509ba | git patch-id --stable   -> cd0873246a5b3a247933cd9f2c32c8934236e6ea   (#2304's only commit)
$ git diff-tree -p ad63e50bee | git patch-id --stable   -> cd0873246a5b3a247933cd9f2c32c8934236e6ea   (a commit in #2305's 6-commit stack)
```

Land #2305 → **#2304 is an empty close.** This is the only PR in the pool that is
redundant *today*.

### 3.2 #2304/#2305 vs #2315 — opposite fixes to the same line

`detcore/src/scheduler.rs`, the line
`info!("scheduler (step2_process_blocked): zero threads left anywhere, fizzling.")`:

* **#2315** — `-info!(...)` / `+trace!(...)`. Removes it from the compared INFO stream.
  Evidence: 4 divergences in 302 SaBRe cell-runs before, **0 in 612 after**.
* **#2304** — keeps it at INFO, relocates it to the scheduler loop's single terminal
  exit, and *adds a test asserting exactly one INFO occurrence*:
  `assert_eq!(scheduler_fizzle.len(), 1, ...)` over the literal
  `" INFO detcore::scheduler: scheduler (step2_process_blocked): zero threads left anywhere, fizzling."`
  in `hermit-cli/tests/sabre_examples.rs`.
  Evidence: recovers two named cells (`io-uring-ring-determinism` 124/123 → 120/120 INFO;
  `periodic-setitimer-delivery` 148/149 → 145/145).

If #2315 lands, #2304's new assertion cannot pass. **This is an owner choice, not a merge
order.**

### 3.3 #2305 vs #2314 commit 1 — same defect, incompatible designs

Defect: `gate.manifest` runs `target/debug/test-harness` before anything builds it, so a
cold `validate full` dies at node 3 of 59 with
`exit 127: target/debug/test-harness: No such file or directory`.

* **#2305** adds a *new* node `pre.manifest_plan` and asserts builders dedupe onto it.
* **#2314** keeps the *existing* `setup.manifest_plan` and repairs the dedup so
  `gate.manifest` retains its edge.

Same two files (`scripts/validate.rs`, `ci/manifest-plan/src/runner.rs`). Pick one; the
loser is a close, not a rebase.

### 3.4 #2250 vs #2311 — same one-token edit

`hermit-cli/src/lib.rs:1447`, `tracing::info!` → `tracing::debug!` on
`"reverie-kvm lifecycle phase timings"`. The two diffs differ **only in the comment above
the line** (6 lines of prose vs 9). Still at INFO on main
(`git grep -n 'lifecycle phase timings' origin/main -- '*.rs'` → `hermit-cli/src/lib.rs:1453`).

**#2250 is the superset** — it also fixes random-device `lseek` in
`detcore/src/syscalls/files.rs` and adds `hermit-cli/tests/kvm_info_log_determinism.rs`.
Land #2250, close #2311.

### 3.5 #2268 vs #2274 — two implementations of the same relabelling

Both delete the same `"Mismatch between run 1 and run 2 outputs."` strings in
`hermit-cli/src/bin/hermit/verify.rs` and replace them differently:

* **#2268** threads a `(String, String)` label pair through the comparator.
* **#2274** introduces `enum ComparedExecutions { Runs, RecordingAndReplay }` with
  `("run 1","run 2")` / `("recording","replay")`.

**#2269 explicitly stacks on #2268** ("That PR's commit is the parent here"). Choosing
#2274 orphans #2269's parentage. #2274 and #2269 are both CONFLICTING; #2268 is CLEAN.

### 3.6 The fixture fail-closed family — two competing idioms over 19 shared files

Same intent (a fixture that counts checks then `return 0` unconditionally can never go
red), two incompatible spellings, applied to overlapping files:

* **Idiom A** — `enum { EXPECTED_CHECKS = 4 };` + `return ok == EXPECTED_CHECKS ? EXIT_SUCCESS : EXIT_FAILURE;`
  → **#2276** (38 fixtures, 35 files, `passed-review-codex`) and **#2277** (31 fixtures, 32 files).
* **Idiom B** — `#define EXPECTED_CHECKS_UNDER_HERMIT 4` + an explicit
  `if (ok != ...) { fprintf(stderr, ...); return 1; }`
  → **#2286** (3 fixtures) and **#2287** (16 fixtures).

Overlap: **#2287's entire 16-file set is inside #2276's 35**; #2276∩#2286 = `fadvise_hints.c`,
`msync_writeback.c`; #2277∩#2286 = `mincore_residency.c`. **19 files are claimed twice.**
Whoever lands first sets the idiom; the rest need rework, not rebase.

**#2282** (assert documented expected values in `eventfd_probe`, `vectored_io`,
`sendfile_copy`) and **#2283** (`umask_mode` exits nonzero when `mkdtemp` fails) are
in the same theme but share no files with anyone — genuinely independent.

### 3.7 #2059 vs #2176 — two implementations of pinned pipe capacity

Both add `const DETERMINISTIC_PIPE_CAPACITY` to `detcore/src/syscalls/files.rs`
(`4 * 1024` vs `4096`). Main has neither. **Neither is ready:** #2059 is CONFLICTING with
the oldest branch point in the pool (82 behind); #2176 has `CHANGES-REQUESTED-AT` at its
current head plus an owner implementation handoff for a pipe-growth finding.

---

## 4. Where the 27 conflicts actually are

`git merge-tree --write-tree --name-only origin/main <head>`, per PR:

* **8 of 27 conflict only in generated/bookkeeping artifacts** (`SCORECARD.md`,
  `ci/expected-e2e-plan.json`, `tests/e2e/manifests/*.toml`) — regenerate, not rework:
  **#2059 #2108 #2255 #2257 #2261 #2263 #2267 #2280**.
* 19 touch real source, and **11 of those conflict in exactly one source file.**
* Deepest three: **#2070** (22 paths / 18 source), **#2303** (12 / 7), **#2213** (10 / 3).

Hotspots, by how many of the 27 hit them:

```
 11  ci/test_harness.sh                            #2012 #2070 #2129 #2182 #2212 #2213 #2215 #2275 #2281 #2285 #2303
  8  SCORECARD.md                                  #2129 #2255 #2257 #2261 #2263 #2267 #2280 #2303
  6  tests/e2e/manifests/c-programs.toml           #2059 #2070 #2255 #2261 #2263 #2303
  5  hermit-cli/src/bin/hermit/run.rs              #2070 #2217 #2269 #2274 #2303
  5  tests/e2e/manifests/backend-parity-c.toml     #2108 #2213 #2263 #2280 #2303
  4  scripts/validate.rs                           #2012 #2070 #2212 #2222
  4  ci/expected-e2e-plan.json                     #2070 #2255 #2257 #2261
  4  hermit-cli/src/bin/hermit/verify.rs           #2070 #2129 #2269 #2274
  4  tests/e2e/manifests/inventory/test-files.json #2108 #2213 #2217 #2303
  4  tests/e2e/manifests/system-utils.toml         #2108 #2213 #2263 #2267
  4  hermit-cli/src/bin/hermit/record_start.rs     #2265 #2269 #2274 #2303
  2  hermit-cli/src/metadata.rs                    #2260 #2262   (the RECORD_VERSION counter)
```

`ci/test_harness.sh` is the single hottest file in the pool: landing anything that touches
it re-conflicts up to ten other PRs. The union over the three verification-command files
(`run.rs`/`verify.rs`/`record_start.rs`) is 7 PRs: #2070 #2129 #2217 #2265 #2269 #2274 #2303.

---

## 5. Explicit landing holds written by the authors themselves

Do not treat these as ready regardless of mergeability:

| PR | what its own body says |
|---|---|
| #2108 | "**STATUS: NOT READY TO LAND** — no green receipt yet" |
| #2218 | "**LANDING HOLD** — overlaps #2129" (same `AGENTS.md` closure section) |
| #2225 | "**LANDING HOLD — OVERLAPPING POLICY PRS:** #2129 and #2218 both edit this region" |
| #2129 | carries the "now-rejected close-at-publication rule" per #2218's body |
| #2262 | its `RECORD_VERSION` target `0x10a` was already consumed on main by `5dde81d08`; needs a fresh number *and* a rebase. Its substantive change (`memory: 1024*1024*1024` → `memory: default_config.memory`) is confirmed unlanded |
| #2286 | "**do not read this description as a review, an approval, or a request to land**" |
| #2269 | "**Stacks on #2268** … review or land it first" |
| #2176 | `CHANGES-REQUESTED-AT: claude b3ca4e155098` |

`#2218` + `#2225` + `#2129` are a three-way policy deadlock over one `AGENTS.md` region.
That is a single owner ruling, not three PRs.

---

## 6. Recommended order — ranked by whether the change is still wanted

Not by size and not by age. The ordering principle is: **first restore the ability to
produce a green validate, because that is what everything else is waiting on; then land
guest-visible determinism fixes; then evidence honesty; then bulk.**

### Tier 0 — decisions only the owner can make (four rulings, land nothing yet)

1. **The fizzle line: #2315 (demote to TRACE) or #2304/#2305 (keep at INFO, emit once)?**
   §3.2. #2315's body states the stakes: "Nine pieces of finished work could not land
   because no validation run would pass."
2. **The manifest builder edge: #2305's new `pre.manifest_plan`, or #2314's repair of the
   existing `setup.manifest_plan`?** §3.3.
3. **The fixture idiom: `EXPECTED_CHECKS` (#2276/#2277) or `EXPECTED_CHECKS_UNDER_HERMIT`
   (#2286/#2287)?** §3.6. 19 files hinge on it.
4. **The `AGENTS.md` closure section: #2129, #2218, or #2225?** §5.

### Tier 1 — unblock a green validate (land immediately after ruling 1 and 2)

The winners of rulings 1 and 2, plus:

* **#2314 commit 2** — `ci/manifest-plan/src/canonical_verdict.rs`, `pub comparison:
  ComparisonReport` → `Option<ComparisonReport>`. The producer documents and writes
  `null`; the consumer declared it non-optional, so a truthful `no_result` surfaced as
  `incomplete verification report: invalid type: null` and mislabelled the same 8 dbt
  cells as ERROR on three consecutive full runs. **Independent of #2237** — verified,
  `git show 6a68515217 | grep -c 'NO_RESULT_EXIT_CODE\|no_result_propagation_bracket'`
  → 0.
* **#2237** (CLEAN, `passed-review-codex`, `APPROVED-AT` at head) — `scripts/validate.rs`
  exit 75 stays a no-result instead of being serialized as a product failure. Different
  layer from #2314 c2; both are wanted.
* **#2307** (CLEAN, +17/-7, one file) — the Node/V8 cell diverged in `wait4` because
  `PATH` selected a Chef wrapper that runs `scribe_cat` telemetry before exec'ing node.
  One flaky cell removed from the gate for a real, named reason.

### Tier 2 — guest-visible determinism and correctness, no rivals, CLEAN

Ordered by how directly they change what a guest observes:

| PR | size | what it changes |
|---|---|---|
| **#2288** | +143/-3 | `sched_setattr` returned `Ok(0)` for a null pointer, an undersized buffer, and an undefined policy — Linux returns EINVAL/E2BIG/EINVAL. A guest branching on the error took the wrong branch *only* under Hermit. |
| **#2233** | +7/-1 | LiteInst `pselect6` leaked kernel-internal errno 512 (`ERESTARTSYS`) to userspace instead of `EINTR`. |
| **#2264** | +1040/-56 | Fail closed on masked record/replay `ppoll`; preserve results across timeout copy-out faults. |
| **#2272** | +226/-38 | `poll` did not capture its partial `revents` copy-out on an error return. |
| **#2236** | +614/-44 | `rt_sigsuspend` was modelled as generic external I/O while `pause` was an indefinite scheduler wait, so an all-threads-waiting program wedged the deadlock diagnostic itself. `passed-review-codex` + `post-facto-human-review`. |
| **#1851** | +633/-19 | `chown`/`fchown`/`fchownat`/`lchown` moved from PassThrough to Determinized — completes the virtual-root identity. Fixes #1849. Three adversarial-review labels + `post-facto-human-review`. |
| **#2150** | +1865/-27 | Per-thread timer slack and `/proc/<tid>/timerslack_ns` virtualized behind one Detcore state. Supersedes closed #1904. `post-facto-human-review`. |
| **#2312** | +80/-17 | `rdtsc` answered from the calling thread's logical time while `clock_gettime` answered from the coordinator clock (the sum over all threads) — two clocks disagreeing by milliseconds, diverging in rate with thread count. |
| **#2234** | +1086/-1 | e2e regression for the already-landed scheduler-deadlock exit fix (#2071). Test-only. `passed-review-codex`. |
| **#2251** | +200/-2 | `sched_loop_inner` logged its startup line *before* blocking on guest registration, so it entered the compared INFO envelope non-deterministically. `passed-review-codex`. Same family as ruling 1, **different line** — not a rival. |

### Tier 3 — evidence honesty (what a green is entitled to mean)

* **#2273** (+85/-5, CLEAN) — the backend-parity matrix printed `byte-identical DETLOG per
  probe` while invoking the lossy `Stripped` comparator. One value now drives both the
  flags and the displayed claim.
* **#2241** (+503/-77, CLEAN, `passed-review-claude`) — DBT `--verify-json` publishes
  `matched`/`diverged` instead of leaving the initial `no_result` stamp, and states
  plainly that stderr and internal logs were *not* compared, so DBT keeps
  `bitwise_parity: false`.
* **#2271** (+64/-0, one file, CLEAN) — deleting `--strict` from the `CompatMode::Strict`
  plan left `validate.rs --self-test` printing `SELF-TEST SUCCESS`. Adds the bracket that
  catches it.
* **#2216** (+653/-12, CLEAN, `passed-review-claude`) — the core-review merge gate had
  **no commit SHA anywhere in its 109 lines**, so a label earned on older code satisfied
  it. Worth landing early: it is the mechanism that makes every other review verdict in
  this pool mean something.
* **#2268 or #2274** (ruling), then **#2269** if #2268 wins.
* **#2282**, **#2283** — independent fixture-honesty fixes, no collisions.

### Tier 4 — dead-code removal, seven PRs, all CLEAN, one lane (`hermit-deslop`)

**#2224** (−238, `bnz` module), **#2231** (−432, `mvar` module), **#2227** (−21, unused
`AtFlags` converters), **#2228** (share record/replay iovec reader), **#2229** (share
vectored offset decoder), **#2230** (share shebang parsing), **#2226** (+185, propagate
`hermit record list`/`clean` inventory errors).

Six of the seven are pure deletion or de-duplication with no behaviour change. **None
carries a review label.** They are the cheapest way to move the open count and the least
valuable per PR — land them as one batch *after* the tiers above, not before, because
they compete for the same validate receipts.

### Tier 5 — rebase-then-land: conflicts only in generated artifacts

**#2255 #2257 #2261 #2263 #2267 #2280** — the chaos/coverage ratchets. Each enables cells
and therefore regenerates `SCORECARD.md` + `ci/expected-e2e-plan.json` + a manifest
`.toml`, which is exactly what they conflict on. #2257 and #2261 carry
`passed-review-codex` with `APPROVED-AT` at head; #2280 carries `passed-review-claude`.
They also conflict with *each other* on the same generated files, so they must be
regenerated in sequence, not in parallel.

### Tier 6 — needs a decision or real rework before it is worth anyone's time

* **#2262** — rebase + fresh `RECORD_VERSION`; substantive change confirmed still wanted.
* **#2260** — membarrier subscription; conflicts in `hermit-cli/src/metadata.rs`, the same
  `RECORD_VERSION` counter as #2262. Sequence the two together.
* **#2265** — container-child panics reported as `Signaled(SIGSEGV, true)`; 2-path conflict.
* **#2059 / #2176** — pipe capacity; see §3.7. Neither is ready.
* **#2217** — KVM L2 evidence; 3-path conflict, no review label.
* **#2275**, **#2222**, **#2212**, **#2215**, **#2285**, **#2012** — validate/CI mechanism,
  all conflicting on `ci/test_harness.sh` or `scripts/validate.rs`. Land at most one per
  rebase cycle.
* **#2281** — the only PR with **both** review lanes. Advances the reverie pin
  `c261050c` (2026-08-11) → `ee6716a6` (2026-08-12); confirmed a real advance, not a
  revert (`git merge-base --is-ancestor c261050c ee6716a6` succeeds). But reverie main is
  now `af82f1b9` (2026-08-17), so its target is itself 5 days behind — re-derive the pin
  rather than landing this one as written.
* **#2172**, **#2182** — reverie-coupled. #2172's dependency, reverie **#443, is MERGED**;
  #2182's dependency, reverie **#444, is open and CLEAN.**

### Tier 7 — the aggregates: treat as closes, not lands

**#2070** (54 files, 22 conflicted), **#2129** (25 files, folds 8 PRs), **#2212**,
**#2213** (folds #2181+#2177+#2161), **#2108** (137 files, 62 already byte-identical in
main, self-declared not ready), **#2303** (90 files, 2428/2499). Aggregates were built to
amortise a validate receipt that the box could not produce. They are now the deepest
conflicts in the pool and they hold the hottest files. Re-deriving each aggregate's
members against current main is cheaper than rebasing the aggregate.

---

## 7. The gated directive, and one gate that has already lifted

**Issue #1857 — "P2 (GATED on <10 OPEN PRs — owner 2026-08-04…)"**, the `dbi` → `dbt`
rename plus the `hermit-dbt`/`reverie-dbt` install-path split. Its own body records that
the rename was *originally* "GATED ON ZERO OPEN PRs because a tree-wide rename conflicts
with every in-flight branch", and notes ~107 open at filing. **Today: 116 hermit + 21
reverie.** The gate cannot clear on its own terms.

**Issue #2219 — "Owner decision docket: eleven rulings that unblock the PR drain (22 open
vs target 10)"**, last touched 2026-08-13. Same "target of under 10". Its stated premise
(22 open) is now off by 5.3x.

No other open issue conditions work on a PR count (searched `GATED on`, `zero open PRs`,
`open PRs`, `<10 OPEN` across 200 open issues).

**But docket ruling A1 is obsolete.** It says "Nothing in reverie can land" because main
required the `merge-gate-v2` context with `strict: true` while Actions are disabled:

```
$ gh api repos/rrnewton/reverie/branches/main/protection --jq '.required_status_checks'
  {"checks":[],"contexts":[],"strict":true, ...}       <- merge-gate-v2 is GONE
$ gh api repos/rrnewton/hermit/branches/main/protection
  HTTP 404 "Branch not protected"                      <- hermit main has NO protection at all
$ gh api repos/rrnewton/{hermit,reverie}/actions/permissions --jq .enabled
  false / false                                        <- Actions still off, as intended
```

GitHub's own verdict on the 12 ready reverie PRs: **9 MERGEABLE/CLEAN
(#430 #444 #447 #452 #453 #454 #459 #460 #461), 3 CONFLICTING/DIRTY (#411 #441 #445).**
Not one reports `BLOCKED`, and not one reports `BEHIND`. I did not attempt a merge, so
whether `strict: true` alone still blocks with zero required contexts is untested — the
`CLEAN` status is the evidence.

---

## 8. Spot-checks: the defect is still on main

"Still wanted" was checked against `origin/main`, not taken from the PR body, for the
following. Each is a live defect today.

```
#2311 / #2250  $ git grep -n "lifecycle phase timings" origin/main -- '*.rs'
                 hermit-cli/src/lib.rs:1453:   "reverie-kvm lifecycle phase timings",     (still under tracing::info!)

#2315 / #2304  $ git show origin/main:detcore/src/scheduler.rs | grep -n "fizzling"
                 info!("scheduler (step2_process_blocked): zero threads left anywhere, fizzling.");

#2288          $ git grep -A12 "fn handle_sched_setattr" origin/main -- 'detcore/src/**'
                 detcore/src/syscalls/threads.rs:1131  pub async fn handle_sched_setattr...
                 detcore/src/syscalls/threads.rs:1136      info!("Suppressing sched_setattr(...)")
                 detcore/src/syscalls/threads.rs:1141      Ok(0)
                 12 lines; the handler never reads the request.

#2262          $ git grep -n "1024 \* 1024 \* 1024" origin/main -- hermit-cli/src/metadata.rs
                 hermit-cli/src/metadata.rs:219:   memory: 1024 * 1024 * 1024,
               $ git grep -n "RECORD_VERSION" origin/main -- hermit-cli/src/metadata.rs
                 hermit-cli/src/metadata.rs:47:    RECORD_VERSION: RecordVersion = RecordVersion(0x10a);
                 -> confirms both halves of #2262's own banner: the 1 GiB hardcode is unlanded
                    AND 0x10a is already taken, so its bump is a silent no-op as written.

#2216          $ git show origin/main:scripts/core-review-protocol-lint.sh | wc -l          -> 109
               $ git show origin/main:scripts/core-review-protocol-lint.sh | grep -cE 'sha|SHA|[0-9a-f]{40}|commit'  -> 0
                 -> the gate really does contain no commit reference of any kind.

#2059 / #2176  $ git grep -n "DETERMINISTIC_PIPE_CAPACITY" origin/main               -> (no match)
                 -> main has neither implementation.
```

Referenced tickets, checked live: **#1849 is OPEN** (the issue #1851 fixes);
**#1904 is CLOSED** (the PR #2150 supersedes); **reverie #443 is MERGED**.

## 9. What I did not do

* No PR was landed, closed, rebased, force-pushed, labelled, or commented on.
* No branch pointer or index was modified. The only write to any repository was
  `git fetch origin main:refs/audit-20260820/main` in `hermit/` and `reverie/` — a new
  ref in a private namespace, no existing ref moved.
* I did not repair the `hermit/` checkout described in §0.
* I did not run validate against any head. Every "landable" judgement in this document
  means *merges cleanly and is not superseded or contested* — **not** *has a qualifying
  receipt*. No head in this pool has one.
