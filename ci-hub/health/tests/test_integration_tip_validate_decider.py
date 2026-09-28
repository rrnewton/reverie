"""The hourly gate reports the previous detached decision and starts the next.

Measured 2026-09-28 on devbig014: one decision needs a cold ci-hub compile
(44.7s) plus two exact validate-status reads (34-60s each, 106.6s cold), which
cannot fit in the gate's 75s. These tests pin the split: the gate never decides
inline, never starts a second decider while one is live, reports a decider that
vanished, and the decider records every outcome under the same tick.lock.
"""

from __future__ import annotations

import fcntl
import importlib.util
import json
import subprocess
import sys
from dataclasses import replace
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_decider_target",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

SHA = "1234567890abcdef1234567890abcdef12345678"
PREVIOUS = "20260928T150000Z-abcdef123456"
PREVIOUS_UNIT = "integration-tip-decide-abcdef123456.service"


def configured(tmp_path: Path):
    source = tmp_path / "hermit"
    source.mkdir(exist_ok=True)
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o755)
    settings = driver.Settings.defaults(
        tool_root=ROOT,
        state_root=tmp_path,
        source_checkout=source,
        ci_hub=tmp_path / "ci-hub",
        operational_tool=operational_tool,
        tool_parent_sha="f" * 40,
        state_dir=tmp_path / "tick-state",
    )
    settings.state_dir.mkdir(parents=True, exist_ok=True)
    return settings


def unit_reply(unit, *, state="inactive", load="not-found", pid="0", invocation="", sub="dead"):
    return (
        f"Id={unit}\nLoadState={load}\nActiveState={state}\nSubState={sub}\n"
        f"MainPID={pid}\nInvocationID={invocation}\n"
    )


class Systemd:
    """Answer `systemctl --user show` for the prior decider only."""

    def __init__(self, **properties) -> None:
        self.properties = properties
        self.queries: list[str] = []
        self.fail = False

    def run(self, command, _cwd, timeout):
        assert timeout > 0
        assert list(command[:3]) == ["systemctl", "--user", "show"], command
        self.queries.append(command[3])
        if self.fail:
            return subprocess.CompletedProcess(command, 1, "", "Failed to connect to bus\n")
        return subprocess.CompletedProcess(
            command, 0, unit_reply(command[3], **self.properties), ""
        )


class Spawns:
    def __init__(self, error: Exception | None = None) -> None:
        self.calls: list[tuple[str, str, Path, float]] = []
        self.error = error

    def __call__(self, _settings, dispatch_id, unit, log, timeout) -> None:
        self.calls.append((dispatch_id, unit, log, timeout))
        if self.error is not None:
            raise self.error


def previous_dispatch(settings, **extra):
    record = {
        "schema_version": 1,
        "dispatch_id": PREVIOUS,
        "unit": PREVIOUS_UNIT,
        "state": "accepted",
        "dispatched_at": "2026-09-28T15:00:00+00:00",
        "log": str(settings.state_dir / "logs" / f"{PREVIOUS}-decider.log"),
        **extra,
    }
    driver.atomic_write_json(settings.state_dir / driver.DISPATCH_RECORD, record)
    return record


def recorded(settings, *, exit_code, **fields):
    driver.atomic_write_json(
        driver.decision_path(settings, PREVIOUS),
        {
            "schema_version": 1,
            "dispatch_id": PREVIOUS,
            "unit": PREVIOUS_UNIT,
            "started_at": "2026-09-28T15:00:01+00:00",
            "finished_at": "2026-09-28T15:03:00+00:00",
            "exit_code": exit_code,
            "fields": fields,
            "output": "".join(f"{key}={value}\n" for key, value in fields.items()),
        },
    )


@pytest.fixture
def no_inline_decision(monkeypatch):
    def must_not_decide(*_args, **_kwargs):
        raise AssertionError("the gate must never decide inline")

    monkeypatch.setattr(driver, "run_tick", must_not_decide)


def read_dispatch(settings):
    return json.loads((settings.state_dir / driver.DISPATCH_RECORD).read_text())


def test_gate_without_history_dispatches_one_decider(
    tmp_path: Path, capsys, no_inline_decision
) -> None:
    settings = configured(tmp_path)
    spawn = Spawns()

    assert driver.gate_tick(settings, run=Systemd().run, spawn=spawn) == 0

    assert len(spawn.calls) == 1
    dispatch_id, unit, log, timeout = spawn.calls[0]
    assert unit == driver.decider_unit(dispatch_id)
    assert log == settings.state_dir / "logs" / f"{dispatch_id}-decider.log"
    assert 0 < timeout <= driver.DECIDER_DISPATCH_TIMEOUT_SECONDS
    record = read_dispatch(settings)
    assert record["dispatch_id"] == dispatch_id
    assert record["unit"] == unit
    assert record["state"] == "accepted"
    assert record["tool_parent_sha"] == "f" * 40
    output = capsys.readouterr().out
    assert "state=decision-dispatched" in output
    assert f"next_decision={dispatch_id}" in output


@pytest.mark.parametrize("exit_code,state", [(0, "launched"), (2, "unknown"), (0, "busy")])
def test_gate_reports_the_recorded_decision_with_its_exit_code(
    tmp_path: Path, capsys, no_inline_decision, exit_code, state
) -> None:
    settings = configured(tmp_path)
    previous_dispatch(settings)
    recorded(
        settings,
        exit_code=exit_code,
        state=state,
        target=SHA,
        launched="1" if state == "launched" else "0",
        summary=f"the decision said {state}",
    )
    systemd = Systemd()
    spawn = Spawns()

    assert driver.gate_tick(settings, run=systemd.run, spawn=spawn) == exit_code

    assert systemd.queries == [], "a recorded decision needs no unit observation"
    assert len(spawn.calls) == 1
    output = capsys.readouterr().out
    assert f"state={state}" in output
    assert f"target={SHA}" in output
    assert f"decision={PREVIOUS}" in output
    assert "decided_at=2026-09-28T15:03:00+00:00" in output
    assert (
        f"summary=decision {PREVIOUS} finished 2026-09-28T15:03:00+00:00: "
        f"the decision said {state}"
    ) in output
    assert f"next_decision={spawn.calls[0][0]}" in output
    assert output.count("summary=") == 1


@pytest.mark.parametrize(
    "state,sub,pid",
    [("active", "running", "4242"), ("activating", "start", "0"), ("deactivating", "stop", "4242")],
)
def test_gate_never_starts_a_second_decider_while_one_is_live(
    tmp_path: Path, capsys, no_inline_decision, state, sub, pid
) -> None:
    settings = configured(tmp_path)
    before = previous_dispatch(settings)
    systemd = Systemd(state=state, sub=sub, pid=pid, load="loaded", invocation="a" * 32)
    spawn = Spawns()

    assert driver.gate_tick(settings, run=systemd.run, spawn=spawn) == 0

    assert systemd.queries == [PREVIOUS_UNIT]
    assert spawn.calls == []
    assert read_dispatch(settings) == before
    output = capsys.readouterr().out
    assert "state=deciding" in output
    assert "no second decider started" in output
    assert "launched=0" in output


@pytest.mark.parametrize(
    "properties",
    [
        {},
        {"state": "failed", "sub": "failed", "load": "loaded", "invocation": "b" * 32},
        {"state": "inactive", "sub": "dead", "load": "loaded", "invocation": "b" * 32},
    ],
)
def test_a_decider_that_ended_without_a_record_is_reported_and_replaced(
    tmp_path: Path, capsys, no_inline_decision, properties
) -> None:
    settings = configured(tmp_path)
    previous_dispatch(settings)
    spawn = Spawns()

    assert driver.gate_tick(settings, run=Systemd(**properties).run, spawn=spawn) == 2

    assert len(spawn.calls) == 1
    assert read_dispatch(settings)["dispatch_id"] == spawn.calls[0][0]
    output = capsys.readouterr().out
    assert "state=unknown" in output
    assert "recorded no decision" in output
    assert f"decision_unit={PREVIOUS_UNIT}" in output


def test_an_unobservable_decider_refuses_and_starts_nothing(
    tmp_path: Path, no_inline_decision
) -> None:
    settings = configured(tmp_path)
    before = previous_dispatch(settings)
    systemd = Systemd()
    systemd.fail = True
    spawn = Spawns()

    with pytest.raises(driver.ObservationUnavailable, match="cannot observe prior decider"):
        driver.gate_tick(settings, run=systemd.run, spawn=spawn)

    assert spawn.calls == []
    assert read_dispatch(settings) == before


@pytest.mark.parametrize(
    "mutation",
    [
        {"dispatch_id": "not-a-dispatch-id"},
        {"unit": "integration-tip-decide-000000000000.service"},
        {"schema_version": 2},
    ],
)
def test_a_malformed_dispatch_record_refuses_and_starts_nothing(
    tmp_path: Path, no_inline_decision, mutation
) -> None:
    settings = configured(tmp_path)
    previous_dispatch(settings, **mutation)
    spawn = Spawns()

    with pytest.raises(driver.ObservationUnavailable):
        driver.gate_tick(settings, run=Systemd().run, spawn=spawn)
    assert spawn.calls == []


def test_a_malformed_decision_record_refuses_and_starts_nothing(
    tmp_path: Path, no_inline_decision
) -> None:
    settings = configured(tmp_path)
    previous_dispatch(settings)
    recorded(settings, exit_code="2", state="unknown", summary="x")
    spawn = Spawns()

    with pytest.raises(driver.ObservationUnavailable, match="unsupported shape"):
        driver.gate_tick(settings, run=Systemd().run, spawn=spawn)
    assert spawn.calls == []


@pytest.mark.parametrize(
    "error,dispatch_state",
    [
        (driver.CommandNotStarted("systemd-run is absent"), "not-started"),
        (driver.ObservationUnavailable("systemd-run timed out after 10.0s"), "unconfirmed"),
    ],
)
def test_a_failed_dispatch_is_unknown_and_keeps_the_previous_decision_visible(
    tmp_path: Path, capsys, no_inline_decision, error, dispatch_state
) -> None:
    settings = configured(tmp_path)
    previous_dispatch(settings)
    recorded(settings, exit_code=0, state="busy", summary="validate-lock was busy")
    spawn = Spawns(error)

    assert driver.gate_tick(settings, run=Systemd().run, spawn=spawn) == 2

    record = read_dispatch(settings)
    assert record["dispatch_id"] == spawn.calls[0][0]
    assert record["state"] == dispatch_state
    output = capsys.readouterr().out
    assert "state=unknown" in output
    assert "next_decision=not-started" in output
    assert "could not start the next decision" in output
    assert "validate-lock was busy" in output


def test_missing_launcher_prerequisites_write_no_dispatch_intent(
    tmp_path: Path, capsys, no_inline_decision
) -> None:
    settings = replace(configured(tmp_path), operational_tool=None)
    spawn = Spawns()

    assert driver.gate_tick(settings, run=Systemd().run, spawn=spawn) == 2

    assert spawn.calls == []
    assert not (settings.state_dir / driver.DISPATCH_RECORD).exists()
    assert "operational-tool is unavailable" in capsys.readouterr().out


def test_the_decider_reenters_this_file_from_the_same_exact_tool_commit(
    tmp_path: Path, monkeypatch
) -> None:
    settings = configured(tmp_path)
    commands = []

    def accepted(command, cwd, timeout):
        commands.append((list(command), cwd, timeout))
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(driver, "run_bounded", accepted)
    log = settings.state_dir / "logs" / f"{PREVIOUS}-decider.log"
    driver.spawn_decider(settings, PREVIOUS, PREVIOUS_UNIT, log, 7.0)

    [(command, cwd, timeout)] = commands
    assert cwd == settings.state_root
    assert timeout == 7.0
    assert log.is_file()
    separator = command.index("--")
    systemd_run = command[:separator]
    assert systemd_run[:4] == ["systemd-run", "--user", "--quiet", "--collect"]
    assert "--unit=integration-tip-decide-abcdef123456" in systemd_run
    assert f"--property=RuntimeMaxSec={driver.DETACHED_RUNTIME_MAX_SECONDS}" in systemd_run
    assert f"--property=StandardOutput=append:{log}" in systemd_run
    assert f"--setenv=DEV_HERMIT_PARENT={settings.state_root}" in systemd_run
    tool = command[separator + 1:]
    assert tool[:8] == [
        str(settings.operational_tool),
        "--run-current-tool",
        "--state-root",
        str(settings.state_root),
        "--tool-parent-sha",
        "f" * 40,
        "--tool-relative",
        "ci-hub/health/integration_tip_validate.py",
    ]
    arguments = tool[tool.index("--") + 1:]
    assert arguments[-3:] == ["--decide", "--dispatch-id", PREVIOUS]
    assert "--ci-hub" not in arguments, "the gate's tool tree is not the decider's"
    assert (ROOT / driver.DECIDER_TOOL_RELATIVE).resolve() == Path(driver.__file__).resolve()


def test_the_decider_decides_under_the_tick_lock_and_records_the_result(
    tmp_path: Path, capsys
) -> None:
    settings = replace(
        configured(tmp_path),
        total_timeout_seconds=driver.DETACHED_TOTAL_TIMEOUT_SECONDS,
        limits=driver.DETACHED_PHASE_LIMITS,
    )
    seen = []

    def tick(received) -> int:
        seen.append(received)
        with (settings.state_dir / "tick.lock").open("a+") as other:
            with pytest.raises(BlockingIOError):
                fcntl.flock(other.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        driver.emit(unrelated_unreadable_handles=3, unrelated_unreadable_record="/r.json")
        driver.emit(state="launched", target=SHA, launched=1, summary="started one validate")
        return 0

    assert driver.decide(settings, PREVIOUS, tick=tick) == 0

    assert seen == [settings]
    record = json.loads(driver.decision_path(settings, PREVIOUS).read_text())
    assert record["dispatch_id"] == PREVIOUS
    assert record["unit"] == PREVIOUS_UNIT
    assert record["exit_code"] == 0
    assert record["fields"] == {
        "unrelated_unreadable_handles": "3",
        "unrelated_unreadable_record": "/r.json",
        "state": "launched",
        "target": SHA,
        "launched": "1",
        "summary": "started one validate",
    }
    assert "state=launched" in capsys.readouterr().out, "the decider log keeps the output"


@pytest.mark.parametrize(
    "error,summary",
    [
        (driver.ObservationUnavailable("status timed out"), "could not decide whether to start"),
        (RuntimeError("boom"), "the decider failed: RuntimeError: boom"),
    ],
)
def test_the_decider_records_a_failed_decision_instead_of_vanishing(
    tmp_path: Path, error, summary
) -> None:
    settings = configured(tmp_path)

    def tick(_settings) -> int:
        raise error

    assert driver.decide(settings, PREVIOUS, tick=tick) == 0

    record = json.loads(driver.decision_path(settings, PREVIOUS).read_text())
    assert record["exit_code"] == 2
    assert record["fields"]["state"] == "unknown"
    assert summary in record["fields"]["summary"]

    # The next gate reports that failure with its exit code.
    previous_dispatch(settings)
    assert driver.gate_tick(settings, run=Systemd().run, spawn=Spawns()) == 2


def test_a_decider_that_cannot_take_the_lock_records_that_it_did_not_decide(
    tmp_path: Path,
) -> None:
    settings = configured(tmp_path)

    def tick(_settings) -> int:
        raise AssertionError("no decision without the tick lock")

    with (settings.state_dir / "tick.lock").open("a+") as held:
        fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        assert driver.decide(settings, PREVIOUS, tick=tick, lock_wait_seconds=0.0) == 0

    record = json.loads(driver.decision_path(settings, PREVIOUS).read_text())
    assert record["exit_code"] == 2
    assert record["fields"]["state"] == "unknown"
    assert "no decision was made" in record["fields"]["summary"]


def test_the_gate_holds_the_tick_lock_and_never_runs_a_second_decision(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    state_dir = tmp_path / "tick-state"
    state_dir.mkdir()

    def must_not_run(*_args, **_kwargs):
        raise AssertionError("a concurrent gate must stop before reading authorities")

    monkeypatch.setattr(driver, "gate_tick", must_not_run)
    monkeypatch.setattr(driver, "run_tick", must_not_run)
    with (state_dir / "tick.lock").open("a+") as held:
        fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        assert driver.main(["--state-root", str(tmp_path), "--state-dir", str(state_dir)]) == 0
    output = capsys.readouterr().out
    assert "state=in-progress" in output
    assert "no second decision started" in output


def test_each_mode_gets_its_own_budget(tmp_path: Path, monkeypatch) -> None:
    state_dir = tmp_path / "tick-state"
    base = ["--state-root", str(tmp_path), "--state-dir", str(state_dir)]
    seen = {}

    def gate(settings, **_kwargs):
        seen["gate"] = settings
        return 0

    def tick(settings, **kwargs):
        seen["dry-run"] = (settings, kwargs)
        return 0

    def decide(settings, dispatch_id, **_kwargs):
        seen["decide"] = (settings, dispatch_id)
        return 0

    monkeypatch.setattr(driver, "gate_tick", gate)
    monkeypatch.setattr(driver, "run_tick", tick)
    monkeypatch.setattr(driver, "decide", decide)

    assert driver.main(base) == 0
    assert seen["gate"].total_timeout_seconds == driver.DEFAULT_TOTAL_TIMEOUT_SECONDS == 75
    assert driver.main([*base, "--dry-run"]) == 0
    settings, kwargs = seen["dry-run"]
    assert kwargs == {"dry_run": True}
    assert settings.total_timeout_seconds == driver.DETACHED_TOTAL_TIMEOUT_SECONDS
    assert settings.limits == driver.DETACHED_PHASE_LIMITS
    assert driver.main([*base, "--decide", "--dispatch-id", PREVIOUS]) == 0
    settings, dispatch_id = seen["decide"]
    assert dispatch_id == PREVIOUS
    assert settings.limits == driver.DETACHED_PHASE_LIMITS

    for refused in (
        [*base, "--decide"],
        [*base, "--dispatch-id", PREVIOUS],
        [*base, "--decide", "--dispatch-id", "../../escape"],
        [*base, "--dry-run", "--total-timeout-seconds", "601"],
        [*base, "--total-timeout-seconds", "180"],
    ):
        with pytest.raises(SystemExit):
            driver.main(refused)
    with pytest.raises(SystemExit):
        driver.main([*base, "--dry-run", "--decide", "--dispatch-id", PREVIOUS])


def test_the_detached_budget_fits_the_measured_decision_and_the_gate_does_not() -> None:
    # Measured 2026-09-28 on devbig014: first status 106.6s cold through
    # operational-tool, second status 59.6s under load, admission 1.1s.
    measured = {"fetch": 5.0, "first": 106.6, "admission": 1.1, "second": 59.6, "launch": 1.0}

    def fits(limits, total) -> bool:
        caps = (
            (measured["fetch"], limits.fetch),
            (measured["first"], limits.first_status),
            (measured["admission"], limits.admission),
            (measured["second"], limits.second_status),
            (measured["launch"], limits.launch),
        )
        return all(spent <= cap for spent, cap in caps) and sum(measured.values()) <= total

    assert fits(driver.DETACHED_PHASE_LIMITS, driver.DETACHED_TOTAL_TIMEOUT_SECONDS)
    assert not fits(driver.PhaseLimits(), driver.DEFAULT_TOTAL_TIMEOUT_SECONDS)
    # The decider may wait for the lock, then decide, inside its unit's lifetime.
    assert (
        driver.DECIDER_LOCK_WAIT_SECONDS + driver.DETACHED_TOTAL_TIMEOUT_SECONDS
        < driver.DETACHED_RUNTIME_MAX_SECONDS
    )
    # The gate's own work is one unit observation and one dispatch.
    assert (
        driver.DECIDER_OBSERVATION_TIMEOUT_SECONDS + driver.DECIDER_DISPATCH_TIMEOUT_SECONDS
        < driver.DEFAULT_TOTAL_TIMEOUT_SECONDS
    )


def test_decider_history_is_bounded_and_only_its_own_files_are_pruned(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    decisions = settings.state_dir / "decisions"
    logs = settings.state_dir / "logs"
    decisions.mkdir()
    logs.mkdir()
    ids = [f"20260928T{index:06d}Z-{index:012x}" for index in range(60)]
    for dispatch_id in ids:
        (decisions / f"{dispatch_id}.json").write_text("{}")
        (logs / f"{dispatch_id}-decider.log").write_text("")
    (logs / f"{ids[0]}-validate.log").write_text("")
    (decisions / "notes.json").write_text("{}")

    driver.prune_decider_history(settings, keep=48)

    assert sorted(path.name for path in decisions.glob("2026*.json")) == [
        f"{dispatch_id}.json" for dispatch_id in ids[-48:]
    ]
    assert len(list(logs.glob("*-decider.log"))) == 48
    assert (logs / f"{ids[0]}-validate.log").exists()
    assert (decisions / "notes.json").exists()
