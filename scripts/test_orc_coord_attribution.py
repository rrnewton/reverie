#!/usr/bin/env python3
"""Tests for the orc-coord-014 team-attribution wrappers.

INERT BY CONSTRUCTION. Every case runs the wrappers with `--check-only`, which
does all the normalisation and refusal work and then exits before touching the
network. No comment is posted, no PR is opened, no label is created, and `gh` is
never invoked -- so running this suite cannot attribute anything to anyone.

Bracketed both ways throughout: for every "X is enforced" there is a case
proving the wrapper accepts the compliant form, because a wrapper that refuses
everything would pass a refusal-only suite.
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import unittest


PLUGIN = Path(__file__).resolve().parent.parent / ".orc" / "plugins" / "hermit-dev"
COMMENT = PLUGIN / "gh-coord-comment"
PR_CREATE = PLUGIN / "gh-coord-pr-create"
AGENTS_MD = Path(__file__).resolve().parent.parent / "AGENTS.md"
TEAM_TAG = "[orc-coord-014]"
COMMENT_IDENTITY = ("--who", "codex-boundary", "--team", "hermit2", "--role", "coordinator")


def run(script: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        (str(script), *args), text=True, capture_output=True, check=False
    )


class CommentPrefixTests(unittest.TestCase):
    """The first line must carry [orc-coord-014]; three distinct outcomes."""

    def comment(self, body: str, repo: str = "rrnewton/hermit"):
        return run(
            COMMENT, *COMMENT_IDENTITY, "--repo", repo, "1844", "--body", body,
            "--check-only",
        )

    def test_missing_who_is_refused(self) -> None:
        r = run(
            COMMENT, "--team", "hermit2", "--role", "coordinator",
            "--repo", "rrnewton/hermit", "1844", "--body", "anonymous",
            "--check-only",
        )
        self.assertEqual(r.returncode, 2)
        self.assertIn("missing required --who", r.stderr)

    def test_missing_prefix_is_normalised_not_refused(self) -> None:
        # The comment is wanted; only the attribution is missing.
        r = self.comment("Rebased and revalidated at the new head.")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(TEAM_TAG + " Rebased and revalidated", r.stdout)

    def test_correct_prefix_passes_through_unchanged(self) -> None:
        body = f"{TEAM_TAG} already attributed."
        r = self.comment(body)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(body, r.stdout)
        # Not double-tagged.
        self.assertEqual(r.stdout.count(TEAM_TAG), 1)

    def test_role_tag_may_precede_the_team_tag(self) -> None:
        # The role tag says what kind of agent; the team tag says which team.
        body = f"[coordinator, opus-5] {TEAM_TAG} Surfaced from the TaskGraph."
        r = self.comment(body)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(body, r.stdout)
        self.assertEqual(r.stdout.count(TEAM_TAG), 1)

    def test_a_different_team_tag_is_refused_not_retagged(self) -> None:
        # Silently retagging would forge attribution -- the exact failure the
        # wrapper exists to prevent.
        r = self.comment("[orc-coord-021] another team's comment")
        self.assertEqual(r.returncode, 2)
        self.assertIn("different team tag", r.stderr)
        self.assertNotIn(TEAM_TAG, r.stdout)

    def test_prefix_is_only_required_on_the_first_line(self) -> None:
        r = self.comment(f"{TEAM_TAG} head\n\nbody mentioning nothing special")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.count(TEAM_TAG), 1)

    def test_empty_comment_is_refused(self) -> None:
        r = self.comment("   \n  ")
        self.assertEqual(r.returncode, 2)
        self.assertIn("empty comment", r.stderr)


class DestinationRestrictionTests(unittest.TestCase):
    """rrnewton only; upstream refused, and refused rather than redirected."""

    def test_upstream_comment_is_refused(self) -> None:
        r = run(COMMENT, *COMMENT_IDENTITY, "--repo", "facebookexperimental/hermit", "1",
                "--body", "x", "--check-only")
        self.assertEqual(r.returncode, 2)
        self.assertIn("internal task tracker", r.stderr)
        # A redirect would post to a DIFFERENT thread, so it must not claim one.
        self.assertIn("NOT a redirect", r.stderr)

    def test_unrelated_repo_is_refused(self) -> None:
        r = run(
            COMMENT, *COMMENT_IDENTITY, "--repo", "someone/else", "1",
            "--body", "x", "--check-only",
        )
        self.assertEqual(r.returncode, 2)
        self.assertIn("restricted to rrnewton", r.stderr)

    def test_rrnewton_is_accepted(self) -> None:
        for repo in ("rrnewton/hermit", "rrnewton/reverie", "rrnewton/dev-hermit"):
            r = run(
                COMMENT, *COMMENT_IDENTITY, "--repo", repo, "1", "--body", "x",
                "--check-only",
            )
            self.assertEqual(r.returncode, 0, f"{repo}: {r.stderr}")

    def test_upstream_pr_creation_is_refused(self) -> None:
        r = run(PR_CREATE, "--repo", "facebookexperimental/reverie",
                "--title", "t", "--check-only")
        self.assertEqual(r.returncode, 2)
        self.assertIn("internal task tracker", r.stderr)


class PrLabelTests(unittest.TestCase):
    """Both labels, unconditionally, without discarding the caller's own."""

    def test_both_team_labels_are_applied(self) -> None:
        r = run(PR_CREATE, "--repo", "rrnewton/hermit", "--title", "t",
                "--body", "b", "--check-only")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--label orc-coord ", r.stdout + " ")
        self.assertIn("orc-coord-014", r.stdout)

    def test_caller_labels_are_preserved_not_replaced(self) -> None:
        r = run(PR_CREATE, "--repo", "rrnewton/hermit", "--title", "t",
                "--label", "mechanism:fixture-oracle", "--check-only")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("mechanism:fixture-oracle", r.stdout)
        self.assertIn("orc-coord-014", r.stdout)
        self.assertIn("orc-coord", r.stdout)

    def test_neither_label_implies_the_other(self) -> None:
        # Both must appear as separate --label arguments.
        r = run(PR_CREATE, "--repo", "rrnewton/hermit", "--title", "t", "--check-only")
        self.assertEqual(r.stdout.count("--label"), 2, r.stdout)


class PolicyTextTests(unittest.TestCase):
    """The plugin tests follow the canonical, team-agnostic attribution policy."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.text = AGENTS_MD.read_text()

    def test_agents_md_states_role_and_full_team_requirements(self) -> None:
        self.assertIn("Role + team tag", self.text)
        self.assertIn("[<role>, MODEL] [<team-slug>, <machine>]", self.text)
        self.assertIn("exact final machine token matching `devbigNNN`", self.text)

    def test_agents_md_requires_the_tag_on_all_three_surfaces(self) -> None:
        self.assertIn("every commit's final line", self.text)
        self.assertIn("PR-description opening", self.text)
        self.assertIn("cross-team comment prefix", self.text)

    def test_wrappers_are_executable(self) -> None:
        for script in (COMMENT, PR_CREATE):
            self.assertTrue(script.is_file(), script)
            self.assertTrue(script.stat().st_mode & 0o111, f"{script} not executable")


if __name__ == "__main__":
    unittest.main()
