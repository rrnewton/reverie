# `claude-exitgate`: the reclaim was refused, and the refusal is a class

**Attempted:** 2026-08-20T19:25:54Z, on `devbig030`, by `hermit-902` (`--actor hermit-902`).
**Outcome: REFUSED, rc=1. Stopped. Nothing was removed.**
**Side effect the owner must know about: the attempt QUARANTINED the lease.** See below.

## The assessment, before the attempt

| | |
| --- | --- |
| slot contents | `hermit/` only |
| branch | `fix/route-failure-to-exit-in-three-parity-fixtures` |
| HEAD | `d857b6b853ad587f2ab9fbd261c49e8ed61463ba` |
| working tree | **clean** — `git status --porcelain` = 0 paths |
| live processes | **0** (six-signal scan) |
| authority | `dead`, `owner-boot-id-mismatch`, agent `hermit-teardown` |
| lease | `active`, expired by 91.6 h |
| disk | 4.2 GB |

**The work is merged into `main`.** PR **#2286** — `merged: true`, `state: closed`,
`head_sha` = `d857b6b85…` exactly. Its `merge_commit_sha` is
`febe2f9ceb11221b3fa8e13f31b6af7133daa011`, and
`compare/febe2f9c…main` returns `status: "ahead", behind_by: 0` — the merge commit is an
ancestor of `main`. The landed commit carries the same title and touches
`tests/backend-parity/fixtures/{fadvise_hints,mincore_residency,msync_writeback}.c`.

### What a reclaim would have destroyed, in bytes

- `target/` — **4.2 GB, 5869 files.** Build output, reproducible by definition.
- `ignored/` — **200 KB, 18 files**: `pr2286-mutants.XoBxgq/` (mutant binaries and JSON
  for the three fixtures) and `e2e-commands/work/`. These back the PR's mutation table —
  and **that table is verbatim in the merged commit message on `main`**, all twelve cells
  (`ok=5 rc 0` / `ok=4 rc 0` / `ok=4 rc 1` per fixture). The artifacts are regenerable; the
  conclusions are already in a tracked, durable place.

So the honest accounting is: **nothing irreplaceable, and less at risk than the two slots
already reclaimed** — those were merely pushed, this one is landed.

## Why it refused anyway

```
release-worktree: stale-clear refused: automatic handoff found dirty or non-durable work; lease quarantined
```

The recorded handoff says exactly which half failed:

```json
{"branch": "fix/route-failure-to-exit-in-three-parity-fixtures",
 "head": "d857b6b853ad587f2ab9fbd261c49e8ed61463ba",
 "clean": true,
 "origin_durable": false}
```

`clean: true`. It is **durability**, not dirtiness.

`remote_durable()` (`release-worktree.rs`) is precise about what it means:

1. `git ls-remote origin`, collecting **only `refs/heads/*`** tips — **`refs/pull/*` refs are
   deliberately not counted.**
2. durable if HEAD is one of those branch tips;
3. otherwise fetch the branch tips and try an **ancestry** proof.

`d857b6b85` fails both. On `origin` it exists **only** as `refs/pull/2286/head` — the
branch was deleted after the merge. And it is not an ancestor of `main`, because **#2286
was rebase-merged**: the landed commit `febe2f9c` is a different object.

## The class, which is the part that generalises

**A rebase-merged PR leaves its slot permanently un-reclaimable through this path.** The
predicate asks "is this commit *object* recoverable from a branch on origin". After a
rebase merge the answer is permanently no, even though the *change* is on `main` — because
rebasing rewrites object identity, and deleting the source branch removes the last
`refs/heads/*` that named the original.

The project's own landing recipe is `gh pr merge --rebase`. **So every slot whose PR landed
by rebase will read `origin_durable: false` forever.** That is a standing accumulation
mechanism, independent of the unassessable-owner one, and it points the same way: the
slot pool acquires rows it can never drain.

The refusal is not a bug. It is conservative and correct on its own terms — the object
genuinely is not recoverable from a branch. The gap is that it has no notion of
*"this change landed under a different object identity"*, and that is the normal outcome of
the normal landing path.

## The side effect: the lease is now quarantined

`prepare_stale_clear` quarantines on an incomplete handoff rather than leaving the row
untouched. Post-attempt:

```
status                = lease-quarantined     (was: active)
lease.state           = quarantined           (was: active)
last_transition_actor = hermit-902
automatic_handoff     = {complete: false, continuation: "<the exact command>", …}
```

The slot, its worktree and all 4.2 GB are **intact and still on disk**; the git worktree is
still registered. Quarantine is retirement from allocation, not destruction.

**But it blocks retry.** `prepare_stale_clear` checks `if lease.state == "quarantined"` and
returns `slot <slot> lease is quarantined` *before* it reaches the authority or handoff
logic. So the recorded `continuation` command in the handoff record cannot in fact succeed
as-is.

*This was predictable and I should have flagged it before running.* The quarantine branch
is in `prepare_stale_clear`, which I had already read earlier in the session. The
authorisation covered attempting the reclaim, and a refusal that records state is within
it — but the owner should have been told the attempt could move the row.

## Options, none taken

1. **Leave it quarantined.** Costs 4.2 GB. Safe, honest, and the row stays visible.
2. **Push the branch to `origin`** as `refs/heads/fix/route-failure-to-exit-in-three-parity-fixtures`
   at `d857b6b85`. That satisfies `remote_durable` on its own terms — not a bypass. It does
   **not** unblock this slot by itself, because the quarantine now refuses first, but it
   makes the durability verdict true.
3. **Clear the quarantine, then retry.** Requires whoever owns the lease lifecycle; not
   attempted here.
4. **Teach `remote_durable` about rebase-landed changes** — e.g. accept a patch-identity
   match against `main` (`git cherry`) alongside object ancestry. This is a behaviour
   change to a refusal and belongs with the owner, like the codex-sentinel asymmetry.

**Not done:** no push, no un-quarantine, no `--force`, nothing removed. `slot10` untouched.
The 19 unassessable slots untouched.
