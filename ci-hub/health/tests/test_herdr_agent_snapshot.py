from __future__ import annotations

import importlib.util
import hashlib
import json
from pathlib import Path
import sqlite3
import sys

import pytest


HEALTH = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "herdr_agent_snapshot", HEALTH / "herdr_agent_snapshot.py"
)
assert SPEC is not None and SPEC.loader is not None
producer = importlib.util.module_from_spec(SPEC)
sys.modules["herdr_agent_snapshot"] = producer
SPEC.loader.exec_module(producer)

sys.path.insert(0, str(HEALTH))
import operational_health  # noqa: E402


def binding(
    tmp_path: Path,
    *,
    current_task: str | None = "kvm_backend_emits_no",
    agentctl_adapter: str = "herdr-foreign",
) -> producer.Bindings:
    return producer.Bindings(
        "chat-bridge",
        (tmp_path / "agentctl").resolve(),
        (tmp_path / "taskgraph.db").resolve(),
        (
            producer.Binding(
                "kvm",
                "dev-hermit-14",
                "wK:p3",
                tmp_path.resolve(),
                "codex",
                current_task,
                "worker",
                "kvm",
                agentctl_adapter,
                "generation-token",
                "native-session",
                "Drive the KVM lane",
            ),
        ),
        hashlib.sha256(b"fixture-binding").hexdigest(),
    )


def herdr(tmp_path: Path, **agent_changes) -> dict:
    agent = {
        "name": "kvm",
        "pane_id": "wK:p3",
        "workspace_id": "wK",
        "cwd": str(tmp_path.resolve()),
        "agent": "codex",
        "agent_status": "done",
    }
    agent.update(agent_changes)
    return {
        "result": {
            "snapshot": {
                "workspaces": [
                    {"workspace_id": "wK", "label": "chat-bridge"}
                ],
                "agents": [agent],
            }
        }
    }


def owned() -> tuple[tuple[str, str], ...]:
    return (("kvm_backend_emits_no", "dev-hermit-14"),)


def bindings_document(
    tmp_path: Path,
    *,
    schema_version: int = 3,
    worker_current_task: str | None = "kvm_backend_emits_no",
    worker_adapter: str = "herdr-foreign",
) -> dict:
    worker: dict[str, object] = {
        "herdr_name": "kvm",
        "snapshot_name": "dev-hermit-14",
        "pane_id": "wK:p3",
        "cwd": str(tmp_path),
        "harness": "codex",
        "current_task": worker_current_task,
        "role": "worker",
        "agentctl_name": "kvm",
        "agentctl_token": "generation-token",
        "agentctl_session_id": "native-session",
        "expected_goal": "Drive the KVM lane",
    }
    coordinator: dict[str, object] = {
        "herdr_name": "hermit-coordinator",
        "snapshot_name": "hermit-coordinator",
        "pane_id": "wK:p1",
        "cwd": str(tmp_path),
        "harness": "codex",
        "current_task": None,
        "role": "coordinator",
        "agentctl_name": "coordinator",
        "agentctl_token": "coordinator-token",
        "agentctl_session_id": "coordinator-session",
        "expected_goal": None,
    }
    if schema_version == 3:
        worker["agentctl_adapter"] = worker_adapter
        coordinator["agentctl_adapter"] = "herdr-foreign"
    return {
        "schema_version": schema_version,
        "workspace": "chat-bridge",
        "agentctl_registry": str(tmp_path / "agentctl"),
        "taskgraph_db": str(tmp_path / "taskgraph.db"),
        "agents": [worker, coordinator],
    }


def test_exact_herdr_identity_becomes_a_schema_checked_claim_snapshot(
    tmp_path: Path,
) -> None:
    bindings = binding(tmp_path)
    result = producer.build_snapshot(
        bindings, herdr(tmp_path), captured_at=10, taskgraph_owned=owned()
    )
    assert result == {
        "schema_version": 1,
        "captured_at": 10,
        "agents": [
            {
                "name": "dev-hermit-14",
                "status": "idle",
                "current_task": "kvm_backend_emits_no",
                "herdr_pane_id": "wK:p3",
                "herdr_name": "kvm",
                "herdr_workspace": "chat-bridge",
                "harness": "codex",
                "role": "worker",
                "source": "herdr-agentctl-snapshot-v2",
            }
        ],
        "authority": {
            "source": "herdr-agentctl-snapshot-v2",
            "taskgraph_db": str(bindings.taskgraph_db),
            "binding_sha256": bindings.binding_sha256,
            "owned_in_progress": [
                {
                    "local_id": "kvm_backend_emits_no",
                    "owner": "dev-hermit-14",
                }
            ],
        },
    }


@pytest.mark.parametrize(
    ("change", "message"),
    [
        ({"pane_id": "wK:p9"}, "pane_id"),
        ({"workspace_id": "wrong"}, "workspace_id"),
        ({"cwd": "/wrong"}, "cwd"),
        ({"agent": "claude"}, "agent"),
        ({"agent_status": "mystery"}, "unsupported status"),
    ],
)
def test_identity_or_status_mismatch_refuses_the_whole_snapshot(
    tmp_path: Path, change: dict[str, str], message: str,
) -> None:
    with pytest.raises(producer.SnapshotUnavailable, match=message):
        producer.build_snapshot(
            binding(tmp_path),
            herdr(tmp_path, **change),
            captured_at=10,
            taskgraph_owned=owned(),
        )


def test_binding_file_rejects_duplicate_delivery_and_claim_identities(
    tmp_path: Path,
) -> None:
    path = tmp_path / "bindings.json"
    row = {
        "herdr_name": "kvm",
        "snapshot_name": "dev-hermit-14",
        "pane_id": "wK:p3",
        "cwd": str(tmp_path),
        "harness": "codex",
        "current_task": "kvm_backend_emits_no",
        "role": "worker",
        "agentctl_name": "kvm",
        "agentctl_adapter": "herdr-foreign",
        "agentctl_token": "generation-token",
        "agentctl_session_id": "native-session",
        "expected_goal": "Drive the KVM lane",
    }
    path.write_text(
        json.dumps(
            {
                "schema_version": 3,
                "workspace": "chat-bridge",
                "agentctl_registry": str(tmp_path / "agentctl"),
                "taskgraph_db": str(tmp_path / "taskgraph.db"),
                "agents": [row, {**row, "herdr_name": "second"}],
            }
        )
    )
    with pytest.raises(producer.SnapshotUnavailable, match="duplicates"):
        producer.load_bindings(path)


def test_schema_v3_binds_managed_adapter_and_allows_taskless_worker(
    tmp_path: Path,
) -> None:
    path = tmp_path / "bindings.json"
    path.write_text(
        json.dumps(
            bindings_document(
                tmp_path,
                worker_current_task=None,
                worker_adapter="herdr",
            )
        )
    )

    loaded = producer.load_bindings(path)
    worker = next(binding for binding in loaded.agents if binding.role == "worker")
    assert worker.agentctl_adapter == "herdr"
    assert worker.current_task is None
    assert worker.expected_goal == "Drive the KVM lane"


def test_schema_v2_remains_foreign_only_and_requires_a_worker_task(
    tmp_path: Path,
) -> None:
    path = tmp_path / "bindings.json"
    document = bindings_document(tmp_path, schema_version=2)
    path.write_text(json.dumps(document))
    loaded = producer.load_bindings(path)
    worker = next(binding for binding in loaded.agents if binding.role == "worker")
    assert worker.agentctl_adapter == "herdr-foreign"

    document["agents"][0]["current_task"] = None
    path.write_text(json.dumps(document))
    with pytest.raises(producer.SnapshotUnavailable, match="needs current_task"):
        producer.load_bindings(path)


def test_schema_v3_refuses_an_unbound_adapter_field(tmp_path: Path) -> None:
    path = tmp_path / "bindings.json"
    document = bindings_document(tmp_path)
    del document["agents"][0]["agentctl_adapter"]
    path.write_text(json.dumps(document))
    with pytest.raises(producer.SnapshotUnavailable, match="must contain exactly"):
        producer.load_bindings(path)


def test_schema_v3_refuses_a_non_herdr_agentctl_adapter(tmp_path: Path) -> None:
    path = tmp_path / "bindings.json"
    document = bindings_document(tmp_path, worker_adapter="turn-runner")
    path.write_text(json.dumps(document))
    with pytest.raises(producer.SnapshotUnavailable, match="must be one of"):
        producer.load_bindings(path)


def test_failed_refresh_preserves_the_last_snapshot(
    tmp_path: Path, monkeypatch, capsys,
) -> None:
    bindings = tmp_path / "bindings.json"
    bindings.write_text(
        json.dumps(
            {
                "schema_version": 3,
                "workspace": "chat-bridge",
                "agentctl_registry": str(tmp_path / "agentctl"),
                "taskgraph_db": str(tmp_path / "taskgraph.db"),
                "agents": [
                    {
                        "herdr_name": "kvm",
                        "snapshot_name": "dev-hermit-14",
                        "pane_id": "wK:p3",
                        "cwd": str(tmp_path),
                        "harness": "codex",
                        "current_task": "kvm_backend_emits_no",
                        "role": "worker",
                        "agentctl_name": "kvm",
                        "agentctl_adapter": "herdr-foreign",
                        "agentctl_token": "generation-token",
                        "agentctl_session_id": "native-session",
                        "expected_goal": "Drive the KVM lane",
                    },
                    {
                        "herdr_name": "hermit-coordinator",
                        "snapshot_name": "hermit-coordinator",
                        "pane_id": "wK:p1",
                        "cwd": str(tmp_path),
                        "harness": "codex",
                        "current_task": None,
                        "role": "coordinator",
                        "agentctl_name": "coordinator",
                        "agentctl_adapter": "herdr-foreign",
                        "agentctl_token": "coordinator-token",
                        "agentctl_session_id": "coordinator-session",
                        "expected_goal": None,
                    }
                ],
            }
        )
    )
    output = tmp_path / "snapshot.json"
    output.write_text("last-known-snapshot\n")
    def unavailable(*_args, **_kwargs):
        raise producer.SnapshotUnavailable("planted identity mismatch")

    monkeypatch.setattr(producer, "collect_snapshot", unavailable)
    code = producer.main(
        [
            "--root",
            str(tmp_path),
            "--bindings",
            str(bindings),
            "--output",
            str(output),
            "--gate",
        ]
    )
    assert code == 75
    assert output.read_text() == "last-known-snapshot\n"
    assert "state=unknown" in capsys.readouterr().out


def test_persist_publishes_private_validated_bytes(tmp_path: Path) -> None:
    document = producer.build_snapshot(
        binding(tmp_path),
        herdr(tmp_path),
        captured_at=10,
        taskgraph_owned=owned(),
    )
    output = tmp_path / "state/snapshot.json"
    producer.persist(output, document)
    assert json.loads(output.read_text()) == document
    assert output.stat().st_mode & 0o777 == 0o600


def test_unbound_named_agent_in_managed_workspace_refuses_incomplete_census(
    tmp_path: Path,
) -> None:
    document = herdr(tmp_path, agent_status="working")
    document["result"]["snapshot"]["agents"].append(
        {
            "name": "surprise-worker",
            "pane_id": "wK:p9",
            "workspace_id": "wK",
            "cwd": str(tmp_path),
            "agent": "codex",
            "agent_status": "working",
        }
    )
    with pytest.raises(producer.SnapshotUnavailable, match="unbound named"):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


def test_coordinator_cannot_be_exempted_from_the_complete_workspace_census(
    tmp_path: Path,
) -> None:
    document = herdr(tmp_path, agent_status="working")
    document["result"]["snapshot"]["agents"].append(
        {
            "name": "hermit-coordinator",
            "pane_id": "wK:p1",
            "workspace_id": "wK",
            "cwd": str(tmp_path),
            "agent": "codex",
            "agent_status": "working",
        }
    )
    with pytest.raises(producer.SnapshotUnavailable, match="unbound named"):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


def test_unnamed_agent_in_managed_workspace_refuses_incomplete_census(
    tmp_path: Path,
) -> None:
    document = herdr(tmp_path)
    document["result"]["snapshot"]["agents"].append(
        {
            "pane_id": "wK:p9",
            "workspace_id": "wK",
            "cwd": str(tmp_path),
            "agent": "codex",
            "agent_status": "working",
        }
    )
    with pytest.raises(producer.SnapshotUnavailable, match="unnamed agent"):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


def test_named_row_with_malformed_harness_refuses_incomplete_census(
    tmp_path: Path,
) -> None:
    document = herdr(tmp_path)
    document["result"]["snapshot"]["agents"].append(
        {
            "name": "surprise-worker",
            "pane_id": "wK:p9",
            "workspace_id": "wK",
            "cwd": str(tmp_path),
            "agent": None,
            "agent_status": "working",
        }
    )
    with pytest.raises(producer.SnapshotUnavailable, match="malformed agent harness"):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


@pytest.mark.parametrize(
    "surprise",
    [
        {"name": "surprise", "agent": "codex", "agent_status": "working"},
        "not-an-agent-object",
    ],
)
def test_malformed_or_unscoped_herdr_row_refuses_complete_census(
    tmp_path: Path, surprise: object,
) -> None:
    document = herdr(tmp_path)
    document["result"]["snapshot"]["agents"].append(surprise)
    with pytest.raises(
        producer.SnapshotUnavailable, match="not an object|invalid.*workspace_id"
    ):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


@pytest.mark.parametrize(
    "surprise, message",
    [
        ("not-a-workspace-object", "workspace row 1 is not an object"),
        ({"workspace_id": "wZ"}, "workspace row 1 label"),
        ({"workspace_id": "bad:id", "label": "other"}, "invalid workspace_id"),
        ({"workspace_id": "wK", "label": "other"}, "workspace_id 'wK' is duplicated"),
    ],
)
def test_malformed_workspace_row_refuses_complete_census(
    tmp_path: Path, surprise: object, message: str
) -> None:
    document = herdr(tmp_path)
    document["result"]["snapshot"]["workspaces"].append(surprise)
    with pytest.raises(producer.SnapshotUnavailable, match=message):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


def test_same_agent_name_in_other_workspace_does_not_poison_bound_match(
    tmp_path: Path,
) -> None:
    document = herdr(tmp_path)
    document["result"]["snapshot"]["workspaces"].append(
        {"workspace_id": "wZ", "label": "other-project"}
    )
    document["result"]["snapshot"]["agents"].append(
        {
            "name": "kvm",
            "pane_id": "wZ:p9",
            "workspace_id": "wZ",
            "cwd": "/other/project",
            "agent": "codex",
            "agent_status": "working",
        }
    )
    produced = producer.build_snapshot(
        binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
    )
    assert [row["herdr_pane_id"] for row in produced["agents"]] == ["wK:p3"]


def test_agent_row_in_undeclared_workspace_refuses_complete_census(
    tmp_path: Path,
) -> None:
    document = herdr(tmp_path)
    document["result"]["snapshot"]["agents"].append(
        {
            "name": "outside-worker",
            "pane_id": "wZ:p9",
            "workspace_id": "wZ",
            "cwd": "/other/project",
            "agent": "codex",
            "agent_status": "working",
        }
    )
    with pytest.raises(producer.SnapshotUnavailable, match="undeclared workspace_id"):
        producer.build_snapshot(
            binding(tmp_path), document, captured_at=10, taskgraph_owned=owned()
        )


def test_producer_roles_keep_taskless_coordinator_out_of_worker_drift(
    tmp_path: Path,
) -> None:
    worker_bindings = binding(tmp_path)
    coordinator = producer.Binding(
        "hermit-coordinator",
        "hermit-coordinator",
        "wK:p1",
        tmp_path.resolve(),
        "codex",
        None,
        "coordinator",
        "coordinator",
        "herdr-foreign",
        "coordinator-token",
        "coordinator-session",
        None,
    )
    bindings = producer.Bindings(
        worker_bindings.workspace,
        worker_bindings.agentctl_registry,
        worker_bindings.taskgraph_db,
        (coordinator, *worker_bindings.agents),
        worker_bindings.binding_sha256,
    )
    document = herdr(tmp_path, agent_status="working")
    document["result"]["snapshot"]["agents"].append(
        {
            "name": "hermit-coordinator",
            "pane_id": "wK:p1",
            "workspace_id": "wK",
            "cwd": str(tmp_path),
            "agent": "codex",
            "agent_status": "working",
        }
    )
    produced = producer.build_snapshot(
        bindings, document, captured_at=10, taskgraph_owned=owned()
    )
    agents = operational_health._parse_agents(produced["agents"])
    report = operational_health.reconcile_active_work(
        (
            operational_health.TaskRecord(
                "kvm_backend_emits_no", "KVM parity", "dev-hermit-14", ()
            ),
        ),
        agents,
    )

    assert {row["role"] for row in produced["agents"]} == {
        "coordinator",
        "worker",
    }
    assert [task.id for task in report.claimed] == ["kvm_backend_emits_no"]
    assert report.off_book == ()
    assert report.misrouted == ()


def taskgraph(
    tmp_path: Path,
    *,
    owner: str = "dev-hermit-14",
    include_task: bool = True,
) -> Path:
    path = tmp_path / "taskgraph.db"
    connection = sqlite3.connect(path)
    connection.execute(
        "CREATE TABLE tasks (local_id TEXT PRIMARY KEY, status TEXT, owner TEXT)"
    )
    if include_task:
        connection.execute(
            "INSERT INTO tasks VALUES (?, ?, ?)",
            ("kvm_backend_emits_no", "IN_PROGRESS", owner),
        )
    connection.commit()
    connection.close()
    return path


def agentctl_list(
    tmp_path: Path,
    *,
    token: str = "generation-token",
    adapter: str = "herdr-foreign",
) -> list[dict]:
    return [
        {
            "name": "kvm",
            "token": token,
            "adapter": adapter,
            "lifecycle": "running",
            "harness": "codex",
            "cwd": str(tmp_path.resolve()),
            "pane_id": "wK:p3",
            "workspace_id": "wK",
            "tab_id": "wK:t3",
            "session_agent": None,
            "session_value": None,
            "goal_session_id": "native-session",
            "goal_command": ["codex", "app-server", "--stdio"],
            "probe_error": None,
        }
    ]


def coordinator_binding(tmp_path: Path) -> producer.Bindings:
    return producer.Bindings(
        "chat-bridge",
        (tmp_path / "registry").resolve(),
        (tmp_path / "taskgraph.db").resolve(),
        (
            producer.Binding(
                "hermit-coordinator",
                "hermit-coordinator",
                "wK:p1",
                tmp_path.resolve(),
                "codex",
                None,
                "coordinator",
                "coordinator",
                "herdr-foreign",
                "coordinator-token",
                "coordinator-session",
                None,
            ),
        ),
        hashlib.sha256(b"coordinator-binding").hexdigest(),
    )


def native_goal(
    session_id: str,
    objective: str,
    status: str,
    *,
    source: str = "native",
) -> dict[str, object]:
    return {
        "source": source,
        "native_status": status,
        "goal": objective,
        "native": {
            "threadId": session_id,
            "status": status,
            "objective": objective,
        },
    }


@pytest.mark.parametrize(
    "goal",
    [
        {
            "source": "native",
            "native_status": "absent",
            "goal": None,
            "native": None,
        },
        native_goal("coordinator-session", "Coordinate Hermit", "active"),
        native_goal("coordinator-session", "Coordinate Hermit", "blocked"),
        native_goal("coordinator-session", "Coordinate Hermit", "paused"),
    ],
)
def test_coordinator_accepts_only_an_exact_bound_native_session(
    tmp_path: Path, monkeypatch, goal: dict[str, object],
) -> None:
    monkeypatch.setattr(producer, "read_agentctl", lambda *_args: goal)
    producer.verify_agentctl_goals(coordinator_binding(tmp_path), Path("agentctl"))


@pytest.mark.parametrize(
    "goal",
    [
        {
            "source": "requested",
            "native_status": "unverified",
            "goal": None,
            "native": None,
        },
        native_goal("replacement-session", "Coordinate Hermit", "active"),
        {
            "source": "native",
            "native_status": "active",
            "goal": "Coordinate Hermit",
            "native": {
                "threadId": "coordinator-session",
                "objective": "Coordinate Hermit",
            },
        },
        {
            **native_goal("coordinator-session", "Coordinate Hermit", "active"),
            "goal": "Replacement objective",
        },
        native_goal("coordinator-session", "Coordinate Hermit", "complete"),
        native_goal("coordinator-session", "Coordinate Hermit", "usageLimited"),
        native_goal("coordinator-session", "Coordinate Hermit", "budgetLimited"),
        {
            "source": "native",
            "native_status": [],
            "goal": "Coordinate Hermit",
            "native": {
                "threadId": "coordinator-session",
                "status": "active",
                "objective": "Coordinate Hermit",
            },
        },
    ],
)
def test_coordinator_refuses_terminal_limited_unverified_or_mismatched_state(
    tmp_path: Path, monkeypatch, goal: dict[str, object],
) -> None:
    monkeypatch.setattr(producer, "read_agentctl", lambda *_args: goal)
    with pytest.raises(
        producer.SnapshotUnavailable,
        match="exact absent, active, blocked, or paused",
    ):
        producer.verify_agentctl_goals(
            coordinator_binding(tmp_path), Path("agentctl")
        )


@pytest.mark.parametrize("native_status", ("active", "blocked", "paused"))
def test_worker_exact_live_or_waiting_goal_retains_claim_authority(
    tmp_path: Path, monkeypatch, native_status: str,
) -> None:
    monkeypatch.setattr(
        producer,
        "read_agentctl",
        lambda *_args: native_goal(
            "native-session", "Drive the KVM lane", native_status
        ),
    )
    producer.verify_agentctl_goals(binding(tmp_path), Path("agentctl"))


@pytest.mark.parametrize(
    "native_status", ("complete", "usageLimited", "budgetLimited")
)
def test_worker_terminal_or_limited_goal_does_not_retain_claim_authority(
    tmp_path: Path, monkeypatch, native_status: str,
) -> None:
    monkeypatch.setattr(
        producer,
        "read_agentctl",
        lambda *_args: native_goal(
            "native-session", "Drive the KVM lane", native_status
        ),
    )
    with pytest.raises(producer.SnapshotUnavailable, match="exact bound objective"):
        producer.verify_agentctl_goals(binding(tmp_path), Path("agentctl"))


@pytest.mark.parametrize(
    "goal",
    [
        {
            "source": "native",
            "native_status": "absent",
            "goal": None,
            "native": None,
        },
        native_goal(
            "native-session", "Drive the KVM lane", "active", source="requested"
        ),
        native_goal("replacement-session", "Drive the KVM lane", "active"),
        native_goal("native-session", "Replacement objective", "active"),
        {
            "source": "native",
            "native_status": "active",
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "objective": "Drive the KVM lane",
            },
        },
        {
            "source": "native",
            "native_status": "blocked",
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": "Drive the KVM lane",
            },
        },
        {
            "source": "native",
            "native_status": [],
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": "Drive the KVM lane",
            },
        },
    ],
)
def test_worker_unverified_mismatched_or_missing_goal_state_refuses(
    tmp_path: Path, monkeypatch, goal: dict[str, object],
) -> None:
    monkeypatch.setattr(producer, "read_agentctl", lambda *_args: goal)
    with pytest.raises(producer.SnapshotUnavailable, match="exact bound objective"):
        producer.verify_agentctl_goals(binding(tmp_path), Path("agentctl"))


def test_agentctl_adapter_drift_refuses_the_bound_generation(
    tmp_path: Path, monkeypatch,
) -> None:
    monkeypatch.setattr(
        producer,
        "read_agentctl",
        lambda *_args: agentctl_list(tmp_path, adapter="herdr-foreign"),
    )
    with pytest.raises(producer.SnapshotUnavailable, match="adapter"):
        producer.verify_agentctl(
            binding(tmp_path, current_task=None, agentctl_adapter="herdr"),
            Path("agentctl"),
        )


@pytest.mark.parametrize("agent_status", ("idle", "needs-input", "needs-permission"))
def test_taskless_managed_waiting_worker_is_parsed_as_off_book(
    tmp_path: Path, monkeypatch, agent_status: str,
) -> None:
    taskgraph(tmp_path, include_task=False)
    bindings = binding(
        tmp_path,
        current_task=None,
        agentctl_adapter="herdr",
    )

    def read(_bin, _registry, *arguments):
        if arguments == ("list",):
            return agentctl_list(tmp_path, adapter="herdr")
        assert arguments == ("goal", "kvm")
        return {
            "source": "native",
            "native_status": "active",
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": "Drive the KVM lane",
            },
        }

    monkeypatch.setattr(producer, "read_agentctl", read)
    monkeypatch.setattr(
        producer,
        "read_herdr",
        lambda _bin: herdr(tmp_path, agent_status=agent_status),
    )
    result = producer.collect_snapshot(
        bindings,
        agentctl_bin=Path("agentctl"),
        herdr_bin=Path("herdr"),
        captured_at=10,
    )

    assert "current_task" not in result["agents"][0]
    assert result["authority"]["owned_in_progress"] == []
    report = operational_health.reconcile_active_work(
        (), operational_health._parse_agents(result["agents"])
    )
    assert report.claimed == ()
    assert [agent.name for agent in report.off_book] == ["dev-hermit-14"]

    legacy_row = dict(result["agents"][0])
    legacy_row.pop("source")
    legacy_report = operational_health.reconcile_active_work(
        (), operational_health._parse_agents([legacy_row])
    )
    assert legacy_report.off_book == ()


def test_generation_goal_and_taskgraph_owner_are_reproved_on_every_refresh(
    tmp_path: Path, monkeypatch,
) -> None:
    taskgraph(tmp_path)

    def read(_bin, _registry, *arguments):
        if arguments == ("list",):
            return agentctl_list(tmp_path)
        assert arguments == ("goal", "kvm")
        return {
            "source": "native",
            "native_status": "active",
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": "Drive the KVM lane",
            },
        }

    monkeypatch.setattr(producer, "read_agentctl", read)
    producer.verify_agentctl(binding(tmp_path), Path("agentctl"))
    producer.verify_agentctl_goals(binding(tmp_path), Path("agentctl"))
    assert producer.verify_taskgraph(binding(tmp_path)) == owned()


def test_owned_nested_worker_outside_bound_fleet_refuses_publication(
    tmp_path: Path,
) -> None:
    path = taskgraph(tmp_path)
    connection = sqlite3.connect(path)
    connection.execute(
        "INSERT INTO tasks VALUES (?, ?, ?)",
        ("nested_task", "IN_PROGRESS", "codex-nested-worker"),
    )
    connection.commit()
    connection.close()

    with pytest.raises(
        producer.SnapshotUnavailable,
        match=(
            "outside the complete bound fleet: "
            "nested_task owner=codex-nested-worker"
        ),
    ):
        producer.verify_taskgraph(binding(tmp_path))


def test_claimed_task_with_another_bound_owner_refuses_publication(
    tmp_path: Path,
) -> None:
    taskgraph(tmp_path, owner="other-worker")
    first = binding(tmp_path)
    other = producer.Binding(
        "other",
        "other-worker",
        "wK:p4",
        tmp_path.resolve(),
        "codex",
        None,
        "worker",
        "other",
        "herdr",
        "other-generation",
        "other-session",
        "Drive another native lane",
    )
    bindings = producer.Bindings(
        first.workspace,
        first.agentctl_registry,
        first.taskgraph_db,
        (*first.agents, other),
        first.binding_sha256,
    )

    with pytest.raises(
        producer.SnapshotUnavailable,
        match="not exactly one IN_PROGRESS row owned by 'dev-hermit-14'",
    ):
        producer.verify_taskgraph(bindings)


@pytest.mark.parametrize(
    ("failure", "message"),
    [
        ("token", "token"),
        ("goal", "exact bound objective"),
        ("owner", "outside the complete bound fleet"),
    ],
)
def test_changed_generation_goal_or_task_assignment_refuses_freshness(
    tmp_path: Path, monkeypatch, failure: str, message: str,
) -> None:
    taskgraph(tmp_path, owner="someone-else" if failure == "owner" else "dev-hermit-14")

    def read(_bin, _registry, *arguments):
        if arguments == ("list",):
            return agentctl_list(
                tmp_path, token="replacement-generation" if failure == "token" else "generation-token"
            )
        return {
            "source": "native",
            "native_status": "active",
            "goal": "Different goal" if failure == "goal" else "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": (
                    "Different goal" if failure == "goal" else "Drive the KVM lane"
                ),
            },
        }

    monkeypatch.setattr(producer, "read_agentctl", read)
    with pytest.raises(producer.SnapshotUnavailable, match=message):
        producer.verify_agentctl(binding(tmp_path), Path("agentctl"))
        producer.verify_agentctl_goals(binding(tmp_path), Path("agentctl"))
        producer.verify_taskgraph(binding(tmp_path))


def test_agentctl_generation_is_bracketed_around_task_and_herdr_reads(
    tmp_path: Path, monkeypatch,
) -> None:
    taskgraph(tmp_path)
    list_reads = 0

    def read(_bin, _registry, *arguments):
        nonlocal list_reads
        if arguments == ("list",):
            list_reads += 1
            rows = agentctl_list(tmp_path)
            if list_reads == 2:
                rows[0]["tab_id"] = "wK:replacement-tab"
            return rows
        return {
            "source": "native",
            "native_status": "active",
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": "Drive the KVM lane",
            },
        }

    monkeypatch.setattr(producer, "read_agentctl", read)
    monkeypatch.setattr(producer, "read_herdr", lambda _bin: herdr(tmp_path))
    with pytest.raises(producer.SnapshotUnavailable, match="changed during"):
        producer.collect_snapshot(
            binding(tmp_path),
            agentctl_bin=Path("agentctl"),
            herdr_bin=Path("herdr"),
            captured_at=10,
        )
    assert list_reads == 2


def test_native_goal_is_revalidated_on_both_sides_of_collection(
    tmp_path: Path, monkeypatch,
) -> None:
    taskgraph(tmp_path)
    goal_reads = 0

    def read(_bin, _registry, *arguments):
        nonlocal goal_reads
        if arguments == ("list",):
            return agentctl_list(tmp_path)
        goal_reads += 1
        objective = "Drive the KVM lane" if goal_reads == 1 else "Replacement goal"
        return {
            "source": "native",
            "native_status": "active",
            "goal": objective,
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": objective,
            },
        }

    monkeypatch.setattr(producer, "read_agentctl", read)
    monkeypatch.setattr(producer, "read_herdr", lambda _bin: herdr(tmp_path))
    with pytest.raises(producer.SnapshotUnavailable, match="exact bound objective"):
        producer.collect_snapshot(
            binding(tmp_path),
            agentctl_bin=Path("agentctl"),
            herdr_bin=Path("herdr"),
            captured_at=10,
        )
    assert goal_reads == 2


def test_collection_timestamp_is_observation_start_not_publication_end(
    tmp_path: Path, monkeypatch,
) -> None:
    taskgraph(tmp_path)
    clock = [10.0]

    def read(_bin, _registry, *arguments):
        clock[0] += 5
        if arguments == ("list",):
            return agentctl_list(tmp_path)
        return {
            "source": "native",
            "native_status": "active",
            "goal": "Drive the KVM lane",
            "native": {
                "threadId": "native-session",
                "status": "active",
                "objective": "Drive the KVM lane",
            },
        }

    def read_live(_bin):
        clock[0] += 15
        return herdr(tmp_path)

    monkeypatch.setattr(producer.time, "time", lambda: clock[0])
    monkeypatch.setattr(producer, "read_agentctl", read)
    monkeypatch.setattr(producer, "read_herdr", read_live)
    result = producer.collect_snapshot(
        binding(tmp_path), agentctl_bin=Path("agentctl"), herdr_bin=Path("herdr")
    )
    assert clock[0] > 10
    assert result["captured_at"] == 10
