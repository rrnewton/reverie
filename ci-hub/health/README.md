# CI health object

Dev-hermit uses the pinned `agent-utils/tick-hub` engine for coordinator
health polling. Project policy stays in this repository:

- `ci-hub/health/tick-hub.yaml` defines checks, cadences, gates, and warning actions.
- `ci-hub/health/tick-hub-state.yaml` is the versioned runtime policy.
- `ci-hub/health/operational_health.py` implements project-specific probes.
- `ci-hub/bin/health-tick` uses `ci-hub/bin/agent-tool` to materialize the exact
  `agent-utils` gitlink on
  demand, provisions the pinned Python runtime dependency into the ignored
  `.tick-hub/venv` when needed, then invokes the real tick-hub CLI.
- `.orc/plugins/hermit-dev/index.ts` is the outer five-minute scheduler and
  wakeup delivery adapter.

## Timing-sensitive load precondition

`./ci-hub/bin/load-probe` takes a one-second host-counter sample and exits
zero only when the box is suitable for timing-sensitive work. The default
policy requires executing CPU at or below 50% and `MemAvailable` at or above
10%. Load average is diagnostic only: it includes uninterruptible sleep and is
never used for the verdict. Override thresholds explicitly when a measurement
has a stricter documented precondition.

The report includes measured executing/idle/iowait CPU, top CPU consumers,
`R/S/D/Z` process states, memory/PSI, and the numeric verdict. In a 3pai PID
namespace, aggregate counters remain host-wide but host PIDs are hidden. The
probe says this loudly, prints visible versus cgroup PID coverage, and ranks
cgroup CPU instead of mislabeling the sandbox-local process list as host-wide.
Exit 1 means measured conditions violate policy; exit 2 means required evidence
was unavailable. It reports its cost directly and avoids the `rust-script`
cache, so this is the canonical 3pai/BpfJailer entrypoint. The typed
`./ci-hub/ci-hub load-probe` alias is also available outside that restriction.

The hub checks GitHub current-main health and open-PR red counts every 15
minutes. Every five minutes it runs the same gentle primary-refresh routine as
`make checkout-fresh`. Clean product checkouts fast-forward to `main`; a
globally consistent Hermit/Reverie/LiteInst2 gitlink snapshot is committed and
pushed to parent `main`. A dirty checkout, detached or stale branch,
Hermit/Reverie pin mismatch, or parent-main race is preserved and hard-warned
instead. Green ticks stay quiet; an `ACTION:` or `ERROR:` wakes the coordinator
with a `HARD WARNING`.

### Scope: what the hub may and may not watch

Owner directive, 2026-08-08: *"tick hub should not be used for interfering with
these orc internal matters"*, and *"I never was involved with ANY of those tick
hub creations ... They are trying to rigidly remind the agent of things it may
not notice. This is good for external systems like github but it seems dangerous
in other cases."*

The admission test for any reminder is **can ORC observe this directly?** If yes,
it does not belong here: ORC is the authority on agents, panes, tasks and
ownership, so a second observer of the same fact cannot be more right, only
differently wrong — and its disagreement arrives as an instruction to a
coordinator mandated to close and respawn agents without asking. If no — GitHub,
GH Actions queues, runner lanes, merge gates, local disk, local git — reminding
the coordinator is the entire point.

Weigh a proposal by what its alarm **causes**, not only by whether it is
accurate. A false alarm about GitHub costs a wasted check; a false alarm about
ORC-internal state costs an agent.

Four reminders were removed on 2026-08-08 under that test: `stuck_agents`,
`active_work_reconciliation`, `unowned_high_priority_backlog`, and
`unowned_residue`. `agent_container_lifecycle` was kept but downgraded to
report-only. `worktree_new_dead_owner` was replaced by `worktree_disk_residue`
(below). The full removal record is inline in `tick-hub.yaml`.

`./ci-hub/ci-hub active-work`, `residue_sweep.py`, `unowned_backlog.py`, and
`operational_health.py agents` all still work and are still useful **on
request**, when a coordinator or agent is actually asking that question. What
was withdrawn is their standing authority to interrupt and instruct.

### Worktree disk residue

`slot_disk_residue.py` keeps the one genuine, non-ORC need that the removed
liveness gate was carrying: huge slots accumulate under `worktrees/` long after
anything was working in them, and nothing reclaims them. Its predecessor asked
*"is the owning agent dead?"* and, on 2026-08-08, answered wrongly for the agent
holding the freshest activity timestamp on the box.

The replacement asks only the disk question — **on disk, consuming space, and no
live process inside** — and reads no agent name, no ORC fleet, no TaskGraph
owner, and not even the `worktree-state.json` status field. Occupancy comes from
`/proc/<pid>/cwd`, which cannot go stale and cannot be forged by editing a file.
Idleness must be sustained (default 24h) and size material (default 1 GiB), so
the ~0.6% unreadable-cwd blind spot cannot survive to a finding. Sizes are
actual disk blocks and are reported as an upper bound, because reflink-seeded
slots share extents and removing one frees less than its measured size.

It pages on the **delta** — a slot that newly qualifies — and carries the
standing backlog in captured fields, so it cannot become the permanently-on gate
that gets muted within a day. It is DETECT ONLY: reclaim needs a recovery SHA
and remains a coordinator decision.

## What this hub structurally cannot see: its own host dying

Everything above runs inside ORC, and `.orc/plugins/hermit-dev/index.ts` is its
outer scheduler. On 2026-08-06 the ORC process and its tmux server died
together, so the scheduler, the tick, and every gate above died in the same
event and reported nothing. A human noticed, after a long silent interval.

Two watchdogs therefore live outside ORC, on independent `systemd --user`
timers. They answer different questions and must not be confused:

- `hermit-health-staleness.timer` asks **"is the tick still firing?"**, purely
  by the mtime of the heartbeat and `.tick-hub/fired-state`. It would read a
  dead ORC as perfectly healthy for as long as those files stay fresh.
- `hermit-orc-liveness.timer` asks **"is ORC itself alive?"**, by cross-checking
  active session rows in `~/.orc/index.db` against `/proc`
  (`scripts/orc-liveness-watchdog.rs`, unit sources under `scripts/systemd/`).
  It runs every two minutes and shares no code, state file, or timer with the
  tick path.

The liveness watchdog trusts neither of the two available proxies.
`sessions.status = 'active'` is a label ORC wrote, not a fact about now — a
SIGKILL leaves it reading `active` forever, which is exactly the case that must
be caught — so it is treated as a claim to dereference. And "the PID exists" is
a proxy for "ORC is running", because PIDs recycle, so the check reads
`/proc/<pid>/cmdline` and compares argv[0]'s basename; a live PID belonging to a
different program is reported as `DEAD_PID_RECYCLED`, never folded into `LIVE`
and never folded into `DEAD_STALE_PID`.

A missing or unreadable database reports `UNKNOWN_*` and alerts rather than
degrading to silence: absence of evidence is not evidence of health. Alerting is
durable-log-first and never claims a delivery it did not make, since the alarm
case is by definition "ORC is not running" and pushing it through an ORC tmux
pane would be self-defeating. Auto-restart is off by default — relaunching Orc
is owner-only while the running process injects a Codex sandbox flag that
post-1.0 Codex rejects, so restarting would faithfully reproduce a coordinator
that cannot spawn.

Read the verdict without running anything:

```bash
cat ~/.local/state/hermit-orc-liveness.status      # latest verdict, one line
tail ~/.local/state/hermit-orc-liveness.jsonl      # durable per-check records
./scripts/orc-liveness-watchdog.rs --dry-run       # check now, write nothing
rust-script --test scripts/orc-liveness-watchdog.rs
```

Hub cadence data lives in the gitignored `.tick-hub/fired-state`. Health and PR
probes are read-only. The primary-snapshot reminder is intentionally mutating,
but only through clean fast-forwards and a path-limited parent gitlink commit;
it has no reset, clean, force-checkout, or force-push path.

Run a dry tick from the repository root:

```bash
HERMIT_AGENT_SNAPSHOT_JSON='[]' ./ci-hub/bin/health-tick --no-header
```

Run and persist due times:

```bash
HERMIT_AGENT_SNAPSHOT_JSON='[]' ./ci-hub/bin/health-tick --flush --no-header
```

`agent-utils` uses `update = none`, so ordinary recursive submodule checkout
does not fetch it. The runner checks out the exact pinned commit on first use
and fails rather than overwriting a dirty checkout.

## Gate-runner presence (`gate_runner_presence.py`) — certification condition 9

On 2026-08-08 the `hermit-gate-newton` installation and credentials were
deleted. The user service stayed **enabled** and failed 203/EXEC for roughly two
days without reporting the deletion, and the missing capacity presented as CI
saturation: gates parked with every leg finished, Merge Gate repeatedly the
worst workflow. The runner was later restored and executed gate jobs again —
**but recovery is not a detector.** Restoring capacity is not the same as being
able to see capacity disappear.

Four surfaces are checked **separately**, because the incident is invisible to
any one of them:

| surface | confirmed by | why it is separate |
| --- | --- | --- |
| `installation` | `run.sh`, `config.sh`, `bin/`, `externals/` **and** the unit's ExecStart target being executable | `_work` survived the deletion, so "the directory exists" is not installation |
| `credentials` | `.credentials`, `.credentials_rsaparams`, `.runner` present, non-empty, parseable | `.runner` is UTF-8 **with BOM**; a plain `utf-8` load raises and reads as corruption |
| `service` | enabled **and** active **and** not failed **and** last exec status 0 | it was `enabled` the entire two days — enabled is necessary and never sufficient |
| `registration` | the `agentId` in `.runner` is still registered with the repo, and online | local health says nothing about whether GitHub will route work here |

**Absence is `NO_RESULT`, never silence.** Any surface that is absent, degraded,
or simply could not be evaluated (no network, no `gh`, `--offline`) makes the
aggregate `NO_RESULT` and exits `2`, so an admission caller fails closed rather
than proceeding on an unconfirmed runner.

```bash
# observe (exit 0 = all four confirmed, 2 = NO_RESULT)
python3 ci-hub/health/gate_runner_presence.py --json

# record on a cadence, then fail closed at admission
python3 ci-hub/health/gate_runner_presence.py --record .tick-hub/gate-runner-presence.json
python3 ci-hub/health/gate_runner_presence.py --admission-check \
    --record .tick-hub/gate-runner-presence.json --max-age 900
```

A recorded `PRESENT` older than `--max-age` is itself `NO_RESULT`: a monitor
whose last answer is arbitrarily old cannot discharge "within a defined
cadence".

### Cadence wiring — APPLIED 2026-08-10

Wired into `ci-hub/health/tick-hub.yaml` at a 900s cadence, fail-visible
(`when: failure`; the tool exits 2 on NO_RESULT).

Two corrections to what was parked here, recorded because both would have bitten
whoever pasted it:

* **The block was in the wrong schema.** It used `every_min:` and `check:`;
  `tick-hub.yaml` uses `cadence_secs:` and `gate:`/`emit:`, and `every_min`
  appears nowhere in the file. Pasted verbatim it would not have scheduled
  anything.
* **The file was not held by another agent.** The 79 "uncommitted lines" were an
  already-landed commit showing as dirt because the local parent checkout was 8
  commits behind `origin/main`. `git diff origin/main -- <file>` was empty the
  whole time. Declining to edit was still the right call on the evidence
  available — the fix is to ask "is this dirty?" against freshly fetched
  `origin/main` rather than a stale local `HEAD`.

The structural problem the original note identified is real and unfixed:
`ci-hub/ci-hub.rs` loads exactly one tick config with no drop-in directory, so
every detector serialises through one file. See the drop-in proposal in
`ai_docs/2026-08-10-tick-hub-dropin-proposal.md`.

### Brackets

`tests/test_gate_runner_presence.sh` brackets **by deletion, not by
inspection**: it builds a scratch installation and a scratch user unit under a
probe root, measures the untouched hierarchy first, then deletes the
installation while leaving the unit enabled — reproducing `UnitFileState=enabled`
with `ExecMainStatus=203`. The live `hermit-gate-runner.service` is never
touched. 14 assertions, both directions, plus the cadence cases.
