#!/usr/bin/env python3
"""The marker carries WHO issued it, so lane independence becomes measurable.

THE LANE IS NOT THE REVIEWER. `claude` and `codex` name a harness FAMILY, and a
fleet of nineteen agents shares those two names, so "both lanes bound this head"
has never been evidence that two reviewers looked at it. One agent can bind both
and the record reads as independent review. That is not a parsing bug -- the
binding simply did not carry who issued it, so no consumer could ask.

⚠️ THE ISSUER WAS ALREADY COMPUTED AND THEN THROWN AWAY. `lane_shas` reads
`comment_author(body)` for its citation logic and returns bare shas, so the one
fact needed to check independence was derived on every call and dropped at the
`return`. Most of what follows pins the plumbing of that fact, not a new parse.

WHAT THIS FILE DOES **NOT** CLAIM. Nothing emits `BY` yet. These tests pin that
every READER tolerates the clause, which is the precondition for a writer to be
allowed to produce one; see `test_readers_tolerate_before_any_writer_emits`. The
ordering is the safety argument, not a preference -- a marker is a string in a
GitHub comment read by several independent programs, with no schema handshake, so
the field has to be inert-but-understood everywhere before it is inert nowhere.

The sharpest case is `test_a_cross_author_quotation_is_now_measurable`, which is
the cell `test_marker_polarity_matrix.py` records as RESIDUAL and routes to this
task by name: agent B pasting agent A's approval binds a lane, because a repeat by
another author is indistinguishable from an independent second verdict when the
marker does not say who issued it.

Needs no network and no build: every case is a synthetic comment body.
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
    """Why the fallback earns its keep: it makes independence measurable on the
    markers that ALREADY EXIST, instead of only on ones written after this lands."""
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
# What the field is FOR.
# --------------------------------------------------------------------------

def test_one_agent_binding_both_lanes_is_now_visible() -> None:
    """⚠️ THE POINT OF THE CHANGE. Both lanes bound at one head -- the shape every
    consumer reads as "two independent reviews" -- issued by ONE agent. Before
    this, the two calls returned `[FULL]` and `[FULL]` and the record was
    indistinguishable from genuine independence. Note the field does not BLOCK
    this; it makes it reportable. Enforcement is a later, separate decision."""
    comments = [
        _comment("hermit-011", f"APPROVED-AT: claude {FULL} BY hermit-011"),
        _comment("hermit-011", f"APPROVED-AT: codex {FULL} BY hermit-011"),
    ]
    claude_shas, claude_by = _bind(comments, "claude")
    codex_shas, codex_by = _bind(comments, "codex")
    assert claude_shas == [FULL] and codex_shas == [FULL]
    assert {i for _, i in claude_by} == {i for _, i in codex_by} == {"hermit-011"}


def test_two_agents_binding_two_lanes_reads_as_independent() -> None:
    """The control for the case above: same shape, two issuers, and the pair is
    distinguishable. Without this the previous test only proves the field is
    constant, not that it DISCRIMINATES."""
    comments = [
        _comment("hermit-011", f"APPROVED-AT: claude {FULL} BY hermit-011"),
        _comment("hermit-030", f"APPROVED-AT: codex {FULL} BY hermit-030"),
    ]
    _, claude_by = _bind(comments, "claude")
    _, codex_by = _bind(comments, "codex")
    assert {i for _, i in claude_by} == {"hermit-011"}
    assert {i for _, i in codex_by} == {"hermit-030"}


def test_a_cross_author_quotation_is_now_measurable() -> None:
    """The RESIDUAL cell `test_marker_polarity_matrix.py` routes here by name.

    Agent B pastes agent A's approval. The existing issuance/citation rule cannot
    suppress it -- that rule keys on the COMMENT AUTHOR, and a repeat by a
    different author is indistinguishable from an independent second verdict. So
    it binds twice, and this test does NOT assert otherwise: changing the binding
    is a policy decision with its own head.

    What changes is that the second binding no longer LIES about its origin. Both
    entries report `hermit-a`, so a consumer can see one verdict cited twice
    rather than two reviews. Read the issuers, not the count."""
    comments = [
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", "Agreed with the analysis:",
                 f"APPROVED-AT: claude {FULL} BY hermit-a"),
    ]
    shas, issuers = _bind(comments)
    assert shas == [FULL, FULL], shas
    assert {i for _, i in issuers} == {"hermit-a"}, issuers


def test_the_same_paste_without_the_clause_still_reads_as_two_reviewers() -> None:
    """⚠️ THE LIMIT OF THIS CHANGE, ASSERTED SO IT IS NOT OVERSOLD. Strip the `BY`
    and the identical paste attributes to hermit-b via its disclosure tag, which
    is exactly the false-independence reading above. The field HELPS ONLY WHERE IT
    IS WRITTEN -- so tolerance is a precondition for a fix, not the fix."""
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
    """Two identity sources that disagreed about what an agent id looks like would
    resolve the same reviewer two ways. `_AGENT_ID` is why they cannot."""
    assert ab._AGENT_ID in ab._DISCLOSURE.pattern
    assert ab._AGENT_ID in ab._MARKER_RE.pattern


def test_marker_issuer_reports_absence_distinctly_from_a_non_marker() -> None:
    """`None` for a bare marker and `None` for a line that is not a marker at all
    are the same value, so a caller must not read `None` as "not a marker"."""
    assert ab.marker_issuer(f"APPROVED-AT: claude {FULL}") is None
    assert ab.marker_issuer("not a marker at all") is None
    assert ab.marker_issuer(f"APPROVED-AT: claude {FULL} BY hermit-011") == "hermit-011"


def test_the_issuer_is_case_normalised() -> None:
    """Agent ids are compared as strings by every consumer, so `Hermit-011` and
    `hermit-011` must not read as two reviewers."""
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
    """The sequence that makes the desync DANGEROUS rather than untidy: a stale
    issuer whose sha equals the live one reads as a second reviewer at the
    current head. Alignment must hold through it, in BOTH directions.

    ⚠️ AND THE TWO DIRECTIONS DIFFER, which is worth pinning on its own. After a
    revoke the SAME reviewer's re-approval at the same sha does NOT rebind -- their
    own re-post is a citation of their earlier verdict under the issuance rule --
    while a DIFFERENT reviewer's does. That is pre-existing behaviour, not
    something this change introduced; it is asserted here so the alignment fix
    cannot quietly alter it."""
    same = _bind([
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", f"CHANGES-REQUESTED-AT: claude {OTHER} BY hermit-b"),
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
    ])
    assert same == ([], []), same

    shas, issuers = _bind([
        _comment("hermit-a", f"APPROVED-AT: claude {FULL} BY hermit-a"),
        _comment("hermit-b", f"CHANGES-REQUESTED-AT: claude {OTHER} BY hermit-b"),
        _comment("hermit-c", f"APPROVED-AT: claude {FULL} BY hermit-c"),
    ])
    assert len(shas) == len(issuers), (shas, issuers)
    assert shas == [FULL] and issuers == [(FULL, "hermit-c")], (shas, issuers)
