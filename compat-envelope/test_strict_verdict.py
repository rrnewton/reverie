#!/usr/bin/env python3
"""Mutation testing for the strict-component verdict producer.

BOTH HALVES, per component. Planting a wrong value proves the verdict fires;
confirming the legitimate stream still passes proves it does not fire on
everything. A verdict module that reports false unconditionally satisfies every
mutation below and is worthless.

Each mutation must also fail for its OWN reason. A suite where several mutations
die at one shared earlier check reports a perfect detection rate while leaving
the checks it meant to exercise unexecuted.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import strict_verdict as sv  # noqa: E402

PFX = "2026-08-07T00:00:00.123456Z  INFO detcore: "

DETLOG = [
    "DETLOG SCHEDRAND: seeding scheduler runqueue with seed 0",
    "DETLOG SYSCALL: [1] brk(NULL) = 0x5bd000",
    "DETLOG TIME: virtual clock advanced to 1000000",
]
# Real shape, from detcore/src/logdiff.rs:1049.
MEM = [
    "DETLOG [memory][detcore, dtid 3] 0x602000-0x623000 rw-p 0 0:0 0 [heap] -> " + "74b43f" * 10,
    "DETLOG [memory][detcore, dtid 3] 0x7ffffffdd000-0x7ffffffff000 rw-p 0 0:0 0 [stack] -> " + "7984d1" * 10,
]


def raw(records, prefix=PFX):
    return "".join(prefix + r + "\n" for r in records)


class ExtractionIsGrounded(unittest.TestCase):
    def test_stack_and_heap_are_separated(self):
        self.assertEqual(len(sv.extract(raw(MEM), "stack")), 1)
        self.assertEqual(len(sv.extract(raw(MEM), "heap")), 1)

    def test_memory_record_splits_into_address_and_content(self):
        got = sv.split_memory(sv.extract(raw(MEM), "heap")[0])
        self.assertIsNotNone(got)
        addr, content = got
        self.assertEqual(addr, "0x602000-0x623000")
        self.assertTrue(content.startswith("74b43f"))

    def test_detlog_includes_memory_records_too(self):
        self.assertEqual(len(sv.extract(raw(DETLOG + MEM), "detlog")), 5)


class LegitimatePopulationStillPasses(unittest.TestCase):
    """The half that proves this is not a blanket rejecter."""

    def test_identical_detlog_passes_with_denominator(self):
        v = sv.detlog_verdict(raw(DETLOG), raw(DETLOG))
        self.assertEqual(v["verdict"], sv.PASS)
        self.assertEqual(v["denominator_a"], 3)

    def test_identical_memory_passes_on_both_dimensions(self):
        for region in ("stack", "heap"):
            mv = sv.memory_verdict(raw(MEM), raw(MEM), region)
            self.assertEqual(mv["content"]["verdict"], sv.PASS, region)
            self.assertEqual(mv["address"]["verdict"], sv.PASS, region)

    def test_wall_clock_prefix_difference_does_not_fail(self):
        v = sv.detlog_verdict(raw(DETLOG, PFX), raw(DETLOG, "2026-08-07T09:59:59.999999Z  INFO detcore: "))
        self.assertEqual(v["verdict"], sv.PASS)

    def test_measured_baseline_sizes_pass(self):
        for n in (141, 368, 1245):
            s = [f"DETLOG SYSCALL: [{i}] read(3) = 64" for i in range(n)]
            v = sv.detlog_verdict(raw(s), raw(s))
            self.assertEqual((v["verdict"], v["denominator_a"]), (sv.PASS, n))


class PlantedMutations(unittest.TestCase):
    def test_M1_detlog_record_changed(self):
        m = list(DETLOG); m[1] = "DETLOG SYSCALL: [1] brk(NULL) = 0xDEADBEEF"
        v = sv.detlog_verdict(raw(DETLOG), raw(m))
        self.assertEqual((v["verdict"], v["differing"], v["common_prefix"]), (sv.FAIL, 1, 1))

    def test_M2_detlog_truncated_is_not_scored_clean(self):
        v = sv.detlog_verdict(raw(DETLOG), raw(DETLOG[:1]))
        self.assertEqual((v["verdict"], v["differing"]), (sv.FAIL, 2))

    def test_M3_heap_CONTENT_divergence_is_detected(self):
        m = [MEM[0].replace("74b43f" * 10, "ffffff" * 10), MEM[1]]
        mv = sv.memory_verdict(raw(MEM), raw(m), "heap")
        self.assertEqual(mv["content"]["verdict"], sv.FAIL)
        self.assertEqual(mv["address"]["verdict"], sv.PASS)  # address unchanged

    def test_M4_heap_ADDRESS_divergence_is_detected_SEPARATELY(self):
        """THE CONFLATION TEST. Address moves, content identical. A single
        whole-line boolean would report FAIL and blame content determinism."""
        m = [MEM[0].replace("0x602000-0x623000", "0x900000-0x921000"), MEM[1]]
        mv = sv.memory_verdict(raw(MEM), raw(m), "heap")
        self.assertEqual(mv["address"]["verdict"], sv.FAIL)
        self.assertEqual(mv["content"]["verdict"], sv.PASS)
        self.assertIsNone(mv["combined"])

    def test_M5_stack_content_divergence_is_detected(self):
        m = [MEM[0], MEM[1].replace("7984d1" * 10, "000000" * 10)]
        mv = sv.memory_verdict(raw(MEM), raw(m), "stack")
        self.assertEqual(mv["content"]["verdict"], sv.FAIL)

    def test_M6_absent_stack_records_are_NOT_MEASURED(self):
        """detlog_stack defaults FALSE. Without this guard the whole stack
        dimension is green by default on runs that emitted nothing."""
        mv = sv.memory_verdict(raw(DETLOG), raw(DETLOG), "stack")
        self.assertEqual(mv["content"]["verdict"], sv.NOT_MEASURED)
        self.assertIn("defaults to FALSE", mv["content"]["reason"])

    def test_M7_both_streams_empty_is_NOT_MEASURED_not_trivial_pass(self):
        v = sv.detlog_verdict("", "")
        self.assertEqual(v["verdict"], sv.NOT_MEASURED)

    def test_mutations_have_DISTINCT_signatures(self):
        sigs = set()
        for mutant in (
            raw([*DETLOG[:1], "DETLOG X", DETLOG[2]]),
            raw(DETLOG[:2]),
            raw(DETLOG + ["DETLOG EXTRA"]),
            "",
        ):
            v = sv.detlog_verdict(raw(DETLOG), mutant)
            self.assertNotEqual(v["verdict"], sv.PASS)
            sigs.add((v["verdict"], v["differing"], v["common_prefix"]))
        self.assertEqual(len(sigs), 4, f"collapsed: {sigs}")


class CrossBackendRefusesAVerdict(unittest.TestCase):
    def test_no_boolean_and_both_denominators(self):
        a = [f"DETLOG A{i}" for i in range(141)]
        b = [f"DETLOG A{i}" for i in range(368)]
        r = sv.cross_backend_prefix(raw(a), raw(b))
        self.assertFalse(r["comparable"])
        self.assertNotIn("verdict", r)
        self.assertEqual((r["denominator_a"], r["denominator_b"]), (141, 368))
        self.assertNotEqual(r["prefix_over_a_pct"], r["prefix_over_b_pct"])


class TierNeverOverclaims(unittest.TestCase):
    def test_subset_is_partial(self):
        t = sv.compose_tier({"stdout": sv.PASS, "stack": sv.PASS, "info_log": "", "heap": ""})
        self.assertTrue(t.startswith("partial:"))

    def test_all_four_is_strict_positive_control(self):
        t = sv.compose_tier({c: sv.PASS for c in sv.STRICT_COMPONENTS})
        self.assertEqual(t, "strict")

    def test_not_measured_does_not_count_toward_strict(self):
        v = {c: sv.PASS for c in sv.STRICT_COMPONENTS}
        v["heap"] = sv.NOT_MEASURED
        self.assertNotEqual(sv.compose_tier(v), "strict")

    def test_nothing_compared_is_blank(self):
        self.assertEqual(sv.compose_tier({"stdout": "", "heap": ""}), "")


class ScorecardFields(unittest.TestCase):
    def test_fields_carry_denominators_and_split_memory_dimensions(self):
        f = sv.scorecard_fields(
            stdout=sv.PASS, info_log=sv.PASS,
            detlog=sv.detlog_verdict(raw(DETLOG), raw(DETLOG)),
            stack=sv.memory_verdict(raw(MEM), raw(MEM), "stack"),
            heap=sv.memory_verdict(raw(MEM), raw(MEM), "heap"),
            stack_flag=True,
            heap_flag=True,
        )
        self.assertEqual(f["detlog_records"], 3)
        self.assertIn("stack_content_parity", f)
        self.assertIn("stack_address_parity", f)
        self.assertEqual(f["stack_parity"], "1")
        self.assertEqual(f["heap_parity"], "1")
        self.assertEqual(f["tier"], "strict")

    def test_flagged_but_unmeasured_memory_records_mode_not_an_outcome(self):
        fields = sv.scorecard_fields(
            stdout=sv.PASS,
            info_log=sv.PASS,
            stack=sv.memory_verdict(raw(DETLOG), raw(DETLOG), "stack"),
            heap=sv.memory_verdict(raw(DETLOG), raw(DETLOG), "heap"),
            stack_flag=True,
            heap_flag=True,
        )
        self.assertEqual(fields["stack_parity"], "1")
        self.assertEqual(fields["heap_parity"], "1")
        self.assertNotEqual(fields["tier"], "strict")

    def test_memory_parity_is_three_valued(self):
        historical = sv.scorecard_fields()
        new_without_flags = sv.scorecard_fields(stack_flag=False, heap_flag=False)
        matching = sv.scorecard_fields(
            stack=sv.memory_verdict(raw(MEM), raw(MEM), "stack"),
            heap=sv.memory_verdict(raw(MEM), raw(MEM), "heap"),
            stack_flag=True,
            heap_flag=True,
        )
        divergent_mem = [MEM[0], MEM[1].replace("7984d1" * 10, "000000" * 10)]
        divergent = sv.scorecard_fields(
            stack=sv.memory_verdict(raw(MEM), raw(divergent_mem), "stack"),
            stack_flag=True,
        )
        self.assertNotIn("stack_parity", historical)
        self.assertNotIn("heap_parity", historical)
        self.assertEqual(new_without_flags["stack_parity"], "0")
        self.assertEqual(new_without_flags["heap_parity"], "0")
        self.assertEqual(matching["stack_parity"], "1")
        self.assertEqual(matching["heap_parity"], "1")
        self.assertEqual(divergent["stack_parity"], "1")

    def test_flagged_memory_without_a_verdict_still_records_the_mode(self):
        fields = sv.scorecard_fields(stack_flag=True, heap_flag=True)
        self.assertEqual(fields["stack_parity"], "1")
        self.assertEqual(fields["heap_parity"], "1")

    def test_a_memory_verdict_with_an_explicitly_disabled_flag_refuses(self):
        with self.assertRaisesRegex(ValueError, "stack verdict.*flag was not passed"):
            sv.scorecard_fields(
                stack=sv.memory_verdict(raw(MEM), raw(MEM), "stack"),
                stack_flag=False,
            )

    def test_a_DECIDED_stdout_verdict_REACHES_A_COLUMN(self):
        """THE REGRESSION. This function used to accept `stdout`, compose the tier
        from it, and then drop it -- so a row could be tiered on a stdout
        comparison whose verdict appeared nowhere in the row. The tier asserted
        the comparison and the evidence for it never reached the CSV.
        """
        f = sv.scorecard_fields(stdout=sv.PASS, info_log=sv.FAIL)
        self.assertEqual(f["stdout_parity"], sv.PASS)
        self.assertEqual(f["info_log_parity"], sv.FAIL)

    def test_a_FAIL_is_written_too_not_only_a_pass(self):
        """A guard that only records greens cannot show a regression."""
        self.assertEqual(sv.scorecard_fields(stdout=sv.FAIL)["stdout_parity"], sv.FAIL)

    def test_NOT_MEASURED_leaves_the_column_BLANK(self):
        """A no-result must not become a string in a verdict column.

        check_cell_comparison.py reads ANY non-blank value as "a comparison was
        performed" and demands a reference beside it, so writing `not-measured`
        would manufacture an unreferenced verdict out of an absence.
        """
        for absent in (sv.NOT_MEASURED, ""):
            f = sv.scorecard_fields(stdout=absent, info_log=absent,
                                    exit_code=absent, oracle=absent)
            for col in ("stdout_parity", "info_log_parity",
                        "exit_code_parity", "oracle_verdict"):
                self.assertNotIn(col, f, f"{col} written for {absent!r}")

    def test_the_three_new_columns_are_expressible_but_default_absent(self):
        """Capacity exists; nothing is invented when no producer supplies it."""
        self.assertNotIn("exit_code_parity", sv.scorecard_fields(stdout=sv.PASS))
        self.assertEqual(
            sv.scorecard_fields(exit_code=sv.PASS)["exit_code_parity"], sv.PASS)
        self.assertEqual(
            sv.scorecard_fields(oracle=sv.FAIL)["oracle_verdict"], sv.FAIL)

    def test_detlog_parity_now_has_a_column_to_land_in(self):
        """The producer already emitted this key; the schema had nowhere to put it."""
        core = json.loads(
            (Path(__file__).resolve().parent / "scorecard-schema.json").read_text()
        )["core"]
        f = sv.scorecard_fields(detlog=sv.detlog_verdict(raw(DETLOG), raw(DETLOG)))
        self.assertIn("detlog_parity", f)
        for col in ("stdout_parity", "stack_parity", "heap_parity",
                    "detlog_parity", "exit_code_parity", "oracle_verdict"):
            self.assertIn(col, core, f"{col} emitted/declared but absent from core")


if __name__ == "__main__":
    unittest.main(verbosity=2)
