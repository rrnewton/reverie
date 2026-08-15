from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


sys.path.insert(0, str(Path(__file__).parent))
import review_rebase as R  # noqa: E402


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        capture_output=True,
        check=True,
    )
    return result.stdout.strip()


def commit(repo: Path, path: str, body: str, message: str) -> str:
    (repo / path).write_text(body)
    git(repo, "add", "--", path)
    git(repo, "commit", "-q", "-m", message)
    return git(repo, "rev-parse", "HEAD")


def init_repo(path: Path) -> Path:
    path.mkdir()
    git(path, "init", "-q", "-b", "main")
    git(path, "config", "user.name", "fixture")
    git(path, "config", "user.email", "fixture@example.invalid")
    return path


def report(head: str, bound_to: str, state: str = R.SUPERSEDED) -> dict:
    return {
        "approval_set_missing": False,
        "approval_sets_missing": 0,
        "critical_label_missing": False,
        "critical_lanes_missing": [],
        "rows": [
            {
                "number": 2261,
                "label": "passed-review-codex",
                "lane": "codex",
                "head": head,
                "bound_to": bound_to,
                "state": state,
            }
        ],
    }


def mechanical_fixture(tmp_path: Path) -> tuple[Path, str, str, str]:
    repo = init_repo(tmp_path / "repo")
    commit(repo, "common", "base\n", "base")
    git(repo, "checkout", "-q", "-b", "feature")
    approved = commit(repo, "feature", "reviewed\n", "reviewed patch")
    git(repo, "checkout", "-q", "main")
    current_base = commit(repo, "main-only", "main\n", "main advance")
    git(repo, "checkout", "-q", "feature")
    git(repo, "rebase", "-q", "main")
    current = git(repo, "rev-parse", "HEAD")
    return repo, approved, current_base, current


def test_mechanical_rebase_with_empty_range_diff_keeps_review(tmp_path: Path) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    verdict = R.evaluate(report(current, approved), repo, current_base, current)
    approval = verdict["approvals"][0]
    assert approval["basis"] == "MECHANICAL_REBASE"
    assert approval["proof"]["commits"] == 1
    assert len(approval["proof"]["range_diff"]) == 1
    assert " = " in approval["proof"]["range_diff"][0]


def test_semantic_conflict_resolution_is_refused(tmp_path: Path) -> None:
    repo = init_repo(tmp_path / "repo")
    commit(repo, "shared", "base\n", "base")
    git(repo, "checkout", "-q", "-b", "feature")
    approved = commit(repo, "shared", "feature\n", "reviewed patch")
    git(repo, "checkout", "-q", "main")
    current_base = commit(repo, "shared", "main\n", "main advance")
    git(repo, "checkout", "-q", "-b", "resolved", current_base)
    current = commit(repo, "shared", "semantic resolution\n", "resolve conflict")

    try:
        R.evaluate(report(current, approved), repo, current_base, current)
    except R.Refused as error:
        assert "range-diff is non-empty" in str(error)
        return
    raise AssertionError("semantic conflict resolution inherited the old review")


def test_current_head_approval_still_passes_directly(tmp_path: Path) -> None:
    repo, _approved, current_base, current = mechanical_fixture(tmp_path)
    verdict = R.evaluate(report(current, current, R.BOUND_CURRENT), repo, current_base, current)
    assert verdict["approvals"][0]["basis"] == R.BOUND_CURRENT


def test_undetermined_approval_refuses(tmp_path: Path) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    fixture = report(current, approved, "UNDETERMINED")
    fixture["rows"][0]["bound_to"] = None
    try:
        R.evaluate(fixture, repo, current_base, current)
    except R.Refused as error:
        assert "UNDETERMINED" in str(error)
        return
    raise AssertionError("an undetermined approval was accepted")


def test_cli_brackets_both_directions(tmp_path: Path) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    fixture = tmp_path / "approval.json"
    fixture.write_text(json.dumps(report(current, approved)))
    command = [
        sys.executable,
        str(Path(R.__file__)),
        "--approval-json",
        str(fixture),
        "--repo-checkout",
        str(repo),
        "--current-base",
        current_base,
        "--current-head",
        current,
    ]
    accepted = subprocess.run(command, text=True, capture_output=True, check=False)
    assert accepted.returncode == 0
    assert json.loads(accepted.stdout)["state"] == "REVIEW_APPLIES"

    fixture.write_text(json.dumps(report(current, "f" * 40)))
    refused = subprocess.run(command, text=True, capture_output=True, check=False)
    assert refused.returncode != 0
    assert "REFUSED" in refused.stderr or "ERROR" in refused.stderr
