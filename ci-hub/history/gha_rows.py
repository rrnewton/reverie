#!/usr/bin/env python3
"""Shared CSV contracts for GitHub Actions run and job history rows."""

from __future__ import annotations

import csv
import datetime as dt
import os
from pathlib import Path
from typing import Iterable, Literal, Mapping, TypedDict


LEGACY_RUN_COLUMNS = (
    "repo",
    "run_id",
    "run_attempt",
    "workflow_id",
    "workflow_name",
    "event",
    "head_branch",
    "head_sha",
    "pull_requests",
    "status",
    "conclusion",
    "created_at",
    "run_started_at",
    "updated_at",
    "queue_s",
    "run_s",
    "html_url",
    "display_title",
)

# Keep the historical columns in place and append measurements. Some readers
# outside this repository may still be positional even though the maintained
# readers below use names.
RUN_COLUMNS = (
    *LEGACY_RUN_COLUMNS,
    "runner_s",
    "jobs_n",
    "jobs_src",
)

JOB_COLUMNS = (
    "repo",
    "run_id",
    "run_attempt",
    "job_id",
    "name",
    "status",
    "conclusion",
    "started_at",
    "completed_at",
    "html_url",
)

JOBS_SRC_DERIVED = "jobs"
JOBS_SRC_ABSENT = "absent"
JobsSource = Literal["", "jobs", "absent"]


class GhaRunRow(TypedDict):
    # `status` and `conclusion` preserve GitHub's raw values. Consumers that
    # need a verdict use check_outcome.CheckOutcome, whose default arm keeps a
    # future provider value from becoming either green or red by accident.
    repo: str
    run_id: str
    run_attempt: str
    workflow_id: str
    workflow_name: str
    event: str
    head_branch: str
    head_sha: str
    pull_requests: str
    status: str
    conclusion: str
    created_at: str
    run_started_at: str
    updated_at: str
    queue_s: str
    run_s: str
    html_url: str
    display_title: str
    runner_s: str
    jobs_n: str
    jobs_src: JobsSource


class GhaJobRow(TypedDict):
    # The same raw-value rule applies to job status and conclusion.
    repo: str
    run_id: str
    run_attempt: str
    job_id: str
    name: str
    status: str
    conclusion: str
    started_at: str
    completed_at: str
    html_url: str


RunKey = tuple[str, str, str]
JobKey = tuple[str, str, str, str]
RunAttemptKey = tuple[str, str, str]


def _text(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, bool):
        raise ValueError("GitHub Actions history fields cannot be booleans")
    return str(value)


def _require_identifier(value: str, *, field: str) -> None:
    if not value:
        raise ValueError(f"GitHub Actions history {field} is missing")


def run_row(value: Mapping[str, object]) -> GhaRunRow:
    """Return one complete run row or refuse a malformed stored contract."""
    unknown = sorted(set(value) - set(RUN_COLUMNS))
    if unknown:
        raise ValueError(
            "GitHub Actions run row has unknown field(s): " + ", ".join(unknown)
        )
    row = GhaRunRow(**{field: _text(value.get(field)) for field in RUN_COLUMNS})
    if not row["repo"]:
        raise ValueError("GitHub Actions run row repo is missing")
    _require_identifier(row["run_id"], field="run_id")
    _require_identifier(row["run_attempt"], field="run_attempt")

    jobs_src = row["jobs_src"]
    runner_s = row["runner_s"]
    jobs_n = row["jobs_n"]
    if jobs_src == JOBS_SRC_DERIVED:
        if not runner_s.isdecimal():
            raise ValueError(
                "GitHub Actions run row jobs_src=jobs requires integer runner_s"
            )
        if not jobs_n.isdecimal() or int(jobs_n) <= 0:
            raise ValueError(
                "GitHub Actions run row jobs_src=jobs requires positive jobs_n"
            )
    elif jobs_src in ("", JOBS_SRC_ABSENT):
        if runner_s or jobs_n:
            raise ValueError(
                "GitHub Actions run row without job evidence must leave "
                "runner_s and jobs_n blank"
            )
    else:
        raise ValueError(
            f"GitHub Actions run row has unsupported jobs_src={jobs_src!r}"
        )
    return row


def job_row(value: Mapping[str, object]) -> GhaJobRow:
    """Return one complete job row or refuse a malformed stored contract."""
    unknown = sorted(set(value) - set(JOB_COLUMNS))
    if unknown:
        raise ValueError(
            "GitHub Actions job row has unknown field(s): " + ", ".join(unknown)
        )
    row = GhaJobRow(**{field: _text(value.get(field)) for field in JOB_COLUMNS})
    if not row["repo"]:
        raise ValueError("GitHub Actions job row repo is missing")
    _require_identifier(row["run_id"], field="run_id")
    _require_identifier(row["run_attempt"], field="run_attempt")
    _require_identifier(row["job_id"], field="job_id")
    return row


def run_key(row: Mapping[str, object]) -> RunKey:
    parsed = run_row(row)
    return parsed["repo"], parsed["run_id"], parsed["run_attempt"]


def job_key(row: Mapping[str, object]) -> JobKey:
    parsed = job_row(row)
    return (
        parsed["repo"],
        parsed["run_id"],
        parsed["run_attempt"],
        parsed["job_id"],
    )


def run_attempt_key(row: Mapping[str, object]) -> RunAttemptKey:
    repo = _text(row.get("repo"))
    run_id = _text(row.get("run_id"))
    run_attempt = _text(row.get("run_attempt"))
    if not repo:
        raise ValueError("GitHub Actions row repo is missing")
    _require_identifier(run_id, field="run_id")
    _require_identifier(run_attempt, field="run_attempt")
    return repo, run_id, run_attempt


def workflow_attempt_order(row: Mapping[str, object]) -> tuple[str, int, int]:
    """Ordering identity for repeated attempts of one workflow run."""
    parsed = run_row(row)
    return (
        parsed["created_at"],
        int(parsed["run_id"]) if parsed["run_id"].isdecimal() else 0,
        int(parsed["run_attempt"]) if parsed["run_attempt"].isdecimal() else 0,
    )


def _read_rows(
    path: str | Path,
    *,
    columns: tuple[str, ...],
    legacy_columns: tuple[str, ...] | None,
    parse,
) -> list[dict[str, str]]:
    if not os.path.isfile(path):
        return []
    with open(path, newline="", errors="replace") as stream:
        reader = csv.DictReader(stream)
        observed = tuple(reader.fieldnames or ())
        allowed = {columns}
        if legacy_columns is not None:
            allowed.add(legacy_columns)
        if observed not in allowed:
            missing = [field for field in columns if field not in observed]
            unknown = [field for field in observed if field not in columns]
            detail = []
            if missing:
                detail.append("missing " + ", ".join(missing))
            if unknown:
                detail.append("unknown " + ", ".join(unknown))
            if not detail:
                detail.append("column order differs")
            raise ValueError(
                f"GitHub Actions history {path} has an unsupported header: "
                + "; ".join(detail)
            )
        rows = []
        for line, raw in enumerate(reader, start=2):
            if None in raw:
                raise ValueError(
                    f"GitHub Actions history {path}:{line} has extra CSV values"
                )
            try:
                rows.append(parse(raw))
            except ValueError as error:
                raise ValueError(
                    f"GitHub Actions history {path}:{line}: {error}"
                ) from error
    return rows


def read_run_rows(path: str | Path) -> list[GhaRunRow]:
    return _read_rows(
        path,
        columns=RUN_COLUMNS,
        legacy_columns=LEGACY_RUN_COLUMNS,
        parse=run_row,
    )


def read_job_rows(path: str | Path) -> list[GhaJobRow]:
    return _read_rows(
        path,
        columns=JOB_COLUMNS,
        legacy_columns=None,
        parse=job_row,
    )


def load_run_store(path: str | Path) -> dict[RunKey, GhaRunRow]:
    rows: dict[RunKey, GhaRunRow] = {}
    for row in read_run_rows(path):
        key = run_key(row)
        if key in rows:
            raise ValueError(f"GitHub Actions run store has duplicate key {key!r}")
        rows[key] = row
    return rows


def load_job_store(path: str | Path) -> dict[JobKey, GhaJobRow]:
    rows: dict[JobKey, GhaJobRow] = {}
    for row in read_job_rows(path):
        key = job_key(row)
        if key in rows:
            raise ValueError(f"GitHub Actions job store has duplicate key {key!r}")
        rows[key] = row
    return rows


def fetched_attempt_keys(
    fetched: Mapping[str, object],
    run_rows: Mapping[RunKey, GhaRunRow],
    job_rows: Mapping[JobKey, GhaJobRow],
) -> set[RunAttemptKey]:
    """Resolve the sidecar to exact attempts, retaining job-row proof.

    The historical sidecar stored a bare run ID. That can be attributed without
    guessing only when the run store has exactly one attempt for that repository
    and run ID. A multi-attempt run remains unproven until fetched with
    `filter=all` and rewritten as `run_id:run_attempt`.
    """
    result = {run_attempt_key(row) for row in job_rows.values()}
    attempts_by_run: dict[tuple[str, str], set[str]] = {}
    for row in run_rows.values():
        attempts_by_run.setdefault((row["repo"], row["run_id"]), set()).add(
            row["run_attempt"]
        )
    for repo, raw_values in fetched.items():
        if not isinstance(repo, str) or not isinstance(raw_values, list):
            continue
        for raw in raw_values:
            if not isinstance(raw, str) or not raw:
                continue
            if ":" in raw:
                run_id, run_attempt = raw.rsplit(":", 1)
                if run_id and run_attempt:
                    result.add((repo, run_id, run_attempt))
                continue
            attempts = attempts_by_run.get((repo, raw), set())
            if len(attempts) == 1:
                result.add((repo, raw, next(iter(attempts))))
    return result


def _epoch(value: str) -> float | None:
    if not value:
        return None
    try:
        return dt.datetime.strptime(value[:19], "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=dt.timezone.utc
        ).timestamp()
    except ValueError:
        return None


def _elapsed_seconds(completed_at: str, started_at: str) -> int | None:
    completed = _epoch(completed_at)
    started = _epoch(started_at)
    if completed is None or started is None or completed < started:
        return None
    return round(completed - started)


def apply_job_measurements(
    run_rows: dict[RunKey, GhaRunRow],
    job_rows: Mapping[JobKey, GhaJobRow],
    fetched_attempts: set[RunAttemptKey],
) -> int:
    """Apply attempt-scoped runner seconds and the three job-evidence states."""
    by_attempt: dict[RunAttemptKey, list[GhaJobRow]] = {}
    for row in job_rows.values():
        by_attempt.setdefault(run_attempt_key(row), []).append(row)

    derived = 0
    for key, row in run_rows.items():
        spans = [
            span
            for job in by_attempt.get(run_attempt_key(row), [])
            if (span := _elapsed_seconds(job["completed_at"], job["started_at"]))
            is not None
        ]
        if spans:
            row["runner_s"] = str(sum(spans))
            row["jobs_n"] = str(len(spans))
            row["jobs_src"] = JOBS_SRC_DERIVED
            derived += 1
        elif run_attempt_key(row) in fetched_attempts:
            row["runner_s"] = ""
            row["jobs_n"] = ""
            row["jobs_src"] = JOBS_SRC_ABSENT
        else:
            row["runner_s"] = ""
            row["jobs_n"] = ""
            row["jobs_src"] = ""
        run_rows[key] = run_row(row)
    return derived


def _write_rows(
    path: str | Path,
    rows: Iterable[Mapping[str, object]],
    *,
    columns: tuple[str, ...],
    parse,
) -> None:
    target = str(path)
    temporary = target + ".tmp"
    with open(temporary, "w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=columns)
        writer.writeheader()
        for value in rows:
            writer.writerow(parse(value))
    os.replace(temporary, target)


def write_run_rows(path: str | Path, rows: Iterable[Mapping[str, object]]) -> None:
    _write_rows(path, rows, columns=RUN_COLUMNS, parse=run_row)


def write_job_rows(path: str | Path, rows: Iterable[Mapping[str, object]]) -> None:
    _write_rows(path, rows, columns=JOB_COLUMNS, parse=job_row)
