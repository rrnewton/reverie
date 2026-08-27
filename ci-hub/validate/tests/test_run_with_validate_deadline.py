from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import run_with_validate_deadline  # noqa: E402


class RunWithValidateDeadlineTest(unittest.TestCase):
    def test_missing_child_is_refused(self) -> None:
        self.assertEqual(2, run_with_validate_deadline.main([]))

    def test_child_receives_fresh_monotonic_epoch(self) -> None:
        with mock.patch.object(
            run_with_validate_deadline.time, "monotonic_ns", return_value=123456789
        ), mock.patch.object(
            run_with_validate_deadline.os, "execvpe", side_effect=RuntimeError("observed")
        ) as execute:
            with self.assertRaisesRegex(RuntimeError, "observed"):
                run_with_validate_deadline.main(["program", "arg"])

        command, argv, environment = execute.call_args.args
        self.assertEqual("program", command)
        self.assertEqual(["program", "arg"], argv)
        self.assertEqual(
            "123456789",
            environment[
                run_with_validate_deadline.STEP_STARTED_MONOTONIC_NS_ENV
            ],
        )
        self.assertNotEqual(
            os.environ.get(run_with_validate_deadline.STEP_STARTED_MONOTONIC_NS_ENV),
            "123456789",
        )


if __name__ == "__main__":
    unittest.main()
