#!/usr/bin/env python3
"""Resolve the TaskGraph database ONE way, for every ci-hub consumer.

Before this module each consumer answered "which database?" for itself, in one
of two broken ways, and the two failure modes are opposites:

* A hardcoded literal (``~/.tg/hermit.db``).  When the predecessor session shut
  down, its database froze; the unowned-backlog census went on reporting a
  fixed 108 for an entire session and could never clear.  Loud, but wrong.
* An implicit ``tg`` invocation with no database named.  ``tg`` has no global
  active database configured, so with ``TG_DB_PATH`` unset it silently falls
  back to an empty ``tasks`` database: ``tg db current`` reports
  ``(no active database - using tasks)`` and ``SELECT COUNT(*) FROM tasks``
  returns 0.  Every gate reading through it then reports a clean, empty fleet.
  Silent, and worse: **a monitoring check that goes quiet precisely because its
  data source vanished reads as health.**

So this resolver REFUSES rather than defaulting.  ``tg``'s implicit ``tasks``
fallback is never reachable through ci-hub: if nothing explicitly names a
database, that is could-not-measure, not zero.  This is the same fail-closed
contract ``ci-hub/taskgraph/unowned_backlog.py`` already honours, where a
database that cannot be read is ``state=unverifiable rc=2`` while an empty
qualifying population is a measured, clean ``0/0``.

Resolution order, most explicit first; every result carries how it was reached
so a consumer can print its own basis instead of asserting one:

1. an explicit ``--db`` argument
2. the ``TG_DB_PATH`` environment variable
3. refuse

Deliberately NOT a resolution step: a ``~/.tg/hermit.db`` compatibility
symlink.  A symlink would make stale literals work by accident and hand the
identical confusion to the next successor session; the whole point is that the
binding is stated, not inherited.  Subprocess consumers should pass
``child_env()`` so the child is bound to the same resolved path explicitly
rather than re-deriving it from whatever it happened to inherit.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

ENV_VAR = "TG_DB_PATH"

# ``tg``'s own fallback when nothing names a database.  Never resolved TO; only
# recognised so the refusal can say what it is declining to read.
IMPLICIT_FALLBACK_STEM = "tasks"


class TaskGraphUnavailable(RuntimeError):
    """No TaskGraph database could be bound, or the bound one is unreadable.

    Consumers translate this into their own could-not-measure state.  It must
    never be caught and turned into an empty result.
    """


_ROW_COUNT = re.compile(r"^\((\d+) rows?\)$")


def parse_json_rows(output: str) -> list[dict[str, object]]:
    """Parse ``tg sql`` json_object output without dropping malformed rows.

    ``tg sql`` prints a trailing row count.  That count is part of the read
    contract: without checking it, one malformed JSON row silently becomes a
    smaller population that still looks complete.
    """

    rows: list[dict[str, object]] = []
    reported: int | None = None
    for raw in output.splitlines():
        line = raw.strip()
        match = _ROW_COUNT.fullmatch(line)
        if match:
            reported = int(match.group(1))
            continue
        if not line.startswith("{"):
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            raise TaskGraphUnavailable(
                f"tg sql returned malformed JSON row: {error.msg}"
            ) from error
        if not isinstance(row, dict):
            raise TaskGraphUnavailable(
                f"tg sql returned {type(row).__name__}, expected an object"
            )
        rows.append(row)
    if reported is None:
        raise TaskGraphUnavailable("tg sql output omitted its row-count footer")
    if reported != len(rows):
        raise TaskGraphUnavailable(
            f"tg sql reported {reported} row(s) but {len(rows)} JSON row(s) were readable"
        )
    return rows


@dataclass(frozen=True)
class Resolution:
    """A bound database plus the provenance of the binding."""

    path: Path
    source: str

    @property
    def basis(self) -> str:
        return f"{self.path} (via {self.source})"


def _candidate(explicit: Path | None, env: Mapping[str, str]) -> Resolution:
    if explicit is not None:
        return Resolution(Path(explicit).expanduser(), "explicit-argument")

    raw = (env.get(ENV_VAR) or "").strip()
    if raw:
        return Resolution(Path(raw).expanduser(), ENV_VAR)

    raise TaskGraphUnavailable(
        f"no TaskGraph database bound: {ENV_VAR} is unset and no --db was given; "
        f"ci-hub refuses tg's implicit '{IMPLICIT_FALLBACK_STEM}' default because "
        "an unbound read returns 0 rows and would report an empty fleet as clean"
    )


# A TaskGraph exposes the task population as `tasks` (base table) and `tasks_v`
# (view).  Consumers read one or the other, so either proves the shape; a
# database with neither is a different database, or a corrupt one.
TASK_RELATIONS = ("tasks", "tasks_v")


def validate(path: Path) -> None:
    """Prove the path is a readable TaskGraph, or raise.

    An empty task population is fine and stays a measured zero.  A missing
    relation is not -- that is a different database entirely.
    """

    if not path.exists():
        raise TaskGraphUnavailable(f"no TaskGraph database at {path}")
    try:
        connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        try:
            marks = ",".join("?" for _ in TASK_RELATIONS)
            present = connection.execute(
                "SELECT COUNT(*) FROM sqlite_master "
                f"WHERE type IN ('table','view') AND name IN ({marks})",
                TASK_RELATIONS,
            ).fetchone()[0]
        finally:
            connection.close()
    except (OSError, sqlite3.Error) as error:
        raise TaskGraphUnavailable(f"cannot read TaskGraph at {path}: {error}") from error
    if not present:
        raise TaskGraphUnavailable(
            f"no {' or '.join(TASK_RELATIONS)} relation in {path}: not a TaskGraph database"
        )


def resolve(
    explicit: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> Resolution:
    """Bind a database and prove it is readable, or raise TaskGraphUnavailable."""

    resolution = _candidate(explicit, os.environ if env is None else env)
    validate(resolution.path)
    return resolution


def child_env(
    resolution: Resolution,
    env: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Environment binding a child process to this exact database.

    Shell consumers (``tg sql``, ``scripts/orphaned-task-detector.sh``) resolve
    through ``tg``, which reads ``TG_DB_PATH``.  Setting it explicitly here is
    what makes the child's binding stated by its caller rather than inherited
    from an ambient environment that may not carry it at all.
    """

    child = dict(os.environ if env is None else env)
    child[ENV_VAR] = str(resolution.path)
    return child


def main(argv: Sequence[str] | None = None) -> int:
    """Expose resolution to non-Python consumers and make it auditable."""

    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--db", type=Path, default=None)
    parser.add_argument(
        "--print-path",
        action="store_true",
        help="print only the resolved path, for shell consumers",
    )
    args = parser.parse_args(argv)

    try:
        resolution = resolve(args.db)
    except TaskGraphUnavailable as error:
        if args.print_path:
            print(f"taskgraph-unavailable: {error}", file=sys.stderr)
        else:
            print("state=unverifiable")
            print(f"summary={error}")
        return 2

    if args.print_path:
        print(resolution.path)
    else:
        print("state=clean")
        print(f"path={resolution.path}")
        print(f"source={resolution.source}")
        print(f"summary=TaskGraph bound: {resolution.basis}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
