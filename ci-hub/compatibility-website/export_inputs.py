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
import importlib.util
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
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence


CI_HUB = Path(__file__).resolve().parents[1]
REPOSITORY = CI_HUB.parent
LEDGER_DIR = CI_HUB / "ledger"
SERIES_DIR = CI_HUB / "series"
WEBSITE_DIR = CI_HUB / "compatibility-website"

ledger_store: Any = None
ledger_publisher: Any = None
series_store: Any = None
validation_ledger: Any = None
finalized_proof: Any = None
parity_summary: Any = None
_AUTHORITY_IMPORT_READER_STATE: dict[str, Any] | None = None


SCHEMA_VERSION = 1
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_MONTH_RE = re.compile(r"^[0-9]{4}-[0-9]{2}$")
_OWNER_PATH_NEEDLE = "/" + "home" + "/"
# Any absolute path, even one cut short inside its first part: a slash before
# a part, following no word character, dot, tilde or slash (so not a slash
# inside a URL or a relative path).
_ABSOLUTE_PATH_RE = re.compile(r"(?<![\w.~/])/[^\s/]")
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
    Path("ci-hub/admission_paths.py"),
    Path("ci-hub/ci-hub"),
    Path("ci-hub/ci-hub.rs"),
    Path("ci-hub/compatibility-website/export_inputs.py"),
    Path("ci-hub/compatibility-website/parity_summary.py"),
    Path("ci-hub/ledger/check-ledger-parses.rs"),
    Path("ci-hub/ledger/check-ledger-events.rs"),
    Path("ci-hub/ledger/ledger.py"),
    Path("ci-hub/ledger/publisher.py"),
    Path("ci-hub/ledger/validate_rows.py"),
    Path("ci-hub/lib/bounded_capture.py"),
    Path("ci-hub/lib/bounded_output.rs"),
    Path("ci-hub/lib/compatibility_census.rs"),
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
    Path("ci-hub/lib/rust_toolchain.py"),
    Path("ci-hub/lib/validate_lock.rs"),
    Path("ci-hub/lib/validate_run_handle.rs"),
    Path("ci-hub/lib/validate_status.rs"),
    Path("ci-hub/lib/validation_evidence.rs"),
    Path("ci-hub/qualifying_receipt.py"),
    Path("ci-hub/series/check-series-rows.rs"),
    Path("ci-hub/series/finalized_proof.py"),
    Path("ci-hub/series/parity_ledger.py"),
    Path("ci-hub/series/series.py"),
    Path("ci-hub/validate/final_validate_status.py"),
    Path("ci-hub/validate/flake_class.py"),
    Path("ci-hub/validate/flaky_cells.py"),
    Path("ci-hub/validate/gate_completeness.py"),
    Path("ci-hub/validate/green_class.py"),
    Path("ci-hub/validate/qualifying-receipt.json"),
    Path("ci-hub/validate/retained-evidence-quarantine.json"),
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
    if (
        len(parts) == 5
        and parts[:2] == ("quarantine", "series")
        and parts[4].endswith(".jsonl")
    ):
        return ("series-quarantine", "published")
    return None


def _git_input_files(
    checkout: Path, commit: str, roots: tuple[str, ...] = ("ledger", "ledger-recovery", "series"),
) -> dict[str, GitSource]:
    listed = _git(
        checkout,
        [
            "ls-tree",
            "-r",
            "-z",
            commit,
            "--",
            *roots,
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


def _series_quarantine_files(ledger: Path, commit: str) -> dict[str, GitSource]:
    return {
        path: source
        for path, source in _git_input_files(ledger, commit, ("quarantine/series",)).items()
        if source.store == "series-quarantine"
    }


def _snapshot_quarantine_paths(snapshot_root: Path) -> list[Path]:
    return sorted((snapshot_root / "quarantine" / "series").glob("*/*/*.jsonl"))


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
    tracked_series = series_store.ledger_root(parent) / "series"
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
    global finalized_proof
    global parity_summary

    before = reader_state()
    sys.path.insert(0, str(LEDGER_DIR))
    sys.path.insert(0, str(SERIES_DIR))
    try:
        loaded = {
            "ledger": importlib.import_module("ledger"),
            "publisher": importlib.import_module("publisher"),
            "series": importlib.import_module("series"),
            "validate_rows": importlib.import_module("validate_rows"),
            "finalized_proof": importlib.import_module("finalized_proof"),
        }
    except (ImportError, OSError) as error:
        raise ExportRefused(f"cannot load canonical readers: {error}") from error
    expected = {
        "ledger": LEDGER_DIR / "ledger.py",
        "publisher": LEDGER_DIR / "publisher.py",
        "series": SERIES_DIR / "series.py",
        "validate_rows": LEDGER_DIR / "validate_rows.py",
        "finalized_proof": SERIES_DIR / "finalized_proof.py",
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
    finalized_proof = loaded["finalized_proof"]
    parity_summary = _load_parity_reader()
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


def _contains_absolute_path(value: Any) -> bool:
    if isinstance(value, str):
        return _ABSOLUTE_PATH_RE.search(value) is not None
    if isinstance(value, dict):
        return any(_contains_absolute_path(item) for item in value.values())
    if isinstance(value, (list, tuple)):
        return any(_contains_absolute_path(item) for item in value)
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


def _load_parity_reader() -> Any:
    path = WEBSITE_DIR / "parity_summary.py"
    name = "compatibility_website_parity_summary"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ExportRefused("cannot load the parity summary reader")
    module = importlib.util.module_from_spec(spec)
    previous = sys.modules.get(name)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as error:
        if previous is None:
            sys.modules.pop(name, None)
        else:
            sys.modules[name] = previous
        raise ExportRefused(f"cannot load the parity summary reader: {error}") from error
    return module


def _parity_reader() -> Any:
    """`parity_summary.py`: the one checker of `scorecard/parity.json`.

    The combined website build binds its captured copy here; everything else
    loads it once from this checkout.
    """
    global parity_summary
    if parity_summary is None:
        parity_summary = _load_parity_reader()
    return parity_summary


def _parity_listing(ledger: Path, commit: str, roots: Sequence[str]) -> list[tuple[str, str, bool]]:
    """(path, object id, regular) for every blob under `roots` at `commit`."""
    reader = _parity_reader()
    entries = []
    for raw in _git(ledger, ["ls-tree", "-r", "-z", commit, "--", *roots]).split(b"\0"):
        if not raw:
            continue
        try:
            metadata, raw_path = raw.split(b"\t", 1)
            mode, object_type, object_id = metadata.decode().split()
            relative = _safe_git_path(raw_path.decode(errors="strict"))
        except (ValueError, UnicodeDecodeError, ExportRefused) as error:
            raise reader.ParitySummaryError(f"git returned malformed ls-tree entry {raw!r}: {error}") from error
        if not re.fullmatch(r"[0-9a-f]{40,64}", object_id):
            raise reader.ParitySummaryError(f"parity ledger path has invalid Git object id: {relative}")
        if not any(relative == root or relative.startswith(root + "/") for root in roots):
            raise reader.ParitySummaryError(f"git listed an unrequested parity ledger path: {relative}")
        entries.append((relative, object_id, object_type == "blob" and mode in {"100644", "100755"}))
    return entries


def _parity_blob(ledger: Path, object_id: str) -> bytes:
    return _git(ledger, ["cat-file", "blob", object_id])


def _parity_store_at(ledger: Path, commit: str) -> tuple[dict[str, bytes], int]:
    """The `parity/` shards hermit digests at `commit`, and the count of every file below `parity/`."""
    reader = _parity_reader()
    shards: dict[str, bytes] = {}
    files = 0
    try:
        for relative, object_id, regular in _parity_listing(ledger, commit, [reader.ROW_STORE]):
            files += 1
            if not regular:
                raise reader.ParitySummaryError(f"parity ledger path {relative} at {commit} is not a regular blob")
            if reader.is_store_shard(relative):
                shards[relative] = _parity_blob(ledger, object_id)
    except ExportRefused as error:
        raise reader.ParitySummaryError(str(error)) from error
    return shards, files


def _parity_is_ancestor(ledger: Path, ancestor: str, descendant: str) -> bool:
    reader = _parity_reader()
    arguments = ["merge-base", "--is-ancestor", ancestor, descendant]
    try:
        result = capture_bounded_process(
            ["git", "--no-replace-objects", "-C", str(ledger), *arguments],
            timeout=_GIT_TIMEOUT_SECONDS,
            stdout_limit=_MAX_GIT_STDOUT_BYTES,
            stderr_limit=_MAX_GIT_STDERR_BYTES,
            environment=_git_environment(),
        )
    except (OSError, subprocess.TimeoutExpired, OutputLimitExceeded) as error:
        raise reader.ParitySummaryError(f"cannot run git in {ledger}: {error}") from error
    if result.returncode in (0, 1):
        return result.returncode == 0
    raise reader.ParitySummaryError(
        f"git {' '.join(arguments)} failed: {_process_detail(result.stderr, result.stdout)}"
    )


def _parity_capture(ledger: Path, series_commit: str) -> dict[str, Any]:
    """Carry the ledger commit's parity summary, bound to the store it summarizes.

    hermit's scorecard writes `scorecard/parity.json` from the ledger's
    `parity/` store, and records the store's digest in
    `generated_from.store_sha256`. The mirror commits it beside the series, but
    a parity-only publication appends rows without regenerating it. So the
    digest is recomputed here from the shards at the series commit: equal, the
    summary is current; equal to the store at the earlier ledger commit it
    names, which the current store extends, it is stale and says how many
    newer record lines it omits; anything else refuses the summary. A parity
    refusal is carried to the parity panel and never refuses the export.
    """
    reader = _parity_reader()
    summary_path = reader.LEDGER_PATH
    try:
        listed = _parity_listing(ledger, series_commit, [summary_path, reader.ROW_STORE, reader.RUN_STORE])
    except (ExportRefused, reader.ParitySummaryError) as error:
        return reader.refused_carriage(
            series_commit, f"ledger commit {series_commit} parity paths cannot be listed: {error}",
            append_refusals_total=None,
        )
    summary_object = None
    store: dict[str, bytes] = {}
    run_shards: dict[str, bytes] = {}
    store_files = 0
    run_marker_files = 0
    irregular = []
    try:
        for relative, object_id, regular in listed:
            if not regular:
                irregular.append(relative)
            top = PurePosixPath(relative).parts[0]
            if relative == summary_path:
                summary_object = object_id
            elif top == reader.ROW_STORE:
                store_files += 1
                if regular and reader.is_store_shard(relative):
                    store[relative] = _parity_blob(ledger, object_id)
            else:
                run_marker_files += 1
                if regular:
                    run_shards[relative] = _parity_blob(ledger, object_id)
        refusals, refusals_total = reader.append_refusals_from(run_shards)
        facts = {"store_files": store_files, "run_marker_files": run_marker_files,
                 "append_refusals": refusals, "append_refusals_total": refusals_total}
        if irregular:
            return reader.refused_carriage(
                series_commit,
                f"ledger commit {series_commit} parity ledger path(s) are not regular blobs: "
                + ", ".join(irregular),
                **facts,
            )
        data = None if summary_object is None else _parity_blob(ledger, summary_object)
    except (ExportRefused, reader.ParitySummaryError) as error:
        return reader.refused_carriage(
            series_commit, f"ledger commit {series_commit} parity ledger cannot be read: {error}",
            append_refusals_total=None,
        )
    return reader.capture_carriage(
        series_commit,
        summary_object=summary_object,
        summary_data=data,
        store=store,
        store_files=store_files,
        run_marker_files=run_marker_files,
        read_store_at=lambda commit: _parity_store_at(ledger, commit),
        is_ancestor=lambda commit: _parity_is_ancestor(ledger, commit, series_commit),
        append_refusals=refusals,
        append_refusals_total=refusals_total,
    )


def _parity_without_paths(
    carriage: dict[str, Any], series_commit: str, checkouts: Mapping[str, Path],
) -> dict[str, Any]:
    """The parity carriage, naming each checkout by its role rather than its path.

    A git failure names the checkout it ran in, and the export refuses any
    text holding the absolute parent checkout path or an owner path. So the
    refusal names each checkout by its role, and a carriage that still holds
    such a path is replaced by a refusal that does not: a parity refusal stays
    in the parity panel and never refuses the export.

    A refusal quotes at most _MAX_PROCESS_DETAIL_BYTES of git's output, which
    can cut a checkout path short so that no role matches it. So a refused
    carriage that still holds any absolute path is replaced too. Only a
    refused one: a summary quotes the guest's own absolute paths (a
    determinism log's "/etc/...", for one), which must not refuse the panel.
    """
    reader = _parity_reader()
    roles: dict[str, str] = {}
    for role, checkout in checkouts.items():
        for path in (str(checkout), str(checkout.resolve())):
            roles.setdefault(path, f"the {role} checkout")
    refusal = carriage.get("refusal")
    if isinstance(refusal, str):
        for path in sorted(roles, key=len, reverse=True):
            refusal = refusal.replace(path, roles[path])
        carriage = {**carriage, "refusal": refusal}
    holds_path = any(_contains_string(carriage, needle) for needle in (*roles, _OWNER_PATH_NEEDLE))
    if holds_path or (carriage.get("state") == "refused" and _contains_absolute_path(carriage)):
        return reader.refused_carriage(
            series_commit,
            f"ledger commit {series_commit} parity data is refused: it holds an absolute path",
            append_refusals_total=None,
        )
    return carriage


def _proof_reader() -> Any:
    global finalized_proof
    if finalized_proof is None:
        path = SERIES_DIR / "finalized_proof.py"
        spec = importlib.util.spec_from_file_location("website_finalized_proof", path)
        if spec is None or spec.loader is None:
            raise ExportRefused("cannot load finalized proof byte reader")
        finalized_proof = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(finalized_proof)
    return finalized_proof


def captured_proof_objects(value: Any) -> dict[str, bytes]:
    """Validate retained public operands, without granting canonical authority.

    One UTF-8 byte string per digest is shared by every manifest referencing it.
    Original relative destinations remain separate and cannot disagree in bytes.
    The unchanged native reader still owns schema-10 evidence semantics.
    """
    proof = _proof_reader()
    if not isinstance(value, dict) or set(value) != {"schema", "manifests", "objects", "missing_current_terminals"}:
        raise ExportRefused("captured finalized proof has unknown or missing fields")
    if type(value["schema"]) is not int or value["schema"] != 1:
        raise ExportRefused("captured finalized proof schema is unsupported")
    if not isinstance(value["manifests"], list) or not isinstance(value["objects"], dict):
        raise ExportRefused("captured finalized proof populations have invalid types")
    blobs: dict[str, bytes] = {}
    for digest, text in value["objects"].items():
        if (not isinstance(digest, str) or re.fullmatch("[0-9a-f]{64}", digest) is None
                or not isinstance(text, str)):
            raise ExportRefused("captured proof object needs a digest and exact UTF-8 bytes")
        try:
            encoded = text.encode("utf-8", errors="strict")
        except UnicodeError as error:
            raise ExportRefused("captured proof object has invalid Unicode") from error
        if _sha256(encoded) != digest:
            raise ExportRefused("captured proof object digest differs from exact bytes")
        blobs[digest] = encoded
    destinations: dict[str, bytes] = {}
    referenced: set[str] = set()
    paths: list[str] = []
    identities: set[tuple[str, ...]] = set()
    events: set[str] = set()
    for manifest in value["manifests"]:
        proof.validate_manifest(manifest)
        path = proof.carrier_path(manifest)
        identity = proof.carrier_identity(manifest)
        event = manifest["terminal"]["event_id"]
        if identity in identities or event in events:
            raise ExportRefused("captured proof repeats or conflicts with a terminal")
        identities.add(identity)
        events.add(event)
        paths.append(path)
        total = 0
        for role in proof.artifact_roles(manifest["artifacts"]):
            descriptor = manifest["artifacts"][role]
            digest = descriptor["sha256"]
            encoded = blobs.get(digest)
            if encoded is None or len(encoded) != descriptor["bytes"]:
                raise ExportRefused("captured proof object is absent or its byte length differs")
            total += len(encoded)
            if total > proof.ARTIFACT_BYTES_PER_ROW:
                raise ExportRefused("captured proof exceeds the existing aggregate reader limit")
            proof.validate_artifact_bytes(encoded, role, manifest)
            destination = descriptor["path"]
            if destination in destinations and destinations[destination] != encoded:
                raise ExportRefused("original artifact path has conflicting captured bytes")
            destinations[destination] = encoded
            referenced.add(digest)
    if paths != sorted(set(paths)):
        raise ExportRefused("captured carriers must have unique sorted identities")
    if referenced != set(blobs):
        raise ExportRefused("captured proof has unreferenced objects")
    missing = value["missing_current_terminals"]
    terminal_fields = {"repository", "run_id", "record_id", "event_id", "event_sha256", "hermit_commit"}
    if not isinstance(missing, list):
        raise ExportRefused("missing current terminals must be an array")
    previous_missing: list[str] = []
    for terminal in missing:
        if (not isinstance(terminal, dict) or set(terminal) != terminal_fields
                or terminal["repository"] != "rrnewton/hermit"
                or any(not isinstance(terminal[key], str) or not terminal[key].strip()
                       for key in terminal_fields)
                or re.fullmatch("[0-9a-f]{64}", terminal["event_sha256"]) is None
                or re.fullmatch("[0-9a-f]{40}", terminal["hermit_commit"]) is None):
            raise ExportRefused("missing current terminal descriptor is invalid")
        if terminal["event_id"] in events:
            raise ExportRefused("captured proof both provides and omits a current terminal")
        events.add(terminal["event_id"])
        previous_missing.append(_canonical_bytes(terminal).decode())
    if previous_missing != sorted(set(previous_missing)):
        raise ExportRefused("missing current terminals must be unique and sorted")
    proof.require_publishable_strings(value)
    return destinations


def stage_captured_proof(root: Path, value: Any) -> None:
    _proof_reader().stage_artifacts(root, captured_proof_objects(value))


def _retained_evidence_quarantined(namespace: Any, row: Mapping[str, Any]) -> bool:
    """Whether a row is an allowlisted lost run; refuse any other marker.

    The namespace judges the marker, so the capture depends on its namespace
    argument and the proof reader only.
    """
    try:
        return namespace.retained_evidence_quarantined(row)
    except (RuntimeError, ValueError) as error:
        raise ExportRefused(str(error)) from error


def _capture_finalized_proof(namespace: Any, ledger: Path, commit: str) -> dict[str, Any]:
    proof = _proof_reader()
    index = proof.read_carriers(ledger, commit)
    manifests: list[dict] = []
    objects: dict[str, str] = {}
    missing: list[dict] = []
    # Query the complete correction groups first. In particular, a successor
    # with another schema still supersedes an older schema-10 terminal.
    candidates = namespace.terminals(repo="hermit", schema_version=10)
    # Historical schema-5 absence remains unavailable; it does not acquire a
    # modern stable-run requirement merely because another run has a census.
    # The canonical reader, not this exporter, selects whole groups for the
    # explicit producer claim and judges every predecessor/opposing successor.
    candidates.extend(namespace.census_terminals(repo="hermit"))
    selections = {}
    for selected in candidates:
        identity = tuple(selected["terminal"][key] for key in
                         ("repository", "run_id", "record_id", "event_id", "event_sha256"))
        if identity in selections and selections[identity] != selected:
            raise ExportRefused("canonical terminal differs across schema correction groups")
        selections[identity] = selected
    for selected in selections.values():
        row, terminal = selected["row"], selected["terminal"]
        if row.get("schema_version") not in (5, 10):
            continue
        if row.get("schema_version") == 5 and "raw_result_input_census_v1" not in row:
            continue
        if row.get("schema_version") == 5:
            proof.derived_row_binding(row)
        if _retained_evidence_quarantined(namespace, row):
            # A lost run's carrier cannot exist; it stays unavailable, as the
            # native verifier reports it, and never becomes proof operands.
            missing.append(terminal)
            continue
        manifest = proof.carrier_for_row(index, row, terminal)
        if manifest is None:
            # Missing transport is not fabricated evidence or a new admission
            # gate. The unchanged native verifier reports Unavailable and no
            # predecessor's operands are substituted for this current event.
            missing.append(terminal)
            continue
        namespace.verify_origin(manifest["parent"], terminal)
        for encoded in proof.read_artifact_objects(ledger, commit, manifest).values():
            if manifest["artifacts"]["schema"] == 2:
                proof.validate_derived_cells(encoded, row)
            digest = _sha256(encoded)
            text = encoded.decode("utf-8", errors="strict")
            if digest in objects and objects[digest] != text:
                raise ExportRefused("captured proof digest has conflicting bytes")
            objects[digest] = text
        manifests.append(manifest)
    value = {"schema": 1, "manifests": sorted(manifests, key=proof.carrier_path),
             "objects": dict(sorted(objects.items())),
             "missing_current_terminals": sorted(missing, key=_canonical_bytes)}
    captured_proof_objects(value)
    return value


def _validation_export(
    snapshot_root: Path,
    *, committed_source: tuple[Path, str] | None = None,
    namespace: Any = None,
) -> dict[str, Any]:
    # Both populations are only read here: counted, digested, and copied by
    # _portable_history_rows.  A namespace supplies its read-only views.
    events = (namespace.events_view() if namespace is not None else
              validation_ledger.events(root=snapshot_root))
    history_rows = (namespace.rows_view() if namespace is not None else
                    validation_ledger.rows(root=snapshot_root, committed_source=committed_source))
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
    *,
    repository: str = "https://github.com/rrnewton/dev-hermit.git",
) -> dict[str, Any]:
    published = series_store.published_snapshot_at(parent, commit, repository=repository)
    if published.commit != commit:
        raise ExportRefused(
            f"series snapshot resolved {published.commit}, expected {commit}"
        )
    copied_published_rows = series_store.union(published_paths)
    if _canonical_bytes(copied_published_rows) != _canonical_bytes(published.rows):
        raise ExportRefused(
            "published series rows disagree with the captured repository commit files"
        )
    rows = series_store.union([*published_paths, *local_paths])
    rows_jsonl = _opaque_jsonl(rows, "SeriesRow")
    return {
        "row_count": len(rows),
        "rows_jsonl": rows_jsonl.decode(),
        "rows_sha256": _sha256(rows_jsonl),
    }


def _series_quarantine_export(
    ledger: Path, commit: str, published_paths: list[Path],
) -> dict[str, Any]:
    """The series writer's published refusals at the series rows' own commit.

    The projection needs them to account for a derived census attempt that the
    public series omits, so they must come from the same ledger commit.
    """
    resolved, published = series_store.published_quarantine_at_repo(ledger, commit)
    if resolved != commit:
        raise ExportRefused(
            f"series quarantine snapshot resolved {resolved}, expected {commit}"
        )
    copied = series_store.union_events(series_store.validate_quarantine_records(
        [
            series_store.parse_line(line)
            for path in published_paths
            for line in path.read_text(encoding="utf-8").split("\n")
            if line.strip()
        ],
        public=True,
    ))
    if _canonical_bytes(copied) != _canonical_bytes(published):
        raise ExportRefused(
            "published series quarantine disagrees with the captured ledger commit files"
        )
    rows_jsonl = _opaque_jsonl(published, "series quarantine")
    return {
        "quarantine_row_count": len(published),
        "quarantine_rows_jsonl": rows_jsonl.decode(),
        "quarantine_rows_sha256": _sha256(rows_jsonl),
    }


def _provenance_sources(
    snapshot_root: Path,
    git_files: dict[str, GitSource],
    mutable: list[tuple[MutableSource, bytes]],
) -> list[dict[str, Any]]:
    selected_paths = [
        *validation_ledger.shard_paths(snapshot_root),
        *sorted((snapshot_root / "series").glob("*/*/*.jsonl")),
        *_snapshot_quarantine_paths(snapshot_root),
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


def build_export(
    parent_value: Path, published_ref: str, series_ref: str = "refs/remotes/origin/main",
    *, published_only: bool = False, state_root: Path | None = None,
    validation_history_commit: str | None = None,
) -> dict[str, Any]:
    _refuse_authority_overrides()
    reader_before = _reader_state()
    imported_reader = _load_authorities()
    if _canonical_bytes(reader_before) != _canonical_bytes(imported_reader):
        raise ExportRefused("reader commit or files changed before authority import")
    parent = _require_repository_root(parent_value)
    commit = _resolve_commit(parent, published_ref)
    state = _require_repository_root(state_root) if state_root is not None else parent
    if validation_history_commit is not None:
        if not isinstance(validation_history_commit, str) or _SHA_RE.fullmatch(validation_history_commit) is None:
            raise ExportRefused("validation history requires a full immutable commit")
        if not published_only:
            raise ExportRefused("fresh validation history requires published-only capture")
    data_root = series_store.shared_parent_root(state).resolve()
    if not data_root.is_dir():
        raise ExportRefused(f"parent data root is unavailable: {data_root}")

    # Validation receipts still belong to dev-hermit. Cell history belongs to
    # the ledger repository; never interpret its commit as a parent commit.
    ledger = series_store.ledger_root(state)
    series_commit = _resolve_commit(ledger, series_ref)
    series_tree = _git(ledger, ["rev-parse", f"{series_commit}:series"]).decode().strip()
    # Freeze public series first; source code and canonical P data are distinct.
    git_files = {
        path: source for path, source in _git_input_files(ledger, series_commit, ("series",)).items()
        if source.store == "series"
    }
    git_files.update(_series_quarantine_files(ledger, series_commit))
    if validation_history_commit is None:
        git_files.update(_git_input_files(parent, commit, ("ledger", "ledger-recovery")))
    mutable = [] if published_only else _capture_mutable_sources(state, data_root)

    with tempfile.TemporaryDirectory(
        prefix="compatibility-website-inputs-"
    ) as raw_snapshot:
        snapshot_root = Path(raw_snapshot)
        namespace = None
        if validation_history_commit is not None:
            commit = validation_history_commit
            # This is the real canonical Git root. The temporary snapshot only
            # supplies portable row pathname expansion, never Git authority.
            namespace = validation_ledger.capture_committed_validation_namespace(
                state, commit, state_root=snapshot_root,
            )
            for source in namespace.source_files():
                relative = source["path"]
                store, location = _input_store_and_location(relative) or ("", "")
                git_files[relative] = GitSource(store, location, relative,
                                               source["blob_oid"], source["data"])
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
        validation = _validation_export(
            snapshot_root, committed_source=((state if namespace is not None else parent), commit),
            **({"namespace": namespace} if namespace is not None else {}),
        )
        series = _series_export(
            state,
            series_commit,
            published_series_paths,
            local_series_paths,
            repository=series_store.LEDGER_REPOSITORY,
        )
        series.update(_series_quarantine_export(
            ledger, series_commit, _snapshot_quarantine_paths(snapshot_root),
        ))
        sources = _provenance_sources(snapshot_root, git_files, mutable)
        parity = _parity_without_paths(
            _parity_capture(ledger, series_commit), series_commit,
            {"ledger": ledger, "parent": parent, "shared": data_root},
        )

        result = {
            "incomplete_data": _incomplete_data(),
            "parity": parity,
            "provenance": {
                "parent_commit": commit,
                "published_ref": commit if namespace is not None else published_ref,
                "series_repository": series_store.LEDGER_REPOSITORY,
                "series_commit": series_commit,
                "series_tree": series_tree,
                "published_only": published_only,
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
        if namespace is not None:
            result["provenance"]["finalized_proof"] = _capture_finalized_proof(
                namespace, ledger, series_commit,
            )
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
    *, series_commit: str | None = None,
) -> dict[str, Any]:
    """Export named history and optional cell/proof data at exact immutable commits."""
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
    ledger = None
    if series_commit is not None:
        if _SHA_RE.fullmatch(series_commit) is None:
            raise ExportRefused("historical --series-commit must be a full immutable lowercase commit; do not use a moving ref")
        ledger = series_store.ledger_root(parent)
        if _resolve_commit(ledger, series_commit) != series_commit:
            raise ExportRefused("historical cell ledger revision differs from the requested commit")
        git_files = {path: source for path, source in git_files.items() if source.store != "series"}
        git_files.update(_git_input_files(ledger, series_commit, ("series",)))
        git_files.update(_series_quarantine_files(ledger, series_commit))

    with tempfile.TemporaryDirectory(
        prefix="compatibility-website-historical-inputs-"
    ) as raw_snapshot, ExitStack() as stack:
        snapshot_root = Path(raw_snapshot)
        namespace = None
        if ledger is not None:
            namespace = stack.enter_context(validation_ledger.capture_committed_validation_namespace(
                parent, commit, state_root=snapshot_root,
            ))
        for relative, source in git_files.items():
            _write_snapshot_file(snapshot_root, relative, source.data)

        published_series_paths = sorted((snapshot_root / "series").glob("*/*/*.jsonl"))
        validation = _validation_export(snapshot_root, committed_source=(parent, commit),
                                        **({"namespace": namespace} if namespace is not None else {}))
        series = _series_export(
            parent, series_commit or commit, published_series_paths, [],
            **({"repository": series_store.LEDGER_REPOSITORY} if ledger is not None else {}),
        )
        if ledger is not None:
            series.update(_series_quarantine_export(
                ledger, series_commit, _snapshot_quarantine_paths(snapshot_root),
            ))
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
        if ledger is not None:
            result["provenance"].update(
                series_repository=series_store.LEDGER_REPOSITORY,
                series_commit=series_commit,
                series_tree=_git(ledger, ["rev-parse", f"{series_commit}:series"]).decode().strip(),
                finalized_proof=_capture_finalized_proof(namespace, ledger, series_commit),
            )
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
    parser.add_argument("--series-ref", default="refs/remotes/origin/main",
                        help="cell-ledger commit or ref for current exports; resolved once, never fetched")
    parser.add_argument("--series-commit", help="full immutable cell-ledger commit for historical series and finalized proof; requires --historical")
    parser.add_argument("--published-only", action="store_true",
                        help="exclude all unpublished and retained local spool files")
    parser.add_argument("--state-root", type=Path,
                        help="shared checkout owning the cell ledger and local spools; defaults to --parent")
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
                **({"series_commit": arguments.series_commit} if arguments.series_commit is not None else {}),
            )
        else:
            if arguments.series_commit is not None:
                raise ExportRefused("--series-commit requires --historical; use --series-ref for current exports")
            if arguments.record_id is not None or arguments.run_id is not None:
                raise ExportRefused("--record-id and --run-id require --historical")
            value = build_export(arguments.parent, arguments.published_ref, arguments.series_ref,
                                 published_only=arguments.published_only, state_root=arguments.state_root)
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
