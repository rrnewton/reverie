#!/usr/bin/env python3
"""The APPROVED-AT grammar has exactly ONE definition, and `lane_shas` uses it.

WHY THIS EXISTS. `approval_binding.py` is the component that settles disputes
about what a reviewer said, and it carried the approval grammar TWICE: the
canonical `_MARKER_RE`, and a second
`^APPROVED-AT:\\s*(claude|codex)\\s+([0-9a-f]{40})$` compiled locally inside
`lane_shas`. That is the same defect the module's own header records against
`ci-hub/bin/gh-merge-verified` -- two parsers for one grammar, which HAD ALREADY
DRIFTED -- and the same one hermit#2610 removed from `docs/PR_SWEEP_VERDICTS.md`,
where the restated copy had drifted four ways at once (decoration, block prefix,
inner whitespace, case) and missed seven real bindings.

⚠️ A COPY IS NOT DANGEROUS BECAUSE IT IS UNTIDY. It is dangerous because the two
copies answer differently on exactly the inputs nobody tested, and the authority
module is where that disagreement decides whether a refusal reads as an approval.

THE TEST IS BEHAVIOURAL, NOT A GREP, and that distinction is the point. Asserting
"the source contains one regex" passes the moment someone writes the second copy
with different whitespace. Instead these replace `parse_marker` and require
`lane_shas` to change its answer. A `lane_shas` that still recognises approvals
after `parse_marker` has been made blind is a `lane_shas` that has grown its own
grammar again -- which is the regression, and it cannot hide from this.
"""
import importlib.util
import os
import sys
from pathlib import Path

tool = os.environ.get("APPROVAL_BINDING_TOOL", "ci-hub/health/approval_binding.py")
if not Path(tool).is_absolute():
    tool = str(Path(__file__).resolve().parents[3] / tool)
spec = importlib.util.spec_from_file_location("ab_single", tool)
ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ab)

SHA = "a" * 40
OTHER = "b" * 40


def _comment(body):
    return [{"body": body, "html_url": "https://example.invalid/c/1"}]


def test_lane_shas_binds_a_canonical_approval():
    """The baseline. Without this, every negative below passes vacuously."""
    assert ab.lane_shas(_comment(f"APPROVED-AT: claude {SHA}"), "claude") == [SHA]


def test_lane_shas_stops_binding_when_parse_marker_goes_blind():
    """⚠️ THE ANTI-DUPLICATION ASSERTION.

    With `parse_marker` returning None for everything, a `lane_shas` that owns no
    grammar can bind nothing. If this ever returns `[SHA]` again, someone has
    reintroduced a local approval regex and the module has two parsers again.
    """
    original = ab.parse_marker
    try:
        ab.parse_marker = lambda line: None
        found = ab.lane_shas(_comment(f"APPROVED-AT: claude {SHA}"), "claude")
    finally:
        ab.parse_marker = original
    assert found == [], (
        "lane_shas bound an approval while parse_marker was blind, so it is "
        "parsing the grammar itself again -- that is the duplicate this guards"
    )


def test_lane_shas_follows_parse_marker_onto_a_shape_it_could_not_know():
    """The positive direction, which a blindness test alone cannot establish.

    A stub that recognises a token the real grammar rejects. `lane_shas` must
    bind it, which is only possible if it takes the classification from
    `parse_marker` rather than matching APPROVED-AT itself.
    """
    original = ab.parse_marker

    def stub(line):
        if line.strip() == f"BLESSED-BY: claude {OTHER}":
            return (ab.MARKER_APPROVED, "claude", OTHER)
        return None

    try:
        ab.parse_marker = stub
        found = ab.lane_shas(_comment(f"BLESSED-BY: claude {OTHER}"), "claude")
    finally:
        ab.parse_marker = original
    assert found == [OTHER], (
        "lane_shas did not follow parse_marker onto a new shape, so it is not "
        "actually delegating the grammar"
    )


def test_a_refusal_kind_from_parse_marker_never_binds_as_an_approval():
    """Routing through a multi-kind parser must not widen what binds.

    `parse_marker` recognises three kinds. `lane_shas` asks for approvals, so a
    refusal or a withdrawal reaching it must contribute nothing -- otherwise
    deduplicating the grammar would have turned a refusal into an approval, which
    is precisely the failure this whole module exists to prevent.
    """
    for kind in (ab.MARKER_REFUSED, ab.MARKER_WITHDRAWN):
        original = ab.parse_marker
        try:
            ab.parse_marker = lambda line, _k=kind: (_k, "claude", SHA)
            found = ab.lane_shas(_comment(f"{kind}: claude {SHA}"), "claude")
        finally:
            ab.parse_marker = original
        assert found == [], f"{kind} bound as an approval"


def test_the_other_lane_still_does_not_bind():
    """Delegation must not lose the lane filter."""
    assert ab.lane_shas(_comment(f"APPROVED-AT: codex {SHA}"), "claude") == []
    assert ab.lane_shas(_comment(f"APPROVED-AT: codex {SHA}"), "codex") == [SHA]


def test_decoration_and_case_still_bind_through_the_shared_parser():
    """The four axes the drifted docs copy got wrong, now answered in one place."""
    for body in (
        f"**APPROVED-AT: claude {SHA}**",
        f"## APPROVED-AT: claude {SHA}",
        f"- APPROVED-AT: claude {SHA}",
        f"approved-at: CLAUDE {SHA}",
        f"APPROVED-AT:   claude   {SHA}",
    ):
        assert ab.lane_shas(_comment(body), "claude") == [SHA], body


def test_a_blockquote_is_a_mention_not_this_comments_approval():
    assert ab.lane_shas(_comment(f"> APPROVED-AT: claude {SHA}"), "claude") == []


def test_refusal_policy_did_not_move_with_the_grammar():
    """`-AT` stays OPTIONAL for refusals and MANDATORY for approvals.

    The asymmetry is deliberate: a malformed refusal must still refuse, a
    malformed approval must still fail to bind. Deduplicating the approval
    grammar must not quietly route refusals through the stricter shape.
    """
    assert ab.lane_shas(_comment(f"CHANGES-REQUESTED: claude {SHA}"), "claude") == []
    assert ab.lane_shas(_comment(f"APPROVED: claude {SHA}"), "claude") == []
    # A refusal in the same comment clears an approval for that lane.
    both = f"APPROVED-AT: claude {SHA}\nCHANGES-REQUESTED-AT: claude {SHA}"
    assert ab.lane_shas(_comment(both), "claude") == []


if __name__ == "__main__":  # pragma: no cover
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"PASS {name}")
            except AssertionError as exc:
                failures += 1
                print(f"FAIL {name}: {exc}")
    sys.exit(1 if failures else 0)
