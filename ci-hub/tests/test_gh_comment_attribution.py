#!/usr/bin/env python3
"""Mutation brackets for the fleet-aware gh write boundary."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SHIM = ROOT / "ci-hub/bin/gh"


class FleetGhTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.capture = root / "capture.json"
        self.fake = root / "real-gh"
        self.fake.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['GH_CAPTURE']).write_text(json.dumps(sys.argv[1:]))\n"
        )
        self.fake.chmod(0o755)
        self.env = os.environ.copy()
        self.env.update(
            FLEET_REAL_GH=str(self.fake),
            GH_CAPTURE=str(self.capture),
            DG_AGENT_NAME="codex-boundary",
            AGENT_SESSION_ID="orc-session-123",
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def run_shim(self, *args: str, env: dict[str, str] | None = None):
        return subprocess.run(
            [str(SHIM), *args], env=env or self.env, text=True, capture_output=True
        )

    def captured(self) -> list[str]:
        return json.loads(self.capture.read_text())

    def test_pr_comment_is_attributed_at_write_boundary(self) -> None:
        result = self.run_shim("pr", "comment", "42", "--body", "evidence")
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertEqual(
            body,
            "[fleet-comment agent=codex-boundary session=orc-session-123] evidence",
        )

    def test_owner_shell_without_agent_identity_is_unchanged(self) -> None:
        env = dict(self.env)
        env.pop("DG_AGENT_NAME")
        result = self.run_shim("pr", "comment", "42", "--body", "owner words", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["pr", "comment", "42", "--body", "owner words"])

    def test_agent_without_session_is_refused_before_write(self) -> None:
        env = dict(self.env)
        for name in (
            "AGENT_SESSION_ID", "ORC_AGENT_SESSION_ID", "CODEX_THREAD_ID",
            "CLAUDE_CODE_PARENT_SESSION_ID", "META_3PAI_INVOCATION_ID",
        ):
            env.pop(name, None)
        result = self.run_shim("pr", "comment", "42", "--body", "ambiguous", env=env)
        self.assertEqual(result.returncode, 2)
        self.assertIn("no fleet session identity", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_review_without_body_gets_attribution_body(self) -> None:
        result = self.run_shim("pr", "review", "42", "--approve")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.captured()[-2:],
            ["--body", "[fleet-comment agent=codex-boundary session=orc-session-123]"],
        )

    def test_raw_api_comment_body_is_attributed(self) -> None:
        result = self.run_shim(
            "api", "-X", "POST", "repos/o/r/issues/42/comments", "-f", "body=evidence"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "body=[fleet-comment agent=codex-boundary session=orc-session-123] evidence",
            self.captured(),
        )

    def test_raw_api_review_body_is_attributed(self) -> None:
        result = self.run_shim(
            "api", "-X", "POST", "repos/o/r/pulls/42/reviews", "-f", "body=approved"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(
            "body=[fleet-comment agent=codex-boundary session=orc-session-123] approved",
            self.captured(),
        )

    def test_raw_graphql_comment_mutation_is_refused(self) -> None:
        result = self.run_shim(
            "api", "graphql", "-f", "query=mutation { addDiscussionComment(input: {}) }"
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("cannot be attributed safely", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_foreign_prefix_is_refused(self) -> None:
        result = self.run_shim(
            "pr", "comment", "42", "--body",
            "[fleet-comment agent=someone-else session=other]\n\nwords",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("different agent or session", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_non_comment_command_passes_through(self) -> None:
        result = self.run_shim("pr", "view", "42", "--json", "headRefOid")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["pr", "view", "42", "--json", "headRefOid"])


if __name__ == "__main__":
    unittest.main()
