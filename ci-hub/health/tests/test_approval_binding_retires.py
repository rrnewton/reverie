"""approval_binding honors the same RETIRES records as the refusal gate."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import datetime
import pathlib

HERE = pathlib.Path(__file__).resolve()
HEALTH = HERE.parent.parent
GATE = HEALTH.parent / "bin" / "changes-requested-gate"

HEAD = "6457ba0acfb4eda22cf5fd617412c6b734b92801"
MOVED_HEAD = "7457ba0acfb4eda22cf5fd617412c6b734b92802"
REFUSAL_ID = 900001
SECOND_REFUSAL_ID = 900002
WITHDRAWAL_ID = 900003
WRONG_ID = 900099


def _load(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


ab = _load("approval_binding_retires", HEALTH / "approval_binding.py")
gate = _load("changes_requested_gate_retires", GATE)


def _comment(
    comment_id: int,
    agent: str | None,
    *lines: str,
    role: str = "review",
    github_actor: str | None = None,
    created_at: str | None = None,
    updated_at: str | None = None,
) -> dict:
    head = [f"[hermit2, {agent}, unresolved, devbig014, role={role}]"] if agent else []
    if created_at is None:
        created_at = (
            datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc)
            + datetime.timedelta(seconds=comment_id)
        ).isoformat().replace("+00:00", "Z")
    return {
        "id": comment_id,
        "body": "\n".join([*head, *lines]),
        "html_url": f"fixture://comment/{comment_id}",
        "user": {"login": github_actor or agent or "fixture-reviewer"},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": True,
        "_current_review_actor_login": github_actor or agent or "fixture-reviewer",
        "_current_review_actor_permission": "write",
        "created_at": created_at,
        "updated_at": updated_at or created_at,
    }


def _state(
    comments: list[dict], current_head: str = HEAD
) -> tuple[str, list[tuple[str, str | None]]]:
    refusals: list[str] = []
    unanswered: list[tuple[str, str | None]] = []
    shas = ab.lane_shas(
        comments,
        "claude",
        refusals=refusals,
        unanswered=unanswered,
        current_head=current_head,
    )
    state = ab.classify_approval(
        current_head,
        shas,
        refused=bool(refusals),
        refused_at=refusals[-1] if refusals else None,
        unanswered=bool(unanswered),
    )
    return state, unanswered


def _gate_result(
    comments: list[dict],
    head: str = HEAD,
    *,
    require_answered: bool = False,
) -> tuple[int, str]:
    return gate.evaluate(
        [comment["body"] for comment in comments],
        head,
        ids=[comment["id"] for comment in comments],
        require_answered=require_answered,
        records=comments,
    )


def _gate_state(comments: list[dict]) -> int:
    return _gate_result(comments)[0]


def _approval(agent: str = "hermit-011", head: str = HEAD) -> dict:
    return _comment(800001, agent, f"APPROVED-AT: claude {head} BY {agent}")


def _refusal(
    comment_id: int, agent: str | None = None, head: str = HEAD
) -> dict:
    suffix = f" BY {agent}" if agent else ""
    return _comment(
        comment_id,
        agent,
        f"CHANGES-REQUESTED-AT: claude {head}{suffix}",
    )


def _withdrawal(
    agent: str | None,
    target: int | None,
    *,
    lane: str = "claude",
    head: str = HEAD,
    role: str = "review",
    marker_agent: str | None = None,
    github_actor: str | None = None,
    extra_target: int | None = None,
) -> dict:
    marker_agent = agent if marker_agent is None else marker_agent
    suffix = f" BY {marker_agent}" if marker_agent else ""
    lines = [f"CHANGES-REQUESTED-WITHDRAWN-AT: {lane} {head}{suffix}"]
    if target is not None:
        lines.append(f"RETIRES {target}")
    if extra_target is not None:
        lines.append(f"RETIRES {extra_target}")
    return _comment(
        WITHDRAWAL_ID,
        agent,
        *lines,
        role=role,
        github_actor=github_actor,
    )


def test_a_retired_unsigned_refusal_stops_blocking() -> None:
    comments = [
        _approval(), _refusal(REFUSAL_ID), _withdrawal("hermit-030", REFUSAL_ID)
    ]
    assert _gate_state(comments) == gate.OK
    state, unanswered = _state(comments)
    assert state == ab.BOUND_CURRENT, (state, unanswered)
    assert unanswered == []


def test_a_later_current_head_withdrawal_discharges_the_lane() -> None:
    comments = [
        _approval(), _refusal(REFUSAL_ID), _withdrawal("hermit-030", None)
    ]
    assert _gate_state(comments) == gate.OK
    state, unanswered = _state(comments)
    assert state == ab.UNDETERMINED, (state, unanswered)
    assert unanswered == []


def test_retiring_a_different_comment_does_not_clear_the_refusal() -> None:
    state, unanswered = _state(
        [_approval(), _refusal(REFUSAL_ID), _withdrawal("hermit-030", WRONG_ID)]
    )
    assert state == ab.REFUSED, (state, unanswered)
    assert unanswered == [(HEAD, None)]


def test_a_precise_withdrawal_does_not_clear_a_second_refusal() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _refusal(SECOND_REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-011", REFUSAL_ID),
    ]
    state, unanswered = _state(comments)
    assert state == ab.REFUSED, (state, unanswered)
    assert unanswered == [(HEAD, "hermit-011")]


def test_another_authorized_reviewer_can_retire_a_named_refusal() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", REFUSAL_ID),
    ]
    assert _gate_state(comments) == gate.OK
    state, unanswered = _state(comments)
    assert state == ab.BOUND_CURRENT, (state, unanswered)
    assert unanswered == []


def test_fleet_name_on_a_current_head_withdrawal_does_not_control_discharge() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", None),
    ]
    assert _gate_state(comments) == gate.OK
    state, unanswered = _state(comments)
    assert state == ab.UNDETERMINED, (state, unanswered)
    assert unanswered == []


def test_the_refusal_issuer_can_retire_the_named_refusal() -> None:
    state, unanswered = _state(
        [
            _approval(),
            _refusal(REFUSAL_ID, "hermit-011"),
            _withdrawal("hermit-011", REFUSAL_ID),
        ]
    )
    assert state == ab.BOUND_CURRENT, (state, unanswered)
    assert unanswered == []


def test_a_coordinator_can_retire_the_exact_named_refusal() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", REFUSAL_ID, role="coordinator"),
    ]
    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.BOUND_CURRENT, [])


def test_current_repository_authorization_is_required() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal["_current_review_actor_authorized"] = False
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_missing_current_permission_evidence_is_not_authority() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal.pop("_current_review_actor_permission")
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_immutable_actor_must_match_permission_annotation() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal["user"] = {"login": "unrelated-outsider"}
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_shared_github_account_can_publish_an_answered_retirement() -> None:
    withdrawal = _withdrawal(
        "review-521-codex",
        REFUSAL_ID,
        github_actor="rrnewton",
    )
    withdrawal.pop("_current_review_actor_authorized")
    requested_paths: list[str] = []

    def permission(path: str) -> object:
        requested_paths.append(path)
        return {
            "role_name": "admin",
            "permission": "admin",
            "user": {"login": "rrnewton"},
        }

    annotated = ab.annotate_current_review_authority(
        [withdrawal], "rrnewton/hermit", permission
    )[0]
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), annotated]

    assert requested_paths == [
        "repos/rrnewton/hermit/collaborators/rrnewton/permission"
    ]
    assert annotated["_current_review_actor_authorized"] is True
    assert annotated["_current_review_actor_login"] == "rrnewton"
    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.BOUND_CURRENT, [])


def test_current_head_withdrawal_is_substantive_answering_evidence() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal(
            "review-521-codex",
            None,
            github_actor="rrnewton",
        ),
    ]

    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.UNDETERMINED, [])


def test_shared_github_account_without_repository_permission_cannot_retire() -> None:
    withdrawal = _withdrawal(
        "review-521-codex",
        REFUSAL_ID,
        github_actor="shared-read-only",
    )
    withdrawal.pop("_current_review_actor_authorized")
    annotated = ab.annotate_current_review_authority(
        [withdrawal],
        "rrnewton/hermit",
        lambda _path: {
            "role_name": "read",
            "permission": "read",
            "user": {"login": "shared-read-only"},
        },
    )[0]
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), annotated]

    assert annotated["_current_review_actor_authorized"] is False
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_authorization_annotation_uses_current_permission() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal.pop("_current_review_actor_authorized")

    for role_name, permission, expected in (
        ("triage", "read", "triage"),
        ("maintain", "write", "maintain"),
        ("write", "write", "write"),
        ("admin", "admin", "admin"),
    ):
        authorized = ab.annotate_current_review_authority(
            [withdrawal],
            "rrnewton/hermit",
            lambda _path, role_name=role_name, permission=permission: {
                "role_name": role_name,
                "permission": permission,
                "user": {"login": "hermit-030"},
            },
        )
        assert authorized[0]["_current_review_actor_authorized"] is True
        assert authorized[0]["_current_review_actor_permission"] == expected

    read_only = ab.annotate_current_review_authority(
        [withdrawal],
        "rrnewton/hermit",
        lambda _path: {
            "role_name": "read",
            "permission": "read",
            "user": {"login": "hermit-030"},
        },
    )
    assert read_only[0]["_current_review_actor_authorized"] is False


def test_an_unauthorized_refusal_cannot_block_an_authorized_approval() -> None:
    refusal = _refusal(REFUSAL_ID, "outside-reader")
    refusal.update(
        user={"login": "outside-reader"},
        _current_review_actor_authorized=False,
        _current_review_actor_login="outside-reader",
        _current_review_actor_permission="read",
    )
    comments = [_approval(), refusal]

    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.BOUND_CURRENT, [])


def test_an_unauthorized_refusal_cannot_create_same_time_ambiguity() -> None:
    when = "2026-01-12T10:00:01Z"
    approval = _comment(
        REFUSAL_ID,
        "authorized-reviewer",
        f"APPROVED-AT: claude {HEAD}",
        created_at=when,
        updated_at=when,
    )
    refusal = _comment(
        WITHDRAWAL_ID,
        "outside-reader",
        f"CHANGES-REQUESTED-AT: claude {HEAD}",
        created_at=when,
        updated_at=when,
    )
    refusal.update(
        user={"login": "outside-reader"},
        _current_review_actor_authorized=False,
        _current_review_actor_login="outside-reader",
        _current_review_actor_permission="read",
    )

    assert ab.canonical_review_records([approval, refusal]) == [approval, refusal]
    assert _state([approval, refusal]) == (ab.BOUND_CURRENT, [])


def test_live_annotation_refuses_an_authority_record_without_an_actor() -> None:
    refusal = _refusal(REFUSAL_ID, "missing-actor")
    refusal.pop("user")
    refusal.pop("_current_review_actor_authorized")
    refusal.pop("_current_review_actor_login")
    refusal.pop("_current_review_actor_permission")
    try:
        ab.annotate_current_review_authority([refusal], "rrnewton/hermit", lambda _: {})
    except ab.ReviewRecordError as error:
        assert "no immutable GitHub actor" in str(error)
    else:
        raise AssertionError("a live authority record without an actor was accepted")


def test_authority_annotation_validation_requires_an_immutable_actor() -> None:
    refusal = _refusal(REFUSAL_ID, "missing-actor")
    refusal.pop("user")
    try:
        ab.validate_review_authority_annotations([refusal])
    except ab.ReviewRecordError as error:
        assert "no immutable GitHub actor" in str(error)
    else:
        raise AssertionError("an authority-bearing record without an actor was accepted")


def test_authority_annotation_validation_requires_explicit_permission() -> None:
    refusal = _refusal(REFUSAL_ID, "fixture-reviewer")
    refusal.pop("_current_review_actor_authorized")
    refusal.pop("_current_review_actor_permission")
    try:
        ab.validate_review_authority_annotations([refusal])
    except ab.ReviewRecordError as error:
        assert "explicit current GitHub actor permission annotation" in str(error)
    else:
        raise AssertionError(
            "an authority-bearing record without a permission annotation was accepted"
        )


def test_authorization_annotation_refuses_missing_permission_evidence() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal.pop("_current_review_actor_authorized")
    try:
        ab.annotate_current_review_authority(
            [withdrawal],
            "rrnewton/hermit",
            lambda _path: {"user": {"login": "hermit-030"}},
        )
    except ab.ReviewRecordError as error:
        assert "malformed" in str(error)
    else:
        raise AssertionError("missing permission evidence did not fail closed")


def test_authorization_annotation_refuses_permission_api_failure() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal.pop("_current_review_actor_authorized")

    def unavailable(_path: str) -> object:
        raise RuntimeError("API unavailable")

    try:
        ab.annotate_current_review_authority(
            [withdrawal], "rrnewton/hermit", unavailable
        )
    except ab.ReviewRecordError as error:
        assert "cannot determine current review authorization" in str(error)
    else:
        raise AssertionError("permission API failure did not fail closed")


def test_authorization_annotation_refuses_mismatched_permission_actor() -> None:
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID)
    withdrawal.pop("_current_review_actor_authorized")
    try:
        ab.annotate_current_review_authority(
            [withdrawal],
            "rrnewton/hermit",
            lambda _path: {
                "role_name": "write",
                "permission": "write",
                "user": {"login": "somebody-else"},
            },
        )
    except ab.ReviewRecordError as error:
        assert "malformed" in str(error)
    else:
        raise AssertionError("mismatched permission actor did not fail closed")


def test_malformed_retires_assertions_do_not_fall_back_to_generic_withdrawal() -> None:
    refusal = _refusal(REFUSAL_ID, "hermit-011")
    for malformed in ("RETIRES", "RETIRES nope", "RETIRES #not-a-number"):
        withdrawal = _withdrawal("hermit-011", None)
        withdrawal["body"] += f"\n{malformed}"
        comments = [_approval(), refusal, withdrawal]
        assert _gate_state(comments) == gate.BLOCKED, malformed
        assert _state(comments)[0] == ab.REFUSED, malformed


def test_retires_text_in_a_quote_does_not_disable_a_canonical_withdrawal() -> None:
    refusal = _refusal(REFUSAL_ID, "hermit-011")
    for quoted in (
        "`RETIRES nope`",
        "> RETIRES nope",
        "```text\nRETIRES nope\n```",
    ):
        withdrawal = _withdrawal("hermit-011", None)
        withdrawal["body"] += f"\n{quoted}"
        assert _gate_state([refusal, withdrawal]) == gate.OK, quoted
        assert _state([refusal, withdrawal]) == (ab.UNDETERMINED, []), quoted


def test_a_reviewer_disclosure_after_a_role_tag_can_retire() -> None:
    withdrawal = {
        "id": WITHDRAWAL_ID,
        "body": "\n".join(
            [
                "[review agent, gpt-5.6-sol] "
                "[hermit2, hermit-030, model, devbig014, role=reviewer]",
                f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD} BY hermit-030",
                f"RETIRES {REFUSAL_ID}",
            ]
        ),
        "user": {"login": "hermit-030"},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": True,
        "_current_review_actor_login": "hermit-030",
        "_current_review_actor_permission": "write",
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
    }
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.BOUND_CURRENT, [])


def test_role_text_does_not_override_current_repository_authorization() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", REFUSAL_ID, role="impl"),
    ]
    assert _gate_state(comments) == gate.OK
    assert _state(comments)[0] == ab.BOUND_CURRENT


def test_unrecognized_role_text_is_audit_metadata_not_authority() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", REFUSAL_ID, role="reviewer-relay"),
    ]
    assert _gate_state(comments) == gate.OK
    assert _state(comments)[0] == ab.BOUND_CURRENT


def test_a_short_malformed_disclosure_is_only_metadata() -> None:
    withdrawal = {
        "id": WITHDRAWAL_ID,
        "body": "\n".join(
            [
                "[hermit2, hermit-030, role=reviewer]",
                f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD} BY hermit-030",
                f"RETIRES {REFUSAL_ID}",
            ]
        ),
        "user": {"login": "rrnewton"},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": True,
        "_current_review_actor_login": "rrnewton",
        "_current_review_actor_permission": "write",
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
    }
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.OK
    assert _state(comments)[0] == ab.BOUND_CURRENT


def test_a_later_copied_reviewer_disclosure_is_only_metadata() -> None:
    withdrawal = {
        "id": WITHDRAWAL_ID,
        "body": "\n".join(
            [
                "[hermit2, hermit-impl, model, devbig014, role=impl]",
                "[hermit2, hermit-review, model, devbig014, role=reviewer]",
                f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD} BY hermit-review",
                f"RETIRES {REFUSAL_ID}",
            ]
        ),
        "user": {"login": "rrnewton"},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": True,
        "_current_review_actor_login": "rrnewton",
        "_current_review_actor_permission": "write",
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
    }
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.OK
    assert _state(comments)[0] == ab.BOUND_CURRENT


def test_a_copied_reviewer_disclosure_after_prose_is_only_metadata() -> None:
    withdrawal = {
        "id": WITHDRAWAL_ID,
        "body": "\n".join(
            [
                "I verified the fix.",
                "",
                "[hermit2, hermit-review, model, devbig014, role=reviewer]",
                f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD} BY hermit-review",
                f"RETIRES {REFUSAL_ID}",
            ]
        ),
        "user": {"login": "rrnewton"},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": True,
        "_current_review_actor_login": "rrnewton",
        "_current_review_actor_permission": "write",
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
    }
    comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
    assert _gate_state(comments) == gate.OK
    assert _state(comments)[0] == ab.BOUND_CURRENT


def test_a_by_clause_that_disagrees_with_the_disclosure_is_only_metadata() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal(
            "hermit-030", REFUSAL_ID, marker_agent="hermit-099"
        ),
    ]
    assert _gate_state(comments) == gate.OK
    assert _state(comments)[0] == ab.BOUND_CURRENT


def test_no_fleet_name_is_required_for_exact_authorized_retirement() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "departed-reviewer"),
        _withdrawal(None, REFUSAL_ID, github_actor="rrnewton"),
    ]
    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.BOUND_CURRENT, [])


def test_hostname_shaped_issuer_is_only_metadata() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "devbig014"),
        _withdrawal(None, REFUSAL_ID, github_actor="rrnewton"),
    ]
    assert _gate_state(comments) == gate.OK
    assert _state(comments) == (ab.BOUND_CURRENT, [])


def test_a_retirement_bound_to_another_lane_cannot_retire() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", REFUSAL_ID, lane="codex"),
    ]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_a_retirement_bound_to_another_head_cannot_retire() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-030", REFUSAL_ID, head=MOVED_HEAD),
    ]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_old_head_refusal_and_current_head_retirement_allow_progress() -> None:
    comments = [
        _approval(head=MOVED_HEAD),
        _refusal(REFUSAL_ID, "hermit-011", head=HEAD),
        _withdrawal("hermit-030", REFUSAL_ID, head=MOVED_HEAD),
    ]
    assert _gate_result(comments, MOVED_HEAD, require_answered=True)[0] == gate.OK
    assert _state(comments, MOVED_HEAD) == (ab.BOUND_CURRENT, [])


def test_graphql_ids_use_the_numeric_comment_url_for_exact_retirement() -> None:
    refusal = _refusal(REFUSAL_ID, "hermit-011", head=HEAD)
    refusal["id"] = "IC_opaque_refusal"
    refusal["url"] = (
        f"https://github.com/o/r/pull/1#issuecomment-{REFUSAL_ID}"
    )
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID, head=MOVED_HEAD)
    withdrawal["id"] = "IC_opaque_withdrawal"
    withdrawal["url"] = (
        f"https://github.com/o/r/pull/1#issuecomment-{WITHDRAWAL_ID}"
    )
    comments = [_approval(head=MOVED_HEAD), refusal, withdrawal]
    assert _state(comments, MOVED_HEAD) == (ab.BOUND_CURRENT, [])


def test_old_head_refusal_and_stale_head_retirement_remain_blocked() -> None:
    comments = [
        _approval(head=MOVED_HEAD),
        _refusal(REFUSAL_ID, "hermit-011", head=HEAD),
        _withdrawal("hermit-030", REFUSAL_ID, head=HEAD),
    ]
    assert (
        _gate_result(comments, MOVED_HEAD, require_answered=True)[0]
        == gate.BLOCKED
    )
    assert _state(comments, MOVED_HEAD)[0] == ab.REFUSED_STALE


def test_an_ambiguous_multi_target_retirement_cannot_retire() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal(
            "hermit-030", REFUSAL_ID, extra_target=SECOND_REFUSAL_ID
        ),
    ]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_a_retirement_posted_before_the_refusal_cannot_retire_it() -> None:
    comments = [
        _approval(),
        _withdrawal(
            "hermit-011",
            REFUSAL_ID,
        )
        | {
            "created_at": "2026-01-12T10:00:01Z",
            "updated_at": "2026-01-12T10:00:01Z",
        },
        _refusal(REFUSAL_ID, "hermit-011")
        | {
            "created_at": "2026-01-12T10:00:02Z",
            "updated_at": "2026-01-12T10:00:02Z",
        },
    ]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_a_refusal_edited_after_its_retirement_is_not_the_retired_version() -> None:
    refusal = _refusal(REFUSAL_ID, "hermit-011") | {
        "created_at": "2026-01-12T10:00:01Z",
        "updated_at": "2026-01-12T10:00:04Z",
    }
    withdrawal = _withdrawal("hermit-011", REFUSAL_ID) | {
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
    }
    comments = [_approval(), refusal, withdrawal]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_a_later_refusal_is_not_retired_with_the_earlier_one() -> None:
    first = _refusal(REFUSAL_ID, "hermit-011") | {
        "created_at": "2026-01-12T10:00:01Z",
        "updated_at": "2026-01-12T10:00:01Z",
    }
    withdrawal = _withdrawal("hermit-011", REFUSAL_ID) | {
        "created_at": "2026-01-12T10:00:02Z",
        "updated_at": "2026-01-12T10:00:02Z",
    }
    later = _refusal(SECOND_REFUSAL_ID, "hermit-012") | {
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
    }
    comments = [_approval(), later, withdrawal, first]
    assert _gate_state(comments) == gate.BLOCKED
    state, unanswered = _state(comments)
    assert state == ab.REFUSED
    assert unanswered == [(HEAD, "hermit-012")]


def test_an_earlier_generic_withdrawal_cannot_clear_a_later_refusal() -> None:
    withdrawal = _withdrawal("hermit-011", None) | {
        "created_at": "2026-01-12T10:00:01Z",
        "updated_at": "2026-01-12T10:00:01Z",
    }
    refusal = _refusal(REFUSAL_ID, "hermit-011") | {
        "created_at": "2026-01-12T10:00:02Z",
        "updated_at": "2026-01-12T10:00:02Z",
    }
    comments = [_approval(), refusal, withdrawal]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_native_review_uses_the_latest_graphql_version_time() -> None:
    record = {
        "fullDatabaseId": REFUSAL_ID,
        "id": "PRR_fixture",
        "body": "Please fix this.",
        "state": "CHANGES_REQUESTED",
        "submittedAt": "2026-01-12T10:00:01Z",
        "updatedAt": "2026-01-12T10:00:02Z",
        "lastEditedAt": "2026-01-12T10:00:03Z",
        "includesCreatedEdit": True,
    }
    assert ab.review_record_version_time(record) == datetime.datetime(
        2026, 1, 12, 10, 0, 3, tzinfo=datetime.timezone.utc
    )


def test_submitted_at_alone_cannot_identify_a_native_review_version() -> None:
    record = {
        "fullDatabaseId": REFUSAL_ID,
        "id": "PRR_fixture",
        "body": "Please fix this.",
        "state": "CHANGES_REQUESTED",
        "submittedAt": "2026-01-12T10:00:01Z",
        "includesCreatedEdit": False,
    }
    assert ab.review_record_version_time(record) is None


def test_review_sources_preserve_author_and_source_identity() -> None:
    rest = ab.review_records_from_rest_pages(
        [[{"id": REFUSAL_ID, "user": {"login": "reviewer-login"}}]],
        "issue-comment",
    )
    native = ab.native_review_records_from_graphql(
        [
            {
                "data": {
                    "repository": {
                        "pullRequest": {
                            "reviews": {
                                "nodes": [
                                    {
                                        "fullDatabaseId": SECOND_REFUSAL_ID,
                                        "author": {"login": "native-reviewer"},
                                        "state": "COMMENTED",
                                    }
                                ],
                                "pageInfo": {
                                    "hasNextPage": False,
                                    "endCursor": None,
                                },
                            }
                        }
                    }
                }
            }
        ]
    )
    assert rest[0]["user"]["login"] == "reviewer-login"
    assert rest[0]["_review_surface"] == "issue-comment"
    assert native[0]["author"]["login"] == "native-reviewer"
    assert native[0]["_review_surface"] == "pull-request-review"
    assert ab.comment_database_id(native[0]) == str(SECOND_REFUSAL_ID)


def test_native_review_pagination_must_reach_a_declared_final_page() -> None:
    unfinished = [
        {
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviews": {
                            "nodes": [],
                            "pageInfo": {
                                "hasNextPage": True,
                                "endCursor": "cursor-1",
                            },
                        }
                    }
                }
            }
        }
    ]
    try:
        ab.native_review_records_from_graphql(unfinished)
    except ab.ReviewRecordError as error:
        assert "hasNextPage is true" in str(error)
    else:
        raise AssertionError("truncated native-review connection did not fail closed")


def test_native_review_query_rejects_graphql_errors() -> None:
    pages = [
        {
            "errors": [{"message": "partial response"}],
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviews": {
                            "nodes": [],
                            "pageInfo": {"hasNextPage": False, "endCursor": None},
                        }
                    }
                }
            },
        }
    ]
    try:
        ab.native_review_records_from_graphql(pages)
    except ab.ReviewRecordError as error:
        assert "GraphQL errors" in str(error)
    else:
        raise AssertionError("partial GraphQL response did not fail closed")


def test_native_review_query_rejects_missing_or_malformed_state() -> None:
    for label, node in (
        ("missing", {"fullDatabaseId": REFUSAL_ID}),
        ("null", {"fullDatabaseId": REFUSAL_ID, "state": None}),
        ("numeric", {"fullDatabaseId": REFUSAL_ID, "state": 7}),
        ("unknown", {"fullDatabaseId": REFUSAL_ID, "state": "UNKNOWN"}),
    ):
        pages = [
            {
                "data": {
                    "repository": {
                        "pullRequest": {
                            "reviews": {
                                "nodes": [node],
                                "pageInfo": {
                                    "hasNextPage": False,
                                    "endCursor": None,
                                },
                            }
                        }
                    }
                }
            }
        ]
        try:
            ab.native_review_records_from_graphql(pages)
        except ab.ReviewRecordError as error:
            assert "malformed state" in str(error)
        else:
            raise AssertionError(f"native state {label} did not fail closed")


def test_rest_review_query_requires_at_least_one_page() -> None:
    try:
        ab.review_records_from_rest_pages([], "issue-comment")
    except ab.ReviewRecordError as error:
        assert "page arrays" in str(error)
    else:
        raise AssertionError("empty REST page set did not fail closed")


def test_rest_review_query_accepts_complete_multiple_pages() -> None:
    records = ab.review_records_from_rest_pages(
        [
            [{"id": REFUSAL_ID, "body": "first"}],
            [{"id": SECOND_REFUSAL_ID, "body": "second"}],
        ],
        "issue-comment",
    )
    assert [record["id"] for record in records] == [REFUSAL_ID, SECOND_REFUSAL_ID]
    assert {record["_review_surface"] for record in records} == {"issue-comment"}


def test_native_review_pagination_accepts_complete_multiple_pages() -> None:
    pages = [
        {
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviews": {
                            "nodes": [
                                {
                                    "fullDatabaseId": REFUSAL_ID,
                                    "state": "COMMENTED",
                                }
                            ],
                            "pageInfo": {
                                "hasNextPage": True,
                                "endCursor": "cursor-1",
                            },
                        }
                    }
                }
            }
        },
        {
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviews": {
                            "nodes": [
                                {
                                    "fullDatabaseId": SECOND_REFUSAL_ID,
                                    "state": "COMMENTED",
                                }
                            ],
                            "pageInfo": {
                                "hasNextPage": False,
                                "endCursor": "cursor-2",
                            },
                        }
                    }
                }
            }
        },
    ]
    assert [
        record["fullDatabaseId"]
        for record in ab.native_review_records_from_graphql(pages)
    ] == [REFUSAL_ID, SECOND_REFUSAL_ID]


def test_mixed_review_surfaces_are_ordered_by_current_version_time() -> None:
    issue = _comment(REFUSAL_ID, "hermit-011", "issue") | {
        "created_at": "2026-01-12T10:00:03Z",
        "updated_at": "2026-01-12T10:00:03Z",
        "_review_surface": "issue-comment",
    }
    native = {
        "fullDatabaseId": SECOND_REFUSAL_ID,
        "id": "PRR_fixture",
        "body": "native",
        "state": "COMMENTED",
        "submittedAt": "2026-01-12T10:00:01Z",
        "updatedAt": "2026-01-12T10:00:01Z",
        "lastEditedAt": None,
        "includesCreatedEdit": False,
        "commit": {"oid": HEAD},
        "_review_surface": "pull-request-review",
    }
    inline = _comment(WITHDRAWAL_ID, "hermit-012", "inline") | {
        "created_at": "2026-01-12T10:00:02Z",
        "updated_at": "2026-01-12T10:00:02Z",
        "_review_surface": "pull-review-comment",
    }
    assert [
        record["_review_surface"]
        for record in ab.canonical_review_records([issue, native, inline])
    ] == ["pull-request-review", "pull-review-comment", "issue-comment"]


def test_equal_authority_timestamps_across_surfaces_are_ambiguous() -> None:
    refusal = _refusal(REFUSAL_ID, "hermit-011") | {
        "created_at": "2026-01-12T10:00:01Z",
        "updated_at": "2026-01-12T10:00:01Z",
        "_review_surface": "issue-comment",
    }
    withdrawal = _withdrawal("hermit-030", REFUSAL_ID) | {
        "created_at": "2026-01-12T10:00:01Z",
        "updated_at": "2026-01-12T10:00:01Z",
        "_review_surface": "pull-review-comment",
    }
    try:
        ab.canonical_review_records([refusal, withdrawal])
    except ab.ReviewRecordError as error:
        assert "chronology is ambiguous" in str(error)
    else:
        raise AssertionError("cross-surface timestamp tie did not fail closed")


def test_duplicate_numeric_identity_across_surfaces_is_ambiguous() -> None:
    first = _comment(REFUSAL_ID, "hermit-011", "first")
    second = _comment(REFUSAL_ID, "hermit-012", "second")
    try:
        ab.canonical_review_records([first, second])
    except ab.ReviewRecordError as error:
        assert "appears more than once" in str(error)
    else:
        raise AssertionError("duplicate cross-surface identity did not fail closed")


def test_pull_request_body_is_not_a_review_authority_surface() -> None:
    body = _comment(REFUSAL_ID, "hermit-011", "ordinary pull-request body") | {
        "_review_surface": "pull-request-body"
    }
    try:
        ab.canonical_review_records([body])
    except ab.ReviewRecordError as error:
        assert "recognized review surface" in str(error)
    else:
        raise AssertionError("pull-request body was accepted as review authority")


def test_later_head_approval_answers_an_older_head_refusal() -> None:
    refusal = _comment(
        REFUSAL_ID,
        "hermit-011",
        f"CHANGES-REQUESTED-AT: claude {MOVED_HEAD}",
        created_at="2026-01-12T10:00:01Z",
    )
    approval = _comment(
        WITHDRAWAL_ID,
        "hermit-011",
        f"APPROVED-AT: claude {HEAD}",
        created_at="2026-01-12T10:00:02Z",
    )
    # Endpoint concatenation put the later approval first. The shared reader
    # restores version order, and the current-head approval answers the older-
    # head objection without consulting optional fleet-name metadata.
    assert ab.lane_shas([approval, refusal], "claude", [], current_head=HEAD) == [HEAD]


def test_an_objection_appended_after_approval_remains_unresolved() -> None:
    refusal = _comment(
        REFUSAL_ID,
        "hermit-011",
        f"CHANGES-REQUESTED-AT: claude {MOVED_HEAD}",
        created_at="2026-01-12T10:00:01Z",
        updated_at="2026-01-12T10:00:03Z",
    )
    approval = _comment(
        WITHDRAWAL_ID,
        "hermit-011",
        f"APPROVED-AT: claude {HEAD}",
        created_at="2026-01-12T10:00:02Z",
        updated_at="2026-01-12T10:00:02Z",
    )
    refusals: list[str] = []
    unanswered: list[tuple[str, str | None]] = []
    shas = ab.lane_shas(
        [refusal, approval],
        "claude",
        [],
        refusals=refusals,
        unanswered=unanswered,
        current_head=HEAD,
    )
    assert shas == []
    assert refusals == [MOVED_HEAD]
    assert unanswered == [(MOVED_HEAD, "hermit-011")]
    assert ab.classify_approval(
        HEAD,
        shas,
        refused=True,
        refused_at=MOVED_HEAD,
        unanswered=True,
    ) == ab.REFUSED_STALE


def test_same_head_approval_and_refusal_conflict_in_both_orders() -> None:
    approval = {"body": f"APPROVED-AT: claude {HEAD}"}
    refusal = {"body": f"CHANGES-REQUESTED-AT: claude {HEAD}"}
    for comments in ([approval, refusal], [refusal, approval]):
        state, unanswered = _state(comments)
        assert state == ab.REFUSED
        assert unanswered == [(HEAD, None)]


def test_same_version_time_all_refusal_spellings_fail_closed_in_either_id_order() -> None:
    when = "2026-01-12T10:00:01Z"
    refusals = (
        f"CHANGES-REQUESTED-AT: claude {HEAD}",
        f"CHANGES-REQUESTED: claude {HEAD}",
        f"CHANGES REQUESTED: claude {HEAD}",
        f"REJECTED: claude {HEAD}",
        f"REQUEST CHANGES AT {HEAD}",
    )
    for refusal_body in refusals:
        for approval_id, refusal_id in (
            (REFUSAL_ID, WITHDRAWAL_ID),
            (WITHDRAWAL_ID, REFUSAL_ID),
        ):
            approval = _comment(
                approval_id,
                "hermit-011",
                f"APPROVED-AT: claude {HEAD}",
                created_at=when,
                updated_at=when,
            )
            refusal = _comment(
                refusal_id,
                "hermit-012",
                refusal_body,
                created_at=when,
                updated_at=when,
            )
            try:
                ab.canonical_review_records([approval, refusal])
            except ab.ReviewRecordError as error:
                assert "opposing review authority has the same version time" in str(error)
            else:
                raise AssertionError(
                    f"tied {refusal_body!r} depended on numeric id order"
                )


def test_optional_identity_metadata_does_not_change_the_canonical_decision() -> None:
    original = _comment(
        REFUSAL_ID,
        "reviewer-one",
        f"APPROVED-AT: claude {HEAD} BY reviewer-one",
    )
    edited = dict(original)
    edited["body"] = (
        "[hermit2, hostname-as-agent, unresolved, devbig030, role=reviewer]\n"
        f"APPROVED-AT: claude {HEAD} BY false-caller-name"
    )
    assert ab.review_record_decisions(original) == ab.review_record_decisions(edited)


def test_all_open_collection_does_not_use_cache_labels_as_its_index(
    monkeypatch,
) -> None:
    approved_pr = {
        "number": 2956,
        "headRefOid": HEAD,
        "labels": [],
        "mergeable": "MERGEABLE",
    }
    unreviewed_pr = {
        "number": 2957,
        "headRefOid": MOVED_HEAD,
        "labels": [],
        "mergeable": "MERGEABLE",
    }
    approval = _comment(
        REFUSAL_ID,
        "hostname-as-agent",
        f"APPROVED-AT: claude {HEAD}",
    )
    review_reads: list[int] = []

    def fake_gh_json(args: list[str], **_kwargs):
        if args[:2] == ["pr", "list"]:
            return [approved_pr, unreviewed_pr]
        if args and args[0] == "api" and "/actions/runs?head_sha=" in args[1]:
            return {"workflow_runs": []}
        raise AssertionError(f"unexpected gh call: {args}")

    def fake_review_records(_repo: str, pr: int) -> list[dict]:
        review_reads.append(pr)
        return [approval] if pr == 2956 else []

    monkeypatch.setattr(ab, "gh_json", fake_gh_json)
    monkeypatch.setattr(ab, "github_review_records", fake_review_records)

    rows, selected = ab.collect_live("rrnewton/hermit", [], {})

    assert selected is None
    assert sorted(review_reads) == [2956, 2957]
    assert [
        (row["number"], row["source"], row["lane"], row["state"])
        for row in rows
    ] == [(2956, "comment", "claude", ab.BOUND_CURRENT)]


def test_a_target_comment_with_two_refusals_is_ambiguous() -> None:
    target = _comment(
        REFUSAL_ID,
        "hermit-011",
        f"CHANGES-REQUESTED-AT: claude {HEAD} BY hermit-011",
        f"CHANGES-REQUESTED-AT: codex {HEAD} BY hermit-011",
    )
    comments = [_approval(), target, _withdrawal("hermit-030", REFUSAL_ID)]
    assert _gate_state(comments) == gate.BLOCKED
    assert _state(comments)[0] == ab.REFUSED


def test_retires_mentions_do_not_change_canonical_withdrawal_semantics() -> None:
    for example in (
        f"Example only; do not post `RETIRES {REFUSAL_ID}`.",
        f"```text\nRETIRES {REFUSAL_ID}\n```",
        f"> RETIRES {REFUSAL_ID}",
    ):
        withdrawal = _comment(
            WITHDRAWAL_ID,
            "hermit-030",
            f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD} BY hermit-030",
            example,
        )
        comments = [_approval(), _refusal(REFUSAL_ID, "hermit-011"), withdrawal]
        assert _gate_state(comments) == gate.OK, example
        assert _state(comments) == (ab.UNDETERMINED, []), example


def test_retires_without_a_withdrawal_does_not_clear_the_refusal() -> None:
    state, unanswered = _state(
        [
            _approval(),
            _refusal(REFUSAL_ID),
            _comment(WITHDRAWAL_ID, "hermit-030", f"RETIRES {REFUSAL_ID}"),
        ]
    )
    assert state == ab.REFUSED, (state, unanswered)
    assert unanswered == [(HEAD, None)]


def test_old_head_retirement_of_unsigned_refusal_does_not_clear_named_peer() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _refusal(SECOND_REFUSAL_ID),
        _withdrawal("hermit-030", SECOND_REFUSAL_ID, head=MOVED_HEAD),
    ]
    rc, reason = _gate_result(comments, MOVED_HEAD)
    assert rc == gate.BLOCKED, reason
    assert "OUTRUN" in reason, reason
    assert "1 other refusal(s) WERE answered" in reason, reason
    assert f"claude@{HEAD[:12]}" in reason, reason
    assert "hermit-011" not in reason, reason
    assert "comment 900002 was retired by authorized GitHub actor hermit-030" in reason


def test_old_head_named_refusal_is_cleared_by_its_own_precise_retirement() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID, "hermit-011"),
        _withdrawal("hermit-011", REFUSAL_ID, head=MOVED_HEAD),
    ]
    rc, reason = _gate_result(comments, MOVED_HEAD)
    assert rc == gate.OK, reason
    assert "ANSWERED" in reason, reason


def test_old_head_unsigned_refusal_keeps_its_precise_exit() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID),
        _withdrawal("hermit-030", REFUSAL_ID, head=MOVED_HEAD),
    ]
    rc, reason = _gate_result(comments, MOVED_HEAD)
    assert rc == gate.OK, reason
    assert "comment 900001 was retired by authorized GitHub actor hermit-030" in reason


def test_old_head_unsigned_unquoted_withdrawal_clears_nothing() -> None:
    comments = [
        _approval(),
        _refusal(REFUSAL_ID),
        _withdrawal("hermit-030", None),
    ]
    rc, reason = _gate_result(comments, MOVED_HEAD)
    assert rc == gate.BLOCKED, reason
    assert f"claude@{HEAD[:12]}" in reason, reason
    assert "hermit-030" not in reason, reason


# ---------------------------------------------------------------------------
# RETIRES-SCOPE: a retirement may survive a head move that leaves the paths it
# named byte-identical, and must NOT survive one that changes them.
# ---------------------------------------------------------------------------


def _claim(sha: str, scope: str | None) -> tuple:
    body = f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {sha}\nRETIRES {REFUSAL_ID}\n"
    if scope is not None:
        body += f"RETIRES-SCOPE: {scope}\n"
    claim = ab.retirement_claim(body)
    assert claim is not None, body
    return claim


def test_scope_is_parsed_and_absent_by_default() -> None:
    assert _claim(HEAD, None)[4] == ()
    assert _claim(HEAD, "demos/ ci/dag")[4] == ("demos/", "ci/dag")
    # Two declarations are ambiguous the same way two RETIRES targets are.
    ambiguous = (
        f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD}\n"
        f"RETIRES {REFUSAL_ID}\n"
        "RETIRES-SCOPE: demos/\n"
        "RETIRES-SCOPE: ci/\n"
    )
    assert ab.retirement_claim(ambiguous)[4] == ()


def test_a_scoped_retirement_survives_only_a_head_move_that_preserves_its_paths() -> None:
    """The whole point, in both directions, with the resolver driven explicitly.

    Reverting `retirement_allowed`'s scope branch makes the SURVIVES assertion
    fail; leaving the branch but ignoring the resolver's answer makes the DIES
    assertion fail. Neither direction passes on its own.
    """
    scoped = [_claim(HEAD, "demos/")]
    unscoped = [_claim(HEAD, None)]

    # No resolver installed: opt-in cannot fire, so behaviour is exactly as before.
    ab.set_scope_equivalence(None)
    assert ab.retirement_allowed("claude", HEAD, scoped) is True
    assert ab.retirement_allowed("claude", MOVED_HEAD, scoped) is False

    calls: list[tuple[str, str, tuple[str, ...]]] = []

    def unchanged(old: str, new: str, scope: tuple[str, ...]) -> bool:
        calls.append((old, new, scope))
        return True

    def changed(old: str, new: str, scope: tuple[str, ...]) -> bool:
        calls.append((old, new, scope))
        return False

    try:
        ab.set_scope_equivalence(unchanged)
        # SURVIVES: head moved, declared paths identical.
        assert ab.retirement_allowed("claude", MOVED_HEAD, scoped) is True
        assert calls[-1] == (HEAD, MOVED_HEAD, ("demos/",))
        # An UNSCOPED retirement never consults the resolver at all.
        before = len(calls)
        assert ab.retirement_allowed("claude", MOVED_HEAD, unscoped) is False
        assert len(calls) == before

        ab.set_scope_equivalence(changed)
        # DIES: head moved and the declared paths changed. This is the assertion
        # that keeps "retires too easily" from being the new failure.
        assert ab.retirement_allowed("claude", MOVED_HEAD, scoped) is False
        # A wrong lane is still refused before the scope is ever considered.
        before = len(calls)
        assert ab.retirement_allowed("codex", MOVED_HEAD, scoped) is False
        assert len(calls) == before
    finally:
        ab.set_scope_equivalence(None)


def test_the_git_resolver_refuses_what_it_cannot_establish(tmp_path) -> None:
    """An unresolvable commit must read as "not equivalent", never as equal."""
    import subprocess

    repo = tmp_path / "r"
    repo.mkdir()
    run = lambda *a: subprocess.run(["git", "-C", str(repo), *a], check=True,
                                    capture_output=True)
    run("init", "-q", "-b", "main")
    run("config", "user.email", "t@example.invalid")
    run("config", "user.name", "t")
    (repo / "kept.txt").write_text("same\n")
    (repo / "other.txt").write_text("one\n")
    run("add", "-A")
    run("commit", "-qm", "first")
    first = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                           capture_output=True, text=True).stdout.strip()
    (repo / "other.txt").write_text("two\n")
    run("add", "-A")
    run("commit", "-qm", "second")
    second = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                            capture_output=True, text=True).stdout.strip()

    eq = ab.git_scope_equivalence(str(repo))
    assert eq(first, second, ("kept.txt",)) is True
    assert eq(first, second, ("other.txt",)) is False
    assert eq(first, second, ("kept.txt", "other.txt")) is False
    # Absent commit, empty scope, empty sha: all refuse rather than answer.
    assert eq(first, "0" * 40, ("kept.txt",)) is False
    assert eq(first, second, ()) is False
    assert eq("", second, ("kept.txt",)) is False


def test_out_of_scope_change_is_reported_not_judged(tmp_path) -> None:
    """The exposure a scoped retirement cannot check must at least be visible.

    A retirement scoped to `demos/` survives a head move that leaves `demos/`
    identical -- correctly, by its own rule -- even when `ci/` changed and the
    objection may have depended on it. Nothing can decide that here. What must
    hold is that the change outside the scope is REPORTED rather than silent.
    """
    import subprocess

    repo = tmp_path / "s"
    repo.mkdir()
    run = lambda *a: subprocess.run(["git", "-C", str(repo), *a], check=True,
                                    capture_output=True)
    run("init", "-q", "-b", "main")
    run("config", "user.email", "t@example.invalid")
    run("config", "user.name", "t")
    (repo / "demos").mkdir()
    (repo / "ci").mkdir()
    (repo / "demos" / "x.sh").write_text("calls helper\n")
    (repo / "ci" / "helper.sh").write_text("v1\n")
    run("add", "-A")
    run("commit", "-qm", "first")
    old = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                         capture_output=True, text=True).stdout.strip()
    # The helper the objection depended on changes; the scoped path does not.
    (repo / "ci" / "helper.sh").write_text("v2\n")
    run("add", "-A")
    run("commit", "-qm", "second")
    new = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                         capture_output=True, text=True).stdout.strip()

    eq = ab.git_scope_equivalence(str(repo))
    # The retirement SURVIVES -- this is the documented, intended behaviour and
    # also precisely the exposure. Both facts are asserted together on purpose.
    assert eq(old, new, ("demos/",)) is True
    # ...and the thing it could not judge is nonetheless named.
    outside = ab.paths_changed_outside_scope(str(repo), old, new, ("demos/",))
    assert outside == ["ci/helper.sh"], outside
    # A scope covering everything that moved reports nothing outside it.
    assert ab.paths_changed_outside_scope(
        str(repo), old, new, ("demos/", "ci/")
    ) == []
    # Unanswerable inputs report nothing rather than guessing.
    assert ab.paths_changed_outside_scope(str(repo), old, new, ()) == []
    assert ab.paths_changed_outside_scope(str(repo), old, "0" * 40, ("demos/",)) == []
