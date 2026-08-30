#!/usr/bin/env python3
"""Reconcile retained CI signals for one exact Hermit commit.

This command does not create another receipt predicate. It asks the existing
``validate-status`` authority for the receipt verdict, then reports the other
stores at their actual scopes and refuses contradictions or missing evidence
that those stores would otherwise hide.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Sequence


CI_HUB = Path(__file__).resolve().parents[1]
ROOT = CI_HUB.parent
sys.path.insert(0, str(CI_HUB / "ledger"))
sys.path.insert(0, str(CI_HUB / "series"))
sys.path.insert(0, str(CI_HUB / "validate"))

import ledger as ledger_store  # noqa: E402
import run_registry  # noqa: E402
import series as canonical_series  # noqa: E402
import validate_rows as canonical_ledger  # noqa: E402


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
LOCAL_VALIDATION_CONTEXT = "Local validation"
TERMINAL_RUN_STATES = frozenset({"completed", "killed", "unknown", "failed"})
EXPECTED_SCORECARD_SOURCE = "../../series"
VALIDATE_STATUS_VERDICTS = frozenset({
    "VALIDATED",
    "FAILED",
    "TRUNCATED",
    "NEEDS-RERUN",
    "NO-RESULT",
    "NOT-VALIDATED",
    "LEDGER-LAGGING",
    "LEDGER-UNREADABLE",
})
GITHUB_STATUS_STATES = frozenset({"error", "failure", "pending", "success"})
RUN_HANDLE_REPOSITORIES = frozenset({"rrnewton/hermit", "rrnewton/reverie"})


class CrosscheckError(RuntimeError):
    """One source could not provide a trustworthy answer."""


def _run(
    command: Sequence[str],
    *,
    cwd: Path,
    timeout: int = 120,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(command),
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=dict(env) if env is not None else None,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise CrosscheckError(f"cannot run {' '.join(command)}: {error}") from error


def _json_object(raw: str, source: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise CrosscheckError(f"{source} emitted malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise CrosscheckError(f"{source} emitted {type(value).__name__}, expected object")
    return value


def git_text(repo: Path, *args: str) -> str:
    result = _run(["git", "--no-replace-objects", "-C", str(repo), *args], cwd=ROOT)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise CrosscheckError(f"git {' '.join(args)} failed in {repo}: {detail}")
    return result.stdout


def validate_status_report(root: Path, sha: str, repo: str) -> dict[str, Any]:
    env = dict(os.environ)
    env["CI_HUB_SKIP_SOFT_MAIN_WARNING"] = "1"
    result = _run(
        [
            str(root / "ci-hub" / "ci-hub"),
            "validate-status",
            "--sha",
            sha,
            "--repo",
            repo,
            "--json",
        ],
        cwd=root,
        timeout=180,
        env=env,
    )
    report = _json_object(result.stdout, "validate-status")
    report["scope"] = "canonical exact-commit receipt qualification"
    report["observed_exit_code"] = result.returncode
    if result.returncode != report.get("exit_code"):
        raise CrosscheckError(
            "validate-status process exit disagrees with its JSON exit_code: "
            f"{result.returncode} != {report.get('exit_code')!r}"
        )
    return report


def ledger_report(root: Path, sha: str, repo: str) -> tuple[dict[str, Any], list[dict]]:
    historical_rows = canonical_ledger.rows(root=root)
    repo_rows = [
        row for row in historical_rows if _repo_matches(row.get("repo"), repo)
    ]
    rows = canonical_ledger.current_validation_run_rows(repo_rows)
    exact = [row for row in rows if row.get("commit") == sha]
    return {
        "scope": "current canonical validation runs for the exact commit",
        "historical_rows": len(historical_rows),
        "repository_historical_rows": len(repo_rows),
        "logical_rows": len(rows),
        "exact_rows": len(exact),
        "exact_run_ids": sorted(
            str(row.get("run_id")) for row in exact if row.get("run_id")
        ),
    }, exact


def series_report(
    root: Path, sha: str
) -> tuple[dict[str, Any], canonical_series.PublishedSnapshot]:
    snapshot = canonical_series.published_snapshot(root)
    published = snapshot.rows
    spool = canonical_series.spool_dir(root)
    local_paths = sorted(spool.glob("*.jsonl")) + sorted(
        (spool / "published").glob("*.jsonl")
    )
    local = canonical_series.union(local_paths)
    combined = logical_series_rows(published, local)
    exact = [row for row in combined if (row.get("series") or {}).get("tree") == sha]
    return {
        "scope": "per-cell observations; not whole-run receipt authority",
        "published_ref": "refs/remotes/origin/main",
        "published_ref_sha": snapshot.commit,
        "published_rows": len(published),
        "local_spool_rows": len(local),
        "logical_rows": len(combined),
        "cross_store_duplicate_rows": len(published) + len(local) - len(combined),
        "exact_rows": len(exact),
    }, snapshot


def logical_series_rows(published: Iterable[dict], local: Iterable[dict]) -> list[dict]:
    """Use the store's authority for duplicate identity and conflict refusal."""
    return canonical_series.union_events([*published, *local])


def canonical_series_event_map(rows: Iterable[Mapping[str, Any]]) -> dict[str, str]:
    """Canonical full-row bodies keyed by the series store's stable identity."""
    by_event: dict[str, str] = {}
    for row in rows:
        event_id = row.get("event_id")
        if not isinstance(event_id, str) or not event_id:
            raise CrosscheckError("published series row has no event_id")
        canonical = json.dumps(row, sort_keys=True, separators=(",", ":"))
        prior = by_event.get(event_id)
        if prior is not None and prior != canonical:
            raise CrosscheckError(
                f"published series event_id {event_id!r} has conflicting bodies"
            )
        by_event[event_id] = canonical
    return by_event


def compare_series_snapshots(
    source_rows: Iterable[Mapping[str, Any]],
    current_rows: Iterable[Mapping[str, Any]],
) -> dict[str, Any]:
    """Classify one source frontier against the current published snapshot."""
    source = canonical_series_event_map(source_rows)
    current = canonical_series_event_map(current_rows)
    source_ids = set(source)
    current_ids = set(current)
    missing = sorted(source_ids - current_ids)
    added = sorted(current_ids - source_ids)
    replaced = sorted(
        event_id
        for event_id in source_ids & current_ids
        if source[event_id] != current[event_id]
    )
    if source == current:
        relation = "current"
    elif not missing and not replaced and source_ids < current_ids:
        relation = "stale"
    else:
        relation = "incorrect"
    return {
        "relation": relation,
        "source_events": len(source),
        "current_events": len(current),
        "added_events": len(added),
        "missing_events": len(missing),
        "replaced_events": len(replaced),
        "first_added_event_id": added[0] if added else None,
        "first_missing_event_id": missing[0] if missing else None,
        "first_replaced_event_id": replaced[0] if replaced else None,
    }


def _scorecard_total_rows(markdown: str) -> list[list[int]]:
    totals = []
    for line in markdown.splitlines():
        if not line.startswith("| **Total** |"):
            continue
        numbers = [int(value) for value in re.findall(r"\*\*(\d+)\*\*", line)]
        if numbers:
            totals.append(numbers)
    return totals


def _scorecard_prose_counts(markdown: str) -> dict[str, int | None]:
    disabled = re.search(
        r"current \*\*(\d+)\*\* manifest-disabled combinations as "
        r"\*\*Not applicable\*\*",
        markdown,
    )
    cross = re.search(
        r"current green/`never-measured` count is \*\*(\d+)\*\*, and the "
        r"current red/`measured-and-passed` count is \*\*(\d+)\*\*",
        markdown,
    )
    return {
        "not_applicable": int(disabled.group(1)) if disabled else None,
        "green_never_measured": int(cross.group(1)) if cross else None,
        "red_measured_and_passed": int(cross.group(2)) if cross else None,
    }


def scorecard_report(
    root: Path,
    hermit: Path,
    sha: str,
    *,
    published_series_snapshot: canonical_series.PublishedSnapshot,
) -> dict[str, Any]:
    cells = _json_object(
        git_text(hermit, "show", f"{sha}:ci/compat-envelope/cells.json"),
        "cells.json",
    )
    markdown = git_text(hermit, "show", f"{sha}:SCORECARD.md")
    detcore_tree = git_text(hermit, "rev-parse", f"{sha}:detcore").strip()
    entries = cells.get("cells")
    if not isinstance(entries, list):
        raise CrosscheckError("cells.json field cells is not an array")

    status_counts = Counter(str(cell.get("status")) for cell in entries)
    measurement_counts = Counter(str(cell.get("measurement")) for cell in entries)
    green_never_measured = sum(
        cell.get("status") == "green" and cell.get("measurement") == "never-measured"
        for cell in entries
    )
    red_measured_and_passed = sum(
        cell.get("status") == "red"
        and cell.get("measurement") == "measured-and-passed"
        for cell in entries
    )
    observed = [cell for cell in entries if cell.get("observations")]
    fresh = [
        cell
        for cell in observed
        if (cell.get("last_tested") or {}).get("detcore_tree") == detcore_tree
    ]
    green_observed = [cell for cell in observed if cell.get("status") == "green"]
    fresh_green = [
        cell
        for cell in green_observed
        if (cell.get("last_tested") or {}).get("detcore_tree") == detcore_tree
    ]

    totals = _scorecard_total_rows(markdown)
    status_total = totals[0] if totals else None
    measurement_total = next((row for row in totals if len(row) == 6), None)
    expected_status = [
        status_counts["green"],
        status_counts["red"],
        status_counts["not-applicable"],
        len(entries),
    ]
    expected_measurement = [
        measurement_counts["never-measured"],
        measurement_counts["measured-and-passed"],
        measurement_counts["measured-no-verdict"],
        measurement_counts["diverged-unlocated"],
        measurement_counts["diverged"],
        len(entries),
    ]
    prose_counts = _scorecard_prose_counts(markdown)
    expected_prose = {
        "not_applicable": status_counts["not-applicable"],
        "green_never_measured": green_never_measured,
        "red_measured_and_passed": red_measured_and_passed,
    }
    projection = cells.get("projection")
    if projection is not None and not isinstance(projection, dict):
        raise CrosscheckError("cells.json projection is not an object")
    projection = projection or {}
    source_commit = projection.get("source_commit")
    source_snapshot: canonical_series.PublishedSnapshot | None = None
    comparison: dict[str, Any] | None = None
    if source_commit is not None:
        if not isinstance(source_commit, str) or not SHA_RE.fullmatch(source_commit):
            raise CrosscheckError(
                "cells.json projection source_commit must be a lowercase 40-hex "
                f"object id, got {source_commit!r}"
            )
        source_snapshot = (
            published_series_snapshot
            if source_commit == published_series_snapshot.commit
            else canonical_series.published_snapshot_at(root, source_commit)
        )
        comparison = compare_series_snapshots(
            source_snapshot.rows, published_series_snapshot.rows
        )

    rows_read = projection.get("rows_read")
    rows_read_valid = (
        isinstance(rows_read, int) and not isinstance(rows_read, bool) and rows_read >= 0
    )
    return {
        "scope": "manifest selection and retained per-cell observations",
        "projection_rows_scope": "canonical event map at projection.source_commit",
        "schema": cells.get("schema"),
        "cell_count": len(entries),
        "status_counts": dict(sorted(status_counts.items())),
        "measurement_counts": dict(sorted(measurement_counts.items())),
        "markdown_status_total": status_total,
        "markdown_measurement_total": measurement_total,
        "status_table_matches_cells": status_total == expected_status,
        "measurement_table_matches_cells": measurement_total == expected_measurement,
        "prose_counts": prose_counts,
        "prose_matches_cells": prose_counts == expected_prose,
        "detcore_tree": detcore_tree,
        "observed_cells": len(observed),
        "fresh_observed_cells": len(fresh),
        "green_observed_cells": len(green_observed),
        "fresh_green_observed_cells": len(fresh_green),
        "projection": projection,
        "projection_source_matches": (
            projection.get("source") == EXPECTED_SCORECARD_SOURCE
        ),
        "projection_source_rows": (
            len(source_snapshot.rows) if source_snapshot is not None else None
        ),
        "projection_rows_read_valid": rows_read_valid,
        "projection_rows_read_matches_source": (
            None
            if source_snapshot is None
            else rows_read_valid and rows_read == len(source_snapshot.rows)
        ),
        "projection_series_comparison": comparison,
        "projection_series_relation": (
            comparison.get("relation") if comparison is not None else None
        ),
        "current_published_series_commit": published_series_snapshot.commit,
        "current_published_series_rows": len(published_series_snapshot.rows),
    }


def _record_identity(row: Mapping[str, Any]) -> set[str]:
    values = set()
    for field in ("run_id", "log_file"):
        value = row.get(field)
        if isinstance(value, str) and value:
            values.add(value)
            values.add(Path(value).name)
    return values


def _repo_matches(value: object, repo: str) -> bool:
    if repo == "rrnewton/hermit":
        return value in {None, "hermit", repo}
    if repo == "rrnewton/reverie":
        return value in {"reverie", repo}
    return value == repo


def _run_handle_attribution(raw: Mapping[str, Any]) -> tuple[str, str]:
    target = raw.get("target")
    if not isinstance(target, str) or not SHA_RE.fullmatch(target):
        raise ValueError("run handle target must be an exact lowercase forty-hex commit")
    repo = raw.get("repo")
    if not isinstance(repo, str) or repo not in RUN_HANDLE_REPOSITORIES:
        raise ValueError(
            "run handle repo must be rrnewton/hermit or rrnewton/reverie"
        )
    return target, repo


def run_handle_report(
    root: Path,
    sha: str,
    repo: str,
    ledger_rows: Iterable[Mapping[str, Any]],
) -> dict[str, Any]:
    ledger_identities: set[str] = set()
    for row in ledger_rows:
        ledger_identities.update(_record_identity(row))
    handles = []
    unreadable = []
    run_root = root / "ignored" / "validate" / "runs"
    for path in sorted(run_root.glob("*.json")):
        try:
            raw_json = json.loads(path.read_text())
            if not isinstance(raw_json, dict):
                raise ValueError("record is not an object")
        except (OSError, ValueError, json.JSONDecodeError) as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        try:
            raw_target, raw_repo = _run_handle_attribution(raw_json)
        except ValueError as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        # Attribution is validated before filtering. A well-formed handle for a
        # different commit or repository is out of scope; a missing or invalid
        # target/repo remains loud because it cannot be assigned safely.
        if raw_target != sha or raw_repo != repo:
            continue
        try:
            raw = raw_json
            projected = run_registry.projection(raw)
        except (RuntimeError, ValueError) as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        if projected.get("target") != raw_target or projected.get("repo") != raw_repo:
            unreadable.append({
                "path": str(path),
                "error": "typed projection changed the validated target/repo attribution",
            })
            continue
        if projected.get("kind") != run_registry.RunKind.VALIDATE.value:
            continue
        state = str(projected.get("state"))
        if state not in TERMINAL_RUN_STATES:
            continue
        admission = projected.get("admission_result")
        admission_state = admission.get("state") if isinstance(admission, dict) else None
        log = str(raw.get("log") or "")
        identifiers = {
            str(raw.get("unit") or "").removesuffix(".service"),
            log,
            Path(log).name,
        } - {"", "."}
        handles.append({
            "path": str(path),
            "unit": projected.get("unit"),
            "state": state,
            "result": raw.get("result"),
            "exit_code": raw.get("exit_code"),
            "admission": admission_state or "unknown-legacy",
            "ledger_event": bool(identifiers & ledger_identities),
        })
    return {
        "scope": "attempt evidence; never receipt authority",
        "run_root": str(run_root),
        "terminal_handles": handles,
        "unreadable_handles": unreadable,
        "unmatched_terminal_handles": [
            handle for handle in handles if not handle.get("ledger_event")
        ],
    }


def github_status_report(payload: Mapping[str, Any]) -> dict[str, Any]:
    statuses = payload.get("statuses")
    if not isinstance(statuses, list):
        raise CrosscheckError("GitHub combined status field statuses is not an array")
    exact = [
        status
        for status in statuses
        if isinstance(status, dict)
        and status.get("context") == LOCAL_VALIDATION_CONTEXT
    ]
    latest = max(
        exact,
        key=lambda status: (
            str(status.get("updated_at") or status.get("created_at") or ""),
            int(status.get("id") or 0),
        ),
        default=None,
    )
    if latest is not None and latest.get("state") not in GITHUB_STATUS_STATES:
        raise CrosscheckError(
            f"GitHub {LOCAL_VALIDATION_CONTEXT} has unsupported state "
            f"{latest.get('state')!r}"
        )
    return {
        "scope": "published projection of an exact local validation receipt",
        "combined_state": payload.get("state"),
        "total_count": payload.get("total_count"),
        "exact_context": LOCAL_VALIDATION_CONTEXT,
        "exact_context_count": len(exact),
        "published": latest is not None,
        "state": latest.get("state") if latest else None,
        "description": latest.get("description") if latest else None,
        "target_url": latest.get("target_url") if latest else None,
    }


def collect_github_status(root: Path, repo: str, sha: str) -> dict[str, Any]:
    command = ["gh", "api", f"repos/{repo}/commits/{sha}/status"]
    if shutil.which(os.environ.get("WITH_PROXY", "with-proxy")):
        command.insert(0, os.environ.get("WITH_PROXY", "with-proxy"))
    result = _run(command, cwd=root, timeout=60)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise CrosscheckError(f"GitHub commit-status query failed: {detail}")
    return github_status_report(_json_object(result.stdout, "GitHub commit status"))


def reconcile(report: Mapping[str, Any]) -> list[str]:
    issues: list[str] = []
    unavailable = report.get("unavailable") or {}
    for source, reason in sorted(unavailable.items()):
        issues.append(f"{source} unavailable: {reason}")

    validate = report.get("validate_status") or {}
    ledger = report.get("ledger") or {}
    verdict = validate.get("verdict")
    if verdict is not None and verdict not in VALIDATE_STATUS_VERDICTS:
        issues.append(f"validate-status emitted unsupported verdict {verdict!r}")
    exact_rows = ledger.get("exact_rows")
    if verdict in {"LEDGER-LAGGING", "LEDGER-UNREADABLE"}:
        issues.append(f"validate-status cannot establish the ledger: {verdict}")
    if exact_rows == 0 and verdict in {
        "VALIDATED",
        "FAILED",
        "NEEDS-RERUN",
        "TRUNCATED",
        "NO-RESULT",
    }:
        issues.append(f"validate-status says {verdict} but the canonical ledger has no row")
    # failed_record_count and withheld_nonpass_record_count explain subsets of
    # disqualified_count. Adding all four would double-count the exact rows.
    accounted = int(validate.get("qualifying_count") or 0) + int(
        validate.get("disqualified_count") or 0
    )
    if isinstance(exact_rows, int) and accounted != exact_rows:
        issues.append(
            "validate-status accounts for "
            f"{accounted} exact row(s), canonical ledger contains {exact_rows}"
        )

    scorecard = report.get("scorecard") or {}
    if scorecard and not scorecard.get("status_table_matches_cells"):
        issues.append("SCORECARD status table disagrees with cells.json")
    if scorecard and not scorecard.get("measurement_table_matches_cells"):
        issues.append("SCORECARD measurement table disagrees with cells.json")
    if scorecard and not scorecard.get("prose_matches_cells"):
        issues.append("SCORECARD prose disagrees with cells.json")
    if scorecard and not scorecard.get("projection_source_matches"):
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection names an unexpected series source: "
            f"{projection.get('source')!r}, expected {EXPECTED_SCORECARD_SOURCE!r}"
        )
    elif scorecard and scorecard.get("projection_series_relation") is None:
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection records no immutable source_commit, so its "
            f"{projection.get('rows_read')!r} row count cannot be classified as "
            "current, stale, or incorrect"
        )
    elif scorecard and not scorecard.get("projection_rows_read_matches_source"):
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection is INCORRECT: rows_read is a source-snapshot "
            f"invariant, but it records {projection.get('rows_read')!r} for "
            f"{scorecard.get('projection_source_rows')!r} canonical row(s) at "
            f"{projection.get('source_commit')}"
        )
    elif scorecard and scorecard.get("projection_series_relation") == "incorrect":
        projection = scorecard.get("projection") or {}
        comparison = scorecard.get("projection_series_comparison") or {}
        issues.append(
            "scorecard projection is INCORRECT: its recorded source snapshot is not "
            "an unchanged subset of the current canonical event map "
            f"(missing={comparison.get('missing_events')}, "
            f"replaced={comparison.get('replaced_events')}, "
            f"current-only={comparison.get('added_events')}; "
            f"source={projection.get('source_commit')}, "
            f"current={scorecard.get('current_published_series_commit')})"
        )
    elif scorecard and scorecard.get("projection_series_relation") == "stale":
        projection = scorecard.get("projection") or {}
        comparison = scorecard.get("projection_series_comparison") or {}
        issues.append(
            "scorecard projection is STALE: its recorded source event map is an "
            "unchanged strict subset of the current published series "
            f"({comparison.get('source_events')} -> {comparison.get('current_events')} "
            f"events; source={projection.get('source_commit')}, "
            f"current={scorecard.get('current_published_series_commit')})"
        )
    elif scorecard and scorecard.get("projection_series_relation") != "current":
        issues.append(
            "scorecard projection has unsupported series relation "
            f"{scorecard.get('projection_series_relation')!r}"
        )

    handles = report.get("run_handles") or {}
    for handle in handles.get("unreadable_handles") or []:
        issues.append(
            "run handle is unreadable, so its target cannot be cross-checked: "
            f"{handle.get('path')}: {handle.get('error')}"
        )
    for handle in handles.get("unmatched_terminal_handles") or []:
        issues.append(
            "terminal validation run has no ledger event: "
            f"{handle.get('unit') or handle.get('path')} "
            f"state={handle.get('state')} admission={handle.get('admission')}"
        )

    github = report.get("github_status") or {}
    if github.get("published"):
        state = github.get("state")
        if state == "success" and verdict != "VALIDATED":
            issues.append(
                f"GitHub {LOCAL_VALIDATION_CONTEXT} says success "
                f"but validate-status says {verdict}"
            )
        if state in {"failure", "error"} and verdict == "VALIDATED":
            issues.append(
                f"GitHub {LOCAL_VALIDATION_CONTEXT} says {state} "
                "but validate-status says VALIDATED"
            )
    return issues


def _collect(
    name: str,
    unavailable: dict[str, str],
    producer: Callable[[], Any],
) -> Any:
    try:
        return producer()
    except (
        CrosscheckError,
        ledger_store.LedgerError,
        OSError,
        RuntimeError,
        ValueError,
    ) as error:
        unavailable[name] = str(error)
        return None


def build_report(root: Path, sha: str, repo: str) -> dict[str, Any]:
    unavailable: dict[str, str] = {}
    report: dict[str, Any] = {
        "schema_version": 1,
        "sha": sha,
        "repo": repo,
        "unavailable": unavailable,
    }

    validate = _collect(
        "validate-status", unavailable, lambda: validate_status_report(root, sha, repo)
    )
    if validate is not None:
        report["validate_status"] = validate

    ledger_result = _collect(
        "canonical-ledger", unavailable, lambda: ledger_report(root, sha, repo)
    )
    exact_ledger_rows: list[dict] = []
    if ledger_result is not None:
        ledger, exact_ledger_rows = ledger_result
        report["ledger"] = ledger

    series_result = _collect("series", unavailable, lambda: series_report(root, sha))
    published_snapshot: canonical_series.PublishedSnapshot | None = None
    if series_result is not None:
        series, published_snapshot = series_result
        report["series"] = series

    if repo != "rrnewton/hermit":
        unavailable["scorecard"] = (
            "only rrnewton/hermit carries the compatibility scorecard"
        )
    elif published_snapshot is not None:
        scorecard = _collect(
            "scorecard",
            unavailable,
            lambda: scorecard_report(
                root,
                root / "hermit",
                sha,
                published_series_snapshot=published_snapshot,
            ),
        )
        if scorecard is not None:
            report["scorecard"] = scorecard
    else:
        unavailable["scorecard"] = "published series was unavailable"

    handles = _collect(
        "run-handles",
        unavailable,
        lambda: run_handle_report(root, sha, repo, exact_ledger_rows),
    )
    if handles is not None:
        report["run_handles"] = handles

    github = _collect(
        "github-status", unavailable, lambda: collect_github_status(root, repo, sha)
    )
    if github is not None:
        report["github_status"] = github

    report["issues"] = reconcile(report)
    report["result"] = "AGREE" if not report["issues"] else "DISAGREE"
    return report


def print_text(report: Mapping[str, Any]) -> None:
    print(f"CI SIGNAL CROSSCHECK {report['sha']} ({report['repo']})")
    validate = report.get("validate_status")
    if validate:
        print(
            "validate-status: "
            f"{validate.get('verdict')} "
            f"qualifying={validate.get('qualifying_count')} "
            f"failed={validate.get('failed_record_count')} "
            f"disqualified={validate.get('disqualified_count')}"
        )
    ledger = report.get("ledger")
    if ledger:
        print(
            f"ledger: exact={ledger.get('exact_rows')} "
            f"logical={ledger.get('logical_rows')}"
        )
    series = report.get("series")
    if series:
        print(
            "series: "
            f"exact={series.get('exact_rows')} logical={series.get('logical_rows')} "
            f"published={series.get('published_rows')} "
            f"cross-store-duplicates={series.get('cross_store_duplicate_rows')} "
            f"at {series.get('published_ref_sha')}"
        )
    scorecard = report.get("scorecard")
    if scorecard:
        projection = scorecard.get("projection") or {}
        print(
            "scorecard: "
            f"cells={scorecard.get('cell_count')} "
            f"observed={scorecard.get('observed_cells')} "
            f"fresh={scorecard.get('fresh_observed_cells')} "
            f"green-observed={scorecard.get('green_observed_cells')} "
            f"fresh-green={scorecard.get('fresh_green_observed_cells')} "
            f"projection-rows={projection.get('rows_read')} "
            f"projection-source={projection.get('source_commit')} "
            f"projection-source-rows={scorecard.get('projection_source_rows')} "
            f"projection-relation={scorecard.get('projection_series_relation')} "
            f"current-series-rows={scorecard.get('current_published_series_rows')} "
            f"current-series-commit={scorecard.get('current_published_series_commit')}"
        )
    handles = report.get("run_handles")
    if handles:
        print(
            "run-handles: "
            f"terminal={len(handles.get('terminal_handles') or [])} "
            f"without-ledger={len(handles.get('unmatched_terminal_handles') or [])}"
        )
    github = report.get("github_status")
    if github:
        if github.get("published"):
            print(f"github-status: {LOCAL_VALIDATION_CONTEXT}={github.get('state')}")
        else:
            print(
                "github-status: absent "
                f"(combined={github.get('combined_state')}, "
                f"total={github.get('total_count')})"
            )
    for source, reason in sorted((report.get("unavailable") or {}).items()):
        print(f"{source}: UNAVAILABLE: {reason}")
    for issue in report.get("issues") or []:
        print(f"DISAGREEMENT: {issue}")
    print(f"RECONCILIATION: {report.get('result')}")


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha", required=True, help="exact lowercase Hermit commit SHA")
    parser.add_argument("--repo", default="rrnewton/hermit")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if not SHA_RE.fullmatch(args.sha):
        parser.error("--sha must be an exact lowercase forty-hex commit")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    report = build_report(ROOT, args.sha, args.repo)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print_text(report)
    return 0 if report["result"] == "AGREE" else 1


if __name__ == "__main__":
    raise SystemExit(main())
