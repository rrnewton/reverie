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
    resource_limits = [
        line
        for line in service.splitlines()
        if line.startswith(
            ("MemoryHigh=", "MemoryMax=", "MemorySwapMax=", "TasksMax=")
        )
    ]
    assert resource_limits == [
        "MemoryHigh=512M",
        "MemoryMax=768M",
        "MemorySwapMax=128M",
        "TasksMax=512",
    ]
    assert "Nice=19" in service
    assert "IOSchedulingClass=idle" in service
    assert "OnCalendar=*:0/15" in timer
    assert "Persistent=true" in timer
    assert "AccuracySec=1min" in timer
    assert 540 < 15 * 60
    assert 15 * 60 + 60 + 540 < 45 * 60


def test_producer_runs_exact_commands_serially(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    state = state_root / "ignored/ops-state"
    state.mkdir(parents=True)
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


@pytest.mark.parametrize(
    ("returncodes", "expected"),
    [
        ([42, 0], 42),
        ([0, 43], 43),
        ([42, 43], 42),
    ],
)
def test_each_independent_publication_runs_and_failures_aggregate(
    tmp_path: Path, returncodes: list[int], expected: int
) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    calls: list[list[str]] = []
    remaining = list(returncodes)

    def run(
        command: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[object]:
        calls.append(command)
        return subprocess.CompletedProcess(command, remaining.pop(0))

    result = publish_disk_snapshots.publish(
        state_root=state_root,
        population_timeout=180,
        run=run,
    )
    assert result == expected
    assert not remaining
    assert len(calls) == 2
    assert "cache-disk-paths" in calls[0]
    assert "cache-disk" in calls[1]


def test_lock_contention_refuses_without_publishing(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state = state_root / "ignored/ops-state"
    state.mkdir(parents=True)
    root_fd = os.open(state_root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        fcntl.flock(root_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result = _run_producer(tmp_path, state_root)
    finally:
        os.close(root_fd)
    assert result.returncode == 75
    assert "state-root lifecycle lock is busy" in result.stderr
    assert not (state / "disk.json").exists()


def test_symlinked_ops_state_is_refused_without_writing_target(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    ignored = state_root / "ignored"
    ignored.mkdir(parents=True)
    victim = tmp_path / "victim-directory"
    victim.mkdir()
    marker = victim / "marker"
    marker.write_bytes(b"must remain byte-identical\n")
    (ignored / "ops-state").symlink_to(victim)

    result = _run_producer(tmp_path, state_root)

    assert result.returncode == 2
    assert "ops-state is not its exact resolved path" in result.stderr
    assert marker.read_bytes() == b"must remain byte-identical\n"
    assert not (victim / "disk.json").exists()


def test_post_entry_ops_state_replacement_cannot_rotate_lock_authority(
    tmp_path: Path,
) -> None:
    state_root = tmp_path / "state"
    state = state_root / "ignored/ops-state"
    state.mkdir(parents=True)
    displaced = state_root / "ignored/ops-state.displaced"
    nested_calls: list[list[str]] = []

    def nested_run(
        command: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[object]:
        nested_calls.append(command)
        return subprocess.CompletedProcess(command, 0)

    with publish_disk_snapshots.stable_root_lock(state_root):
        state.rename(displaced)
        state.mkdir()
        with pytest.raises(
            publish_disk_snapshots.LockBusyError,
            match="state-root lifecycle lock is busy",
        ):
            publish_disk_snapshots.publish(
                state_root=state_root,
                population_timeout=180,
                run=nested_run,
            )
    assert nested_calls == []

    result = publish_disk_snapshots.publish(
        state_root=state_root,
        population_timeout=180,
        run=nested_run,
    )
    assert result == 0
    assert len(nested_calls) == 2


def test_state_root_authority_refuses_a_noncanonical_alias(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    alias = tmp_path / "state-alias"
    alias.symlink_to(state_root, target_is_directory=True)
    with pytest.raises(
        publish_disk_snapshots.LockAuthorityError,
        match="state root is not its exact resolved path",
    ):
        with publish_disk_snapshots.stable_root_lock(alias):
            raise AssertionError("a noncanonical root must never enter its body")

    result = _run_producer(tmp_path, alias)
    assert result.returncode == 2
    assert "state root is not its exact resolved path" in result.stderr


def test_replaced_state_root_is_refused_before_entering(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    displaced = tmp_path / "state.displaced"
    original_verify = publish_disk_snapshots._verify_root_path

    def replace_then_verify(path: Path, held_fd: int) -> None:
        state_root.rename(displaced)
        state_root.mkdir()
        original_verify(path, held_fd)

    monkeypatch.setattr(
        publish_disk_snapshots, "_verify_root_path", replace_then_verify
    )
    with pytest.raises(
        publish_disk_snapshots.LockAuthorityError,
        match="state-root path changed after it was opened",
    ):
        with publish_disk_snapshots.stable_root_lock(state_root):
            raise AssertionError("a replaced state root must never enter its body")


def test_check_is_read_only(tmp_path: Path) -> None:
    state_root = tmp_path / "state"
    state_root.mkdir()
    result = _run_producer(tmp_path, state_root, "--check")
    assert result.returncode == 0, result.stderr
    assert "producer and dependencies are available" in result.stdout
    assert not (state_root / "ignored").exists()


def _installer_environment(
    tmp_path: Path, *, preinstall: bool = True
) -> tuple[dict[str, str], Path, Path, Path]:
    home = tmp_path / "home"
    state_root = home / "work/dev-hermit"
    state_root.mkdir(parents=True)
    libexec = home / ".local/libexec/dev-hermit"
    libexec.mkdir(parents=True)
    bootstrap = libexec / "operational-tool"
    helper_source = tmp_path / "fixture-gh"
    helper_source.write_text("#!/bin/sh\nexit 0\n")
    helper_source.chmod(0o755)
    github_helper = libexec / "gh"
    if preinstall:
        bootstrap.write_bytes(BOOTSTRAP_SOURCE.read_bytes())
        bootstrap.chmod(0o555)
        github_helper.write_bytes(helper_source.read_bytes())
        github_helper.chmod(0o555)
    fake_bin = tmp_path / "system-bin"
    fake_bin.mkdir()
    log = tmp_path / "systemctl.log"
    systemctl = fake_bin / "systemctl"
    systemctl.write_text(
        "#!/usr/bin/env bash\n"
        "set -eu\n"
        'printf \'%s\\n\' "$*" >> "$SYSTEMCTL_LOG"\n'
        "case \" $* \" in *' status '*) exit 3;; esac\n"
        'case "${FAKE_FAIL_COMMAND:-}:$*" in\n'
        "  'enable:--user enable hermit-disk-snapshot.timer') exit 17;;\n"
        "  'restart:--user restart hermit-disk-snapshot.timer') exit 19;;\n"
        "  'service-active:--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.service') exit 23;;\n"
        "esac\n"
        "service_load=not-found\n"
        "timer_load=not-found\n"
        '[[ -f "$HERMIT_USER_UNIT_DIR/hermit-disk-snapshot.service" ]] && service_load=loaded\n'
        '[[ -f "$HERMIT_USER_UNIT_DIR/hermit-disk-snapshot.timer" ]] && timer_load=loaded\n'
        "timer_active=inactive\n"
        "timer_sub=dead\n"
        'if grep -Fq -- "--user restart hermit-disk-snapshot.timer" "$SYSTEMCTL_LOG"; then\n'
        "  timer_active=active\n"
        "  timer_sub=waiting\n"
        "fi\n"
        "property= unit=\n"
        'for argument in "$@"; do\n'
        "  case $argument in --property=*) property=${argument#--property=};; esac\n"
        "  unit=$argument\n"
        "done\n"
        'case "$unit:$property" in\n'
        "  hermit-disk-snapshot.service:LoadState) printf '%s\\n' \"${FAKE_SERVICE_LOAD:-$service_load}\";;\n"
        "  hermit-disk-snapshot.service:ActiveState)\n"
        "    if [[ -n ${FAKE_POST_SERVICE_ACTIVE:-} ]] && grep -Fq -- '--user daemon-reload' \"$SYSTEMCTL_LOG\"; then\n"
        "      printf '%s\\n' \"$FAKE_POST_SERVICE_ACTIVE\"\n"
        "    else\n"
        "      printf '%s\\n' \"${FAKE_SERVICE_ACTIVE:-inactive}\"\n"
        "    fi;;\n"
        "  hermit-disk-snapshot.service:SubState) printf '%s\\n' \"${FAKE_SERVICE_SUB:-dead}\";;\n"
        "  hermit-disk-snapshot.timer:LoadState) printf '%s\\n' \"${FAKE_TIMER_LOAD:-$timer_load}\";;\n"
        "  hermit-disk-snapshot.timer:ActiveState) printf '%s\\n' \"${FAKE_TIMER_ACTIVE:-$timer_active}\";;\n"
        "  hermit-disk-snapshot.timer:SubState) printf '%s\\n' \"${FAKE_TIMER_SUB:-$timer_sub}\";;\n"
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
            "DEV_HERMIT_GH_SOURCE": str(helper_source),
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
    assert (
        "installed bootstrap and GitHub credential helper match reviewed sources"
        in result.stdout
    )
    assert bootstrap.read_bytes() == before
    assert not unit_dir.exists()
    assert not log.exists()


@pytest.mark.parametrize("helper_state", ["missing", "different"])
def test_installer_check_rejects_absent_or_mismatched_github_helper_without_writes(
    tmp_path: Path, helper_state: str
) -> None:
    environment, unit_dir, log, bootstrap = _installer_environment(tmp_path)
    github_helper = bootstrap.parent / "gh"
    if helper_state == "missing":
        github_helper.unlink()
    else:
        github_helper.chmod(0o755)
        github_helper.write_text("#!/bin/sh\nexit 17\n")
        github_helper.chmod(0o555)
    result = subprocess.run(
        [str(INSTALLER), "--check"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 2
    assert "installed GitHub credential helper is absent or differs" in result.stderr
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
    github_helper = bootstrap.parent / "gh"
    helper_source = Path(environment["DEV_HERMIT_GH_SOURCE"])
    assert github_helper.read_bytes() == helper_source.read_bytes()
    assert not github_helper.is_symlink()
    assert github_helper.stat().st_mode & stat.S_IWUSR == 0
    assert (unit_dir / SERVICE.name).read_bytes() == SERVICE.read_bytes()
    assert (unit_dir / TIMER.name).read_bytes() == TIMER.read_bytes()
    assert log.read_text().splitlines() == [
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.service",
        "--user daemon-reload",
        "--user disable --now hermit-disk-snapshot.timer",
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=SubState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=SubState --value hermit-disk-snapshot.service",
    ]
    assert "timer installed disabled and inactive" in result.stdout


def test_cold_install_then_check_is_executable_and_keeps_timer_disabled(
    tmp_path: Path,
) -> None:
    environment, unit_dir, log, bootstrap = _installer_environment(
        tmp_path, preinstall=False
    )
    github_helper = bootstrap.parent / "gh"
    assert not bootstrap.exists()
    assert not github_helper.exists()

    installed = subprocess.run(
        [str(INSTALLER), "--no-start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert installed.returncode == 0, installed.stderr
    assert bootstrap.read_bytes() == BOOTSTRAP_SOURCE.read_bytes()
    assert (
        github_helper.read_bytes()
        == Path(environment["DEV_HERMIT_GH_SOURCE"]).read_bytes()
    )
    assert (unit_dir / TIMER.name).exists()
    calls_after_install = log.read_text()
    assert " enable " not in f" {calls_after_install} "
    assert "--user disable --now hermit-disk-snapshot.timer" in calls_after_install

    checked = subprocess.run(
        [str(INSTALLER), "--check"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert checked.returncode == 0, checked.stderr
    assert "match reviewed sources" in checked.stdout
    assert log.read_text() == calls_after_install


@pytest.mark.parametrize("active_unit", ["timer", "service"])
def test_no_start_refuses_an_active_existing_unit_before_installing(
    tmp_path: Path, active_unit: str
) -> None:
    environment, unit_dir, log, bootstrap = _installer_environment(tmp_path)
    unit_dir.mkdir()
    unit_source = TIMER if active_unit == "timer" else SERVICE
    (unit_dir / unit_source.name).write_bytes(unit_source.read_bytes())
    original_bootstrap = bootstrap.read_bytes()
    github_helper = bootstrap.parent / "gh"
    original_helper = github_helper.read_bytes()
    if active_unit == "timer":
        environment.update(
            {
                "FAKE_TIMER_ACTIVE": "active",
                "FAKE_TIMER_SUB": "waiting",
            }
        )
        expected_state = "loaded/active + not-found/inactive"
    else:
        environment.update(
            {
                "FAKE_SERVICE_ACTIVE": "activating",
                "FAKE_SERVICE_SUB": "start",
            }
        )
        expected_state = "not-found/inactive + loaded/activating"
    result = subprocess.run(
        [str(INSTALLER), "--no-start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 1
    assert f"existing timer/service is {expected_state}" in result.stderr
    assert bootstrap.read_bytes() == original_bootstrap
    assert github_helper.read_bytes() == original_helper
    assert log.read_text().splitlines() == [
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.timer",
        "--user show --no-pager --property=LoadState --value hermit-disk-snapshot.service",
        "--user show --no-pager --property=ActiveState --value hermit-disk-snapshot.service",
    ]


def test_no_start_postcondition_failure_stops_both_units(tmp_path: Path) -> None:
    environment, _unit_dir, log, _bootstrap = _installer_environment(tmp_path)
    environment["FAKE_POST_SERVICE_ACTIVE"] = "activating"
    result = subprocess.run(
        [str(INSTALLER), "--no-start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 1
    assert "timer/service did not remain inactive" in result.stderr
    assert log.read_text().splitlines()[-2:] == [
        "--user disable --now hermit-disk-snapshot.timer",
        "--user stop hermit-disk-snapshot.service",
    ]


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


def test_installer_start_accepts_coherent_activating_service_and_running_timer(
    tmp_path: Path,
) -> None:
    environment, _unit_dir, _log, _bootstrap = _installer_environment(tmp_path)
    environment.update(
        {
            "FAKE_SERVICE_ACTIVE": "activating",
            "FAKE_SERVICE_SUB": "start",
            "FAKE_TIMER_SUB": "running",
        }
    )
    result = subprocess.run(
        [str(INSTALLER), "--start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "service=activating/start timer=active/running" in result.stdout


@pytest.mark.parametrize(
    ("failure", "expected_returncode"),
    [
        ("enable", 17),
        ("restart", 19),
        ("service-active", 23),
    ],
)
def test_installer_start_failure_rolls_back_timer_and_service(
    tmp_path: Path, failure: str, expected_returncode: int
) -> None:
    environment, _unit_dir, log, _bootstrap = _installer_environment(tmp_path)
    environment["FAKE_FAIL_COMMAND"] = failure
    result = subprocess.run(
        [str(INSTALLER), "--start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == expected_returncode
    assert "service=" not in result.stdout
    assert log.read_text().splitlines()[-2:] == [
        "--user disable --now hermit-disk-snapshot.timer",
        "--user stop hermit-disk-snapshot.service",
    ]


@pytest.mark.parametrize(
    ("service_active", "service_sub", "timer_active", "timer_sub"),
    [
        ("activating", "start", "active", "waiting"),
        ("inactive", "dead", "active", "running"),
        ("failed", "failed", "active", "waiting"),
        ("inactive", "dead", "inactive", "dead"),
    ],
)
def test_installer_start_rejects_incoherent_service_timer_pairs(
    tmp_path: Path,
    service_active: str,
    service_sub: str,
    timer_active: str,
    timer_sub: str,
) -> None:
    environment, _unit_dir, log, _bootstrap = _installer_environment(tmp_path)
    environment.update(
        {
            "FAKE_SERVICE_ACTIVE": service_active,
            "FAKE_SERVICE_SUB": service_sub,
            "FAKE_TIMER_ACTIVE": timer_active,
            "FAKE_TIMER_SUB": timer_sub,
        }
    )
    result = subprocess.run(
        [str(INSTALLER), "--start"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 1
    assert "incoherent service/timer state" in result.stderr
    assert log.read_text().splitlines()[-2:] == [
        "--user disable --now hermit-disk-snapshot.timer",
        "--user stop hermit-disk-snapshot.service",
    ]
