"""The actual dispatch boundary reserves stdout for the requested tool.

Materialization is a fixture here: no network, installed wrapper, or authority
verifier is exercised. Its progress reproduces Git's cold submodule output.
"""

from pathlib import Path
import os
import re
import subprocess
import tempfile
import unittest


WRAPPER = Path(__file__).resolve().parents[2] / "scripts/hermit-health-tick.sh"
PROGRESS = "Submodule path 'hermit': checked out 'fixture'"


class OperationalToolStdoutTests(unittest.TestCase):
    def run_dispatch(self, *, setup_exit=0, payload_exit=0):
        match = re.search(
            r"^run_current_tool\(\) \{.*?^\}$",
            WRAPPER.read_text(),
            re.MULTILINE | re.DOTALL,
        )
        self.assertIsNotNone(match, "production dispatch function must be present")
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
            return subprocess.run(
                [
                    "bash", "-c", preamble + "\n" + match.group(0)
                    + '\nrun_current_tool --run-current-tool --state-root "$FIXTURE_STATE"'
                    + ' --tool-relative payload -- requested-argument\n',
                ],
                env=os.environ | {
                    "FIXTURE_STATE": str(state),
                    "FIXTURE_CACHE": str(root / "cache"),
                    "FIXTURE_TOOL": str(tool),
                    "FIXTURE_PROGRESS": PROGRESS,
                    "FIXTURE_SETUP_EXIT": str(setup_exit),
                    "FIXTURE_PAYLOAD_EXIT": str(payload_exit),
                },
                capture_output=True, text=True, timeout=10, check=False,
            )

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


if __name__ == "__main__":
    unittest.main()
