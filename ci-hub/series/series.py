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

import base64
import copy
import contextlib
import ctypes
import errno
import fcntl
import hashlib
import importlib.util
import json
import math
import os
import re
import secrets
import shutil
import socket
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from collections.abc import Callable, Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "ledger"))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "lib"))

import rust_toolchain  # noqa: E402

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
LEDGER_REPOSITORY = "https://github.com/rrnewton/hermit_test_ledger.git"
PARENT_REPOSITORY = "https://github.com/rrnewton/dev-hermit.git"
LEDGER_ROOT_ENV = "DEV_HERMIT_TEST_LEDGER_ROOT"
SERIES_SPOOL_LOCK_TIMEOUT_SECONDS = 5.0
#: Retaining a refused input waits longer than a row append: the alternative to
#: waiting out a concurrent quarantine publication is losing the input's bytes.
QUARANTINE_LOCK_TIMEOUT_SECONDS = 120.0
SCORECARD_SNAPSHOT_MAX_DIRECTORY_ENTRIES = 4096
SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS = 1.0
SCORECARD_SNAPSHOT_USAGE = """usage:
  series.py snapshot --parent <dev-hermit root> --output <run-scoped.json>

Create one immutable scorecard-series-snapshot/v1 file from the exact parent
origin/main series tree and the bounded local series sources.

options:
  --parent <dev-hermit root>  workspace containing the ledger checkout and local spool
  --output <run-scoped.json>  new snapshot path; an existing different file refuses
  -h, --help                  show this help and exit successfully"""
#: `series.py append-parity`'s valued flags and the option each sets. Its
#: usage below names every one; a test holds the two together.
APPEND_PARITY_FLAGS = {
    "--parent": "parent", "--producer": "producer", "--run-id": "run_id",
    "--tree": "tree", "--emitted-at": "emitted_at",
    "--source-tree-dirty": "source_tree_dirty",
}
APPEND_PARITY_USAGE = """usage:
  series.py append-parity --parent <dev-hermit root> --producer <validate|pressure-test>
      --run-id <id> --tree <40-hex sha> --source-tree-dirty <true|false>
      [--emitted-at <YYYY-MM-DDTHH:MM:SSZ>] < parity-ledger-sources.jsonl

Spool one run's parity rows, one hermit ParityLedgerSource JSON object per
stdin line, as one batch behind a run marker in the parent's UNPUBLISHED
parity spool. Empty stdin records a zero-record marker. When the call names a
valid producer, run and tree, refused rows or a missing or invalid
--source-tree-dirty are spooled as a parity.refusal carrying the message.

options:
  --parent <dev-hermit root>           workspace whose local parity spool receives the batch
  --producer <validate|pressure-test>  the producer that measured the rows
  --run-id <id>                        the producing run; a row naming another run is refused
  --tree <40-hex sha>                  the Hermit tree; a row naming another tree is refused
  --source-tree-dirty <true|false>     whether that tree had uncommitted changes; never defaulted
  --emitted-at <YYYY-MM-DDTHH:MM:SSZ>  the batch's emission time in UTC; defaults to now
  -h, --help                           show this help and exit successfully

exit status: 0 when the batch was written or was already recorded identically,
1 when the rows were refused, 2 for a usage error or an unwritable spool.
Help alone writes nothing; help with any other argument is a usage error."""
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
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_PROJECTION_RESULTS_SCHEMA_VERSION = 1
_WRITE_RESULTS_SCHEMA_VERSION = 1
QUARANTINE_SCHEMA = "series-quarantine/v2"
# Written by series.py from f44dd42e0d until v2 replaced it. It stored the
# PARSED object and a digest of its canonical re-serialization, never the
# received bytes, so it is upgraded on publication and labelled as such.
LEGACY_QUARANTINE_SCHEMA = "series-quarantine/v1"
QUARANTINE_EVENT_TYPE = "series.quarantine"
QUARANTINE_SUMMARY_SCHEMA = "series-quarantine-summary/v1"
QUARANTINE_STAGES = frozenset({
    "parse", "projection", "shared-checker", "spool-conflict", "batch", "writer-error",
})
#: Exact records whose content cannot be published without an owner path.
QUARANTINE_HELD_SUBDIR = "held"
#: Top-level quarantine files the publisher could not read, moved aside whole so
#: the readable ones still publish. Never republished or deleted; the backlog
#: gate counts every file here as held back from publication and alarms.
QUARANTINE_CORRUPT_SUBDIR = "corrupt"
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
    # Older snapshots named the parent repository implicitly.
    repository: str = PARENT_REPOSITORY


@dataclass(frozen=True)
class ProjectionResults:
    """Hermit's projection result for every row in one canonical input."""

    row_count: int
    input_jsonl_sha256: str
    results: dict[str, str | None]


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


def quarantine_dir(parent: Any) -> Path:
    """The live, UNPUBLISHED spool of refused series inputs.

    A cell result the writer refuses is kept here with its exact received bytes
    and a typed reason, instead of being discarded or taking its valid siblings
    with it. The ledger publisher (``publisher.py --store series-quarantine``,
    run by ``mirror.py`` beside every series publication) appends these records
    to ``quarantine/series/<team>/<host>/<YYYY-MM>.jsonl`` in the test ledger,
    outside the ``series/`` tree that every series reader globs. Its records are
    typed ``series-quarantine/v2``, never ``SeriesRow``.
    """
    return shared_parent_root(parent) / "ignored" / "ci-hub" / "series-quarantine"


def quarantine_held_dir(parent: Any) -> Path:
    """Exact refused inputs whose content cannot be published portably.

    The test ledger is public and carries no owner path. A record that cannot
    be made public by owner-home token substitution is published as a
    content-free stub (digest, length, reason). Its exact bytes then exist only
    here, on this host: the ledger cannot restore them. Nothing deletes this
    directory, and ``ledger_spool_backlog --store series-quarantine`` alarms
    while it holds anything.
    """
    return quarantine_dir(parent) / QUARANTINE_HELD_SUBDIR


def quarantine_shard_path(team: str, host: str, month: str) -> str:
    """`quarantine/series/<team>/<host>/<YYYY>-<MM>.jsonl` in the test ledger.

    Deliberately outside `series/`: the scorecard, the website, and the
    published-snapshot reader all read `series/*/*/*.jsonl` (the scorecard
    recursively), and a quarantine record is not a `SeriesRow`.
    """
    return "quarantine/" + shard_path(team, host, month)


PARITY_SCHEMA = "parity-ledger/v1"
_PARITY_LEDGER_MODULE = "ci_hub_series_parity_ledger"


def parity_ledger_module():
    """The parity ledger store (`parity_ledger.py`), loaded once per process.

    Every loader (this module, the ledger publisher, the mirror, the website
    exporter) shares this one module object, so `ParityLedgerError` is one
    class wherever it is caught. The module is registered before it runs:
    a dataclass looks its module up in `sys.modules` while it is defined.
    """
    module = sys.modules.get(_PARITY_LEDGER_MODULE)
    if module is not None:
        return module
    path = Path(__file__).resolve().parent / "parity_ledger.py"
    spec = importlib.util.spec_from_file_location(_PARITY_LEDGER_MODULE, path)
    if spec is None or spec.loader is None:
        raise SeriesError(f"cannot load the parity ledger store from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[_PARITY_LEDGER_MODULE] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(_PARITY_LEDGER_MODULE, None)
        raise
    return module


def parity_spool_dir(parent: Any) -> Path:
    """The live, UNPUBLISHED spool of parity batches, one file per run.

    `series.py append-parity` writes here; the ledger publisher
    (`publisher.py --store parity`, run by `mirror.py` in the same commit as
    the series rows) appends each batch to `parity/<team>/<host>/<YYYY-MM>.jsonl`
    and its run marker to `parity-runs/<team>/<host>/<YYYY-MM>.jsonl`. Both
    trees are outside the `series/` glob. See `parity_ledger.py` and
    https://github.com/rrnewton/hermit/issues/3301.
    """
    return shared_parent_root(parent) / "ignored" / "ci-hub" / "parity-spool"


def parity_shard_path(team: str, host: str, month: str) -> str:
    """`parity/<team>/<host>/<YYYY-MM>.jsonl`: where a run's parity rows go."""
    return parity_ledger_module().shard_path("parity", team, host, month)


def parity_run_shard_path(team: str, host: str, month: str) -> str:
    """`parity-runs/<team>/<host>/<YYYY-MM>.jsonl`: one marker per appended run."""
    return parity_ledger_module().shard_path("parity-runs", team, host, month)


def ledger_root(parent: Any) -> Path:
    """Resolve the existing cell ledger; never create or silently replace it."""
    state = shared_parent_root(parent)
    configured = _git(state, [
        "config", "--file", str(state / ".gitmodules"), "--get",
        "submodule.hermit_test_ledger.url",
    ])
    if configured.returncode or configured.stdout.strip() != LEDGER_REPOSITORY:
        raise SeriesError("hermit_test_ledger submodule URL is absent or unexpected")
    override = os.environ.get(LEDGER_ROOT_ENV)
    root = Path(override).expanduser().resolve() if override else state / "hermit_test_ledger"
    if not _is_checkout(root):
        raise SeriesError(f"cell ledger checkout is unavailable: {root}")
    # get-url expands transport rewrites; the configured identity remains the
    # ledger URL even when a proxy or a local test remote supplies transport.
    remote = _git(root, ["config", "--get", "remote.origin.url"])
    if remote.returncode or remote.stdout.strip() not in {
        LEDGER_REPOSITORY, "git@github.com:rrnewton/hermit_test_ledger.git",
    }:
        raise SeriesError(f"cell ledger checkout has an unexpected origin: {root}")
    return root


def published_repository(parent: Any, repository: str) -> Path:
    """Keep historical parent SHAs in the repository that originally owned them."""
    if repository == PARENT_REPOSITORY:
        return Path(parent).resolve()
    if repository == LEDGER_REPOSITORY:
        return ledger_root(parent)
    raise SeriesError(f"unsupported series repository identity: {repository!r}")


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


def write_admission(rows: Sequence[Any]) -> list[str | None]:
    """Return Hermit's write-boundary verdict for each row independently.

    ``None`` admits the row; a string is the exact refusal. Unlike
    ``validate_rows``, one refused row does not decide the fate of the rows
    beside it: the shared checker's ``--write-results`` mode judges every row
    in one invocation. A checker that cannot run, or that returns a result
    envelope that does not account for exactly these rows, raises
    ``SeriesError``; that is a tool failure, not a verdict about any row.
    """
    verdicts: list[str | None] = [None] * len(rows)
    candidates: list[tuple[int, dict]] = []
    for index, row in enumerate(rows):
        try:
            copied = _checked_rows([row], strict_paths=True)[0]
            _canonical_row_json(copied)
        except MalformedRow as error:
            verdicts[index] = str(error)
            continue
        candidates.append((index, copied))
    if not candidates:
        return verdicts
    checker_input = _checker_input(row for _, row in candidates)
    expected_sha256 = hashlib.sha256(checker_input.encode()).hexdigest()
    result = _run_shared_checker(checker_input, ["--write-results"])
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise SeriesError(f"shared SeriesRow write checker failed: {detail}")
    try:
        output = json.loads(result.stdout)
    except (json.JSONDecodeError, TypeError, RecursionError) as error:
        raise SeriesError(
            f"shared SeriesRow write checker returned malformed JSON: {error}"
        ) from error
    if not isinstance(output, dict) or set(output) != {
        "input_jsonl_sha256", "input_row_count", "results", "schema_version",
    }:
        raise SeriesError("shared SeriesRow write checker returned an invalid top-level shape")
    if (
        type(output["schema_version"]) is not int
        or output["schema_version"] != _WRITE_RESULTS_SCHEMA_VERSION
    ):
        raise SeriesError("shared SeriesRow write checker returned an unsupported schema_version")
    if type(output["input_row_count"]) is not int or output["input_row_count"] != len(candidates):
        raise SeriesError("shared SeriesRow write checker input_row_count disagrees with rows")
    if output["input_jsonl_sha256"] != expected_sha256:
        raise SeriesError("shared SeriesRow write checker input_jsonl_sha256 disagrees with rows")
    results = output["results"]
    if not isinstance(results, list) or len(results) != len(candidates):
        raise SeriesError("shared SeriesRow write checker did not return one result per row")
    for position, ((index, _row), item) in enumerate(zip(candidates, results), 1):
        if (
            not isinstance(item, dict)
            or set(item) != {"line", "result"}
            or type(item["line"]) is not int
            or item["line"] != position
            or not (item["result"] is None or isinstance(item["result"], str))
        ):
            raise SeriesError(
                f"shared SeriesRow write checker result {position} is malformed"
            )
        verdicts[index] = item["result"]
    return verdicts


def _validate_rows(
    rows: Iterable[Any], *, strict_paths: bool, read: bool,
) -> list[dict]:
    """Apply Hermit's shared SeriesRow checks for one read or write boundary."""
    checked = _checked_rows(rows, strict_paths=strict_paths)
    if not checked:
        return checked

    arguments = ["--read"] if read else []
    result = _run_shared_checker(_checker_input(checked), arguments)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise MalformedRow(detail)
    return checked


def projection_results(
    rows: Iterable[Any], *, strict_paths: bool = True,
) -> ProjectionResults:
    """Return Hermit's validate_for_projection result for every event id.

    ``None`` means the shared type admitted the row. A nonempty string is the
    exact rejection returned by ``SeriesRow::validate_for_projection``. Rows
    are sorted by their canonical JSON before invoking the checker so callers
    receive the same result map and input digest for any ordering of one row
    population.
    """
    checked = _checked_rows(rows, strict_paths=strict_paths)
    ordered = sorted(checked, key=_canonical_row_json)
    checker_input = _checker_input(ordered)
    expected_sha256 = hashlib.sha256(checker_input.encode()).hexdigest()
    if not ordered:
        return ProjectionResults(0, expected_sha256, {})

    result = _run_shared_checker(checker_input, ["--projection-results"])
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise MalformedRow(detail)
    return _parse_projection_results(
        result.stdout,
        expected_event_ids={str(row["event_id"]) for row in ordered},
        expected_row_count=len(ordered),
        expected_sha256=expected_sha256,
    )


def _parse_projection_results(
    encoded: str,
    *,
    expected_event_ids: set[str],
    expected_row_count: int,
    expected_sha256: str,
) -> ProjectionResults:
    """Validate the checker envelope before exposing any projection result."""
    try:
        output = json.loads(encoded)
    except (json.JSONDecodeError, TypeError, RecursionError) as error:
        raise SeriesError(
            f"shared SeriesRow projection checker returned malformed JSON: {error}"
        ) from error
    if not isinstance(output, dict) or set(output) != {
        "input_jsonl_sha256", "input_row_count", "results", "schema_version",
    }:
        raise SeriesError(
            "shared SeriesRow projection checker returned an invalid top-level shape"
        )
    if (
        not isinstance(output["schema_version"], int)
        or isinstance(output["schema_version"], bool)
        or output["schema_version"] != _PROJECTION_RESULTS_SCHEMA_VERSION
    ):
        raise SeriesError(
            "shared SeriesRow projection checker returned an unsupported schema_version"
        )
    input_row_count = output["input_row_count"]
    if not isinstance(input_row_count, int) or isinstance(input_row_count, bool):
        raise SeriesError(
            "shared SeriesRow projection checker returned an invalid input_row_count"
        )
    if input_row_count != expected_row_count:
        raise SeriesError(
            "shared SeriesRow projection checker input_row_count disagrees with rows"
        )
    input_jsonl_sha256 = output["input_jsonl_sha256"]
    if (
        not isinstance(input_jsonl_sha256, str)
        or _SHA256_RE.fullmatch(input_jsonl_sha256) is None
    ):
        raise SeriesError(
            "shared SeriesRow projection checker returned an invalid input_jsonl_sha256"
        )
    if input_jsonl_sha256 != expected_sha256:
        raise SeriesError(
            "shared SeriesRow projection checker input_jsonl_sha256 disagrees with rows"
        )
    raw_results = output["results"]
    if not isinstance(raw_results, list):
        raise SeriesError(
            "shared SeriesRow projection checker results must be an array"
        )
    parsed = _parse_projection_result_items(raw_results)
    returned_event_ids = set(parsed)
    missing = sorted(expected_event_ids.difference(returned_event_ids))
    unknown = sorted(returned_event_ids.difference(expected_event_ids))
    if missing:
        raise SeriesError(
            "shared SeriesRow projection checker omitted event_id(s): "
            + ", ".join(missing)
        )
    if unknown:
        raise SeriesError(
            "shared SeriesRow projection checker returned unknown event_id(s): "
            + ", ".join(unknown)
        )
    if len(parsed) != expected_row_count:
        raise SeriesError(
            "shared SeriesRow projection checker result count disagrees with rows"
        )
    return ProjectionResults(
        row_count=input_row_count,
        input_jsonl_sha256=input_jsonl_sha256,
        results=dict(sorted(parsed.items())),
    )


def _parse_projection_result_items(raw_results: list[Any]) -> dict[str, str | None]:
    """Parse one complete event-id-to-result mapping, refusing ambiguity."""
    parsed: dict[str, str | None] = {}
    for index, item in enumerate(raw_results):
        if not isinstance(item, dict) or set(item) != {"event_id", "result"}:
            raise SeriesError(
                f"shared SeriesRow projection checker result {index} has an invalid shape"
            )
        event_id = item["event_id"]
        if not isinstance(event_id, str) or not event_id.strip():
            raise SeriesError(
                f"shared SeriesRow projection checker result {index} has an invalid event_id"
            )
        if event_id in parsed:
            raise SeriesError(
                f"shared SeriesRow projection checker repeated event_id {event_id!r}"
            )
        projection_result = item["result"]
        if projection_result is not None and (
            not isinstance(projection_result, str) or not projection_result.strip()
        ):
            raise SeriesError(
                f"shared SeriesRow projection checker result for {event_id!r} "
                "must be null or a nonempty string"
            )
        parsed[event_id] = projection_result
    return parsed


def _checked_rows(rows: Iterable[Any], *, strict_paths: bool) -> list[dict]:
    """Copy input rows and enforce parent-owned path publication rules."""
    checked = []
    for row in rows:
        if not isinstance(row, dict):
            raise MalformedRow(f"row must be an object, got {type(row).__name__}")
        copied = dict(row)
        if strict_paths and _contains_owner_path(copied):
            raise MalformedRow("row contains a workspace-local owner path")
        checked.append(copied)
    return checked


def _canonical_row_json(row: dict) -> str:
    try:
        return json.dumps(row, sort_keys=True, separators=(",", ":"))
    except (TypeError, ValueError, RecursionError) as error:
        raise MalformedRow(f"row is not canonical JSON: {error}") from error


def _checker_input(rows: Iterable[dict]) -> str:
    return "".join(_canonical_row_json(row) + "\n" for row in rows)


def prepare_shared_checker() -> None:
    """Prepare the same read-only checker before taking publication authority."""
    result = _run_shared_checker("", ["--help"])
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise SeriesError(f"cannot prepare the shared SeriesRow checker: {detail}")


def _run_shared_checker(
    checker_input: str, arguments: Sequence[str],
) -> subprocess.CompletedProcess[str]:
    """Invoke Hermit's shared row checker without accepting partial output."""
    checker = Path(__file__).resolve().with_name("check-series-rows.rs")
    # The checker compiles the current tool's Hermit dependency, which may use
    # a different channel from an inherited historical measurement environment.
    tool_hermit = checker.parents[2] / "hermit"
    environment = dict(os.environ)
    # This compiler/checker reads stdin only; it never needs ledger authority.
    environment.pop(LEDGER_ROOT_ENV, None)
    environment.pop("DEV_HERMIT_TEST_LEDGER_PUBLISH_LOCK_FD", None)
    try:
        selected = rust_toolchain.selected_toolchain(tool_hermit, environment=environment)
        environment["RUSTUP_TOOLCHAIN"] = selected
        environment["RUSTUP_AUTO_INSTALL"] = "0"
        return subprocess.run(
            ["rust-script", "--force", "--toolchain", selected, str(checker), *arguments],
            input=checker_input,
            env=environment,
            capture_output=True,
            text=True,
            timeout=300,
            check=False,
        )
    except (rust_toolchain.ToolchainRefused, OSError, subprocess.TimeoutExpired, UnicodeError) as error:
        raise SeriesError(f"cannot run the shared SeriesRow checker: {error}") from error


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
    if failure_class == "product_failure" and _is_str_in(outcome, {"diverged", "errored"}):
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
            elif verdict == "no_result" and _is_str_in(comparison.get("no_result_kind"), {
                "first_run_rejected", "container_failed",
            }):
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


def append_cell_rows(
    parent: Any, path: Any, rows: Iterable[dict], *,
    conflicts: list[tuple[int, dict, str]] | None = None,
) -> int:
    """Append projected cell rows once, refusing a changed repeated identity.

    With ``conflicts``, a row whose event_id repeats within the batch or
    already records a different body is set aside there as
    ``(position in rows, row, reason)`` and the remaining rows are still
    appended. Without it, such a row refuses the whole batch.
    """
    path = Path(path)
    checked = validate_rows(rows)
    # Read the locally available immutable ledger ref before taking the spool
    # lock. Publishing/fetching is separate from retaining a producer's result.
    published = published_snapshot_at(
        parent, "refs/remotes/origin/main", repository=LEDGER_REPOSITORY,
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    with series_spool_transition_lock(path.parent, exclusive=True):
        # Check both the live spool and published shards. Once a batch has been
        # published and its spool removed, re-emitting it must still be a no-op.
        local = sorted(path.parent.glob("*.jsonl")) + sorted(
            (path.parent / "published").glob("*.jsonl")
        )
        existing = union_events([*published.rows, *union(local)])
        known = {str(row.get("event_id", "")): row for row in existing}
        pending = []
        batch_ids = set()
        for position, row in enumerate(checked):
            event_id = str(row.get("event_id", ""))
            if event_id in batch_ids:
                reason = (
                    f"cell batch repeats event_id {event_id!r}; run_index must identify one run"
                )
                if conflicts is None:
                    raise SeriesError(reason)
                conflicts.append((position, row, reason))
                continue
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
                reason = (
                    f"event_id {event_id!r} already records a different row; refusing rewrite"
                )
                if conflicts is None:
                    raise SeriesError(reason)
                conflicts.append((position, row, reason))
        return append_events(path, pending) if pending else 0


def _quarantine_name(run_id: str) -> str:
    """One quarantine file per run, safe even for a run id the writer refused."""
    if run_id and "/" not in run_id and run_id not in (".", "..") and "\0" not in run_id:
        return f"{run_id}.jsonl"
    return "invalid-run-id-" + hashlib.sha256(run_id.encode()).hexdigest() + ".jsonl"


def _quarantine_cell(raw: Any) -> str | None:
    """Best-effort cell name for a record whose row may be malformed."""
    if not isinstance(raw, dict):
        return None
    payload = raw.get("series")
    if isinstance(payload, dict) and isinstance(payload.get("cell"), str):
        return payload["cell"]
    parts = [raw.get(name) for name in ("test", "mode", "backend")]
    if all(isinstance(part, str) and part for part in parts):
        return "/".join(parts)
    return None


class MalformedQuarantine(SeriesError):
    """A quarantine record that cannot be retained or published as evidence."""


_QUARANTINE_KEYS = frozenset({
    "schema", "event_id", "event_type", "team", "host", "emitted_at", "run_id",
    "producer", "tree", "stage", "error", "reason", "cell", "row_event_id",
    "inputs", "row", "owner_paths", "withheld", "legacy",
})
_QUARANTINE_INPUT_KEYS = frozenset({"input_row", "raw", "raw_base64", "raw_sha256", "raw_bytes"})
_QUARANTINE_LEGACY_KEYS = frozenset({
    "schema", "quarantine_id", "input_row", "raw", "raw_sha256", "raw_bytes",
})
_QUARANTINE_ID_RE = re.compile(r"series-quarantine-[0-9a-f]{64}")
_QUARANTINE_EMITTED_AT_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\S+")
#: A stand-in for one owner-home prefix in a published record. The ledger
#: publishes only the prefix's byte length, so the bytes come back only for a
#: reader who already knows the prefix (see portable_quarantine_record).
_OWNER_HOME_TOKEN_RE = re.compile(r"\{\{OWNER_HOME_[0-9]+\}\}")
#: One owner-home prefix: the needle plus the account segment that follows it.
#: The segment stops at any character that JSON escapes or that delimits a path,
#: so the same substring appears identically in the encoded and decoded record.
_OWNER_HOME_PREFIX_RE = re.compile(
    re.escape(_OWNER_PATH_NEEDLE) + r"[^/\\\"'\s=,:;()\[\]{}<>]+"
)
_QUARANTINE_REDACTED = "[withheld: contained an owner path]"


def _writer_host() -> str:
    """This machine's short name: the shard a refused input is published under."""
    host = socket.gethostname().split(".")[0]
    if not host or "/" in host or host in (".", "..") or "\0" in host:
        raise MalformedQuarantine(f"cannot derive a host path segment from {socket.gethostname()!r}")
    return host


def quarantine_input(input_row: int | None, raw: bytes) -> dict:
    """Describe one received input by its exact bytes.

    ``raw`` holds the bytes as UTF-8 text when they decode strictly (so they
    re-encode to the same bytes), otherwise ``raw_base64`` holds them. The
    digest and length are always over the exact received bytes.
    """
    if not isinstance(raw, (bytes, bytearray)):
        raise MalformedQuarantine(
            f"a quarantine input must be the received bytes, got {type(raw).__name__}"
        )
    raw = bytes(raw)
    try:
        text: str | None = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = None
    return {
        "input_row": input_row,
        "raw": text,
        "raw_base64": None if text is not None else base64.b64encode(raw).decode("ascii"),
        "raw_sha256": hashlib.sha256(raw).hexdigest(),
        "raw_bytes": len(raw),
    }


def _quarantine_event_id(record: Mapping[str, Any]) -> str:
    """Identity of one refusal: what was refused, where, and at which stage.

    It binds the exact input digests, not the observation time, so replaying
    one run's inputs is a no-op rather than a second record.
    """
    legacy = record.get("legacy")
    if legacy is not None:
        identity: dict[str, Any] = {
            "event_id": record.get("row_event_id"),
            "input_row": legacy.get("input_row"),
            "producer": record.get("producer"),
            "raw_sha256": legacy.get("raw_sha256"),
            "run_id": record.get("run_id"),
            "stage": record.get("stage"),
            "tree": record.get("tree"),
        }
    else:
        identity = {
            "inputs": [[item.get("input_row"), item.get("raw_sha256")]
                       for item in record.get("inputs") or []],
            "producer": record.get("producer"),
            "row_event_id": record.get("row_event_id"),
            "run_id": record.get("run_id"),
            "stage": record.get("stage"),
            "tree": record.get("tree"),
        }
    encoded = json.dumps(identity, sort_keys=True, separators=(",", ":"))
    return "series-quarantine-" + hashlib.sha256(encoded.encode()).hexdigest()


def quarantine_record(
    *, stage: str, run_id: str, producer: str, tree: str, emitted_at: str,
    error: str, reason: str, inputs: Sequence[dict], row: dict | None = None,
    row_event_id: str | None = None, cell: str | None = None, host: str | None = None,
) -> dict:
    """Describe one refusal with the exact inputs it refused.

    ``inputs`` come from :func:`quarantine_input`, so each carries the input
    exactly as received, byte for byte. ``row`` is the projected series row
    for a refusal made after projection (shared checker or spool conflict);
    it is derived, never a substitute for the inputs. The local record keeps
    every byte, owner paths included; :func:`portable_quarantine_record` makes
    the published form.
    """
    record = {
        "schema": QUARANTINE_SCHEMA,
        "event_id": "",
        "event_type": QUARANTINE_EVENT_TYPE,
        "team": "hermit",
        "host": host if host is not None else _writer_host(),
        "emitted_at": emitted_at,
        "run_id": run_id,
        "producer": producer,
        "tree": tree,
        "stage": stage,
        "error": error,
        "reason": reason,
        "cell": cell,
        "row_event_id": row_event_id,
        "inputs": [dict(item) for item in inputs],
        "row": copy.deepcopy(row) if row is not None else None,
        "owner_paths": [],
        "withheld": None,
        "legacy": None,
    }
    record["event_id"] = _quarantine_event_id(record)
    validate_quarantine_records([record], public=False)
    return record


def upgrade_legacy_quarantine(record: Mapping[str, Any], *, host: str) -> dict:
    """Carry a ``series-quarantine/v1`` record into the published v2 type.

    v1 stored the parsed object and a digest of its canonical re-serialization,
    never the received bytes, so the upgraded record has no ``inputs``. The v1
    content is kept verbatim under ``legacy`` and labelled as such; it is not
    presented as the received bytes. Its identity is the v1 ``quarantine_id``.
    """
    if not isinstance(record, Mapping) or record.get("schema") != LEGACY_QUARANTINE_SCHEMA:
        raise MalformedQuarantine("not a series-quarantine/v1 record")
    recorded_host = record.get("host")
    if not (isinstance(recorded_host, str) and recorded_host and "/" not in recorded_host
            and recorded_host not in (".", "..")):
        recorded_host = host
    upgraded = {
        "schema": QUARANTINE_SCHEMA,
        "event_id": record.get("quarantine_id"),
        "event_type": QUARANTINE_EVENT_TYPE,
        "team": "hermit",
        "host": recorded_host,
        "emitted_at": record.get("emitted_at"),
        "run_id": record.get("run_id"),
        "producer": record.get("producer"),
        "tree": record.get("tree"),
        "stage": record.get("stage"),
        "error": "legacy-v1-unrecorded",
        "reason": record.get("reason"),
        "cell": record.get("cell"),
        "row_event_id": record.get("event_id"),
        "inputs": [],
        "row": None,
        "owner_paths": [],
        "withheld": None,
        "legacy": {
            "schema": LEGACY_QUARANTINE_SCHEMA,
            "quarantine_id": record.get("quarantine_id"),
            "input_row": record.get("input_row"),
            "raw": copy.deepcopy(record.get("raw")),
            "raw_sha256": record.get("raw_sha256"),
            "raw_bytes": record.get("raw_bytes"),
        },
    }
    validate_quarantine_records([upgraded], public=False)
    return upgraded


def _quarantine_text(record: Mapping[str, Any]) -> str:
    return json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def quarantine_exposes_owner_path(record: Mapping[str, Any]) -> bool:
    """True when any published form of ``record`` would carry an owner path.

    Stricter than the series row guard: it also reads keys, the decoded
    content of each JSON input line, and base64-held bytes.
    """
    try:
        text = _quarantine_text(record)
    except (TypeError, ValueError, RecursionError):
        return True
    if _OWNER_PATH_NEEDLE in text:
        return True
    needle = _OWNER_PATH_NEEDLE.encode()
    for item in record.get("inputs") or []:
        if not isinstance(item, Mapping):
            continue
        raw = item.get("raw")
        if isinstance(raw, str):
            try:
                decoded = json.dumps(json.loads(raw), ensure_ascii=False)
            except (ValueError, RecursionError):
                decoded = ""
            if _OWNER_PATH_NEEDLE in decoded:
                return True
        encoded = item.get("raw_base64")
        if isinstance(encoded, str):
            try:
                content = base64.b64decode(encoded, validate=True)
            except ValueError:
                return True
            if needle in content:
                return True
    return False


def _withheld_quarantine_stub(record: Mapping[str, Any], detail: str) -> dict:
    """A content-free public stand-in for a record token substitution cannot publish.

    It keeps each input's digest and length and none of its bytes. The ledger
    cannot restore the record from it: the only exact copy is the host-local
    one that the publisher moves into ``held/`` (:func:`quarantine_held_dir`),
    where the health gate alarms until someone deals with it.
    """
    stub = copy.deepcopy(dict(record))
    stub["inputs"] = [
        {
            "input_row": item.get("input_row"),
            "raw": None,
            "raw_base64": None,
            "raw_sha256": item.get("raw_sha256"),
            "raw_bytes": item.get("raw_bytes"),
        }
        for item in record.get("inputs") or []
    ]
    stub["row"] = None
    stub["owner_paths"] = []
    stub["withheld"] = {"reason": "owner-path", "detail": detail}
    if stub.get("legacy") is not None:
        stub["legacy"]["raw"] = None
    for name in ("reason", "cell", "tree", "run_id", "error", "row_event_id", "host"):
        value = stub.get(name)
        if isinstance(value, str) and _OWNER_PATH_NEEDLE in value:
            stub[name] = _QUARANTINE_REDACTED
    if quarantine_exposes_owner_path(stub):
        raise MalformedQuarantine(
            f"quarantine record {record.get('event_id')!r} still exposes an owner path "
            "after withholding its content"
        )
    return stub


def portable_quarantine_record(record: Mapping[str, Any]) -> dict:
    """The deterministic form of ``record`` published to the public test ledger.

    Each distinct owner-home prefix (the needle plus its account segment) is
    replaced by ``{{OWNER_HOME_<n>}}`` and listed in ``owner_paths`` with its
    byte length only. The published record is therefore NOT the received
    bytes: it is exact only after each token is replaced by a prefix that the
    ledger does not contain, and ``raw_sha256``/``raw_bytes`` prove a
    substitution a reader who knows the prefixes makes. The substitution is
    checked to round-trip here, before publication. A record that token
    substitution cannot make public (a token already in the content, a path
    the prefix pattern does not cover, or base64-held bytes) is published as
    a stub with ``withheld`` set, each input's digest and length, and no
    content. Its bytes are then kept only in the host-local ``held/``
    directory (:func:`quarantine_held_dir`). Idempotent.
    """
    text = _quarantine_text(record)
    prefixes: list[str] = []
    for match in _OWNER_HOME_PREFIX_RE.finditer(text):
        if match.group(0) not in prefixes:
            prefixes.append(match.group(0))
    if not prefixes:
        portable = json.loads(text)
    elif _OWNER_HOME_TOKEN_RE.search(text) is not None:
        return _withheld_quarantine_stub(
            record, "the content already contains an owner-home token, so a token "
            "substitution could not be told apart from the content",
        )
    else:
        tokens = {prefix: "{{OWNER_HOME_%d}}" % ordinal
                  for ordinal, prefix in enumerate(prefixes, 1)}
        replaced = _OWNER_HOME_PREFIX_RE.sub(lambda match: tokens[match.group(0)], text)
        restored = replaced
        for prefix, token in tokens.items():
            restored = restored.replace(token, prefix)
        if restored != text:
            return _withheld_quarantine_stub(
                record, "replacing owner paths did not round-trip exactly",
            )
        portable = json.loads(replaced)
        portable["owner_paths"] = [
            *(record.get("owner_paths") or []),
            *({"token": token, "bytes": len(prefix.encode())}
              for prefix, token in tokens.items()),
        ]
    if quarantine_exposes_owner_path(portable):
        return _withheld_quarantine_stub(
            record, "an owner path remains that the owner-home pattern does not "
            "replace",
        )
    return portable


def _require(condition: bool, index: int, message: str) -> None:
    if not condition:
        raise MalformedQuarantine(f"quarantine record {index}: {message}")


def validate_quarantine_records(records: Iterable[Any], *, public: bool = True) -> list[dict]:
    """Refuse a quarantine record that is not complete, exact evidence.

    Every received input must be present with a digest and length that its
    content proves (base64 always; text whenever no owner-home token stands in
    it). ``public`` additionally refuses any record that would publish an
    owner path.
    """
    checked = []
    for index, record in enumerate(records, 1):
        _require(isinstance(record, dict), index, "must be an object")
        _require(set(record) == _QUARANTINE_KEYS, index,
                 f"keys differ from the v2 type: {sorted(set(record) ^ _QUARANTINE_KEYS)}")
        _require(record["schema"] == QUARANTINE_SCHEMA, index, f"schema {record['schema']!r}")
        _require(record["event_type"] == QUARANTINE_EVENT_TYPE, index, "wrong event_type")
        _require(record["team"] == "hermit", index, "team must be hermit")
        host = record["host"]
        _require(isinstance(host, str) and bool(host) and "/" not in host
                 and host not in (".", "..") and "\0" not in host, index, f"host {host!r}")
        emitted_at = record["emitted_at"]
        _require(isinstance(emitted_at, str)
                 and _QUARANTINE_EMITTED_AT_RE.fullmatch(emitted_at) is not None,
                 index, f"emitted_at {emitted_at!r}")
        for name in ("run_id", "producer", "tree", "reason"):
            _require(isinstance(record[name], str), index, f"{name} must be a string")
        _require(isinstance(record["error"], str) and bool(record["error"].strip()),
                 index, "error must name the refusal type")
        _require(record["stage"] in QUARANTINE_STAGES, index, f"stage {record['stage']!r}")
        for name in ("cell", "row_event_id"):
            _require(record[name] is None or isinstance(record[name], str), index,
                     f"{name} must be a string or null")
        _require(record["row"] is None or isinstance(record["row"], dict), index,
                 "row must be an object or null")
        event_id = record["event_id"]
        _require(isinstance(event_id, str) and _QUARANTINE_ID_RE.fullmatch(event_id) is not None,
                 index, f"event_id {event_id!r}")
        withheld = record["withheld"]
        _require(withheld is None or (
            isinstance(withheld, dict) and set(withheld) == {"reason", "detail"}
            and withheld["reason"] == "owner-path" and isinstance(withheld["detail"], str)
        ), index, "withheld must be null or {reason: owner-path, detail}")
        owner_paths = record["owner_paths"]
        _require(isinstance(owner_paths, list), index, "owner_paths must be an array")
        # The content a token must appear in: everything but the token list itself.
        text = _quarantine_text({name: value for name, value in record.items()
                                 if name != "owner_paths"})
        growth = {}
        for ordinal, entry in enumerate(owner_paths, 1):
            _require(isinstance(entry, dict) and set(entry) == {"token", "bytes"}, index,
                     "owner_paths entries are {token, bytes}")
            _require(entry["token"] == "{{OWNER_HOME_%d}}" % ordinal, index,
                     f"owner_paths token {ordinal} is {entry['token']!r}")
            size = entry["bytes"]
            _require(type(size) is int and size > len(_OWNER_PATH_NEEDLE), index,
                     "owner_paths bytes must exceed the needle length")
            _require(entry["token"] in text, index, f"{entry['token']} is listed but unused")
            growth[entry["token"]] = size - len(entry["token"].encode())
        _require(not (withheld is not None and owner_paths), index,
                 "a withheld record carries no owner-home tokens")
        inputs = record["inputs"]
        legacy = record["legacy"]
        _require(isinstance(inputs, list), index, "inputs must be an array")
        _require(bool(inputs) or legacy is not None, index,
                 "a v2 refusal must carry the inputs it refused")
        for item in inputs:
            _require(isinstance(item, dict) and set(item) == _QUARANTINE_INPUT_KEYS, index,
                     "inputs entries are {input_row, raw, raw_base64, raw_sha256, raw_bytes}")
            row_number = item["input_row"]
            _require(row_number is None or (type(row_number) is int and row_number >= 1),
                     index, "input_row must be a positive int or null")
            digest, size = item["raw_sha256"], item["raw_bytes"]
            _require(isinstance(digest, str) and _SHA256_RE.fullmatch(digest) is not None,
                     index, "raw_sha256 must be 64-hex")
            _require(type(size) is int and size >= 0, index, "raw_bytes must be a count")
            raw, encoded = item["raw"], item["raw_base64"]
            if withheld is not None:
                _require(raw is None and encoded is None, index,
                         "a withheld record publishes no input content")
                continue
            _require((raw is None) != (encoded is None), index,
                     "exactly one of raw and raw_base64 holds the input")
            if encoded is not None:
                try:
                    content = base64.b64decode(encoded, validate=True) if isinstance(encoded, str) else None
                except ValueError:
                    content = None
                _require(content is not None, index, "raw_base64 is not base64")
                _require(hashlib.sha256(content).hexdigest() == digest and len(content) == size,
                         index, "raw_base64 does not match raw_sha256/raw_bytes")
                continue
            _require(isinstance(raw, str), index, "raw must be text")
            if not owner_paths:
                content = raw.encode("utf-8")
                _require(hashlib.sha256(content).hexdigest() == digest and len(content) == size,
                         index, "raw does not match raw_sha256/raw_bytes")
            else:
                restored = len(raw.encode("utf-8")) + sum(
                    raw.count(token) * delta for token, delta in growth.items()
                )
                _require(restored == size, index,
                         "raw with owner-home tokens restored does not match raw_bytes")
        if withheld is not None:
            _require(record["row"] is None, index, "a withheld record publishes no row")
        if record["stage"] in {"shared-checker", "spool-conflict"} and legacy is None:
            _require(isinstance(record["row_event_id"], str), index,
                     "a post-projection refusal names its row event id")
            _require(record["row"] is not None or withheld is not None, index,
                     "a post-projection refusal keeps its projected row")
        elif legacy is None:
            _require(record["row"] is None, index, "only a post-projection refusal has a row")
        if legacy is not None:
            _require(isinstance(legacy, dict) and set(legacy) == _QUARANTINE_LEGACY_KEYS, index,
                     "legacy has the v1 fields")
            _require(legacy["schema"] == LEGACY_QUARANTINE_SCHEMA, index, "legacy schema")
            _require(legacy["quarantine_id"] == event_id, index,
                     "an upgraded record keeps its v1 identity")
            _require(not inputs, index, "v1 never recorded the received bytes")
            _require(isinstance(legacy["raw_sha256"], str)
                     and _SHA256_RE.fullmatch(legacy["raw_sha256"]) is not None,
                     index, "legacy raw_sha256 must be 64-hex")
            if withheld is None and not owner_paths:
                canonical = json.dumps(
                    legacy["raw"], sort_keys=True, separators=(",", ":"), ensure_ascii=False,
                ).encode()
                _require(hashlib.sha256(canonical).hexdigest() == legacy["raw_sha256"]
                         and len(canonical) == legacy["raw_bytes"],
                         index, "legacy raw does not match its v1 digest")
        _require(_quarantine_event_id(record) == event_id, index,
                 "event_id does not bind the recorded identity")
        if public:
            _require(not quarantine_exposes_owner_path(record), index,
                     "would publish an owner path")
        checked.append(record)
    return checked


def _read_quarantine_file(path: Path) -> list[dict]:
    """Every record in one local quarantine file, strictly parsed.

    Lines are split on ``\\n`` only: v1 files were written without ASCII
    escaping, and ``str.splitlines`` would also split inside a string at
    U+2028 and other Unicode line boundaries.
    """
    if not path.is_file():
        return []
    try:
        text = path.read_bytes().decode("utf-8")
    except UnicodeDecodeError as error:
        raise MalformedEvent(f"{path}: quarantine file is not UTF-8: {error}") from error
    if text and not text.endswith("\n"):
        raise MalformedEvent(
            f"{path}: final line has no newline terminator; a writer died mid-append"
        )
    return [parse_line(line) for line in text.split("\n") if line.strip()]


def quarantine_identity(record: Mapping[str, Any]) -> Any:
    """The identity of one local quarantine line.

    A v2 record is named by its ``event_id``. A v1 line's own ``event_id``
    names the ROW it refused; its refusal identity is ``quarantine_id``.
    """
    if record.get("schema") == LEGACY_QUARANTINE_SCHEMA:
        return record.get("quarantine_id")
    return record.get("event_id")


def _local_quarantine_ids(path: Path) -> set[str]:
    known = set()
    for record in _read_quarantine_file(path):
        for name in ("event_id", "quarantine_id"):
            value = record.get(name)
            if isinstance(value, str) and value:
                known.add(value)
    return known


def _readable_local_quarantine_ids(directory: Path) -> set[str]:
    """Identities in every readable local quarantine file.

    Reads the pending, ``published/`` and ``held/`` files. Deduplication is
    best effort: an unreadable file is skipped here, not fatal. The publisher
    moves it into ``corrupt/`` and the backlog gate alarms on it. A duplicate
    that gets through is dropped again at publication.
    """
    known: set[str] = set()
    for folder in (directory, directory / "published", directory / QUARANTINE_HELD_SUBDIR):
        try:
            paths = sorted(folder.glob("*.jsonl")) if folder.is_dir() else []
        except OSError:
            continue
        for path in paths:
            try:
                known |= _local_quarantine_ids(path)
            except (OSError, LedgerError):
                continue
    return known


def _fsync_quarantine_directory(directory: Path) -> None:
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def write_quarantine_file(directory: Path, name: str, text: str, *, replace: bool = False) -> Path:
    """Durably create ``directory/name`` holding exactly ``text``.

    The bytes go to a hidden ``.partial`` file, which no reader globs, and are
    fsynced. The file is then linked (or with ``replace``, renamed) into place
    and the directory is fsynced. A reader therefore sees either the whole file
    or no file, never a torn one. Without ``replace``, an existing name is an
    error and is never overwritten.
    """
    directory.mkdir(parents=True, exist_ok=True)
    final = directory / name
    partial = directory / f".{name}.{os.getpid()}-{secrets.token_hex(4)}.partial"
    descriptor = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o644)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        if replace:
            os.replace(partial, final)
        else:
            os.link(partial, final)
    finally:
        partial.unlink(missing_ok=True)
    _fsync_quarantine_directory(directory)
    return final


def append_quarantine(parent: Any, run_id: str, records: Sequence[dict]) -> dict:
    """Retain refused inputs once per identity, exactly, with fsync.

    Each call writes the new records to a NEW immutable file,
    ``<run>.<time_ns>-<pid>-<random>.jsonl``, and never appends to an existing
    one. It takes no lock, so it never waits for a publisher, even one that is
    part way through a network push. The publisher snapshots the complete
    top-level files and moves exactly those files once they are published. A
    file created during a publication is therefore left for the next one.

    Records are deduplicated against every readable local file and against
    those already published in the test ledger, so a replayed run adds nothing.
    Returns ``{"path", "written", "duplicates", "published_check_error"}``, where
    ``path`` is the new file, or None when nothing was new.
    """
    directory = quarantine_dir(parent)
    result: dict[str, Any] = {
        "path": None, "written": 0, "duplicates": 0, "published_check_error": None,
    }
    if not records:
        return result
    validate_quarantine_records(records, public=False)
    published: set[str] = set()
    try:
        published = {
            record["event_id"]
            for record in published_quarantine_at(parent, "refs/remotes/origin/main")[1]
        }
    except (LedgerError, OSError, subprocess.SubprocessError) as error:
        # Retaining the refusal matters more than deduplicating it. A duplicate
        # of a published identity is dropped again at publication.
        result["published_check_error"] = str(error)
    directory.mkdir(parents=True, exist_ok=True)
    known = published | _readable_local_quarantine_ids(directory)
    pending = []
    for record in records:
        if record["event_id"] in known:
            result["duplicates"] += 1
            continue
        known.add(record["event_id"])
        pending.append(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")
    if pending:
        stem = _quarantine_name(run_id).removesuffix(".jsonl")
        if len(stem.encode()) > 150:
            # The record carries the run id; the name only has to be unique
            # and fit a 255-byte path component with its suffix.
            stem = "run-id-sha256-" + hashlib.sha256(run_id.encode()).hexdigest()
        name = f"{stem}.{time.time_ns()}-{os.getpid()}-{secrets.token_hex(4)}.jsonl"
        result["path"] = write_quarantine_file(directory, name, "".join(pending))
    result["written"] = len(pending)
    return result


def _published_form(record: Mapping[str, Any], *, host: str) -> dict:
    """The exact public record a local quarantine line publishes as."""
    if record.get("schema") == LEGACY_QUARANTINE_SCHEMA:
        record = upgrade_legacy_quarantine(record, host=host)
    else:
        validate_quarantine_records([record], public=False)
    return portable_quarantine_record(record)


def _quarantine_file_forms(path: Path, host: str) -> list[dict]:
    """The validated public form of every record in one local file, in order."""
    forms = []
    for record in _read_quarantine_file(path):
        portable = _published_form(record, host=host)
        validate_quarantine_records([portable], public=True)
        quarantine_shard_path("hermit", portable["host"], portable["emitted_at"][:7])
        forms.append(portable)
    return forms


def set_aside_unreadable_quarantine(directory: Any, *, host: str | None = None,
                                    folder: str | None = None) -> list[str]:
    """Move each local file that cannot be published whole into ``corrupt/``.

    Checks every ``*.jsonl`` file in ``directory`` (or in its ``folder``
    subdirectory). A file that is not UTF-8, lacks its final newline, has a line
    that does not parse, or has a record whose public form does not validate is
    renamed to ``corrupt/<folder>--<name>``. The rename is atomic and the bytes
    are unchanged. Nothing republishes or deletes ``corrupt/``, and
    ``ledger_spool_backlog --store series-quarantine`` counts every file there
    as held back and alarms. One bad file therefore no longer stops the others
    publishing. Returns the names it moved.
    """
    root = Path(directory)
    source = root / folder if folder else root
    if not source.is_dir():
        return []
    fallback = host if host is not None else _writer_host()
    corrupt = root / QUARANTINE_CORRUPT_SUBDIR
    moved: list[str] = []
    for path in sorted(source.glob("*.jsonl")):
        try:
            _quarantine_file_forms(path, fallback)
            continue
        except FileNotFoundError:
            continue
        except (OSError, LedgerError):
            pass
        corrupt.mkdir(parents=True, exist_ok=True)
        target = corrupt / f"{folder or 'pending'}--{path.name}"
        if target.exists():
            target = corrupt / f"{folder or 'pending'}--{time.time_ns()}-{path.name}"
        os.replace(path, target)
        moved.append(target.name)
    if moved:
        _fsync_quarantine_directory(corrupt)
        _fsync_quarantine_directory(source)
    return moved


def quarantine_publication_snapshot(
    directory: Any, *, host: str | None = None,
) -> tuple[dict[tuple[str, str, str], list[dict]], list[Path]]:
    """Group pending local quarantine files by the ledger shard they publish into.

    Only top-level ``*.jsonl`` files are pending. ``published/`` (proven
    published, awaiting durability), ``held/`` and ``corrupt/`` are never
    republished. Every record is converted to its portable form and validated
    as public. This function stays strict: the publisher first calls
    ``set_aside_unreadable_quarantine``, so a file that fails here is one that
    changed after that check, and the publication refuses. An identity is
    published once. Its identity excludes the observation time, so a later line
    with the same identity is the same refusal seen again, and the first one (in
    file, then line, order) stands.
    """
    root = Path(directory)
    pending: dict[tuple[str, str, str], list[dict]] = {}
    files: list[Path] = []
    if not root.is_dir():
        return pending, files
    fallback = host if host is not None else _writer_host()
    seen: set[str] = set()
    for path in sorted(root.glob("*.jsonl")):
        for portable in _quarantine_file_forms(path, fallback):
            if portable["event_id"] in seen:
                continue
            seen.add(portable["event_id"])
            key = ("hermit", portable["host"], portable["emitted_at"][:7])
            pending.setdefault(key, []).append(portable)
        files.append(path)
    return {key: union_events(value) for key, value in pending.items()}, files


def published_quarantine_at_repo(repo: Any, published_ref: str) -> tuple[str, list[dict]]:
    """Every published quarantine record at one resolved ledger commit."""
    repo = Path(repo)
    resolved = _git(repo, ["--no-replace-objects", "rev-parse", "--verify",
                           f"{published_ref}^{{commit}}"])
    commit = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA_RE.fullmatch(commit) is None:
        raise SeriesError(
            f"cannot resolve published quarantine revision {published_ref}: "
            + (resolved.stderr.strip() or "no output")
        )
    listed = _git(repo, ["--no-replace-objects", "ls-tree", "-r", "--name-only", commit,
                         "--", "quarantine/series"])
    if listed.returncode != 0:
        raise SeriesError(
            f"cannot list quarantine shards at {commit}: "
            + (listed.stderr.strip() or "no output")
        )
    records: list[dict] = []
    for relative in listed.stdout.splitlines():
        parts = Path(relative).parts
        if (len(parts) != 5 or parts[:2] != ("quarantine", "series")
                or not relative.endswith(".jsonl")):
            continue
        shown = _git(repo, ["--no-replace-objects", "show", f"{commit}:{relative}"])
        if shown.returncode != 0:
            raise SeriesError(
                f"cannot read {commit}:{relative}: " + (shown.stderr.strip() or "no output")
            )
        if shown.stdout and not shown.stdout.endswith("\n"):
            raise MalformedEvent(f"{commit}:{relative}: final line has no newline terminator")
        records.extend(parse_line(line) for line in shown.stdout.split("\n") if line.strip())
    return commit, union_events(validate_quarantine_records(records, public=True))


def published_quarantine_at(parent: Any, published_ref: str) -> tuple[str, list[dict]]:
    return published_quarantine_at_repo(
        published_repository(parent, LEDGER_REPOSITORY), published_ref,
    )


def store_paths(parent: Any) -> list[Path]:
    """Every tracked, live, and published-but-not-yet-synced batch."""
    tracked = sorted((ledger_root(parent) / "series").glob("*/*/*.jsonl"))
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
    """Fetch the ledger, then read one immutable published revision."""
    root = ledger_root(parent)
    refresh_origin_main(root, "cell ledger")
    return published_snapshot_at(
        parent, "refs/remotes/origin/main", repository=LEDGER_REPOSITORY,
    )


def published_snapshot_at(
    parent: Any, published_ref: str, *, repository: str = PARENT_REPOSITORY,
) -> PublishedSnapshot:
    """Resolve ``published_ref`` once and read only that immutable commit.

    Every tree and blob lookup below uses the resolved 40-hex object id.  A
    concurrent fetch may move the caller's ref after resolution, but it cannot
    splice shards from two revisions into one reported snapshot.
    """
    return _published_snapshot_at_repo(
        published_repository(parent, repository), published_ref, repository=repository,
    )


def _published_snapshot_at_repo(
    parent: Path, published_ref: str, *, repository: str,
) -> PublishedSnapshot:
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
    return PublishedSnapshot(commit, union_events(validate_read_rows(rows)), repository)


def published_rows(parent: Any) -> list[dict]:
    """Compatibility wrapper returning rows from the current published snapshot."""
    return published_snapshot(parent).rows


def published_rows_at(
    parent: Any, published_ref: str, *, repository: str = PARENT_REPOSITORY,
) -> list[dict]:
    """Compatibility wrapper returning rows from one resolved revision."""
    return published_snapshot_at(parent, published_ref, repository=repository).rows


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
    flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK
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


def _bounded_jsonl_names(
    directory_fd: int,
    *,
    display: str,
    deadline: float,
    entries_seen: int,
    context: str,
) -> tuple[list[str], int]:
    """Stream one directory under the snapshot's total entry/time budget."""
    names = []
    try:
        iterator = os.scandir(directory_fd)
    except OSError as error:
        raise SeriesError(
            f"cannot enumerate scorecard snapshot sources in {display}: {error}"
        ) from error
    try:
        while True:
            if time.monotonic() > deadline:
                raise SeriesError(
                    "scorecard source identity phase exceeded "
                    f"{SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS:.3f}s {context}"
                )
            try:
                entry = next(iterator)
            except StopIteration:
                break
            entries_seen += 1
            if entries_seen > SCORECARD_SNAPSHOT_MAX_DIRECTORY_ENTRIES:
                raise SeriesError(
                    "scorecard source identity phase inspected "
                    f"{entries_seen} total directory entries, exceeding the explicit "
                    f"maximum of {SCORECARD_SNAPSHOT_MAX_DIRECTORY_ENTRIES} {context}"
                )
            if time.monotonic() > deadline:
                raise SeriesError(
                    "scorecard source identity phase exceeded "
                    f"{SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS:.3f}s {context}"
                )
            if entry.name.endswith(".jsonl"):
                names.append(entry.name)
    except OSError as error:
        raise SeriesError(
            f"cannot enumerate scorecard snapshot sources in {display}: {error}"
        ) from error
    finally:
        iterator.close()
    names.sort()
    return names, entries_seen


def _bounded_snapshot_source_keys(
    spool_fd: int,
    published_fd: int | None,
    *,
    spool: Path,
    deadline: float,
    context: str,
) -> list[tuple[str, str]]:
    """Return deterministic source keys without first materializing all names."""
    live_names, entries_seen = _bounded_jsonl_names(
        spool_fd,
        display=str(spool),
        deadline=deadline,
        entries_seen=0,
        context=context,
    )
    published_names = []
    if published_fd is not None:
        published_names, entries_seen = _bounded_jsonl_names(
            published_fd,
            display=str(spool / "published"),
            deadline=deadline,
            entries_seen=entries_seen,
            context=context,
        )
    source_keys = [("live", name) for name in live_names]
    source_keys.extend(
        ("published-retained", f"published/{name}") for name in published_names
    )
    return source_keys


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
    """Held local frontier and every source inode through snapshot readback."""

    spool: Path
    spool_fd: int
    spool_identity: tuple[int, int]
    published_fd: int | None
    published_signature: tuple[int, ...] | None
    source_keys: list[tuple[str, str]]
    sources: list[_HeldSnapshotSource]

    def _verify_directory(self) -> None:
        held = os.fstat(self.spool_fd)
        try:
            named = os.lstat(self.spool)
        except OSError as error:
            raise SeriesError(
                f"series spool changed while snapshot was held: {error}"
            ) from error
        identity = (held.st_dev, held.st_ino)
        if (
            not stat.S_ISDIR(held.st_mode)
            or not stat.S_ISDIR(named.st_mode)
            or identity != self.spool_identity
            or (named.st_dev, named.st_ino) != self.spool_identity
        ):
            raise SeriesError("series spool pathname changed while snapshot was held")

        if self.published_fd is None:
            try:
                os.stat(
                    "published",
                    dir_fd=self.spool_fd,
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
            self.spool_fd,
            "published",
            str(self.spool / "published"),
        )
        if (
            not stat.S_ISDIR(held_published.st_mode)
            or not stat.S_ISDIR(named_published.st_mode)
            or _stat_signature(held_published) != self.published_signature
            or _stat_signature(named_published) != self.published_signature
        ):
            raise SeriesError("retained series directory changed while snapshot was held")

    def identity_generation(self) -> tuple[Any, ...]:
        """Exact source set and inode metadata, without rereading source bytes."""
        self._verify_directory()
        deadline = time.monotonic() + SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS
        current = _bounded_snapshot_source_keys(
            self.spool_fd,
            self.published_fd,
            spool=self.spool,
            deadline=deadline,
            context=f"while verifying held sources in {self.spool}",
        )
        if current != self.source_keys:
            raise SeriesError("series spool changed while its projection snapshot was held")
        identities = []
        for source in self.sources:
            held = os.fstat(source.descriptor)
            named = _lstat_at(source.directory_fd, source.name, source.relative)
            if (
                not stat.S_ISREG(held.st_mode)
                or not stat.S_ISREG(named.st_mode)
                or _stat_signature(held) != source.signature
                or _stat_signature(named) != source.signature
            ):
                raise SeriesError(
                    f"series snapshot source pathname changed while held open: "
                    f"{source.relative}"
                )
            identities.append(
                (source.kind, source.relative, source.signature)
            )
        return (
            self.spool_identity,
            self.published_signature,
            tuple(identities),
        )

    def verify(self) -> None:
        self.identity_generation()
        for source in self.sources:
            source.verify()

    def close(self) -> None:
        for source in reversed(self.sources):
            source.close()
        if self.published_fd is not None:
            os.close(self.published_fd)
        os.close(self.spool_fd)


@contextlib.contextmanager
def _local_snapshot_capture(parent: Path):
    """Bind local sources independently of the cooperative writer lock."""
    spool = spool_dir(parent)
    spool.mkdir(parents=True, exist_ok=True)
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    try:
        spool_fd = os.open(spool, directory_flags)
    except OSError as error:
        raise SeriesError(
            f"cannot open regular series spool directory {spool}: {error}"
        ) from error
    published_fd: int | None = None
    sources: list[_HeldSnapshotSource] = []
    capture: _LocalSnapshotCapture | None = None
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
        try:
            published_fd = os.open("published", directory_flags, dir_fd=spool_fd)
        except FileNotFoundError:
            published_fd = None
        except OSError as error:
            raise SeriesError(
                f"cannot open regular retained series directory {spool / 'published'}: {error}"
            ) from error

        deadline = time.monotonic() + SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS
        source_keys = _bounded_snapshot_source_keys(
            spool_fd,
            published_fd,
            spool=spool,
            deadline=deadline,
            context=f"while binding held sources in {spool}",
        )
        capture = _LocalSnapshotCapture(
            spool=spool,
            spool_fd=spool_fd,
            spool_identity=(spool_status.st_dev, spool_status.st_ino),
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
            os.close(spool_fd)


def _snapshot_source_generation(lock: _SeriesSpoolLock) -> tuple[Any, ...]:
    """Measure names and inode metadata within one bounded shared-lock phase."""
    started = time.monotonic()
    deadline = started + SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS
    lock_path = lock.spool / ".append.lock"

    def check_deadline() -> None:
        if time.monotonic() > deadline:
            raise SeriesError(
                "scorecard source identity phase exceeded "
                f"{SCORECARD_SNAPSHOT_IDENTITY_TIMEOUT_SECONDS:.3f}s while holding "
                f"shared series spool lock {lock_path}"
            )

    lock.verify()
    check_deadline()
    spool_signature = _stat_signature(os.fstat(lock.spool_fd))
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    published_fd: int | None = None
    published_signature: tuple[int, ...] | None = None
    try:
        try:
            published_status = os.stat(
                "published", dir_fd=lock.spool_fd, follow_symlinks=False,
            )
        except FileNotFoundError:
            published_status = None
        except OSError as error:
            raise SeriesError(
                f"cannot inspect retained series directory {lock.spool / 'published'}: "
                f"{error}"
            ) from error
        if published_status is not None:
            if not stat.S_ISDIR(published_status.st_mode):
                raise SeriesError(
                    f"retained series path is not a directory: {lock.spool / 'published'}"
                )
            try:
                published_fd = os.open(
                    "published", directory_flags, dir_fd=lock.spool_fd,
                )
            except OSError as error:
                raise SeriesError(
                    f"cannot open regular retained series directory "
                    f"{lock.spool / 'published'}: {error}"
                ) from error
            opened = os.fstat(published_fd)
            if _stat_signature(opened) != _stat_signature(published_status):
                raise SeriesError(
                    "retained series directory changed while its identity was measured"
                )
            published_signature = _stat_signature(opened)

        source_keys = _bounded_snapshot_source_keys(
            lock.spool_fd,
            published_fd,
            spool=lock.spool,
            deadline=deadline,
            context=f"while holding shared series spool lock {lock_path}",
        )

        identities = []
        for kind, relative in source_keys:
            if kind == "live":
                directory_fd, name = lock.spool_fd, relative
            else:
                assert published_fd is not None
                directory_fd, name = published_fd, Path(relative).name
            status = _lstat_at(directory_fd, name, relative)
            if not stat.S_ISREG(status.st_mode):
                raise SeriesError(
                    f"series snapshot source is not a regular file: {relative}"
                )
            identities.append((kind, relative, _stat_signature(status)))
            check_deadline()

        for kind, relative, expected in identities:
            if kind == "live":
                directory_fd, name = lock.spool_fd, relative
            else:
                assert published_fd is not None
                directory_fd, name = published_fd, Path(relative).name
            status = _lstat_at(directory_fd, name, relative)
            if not stat.S_ISREG(status.st_mode) or _stat_signature(status) != expected:
                raise SeriesError(
                    f"series snapshot source changed while its identity was measured: "
                    f"{relative}"
                )
            check_deadline()
        held_spool = os.fstat(lock.spool_fd)
        try:
            named_spool = os.lstat(lock.spool)
        except OSError as error:
            raise SeriesError(
                f"series spool changed while its identity was measured: {error}"
            ) from error
        if (
            _stat_signature(held_spool) != spool_signature
            or _stat_signature(named_spool) != spool_signature
        ):
            raise SeriesError(
                "series source set changed while its identity was measured"
            )
        if published_fd is not None:
            held = os.fstat(published_fd)
            named = _lstat_at(
                lock.spool_fd, "published", str(lock.spool / "published")
            )
            if (
                _stat_signature(held) != published_signature
                or _stat_signature(named) != published_signature
            ):
                raise SeriesError(
                    "retained series directory changed while its identity was measured"
                )
        lock.verify()
        check_deadline()
        return (
            lock.spool_identity,
            published_signature,
            tuple(identities),
        )
    finally:
        if published_fd is not None:
            os.close(published_fd)


def _origin_main_commit(parent: Path) -> str:
    """Bind the exact fetched origin/main commit of the supplied repository."""
    origin_ref = "refs/remotes/origin/main"
    resolved = _git(parent, [
        "--no-replace-objects", "rev-parse", "--verify", f"{origin_ref}^{{commit}}",
    ])
    commit = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA_RE.fullmatch(commit) is None:
        detail = resolved.stderr.strip() or resolved.stdout.strip() or "no output"
        raise SeriesError(f"cannot bind ledger origin/main for scorecard snapshot: {detail}")
    return commit


@contextlib.contextmanager
def _scorecard_projection_capture(parent: Any):
    """Build a snapshot while retaining every mutable source descriptor."""
    parent = Path(parent).resolve()
    repository = ledger_root(parent)
    # Both network refreshes run outside the bounded writer lock. The snapshot
    # is usable only when the exact published ref and the independently
    # recomputed local source generation stay unchanged across capture.
    refresh_origin_main(repository, "cell ledger")
    published_commit = _origin_main_commit(repository)
    published = _published_snapshot_at_repo(
        repository, published_commit, repository=LEDGER_REPOSITORY,
    )
    source_tree = _series_tree_at(repository, published.commit)
    with contextlib.ExitStack() as held_sources:
        with series_spool_transition_lock(
            spool_dir(parent), exclusive=False,
        ) as lock:
            before = _snapshot_source_generation(lock)

        local = held_sources.enter_context(_local_snapshot_capture(parent))
        captured = local.identity_generation()

        with series_spool_transition_lock(
            spool_dir(parent), exclusive=False,
        ) as lock:
            after = _snapshot_source_generation(lock)
            if before != captured or captured != after:
                raise SeriesError(
                    "series source generation changed during scorecard snapshot capture"
                )

        refresh_origin_main(repository, "cell ledger")
        refreshed_commit = _origin_main_commit(repository)
        if refreshed_commit != published_commit:
            raise SeriesError(
                "ledger origin/main changed during scorecard snapshot capture: "
                f"{published_commit} -> {refreshed_commit}"
            )

        with series_spool_transition_lock(
            spool_dir(parent), exclusive=False,
        ) as lock:
            final_generation = _snapshot_source_generation(lock)
            if final_generation != captured:
                raise SeriesError(
                    "series source generation changed after published-ref verification"
                )
        local.verify()

        local_rows = [row for source in local.sources for row in source.rows]
        try:
            rows = union_events([*published.rows, *local_rows])
        except LedgerError as error:
            raise SeriesError(f"cannot form canonical series snapshot: {error}") from error
        rows_bytes = _canonical_rows_bytes(rows)
        snapshot = {
            "schema": SCORECARD_SNAPSHOT_SCHEMA,
            "source": {
                "repository": LEDGER_REPOSITORY,
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
    def unique_fields(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        fields: dict[str, Any] = {}
        for key, value in pairs:
            if key in fields:
                raise SeriesError(f"scorecard snapshot has duplicate JSON field {key!r}")
            fields[key] = value
        return fields

    try:
        snapshot = json.loads(payload, object_pairs_hook=unique_fields)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SeriesError(f"scorecard snapshot is not valid JSON: {error}") from error
    if not isinstance(snapshot, dict) or set(snapshot) != {
        "schema", "source", "rows_read", "rows_sha256", "rows",
    }:
        raise SeriesError("scorecard snapshot has an invalid top-level shape")
    source = snapshot.get("source")
    required_source = {"path", "commit", "tree", "published_rows", "local_sources"}
    if not isinstance(source, dict) or set(source) not in (
        required_source, required_source | {"repository"},
    ):
        raise SeriesError("scorecard snapshot has an invalid source identity")
    if not _is_str_in(source.get("repository", PARENT_REPOSITORY), {
        PARENT_REPOSITORY, LEDGER_REPOSITORY,
    }):
        raise SeriesError("scorecard snapshot names an unsupported repository")
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
            not _is_str_in(item.get("kind"), {"live", "published-retained"})
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
    flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK
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


RETIRED_IDS_FILE = "tests/e2e/manifests/inventory/retired-ids.json"
RETIRED_IDS_SCHEMA = 1
_RETIRED_IDS_FIELDS = frozenset({"schema", "description", "retirements"})
_RETIREMENT_FIELDS = frozenset({
    "retired_bucket", "successor_bucket", "last_live_commit", "reason", "ids", "collisions",
})
_RETIRED_TEST_NAME_RE = re.compile(r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?")


def _retired_test_name(test: str, bucket: str) -> str | None:
    """``name`` of ``bucket/name``, with Hermit's ``retired_ids::test_name`` rule."""
    prefix = bucket + "/"
    if not test.startswith(prefix):
        return None
    name = test[len(prefix):]
    return name if _RETIRED_TEST_NAME_RE.fullmatch(name) else None


def _retired_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise SeriesError(f"retired-id map {label} must be a non-empty string")
    return value


def _retired_string_map(value: object, label: str) -> dict[str, str]:
    if not isinstance(value, dict) or any(
        not isinstance(key, str) or not isinstance(item, str) for key, item in value.items()
    ):
        raise SeriesError(f"retired-id map {label} must map strings to strings")
    return value


def parse_retired_ids(text: str) -> dict[str, str]:
    """Every retired test id and its successor, from Hermit's ``retired-ids.json``.

    When Hermit folds one manifest bucket into another, a program keeps its
    history but changes its test id (the backend-parity-c bucket became
    c-programs, https://github.com/rrnewton/hermit/issues/3301). Hermit records
    each old id and the id that replaced it in :data:`RETIRED_IDS_FILE`, and
    its own reader, ``hermit_manifest_plan::retired_ids::RetiredIds::parse``,
    decides what a valid map is. This reader accepts only the maps that one
    accepts, so the two never resolve one id differently: exactly the schema-1
    fields; non-empty text fields; a full lowercase ``last_live_commit``; every
    retired id a test of its retired bucket and every successor a test of its
    successor bucket; each successor the plain bucket-prefix rename unless the
    old id is listed, with a reason, in ``collisions``; no id retired twice; no
    successor shared by two ids; and no successor that is itself retired.
    Anything else raises :class:`SeriesError`.
    """
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        raise SeriesError(f"invalid retired-id map: {error}") from error
    if not isinstance(document, dict) or set(document) != _RETIRED_IDS_FIELDS:
        raise SeriesError(
            "retired-id map must hold exactly schema, description and retirements"
        )
    schema = document["schema"]
    if type(schema) is not int or schema != RETIRED_IDS_SCHEMA:
        raise SeriesError(
            f"retired-id map schema must be {RETIRED_IDS_SCHEMA}, found {schema!r}"
        )
    _retired_string(document["description"], "description")
    retirements = document["retirements"]
    if not isinstance(retirements, list) or not retirements:
        raise SeriesError("retired-id map has no retirement")
    successors: dict[str, str] = {}
    shared: set[str] = set()
    buckets: set[str] = set()
    for index, retirement in enumerate(retirements):
        if not isinstance(retirement, dict) or set(retirement) != _RETIREMENT_FIELDS:
            raise SeriesError(
                f"retired-id map retirement {index} must hold exactly "
                + ", ".join(sorted(_RETIREMENT_FIELDS))
            )
        bucket = _retired_string(retirement["retired_bucket"], "retired_bucket")
        successor_bucket = _retired_string(
            retirement["successor_bucket"], "successor_bucket"
        )
        if bucket == successor_bucket:
            raise SeriesError(f"retirement of {bucket} names itself as successor")
        if bucket in buckets:
            raise SeriesError(f"bucket {bucket} is retired twice")
        buckets.add(bucket)
        last_live = retirement["last_live_commit"]
        if not isinstance(last_live, str) or _SHA_RE.fullmatch(last_live) is None:
            raise SeriesError(
                f"retirement of {bucket} needs a full lowercase last_live_commit SHA"
            )
        _retired_string(retirement["reason"], f"retirement of {bucket} reason")
        ids = _retired_string_map(retirement["ids"], f"retirement of {bucket} ids")
        collisions = _retired_string_map(
            retirement["collisions"], f"retirement of {bucket} collisions"
        )
        if not ids:
            raise SeriesError(f"retirement of {bucket} retires no id")
        for old, reason in collisions.items():
            if old not in ids:
                raise SeriesError(
                    f"collision {old!r} is not a retired id of bucket {bucket}"
                )
            _retired_string(reason, f"collision {old!r} reason")
        for old, new in ids.items():
            old_name = _retired_test_name(old, bucket)
            if old_name is None:
                raise SeriesError(f"retired id {old!r} is not a test of bucket {bucket}")
            new_name = _retired_test_name(new, successor_bucket)
            if new_name is None:
                raise SeriesError(
                    f"successor {new!r} of {old!r} is not a test of bucket {successor_bucket}"
                )
            listed = old in collisions
            if old_name == new_name and listed:
                raise SeriesError(
                    f"collision {old!r} maps to the plain prefix rename {new!r}"
                )
            if old_name != new_name and not listed:
                raise SeriesError(
                    f"successor {new!r} of {old!r} is neither the prefix rename "
                    f"{successor_bucket}/{old_name} nor a listed collision"
                )
            if old in successors:
                raise SeriesError(f"retired id {old!r} is listed twice")
            if new in shared:
                raise SeriesError(f"successor {new!r} is shared by two retired ids")
            successors[old] = new
            shared.add(new)
    chained = sorted(shared.intersection(successors))
    if chained:
        raise SeriesError(
            f"{chained[0]!r} is both a retired id and a successor; "
            "record the final id directly"
        )
    return successors


def retired_ids_at(checkout: Path, ref: str) -> tuple[str, dict[str, str]] | None:
    """``(commit, map)``: Hermit's retired-id map as committed at ``ref``.

    ``ref`` is resolved once to a commit and the file is read from that
    immutable commit, never from the working tree. ``None`` means no map
    applies and every test id is joined exactly as recorded, which is what
    every reader did before the map existed: ``checkout`` is not a checkout,
    ``ref`` does not resolve, or the commit predates the file. A file that is
    present but unreadable or malformed raises :class:`SeriesError` rather than
    silently dropping the joins it would have made.
    """
    checkout = Path(checkout)
    if not _is_checkout(checkout):
        return None
    resolved = _git(checkout, ["rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"])
    commit = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA_RE.fullmatch(commit) is None:
        return None
    listed = _git(checkout, ["ls-tree", "-z", commit, "--", RETIRED_IDS_FILE])
    if listed.returncode != 0:
        detail = listed.stderr.strip() or "no output"
        raise SeriesError(f"cannot list {RETIRED_IDS_FILE} at Hermit {commit}: {detail}")
    entries = [entry for entry in listed.stdout.split("\0") if entry]
    if not entries:
        return None
    meta, _, path = entries[0].partition("\t")
    fields = meta.split()
    if len(entries) != 1 or path != RETIRED_IDS_FILE or len(fields) != 3 or fields[1] != "blob":
        raise SeriesError(f"{RETIRED_IDS_FILE} at Hermit {commit} is not one file")
    env = dict(os.environ)
    for name in _GIT_REPO_SCOPED_ENV:
        env.pop(name, None)
    shown = subprocess.run(
        ["git", "-C", str(checkout), "cat-file", "blob", fields[2]],
        capture_output=True, timeout=60, check=False, env=env,
    )
    if shown.returncode != 0:
        detail = shown.stderr.decode(errors="replace").strip() or "no output"
        raise SeriesError(f"cannot read {RETIRED_IDS_FILE} at Hermit {commit}: {detail}")
    try:
        text = shown.stdout.decode("utf-8")
    except UnicodeDecodeError as error:
        raise SeriesError(f"{RETIRED_IDS_FILE} at Hermit {commit} is not UTF-8") from error
    try:
        return commit, parse_retired_ids(text)
    except SeriesError as error:
        raise SeriesError(f"{RETIRED_IDS_FILE} at Hermit {commit}: {error}") from error


def resolve_test_id(test: str, retired: Mapping[str, str] | None) -> str:
    """The live test id that carries ``test``'s history: its successor if retired."""
    return retired.get(test, test) if retired else test


def resolve_cell(cell: str, retired: Mapping[str, str] | None) -> str:
    """``<test>/<mode>/<backend>`` with a retired ``test`` replaced by its successor."""
    parts = cell.rsplit("/", 2)
    if len(parts) != 3:
        return cell
    return "/".join([resolve_test_id(parts[0], retired), parts[1], parts[2]])


def history_rows(
    rows: Iterable[dict],
    test: str,
    tree: str | None = None,
    retired: Mapping[str, str] | None = None,
) -> list[dict]:
    """All rows for one test, ordered as they were emitted.

    Without ``retired`` this is one exact test id. With Hermit's retired-id map
    (:func:`retired_ids_at`), a row recorded under a retired id and a row
    recorded under its successor belong to one history, whichever of the two
    ``test`` names. Each row keeps its recorded ``series.cell``.
    """
    wanted = resolve_test_id(test, retired)
    selected = [
        row for row in rows
        if (recorded := test_id(row)) is not None
        and resolve_test_id(recorded, retired) == wanted
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


def _validate_container_failed_report(raw: str, attempt: dict, index: int) -> None:
    """Match the shared report's closed container-disposition contract."""
    def unique_fields(pairs):
        fields = {}
        for name, value in pairs:
            if name in fields:
                raise MalformedRow(
                    f"cell attempt {index} container_failed report has duplicate field {name!r}"
                )
            fields[name] = value
        return fields

    def invalid_constant(value):
        raise MalformedRow(
            f"cell attempt {index} container_failed report has non-JSON constant {value}"
        )

    report = json.loads(raw, object_pairs_hook=unique_fields,
                        parse_constant=invalid_constant)
    required = {
        "verified", "bitwise_parity", "verdict", "infrastructure_error",
        "comparison", "no_result_reason", "compared_log_messages",
        "guest_exit_code", "guest_signal", "first_divergent_scheduler_turn",
        "first_divergent_virtual_nanoseconds", "first_divergent_record",
        "first_divergent_syscall", "first_divergent_left_message",
        "first_divergent_right_message",
    }
    if not required <= report.keys():
        raise MalformedRow(
            f"cell attempt {index} container_failed report omits current producer fields"
        )
    reason = report["no_result_reason"]
    if (
        not isinstance(reason, dict)
        or set(reason) != {"kind", "run", "disposition"}
        or reason["kind"] != "container_failed"
        or not isinstance(reason["run"], str)
        or reason["run"] not in {"run1", "run2"}
    ):
        raise MalformedRow(f"cell attempt {index} has invalid container_failed reason")
    disposition = reason["disposition"]
    valid = False
    if isinstance(disposition, dict):
        if set(disposition) == {"kind", "code"} and disposition["kind"] == "exited":
            code = disposition["code"]
            valid = (
                type(code) is int and 0 <= code <= 255
                and code not in {122, 124} and not 129 <= code <= 192
            )
        elif (
            set(disposition) == {"kind", "signal", "core_dumped"}
            and disposition["kind"] == "signaled"
        ):
            signal = disposition["signal"]
            valid = (
                type(signal) is int and 1 <= signal <= 64
                and type(disposition["core_dumped"]) is bool
            )
    if not valid:
        raise MalformedRow(f"cell attempt {index} has invalid container_failed disposition")
    if (
        any(report.get(field) is not None for field in (
            "guest_exit_code", "guest_signal", "compared_outputs", "runtime",
            "dbt_counted_branches",
        ))
        or attempt.get("outcome") != "FAIL"
        or type(attempt.get("status")) is not int
        or attempt["status"] != 125
        or attempt.get("signal") is not None
        or attempt.get("timed_out") is not False
        or attempt.get("error_kind") is not None
    ):
        raise MalformedRow(
            f"cell attempt {index} container_failed disposition is inconsistent"
        )


def _expected_guest_exit(row: dict) -> tuple[int | None, int | None] | None:
    """Return a verify cell's declared nonzero guest disposition, if any.

    The Hermit manifest may declare that a verify cell's guest exits with one
    exact nonzero code or dies from one exact signal. The runner then passes
    ``--verify-allow=failure`` and passes the attempt only when the canonical
    report and Hermit's own status both show that disposition, and it copies
    the declaration onto the result row. The row's declaration is the only
    authority under which a matched attempt may carry a nonzero status, so
    its shape is checked here exactly as the manifest loader checks it.
    Returns ``(code, signal)`` with exactly one member set.
    """
    if "expected_guest_exit" not in row:
        return None
    declared = row["expected_guest_exit"]
    if _cell_mode(row) != "verify":
        raise MalformedRow("expected_guest_exit is valid only on a verify cell result")
    if not isinstance(declared, dict) or set(declared) != {"code", "signal", "reason"}:
        raise MalformedRow(
            "expected_guest_exit must be an object with exactly code, signal and reason"
        )
    code = declared["code"]
    signal = declared["signal"]
    reason = declared["reason"]
    if not isinstance(reason, str) or not reason.strip():
        raise MalformedRow("expected_guest_exit.reason must be substantive")
    if code is not None and signal is None:
        if type(code) is not int or not 1 <= code <= 255:
            raise MalformedRow(
                "expected_guest_exit.code must be a nonzero exit status in 1..=255, "
                f"got {code!r}"
            )
    elif code is None and signal is not None:
        if type(signal) is not int or not 1 <= signal <= 64:
            raise MalformedRow(
                "expected_guest_exit.signal must be a signal number in 1..=64, "
                f"got {signal!r}"
            )
    else:
        raise MalformedRow(
            "expected_guest_exit must name exactly one of code or signal"
        )
    return code, signal


#: Where Hermit's E2E manifests live in a Hermit commit, and the file of
#: shared defaults beside them that declares no test (ci/manifest-plan's
#: DEFAULTS_FILE). The Rust loader reads every other ``*.yaml`` file there.
HERMIT_MANIFEST_DIR = "tests/e2e/manifests"
HERMIT_MANIFEST_DEFAULTS = "defaults.yaml"

# Hermit's manifest loader (ci/manifest-plan/src/manifest_value.rs) parses YAML
# with serde_yaml 0.9.34, whose plain-scalar rules are YAML 1.2 core, not the
# YAML 1.1 rules PyYAML applies by default. Under PyYAML's rules `yes`, `on`,
# `010`, `1_0`, `1:30` and `2001-12-14` are not strings, and `0o7` is not an
# integer. These mirror serde_yaml's de.rs (visit_untagged_scalar, visit_int,
# parse_f64 and digits_but_not_number) so that both read the same values.
_SERDE_YAML_INT_TAG = "tag:hermit.invalid,2026:serde-yaml-int"
_SERDE_YAML_FLOAT_TAG = "tag:hermit.invalid,2026:serde-yaml-float"
_SERDE_YAML_RADIX = (("0x", 16, re.compile(r"[0-9a-fA-F]+")),
                     ("0o", 8, re.compile(r"[0-7]+")),
                     ("0b", 2, re.compile(r"[01]+")))
_SERDE_YAML_DECIMAL = re.compile(r"[0-9]+")
_SERDE_YAML_FLOAT = re.compile(r"-?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?")


def _serde_yaml_leading_zero(scalar: str) -> bool:
    """serde_yaml's ``digits_but_not_number``: ``0`` then digits is a string."""
    bare = scalar[1:] if scalar[:1] in ("+", "-") else scalar
    return len(bare) > 1 and bare[0] == "0" and _SERDE_YAML_DECIMAL.fullmatch(bare[1:]) is not None


def _serde_yaml_int(scalar: str) -> int | None:
    """The integer serde_yaml 0.9.34 resolves a plain scalar to, or None."""
    unpositive = scalar[1:] if scalar.startswith("+") else scalar
    for prefix, base, digits in _SERDE_YAML_RADIX:
        if unpositive.startswith(prefix) and digits.fullmatch(unpositive[2:]):
            value = int(unpositive[2:], base)
            return value if value < 1 << 128 else None
        if scalar.startswith("-" + prefix) and digits.fullmatch(scalar[3:]):
            value = -int(scalar[3:], base)
            return value if value >= -(1 << 127) else None
    if _serde_yaml_leading_zero(scalar):
        return None
    if _SERDE_YAML_DECIMAL.fullmatch(unpositive):
        value = int(unpositive)
        return value if value < 1 << 128 else None
    if scalar.startswith("-") and _SERDE_YAML_DECIMAL.fullmatch(scalar[1:]):
        value = int(scalar)
        return value if value >= -(1 << 127) else None
    return None


def _serde_yaml_float(scalar: str) -> float | None:
    """The float serde_yaml 0.9.34 resolves a plain non-integer scalar to, or None."""
    if _serde_yaml_leading_zero(scalar):
        return None
    unpositive = scalar[1:] if scalar.startswith("+") else scalar
    if unpositive[:1] == "+" or (scalar.startswith("+") and unpositive[:1] == "-"):
        return None
    if unpositive in (".inf", ".Inf", ".INF"):
        return math.inf
    if scalar in ("-.inf", "-.Inf", "-.INF"):
        return -math.inf
    if scalar in (".nan", ".NaN", ".NAN"):
        return math.nan
    if _SERDE_YAML_FLOAT.fullmatch(unpositive) is None:
        return None
    value = float(unpositive)
    return value if math.isfinite(value) else None


def _serde_yaml_plain_tag(scalar: str) -> str:
    if scalar in ("", "~", "null", "Null", "NULL"):
        return "tag:yaml.org,2002:null"
    if scalar in ("true", "True", "TRUE", "false", "False", "FALSE"):
        return "tag:yaml.org,2002:bool"
    if _serde_yaml_int(scalar) is not None:
        return _SERDE_YAML_INT_TAG
    if _serde_yaml_float(scalar) is not None:
        return _SERDE_YAML_FLOAT_TAG
    return "tag:yaml.org,2002:str"


def _manifest_yaml_loader(yaml: Any) -> type:
    """A PyYAML loader that reads a manifest as Hermit's loader does.

    Plain scalars resolve by serde_yaml's rules above; a quoted scalar is a
    string in both. A duplicate or non-string mapping key is refused, as
    ``manifest_value.rs`` refuses it: PyYAML would otherwise keep the last
    duplicate silently.
    """
    base = getattr(yaml, "CSafeLoader", yaml.SafeLoader)

    class ManifestLoader(base):  # type: ignore[misc, valid-type]
        def resolve(self, kind, value, implicit):
            if kind is yaml.ScalarNode and implicit[0]:
                return _serde_yaml_plain_tag(value)
            return super().resolve(kind, value, implicit)

        def construct_mapping(self, node, deep=False):
            seen: set[str] = set()
            for key_node, _ in node.value:
                key = self.construct_object(key_node, deep=True)
                if not isinstance(key, str):
                    raise yaml.constructor.ConstructorError(
                        None, None, f"manifest mapping key {key!r} is not a string",
                        key_node.start_mark,
                    )
                if key in seen:
                    raise yaml.constructor.ConstructorError(
                        None, None, f"duplicate manifest key {key!r}", key_node.start_mark,
                    )
                seen.add(key)
            return super().construct_mapping(node, deep=deep)

    def construct_int(loader, node):
        value = _serde_yaml_int(loader.construct_scalar(node))
        # serde_yaml resolves integers up to 128 bits, but its Value holds
        # only i64 and u64 and refuses the rest.
        if value is None or not -(1 << 63) <= value < 1 << 64:
            raise yaml.constructor.ConstructorError(
                None, None, f"integer {node.value} is outside i64 and u64", node.start_mark,
            )
        return value

    ManifestLoader.add_constructor(_SERDE_YAML_INT_TAG, construct_int)
    ManifestLoader.add_constructor(
        _SERDE_YAML_FLOAT_TAG,
        lambda loader, node: _serde_yaml_float(loader.construct_scalar(node)),
    )
    return ManifestLoader


def manifest_guest_exit_declarations(
    source: Path, tree: str,
) -> dict[str, tuple[int | None, int | None, str] | None]:
    """Every manifest test id at Hermit ``tree`` and its verify guest-exit declaration.

    Reads ``tests/e2e/manifests/*.yaml`` (not ``defaults.yaml``) from the
    immutable commit, never from a working tree, resolving scalars as Hermit's
    serde_yaml loader does (:func:`_manifest_yaml_loader`). Each test id maps to
    ``(code, signal, reason)`` from ``modes.verify.expected_guest_exit``, or
    to None when the entry declares none. The manifest, not the row, is the
    authority for a declared nonzero guest exit: the runner copies that
    declaration onto the result row verbatim, so a row that claims one its
    tree's manifest does not make is not evidence of an intended failure.
    A tree that cannot be read, or whose manifests do not parse into test
    entries with unique ids, raises :class:`SeriesError`: that is a fault in
    the invocation's source, not in any one row.
    """
    listed = _git(source, ["ls-tree", "-z", tree, "--", HERMIT_MANIFEST_DIR + "/"])
    if listed.returncode != 0:
        detail = listed.stderr.strip() or "no output"
        raise SeriesError(f"cannot list Hermit manifests at {tree}: {detail}")
    try:
        import yaml
    except ImportError as error:
        raise SeriesError(
            f"cannot read Hermit manifests at {tree} without PyYAML: {error}"
        ) from error
    loader = _manifest_yaml_loader(yaml)
    env = dict(os.environ)
    for name in _GIT_REPO_SCOPED_ENV:
        env.pop(name, None)
    declarations: dict[str, tuple[int | None, int | None, str] | None] = {}
    for entry in listed.stdout.split("\0"):
        if not entry:
            continue
        meta, _, path = entry.partition("\t")
        fields = meta.split()
        name = path.rsplit("/", 1)[-1]
        if (len(fields) != 3 or fields[1] != "blob" or not name.endswith(".yaml")
                or name == HERMIT_MANIFEST_DEFAULTS):
            continue
        shown = subprocess.run(
            ["git", "-C", str(source), "cat-file", "blob", fields[2]],
            capture_output=True, timeout=60, check=False, env=env,
        )
        if shown.returncode != 0:
            detail = shown.stderr.decode(errors="replace").strip() or "no output"
            raise SeriesError(f"cannot read {path} at Hermit {tree}: {detail}")
        try:
            document = yaml.load(shown.stdout.decode("utf-8"), Loader=loader)
        except (UnicodeDecodeError, yaml.YAMLError) as error:
            raise SeriesError(f"{path} at Hermit {tree} is not UTF-8 YAML: {error}") from error
        tests = document.get("test") if isinstance(document, dict) else None
        if not isinstance(tests, list):
            raise SeriesError(f"{path} at Hermit {tree} has no test list")
        for test in tests:
            identifier = test.get("id") if isinstance(test, dict) else None
            if not isinstance(identifier, str) or not identifier:
                raise SeriesError(f"{path} at Hermit {tree} has a test without a string id")
            if identifier in declarations:
                raise SeriesError(f"Hermit {tree} declares manifest test {identifier!r} twice")
            modes = test.get("modes")
            verify = modes.get("verify") if isinstance(modes, dict) else None
            declared = verify.get("expected_guest_exit") if isinstance(verify, dict) else None
            if declared is None:
                declarations[identifier] = None
                continue
            if not isinstance(declared, dict) or not set(declared) <= {"code", "signal", "reason"}:
                raise SeriesError(
                    f"{path} at Hermit {tree}: {identifier} expected_guest_exit is not "
                    "an object of code, signal and reason"
                )
            declarations[identifier] = (
                declared.get("code"), declared.get("signal"), declared.get("reason"),
            )
    return declarations


def _bind_expected_guest_exit(
    row: dict, tree: str,
    declarations: Callable[[], dict[str, tuple[int | None, int | None, str] | None]],
) -> None:
    """Refuse a row whose guest-exit declaration its tree's manifest does not make.

    The row's declaration is shape-checked first (:func:`_expected_guest_exit`).
    It must then equal, in code, signal AND reason, the declaration on the
    manifest entry named by the row's ``test`` at the row's Hermit ``tree``.
    The runner copies all three verbatim, so any difference means the row
    was not produced under this tree's manifest; that includes a row from a
    dirty working tree whose uncommitted manifest declared it. Such a row is
    refused as :class:`MalformedRow` (quarantined, never admitted). An
    undeclared row never reads the manifests.
    """
    expected = _expected_guest_exit(row)
    if expected is None:
        return
    test = row.get("test")
    if not isinstance(test, str) or not test:
        raise MalformedRow("a declared guest exit needs the row's manifest test id")
    manifests = declarations()
    if test not in manifests:
        raise MalformedRow(
            f"expected_guest_exit is declared for {test!r}, which has no manifest "
            f"entry at Hermit {tree}"
        )
    authority = manifests[test]
    if authority is None:
        raise MalformedRow(
            f"expected_guest_exit is declared for {test!r}, whose manifest entry at "
            f"Hermit {tree} declares none"
        )
    declared = row["expected_guest_exit"]
    claimed = (declared["code"], declared["signal"], declared["reason"])
    if any(type(have) is not type(want) or have != want
           for have, want in zip(claimed, authority)):
        raise MalformedRow(
            f"expected_guest_exit code={claimed[0]!r} signal={claimed[1]!r} for {test!r} "
            f"disagrees with its manifest entry at Hermit {tree} "
            f"(code={authority[0]!r} signal={authority[1]!r}) or its reason"
        )


def _require_matched_evidence(
    attempt: dict,
    report: dict,
    *,
    index: int,
    expected_guest_exit: tuple[int | None, int | None] | None,
) -> tuple[int | None, int | None]:
    """Refuse a matched attempt whose evidence is malformed or contradicts itself.

    This runs before a matched attempt is classified, so a ``failed_match``
    can only be structurally valid evidence that ended differently from what
    its row allows; it can never be a contradiction relabelled as red.

    Present ``compared_outputs`` must satisfy Hermit's
    ``require_exact_output_match`` (``hermit-cli/src/canonical_verdict.rs``):
    Hermit's complete canonical shape on both sides, identical left and
    right, exactly one guest disposition, agreeing with the report's own
    guest disposition. An undeclared report may omit them, as a historical
    receipt does. A declared row additionally requires the outputs,
    ``--verify-allow=failure`` before the guest separator, and a Hermit status
    that reports the guest's disposition (a signal death either as that
    signal or as ``128 + signo``).

    Returns the report's guest disposition ``(code, signal)``.
    """
    def refuse(detail: str) -> None:
        raise MalformedRow(
            f"cell attempt {index} matched report is internally inconsistent: {detail}"
        )

    if report.get("verified") is not True or report.get("bitwise_parity") is not True:
        refuse("verified and bitwise_parity must both be true")
    status = attempt.get("status")
    observed_signal = attempt.get("signal")
    for name, value in (("status", status), ("signal", observed_signal)):
        if not _is_optional_i32(value):
            refuse(f"attempt {name} is not an i32")
    guest = (report.get("guest_exit_code"), report.get("guest_signal"))
    if not all(_is_optional_i32(value) for value in guest):
        refuse("report guest disposition is not typed")
    outputs = report.get("compared_outputs")
    if outputs is None:
        if expected_guest_exit is not None:
            refuse("a declared guest exit requires compared_outputs")
    else:
        if not isinstance(outputs, dict) or set(outputs) != {"left", "right"}:
            refuse("compared_outputs must have exactly left and right")
        for side in ("left", "right"):
            output = outputs[side]
            if not _is_canonical_compared_output(output):
                refuse(f"{side} compared output is not Hermit's canonical shape")
            if (output["exit_code"] is None) == (output["signal"] is None):
                refuse(f"{side} compared output must record exactly one guest disposition")
        # Both sides are typed above: Python's == equates 7 with 7.0 and 1
        # with True, which Hermit's typed reader refuses on either side.
        if outputs["left"] != outputs["right"]:
            refuse("left and right compared outputs differ")
        if guest != (outputs["left"]["exit_code"], outputs["left"]["signal"]):
            refuse("compared outputs contradict the report's guest disposition")
    if expected_guest_exit is not None:
        argv = attempt.get("argv")
        if not isinstance(argv, list) or not all(isinstance(arg, str) for arg in argv):
            refuse("attempt argv is not a list of strings")
        hermit_args = argv[:argv.index("--")] if "--" in argv else argv
        if "--verify-allow=failure" not in hermit_args:
            refuse("a declared guest exit ran without --verify-allow=failure")
        code, signal = guest
        if (code is None) == (signal is None):
            refuse("report must record exactly one guest disposition")
        if code is not None:
            reported = status == code and observed_signal is None
        else:
            reported = (status is None and observed_signal == signal) or (
                status == 128 + signal and observed_signal is None
            )
        if not reported:
            refuse("Hermit's status does not report the guest's disposition")
    return guest


_COMPARED_OUTPUT_FIELDS = frozenset({
    "exit_code", "signal", "stdout_sha256", "stdout_bytes", "stderr_sha256", "stderr_bytes",
})


def _is_u64(value: Any) -> bool:
    return type(value) is int and 0 <= value <= 2**64 - 1


def _is_optional_i32(value: Any) -> bool:
    return value is None or (type(value) is int and -(2**31) <= value <= 2**31 - 1)


def _is_typed_runtime(runtime: Any) -> bool:
    """Whether ``runtime`` reads as Hermit's ``Option<VerificationRuntime>``.

    Each present run is a ``RuntimeStats``: ``scheduler_turns`` and
    ``virtual_nanoseconds`` are required u64s and ``syscalls`` an optional u64.
    Neither Rust type denies unknown fields, so other fields are not refused.
    """
    if runtime is None:
        return True
    if not isinstance(runtime, dict):
        return False
    for run in ("run1", "run2"):
        stats = runtime.get(run)
        if stats is None:
            continue
        if (
            not isinstance(stats, dict)
            or not _is_u64(stats.get("scheduler_turns"))
            or not _is_u64(stats.get("virtual_nanoseconds"))
            or not (stats.get("syscalls") is None or _is_u64(stats["syscalls"]))
        ):
            return False
    return True


def _is_canonical_compared_output(output: Any) -> bool:
    """Whether one side of ``compared_outputs`` has exactly Hermit's typed fields."""
    return (
        isinstance(output, dict)
        and set(output) == _COMPARED_OUTPUT_FIELDS
        and _is_optional_i32(output["exit_code"])
        and _is_optional_i32(output["signal"])
        and all(
            isinstance(output[field], str)
            and re.fullmatch(r"[0-9a-f]{64}", output[field]) is not None
            for field in ("stdout_sha256", "stderr_sha256")
        )
        and all(
            _is_u64(output[field]) for field in ("stdout_bytes", "stderr_bytes")
        )
    )


_U64_MAX = 2**64 - 1
# The texts ``expected_stdout_mismatch_reason`` and
# ``expected_stdout_contains_mismatch_reason`` in Hermit's
# ci/manifest-plan/src/runner.rs write. Rust's ``u64`` display has no sign and
# no leading zero, and its digests are lowercase SHA-256.
_EXACT_STDOUT_REASON_RE = re.compile(
    r"verify (?P<run>[a-z]+) run stdout \((?P<observed_bytes>0|[1-9][0-9]*) bytes, "
    r"sha256 (?P<observed_sha256>[0-9a-f]{64})\) differs from the declared "
    r"expected_stdout \((?P<declared_bytes>0|[1-9][0-9]*) bytes, "
    r"sha256 (?P<declared_sha256>[0-9a-f]{64})\)"
)
_CONTAINS_STDOUT_REASON_RE = re.compile(
    r"verify stdout \((?P<observed_bytes>0|[1-9][0-9]*) bytes, "
    r"sha256 (?P<observed_sha256>[0-9a-f]{64})\) does not contain the declared "
    r"expected_stdout_contains text (?P<quoted>\".*\")",
    re.DOTALL,
)


def _rust_debug_ascii(text: str) -> str | None:
    """Rust's ``{:?}`` rendering of an ASCII ``text``, or None for other text.

    Beyond ASCII, Rust's rendering depends on its Unicode printability tables,
    which this reader does not reproduce; such a text is not recognized.
    """
    rendered = ['"']
    for character in text:
        code = ord(character)
        if code > 0x7F:
            return None
        rendered.append({
            "\t": "\\t", "\r": "\\r", "\n": "\\n", "\0": "\\0",
            "\\": "\\\\", '"': '\\"',
        }.get(character) or (
            f"\\u{{{code:x}}}" if code < 0x20 or code == 0x7F else character
        ))
    rendered.append('"')
    return "".join(rendered)


def _parse_rust_debug_ascii(quoted: str) -> str | None:
    """Invert :func:`_rust_debug_ascii`, or None when it could not have written ``quoted``."""
    if len(quoted) < 2 or not quoted.startswith('"') or not quoted.endswith('"'):
        return None
    inner = quoted[1:-1]
    text = []
    position = 0
    while position < len(inner):
        character = inner[position]
        position += 1
        if character != "\\":
            text.append(character)
            continue
        escape = inner[position:position + 1]
        position += 1
        if escape == "u":
            closing = inner.find("}", position)
            digits = inner[position + 1:closing] if closing > position else ""
            if (not inner.startswith("{", position) or not digits
                    or re.fullmatch(r"[0-9a-f]+", digits) is None or int(digits, 16) > 0x7F):
                return None
            text.append(chr(int(digits, 16)))
            position = closing + 1
        elif escape in ("t", "r", "n", "0", "\\", '"'):
            text.append({"t": "\t", "r": "\r", "n": "\n", "0": "\0"}.get(escape, escape))
        else:
            return None
    decoded = "".join(text)
    return decoded if _rust_debug_ascii(decoded) == quoted else None


def _expected_output_failure_reason(
    attempt: dict, report: dict, *, mode: str, index: int,
) -> str | None:
    """The runner's reason for a failed declared stdout on a matched FAIL, or None.

    Hermit's runner checks a verify cell's declared ``expected_stdout`` and
    ``expected_stdout_contains`` after the canonical comparison matched and
    before the guest's exit, and records a failure only as the attempt's
    ``reason``: no timeout, no error kind, and possibly status 0. None: the
    reason is not one of those texts. A string: it is, and the evidence bears
    it out, so the attempt is a failed match. MalformedRow: it claims such a
    failure that the evidence contradicts.

    The evidence is what the runner decided from. For an exact
    ``expected_stdout`` that is the matched report's per-run stdout identity,
    so the captured ``stdout`` is not consulted. For
    ``expected_stdout_contains`` the runner searched the capture after
    requiring it to equal both compared runs, so a recorded capture must be
    those bytes and must omit the text. Hermit's runner records a capture it
    cannot read as UTF-8 as empty, so an empty capture of nonempty output is
    absent, not other bytes. The row does not carry the manifest's declared
    stdout, so the declared side is checked only for what it cannot be.
    """
    reason = attempt.get("reason")
    if not isinstance(reason, str):
        return None
    exact = _EXACT_STDOUT_REASON_RE.fullmatch(reason)
    contains = None if exact else _CONTAINS_STDOUT_REASON_RE.fullmatch(reason)
    claim = exact or contains
    if claim is None:
        return None
    text = None
    if contains is not None:
        text = _parse_rust_debug_ascii(contains["quoted"])
        if text is None:
            return None

    def contradiction(why: str) -> MalformedRow:
        return MalformedRow(
            f"cell attempt {index} reason claims a failed declared expected output "
            f"that contradicts its evidence: {why}"
        )

    if mode != "verify":
        raise contradiction(
            f"only a verify cell declares expected stdout, and this is a {mode} cell"
        )
    status = attempt.get("status")
    signal = attempt.get("signal")
    completed = (
        type(status) is int and status >= 0 and signal is None
    ) or (
        status is None and type(signal) is int and signal > 0
    )
    if (
        attempt.get("timed_out") is not False
        or attempt.get("error_kind") is not None
        or not completed
    ):
        raise contradiction(
            "the runner fails declared stdout only for an attempt that completed "
            "with a status or signal, no timeout and no error kind"
        )
    outputs = report.get("compared_outputs")
    if (
        not isinstance(outputs, dict)
        or set(outputs) != {"left", "right"}
        or not _is_canonical_compared_output(outputs["left"])
        or outputs["left"] != outputs["right"]
        or not all(
            outputs["left"][field] is None or type(outputs["left"][field]) is int
            for field in ("exit_code", "signal")
        )
    ):
        raise contradiction("the matched report has no identical, complete compared outputs")
    observed_bytes = int(claim["observed_bytes"])
    observed_sha256 = claim["observed_sha256"]
    if observed_bytes > _U64_MAX:
        raise contradiction("its observed byte count exceeds u64")
    left = outputs["left"]
    if left["stdout_bytes"] != observed_bytes or left["stdout_sha256"] != observed_sha256:
        raise contradiction(
            f"both compared runs printed {left['stdout_bytes']} bytes with sha256 "
            f"{left['stdout_sha256']}, not the stdout the reason names"
        )
    if exact is not None:
        # The runner names the first run whose stdout differs, and both runs
        # of a matched pair print the same bytes.
        if exact["run"] != "first":
            raise contradiction(
                f"it names the {exact['run']} run, but the first run of a matched "
                "pair printed the same bytes"
            )
        declared_bytes = int(exact["declared_bytes"])
        if declared_bytes > _U64_MAX:
            raise contradiction("its declared byte count exceeds u64")
        if declared_bytes == observed_bytes and exact["declared_sha256"] == observed_sha256:
            raise contradiction("the declared stdout is the observed stdout")
        return reason
    if not text:
        raise contradiction("the runner finds an empty declared text in any stdout")
    stdout = attempt.get("stdout")
    if stdout is None or (stdout == "" and observed_bytes > 0):
        return reason
    if not isinstance(stdout, str):
        raise contradiction("the captured stdout is not text")
    try:
        captured = stdout.encode("utf-8")
    except UnicodeEncodeError:
        raise contradiction("the captured stdout is not UTF-8 text") from None
    if (
        len(captured) != observed_bytes
        or hashlib.sha256(captured).hexdigest() != observed_sha256
    ):
        raise contradiction("the captured stdout is not the stdout the reason names")
    if text in stdout:
        raise contradiction("the captured stdout contains the declared text")
    return reason


#: The top-level fields ``VerificationReport::from_producer_json_value``
#: (``hermit-cli/src/canonical_verdict.rs``) requires present, null or not, in
#: every retained cell receipt. Hermit's scorecard reads cell receipts through
#: ``from_retained_cell_json_slice``, which requires these but not
#: ``compared_outputs``.
_RETAINED_REPORT_FIELDS = (
    "verified",
    "bitwise_parity",
    "verdict",
    "infrastructure_error",
    "comparison",
    "no_result_reason",
    "compared_log_messages",
    "guest_exit_code",
    "guest_signal",
    "first_divergent_scheduler_turn",
    "first_divergent_virtual_nanoseconds",
    "first_divergent_record",
    "first_divergent_syscall",
    "first_divergent_left_message",
    "first_divergent_right_message",
)

#: ``ComparisonReport``'s fields, which a current producer writes every one of.
_COMPARISON_FIELDS = (
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
)


def _is_rust_string(value: Any) -> bool:
    """Whether serde_json reads ``value`` as a Rust ``String``.

    Python's JSON reader accepts an unpaired surrogate escape, which is not
    valid UTF-8 and which serde_json refuses.
    """
    if not isinstance(value, str):
        return False
    try:
        value.encode("utf-8")
    except UnicodeEncodeError:
        return False
    return True


def _is_optional(value: Any, check) -> bool:
    return value is None or check(value)


def _is_str_in(value: Any, choices) -> bool:
    """Whether ``value`` is one of the string ``choices``.

    Row data may put an array or object where a string belongs; a bare ``in``
    against a set would raise ``TypeError`` for those instead of refusing.
    """
    return isinstance(value, str) and value in choices


def _is_u64_pair(value: Any) -> bool:
    """``ComparedLogMessages`` or ``DbtCountedBranchComparison``: two u64s.

    Neither Rust type denies unknown fields, so other fields are not refused.
    """
    return (
        isinstance(value, dict)
        and "left" in value
        and "right" in value
        and _is_u64(value["left"])
        and _is_u64(value["right"])
    )


def _is_typed_compared_output(output: Any) -> bool:
    """``ComparedOutput`` as serde reads it: exactly its fields, each typed.

    This is the type alone. ``_is_canonical_compared_output`` additionally
    requires the digest shape that ``require_exact_output_match`` checks for a
    matched report.
    """
    return (
        isinstance(output, dict)
        and set(output) == _COMPARED_OUTPUT_FIELDS
        and _is_optional_i32(output["exit_code"])
        and _is_optional_i32(output["signal"])
        and _is_rust_string(output["stdout_sha256"])
        and _is_rust_string(output["stderr_sha256"])
        and _is_u64(output["stdout_bytes"])
        and _is_u64(output["stderr_bytes"])
    )


def _is_typed_comparison(comparison: Any) -> bool:
    """``ComparisonReport`` as serde reads it. It does not deny unknown fields."""
    if not isinstance(comparison, dict):
        return False
    get = comparison.get
    optional_bools = (
        "compare_io_buffers", "virtualize_time", "strip_lines", "canonicalize_addresses",
        "full_trace", "exact_remainder", "ignore_lines", "skip_commit", "skip_detlog",
    )
    return (
        _is_str_in(get("strictness"), {"stripped", "canonical"})
        and _is_optional(get("display_name"), _is_rust_string)
        and type(get("compare_logs")) is bool
        and all(_is_optional(get(key), lambda v: type(v) is bool) for key in optional_bools)
        and _is_optional(
            get("log_scope"),
            lambda v: _is_str_in(v, {"deterministic", "info", "full_trace"}),
        )
        and _is_str_in(get("record_envelope"), {
            "all_records_v1", "dbt_evidence_transport_v1",
            "cross_backend_detcore_v1", "caller_defined",
        })
        and all(
            _is_optional(
                get(key),
                lambda v: isinstance(v, list) and all(_is_rust_string(item) for item in v),
            )
            for key in ("stripped_prefixes", "canonicalizations")
        )
    )


def _require_retained_comparison_report(report: dict, *, index: int) -> None:
    """Refuse a matched or diverged report Hermit's retained-cell reader refuses.

    Mirrors ``VerificationReport::from_retained_cell_json_slice`` in the pinned
    ``hermit-cli/src/canonical_verdict.rs``: every producer field present, the
    comparison's current fields present, and every present field of the type
    serde reads, whether or not a later check here looks at it. A field this
    writer does not otherwise read is still evidence the row's identity digests,
    so a report Hermit cannot parse must not be projected as if it could.
    Unknown top-level fields are not refused, as serde does not refuse them.
    """
    def refuse(detail: str) -> None:
        raise MalformedRow(
            f"cell attempt {index} verification report is not Hermit's typed "
            f"retained report: {detail}"
        )

    for field in _RETAINED_REPORT_FIELDS:
        if field not in report:
            refuse(f"missing producer field {field!r}")
    comparison = report["comparison"]
    if isinstance(comparison, dict):
        for field in _COMPARISON_FIELDS:
            if field not in comparison:
                refuse(f"comparison omitted {field!r}")
    if type(report["verified"]) is not bool or type(report["bitwise_parity"]) is not bool:
        refuse("verified and bitwise_parity must be booleans")
    if report["no_result_reason"] is not None:
        refuse("a comparison verdict carries no_result_reason")
    if report["infrastructure_error"] is not None:
        refuse("a comparison verdict carries infrastructure_error")
    if not _is_typed_comparison(comparison):
        refuse("comparison is not Hermit's typed ComparisonReport")
    if not _is_optional(report["compared_log_messages"], _is_u64_pair):
        refuse("compared_log_messages is not two u64 counts")
    if not _is_optional(report.get("dbt_counted_branches"), _is_u64_pair):
        refuse("dbt_counted_branches is not two u64 counts")
    if not _is_typed_runtime(report.get("runtime")):
        refuse("runtime is not Hermit's typed runtime")
    outputs = report.get("compared_outputs")
    if outputs is not None and not (
        isinstance(outputs, dict)
        and set(outputs) == {"left", "right"}
        and _is_typed_compared_output(outputs["left"])
        and _is_typed_compared_output(outputs["right"])
    ):
        refuse("compared_outputs is not Hermit's typed ComparedOutputs")
    for field in ("guest_exit_code", "guest_signal"):
        if not _is_optional_i32(report[field]):
            refuse(f"{field} is not an optional i32")
    for field in COORDINATE_KEYS:
        if not _is_optional(report[field], _is_u64):
            refuse(f"{field} is not an optional u64")
    for side in FIRST_DIVERGENT_MESSAGE_KEYS:
        field = f"first_divergent_{side}_message"
        if not _is_optional(report[field], _is_rust_string):
            refuse(f"{field} is not an optional string")


def _typed_comparison_attempt(
    attempt: Any,
    *,
    mode: str,
    index: int,
    expected_guest_exit: tuple[int | None, int | None] | None = None,
) -> str:
    """Return matched/diverged/no_result/timeout/errored/failed_match for one typed attempt.

    ``expected_guest_exit`` is the row's validated declaration from
    ``_expected_guest_exit``. Without one, a matched attempt must have exited
    0 (any status in chaos) and not by a signal. With one, a matched attempt
    must end exactly as declared and nothing else. A matched attempt the
    runner FAILed for a failed declared stdout, which
    :func:`_expected_output_failure_reason` verifies, is ``failed_match``
    however it ended: the runner checks declared stdout before the exit. A
    matched attempt the runner failed because it did not end as its row
    allows is ``failed_match`` too. Either is red evidence, never credit.
    """
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
    def unique_fields(pairs):
        fields = {}
        for name, value in pairs:
            if name in fields:
                raise MalformedRow(
                    f"cell attempt {index} verification report has duplicate field {name!r}"
                )
            fields[name] = value
        return fields

    def invalid_constant(value):
        raise MalformedRow(
            f"cell attempt {index} verification report has non-JSON constant {value}"
        )

    def finite_float(text):
        value = float(text)
        if not math.isfinite(value):
            raise MalformedRow(
                f"cell attempt {index} verification report has out-of-range number {text}"
            )
        return value

    try:
        # Read the evidence from the original bytes: an ordinary dict has
        # already collapsed duplicate keys to whichever came last, and Python
        # accepts NaN and Infinity, and parses 1e999 as infinity, none of which
        # Hermit's serde_json reads. Every later reader of this report relies
        # on this parse.
        report = json.loads(
            raw,
            object_pairs_hook=unique_fields,
            parse_constant=invalid_constant,
            parse_float=finite_float,
        )
    except ValueError as error:
        # JSONDecodeError, and an integer past Python's digit limit.
        raise MalformedRow(
            f"cell attempt {index} verification report is malformed: {error}"
        ) from error
    if not isinstance(report, dict):
        raise MalformedRow(f"cell attempt {index} verification report must be an object")
    if not _is_typed_runtime(report.get("runtime")):
        raise MalformedRow(
            f"cell attempt {index} verification report runtime is not Hermit's typed runtime"
        )

    verdict = report.get("verdict")
    if _is_str_in(verdict, {"matched", "diverged"}):
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
        if not (_is_u64(left) and _is_u64(right)):
            # Hermit's CompareLogMessageCounts are u64s.
            raise MalformedRow(
                f"cell attempt {index} INFO-message counts exceed u64"
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
            and _is_str_in(
                comparison.get("record_envelope"),
                {"all_records_v1", "dbt_evidence_transport_v1"},
            )
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
        if verdict == "matched":
            # Every structural and integrity check refuses before the attempt
            # is classified, canonical or not; only then can a valid match be
            # judged against the exit its row allows.
            guest = _require_matched_evidence(
                attempt, report, index=index, expected_guest_exit=expected_guest_exit
            )
        # After the specific checks above, so each keeps its own refusal; this
        # refuses every remaining field Hermit's typed reader would refuse.
        _require_retained_comparison_report(report, index=index)
        if not canonical:
            return "no_result"
        if verdict == "matched":
            if attempt.get("outcome") == "FAIL" and _expected_output_failure_reason(
                attempt, report, mode=mode, index=index
            ) is not None:
                return "failed_match"
            status = attempt.get("status")
            if expected_guest_exit is None:
                disposition_consistent = (
                    type(status) is int
                    and status >= 0
                    and (mode == "chaos" or status == 0)
                    and attempt.get("signal") is None
                )
            else:
                disposition_consistent = mode == "verify" and guest == expected_guest_exit
            if attempt.get("outcome") == "PASS":
                if (
                    attempt.get("timed_out") is not False
                    or attempt.get("error_kind") is not None
                    or not disposition_consistent
                ):
                    raise MalformedRow(
                        f"cell attempt {index} matched report is internally inconsistent"
                    )
                return "matched"
            # The runner's other red for a match: a declared cell that ended
            # differently, or an undeclared verify that exited nonzero, is an
            # attempt FAIL with no timeout or error kind inside a crash-error
            # row. It earns no credit but is kept as red evidence rather than
            # refusing every sibling, exactly as Hermit's scorecard reader
            # retains it. Only its contradictions refuse: a divergence
            # coordinate or first-divergent message, or an attempt that in
            # fact ended as its row allows.
            if (
                attempt.get("outcome") != "FAIL"
                or attempt.get("timed_out") is not False
                or attempt.get("error_kind") is not None
            ):
                raise MalformedRow(
                    f"cell attempt {index} matched report is internally inconsistent"
                )
            divergence_keys = (
                *COORDINATE_KEYS,
                *(f"first_divergent_{side}_message" for side in FIRST_DIVERGENT_MESSAGE_KEYS),
            )
            if any(report.get(key) is not None for key in divergence_keys) or any(
                attempt.get(key) is not None for key in divergence_keys
            ):
                raise MalformedRow(
                    f"cell attempt {index} matched report the runner failed carries a divergence coordinate"
                )
            if disposition_consistent:
                raise MalformedRow(
                    f"cell attempt {index} matched report is internally inconsistent: "
                    "the runner failed it without a failed declared stdout, but it ends "
                    "as its row allows"
                )
            return "failed_match"
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
            or not _is_str_in(attempt.get("outcome"), {"FAIL", "ERROR"})
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
        elif kind == "container_failed":
            _validate_container_failed_report(raw, attempt, index)
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


def _parity_object(value: Any, name: str, required: set[str],
                   optional: set[str] = frozenset(), *, extra: bool = False) -> dict:
    if (not isinstance(value, dict) or not required <= value.keys()
            or (not extra and value.keys() - required - optional)):
        raise MalformedRow(f"backend parity {name} has missing or unknown fields")
    return value


def _parity_uint(value: Any, name: str, minimum: int = 0) -> int:
    if type(value) is not int or not minimum <= value <= 2**64 - 1:
        raise MalformedRow(f"backend parity {name} is not a valid unsigned count")
    return value


def _parity_digest(value: Any, name: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise MalformedRow(f"backend parity {name} is not a SHA-256 digest")
    return value


def _parity_unique_fields(pairs):
    fields = {}
    for key, value in pairs:
        if key in fields:
            raise MalformedRow(f"backend parity evidence has duplicate field {key!r}")
        fields[key] = value
    return fields


def _parity_nonfinite_constant(value):
    raise MalformedRow(f"backend parity evidence contains non-JSON numeric constant {value}")


def _parity_output(value: Any) -> dict:
    output = dict(_parity_object(value, "output", {
        "stdout_sha256", "stdout_bytes",
        "stderr_sha256", "stderr_bytes",
    }, {"exit_code", "signal"}))
    output.setdefault("exit_code", None)
    output.setdefault("signal", None)
    for key in ("exit_code", "signal"):
        number = output[key]
        if number is not None and (type(number) is not int or not -2**31 <= number < 2**31):
            raise MalformedRow(f"backend parity output has invalid {key}")
    if (output["exit_code"] is None) == (output["signal"] is None):
        raise MalformedRow("backend parity output must have exactly one guest disposition")
    for stream in ("stdout", "stderr"):
        _parity_digest(output[f"{stream}_sha256"], f"{stream} digest")
        _parity_uint(output[f"{stream}_bytes"], f"{stream} bytes")
    return output


def _parity_verification(value: Any, *, current: bool) -> dict:
    """Canonical typed serialization of a matched VerificationReport.

    Keep the Rust optional-field serialization rules for evidence identity;
    invocation reports themselves retain their original bytes and digest.
    """
    nullable = {
        "no_result_reason", "infrastructure_error", "guest_exit_code", "guest_signal",
        *COORDINATE_KEYS, "first_divergent_left_message", "first_divergent_right_message",
    }
    required = {"verified", "bitwise_parity", "verdict", "comparison",
                "compared_log_messages", "compared_outputs"}
    value = _parity_object(value, "verification", required | (nullable if current else set()),
                           extra=True)
    report = {key: value.get(key) for key in required | nullable}
    if (report["verified"] is not True or report["bitwise_parity"] is not True
            or report["verdict"] != "matched"
            or report["no_result_reason"] is not None
            or report["infrastructure_error"] is not None
            or any(report[key] is not None for key in
                   (*COORDINATE_KEYS, "first_divergent_left_message", "first_divergent_right_message"))):
        raise MalformedRow("backend parity operand is not a consistent match")
    comparison = _parity_object(report["comparison"], "operand comparison", {
        "strictness", "display_name", "compare_logs", "compare_io_buffers", "log_scope",
        "record_envelope", "virtualize_time", "strip_lines", "canonicalize_addresses",
        "full_trace", "exact_remainder", "stripped_prefixes", "canonicalizations",
        "ignore_lines", "skip_commit", "skip_detlog",
    }, extra=True)
    # These known fields are exactly the ones serialized by ComparisonReport.
    report["comparison"] = {key: comparison[key] for key in (
        "strictness", "display_name", "compare_logs", "compare_io_buffers", "log_scope",
        "record_envelope", "virtualize_time", "strip_lines", "canonicalize_addresses",
        "full_trace", "exact_remainder", "stripped_prefixes", "canonicalizations",
        "ignore_lines", "skip_commit", "skip_detlog",
    )}
    for key in ("compare_logs", "compare_io_buffers", "virtualize_time", "strip_lines",
                "canonicalize_addresses", "full_trace", "exact_remainder", "ignore_lines",
                "skip_commit", "skip_detlog"):
        if type(comparison[key]) is not bool:
            raise MalformedRow(f"backend parity operand comparison has invalid {key}")
    counts = _parity_object(report["compared_log_messages"], "operand counts", {"left", "right"}, extra=True)
    report["compared_log_messages"] = {
        key: _parity_uint(counts[key], f"operand {key} INFO count", 1) for key in ("left", "right")
    }
    outputs = _parity_object(report["compared_outputs"], "compared outputs", {"left", "right"})
    left, right = (_parity_output(outputs[key]) for key in ("left", "right"))
    if (left != right or report["guest_exit_code"] != left["exit_code"]
            or report["guest_signal"] != left["signal"]
            or any(type(report[key]) is not type(left[key.removeprefix("guest_")])
                   for key in ("guest_exit_code", "guest_signal"))):
        raise MalformedRow("backend parity operand outputs or guest disposition disagree")
    report["compared_outputs"] = {"left": left, "right": right}
    branches = value.get("dbt_counted_branches")
    if branches is not None:
        branches = _parity_object(branches, "DBT counts", {"left", "right"}, extra=True)
        report["dbt_counted_branches"] = {
            key: _parity_uint(branches[key], f"DBT {key} count") for key in ("left", "right")
        }
    runtime = value.get("runtime")
    if runtime is not None:
        runtime = _parity_object(runtime, "runtime", set(), {"run1", "run2"}, extra=True)
        report["runtime"] = {}
        for key in ("run1", "run2"):
            if runtime.get(key) is not None:
                stats = _parity_object(runtime[key], key, {"scheduler_turns", "virtual_nanoseconds"},
                                       {"syscalls"}, extra=True)
                report["runtime"][key] = {
                    name: _parity_uint(stats[name], f"{key} {name}")
                    for name in ("scheduler_turns", "virtual_nanoseconds")
                }
                if stats.get("syscalls") is not None:
                    report["runtime"][key]["syscalls"] = _parity_uint(stats["syscalls"], f"{key} syscalls")
    return report


def _typed_backend_parity(row: dict, dispositions: list[str]) -> dict:
    """Authenticate BackendParityReport, separately from operand repeatability.

    Mirrors backend_parity.rs::validate, LogDiffReport's cross-backend policy,
    and scorecard.rs's attempt/outer-result binding. Returns typed serialization.
    """
    if row.get("mode") != "verify" or dispositions != ["matched", "matched"]:
        raise MalformedRow("backend parity requires exactly two strict matching verify attempts")
    report = copy.deepcopy(_parity_object(row.get("backend_parity"), "report", {
        "schema", "verdict", "reference", "candidate", "comparison",
    }))
    if _parity_uint(report["schema"], "schema") != 1:
        raise MalformedRow("backend parity report schema must be 1")
    backend = row.get("backend")
    if not isinstance(backend, str) or not backend or backend == "ptrace":
        raise MalformedRow("backend parity candidate must be the non-ptrace cell backend")
    for index, label, expected in ((0, "candidate", backend), (1, "reference", "ptrace")):
        operand = _parity_object(report[label], label, {
            "backend", "verification", "output", "retained_log", "retained_log_sha256",
        })
        attempt = row["attempts"][index]
        raw = json.loads(attempt["verification_report"], object_pairs_hook=_parity_unique_fields,
                         parse_constant=_parity_nonfinite_constant)
        actual = _parity_verification(raw, current=True)
        embedded = _parity_verification(operand["verification"], current=False)
        argv = attempt.get("argv")
        if not isinstance(argv, list) or not all(isinstance(arg, str) for arg in argv):
            raise MalformedRow("backend parity attempt has no argv")
        named = next((argv[i + 1] for i, arg in enumerate(argv[:-1]) if arg == "--backend"), None)
        if operand["backend"] != expected or named != expected or embedded != actual:
            raise MalformedRow("backend parity operand does not match its attempt report or backend")
        output = _parity_output(operand["output"])
        if output != actual["compared_outputs"]["left"]:
            raise MalformedRow("backend parity selected output differs from its verified output")
        if not isinstance(operand["retained_log"], str) or not operand["retained_log"].strip():
            raise MalformedRow("backend parity retained log path is empty")
        _parity_digest(operand["retained_log_sha256"], "retained log digest")
        operand["verification"] = embedded
        operand["output"] = output
    coordinates = {*COORDINATE_KEYS, "first_divergent_left_message", "first_divergent_right_message"}
    comparison = _parity_object(report["comparison"], "log comparison", {
        "schema", "verdict", "selected_messages", "records", "inputs", "comparison",
    }, coordinates | {"refusal", "follow_stopped_because"})
    if (_parity_uint(comparison["schema"], "log comparison schema") != 1
            or comparison["verdict"] not in ("matched", "diverged")
            or comparison.get("refusal") is not None
            or comparison.get("follow_stopped_because") is not None):
        raise MalformedRow("backend parity log comparison has no complete verdict")
    policy = {
        "stream": "info", "record_envelope": "cross_backend_detcore_v1",
        "unsafe_strip_lines": False, "canonicalize_host_addresses": True,
        "require_structured_events": True, "ignored_line_substrings": [],
        "skip_commit": False, "skip_detlog": False,
        "included_detlog_kinds": ["syscall", "syscall_result", "other"], "git_diff": False,
    }
    actual_policy = _parity_object(comparison["comparison"], "log policy", set(policy))
    if any(type(actual_policy[key]) is not type(value) or actual_policy[key] != value
           for key, value in policy.items()):
        raise MalformedRow("backend parity log comparison did not use canonical INFO policy")
    inputs = _parity_object(comparison["inputs"], "captured inputs", {"left", "right"})
    selected = _parity_object(comparison["selected_messages"], "selected messages", {"left", "right"})
    records = _parity_object(comparison["records"], "records", {
        "compared", "available_left", "available_right", "withheld_incomplete_tail",
    })
    for side, label in (("left", "reference"), ("right", "candidate")):
        captured = _parity_object(inputs[side], f"{side} input", {"sha256", "bytes"})
        _parity_uint(captured["bytes"], f"{side} input bytes", 1)
        if _parity_digest(captured["sha256"], f"{side} input hash") != report[label]["retained_log_sha256"]:
            raise MalformedRow("backend parity retained log differs from captured input hash")
        count = _parity_uint(selected[side], f"{side} selected messages", 1)
        if count > _parity_uint(records[f"available_{side}"], f"{side} available records", 1):
            raise MalformedRow("backend parity selected messages exceed available records")
    if (records["withheld_incomplete_tail"] is not False
            or _parity_uint(records["compared"], "compared records", 1)
            != min(records["available_left"], records["available_right"])
            or (comparison["verdict"] == "matched" and selected["left"] != selected["right"])):
        raise MalformedRow("backend parity log comparison is incomplete or has inconsistent counts")
    for key in COORDINATE_KEYS:
        if comparison.get(key) is not None:
            _parity_uint(comparison[key], key)
        if row.get(key) is not None:
            _parity_uint(row[key], f"outer {key}")
    for key in ("first_divergent_left_message", "first_divergent_right_message"):
        if comparison.get(key) is not None and not isinstance(comparison[key], str):
            raise MalformedRow("backend parity divergent message must be a string")
    matched = (report["reference"]["output"] == report["candidate"]["output"]
               and comparison["verdict"] == "matched")
    expected = ("matched", "PASS", "pass", None) if matched else (
        "diverged", "FAIL", "parity-failure", "product_failure")
    if ((report["verdict"], row.get("outcome"), row.get("result"), row.get("failure_class")) != expected
            or any(row.get(key) != (None if matched else comparison.get(key)) for key in coordinates)):
        raise MalformedRow("backend parity witness contradicts the outer result or divergence details")
    # Match serde's optional-field serialization, without rewriting retained
    # paths or any comparator evidence using the invocation's workspace root.
    for key in coordinates | {"refusal", "follow_stopped_because"}:
        if key in {"refusal", "follow_stopped_because", "first_divergent_record"}:
            if comparison.get(key) is None:
                comparison.pop(key, None)
        else:
            comparison.setdefault(key, None)
    return report


def _cell_outcome(row: dict) -> str:
    """Project the framework's exact result without reconstructing it."""
    mode = _cell_mode(row)
    expected_guest_exit = _expected_guest_exit(row)
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
    # Checked as a string first: a list or object here is malformed data, and
    # set membership would otherwise raise TypeError on the unhashable value.
    if not isinstance(outcome, str) or outcome not in allowed_cell_outcomes:
        raise MalformedRow(
            "cell result outcome disagrees with its recorded classification: "
            f"outcome={outcome!r} result={result!r} failure_class={failure_class!r}"
        )
    has_parity = row.get("backend_parity") is not None
    if (has_parity or result == "parity-failure") and mode != "verify":
        raise MalformedRow("backend parity requires verify mode")
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
        _typed_comparison_attempt(
            attempt, mode=mode, index=index, expected_guest_exit=expected_guest_exit
        )
        for index, attempt in enumerate(attempts, 1)
    ]
    if "failed_match" in dispositions:
        # A retained failed match is the runner's crash-error red. Hermit's
        # runner types a matched attempt it FAILed as crash-error, and the
        # shared failed_match evidence requires exactly that result, so it
        # cannot sit in any other row, or beside a divergence.
        if classification != ("crash-error", "product_failure"):
            raise MalformedRow(
                f"row retaining a matched attempt the runner failed records result {result!r} "
                f"with failure_class {failure_class!r}, but that attempt supports only a "
                "crash-error result"
            )
        if any(row.get(key) is not None for key in COORDINATE_KEYS):
            raise MalformedRow(
                "row retaining a matched attempt the runner failed carries a divergence coordinate"
            )
        if "diverged" in dispositions:
            # crash-error accepts failed_match as support, so a diverged
            # sibling would otherwise be erased from the row's verdict.
            raise MalformedRow(
                "row retaining a matched attempt the runner failed also carries a diverged attempt"
            )
    if has_parity:
        _typed_backend_parity(row, dispositions)
        return projected
    if result == "parity-failure":
        raise MalformedRow("parity-failure has no backend parity witness")
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
        ("crash-error", "product_failure"): {"no_result", "failed_match"},
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
    """Keep ordinary stored identities; bind parity with the scorecard's typed witness."""
    declared = None
    if _expected_guest_exit(row) is not None:
        declared = row["expected_guest_exit"]
    parity = None
    if row.get("backend_parity") is not None:
        parity = _typed_backend_parity(row, [
            _typed_comparison_attempt(
                attempt,
                mode=_cell_mode(row),
                index=index,
                expected_guest_exit=_expected_guest_exit(row),
            )
            for index, attempt in enumerate(row.get("attempts", []), 1)
        ])
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
    # Preserve historical ordinary-row identities. Rust also hashes an absent
    # witness as null; that preexisting no-parity mismatch remains explicit.
    if parity is not None:
        evidence["backend_parity"] = parity
    # A declaration authorizes converting a matched nonzero exit from a
    # refusal into a pass, so the complete checked declaration is part of the
    # identity: removing or changing it cannot keep the accepted row's hash.
    # Undeclared rows keep their historical identity, exactly as ResultRow's
    # Rust identity does.
    if declared is not None:
        evidence["expected_guest_exit"] = {
            "code": declared["code"],
            "reason": declared["reason"],
            "signal": declared["signal"],
        }
    encoded = json.dumps(
        evidence, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
    try:
        data = encoded.encode()
    except UnicodeEncodeError as error:
        # A lone surrogate: Python accepts it; serde_json refuses it.
        raise MalformedRow(f"cell result is not valid Unicode: {error}") from error
    return hashlib.sha256(data).hexdigest()


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
    if verdict == "matched" and disposition == "failed_match":
        return "failed_match"
    if _is_str_in(verdict, {"matched", "diverged"}) and disposition == "no_result":
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
            disposition = _typed_comparison_attempt(
                attempt,
                mode=mode,
                index=index,
                expected_guest_exit=_expected_guest_exit(row),
            )
            if disposition == "failed_match":
                # Pressure evidence types every matched report as a canonical
                # pass, so it cannot say that the runner failed this one.
                raise MalformedRow(
                    f"pressure inner attempt {index} is a matched attempt failed by its "
                    "declared expected output, which pressure evidence cannot represent"
                )
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


def _declared_guest_exit_evidence(row: dict) -> dict | None:
    """Retain the declaration a verify cell was judged under, for audit.

    ``expected_guest_exit`` is what lets a matched attempt with a nonzero
    guest exit pass instead of being refused. A published row must therefore
    say which declaration applied, bind it to the exact source evidence
    identity (which includes the declaration), and keep each attempt's
    process disposition so a reader can see what was admitted.
    """
    if _expected_guest_exit(row) is None:
        return None
    declared = row["expected_guest_exit"]
    attempts = row.get("attempts")
    if not isinstance(attempts, list) or not attempts:
        raise _UntypedComparisonCell("comparison cell result has no typed attempts")
    retained = []
    for ordinal, attempt in enumerate(attempts, 1):
        if not isinstance(attempt, dict):
            raise MalformedRow(f"cell attempt {ordinal} must be an object")
        index = attempt.get("index")
        outcome = attempt.get("outcome")
        timed_out = attempt.get("timed_out")
        report_sha = attempt.get("verification_report_sha256")
        if not isinstance(index, str) or not index.strip():
            raise MalformedRow(f"cell attempt {ordinal} has no nonempty typed index")
        if not isinstance(outcome, str) or not outcome.strip():
            raise MalformedRow(f"cell attempt {ordinal} has no nonempty outcome")
        if not isinstance(timed_out, bool):
            raise MalformedRow(f"cell attempt {ordinal} timed_out must be a bool")
        if report_sha is not None and (
            not isinstance(report_sha, str) or _SHA256_RE.fullmatch(report_sha) is None
        ):
            raise MalformedRow(
                f"cell attempt {ordinal} verification_report_sha256 must be 64-hex or null"
            )
        for name in ("status", "signal"):
            value = attempt.get(name)
            if value is not None and (type(value) is not int or value < 0):
                raise MalformedRow(
                    f"cell attempt {ordinal} {name} has invalid value {value!r}"
                )
        retained.append({
            "index": index,
            "outcome": outcome,
            "status": attempt.get("status"),
            "signal": attempt.get("signal"),
            "timed_out": timed_out,
            "verification_report_sha256": report_sha,
        })
    return {
        "code": declared["code"],
        "signal": declared["signal"],
        "reason": declared["reason"],
        "evidence_sha256": _cell_result_evidence_sha256(row),
        "attempts": retained,
    }


def _no_verdict_evidence(row: dict, projected_outcome: str) -> dict | None:
    """Retain typed non-comparison dispositions without reconstructing one."""
    mode = row.get("mode")
    if not _is_str_in(mode, _COMPARISON_MODES) or not _is_str_in(projected_outcome, {
        "no_result", "timeout", "errored",
    }):
        return None
    attempts = row.get("attempts")
    if not isinstance(attempts, list) or not attempts:
        raise _UntypedComparisonCell("comparison cell result has no typed attempts")

    dispositions = []
    for ordinal, attempt in enumerate(attempts, 1):
        kind = _typed_comparison_attempt(
            attempt,
            mode=mode,
            index=ordinal,
            expected_guest_exit=_expected_guest_exit(row),
        )
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
            # A failed match is the crash-error red, so its disposition is
            # the errored outcome that result projects to.
            "disposition": "errored" if kind == "failed_match" else kind,
            "error_kind": error_kind,
            "status": status,
            "signal": signal,
            "timed_out": timed_out,
            "verification_report_sha256": report_sha,
        }
        if retained["kind"] == "comparison_refused":
            report = json.loads(attempt["verification_report"])
            retained["detail"] = report["no_result_reason"]["detail"]
        elif retained["kind"] == "failed_match":
            # The runner's reason for a failed declared stdout is the only
            # record of that failure; _typed_comparison_attempt verified it
            # above. Hermit's shared checker admits no other detail: a match
            # the runner failed by its exit is told by its status or signal
            # and the row's declaration.
            report = json.loads(attempt["verification_report"])
            if _expected_output_failure_reason(
                attempt, report, mode=mode, index=ordinal
            ) is not None:
                retained["detail"] = attempt["reason"]
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
    return collapse_rows_with_sources(rows, [[index] for index in range(len(rows))])[0]


def collapse_rows_with_sources(
    rows: Sequence[dict], sources: Sequence[Sequence[int]],
) -> tuple[list[dict], list[list[int]]]:
    """:func:`collapse_rows`, also returning which ``sources`` each output folds.

    ``sources[i]`` names the inputs behind ``rows[i]``; the second result
    names, for each collapsed row, the union of its folded rows' sources.
    """
    if len(sources) != len(rows):
        raise SeriesError("collapse_rows_with_sources needs one source list per row")
    order = sorted(
        range(len(rows)),
        key=lambda index: (
            str((rows[index].get("series") or {}).get("cell", "")),
            int((rows[index].get("series") or {}).get("run_index", -1)),
            int((rows[index].get("series") or {}).get("attempt", 1)),
        ),
    )
    collapsed: list[dict] = []
    merged: list[list[int]] = []
    collapsible: dict[tuple[str, str], int] = {}
    for index in order:
        row = rows[index]
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
            merged[previous_index].extend(sources[index])
            continue
        collapsed.append(json.loads(json.dumps(row)))
        merged.append(list(sources[index]))
        if "attempt" not in payload:
            collapsible[key] = len(collapsed) - 1
    return collapsed, merged


def _rows_from_cell_results(
    cell_results: Sequence[dict], *, parent: Any, producer: str, run_id: str,
    tree: str, emitted_at: str, hermit_checkout: Any | None = None,
    quarantine: list[dict] | None, input_numbers: Sequence[int] | None = None,
    raw_inputs: Sequence[bytes] | None = None, provenance: list[list[int]] | None = None,
) -> list[dict]:
    """Convert typed harness results into validated, compressed series rows.

    With ``quarantine=None`` any refused cell result refuses the batch. With a
    list, each cell result is judged on its own: one the projection refuses as
    malformed data (:class:`MalformedRow`) is recorded in ``quarantine`` with
    its typed reason and its exact received bytes (``raw_inputs``, required
    then), and the independent valid results are still returned. Any other
    exception is a defect in this code, not in the data, and propagates.
    Refusals that concern the invocation rather than any one row (producer,
    run id, tree, source ancestry and depth, the recorded Detcore tree, or a
    checker that cannot run) still raise, because no row could be written
    correctly. ``input_numbers`` names each cell result's original input line;
    by default it is the 1-based position in the batch. ``provenance``, when
    given, receives for each returned row the indices of the cell results it
    folds.
    """
    if input_numbers is not None and len(input_numbers) != len(cell_results):
        raise SeriesError("input_numbers must name every cell result")
    if quarantine is not None and (raw_inputs is None or len(raw_inputs) != len(cell_results)):
        raise SeriesError("quarantining needs the exact received bytes of every cell result")

    def received(index: int) -> dict:
        number = input_numbers[index] if input_numbers is not None else index + 1
        assert raw_inputs is not None
        return quarantine_input(number, raw_inputs[index])
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
    declared_exits: list[dict] = []

    def manifest_declarations() -> dict:
        # Read once, and only when some row declares a guest exit.
        if not declared_exits:
            declared_exits.append(manifest_guest_exit_declarations(source, tree))
        return declared_exits[0]

    rows = []
    row_sources: list[list[int]] = []
    pressure_counts: dict[tuple[str, int], int] = {}
    if producer in {"pressure-test", "hermit-repeat"}:
        for cell_result in cell_results:
            if not isinstance(cell_result, dict):
                continue
            if cell_result.get("outcome") == "HOST-INAPPLICABLE":
                continue
            run_index = cell_result.get("run_index")
            if isinstance(run_index, int) and not isinstance(run_index, bool):
                try:
                    cell = _cell_name(cell_result)
                except MalformedRow:
                    if quarantine is None:
                        raise
                    # Quarantined with its exact reason by the loop below.
                    continue
                key = (cell, run_index)
                pressure_counts[key] = pressure_counts.get(key, 0) + 1
    for position, cell_result in enumerate(cell_results, 1):
        # An unmet prerequisite is recorded by the validate-level ledger. It
        # produced no execution and therefore produces no cell observation.
        if isinstance(cell_result, dict) and cell_result.get("outcome") == "HOST-INAPPLICABLE":
            continue
        try:
            if not isinstance(cell_result, dict):
                raise MalformedRow(
                    f"cell result must be an object, got {type(cell_result).__name__}"
                )
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
            _bind_expected_guest_exit(cell_result, tree, manifest_declarations)
            runtime = _cell_runtime(cell_result)
            projected_outcome = _cell_outcome(cell_result)
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
            declared_guest_exit = _declared_guest_exit_evidence(cell_result)
            if declared_guest_exit is not None:
                # The audit binds one outer attempt's evidence identity, so the
                # row names that attempt and is never collapsed with another.
                payload["attempt"] = run_index if producer == "validate" else attempt
                payload["declared_guest_exit"] = declared_guest_exit
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
            row_sources.append([position - 1])
        except MalformedRow as error:
            # Only the typed data refusal is quarantined. A TypeError, KeyError,
            # AttributeError or ValueError escaping the projection is a defect
            # in this code; catching it here would file the bug as bad data.
            if quarantine is None:
                raise
            quarantine.append(quarantine_record(
                stage="projection",
                run_id=run_id,
                producer=producer,
                tree=tree,
                emitted_at=emitted_at,
                error=type(error).__name__,
                reason=str(error),
                inputs=[received(position - 1)],
                cell=_quarantine_cell(cell_result),
            ))

    collapsed, collapsed_sources = collapse_rows_with_sources(rows, row_sources)
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
    if quarantine is None:
        validate_rows(collapsed)
        if provenance is not None:
            provenance.extend(collapsed_sources)
        return collapsed
    admitted = []
    for row, sources, verdict in zip(collapsed, collapsed_sources, write_admission(collapsed)):
        if verdict is None:
            admitted.append(row)
            if provenance is not None:
                provenance.append(list(sources))
            continue
        quarantine.append(quarantine_record(
            stage="shared-checker",
            run_id=run_id,
            producer=producer,
            tree=tree,
            emitted_at=emitted_at,
            error="SharedCheckerRefusal",
            reason=verdict,
            inputs=[received(index) for index in sources],
            row=row,
            row_event_id=str(row.get("event_id", "")),
            cell=_quarantine_cell(row),
        ))
    return admitted


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
        quarantine=None,
    )


__all__ = [
    "COORDINATE_KEYS",
    "ENVELOPE_VERSION",
    "FIRST_DIVERGENT_MESSAGE_KEYS",
    "HOST_FACTS_ENVELOPE_VERSION",
    "LEGACY_ENVELOPE_VERSION",
    "READABLE_ENVELOPE_VERSIONS",
    "SERIES_SPOOL_LOCK_TIMEOUT_SECONDS",
    "MalformedRow",
    "ProjectionResults",
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
    "projection_results",
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


def _stdin_unique_fields(pairs):
    fields = {}
    for key, value in pairs:
        if key in fields:
            raise MalformedRow(f"duplicate field {key!r}")
        fields[key] = value
    return fields


def _stdin_nonjson_constant(value):
    raise MalformedRow(f"non-JSON numeric constant {value}")


def _stdin_finite_float(text):
    value = float(text)
    if not math.isfinite(value):
        raise MalformedRow(f"out-of-range number {text}")
    return value


def _stdin_rows() -> tuple[list[dict] | None, int]:
    rows = []
    for number, line in enumerate(sys.stdin.read().splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
            if isinstance(row, dict) and "backend_parity" in row:
                row = json.loads(line, object_pairs_hook=_parity_unique_fields,
                                 parse_constant=_parity_nonfinite_constant,
                                 parse_float=_stdin_finite_float)
            # serde_json refuses a lone surrogate anywhere in a row.
            json.dumps(row, ensure_ascii=False).encode()
            rows.append(row)
        except (ValueError, MalformedRow) as error:
            # ValueError covers JSONDecodeError, an integer past Python's digit
            # limit and UnicodeEncodeError.
            print(f"series: stdin line {number} is not JSON: {error}", file=sys.stderr)
            return None, 2
    return rows, 0


def _stdin_cell_results() -> tuple[
    list[tuple[int, Any, bytes]], list[tuple[int, bytes, str, str]]
]:
    """Read stdin as bytes, separating parsed lines from lines that are not JSON.

    Returns ``(parsed, failures)``: ``parsed`` holds ``(line_number, value,
    raw)`` and ``failures`` holds ``(line_number, raw, error_type, reason)``.
    ``raw`` is the exact received line, without only its ``\\n`` terminator.
    Lines are split on ``\\n`` alone: ``str.splitlines`` would also split a
    JSON string at U+2028, U+0085 or a form feed, which a JSON writer may emit
    unescaped. Lines of ASCII whitespace are skipped. Nothing is refused here;
    the caller quarantines failures.

    Every line is read refusing duplicate fields, non-JSON constants, and
    numbers that overflow to infinity (``1e999``) in every field of every row,
    as Hermit's serde_json does. A duplicate ``expected_guest_exit`` or
    ``attempts`` would otherwise be collapsed before validation sees it.
    """
    buffer = getattr(sys.stdin, "buffer", None)
    data = buffer.read() if buffer is not None else sys.stdin.read().encode("utf-8")
    lines = data.split(b"\n")
    if lines and lines[-1] == b"":
        lines.pop()
    parsed: list[tuple[int, Any, bytes]] = []
    failures: list[tuple[int, bytes, str, str]] = []
    for number, raw in enumerate(lines, 1):
        if not raw.strip():
            continue
        try:
            line = raw.decode("utf-8")
            row = json.loads(line, object_pairs_hook=_stdin_unique_fields,
                             parse_constant=_stdin_nonjson_constant,
                             parse_float=_stdin_finite_float)
            # serde_json refuses a lone surrogate anywhere in a row.
            json.dumps(row, ensure_ascii=False).encode()
        except (ValueError, RecursionError, MalformedRow) as error:
            # ValueError covers UnicodeDecodeError, JSONDecodeError, an integer
            # past Python's digit limit and UnicodeEncodeError.
            failures.append((
                number, raw, type(error).__name__, f"stdin line {number} is not JSON: {error}",
            ))
            continue
        parsed.append((number, row, raw))
    return parsed, failures


def _quarantine_summary(record: dict) -> str:
    """One refusal without its content: identity, reason, and input digests."""
    summary = {
        key: value for key, value in record.items()
        if key not in {"inputs", "row", "legacy"}
    }
    summary["inputs"] = [
        {name: item.get(name) for name in ("input_row", "raw_sha256", "raw_bytes")}
        for item in record.get("inputs") or []
    ]
    return json.dumps(summary, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


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


def _parity_stdin_bytes() -> bytes:
    buffer = getattr(sys.stdin, "buffer", None)
    return buffer.read() if buffer is not None else sys.stdin.read().encode("utf-8")


def _parity_stdin_lines(data: bytes) -> int | None:
    """How many lines stdin held, as `_parity_stdin_sources` splits it; None if not UTF-8."""
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return None
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return len(lines)


def _parity_stdin_sources(parity, data: bytes) -> list[Any]:
    """One `ParityLedgerSource` per stdin line, in order; strict JSON.

    Unlike `append-cells`, nothing is quarantined: parity rows are derived from
    files hermit already retains, so a refused batch is re-exportable
    (`test-harness parity export`). It is refused whole, loudly, and the
    refusal is recorded as a `parity.refusal`.
    """
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise parity.ParityLedgerError(f"stdin is not UTF-8: {error}") from error
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    sources = []
    for number, line in enumerate(lines, 1):
        if not line.strip():
            raise parity.ParityLedgerError(f"stdin line {number} is blank")
        sources.append(parity.loads(line, f"stdin line {number}"))
    return sources


def _record_parity_refusal(parity, spool: Path, *, producer: str, run_id: str, tree: str,
                           emitted_at: str, dirty: bool | None, data: bytes, error: Any) -> str:
    """Spool a `parity.refusal` for this call; a sentence naming where it went.

    A refusal that itself cannot be recorded (an identity the ledger cannot
    hold, an unwritable spool) is named, never raised: the caller's own
    refusal is what the exit status reports.
    """
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", emitted_at) is None:
        # A malformed --emitted-at is itself a refusal; the refusal is dated now.
        emitted_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    try:
        event = parity.build_refusal(
            producer=producer, run_id=run_id, hermit_sha=tree, emitted_at=emitted_at,
            source_tree_dirty=dirty, host=_writer_host(), input_bytes=data,
            input_lines=_parity_stdin_lines(data), error=error,
        )
        with series_spool_transition_lock(spool, exclusive=True):
            outcome = parity.append_refusal(spool, event)
    except (parity.ParityLedgerError, MalformedQuarantine, OSError, SeriesError) as failure:
        return f"the refusal could NOT be recorded: {failure}"
    if outcome["batch"] is None:
        return f"the refusal is already recorded ({outcome['recorded_in']})"
    return f"the refusal is recorded -> {spool / outcome['batch']}"


def _cmd_append_parity(argv: list[str]) -> int:
    """Spool one run's parity rows: `parity: ...` on stdout, a refusal on stderr.

    Exit 0 when the batch was written or was already recorded identically,
    1 when the rows were refused, 2 for a usage error or an unwritable spool.
    A refusal of a call that names a valid producer, run and tree is spooled
    as a `parity.refusal` (see parity_ledger.py), so the run reads as refused
    rather than as never attempted.

    `--help` or `-h` alone prints APPEND_PARITY_USAGE on stdout, exits 0 and
    reads and writes nothing: hermit's parity export probes the writer that
    way before it sends rows. Help with any other argument is a usage error
    (exit 2), and nothing is read or written either.
    """
    if argv in (["--help"], ["-h"]):
        print(APPEND_PARITY_USAGE)
        return 0
    if any(argument in {"-h", "--help"} for argument in argv):
        return _append_parity_usage_error("help cannot be combined with other arguments")
    options: dict[str, str] = {}
    flags = APPEND_PARITY_FLAGS
    i = 0
    while i < len(argv):
        name = flags.get(argv[i])
        if name is None or i + 1 >= len(argv):
            return _usage(f"unknown append-parity argument {argv[i]!r}")
        if name in options:
            return _usage(f"append-parity supplies {argv[i]} more than once")
        options[name] = argv[i + 1]
        i += 2
    missing = [
        flag for flag, name in flags.items()
        if name in {"parent", "producer", "run_id", "tree"} and not options.get(name)
    ]
    if missing:
        return _usage(f"append-parity requires {', '.join(missing)}")
    emitted_at = options.get("emitted_at") or datetime.now(timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    parity = parity_ledger_module()
    parent = options["parent"]
    spool = parity_spool_dir(parent)
    try:
        data = _parity_stdin_bytes()
    except OSError as error:
        print(f"series: cannot read the parity rows on stdin: {error}", file=sys.stderr)
        return 2
    # Where no producer, run and tree can be named, no refusal can say whose it is.
    identified = (
        options["producer"] in parity.PRODUCERS
        and options["run_id"].strip() and "\0" not in options["run_id"]
        and re.fullmatch(r"[0-9a-f]{40}", options["tree"]) is not None
    )

    def refuse(error: Any, dirty: bool | None, status: int) -> int:
        recorded = (
            _record_parity_refusal(
                parity, spool, producer=options["producer"], run_id=options["run_id"],
                tree=options["tree"], emitted_at=emitted_at, dirty=dirty, data=data, error=error,
            )
            if identified else "the refusal is not recorded: no valid producer, run and tree name it"
        )
        if status == 2:
            _usage(str(error))
            print(f"series: {recorded}", file=sys.stderr)
            return 2
        print(f"series: REFUSED the parity batch, no row was written; {recorded}: {error}",
              file=sys.stderr)
        return status

    # Like append-cells, the tree state is evidence and is never defaulted:
    # a row stamped clean without a caller saying so would be admitted as
    # clean evidence by every scorecard that excludes dirty rows.
    dirty_text = options.get("source_tree_dirty")
    if dirty_text is None:
        return refuse("append-parity requires --source-tree-dirty true|false", None, 2)
    if dirty_text not in {"true", "false"}:
        return refuse(
            f"append-parity --source-tree-dirty must be true or false, got {dirty_text!r}", None, 2
        )
    dirty = dirty_text == "true"
    try:
        host = _writer_host()
        sources = _parity_stdin_sources(parity, data)
    except (MalformedQuarantine, parity.ParityLedgerError) as error:
        return refuse(error, dirty, 1)
    # What the ledger already holds for this run, so a replay is recognised
    # and a changed record is refused even after its batch was drained. An
    # unreadable ledger is named, not fatal: the publisher repeats this check
    # against the ledger it pushes to.
    published: dict[str, dict] = {}
    published_check: str | None = None
    try:
        at = parity.published_parity_at_repo(ledger_root(parent), "refs/remotes/origin/main")
        published = at.events
        published_check = f"read {len(published)} published parity event(s) at {at.commit}"
        if at.invalid:
            published_check += f"; {len(at.invalid)} published entr(ies) could not be indexed"
    except (SeriesError, parity.ParityLedgerError, OSError, subprocess.SubprocessError) as error:
        published_check = f"published ledger not consulted: {error}"
    try:
        with series_spool_transition_lock(spool, exclusive=True):
            result = parity.append_parity(
                spool, sources,
                producer=options["producer"], run_id=options["run_id"],
                hermit_sha=options["tree"], emitted_at=emitted_at,
                source_tree_dirty=dirty, host=host,
                published=published, published_check=published_check,
            )
    except parity.ParityLedgerError as error:
        return refuse(error, dirty, 1)
    except (OSError, SeriesError) as error:
        print(f"series: cannot write the parity spool {spool}: {error}", file=sys.stderr)
        return 2
    counts = ", ".join(
        f"{verdict} {count}" for verdict, count in result["verdicts"].items() if count
    ) or "no rows"
    subject = f"{result['producer']} run {result['run_id']}"
    if result["already_recorded"]:
        print(
            f"parity: {result['records']} record(s) for {subject} are already recorded "
            f"({', '.join(result['recorded_in'])}); nothing written"
        )
    elif result["records"] == 0:
        print(
            f"parity: recorded an empty parity selection for {subject} "
            f"-> {spool / result['batch']}"
        )
    else:
        print(
            f"parity: appended {result['records']} record(s) ({counts}) for {subject} "
            f"-> {spool / result['batch']}"
        )
    print(f"parity: {published_check}", file=sys.stderr)
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
    producer, run_id, tree = str(producer), str(run_id), str(tree)
    if emitted_at is not None and _QUARANTINE_EMITTED_AT_RE.fullmatch(emitted_at) is None:
        return _usage(f"append-cells --emitted-at must be a UTC timestamp, got {emitted_at!r}")
    try:
        host = _writer_host()
    except MalformedQuarantine as error:
        print(f"series: REFUSED the cell batch, nothing was written: {error}", file=sys.stderr)
        return 2
    parsed, failures = _stdin_cell_results()
    if not parsed and not failures:
        print("series: no cell results on stdin; nothing appended", file=sys.stderr)
        return 1
    emitted_at = emitted_at or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    total = len(parsed) + len(failures)

    def record(stage: str, error: str, reason: str, inputs: list[dict], *,
               row: dict | None = None) -> dict:
        return quarantine_record(
            stage=stage, run_id=run_id, producer=producer, tree=tree,
            emitted_at=emitted_at, error=error, reason=reason, inputs=inputs,
            row=row, row_event_id=None if row is None else str(row.get("event_id", "")),
            cell=_quarantine_cell(row), host=host,
        )

    def parse_records() -> list[dict]:
        return [
            record("parse", error_type, reason, [quarantine_input(number, raw)])
            for number, raw, error_type, reason in failures
        ]

    # Each cell result is judged on its own. A refused one is quarantined with
    # its exact reason and received bytes; the independent valid results beside
    # it are still appended. Only a refusal about the invocation itself (not
    # any one row) keeps every row out, and then every row is quarantined too.
    quarantined = parse_records()
    series_rows: list[dict] = []
    provenance: list[list[int]] = []
    written = 0
    target = None
    batch_error = None
    exit_on_error = 1
    try:
        if parsed:
            series_rows = _rows_from_cell_results(
                [value for _, value, _ in parsed],
                parent=parent,
                producer=producer,
                run_id=run_id,
                tree=tree,
                emitted_at=emitted_at,
                hermit_checkout=hermit_checkout,
                quarantine=quarantined,
                input_numbers=[number for number, _, _ in parsed],
                raw_inputs=[raw for _, _, raw in parsed],
                provenance=provenance,
            )
        if series_rows:
            conflicts: list[tuple[int, dict, str]] = []
            target = spool_dir(parent) / _batch_name(series_rows)
            target.parent.mkdir(parents=True, exist_ok=True)
            written = append_cell_rows(parent, target, series_rows, conflicts=conflicts)
            conflicted = {position for position, _, _ in conflicts}
            for position, row, reason in conflicts:
                quarantined.append(record(
                    "spool-conflict", "SpoolConflict", reason,
                    [quarantine_input(parsed[index][0], parsed[index][2])
                     for index in provenance[position]],
                    row=row,
                ))
            series_rows = [
                row for position, row in enumerate(series_rows) if position not in conflicted
            ]
    except (MalformedEvent, SeriesError) as error:
        batch_error = f"series: REFUSED the cell batch, nothing was written: {error}"
        batch_error_type = type(error).__name__
    except OSError as error:
        batch_error = f"series: cannot write the spool: {error}"
        batch_error_type = type(error).__name__
        exit_on_error = 2
    except Exception as error:
        # A defect in this writer, not a refusal of the data. Retain every
        # received input exactly, so the run's cells survive the bug, and then
        # let the exception propagate with its traceback and a nonzero exit.
        detail = (
            f"series writer failed with {type(error).__name__}: {error}; "
            f"{written} row(s) had been appended to {target}"
        )
        retained = parse_records() + [
            record("writer-error", type(error).__name__, detail,
                   [quarantine_input(number, raw)])
            for number, _, raw in parsed
        ]
        try:
            outcome = append_quarantine(parent, run_id, retained)
            print(
                f"series: WRITER ERROR; retained {len(retained)} input(s) at "
                f"{outcome['path']}: {detail}",
                file=sys.stderr,
            )
        except (OSError, LedgerError) as retention_error:
            print(
                f"series: WRITER ERROR and FAILED to retain {len(retained)} input(s): "
                f"{retention_error}; {detail}",
                file=sys.stderr,
            )
        raise
    if batch_error is not None:
        series_rows = []
        written = 0
        quarantined = parse_records() + [
            record("batch", batch_error_type, batch_error.removeprefix("series: "),
                   [quarantine_input(number, raw)])
            for number, _, raw in parsed
        ]

    retention: dict[str, Any] = {
        "path": None, "written": 0, "duplicates": 0, "published_check_error": None,
    }
    quarantine_error = None
    if quarantined:
        try:
            retention = append_quarantine(parent, run_id, quarantined)
        except (OSError, LedgerError) as error:
            quarantine_error = error

    if batch_error is not None:
        exit_code = exit_on_error
    elif quarantine_error is not None:
        exit_code = 1
    elif not quarantined:
        exit_code = 0
    elif not series_rows:
        # Nothing independent survived: a batch whose every row was refused
        # must not read as a successful append.
        exit_code = 1
    elif producer == "validate":
        # The valid rows are appended and every refused input is retained,
        # byte for byte, in the quarantine spool (fsynced above). That spool
        # is published to the test ledger beside the series at finalize, its
        # count is recorded in the run's ledger_mirror.raw_publication, and
        # ledger_spool_backlog --store series-quarantine alarms while any
        # recent refusal exists. Failing the validate here would instead make
        # a data refusal in one cell fail the tests of every other cell.
        exit_code = 0
    else:
        exit_code = 1

    stream = sys.stdout if exit_code == 0 else sys.stderr
    for item in quarantined:
        print("series: QUARANTINED " + _quarantine_summary(item), file=stream)
    if batch_error is not None:
        print(batch_error, file=sys.stderr)
    if series_rows:
        represented = sum(int(row["series"].get("num_runs", 1)) for row in series_rows)
        print(
            f"series: appended {written} row(s) representing {represented} run(s) to {target}"
        )
    elif batch_error is None and not quarantined:
        print(
            "series: no completed cell rows; unmet prerequisites remain in the validate record",
            file=sys.stderr,
        )
    if quarantined:
        where = str(retention["path"]) if quarantine_error is None else "NOWHERE"
        print(
            f"series: quarantined {len(quarantined)} record(s) covering "
            f"{len({item['input_row'] for record_ in quarantined for item in record_['inputs']})}"
            f" of {total} input(s) to {where}",
            file=stream,
        )
    if quarantine_error is not None:
        print(
            f"series: FAILED to retain {len(quarantined)} quarantined record(s): "
            f"{quarantine_error}",
            file=sys.stderr,
        )
    stages: dict[str, int] = {}
    for item in quarantined:
        stages[item["stage"]] = stages.get(item["stage"], 0) + 1
    print("series: QUARANTINE_SUMMARY " + json.dumps({
        "schema": QUARANTINE_SUMMARY_SCHEMA,
        "run_id": run_id,
        "producer": producer,
        "inputs": total,
        "rows_appended": written,
        "rows_admitted": len(series_rows),
        "quarantined_records": len(quarantined),
        "quarantined_inputs": len({
            item["input_row"] for record_ in quarantined for item in record_["inputs"]
        }),
        "stages": stages,
        "retained": quarantine_error is None,
        "written": retention["written"],
        "duplicates": retention["duplicates"],
        "published_check_error": retention["published_check_error"],
        "retention_error": None if quarantine_error is None else str(quarantine_error),
        "exit_code": exit_code,
    }, sort_keys=True), file=stream)
    return exit_code


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
        stored = history_store_rows(parent)
    except (MalformedEvent, SeriesError, OSError, subprocess.SubprocessError) as error:
        print(f"series: cannot read history: {error}", file=sys.stderr)
        return 2
    # The retired-id map is read from the same refreshed Hermit origin/main
    # that labels current ancestry below, so a test whose older rows carry a
    # retired id is joined by the map Hermit main publishes now.
    hermit_checkout = parent / "hermit"
    if _is_checkout(hermit_checkout):
        refresh_origin_main(hermit_checkout, "Hermit")
    try:
        retired_source = retired_ids_at(hermit_checkout, "origin/main")
    except (SeriesError, OSError, subprocess.SubprocessError) as error:
        print(f"series: cannot read Hermit's retired-id map: {error}", file=sys.stderr)
        return 2
    retired = retired_source[1] if retired_source is not None else None
    rows = history_rows(stored, wanted_test, wanted_tree, retired)
    if not rows:
        suffix = f" at {wanted_tree}" if wanted_tree else ""
        print(
            f"series: no runs recorded for test {wanted_test!r}{suffix}",
            file=sys.stderr,
        )
        return 1

    trees = [str((row.get("series") or {}).get("tree", "")) for row in rows]
    current = main_ancestry(hermit_checkout, trees)
    represented = sum(int((row.get("series") or {}).get("num_runs", 1)) for row in rows)
    print(
        f"test={wanted_test} rows={len(rows)} runs={represented} "
        "source=test-ledger-origin-main+local-spool"
    )
    resolved_test = resolve_test_id(wanted_test, retired)
    joined = sorted(
        {recorded for row in rows if (recorded := test_id(row)) is not None}
        | {wanted_test}
    )
    if retired_source is not None and joined != [resolved_test]:
        print(
            f"joined test ids {' + '.join(joined)} as {resolved_test} "
            f"(retired-id map {RETIRED_IDS_FILE} at Hermit {retired_source[0]})"
        )
    by_cell: dict[str, dict[str, Any]] = {}
    for row in rows:
        payload = row.get("series") or {}
        cell = resolve_cell(str(payload.get("cell", "-")), retired)
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
    if argv in (["--help"], ["-h"]):
        print(SCORECARD_SNAPSHOT_USAGE)
        return 0
    if any(argument in {"-h", "--help"} for argument in argv):
        return _snapshot_usage_error("help cannot be combined with other arguments")

    parent = None
    output = None
    i = 0
    while i < len(argv):
        if argv[i] == "--parent":
            if i + 1 >= len(argv):
                return _snapshot_usage_error("--parent requires a value")
            parent = argv[i + 1]
            i += 2
            continue
        if argv[i] == "--output":
            if i + 1 >= len(argv):
                return _snapshot_usage_error("--output requires a value")
            output = argv[i + 1]
            i += 2
            continue
        return _snapshot_usage_error(f"unknown snapshot argument {argv[i]!r}")
    missing = [
        name
        for name, value in (("--parent", parent), ("--output", output))
        if not value
    ]
    if missing:
        return _snapshot_usage_error(f"snapshot requires {', '.join(missing)}")
    output_path = Path(output)
    try:
        report = write_scorecard_projection_snapshot(parent, output_path)
    except (MalformedEvent, SeriesError, OSError, subprocess.SubprocessError) as error:
        staging_glob = output_path.parent / f".{output_path.name}.staging-*"
        print(
            f"series: snapshot failed: {error}\n"
            "series: output state is not asserted after failure; inspect "
            f"{output_path} and {staging_glob}. Preserve any file found, correct "
            "the named failure, then retry with a new run-scoped --output. Run "
            "`series.py snapshot --help` for usage.",
            file=sys.stderr,
        )
        return 1
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


def _snapshot_usage_error(message: str) -> int:
    print(f"series: {message}", file=sys.stderr)
    print("Run `series.py snapshot --help` for the supported flags.", file=sys.stderr)
    print(SCORECARD_SNAPSHOT_USAGE, file=sys.stderr)
    return 2


def _append_parity_usage_error(message: str) -> int:
    print(f"series: {message}", file=sys.stderr)
    print("Run `series.py append-parity --help` for the supported flags.", file=sys.stderr)
    print(APPEND_PARITY_USAGE, file=sys.stderr)
    return 2


def _usage(message: str) -> int:
    print(f"series: {message}", file=sys.stderr)
    print(
        "usage:\n"
        "  series.py append --parent <dev-hermit root>  < rows.jsonl\n"
        "  series.py append-cells --parent <root> "
        "--producer <validate|pressure-test|hermit-repeat>\n"
        "      --run-id <id> --tree <sha> [--checkout <Hermit checkout>]\n"
        "      [--emitted-at <UTC>] < cell-results.jsonl\n"
        "  series.py append-parity --parent <root> --producer <validate|pressure-test>\n"
        "      --run-id <id> --tree <sha> --source-tree-dirty <true|false>\n"
        "      [--emitted-at <UTC>] < parity-ledger-sources.jsonl\n"
        "  series.py history --test <test id> [--tree <sha>] [--parent <root>]\n"
        "  series.py snapshot --parent <root> --output <run-scoped.json>\n"
        "append validates complete series rows. append-cells converts typed harness\n"
        "results, compresses exact equal runs, and writes the UNPUBLISHED spool.\n"
        "append-parity spools one run's parity rows (hermit's ParityLedgerSource) with\n"
        "a run marker; empty stdin records a zero-record marker. A refused call is\n"
        "spooled as a parity.refusal carrying its message.\n"
        "history freshly reads tracked shards from test-ledger origin/main plus that local spool.\n"
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
    if argv[0] == "append-parity":
        return _cmd_append_parity(argv[1:])
    if argv[0] == "history":
        return _cmd_history(argv[1:])
    if argv[0] == "snapshot":
        return _cmd_snapshot(argv[1:])
    return _usage(f"unknown subcommand {argv[0]!r}")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
