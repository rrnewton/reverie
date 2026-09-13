#!/usr/bin/env python3
"""Publish ledger or per-test series events to shared Git-backed history.

Three properties, in the order they matter:

**A local event is never lost.** Every event is written to its local spool
*before* any Git operation, and the spool entry is dropped only after the
commit carrying it is confirmed to be an ancestor of the freshly-fetched remote
branch. Push reporting success is not that confirmation — a push can succeed
against a ref that is then replaced — so publication is verified by ancestry,
not by an exit code.

**A concurrent publisher never costs us a row.** Two machines write different
shards and merge trivially. Two producers on one machine write immutable spool
batches and `ledger.append_events` emits every line with one `O_APPEND` write.
The remaining case is the interesting one: a
non-fast-forward push, meaning someone appended to the same shard between our
read and our push. That is resolved by an APPEND-AWARE merge — re-read the
remote shard, keep every remote line in place, and re-append only the events
that are not already there. Never `git checkout --theirs`, never a rewrite: the
file is append-only and a whole-file replacement is precisely the data loss
this module exists to prevent.

**No row is ever rewritten.** `verify_append_only` runs against the remote
content before each push. If our proposed content is not a line-for-line
extension of what is published, the publish is refused rather than forced.

**A series snapshot cannot miss a row during publication.** Series publication
holds the spool's `.append.lock` only long enough to copy the exact pending
files, releases it for Git/network work, then reacquires it to mark the exact
published commit and retain only unchanged files. New appends stay live. The
scorecard snapshot takes the same lock in shared mode before binding its local
published-commit marker and holds it through output readback. The public
operation owns each bounded lock phase; its internal drain and reap helpers
never reacquire it.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import stat
import subprocess
import sys
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

from ledger import (  # noqa: F401 - re-exported for callers
    MalformedEvent,
    append_events,
    parse_line,
    read_event_shard,
    read_shard,
    shard_path,
    union_events,
    validate_events,
    verify_append_only,
)

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

DEFAULT_REMOTE = "origin"
DEFAULT_BRANCH = "main"
#: Bounded so a genuinely wedged remote surfaces as a failure with the spool
#: intact, instead of spinning forever and looking healthy.
DEFAULT_ATTEMPTS = 5
STORES = ("ledger", "series")


class PublishRefused(Exception):
    """The publish would have rewritten published bytes, or the events are invalid."""


@dataclass(frozen=True)
class _SeriesBatchFile:
    """Exact bytes and inode captured during the short publication lock."""

    path: Path
    device: int
    inode: int
    mode: int
    size: int
    mtime_ns: int
    ctime_ns: int
    sha256: str
    payload: bytes


def _series_store():
    """Load the sibling series module only for a series publication."""
    name = "ci_hub_series_store_for_publisher"
    existing = sys.modules.get(name)
    if existing is not None:
        return existing
    path = Path(__file__).resolve().parents[1] / "series" / "series.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise PublishRefused(f"cannot load series store from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def _git(repo: Path, *args: str, check: bool = True) -> bounded_capture.CapturedProcess:
    return bounded_capture.git(args, cwd=repo, check=check)


def _failure_detail(result: subprocess.CompletedProcess) -> str:
    """The WHOLE reason a push or a serialized write failed, never its last line.

    `scripts/parent-main-write` prints its explained `REFUSED: <reason>` FIRST
    and may then print a multi-line generic banner. Keeping
    `stderr.splitlines()[-1]` — which this module did — therefore reported the
    banner's tail, "change: nothing was published and nothing was modified",
    and discarded the cause outright.

    Measured 2026-08-09, publishing the devbig030 receipt for hermit
    `2b38d8e6`: the whole stderr was

        parent-main-write: REFUSED: machine-attribution checker is missing:
          /home/newton/work/dev-hermit/hermit/scripts/check_commit_attribution.py
        parent-main-write: ABORTED at line 124 with status 1, and no reason
          was printed. ...

    The first line names the entire root cause; the reported last line names
    nothing. A truncation that keeps the least informative end of a
    deliberately-explained refusal is worse than no capture at all, because it
    reads as a complete answer.
    """
    detail = result.stderr.strip() or result.stdout.strip()
    return detail or f"exited {result.returncode} with no output"


def _root_commits(repo: Path, *refs: str) -> set[str]:
    """Root commit ids — the identity a directory name cannot forge.

    Two checkouts are replicas of one repository iff they share a root commit.
    A path component, a remote URL, or a directory name is a LABEL: it can be
    edited, and it can be plausible while being wrong.
    """
    for ref in refs:
        found = _git(repo, "rev-list", "--max-parents=0", ref, check=False)
        if found.returncode == 0 and found.stdout.split():
            return set(found.stdout.split())
    return set()


# --------------------------------------------------------------------------- spool


def spool_file(
    spool_dir: str | os.PathLike[str], team: str, host: str, when: str,
    batch_id: str | None = None,
) -> Path:
    identity = batch_id or uuid.uuid4().hex
    return Path(spool_dir) / f"{team}__{host}__{when[:7]}__{identity}.jsonl"


def spool(spool_dir: str | os.PathLike[str], team: str, host: str, events: Sequence[dict]) -> Path:
    """Durably record events locally before any network or Git work.

    This is the ordering that makes loss impossible: if everything after this
    call fails, the events are still on disk and the next publish picks them
    up. Each call builds a uniquely named batch outside the publisher's glob,
    then atomically renames it into view; concurrent producers never share a
    file and a publisher never observes a half-built batch.
    """
    if not events:
        raise PublishRefused("refusing to spool an empty batch")
    when = str(events[0].get("emitted_at", ""))
    target = spool_file(spool_dir, team, host, when)
    temporary = target.with_suffix(".tmp")
    append_events(temporary, events)
    # Publishers ignore .tmp. The completed batch becomes visible all at once,
    # and is thereafter immutable, so draining a snapshot cannot unlink a row
    # appended by a producer that stayed active during publication.
    os.replace(temporary, target)
    return target


def _spool_snapshot(
    spool_dir: str | os.PathLike[str],
) -> tuple[dict[tuple[str, str, str], list[dict]], list[Path]]:
    """One immutable batch snapshot and the exact files that comprise it."""
    pending: dict[tuple[str, str, str], list[dict]] = {}
    files: list[Path] = []
    root = Path(spool_dir)
    if not root.is_dir():
        return pending, files
    for path in sorted(root.glob("*.jsonl")):
        parts = path.stem.split("__")
        if len(parts) not in {3, 4}:
            raise PublishRefused(f"unrecognised spool batch name: {path.name}")
        team, host, month = parts[:3]
        pending.setdefault((team, host, month), []).extend(read_shard(path))
        files.append(path)
    return ({key: union_events(value) for key, value in pending.items()}, files)


def _series_spool_snapshot(
    spool_dir: str | os.PathLike[str],
) -> tuple[dict[tuple[str, str, str], list[dict]], list[Path]]:
    """Group stable run-id batches by the tracked shard they publish into."""
    pending: dict[tuple[str, str, str], list[dict]] = {}
    files: list[Path] = []
    root = Path(spool_dir)
    if not root.is_dir():
        return pending, files
    series_store = _series_store()
    for path in sorted(root.glob("*.jsonl")):
        events = read_shard(path)
        try:
            # A spool batch was already written by its producer and may use a
            # retained readable schema. Keep the current-write boundary in
            # series.append_rows(); publication must not strand older pending
            # batches merely because the writer has since advanced.
            series_store.validate_read_rows(events)
        except series_store.SeriesError as error:
            raise PublishRefused(f"{path}: {error}") from error
        for event in events:
            try:
                emitted_at = str(event.get("emitted_at", ""))
                month = emitted_at[:7]
                team = str(event.get("team", ""))
                host = str(event.get("host", ""))
                series_store.shard_path(team, host, month)
            except series_store.SeriesError as error:
                raise PublishRefused(f"{path}: {error}") from error
            pending.setdefault((team, host, month), []).append(event)
        files.append(path)
    return ({key: union_events(value) for key, value in pending.items()}, files)


def _file_signature(status: os.stat_result) -> tuple[int, ...]:
    return (
        status.st_dev,
        status.st_ino,
        status.st_mode,
        status.st_size,
        status.st_mtime_ns,
        status.st_ctime_ns,
    )


def _read_descriptor(descriptor: int) -> bytes:
    chunks = []
    offset = 0
    while True:
        chunk = os.pread(descriptor, 1024 * 1024, offset)
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)
        offset += len(chunk)


def _capture_series_batch_files(lock) -> list[_SeriesBatchFile]:
    """Copy exact live batch bytes while holding the short exclusive lock."""
    captured = []
    flags = os.O_RDONLY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    for name in sorted(
        item for item in os.listdir(lock.spool_fd) if item.endswith(".jsonl")
    ):
        display = lock.spool / name
        try:
            descriptor = os.open(name, flags, dir_fd=lock.spool_fd)
        except OSError as error:
            raise PublishRefused(
                f"cannot bind series publication batch {display}: {error}"
            ) from error
        try:
            before = os.fstat(descriptor)
            named = os.stat(name, dir_fd=lock.spool_fd, follow_symlinks=False)
            if (
                not stat.S_ISREG(before.st_mode)
                or not stat.S_ISREG(named.st_mode)
                or _file_signature(before) != _file_signature(named)
            ):
                raise PublishRefused(
                    f"series publication batch changed while opening: {display}"
                )
            payload = _read_descriptor(descriptor)
            after = os.fstat(descriptor)
            named_after = os.stat(
                name, dir_fd=lock.spool_fd, follow_symlinks=False,
            )
            if (
                _file_signature(after) != _file_signature(before)
                or _file_signature(named_after) != _file_signature(before)
            ):
                raise PublishRefused(
                    f"series publication batch changed while reading: {display}"
                )
            captured.append(
                _SeriesBatchFile(
                    path=display,
                    device=before.st_dev,
                    inode=before.st_ino,
                    mode=before.st_mode,
                    size=before.st_size,
                    mtime_ns=before.st_mtime_ns,
                    ctime_ns=before.st_ctime_ns,
                    sha256=hashlib.sha256(payload).hexdigest(),
                    payload=payload,
                )
            )
        finally:
            os.close(descriptor)
    return captured


def _pending_from_series_batch_files(
    files: Sequence[_SeriesBatchFile],
) -> dict[tuple[str, str, str], list[dict]]:
    """Parse the immutable in-memory batch after releasing the spool lock."""
    pending: dict[tuple[str, str, str], list[dict]] = {}
    series_store = _series_store()
    for captured in files:
        if captured.payload and not captured.payload.endswith(b"\n"):
            raise PublishRefused(
                f"{captured.path}: final line is truncated"
            )
        try:
            text = captured.payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise PublishRefused(
                f"{captured.path}: batch is not UTF-8: {error}"
            ) from error
        events = [
            parse_line(line) for line in text.splitlines() if line.strip()
        ]
        try:
            series_store.validate_read_rows(events)
        except series_store.SeriesError as error:
            raise PublishRefused(
                f"{captured.path}: series event batch is invalid: {error}"
            ) from error
        for event in events:
            try:
                emitted_at = str(event.get("emitted_at", ""))
                month = emitted_at[:7]
                team = str(event.get("team", ""))
                host = str(event.get("host", ""))
                series_store.shard_path(team, host, month)
            except series_store.SeriesError as error:
                raise PublishRefused(f"{captured.path}: {error}") from error
            pending.setdefault((team, host, month), []).append(event)
    return {key: union_events(value) for key, value in pending.items()}


def _series_batch_file_unchanged(lock, captured: _SeriesBatchFile) -> bool:
    """True only when the original live pathname still names the exact bytes."""
    if captured.path.parent != lock.spool:
        return False
    flags = os.O_RDONLY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(captured.path.name, flags, dir_fd=lock.spool_fd)
    except OSError:
        return False
    try:
        status = os.fstat(descriptor)
        named = os.stat(
            captured.path.name,
            dir_fd=lock.spool_fd,
            follow_symlinks=False,
        )
        expected = (
            captured.device,
            captured.inode,
            captured.mode,
            captured.size,
            captured.mtime_ns,
            captured.ctime_ns,
        )
        return (
            stat.S_ISREG(status.st_mode)
            and stat.S_ISREG(named.st_mode)
            and _file_signature(status) == expected
            and _file_signature(named) == expected
            and hashlib.sha256(_read_descriptor(descriptor)).hexdigest()
            == captured.sha256
            and _file_signature(os.fstat(descriptor)) == expected
            and _file_signature(
                os.stat(
                    captured.path.name,
                    dir_fd=lock.spool_fd,
                    follow_symlinks=False,
                )
            )
            == expected
        )
    except OSError:
        return False
    finally:
        os.close(descriptor)


def _prepare_series_snapshot_marker(
    source_root: Path | None,
    published_commit: str,
    *,
    remote: str,
    branch: str,
) -> set[str] | None:
    """Fetch marker objects without holding the series spool lock."""
    if source_root is None:
        return None
    fetched = _git(source_root, "fetch", remote, branch, check=False)
    if fetched.returncode != 0:
        raise PublishRefused(
            "series batch is published but its source-root marker fetch failed; "
            f"spool retained: {_failure_detail(fetched)}"
        )
    present = _git(
        source_root,
        "cat-file",
        "-e",
        f"{published_commit}^{{commit}}",
        check=False,
    )
    if present.returncode != 0:
        raise PublishRefused(
            f"series batch is published at {published_commit}, but that commit is "
            "unavailable in the source root; spool retained"
        )
    ancestry = _git(
        source_root,
        "merge-base",
        "--is-ancestor",
        published_commit,
        f"{remote}/{branch}",
        check=False,
    )
    if ancestry.returncode != 0:
        raise PublishRefused(
            f"series batch commit {published_commit} is not in freshly fetched "
            f"{remote}/{branch}; spool retained"
        )
    marker_event_ids = _source_commit_event_ids(
        source_root, published_commit, store="series",
    )
    head = _git(
        source_root, "rev-parse", "--verify", "HEAD^{commit}", check=False,
    )
    if head.returncode != 0:
        raise PublishRefused(
            "cannot bind the producing checkout HEAD before retained-row reaping; "
            "spool retained"
        )
    head_event_ids = _source_commit_event_ids(
        source_root, head.stdout.strip(), store="series",
    )
    # Retention serves both readers: ordinary producer-local readers consume
    # HEAD plus the spool, while scorecard snapshots consume the marker plus
    # the spool. A retained row can disappear only after both immutable views
    # contain it.
    return marker_event_ids & head_event_ids


def _source_commit_event_ids(
    source_root: Path, commit: str, *, store: str,
) -> set[str]:
    """Read event identities from one immutable source-root commit."""
    listed = _git(
        source_root,
        "ls-tree",
        "-r",
        "--name-only",
        commit,
        "--",
        store,
        check=False,
    )
    if listed.returncode != 0:
        raise PublishRefused(
            f"cannot list {store} rows at source commit {commit}: "
            f"{_failure_detail(listed)}"
        )
    present = set()
    for relative in listed.stdout.splitlines():
        parts = Path(relative).parts
        if (
            len(parts) != 4
            or parts[0] != store
            or Path(relative).suffix != ".jsonl"
        ):
            continue
        shown = _git(
            source_root, "show", f"{commit}:{relative}", check=False,
        )
        if shown.returncode != 0:
            raise PublishRefused(
                f"cannot read {relative} at source commit {commit}: "
                f"{_failure_detail(shown)}"
            )
        if shown.stdout and not shown.stdout.endswith("\n"):
            raise PublishRefused(
                f"{relative} at source commit {commit} has a truncated final line"
            )
        events = [
            parse_line(line) for line in shown.stdout.splitlines() if line.strip()
        ]
        _validate_store_events(store, events, f"{commit}:{relative}", read=True)
        present.update(
            identifier
            for event in events
            if isinstance((identifier := event.get("event_id")), str) and identifier
        )
    return present


def _publish_series_snapshot_marker_locked(
    source_root: Path, published_commit: str, series_store,
) -> str:
    """Advance the exact local marker before any represented source is drained."""
    marker_ref = series_store.SERIES_SNAPSHOT_REF
    resolved = _git(
        source_root, "rev-parse", "--verify", f"{marker_ref}^{{commit}}", check=False,
    )
    previous = resolved.stdout.strip() if resolved.returncode == 0 else None
    target = published_commit
    if previous and previous != published_commit:
        previous_before_new = _git(
            source_root,
            "merge-base",
            "--is-ancestor",
            previous,
            published_commit,
            check=False,
        )
        if previous_before_new.returncode != 0:
            new_before_previous = _git(
                source_root,
                "merge-base",
                "--is-ancestor",
                published_commit,
                previous,
                check=False,
            )
            if new_before_previous.returncode != 0:
                raise PublishRefused(
                    "series snapshot marker and newly published commit are not comparable; "
                    "spool retained"
                )
            target = previous
    old = previous or "0" * 40
    updated = _git(
        source_root, "update-ref", marker_ref, target, old, check=False,
    )
    if updated.returncode != 0:
        raise PublishRefused(
            f"cannot publish local series snapshot marker {marker_ref}; spool retained: "
            f"{_failure_detail(updated)}"
        )
    verified = _git(
        source_root, "rev-parse", "--verify", f"{marker_ref}^{{commit}}", check=False,
    )
    if verified.returncode != 0 or verified.stdout.strip() != target:
        raise PublishRefused(
            f"local series snapshot marker {marker_ref} did not read back as {target}; "
            "spool retained"
        )
    return target


def _store_snapshot(
    store: str, spool_dir: str | os.PathLike[str]
) -> tuple[dict[tuple[str, str, str], list[dict]], list[Path]]:
    if store == "ledger":
        return _spool_snapshot(spool_dir)
    if store == "series":
        return _series_spool_snapshot(spool_dir)
    raise PublishRefused(f"unknown store {store!r}; expected one of {STORES}")


def _store_shard(store: str, team: str, host: str, month: str) -> str:
    if store == "ledger":
        return f"ledger/{team}/{host}/{month}.jsonl"
    if store == "series":
        return _series_store().shard_path(team, host, month)
    raise PublishRefused(f"unknown store {store!r}; expected one of {STORES}")


def _validate_store_events(
    store: str, events: Sequence[dict], relative: str, *, read: bool = False
) -> None:
    if store == "ledger":
        for index, codes in enumerate(validate_events(events, relative), 1):
            if codes:
                raise PublishRefused(
                    f"{relative}:{index}: event {events[index - 1].get('event_id')!r} "
                    f"violates {codes}"
                )
        return
    if store == "series":
        series_store = _series_store()
        try:
            if read:
                series_store.validate_read_rows(events)
            else:
                series_store.validate_rows(events)
        except series_store.SeriesError as error:
            raise PublishRefused(
                f"{relative}: series event batch is invalid: {error}"
            ) from error
        return
    raise PublishRefused(f"unknown store {store!r}; expected one of {STORES}")


#: Batches proven published but not yet durable in the PRODUCING checkout.
#:
#: Dropping a batch the instant ancestry is proven blinds its own producer:
#: publication writes through a separate dedicated clone, so between the drop
#: and the producer's next sync the producing root has lost the spool copy and
#: not yet gained the published shard. Measured 2026-08-11 on devbig014 -- a
#: producer answered NOT-VALIDATED with 0 records for a receipt it had published
#: seconds earlier, which is indistinguishable from "never ran".
#:
#: Retention is a MOVE, not a copy, and it is atomic within one filesystem. The
#: batch stays readable by `spooled()` so the producer never goes blind, while
#: `_spool_snapshot` keeps globbing only the top level so a retained batch is
#: never republished. `ledger_spool_backlog` filters on `*.jsonl` entries and so
#: does not count this directory as backlog -- retained is published, not stalled.
RETAINED_SUBDIR = "published"


def retained_dir(spool_dir: str | os.PathLike[str]) -> Path:
    return Path(spool_dir) / RETAINED_SUBDIR


def retained(spool_dir: str | os.PathLike[str]) -> dict[tuple[str, str, str], list[dict]]:
    """Published-but-not-yet-durable batches, keyed like ``spooled``."""
    return _spool_snapshot(retained_dir(spool_dir))[0]


def spooled(spool_dir: str | os.PathLike[str]) -> dict[tuple[str, str, str], list[dict]]:
    """Every PENDING event, keyed by (team, host, month).

    Pending means not yet published. Retained batches are excluded here on
    purpose: this is the publisher's view, and a retained batch must never be
    republished. The READER's view is `validate_rows.events`, which unions this
    with `retained` so a producer keeps seeing its own receipt.
    """
    return _spool_snapshot(spool_dir)[0]


def stage_tracked(
    source_root: str | os.PathLike[str], spool_dir: str | os.PathLike[str]
) -> dict:
    """Copy tracked history into immutable spool batches without changing it."""
    source_root = Path(source_root).resolve()
    staged = 0
    batches: list[str] = []
    pattern = "ledger/*/*/[0-9][0-9][0-9][0-9]-[0-9][0-9].jsonl"
    for path in sorted(source_root.glob(pattern)):
        relative = path.relative_to(source_root)
        team, host = relative.parts[1], relative.parts[2]
        events = read_event_shard(path)
        if not events:
            continue
        target = spool(spool_dir, team, host, events)
        batches.append(str(target))
        staged += len(events)
    return {"staged": staged, "batches": batches, "source_unchanged": True}


# --------------------------------------------------------------------------- merge


def merge_shard(
    remote_text: str,
    events: Sequence[dict],
    *,
    store: str = "ledger",
    read_events: bool = False,
) -> str:
    """Append-aware merge: published lines stay put, new events go after them.

    Returns the full shard text. An event already present remotely is not
    re-appended, which is what makes a retry idempotent — the same batch pushed
    twice yields one copy, not two.
    """
    remote_lines = [ln for ln in remote_text.splitlines() if ln.strip()]
    if remote_text and not remote_text.endswith("\n"):
        raise MalformedEvent("remote shard's final line is truncated; refusing to append")
    remote_events = [parse_line(line) for line in remote_lines]
    _validate_store_events(store, remote_events, f"remote {store} shard", read=True)
    _validate_store_events(
        store,
        events,
        f"proposed {store} shard append",
        read=read_events,
    )
    have = {ln for ln in remote_lines}
    have_by_id: dict[str, dict] = {}
    for event in remote_events:
        have_by_id[str(event.get("event_id", ""))] = event
    additions = []
    for event in events:
        line = json.dumps(event, sort_keys=True)
        event_id = str(event.get("event_id", ""))
        prior = have_by_id.get(event_id)
        if prior is not None:
            if json.dumps(prior, sort_keys=True) != line:
                raise PublishRefused(
                    f"event_id {event_id!r} is already published with a different body"
                )
            continue
        if line in have:
            continue
        have.add(line)
        have_by_id[event_id] = event
        additions.append(line)
    body = remote_lines + additions
    return "".join(line + "\n" for line in body)


def _events_from_text(text: str, label: str) -> list[dict]:
    if text and not text.endswith("\n"):
        raise PublishRefused(f"{label}: final line is truncated")
    events = [parse_line(line) for line in text.splitlines() if line.strip()]
    _validate_store_events("ledger", events, label)
    return events


def split_published_to_match_source(
    repo: str | os.PathLike[str],
    source_root: str | os.PathLike[str],
    shards: Sequence[str],
    *,
    remote: str = DEFAULT_REMOTE,
    branch: str = DEFAULT_BRANCH,
    serialized_writer: str | os.PathLike[str] | None = None,
    attribution: str,
) -> dict:
    """Preserve a divergent remote shard, then make its primary match source.

    This is a migration-only escape hatch for two append-only replicas that
    already diverged in line order. The published bytes are moved verbatim to
    an immutable ``ledger-recovery`` shard in the SAME commit that makes the
    primary byte-identical to the untouched source. It refuses unless every
    source event is already present remotely with the same body, and verifies
    that the post-split adapter union is exactly the pre-split remote union.
    """
    repo = Path(repo).resolve()
    source_root = Path(source_root).resolve()
    if repo == source_root:
        raise PublishRefused(
            "split checkout is the shared producer parent; use a dedicated clone"
        )
    if "\n" in attribution or not attribution:
        raise PublishRefused("split publication requires one final attribution line")
    if serialized_writer is not None and (remote != "origin" or branch != "main"):
        raise PublishRefused("serialized parent publication requires origin/main")
    if _git(repo, "status", "--porcelain").stdout.strip():
        raise PublishRefused("split checkout is not clean")

    _git(repo, "fetch", remote, branch)
    _git(repo, "checkout", "--detach", f"{remote}/{branch}")
    remote_commit = _git(repo, "rev-parse", f"{remote}/{branch}").stdout.strip()
    written: list[str] = []
    evidence: list[dict] = []
    if not shards:
        raise PublishRefused("split requires at least one explicit primary shard")
    for rel in sorted(shards):
        parts = Path(rel).parts
        if (
            len(parts) != 4
            or parts[0] != "ledger"
            or len(Path(parts[3]).stem) != 7
            or Path(parts[3]).suffix != ".jsonl"
        ):
            raise PublishRefused(f"not a primary ledger shard: {rel}")
        source = source_root / rel
        if not source.is_file():
            raise PublishRefused(f"source shard does not exist: {rel}")
        shown = _git(repo, "show", f"{remote}/{branch}:{rel}", check=False)
        if shown.returncode != 0:
            raise PublishRefused(f"{rel}: source shard is not published")
        source_text = source.read_text()
        remote_text = shown.stdout
        if source_text == remote_text:
            continue
        source_events = _events_from_text(source_text, f"source {rel}")
        remote_events = _events_from_text(remote_text, f"remote {rel}")
        source_by_id = {event.get("event_id"): event for event in source_events}
        remote_by_id = {event.get("event_id"): event for event in remote_events}
        union_events([*remote_events, *source_events])  # refuses body conflicts
        missing = sorted(set(source_by_id) - set(remote_by_id))
        if missing:
            raise PublishRefused(
                f"{rel}: {len(missing)} source event(s) are not published; publish first"
            )

        _, team, host, filename = Path(rel).parts
        month = Path(filename).stem
        recovery_rel = (
            Path("ledger-recovery") / team / host / month / f"{remote_commit}.jsonl"
        ).as_posix()
        recovery = repo / recovery_rel
        recovery.parent.mkdir(parents=True, exist_ok=True)
        recovery.write_text(remote_text)
        (repo / rel).write_text(source_text)
        post_union = union_events([*source_events, *remote_events])
        if {e["event_id"] for e in post_union} != set(remote_by_id):
            raise PublishRefused(f"{rel}: split would change the published event-id union")
        written.extend([rel, recovery_rel])
        evidence.append(
            {
                "primary": rel,
                "recovery": recovery_rel,
                "source_events": len(source_by_id),
                "preserved_events": len(remote_by_id),
            }
        )

    if not written:
        return {"split": 0, "commit": None, "note": "all primary shards already match"}
    _git(repo, "add", "--", *written)
    message = (
        "ledger: preserve divergent history while adopting live shard bytes"
        f"\n\n{attribution}"
    )
    _git(repo, "commit", "-q", "-m", message, "--", *written)
    commit = _git(repo, "rev-parse", "HEAD").stdout.strip()
    if serialized_writer is None:
        pushed = _git(repo, "push", remote, f"HEAD:refs/heads/{branch}", check=False)
    else:
        pushed = bounded_capture.capture(
            [str(serialized_writer), "publish", commit], cwd=repo,
        )
    if pushed.returncode != 0:
        raise PublishRefused(
            f"split commit retained but not published: {_failure_detail(pushed)}"
        )
    _git(repo, "fetch", remote, branch)
    ancestry = _git(
        repo, "merge-base", "--is-ancestor", commit, f"{remote}/{branch}", check=False
    )
    if ancestry.returncode != 0:
        raise PublishRefused("split commit is not an ancestor of freshly fetched main")
    return {
        "split": len(evidence),
        "commit": commit,
        "source_unchanged": True,
        "event_union_preserved": True,
        "shards": evidence,
    }


# --------------------------------------------------------------------------- publish


def publish(
    repo: str | os.PathLike[str],
    spool_dir: str | os.PathLike[str],
    *,
    remote: str = DEFAULT_REMOTE,
    branch: str = DEFAULT_BRANCH,
    attempts: int = DEFAULT_ATTEMPTS,
    fetch: bool = True,
    serialized_writer: str | os.PathLike[str] | None = None,
    attribution: str | None = None,
    source_root: str | os.PathLike[str] | None = None,
    store: str = "ledger",
) -> dict:
    """Publish series from a short locked byte snapshot, with no network lock."""
    if store != "series":
        return _publish_prepared(
            repo,
            spool_dir,
            remote=remote,
            branch=branch,
            attempts=attempts,
            fetch=fetch,
            serialized_writer=serialized_writer,
            attribution=attribution,
            source_root=source_root,
            store=store,
        )
    series_store = _series_store()
    try:
        with series_store.series_spool_transition_lock(
            spool_dir, exclusive=True,
        ) as lock:
            batch_files = _capture_series_batch_files(lock)
    except series_store.SeriesError as error:
        raise PublishRefused(f"cannot hold series transition lock: {error}") from error
    pending = _pending_from_series_batch_files(batch_files)
    return _publish_prepared(
        repo,
        spool_dir,
        remote=remote,
        branch=branch,
        attempts=attempts,
        fetch=fetch,
        serialized_writer=serialized_writer,
        attribution=attribution,
        source_root=source_root,
        store=store,
        prepared_pending=pending,
        prepared_files=batch_files,
    )


def _publish_prepared(
    repo: str | os.PathLike[str],
    spool_dir: str | os.PathLike[str],
    *,
    remote: str = DEFAULT_REMOTE,
    branch: str = DEFAULT_BRANCH,
    attempts: int = DEFAULT_ATTEMPTS,
    fetch: bool = True,
    serialized_writer: str | os.PathLike[str] | None = None,
    attribution: str | None = None,
    source_root: str | os.PathLike[str] | None = None,
    store: str = "ledger",
    prepared_pending: dict[tuple[str, str, str], list[dict]] | None = None,
    prepared_files: Sequence[_SeriesBatchFile] | None = None,
) -> dict:
    """Publish one store's captured batch with network work outside all locks.

    Returns evidence, not a boolean: the commit written, how many attempts the
    push needed, and the ancestry check that authorised draining the spool.
    """
    repo = Path(repo).resolve()
    if source_root is not None:
        source_root = Path(source_root).resolve()
        if repo == source_root:
            raise PublishRefused(
                "publisher checkout is the shared producer parent; use a dedicated clone or slot"
            )
        # WHICH REPOSITORY IS THIS? Asked before a single byte is written.
        #
        # Measured 2026-08-09: `--repo hermit` resolved, relative to the parent,
        # to the Hermit PRODUCT submodule. The publisher accepted it, detached
        # that primary checkout at ITS origin/main, and wrote five
        # `ledger: append 1 event(s)` commits into a product repository — one
        # per retry — before the serialized writer, which exists only in the
        # parent, refused for an unrelated reason. Nothing here asked the one
        # question that mattered. Identity is shared history, not a name.
        theirs = _root_commits(repo, f"{remote}/{branch}", "HEAD")
        ours = _root_commits(source_root, "HEAD", f"{remote}/{branch}")
        if not ours or not theirs or not (ours & theirs):
            raise PublishRefused(
                f"publisher checkout {repo} shares no root commit with the ledger source "
                f"{source_root}, so it is a DIFFERENT repository: publishing would commit "
                "ledger/ shards into history that does not own them, and would detach that "
                "checkout to do it. Pass a dedicated clean clone of the ledger repository "
                "(ci-hub/ledger/README.md), never a product submodule or a bare directory name."
            )
    if serialized_writer is not None:
        if remote != "origin" or branch != "main":
            raise PublishRefused("serialized parent publication requires origin/main")
        if not attribution or "\n" in attribution:
            raise PublishRefused("serialized publication requires one final attribution line")
        status = _git(repo, "status", "--porcelain")
        if status.stdout.strip():
            raise PublishRefused("publisher checkout is not clean")
    reaped = 0
    if store == "series":
        if prepared_pending is None or prepared_files is None:
            raise PublishRefused("series publication omitted its locked batch snapshot")
        pending = prepared_pending
        snapshot_files: Sequence[Path] | Sequence[_SeriesBatchFile] = prepared_files
    else:
        # Ledger retains its existing lifecycle. Only the series store shares
        # the scorecard snapshot lock.
        reaped = (
            _reap_retained_with_transition_lock_held(
                spool_dir, Path(source_root), store=store,
            )
            if source_root else 0
        )
        pending, snapshot_files = _store_snapshot(store, spool_dir)
    if not pending:
        if store == "series" and source_root is not None:
            reaped = reap_retained(
                spool_dir, Path(source_root), store=store,
            )
        return {
            "published": 0,
            "commit": None,
            "attempts": 0,
            "shards": [],
            "reaped_retained": reaped,
        }

    # Validate before touching Git. A refused batch stays spooled and visible
    # rather than being written into shared history and linted afterwards.
    for (team, host, month), events in pending.items():
        rel = _store_shard(store, team, host, month)
        _validate_store_events(store, events, rel, read=(store == "series"))

    last_error = ""
    for attempt in range(1, attempts + 1):
        if fetch:
            _git(repo, "fetch", remote, branch, check=False)
        _git(repo, "checkout", "--detach", f"{remote}/{branch}", check=False)

        written: list[str] = []
        for (team, host, month), events in sorted(pending.items()):
            rel = _store_shard(store, team, host, month)
            target = repo / rel
            # The PUBLISHED content, read from the remote ref -- never from the
            # working tree. A leftover file from a failed earlier attempt is our
            # own output, not published state; reading it made publish()
            # conclude "all events already published" and DRAIN THE SPOOL while
            # nothing had landed. Found the hard way migrating the 654-row
            # legacy ledger: attempt 1 wrote the file and then failed at commit,
            # attempt 2 saw its own leftover and reported 0 published.
            shown = _git(repo, "show", f"{remote}/{branch}:{rel}", check=False)
            remote_text = shown.stdout if shown.returncode == 0 else ""
            merged = merge_shard(
                remote_text,
                events,
                store=store,
                read_events=(store == "series"),
            )
            violations = verify_append_only(remote_text, merged)
            if violations:
                raise PublishRefused(f"{rel}: merge would violate append-only {violations}")
            if merged == remote_text:
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(merged)
            written.append(rel)

        if not written:
            if store == "series":
                published_commit = _git(
                    repo, "rev-parse", f"{remote}/{branch}", check=False,
                ).stdout.strip()
                marker_root = Path(source_root) if source_root else None
                durable_event_ids = _prepare_series_snapshot_marker(
                    marker_root,
                    published_commit,
                    remote=remote,
                    branch=branch,
                )
                retained_count, reaped, changed, marker = _finish_series_publication(
                    spool_dir,
                    prepared_files or (),
                    source_root=marker_root,
                    published_commit=published_commit,
                    durable_event_ids=durable_event_ids,
                )
            else:
                _drain(snapshot_files)
                retained_count, changed, marker = len(snapshot_files), [], None
            report = {
                "published": 0,
                "commit": None,
                "attempts": attempt,
                "shards": [],
                "note": "all events already published",
                "reaped_retained": reaped,
                "retained": retained_count,
            }
            if store == "series":
                report.update({
                    "changed_sources": changed,
                    "series_snapshot_commit": marker,
                })
            return report

        _git(repo, "add", "--", *written)
        subject = f"{store}: append {sum(len(v) for v in pending.values())} event(s)"
        message = subject if attribution is None else f"{subject}\n\n{attribution}"
        _git(repo, "commit", "-q", "-m", message, "--", *written)
        commit = _git(repo, "rev-parse", "HEAD").stdout.strip()

        if serialized_writer is None:
            pushed = _git(repo, "push", remote, f"HEAD:refs/heads/{branch}", check=False)
        else:
            pushed = bounded_capture.capture(
                [str(serialized_writer), "publish", commit], cwd=repo,
            )
        if pushed.returncode == 0:
            # Push exit status is not publication. Confirm the commit is an
            # ancestor of the freshly-fetched branch before dropping anything.
            if fetch:
                _git(repo, "fetch", remote, branch, check=False)
            ancestry = _git(repo, "merge-base", "--is-ancestor", commit,
                            f"{remote}/{branch}", check=False)
            if ancestry.returncode == 0:
                if store == "series":
                    marker_root = Path(source_root) if source_root else None
                    durable_event_ids = _prepare_series_snapshot_marker(
                        marker_root,
                        commit,
                        remote=remote,
                        branch=branch,
                    )
                    retained_count, reaped, changed, marker = _finish_series_publication(
                        spool_dir,
                        prepared_files or (),
                        source_root=marker_root,
                        published_commit=commit,
                        durable_event_ids=durable_event_ids,
                    )
                else:
                    _drain(snapshot_files)
                    retained_count, changed, marker = len(snapshot_files), [], None
                report = {
                    "published": sum(len(v) for v in pending.values()),
                    "commit": commit,
                    "attempts": attempt,
                    "shards": written,
                    "ancestry": f"{commit} is an ancestor of {remote}/{branch}",
                    "reaped_retained": reaped,
                    "retained": retained_count,
                    "transport": "parent-main-write" if serialized_writer else "git",
                }
                if store == "series":
                    report.update({
                        "changed_sources": changed,
                        "series_snapshot_commit": marker,
                    })
                return report
            last_error = "push reported success but the commit is not an ancestor of the branch"
            continue

        # Non-fast-forward: someone appended to a shard we are also appending
        # to. Loop; the next pass re-reads the remote shard and re-merges, so
        # their lines are preserved and ours are added once.
        #
        # Keep the WHOLE failure, not its last line: see `_failure_detail`.
        last_error = _failure_detail(pushed)

    raise PublishRefused(
        f"gave up after {attempts} attempts; spool retained. last error: {last_error}"
    )


def _finish_series_publication(
    spool_dir: str | os.PathLike[str],
    batch_files: Sequence[_SeriesBatchFile],
    *,
    source_root: Path | None,
    published_commit: str,
    durable_event_ids: set[str] | None,
) -> tuple[int, int, list[str], str | None]:
    """Under short EX, retain only unchanged files after publication proof."""
    if re.fullmatch(r"[0-9a-f]{40}", published_commit or "") is None:
        raise PublishRefused(
            "series batch publication has no exact published commit; spool retained"
        )
    series_store = _series_store()
    if source_root is None:
        return 0, 0, [str(item.path) for item in batch_files], None
    if durable_event_ids is None:
        raise PublishRefused(
            "series batch publication omitted its immutable committed row set; "
            "spool retained"
        )
    try:
        with series_store.series_spool_transition_lock(
            spool_dir, exclusive=True,
        ) as lock:
            marker = _publish_series_snapshot_marker_locked(
                source_root, published_commit, series_store,
            )
            retained_count = 0
            changed = []
            destination = retained_dir(lock.spool)
            for captured in batch_files:
                if not _series_batch_file_unchanged(lock, captured):
                    changed.append(str(captured.path))
                    continue
                destination.mkdir(parents=True, exist_ok=True)
                retained_path = destination / captured.path.name
                if retained_path.exists():
                    changed.append(str(captured.path))
                    continue
                # Every cooperating series writer is excluded here. A file
                # that changed during network work remains live and is not in
                # this exact batch's drain set.
                os.replace(captured.path, retained_path)
                retained_count += 1
            reaped = (
                _reap_retained_with_transition_lock_held(
                    spool_dir,
                    source_root,
                    store="series",
                    durable_event_ids=durable_event_ids,
                )
                if source_root is not None else 0
            )
            return retained_count, reaped, changed, marker
    except series_store.SeriesError as error:
        raise PublishRefused(f"cannot hold series transition lock: {error}") from error


def _drain(snapshot_files: Sequence[Path]) -> None:
    """Retain only immutable batches proven published; later batches survive.

    The safety property is unchanged and is not weakened here: this still runs
    ONLY after the commit carrying the batch is confirmed an ancestor of the
    freshly-fetched remote. What changes is that a proven batch is MOVED into
    the retention directory instead of unlinked, so the producing root keeps
    reading it until the published shard is durably present there. Reaping is
    `reap_retained`, and it is conditional on that durability.
    """
    for path in snapshot_files:
        if not path.exists():
            continue
        destination = retained_dir(path.parent)
        destination.mkdir(parents=True, exist_ok=True)
        # Same filesystem, so this is atomic: a reader sees the batch at exactly
        # one of the two paths, never neither.
        os.replace(path, destination / path.name)


def _source_root_event_ids(source_root: Path, *, store: str = "ledger") -> set[str]:
    """Event ids the PRODUCING checkout can itself read from tracked shards."""
    present: set[str] = set()
    if store not in STORES:
        raise PublishRefused(f"unknown store {store!r}; expected one of {STORES}")
    pattern = f"{store}/*/*/[0-9][0-9][0-9][0-9]-[0-9][0-9].jsonl"
    for path in sorted(Path(source_root).glob(pattern)):
        reader = read_event_shard if store == "ledger" else read_shard
        for event in reader(path):
            identifier = event.get("event_id")
            if isinstance(identifier, str) and identifier:
                present.add(identifier)
    return present


def reap_retained(
    spool_dir: str | os.PathLike[str], source_root: Path, *, store: str = "ledger"
) -> int:
    """Reap retained series rows under the same lock as publish/snapshot."""
    if store != "series":
        return _reap_retained_with_transition_lock_held(
            spool_dir, source_root, store=store,
        )
    series_store = _series_store()
    marker = _git(
        source_root,
        "rev-parse",
        "--verify",
        f"{series_store.SERIES_SNAPSHOT_REF}^{{commit}}",
        check=False,
    )
    if marker.returncode != 0:
        return 0
    marker_commit = marker.stdout.strip()
    marker_event_ids = _source_commit_event_ids(
        source_root, marker_commit, store="series",
    )
    head = _git(
        source_root, "rev-parse", "--verify", "HEAD^{commit}", check=False,
    )
    if head.returncode != 0:
        raise PublishRefused(
            "cannot bind the producing checkout HEAD before retained-row reaping"
        )
    head_event_ids = _source_commit_event_ids(
        source_root, head.stdout.strip(), store="series",
    )
    durable_event_ids = marker_event_ids & head_event_ids
    try:
        with series_store.series_spool_transition_lock(
            spool_dir, exclusive=True,
        ):
            current = _git(
                source_root,
                "rev-parse",
                "--verify",
                f"{series_store.SERIES_SNAPSHOT_REF}^{{commit}}",
                check=False,
            )
            if current.returncode != 0 or current.stdout.strip() != marker_commit:
                raise PublishRefused(
                    "series snapshot marker changed before retained-row reaping; "
                    "nothing was removed"
                )
            return _reap_retained_with_transition_lock_held(
                spool_dir,
                source_root,
                store=store,
                durable_event_ids=durable_event_ids,
            )
    except series_store.SeriesError as error:
        raise PublishRefused(f"cannot hold series transition lock: {error}") from error


def _reap_retained_with_transition_lock_held(
    spool_dir: str | os.PathLike[str],
    source_root: Path,
    *,
    store: str = "ledger",
    durable_event_ids: set[str] | None = None,
) -> int:
    """Delete retained batches now durably readable from the source root.

    This is the exact condition the retention exists for: a batch may be
    forgotten only once the producing checkout itself contains the published
    event, not merely once some other clone does. A batch whose events are not
    all present stays retained, so a source root that never syncs keeps its
    copy rather than losing visibility.
    """
    directory = retained_dir(spool_dir)
    if not directory.is_dir():
        return 0
    durable = (
        durable_event_ids
        if durable_event_ids is not None
        else _source_root_event_ids(Path(source_root), store=store)
    )
    reaped = 0
    for path in sorted(directory.glob("*.jsonl")):
        reader = read_event_shard if store == "ledger" else read_shard
        events = reader(path)
        identifiers = {
            event.get("event_id")
            for event in events
            if isinstance(event.get("event_id"), str)
        }
        if not identifiers or not identifiers <= durable:
            continue
        path.unlink(missing_ok=True)
        reaped += 1
    return reaped


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser(
        "stage-tracked", description="copy tracked shards into the spool without changing them", help="copy tracked shards into the spool without changing them"
    )
    stage.add_argument("--source-root", type=Path, required=True)
    stage.add_argument("--spool-dir", type=Path, required=True)
    send = commands.add_parser(
        "publish", description="publish a live spool from an isolated checkout", help="publish a live spool from an isolated checkout"
    )
    send.add_argument("--source-root", type=Path, required=True)
    send.add_argument("--repo", type=Path, required=True)
    send.add_argument("--spool-dir", type=Path, required=True)
    send.add_argument("--writer", type=Path, required=True)
    send.add_argument("--attribution", required=True)
    send.add_argument("--attempts", type=int, default=DEFAULT_ATTEMPTS)
    send.add_argument("--store", choices=STORES, default="ledger")
    split = commands.add_parser(
        "split-published-shard",
        description="preserve divergent remote bytes and make the primary match the source",
        help="preserve divergent remote bytes and make the primary match the source",
    )
    split.add_argument("--source-root", type=Path, required=True)
    split.add_argument("--repo", type=Path, required=True)
    split.add_argument("--writer", type=Path, required=True)
    split.add_argument("--attribution", required=True)
    split.add_argument("--shard", action="append", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "stage-tracked":
            report = stage_tracked(args.source_root, args.spool_dir)
        elif args.command == "publish":
            report = publish(
                args.repo,
                args.spool_dir,
                attempts=args.attempts,
                serialized_writer=args.writer,
                attribution=args.attribution,
                source_root=args.source_root,
                store=args.store,
            )
        else:
            report = split_published_to_match_source(
                args.repo,
                args.source_root,
                args.shard,
                serialized_writer=args.writer,
                attribution=args.attribution,
            )
        print(json.dumps(report, sort_keys=True))
        return 0
    except (OSError, subprocess.SubprocessError, PublishRefused, MalformedEvent) as error:
        print(f"ledger-publisher: REFUSED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
