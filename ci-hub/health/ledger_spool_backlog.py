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
sys.path.insert(0, str(ROOT / "ci-hub" / "series"))
from series import spool_dir as series_spool_dir  # noqa: E402

DEFAULT_MAX_AGE_SECONDS = 3600


class SpoolUnavailable(RuntimeError):
    """The backlog could not be measured; unknown is never clean."""


@dataclass(frozen=True)
class SpoolBacklog:
    depth: int
    oldest_age_seconds: float
    oldest_entry: str | None
    max_age_seconds: float
    oldest_parent: str | None = None
    parents_scanned: int = 1

    @property
    def stalled(self) -> bool:
        return self.depth > 0 and self.oldest_age_seconds > self.max_age_seconds


def producing_parents(root: Path) -> list[Path]:
    """Every parent checkout on this box that can produce ledger events.

    THE FLEET IS MULTI-PARENT AND THE SPOOL IS PER-PARENT. Scanning only this
    script's own root measures one parent and reports the others as drained,
    which is indistinguishable from healthy. Measured 2026-08-11 on devbig014:
    three producing parents existed (dev-hermit, dev-hermit4, dev-hermit5) and a
    root-scoped scan covered 1 of 3. Both real invisibility incidents that day
    lived in parents this script does not live in -- a 55-minute-old fully
    qualifying green sat unpublished and unalarmed in dev-hermit5 while the
    root-scoped gate reported its own parent as merely "pending".

    A sibling is a producing parent when it carries the ledger producer module.
    That is deliberately the producer, not the spool directory: a parent that
    has not spooled yet must still be enumerated, or the gate goes blind exactly
    when a new parent starts producing.
    """

    roots = [root]
    try:
        siblings = sorted(root.parent.iterdir())
    except OSError as exc:
        raise SpoolUnavailable(
            f"cannot enumerate producing parent checkouts under {root.parent}: {exc}"
        ) from exc
    for candidate in siblings:
        if candidate == root or not candidate.is_dir():
            continue
        if (candidate / "ci-hub" / "ledger" / "validate_rows.py").is_file():
            roots.append(candidate)
    return roots


def measure_parents(
    spool_dirs: list[Path], *, now: float | None = None, max_age_seconds: float
) -> SpoolBacklog:
    """Aggregate backlog across every parent, keeping unknown != clean.

    Depth sums; age takes the WORST parent, because one stalled parent is a
    stalled fleet for every actor that cannot read its spool. The owning parent
    travels with the alarm -- an operator who is not told which parent is stuck
    cannot act on it.
    """

    now = time.time() if now is None else now
    total_depth = 0
    worst: SpoolBacklog | None = None
    worst_parent: str | None = None
    for spool_dir in spool_dirs:
        backlog = measure(spool_dir, now=now, max_age_seconds=max_age_seconds)
        total_depth += backlog.depth
        if backlog.depth and (
            worst is None or backlog.oldest_age_seconds > worst.oldest_age_seconds
        ):
            worst = backlog
            worst_parent = str(spool_dir)
    if worst is None:
        return SpoolBacklog(
            0, 0.0, None, max_age_seconds, None, len(spool_dirs)
        )
    return SpoolBacklog(
        total_depth,
        worst.oldest_age_seconds,
        worst.oldest_entry,
        max_age_seconds,
        worst_parent,
        len(spool_dirs),
    )


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


def render(backlog: SpoolBacklog, *, store: str = "ledger") -> tuple[str, str]:
    age = round(backlog.oldest_age_seconds, 1)
    bound = round(backlog.max_age_seconds, 1)
    scope = f"parents_scanned={backlog.parents_scanned}"
    owner = f"; oldest_parent={backlog.oldest_parent}" if backlog.oldest_parent else ""
    label = "validation ledger" if store == "ledger" else "per-test series"
    if backlog.stalled:
        return (
            "stalled",
            f"{label} spool publication stalled: oldest unpublished batch age={age}s "
            f"exceeds bound={bound}s; depth={backlog.depth}; "
            f"oldest_entry={backlog.oldest_entry}{owner}; {scope}",
        )
    if backlog.depth:
        return (
            "pending",
            f"{label} spool has {backlog.depth} fresh unpublished batch(es); "
            f"oldest_age={age}s <= bound={bound}s{owner}; {scope}",
        )
    return "drained", f"{label} spool drained: depth=0; bound={bound}s; {scope}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--gate", action="store_true", help="exit 1 when backlog is too old"
    )
    parser.add_argument("--json", action="store_true", help="emit one JSON report")
    parser.add_argument(
        "--spool-dir",
        type=Path,
        default=None,
        help=(
            "measure exactly this spool directory instead of discovering every "
            "producing parent on the box (default: discover all parents)"
        ),
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(os.environ.get("DEV_HERMIT_PARENT", ROOT)),
        help="state-root parent whose producer siblings are discovered",
    )
    parser.add_argument(
        "--store",
        choices=("ledger", "series"),
        default="ledger",
        help="measure the validation ledger or per-test series spool",
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

    if args.spool_dir is not None:
        spool_dirs = [args.spool_dir]
    else:
        locate = live_spool_dir if args.store == "ledger" else series_spool_dir
        spool_dirs = [locate(parent) for parent in producing_parents(args.root.resolve())]

    try:
        backlog = measure_parents(spool_dirs, max_age_seconds=args.max_age_seconds)
    except SpoolUnavailable as exc:
        label = "validation ledger" if args.store == "ledger" else "per-test series"
        print("state=unknown")
        print(f"summary={label} spool backlog unavailable: {exc}")
        return 2

    state, summary = render(backlog, store=args.store)
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
