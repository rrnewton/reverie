"""approval_binding honors the same RETIRES records as the refusal gate."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib

HERE = pathlib.Path(__file__).resolve()
HEALTH = HERE.parent.parent
GATE = HEALTH.parent / "bin" / "changes-requested-gate"

HEAD = "6457ba0acfb4eda22cf5fd617412c6b734b92801"
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


def _comment(comment_id: int, agent: str | None, *lines: str) -> dict:
    head = [f"[hermit2, {agent}, unresolved, devbig014, role=review]"] if agent else []
    return {
        "id": comment_id,
        "body": "\n".join([*head, *lines]),
        "html_url": f"fixture://comment/{comment_id}",
    }


def _state(comments: list[dict]) -> tuple[str, list[tuple[str, str | None]]]:
    refusals: list[str] = []
    unanswered: list[tuple[str, str | None]] = []
    shas = ab.lane_shas(
        comments, "claude", refusals=refusals, unanswered=unanswered
    )
    state = ab.classify_approval(
        HEAD,
        shas,
        refused=bool(refusals),
        refused_at=refusals[-1] if refusals else None,
        unanswered=bool(unanswered),
    )
    return state, unanswered


def _gate_state(comments: list[dict]) -> int:
    return gate.evaluate(
        [comment["body"] for comment in comments],
        HEAD,
        ids=[comment["id"] for comment in comments],
    )[0]


def _approval(agent: str = "hermit-011") -> dict:
    return _comment(800001, agent, f"APPROVED-AT: claude {HEAD} BY {agent}")


def _refusal(comment_id: int, agent: str | None = None) -> dict:
    suffix = f" BY {agent}" if agent else ""
    return _comment(
        comment_id,
        agent,
        f"CHANGES-REQUESTED-AT: claude {HEAD}{suffix}",
    )


def _withdrawal(agent: str, target: int | None) -> dict:
    lines = [f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD} BY {agent}"]
    if target is not None:
        lines.append(f"RETIRES {target}")
    return _comment(WITHDRAWAL_ID, agent, *lines)


def test_a_retired_unsigned_refusal_stops_blocking() -> None:
    comments = [
        _approval(), _refusal(REFUSAL_ID), _withdrawal("hermit-030", REFUSAL_ID)
    ]
    assert _gate_state(comments) == gate.OK
    state, unanswered = _state(comments)
    assert state == ab.BOUND_CURRENT, (state, unanswered)
    assert unanswered == []


def test_the_same_unretired_refusal_still_blocks() -> None:
    comments = [
        _approval(), _refusal(REFUSAL_ID), _withdrawal("hermit-030", None)
    ]
    assert _gate_state(comments) == gate.BLOCKED
    state, unanswered = _state(comments)
    assert state == ab.REFUSED, (state, unanswered)
    assert unanswered == [(HEAD, None)]


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


def test_another_issuer_cannot_retire_a_named_refusal() -> None:
    state, unanswered = _state(
        [
            _approval(),
            _refusal(REFUSAL_ID, "hermit-011"),
            _withdrawal("hermit-030", REFUSAL_ID),
        ]
    )
    assert state == ab.REFUSED, (state, unanswered)
    assert unanswered == [(HEAD, "hermit-011")]


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
