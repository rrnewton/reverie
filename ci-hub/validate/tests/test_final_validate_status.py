from __future__ import annotations

import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import final_validate_status as status  # noqa: E402


class FinalValidateStatusTest(unittest.TestCase):
    def test_fixed_values_are_coupled_to_distinct_exits(self) -> None:
        for word, exit_code in (("PASSED", 0), ("FAILED", 1), ("COULD_NOT_RUN", 75)):
            with self.subTest(word=word):
                output = f"FINAL_VALIDATE_STATUS: {word}\n"
                self.assertEqual(
                    word,
                    status.read_coupled_final_validate_status(output, exit_code),
                )

    def test_last_occurrence_wins(self) -> None:
        output = (
            "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            "foreign text\n"
            "FINAL_VALIDATE_STATUS: PASSED\n"
        )
        self.assertEqual("PASSED", status.read_coupled_final_validate_status(output, 0))

    def test_indented_documentation_is_not_a_status_line(self) -> None:
        self.assertIsNone(
            status.read_coupled_final_validate_status(
                "  FINAL_VALIDATE_STATUS: PASSED\n", 0
            )
        )

    def test_absence_remains_absence(self) -> None:
        self.assertIsNone(status.read_coupled_final_validate_status("ordinary output\n", 1))

    def test_unknown_value_and_exit_disagreement_are_rejected(self) -> None:
        with self.assertRaises(status.FinalValidateStatusError):
            status.read_coupled_final_validate_status(
                "FINAL_VALIDATE_STATUS: MAYBE\n", 1
            )
        with self.assertRaises(status.FinalValidateStatusError):
            status.read_coupled_final_validate_status(
                "FINAL_VALIDATE_STATUS: PASSED\n", 1
            )


if __name__ == "__main__":
    unittest.main()
