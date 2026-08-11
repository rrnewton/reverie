#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""The acceptance property: a receipt one parent produced, another can read.

This is the failure that recurred three times on 2026-08-11. A receipt was
VALIDATED to the parent that produced it and NOT-VALIDATED to every other actor,
including the landing authority, because publication was a manual command nobody
ran. A green only its producer can see is not landing authority for anyone.

Everything here runs against an ISOLATED bare remote and throwaway clones. It
deliberately does NOT run a real Hermit validate: that takes a box-exclusive
lock for ~13 minutes, and on 2026-08-11 that lock was being held clear for a P0
compat sweep. Manufacturing a receipt by taking the lock would have damaged the
thing the automation exists to protect. What this pins is the mechanism -- spool,
publish, retain, and read from a checkout that never saw the spool.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest


HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import publisher  # noqa: E402
import validate_rows  # noqa: E402


SHA = "b" * 40


def event(identifier: str) -> dict:
    return {
        "schema": "validate-ledger/v1",
        "event_id": identifier,
        "event_type": "run.result",
        "run_id": identifier,
        "emitted_at": "2026-08-11T05:00:00Z",
        "team": "hermit",
        "host": "hosta",
        "outcome": "pass",
        "commit": SHA,
        "producer": {"source": "observed", "tool": "t", "tool_version": "5"},
    }


def git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=True
    )


@pytest.fixture()
def fleet(tmp_path):
    """A bare origin plus a producing parent and a publisher clone."""
    bare = tmp_path / "origin.git"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(bare)], check=True)
    producer = tmp_path / "dev-hermit-producer"
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
    return bare, producer, clone


def test_a_receipt_one_parent_produced_is_readable_by_another(fleet) -> None:
    bare, producer, clone = fleet
    spool = validate_rows.live_spool_dir(producer)
    spool.mkdir(parents=True)
    publisher.spool(spool, "hermit", "hosta", [event("X1")])

    # BEFORE: a checkout that never saw the producer's spool cannot read it.
    # This is the negative leg -- without it, the positive leg proves nothing.
    reader = bare.parent / "independent-reader"
    subprocess.run(["git", "clone", "-q", str(bare), str(reader)], check=True)
    before = {e.get("event_id") for e in validate_rows.events(root=reader)}
    assert "X1" not in before, "fixture is broken: the reader could already see it"

    result = publisher.publish(clone, spool, source_root=producer)
    assert result["published"] == 1
    assert "is an ancestor of" in result["ancestry"]

    # AFTER: the SAME independent checkout, refreshed, now reads the receipt.
    git(reader, "pull", "-q", "--ff-only")
    after = {e.get("event_id") for e in validate_rows.events(root=reader)}
    assert "X1" in after, "published receipt is still invisible to another checkout"


def test_the_producer_never_blinks_across_the_publish(fleet) -> None:
    """Defect 1, at the boundary that matters: the producer's own read."""
    _, producer, clone = fleet
    spool = validate_rows.live_spool_dir(producer)
    spool.mkdir(parents=True)
    publisher.spool(spool, "hermit", "hosta", [event("X2")])

    assert "X2" in {e.get("event_id") for e in validate_rows.events(root=producer)}
    publisher.publish(clone, spool, source_root=producer)
    # The producing root has NOT pulled. Before retention this read returned
    # nothing at all for a receipt it had just published.
    assert "X2" in {
        e.get("event_id") for e in validate_rows.events(root=producer)
    }, "producer went blind to its own published receipt"


def test_retention_is_released_once_the_producer_syncs(fleet) -> None:
    _, producer, clone = fleet
    spool = validate_rows.live_spool_dir(producer)
    spool.mkdir(parents=True)
    publisher.spool(spool, "hermit", "hosta", [event("X3")])
    publisher.publish(clone, spool, source_root=producer)
    assert publisher.retained(spool), "must be retained while the producer is stale"

    git(producer, "fetch", "-q", "origin", "main")
    git(producer, "merge", "-q", "--ff-only", "FETCH_HEAD")
    # The next publish sweep reaps what the producer can now read for itself.
    publisher.publish(clone, spool, source_root=producer)
    assert not publisher.retained(spool), "retention must be released, not permanent"
    assert "X3" in {e.get("event_id") for e in validate_rows.events(root=producer)}
