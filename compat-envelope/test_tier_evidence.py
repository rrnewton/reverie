#!/usr/bin/env python3
"""Both directions for the tier-evidence check.

The negative half is the point of the module, but the positive half is what
stops it being useless: a checker that refused every row would satisfy every
negative test below and destroy the tier. So every refusal here is paired with a
row that differs ONLY in the field under test and MUST be upheld.

The planted case the task named -- a cell claiming FULL while missing stdout --
is `test_NEGATIVE_full_missing_stdout_is_rejected`. It is not hypothetical: six
such rows were live in `scorecard.csv` on 2026-08-07 and the vocabulary gate
scored all six as qualified green.
"""

from __future__ import annotations

import datetime as _dt
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("te", HERE / "tier_evidence.py")
te = importlib.util.module_from_spec(_spec)
# See the note in tier_evidence._load: a dataclass in a module that is not in
# sys.modules cannot resolve __module__ and fails to construct.
sys.modules["te"] = te
_spec.loader.exec_module(te)

NOW = _dt.datetime(2026, 8, 7, tzinfo=_dt.timezone.utc)

HEADER = ("test_id,test_mode,backend,outcome,stdout_parity,compared_log_messages,"
          "stack_parity,heap_parity,duration_ms,comparison_tier")
FULL = te.FULL
SPOT = te.SPOT_CHECK


def run(rows, ledger_rows=(), *, header=HEADER, now=NOW, cadence_days=14):
    """Check `rows` in a throwaway root; returns the Report."""
    tmp = tempfile.mkdtemp()
    root = Path(tmp)
    (root / "probe-scorecard.csv").write_text("\n".join((header, *rows)) + "\n")
    ledger = root / "ledger.csv"
    ledger.write_text("\n".join((
        "test_id,test_mode,backend,duration_ms,spot_check_utc,hermit_sha,result,detail",
        *ledger_rows)) + "\n")
    return te.check(root, now=now, cadence_days=cadence_days, ledger_path=ledger)


def full_row(name, stdout="1", info="348|348", stack="1", heap="1"):
    return f"{name},verify,ptrace,pass,{stdout},{info},{stack},{heap},120,{FULL}"


def spot_row(name):
    return f"{name},verify,ptrace,pass,1,348|348,,,9000,{SPOT}"


def receipt(name, when="2026-08-05T00:00:00Z", sha="abc1234def"):
    return f"{name},verify,ptrace,9000,{when},{sha},PASS,receipt"


class FullTier(unittest.TestCase):
    def test_POSITIVE_a_complete_full_claim_is_upheld(self):
        """Without this the negatives below prove nothing."""
        r = run([full_row("ok")])
        self.assertEqual((r.claims, r.upheld, len(r.violations)), (1, 1, 0))

    def test_NEGATIVE_full_missing_stdout_is_rejected(self):
        """THE PLANTED CASE. Six of these were live and scored qualified green."""
        r = run([full_row("no-stdout", stdout="")])
        self.assertEqual(r.upheld, 0)
        self.assertEqual(len(r.violations), 1)
        self.assertIn("missing:stdout", "; ".join(r.violations[0].reasons))

    def test_NEGATIVE_full_missing_info_log_is_rejected(self):
        r = run([full_row("no-info", info="")])
        self.assertIn("missing:info_log", "; ".join(r.violations[0].reasons))

    def test_NEGATIVE_an_info_comparison_of_zero_records_is_not_evidence(self):
        """`0|0` is a comparison that compared nothing; non-blank must not mean measured."""
        r = run([full_row("zero-info", info="0|0")])
        self.assertIn("empty-comparison:info_log", "; ".join(r.violations[0].reasons))

    def test_NEGATIVE_every_missing_verdict_component_is_named(self):
        """A reader must learn everything to fix, not discover it one run at a time."""
        r = run([full_row("nothing", stdout="", info="", stack="", heap="")])
        joined = "; ".join(r.violations[0].reasons)
        for component in ("stdout", "info_log"):
            self.assertIn(component, joined)

    def test_optional_mode_columns_do_not_determine_base_green(self):
        narrow = ("test_id,test_mode,backend,outcome,stdout_parity,"
                  "compared_log_messages,duration_ms,comparison_tier")
        r = run([f"no-cols,verify,ptrace,pass,1,348|348,120,{FULL}"], header=narrow)
        self.assertEqual((r.claims, r.upheld, r.violations), (1, 1, []))


class SpotCheckTier(unittest.TestCase):
    def test_POSITIVE_spot_check_with_verdict_is_upheld(self):
        r = run([spot_row("good")], [receipt("good")])
        self.assertEqual((r.claims, r.upheld), (1, 1))

    def test_mode_receipt_age_does_not_override_the_existing_verdict(self):
        for ledger_rows in ((), [receipt("s", when="2026-06-01T00:00:00Z")],
                            [receipt("s", sha="gf89c69766371-dirty")]):
            r = run([spot_row("s")], ledger_rows)
            self.assertEqual((r.claims, r.upheld, r.violations), (1, 1, []))

    def test_spot_check_still_requires_stdout_and_info_EVERY_run(self):
        """Both qualifying tiers still require stdout and INFO verdict evidence."""
        row = f"x,verify,ptrace,pass,,348|348,,,9000,{SPOT}"
        r = run([row], [receipt("x")])
        self.assertIn("missing:stdout", "; ".join(r.violations[0].reasons))


class ScopeAndCounting(unittest.TestCase):
    def test_a_non_qualifying_tier_is_not_a_claim(self):
        """legacy-unqualified asserts nothing, so it is neither upheld nor violated."""
        r = run([f"leg,verify,ptrace,pass,,,,,120,legacy-unqualified"])
        self.assertEqual((r.rows, r.claims, len(r.violations)), (1, 0, 0))

    def test_counts_carry_their_denominator(self):
        r = run([full_row("a"), full_row("b", stdout=""),
                 f"c,verify,ptrace,pass,,,,,120,legacy-unqualified"])
        self.assertEqual(r.rows, 3)
        self.assertEqual(r.claims, 2)          # the legacy row is not a claim
        self.assertEqual(r.upheld, 1)
        self.assertEqual(len(r.violations), 1)

    def test_an_empty_population_is_REFUSED_not_a_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(te.PopulationError):
                te.check(Path(tmp), now=NOW, cadence_days=14,
                         ledger_path=Path(tmp) / "nope.csv")


class DirtyPredicate(unittest.TestCase):
    def test_clean_shas_are_not_dirty(self):
        for sha in ("abc1234", "a" * 40, "g1234567890a"):
            self.assertFalse(te.is_dirty_sha(sha), sha)

    def test_dirty_spellings_are_caught(self):
        for sha in ("", "   ", "gf89c69766371-dirty", "abc-DIRTY", "dirty"):
            self.assertTrue(te.is_dirty_sha(sha), sha)


class DebtRegister(unittest.TestCase):
    """The ratchet that lets this gate be WIRED without a day-one red.

    Both directions, because a register that only ever excuses is the silence this
    checker exists to remove. It must accept known debt, refuse NEW debt, and
    refuse ITSELF once the debt is gone.
    """

    def _run(self, rows, entries, *, header=HEADER):
        import json
        tmp = Path(tempfile.mkdtemp())
        (tmp / "probe-scorecard.csv").write_text("\n".join((header, *rows)) + "\n")
        ledger = tmp / "ledger.csv"
        ledger.write_text("test_id,test_mode,backend,duration_ms,spot_check_utc,"
                          "hermit_sha,result,detail\n")
        path = tmp / "baseline.json"
        path.write_text(json.dumps({"unevidenced_claims": entries}))
        register = te.load_baseline(path)
        return te.check(tmp, now=NOW, cadence_days=14, ledger_path=ledger,
                        baseline=register, baseline_path=path)

    @staticmethod
    def _entry(name, why="known debt, tracked elsewhere"):
        return {"file": "probe-scorecard.csv", "test_id": name, "test_mode": "verify",
                "backend": "ptrace", "tier": FULL, "why": why}

    def test_registered_debt_is_counted_and_printed_but_does_not_fail(self):
        report = self._run([full_row("g", stdout="")], [self._entry("g")])
        self.assertEqual(report.claims, 1)
        self.assertEqual(report.upheld, 0)          # still honestly 0 evidenced
        self.assertEqual(len(report.registered), 1)  # counted
        self.assertEqual(report.violations, [])      # but not fatal
        self.assertEqual(report.stale, [])
        self.assertIn("registered debt", report.render())
        self.assertIn("fully evidenced        : 0 of 1", report.render())

    def test_an_UNREGISTERED_unevidenced_claim_still_fails(self):
        """The ratchet. A seventh claim cannot join the six by appearing."""
        report = self._run([full_row("g", stdout=""), full_row("h", stdout="")],
                           [self._entry("g")])
        self.assertEqual(len(report.registered), 1)
        self.assertEqual(len(report.violations), 1)
        self.assertEqual(report.violations[0].test_id, "h")

    def test_an_entry_whose_claim_became_EVIDENCED_is_stale_and_fails(self):
        """A register must not outlive its reason, or it becomes the new silence."""
        report = self._run([full_row("g")], [self._entry("g")])
        self.assertEqual(report.upheld, 1)
        self.assertEqual(report.registered, [])
        self.assertEqual(len(report.stale), 1)
        self.assertIn("now EVIDENCED", report.stale[0])

    def test_an_entry_whose_row_vanished_is_stale_and_fails(self):
        report = self._run([full_row("other")], [self._entry("deleted-guest")])
        self.assertEqual(len(report.stale), 1)
        self.assertIn("no row carries this claim", report.stale[0])

    def test_identity_is_not_the_line_number(self):
        """Rows are appended and rewritten; keying on a line would re-point debt."""
        row = {"test_id": "g", "test_mode": "verify", "backend": "ptrace",
               "comparison_tier": FULL}
        self.assertEqual(te.claim_identity("s.csv", row),
                         "s.csv|g|verify|ptrace|" + FULL)

    def test_an_entry_without_a_reason_is_refused(self):
        import json
        tmp = Path(tempfile.mkdtemp())
        path = tmp / "b.json"
        entry = self._entry("g")
        del entry["why"]
        path.write_text(json.dumps({"unevidenced_claims": [entry]}))
        with self.assertRaises(te.PopulationError):
            te.load_baseline(path)

    def test_no_baseline_keeps_the_uncompromising_verdict(self):
        """Bare `tier_evidence.py` must be unchanged by any of this."""
        report = run([full_row("g", stdout="")])
        self.assertEqual(len(report.violations), 1)
        self.assertEqual(report.registered, [])
        self.assertIsNone(report.baseline)

    def test_the_shipped_register_matches_the_shipped_scorecards(self):
        """The live register must describe live debt -- no stale entries, and no
        unregistered claim. This is the assertion that fails if someone lands a new
        unevidenced claim, or fixes one and forgets to prune the register."""
        baseline = HERE / "tier-evidence-baseline.json"
        if not baseline.exists():
            self.skipTest("no shipped register")
        register = te.load_baseline(baseline)
        report = te.check(HERE, now=_dt.datetime.now(_dt.timezone.utc),
                          cadence_days=14, ledger_path=te._cadence.LEDGER,
                          baseline=register, baseline_path=baseline)
        self.assertEqual(report.violations, [], "unregistered unevidenced tier claim")
        self.assertEqual(report.stale, [], "debt-register entry no longer describes live debt")


if __name__ == "__main__":
    unittest.main(verbosity=2)
