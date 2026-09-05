#!/usr/bin/env python3
"""The merge verifier binds required review sets to exact review artifacts.

Fleet names in disclosures and ``BY`` clauses are optional metadata. They do
not change the exact-head decision. The immutable GitHub actor must still have
current repository permission, and cache labels cannot add or remove authority.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import pathlib

HERE = pathlib.Path(__file__).resolve()
BIN = HERE.parent.parent.parent / "bin"

HEAD = "6457ba0acfb4eda22cf5fd617412c6b734b92801"
OLD = "b3ca4e1509f7d2a1c4e8b60d3f7a95126ce4d880"


def _load(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


tool = _load("gh_merge_verified_issuer_under_test", BIN / "gh-merge-verified")


_NEXT_COMMENT_ID = 900000


def _comment(agent: str | None, *lines: str) -> dict:
    global _NEXT_COMMENT_ID
    _NEXT_COMMENT_ID += 1
    head = [f"[hermit2, {agent}, unresolved, devbig014, role=review]"] if agent else []
    return {
        "id": _NEXT_COMMENT_ID,
        "body": "\n".join([*head, *lines]),
        "user": {"login": "fixture-reviewer"},
        "html_url": (
            "https://github.com/rrnewton/dev-hermit/pull/1"
            f"#issuecomment-{_NEXT_COMMENT_ID}"
        ),
        "created_at": "2026-09-04T00:00:01Z",
        "updated_at": "2026-09-04T00:00:01Z",
    }


def _install_run(comments: list[dict], labels: list[str]):
    def fake(argv, **_kwargs):
        joined = " ".join(argv)
        if " api graphql " in f" {joined} ":
            return (
                0,
                json.dumps(
                    [
                        {
                            "data": {
                                "repository": {
                                    "pullRequest": {
                                        "reviews": {
                                            "nodes": [],
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
                ),
                "",
            )
        if "/issues/1/comments" in joined:
            return 0, json.dumps([comments]), ""
        if "/pulls/1/comments" in joined:
            return 0, "[[]]", ""
        if "/collaborators/fixture-reviewer/permission" in joined:
            return 0, json.dumps({
                "role_name": "write",
                "permission": "write",
                "user": {"login": "fixture-reviewer"},
            }), ""
        if " pr view " in f" {joined} ":
            return (
                0,
                json.dumps(
                    {
                        "labels": [
                            {"name": f"passed-review-{lane}"} for lane in labels
                        ]
                    }
                ),
                "",
            )
        raise AssertionError(f"unexpected gh call: {argv!r}")

    return fake


def _run(lanes: list[str], comments: list[dict]):
    """Review-admission findings for a PR carrying `lanes` and `comments`."""
    saved = tool.run
    tool.run = _install_run(comments, lanes)
    try:
        return tool.review_admission_problems("1", "rrnewton/dev-hermit", HEAD)
    finally:
        tool.run = saved


# --------------------------------------------------------------------------
# The two cases the rule exists for.
# --------------------------------------------------------------------------

def test_same_fleet_name_on_both_lanes_does_not_change_exact_head_binding() -> None:
    stale = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-011", f"APPROVED-AT: codex {HEAD} BY hermit-011"),
        ],
    )
    assert stale == [], stale


def test_different_fleet_names_have_the_same_result() -> None:
    stale = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-030", f"APPROVED-AT: codex {HEAD} BY hermit-030"),
        ],
    )
    assert stale == [], stale


# --------------------------------------------------------------------------
# Where it must NOT fire.
# --------------------------------------------------------------------------

def test_a_binding_without_fleet_name_metadata_still_binds() -> None:
    stale = _run(
        ["claude", "codex"],
        [
            _comment(None, f"APPROVED-AT: claude {HEAD}"),
            _comment(None, f"APPROVED-AT: codex {HEAD}"),
        ],
    )
    assert stale == [], stale


def test_mixed_fleet_name_metadata_still_binds() -> None:
    stale = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment(None, f"APPROVED-AT: codex {HEAD}"),
        ],
    )
    assert stale == [], stale


def test_a_single_claimed_lane_is_untouched() -> None:
    """An ordinary change needs one canonical lane, independent of labels."""
    problems = _run(
        ["claude"],
        [_comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011")],
    )
    assert problems == [], problems


def test_a_pr_with_no_labels_is_untouched() -> None:
    problems = _run([], [_comment("hermit-011", f"APPROVED-AT: claude {HEAD}")])
    assert problems == [], problems


def test_only_bindings_at_this_head_count_and_stale_labels_add_no_requirement() -> None:
    """One current lane satisfies an ordinary change; a stale cache adds nothing."""
    problems = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-011", f"APPROVED-AT: codex {OLD} BY hermit-011"),
        ],
    )
    assert problems == [], problems


# --------------------------------------------------------------------------
# Exact-head evidence remains required even when a cache label exists.
# --------------------------------------------------------------------------

def test_stale_evidence_cannot_become_current_through_a_cache_label() -> None:
    problems = _run(
        ["claude"],
        [_comment("hermit-011", f"APPROVED-AT: claude {OLD} BY hermit-011")],
    )
    assert problems == [
        "required approval set missing: no canonical exact-head review approval"
    ], problems


def test_both_exact_head_lanes_are_current_without_identity_checks() -> None:
    comments = [
        _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
        _comment("hermit-011", f"APPROVED-AT: codex {HEAD} BY hermit-011"),
    ]
    saved = tool.run
    tool.run = _install_run(comments, ["claude", "codex"])
    try:
        assert tool.review_admission_problems(
            "1", "rrnewton/dev-hermit", HEAD
        ) == []
    finally:
        tool.run = saved


def test_unreadable_state_is_still_refused_as_absence() -> None:
    saved = tool.run
    tool.run = lambda *a, **k: (1, "", "boom")
    try:
        assert tool.review_admission_problems("1", "r/r", HEAD) == [
            "__UNREADABLE__"]
    finally:
        tool.run = saved
