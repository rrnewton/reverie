# Nested Worktree Protocol

Agent work uses registered nested slots. A normal slot has all three product
worktrees, each with its own writable build output:

```text
worktrees/slots/<slot>/hermit
worktrees/slots/<slot>/reverie
worktrees/slots/<slot>/liteinst2
```

Temporary validation checkouts and per-run Cargo homes live under
`worktrees/validate/`. Validation logs, run records, and any checkout-local
receipt recovered before cleanup remain under `ignored/validate/`; the
temporary checkout is removed when the run finishes. Use
`ci-hub validate-run --sweep-completed` to retry cleanup left by an interrupted
waiter.

Never develop in the primary `hermit/`, `reverie/`, or `liteinst2/` checkout,
and never create an agent worktree with raw `git worktree add`. Read
`AGENTS.md` and
`ai_docs/transient/2026-07-27-worktree-management-map.md` before changing slot
state.

## Allocate a slot

Use the registry-aware allocator from the repository root:

```bash
scripts/allocate-worktree.rs \
  --agent hermit-api \
  --task impl-example \
  --product all \
  --purpose "Implement the example change"
```

The allocator chooses a canonical available slot, creates the nested product
worktrees from their primary checkouts, and atomically updates both
`worktree-state.json` and the managed table in `worktrees/ACTIVE.md`. Those
files are machine-local; do not hand-edit their managed state. At most twelve
slots may be active and at most five clean slots may be parked. If allocation
reports an ownership conflict or capacity problem, preserve the existing slot
and escalate rather than deleting it.

`scripts/worktree_registry.rs` owns the schema-3 registry row and its status
values. The allocator, releaser, and registry checker import that type;
Python ownership and health readers validate the exact bytes through
`ci-hub/lib/worktree_registry.py` before reading their own fields. The readers
do not all ask the same question: capacity counts only `active`, guarded owner
checks also inspect held and release states, and the allocation guard treats
`released` and `owner-lease-revoked` as no longer assigning the agent a slot.
Those distinctions are deliberate. A missing or unknown status is not one of
them and is refused before any reader can supply its own default.

The production readers are `allocate-worktree.rs`, `release-worktree.rs`,
`check-worktree-registry.rs`, `check-agent-slot.sh`, `worktree-gc.sh`,
`slot-owner-authority.py`, `worktree_liveness.py`, `agent_identity.py`,
`slot_agent_parity.py`, `slot_disk_residue.py`, and `residue_sweep.py`.
`owner_sidecar` and `coordinator_lease` remain separately versioned artifacts;
the registry type preserves them without claiming their schemas.

`scripts/agent_snapshot.rs` separately owns the schema-1 ORC agent snapshot
entry fields used by this repository and the exhaustive meaning of every
accepted agent status. The two producers are `operational_health.py` and
`agent-container-reconcile-bridge.rs`. Its production readers are
`allocate-worktree.rs`, `release-worktree.rs`, `agent-podman.rs`,
`operational_health.py`, and the optional snapshot path in
`slot_agent_parity.py`; Python reaches the Rust contract through
`ci-hub/lib/agent_snapshot.py`. These readers still ask different questions
(owner binding, release authority, container lifetime, task claims, and slot
parity), but none may independently decide whether a serialized status is live.
An unknown status refuses by name instead of entering a denominator under a
reader-specific default.

### Measured reporting failures

Three corrected reports are relevant when diagnosing slot capacity, ownership,
or agent availability:

- The old disk-residue predicate reported zero quarantined slots while the live
  registry contained twenty-five rows whose status was `lease-quarantined`.
  Quarantine reporting now uses the registry status. The count names retained
  rows; it does not by itself prove that every row is safe to reclaim.
- A live agent snapshot contained eighteen named entries, including six with
  status `unreachable`. Counting every name as live produced eighteen agents;
  applying the shared status meaning produced twelve and changed the number of
  active slots without a live owner from forty-one to forty-three. The newly
  exposed names included `cloudlint` and `cloudrecover`, agents that had already
  departed. A named terminal entry is not evidence that an agent is available.
- Slot assignment previously selected the first matching historical or current
  registry row. For `hermit-001`, that could select released history instead of
  the active `flock2407` assignment. For `codex-lsm`, two current rows exist;
  assignment now refuses that ambiguity instead of guessing which row owns the
  slot.

These failures made quarantined, unowned, or ambiguously owned slots look
healthy. Treat an empty or green report as meaningful only when its population
and ambiguity checks are part of the reported contract.

Before moving, removing, or reclaiming any slot, prove from the running system
that no live agent process uses it. The presence or absence of a handoff is not
a liveness check: moving a live slot breaks the session's hook paths.

`scripts/slot-init.sh <slot> [product] [start-point]` is an unregistered manual
scaffolding fallback. It creates detached worktrees but does not update either
registry, so it is not the normal path for live agent work.

## Work in the slot

Confirm every assigned checkout is clean before editing. Fetch the intended
remote base without changing checked-out files, then create a task-specific
feature branch inside each product repository that will change. Leave
unchanged products detached and record their exact SHAs. Product source,
formatting, builds, tests, and commits all run inside the assigned nested
checkout; parent-only coordination changes remain in the parent repository
under explicit ownership.

Never share a writable path or branch. Research-only sharing and mutating
agents with disjoint path ownership must be explicitly recorded by the
allocator and in `worktrees/ACTIVE.md`.

## Close and release

Before release, record each product's branch, exact SHA, validation, and
publication disposition in `worktrees/ARCHIVED.md`. All assigned checkouts must
be clean. Remove a completed slot through the registry-aware release script:

```bash
scripts/release-worktree.rs --slot <slot> --clean
```

Omit `--clean` only when deliberately retaining a clean parked cache. The
release script updates both machine-local registries and never deletes feature
branches. Never release, remove, reset, clean, or reuse a slot containing work
that has not been handed off with a recovery SHA.
