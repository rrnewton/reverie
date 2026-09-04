"""`comment_author` must find a disclosure tag sitting behind a role tag -- and
must never take one out of somebody else's quoted comment.

⚠️ WHY THIS MATTERS MORE THAN A PARSER DETAIL. Generic approvals and unquoted
withdrawals remain issuer-bound, while an exact RETIRES record requires a valid
reviewer/coordinator disclosure. Losing that tag either strands ordinary review
history or makes an exact resolution carry no authority.

⚠️ AND THE OPPOSITE FAILURE IS WORSE. Reading a tag out of a QUOTED comment would
attribute one agent's refusal to another and let a generic withdrawal clear it.
Every quoting shape below is asserted to yield None.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from approval_binding import comment_author  # noqa: E402

ROLE = "[review agent, claude-opus-5]"
TAG = "[hermit2, hermit-016, devbig014, role=review]"


def test_a_bare_disclosure_tag_still_reads(  ) -> None:
    assert comment_author(f"{TAG}\nbody\n") == "hermit-016"


def test_a_disclosure_tag_behind_a_role_tag_reads() -> None:
    """The exact shape of the three real refusals."""
    assert comment_author(f"{ROLE} {TAG}\nbody\n") == "hermit-016"


def test_a_disclosure_tag_on_a_later_line_reads() -> None:
    """hermit#2692's refusal: a heading first, the tag on line 2."""
    body = "## CHANGES-REQUESTED-AT: claude " + "a" * 40 + f"\n\n{ROLE} {TAG} — non-author\n"
    assert comment_author(body) == "hermit-016"


def test_several_leading_role_tags_still_reach_the_disclosure() -> None:
    assert comment_author(f"[a b, c] [d e, f] {TAG}\n") == "hermit-016"


def test_the_first_disclosure_tag_on_a_line_wins() -> None:
    """⚠️ A greedy prefix would return the LAST tag -- someone else's."""
    body = "[hermit2, hermit-004, x, y] [hermit2, hermit-016, x, y]\n"
    assert comment_author(body) == "hermit-004"


# ---- the attribution-transfer cases: every one must be None ----------------

def test_a_blockquoted_tag_is_not_the_author() -> None:
    body = f"{ROLE} agent(hermit-030) writing.\n\n> {TAG}\n> I refused this.\n"
    assert comment_author(body) is None


def test_an_indented_blockquoted_tag_is_not_the_author() -> None:
    assert comment_author(f"{ROLE}\n\n  > {TAG}\n") is None


def test_an_indented_paste_is_not_the_author() -> None:
    """⚠️ THE ONE THAT CAUGHT THE FIRST DRAFT. Separating the leading groups with
    `\\s*` let the prefix cross newlines and reach this from the role tag on line
    0. `[ \\t]*` keeps the match on one line."""
    assert comment_author(f"{ROLE}\n\n    {TAG}\n") is None


def test_a_quote_marker_after_a_role_tag_on_one_line_is_not_the_author() -> None:
    assert comment_author(f"{ROLE} > {TAG}\n") is None


def test_a_role_tag_alone_names_nobody() -> None:
    assert comment_author(f"{ROLE}\nbody\n") is None


def test_a_two_field_bracket_is_not_a_disclosure() -> None:
    """`[coordinator, FABLE] [claude-coord-fable, devbig030]` -- hermit#2176.
    Neither group has the three fields a disclosure needs, so it stays None."""
    assert comment_author("[coordinator, FABLE] [claude-coord-fable, devbig030]\n") is None
