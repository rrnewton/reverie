# Claude coordinator — detailed instructions

Referenced from `AGENTS.md` → **Coordinator Modes**. Read this if your team slug is
`claude-coord-NNN`. `AGENTS.md` remains the authority; this file only expands your row.

**Follow skill `hermit-validation-authority` for validation authority.**

## Substrate

You have **a shell**. You have **no tmux, no panes, no ORC plugin surface**.

- **Primary workers: native agent teams.** The first-party `Agent` tool, running in-process. This is the
  default and carries the bulk of the work. Keep 1–5 busy on real tasks.
- **Cross-CLI adversarial review: external agents, to invoke Codex reviewers** of Claude-written code.
  Use both — native for the work, external for the cross-model review. A Claude reviewer reviewing
  Claude-written code does not satisfy the dual-review requirement.

## Your workers are not something you discover

You spawned them, so you know whether they exist. A completed or failed subagent returns its outcome to
you directly.

Never run `tmux ls`, `ps`, a pane registry, `ACTIVE.md`, or
`ci-hub/health/agent_liveness_probe.py` to learn whether **your own** team is alive. Those enumerate
tmux-launched ORC agents. They will find nothing, and "no live panes" then reads as an infrastructure
outage when the true statement is *"I dispatched nothing."*

> **Recorded failure, 2026-08-09.** A Claude coordinator reported "my fleet is dead — no live panes"
> after running zero subagents for an entire session, and spent hours on a 15-minute monitoring ritual
> instead of landing work. The seven `claude176-*` rows it found in `ACTIVE.md` were worktree-slot
> registrations, not agents. Meanwhile the `ci-hub` it was reporting from was 487 commits stale and was
> printing `HEALTHY` over a red `main`.

Corollaries:

- If a standing instruction says to keep N subagents busy and a session constraint says not to spawn any,
  **say so and ask** — do not resolve it silently and then describe the result as an outage.
- Refresh the parent checkout and re-check `ci-hub`'s own commit date before trusting a health verdict.

## What does NOT apply to you

`tmux_pane_id`, `cgroup_path`, `observe_owner_lease()`, `orc.scripts.*`, `orc.registerScript`,
`scripts/orc-hermit-msg.py`, Herdr tabs/panes, `agent_liveness_probe.py`.

- A slot you allocate is **born lease-less and stays that way**. That is normal and permanent, not a
  defect to repair, and it never blocks dispatch.
- **"The coordinator has no shell" is an ORC constraint.** You have one. Run the commands yourself or hand
  them to your own subagents. Never cite it as a reason not to act.
- **Hard Invariant 13's fifteen-agent cap counts independently-launched top-level `claude`/`codex`
  processes.** A native subagent runs inside your process and does not consume it. Worktree-slot caps
  (twelve active, five parked) apply to you exactly as to every other mode.

## Waking and routing

- **Your own subagents:** `SendMessage` addresses one you spawned, by id or name, and its result returns
  to you. Task notes (`tg note`) remain the durable record; the direct channel is only the wake.
- **A foreign fleet agent:** `SendMessage` cannot resolve fleet names and is not delivery
  acknowledgement. Write the note first, then ask that fleet's coordinator to relay. Never claim a
  foreign agent was notified merely because a message was attempted.

## Cross-model review: ASK THE PEER TEAM (corrected 2026-08-11)

**Superseding the 2026-08-09 "KNOWN GAP" note that stood here, which was wrong.** That note said no
mechanism existed on this box to obtain a Codex-family review, inferred from the absence of a registered
external-agent entrypoint. Absence of a *tool* is not absence of a *mechanism*.

**The mechanism is asking a Codex-family peer team on the GitHub thread.** Demonstrated 2026-08-11:
`rrnewton/hermit#2120` carries `APPROVED-AT: codex 7744e681b834` from
`[coordinator, GPT-5.6-SOL] [codex-hermit4-014, devbig014]`, and that team answered a relay of mine within
the hour and voluntarily released a claim. Peer coordinators are responsive on threads and are the review
channel.

So when a `post-facto-human-review` PR needs its Codex half:

1. Supply YOUR half first — a real exact-head adversarial review, role-tagged and carrying the full 40-hex
   head SHA. That is the authority; labels are caches, and any push invalidates it.
2. Then ASK on the thread, naming the PR, the exact head, your verdict, and what you want checked. Invite
   contradiction, especially on a BLOCK: a wrong BLOCK that stands is worse than one that gets corrected.
3. If the peer team's queue is full, that is a real answer. Do not treat silence as permission.

The `.claude/agents/` entrypoint still does not exist, and a registered one would be better than a thread
request. But **do not record cross-model review as impossible** — it is not, and recording it as impossible
cost this team hours of blocked PRs that a single comment unblocked.

[coordinator, claude-opus-5] [claude-coord-176]
