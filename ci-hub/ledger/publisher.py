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
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import importlib.util
import json
import os
import stat
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Sequence

from ledger import (  # noqa: F401 - re-exported for callers
    MalformedEvent,
    SharedTypeCheckerUnavailable,
    append_events,
    parse_line,
    read_event_shard,
    read_shard,
    prepare_shared_checker,
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
#: ``series-quarantine`` publishes refused series inputs to
#: ``quarantine/series/<team>/<host>/<YYYY-MM>.jsonl``, outside the
#: ``series/`` tree every series reader globs.
STORES = ("ledger", "series", "series-quarantine")
CHECKOUT_LOCK_TIMEOUT_SECONDS = 60.0


class PublishRefused(Exception):
    """The publish would have rewritten published bytes, or the events are invalid."""


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


@contextlib.contextmanager
def _checkout_transaction(repo: Path):
    """Serialize this checkout's index/HEAD, separately from the main push lock.

    Callers prepare native tools first. The existing parent-main writer is then
    acquired only inside this transaction; neither lock spans cell publication.
    A per-worktree Git directory also makes aliases of one checkout agree.
    """
    git_dir = Path(_git(repo, "rev-parse", "--absolute-git-dir").stdout.strip()).resolve()
    path = git_dir / "ci-hub-ledger-publisher.lock"
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
    try:
        metadata = os.fstat(fd)
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                or metadata.st_nlink != 1):
            raise PublishRefused("publisher checkout lock is not an owned regular file")
        deadline = time.monotonic() + CHECKOUT_LOCK_TIMEOUT_SECONDS
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise PublishRefused("publisher checkout transaction lock timed out; spool retained")
                time.sleep(0.1)
        yield
    finally:
        os.close(fd)


def _prepare_parent_checker() -> None:
    try:
        prepare_shared_checker()
    except SharedTypeCheckerUnavailable as error:
        raise PublishRefused(f"parent publisher checker preparation failed: {error}") from error


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


def _require_source_repository(repo: Path, source_root, remote: str, branch: str) -> None:
    """Check identity before creating a lock, and again after waiting for it."""
    if source_root is None:
        return
    source_root = Path(source_root).resolve()
    if repo == source_root:
        raise PublishRefused(
            "publisher checkout is the shared producer parent; use a dedicated clone or slot"
        )
    # Identity is shared history, not a directory name. Passing the Hermit
    # product checkout once detached it and committed ledger shards there.
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
    *, object_pairs_hook=None, canonical=None,
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
        options = {} if object_pairs_hook is None else {"object_pairs_hook": object_pairs_hook}
        pending.setdefault((team, host, month), []).extend(read_shard(path, **options))
        files.append(path)
    return ({key: union_events(value, canonical=canonical) for key, value in pending.items()}, files)


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


def _quarantine_spool_snapshot(
    spool_dir: str | os.PathLike[str],
) -> tuple[dict[tuple[str, str, str], list[dict]], list[Path]]:
    """Group pending refused inputs by the quarantine shard they publish into."""
    series_store = _series_store()
    try:
        return series_store.quarantine_publication_snapshot(spool_dir)
    except series_store.LedgerError as error:
        raise PublishRefused(f"{spool_dir}: quarantine spool is invalid: {error}") from error


def _store_snapshot(
    store: str, spool_dir: str | os.PathLike[str]
) -> tuple[dict[tuple[str, str, str], list[dict]], list[Path]]:
    if store == "ledger":
        return _spool_snapshot(spool_dir)
    if store == "series":
        return _series_spool_snapshot(spool_dir)
    if store == "series-quarantine":
        return _quarantine_spool_snapshot(spool_dir)
    raise PublishRefused(f"unknown store {store!r}; expected one of {STORES}")


def _store_shard(store: str, team: str, host: str, month: str) -> str:
    if store == "ledger":
        return f"ledger/{team}/{host}/{month}.jsonl"
    if store == "series":
        return _series_store().shard_path(team, host, month)
    if store == "series-quarantine":
        return _series_store().quarantine_shard_path(team, host, month)
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
    if store == "series-quarantine":
        series_store = _series_store()
        try:
            series_store.validate_quarantine_records(events, public=True)
        except series_store.SeriesError as error:
            raise PublishRefused(
                f"{relative}: quarantine batch is invalid: {error}"
            ) from error
        return
    raise PublishRefused(f"unknown store {store!r}; expected one of {STORES}")


def _published_quarantine_ids(repo: Path, ref: str) -> set[str]:
    """Identities of every refusal already published at ``ref``, in any shard."""
    series_store = _series_store()
    try:
        return {
            record["event_id"]
            for record in series_store.published_quarantine_at_repo(repo, ref)[1]
        }
    except series_store.LedgerError as error:
        raise PublishRefused(f"cannot read published quarantine at {ref}: {error}") from error


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


def retained(spool_dir: str | os.PathLike[str], *, object_pairs_hook=None,
             canonical=None) -> dict[tuple[str, str, str], list[dict]]:
    """Published-but-not-yet-durable batches, keyed like ``spooled``."""
    return _spool_snapshot(retained_dir(spool_dir), object_pairs_hook=object_pairs_hook,
                           canonical=canonical)[0]


def spooled(spool_dir: str | os.PathLike[str], *, object_pairs_hook=None,
            canonical=None) -> dict[tuple[str, str, str], list[dict]]:
    """Every PENDING event, keyed by (team, host, month).

    Pending means not yet published. Retained batches are excluded here on
    purpose: this is the publisher's view, and a retained batch must never be
    republished. The READER's view is `validate_rows.events`, which unions this
    with `retained` so a producer keeps seeing its own receipt.
    """
    return _spool_snapshot(spool_dir, object_pairs_hook=object_pairs_hook,
                           canonical=canonical)[0]


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


def _split_published_to_match_source(
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
    """Keep migration and ordinary publication on the same checkout lock."""
    repo = Path(repo).resolve()
    if repo == Path(source_root).resolve():
        raise PublishRefused("split checkout is the shared producer parent; use a dedicated clone")
    _prepare_parent_checker()
    with _checkout_transaction(repo):
        return _split_published_to_match_source(
            repo, source_root, shards, remote=remote, branch=branch,
            serialized_writer=serialized_writer, attribution=attribution,
        )


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
    network_wrapper: str | None = None,
) -> dict:
    """Publish with exclusive parent-checkout mutation and unchanged push guards.

    Series publication already holds its own cell-ledger writer lock. Parent
    publication takes this checkout lock before the machine-wide main writer,
    and releases both before its caller can acquire cell-ledger authority.
    """
    repo = Path(repo).resolve()
    _require_source_repository(repo, source_root, remote, branch)
    if store == "ledger":
        _prepare_parent_checker()
    transaction = _checkout_transaction(repo) if store == "ledger" else contextlib.nullcontext()
    with transaction:
        return _publish(
            repo, spool_dir, remote=remote, branch=branch, attempts=attempts,
            fetch=fetch, serialized_writer=serialized_writer, attribution=attribution,
            source_root=source_root, store=store, network_wrapper=network_wrapper,
        )


def _publish(
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
    network_wrapper: str | None = None,
) -> dict:
    """Publish one store's spool; drain only once ancestry proves it.

    Returns evidence, not a boolean: the commit written, how many attempts the
    push needed, and the ancestry check that authorised draining the spool.
    """
    repo = Path(repo).resolve()

    def network(*args: str, check: bool = True):
        if network_wrapper is None:
            return _git(repo, *args, check=check)
        return bounded_capture.capture(
            [network_wrapper, "git", *args], cwd=repo, check=check,
        )
    _require_source_repository(repo, source_root, remote, branch)
    if serialized_writer is not None:
        if remote != "origin" or branch != "main":
            raise PublishRefused("serialized parent publication requires origin/main")
        if not attribution or "\n" in attribution:
            raise PublishRefused("serialized publication requires one final attribution line")
        status = _git(repo, "status", "--porcelain")
        if status.stdout.strip():
            raise PublishRefused("publisher checkout is not clean")
    # Reap first: batches the PRODUCING root can now read for itself are done
    # being retained. Doing it here means every publish run also collects what
    # the previous run left behind, with no separate schedule to forget.
    reaped = reap_retained(spool_dir, Path(source_root), store=store) if source_root else 0

    pending, snapshot_files = _store_snapshot(store, spool_dir)
    if not pending:
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
        try:
            if fetch:
                network("fetch", remote, branch)
            _git(repo, "checkout", "--detach", f"{remote}/{branch}")
        except subprocess.CalledProcessError as error:
            raise PublishRefused(
                f"cannot prepare published branch; spool retained: {_failure_detail(error)}"
            ) from error

        attempt_pending = pending
        already_published = 0
        if store == "series-quarantine":
            # A refusal's identity excludes when it was observed, so a replayed
            # run can carry an identity already published (possibly in another
            # month's shard) with a later emitted_at. The first published body
            # stands; appending the replay would put one identity in two shards.
            published_ids = _published_quarantine_ids(repo, f"{remote}/{branch}")
            attempt_pending = {}
            for key, events in pending.items():
                fresh = [event for event in events if event["event_id"] not in published_ids]
                already_published += len(events) - len(fresh)
                if fresh:
                    attempt_pending[key] = fresh

        written: list[str] = []
        for (team, host, month), events in sorted(attempt_pending.items()):
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
            _drain(snapshot_files)
            return {"published": 0, "commit": None, "attempts": attempt,
                    "shards": [], "note": "all events already published",
                    "already_published": already_published,
                    "reaped_retained": reaped}

        _git(repo, "add", "--", *written)
        subject = f"{store}: append {sum(len(v) for v in attempt_pending.values())} event(s)"
        message = subject if attribution is None else f"{subject}\n\n{attribution}"
        _git(repo, "commit", "-q", "-m", message, "--", *written)
        commit = _git(repo, "rev-parse", "HEAD").stdout.strip()

        if serialized_writer is None:
            pushed = network("push", remote, f"HEAD:refs/heads/{branch}", check=False)
        else:
            pushed = bounded_capture.capture(
                [str(serialized_writer), "publish", commit], cwd=repo,
            )
        if pushed.returncode == 0:
            # Push exit status is not publication. Confirm the commit is an
            # ancestor of the freshly-fetched branch before dropping anything.
            if fetch:
                try:
                    network("fetch", remote, branch)
                except subprocess.CalledProcessError as error:
                    raise PublishRefused(
                        f"cannot confirm publication; spool retained: {_failure_detail(error)}"
                    ) from error
            ancestry = _git(repo, "merge-base", "--is-ancestor", commit,
                            f"{remote}/{branch}", check=False)
            if ancestry.returncode == 0:
                _drain(snapshot_files)
                return {"published": sum(len(v) for v in attempt_pending.values()),
                        "already_published": already_published,
                        "commit": commit, "attempts": attempt, "shards": written,
                        "ancestry": f"{commit} is an ancestor of {remote}/{branch}",
                        "reaped_retained": reaped,
                        "retained": len(snapshot_files),
                        "transport": "parent-main-write" if serialized_writer else "git"}
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
    if store == "series-quarantine":
        return _reap_retained_quarantine(Path(spool_dir), Path(source_root))
    if store == "series":
        # The pinned submodule worktree may lag its fetched history. Only full
        # matching bodies at a named published commit justify dropping a copy.
        module = _series_store()
        published = module._published_snapshot_at_repo(
            Path(source_root), "refs/remotes/origin/main",
            repository=module.LEDGER_REPOSITORY,
        )
        durable_rows = {row["event_id"]: row for row in published.rows}
    else:
        durable = _source_root_event_ids(Path(source_root), store=store)
    reaped = 0
    for path in sorted(directory.glob("*.jsonl")):
        reader = read_event_shard if store == "ledger" else read_shard
        events = reader(path)
        if store == "series":
            if events and all(durable_rows.get(event.get("event_id")) == event for event in events):
                path.unlink()
                reaped += 1
            continue
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


def _reap_retained_quarantine(spool_dir: Path, source_root: Path) -> int:
    """Forget a published quarantine file only once the source root reads it.

    A file is done when every record's identity is published at the source
    root's ``refs/remotes/origin/main``. A file holding any record whose
    public form was withheld (an owner path that cannot be replaced
    reversibly) is MOVED into ``held/`` instead of deleted: the published
    stub proves the refusal happened but carries none of its bytes, so this
    host keeps the only exact copy, and ``ledger_spool_backlog --store
    series-quarantine`` alarms while ``held/`` is not empty.
    """
    series_store = _series_store()
    directory = retained_dir(spool_dir)
    try:
        published = _published_quarantine_ids(source_root, "refs/remotes/origin/main")
        host = series_store._writer_host()
    except series_store.SeriesError as error:
        raise PublishRefused(f"cannot reap retained quarantine: {error}") from error
    held = spool_dir / series_store.QUARANTINE_HELD_SUBDIR
    reaped = 0
    for path in sorted(directory.glob("*.jsonl")):
        try:
            records = series_store._read_quarantine_file(path)
            forms = [series_store._published_form(record, host=host) for record in records]
        except series_store.LedgerError as error:
            raise PublishRefused(f"{path}: retained quarantine is invalid: {error}") from error
        if not forms or not all(form["event_id"] in published for form in forms):
            continue
        if any(form["withheld"] is not None for form in forms):
            held.mkdir(parents=True, exist_ok=True)
            target = held / path.name
            present = series_store._local_quarantine_ids(target)
            lines = [
                json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
                for record, form in zip(records, forms)
                if form["withheld"] is not None and form["event_id"] not in present
            ]
            if lines:
                with open(target, "a", encoding="ascii") as handle:
                    handle.write("".join(lines))
                    handle.flush()
                    os.fsync(handle.fileno())
        path.unlink()
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
    send.add_argument("--writer", type=Path)
    send.add_argument("--attribution")
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
            if args.store == "ledger" and (args.writer is None or not args.attribution):
                raise PublishRefused("parent validation publication requires --writer and --attribution")
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
