#!/usr/bin/env python3
"""Bracket the live merge-authority verifier, including both half states."""

from __future__ import annotations

import contextlib
import io
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import operational_health


ROOT = Path(__file__).resolve().parents[3]
REAL_CHECKER = ROOT / "hermit" / "scripts" / "configure-merge-gate-ruleset.sh"


class MergeGateAuthorityTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="merge-authority-")
        self.addCleanup(self._tmp.cleanup)
        self.bin = Path(self._tmp.name) / "bin"
        self.bin.mkdir()
        self._write_executable("with-proxy", '#!/usr/bin/env bash\nexec "$@"\n')
        self._write_executable(
            "gh",
            r'''#!/usr/bin/env python3
import json
import os
import sys

args = sys.argv[1:]
if args[:2] == ["api", "--paginate"] and args[2:] == ["repos/rrnewton/hermit/rulesets"]:
    print(json.dumps([{"id": 20244443, "name": "main check gating (admin-bypassable)"}]))
    raise SystemExit(0)
if args[:2] == ["api", "repos/rrnewton/hermit/rulesets/20244443"]:
    print(json.dumps({
        "id": 20244443,
        "name": "main check gating (admin-bypassable)",
        "target": "branch",
        "enforcement": "active",
        "conditions": {},
        "rules": [{"type": "required_status_checks", "parameters": {
            "required_status_checks": [{"context": "merge-gate-v4", "integration_id": 15368}]
        }}],
        "bypass_actors": [],
    }))
    raise SystemExit(0)
if (args[:3] == ["api", "--method", "GET"] and
        "contents/.github/workflows/merge-gate.yml" in args[3]):
    print(os.environ["STUB_MAIN_BLOB"])
    raise SystemExit(0)
if args[:2] == ["variable", "get"]:
    if os.environ.get("STUB_VARIABLE_AUTH_FAIL") == "1":
        print("HTTP 403: Resource not accessible", file=sys.stderr)
        raise SystemExit(1)
    name = args[2]
    if name == "MERGE_GATE_V4_BLOB":
        print(os.environ["STUB_EXPECTED_BLOB"])
    elif name == "MERGE_GATE_LEGACY_CONTEXT":
        print(os.environ.get("STUB_LEGACY_SHIM", "false"))
    else:
        raise SystemExit(1)
    raise SystemExit(0)
if args[:2] == ["variable", "list"]:
    if os.environ.get("STUB_VARIABLE_AUTH_FAIL") == "1":
        print("HTTP 403: Resource not accessible", file=sys.stderr)
        raise SystemExit(1)
    print(json.dumps([{"name": "MERGE_GATE_V4_BLOB"}, {"name": "MERGE_GATE_LEGACY_CONTEXT"}]))
    raise SystemExit(0)
print("unsupported fake gh invocation: " + repr(args), file=sys.stderr)
raise SystemExit(2)
''',
        )

    def _write_executable(self, name: str, content: str) -> Path:
        path = self.bin / name
        path.write_text(content)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return path

    def _run(
        self, main_blob: str, expected_blob: str, *, auth_fail: bool = False
    ) -> tuple[int, dict[str, str]]:
        env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "STUB_MAIN_BLOB": main_blob,
            "STUB_EXPECTED_BLOB": expected_blob,
            "STUB_LEGACY_SHIM": "false",
        }
        if auth_fail:
            env["STUB_VARIABLE_AUTH_FAIL"] = "1"
        output = io.StringIO()
        with mock.patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(checker=REAL_CHECKER)
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        return rc, fields

    def test_matching_authority_and_main_are_accepted(self) -> None:
        rc, fields = self._run("a" * 40, "a" * 40)
        self.assertEqual(0, rc)
        self.assertEqual("ok", fields["state"])
        self.assertEqual("none", fields["scope"])

    def test_authority_advanced_before_main_is_loudly_refused(self) -> None:
        old, new = "a" * 40, "b" * 40
        rc, fields = self._run(old, new)
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertEqual("global-merge-admission", fields["scope"])
        self.assertIn(old, fields["summary"])
        self.assertIn(new, fields["summary"])

    def test_main_advanced_before_authority_is_loudly_refused(self) -> None:
        old, new = "a" * 40, "b" * 40
        rc, fields = self._run(new, old)
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertEqual("global-merge-admission", fields["scope"])
        self.assertIn(old, fields["summary"])
        self.assertIn(new, fields["summary"])

    def test_unreadable_variables_are_unknown_not_a_transition(self) -> None:
        rc, fields = self._run("a" * 40, "a" * 40, auth_fail=True)
        self.assertEqual(2, rc)
        self.assertEqual("unavailable", fields["state"])
        self.assertEqual("unknown", fields["scope"])
        self.assertIn("NOT transition evidence", fields["summary"])

    def test_timeout_is_unknown_not_a_transition(self) -> None:
        sleeper = self._write_executable(
            "slow-checker", "#!/usr/bin/env python3\nwhile True:\n    pass\n"
        )
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(
                checker=sleeper, timeout=0.05
            )
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        self.assertEqual(2, rc)
        self.assertEqual("unavailable", fields["state"])
        self.assertIn("timeout", fields["summary"])

    def test_wrapper_zero_without_verifier_pass_is_unknown(self) -> None:
        empty_success = self._write_executable(
            "empty-success", "#!/usr/bin/env bash\nexit 0\n"
        )
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(checker=empty_success)
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        self.assertEqual(2, rc)
        self.assertEqual("unavailable", fields["state"])
        self.assertNotEqual("ok", fields["state"])

    def test_tick_adopts_the_global_gate_at_five_minute_cadence(self) -> None:
        config = yaml.safe_load((ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text())
        gates = {gate["name"]: gate for gate in config["reminders"]}
        gate = gates["hermit_merge_gate_authority"]
        self.assertEqual(
            "./ci-hub/health/operational_health.py merge-gate-authority",
            gate["gate"]["cmd"],
        )
        self.assertLessEqual(gate["cadence_secs"], 300)
        self.assertIn("GLOBAL", gate["emit"]["title"])
        self.assertLess(
            operational_health.MERGE_GATE_AUTHORITY_TIMEOUT_SECS
            + operational_health.MERGE_GATE_AUTHORITY_PROBE_TIMEOUT_SECS,
            30,
        )


if __name__ == "__main__":
    unittest.main()
