#!/usr/bin/env python3
"""Tests for ci-hub/bin/gh-pr-merge-verified.

Network-free: `gh` and `git` are replaced by stubs on PATH, so the tests pin the
DECISION LOGIC rather than GitHub's behaviour.

The load-bearing test is test_failed_merge_is_reported_as_failure. A fix shown
only against a merge that worked proves nothing -- that case already worked. The
defect was that `gh pr merge ... | tail -1` reports rc=0 when GitHub REFUSES.
"""
import json, os, stat, subprocess, sys, tempfile, unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parent.parent / "ci-hub" / "bin" / "gh-pr-merge-verified"


def stub(d: Path, name: str, body: str):
    p = d / name
    p.write_text("#!/usr/bin/env bash\n" + body)
    p.chmod(p.stat().st_mode | stat.S_IEXEC)


class GhPrMergeVerified(unittest.TestCase):
    def setUp(self):
        self._t = tempfile.TemporaryDirectory()
        self.bin = Path(self._t.name)

    def tearDown(self):
        self._t.cleanup()

    def run_tool(self, *args):
        env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}")
        return subprocess.run([sys.executable, str(TOOL), *args],
                              capture_output=True, text=True, env=env)

    def test_failed_merge_is_reported_as_failure(self):
        """THE point: gh refusing the merge must not read as a landing."""
        stub(self.bin, "gh", 'if [ "$2" = view ]; then echo \'{"state":"OPEN","baseRefName":"main"}\'; exit 0; fi\n'
                             'echo "GraphQL: Pull Request is still a draft" >&2; exit 1\n')
        stub(self.bin, "git", "exit 0\n")
        r = self.run_tool("42", "--repo", "o/n", "--squash")
        self.assertEqual(r.returncode, 2)
        self.assertIn("did NOT land", r.stderr)

    def test_merged_state_without_reachability_is_not_landed(self):
        """`state: MERGED` is a report; the branch is the artifact."""
        stub(self.bin, "gh", 'echo \'{"state":"MERGED","baseRefName":"main","mergeCommit":{"oid":"deadbeef"}}\'\n')
        stub(self.bin, "git", 'case "$1" in fetch) exit 0;; merge-base) exit 1;; esac\nexit 0\n')
        r = self.run_tool("42", "--repo", "o/n")
        self.assertEqual(r.returncode, 3)
        self.assertIn("NOT\n  reachable", r.stderr)

    def test_merged_and_reachable_is_verified(self):
        stub(self.bin, "gh", 'echo \'{"state":"MERGED","baseRefName":"main","mergeCommit":{"oid":"cafe1234"}}\'\n')
        stub(self.bin, "git", "exit 0\n")
        r = self.run_tool("42", "--repo", "o/n")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("VERIFIED", r.stdout)

    def test_merged_state_with_no_merge_commit_is_not_landed(self):
        stub(self.bin, "gh", 'echo \'{"state":"MERGED","baseRefName":"main"}\'\n')
        stub(self.bin, "git", "exit 0\n")
        r = self.run_tool("42", "--repo", "o/n")
        self.assertEqual(r.returncode, 3)

    def test_unreadable_pull_request_is_not_success(self):
        stub(self.bin, "gh", "exit 1\n")
        stub(self.bin, "git", "exit 0\n")
        r = self.run_tool("42", "--repo", "o/n")
        self.assertEqual(r.returncode, 1)

    def test_non_numeric_argument_is_usage(self):
        stub(self.bin, "gh", "exit 0\n")
        r = self.run_tool("not-a-number")
        self.assertEqual(r.returncode, 1)


if __name__ == "__main__":
    unittest.main()
