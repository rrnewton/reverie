import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TICK_MINUTES = 60


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def test_every_outer_tick_frequency_source_says_sixty_minutes() -> None:
    assert (
        f"const OPERATIONAL_TICK_INTERVAL_MS = {TICK_MINUTES} * 60 * 1000;"
        in read(".orc/plugins/hermit-dev/index.ts")
    )
    assert (
        f"--current-tick-min {TICK_MINUTES} \\"
        in read("ci-hub/bin/health-tick")
    )
    assert (
        f'.args(["--current-tick-min", "{TICK_MINUTES}"])'
        in read("ci-hub/ci-hub.rs")
    )
    assert (
        f"tick_frequency_min: {TICK_MINUTES}"
        in read("ops/ops-state.example.yaml")
    )
    assert (
        f"OnUnitActiveSec={TICK_MINUTES}min"
        in read("scripts/systemd/hermit-health-tick.timer")
    )


def test_no_tick_hub_reminder_claims_a_faster_cadence_than_the_outer_tick() -> None:
    config = read("ci-hub/health/tick-hub.yaml")
    cadences = [
        int(match.group(1))
        for match in re.finditer(r"^    cadence_secs: (\d+)$", config, re.MULTILINE)
    ]
    assert cadences
    assert min(cadences) >= TICK_MINUTES * 60


def test_expensive_wrkslots_audit_has_a_persistent_calendar_schedule() -> None:
    timer = read("scripts/systemd/hermit-wrkslots-audit.timer")
    service = read("scripts/systemd/hermit-wrkslots-audit.service")
    config = read("ci-hub/health/tick-hub.yaml")

    assert "OnCalendar=hourly" in timer
    assert "OnActiveSec=30s" in timer
    assert "Persistent=true" in timer
    assert "OnUnitActiveSec" not in timer
    assert "Unit=hermit-wrkslots-audit.service" in timer
    assert "operational-tool --run-current-tool" in service
    assert "--tool-relative ci-hub/bin/wrkslots-audit" in service
    assert "KillMode=control-group" in service
    assert "Nice=19" in service
    assert "IOSchedulingClass=idle" in service
    assert "ci-hub/health/wrkslots_audit_result.py" in config
    assert 'cmd: ci-hub/bin/wrkslots --project-root' not in config
    assert "advance intentionally creates that visible NO_RESULT window" in config


def test_staleness_alarm_preserves_three_missed_tick_threshold() -> None:
    source = read("scripts/hermit-health-staleness.sh")
    assert f"INTERVAL_MIN={TICK_MINUTES}" in source
    assert "N=3" in source
    assert "THRESH=$(( INTERVAL_MIN * N * 60 ))" in source
