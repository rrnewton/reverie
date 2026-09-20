"""The actual dispatch boundary reserves stdout for the requested tool.

Materialization is a fixture here: no network, installed wrapper, or authority
verifier is exercised. Its progress reproduces Git's cold submodule output.
"""

from pathlib import Path
import os
import re
import shlex
import subprocess
import tempfile
import unittest


WRAPPER = Path(__file__).resolve().parents[2] / "scripts/hermit-health-tick.sh"
PROGRESS = "Submodule path 'hermit': checked out 'fixture'"


class OperationalToolStdoutTests(unittest.TestCase):
    def run_dispatch(self, *, setup_exit=0, payload_exit=0,
                     arguments=("requested-argument",)):
        functions = []
        for name in ("operational_tool_usage", "parse_current_tool_arguments", "run_current_tool"):
            match = re.search(
                rf"^{name}\(\) \{{.*?^\}}$", WRAPPER.read_text(),
                re.MULTILINE | re.DOTALL,
            )
            self.assertIsNotNone(match, f"production function {name} must be present")
            functions.append(match.group(0))
        with tempfile.TemporaryDirectory(prefix="operational-stdout-") as directory:
            root = Path(directory)
            state = root / "state"
            tool = root / "tool"
            state.mkdir()
            tool.mkdir()
            payload = tool / "payload"
            payload.write_text(
                '#!/bin/sh\n'
                'printf \'{"verdict":"fixture","arg":"%s"}\\n\' "$1"\n'
                'printf \'payload diagnostic\\n\' >&2\n'
                'printf \'%s\\0\' "$@" >"$FIXTURE_ARGUMENTS"\n'
                'exit "$FIXTURE_PAYLOAD_EXIT"\n'
            )
            payload.chmod(0o755)
            # The stub only supplies materialization's outputs to the unchanged
            # production dispatch. These shape-valid values are not real seals.
            preamble = r'''
set -uo pipefail
ROOT=$FIXTURE_STATE
TOOL_CACHE=$FIXTURE_CACHE
materialize_current_tool_root() {
    printf '%s\n' "$FIXTURE_PROGRESS"
    if [[ $FIXTURE_SETUP_EXIT != 0 ]]; then
        return "$FIXTURE_SETUP_EXIT"
    fi
    TOOL_ROOT_RESULT=$FIXTURE_TOOL
    TOOL_PARENT_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    TOOL_HERMIT_SHA=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    TOOL_AGENT_UTILS_SHA=cccccccccccccccccccccccccccccccccccccccc
    TOOL_BOOTSTRAP_SHA256=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
    TOOL_CONTENT_SHA256=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
    TOOL_AUTHORITY_PATH=/proc/123/fd/4
}
verified_installed_operational_tool() { :; }
'''
            result = subprocess.run(
                [
                    "bash", "-c", preamble + "\n" + "\n".join(functions)
                    + '\nrun_current_tool --run-current-tool --state-root "$FIXTURE_STATE"'
                    + ' --tool-relative payload -- ' + shlex.join(arguments) + '\n',
                ],
                env=os.environ | {
                    "FIXTURE_STATE": str(state),
                    "FIXTURE_CACHE": str(root / "cache"),
                    "FIXTURE_TOOL": str(tool),
                    "FIXTURE_PROGRESS": PROGRESS,
                    "FIXTURE_SETUP_EXIT": str(setup_exit),
                    "FIXTURE_PAYLOAD_EXIT": str(payload_exit),
                    "FIXTURE_ARGUMENTS": str(root / "arguments"),
                },
                capture_output=True, text=True, timeout=10, check=False,
            )
            result.forwarded_arguments = (
                (root / "arguments").read_bytes() if (root / "arguments").exists() else None
            )
            return result

    def test_cold_setup_diagnostics_do_not_contaminate_payload(self):
        result = self.run_dispatch()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout, '{"verdict":"fixture","arg":"requested-argument"}\n'
        )
        self.assertIn(PROGRESS, result.stderr)
        self.assertIn("payload diagnostic", result.stderr)

    def test_failed_setup_retains_status_and_diagnostics_without_payload(self):
        result = self.run_dispatch(setup_exit=37)
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, PROGRESS + "\n")

    def test_failed_payload_retains_its_stdout_and_exit_code(self):
        result = self.run_dispatch(payload_exit=17)
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertEqual(
            result.stdout, '{"verdict":"fixture","arg":"requested-argument"}\n'
        )
        self.assertIn(PROGRESS, result.stderr)
        self.assertIn("payload diagnostic", result.stderr)

    def test_payload_help_and_literal_arguments_remain_child_owned(self):
        arguments = ("--help", "two words", "", "$(not-a-command)", "--unknown")
        for status in (0, 126, 127):
            with self.subTest(status=status):
                result = self.run_dispatch(payload_exit=status, arguments=arguments)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual(result.stdout, '{"verdict":"fixture","arg":"--help"}\n')
                self.assertEqual(result.forwarded_arguments,
                                 b"".join(arg.encode() + b"\0" for arg in arguments))
                self.assertIn("payload diagnostic", result.stderr)


class OperationalToolEntryTests(unittest.TestCase):
    """Exercise the complete entry dispatch with inert operational boundaries."""

    def run_entry(self, arguments, *, with_home=True, empty_home=False,
                  command_exit=37, instrument=True):
        with tempfile.TemporaryDirectory(prefix="operational-entry-") as directory:
            root = Path(directory)
            marker = root / "effects"
            home = root / "home"
            home.mkdir()
            retained = home / "retained-report"
            retained.write_bytes(b"existing report\n")
            before = retained.stat()
            fake_bin = root / "bin"
            fake_bin.mkdir()
            for name in (
                "git", "mkdir", "chmod", "mktemp", "flock", "find", "rm",
                "date", "stat", "realpath", "python3", "with-proxy", "setsid",
            ):
                executable = fake_bin / name
                executable.write_text(
                    '#!/bin/bash\nprintf "external:%s\\n" "$0" >>"$ENTRY_EFFECTS"\nexit 93\n'
                )
                executable.chmod(0o755)
            # Keep the real parser and bottom-level dispatcher. Only the actual
            # operational entry points/trap are replaced: no live health path,
            # materialization, lock, or report delivery can execute in this test.
            source = WRAPPER.read_text()
            boundary = "# The findings first, then the tool roots:"
            self.assertEqual(source.count(boundary), 1)
            inert = r'''
main() {
    printf 'main\0' >>"$ENTRY_EFFECTS"
    if [[ $# -gt 0 ]]; then printf '%s\0' "$@" >>"$ENTRY_EFFECTS"; fi
    return "$ENTRY_EXIT"
}
materialize_current_tool_root() {
    printf 'materialize\0' >>"$ENTRY_EFFECTS"
    return 73
}
finish_wrapper() { printf 'exit-trap\0' >>"$ENTRY_EFFECTS"; }
'''
            wrapper = root / "operational-tool"
            wrapper.write_text(source.replace(boundary, inert + "\n" + boundary)
                               if instrument else source)
            environment = {
                "PATH": str(fake_bin), "ENTRY_EFFECTS": str(marker),
                "ENTRY_EXIT": str(command_exit),
            }
            if with_home:
                environment["HOME"] = "" if empty_home else str(home)
            result = subprocess.run(
                ["/bin/bash", str(wrapper), *arguments], env=environment,
                capture_output=True, text=True, timeout=5, check=False,
            )
            effects = marker.read_bytes() if marker.exists() else b""
            self.assertEqual(retained.read_bytes(), b"existing report\n")
            self.assertEqual(retained.stat().st_mtime_ns, before.st_mtime_ns)
            self.assertEqual(list(home.iterdir()), [retained])
            return result, effects

    def test_standalone_installed_layout_needs_no_sibling_for_help(self):
        # Exact modified bytes, with no function replacement or sibling files.
        # External tripwires still prevent a regression from reaching live work.
        for flag in ("-h", "--help"):
            for arguments in ([flag], ["--run-current-tool", flag]):
                with self.subTest(arguments=arguments):
                    result, effects = self.run_entry(
                        arguments, with_home=False, instrument=False)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("usage:", result.stdout)
                    self.assertEqual(result.stderr, "")
                    self.assertEqual(effects, b"")

    def test_both_help_forms_precede_home_and_all_operational_boundaries(self):
        for flag in ("-h", "--help"):
            for arguments in ([flag], ["--run-current-tool", flag],
                              ["--run-current-tool", "--state-root", "/unused", flag]):
                for with_home, empty_home in ((False, False), (True, False), (True, True)):
                    with self.subTest(arguments=arguments, with_home=with_home, empty_home=empty_home):
                        result, effects = self.run_entry(
                            arguments, with_home=with_home, empty_home=empty_home)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn("usage:", result.stdout)
                        for option in ("--emit-output", "--run-current-tool", "--tool-relative"):
                            self.assertIn(option, result.stdout)
                        self.assertEqual(result.stderr, "")
                        self.assertEqual(effects, b"")

    def test_unknown_and_malformed_arguments_refuse_before_side_effects(self):
        cases = (["--unknown"], ["unexpected"], ["--emit-output", "unexpected"],
                 ["--emit-output", "--help"], ["--help", "unexpected"],
                 ["--run-current-tool", "--unknown"],
                 ["--run-current-tool", "--state-root"], ["--run-current-tool"])
        for arguments in cases:
            with self.subTest(arguments=arguments):
                result, effects = self.run_entry(arguments, with_home=False)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("--help", result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertEqual(effects, b"")

    def test_normal_health_modes_preserve_arguments_and_execution_failures(self):
        for arguments in ([], ["--emit-output"]):
            for status in (0, 37, 126, 127):
                with self.subTest(arguments=arguments, status=status):
                    result, effects = self.run_entry(arguments, command_exit=status)
                    self.assertEqual(result.returncode, status, result.stderr)
                    expected = b"main\0" + b"".join(a.encode() + b"\0" for a in arguments)
                    self.assertEqual(effects, expected + b"exit-trap\0")


if __name__ == "__main__":
    unittest.main()
