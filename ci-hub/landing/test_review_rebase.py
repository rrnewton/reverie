from __future__ import annotations

import argparse
import copy
import importlib.machinery
import importlib.util
import json
import subprocess
import sys
from pathlib import Path


sys.path.insert(0, str(Path(__file__).parent))
import review_rebase as R  # noqa: E402


REVIEW_ATTEST_PATH = Path(__file__).resolve().parents[1] / "bin" / "review-attest"
REVIEW_ATTEST_LOADER = importlib.machinery.SourceFileLoader(
    "review_attest_for_rebase_test", str(REVIEW_ATTEST_PATH)
)
REVIEW_ATTEST_SPEC = importlib.util.spec_from_loader(
    REVIEW_ATTEST_LOADER.name, REVIEW_ATTEST_LOADER
)
assert REVIEW_ATTEST_SPEC is not None
REVIEW_ATTEST = importlib.util.module_from_spec(REVIEW_ATTEST_SPEC)
REVIEW_ATTEST_LOADER.exec_module(REVIEW_ATTEST)

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
        "critical_required": False,
        "critical_lanes_missing": [],
        "critical_attestations": [],
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
    fixture = report(current, approved, R.UNDETERMINED)
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
        "critical_required": False,
        "critical_lanes_missing": [],
        "critical_attestations": [],
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
    fixture["rows"].append(comment_row("codex", head, R.REFUSED))
    fixture["rows"].append(comment_row("codex", head, R.UNDETERMINED))
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
    fixture["rows"].append(comment_row("codex", head, R.REFUSED_STALE))
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


def current_critical_attestation(head: str) -> dict:
    return {
        "comment_id": "900001",
        "url": "https://github.com/rrnewton/hermit/pull/2261#issuecomment-900001",
        "github_actor": "rrnewton",
        "permission": "write",
        "authorized": True,
        "version_time": "2026-09-05T12:00:00+00:00",
        "head": head,
        "critical_class": "hermit-syscall",
        "error": None,
    }


def test_current_critical_attestation_requires_two_lanes() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    fixture["critical_attestations"] = [current_critical_attestation(head)]
    try:
        R.evaluate(fixture, Path("."), "b" * 40, head)
    except R.Refused as error:
        assert "critical review families missing: claude" in str(error)
    else:
        raise AssertionError("one lane satisfied a configured two-lane review")


def test_ordinary_review_ignores_an_undetermined_optional_lane() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    fixture["rows"].append(comment_row("claude", head, R.UNDETERMINED))
    verdict = R.evaluate(fixture, Path("/unused"), "b" * 40, head)
    assert verdict["approvals"] == [
        {"lane": "codex", "basis": R.BOUND_CURRENT}
    ]


def test_critical_review_requires_an_undetermined_second_lane() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    fixture["critical_attestations"] = [current_critical_attestation(head)]
    fixture["rows"].append(comment_row("claude", head, R.UNDETERMINED))
    try:
        R.evaluate(fixture, Path("/unused"), "b" * 40, head)
    except R.Refused as error:
        assert "critical review families missing: claude" in str(error)
    else:
        raise AssertionError("a critical review accepted an undetermined second lane")


def test_ordinary_review_ignores_a_superseded_optional_lane() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    stale = comment_row("claude", head, R.SUPERSEDED)
    stale["bound_to"] = "f" * 40
    fixture["rows"].append(stale)
    verdict = R.evaluate(fixture, Path("/unused"), "b" * 40, head)
    assert verdict["approvals"] == [
        {"lane": "codex", "basis": R.BOUND_CURRENT}
    ]


def test_critical_review_carries_a_superseded_second_lane(tmp_path: Path) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    fixture = comment_only_report(current)
    fixture["critical_attestations"] = [current_critical_attestation(current)]
    stale = comment_row("claude", current, R.SUPERSEDED)
    stale["bound_to"] = approved
    fixture["rows"].append(stale)
    verdict = R.evaluate(fixture, repo, current_base, current)
    assert [approval["lane"] for approval in verdict["approvals"]] == [
        "claude",
        "codex",
    ]
    assert verdict["approvals"][0]["basis"] == "MECHANICAL_REBASE"
    assert verdict["approvals"][1]["basis"] == R.BOUND_CURRENT


def test_critical_review_refuses_an_uncarried_superseded_second_lane(
    tmp_path: Path,
) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    current = commit(repo, "feature", "reviewed\nchanged\n", "semantic change")
    fixture = comment_only_report(current)
    fixture["critical_attestations"] = [current_critical_attestation(current)]
    stale = comment_row("claude", current, R.SUPERSEDED)
    stale["bound_to"] = approved
    fixture["rows"].append(stale)
    try:
        R.evaluate(fixture, repo, current_base, current)
    except R.Refused as error:
        detail = str(error)
        assert "critical review families missing: claude" in detail
        assert "mechanical carry refused" in detail
    else:
        raise AssertionError("a critical review accepted an uncarried second lane")


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


def test_report_without_critical_attestation_evidence_refuses() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    del fixture["critical_attestations"]
    try:
        R.evaluate(fixture, Path("/unused"), "b" * 40, head)
    except R.Refused as error:
        assert "no canonical critical attestation evidence" in str(error)
    else:
        raise AssertionError("a report without critical attestation evidence passed")


def test_critical_summary_without_attestation_evidence_refuses() -> None:
    head = "a" * 40
    fixture = comment_only_report(head)
    fixture["critical_required"] = True
    fixture["critical_lanes_missing"] = ["claude"]
    try:
        R.evaluate(fixture, Path("/unused"), "b" * 40, head)
    except R.Refused as error:
        assert "summary has no canonical attestation evidence" in str(error)
    else:
        raise AssertionError("a critical summary without its evidence passed")


def authority_record(body: str, record_id: int, timestamp: str) -> dict:
    return {
        "body": body,
        "id": record_id,
        "html_url": (
            "https://github.com/rrnewton/hermit/pull/2261"
            f"#issuecomment-{record_id}"
        ),
        "created_at": timestamp,
        "updated_at": timestamp,
        "user": {"login": "rrnewton"},
        "_review_surface": "issue-comment",
        "_current_review_actor_login": "rrnewton",
        "_current_review_actor_permission": "write",
        "_current_review_actor_authorized": True,
    }


def approval_binding_report(
    tmp_path: Path,
    current: str,
    approved: str,
    lanes: tuple[str, ...],
) -> dict:
    args = argparse.Namespace(
        critical="hermit-syscall",
        family="codex",
        head=approved,
        outcome="approval",
        comment_url=(
            "https://github.com/rrnewton/hermit/pull/2261"
            "#issuecomment-900001"
        ),
        reviewer=None,
        round=1,
        task="critical-rebase-test",
    )
    comments = [
        authority_record(
            REVIEW_ATTEST._comment_body(args, "rrnewton"),
            900002,
            "2026-09-05T12:00:00Z",
        )
    ]
    for offset, lane in enumerate(lanes, start=3):
        comments.append(
            authority_record(
                f"APPROVED-AT: {lane} {approved}",
                900000 + offset,
                f"2026-09-05T12:00:0{offset}Z",
            )
        )
    fixture = tmp_path / ("approval-" + "-".join(lanes) + ".json")
    fixture.write_text(
        json.dumps(
            {
                "actions_enabled": True,
                "prs": [
                    {
                        "number": 2261,
                        "head": current,
                        "labels": ["post-facto-human-review"],
                        "comments": comments,
                        "runs_at_head": 1,
                        "mergeable": "MERGEABLE",
                    }
                ],
            }
        )
    )
    result = subprocess.run(
        [
            sys.executable,
            str(Path(R.approval_authority.__file__)),
            "--from-json",
            str(fixture),
            "--pr",
            "2261",
            "--json",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def test_mechanical_rebase_carries_critical_attestation_and_requires_both_lanes(
    tmp_path: Path,
) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)

    one_lane = approval_binding_report(tmp_path, current, approved, ("codex",))
    assert one_lane["critical_required"] is True
    assert one_lane["current_critical_classes"] == []
    assert one_lane["critical_attestations"][0]["head"] == approved
    try:
        R.evaluate(one_lane, repo, current_base, current)
    except R.Refused as error:
        assert "critical review families missing: claude" in str(error)
    else:
        raise AssertionError("carried critical classification accepted only one lane")

    both_lanes = approval_binding_report(
        tmp_path, current, approved, ("codex", "claude")
    )
    verdict = R.evaluate(both_lanes, repo, current_base, current)
    assert {approval["lane"] for approval in verdict["approvals"]} == {
        "claude",
        "codex",
    }
    assert verdict["critical_attestations"][0]["basis"] == "MECHANICAL_REBASE"
    assert any(
        verdict["critical_attestations"][0]["proof"] == approval["proof"]
        for approval in verdict["approvals"]
    )

    cache_mutation = copy.deepcopy(both_lanes)
    cache_mutation["critical_required"] = False
    cache_mutation["critical_classes"] = []
    for evidence in cache_mutation["critical_attestations"]:
        evidence["label"] = "post-facto-human-review"
        evidence["source"] = "label"
    for row in cache_mutation["rows"]:
        row["label"] = "passed-review-" + row["lane"]
        row["source"] = "label"
    assert R.evaluate(cache_mutation, repo, current_base, current) == verdict


def test_changed_patch_cannot_carry_critical_classification(tmp_path: Path) -> None:
    repo, approved, current_base, current = mechanical_fixture(tmp_path)
    report = approval_binding_report(tmp_path, current, approved, ("codex", "claude"))
    current = commit(repo, "feature", "reviewed\nchanged\n", "semantic change")
    for row in report["rows"]:
        row["head"] = current
    try:
        R.evaluate(report, repo, current_base, current)
    except R.Refused as error:
        assert "critical classification is not bound" in str(error)
    else:
        raise AssertionError("changed patch inherited an old critical attestation")
