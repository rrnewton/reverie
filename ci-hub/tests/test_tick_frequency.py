import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TICK_MINUTES = 30
AGENT_SNAPSHOT_MINUTES = 1
DISK_SNAPSHOT_MINUTES = 15


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def test_every_outer_tick_frequency_source_says_thirty_minutes() -> None:
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


def test_systemd_health_and_agent_snapshot_are_the_sole_operational_drivers() -> None:
    plugin = read(".orc/plugins/hermit-dev/index.ts")
    operational_health = read("ci-hub/health/operational_health.py")
    health_service = read("scripts/systemd/hermit-health-tick.service")
    health_timer = read("scripts/systemd/hermit-health-tick.timer")
    agent_service = read("scripts/systemd/hermit-herdr-agent-snapshot.service")
    agent_timer = read("scripts/systemd/hermit-herdr-agent-snapshot.timer")

    assert f"OnUnitActiveSec={TICK_MINUTES}min" in health_timer
    assert "--tool-relative scripts/hermit-operational-tick" in health_service
    assert f"OnUnitInactiveSec={AGENT_SNAPSHOT_MINUTES}min" in agent_timer
    assert "--tool-relative ci-hub/health/herdr_agent_snapshot.py" in agent_service
    assert "--output %h/work/dev-hermit/ignored/ci-hub/agent-snapshot.json" in agent_service
    assert 'operationalHealthOwner: "hermit-health-tick.timer"' in plugin
    assert 'agentSnapshotOwner: "hermit-herdr-agent-snapshot.timer"' in plugin
    assert "retiredSystemdReplacedWorkflows" in plugin

    match = re.search(
        r"DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS\s*=\s*(\d+)\s*\*\s*60",
        operational_health,
    )
    assert match is not None
    assert AGENT_SNAPSHOT_MINUTES < int(match.group(1))


def test_systemd_replaced_orc_producers_are_retirement_only() -> None:
    plugin = read(".orc/plugins/hermit-dev/index.ts")

    assert "const RETIRED_SYSTEMD_REPLACED_WORKFLOW_NAMES = [" in plugin
    for workflow in (
        "hermit-dev-operational-health-v1",
        "hermit-dev-agent-snapshot-5m-v1",
        "hermit-dev-disk-free-15m-v1",
        "hermit-dev-disk-paths-15m-v1",
    ):
        assert plugin.count(f'"{workflow}"') == 1
    assert "...RETIRED_SYSTEMD_REPLACED_WORKFLOW_NAMES" in plugin
    for forbidden in (
        "OPERATIONAL_TICK_SCRIPT_NAME",
        "AGENT_SNAPSHOT_SCRIPT_NAME",
        "DISK_FREE_SCRIPT_NAME",
        "DISK_PATH_SCRIPT_NAME",
        "hermitOperationalTick",
        "hermitCacheAgentSnapshot",
        "hermitCacheDiskFree",
        "hermitCacheDiskPaths",
        "operationalHealthHeartbeat",
        "agentSnapshotHeartbeat",
        "diskFreeHeartbeat",
        "diskPathHeartbeat",
        "./scripts/hermit-operational-tick",
        "operational_health.py cache-agents",
        "operational_health.py cache-disk",
    ):
        assert forbidden not in plugin


def test_active_runbooks_name_systemd_as_the_sole_driver() -> None:
    config = read("ci-hub/health/tick-hub.yaml")
    containers = read("ci-hub/containers/README.md")
    bridge = read("ci-hub/health/agent-container-reconcile-bridge.rs")
    ci_hub_readme = read("ci-hub/README.md")
    health_readme = read("ci-hub/health/README.md")
    operational_tick = read("scripts/hermit-operational-tick")
    installer = read("scripts/install-disk-snapshot.sh")

    assert "operationalHealthHeartbeat re-reads this file" not in config
    assert "hermit-health-tick.timer re-reads this file" in config
    assert "The outer ORC workflow" not in config
    assert "The outer ORC workflow" not in ci_hub_readme
    assert "shared by the ORC workflow" not in operational_tick
    assert "hermit-health-tick.timer" in ci_hub_readme
    assert "owned by the durable systemd timer" in operational_tick
    assert "The admission test for any reminder is **can ORC" not in health_readme
    assert "Herdr plus the exact\nTaskGraph-bound agent snapshot" in health_readme
    assert "## Retired ORC bridge" in containers
    assert (
        "Do not launch `ci-hub/health/agent-container-reconcile-bridge.rs`"
        in containers
    )
    assert (
        "systemd-run --user --unit=dev-hermit-agent-container-reconcile-bridge"
        not in containers
    )
    assert "This is retired under Herdr and must not be launched" in bridge
    assert "first run\n`scripts/install-disk-snapshot.sh --no-start`" in health_readme
    assert "leaves the disk timer both\ndisabled and inactive" in health_readme
    assert '"$systemctl_bin" --user disable --now "$timer"' in installer


def test_orc_retirement_is_observed_and_fails_closed() -> None:
    plugin = read(".orc/plugins/hermit-dev/index.ts")
    start = plugin.index("async function reconcileWorkflowRetirement")
    end = plugin.index("/** Report whether every heartbeat", start)
    reconciliation = plugin[start:end]

    assert reconciliation.count("await orc.listWorkflows()") == 2
    assert "result.absentBefore" in reconciliation
    assert "result.killErrors.push" in reconciliation
    assert "result.stillPresentSystemdReplaced" in reconciliation
    assert "result.stillPresent.length === 0" in reconciliation
    assert "workflowRetirementAtStartup: lastWorkflowRetirement" in plugin
    assert "retainWorkflowRetirementCheck(lastWorkflowRetirement)" in plugin
    assert "HARD WARNING: competing ORC producer retirement failed" in plugin
    assert 'throw new Error("competing workflow retirement failed: "' in plugin


def test_disk_writer_has_a_durable_fifteen_minute_systemd_schedule() -> None:
    service = read("scripts/systemd/hermit-disk-snapshot.service")
    timer = read("scripts/systemd/hermit-disk-snapshot.timer")

    assert f"OnCalendar=*:0/{DISK_SNAPSHOT_MINUTES}" in timer
    assert "Persistent=true" in timer
    assert "Unit=hermit-disk-snapshot.service" in timer
    assert "operational-tool --run-current-tool" in service
    assert "--tool-relative scripts/publish-disk-snapshots.py" in service
    assert "--population-timeout 180" in service


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
    assert (
        "NO_RESULT WINDOW UNTIL THE NEXT INDEPENDENT AUDIT PUBLISHES EVIDENCE"
        in config
    )


def test_health_tick_uses_the_installed_immutable_bootstrap() -> None:
    service = read("scripts/systemd/hermit-health-tick.service")

    assert "ExecStart=%h/.local/libexec/dev-hermit/operational-tool" in service
    assert "--run-current-tool" in service
    assert "--state-root %h/work/dev-hermit" in service
    assert "--tool-relative scripts/hermit-operational-tick" in service
    assert "ExecStart=%h/work/dev-hermit/scripts/hermit-health-tick.sh" not in service
    assert (
        "Environment=PATH=%h/.local/libexec/dev-hermit:%h/bin:%h/.local/bin:"
        in service
    )
    assert "Environment=FLEET_REAL_GH=%h/.local/libexec/dev-hermit/gh" in service


def test_staleness_alarm_preserves_three_missed_tick_threshold() -> None:
    source = read("scripts/hermit-health-staleness.sh")
    assert f"INTERVAL_MIN={TICK_MINUTES}" in source
    assert "N=3" in source
    assert "THRESH=$(( INTERVAL_MIN * N * 60 ))" in source
