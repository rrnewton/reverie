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

import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
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

LEGACY_ENVELOPE_VERSION = "stress-series/v1"
HOST_FACTS_ENVELOPE_VERSION = "stress-series/v2"
ENVELOPE_VERSION = "stress-series/v3"
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

_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
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


def append_rows(path: Any, rows: Iterable[dict]) -> int:
    """Validate then append. Refuses the batch before writing any of it."""
    checked = validate_rows(rows)
    return append_events(path, checked)


def append_cell_rows(parent: Any, path: Any, rows: Iterable[dict]) -> int:
    """Append projected cell rows once, refusing a changed repeated identity."""
    path = Path(path)
    checked = validate_rows(rows)
    path.parent.mkdir(parents=True, exist_ok=True)
    lock_fd = os.open(path.parent / ".append.lock", os.O_WRONLY | os.O_CREAT, 0o644)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
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
    finally:
        os.close(lock_fd)


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
            or not isinstance(reason, dict)
            or any(report.get(field) is not None for field in contradiction_fields)
            or any(attempt.get(field) is not None for field in contradiction_fields[:-1])
            or attempt.get("outcome") not in {"FAIL", "ERROR"}
        ):
            raise MalformedRow(
                f"cell attempt {index} no_result report is internally inconsistent"
            )
        kind = reason.get("kind")
        if kind == "not_run":
            if report.get("guest_exit_code") is not None or report.get("guest_signal") is not None:
                raise MalformedRow(
                    f"cell attempt {index} not_run report records a guest disposition"
                )
        elif kind == "first_run_rejected":
            exit_code = reason.get("exit_code")
            signal = reason.get("signal")
            if (
                (exit_code is None) == (signal is None)
                or report.get("guest_exit_code") != exit_code
                or report.get("guest_signal") != signal
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
                or not _has_nonzero_process_disposition(attempt)
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
        ):
            raise MalformedRow(
                f"cell attempt {index} infrastructure_error report omitted its reason"
            )
        return "errored"
    raise MalformedRow(f"cell attempt {index} has unknown verdict {verdict!r}")


def _cell_outcome(row: dict) -> str:
    """Project the framework's exact result without reconstructing it."""
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
            ("timeout", "no_result"): ("timeout", {"FAIL"}),
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
    mode = row.get("mode")
    if mode not in _COMPARISON_MODES:
        return projected

    attempts = row.get("attempts")
    if not isinstance(attempts, list) or not attempts:
        raise MalformedRow("comparison cell result has no typed attempts")
    # The framework-owned result above decides the projection. Attempt reports
    # remain independently validated so malformed evidence cannot hide behind
    # a well-formed terminal classification, but an outer reader must not
    # reconstruct a competing verdict from attempt order.
    [
        _typed_comparison_attempt(attempt, mode=mode, index=index)
        for index, attempt in enumerate(attempts, 1)
    ]
    return projected


def _cell_name(row: dict) -> str:
    parts = []
    for key in ("test", "mode"):
        value = row.get(key)
        if not isinstance(value, str) or not value:
            raise MalformedRow(f"cell result has no nonempty {key}")
        parts.append(value)
    backend = row.get("backend")
    if backend is None and row.get("mode") == "naked":
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


def rows_from_cell_results(
    cell_results: Sequence[dict], *, parent: Any, producer: str, run_id: str,
    tree: str, emitted_at: str, hermit_checkout: Any | None = None,
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
    for cell_result in cell_results:
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
        payload = {
            "cell": _cell_name(cell_result),
            "tree": tree,
            "run_index": run_index,
            "num_runs": 1,
            "outcome": _cell_outcome(cell_result),
            "result": cell_result.get("result"),
            "failure_class": cell_result.get("failure_class"),
            "main_ancestry": ancestry,
            "depth": depth,
            "runtime": _cell_runtime(cell_result),
            "source_tree_dirty": source_tree_dirty,
            "machine_shortname": machine_shortname,
            "kernel_version": kernel_version,
            "host_capabilities": host_capabilities,
        }
        detcore_tree = cell_result.get("detcore_tree")
        if detcore_tree is not None:
            if not isinstance(detcore_tree, str) or _SHA_RE.fullmatch(detcore_tree) is None:
                raise MalformedRow(
                    "cell result detcore_tree must be a 40-hex object id when present, "
                    f"got {detcore_tree!r}"
                )
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


__all__ = [
    "COORDINATE_KEYS",
    "ENVELOPE_VERSION",
    "FIRST_DIVERGENT_MESSAGE_KEYS",
    "HOST_FACTS_ENVELOPE_VERSION",
    "LEGACY_ENVELOPE_VERSION",
    "READABLE_ENVELOPE_VERSIONS",
    "MalformedRow",
    "SeriesError",
    "append_rows",
    "collapse_rows",
    "commit_depth",
    "history_rows",
    "history_store_rows",
    "main_ancestry",
    "measurement_state",
    "observed_range",
    "parse_line",
    "published_rows",
    "row_identity",
    "rows_from_cell_results",
    "series_for",
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
    try:
        series_rows = rows_from_cell_results(
            rows,
            parent=parent,
            producer=str(producer),
            run_id=str(run_id),
            tree=str(tree),
            emitted_at=emitted_at,
            hermit_checkout=hermit_checkout,
        )
        if not series_rows:
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
        "append validates complete series rows. append-cells converts typed harness\n"
        "results, compresses exact equal runs, and writes the UNPUBLISHED spool.\n"
        "history freshly reads tracked shards from parent origin/main plus that local spool.",
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
    return _usage(f"unknown subcommand {argv[0]!r}")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
