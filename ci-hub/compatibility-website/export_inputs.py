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
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence


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
_HISTORICAL_TEST_ENVIRONMENT = (
    "CI_HUB_VALIDATE_LEDGER_TEST_ROOT",
    "HERMIT_VALIDATE_STOP_TEST_MODE",
)
_GIT_TIMEOUT_SECONDS = 60
_MAX_GIT_STDOUT_BYTES = 256 << 20
_MAX_GIT_STDERR_BYTES = 64 << 10
_CHILD_EXIT_POLL_SECONDS = 0.05
_PROCESS_GROUP_TERM_GRACE_SECONDS = 0.25
_PROCESS_GROUP_POLL_SECONDS = 0.01
_MAX_PROCESS_DETAIL_BYTES = 2_000
_READER_PATHS = (
    Path("ci-hub/ci-hub"),
    Path("ci-hub/ci-hub.rs"),
    Path("ci-hub/compatibility-website/export_inputs.py"),
    Path("ci-hub/ledger/check-ledger-parses.rs"),
    Path("ci-hub/ledger/check-ledger-events.rs"),
    Path("ci-hub/ledger/ledger.py"),
    Path("ci-hub/ledger/publisher.py"),
    Path("ci-hub/ledger/validate_rows.py"),
    Path("ci-hub/lib/bounded_capture.py"),
    Path("ci-hub/lib/bounded_output.rs"),
    Path("ci-hub/lib/compatibility_website.rs"),
    Path("ci-hub/lib/failure_obligations.rs"),
    Path("ci-hub/lib/history_queries.rs"),
    Path("ci-hub/lib/landing_lock.rs"),
    Path("ci-hub/lib/lease.rs"),
    Path("ci-hub/lib/ledger_event.rs"),
    Path("ci-hub/lib/measured.rs"),
    Path("ci-hub/lib/published_receipt.rs"),
    Path("ci-hub/lib/qualifying_receipt.rs"),
    Path("ci-hub/lib/records.rs"),
    Path("ci-hub/lib/validate_lock.rs"),
    Path("ci-hub/lib/validate_run_handle.rs"),
    Path("ci-hub/lib/validate_status.rs"),
    Path("ci-hub/lib/validation_evidence.rs"),
    Path("ci-hub/qualifying_receipt.py"),
    Path("ci-hub/series/check-series-rows.rs"),
    Path("ci-hub/series/series.py"),
    Path("ci-hub/validate/final_validate_status.py"),
    Path("ci-hub/validate/flake_class.py"),
    Path("ci-hub/validate/flaky_cells.py"),
    Path("ci-hub/validate/gate_completeness.py"),
    Path("ci-hub/validate/green_class.py"),
    Path("ci-hub/validate/qualifying-receipt.json"),
    Path("ci-hub/validate/run_registry.py"),
    Path("ci-hub/validate/service_result.py"),
)


class ExportRefused(Exception):
    """The inputs cannot support one reproducible canonical export."""


class OutputLimitExceeded(Exception):
    """A child produced more bytes than its caller's fixed capture bound."""

    def __init__(self, stream: str, limit: int) -> None:
        super().__init__(f"{stream} exceeded {limit} bytes")
        self.stream = stream
        self.limit = limit


def _process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    return True


def _clean_process_group(process: subprocess.Popen[bytes]) -> None:
    """Bound the lifetime of the private process group owned by ``process``."""

    process_group = process.pid
    try:
        os.killpg(process_group, signal.SIGTERM)
    except ProcessLookupError:
        pass

    deadline = time.monotonic() + _PROCESS_GROUP_TERM_GRACE_SECONDS
    while _process_group_exists(process_group):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            try:
                os.killpg(process_group, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
        time.sleep(min(remaining, _PROCESS_GROUP_POLL_SECONDS))

    if process.poll() is None:
        try:
            process.wait(timeout=_PROCESS_GROUP_TERM_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process_group, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=_PROCESS_GROUP_TERM_GRACE_SECONDS)


def capture_bounded_process(
    command: Sequence[str],
    *,
    environment: Mapping[str, str],
    timeout: float,
    stdout_limit: int,
    stderr_limit: int,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[bytes]:
    """Capture one child without an unbounded pipe, file, or memory read.

    Once the direct child exits, an inherited descriptor in a grandchild does
    not keep this call waiting for EOF. While the child is live, output is read
    incrementally and the fixed byte limits are enforced before it can finish.
    """

    if timeout <= 0:
        raise ValueError("timeout must be positive")
    if stdout_limit < 0 or stderr_limit < 0:
        raise ValueError("output limits must be nonnegative")

    process = subprocess.Popen(
        list(command),
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    selector: selectors.BaseSelector | None = None
    deadline = time.monotonic() + timeout
    try:
        assert process.stdout is not None and process.stderr is not None
        stdout_descriptor = process.stdout.fileno()
        stderr_descriptor = process.stderr.fileno()
        streams = {
            stdout_descriptor: (
                "stdout",
                process.stdout,
                bytearray(),
                stdout_limit,
            ),
            stderr_descriptor: (
                "stderr",
                process.stderr,
                bytearray(),
                stderr_limit,
            ),
        }
        selector = selectors.DefaultSelector()
        for descriptor in streams:
            os.set_blocking(descriptor, False)
            selector.register(descriptor, selectors.EVENT_READ)
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(command, timeout)
            child_running = process.poll() is None
            events = selector.select(
                min(remaining, _CHILD_EXIT_POLL_SECONDS) if child_running else 0
            )
            if not events:
                if process.poll() is None:
                    continue
                break
            for key, _mask in events:
                stream, handle, captured, limit = streams[key.fd]
                chunk = os.read(key.fd, min(65536, limit - len(captured) + 1))
                if not chunk:
                    selector.unregister(key.fd)
                    handle.close()
                    continue
                available = limit - len(captured)
                captured.extend(chunk[:available])
                if len(chunk) > available:
                    raise OutputLimitExceeded(stream, limit)

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(command, timeout)
        returncode = process.wait(timeout=remaining)
        return subprocess.CompletedProcess(
            list(command),
            returncode,
            bytes(streams[stdout_descriptor][2]),
            bytes(streams[stderr_descriptor][2]),
        )
    finally:
        _clean_process_group(process)
        if selector is not None:
            selector.close()
        if process.stdout is not None:
            process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()


def _process_detail(stderr: bytes, stdout: bytes) -> str:
    encoded = stderr or stdout
    truncated = len(encoded) > _MAX_PROCESS_DETAIL_BYTES
    detail = encoded[:_MAX_PROCESS_DETAIL_BYTES].decode(errors="replace").strip()
    if truncated:
        detail += "... [truncated]"
    return detail or "no output"


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
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    return environment


def _historical_reader_environment() -> dict[str, str]:
    environment = _git_environment()
    for name in _HISTORICAL_TEST_ENVIRONMENT:
        environment.pop(name, None)
    return environment


def _git(checkout: Path, arguments: Sequence[str]) -> bytes:
    try:
        result = capture_bounded_process(
            [
                "git",
                "--no-replace-objects",
                "-C",
                str(checkout),
                *arguments,
            ],
            timeout=_GIT_TIMEOUT_SECONDS,
            stdout_limit=_MAX_GIT_STDOUT_BYTES,
            stderr_limit=_MAX_GIT_STDERR_BYTES,
            environment=_git_environment(),
        )
    except (OSError, subprocess.TimeoutExpired, OutputLimitExceeded) as error:
        raise ExportRefused(f"cannot run git in {checkout}: {error}") from error
    if result.returncode != 0:
        raise ExportRefused(
            f"git {' '.join(arguments)} failed in {checkout}: "
            f"{_process_detail(result.stderr, result.stdout)}"
        )
    if result.stderr:
        raise ExportRefused(
            f"git {' '.join(arguments)} wrote unexpected stderr in {checkout}: "
            f"{_process_detail(result.stderr, b'')}"
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
            raise ExportRefused(
                f"git returned malformed ls-tree entry {raw!r}"
            ) from error
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
            raise ExportRefused(
                f"cannot inspect input path {current}: {error}"
            ) from error
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
    parent: Path,
    data_root: Path,
) -> list[tuple[MutableSource, bytes]]:
    before = _discover_mutable_sources(parent, data_root)
    first = {source.identity(): _stable_file_bytes(source.path) for source in before}
    after = _discover_mutable_sources(parent, data_root)
    if [source.identity() for source in before] != [
        source.identity() for source in after
    ]:
        raise ExportRefused(
            "live or retained spool path set changed while inputs were captured"
        )
    second = {source.identity(): _stable_file_bytes(source.path) for source in after}
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


def _observed_hermit_reader() -> dict[str, str]:
    """Return the two independently observed Hermit revisions after clean checks."""
    listing = (
        _git(REPOSITORY, ["ls-tree", "HEAD", "--", "hermit"])
        .decode(errors="strict")
        .strip()
    )
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
    status = _git(
        checkout,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    if status:
        raise ExportRefused("the pinned Hermit submodule has local changes")
    return {"checkout_commit": actual, "gitlink_commit": expected}


def _hermit_reader() -> dict[str, str]:
    """Live exports require the materialized checkout to equal the gitlink."""
    observed = _observed_hermit_reader()
    if observed["checkout_commit"] != observed["gitlink_commit"]:
        raise ExportRefused(
            f"Hermit submodule is at {observed['checkout_commit']}, expected "
            f"recorded gitlink {observed['gitlink_commit']}"
        )
    return observed


def _historical_hermit_reader() -> dict[str, str]:
    """Historical builds bind a clean prerequisite checkout separately."""
    return _observed_hermit_reader()


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


def _historical_reader_state() -> dict[str, Any]:
    """Bind exact reader bytes without changing the live clean-tree contract."""
    commit = _resolve_commit(REPOSITORY, "HEAD")
    tree = _git_text(REPOSITORY, ["rev-parse", f"{commit}:ci-hub"])
    if not re.fullmatch(r"[0-9a-f]{40,64}", tree):
        raise ExportRefused(f"reader ci-hub tree has invalid object id {tree!r}")
    files = _reader_files()
    dirty_paths = _unexpected_ci_hub_changes()
    hermit = _historical_hermit_reader()
    identity = {
        "ci_hub_tree": tree,
        "commit": commit,
        "dirty_paths": dirty_paths,
        "files": files,
        "hermit": hermit,
        "worktree_clean": not dirty_paths,
    }
    return {
        **identity,
        "files_sha256": _value_sha256(files),
        "source_identity_sha256": _value_sha256(identity),
    }


def _import_authorities(reader_state: Any) -> dict[str, Any]:
    global ledger_store
    global ledger_publisher
    global series_store
    global validation_ledger

    before = reader_state()
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
    after = reader_state()
    if _canonical_bytes(before) != _canonical_bytes(after):
        raise ExportRefused("reader commit or files changed while authorities loaded")

    ledger_store = loaded["ledger"]
    ledger_publisher = loaded["publisher"]
    series_store = loaded["series"]
    validation_ledger = loaded["validate_rows"]
    return after


def _load_authorities() -> dict[str, Any]:
    global _AUTHORITY_IMPORT_READER_STATE

    if _AUTHORITY_IMPORT_READER_STATE is None:
        _AUTHORITY_IMPORT_READER_STATE = _import_authorities(_reader_state)
    return _AUTHORITY_IMPORT_READER_STATE


def _load_historical_authorities() -> dict[str, Any]:
    return _import_authorities(_historical_reader_state)


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
        return {key: _replace_string(item, old, new) for key, item in value.items()}
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


def _validation_export(
    snapshot_root: Path,
) -> dict[str, Any]:
    events = validation_ledger.events(root=snapshot_root)
    history_rows = validation_ledger.rows(root=snapshot_root)
    portable_history = _portable_history_rows(history_rows, snapshot_root)
    history_jsonl = _opaque_jsonl(portable_history, "HistoryRow")
    return {
        "event_count": len(events),
        "events_sha256": _value_sha256(events),
        "history_row_count": len(portable_history),
        "history_rows_jsonl": history_jsonl.decode(),
        "history_rows_sha256": _sha256(history_jsonl),
    }


def _opaque_jsonl(rows: Any, label: str) -> bytes:
    """Frame canonical-reader objects without interpreting their Rust fields."""
    if not isinstance(rows, list):
        raise ExportRefused(f"canonical {label} population is not an array")
    encoded: list[bytes] = []
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise ExportRefused(
                f"canonical {label} population row {index} is not an object"
            )
        encoded.append(_canonical_bytes(row) + b"\n")
    return b"".join(encoded)


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
    rows_jsonl = _opaque_jsonl(rows, "SeriesRow")
    return {
        "row_count": len(rows),
        "rows_jsonl": rows_jsonl.decode(),
        "rows_sha256": _sha256(rows_jsonl),
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

        published_series_paths = sorted((snapshot_root / "series").glob("*/*/*.jsonl"))
        local_series_paths = [
            snapshot_root / source.relative
            for source, _data in mutable
            if source.store == "series"
        ]
        validation = _validation_export(snapshot_root)
        series = _series_export(
            parent,
            commit,
            published_series_paths,
            local_series_paths,
        )
        sources = _provenance_sources(snapshot_root, git_files, mutable)

        result = {
            "incomplete_data": _incomplete_data(),
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


def build_historical_export(
    parent_value: Path,
    published_commit: str,
    record_id: str,
    run_id: str,
) -> dict[str, Any]:
    """Export one immutable published snapshot and bind its named HistoryRow."""
    _refuse_authority_overrides()
    if _SHA_RE.fullmatch(published_commit) is None:
        raise ExportRefused(
            "historical published commit must be a full lowercase 40-hex commit"
        )
    reader_before = _load_historical_authorities()
    parent = _require_repository_root(parent_value)
    commit = _resolve_commit(parent, published_commit)
    if commit != published_commit:
        raise ExportRefused(
            f"historical published commit resolved to {commit}, expected {published_commit}"
        )
    git_files = _git_input_files(parent, commit)

    with tempfile.TemporaryDirectory(
        prefix="compatibility-website-historical-inputs-"
    ) as raw_snapshot:
        snapshot_root = Path(raw_snapshot)
        for relative, source in git_files.items():
            _write_snapshot_file(snapshot_root, relative, source.data)

        published_series_paths = sorted((snapshot_root / "series").glob("*/*/*.jsonl"))
        validation = _validation_export(snapshot_root)
        series = _series_export(
            parent,
            commit,
            published_series_paths,
            [],
        )
        sources = _provenance_sources(snapshot_root, git_files, [])
        result = {
            "incomplete_data": _incomplete_data(),
            "provenance": {
                "historical": {
                    "record_id": record_id,
                    "run_id": run_id,
                },
                "mode": "historical",
                "parent_commit": commit,
                "published_only": True,
                "published_ref": published_commit,
                "reader_ci_hub_tree": reader_before["ci_hub_tree"],
                "reader_commit": reader_before["commit"],
                "reader_files": reader_before["files"],
                "reader_files_sha256": reader_before["files_sha256"],
                "reader_hermit": reader_before["hermit"],
                "reader_source_identity_sha256": reader_before[
                    "source_identity_sha256"
                ],
                "reader_worktree_clean": reader_before["worktree_clean"],
                "reader_worktree_dirty_paths": reader_before["dirty_paths"],
                "source_count": len(sources),
                "sources": sources,
                "sources_sha256": _value_sha256(sources),
            },
            "schema_version": SCHEMA_VERSION,
            "series": series,
            "validation_ledger": validation,
        }
    reader_after = _historical_reader_state()
    if _canonical_bytes(reader_before) != _canonical_bytes(reader_after):
        raise ExportRefused("reader commit or files changed during historical export")
    if _contains_string(result, str(parent)):
        raise ExportRefused(
            "historical export contains the absolute parent checkout path"
        )
    if _contains_string(result, _OWNER_PATH_NEEDLE):
        raise ExportRefused("historical export contains an absolute owner path")
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
    parser.add_argument(
        "--historical",
        action="store_true",
        help="read only published bytes at a full immutable commit",
    )
    parser.add_argument("--record-id", help="unique historical HistoryRow record id")
    parser.add_argument("--run-id", help="unique historical validation run id")
    arguments = parser.parse_args(argv)
    try:
        if arguments.historical:
            if arguments.record_id is None or arguments.run_id is None:
                raise ExportRefused("--historical requires --record-id and --run-id")
            value = build_historical_export(
                arguments.parent,
                arguments.published_ref,
                arguments.record_id,
                arguments.run_id,
            )
        else:
            if arguments.record_id is not None or arguments.run_id is not None:
                raise ExportRefused("--record-id and --run-id require --historical")
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
