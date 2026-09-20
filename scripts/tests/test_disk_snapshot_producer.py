#!/usr/bin/env python3
"""Contracts for the durable serialized disk-snapshot producer."""

from __future__ import annotations

import fcntl
import os
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
PRODUCER = ROOT / "scripts/publish-disk-snapshots.sh"
INSTALLER = ROOT / "scripts/install-disk-snapshot.sh"
SERVICE = ROOT / "scripts/systemd/hermit-disk-snapshot.service"
TIMER = ROOT / "scripts/systemd/hermit-disk-snapshot.timer"


def _fake_python(tmp_path: Path) -> tuple[Path, Path]:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    log = tmp_path / "python.log"
    python = fake_bin / "python3"
    python.write_text(
        "#!/usr/bin/env bash\n"
        "set -eu\n"
        "printf '%s\\n' \"$*\" >> \"$DISK_TEST_PYTHON_LOG\"\n"
        "case \" ${DISK_TEST_FAIL_COMMAND:-} \" in\n"
        "  ' cache-disk-paths ') case \" $* \" in *' cache-disk-paths '*) exit 42;; esac;;\n"
        "esac\n"
    )
    python.chmod(0o755)
    return fake_bin, log


def _run_producer(
    tmp_path: Path, state_root: Path, *arguments: str, fail: str = ""
) -> subprocess.CompletedProcess[str]:
    fake_bin, log = _fake_python(tmp_path)
    environment = dict(os.environ)
    environment.update(
        {
            "PATH": f"{fake_bin}:{environment['PATH']}",
            "DISK_TEST_PYTHON_LOG": str(log),
            "DISK_TEST_FAIL_COMMAND": fail,
        }
    )
    return subprocess.run(
        [str(PRODUCER), "--state-root", str(state_root), *arguments],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )


def test_unit_uses_one_exact_bounded_serial_producer() -> None:
    service = SERVICE.read_text()
    timer = TIMER.read_text()
    exact_exec = (
        "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
        "--run-current-tool --state-root %h/work/dev-hermit "
        "--tool-relative scripts/publish-disk-snapshots.sh -- "
        "--state-root %h/work/dev-hermit --population-timeout 180"
    )
    assert service.count("ExecStart=") == 1
    assert exact_exec in service
    assert "TimeoutStartSec=420" in service
    assert "TimeoutStopSec=5s" in service
    assert "KillMode=control-group" in service
    assert "MemoryHigh=512M" in service
    assert "MemoryMax=768M" in service
    assert "MemorySwapMax=128M" in service
    assert "TasksMax=512" in service
    assert "Nice=19" in service
    assert "IOSchedulingClass=idle" in service
    assert "OnCalendar=*:0/15" in timer
    assert "Persistent=true" in timer
    assert "AccuracySec=1min" in timer
    assert 110 + 30 + 180 + 30 + 30 < 420
    assert 420 < 15 * 60
    assert 15 * 60 + 60 + 420 < 45 * 60


def test_producer_runs_exact_commands_serially(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    result = _run_producer(tmp_path, state_root, "--population-timeout", "180")
    assert result.returncode == 0, result.stderr
    health = ROOT / "ci-hub/health/operational_health.py"
    state = state_root / "ignored/ops-state"
    assert (tmp_path / "python.log").read_text().splitlines() == [
        (
            f"{health} cache-disk-paths --state-root {state_root} "
            f"--state-file {state}/disk-paths.json "
            f"--previous-state-file {state}/disk-paths.previous.json "
            "--population-timeout 180"
        ),
        (
            f"{health} cache-disk --filesystem-path {state_root} "
            f"--state-file {state}/disk.json"
        ),
    ]


def test_first_failure_is_closed_and_skips_second_publication(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    result = _run_producer(
        tmp_path,
        state_root,
        "--population-timeout",
        "180",
        fail="cache-disk-paths",
    )
    assert result.returncode == 42
    calls = (tmp_path / "python.log").read_text().splitlines()
    assert len(calls) == 1
    assert " cache-disk-paths " in f" {calls[0]} "


def test_lock_contention_refuses_without_publishing(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state = state_root / "ignored/ops-state"
    state.mkdir(parents=True)
    lock_path = state / "disk-snapshot.lock"
    with lock_path.open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result = _run_producer(tmp_path, state_root)
    assert result.returncode == 75
    assert "another disk snapshot publication is active" in result.stderr
    assert not (tmp_path / "python.log").exists()


def test_check_is_read_only_and_does_not_invoke_python(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    result = _run_producer(tmp_path, state_root, "--check")
    assert result.returncode == 0, result.stderr
    assert "producer and dependencies are available" in result.stdout
    assert not (state_root / "ignored").exists()
    assert not (tmp_path / "python.log").exists()


def _installer_environment(tmp_path: Path) -> tuple[dict[str, str], Path, Path]:
    home = tmp_path / "home"
    state_root = home / "work/dev-hermit"
    state_root.mkdir(parents=True)
    libexec = home / ".local/libexec/dev-hermit"
    libexec.mkdir(parents=True)
    bootstrap = libexec / "operational-tool"
    bootstrap.write_text("#!/bin/sh\nexit 0\n")
    bootstrap.chmod(0o755)
    fake_bin = tmp_path / "system-bin"
    fake_bin.mkdir()
    log = tmp_path / "systemctl.log"
    systemctl = fake_bin / "systemctl"
    systemctl.write_text(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SYSTEMCTL_LOG\"\n"
    )
    systemctl.chmod(0o755)
    unit_dir = tmp_path / "units"
    environment = dict(os.environ)
    environment.update(
        {
            "HOME": str(home),
            "DEV_HERMIT_PARENT": str(state_root),
            "HERMIT_USER_UNIT_DIR": str(unit_dir),
            "SYSTEMCTL_BIN": str(systemctl),
            "SYSTEMCTL_LOG": str(log),
            "OPERATIONAL_TOOL_BIN": str(bootstrap),
        }
    )
    return environment, unit_dir, log


def test_installer_check_does_not_write_or_call_systemd(tmp_path: Path) -> None:
    environment, unit_dir, log = _installer_environment(tmp_path)
    result = subprocess.run(
        [str(INSTALLER), "--check"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert not unit_dir.exists()
    assert not log.exists()


def test_installer_copies_exact_units_and_start_is_explicit(tmp_path: Path) -> None:
    environment, unit_dir, log = _installer_environment(tmp_path)
    result = subprocess.run(
        [str(INSTALLER), "--no-start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert (unit_dir / SERVICE.name).read_bytes() == SERVICE.read_bytes()
    assert (unit_dir / TIMER.name).read_bytes() == TIMER.read_bytes()
    assert log.read_text().splitlines() == [
        "--user daemon-reload",
        "--user enable hermit-disk-snapshot.timer",
    ]
    assert "timer enabled but not started" in result.stdout

    log.unlink()
    started = subprocess.run(
        [str(INSTALLER), "--start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert started.returncode == 0, started.stderr
    assert log.read_text().splitlines() == [
        "--user daemon-reload",
        "--user enable hermit-disk-snapshot.timer",
        "--user restart hermit-disk-snapshot.timer",
        "--user status --no-pager hermit-disk-snapshot.service hermit-disk-snapshot.timer",
    ]
