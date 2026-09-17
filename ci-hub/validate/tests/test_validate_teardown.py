#!/usr/bin/env python3
"""A validate unit must retire its own checkout when it stops.

Teardown used to belong to whichever agent was waiting on the unit, and that
agent routinely dies: measured 2026-09-17, twenty-nine of two hundred and five
completed validate runs carry no writeback attempt at all. Those checkouts can
then never be removed, because removal requires a completed writeback. These
tests pin the parts of the replacement that fail SILENTLY if they regress -- a
systemd Exec string that renders to nothing, a tool path that cannot be executed,
and a working directory inside the tree being deleted.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import start_unit as su  # noqa: E402


TARGET = "7" * 40
REPOSITORY_ROOT = Path(__file__).resolve().parents[3]


def properties(command: list[str]) -> dict[str, str]:
    """Extract the KEY=VALUE properties systemd-run is told to set."""

    found: dict[str, str] = {}
    for flag, value in zip(command, command[1:]):
        if flag == "--property" and "=" in value:
            key, _, rest = value.partition("=")
            found[key] = rest
    return found


def build(root: Path, **overrides: object) -> list[str]:
    arguments: dict[str, object] = {
        "root": root,
        "checkout": root / "worktrees/validate/validate-fresh-abc-1-def",
        "target": TARGET,
        "agent": "test-agent",
        "unit": "validate-test-unit",
        "record": root / "ignored/validate/runs/validate-test-unit.json",
        "log": root / "out.log",
        "pr": None,
        "validate_args": ["full"],
        "wait": 1,
        "hold": 1,
        "child_deadline": 3600,
        "environment": {"HOME": "/home/tester", "PATH": "/usr/bin"},
    }
    arguments.update(overrides)
    return su.build_systemd_command(**arguments)  # type: ignore[arg-type]


class SystemdExecValueTest(unittest.TestCase):
    """The rendering whose failure mode is a shell that runs nothing."""

    def test_every_hazardous_character_survives_a_real_transient_unit(self) -> None:
        """The control for the silent trap, checked against systemd itself.

        systemd expands `$VAR` in Exec arguments, so a value carrying a dollar
        sign -- or the usual `"$0" "$@"` shell wrapper -- is replaced with an
        empty string and the unit succeeds having done nothing. Nothing short of
        running systemd proves this, so this test runs it.
        """

        if subprocess.run(
            ["systemd-run", "--user", "--version"], capture_output=True
        ).returncode != 0:
            self.skipTest("no user systemd manager available")
        with tempfile.TemporaryDirectory() as raw:
            report = Path(raw) / "report.txt"
            program = (
                "import sys, pathlib\n"
                f'pathlib.Path({str(report)!r}).write_text("|".join(sys.argv[1:]))\n'
            )
            argv = [
                sys.executable,
                "-c",
                program,
                "plain",
                "dollar $VAR and ${BRACE}",
                "quotes ' and \"",
                "tab\tand\nnewline",
            ]
            unit = f"validate-teardown-test-{os.getpid()}"
            subprocess.run(
                [
                    "systemd-run",
                    "--user",
                    "--collect",
                    "--unit",
                    unit,
                    "--property",
                    "TimeoutStopSec=60",
                    "--property",
                    f"ExecStopPost={su.systemd_exec_value(argv)}",
                    "--",
                    "/bin/true",
                ],
                check=True,
                capture_output=True,
            )
            for _ in range(200):
                if report.exists():
                    break
                import time

                time.sleep(0.1)
            self.assertTrue(report.exists(), "ExecStopPost never wrote its report")
            self.assertEqual(
                report.read_text(),
                "|".join(argv[3:]),
            )

    def test_rendering_quotes_every_element_separately(self) -> None:
        value = su.systemd_exec_value(["/bin/prog", "one two", "three"])

        self.assertEqual(value, '"/bin/prog" "one two" "three"')


class TeardownPropertiesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)

    def test_the_unit_carries_a_teardown_and_an_explicit_stop_timeout(self) -> None:
        found = properties(build(self.root))

        self.assertIn("ExecStopPost", found)
        self.assertIn("validate-test-unit", found["ExecStopPost"])
        self.assertIn("--retire-unit", found["ExecStopPost"])
        self.assertEqual(
            found["TimeoutStopSec"], str(su.VALIDATE_TEARDOWN_TIMEOUT_SECONDS)
        )

    def test_the_stop_timeout_exceeds_systemd_default(self) -> None:
        """Measured: one 44 GB retirement is 84.31s for the removal alone.

        systemd's DefaultTimeoutStopUSec is 90 seconds, so accepting the default
        would kill a teardown part-way and leave a half-deleted checkout -- worse
        than an intact one, because it is neither evidence nor reclaimed space.
        """

        self.assertGreater(su.VALIDATE_TEARDOWN_TIMEOUT_SECONDS, 90)

    def test_a_checkout_backed_launcher_runs_its_own_tooling_copy(self) -> None:
        found = properties(build(self.root))

        self.assertIn(
            str(self.root / su.TEARDOWN_ENTRY_POINT), found["ExecStopPost"]
        )

    def test_an_fd_backed_launcher_never_bakes_in_its_expiring_proc_path(
        self,
    ) -> None:
        """An fd tool root dies with its launcher; the teardown outlives both.

        ExecStopPost runs after the main process, by which point the unit's own
        private root is released too, so the teardown must re-enter through the
        installed bootstrap at the exact cached parent SHA rather than name any
        `/proc/<pid>/fd/<n>` path.
        """

        teardown_prefix = [
            sys.executable,
            "-c",
            su.PINNED_BOOTSTRAP_EXEC,
            "/home/tester/.local/libexec/dev-hermit/operational-tool",
            "0" * 64,
            "--run-current-tool",
            "--tool-relative",
            su.TEARDOWN_ENTRY_POINT,
            "--",
        ]
        found = properties(
            build(
                self.root,
                unit_tool_prefix=["/proc/1234/fd/5/ci-hub/ci-hub"],
                teardown_tool_prefix=teardown_prefix,
            )
        )

        self.assertIn("--run-current-tool", found["ExecStopPost"])
        self.assertIn(su.TEARDOWN_ENTRY_POINT, found["ExecStopPost"])
        self.assertNotIn("/proc/1234/fd/", found["ExecStopPost"])

    def test_the_teardown_entry_point_is_executable_in_git(self) -> None:
        """The operational-tool refuses a `--tool-relative` that is not +x.

        It checks `[[ ! -L $path && -f $path && -x $path ]]` against the file in
        the materialized parent SHA, so the mode has to be recorded in Git, not
        merely set in one checkout. Strip the bit and every fd-backed validate's
        teardown refuses with "immutable tool is not executable".
        """

        listed = subprocess.run(
            ["git", "-C", str(REPOSITORY_ROOT), "ls-files", "-s", su.TEARDOWN_ENTRY_POINT],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
        self.assertTrue(listed, f"{su.TEARDOWN_ENTRY_POINT} is not tracked")
        self.assertEqual(listed[0], "100755", listed)


class RetireWorkingDirectoryTest(unittest.TestCase):
    def test_retire_stands_outside_the_tree_before_reading_the_record(self) -> None:
        """Two callers arrive with cwd inside the checkout they are deleting.

        ExecStopPost inherits the unit's WorkingDirectory, which IS the checkout,
        and `wrkslots remove-validate-batch` re-enters through `systemd-run
        --working-directory=$PWD`. Without this the remover's own unit refuses to
        start, and the refusal names a missing directory rather than the cause.
        """

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            inside = root / "worktrees/validate/validate-fresh-abc-1-def"
            inside.mkdir(parents=True)
            previous = os.getcwd()
            os.chdir(inside)
            try:
                # There is no run handle, so this skips -- but only AFTER the
                # chdir, which is the property under test.
                self.assertEqual(
                    su.retire_unit_after_writeback(
                        "validate-absent-unit",
                        root=root,
                        run=su.run_command,
                        json_output=False,
                    ),
                    0,
                )
                self.assertEqual(Path(os.getcwd()), Path("/"))
            finally:
                os.chdir(previous)

    def test_a_unit_that_never_wrote_a_handle_is_skipped_not_failed(self) -> None:
        """ExecStopPost fires for a launch refused at admission too.

        Such a unit records no checkout. Exiting non-zero would write a failing
        teardown into the journal of every refused launch, which is how a real
        teardown failure becomes invisible.
        """

        with tempfile.TemporaryDirectory() as raw:
            self.assertEqual(
                su.retire_unit_after_writeback(
                    "validate-never-launched",
                    root=Path(raw),
                    run=su.run_command,
                    json_output=False,
                ),
                0,
            )


if __name__ == "__main__":
    unittest.main()
