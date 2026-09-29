from __future__ import annotations

import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys

import yaml


ROOT = Path(__file__).resolve().parents[3]
HEALTH = ROOT / "ci-hub/health"
sys.path.insert(0, str(HEALTH))
import slot_agent_parity  # noqa: E402


def test_resource_audit_tick_gate_emits_captured_metrics() -> None:
    environment = dict(os.environ)
    # Force one deterministic CRIT without weakening any production threshold.
    environment.update({"ZOMBIE_WARN": "0", "ZOMBIE_CRIT": "0"})
    result = subprocess.run(
        [str(ROOT / "scripts/resource_audit.sh"), "--tick-gate"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 2, result.stderr
    fields = dict(line.split("=", 1) for line in result.stdout.splitlines())
    assert fields["state"] == "critical"
    assert int(fields["critical_findings"]) >= 1
    for name in ("memory_pct", "swap_gb", "my_procs", "zombies"):
        assert name in fields
    assert "{" not in fields["summary"]
    assert "}" not in fields["summary"]


def _neutral_resource_environment() -> dict[str, str]:
    environment = dict(os.environ)
    environment.update(
        {
            "DISK_PATHS": "/",
            # Make the refusal-only controls independent of host pressure. The
            # combined-result control below deliberately restores one CRIT.
            "MEM_WARN": "1000000",
            "MEM_CRIT": "1000001",
            "SWAP_WARN_GB": "1000000",
            "SWAP_CRIT_GB": "1000001",
            "MEM_PSI_WARN": "1000000",
            "MEM_PSI_CRIT": "1000001",
            "CPU_PSI_WARN": "1000000",
            "CPU_PSI_CRIT": "1000001",
            "LOAD_WARN_RATIO": "1000000",
            "LOAD_CRIT_RATIO": "1000001",
            "DISK_WARN": "1000000",
            "DISK_CRIT": "1000001",
            "ZOMBIE_WARN": "1000000",
            "ZOMBIE_CRIT": "1000001",
            "PROC_WARN": "1000000",
            "PROC_CRIT": "1000001",
            "VALIDATE_WARN": "1000000",
            "VALIDATE_CRIT": "1000001",
        }
    )
    return environment


def _resource_audit_with_failed_command(
    tmp_path: Path, command: str, *, force_zombie_critical: bool = False
) -> subprocess.CompletedProcess[str]:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake = fake_bin / command
    fake.write_text("#!/bin/sh\nexit 42\n")
    fake.chmod(0o755)
    environment = _neutral_resource_environment()
    environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
    if force_zombie_critical:
        environment.update({"ZOMBIE_WARN": "0", "ZOMBIE_CRIT": "0"})
    return subprocess.run(
        [str(ROOT / "scripts/resource_audit.sh"), "--tick-gate"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )


def test_resource_gate_refuses_when_df_cannot_observe_existing_path(
    tmp_path: Path,
) -> None:
    result = _resource_audit_with_failed_command(tmp_path, "df")
    fields = dict(line.split("=", 1) for line in result.stdout.splitlines())
    assert result.returncode == 75, result.stderr
    assert fields["state"] == "unknown"
    assert int(fields["collection_errors"]) >= 1
    assert "df failed for configured path /" in fields["summary"]
    assert "all resource thresholds" not in fields["summary"]


def test_resource_gate_refuses_when_ps_cannot_observe_processes(
    tmp_path: Path,
) -> None:
    result = _resource_audit_with_failed_command(tmp_path, "ps")
    fields = dict(line.split("=", 1) for line in result.stdout.splitlines())
    assert result.returncode == 75, result.stderr
    assert fields["state"] == "unknown"
    assert int(fields["collection_errors"]) >= 2
    assert "cannot enumerate user processes" in fields["summary"]
    assert "cannot enumerate zombie processes" in fields["summary"]
    assert "all resource thresholds" not in fields["summary"]


def test_resource_gate_preserves_known_critical_when_df_is_unknown(
    tmp_path: Path,
) -> None:
    result = _resource_audit_with_failed_command(
        tmp_path, "df", force_zombie_critical=True
    )
    fields = dict(line.split("=", 1) for line in result.stdout.splitlines())
    assert result.returncode == 2, result.stderr
    assert fields["state"] == "critical"
    assert int(fields["critical_findings"]) >= 1
    assert int(fields["collection_errors"]) >= 1
    assert "zombies:" in fields["summary"]
    assert "df failed for configured path /" in fields["summary"]


def test_resource_gate_refuses_a_missing_configured_disk_path(
    tmp_path: Path,
) -> None:
    environment = _neutral_resource_environment()
    environment["DISK_PATHS"] = str(tmp_path / "missing")
    result = subprocess.run(
        [str(ROOT / "scripts/resource_audit.sh"), "--tick-gate"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    fields = dict(line.split("=", 1) for line in result.stdout.splitlines())
    assert result.returncode == 75, result.stderr
    assert fields["state"] == "unknown"
    assert "configured disk path does not exist" in fields["summary"]


def test_slot_parity_refusal_cannot_create_recursive_placeholders(
    monkeypatch, tmp_path: Path
) -> None:
    def unavailable(_database=None):
        raise slot_agent_parity.ParityUnavailable(
            "tmux format #{session_name} #{pane_dead} #{pane_current_command}"
        )

    monkeypatch.setattr(slot_agent_parity, "live_taskgraph_db", unavailable)
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        result = slot_agent_parity.main(["--gate", "--root", str(tmp_path)])
    assert result == 2
    report = output.getvalue()
    assert "#{session_name}" not in report
    assert "#(session_name)" in report


def test_herdr_snapshot_producer_runs_independently_inside_freshness_bound() -> None:
    service = (ROOT / "scripts/systemd/hermit-herdr-agent-snapshot.service").read_text()
    timer = (ROOT / "scripts/systemd/hermit-herdr-agent-snapshot.timer").read_text()
    health_service = (ROOT / "scripts/systemd/hermit-health-tick.service").read_text()
    assert "OnUnitInactiveSec=1min" in timer
    assert "AccuracySec=5s" in timer
    assert "TimeoutStartSec=110" in service
    # `captured_at` is the observation START. One full old collection, inactive
    # interval, accuracy, and one full next collection still fit the consumer.
    assert 110 + 60 + 5 + 110 < slot_agent_parity.DEFAULT_MAX_SOURCE_AGE_SECS
    assert "herdr_agent_snapshot.py" in service
    assert "--bindings %h/work/dev-hermit/.tick-hub/herdr-agent-bindings.json" in service
    assert "--output %h/work/dev-hermit/ignored/ci-hub/agent-snapshot.json" in service
    assert "Environment=GOMAXPROCS=4" in service
    assert "Environment=MALLOC_CONF=background_thread:false,narenas:4" in service
    bootstrap = (ROOT / "scripts/hermit-health-tick.sh").read_text()
    assert "-c pack.threads=2" in bootstrap
    assert "-c pack.windowMemory=64m" in bootstrap
    assert "-c pack.deltaCacheSize=64m" in bootstrap
    assert "MemoryHigh=512M" in service
    assert "MemoryMax=768M" in service
    assert "MemorySwapMax=128M" in service
    assert "TasksMax=512" in service
    assert "--herdr-bin %h/bin/herdr" in service
    assert "--agentctl-bin %h/.local/bin/agentctl" in service
    assert "EnvironmentFile=-%h/.config/dev-hermit/health.env" in health_service


def test_herdr_snapshot_units_have_an_explicit_installer() -> None:
    installer = (ROOT / "scripts/install-herdr-agent-snapshot.sh").read_text()
    assert "service=hermit-herdr-agent-snapshot.service" in installer
    assert "timer=hermit-herdr-agent-snapshot.timer" in installer
    assert 'install -m 0644 "$source_root/scripts/systemd/$service"' in installer
    assert 'install -m 0644 "$source_root/scripts/systemd/$timer"' in installer
    assert '"$systemctl_bin" --user enable "$timer"' in installer
    assert '"$systemctl_bin" --user restart "$timer"' in installer
    assert "--gate --check" in installer


def test_installer_still_arms_retry_timer_when_live_check_is_unknown(
    tmp_path: Path,
) -> None:
    state = tmp_path / "state"
    (state / ".tick-hub").mkdir(parents=True)
    (state / ".tick-hub/herdr-agent-bindings.json").write_text("{}")
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    for name in ("herdr", "agentctl"):
        executable = fake_bin / name
        executable.write_text("#!/bin/sh\nexit 0\n")
        executable.chmod(0o755)
    python = fake_bin / "python3"
    python.write_text("#!/bin/sh\nexit 75\n")
    python.chmod(0o755)
    systemctl_log = tmp_path / "systemctl.log"
    systemctl = fake_bin / "systemctl"
    systemctl.write_text(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SYSTEMCTL_LOG\"\nexit 0\n"
    )
    systemctl.chmod(0o755)
    unit_dir = tmp_path / "units"
    environment = dict(os.environ)
    environment.update(
        {
            "PATH": f"{fake_bin}:{environment['PATH']}",
            "DEV_HERMIT_PARENT": str(state),
            "HERDR_BIN": str(fake_bin / "herdr"),
            "AGENTCTL_BIN": str(fake_bin / "agentctl"),
            "HERMIT_USER_UNIT_DIR": str(unit_dir),
            "SYSTEMCTL_BIN": str(systemctl),
            "SYSTEMCTL_LOG": str(systemctl_log),
        }
    )
    result = subprocess.run(
        [str(ROOT / "scripts/install-herdr-agent-snapshot.sh")],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 75
    assert "installing the retry timer anyway" in result.stderr
    assert (unit_dir / "hermit-herdr-agent-snapshot.service").exists()
    assert (unit_dir / "hermit-herdr-agent-snapshot.timer").exists()
    calls = systemctl_log.read_text()
    assert "--user enable hermit-herdr-agent-snapshot.timer" in calls
    assert "--user restart hermit-herdr-agent-snapshot.timer" in calls


def test_slot_parity_reads_the_live_herdr_census() -> None:
    # The snapshot this gate used to read stopped being produced (its producer
    # exits 75 without agentctl); the herdr source reads herdr and /proc live.
    config = yaml.safe_load((ROOT / "ci-hub/health/tick-hub.yaml").read_text())
    reminder = {row["name"]: row for row in config["reminders"]}[
        "slot_agent_parity"
    ]
    assert reminder["gate"]["cmd"].endswith("--agent-source herdr")
    assert reminder["gate"]["when"] == "failure"
    assert reminder["gate"]["capture"] is True
