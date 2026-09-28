from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import time
from dataclasses import replace
from pathlib import Path

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

SHA = "1234567890abcdef1234567890abcdef12345678"
PRIOR_UNIT = f"validate-ops-tick-{SHA[:12]}-abcdef123456-launcher.service"


def launcher_reply(unit, *, state="inactive", load="not-found", pid="0", invocation="", sub="dead"):
    return (
        f"Id={unit}\nLoadState={load}\nActiveState={state}\nSubState={sub}\n"
        f"MainPID={pid}\nInvocationID={invocation}\n"
    )


def prior_attempt(configured, *, state="starting", target=SHA, **extra):
    attempt = {
        "schema_version": 1,
        "attempt_id": "interrupted-after-systemd-accept",
        "target": target,
        "state": state,
        "started_at": "2026-09-05T01:00:00+00:00",
        "launcher_unit": f"validate-ops-tick-{target[:12]}-abcdef123456-launcher.service",
        **extra,
    }
    driver.atomic_write_json(configured.state_dir / "latest.json", attempt)
    return attempt


def settings(tmp_path: Path) -> object:
    source = tmp_path / "hermit"
    source.mkdir()
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o755)
    return driver.Settings.defaults(
        tool_root=ROOT,
        state_root=tmp_path,
        source_checkout=source,
        ci_hub=tmp_path / "ci-hub",
        operational_tool=operational_tool,
        tool_parent_sha="f" * 40,
        state_dir=tmp_path / "tick-state",
    )


class Authorities:
    def __init__(
        self,
        *,
        records: int = 0,
        in_progress: int = 0,
        admissible: bool = True,
        target: str = SHA,
    ) -> None:
        self.records = records
        self.record_verdict = "VALIDATED"
        self.record_result = "pass"
        self.record_exit_code = 0
        self.record_detail = "all required validation gates passed"
        self.in_progress = in_progress
        self.admissible = admissible
        self.target = target
        self.launches: list[str] = []
        self.status_reads = 0
        self.launch_units: list[str] = []
        self.launcher_queries: list[str] = []
        self.launcher_properties = {}

    def fetch(self, _settings: object, _timeout: float) -> str:
        return self.target

    def run(
        self, command: list[str] | tuple[str, ...], _cwd: Path, _timeout: float
    ) -> subprocess.CompletedProcess[str]:
        if command[:3] == ["systemctl", "--user", "show"]:
            unit = command[3]
            self.launcher_queries.append(unit)
            return subprocess.CompletedProcess(
                command, 0, launcher_reply(unit, **self.launcher_properties), ""
            )
        if "validate-status" in command and "--sha" in command:
            self.status_reads += 1
            in_progress = [
                {
                    "sha": self.target,
                    "in_progress": True,
                    "summary": f"run {index}",
                }
                for index in range(self.in_progress)
            ]
            handles = [
                {"unit": f"validate-{index}.service", "state": "running", "sha": self.target,
                 "started_at": "2026-09-05T00:00:00Z", "slot": f"slot-{index}",
                 "log_file": f"/logs/validate-{index}.log", "process_verified": True,
                 "admission": "admitted", "finished_at": None, "result": None,
                 "detail": None, "exit_code": None}
                for index in range(self.in_progress)
            ]
            verdict = self.record_verdict if self.records else "NOT-VALIDATED"
            exit_code = self.record_exit_code if self.records else 4
            exact_records = [
                {
                    "verdict": self.record_verdict,
                    "result": self.record_result,
                    "exit_code": self.record_exit_code,
                    "started_at": "2026-09-05T00:00:00Z",
                    "finished_at": "2026-09-05T00:10:00Z",
                    "detail": self.record_detail,
                }
                for _ in range(self.records)
            ]
            report = {
                "schema_version": driver.EXACT_VALIDATE_STATUS_SCHEMA_VERSION,
                "repo": driver.REPO,
                "sha": self.target,
                "verdict": verdict,
                "exit_code": exit_code,
                "qualifying_count": self.records if verdict == "VALIDATED" else 0,
                "disqualified_count": 0 if verdict == "VALIDATED" else self.records,
                "record_count": self.records,
                "exact_records": exact_records,
                "ledger_freshness": (
                    None
                    if self.records
                    else {
                        "state": "current",
                        "local_absence_is_authoritative": True,
                    }
                ),
                "in_progress_count": len(in_progress),
                "in_progress": in_progress,
                "run_handles": handles,
                "unreadable_handles": [],
            }
            return subprocess.CompletedProcess(command, report["exit_code"], json.dumps(report), "")
        if "admission-status" in command:
            assert "--target-was-fetched-main" in command
            report = {
                "schema_version": 1,
                "target": self.target,
                "kind": "validate",
                "admissible": self.admissible,
                "state": "admissible" if self.admissible else "busy",
                "reason_code": (
                    "immediate-admission-available"
                    if self.admissible
                    else "global-priority-queue-not-ready"
                ),
                "detail": (
                    "validation slot 2 allows an immediate attempt"
                    if self.admissible
                    else "global priority queue position 2 is behind another validate"
                ),
            }
            return subprocess.CompletedProcess(
                command, 0 if self.admissible else 1, json.dumps(report), ""
            )
        raise AssertionError(command)

    def spawn(
        self,
        _settings: object,
        target: str,
        attempt_id: str,
        unit: str,
        _validate_log: Path,
        _launcher_log: Path,
        _timeout: float,
    ) -> dict[str, object]:
        assert target == self.target
        self.launches.append(attempt_id)
        self.launch_units.append(unit)
        self.in_progress = 1
        return {"unit": f"{unit}-launcher.service", "accepted": True}


def read_twice(
    tmp_path: Path,
    authorities: Authorities,
):
    configured = settings(tmp_path)
    first = driver.run_tick(
        configured,
        fetch=authorities.fetch,
        run=authorities.run,
        spawn=authorities.spawn,
    )
    time.sleep(0.01)
    second = driver.run_tick(
        configured,
        fetch=authorities.fetch,
        run=authorities.run,
        spawn=authorities.spawn,
    )
    return first, second


class TimedAuthorities(Authorities):
    """Account for elapsed authority work without sleeping or launching jobs."""

    def __init__(self, *, durations=None, evidence_read=None, evidence=None):
        super().__init__()
        self.elapsed = 0.0
        self.bounds: list[tuple[str, float]] = []
        self.durations = {
            "fetch": 2.0,
            "status-1": 47.0,
            "admission": 1.0,
            "status-2": 10.0,
            "spawn": 1.0,
            "launcher": 0.5,
            **(durations or {}),
        }
        self.evidence_read = evidence_read
        self.evidence = evidence

    def consume(self, phase, timeout):
        assert timeout > 0
        self.bounds.append((phase, timeout))
        duration = self.durations[phase]
        if duration > timeout:
            self.elapsed += timeout
            raise driver.ObservationUnavailable(f"{phase} timed out after {timeout}s")
        self.elapsed += duration

    def fetch(self, configured, timeout):
        self.consume("fetch", timeout)
        return super().fetch(configured, timeout)

    def run(self, command, cwd, timeout):
        if command[0] == "systemctl":
            self.consume("launcher", timeout)
        elif "validate-status" in command:
            reading = self.status_reads + 1
            self.consume(f"status-{reading}", timeout)
            if reading == self.evidence_read:
                if self.evidence == "record":
                    self.records = 1
                elif self.evidence == "in-flight":
                    self.in_progress = 1
                else:
                    raise AssertionError(self.evidence)
        else:
            assert "admission-status" in command
            self.consume("admission", timeout)
        return super().run(command, cwd, timeout)

    def spawn(self, configured, target, attempt, unit, validate_log, launcher_log, timeout):
        self.consume("spawn", timeout)
        return super().spawn(
            configured, target, attempt, unit, validate_log, launcher_log, timeout
        )


def test_cold_status_and_second_observation_finish_within_total_deadline(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    configured = settings(tmp_path)
    authorities = TimedAuthorities()
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    assert configured.total_timeout_seconds == 75.0
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 0
    assert authorities.bounds == [
        ("fetch", 25.0),
        ("status-1", 50.0),
        ("admission", 10.0),
        ("status-2", 25.0),
        ("spawn", 10.0),
    ]
    assert authorities.elapsed == 61.0
    assert authorities.status_reads == 2
    assert len(authorities.launches) == 1
    assert "state=launched" in capsys.readouterr().out


@pytest.mark.parametrize(
    "durations,total,expected_bounds,elapsed",
    [
        ({"fetch": 20.0}, 60.0, [("fetch", 25.0), ("status-1", 40.0)], 60.0),
        (
            {"fetch": 24.0, "status-1": 49.0, "admission": 2.0},
            75.0,
            [("fetch", 25.0), ("status-1", 50.0), ("admission", 2.0)],
            75.0,
        ),
        (
            {"fetch": 20.0},
            75.0,
            [("fetch", 25.0), ("status-1", 50.0), ("admission", 8.0), ("status-2", 7.0)],
            75.0,
        ),
        (
            {"status-1": 1.0, "status-2": 26.0},
            75.0,
            [("fetch", 25.0), ("status-1", 50.0), ("admission", 10.0), ("status-2", 25.0)],
            29.0,
        ),
    ],
)
def test_status_deadline_exhaustion_never_launches(
    tmp_path: Path, monkeypatch, durations, total, expected_bounds, elapsed
) -> None:
    configured = replace(settings(tmp_path), total_timeout_seconds=total)
    authorities = TimedAuthorities(durations=durations)
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    with pytest.raises(driver.ObservationUnavailable):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert authorities.bounds == expected_bounds
    assert authorities.elapsed == elapsed
    assert authorities.elapsed <= configured.total_timeout_seconds
    assert authorities.launches == []
    assert not (configured.state_dir / "latest.json").exists()


def test_launcher_request_uses_only_remaining_decision_time(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    configured = settings(tmp_path)
    authorities = TimedAuthorities(durations={"status-2": 20.0, "spawn": 6.0})
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 2
    assert authorities.bounds[4:] == [("spawn", 5.0)]
    assert authorities.elapsed == configured.total_timeout_seconds == 75.0
    assert authorities.launches == []
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-unknown"
    assert "finished_at" not in latest
    assert "launched=unknown" in capsys.readouterr().out


def test_exhausted_launch_budget_writes_no_intent(tmp_path: Path, monkeypatch) -> None:
    configured = settings(tmp_path)
    authorities = TimedAuthorities(durations={"status-2": 25.0})
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    with pytest.raises(driver.ObservationUnavailable, match="deadline expired"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert authorities.elapsed == 75.0
    assert len(authorities.bounds) == 4
    assert authorities.launches == []
    assert not (configured.state_dir / "latest.json").exists()
    assert not (configured.state_dir / "logs").exists()


@pytest.mark.parametrize("evidence_read", [1, 2])
@pytest.mark.parametrize("evidence", ["record", "in-flight"])
def test_cold_status_preserves_launch_suppression_at_either_observation(
    tmp_path: Path, monkeypatch, capsys, evidence_read, evidence
) -> None:
    configured = settings(tmp_path)
    authorities = TimedAuthorities(evidence_read=evidence_read, evidence=evidence)
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 0
    assert authorities.status_reads == evidence_read
    assert authorities.launches == []
    assert authorities.elapsed < configured.total_timeout_seconds
    assert dict(authorities.bounds)["status-1"] == 50.0
    if evidence_read == 2:
        assert dict(authorities.bounds)["status-2"] == 25.0
    assert "launched=0" in capsys.readouterr().out


def test_two_record_present_readings_launch_nothing(tmp_path: Path, capsys) -> None:
    authorities = Authorities(records=1)
    assert read_twice(tmp_path, authorities) == (0, 0)
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert output.count("state=record-present") == 2
    assert output.count("record_count=1") == 2


def test_two_in_progress_readings_launch_nothing(tmp_path: Path, capsys) -> None:
    authorities = Authorities(in_progress=1)

    assert read_twice(tmp_path, authorities) == (0, 0)
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert output.count("state=in-progress") == 2
    assert output.count("in_progress_count=1") == 2


def test_in_progress_without_a_qualifying_handle_refuses_the_decision(
    tmp_path: Path,
) -> None:
    authorities = Authorities(in_progress=1)

    def inconsistent_status(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" not in command:
            return result
        value = json.loads(result.stdout)
        value["run_handles"] = []
        return subprocess.CompletedProcess(
            command, result.returncode, json.dumps(value), ""
        )

    with pytest.raises(
        driver.ObservationUnavailable,
        match="in-progress run without a process-verified launching or running handle",
    ):
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=inconsistent_status,
            spawn=authorities.spawn,
        )
    assert authorities.launches == []


def test_unverified_launching_handle_appearing_between_reads_is_unknown(
    tmp_path: Path, capsys
) -> None:
    authorities = Authorities()

    def launching_between_reads(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" not in command or authorities.status_reads != 2:
            return result
        value = json.loads(result.stdout)
        value["run_handles"] = [
            {
                "unit": "validate-current-main.service",
                "state": "launching",
                "sha": SHA,
                "started_at": "2026-09-11T17:00:00Z",
                "slot": "validate-current-main",
                "log_file": "/state/ignored/validate/current-main.log",
                "process_verified": False,
                "admission": None,
                "finished_at": None,
                "result": None,
                "detail": None,
                "exit_code": None,
            }
        ]
        return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")

    assert (
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=launching_between_reads,
            spawn=authorities.spawn,
        )
        == 2
    )
    assert authorities.status_reads == 2
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert "state=unknown" in output
    assert "head=" + SHA in output
    assert "slot=validate-current-main" in output
    assert "log=/state/ignored/validate/current-main.log" in output
    assert "process_verified=False" in output
    assert "repair or finish the handle before retrying" in output


@pytest.mark.parametrize("state", ["preparing", "launching", "running"])
def test_persistent_unverified_outstanding_handle_suppresses_with_remedy(
    tmp_path: Path, capsys, state: str
) -> None:
    authorities = Authorities()

    def outstanding_on_both_reads(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" not in command:
            return result
        value = json.loads(result.stdout)
        value["run_handles"] = [
            {
                "unit": "validate-stale.service",
                "state": state,
                "sha": SHA,
                "started_at": "2026-09-01T00:00:00Z",
                "slot": "validate-stale",
                "log_file": "/logs/validate-stale.log",
                "process_verified": False,
                "admission": None,
                "finished_at": None,
                "result": None,
                "detail": None,
                "exit_code": None,
            }
        ]
        return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")

    assert driver.run_tick(
        settings(tmp_path),
        fetch=authorities.fetch,
        run=outstanding_on_both_reads,
        spawn=authorities.spawn,
    ) == 2
    assert authorities.status_reads == 2
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert "state=unknown" in output
    assert f"state={state}" in output
    assert "process_verified=False" in output
    assert "repair or finish the handle before retrying" in output
    assert "state=in-progress" not in output


@pytest.mark.parametrize(
    ("state", "reason_code"),
    [
        ("preparing", "exact-target-already-queued"),
        ("launching", "exact-target-already-queued"),
        ("running", "exact-target-already-running"),
    ],
)
def test_unverified_handle_with_exact_lock_corroboration_is_in_progress(
    tmp_path: Path, capsys, state: str, reason_code: str
) -> None:
    authorities = Authorities()

    def corroborated(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" in command:
            value = json.loads(result.stdout)
            value["run_handles"] = [
                {
                    "unit": "validate-corroborated.service",
                    "state": state,
                    "sha": SHA,
                    "started_at": "2026-09-11T18:00:00Z",
                    "slot": "validate-corroborated",
                    "log_file": "/logs/validate-corroborated.log",
                    "process_verified": False,
                    "admission": None,
                    "finished_at": None,
                    "result": None,
                    "detail": None,
                    "exit_code": None,
                }
            ]
            return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")
        if "admission-status" in command:
            value = {
                "schema_version": 1,
                "target": SHA,
                "kind": "validate",
                "admissible": False,
                "state": "busy",
                "reason_code": reason_code,
                "detail": f"validate-lock confirms {reason_code} for {SHA}",
            }
            return subprocess.CompletedProcess(command, 1, json.dumps(value), "")
        raise AssertionError(command)

    assert driver.run_tick(
        settings(tmp_path),
        fetch=authorities.fetch,
        run=corroborated,
        spawn=authorities.spawn,
    ) == 0
    assert authorities.status_reads == 2
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert "state=in-progress" in output
    assert "state=unknown" not in output
    assert reason_code in output
    assert "launched=0" in output


@pytest.mark.parametrize("state", ["completed", "refused"])
def test_terminal_or_refused_handles_allow_one_launch_after_stable_absence(
    tmp_path: Path, capsys, state: str
) -> None:
    authorities = Authorities()

    def terminal_on_both_reads(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" not in command:
            return result
        value = json.loads(result.stdout)
        value["run_handles"] = [
            {
                "unit": "validate-finished.service",
                "state": state,
                "sha": SHA,
                "started_at": "2026-09-01T00:00:00Z",
                "slot": "validate-finished",
                "log_file": "/logs/validate-finished.log",
                "process_verified": False,
                "admission": "refused" if state == "refused" else "admitted",
                "finished_at": "2026-09-01T00:01:00Z",
                "result": "refused" if state == "refused" else "failure",
                "detail": "terminal fixture",
                "exit_code": 1,
            }
        ]
        return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")

    assert driver.run_tick(
        settings(tmp_path),
        fetch=authorities.fetch,
        run=terminal_on_both_reads,
        spawn=authorities.spawn,
    ) == 0
    assert authorities.status_reads == 2
    assert len(authorities.launches) == 1
    output = capsys.readouterr().out
    assert "state=launched" in output
    assert "launched=1" in output


@pytest.mark.parametrize(
    "reason_code",
    ["exact-target-already-queued", "exact-target-already-running"],
)
def test_lock_only_exact_target_suppresses_launch_after_two_empty_snapshots(
    tmp_path: Path, capsys, reason_code: str
) -> None:
    authorities = Authorities()
    events: list[str] = []

    def lock_only(command, cwd, timeout):
        if "validate-status" in command:
            events.append("snapshot")
            return authorities.run(command, cwd, timeout)
        if "admission-status" in command:
            events.append("lock")
            report = {
                "schema_version": 1,
                "target": SHA,
                "kind": "validate",
                "admissible": False,
                "state": "busy",
                "reason_code": reason_code,
                "detail": f"validation slot 3 already holds exact target {SHA}",
            }
            return subprocess.CompletedProcess(command, 1, json.dumps(report), "")
        raise AssertionError(command)

    assert (
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=lock_only,
            spawn=authorities.spawn,
        )
        == 0
    )
    assert events == ["snapshot", "lock", "snapshot"]
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert "state=in-progress" in output
    assert "validation slot 3 already holds exact target" in output
    assert "launched=0" in output


def test_absent_record_and_run_launches_exactly_once_across_two_readings(
    tmp_path: Path, capsys
) -> None:
    authorities = Authorities()
    assert read_twice(tmp_path, authorities) == (0, 0)
    assert len(authorities.launches) == 1
    output = capsys.readouterr().out
    assert output.count("state=launched") == 1
    assert authorities.status_reads >= 3
    assert output.count("launched=1") == 1
    assert output.count("no validate started") == 1


def test_failed_prior_launch_is_not_reported_as_never_attempted(
    tmp_path: Path, capsys
) -> None:
    configured = settings(tmp_path)
    driver.atomic_write_json(
        configured.state_dir / "latest.json",
        {
            "schema_version": 1,
            "attempt_id": "prior-attempt",
            "target": SHA,
            "state": "launcher-running",
            "started_at": "2026-09-05T01:00:00+00:00",
            "launcher_unit": PRIOR_UNIT,
            "launcher_process": {
                "pid": 123,
                "start_ticks": 456,
                "boot_id": "old-boot",
            },
        },
    )
    authorities = Authorities()
    assert (
        driver.run_tick(
            configured,
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
        )
        == 0
    )
    assert len(authorities.launches) == 1
    output = capsys.readouterr().out
    assert "prior attempt prior-attempt ended or stopped being observable" in output
    assert "and no ledger row" in output
    assert "never attempted" not in output


def test_legacy_unbound_launcher_remains_unknown_with_its_prior_context(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    attempt = prior_attempt(configured, state="launcher-running")
    del attempt["launcher_unit"]
    attempt["launcher_process"] = {"pid": 123, "start_ticks": 456, "boot_id": "old-boot"}
    driver.atomic_write_json(configured.state_dir / "latest.json", attempt)
    authorities = Authorities()
    with pytest.raises(driver.ObservationUnavailable, match="exact bound launcher identity") as error:
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert "prior attempt interrupted-after-systemd-accept" in str(error.value)
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == attempt


def test_no_prior_handle_does_not_claim_never_attempted(tmp_path: Path, capsys) -> None:
    authorities = Authorities()
    assert (
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "ledger absence alone cannot tell" in output
    assert "never attempted or ended before writing a ledger row" in output


def test_failed_run_handle_is_reported_as_a_prior_attempt(tmp_path: Path, capsys) -> None:
    authorities = Authorities()

    def failed_handle(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" in command:
            value = json.loads(result.stdout)
            value["run_handles"] = [
                {
                    "unit": "validate-failed.service",
                    "state": "completed",
                    "sha": SHA,
                    "started_at": "2026-09-05T01:00:00Z",
                    "finished_at": "2026-09-05T01:05:00Z",
                    "result": "failure",
                    "exit_code": 1,
                }
            ]
            return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")
        return result

    assert (
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=failed_handle,
            spawn=authorities.spawn,
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "prior attempt validate-failed.service ended or stopped being observable" in output
    assert "state=completed result=failure exit=1 and no ledger row" in output


def test_unavailable_queue_refuses_without_starting_a_launcher(
    tmp_path: Path, capsys
) -> None:
    authorities = Authorities(admissible=False)
    assert read_twice(tmp_path, authorities) == (0, 0)
    assert authorities.launches == []
    output = capsys.readouterr().out
    assert output.count("state=busy") == 2
    assert output.count("no validate started") == 2


def test_launch_failure_is_recorded_without_a_ledger_claim(tmp_path: Path, capsys) -> None:
    authorities = Authorities()
    configured = settings(tmp_path)

    def fail(*_args: object) -> dict[str, object]:
        latest = json.loads((configured.state_dir / "latest.json").read_text())
        assert latest["state"] == "starting"
        assert latest["launcher_unit"].endswith("-launcher.service")
        raise driver.CommandNotStarted("fixture launcher refused before Popen")

    assert (
        driver.run_tick(
            configured,
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=fail,
        )
        == 1
    )
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-not-started"
    output = capsys.readouterr().out
    assert "launcher was not started" in output
    assert "state=launch-not-started" in output


def test_pre_recorded_launcher_unit_is_not_used_as_run_truth(
    tmp_path: Path, capsys
) -> None:
    configured = settings(tmp_path)
    prior_attempt(configured)
    authorities = Authorities()
    assert (
        driver.run_tick(
            configured,
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
        )
        == 0
    )
    assert len(authorities.launches) == 1
    assert authorities.launcher_queries == [PRIOR_UNIT]
    assert authorities.launch_units == [PRIOR_UNIT.removesuffix("-launcher.service")]
    assert authorities.status_reads == 2
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["launcher_identity_unconfirmed"] is True
    assert latest["reused_launcher_from"]["attempt_id"] == "interrupted-after-systemd-accept"
    assert "state=launched" in capsys.readouterr().out


def test_launcher_runs_in_its_own_bounded_user_unit(tmp_path: Path, monkeypatch) -> None:
    configured = settings(tmp_path)
    commands: list[list[str]] = []

    def accepted(command, cwd, timeout):
        # The provisioning preflight asks first, within half the launch budget;
        # its own suite is test_integration_tip_validate_provisioning.py.
        if command[0] != "systemd-run":
            assert 0 < timeout <= 3.5
            if command[0] == "systemctl":
                return subprocess.CompletedProcess(command, 0, "HOME=/home/fixture\n", "")
            return subprocess.CompletedProcess(command, 1, "", "")
        commands.append(list(command))
        assert cwd == tmp_path
        # The launch keeps whatever the preflight left of its 7s budget.
        assert 3.5 <= timeout <= 7.0
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(driver, "run_bounded", accepted)
    result = driver.spawn_validate(
        configured,
        SHA,
        "attempt",
        "validate-ops-tick-fixture",
        tmp_path / "validate.log",
        tmp_path / "launcher.log",
        7.0,
    )
    [command] = commands
    assert command[:4] == ["systemd-run", "--user", "--quiet", "--collect"]
    assert "--unit=validate-ops-tick-fixture-launcher" in command
    assert "--property=RuntimeMaxSec=5000" in command
    separator = command.index("--")
    assert command[separator + 1] == str(configured.operational_tool)
    assert command[separator + 2 : separator + 4] == [
        "--run-current-tool",
        "--state-root",
    ]
    assert "--tool-parent-sha" in command[separator:]
    assert command[command.index("--tool-parent-sha") + 1] == "f" * 40
    inner = command.index("--", separator + 1)
    assert command[inner + 1] == "validate-run"
    assert "--no-wait" in command[inner:]
    assert "--skip-if-recorded" in command[inner:]
    assert result["unit"] == "validate-ops-tick-fixture-launcher.service"


@pytest.mark.parametrize("state,sub,pid", [("active", "running", "123"), ("activating", "start", "0"), ("deactivating", "stop", "123")])
def test_live_prior_launcher_suppresses_without_claiming_validation(
    tmp_path: Path, capsys, state, sub, pid
) -> None:
    configured = settings(tmp_path)
    attempt = prior_attempt(configured, state="launch-unknown")
    authorities = Authorities()
    authorities.launcher_properties = dict(
        state=state, sub=sub, pid=pid, load="loaded", invocation="a" * 32
    )
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 0
    assert authorities.launcher_queries == [PRIOR_UNIT]
    assert authorities.status_reads == 1
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == attempt
    output = capsys.readouterr().out
    assert "state=launcher-in-progress" in output
    assert "not validation result evidence" in output
    assert "state=record-present" not in output


@pytest.mark.parametrize("failure", ["timeout", "nonzero", "duplicate", "missing", "wrong-id", "contradictory", "unknown-state"])
def test_unreadable_prior_launcher_never_dispatches(tmp_path: Path, failure) -> None:
    configured = settings(tmp_path)
    attempt = prior_attempt(configured, state="launch-unknown")
    authorities = Authorities()

    def uncertain(command, cwd, timeout):
        if command[0] != "systemctl":
            return authorities.run(command, cwd, timeout)
        assert timeout <= driver.LAUNCHER_OBSERVATION_TIMEOUT_SECONDS
        if failure == "timeout":
            raise driver.ObservationUnavailable("query timed out")
        reply = launcher_reply(PRIOR_UNIT)
        rc = 0
        if failure == "nonzero":
            rc = 1
        elif failure == "duplicate":
            reply += "ActiveState=inactive\n"
        elif failure == "missing":
            reply = reply.replace("LoadState=not-found\n", "")
        elif failure == "wrong-id":
            reply = reply.replace(PRIOR_UNIT, "foreign.service")
        elif failure == "contradictory":
            reply = reply.replace("MainPID=0", "MainPID=123")
        elif failure == "unknown-state":
            reply = reply.replace("ActiveState=inactive", "ActiveState=mystery")
        return subprocess.CompletedProcess(command, rc, reply, "query failed" if rc else "")

    with pytest.raises(driver.ObservationUnavailable):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=uncertain, spawn=authorities.spawn
        )
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == attempt


@pytest.mark.parametrize("terminal", [False, True])
def test_unconfirmed_same_tip_retry_keeps_both_unit_identities(tmp_path: Path, terminal) -> None:
    configured = settings(tmp_path)
    old = prior_attempt(configured, state="launch-unknown", launcher_log="/retained/old.log")
    authorities = Authorities()
    if terminal:
        authorities.launcher_properties = dict(
            state="failed", load="loaded", sub="failed", invocation="a" * 32
        )
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 0
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["launcher_unit"] == old["launcher_unit"]
    assert latest["unit"] == old["launcher_unit"].replace("-launcher.service", ".service")
    assert latest["attempt_id"] != old["attempt_id"]
    assert latest["launcher_log"] != old["launcher_log"]
    assert latest["reused_launcher_from"]["launcher_log"] == "/retained/old.log"
    assert latest["launcher_identity_unconfirmed"] is True
    assert authorities.status_reads == 2
    assert len(authorities.launches) == 1


def test_unconfirmed_different_tip_does_not_replace_prior_identity(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown", target="f" * 40)
    authorities = Authorities()
    with pytest.raises(driver.ObservationUnavailable, match="another tip has an unconfirmed"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


def retained_terminal_handle(configured, attempt, **updates):
    """Create input for the real current registry parser, not its projection."""
    unit = attempt["launcher_unit"].removesuffix("-launcher.service") + ".service"
    path = configured.state_root / "ignored" / "validate" / "runs" / f"{unit.removesuffix('.service')}.json"
    record = {
        "schema_version": 1,
        "producer": "ci-hub/validate/run_registry.py",
        "kind": "validate",
        "state": "completed",
        "unit": unit,
        "target": attempt["target"],
        "repo": driver.REPO,
        "agent": "ops-tick",
        "checkout": str(configured.state_root / "worktrees" / "validate" / "retired"),
        "source_checkout": str(configured.source_checkout),
        "temporary_checkout": True,
        "cargo_home": str(configured.state_root / "ignored" / "validate" / "cargo-homes" / "retired"),
        "log": str(configured.state_root / "ignored" / "validate" / "old.log"),
        "started_at": "2026-09-05T01:00:00+00:00",
        "parent_checkout_head": "f" * 40,
        "admission": "ci-hub validate-lock",
        "pane_role": "observer-only",
        "validate_lock_child_deadline_seconds": 3600,
        "e2e_result_root": "/retired/e2e",
        "safe_ci_dag_runner_log_dir": "/retired/dag",
        "hermit_run_timeout_seconds": 3300,
        "finished_at": "2026-09-05T02:00:00+00:00",
        "result": "failure",
        "exit_code": 1,
        "final_validate_status": "FAILED",
        **updates,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(record))
    return path, record


class RetainedHandleAuthorities(Authorities):
    """Execute only the real bounded registry reader; simulate other services."""

    def __init__(self, configured, **kwargs):
        super().__init__(**kwargs)
        self.configured = configured
        self.registry_reads = []

    def run(self, command, cwd, timeout):
        if "inspect-current-records" not in command:
            return super().run(command, cwd, timeout)
        assert command[:4] == [
            sys.executable, "-B",
            str(self.configured.tool_root / "ci-hub" / "validate" / "run_registry.py"),
            "inspect-current-records",
        ]
        assert len(command) == 6 and command[4] == "--path"
        assert cwd == self.configured.state_root
        assert 0 < timeout <= driver.CONSUMED_IDENTITY_TIMEOUT_SECONDS
        self.registry_reads.append((command, timeout))
        return driver.run_bounded(command, cwd, timeout)


@pytest.mark.parametrize("outcome", [
    {},
    {"result": "success", "exit_code": 0, "final_validate_status": "PASSED"},
    {"result": "no-result", "exit_code": 75, "final_validate_status": "COULD_NOT_RUN"},
    {"result": "no-result", "exit_code": 75, "final_validate_status": "COULD_NOT_RUN",
     "service_result_schema": 5, "selection_mode": None, "executed_nodes": 0,
     "executed_tests": None, "passed_tests": None, "scorecard_writeback": None,
     "detail": "launcher refused: missing KVM device"},
    {"state": "refused", "result": "systemd-launch-refused", "exit_code": 3, "detail": "stale-base"},
    {"state": "killed", "result": "terminated", "exit_code": 143, "signal": 15},
])
def test_retry_then_retained_terminal_identity_allows_next_tip(tmp_path: Path, outcome) -> None:
    configured = settings(tmp_path)
    old = prior_attempt(configured, state="launch-unknown")
    retry = Authorities()
    assert driver.run_tick(
        configured, fetch=retry.fetch, run=retry.run, spawn=retry.spawn
    ) == 0
    acknowledged = json.loads((configured.state_dir / "latest.json").read_text())
    assert acknowledged["state"] == "launcher-accepted"
    assert acknowledged["launcher_identity_unconfirmed"] is True
    assert acknowledged["launcher_unit"] == old["launcher_unit"]
    path, record = retained_terminal_handle(configured, acknowledged, **outcome)
    retained_bytes = path.read_bytes()
    metadata = path.stat()
    next_tip = RetainedHandleAuthorities(configured, target="e" * 40)
    assert driver.run_tick(
        configured, fetch=next_tip.fetch, run=next_tip.run, spawn=next_tip.spawn
    ) == 0
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["target"] == next_tip.target
    assert latest["launcher_unit"] != old["launcher_unit"]
    assert latest["launcher_identity_unconfirmed"] is False
    proof = latest["consumed_prior_launcher"]
    assert proof["record_path"] == str(path)
    assert (proof["record_dev"], proof["record_ino"]) == (metadata.st_dev, metadata.st_ino)
    assert proof["tool_parent_sha"] == configured.tool_parent_sha
    for field in ("unit", "target", "state", "result", "exit_code", "finished_at"):
        assert proof[field] == record[field]
    assert proof["detail"] == record.get("detail")
    assert path.read_bytes() == retained_bytes
    assert next_tip.status_reads == 2
    assert len(next_tip.registry_reads) == 1
    assert len(next_tip.launches) == 1


@pytest.mark.parametrize("bad", [
    "missing", "malformed", "schema", "producer", "target", "unit", "repo",
    "nonterminal", "unknown", "unfinished", "exit-type", "attempt-unit",
])
def test_unconfirmed_identity_needs_exact_readable_terminal_record(tmp_path: Path, bad) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown", target="f" * 40)
    changes = {
        "schema": {"schema_version": 2},
        "producer": {"producer": "foreign"},
        "target": {"target": "e" * 40},
        "unit": {"unit": "validate-unrelated.service"},
        "repo": {"repo": "rrnewton/reverie"},
        "nonterminal": {"state": "running", "finished_at": None},
        "unknown": {"state": "unknown", "result": "unknown", "detail": "observation unavailable"},
        "unfinished": {"finished_at": None},
        "exit-type": {"exit_code": True},
    }
    path, _record = retained_terminal_handle(configured, prior, **changes.get(bad, {}))
    if bad == "missing":
        path.unlink()
    elif bad == "malformed":
        path.write_text("{not json")
    elif bad == "attempt-unit":
        prior["unit"] = "validate-unrelated.service"
        driver.atomic_write_json(configured.state_dir / "latest.json", prior)
    authorities = RetainedHandleAuthorities(configured)
    with pytest.raises(driver.ObservationUnavailable, match="no retained terminal consumption proof"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


@pytest.mark.parametrize("mutation", ["deleted", "replaced", "symlink", "report-path", "unreadable", "timeout"])
def test_terminal_identity_inspection_refuses_races_and_uncertainty(tmp_path: Path, mutation) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown", target="f" * 40)
    path, _record = retained_terminal_handle(configured, prior)
    authorities = RetainedHandleAuthorities(configured)

    def raced(command, cwd, timeout):
        reply = authorities.run(command, cwd, timeout)
        if "inspect-current-records" not in command:
            return reply
        if mutation == "timeout":
            raise driver.ObservationUnavailable("registry query timed out")
        if mutation in {"deleted", "replaced", "symlink"}:
            retained = path.with_suffix(".retained")
            path.rename(retained)
            if mutation == "replaced":
                path.write_bytes(retained.read_bytes())
            elif mutation == "symlink":
                path.symlink_to(retained)
        else:
            report = json.loads(reply.stdout)
            if mutation == "report-path":
                report["records"][0]["path"] += ".foreign"
            else:
                report["unreadable"] = [{"path": str(path), "error": "concurrent unreadable"}]
            reply.stdout = json.dumps(report)
        return reply

    with pytest.raises(driver.ObservationUnavailable, match="no retained terminal consumption proof"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=raced, spawn=authorities.spawn
        )
    assert len(authorities.registry_reads) == 1
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


def test_symlink_terminal_identity_is_refused_before_read(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown", target="f" * 40)
    path, _record = retained_terminal_handle(configured, prior)
    real = path.with_suffix(".retained")
    path.rename(real)
    path.symlink_to(real)
    authorities = RetainedHandleAuthorities(configured)
    with pytest.raises(driver.ObservationUnavailable, match="canonical regular handle"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert authorities.registry_reads == []
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


@pytest.mark.parametrize("evidence", ["record", "in-flight", "busy"])
def test_consumed_identity_still_requires_fresh_admission_and_second_status(tmp_path: Path, evidence) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown", target="f" * 40)
    retained_terminal_handle(configured, prior)
    authorities = RetainedHandleAuthorities(configured, admissible=evidence != "busy")

    def handoff(command, cwd, timeout):
        if "validate-status" in command and authorities.status_reads == 1:
            authorities.records = int(evidence == "record")
            authorities.in_progress = int(evidence == "in-flight")
        return authorities.run(command, cwd, timeout)

    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=handoff, spawn=authorities.spawn
    ) == 0
    assert len(authorities.registry_reads) == 1
    assert authorities.status_reads == 2
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


def test_consumption_query_clamps_to_remaining_global_deadline(tmp_path: Path, monkeypatch) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown", target="f" * 40)
    retained_terminal_handle(configured, prior)
    authorities = RetainedHandleAuthorities(configured)
    elapsed = [0.0]
    monkeypatch.setattr(driver.time, "monotonic", lambda: elapsed[0])

    def slow_observation(command, cwd, timeout):
        if "inspect-current-records" in command:
            assert timeout == 2.0
            elapsed[0] += timeout
            raise driver.ObservationUnavailable("bounded retained-record read timed out")
        reply = authorities.run(command, cwd, timeout)
        if command[0] == "systemctl":
            elapsed[0] = configured.total_timeout_seconds - 2.0
        return reply

    with pytest.raises(driver.ObservationUnavailable, match="bounded retained-record read timed out"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=slow_observation, spawn=authorities.spawn
        )
    assert elapsed[0] == configured.total_timeout_seconds
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


@pytest.mark.parametrize("terminal", [False, True])
def test_acknowledged_ended_launcher_still_requires_second_status(tmp_path: Path, terminal) -> None:
    configured = settings(tmp_path)
    prior_attempt(configured, state="launcher-accepted", launcher={"accepted": True, "unit": PRIOR_UNIT})
    authorities = Authorities()
    if terminal:
        authorities.launcher_properties = dict(
            state="failed", load="loaded", sub="failed", invocation="a" * 32
        )

    def handoff(command, cwd, timeout):
        if "validate-status" in command and authorities.status_reads == 1:
            authorities.in_progress = 1
        return authorities.run(command, cwd, timeout)

    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=handoff, spawn=authorities.spawn
    ) == 0
    assert authorities.status_reads == 2
    assert authorities.launcher_queries == [PRIOR_UNIT]
    assert authorities.launches == []


class PreflightClient:
    """A finished provisioning-preflight query: the manager environment, then
    no submodules. The preflight has its own suite in
    test_integration_tip_validate_provisioning.py."""

    pid = 4242

    def __init__(self, command) -> None:
        manager = command[0] == "systemctl"
        self.returncode = 0 if manager else 1
        self.output = "HOME=/home/fixture\n" if manager else ""

    def communicate(self, timeout=None):
        return self.output, ""


def test_real_spawn_timeout_preserves_accepted_but_unconfirmed_launcher(tmp_path: Path, monkeypatch, capsys) -> None:
    configured = settings(tmp_path)
    authorities = Authorities()
    dispatched = []

    class PendingClient:
        pid = 87654321
        calls = 0

        def communicate(self, timeout=None):
            self.calls += 1
            if self.calls == 1:
                authorities.launcher_properties = dict(
                    state="active", load="loaded", sub="running", pid="123", invocation="a" * 32
                )
                raise subprocess.TimeoutExpired("systemd-run", timeout)
            return "", ""

    def popen(command, **_kwargs):
        if command[0] != "systemd-run":
            return PreflightClient(command)
        dispatched.append(command)
        return PendingClient()

    killed = []
    monkeypatch.setattr(driver.subprocess, "Popen", popen)
    monkeypatch.setattr(driver.os, "killpg", lambda pid, sig: killed.append((pid, sig)))
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run
    ) == 2
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-unknown"
    assert "finished_at" not in latest
    assert latest["launcher_identity_unconfirmed"] is True
    assert f"--unit={latest['launcher_unit'].removesuffix('.service')}" in dispatched[0]
    assert killed == [(PendingClient.pid, driver.signal.SIGKILL)]
    assert "launched=unknown" in capsys.readouterr().out
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 0
    assert authorities.launches == []
    assert authorities.launcher_queries == [latest["launcher_unit"]]
    assert len(dispatched) == 1


def test_popen_failure_is_definitely_not_started(tmp_path: Path, monkeypatch) -> None:
    configured = settings(tmp_path)
    authorities = Authorities()

    attempted = []

    def refused(command, **_kwargs):
        if command[0] != "systemd-run":
            return PreflightClient(command)
        attempted.append(command[0])
        raise OSError("Popen could not execute systemd-run")

    monkeypatch.setattr(driver.subprocess, "Popen", refused)
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run
    ) == 1
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-not-started"
    assert "finished_at" in latest
    assert attempted == ["systemd-run"]
    assert "Popen could not execute systemd-run" in latest["detail"]


def test_missing_launcher_prerequisite_writes_no_intent(tmp_path: Path) -> None:
    configured = replace(settings(tmp_path), operational_tool=None)
    authorities = Authorities()
    with pytest.raises(driver.CommandNotStarted, match="short-lived tool snapshot"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert not (configured.state_dir / "latest.json").exists()
    assert authorities.launches == []


def test_prior_launcher_query_is_clamped_to_global_deadline(tmp_path: Path, monkeypatch) -> None:
    configured = settings(tmp_path)
    prior = prior_attempt(configured, state="launch-unknown")
    authorities = TimedAuthorities(durations={"fetch": 24.0, "status-1": 49.0, "launcher": 3.0})
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    with pytest.raises(driver.ObservationUnavailable, match="launcher timed out"):
        driver.run_tick(
            configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
        )
    assert authorities.elapsed == 75.0
    assert authorities.bounds == [("fetch", 25.0), ("status-1", 50.0), ("launcher", 2.0)]
    assert authorities.launches == []
    assert json.loads((configured.state_dir / "latest.json").read_text()) == prior


def test_deadline_expiring_during_intent_write_is_not_a_dispatch(tmp_path: Path, monkeypatch) -> None:
    configured = settings(tmp_path)
    authorities = TimedAuthorities()
    monkeypatch.setattr(driver.time, "monotonic", lambda: authorities.elapsed)
    original = driver.atomic_write_json

    def write_then_expire(path, record):
        original(path, record)
        authorities.elapsed = 75.0

    monkeypatch.setattr(driver, "atomic_write_json", write_then_expire)
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 1
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-not-started"
    assert "deadline expired" in latest["detail"]
    assert authorities.launches == []
    assert not (configured.state_dir / "logs").exists()


def test_retry_pre_dispatch_refusal_does_not_clear_earlier_uncertainty(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    prior_attempt(configured, state="launch-unknown")
    authorities = Authorities()

    def refuse(*_args):
        raise driver.CommandNotStarted("this retry could not execute the client")

    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=refuse
    ) == 2
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-unknown"
    assert latest["launcher_unit"] == PRIOR_UNIT
    assert latest["launcher_identity_unconfirmed"] is True
    assert "finished_at" not in latest


def test_acknowledged_different_tip_absence_allows_fresh_authority_path(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    target = "f" * 40
    old_unit = f"validate-ops-tick-{target[:12]}-abcdef123456-launcher.service"
    prior_attempt(configured, state="launcher-accepted", target=target, launcher={"accepted": True, "unit": old_unit})
    authorities = Authorities()
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=authorities.spawn
    ) == 0
    assert authorities.launcher_queries == [old_unit]
    assert authorities.status_reads == 2
    assert len(authorities.launches) == 1
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["target"] == SHA
    assert latest["launcher_unit"] != old_unit
    assert latest["launcher_identity_unconfirmed"] is False


@pytest.mark.parametrize("accepted", [False, True])
def test_evidence_write_failure_does_not_claim_dispatch_failed(tmp_path: Path, monkeypatch, capsys, accepted) -> None:
    configured = settings(tmp_path)
    authorities = Authorities()
    original = driver.atomic_write_json
    writes = 0

    def write_once(path, record):
        nonlocal writes
        writes += 1
        if writes > 1:
            raise OSError("fixture disk full after intent")
        original(path, record)

    def spawn(*args):
        if accepted:
            return authorities.spawn(*args)
        raise driver.ObservationUnavailable("acceptance was not observable")

    monkeypatch.setattr(driver, "atomic_write_json", write_once)
    assert driver.run_tick(
        configured, fetch=authorities.fetch, run=authorities.run, spawn=spawn
    ) == 2
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "starting"
    assert "finished_at" not in latest
    output = capsys.readouterr().out
    assert f"launched={'1' if accepted else 'unknown'}" in output
    assert "launch-failed" not in output


def test_launcher_refuses_a_short_lived_tool_root(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    configured = replace(configured, operational_tool=None)
    with pytest.raises(driver.ObservationUnavailable, match="short-lived tool snapshot"):
        driver.spawn_validate(
            configured,
            SHA,
            "attempt",
            "validate-ops-tick-fixture",
            tmp_path / "validate.log",
            tmp_path / "launcher.log",
            7.0,
        )


def test_unknown_admission_is_visible_and_never_launches(tmp_path: Path) -> None:
    authorities = Authorities()

    def unknown(command, cwd, timeout):
        if "admission-status" not in command:
            return authorities.run(command, cwd, timeout)
        report = {
            "schema_version": 1,
            "target": SHA,
            "kind": "validate",
            "admissible": False,
            "state": "unknown",
            "reason_code": "memory-admission-unknown",
            "detail": "cannot read MemAvailable",
        }
        return subprocess.CompletedProcess(command, 2, json.dumps(report), "")

    assert (
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=unknown,
            spawn=authorities.spawn,
        )
        == 2
    )
    assert authorities.launches == []


@pytest.mark.parametrize("state", ["stale", "quarantined", "refused"])
def test_non_busy_admission_refusals_stay_visible(
    tmp_path: Path, state: str
) -> None:
    authorities = Authorities()

    def refused(command, cwd, timeout):
        if "admission-status" not in command:
            return authorities.run(command, cwd, timeout)
        report = {
            "schema_version": 1,
            "target": SHA,
            "kind": "validate",
            "admissible": False,
            "state": state,
            "reason_code": f"fixture-{state}",
            "detail": f"fixture {state} refusal",
        }
        return subprocess.CompletedProcess(command, 3, json.dumps(report), "")

    assert (
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=refused,
            spawn=authorities.spawn,
        )
        == 2
    )
    assert authorities.launches == []


def test_unattributable_unreadable_handle_refuses_the_decision(tmp_path: Path) -> None:
    authorities = Authorities()

    def unreadable(command, cwd, timeout):
        result = authorities.run(command, cwd, timeout)
        if "validate-status" in command:
            value = json.loads(result.stdout)
            value["unreadable_handles"] = [
                {"path": "/state/broken.json", "error": "target field is unreadable"}
            ]
            return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")
        return result

    with pytest.raises(driver.ObservationUnavailable, match="cannot be proved unrelated"):
        driver.run_tick(
            settings(tmp_path),
            fetch=authorities.fetch,
            run=unreadable,
            spawn=authorities.spawn,
        )
    assert authorities.launches == []


def test_tick_configuration_is_hourly_serial_and_bounded() -> None:
    config = yaml.safe_load((ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text())
    reminder = next(
        row for row in config["reminders"] if row["name"] == "integration_tip_validate"
    )
    assert reminder["cadence_secs"] == 3600
    assert reminder["gate"]["timeout_secs"] == 90
    assert driver.DEFAULT_TOTAL_TIMEOUT_SECONDS == 75
    assert reminder["gate"]["capture"] is True
    assert reminder["gate"]["when"] == "failure"
    assert "parallel" not in reminder["gate"]
    assert "integration_tip_validate.py" in reminder["gate"]["cmd"]
