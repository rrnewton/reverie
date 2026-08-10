# Claude coordinator — detailed instructions

Referenced from `AGENTS.md` → **Coordinator Modes**. Read this if your team slug is
`claude-coord-NNN`. `AGENTS.md` remains the authority; this file only expands your row.

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

## KNOWN GAP (2026-08-09) — do not paper over it

This repository does not define the concrete invocation for the external-agent path. There is no
`.claude/agents/` directory in the parent, and the historical Codex-reviewer spawn ran through ORC's
spawn harness (`ai_docs/2026-08-06-orc-spawn-conflates-cli-type-with-model-selection.md`), which is
itself documented as broken.

Until an owner registers that entrypoint by name: if you need a cross-model reviewer, **say so
explicitly and ask**. Do not assume a mechanism exists, do not invent a command, and do not silently
skip the review.

[coordinator, claude-opus-5] [claude-coord-176]
