#!/usr/bin/env python3
"""Publish an append-only observation history and a deduplicated current view.

The history is the measurement ledger.  The current scorecard is a projection:
the last physical observation for each ``(test_id, backend)`` cell.  Publishing
the same run twice is idempotent; reusing a run id for different bytes refuses.

``stack_parity`` and ``heap_parity`` are optional, three-valued run modes.  An
initial migration leaves them blank because historical rows do not say whether
the flags were passed. A newly published observation must explicitly carry
``0`` when the flag was omitted and ``1`` when it was passed. Missing/blank mode
on a new observation refuses publication; verdicts live in outcome columns.
"""

from __future__ import annotations

import argparse
import csv
import fcntl
import json
import os
import sys
import tempfile
from pathlib import Path

OPTIONAL_PARITY_COLUMNS = ("stack_parity", "heap_parity")
CELL_KEY = ("test_id", "backend")
SCHEMA = Path(__file__).with_name("scorecard-schema.json")


class PublishError(Exception):
    pass


def read_csv(path: Path) -> tuple[list[str], list[dict[str, str]]]:
    try:
        with path.open(newline="", encoding="utf-8") as handle:
            reader = csv.DictReader(handle)
            if reader.fieldnames is None:
                raise PublishError(f"{path} has no CSV header")
            rows = list(reader)
    except OSError as error:
        raise PublishError(f"cannot read {path}: {error}") from error
    if any(None in row for row in rows):
        raise PublishError(f"{path} contains an over-wide row")
    missing = [column for column in CELL_KEY if column not in reader.fieldnames]
    if missing:
        raise PublishError(f"{path} is missing cell key column(s): {', '.join(missing)}")
    return list(reader.fieldnames), rows


def shared_header() -> list[str]:
    try:
        document = json.loads(SCHEMA.read_text(encoding="utf-8"))
        header = document["variants"]["scorecard.csv"]["columns"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise PublishError(f"cannot read shared scorecard row from {SCHEMA}: {error}") from error
    if not isinstance(header, list) or not header or not all(
        isinstance(column, str) and column for column in header
    ):
        raise PublishError(f"{SCHEMA}: scorecard.csv columns are malformed")
    if len(header) != len(set(header)):
        raise PublishError(f"{SCHEMA}: scorecard.csv contains duplicate columns")
    return header


def require_shared_header(path: Path, actual: list[str], expected: list[str]) -> None:
    if actual != expected:
        raise PublishError(
            f"{path} header differs from the shared scorecard row; "
            "run compat-envelope/check-scorecard-schema.rs"
        )


def normalise_rows(
    rows: list[dict[str, str]], header: list[str], *, historical: bool
) -> list[dict[str, str]]:
    out = []
    for line, source in enumerate(rows, start=2):
        row = {column: source.get(column, "") or "" for column in header}
        for column in OPTIONAL_PARITY_COLUMNS:
            value = row[column].strip().lower()
            if historical and not value:
                row[column] = ""
            elif value in ("1", "true"):
                row[column] = "1"
            elif value in ("0", "false"):
                row[column] = "0"
            elif not value:
                raise PublishError(
                    f"row {line} has blank {column} in a new observation; "
                    "new producers must emit 0 when the flag was omitted and "
                    "1 when it was passed"
                )
            else:
                raise PublishError(
                    f"row {line} has invalid {column}={row[column]!r}; "
                    "expected historical blank, 0/false, or 1/true"
                )
        out.append(row)
    return out


def current_rows(rows: list[dict[str, str]]) -> list[dict[str, str]]:
    newest: dict[tuple[str, str], int] = {}
    for index, row in enumerate(rows):
        key = tuple((row.get(column) or "").strip() for column in CELL_KEY)
        if not all(key):
            raise PublishError(f"history row {index + 2} has blank cell key {key!r}")
        newest[key] = index
    return [row for index, row in enumerate(rows) if newest[tuple(row[c].strip() for c in CELL_KEY)] == index]


def atomic_write(path: Path, header: list[str], rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", newline="", encoding="utf-8", dir=path.parent,
        prefix=f".{path.name}.", delete=False,
    ) as handle:
        temporary = Path(handle.name)
        writer = csv.DictWriter(handle, fieldnames=header, lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
        handle.flush()
        os.fsync(handle.fileno())
    try:
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def initialise(current: Path, history: Path) -> tuple[int, int]:
    if history.exists():
        raise PublishError(f"history already exists: {history}")
    old_header, old_rows = read_csv(current)
    header = shared_header()
    unknown = [column for column in old_header if column not in header]
    if unknown:
        raise PublishError(f"{current} has unknown columns: {', '.join(unknown)}")
    history_rows = normalise_rows(old_rows, header, historical=True)
    projected = current_rows(history_rows)
    atomic_write(history, header, history_rows)
    atomic_write(current, header, projected)
    return len(history_rows), len(projected)


def upgrade_only(path: Path) -> int:
    """Widen one historical scorecard without inventing either run mode."""
    old_header, rows = read_csv(path)
    header = shared_header()
    unknown = [column for column in old_header if column not in header]
    if unknown:
        raise PublishError(f"{path} has unknown columns: {', '.join(unknown)}")
    rows = normalise_rows(rows, header, historical=True)
    atomic_write(path, header, rows)
    return len(rows)


def publish(observation: Path, current: Path, history: Path) -> tuple[int, int, bool]:
    history_header, history_rows_raw = read_csv(history)
    header = shared_header()
    require_shared_header(history, history_header, header)
    history_rows = normalise_rows(history_rows_raw, header, historical=True)
    source_header, source_rows_raw = read_csv(observation)
    unknown = [column for column in source_header if column not in header]
    if unknown:
        raise PublishError(
            f"observation {observation} has column(s) outside the shared scorecard row: "
            f"{', '.join(unknown)}"
        )
    if len(source_header) != len(set(source_header)):
        raise PublishError(f"observation {observation} has duplicate columns")
    source_rows = normalise_rows(source_rows_raw, header, historical=False)
    if not source_rows:
        raise PublishError(f"observation {observation} has no rows")
    run_ids = {(row.get("run_id") or "").strip() for row in source_rows}
    if "" in run_ids or len(run_ids) != 1:
        raise PublishError(
            f"observation must carry exactly one nonblank run_id; found {sorted(run_ids)!r}"
        )
    run_id = next(iter(run_ids))
    prior = [row for row in history_rows if (row.get("run_id") or "").strip() == run_id]
    if prior:
        if prior != source_rows:
            raise PublishError(f"run_id {run_id!r} already exists with different rows")
        changed = False
    else:
        history_rows.extend(source_rows)
        changed = True
        # History first: if publication is interrupted before the current view,
        # an idempotent retry reconstructs the view from the durable ledger.
        atomic_write(history, header, history_rows)
    projected = current_rows(history_rows)
    atomic_write(current, header, projected)
    return len(history_rows), len(projected), changed


def check_projection(current: Path, history: Path) -> tuple[int, int]:
    history_header, history_rows_raw = read_csv(history)
    header = shared_header()
    require_shared_header(history, history_header, header)
    history_rows = normalise_rows(history_rows_raw, header, historical=True)
    current_header, actual = read_csv(current)
    require_shared_header(current, current_header, header)
    expected = current_rows(history_rows)
    if actual != expected:
        raise PublishError(
            f"current view is not newest-per-(test_id, backend): "
            f"actual={len(actual)} expected={len(expected)}"
        )
    return len(history_rows), len(actual)


def green_counts(rows: list[dict[str, str]]) -> tuple[int, int]:
    """Apply the strict-green standard without letting tier labels self-certify."""
    qualifying = {
        "full-stdout-info-stack-heap",
        "stdout-info-stack-heap-spot-check",
    }

    def true(value: str | None) -> bool:
        return (value or "").strip().lower() in {"1", "true"}

    def nonempty_comparison(value: str | None) -> bool:
        value = (value or "").strip().lower()
        return value not in {"", "-", "n/a", "none", "null", "0", "0|0", "0/0"}

    base = [
        row
        for row in rows
        if (row.get("outcome") or "").strip() == "pass"
        and (row.get("comparison_tier") or "").strip() in qualifying
        and true(row.get("stdout_parity"))
        and nonempty_comparison(row.get("compared_log_messages"))
    ]
    highest = [
        row
        for row in base
        if true(row.get("stack_parity")) and true(row.get("heap_parity"))
    ]
    return len(base), len(highest)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current", required=True, type=Path)
    parser.add_argument("--history", required=True, type=Path)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--initialize", action="store_true")
    action.add_argument("--observation", type=Path)
    action.add_argument("--upgrade-only", action="store_true")
    action.add_argument("--check", action="store_true")
    args = parser.parse_args()

    lock = args.current.parent / "ignored" / "scorecard-publish.lock"
    lock.parent.mkdir(parents=True, exist_ok=True)
    try:
        with lock.open("a+", encoding="utf-8") as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
            if args.check:
                historical, current = check_projection(args.current, args.history)
                changed = False
            elif args.upgrade_only:
                historical = current = upgrade_only(args.current)
                changed = True
            elif args.initialize:
                historical, current = initialise(args.current, args.history)
                changed = True
            else:
                historical, current, changed = publish(
                    args.observation, args.current, args.history
                )
    except PublishError as error:
        print(f"REFUSED: {error}", file=sys.stderr)
        return 2

    superseded = historical - current
    state = "published" if changed else "idempotent-no-op"
    print(
        f"{state}: {historical} stored historical rows / {current} current "
        f"(test_id, backend) cells / {superseded} superseded rows. "
        "This is a counting correction: neither progress nor regression."
    )
    _, selected = read_csv(args.current)
    base, highest = green_counts(selected)
    high_suffix = " (no base-green population)" if base == 0 else ""
    print(
        f"base green: {base}/{len(selected)} current cells; highest-grade green: "
        f"{highest}/{base} base-green cells{high_suffix}."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
