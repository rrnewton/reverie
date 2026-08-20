# The slot-owner authority can report `alive`; nothing on this host can produce one

**Investigated:** 2026-08-20, ~18:35Z–19:00Z, on `devbig030`, read-only.
**By:** hermit-902 (claude, opus-5), task `sidecar-authority-cannot-report-alive`.
**Status: research and proposal. Nothing implemented. Nothing reclaimed.**

## Correcting the framing this task was opened on

The task was opened on my sentence *"a liveness authority that cannot report liveness."*
That is wrong about the code and right about the situation, and the difference relocates
the defect from the reader to the writer.

`scripts/slot-owner-authority.py::assess_state` ends in
`verdict("alive", "owner-process-alive", …)` after checking, in order: sidecar valid →
current `boot_id` readable → `boot_id` matches → `/proc/<pid>/stat` readable →
`start_ticks` matches → process not in `{Z, X}`. `alive` is reachable.

It is unreachable **on this host** because no registry row carries the evidence. Every one
of the 24 sidecars records `boot_id=3ac87f92-…` (the pre-reboot boot); the three slots
allocated today for live agents carry no sidecar at all.

## The three states, exhaustively

| state | reason codes |
| --- | --- |
| `alive` | `owner-process-alive` |
| `dead` | `owner-boot-id-mismatch`, `owner-pid-absent`, `owner-pid-reused`, `owner-process-exited` — each routed through `dead_if_owner_domain_empty`, which downgrades to indeterminate when the recorded cgroup is non-empty or unreadable |
| `indeterminate` | `slot-missing`, `sidecar-invalid`, `registry-unreadable`, `boot-id-unreadable`, `owner-cgroup-unreadable`, `owner-cgroup-nonempty`, `owner-process-unreadable` |

`Verdict.exit_code`: `dead`→0, `alive`→1, everything else→2. `may_recover` is
`state == "dead"` alone.

## The consumers, and what each does with each state

**`scripts/release-worktree.rs`, and only under `--stale-clear`** (line 5569:
`let stale_clear_authority = if stale_clear { Some(owner_authority(…)) } else { None }`).
Plain `--clean`, `--force`, `--request-release` and
`--coordinator-finalize-orphan-residue` never consult it.

In `prepare_stale_clear` (5353):

- `indeterminate` → hard `Err`, *"stale-clear is fail-closed"*.
- `dead` → proceeds to the automatic-handoff checks.
- `alive` → `Err`, **unless** `lease.source == "codex-systemd-sentinel-v1"`, in which case
  the check at 5361 is skipped and an expired lease suffices. **This is the only place in
  the tree where "not dead" does not refuse.** It reads as deliberate — systemd is that
  sentinel's own authority — but it should be an explicit decision on the record rather
  than something a reader discovers.

**`ci-hub/health/residue_sweep.py:340`** (`--all`), feeding two predicates consumed by
`ci-hub/health/slot_disk_residue.py:620–634`:

- `guarded_dead_owner_slots` — only `dead` produces a row, and `if slot in occupied_slots:
  continue` drops any slot with a live `/proc` user. `indeterminate` contributes nothing in
  either direction.
- `guarded_live_owner_slots` — **only `alive` protects.** This is the sole place an `alive`
  verdict is load-bearing:

  ```python
  for result in results:
      if result.slot in live_owner_slots:
          result.reclaimable = False
          result.reason = ("slot-owner-sidecar registered owner is alive; "
                           "idle time cannot qualify this slot")
  ```

  With zero `alive` verdicts this loop is a **no-op**. The tick reports the fact itself:
  `guarded_live_owner_slots=0`.

## By design, not by luck — checked adversarially

The combination the task feared — a detector that cannot say `alive` plus a reclaim path
that treats not-alive as reclaimable — **is not present**. Every path that can remove a
slot refuses these slots *before* reaching any occupancy check:

`recorded_owner_authorities` (1080) iterates `slot_state["agents"]` and requires **both**
`tmux_pane_id` and `cgroup_path` on every agent, erroring otherwise with

> `recorded owner '<name>' lacks tmux_pane_id lease data; re-run allocate-worktree.rs for this owner while its exact pane is live`

and it is called at 5762 inside `if clean { … }` as
`.unwrap_or_else(|error| die(&format!("owner lease refused cleanup: {error}")))`.
**`--force` does not bypass it** — the gate is on `clean`, removal only happens under
`clean`, and `--force` sits downstream. Behind it, `slot_lease_view` needs
`owner_sidecar.lease`, so a sidecar-less slot cannot reach the stale-clear gate at all.
`live_process_users` plus the post-fence recheck is a fourth, occupancy-based layer.

Measured — all three of today's live-agent slots:

```
pr2263-901       agent=hermit-901-pr2263   read_only=False  tmux_pane_id=None  cgroup_path=None  sidecar=NO
gchat-crash-902  agent=hermit-902          read_only=False  tmux_pane_id=None  cgroup_path=None  sidecar=NO
cpuid-recheck    agent=hermit-coord-cpuid  read_only=False  tmux_pane_id=None  cgroup_path=None  sidecar=NO
```

against the pre-reboot rows that do have it (`coord` `%76`, `pollrevents` `%71`,
`revents-measure` `%78`). None of the three can be deleted today by any path.

### Two designed protections are nonetheless inert, and neither fails loudly

1. `guarded_live_owner_slots` needs `alive`, so it protects nothing (`=0`).
2. `live_process_users` has a name-based owner check — `process_agent_name()` reads
   `DG_AGENT_NAME` from `/proc/<pid>/environ` and refuses if it matches a recorded owner
   name. Census of own-uid processes:

   ```
   DG_AGENT_NAME=hermit-901-pr2263   -> 0 processes
   DG_AGENT_NAME=hermit-coord-cpuid  -> 0 processes
   DG_AGENT_NAME=hermit-902          -> 11 processes
   ```

   The live agents advertise the ORC **pane** names (`hermit-901` ×3, `hermit-coord` ×5,
   `hermit-cortex` ×7) while the registry records **task-suffixed owner** names. Two of
   three do not match. `gchat-crash-902` matches only because my pane name and my registry
   owner name happen to be the same string — that one *is* luck, and it expires the next
   time someone names a slot owner after its task.

## What actually breaks: a wrong list, not a lost slot

Nothing holds these three slots off the reclaim candidate list except the clock and the
size floor — `DEFAULT_IDLE_HOURS = 24.0`, `DEFAULT_MIN_GIB = 1.0`. All three have
`idle_since = 2026-08-20T18:13:43Z` and contain a hermit checkout, and the clock only
resets when the six-signal scan sees a process **inside** the slot — which never happens
while these agents work from the parent.

**So at 2026-08-21T18:13:43Z the tick will name three live agents' slots reclaimable**,
with the reason `idle 24.0h, N GiB on disk, no process inside` — true and misleading in the
same sentence — and every release attempt will then refuse with
`owner lease refused cleanup: … lacks tmux_pane_id lease data`, which reads like a tooling
bug rather than a safety verdict.

That is the harm: a standing list of slots simultaneously advertised as reclaimable and
impossible to reclaim. It is the exact input that teaches an operator that refusals are
noise to work around, on a box that has just spent a day acting on an alarm that looked
real. A detector that cries wolf at a live agent's slot is not safe merely because the door
happens to be locked.

One root cause, three consequences: pane/cgroup lease absent → **cannot be released**
(good) **and cannot be assessed** (bad) **and cannot be protected from the idle clock**
(bad).

## Root cause

`scripts/allocate-worktree.rs:2509`:

```rust
if !read_mostly && !codex_systemd_sentinel {
    match observed_owner_lease.as_ref().expect("non-Codex allocation observes owner") {
        Ok(lease) => entry["owner_sidecar"] = observed_owner_sidecar(&slot, &agent, bound_task, &now, lease)…,
        Err(_)    => entry.as_object_mut()….remove("owner_sidecar"),   // failed adoption invalidates the old incarnation
    }
}
```

Owner-lease observation is failing for ORC-spawned agents on this host, so allocation takes
the `Err` branch: no sidecar, and no `tmux_pane_id`/`cgroup_path` on the agent entry.
Everything above follows from that one branch. **Why observation fails is not established
here** — that is the next question and this task was scoped to research, so I did not guess
at it.

## Proposed repair — not implemented

**1. Fix the writer. This is the only one that matters; the rest is mitigation.**
Diagnose why `observed_owner_lease` returns `Err` for ORC-spawned agents and repair it, so
current agents get a sidecar and pane/cgroup lease data again. That single fix restores
`alive`, re-arms `guarded_live_owner_slots`, makes these slots releasable through the
normal path, and removes them from the candidate list — all four, at once.

**2. Make the gap loud instead of silent.** `slot_disk_residue` already emits
`guarded_live_owner_slots`. Add a *finding* (not just a field) when a slot is
`status=active`, has a mutating owner, and has neither a sidecar nor pane/cgroup lease
data — "N active slot(s) cannot be assessed and cannot be released". Today that condition
is a silence, and silence has already been mistaken for health on this box once this week.

**3. Do not let an unassessable slot become a reclaim candidate.** Treat "active +
mutating owner + no owner lease data" exactly as `live_owner_slots` treats `alive`:
`reclaimable = False`, reason *"owner cannot be assessed; idle time cannot qualify this
slot"*. This only ever removes slots from the candidate list and never adds one, so it
cannot create a hazard — consistent with the tree's own stated rule that the mirrored
predicate is *"deliberately allowed to be STRICTER-or-equal and never looser"*.

**4. Decide the codex-sentinel `alive` asymmetry on the record** — the one branch where a
non-`dead` verdict does not refuse. Either document why an expired systemd lease outranks a
live-process verdict, or make it refuse like the others.

**5. Align the owner-name spellings, or stop depending on them.** The registry records
task-suffixed owner names while processes advertise pane names, which silently disables the
`DG_AGENT_NAME` check in `live_process_users`. Either bind them at allocation time or drop
the name check and rely on the identity evidence, which is what the rest of this mechanism
already does.

**Explicitly not proposed:** relaxing any refusal. `indeterminate` must keep refusing. The
fix is to stop producing `indeterminate`, never to start permitting it.

## While this is open

`pollrevents`, `revents-measure` and `slot10` remain genuinely orphaned, and `slot10` holds
the Reverie pin bump on `fix/bump-reverie-pin-ee6716a`. Reclaim nothing.
