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
ROOT = HERE.parents[2]
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
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    cfg.write_text(
        yaml.safe_dump({"reminders": [live.get(n, {"name": n}) for n in names]})
    )
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


def test_chat_bridge_resource_gate_cannot_disappear_silently(tmp_path):
    cfg = write_config(
        tmp_path,
        [name for name in gsi.REQUIRED if name != "chat_bridge_resource_health"],
    )
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert fields["missing"] == "chat_bridge_resource_health"
    assert "runaway writes" in fields["summary"]


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


def test_transition_gate_name_cannot_survive_with_an_inert_shape(tmp_path):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    live["stale_tick_processes"]["gate"] = {
        "cmd": "true",
        "when": "failure",
        "capture": True,
    }
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_transition_gate_name_cannot_survive_without_its_runtime_bound(tmp_path):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    live["host_resource_audit"]["gate"].pop("timeout_secs")
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_transition_gate_name_cannot_survive_with_an_inert_cadence(tmp_path):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    live["host_resource_audit"]["cadence_secs"] = 31_536_000
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_transition_gate_name_cannot_survive_without_escalating_emit(tmp_path):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    live["host_resource_audit"]["emit"].pop("skill")
    live["host_resource_audit"]["emit"]["title"] = "resource check ran"
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_required_gate_name_cannot_survive_without_a_command(tmp_path):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    live["btrfs_headroom"].pop("gate")
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "gate definition is missing" in fields["summary"]


def test_chat_bridge_gate_execution_shape_is_pinned_and_detection_only():
    config = yaml.safe_load(LIVE_CONFIG.read_text())
    reminder = {row["name"]: row for row in config["reminders"]}[
        "chat_bridge_resource_health"
    ]
    assert reminder == {
        "name": "chat_bridge_resource_health",
        "cadence_secs": 1800,
        "gate": {
            "timeout_secs": 15,
            "cmd": (
                "python3 ci-hub/health/chat_bridge_health.py "
                '--root "$DEV_HERMIT_PARENT"'
            ),
            "when": "failure",
            "capture": True,
        },
        "emit": {
            "kind": "action",
            "skill": "hard-warn-coordinator",
            "fields": {"component": "chat-bridge-resource-health"},
            "title": "Chat bridge resource health needs attention: {summary}",
        },
    }
    assert gsi.REQUIRED_GATE_SHAPES["chat_bridge_resource_health"] == reminder
    command = f" {reminder['gate']['cmd']} "
    for mutator in (
        "--apply",
        " kill ",
        " reset-",
        " restart ",
        " stop ",
        " disable ",
        " unmount ",
        " delete ",
    ):
        assert mutator not in command


def test_chat_bridge_gate_name_cannot_survive_at_an_inert_cadence(tmp_path):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    live["chat_bridge_resource_health"]["cadence_secs"] = 31_536_000
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    code, fields = run(["--config", str(cfg), "--no-drift"])
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_chat_bridge_gate_rejects_float_substitution_for_integer_cadence(tmp_path):
    code, fields = _mutated_chat_config(
        tmp_path,
        lambda reminder: reminder.update(cadence_secs=1800.0),
    )
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_chat_bridge_gate_rejects_nested_float_timeout_substitution(tmp_path):
    code, fields = _mutated_chat_config(
        tmp_path,
        lambda reminder: reminder["gate"].update(timeout_secs=15.0),
    )
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def _mutated_chat_config(tmp_path, mutator):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    mutator(live["chat_bridge_resource_health"])
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    return run(["--config", str(cfg), "--no-drift"])


def test_chat_bridge_gate_cannot_be_suppressed_by_never_true_flag(tmp_path):
    code, fields = _mutated_chat_config(
        tmp_path,
        lambda reminder: reminder.update(requires_flags=["never_configured"]),
    )
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_chat_bridge_gate_cannot_gain_a_suppressing_dependency(tmp_path):
    code, fields = _mutated_chat_config(
        tmp_path,
        lambda reminder: reminder.update(depends_on=["host_resource_audit"]),
    )
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_chat_bridge_gate_cannot_move_to_a_phase_or_narrow_window(tmp_path):
    def phase(reminder):
        reminder["cadence_offset_secs"] = 1200
        reminder["cadence_window_secs"] = 60

    code, fields = _mutated_chat_config(tmp_path, phase)
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_chat_bridge_gate_rejects_an_unreviewed_future_jitter_key(tmp_path):
    code, fields = _mutated_chat_config(
        tmp_path,
        lambda reminder: reminder.update(cadence_jitter_secs=1700),
    )
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


def test_chat_bridge_gate_cannot_silently_gain_parallel_execution(tmp_path):
    code, fields = _mutated_chat_config(
        tmp_path,
        lambda reminder: reminder["gate"].update(parallel=True),
    )
    assert code == 1
    assert "protected gate execution shape changed" in fields["summary"]


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


def test_transition_resource_schedules_share_the_existing_tick_and_only_report():
    config = yaml.safe_load(LIVE_CONFIG.read_text())
    reminders = {row["name"]: row for row in config["reminders"]}
    expected = {
        "host_resource_audit": {
            "timeout_secs": 30,
            "cmd": "scripts/resource_audit.sh --tick-gate",
            "when": "failure",
            "capture": True,
        },
        "operational_input_freshness": {
            "timeout_secs": 10,
            "cmd": (
                "python3 ci-hub/health/operational_health.py input-freshness "
                '--state-root "$DEV_HERMIT_PARENT"'
            ),
            "when": "failure",
            "capture": True,
        },
        "stale_tick_processes": {
            "timeout_secs": 10,
            "cmd": (
                "python3 ci-hub/health/stale_tick_processes.py --gate "
                '--root "$DEV_HERMIT_PARENT" --max-age-secs 900'
            ),
            "when": "failure",
            "capture": True,
        },
    }
    for name, gate in expected.items():
        reminder = reminders[name]
        assert reminder["cadence_secs"] == 900
        assert reminder["gate"] == gate
        assert reminder["emit"]["kind"] == "action"
        assert reminder["emit"]["title"].endswith("{summary}")
        command = reminder["gate"]["cmd"]
        for mutator in ("--apply", " kill ", " remove ", " delete "):
            assert mutator not in f" {command} "


def test_transition_resource_schedules_remain_eligible_on_every_outer_tick():
    config = yaml.safe_load(LIVE_CONFIG.read_text())
    reminders = {row["name"]: row for row in config["reminders"]}
    names = {
        "host_resource_audit",
        "operational_input_freshness",
        "stale_tick_processes",
        "orphaned_hermit_reaper",
    }
    timer = (ROOT / "scripts/systemd/hermit-health-tick.timer").read_text()
    service = (ROOT / "scripts/systemd/hermit-health-tick.service").read_text()
    assert "OnUnitActiveSec=30min" in timer
    assert "TimeoutStartSec=650" in service
    # Two tick starts are at least 1,800 seconds apart. Even if a gate moves
    # from the very end of one complete service window to the start of the
    # next, its observations remain at least 1,150 seconds apart. A 900-second
    # inner throttle therefore cannot alias away the next scheduled check.
    assert 900 < 30 * 60 - 650
    assert {reminders[name]["cadence_secs"] for name in names} == {900}


# The website pair (https://github.com/rrnewton/dev-hermit/issues/394, round 2).
SITE_GATES = {
    "site_rebuild_health": "compatibility website rebuild failures and a stopped builder",
    "site_data_freshness": "stale published website data and silently excluded rows",
}


def test_site_gates_are_required_and_pinned_to_the_live_rows():
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    for name, why in SITE_GATES.items():
        assert gsi.REQUIRED[name] == why
        assert gsi._same_typed_value(live[name], gsi.REQUIRED_GATE_SHAPES[name]), name
    assert live["site_rebuild_health"]["gate"]["cmd"] == (
        "./ci-hub/health/site_rebuild_health.py --gate --dispatch "
        '--state "$DEV_HERMIT_PARENT/.tick-hub/site-rebuild-health.json"'
    )
    assert live["site_data_freshness"]["gate"]["cmd"] == (
        "./ci-hub/health/site_data_freshness.py --gate "
        '--state "$DEV_HERMIT_PARENT/.tick-hub/site-data-freshness.json"'
    )
    assert {live[name]["cadence_secs"] for name in SITE_GATES} == {0}


def test_removing_either_site_gate_fires_the_integrity_gate(tmp_path):
    for name, why in SITE_GATES.items():
        cfg = write_config(tmp_path, [n for n in gsi.REQUIRED if n != name])
        code, fields = run(["--config", str(cfg), "--no-drift"])
        assert code == 1, name
        assert fields["missing"] == name
        assert f"{name} -- nothing is watching {why}" in fields["summary"]


def _mutated_site_config(tmp_path, name, mutator):
    live = {
        row["name"]: row
        for row in yaml.safe_load(LIVE_CONFIG.read_text())["reminders"]
    }
    mutator(live[name])
    cfg = tmp_path / "tick-hub.yaml"
    cfg.write_text(yaml.safe_dump({"reminders": list(live.values())}))
    return run(["--config", str(cfg), "--no-drift"])


SITE_MUTATIONS = [
    # The round-1 cadence that let a tick skip them.
    ("cadence 1800", lambda r: r.update(cadence_secs=1800)),
    ("float cadence", lambda r: r.update(cadence_secs=0.0)),
    ("offset", lambda r: r.update(cadence_offset_secs=60)),
    ("never-true flag", lambda r: r.update(requires_flags=["never_configured"])),
    ("dependency", lambda r: r.update(depends_on=["gate_set_integrity"])),
    ("no --gate", lambda r: r["gate"].update(cmd=r["gate"]["cmd"].replace(" --gate", ""))),
    ("state elsewhere", lambda r: r["gate"].update(cmd=r["gate"]["cmd"].replace(".tick-hub/", "/tmp/"))),
    ("parallel", lambda r: r["gate"].update(parallel=True)),
    ("timeout", lambda r: r["gate"].update(timeout_secs=5)),
    ("no outcome field", lambda r: r["emit"]["fields"].pop("outcome")),
    ("no skill", lambda r: r["emit"].pop("skill")),
]


def test_a_site_gate_cannot_survive_with_an_inert_shape(tmp_path):
    for name in SITE_GATES:
        for label, mutator in SITE_MUTATIONS:
            code, fields = _mutated_site_config(tmp_path, name, mutator)
            assert code == 1, (name, label)
            assert f"{name} -> protected gate execution shape changed" in fields["summary"], (name, label)
    # when=success is refused one check earlier, under its own message.
    for name in SITE_GATES:
        code, fields = _mutated_site_config(tmp_path, name, lambda r: r["gate"].update(when="success"))
        assert code == 1, name
        assert f"{name} -> gate must use when=failure and capture=true" in fields["summary"]
    # Rebuild-specific: dispatch is what keeps the site building.
    code, fields = _mutated_site_config(
        tmp_path, "site_rebuild_health",
        lambda r: r["gate"].update(cmd=r["gate"]["cmd"].replace(" --dispatch", "")),
    )
    assert code == 1
    assert "site_rebuild_health -> protected gate execution shape changed" in fields["summary"]
    # Freshness-specific: dropping the unaccepted-exclusion count from the page.
    code, fields = _mutated_site_config(
        tmp_path, "site_data_freshness", lambda r: r["emit"]["fields"].pop("excluded_unaccepted"),
    )
    assert code == 1
    assert "site_data_freshness -> protected gate execution shape changed" in fields["summary"]
