#!/usr/bin/env python3
"""Contracts for the durable serialized disk-snapshot producer."""

from __future__ import annotations

import fcntl
import importlib.util
import os
from pathlib import Path
import stat
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[2]
PRODUCER = ROOT / "scripts/publish-disk-snapshots.py"
INSTALLER = ROOT / "scripts/install-disk-snapshot.sh"
BOOTSTRAP_SOURCE = ROOT / "scripts/hermit-health-tick.sh"
SERVICE = ROOT / "scripts/systemd/hermit-disk-snapshot.service"
TIMER = ROOT / "scripts/systemd/hermit-disk-snapshot.timer"

SPEC = importlib.util.spec_from_file_location("publish_disk_snapshots", PRODUCER)
assert SPEC is not None and SPEC.loader is not None
publish_disk_snapshots = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publish_disk_snapshots)


def _fake_dependencies(tmp_path: Path) -> Path:
    fake_bin = tmp_path / "dependency-bin"
    fake_bin.mkdir(exist_ok=True)
    for name in ("btrfs", "compsize", "nice", "ionice"):
        executable = fake_bin / name
        executable.write_text("#!/bin/sh\nexit 0\n")
        executable.chmod(0o755)
    return fake_bin


def _run_producer(
    tmp_path: Path, state_root: Path, *arguments: str
) -> subprocess.CompletedProcess[str]:
    environment = dict(os.environ)
    fake_bin = _fake_dependencies(tmp_path)
    environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
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
    bootstrap = BOOTSTRAP_SOURCE.read_text()
    exact_exec = (
        "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
        "--run-current-tool --state-root %h/work/dev-hermit "
        "--tool-relative scripts/publish-disk-snapshots.py -- "
        "--state-root %h/work/dev-hermit --population-timeout 180"
    )
    assert service.count("ExecStart=") == 1
    assert exact_exec in service
    assert "Environment=HERMIT_HEALTH_TOOL_LOCK_TIMEOUT=120" in service
    assert 'flock -w "${HERMIT_HEALTH_TOOL_LOCK_TIMEOUT:-120}"' in bootstrap
    assert "TimeoutStartSec=540" in service
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
    assert 120 + 110 + 30 + 180 + 30 + 30 < 540
    assert 540 < 15 * 60
    assert 15 * 60 + 60 + 540 < 45 * 60


def test_producer_runs_exact_commands_serially(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    state = state_root / "ignored/ops-state"
    state.mkdir(parents=True)
    lock = state / "disk-snapshot.lock"
    lock.write_bytes(b"retained lock contents\n")
    calls: list[tuple[list[str], dict[str, object]]] = []

    def run(
        command: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess[object]:
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0)

    result = publish_disk_snapshots.publish(
        state_root=state_root,
        population_timeout=180,
        producer=ROOT / "ci-hub/health/operational_health.py",
        run=run,
    )
    health = ROOT / "ci-hub/health/operational_health.py"
    assert result == 0
    assert lock.read_bytes() == b"retained lock contents\n"
    assert [command for command, _kwargs in calls] == [
        [
            sys.executable,
            str(health),
            "cache-disk-paths",
            "--state-root",
            str(state_root),
            "--state-file",
            str(state / "disk-paths.json"),
            "--previous-state-file",
            str(state / "disk-paths.previous.json"),
            "--population-timeout",
            "180",
        ],
        [
            sys.executable,
            str(health),
            "cache-disk",
            "--filesystem-path",
            str(state_root),
            "--state-file",
            str(state / "disk.json"),
        ],
    ]
    assert all(kwargs["check"] is False for _command, kwargs in calls)


def test_first_failure_is_closed_and_skips_second_publication(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    calls: list[list[str]] = []

    def fail(
        command: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[object]:
        calls.append(command)
        return subprocess.CompletedProcess(command, 42)

    result = publish_disk_snapshots.publish(
        state_root=state_root,
        population_timeout=180,
        run=fail,
    )
    assert result == 42
    assert len(calls) == 1
    assert "cache-disk-paths" in calls[0]


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
    assert not (state / "disk.json").exists()


def test_symlink_lock_is_refused_without_following_or_truncating(
    tmp_path: Path,
) -> None:
    state_root = tmp_path / "state"
    state = state_root / "ignored/ops-state"
    state.mkdir(parents=True)
    victim = tmp_path / "victim"
    victim.write_bytes(b"must remain byte-identical\n")
    (state / "disk-snapshot.lock").symlink_to(victim)

    result = _run_producer(tmp_path, state_root)

    assert result.returncode == 2
    assert "lock path must not be a symlink" in result.stderr
    assert victim.read_bytes() == b"must remain byte-identical\n"
    assert not (state / "disk.json").exists()


def test_replaced_lock_path_is_refused_after_acquisition(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    lock_path = tmp_path / "disk-snapshot.lock"
    lock_path.write_bytes(b"original\n")
    displaced = tmp_path / "displaced"

    original_verify = publish_disk_snapshots._verify_lock_path

    def replace_then_verify(path: Path, held_fd: int) -> None:
        lock_path.rename(displaced)
        lock_path.write_bytes(b"replacement\n")
        original_verify(path, held_fd)

    monkeypatch.setattr(
        publish_disk_snapshots, "_verify_lock_path", replace_then_verify
    )

    with pytest.raises(
        publish_disk_snapshots.LockAuthorityError,
        match="lock path changed after it was opened",
    ):
        with publish_disk_snapshots.stable_lock(lock_path):
            raise AssertionError("an unstable lock must never enter its body")
    assert lock_path.read_bytes() == b"replacement\n"
    assert displaced.read_bytes() == b"original\n"


def test_multi_link_lock_is_refused(tmp_path: Path) -> None:
    lock_path = tmp_path / "disk-snapshot.lock"
    alias = tmp_path / "lock-alias"
    lock_path.touch()
    os.link(lock_path, alias)
    with pytest.raises(
        publish_disk_snapshots.LockAuthorityError,
        match="single-link",
    ):
        with publish_disk_snapshots.stable_lock(lock_path):
            raise AssertionError("a multi-link lock must never enter its body")


def test_check_is_read_only(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    result = _run_producer(tmp_path, state_root, "--check")
    assert result.returncode == 0, result.stderr
    assert "producer and dependencies are available" in result.stdout
    assert not (state_root / "ignored").exists()


def _installer_environment(tmp_path: Path) -> tuple[dict[str, str], Path, Path, Path]:
    home = tmp_path / "home"
    state_root = home / "work/dev-hermit"
    state_root.mkdir(parents=True)
    libexec = home / ".local/libexec/dev-hermit"
    libexec.mkdir(parents=True)
    bootstrap = libexec / "operational-tool"
    bootstrap.write_bytes(BOOTSTRAP_SOURCE.read_bytes())
    bootstrap.chmod(0o555)
    fake_bin = tmp_path / "system-bin"
    fake_bin.mkdir()
    log = tmp_path / "systemctl.log"
    systemctl = fake_bin / "systemctl"
    systemctl.write_text(
        "#!/usr/bin/env bash\n"
        "set -eu\n"
        'printf \'%s\\n\' "$*" >> "$SYSTEMCTL_LOG"\n'
        "case \" $* \" in *' status '*) exit 3;; esac\n"
        "property= unit=\n"
        'for argument in "$@"; do\n'
        "  case $argument in --property=*) property=${argument#--property=};; esac\n"
        "  unit=$argument\n"
        "done\n"
        'case "$unit:$property" in\n'
        "  hermit-disk-snapshot.service:LoadState) printf '%s\\n' \"${FAKE_SERVICE_LOAD:-loaded}\";;\n"
        "  hermit-disk-snapshot.service:ActiveState) printf '%s\\n' \"${FAKE_SERVICE_ACTIVE:-inactive}\";;\n"
        "  hermit-disk-snapshot.service:SubState) printf '%s\\n' \"${FAKE_SERVICE_SUB:-dead}\";;\n"
        "  hermit-disk-snapshot.timer:LoadState) printf '%s\\n' \"${FAKE_TIMER_LOAD:-loaded}\";;\n"
        "  hermit-disk-snapshot.timer:ActiveState) printf '%s\\n' \"${FAKE_TIMER_ACTIVE:-active}\";;\n"
        "  hermit-disk-snapshot.timer:SubState) printf '%s\\n' \"${FAKE_TIMER_SUB:-waiting}\";;\n"
        "esac\n"
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
        }
    )
    return environment, unit_dir, log, bootstrap


def test_installer_check_binds_exact_installed_bootstrap_without_writes(
    tmp_path: Path,
) -> None:
    environment, unit_dir, log, bootstrap = _installer_environment(tmp_path)
    before = bootstrap.read_bytes()
    result = subprocess.run(
        [str(INSTALLER), "--check"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "installed bootstrap matches reviewed source" in result.stdout
    assert bootstrap.read_bytes() == before
    assert not unit_dir.exists()
    assert not log.exists()


def test_installer_check_rejects_dummy_bootstrap_and_ignores_override(
    tmp_path: Path,
) -> None:
    environment, unit_dir, log, bootstrap = _installer_environment(tmp_path)
    bootstrap.chmod(0o755)
    bootstrap.write_text("#!/bin/sh\nexit 0\n")
    bootstrap.chmod(0o555)
    environment["OPERATIONAL_TOOL_BIN"] = "/bin/true"
    result = subprocess.run(
        [str(INSTALLER), "--check"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 2
    assert "differs from reviewed source" in result.stderr
    assert not unit_dir.exists()
    assert not log.exists()


def test_installer_copies_bootstrap_and_units_before_explicit_start(
    tmp_path: Path,
) -> None:
    environment, unit_dir, log, bootstrap = _installer_environment(tmp_path)
    bootstrap.chmod(0o755)
    bootstrap.write_text("stale bootstrap\n")
    bootstrap.chmod(0o755)
    result = subprocess.run(
        [str(INSTALLER), "--no-start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert bootstrap.read_bytes() == BOOTSTRAP_SOURCE.read_bytes()
    assert not bootstrap.is_symlink()
    assert bootstrap.stat().st_mode & stat.S_IWUSR == 0
    assert (unit_dir / SERVICE.name).read_bytes() == SERVICE.read_bytes()
    assert (unit_dir / TIMER.name).read_bytes() == TIMER.read_bytes()
    assert log.read_text().splitlines() == [
        "--user daemon-reload",
        "--user enable hermit-disk-snapshot.timer",
    ]
    assert "timer enabled but not started" in result.stdout


def test_installer_start_accepts_healthy_inactive_oneshot_without_status_rc3(
    tmp_path: Path,
) -> None:
    environment, _unit_dir, log, _bootstrap = _installer_environment(tmp_path)
    status_probe = subprocess.run(
        [
            environment["SYSTEMCTL_BIN"],
            "--user",
            "status",
            "hermit-disk-snapshot.service",
        ],
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert status_probe.returncode == 3
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
    calls = log.read_text().splitlines()
    assert not any(" status " in f" {call} " for call in calls)
    assert calls == [
        "--user daemon-reload",
        "--user enable hermit-disk-snapshot.timer",
        "--user restart hermit-disk-snapshot.timer",
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=SubState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=SubState --value hermit-disk-snapshot.timer",
    ]
    assert "service=inactive/dead timer=active/waiting" in started.stdout


def test_installer_start_rejects_timer_that_is_not_active_and_waiting(
    tmp_path: Path,
) -> None:
    environment, _unit_dir, _log, _bootstrap = _installer_environment(tmp_path)
    environment["FAKE_TIMER_ACTIVE"] = "inactive"
    result = subprocess.run(
        [str(INSTALLER), "--start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 1
    assert "timer is not loaded and waiting: loaded/inactive/waiting" in result.stderr
