#!/usr/bin/env python3

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
from pathlib import Path
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
    with mock.patch.object(
        cleanup.start_unit, "sweep_completed_checkouts", return_value=report
    ), contextlib.redirect_stdout(output):
        code = cleanup.main(["--state-root", str(ROOT)])
    fields = dict(
        line.split("=", 1) for line in output.getvalue().splitlines() if "=" in line
    )
    return code, fields


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


def test_non_running_retention_fails_and_names_the_path_and_reason() -> None:
    code, fields = run_with(
        {
            "removed": ["/old"],
            "removed_cargo_homes": [],
            "retained": [{"checkout": "/held", "reason": "live process"}],
            "deferred": [],
            "bookkeeping_errors": [],
        }
    )
    assert code == 1
    assert fields["state"] == "failed"
    assert "/held: live process" in fields["summary"]
    assert fields["cleanup_failures"] == "1"


def test_bookkeeping_failure_is_not_reported_as_clean() -> None:
    code, fields = run_with(
        {
            "removed": ["/old"],
            "removed_cargo_homes": [],
            "retained": [],
            "deferred": [],
            "bookkeeping_errors": [
                {"record": "/record.json", "reason": "old schema"}
            ],
        }
    )
    assert code == 1
    assert fields["state"] == "failed"
    assert "/record.json: old schema" in fields["summary"]


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


def test_json_reports_the_same_counts() -> None:
    report = {
        "removed": ["/old"],
        "removed_cargo_homes": ["/cargo"],
        "retained": [],
        "deferred": [{"checkout": "/later", "reason": "bounded batch"}],
        "bookkeeping_errors": [],
    }
    output = io.StringIO()
    with mock.patch.object(
        cleanup.start_unit, "sweep_completed_checkouts", return_value=report
    ), contextlib.redirect_stdout(output):
        code = cleanup.main(["--state-root", str(ROOT), "--json"])
    assert code == 0
    rendered = json.loads(output.getvalue())
    assert rendered["removed"] == ["/old"]
    assert rendered["removed_cargo_homes"] == ["/cargo"]
    assert rendered["deferred"] == [
        {"checkout": "/later", "reason": "bounded batch"}
    ]
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
    assert fields["deferred_checkouts"] == "1"
    assert "deferred 1 checkout(s)" in fields["summary"]
