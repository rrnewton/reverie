"""Tests for lib/bounded_capture.

⚠️ THE CENTRAL TEST HERE MUST FAIL, NOT HANG. The defect under test IS a hang, so
a test that simply calls the bad code and waits would, on regression, block the
whole suite instead of reporting. Every test that could hang runs the call on a
worker thread and asserts it FINISHED within a generous bound. The bound is on
the TEST, not on the product: `capture()` itself takes no timeout, and adding
one would be the fix this change explicitly rejects.
"""

from __future__ import annotations

import subprocess
import sys
import threading
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import bounded_capture  # noqa: E402


# A child that exits IMMEDIATELY but leaves one background grandchild holding
# the inherited stdout/stderr. This is the owner's hung box in miniature: the
# child is gone and its exit status is knowable, but the pipe never sees EOF.
ORPHAN_HOLDER = (
    "import subprocess,sys;"
    "sys.stdout.write('done\\n');sys.stdout.flush();"
    # The grandchild inherits this process's stdout/stderr and sleeps well past
    # any patience the test has. It is NOT waited on -- that is the point.
    "subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)']);"
    "sys.exit(7)"
)


def _finishes(fn, seconds=25):
    """Run fn on a worker; return (finished, result_or_exception).

    Never joins forever, so a regression is reported as a FAILURE with a name
    rather than a suite that never returns.
    """
    box = {}

    def target():
        try:
            box["value"] = fn()
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            box["error"] = exc

    thread = threading.Thread(target=target, daemon=True)
    thread.start()
    thread.join(seconds)
    if thread.is_alive():
        return False, None
    if "error" in box:
        raise box["error"]
    return True, box.get("value")


class OrphanGrandchildDoesNotBlockCapture(unittest.TestCase):
    """The regression test for the class. This is the owner's hang, reduced."""

    def test_capture_returns_while_a_grandchild_still_holds_the_descriptors(self):
        finished, result = _finishes(
            lambda: bounded_capture.capture([sys.executable, "-c", ORPHAN_HOLDER])
        )
        self.assertTrue(
            finished,
            "capture() did not return while an orphan grandchild held the "
            "inherited descriptors -- this is exactly the hang the helper exists "
            "to remove, so the helper has regressed to pipe semantics",
        )
        self.assertEqual(result.returncode, 7, "the child's real exit status is reported")
        self.assertEqual(result.stdout.strip(), "done")

    def test_the_control_confirms_the_scenario_actually_reproduces_the_hang(self):
        """⚠️ THE MUST-FAIL CONTROL.

        If `capture_output=True` on this same command DOESN'T hang, then the
        scenario above is not reproducing the defect and the passing test above
        proves nothing. This asserts the bad shape really is bad, so the good
        shape passing is evidence rather than a configuration.
        """
        finished, _ = _finishes(
            lambda: subprocess.run(
                [sys.executable, "-c", ORPHAN_HOLDER],
                capture_output=True,
                text=True,
            ),
            seconds=8,
        )
        self.assertFalse(
            finished,
            "capture_output=True returned promptly on a command that leaves an "
            "orphan holding the pipe. The scenario no longer reproduces the "
            "defect, so the sibling test above is now vacuous -- fix this "
            "fixture before trusting it.",
        )


class CaptureMatchesSubprocessRunWhereItShould(unittest.TestCase):
    def test_stdout_stderr_and_returncode(self):
        result = bounded_capture.capture(
            [sys.executable, "-c", "import sys;print('out');print('err',file=sys.stderr);sys.exit(3)"]
        )
        self.assertEqual(result.stdout.strip(), "out")
        self.assertEqual(result.stderr.strip(), "err")
        self.assertEqual(result.returncode, 3)

    def test_binary_mode(self):
        result = bounded_capture.capture(
            [sys.executable, "-c", "import sys;sys.stdout.buffer.write(b'\\x00\\xff')"],
            text=False,
        )
        self.assertEqual(result.stdout, b"\x00\xff")

    def test_check_raises_the_same_error_subprocess_would(self):
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            bounded_capture.capture([sys.executable, "-c", "import sys;sys.exit(4)"], check=True)
        self.assertEqual(caught.exception.returncode, 4)

    def test_input_is_delivered(self):
        result = bounded_capture.capture(
            [sys.executable, "-c", "import sys;sys.stdout.write(sys.stdin.read().upper())"],
            input="hello",
        )
        self.assertEqual(result.stdout, "HELLO")

    def test_stdin_defaults_to_devnull_so_a_prompting_child_cannot_block(self):
        finished, result = _finishes(
            lambda: bounded_capture.capture(
                [sys.executable, "-c", "import sys;print(repr(sys.stdin.read()))"]
            )
        )
        self.assertTrue(finished, "a child reading stdin blocked; stdin is not DEVNULL")
        self.assertEqual(result.stdout.strip(), "''")

    def test_owning_stdout_is_enforced_rather_than_silently_ignored(self):
        with self.assertRaises(TypeError):
            bounded_capture.capture(["true"], stdout=subprocess.PIPE)


class GitHelper(unittest.TestCase):
    def test_git_builds_a_dash_c_invocation(self):
        result = bounded_capture.git(["rev-parse", "--show-toplevel"], cwd=Path(__file__).parent)
        self.assertEqual(result.returncode, 0)
        self.assertTrue(result.stdout.strip())

    def test_git_reports_a_failure_rather_than_raising(self):
        result = bounded_capture.git(["cat-file", "-e", "0" * 40], cwd=Path(__file__).parent)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
