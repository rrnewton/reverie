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
    rows = canonical_ledger.rows(root=root)
    exact = [
        row
        for row in rows
        if row.get("commit") == sha and _repo_matches(row.get("repo"), repo)
    ]
    return {
        "scope": "canonical validation rows for the exact commit",
        "logical_rows": len(rows),
        "exact_rows": len(exact),
        "exact_run_ids": sorted(
            str(row.get("run_id")) for row in exact if row.get("run_id")
        ),
    }, exact


def series_report(root: Path, sha: str) -> tuple[dict[str, Any], list[dict]]:
    published = canonical_series.published_rows(root)
    spool = canonical_series.spool_dir(root)
    local_paths = sorted(spool.glob("*.jsonl")) + sorted(
        (spool / "published").glob("*.jsonl")
    )
    local = canonical_series.union(local_paths)
    combined = logical_series_rows(published, local)
    exact = [row for row in combined if (row.get("series") or {}).get("tree") == sha]
    parent_main = git_text(root, "rev-parse", "refs/remotes/origin/main").strip()
    return {
        "scope": "per-cell observations; not whole-run receipt authority",
        "published_ref": "refs/remotes/origin/main",
        "published_ref_sha": parent_main,
        "published_rows": len(published),
        "local_spool_rows": len(local),
        "logical_rows": len(combined),
        "cross_store_duplicate_rows": len(published) + len(local) - len(combined),
        "exact_rows": len(exact),
    }, published


def logical_series_rows(published: Iterable[dict], local: Iterable[dict]) -> list[dict]:
    """Use the store's authority for duplicate identity and conflict refusal."""
    return canonical_series.union_events([*published, *local])


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
    hermit: Path,
    sha: str,
    *,
    published_series_rows: int,
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
    return {
        "scope": "manifest selection and retained per-cell observations",
        "projection_rows_scope": (
            "readable rows at projection.refreshed_at; the source revision is not recorded"
        ),
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
        "projection_row_count_current": (
            projection.get("rows_read") == published_series_rows
        ),
        "current_published_series_rows": published_series_rows,
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
        # A malformed handle for another target cannot contradict this SHA.
        # Select by the two attribution fields before asking the typed reader
        # to validate the complete record; malformed JSON remains unassignable
        # and therefore loud.
        if raw_json.get("target") != sha or not _repo_matches(raw_json.get("repo"), repo):
            continue
        try:
            raw = run_registry.read_record(path)
            projected = run_registry.projection(raw)
        except (OSError, RuntimeError, ValueError) as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        if projected.get("target") != sha or not _repo_matches(projected.get("repo"), repo):
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
    elif scorecard and not scorecard.get("projection_row_count_current"):
        projection = scorecard.get("projection") or {}
        series = report.get("series") or {}
        issues.append(
            "scorecard projection is STALE, not arithmetically contradictory: "
            f"it read {projection.get('rows_read')!r} row(s) at "
            f"{projection.get('refreshed_at') or 'an unrecorded time'}, while the named "
            f"published series now has {scorecard.get('current_published_series_rows')!r} "
            f"logical row(s) at {series.get('published_ref_sha') or 'an unknown revision'}; "
            "the projection does not record the parent commit or event frontier needed to "
            "reload its historical input exactly"
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
    published_rows: list[dict] = []
    if series_result is not None:
        series, published_rows = series_result
        report["series"] = series

    if repo != "rrnewton/hermit":
        unavailable["scorecard"] = (
            "only rrnewton/hermit carries the compatibility scorecard"
        )
    elif series_result is not None:
        scorecard = _collect(
            "scorecard",
            unavailable,
            lambda: scorecard_report(
                root / "hermit", sha, published_series_rows=len(published_rows)
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
            f"current-series-rows={scorecard.get('current_published_series_rows')}"
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
