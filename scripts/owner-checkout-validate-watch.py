#!/usr/bin/env python3
"""Periodically validate the owner's canonical Hermit commit, fail closed.

The owner's checkout supplies an exact commit and Git objects, but is not the
tree validation executes.  ``validate-run --materialize-target`` creates and
checks a fresh registered checkout of that commit.  Mutable owner-checkout
state is recorded and warned about without preventing that isolated run; the
fresh checkout's own cleanliness remains a blocking admission condition.
Every invocation writes a durable result before notification.
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
import time
from typing import Any, Iterable, Sequence


TOOL_ROOT = Path(os.path.abspath(__file__)).parents[1]
RUN_REGISTRY_DIR = TOOL_ROOT / "ci-hub" / "validate"
LEDGER_DIR = TOOL_ROOT / "ci-hub" / "ledger"
sys.path.insert(0, str(RUN_REGISTRY_DIR))
sys.path.insert(0, str(LEDGER_DIR))
import run_registry  # type: ignore  # noqa: E402
import validate_rows  # type: ignore  # noqa: E402


PASS = 0
FAIL = 1
NO_RESULT = 2
CANNOT_START = 3
RESTORE_FAILED = 4
BLOCKED_ON_LOCK = 6
#: ⚠️ 5 WAS `NOTIFY_FAILED` AND IS RETIRED. It was returned INSTEAD of the
#: verdict when the report could not be delivered, so a pass and a failure left
#: the same exit code behind. Measured 2026-09-05 on two consecutive firings an
#: hour apart: firing 1 was state FAIL and firing 2 was state
#: PASS_ALREADY_VALIDATED, and both exited 5. The owner's checkout had passed and
#: nothing in the exit code said so. A dead value in an exit-code table invites
#: reuse, so it is named here rather than left as a gap.
#:
#: ⚠️ AND THE FIX IS NOT "RETURN THE VERDICT INSTEAD". A report nobody received
#: has to stay visible, which is exactly what the unit's SuccessExitStatus list
#: is for. One integer, two independent facts -- so the verdict keeps 0..6 and an
#: undelivered run adds a fixed offset. The verdict is still recoverable and the
#: code still falls outside SuccessExitStatus, so neither fact is silent.
#:
#: 16 because it must stay under 126 to avoid the shell's cannot-execute codes
#: and under 128 to avoid the signal range, and 64..78 are the sysexits values
#: this repository already uses elsewhere -- scripts/validate.rs spends 75 on
#: no_result -- so an offset there would collide with codes that already mean
#: something else. 16 leaves 7..15 free.
UNDELIVERED_OFFSET = 16
#: The verdicts, by the state each run records. Kept beside the offset because
#: the two only make sense together.
VERDICT_BY_STATE = {
    "PASS": PASS,
    "PASS_WITH_KNOWN_EXCEPTION": PASS,
    "PASS_ALREADY_VALIDATED": PASS,
    "FAIL": FAIL,
    "NO_RESULT": NO_RESULT,
    "CANNOT_START": CANNOT_START,
    "RESTORE_FAILED": RESTORE_FAILED,
    "BLOCKED_ON_LOCK": BLOCKED_ON_LOCK,
}


def exit_code_for(state: object, *, delivered: bool) -> int:
    """The run's exit code: the verdict, offset when the report did not get out.

    An unrecognised state is CANNOT_START, which is what the substituted mapping
    did too -- an unknown verdict must not read as a pass.
    """
    verdict = VERDICT_BY_STATE.get(str(state), CANNOT_START)
    return verdict if delivered else verdict + UNDELIVERED_OFFSET


def verdict_from_exit_code(code: int) -> int:
    """The validation verdict carried by an exit code, delivered or not."""
    return code - UNDELIVERED_OFFSET if code >= UNDELIVERED_OFFSET else code


def report_reached_its_reader(code: int) -> bool:
    """Whether the run's report was delivered. Never inferred from the verdict."""
    return code < UNDELIVERED_OFFSET
ADMISSION_STATUS_UNKNOWN = 2
KNOWN_EXCEPTION_PATH = "validate"
KNOWN_EXCEPTION_TARGET = "scripts/validate.rs"
ADMISSION_CURSOR_SCHEMA = 1
ADMISSION_CURSOR_NAME = "owner-watch-admission.json"
ACQUIRED_PATTERN = re.compile(
    r"^validate-lock: ACQUIRED by (?P<agent>\S+) running "
    r"(?P<kind>\S+) (?P<target>[0-9a-f]{40})(?:\s|$)"
)
NODE_LINE_PATTERN = re.compile(r"^\[(?P<node>[^]]+)\]\s+(?P<body>.*)$")
E2E_NODE_PATTERN = re.compile(r"^(?:privileged-)?e2e\.")
# `validate-run` spends ONE exit code (2) on every admission refusal, so the code
# alone cannot separate "this SHA is already proven green, nothing to re-run"
# from "this head cannot be validated at all". The durable line can, and it is
# the only place that distinction is written down.
ADMISSION_REFUSAL_PATTERN = re.compile(r"^validate-run: REFUSED: (?P<detail>.+)$")
READMISSION_REFUSAL_PATTERN = re.compile(
    r"canonical validate-status already has terminal verdict "
    r"(?P<verdict>[A-Z][A-Z-]*) for unchanged SHA (?P<sha>[0-9a-f]{40})"
)
CANONICAL_GREEN_VERDICT = "VALIDATED"
CANONICAL_RED_VERDICT = "FAILED"
COMMIT_VALIDATION_HARD_GREEN = "HARD_GREEN"
COMMIT_VALIDATION_NOT_HARD_GREEN = "NOT_HARD_GREEN"
COMMIT_VALIDATION_UNKNOWN = "UNKNOWN"
CHECKOUT_FRESHNESS_UNKNOWN = "UNKNOWN"
PROCESS_LIVENESS_LIVE = "LIVE"
PROCESS_LIVENESS_DEAD = "DEAD"
PROCESS_LIVENESS_UNKNOWN = "UNKNOWN"
CHECKOUT_FRESHNESS_BY_RELATION = {
    "current": "CURRENT",
    "stale": "BEHIND",
    "ahead": "AHEAD",
    "diverged": "DIVERGED",
}
SCORECARD_PATHS = ("SCORECARD.md", "ci/compat-envelope/cells.json")
RELINK_DIRECT_SIGNATURES = (
    (
        "non-executable",
        re.compile(r"(?:not executable|non-executable)", re.IGNORECASE),
    ),
    ("exec-format-error", re.compile(r"exec format error", re.IGNORECASE)),
    ("text-file-busy", re.compile(r"text file busy", re.IGNORECASE)),
)


class TypedAdmissionRefusal(Exception):
    """Internal control flow after a producer proves payload non-admission."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def epoch_millis() -> int:
    return time.time_ns() // 1_000_000


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


def checkout_revision_context(
    checkout: Path, *, head: str, origin_main: str
) -> dict[str, Any]:
    """Describe the measured revision without requiring it to equal the tip."""
    completed = run(
        ["git", "rev-list", "--left-right", "--count", f"{head}...origin/main"],
        cwd=checkout,
    )
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "unknown error").strip()
        raise RuntimeError(f"cannot measure checkout distance from origin/main: {detail}")
    fields = (completed.stdout or "").split()
    if len(fields) != 2:
        raise RuntimeError(
            "cannot measure checkout distance from origin/main: "
            f"expected two counts, got {(completed.stdout or '').strip()!r}"
        )
    try:
        ahead, behind = (int(field) for field in fields)
    except ValueError as error:
        raise RuntimeError(
            "cannot measure checkout distance from origin/main: "
            f"non-integer counts {(completed.stdout or '').strip()!r}"
        ) from error

    if head == origin_main and ahead == 0 and behind == 0:
        relation = "current"
    elif behind > 0 and ahead == 0:
        relation = "stale"
    elif ahead > 0 and behind == 0:
        relation = "ahead"
    else:
        relation = "diverged"
    current = relation == "current"
    qualifier = (
        f"validate describes current main at SHA {head}"
        if current
        else (
            f"validate describes the owner's checkout at SHA {head}, which is "
            f"{behind} commit(s) behind and {ahead} commit(s) ahead of "
            f"origin/main {origin_main}; it does not describe current main"
        )
    )
    return {
        "head": head,
        "origin_main": origin_main,
        "relation": relation,
        "freshness": CHECKOUT_FRESHNESS_BY_RELATION[relation],
        "behind": behind,
        "ahead": ahead,
        "current": current,
        "verdict_qualifier": qualifier,
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


def admission_cursor_path(state_dir: Path) -> Path:
    return state_dir / ADMISSION_CURSOR_NAME


def load_admission_cursor(state_dir: Path) -> dict[str, Any] | None:
    path = admission_cursor_path(state_dir)
    try:
        cursor = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    if not isinstance(cursor, dict):
        return None
    if cursor.get("schema_version") != ADMISSION_CURSOR_SCHEMA:
        return None
    if not isinstance(cursor.get("observation_started_at_epoch_ms"), int):
        return None
    admission = cursor.get("last_admission")
    if admission is not None and not (
        isinstance(admission, dict)
        and isinstance(admission.get("admitted_at_epoch_ms"), int)
        and isinstance(admission.get("attempt_id"), str)
        and isinstance(admission.get("target"), str)
    ):
        return None
    return cursor


def ensure_admission_cursor(state_dir: Path) -> dict[str, Any]:
    cursor = load_admission_cursor(state_dir)
    if cursor is not None:
        return cursor
    now_ms = epoch_millis()
    cursor = {
        "schema_version": ADMISSION_CURSOR_SCHEMA,
        "observation_started_at": utc_now(),
        "observation_started_at_epoch_ms": now_ms,
        "last_admission": None,
    }
    atomic_write(
        admission_cursor_path(state_dir),
        json.dumps(cursor, indent=2, sort_keys=True) + "\n",
    )
    return cursor


def acquired_line(log: Path, *, target: str) -> str | None:
    """Return the exact owner-watch acquisition line bound to ``target``.

    The marker is near the beginning of validate-lock output.  Bound the read so
    polling it remains cheap even while a full validation grows a large log.
    """
    try:
        with log.open("r", encoding="utf-8", errors="replace") as source:
            text = source.read(128 * 1024)
    except OSError:
        return None
    for line in text.splitlines():
        match = ACQUIRED_PATTERN.match(line)
        if not match:
            continue
        if (
            match.group("agent") == "owner-checkout-watch"
            and match.group("kind") == "validate"
            and match.group("target") == target
        ):
            return line
    return None


def validation_admission_result(record: Path) -> tuple[dict[str, Any] | None, str | None]:
    """Read the producer-owned admission result; missing evidence stays unknown."""
    try:
        value = run_registry.read_record(record)
    except RuntimeError as error:
        return None, str(error)
    raw = value.get("admission_result")
    if raw is None:
        return None, None
    if not isinstance(raw, dict):
        # Current records are checked by run_registry before reaching here.
        return None, "validation run handle admission_result is not an object"
    return dict(raw), None


def validation_run_process_liveness(record: Path) -> str:
    """Classify the current handle's exact process identity without guessing.

    This is deliberately narrower than "the handle says running": a stale
    state label cannot veto a prior canonical result forever. A malformed
    record or unreadable process observation is UNKNOWN, distinct from a
    missing/recycled process that is positively DEAD.
    """
    try:
        value = run_registry.read_record(record)
        handle = run_registry.parse_current_record(value)
    except RuntimeError:
        return PROCESS_LIVENESS_UNKNOWN
    if handle.process_identity is None:
        return PROCESS_LIVENESS_DEAD
    identity = handle.process_identity
    try:
        current_boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError:
        return PROCESS_LIVENESS_UNKNOWN
    if not current_boot:
        return PROCESS_LIVENESS_UNKNOWN
    if identity.boot_id != current_boot:
        return PROCESS_LIVENESS_DEAD
    try:
        fields = (
            Path(f"/proc/{identity.pid}/stat")
            .read_text()
            .rsplit(") ", 1)[-1]
            .split()
        )
    except FileNotFoundError:
        return PROCESS_LIVENESS_DEAD
    except OSError:
        return PROCESS_LIVENESS_UNKNOWN
    try:
        observed_ticks = int(fields[19])
    except (IndexError, ValueError):
        return PROCESS_LIVENESS_UNKNOWN
    return (
        PROCESS_LIVENESS_LIVE
        if observed_ticks == identity.start_ticks
        else PROCESS_LIVENESS_DEAD
    )


def record_admission(
    state_dir: Path,
    result: dict[str, Any],
    *,
    target: str,
    run_record: Path,
    admission_result: dict[str, Any],
) -> dict[str, Any]:
    """Publish the framework's exact admission, never a conclusion from text."""
    recorded_at = str(admission_result["recorded_at"])
    parsed = parse_time(recorded_at)
    if parsed is None:
        raise RuntimeError("typed admission result has an unreadable recorded_at")
    admission = {
        "state": "ADMITTED",
        "agent": "owner-checkout-watch",
        "kind": "validate",
        "target": target,
        "attempt_id": result["attempt_id"],
        "admitted_at": recorded_at,
        "admitted_at_epoch_ms": int(parsed.timestamp() * 1000),
        "run_record": str(run_record),
        "admission_result": admission_result,
        "result_json": result["result_json"],
    }
    result["lock_admission"] = admission
    cursor = ensure_admission_cursor(state_dir)
    cursor["last_admission"] = admission
    atomic_write(
        admission_cursor_path(state_dir),
        json.dumps(cursor, indent=2, sort_keys=True) + "\n",
    )
    # Publish the in-flight fact immediately. A red, killed, or still-running
    # validation is nevertheless an admission; the final writer adds verdict.
    atomic_write(
        Path(str(result["result_json"])),
        json.dumps(result, indent=2, sort_keys=True) + "\n",
    )
    return admission


def observe_admission_result(
    record: Path,
    *,
    state_dir: Path,
    result: dict[str, Any],
    target: str,
) -> dict[str, Any] | None:
    admission, error = validation_admission_result(record)
    result["validation_run_record"] = str(record)
    if error is not None:
        result["lock_admission"] = {
            "state": "UNKNOWN",
            "reason": error,
            "run_record": str(record),
        }
        return None
    if admission is None:
        return None
    result["admission_result"] = admission
    if admission.get("state") == "admitted":
        if result.get("lock_admission", {}).get("state") != "ADMITTED":
            record_admission(
                state_dir,
                result,
                target=target,
                run_record=record,
                admission_result=admission,
            )
    elif admission.get("state") == "refused":
        result["lock_admission"] = {
            "state": "NOT_ADMITTED",
            "reason": admission.get("reason"),
            "exit_code": admission.get("exit_code"),
            "recorded_at": admission.get("recorded_at"),
            "run_record": str(record),
        }
    return admission


def parse_boundary(value: str) -> int:
    try:
        # Release records carry epoch seconds. Preserve fractional input while
        # comparing at millisecond precision so same-second handoffs are visible.
        return int(float(value) * 1000)
    except ValueError:
        parsed = parse_time(value)
        if parsed is None:
            raise ValueError("boundary must be epoch seconds or RFC3339")
        return int(parsed.timestamp() * 1000)


def admission_status_since(state_dir: Path, boundary: str) -> tuple[int, dict[str, Any]]:
    """Cheap public observable: did the watch acquire after ``boundary``?"""
    try:
        boundary_ms = parse_boundary(boundary)
    except (ValueError, OverflowError) as error:
        return ADMISSION_STATUS_UNKNOWN, {
            "schema_version": 1,
            "state": "UNKNOWN",
            "admitted": False,
            "reason": str(error),
            "boundary": boundary,
        }
    cursor = load_admission_cursor(state_dir)
    if cursor is None:
        return ADMISSION_STATUS_UNKNOWN, {
            "schema_version": 1,
            "state": "UNKNOWN",
            "admitted": False,
            "reason": "owner-watch admission cursor is missing, unreadable, or unknown-schema",
            "boundary_epoch_ms": boundary_ms,
            "cursor": str(admission_cursor_path(state_dir)),
        }
    observation_ms = int(cursor["observation_started_at_epoch_ms"])
    admission = cursor.get("last_admission")
    if isinstance(admission, dict) and admission["admitted_at_epoch_ms"] > boundary_ms:
        return 0, {
            "schema_version": 1,
            "state": "ADMITTED",
            "admitted": True,
            "reason": "exact owner-watch validate-lock acquisition observed after boundary",
            "boundary_epoch_ms": boundary_ms,
            "observation_started_at_epoch_ms": observation_ms,
            "admission": admission,
        }
    if boundary_ms < observation_ms:
        return ADMISSION_STATUS_UNKNOWN, {
            "schema_version": 1,
            "state": "UNKNOWN",
            "admitted": False,
            "reason": "boundary predates authoritative admission observation",
            "boundary_epoch_ms": boundary_ms,
            "observation_started_at_epoch_ms": observation_ms,
            "last_admission": admission,
        }
    return 1, {
        "schema_version": 1,
        "state": "NOT_ADMITTED",
        "admitted": False,
        "reason": "no exact owner-watch validate-lock acquisition observed after boundary",
        "boundary_epoch_ms": boundary_ms,
        "observation_started_at_epoch_ms": observation_ms,
        "last_admission": admission,
    }


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


def canonical_ledger_rows(state_root: Path) -> list[dict[str, Any]]:
    """Read canonical state with the adapter imported from this exact tool tree."""
    return validate_rows.rows(root=state_root)


def latest_attempt_row(
    candidates: Iterable[dict[str, Any]],
    *,
    checkout: Path,
    sha: str,
    launched_at: dt.datetime,
    run_id: str | None = None,
) -> dict[str, Any] | None:
    rows: list[dict[str, Any]] = []
    for row in candidates:
        if row.get("commit") != sha:
            continue
        if run_id is not None:
            if row.get("run_id") != run_id:
                continue
        else:
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


def qualify_row(
    tool_root: Path,
    state_root: Path,
    checkout: Path,
    sha: str,
    row: dict[str, Any],
) -> tuple[bool, str]:
    sys.path.insert(0, str(tool_root / "ci-hub"))
    import qualifying_receipt  # type: ignore

    predicate = qualifying_receipt.load()
    return qualifying_receipt.authoritative_row_qualification(
        row,
        sha,
        predicate,
        repositories=[checkout, state_root],
    )


def lock_block_evidence(log: Path) -> dict[str, Any] | None:
    """Recognize an explicit fail-closed validate-lock refusal from durable output."""
    if not log.is_file():
        return None
    text = log.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    anchor_refusal_line = next(
        (
            line
            for line in lines
            if "REFUSED: box-exclusive anchor held" in line
        ),
        None,
    )
    direct_refusal_line = next(
        (line for line in lines if line.startswith("REFUSED: ")),
        None,
    )
    acquired = any(ACQUIRED_PATTERN.match(line) for line in lines)
    markers = [
        marker
        for marker in ("quarantine: ACTIVE", "QUARANTINED")
        if marker in text
    ]
    if anchor_refusal_line is not None:
        markers.append("REFUSED: box-exclusive anchor held")
    elif direct_refusal_line is not None and not acquired:
        markers.append("REFUSED: validate-lock refused before acquisition")
    exit_three = any(
        "validate-lock" in line and "exit=3" in line
        for line in lines
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
    refusal_line = anchor_refusal_line or direct_refusal_line
    if refusal_line is not None:
        evidence["refusal_line"] = refusal_line
    if anchor_refusal_line is not None:
        anchor: dict[str, Any] = {}
        for key in ("pid", "start_ticks"):
            match = re.search(rf"\b{key}=(\d+)\b", anchor_refusal_line)
            if match:
                anchor[key] = int(match.group(1))
        for key in ("boot_id", "agent", "target", "unit", "record"):
            # The refusal is prose after the key/value identity and terminates
            # record=PATH with ';'.  Treating that delimiter as part of PATH
            # makes a complete PID/start-ticks/boot-id tuple look unattributed.
            match = re.search(rf"\b{key}=([^\s;]+)", anchor_refusal_line)
            if match:
                anchor[key] = match.group(1)
        if "pid" in anchor:
            evidence["refusal_pid"] = anchor["pid"]
        if anchor:
            evidence["anchor_identity"] = anchor
    return evidence


def admission_refusal_evidence(log: Path) -> dict[str, Any] | None:
    """Recognize a ``validate-run`` ADMISSION refusal and name which kind it is.

    A refusal is not a verdict on the product.  Reporting every nonzero wrapper
    exit as FAIL is what made the de-duplication guard — whose whole job is to
    say "this commit is ALREADY PROVEN GREEN, do not burn the box-exclusive lane
    re-proving it" — render to the owner as a failure.  The guard is correct; only
    its classification was wrong.  So separate the reasons here and let the caller
    treat them differently, rather than widening or disabling the guard.
    """
    if not log.is_file():
        return None
    for raw in log.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw.strip()
        match = ADMISSION_REFUSAL_PATTERN.match(line)
        if match is None:
            continue
        evidence: dict[str, Any] = {
            "kind": "admission-refusal",
            "line": line,
            "detail": match.group("detail"),
        }
        terminal = READMISSION_REFUSAL_PATTERN.search(line)
        if terminal is not None:
            evidence.update(
                kind="canonical-terminal-readmission",
                refused_verdict=terminal.group("verdict"),
                refused_sha=terminal.group("sha"),
            )
        return evidence
    return None


def canonical_validate_status(tool_root: Path, state_root: Path, sha: str) -> dict[str, Any]:
    """Ask the ONE canonical authority whether THIS COMMIT is validated.

    Bound to the SHA, never to this attempt.  A refused re-admission writes no
    ledger row of its own, yet the commit it declined to re-measure may already
    carry a qualifying receipt — deriving the answer from "my attempt produced no
    row" is the same cache-for-source confusion that drifted the
    ``locally-validated`` label.  Unreadable output, a verdict for some other
    SHA, or a VALIDATED verdict with no receipt behind it are all NOT green.
    """
    completed = run(
        [
            "env",
            f"DEV_HERMIT_PARENT={state_root}",
            f"DEV_HERMIT_TOOL_ROOT={tool_root}",
            tool_root / "ci-hub/ci-hub",
            "validate-status",
            "--sha",
            sha,
            "--json",
        ],
        cwd=tool_root,
    )
    status: dict[str, Any] = {
        "sha": sha,
        "available": False,
        "verdict": None,
        "exit_code": completed.returncode,
        "receipt": None,
        "reason": "",
    }
    detail = (completed.stderr or completed.stdout or f"exit {completed.returncode}").strip()
    try:
        report = json.loads(completed.stdout or "")
        verdict = str(report["verdict"])
        reported_sha = str(report["sha"])
    except (ValueError, TypeError, KeyError) as error:
        status["reason"] = f"canonical validate-status unreadable ({detail[:400]}; {error})"
        return status
    if reported_sha != sha:
        status["reason"] = (
            f"canonical validate-status answered for {reported_sha}, not the requested {sha}"
        )
        return status
    receipt = report.get("newest_qualifying")
    if isinstance(receipt, dict):
        identity = receipt.get("receipt_identity")
        status["receipt"] = {
            "validated_at": receipt.get("finished_at"),
            "profile": receipt.get("profile"),
            "selection_mode": receipt.get("selection_mode"),
            "coverage_satisfied": receipt.get("coverage_satisfied"),
            "coverage_status": receipt.get("coverage_status"),
            "discovered_tests": receipt.get("discovered_tests"),
            "executed_tests": receipt.get("executed_tests"),
            "filtered_tests": receipt.get("filtered_tests"),
            "failures": receipt.get("failures"),
            "checks": receipt.get("checks"),
            "log_file": receipt.get("log_file"),
            "receipt_digest": (identity or {}).get("digest")
            if isinstance(identity, dict)
            else None,
        }
    if verdict == CANONICAL_GREEN_VERDICT and status["receipt"] is None:
        status["reason"] = (
            "canonical validate-status is internally inconsistent: verdict=VALIDATED "
            "with no qualifying receipt behind it"
        )
        return status
    status.update(
        available=True,
        verdict=verdict,
        reason=f"canonical validate-status verdict={verdict} for {sha}",
    )
    return status


def describe_receipt(receipt: dict[str, Any] | None) -> str:
    """Render the existing receipt so a green needs no second lookup to audit."""
    if not receipt:
        return "no receipt fields available"
    return (
        f"validated_at={receipt.get('validated_at')} "
        f"profile={receipt.get('profile')} "
        f"selection={receipt.get('selection_mode')} "
        f"coverage_satisfied={receipt.get('coverage_satisfied')} "
        f"coverage_status={receipt.get('coverage_status')} "
        f"executed_tests={receipt.get('executed_tests')} "
        f"failures={receipt.get('failures')} "
        f"checks={receipt.get('checks')}"
    )


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


def process_systemd_unit(proc_root: Path, pid: int) -> str | None:
    """Return the live process's systemd service unit from cgroup v2."""
    try:
        text = (proc_root / str(pid) / "cgroup").read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        fields = line.split(":", 2)
        if len(fields) != 3:
            continue
        for component in reversed(fields[2].split("/")):
            if component.endswith(".service"):
                return component
    return None


def lock_holder_attribution(
    parent: Path,
    evidence: dict[str, Any],
    *,
    proc_root: Path = Path("/proc"),
    boot_id_path: Path = Path("/proc/sys/kernel/random/boot_id"),
) -> dict[str, Any]:
    """Bind a refusal to the live lock holder or explicitly decline to guess."""
    anchor = evidence.get("anchor_identity")
    required = ("pid", "start_ticks", "boot_id", "agent", "target", "unit", "record")
    if not isinstance(anchor, dict) or any(key not in anchor for key in required):
        return {
            "status": "unattributed",
            "reason": "refusal carried no complete anchor identity",
        }
    owner_pid = anchor["pid"]
    owner_ticks = anchor["start_ticks"]
    if (
        not isinstance(owner_pid, int)
        or isinstance(owner_pid, bool)
        or owner_pid <= 0
        or not isinstance(owner_ticks, int)
        or isinstance(owner_ticks, bool)
        or owner_ticks <= 0
        or any(not isinstance(anchor[key], str) or not anchor[key] for key in required[2:])
    ):
        return {"status": "unattributed", "reason": "anchor identity is malformed"}

    target = anchor["target"]
    agent = anchor["agent"]
    unit = anchor["unit"]
    record_path = Path(anchor["record"])
    expected_name = f"{unit.removesuffix('.service')}.json"
    if not record_path.is_absolute() or record_path.name != expected_name:
        return {
            "status": "unattributed",
            "reason": "anchor record path does not bind its recorded systemd unit",
            "agent": agent,
            "target": target,
        }

    expected_identity = {
        "pid": owner_pid,
        "start_ticks": owner_ticks,
        "boot_id": anchor["boot_id"],
    }
    matches: list[tuple[Path, dict[str, Any]]] = []
    for candidate in sorted(record_path.parent.glob("*.json")):
        try:
            record = json.loads(candidate.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if (
            record.get("schema_version") == 1
            and record.get("state") in {"launching", "running"}
            and record.get("target") == target
            and record.get("agent") == agent
            and record.get("unit") == unit
            and record.get("process_identity") == expected_identity
        ):
            matches.append((candidate.resolve(), record))
    resolved_record = record_path.resolve()
    if len(matches) != 1 or matches[0][0] != resolved_record:
        return {
            "status": "unattributed",
            "reason": f"anchor identity matched {len(matches)} exact run records, expected 1",
            "agent": agent,
            "target": target,
        }

    record = matches[0][1]
    try:
        current_boot = boot_id_path.read_text(encoding="utf-8").strip()
    except OSError:
        return {
            "status": "unattributed",
            "reason": "current boot identity is unreadable",
            "agent": agent,
            "target": target,
        }
    identity_result = {
        "pid": owner_pid,
        "start_ticks": owner_ticks,
        "boot_id": anchor["boot_id"],
        "live_match": False,
    }
    if current_boot != anchor["boot_id"]:
        return {
            "status": "dead",
            "reason": "recorded anchor belongs to a different boot",
            "agent": agent,
            "target": target,
            "unit": unit,
            "process_identity": identity_result,
        }
    stat_path = proc_root / str(owner_pid) / "stat"
    live_ticks = process_start_ticks(proc_root, owner_pid)
    if live_ticks is None:
        if not stat_path.exists():
            return {
                "status": "dead",
                "reason": "recorded anchor pid is absent",
                "agent": agent,
                "target": target,
                "unit": unit,
                "process_identity": identity_result,
            }
        return {
            "status": "unattributed",
            "reason": "live pid start_ticks could not be verified",
            "agent": agent,
            "target": target,
        }
    if live_ticks != owner_ticks:
        return {
            "status": "dead",
            "reason": "recorded anchor pid has been reused",
            "agent": agent,
            "target": target,
            "unit": unit,
            "process_identity": identity_result,
        }
    live_unit = process_systemd_unit(proc_root, owner_pid)
    if live_unit != unit:
        return {
            "status": "unattributed",
            "reason": "live process systemd unit does not match the anchor record",
            "agent": agent,
            "target": target,
        }

    identity_result["live_match"] = True
    pr = record.get("pr")
    reserved = pr == 2012
    return {
        "status": "attributed",
        "agent": agent,
        "target": target,
        "pr": pr,
        "unit": unit,
        "process_identity": identity_result,
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


def _binary_snapshot(
    binary: Path, *, launched_at: dt.datetime, observed_at: dt.datetime
) -> dict[str, Any]:
    """Record post-run binary identity without treating its presence as innocence."""
    evidence: dict[str, Any] = {
        "path": str(binary),
        "observed_at": observed_at.isoformat().replace("+00:00", "Z"),
        "exists_after_run": binary.exists(),
    }
    if not binary.exists():
        return evidence
    metadata = binary.stat()
    mtime = dt.datetime.fromtimestamp(metadata.st_mtime, tz=dt.timezone.utc)
    evidence.update(
        executable_after_run=os.access(binary, os.X_OK),
        inode=metadata.st_ino,
        nlink=metadata.st_nlink,
        size=metadata.st_size,
        mode=oct(metadata.st_mode & 0o7777),
        mtime_ns=metadata.st_mtime_ns,
        mtime=mtime.isoformat().replace("+00:00", "Z"),
        mtime_relation_to_observed_run=(
            "inside-observed-run"
            if launched_at <= mtime <= observed_at
            else "before-observed-run"
            if mtime < launched_at
            else "after-observation"
        ),
        mtime_offset_from_run_start_seconds=(mtime - launched_at).total_seconds(),
    )
    return evidence


def relink_race_recurrence_evidence(
    log: Path,
    checkout: Path,
    *,
    launched_at: dt.datetime,
    observed_at: dt.datetime | None = None,
) -> dict[str, Any]:
    """Detect evidence consistent with the intermittent Hermit relink race.

    This is deliberately evidence-only: it neither changes the validate verdict
    nor calls an ordinary product failure a relink race.  The durable log has no
    per-line wall clock, so compile overlap is bound by line order and the
    post-run binary mtime is explicitly scoped to the overall observed run.
    """
    observed = observed_at or dt.datetime.now(dt.timezone.utc)
    base: dict[str, Any] = {
        "triggered": False,
        "log": str(log),
        "suspect_count": 0,
        "simultaneous_consumer_count": 0,
        "simultaneity_basis": (
            "adjacent durable-log emissions; log lines have no wall-clock timestamps"
        ),
        "verdict_effect": "none-evidence-only",
    }
    if not log.is_file():
        return {**base, "reason": "durable log missing"}

    lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
    binary = checkout / "target/debug/hermit"
    binary_snapshot = _binary_snapshot(
        binary, launched_at=launched_at, observed_at=observed
    )

    details: dict[str, list[str]] = {}
    detail_node: str | None = None
    for line in lines:
        match = NODE_LINE_PATTERN.match(line)
        if match is None:
            continue
        node, body = match.group("node"), match.group("body")
        if body == "----- detail -----":
            detail_node = node
            details[node] = []
        elif body == "----- end detail -----" and detail_node == node:
            detail_node = None
        elif detail_node == node and body.strip():
            details[node].append(body)

    active_compiles: dict[str, int] = {}
    node_starts: dict[str, int] = {}
    suspects: list[dict[str, Any]] = []
    for line_number, line in enumerate(lines, start=1):
        match = NODE_LINE_PATTERN.match(line)
        if match is None:
            continue
        node, body = match.group("node"), match.group("body")
        if E2E_NODE_PATTERN.match(node) and "▶ START" in body:
            node_starts[node] = line_number
        if re.search(r"\bCompiling hermit v", body):
            active_compiles[node] = line_number
        elif node in active_compiles and re.search(r"\bFinished `[^`]+` profile", body):
            active_compiles.pop(node, None)

        if not E2E_NODE_PATTERN.match(node):
            continue
        signatures = [
            name
            for name, pattern in RELINK_DIRECT_SIGNATURES
            if pattern.search(body)
        ]
        lower = body.lower()
        if (
            "no such file" in lower
            and str(binary) in body
            and binary_snapshot["exists_after_run"]
        ):
            signatures.append("no-such-file-path-exists-afterwards")
        timeout_failure = (
            "✗ FAIL" in body
            and bool(
                re.search(
                    r"(?:timed? out|timeout|exit\s+124|>\s*\d+(?:\.\d+)?s)",
                    body,
                    re.IGNORECASE,
                )
            )
            and not details.get(node)
        )
        if timeout_failure:
            signatures.append("unexplained-empty-detail-timeout")
        if not signatures:
            continue
        suspects.append(
            {
                "node": node,
                "line_number": line_number,
                "line": line,
                "signatures": signatures,
                "node_start_line": node_starts.get(node),
                "detail_lines": details.get(node, []),
                "compile_in_flight": bool(active_compiles),
                "active_hermit_compiles": [
                    {"node": compile_node, "start_line": start_line}
                    for compile_node, start_line in active_compiles.items()
                ],
                "binary_mtime_relative_to_node_window": (
                    "unknown-no-per-line-wall-clock; see line-order compile "
                    "overlap and overall-run mtime"
                ),
            }
        )

    largest: list[dict[str, Any]] = []
    current: list[dict[str, Any]] = []
    for suspect in suspects:
        if current and suspect["line_number"] != current[-1]["line_number"] + 1:
            if len({item["node"] for item in current}) > len(
                {item["node"] for item in largest}
            ):
                largest = current
            current = []
        current.append(suspect)
    if len({item["node"] for item in current}) > len(
        {item["node"] for item in largest}
    ):
        largest = current
    simultaneous_nodes = sorted({item["node"] for item in largest})
    return {
        **base,
        "triggered": bool(suspects),
        "reason": (
            "one or more e2e consumers matched the relink-race recurrence signature"
            if suspects
            else "no e2e consumer matched the relink-race recurrence signature"
        ),
        "suspect_count": len(suspects),
        "suspects": suspects,
        "compile_in_flight_at_any_suspect": any(
            item["compile_in_flight"] for item in suspects
        ),
        "simultaneous_consumer_count": len(simultaneous_nodes),
        "simultaneous_consumers": simultaneous_nodes,
        "simultaneous_line_range": (
            [largest[0]["line_number"], largest[-1]["line_number"]]
            if largest
            else None
        ),
        "binary_snapshot": binary_snapshot,
    }


def relink_race_retirement_evidence(
    log: Path, *, recurrence_triggered: bool
) -> dict[str, Any]:
    """Require real compile/consumer overlap before retiring the relink race.

    A green run with no overlap is deliberately a non-result for this question.
    The positive witness is line-ordered because the durable DAG log has no
    per-line wall clock: app_strict_verify must be between its Hermit compile
    and Finished lines while e2e nodes emit real per-cell PASS activity. Every
    e2e node that started in the run must later reach terminal PASS.
    """
    base: dict[str, Any] = {
        "confirmed": False,
        "log": str(log),
        "verdict_effect": "none-evidence-only",
        "criterion": (
            "test.app_strict_verify Hermit compile overlaps e2e cell execution, "
            "with terminal PASS for every started e2e consumer"
        ),
    }
    if not log.is_file():
        return {**base, "reason": "durable log missing"}

    lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
    started: dict[str, int] = {}
    terminal: dict[str, dict[str, Any]] = {}
    parsed: list[tuple[int, str, str]] = []
    for line_number, line in enumerate(lines, start=1):
        match = NODE_LINE_PATTERN.match(line)
        if match is None:
            continue
        node, body = match.group("node"), match.group("body")
        parsed.append((line_number, node, body))
        if not E2E_NODE_PATTERN.match(node):
            continue
        if "▶ START" in body:
            started[node] = line_number
        status = (
            "PASS"
            if "✓ PASS" in body
            else "FAIL"
            if "✗ FAIL" in body
            else "ABORT"
            if "⊘ ABORT" in body
            else None
        )
        if status is not None:
            terminal[node] = {
                "status": status,
                "line_number": line_number,
                "line": line,
            }

    compile_windows: list[dict[str, Any]] = []
    compile_start: int | None = None
    compile_start_line: str | None = None
    for line_number, node, body in parsed:
        if node != "test.app_strict_verify":
            continue
        if re.search(r"\bCompiling hermit v", body):
            compile_start = line_number
            compile_start_line = lines[line_number - 1]
        elif compile_start is not None and re.search(
            r"\bFinished `[^`]+` profile", body
        ):
            activity = []
            for event_line, event_node, event_body in parsed:
                if not (compile_start < event_line < line_number):
                    continue
                if not E2E_NODE_PATTERN.match(event_node):
                    continue
                if not re.match(r"^PASS\s+", event_body):
                    continue
                activity.append(
                    {
                        "node": event_node,
                        "line_number": event_line,
                        "line": lines[event_line - 1],
                    }
                )
            compile_windows.append(
                {
                    "start_line_number": compile_start,
                    "start_line": compile_start_line,
                    "finish_line_number": line_number,
                    "finish_line": lines[line_number - 1],
                    "e2e_pass_activity": activity,
                    "overlap_consumers": sorted(
                        {event["node"] for event in activity}
                    ),
                }
            )
            compile_start = None
            compile_start_line = None

    started_nodes = sorted(started)
    nonpassing = [
        {
            "node": node,
            "start_line_number": started[node],
            "terminal": terminal.get(node),
        }
        for node in started_nodes
        if terminal.get(node, {}).get("status") != "PASS"
    ]
    witnessed = [window for window in compile_windows if window["overlap_consumers"]]
    confirmed = bool(witnessed) and bool(started_nodes) and not nonpassing
    if recurrence_triggered:
        confirmed = False
        reason = "recurrence signature triggered in the same run"
    elif not compile_windows:
        reason = "no complete test.app_strict_verify Hermit compile window"
    elif not witnessed:
        reason = "compile occurred, but no e2e per-cell PASS activity overlapped it"
    elif nonpassing:
        reason = "overlap occurred, but not every started e2e consumer reached PASS"
    else:
        reason = (
            "positive confirmation: app_strict_verify compiled Hermit while e2e "
            "consumers executed, and every started e2e consumer reached PASS"
        )
    return {
        **base,
        "confirmed": confirmed,
        "reason": reason,
        "compile_windows": compile_windows,
        "witnessed_window_count": len(witnessed),
        "started_consumer_count": len(started_nodes),
        "started_consumers": started_nodes,
        "terminal_pass_count": sum(
            terminal.get(node, {}).get("status") == "PASS" for node in started_nodes
        ),
        "nonpassing_consumers": nonpassing,
        "recurrence_triggered": recurrence_triggered,
        "timing_basis": (
            "durable-log line order; per-line wall-clock timestamps unavailable"
        ),
    }


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
    """Capture ``validate-run`` wrapper output in its own append-only file.

    This path must be distinct from the service log passed through ``--log``.
    ``start_unit.py`` exclusively reserves the service path before systemd can
    append to it; opening that same path here would make every owner-watch run
    refuse itself before the unit existed.
    """
    log.parent.mkdir(parents=True, exist_ok=True)
    return log.open("a", encoding="utf-8")


def run_validate_observing_admission(
    command: Sequence[str | os.PathLike[str]],
    *,
    cwd: Path,
    launcher_log: Path,
    service_log: Path,
    run_record: Path,
    state_dir: Path,
    result: dict[str, Any],
    target: str,
) -> subprocess.CompletedProcess:
    """Run the detached producer while publishing its typed admission result."""
    if launcher_log.resolve() == service_log.resolve():
        raise RuntimeError("launcher capture and validation service log must be distinct")
    admission_seen = False
    with append_launcher_output(launcher_log) as output:
        process = subprocess.Popen(
            [str(item) for item in command],
            cwd=cwd,
            stdout=output,
            stderr=subprocess.STDOUT,
            text=True,
        )
        while process.poll() is None:
            if not admission_seen:
                admission = observe_admission_result(
                    run_record,
                    state_dir=state_dir,
                    result=result,
                    target=target,
                )
                admission_seen = admission is not None
            time.sleep(0.2)
        returncode = process.wait()
    if not admission_seen:
        observe_admission_result(
            run_record,
            state_dir=state_dir,
            result=result,
            target=target,
        )
    return subprocess.CompletedProcess(command, returncode)


def scorecard_files_are_current(checkout: Path) -> bool:
    script = checkout / "ci/compat-envelope/scorecard.rs"
    return script.is_file() and run([script, "check"], cwd=checkout).returncode == 0


def preserve_and_restore(
    checkout: Path,
    pre: dict[str, Any],
    recovery: Path,
) -> tuple[bool, dict[str, Any], bool]:
    """Recover post-run dirt without overwriting a valid generated scorecard."""
    post = tree_state(checkout)
    if post["whole_tree_hash"] == pre["whole_tree_hash"]:
        return True, post, False

    recovery.mkdir(parents=True, exist_ok=False)
    atomic_write(recovery / "pre.json", json.dumps(pre, indent=2, sort_keys=True) + "\n")
    atomic_write(recovery / "post.json", json.dumps(post, indent=2, sort_keys=True) + "\n")
    atomic_write(
        recovery / "tracked.patch",
        git(
            checkout,
            "diff",
            "--binary",
            "HEAD",
            "--",
            ".",
            *(f":(exclude){path}" for path in SCORECARD_PATHS),
        ),
    )

    # Moving HEAD is not a validation side effect and may be somebody else's
    # concurrent work. Preserve evidence and refuse to rewrite history.
    if post["head"] != pre["head"] or post["branch"] != pre["branch"]:
        return False, post, False

    generated_changed = bool(
        git(checkout, "diff", "--name-only", "HEAD", "--", *SCORECARD_PATHS).strip()
    )
    pre_untracked = {
        line[3:] for line in pre["dirty_paths"] if line.startswith("?? ")
    }
    untracked_raw = git(
        checkout, "ls-files", "--others", "--exclude-standard", "-z"
    )
    for relative in [
        item for item in untracked_raw.split("\0") if item and item not in pre_untracked
    ]:
        source = checkout / relative
        destination = recovery / "untracked" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(source), str(destination))

    changed_raw = git(checkout, "diff", "--name-only", "-z", "HEAD", "--")
    changed = [
        item
        for item in changed_raw.split("\0")
        if item and item not in SCORECARD_PATHS
    ]
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
    remaining = [
        line for line in restored["dirty_paths"] if line[3:] not in SCORECARD_PATHS
    ]
    clean = (
        restored["head"] == pre["head"]
        and restored["branch"] == pre["branch"]
        and restored["submodules"] == pre["submodules"]
        and remaining == pre["dirty_paths"]
    )
    scorecard_refused = generated_changed and not scorecard_files_are_current(checkout)
    return clean, restored, scorecard_refused


def render_message(result: dict[str, Any]) -> str:
    exception = result.get("known_dirty_exception") or {}
    lines = [
        "OWNER CHECKOUT PERIODIC VALIDATE: " + str(result["state"]),
        f"checkout: {result['checkout']}",
        f"state_root: {result.get('state_root') or 'unresolved'}",
        f"tool_root: {result.get('tool_root') or 'unresolved'}",
        f"tool_content_sha256: {result.get('tool_content_sha256') or 'unresolved'}",
        f"tool_parent_sha: {result.get('tool_parent_sha') or 'unresolved'}",
        f"tool_hermit_sha: {result.get('tool_hermit_sha') or 'unresolved'}",
        f"tool_agent_utils_sha: {result.get('tool_agent_utils_sha') or 'unresolved'}",
        "tool_bootstrap_sha256: "
        + str(result.get("tool_bootstrap_sha256") or "unresolved"),
        f"sha: {result.get('sha') or 'unresolved'}",
        f"source_tree: {result.get('source_tree') or 'unresolved'}",
        f"origin_main_sha: {result.get('origin_main_sha') or 'unresolved'}",
        "checkout_freshness: "
        + str(result.get("checkout_freshness") or CHECKOUT_FRESHNESS_UNKNOWN),
        f"checkout_behind: {result.get('checkout_behind')!r}",
        f"checkout_ahead: {result.get('checkout_ahead')!r}",
        f"verdict_qualifier: {result.get('verdict_qualifier') or 'unresolved'}",
        f"attempt: {result['attempt_id']}",
        f"reason: {result.get('reason') or 'none'}",
        f"executed_tests: {result.get('executed_tests')!r}",
        f"known_dirty_exception_applied: {exception.get('applied')!r}",
        f"known_dirty_exception_paths: {exception.get('paths')!r}",
        f"known_dirty_exception_make_assessment: {exception.get('make_assessment')!r}",
        f"canonical_qualified: {result.get('canonical_qualified')!r}",
        f"exception_adjusted_qualified: {result.get('exception_adjusted_qualified')!r}",
        f"canonical_verdict: {(result.get('canonical_status') or {}).get('verdict')!r}",
        f"canonical_receipt: {(result.get('canonical_status') or {}).get('receipt')!r}",
        f"admission_refusal: {result.get('admission_refusal')!r}",
        "commit_validation_state: "
        + str(result.get("commit_validation_state") or COMMIT_VALIDATION_UNKNOWN),
        f"commit_validation_sha: {result.get('commit_validation_sha') or 'unresolved'}",
        f"commit_validation_reason: {result.get('commit_validation_reason') or 'none'}",
        f"lock_admission: {result.get('lock_admission')!r}",
        f"lock_block_evidence: {result.get('lock_block_evidence')!r}",
        f"lock_holder: {result.get('lock_holder')!r}",
        f"privileged_tests_node: {result.get('privileged_tests_node')!r}",
        f"relink_race_recurrence: {result.get('relink_race_recurrence')!r}",
        f"relink_race_retirement: {result.get('relink_race_retirement')!r}",
        f"raw_failure_line: {result.get('raw_failure_line')!r}",
        f"pre_tree_hash: {result.get('pre_tree_hash') or 'unavailable'}",
        f"post_tree_hash: {result.get('post_tree_hash') or 'unavailable'}",
        f"dirty_paths_after: {result.get('dirty_paths_after')!r}",
        f"source_checkout_warnings: {result.get('source_checkout_warnings')!r}",
        f"validate_log: {result.get('validate_log') or 'none'}",
        f"launcher_log: {result.get('launcher_log') or 'none'}",
        f"result_json: {result['result_json']}",
    ]
    if result.get("recovery"):
        lines.append(f"recovery: {result['recovery']}")
    return "\n".join(lines) + "\n"


def notify(tool_root: Path, state_root: Path, message_file: Path) -> tuple[int, str]:
    completed = run(
        [
            "env",
            f"DEV_HERMIT_PARENT={state_root}",
            f"DEV_HERMIT_TOOL_ROOT={tool_root}",
            sys.executable,
            tool_root / "scripts/orc-hermit-msg.py",
            "--transport",
            "auto",
            "--message-file",
            message_file,
        ],
        cwd=tool_root,
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
    result.add_argument(
        "--admission-status-since",
        metavar="EPOCH_OR_RFC3339",
        help="print cheap ADMITTED/NOT_ADMITTED/UNKNOWN JSON; performs no git or network operation",
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    state_dir = args.state_dir.resolve()
    if args.admission_status_since is not None:
        rc, status = admission_status_since(state_dir, args.admission_status_since)
        print(json.dumps(status, sort_keys=True))
        return rc
    state_dir.mkdir(parents=True, exist_ok=True)
    with run_registry.owner_watch_population_lock(state_dir):
        return run_owner_check(args, state_dir)


def run_owner_check(args: argparse.Namespace, state_dir: Path) -> int:
    """Produce one owner-watch attempt while holding the population lock."""

    checkout = args.checkout.resolve()
    state_root = checkout.parent.resolve()
    tool_root = TOOL_ROOT
    # The timer and a manual spot check can begin in the same second. Include
    # microseconds and PID so their start/failure records can never overwrite
    # one another before the nonblocking singleton lock decides the winner.
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    attempt_id = f"{timestamp}-{os.getpid()}"
    result_path = state_dir / "results" / f"{attempt_id}.json"
    message_path = state_dir / "results" / f"{attempt_id}.md"
    invocation_ledger = state_dir / "invocations.jsonl"
    result: dict[str, Any] = {
        "schema_version": 2,
        "attempt_id": attempt_id,
        "started_at": utc_now(),
        "checkout": str(checkout),
        "state_root": str(state_root),
        "tool_root": str(tool_root),
        "tool_content_sha256": os.environ.get("DEV_HERMIT_TOOL_CONTENT_SHA256"),
        "tool_parent_sha": os.environ.get("DEV_HERMIT_TOOL_PARENT_SHA"),
        "tool_hermit_sha": os.environ.get("DEV_HERMIT_TOOL_HERMIT_SHA"),
        "tool_agent_utils_sha": os.environ.get("DEV_HERMIT_TOOL_AGENT_UTILS_SHA"),
        "tool_bootstrap_sha256": os.environ.get("DEV_HERMIT_TOOL_BOOTSTRAP_SHA256"),
        "result_json": str(result_path),
        "state": "CANNOT_START",
        "reason": "initializing",
        "executed_tests": None,
        "dirty_paths_after": None,
        "source_checkout_warnings": [],
        # UNKNOWN is deliberate: initialization has established neither a
        # failing commit nor a stale checkout. Each fact becomes known only
        # after its own authority is queried.
        "commit_validation_state": COMMIT_VALIDATION_UNKNOWN,
        "commit_validation_sha": None,
        "commit_validation_reason": "canonical validate-status not queried",
        "checkout_freshness": CHECKOUT_FRESHNESS_UNKNOWN,
        "lock_admission": {
            "state": "NOT_ATTEMPTED",
            "reason": "watch preflight has not launched validate-lock",
        },
        "known_dirty_exception": {
            "policy_path": KNOWN_EXCEPTION_PATH,
            "applied": False,
            "paths": [],
            "observed_status": [],
        },
    }
    ensure_admission_cursor(state_dir)
    append_jsonl(invocation_ledger, {**result, "event": "attempt-start"})

    lock_path = state_dir / "watch.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    lock = lock_path.open("w")

    def conclude() -> int:
        lock.close()
        return finish(
            tool_root,
            state_root,
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
            source_tree=pre["tree"],
            commit_validation_sha=pre["head"],
            pre_tree_hash=pre["whole_tree_hash"],
            dirty_paths_before=pre["dirty_paths"],
        )
        exception = known_exception_assessment(checkout, pre["dirty_paths"])
        result["known_dirty_exception"] = exception
        if pre["branch"] != "main":
            result["source_checkout_warnings"].append(
                f"ACTION: owner checkout branch is {pre['branch']!r}, expected 'main'; "
                f"validation will measure exact commit {pre['head']} in a fresh checkout"
            )
        if pre["dirty_paths"] and not exception["matched"]:
            result["source_checkout_warnings"].append(
                "ACTION: owner checkout has local changes; preserve or finish them when "
                "convenient. They are not validation inputs because the exact committed "
                f"target {pre['head']} will be materialized separately. Paths: "
                + ", ".join(pre["dirty_paths"])
            )
        if exception["matched"]:
            result["source_checkout_warnings"].append(
                "ACTION: owner checkout has the historical untracked validate symlink; "
                "it is not an input to the fresh materialized checkout"
            )
        for warning in result["source_checkout_warnings"]:
            print(f"owner-checkout-validate-watch: WARNING: {warning}", file=sys.stderr)

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
        revision = checkout_revision_context(
            checkout, head=pre["head"], origin_main=origin
        )
        result.update(
            origin_main_sha=origin,
            checkout_freshness=revision["freshness"],
            checkout_behind=revision["behind"],
            checkout_ahead=revision["ahead"],
            verdict_qualifier=revision["verdict_qualifier"],
        )

        if args.dry_run:
            result.update(state="NO_RESULT", reason="dry-run: launch deliberately skipped")
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = pre["dirty_paths"]
            return conclude()

        launched_at = dt.datetime.now(dt.timezone.utc)
        validate_log = state_dir / "logs" / f"{attempt_id}-validate-run.log"
        launcher_log = state_dir / "logs" / f"{attempt_id}-validate-run-launcher.log"
        validate_log.parent.mkdir(parents=True, exist_ok=True)
        result["launcher_log"] = str(launcher_log)
        unit = f"validate-owner-checkout-{attempt_id.lower()}"
        run_record = run_registry.record_path(state_root, unit)
        # Inherit validate-run's canonical full-validation deadline. A separate
        # 1800s override derived only 1619s and refused the committed 2400s lint
        # node before execution. The launcher owns the inner/outer ordering.
        command = [
            "env",
            f"DEV_HERMIT_PARENT={state_root}",
            f"DEV_HERMIT_TOOL_ROOT={tool_root}",
            tool_root / "ci-hub/ci-hub",
            "validate-run",
            "--checkout",
            checkout,
            "--materialize-target",
            "--agent",
            "owner-checkout-watch",
            "--target",
            pre["head"],
            "--unit",
            unit,
            "--log",
            validate_log,
            "--wait",
            "1",
            "--caller-poll-seconds",
            "5",
            "--",
            "full",
        ]
        launched = True
        result["lock_admission"] = {
            "state": "UNKNOWN",
            "reason": "validate-lock launched but no exact admission or refusal observed yet",
        }
        completed = run_validate_observing_admission(
            command,
            cwd=tool_root,
            launcher_log=launcher_log,
            service_log=validate_log,
            run_record=run_record,
            state_dir=state_dir,
            result=result,
            target=pre["head"],
        )
        result["validate_run_rc"] = completed.returncode
        admission_result = observe_admission_result(
            run_record,
            state_dir=state_dir,
            result=result,
            target=pre["head"],
        )
        lock_refused = (
            admission_result is not None
            and admission_result.get("state") == "refused"
            and admission_result.get("reason") == "validation-lock-refusal"
        )
        if lock_refused:
            # Text remains useful for an operator-facing holder attribution, but
            # it no longer decides whether admission was refused.
            lock_block = lock_block_evidence(validate_log)
            if lock_block is not None:
                result["lock_block_evidence"] = lock_block
                result["lock_holder"] = lock_holder_attribution(state_root, lock_block)

        typed_refused = (
            admission_result is not None
            and admission_result.get("state") == "refused"
        )
        if typed_refused:
            # This producer-owned result proves that no validation payload was
            # admitted. validate-run has already performed its liveness-checked
            # no-evidence cleanup before returning. Do not scan or publish the
            # canonical ledger and do not ask validate-status to perform its
            # remote absence probe: there cannot be an exact-attempt row, and a
            # prior commit verdict is not this rejected attempt's verdict.
            result["ledger_row_found"] = False
            result["validate_log"] = str(validate_log)
            result["admission_refusal"] = admission_refusal_evidence(launcher_log)
            if lock_refused:
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
            else:
                result.update(
                    state="NO_RESULT",
                    reason=(
                        "the typed validation admission result refused product work, "
                        "so this SHA is UNVALIDATED rather than failing and this "
                        "attempt has no validation verdict: "
                        f"{admission_result.get('reason')}"
                    ),
                )
            raise TypedAdmissionRefusal

        row = latest_attempt_row(
            canonical_ledger_rows(state_root),
            checkout=checkout,
            sha=pre["head"],
            launched_at=launched_at,
            run_id=unit,
        )
        result["ledger_row_found"] = row is not None
        if row is not None:
            result["executed_tests"] = row.get("executed_tests")
            result["coverage"] = row.get("coverage")
            result["validate_log"] = row.get("log_file") or str(validate_log)
            result["ledger_result"] = row.get("result")
            qualified, reason = qualify_row(
                tool_root, state_root, checkout, pre["head"], row
            )
            result["canonical_qualified"] = qualified
            result["canonical_qualification_reason"] = reason
            exception_qualified = False
            exception_reason = "not evaluated"
            if exception["applied"]:
                adjusted = dict(row)
                adjusted["tree_dirty"] = False
                adjusted["commit_anchored"] = True
                exception_qualified, exception_reason = qualify_row(
                    tool_root, state_root, checkout, pre["head"], adjusted
                )
            result["exception_adjusted_qualified"] = exception_qualified
            result["exception_adjusted_reason"] = exception_reason
        else:
            qualified, reason = False, "no exact-attempt ledger row"
            exception_qualified = False
            result["validate_log"] = str(validate_log)

        # Hard green is a property of this exact COMMIT, asked of its one
        # canonical evidence reader. Checkout freshness is deliberately absent
        # from this classification: a validated commit remains HARD_GREEN when
        # the working copy later falls behind. Conversely, this harness does
        # not infer soft-green landability; that requires the separate recorded
        # X-to-Y rebase defined by hermit-validation-authority.
        canonical = canonical_validate_status(tool_root, state_root, pre["head"])
        result["canonical_status"] = canonical
        canonical_green = (
            canonical["available"] and canonical["verdict"] == CANONICAL_GREEN_VERDICT
        )
        if canonical_green:
            result["commit_validation_state"] = COMMIT_VALIDATION_HARD_GREEN
            result["commit_validation_reason"] = (
                "canonical validate-status reports VALIDATED for exact commit "
                f"{pre['head']}; " + describe_receipt(canonical["receipt"])
            )
        elif not canonical["available"]:
            result["commit_validation_state"] = COMMIT_VALIDATION_UNKNOWN
            result["commit_validation_reason"] = (
                canonical["reason"] or "canonical validate-status unavailable"
            )
        else:
            result["commit_validation_state"] = COMMIT_VALIDATION_NOT_HARD_GREEN
            result["commit_validation_reason"] = (
                f"canonical validate-status verdict is {canonical['verdict']}, not "
                f"{CANONICAL_GREEN_VERDICT}, for exact commit {pre['head']}"
            )

        # Retain presentation evidence for old reports, but never let it select
        # a functional outcome. The current authority is `admission_result`.
        refusal = admission_refusal_evidence(launcher_log)
        result["admission_refusal"] = refusal

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
        recurrence = relink_race_recurrence_evidence(
            evidence_log,
            checkout,
            launched_at=launched_at,
        )
        result["relink_race_recurrence"] = recurrence
        result["relink_race_retirement"] = relink_race_retirement_evidence(
            evidence_log,
            recurrence_triggered=recurrence["triggered"],
        )

        if row is None and lock_refused:
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
        elif row is None and admission_result is None and (
            process_liveness := validation_run_process_liveness(run_record)
        ) != PROCESS_LIVENESS_DEAD:
            result.update(
                state="NO_RESULT",
                reason=(
                    "validation admission is UNKNOWN while its exact recorded process "
                    f"liveness is {process_liveness}; a prior commit verdict cannot "
                    "classify this attempt"
                ),
            )
        elif row is None and canonical_green:
            result.update(
                state="PASS_ALREADY_VALIDATED",
                reason=(
                    "GREEN: canonical validate-status already has terminal verdict "
                    f"{CANONICAL_GREEN_VERDICT} for exact SHA {pre['head']}; no result "
                    "for this attempt is being promoted into that authority; existing "
                    "receipt "
                    + describe_receipt(canonical["receipt"])
                    + "; "
                    + revision["verdict_qualifier"]
                ),
            )
        elif row is None and canonical["available"] and (
            canonical["verdict"] == CANONICAL_RED_VERDICT
        ):
            result.update(
                state="FAIL",
                reason=(
                    "canonical validate-status already has terminal verdict "
                    f"{CANONICAL_RED_VERDICT} for exact SHA {pre['head']}; no result "
                    "for this attempt is being promoted into that authority; "
                    + revision["verdict_qualifier"]
                ),
            )
        elif row is None and admission_result is None:
            result.update(
                state="NO_RESULT",
                reason=(
                    "validation admission is UNKNOWN because the run handle carries no "
                    "typed admission result; log text is presentation only"
                ),
            )
        elif completed.returncode != 0:
            wrapper_reason = (
                "canonical validate-status is VALIDATED for this exact run, "
                f"but validate-run returned {completed.returncode} after publishing "
                "that receipt; the validation verdict remains VALIDATED"
                if row is not None and canonical_green
                else f"make validate exit={completed.returncode}; {reason}"
            )
            result.update(
                state="FAIL",
                reason=wrapper_reason,
            )
        elif exception["applied"] and (qualified or exception_qualified):
            receipt_scope = (
                "canonical receipt also qualifies"
                if qualified and revision["current"]
                else "receipt is useful only for this exact checkout revision"
            )
            result.update(
                state="PASS_WITH_KNOWN_EXCEPTION",
                reason=(
                    "counted full run passed with the disclosed owner-symlink exception; "
                    + receipt_scope
                    + "; "
                    + revision["verdict_qualifier"]
                ),
            )
        elif qualified:
            result.update(
                state="PASS",
                reason=(
                    (
                        "authoritative counted full receipt qualifies; "
                        if revision["current"]
                        else "counted full receipt qualifies for this exact checkout only; "
                    )
                    + revision["verdict_qualifier"]
                ),
            )
        else:
            result.update(state="NO_RESULT", reason=reason)

    except TypedAdmissionRefusal:
        # The result was classified from the producer-owned typed refusal above.
        # Continue through the common tree observation and durable writer.
        pass
    except Exception as error:  # fail closed; the result below is the alarm
        result.update(state="CANNOT_START", reason=f"watch internal error: {error}")
    finally:
        # Validation executes in a fresh checkout, so this process has no
        # authority to restore the owner's mutable checkout. Record any change
        # observed across the run as a warning; it may be concurrent owner work.
        if launched and pre is not None:
            try:
                post = tree_state(checkout)
                result["post_tree_hash"] = post["whole_tree_hash"]
                result["dirty_paths_after"] = post["dirty_paths"]
                if post["whole_tree_hash"] != pre["whole_tree_hash"]:
                    result["source_checkout_warnings"].append(
                        "ACTION: owner checkout changed while isolated validation ran; "
                        "the watcher left it untouched"
                    )
            except Exception as observation_error:
                result["source_checkout_warnings"].append(
                    "ACTION: could not re-observe owner checkout after validation: "
                    f"{observation_error}"
                )
        elif pre is not None and "post_tree_hash" not in result:
            # Preflight refusals never mutate the checkout; still carry both
            # sides of the identity bracket in their durable result.
            result["post_tree_hash"] = pre["whole_tree_hash"]
            result["dirty_paths_after"] = pre["dirty_paths"]

    return conclude()


def finish(
    tool_root: Path,
    state_root: Path,
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
        notify_rc, notify_detail = notify(tool_root, state_root, message_path)
    result["notification_rc"] = notify_rc
    result["notification_detail"] = notify_detail
    atomic_write(result_path, json.dumps(result, indent=2, sort_keys=True) + "\n")
    append_jsonl(ledger, {**result, "event": "attempt-finish"})
    print(render_message(result), end="")
    # ⚠️ THIS USED TO RETURN BEFORE THE VERDICT WAS EVEN COMPUTED. `return
    # NOTIFY_FAILED` sat here and short-circuited the mapping below, so a run
    # whose report did not get out reported nothing about the tree it had just
    # validated. Both facts now travel: the verdict in the code, the delivery
    # outcome in whether it is offset.
    code = exit_code_for(result["state"], delivered=notify_rc == 0)
    if notify_rc != 0:
        print(
            "owner-checkout-validate-watch: NOTIFICATION FAILED: "
            f"{notify_detail}. The validation verdict is UNAFFECTED and is "
            f"{result['state']}; this exit code is {code}, which is that verdict "
            f"plus {UNDELIVERED_OFFSET} because the report did not reach anyone.",
            file=sys.stderr,
        )
    return code


if __name__ == "__main__":
    raise SystemExit(main())
