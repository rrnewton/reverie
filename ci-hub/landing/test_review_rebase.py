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


def comment_row(lane: str, head: str, state: str) -> dict:
    """A row as `approval_binding.py` reports it when no label carries it.

    A refusal is the NORMAL shape here: nobody attests a `CHANGES-REQUESTED-AT`,
    so a refusing lane has a comment row and no `passed-review-*` label.
    """
    return {
        "number": 2261,
        "label": None,
        "source": "comment",
        "lane": lane,
        "head": head,
        "bound_to": head if state == R.BOUND_CURRENT else None,
        "state": state,
    }


def comment_only_report(head: str, lane: str = "codex") -> dict:
    return {
        "approval_set_missing": False,
        "approval_sets_missing": 0,
        "critical_label_missing": False,
        "critical_lanes_missing": [],
        "rows": [comment_row(lane, head, R.BOUND_CURRENT)],
    }


def labelled_current(head: str) -> dict:
    fixture = report(head, head, state=R.BOUND_CURRENT)
    fixture["rows"][0]["source"] = "label"
    return fixture


def test_a_comment_refusal_blocks_even_with_no_label_to_carry_it() -> None:
    """⚠️ THE SKIP FAILED OPEN, AND ONLY IN THE PERMISSIVE DIRECTION.

    Not admitting a comment APPROVAL is conservative; not honouring a comment
    REFUSAL is permissive, and one `continue` did both. Measured 2026-08-26
    against main 9d4c998cfa5a: hermit#2680's latest codex marker is
    `CHANGES-REQUESTED-AT` at the CURRENT head, never withdrawn, and this gate
    authorized the PR.
    """
    head = "a" * 40
    fixture = labelled_current(head)
    fixture["rows"].append(comment_row("codex", head, "REFUSED"))
    try:
        R.evaluate(fixture, Path("."), "b" * 40, head)
    except R.Refused as error:
        assert "codex" in str(error)
        assert "REFUSED" in str(error)
    else:
        raise AssertionError("a live comment refusal must block the landing gate")


def test_a_stale_comment_refusal_blocks_because_a_self_withdrawal_is_not_a_discharge() -> None:
    """The hermit#2668 shape, and it defeats an anti-self-clearing rule.

    Its codex refusal was withdrawn by `agent(hermit-001)` -- the PR AUTHOR.
    `approval_binding.py` correctly refuses a withdrawal that is not the
    refusal's own author and keeps reporting `REFUSED_STALE`; this gate skipped
    the row and authorized anyway, so the rule it enforces was reachable around.
    """
    head = "a" * 40
    fixture = labelled_current(head)
    fixture["rows"].append(comment_row("codex", head, "REFUSED_STALE"))
    try:
        R.evaluate(fixture, Path("."), "b" * 40, head)
    except R.Refused as error:
        assert "REFUSED_STALE" in str(error)
    else:
        raise AssertionError("an undischarged stale refusal must block")


def test_a_current_comment_approval_admits_without_a_cache_label() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    verdict = R.evaluate(fixture, Path("."), "b" * 40, head)
    assert verdict["state"] == "REVIEW_APPLIES"
    assert [a["lane"] for a in verdict["approvals"]] == ["codex"]
    assert "label" not in verdict["approvals"][0]
    assert "source" not in verdict["approvals"][0]


def test_mechanical_carry_is_invariant_to_label_cache_metadata(tmp_path: Path) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    canonical = report(current, approved)
    canonical["rows"][0].update(label=None, source="comment")
    cached = report(current, approved)
    cached["rows"][0].update(label="passed-review-codex", source="label")
    absent = report(current, approved)
    absent["rows"][0].pop("label")
    absent["rows"][0].pop("source", None)

    canonical_verdict = R.evaluate(canonical, repo, current_base, current)
    assert canonical_verdict == R.evaluate(cached, repo, current_base, current)
    assert canonical_verdict == R.evaluate(absent, repo, current_base, current)
    approval = canonical_verdict["approvals"][0]
    assert approval["basis"] == "MECHANICAL_REBASE"
    assert "label" not in approval
    assert "source" not in approval


def test_an_empty_accepted_review_set_refuses() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    fixture["rows"] = [
        {
            "number": 2261,
            "label": "locally-validated",
            "source": "label",
            "lane": "local-validation",
            "head": head,
            "bound_to": head,
            "state": R.BOUND_CURRENT,
        }
    ]
    try:
        R.evaluate(fixture, Path("."), "b" * 40, head)
    except R.Refused as error:
        assert "no accepted review approval" in str(error)
    else:
        raise AssertionError("a report with no accepted review lane was authorized")


def test_configured_two_lane_review_refuses_when_one_lane_is_missing() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    fixture["critical_lanes_missing"] = ["claude"]
    try:
        R.evaluate(fixture, Path("."), "b" * 40, head)
    except R.Refused as error:
        assert "critical review families missing: claude" in str(error)
    else:
        raise AssertionError("one lane satisfied a configured two-lane review")


def test_cli_accepts_a_current_comment_approval_without_a_label(tmp_path: Path) -> None:
    repo, _approved, current_base, current = mechanical_fixture(tmp_path)
    fixture = tmp_path / "comment-approval.json"
    fixture.write_text(json.dumps(comment_only_report(current)))
    result = subprocess.run(
        [
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
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    verdict = json.loads(result.stdout)
    assert verdict["approvals"] == [
        {
            "basis": R.BOUND_CURRENT,
            "lane": "codex",
        }
    ]
