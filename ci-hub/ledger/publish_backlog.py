#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Drain every producing parent's ledger spool, unattended.

WHY THIS EXISTS. Publication was a manual command. Nothing ran it, so a receipt
was readable only by the parent that produced it -- VALIDATED to its producer
and NOT-VALIDATED to every other actor, including the landing authority. That
recurred three times on 2026-08-11 alone, the worst case being a fully
qualifying 794-second green that sat invisible for 55 minutes.

WHY IT IS NOT A TICK GATE. tick-hub already ran this experiment and recorded the
result: `unpushed_parent_commits` had its `--rescue` mutation REMOVED on
2026-08-08 because one rescue round-trip took 65.8s and 74.8s against a 30s gate
budget, so the gate reported "could not run ... timed out" every cycle for a day
and never emitted a measurement -- while the very backlog it watched grew. A
publish is strictly heavier than a rescue. So the tick LAUNCHES this detached
and does not await it; observation keeps its budget and the mutation gets its
own. Detection (`ledger_spool_backlog.py`) stays read-only and separate.

SAFETY. This adds no new authority. It schedules the existing publisher, whose
guarantees are unchanged: it writes through a dedicated clone, never the shared
producer parent; it goes out through `scripts/parent-main-write`, which is the
machine-wide serialized writer with a fresh-origin CAS boundary; it appends only
and refuses any content that is not a line-for-line extension of what is
published; and it drops a batch only after the commit carrying it is confirmed
an ancestor of the freshly-fetched remote. Concurrency is safe by construction:
two publishers collide at the serialized writer, and a non-fast-forward is
resolved by re-reading and re-appending, never by overwriting.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "health"))
sys.path.insert(0, str(ROOT / "ci-hub" / "ledger"))
sys.path.insert(0, str(ROOT / "scripts"))

from check_commit_attribution import AttributionError, detect_machine  # noqa: E402
from ledger_spool_backlog import producing_parents  # noqa: E402
from validate_rows import live_spool_dir  # noqa: E402

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
    subprocess.run(
        [net_wrapper, "git", "clone", "--quiet", "--reference", str(ROOT), ORIGIN,
         str(clone)],
        check=True,
        capture_output=True,
        text=True,
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


def publish_parent(parent: Path, clone: Path, *, timeout: float,
                   attribution: str) -> dict:
    spool = live_spool_dir(parent)
    result = subprocess.run(
        [
            sys.executable,
            str(ROOT / "ci-hub" / "ledger" / "publisher.py"),
            "publish",
            "--source-root", str(parent),
            "--repo", str(clone),
            "--spool-dir", str(spool),
            "--writer", str(parent / "scripts" / "parent-main-write"),
            "--attribution", attribution,
        ],
        capture_output=True,
        text=True,
        check=False,
        timeout=timeout,
    )
    report: dict = {"parent": str(parent), "rc": result.returncode}
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

    clone = ROOT / CLONE_RELATIVE
    reports: list[dict] = []
    parents = [p for p in producing_parents(ROOT) if pending_batches(live_spool_dir(p))]
    if not parents:
        print(json.dumps({"parents_with_backlog": 0, "published": 0}, sort_keys=True))
        return 0
    if args.dry_run:
        print(json.dumps(
            {"parents_with_backlog": len(parents),
             "would_publish": {str(p): pending_batches(live_spool_dir(p))
                               for p in parents}},
            sort_keys=True,
        ))
        return 0

    # Fail CLOSED on an underivable machine. Publishing under a guessed or blank
    # token would be refused by the writer anyway, and a wrong token that did
    # pass would misattribute the row.
    try:
        attribution = publishing_attribution()
    except AttributionError as error:
        print(json.dumps(
            {"error": f"cannot derive the publishing machine: {error}"},
            sort_keys=True,
        ))
        return 2

    try:
        ensure_clone(clone, net_wrapper=args.net_wrapper)
    except subprocess.CalledProcessError as error:
        print(json.dumps(
            {"error": f"cannot provision publisher clone: {error.stderr[-300:]}"},
            sort_keys=True,
        ))
        return 2

    failed = 0
    for parent in parents:
        try:
            report = publish_parent(parent, clone, timeout=args.timeout_seconds,
                                    attribution=attribution)
        except subprocess.TimeoutExpired:
            report = {"parent": str(parent), "rc": 124, "error": "publish timed out"}
        # A refusal is never fatal to the sweep: the batch stays spooled and the
        # backlog gate keeps reporting it. One stuck parent must not stop the
        # others from becoming visible.
        if report.get("rc"):
            failed += 1
        reports.append(report)

    print(json.dumps(
        {"parents_with_backlog": len(parents),
         "published": sum(r.get("published", 0) or 0 for r in reports),
         "failed_parents": failed,
         "reports": reports},
        sort_keys=True,
    ))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
