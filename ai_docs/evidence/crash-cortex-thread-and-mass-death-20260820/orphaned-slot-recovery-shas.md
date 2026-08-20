# Recovery SHAs for the three orphaned slots — all three are pushed

**Established:** 2026-08-20, ~19:3xZ, on `devbig030`, **read-only**. Nothing released,
reclaimed, quarantined or pushed.
**By:** hermit-902 (claude, opus-5).
**Purpose:** satisfy the standing rule *never remove a dirty slot without a documented
recovery SHA*. This is that document.

## Verdict

**All three slots' work is on a remote. None of them carries anything irreplaceable in
git.** Every working tree is clean — `git status --porcelain` returns **0 paths** for all
three, which includes untracked files.

Sweeping the **full** `ls-remote` output of `rrnewton/hermit` for refs whose *value* is the
local HEAD — rather than looking up the local branch *name* — every one of the three is
both a branch head and **an open PR head**:

| slot | local branch | HEAD | refs on `rrnewton/hermit` with that exact value |
| --- | --- | --- | --- |
| `slot10` | `fix/bump-reverie-pin-ee6716a` | `27526f68f4d7ce526a6f5f7749d28c2252aee629` | `refs/heads/fix/bump-reverie-pin-ee6716a`, `refs/pull/2281/head` |
| `pollrevents` | `fix/poll-partial-revents-copyout` | `e8511b270d7005c3e08db0cebd6f74c0b4b30ca3` | `refs/heads/fix/poll-partial-revents-copyout`, `refs/pull/2272/head` |
| `revents-measure` | `diag/measure-replay-partial-revents-copyout` | `2ba0ff09794da7d9bc57cc2a14938487d5881e83` | `refs/heads/fix/verify-report-says-what-a-verdict-means`, `refs/pull/2269/head` |

Note the third row: **the branch name on the remote is not the local branch name.** Note
also that the PR head refs give a second, independent copy of each commit on GitHub.

Each repo has three remotes: `origin` = `https://github.com/rrnewton/hermit.git`,
and `fbexp` = `upstream` = `https://github.com/facebookexperimental/hermit.git`. **None of
the three branches is on `facebookexperimental/hermit`** — that is expected; the fork is
where work lives, and it is what matters for surviving host loss.

Every negative was taken with a control, so an absence is a real answer rather than a
failed lookup: `ls-remote <remote> refs/heads/main` returned
`72e453808194c84173570045d66147202a552363` for `rrnewton/hermit` and
`9084dca5c7476d83354a7462ee3cf45bff630cbd` for `facebookexperimental/hermit` on every
query.

## The trap this nearly fell into, twice

**`revents-measure`'s branch name does not exist on any remote.** Looking up
`refs/heads/diag/measure-replay-partial-revents-copyout` returns nothing on all three
remotes. A name-based check therefore reports "unpushed" — and would be wrong. The commit
is on `origin` under a *different* branch name, `fix/verify-report-says-what-a-verdict-means`,
and is additionally the head of **PR #2269**. It was found by matching the **sha** against
the full `ls-remote` output, not the name.

Separately, an earlier pass on `slot10` queried the **parent** repository's remotes
(`rrnewton/dev-hermit`, `facebookexperimental/hermit`, `facebookexperimental/reverie`),
got three empty results, and was one step from "not pushed anywhere". The slot's own repo
has `origin` = `rrnewton/hermit`, which is where it lives.

**So: enumerate the remotes of the repository you are actually asking about, and match on
sha, not on branch name.** Two independent ways to manufacture a false "at risk" verdict
on the same three slots.

## What is on them, in full

Each slot contains exactly one repository, `hermit/`. No `reverie/`, no `liteinst2/`.

`revents-measure` is 4 commits ahead of `origin/main`, all authored by `hermit-revents`:

```
2ba0ff097  Name the compared streams in the log diff too
81ba5e59d  Document why record mode does not virtualize time
aae1bc141  Report what a verification verdict is entitled to mean
3e96975fc  Name what each verification comparison actually compared
```

## The one category that is *not* on a remote, and why it is small

Gitignored artifacts are outside every branch by construction, so "the branch is pushed"
says nothing about them. Checked with `git status --porcelain --ignored=matching`:

| slot | total on disk | ignored paths |
| --- | --- | --- |
| `slot10` | 3.5 GB | `target/` only |
| `pollrevents` | 8.8 GB | `target/` only |
| `revents-measure` | 13 GB | `target/`, and `ignored/` |

`target/` is build output and is reproducible by definition. The only non-build ignored
content anywhere in the three is `revents-measure/hermit/ignored/e2e` — **1.1 MB, 9 run
directories, 294 files**, dated 2026-08-15 20:18–20:34 UTC (pre-reboot). Each run is a
single cell, e.g.:

```json
{"schema": 1, "tests": 1, "cells": 1, "passed": 1, "failed": 0, "errors": 0,
 "by_mode": {"chaos": {"cells": 1, "passed": 1}}}
```

That is the by-product of iterating on a fix — one-cell local e2e runs with
`results.jsonl`, `junit.xml`, `summary.json` — not a measurement corpus. Re-runnable, and
cheap to re-run. It is the only thing in the three slots that a reclaim would actually
destroy, and it is worth 1.1 MB.

## What this means for the freeze

The freeze was imposed against unexplained catastrophe. That frame is retired: the fleet
died at the 2026-08-16T04:17:54Z reboot (`README.md`, `coordinator-death-is-not-the-cause.md`
in this directory). What remained was "three orphaned slots with unverified branch state",
and their branch state is now verified: **all three are on a remote at their exact local
HEAD, all three working trees are clean, and the total unbacked content across them is
1.1 MB of reproducible e2e output.**

Reclaiming the three would return roughly **25.3 GB**.

---

# Reclaim record — `pollrevents` and `revents-measure`, 2026-08-20

**Reclaimer:** `hermit-902` (claude, opus-5), passed as `--actor hermit-902`.
**Authorised by:** the owner, on the evidence above, after retiring the freeze's original
justification. **`slot10` was explicitly held and was not touched.** The 19 unassessable
slots were explicitly excluded and were not touched.

## What was run

```
./scripts/release-worktree.rs --slot pollrevents --clean --stale-clear \
  --expected-generation 3ac87f92-e589-489d-9e37-bc1ab32d6a3e:3419300:31290638 \
  --actor hermit-902                                                        # rc=0

./scripts/release-worktree.rs --slot revents-measure --clean --stale-clear \
  --expected-generation 3ac87f92-e589-489d-9e37-bc1ab32d6a3e:3522131:33580787 \
  --actor hermit-902                                                        # rc=0
```

No `--force`. No `--push` (nothing needed pushing). Both leases were expired — by 109.1 h
and 110.9 h respectively — `state=active`, not quarantined.

**The tool proved reachability itself rather than trusting this document.** Both runs
printed:

> `pre-recycle guardrail: verifying every outer and initialized nested HEAD is reachable from its own origin...`
> `✓ verified 1 outer/nested repository HEAD(s) on origin`

That is an independent confirmation of the push status recorded above, taken by the
release path at the moment of removal.

## Recovery handoff

Everything needed to restore either slot's work:

| slot | branch | recovery SHA | where it lives now |
| --- | --- | --- | --- |
| `pollrevents` | `fix/poll-partial-revents-copyout` | `e8511b270d7005c3e08db0cebd6f74c0b4b30ca3` | `origin refs/heads/fix/poll-partial-revents-copyout`, `origin refs/pull/2272/head`, and the local branch in the primary `hermit/` checkout |
| `revents-measure` | `diag/measure-replay-partial-revents-copyout` | `2ba0ff09794da7d9bc57cc2a14938487d5881e83` | `origin refs/heads/fix/verify-report-says-what-a-verdict-means`, `origin refs/pull/2269/head`, and the local branch in the primary `hermit/` checkout |

All four remote refs were re-queried **after** the reclaim and still hold those exact
SHAs. Both feature branches also still exist in the primary checkout at the same SHAs —
`release-worktree.rs` never deletes a feature branch, so recovery does not even require
the network.

## What was actually destroyed

`revents-measure/hermit/ignored/e2e` — 1.1 MB, 9 single-cell chaos-mode run directories,
294 files, dated pre-reboot. Gitignored, therefore on no branch and no remote. Accepted
knowingly: it is the by-product of iterating on a fix and re-runnable. Nothing else in
either slot was outside git except `target/`, which is build output.

## Verified after

```
worktrees/pollrevents        removed        registry row ABSENT
worktrees/revents-measure    removed        registry row ABSENT
worktrees/slot10             PRESENT 3.5G   status=active      <- untouched, as instructed
```

Health tick, before → after: `dead_owner_slots` 4 → **2** (`claude-exitgate` and `slot10`
remain), `reclaimable_slots` 14 → 13, `reclaimable_gib` 63.06 → 60.23,
`unassessable_owner_slots` **19 → 19** (untouched, as instructed).

**Disk returned: an upper bound of 21.8 GiB** (8.8 + 13). It is an upper bound, not a
measurement: slots are seeded with `cp -a --reflink=auto`, so extents are shared and the
true figure is lower. No before-reading of `df` was captured, so the actual figure is not
established here.

## Advisory surfaced by both runs, not acted on

The canonical registry verifier reports **6 drift rows**, unrelated to this reclaim and
outside its scope:

```
cadence2              hermit recorded=lane/chaos-ptrace-strict-ratchet  actual=lane/chaos-ptrace-fp-reduction
ci                    hermit recorded=feature/bind-local-full-e2e-kvm-coverage  actual=land-2280
claude-fixtureassert  hermit recorded=fix/umask-mode-setup-failure-is-not-a-pass  actual=fix/assert-expected-values-in-three-parity-fixtures
codex-hostsweep907    OWNERS must have exactly one mutating owner, found 0
dbt-symbol            hermit recorded=diag/dbt-undefined-symbol-640d  actual=fix/dbt-abi-v2-exports-640d
fix-futex             hermit recorded=replay/ratchet-ptrace  actual=rebase/pr2251
```

The suggested remedy is `scripts/allocate-worktree.rs --repair`. **Not run** — it is
another agent's territory and outside this authorisation.

---

## Original pre-reclaim note, retained

**Nothing here reclaims, releases or pushes anything** — that is the owner's decision, and
there are reasons to hold that are independent of data loss: all three branches are
unlanded and each is the head of an open PR (**#2281**, **#2272**, **#2269**), and
`slot10`'s pin bump is being redone fresh by hermit-901 in its own worktree, so the two
versions should be reconciled before that slot is disturbed.
