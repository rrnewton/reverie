# The 2026-08-20 "hermit fleet died" alarm was a reboot-spanning delta

**Collected:** 2026-08-20, ~18:03Z–18:20Z, on `devbig030.atn3.facebook.com`, read-only.
**Collected by:** hermit-902 (claude, opus-5), task
`migrate-the-crash-cortex-thread-and-gather-the-mass-death-evidence`.
**Nothing was released, reclaimed, quarantined or killed.**

Two deliverables live in this directory:

| file | what it is |
| --- | --- |
| `crash-cortex-thread-transcript.md` | Google Chat `spaces/AAQA7wp4wy8` thread `j6EtEGFW7C8`, migrated. |
| this file | the mass-death evidence, and what it actually shows. |
| `raw/` | the unedited command output every claim below rests on. |

---

## The claim

At 2026-08-20T17:59Z the hermit coordinator sent, to the Crash Cortex DM
`spaces/7jYLDKAAAAE` and to `spaces/AAQAA6Irlwg`:

> **My entire agent fleet died about two minutes ago: 15 agents gone at once, 1 survivor.**
> […] I hypothesised a `boot_id` mismatch marking every sidecar dead simultaneously […]
> **It was not an instrument fault. The processes died.**

The 15: `coord`, `shell`, `sabre`, `fixtures`, `li2`, `observable`, `revents`, `kvm2`,
`panic`, `codexrev`, `liteinst`, `dbt`, `bytes`, `consistency`, `teardown`. Survivor:
`hermit-research`. Supporting readings: `slots_occupied` 11 → 0, `zombie_pids` 59 → 9,
six slots at `authority_state=dead` with `live_processes_under_slot=0`.

Both messages are in
`~/.orc/logs/4fb50e87-5d91-4294-88b2-afeedf6cc917.2026-08-20-17-59.log` and
`…-18-02.log` as `meta_cli: dispatching` `SendMessage` records.

## What actually happened

**The fleet died at an orderly operator reboot on 2026-08-16T04:17:54Z.** By the time the
alarm was written it had been dead for **4 days 13 hours 41 minutes**. Nothing died on
2026-08-20.

The coordinator is not lying and its readings are not wrong. It is a long-lived ORC
session (`4fb50e87-…`, name `hermit`, db `hermit3`, 16 165 messages, created 2026-07-20)
that went down with the machine, stayed down 4.5 days, and was resumed at
2026-08-20T17:55:07Z. Four minutes into its new life it looked around, found the fleet it
remembered was gone, asked the tick, and the tick handed it a delta measured against a
sample from before the reboot. Both instruments agreed — but they shared one stale
baseline, and agreement between two readings off the same stale baseline is not
corroboration.

### The timeline

| time (UTC) | event | source |
| --- | --- | --- |
| 2026-08-16T04:03:03Z | slot registry `worktree-state.json` last written before the gap | its own `updated` field + mtime |
| 2026-08-16T04:11:24Z | ORC session `4fb50e87` writes its last log line | `~/.orc/logs/4fb50e87-….2026-08-16-04-11.log` |
| 2026-08-16T04:12Z | clean shutdown begins | `last -x shutdown`: `6.13.2-0_fbk15_h  Sat Aug 15 21:12 - 21:18 (00:05)` |
| 2026-08-16T04:17:54Z | boot | `/proc/stat` `btime 1786853874`; `last -x reboot`; `who -b` |
| 2026-08-16T04:18:45Z | a **new** tmux server starts (pid 35997) | `ps -o lstart -p 35997` |
| — | **4 d 13.7 h with no hermit ORC process at all** | `~/.orc/logs` holds 99 files dated 2026-08-16 and 15 dated 2026-08-20, nothing between |
| 2026-08-20T17:55:07Z | `orc --db hermit3 --resume --session-id 4fb50e87-…` starts, pid 683755 | `ps -o lstart -p 683755` |
| 2026-08-20T17:55:12Z | 26 of 30 tick gates fire at once | `.tick-hub/fired-state`, `last_fired=1787248512` |
| 2026-08-20T17:55:30Z | residue tick overwrites its prior sample | mtime of `.tick-hub/slot-disk-residue-state.json` |
| 2026-08-20T17:58:32Z | task `six-slots-read-dead-while-their-owners-are-alive` created | `hermit3` tg db `created_at` |
| 2026-08-20T17:59Z | "the hermit fleet died" sent to two spaces | ORC log |
| 2026-08-20T18:01:31Z | **first** agent pane process of the new session starts (`hermit-901`) | `ps -o lstart -p 1002072` |

The kernel version changes across the boundary — `6.13.2-0_fbk15_hardened` before,
`6.19.2-0_fbk0_hardened_rc25_0_g1d99d2f6053c` running now. A five-minute down window and a
kernel bump is a planned upgrade. **The cause of death is that the owner rebooted the
box.**

### Each supporting reading, resolved

**`slots_occupied` 11 → 0 and `zombie_pids` 59 → 9 are both reboot-spanning.**
`.tick-hub/slot-disk-residue-state.json` is the residue tick's prior-sample store. Its
per-slot `sized_at` values are all `2026-08-15T23:24Z`–`23:57Z`; the file was next written
at 17:55:30Z on 2026-08-20. The 11 and the 59 are pre-reboot; the 0 and the 9 are post.
The tick stores a prior sample but records nothing about which boot it came from, so it
cannot tell a delta from an incident.

**The six dead sidecars are not six. They are every active row that has a sidecar, out of
15 dead rows, and all 15 are dead for the same reason.**
`scripts/slot-owner-authority.py --all --json`, run twice under the shared registry lock:

```
registry_sha256 01ce17b38cce8b88…   29 rows:  15 dead, 14 indeterminate
registry_sha256 62dfd892f3859c66…   32 rows:  15 dead, 17 indeterminate   (registry changed under us at 18:08:58Z)
```

Every one of the 15 dead carries `reason_code=owner-boot-id-mismatch`, detail *"the
recorded supervisor belongs to a previous host boot"*. **Not one is dead for a missing pid
or a `start_ticks` mismatch** — which is what a process that actually died would produce.
The six named in the alarm (`ci`, `claude-exitgate`, `coord`, `pollrevents`,
`revents-measure`, `slot10`) are simply the subset whose slot `state` is `active`.

Every sidecar in the registry — 24 of them at the second read — records
`boot_id=3ac87f92-e589-489d-9e37-bc1ab32d6a3e`. The live boot_id is
`5dc71364-bd91-4260-8815-573614831137`, stable across repeated reads and across separate
processes. **Zero sidecars belong to the running boot.** `slot10`'s
`owner_identity=1750257@39103318` is a pid and a `start_ticks` from a kernel that is no
longer running; it cannot be resolved at all, in either direction.

**"One survivor" is not a process that survived.** The alarm itself records
`hermit-research` as *(free)*. A free agent holds no tmux pane, so it had nothing to lose
when the tmux server went down with the machine.

### The step that made it survive scrutiny

Task `six-slots-read-dead-while-their-owners-are-alive`, created 17:58:32Z, states:

> **TWO OF THEM BELONG TO AGENTS THAT ARE ALIVE AND RESPONDING IN TMUX RIGHT NOW —
> `slot10` is hermit-shell's (task `bump-reverie-pin-to-main-tip`) and `coord` is
> hermit-coord's.**

That sentence is what refuted the stale-identity explanation: if the owners are alive, a
stale record cannot be the story. **It was not true when it was written.** Pane process
start times (`ps -o lstart=`), against a tmux session created 10:55:08 PDT:

```
%169 orc            10:55:07 PDT = 17:55:07Z
%170 hermit-901     11:01:31 PDT = 18:01:31Z
%171 hermit-902     11:03:17 PDT = 18:03:17Z
%172 hermit-coord   11:03:57 PDT = 18:03:57Z
%173 hermit-cortex  11:04:39 PDT = 18:04:39Z
```

At 17:58:32Z the session held exactly one pane — orc's own. `hermit-coord`'s process did
not start for another 5 min 25 s, and there is no `hermit-shell` process on the box at all
(checked with `ps` and full args; `pgrep -f` self-matches).

What produced the claim is a **name match**: slot `coord` and agent `hermit-coord` share a
word. `slot-owner-authority.py` refuses name matching by construction — it answers only
from `(boot_id, supervisor_pid, start_ticks)` — and it answered
`owner-boot-id-mismatch`. The conclusion that overrode it came from matching strings
against agents that did not yet exist.

The chain ran: stale sidecars read dead → *but their agents are alive* → therefore not an
instrument artifact → therefore fifteen real deaths → broadcast to an external party. Each
step follows from the one before. Only the second is wrong.

### What was ruled out, not assumed

- **Kernel OOM.** `dmesg -T` is silent from 10:10:55 PDT onward; nothing at all in the
  10:40–11:05 PDT window. The only OOM kills in the buffer are at 10:10 PDT inside
  `safe.slice/safe-ci-*.scope/step-mem.hog|breach-default|offender` — the safe-dev-limits
  harness deliberately breaching its own cgroup limits. Same uid (212630), but a test
  fixture, and 43 minutes earlier.
- **Host memory pressure.** `free -g`: 1010 total, 246 used, 764 available.
- **The hardened kernel blinding the occupancy scan.** This is a live hypothesis — the new
  kernel is a `_hardened` build and `slot_disk_residue.occupancy()` reads
  `/proc/<pid>/{cwd,exe,root,fd,map_files,maps}`. It is false. Run against the live
  `/proc`, the scan works: it found a genuine occupant of slot `ci` (pid 1566607, a `find`
  over `worktrees/fix-futex`, seen through its `fd` links), counted `zombie_pids=9`
  matching `ps -eo stat` exactly, and reported only `blind_pids=2`. **0 is the true
  current occupancy.** It is the 11 it was compared against that belongs to another boot.
- **A tmux server crash.** The server (pid 35997) is shared with four other ORC sessions,
  all created 2026-08-18 and all still running. It did not restart on 2026-08-20.

## What is still true and still matters

- **The 15 agents really are gone, and their work really was interrupted mid-flight.** The
  loss is real; only its date and cause change. The 13 tasks tagged `bankrupted-2026-08-16`
  in the `hermit3` tg db are the correct accounting, and that tag carries the **right**
  date. (Those same 13 rows have since also picked up `bankrupted-2026-08-20`, which does
  not. Their `tags` column is additionally mangled by nested JSON encoding —
  `"[\"[\\\"manifest\\\"\""` — which will defeat a naive tag query.)
- **Do not reclaim.** 31 slots read reclaimable holding up to 403.6 GiB, and `slot10`
  carries the unlanded Reverie pin bump on `fix/bump-reverie-pin-ee6716a`. That the
  liveness reading is stale makes the reclaim list *more* dangerous, not less: the
  registry has no record of the current boot at all, so it cannot distinguish a slot idle
  since the reboot from one a live agent is using. The occupancy scan can, and it is the
  thing to ask.
- **The false report went outside the project.** The 17:59Z message landed in
  `spaces/7jYLDKAAAAE` — the Crash Cortex DM with Danielle Costantino — tagged
  `[$orc-hermit3]`, offering the "mass death" as a live case for Crash Cortex to analyse.
  It needs correcting there, not just here.

## The defect worth fixing

The tick keeps a prior sample and no notion of which boot produced it, so after any
restart it reports its first post-restart reading as a delta against the last pre-restart
one. A boot identity is already available and already used elsewhere in the same tree —
`slot-owner-authority.py` reads `/proc/sys/kernel/random/boot_id`, and that check is
exactly what got the right answer here while the delta got the wrong one. Stamping the
residue tick's prior sample with the boot it was taken under, and reporting
*no comparable prior sample* instead of a delta when it does not match, removes this whole
class of report.

Separately, `scripts/hermit-health-tick.sh` exists to make the tick survive session
recycling via a systemd `--user` timer, and its own header names the failure it replaces:
*"when the hosting coordinator session was recycled the loop stopped […] and the silence
looked exactly like health."* **It is not installed.** There is no `hermit-health-tick`
unit or timer in `systemctl --user list-timers --all`, and
`~/.local/state/hermit-health-tick-invocations.log` does not exist, so it has never run
here. The tick is still ORC-session-bound, which is why 26 gates went 4.5 days without
firing and then all fired within five seconds of the resume.
