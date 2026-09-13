#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Stress/flakiness SERIES store: append-only history per (cell, tree, run).

WHY A SECOND STORE AND NOT A COLUMN ON THE FIRST. The validation ledger records
one event per validation RUN of a whole tree. This represents one observation
per CELL per REPETITION, with equal observations allowed to share one row, so a
single pressure campaign over 40 cells at N=20 represents 800 runs
against one tree. Those are different cardinalities and different lifetimes,
and folding them into the validate shards would make a month's shard unreadable
for its original purpose.

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
tracked shard. The scheduled parent publisher moves those rows into tracked
`series/` shards through the serialized parent-main writer. Readers union the
spool, batches retained until the producing checkout syncs, and every tracked
shard, so a row is visible before, during and after publication. A reader that
consulted only one source would report a campaign as missing while it was being
published.
"""

from __future__ import annotations

import copy
import contextlib
import ctypes
import errno
import fcntl
import hashlib
import json
import math
import os
import re
import secrets
import shutil
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from collections.abc import Iterable, Mapping, Sequence
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

LEGACY_ENVELOPE_VERSION = "stress-series/v1"
HOST_FACTS_ENVELOPE_VERSION = "stress-series/v2"
ENVELOPE_VERSION = "stress-series/v3"
SCORECARD_SNAPSHOT_SCHEMA = "scorecard-series-snapshot/v1"
SCORECARD_SNAPSHOT_SOURCE = "series"
SERIES_SNAPSHOT_REF = "refs/ci-hub/series/published"
SERIES_SPOOL_LOCK_TIMEOUT_SECONDS = 5.0
READABLE_ENVELOPE_VERSIONS = frozenset({
    LEGACY_ENVELOPE_VERSION,
    HOST_FACTS_ENVELOPE_VERSION,
    ENVELOPE_VERSION,
})

#: The four divergence coordinates, stored as RAW MEASURED POSITIONS. A row may
#: count repeated observations only when every stored position is exactly equal.
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

FIRST_DIVERGENT_MESSAGE_KEYS = ("left", "right")
WORKSPACE_TOKEN = "{{WORKSPACE_ROOT}}"
RECORDED_ROOT = "/repo"
# Unicode White_Space (not Python's broader str.isspace set) plus the exact
# punctuation that delimits path tokens in recorded argv/env/shell text.
_EVIDENCE_PATH_DELIMITERS = frozenset(
    "\u0009\u000a\u000b\u000c\u000d\u0020\u0085\u00a0\u1680"
    "\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a"
    "\u2028\u2029\u202f\u205f\u3000"
    "=,:;()[]{}<>\"'"
)

_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
PRESSURE_PROMOTION_REPETITIONS = 10
# Built rather than written literally so this module does not itself contain the
# owner-path needle the portability check greps tracked files for.
_OWNER_PATH_NEEDLE = "/" + "home" + "/"
_GIT_REPO_SCOPED_ENV = (
    "GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE", "GIT_PREFIX", "GIT_INDEX_VERSION",
)


class SeriesError(LedgerError):
    """Any series-store violation."""


class MalformedRow(SeriesError):
    """A row that does not satisfy the schema."""


class _UntypedComparisonCell(MalformedRow):
    """A pressure comparison result predating typed attempt evidence."""


@dataclass(frozen=True)
class PublishedSnapshot:
    """One immutable published-series revision and its canonical rows."""

    commit: str
    rows: list[dict]


def shard_path(team: str, host: str, month: str) -> str:
    """`series/<team>/<host>/<YYYY>-<MM>.jsonl`.

    Beside `ledger/`, never inside it: two roots keep a `git log` over
    validation history readable when a campaign emits 800 rows in a minute.
    The path is a storage index derived from the row's authoritative `host`
    field. Moving or republishing a row cannot change the machine it describes;
    the v2 type also requires `host == series.machine_shortname`. Sharding per
    team and short host is what makes two machines merge trivially.
    """
    if not re.fullmatch(r"\d{4}-\d{2}", month or ""):
        raise MalformedRow(f"month must be YYYY-MM, got {month!r}")
    for name, value in (("team", team), ("host", host)):
        if not value or "/" in value or value in (".", ".."):
            raise MalformedRow(f"{name} must be a single path segment, got {value!r}")
    return f"series/{team}/{host}/{month}.jsonl"


def shared_parent_root(parent: Any) -> Path:
    """Return the primary checkout root shared by a linked parent worktree.

    A series producer commonly runs from a detached dev-hermit worktree. If its
    spool stayed under that worktree, the scheduled publisher rooted at the
    primary checkout would never see it. The validation ledger already resolves
    the same Git common directory for this reason. Independent clones keep their
    own ``.git`` directory and therefore remain independent producer roots.
    """
    parent = Path(parent).resolve()
    env = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    try:
        result = subprocess.run(
            [
                "git", "--no-replace-objects", "-C", str(parent), "rev-parse",
                "--path-format=absolute", "--git-common-dir",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
    except (OSError, subprocess.TimeoutExpired):
        return parent
    if result.returncode != 0 or not result.stdout.strip():
        return parent
    common_dir = Path(result.stdout.strip()).resolve()
    if common_dir.name != ".git":
        return parent
    shared_root = common_dir.parent
    if not (shared_root / "ci-hub" / "series" / "series.py").is_file():
        return parent
    return shared_root


def spool_dir(parent: Any) -> Path:
    """The live, UNPUBLISHED spool in the shared parent checkout."""
    return shared_parent_root(parent) / "ignored" / "ci-hub" / "series-spool"


def row_identity(row: dict) -> tuple[str, str, int, int]:
    """The (cell, tree, run, attempt) key for one execution."""
    payload = row.get("series") or {}
    return (
        str(payload.get("cell", "")),
        str(payload.get("tree", "")),
        int(payload.get("run_index", -1)),
        int(payload.get("attempt", 1)),
    )


def validate_row(row: Any, *, strict_paths: bool = True) -> dict:
    """Refuse a row that cannot be read later. The LINTER, not the reader.

    Hermit's shared Rust type owns the serialized shape and cross-field rules.
    Python retains only the parent-specific owner-path rule.
    """
    return validate_rows([row], strict_paths=strict_paths)[0]


def validate_rows(rows: Iterable[Any], *, strict_paths: bool = True) -> list[dict]:
    """Validate a batch with one invocation of the shared Rust type checker."""
    return _validate_rows(rows, strict_paths=strict_paths, read=False)


def validate_read_rows(rows: Iterable[Any], *, strict_paths: bool = True) -> list[dict]:
    """Validate stored rows, retaining readable v1 but requiring complete v2."""
    return _validate_rows(rows, strict_paths=strict_paths, read=True)


def _validate_rows(
    rows: Iterable[Any], *, strict_paths: bool, read: bool,
) -> list[dict]:
    """Apply Hermit's shared SeriesRow checks for one read or write boundary."""
    checked = []
    for row in rows:
        if not isinstance(row, dict):
            raise MalformedRow(f"row must be an object, got {type(row).__name__}")
        copied = dict(row)
        if strict_paths and _contains_owner_path(copied):
            raise MalformedRow("row contains a workspace-local owner path")
        checked.append(copied)
    if not checked:
        return checked

    checker = Path(__file__).resolve().with_name("check-series-rows.rs")
    try:
        command = [str(checker), "--read"] if read else [str(checker)]
        result = subprocess.run(
            command,
            input="".join(
                json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n"
                for row in checked
            ),
            capture_output=True,
            text=True,
            timeout=300,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SeriesError(f"cannot run the shared SeriesRow checker: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise MalformedRow(detail)
    return checked


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
    return union_events(validate_read_rows(events))


def series_for(rows: Iterable[dict], cell: str, tree: str) -> list[dict]:
    """Every repetition of one cell against one tree, in run order.

    This is the READER for steps 4, 5 and 8: a stored row may count exactly equal
    observations, and {earliest,latest} is a DERIVED view over this list rather
    than a stored field.
    """
    out = [
        r for r in rows
        if (r.get("series") or {}).get("cell") == cell
        and (r.get("series") or {}).get("tree") == tree
    ]
    out.sort(key=lambda r: (
        int((r.get("series") or {}).get("run_index", -1)),
        int((r.get("series") or {}).get("attempt", 1)),
    ))
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
    minimum = None
    maximum = None
    samples = 0
    for row in rows:
        payload = row.get("series") or {}
        if payload.get("outcome") != "diverged":
            continue
        value = (payload.get("coordinates") or {}).get(coordinate)
        if isinstance(value, int) and not isinstance(value, bool):
            minimum = value if minimum is None else min(minimum, value)
            maximum = value if maximum is None else max(maximum, value)
            count = payload.get("num_runs", 1)
            samples += (
                count
                if isinstance(count, int) and not isinstance(count, bool) and count > 0
                else 1
            )
    if minimum is None or maximum is None:
        return None
    return {"earliest": minimum, "latest": maximum, "samples": samples}


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


def _pressure_outer_kind(payload: dict) -> str:
    outcome = payload.get("outcome")
    result = payload.get("result")
    failure_class = payload.get("failure_class")
    if outcome == "passed" and result == "pass" and failure_class is None:
        return "pass"
    if failure_class == "product_failure" and outcome in {"diverged", "errored"}:
        return "product"
    return {
        "understood_infrastructure_failure": "infrastructure",
        "understood_prerequisite_failure": "prerequisite",
        "no_result": "no_result",
    }.get(failure_class, "unknown")


def _pressure_inner_kinds(payload: dict) -> list[str] | None:
    """Interpret retained, shared-type-validated facts for qualification only."""
    evidence = payload.get("pressure_evidence")
    if evidence is None:
        return None
    mode = payload["cell"].rsplit("/", 2)[-2]
    kinds = []
    for attempt in evidence["attempts"]:
        comparison = attempt.get("comparison")
        non_product = {
            "guest-launch-refused": "prerequisite",
            "backend-unavailable": "prerequisite",
            "infrastructure": "infrastructure",
            "result-publication": "infrastructure",
            "incomplete-verification-evidence": "no_result",
            "invalid-backend-evidence": "no_result",
        }.get(attempt.get("error_kind"))
        if non_product is not None:
            kind = non_product
        elif comparison is not None:
            verdict = comparison["verdict"]
            if comparison["canonical"] and verdict == "matched":
                kind = "pass"
            elif comparison["canonical"] and verdict == "diverged":
                kind = "product"
            elif verdict == "infrastructure_error":
                kind = "infrastructure"
            elif verdict == "no_result" and comparison.get("no_result_kind") == "first_run_rejected":
                kind = "product"
            else:
                kind = "no_result"
        elif mode in _COMPARISON_MODES or attempt["timed_out"]:
            kind = "no_result"
        elif attempt["outcome"] == "PASS":
            # Custom modes can deliberately expect a nonzero exit or signal.
            # The shared validator checks their completed process disposition.
            kind = "pass"
        elif attempt["outcome"] == "FAIL":
            if attempt.get("status") is None and attempt.get("signal") is None:
                return None
            kind = "product"
        else:
            kind = "no_result"
        kinds.append(kind)
    return kinds


def classify_pressure_sample(
    rows: Iterable[dict], selected_cells: Sequence[str], campaign_id: str
) -> dict[str, dict[str, Any]]:
    """Classify one fixed ten-repetition pressure campaign by exact cell.

    Rows are the accepted series projection, not website state.  The latter
    intentionally collapses every non-product failure to an unknown result and
    therefore cannot distinguish infrastructure, prerequisites, and no-result.
    Exact duplicate events are harmless publication replay; two distinct events
    claiming the same repetition/attempt make that cell incomplete.
    """
    selected = list(selected_cells)
    if not isinstance(campaign_id, str) or not campaign_id or any(
        not isinstance(cell, str) or not cell for cell in selected
    ):
        raise SeriesError("pressure classification requires a named campaign and unique cells")
    if len(selected) != len(set(selected)):
        raise SeriesError("pressure classification requires a named campaign and unique cells")

    by_cell: dict[str, dict[int, dict[int, dict]]] = {
        cell: {} for cell in selected
    }
    duplicate_attempts = {cell: 0 for cell in selected}
    seen_events: dict[str, dict] = {}
    trees = set()
    campaign_rows = [row for row in rows if row.get("run_id") == campaign_id]
    for row in validate_read_rows(campaign_rows):
        event_id = row.get("event_id")
        if isinstance(event_id, str) and event_id in seen_events:
            if seen_events[event_id] != row:
                raise SeriesError(f"pressure event_id {event_id!r} changed content")
            continue
        if isinstance(event_id, str):
            seen_events[event_id] = row
        producer = row.get("producer")
        producer_name = producer.get("tool") if isinstance(producer, dict) else producer
        if producer_name != "pressure-test":
            raise SeriesError(
                f"campaign {campaign_id!r} contains non-pressure producer {producer_name!r}"
            )
        payload = row.get("series")
        if not isinstance(payload, dict):
            raise SeriesError("pressure campaign row has no series payload")
        cell = payload.get("cell")
        if cell not in by_cell:
            raise SeriesError(
                f"campaign {campaign_id!r} contains unselected cell {cell!r}"
            )
        tree = payload.get("tree")
        if not isinstance(tree, str) or not tree:
            raise SeriesError("pressure campaign row has no source tree")
        trees.add(tree)
        first = payload.get("run_index")
        count = payload.get("num_runs", 1)
        attempt = payload.get("attempt", 1)
        if (
            not isinstance(first, int)
            or isinstance(first, bool)
            or not isinstance(count, int)
            or isinstance(count, bool)
            or count < 1
            or not isinstance(attempt, int)
            or isinstance(attempt, bool)
            or attempt < 1
            or (count > 1 and "attempt" in payload)
        ):
            raise SeriesError("pressure campaign row has invalid repetition/attempt span")
        last = payload.get("last_run_index", first + count - 1)
        if last != first + count - 1:
            raise SeriesError("pressure campaign row has an inconsistent repetition span")
        for repetition in range(first, last + 1):
            attempts = by_cell[cell].setdefault(repetition, {})
            if attempt in attempts:
                duplicate_attempts[cell] += 1
            else:
                attempts[attempt] = payload
    if len(trees) > 1:
        raise SeriesError(f"campaign {campaign_id!r} spans multiple source trees")

    expected = set(range(1, PRESSURE_PROMOTION_REPETITIONS + 1))
    classifications: dict[str, dict[str, Any]] = {}
    for cell in selected:
        repetitions = by_cell[cell]
        counts = {
            "qualifying_passes": 0,
            "terminal_passes": 0,
            "terminal_product_failures": 0,
            "infrastructure_failures": 0,
            "prerequisite_failures": 0,
            "no_results": 0,
            "mixed_repetitions": 0,
            "retried_repetitions": 0,
            "unknown_history_repetitions": 0,
        }
        malformed = duplicate_attempts[cell] > 0 or set(repetitions) - expected != set()
        for repetition in sorted(set(repetitions) & expected):
            attempts = repetitions[repetition]
            ordinals = sorted(attempts)
            if len(ordinals) > 1:
                counts["retried_repetitions"] += 1
            if (
                ordinals != list(range(1, len(ordinals) + 1))
                or len(ordinals) > 2
                or any(
                    attempts[ordinal]["outcome"] == "passed"
                    for ordinal in ordinals[:-1]
                )
            ):
                # The framework caps outer attempts at two and stops on PASS.
                # This history cannot supply a valid terminal observation.
                malformed = True
                counts["mixed_repetitions"] += 1
                continue
            kinds = []
            inner_kinds = []
            history_known = True
            for ordinal in ordinals:
                payload = attempts[ordinal]
                kinds.append(_pressure_outer_kind(payload))
                retained = _pressure_inner_kinds(payload)
                if not retained or payload.get("source_tree_dirty", False):
                    history_known = False
                else:
                    inner_kinds.extend(retained)
            if not history_known:
                counts["unknown_history_repetitions"] += 1
            adverse_kinds = set(kinds) | (set(inner_kinds) - {"pass"})
            # Valid retries select PASS over FAIL over ERROR. Retain every
            # adverse attempt when deciding whether the history is homogeneous.
            if "pass" in kinds:
                counts["terminal_passes"] += 1
                if (
                    history_known
                    and ordinals == [1]
                    and kinds == ["pass"]
                    and set(inner_kinds) == {"pass"}
                ):
                    counts["qualifying_passes"] += 1
            elif history_known and adverse_kinds == {"product"}:
                counts["terminal_product_failures"] += 1
            elif history_known and adverse_kinds == {"infrastructure"}:
                counts["infrastructure_failures"] += 1
            elif (
                history_known
                and adverse_kinds == {"prerequisite"}
            ):
                counts["prerequisite_failures"] += 1
            elif history_known and adverse_kinds == {"no_result"}:
                counts["no_results"] += 1
            else:
                counts["mixed_repetitions"] += 1

        observed = len(set(repetitions) & expected)
        missing = PRESSURE_PROMOTION_REPETITIONS - observed
        complete = not malformed and missing == 0 and counts["unknown_history_repetitions"] == 0
        if complete and counts["qualifying_passes"] == PRESSURE_PROMOTION_REPETITIONS:
            classification = "promotion-candidate"
        elif complete and counts["terminal_passes"] > 0:
            classification = "intermittent"
        elif complete and counts["terminal_product_failures"] == PRESSURE_PROMOTION_REPETITIONS:
            classification = "confirmed-failing"
        elif complete and counts["infrastructure_failures"] == PRESSURE_PROMOTION_REPETITIONS:
            classification = "infrastructure-failure"
        elif complete and counts["prerequisite_failures"] == PRESSURE_PROMOTION_REPETITIONS:
            classification = "prerequisite-failure"
        elif complete and counts["no_results"] == PRESSURE_PROMOTION_REPETITIONS:
            classification = "no-result"
        else:
            classification = "incomplete"
        classifications[cell] = {
            "classification": classification,
            "promotion_candidate": classification == "promotion-candidate",
            "expected_repetitions": PRESSURE_PROMOTION_REPETITIONS,
            "observed_repetitions": observed,
            "missing_repetitions": missing,
            "duplicate_attempts": duplicate_attempts[cell],
            **counts,
        }
    return classifications


def append_rows(path: Any, rows: Iterable[dict]) -> int:
    """Validate then append. Refuses the batch before writing any of it."""
    checked = validate_rows(rows)
    return append_events(path, checked)


def append_cell_rows(parent: Any, path: Any, rows: Iterable[dict]) -> int:
    """Append projected cell rows once, refusing a changed repeated identity."""
    path = Path(path)
    checked = validate_rows(rows)
    path.parent.mkdir(parents=True, exist_ok=True)
    with series_spool_transition_lock(path.parent, exclusive=True):
        # Check both the live spool and published shards. Once a batch has been
        # published and its spool removed, re-emitting it must still be a no-op.
        existing = union(store_paths(parent))
        known = {str(row.get("event_id", "")): row for row in existing}
        pending = []
        batch_ids = set()
        for row in checked:
            event_id = str(row.get("event_id", ""))
            if event_id in batch_ids:
                raise SeriesError(
                    f"cell batch repeats event_id {event_id!r}; run_index must identify one run"
                )
            batch_ids.add(event_id)
            prior = known.get(event_id)
            if prior is None:
                known[event_id] = row
                pending.append(row)
                continue
            # The observation time belongs to the original append. A later
            # command that reads the same retained run gets today's time by
            # default; restore the recorded time before comparing so an exact
            # replay is a no-op.
            replay = json.loads(json.dumps(row))
            replay["emitted_at"] = prior.get("emitted_at")
            if json.dumps(replay, sort_keys=True, separators=(",", ":")) != json.dumps(
                prior, sort_keys=True, separators=(",", ":")
            ):
                raise SeriesError(
                    f"event_id {event_id!r} already records a different row; refusing rewrite"
                )
        return append_events(path, pending) if pending else 0


def store_paths(parent: Any) -> list[Path]:
    """Every tracked, live, and published-but-not-yet-synced batch."""
    parent = Path(parent)
    tracked = sorted((parent / "series").glob("*/*/*.jsonl"))
    spool = spool_dir(parent)
    live = sorted(spool.glob("*.jsonl"))
    retained = sorted((spool / "published").glob("*.jsonl"))
    return tracked + live + retained


def refresh_origin_main(checkout: Any, repository: str) -> None:
    """Refresh one checkout's remote main without moving its working tree."""
    checkout = Path(checkout)
    if not _is_checkout(checkout):
        raise SeriesError(f"{repository} checkout is unavailable at {checkout}")
    command = [
        "git", "-C", str(checkout), "fetch", "--quiet", "--force", "origin",
        "refs/heads/main:refs/remotes/origin/main",
    ]
    proxy = shutil.which(os.environ.get("WITH_PROXY", "with-proxy"))
    if proxy:
        command.insert(0, proxy)
    env = dict(os.environ)
    for name in _GIT_REPO_SCOPED_ENV:
        env.pop(name, None)
    fetched = subprocess.run(
        command, capture_output=True, text=True, timeout=60, check=False, env=env,
    )
    if fetched.returncode != 0:
        detail = fetched.stderr.strip() or fetched.stdout.strip() or "no output"
        raise SeriesError(f"cannot fetch {repository} origin/main: {detail}")


def published_snapshot(parent: Any) -> PublishedSnapshot:
    """Fetch once, then read one resolved parent ``origin/main`` snapshot."""
    parent = Path(parent)
    refresh_origin_main(parent, "parent")
    return published_snapshot_at(parent, "refs/remotes/origin/main")


def published_snapshot_at(parent: Any, published_ref: str) -> PublishedSnapshot:
    """Resolve ``published_ref`` once and read only that immutable commit.

    Every tree and blob lookup below uses the resolved 40-hex object id.  A
    concurrent fetch may move the caller's ref after resolution, but it cannot
    splice shards from two revisions into one reported snapshot.
    """
    parent = Path(parent)
    resolved = _git(parent, [
        "--no-replace-objects", "rev-parse", "--verify", f"{published_ref}^{{commit}}",
    ])
    if resolved.returncode != 0:
        raise SeriesError(
            f"cannot resolve published series revision {published_ref}: "
            + (resolved.stderr.strip() or "no output")
        )
    commit = resolved.stdout.strip()
    if not _SHA_RE.fullmatch(commit):
        raise SeriesError(
            f"published series revision {published_ref} resolved to invalid commit "
            f"{commit!r}"
        )

    listed = _git(parent, [
        "--no-replace-objects", "ls-tree", "-r", "--name-only", commit, "--", "series",
    ])
    if listed.returncode != 0:
        raise SeriesError(
            f"cannot list series shards at {commit}: "
            + (listed.stderr.strip() or "no output")
        )
    rows: list[dict] = []
    for relative in listed.stdout.splitlines():
        parts = Path(relative).parts
        if len(parts) != 4 or parts[0] != "series" or not relative.endswith(".jsonl"):
            continue
        shown = _git(parent, ["--no-replace-objects", "show", f"{commit}:{relative}"])
        if shown.returncode != 0:
            raise SeriesError(
                f"cannot read parent {commit}:{relative}: "
                + (shown.stderr.strip() or "no output")
            )
        if shown.stdout and not shown.stdout.endswith("\n"):
            raise MalformedEvent(
                f"parent {commit}:{relative}: final line has no newline terminator"
            )
        rows.extend(parse_line(line) for line in shown.stdout.splitlines() if line.strip())
    return PublishedSnapshot(commit, union_events(validate_read_rows(rows)))


def published_rows(parent: Any) -> list[dict]:
    """Compatibility wrapper returning rows from the current published snapshot."""
    return published_snapshot(parent).rows


def published_rows_at(parent: Any, published_ref: str) -> list[dict]:
    """Compatibility wrapper returning rows from one resolved revision."""
    return published_snapshot_at(parent, published_ref).rows


def history_store_rows(parent: Any) -> list[dict]:
    """Published parent rows plus this checkout's unpublished and retained batches."""
    spool = spool_dir(parent)
    local = sorted(spool.glob("*.jsonl")) + sorted((spool / "published").glob("*.jsonl"))
    return union_events([*published_rows(parent), *union(local)])


def _series_tree_at(parent: Path, commit: str) -> str:
    """Resolve the exact tree containing the published series at ``commit``."""
    resolved = _git(parent, [
        "--no-replace-objects",
        "rev-parse",
        "--verify",
        f"{commit}:{SCORECARD_SNAPSHOT_SOURCE}",
    ])
    tree = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA_RE.fullmatch(tree) is None:
        detail = resolved.stderr.strip() or resolved.stdout.strip() or "no output"
        raise SeriesError(
            f"cannot resolve published series tree at {commit}: {detail}"
        )
    object_type = _git(parent, ["--no-replace-objects", "cat-file", "-t", tree])
    if object_type.returncode != 0 or object_type.stdout.strip() != "tree":
        raise SeriesError(
            f"published series object {tree} at {commit} is not a tree"
        )
    return tree


def _canonical_rows_bytes(rows: Sequence[dict]) -> bytes:
    return "".join(
        json.dumps(row, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
        for row in rows
    ).encode()


def _stat_signature(status: os.stat_result) -> tuple[int, ...]:
    """Fields that must remain stable while immutable evidence is consumed."""
    return (
        status.st_dev,
        status.st_ino,
        status.st_mode,
        status.st_nlink,
        status.st_size,
        status.st_mtime_ns,
        status.st_ctime_ns,
    )


def _read_held_fd(descriptor: int) -> bytes:
    """Read from an already-bound descriptor without reopening its pathname."""
    chunks = []
    offset = 0
    while True:
        chunk = os.pread(descriptor, 1024 * 1024, offset)
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)
        offset += len(chunk)


def _lstat_at(directory_fd: int, name: str, display: str) -> os.stat_result:
    try:
        return os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except OSError as error:
        raise SeriesError(
            f"series snapshot path changed or is unavailable: {display}: {error}"
        ) from error


@dataclass
class _HeldSnapshotSource:
    """One source whose descriptor and directory entry stay bound until publish."""

    kind: str
    relative: str
    directory_fd: int
    name: str
    descriptor: int
    signature: tuple[int, ...]
    sha256: str
    size: int
    rows: list[dict]

    def verify(self) -> None:
        display = self.relative
        current = os.fstat(self.descriptor)
        if (
            not stat.S_ISREG(current.st_mode)
            or _stat_signature(current) != self.signature
        ):
            raise SeriesError(f"series snapshot source changed while held open: {display}")
        current_path = _lstat_at(self.directory_fd, self.name, display)
        if (
            not stat.S_ISREG(current_path.st_mode)
            or _stat_signature(current_path) != self.signature
        ):
            raise SeriesError(
                f"series snapshot source pathname changed while held open: {display}"
            )
        if hashlib.sha256(_read_held_fd(self.descriptor)).hexdigest() != self.sha256:
            raise SeriesError(
                f"series snapshot source content changed while held open: {display}"
            )

    def close(self) -> None:
        os.close(self.descriptor)


def _open_snapshot_source(
    directory_fd: int, name: str, *, kind: str, relative: str,
) -> _HeldSnapshotSource:
    """Open and parse one source through its held parent directory descriptor."""
    flags = os.O_RDONLY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(name, flags, dir_fd=directory_fd)
    except OSError as error:
        raise SeriesError(
            f"cannot open regular series snapshot source {relative}: {error}"
        ) from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise SeriesError(f"series snapshot source is not a regular file: {relative}")
        path_status = _lstat_at(directory_fd, name, relative)
        if (
            not stat.S_ISREG(path_status.st_mode)
            or (path_status.st_dev, path_status.st_ino) != (before.st_dev, before.st_ino)
        ):
            raise SeriesError(
                f"series snapshot source pathname does not name its opened file: {relative}"
            )
        payload = _read_held_fd(descriptor)
        signature = _stat_signature(before)
        after = os.fstat(descriptor)
        if _stat_signature(after) != signature:
            raise SeriesError(f"series snapshot source changed while being read: {relative}")
        digest = hashlib.sha256(payload).hexdigest()
        if payload and not payload.endswith(b"\n"):
            raise MalformedEvent(
                f"{relative}: final line has no newline terminator — the batch is truncated"
            )
        try:
            text = payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise MalformedEvent(f"{relative}: batch is not UTF-8: {error}") from error
        rows = validate_read_rows(
            [parse_line(line) for line in text.splitlines() if line.strip()]
        )
        source = _HeldSnapshotSource(
            kind=kind,
            relative=relative,
            directory_fd=directory_fd,
            name=name,
            descriptor=descriptor,
            signature=signature,
            sha256=digest,
            size=len(payload),
            rows=rows,
        )
        source.verify()
        return source
    except Exception:
        os.close(descriptor)
        raise


def _jsonl_names(directory_fd: int) -> list[str]:
    return sorted(name for name in os.listdir(directory_fd) if name.endswith(".jsonl"))


@dataclass
class _SeriesSpoolLock:
    """One owner for the series spool lock and its bound directory inode."""

    spool: Path
    spool_fd: int
    spool_identity: tuple[int, int]
    lock_fd: int
    lock_signature: tuple[int, ...]

    def verify(self) -> None:
        held = os.fstat(self.spool_fd)
        try:
            named = os.lstat(self.spool)
        except OSError as error:
            raise SeriesError(f"series spool changed while lock was held: {error}") from error
        identity = (held.st_dev, held.st_ino)
        if (
            not stat.S_ISDIR(held.st_mode)
            or not stat.S_ISDIR(named.st_mode)
            or identity != self.spool_identity
            or (named.st_dev, named.st_ino) != self.spool_identity
        ):
            raise SeriesError("series spool pathname changed while lock was held")

        held_lock = os.fstat(self.lock_fd)
        named_lock = _lstat_at(
            self.spool_fd, ".append.lock", str(self.spool / ".append.lock")
        )
        if (
            not stat.S_ISREG(held_lock.st_mode)
            or not stat.S_ISREG(named_lock.st_mode)
            or _stat_signature(held_lock) != self.lock_signature
            or _stat_signature(named_lock) != self.lock_signature
        ):
            raise SeriesError("series snapshot writer lock changed while held")

    def close(self) -> None:
        try:
            fcntl.flock(self.lock_fd, fcntl.LOCK_UN)
        finally:
            os.close(self.lock_fd)
            os.close(self.spool_fd)


@contextlib.contextmanager
def series_spool_transition_lock(
    spool: Any,
    *,
    exclusive: bool,
    timeout_seconds: float = SERIES_SPOOL_LOCK_TIMEOUT_SECONDS,
    poll_seconds: float = 0.05,
):
    """Serialize series writers/publishers or hold a stable reader frontier.

    This is the sole lock owner. Callers that already hold it must use their
    ``*_locked`` helper instead of recursively opening the same lock pathname.
    """
    if (
        not math.isfinite(timeout_seconds)
        or timeout_seconds < 0
        or not math.isfinite(poll_seconds)
        or poll_seconds <= 0
    ):
        raise SeriesError(
            "series spool lock timeout must be finite and nonnegative and poll "
            "interval must be finite and positive"
        )
    spool = Path(spool)
    try:
        spool.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise SeriesError(
            f"cannot create series spool for lock {spool / '.append.lock'}: {error}"
        ) from error
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    try:
        spool_fd = os.open(spool, directory_flags)
    except OSError as error:
        raise SeriesError(
            f"cannot open regular series spool directory {spool}: {error}"
        ) from error
    lock_fd = -1
    held: _SeriesSpoolLock | None = None
    try:
        spool_status = os.fstat(spool_fd)
        named_spool = os.lstat(spool)
        if (
            not stat.S_ISDIR(spool_status.st_mode)
            or not stat.S_ISDIR(named_spool.st_mode)
            or (spool_status.st_dev, spool_status.st_ino)
            != (named_spool.st_dev, named_spool.st_ino)
        ):
            raise SeriesError(
                f"series spool pathname does not name its opened directory: {spool}"
            )

        lock_flags = os.O_RDWR | os.O_CREAT | os.O_CLOEXEC
        if hasattr(os, "O_NOFOLLOW"):
            lock_flags |= os.O_NOFOLLOW
        try:
            lock_fd = os.open(
                ".append.lock", lock_flags, 0o644, dir_fd=spool_fd,
            )
        except OSError as error:
            raise SeriesError(
                f"cannot open series spool lock {spool / '.append.lock'}: {error}"
            ) from error
        lock_status = os.fstat(lock_fd)
        if not stat.S_ISREG(lock_status.st_mode):
            raise SeriesError(
                f"series snapshot lock is not a regular file: {spool / '.append.lock'}"
            )
        mode = fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        deadline = time.monotonic() + timeout_seconds
        while True:
            try:
                fcntl.flock(lock_fd, mode | fcntl.LOCK_NB)
                break
            except BlockingIOError as error:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    kind = "exclusive" if exclusive else "shared"
                    raise SeriesError(
                        f"timed out after {timeout_seconds:.3f}s "
                        f"acquiring {kind} series spool lock {spool / '.append.lock'}"
                    ) from error
                time.sleep(min(poll_seconds, remaining))
            except OSError as error:
                raise SeriesError(
                    f"cannot acquire series spool lock {spool / '.append.lock'}: {error}"
                ) from error
        held = _SeriesSpoolLock(
            spool=spool,
            spool_fd=spool_fd,
            spool_identity=(spool_status.st_dev, spool_status.st_ino),
            lock_fd=lock_fd,
            lock_signature=_stat_signature(lock_status),
        )
        held.verify()
        yield held
        held.verify()
    finally:
        if held is not None:
            held.close()
        else:
            if lock_fd >= 0:
                try:
                    fcntl.flock(lock_fd, fcntl.LOCK_UN)
                finally:
                    os.close(lock_fd)
            os.close(spool_fd)


@dataclass
class _LocalSnapshotCapture:
    """Held local frontier and every source inode under one shared lock."""

    lock: _SeriesSpoolLock
    published_fd: int | None
    published_signature: tuple[int, ...] | None
    source_keys: list[tuple[str, str]]
    sources: list[_HeldSnapshotSource]

    def _verify_directory(self) -> None:
        self.lock.verify()

        if self.published_fd is None:
            try:
                os.stat(
                    "published",
                    dir_fd=self.lock.spool_fd,
                    follow_symlinks=False,
                )
            except FileNotFoundError:
                return
            except OSError as error:
                raise SeriesError(
                    f"retained series directory changed while snapshot was held: {error}"
                ) from error
            raise SeriesError("retained series directory appeared while snapshot was held")

        held_published = os.fstat(self.published_fd)
        named_published = _lstat_at(
            self.lock.spool_fd,
            "published",
            str(self.lock.spool / "published"),
        )
        if (
            not stat.S_ISDIR(held_published.st_mode)
            or not stat.S_ISDIR(named_published.st_mode)
            or _stat_signature(held_published) != self.published_signature
            or _stat_signature(named_published) != self.published_signature
        ):
            raise SeriesError("retained series directory changed while snapshot was held")

    def verify(self) -> None:
        self._verify_directory()
        current = [("live", name) for name in _jsonl_names(self.lock.spool_fd)]
        if self.published_fd is not None:
            current.extend(
                ("published-retained", f"published/{name}")
                for name in _jsonl_names(self.published_fd)
            )
        if current != self.source_keys:
            raise SeriesError("series spool changed while its projection snapshot was held")
        for source in self.sources:
            source.verify()

    def close(self) -> None:
        for source in reversed(self.sources):
            source.close()
        if self.published_fd is not None:
            os.close(self.published_fd)


@contextlib.contextmanager
def _local_snapshot_capture(lock: _SeriesSpoolLock):
    """Bind local sources while the caller owns the shared series lock."""
    spool = lock.spool
    spool_fd = lock.spool_fd
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    published_fd: int | None = None
    sources: list[_HeldSnapshotSource] = []
    capture: _LocalSnapshotCapture | None = None
    try:
        try:
            published_fd = os.open("published", directory_flags, dir_fd=spool_fd)
        except FileNotFoundError:
            published_fd = None
        except OSError as error:
            raise SeriesError(
                f"cannot open regular retained series directory {spool / 'published'}: {error}"
            ) from error

        source_keys = [("live", name) for name in _jsonl_names(spool_fd)]
        if published_fd is not None:
            source_keys.extend(
                ("published-retained", f"published/{name}")
                for name in _jsonl_names(published_fd)
            )
        capture = _LocalSnapshotCapture(
            lock=lock,
            published_fd=published_fd,
            published_signature=(
                _stat_signature(os.fstat(published_fd))
                if published_fd is not None else None
            ),
            source_keys=source_keys,
            sources=sources,
        )
        for kind, relative in source_keys:
            if kind == "live":
                directory_fd, name = spool_fd, relative
            else:
                assert published_fd is not None
                directory_fd, name = published_fd, Path(relative).name
            sources.append(
                _open_snapshot_source(
                    directory_fd, name, kind=kind, relative=relative,
                )
            )
        capture.verify()
        yield capture
    finally:
        if capture is not None:
            capture.close()
        else:
            for source in reversed(sources):
                source.close()
            if published_fd is not None:
                os.close(published_fd)


@contextlib.contextmanager
def _scorecard_projection_capture(parent: Any):
    """Build a snapshot while retaining every mutable source descriptor."""
    parent = Path(parent).resolve()
    # Network work is outside the spool lock. The local ref is bound only after
    # the shared lock is held, so a publisher either leaves its batch visible
    # locally or finishes the exclusive drain before this commit is selected.
    refresh_origin_main(parent, "parent")
    with series_spool_transition_lock(spool_dir(parent), exclusive=False) as lock:
        # LOCK ORDER: series spool first, then remote fetch/ref binding, then
        # local file descriptors, and finally output publication. The series
        # publisher takes the same lock exclusively before it binds or changes
        # the remote commit and never recursively reacquires it.
        origin_ref = "refs/remotes/origin/main"
        origin = _git(parent, [
            "--no-replace-objects", "rev-parse", "--verify", f"{origin_ref}^{{commit}}",
        ])
        marker = _git(parent, [
            "--no-replace-objects",
            "rev-parse",
            "--verify",
            f"{SERIES_SNAPSHOT_REF}^{{commit}}",
        ])
        if origin.returncode != 0:
            raise SeriesError(
                "cannot bind parent origin/main for scorecard snapshot: "
                + (origin.stderr.strip() or "no output")
            )
        selected = origin.stdout.strip()
        if marker.returncode == 0:
            marked = marker.stdout.strip()
            marker_before_origin = _git(parent, [
                "--no-replace-objects",
                "merge-base",
                "--is-ancestor",
                marked,
                selected,
            ])
            if marker_before_origin.returncode == 0:
                pass
            else:
                origin_before_marker = _git(parent, [
                    "--no-replace-objects",
                    "merge-base",
                    "--is-ancestor",
                    selected,
                    marked,
                ])
                if origin_before_marker.returncode != 0:
                    raise SeriesError(
                        "series snapshot marker and parent origin/main are not comparable"
                    )
                selected = marked
        published = published_snapshot_at(parent, selected)
        source_tree = _series_tree_at(parent, published.commit)
        with _local_snapshot_capture(lock) as local:
            local_rows = [row for source in local.sources for row in source.rows]
            try:
                rows = union_events([*published.rows, *local_rows])
            except LedgerError as error:
                raise SeriesError(f"cannot form canonical series snapshot: {error}") from error
            rows_bytes = _canonical_rows_bytes(rows)
            snapshot = {
                "schema": SCORECARD_SNAPSHOT_SCHEMA,
                "source": {
                    "path": SCORECARD_SNAPSHOT_SOURCE,
                    "commit": published.commit,
                    "tree": source_tree,
                    "published_rows": len(published.rows),
                    "local_sources": [
                        {
                            "kind": source.kind,
                            "path": source.relative,
                            "device": source.signature[0],
                            "inode": source.signature[1],
                            "rows": len(source.rows),
                            "size": source.size,
                            "sha256": source.sha256,
                        }
                        for source in local.sources
                    ],
                },
                "rows_read": len(rows),
                "rows_sha256": hashlib.sha256(rows_bytes).hexdigest(),
                "rows": rows,
            }
            local.verify()
            yield snapshot, local


def scorecard_projection_snapshot(parent: Any) -> dict[str, Any]:
    """Build one immutable published+local series input for scorecard write-back.

    Failure is terminal for this operation.  In particular, an unavailable
    published revision never falls back to the tracked files in the caller's
    checkout, which may be arbitrarily stale.
    """
    with _scorecard_projection_capture(parent) as (snapshot, local):
        local.verify()
        return snapshot


def _snapshot_bytes(snapshot: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(snapshot, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    ).encode()


def _validate_snapshot_payload(
    payload: bytes, *, expected_sha256: str,
) -> dict[str, Any]:
    """Validate exact snapshot bytes after a caller binds their inode."""
    actual_sha256 = hashlib.sha256(payload).hexdigest()
    if actual_sha256 != expected_sha256:
        raise SeriesError(
            "scorecard snapshot digest mismatch: "
            f"expected {expected_sha256}, got {actual_sha256}"
        )
    try:
        snapshot = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SeriesError(f"scorecard snapshot is not valid JSON: {error}") from error
    if not isinstance(snapshot, dict) or set(snapshot) != {
        "schema", "source", "rows_read", "rows_sha256", "rows",
    }:
        raise SeriesError("scorecard snapshot has an invalid top-level shape")
    source = snapshot.get("source")
    if not isinstance(source, dict) or set(source) != {
        "path", "commit", "tree", "published_rows", "local_sources",
    }:
        raise SeriesError("scorecard snapshot has an invalid source identity")
    if (
        snapshot.get("schema") != SCORECARD_SNAPSHOT_SCHEMA
        or source.get("path") != SCORECARD_SNAPSHOT_SOURCE
        or _SHA_RE.fullmatch(str(source.get("commit", ""))) is None
        or _SHA_RE.fullmatch(str(source.get("tree", ""))) is None
        or type(source.get("published_rows")) is not int
        or source["published_rows"] < 0
    ):
        raise SeriesError("scorecard snapshot has an invalid source identity")
    local_sources = source.get("local_sources")
    if not isinstance(local_sources, list):
        raise SeriesError("scorecard snapshot local_sources must be an array")
    seen_paths = set()
    ordered_sources = []
    for item in local_sources:
        if not isinstance(item, dict) or set(item) != {
            "kind", "path", "device", "inode", "rows", "size", "sha256",
        }:
            raise SeriesError("scorecard snapshot has an invalid local source")
        relative = item.get("path")
        if (
            item.get("kind") not in {"live", "published-retained"}
            or not isinstance(relative, str)
            or not relative
            or Path(relative).is_absolute()
            or ".." in Path(relative).parts
            or relative in seen_paths
            or type(item.get("device")) is not int
            or item["device"] < 0
            or type(item.get("inode")) is not int
            or item["inode"] <= 0
            or type(item.get("rows")) is not int
            or item["rows"] < 0
            or type(item.get("size")) is not int
            or item["size"] < 0
            or re.fullmatch(r"[0-9a-f]{64}", str(item.get("sha256", ""))) is None
        ):
            raise SeriesError("scorecard snapshot has an invalid local source")
        if item["kind"] == "live" and len(Path(relative).parts) != 1:
            raise SeriesError("scorecard snapshot live source has an invalid path")
        if item["kind"] == "published-retained" and (
            len(Path(relative).parts) != 2
            or Path(relative).parts[0] != "published"
        ):
            raise SeriesError(
                "scorecard snapshot published-retained source has an invalid path"
            )
        seen_paths.add(relative)
        ordered_sources.append((item["kind"], relative))
    source_order = {"live": 0, "published-retained": 1}
    if ordered_sources != sorted(
        ordered_sources, key=lambda item: (source_order[item[0]], item[1])
    ):
        raise SeriesError("scorecard snapshot local sources are not in canonical order")
    rows = snapshot.get("rows")
    if not isinstance(rows, list):
        raise SeriesError("scorecard snapshot rows must be an array")
    checked = validate_read_rows(rows)
    canonical = union_events(checked)
    if canonical != rows:
        raise SeriesError("scorecard snapshot rows are not a unique canonical event order")
    if type(snapshot.get("rows_read")) is not int or snapshot["rows_read"] != len(rows):
        raise SeriesError("scorecard snapshot rows_read does not match its rows")
    if source["published_rows"] > len(rows):
        raise SeriesError("scorecard snapshot published_rows exceeds its canonical rows")
    rows_sha256 = hashlib.sha256(_canonical_rows_bytes(rows)).hexdigest()
    if snapshot.get("rows_sha256") != rows_sha256:
        raise SeriesError("scorecard snapshot row digest does not match its rows")
    return snapshot


def _verify_named_descriptor(
    directory_fd: int,
    name: str,
    descriptor: int,
    signature: tuple[int, ...],
    display: str,
) -> None:
    current = os.fstat(descriptor)
    named = _lstat_at(directory_fd, name, display)
    if (
        not stat.S_ISREG(current.st_mode)
        or not stat.S_ISREG(named.st_mode)
        or _stat_signature(current) != signature
        or _stat_signature(named) != signature
    ):
        raise SeriesError(f"scorecard snapshot pathname changed while held open: {display}")


def _read_scorecard_projection_snapshot_at(
    directory_fd: int,
    name: str,
    *,
    display: str,
    expected_sha256: str,
) -> dict[str, Any]:
    flags = os.O_RDONLY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(name, flags, dir_fd=directory_fd)
    except OSError as error:
        raise SeriesError(
            f"scorecard snapshot is not a regular file: {display}: {error}"
        ) from error
    try:
        status = os.fstat(descriptor)
        if not stat.S_ISREG(status.st_mode):
            raise SeriesError(f"scorecard snapshot is not a regular file: {display}")
        signature = _stat_signature(status)
        _verify_named_descriptor(directory_fd, name, descriptor, signature, display)
        payload = _read_held_fd(descriptor)
        _verify_named_descriptor(directory_fd, name, descriptor, signature, display)
        snapshot = _validate_snapshot_payload(
            payload, expected_sha256=expected_sha256,
        )
        _verify_named_descriptor(directory_fd, name, descriptor, signature, display)
        return snapshot
    finally:
        os.close(descriptor)


def _directory_identity(path: Path, descriptor: int) -> tuple[int, int]:
    held = os.fstat(descriptor)
    try:
        named = os.lstat(path)
    except OSError as error:
        raise SeriesError(f"scorecard snapshot directory changed: {path}: {error}") from error
    if (
        not stat.S_ISDIR(held.st_mode)
        or not stat.S_ISDIR(named.st_mode)
        or (held.st_dev, held.st_ino) != (named.st_dev, named.st_ino)
    ):
        raise SeriesError(f"scorecard snapshot directory pathname changed: {path}")
    return held.st_dev, held.st_ino


def read_scorecard_projection_snapshot(
    path: Any, *, expected_sha256: str
) -> dict[str, Any]:
    """Read one exact snapshot from a held no-follow descriptor."""
    path = Path(path)
    if re.fullmatch(r"[0-9a-f]{64}", expected_sha256 or "") is None:
        raise SeriesError("scorecard snapshot expected_sha256 must be 64 lowercase hex")
    if not path.name:
        raise SeriesError(f"scorecard snapshot path has no filename: {path}")
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    try:
        directory_fd = os.open(path.parent, directory_flags)
    except OSError as error:
        raise SeriesError(
            f"scorecard snapshot parent is not a regular directory: {path.parent}: {error}"
        ) from error
    try:
        identity = _directory_identity(path.parent, directory_fd)
        snapshot = _read_scorecard_projection_snapshot_at(
            directory_fd,
            path.name,
            display=str(path),
            expected_sha256=expected_sha256,
        )
        if _directory_identity(path.parent, directory_fd) != identity:
            raise SeriesError(f"scorecard snapshot directory changed: {path.parent}")
        return snapshot
    finally:
        os.close(directory_fd)


def _write_all(descriptor: int, payload: bytes) -> None:
    offset = 0
    while offset < len(payload):
        written = os.write(descriptor, payload[offset:])
        if written <= 0:
            raise SeriesError("scorecard snapshot staging write made no progress")
        offset += written


def _open_anonymous_snapshot_stage(directory_fd: int) -> int:
    if not hasattr(os, "O_TMPFILE"):
        raise OSError(errno.EOPNOTSUPP, "O_TMPFILE is unavailable")
    return os.open(
        ".", os.O_RDWR | os.O_CLOEXEC | os.O_TMPFILE, 0o600, dir_fd=directory_fd,
    )


def _open_named_snapshot_stage(directory_fd: int, output_name: str) -> tuple[int, str]:
    flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    while True:
        name = f".{output_name}.staging-{os.getpid()}-{secrets.token_hex(16)}"
        try:
            return os.open(name, flags, 0o600, dir_fd=directory_fd), name
        except FileExistsError:
            continue


def _link_anonymous_noreplace(
    descriptor: int, directory_fd: int, output_name: str,
) -> None:
    """Publish an O_TMPFILE inode with linkat(AT_EMPTY_PATH), never replacing."""
    libc = ctypes.CDLL(None, use_errno=True)
    linkat = getattr(libc, "linkat", None)
    if linkat is None:
        raise OSError(errno.ENOSYS, "linkat is unavailable")
    result = linkat(
        ctypes.c_int(descriptor),
        ctypes.c_char_p(b""),
        ctypes.c_int(directory_fd),
        ctypes.c_char_p(os.fsencode(output_name)),
        ctypes.c_int(0x1000),  # AT_EMPTY_PATH
    )
    if result != 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number), output_name)


def _rename_noreplace(
    directory_fd: int, source_name: str, output_name: str,
) -> None:
    """Move a named stage into place atomically without replacing an output."""
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise OSError(errno.ENOSYS, "renameat2 is unavailable")
    result = renameat2(
        ctypes.c_int(directory_fd),
        ctypes.c_char_p(os.fsencode(source_name)),
        ctypes.c_int(directory_fd),
        ctypes.c_char_p(os.fsencode(output_name)),
        ctypes.c_uint(1),  # RENAME_NOREPLACE
    )
    if result != 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number), output_name)


def _validate_staged_snapshot(
    descriptor: int,
    payload: bytes,
    digest: str,
    *,
    directory_fd: int,
    stage_name: str | None,
    display: str,
) -> tuple[int, ...]:
    _write_all(descriptor, payload)
    os.fsync(descriptor)
    status = os.fstat(descriptor)
    if not stat.S_ISREG(status.st_mode):
        raise SeriesError("scorecard snapshot staging descriptor is not a regular file")
    signature = _stat_signature(status)
    if stage_name is not None:
        _verify_named_descriptor(
            directory_fd, stage_name, descriptor, signature, display,
        )
    readback = _read_held_fd(descriptor)
    if _stat_signature(os.fstat(descriptor)) != signature:
        raise SeriesError("scorecard snapshot staging file changed during readback")
    _validate_snapshot_payload(readback, expected_sha256=digest)
    if stage_name is not None:
        _verify_named_descriptor(
            directory_fd, stage_name, descriptor, signature, display,
        )
    return signature


def _existing_snapshot(
    directory_fd: int, output_name: str, output: Path, digest: str,
) -> bool:
    try:
        status = os.stat(output_name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return False
    except OSError as error:
        raise SeriesError(f"cannot inspect scorecard snapshot output {output}: {error}") from error
    if not stat.S_ISREG(status.st_mode):
        raise SeriesError(
            f"scorecard snapshot output already exists with different content: {output}"
        )
    try:
        _read_scorecard_projection_snapshot_at(
            directory_fd,
            output_name,
            display=str(output),
            expected_sha256=digest,
        )
    except SeriesError as error:
        raise SeriesError(
            f"scorecard snapshot output already exists with different content: {output}: {error}"
        ) from error
    return True


def _finish_snapshot_publication(
    directory_fd: int, output: Path, digest: str,
) -> dict[str, Any]:
    os.fsync(directory_fd)
    return _read_scorecard_projection_snapshot_at(
        directory_fd,
        output.name,
        display=str(output),
        expected_sha256=digest,
    )


def _publish_snapshot_payload(
    directory_fd: int, output: Path, payload: bytes, digest: str,
) -> dict[str, Any]:
    """Publish exact bytes without any failure-path unlink by pathname."""
    if _existing_snapshot(directory_fd, output.name, output, digest):
        return _finish_snapshot_publication(directory_fd, output, digest)

    unsupported = {
        errno.EINVAL, errno.EISDIR, errno.ENOENT, errno.ENOSYS,
        errno.EOPNOTSUPP, errno.EPERM,
    }
    anonymous_fd: int | None = None
    try:
        try:
            anonymous_fd = _open_anonymous_snapshot_stage(directory_fd)
        except OSError as error:
            if error.errno not in unsupported:
                raise SeriesError(
                    f"cannot create anonymous scorecard snapshot stage: {error}"
                ) from error
        else:
            _validate_staged_snapshot(
                anonymous_fd,
                payload,
                digest,
                directory_fd=directory_fd,
                stage_name=None,
                display="anonymous scorecard snapshot stage",
            )
            try:
                _link_anonymous_noreplace(
                    anonymous_fd, directory_fd, output.name,
                )
            except FileExistsError:
                if not _existing_snapshot(directory_fd, output.name, output, digest):
                    raise AssertionError("existing snapshot disappeared during inspection")
                return _finish_snapshot_publication(directory_fd, output, digest)
            except OSError as error:
                if error.errno not in unsupported:
                    raise
            else:
                return _finish_snapshot_publication(directory_fd, output, digest)
    finally:
        if anonymous_fd is not None:
            os.close(anonymous_fd)

    stage_fd, stage_name = _open_named_snapshot_stage(directory_fd, output.name)
    stage_moved = False
    try:
        signature = _validate_staged_snapshot(
            stage_fd,
            payload,
            digest,
            directory_fd=directory_fd,
            stage_name=stage_name,
            display=str(output.parent / stage_name),
        )
        # Make the fallback's quarantine name durable before any operation
        # that might have to preserve it for inspection.
        os.fsync(directory_fd)
        _verify_named_descriptor(
            directory_fd,
            stage_name,
            stage_fd,
            signature,
            str(output.parent / stage_name),
        )
        try:
            _rename_noreplace(directory_fd, stage_name, output.name)
            stage_moved = True
        except FileExistsError:
            if not _existing_snapshot(directory_fd, output.name, output, digest):
                raise AssertionError("existing snapshot disappeared during inspection")
            raise SeriesError(
                "an identical scorecard snapshot appeared while a named staging "
                "file was held; refusing success because the staging pathname "
                "cannot be removed by inode"
            )
        except OSError as error:
            if error.errno != errno.ENOSYS:
                raise
            try:
                os.link(
                    stage_name,
                    output.name,
                    src_dir_fd=directory_fd,
                    dst_dir_fd=directory_fd,
                    follow_symlinks=False,
                )
            except FileExistsError:
                if not _existing_snapshot(directory_fd, output.name, output, digest):
                    raise AssertionError("existing snapshot disappeared during inspection")
                raise SeriesError(
                    "an identical scorecard snapshot appeared while the no-replace "
                    "move was unavailable; refusing success with named staging residue"
                )
            os.fsync(directory_fd)
            raise SeriesError(
                "scorecard snapshot was linked without replacement, but the atomic "
                "move primitive is unavailable; refusing success with named staging "
                "residue"
            )
        return _finish_snapshot_publication(directory_fd, output, digest)
    except Exception as error:
        if not stage_moved:
            raise SeriesError(
                f"{error}; named staging pathname was left in place: "
                f"{output.parent / stage_name}"
            ) from error
        raise
    finally:
        os.close(stage_fd)


def write_scorecard_projection_snapshot(parent: Any, output: Any) -> dict[str, Any]:
    """Atomically publish one run-scoped snapshot without replacing evidence."""
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    try:
        directory_fd = os.open(output.parent, directory_flags)
    except OSError as error:
        raise SeriesError(
            f"scorecard snapshot parent is not a regular directory: {output.parent}: {error}"
        ) from error
    try:
        directory_identity = _directory_identity(output.parent, directory_fd)
        with _scorecard_projection_capture(parent) as (snapshot, local):
            payload = _snapshot_bytes(snapshot)
            digest = hashlib.sha256(payload).hexdigest()
            local.verify()
            _publish_snapshot_payload(directory_fd, output, payload, digest)
            local.verify()
            if _directory_identity(output.parent, directory_fd) != directory_identity:
                raise SeriesError(
                    f"scorecard snapshot directory changed during publication: {output.parent}"
                )
            return {
                "state": "available",
                "path": str(output.absolute()),
                "sha256": digest,
                "source_commit": snapshot["source"]["commit"],
                "source_tree": snapshot["source"]["tree"],
                "rows_read": snapshot["rows_read"],
            }
    finally:
        os.close(directory_fd)


def test_id(row: dict) -> str | None:
    """The test part of the existing ``<test>/<mode>/<backend>`` cell id."""
    cell = str((row.get("series") or {}).get("cell", ""))
    parts = cell.rsplit("/", 2)
    return parts[0] if len(parts) == 3 else None


def history_rows(rows: Iterable[dict], test: str, tree: str | None = None) -> list[dict]:
    """All rows for one exact test id, ordered as they were emitted."""
    selected = [
        row for row in rows
        if test_id(row) == test
        and (tree is None or (row.get("series") or {}).get("tree") == tree)
    ]
    return sorted(
        selected,
        key=lambda row: (
            str(row.get("emitted_at", "")),
            str(row.get("run_id", "")),
            int((row.get("series") or {}).get("run_index", -1)),
            int((row.get("series") or {}).get("attempt", 1)),
            str(row.get("event_id", "")),
        ),
    )


def _git(checkout: Path, args: Sequence[str]) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    for name in _GIT_REPO_SCOPED_ENV:
        env.pop(name, None)
    return subprocess.run(
        ["git", "-C", str(checkout), *args],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
        env=env,
    )


def _is_checkout(checkout: Path) -> bool:
    """True only when git resolves *this directory* as the checkout root.

    An uninitialised submodule is an empty directory. ``git -C`` otherwise
    walks upward and would silently report the parent dev-hermit commit as the
    Hermit or Reverie commit.
    """
    if not checkout.is_dir():
        return False
    top = _git(checkout, ["rev-parse", "--show-toplevel"])
    if top.returncode != 0:
        return False
    try:
        return Path(top.stdout.strip()).resolve() == checkout.resolve()
    except OSError:
        return False


def main_ancestry(checkout: Path, trees: Iterable[str]) -> dict[str, tuple[bool | None, dict | None]]:
    """Resolve current ``origin/main`` ancestry and depth for each requested SHA.

    ``None`` is deliberately distinct from ``False``: an unavailable object or
    checkout did not establish that a tree is off main.
    """
    requested = sorted(set(trees))
    if not requested:
        return {}
    if not _is_checkout(checkout):
        return {tree: (None, None) for tree in requested}

    main = _git(checkout, ["rev-parse", "--verify", "origin/main^{commit}"])
    if main.returncode != 0:
        return {tree: (None, None) for tree in requested}

    resolved: dict[str, tuple[bool | None, dict | None]] = {}
    for tree in requested:
        if _SHA_RE.fullmatch(tree) is None:
            resolved[tree] = (None, None)
            continue
        present = _git(checkout, ["cat-file", "-e", f"{tree}^{{commit}}"])
        if present.returncode != 0:
            resolved[tree] = (None, None)
            continue
        ancestor = _git(checkout, ["merge-base", "--is-ancestor", tree, "origin/main"])
        if ancestor.returncode == 1:
            resolved[tree] = (False, None)
            continue
        if ancestor.returncode != 0:
            resolved[tree] = (None, None)
            continue
        resolved[tree] = (True, commit_depth(checkout, tree))
    return resolved


def commit_depth(checkout: Path, tree: str) -> dict | None:
    """The scorecard's existing ``{commits, first_parent}`` depth shape."""
    counts = {}
    for key, arguments in (
        ("commits", ["rev-list", "--count", tree]),
        ("first_parent", ["rev-list", "--count", "--first-parent", tree]),
    ):
        result = _git(checkout, arguments)
        value = result.stdout.strip()
        if result.returncode != 0 or not value.isdigit() or int(value) < 1:
            return None
        counts[key] = int(value)
    return counts


def source_depths(
    parent: Any, tree: str, *, hermit_checkout: Any | None = None,
) -> dict[str, dict]:
    """The scorecard's per-repository depths at write time."""
    parent = Path(parent)
    hermit = Path(hermit_checkout) if hermit_checkout is not None else parent / "hermit"
    depth = commit_depth(hermit, tree) if _is_checkout(hermit) else None
    if depth is None:
        raise SeriesError(f"cannot resolve Hermit source depth for {tree}")
    depths = {"hermit": depth}

    # Match scorecard.rs: Reverie is a sibling checkout whose presence depends
    # on the surrounding workspace, so omit it when it is unavailable rather
    # than inventing a depth. Its HEAD is the source state available at write
    # time; it is not a path inside the Hermit commit.
    reverie = parent / "reverie"
    if _is_checkout(reverie):
        reverie_depth = commit_depth(reverie, "HEAD")
        if reverie_depth is not None:
            depths["reverie"] = reverie_depth
    return depths


_COMPARISON_MODES = frozenset({"verify", "replay", "chaos"})
_KNOWN_CELL_MODES = _COMPARISON_MODES | {"naked", "custom"}


def _cell_mode(row: dict) -> str:
    mode = row.get("mode")
    if not isinstance(mode, str) or not mode:
        raise MalformedRow("cell result has no nonempty mode")
    if mode not in _KNOWN_CELL_MODES:
        raise MalformedRow(f"cell result has unknown mode {mode!r}")
    return mode


def _has_nonzero_process_disposition(attempt: dict) -> bool:
    status = attempt.get("status")
    signal = attempt.get("signal")
    return (
        isinstance(status, int)
        and not isinstance(status, bool)
        and status > 0
        and signal is None
    ) or (
        status is None
        and isinstance(signal, int)
        and not isinstance(signal, bool)
        and signal > 0
    )


def _typed_comparison_attempt(attempt: Any, *, mode: str, index: int) -> str:
    """Return matched/diverged/no_result/timeout/errored for one typed attempt."""
    if not isinstance(attempt, dict):
        raise MalformedRow(f"cell attempt {index} must be an object")
    raw = attempt.get("verification_report")
    if raw is None:
        if attempt.get("verification_report_sha256") is not None:
            raise MalformedRow(
                f"cell attempt {index} has a verification report hash without a report"
            )
        if (
            attempt.get("outcome") == "ERROR"
            and attempt.get("timed_out") is True
            and isinstance(attempt.get("error_kind"), str)
            and bool(attempt["error_kind"].strip())
            and _has_nonzero_process_disposition(attempt)
        ):
            return "timeout"
        raise MalformedRow(
            f"cell attempt {index} has no verification report and did not time out"
        )
    if not isinstance(raw, str):
        raise MalformedRow(f"cell attempt {index} verification report must be a string")
    recorded_sha = attempt.get("verification_report_sha256")
    if not isinstance(recorded_sha, str) or hashlib.sha256(raw.encode()).hexdigest() != recorded_sha:
        raise MalformedRow(f"cell attempt {index} verification report hash mismatch")
    try:
        report = json.loads(raw)
    except json.JSONDecodeError as error:
        raise MalformedRow(
            f"cell attempt {index} verification report is malformed: {error}"
        ) from error
    if not isinstance(report, dict):
        raise MalformedRow(f"cell attempt {index} verification report must be an object")

    verdict = report.get("verdict")
    if verdict in {"matched", "diverged"}:
        if (
            report.get("no_result_reason") is not None
            or report.get("infrastructure_error") is not None
        ):
            raise MalformedRow(
                f"cell attempt {index} comparison report is internally inconsistent"
            )
        comparison = report.get("comparison")
        counts = report.get("compared_log_messages")
        if not isinstance(comparison, dict):
            raise MalformedRow(f"cell attempt {index} has no comparison object")
        if not isinstance(counts, dict):
            raise MalformedRow(f"cell attempt {index} has no INFO-message counts")
        for field in (
            "strictness",
            "display_name",
            "compare_logs",
            "compare_io_buffers",
            "log_scope",
            "record_envelope",
            "virtualize_time",
            "strip_lines",
            "canonicalize_addresses",
            "full_trace",
            "exact_remainder",
            "stripped_prefixes",
            "canonicalizations",
            "ignore_lines",
            "skip_commit",
            "skip_detlog",
        ):
            if field not in comparison:
                raise MalformedRow(
                    f"cell attempt {index} comparison omitted {field!r}"
                )
        left = counts.get("left")
        right = counts.get("right")
        if (
            not isinstance(left, int)
            or isinstance(left, bool)
            or left <= 0
            or not isinstance(right, int)
            or isinstance(right, bool)
            or right <= 0
        ):
            raise MalformedRow(
                f"cell attempt {index} has no nonzero INFO-message counts"
            )
        if verdict == "matched" and (
            report.get("verified") is not True
            or report.get("bitwise_parity") is not True
        ):
            raise MalformedRow(
                f"cell attempt {index} matched report is internally inconsistent"
            )
        if verdict == "diverged" and (
            report.get("verified") is not False
            or report.get("bitwise_parity") is not False
        ):
            raise MalformedRow(
                f"cell attempt {index} divergence report is internally inconsistent"
            )
        expected_virtualize_time = mode != "replay"
        canonical = (
            comparison.get("strictness") == "canonical"
            and comparison.get("display_name") == "BitwiseInfoV1"
            and comparison.get("compare_logs") is True
            and comparison.get("compare_io_buffers") is True
            and comparison.get("log_scope") == "info"
            and comparison.get("record_envelope")
            in {"all_records_v1", "dbt_evidence_transport_v1"}
            and comparison.get("virtualize_time") is expected_virtualize_time
            and comparison.get("strip_lines") is False
            and comparison.get("canonicalize_addresses") is True
            and comparison.get("full_trace") is True
            and comparison.get("exact_remainder") is True
            and comparison.get("stripped_prefixes") == ["real-wall-clock-prefix/v1"]
            and comparison.get("canonicalizations")
            == ["host-address-to-first-appearance-ordinal/v1"]
            and comparison.get("ignore_lines") is False
            and comparison.get("skip_commit") is False
            and comparison.get("skip_detlog") is False
        )
        if not canonical:
            return "no_result"
        if verdict == "matched":
            status = attempt.get("status")
            if (
                report.get("verified") is not True
                or report.get("bitwise_parity") is not True
                or attempt.get("outcome") != "PASS"
                or attempt.get("timed_out") is not False
                or attempt.get("error_kind") is not None
                or not isinstance(status, int)
                or isinstance(status, bool)
                or status < 0
                or (mode != "chaos" and status != 0)
                or attempt.get("signal") is not None
            ):
                raise MalformedRow(
                    f"cell attempt {index} matched report is internally inconsistent"
                )
            return "matched"
        if report.get("verified") is not False or report.get("bitwise_parity") is not False:
            raise MalformedRow(
                f"cell attempt {index} divergence report is internally inconsistent"
            )
        return "diverged"

    if verdict == "no_result":
        reason = report.get("no_result_reason")
        contradiction_fields = (
            "first_divergent_scheduler_turn",
            "first_divergent_virtual_nanoseconds",
            "first_divergent_record",
            "first_divergent_syscall",
            "first_divergent_left_message",
            "first_divergent_right_message",
            "dbt_counted_branches",
        )
        if (
            report.get("verified") is not False
            or report.get("bitwise_parity") is not False
            or report.get("comparison") is not None
            or report.get("compared_log_messages") is not None
            or report.get("infrastructure_error") is not None
            or "no_result_reason" not in report
            or (reason is not None and not isinstance(reason, dict))
            or any(report.get(field) is not None for field in contradiction_fields)
            or any(attempt.get(field) is not None for field in contradiction_fields[:-1])
            or attempt.get("outcome") not in {"FAIL", "ERROR"}
        ):
            raise MalformedRow(
                f"cell attempt {index} no_result report is internally inconsistent"
            )
        kind = "unspecified" if reason is None else reason.get("kind")
        no_process_timeout = (
            kind == "not_run"
            and attempt.get("timed_out") is True
            and attempt.get("status") is None
            and attempt.get("signal") is None
            and attempt.get("error_kind")
            in ("incomplete-verification-evidence", "cpu-timeout", "wall-timeout")
        )
        if reason is None or kind == "comparison_refused":
            # These current-producer cases distinguish an explicit null cause
            # from missing evidence. Older typed reasons retain their existing
            # historical-reader rules below.
            def current_fields(pairs):
                fields = {}
                for name, value in pairs:
                    if name in fields:
                        raise MalformedRow(
                            f"cell attempt {index} current no_result report has duplicate field {name!r}"
                        )
                    fields[name] = value
                return fields

            # Inspect the original bytes: an ordinary dict has already lost
            # duplicate keys, including contradictory values and nested detail.
            report = json.loads(raw, object_pairs_hook=current_fields)
            reason = report["no_result_reason"]
            for field in (
                "verified", "bitwise_parity", "verdict", "infrastructure_error",
                "comparison", "no_result_reason", "compared_log_messages",
                "guest_exit_code", "guest_signal",
                "first_divergent_scheduler_turn", "first_divergent_virtual_nanoseconds",
                "first_divergent_record", "first_divergent_syscall",
                "first_divergent_left_message", "first_divergent_right_message",
            ):
                if field not in report:
                    raise MalformedRow(
                        f"cell attempt {index} current no_result report omitted {field!r}"
                    )
            for field, minimum in (("guest_exit_code", 0), ("guest_signal", 1)):
                value = report[field]
                if value is not None and (
                    not isinstance(value, int) or isinstance(value, bool)
                    or not minimum <= value <= 2**31 - 1
                ):
                    raise MalformedRow(
                        f"cell attempt {index} current no_result report has invalid {field}"
                    )
            if report["guest_exit_code"] is not None and report["guest_signal"] is not None:
                raise MalformedRow(
                    f"cell attempt {index} current no_result report has contradictory guest disposition"
                )
            if (
                attempt.get("outcome") != "ERROR"
                or attempt.get("timed_out") is not False
                or not isinstance(attempt.get("error_kind"), str)
                or not attempt["error_kind"].strip()
                or not _has_nonzero_process_disposition(attempt)
            ):
                raise MalformedRow(
                    f"cell attempt {index} {kind} disposition is incomplete"
                )
            if kind == "comparison_refused" and (
                not isinstance(reason.get("detail"), str)
                or not reason["detail"].strip()
                or attempt["error_kind"] != "incomplete-verification-evidence"
            ):
                raise MalformedRow(
                    f"cell attempt {index} comparison_refused detail or error kind is incomplete"
                )
        elif kind == "not_run":
            if (
                report.get("guest_exit_code") is not None
                or report.get("guest_signal") is not None
                or attempt.get("outcome") != "ERROR"
                or not isinstance(attempt.get("error_kind"), str)
                or not attempt["error_kind"].strip()
                or not (
                    _has_nonzero_process_disposition(attempt)
                    or no_process_timeout
                )
            ):
                raise MalformedRow(
                    f"cell attempt {index} NotRun report has no complete process or pre-launch timeout disposition"
                )
        elif kind == "first_run_rejected":
            exit_code = reason.get("exit_code")
            signal = reason.get("signal")
            if (
                (exit_code is None) == (signal is None)
                or report.get("guest_exit_code") != exit_code
                or report.get("guest_signal") != signal
                or attempt.get("outcome") != "FAIL"
                or attempt.get("timed_out") is not False
                or not isinstance(attempt.get("status"), int)
                or isinstance(attempt.get("status"), bool)
                or attempt["status"] <= 0
                or attempt.get("signal") is not None
                or attempt.get("error_kind") is not None
            ):
                raise MalformedRow(
                    f"cell attempt {index} first_run_rejected disposition is inconsistent"
                )
        else:
            raise MalformedRow(
                f"cell attempt {index} has unknown no_result reason {kind!r}"
            )
        if attempt.get("timed_out") is True:
            if (
                attempt.get("outcome") != "ERROR"
                or not isinstance(attempt.get("error_kind"), str)
                or not attempt["error_kind"].strip()
                or not (
                    _has_nonzero_process_disposition(attempt)
                    or no_process_timeout
                )
            ):
                raise MalformedRow(
                    f"cell attempt {index} timed-out no_result disposition is incomplete"
                )
            return "timeout"
        return "no_result"
    if verdict == "infrastructure_error":
        reason = report.get("infrastructure_error")
        if (
            not isinstance(reason, dict)
            or reason.get("kind") != "skid_overshoot"
            or not isinstance(reason.get("count"), int)
            or isinstance(reason.get("count"), bool)
            or attempt.get("outcome") != "ERROR"
            or attempt.get("timed_out") is not False
            or not _has_nonzero_process_disposition(attempt)
        ):
            raise MalformedRow(
                f"cell attempt {index} infrastructure_error report or process disposition is incomplete"
            )
        return "errored"
    raise MalformedRow(f"cell attempt {index} has unknown verdict {verdict!r}")


def _cell_outcome(row: dict) -> str:
    """Project the framework's exact result without reconstructing it."""
    mode = _cell_mode(row)
    result = row.get("result")
    failure_class = row.get("failure_class")
    classification = (result, failure_class)
    try:
        projected, allowed_cell_outcomes = {
            ("pass", None): ("passed", {"PASS"}),
            ("determinism-failure", "product_failure"): ("diverged", {"FAIL"}),
            ("parity-failure", "product_failure"): ("diverged", {"FAIL"}),
            ("replay-failure", "product_failure"): ("diverged", {"FAIL"}),
            ("crash-error", "product_failure"): ("errored", {"FAIL"}),
            ("timeout", "no_result"): ("timeout", {"FAIL", "ERROR"}),
            ("oom", "no_result"): ("no_result", {"FAIL"}),
            (None, "understood_infrastructure_failure"): ("errored", {"ERROR"}),
            (None, "understood_prerequisite_failure"): (
                "skipped", {"ERROR", "HOST-INAPPLICABLE"},
            ),
            (None, "no_result"): ("no_result", {"ERROR"}),
        }[classification]
    except (KeyError, TypeError) as error:
        raise MalformedRow(
            "cell result has no recognised result/failure_class pair: "
            f"result={result!r} failure_class={failure_class!r}"
        ) from error
    outcome = row.get("outcome")
    if outcome not in allowed_cell_outcomes:
        raise MalformedRow(
            "cell result outcome disagrees with its recorded classification: "
            f"outcome={outcome!r} result={result!r} failure_class={failure_class!r}"
        )
    if mode not in _COMPARISON_MODES:
        return projected

    attempts = row.get("attempts")
    if not isinstance(attempts, list) or not attempts:
        raise _UntypedComparisonCell("comparison cell result has no typed attempts")
    # The framework-owned result above decides the projection. Attempt reports
    # remain independently validated so malformed evidence cannot hide behind
    # a well-formed terminal classification, but an outer reader must not
    # reconstruct a competing verdict from attempt order.
    dispositions = [
        _typed_comparison_attempt(attempt, mode=mode, index=index)
        for index, attempt in enumerate(attempts, 1)
    ]
    # Two framework classifications intentionally project to ``errored`` but
    # arise at different layers.  A crash-error is a product failure before a
    # comparison verdict exists, so its canonical attempt is ``no_result``;
    # only an understood infrastructure failure carries ``errored`` attempt
    # evidence.  Key this obligation by the full classification rather than
    # collapsing it through the outward projection.
    supporting_dispositions = {
        ("pass", None): {"matched"},
        ("determinism-failure", "product_failure"): {"diverged"},
        ("parity-failure", "product_failure"): {"diverged"},
        ("replay-failure", "product_failure"): {"diverged"},
        ("crash-error", "product_failure"): {"no_result"},
        ("timeout", "no_result"): {"timeout"},
        ("oom", "no_result"): {"no_result", "timeout"},
        (None, "understood_infrastructure_failure"): {"errored"},
        (None, "understood_prerequisite_failure"): {"no_result", "timeout"},
        (None, "no_result"): {"no_result", "timeout"},
    }[classification]
    if not supporting_dispositions.intersection(dispositions):
        raise MalformedRow(
            "framework result has no supporting canonical typed attempt: "
            f"projected={projected!r} dispositions={dispositions!r}"
        )
    return projected


def _cell_result_evidence_sha256(row: dict) -> str:
    """Use the scorecard writer's stable identity for one retained CellResult."""
    row = _normalise_cell_result_for_evidence(row)
    evidence = {
        key: row.get(key)
        for key in (
            "run_id",
            "attempt",
            "hermit_sha",
            "binary_sha256",
            "test_sha256",
            "test",
            "category",
            "lane",
            "mode",
            "backend",
            "classification",
            "outcome",
            "log_level",
            "effective_args",
            "argv",
            "guest_argv",
            "env",
            "cwd",
            "shell_command",
            "relaxations",
            "attempts",
        )
    }
    # ResultRow's Rust type defaults an omitted timeout to zero before hashing,
    # and serde_json writes Unicode as UTF-8 rather than ASCII escapes.
    evidence["timeout_seconds"] = row.get("timeout_seconds", 0)
    encoded = json.dumps(
        evidence, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
    return hashlib.sha256(encoded.encode()).hexdigest()


def _rewrite_recorded_root(value: str, root: str) -> str:
    """Rewrite producing-root path tokens without aliasing sibling paths."""
    return _replace_evidence_root_token(value, root, RECORDED_ROOT)


def _replace_evidence_root_token(value: str, old: str, new: str) -> str:
    """Replace invocation path tokens, including option and PATH values."""
    def delimiter(character: str) -> bool:
        return character in _EVIDENCE_PATH_DELIMITERS

    start = 0
    pieces = []
    while True:
        found = value.find(old, start)
        if found < 0:
            pieces.append(value[start:])
            return "".join(pieces)
        end = found + len(old)
        preceding = value[found - 1 : found]
        following = value[end : end + 1]
        starts_at_boundary = not preceding or delimiter(preceding)
        ends_at_boundary = (
            not following or following == "/" or delimiter(following)
        )
        if starts_at_boundary and ends_at_boundary:
            pieces.extend((value[start:found], new))
            start = end
        else:
            pieces.append(value[start:end])
            start = end


def _rewrite_recorded_root_json(value: Any, root: str) -> Any:
    if isinstance(value, str):
        return _rewrite_recorded_root(value, root)
    if isinstance(value, list):
        return [_rewrite_recorded_root_json(item, root) for item in value]
    if isinstance(value, dict):
        return {
            key: _rewrite_recorded_root_json(item, root)
            for key, item in value.items()
        }
    return value


def _recorded_shell_quote(value: str) -> str:
    safe = "_@%+=:,./-"
    if value and all(
        character.isascii() and (character.isalnum() or character in safe)
        for character in value
    ):
        return value
    return "'" + value.replace("'", "'\"'\"'") + "'"


def _literal_shell_command(cwd: str, env: dict[str, str], argv: list[str]) -> str:
    words = ["cd", _recorded_shell_quote(cwd), "&&", "env"]
    words.extend(
        _recorded_shell_quote(f"{name}={value}")
        for name, value in sorted(env.items())
    )
    words.extend(_recorded_shell_quote(argument) for argument in argv)
    return " ".join(words)


def _normalise_cell_result_for_evidence(row: dict) -> dict:
    """Mirror scorecard ingestion before computing ResultRow evidence identity."""
    row = copy.deepcopy(row)
    root = row.get("cwd")
    if (
        not isinstance(root, str)
        or not root
        or root == RECORDED_ROOT
        or not root.startswith("/")
    ):
        return row

    for key in ("argv", "guest_argv", "effective_args"):
        values = row.get(key)
        if isinstance(values, list):
            row[key] = [
                _rewrite_recorded_root(value, root)
                if isinstance(value, str)
                else value
                for value in values
            ]
    env = row.get("env")
    if isinstance(env, dict):
        row["env"] = {
            name: _rewrite_recorded_root(value, root)
            if isinstance(value, str)
            else value
            for name, value in env.items()
        }
    attempts = row.get("attempts")
    if isinstance(attempts, list):
        row["attempts"] = [
            _rewrite_recorded_root_json(attempt, root)
            for attempt in attempts
        ]
        for attempt in row["attempts"]:
            if not isinstance(attempt, dict):
                continue
            attempt_cwd = attempt.get("cwd")
            attempt_argv = attempt.get("argv")
            attempt_env = attempt.get("env")
            if (
                isinstance(attempt_cwd, str)
                and isinstance(attempt_argv, list)
                and all(isinstance(value, str) for value in attempt_argv)
                and isinstance(attempt_env, dict)
                and all(
                    isinstance(name, str) and isinstance(value, str)
                    for name, value in attempt_env.items()
                )
            ):
                attempt["shell_command"] = _literal_shell_command(
                    attempt_cwd, attempt_env, attempt_argv
                )
    row["cwd"] = RECORDED_ROOT
    top_env = row.get("env")
    top_argv = row.get("argv")
    if (
        isinstance(top_env, dict)
        and all(
            isinstance(name, str) and isinstance(value, str)
            for name, value in top_env.items()
        )
        and isinstance(top_argv, list)
        and all(isinstance(value, str) for value in top_argv)
    ):
        row["shell_command"] = _literal_shell_command(
            row["cwd"], top_env, top_argv
        )
    return row


def _no_verdict_kind(attempt: dict, disposition: str) -> str:
    """Name the producer-validated reason without reclassifying the result."""
    raw = attempt.get("verification_report")
    if raw is None:
        if disposition != "timeout":
            raise MalformedRow("only a typed timeout may omit its verification report")
        return "missing_report_timeout"
    report = json.loads(raw)
    verdict = report.get("verdict")
    if verdict == "no_result":
        reason = report["no_result_reason"]
        return "unspecified" if reason is None else reason["kind"]
    if verdict == "infrastructure_error":
        return "infrastructure_error"
    if verdict in {"matched", "diverged"} and disposition == "no_result":
        return {
            "matched": "noncanonical_match",
            "diverged": "noncanonical_divergence",
        }[verdict]
    raise MalformedRow(
        f"typed {disposition} attempt has unsupported report verdict {verdict!r}"
    )


def _pressure_evidence(row: dict) -> dict | None:
    """Retain the inner history without replacing the framework's verdict.

    A terminal PASS can contain earlier adverse observations. The outward
    series outcome cannot prove first-attempt qualification after that history
    has been discarded. Each evidence object identifies one outer attempt;
    several declared successful inner subruns are valid and are not themselves
    proof that the outer attempt was retried.
    """
    attempts = row.get("attempts")
    if attempts is None or attempts == []:
        # Historical/noncomparison results may lack this evidence. They remain
        # reportable, but the promotion classifier cannot supply missing facts.
        return None
    if not isinstance(attempts, list):
        raise MalformedRow("pressure cell attempts must be an array")
    mode = _cell_mode(row)
    retained = []
    for index, attempt in enumerate(attempts, 1):
        if not isinstance(attempt, dict):
            raise MalformedRow(f"pressure inner attempt {index} must be an object")
        comparison = None
        if mode in _COMPARISON_MODES:
            disposition = _typed_comparison_attempt(attempt, mode=mode, index=index)
            raw = attempt.get("verification_report")
            if raw is not None:
                # The existing reader above verifies the report bytes, hash,
                # complete comparison policy and invocation disposition.
                report = json.loads(raw)
                verdict = report["verdict"]
                for name, minimum in (("guest_exit_code", 0), ("guest_signal", 1)):
                    value = report.get(name)
                    if value is not None and (
                        not isinstance(value, int) or isinstance(value, bool)
                        or not minimum <= value <= 2**31 - 1
                    ):
                        raise MalformedRow(f"pressure inner attempt {index} has invalid {name}")
                if report.get("guest_exit_code") is not None and report.get("guest_signal") is not None:
                    raise MalformedRow(f"pressure inner attempt {index} has contradictory guest disposition")
                if verdict == "infrastructure_error" and (
                    not 0 < report["infrastructure_error"]["count"] < 2**64
                    or report.get("verified") is not False
                    or report.get("bitwise_parity") is not False
                    or report.get("no_result_reason") is not None
                ):
                    raise MalformedRow(f"pressure inner attempt {index} has inconsistent infrastructure evidence")
                counts = report.get("compared_log_messages")
                if verdict in {"matched", "diverged"} and any(
                    counts[side] >= 2**64 for side in ("left", "right")
                ):
                    raise MalformedRow(f"pressure inner attempt {index} INFO counts exceed u64")
                canonical = disposition in {"matched", "diverged"}
                if verdict == "matched" and counts["left"] != counts["right"]:
                    # Historical projection accepts this shape, but unequal
                    # counts cannot prove a complete canonical match.
                    canonical = False
                comparison = {
                    "verdict": verdict,
                    "canonical": canonical,
                    "report_sha256": attempt["verification_report_sha256"],
                    "no_result_kind": (
                        _no_verdict_kind(attempt, disposition)
                        if verdict == "no_result" else None
                    ),
                }
        retained.append({
            "index": attempt.get("index"),
            "outcome": attempt.get("outcome"),
            "error_kind": attempt.get("error_kind"),
            "status": attempt.get("status"),
            "signal": attempt.get("signal"),
            "timed_out": attempt.get("timed_out"),
            "comparison": comparison,
        })
    return {
        "evidence_sha256": _cell_result_evidence_sha256(row),
        "attempts": retained,
    }


def _no_verdict_evidence(row: dict, projected_outcome: str) -> dict | None:
    """Retain typed non-comparison dispositions without reconstructing one."""
    mode = row.get("mode")
    if mode not in _COMPARISON_MODES or projected_outcome not in {
        "no_result", "timeout", "errored",
    }:
        return None
    attempts = row.get("attempts")
    if not isinstance(attempts, list) or not attempts:
        raise _UntypedComparisonCell("comparison cell result has no typed attempts")

    dispositions = []
    for ordinal, attempt in enumerate(attempts, 1):
        kind = _typed_comparison_attempt(attempt, mode=mode, index=ordinal)
        if kind in {"matched", "diverged"}:
            continue
        index = attempt.get("index")
        if not isinstance(index, str) or not index.strip():
            raise MalformedRow(
                f"cell attempt {ordinal} has no nonempty typed index"
            )
        timed_out = attempt.get("timed_out")
        if not isinstance(timed_out, bool):
            raise MalformedRow(
                f"cell attempt {ordinal} timed_out must be a bool"
            )
        error_kind = attempt.get("error_kind")
        if error_kind is not None and (
            not isinstance(error_kind, str) or not error_kind.strip()
        ):
            raise MalformedRow(
                f"cell attempt {ordinal} error_kind must be nonempty or null"
            )
        status = attempt.get("status")
        signal = attempt.get("signal")
        for name, value, minimum in (("status", status, 0), ("signal", signal, 1)):
            if value is not None and (
                not isinstance(value, int)
                or isinstance(value, bool)
                or value < minimum
            ):
                raise MalformedRow(
                    f"cell attempt {ordinal} {name} has invalid value {value!r}"
                )
        if status is not None and signal is not None:
            raise MalformedRow(
                f"cell attempt {ordinal} records both status and signal"
            )
        report_sha = attempt.get("verification_report_sha256")
        if report_sha is not None and (
            not isinstance(report_sha, str)
            or re.fullmatch(r"[0-9a-f]{64}", report_sha) is None
        ):
            raise MalformedRow(
                f"cell attempt {ordinal} has invalid verification report identity"
            )
        retained = {
            "index": index,
            "kind": _no_verdict_kind(attempt, kind),
            "attempt_outcome": attempt.get("outcome"),
            "disposition": kind,
            "error_kind": error_kind,
            "status": status,
            "signal": signal,
            "timed_out": timed_out,
            "verification_report_sha256": report_sha,
        }
        if retained["kind"] == "comparison_refused":
            report = json.loads(attempt["verification_report"])
            retained["detail"] = report["no_result_reason"]["detail"]
        dispositions.append(retained)
    if not dispositions:
        if projected_outcome == "errored" and row.get("result") == "crash-error":
            return None
        raise MalformedRow(
            "non-comparison cell result has no typed no-verdict attempt"
        )
    return {
        "evidence_sha256": _cell_result_evidence_sha256(row),
        "attempts": dispositions,
    }


def _cell_name(row: dict) -> str:
    test = row.get("test")
    if not isinstance(test, str) or not test:
        raise MalformedRow("cell result has no nonempty test")
    mode = _cell_mode(row)
    parts = [test, mode]
    backend = row.get("backend")
    if backend is None and mode == "naked":
        backend = "native"
    if not isinstance(backend, str) or not backend:
        raise MalformedRow("cell result has no nonempty backend")
    parts.append(backend)
    return "/".join(parts)


def _cell_coordinates(row: dict) -> dict | None:
    located = {}
    attempts = row.get("attempts")
    attempts = attempts if isinstance(attempts, list) else []
    for key in COORDINATE_KEYS:
        value = row.get(key)
        if not isinstance(value, int) or isinstance(value, bool):
            value = next((
                attempt.get(key)
                for attempt in attempts
                if isinstance(attempt, dict)
                and isinstance(attempt.get(key), int)
                and not isinstance(attempt.get(key), bool)
            ), None)
        if isinstance(value, int) and not isinstance(value, bool):
            located[key] = value
    return located or None


def _replace_at_path_boundary(value: str, old: str, new: str) -> str:
    """Replace a known path without consuming a sibling path's prefix."""
    start = 0
    pieces = []
    while True:
        found = value.find(old, start)
        if found < 0:
            pieces.append(value[start:])
            return "".join(pieces)
        end = found + len(old)
        preceding = value[found - 1 : found]
        following = value[end : end + 1]
        before_preceding = value[found - 2 : found - 1]
        opening_quote = preceding in "\"'" and (
            not before_preceding
            or before_preceding.isspace()
            or before_preceding in "=([{,:>"
        )
        starts_at_boundary = (
            not preceding
            or preceding.isspace()
            or preceding in "=([{,:"
            or opening_quote
        )
        ends_at_boundary = not following or following == "/" or (
            opening_quote and following == preceding
        )
        if starts_at_boundary and ends_at_boundary:
            pieces.extend((value[start:found], new))
            start = end
        else:
            pieces.append(value[start:end])
            start = end


def _cell_first_divergent_messages(row: dict) -> dict | None:
    attempts = row.get("attempts")
    attempts = attempts if isinstance(attempts, list) else []
    for candidate in [row, *attempts]:
        if not isinstance(candidate, dict):
            continue
        left = candidate.get("first_divergent_left_message")
        right = candidate.get("first_divergent_right_message")
        left = left if isinstance(left, str) and left else None
        right = right if isinstance(right, str) and right else None
        if left is not None or right is not None:
            messages = {"left": left, "right": right}
            cwd = row.get("cwd")
            if not isinstance(cwd, str) or _OWNER_PATH_NEEDLE not in cwd:
                return messages

            # The retained harness row is the exact local evidence and keeps
            # its absolute paths.  The published series is portable evidence:
            # retain the diagnostic text, but replace the exact execution cwd
            # with the same workspace token used by the validation ledger.
            # Do not infer a broader workspace or owner-home prefix: only the
            # CellResult cwd is producer-owned evidence, and normalizing a
            # sibling path would let an unknown owner path evade the guard.
            messages = {
                side: (
                    _replace_at_path_boundary(value, cwd, WORKSPACE_TOKEN)
                    if value is not None
                    else None
                )
                for side, value in messages.items()
            }
            return messages
    return None


def _cell_runtime(row: dict) -> dict:
    runtime = row.get("runtime")
    if runtime is None:
        runtime = {}
    elif isinstance(runtime, dict):
        runtime = dict(runtime)
    else:
        raise MalformedRow("cell result runtime must be an object or null")
    wall = row.get("duration_ms")
    if wall is None:
        wall = None
    elif not isinstance(wall, (int, float)) or isinstance(wall, bool) or wall < 0:
        raise MalformedRow(
            f"cell result duration_ms must be a non-negative number or null, got {wall!r}"
        )
    runtime["wall_time_min_ms"] = wall
    runtime["wall_time_max_ms"] = wall
    return runtime


def _compression_key(payload: dict) -> str:
    stable = dict(payload)
    stable.pop("run_index", None)
    stable.pop("last_run_index", None)
    stable.pop("num_runs", None)
    runtime = stable.get("runtime")
    if isinstance(runtime, dict):
        runtime = dict(runtime)
        runtime.pop("wall_time_min_ms", None)
        runtime.pop("wall_time_max_ms", None)
        stable["runtime"] = runtime
    return json.dumps(stable, sort_keys=True, separators=(",", ":"))


def _last_run_index(payload: dict) -> int:
    first = int(payload["run_index"])
    count = int(payload.get("num_runs", 1))
    return int(payload.get("last_run_index", first + count - 1))


def collapse_rows(rows: Sequence[dict]) -> list[dict]:
    """Fold exact equal executions while preserving retry rows and first-seen order."""
    ordered = sorted(
        rows,
        key=lambda row: (
            str((row.get("series") or {}).get("cell", "")),
            int((row.get("series") or {}).get("run_index", -1)),
            int((row.get("series") or {}).get("attempt", 1)),
        ),
    )
    collapsed: list[dict] = []
    collapsible: dict[tuple[str, str], int] = {}
    for row in ordered:
        payload = row["series"]
        key = (str(row.get("run_id", "")), _compression_key(payload))
        previous_index = None if "attempt" in payload else collapsible.get(key)
        if previous_index is not None:
            previous = collapsed[previous_index]
            previous_payload = previous["series"]
            added_runs = int(payload.get("num_runs", 1))
            previous_last = _last_run_index(previous_payload)
            added_last = _last_run_index(payload)
            previous_payload["num_runs"] = (
                int(previous_payload.get("num_runs", 1)) + added_runs
            )
            previous_payload["last_run_index"] = max(previous_last, added_last)
            previous_runtime = previous_payload.get("runtime") or {}
            runtime = payload.get("runtime") or {}
            walls = [
                previous_runtime.get("wall_time_min_ms"),
                previous_runtime.get("wall_time_max_ms"),
                runtime.get("wall_time_min_ms"),
                runtime.get("wall_time_max_ms"),
            ]
            walls = [
                value for value in walls
                if isinstance(value, (int, float)) and not isinstance(value, bool)
            ]
            if walls:
                previous_runtime["wall_time_min_ms"] = min(walls)
                previous_runtime["wall_time_max_ms"] = max(walls)
                previous_payload["runtime"] = previous_runtime
            continue
        collapsed.append(json.loads(json.dumps(row)))
        if "attempt" not in payload:
            collapsible[key] = len(collapsed) - 1
    return collapsed


def _rows_from_cell_results(
    cell_results: Sequence[dict], *, parent: Any, producer: str, run_id: str,
    tree: str, emitted_at: str, hermit_checkout: Any | None = None,
    rejected_untyped: list[dict] | None,
) -> list[dict]:
    """Convert typed harness results into validated, compressed series rows."""
    if producer not in {"validate", "pressure-test", "hermit-repeat"}:
        raise SeriesError(
            "producer must be validate, pressure-test, or hermit-repeat, "
            f"got {producer!r}"
        )
    if not run_id or "/" in run_id or run_id in (".", ".."):
        raise SeriesError(f"run_id must be a single path segment, got {run_id!r}")
    if _SHA_RE.fullmatch(tree) is None:
        raise MalformedRow(f"tree must be a 40-hex commit sha, got {tree!r}")
    source = Path(hermit_checkout) if hermit_checkout is not None else Path(parent) / "hermit"
    ancestry = main_ancestry(source, [tree]).get(tree, (None, None))[0]
    if ancestry is None:
        raise SeriesError(f"cannot resolve whether Hermit {tree} is on origin/main ancestry")
    depth = source_depths(parent, tree, hermit_checkout=source)
    recorded_detcore_tree = None

    rows = []
    pressure_counts: dict[tuple[str, int], int] = {}
    if producer in {"pressure-test", "hermit-repeat"}:
        for cell_result in cell_results:
            if cell_result.get("outcome") == "HOST-INAPPLICABLE":
                continue
            run_index = cell_result.get("run_index")
            if isinstance(run_index, int) and not isinstance(run_index, bool):
                key = (_cell_name(cell_result), run_index)
                pressure_counts[key] = pressure_counts.get(key, 0) + 1
    for input_row, cell_result in enumerate(cell_results, 1):
        # An unmet prerequisite is recorded by the validate-level ledger. It
        # produced no execution and therefore produces no cell observation.
        if cell_result.get("outcome") == "HOST-INAPPLICABLE":
            continue
        schema = cell_result.get("schema")
        if not isinstance(schema, int) or isinstance(schema, bool) or schema != 4:
            raise MalformedRow(f"cell result schema must be 4, got {schema!r}")
        source_tree_dirty = cell_result.get("source_tree_dirty")
        if not isinstance(source_tree_dirty, bool):
            raise MalformedRow(
                "cell result source_tree_dirty must be a bool, "
                f"got {source_tree_dirty!r}"
            )
        if cell_result.get("hermit_sha") != tree:
            raise MalformedRow(
                f"cell result Hermit sha {cell_result.get('hermit_sha')!r} does not match {tree}"
            )
        machine_shortname = cell_result.get("machine_shortname")
        if not isinstance(machine_shortname, str) or not machine_shortname:
            raise MalformedRow("cell result has no nonempty machine_shortname")
        kernel_version = cell_result.get("kernel_version")
        if not isinstance(kernel_version, str) or not kernel_version:
            raise MalformedRow("cell result has no nonempty kernel_version")
        host_capabilities = cell_result.get("host_capabilities")
        if not isinstance(host_capabilities, dict):
            raise MalformedRow("cell result has no host_capabilities object")
        if producer == "validate":
            run_index = cell_result.get("attempt")
            if (
                not isinstance(run_index, int)
                or isinstance(run_index, bool)
                or run_index < 1
            ):
                raise MalformedRow(
                    "validate cell result attempt must be a positive int, "
                    f"got {run_index!r}"
                )
        else:
            run_index = cell_result.get("run_index")
            if (
                not isinstance(run_index, int)
                or isinstance(run_index, bool)
                or run_index < 0
            ):
                raise MalformedRow(
                    "pressure-test cell result run_index must be a non-negative int, "
                    f"got {run_index!r}"
                )
            attempt = cell_result.get("attempt", 1)
            if (
                not isinstance(attempt, int)
                or isinstance(attempt, bool)
                or attempt < 1
            ):
                raise MalformedRow(
                    "pressure-test cell result attempt must be a positive int, "
                    f"got {attempt!r}"
                )
        detcore_tree = cell_result.get("detcore_tree")
        if detcore_tree is not None and (
            not isinstance(detcore_tree, str)
            or _SHA_RE.fullmatch(detcore_tree) is None
        ):
            raise MalformedRow(
                "cell result detcore_tree must be a 40-hex object id when present, "
                f"got {detcore_tree!r}"
            )
        runtime = _cell_runtime(cell_result)
        try:
            projected_outcome = _cell_outcome(cell_result)
        except _UntypedComparisonCell as error:
            if rejected_untyped is None or producer != "pressure-test":
                raise
            # Old retained pressure campaigns can contain one comparison cell
            # written before typed attempts were mandatory alongside many
            # current, fully typed cells. That untyped cell cannot qualify as a
            # measurement, but it must not erase the independent observations.
            # Keep the original batch in pressure_counts above so the identity
            # of every accepted row is unchanged by this rejection.
            rejected_untyped.append({
                "attempt": cell_result.get("attempt"),
                "backend": cell_result.get("backend"),
                "campaign_run_id": run_id,
                "cell_result_sha256": hashlib.sha256(json.dumps(
                    cell_result,
                    ensure_ascii=False,
                    sort_keys=True,
                    separators=(",", ":"),
                ).encode()).hexdigest(),
                "cell_result_run_id": cell_result.get("run_id"),
                "hermit_sha": cell_result.get("hermit_sha"),
                "input_row": input_row,
                "mode": cell_result.get("mode"),
                "reason": str(error),
                "run_index": cell_result.get("run_index"),
                "test": cell_result.get("test"),
                "tree": tree,
            })
            continue
        no_verdict_evidence = _no_verdict_evidence(cell_result, projected_outcome)
        payload = {
            "cell": _cell_name(cell_result),
            "tree": tree,
            "run_index": run_index,
            "num_runs": 1,
            "outcome": projected_outcome,
            "result": cell_result.get("result"),
            "failure_class": cell_result.get("failure_class"),
            "main_ancestry": ancestry,
            "depth": depth,
            "runtime": runtime,
            "source_tree_dirty": source_tree_dirty,
            "machine_shortname": machine_shortname,
            "kernel_version": kernel_version,
            "host_capabilities": host_capabilities,
        }
        if no_verdict_evidence is not None:
            # A validate result's run_index currently carries the outer attempt
            # ordinal too, but naming it explicitly prevents collapse_rows from
            # combining two equal retry rows and makes the identity unambiguous
            # to readers that do not know producer-specific run_index rules.
            payload["attempt"] = run_index if producer == "validate" else attempt
            payload["no_verdict_evidence"] = no_verdict_evidence
        if producer == "pressure-test":
            pressure_evidence = _pressure_evidence(cell_result)
            if pressure_evidence is not None:
                payload["attempt"] = attempt
                payload["pressure_evidence"] = pressure_evidence
        if detcore_tree is None and no_verdict_evidence is not None:
            if recorded_detcore_tree is None:
                resolved = _git(source, ["rev-parse", "--verify", f"{tree}:detcore"])
                recorded_detcore_tree = resolved.stdout.strip()
                if (
                    resolved.returncode != 0
                    or _SHA_RE.fullmatch(recorded_detcore_tree) is None
                ):
                    detail = resolved.stderr.strip() or resolved.stdout.strip() or "no output"
                    raise SeriesError(
                        f"cannot resolve Detcore tree recorded by Hermit {tree}: {detail}"
                    )
            detcore_tree = recorded_detcore_tree
        if detcore_tree is not None:
            payload["detcore_tree"] = detcore_tree
        if (
            producer == "pressure-test"
            and pressure_counts.get((payload["cell"], run_index), 0) > 1
        ):
            payload["attempt"] = attempt
        coordinates = _cell_coordinates(cell_result)
        if coordinates is not None and payload["outcome"] == "diverged":
            payload["coordinates"] = coordinates
        messages = _cell_first_divergent_messages(cell_result)
        if messages is not None and payload["outcome"] == "diverged":
            payload["first_divergent_messages"] = messages
        envelope = {
            "schema": ENVELOPE_VERSION,
            "event_id": "",
            "event_type": "series.observation",
            "emitted_at": emitted_at,
            "team": "hermit",
            "host": machine_shortname,
            "run_id": run_id,
            "producer": producer,
            "series": payload,
        }
        rows.append(envelope)

    collapsed = collapse_rows(rows)
    for row in collapsed:
        payload = row["series"]
        identity = json.dumps(
            {
                "run_id": run_id,
                "producer": producer,
                "cell": payload["cell"],
                "tree": payload["tree"],
                "run_index": payload["run_index"],
                **({"attempt": payload["attempt"]} if "attempt" in payload else {}),
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        row["event_id"] = "series-" + hashlib.sha256(identity.encode()).hexdigest()
    validate_rows(collapsed)
    return collapsed


def rows_from_cell_results(
    cell_results: Sequence[dict], *, parent: Any, producer: str, run_id: str,
    tree: str, emitted_at: str, hermit_checkout: Any | None = None,
) -> list[dict]:
    """Convert a complete typed harness batch or refuse it before writing."""
    return _rows_from_cell_results(
        cell_results,
        parent=parent,
        producer=producer,
        run_id=run_id,
        tree=tree,
        emitted_at=emitted_at,
        hermit_checkout=hermit_checkout,
        rejected_untyped=None,
    )


__all__ = [
    "COORDINATE_KEYS",
    "ENVELOPE_VERSION",
    "FIRST_DIVERGENT_MESSAGE_KEYS",
    "HOST_FACTS_ENVELOPE_VERSION",
    "LEGACY_ENVELOPE_VERSION",
    "READABLE_ENVELOPE_VERSIONS",
    "SERIES_SNAPSHOT_REF",
    "SERIES_SPOOL_LOCK_TIMEOUT_SECONDS",
    "MalformedRow",
    "SeriesError",
    "append_rows",
    "collapse_rows",
    "classify_pressure_sample",
    "commit_depth",
    "history_rows",
    "history_store_rows",
    "main_ancestry",
    "measurement_state",
    "observed_range",
    "parse_line",
    "published_rows",
    "read_scorecard_projection_snapshot",
    "row_identity",
    "rows_from_cell_results",
    "scorecard_projection_snapshot",
    "series_for",
    "series_spool_transition_lock",
    "shared_parent_root",
    "shard_path",
    "spool_dir",
    "source_depths",
    "store_paths",
    "test_id",
    "union",
    "validate_row",
    "validate_read_rows",
    "validate_rows",
    "verify_append_only",
    "write_scorecard_projection_snapshot",
]


# --------------------------------------------------------------------------
# Write-boundary CLI.
#
# Step 3 left this module a library with no entry point. The commands below are
# the Python projection and spool front door, while Hermit's shared `SeriesRow`
# is the write-boundary type invoked by `_validated_rows`.
#
# This is deliberately the only thing the CLI does. It does not publish, it does
# not read, and it never touches the tracked shard -- a producer writes the
# spool and publication stays a separate, reviewed step.
# --------------------------------------------------------------------------


def _batch_name(rows: Sequence[dict]) -> str:
    """Name the spool batch after the run that produced it.

    Deliberately NOT a timestamp or a pid. A campaign re-run against the same
    tree must land in the same file so `union_events` can dedup it by
    `event_id`; a clock-derived name would turn one logical batch into two and
    defeat the dedup this store already relies on.
    """
    run_ids = {str(r.get("run_id", "")) for r in rows}
    if len(run_ids) != 1:
        raise SeriesError(
            f"a batch must carry exactly one run_id, got {len(run_ids)}: "
            f"{sorted(run_ids)}"
        )
    run_id = run_ids.pop()
    if not run_id or "/" in run_id or run_id in (".", ".."):
        raise SeriesError(f"run_id must be a single path segment, got {run_id!r}")
    return f"{run_id}.jsonl"


def _stdin_rows() -> tuple[list[dict] | None, int]:
    rows = []
    for number, line in enumerate(sys.stdin.read().splitlines(), 1):
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as error:
            print(f"series: stdin line {number} is not JSON: {error}", file=sys.stderr)
            return None, 2
    return rows, 0


def _cmd_append(argv: list[str]) -> int:
    parent = None
    i = 0
    while i < len(argv):
        if argv[i] == "--parent" and i + 1 < len(argv):
            parent = argv[i + 1]
            i += 2
            continue
        return _usage(f"unknown argument {argv[i]!r}")
    if not parent:
        return _usage("append requires --parent <dev-hermit root>")

    rows, error = _stdin_rows()
    if rows is None:
        return error
    if not rows:
        # Not an error, and deliberately not silent: a campaign that measured
        # nothing must not read as a campaign that was never run.
        print("series: no rows on stdin; nothing appended", file=sys.stderr)
        return 0

    try:
        target = spool_dir(parent) / _batch_name(rows)
        target.parent.mkdir(parents=True, exist_ok=True)
        written = append_rows(target, rows)
    except (MalformedRow, SeriesError) as error:
        # The batch is refused WHOLE by append_rows. Say which row so the
        # producer can fix it rather than bisecting its own output.
        print(f"series: REFUSED the batch, nothing was written: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"series: cannot write the spool: {error}", file=sys.stderr)
        return 2

    print(f"series: appended {written} row(s) to {target}")
    return 0


def _cmd_append_cells(argv: list[str]) -> int:
    parent = None
    hermit_checkout = None
    producer = None
    run_id = None
    tree = None
    emitted_at = None
    i = 0
    while i < len(argv):
        if argv[i] == "--parent" and i + 1 < len(argv):
            parent = argv[i + 1]
        elif argv[i] == "--checkout" and i + 1 < len(argv):
            hermit_checkout = argv[i + 1]
        elif argv[i] == "--producer" and i + 1 < len(argv):
            producer = argv[i + 1]
        elif argv[i] == "--run-id" and i + 1 < len(argv):
            run_id = argv[i + 1]
        elif argv[i] == "--tree" and i + 1 < len(argv):
            tree = argv[i + 1]
        elif argv[i] == "--emitted-at" and i + 1 < len(argv):
            emitted_at = argv[i + 1]
        else:
            return _usage(f"unknown append-cells argument {argv[i]!r}")
        i += 2
    missing = [
        name for name, value in (
            ("--parent", parent),
            ("--producer", producer),
            ("--run-id", run_id),
            ("--tree", tree),
        ) if not value
    ]
    if missing:
        return _usage(f"append-cells requires {', '.join(missing)}")
    rows, error = _stdin_rows()
    if rows is None:
        return error
    if not rows:
        print("series: no cell results on stdin; nothing appended", file=sys.stderr)
        return 1
    emitted_at = emitted_at or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    rejected_untyped: list[dict] = []
    try:
        series_rows = _rows_from_cell_results(
            rows,
            parent=parent,
            producer=str(producer),
            run_id=str(run_id),
            tree=str(tree),
            emitted_at=emitted_at,
            hermit_checkout=hermit_checkout,
            rejected_untyped=(
                rejected_untyped if producer == "pressure-test" else None
            ),
        )
        if not series_rows:
            if rejected_untyped:
                for rejection in rejected_untyped:
                    print(
                        "series: REJECTED non-qualifying cell "
                        + json.dumps(rejection, sort_keys=True, separators=(",", ":")),
                        file=sys.stderr,
                    )
                print(
                    "series: REFUSED "
                    f"{len(rejected_untyped)} untyped comparison cell result(s); "
                    "no valid completed cell rows were available to retain",
                    file=sys.stderr,
                )
                return 1
            print(
                "series: no completed cell rows; unmet prerequisites remain in the validate record",
                file=sys.stderr,
            )
            return 0
        target = spool_dir(parent) / _batch_name(series_rows)
        target.parent.mkdir(parents=True, exist_ok=True)
        written = append_cell_rows(parent, target, series_rows)
    except (MalformedEvent, MalformedRow, SeriesError) as error:
        print(f"series: REFUSED the cell batch, nothing was written: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"series: cannot write the spool: {error}", file=sys.stderr)
        return 2
    represented = sum(int(row["series"].get("num_runs", 1)) for row in series_rows)
    print(
        f"series: appended {written} row(s) representing {represented} run(s) to {target}"
    )
    if rejected_untyped:
        for rejection in rejected_untyped:
            print(
                "series: REJECTED non-qualifying cell "
                + json.dumps(rejection, sort_keys=True, separators=(",", ":")),
                file=sys.stderr,
            )
        print(
            "series: REFUSED "
            f"{len(rejected_untyped)} untyped comparison cell result(s); "
            f"retained {len(series_rows)} valid series row(s) representing "
            f"{represented} run(s)",
            file=sys.stderr,
        )
        return 1
    return 0


def _depth_text(depth: Any) -> str:
    if not isinstance(depth, dict):
        return "-"
    repositories = []
    for name in sorted(depth):
        counts = depth.get(name)
        if not isinstance(counts, dict):
            continue
        commits = counts.get("commits")
        first_parent = counts.get("first_parent")
        if isinstance(commits, int) and isinstance(first_parent, int):
            repositories.append(f"{name}:{commits}/{first_parent}")
    return ",".join(repositories) or "-"


def _runtime_value(runtime: Any, key: str) -> str:
    if not isinstance(runtime, dict):
        return "-"
    values = []
    for run in ("run1", "run2"):
        part = runtime.get(run)
        value = part.get(key) if isinstance(part, dict) else None
        if isinstance(value, int) and not isinstance(value, bool):
            values.append(str(value))
        elif run == "run1":
            values.append("-")
    return "/".join(values) if values else "-"


def _wall_text(runtime: Any) -> str:
    if not isinstance(runtime, dict):
        return "-"
    minimum = runtime.get("wall_time_min_ms")
    maximum = runtime.get("wall_time_max_ms")
    if not isinstance(minimum, (int, float)) or isinstance(minimum, bool):
        return "-"
    if not isinstance(maximum, (int, float)) or isinstance(maximum, bool):
        return "-"
    return str(minimum) if minimum == maximum else f"{minimum}-{maximum}"


def _producer_text(producer: Any) -> str:
    if isinstance(producer, dict):
        tool = producer.get("tool")
        return str(tool) if tool else "-"
    return str(producer) if producer else "-"


def _run_span(payload: dict) -> str:
    first = payload.get("run_index")
    count = payload.get("num_runs", 1)
    if not isinstance(first, int) or not isinstance(count, int) or count < 1:
        return "-"
    attempt = payload.get("attempt")
    if isinstance(attempt, int) and not isinstance(attempt, bool):
        return f"{first} attempt {attempt}"
    if count == 1:
        return str(first)
    last = payload.get("last_run_index", first + count - 1)
    return f"{first}-{last} ({count})"


def _coordinate_text(payload: dict, coordinate: str) -> str:
    value = (payload.get("coordinates") or {}).get(coordinate)
    if isinstance(value, int) and not isinstance(value, bool):
        return str(value)
    return "-"


def _first_divergent_messages_text(payload: dict) -> str:
    messages = payload.get("first_divergent_messages")
    if not isinstance(messages, dict):
        return "-"
    return json.dumps(
        {side: messages.get(side) for side in FIRST_DIVERGENT_MESSAGE_KEYS},
        sort_keys=True,
        separators=(",", ":"),
    )


def _cmd_history(argv: list[str]) -> int:
    parent = Path(__file__).resolve().parents[2]
    wanted_test = None
    wanted_tree = None
    i = 0
    while i < len(argv):
        if argv[i] == "--parent" and i + 1 < len(argv):
            parent = Path(argv[i + 1])
            i += 2
            continue
        if argv[i] == "--test" and i + 1 < len(argv):
            wanted_test = argv[i + 1]
            i += 2
            continue
        if argv[i] == "--tree" and i + 1 < len(argv):
            wanted_tree = argv[i + 1]
            i += 2
            continue
        return _usage(f"unknown history argument {argv[i]!r}")
    if not wanted_test:
        return _usage("history requires --test <test id>")
    if wanted_tree is not None and _SHA_RE.fullmatch(wanted_tree) is None:
        return _usage("history --tree must be a full lowercase 40-hex commit sha")

    try:
        rows = history_rows(history_store_rows(parent), wanted_test, wanted_tree)
    except (MalformedEvent, SeriesError, OSError, subprocess.SubprocessError) as error:
        print(f"series: cannot read history: {error}", file=sys.stderr)
        return 2
    if not rows:
        suffix = f" at {wanted_tree}" if wanted_tree else ""
        print(
            f"series: no runs recorded for test {wanted_test!r}{suffix}",
            file=sys.stderr,
        )
        return 1

    trees = [str((row.get("series") or {}).get("tree", "")) for row in rows]
    hermit_checkout = parent / "hermit"
    if _is_checkout(hermit_checkout):
        refresh_origin_main(hermit_checkout, "Hermit")
    current = main_ancestry(hermit_checkout, trees)
    represented = sum(int((row.get("series") or {}).get("num_runs", 1)) for row in rows)
    print(
        f"test={wanted_test} rows={len(rows)} runs={represented} "
        "source=parent-origin-main+local-spool"
    )
    by_cell: dict[str, dict[str, Any]] = {}
    for row in rows:
        payload = row.get("series") or {}
        cell = str(payload.get("cell", "-"))
        totals = by_cell.setdefault(cell, {"runs": 0, "minimum": None, "maximum": None})
        totals["runs"] += int(payload.get("num_runs", 1))
        runtime = payload.get("runtime") or {}
        minimum = runtime.get("wall_time_min_ms")
        maximum = runtime.get("wall_time_max_ms")
        if isinstance(minimum, (int, float)) and not isinstance(minimum, bool):
            totals["minimum"] = (
                minimum if totals["minimum"] is None else min(totals["minimum"], minimum)
            )
        if isinstance(maximum, (int, float)) and not isinstance(maximum, bool):
            totals["maximum"] = (
                maximum if totals["maximum"] is None else max(totals["maximum"], maximum)
            )
    for cell, totals in sorted(by_cell.items()):
        minimum = totals["minimum"] if totals["minimum"] is not None else "-"
        maximum = totals["maximum"] if totals["maximum"] is not None else "-"
        print(
            f"cell={cell}\truns={totals['runs']}\t"
            f"wall_time_min_ms={minimum}\twall_time_max_ms={maximum}"
        )
    print(
        "day\tsha\tmain(current/recorded)\tdepth(repo:commits/first-parent)\tcell\toutcome\t"
        "result\tfailure_class\truns\t"
        "turns(run1/run2)\tvirtual_ns(run1/run2)\tsyscalls(run1/run2)\t"
        "wall_ms(min-max)\tfirst_divergent_record\tfirst_divergent_syscall\t"
        "first_divergent_scheduler_turn\tfirst_divergent_virtual_nanoseconds\t"
        "first_divergent_messages\tproducer\trun_id"
    )
    for row in rows:
        payload = row.get("series") or {}
        tree = str(payload.get("tree", ""))
        on_main, current_depth = current.get(tree, (None, None))
        recorded = payload.get("main_ancestry")
        current_text = "yes" if on_main is True else "no" if on_main is False else "unknown"
        recorded_text = (
            "yes" if recorded is True else "no" if recorded is False else "unknown"
        )
        main_text = f"{current_text}/{recorded_text}"
        depth = dict(payload.get("depth") or {})
        if current_depth is not None:
            depth["hermit"] = current_depth
        runtime = payload.get("runtime")
        emitted_at = str(row.get("emitted_at", ""))
        day = emitted_at[:10] if len(emitted_at) >= 10 else "-"
        print(
            "\t".join([
                day,
                tree or "-",
                main_text,
                _depth_text(depth),
                str(payload.get("cell", "-")),
                str(payload.get("outcome", "-")),
                str(payload.get("result") or "-"),
                str(payload.get("failure_class") or "-"),
                _run_span(payload),
                _runtime_value(runtime, "scheduler_turns"),
                _runtime_value(runtime, "virtual_nanoseconds"),
                _runtime_value(runtime, "syscalls"),
                _wall_text(runtime),
                _coordinate_text(payload, "first_divergent_record"),
                _coordinate_text(payload, "first_divergent_syscall"),
                _coordinate_text(payload, "first_divergent_scheduler_turn"),
                _coordinate_text(payload, "first_divergent_virtual_nanoseconds"),
                _first_divergent_messages_text(payload),
                _producer_text(row.get("producer")),
                str(row.get("run_id", "-")),
            ])
        )
    return 0


def _cmd_snapshot(argv: list[str]) -> int:
    parent = None
    output = None
    i = 0
    while i < len(argv):
        if argv[i] == "--parent" and i + 1 < len(argv):
            parent = argv[i + 1]
            i += 2
            continue
        if argv[i] == "--output" and i + 1 < len(argv):
            output = argv[i + 1]
            i += 2
            continue
        return _usage(f"unknown snapshot argument {argv[i]!r}")
    missing = [
        name
        for name, value in (("--parent", parent), ("--output", output))
        if not value
    ]
    if missing:
        return _usage(f"snapshot requires {', '.join(missing)}")
    try:
        report = write_scorecard_projection_snapshot(parent, output)
    except (MalformedEvent, SeriesError, OSError, subprocess.SubprocessError) as error:
        print(
            "series: snapshot unavailable; no new snapshot was published: "
            f"{error}",
            file=sys.stderr,
        )
        return 1
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


def _usage(message: str) -> int:
    print(f"series: {message}", file=sys.stderr)
    print(
        "usage:\n"
        "  series.py append --parent <dev-hermit root>  < rows.jsonl\n"
        "  series.py append-cells --parent <root> "
        "--producer <validate|pressure-test|hermit-repeat>\n"
        "      --run-id <id> --tree <sha> [--checkout <Hermit checkout>]\n"
        "      [--emitted-at <UTC>] < cell-results.jsonl\n"
        "  series.py history --test <test id> [--tree <sha>] [--parent <root>]\n"
        "  series.py snapshot --parent <root> --output <run-scoped.json>\n"
        "append validates complete series rows. append-cells converts typed harness\n"
        "results, compresses exact equal runs, and writes the UNPUBLISHED spool.\n"
        "history freshly reads tracked shards from parent origin/main plus that local spool.\n"
        "snapshot atomically writes that canonical union with immutable source identities;\n"
        "an unavailable source is refused and never replaced by stale tracked output.",
        file=sys.stderr,
    )
    return 2


def main(argv: list[str]) -> int:
    if not argv:
        return _usage("a subcommand is required")
    if argv[0] == "append":
        return _cmd_append(argv[1:])
    if argv[0] == "append-cells":
        return _cmd_append_cells(argv[1:])
    if argv[0] == "history":
        return _cmd_history(argv[1:])
    if argv[0] == "snapshot":
        return _cmd_snapshot(argv[1:])
    return _usage(f"unknown subcommand {argv[0]!r}")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
