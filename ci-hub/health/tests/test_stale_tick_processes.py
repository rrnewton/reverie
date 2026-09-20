from __future__ import annotations

import contextlib
import io
import os
from pathlib import Path
import sys


HEALTH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HEALTH))
import stale_tick_processes as probe  # noqa: E402


def process(
    pid: int,
    ppid: int,
    *,
    argv: tuple[str, ...],
    start_ticks: int = 0,
) -> probe.Process:
    return probe.Process(
        pid=pid,
        ppid=ppid,
        state="S",
        start_ticks=start_ticks,
        uid=123,
        argv=argv,
        cgroup=f"/fixture/{pid}",
    )


def health_argv(root: Path) -> tuple[str, ...]:
    return (
        "python3",
        "/tool/agent-utils/py/bin/tick-hub",
        "tick",
        "--config",
        "/tool/ci-hub/health/tick-hub.yaml",
        "--fired-state",
        str(root / ".tick-hub/fired-state"),
        "--flush",
    )


def test_stale_scan_collapses_launcher_pair_and_excludes_current_ancestors(
    tmp_path: Path,
) -> None:
    tick = health_argv(tmp_path)
    other = ("python3", "something-else")
    scan = probe.Scan(
        processes={
            100: process(100, 1, argv=tick),
            101: process(101, 100, argv=tick),
            200: process(200, 1, argv=tick, start_ticks=9_000),
            500: process(500, 501, argv=other),
            501: process(501, 1, argv=tick),
        },
        unreadable_same_uid=0,
    )
    findings = probe.stale_invocations(
        scan,
        root=tmp_path,
        uptime_seconds=100,
        clock_ticks=100,
        max_age_secs=50,
        self_pid=500,
    )
    assert [(item.pid, int(age)) for item, age in findings] == [(100, 100)]


def test_match_requires_the_production_config_state_pair_and_flush(
    tmp_path: Path,
) -> None:
    argv = list(health_argv(tmp_path))
    assert probe.is_health_tick(process(1, 0, argv=tuple(argv)), tmp_path)

    argv.remove("--flush")
    assert not probe.is_health_tick(process(1, 0, argv=tuple(argv)), tmp_path)

    wrong_state = list(health_argv(tmp_path))
    index = wrong_state.index("--fired-state") + 1
    wrong_state[index] = str(tmp_path / "other-state")
    assert not probe.is_health_tick(process(1, 0, argv=tuple(wrong_state)), tmp_path)

    wrong_entrypoint = list(health_argv(tmp_path))
    wrong_entrypoint[1] = "/tmp/lookalike-tick-hub"
    assert not probe.is_health_tick(
        process(1, 0, argv=tuple(wrong_entrypoint)), tmp_path
    )


def stat_line(pid: int, *, ppid: int = 1, start_ticks: int = 123) -> str:
    fields = ["S", str(ppid), *(["0"] * 17), str(start_ticks)]
    return f"{pid} (fixture) {' '.join(fields)}\n"


def write_process(proc_root: Path, pid: int) -> Path:
    directory = proc_root / str(pid)
    directory.mkdir()
    (directory / "stat").write_text(stat_line(pid))
    uid = os.getuid()
    (directory / "status").write_text(f"Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n")
    (directory / "cmdline").write_bytes(b"python3\0something-else\0")
    (directory / "cgroup").write_text("0::/fixture\n")
    return directory


def test_same_uid_procfs_denial_refuses_a_clean_census(
    tmp_path: Path, monkeypatch,
) -> None:
    directory = write_process(tmp_path, 123)
    original = Path.read_text

    def denied(path: Path, *args, **kwargs):
        if path == directory / "stat":
            raise PermissionError("fixture denial")
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", denied)
    scan = probe.read_processes(tmp_path, os.getuid())
    assert scan.processes == {}
    assert scan.unreadable_same_uid == 1


def test_pid_reuse_is_not_spliced_into_one_process(
    tmp_path: Path, monkeypatch,
) -> None:
    directory = write_process(tmp_path, 123)
    original = Path.read_text
    reads = 0

    def reused(path: Path, *args, **kwargs):
        nonlocal reads
        if path == directory / "stat":
            reads += 1
            return stat_line(123, start_ticks=123 if reads == 1 else 999)
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", reused)
    scan = probe.read_processes(tmp_path, os.getuid())
    assert scan.processes == {}
    assert scan.unreadable_same_uid == 0


def test_gate_maps_unknown_to_tick_hub_no_result(
    tmp_path: Path, monkeypatch,
) -> None:
    monkeypatch.setattr(
        probe,
        "read_processes",
        lambda _root, _uid: probe.Scan(processes={}, unreadable_same_uid=1),
    )
    (tmp_path / "uptime").write_text("100 100\n")
    assert probe.main(["--gate", "--proc-root", str(tmp_path)]) == 75
    assert probe.main(["--proc-root", str(tmp_path)]) == 2


def test_probe_has_no_process_mutation_surface() -> None:
    source = (HEALTH / "stale_tick_processes.py").read_text()
    assert "os.kill" not in source
    assert "signal." not in source
    assert "--apply" not in source


def test_summary_cannot_create_recursive_tick_placeholders() -> None:
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        probe._field("summary", "cgroup=/fixture/#{session_name}")
    assert output.getvalue() == "summary=cgroup=/fixture/#(session_name)\n"
