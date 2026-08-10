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
import re
import shutil
import subprocess
import sys
from typing import Any, Iterable, Sequence


PASS = 0
FAIL = 1
NO_RESULT = 2
CANNOT_START = 3
RESTORE_FAILED = 4
NOTIFY_FAILED = 5
BLOCKED_ON_LOCK = 6
KNOWN_EXCEPTION_PATH = "validate"
KNOWN_EXCEPTION_TARGET = "scripts/validate.rs"


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


def known_exception_assessment(checkout: Path, status: list[str]) -> dict[str, Any]:
    """Classify exactly one known owner path without ever calling it clean."""
    link = checkout / KNOWN_EXCEPTION_PATH
    observed_target = os.readlink(link) if link.is_symlink() else None
    exact = (
        status == [f"?? {KNOWN_EXCEPTION_PATH}"]
        and link.is_symlink()
        and observed_target == KNOWN_EXCEPTION_TARGET
    )
    makefile = (checkout / "Makefile").read_text(encoding="utf-8", errors="replace")
    logical_makefile = makefile.replace("\\\n", " ")
    phony = any(
        line.startswith(".PHONY:") and KNOWN_EXCEPTION_PATH in line.split()[1:]
        for line in logical_makefile.splitlines()
    )
    direct_recipe = "./scripts/validate.rs $(ARGS)" in makefile
    return {
        "policy_path": KNOWN_EXCEPTION_PATH,
        "matched": exact,
        "applied": False,
        "paths": [],
        "observed_status": status,
        "object_kind": "symlink" if link.is_symlink() else None,
        "observed_symlink_target": observed_target,
        "required_symlink_target": KNOWN_EXCEPTION_TARGET,
        "could_affect_make_validate": not (phony and direct_recipe),
        "make_assessment": (
            "validate is .PHONY and its recipe invokes ./scripts/validate.rs directly; "
            "the ./validate symlink is not selected"
            if phony and direct_recipe
            else "Makefile did not prove both .PHONY membership and the direct recipe; "
            "the exception may affect make validate"
        ),
    }


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


def canonical_ledger_rows(parent: Path) -> list[dict[str, Any]]:
    adapter = parent / "ci-hub/ledger/validate_rows.py"
    completed = run([sys.executable, adapter, "rows"], cwd=parent)
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()
        raise RuntimeError(f"canonical ledger reader refused: {detail}")
    rows: list[dict[str, Any]] = []
    for line in (completed.stdout or "").splitlines():
        try:
            row = json.loads(line)
        except (ValueError, TypeError):
            continue
        if isinstance(row, dict):
            rows.append(row)
    return rows


def latest_attempt_row(
    candidates: Iterable[dict[str, Any]],
    *,
    checkout: Path,
    sha: str,
    launched_at: dt.datetime,
) -> dict[str, Any] | None:
    rows: list[dict[str, Any]] = []
    for row in candidates:
        if row.get("commit") != sha:
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


def lock_block_evidence(log: Path) -> dict[str, Any] | None:
    """Recognize an explicit fail-closed validate-lock refusal from durable output."""
    if not log.is_file():
        return None
    text = log.read_text(encoding="utf-8", errors="replace")
    refusal_line = next(
        (
            line
            for line in text.splitlines()
            if "REFUSED: box-exclusive anchor held" in line
        ),
        None,
    )
    markers = [
        marker
        for marker in ("quarantine: ACTIVE", "QUARANTINED")
        if marker in text
    ]
    if refusal_line is not None:
        markers.append("REFUSED: box-exclusive anchor held")
    exit_three = any(
        "validate-lock" in line and "exit=3" in line
        for line in text.splitlines()
    )
    if not (markers and exit_three):
        return None
    evidence: dict[str, Any] = {
        "kind": "validate-lock-refusal",
        "refusal_markers": markers,
        "validate_lock_exit": 3,
        "log": str(log),
        "interpretation": "validation was refused before product work executed",
    }
    if refusal_line is not None:
        evidence["refusal_line"] = refusal_line
        match = re.search(r"\bpid=(\d+)\b", refusal_line)
        if match:
            evidence["refusal_pid"] = int(match.group(1))
    return evidence


def parse_key_value_file(path: Path) -> dict[str, str]:
    if not path.is_file():
        return {}
    fields: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            fields[key] = value
    return fields


def process_start_ticks(proc_root: Path, pid: int) -> int | None:
    try:
        text = (proc_root / str(pid) / "stat").read_text(encoding="utf-8")
        # Fields after the final ") " begin with field 3; starttime is field 22.
        return int(text.rsplit(") ", 1)[1].split()[19])
    except (OSError, IndexError, ValueError):
        return None


def lock_holder_attribution(
    parent: Path,
    evidence: dict[str, Any],
    *,
    proc_root: Path = Path("/proc"),
    boot_id_path: Path = Path("/proc/sys/kernel/random/boot_id"),
) -> dict[str, Any]:
    """Bind a refusal to the live lock holder or explicitly decline to guess."""
    refusal_pid = evidence.get("refusal_pid")
    if not isinstance(refusal_pid, int):
        return {"status": "unattributed", "reason": "refusal carried no holder pid"}

    lease = parse_key_value_file(parent / ".validate-lock")
    owner = parse_key_value_file(parent / ".validate-lock.owner")
    cleanup = parse_key_value_file(parent / ".validate-lock.cleanup-required")
    try:
        owner_pid = int(owner["pid"])
        owner_ticks = int(owner["start_ticks"])
        current_boot = boot_id_path.read_text(encoding="utf-8").strip()
    except (KeyError, OSError, ValueError):
        return {"status": "unattributed", "reason": "canonical owner identity unavailable"}

    target = lease.get("target")
    agent = lease.get("agent")
    operation = cleanup.get("operation")
    identity_matches = (
        refusal_pid == owner_pid
        and owner.get("boot_id") == current_boot
        and process_start_ticks(proc_root, owner_pid) == owner_ticks
        and bool(target)
        and bool(agent)
        and cleanup.get("agent") == agent
        and operation == f"validate:{target}"
    )
    if not identity_matches:
        return {
            "status": "unattributed",
            "reason": "refusal pid did not bind to the live canonical lock identity",
        }

    records: list[dict[str, Any]] = []
    for record_path in sorted((parent / "ignored/validate/runs").glob("*.json")):
        try:
            record = json.loads(record_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        unit = str(record.get("unit", ""))
        cgroup_unit = Path(cleanup.get("cgroup", "")).name
        if (
            record.get("target") == target
            and record.get("agent") == agent
            and unit
            and unit == cgroup_unit
        ):
            records.append(record)
    if len(records) != 1:
        return {
            "status": "unattributed",
            "reason": f"canonical identity matched {len(records)} exact run records, expected 1",
            "agent": agent,
            "target": target,
        }

    record = records[0]
    pr = record.get("pr")
    reserved = pr == 2012
    return {
        "status": "attributed",
        "agent": agent,
        "target": target,
        "pr": pr,
        "unit": record.get("unit"),
        "process_identity": {
            "pid": owner_pid,
            "start_ticks": owner_ticks,
            "boot_id": current_boot,
            "live_match": True,
        },
        "contention_class": (
            "reserved-publication-window" if reserved else "ordinary-contention"
        ),
        "interpretation": (
            "reserved #2012 publication window"
            if reserved
            else "identified holder is not the reserved #2012 publication window"
        ),
    }


def privileged_tests_evidence(log: Path) -> dict[str, Any] | None:
    """Extract the decisive privileged-tests node verdict and raw failure detail."""
    if not log.is_file():
        return None
    lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
    prefix = "[privileged-build.privileged_tests]"
    terminal = next(
        (
            line
            for line in reversed(lines)
            if line.startswith(prefix)
            and any(token in line for token in ("✓ PASS", "✗ FAIL", "⊘ ABORT"))
        ),
        None,
    )
    if terminal is None:
        return None
    status = (
        "PASS"
        if "✓ PASS" in terminal
        else "FAIL"
        if "✗ FAIL" in terminal
        else "ABORT"
    )
    timing = re.search(r"\(([^()]*)\)\s*$", terminal)
    timing_text = timing.group(1) if timing else None
    duration = timing_text.split(",", 1)[0].strip() if timing_text else None
    exit_match = re.search(r"\bexit\s+(\d+)\b", timing_text or "")

    detail: list[str] = []
    detail_start = f"{prefix} ----- detail -----"
    detail_end = f"{prefix} ----- end detail -----"
    try:
        start = lines.index(detail_start) + 1
        end = lines.index(detail_end, start)
        detail = [line for line in lines[start:end] if line.strip()]
    except ValueError:
        pass
    evidence: dict[str, Any] = {
        "status": status,
        "duration": duration,
        "timing": timing_text,
        "exit_code": int(exit_match.group(1)) if exit_match else None,
        "terminal_line": terminal,
        "detail_lines": detail,
        "detail_empty": not detail,
        "log": str(log),
    }
    if status != "PASS":
        evidence["raw_failure_line"] = detail[0] if detail else terminal
    evidence["prior_refusal_signature"] = (
        status == "FAIL"
        and duration == "0s"
        and evidence["exit_code"] == 1
        and not detail
    )
    return evidence


def first_raw_failure_line(log: Path) -> str | None:
    if not log.is_file():
        return None
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if any(
            marker in line
            for marker in ("✗ FAIL", "🚫 validate REFUSED", "❌ validate FAIL")
        ):
            return line
    return None


def append_launcher_output(log: Path):
    """Capture validate-run's wrapper output without overwriting its service log.

    ``start_unit.py`` points systemd's stdout/stderr at this same path.  The
    wrapper remains alive while that service writes, so a normal ``"w"`` file
    descriptor can later overwrite the beginning of the service's refusal.
    O_APPEND makes the two writers preserve each other's complete evidence.
    """
    return log.open("a", encoding="utf-8")


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
    exception = result.get("known_dirty_exception") or {}
    lines = [
        "OWNER CHECKOUT PERIODIC VALIDATE: " + str(result["state"]),
        f"checkout: {result['checkout']}",
        f"sha: {result.get('sha') or 'unresolved'}",
        f"attempt: {result['attempt_id']}",
        f"reason: {result.get('reason') or 'none'}",
        f"executed_tests: {result.get('executed_tests')!r}",
        f"known_dirty_exception_applied: {exception.get('applied')!r}",
        f"known_dirty_exception_paths: {exception.get('paths')!r}",
        f"known_dirty_exception_make_assessment: {exception.get('make_assessment')!r}",
        f"canonical_qualified: {result.get('canonical_qualified')!r}",
        f"exception_adjusted_qualified: {result.get('exception_adjusted_qualified')!r}",
        f"landing_authority: {result.get('landing_authority')!r}",
        f"lock_block_evidence: {result.get('lock_block_evidence')!r}",
        f"lock_holder: {result.get('lock_holder')!r}",
        f"privileged_tests_node: {result.get('privileged_tests_node')!r}",
        f"raw_failure_line: {result.get('raw_failure_line')!r}",
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
        "landing_authority": False,
        "known_dirty_exception": {
            "policy_path": KNOWN_EXCEPTION_PATH,
            "applied": False,
            "paths": [],
            "observed_status": [],
        },
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
        exception = known_exception_assessment(checkout, pre["dirty_paths"])
        result["known_dirty_exception"] = exception
        if pre["branch"] != "main":
            result["reason"] = f"owner checkout branch is {pre['branch']!r}, expected 'main'"
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = pre["dirty_paths"]
            return conclude()
        if pre["dirty_paths"] and not exception["matched"]:
            result["reason"] = "owner checkout is dirty before launch"
            result["dirty_paths_after"] = pre["dirty_paths"]
            result["post_tree_hash"] = pre["whole_tree_hash"]
            return conclude()
        if exception["matched"]:
            exception["applied"] = True
            exception["paths"] = [KNOWN_EXCEPTION_PATH]

        if not args.no_fetch:
            fetched = run(["with-proxy", "git", "fetch", "origin", "main"], cwd=checkout)
            if fetched.returncode != 0:
                result["reason"] = "origin/main fetch failed: " + (
                    fetched.stderr or fetched.stdout or "unknown error"
                ).strip()
                result["post_tree_hash"] = pre["whole_tree_hash"]
                result["dirty_paths_after"] = pre["dirty_paths"]
                return conclude()
        origin = git(checkout, "rev-parse", "origin/main").strip()
        if origin != pre["head"]:
            result["reason"] = f"owner checkout is stale: HEAD={pre['head']} origin/main={origin}"
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = pre["dirty_paths"]
            return conclude()

        if args.dry_run:
            result.update(state="NO_RESULT", reason="dry-run: launch deliberately skipped")
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = pre["dirty_paths"]
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
            *(["--allow-owner-validate-symlink"] if exception["applied"] else []),
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
        with append_launcher_output(validate_log) as output:
            completed = run(command, cwd=parent, stdout=output, stderr=subprocess.STDOUT)
        result["validate_run_rc"] = completed.returncode
        lock_block = lock_block_evidence(validate_log)
        if lock_block is not None:
            result["lock_block_evidence"] = lock_block
            holder = lock_holder_attribution(parent, lock_block)
            result["lock_holder"] = holder

        row = latest_attempt_row(
            canonical_ledger_rows(parent),
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
            result["canonical_qualified"] = qualified
            result["canonical_qualification_reason"] = reason
            result["landing_authority"] = qualified
            exception_qualified = False
            exception_reason = "not evaluated"
            if exception["applied"]:
                adjusted = dict(row)
                adjusted["tree_dirty"] = False
                adjusted["commit_anchored"] = True
                exception_qualified, exception_reason = qualify_row(
                    parent, checkout, pre["head"], adjusted
                )
            result["exception_adjusted_qualified"] = exception_qualified
            result["exception_adjusted_reason"] = exception_reason
        else:
            qualified, reason = False, "no exact-attempt ledger row"
            exception_qualified = False
            result["validate_log"] = str(validate_log)

        evidence_log = Path(str(result["validate_log"]))
        node_evidence = privileged_tests_evidence(evidence_log)
        if node_evidence is not None:
            result["privileged_tests_node"] = node_evidence
            if node_evidence.get("raw_failure_line"):
                result["raw_failure_line"] = node_evidence["raw_failure_line"]
        if "raw_failure_line" not in result:
            raw_failure = first_raw_failure_line(evidence_log)
            if raw_failure is not None:
                result["raw_failure_line"] = raw_failure

        if row is None and lock_block is not None:
            holder = result.get("lock_holder") or {}
            holder_interpretation = holder.get("interpretation")
            holder_context = (
                f": {holder_interpretation}" if holder_interpretation else ""
            )
            result.update(
                state="BLOCKED_ON_LOCK",
                reason=(
                    "validate-lock refused admission before product work"
                    f"{holder_context}; no exact-attempt ledger row"
                ),
            )
        elif row is not None and row.get("result") == "fail":
            result.update(
                state="FAIL",
                reason=(
                    "exact-attempt canonical ledger row reports result=fail; "
                    f"wrapper exit={completed.returncode} is not the job verdict"
                ),
            )
        elif completed.returncode != 0:
            result.update(
                state="FAIL",
                reason=f"make validate exit={completed.returncode}; {reason}",
            )
        elif exception["applied"] and (qualified or exception_qualified):
            result.update(
                state="PASS_WITH_KNOWN_EXCEPTION",
                reason=(
                    "counted full run passed with the disclosed owner-symlink exception; "
                    + (
                        "canonical receipt also qualifies"
                        if qualified
                        else "canonical receipt remains non-authoritative"
                    )
                ),
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
        "PASS_WITH_KNOWN_EXCEPTION": PASS,
        "FAIL": FAIL,
        "NO_RESULT": NO_RESULT,
        "CANNOT_START": CANNOT_START,
        "RESTORE_FAILED": RESTORE_FAILED,
        "BLOCKED_ON_LOCK": BLOCKED_ON_LOCK,
    }.get(str(result["state"]), CANNOT_START)


if __name__ == "__main__":
    raise SystemExit(main())
