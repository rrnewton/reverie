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

        health = self.repo / "ci-hub" / "health"
        health.mkdir()
        operational_health = health / "operational_health.py"
        operational_health.write_text(
            textwrap.dedent(
                """\
                import os
                import pathlib
                import sys

                if sys.argv[1:] != ["cache-agents"]:
                    raise SystemExit(64)
                record = os.environ.get("FAKE_CACHE_RECORD")
                if record:
                    pathlib.Path(record).write_text(
                        os.environ.get("HERMIT_AGENT_SNAPSHOT_JSON", "missing")
                    )
                raise SystemExit(int(os.environ.get("FAKE_CACHE_EXIT", "0")))
                """
            )
        )

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

    def _run(
        self,
        *,
        mode: str,
        exit_status: int = 0,
        snapshot: str | None = None,
        cache_exit: int = 0,
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.pop("HERMIT_AGENT_SNAPSHOT_JSON", None)
        cache_record = self.repo / "cache-record.json"
        environment.update(
            {
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "FAKE_TICK_MODE": mode,
                "FAKE_TICK_EXIT": str(exit_status),
                "FAKE_CACHE_EXIT": str(cache_exit),
                "FAKE_CACHE_RECORD": str(cache_record),
                "PATH": f"{self.fake_bin}:{environment['PATH']}",
            }
        )
        if snapshot is not None:
            environment["HERMIT_AGENT_SNAPSHOT_JSON"] = snapshot
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

    def test_failed_producer_names_its_exit_and_every_unobserved_check(self) -> None:
        (self.repo / "ci-hub" / "health" / "tick-hub.yaml").write_text(
            "checks:\n  - name: first\n  - name: second\n"
        )
        result = self._run(mode="does-not-run", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn(
            "ERROR: tick exited 64 before reporting; 2 configured check(s) DID NOT RUN",
            result.stdout,
        )
        action = next(line for line in result.stdout.splitlines() if line.startswith("ACTION:"))
        self.assertIn("detail=\"producer exit 64; 2 configured check(s) unobserved", action)
        self.assertIn("the tick exited 64 before running any gate", action)
        self.assertNotIn("producer exit ;", action)
        self.assertEqual(1, result.stdout.count("ACTION:"))

    def test_failed_producer_never_reports_zero_when_config_has_no_checks(self) -> None:
        (self.repo / "ci-hub" / "health" / "tick-hub.yaml").write_text("checks:\n")
        result = self._run(mode="does-not-run", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn("unknown configured check(s) DID NOT RUN", result.stdout)
        self.assertNotIn("0 configured check(s)", result.stdout)

    def test_scheduled_snapshot_is_passed_to_cache_agents(self) -> None:
        snapshot = '[{"name":"worker","tmux_pane_id":"%7"}]'
        result = self._run(mode="quiet", snapshot=snapshot)

        self.assertEqual(0, result.returncode)
        self.assertEqual(snapshot, (self.repo / "cache-record.json").read_text())

    def test_a_rejected_snapshot_does_not_hide_every_other_gate(self) -> None:
        result = self._run(mode="quiet", snapshot="[]", cache_exit=1)

        self.assertEqual(1, result.returncode)
        self.assertEqual("HEALTH: quiet evaluation\n", result.stdout)
        self.assertIn("could not cache the ORC agent snapshot", result.stderr)


if __name__ == "__main__":
    unittest.main()
