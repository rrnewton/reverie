from __future__ import annotations

import fcntl
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_reporting_target",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

SHA = "1234567890abcdef1234567890abcdef12345678"


def configured(tmp_path: Path):
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


def status_report(
    *,
    verdict: str = "NOT-VALIDATED",
    exit_code: int = 4,
    exact_records: list[dict[str, object]] | None = None,
    run_handles: list[dict[str, object]] | None = None,
) -> dict[str, object]:
    records = [] if exact_records is None else exact_records
    handles = [] if run_handles is None else run_handles
    return {
        "schema_version": 2,
        "repo": driver.REPO,
        "sha": SHA,
        "verdict": verdict,
        "exit_code": exit_code,
        "record_count": len(records),
        "exact_records": records,
        "qualifying_count": len(records) if verdict == "VALIDATED" else 0,
        "disqualified_count": 0 if verdict == "VALIDATED" else len(records),
        "ledger_freshness": (
            None
            if records
            else {"state": "current", "local_absence_is_authoritative": True}
        ),
        "in_progress_count": 0,
        "in_progress": [],
        "run_handles": handles,
        "unreadable_handles": [],
    }


class Authorities:
    def __init__(self, report: dict[str, object]) -> None:
        self.report = report
        self.launches = 0

    def fetch(self, _settings, _timeout: float) -> str:
        return SHA

    def run(self, command, _cwd: Path, _timeout: float):
        if "validate-status" in command:
            return subprocess.CompletedProcess(
                command,
                int(self.report["exit_code"]),
                json.dumps(self.report),
                "",
            )
        if "admission-status" in command:
            value = {
                "schema_version": 1,
                "target": SHA,
                "kind": "validate",
                "admissible": True,
                "state": "admissible",
                "reason_code": "immediate-admission-available",
                "detail": "validation slot 0 allows an immediate attempt",
            }
            return subprocess.CompletedProcess(command, 0, json.dumps(value), "")
        raise AssertionError(command)

    def spawn(self, *_args) -> dict[str, object]:
        self.launches += 1
        return {"unit": "validate-fixture-launcher.service", "accepted": True}


@pytest.mark.parametrize(
    ("verdict", "result", "exit_code", "state"),
    [
        ("FAILED", "fail", 3, "failed"),
        ("NO-RESULT", "no_result", 4, "no-result"),
    ],
)
def test_non_green_record_surfaces_cause_without_duplicate_launch(
    tmp_path: Path,
    capsys,
    verdict: str,
    result: str,
    exit_code: int,
    state: str,
) -> None:
    detail = "build.rust_scripts_in_pinned_root (exit=1, detail=compiler failed)"
    authorities = Authorities(
        status_report(
            verdict=verdict,
            exit_code=exit_code,
            exact_records=[
                {
                    "verdict": verdict,
                    "result": result,
                    "exit_code": exit_code,
                    "started_at": "2026-09-06T01:00:00Z",
                    "finished_at": "2026-09-06T01:10:00Z",
                    "detail": detail,
                }
            ],
        )
    )
    assert (
        driver.run_tick(
            configured(tmp_path),
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
        )
        == exit_code
    )
    output = capsys.readouterr().out
    assert f"state={state}" in output
    assert f"verdict={verdict}" in output
    assert f"record_detail={verdict} result={result} exit={exit_code} detail={detail}" in output
    assert "next_action=inspect the recorded outcome" in output
    assert authorities.launches == 0


def test_malformed_exact_record_refuses_without_launch(tmp_path: Path) -> None:
    report = status_report(
        verdict="FAILED",
        exit_code=3,
        exact_records=[
            {
                "verdict": "FAILED",
                "result": "fail",
                "exit_code": 1,
                "detail": "",
            }
        ],
    )
    authorities = Authorities(report)
    with pytest.raises(driver.ObservationUnavailable, match="malformed exact record"):
        driver.run_tick(
            configured(tmp_path),
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
        )
    assert authorities.launches == 0


def test_unreadable_validate_status_refuses_without_launch(tmp_path: Path) -> None:
    authorities = Authorities(status_report())

    def unreadable(command, _cwd: Path, _timeout: float):
        return subprocess.CompletedProcess(command, 4, "{", "broken status output")

    with pytest.raises(driver.ObservationUnavailable, match="unreadable JSON"):
        driver.run_tick(
            configured(tmp_path),
            fetch=authorities.fetch,
            run=unreadable,
            spawn=authorities.spawn,
        )
    assert authorities.launches == 0


def test_terminal_run_names_outcome_detail_and_next_action(
    tmp_path: Path, capsys
) -> None:
    report = status_report(
        run_handles=[
            {
                "unit": "validate-failed.service",
                "state": "completed",
                "sha": SHA,
                "started_at": "2026-09-06T01:00:00Z",
                "finished_at": "2026-09-06T01:10:00Z",
                "result": "failure",
                "detail": "test runner exited before writing a ledger row",
                "exit_code": 1,
            }
        ]
    )
    authorities = Authorities(report)
    assert (
        driver.run_tick(
            configured(tmp_path),
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
            dry_run=True,
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "state=completed result=failure exit=1" in output
    assert "detail=test runner exited before writing a ledger row" in output
    assert "next_action=run without --dry-run to start one validate" in output
    assert authorities.launches == 0


def test_dry_run_and_live_launch_have_distinct_mutation_boundary(
    tmp_path: Path, capsys
) -> None:
    configured_settings = configured(tmp_path)
    authorities = Authorities(status_report())

    assert (
        driver.run_tick(
            configured_settings,
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
            dry_run=True,
        )
        == 0
    )
    assert authorities.launches == 0
    assert not (configured_settings.state_dir / "latest.json").exists()

    assert (
        driver.run_tick(
            configured_settings,
            fetch=authorities.fetch,
            run=authorities.run,
            spawn=authorities.spawn,
        )
        == 0
    )
    assert authorities.launches == 1
    assert (configured_settings.state_dir / "latest.json").is_file()
    output = capsys.readouterr().out
    assert "state=would-launch" in output
    assert "state=launched" in output


def test_concurrent_tick_lock_prevents_a_second_decision(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    state_dir = tmp_path / "tick-state"
    state_dir.mkdir()
    lock_path = state_dir / "tick.lock"

    def must_not_run(*_args, **_kwargs):
        raise AssertionError("a concurrent tick must stop before reading authorities")

    monkeypatch.setattr(driver, "run_tick", must_not_run)
    with lock_path.open("a+") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        assert (
            driver.main(
                [
                    "--state-root",
                    str(tmp_path),
                    "--state-dir",
                    str(state_dir),
                    "--dry-run",
                ]
            )
            == 0
        )
    output = capsys.readouterr().out
    assert "state=in-progress" in output
    assert "no second decision started" in output
