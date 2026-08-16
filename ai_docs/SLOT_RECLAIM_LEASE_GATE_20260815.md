# 40 of 59 slots cannot be reclaimed through the sanctioned tooling — 2026-08-15

**A standing capacity blocker, not a hygiene backlog.** "22 candidates, nobody has inspected them"
was the wrong diagnosis: the candidates are not uninspected. **They are unreleasable regardless of
how safe they are**, and that is why a careful single-slot reclaim came back empty.

Established by `hermit-coord` on devbig030 while attempting to free exactly one slot for
`hermit-shell`. **No slot was released, nothing was forced, no registry was edited.**

## The number was right. The method was not. Both matter.

The original count classified slots by **looking for lease-block key names** at slot level —
`generation`, `tmux_pane_id`, `lease`, `owner_sidecar`, `lease_expires`. Re-derived instead from
**what `release-worktree.rs` actually reads**:

- **`:1011`** — the sentinel path fires only on `slot_state.get("coordinator_lease")` with
  `schema_version == 1`.
- **`:1080-1092`** — otherwise it loops `slot_state["agents"]` and requires **both**
  `tmux_pane_id` **and** `cgroup_path` on **every** entry. **`read_only` is not filtered.**

| | clears the gate | refused |
|---|---:|---:|
| key-name matcher | 19 | 40 |
| **behaviour-derived (correct)** | **19** | **40** |

**The totals agree and the lists do not.** The two methods disagree on exactly two slots, in
**opposite directions**, and the errors cancel:

- **`coord`** — key-name says *pass* (it has a slot-level `owner_sidecar`), behaviour says **FAIL**.
- **`lander`** — key-name says *FAIL* (no slot-level key), behaviour says **pass**.

**So the total survived by cancellation, not by correctness.** Anyone using the key-name *list* to
choose a slot would have tried `coord`, which refuses, and skipped `lander`, which clears. **A
matcher that is wrong in both directions produces a right number and a wrong answer.**

## The two refusals, verbatim

```
release-worktree: owner lease refused cleanup: recorded owner 'claude-f1-2213' lacks tmux_pane_id
lease data; re-run allocate-worktree.rs for this owner while its exact pane is live
```

```
release-worktree: --stale-clear requires --clean, --expected-generation, and --actor; it is
incompatible with --force/--push/--request-release
```

**Neither is a defect. Both are the allocator working.** The first names a real missing precondition;
the second enforces a CAS identity. What makes them a blocker is that **together they close both
doors on the same slot**:

- `--clean`'s stated remedy is *"re-run allocate-worktree.rs for this owner while its exact pane is
  live."* For a slot whose owner is **verified dead**, there is no live pane and there cannot be one.
- `--stale-clear` is the mode built for owner death — and it requires `--expected-generation`, which
  a slot with no lease block has never had.

**The pre-lease slots are stranded by construction.** `claude-f1-2213` was allocated 2026-08-13,
before lease recording existed.

## The one candidate that passed every safety gate

`claude-f1-2213` cleared all five and still could not be released:

| gate | result |
|---|---|
| owner liveness | **rc=0 VERIFIED-DEAD** — "no process carries `DG_AGENT_NAME=claude-f1-2213` across a complete scan of 607" |
| working tree | clean, 0 modified |
| process use | 0 processes with a cwd in the path |
| products | one (`hermit`) |
| **durability** | **recovery SHA `23175f2c86e799a9e5f10f799b47616390c94492`, contained in `origin/agg/ci-harness-2181-2177-2161`** |

### Branch-unpublished versus commit-reachable — the distinction that makes a durability proof survive review

Its branch `f1/pr2213-deletion-brackets` is **not on the remote**; `ls-remote` returns nothing. Its
**HEAD commit is** reachable — from a *different* ref. **The branch is unpublished while the commit
is recoverable, and only the second is durability.**

A check keyed on "is the branch pushed" calls this unsafe and refuses a reclaim that is provably
safe. A check keyed on tip-equality calls a commit absent when it is preserved as an ancestor. **Ask
whether the commit is reachable from any remote ref, from a repository with fresh remote refs — never
from the slot's own cache.** `slot07`'s own `git log --branches --not --remotes` reported **710
commits on no remote ref** while `ls-remote` showed its branch tip *is* its HEAD.

## A distinct sub-case: a read-only sharer vetoes a fully-leased owner

`coord` is refused **solely because two read-only sharers lack lease data**, while its mutating owner
carries both `tmux_pane_id` and `cgroup_path`. The agent loop at `:1080` does not filter `read_only`,
so an observer with no write authority can block release of a slot whose actual owner is complete.
**One slot is in this state today.** Recorded as an observation; whether the loop should skip
read-only entries is an owner decision about reclaim authority.

## Cross-host correspondence — same family, and NOT established as the same cause

**RELAYED:** devbig014 reports **31 of 36 dead-owner slots unreclaimable for a missing-lease-data
reason.** Same family, different host. **Their allocator is `wrkslots`; ours is
`scripts/allocate-worktree.rs` — different implementations, so a fault in one does not imply the
other.** The correspondence is worth noting and is not evidence about our cause.

## What is NOT proposed

**Not a timeout that breaks a lease.** Expiry makes reclaim *eligible*; it does not authorise it. A
live owner, an indeterminate owner, or a failed cleanliness or durability proof must still refuse —
that is the property being protected, and widening a destructive path under capacity pressure is
exactly when it is most dangerous.

The gap is narrower than "reclaim is too strict": **there is no sanctioned way to finalize a
lease-less slot after verified owner death.** Closing it is a change to reclaim authority and is
owner territory.
