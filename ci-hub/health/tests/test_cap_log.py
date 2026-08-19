"""Bracket cap-log from both sides: the cap must hold, and truncation must show.

The behaviour that matters most was verified against a real hung hermit run
rather than a synthetic producer -- a `--log=info` run whose guest never exits,
whose stderr froze at the cap while the run kept going. See the task notes on
bound-hermit-stderr. These tests bracket the logic around that.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

CAP_LOG = Path(__file__).resolve().parent.parent.parent / "bin" / "cap-log"


def run(args: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(CAP_LOG), *args], capture_output=True, text=True, timeout=120
    )


class CapHoldsTest(unittest.TestCase):
    def test_file_never_exceeds_the_cap(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            cap = 64 * 1024
            # Producer writes ~4 MiB, 64x the cap.
            proc = run([
                "--max-bytes", str(cap), "--stderr", str(out), "--",
                sys.executable, "-c",
                "import sys\nsys.stderr.write('x'*4096*1024)\n",
            ])
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertLessEqual(out.stat().st_size, cap)

    def test_producer_is_not_blocked_by_the_cap(self) -> None:
        """The child must RUN TO COMPLETION past the cap.

        The naive implementation stops reading, which blocks the writer forever:
        measured on hermit, a run that finished in 27s unbounded was still alive
        at 300s when its log sink stopped accepting bytes. The child's own exit
        code reaching us is the proof it was never wedged.
        """
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            proc = run([
                "--max-bytes", "8192", "--stderr", str(out), "--",
                sys.executable, "-c",
                "import sys\nsys.stderr.write('y'*2048*1024)\nsys.exit(42)\n",
            ])
            self.assertEqual(proc.returncode, 42, "child exit code must survive the cap")

    def test_child_exit_code_is_propagated_when_not_truncated(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            proc = run([
                "--max-bytes", "1M", "--stderr", str(out), "--",
                sys.executable, "-c", "import sys; sys.exit(7)",
            ])
            self.assertEqual(proc.returncode, 7)


class TruncationIsVisibleTest(unittest.TestCase):
    def test_truncated_file_says_so_in_its_last_line(self) -> None:
        """A capped log that reads as complete is worse than a missing one."""
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            run([
                "--max-bytes", "8192", "--stderr", str(out), "--",
                sys.executable, "-c", "import sys; sys.stderr.write('z'*1024*1024)",
            ])
            tail = out.read_bytes().decode(errors="replace").splitlines()[-1]
            self.assertIn("*** TRUNCATED ***", tail)
            self.assertIn("DISCARDED", tail)

    def test_sidecar_carries_the_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            run([
                "--max-bytes", "8192", "--stderr", str(out), "--",
                sys.executable, "-c", "import sys; sys.stderr.write('z'*1024*1024)",
            ])
            side = out.with_name(out.name + ".truncated.json")
            self.assertTrue(side.exists(), "a harness must detect truncation without parsing")
            rec = json.loads(side.read_text())
            self.assertTrue(rec["truncated"])
            self.assertGreater(rec["dropped_bytes"], 0)
            self.assertEqual(rec["cap_bytes"], 8192)

    def test_no_false_truncation_marker_when_under_cap(self) -> None:
        """A marker on a complete log would be its own kind of lie."""
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            run([
                "--max-bytes", "1M", "--stderr", str(out), "--",
                sys.executable, "-c", "import sys; sys.stderr.write('short\\n')",
            ])
            self.assertEqual(out.read_text(), "short\n")
            self.assertFalse(out.with_name(out.name + ".truncated.json").exists())

    def test_fail_on_truncate_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "e.log"
            argv = [
                "--max-bytes", "8192", "--stderr", str(out), "--",
                sys.executable, "-c", "import sys; sys.stderr.write('z'*1024*1024)",
            ]
            self.assertEqual(run(argv).returncode, 0, "default: child status wins")
            self.assertEqual(run(["--fail-on-truncate", *argv]).returncode, 79)


class SizeParsingTest(unittest.TestCase):
    def test_units(self) -> None:
        # cap-log has no .py suffix, so spec_from_file_location cannot infer a
        # loader and returns None; name the loader explicitly.
        import importlib.util
        from importlib.machinery import SourceFileLoader

        loader = SourceFileLoader("cap_log", str(CAP_LOG))
        spec = importlib.util.spec_from_loader("cap_log", loader)
        mod = importlib.util.module_from_spec(spec)
        sys.modules["cap_log"] = mod
        loader.exec_module(mod)
        self.assertEqual(mod.parse_size("1024"), 1024)
        self.assertEqual(mod.parse_size("64K"), 65536)
        self.assertEqual(mod.parse_size("512M"), 512 << 20)
        self.assertEqual(mod.parse_size("2G"), 2 << 30)
        self.assertEqual(mod.parse_size("2GB"), 2 << 30)

    def test_cap_below_marker_reserve_is_rejected(self) -> None:
        """A cap too small to hold its own truncation notice cannot be honest."""
        with tempfile.TemporaryDirectory() as tmp:
            proc = run([
                "--max-bytes", "16", "--stderr", str(Path(tmp) / "e"), "--", "/bin/true",
            ])
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("must exceed", proc.stderr)


if __name__ == "__main__":
    unittest.main()
