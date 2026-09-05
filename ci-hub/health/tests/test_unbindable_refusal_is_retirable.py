"""A refusal that names no head must still be RETIRABLE, or it blocks forever.

THE DEFECT THIS PINS. A refusal marker carrying no readable sha -- `CHANGES-REQUESTED`
written at column 0, with or without trailing prose -- lands in `unbindable` and the
gate returns rc=2 INDETERMINATE. That is the right verdict: a marker that cannot be
bound is not a marker that does not apply.

⚠️ BUT IT USED TO BE UNRETIRABLE BY ANY MEANS AT ALL, AND THAT MADE IT PERMANENT.
It names no sha, so pushing never stops it naming the head. And the withdrawal
machinery runs only over refusals that DO name the head, so nothing sha-less ever
reached it. Measured against a bare refusal plus a withdrawal in five spellings --
bare, lane-only, lane+head-sha, and an explicit `RETIRES <id>` -- ALL FIVE returned
INDETERMINATE, while the same withdrawal took a well-formed refusal from BLOCKED to
OK. The gate's own message said "or withdraw it", and that could not be done. The
only real exit was persuading someone to edit their comment.

⚠️ THE FIX IS DELIBERATELY THE NARROWEST ONE, AND THE MUST-NOT-CLEAR CASES BELOW ARE
THE POINT. Only `RETIRES <comment-id>` retires a sha-less refusal:

  * it names exactly one comment, so it cannot over-clear the way a lane-wide
    withdrawal would;
  * it does not depend on the disclosure tag, which is absent roughly a third of
    the time, so it cannot be defeated by a missing issuer;
  * and the blunt spellings stay powerless, because a withdrawal that names neither
    a head nor a comment is not evidence about a marker that named no head either.

A test that only checked "RETIRES clears it" would pass against a gate that cleared
on ANY withdrawal, which is the fail-open version of this fix. So the three
must-not-clear cases and the wrong-id case carry as much weight as the positive one.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib
import unittest

GATE = pathlib.Path(__file__).resolve().parents[2] / "bin" / "changes-requested-gate"
HEAD = "e1f2a3b4c5d60718293a4b5c6d7e8f9012345678"
TAG = "[hermit2, hermit-030, unresolved, devbig014, role=review]\n"

OK, BLOCKED, INDETERMINATE = 0, 1, 2

# Two ids that are far apart and obviously synthetic, so a wrong-id test cannot
# pass by colliding with the right one.
REFUSAL_ID = 900001
WITHDRAWAL_ID = 900002
UNRELATED_ID = 900099


def _gate():
    loader = importlib.machinery.SourceFileLoader("gate_under_test", str(GATE))
    spec = importlib.util.spec_from_loader("gate_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


# A refusal issued in earnest that names no head. This is the shape a reviewer
# produces by opening with the token and then explaining, which is ordinary.
BARE_REFUSAL = TAG + "CHANGES-REQUESTED because the boundary test is off the boundary."


def _withdrawal(body_tail: str = "") -> str:
    return TAG + "CHANGES-REQUESTED-WITHDRAWN-AT" + body_tail


class ShaLessRefusalStaysClosedUnlessPreciselyRetired(unittest.TestCase):
    def _evaluate(self, bodies, ids):
        gate = _gate()
        grammar = gate._load_grammar()
        records = [
            {
                "id": record_id,
                "body": body,
                "user": {
                    "login": grammar.comment_author(body) or "fixture-reviewer"
                },
                "_review_surface": "issue-comment",
                "created_at": f"2026-09-04T08:{position:02d}:00Z",
                "updated_at": f"2026-09-04T08:{position:02d}:00Z",
            }
            for position, (record_id, body) in enumerate(zip(ids, bodies))
        ]
        records = grammar.annotate_current_review_authority(
            records,
            "rrnewton/hermit",
            lambda path: {
                "role_name": "write",
                "permission": "write",
                "user": {
                    "login": path.split("/collaborators/", 1)[1].split("/", 1)[0]
                },
            },
        )
        return gate.evaluate(bodies, HEAD, ids, records=records)

    def test_alone_it_is_indeterminate_not_a_pass(self):
        code, message = self._evaluate([BARE_REFUSAL], [REFUSAL_ID])
        self.assertEqual(code, INDETERMINATE, message)
        self.assertIn("no readable sha", message)

    # ---- must NOT clear: the fail-open cases -------------------------------

    def test_a_bare_withdrawal_does_not_retire_it(self):
        code, message = self._evaluate(
            [BARE_REFUSAL, _withdrawal()], [REFUSAL_ID, WITHDRAWAL_ID]
        )
        self.assertEqual(
            code, INDETERMINATE,
            "a withdrawal naming neither a head nor a comment retired a marker that "
            f"named no head: {message}",
        )

    def test_a_lane_only_withdrawal_does_not_retire_it(self):
        code, message = self._evaluate(
            [BARE_REFUSAL, _withdrawal(": claude")], [REFUSAL_ID, WITHDRAWAL_ID]
        )
        self.assertEqual(code, INDETERMINATE, message)

    def test_a_lane_and_head_withdrawal_does_not_retire_it(self):
        # This one is the closest miss: it is a perfectly well-formed withdrawal.
        # It still must not reach a refusal that never said which head it meant.
        code, message = self._evaluate(
            [BARE_REFUSAL, _withdrawal(f": claude {HEAD}")], [REFUSAL_ID, WITHDRAWAL_ID]
        )
        self.assertEqual(code, INDETERMINATE, message)

    def test_retiring_a_different_comment_does_not_retire_it(self):
        code, message = self._evaluate(
            [BARE_REFUSAL, _withdrawal(f": claude {HEAD}\nRETIRES {UNRELATED_ID}")],
            [REFUSAL_ID, WITHDRAWAL_ID],
        )
        self.assertEqual(
            code, INDETERMINATE,
            f"a withdrawal naming a DIFFERENT comment retired this one: {message}",
        )

    # ---- must clear: the block is retirable at all -------------------------

    def test_a_withdrawal_naming_the_refusing_comment_retires_it(self):
        code, message = self._evaluate(
            [BARE_REFUSAL, _withdrawal(f": claude {HEAD}\nRETIRES {REFUSAL_ID}")],
            [REFUSAL_ID, WITHDRAWAL_ID],
        )
        self.assertEqual(
            code, OK,
            "a sha-less refusal named by its own RETIRES withdrawal is still "
            f"blocking, so the block remains permanent: {message}",
        )


class TheWellFormedPathIsUnchanged(unittest.TestCase):
    """⚠️ THE CONTROL. If these ever change, the fix above has leaked out of the
    sha-less case and into the ordinary one, which is where all the real refusals
    live."""

    def _evaluate(self, bodies):
        return _gate().evaluate(bodies, HEAD)

    def test_a_well_formed_refusal_still_blocks(self):
        code, _ = self._evaluate([TAG + f"CHANGES-REQUESTED-AT: claude {HEAD}"])
        self.assertEqual(code, BLOCKED)

    def test_a_current_head_withdrawal_clears_a_well_formed_refusal(self):
        code, _ = self._evaluate(
            [
                TAG + f"CHANGES-REQUESTED-AT: claude {HEAD}",
                TAG + f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {HEAD}",
            ]
        )
        self.assertEqual(code, OK)

    def test_prose_with_no_marker_is_still_clear(self):
        code, _ = self._evaluate(["Just a comment about the change."])
        self.assertEqual(code, OK)


if __name__ == "__main__":
    unittest.main()
