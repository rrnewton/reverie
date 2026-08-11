#!/usr/bin/env python3
"""Unit tests for the ci-hub README command extractor."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("documented_commands.py")
SPEC = importlib.util.spec_from_file_location("documented_commands", MODULE_PATH)
assert SPEC and SPEC.loader
documented_commands = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = documented_commands
SPEC.loader.exec_module(documented_commands)


class DocumentedCommandsTest(unittest.TestCase):
    def test_repository_inventory_is_complete_and_classified(self) -> None:
        commands = documented_commands.extract_commands()
        self.assertEqual(len(commands), documented_commands.EXPECTED_COMMANDS)
        self.assertEqual(
            {command.mode for command in commands},
            {"setup", "parse", "local-read", "live-read"},
        )
        self.assertTrue(
            any(
                "directives/check.py --quickstart" in command.text
                for command in commands
            )
        )
        self.assertTrue(any("ci-hub/ci-hub quickstart" in command.text for command in commands))
        self.assertTrue(any("systemd-run --user" in command.text for command in commands))
        self.assertFalse(
            any(
                "hosted-status --repo rrnewton/dev-hermit" in command.text
                for command in commands
            )
        )
        reconcile_commands = [
            command
            for command in commands
            if command.text.startswith("./ci-hub/bin/reconcile-receipts")
        ]
        self.assertEqual(len(reconcile_commands), 2)
        self.assertEqual({command.mode for command in reconcile_commands}, {"live-read"})

    def test_systemd_activation_is_syntax_only(self) -> None:
        command = "systemd-run --user --unit=fixture /bin/true"
        self.assertEqual(documented_commands._classify(command), "parse")
        self.assertEqual(documented_commands._parse_probe(command), "systemd-run --help")

    def test_reconcile_receipts_is_a_live_read_with_exact_binary_identity(self) -> None:
        self.assertEqual(
            documented_commands._classify(
                "./ci-hub/bin/reconcile-receipts # human table"
            ),
            "live-read",
        )
        with self.assertRaises(documented_commands.DocsCommandError):
            documented_commands._classify(
                "./ci-hub/bin/reconcile-receipts-untrusted # human table"
            )

    def test_reconcile_receipts_non_live_mode_executes_only_help(self) -> None:
        command = documented_commands.DocumentedCommand(
            documented_commands.ROOT / "ci-hub/README.md",
            73,
            "./ci-hub/bin/reconcile-receipts --json",
            "live-read",
        )
        self.assertEqual(
            documented_commands._parse_probe(command.text),
            "./ci-hub/bin/reconcile-receipts --help",
        )
        reports = documented_commands._run_one(
            command,
            root=documented_commands.ROOT,
            environment=os.environ.copy(),
            live=False,
            verify_purity=False,
        )
        self.assertIn("PASS live-read", reports[0])

    def test_unclassified_command_fails_loudly(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "README.md"
            path.write_text("```bash\n./not-a-real-tool --quiet\n```\n")
            with self.assertRaises(documented_commands.DocsCommandError):
                documented_commands.extract_commands((path,))

    def test_cost_lines_do_not_count_as_domain_output(self) -> None:
        output = (
            "# x tool COST ESTIMATE wall=unknown cpu=unknown basis='unknown'\n"
            "# x tool COST ACTUAL wall=0.1s cpu=0.1s exit=0\n"
        )
        self.assertEqual(documented_commands._business_output(output), "")

    def test_mtime_only_change_is_a_purity_failure(self) -> None:
        self.assertEqual(
            documented_commands._changed_mtimes({"a": 1, "b": 2}, {"a": 3, "b": 2}),
            ["a"],
        )

    def test_tg_quickstart_contract_rejects_home_writes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "tg"
            binary.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' 'TaskGraph agent quickstart' "
                "'tg claim TASK_ID' 'tg note TASK_ID' 'TG_DB_PATH'\n"
            )
            binary.chmod(0o755)
            reports = documented_commands._run_tg_quickstart(
                str(binary),
                root=documented_commands.ROOT,
                environment=os.environ.copy(),
                verify_purity=False,
            )
            self.assertIn("PASS quickstart tg", reports[0])

            binary.write_text(binary.read_text() + "touch \"$HOME/side-effect\"\n")
            with self.assertRaises(documented_commands.DocsCommandError):
                documented_commands._run_tg_quickstart(
                    str(binary),
                    root=documented_commands.ROOT,
                    environment=os.environ.copy(),
                    verify_purity=False,
                )

    def test_closeout_requires_push_and_dirty_tree_accounting(self) -> None:
        with self.assertRaises(documented_commands.DocsCommandError):
            documented_commands._evaluate_closeout(
                head="a" * 40,
                origin_main="b" * 40,
                unpushed=1,
                dirty="",
                dirty_note=None,
            )
        with self.assertRaises(documented_commands.DocsCommandError):
            documented_commands._evaluate_closeout(
                head="a" * 40,
                origin_main="a" * 40,
                unpushed=0,
                dirty=" M concurrent.txt",
                dirty_note=None,
            )
        reports = documented_commands._evaluate_closeout(
            head="a" * 40,
            origin_main="a" * 40,
            unpushed=0,
            dirty=" M concurrent.txt",
            dirty_note="concurrent.txt is owned by hermit-226 and intentionally left unchanged",
        )
        self.assertIn("concurrent.txt", "\n".join(reports))


class CouldNotMeasureTest(unittest.TestCase):
    """The third state. A probe that measures nothing must not read as a pass.

    Both directions, with counts, because the defect this replaces was a probe
    that was green in three different ways at once: SKIPped on hosted CI (tg
    absent), failing locally behind an unrelated red, and unit-tested only
    against a stub that satisfied the contract by construction.
    """

    def _stub_tg(self, directory: str, commands_block: str) -> str:
        binary = Path(directory) / "tg"
        binary.write_text(
            "#!/bin/sh\ncat <<'EOF'\nSome prose preamble mentioning quickstart.\n\n"
            f"Commands:\n{commands_block}\n\nArguments:\n  [TASK]...\nEOF\n"
        )
        binary.chmod(0o755)
        return str(binary)

    def test_help_scanner_reads_the_commands_block_not_the_prose(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # NEGATIVE: 'quickstart' appears in the prose only -- must be False,
            # otherwise the scanner would happily confirm a fictional interface.
            absent = self._stub_tg(tmp, "  show   Show a task\n  agent-setup   Set up")
            self.assertFalse(documented_commands._tg_exposes_quickstart(absent))
        with tempfile.TemporaryDirectory() as tmp:
            # POSITIVE: same scanner, real entry -- proves it is not just False.
            present = self._stub_tg(tmp, "  quickstart   Print the primer\n  show   Show")
            self.assertTrue(documented_commands._tg_exposes_quickstart(present))
        # And a binary that does not exist answers False rather than raising.
        self.assertFalse(documented_commands._tg_exposes_quickstart("/nonexistent/tg"))

    def test_a_declared_could_not_measure_is_reported_not_swallowed(self) -> None:
        report = documented_commands._unverifiable("quickstart tg", "tg is not on PATH")
        self.assertTrue(report.startswith("UNVERIFIABLE "))
        self.assertIn("tg is not on PATH", report)
        self.assertIn("declared:", report)

    def test_an_UNDECLARED_could_not_measure_is_rc_2(self) -> None:
        """The fail-closed direction, matching unowned_backlog.py's rc 2."""
        with self.assertRaises(documented_commands.DocsUnverifiable):
            documented_commands._unverifiable("probe-nobody-declared", "no tool")
        original = documented_commands.run
        try:
            def _raise(**_kwargs):
                raise documented_commands.DocsUnverifiable("probe-x: measured nothing")

            documented_commands.run = _raise
            self.assertEqual(documented_commands.main([]), 2)
        finally:
            documented_commands.run = original

    def test_unverifiable_is_counted_apart_from_measured_probes(self) -> None:
        """probes= must count MEASUREMENTS. The old code put the SKIP string in
        that same list, so measuring nothing inflated the headline like a pass."""
        original = documented_commands.run
        try:
            documented_commands.run = lambda **_kwargs: [
                "PASS a",
                "PASS b",
                "UNVERIFIABLE quickstart tg: ... -- declared: ...",
            ]
            buffer = io.StringIO()
            with contextlib.redirect_stdout(buffer):
                self.assertEqual(documented_commands.main([]), 0)
            summary = buffer.getvalue().splitlines()[-1]
            self.assertIn("probes=2", summary)
            self.assertIn("unverifiable=1", summary)
            self.assertNotIn("probes=3", summary)
        finally:
            documented_commands.run = original

    def test_declared_unmeasurable_entries_are_still_true(self) -> None:
        """A declaration must not outlive its reason.

        Asserts something real in BOTH branches -- a branch that asserted
        nothing would be the very hole this class exists to close.
        """
        self.assertIn("quickstart tg", documented_commands.DECLARED_UNMEASURABLE)
        binary = os.environ.get("CI_HUB_TG_BIN") or shutil.which("tg")
        if binary is None:
            # Tool absent IS one of the declared reasons; assert the machinery
            # actually fires rather than silently returning nothing.
            report = documented_commands._unverifiable("quickstart tg", "tg absent")
            self.assertTrue(report.startswith("UNVERIFIABLE "))
            return
        self.assertFalse(
            documented_commands._tg_exposes_quickstart(binary),
            "tg now HAS a `quickstart` subcommand, so this probe is measurable "
            "again -- delete the DECLARED_UNMEASURABLE['quickstart tg'] entry and "
            "let the purity probe run for real.",
        )


if __name__ == "__main__":
    unittest.main()
