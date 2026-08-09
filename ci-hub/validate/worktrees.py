#!/usr/bin/env python3
"""Consume the Rust validation driver's hub-report artifacts.

`hermit/scripts/validate.rs` (as of the smart-selection / worktree-registration work)
walks up from its checkout root, and when it finds this hub it reports every run
into two gitignored artifacts under the parent `ignored/ci-hub/` store:

  1. `validate-runs.jsonl` -- append-only, one schema_version>=3 record per run
     (byte-identical to the parent validate-run ledger line). Runs are events,
     never deduplicated.
  2. `worktree-registry.json` -- an object keyed by absolute worktree path, one
     entry per worktree, upserted idempotently on every run. Each entry keeps
     `first_seen` and refreshes `last_seen`/`last_commit`/`last_result`/
     `last_profile`/`last_selection_mode`.

Without a consumer those artifacts are write-only: they accumulate but nothing
surfaces them. This tool is that consumer -- it renders which worktrees exist,
how fresh each is, and (optionally) the most recent runs. It is strictly
read-only.

The producer fails closed if the hub or its dependencies are unavailable. A
standalone product checkout may opt out explicitly, but prints a loud warning.
This reader still tolerates a missing or partial historical artifact and reports
exactly what is present.

Usage:
  ci-hub/validate/worktrees.py                 # registered-worktree table
  ci-hub/validate/worktrees.py --runs 10       # + 10 most-recent runs
  ci-hub/validate/worktrees.py --json          # machine-readable report
  ci-hub/validate/worktrees.py --stale-hours 6 # flag worktrees unseen >6h
  ci-hub/validate/worktrees.py --data-dir DIR  # override ignored/ci-hub
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sys
import time
from typing import Any


def default_data_dir() -> str:
    """Locate parent `ignored/ci-hub` (this lives in `ci-hub/validate`)."""
    env = os.environ.get("CI_HUB_IGNORED_DIR")
    if env:
        return os.path.abspath(env)
    parent = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    return os.path.join(parent, "ignored", "ci-hub")


def load_registry(path: str) -> list[dict[str, Any]]:
    """Return registry entries newest-last; tolerate missing/corrupt files."""
    if not os.path.isfile(path):
        return []
    try:
        with open(path, encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return []
    if not isinstance(data, dict):
        return []
    entries = [value for value in data.values() if isinstance(value, dict)]
    entries.sort(key=lambda entry: entry.get("last_seen_epoch") or 0)
    return entries


def load_runs(path: str, limit: int) -> list[dict[str, Any]]:
    """Return the newest `limit` run records, oldest-first, skipping bad lines."""
    if limit <= 0 or not os.path.isfile(path):
        return []
    records: list[dict[str, Any]] = []
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    record = json.loads(line)
                except ValueError:
                    continue
                if isinstance(record, dict):
                    records.append(record)
    except OSError:
        return []
    return records[-limit:]


def humanize_age(epoch: Any, now: float) -> str:
    """Render an age like `12m`, `3h`, or `2d` from an epoch second value."""
    try:
        delta = now - float(epoch)
    except (TypeError, ValueError):
        return "?"
    if delta < 0:
        delta = 0.0
    if delta < 90:
        return f"{int(delta)}s"
    if delta < 90 * 60:
        return f"{int(delta / 60)}m"
    if delta < 36 * 3600:
        return f"{int(delta / 3600)}h"
    return f"{int(delta / 86400)}d"


def is_stale(epoch: Any, now: float, stale_seconds: float) -> bool:
    try:
        return (now - float(epoch)) > stale_seconds
    except (TypeError, ValueError):
        return False


def short_sha(value: Any) -> str:
    text = str(value or "")
    return text[:12] if text and text != "unknown" else (text or "-")


def render_table(rows: list[list[str]], headers: list[str]) -> str:
    widths = [len(header) for header in headers]
    for row in rows:
        for index, cell in enumerate(row):
            widths[index] = max(widths[index], len(cell))
    lines = ["  ".join(header.ljust(widths[i]) for i, header in enumerate(headers)).rstrip()]
    for row in rows:
        lines.append("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)).rstrip())
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Consume Rust validate hub-report artifacts (read-only).",
    )
    parser.add_argument(
        "--data-dir",
        default=default_data_dir(),
        help="Directory holding the hub artifacts (default: parent ignored/ci-hub).",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=0,
        help="Also show this many most-recent hub-reported runs (default: 0).",
    )
    parser.add_argument(
        "--stale-hours",
        type=float,
        default=24.0,
        help="Flag a worktree stale when unseen for longer than this (default: 24).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit a machine-readable report instead of tables.",
    )
    args = parser.parse_args(argv)

    registry_path = os.path.join(args.data_dir, "worktree-registry.json")
    runs_path = os.path.join(args.data_dir, "validate-runs.jsonl")
    now = time.time()
    stale_seconds = args.stale_hours * 3600.0

    entries = load_registry(registry_path)
    runs = load_runs(runs_path, args.runs)

    for entry in entries:
        entry["_stale"] = is_stale(entry.get("last_seen_epoch"), now, stale_seconds)

    if args.json:
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc)
            .replace(microsecond=0)
            .isoformat()
            .replace("+00:00", "Z"),
            "data_dir": os.path.abspath(args.data_dir),
            "stale_hours": args.stale_hours,
            "worktree_count": len(entries),
            "stale_count": sum(1 for entry in entries if entry.get("_stale")),
            "worktrees": entries,
            "runs": runs,
        }
        print(json.dumps(report, indent=2))
        return 0

    if not entries:
        print(
            f"no worktrees registered yet (looked in {os.path.abspath(args.data_dir)});"
            " a plain ./scripts/validate.rs under this hub registers one."
        )
    else:
        rows = []
        for entry in entries:
            marker = "STALE" if entry.get("_stale") else ""
            rows.append(
                [
                    str(entry.get("slot") or "-"),
                    str(entry.get("branch") or "-"),
                    str(entry.get("state") or "-"),
                    str(entry.get("last_result") or "-"),
                    str(entry.get("last_profile") or "-"),
                    str(entry.get("last_selection_mode") or "-"),
                    "dirty" if entry.get("tree_dirty") else "clean",
                    "yes" if entry.get("commit_anchored") else "no",
                    humanize_age(entry.get("last_seen_epoch"), now),
                    marker,
                    short_sha(entry.get("last_commit")),
                    str(entry.get("path") or "-"),
                ]
            )
        headers = [
            "SLOT",
            "BRANCH",
            "STATE",
            "RESULT",
            "PROFILE",
            "SEL",
            "TREE",
            "ANCHORED",
            "AGE",
            "",
            "COMMIT",
            "PATH",
        ]
        stale = sum(1 for entry in entries if entry.get("_stale"))
        print(
            f"{len(entries)} registered worktree(s), {stale} stale "
            f"(>{args.stale_hours:g}h); newest last:"
        )
        print(render_table(rows, headers))

    if args.runs > 0:
        print()
        if not runs:
            print(f"no hub-reported runs yet (looked in {os.path.abspath(runs_path)}).")
        else:
            rows = []
            for record in runs:
                checks = record.get("checks")
                failures = record.get("failures")
                counts = "-"
                if checks is not None or failures is not None:
                    counts = f"{failures or 0}/{checks or 0}"
                wall = record.get("real_seconds")
                rows.append(
                    [
                        str(record.get("finished_at") or "-"),
                        str(record.get("slot") or "-"),
                        str(record.get("profile") or "-"),
                        str(record.get("selection_mode") or "-"),
                        str(record.get("result") or "-"),
                        counts,
                        str(wall) if wall is not None else "-",
                        short_sha(record.get("commit")),
                    ]
                )
            headers = [
                "FINISHED",
                "SLOT",
                "PROFILE",
                "SEL",
                "RESULT",
                "FAIL/CHK",
                "WALL(s)",
                "COMMIT",
            ]
            print(f"{len(runs)} most-recent hub-reported run(s), newest last:")
            print(render_table(rows, headers))

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
