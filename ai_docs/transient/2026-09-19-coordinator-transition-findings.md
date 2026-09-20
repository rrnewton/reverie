# Coordinator transition findings — 2026-09-19

Two recovery assumptions failed during the ORC-to-Codex coordinator handoff.

## Timeline archive was not current

The expected executable at
`/home/newton/agent_logs_archive/summary/hermit/timeline` did not exist. The
available backup at
`/home/newton/agent_logs_archive_bak/summary/hermit/timeline` ended on
2026-08-26, so it could not describe the preceding 24 hours. Recovery therefore
used the prior ORC workflow database, the preserved handoffs, TaskGraph notes,
and the exact Codex rollout files. A future extraction should publish the
current archive atomically and expose its newest source timestamp so a stale
but successful query cannot be mistaken for a current handoff.

## Starting below `worktrees/` did not change the inherited role

The new KVM and LiteInst Codex tabs started in
`/home/newton/work/dev-hermit/worktrees`, but both sessions still inherited the
parent instruction that describes the agent as the coordinator. The
coordinator explicitly corrected both sessions to long-horizon worker roles,
bounded their writable slots, and prohibited sibling-lane and owner-facing Chat
management. Future worker launch/adoption should use the owned product slot as
the process working directory and carry an explicit worker-role assertion;
directory placement under `worktrees/` alone is not a role boundary.

## Fleet authority had to move with the coordinator substrate

The durable health service had no `TG_DB_PATH`, and the live-agent census used
by slot parity could only discover an ORC pane. With ORC gone, the check
returned `state=unknown` and `active-work` could not establish any pull-request
claim. The transition now binds the ordinary health service through the local
`~/.config/dev-hermit/health.env` file. A separate one-minute producer reads a
private schema-v2 binding and brackets every refresh with two observations of
the adopted `agentctl` generation and native Codex goal. Between those reads it
checks the complete four-pane Herdr workspace (coordinator included) and reads
all asserted TaskGraph assignments plus the complete owned `IN_PROGRESS` set in
one read-only SQLite transaction. The artifact records that exact set and the
absolute database path; `active-work` requires them to match its current query
before it can reconcile claims. A
failed, partial, reassigned, or generation-changed refresh preserves the prior
artifact so it ages out; it never publishes a fresh incomplete fleet.

The first takeover census appeared to establish three claims, but that result
was unsafe: Mega had assigned an owned TaskGraph row to a nested native Codex
worker, `codex-main_health`, which has no independent Herdr pane or
`DG_AGENT_NAME` process identity. A four-pane census therefore cannot prove
that owner dead and must not call its work orphaned. The producer now reads all
owned `IN_PROGRESS` rows in the same SQLite transaction as its exact assignment
checks and refuses publication when any owner is outside the bound fleet.
`active-work` likewise reports claim state `unknown` when the complete snapshot
is missing, stale, or refused; it no longer turns a name-only process census
into claim-expiry authority. This deliberately keeps claims unknown while a
nested worker has no complete liveness sidecar instead of making live work look
free. The initially published incomplete artifact was retained for diagnosis as
`ignored/ci-hub/agent-snapshot.incomplete-owner-20260919-2314.json` and removed
from the authority path; a default `active-work --json` now returns
`state=unknown` with exit 2.

The coordinator-owned binding is
`$DEV_HERMIT_PARENT/.tick-hub/herdr-agent-bindings.json`; changing a lane name,
pane, cwd, adopted generation, native session/goal, TaskGraph owner, or current
task requires an explicit binding update.

## Chat intake needed a resource-bound fallback

The requested `vibetalk-hermit-014` space was created as
`spaces/AAQAWBA0g6A` and pinned to the exact coordinator pane. The Meta CLI
push-tail process repeatedly exhausted a 768 MiB/256-task service before it
could hold a usable stream. Isolating each ten-second REST poll avoided that
crash but wrote 691 MiB in 18 minutes, which is itself an operational leak.
Both unsafe deployments were stopped. The bridge remains configured while
`agentctl` gains a bounded long-poll interval; deployment must use Meta's
persistent worker and a cadence justified by measured bytes per day before the
user service is re-enabled. The failed stream observer record is retained as
`input.streaming-disabled-20260919-2228.json` in the private bridge state for
diagnosis.
