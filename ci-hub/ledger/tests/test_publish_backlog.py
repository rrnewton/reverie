#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""A detached publish reports publication, not merely unit acceptance."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE.parent))

import publish_backlog  # noqa: E402
import publisher  # noqa: E402


def git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=True
    )


def event(identifier: str) -> dict:
    return {
        "schema": "validate-ledger/v1",
        "event_id": identifier,
        "event_type": "run.result",
        "run_id": identifier,
        "emitted_at": "2026-08-13T00:00:00Z",
        "team": "hermit",
        "host": "hosta",
        "outcome": "pass",
        "commit": "a" * 40,
        "producer": {"source": "observed", "tool": "t", "tool_version": "5"},
    }


@pytest.fixture()
def local_fleet(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    bare = tmp_path / "origin.git"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(bare)], check=True)
    producer = tmp_path / "producer"
    subprocess.run(["git", "init", "-q", "-b", "main", str(producer)], check=True)
    (producer / "README").write_text("ledger\n")
    git(producer, "add", "README")
    git(
        producer, "-c", "user.name=t", "-c", "user.email=t@invalid",
        "commit", "-q", "-m", "seed",
    )
    git(producer, "remote", "add", "origin", str(bare))
    git(producer, "push", "-q", "origin", "main")
    clone = tmp_path / "publisher-clone"
    subprocess.run(["git", "clone", "-q", str(bare), str(clone)], check=True)

    writer = producer / "scripts" / "parent-main-write"
    writer.parent.mkdir()
    writer.write_text('#!/bin/sh\nexec git push origin "$2:refs/heads/main"\n')
    writer.chmod(0o755)
    spool = producer / "spool"
    spool.mkdir()

    monkeypatch.setattr(publish_backlog, "CLONE_RELATIVE", clone)
    monkeypatch.setattr(publish_backlog, "producing_parents", lambda _root: [producer])
    monkeypatch.setattr(publish_backlog, "live_spool_dir", lambda _parent: spool)
    return producer, clone, spool


def test_success_exits_zero_and_drains(local_fleet, tmp_path: Path) -> None:
    _, _, spool = local_fleet
    publisher.spool(spool, "hermit", "hosta", [event("GOOD")])
    status = tmp_path / "status.json"

    assert publish_backlog.main(["--status-file", str(status)]) == 0
    completed = json.loads(status.read_text())
    assert completed["exit_code"] == 0
    assert completed["published"] == 1
    assert not list(spool.glob("*.jsonl")), "published batch stayed pending"
    assert list((spool / publisher.RETAINED_SUBDIR).glob("*.jsonl"))
    assert publish_backlog.report_status(status) == 0


def test_unpublishable_batch_is_preserved_and_reported(local_fleet, tmp_path: Path) -> None:
    _, _, spool = local_fleet
    batch = spool / "hermit__hosta__2026-08__invalid.jsonl"
    batch.write_text("{not-json}\n")
    status = tmp_path / "status.json"

    assert publish_backlog.main(["--status-file", str(status)]) == 1
    completed = json.loads(status.read_text())
    assert completed["exit_code"] == 1
    assert completed["failed_parents"] == 1
    assert completed["reports"][0]["rc"] == 2
    assert "REFUSED" in completed["reports"][0]["error"]
    assert batch.exists(), "unpublishable evidence was removed"
    assert publish_backlog.report_status(status) == 1


def captured_fields(output: str) -> dict[str, str]:
    """Mirror tick-hub's one-key/value-pair-per-line capture contract."""
    fields = {}
    for line in output.splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            fields[key] = value
    return fields


def executable(path: Path, body: str) -> None:
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


def run_launcher(tmp_path: Path, exit_code: int) -> subprocess.CompletedProcess:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    executable(fake_bin / "systemctl", "exit 1\n")
    executable(
        fake_bin / "systemd-run",
        f"printf '%s\\n' \"$@\" > {tmp_path / 'systemd-run.args'}\nexit 0\n",
    )
    executable(fake_bin / "gh", "exit 0\n")
    os.symlink(sys.executable, fake_bin / "python3")
    status = tmp_path / "status.json"
    status.write_text(json.dumps({
        "exit_code": exit_code,
        "failed_parents": 1 if exit_code else 0,
        "parents_with_backlog": 1,
        "published": 0 if exit_code else 1,
        "reports": ([{"rc": 2, "error": "deliberately unpublishable"}]
                    if exit_code else []),
    }))
    environment = os.environ.copy()
    environment.update({
        "PATH": f"{fake_bin}:/usr/bin:/bin",
        "LEDGER_PUBLISH_STATUS_FILE": str(status),
        "LEDGER_PUBLISH_UNIT": "ledger-publish-test",
    })
    return subprocess.run(
        [str(ROOT / "ci-hub/ledger/launch_publish_backlog.sh")],
        capture_output=True,
        text=True,
        env=environment,
        check=False,
    )


def test_launcher_stays_zero_after_success(tmp_path: Path) -> None:
    result = run_launcher(tmp_path, 0)
    assert result.returncode == 0, result.stdout + result.stderr
    fields = captured_fields(result.stdout)
    assert fields["state"] == "launched"
    assert fields["summary"] == "ledger publish sweep launched detached as ledger-publish-test"
    arguments = (tmp_path / "systemd-run.args").read_text()
    assert "--setenv=PATH=" in arguments
    assert "--status-file" in arguments


def test_launcher_is_loud_after_failed_publish_and_retries(tmp_path: Path) -> None:
    result = run_launcher(tmp_path, 1)
    assert result.returncode == 1
    fields = captured_fields(result.stdout)
    assert fields["state"] == "failed"
    assert "deliberately unpublishable" in fields["summary"]
    assert fields["retry"] == "launched detached as ledger-publish-test"
    assert (tmp_path / "systemd-run.args").exists(), "failure suppressed the retry"


def test_child_that_never_completes_cannot_leave_stale_success(tmp_path: Path) -> None:
    first = run_launcher(tmp_path, 0)
    assert first.returncode == 0

    # The fake systemd-run accepts the unit but deliberately never executes its
    # child. The launch marker must replace the old success before acceptance.
    status = tmp_path / "status.json"
    started = json.loads(status.read_text())
    assert started["state"] == "started"
    assert started["exit_code"] == publish_backlog.INCOMPLETE_EXIT

    second = subprocess.run(
        [str(ROOT / "ci-hub/ledger/launch_publish_backlog.sh")],
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "PATH": f"{tmp_path / 'bin'}:/usr/bin:/bin",
            "LEDGER_PUBLISH_STATUS_FILE": str(status),
            "LEDGER_PUBLISH_UNIT": "ledger-publish-test",
        },
        check=False,
    )
    fields = captured_fields(second.stdout)
    assert second.returncode == publish_backlog.INCOMPLETE_EXIT
    assert fields["state"] == "failed"
    assert fields["summary"] == "ledger publish sweep started but did not record completion"


def test_multiline_failure_cannot_create_captured_fields(tmp_path: Path) -> None:
    status = tmp_path / "status.json"
    status.write_text(json.dumps({
        "state": "completed",
        "exit_code": 1,
        "failed_parents": 1,
        "reports": [{"rc": 2, "error": "first line\nsummary=forged\nstate=completed"}],
    }))
    result = subprocess.run(
        [sys.executable, str(ROOT / "ci-hub/ledger/publish_backlog.py"),
         "--report-status", str(status)],
        capture_output=True,
        text=True,
        check=False,
    )
    fields = captured_fields(result.stdout)
    assert result.returncode == 1
    assert fields["state"] == "failed"
    assert fields["summary"].count("summary=forged") == 1
    assert len(result.stdout.splitlines()) == 2
