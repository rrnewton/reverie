#!/usr/bin/env python3
"""Periodically validate the owner's canonical Hermit checkout, fail closed.

This is deliberately not a worktree/clone monitor.  It exercises the same
``~/work/dev-hermit/hermit`` checkout the owner spot-checks, through the
canonical detached ``validate-run`` producer and its literal ``make validate``
entrypoint.  Every invocation writes a durable result before notification.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from typing import Any, Sequence


PASS = 0
FAIL = 1
NO_RESULT = 2
CANNOT_START = 3
RESTORE_FAILED = 4
NOTIFY_FAILED = 5


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def run(
    argv: Sequence[str | os.PathLike[str]],
    *,
    cwd: Path,
    check: bool = False,
    stdout: Any = subprocess.PIPE,
    stderr: Any = subprocess.PIPE,
) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(item) for item in argv],
        cwd=cwd,
        check=check,
        stdout=stdout,
        stderr=stderr,
        text=stdout == subprocess.PIPE,
    )


def git(checkout: Path, *args: str, check: bool = True) -> str:
    completed = run(["git", *args], cwd=checkout)
    if check and completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()
        raise RuntimeError(f"git {' '.join(args)} failed: {detail}")
    return completed.stdout or ""


def dirty_paths(checkout: Path) -> list[str]:
    raw = git(checkout, "status", "--porcelain=v1", "--untracked-files=all")
    return [line for line in raw.splitlines() if line]


def tree_state(checkout: Path) -> dict[str, Any]:
    branch = git(checkout, "branch", "--show-current").strip()
    head = git(checkout, "rev-parse", "HEAD").strip()
    tree = git(checkout, "rev-parse", "HEAD^{tree}").strip()
    status = git(checkout, "status", "--porcelain=v1", "--untracked-files=all")
    submodules = git(checkout, "submodule", "status", "--recursive", check=False)
    tracked_diff = git(checkout, "diff", "--binary", "HEAD", "--")
    material = json.dumps(
        {
            "branch": branch,
            "head": head,
            "tree": tree,
            "status": status,
            "submodules": submodules,
            "tracked_diff": tracked_diff,
        },
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    return {
        "branch": branch,
        "head": head,
        "tree": tree,
        "dirty_paths": [line for line in status.splitlines() if line],
        "submodules": submodules.splitlines(),
        "whole_tree_hash": hashlib.sha256(material).hexdigest(),
    }


def atomic_write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    with temporary.open("w", encoding="utf-8") as output:
        output.write(text)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def append_jsonl(path: Path, record: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as output:
        fcntl.flock(output.fileno(), fcntl.LOCK_EX)
        output.write(json.dumps(record, sort_keys=True) + "\n")
        output.flush()
        os.fsync(output.fileno())


def parse_time(value: Any) -> dt.datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(dt.timezone.utc)


def latest_attempt_row(
    ledger: Path,
    *,
    checkout: Path,
    sha: str,
    launched_at: dt.datetime,
) -> dict[str, Any] | None:
    if not ledger.is_file():
        return None
    rows: list[dict[str, Any]] = []
    with ledger.open(encoding="utf-8", errors="replace") as source:
        fcntl.flock(source.fileno(), fcntl.LOCK_SH)
        for line in source:
            try:
                row = json.loads(line)
            except (ValueError, TypeError):
                continue
            if not isinstance(row, dict) or row.get("commit") != sha:
                continue
            cwd = row.get("cwd")
            if not isinstance(cwd, str) or Path(cwd).resolve() != checkout.resolve():
                continue
            started = parse_time(row.get("started_at"))
            if started is None or started < launched_at - dt.timedelta(seconds=2):
                continue
            rows.append(row)
    if not rows:
        return None
    return max(
        rows,
        key=lambda row: (
            parse_time(row.get("finished_at")) or dt.datetime.min.replace(tzinfo=dt.timezone.utc),
            str(row.get("log_file") or ""),
        ),
    )


def qualify_row(parent: Path, checkout: Path, sha: str, row: dict[str, Any]) -> tuple[bool, str]:
    sys.path.insert(0, str(parent / "ci-hub"))
    import qualifying_receipt  # type: ignore

    predicate = qualifying_receipt.load()
    return qualifying_receipt.authoritative_row_qualification(
        row,
        sha,
        predicate,
        repositories=[checkout, parent],
    )


def preserve_and_restore(
    checkout: Path,
    pre: dict[str, Any],
    recovery: Path,
) -> tuple[bool, dict[str, Any]]:
    """Recover any post-run dirt, preserving bytes before touching them."""
    post = tree_state(checkout)
    if post["whole_tree_hash"] == pre["whole_tree_hash"]:
        return True, post

    recovery.mkdir(parents=True, exist_ok=False)
    atomic_write(recovery / "pre.json", json.dumps(pre, indent=2, sort_keys=True) + "\n")
    atomic_write(recovery / "post.json", json.dumps(post, indent=2, sort_keys=True) + "\n")
    atomic_write(recovery / "tracked.patch", git(checkout, "diff", "--binary", "HEAD", "--"))

    # Moving HEAD is not a validation side effect and may be somebody else's
    # concurrent work. Preserve evidence and refuse to rewrite history.
    if post["head"] != pre["head"] or post["branch"] != pre["branch"]:
        return False, post

    untracked_raw = git(
        checkout, "ls-files", "--others", "--exclude-standard", "-z"
    )
    for relative in [item for item in untracked_raw.split("\0") if item]:
        source = checkout / relative
        destination = recovery / "untracked" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(source), str(destination))

    changed_raw = git(checkout, "diff", "--name-only", "-z", "HEAD", "--")
    changed = [item for item in changed_raw.split("\0") if item]
    if changed:
        completed = run(
            ["git", "restore", "--source=HEAD", "--staged", "--worktree", "--", *changed],
            cwd=checkout,
        )
        if completed.returncode != 0:
            atomic_write(
                recovery / "restore-error.txt",
                (completed.stderr or completed.stdout or "git restore failed") + "\n",
            )

    restored = tree_state(checkout)
    return restored["whole_tree_hash"] == pre["whole_tree_hash"], restored


def render_message(result: dict[str, Any]) -> str:
    lines = [
        "OWNER CHECKOUT PERIODIC VALIDATE: " + str(result["state"]),
        f"checkout: {result['checkout']}",
        f"sha: {result.get('sha') or 'unresolved'}",
        f"attempt: {result['attempt_id']}",
        f"reason: {result.get('reason') or 'none'}",
        f"executed_tests: {result.get('executed_tests')!r}",
        f"pre_tree_hash: {result.get('pre_tree_hash') or 'unavailable'}",
        f"post_tree_hash: {result.get('post_tree_hash') or 'unavailable'}",
        f"dirty_paths_after: {result.get('dirty_paths_after')!r}",
        f"validate_log: {result.get('validate_log') or 'none'}",
        f"result_json: {result['result_json']}",
    ]
    if result.get("recovery"):
        lines.append(f"recovery: {result['recovery']}")
    return "\n".join(lines) + "\n"


def notify(parent: Path, message_file: Path) -> tuple[int, str]:
    completed = run(
        [
            sys.executable,
            parent / "scripts/orc-hermit-msg.py",
            "--transport",
            "auto",
            "--message-file",
            message_file,
        ],
        cwd=parent,
    )
    detail = ((completed.stdout or "") + (completed.stderr or "")).strip()
    return completed.returncode, detail[-2000:]


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument(
        "--checkout",
        type=Path,
        default=Path.home() / "work/dev-hermit/hermit",
    )
    result.add_argument(
        "--state-dir",
        type=Path,
        default=Path.home() / ".local/state/hermit-owner-validate",
    )
    result.add_argument("--no-fetch", action="store_true", help=argparse.SUPPRESS)
    result.add_argument("--no-notify", action="store_true", help=argparse.SUPPRESS)
    result.add_argument("--dry-run", action="store_true")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    checkout = args.checkout.resolve()
    parent = checkout.parent.resolve()
    state_dir = args.state_dir.resolve()
    # The timer and a manual spot check can begin in the same second. Include
    # microseconds and PID so their start/failure records can never overwrite
    # one another before the nonblocking singleton lock decides the winner.
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    attempt_id = f"{timestamp}-{os.getpid()}"
    result_path = state_dir / "results" / f"{attempt_id}.json"
    message_path = state_dir / "results" / f"{attempt_id}.md"
    invocation_ledger = state_dir / "invocations.jsonl"
    result: dict[str, Any] = {
        "schema_version": 1,
        "attempt_id": attempt_id,
        "started_at": utc_now(),
        "checkout": str(checkout),
        "result_json": str(result_path),
        "state": "CANNOT_START",
        "reason": "initializing",
        "executed_tests": None,
        "dirty_paths_after": None,
    }
    append_jsonl(invocation_ledger, {**result, "event": "attempt-start"})

    lock_path = state_dir / "watch.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    lock = lock_path.open("w")

    def conclude() -> int:
        lock.close()
        return finish(
            parent,
            state_dir,
            result_path,
            message_path,
            invocation_ledger,
            result,
            args,
        )

    try:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        result["reason"] = "previous periodic owner-checkout validation is still active"
        return conclude()

    launched = False
    pre: dict[str, Any] | None = None
    try:
        pre = tree_state(checkout)
        result.update(
            sha=pre["head"],
            pre_tree_hash=pre["whole_tree_hash"],
            dirty_paths_before=pre["dirty_paths"],
        )
        if pre["branch"] != "main":
            result["reason"] = f"owner checkout branch is {pre['branch']!r}, expected 'main'"
            return conclude()
        if pre["dirty_paths"]:
            result["reason"] = "owner checkout is dirty before launch"
            result["dirty_paths_after"] = pre["dirty_paths"]
            result["post_tree_hash"] = pre["whole_tree_hash"]
            return conclude()

        if not args.no_fetch:
            fetched = run(["with-proxy", "git", "fetch", "origin", "main"], cwd=checkout)
            if fetched.returncode != 0:
                result["reason"] = "origin/main fetch failed: " + (
                    fetched.stderr or fetched.stdout or "unknown error"
                ).strip()
                return conclude()
        origin = git(checkout, "rev-parse", "origin/main").strip()
        if origin != pre["head"]:
            result["reason"] = f"owner checkout is stale: HEAD={pre['head']} origin/main={origin}"
            return conclude()

        if args.dry_run:
            result.update(state="NO_RESULT", reason="dry-run: launch deliberately skipped")
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = []
            return conclude()

        launched_at = dt.datetime.now(dt.timezone.utc)
        validate_log = state_dir / "logs" / f"{attempt_id}-validate-run.log"
        validate_log.parent.mkdir(parents=True, exist_ok=True)
        command = [
            parent / "ci-hub/ci-hub",
            "validate-run",
            "--checkout",
            checkout,
            "--in-place",
            "--make-validate",
            "--agent",
            "owner-checkout-watch",
            "--target",
            pre["head"],
            "--unit",
            f"validate-owner-checkout-{attempt_id.lower()}",
            "--log",
            validate_log,
            "--wait",
            "1",
            "--child-deadline",
            "1800",
            "--caller-poll-seconds",
            "5",
        ]
        launched = True
        with validate_log.open("w", encoding="utf-8") as output:
            completed = run(command, cwd=parent, stdout=output, stderr=subprocess.STDOUT)
        result["validate_run_rc"] = completed.returncode

        row = latest_attempt_row(
            parent / "ignored/validate-run-ledger.jsonl",
            checkout=checkout,
            sha=pre["head"],
            launched_at=launched_at,
        )
        result["ledger_row_found"] = row is not None
        if row is not None:
            result["executed_tests"] = row.get("executed_tests")
            result["coverage"] = row.get("coverage")
            result["validate_log"] = row.get("log_file") or str(validate_log)
            result["ledger_result"] = row.get("result")
            qualified, reason = qualify_row(parent, checkout, pre["head"], row)
        else:
            qualified, reason = False, "no exact-attempt ledger row"
            result["validate_log"] = str(validate_log)

        if completed.returncode != 0:
            result.update(
                state="FAIL",
                reason=f"make validate exit={completed.returncode}; {reason}",
            )
        elif qualified:
            result.update(state="PASS", reason="authoritative counted full receipt qualifies")
        else:
            result.update(state="NO_RESULT", reason=reason)

    except Exception as error:  # fail closed; the result below is the alarm
        result.update(state="CANNOT_START", reason=f"watch internal error: {error}")
    finally:
        # Once the producer has been attempted, cleanup is unconditional: an
        # exception while reading its ledger is not permission to leave the
        # owner's checkout dirty.
        if launched and pre is not None:
            recovery = state_dir / "recovery" / attempt_id
            try:
                restored, post = preserve_and_restore(checkout, pre, recovery)
                result["post_tree_hash"] = post["whole_tree_hash"]
                result["dirty_paths_after"] = post["dirty_paths"]
                if recovery.exists():
                    result["recovery"] = str(recovery)
                if not restored or post["whole_tree_hash"] != pre["whole_tree_hash"]:
                    result.update(
                        state="RESTORE_FAILED",
                        reason="post-run tree differs; recovery retained",
                    )
            except Exception as restore_error:
                result.update(
                    state="RESTORE_FAILED",
                    reason=f"post-run restoration raised: {restore_error}",
                )
        elif pre is not None and "post_tree_hash" not in result:
            # Preflight refusals never mutate the checkout; still carry both
            # sides of the identity bracket in their durable result.
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = pre["dirty_paths"]

    return conclude()


def finish(
    parent: Path,
    state_dir: Path,
    result_path: Path,
    message_path: Path,
    ledger: Path,
    result: dict[str, Any],
    args: argparse.Namespace,
) -> int:
    result["finished_at"] = utc_now()
    atomic_write(result_path, json.dumps(result, indent=2, sort_keys=True) + "\n")
    atomic_write(message_path, render_message(result))
    notify_rc, notify_detail = (0, "notification disabled")
    if not args.no_notify:
        notify_rc, notify_detail = notify(parent, message_path)
    result["notification_rc"] = notify_rc
    result["notification_detail"] = notify_detail
    atomic_write(result_path, json.dumps(result, indent=2, sort_keys=True) + "\n")
    append_jsonl(ledger, {**result, "event": "attempt-finish"})
    print(render_message(result), end="")
    if notify_rc != 0:
        print(
            f"owner-checkout-validate-watch: NOTIFICATION FAILED: {notify_detail}",
            file=sys.stderr,
        )
        return NOTIFY_FAILED
    return {
        "PASS": PASS,
        "FAIL": FAIL,
        "NO_RESULT": NO_RESULT,
        "CANNOT_START": CANNOT_START,
        "RESTORE_FAILED": RESTORE_FAILED,
    }.get(str(result["state"]), CANNOT_START)


if __name__ == "__main__":
    raise SystemExit(main())
