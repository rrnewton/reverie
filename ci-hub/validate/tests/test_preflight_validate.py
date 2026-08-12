#!/usr/bin/env python3
"""Network-free tests for the composite validation admission authority."""

from __future__ import annotations

import contextlib
import io
import sys
import unittest
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import preflight_validate as pv  # noqa: E402


HEAD = "a" * 40
def floors(ok: bool = True) -> dict:
    return {
        "head": HEAD,
        "repo": "rrnewton/hermit",
        "n_anchors": 2,
        "anchors": [],
        "missing": [] if ok else [{"sha": "c" * 40}],
        "ok": ok,
    }


class AdmissionVerdictTest(unittest.TestCase):
    def evaluate(self, contains: bool, *, fixed_ok: bool = True) -> dict:
        with mock.patch.object(pv.preflight_anchor, "preflight", return_value=floors(fixed_ok)):
            return pv.admission(
                HEAD,
                checkout="/checkout",
                repo="rrnewton/hermit",
                anchors_path="/anchors.json",
                base_branch="main",
            )

    def test_mutable_tip_is_owned_by_validate_lock(self) -> None:
        result = self.evaluate(False)

        self.assertTrue(result["ok"])
        self.assertEqual("checked-by-validate-lock", result["moving_base"]["status"])
        message = pv.render(result)
        self.assertIn("OK:", message)
        self.assertIn("checked by validate-lock", message)

        with mock.patch.object(pv, "admission", return_value=result), \
             contextlib.redirect_stdout(io.StringIO()):
            rc = pv.main(["--head", HEAD])
        self.assertEqual(pv.EXIT_OK, rc)

    def test_head_containing_current_base_is_admitted(self) -> None:
        result = self.evaluate(True)

        self.assertTrue(result["ok"])
        self.assertIn("OK:", pv.render(result))
        self.assertIn("validate-lock", pv.render(result))

    def test_fixed_floor_still_refuses_even_when_moving_base_passes(self) -> None:
        result = self.evaluate(True, fixed_ok=False)

        self.assertFalse(result["ok"])

    def test_non_exact_head_is_an_error(self) -> None:
        with self.assertRaises(pv.AdmissionError):
            pv.admission(
                "origin/feature",
                checkout="/checkout",
                repo="rrnewton/hermit",
                anchors_path="/anchors.json",
                base_branch="main",
            )


if __name__ == "__main__":
    unittest.main()
