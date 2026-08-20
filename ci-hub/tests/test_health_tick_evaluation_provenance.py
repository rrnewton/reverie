#!/usr/bin/env python3
"""Regression tests for visible health-tick evaluation provenance."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HEALTH_TICK = ROOT / "ci-hub" / "bin" / "health-tick"
OBSERVED_AT = "2026-08-20T19:57:31Z"


class HealthTickEvaluationProvenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="health-tick-provenance-")
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        (self.repo / "ci-hub" / "bin").mkdir(parents=True)
        shutil.copy2(HEALTH_TICK, self.repo / "ci-hub" / "bin" / "health-tick")

        agent_tool = self.repo / "ci-hub" / "bin" / "agent-tool"
        agent_tool.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                printf 'fake-agent stderr\\n' >&2
                case ${FAKE_TICK_MODE:-warnings} in
                    warnings)
                        printf 'HEALTH: evaluated\\n'
                        printf 'ACTION: investigate component=pin title="Reverie pin is stale"\\n'
                        printf 'NOTE: pin lag is within the note band\\n'
                        ;;
                    quiet)
                        printf 'HEALTH: quiet evaluation\\n'
                        ;;
                    *) exit 64 ;;
                esac
                exit "${FAKE_TICK_EXIT:-0}"
                """
            )
        )
        agent_tool.chmod(0o755)

        self.fake_bin = self.repo / "fake-bin"
        self.fake_bin.mkdir()
        fake_date = self.fake_bin / "date"
        fake_date.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                [[ $* == '-u +%Y-%m-%dT%H:%M:%SZ' ]]
                printf '{OBSERVED_AT}\\n'
                """
            )
        )
        fake_date.chmod(0o755)

        self._git("init", "-q")
        self._git("add", "ci-hub")
        self._git(
            "-c",
            "user.name=Health Tick Test",
            "-c",
            "user.email=health-tick-test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        )
        self.input_sha = self._git("rev-parse", "HEAD").stdout.strip()

    def _git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=self.repo,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def _run(self, *, mode: str, exit_status: int = 0) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.pop("HERMIT_AGENT_SNAPSHOT_JSON", None)
        environment.update(
            {
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "FAKE_TICK_MODE": mode,
                "FAKE_TICK_EXIT": str(exit_status),
                "PATH": f"{self.fake_bin}:{environment['PATH']}",
            }
        )
        return subprocess.run(
            [str(self.repo / "ci-hub" / "bin" / "health-tick")],
            cwd=self.repo,
            check=False,
            text=True,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_action_and_note_are_visibly_dated_without_suppression(self) -> None:
        result = self._run(mode="warnings", exit_status=23)
        provenance = f"observed_at={OBSERVED_AT} input_sha={self.input_sha}"

        self.assertEqual(23, result.returncode)
        self.assertEqual("fake-agent stderr\n", result.stderr)
        self.assertEqual(
            [
                "HEALTH: evaluated",
                "ACTION: investigate component=pin "
                f'title="Reverie pin is stale {provenance}"',
                f"NOTE: pin lag is within the note band {provenance}",
            ],
            result.stdout.splitlines(),
        )
        self.assertEqual(1, result.stdout.count("ACTION:"))
        self.assertEqual(1, result.stdout.count("NOTE:"))

    def test_quiet_evaluation_remains_quiet(self) -> None:
        result = self._run(mode="quiet")

        self.assertEqual(0, result.returncode)
        self.assertEqual("fake-agent stderr\n", result.stderr)
        self.assertEqual("HEALTH: quiet evaluation\n", result.stdout)
        self.assertNotIn("ACTION:", result.stdout)
        self.assertNotIn("NOTE:", result.stdout)


if __name__ == "__main__":
    unittest.main()
