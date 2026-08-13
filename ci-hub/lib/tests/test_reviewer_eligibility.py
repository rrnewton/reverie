#!/usr/bin/env python3
"""Bracket the dispatch precheck in BOTH directions, on real git history.

Fixtures are a throwaway git repo and a synthetic /proc tree.  Nothing here
touches a real repository, pull request, label, approval or process.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

BIN = Path(__file__).resolve().parent.parent.parent / "bin" / "reviewer-eligibility"
_spec = importlib.util.spec_from_loader(
    "reviewer_eligibility",
    importlib.machinery.SourceFileLoader("reviewer_eligibility", str(BIN)),
)
assert _spec and _spec.loader
re_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(re_mod)


TAG = "[hermit2, {agent}, unresolved, devbig030, role=impl]"


def _git(repo: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=True
    )
    return done.stdout.strip()


def make_repo(subjects: list[tuple[str, str]]) -> tuple[Path, str, str]:
    """Build a repo whose commits carry ``subjects`` as (human_name, subject).

    Returns (repo, base, head).
    """
    tmp = Path(tempfile.mkdtemp(prefix="revelig-"))
    _git_init = subprocess.run(
        ["git", "init", "-q", "-b", "main", str(tmp)], capture_output=True, check=True
    )
    del _git_init
    _git(tmp, "config", "user.email", "t@x")
    _git(tmp, "config", "user.name", "Base Author")
    (tmp / "f").write_text("base\n")
    _git(tmp, "add", "f")
    _git(tmp, "-c", "core.hooksPath=/dev/null", "commit", "-qm", "base commit")
    base = _git(tmp, "rev-parse", "HEAD")
    for index, (human, subject) in enumerate(subjects):
        (tmp / "f").write_text(f"change {index}\n")
        _git(tmp, "add", "f")
        _git(
            tmp,
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            f"user.name={human}",
            "commit",
            "-qm",
            subject,
        )
    return tmp, base, _git(tmp, "rev-parse", "HEAD")


def make_proc(agents: dict[str, list[tuple[str, ...]]]) -> Path:
    """Synthesize a /proc tree: agent name -> list of argv tuples."""
    root = Path(tempfile.mkdtemp(prefix="revelig-proc-"))
    pid = 1000
    for agent, argvs in agents.items():
        for argv in argvs:
            pid += 1
            entry = root / str(pid)
            entry.mkdir()
            (entry / "cmdline").write_bytes(
                b"\0".join(part.encode() for part in argv) + b"\0"
            )
            (entry / "environ").write_bytes(
                b"PATH=/usr/bin\0DG_AGENT_NAME=" + agent.encode() + b"\0"
            )
    # A non-numeric entry must be skipped, as in the real /proc.
    (root / "self").mkdir()
    return root


CLAUDE = ("/usr/local/bin/claude_code/claude", "--permission-mode", "acceptEdits")
CODEX = ("codex", "--sandbox", "workspace-write")


class Bracket(unittest.TestCase):
    """The two directions that matter, on the real #2216 shape."""

    def setUp(self) -> None:
        # Reproduces PR #2216: two commits, BOTH tagged hermit-903, but the
        # human %an differs between them -- one is the owner's git identity.
        self.repo, self.base, self.head = make_repo(
            [
                ("hermit-903", TAG.format(agent="hermit-903") + " Bind the gate"),
                ("Ryan Newton", TAG.format(agent="hermit-903") + " Honour masks"),
            ]
        )
        self.proc = make_proc(
            {
                "hermit-903": [CLAUDE],
                "hermit-bpf-research": [CLAUDE],
                "hermit-907": [CODEX, CODEX],
            }
        )

    def run_tool(self, candidate: str, lane: str) -> int:
        return re_mod.main(
            [
                "--candidate",
                candidate,
                "--lane",
                lane,
                "--base",
                self.base,
                "--head",
                self.head,
                "--repo",
                str(self.repo),
                "--proc-root",
                str(self.proc),
            ]
        )

    def test_REFUSES_the_author_for_its_own_lane(self) -> None:
        """Direction 1. hermit-903 wrote both commits."""
        self.assertEqual(self.run_tool("hermit-903", "claude"), re_mod.REFUSED)

    def test_ACCEPTS_an_uninvolved_agent_of_the_right_family(self) -> None:
        """Direction 2. Without this the rule could refuse everyone."""
        self.assertEqual(
            self.run_tool("hermit-bpf-research", "claude"), re_mod.ELIGIBLE
        )

    def test_REFUSES_the_wrong_family_even_though_it_wrote_nothing(self) -> None:
        """hermit-907 authored none of it and still cannot fill a claude lane."""
        self.assertEqual(self.run_tool("hermit-907", "claude"), re_mod.REFUSED)

    def test_the_same_wrong_family_agent_IS_eligible_for_its_own_lane(self) -> None:
        """The family test must discriminate, not just refuse."""
        self.assertEqual(self.run_tool("hermit-907", "codex"), re_mod.ELIGIBLE)

    def test_the_lane_is_not_left_unfillable(self) -> None:
        """The scarcity guard.

        With exactly two claude-family agents, a rule that disqualified both
        would be worse than the defect.  The author is refused and the other
        claude agent is accepted, so the lane still has an occupant.
        """
        self.assertEqual(self.run_tool("hermit-903", "claude"), re_mod.REFUSED)
        self.assertEqual(
            self.run_tool("hermit-bpf-research", "claude"), re_mod.ELIGIBLE
        )


class AttributionSubtlety(unittest.TestCase):
    """The %an trap, stated as an executable test."""

    def test_identical_human_author_on_every_commit_still_resolves_agents(self) -> None:
        repo, base, head = make_repo(
            [
                ("Ryan Newton", TAG.format(agent="hermit-903") + " one"),
                ("Ryan Newton", TAG.format(agent="hermit-906") + " two"),
            ]
        )
        # %an is the SAME on both, so a check keyed on it sees one author.
        names = set(
            subprocess.run(
                ["git", "-C", str(repo), "log", "--format=%an", f"{base}..{head}"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.split("\n")
        ) - {""}
        self.assertEqual(names, {"Ryan Newton"})
        # The tag distinguishes them, which is why the tool reads the subject.
        found = re_mod.commit_authors(repo, base, head)
        self.assertEqual(found.authors, frozenset({"hermit-903", "hermit-906"}))
        self.assertEqual(found.unattributed, ())


class CannotDetermine(unittest.TestCase):
    """A precheck that cannot see the answer must never return ELIGIBLE."""

    def setUp(self) -> None:
        self.proc = make_proc({"hermit-bpf-research": [CLAUDE]})

    def _run(self, repo: Path, base: str, head: str, candidate: str) -> int:
        return re_mod.main(
            [
                "--candidate", candidate, "--lane", "claude",
                "--base", base, "--head", head,
                "--repo", str(repo), "--proc-root", str(self.proc),
            ]
        )

    def test_untagged_commit_yields_NOT_DETERMINED_not_eligible(self) -> None:
        repo, base, head = make_repo(
            [
                ("Ryan Newton", TAG.format(agent="hermit-906") + " tagged"),
                ("Ryan Newton", "an untagged commit subject"),
            ]
        )
        self.assertEqual(
            self._run(repo, base, head, "hermit-bpf-research"), re_mod.NOT_DETERMINED
        )

    def test_a_definite_authorship_refusal_beats_an_incomplete_author_set(self) -> None:
        """Ordering check.

        The candidate is visibly an author, so the untagged commit alongside
        cannot make the answer less certain -- REFUSED must win over 75.
        """
        repo, base, head = make_repo(
            [
                ("Ryan Newton", TAG.format(agent="hermit-903") + " tagged"),
                ("Ryan Newton", "an untagged commit subject"),
            ]
        )
        self.assertEqual(self._run(repo, base, head, "hermit-903"), re_mod.REFUSED)

    def test_agent_with_no_live_process_yields_NOT_DETERMINED(self) -> None:
        repo, base, head = make_repo(
            [("Ryan Newton", TAG.format(agent="hermit-906") + " tagged")]
        )
        self.assertEqual(
            self._run(repo, base, head, "an-agent-that-is-not-running"),
            re_mod.NOT_DETERMINED,
        )

    def test_conflicting_families_yield_NOT_DETERMINED_not_a_pick(self) -> None:
        repo, base, head = make_repo(
            [("Ryan Newton", TAG.format(agent="hermit-906") + " tagged")]
        )
        proc = make_proc({"two-faced": [CLAUDE, CODEX]})
        self.assertEqual(
            re_mod.main(
                [
                    "--candidate", "two-faced", "--lane", "claude",
                    "--base", base, "--head", head,
                    "--repo", str(repo), "--proc-root", str(proc),
                ]
            ),
            re_mod.NOT_DETERMINED,
        )

    def test_bad_revision_range_yields_NOT_DETERMINED(self) -> None:
        repo, base, head = make_repo(
            [("Ryan Newton", TAG.format(agent="hermit-906") + " tagged")]
        )
        del base, head
        self.assertEqual(
            self._run(repo, "0" * 40, "1" * 40, "hermit-bpf-research"),
            re_mod.NOT_DETERMINED,
        )


if __name__ == "__main__":
    unittest.main()
