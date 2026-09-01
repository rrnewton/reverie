from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[3]
MODULE = ROOT / "ci-hub/health/rust_script_cache_cleanup.py"
SPEC = importlib.util.spec_from_file_location("rust_script_cache_cleanup", MODULE)
assert SPEC is not None and SPEC.loader is not None
cleanup = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = cleanup
SPEC.loader.exec_module(cleanup)


def cache_root(tmp_path: Path) -> Path:
    root = tmp_path / "cache" / "rust-script"
    root.mkdir(parents=True)
    return root


def fake_process(proc: Path, pid: int, *, cwd: Path | None = None) -> None:
    entry = proc / str(pid)
    (entry / "fd").mkdir(parents=True)
    (entry / "comm").write_text("sleep\n")
    (entry / "cmdline").write_bytes(b"sleep\x0060\x00")
    (entry / "environ").write_bytes(b"")
    (entry / "maps").write_text("")
    (entry / "stat").write_text(f"{pid} (sleep) S 1")
    if cwd is not None:
        os.symlink(cwd, entry / "cwd")


def test_report_only_names_over_limit_without_mutating(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    payload = root / "artifact"
    payload.write_bytes(b"payload")
    code, summary = cleanup.cleanup(root, 0, False, tmp_path / "proc")
    assert code == 1
    assert "report-only" in summary
    assert payload.read_bytes() == b"payload"


def test_report_only_does_not_create_absent_cache(tmp_path: Path) -> None:
    root = tmp_path / "cache" / "rust-script"
    root.parent.mkdir()
    code, summary = cleanup.cleanup(root, 0, False, tmp_path / "proc")
    assert code == 0
    assert "cache is absent" in summary
    assert not root.exists()


def test_apply_atomically_replaces_and_removes_generated_cache(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    (tmp_path / "proc").mkdir()
    (root / "projects").mkdir()
    (root / "projects" / "generated").write_bytes(b"payload")
    code, summary = cleanup.cleanup(root, 0, True, tmp_path / "proc")
    assert code == 0
    assert "removed generated rust-script cache" in summary
    assert root.is_dir()
    assert list(root.iterdir()) == []
    assert list(root.parent.glob(f"{cleanup.FENCE_PREFIX}*")) == []


def test_positive_cwd_reference_retains_cache(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    proc = tmp_path / "proc"
    fake_process(proc, 77, cwd=root / "projects" / "one")
    (root / "artifact").write_bytes(b"payload")
    code, summary = cleanup.cleanup(root, 0, True, proc)
    assert code == 1
    assert "pid 77 cwd=" in summary
    assert (root / "artifact").exists()


def test_parent_cwd_reference_is_not_ignored(tmp_path: Path, monkeypatch: object) -> None:
    root = cache_root(tmp_path)
    proc = tmp_path / "proc"
    fake_process(proc, 76, cwd=root)
    monkeypatch.setattr(cleanup.os, "getppid", lambda: 76)  # type: ignore[attr-defined]
    (root / "artifact").write_bytes(b"payload")

    code, summary = cleanup.cleanup(root, 0, True, proc)

    assert code == 1
    assert "pid 76 cwd=" in summary
    assert (root / "artifact").exists()


def test_rust_script_with_default_cache_retains_cache(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    proc = tmp_path / "proc"
    fake_process(proc, 78)
    (proc / "78" / "comm").write_text("rust-script\n")
    (proc / "78" / "environ").write_bytes(
        f"XDG_CACHE_HOME={root.parent}\0".encode()
    )
    (root / "artifact").write_bytes(b"payload")
    code, summary = cleanup.cleanup(root, 0, True, proc)
    assert code == 1
    assert "rust-script-env=" in summary
    assert (root / "artifact").exists()


def test_valid_inactive_prior_fence_is_removed(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    proc = tmp_path / "proc"
    proc.mkdir()
    token = "12-deadbeef"
    fence = root.parent / f"{cleanup.FENCE_PREFIX}{token}"
    fence.mkdir()
    (fence / cleanup.MARKER).write_text(
        cleanup.json.dumps(cleanup._marker_payload(root, token), sort_keys=True) + "\n"
    )
    (fence / "artifact").write_bytes(b"old")
    code, summary = cleanup.cleanup(root, 1 << 20, True, proc)
    assert code == 0
    assert "removed 1 prior fence" in summary
    assert not fence.exists()


def test_active_prior_fence_is_retained(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    proc = tmp_path / "proc"
    token = "12-stillactive"
    fence = root.parent / f"{cleanup.FENCE_PREFIX}{token}"
    fence.mkdir()
    (fence / cleanup.MARKER).write_text(
        cleanup.json.dumps(cleanup._marker_payload(root, token), sort_keys=True) + "\n"
    )
    payload = fence / "artifact"
    payload.write_bytes(b"old")
    fake_process(proc, 79, cwd=fence)

    code, summary = cleanup.cleanup(root, 1 << 20, True, proc)

    assert code == 1
    assert "retained active cleanup fence" in summary
    assert payload.read_bytes() == b"old"


def test_process_appearing_after_rename_retains_fence(
    tmp_path: Path, monkeypatch: object
) -> None:
    root = cache_root(tmp_path)
    proc = tmp_path / "proc"
    proc.mkdir()
    (root / "artifact").write_bytes(b"payload")
    calls = 0

    def appear_after_rename(candidate: Path, _proc_root: Path) -> list[cleanup.ProcessUse]:
        nonlocal calls
        calls += 1
        if calls == 2:
            return [cleanup.ProcessUse(80, "cwd", str(candidate))]
        return []

    monkeypatch.setattr(cleanup, "process_uses", appear_after_rename)  # type: ignore[attr-defined]
    code, summary = cleanup.cleanup(root, 0, True, proc)

    assert code == 1
    assert "retained active rust-script.cleanup-" in summary
    assert root.is_dir() and list(root.iterdir()) == []
    fences = list(root.parent.glob(f"{cleanup.FENCE_PREFIX}*"))
    assert len(fences) == 1
    assert (fences[0] / "artifact").read_bytes() == b"payload"


def test_unrecognized_fence_refuses_deletion(tmp_path: Path) -> None:
    root = cache_root(tmp_path)
    (tmp_path / "proc").mkdir()
    fence = root.parent / f"{cleanup.FENCE_PREFIX}not-ours"
    fence.mkdir()
    try:
        cleanup.cleanup(root, 1 << 20, True, tmp_path / "proc")
    except cleanup.CleanupUnavailable as error:
        assert "unrecognized" in str(error)
    else:
        raise AssertionError("unmarked directory was accepted as a cleanup fence")


def test_symlink_cache_root_refuses(tmp_path: Path) -> None:
    target = tmp_path / "target"
    target.mkdir()
    cache = tmp_path / "cache"
    cache.mkdir()
    root = cache / "rust-script"
    root.symlink_to(target, target_is_directory=True)
    try:
        cleanup.cleanup(root, 0, True, tmp_path / "proc")
    except cleanup.CleanupUnavailable as error:
        assert "real directory" in str(error)
    else:
        raise AssertionError("symlink cache root was accepted")


def test_tick_runs_apply_hourly_with_the_measured_bound() -> None:
    config = yaml.safe_load((ROOT / "ci-hub/health/tick-hub.yaml").read_text())
    reminder = {item["name"]: item for item in config["reminders"]}[
        "rust_script_cache_cleanup"
    ]
    assert reminder["cadence_secs"] == 3600
    assert reminder["gate"] == {
        "timeout_secs": 300,
        "cmd": (
            "python3 ci-hub/health/rust_script_cache_cleanup.py --apply "
            '--max-gib 64 --cache-root "$HOME/.cache/rust-script"'
        ),
        "when": "failure",
        "capture": True,
    }


def test_unrepresentable_limit_fails_closed(capsys: object) -> None:
    code = cleanup.main(["--max-gib", "1e308"])
    output = capsys.readouterr().out  # type: ignore[attr-defined]
    assert code == 2
    assert "state=could-not-determine" in output
