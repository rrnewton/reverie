#!/usr/bin/env python3
"""Bracket the live merge-authority verifier at the owner-policy boundary."""

from __future__ import annotations

import contextlib
import io
import time
import subprocess
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
REAL_CHECKER = Path(
    os.environ.get(
        "MERGE_GATE_AUTHORITY_TEST_CHECKER",
        ROOT / "hermit" / "scripts" / "configure-merge-gate-ruleset.sh",
    )
)


def _pinned_hermit_commit() -> str:
    try:
        proc = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD:hermit"],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"unknown (git rev-parse HEAD:hermit: {error})"
    if proc.returncode != 0:
        return f"unknown (git rev-parse HEAD:hermit rc={proc.returncode})"
    return proc.stdout.strip()


def require_real_checker(test: unittest.TestCase) -> None:
    """Refuse, naming the precondition, when the real verifier is absent.

    Without the script the gate can only report state=unavailable (exit 127),
    so a refusal bracket would compare nothing and the unavailable-evidence
    test would pass for the wrong reason.
    """
    if REAL_CHECKER.is_file() and os.access(REAL_CHECKER, os.X_OK):
        return
    test.fail(
        f"MISSING PRECONDITION: the real merge-authority verifier {REAL_CHECKER} "
        "is not an executable file. These tests bracket that script from the "
        f"hermit submodule, pinned at {_pinned_hermit_commit()}; populate "
        "hermit at that commit, or set MERGE_GATE_AUTHORITY_TEST_CHECKER to a "
        "copy of scripts/configure-merge-gate-ruleset.sh from it."
    )


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
    if os.environ.get("STUB_RULESET_AUTH_FAIL") == "1":
        print("HTTP 403: Resource not accessible", file=sys.stderr)
        raise SystemExit(1)
    print(json.dumps([
        {"id": 20244443, "name": "main check gating (admin-bypassable)"},
        {"id": 20307165, "name": "main history protection"},
    ]))
    raise SystemExit(0)
if args[:2] == ["api", "repos/rrnewton/hermit/rulesets/20244443"]:
    required = []
    context = os.environ.get("STUB_REQUIRED_CONTEXT", "")
    if context:
        required.append({"context": context, "integration_id": 15368})
    rules = []
    if required:
        rules.append({"type": "required_status_checks", "parameters": {
            "required_status_checks": required
        }})
    print(json.dumps({
        "id": 20244443,
        "name": "main check gating (admin-bypassable)",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {
            "include": ["~DEFAULT_BRANCH"],
            "exclude": [],
        }},
        "rules": rules,
        "bypass_actors": [],
    }))
    raise SystemExit(0)
if args[:2] == ["api", "repos/rrnewton/hermit/rulesets/20307165"]:
    rules = [{"type": "deletion"}]
    if os.environ.get("STUB_HISTORY_NON_FF", "1") == "1":
        rules.append({"type": "non_fast_forward"})
    if os.environ.get("STUB_HISTORY_LINEAR", "1") == "1":
        rules.append({"type": "required_linear_history"})
    print(json.dumps({
        "id": 20307165,
        "name": "main history protection",
        "target": os.environ.get("STUB_HISTORY_TARGET", "branch"),
        "enforcement": os.environ.get("STUB_HISTORY_ENFORCEMENT", "active"),
        "conditions": {"ref_name": {
            "include": ["~DEFAULT_BRANCH"],
            "exclude": [],
        }},
        "rules": rules,
        "bypass_actors": ([{"actor_id": 5}] if
                          os.environ.get("STUB_HISTORY_BYPASS", "0") == "1" else []),
    }))
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
        self,
        *,
        required_context: str = "",
        history_non_ff: bool = True,
        history_linear: bool = True,
        history_bypass: bool = False,
        history_target: str = "branch",
        ruleset_auth_fail: bool = False,
    ) -> tuple[int, dict[str, str]]:
        require_real_checker(self)
        env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "STUB_REQUIRED_CONTEXT": required_context,
            "STUB_HISTORY_NON_FF": "1" if history_non_ff else "0",
            "STUB_HISTORY_LINEAR": "1" if history_linear else "0",
            "STUB_HISTORY_BYPASS": "1" if history_bypass else "0",
            "STUB_HISTORY_TARGET": history_target,
        }
        if ruleset_auth_fail:
            env["STUB_RULESET_AUTH_FAIL"] = "1"
        output = io.StringIO()
        with mock.patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(checker=REAL_CHECKER)
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        return rc, fields

    def test_owner_policy_without_hosted_check_is_accepted(self) -> None:
        rc, fields = self._run()
        self.assertEqual(0, rc)
        self.assertEqual("ok", fields["state"])
        self.assertEqual("none", fields["scope"])

    def test_retired_v4_required_context_is_loudly_refused(self) -> None:
        rc, fields = self._run(required_context="merge-gate-v4")
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertEqual("global-merge-admission", fields["scope"])
        self.assertIn("types=required_status_checks", fields["summary"])

    def test_unrelated_required_context_is_loudly_refused(self) -> None:
        rc, fields = self._run(required_context="some-other-gate")
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertEqual("global-merge-admission", fields["scope"])
        self.assertIn("types=required_status_checks", fields["summary"])

    def test_missing_non_fast_forward_protection_is_loudly_refused(self) -> None:
        rc, fields = self._run(history_non_ff=False)
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertIn("types=deletion,required_linear_history", fields["summary"])

    def test_missing_linear_history_protection_is_loudly_refused(self) -> None:
        rc, fields = self._run(history_linear=False)
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertIn("types=deletion,non_fast_forward", fields["summary"])

    def test_bypassable_history_protection_is_loudly_refused(self) -> None:
        rc, fields = self._run(history_bypass=True)
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertIn("bypass_count=1", fields["summary"])

    def test_history_protection_scoped_off_branches_is_loudly_refused(self) -> None:
        rc, fields = self._run(history_target="tag")
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])
        self.assertIn("scope=tag", fields["summary"])

    def test_unreadable_rulesets_are_unknown_not_a_transition(self) -> None:
        rc, fields = self._run(ruleset_auth_fail=True)
        self.assertEqual(2, rc)
        self.assertEqual("unavailable", fields["state"])
        self.assertEqual("unknown", fields["scope"])
        self.assertIn("evidence unavailable", fields["summary"])

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

    def test_a_timed_out_checker_leaves_no_process_behind(self) -> None:
        """A timeout must kill what it timed out on.

        Regression guard for a measured leak: nine spinners from the test above
        had accumulated as PPID=1 orphans, 899% of a core between them, the
        oldest running 18.9 hours. `subprocess.run(timeout=...)` kills only the
        direct child, which is not the surviving process when the checker is
        reached through a shebang.
        """
        marker = "leak-probe-slow-checker"
        sleeper = self._write_executable(
            marker, "#!/usr/bin/env python3\nwhile True:\n    pass\n"
        )

        def survivors() -> int:
            found = subprocess.run(
                ["pgrep", "-f", f"{marker}"], capture_output=True, text=True
            )
            # Do not count this test's own pgrep: -f matches its command line too.
            return len(
                [
                    line
                    for line in found.stdout.split()
                    if line.strip() and line.strip() != str(os.getpid())
                ]
            )

        before = survivors()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            operational_health.merge_gate_authority_gate(checker=sleeper, timeout=0.05)
        time.sleep(0.3)
        self.assertEqual(
            before,
            survivors(),
            "the timed-out checker is still running: a timeout that abandons its "
            "child leaks a whole core per invocation",
        )

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

    def test_old_prose_pass_is_not_a_typed_result(self) -> None:
        prose_success = self._write_executable(
            "prose-success", "#!/usr/bin/env bash\necho 'PASS: looks good'\n"
        )
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(checker=prose_success)
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        self.assertEqual(2, rc)
        self.assertEqual("unavailable", fields["state"])

    def test_mutating_typed_state_changes_the_health_result(self) -> None:
        typed_failure = self._write_executable(
            "typed-failure",
            "#!/usr/bin/env bash\n"
            "echo '{\"schema\":1,\"state\":\"fail\",\"gate_policy_matches\":false,"
            "\"history_policy_matches\":true}'\n"
            "exit 1\n",
        )
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(checker=typed_failure)
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        self.assertEqual(1, rc)
        self.assertEqual("interrupted-transition", fields["state"])

        typed_success = self._write_executable(
            "typed-success",
            "#!/usr/bin/env bash\n"
            "echo '{\"schema\":1,\"state\":\"pass\",\"gate_policy_matches\":true,"
            "\"history_policy_matches\":true}'\n",
        )
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            rc = operational_health.merge_gate_authority_gate(checker=typed_success)
        fields = dict(line.split("=", 1) for line in output.getvalue().splitlines())
        self.assertEqual(0, rc)
        self.assertEqual("ok", fields["state"])

    def test_tick_adopts_the_global_gate_at_hourly_cadence(self) -> None:
        config = yaml.safe_load((ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text())
        gates = {gate["name"]: gate for gate in config["reminders"]}
        gate = gates["hermit_merge_gate_authority"]
        self.assertEqual(
            "./ci-hub/health/operational_health.py merge-gate-authority",
            gate["gate"]["cmd"],
        )
        self.assertEqual(gate["cadence_secs"], 3600)
        self.assertIn("GLOBAL", gate["emit"]["title"])
        self.assertLess(operational_health.MERGE_GATE_AUTHORITY_TIMEOUT_SECS, 30)


if __name__ == "__main__":
    unittest.main()
