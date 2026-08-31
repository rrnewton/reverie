# Worktree Slot Protocol

`ci-hub/bin/wrkslots` is the only interface for creating, registering,
recovering, or removing a Git worktree. Live agent slots and disposable
validation slots have separate roots:

```text
worktrees/slots/<slot>/       live agent checkout
worktrees/validate/<slot>/    disposable validation checkout
```

Temporary validation checkouts live under `worktrees/validate/`. Per-run Cargo
homes live under `ignored/validate/cargo-homes/`, alongside validation logs,
run records, and any checkout-local receipt recovered before cleanup. The
temporary checkout and Cargo home are removed when the run finishes. Use
`ci-hub validate-run --sweep-completed` to retry cleanup left by an interrupted
waiter.

Never develop in the shared primary checkout, and never invoke raw `git
worktree add`, `git worktree remove`, `scripts/allocate-worktree.rs`,
`scripts/release-worktree.rs`, or `scripts/slot-init.sh` to change live state.
Those older entrypoints refuse and direct callers here. Read `AGENTS.md` before
changing slot state.

## Allocate a slot

Agents ask the coordinator for a path. The coordinator creates the slot from
the primary repository root:

```bash
ci-hub/bin/wrkslots --allow-existing-unregistered-worktrees \
  create hermit-api \
  --slot-type agent --coordinator-authorized \
  --agent hermit-api --task impl-example \
  --purpose "Implement the example change" \
  --repo checkout=. --branch checkout=impl-example \
  --coordinator-pid "$COORDINATOR_PID" --owner-pid "$OWNER_PID"
```

The required flag is a reminder and recorded provenance, not a claimed
permission boundary between same-user processes. A delegated subagent uses the
same command only after its coordinator supplied the assignment. Omitting the
flag refuses before creating a branch, directory, or lifecycle record and says
to ask the coordinator.

This repository still has worktrees created before wrkslots. During that
migration, commands operating on a registered slot pass
`--allow-existing-unregistered-worktrees` as a global flag. It retains and
warns about every unregistered directory; it never treats one as free or
authorizes its removal. Omit the flag when auditing whether migration is
complete—the strict refusal then names the first remaining directory.

The wrapper resolves the primary checkout from Git's common directory. This
keeps one append-only history even when the command is launched from a linked
slot. Pass `--project-root PATH` only for a deliberately separate project or a
test fixture.

Before moving, removing, or reclaiming any slot, prove from the running system
that no live agent process uses it. The presence or absence of a handoff is not
a liveness check: moving a live slot breaks the session's hook paths.

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

The owner renews its time-to-live while active:

```bash
ci-hub/bin/wrkslots heartbeat <slot> --agent <agent> \
  --owner-pid "$OWNER_PID" --expected-generation 1
```

Any later participant may reclaim after the exact owner is proven dead and the
time-to-live has expired. `remove` first publishes unpushed commits and tracked,
untracked, and ignored files outside configured cache paths, then verifies the
remote receipt before deletion:

```bash
ci-hub/bin/wrkslots --allow-existing-unregistered-worktrees \
  remove <slot> \
  --coordinator-pid "$CURRENT_COORDINATOR_PID" --expected-generation 1
```

If an untracked `HANDOFF.md` exists, first run `wrkslots read-handoff`; unread
or changed contents block removal. If process or registered-liveness evidence
is unavailable, removal refuses instead of treating the slot as free. A
validate slot may use `remove --validate-complete` after its external logs and
run record are durable; it never runs the agent-work salvage path.

Every mutating step first appends the slot transition or operation progress to
`worktrees/EVENTS.<machine>/`. `ACTIVE.*`, `ARCHIVED.*`, holds, and journals are
compatibility views. If a participant exits mid-operation, preserve the paths
and run:

```bash
ci-hub/bin/wrkslots --allow-existing-unregistered-worktrees \
  recover --coordinator-pid "$CURRENT_COORDINATOR_PID"
```

The recovery record contains what a later participant needs; it does not
require the original coordinator to return.
