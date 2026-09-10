from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_fetch_target",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

STALE_SHA = "1" * 40
CURRENT_SHA = "2" * 40


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


def newer_shared_ref_result():
    return driver.fetch_ref.FetchResult(
        remote="origin",
        branch="main",
        tracking_ref="refs/remotes/origin/main",
        previous_sha=STALE_SHA,
        fetched_sha=STALE_SHA,
        tracking_sha=CURRENT_SHA,
        cas_retries=1,
        disposition="newer-shared-ref-kept",
    )


def test_newer_reconciled_tip_is_launched_instead_of_consuming_cadence_on_stale_record(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    monkeypatch.setattr(
        driver.fetch_ref,
        "fetch_remote_tracking_branch",
        lambda *_args, **_kwargs: newer_shared_ref_result(),
    )
    status_targets: list[str] = []
    launch_targets: list[str] = []

    def run(command, _cwd: Path, _timeout: float):
        if "validate-status" in command:
            target = command[command.index("--sha") + 1]
            status_targets.append(target)
            # The stale ancestor already has a green record. Selecting it would
            # return success without launching and consume this scheduled tick.
            has_record = target == STALE_SHA
            records = (
                [
                    {
                        "verdict": "VALIDATED",
                        "result": "pass",
                        "exit_code": 0,
                        "started_at": "2026-09-06T01:00:00Z",
                        "finished_at": "2026-09-06T01:10:00Z",
                        "detail": "all required validation gates passed",
                    }
                ]
                if has_record
                else []
            )
            report = {
                "schema_version": driver.EXACT_VALIDATE_STATUS_SCHEMA_VERSION,
                "repo": driver.REPO,
                "sha": target,
                "verdict": "VALIDATED" if has_record else "NOT-VALIDATED",
                "exit_code": 0 if has_record else 4,
                "record_count": len(records),
                "exact_records": records,
                "qualifying_count": len(records),
                "disqualified_count": 0,
                "ledger_freshness": (
                    None
                    if has_record
                    else {"state": "current", "local_absence_is_authoritative": True}
                ),
                "in_progress_count": 0,
                "in_progress": [],
                "run_handles": [],
                "unreadable_handles": [],
            }
            return subprocess.CompletedProcess(
                command, int(report["exit_code"]), json.dumps(report), ""
            )
        if "admission-status" in command:
            target = command[command.index("--target") + 1]
            report = {
                "schema_version": 1,
                "target": target,
                "kind": "validate",
                "admissible": True,
                "state": "admissible",
                "reason_code": "immediate-admission-available",
                "detail": "validation slot 0 allows an immediate attempt",
            }
            return subprocess.CompletedProcess(command, 0, json.dumps(report), "")
        raise AssertionError(command)

    def spawn(_settings, target, *_args):
        launch_targets.append(target)
        return {"pid": 123, "start_ticks": 456, "boot_id": "boot-fixture"}

    settings = configured(tmp_path)
    assert (
        driver.run_tick(
            settings,
            fetch=driver.fetch_tip,
            run=run,
            spawn=spawn,
        )
        == 0
    )
    assert status_targets == [CURRENT_SHA]
    assert launch_targets == [CURRENT_SHA]
    assert json.loads((settings.state_dir / "latest.json").read_text())["target"] == CURRENT_SHA
    output = capsys.readouterr().out
    assert f"target={CURRENT_SHA}" in output
    assert "state=launched" in output


@pytest.mark.parametrize(
    "reason",
    [
        "REFUSED: refs/remotes/origin/main is unresolved",
        "REFUSED: refs/remotes/origin/main moved sideways while fetching",
    ],
)
def test_unresolved_or_divergent_reconciliation_stays_fail_closed(
    tmp_path: Path, monkeypatch, reason: str
) -> None:
    def refuse(*_args, **_kwargs):
        raise driver.fetch_ref.FetchRefError(reason)

    monkeypatch.setattr(driver.fetch_ref, "fetch_remote_tracking_branch", refuse)
    settings = configured(tmp_path)
    with pytest.raises(driver.ObservationUnavailable, match="cannot fetch the exact Hermit main tip"):
        driver.fetch_tip(settings, 1.0)
    assert not settings.state_dir.exists()
