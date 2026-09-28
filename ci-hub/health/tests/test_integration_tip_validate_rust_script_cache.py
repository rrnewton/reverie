"""Each decision builds ci-hub in its own rust-script cache.

Measured 2026-09-28 on devbig014: `ci-hub/ci-hub` runs `rust-script --force`,
and every rust-script on the host builds into one target directory under one
Cargo lock. An already-built `ci-hub --help` took 0.14-0.36s alone and 54.5s
and 72.8s behind other lanes' script builds; decider
20260928T174539Z-c17ba8f8b9de spent its whole 180s second-status phase waiting.
These tests pin the isolation: every ci-hub child of a decider or dry run sees
an empty cache owned by that decision, the cache is only used under tick.lock
and is removed afterwards, a cache that cannot be emptied refuses the decision,
and a live run still blocks the launch. The read-only checker tree that
ci-hub/ledger/ledger.py writes under $XDG_CACHE_HOME when DEV_HERMIT_TOOL_CACHE
is unset must neither survive a decision nor refuse the next one.
"""

from __future__ import annotations

import fcntl
import importlib.util
import json
import os
import stat
import sys
from dataclasses import replace
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_rust_script_cache_target",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

SHA = "1234567890abcdef1234567890abcdef12345678"
DISPATCH = "20260928T180000Z-abcdef123456"
WRAPPER = ROOT / "ci-hub" / "ci-hub"

LEDGER_SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_rust_script_cache_ledger",
    ROOT / "ci-hub" / "ledger" / "ledger.py",
)
ledger = importlib.util.module_from_spec(LEDGER_SPEC)
assert LEDGER_SPEC.loader is not None
sys.modules[LEDGER_SPEC.name] = ledger
LEDGER_SPEC.loader.exec_module(ledger)

ROOT_IGNORES_WRITE_BITS = pytest.mark.skipif(
    os.geteuid() == 0,
    reason="root ignores directory write bits, so a read-only tree cannot be reproduced",
)

# Stands in for rust-script behind the real ci-hub wrapper. It records the
# cache each call was given and what that cache held, then answers
# validate-status with the prepared report; anything else is refused.
FAKE_RUST_SCRIPT = """#!/usr/bin/env python3
import json, os, sys
cache = os.environ.get("XDG_CACHE_HOME")
with open(os.environ["FAKE_RUST_SCRIPT_CALLS"], "a") as calls:
    calls.write(json.dumps({
        "argv": sys.argv[1:],
        "cache": cache,
        "cache_entries": sorted(os.listdir(cache)) if cache and os.path.isdir(cache) else None,
    }) + "\\n")
if "validate-status" in sys.argv:
    with open(os.environ["FAKE_VALIDATE_STATUS"]) as report:
        text = report.read()
    sys.stdout.write(text)
    sys.exit(json.loads(text)["exit_code"])
sys.stderr.write("unexpected ci-hub command\\n")
sys.exit(3)
"""


def configured(tmp_path: Path, *, ci_hub: Path | None = None):
    source = tmp_path / "hermit"
    source.mkdir(exist_ok=True)
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o755)
    settings = driver.Settings.defaults(
        tool_root=ROOT,
        state_root=tmp_path,
        source_checkout=source,
        ci_hub=ci_hub or tmp_path / "ci-hub",
        operational_tool=operational_tool,
        tool_parent_sha="f" * 40,
        state_dir=tmp_path / "tick-state",
    )
    settings.state_dir.mkdir(parents=True, exist_ok=True)
    return replace(
        settings,
        total_timeout_seconds=driver.DETACHED_TOTAL_TIMEOUT_SECONDS,
        limits=driver.DETACHED_PHASE_LIMITS,
    )


def cache_root(settings) -> Path:
    return settings.state_dir / driver.DECISION_RUST_SCRIPT_CACHE


def stale_cache(settings) -> Path:
    root = cache_root(settings)
    (root / "rust-script" / "binaries").mkdir(parents=True)
    (root / "rust-script" / "binaries" / "left-by-a-killed-decider").write_text("x")
    return root


def child_cache(settings) -> list[str]:
    """What an authority query started through run_bounded sees."""

    completed = driver.run_bounded(
        ["sh", "-c", 'printf "%s\\n" "$XDG_CACHE_HOME"; ls -A "$XDG_CACHE_HOME"'],
        settings.state_root,
        10.0,
    )
    assert completed.returncode == 0, completed.stderr
    return completed.stdout.splitlines()


def assert_tick_lock_held(settings) -> None:
    with (settings.state_dir / "tick.lock").open("a+") as other:
        with pytest.raises(BlockingIOError):
            fcntl.flock(other.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)


def test_a_decision_gives_its_children_an_empty_cache_of_its_own(
    tmp_path: Path, monkeypatch
) -> None:
    settings = configured(tmp_path)
    shared = tmp_path / "shared-user-cache"
    monkeypatch.setenv("XDG_CACHE_HOME", str(shared))
    root = stale_cache(settings)
    seen = []

    def tick(received) -> int:
        assert_tick_lock_held(received)
        seen.append(child_cache(received))
        driver.emit(state="in-progress", launched=0, summary="observed")
        return 0

    assert driver.decide(settings, DISPATCH, tick=tick) == 0

    assert seen == [[str(root)]], "the child sees the decision cache, emptied, and nothing else"
    assert not root.exists(), "the decision removes its cache afterwards"
    assert os.environ["XDG_CACHE_HOME"] == str(shared), "the caller's cache is restored"
    record = json.loads(driver.decision_path(settings, DISPATCH).read_text())
    assert record["exit_code"] == 0


def test_the_real_ci_hub_wrapper_builds_in_the_decision_cache_and_a_live_run_still_blocks(
    tmp_path: Path, monkeypatch
) -> None:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    rust_script = fake_bin / "rust-script"
    rust_script.write_text(FAKE_RUST_SCRIPT)
    rust_script.chmod(0o755)
    calls = tmp_path / "rust-script-calls.jsonl"
    report_path = tmp_path / "validate-status.json"
    live_handle = {
        "unit": "validate-coord-1234567890ab.service",
        "state": "running",
        "sha": SHA,
        "started_at": "2026-09-28T18:07:49Z",
        "slot": "validate-fresh-1234567890ab",
        "log_file": "/logs/validate-coord.log",
        "process_verified": True,
        "admission": "admitted",
        "finished_at": None,
        "result": None,
        "detail": None,
        "exit_code": None,
    }
    report_path.write_text(
        json.dumps(
            {
                "schema_version": driver.EXACT_VALIDATE_STATUS_SCHEMA_VERSION,
                "repo": driver.REPO,
                "sha": SHA,
                "verdict": "NOT-VALIDATED",
                "exit_code": 4,
                "qualifying_count": 0,
                "disqualified_count": 0,
                "record_count": 0,
                "exact_records": [],
                "ledger_freshness": {"state": "current", "local_absence_is_authoritative": True},
                "in_progress_count": 1,
                "in_progress": [{"sha": SHA, "in_progress": True, "summary": "agent=coord"}],
                "run_handles": [live_handle],
                "unreadable_handles": [],
            }
        )
    )
    monkeypatch.setenv("PATH", f"{fake_bin}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv("FAKE_RUST_SCRIPT_CALLS", str(calls))
    monkeypatch.setenv("FAKE_VALIDATE_STATUS", str(report_path))
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "shared-user-cache"))
    settings = configured(tmp_path, ci_hub=WRAPPER)
    root = stale_cache(settings)
    spawned = []

    def must_not_spawn(*args, **_kwargs):
        spawned.append(args)
        raise AssertionError("a live run for the exact target must block the launch")

    def fetch(_settings, _timeout) -> str:
        return SHA

    def tick(received) -> int:
        return driver.run_tick(received, fetch=fetch, spawn=must_not_spawn)

    assert driver.decide(settings, DISPATCH, tick=tick) == 0

    record = json.loads(driver.decision_path(settings, DISPATCH).read_text())
    assert record["exit_code"] == 0, record["output"]
    assert record["fields"]["state"] == "in-progress"
    assert record["fields"]["launched"] == "0"
    assert spawned == []
    observed = [json.loads(line) for line in calls.read_text().splitlines()]
    assert observed, "the real wrapper reached rust-script"
    assert all(call["argv"][0] == "--force" for call in observed)
    assert all("validate-status" in call["argv"] for call in observed)
    assert {call["cache"] for call in observed} == {str(root)}
    assert observed[0]["cache_entries"] == [], "the first build starts from an empty cache"
    assert not root.exists()


def test_a_dry_run_decides_in_the_decision_cache(tmp_path: Path, monkeypatch) -> None:
    state_dir = tmp_path / "tick-state"
    shared = tmp_path / "shared-user-cache"
    monkeypatch.setenv("XDG_CACHE_HOME", str(shared))
    seen = {}

    def tick(settings, **kwargs):
        assert kwargs == {"dry_run": True}
        assert_tick_lock_held(settings)
        seen["child"] = child_cache(settings)
        seen["root"] = cache_root(settings)
        return 0

    monkeypatch.setattr(driver, "run_tick", tick)
    assert (
        driver.main(
            ["--state-root", str(tmp_path), "--state-dir", str(state_dir), "--dry-run"]
        )
        == 0
    )
    assert seen["child"] == [str(seen["root"])]
    assert not seen["root"].exists()
    assert os.environ["XDG_CACHE_HOME"] == str(shared)


def test_the_gate_itself_never_touches_the_decision_cache(tmp_path: Path, monkeypatch) -> None:
    state_dir = tmp_path / "tick-state"
    shared = tmp_path / "shared-user-cache"
    monkeypatch.setenv("XDG_CACHE_HOME", str(shared))
    seen = {}

    def gate(settings, **_kwargs):
        seen["cache"] = os.environ.get("XDG_CACHE_HOME")
        seen["exists"] = cache_root(settings).exists()
        return 0

    monkeypatch.setattr(driver, "gate_tick", gate)
    assert driver.main(["--state-root", str(tmp_path), "--state-dir", str(state_dir)]) == 0
    assert seen == {"cache": str(shared), "exists": False}


def test_a_cache_that_cannot_be_emptied_refuses_the_decision(
    tmp_path: Path, monkeypatch
) -> None:
    settings = configured(tmp_path)
    shared = tmp_path / "shared-user-cache"
    monkeypatch.setenv("XDG_CACHE_HOME", str(shared))
    stale_cache(settings)

    def cannot_remove(path, *args, **kwargs):
        raise PermissionError(13, "Permission denied", str(path))

    monkeypatch.setattr(driver.shutil, "rmtree", cannot_remove)

    def tick(_settings) -> int:
        raise AssertionError("no decision may run in a cache it could not empty")

    assert driver.decide(settings, DISPATCH, tick=tick) == 0

    record = json.loads(driver.decision_path(settings, DISPATCH).read_text())
    assert record["exit_code"] == 2
    assert record["fields"]["state"] == "unknown"
    assert record["fields"]["launched"] == "0"
    assert "empty rust-script cache" in record["fields"]["summary"]
    assert "Permission denied" in record["fields"]["summary"]
    assert os.environ["XDG_CACHE_HOME"] == str(shared)


def test_a_symlinked_cache_is_replaced_and_its_target_is_left_alone(
    tmp_path: Path, monkeypatch
) -> None:
    settings = configured(tmp_path)
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "shared-user-cache"))
    elsewhere = tmp_path / "someone-elses-cache"
    elsewhere.mkdir()
    (elsewhere / "keep").write_text("not ours")
    cache_root(settings).symlink_to(elsewhere, target_is_directory=True)
    seen = []

    def tick(received) -> int:
        root = cache_root(received)
        assert root.is_dir() and not root.is_symlink()
        seen.append(child_cache(received))
        return 0

    assert driver.decide(settings, DISPATCH, tick=tick) == 0

    assert seen == [[str(cache_root(settings))]]
    assert (elsewhere / "keep").read_text() == "not ours"
    assert not cache_root(settings).exists()


def materialize_ledger_checker(monkeypatch, cache: Path) -> Path:
    """Have the real ledger producer write its read-only tree under `cache`."""

    monkeypatch.delenv("DEV_HERMIT_TOOL_CACHE", raising=False)
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER_CACHE", raising=False)
    monkeypatch.delenv("DEV_HERMIT_TOOL_AUTHORITY", raising=False)
    monkeypatch.setenv("XDG_CACHE_HOME", str(cache))
    checker = ledger._materialized_shared_type_checker()
    assert checker.is_relative_to(cache), checker
    content_root = checker.parents[2]
    for directory in (content_root, checker.parent):
        mode = stat.S_IMODE(directory.lstat().st_mode)
        assert mode == 0o555, f"{directory} is {oct(mode)}; the producer changed shape"
    return checker


@ROOT_IGNORES_WRITE_BITS
def test_a_read_only_ledger_checker_tree_neither_survives_nor_refuses_the_next_decision(
    tmp_path: Path, monkeypatch
) -> None:
    settings = configured(tmp_path)
    root = cache_root(settings)
    shared = tmp_path / "shared-user-cache"
    root.mkdir(parents=True)
    materialize_ledger_checker(monkeypatch, root)
    monkeypatch.setenv("XDG_CACHE_HOME", str(shared))
    seen = []

    def tick(received) -> int:
        seen.append(child_cache(received))
        materialize_ledger_checker(monkeypatch, cache_root(received))
        driver.emit(state="in-progress", launched=0, summary="observed")
        return 0

    for dispatch in (DISPATCH, "20260928T190000Z-abcdef123456"):
        assert driver.decide(settings, dispatch, tick=tick) == 0
        record = json.loads(driver.decision_path(settings, dispatch).read_text())
        assert record["exit_code"] == 0, record["fields"]["summary"]
        assert record["fields"]["state"] == "in-progress"
        assert not root.exists(), "the read-only tree the decision's tools wrote is gone"

    assert seen == [[str(root)], [str(root)]], "each decision started from an empty cache"


@ROOT_IGNORES_WRITE_BITS
def test_removing_the_cache_never_changes_a_read_only_directory_it_links_to(
    tmp_path: Path, monkeypatch
) -> None:
    settings = configured(tmp_path)
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "shared-user-cache"))
    elsewhere = tmp_path / "someone-elses-read-only-tree"
    elsewhere.mkdir()
    (elsewhere / "keep").write_text("not ours")
    elsewhere.chmod(0o555)
    root = stale_cache(settings)
    (root / "rust-script" / "link").symlink_to(elsewhere, target_is_directory=True)
    (root / "rust-script").chmod(0o555)

    def tick(_received) -> int:
        driver.emit(state="in-progress", launched=0, summary="observed")
        return 0

    try:
        assert driver.decide(settings, DISPATCH, tick=tick) == 0
        record = json.loads(driver.decision_path(settings, DISPATCH).read_text())
        assert record["exit_code"] == 0, record["fields"]["summary"]
        assert not root.exists()
        assert stat.S_IMODE(elsewhere.lstat().st_mode) == 0o555
        assert (elsewhere / "keep").read_text() == "not ours"
    finally:
        elsewhere.chmod(0o755)
