#!/usr/bin/env python3
"""Export canonical validation-ledger and series inputs for the website.

This is a read-only boundary. It resolves one explicit parent commit, copies
the mutable live and retained spool batches only after a stable-read check, and
then delegates row interpretation to the existing ledger and series readers.
It never derives a cell state and never writes a measured/not-measured field.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
from collections import Counter
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Sequence


CI_HUB = Path(__file__).resolve().parents[1]
REPOSITORY = CI_HUB.parent
LEDGER_DIR = CI_HUB / "ledger"
SERIES_DIR = CI_HUB / "series"

ledger_store: Any = None
ledger_publisher: Any = None
series_store: Any = None
validation_ledger: Any = None
_AUTHORITY_IMPORT_READER_STATE: dict[str, Any] | None = None


SCHEMA_VERSION = 1
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_MONTH_RE = re.compile(r"^[0-9]{4}-[0-9]{2}$")
_OWNER_PATH_NEEDLE = "/" + "home" + "/"
# Each fingerprint binds the canonical JSON for record_id, commit,
# schema_version, run_id, and cell_results.  The narrow binding keeps a nearby
# malformed supported-schema row visible to the fail-closed consumer.
_WEBSITE_SELECTION_RETIREMENTS = {
    "devbig014-1787003302-994556": {
        "commit": "99e378524b9a6a22f5b74ab24192b906ab371a13",
        "fingerprint": "b9a229ec66622b701f33adc8b608968075c4e946b951876416394320b2a6ac52",
    },
    "devbig014-1787005074-1930458": {
        "commit": "88d4d61bfa9be43fa10513980485e9e55ef353dc",
        "fingerprint": "64142a34df8d7d699e2b7567aee02236c472158fa554f5c6fb31275d50bd729e",
    },
}
_READER_PATHS = (
    Path("ci-hub/ci-hub"),
    Path("ci-hub/ci-hub.rs"),
    Path("ci-hub/compatibility-website/export_inputs.py"),
    Path("ci-hub/ledger/check-ledger-events.rs"),
    Path("ci-hub/ledger/ledger.py"),
    Path("ci-hub/ledger/publisher.py"),
    Path("ci-hub/ledger/validate_rows.py"),
    Path("ci-hub/lib/bounded_capture.py"),
    Path("ci-hub/lib/ledger_event.rs"),
    Path("ci-hub/lib/validation_evidence.rs"),
    Path("ci-hub/qualifying_receipt.py"),
    Path("ci-hub/series/check-series-rows.rs"),
    Path("ci-hub/series/series.py"),
    Path("ci-hub/validate/flake_class.py"),
    Path("ci-hub/validate/flaky_cells.py"),
    Path("ci-hub/validate/gate_completeness.py"),
    Path("ci-hub/validate/green_class.py"),
    Path("ci-hub/validate/qualifying-receipt.json"),
)


class ExportRefused(Exception):
    """The inputs cannot support one reproducible canonical export."""


def _refuse_authority_overrides() -> None:
    if "CI_HUB_LEDGER_EVENT_CHECKER" in os.environ:
        raise ExportRefused(
            "CI_HUB_LEDGER_EVENT_CHECKER is a fixture-only override and cannot "
            "be used for a canonical export"
        )


@dataclass(frozen=True, order=True)
class MutableSource:
    """One live or retained spool batch and its parent-relative identity."""

    store: str
    location: str
    relative: str
    path: Path

    def identity(self) -> tuple[str, str, str]:
        return (self.store, self.location, self.relative)


@dataclass(frozen=True)
class GitSource:
    """One regular JSONL blob selected from the resolved parent commit."""

    store: str
    location: str
    relative: str
    object_id: str
    data: bytes


def _git_environment() -> dict[str, str]:
    environment = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    return environment


def _git(checkout: Path, arguments: Sequence[str]) -> bytes:
    try:
        result = subprocess.run(
            [
                "git",
                "--no-replace-objects",
                "-C",
                str(checkout),
                *arguments,
            ],
            check=False,
            capture_output=True,
            timeout=60,
            env=_git_environment(),
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ExportRefused(f"cannot run git in {checkout}: {error}") from error
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).decode(errors="replace").strip()
        raise ExportRefused(
            f"git {' '.join(arguments)} failed in {checkout}: "
            f"{detail or f'exit {result.returncode} with no output'}"
        )
    return result.stdout


def _git_text(checkout: Path, arguments: Sequence[str]) -> str:
    return _git(checkout, arguments).decode(errors="strict").strip()


def _require_repository_root(value: Path) -> Path:
    root = value.expanduser().resolve()
    if not root.is_dir():
        raise ExportRefused(f"parent is not a directory: {root}")
    top = Path(_git_text(root, ["rev-parse", "--show-toplevel"])).resolve()
    if top != root:
        raise ExportRefused(
            f"--parent must name the dev-hermit checkout root, got {root} "
            f"(git root is {top})"
        )
    return root


def _resolve_commit(checkout: Path, reference: str) -> str:
    commit = _git_text(
        checkout,
        ["rev-parse", "--verify", f"{reference}^{{commit}}"],
    )
    if _SHA_RE.fullmatch(commit) is None:
        raise ExportRefused(
            f"published reference {reference!r} resolved to invalid commit {commit!r}"
        )
    return commit


def _safe_git_path(raw: str) -> str:
    path = PurePosixPath(raw)
    if path.is_absolute() or not path.parts or ".." in path.parts:
        raise ExportRefused(f"git returned an unsafe input path: {raw!r}")
    return path.as_posix()


def _input_store_and_location(relative: str) -> tuple[str, str] | None:
    parts = PurePosixPath(relative).parts
    if (
        len(parts) == 4
        and parts[0] == "ledger"
        and _MONTH_RE.fullmatch(Path(parts[3]).stem)
        and parts[3].endswith(".jsonl")
    ):
        return ("validation-ledger", "published")
    if (
        len(parts) == 5
        and parts[0] == "ledger-recovery"
        and _MONTH_RE.fullmatch(parts[3])
        and parts[4].endswith(".jsonl")
    ):
        return ("validation-ledger", "recovery")
    if len(parts) == 4 and parts[0] == "series" and parts[3].endswith(".jsonl"):
        return ("series", "published")
    return None


def _git_input_files(checkout: Path, commit: str) -> dict[str, GitSource]:
    listed = _git(
        checkout,
        [
            "ls-tree",
            "-r",
            "-z",
            commit,
            "--",
            "ledger",
            "ledger-recovery",
            "series",
        ],
    )
    sources: dict[str, GitSource] = {}
    for raw in listed.split(b"\0"):
        if not raw:
            continue
        try:
            metadata, raw_path = raw.split(b"\t", 1)
            mode, object_type, raw_object_id = metadata.decode().split()
        except (ValueError, UnicodeDecodeError) as error:
            raise ExportRefused(f"git returned malformed ls-tree entry {raw!r}") from error
        relative = _safe_git_path(raw_path.decode(errors="strict"))
        selected = _input_store_and_location(relative)
        if object_type != "blob" or mode not in {"100644", "100755"}:
            raise ExportRefused(
                f"published ledger or series path is not a regular blob: {relative} "
                f"(mode={mode}, type={object_type})"
            )
        if not re.fullmatch(r"[0-9a-f]{40,64}", raw_object_id):
            raise ExportRefused(
                f"published input has invalid Git object id: {relative}"
            )
        store, location = selected or ("", "")
        sources[relative] = GitSource(
            store=store,
            location=location,
            relative=relative,
            object_id=raw_object_id,
            data=_git(checkout, ["show", f"{commit}:{relative}"]),
        )
    return dict(sorted(sources.items()))


def _read_regular_file(path: Path) -> bytes:
    try:
        before = os.lstat(path)
    except OSError as error:
        raise ExportRefused(f"cannot inspect input file {path}: {error}") from error
    if not stat.S_ISREG(before.st_mode):
        raise ExportRefused(f"input is not a regular file: {path}")

    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    flags |= getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ExportRefused(f"cannot open input file {path}: {error}") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ExportRefused(f"input is not a regular file: {path}")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        return b"".join(chunks)
    except OSError as error:
        raise ExportRefused(f"cannot read input file {path}: {error}") from error
    finally:
        os.close(descriptor)


def _stable_file_bytes(path: Path) -> bytes:
    first = _read_regular_file(path)
    second = _read_regular_file(path)
    if first != second:
        raise ExportRefused(f"input changed while it was being read: {path}")
    return first


def _relative_to(root: Path, path: Path) -> str:
    try:
        return path.relative_to(root).as_posix()
    except ValueError as error:
        raise ExportRefused(f"input escapes the parent data root: {path}") from error


def _check_directory_path(root: Path, directory: Path) -> None:
    relative = _relative_to(root, directory)
    current = root
    for part in PurePosixPath(relative).parts:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            return
        except OSError as error:
            raise ExportRefused(f"cannot inspect input path {current}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise ExportRefused(f"input path contains a symlink: {current}")
        if not stat.S_ISDIR(metadata.st_mode):
            raise ExportRefused(f"input parent is not a directory: {current}")


def _checked_relative_file(root: Path, path: Path) -> str:
    relative = _relative_to(root, path)
    _check_directory_path(root, path.parent)
    try:
        metadata = os.lstat(path)
    except OSError as error:
        raise ExportRefused(f"cannot inspect input path {path}: {error}") from error
    if stat.S_ISLNK(metadata.st_mode):
        raise ExportRefused(f"input path contains a symlink: {path}")
    if not stat.S_ISREG(metadata.st_mode):
        raise ExportRefused(f"input is not a regular file: {path}")
    return relative


def _discover_mutable_sources(parent: Path, data_root: Path) -> list[MutableSource]:
    sources: list[MutableSource] = []

    validation_spool = validation_ledger.live_spool_dir(data_root)
    for location, directory in (
        ("live", validation_spool),
        ("retained", ledger_publisher.retained_dir(validation_spool)),
    ):
        _check_directory_path(data_root, directory)
        for path in sorted(directory.glob("*.jsonl")):
            relative = _checked_relative_file(data_root, path)
            sources.append(
                MutableSource(
                    store="validation-ledger",
                    location=location,
                    relative=relative,
                    path=path,
                )
            )

    series_spool = series_store.spool_dir(parent)
    retained_series = series_spool / ledger_publisher.RETAINED_SUBDIR
    _check_directory_path(data_root, series_spool)
    _check_directory_path(data_root, retained_series)
    tracked_series = parent / "series"
    for path in series_store.store_paths(parent):
        path = Path(path)
        try:
            path.relative_to(tracked_series)
            continue
        except ValueError:
            pass
        if path.parent == series_spool:
            location = "live"
        elif path.parent == retained_series:
            location = "retained"
        else:
            raise ExportRefused(
                f"series authority returned an unrecognised source path: {path}"
            )
        sources.append(
            MutableSource(
                store="series",
                location=location,
                relative=_checked_relative_file(data_root, path),
                path=path,
            )
        )

    sources.sort(key=MutableSource.identity)
    identities = [source.identity() for source in sources]
    if len(identities) != len(set(identities)):
        raise ExportRefused("one mutable input path was classified more than once")
    return sources


def _capture_mutable_sources(
    parent: Path, data_root: Path,
) -> list[tuple[MutableSource, bytes]]:
    before = _discover_mutable_sources(parent, data_root)
    first = {
        source.identity(): _stable_file_bytes(source.path)
        for source in before
    }
    after = _discover_mutable_sources(parent, data_root)
    if [source.identity() for source in before] != [
        source.identity() for source in after
    ]:
        raise ExportRefused(
            "live or retained spool path set changed while inputs were captured"
        )
    second = {
        source.identity(): _stable_file_bytes(source.path)
        for source in after
    }
    if first != second:
        raise ExportRefused(
            "live or retained spool content changed while inputs were captured"
        )
    return [(source, first[source.identity()]) for source in before]


def _write_snapshot_file(root: Path, relative: str, data: bytes) -> Path:
    target = root / relative
    try:
        target.relative_to(root)
    except ValueError as error:
        raise ExportRefused(f"snapshot path escapes its root: {relative}") from error
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
    return target


def _canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
    ).encode()


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _value_sha256(value: Any) -> str:
    return _sha256(_canonical_bytes(value))


def _line_count(data: bytes) -> int:
    return sum(1 for line in data.splitlines() if line.strip())


def _source_record(
    *,
    store: str,
    location: str,
    relative: str,
    data: bytes,
    git_object: str | None = None,
) -> dict[str, Any]:
    record = {
        "bytes": len(data),
        "line_count": _line_count(data),
        "location": location,
        "path": relative,
        "sha256": _sha256(data),
        "store": store,
    }
    if git_object is not None:
        record["git_object"] = git_object
    return record


def _reader_files() -> list[dict[str, Any]]:
    out = []
    for relative in _READER_PATHS:
        path = REPOSITORY / relative
        data = _stable_file_bytes(path)
        out.append(
            {
                "bytes": len(data),
                "path": relative.as_posix(),
                "sha256": _sha256(data),
            }
        )
    return out


def _unexpected_ci_hub_changes() -> list[str]:
    raw = _git(
        REPOSITORY,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", "ci-hub"],
    )
    unexpected = []
    for entry in raw.split(b"\0"):
        if not entry:
            continue
        text = entry.decode(errors="replace")
        path = text[3:] if len(text) >= 4 and text[2] == " " else text
        unexpected.append(path)
    return sorted(unexpected)


def _hermit_reader() -> dict[str, str]:
    listing = _git(REPOSITORY, ["ls-tree", "HEAD", "--", "hermit"]).decode(
        errors="strict"
    ).strip()
    match = re.fullmatch(r"160000 commit ([0-9a-f]{40})\thermit", listing)
    if match is None:
        raise ExportRefused("reader commit does not contain one Hermit gitlink")
    expected = match.group(1)
    checkout = REPOSITORY / "hermit"
    if not checkout.is_dir():
        raise ExportRefused("the pinned Hermit submodule is not materialized")
    top = Path(_git_text(checkout, ["rev-parse", "--show-toplevel"])).resolve()
    if top != checkout.resolve():
        raise ExportRefused("the pinned Hermit path is not its own checkout")
    actual = _resolve_commit(checkout, "HEAD")
    if actual != expected:
        raise ExportRefused(
            f"Hermit submodule is at {actual}, expected recorded gitlink {expected}"
        )
    status = _git(
        checkout,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    if status:
        raise ExportRefused("the pinned Hermit submodule has local changes")
    return {"checkout_commit": actual, "gitlink_commit": expected}


def _reader_state() -> dict[str, Any]:
    unexpected = _unexpected_ci_hub_changes()
    if unexpected:
        raise ExportRefused(
            "ci-hub reader files have unrecorded changes: " + ", ".join(unexpected)
        )
    commit = _resolve_commit(REPOSITORY, "HEAD")
    tree = _git_text(REPOSITORY, ["rev-parse", f"{commit}:ci-hub"])
    if not re.fullmatch(r"[0-9a-f]{40,64}", tree):
        raise ExportRefused(f"reader ci-hub tree has invalid object id {tree!r}")
    files = _reader_files()
    return {
        "ci_hub_tree": tree,
        "commit": commit,
        "files": files,
        "files_sha256": _value_sha256(files),
        "hermit": _hermit_reader(),
    }


def _load_authorities() -> dict[str, Any]:
    global ledger_store
    global ledger_publisher
    global series_store
    global validation_ledger
    global _AUTHORITY_IMPORT_READER_STATE

    if _AUTHORITY_IMPORT_READER_STATE is not None:
        return _AUTHORITY_IMPORT_READER_STATE

    before = _reader_state()
    sys.path.insert(0, str(LEDGER_DIR))
    sys.path.insert(0, str(SERIES_DIR))
    try:
        loaded = {
            "ledger": importlib.import_module("ledger"),
            "publisher": importlib.import_module("publisher"),
            "series": importlib.import_module("series"),
            "validate_rows": importlib.import_module("validate_rows"),
        }
    except (ImportError, OSError) as error:
        raise ExportRefused(f"cannot load canonical readers: {error}") from error
    expected = {
        "ledger": LEDGER_DIR / "ledger.py",
        "publisher": LEDGER_DIR / "publisher.py",
        "series": SERIES_DIR / "series.py",
        "validate_rows": LEDGER_DIR / "validate_rows.py",
    }
    for name, module in loaded.items():
        actual = Path(str(getattr(module, "__file__", ""))).resolve()
        if actual != expected[name].resolve():
            raise ExportRefused(
                f"imported {name} from {actual}, expected {expected[name]}"
            )
    after = _reader_state()
    if _canonical_bytes(before) != _canonical_bytes(after):
        raise ExportRefused("reader commit or files changed while authorities loaded")

    ledger_store = loaded["ledger"]
    ledger_publisher = loaded["publisher"]
    series_store = loaded["series"]
    validation_ledger = loaded["validate_rows"]
    _AUTHORITY_IMPORT_READER_STATE = after
    return after


def _schema_counts(rows: Iterable[dict], field: str) -> dict[str, int]:
    counts = Counter(
        str(row[field]) if field in row else "missing"
        for row in rows
    )
    return dict(sorted(counts.items()))


def _represented_runs(rows: Iterable[dict]) -> int:
    total = 0
    for row in rows:
        value = (row.get("series") or {}).get("num_runs", 1)
        if not isinstance(value, int) or isinstance(value, bool) or value < 1:
            raise ExportRefused(
                f"canonical series row has invalid num_runs {value!r}"
            )
        total += value
    return total


def _contains_string(value: Any, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    if isinstance(value, dict):
        return any(_contains_string(item, needle) for item in value.values())
    if isinstance(value, (list, tuple)):
        return any(_contains_string(item, needle) for item in value)
    return False


def _replace_string(value: Any, old: str, new: str) -> Any:
    """Return a copy with one exact captured-root string made portable."""
    if isinstance(value, str):
        return value.replace(old, new)
    if isinstance(value, dict):
        return {
            key: _replace_string(item, old, new)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [_replace_string(item, old, new) for item in value]
    if isinstance(value, tuple):
        return tuple(_replace_string(item, old, new) for item in value)
    return value


def _portable_history_rows(rows: list[dict], snapshot_root: Path) -> list[dict]:
    # rows() deliberately restores {{WORKSPACE_ROOT}} for operational callers.
    # This export is root-independent, so reverse exactly that restoration and
    # leave every other field untouched for the later allowlisted site view.
    portable = _replace_string(
        rows,
        str(snapshot_root),
        validation_ledger.WORKSPACE_TOKEN,
    )
    if _contains_string(portable, str(snapshot_root)):
        raise ExportRefused("validation rows retained the temporary snapshot path")
    if _contains_string(portable, _OWNER_PATH_NEEDLE):
        raise ExportRefused("validation rows contain an absolute owner path")
    return portable


def _incomplete_data() -> list[dict[str, str]]:
    return [
        {
            "field": "fields allowed for publication",
            "input": "validation ledger and series",
            "reason": (
                "canonical rows retain diagnostic strings and absolute non-workspace "
                "paths; a later website view must allowlist fields rather than "
                "publish these input rows directly"
            ),
        },
        {
            "field": "projection validation reason",
            "input": "series",
            "reason": (
                "the current shared checker exposes read and write validation but "
                "does not expose validate_for_projection results"
            ),
        },
        {
            "field": "row source path and line",
            "input": "series",
            "reason": (
                "the canonical published snapshot and union discard physical row "
                "locations"
            ),
        },
        {
            "field": "HistoryRow source path and line",
            "input": "validation ledger",
            "reason": (
                "the canonical HistoryRow view applies corrections and deduplicates "
                "rows without returning a one-to-one source location"
            ),
        },
    ]


def _website_current_validation_rows(
    history_rows: list[dict],
) -> tuple[list[dict], list[dict[str, str]]]:
    """Keep two exact pre-comparison-field rows as history, not selection evidence."""
    current_rows = validation_ledger.current_validation_run_rows(history_rows)
    usable: list[dict] = []
    incomplete: list[dict[str, str]] = []
    fingerprint_fields = (
        "record_id",
        "commit",
        "schema_version",
        "run_id",
        "cell_results",
    )
    for row in current_rows:
        record_id = row.get("record_id")
        retirement = _WEBSITE_SELECTION_RETIREMENTS.get(record_id)
        if retirement is None:
            usable.append(row)
            continue
        fingerprint = _value_sha256(
            {field: row.get(field) for field in fingerprint_fields}
        )
        if (
            row.get("schema_version") != 6
            or row.get("commit") != retirement["commit"]
            or fingerprint != retirement["fingerprint"]
        ):
            raise ExportRefused(
                f"historical validation row {record_id!r} no longer matches "
                "the exact pre-comparison-field evidence selected for retirement"
            )
        incomplete.append(
            {
                "field": "cell_results",
                "input": "validation ledger",
                "reason": (
                    "schema-6 cell_results predate compare_io_buffers, "
                    "display_name, record_envelope, and virtualize_time; the row "
                    "remains in validation history but contributes no current "
                    f"website selection evidence for Hermit commit {retirement['commit']}"
                ),
                "record_id": record_id,
                "run_id": str(row.get("run_id")),
                "schema": "6",
            }
        )
    return usable, sorted(incomplete, key=lambda item: item["record_id"])


def _validation_export(
    snapshot_root: Path,
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    events = validation_ledger.events(root=snapshot_root)
    history_rows = validation_ledger.rows(root=snapshot_root)
    current_rows, incomplete = _website_current_validation_rows(history_rows)
    portable_history = _portable_history_rows(history_rows, snapshot_root)
    portable_current = _portable_history_rows(current_rows, snapshot_root)
    return (
        {
            "current_validation_run_row_count": len(portable_current),
            "current_validation_run_rows": portable_current,
            "current_validation_run_rows_sha256": _value_sha256(portable_current),
            "event_count": len(events),
            "event_schema_counts": _schema_counts(events, "schema"),
            "events": events,
            "events_sha256": _value_sha256(events),
            "history_row_count": len(portable_history),
            "history_row_schema_counts": _schema_counts(
                portable_history,
                "schema_version",
            ),
            "history_rows": portable_history,
            "history_rows_sha256": _value_sha256(portable_history),
        },
        incomplete,
    )


def _series_export(
    parent: Path,
    commit: str,
    published_paths: list[Path],
    local_paths: list[Path],
) -> dict[str, Any]:
    published = series_store.published_snapshot_at(parent, commit)
    if published.commit != commit:
        raise ExportRefused(
            f"series snapshot resolved {published.commit}, expected {commit}"
        )
    copied_published_rows = series_store.union(published_paths)
    if _canonical_bytes(copied_published_rows) != _canonical_bytes(published.rows):
        raise ExportRefused(
            "published series rows disagree with the captured parent commit files"
        )
    rows = series_store.union([*published_paths, *local_paths])
    return {
        "represented_run_count": _represented_runs(rows),
        "row_count": len(rows),
        "rows": rows,
        "rows_sha256": _value_sha256(rows),
        "schema_counts": _schema_counts(rows, "schema"),
    }


def _provenance_sources(
    snapshot_root: Path,
    git_files: dict[str, GitSource],
    mutable: list[tuple[MutableSource, bytes]],
) -> list[dict[str, Any]]:
    selected_paths = [
        *validation_ledger.shard_paths(snapshot_root),
        *sorted((snapshot_root / "series").glob("*/*/*.jsonl")),
    ]
    sources: list[dict[str, Any]] = []
    for path in selected_paths:
        relative = path.relative_to(snapshot_root).as_posix()
        source = git_files.get(relative)
        if source is None:
            raise ExportRefused(
                f"captured published input has no parent-commit source: {relative}"
            )
        if not source.store or not source.location:
            raise ExportRefused(
                f"an authority selected an unclassified source: {relative}"
            )
        sources.append(
            _source_record(
                store=source.store,
                location=source.location,
                relative=relative,
                data=source.data,
                git_object=source.object_id,
            )
        )
    sources.extend(
        _source_record(
            store=source.store,
            location=source.location,
            relative=source.relative,
            data=data,
        )
        for source, data in mutable
    )
    return sorted(
        sources,
        key=lambda item: (item["store"], item["location"], item["path"]),
    )


def build_export(parent_value: Path, published_ref: str) -> dict[str, Any]:
    _refuse_authority_overrides()
    reader_before = _reader_state()
    imported_reader = _load_authorities()
    if _canonical_bytes(reader_before) != _canonical_bytes(imported_reader):
        raise ExportRefused("reader commit or files changed before authority import")
    parent = _require_repository_root(parent_value)
    commit = _resolve_commit(parent, published_ref)
    data_root = series_store.shared_parent_root(parent).resolve()
    if not data_root.is_dir():
        raise ExportRefused(f"parent data root is unavailable: {data_root}")

    git_files = _git_input_files(parent, commit)
    mutable = _capture_mutable_sources(parent, data_root)

    with tempfile.TemporaryDirectory(
        prefix="compatibility-website-inputs-"
    ) as raw_snapshot:
        snapshot_root = Path(raw_snapshot)
        for relative, source in git_files.items():
            _write_snapshot_file(snapshot_root, relative, source.data)
        for source, data in mutable:
            _write_snapshot_file(snapshot_root, source.relative, data)

        published_series_paths = sorted(
            (snapshot_root / "series").glob("*/*/*.jsonl")
        )
        local_series_paths = [
            snapshot_root / source.relative
            for source, _data in mutable
            if source.store == "series"
        ]
        validation, validation_incomplete = _validation_export(snapshot_root)
        series = _series_export(
            parent,
            commit,
            published_series_paths,
            local_series_paths,
        )
        sources = _provenance_sources(snapshot_root, git_files, mutable)

        result = {
            "incomplete_data": [*_incomplete_data(), *validation_incomplete],
            "provenance": {
                "parent_commit": commit,
                "published_ref": published_ref,
                "reader_ci_hub_tree": reader_before["ci_hub_tree"],
                "reader_commit": reader_before["commit"],
                "reader_files": reader_before["files"],
                "reader_files_sha256": reader_before["files_sha256"],
                "reader_hermit": reader_before["hermit"],
                "source_count": len(sources),
                "sources": sources,
                "sources_sha256": _value_sha256(sources),
            },
            "schema_version": SCHEMA_VERSION,
            "series": series,
            "validation_ledger": validation,
        }
    reader_after = _reader_state()
    if _canonical_bytes(reader_before) != _canonical_bytes(reader_after):
        raise ExportRefused("reader commit or files changed during export")
    if _contains_string(result, str(parent)):
        raise ExportRefused("export contains the absolute parent checkout path")
    if _contains_string(result, _OWNER_PATH_NEEDLE):
        raise ExportRefused("export contains an absolute owner path")
    return result


def render_export(value: dict[str, Any]) -> bytes:
    return _canonical_bytes(value) + b"\n"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--parent",
        required=True,
        type=Path,
        help="dev-hermit checkout whose commit and shared local spool are read",
    )
    parser.add_argument(
        "--published-ref",
        required=True,
        help="explicit parent commit or ref; resolved once and never fetched",
    )
    arguments = parser.parse_args(argv)
    try:
        value = build_export(arguments.parent, arguments.published_ref)
    except (ExportRefused, OSError, ValueError) as error:
        print(f"compatibility-website input export: REFUSED: {error}", file=sys.stderr)
        return 2
    except Exception as error:
        if ledger_store is None or not isinstance(error, ledger_store.LedgerError):
            raise
        print(f"compatibility-website input export: REFUSED: {error}", file=sys.stderr)
        return 2
    sys.stdout.buffer.write(render_export(value))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
