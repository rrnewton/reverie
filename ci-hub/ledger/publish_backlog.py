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
from datetime import datetime, timezone
import json
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "health"))
sys.path.insert(0, str(ROOT / "ci-hub" / "ledger"))

from ledger_spool_backlog import producing_parents  # noqa: E402
from validate_rows import live_spool_dir  # noqa: E402

#: One persistent dedicated clone, reused across runs. It is NOT any parent
#: checkout: the publisher refuses a repo equal to its source root, and refuses
#: a repo that shares no root commit with it.
CLONE_RELATIVE = Path("ignored/ci-hub/ledger-publisher-clone")
ORIGIN = "https://github.com/rrnewton/dev-hermit.git"
INCOMPLETE_EXIT = 125


def write_status(status_file: Path, report: dict) -> str:
    """Atomically replace the detached sweep handoff."""
    rendered = json.dumps(report, sort_keys=True)
    status_file.parent.mkdir(parents=True, exist_ok=True)
    temporary = status_file.with_name(f".{status_file.name}.{os.getpid()}.tmp")
    temporary.write_text(rendered + "\n")
    os.replace(temporary, status_file)
    return rendered


def emit_fields(state: str, summary: str) -> None:
    """Emit the two separate key/value lines consumed by tick-hub capture."""
    print(f"state={state}")
    print(f"summary={summary}")


def one_line(value: object, limit: int = 400) -> str:
    """Keep nested tool output from creating or overriding captured fields."""
    printable = "".join(character if character.isprintable() else " " for character in str(value))
    return " ".join(printable.split())[:limit] or "no failure detail"


def mark_started(status_file: Path) -> int:
    """Make an accepted launch distinguishable from a completed publication."""
    write_status(status_file, {
        "state": "started",
        "exit_code": INCOMPLETE_EXIT,
        "started_at": datetime.now(timezone.utc).isoformat(),
    })
    return 0


def finish(report: dict, exit_code: int, status_file: Path | None) -> int:
    """Emit one completed sweep result, optionally with an atomic handoff."""
    completed = dict(report)
    completed["state"] = "completed"
    completed["exit_code"] = exit_code
    completed["finished_at"] = datetime.now(timezone.utc).isoformat()
    rendered = json.dumps(completed, sort_keys=True)
    if status_file is not None:
        rendered = write_status(status_file, completed)
    print(rendered)
    return exit_code


def report_status(status_file: Path) -> int:
    """Report whether the last detached sweep actually completed successfully."""
    try:
        completed = json.loads(status_file.read_text())
        exit_code = completed["exit_code"]
        if not isinstance(exit_code, int) or exit_code < 0 or exit_code > 255:
            raise ValueError("exit_code is not an unsigned byte")
    except (OSError, KeyError, ValueError, json.JSONDecodeError) as error:
        emit_fields(
            "failed",
            f"ledger publish result is unreadable: {status_file}: {one_line(error)}",
        )
        return 2

    if completed.get("state") == "started":
        emit_fields(
            "failed",
            "ledger publish sweep started but did not record completion",
        )
        return INCOMPLETE_EXIT

    if exit_code == 0:
        emit_fields(
            "completed",
            "ledger publish sweep completed: "
            f"published={completed.get('published', 0)} "
            f"parents_with_backlog={completed.get('parents_with_backlog', 0)}",
        )
        return 0

    failures = [
        report for report in completed.get("reports", []) if report.get("rc")
    ]
    detail = failures[0].get("error", "no failure detail") if failures else completed.get(
        "error", "no failure detail"
    )
    emit_fields(
        "failed",
        "ledger publish sweep failed: "
        f"exit_code={exit_code} failed_parents={completed.get('failed_parents', 0)} "
        f"detail={one_line(detail)}",
    )
    return exit_code


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


def publish_parent(parent: Path, clone: Path, *, timeout: float) -> dict:
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
            "--attribution", "[impl agent, automation] [ledger-publisher, devbig014]",
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
        "--status-file",
        type=Path,
        help="atomically record the completed sweep for the detached launcher",
    )
    status_actions = parser.add_mutually_exclusive_group()
    status_actions.add_argument(
        "--report-status",
        type=Path,
        help="report and return the exit status of a previous detached sweep",
    )
    status_actions.add_argument(
        "--mark-started",
        type=Path,
        help="atomically mark a detached sweep incomplete before launching it",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what would be published without writing anything",
    )
    args = parser.parse_args(argv)

    if args.report_status is not None:
        return report_status(args.report_status)
    if args.mark_started is not None:
        return mark_started(args.mark_started)

    clone = ROOT / CLONE_RELATIVE
    reports: list[dict] = []
    parents = [p for p in producing_parents(ROOT) if pending_batches(live_spool_dir(p))]
    if not parents:
        return finish(
            {"parents_with_backlog": 0, "published": 0}, 0, args.status_file
        )
    if args.dry_run:
        return finish(
            {"parents_with_backlog": len(parents),
             "would_publish": {str(p): pending_batches(live_spool_dir(p))
                               for p in parents}}, 0, args.status_file
        )

    try:
        ensure_clone(clone, net_wrapper=args.net_wrapper)
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "no output")[-300:]
        return finish(
            {"error": f"cannot provision publisher clone: {detail}"},
            2,
            args.status_file,
        )

    failed = 0
    for parent in parents:
        try:
            report = publish_parent(parent, clone, timeout=args.timeout_seconds)
        except subprocess.TimeoutExpired:
            report = {"parent": str(parent), "rc": 124, "error": "publish timed out"}
        # A refusal is never fatal to the sweep: the batch stays spooled and the
        # backlog gate keeps reporting it. One stuck parent must not stop the
        # others from becoming visible.
        if report.get("rc"):
            failed += 1
        reports.append(report)

    exit_code = 1 if failed else 0
    return finish(
        {"parents_with_backlog": len(parents),
         "published": sum(r.get("published", 0) or 0 for r in reports),
         "failed_parents": failed,
         "reports": reports},
        exit_code,
        args.status_file,
    )


if __name__ == "__main__":
    raise SystemExit(main())
