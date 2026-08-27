#!/usr/bin/env python3
"""Both-way verification for the RESULT-LEVEL zero-test-green detector.

A detector that rejects everything passes the negative test alone, so every
downgrade case is paired with a genuine-pass case that MUST still be accepted:
- NEGATIVE: plant a zero-test green -> it is REFUSED (no_result).
- POSITIVE: plant a genuine passing run -> it is ACCEPTED (green).
"""

from __future__ import annotations

import io
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

REMEDIATION = Path(__file__).resolve().parents[1]
HISTORY = REMEDIATION.parents[0] / "history"
sys.path.insert(0, str(REMEDIATION))
sys.path.insert(0, str(HISTORY))

import nonzero_result
import protocol

# A real cargo test-target that COMPILED and ran but whose tests were all gated
# out (`--features notifier`): the binary runs, executes zero tests, exits 0.
GATED_ZERO_TEST = (
    "   Compiling reverie-notifier v0.1.0\n"
    "    Finished test [unoptimized + debuginfo] target(s)\n"
    "     Running unittests src/lib.rs (target/debug/deps/reverie_notifier-abc)\n"
    "\n"
    "running 0 tests\n"
    "\n"
    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
)

# The EXACT captured shape from reverie#350's validate log (lines 474/476): a
# success line that verified NOTHING, an EMPTY TARGET — zero executed AND zero
# filtered. The `test result: ok` line alone cannot tell this from FULLY_FILTERED
# below; only the filtered count can.
REVERIE_350_EMPTY_TARGET = (
    "running 0 tests\n"
    "\n"
    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
)

# The SIBLING shape: tests EXIST but the selection excluded every one. Same `ok. 0
# passed`, but filtered > 0 — so the run is FILTERED TO EMPTY, not an empty
# target. Distinguishing the two is the whole point of carrying filtered_tests.
FULLY_FILTERED = (
    "running 0 tests\n"
    "\n"
    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 154 filtered out; finished in 0.00s\n"
)

# The `1 passed; 154 filtered out` narrowed-scope trap: a real pass, but over a
# subset the consumer must be shown.
NARROWED_PASS = (
    "running 1 test\n"
    "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 154 filtered out; finished in 0.01s\n"
)

# A genuine passing validate: many banners, real executed tests.
GENUINE_PASS = (
    "     Running unittests src/lib.rs (target/debug/deps/detcore-111)\n"
    "\n"
    "running 47 tests\n"
    "test tests::a ... ok\n"
    "test result: ok. 47 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
    "     Running unittests src/main.rs (target/debug/deps/hermit-222)\n"
    "running 0 tests\n"
    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
    "✅ Validation summary [full] (12 passed, 0 failed)\n"
)


class ExecutedTestCountTest(unittest.TestCase):
    def test_no_banner_is_unknown_not_zero(self) -> None:
        # UNKNOWN must be distinct from zero: a log with no test-runner banner
        # (empty, or build-only) is None, never 0 — so it is never downgraded.
        self.assertIsNone(nonzero_result.executed_test_count(""))
        self.assertIsNone(
            nonzero_result.executed_test_count("Finished. everything compiled.")
        )

    def test_all_zero_banners_sum_to_zero(self) -> None:
        self.assertEqual(nonzero_result.executed_test_count(GATED_ZERO_TEST), 0)

    def test_real_banners_sum_executed(self) -> None:
        # 47 + 0 across two binaries; the zero-test binary does not mask the real
        # one, so the run as a whole executed tests.
        self.assertEqual(nonzero_result.executed_test_count(GENUINE_PASS), 47)

    def test_singular_running_one_test(self) -> None:
        self.assertEqual(
            nonzero_result.executed_test_count("running 1 test\n"), 1
        )

    def test_passed_count_fallback_when_running_banner_absent(self) -> None:
        self.assertEqual(
            nonzero_result.executed_test_count("test result: ok. 5 passed; 0 failed"),
            5,
        )


class FilteredTestCountTest(unittest.TestCase):
    """The filtered count is what tells two zero-executed runs apart — the exact
    ambiguity the reverie#350 fixture exposes."""

    def test_no_result_line_is_unknown(self) -> None:
        self.assertIsNone(nonzero_result.filtered_test_count(""))
        self.assertIsNone(nonzero_result.filtered_test_count("running 0 tests\n"))

    def test_empty_target_has_zero_filtered(self) -> None:
        # reverie#350: executed 0 AND filtered 0 -> nothing exists to run.
        self.assertEqual(
            nonzero_result.executed_test_count(REVERIE_350_EMPTY_TARGET), 0
        )
        self.assertEqual(
            nonzero_result.filtered_test_count(REVERIE_350_EMPTY_TARGET), 0
        )

    def test_fully_filtered_has_nonzero_filtered(self) -> None:
        # Same `ok. 0 passed`, but filtered 154 -> tests exist, all excluded.
        self.assertEqual(nonzero_result.executed_test_count(FULLY_FILTERED), 0)
        self.assertEqual(nonzero_result.filtered_test_count(FULLY_FILTERED), 154)

    def test_two_zero_executed_shapes_are_distinguishable(self) -> None:
        # EXTRACTED, not phrase-matched: both print `test result: ok. 0 passed`,
        # so a substring match cannot separate them; the filtered COUNT does.
        self.assertEqual(
            nonzero_result.executed_test_count(REVERIE_350_EMPTY_TARGET),
            nonzero_result.executed_test_count(FULLY_FILTERED),
        )
        self.assertNotEqual(
            nonzero_result.filtered_test_count(REVERIE_350_EMPTY_TARGET),
            nonzero_result.filtered_test_count(FULLY_FILTERED),
        )

    def test_narrowed_pass_carries_its_filtered_scope(self) -> None:
        self.assertEqual(nonzero_result.executed_test_count(NARROWED_PASS), 1)
        self.assertEqual(nonzero_result.filtered_test_count(NARROWED_PASS), 154)


class LedgerFieldsCliTest(unittest.TestCase):
    """The single-source hook `validate.sh` calls instead of a bash regex copy.

    Output is two whitespace-separated JSON literals: an integer or `null`, so a
    shell writer splices them straight into a JSONL record. Values are EXTRACTED
    from the log's own numbers, never phrase-matched.
    """

    def test_empty_target_fields(self) -> None:
        self.assertEqual(
            nonzero_result._ledger_fields(REVERIE_350_EMPTY_TARGET), "0 0"
        )

    def test_fully_filtered_fields(self) -> None:
        self.assertEqual(nonzero_result._ledger_fields(FULLY_FILTERED), "0 154")

    def test_genuine_pass_fields(self) -> None:
        self.assertEqual(nonzero_result._ledger_fields(GENUINE_PASS), "47 0")

    def test_no_banner_is_null_null(self) -> None:
        # UNKNOWN -> null null, so the reader leaves the row fail-safe alone.
        self.assertEqual(nonzero_result._ledger_fields("built ok\n"), "null null")

    def test_json_literals_are_int_or_null(self) -> None:
        # Every token must be a valid standalone JSON value for shell splicing.
        for fixture in (REVERIE_350_EMPTY_TARGET, FULLY_FILTERED, GENUINE_PASS, ""):
            for tok in nonzero_result._ledger_fields(fixture).split():
                self.assertRegex(tok, r"^(null|\d+)$")

    def test_main_reads_file(self) -> None:
        with tempfile.NamedTemporaryFile("w", suffix=".log", delete=False) as fh:
            fh.write(FULLY_FILTERED)
            path = fh.name
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = nonzero_result.main(["--ledger-fields", path])
        self.assertEqual(rc, 0)
        self.assertEqual(buf.getvalue().strip(), "0 154")

    def test_main_missing_file_is_null_null_exit_0(self) -> None:
        # A read hiccup must never fail the run or fabricate a zero.
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = nonzero_result.main(["--ledger-fields", "/no/such/log/file.xyz"])
        self.assertEqual(rc, 0)
        self.assertEqual(buf.getvalue().strip(), "null null")

    def test_main_stdin(self) -> None:
        # `-` reads stdin: the invocation shape validate.sh may pipe the log.
        proc = subprocess.run(
            [sys.executable, str(REMEDIATION / "nonzero_result.py"),
             "--ledger-fields", "-"],
            input=REVERIE_350_EMPTY_TARGET, capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout.strip(), "0 0")


class IsZeroTestGreenTest(unittest.TestCase):
    def test_negative_zero_test_green_is_refused(self) -> None:
        self.assertTrue(nonzero_result.is_zero_test_green(GATED_ZERO_TEST))

    def test_positive_genuine_pass_is_accepted(self) -> None:
        self.assertFalse(nonzero_result.is_zero_test_green(GENUINE_PASS))

    def test_unknown_log_is_not_flagged(self) -> None:
        # Fail-safe: a green whose log carries no banner is NOT flagged.
        self.assertFalse(nonzero_result.is_zero_test_green(""))
        self.assertFalse(nonzero_result.is_zero_test_green("built ok, no tests logged"))


class ClassifyLocalFinalStatusTest(unittest.TestCase):
    """The speculative-land classifier consumes validate's fixed status."""

    def test_status_passes_even_when_old_prose_would_have_said_zero_test(self) -> None:
        state, reason = protocol._classify_local(
            0,
            GATED_ZERO_TEST
            + "FINAL_VALIDATE_STATUS: PASSED\n"
            + "# ci-hub/validate-lock tool COST ACTUAL exit=0\n",
        )
        self.assertEqual(state, "green")
        self.assertEqual(reason, "final-status:PASSED")

    def test_positive_genuine_pass_stays_green(self) -> None:
        state, reason = protocol._classify_local(
            0,
            GENUINE_PASS
            + "FINAL_VALIDATE_STATUS: PASSED\n"
            + "# ci-hub/validate-lock tool COST ACTUAL exit=0\n",
        )
        self.assertEqual(state, "green")
        self.assertEqual(reason, "final-status:PASSED")

    def test_clean_exit_without_captured_output_is_no_result(self) -> None:
        self.assertEqual(protocol._local_state(0), "no_result")
        self.assertEqual(protocol._local_state(0, ""), "no_result")

    def test_absence_is_no_result_never_red(self) -> None:
        state, _ = protocol._classify_local(0, GATED_ZERO_TEST)
        self.assertNotEqual(state, "red")


if __name__ == "__main__":
    unittest.main()
