# ORC coordinator — detailed instructions

Referenced from `AGENTS.md` → **Coordinator Modes**. Read this if your team slug is `orc-coord-NNN`.
`AGENTS.md` remains the authority; this file only expands your row.

**Follow skill `hermit-validation-authority` for validation authority.**

## Substrate

You launch **every** Claude/Codex agent **inside tmux sessions you own**, driven by your own management
mechanisms. You do **not** use the external-subagent scripts that the Claude and Codex coordinators use —
cross-CLI adversarial review goes through the same tmux mechanism as ordinary dispatch.

**You have NO SHELL.** Your only effects are `orc.*` plus scripts pre-registered via `orc.registerScript`
(today: `hermitOperationalTick`, `hermitVerifyAgentDeath`, and the inert `hermitEchoScriptArgv` diagnostic).

Consequence: **every coordinator duty in `AGENTS.md` means "ensure this outcome and require the evidence
back", never "personally type this command."** That gap — not disobedience — jammed task closure until
2026-08-08. To make a command routinely ORC-runnable, register it on the plugin surface.

This constraint is **yours alone**. Claude and Codex coordinators have a shell and may not cite it.

## Mechanisms that are yours (and only yours)

`tmux_pane_id`, `cgroup_path`, `observe_owner_lease()`, `orc.scripts.*`, `orc.registerScript`,
`scripts/orc-hermit-msg.py`, Herdr tabs/panes, `ci-hub/health/agent_liveness_probe.py`.

An absent pane is meaningful **only** for an agent you tmux-launched. It says nothing about another
coordinator's native subagent, and you should not report one as dead on that basis.

## Slot leases

A slot allocated before its agent exists is **born lease-less, and that is normal** —
`observe_owner_lease()` has no live pane to bind, so the allocator warns and proceeds (54% of live slots
on 2026-08-08).

**Repair by ADOPTION, never release-then-reallocate** — that collides slots and has already broken one.
On arrival the living owner re-runs the exact `allocate-worktree.rs` line, no flags, and the row gains
`tmux_pane_id` + `cgroup_path`. Leases never expire, so an unadopted row stays un-`--clean`-able once its
owner dies; a plain release without `--clean` still deregisters it, and only disk reclaim is blocked.

## Verify Before You Replace (Hard Invariant 16)

An alarm is a **trigger to check**, never a **warrant to act**. The mission mandate removes the need for
PERMISSION to replace an agent; it does not remove the need for EVIDENCE.

```
orc.scripts.hermitVerifyAgentDeath("<name>")   # python3 ci-hub/health/agent_liveness_probe.py <name>
```

rc 0 verified-dead → replace, promptly. **rc 1 (alive) and rc 2 (unverifiable) BOTH REFUSE** — failing to
prove life is not proving death. The probe reads `DG_AGENT_NAME` from `/proc`, independent of ORC, the
agent snapshot, and tick-hub; it never kills; and if it can see no agent at all it returns 2 rather than
declaring the whole fleet dead.

**Why an independent source, not a fresher one.** Measured 2026-08-08: `/proc` held **29 live agents while
the snapshot health-tick listed 13** — it cannot see other ORC sessions' agents, so repairing its contents
cannot fix this. That same day the tick called `fleet-forensics` dead while it held the freshest activity
timestamp on the box; obeying the mandate literally would have destroyed a live agent mid-P0.

## Waking and routing

Relay through `scripts/orc-hermit-msg.py`. Task notes (`tg note`) are the durable record and do not wake.
To reach another fleet's agent, note first, then ask that fleet's coordinator to relay — never claim
delivery from an attempt.

[coordinator, claude-opus-5] [claude-coord-176]
