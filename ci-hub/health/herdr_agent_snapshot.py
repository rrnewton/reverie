#!/usr/bin/env python3
"""Publish a schema-checked snapshot from Herdr, agentctl, and TaskGraph proof.

The private binding pins each exact Herdr presentation, adopted agentctl
generation/native session/goal, and TaskGraph owner/current task. Every
agent-bearing pane in the workspace must be bound, including the coordinator.
Failure preserves the last snapshot so consumers see it age out rather than a
fresh, incomplete or reassigned fleet.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", ROOT)).resolve()
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
import agent_snapshot as agent_snapshot_contract  # noqa: E402
import bounded_capture  # noqa: E402


class SnapshotUnavailable(RuntimeError):
    """The Herdr fleet could not be proven; never publish a partial snapshot."""


@dataclass(frozen=True)
class Binding:
    herdr_name: str
    snapshot_name: str
    pane_id: str
    cwd: Path
    harness: str
    current_task: str | None
    role: str
    agentctl_name: str
    agentctl_token: str
    agentctl_session_id: str
    expected_goal: str | None


@dataclass(frozen=True)
class Bindings:
    workspace: str
    agentctl_registry: Path
    taskgraph_db: Path
    agents: tuple[Binding, ...]
    binding_sha256: str


STATUS_MAP = {
    "active": "active",
    "blocked": "needs-input",
    "busy": "busy",
    "dead": "dead",
    "done": "idle",
    "error": "error",
    "exited": "exited",
    "idle": "idle",
    "needs-input": "needs-input",
    "needs-permission": "needs-permission",
    "running": "running",
    "spawning": "spawning",
    "stopped": "terminated",
    "working": "working",
}
PANE = re.compile(r"w[A-Za-z0-9]+:p[A-Za-z0-9]+")
WORKSPACE = re.compile(r"w[A-Za-z0-9]+")
HARNESS = re.compile(r"[A-Za-z][A-Za-z0-9_-]*")


def _pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in items:
        if key in result:
            raise SnapshotUnavailable(f"duplicate JSON field {key!r}")
        result[key] = value
    return result


def _text(value: object, name: str) -> str:
    if not isinstance(value, str) or not value or value.strip() != value or "\0" in value:
        raise SnapshotUnavailable(f"{name} must be a nonempty trimmed string")
    return value


def load_bindings(path: Path) -> Bindings:
    try:
        raw_bytes = path.read_bytes()
        document = json.loads(raw_bytes.decode("utf-8"), object_pairs_hook=_pairs)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SnapshotUnavailable(f"cannot read binding file {path}: {error}") from error
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "workspace",
        "agentctl_registry",
        "taskgraph_db",
        "agents",
    }:
        raise SnapshotUnavailable(
            "binding file must contain exactly schema_version, workspace, "
            "agentctl_registry, taskgraph_db, and agents"
        )
    if document["schema_version"] != 2:
        raise SnapshotUnavailable("binding schema_version must be 2")
    workspace = _text(document["workspace"], "workspace")
    agentctl_registry = Path(
        _text(document["agentctl_registry"], "agentctl_registry")
    )
    taskgraph_db = Path(_text(document["taskgraph_db"], "taskgraph_db"))
    if not agentctl_registry.is_absolute() or not taskgraph_db.is_absolute():
        raise SnapshotUnavailable("agentctl_registry and taskgraph_db must be absolute")
    raw_agents = document["agents"]
    if not isinstance(raw_agents, list) or not raw_agents:
        raise SnapshotUnavailable("bindings agents must be a nonempty list")

    bindings: list[Binding] = []
    names: set[str] = set()
    snapshot_names: set[str] = set()
    panes: set[str] = set()
    agentctl_names: set[str] = set()
    required = {
        "herdr_name",
        "snapshot_name",
        "pane_id",
        "cwd",
        "harness",
        "current_task",
        "role",
        "agentctl_name",
        "agentctl_token",
        "agentctl_session_id",
        "expected_goal",
    }
    for index, raw_agent in enumerate(raw_agents):
        if not isinstance(raw_agent, dict) or set(raw_agent) != required:
            raise SnapshotUnavailable(
                f"binding {index} must contain exactly {sorted(required)}"
            )
        herdr_name = _text(raw_agent["herdr_name"], f"binding {index} herdr_name")
        snapshot_name = _text(
            raw_agent["snapshot_name"], f"binding {index} snapshot_name"
        )
        pane_id = _text(raw_agent["pane_id"], f"binding {index} pane_id")
        if PANE.fullmatch(pane_id) is None:
            raise SnapshotUnavailable(f"binding {index} pane_id is not a Herdr pane")
        cwd_text = _text(raw_agent["cwd"], f"binding {index} cwd")
        cwd = Path(cwd_text)
        if not cwd.is_absolute():
            raise SnapshotUnavailable(f"binding {index} cwd is not absolute")
        harness = _text(raw_agent["harness"], f"binding {index} harness")
        if HARNESS.fullmatch(harness) is None:
            raise SnapshotUnavailable(f"binding {index} harness is invalid")
        current = raw_agent.get("current_task")
        if current is not None:
            current = _text(current, f"binding {index} current_task")
        role = _text(raw_agent["role"], f"binding {index} role")
        if role not in {"coordinator", "worker"}:
            raise SnapshotUnavailable(f"binding {index} role must be coordinator or worker")
        agentctl_name = _text(raw_agent["agentctl_name"], f"binding {index} agentctl_name")
        agentctl_token = _text(raw_agent["agentctl_token"], f"binding {index} agentctl_token")
        agentctl_session_id = _text(
            raw_agent["agentctl_session_id"], f"binding {index} agentctl_session_id"
        )
        expected_goal = raw_agent["expected_goal"]
        if expected_goal is not None:
            expected_goal = _text(expected_goal, f"binding {index} expected_goal")
        if role == "worker" and (current is None or expected_goal is None):
            raise SnapshotUnavailable(
                f"binding {index} worker needs current_task and expected_goal"
            )
        if role == "coordinator" and (current is not None or expected_goal is not None):
            raise SnapshotUnavailable(
                f"binding {index} coordinator cannot assert a task or goal"
            )
        if (herdr_name in names or snapshot_name in snapshot_names or pane_id in panes
                or agentctl_name in agentctl_names):
            raise SnapshotUnavailable(
                f"binding {index} duplicates a Herdr name, snapshot name, pane, "
                "or agentctl name"
            )
        names.add(herdr_name)
        snapshot_names.add(snapshot_name)
        panes.add(pane_id)
        agentctl_names.add(agentctl_name)
        bindings.append(
            Binding(
                herdr_name,
                snapshot_name,
                pane_id,
                cwd.resolve(),
                harness,
                current,
                role,
                agentctl_name,
                agentctl_token,
                agentctl_session_id,
                expected_goal,
            )
        )
    if sum(binding.role == "coordinator" for binding in bindings) != 1:
        raise SnapshotUnavailable("bindings must contain exactly one coordinator")
    return Bindings(
        workspace,
        agentctl_registry.resolve(),
        taskgraph_db.resolve(),
        tuple(bindings),
        hashlib.sha256(raw_bytes).hexdigest(),
    )


def read_agentctl(
    agentctl_bin: Path, registry: Path, *arguments: str
) -> object:
    try:
        result = bounded_capture.capture(
            (str(agentctl_bin), "--registry", str(registry), *arguments), timeout=5
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SnapshotUnavailable(f"cannot read agentctl {' '.join(arguments)}: {error}") from error
    if result.returncode != 0:
        detail = " ".join((result.stderr or result.stdout).split())
        raise SnapshotUnavailable(
            f"agentctl {' '.join(arguments)} exited {result.returncode}: "
            f"{detail or 'no diagnostic'}"
        )
    if len(result.stdout.encode("utf-8")) > 4 * 1024 * 1024:
        raise SnapshotUnavailable("agentctl output exceeded 4 MiB")
    try:
        return json.loads(result.stdout, object_pairs_hook=_pairs)
    except json.JSONDecodeError as error:
        raise SnapshotUnavailable(f"agentctl output is not JSON: {error}") from error


def verify_taskgraph(bindings: Bindings) -> tuple[tuple[str, str], ...]:
    claimed = [binding for binding in bindings.agents if binding.current_task is not None]
    try:
        connection = sqlite3.connect(
            f"{bindings.taskgraph_db.as_uri()}?mode=ro", uri=True, timeout=5
        )
        try:
            connection.execute("PRAGMA query_only = ON")
            # Pin both questions to one SQLite read transaction. Otherwise an
            # owner can appear between the completeness query and the exact
            # assignment query while the resulting snapshot still looks fresh.
            connection.execute("BEGIN")
            owned_rows = connection.execute(
                "SELECT local_id, owner FROM tasks "
                "WHERE status = 'IN_PROGRESS' "
                "AND owner IS NOT NULL AND trim(owner) <> ''"
            ).fetchall()
            if claimed:
                placeholders = ",".join("?" for _ in claimed)
                rows = connection.execute(
                    f"SELECT local_id, status, owner FROM tasks "
                    f"WHERE local_id IN ({placeholders})",
                    tuple(binding.current_task for binding in claimed),
                ).fetchall()
            else:
                rows = []
        finally:
            connection.close()
    except (OSError, sqlite3.Error) as error:
        raise SnapshotUnavailable(f"cannot verify TaskGraph task: {error}") from error

    represented_owners = {
        binding.snapshot_name
        for binding in bindings.agents
        if binding.role == "worker"
    }
    uncovered = sorted(
        (str(task), str(owner))
        for task, owner in owned_rows
        if str(owner) not in represented_owners
    )
    if uncovered:
        detail = ", ".join(f"{task} owner={owner}" for task, owner in uncovered)
        raise SnapshotUnavailable(
            "TaskGraph has owned IN_PROGRESS work outside the complete bound "
            f"fleet: {detail}"
        )

    indexed: dict[str, list[tuple[str, str | None]]] = {}
    for task, status, owner in rows:
        indexed.setdefault(str(task), []).append((str(status), owner))
    for binding in claimed:
        observed = indexed.get(str(binding.current_task), [])
        if observed != [("IN_PROGRESS", binding.snapshot_name)]:
            raise SnapshotUnavailable(
                f"TaskGraph task {binding.current_task!r} is not exactly one "
                f"IN_PROGRESS row owned by {binding.snapshot_name!r}: {observed!r}"
            )
    return tuple(sorted((str(task), str(owner)) for task, owner in owned_rows))


def verify_agentctl(
    bindings: Bindings, agentctl_bin: Path
) -> dict[str, dict[str, Any]]:
    raw = read_agentctl(agentctl_bin, bindings.agentctl_registry, "list")
    if not isinstance(raw, list) or any(not isinstance(row, dict) for row in raw):
        raise SnapshotUnavailable("agentctl list is not an array of records")
    if any(not isinstance(row.get("name"), str) for row in raw):
        raise SnapshotUnavailable("agentctl list contains a record without a string name")
    records = {str(row["name"]): row for row in raw}
    expected_names = {binding.agentctl_name for binding in bindings.agents}
    if len(records) != len(raw) or set(records) != expected_names:
        raise SnapshotUnavailable(
            f"agentctl registry names are {sorted(str(name) for name in records)}, "
            f"expected {sorted(expected_names)}"
        )
    for binding in bindings.agents:
        row = records[binding.agentctl_name]
        expected = {
            "token": binding.agentctl_token,
            "adapter": "herdr-foreign",
            "lifecycle": "running",
            "harness": binding.harness,
            "cwd": str(binding.cwd),
            "pane_id": binding.pane_id,
            "workspace_id": binding.pane_id.split(":", 1)[0],
            "goal_session_id": binding.agentctl_session_id,
            "goal_command": ["codex", "app-server", "--stdio"],
            "probe_error": None,
        }
        for key, value in expected.items():
            if row.get(key) != value:
                raise SnapshotUnavailable(
                    f"agentctl agent {binding.agentctl_name!r} {key} is "
                    f"{row.get(key)!r}, expected {value!r}"
                )
    return records


def verify_agentctl_goals(bindings: Bindings, agentctl_bin: Path) -> None:
    for binding in bindings.agents:
        goal = read_agentctl(
            agentctl_bin,
            bindings.agentctl_registry,
            "goal",
            binding.agentctl_name,
        )
        if not isinstance(goal, dict):
            raise SnapshotUnavailable(
                f"agentctl goal for {binding.agentctl_name!r} is not an object"
            )
        native = goal.get("native")
        if binding.role == "worker" and (
            goal.get("source") != "native"
            or goal.get("native_status") != "active"
            or goal.get("goal") != binding.expected_goal
            or not isinstance(native, dict)
            or native.get("threadId") != binding.agentctl_session_id
            or native.get("status") != "active"
            or native.get("objective") != binding.expected_goal
        ):
            raise SnapshotUnavailable(
                f"agentctl goal for {binding.agentctl_name!r} does not prove "
                "the exact active objective"
            )
        if binding.role == "coordinator":
            native_status = goal.get("native_status")
            absent = (
                native_status == "absent"
                and goal.get("goal") is None
                and native is None
            )
            bound_session = (
                native_status in ("active", "paused")
                and isinstance(goal.get("goal"), str)
                and bool(goal.get("goal"))
                and isinstance(native, dict)
                and native.get("threadId") == binding.agentctl_session_id
                and native.get("status") == native_status
                and native.get("objective") == goal.get("goal")
            )
            if goal.get("source") != "native" or not (absent or bound_session):
                raise SnapshotUnavailable(
                    f"agentctl coordinator {binding.agentctl_name!r} native "
                    "goal state is not one exact absent, active, or paused session: "
                    f"source={goal.get('source')!r}, "
                    f"status={native_status!r}"
                )


def _agentctl_identity(records: dict[str, dict[str, Any]]) -> dict[str, tuple[object, ...]]:
    fields = (
        "token",
        "adapter",
        "lifecycle",
        "harness",
        "cwd",
        "workspace_id",
        "tab_id",
        "pane_id",
        "session_agent",
        "session_value",
        "goal_session_id",
        "goal_command",
    )
    return {
        name: tuple(record.get(field) for field in fields)
        for name, record in records.items()
    }


def collect_snapshot(
    bindings: Bindings,
    *,
    agentctl_bin: Path,
    herdr_bin: Path,
    captured_at: float | None = None,
) -> dict[str, Any]:
    """Bracket the composite fleet observation with one adopted-generation read."""
    # Stamp the start, never the end: publication after a slow collection must
    # not make its earliest evidence look younger than it is.
    observed_at = time.time() if captured_at is None else captured_at
    before = verify_agentctl(bindings, agentctl_bin)
    verify_agentctl_goals(bindings, agentctl_bin)
    taskgraph_owned = verify_taskgraph(bindings)
    herdr_document = read_herdr(herdr_bin)
    verify_agentctl_goals(bindings, agentctl_bin)
    after = verify_agentctl(bindings, agentctl_bin)
    if _agentctl_identity(before) != _agentctl_identity(after):
        raise SnapshotUnavailable("agentctl fleet identity changed during snapshot collection")
    return build_snapshot(
        bindings,
        herdr_document,
        captured_at=observed_at,
        taskgraph_owned=taskgraph_owned,
    )


def read_herdr(herdr_bin: Path) -> dict[str, Any]:
    try:
        result = bounded_capture.capture(
            (str(herdr_bin), "api", "snapshot"), timeout=15
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SnapshotUnavailable(f"cannot read Herdr snapshot: {error}") from error
    if result.returncode != 0:
        detail = " ".join((result.stderr or result.stdout).split())
        raise SnapshotUnavailable(
            f"Herdr snapshot exited {result.returncode}: {detail or 'no diagnostic'}"
        )
    if len(result.stdout.encode("utf-8")) > 4 * 1024 * 1024:
        raise SnapshotUnavailable("Herdr snapshot exceeded 4 MiB")
    try:
        document = json.loads(result.stdout, object_pairs_hook=_pairs)
    except json.JSONDecodeError as error:
        raise SnapshotUnavailable(f"Herdr snapshot is not JSON: {error}") from error
    if not isinstance(document, dict):
        raise SnapshotUnavailable("Herdr snapshot root is not an object")
    return document


def _mapping(value: object, name: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SnapshotUnavailable(f"{name} is not an object")
    return value


def _list(value: object, name: str) -> list[Any]:
    if not isinstance(value, list):
        raise SnapshotUnavailable(f"{name} is not a list")
    return value


def build_snapshot(
    bindings: Bindings,
    document: dict[str, Any],
    *,
    captured_at: float,
    taskgraph_owned: tuple[tuple[str, str], ...],
) -> dict[str, Any]:
    result = _mapping(document.get("result"), "Herdr result")
    snapshot = _mapping(result.get("snapshot"), "Herdr result.snapshot")
    workspaces = _list(snapshot.get("workspaces"), "Herdr workspaces")
    validated_workspaces: list[dict[str, Any]] = []
    workspace_ids: set[str] = set()
    for index, row in enumerate(workspaces):
        if not isinstance(row, dict):
            raise SnapshotUnavailable(
                f"Herdr workspace row {index} is not an object"
            )
        row_workspace_id = _text(
            row.get("workspace_id"), f"Herdr workspace row {index} workspace_id"
        )
        _text(row.get("label"), f"Herdr workspace row {index} label")
        if WORKSPACE.fullmatch(row_workspace_id) is None:
            raise SnapshotUnavailable(
                f"Herdr workspace row {index} has invalid workspace_id "
                f"{row_workspace_id!r}"
            )
        if row_workspace_id in workspace_ids:
            raise SnapshotUnavailable(
                f"Herdr workspace_id {row_workspace_id!r} is duplicated"
            )
        workspace_ids.add(row_workspace_id)
        validated_workspaces.append(row)
    workspace_matches = [
        row
        for row in validated_workspaces
        if row.get("label") == bindings.workspace
    ]
    if len(workspace_matches) != 1:
        raise SnapshotUnavailable(
            f"expected one Herdr workspace labelled {bindings.workspace!r}, "
            f"found {len(workspace_matches)}"
        )
    workspace_id = _text(
        workspace_matches[0].get("workspace_id"), "Herdr workspace_id"
    )
    live = _list(snapshot.get("agents"), "Herdr agents")
    validated_live: list[dict[str, Any]] = []
    for index, row in enumerate(live):
        if not isinstance(row, dict):
            raise SnapshotUnavailable(f"Herdr agent row {index} is not an object")
        row_workspace = row.get("workspace_id")
        if (
            not isinstance(row_workspace, str)
            or WORKSPACE.fullmatch(row_workspace) is None
            or row_workspace not in workspace_ids
        ):
            raise SnapshotUnavailable(
                f"Herdr agent row {index} has invalid or undeclared workspace_id "
                f"{row_workspace!r}"
            )
        validated_live.append(row)
    configured_names = {binding.herdr_name for binding in bindings.agents}
    workspace_rows = [
        row
        for row in validated_live
        if row.get("workspace_id") == workspace_id
    ]
    managed_rows: list[dict[str, Any]] = []
    for row in workspace_rows:
        harness = row.get("agent")
        # Herdr may report a plain shell pane in the agents array. It is
        # explicitly non-agent only when all three agent-bearing fields are
        # absent/null. A name or status paired with a missing/malformed harness
        # is an incomplete census, not a shell pane to filter away.
        if (
            harness is None
            and row.get("name") is None
            and row.get("agent_status") is None
        ):
            continue
        if not isinstance(harness, str) or not harness:
            raise SnapshotUnavailable(
                "configured workspace has malformed agent harness at pane "
                f"{row.get('pane_id')!r}: {harness!r}"
            )
        managed_rows.append(row)
    unnamed = [row.get("pane_id") for row in managed_rows if not row.get("name")]
    if unnamed:
        raise SnapshotUnavailable(
            "configured workspace has unnamed agent pane(s): "
            + ", ".join(str(value) for value in unnamed)
        )
    observed_names = {str(row["name"]) for row in managed_rows}
    unexpected = sorted(observed_names - configured_names)
    if unexpected:
        raise SnapshotUnavailable(
            "configured workspace has unbound named Herdr agent(s): "
            + ", ".join(unexpected)
        )
    output: list[dict[str, object]] = []
    for binding in bindings.agents:
        matches = [
            row
            for row in managed_rows
            if row.get("name") == binding.herdr_name
        ]
        if len(matches) != 1:
            same_name = [
                row
                for row in validated_live
                if row.get("name") == binding.herdr_name
            ]
            if not matches and len(same_name) == 1:
                raise SnapshotUnavailable(
                    f"Herdr agent {binding.herdr_name!r} workspace_id is "
                    f"{same_name[0].get('workspace_id')!r}, expected {workspace_id!r}"
                )
            raise SnapshotUnavailable(
                f"expected one live Herdr agent named {binding.herdr_name!r}, "
                f"found {len(matches)}"
            )
        row = matches[0]
        checks = {
            "pane_id": binding.pane_id,
            "workspace_id": workspace_id,
            "agent": binding.harness,
        }
        for key, expected in checks.items():
            if row.get(key) != expected:
                raise SnapshotUnavailable(
                    f"Herdr agent {binding.herdr_name!r} {key} is {row.get(key)!r}, "
                    f"expected {expected!r}"
                )
        cwd = row.get("cwd")
        if not isinstance(cwd, str) or Path(cwd).resolve() != binding.cwd:
            raise SnapshotUnavailable(
                f"Herdr agent {binding.herdr_name!r} cwd is {cwd!r}, "
                f"expected {str(binding.cwd)!r}"
            )
        raw_status = row.get("agent_status")
        status = STATUS_MAP.get(raw_status) if isinstance(raw_status, str) else None
        if status is None:
            raise SnapshotUnavailable(
                f"Herdr agent {binding.herdr_name!r} has unsupported status {raw_status!r}"
            )
        agent: dict[str, object] = {
            "name": binding.snapshot_name,
            "status": status,
            "herdr_pane_id": binding.pane_id,
            "herdr_name": binding.herdr_name,
            "herdr_workspace": bindings.workspace,
            "harness": binding.harness,
            "role": binding.role,
            "source": "herdr-agentctl-snapshot-v2",
        }
        if binding.current_task is not None:
            agent["current_task"] = binding.current_task
        output.append(agent)
    built = {
        "schema_version": 1,
        "captured_at": captured_at,
        "agents": output,
        "authority": {
            "source": "herdr-agentctl-snapshot-v2",
            "taskgraph_db": str(bindings.taskgraph_db),
            "binding_sha256": bindings.binding_sha256,
            "owned_in_progress": [
                {"local_id": task, "owner": owner}
                for task, owner in taskgraph_owned
            ],
        },
    }
    encoded = (json.dumps(built, sort_keys=True) + "\n").encode("utf-8")
    try:
        agent_snapshot_contract.parse_snapshot_bytes(encoded)
    except agent_snapshot_contract.AgentSnapshotSchemaError as error:
        raise SnapshotUnavailable(f"generated agent snapshot was refused: {error}") from error
    return built


def persist(path: Path, document: dict[str, Any]) -> None:
    encoded = (json.dumps(document, sort_keys=True) + "\n").encode("utf-8")
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    descriptor = os.open(
        temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600
    )
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def field(name: str, value: object) -> None:
    if name == "summary":
        value = str(value).replace("{", "(").replace("}", ")")
    print(f"{name}={value}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=STATE_ROOT)
    parser.add_argument("--bindings", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--herdr-bin", type=Path, default=Path("herdr"))
    parser.add_argument("--agentctl-bin", type=Path, default=Path("agentctl"))
    parser.add_argument("--gate", action="store_true")
    parser.add_argument(
        "--check", action="store_true", help="validate live identity without publishing"
    )
    args = parser.parse_args(argv)
    root = args.root.expanduser().resolve()
    bindings_path = args.bindings or root / ".tick-hub/herdr-agent-bindings.json"
    output_path = args.output or root / "ignored/ci-hub/agent-snapshot.json"
    try:
        bindings = load_bindings(bindings_path)
        document = collect_snapshot(
            bindings,
            agentctl_bin=args.agentctl_bin,
            herdr_bin=args.herdr_bin,
        )
        if not args.check:
            persist(output_path, document)
    except (SnapshotUnavailable, OSError) as error:
        field("state", "unknown")
        field("summary", f"Herdr agent snapshot unavailable: {' '.join(str(error).split())}")
        return 75 if args.gate else 2
    field("state", "ok")
    field("agents", len(document["agents"]))
    field(
        "summary",
        f"validated {len(document['agents'])} exact Herdr agent binding(s)"
        + (" without publishing" if args.check else " and published a fresh snapshot"),
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
