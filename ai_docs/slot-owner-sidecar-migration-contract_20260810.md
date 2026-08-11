# Slot-owner sidecar and legacy recovery contract (2026-08-10)

## Decision

Every new or re-adopted slot owner must carry a versioned `owner_sidecar` in
its `worktree-state.json` slot row. The allocator writes it under the same
exclusive registry lock as the ownership update. Keeping it in the guarded row
is intentional: a second file would create a crash window in which the registry
and its alleged owner generation disagree.

Schema 1 binds all of these values:

- slot, mutating agent, task, source, and recording timestamp;
- supervisor PID, host boot ID, and `/proc/<pid>/stat` start ticks;
- an exact generation derived from that three-part process identity;
- the owning cgroup and, for ORC owners, the tmux pane.

`scripts/slot-owner-authority.py --slot <slot> --json` is the only verifier for
this sidecar. It holds the registry lock while reading and returns:

- `dead`, rc 0, only for a boot-ID mismatch, absent PID, PID reuse, or an exact
  recorded process in a terminal kernel state **and** an empty/absent recorded
  owner cgroup;
- `alive`, rc 1, when PID, boot ID, and start ticks all match a running process;
- `indeterminate`, rc 2, for missing, malformed, unsupported, unreadable, or
  binding-mismatched evidence.

Only `dead` can participate in a recovery decision. `alive` and
`indeterminate` both refuse. Validate-lock remains a separate box-compute
authority and is never consulted for slot ownership.

The cgroup condition closes a process-namespace blind spot: a sandbox may not
resolve a host PID through its `/proc` even while the owner remains in the
recorded cgroup. Any cgroup member, including an unmapped member rendered as
PID `0`, makes the result `indeterminate`; unreadable cgroup state does too.

## Allocation and adoption

For an ORC owner, `allocate-worktree.rs` resolves the requested agent to one
live tmux pane, samples the pane supervisor start ticks before and after reading
its cgroup, reads the current boot ID, and publishes schema 1 atomically with
the pane/cgroup lease. A failed adoption removes any prior sidecar so an old
incarnation cannot survive as apparent authority.

For a Codex coordinator slot, the already-versioned systemd sentinel remains
the slot-level supervisor. The allocator first re-verifies its recorded PID and
start ticks, then adds the current boot ID and publishes the same schema-1 slot
sidecar. This sidecar proves the sentinel incarnation, not that an individual
logical Codex subagent is running.

An allocation performed before its future owner exists remains deliberately
unbound and therefore `indeterminate`. The living owner must re-run the exact
allocation command (adoption) before the row becomes provable. Inventing a
coordinator or allocator PID as a substitute would bind the wrong subject.

## Legacy cohort: 34 recoverable, 34 permanently unprovable from recorded evidence

The task-start measurement was 68 of 73 active rows without a complete
mutating-owner pane/cgroup lease. One concurrent allocation later changed the
live totals to 69 of 74; the analysis below preserves the original 68-row
denominator by excluding that later `codex-validatelock` row.

| Legacy class | Rows | Recoverable now | Contract |
|---|---:|---:|---|
| Exact-live coordinator sentinel (`main_pid@main_pid_starttime` matches `/proc`) | 33 | 33 | Re-run the exact sentinel allocation/adoption; verify first, then record boot ID and schema 1. |
| Exact live ORC pane but no recorded cgroup (`codex-timeouts`) | 1 | 1 | Re-run allocation from the live pane; capture the pane supervisor identity and cgroup. |
| No exact-live sentinel or pane identity | 34 | 0 | Do not synthesize a sidecar. These are permanently unprovable from the historical slot record and remain `indeterminate`; they must age out through the separately reviewed recovery mechanism below. |
| **Total** | **68** | **34** | **34/68 recoverable; 34/68 permanently unprovable from recorded evidence.** |

All 33 sentinel identities were re-read from `/proc` and matched their recorded
start ticks. The one ORC migration candidate was present in the fresh agent
snapshot with a live pane. These positive facts permit a present-tense rebind;
absence from either source was not interpreted as death.

## Recovery for the 34 unprovable rows

These rows cannot be migrated to schema 1 retroactively. Their missing process
identity is historical evidence that was never recorded, and a later PID,
agent-name census, idle clock, clean checkout, or empty process scan cannot
reconstruct it.

The only current non-destructive recovery is the coordinator protocol adopted
for legacy rows:

1. `agent_liveness_probe.py <recorded-owner> --json` must return rc 0
   `verified-dead`; rc 1 `alive` or rc 2 `unverifiable` refuses.
2. Independently prove all child repositories clean and detached as recorded,
   archive exact recovery SHAs/branches, and prove zero users through
   cwd/exe/root/fd/map-files/maps and cgroup inspection.
3. Confirm the row has no live `coordinator_lease` (non-clean release is
   refused for those rows).
4. A coordinator may run `release-worktree.rs --slot <exact-slot>` without
   `--clean`. This retains every file and marks the row released.

The non-clean release command merely warns about dirt and does not itself
enforce the zero-process census. Therefore the external proofs above are
mandatory. A row whose owner cannot be independently proven dead remains
active indefinitely unless a future owner returns and performs a live
adoption, or the human owner approves a new, separately reviewed mechanism.
Age alone is not such a mechanism.

## Required brackets

The verifier tests pin both sides and the failure boundary:

- exact live identity: `alive`, never `dead`;
- missing PID, reboot, or PID reuse: exact old incarnation `dead`;
- missing legacy sidecar, unsupported schema, binding mismatch, malformed
  process record, unreadable cgroup, or unreadable registry: `indeterminate`,
  never `dead`.
