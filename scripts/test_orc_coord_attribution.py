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
GH_BOUNDARY = Path(__file__).resolve().parent.parent / "ci-hub" / "bin" / "gh"
COMMENT_IDENTITY = ("--who", "codex-boundary", "--team", "hermit2", "--role", "coordinator")


def run(script: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        (str(script), *args), text=True, capture_output=True, check=False
    )


class CommentPrefixTests(unittest.TestCase):
    """The coordinator wrapper delegates all prefix formatting to ci-hub/bin/gh."""

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

    def test_body_is_forwarded_without_a_competing_legacy_prefix(self) -> None:
        r = self.comment("Rebased and revalidated at the new head.")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("Rebased and revalidated at the new head.", r.stdout)
        self.assertNotIn("[orc-coord-014]", r.stdout)

    def test_multiline_body_passes_through_unchanged(self) -> None:
        body = "head\n\nbody"
        r = self.comment(body)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(body, r.stdout)

    def test_identity_arguments_are_forwarded_to_the_boundary(self) -> None:
        r = self.comment("Surfaced from the TaskGraph.")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("who=codex-boundary", r.stdout)
        self.assertIn("team=hermit2", r.stdout)
        self.assertIn("role=coordinator", r.stdout)

    def test_relay_enum_value_is_forwarded(self) -> None:
        r = run(
            COMMENT, "--who", "codex-boundary", "--team", "hermit2",
            "--role", "relay", "--repo", "rrnewton/hermit", "1844",
            "--body", "relayed verdict", "--check-only",
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("role=relay", r.stdout)

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
    """The executable boundary carries the final five-field contract."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.text = GH_BOUNDARY.read_text()

    def test_boundary_states_five_field_newline_format(self) -> None:
        self.assertIn("[team-slug, agent-name, resolved-model, machine-shortname, role=enum]", self.text)
        self.assertIn('return f"{disclosure.prefix}\\n{body}"', self.text)

    def test_boundary_has_exact_closed_role_enum(self) -> None:
        self.assertIn(
            'ROLES = frozenset({"impl", "debugger", "reviewer", "relay", "coordinator", "human"})',
            self.text,
        )

    def test_boundary_does_not_accept_a_model_argument(self) -> None:
        self.assertIn("--model is process-resolved", self.text)

    def test_wrappers_are_executable(self) -> None:
        for script in (COMMENT, PR_CREATE):
            self.assertTrue(script.is_file(), script)
            self.assertTrue(script.stat().st_mode & 0o111, f"{script} not executable")


if __name__ == "__main__":
    unittest.main()
