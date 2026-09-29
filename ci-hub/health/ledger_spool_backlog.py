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

``--store series-quarantine`` measures the refused series inputs. A validate
run that appends its valid rows exits 0 even when it quarantined others, so a
quarantine must raise an alarm somewhere else: this gate fails while any
refusal was observed within ``--recent-seconds`` (read from this host's
quarantine files and from the records published in the test ledger), while
``held/`` keeps exact records whose content could not be published, while
``corrupt/`` keeps any file the publisher could not read (counted as a file,
never parsed), and when unpublished refusals are older than the backlog bound.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import os
import stat
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LEDGER_LIB = ROOT / "ci-hub" / "ledger"
sys.path.insert(0, str(LEDGER_LIB))
from validate_rows import live_spool_dir  # noqa: E402
sys.path.insert(0, str(ROOT / "ci-hub" / "series"))
import series  # noqa: E402
from series import spool_dir as series_spool_dir  # noqa: E402

DEFAULT_MAX_AGE_SECONDS = 3600
#: How long one observed refusal keeps the quarantine gate failing.
DEFAULT_QUARANTINE_RECENT_SECONDS = 86400
STORES = ("ledger", "series", "series-quarantine")
STORE_LABELS = {
    "ledger": "validation ledger",
    "series": "per-test series",
    "series-quarantine": "series quarantine",
}


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


@dataclass(frozen=True)
class QuarantineState:
    """Refusals observed recently and exact records withheld from publication."""

    recent_records: int
    held_records: int
    recent_seconds: float
    #: Parents whose test ledger has no ``refs/remotes/origin/main`` to read.
    ledger_unread: int = 0
    recent_parent: str | None = None
    held_parent: str | None = None
    #: Local files the publisher set aside in ``corrupt/`` because they could
    #: not be read. Their records cannot be counted, so the files are.
    corrupt_files: int = 0


def _emitted_epoch(value: object) -> float | None:
    if not isinstance(value, str):
        return None
    try:
        moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=timezone.utc)
    return moment.timestamp()


def _published_quarantine(parent: Path) -> list[dict] | None:
    """Records published in this parent's test ledger; None when unreadable.

    ``None`` means there is no ledger checkout or no fetched main to read, which
    is reported as ``ledger_unread``. A ledger that exists but holds invalid
    quarantine records is unavailable, never clean.
    """
    try:
        ledger = series.ledger_root(parent)
    except series.SeriesError:
        return None
    ref = subprocess.run(
        ["git", "-C", str(ledger), "rev-parse", "--verify", "--quiet",
         "refs/remotes/origin/main^{commit}"],
        capture_output=True, text=True, check=False,
    )
    if ref.returncode != 0:
        return None
    try:
        return series.published_quarantine_at_repo(ledger, "refs/remotes/origin/main")[1]
    except (series.LedgerError, OSError, subprocess.SubprocessError) as exc:
        raise SpoolUnavailable(f"cannot read published quarantine in {ledger}: {exc}") from exc


def measure_quarantine(
    parents: list[Path], *, now: float | None = None, recent_seconds: float,
) -> QuarantineState:
    """Count distinct refusals seen within the window and records held back.

    A local file's mtime is when this host last appended a refusal to it;
    a published record's ``emitted_at`` is when its run observed it. Both are
    read because a published file is reaped from this host once the ledger
    holds it. An unparseable ``emitted_at`` counts as recent: unknown is never
    clean.
    """
    now = time.time() if now is None else now
    since = now - recent_seconds
    recent: set = set()
    held: set = set()
    unread = 0
    recent_parent = held_parent = None
    corrupt = 0
    for parent in parents:
        directory = series.quarantine_dir(parent)
        held_dir = directory / series.QUARANTINE_HELD_SUBDIR
        corrupt_dir = directory / series.QUARANTINE_CORRUPT_SUBDIR
        try:
            set_aside = sorted(corrupt_dir.glob("*.jsonl")) if corrupt_dir.is_dir() else []
        except OSError as exc:
            raise SpoolUnavailable(f"cannot list {corrupt_dir}: {exc}") from exc
        if set_aside:
            corrupt += len(set_aside)
            held_parent = held_parent or str(parent)
        for folder in (directory, directory / "published", held_dir):
            try:
                paths = sorted(folder.glob("*.jsonl"))
                for path in paths:
                    mtime = path.stat().st_mtime
                    identities = {series.quarantine_identity(record)
                                  for record in series._read_quarantine_file(path)}
                    if folder == held_dir and identities - held:
                        held |= identities
                        held_parent = held_parent or str(parent)
                    if mtime >= since and identities - recent:
                        recent |= identities
                        recent_parent = recent_parent or str(parent)
            except FileNotFoundError:
                # A concurrent publisher moved or reaped the file after listing.
                continue
            except (OSError, series.LedgerError) as exc:
                raise SpoolUnavailable(f"cannot read quarantine files in {folder}: {exc}") from exc
        published = _published_quarantine(parent)
        if published is None:
            unread += 1
            continue
        for record in published:
            emitted = _emitted_epoch(record.get("emitted_at"))
            if (emitted is None or emitted >= since) and record["event_id"] not in recent:
                recent.add(record["event_id"])
                recent_parent = recent_parent or str(parent)
    return QuarantineState(len(recent), len(held), recent_seconds, unread,
                           recent_parent, held_parent, corrupt)


def render_quarantine(
    backlog: SpoolBacklog, quarantine: QuarantineState,
) -> tuple[str, str]:
    state, summary = render(backlog, store="series-quarantine")
    window = round(quarantine.recent_seconds, 1)
    counts = (f"recent_records={quarantine.recent_records} within {window}s; "
              f"held_records={quarantine.held_records}; "
              f"corrupt_files={quarantine.corrupt_files}; "
              f"ledger_unread_parents={quarantine.ledger_unread}")
    if state == "stalled":
        return state, f"{summary}; {counts}"
    if quarantine.held_records or quarantine.corrupt_files:
        return ("withheld",
                f"series quarantine holds {quarantine.held_records} exact record(s) whose "
                f"content could not be published portably and {quarantine.corrupt_files} "
                f"unreadable file(s) set aside in corrupt/, in {quarantine.held_parent}; "
                f"{counts}")
    if quarantine.recent_records:
        return ("quarantined",
                f"series writer refused {quarantine.recent_records} input record(s) within "
                f"{window}s (first seen in {quarantine.recent_parent}); {counts}")
    return state, f"{summary}; {counts}"


def render(backlog: SpoolBacklog, *, store: str = "ledger") -> tuple[str, str]:
    age = round(backlog.oldest_age_seconds, 1)
    bound = round(backlog.max_age_seconds, 1)
    scope = f"parents_scanned={backlog.parents_scanned}"
    owner = f"; oldest_parent={backlog.oldest_parent}" if backlog.oldest_parent else ""
    label = STORE_LABELS[store]
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
        "--gate", action="store_true",
        help="exit 1 when backlog is too old (series-quarantine: also when a "
             "refusal is recent, an exact record is held or an unreadable file is set aside)",
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
        choices=STORES,
        default="ledger",
        help="measure the validation ledger, per-test series or series quarantine spool",
    )
    parser.add_argument(
        "--recent-seconds",
        type=float,
        default=DEFAULT_QUARANTINE_RECENT_SECONDS,
        help=("series-quarantine only: how long one refusal keeps the gate failing "
              f"(default: {DEFAULT_QUARANTINE_RECENT_SECONDS}s)"),
    )
    parser.add_argument(
        "--max-age-seconds",
        type=float,
        default=DEFAULT_MAX_AGE_SECONDS,
        help=f"oldest allowed unpublished age (default: {DEFAULT_MAX_AGE_SECONDS}s)",
    )
    args = parser.parse_args(argv)
    if args.max_age_seconds < 0 or args.recent_seconds < 0:
        parser.error("--max-age-seconds and --recent-seconds must be non-negative")
    if args.store == "series-quarantine" and args.spool_dir is not None:
        parser.error("--store series-quarantine discovers parents; --spool-dir is not supported")

    parents: list[Path] = []
    if args.spool_dir is not None:
        spool_dirs = [args.spool_dir]
    else:
        locate = {"ledger": live_spool_dir, "series": series_spool_dir,
                  "series-quarantine": series.quarantine_dir}[args.store]
        parents = producing_parents(args.root.resolve())
        spool_dirs = [locate(parent) for parent in parents]

    label = STORE_LABELS[args.store]
    quarantine = None
    try:
        backlog = measure_parents(spool_dirs, max_age_seconds=args.max_age_seconds)
        if args.store == "series-quarantine":
            quarantine = measure_quarantine(parents, recent_seconds=args.recent_seconds)
    except SpoolUnavailable as exc:
        print("state=unknown")
        print(f"summary={label} spool backlog unavailable: {exc}")
        return 2

    if quarantine is None:
        state, summary = render(backlog, store=args.store)
    else:
        state, summary = render_quarantine(backlog, quarantine)
    report = {
        "state": state,
        "summary": summary,
        "spool_depth": backlog.depth,
        "oldest_age_seconds": round(backlog.oldest_age_seconds, 1),
        "max_age_seconds": round(backlog.max_age_seconds, 1),
        "oldest_entry": backlog.oldest_entry or "none",
    }
    if quarantine is not None:
        report.update({
            "quarantine_recent_records": quarantine.recent_records,
            "quarantine_held_records": quarantine.held_records,
            "quarantine_corrupt_files": quarantine.corrupt_files,
            "quarantine_recent_seconds": round(quarantine.recent_seconds, 1),
            "quarantine_ledger_unread_parents": quarantine.ledger_unread,
        })
    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        for key, value in report.items():
            print(f"{key}={value}")
    failing = backlog.stalled or state in {"withheld", "quarantined"}
    return 1 if args.gate and failing else 0


if __name__ == "__main__":
    raise SystemExit(main())
