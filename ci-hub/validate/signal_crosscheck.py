#!/usr/bin/env python3
"""Reconcile retained CI signals for one exact Hermit commit.

This command does not create another receipt predicate. It asks the existing
``validate-status`` authority for the receipt verdict, then reports the other
stores at their actual scopes and refuses contradictions or missing evidence
that those stores would otherwise hide. When a rebased validation commit is no
longer present locally, only scorecard content may be read through the
tree-matched main commit reported by ``validate-status``; every evidence store
remains queried by the original validated SHA.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import stat
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, NamedTuple, Sequence


CI_HUB = Path(__file__).resolve().parents[1]
ROOT = CI_HUB.parent
sys.path.insert(0, str(CI_HUB / "ledger"))
sys.path.insert(0, str(CI_HUB / "series"))
sys.path.insert(0, str(CI_HUB / "validate"))

import ledger as ledger_store  # noqa: E402
import run_registry  # noqa: E402
import series as canonical_series  # noqa: E402
import service_result  # noqa: E402
import validate_rows as canonical_ledger  # noqa: E402


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
LOCAL_VALIDATION_CONTEXT = "Local validation"
TERMINAL_RUN_STATES = frozenset({"refused", "completed", "killed", "unknown", "failed"})
VALIDATE_STATUS_VERDICTS = frozenset({
    "VALIDATED",
    "FAILED",
    "TRUNCATED",
    "NEEDS-RERUN",
    "NO-RESULT",
    "TERMINATED",
    "NOT-VALIDATED",
    "LEDGER-LAGGING",
    "LEDGER-UNREADABLE",
})
GITHUB_STATUS_STATES = frozenset({"error", "failure", "pending", "success"})
RUN_HANDLE_REPOSITORIES = frozenset({"rrnewton/hermit", "rrnewton/reverie"})
CANONICAL_SERIES_PATH = "series"
PATH_EXPLICIT_CELLS_SCHEMA = 8
HISTORICAL_STATUS_COLUMNS = (
    "Backend",
    "Green",
    "Red",
    "Not applicable",
    "Total",
)
FULL_STATUS_COLUMNS = (
    "Backend",
    "Selected by full",
    "Not selected by full",
    "Not applicable",
    "In the manifest",
)
HISTORICAL_MEASUREMENT_COLUMNS = (
    "Status",
    "`never-measured`",
    "`measured-and-passed`",
    "`measured-no-verdict`",
    "`diverged-unlocated`",
    "`diverged`",
    "Total",
)
FULL_MEASUREMENT_COLUMNS = (
    *HISTORICAL_MEASUREMENT_COLUMNS[:-1],
    "In the manifest",
)
SCORECARD_STATUSES = frozenset({"green", "red", "not-applicable"})
SCORECARD_MEASUREMENTS = frozenset({
    "never-measured",
    "measured-and-passed",
    "measured-no-verdict",
    "diverged-unlocated",
    "diverged",
})


class CrosscheckError(RuntimeError):
    """One source could not provide a trustworthy answer."""


class Roots(NamedTuple):
    tool_root: Path
    state_root: Path
    git_common_dir: Path


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


def _git_environment() -> dict[str, str]:
    environment = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    return environment


def _resolved(path: Path, role: str) -> Path:
    try:
        return path.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise CrosscheckError(f"{role} is unavailable: {path}: {error}") from error


def _json_object(raw: str, source: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise CrosscheckError(f"{source} emitted malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise CrosscheckError(f"{source} emitted {type(value).__name__}, expected object")
    return value


def git_text(repo: Path, *args: str) -> str:
    result = _run(
        ["git", "--no-replace-objects", "-C", str(repo), *args],
        cwd=repo,
        env=_git_environment(),
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise CrosscheckError(f"git {' '.join(args)} failed in {repo}: {detail}")
    return result.stdout


def resolve_roots(tool_root: Path) -> Roots:
    """Bind immutable code to this checkout and mutable state to its primary."""
    tool_root = _resolved(tool_root, "tool root")

    tool_top = _resolved(
        Path(
            git_text(
                tool_root,
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
            ).strip()
        ),
        "tool repository top level",
    )
    if tool_top != tool_root:
        raise CrosscheckError(
            f"tool root {tool_root} is not the repository top level {tool_top}"
        )

    common_text = git_text(
        tool_root, "rev-parse", "--path-format=absolute", "--git-common-dir"
    ).strip()
    common_dir = _resolved(Path(common_text), "canonical Git common directory")
    if common_dir.name != ".git" or not common_dir.is_dir():
        raise CrosscheckError(
            f"canonical Git common directory is malformed: {common_dir}"
        )

    state_root = _resolved(common_dir.parent, "canonical state root")
    state_top = _resolved(
        Path(
            git_text(
                state_root,
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
            ).strip()
        ),
        "canonical state repository top level",
    )
    state_common = _resolved(
        Path(
            git_text(
                state_root,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ).strip()
        ),
        "canonical state Git common directory",
    )
    if state_top != state_root or state_common != common_dir:
        raise CrosscheckError(
            "canonical state root disagrees with the linked checkout's Git common "
            f"directory: state_root={state_root}, state_top={state_top}, "
            f"state_common={state_common}, tool_common={common_dir}"
        )
    return Roots(tool_root=tool_root, state_root=state_root, git_common_dir=common_dir)


def validate_status_report(
    tool_root: Path, state_root: Path, sha: str, repo: str
) -> dict[str, Any]:
    env = _git_environment()
    env["CI_HUB_SKIP_SOFT_MAIN_WARNING"] = "1"
    result = _run(
        [
            str(tool_root / "ci-hub" / "ci-hub"),
            "validate-status",
            "--sha",
            sha,
            "--repo",
            repo,
            "--columns",
            "main",
            "--json",
        ],
        cwd=state_root,
        timeout=180,
        env=env,
    )
    report = _json_object(result.stdout, "validate-status")
    if report.get("sha") != sha:
        raise CrosscheckError(
            "validate-status answered for a different commit: "
            f"requested={sha}, reported={report.get('sha')!r}"
        )
    if report.get("repo") != repo:
        raise CrosscheckError(
            "validate-status answered for a different repository: "
            f"requested={repo}, reported={report.get('repo')!r}"
        )
    report["scope"] = "canonical exact-commit receipt qualification"
    report["observed_exit_code"] = result.returncode
    if result.returncode != report.get("exit_code"):
        raise CrosscheckError(
            "validate-status process exit disagrees with its JSON exit_code: "
            f"{result.returncode} != {report.get('exit_code')!r}"
        )
    return report


def git_commit_available(repo: Path, sha: str) -> bool:
    """Whether this object database can dereference `sha` as a commit."""
    result = _run(
        [
            "git",
            "--no-replace-objects",
            "-C",
            str(repo),
            "rev-parse",
            "--verify",
            "--quiet",
            f"{sha}^{{commit}}",
        ],
        cwd=repo,
        env=_git_environment(),
    )
    if result.returncode == 0:
        resolved = result.stdout.strip()
        if not SHA_RE.fullmatch(resolved):
            raise CrosscheckError(
                f"git resolved {sha} to malformed commit object {resolved!r}"
            )
        return True
    if result.returncode == 1:
        return False
    detail = result.stderr.strip() or result.stdout.strip() or "no output"
    raise CrosscheckError(
        f"cannot test whether commit {sha} is available in {repo}: {detail}"
    )


def scorecard_content_commit(
    hermit: Path,
    requested_sha: str,
    validate_status: Mapping[str, Any] | None,
) -> tuple[str, str]:
    """Choose scorecard content without changing the evidence commit."""
    if git_commit_available(hermit, requested_sha):
        return requested_sha, "exact-sha"
    if validate_status is None:
        raise CrosscheckError(
            f"scorecard commit {requested_sha} is unavailable and validate-status "
            "provided no relationship to main"
        )
    if validate_status.get("sha") != requested_sha:
        raise CrosscheckError(
            "validate-status main relationship belongs to a different commit: "
            f"requested={requested_sha}, reported={validate_status.get('sha')!r}"
        )

    main_match = validate_status.get("main_match")
    main_sha = validate_status.get("main_sha")
    if main_match == "tree":
        if not isinstance(main_sha, str) or not SHA_RE.fullmatch(main_sha):
            raise CrosscheckError(
                "validate-status reported a tree match without an exact lowercase "
                f"forty-hex main SHA: {main_sha!r}"
            )
        if not git_commit_available(hermit, main_sha):
            raise CrosscheckError(
                f"validate-status tree-matched {requested_sha} to main {main_sha}, "
                "but that main commit is unavailable locally"
            )
        return main_sha, "tree"
    if main_match == "no-match":
        raise CrosscheckError(
            f"scorecard commit {requested_sha} is unavailable and validate-status "
            "found no matching tree on main"
        )
    if main_match == "exact-sha":
        raise CrosscheckError(
            f"validate-status reports {requested_sha} on main by exact SHA, but the "
            "commit is unavailable locally"
        )
    if main_match == "unverifiable":
        detail = validate_status.get("main_match_detail")
        suffix = f": {detail}" if isinstance(detail, str) and detail else ""
        raise CrosscheckError(
            f"scorecard commit {requested_sha} is unavailable and validate-status "
            f"could not verify its relationship to main{suffix}"
        )
    raise CrosscheckError(
        f"scorecard commit {requested_sha} is unavailable and validate-status "
        f"reported unsupported main relationship {main_match!r}"
    )


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
    tool_root: Path, state_root: Path, sha: str
) -> tuple[dict[str, Any], canonical_series.PublishedSnapshot]:
    snapshot = canonical_series.published_snapshot(tool_root)
    published = snapshot.rows
    spool = canonical_series.spool_dir(state_root)
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


def _markdown_row(line: str) -> tuple[str, ...] | None:
    stripped = line.strip()
    if not stripped.startswith("|") or not stripped.endswith("|"):
        return None
    return tuple(part.strip() for part in stripped[1:-1].split("|"))


def _scorecard_table_total(
    markdown: str, columns: tuple[str, ...]
) -> list[int] | None:
    """Read one exact table's bold total row, refusing ambiguity."""
    lines = markdown.splitlines()
    header_indexes = [
        index for index, line in enumerate(lines) if _markdown_row(line) == columns
    ]
    if len(header_indexes) != 1:
        return None
    header_index = header_indexes[0]
    if header_index + 1 >= len(lines):
        return None
    separator = _markdown_row(lines[header_index + 1])
    if (
        separator is None
        or len(separator) != len(columns)
        or any(re.fullmatch(r":?-{3,}:?", cell) is None for cell in separator)
    ):
        return None
    total: list[int] | None = None
    for candidate in lines[header_index + 2 :]:
        row = _markdown_row(candidate)
        if row is None:
            break
        if len(row) != len(columns) or not row[0] or total is not None:
            return None
        if row[0] != "**Total**":
            if any(re.fullmatch(r"\d+", cell) is None for cell in row[1:]):
                return None
            continue
        values: list[int] = []
        for cell in row[1:]:
            match = re.fullmatch(r"\*\*(\d+)\*\*", cell)
            if match is None:
                return None
            values.append(int(match.group(1)))
        total = values
    return total


def _scorecard_known_table_total(
    markdown: str, choices: tuple[tuple[str, ...], ...]
) -> tuple[tuple[str, ...] | None, list[int] | None]:
    lines = markdown.splitlines()
    headers = [
        columns
        for columns in choices
        if any(_markdown_row(line) == columns for line in lines)
    ]
    if len(headers) != 1:
        return None, None
    columns = headers[0]
    return columns, _scorecard_table_total(markdown, columns)


def _scorecard_status_total(
    markdown: str,
) -> tuple[tuple[str, ...] | None, list[int] | None]:
    return _scorecard_known_table_total(
        markdown, (HISTORICAL_STATUS_COLUMNS, FULL_STATUS_COLUMNS)
    )


def _scorecard_measurement_total(
    markdown: str,
) -> tuple[tuple[str, ...] | None, list[int] | None]:
    return _scorecard_known_table_total(
        markdown, (HISTORICAL_MEASUREMENT_COLUMNS, FULL_MEASUREMENT_COLUMNS)
    )


def _scorecard_status_columns_for_schema(value: object) -> tuple[str, ...] | None:
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        return None
    if value < PATH_EXPLICIT_CELLS_SCHEMA:
        return HISTORICAL_STATUS_COLUMNS
    return FULL_STATUS_COLUMNS


def _scorecard_measurement_columns_for_schema(
    value: object,
) -> tuple[str, ...] | None:
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        return None
    if value < PATH_EXPLICIT_CELLS_SCHEMA:
        return HISTORICAL_MEASUREMENT_COLUMNS
    return FULL_MEASUREMENT_COLUMNS


def _unique_prose_counts(markdown: str, pattern: str) -> tuple[int, ...] | None:
    matches = re.findall(pattern, markdown)
    if len(matches) != 1:
        return None
    values = matches[0]
    if isinstance(values, str):
        values = (values,)
    return tuple(int(value) for value in values)


def _scorecard_prose_counts(
    markdown: str, status_columns: tuple[str, ...] | None
) -> dict[str, int | None]:
    has_historical_wording = any(
        marker in markdown
        for marker in (
            "manifest-disabled combinations",
            "green/`never-measured`",
            "red/`measured-and-passed`",
        )
    )
    has_full_wording = any(
        marker in markdown
        for marker in (
            "cells in the manifest",
            "cells selected by full",
            "cells not selected by full",
        )
    )
    if has_historical_wording and has_full_wording:
        return {}
    if status_columns == HISTORICAL_STATUS_COLUMNS:
        if any(
            markdown.count(marker) != 1
            for marker in (
                "manifest-disabled combinations",
                "current green/`never-measured` count",
                "current red/`measured-and-passed` count",
            )
        ):
            return {}
        not_applicable = _unique_prose_counts(
            markdown,
            r"current \*\*(\d+)\*\* manifest-disabled combinations as\s+"
            r"\*\*Not applicable\*\*",
        )
        cross = _unique_prose_counts(
            markdown,
            r"current green/`never-measured` count is \*\*(\d+)\*\*, and the\s+"
            r"current red/`measured-and-passed` count is \*\*(\d+)\*\*",
        )
        return {
            "not_applicable": not_applicable[0] if not_applicable else None,
            "green_never_measured": cross[0] if cross else None,
            "red_measured_and_passed": cross[1] if cross else None,
        }
    if status_columns == FULL_STATUS_COLUMNS:
        if any(
            markdown.count(marker) != 1
            for marker in (
                "The count table includes all",
                "cells in the manifest; no row is omitted.",
                "Of the cells selected by full",
                "cells not selected by full",
            )
        ):
            return {}
        in_manifest = _unique_prose_counts(
            markdown,
            r"The count table includes all \*\*(\d+)\*\* cells in the manifest;\s+"
            r"no row is omitted\.",
        )
        cross = _unique_prose_counts(
            markdown,
            r"Of the cells selected by full, \*\*(\d+)\*\* have `never-measured`;\s+"
            r"of the cells not selected by full, \*\*(\d+)\*\* have "
            r"`measured-and-passed`\.",
        )
        return {
            "in_manifest": in_manifest[0] if in_manifest else None,
            "selected_by_full_never_measured": cross[0] if cross else None,
            "not_selected_by_full_measured_and_passed": (
                cross[1] if cross else None
            ),
        }
    return {}


def commit_is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    """Whether two exact commits establish one published-history relationship."""
    result = _run(
        [
            "git",
            "--no-replace-objects",
            "-C",
            str(repo),
            "merge-base",
            "--is-ancestor",
            ancestor,
            descendant,
        ],
        cwd=repo,
        env=_git_environment(),
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = result.stderr.strip() or result.stdout.strip() or "no output"
    raise CrosscheckError(
        f"cannot compare series source commit {ancestor} with published commit "
        f"{descendant}: {detail}"
    )


def git_tree_at(repo: Path, commit: str, path: str) -> str:
    """Return the exact Git tree object for `path` at `commit`."""
    tree = git_text(repo, "rev-parse", "--verify", f"{commit}:{path}").strip()
    if not SHA_RE.fullmatch(tree):
        raise CrosscheckError(
            f"Git tree for {path} at {commit} is not a lowercase forty-hex object id: "
            f"{tree!r}"
        )
    object_type = git_text(repo, "cat-file", "-t", tree).strip()
    if object_type != "tree":
        raise CrosscheckError(
            f"Git object for {path} at {commit} is {object_type!r}, expected 'tree'"
        )
    return tree


def scorecard_report(
    root: Path,
    hermit: Path,
    requested_sha: str,
    *,
    content_sha: str | None = None,
    content_matched_by: str = "exact-sha",
    published_series_snapshot: canonical_series.PublishedSnapshot,
) -> dict[str, Any]:
    content_sha = content_sha or requested_sha
    if content_matched_by not in {"exact-sha", "tree"}:
        raise CrosscheckError(
            f"unsupported scorecard content match {content_matched_by!r}"
        )
    cells = _json_object(
        git_text(hermit, "show", f"{content_sha}:ci/compat-envelope/cells.json"),
        "cells.json",
    )
    markdown = git_text(hermit, "show", f"{content_sha}:SCORECARD.md")
    detcore_tree = git_text(hermit, "rev-parse", f"{content_sha}:detcore").strip()
    entries = cells.get("cells")
    if not isinstance(entries, list):
        raise CrosscheckError("cells.json field cells is not an array")
    for index, cell in enumerate(entries):
        if not isinstance(cell, dict):
            raise CrosscheckError(f"cells.json cell {index} is not an object")
        status = cell.get("status")
        if not isinstance(status, str) or status not in SCORECARD_STATUSES:
            raise CrosscheckError(
                f"cells.json cell {index} has unsupported status {status!r}"
            )
        measurement = cell.get("measurement")
        if (
            not isinstance(measurement, str)
            or measurement not in SCORECARD_MEASUREMENTS
        ):
            raise CrosscheckError(
                f"cells.json cell {index} has unsupported measurement {measurement!r}"
            )

    status_counts = Counter(cell["status"] for cell in entries)
    measurement_counts = Counter(cell["measurement"] for cell in entries)
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

    cells_schema = cells.get("schema")
    expected_status_columns = _scorecard_status_columns_for_schema(cells_schema)
    expected_measurement_columns = _scorecard_measurement_columns_for_schema(
        cells_schema
    )
    status_columns, status_total = _scorecard_status_total(markdown)
    measurement_columns, measurement_total = _scorecard_measurement_total(markdown)
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
    prose_columns = (
        status_columns if status_columns == expected_status_columns else None
    )
    prose_counts = _scorecard_prose_counts(markdown, prose_columns)
    if expected_status_columns == HISTORICAL_STATUS_COLUMNS:
        expected_prose: dict[str, int] | None = {
            "not_applicable": status_counts["not-applicable"],
            "green_never_measured": green_never_measured,
            "red_measured_and_passed": red_measured_and_passed,
        }
    elif expected_status_columns == FULL_STATUS_COLUMNS:
        expected_prose = {
            "in_manifest": len(entries),
            "selected_by_full_never_measured": green_never_measured,
            "not_selected_by_full_measured_and_passed": red_measured_and_passed,
        }
    else:
        expected_prose = None
    path_explicit_cells_match = (
        expected_status_columns != FULL_STATUS_COLUMNS
        or all(
            "enabled" not in cell and "ci_disabled_reason" not in cell
            for cell in entries
        )
    )
    path_explicit_prose_matches = (
        expected_status_columns != FULL_STATUS_COLUMNS
        or re.search(r"\benabled\b", markdown, re.IGNORECASE) is None
    )
    projection = cells.get("projection")
    if projection is not None and not isinstance(projection, dict):
        raise CrosscheckError("cells.json projection is not an object")
    projection = projection or {}
    source = projection.get("source")
    source_commit = projection.get("source_commit")
    source_tree = projection.get("source_tree")
    source_snapshot: canonical_series.PublishedSnapshot | None = None
    comparison: dict[str, Any] | None = None
    source_commit_on_published_history: bool | None = None
    canonical_source_tree: str | None = None
    source_tree_matches: bool | None = None
    if source_commit is not None:
        if not isinstance(source_commit, str) or not SHA_RE.fullmatch(source_commit):
            raise CrosscheckError(
                "cells.json projection source_commit must be a lowercase 40-hex "
                f"object id, got {source_commit!r}"
            )
        if source_tree is not None and (
            not isinstance(source_tree, str) or not SHA_RE.fullmatch(source_tree)
        ):
            raise CrosscheckError(
                "cells.json projection source_tree must be a lowercase 40-hex "
                f"object id, got {source_tree!r}"
            )
        source_commit_on_published_history = commit_is_ancestor(
            root, source_commit, published_series_snapshot.commit
        )
        if source_commit_on_published_history and source_tree is not None:
            canonical_source_tree = git_tree_at(
                root, source_commit, CANONICAL_SERIES_PATH
            )
            source_tree_matches = source_tree == canonical_source_tree
        if (
            source_commit_on_published_history
            and source == CANONICAL_SERIES_PATH
            and source_tree_matches
        ):
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
        "requested_sha": requested_sha,
        "content_sha": content_sha,
        "content_matched_by": content_matched_by,
        "projection_rows_scope": "canonical event map at projection.source_commit",
        "schema": cells_schema,
        "cell_count": len(entries),
        "status_counts": dict(sorted(status_counts.items())),
        "measurement_counts": dict(sorted(measurement_counts.items())),
        "markdown_status_total": status_total,
        "markdown_measurement_total": measurement_total,
        "status_table_matches_cells": (
            status_columns == expected_status_columns
            and status_total == expected_status
            and path_explicit_cells_match
        ),
        "measurement_table_matches_cells": (
            measurement_columns == expected_measurement_columns
            and measurement_total == expected_measurement
        ),
        "prose_counts": prose_counts,
        "prose_matches_cells": (
            expected_prose is not None
            and prose_counts == expected_prose
            and path_explicit_prose_matches
        ),
        "detcore_tree": detcore_tree,
        "observed_cells": len(observed),
        "fresh_observed_cells": len(fresh),
        "green_observed_cells": len(green_observed),
        "fresh_green_observed_cells": len(fresh_green),
        "projection": projection,
        "projection_source_declared": (
            isinstance(source, str) and bool(source.strip())
        ),
        "projection_source_path_is_canonical": source == CANONICAL_SERIES_PATH,
        "projection_source_commit_on_published_history": (
            source_commit_on_published_history
        ),
        "projection_source_tree": source_tree,
        "canonical_source_tree": canonical_source_tree,
        "projection_source_tree_matches": source_tree_matches,
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


def _repo_matches(value: object, repo: str) -> bool:
    if repo == "rrnewton/hermit":
        return value in {None, "hermit", repo}
    if repo == "rrnewton/reverie":
        return value in {"reverie", repo}
    return value == repo


def _machine_shortname(value: object) -> str:
    """Normalize an explicit host without inferring a missing machine."""
    return str(value or "").split(".", 1)[0]


def _run_handle_id(value: object) -> str:
    """Return the durable run id encoded by a systemd validation unit."""
    if not isinstance(value, str) or not value.endswith(".service"):
        raise ValueError("run handle unit must end in .service")
    run_id = value.removesuffix(".service")
    if not run_id:
        raise ValueError("run handle unit must name a run")
    return run_id


def _wrapper_outcome(raw: Mapping[str, Any]) -> dict[str, Any] | None:
    """Validate the wrapper verdict when a terminal handle records one."""
    verdict = raw.get("canonical_verdict")
    exit_code = raw.get("wrapper_exit_code")
    if verdict is None and exit_code is None:
        return None
    if (
        not isinstance(verdict, str)
        or not isinstance(exit_code, int)
        or isinstance(exit_code, bool)
    ):
        raise ValueError(
            "run handle wrapper outcome must contain a verdict and integer exit code"
        )
    if verdict == "VALIDATED":
        expected = 0
    elif verdict == "FAILED":
        expected = 3
    elif verdict in {"TRUNCATED", "NEEDS-RERUN", "NO-RESULT", "NOT-VALIDATED"}:
        expected = 4
    else:
        expected = None
    if expected is None:
        raise ValueError(f"run handle wrapper outcome has unsupported verdict {verdict!r}")
    if exit_code != expected:
        raise ValueError(
            "run handle wrapper outcome disagrees: "
            f"verdict {verdict} requires exit {expected}, got {exit_code}"
        )
    return {"verdict": verdict, "exit_code": exit_code}


def _run_handle_target(raw: Mapping[str, Any]) -> str:
    target = raw.get("target")
    if not isinstance(target, str) or not SHA_RE.fullmatch(target):
        raise ValueError("run handle target must be an exact lowercase forty-hex commit")
    return target


def _run_handle_repo(raw: Mapping[str, Any]) -> str:
    repo = raw.get("repo")
    if not isinstance(repo, str) or repo not in RUN_HANDLE_REPOSITORIES:
        raise ValueError(
            "run handle repo must be rrnewton/hermit or rrnewton/reverie"
        )
    return repo


def _is_unknown_pre_admission_artifact(
    raw: Mapping[str, Any],
    projected: Mapping[str, Any],
    wrapper_outcome: Mapping[str, Any] | None,
    ledger_event_count: int,
    run_log_bytes: int | None,
) -> bool:
    """Classify old history with no attributable admission or outcome.

    Older launchers could leave an UNKNOWN handle before validate-lock published
    its process identity and admission result.  Missing attribution does not
    prove that the service never ran, so this remains explicitly unknown.  It
    is history rather than a contradictory outcome only when no execution or
    outcome evidence exists anywhere on the handle or in the ledger.
    """
    outcome_fields = (
        "exit_code",
        "signal",
        "executed_nodes",
        "executed_tests",
        "final_validate_status",
        "selection_mode",
        "scorecard_writeback",
        "measured_result",
        "commit_status_publication",
    )
    return (
        raw.get("producer") == run_registry.PRODUCER
        and projected.get("state") == run_registry.RunState.UNKNOWN.value
        and projected.get("process_identity") is None
        and projected.get("admission_result") is None
        and projected.get("host") is None
        and all(raw.get(field) is None for field in outcome_fields)
        and not raw.get("observed_safe_ci_cgroups")
        and wrapper_outcome is None
        and ledger_event_count == 0
        and run_log_bytes in {None, 0}
    )


def _retained_log_bytes(raw: Mapping[str, Any]) -> int | None:
    """Return the size of this handle's retained log, if trustworthy.

    ``start_unit`` resolves and exclusively reserves the recorded path as an
    empty regular file before it asks systemd to launch the service. A
    non-empty file therefore proves that the service wrote output; it cannot
    be classified with a handle that never launched. Refuse to infer through a
    symlink because current launchers record the resolved target instead.
    """
    raw_log = raw.get("log")
    if not isinstance(raw_log, str) or not raw_log:
        return None
    candidate = Path(raw_log)
    if not candidate.is_absolute():
        return None
    try:
        metadata = candidate.lstat()
    except (OSError, ValueError):
        return None
    if not stat.S_ISREG(metadata.st_mode):
        return None
    return metadata.st_size


def _validation_service_result_evidence(
    root: Path,
    handle_path: Path,
    handle_raw: Mapping[str, Any],
    result_path: Path,
    payload: bytes,
) -> service_result.RunEvidence | None:
    """Read a framework result only through its exact producer handle."""
    unit = handle_raw.get("unit")
    marker = handle_raw.get("service_result_schema")
    if not (
        handle_raw.get("producer") == run_registry.PRODUCER
        and isinstance(unit, str)
        and run_registry.record_path(root, unit) == handle_path
        and service_result.result_path(handle_path) == result_path
        and marker
        in {
            service_result.HISTORICAL_SCHEMA_VERSION,
            service_result.WRITEBACK_SCHEMA_VERSION,
            service_result.SCHEMA_VERSION,
        }
    ):
        return None
    target = handle_raw.get("target")
    if not isinstance(target, str) or SHA_RE.fullmatch(target) is None:
        return None
    evidence = service_result.read_framework_result_bytes(
        payload,
        expected_commit=target,
    )
    if evidence.service_result_schema != marker:
        raise RuntimeError(
            "validation-service-result-schema_version: "
            f"handle records {marker}, result records {evidence.service_result_schema}"
        )
    return evidence


def _run_handle_candidate_error(
    root: Path,
    path: Path,
    raw: Mapping[str, Any],
) -> str | None:
    """Return why a record cannot be handled by the existing handle path."""
    try:
        raw_target = _run_handle_target(raw)
        raw_repo = _run_handle_repo(raw)
        projected = run_registry.projection(raw)
        if projected.get("target") != raw_target or projected.get("repo") != raw_repo:
            raise ValueError(
                "typed projection changed the validated target/repo attribution"
            )
        run_id = _run_handle_id(projected.get("unit"))
        if run_registry.record_path(root, f"{run_id}.service") != path:
            raise ValueError("run handle unit does not map to its record path")
        _wrapper_outcome(raw)
    except (RuntimeError, ValueError) as error:
        return str(error)
    return None


def _candidate_validation_service_result(
    root: Path,
    path: Path,
    payload: bytes,
    raw: Mapping[str, Any],
) -> tuple[service_result.RunEvidence | None, str | None]:
    """Read a service-result candidate only through its sibling handle."""
    if not path.name.endswith(".service-result.json"):
        return None, None
    handle_error = _run_handle_candidate_error(root, path, raw)
    # A current producer handle has a closed, fully validated schema and wins
    # an exact filename collision. Legacy projection is intentionally
    # permissive, so a legacy-looking record beside a producer-owned result is
    # ambiguous until the result parser has had its say.
    if handle_error is None and raw.get("producer") == run_registry.PRODUCER:
        return None, None
    sibling = path.with_name(
        f"{path.name.removesuffix('.service-result.json')}.json"
    )
    try:
        sibling_raw = run_registry.read_record(sibling)
    except RuntimeError:
        return None, handle_error
    evidence = _validation_service_result_evidence(
        root, sibling, sibling_raw, path, payload
    )
    if evidence is None:
        return (None, None) if handle_error is None else (None, handle_error)
    return evidence, None


def _is_admitted_pre_plan_no_result(
    root: Path,
    path: Path,
    raw: Mapping[str, Any],
    projected: Mapping[str, Any],
    wrapper_outcome: Mapping[str, Any] | None,
) -> bool:
    """Return whether a typed admitted run stopped before any DAG node ran."""
    admission = projected.get("admission_result")
    if not (
        projected.get("state") == run_registry.RunState.COMPLETED.value
        and isinstance(admission, Mapping)
        and admission.get("state") == run_registry.AdmissionState.ADMITTED.value
        and raw.get("result") == "no-result"
        and raw.get("exit_code") == 75
        and wrapper_outcome is None
        and raw.get("executed_nodes") == 0
        and "executed_tests" in raw
        and raw.get("executed_tests") is None
        and raw.get("final_validate_status") == "COULD_NOT_RUN"
        and "selection_mode" in raw
        and raw.get("selection_mode") is None
        and raw.get("service_result_schema") == service_result.SCHEMA_VERSION
        and "scorecard_writeback" in raw
        and raw.get("scorecard_writeback") is None
        and raw.get("result_source") == "validation-service-result"
    ):
        return False
    result_path = service_result.result_path(path)
    try:
        payload = result_path.read_bytes()
    except OSError:
        return False
    try:
        evidence = _validation_service_result_evidence(
            root,
            path,
            raw,
            result_path,
            payload,
        )
    except RuntimeError:
        return False
    return (
        evidence is not None
        and evidence.profile == "full"
        and evidence.inner_exit == 75
        and evidence.executed_nodes == 0
        and evidence.executed_tests is None
        and evidence.final_validate_status == "COULD_NOT_RUN"
        and evidence.selection_mode is None
        and evidence.scorecard_writeback is None
    )


def run_handle_report(
    root: Path,
    sha: str,
    repo: str,
    ledger_rows: Iterable[Mapping[str, Any]],
) -> dict[str, Any]:
    run_registry.require_no_owner_watch_correction(root)
    ledger_run_ids: dict[str, list[Mapping[str, Any]]] = {}
    for row in ledger_rows:
        run_id = row.get("run_id")
        if (
            isinstance(run_id, str)
            and run_id
            and row.get("commit") == sha
            and _repo_matches(row.get("repo"), repo)
        ):
            ledger_run_ids.setdefault(run_id, []).append(row)
    matching_handles = []
    terminal_handles = []
    unreadable = []
    run_root = root / "ignored" / "validate" / "runs"
    for path in sorted(run_root.glob("*.json")):
        try:
            payload = path.read_bytes()
            raw_json = json.loads(payload)
            if not isinstance(raw_json, dict):
                raise ValueError("record is not an object")
        except (OSError, ValueError, json.JSONDecodeError) as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        try:
            result_evidence, candidate_handle_error = (
                _candidate_validation_service_result(
                    root,
                    path,
                    payload,
                    raw_json,
                )
            )
        except RuntimeError as error:
            unreadable.append({
                "path": str(path),
                "source": "validation-service-result",
                "error": str(error),
            })
            continue
        if result_evidence is not None:
            continue
        if candidate_handle_error is not None:
            unreadable.append({
                "path": str(path),
                "error": candidate_handle_error,
            })
            continue
        try:
            raw_target = _run_handle_target(raw_json)
        except ValueError as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        # A valid non-matching target proves this handle is out of scope without
        # guessing which repository an older schema omitted. An invalid target,
        # or an exact-target handle with no valid repository, remains loud.
        if raw_target != sha:
            continue
        try:
            raw_repo = _run_handle_repo(raw_json)
        except ValueError as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        if raw_repo != repo:
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
        try:
            run_id = _run_handle_id(projected.get("unit"))
            wrapper_outcome = _wrapper_outcome(raw)
        except ValueError as error:
            unreadable.append({"path": str(path), "error": str(error)})
            continue
        state = str(projected.get("state"))
        admission = projected.get("admission_result")
        admission_state = admission.get("state") if isinstance(admission, dict) else None
        admission_run_number = (
            admission.get("run_number") if isinstance(admission, dict) else None
        )
        projected_host = projected.get("host")
        machine_shortname = _machine_shortname(projected_host)
        ledger_events = ledger_run_ids.get(run_id, [])
        run_log_bytes = _retained_log_bytes(raw)
        non_run_refusal = (
            state == run_registry.RunState.REFUSED.value
            and raw.get("result") == "systemd-launch-refused"
        )
        no_receipt_attempt = (
            not ledger_events
            and _is_admitted_pre_plan_no_result(
                root,
                path,
                raw,
                projected,
                wrapper_outcome,
            )
        )
        unknown_pre_admission = _is_unknown_pre_admission_artifact(
            raw,
            projected,
            wrapper_outcome,
            len(ledger_events),
            run_log_bytes,
        )
        handle = {
            "path": str(path),
            "unit": projected.get("unit"),
            "run_id": run_id,
            "state": state,
            "result": raw.get("result"),
            "exit_code": raw.get("exit_code"),
            "producer": raw.get("producer"),
            "admission": admission_state,
            "run_number": admission_run_number,
            "host": projected_host,
            "machine_shortname": machine_shortname,
            "run_log_bytes": run_log_bytes,
            "wrapper_outcome": wrapper_outcome,
            "non_run_refusal": non_run_refusal,
            "no_receipt_attempt": no_receipt_attempt,
            "unknown_pre_admission": unknown_pre_admission,
            "ledger_event": (
                admission_state == run_registry.AdmissionState.ADMITTED.value
                and len(ledger_events) == 1
            ),
            "ledger_event_id": (
                ledger_events[0].get("record_id") if len(ledger_events) == 1 else None
            ),
            "ledger_event_count": len(ledger_events),
        }
        matching_handles.append(handle)
        if state in TERMINAL_RUN_STATES:
            terminal_handles.append(handle)
    report = {
        "scope": "attempt evidence; never receipt authority",
        "run_root": str(run_root),
        "matching_handles": matching_handles,
        "terminal_handles": terminal_handles,
        "unreadable_handles": unreadable,
        "unknown_pre_admission_handles": [
            handle
            for handle in terminal_handles
            if handle.get("unknown_pre_admission")
        ],
        "admitted_without_ledger_handles": [
            handle
            for handle in terminal_handles
            if handle.get("admission") == run_registry.AdmissionState.ADMITTED.value
            and not handle.get("ledger_event")
            and not handle.get("no_receipt_attempt")
        ],
        "terminal_handles_missing_admission": [
            handle
            for handle in terminal_handles
            if handle.get("producer") == run_registry.PRODUCER
            and handle.get("admission") is None
            and not handle.get("non_run_refusal")
            and not handle.get("unknown_pre_admission")
        ],
    }
    run_registry.require_no_owner_watch_correction(root)
    return report


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
    if scorecard and not scorecard.get("projection_source_declared"):
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection does not name its series source: "
            f"{projection.get('source')!r}"
        )
    elif scorecard and (scorecard.get("projection") or {}).get("source_commit") is None:
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection records no immutable source_commit, so its "
            f"{projection.get('rows_read')!r} row count cannot be classified as "
            "current, stale, or incorrect"
        )
    elif scorecard and scorecard.get("projection_source_tree") is None:
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection records no immutable source_tree, so its source "
            f"{projection.get('source')!r} cannot be verified; regenerate it"
        )
    elif (
        scorecard
        and scorecard.get("projection_source_commit_on_published_history") is False
    ):
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection source_commit is not on the current published "
            "parent history: "
            f"source={projection.get('source_commit')}, "
            f"current={scorecard.get('current_published_series_commit')}"
        )
    elif scorecard and not scorecard.get("projection_source_path_is_canonical"):
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection identifies a non-canonical series path: "
            f"{projection.get('source')!r}, expected {CANONICAL_SERIES_PATH!r}"
        )
    elif scorecard and not scorecard.get("projection_source_tree_matches"):
        projection = scorecard.get("projection") or {}
        issues.append(
            "scorecard projection source_tree does not match the canonical series "
            f"tree at {projection.get('source_commit')}: "
            f"recorded={scorecard.get('projection_source_tree')}, "
            f"canonical={scorecard.get('canonical_source_tree')}"
        )
    elif scorecard and scorecard.get("projection_series_relation") is None:
        issues.append(
            "scorecard projection has an immutable source identity but no series "
            "comparison result"
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
        if handle.get("source") == "validation-service-result":
            issues.append(
                "validation service result is unreadable or mismatched: "
                f"{handle.get('path')}: {handle.get('error')}"
            )
        else:
            issues.append(
                "run handle is unreadable, so its target cannot be cross-checked: "
                f"{handle.get('path')}: {handle.get('error')}"
            )
    for handle in handles.get("admitted_without_ledger_handles") or []:
        issues.append(
            "admitted terminal validation run has no ledger event: "
            f"{handle.get('unit') or handle.get('path')} "
            f"state={handle.get('state')} admission={handle.get('admission')}"
        )
    for handle in handles.get("terminal_handles_missing_admission") or []:
        run_log = handle.get("run_log_bytes")
        evidence = (
            f"; retained log has {run_log} byte(s)"
            if isinstance(run_log, int) and run_log > 0
            else ""
        )
        issues.append(
            "current-producer terminal validation run has start or outcome evidence "
            "but no typed admission_result: "
            f"{handle.get('unit') or handle.get('path')} state={handle.get('state')}"
            f"{evidence}"
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


def build_report(tool_root: Path, sha: str, repo: str) -> dict[str, Any]:
    unavailable: dict[str, str] = {}
    report: dict[str, Any] = {
        "schema_version": 1,
        "sha": sha,
        "repo": repo,
        "tool_root": str(tool_root.absolute()),
        "state_root": None,
        "git_common_dir": None,
        "unavailable": unavailable,
    }

    roots = _collect("state-root", unavailable, lambda: resolve_roots(tool_root))
    if roots is None:
        report["issues"] = reconcile(report)
        report["result"] = "DISAGREE"
        return report
    report["tool_root"] = str(roots.tool_root)
    report["state_root"] = str(roots.state_root)
    report["git_common_dir"] = str(roots.git_common_dir)

    validate = _collect(
        "validate-status",
        unavailable,
        lambda: validate_status_report(roots.tool_root, roots.state_root, sha, repo),
    )
    if validate is not None:
        report["validate_status"] = validate

    ledger_result = _collect(
        "canonical-ledger",
        unavailable,
        lambda: ledger_report(roots.state_root, sha, repo),
    )
    exact_ledger_rows: list[dict] = []
    if ledger_result is not None:
        ledger, exact_ledger_rows = ledger_result
        report["ledger"] = ledger

    series_result = _collect(
        "series",
        unavailable,
        lambda: series_report(roots.tool_root, roots.state_root, sha),
    )
    published_snapshot: canonical_series.PublishedSnapshot | None = None
    if series_result is not None:
        series, published_snapshot = series_result
        report["series"] = series

    if repo != "rrnewton/hermit":
        unavailable["scorecard"] = (
            "only rrnewton/hermit carries the compatibility scorecard"
        )
    elif published_snapshot is not None:
        def read_scorecard() -> dict[str, Any]:
            content_sha, content_matched_by = scorecard_content_commit(
                roots.tool_root / "hermit", sha, validate
            )
            return scorecard_report(
                roots.tool_root,
                roots.tool_root / "hermit",
                sha,
                content_sha=content_sha,
                content_matched_by=content_matched_by,
                published_series_snapshot=published_snapshot,
            )

        scorecard = _collect("scorecard", unavailable, read_scorecard)
        if scorecard is not None:
            report["scorecard"] = scorecard
    else:
        unavailable["scorecard"] = "published series was unavailable"

    handles = _collect(
        "run-handles",
        unavailable,
        lambda: run_handle_report(roots.state_root, sha, repo, exact_ledger_rows),
    )
    if handles is not None:
        report["run_handles"] = handles

    github = _collect(
        "github-status",
        unavailable,
        lambda: collect_github_status(roots.tool_root, repo, sha),
    )
    if github is not None:
        report["github_status"] = github

    report["issues"] = reconcile(report)
    report["result"] = "AGREE" if not report["issues"] else "DISAGREE"
    return report


def print_text(report: Mapping[str, Any]) -> None:
    print(f"CI SIGNAL CROSSCHECK {report['sha']} ({report['repo']})")
    print(
        f"roots: tool={report.get('tool_root')} state={report.get('state_root')} "
        f"git-common-dir={report.get('git_common_dir')}"
    )
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
            f"requested={scorecard.get('requested_sha')} "
            f"content={scorecard.get('content_sha')} "
            f"matched-by={scorecard.get('content_matched_by')} "
            f"cells={scorecard.get('cell_count')} "
            f"observed={scorecard.get('observed_cells')} "
            f"fresh={scorecard.get('fresh_observed_cells')} "
            f"green-observed={scorecard.get('green_observed_cells')} "
            f"fresh-green={scorecard.get('fresh_green_observed_cells')} "
            f"projection-rows={projection.get('rows_read')} "
            f"projection-source={projection.get('source_commit')} "
            f"projection-source-tree={projection.get('source_tree')} "
            f"projection-source-rows={scorecard.get('projection_source_rows')} "
            f"projection-relation={scorecard.get('projection_series_relation')} "
            f"current-series-rows={scorecard.get('current_published_series_rows')} "
            f"current-series-commit={scorecard.get('current_published_series_commit')}"
        )
    handles = report.get("run_handles")
    if handles:
        print(
            "run-handles: "
            f"matching={len(handles.get('matching_handles') or [])} "
            f"terminal={len(handles.get('terminal_handles') or [])} "
            "admitted-without-ledger="
            f"{len(handles.get('admitted_without_ledger_handles') or [])} "
            "no-receipt-attempts="
            f"{sum(bool(handle.get('no_receipt_attempt')) for handle in handles.get('terminal_handles') or [])} "
            "unknown-pre-admission="
            f"{len(handles.get('unknown_pre_admission_handles') or [])} "
            "terminal-without-admission="
            f"{len(handles.get('terminal_handles_missing_admission') or [])}"
        )
        for handle in handles.get("unknown_pre_admission_handles") or []:
            print(
                "run-handle-history: unknown pre-admission "
                f"{handle.get('unit') or handle.get('path')} "
                f"state={handle.get('state')} "
                "(no process identity, admission result, or structured "
                "execution/outcome evidence)"
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
