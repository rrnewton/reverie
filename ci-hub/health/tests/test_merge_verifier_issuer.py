#!/usr/bin/env python3
"""The merge verifier refuses two lanes bound by ONE agent.

THE TWO-LANE RULE RESTS ON INDEPENDENCE NOBODY COULD MEASURE. `claude` and
`codex` name a harness FAMILY, and nineteen agents share those two names, so
"this head carries both lanes" was never evidence that two reviewers looked at
it. One agent could bind both, and the record was indistinguishable from genuine
independent review.

dev-hermit#191 made the binding carry its issuer. This file pins the consumer
that finally asks the question -- and, just as importantly, pins the two places
it must NOT ask it:

  - fewer than two lanes claimed: untouched, exactly like the stale-label rule
    it sits beside. This imposes no review requirement on a pull request that
    never claimed one.
  - issuer unknown: DECLINES TO ANSWER. Nothing emits `BY` yet, so the issuer
    usually comes from the comment's disclosure tag, and that tag is missing on
    a large minority of agent comments. Refusing on an unknown identity would
    block nearly the whole fleet on the day it lands, for a fact never
    established.

Needs no network: `run` is replaced with a fixture, so no `gh` call is made.
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


def _comment(agent: str | None, *lines: str) -> dict:
    head = [f"[hermit2, {agent}, unresolved, devbig014, role=review]"] if agent else []
    return {"body": "\n".join([*head, *lines]), "html_url": f"c/{agent}"}


def _run(lanes: list[str], comments: list[dict]):
    """``(stale, single_issuer)`` for a PR carrying `lanes` and `comments`."""
    doc = {
        "labels": [{"name": f"passed-review-{l}"} for l in lanes],
        "comments": comments,
        "reviews": [],
    }
    saved = tool.run
    tool.run = lambda *a, **k: (0, json.dumps(doc), "")
    try:
        one: list[str] = []
        stale = tool.stale_review_labels("1", "rrnewton/dev-hermit", HEAD,
                                         single_issuer=one)
        return stale, one
    finally:
        tool.run = saved


# --------------------------------------------------------------------------
# The two cases the rule exists for.
# --------------------------------------------------------------------------

def test_same_issuer_on_both_lanes_is_REJECTED() -> None:
    """⚠️ THE CASE THE TWO-LANE RULE WAS ALWAYS MEANT TO CATCH. Both lanes bound
    at this exact head, both labels present, every marker canonical -- and one
    agent wrote both. Before the issuer existed this was a clean two-lane head."""
    stale, one = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-011", f"APPROVED-AT: codex {HEAD} BY hermit-011"),
        ],
    )
    assert stale == [], stale          # nothing stale: both ARE bound at the head
    assert len(one) == 1, one
    assert "hermit-011" in one[0] and "claude, codex" in one[0], one


def test_different_issuers_are_ACCEPTED() -> None:
    """The control. Identical shape, two agents -- and it must pass cleanly.
    Without this the test above only proves the check FIRES, not that it
    DISCRIMINATES; a check that refuses everything would satisfy it."""
    stale, one = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-030", f"APPROVED-AT: codex {HEAD} BY hermit-030"),
        ],
    )
    assert stale == [], stale
    assert one == [], one


# --------------------------------------------------------------------------
# Where it must NOT fire.
# --------------------------------------------------------------------------

def test_an_unattributable_binding_declines_to_answer() -> None:
    """No `BY`, no disclosure tag. This must NOT refuse: it has evidence of
    neither one agent nor two. Refusing here would block on the absence of
    evidence, which is the inversion the surrounding scope rule exists to avoid."""
    stale, one = _run(
        ["claude", "codex"],
        [
            {"body": f"APPROVED-AT: claude {HEAD}", "html_url": "c/1"},
            {"body": f"APPROVED-AT: codex {HEAD}", "html_url": "c/2"},
        ],
    )
    assert stale == [] and one == [], (stale, one)


def test_one_known_issuer_plus_one_unknown_declines() -> None:
    """The mixed case, and the dangerous one to get wrong. An unknown issuer is
    not 'probably the same agent': it is unattributed. Treating the pair as one
    agent would refuse a head that may well have had two reviewers."""
    stale, one = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            {"body": f"APPROVED-AT: codex {HEAD}", "html_url": "c/2"},
        ],
    )
    assert one == [], one


def test_a_single_claimed_lane_is_untouched() -> None:
    """Same scope discipline as the stale-label rule beside it: this imposes no
    review requirement on a pull request that never claimed two lanes."""
    stale, one = _run(
        ["claude"],
        [_comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011")],
    )
    assert stale == [] and one == [], (stale, one)


def test_a_pr_with_no_labels_is_untouched() -> None:
    stale, one = _run([], [_comment("hermit-011", f"APPROVED-AT: claude {HEAD}")])
    assert stale == [] and one == [], (stale, one)


def test_only_bindings_at_THIS_head_count() -> None:
    """⚠️ A LANE'S EARLIER BINDING SAYS NOTHING ABOUT THE CODE BEING MERGED. Here
    hermit-011 bound claude at this head and also bound codex at a SUPERSEDED
    sha. The codex label is therefore STALE -- reported by the existing rule --
    and the one-agent check must not additionally fire, because only one lane is
    actually bound here."""
    stale, one = _run(
        ["claude", "codex"],
        [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-011", f"APPROVED-AT: codex {OLD} BY hermit-011"),
        ],
    )
    assert len(stale) == 1 and "passed-review-codex" in stale[0], stale
    assert one == [], one


# --------------------------------------------------------------------------
# The pre-existing rule must keep working.
# --------------------------------------------------------------------------

def test_the_stale_label_rule_is_unchanged() -> None:
    """The out-param is additive; adding it must not disturb what this function
    already refused."""
    stale, one = _run(
        ["claude"],
        [_comment("hermit-011", f"APPROVED-AT: claude {OLD} BY hermit-011")],
    )
    assert len(stale) == 1 and "passed-review-claude" in stale[0], stale
    assert one == [], one


def test_omitting_the_out_param_is_unchanged_behaviour() -> None:
    """Every existing caller passes no `single_issuer`. They must be untouched,
    and in particular must not crash on the new code path."""
    doc = {
        "labels": [{"name": "passed-review-claude"}, {"name": "passed-review-codex"}],
        "comments": [
            _comment("hermit-011", f"APPROVED-AT: claude {HEAD} BY hermit-011"),
            _comment("hermit-011", f"APPROVED-AT: codex {HEAD} BY hermit-011"),
        ],
        "reviews": [],
    }
    saved = tool.run
    tool.run = lambda *a, **k: (0, json.dumps(doc), "")
    try:
        assert tool.stale_review_labels("1", "rrnewton/dev-hermit", HEAD) == []
    finally:
        tool.run = saved


def test_unreadable_state_is_still_refused_as_absence() -> None:
    saved = tool.run
    tool.run = lambda *a, **k: (1, "", "boom")
    try:
        one: list[str] = []
        assert tool.stale_review_labels("1", "r/r", HEAD, single_issuer=one) == [
            "__UNREADABLE__"]
        assert one == [], one
    finally:
        tool.run = saved
