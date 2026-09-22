#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Drain every producing parent's validation-ledger and per-test series spools.

WHY THIS EXISTS. Publication was a manual command. Nothing ran it, so a receipt
or per-test run was readable only by the parent that produced it and invisible
to every other actor. For validation receipts that recurred three times on
2026-08-11 alone, the worst case being a fully qualifying 794-second green that
sat invisible for 55 minutes. The same mechanism publishes the per-test series;
without it, the run history would repeat that defect by construction.

WHY IT IS NOT A TICK GATE. tick-hub already ran this experiment and recorded the
result: `unpushed_parent_commits` had its `--rescue` mutation REMOVED on
2026-08-08 because one rescue round-trip took 65.8s and 74.8s against a 30s gate
budget, so the gate reported "could not run ... timed out" every cycle for a day
and never emitted a measurement -- while the very backlog it watched grew. A
publish is strictly heavier than a rescue. So the tick LAUNCHES this detached
and does not await it; observation keeps its budget and the mutation gets its
own. Detection (`ledger_spool_backlog.py`) stays read-only and separate.

SAFETY. Both routes use the existing append-aware publisher and retain batches
until a fresh remote commit proves publication. Validation receipts retain the
serialized parent-main writer. Cell series instead use the existing test-ledger
repository and its locked, persistent linked writer; they do not create another
parent clone or commit cell history to parent main.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


TOOL_ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", TOOL_ROOT)).resolve()
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "health"))
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "ledger"))
sys.path.insert(0, str(TOOL_ROOT / "scripts"))

from check_commit_attribution import AttributionError, detect_machine  # noqa: E402
from ledger_spool_backlog import SpoolUnavailable, producing_parents  # noqa: E402
from validate_rows import live_spool_dir  # noqa: E402
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "series"))
from series import spool_dir as series_spool_dir  # noqa: E402
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "lib"))
import bounded_capture  # noqa: E402

#: One persistent dedicated clone, reused across runs. It is NOT any parent
#: checkout: the publisher refuses a repo equal to its source root, and refuses
#: a repo that shares no root commit with it.
CLONE_RELATIVE = Path("ignored/ci-hub/ledger-publisher-clone")
ORIGIN = "https://github.com/rrnewton/dev-hermit.git"


def pending_batches(spool_dir: Path) -> int:
    """Top-level batches only: retained ones are published, not backlog."""
    if not spool_dir.is_dir():
        return 0
    return len(list(spool_dir.glob("*.jsonl")))


def ensure_clone(clone: Path, *, net_wrapper: str) -> None:
    if (clone / ".git").is_dir():
        return
    clone.parent.mkdir(parents=True, exist_ok=True)
    bounded_capture.capture(
        [net_wrapper, "git", "clone", "--quiet", "--reference", str(STATE_ROOT), ORIGIN,
         str(clone)],
        check=True,
    )


def publishing_attribution() -> str:
    """The canonical attribution line for the machine actually publishing.

    NEVER a literal machine token. `parent-main-write` verifies the final
    role/team line against THIS host via `check_commit_attribution.py
    --expected-machine auto`, so a hardcoded token publishes from exactly one
    box and is refused everywhere else. Measured 2026-08-14: the literal
    `devbig014` here left devbig030's spool unpublished for 39 hours and 14
    batches while every 15-minute sweep correctly refused. Deriving from
    `detect_machine`, the same function the verifier resolves `auto` to, keeps
    producer and verifier on ONE definition of machine identity; a second
    hostname lookup here would reintroduce the divergence from the other end.
    """
    return f"[impl agent, automation] [ledger-publisher, {detect_machine()}]"


def pending_stores(parent: Path) -> dict[str, Path]:
    """Every append-only store in this parent that has unpublished batches."""
    candidates = {
        "ledger": live_spool_dir(parent),
        "series": series_spool_dir(parent),
    }
    return {
        name: spool
        for name, spool in candidates.items()
        if pending_batches(spool)
    }


def publish_parent(parent: Path, clone: Path, *, store: str, spool: Path,
                   timeout: float, attribution: str, net_wrapper: str = "with-proxy") -> dict:
    if store == "series":
        command = [
            sys.executable, str(TOOL_ROOT / "ci-hub/series/mirror.py"),
            "--parent", str(parent), "--publish-only", "--net-wrapper", net_wrapper,
        ]
    else:
        command = [
            sys.executable, str(TOOL_ROOT / "ci-hub/ledger/publisher.py"), "publish",
            "--source-root", str(parent), "--repo", str(clone), "--spool-dir", str(spool),
            "--writer", str(TOOL_ROOT / "scripts/parent-main-write"),
            "--attribution", attribution, "--store", store,
        ]
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
        timeout=timeout,
    )
    report: dict = {
        "parent": str(parent),
        "store": store,
        "rc": result.returncode,
    }
    try:
        report.update(json.loads(result.stdout.strip().splitlines()[-1]))
    except (ValueError, IndexError):
        report["error"] = (result.stderr or result.stdout).strip()[-400:]
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--net-wrapper", default="with-proxy")
    parser.add_argument("--timeout-seconds", type=float, default=600.0)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what would be published without writing anything",
    )
    args = parser.parse_args(argv)

    clone = STATE_ROOT / CLONE_RELATIVE
    reports: list[dict] = []
    try:
        work = {
            parent: pending_stores(parent)
            for parent in producing_parents(STATE_ROOT)
        }
    except SpoolUnavailable as error:
        print(json.dumps({"state": "unknown", "error": str(error)}, sort_keys=True))
        return 2
    work = {parent: stores for parent, stores in work.items() if stores}
    if not work:
        print(json.dumps(
            {"parents_with_backlog": 0, "stores_with_backlog": 0, "published": 0},
            sort_keys=True,
        ))
        return 0
    if args.dry_run:
        print(json.dumps(
            {"parents_with_backlog": len(work),
             "stores_with_backlog": sum(len(stores) for stores in work.values()),
             "would_publish": {
                 str(parent): {
                     name: pending_batches(spool)
                     for name, spool in stores.items()
                 }
                 for parent, stores in work.items()
             }},
            sort_keys=True,
        ))
        return 0

    # Fail CLOSED on an underivable machine. Publishing under a guessed or blank
    # token would be refused by the writer anyway, and a wrong token that did
    # pass would misattribute the row.
    needs_parent_writer = any("ledger" in stores for stores in work.values())
    try:
        attribution = publishing_attribution() if needs_parent_writer else ""
    except AttributionError as error:
        print(json.dumps(
            {"error": f"cannot derive the publishing machine: {error}"},
            sort_keys=True,
        ))
        return 2

    try:
        if needs_parent_writer:
            ensure_clone(clone, net_wrapper=args.net_wrapper)
    except subprocess.CalledProcessError as error:
        print(json.dumps(
            {"error": f"cannot provision publisher clone: {error.stderr[-300:]}"},
            sort_keys=True,
        ))
        return 2

    failed = 0
    failed_parents: set[Path] = set()
    for parent, stores in work.items():
        for store, spool in stores.items():
            try:
                report = publish_parent(
                    parent,
                    clone,
                    store=store,
                    spool=spool,
                    timeout=args.timeout_seconds,
                    attribution=attribution,
                    net_wrapper=args.net_wrapper,
                )
            except subprocess.TimeoutExpired:
                report = {
                    "parent": str(parent),
                    "store": store,
                    "rc": 124,
                    "error": "publish timed out",
                }
            # A refusal is never fatal to the sweep: the batch stays spooled and the
            # backlog gate keeps reporting it. One stuck store must not stop the
            # others from becoming visible.
            if report.get("rc"):
                failed += 1
                failed_parents.add(parent)
            reports.append(report)

    print(json.dumps(
        {"parents_with_backlog": len(work),
         "stores_with_backlog": sum(len(stores) for stores in work.values()),
         "published": sum(r.get("published", 0) or 0 for r in reports),
         "failed_parents": len(failed_parents),
         "failed_stores": failed,
         "reports": reports},
        sort_keys=True,
    ))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
