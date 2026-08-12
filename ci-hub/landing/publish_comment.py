#!/usr/bin/env python3
"""Publish a landing outcome comment without losing a failed write.

GitHub is the human-visible record.  The registered coordination-comment
wrapper supplies the required disclosure and restricts the destination.  If
that write fails, this command writes one local JSON record under the landing
log directory and prints its exact path to stderr.
"""

from __future__ import annotations

import argparse
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
COMMENT_WRAPPER = ROOT / ".orc/plugins/hermit-dev/gh-coord-comment"
DEFAULT_RECORD_DIR = ROOT / "ignored/ci-hub/landing"
KINDS = ("abandoned", "no-result", "post-land-arm-failed")

Runner = Callable[..., subprocess.CompletedProcess[str]]


@dataclass(frozen=True)
class Request:
    repo: str
    pr: int
    agent: str
    team: str
    kind: str
    reason: str
    record_dir: Path


def _positive_int(raw: str) -> int:
    try:
        value = int(raw)
    except ValueError as error:
        raise argparse.ArgumentTypeError("PR must be a positive integer") from error
    if value <= 0:
        raise argparse.ArgumentTypeError("PR must be a positive integer")
    return value


def _nonempty(raw: str) -> str:
    if not raw.strip():
        raise argparse.ArgumentTypeError("value must not be empty")
    return raw


def _repo(raw: str) -> str:
    if not raw.startswith("rrnewton/") or raw.count("/") != 1:
        raise argparse.ArgumentTypeError("repository must be one rrnewton/* fork")
    owner, name = raw.split("/", 1)
    if owner != "rrnewton" or not name or any(char.isspace() for char in name):
        raise argparse.ArgumentTypeError("repository must be one rrnewton/* fork")
    return raw


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description=(
            "Post one landing outcome through the registered GitHub comment "
            "boundary; preserve a local JSON record when publication fails."
        )
    )
    result.add_argument("--repo", type=_repo, required=True)
    result.add_argument("--pr", type=_positive_int, required=True)
    result.add_argument("--agent", type=_nonempty, required=True)
    result.add_argument("--team", type=_nonempty, required=True)
    result.add_argument("--kind", choices=KINDS, required=True)
    result.add_argument("--reason", type=_nonempty, required=True)
    result.add_argument("--record-dir", type=Path, default=DEFAULT_RECORD_DIR)
    return result


def comment_body(kind: str, reason: str) -> str:
    if kind == "abandoned":
        return (
            f"ABANDONED landing attempt: {reason}. The lock was released so the "
            "FIFO can continue. Retry if the PR remains open; durable recovery "
            "will arm exact-SHA verification if GitHub completed the merge."
        )
    if kind == "no-result":
        return (
            f"NO-RESULT landing pause: {reason}. No failing verdict was recorded; "
            "the missing check was re-dispatched and this PR remains blocked until "
            "it produces PASSED or FAILED."
        )
    if kind == "post-land-arm-failed":
        return (
            "LANDED, but immediate exact-SHA verification arming failed after the "
            "landing lock released. The durable prepared intent remains open and "
            "the ORC recovery watcher will retry; this landing is not being "
            f"reported complete. Detail: {reason}."
        )
    raise ValueError(f"unsupported landing comment kind: {kind}")


def _command(request: Request, body: str) -> list[str]:
    return [
        str(COMMENT_WRAPPER),
        "--who",
        request.agent,
        "--team",
        request.team,
        "--role",
        "reviewer",
        "--repo",
        request.repo,
        "--pr",
        str(request.pr),
        "--body",
        body,
    ]


def _failure_detail(process: subprocess.CompletedProcess[str]) -> str:
    parts = [part.strip() for part in (process.stderr, process.stdout) if part.strip()]
    detail = "\n".join(parts) or "comment command returned no diagnostic output"
    return detail[:16_000]


def _write_failure_record(
    request: Request,
    *,
    body: str,
    process: subprocess.CompletedProcess[str],
    now: datetime,
    pid: int,
) -> Path:
    request.record_dir.mkdir(parents=True, exist_ok=True)
    utc = now.astimezone(timezone.utc)
    stamp = utc.strftime("%Y%m%dT%H%M%S.%fZ")
    record = request.record_dir / (
        f"land-pr{request.pr}-{stamp}-{pid}-{request.kind}-comment-failed.json"
    )
    temporary = record.with_suffix(record.suffix + ".tmp")
    payload = {
        "schema": 1,
        "recorded_at": utc.isoformat().replace("+00:00", "Z"),
        "repository": request.repo,
        "pr": request.pr,
        "kind": request.kind,
        "reason": request.reason,
        "body": body,
        "agent": request.agent,
        "team": request.team,
        "role": "reviewer",
        "comment_exit_code": process.returncode,
        "comment_error": _failure_detail(process),
    }
    try:
        with temporary.open("x", encoding="utf-8") as handle:
            json.dump(payload, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, record)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise
    return record


def publish(
    request: Request,
    *,
    runner: Runner = subprocess.run,
    now: datetime | None = None,
    pid: int | None = None,
) -> int:
    body = comment_body(request.kind, request.reason)
    process = runner(_command(request, body), text=True, capture_output=True)
    if process.returncode == 0:
        print(
            f"LANDING COMMENT POSTED kind={request.kind} "
            f"repo={request.repo} pr={request.pr}"
        )
        return 0

    detail = _failure_detail(process)
    try:
        record = _write_failure_record(
            request,
            body=body,
            process=process,
            now=now or datetime.now(timezone.utc),
            pid=pid if pid is not None else os.getpid(),
        )
    except OSError as error:
        print(
            "LANDING COMMENT PUBLICATION FAILED "
            f"kind={request.kind} repo={request.repo} pr={request.pr} "
            f"rc={process.returncode}; could not write the local failure record: "
            f"{error}; comment error: {detail}",
            file=sys.stderr,
        )
        return 2

    print(
        "LANDING COMMENT PUBLICATION FAILED "
        f"kind={request.kind} repo={request.repo} pr={request.pr} "
        f"rc={process.returncode}; durable_record={record}; comment error: {detail}",
        file=sys.stderr,
    )
    return 1


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    return publish(
        Request(
            repo=args.repo,
            pr=args.pr,
            agent=args.agent,
            team=args.team,
            kind=args.kind,
            reason=args.reason,
            record_dir=args.record_dir,
        )
    )


if __name__ == "__main__":
    raise SystemExit(main())
