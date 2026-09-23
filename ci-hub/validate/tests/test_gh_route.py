"""Execute the real preflight routes against local CLI/proxy opponents."""

import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gh_route
import preflight_anchor as pa


class GithubRouteTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.calls = self.root / "calls.jsonl"
        self.selected = self.root / "selected gh"
        self.selected.write_text(
            f"#!{sys.executable}\n"
            "import json,os,sys\n"
            "with open(os.environ['GH_ROUTE_CALLS'],'a') as f: "
            "f.write(json.dumps(sys.argv[1:])+'\\n')\n"
            "if os.environ.get('GH_ROUTE_ERROR'): sys.exit(23)\n"
            "print(json.dumps({'headRefOid':'a'*40,'headRefName':'topic'}) "
            "if sys.argv[1]=='pr' else 'ahead')\n"
        )
        self.selected.chmod(0o755)
        proxy = self.root / "with-proxy"
        proxy.write_text('#!/bin/sh\nexec "$@"\n')
        proxy.chmod(0o755)
        stale = self.root / "gh"
        stale.write_text('#!/bin/sh\nexit 99\n')
        stale.chmod(0o755)
        patch = mock.patch.dict(os.environ, {
            "FLEET_REAL_GH": str(self.selected), "PATH": str(self.root),
            "GH_ROUTE_CALLS": str(self.calls),
        })
        patch.start()
        self.addCleanup(patch.stop)

    def test_pr_resolution_and_compare_execute_selected_cli_not_path_gh(self):
        self.assertEqual(("a" * 40, "topic"), pa.resolve_pr_head("owner/repo", "12"))
        with mock.patch.object(pa, "_local_contains", return_value=None):
            self.assertEqual((True, "github-compare"),
                             pa.head_contains("", "owner/repo", "b" * 40, "a" * 40))
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual([
            ["pr", "view", "12", "--repo", "owner/repo", "--json", "headRefOid,headRefName"],
            ["api", f"repos/owner/repo/compare/{'b' * 40}...{'a' * 40}", "--jq", ".status"],
        ], calls)
        self.assertEqual(["with-proxy", str(self.selected), "api", "user"],
                         gh_route.command("api", "user"))

    def test_bad_selection_refuses_before_api(self):
        nonexecutable = self.root / "not-executable"
        nonexecutable.write_text("no")
        for value in (None, "", "gh", str(self.root), str(nonexecutable), str(self.root / "absent")):
            with self.subTest(value=value), mock.patch.dict(os.environ):
                if value is None:
                    os.environ.pop("FLEET_REAL_GH", None)
                else:
                    os.environ["FLEET_REAL_GH"] = value
                with mock.patch.object(pa, "_run") as run:
                    with self.assertRaises(pa.PreflightError):
                        pa.resolve_pr_head("owner/repo", "12")
                    with self.assertRaises(pa.PreflightError):
                        pa._compare_contains("owner/repo", "b" * 40, "a" * 40)
                run.assert_not_called()

    def test_proxy_absence_and_cli_error_never_fall_back(self):
        (self.root / "with-proxy").unlink()
        with self.assertRaises(FileNotFoundError):
            pa.resolve_pr_head("owner/repo", "12")
        self.assertFalse(self.calls.exists())
        proxy = self.root / "with-proxy"
        proxy.write_text('#!/bin/sh\nexec "$@"\n')
        proxy.chmod(0o755)
        with mock.patch.dict(os.environ, {"GH_ROUTE_ERROR": "1"}):
            with self.assertRaises(pa.PreflightError):
                pa._compare_contains("owner/repo", "b" * 40, "a" * 40)

    def test_reentry_restores_recorded_route_and_refuses_contradictions(self):
        record = {"github_executable": str(self.selected)}
        self.assertEqual(str(self.selected), gh_route.recorded_environment(record, {})["FLEET_REAL_GH"])
        self.assertEqual(str(self.selected), gh_route.recorded_environment(
            {}, {"FLEET_REAL_GH": str(self.selected)})["FLEET_REAL_GH"])
        for recorded, environment in ((record, {"FLEET_REAL_GH": sys.executable}),
                                      ({"github_executable": None}, {}), ({}, {})):
            with self.subTest(record=recorded, environment=environment):
                with self.assertRaises(ValueError):
                    gh_route.recorded_environment(recorded, environment)


if __name__ == "__main__":
    unittest.main()
