#!/usr/bin/env python3
"""Brackets for the wrkslots-doctor tick gate.

The load-bearing requirement is not that it can be LOUD -- almost any gate can.
It is that it can be QUIET, on both a clean population and an unchanged dirty
one. A tick line that always carries a slot warning is filtered out within a
week, and after that the signal is worse than absent.
"""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

HEALTH = Path(__file__).resolve().parents[1]
GATE = HEALTH / "wrkslots_doctor_findings.py"


def write_stub(path: Path, *, findings: list[dict], returncode: int = 0, body: str | None = None) -> None:
    """A stub standing in for `wrkslots doctor --format json`.

    The gate runs it as a real subprocess and parses its real output, so these
    brackets exercise the actual invoke-and-parse path rather than a seam that
    bypasses it.
    """
    if body is None:
        document = json.dumps({"schema": 2, "findings": findings})
        body = f"cat <<'JSON'\n{document}\nJSON\nexit {returncode}\n"
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


def run_gate(tmp: Path, stub: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(GATE),
            "--root",
            str(tmp),
            "--wrkslots",
            str(stub),
            "--state",
            str(tmp / "state.json"),
            "--gate",
            *extra,
        ],
        capture_output=True,
        text=True,
        check=False,
    )


ROW = {"kind": "row-without-directory", "slot": "slot-a", "detail": "no directory"}
DEAD = {"kind": "dead-owner", "slot": "slot-b", "detail": "owner is dead"}


class DoctorGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(__import__("tempfile").mkdtemp())
        self.stub = self.tmp / "wrkslots-stub"

    def tearDown(self) -> None:
        __import__("shutil").rmtree(self.tmp, ignore_errors=True)

    def test_clean_population_is_silent(self) -> None:
        """THE DIRECTION THAT MATTERS MOST. No findings must mean no page."""
        write_stub(self.stub, findings=[])
        first = run_gate(self.tmp, self.stub)
        self.assertEqual(0, first.returncode, first.stderr)
        second = run_gate(self.tmp, self.stub)
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertIn("state=ok", second.stdout)
        self.assertIn("no registry/storage disagreement", second.stdout)
        self.assertIn("new_findings=0", second.stdout)

    def test_first_run_adopts_a_standing_backlog_without_paging(self) -> None:
        """122 findings already present must not make the first tick the loudest."""
        write_stub(self.stub, findings=[ROW, DEAD])
        result = run_gate(self.tmp, self.stub)
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("state=unknown", result.stdout)
        self.assertIn("recorded a baseline of 2 standing finding(s)", result.stdout)

    def test_unchanged_standing_backlog_stays_quiet_but_stays_counted(self) -> None:
        """Quiet on an unchanged dirty population, with the backlog still visible.

        This is what stops the gate being muted: the count is carried in the
        fields every tick even though nothing pages.
        """
        write_stub(self.stub, findings=[ROW, DEAD])
        run_gate(self.tmp, self.stub)
        again = run_gate(self.tmp, self.stub)
        self.assertEqual(0, again.returncode, again.stderr)
        self.assertIn("state=ok", again.stdout)
        self.assertIn("findings_total=2", again.stdout)
        self.assertIn("standing_row_without_directory=1", again.stdout)
        self.assertIn("standing_dead_owner=1", again.stdout)

    def test_a_new_finding_pages_and_is_named(self) -> None:
        """The other direction: something genuinely new must page."""
        write_stub(self.stub, findings=[ROW])
        run_gate(self.tmp, self.stub)
        write_stub(self.stub, findings=[ROW, DEAD])
        result = run_gate(self.tmp, self.stub)
        self.assertEqual(1, result.returncode, result.stderr)
        self.assertIn("state=actionable", result.stdout)
        self.assertIn("1 new registry/storage disagreement(s); 2 standing", result.stdout)
        self.assertIn("new=dead-owner slot-b", result.stdout)
        self.assertNotIn("new=row-without-directory", result.stdout)

    def test_a_repaired_finding_that_returns_pages_again(self) -> None:
        """The baseline is the current set, never a cumulative union.

        A disagreement that is repaired and later recurs is a new event and must
        page again, or the gate goes permanently blind to a recurring slot.
        """
        write_stub(self.stub, findings=[ROW])
        run_gate(self.tmp, self.stub)
        write_stub(self.stub, findings=[])
        cleared = run_gate(self.tmp, self.stub)
        self.assertEqual(0, cleared.returncode, cleared.stderr)
        write_stub(self.stub, findings=[ROW])
        returned = run_gate(self.tmp, self.stub)
        self.assertEqual(1, returned.returncode, returned.stderr)
        self.assertIn("new=row-without-directory slot-a", returned.stdout)

    def test_an_unavailable_answer_pages_rather_than_reading_as_clean(self) -> None:
        """A gate that cannot measure must not be silent.

        Exiting 0 on a broken doctor is indistinguishable from a clean
        population, which is the confusion this gate exists to remove. 2 matches
        `wrkslots audit --gate`'s own unavailable status.
        """
        write_stub(self.stub, findings=[], returncode=3, body="echo boom >&2\nexit 3\n")
        result = run_gate(self.tmp, self.stub)
        self.assertEqual(2, result.returncode, result.stdout)
        self.assertIn("state=unknown", result.stdout)
        self.assertIn("wrkslots doctor failed", result.stdout)

    def test_unparseable_output_is_unavailable_not_clean(self) -> None:
        write_stub(self.stub, findings=[], body="echo 'not json'\n")
        result = run_gate(self.tmp, self.stub)
        self.assertEqual(2, result.returncode, result.stdout)
        self.assertIn("unparseable JSON", result.stdout)

    def test_an_unavailable_tick_does_not_consume_the_baseline(self) -> None:
        """A failed tick must not swallow findings a later working tick would page.

        If the unavailable path wrote a baseline, the next healthy tick would see
        those findings as already-reported and never mention them.
        """
        write_stub(self.stub, findings=[ROW])
        run_gate(self.tmp, self.stub)
        write_stub(self.stub, findings=[], body="exit 3\n")
        run_gate(self.tmp, self.stub)
        write_stub(self.stub, findings=[ROW, DEAD])
        result = run_gate(self.tmp, self.stub)
        self.assertEqual(1, result.returncode, result.stderr)
        self.assertIn("new=dead-owner slot-b", result.stdout)

    def test_it_authorizes_and_changes_nothing(self) -> None:
        """DETECT ONLY. The only file it may write is its own baseline."""
        write_stub(self.stub, findings=[ROW])
        before = {p.name for p in self.tmp.iterdir()}
        run_gate(self.tmp, self.stub)
        after = {p.name for p in self.tmp.iterdir()}
        self.assertEqual({"state.json"}, after - before)

    def test_dry_run_leaves_the_baseline_untouched(self) -> None:
        write_stub(self.stub, findings=[ROW])
        run_gate(self.tmp, self.stub)
        write_stub(self.stub, findings=[ROW, DEAD])
        first = run_gate(self.tmp, self.stub, "--dry-run")
        self.assertEqual(1, first.returncode)
        second = run_gate(self.tmp, self.stub, "--dry-run")
        self.assertEqual(1, second.returncode, "a dry run must not record the new finding")


if __name__ == "__main__":
    unittest.main()
