# The coordinator did not kill the fleet — its parent did, and took it too

**Established:** 2026-08-20, ~19:1xZ, on `devbig030`, read-only.
**By:** hermit-902 (claude, opus-5), task `did-the-coordinator-dying-kill-the-fleet`.
Raw output: `raw/coordinator-death-parentage-and-slot10.txt`. Addendum to `README.md`
in this directory, which established the reboot; this closes the mechanism.

## The hypothesis

*"If the coordinator process died, tmux-hosted agents dying alongside it explains 15
simultaneous deaths with no exotic mechanism at all."* The instinct — no exotic
mechanism — is right. The mechanism is one level up.

## The coordinator died gracefully, at shutdown

Its last log file, `~/.orc/logs/4fb50e87-…2026-08-16-04-11.log` (204 lines), ends at
`04:11:24.658Z` with an orderly teardown:

```
INFO  HTTP server shutting down gracefully
INFO  routable HTTP server (IPv4 routable) shutting down gracefully
INFO  Orc Thrift server shutting down
DEBUG event subscriber cancelled, draining remaining events
INFO  GSD periodic sync: cancellation received; stopping
DEBUG reconciler cancelled            (x4)
DEBUG Periodic job cancelled          (x3)
```

That is a SIGTERM being handled, 36 s before the recorded system shutdown at 04:12Z —
not a crash.

## The hosting relationship refutes the causal claim

Measured `ppid` for every relevant process, across three live ORC sessions:

```
35997    ppid 30515    tmux: server     Sat Aug 15 21:18:45  (= 2026-08-16T04:18:45Z)
683755   ppid 35997    orc (hermit3)
1002072  ppid 35997    codex   hermit-901
1105199  ppid 35997    claude  hermit-902
1161448  ppid 35997    codex   hermit-coord
1233935  ppid 35997    claude  hermit-cortex
4045741  ppid 35997    orc (hatch)      + 13 agent panes, all ppid 35997
2379445  ppid 35997    orc (scx-sim)    + 4  agent panes, all ppid 35997
```

**Every agent is a child of the tmux server. Not one is a child of any orc.** The
coordinator and the agents are siblings.

And the tmux server is itself new: started `04:18:45Z`, **51 seconds after the boot at
`04:17:54Z`**. The pre-reboot server, which held all 16 panes, went down with the machine.

## The coordinator issued no teardown as it died

Its last three log files were searched for `kill-pane`, `kill-session`, `kill_pane`,
`killing agent`, agent `stop|kill|terminat`, `send-keys`, pane `kill|destroy`: **zero
hits**. Its shutdown touches only its own Thrift server, HTTP servers, reconcilers,
periodic jobs and event subscribers.

tmux is configured so that its death would not have taken the session anyway:
`destroy-unattached off`, `remain-on-exit off`. An orc exit closes its own window
(window 1); the session survives while other windows exist.

## Conclusion

```
host reboot -> tmux server destroyed -> all 16 panes (orc + 15 agents) killed at once
```

**Common-parent death, not coordinator-induced death.** The simultaneity is exactly why
it reads as "the coordinator took them with it". On this evidence, had the coordinator
alone died, the agents would have kept running.

### What cannot be established, stated as the real answer it is

There is **no observed instance** of a pane surviving an orc death on this host. No orc
has died and restarted inside a boot epoch in the surviving evidence — `46e440f8`'s orc
has run since Aug 18 07:55:01, `e7cabcde`'s since 09:02:12, `orcclaw`'s since 09:30:38,
each predating every one of its own agents. So "agents survive a coordinator restart" is
an **inference** from parentage, tmux options and the absent teardown, not an observation.
The specific question is settled; the general one is not.

Corroborating, at code level only: `release-worktree.rs:1427` refuses when a
*"recorded owner is absent/terminal in ORC but retains N exact-name tmux pane(s)"* — the
tree already encodes that a pane can outlive its ORC agent record.

## The bookkeeping that manufactured the alarm

From the dying coordinator's own log, `04:11:08.195Z`–`04:11:10.900Z`:

```
thrift sync failed; marking agents unreachable        (x2, ports 31419 and 31416)
DB_UPDATE: agent status change  orc-e7cabcde-…       managed -> Unreachable
DB_UPDATE: agent status change  orc-orcclaw-facc6d30 managed -> Unreachable
DB_UPDATE: agent status change  hermit-coord         busy    -> NeedsInput
```

It marked agents **unreachable, not dead**, then died before revisiting it. Four days
later the resumed coordinator asked for `hermit-coord`, got `agent hermit-coord was not
found`, and read a four-day-old unreachable record as a death two minutes old. **The alarm
was manufactured by a resume, not by an event.**

## Recovery: what survives, and slot10 specifically

| event | lost | kept |
| --- | --- | --- |
| coordinator death | agent context (the conversation), ORC bookkeeping | everything on disk — a slot is a git worktree owned by the filesystem, not by the orc process |
| host death | additionally: anything uncommitted or unpushed, and every pane | committed-and-pushed work |

**The Reverie pin bump is pushed. It is not at risk.**

```
repo             worktrees/slot10/hermit
origin           https://github.com/rrnewton/hermit.git
branch           fix/bump-reverie-pin-ee6716a
local HEAD       27526f68f4d7ce526a6f5f7749d28c2252aee629
remote ref       27526f68f4d7ce526a6f5f7749d28c2252aee629   (same sha)
dirty paths      0
ahead of remote  0
```

It is **not** on `fbexp` (`facebookexperimental/hermit`), checked with a control so the
absence is a real answer and not a failed lookup: `ls-remote fbexp refs/heads/main`
returned `9084dca5c747…` while the branch pattern returned nothing. Being on the
`rrnewton` fork is what matters for surviving host loss.

*A correction worth recording, because it nearly went the other way:* the first check ran
`ls-remote` against `dev-hermit`, `facebookexperimental/hermit` and
`facebookexperimental/reverie`, got three empty results, and was one step from
"not pushed anywhere". The slot's own repo has a **second** remote — `origin` =
`rrnewton/hermit` — and that is where it lives. Enumerate the remotes of the repository
you are actually asking about, not the parent's.

So the stated reason for the reclaim freeze on `slot10` — unlanded work at risk — does not
hold. **Nothing was reclaimed**; that remains the owner's call, and the branch is still
unlanded and unreviewed.
