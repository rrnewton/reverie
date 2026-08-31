"""Tests for lib/bounded_capture.

⚠️ THE CENTRAL TEST HERE MUST FAIL, NOT HANG. The defect under test IS a hang, so
a test that simply calls the bad code and waits would, on regression, block the
whole suite instead of reporting. Every test that could hang runs the call on a
worker thread and asserts it FINISHED within a generous outer bound. The shared
helper's optional timeout bounds only a still-running direct child; the regular
files remain what lets an already-exited child return while a grandchild keeps
its inherited descriptors open.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import threading
import time
import unittest
from contextlib import contextmanager
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import bounded_capture  # noqa: E402


# A child that exits IMMEDIATELY but leaves one background grandchild holding
# the inherited stdout/stderr. This is the owner's hung box in miniature: the
# child is gone and its exit status is knowable, but the pipe never sees EOF.
GRANDCHILD_HOLDER = (
    "import pathlib,sys,time;"
    "ready=pathlib.Path(sys.argv[1]);"
    "sentinel=pathlib.Path(sys.argv[2]);"
    "released=pathlib.Path(sys.argv[3]);"
    "deadline=time.monotonic()+30;ready.touch();"
    "\nwhile not sentinel.exists() and time.monotonic()<deadline: time.sleep(0.01)"
    "\nreleased.touch()"
)
ORPHAN_HOLDER = (
    "import subprocess,sys;"
    "sys.stdout.write('done\\n');sys.stdout.flush();"
    # The grandchild inherits this process's stdout/stderr and waits until the
    # test releases it. It is NOT waited on -- that is the point.
    "subprocess.Popen([sys.executable,'-c',sys.argv[4],sys.argv[1],sys.argv[2],sys.argv[3]]);"
    "sys.exit(7)"
)


def _orphan_command(ready: Path, sentinel: Path, released: Path) -> list[str]:
    return [
        sys.executable,
        "-c",
        ORPHAN_HOLDER,
        str(ready),
        str(sentinel),
        str(released),
        GRANDCHILD_HOLDER,
    ]


def _wait_for(path: Path, *, label: str) -> None:
    deadline = time.monotonic() + 10
    while not path.exists():
        if time.monotonic() >= deadline:
            raise AssertionError(f"{label} was not observed")
        time.sleep(0.01)


def _release_grandchild(sentinel: Path, released: Path) -> None:
    sentinel.touch()
    _wait_for(released, label="orphan-holder grandchild exit")


def _cleanup_worker(
    thread: threading.Thread, sentinel: Path, released: Path
) -> BaseException | None:
    cleanup_error: BaseException | None = None
    try:
        _release_grandchild(sentinel, released)
    except BaseException as error:  # noqa: BLE001 - returned to preserve first failure
        cleanup_error = error
    finally:
        thread.join(10)
        if thread.is_alive():
            liveness_error = AssertionError("fixture worker did not clean up")
            if cleanup_error is None:
                cleanup_error = liveness_error
            else:
                cleanup_error.add_note(str(liveness_error))
    return cleanup_error


@contextmanager
def _cleaned_orphan_worker(
    thread: threading.Thread, sentinel: Path, released: Path
):
    try:
        yield
    except BaseException as primary:  # noqa: BLE001 - preserve after cleanup
        cleanup_error = _cleanup_worker(thread, sentinel, released)
        if cleanup_error is not None:
            primary.add_note(f"fixture cleanup also failed: {cleanup_error}")
        raise
    else:
        cleanup_error = _cleanup_worker(thread, sentinel, released)
        if cleanup_error is not None:
            raise cleanup_error


def _start(fn):
    box = {}

    def target():
        try:
            box["value"] = fn()
        except BaseException as exc:  # noqa: BLE001 - reported, not swallowed
            box["error"] = exc

    thread = threading.Thread(target=target, daemon=True)
    thread.start()
    return thread, box


def _finishes(fn, seconds=25):
    """Run fn on a worker; return (finished, result_or_exception).

    Never joins forever, so a regression is reported as a FAILURE with a name
    rather than a suite that never returns.
    """
    thread, box = _start(fn)
    thread.join(seconds)
    if thread.is_alive():
        return False, None
    if "error" in box:
        raise box["error"]
    return True, box.get("value")


class OrphanGrandchildDoesNotBlockCapture(unittest.TestCase):
    """The regression test for the class. This is the owner's hang, reduced."""

    def _assert_capture_returns(self, *, timeout=None):
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / "ready"
            sentinel = Path(directory) / "release"
            released = Path(directory) / "released"
            thread, box = _start(
                lambda: bounded_capture.capture(
                    _orphan_command(ready, sentinel, released), timeout=timeout
                )
            )
            with _cleaned_orphan_worker(thread, sentinel, released):
                _wait_for(ready, label="orphan-holder readiness")
                thread.join(8)
                finished = not thread.is_alive()
        if "error" in box:
            raise box["error"]
        result = box.get("value")
        self.assertTrue(
            finished,
            "capture() did not return while an orphan grandchild held the "
            "inherited descriptors -- this is exactly the hang the helper exists "
            "to remove, so the helper has regressed to pipe semantics",
        )
        self.assertEqual(result.returncode, 7, "the child's real exit status is reported")
        self.assertEqual(result.stdout.strip(), "done")

    def test_capture_without_timeout_returns_while_a_grandchild_holds_descriptors(self):
        self._assert_capture_returns()

    def test_capture_with_timeout_returns_while_a_grandchild_holds_descriptors(self):
        self._assert_capture_returns(timeout=5)

    def test_the_control_confirms_the_scenario_actually_reproduces_the_hang(self):
        """⚠️ THE MUST-FAIL CONTROL.

        If `capture_output=True` on this same command DOESN'T hang, then the
        scenario above is not reproducing the defect and the passing test above
        proves nothing. This asserts the bad shape really is bad, so the good
        shape passing is evidence rather than a configuration.
        """
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / "ready"
            sentinel = Path(directory) / "release"
            released = Path(directory) / "released"
            thread, box = _start(
                lambda: subprocess.run(
                    _orphan_command(ready, sentinel, released),
                    capture_output=True,
                    text=True,
                )
            )
            with _cleaned_orphan_worker(thread, sentinel, released):
                _wait_for(ready, label="orphan-holder readiness")
                thread.join(8)
                stayed_blocked = thread.is_alive()
        self.assertTrue(
            stayed_blocked,
            "capture_output=True returned while a ready grandchild held the "
            "pipe; the control no longer reproduces the defect",
        )
        if "error" in box:
            raise box["error"]


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

    def test_default_timeout_none_preserves_early_exit_broken_pipe(self):
        with self.assertRaises(BrokenPipeError):
            bounded_capture.capture(
                [
                    sys.executable,
                    "-c",
                    "import os,time;os.close(0);time.sleep(0.1)",
                ],
                input="x" * (1024 * 1024),
            )

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

    def test_timeout_kills_and_reaps_a_still_running_direct_child(self):
        started = time.monotonic()

        def invoke():
            try:
                bounded_capture.capture(
                    [sys.executable, "-c", "import time;time.sleep(10)"],
                    timeout=0.1,
                )
            except subprocess.TimeoutExpired as error:
                return error
            raise AssertionError("a still-running direct child did not time out")

        finished, error = _finishes(invoke, seconds=5)
        self.assertTrue(finished, "the direct-child timeout did not finish")
        self.assertLess(time.monotonic() - started, 5)
        self.assertIsInstance(error, subprocess.TimeoutExpired)
        self.assertGreater(error.timeout, 0)
        self.assertLessEqual(error.timeout, 0.1)
        self.assertIsNone(error.output)

    def test_timeout_also_bounds_input_delivery(self):
        def invoke():
            try:
                bounded_capture.capture(
                    [sys.executable, "-c", "import time;time.sleep(10)"],
                    input="x" * (1024 * 1024),
                    timeout=0.1,
                )
            except subprocess.TimeoutExpired as error:
                return error
            raise AssertionError("blocked input delivery did not time out")

        finished, error = _finishes(invoke, seconds=5)
        self.assertTrue(finished, "the input-delivery timeout did not finish")
        self.assertIsInstance(error, subprocess.TimeoutExpired)


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
