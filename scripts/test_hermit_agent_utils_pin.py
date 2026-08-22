#!/usr/bin/env python3
"""Tests for the hermit-side agent-utils pin currency gate.

Every case builds a throwaway parent plus hermit and agent-utils repos, so
nothing here touches the real workspace or the network. The remote is a local
bare repo, which exercises the same `git ls-remote origin refs/heads/main` and
`git fetch origin refs/heads/main` paths without leaving the machine.

BRACKETED BOTH WAYS THROUGHOUT, and that is the half that matters. Against the
live workspace this gate is RED (212 commits behind), so "it printed ERROR" is
not evidence that it works -- a gate that fails on everything would pass a
failures-only suite, and would be ignored within a week. Each failing case here
has a matching passing case, and the budget boundary is checked on both sides.
"""

from __future__ import annotations

import contextlib
from io import StringIO
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

from scripts import check_hermit_agent_utils_pin as gate


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ("git", "-C", str(repo), *args), text=True, capture_output=True, check=True
    ).stdout.strip()


def gitlink(repo: Path, path: str, sha: str) -> None:
    """Record `path` as a gitlink at `sha` without needing a checkout there."""
    git(repo, "update-index", "--add", "--cacheinfo", f"160000,{sha},{path}")


class PinFixture(unittest.TestCase):
    """A miniature parent: a hermit repo recording an agent-utils gitlink."""

    def setUp(self) -> None:
        # Local remotes must not be dialled through the egress proxy.
        self._env = dict(os.environ)
        os.environ["PIN_INVARIANT_DISABLE_PROXY"] = "1"
        self.temp = tempfile.TemporaryDirectory()
        base = Path(self.temp.name)
        self.root = base / "parent"
        self.root.mkdir()

        self.remote = base / "agent-utils.git"
        subprocess.run(("git", "init", "--bare", "--initial-branch=main", str(self.remote)),
                       check=True, capture_output=True)
        self.agent_utils = self.root / "agent-utils"
        self._init(self.agent_utils)
        git(self.agent_utils, "remote", "add", "origin", str(self.remote))
        git(self.agent_utils, "push", "-q", "origin", "main")

        self.hermit = self.root / "hermit"
        self._init(self.hermit)

        self._init(self.root)
        self.record_pin(git(self.agent_utils, "rev-parse", "HEAD"))

    def tearDown(self) -> None:
        os.environ.clear()
        os.environ.update(self._env)
        self.temp.cleanup()

    def _init(self, repo: Path) -> None:
        subprocess.run(("git", "init", "--initial-branch=main", str(repo)),
                       check=True, capture_output=True)
        git(repo, "config", "user.email", "t@e.c")
        git(repo, "config", "user.name", "T")
        (repo / "README").write_text("x\n")
        git(repo, "add", "README")
        git(repo, "commit", "-q", "-m", "initial")

    def record_pin(self, agent_utils_sha: str) -> None:
        """Point hermit at `agent_utils_sha`, and the parent at that hermit commit."""
        gitlink(self.hermit, "agent-utils", agent_utils_sha)
        git(self.hermit, "commit", "-q", "-m", f"pin agent-utils {agent_utils_sha[:8]}")
        gitlink(self.root, "hermit", git(self.hermit, "rev-parse", "HEAD"))
        git(self.root, "commit", "-q", "-m", "record hermit gitlink")

    def advance_agent_utils(self, count: int) -> str:
        for n in range(count):
            (self.agent_utils / "README").write_text(f"step {n}\n")
            git(self.agent_utils, "add", "README")
            git(self.agent_utils, "commit", "-q", "-m", f"step {n}")
        git(self.agent_utils, "push", "-q", "origin", "main")
        return git(self.agent_utils, "rev-parse", "HEAD")

    def measure(self, **kwargs):
        return gate.measure(self.root, **kwargs)

    def run_cli(self, *argv: str) -> tuple[int, str]:
        # The gate writes its failure line to stderr on purpose. Swallow it here
        # so a deliberate-failure case does not print an ERROR into the suite
        # output, where a reader would take it for a real one.
        out, err = StringIO(), StringIO()
        with contextlib.redirect_stderr(err):
            code = gate.main(["--root", str(self.root), *argv], out=out)
        return code, out.getvalue() + err.getvalue()


class PinIsCurrentTests(PinFixture):
    """POSITIVE CONTROLS -- without these, a gate that always fails looks correct."""

    def test_pin_equal_to_main_is_current_and_passes(self) -> None:
        result = self.measure()
        self.assertEqual(result["state"], "current", result)
        self.assertEqual(result["lag"], 0, result)
        self.assertEqual(gate.verdict(result), gate.CURRENT, result)

    def test_pin_behind_but_inside_the_budget_passes(self) -> None:
        self.advance_agent_utils(5)
        result = self.measure(max_lag=30)
        self.assertEqual(result["state"], "lagging", result)
        self.assertEqual(result["lag"], 5, result)
        self.assertEqual(gate.verdict(result), gate.CURRENT, result)

    def test_lag_exactly_at_the_budget_passes(self) -> None:
        """The boundary is inclusive; assert the side that must stay green."""
        self.advance_agent_utils(4)
        result = self.measure(max_lag=4)
        self.assertEqual(result["lag"], 4, result)
        self.assertEqual(gate.verdict(result), gate.CURRENT, result)

    def test_cli_exits_zero_and_reports_the_lag_while_passing(self) -> None:
        self.advance_agent_utils(3)
        code, text = self.run_cli("--max-lag", "30")
        self.assertEqual(code, 0, text)
        self.assertIn("lag=3", text)
        self.assertIn("within the 30-commit budget", text)

    def test_offline_skips_without_touching_anything(self) -> None:
        code, text = self.run_cli("--offline")
        self.assertEqual(code, 0, text)
        self.assertIn("[skip] --offline", text)


class PinIsStaleTests(PinFixture):
    """NEGATIVE CONTROLS -- the condition the gate exists to catch."""

    def test_lag_one_past_the_budget_fails(self) -> None:
        """One commit either side of the boundary, so the budget is what decides."""
        self.advance_agent_utils(5)
        self.assertEqual(gate.verdict(self.measure(max_lag=5)), gate.CURRENT)
        self.assertEqual(gate.verdict(self.measure(max_lag=4)), gate.OVER_BUDGET)

    def test_failure_message_names_the_pin_the_main_and_the_gap(self) -> None:
        pin = git(self.agent_utils, "rev-parse", "HEAD")
        main = self.advance_agent_utils(40)
        code, text = self.run_cli("--max-lag", "30")
        self.assertEqual(code, 1, text)
        self.assertIn(pin[:12], text)
        self.assertIn(main[:12], text)
        self.assertIn("40 commit(s) behind", text)
        self.assertIn("30-commit budget", text)

    def test_a_rewritten_history_is_a_state_and_never_a_lag_number(self) -> None:
        """A count over a non-ancestor pin would invite a bump that is not an advance."""
        git(self.agent_utils, "checkout", "-q", "-b", "side", "HEAD~0")
        (self.agent_utils / "README").write_text("side\n")
        git(self.agent_utils, "add", "README")
        git(self.agent_utils, "commit", "-q", "-m", "side")
        side = git(self.agent_utils, "rev-parse", "HEAD")
        git(self.agent_utils, "checkout", "-q", "main")
        self.advance_agent_utils(2)
        self.record_pin(side)

        result = self.measure()
        self.assertEqual(result["state"], "non-ancestor", result)
        self.assertNotIn("lag", result)
        self.assertEqual(gate.verdict(result), gate.OVER_BUDGET, result)
        self.assertIn("Do NOT bump to main", gate.render(result))


class UnmeasurableFailsClosedTests(PinFixture):
    """`I could not measure the lag` must never read as `the lag is zero`."""

    def test_uninitialised_agent_utils_is_unknown_not_zero(self) -> None:
        """An empty submodule dir makes git answer about the PARENT repo."""
        for child in sorted(self.agent_utils.rglob("*"), reverse=True):
            child.unlink() if child.is_file() or child.is_symlink() else child.rmdir()
        result = self.measure()
        self.assertEqual(result["state"], "unknown", result)
        self.assertNotIn("lag", result)
        self.assertEqual(gate.verdict(result), gate.UNKNOWN, result)

    def test_hermit_commit_absent_from_the_checkout_is_unknown(self) -> None:
        # A well-formed sha no repo has -- the shape of the live devbig030 report
        # of a gitlink pointing at an object no remote carries. Not the all-zero
        # sha, which git reserves and refuses to record.
        gitlink(self.root, "hermit", "dead" * 10)
        git(self.root, "commit", "-q", "-m", "record an unreachable hermit gitlink")
        result = self.measure()
        self.assertEqual(result["state"], "unknown", result)
        self.assertEqual(gate.verdict(result), gate.UNKNOWN, result)

    def test_hermit_recording_no_agent_utils_gitlink_is_unknown(self) -> None:
        git(self.hermit, "rm", "-q", "--cached", "agent-utils")
        git(self.hermit, "commit", "-q", "-m", "drop the agent-utils gitlink")
        gitlink(self.root, "hermit", git(self.hermit, "rev-parse", "HEAD"))
        git(self.root, "commit", "-q", "-m", "record it")
        result = self.measure()
        self.assertEqual(result["state"], "unknown", result)
        self.assertIn("records no agent-utils gitlink", result["reason"])
        self.assertEqual(gate.verdict(result), gate.UNKNOWN, result)


class BudgetProvenanceTests(unittest.TestCase):
    """The threshold was measured; keep it from being quietly padded later."""

    SMALLEST_OBSERVED_BREAKING_INTERVAL = 38

    def test_default_budget_stays_under_the_measured_breaking_interval(self) -> None:
        self.assertLessEqual(
            gate.DEFAULT_MAX_LAG,
            self.SMALLEST_OBSERVED_BREAKING_INTERVAL,
            "The default budget was measured against the smallest observed gap between "
            "agent-utils changes that broke hermit (38 commits, from the four episodes at "
            "positions 9, 67, 158 and 196 of the 212-commit advance in hermit PR 2362). "
            "Raising it past that gap lets a SECOND breaking change land before anyone is "
            "told, which is the failure this gate exists to prevent. If the measurement has "
            "changed, redo both halves recorded in the module docstring and update this "
            "bound with the new numbers -- do not simply raise the budget.",
        )

    def test_the_docstring_records_how_the_number_was_derived(self) -> None:
        """A threshold whose reasoning is not written down gets assumed to be padding."""
        doc = gate.__doc__ or ""
        self.assertIn("WHY THE DEFAULT IS 30 COMMITS", doc)
        self.assertIn("38", doc)


if __name__ == "__main__":
    unittest.main()
