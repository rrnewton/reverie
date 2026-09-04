# Agent-owned Podman containers

An agent can disappear while its Podman container keeps running. The container
then continues holding processes, PID namespaces, mounts, and storage without
any live agent being responsible for it. `scripts/agent-podman.rs` prevents that
for new containers and reports older containers that lack ownership metadata.

Run the pure primer before using the tool:

```bash
./scripts/agent-podman.rs quickstart
```

## Ownership contract

Agents must create Podman containers through this wrapper, including foreground
`--rm` commands. `--rm` alone is insufficient: a hung command never exits, so
Podman never reaches its automatic removal step.

```bash
./scripts/agent-podman.rs run --task TASK -- --rm IMAGE COMMAND
```

The wrapper records the agent name, exact 3pai invocation, task, pane, and
lifetime as Podman labels. It also writes a transferable ownership record under
`ignored/ci-hub/`. The exact invocation matters because ORC reuses agent names;
seeing a new `hermit-238b` does not prove it owns containers created by an older
`hermit-238b` process.

The default `agent` lifetime means the operational tick may gracefully stop and
remove the container after that exact agent invocation disappears. There is no
force-remove fallback. A failed graceful cleanup is a hard warning.

Use `task` lifetime only for a deliberately retained environment:

```bash
./scripts/agent-podman.rs run --task TASK --lifetime task -- -d --name NAME IMAGE sleep infinity
```

When responsibility changes, the new live owner claims it explicitly:

```bash
./scripts/agent-podman.rs transfer NAME --task TASK --lifetime task
```

A task-retained container whose creator is gone is reported as
`transfer-required`; it is never deleted automatically. A legacy unlabelled
container is also report-only, even when idle or zombie-bearing. Idle state is
not evidence that an environment is abandoned.

## Reconciliation

The operational tick runs every thirty minutes, but this hourly reminder runs
the NON-applying form on every other tick. `tick-hub.yaml` invokes exactly this,
and it carries no `--apply`:

```bash
./scripts/agent-podman.rs reconcile --agents-json ignored/ci-hub/agent-snapshot.json
```

⚠️ THIS PARAGRAPH USED TO SAY THE TICK RAN "the applying form against its fresh
ORC agent snapshot", above a copy-pasteable `--apply` command. BOTH HALVES WERE
FALSE, and together they invited a reader to paste a destructive command on the
strength of two untrue claims. `--apply` was withdrawn from the tick (see the
comment above `agent_container_lifecycle` in `ci-hub/health/tick-hub.yaml`), and
the snapshot is not fresh -- measured 2026-08-26 it was over five hours old,
because its only producer, `ci-hub/health/agent-container-reconcile-bridge.rs`,
is scheduled by nothing.

The applying form still exists and is still the right tool for reclaiming leaked
containers, but run it deliberately rather than because this page said the tick
does:

```bash
./scripts/agent-podman.rs reconcile --agents-json <a snapshot you trust> --apply
```

Use `audit` for a read-only inventory. Both commands state which containers are
live, reclaimable, awaiting transfer, owner-unknown, or legacy/unmanaged, plus
the zombie count and PID namespace when available.

```bash
./scripts/agent-podman.rs audit --agents-json ignored/ci-hub/agent-snapshot.json
```

The reconciler refuses to act on a stale agent snapshot. If a live agent's exact
invocation cannot be established, the result is `owner-unknown` and no removal
occurs. These fail-closed cases distinguish “safe to clean” from “looks idle.”

## Bridging an old ORC process

The canonical scheduler is the ORC workflow
`hermit-dev-operational-health-v1`. An ORC process started before that workflow
was added cannot load it without a restart. Do not restart a live fleet solely
for this poll. Start the bounded bridge as a user service instead:

```bash
systemd-run --user --unit=dev-hermit-agent-container-reconcile-bridge \
  --property=Restart=on-failure --property=RestartSec=15s \
  --setenv=PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
  --working-directory="$HOME/work/dev-hermit" \
  "$HOME/work/dev-hermit/ci-hub/health/agent-container-reconcile-bridge.rs"
```

The bridge refreshes the same agent snapshot and invokes the same reconciler.
It survives agent recycling because systemd owns it. It exits successfully as
soon as the canonical workflow is observed alive, so a later ORC restart cannot
leave two schedulers running. Inspect it without mutation with:

```bash
systemctl --user status dev-hermit-agent-container-reconcile-bridge.service
```
