#!/usr/bin/env python3
"""Publish ledger events to the shared Git-backed history.

Three properties, in the order they matter:

**A local event is never lost.** Every event is written to a local spool
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
import json
import os
import subprocess
import sys
import uuid
from pathlib import Path
from typing import Sequence

from ledger import (  # noqa: F401 - re-exported for callers
    MalformedEvent,
    append_events,
    parse_line,
    read_shard,
    shard_path,
    union_events,
    validate_event,
    verify_append_only,
)

DEFAULT_REMOTE = "origin"
DEFAULT_BRANCH = "main"
#: Bounded so a genuinely wedged remote surfaces as a failure with the spool
#: intact, instead of spinning forever and looking healthy.
DEFAULT_ATTEMPTS = 5


class PublishRefused(Exception):
    """The publish would have rewritten published bytes, or the events are invalid."""


def _git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=check
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


def spooled(spool_dir: str | os.PathLike[str]) -> dict[tuple[str, str, str], list[dict]]:
    """Every pending event, keyed by (team, host, month)."""
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
        events = read_shard(path)
        if not events:
            continue
        target = spool(spool_dir, team, host, events)
        batches.append(str(target))
        staged += len(events)
    return {"staged": staged, "batches": batches, "source_unchanged": True}


# --------------------------------------------------------------------------- merge


def merge_shard(remote_text: str, events: Sequence[dict]) -> str:
    """Append-aware merge: published lines stay put, new events go after them.

    Returns the full shard text. An event already present remotely is not
    re-appended, which is what makes a retry idempotent — the same batch pushed
    twice yields one copy, not two.
    """
    remote_lines = [ln for ln in remote_text.splitlines() if ln.strip()]
    if remote_text and not remote_text.endswith("\n"):
        raise MalformedEvent("remote shard's final line is truncated; refusing to append")
    have = {ln for ln in remote_lines}
    have_ids = set()
    for line in remote_lines:
        have_ids.add(parse_line(line).get("event_id"))
    additions = []
    for event in events:
        line = json.dumps(event, sort_keys=True)
        if line in have or event.get("event_id") in have_ids:
            continue
        have.add(line)
        have_ids.add(event.get("event_id"))
        additions.append(line)
    body = remote_lines + additions
    return "".join(line + "\n" for line in body)


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
) -> dict:
    """Publish everything spooled; drop spool entries only once ancestry proves it.

    Returns evidence, not a boolean: the commit written, how many attempts the
    push needed, and the ancestry check that authorised draining the spool.
    """
    repo = Path(repo).resolve()
    if source_root is not None and repo == Path(source_root).resolve():
        raise PublishRefused(
            "publisher checkout is the shared producer parent; use a dedicated clone or slot"
        )
    if serialized_writer is not None:
        if remote != "origin" or branch != "main":
            raise PublishRefused("serialized parent publication requires origin/main")
        if not attribution or "\n" in attribution:
            raise PublishRefused("serialized publication requires one final attribution line")
        status = _git(repo, "status", "--porcelain")
        if status.stdout.strip():
            raise PublishRefused("publisher checkout is not clean")
    pending, snapshot_files = _spool_snapshot(spool_dir)
    if not pending:
        return {"published": 0, "commit": None, "attempts": 0, "shards": []}

    # Validate before touching Git. A refused batch stays spooled and visible
    # rather than being written into shared history and linted afterwards.
    for (team, host, month), events in pending.items():
        rel = f"ledger/{team}/{host}/{month}.jsonl"
        for event in events:
            codes = validate_event(event, rel)
            if codes:
                raise PublishRefused(f"{rel}: event {event.get('event_id')!r} violates {codes}")

    last_error = ""
    for attempt in range(1, attempts + 1):
        if fetch:
            _git(repo, "fetch", remote, branch, check=False)
        _git(repo, "checkout", "--detach", f"{remote}/{branch}", check=False)

        written: list[str] = []
        for (team, host, month), events in sorted(pending.items()):
            rel = f"ledger/{team}/{host}/{month}.jsonl"
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
            merged = merge_shard(remote_text, events)
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
                    "shards": [], "note": "all events already published"}

        _git(repo, "add", "--", *written)
        subject = f"ledger: append {sum(len(v) for v in pending.values())} event(s)"
        message = subject if attribution is None else f"{subject}\n\n{attribution}"
        _git(repo, "-c", "user.name=ledger-publisher",
             "-c", "user.email=ledger-publisher@invalid",
             "commit", "-q", "-m", message, "--", *written)
        commit = _git(repo, "rev-parse", "HEAD").stdout.strip()

        if serialized_writer is None:
            pushed = _git(repo, "push", remote, f"HEAD:refs/heads/{branch}", check=False)
        else:
            pushed = subprocess.run(
                [str(serialized_writer), "publish", commit], cwd=repo,
                capture_output=True, text=True, check=False,
            )
        if pushed.returncode == 0:
            # Push exit status is not publication. Confirm the commit is an
            # ancestor of the freshly-fetched branch before dropping anything.
            if fetch:
                _git(repo, "fetch", remote, branch, check=False)
            ancestry = _git(repo, "merge-base", "--is-ancestor", commit,
                            f"{remote}/{branch}", check=False)
            if ancestry.returncode == 0:
                _drain(snapshot_files)
                return {"published": sum(len(v) for v in pending.values()),
                        "commit": commit, "attempts": attempt, "shards": written,
                        "ancestry": f"{commit} is an ancestor of {remote}/{branch}",
                        "transport": "parent-main-write" if serialized_writer else "git"}
            last_error = "push reported success but the commit is not an ancestor of the branch"
            continue

        # Non-fast-forward: someone appended to a shard we are also appending
        # to. Loop; the next pass re-reads the remote shard and re-merges, so
        # their lines are preserved and ours are added once.
        last_error = pushed.stderr.strip().splitlines()[-1] if pushed.stderr.strip() else "push failed"

    raise PublishRefused(
        f"gave up after {attempts} attempts; spool retained. last error: {last_error}"
    )


def _drain(snapshot_files: Sequence[Path]) -> None:
    """Remove only immutable batches proven published; later batches survive."""
    for path in snapshot_files:
        path.unlink(missing_ok=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser(
        "stage-tracked", help="copy tracked shards into the spool without changing them"
    )
    stage.add_argument("--source-root", type=Path, required=True)
    stage.add_argument("--spool-dir", type=Path, required=True)
    send = commands.add_parser(
        "publish", help="publish a live spool from an isolated checkout"
    )
    send.add_argument("--source-root", type=Path, required=True)
    send.add_argument("--repo", type=Path, required=True)
    send.add_argument("--spool-dir", type=Path, required=True)
    send.add_argument("--writer", type=Path, required=True)
    send.add_argument("--attribution", required=True)
    send.add_argument("--attempts", type=int, default=DEFAULT_ATTEMPTS)
    args = parser.parse_args(argv)
    try:
        if args.command == "stage-tracked":
            report = stage_tracked(args.source_root, args.spool_dir)
        else:
            report = publish(
                args.repo,
                args.spool_dir,
                attempts=args.attempts,
                serialized_writer=args.writer,
                attribution=args.attribution,
                source_root=args.source_root,
            )
        print(json.dumps(report, sort_keys=True))
        return 0
    except (OSError, subprocess.SubprocessError, PublishRefused, MalformedEvent) as error:
        print(f"ledger-publisher: REFUSED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
