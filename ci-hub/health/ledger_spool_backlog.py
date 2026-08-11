#!/usr/bin/env python3
"""Alarm when validation-ledger events remain unpublished too long.

The ledger producer already durably records every event as one immutable
``*.jsonl`` spool batch before publication.  The missing measurement was the
age of that standing backlog: a stalled publisher and an unused publisher both
looked like "zero newly published rows".

This detector is deliberately read-only.  It takes one directory snapshot,
counts complete batches, and uses their mtimes as time spent unpublished.
Event ``emitted_at`` is the wrong clock: a historical event can be imported
today and is not evidence that today's publisher has been stuck since the
historical run.  In-flight ``*.tmp`` batches are ignored because the producer
has not atomically admitted them to the publishable spool yet.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import sys
import time
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LEDGER_LIB = ROOT / "ci-hub" / "ledger"
sys.path.insert(0, str(LEDGER_LIB))
from validate_rows import live_spool_dir  # noqa: E402

DEFAULT_MAX_AGE_SECONDS = 3600


class SpoolUnavailable(RuntimeError):
    """The backlog could not be measured; unknown is never clean."""


@dataclass(frozen=True)
class SpoolBacklog:
    depth: int
    oldest_age_seconds: float
    oldest_entry: str | None
    max_age_seconds: float

    @property
    def stalled(self) -> bool:
        return self.depth > 0 and self.oldest_age_seconds > self.max_age_seconds


def measure(
    spool_dir: Path, *, now: float | None = None, max_age_seconds: float
) -> SpoolBacklog:
    """Take one cheap, race-tolerant snapshot of complete spool batches."""

    now = time.time() if now is None else now
    try:
        entries = list(os.scandir(spool_dir))
    except FileNotFoundError:
        entries = []
    except OSError as exc:
        raise SpoolUnavailable(f"cannot list ledger spool {spool_dir}: {exc}") from exc

    completed: list[tuple[str, float]] = []
    for entry in entries:
        if not entry.name.endswith(".jsonl"):
            continue
        try:
            metadata = entry.stat(follow_symlinks=False)
        except FileNotFoundError:
            # A concurrent publisher proved and drained this exact immutable
            # batch after the listing.  It is no longer backlog.
            continue
        except OSError as exc:
            raise SpoolUnavailable(
                f"cannot stat spool batch {entry.path}: {exc}"
            ) from exc
        if not stat.S_ISREG(metadata.st_mode):
            raise SpoolUnavailable(f"spool batch is not a regular file: {entry.path}")
        completed.append((entry.name, metadata.st_mtime))

    if not completed:
        return SpoolBacklog(0, 0.0, None, max_age_seconds)
    oldest_name, oldest_mtime = min(completed, key=lambda item: item[1])
    return SpoolBacklog(
        len(completed),
        max(0.0, now - oldest_mtime),
        oldest_name,
        max_age_seconds,
    )


def render(backlog: SpoolBacklog) -> tuple[str, str]:
    age = round(backlog.oldest_age_seconds, 1)
    bound = round(backlog.max_age_seconds, 1)
    if backlog.stalled:
        return (
            "stalled",
            f"ledger spool publication stalled: oldest unpublished batch age={age}s "
            f"exceeds bound={bound}s; depth={backlog.depth}; "
            f"oldest_entry={backlog.oldest_entry}",
        )
    if backlog.depth:
        return (
            "pending",
            f"ledger spool has {backlog.depth} fresh unpublished batch(es); "
            f"oldest_age={age}s <= bound={bound}s",
        )
    return "drained", f"ledger spool drained: depth=0; bound={bound}s"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--gate", action="store_true", help="exit 1 when backlog is too old"
    )
    parser.add_argument("--json", action="store_true", help="emit one JSON report")
    parser.add_argument(
        "--spool-dir",
        type=Path,
        default=live_spool_dir(ROOT),
        help="spool directory (default: canonical live validation-ledger spool)",
    )
    parser.add_argument(
        "--max-age-seconds",
        type=float,
        default=DEFAULT_MAX_AGE_SECONDS,
        help=f"oldest allowed unpublished age (default: {DEFAULT_MAX_AGE_SECONDS}s)",
    )
    args = parser.parse_args(argv)
    if args.max_age_seconds < 0:
        parser.error("--max-age-seconds must be non-negative")

    try:
        backlog = measure(args.spool_dir, max_age_seconds=args.max_age_seconds)
    except SpoolUnavailable as exc:
        print("state=unknown")
        print(f"summary=ledger spool backlog unavailable: {exc}")
        return 2

    state, summary = render(backlog)
    report = {
        "state": state,
        "summary": summary,
        "spool_depth": backlog.depth,
        "oldest_age_seconds": round(backlog.oldest_age_seconds, 1),
        "max_age_seconds": round(backlog.max_age_seconds, 1),
        "oldest_entry": backlog.oldest_entry or "none",
    }
    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        for key, value in report.items():
            print(f"{key}={value}")
    return 1 if args.gate and backlog.stalled else 0


if __name__ == "__main__":
    raise SystemExit(main())
