#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Produce the current, dated Hermit red-cell register.

``red`` is a scorecard selection state: it means that a manifest cell is not
selected by the full validation plan.  It is not a synonym for a failed run.
This reader keeps that state separate from the scorecard's measurement state
and dates both from the first-parent history in which the current value was
established on main.

The default command refreshes both remote main refs before resolving either of
them.  A saved report says when that refresh happened and when its 36-hour
quotation window ends; old output must be regenerated, not silently reused.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Sequence


CELL_PATH = "ci/compat-envelope/cells.json"
DEFAULT_HERMIT_REF = "refs/remotes/origin/main"
DEFAULT_PARENT_REF = "refs/remotes/origin/main"
MAX_AGE_HOURS = 36
VALIDATION_LIMIT = 12
MEASUREMENT_STATES = {
    "never-measured",
    "measured-and-passed",
    "measured-no-verdict",
    "diverged-unlocated",
    "diverged",
}


class RegisterError(RuntimeError):
    """The register could not establish a complete, current answer."""


@dataclass(frozen=True)
class Revision:
    commit: str
    committed_at: str


def run(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    timeout: int = 180,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(command),
        cwd=cwd,
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
    )


def checked(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    timeout: int = 180,
) -> str:
    completed = run(command, cwd=cwd, timeout=timeout)
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or "no output"
        raise RegisterError(f"{shlex.join(command)} failed: {detail}")
    return completed.stdout


def git(repo: Path, arguments: Sequence[str], *, timeout: int = 180) -> str:
    return checked(
        ["git", "-C", str(repo), "--no-replace-objects", *arguments],
        timeout=timeout,
    )


def refresh_main(repo: Path, repository: str) -> list[str]:
    proxy = shutil.which(os.environ.get("WITH_PROXY", "with-proxy"))
    command = [
        "git",
        "-C",
        str(repo),
        "fetch",
        "--quiet",
        "--force",
        "origin",
        "refs/heads/main:refs/remotes/origin/main",
    ]
    if proxy:
        command.insert(0, proxy)
    completed = run(command, timeout=90)
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or "no output"
        raise RegisterError(f"cannot refresh {repository} origin/main: {detail}")
    return command


def resolve_commit(repo: Path, ref: str) -> str:
    value = git(repo, ["rev-parse", "--verify", f"{ref}^{{commit}}"]).strip()
    if len(value) != 40 or any(
        character not in "0123456789abcdef" for character in value
    ):
        raise RegisterError(f"{ref} resolved to invalid commit {value!r}")
    return value


def load_cells(repo: Path, ref: str) -> dict[str, Any]:
    raw = git(repo, ["show", f"{ref}:{CELL_PATH}"], timeout=300)
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as error:
        raise RegisterError(f"{ref}:{CELL_PATH} is not valid JSON: {error}") from error
    if not isinstance(data, dict) or not isinstance(data.get("cells"), list):
        raise RegisterError(f"{ref}:{CELL_PATH} has no cells array")
    return data


def cell_key(cell: dict[str, Any]) -> tuple[str, str, str, str, str]:
    values = tuple(
        str(cell.get(field) or "")
        for field in ("lane", "category", "test", "mode", "backend")
    )
    if any(not value for value in values):
        raise RegisterError(f"cell has incomplete identity: {values!r}")
    return values  # type: ignore[return-value]


def cell_id(key: tuple[str, str, str, str, str]) -> str:
    _lane, _category, test, mode, backend = key
    return f"{test}/{mode}/{backend}"


def reason_value(cell: dict[str, Any]) -> Any:
    reason = cell.get("not_selected_by_full_reason")
    legacy_reason = cell.get("ci_disabled_reason")
    if reason is not None and legacy_reason is not None and reason != legacy_reason:
        raise RegisterError(
            f"{cell_id(cell_key(cell))} carries conflicting "
            "not_selected_by_full_reason and ci_disabled_reason values"
        )
    return reason if reason is not None else legacy_reason


def compact_state(
    data: dict[str, Any],
    wanted: set[tuple[str, str, str, str, str]],
) -> dict[tuple[str, str, str, str, str], dict[str, Any]]:
    result: dict[tuple[str, str, str, str, str], dict[str, Any]] = {}
    for raw in data["cells"]:
        if not isinstance(raw, dict):
            raise RegisterError("cells array contains a non-object")
        key = cell_key(raw)
        if key not in wanted:
            continue
        result[key] = {
            "status": raw.get("status"),
            "measurement": raw.get("measurement"),
            "reason": reason_value(raw),
        }
    return result


def path_revisions(repo: Path, ref: str) -> list[Revision]:
    if git(repo, ["rev-parse", "--is-shallow-repository"]).strip() == "true":
        raise RegisterError("Hermit checkout is shallow; red-cell dates cannot be established")
    output = git(
        repo,
        ["log", "--first-parent", "--format=%H%x09%cI", ref, "--", CELL_PATH],
    )
    revisions: list[Revision] = []
    for line in output.splitlines():
        if not line.strip():
            continue
        try:
            commit, committed_at = line.split("\t", 1)
        except ValueError as error:
            raise RegisterError(f"malformed Git history line: {line!r}") from error
        revisions.append(Revision(commit=commit, committed_at=committed_at))
    if not revisions:
        raise RegisterError(f"{ref} has no history for {CELL_PATH}")
    oldest_parent = run(
        [
            "git",
            "-C",
            str(repo),
            "--no-replace-objects",
            "rev-parse",
            "--verify",
            f"{revisions[-1].commit}^",
        ]
    )
    if oldest_parent.returncode == 0:
        parent = oldest_parent.stdout.strip()
        existed = run(
            [
                "git",
                "-C",
                str(repo),
                "--no-replace-objects",
                "cat-file",
                "-e",
                f"{parent}:{CELL_PATH}",
            ]
        )
        if existed.returncode == 0:
            raise RegisterError(
                f"first-parent history does not reach the addition of {CELL_PATH}"
            )
    return revisions


def derive_dates(
    repo: Path,
    ref: str,
    current_cells: Sequence[dict[str, Any]],
) -> dict[tuple[str, str, str, str, str], dict[str, Revision]]:
    """Date current status, measurement, and reason on main's first parent."""
    targets = {cell_key(cell): cell for cell in current_cells}
    if len(targets) != len(current_cells):
        raise RegisterError("current red cells contain duplicate identities")
    wanted = set(targets)
    revisions = path_revisions(repo, ref)
    unresolved = {
        key: {"status", "measurement", "reason"}
        for key in wanted
    }
    answer: dict[tuple[str, str, str, str, str], dict[str, Revision]] = {
        key: {} for key in wanted
    }

    newer = compact_state(load_cells(repo, revisions[0].commit), wanted)
    expected = compact_state({"cells": list(current_cells)}, wanted)
    if newer != expected:
        raise RegisterError(
            f"newest {CELL_PATH} history revision does not match resolved {ref}"
        )

    for index, revision in enumerate(revisions):
        if not any(unresolved.values()):
            break
        if index:
            newer = compact_state(load_cells(repo, revision.commit), wanted)
        if index + 1 < len(revisions):
            older = compact_state(load_cells(repo, revisions[index + 1].commit), wanted)
        else:
            older = {}
        for key, fields in unresolved.items():
            current = newer.get(key)
            if current is None:
                continue
            previous = older.get(key)
            for field in tuple(fields):
                target = expected[key][field]
                if current[field] == target and (
                    previous is None or previous.get(field) != target
                ):
                    answer[key][field] = revision
                    fields.remove(field)

    missing = {
        cell_id(key): sorted(fields)
        for key, fields in unresolved.items()
        if fields
    }
    if missing:
        sample = dict(list(sorted(missing.items()))[:5])
        raise RegisterError(
            f"could not date {len(missing)} current red cell(s); sample={sample}"
        )
    return answer


def observation_event_ids(cell: dict[str, Any]) -> list[str]:
    result: set[str] = set()
    observations = cell.get("observations") or []
    if not isinstance(observations, list):
        raise RegisterError(f"{cell_id(cell_key(cell))} observations is not an array")
    for observation in observations:
        if not isinstance(observation, dict):
            raise RegisterError(f"{cell_id(cell_key(cell))} has a non-object observation")
        event_ids = observation.get("event_ids") or []
        if not isinstance(event_ids, list):
            raise RegisterError(f"{cell_id(cell_key(cell))} event_ids is not an array")
        for event_id in event_ids:
            if not isinstance(event_id, str) or not event_id:
                raise RegisterError(f"{cell_id(cell_key(cell))} has an invalid event id")
            result.add(event_id)
    return sorted(result)


def load_series_module(parent: Path) -> Any:
    path = parent / "ci-hub" / "series" / "series.py"
    spec = importlib.util.spec_from_file_location("red_cell_register_series", path)
    if spec is None or spec.loader is None:
        raise RegisterError(f"cannot import series reader from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def series_evidence(
    parent: Path, parent_ref: str
) -> tuple[str, dict[str, dict[str, Any]]]:
    module = load_series_module(parent)
    try:
        snapshot = module.published_snapshot_at(parent, parent_ref)
    except Exception as error:
        raise RegisterError(f"cannot read the published results series: {error}") from error
    evidence: dict[str, dict[str, Any]] = {}
    for row in snapshot.rows:
        event_id = row.get("event_id")
        if not isinstance(event_id, str):
            continue
        series = row.get("series") or {}
        evidence[event_id] = {
            "event_id": event_id,
            "emitted_at": row.get("emitted_at"),
            "run_id": row.get("run_id"),
            "producer": row.get("producer"),
            "cell": series.get("cell"),
            "tree": series.get("tree"),
            "run_index": series.get("run_index"),
            "outcome": series.get("outcome"),
            "result": series.get("result"),
            "failure_class": series.get("failure_class"),
            "divergence": row.get("divergence"),
        }
    return snapshot.commit, evidence


def command_evidence(command: Sequence[str], parent: Path) -> dict[str, Any]:
    completed = run(command, cwd=parent, timeout=300)
    return {
        "command": shlex.join(command),
        "exit_code": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "stdout_sha256": hashlib.sha256(completed.stdout.encode()).hexdigest(),
        "stderr_sha256": hashlib.sha256(completed.stderr.encode()).hexdigest(),
    }


def active_task_text(
    parent: Path, active: dict[str, Any]
) -> tuple[dict[str, str], str | None]:
    state = active.get("state")
    if state not in {"ok", "drift"} or not isinstance(active.get("claimed"), list):
        return {}, str(active.get("reason") or active.get("error") or state or "unknown")
    details: dict[str, str] = {}
    for task in active["claimed"]:
        if not isinstance(task, dict) or not isinstance(task.get("id"), str):
            continue
        task_id = task["id"]
        shown = run(["tg", task_id, "-v"], cwd=parent, timeout=30)
        if shown.returncode != 0:
            return {}, f"tg {task_id} -v failed with exit {shown.returncode}"
        details[task_id] = shown.stdout
    return details, None


def task_matches(cell: dict[str, Any], task_id: str, text: str) -> str | None:
    key = cell_key(cell)
    full = cell_id(key)
    combined = f"{task_id}\n{text}"

    def mentions(identifier: str) -> bool:
        return re.search(
            rf"(?<![A-Za-z0-9_./-]){re.escape(identifier)}(?![A-Za-z0-9_./-])",
            combined,
        ) is not None

    if mentions(full):
        return "exact cell id"
    if mentions(key[2]):
        return "test id"
    return None


def current_work_for(
    cell: dict[str, Any],
    active: dict[str, Any],
    details: dict[str, str],
    unavailable: str | None,
) -> dict[str, Any]:
    if unavailable is not None:
        return {"state": "unknown", "reason": unavailable, "tasks": []}
    matches = []
    claimed_by_id = {
        task["id"]: task
        for task in active.get("claimed", [])
        if isinstance(task, dict) and isinstance(task.get("id"), str)
    }
    for task_id, text in sorted(details.items()):
        match = task_matches(cell, task_id, text)
        if match is None:
            continue
        task = claimed_by_id[task_id]
        matches.append(
            {
                "task": task_id,
                "title": task.get("title"),
                "owner": task.get("owner"),
                "match": match,
            }
        )
    return {
        "state": (
            "current claimed task mentions this cell"
            if matches
            else "no current claimed task mentions this cell"
        ),
        "tasks": matches,
    }


def iso_now() -> datetime:
    return datetime.now(timezone.utc).replace(microsecond=0)


def make_register(args: argparse.Namespace, invocation: str) -> dict[str, Any]:
    parent = args.parent.resolve()
    hermit = args.hermit.resolve()
    runtime_root = args.runtime_root.resolve()
    refresh_commands = [
        shlex.join(refresh_main(parent, "dev-hermit")),
        shlex.join(refresh_main(hermit, "Hermit")),
    ]

    verified_at = iso_now()
    parent_commit = resolve_commit(parent, DEFAULT_PARENT_REF)
    hermit_commit = resolve_commit(hermit, DEFAULT_HERMIT_REF)
    scorecard_blob = git(hermit, ["rev-parse", f"{hermit_commit}:{CELL_PATH}"]).strip()
    scorecard = load_cells(hermit, hermit_commit)
    red_cells = [
        cell
        for cell in scorecard["cells"]
        if isinstance(cell, dict) and cell.get("status") == "red"
    ]
    for cell in red_cells:
        measurement = cell.get("measurement")
        if measurement not in MEASUREMENT_STATES:
            raise RegisterError(
                f"{cell_id(cell_key(cell))} has unknown measurement state {measurement!r}"
            )
        if reason_value(cell) is None:
            raise RegisterError(f"{cell_id(cell_key(cell))} has no current red reason")

    dates = derive_dates(hermit, hermit_commit, red_cells)
    series_commit, events = series_evidence(parent, parent_commit)

    validate_command = [
        "./ci-hub/ci-hub",
        "validate-status",
        "--limit",
        str(VALIDATION_LIMIT),
    ]
    validation = command_evidence(validate_command, runtime_root)
    validation["checked_at"] = verified_at.isoformat().replace("+00:00", "Z")

    active_command = ["./ci-hub/ci-hub", "active-work", "--json"]
    active_result = command_evidence(active_command, runtime_root)
    try:
        active = json.loads(active_result["stdout"])
    except json.JSONDecodeError:
        active = {
            "state": "unknown",
            "error": "active-work did not return JSON",
        }
    details, unavailable = active_task_text(runtime_root, active)
    active_result["checked_at"] = verified_at.isoformat().replace("+00:00", "Z")
    active_result["state"] = active.get("state")
    if unavailable:
        active_result["reason"] = unavailable

    rows = []
    missing_series: set[str] = set()
    wrong_cell_series: list[tuple[str, str, Any]] = []
    for cell in sorted(red_cells, key=cell_key):
        key = cell_key(cell)
        event_ids = observation_event_ids(cell)
        missing_series.update(
            event_id for event_id in event_ids if event_id not in events
        )
        row_events = [events[event_id] for event_id in event_ids if event_id in events]
        wrong_cell_series.extend(
            (event_id, cell_id(key), events[event_id].get("cell"))
            for event_id in event_ids
            if event_id in events and events[event_id].get("cell") != cell_id(key)
        )
        status_revision = dates[key]["status"]
        measurement_revision = dates[key]["measurement"]
        reason_revision = dates[key]["reason"]
        rows.append(
            {
                "cell": {
                    "lane": key[0],
                    "category": key[1],
                    "test": key[2],
                    "mode": key[3],
                    "backend": key[4],
                    "id": cell_id(key),
                },
                "status": "red",
                "selection_state": "not selected",
                "status_established_at": status_revision.committed_at,
                "status_established_by_commit": status_revision.commit,
                "reason": reason_value(cell),
                "reason_established_at": reason_revision.committed_at,
                "reason_established_by_commit": reason_revision.commit,
                "measurement_state": cell["measurement"],
                "measurement_state_established_at": measurement_revision.committed_at,
                "measurement_state_established_by_commit": measurement_revision.commit,
                "measurement_evidence": {
                    "event_ids": event_ids,
                    "published_series_events": row_events,
                    "last_tested": cell.get("last_tested"),
                    "pre_series_corpus": bool(
                        (scorecard.get("projection") or {}).get("pre_series_corpus")
                    ),
                },
                "current_work": current_work_for(cell, active, details, unavailable),
                "produced_by": invocation,
                "source": {
                    "hermit_commit": hermit_commit,
                    "scorecard_blob": scorecard_blob,
                    "path": CELL_PATH,
                },
            }
        )

    if missing_series:
        raise RegisterError(
            f"{len(missing_series)} scorecard event id(s) are absent from the published "
            f"series at {series_commit}; sample={sorted(missing_series)[:5]}"
        )
    if wrong_cell_series:
        raise RegisterError(
            f"{len(wrong_cell_series)} scorecard event id(s) name a different cell "
            f"in the published series; sample={wrong_cell_series[:3]}"
        )

    counts: dict[str, int] = {}
    for row in rows:
        state = row["measurement_state"]
        counts[state] = counts.get(state, 0) + 1

    verified = verified_at.isoformat().replace("+00:00", "Z")
    quotable_until = (verified_at + timedelta(hours=MAX_AGE_HOURS)).isoformat().replace(
        "+00:00", "Z"
    )
    return {
        "schema": 1,
        "meaning": {
            "red": "manifest cell not selected by the full validation plan; not a failed result",
            "measurement_states": sorted(MEASUREMENT_STATES),
        },
        "verified_at": verified,
        "quotable_until": quotable_until,
        "max_age_hours": MAX_AGE_HOURS,
        "produced_by": invocation,
        "refresh_commands": refresh_commands,
        "sources": {
            "dev_hermit_commit": parent_commit,
            "hermit_commit": hermit_commit,
            "scorecard_path": CELL_PATH,
            "scorecard_blob": scorecard_blob,
            "scorecard_projection": scorecard.get("projection"),
            "published_series_commit": series_commit,
            "reader_root": str(runtime_root),
            "validation_reader": validation,
            "current_work_reader": active_result,
        },
        "counts": {"red": len(rows), "measurement_state": counts},
        "cells": rows,
    }


def parser() -> argparse.ArgumentParser:
    root = Path(__file__).resolve().parent.parent
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--parent", type=Path, default=root)
    result.add_argument("--hermit", type=Path, default=root / "hermit")
    result.add_argument(
        "--runtime-root",
        type=Path,
        default=root,
        help="parent checkout from which validate-status, active-work, and tg run",
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    parsed = parser().parse_args(argv)
    invocation = shlex.join(
        ["scripts/red-cell-register.py", *(argv if argv is not None else sys.argv[1:])]
    )
    try:
        register = make_register(parsed, invocation)
    except (RegisterError, subprocess.TimeoutExpired) as error:
        print(f"red-cell-register: REFUSED: {error}", file=sys.stderr)
        return 2
    json.dump(register, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
