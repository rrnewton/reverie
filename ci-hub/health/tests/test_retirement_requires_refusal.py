"""A RETIRES claim must name a comment that actually issued a refusal."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib

HERE = pathlib.Path(__file__).resolve()
HEALTH = HERE.parent.parent
GATE = HEALTH.parent / "bin" / "changes-requested-gate"

OLD_HEAD = "5b6ce1fe80953a4f3a698233e2e28be87f8d3613"
NEW_HEAD = "96eeb0d603875e595f36d6ce97fc77a998ea53a2"
COMMENT_ID = 5250055861
WITHDRAWAL_ID = 900003


def _load(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


grammar = _load("retirement_refusal_grammar", HEALTH / "approval_binding.py")
gate = _load("retirement_refusal_gate", GATE)


def _withdrawal(target: int) -> str:
    return (
        "[hermit2, hermit-030, unresolved, host, role=reviewer]\n"
        f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {NEW_HEAD} BY hermit-030\n"
        f"RETIRES {target}"
    )


def _records(refusal: str, withdrawal: str) -> list[dict]:
    records = [
        {
            "id": COMMENT_ID,
            "body": refusal,
            "user": {"login": grammar.comment_author(refusal) or "fixture-reviewer"},
            "_review_surface": "issue-comment",
            "created_at": "2026-09-04T08:00:00Z",
            "updated_at": "2026-09-04T08:00:00Z",
        },
        {
            "id": WITHDRAWAL_ID,
            "body": withdrawal,
            "user": {
                "login": grammar.comment_author(withdrawal) or "fixture-reviewer"
            },
            "_review_surface": "issue-comment",
            "created_at": "2026-09-04T08:01:00Z",
            "updated_at": "2026-09-04T08:01:00Z",
        },
    ]
    return grammar.annotate_current_review_authority(
        records,
        "rrnewton/hermit",
        lambda path: {
            "role_name": "write",
            "permission": "write",
            "user": {"login": path.split("/collaborators/", 1)[1].split("/", 1)[0]},
        },
    )


NON_REFUSAL = f"""[coordinator, reviewer]

## Verdict: STAYS OPEN after a read-only sweep

The earlier review is
CHANGES-REQUESTED and the `{OLD_HEAD[:8]}` rebase answered none of it.
"""


def test_a_non_refusal_comment_remains_fail_closed_but_cannot_be_retired() -> None:
    markers = gate.markers_with_ids([(COMMENT_ID, NON_REFUSAL)])
    assert markers == [(gate.REFUSAL, None, OLD_HEAD[:8], COMMENT_ID)]
    assert not grammar.comment_contains_refusal(NON_REFUSAL)

    records = _records(NON_REFUSAL, _withdrawal(COMMENT_ID))
    code, reason = gate.evaluate(
        [record["body"] for record in records],
        NEW_HEAD,
        [COMMENT_ID, WITHDRAWAL_ID],
        require_answered=True,
        records=records,
    )
    assert code == gate.BLOCKED, reason
    assert "DOES NOT CONTAIN A REFUSAL" in reason, reason
    assert f"comment {COMMENT_ID}" in reason, reason


def test_a_heading_formatted_refusal_is_still_found_and_can_be_retired() -> None:
    refusal = (
        "[hermit2, hermit-030, unresolved, host, role=reviewer]\n"
        f"## **CHANGES-REQUESTED-AT: claude {OLD_HEAD} BY hermit-030**"
    )
    assert grammar.comment_contains_refusal(refusal)
    assert gate.markers_with_ids([(COMMENT_ID, refusal)]) == [
        (gate.REFUSAL, "claude", OLD_HEAD, COMMENT_ID)
    ]

    records = _records(refusal, _withdrawal(COMMENT_ID))
    code, reason = gate.evaluate(
        [record["body"] for record in records],
        NEW_HEAD,
        [COMMENT_ID, WITHDRAWAL_ID],
        require_answered=True,
        records=records,
    )
    assert code == gate.OK, reason


def test_existing_noncanonical_refusal_forms_still_count_as_refusals() -> None:
    forms = (
        f"CHANGES-REQUESTED: claude {OLD_HEAD}",
        f"CHANGES REQUESTED: claude {OLD_HEAD}",
        f"REQUEST CHANGES AT {OLD_HEAD}",
        f"REJECTED: claude {OLD_HEAD}",
        f"CHANGES-REQUESTED claude {OLD_HEAD}",
        f"CHANGES-REQUESTED {OLD_HEAD}",
        "CHANGES-REQUESTED because the boundary check is missing.",
    )
    for form in forms:
        assert grammar.comment_contains_refusal(form), form


def test_a_genuine_sha_less_refusal_keeps_its_precise_exit() -> None:
    refusal = (
        "[hermit2, hermit-030, unresolved, host, role=reviewer]\n"
        "CHANGES-REQUESTED because the boundary check is missing."
    )
    assert grammar.comment_contains_refusal(refusal)
    records = _records(refusal, _withdrawal(COMMENT_ID))
    code, reason = gate.evaluate(
        [record["body"] for record in records],
        NEW_HEAD,
        [COMMENT_ID, WITHDRAWAL_ID],
        require_answered=True,
        records=records,
    )
    assert code == gate.OK, reason
