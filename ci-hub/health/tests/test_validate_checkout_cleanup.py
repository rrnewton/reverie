#!/usr/bin/env python3

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
from unittest import mock

import yaml


ROOT = Path(__file__).resolve().parents[3]
MODULE = ROOT / "ci-hub" / "health" / "validate_checkout_cleanup.py"
SPEC = importlib.util.spec_from_file_location("validate_checkout_cleanup", MODULE)
assert SPEC and SPEC.loader
cleanup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cleanup)


def run_with(report: dict) -> tuple[int, dict[str, str]]:
    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit, "sweep_completed_checkouts", return_value=report
        ),
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.main(["--state-root", str(ROOT), "--sweep"])
    fields = dict(
        line.split("=", 1) for line in output.getvalue().splitlines() if "=" in line
    )
    return code, fields


def retirement_report(
    *,
    removed: list[dict] | None = None,
    retained: list[dict] | None = None,
    deferred: list[dict] | None = None,
    could_not_determine: list[dict] | None = None,
) -> dict:
    indeterminate = could_not_determine or []
    return {
        "schema": cleanup.RETIREMENT_RESULT_SCHEMA,
        "limit": cleanup.RETIREMENT_BATCH_LIMIT,
        "removed": removed or [],
        "retained": retained or [],
        "deferred": deferred or [],
        "could_not_determine": indeterminate,
        "recovery_required": bool(indeterminate),
        "state": "could-not-determine" if indeterminate else "ok",
        "summary": "fixture retirement result",
        "provider_returncode": 3 if indeterminate else 0,
    }


def provider_report(**outcomes: list[dict]) -> dict:
    result = retirement_report(**outcomes)
    result.pop("state")
    result.pop("summary")
    result.pop("provider_returncode")
    return result


def test_running_checkout_is_expected_and_does_not_fail_the_recurring_cleanup() -> None:
    code, fields = run_with(
        {
            "removed": [],
            "removed_cargo_homes": [],
            "retained": [{"checkout": "/run", "reason": "unit-running"}],
            "deferred": [],
            "bookkeeping_errors": [],
        }
    )
    assert code == 0
    assert fields["state"] == "ok"
    assert "retained 1 running checkout(s)" in fields["summary"]


def test_correct_permanent_refusal_is_visible_without_failing_cleanup() -> None:
    retained = [
        {
            "checkout": "/held-primary",
            "reason": "this refusal is correct and permanent -- do not remove it",
        }
    ]
    retained.extend(
        {
            "checkout": f"/held-{index}",
            "reason": "cleanup correctly refused this path",
        }
        for index in range(6)
    )
    code, fields = run_with(
        {
            "removed": [],
            "removed_cargo_homes": ["/cargo-one", "/cargo-two"],
            "retained": retained,
            "deferred": [],
            "bookkeeping_errors": [],
        }
    )
    assert code == 0
    assert fields["state"] == "ok"
    assert fields["removed_cargo_homes"] == "2"
    assert (
        "/held-primary: this refusal is correct and permanent -- do not remove it"
        in fields["summary"]
    )
    assert fields["cleanup_refusals"] == "7"
    assert fields["cleanup_failures"] == "0"


def test_bookkeeping_failure_is_not_reported_as_clean() -> None:
    code, fields = run_with(
        {
            "removed": ["/old"],
            "removed_cargo_homes": [],
            "retained": [],
            "deferred": [],
            "bookkeeping_errors": [{"record": "/record.json", "reason": "old schema"}],
        }
    )
    assert code == 1
    assert fields["state"] == "failed"
    assert "/record.json: old schema" in fields["summary"]
    assert fields["cleanup_refusals"] == "0"
    assert fields["cleanup_failures"] == "1"


def test_cleanup_attempt_error_is_not_reported_as_a_refusal() -> None:
    code, fields = run_with(
        {
            "removed": [],
            "removed_cargo_homes": [],
            "retained": [
                {
                    "checkout": "/candidate",
                    "cleanup_failed": True,
                    "reason": "cleanup provider returned no result",
                }
            ],
            "deferred": [],
            "bookkeeping_errors": [],
        }
    )
    assert code == 1
    assert fields["state"] == "failed"
    assert fields["cleanup_refusals"] == "0"
    assert fields["cleanup_failures"] == "1"
    assert "/candidate: cleanup provider returned no result" in fields["summary"]


def test_sweep_error_remains_nonzero_and_could_not_determine() -> None:
    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit,
            "sweep_completed_checkouts",
            side_effect=RuntimeError("process census unavailable"),
        ),
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.main(["--state-root", str(ROOT), "--sweep", "--json"])

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    rendered = json.loads(output.getvalue())
    assert rendered["state"] == "could-not-determine"
    assert "process census unavailable" in rendered["summary"]


def test_recurring_cleanup_is_wired_into_tick_hub() -> None:
    config = yaml.safe_load((ROOT / "ci-hub/health/tick-hub.yaml").read_text())
    reminder = next(
        row for row in config["reminders"] if row["name"] == "validate_checkout_cleanup"
    )
    assert reminder["cadence_secs"] == 3600
    assert reminder["gate"] == {
        "timeout_secs": 30,
        "cmd": (
            "python3 ci-hub/health/validate_checkout_cleanup.py "
            "--state-root /home/newton/work/dev-hermit"
        ),
        "when": "failure",
        "capture": True,
    }
    assert reminder["emit"]["title"].endswith("{summary}")


def test_worker_repeats_bounded_sweeps_without_a_larger_gate_timeout() -> None:
    worker = (ROOT / "scripts/validate-checkout-cleanup-worker").read_text()
    assert "--drain --json" in worker
    assert 'exec "$ROOT/scripts/hermit-box-run"' not in worker


def test_json_reports_the_same_counts() -> None:
    report = {
        "removed": ["/old"],
        "removed_cargo_homes": ["/cargo"],
        "retained": [],
        "deferred": [{"checkout": "/later", "reason": "bounded batch"}],
        "bookkeeping_errors": [],
    }
    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit, "sweep_completed_checkouts", return_value=report
        ),
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.main(["--state-root", str(ROOT), "--sweep", "--json"])
    assert code == 0
    rendered = json.loads(output.getvalue())
    assert rendered["removed"] == ["/old"]
    assert rendered["removed_cargo_homes"] == ["/cargo"]
    assert rendered["deferred"] == [{"checkout": "/later", "reason": "bounded batch"}]
    assert rendered["cleanup_failures"] == 0


def test_deferred_checkout_is_neutral_and_visible() -> None:
    code, fields = run_with(
        {
            "removed": [],
            "removed_cargo_homes": [],
            "retained": [],
            "deferred": [{"checkout": "/later", "reason": "bounded batch"}],
            "bookkeeping_errors": [],
        }
    )
    assert code == 0
    assert fields["state"] == "ok"
    assert fields["removed_checkouts"] == "0"
    assert fields["removed_cargo_homes"] == "0"
    assert fields["deferred_checkouts"] == "1"
    assert "deferred 1 checkout(s)" in fields["summary"]


def test_drain_repeats_bounded_sweeps_until_nothing_is_deferred(
    tmp_path: Path,
) -> None:
    reports = [
        {
            "removed": ["/checkout"],
            "removed_cargo_homes": [],
            "retained": [],
            "deferred": [{"checkout": "/cargo", "reason": "bounded batch"}],
            "bookkeeping_errors": [],
        },
        {
            "removed": [],
            "removed_cargo_homes": ["/cargo"],
            "retained": [],
            "deferred": [],
            "bookkeeping_errors": [],
        },
    ]
    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit, "sweep_completed_checkouts", side_effect=reports
        ) as sweep,
        mock.patch.object(
            cleanup, "run_retirement_batch", return_value=(0, retirement_report())
        ) as retire,
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.main(["--state-root", str(tmp_path), "--drain", "--json"])

    assert code == 0
    assert sweep.call_count == 2
    retire.assert_called_once_with(tmp_path, run=subprocess.run)
    rendered = json.loads(output.getvalue())
    assert rendered["status"] == "completed"
    assert rendered["removed"] == ["/checkout"]
    assert rendered["removed_cargo_homes"] == ["/cargo"]
    latest = json.loads((tmp_path / cleanup.STATE_RELATIVE / "latest.json").read_text())
    assert latest == rendered


def test_drain_still_fails_when_authorized_cleanup_cannot_finish(
    tmp_path: Path,
) -> None:
    report = {
        "removed": ["/checkout"],
        "removed_cargo_homes": [],
        "retained": [
            {
                "checkout": "/protected",
                "reason": "refused because the checkout holds authored work",
            }
        ],
        "deferred": [],
        "bookkeeping_errors": [
            {"record": "/record.json", "reason": "could not record removal"}
        ],
    }
    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit, "sweep_completed_checkouts", return_value=report
        ),
        mock.patch.object(
            cleanup, "run_retirement_batch", return_value=(0, retirement_report())
        ) as retire,
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.main(["--state-root", str(tmp_path), "--drain", "--json"])

    assert code == 1
    retire.assert_called_once_with(tmp_path, run=subprocess.run)
    rendered = json.loads(output.getvalue())
    assert rendered["status"] == "failed"
    assert rendered["last"]["cleanup_refusals"] == 1
    assert rendered["last"]["cleanup_failures"] == 1


def test_drain_preserves_validation_indeterminate_after_retirement(
    tmp_path: Path,
) -> None:
    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit,
            "sweep_completed_checkouts",
            side_effect=RuntimeError("process census unavailable"),
        ) as sweep,
        mock.patch.object(
            cleanup, "run_retirement_batch", return_value=(0, retirement_report())
        ) as retire,
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.main(["--state-root", str(tmp_path), "--drain", "--json"])

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    sweep.assert_called_once()
    retire.assert_called_once_with(tmp_path, run=subprocess.run)
    rendered = json.loads(output.getvalue())
    assert rendered["status"] == "failed"
    assert rendered["state"] == "could-not-determine"
    assert rendered["validation_state"] == "could-not-determine"
    latest, progress, _log = cleanup.state_paths(tmp_path)
    assert json.loads(latest.read_text()) == rendered
    assert json.loads(progress.read_text()) == rendered


def test_max_sweep_path_preserves_validation_indeterminate_and_retires_once(
    tmp_path: Path,
) -> None:
    sweep_result = {
        "state": "could-not-determine",
        "summary": "validation ownership could not be determined",
        "deferred": [{"checkout": "/later", "reason": "retry"}],
    }
    output = io.StringIO()
    with (
        mock.patch.object(cleanup, "MAX_SWEEPS_PER_WORKER", 1),
        mock.patch.object(
            cleanup,
            "run_one_sweep",
            return_value=(cleanup.start_unit.EXIT_COULD_NOT_DETERMINE, sweep_result),
        ) as sweep,
        mock.patch.object(
            cleanup, "run_retirement_batch", return_value=(0, retirement_report())
        ) as retire,
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.drain(tmp_path, as_json=True)

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    sweep.assert_called_once_with(tmp_path)
    retire.assert_called_once_with(tmp_path, run=subprocess.run)
    rendered = json.loads(output.getvalue())
    assert rendered["status"] == "failed"
    assert rendered["state"] == "could-not-determine"
    assert rendered["validation_state"] == "could-not-determine"
    assert rendered["sweeps_completed"] == 1


def test_drain_runs_one_serial_retirement_batch_and_records_outcomes(
    tmp_path: Path,
) -> None:
    timeline: list[str] = []
    reports = [
        {
            "removed": ["/checkout"],
            "removed_cargo_homes": [],
            "retained": [],
            "deferred": [{"checkout": "/later", "reason": "bounded batch"}],
            "bookkeeping_errors": [],
        },
        {
            "removed": [],
            "removed_cargo_homes": [],
            "retained": [],
            "deferred": [],
            "bookkeeping_errors": [],
        },
    ]

    def sweep(_root: Path, **_kwargs: object) -> dict:
        timeline.append("validation")
        return reports.pop(0)

    def row(slot: str, outcome: str, reason: str) -> dict:
        return {
            "machine": "testhost",
            "slot": slot,
            "generation": 1,
            "sha256": (slot[-1] * 64),
            "outcome": outcome,
            "reason": reason,
        }

    provider = retirement_report(
        removed=[row("slot-a", "removed", "ordinary remove completed")],
        retained=[row("slot-b", "retained", "authored work")],
        deferred=[row("slot-c", "deferred", "batch limit")],
    )
    provider.pop("state")
    provider.pop("summary")
    provider.pop("provider_returncode")
    commands: list[list[str]] = []

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        timeline.append("retirement")
        commands.append(command)
        assert kwargs == {"capture_output": True, "text": True, "check": False}
        return subprocess.CompletedProcess(command, 0, json.dumps(provider), "")

    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit, "sweep_completed_checkouts", side_effect=sweep
        ),
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.drain(tmp_path, as_json=True, run=run)

    assert code == 0
    assert timeline == ["validation", "validation", "retirement"]
    assert len(commands) == 1
    command = commands[0]
    assert command == [
        str(ROOT / "ci-hub" / "bin" / "wrkslots"),
        "--project-root",
        str(tmp_path.resolve()),
        "--allow-existing-unregistered-worktrees",
        "--wait-lock",
        "10",
        "retire-pending",
        "--limit",
        "8",
        "--coordinator-pid",
        str(cleanup.os.getpid()),
        "--coordinator-authorized",
        "--format",
        "json",
    ]
    rendered = json.loads(output.getvalue())
    assert rendered["validation_state"] == "ok"
    assert rendered["retirement_removed"] == 1
    assert rendered["retirement_retained"] == 1
    assert rendered["retirement_deferred"] == 1
    assert rendered["retirement_could_not_determine"] == 0
    assert rendered["retirement"]["removed"][0]["slot"] == "slot-a"
    assert rendered["retirement"]["retained"][0]["slot"] == "slot-b"
    assert rendered["retirement"]["deferred"][0]["slot"] == "slot-c"
    latest, progress, _log = cleanup.state_paths(tmp_path)
    assert json.loads(latest.read_text()) == rendered
    assert json.loads(progress.read_text()) == rendered


def test_retirement_could_not_determine_fails_with_structured_outcome(
    tmp_path: Path,
) -> None:
    report = {
        "removed": [],
        "removed_cargo_homes": [],
        "retained": [],
        "deferred": [],
        "bookkeeping_errors": [],
    }
    provider = retirement_report(
        could_not_determine=[
            {
                "machine": "testhost",
                "slot": "slot-d",
                "generation": 2,
                "sha256": "d" * 64,
                "outcome": "could-not-determine",
                "reason": "partial state update",
                "recovery_required": True,
            }
        ]
    )
    provider.pop("state")
    provider.pop("summary")
    provider.pop("provider_returncode")

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, 3, json.dumps(provider), "")

    output = io.StringIO()
    with (
        mock.patch.object(
            cleanup.start_unit, "sweep_completed_checkouts", return_value=report
        ),
        contextlib.redirect_stdout(output),
    ):
        code = cleanup.drain(tmp_path, as_json=True, run=run)

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    rendered = json.loads(output.getvalue())
    assert rendered["status"] == "failed"
    assert rendered["state"] == "could-not-determine"
    assert rendered["validation_state"] == "ok"
    assert rendered["retirement_could_not_determine"] == 1
    assert rendered["retirement"]["recovery_required"] is True
    assert rendered["retirement"]["could_not_determine"][0]["slot"] == "slot-d"
    latest, progress, _log = cleanup.state_paths(tmp_path)
    assert json.loads(latest.read_text()) == rendered
    assert json.loads(progress.read_text()) == rendered


def test_malformed_retirement_result_is_could_not_determine(tmp_path: Path) -> None:
    malformed = retirement_report(
        removed=[
            {
                "machine": "testhost",
                "slot": "slot-e",
                "generation": 3,
                "sha256": "e" * 64,
                "outcome": "retained",
                "reason": "wrong category",
            }
        ]
    )
    malformed.pop("state")
    malformed.pop("summary")
    malformed.pop("provider_returncode")

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, 0, json.dumps(malformed), "")

    code, rendered = cleanup.run_retirement_batch(tmp_path, run=run)

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    assert rendered["state"] == "could-not-determine"
    assert rendered["recovery_required"] is True
    assert rendered["provider_returncode"] == 0
    assert "wrong outcome" in rendered["could_not_determine"][0]["reason"]


def test_retirement_rejects_unknown_top_level_field(tmp_path: Path) -> None:
    provider = provider_report()
    provider["state"] = "ok"

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, 0, json.dumps(provider), "")

    code, rendered = cleanup.run_retirement_batch(tmp_path, run=run)

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    assert rendered["state"] == "could-not-determine"
    assert "unknown=['state']" in rendered["could_not_determine"][0]["reason"]


def test_retirement_rejects_recovery_marker_on_non_indeterminate_row(
    tmp_path: Path,
) -> None:
    provider = provider_report(
        removed=[
            {
                "machine": "testhost",
                "slot": "slot-a",
                "generation": 1,
                "sha256": "a" * 64,
                "outcome": "removed",
                "reason": "ordinary remove completed",
                "recovery_required": False,
            }
        ]
    )

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(command, 0, json.dumps(provider), "")

    code, rendered = cleanup.run_retirement_batch(tmp_path, run=run)

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    assert rendered["state"] == "could-not-determine"
    assert "recovery_required" in rendered["could_not_determine"][0]["reason"]


def test_retirement_attempt_limit_excludes_deferred_outcomes(tmp_path: Path) -> None:
    def row(index: int, outcome: str) -> dict:
        return {
            "machine": "testhost",
            "slot": f"slot-{index}",
            "generation": 1,
            "sha256": f"{index:064x}",
            "outcome": outcome,
            "reason": "fixture",
        }

    too_many_attempted = provider_report(
        removed=[
            row(index, "removed") for index in range(cleanup.RETIREMENT_BATCH_LIMIT + 1)
        ]
    )

    def attempted_run(
        command: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(
            command, 0, json.dumps(too_many_attempted), ""
        )

    code, rendered = cleanup.run_retirement_batch(tmp_path, run=attempted_run)
    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    assert "more attempted outcomes" in rendered["could_not_determine"][0]["reason"]

    deferred = [
        row(index, "deferred") for index in range(cleanup.RETIREMENT_BATCH_LIMIT + 1)
    ]
    deferred[0].update(
        {
            "event_sequence": 1,
            "enqueued_at": "2026-09-18T00:00:00+00:00",
            "last_attempt_sequence": None,
            "artifact_path": None,
            "artifact_source": None,
            "blocked_reason": None,
        }
    )
    deferred_provider = provider_report(deferred=deferred)

    def deferred_run(
        command: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(
            command, 0, json.dumps(deferred_provider), ""
        )

    code, rendered = cleanup.run_retirement_batch(tmp_path, run=deferred_run)
    assert code == 0
    assert rendered["state"] == "ok"
    assert len(rendered["deferred"]) == cleanup.RETIREMENT_BATCH_LIMIT + 1


def test_controller_launches_immutable_bounded_worker_and_reports_no_prior_result(
    tmp_path: Path, monkeypatch: object
) -> None:
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o755)
    monkeypatch.setenv("DEV_HERMIT_OPERATIONAL_TOOL", str(operational_tool))  # type: ignore[attr-defined]
    monkeypatch.setenv("DEV_HERMIT_TOOL_PARENT_SHA", "a" * 40)  # type: ignore[attr-defined]
    commands: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(command)
        if command[0] == "systemctl":
            return subprocess.CompletedProcess(
                command,
                0,
                "LoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\n",
                "",
            )
        return subprocess.CompletedProcess(command, 0, "", "")

    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        code = cleanup.controller(tmp_path, as_json=True, run=run)

    assert code == cleanup.start_unit.EXIT_COULD_NOT_DETERMINE
    rendered = json.loads(output.getvalue())
    assert rendered["state"] == "could-not-determine"
    launch = next(command for command in commands if command[0] == "systemd-run")
    assert f"--unit={cleanup.WORKER_UNIT}" in launch
    separator = launch.index("--")
    assert launch[separator + 1] == str(operational_tool)
    assert launch[launch.index("--tool-relative") + 1] == (
        "scripts/validate-checkout-cleanup-worker"
    )


def test_controller_reports_prior_completion_while_starting_next_worker(
    tmp_path: Path, monkeypatch: object
) -> None:
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o755)
    monkeypatch.setenv("DEV_HERMIT_OPERATIONAL_TOOL", str(operational_tool))  # type: ignore[attr-defined]
    monkeypatch.setenv("DEV_HERMIT_TOOL_PARENT_SHA", "b" * 40)  # type: ignore[attr-defined]
    latest, _progress, _log = cleanup.state_paths(tmp_path)
    cleanup.atomic_write_json(
        latest,
        {
            "schema": cleanup.RESULT_SCHEMA,
            "status": "completed",
            "summary": "removed one checkout",
        },
    )

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[0] == "systemctl":
            return subprocess.CompletedProcess(
                command,
                0,
                "LoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\n",
                "",
            )
        return subprocess.CompletedProcess(command, 0, "", "")

    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        code = cleanup.controller(tmp_path, as_json=True, run=run)

    assert code == 0
    rendered = json.loads(output.getvalue())
    assert rendered["state"] == "ok"
    assert "removed one checkout" in rendered["summary"]
