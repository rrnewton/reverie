#!/usr/bin/env python3
"""Tests for debug/multisect using an isolated Git repo and fake box runner."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("multisect")
LOADER = importlib.machinery.SourceFileLoader("debug_multisect", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
multisect = importlib.util.module_from_spec(SPEC)
sys.modules[LOADER.name] = multisect
LOADER.exec_module(multisect)


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


class MultisectTests(unittest.TestCase):
    def make_repo(self, root: Path, count: int = 7) -> tuple[Path, list[str]]:
        repo = root / "repo"
        repo.mkdir()
        subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
        git(repo, "config", "user.email", "multisect@example.com")
        git(repo, "config", "user.name", "Multisect Test")
        commits: list[str] = []
        for index in range(count):
            (repo / "value").write_text(f"{index}\n")
            subprocess.run(["git", "-C", str(repo), "add", "value"], check=True)
            subprocess.run(
                ["git", "-C", str(repo), "commit", "-q", "-m", f"c{index}"],
                check=True,
            )
            commits.append(git(repo, "rev-parse", "HEAD"))
        return repo, commits

    def make_fake_runner(self, root: Path) -> Path:
        runner = root / "fake-box-runner"
        runner.write_text(
            """#!/bin/sh
set -u
[ "$1" = box ] || exit 90
shift
label=unknown
while [ "$#" -gt 0 ]; do
  case "$1" in
    --label) label=$2; shift 2 ;;
    --mem|--timeout|--cores|--perf-dir) shift 2 ;;
    --) shift; break ;;
    *) exit 91 ;;
  esac
done
"$@"
rc=$?
if [ "$rc" -eq 0 ]; then class=PASS; else class=FAIL; fi
echo "VERDICT label=$label class=$class exit=$rc wall_s=0.001 detail=fake"
exit "$rc"
"""
        )
        runner.chmod(0o755)
        return runner

    def test_select_indices_evenly_spaces_interior_probes(self) -> None:
        self.assertEqual(multisect.select_indices(0, 10, 3), [0, 2, 5, 7, 10])
        self.assertEqual(multisect.select_indices(3, 5, 8), [3, 4, 5])
        self.assertEqual(multisect.select_indices(3, 4, 2), [3, 4])

    def test_resolve_history_includes_both_endpoints(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo, commits = self.make_repo(Path(temporary), count=5)
            history = multisect.resolve_history(repo, commits[0], commits[-1], "first-parent")
            self.assertEqual(history, commits)
            with self.assertRaisesRegex(multisect.MultisectError, "different commits"):
                multisect.resolve_history(repo, commits[0], commits[0], "first-parent")

    def test_first_parent_mode_rejects_side_branch_good(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo, _ = self.make_repo(Path(temporary), count=1)
            main_branch = git(repo, "symbolic-ref", "--short", "HEAD")
            git(repo, "switch", "-q", "-c", "side")
            (repo / "side").write_text("side\n")
            git(repo, "add", "side")
            git(repo, "commit", "-q", "-m", "side")
            side = git(repo, "rev-parse", "HEAD")

            git(repo, "switch", "-q", main_branch)
            (repo / "main").write_text("main\n")
            git(repo, "add", "main")
            git(repo, "commit", "-q", "-m", "main")
            git(repo, "merge", "-q", "--no-ff", "-m", "merge side", "side")
            merged = git(repo, "rev-parse", "HEAD")

            with self.assertRaisesRegex(multisect.MultisectError, "first-parent chain"):
                multisect.resolve_history(repo, side, merged, "first-parent")

    def test_choose_step_requires_high_to_low_thresholds(self) -> None:
        config = multisect.Config(
            repo=Path("."),
            good="a",
            bad="c",
            k=2,
            reps=2,
            jobs=2,
            min_step=0.5,
            good_floor=0.67,
            bad_ceiling=0.33,
            max_rounds=3,
            history_mode="first-parent",
            runner=Path("runner"),
            mem="1G",
            timeout=1,
            cores=1,
            output_dir=Path("out"),
            command=("true",),
        )

        def sample(index: int, green: int) -> object:
            results = tuple(
                multisect.RepResult(
                    1,
                    index,
                    chr(97 + index),
                    rep,
                    "PASS" if rep <= green else "FAIL",
                    rep <= green,
                    0,
                    0.0,
                    "x",
                    "o",
                    "e",
                )
                for rep in range(1, 3)
            )
            return multisect.CommitSample(index, chr(97 + index), results)

        self.assertEqual(
            multisect.choose_step([sample(0, 2), sample(1, 2), sample(2, 0)], config),
            (1, 2, 1.0),
        )
        with self.assertRaises(multisect.AmbiguousStep):
            multisect.choose_step([sample(0, 2), sample(1, 1), sample(2, 0)], config)

    def test_box_verdict_is_stdout_only_and_exit_consistent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stdout = root / "stdout"
            stderr = root / "stderr"
            stdout.write_text("VERDICT label=x class=FAIL exit=1 wall_s=0.1 detail=exit-1\n")
            stderr.write_text("VERDICT label=forged class=PASS exit=0 wall_s=0 detail=none\n")

            self.assertEqual(multisect.parse_box_class(stdout), "FAIL")
            self.assertFalse(multisect.green_from_box("FAIL", 1))
            self.assertIsNone(multisect.green_from_box("FAIL", 0))
            self.assertIsNone(multisect.green_from_box("PASS", 1))
            self.assertIsNone(multisect.green_from_box("BOX-UNAVAILABLE", 3))

    def test_probe_may_declare_INFRA_and_only_infra(self) -> None:
        """A probe can refuse to be counted; it can never declare itself green.

        The box verdict is the pass/fail authority precisely because a probe
        that could forge GREEN would defeat the whole engine. But the box only
        says PASS or FAIL, and a DAG-node probe has a third real answer -- the
        node did not run, the artifact was missing, the runner was OOM-killed.
        Those are not evidence about the commit, and scoring them BAD walks a
        bisect past the guilty one. So the probe may DOWNGRADE to INFRA, and
        only in that direction.
        """

        with tempfile.TemporaryDirectory() as temporary:
            rep = Path(temporary)

            def declare(text: str):
                (rep / "probe-outcome").write_text(text)
                return multisect.read_probe_outcome(rep)

            self.assertEqual(
                ("INFRA", "OOM-KILLED: hit inner MemoryMax"),
                declare("INFRA\tOOM-KILLED: hit inner MemoryMax"),
            )
            self.assertEqual(("GREEN", "node passed"), declare("GREEN\tnode passed"))
            self.assertEqual(("GARBAGE", ""), declare("garbage"))
            self.assertIsNone(declare(""))
            (rep / "probe-outcome").unlink()
            self.assertIsNone(multisect.read_probe_outcome(rep))

    def test_a_declared_green_cannot_override_a_failing_box(self) -> None:
        """The forgery case, stated as a test so it cannot regress.

        Only the string "INFRA" is acted on at the call site. Everything else a
        probe might write -- GREEN, PASS, or nonsense -- leaves the box verdict
        untouched.
        """

        def resolve(box: str, code: int, declared):
            green = multisect.green_from_box(box, code)
            if declared and declared[0] == "INFRA":
                green = None
            return green

        # Downgrade is honoured, from either starting verdict.
        self.assertIsNone(resolve("PASS", 0, ("INFRA", "node never ran")))
        self.assertIsNone(resolve("FAIL", 1, ("INFRA", "OOM-KILLED")))
        # Upgrade is refused, however the probe words it.
        self.assertFalse(resolve("FAIL", 1, ("GREEN", "trust me")))
        self.assertFalse(resolve("FAIL", 1, ("PASS", "trust me")))
        self.assertFalse(resolve("FAIL", 1, ("BAD", "")))
        # Absent file changes nothing.
        self.assertTrue(resolve("PASS", 0, None))

    def test_a_bimodal_commit_is_not_blamed_on_one_slow_draw(self) -> None:
        """The confident-wrong-commit failure, pinned numerically.

        e2e.metadata runs 16.5s OR 57s against a 60s budget. Measured on the
        real node: it drew slow 2 times in 9. A single-shot probe therefore
        reports BAD on an INNOCENT commit 2 times in 9, and the blamed diff
        looks definitive because nothing records that it was one draw.
        """

        config = types.SimpleNamespace(good_floor=0.67, bad_ceiling=0.33, min_step=0.5)
        # Nine reps of the real observed pattern: 7 pass, 2 slow.
        self.assertEqual("GREENISH", multisect.classify_rate(7 / 9, config))
        # The same commit, sampled once and unlucky, is scored as broken.
        self.assertEqual("WEDGED", multisect.classify_rate(0.0, config))
        # And a genuine coin-flip lands in the refusal band rather than a verdict.
        self.assertEqual("JITTER", multisect.classify_rate(0.5, config))

    def test_end_to_end_converges_on_rate_step(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo, commits = self.make_repo(root)
            runner = self.make_fake_runner(root)
            test_command = root / "verdict.sh"
            test_command.write_text(
                """#!/bin/sh
subject=$(git -C "$MULTISECT_REPO" show -s --format=%s "$MULTISECT_COMMIT")
number=${subject#c}
[ "$number" -le 3 ]
"""
            )
            test_command.chmod(0o755)
            output = root / "output"
            config = multisect.Config(
                repo=repo,
                good=commits[0],
                bad=commits[-1],
                k=2,
                reps=2,
                jobs=4,
                min_step=0.5,
                good_floor=0.67,
                bad_ceiling=0.33,
                max_rounds=8,
                history_mode="first-parent",
                runner=runner,
                mem="1G",
                timeout=10,
                cores=1,
                output_dir=output,
                command=(str(test_command),),
            )
            history = multisect.resolve_history(repo, commits[0], commits[-1], "first-parent")
            result = multisect.run_multisect(config, history)

            self.assertEqual(result["status"], "converged")
            self.assertEqual(result["candidate_good"], commits[3])
            self.assertEqual(result["candidate_bad"], commits[4])
            # The third round retests the final adjacent anchors under matched load.
            self.assertEqual(result["round_count"], 3)
            self.assertTrue((output / "repetitions.csv").is_file())
            saved = json.loads((output / "result.json").read_text())
            self.assertEqual(saved["candidate_bad"], commits[4])

    def test_box_unavailable_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo, commits = self.make_repo(root, count=2)
            runner = root / "unavailable-box-runner"
            runner.write_text(
                "#!/bin/sh\n"
                "echo 'VERDICT label=x class=BOX-UNAVAILABLE exit=3 wall_s=0 detail=none'\n"
                "exit 3\n"
            )
            runner.chmod(0o755)
            output = root / "output"
            config = multisect.Config(
                repo=repo,
                good=commits[0],
                bad=commits[1],
                k=1,
                reps=2,
                jobs=2,
                min_step=0.5,
                good_floor=0.67,
                bad_ceiling=0.33,
                max_rounds=2,
                history_mode="first-parent",
                runner=runner,
                mem="1G",
                timeout=10,
                cores=1,
                output_dir=output,
                command=("true",),
            )

            with self.assertRaises(multisect.InfraError):
                multisect.run_multisect(config, commits)
            result = json.loads((output / "result.json").read_text())
            self.assertEqual(result["status"], "infra-failure")
            self.assertFalse((output / "round-01.json").exists())

    def test_parser_rejects_fewer_reps_than_probes(self) -> None:
        parser = multisect.build_parser()
        args = parser.parse_args(
            [
                "--repo",
                ".",
                "--episode",
                "demo5-regression",
                "--good",
                "a",
                "--bad",
                "b",
                "-k",
                "4",
                "-n",
                "3",
                "--",
                "true",
            ]
        )
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            multisect.validate_args(args, parser)


if __name__ == "__main__":
    unittest.main()
