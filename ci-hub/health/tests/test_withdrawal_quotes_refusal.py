"""A withdrawal retires the refusal it NAMES, and only that one.

Built on hermit#2625's REAL comments -- real bodies, real ids, real head -- because
a synthetic fixture would have shipped a fix that does not work.

⚠️ WHAT THE REAL PAIR TAUGHT THAT INVENTED INPUT WOULD NOT.
The cheap design is to read the issuer out of the `[hermit2, <agent>, ...]`
disclosure tag and let a withdrawal retire only its own refusals. On the real pair
that FAILS: of the two refusals at `f013ac4b1a31`, `hermit-012`'s carries a tag and
`hermit-015`'s carries NONE. The refusal that must keep binding is exactly the one
with no readable issuer. A fixture with tags on both would have passed.

Two refusals from different agents also have BYTE-IDENTICAL marker text
(`CHANGES-REQUESTED-AT: claude <sha>`), so "quote the exact text" cannot separate
them. The comment id is assigned by GitHub, is always present, and is unique -- so
it is the only identity available.

WHAT IS PINNED, all four against the same real bodies:
  * a withdrawal naming one refusal retires that one and leaves the other BLOCKING
  * a withdrawal naming NOTHING keeps the previous lane-wide behaviour, because
    retiring nothing would flip every live head carrying an existing unquoted
    withdrawal to BLOCKED at once -- and the lane-wide path already reports the
    one-for-many clearance, so the ambiguous case is visible while the precise
    spelling is adopted
  * a lane that adopts the precise spelling opts OUT of the lane-wide one, or the
    two coexist as an OR and naming a refusal changes nothing
  * ⚠️ naming the OTHER agent's refusal does not retire your own -- the control
    that proves the id is MATCHED rather than merely PRESENT. Without it, any
    `RETIRES <digits>` would satisfy the first case.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import pathlib
import unittest

HERE = pathlib.Path(__file__).resolve().parent
GATE = HERE.parents[1] / "bin" / "changes-requested-gate"
FIXTURE = HERE / "fixtures" / "pr2625_withdrawal_pair.json"


def _gate():
    loader = importlib.machinery.SourceFileLoader("gate_under_test", str(GATE))
    spec = importlib.util.spec_from_loader("gate_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class WithdrawalNamesItsRefusal(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = _gate()
        self.grammar = self.gate._load_grammar()
        data = json.loads(FIXTURE.read_text())
        self.head = data["head"]
        by_role = {c["role"]: c for c in data["comments"]}
        self.r12 = by_role["refusal_012"]
        self.r15 = by_role["refusal_015"]
        self.w12 = by_role["withdrawal_012"]

    def _quote(self, target_id: int) -> str:
        return f"{self.w12['body']}\n\nRETIRES {target_id}\n"

    def _records(self, bodies: list[str], ids: list[int]) -> list[dict]:
        records = [
            {
                "id": record_id,
                "body": body,
                "user": {
                    "login": self.grammar.comment_author(body) or "fixture-reviewer"
                },
                "_review_surface": "issue-comment",
                "created_at": f"2026-09-04T00:00:{position:02d}Z",
                "updated_at": f"2026-09-04T00:00:{position:02d}Z",
            }
            for position, (record_id, body) in enumerate(zip(ids, bodies), 1)
        ]
        return self.grammar.annotate_current_review_authority(
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

    def test_the_real_pair_still_blocks_when_only_one_refusal_is_named(self) -> None:
        """THE DELIVERABLE. hermit-012 withdraws by quotation; hermit-015 still blocks."""
        bodies = [self.r12["body"], self.r15["body"], self._quote(self.r12["id"])]
        ids = [self.r12["id"], self.r15["id"], self.w12["id"]]
        rc, msg = self.gate.evaluate(
            bodies,
            self.head,
            ids=ids,
            records=self._records(bodies, ids),
        )
        self.assertEqual(rc, self.gate.BLOCKED, msg)
        self.assertIn("1 unresolved refusal(s) bind the exact head", msg)
        self.assertIn("1 other refusal(s) WERE answered", msg)

    def test_naming_the_other_agents_refusal_does_not_retire_your_own(self) -> None:
        """⚠️ THE CONTROL. Proves the id is matched, not merely present."""
        bodies = [self.r12["body"], self.r15["body"], self._quote(self.r15["id"])]
        ids = [self.r12["id"], self.r15["id"], self.w12["id"]]
        rc, msg = self.gate.evaluate(
            bodies,
            self.head,
            ids=ids,
            records=self._records(bodies, ids),
        )
        self.assertEqual(rc, self.gate.BLOCKED, msg)

    def test_a_quoted_withdrawal_still_clears_when_it_is_the_only_refusal(self) -> None:
        """And it must not over-block: naming your own sole refusal clears."""
        bodies = [self.r12["body"], self._quote(self.r12["id"])]
        ids = [self.r12["id"], self.w12["id"]]
        rc, msg = self.gate.evaluate(
            bodies,
            self.head,
            ids=ids,
            records=self._records(bodies, ids),
        )
        self.assertEqual(rc, self.gate.OK, msg)

    def test_one_generic_withdrawal_cannot_clear_two_refusals(self) -> None:
        """A generic withdrawal clears at most its issuer's refusal."""
        rc, msg = self.gate.evaluate(
            [self.r12["body"], self.r15["body"], self.w12["body"]],
            self.head,
            ids=[self.r12["id"], self.r15["id"], self.w12["id"]],
        )
        self.assertEqual(rc, self.gate.BLOCKED, msg)
        self.assertIn("1 refusal(s) bound", msg)

    def test_the_fixture_is_the_real_pair_and_keeps_the_property_that_matters(self) -> None:
        """If someone regenerates the fixture from tagged comments, the issuer-based
        design becomes viable again and these tests stop testing what they claim."""
        self.assertNotIn("[hermit2,", self.r15["body"][:200],
                         "hermit-015's refusal must remain the UNTAGGED one")
        self.assertIn("[hermit2,", self.w12["body"][:200])
        self.assertNotEqual(self.r12["id"], self.r15["id"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
