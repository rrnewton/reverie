# CI health object

Health reports observe mechanisms; they do not select validation authority.
Follow skill `hermit-validation-authority` for CI mode, evidence qualification, and the
local-versus-GitHub boundary.

Dev-hermit uses the pinned `agent-utils/tick-hub` engine for coordinator
health polling. Project policy stays in this repository:

- `ci-hub/health/tick-hub.yaml` is versioned policy: it defines checks,
  cadences, gates, and warning actions.
- `.ops-state.yaml` is gitignored per-host runtime state consumed by both the
  tick engine and project health checks. `ops/ops-state.example.yaml` is its
  versioned schema/example; copy it rather than committing the live state.
- `ci-hub/health/operational_health.py` implements project-specific probes.
- `ci-hub/bin/health-tick` uses `ci-hub/bin/agent-tool` to run the tick-hub
  implementation pinned by Hermit's nested `agent-utils` gitlink. The expected
  pin comes from `DEV_HERMIT_TOOL_ROOT`'s exact `HEAD` when a current tooling
  checkout is supplied; otherwise the adapter freshly fetches parent
  `origin/main` into a process-private ref and resolves one exact commit. It
  keeps that ref alive through both nested gitlink reads and compare-deletes it
  on normal exit or a catchable signal; it never changes `FETCH_HEAD`, the
  shared `origin/main` tracking ref, or a ref whose object identity changed.
  It never treats a stale dirty state-root `HEAD` as pin authority. Normally the
  materialized checkout is at that exact commit. If a landing has moved the
  shared Hermit checkout first, the reporter may continue only when its complete
  tracked Python entrypoint and package are byte-identical to the selected pin;
  any tick-hub difference still refuses. `DEV_HERMIT_PARENT` may name the dirty
  primary that owns the materialized Hermit/agent-utils checkouts. The wrapper
  provisions the pinned Python runtime dependency into the ignored
  `.tick-hub/venv` when needed, then invokes the real tick-hub CLI.
- `scripts/systemd/hermit-health-tick.timer` is the sole thirty-minute
  scheduler, so the tick survives coordinator-session loss.
- `.orc/plugins/hermit-dev/index.ts` retires the superseded ORC health workflow
  at startup and fails startup reconciliation if it remains registered. The
  wrapper lock serializes a bounded manual invocation or service restart
  against the scheduled run before either touches shared reports or gate state.

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

The outer hub runs every thirty minutes; the GitHub current-main health and
open-PR red-count reminders remain hourly and therefore run every other tick.
Before it loads the versioned tick runner or configuration, the durable wrapper
asks the current immutable tool root's `scripts/parent-main-write sync
--preserve-dirty` to fast-forward the shared parent checkout. A direct/manual
wrapper run falls back to the writer in the shared parent. It advances only when
local `main` is an ancestor of
freshly fetched `origin/main` and every incoming path is disjoint from every
tracked, staged, untracked, ignored, or dirty-submodule path, except for a
gitlink whose nested checkout already names the fetched target and whose index
still names either endpoint. The fast-forward changes that superproject pointer
without touching the nested checkout. Unrelated work remains in place; every
other overlap and divergence is preserved and reported with the blocking paths
and the before and after SHAs. It never adopts, stashes, resets, reconciles, or
creates a merge commit. This ordering makes a landed ci-hub change available to
the rest of the same tick.

The tick then runs the same gentle primary-refresh routine as `make
checkout-fresh`. Clean product checkouts fast-forward to `main`; a globally
consistent Hermit/Reverie/LiteInst2 gitlink snapshot is committed and pushed to
parent `main`. A dirty checkout, detached or stale branch, Hermit/Reverie pin
mismatch, or parent-main race is preserved and hard-warned instead. Green ticks
stay quiet; an `ACTION:` or `ERROR:` wakes the coordinator with a `HARD WARNING`.

A durable user-systemd timer publishes disk evidence every fifteen minutes,
independently of an ORC session. `hermit-disk-snapshot.service` runs one current,
immutable `scripts/publish-disk-snapshots.py` and serializes both publications
under one lock. It writes the fresh whole-filesystem Btrfs allocator reading to
`ignored/ops-state/disk.json`, writes bounded `compsize -b -x` readings for the
main top-level populations to `ignored/ops-state/disk-paths.json`, and retains
the preceding published path run at
`ignored/ops-state/disk-paths.previous.json`. The path scan runs at idle I/O and
lowest normal CPU priority, with a 180-second bound per concurrently measured
population. `TimeoutStartSec=540` is the fail-closed bound on the complete
service path, including immutable-tool setup and both publications; no measured
cold-run component is treated as a code-enforced sub-bound. The service also
has explicit memory, swap, and task caps. Both independent publications are
attempted serially under the shared lock, and any command failure fails the
oneshot without suppressing the other source of disk evidence.

For a cold or repair installation, first run
`scripts/install-disk-snapshot.sh --no-start`. It checks producer dependencies,
installs the exact bootstrap and GitHub helper, and leaves the disk timer both
disabled and inactive; it refuses if an existing timer or service is active.
Then run
`scripts/install-disk-snapshot.sh --check` to bind the installed bytes back to
the reviewed source. On a host with a live transitional ORC, load or restart it
and require
`orc.hermitDevStatus().workflowRetirementAtStartup.pass === true`; that receipt
is a pre/post-registry observation that all replaced workflows are absent. On a
host where ORC has already been removed, record the bounded process and service
census proving that no ORC scheduler remains. Only then use the explicit
`scripts/install-disk-snapshot.sh --start`, so old and new producers never write
the same artifacts concurrently.

The five populations are registered slots, disposable validation checkouts,
the shared Hermit/Reverie/LiteInst2 checkouts, the parent Git object database,
and the home cache directories. They are deliberately a bounded sample rather
than a whole-home walk. A whole-filesystem delta that these rows do not explain
is evidence that the change occurred outside the sampled paths.

Every path row says `complete`, `partial`, or `unavailable` and retains its
diagnostics. A number from a partial row describes only what `compsize` could
read; it must not be presented as the whole population. The file also brackets
the scans with fresh `btrfs filesystem usage -b` free-space readings. Separate
path values are deliberately marked non-additive because reflink-shared extents
can appear in more than one population. Compare current and previous deltas to
the bracketed whole-filesystem change; any unexplained difference remains a
measured gap rather than being assigned to a path by inference.

### Scope: what the hub may and may not watch

Owner directive, 2026-08-08: *"tick hub should not be used for interfering with
these orc internal matters"*, and *"I never was involved with ANY of those tick
hub creations ... They are trying to rigidly remind the agent of things it may
not notice. This is good for external systems like github but it seems dangerous
in other cases."*

The owner directive used ORC as the coordinator substrate then in service. The
current admission test is **can the coordinator's primary authority observe
this directly?** If yes, it does not belong here: Herdr plus the exact
TaskGraph-bound agent snapshot is the authority on agents, panes, tasks and
ownership, so a second observer of the same fact cannot be more right, only
differently wrong. If no — GitHub, GH Actions queues, runner lanes, merge gates,
local disk, local git — reminding the coordinator is the entire point.

Weigh a proposal by what its alarm **causes**, not only by whether it is
accurate. A false alarm about GitHub costs a wasted check; a false alarm about
coordinator-internal state costs an agent.

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

`slot_disk_residue.py` keeps the one genuine need outside the coordinator
census that the removed liveness gate was carrying: huge slots accumulate under
`worktrees/` long after anything was working in them, and nothing reclaims
them. Its predecessor asked
*"is the owning agent dead?"* and, on 2026-08-08, answered wrongly for the agent
holding the freshest activity timestamp on the box.

The replacement asks only the disk question — **on disk, consuming space, and no
live process inside** — and reads no agent name, no Herdr fleet, no TaskGraph
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

## Independent host and coordinator liveness

The health tick and its gates now run from a `systemd --user` timer, outside
ORC. This replaces the historical arrangement in which ORC scheduled the tick:
on 2026-08-06 the ORC process and its tmux server died together, so the old
scheduler and every gate died silently until a human noticed.

Two additional `systemd --user` watchdog timers answer different questions and
must not be confused:

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
./ci-hub/bin/health-tick --no-header
```

Run and persist due times:

```bash
./ci-hub/bin/health-tick --flush --no-header
```

The scheduled systemd path reads the independently published typed Herdr
snapshot; it does not inject ambient `HERMIT_AGENT_SNAPSHOT_JSON`. Manual runs
must not invent that compatibility input. An empty array is no observation and
is refused before it can replace the last usable agent snapshot.

The parent `agent-utils` path is a symlink to `hermit/agent-utils`. A recursive
checkout initializes that nested submodule. For a selective checkout, initialize
Hermit first and then run:

```bash
git -C hermit submodule update --init --checkout -- agent-utils
```

The runner checks out the exact nested pin from Hermit on first use and fails
rather than overwriting a dirty checkout.

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

## Emitting an approval that the gate actually reads (`approval_binding.py`)

⚠️ **`gh pr review --approve` is not the mechanism, and its refusal settles
nothing.** It rejects self-approval with `Can not approve your own pull request`,
which has been reported as "no agent on this fork can supply an approval". That
is true of GitHub's native review state and false of the thing that decides
landing: `approval_binding.py` never consults native review state. It derives
authority from a line in a comment body, and `passed-review-*` is only a cache
pointing at that line.

**The one authority-bearing shape.** Anchored, whole-line, case-insensitive
(`approval_binding.py`):

```text
APPROVED-AT: claude 45a96114aa8ce603f2d9d39d1ae3f7c92d1f8267
```

Each of these is independently fatal if wrong: the **colon**; an **explicit
lane**; the **full 40-hex** sha (a 12-char prefix does not match); and the line
being the **entire line** — trailing prose on it kills the binding.

Valid lanes are exactly `claude` and `codex` (`LANES`). **The role tag does not
encode the lane** — a codex review may be role-tagged with a model name — so the
lane is what you type, not who you are.

**Whole-line markdown is tolerated; inline is not.** `undecorate()` strips
`` ` ``, `**`, `__`, `*`, `_` only when a wrapper encloses the complete line:

| form | result |
| --- | --- |
| `**APPROVED-AT: claude <sha>**` | binds |
| ``` `APPROVED-AT: claude <sha>` ``` | binds |
| ``APPROVED-AT: claude `<sha>` `` | **malformed — lane reads UNDETERMINED** |

That third form is not an error you will see; it is an absence.

**State, and only the newest binding counts.** No binding for a lane →
`UNDETERMINED`; the lane's **newest** binding equal to the current head →
`BOUND_CURRENT`; otherwise `SUPERSEDED`. ⚠️ Re-posting an older line to
"reconfirm" therefore **demotes your own lane**. A rejection
(`CHANGES-REQUESTED-AT: <lane> <40-hex>`, or the historical lane-less
`REQUEST CHANGES AT <40-hex>`) zeroes its whole comment even if that comment also
carries an `APPROVED-AT` line. A verdict-ish keyword beside a 40-hex matching
neither grammar is reported as malformed, never silently skipped.

**The required review set depends on the canonical attestation.** An ordinary
change needs one valid exact-head Codex or Claude approval. A change carrying a
critical-class review attestation needs both. Canonical comment records and the
current GitHub actor permission establish authority. `passed-review-*` and
`adversarial-review-*` labels are routing and reconciliation caches: a label
with no canonical comment grants no authority, and a valid comment does not
lose authority because its cache label is absent. An unresolved substantive
objection in either lane still blocks an ordinary or critical change.

**A rebase supersedes the direct binding.** Any push that changes the head sha
makes an existing binding `SUPERSEDED`. Measured on
hermit#2478: 28 `APPROVED-AT: codex` lines in seven hours, none naming the
current head, the last moved by a pure rebase. `review_rebase.py` may carry both
an approval and its critical classification only when the canonical comment
identity and current GitHub actor permission are present and `git range-diff`
proves the patch series unchanged. Direct consumers that cannot perform that
proof require current-head evidence instead. ⚠️ But **`main` advancing
invalidates nothing** — only a push to the *branch* does, and a head two commits
behind `main` can still be `mergeable_state: clean`. Conflating those two makes
this look unsolvable. Rebase before review when practical; otherwise the landing
path must produce the mechanical proof or collect the required review set again
at the current head.

### ⚠️ Self-approval is possible here and it is not legitimate

There is **no independence check anywhere in the gate path**. `approval_binding.py`
reads a line out of a comment body and cannot see who wrote it;
`core-review-protocol-lint.sh` reads labels. An author *can* bind its own lane
and the gate *will* pass.

It is still forbidden. `post-facto-review` requires "independent exact-head
approval from one Claude-family reviewer and one Codex-family reviewer. **Neither
is the author.**" Native review refuses self-approval outright. So of the three
mechanisms, the only one that cannot check independence is the one that decides
landing — **the gate accepting it is a gap, not a permission**, and its silence
is not consent.

What this mechanism legitimately unlocks is the *other* case: a **non-author**
agent that cannot use `gh pr review` can still supply a real approval. It does
not let an author self-clear. Pushing a head — even a purely mechanical rebase —
makes you an author of it for this purpose.

If the fleet wants the gate to enforce what the protocol already states, the
check is cheap: compare the comment author against the pull-request author and
refuse a lane bound by the author.
