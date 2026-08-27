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


def test_staleness_alarm_preserves_three_missed_tick_threshold() -> None:
    source = read("scripts/hermit-health-staleness.sh")
    assert f"INTERVAL_MIN={TICK_MINUTES}" in source
    assert "N=3" in source
    assert "THRESH=$(( INTERVAL_MIN * N * 60 ))" in source
