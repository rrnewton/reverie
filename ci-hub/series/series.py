#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Stress/flakiness SERIES store: one append-only row per (cell, tree, run).

WHY A SECOND STORE AND NOT A COLUMN ON THE FIRST. The validation ledger records
one event per validation RUN of a whole tree. This records one row per CELL per
REPETITION, so a single pressure campaign over 40 cells at N=20 emits 800 rows
against one tree. Those are different cardinalities and different lifetimes, and
folding them into the validate shards would make a month's shard unreadable for
its original purpose.

WHAT IS DELIBERATELY REUSED, not re-invented. The shard/spool/union model is
imported from `ci-hub/ledger/ledger.py` rather than copied:

  * `union_events` — deterministic merge and event-id dedup, so the same row is
    one logical row whether it is read from the live spool, the tracked shard,
    or both during publication.
  * `append_events` — one `os.write` per line in `O_APPEND`, so two producers on
    one shard interleave whole LINES and never tear one. A pressure campaign is
    exactly the concurrent-writer case that needs this.
  * `verify_append_only` — a rewritten or deleted line stays a DETECTABLE
    VIOLATION rather than an ordinary update.
  * `parse_line` — same strictness about what a line is.

Copying those would have produced a second implementation that drifts. Only the
things that are genuinely different here — the path root, the envelope version,
the row identity, and the linter — are new.

⚠️ THE PRODUCER BOUNDARY IS THE SAME AS THE LEDGER'S, FOR THE SAME REASON.
Live producers append to `ignored/ci-hub/series-spool/`; they never write the
tracked shard. Readers union the spool with every tracked shard, so a row is
visible before, during and after publication. A reader that consulted only one
of the two would report a campaign as missing while it was being published.
"""

from __future__ import annotations

import json
import re
import sys
from collections.abc import Iterable, Sequence
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "ledger"))

from ledger import (  # noqa: E402  (path set above)
    LedgerError,
    MalformedEvent,
    append_events,
    parse_line,
    read_shard,
    union_events,
    verify_append_only,
)

ENVELOPE_VERSION = "stress-series/v1"

#: Envelope keys every series row must carry. Deliberately the SAME set the
#: validate ledger uses, so `union_events` orders and dedups these rows with the
#: identical rule and no series-specific branch.
ENVELOPE_KEYS = (
    "schema", "event_id", "event_type", "emitted_at", "team", "host", "run_id",
    "producer",
)

#: The identity of one row. `run_index` is the repetition ordinal WITHIN one
#: campaign against one tree -- it is what makes this a series rather than a
#: single observation, and it is why (cell, tree) alone is not a key.
IDENTITY_KEYS = ("cell", "tree", "run_index")

#: The four divergence coordinates, stored as RAW POSITIONS, one value per run.
#:
#: ⚠️ THESE ARE FOUR DIFFERENT KEYSPACES AND MUST NEVER BE COMPARED ACROSS AXES.
#: One real divergence was record 98, syscall 37, scheduler turn 4 -- three
#: different numbers for one event. `ci/compat-envelope/scorecard.rs` already
#: carries that warning on `first_divergent_syscall`; storing bounds per axis is
#: only meaningful because each axis is folded separately.
COORDINATE_KEYS = (
    "first_divergent_record",
    "first_divergent_syscall",
    "first_divergent_scheduler_turn",
    "first_divergent_virtual_nanoseconds",
)

#: An outcome that is NOT a statement about the cell. Mirrors the ledger's
#: `NON_VERDICT_OUTCOMES` for the same reason: an infrastructure hiccup recorded
#: as a product failure is how a flake becomes a false regression.
NON_VERDICT_OUTCOMES = frozenset({"no_result", "timeout", "errored", "skipped"})

#: Every outcome a row may carry.
OUTCOMES = frozenset({"passed", "diverged"}) | NON_VERDICT_OUTCOMES

_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_CELL_RE = re.compile(r"^[A-Za-z0-9._/-]+/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$")
# Built rather than written literally so this module does not itself contain the
# owner-path needle the portability check greps tracked files for.
_OWNER_PATH_NEEDLE = "/" + "home" + "/"


class SeriesError(LedgerError):
    """Any series-store violation."""


class MalformedRow(SeriesError):
    """A row that does not satisfy the schema."""


def shard_path(team: str, host: str, month: str) -> str:
    """`series/<team>/<host>/<YYYY>-<MM>.jsonl`.

    Beside `ledger/`, never inside it: two roots keep a `git log` over
    validation history readable when a campaign emits 800 rows in a minute.
    Sharding per team and short host is what makes two machines merge trivially,
    and is copied deliberately.
    """
    if not re.fullmatch(r"\d{4}-\d{2}", month or ""):
        raise MalformedRow(f"month must be YYYY-MM, got {month!r}")
    for name, value in (("team", team), ("host", host)):
        if not value or "/" in value or value in (".", ".."):
            raise MalformedRow(f"{name} must be a single path segment, got {value!r}")
    return f"series/{team}/{host}/{month}.jsonl"


def spool_dir(parent: Any) -> Path:
    """The live, UNPUBLISHED spool. Ignored by Git, exactly like the ledger's."""
    return Path(parent) / "ignored" / "ci-hub" / "series-spool"


def row_identity(row: dict) -> tuple[str, str, int]:
    """The (cell, tree, run) key. Two rows sharing it describe one repetition."""
    payload = row.get("series") or {}
    return (
        str(payload.get("cell", "")),
        str(payload.get("tree", "")),
        int(payload.get("run_index", -1)),
    )


def validate_row(row: Any, *, strict_paths: bool = True) -> dict:
    """Refuse a row that cannot be read later. The LINTER, not the reader.

    Kept separate from the fold for the ledger's stated reason: a reader that
    enforced the lint could not replay history already written. This refuses at
    the WRITE boundary only.
    """
    if not isinstance(row, dict):
        raise MalformedRow(f"row must be an object, got {type(row).__name__}")
    missing = [k for k in ENVELOPE_KEYS if not row.get(k)]
    if missing:
        raise MalformedRow(f"missing envelope keys: {', '.join(sorted(missing))}")
    if row["schema"] != ENVELOPE_VERSION:
        raise MalformedRow(f"schema must be {ENVELOPE_VERSION!r}, got {row['schema']!r}")

    payload = row.get("series")
    if not isinstance(payload, dict):
        raise MalformedRow("row must carry a 'series' object")
    missing = [k for k in IDENTITY_KEYS if payload.get(k) in (None, "")]
    if missing:
        raise MalformedRow(f"series missing identity keys: {', '.join(sorted(missing))}")

    cell = str(payload["cell"])
    if not _CELL_RE.fullmatch(cell):
        raise MalformedRow(
            f"cell must be '<test>/<mode>/<backend>' with the test's own slashes "
            f"allowed, got {cell!r}"
        )
    tree = str(payload["tree"])
    if not _SHA_RE.fullmatch(tree):
        raise MalformedRow(f"tree must be a 40-hex commit sha, got {tree!r}")
    run_index = payload["run_index"]
    if not isinstance(run_index, int) or isinstance(run_index, bool) or run_index < 0:
        raise MalformedRow(f"run_index must be a non-negative int, got {run_index!r}")

    outcome = payload.get("outcome")
    if outcome not in OUTCOMES:
        raise MalformedRow(
            f"outcome must be one of {sorted(OUTCOMES)}, got {outcome!r}"
        )

    coords = payload.get("coordinates")
    if coords is not None and not isinstance(coords, dict):
        raise MalformedRow("coordinates must be an object when present")
    if isinstance(coords, dict):
        unknown = sorted(set(coords) - set(COORDINATE_KEYS))
        if unknown:
            raise MalformedRow(f"unknown coordinate keys: {', '.join(unknown)}")
        for key, value in coords.items():
            if value is None:
                continue
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise MalformedRow(
                    f"coordinate {key} must be a non-negative int or null, got {value!r}"
                )

    # ⚠️ THE ONE CROSS-FIELD RULE, and it is the whole point of the store.
    # 'diverged' with no located coordinate is a LEGITIMATE and distinct state --
    # the run diverged and the position could not be established -- so it is
    # allowed. What is refused is the inverse: coordinates on a row that did not
    # diverge, which would let a passing run contribute a position to a range.
    if outcome != "diverged" and isinstance(coords, dict):
        located = {k: v for k, v in coords.items() if v is not None}
        if located:
            raise MalformedRow(
                f"outcome {outcome!r} must not carry located coordinates "
                f"{sorted(located)}; only 'diverged' may"
            )

    if strict_paths and _contains_owner_path(row):
        raise MalformedRow("row contains a workspace-local owner path")
    return row


def _contains_owner_path(value: Any) -> bool:
    if isinstance(value, str):
        return _OWNER_PATH_NEEDLE in value
    if isinstance(value, dict):
        return any(_contains_owner_path(v) for v in value.values())
    if isinstance(value, (list, tuple)):
        return any(_contains_owner_path(v) for v in value)
    return False


def union(paths: Sequence[Any]) -> list[dict]:
    """Deterministic union across spool batches and tracked shards.

    Delegates to the ledger's `union_events`: same ordering rule, same event-id
    dedup, no series-specific branch.
    """
    events: list[dict] = []
    for path in paths:
        events.extend(read_shard(path))
    return union_events(events)


def series_for(rows: Iterable[dict], cell: str, tree: str) -> list[dict]:
    """Every repetition of one cell against one tree, in run order.

    This is the READER for steps 4, 5 and 8: the stored form is one row per run,
    and {earliest,latest} is a DERIVED view over this list rather than a stored
    field.
    """
    out = [
        r for r in rows
        if (r.get("series") or {}).get("cell") == cell
        and (r.get("series") or {}).get("tree") == tree
    ]
    out.sort(key=lambda r: int((r.get("series") or {}).get("run_index", -1)))
    return out


def observed_range(rows: Iterable[dict], coordinate: str) -> dict | None:
    """`{earliest, latest, samples}` for ONE coordinate, DERIVED not stored.

    Returns None when no row located that coordinate, which is how
    'diverged-but-unlocated' stays distinguishable from 'never-measured' at the
    consumer: the caller knows whether it had rows at all.

    ⚠️ ONE COORDINATE AT A TIME, BY CONSTRUCTION. Taking a min across axes would
    mix record numbers with syscall counts; the signature does not allow it.
    """
    if coordinate not in COORDINATE_KEYS:
        raise SeriesError(f"unknown coordinate {coordinate!r}")
    values = [
        (r.get("series") or {}).get("coordinates", {}).get(coordinate)
        for r in rows
        if (r.get("series") or {}).get("outcome") == "diverged"
    ]
    located = [v for v in values if isinstance(v, int) and not isinstance(v, bool)]
    if not located:
        return None
    return {"earliest": min(located), "latest": max(located), "samples": len(located)}


def measurement_state(rows: Sequence[dict]) -> str:
    """never-measured | measured-and-passed | diverged-unlocated | diverged.

    Step 6 needs these four readable from the row without consulting another
    file; this is the function that defines them so every consumer agrees.
    """
    if not rows:
        return "never-measured"
    outcomes = {(r.get("series") or {}).get("outcome") for r in rows}
    if "diverged" not in outcomes:
        return "measured-and-passed" if outcomes <= {"passed"} else "measured-no-verdict"
    for coordinate in COORDINATE_KEYS:
        if observed_range(rows, coordinate) is not None:
            return "diverged"
    return "diverged-unlocated"


def append_rows(path: Any, rows: Iterable[dict]) -> int:
    """Validate then append. Refuses the batch before writing any of it."""
    checked = [validate_row(dict(r)) for r in rows]
    return append_events(path, checked)


__all__ = [
    "COORDINATE_KEYS",
    "ENVELOPE_KEYS",
    "ENVELOPE_VERSION",
    "IDENTITY_KEYS",
    "NON_VERDICT_OUTCOMES",
    "OUTCOMES",
    "MalformedRow",
    "SeriesError",
    "append_rows",
    "measurement_state",
    "observed_range",
    "parse_line",
    "row_identity",
    "series_for",
    "shard_path",
    "spool_dir",
    "union",
    "validate_row",
    "verify_append_only",
]
