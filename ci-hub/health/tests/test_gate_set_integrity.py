#!/usr/bin/env python3
"""Brackets for the gate-set integrity check.

THE PROPERTY THAT MATTERS IS THE REFUSAL, NOT THE PASS. A checker whose purpose
is detecting inert mechanisms is worthless if it can itself become inert, and
the way it would become inert is by having nothing to expect and reporting that
as success. So the emptied-expectations and unreadable-config cases are tested
first and hardest.

Tests drive `main()` directly with an argv, so a change to the CLI that breaks
invocation shows up here rather than only in the tick.
"""

import importlib.util
import io
import contextlib
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
LIVE_CONFIG = HERE.parent / "tick-hub.yaml"
SPEC = importlib.util.spec_from_file_location("gsi", HERE.parent / "gate_set_integrity.py")
gsi = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gsi)


def run(argv):
    """Return (exit_code, {field: value}) from a direct main() call."""
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        code = gsi.main(argv)
    fields = dict(
        line.split("=", 1) for line in buf.getvalue().splitlines() if "=" in line
    )
    return code, fields


def write_config(tmp_path, names):
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": [{"name": n} for n in names]}))
    return cfg


def test_emptied_expectations_refuse_rather_than_pass(monkeypatch, tmp_path):
    """The trap: a checker that expects nothing passes everything."""
    monkeypatch.setattr(gsi, "REQUIRED", {})
    cfg = write_config(tmp_path, ["anything"])
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 2, "an empty REQUIRED set must REFUSE, never pass"
    assert "EMPTY" in fields["summary"]


def test_unreadable_config_refuses(tmp_path):
    """Could-not-look and looked-and-fine must not share an exit code."""
    code, _ = run(["--config", str(tmp_path / "nope.yaml"), "--no-drift"])
    assert code == 2
    empty = tmp_path / "empty.yaml"
    empty.write_text("reminders: []\n")
    code, fields = run(["--config", str(empty), "--no-drift"])
    assert code == 2, "a config with zero reminders must refuse, not report nothing missing"
    assert "REFUSING" in fields["summary"]


def test_missing_required_gate_alarms(tmp_path):
    cfg = write_config(tmp_path, [n for n in gsi.REQUIRED if n != "btrfs_headroom"])
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert fields["missing"] == "btrfs_headroom"
    # The alarm must say what stops being watched, not just a name.
    assert "nothing is watching" in fields["summary"]


def test_checker_requires_itself(tmp_path):
    """Pruning every reminder except this one must still alarm."""
    cfg = write_config(tmp_path, [n for n in gsi.REQUIRED if n != "gate_set_integrity"])
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "gate_set_integrity" in fields["missing"]


def test_all_present_passes(tmp_path):
    cfg = write_config(tmp_path, list(gsi.REQUIRED))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 0
    assert fields["missing"] == "none"


def test_a_skipped_drift_check_is_not_reported_as_performed(tmp_path):
    """Reporting a skip as a completed check is the defect, one level down."""
    cfg = write_config(tmp_path, list(gsi.REQUIRED))
    _, fields = run(["--config", str(cfg), "--no-drift"])
    assert fields["drift_check"] == "skipped"


def test_unavailable_drift_is_declared(tmp_path, monkeypatch):
    """If git cannot answer, say so rather than contributing a silent nothing."""
    cfg = write_config(tmp_path, list(gsi.REQUIRED))
    monkeypatch.setattr(gsi, "main_reminder_names", lambda p, r: (None, "git exploded"))
    code, fields = run(["--config", str(cfg)])
    assert fields["drift_check"] == "unavailable"
    assert "UNAVAILABLE" in fields["summary"]
    assert code == 0, "an unavailable drift check is a disclosure, not a failure"


def test_drift_alarms_without_any_hardcoded_list(tmp_path, monkeypatch):
    """The case a REQUIRED list cannot catch: a gate added to main later."""
    cfg = write_config(tmp_path, list(gsi.REQUIRED))
    monkeypatch.setattr(
        gsi, "main_reminder_names",
        lambda p, r: (set(gsi.REQUIRED) | {"a_gate_added_after_this_checker"}, ""),
    )
    code, fields = run(["--config", str(cfg)])
    assert code == 1
    assert fields["drift_from_main"] == "a_gate_added_after_this_checker"
    assert fields["missing"] == "none", "this must be caught by drift, not by REQUIRED"


def test_every_required_entry_explains_what_goes_unwatched():
    """A bare name tells a reader nothing about what they are switching off."""
    for name, why in gsi.REQUIRED.items():
        assert why and len(why) > 10, f"{name} has no meaningful explanation"


def test_orphan_reaper_schedule_is_detection_only_and_bounded():
    """The scheduled path must report candidates without gaining kill authority."""
    config = yaml.safe_load(LIVE_CONFIG.read_text())
    reminder = {row["name"]: row for row in config["reminders"]}[
        "orphaned_hermit_reaper"
    ]
    assert reminder["cadence_secs"] == 900
    assert reminder["gate"] == {
        "timeout_secs": 60,
        "cmd": "python3 scripts/orphaned-hermit-reaper.py --report-orphans",
        "when": "failure",
        "capture": True,
    }
    assert "--apply" not in reminder["gate"]["cmd"]
    assert reminder["emit"]["title"].endswith("{summary}")
