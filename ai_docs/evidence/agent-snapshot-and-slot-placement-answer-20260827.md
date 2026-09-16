# The agent snapshot, and slot placement as a spawn parameter

Written 2026-08-27 by agent(hermit-136) for the coordinator to relay to the
owner. Every figure below was measured on devbig014 on 2026-08-27, not recalled.

---

## 1. What the agent snapshot is

**File:** `ignored/ci-hub/agent-snapshot.json`

**Contents:** a cache of **one ORC session's** fleet listing.

```json
{"schema_version": 1,
 "captured_at": 1787767983.04,
 "agents": [{"name": "hermit-104", "status": "busy", "tmux_pane_id": "%3",
             "current_task": "..."}, ...]}
```

Currently 14,391 bytes, 25 entries, `captured_at` 2026-08-26T18:13:03Z.

**Its writer:** `ci-hub/health/agent-container-reconcile-bridge.rs`, which writes
the file at `:203-204` via `persist_snapshot` (`:254`).

### Why it has been stale for over nine hours

**Because nothing runs the writer.** The only reference to that bridge anywhere
outside its own source is a *comment* in `ci-hub/tests/test_agent_podman.py:248`.
No tick-hub probe, no systemd timer, no script invokes it. It runs when a human
runs it.

It fell into a gap between scheduling regimes: its own header calls it
"deliberately temporary", handing off to the canonical workflow
`hermit-dev-operational-health-v1`, which was itself retired in favour of the
`hermit-health-tick.timer` systemd unit — and the bridge was never added to that
unit.

So **the 600s bound describes a refresh cadence that nothing was ever wired to
meet.** Measured age at the time of writing: **33,748s = 9.4h**. That is simply
time since the last hand-write.

Scheduling it would not be enough. A prior measurement (2026-08-26) found four
independent breaks, no two sharing a fix:

1. no scheduler (above);
2. `DEFAULT_STATE_URL` (`:50`) hardcoded to `127.0.0.1:44109`, where nothing
   listens — the live `orc` serves **38449**;
3. that live port answers `403 Forbidden: browser-markers-required`;
4. shape mismatch — the producer writes an object, `active-work` wants a bare
   `orc.listAgents()` array and rejects it with `agent-snapshot-is-not-a-list`.

And a names-only producer would be **worse than none**: every agent would get
`status=""`, which routes *every* owned task to `misrouted`. A fresh cache would
produce a flood of false misroutes where the stale one produces an honest
refusal.

### What goes unchecked while it is stale

**Only a report. Nothing goes unreclaimed.**

The tick runs (`ci-hub/health/tick-hub.yaml:723`):

```
./ci-hub/health/bounded_gate.py --name agent_container_lifecycle --timeout-secs 45 \
  -- ./scripts/agent-podman.rs reconcile --agents-json ignored/ci-hub/agent-snapshot.json
```

with **no `--apply`**, and `apply: false` maps to `"audit"` — the read-only mode
(`scripts/agent-podman.rs:320-321`).

`--apply` was withdrawn deliberately. `tick-hub.yaml:786-793` records why: the
reclaim predicate fell back to *"owner agent is absent from the ORC snapshot" →
`gracefully_remove()`*, so **a stale or empty snapshot would authorize destroying
a live agent's container**, and headless agents have no tmux pane for the
process-evidence path to rescue them. The audit half was kept; `--apply` is
withdrawn "until the predicate rests on process evidence alone."

Run by hand, it exits 1 with empty stdout and
`agent-podman: agent snapshot is stale: age=... max=600s` on stderr.

**And there is currently nothing to see: `podman ps -aq` returns 0 containers.**
The check is erroring about a cache while guarding a population of size zero.

> It is safe to leave this erroring. It is **not** safe to quiet it by widening
> the 600s bound (that hides all four breaks at once), by deleting the file (a
> stale *or empty* snapshot is what authorizes the destructive path), or by
> restoring `--apply`.

Its `error` state was also being announced with a title that asserted a finding
it had not made — "agent-owned containers need attention" was used for both
`action-required` and `error`. `tick-hub.yaml:807-820` records that fix.

**A correction to an earlier answer of mine:** I first said "nothing writes it."
There *is* a writer binary; it is unscheduled and broken, which is not the same
as absent. `ci-hub/health/operational_health.py:1392` says "HAS NO POSSIBLE
PRODUCER", which is that file's shorthand for "nothing can currently produce
usable content."

---

## 2. Slot allocation, as a spawn parameter

### The command

Run **from the parent repo root**, **by the agent that will own the slot**:

```bash
cd /home/newton/work/dev-hermit
scripts/allocate-worktree.rs \
  --agent hermit-139 \
  --task <task-id> \
  --product all \
  --purpose "<one line>"
```

`--agent` is the only required flag. `--product all` is the default and creates
all three product worktrees.

### The resulting path — this is the cwd to set at spawn time

`--slot` defaults to the agent name with a leading `hermit-` stripped, so
`--agent hermit-139` produces:

```
/home/newton/work/dev-hermit/worktrees/139
├── hermit/
├── reverie/
└── liteinst2/
```

**The spawn cwd is the slot root**, i.e. the literal template:

```
/home/newton/work/dev-hermit/worktrees/<agent-name-without-the-hermit-prefix>
```

That is what the one correctly-placed slot on the box records as its
`working_directory`, and it is what release checks.

### Three things that will bite, all verified

**(a) The slot root contains no `ci-hub/` and no `scripts/`.** A slot holds only
the product worktrees. So every dispatch line that says
`ci-hub/bin/tg-note-verified` or `scripts/allocate-worktree.rs` breaks by
relative path from inside a slot, and the agent's workaround is to `cd` back to
the parent — the exact behaviour being fixed. Give those as absolute parent
paths in the dispatch. (The allocator itself is safe to invoke from inside a
slot: `find_root` requires `.gitmodules` plus all three product dirs, and a slot
has no `.gitmodules`, so it walks up to the parent.)

**(b) Allocate before the spawn for the path, but the agent must re-run the
identical command as its first action.** A pre-spawn allocation *cannot record
ownership*. The allocator's own policy text: *"After an owner starts or is
replaced, re-run the same allocation to adopt its exact pane/cgroup lease;
release refuses legacy or unbound ownership."* Make that re-run a spawn
parameter too — it is the same class of instruction that failed as dispatch
prose. Since `454f8fe72` that re-run binds from the agent's own process
ancestry; a coordinator running it on the agent's behalf is **refused by
design**, because binding your own supervisor as someone else's owner would make
a live agent's slot read dead.

**(c) Two live agents already own a slot under a non-default name.**
`hermit-107` owns `lseekcmd`; `hermit-120` owns `slot120`. The rule is one slot
per agent, so a bare `--agent hermit-107` tries slot `107` and is **refused**.
Pass `--slot lseekcmd` / `--slot slot120` for those two.

### Slot names currently free

`139`, `140`, `141` are free. Every live agent's default slot name is either free
or already owned by that same agent — there are no collisions with the 33 empty
active rows, which live in a different namespace (`003`, `cloudlint`, `diverge`,
`rework-2280`, `slot74`).
