"""Unreadable run handles: set aside only what provably predates the exact tip.

Measured 2026-09-28: devbig014 had 72 unreadable handles and devbig030 had
149, and every hourly decision on both hosts refused on them. The devbig030
handles last changed on 2026-08-11, seven weeks before the tip they blocked.
These tests pin both halves of the rule: such handles no longer refuse, and
anything that cannot be proved older than the tip still does, while a live
exact-tip run or the exact validate-lock target still suppresses the launch.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_unreadable_target",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

SHA = "1234567890abcdef1234567890abcdef12345678"
DAY = 86400


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


def write_handles(tmp_path: Path, *names: str) -> list[Path]:
    runs = tmp_path / "ignored" / "validate" / "runs"
    runs.mkdir(parents=True, exist_ok=True)
    paths = []
    for name in names:
        path = runs / name
        path.write_text('{"schema": "not-a-run-handle"}\n')
        paths.append(path)
    return paths


class Authorities:
    """Exact validate-status, admission, and git answers without real commands."""

    def __init__(
        self,
        unreadable: list[Path],
        *,
        commit_time: int,
        in_progress: int = 0,
        admission_reason: str = "immediate-admission-available",
    ) -> None:
        self.unreadable = unreadable
        self.commit_time = commit_time
        self.in_progress = in_progress
        self.admission_reason = admission_reason
        self.commands: list[list[str]] = []
        self.launches: list[str] = []

    def fetch(self, _settings, _timeout: float) -> str:
        return SHA

    def run(self, command, _cwd: Path, timeout: float):
        assert timeout > 0
        command = list(command)
        self.commands.append(command)
        if command[:2] == ["git", "-C"]:
            assert command[3:] == ["show", "-s", "--format=%ct", f"{SHA}^{{commit}}"]
            return subprocess.CompletedProcess(command, 0, f"{self.commit_time}\n", "")
        if "validate-status" in command:
            handles = [
                {
                    "unit": "validate-live.service",
                    "state": "running",
                    "sha": SHA,
                    "started_at": "2026-09-28T16:00:00Z",
                    "slot": "slot-1",
                    "log_file": "/logs/validate-live.log",
                    "process_verified": True,
                    "admission": "admitted",
                    "finished_at": None,
                    "result": None,
                    "detail": None,
                    "exit_code": None,
                }
                for _ in range(self.in_progress)
            ]
            report = {
                "schema_version": driver.EXACT_VALIDATE_STATUS_SCHEMA_VERSION,
                "repo": driver.REPO,
                "sha": SHA,
                "verdict": "NOT-VALIDATED",
                "exit_code": 4,
                "record_count": 0,
                "exact_records": [],
                "ledger_freshness": {"state": "current", "local_absence_is_authoritative": True},
                "in_progress_count": self.in_progress,
                "in_progress": [
                    {"sha": SHA, "in_progress": True, "summary": "live run"}
                    for _ in range(self.in_progress)
                ],
                "run_handles": handles,
                "unreadable_handles": [
                    {"path": str(path), "error": "validation run handle has no repo"}
                    for path in self.unreadable
                ],
            }
            return subprocess.CompletedProcess(command, 4, json.dumps(report), "")
        if "admission-status" in command:
            admissible = self.admission_reason == "immediate-admission-available"
            report = {
                "schema_version": 1,
                "target": SHA,
                "kind": "validate",
                "admissible": admissible,
                "state": "admissible" if admissible else "busy",
                "reason_code": self.admission_reason,
                "detail": (
                    "validation slot 2 allows an immediate attempt"
                    if admissible
                    else f"validation slot 3 already holds exact target {SHA}"
                ),
            }
            return subprocess.CompletedProcess(
                command, 0 if admissible else 1, json.dumps(report), ""
            )
        raise AssertionError(command)

    def spawn(self, _settings, target, attempt_id, unit, *_rest):
        assert target == SHA
        self.launches.append(attempt_id)
        return {"unit": f"{unit}-launcher.service", "accepted": True}

    def git_queries(self) -> int:
        return sum(command[:2] == ["git", "-C"] for command in self.commands)

    def tick(self, settings, **kwargs) -> int:
        return driver.run_tick(
            settings, fetch=self.fetch, run=self.run, spawn=self.spawn, **kwargs
        )


def test_handles_older_than_the_tip_commit_are_recorded_and_do_not_refuse(
    tmp_path: Path, capsys
) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-legacy-a.json", "validate-legacy-b.json")
    # Files created now are provably older than a tip committed two days later.
    authorities = Authorities(paths, commit_time=int(time.time()) + 2 * DAY)

    assert authorities.tick(settings) == 0

    assert len(authorities.launches) == 1
    output = capsys.readouterr().out
    assert "state=launched" in output
    assert "unrelated_unreadable_handles=2" in output
    record_path = settings.state_dir / driver.UNRELATED_UNREADABLE_RECORD
    assert f"unrelated_unreadable_record={record_path}" in output
    record = json.loads(record_path.read_text())
    assert record["target"] == SHA
    assert record["count"] == 2
    assert [row["path"] for row in record["handles"]] == [str(path) for path in paths]
    assert all(row["error"] == "validation run handle has no repo" for row in record["handles"])
    assert "minus 3600s" in record["rule"]


def test_a_handle_changed_after_the_tip_commit_still_refuses(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-fresh.json")
    authorities = Authorities(paths, commit_time=int(time.time()) - DAY)

    with pytest.raises(driver.ObservationUnavailable, match="cannot be proved unrelated") as error:
        authorities.tick(settings)

    assert "1 may postdate exact tip" in str(error.value)
    assert str(paths[0]) in str(error.value)
    assert authorities.launches == []
    assert not (settings.state_dir / driver.UNRELATED_UNREADABLE_RECORD).exists()
    assert not (settings.state_dir / "latest.json").exists()


def test_the_clock_margin_keeps_a_handle_just_before_the_commit_blocking(
    tmp_path: Path,
) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-skewed.json")
    # Written ten minutes before the commit time: inside the one-hour margin.
    authorities = Authorities(paths, commit_time=int(time.time()) + 600)

    with pytest.raises(driver.ObservationUnavailable, match="cannot be proved unrelated"):
        authorities.tick(settings)
    assert authorities.launches == []


def test_a_backdated_mtime_does_not_make_a_fresh_handle_look_old(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-touched.json")
    ancient = time.time() - 365 * DAY
    os.utime(paths[0], (ancient, ancient))
    assert driver.handle_last_change(paths[0]) >= time.time() - 60
    authorities = Authorities(paths, commit_time=int(time.time()) - DAY)

    with pytest.raises(driver.ObservationUnavailable, match="cannot be proved unrelated"):
        authorities.tick(settings)
    assert authorities.launches == []


def test_a_dangling_symlink_handle_refuses(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    runs = tmp_path / "ignored" / "validate" / "runs"
    runs.mkdir(parents=True)
    link = runs / "validate-dangling.json"
    link.symlink_to(tmp_path / "gone.json")
    authorities = Authorities([link], commit_time=int(time.time()) + 2 * DAY)

    with pytest.raises(driver.ObservationUnavailable, match="cannot inspect it"):
        authorities.tick(settings)
    assert authorities.git_queries() == 0
    assert authorities.launches == []


def test_an_uninspectable_handle_refuses_even_beside_old_ones(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-legacy.json")
    missing = tmp_path / "ignored" / "validate" / "runs" / "validate-vanished.json"
    authorities = Authorities([*paths, missing], commit_time=int(time.time()) + 2 * DAY)

    with pytest.raises(driver.ObservationUnavailable, match="cannot be proved unrelated") as error:
        authorities.tick(settings)

    assert "could not read 2 run handle(s)" in str(error.value)
    assert "cannot inspect it" in str(error.value)
    assert authorities.git_queries() == 0
    assert authorities.launches == []


def test_an_unreadable_entry_without_a_path_refuses(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    authorities = Authorities([], commit_time=int(time.time()) + 2 * DAY)
    original = authorities.run

    def pathless(command, cwd, timeout):
        result = original(command, cwd, timeout)
        if "validate-status" in command:
            value = json.loads(result.stdout)
            value["unreadable_handles"] = [{"error": "no path"}]
            return subprocess.CompletedProcess(command, result.returncode, json.dumps(value), "")
        return result

    authorities.run = pathless
    with pytest.raises(driver.ObservationUnavailable, match="without a path"):
        authorities.tick(settings)
    assert authorities.launches == []


def test_an_unreadable_commit_time_refuses(tmp_path: Path) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-legacy.json")
    authorities = Authorities(paths, commit_time=0)
    original = authorities.run

    def missing_commit(command, cwd, timeout):
        if command[:2] == ["git", "-C"]:
            return subprocess.CompletedProcess(command, 128, "", "fatal: bad object\n")
        return original(command, cwd, timeout)

    authorities.run = missing_commit
    with pytest.raises(driver.ObservationUnavailable, match="cannot read the commit time"):
        authorities.tick(settings)
    assert authorities.launches == []


def test_an_unwritable_evidence_record_refuses(tmp_path: Path, monkeypatch) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-legacy.json")
    authorities = Authorities(paths, commit_time=int(time.time()) + 2 * DAY)
    original = driver.atomic_write_json

    def refuse_record(path, value):
        if Path(path).name == driver.UNRELATED_UNREADABLE_RECORD:
            raise OSError("read-only file system")
        return original(path, value)

    monkeypatch.setattr(driver, "atomic_write_json", refuse_record)
    with pytest.raises(driver.ObservationUnavailable, match="cannot record 1 unrelated"):
        authorities.tick(settings)
    assert authorities.launches == []


def test_a_live_exact_tip_run_still_blocks_with_unrelated_handles_present(
    tmp_path: Path, capsys
) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-legacy-a.json", "validate-legacy-b.json")
    authorities = Authorities(
        paths, commit_time=int(time.time()) + 2 * DAY, in_progress=1
    )

    assert authorities.tick(settings) == 0

    assert authorities.launches == []
    output = capsys.readouterr().out
    assert "state=in-progress" in output
    assert "unit=validate-live.service state=running" in output
    assert "process_verified=True" in output
    assert "launched=0" in output
    assert "unrelated_unreadable_handles=2" in output


@pytest.mark.parametrize(
    "reason_code", ["exact-target-already-queued", "exact-target-already-running"]
)
def test_the_exact_validate_lock_target_still_blocks_with_unrelated_handles_present(
    tmp_path: Path, capsys, reason_code: str
) -> None:
    settings = configured(tmp_path)
    paths = write_handles(tmp_path, "validate-legacy.json")
    authorities = Authorities(
        paths, commit_time=int(time.time()) + 2 * DAY, admission_reason=reason_code
    )

    assert authorities.tick(settings) == 0

    assert authorities.launches == []
    output = capsys.readouterr().out
    assert "state=in-progress" in output
    assert "validation slot 3 already holds exact target" in output
