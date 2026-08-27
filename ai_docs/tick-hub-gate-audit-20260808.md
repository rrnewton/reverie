# Deep audit of the tick-hub gates — 2026-08-08

**Why this exists.** Owner, web, 2026-08-08:

> I never was involved with ANY of those tick hub creations. I would suggest a deep
> audit of them. They are trying to rigidly remind the agent of things it may not
> notice. This is good for external systems like github but it seems dangerous in
> other cases.

Auditor: `tickhub-surgeon`. All findings below are from reading the source and
measuring the running system on `devbig1073`; every number states how it was
obtained. Nothing in this document was inferred from a gate's own description.

---

## 1. The governance finding (the axis the owner actually raised)

**Git history cannot establish provenance for a single one of these gates.**

All 17 arrived in **12 commits over 6 days**, 2026-08-03 → 2026-08-07. Both
mechanisms that could attribute them are blank:

| Attribution mechanism | Result |
|---|---|
| git author | `Ryan Newton` on **12 of 12** — the shared machine identity every agent commits under. Cannot distinguish owner from agent. |
| AGENTS.md role + team trailer | present on **0 of 12** |

So the one mechanism designed to attribute autonomous work was absent on exactly
the commits where it mattered most. This neither corroborates nor refutes the
owner's recollection from git alone — but combined with the owner's statement, the
conclusion is that **this is self-authored monitoring that issues instructions to a
coordinator operating under an autonomous close-and-respawn mandate, with no record
of who authorized any of it.**

The whole surface is 6 days old. It accreted fast, unreviewed, and faster than it
was verified. (This audit is not exempt: §3a records a claim it got wrong by
reading a stale copy, and the correction that followed from measuring the live
one.)

Commit map:

| Commit | Date | Gates introduced |
|---|---|---|
| `8698bd1` | 08-03 | primary_checkout_snapshot, github_main_health, pull_request_health, **stuck_agents** |
| `7a42ff5` | 08-03 | speculative_land_obligations (removed 2026-08-26) |
| `4ed8e67` | 08-03 | ci_queue_health |
| `809eb05` | 08-03 | ci_lane_saturation |
| `847bf49` | 08-03 | **active_work_reconciliation** |
| `6f91646` | 08-03 | agent_container_lifecycle |
| `61e1e28` | 08-03 | memory_skill_sync (never activated) |
| `94c363c` | 08-04 | owner_tooling_directives |
| `dbee4e3` | 08-05 | **unowned_residue** |
| `3c421b2` | 08-06 | unpushed_parent_commits |
| `0630284` | 08-07 | **worktree_new_dead_owner**, validate_wall_regression, validate_wall_series_readiness |
| `e886201` | 08-07 | merge_gate_refire_due |
| `ecb2615` | 08-07 | **unowned_high_priority_backlog** |

Bold = removed 2026-08-08 (`119aa11`).

---

## 2. Five failure shapes — a checklist for anyone adding a gate

**Every one of these looks correct in source and fails in operation.** That is the
common thread, and it is why each must be checked by RUNNING the gate, not by
reading it. Reading the source is how all four survived.

| # | Shape | Ask | Antidote |
|---|---|---|---|
| 1 | **Bound to a moving reference** it cannot catch | Can the actor hold the target still long enough to satisfy this? | Time-budgeted deferral; judge "already done" against the *published* reference |
| 2 | **Alarms on a condition the recipient cannot clear** | Is the remedy available to whoever receives this page? | DELTA (page on newly-qualifying only); carry the standing set in fields |
| 3 | **The process outlives its own report** | Can this gate's work continue after the tick records a verdict, and does it mutate? | Process-group teardown on timeout (already present); never hold a shared lock |
| 4 | **The alarm text was never exercised** | Has anyone ever seen this gate's output *rendered*, not just its source? | Run it through `parse_kv_lines` + `render_emit`; assert `summary` captured and zero surviving `{placeholder}` |
| 5 | **The named remedy does not resolve the named condition** | Follow up: after someone runs the remedy, did the condition actually clear? | Split the population by cause; give each cause its own remedy, or say "no known remedy" and why |

**Shapes 1–4 describe a gate that DETECTS wrongly. Shape 5 describes one that
detects correctly and PRESCRIBES uselessly**, and it is the most insidious of the
five because *naming a remedy is exactly what makes an alarm look rigorous*. The
gate cited two sections down as the model carried it: `merge_gate_refire_due`
named `--refire` for every parked gate, and measured over 8 real refires, 3
resolved green and 5 resolved RED — all five failing on
`reverie-pin-is-latest-main`, four of them with **every leg green**. That pin
check compares the head against a `reverie/main` that advances continuously, so a
refire re-runs it against a target that has moved further ahead: for those PRs the
remedy was *structurally incapable* of succeeding. Shape 5 is therefore often
shape 1 wearing a remedy's clothes.

Its detection cost is the thing to note: shapes 1–4 are visible by inspecting the
gate, shape 5 is only visible by **following the remedy to its outcome**. Nobody
had, because each individual refire looked reasonable. The fix was
`REFIRE_FUTILE` — a separate class, reading the merge gate's own pin verdict
rather than recomputing it, carrying a re-pin instruction and never a refire.

It also settles a question that was live at the time: whether refire should be
automatic, since the gate names its own fix. No — automating a remedy that fails
5 times in 8 industrialises the error, unbounded, against a receding target, with
each dispatch defensible in isolation. Revisit only once the refirable set is
established.

Shape 4's predicate is narrower than it first looks and the correction matters:
the risk is **not** "a gate that has never fired". `validate_wall_series_readiness`
fired every day for a day and a half emitting a literal `{summary}` plus a garbage
field, and nobody read it. The operative predicate is **a gate whose rendered
output has never been inspected.** Waiting for a first fire would not have caught it.

### What a good gate looks like

`merge_gate_refire_due` after its rework is the counter-example to hold up. It
separates newly-parked from standing so it is satisfiable; it names the exact
remedy command in the alarm so the reader can act without re-deriving; and it
states its own coverage as *"70 of 143 heads, a floor not a census"* so the count
travels with its denominator. A gate that does those three things is hard to get
wrong in operation.

Three were known when this audit was commissioned; the fourth was found during it, and the fifth by following a remedy to its outcome afterwards.
Shape 3 as originally written here was **wrong and has been corrected** — see §3a.
The genuinely damaging finding is §3b, which is independent of it.

1. **Bound to a moving reference it cannot catch.** `primary_checkout_snapshot`
   demanded the parent equal a live-queried `origin/main` while 13 agents push it.
2. **Alarms on a condition the recipient cannot clear.** `merge_gate_refire_due`
   paged every 30 min for a backlog it deliberately does not auto-drain.
3. **The process outlives its own report.** See §3 — **partially retracted**:
   the timeout path was already fixed on 2026-08-05 and is correct. The residual
   gap is the *success* path, which never reaps the group.
4. **Mutation under a read-shaped name.** `unpushed_parent_commits` sounds like a
   report and performs remote pushes; `primary_checkout_snapshot` sounds like a
   snapshot read and commits + pushes to parent `main`.

The unifying rule, now recorded in `tick-hub.yaml`:

> **An alarm must be satisfiable by the actor it instructs.** Otherwise it is
> obeyed in a loop or muted — and a muted gate is *silently disarmed*, which is
> strictly worse than never having built it.

Three antidotes already exist in-tree and are now each in use: **DELTA**
(`slot_disk_residue`, `gate_refire --gate`), **SUSTAINED-ACROSS-N**
(`lane_health`), **TIME-BUDGETED DEFERRAL** (`primary_snapshot_gate`).

---

## 3. Shape 3: what was claimed, what is actually true, and the live instance

### 3a. CORRECTED 2026-08-08 — the timeout path was already fixed; the success path is the real gap

> **This section originally claimed the runner kills only the gate's direct child
> on timeout, and generalised that to "all 17 gates can outlive their report".
> That claim was WRONG and is retracted.** It was read from
> `hermit/agent-utils/py/tick_hub/probes.py` — hermit's own nested pin at
> `a6f4232`, **114 commits behind** and dated 2026-08-04. That is not the copy
> tick-hub executes. `ci-hub/bin/agent-tool` runs `$ROOT/agent-utils` at the
> parent's pinned gitlink, and the correction below is measured against that.

**What the live runner actually does.** At dev-hermit's pinned agent-utils commit
`961f9fd`, `py/tick_hub/probes.py` already spawns with
`start_new_session=(os.name == "posix")` and, on `TimeoutExpired` or `OSError`,
calls `_terminate()` → `os.killpg(proc.pid, SIGKILL)`. It even ships a test named
`test_gate_timeout_kills_background_descendants`. The process-group teardown
landed in `5ef91c5`, 2026-08-05.

**Bracketed empirically against the live module, both directions:**

| Scenario (gate backgrounds `sleep 40` via `( … & )`) | Gate result | Background child survives? |
|---|---|---|
| **A** — gate exceeds its budget | `ok=False rc=-1 "timed out after 3s"` | **NO** — killed with the group |
| **B** — gate exits 0 having backgrounded the child | `ok=True rc=0` | **YES** |

So the defect is not the one filed. **The timeout path is correct. The SUCCESS
path never reaps the group**, because `_terminate()` is only reached from the
timeout and OSError branches. Scenario B is exactly the post-commit shape: the
gate finishes quickly and successfully, and the rescue pusher it backgrounded
keeps running — which, combined with §3b's fd inheritance, is the real mechanism
behind the wedged mutex.

**Exposure of the stale copy: none.** `hermit/agent-utils` lacks the fix
(`5ef91c5` is not an ancestor of `a6f4232`), but nothing in hermit invokes
`tick_hub` — hermit uses agent-utils only for `safe-ci-dag-runner`. No workflow or
script runs the old runner.

**No runner change is recommended, and that is a deliberate call.** Killing the
process group on the *success* path would destroy deliberately-detached work: the
post-commit hook backgrounds the rescue pusher precisely so a commit is not
blocked on a network push, and truncating it would lose the rescue of unpushed
commits — a data-safety feature — automatically and unattended on a 14-agent box.
The actual harm is the *mutex*, not the survival, and §3b fixes that surgically
with `9>&-`. Building a kill-on-success path on a refuted premise would be the
same pathology this audit documents.

Two orphaned `unpushed_parent_commits --scope all --rescue` trees were observed at
`ppid=1` during the audit, from `/tmp/post-commit-lock-test.AECFbV/{fixed,legacy}`.
Those are another agent's A/B harness, run by hand — **not** tick-hub timeouts, and
they are not evidence of a runner defect.

### 3b. The serialized parent-main lock is held by inherited fd 9, not by its owner

**This is the most consequential finding in the audit.**

`scripts/parent-main-write` takes its mutex with `exec 9>"$lock"; flock 9`. A POSIX
`flock` is released when **every** file descriptor referring to that open file
description is closed — not when the acquiring script exits. Fd 9 is inherited by
every descendant.

`git commit` inside `parent-main-write commit` fires the **post-commit hook**,
which does exactly this (canonical hook, lines 18-19):

```bash
( "$root/ci-hub/health/unpushed_parent_commits.py" --scope all --rescue \
    >> "$root/ignored/unpushed-parent-rescue.log" 2>&1 & ) >/dev/null 2>&1
```

A detached background job that performs **network pushes** and inherits fd 9. It
therefore holds the parent-main mutex for its entire lifetime, long after
`parent-main-write` has printed `PARENT_MAIN_WRITE published=... ancestry=1/1` and
exited.

**Observed directly**: the lockfile recorded `pid=950753`, that pid was **GONE**,
and 12 unrelated descendant processes held the lock fd open. A subsequent writer
sees `REFUSED: another parent-main writer owns ... after 0s` naming a dead pid.

**Controlled reproduction**, own processes only, both directions:

```bash
# BROKEN — reproduces the production pattern
exec 9>the.lock; flock -n 9
( sleep 25 & ) >/dev/null 2>&1      # the post-commit pattern
exit                                 # "its own report says done"
=> LOCK STILL HELD by the detached grandchild (pid=… fd=9 cmd=sleep 25)

# FIXED — one token
( sleep 25 9>&- & ) >/dev/null 2>&1  # close the inherited lock fd
=> LOCK FREE, and the background job still runs
```

**Recommended fix:** close the inherited descriptor at the background launch
(`9>&-`, or defensively `{9..255}>&-`). This is strictly better than the timeout
wrapper being A/B tested: a `timeout 240` still holds the mutex for up to 240
seconds, whereas `9>&-` releases it immediately while the rescue push proceeds.
**Not applied here** — `/tmp/post-commit-lock-test.{fixed,legacy}` shows another
agent owns this experiment; this audit hands them the mechanism and the bracket.

### 3c. What this cost, measured

Four consecutive `parent-main-write` publish attempts by this agent were refused
during this session. Landing a 3-file commit required building it off `origin/main`
with a temporary index and `commit-tree`, because the shared parent tree could not
be fast-forwarded (an incoming commit touched a file another agent held
uncommitted, and git correctly refused to clobber it).

### 3d. One claim in the commissioning brief, corrected

The brief states `unpushed_parent_commits --rescue` "holds the serialized
parent-main writer lock … it is how it acquired the lock in the first place". The
*conclusion* is right; the *route* is not. Its own pushes go to
`refs/heads/rescue/auto-<sha>`, and the pre-push gate is destination-ref-scoped —
`[ "$remote_ref" = refs/heads/main ] || continue` — so a rescue push never invokes
`verify-push` and never takes the lock itself. It acquires the mutex **by
inheritance from the post-commit hook of whoever was publishing**, per §3b. This
matters for the fix: hardening `unpushed_parent_commits` alone would not release
the lock; the fd must be closed at the hook.

---

## 4. The 17, on three axes

**Observability** = can ORC see this directly? If yes it must not be policed here.
**Blast radius** = what does firing this *cause*?

| # | Gate | Cadence | Command | Observability | Blast radius | Verdict |
|---|---|---|---|---|---|---|
| 1 | `merge_gate_refire_due` | 1800s | `gate_refire.py --gate` | **External** — GitHub merge-gate parking; ORC blind | Page → human runs `--refire`. No autonomous action | **KEEP, re-scoped** `8a0545d`: was standing-state + literal `{summary}`; now delta + names PRs |
| 2 | `worktree_disk_residue` | 900s | `slot_disk_residue.py --gate` | **Local disk** — ORC has no shell | Detect-only; reclaim needs a recovery SHA | **KEEP** (new `119aa11`, replaces #13) |
| 3 | `validate_wall_regression` | 3600s | `wall_series.py --gate-regression` | **Ledger** — ORC blind | Report only; currently HOLDS at rc=0 | **KEEP** |
| 4 | `validate_wall_series_readiness` | 86400s | `wall_series.py --readiness` | **Ledger** — ORC blind | Report only | **KEEP** — standing by design, but daily; re-verify the 90d/3.91d premise (see §5) |
| 5 | `owner_tooling_directives` | 3600s | `directives/check.py` | **Owner intent recorded outside TaskGraph** — ORC blind | Report only | **KEEP — high value**, but currently red for a lineage artifact; see §5 |
| 6 | `unpushed_parent_commits` | 900s | `unpushed_parent_commits.py --scope all --rescue` | **"Not on GitHub"** — external by construction | ⚠️ **MUTATES**: pushes `rescue/auto-*`. Additive, cannot destroy | **KEEP, but see §3b/§6** — mutation under a read-shaped name; self-clears via `--not --remotes` |
| 7 | `speculative_land_obligations` | 300s | removed | **Retired** | No action | **REMOVED 2026-08-26** |
| 8 | `primary_checkout_snapshot` | 300s | `operational_health.py primary-snapshot` | **Local git** — ORC has no shell | ⚠️ **MUTATES**: commits + pushes parent `main`. Touches no agent | **KEEP, re-scoped** `0e14559`: moving-reference + ordering bug fixed; 60-min deferral budget |
| 9 | `github_main_health` | 900s | `operational_health.py github-main` | **External** | Report only | **KEEP** |
| 10 | `pull_request_health` | 900s | `operational_health.py pull-requests` | **External** | Report only | **KEEP** |
| 11 | `ci_queue_health` | 900s | `operational_health.py queue-health` | **External** | Report only | **KEEP** — shape (c) risk: instantaneous sample, no sustain |
| 12 | `ci_lane_saturation` | 900s | `lane_health.py tick` | **External** — runner lanes | Report only | **KEEP** — already the SUSTAINED-ACROSS-N reference implementation |
| 13 | `worktree_new_dead_owner` | 900s | `worktree_liveness.py --fail-on-new-dead` | ❌ **ORC owns agent liveness** | 🔴 Feeds autonomous close-and-respawn | **REPLACED** by #2. Reported an agent dead while it held the freshest activity timestamp on the box |
| 14 | `stuck_agents` | 300s | `operational_health.py agents` | ❌ **ORC owns the fleet** | 🔴 Feeds autonomous close-and-respawn | **REMOVED** `119aa11` |
| 15 | `active_work_reconciliation` | 300s | `ci-hub active-work --gate` | ❌ **ORC owns tasks + agents** | 🔴 Task reassignment / agent replacement | **REMOVED** `119aa11` |
| 16 | `unowned_high_priority_backlog` | 300s | `unowned_backlog.py --gate` | ❌ **ORC owns TaskGraph** | Report; censused a DB the orchestrator could not see for a whole session | **REMOVED** `119aa11` |
| 17 | `unowned_residue` | 3600s | `residue_sweep.py --gate` | ❌ **ORC fleet + TaskGraph** | Report + routes notes | **REMOVED** `119aa11`; its one non-ORC signal is covered by #2 |
| — | `agent_container_lifecycle` | 300s | `agent-podman.rs reconcile` | **Containers** — libpod scopes outside the agent cgroup; ORC blind | 🔴 **was** `--apply` → `podman stop`+`rm` | **KEEP, re-scoped** `119aa11`: `--apply` withdrawn; audit retained |
| — | `memory_skill_sync` | 3600s | *(commented out)* | n/a | n/a | **INACTIVE since `61e1e28`** — never activated. See §5 |

Net: **17 → 13 active.** 4 removed, 3 re-scoped, 1 replaced, 9 kept unchanged.

Every removal was an ORC-observability failure with a destructive or misleading
blast radius. **No gate was removed for watching something real.**

---

## 5. Gates whose stated purpose may no longer exist

- **`memory_skill_sync`** — committed 2026-08-03 and deliberately left commented
  out ("Activation is the owner's call"). Five days inactive. Either activate it or
  delete it; a permanently-disabled block is a claim of coverage that does not
  exist.
- **`validate_wall_series_readiness`** — its premise is retention 3.91d < 90d and
  conditioning 4/26. Both are dated 2026-08-07 measurements. **Not re-derived
  here** (would need a ledger query). If retention has since grown, the gate is
  reporting a blocker that has cleared. Flagged, not concluded.
- **`ci_queue_health`** — the only remaining gate that pages on an instantaneous
  sample, which its neighbour `ci_lane_saturation` explicitly contrasts itself
  against. Not a defect today; the shape (c) candidate if it becomes noisy.

- **`owner_tooling_directives` — a claim I was given, checked, and must qualify.**
  The commissioning brief credits this gate with "21 pieces of dropped owner
  intent", and it is the reason the brief warns against removing gates that look
  like noise. The gate genuinely earns its place — owner intent recorded outside
  the TaskGraph is invisible to ORC and to every other check. But its live output
  right now is not 21 fresh drops:

  ```
  state=red  source_rows=16  records=21
  satisfied=0 partial=0 open=0 gated=0 needs_owner=0 unaccountable=0
  missing_task=21  not_landed=0  unverifiable=0  drift=21
  ```

  **All 21 records are `missing_task`** — their task ids do not resolve in the
  current database. That is the `hermit1.db` → `hermit2.db` lineage split, not 21
  newly dropped directives. The in-flight fix is `fe29f35` *"directives: resolve
  tasks across the database lineage, and split task_not_found from missing_task"*.

  Two consequences worth stating. First, the *tracking* value (21 directives under
  watch, 16 source rows) is real and is the reason to keep the gate. Second, until
  `fe29f35` propagates, this gate is red every hour for a reason its recipient
  cannot clear — **failure shape 2**, arrived at from a different direction. It
  should self-resolve; if it does not, it needs the same delta or deferral
  treatment as #1 and #8.

---

## 6. Recommended follow-ups (none applied here)

1. **`9>&-` at the post-commit hook's background launch** (§3b). Highest value on
   the board: it unblocks parent-main writes fleet-wide. Owner:
   whoever owns `/tmp/post-commit-lock-test`.
2. ~~Kill the process *group* on gate timeout~~ — **WITHDRAWN, already done.**
   `start_new_session` + `killpg` landed in agent-utils `5ef91c5` (2026-08-05) and
   is live at dev-hermit's pin. Verified empirically; see §3a. The residual
   success-path gap is deliberately NOT being fixed in the runner — killing
   deliberately-detached work unattended is worse than the leak, and §3b addresses
   the actual harm.
3. **Rename mutating gates to say so**, or split them. `unpushed_parent_commits`
   and `primary_checkout_snapshot` both read as reports and both write.
4. **`rescope-agent-container-reclaim-to-process-evidence`** — filed; restore
   autonomous container reclaim only on process evidence, never an agent census.
5. **Require the role+team trailer on tick-hub changes.** 0 of 12 carried one. A
   pre-commit check on `ci-hub/health/tick-hub.yaml` would close the governance gap
   the owner raised, at the point where it recurs.

---

## 7. Admission tests now recorded in `tick-hub.yaml`

Both are inline in the config, where the next author meets them before adding a
gate:

1. **Can ORC observe this directly?** Yes → it does not belong here.
2. **Is this alarm satisfiable by the actor it instructs?** No → it will be looped
   on or muted.

To which this audit adds a third, from §3:

3. **Can this gate's work outlive its own report, and does it mutate?** If both,
   it must not hold a shared lock — see §3a/§3b. Note the runner already tears down
   the process group on *timeout*; the surviving case is a gate that exits
   successfully having backgrounded work.
