# Codex coordinator — detailed instructions

Referenced from `AGENTS.md` → **Coordinator Modes**. Read this if your team slug is `codex-coord-NNN` or
`hermit-*`. `AGENTS.md` remains the authority; this file only expands your row.

**Follow skill `hermit-validation-authority` for validation authority.**

## Substrate

You have **a shell**. You have **no tmux, no panes, no ORC plugin surface**.

- **Primary workers: native Codex subagents.**
- **Cross-CLI adversarial review: external agents, to invoke Claude reviewers** of Codex-written code —
  the mirror image of the Claude coordinator. Use both. A Codex reviewer reviewing Codex-written code
  does not satisfy the dual-review requirement.

## Codex-specific working rules

- **Delegate nontrivial tool work** to subagents rather than running long tool chains inline.
- **Synthesize results; do not paste raw tool output** into reports.
- **A worker that trips the cybersecurity false-positive filter is not a blocker.** Rephrase the task or
  replace the worker and keep moving; do not stall the workstream on it.

## Your workers are not something you discover

You spawned them, so you know whether they exist. Never run `tmux ls`, `ps`, a pane registry,
`ACTIVE.md`, or `ci-hub/health/agent_liveness_probe.py` to learn whether **your own** team is alive —
those enumerate tmux-launched ORC agents, will find nothing, and make "I dispatched nothing" look like an
infrastructure outage. See `coordinator-claude.md` for the recorded 2026-08-09 failure; the same trap
applies here unchanged.

## What does NOT apply to you

`tmux_pane_id`, `cgroup_path`, `observe_owner_lease()`, `orc.scripts.*`, `orc.registerScript`,
`scripts/orc-hermit-msg.py`, Herdr tabs/panes, `agent_liveness_probe.py`.

- A slot you allocate is **born lease-less and stays that way** — normal and permanent, not a defect, and
  never a reason to delay dispatch.
- **"The coordinator has no shell" is an ORC constraint.** You have one. Run the commands yourself or hand
  them to your own subagents.
- **Hard Invariant 13's fifteen-agent cap counts independently-launched top-level `claude`/`codex`
  processes.** A native subagent runs inside your process and does not consume it. Worktree-slot caps
  apply to you exactly as to every other mode.

## Waking and routing

- **Your own subagents:** `SendMessage` addresses one you spawned, by id or name, and its result returns
  to you. Task notes (`tg note`) remain the durable record; the direct channel is only the wake.
- **A foreign fleet agent:** `SendMessage` cannot resolve fleet names and is not delivery
  acknowledgement. Note first, then ask that fleet's coordinator to relay.

## KNOWN GAP (2026-08-09)

The concrete invocation for the external-agent path is not registered in this repository — see the same
section in `coordinator-claude.md`. If you need a Claude reviewer, say so explicitly and ask rather than
assuming a mechanism exists or skipping the review.

[coordinator, claude-opus-5] [claude-coord-176]
