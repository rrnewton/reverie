#!/usr/bin/env python3
"""Brackets for the gate-completeness fix.

The required acceptance test is the planted 5-of-6 partial: it must REFUSE.
Both directions are bracketed, because a completeness rule that refuses
everything is as useless as one that accepts everything.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gate_completeness as G
from flake_class import FULL_GATES_EXPECTED


def row(**kw):
    base = {"schema_version": 5, "profile": "full", "result": "pass"}
    base.update(kw)
    return base


class PlantedPartialTest(unittest.TestCase):
    """THE bug: a short run certified complete by an inferred denominator."""

    def test_5_of_6_with_undeclared_contract_is_REFUSED(self) -> None:
        """The acceptance test for this fix.

        Before: gate_counts -> (5,5) via the hardcode, is_qualified -> True.
        A 5-of-6 partial wearing a complete-green badge.
        """
        ok, why = G.gates_complete(row(gates_run=5))          # gates_expected ABSENT
        self.assertFalse(ok)
        self.assertIn("did not declare gates_expected", why)

    def test_5_of_6_with_DECLARED_contract_is_refused_as_a_short_run(self) -> None:
        ok, why = G.gates_complete(row(gates_run=5, gates_expected=6))
        self.assertFalse(ok)
        self.assertIn("short run: 5 of 6", why)

    def test_the_inferred_denominator_is_what_hid_it(self) -> None:
        """Pin the mechanism, not just the symptom: with the contract absent the
        resolver still yields the hardcode, and that is exactly what must stop
        counting as proof."""
        ran, expected, source = G.resolve_gates(row(gates_run=5))
        self.assertEqual((ran, expected), (5, FULL_GATES_EXPECTED))
        self.assertEqual(source, G.INFERRED)


class PositiveControlTest(unittest.TestCase):
    """A rule that refuses everything is useless."""

    def test_complete_declared_run_passes(self) -> None:
        ok, why = G.gates_complete(row(gates_run=6, gates_expected=6))
        self.assertTrue(ok, why)
        self.assertIn("declared", why)

    def test_over_run_passes(self) -> None:
        """ran > expected is a complete run, not a short one -- the same
        allowance flake_class.is_truncated makes, and for the same reason."""
        ok, why = G.gates_complete(row(gates_run=7, gates_expected=6))
        self.assertTrue(ok, why)

    def test_legacy_receipt_keeps_the_inference(self) -> None:
        """RATCHET, not retroactive invalidation: pre-declaration schemas never
        recorded the field and never can, so they keep the legacy path rather
        than being voided by a rule they could not have satisfied."""
        ok, why = G.gates_complete(row(schema_version=3, gates_run=5))
        self.assertTrue(ok, why)
        self.assertIn(G.INFERRED, why)

    def test_legacy_short_run_still_refused(self) -> None:
        """The legacy allowance is not a blanket pass."""
        ok, why = G.gates_complete(row(schema_version=3, gates_run=2, gates_expected=6))
        self.assertFalse(ok)
        self.assertIn("short run", why)

    def test_typed_empty_bucket_accounts_without_becoming_pass(self) -> None:
        receipt = row(
            gates_run=5,
            gates_expected=6,
            skipped_nodes=1,
            intentional_skipped_nodes=[
                {
                    "name": "privileged-e2e.manifest_applications",
                    "reason": "empty-manifest-bucket",
                }
            ],
            dependency_skipped_nodes=[],
            unaccounted_nodes=[],
            gates=[{"name": f"g{i}", "result": "pass"} for i in range(5)],
        )
        ok, why = G.gates_complete(receipt)
        self.assertTrue(ok, why)
        self.assertIn("skips are not PASS", why)
        self.assertEqual(receipt["gates_run"], 5)
        self.assertEqual(len(receipt["gates"]), 5)

    def test_nonempty_peer_still_requires_execution(self) -> None:
        ok, why = G.gates_complete(row(gates_run=5, gates_expected=6))
        self.assertFalse(ok)
        self.assertIn("short run", why)


class IntentionalSkipRefusalTest(unittest.TestCase):
    def typed(self, **overrides):
        value = row(
            gates_run=5,
            gates_expected=6,
            skipped_nodes=1,
            intentional_skipped_nodes=[
                {
                    "name": "privileged-e2e.manifest_applications",
                    "reason": "empty-manifest-bucket",
                }
            ],
            dependency_skipped_nodes=[],
            unaccounted_nodes=[],
        )
        value.update(overrides)
        return value

    def test_unknown_reason_refused(self) -> None:
        value = self.typed()
        value["intentional_skipped_nodes"][0]["reason"] = "looks-empty"
        ok, why = G.gates_complete(value)
        self.assertFalse(ok)
        self.assertIn("unknown reason", why)

    def test_dependency_skip_refused(self) -> None:
        ok, why = G.gates_complete(
            self.typed(dependency_skipped_nodes=["downstream.node"])
        )
        self.assertFalse(ok)
        self.assertIn("dependency_skipped_nodes is nonempty", why)

    def test_unaccounted_node_refused(self) -> None:
        ok, why = G.gates_complete(self.typed(unaccounted_nodes=["lost.node"]))
        self.assertFalse(ok)
        self.assertIn("unaccounted_nodes is nonempty", why)

    def test_mismatched_accounting_refused(self) -> None:
        ok, why = G.gates_complete(self.typed(gates_expected=7))
        self.assertFalse(ok)
        self.assertIn("accounting mismatch", why)

    def test_skip_count_cannot_be_hidden(self) -> None:
        ok, why = G.gates_complete(self.typed(skipped_nodes=0))
        self.assertFalse(ok)
        self.assertIn("skipped_nodes must equal", why)

    def test_typed_skip_cannot_replace_a_declared_contract(self) -> None:
        value = self.typed()
        del value["gates_expected"]
        ok, why = G.gates_complete(value)
        self.assertFalse(ok)
        self.assertIn("did not declare gates_expected", why)


class DegenerateInputTest(unittest.TestCase):
    def test_missing_counts_refused(self) -> None:
        self.assertFalse(G.gates_complete(row())[0])

    def test_vacuous_contract_refused(self) -> None:
        ok, why = G.gates_complete(row(gates_run=0, gates_expected=0))
        self.assertFalse(ok)
        self.assertIn("vacuous", why)

    def test_bool_gates_run_refused(self) -> None:
        """bool is an int in Python; True must not read as 'one gate ran'."""
        self.assertFalse(G.gates_complete(row(gates_run=True, gates_expected=1))[0])

    def test_checks_fallback_still_honoured(self) -> None:
        """`ran` legitimately falls back to `checks` when gates_run is absent."""
        ran, _e, _s = G.resolve_gates(row(checks=6, gates_expected=6))
        self.assertEqual(ran, 6)


if __name__ == "__main__":
    unittest.main()
