#!/usr/bin/env python3
"""Bracket: a --no-rebase landing must refuse a head that lacks current main.

Measured defect: https://github.com/rrnewton/reverie/pull/679 was queued with
--no-rebase at head 169f1d0b9f9cf9abb3d78c13c1e61811ae43c1ac, one landing
behind reverie main, and land-pr.sh landed it as 547278b9. Nothing on the
--no-rebase path compared the head with main.

Two layers, because each alone is insufficient:

* The PREDICATE (head-contains-main.sh) runs against REAL git repositories, so
  ancestry, the private-ref fetch, and the url.insteadOf refusal are exercised
  with real semantics rather than modelled.
* The LANDER (land-pr.sh) runs end to end against the stub programs in
  testdata/, so the refusal is proven to be wired into the real execution path,
  to fire before `gh pr merge`, and to print the message an operator needs.
  A mutant lander with the call sites deleted must LAND the behind head; if it
  does not, the scenario is not exercising the defect.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


LANDING = Path(__file__).resolve().parent
LANDER = LANDING / "land-pr.sh"
PREDICATE = LANDING / "head-contains-main.sh"
STUBS = LANDING / "testdata"

HEAD_SHA = "b" * 40
MAIN_SHA = "a" * 40


def git(repo: Path, *args: str) -> str:
    env = os.environ.copy()
    env.update(
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.invalid",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.invalid",
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_NOSYSTEM="1",
    )
    for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
        env.pop(name, None)
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    ).stdout.strip()


class ContainsMainPredicateTest(unittest.TestCase):
    """head-contains-main.sh against real repositories."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        tmp = Path(self._tmp.name)
        self.canonical = tmp / "canonical.git"
        self.checkout = tmp / "checkout"
        seed = tmp / "seed"
        git(tmp, "init", "-q", "--bare", "-b", "main", str(self.canonical))
        git(tmp, "init", "-q", "-b", "main", str(seed))
        git(seed, "commit", "-q", "--allow-empty", "-m", "base")
        self.base = git(seed, "rev-parse", "HEAD")
        git(seed, "push", "-q", str(self.canonical), "main")
        git(tmp, "clone", "-q", str(self.canonical), str(self.checkout))
        # The PR head is cut from `base`. Then main advances by one landing,
        # which is the reverie#679 shape.
        git(self.checkout, "checkout", "-q", "-b", "feature", self.base)
        git(self.checkout, "commit", "-q", "--allow-empty", "-m", "pr work")
        self.behind_head = git(self.checkout, "rev-parse", "HEAD")
        git(seed, "commit", "-q", "--allow-empty", "-m", "another landing")
        self.main = git(seed, "rev-parse", "HEAD")
        git(seed, "push", "-q", str(self.canonical), "main")
        # A head that does contain the advanced main.
        git(self.checkout, "fetch", "-q", "origin", "main")
        git(self.checkout, "checkout", "-q", "-b", "rebased", "origin/main")
        git(self.checkout, "commit", "-q", "--allow-empty", "-m", "pr work")
        self.containing_head = git(self.checkout, "rev-parse", "HEAD")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def ask(self, head: str, url: str | None = None) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["GIT_CONFIG_GLOBAL"] = "/dev/null"
        env["GIT_CONFIG_NOSYSTEM"] = "1"
        # If with-proxy is on PATH the predicate uses it; it only sets proxy
        # variables, which a filesystem URL ignores.
        return subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; lander_head_contains_canonical_main "$2" "$3" "$4"',
                "predicate",
                str(PREDICATE),
                str(self.checkout),
                url if url is not None else str(self.canonical),
                head,
            ],
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
        )

    def private_refs(self) -> str:
        return git(self.checkout, "for-each-ref", "refs/land-pr/")

    def test_head_containing_main_passes(self) -> None:
        result = self.ask(self.containing_head)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.strip(), f"CONTAINS {self.main}")
        self.assertEqual(self.private_refs(), "")

    def test_head_behind_main_is_refused(self) -> None:
        result = self.ask(self.behind_head)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.strip(), f"BEHIND {self.main}")
        self.assertEqual(self.private_refs(), "")

    def test_stale_shared_origin_main_cannot_admit_a_behind_head(self) -> None:
        # A sibling worktree's `git fetch origin` against a stale mirror
        # rewinds the SHARED refs/remotes/origin/main. The behind head contains
        # that stale value, so a check reading origin/main would pass it.
        git(self.checkout, "update-ref", "refs/remotes/origin/main", self.base)
        git(self.checkout, "merge-base", "--is-ancestor",
            "refs/remotes/origin/main", self.behind_head)
        result = self.ask(self.behind_head)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.strip(), f"BEHIND {self.main}")
        # And the predicate did not "repair" the shared ref as a side effect.
        self.assertEqual(
            git(self.checkout, "rev-parse", "refs/remotes/origin/main"), self.base
        )

    def test_insteadof_rewritten_url_is_undetermined(self) -> None:
        mirror = Path(self._tmp.name) / "stale-mirror.git"
        git(Path(self._tmp.name), "clone", "-q", "--bare", str(self.canonical), str(mirror))
        git(self.checkout, "config", f"url.{mirror}.insteadOf", str(self.canonical))
        result = self.ask(self.containing_head)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"^UNDETERMINED .* is rewritten by git url\.insteadOf")

    def test_unreachable_canonical_repository_is_undetermined(self) -> None:
        result = self.ask(self.containing_head, url=str(Path(self._tmp.name) / "absent.git"))
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"^UNDETERMINED could not fetch main from ")
        self.assertEqual(self.private_refs(), "")

    def test_head_absent_locally_is_undetermined_not_behind(self) -> None:
        result = self.ask("c" * 40)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"^UNDETERMINED head c{40} is not present")


class NoRebaseLanderTest(unittest.TestCase):
    """land-pr.sh --no-rebase end to end against stub programs."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        tmp = Path(self._tmp.name)
        self.tmp = tmp
        self.root = tmp / "root"
        landing = self.root / "ci-hub/landing"
        (self.root / "ci-hub/bin").mkdir(parents=True)
        landing.mkdir(parents=True)
        (tmp / "bin").mkdir()
        (tmp / "wt").mkdir()
        for name in ("worktree-mutation-fence.sh", "merge-gate-status.jq",
                     "head-contains-main.sh"):
            shutil.copy(LANDING / name, landing / name)
        shutil.copy(LANDER, landing / "land-pr.sh")
        for name in ("git", "gh", "python3", "jq", "with-proxy", "flock", "timeout"):
            shutil.copy(STUBS / name, tmp / "bin" / name)
        shutil.copy(STUBS / "ci-hub-stub", self.root / "ci-hub/ci-hub")
        shutil.copy(STUBS / "pr-head-ci-stub", self.root / "ci-hub/bin/pr-head-ci")
        # Control: the same lander with every call site of the predicate removed.
        source = LANDER.read_text()
        # Each call is replaced by `:` rather than deleted, so an if-body that
        # held only the call stays syntactically valid.
        mutant = re.sub(
            r'^(\s*)require_no_rebase_head_contains_main "\$.*$',
            r"\1:",
            source,
            flags=re.M,
        )
        self.assertEqual(
            source.count('require_no_rebase_head_contains_main "$'), 2,
            "expected exactly two call sites (step 2 and the final boundary)",
        )
        (landing / "land-pr-mutant.sh").write_text(mutant)
        for path in [*(tmp / "bin").iterdir(), *landing.iterdir(),
                     self.root / "ci-hub/ci-hub", self.root / "ci-hub/bin/pr-head-ci"]:
            path.chmod(0o755)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def land(self, label: str, lander: str, *, ancestor: bool,
             ancestor_seq: str = "", fetch_rc: str = "0") -> tuple[int, str]:
        scen = self.tmp / f"scen-{label}"
        scen.mkdir()
        # The head already holds an exact-head verdict: the #679 shape, where
        # validation and approval at the exact head were both satisfied.
        (scen / "jit_ok").touch()
        env = os.environ.copy()
        env.update(
            PATH=f"{self.tmp / 'bin'}{os.pathsep}{env.get('PATH', '')}",
            SCEN_DIR=str(scen),
            CALLS=str(self.tmp / f"calls-{label}.log"),
            SCEN_ORIG=HEAD_SHA,
            SCEN_HEAD=HEAD_SHA,
            SCEN_MAIN=MAIN_SHA,
            SCEN_ANCESTOR="1" if ancestor else "0",
            SCEN_ANCESTOR_SEQ=ancestor_seq,
            SCEN_CANONICAL_FETCH_RC=fetch_rc,
            SCEN_CR_CLEAR="1",
            CI_HUB_POST_LAND_AUDIT="0",
            CI_HUB_LANDING_LOG_DIR=str(self.tmp / "logs"),
        )
        (self.tmp / f"calls-{label}.log").touch()
        result = subprocess.run(
            ["bash", str(self.root / "ci-hub/landing" / lander), "999", "feature",
             "--checkout", str(self.tmp / "wt"), "--agent", "test",
             "--foreground", "--no-rebase"],
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=120,
        )
        return result.returncode, result.stdout

    def merge_attempts(self, label: str) -> int:
        calls = (self.tmp / f"calls-{label}.log").read_text()
        return len(re.findall(r"^gh pr merge ", calls, re.M))

    def test_head_containing_main_passes_both_checks_and_lands(self) -> None:
        rc, out = self.land("contains", "land-pr.sh", ancestor=True)
        passed = re.findall(
            rf"no-rebase: head {HEAD_SHA} contains current rrnewton/hermit main "
            rf"{MAIN_SHA} \(read from https://github\.com/rrnewton/hermit\)",
            out,
        )
        self.assertEqual(len(passed), 2, out)
        self.assertNotIn("REFUSING TO LAND", out)
        self.assertEqual(self.merge_attempts("contains"), 1, out)
        self.assertIn("LANDED:", out)
        self.assertEqual(rc, 0, out)
        # Main was read from the canonical URL, not from the shared origin/main.
        calls = (self.tmp / "calls-contains.log").read_text()
        self.assertRegex(
            calls,
            r"with-proxy git -C \S+ fetch -q --no-tags --no-write-fetch-head "
            r"--no-recurse-submodules https://github\.com/rrnewton/hermit "
            r"\+refs/heads/main:refs/land-pr/contains-main/",
        )

    def test_head_behind_main_is_refused_before_merge(self) -> None:
        rc, out = self.land("behind", "land-pr.sh", ancestor=False)
        self.assertIn(
            f"ABANDON: REFUSING TO LAND with --no-rebase: head {HEAD_SHA} does not "
            f"contain current rrnewton/hermit main {MAIN_SHA}. Its validation and "
            "approvals describe a tree that is not what would land. Rebase feature "
            f"onto main {MAIN_SHA}, re-validate the new head, and re-queue it.",
            out,
        )
        self.assertEqual(rc, 3, out)
        self.assertEqual(self.merge_attempts("behind"), 0, out)
        self.assertNotIn("LANDED:", out)
        self.assertNotIn("contains current", out)

    def test_main_advancing_after_step_two_is_refused_at_the_final_boundary(self) -> None:
        # The head contains main when the sequence starts; main then advances
        # before the final receipt boundary. The step-2 answer must not carry.
        rc, out = self.land("advanced", "land-pr.sh", ancestor=True, ancestor_seq="1 0")
        self.assertEqual(
            len(re.findall(rf"no-rebase: head {HEAD_SHA} contains current", out)), 1, out
        )
        self.assertIn(
            f"ABANDON: REFUSING TO LAND with --no-rebase: head {HEAD_SHA} does not "
            f"contain current rrnewton/hermit main {MAIN_SHA}.",
            out,
        )
        self.assertIn("re-validate the new head", out)
        self.assertEqual(rc, 5, out)
        self.assertEqual(self.merge_attempts("advanced"), 0, out)
        self.assertNotIn("review applicability", out)

    def test_undetermined_ancestry_is_refused_and_says_so(self) -> None:
        # A head that WOULD pass (ancestor=True) is still refused when main
        # cannot be read, and the message must not advise a rebase.
        rc, out = self.land("nofetch", "land-pr.sh", ancestor=True, fetch_rc="128")
        self.assertIn(
            f"ABANDON: REFUSING TO LAND with --no-rebase: could not determine whether "
            f"head {HEAD_SHA} contains current rrnewton/hermit main (rc=2). Nothing was "
            "compared, so this is not a statement that the head needs rebasing. "
            "Output was: UNDETERMINED could not fetch main from "
            "https://github.com/rrnewton/hermit (rc=128)",
            out,
        )
        self.assertEqual(rc, 3, out)
        self.assertEqual(self.merge_attempts("nofetch"), 0, out)

    def test_lander_refuses_to_start_without_the_predicate(self) -> None:
        (self.root / "ci-hub/landing/head-contains-main.sh").unlink()
        rc, out = self.land("nohelper", "land-pr.sh", ancestor=True)
        self.assertEqual(rc, 2, out)
        self.assertIn("land-pr: cannot load", out)
        self.assertEqual(self.merge_attempts("nohelper"), 0, out)

    def test_control_mutant_without_the_check_issues_the_merge(self) -> None:
        _rc, out = self.land("mutant", "land-pr-mutant.sh", ancestor=False)
        self.assertNotIn("REFUSING TO LAND", out)
        # The defect is the merge command being ISSUED for a behind head. (The
        # stub's single ancestry answer also fails the post-merge proof, so the
        # mutant's exit status says nothing here and is not asserted.)
        self.assertIn("changes-requested gate: clear", out)
        self.assertEqual(self.merge_attempts("mutant"), 1, out)


if __name__ == "__main__":
    unittest.main()
