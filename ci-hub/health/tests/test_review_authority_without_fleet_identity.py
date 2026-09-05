"""Exact-head review authority does not depend on transient fleet names."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
from typing import Any

import pytest


HERE = pathlib.Path(__file__).resolve()
TOOL = HERE.parent.parent / "approval_binding.py"

PR2956_HEAD = "88bd94fdfc841eb0512b81707e6e2e8f56d589a6"
PR2950_HEAD = "58bf4f5c17880395400ab3cb6482d0de2ee03a84"
PR2904_HEAD = "226a6506daf9745508a430e8bc2997d90e9ba164"


def _record(
    record_id: int,
    body: str,
    timestamp: str,
    *,
    actor: str = "rrnewton",
    authorized: bool = True,
) -> dict[str, Any]:
    return {
        "id": record_id,
        "body": body,
        "user": {"login": actor},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": authorized,
        "_current_review_actor_login": actor,
        "_current_review_actor_permission": "write" if authorized else "read",
        "created_at": timestamp,
        "updated_at": timestamp,
    }


def _run(tmp_path: pathlib.Path, pr: dict[str, Any]) -> tuple[int, dict[str, Any]]:
    fixture = tmp_path / f"pr-{pr['number']}.json"
    fixture.write_text(json.dumps([pr]), encoding="utf-8")
    result = subprocess.run(
        [
            sys.executable,
            str(TOOL),
            "--from-json",
            str(fixture),
            "--pr",
            str(pr["number"]),
            "--gate",
            "--json",
        ],
        text=True,
        capture_output=True,
        timeout=30,
    )
    return result.returncode, json.loads(result.stdout)


def test_pr_2956_exact_comment_is_authority_without_a_label(
    tmp_path: pathlib.Path,
) -> None:
    pr = {
        "number": 2956,
        "head": PR2956_HEAD,
        "labels": [],
        "comments": [
            _record(
                5552507459,
                "\n".join(
                    [
                        "[hermit2, review-cpuid, unresolved, devbig014, role=reviewer]",
                        f"APPROVED-AT: claude {PR2956_HEAD}",
                    ]
                ),
                "2026-09-05T14:34:02Z",
            )
        ],
        "runs_at_head": 0,
        "mergeable": "MERGEABLE",
    }

    rc, report = _run(tmp_path, pr)

    assert rc == 0
    assert report["approvals"] == 1
    assert report["authoritative"] == 1
    assert report["approval_set_missing"] is False
    assert report["rows"][0]["source"] == "comment"
    assert report["rows"][0]["bound_to"] == PR2956_HEAD


def test_pr_2950_chronology_is_authority_without_a_label_or_name_match(
    tmp_path: pathlib.Path,
) -> None:
    pr = {
        "number": 2950,
        "head": PR2950_HEAD,
        "labels": [],
        "comments": [
            _record(
                5545346256,
                "\n".join(
                    [
                        "[hermit2, review-cpuid, unresolved, devbig014, role=reviewer]",
                        "CHANGES-REQUESTED-AT: claude "
                        "c3a85da26110fd8dfc31433c8d4109726127703b",
                    ]
                ),
                "2026-09-04T19:14:42Z",
            ),
            _record(
                5552679512,
                "\n".join(
                    [
                        "[hermit2, review-cpuid, unresolved, devbig014, role=reviewer]",
                        "CHANGES-REQUESTED-WITHDRAWN-AT: claude "
                        "2a18b7bea06cf66f5cf4a973466afa506c0bd2a4",
                        "APPROVED-AT: claude "
                        "2a18b7bea06cf66f5cf4a973466afa506c0bd2a4",
                    ]
                ),
                "2026-09-05T15:03:48Z",
            ),
            _record(
                5552753110,
                "\n".join(
                    [
                        "[hermit2, deliberately-different-name, unresolved, devbig030, role=reviewer]",
                        f"APPROVED-AT: claude {PR2950_HEAD}",
                    ]
                ),
                "2026-09-05T15:16:26Z",
            ),
        ],
        "runs_at_head": 0,
        "mergeable": "MERGEABLE",
    }

    rc, report = _run(tmp_path, pr)

    assert rc == 0
    assert report["approvals"] == 1
    assert report["authoritative"] == 1
    assert report["approval_set_missing"] is False
    assert report["rows"][0]["state"] == "BOUND_CURRENT"


@pytest.mark.parametrize(
    ("number", "refusal_metadata", "retirement_metadata", "by_metadata"),
    [
        (2904, "departed-reviewer", None, None),
        (2905, None, None, None),
        (2906, "first-reviewer", "second-reviewer", "third-reviewer"),
        (2907, "devbig014", None, None),
    ],
)
def test_departed_or_irregular_fleet_names_do_not_prevent_exact_retirement(
    tmp_path: pathlib.Path,
    number: int,
    refusal_metadata: str | None,
    retirement_metadata: str | None,
    by_metadata: str | None,
) -> None:
    head = PR2904_HEAD if number == 2904 else f"{number:040x}"
    refused_head = "1eef513dc8a9d97d910206fe3f72bfb28a67ea62"
    refusal_id = 5519564951 + number
    disclosure = (
        f"[hermit2, {refusal_metadata}, unresolved, devbig014, role=reviewer]\n"
        if refusal_metadata
        else ""
    )
    retirement_disclosure = (
        f"[hermit2, {retirement_metadata}, unresolved, devbig030, role=reviewer]\n"
        if retirement_metadata
        else ""
    )
    by = f" BY {by_metadata}" if by_metadata else ""
    pr = {
        "number": number,
        "head": head,
        "labels": [],
        "comments": [
            _record(
                refusal_id,
                disclosure + f"CHANGES-REQUESTED-AT: codex {refused_head}",
                "2026-09-03T02:47:24Z",
            ),
            _record(
                refusal_id + 100,
                retirement_disclosure
                + f"CHANGES-REQUESTED-WITHDRAWN-AT: codex {head}{by}\n"
                + f"RETIRES {refusal_id}",
                "2026-09-03T04:47:24Z",
            ),
            _record(
                refusal_id + 200,
                f"APPROVED-AT: codex {head}",
                "2026-09-03T05:47:24Z",
            ),
        ],
        "runs_at_head": 0,
        "mergeable": "MERGEABLE",
    }

    rc, report = _run(tmp_path, pr)

    assert rc == 0
    assert report["approvals"] == 1
    assert report["authoritative"] == 1
    assert report["rows"][0]["state"] == "BOUND_CURRENT"


@pytest.mark.parametrize(
    "body",
    [
        f"APPROVED-AT: claude {'1' * 40}",
        "APPROVED-AT: claude",
        f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {'1' * 40}",
    ],
)
def test_stale_or_malformed_artifacts_do_not_grant_authority(
    tmp_path: pathlib.Path, body: str
) -> None:
    head = "2" * 40
    pr = {
        "number": 2960,
        "head": head,
        "labels": [],
        "comments": [_record(5590000001, body, "2026-09-05T01:00:00Z")],
        "runs_at_head": 0,
        "mergeable": "MERGEABLE",
    }

    rc, report = _run(tmp_path, pr)

    assert rc == 1
    assert report["authoritative"] == 0


def test_unauthorized_actor_cannot_grant_exact_head_approval(
    tmp_path: pathlib.Path,
) -> None:
    head = "3" * 40
    pr = {
        "number": 2963,
        "head": head,
        "labels": [],
        "comments": [
            _record(
                5590000002,
                f"APPROVED-AT: codex {head}",
                "2026-09-05T01:00:00Z",
                actor="read-only-user",
                authorized=False,
            )
        ],
        "runs_at_head": 0,
        "mergeable": "MERGEABLE",
    }

    rc, report = _run(tmp_path, pr)

    assert rc == 1
    assert report["authoritative"] == 0
    assert report["approval_set_missing"] is True
