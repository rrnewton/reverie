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
`ignored/ci-hub/`. The exact invocation matters because coordinator substrates
can reuse agent names; seeing a new `hermit-238b` does not prove it owns
containers created by an older `hermit-238b` process.

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
comment above `agent_container_lifecycle` in `ci-hub/health/tick-hub.yaml`). The
current snapshot is instead published from exact Herdr pane identities by
`hermit-herdr-agent-snapshot.timer` every minute; consumers still enforce their
own freshness bounds and must fail closed when that evidence is absent.

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

## Retired ORC bridge

Do not launch `ci-hub/health/agent-container-reconcile-bridge.rs`. It is a
legacy migration bridge retained for historical compatibility and tests, not a
current producer. Herdr pane identity plus `.tick-hub/herdr-agent-bindings.json`
is the current census authority, and `hermit-herdr-agent-snapshot.timer`
publishes its snapshot. The bridge detects that binding marker and exits before
writing or reconciling, but running a second producer is still an operational
error rather than a fallback.

An old transient bridge should therefore be absent or inactive. Inspect it
without mutation with:

```bash
systemctl --user status dev-hermit-agent-container-reconcile-bridge.service
```
