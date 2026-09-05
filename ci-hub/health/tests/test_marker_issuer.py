#!/usr/bin/env python3
"""Marker issuer fields remain readable as optional audit metadata.

Approval and refusal authority does not depend on a fleet-agent name. These
tests preserve compatibility with existing ``BY`` clauses and disclosure tags
while proving that lane and SHA parsing is unchanged. Needs no network or build.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib

HERE = pathlib.Path(__file__).resolve()
HEALTH = HERE.parent.parent

FULL = "6457ba0acfb4eda22cf5fd617412c6b734b92801"
OTHER = "b3ca4e1509f7d2a1c4e8b60d3f7a95126ce4d880"


def _load(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


ab = _load("approval_binding_issuer", HEALTH / "approval_binding.py")


def _comment(agent: str | None, *lines: str) -> dict:
    """A comment body, optionally opening with the disclosure tag agents emit."""
    head = [f"[hermit2, {agent}, unresolved, devbig014, role=review]"] if agent else []
    return {"body": "\n".join([*head, *lines]), "html_url": f"c/{agent}"}


def _bind(comments: list[dict], lane: str = "claude"):
    """``(shas, issuers)`` for `lane`; the two lists are index-aligned."""
    issuers: list[tuple[str, str | None]] = []
    shas = ab.lane_shas(comments, lane, issuers=issuers)
    return shas, issuers


# --------------------------------------------------------------------------
# The invariant: nothing that bound before stops binding.
# --------------------------------------------------------------------------

def test_an_existing_marker_without_an_issuer_still_binds() -> None:
    """THE MIGRATION INVARIANT. Every marker written to date omits `BY`."""
    shas, _ = _bind([_comment("hermit-007", f"APPROVED-AT: claude {FULL}")])
    assert shas == [FULL], shas


def test_parse_marker_still_returns_exactly_three_fields() -> None:
    """The clause is a NAMED group added after group 3, so the 3-tuple contract
    every existing consumer unpacks is unchanged. A 4-tuple here would be a
    reader-visible break dressed up as an additive field."""
    assert ab.parse_marker(f"APPROVED-AT: claude {FULL}") == (
        "APPROVED-AT", "claude", FULL)
    assert ab.parse_marker(f"APPROVED-AT: claude {FULL} BY hermit-011") == (
        "APPROVED-AT", "claude", FULL)


def test_readers_tolerate_before_any_writer_emits() -> None:
    """⚠️ THE ORDERING, PINNED. Emitting first is the unsafe direction and the
    failure is NOT symmetric: an unparsed approval goes `malformed` and fails to
    bind (loud, safe), while an unparsed REFUSAL goes malformed and still blocks
    -- refusing a head nobody refused. So both polarities must parse the clause,
    and this asserts both, not just the approval that is easy to check."""
    assert ab.parse_marker(f"APPROVED-AT: claude {FULL} BY hermit-011")
    assert ab.parse_marker(f"CHANGES-REQUESTED-AT: codex {FULL} BY hermit-030")
    assert ab.parse_marker(
        f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {FULL} BY hermit-011")


def test_a_refusal_carrying_an_issuer_still_refuses() -> None:
    """The asymmetry above, exercised through the binder rather than the grammar:
    a refusal clears the lane whether or not it names its issuer."""
    shas, _ = _bind([
        _comment("hermit-007", f"APPROVED-AT: claude {FULL}"),
        _comment("hermit-030", f"CHANGES-REQUESTED-AT: claude {FULL} BY hermit-030"),
    ])
    assert shas == [], shas


def test_the_clause_changes_neither_lane_nor_sha() -> None:
    shas, _ = _bind([_comment("hermit-011", f"APPROVED-AT: claude {FULL} BY hermit-011")])
    assert shas == [FULL], shas
    assert _bind([_comment("hermit-011", f"APPROVED-AT: codex {FULL} BY hermit-011")])[0] == []


# --------------------------------------------------------------------------
# The issuer itself.
# --------------------------------------------------------------------------

def test_the_issuer_is_read_from_the_binding() -> None:
    _, issuers = _bind([_comment("hermit-011", f"APPROVED-AT: claude {FULL} BY hermit-030")])
    assert issuers == [(FULL, "hermit-030")], issuers


def test_the_issuer_falls_back_to_the_disclosure_tag() -> None:
    """Existing disclosure tags remain available as optional metadata."""
    _, issuers = _bind([_comment("hermit-007", f"APPROVED-AT: claude {FULL}")])
    assert issuers == [(FULL, "hermit-007")], issuers


def test_the_binding_outranks_the_surrounding_comment() -> None:
    """They almost always agree. When they do not, the marker is the more specific
    claim and the one a reviewer wrote deliberately -- and it is also the only one
    that survives being pasted into somebody else's comment."""
    _, issuers = _bind([_comment("hermit-b", f"APPROVED-AT: claude {FULL} BY hermit-a")])
    assert issuers == [(FULL, "hermit-a")], issuers


def test_an_unattributable_binding_reports_none_rather_than_guessing() -> None:
    """No `BY`, no disclosure tag. `None` is the honest answer: a fabricated
    issuer would make an independence check confidently WRONG rather than blind,
    and blind is recoverable."""
    shas, issuers = _bind([{"body": f"APPROVED-AT: claude {FULL}", "html_url": "c/x"}])
    assert shas == [FULL] and issuers == [(FULL, None)], (shas, issuers)


# --------------------------------------------------------------------------
# Metadata reporting does not affect authority.
# --------------------------------------------------------------------------

def test_matching_metadata_is_reported_for_both_lanes() -> None:
    """Matching optional names do not stop either exact-head lane binding."""
    comments = [
        _comment("hermit-011", f"APPROVED-AT: claude {FULL} BY hermit-011"),
        _comment("hermit-011", f"APPROVED-AT: codex {FULL} BY hermit-011"),
    ]
    claude_shas, claude_by = _bind(comments, "claude")
    codex_shas, codex_by = _bind(comments, "codex")
    assert claude_shas == [FULL] and codex_shas == [FULL]
    assert {i for _, i in claude_by} == {i for _, i in codex_by} == {"hermit-011"}


def test_different_metadata_is_reported_for_both_lanes() -> None:
    """Different optional names likewise do not stop either lane binding."""
    comments = [
        _comment("hermit-011", f"APPROVED-AT: claude {FULL} BY hermit-011"),
        _comment("hermit-030", f"APPROVED-AT: codex {FULL} BY hermit-030"),
    ]
    _, claude_by = _bind(comments, "claude")
    _, codex_by = _bind(comments, "codex")
    assert {i for _, i in claude_by} == {"hermit-011"}
    assert {i for _, i in codex_by} == {"hermit-030"}


def test_a_repeated_top_level_marker_keeps_its_optional_metadata() -> None:
    """Two canonical assertions bind; the optional ``BY`` value is preserved."""
    comments = [
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", "Agreed with the analysis:",
                 f"APPROVED-AT: claude {FULL} BY hermit-a"),
    ]
    shas, issuers = _bind(comments)
    assert shas == [FULL, FULL], shas
    assert {i for _, i in issuers} == {"hermit-a"}, issuers


def test_a_repeated_marker_without_by_reports_each_disclosure_name() -> None:
    """Disclosure values remain metadata when a ``BY`` clause is absent."""
    comments = [
        _comment("hermit-a", f"APPROVED-AT: claude {FULL}"),
        _comment("hermit-b", "Agreed with the analysis:",
                 f"APPROVED-AT: claude {FULL}"),
    ]
    _, issuers = _bind(comments)
    assert {i for _, i in issuers} == {"hermit-a", "hermit-b"}, issuers


# --------------------------------------------------------------------------
# The clause must not become a new way to smuggle a binding.
# --------------------------------------------------------------------------

def test_a_quoted_issuer_marker_does_not_bind() -> None:
    """A fenced marker is excluded before the grammar runs, and adding an issuer
    must not route around that."""
    shas, _ = _bind([_comment("hermit-011", "```",
                              f"APPROVED-AT: claude {FULL} BY hermit-011", "```")])
    assert shas == [], shas


def test_a_malformed_issuer_does_not_bind() -> None:
    """`BY` with no agent, or an agent name with characters an id cannot contain.
    The clause is optional, so a BROKEN one must fail the whole match rather than
    silently degrade to a bare marker -- otherwise a typo in the issuer would bind
    a lane while reporting no issuer at all, which is worse than either."""
    for bad in (f"APPROVED-AT: claude {FULL} BY",
                f"APPROVED-AT: claude {FULL} BY -leading",
                f"APPROVED-AT: claude {FULL} BY hermit 011",
                f"APPROVED-AT: claude {FULL} BY hermit_011"):
        assert ab.parse_marker(bad) is None, bad


def test_the_issuer_shares_one_definition_with_the_disclosure_tag() -> None:
    """Disclosure and ``BY`` metadata use the same token grammar."""
    assert ab._AGENT_ID in ab._DISCLOSURE.pattern
    assert ab._AGENT_ID in ab._MARKER_RE.pattern


def test_marker_issuer_reports_absence_distinctly_from_a_non_marker() -> None:
    """`None` for a bare marker and `None` for a line that is not a marker at all
    are the same value, so a caller must not read `None` as "not a marker"."""
    assert ab.marker_issuer(f"APPROVED-AT: claude {FULL}") is None
    assert ab.marker_issuer("not a marker at all") is None
    assert ab.marker_issuer(f"APPROVED-AT: claude {FULL} BY hermit-011") == "hermit-011"


def test_the_issuer_is_case_normalised() -> None:
    """Optional issuer metadata has stable case-normalized output."""
    assert ab.marker_issuer(f"APPROVED-AT: claude {FULL} BY Hermit-011") == "hermit-011"


def test_a_decorated_marker_keeps_its_issuer() -> None:
    """Bold, headings and list bullets are stripped before the grammar runs; the
    clause sits at the END of the line, which is where trailing decoration lands."""
    _, issuers = _bind([_comment("hermit-011",
                                 f"**APPROVED-AT: claude {FULL} BY hermit-030**")])
    assert issuers == [(FULL, "hermit-030")], issuers


def test_issuers_stays_index_aligned_with_the_shas() -> None:
    """The out-param is a parallel list, so a consumer zipping them must not be
    able to mis-pair. Two distinct heads, one lane."""
    comments = [
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", f"APPROVED-AT: claude {OTHER} BY hermit-b"),
    ]
    shas, issuers = _bind(comments)
    assert shas == [FULL, OTHER], shas
    assert issuers == [(FULL, "hermit-a"), (OTHER, "hermit-b")], issuers


def test_omitting_the_out_param_is_unchanged_behaviour() -> None:
    """Every existing caller passes no `issuers`. They must be untouched."""
    assert ab.lane_shas([_comment("hermit-011", f"APPROVED-AT: claude {FULL}")],
                        "claude") == [FULL]


def test_a_rejection_clears_the_issuers_with_the_shas() -> None:
    """⚠️ REGRESSION TEST FOR A BUG I SHIPPED. `found.clear()` on a rejection used
    to leave `issuers` populated, so the approvals vanished while the names of who
    made them survived. A consumer zipping the two mis-pairs; one reading only
    `issuers` sees reviewers for a lane that is not bound at all.

    Every case in the original alignment test used a clean stream with no
    rejection, which is exactly why it passed while the invariant was broken.
    Found on hermit#2637, where lane_shas returned [] and issuers returned one
    entry naming the author of a revoked approval."""
    shas, issuers = _bind([
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", f"CHANGES-REQUESTED-AT: claude {OTHER} BY hermit-b"),
    ])
    assert shas == [], shas
    assert issuers == [], issuers


def test_alignment_survives_a_revoke_then_rebind() -> None:
    """A later exact-head approval rebinds regardless of fleet-name metadata."""
    same = _bind([
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", f"CHANGES-REQUESTED-AT: claude {OTHER} BY hermit-b"),
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
    ])
    assert same == ([FULL], [(FULL, "hermit-a")]), same

    shas, issuers = _bind([
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", f"CHANGES-REQUESTED-AT: claude {OTHER} BY hermit-b"),
        _comment("hermit-c", f"APPROVED-AT: claude {FULL} BY hermit-c"),
    ])
    assert len(shas) == len(issuers), (shas, issuers)
    assert shas == [FULL] and issuers == [(FULL, "hermit-c")], (shas, issuers)
