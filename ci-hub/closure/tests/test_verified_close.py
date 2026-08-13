#!/usr/bin/env python3
"""Fixture-only tests for the task-closure evidence gate."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


MODULE_PATH = Path(__file__).resolve().parents[1] / "verified_close.py"
SPEC = importlib.util.spec_from_file_location("verified_close", MODULE_PATH)
assert SPEC and SPEC.loader
verified_close = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = verified_close
SPEC.loader.exec_module(verified_close)


def _git(repo: Path, *args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=True,
        env=env,
    )
    return result.stdout.strip()


def _fixture_repo(root: Path, name: str, value: str) -> tuple[Path, str, str]:
    repo = root / name
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    _git(repo, "config", "user.name", "Closure Environment Test")
    _git(repo, "config", "user.email", "closure-env@example.invalid")
    (repo / "tracked.txt").write_text(value + "\n", encoding="utf-8")
    _git(repo, "add", "tracked.txt")
    _git(repo, "commit", "-qm", value)
    return repo, _git(repo, "rev-parse", "HEAD"), _git(repo, "rev-parse", ":tracked.txt")


class GitEnvironmentBoundaryTest(unittest.TestCase):
    """The argv must name the repository that Git actually reads."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.target, self.target_head, self.target_blob = _fixture_repo(
            root, "target", "target"
        )
        self.wrong, self.wrong_head, self.wrong_blob = _fixture_repo(
            root, "wrong", "wrong"
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def _ambient(self, **selectors: str) -> dict[str, str]:
        env = dict(os.environ)
        env.update(selectors)
        return env

    def _safe(self, args: tuple[str, ...], env: dict[str, str]) -> str:
        with mock.patch.dict(os.environ, env, clear=True):
            result = verified_close._run(
                ("git", "-C", str(self.target), *args), cwd=self.target
            )
        self.assertEqual(0, result.returncode, result.stderr)
        return result.stdout.strip()

    def test_git_dir_wrong_object_store_is_observable_then_removed(self) -> None:
        env = self._ambient(GIT_DIR=str(self.wrong / ".git"))
        self.assertEqual(self.wrong_head, _git(self.target, "rev-parse", "HEAD", env=env))
        self.assertEqual(self.target_head, self._safe(("rev-parse", "HEAD"), env))

    def test_git_common_dir_wrong_object_store_is_observable_then_removed(self) -> None:
        env = self._ambient(
            GIT_DIR=str(self.target / ".git"),
            GIT_COMMON_DIR=str(self.wrong / ".git"),
        )
        # HEAD may remain a per-worktree file, so ask Git which COMMON store it
        # actually selected. The unsafe command names target on its argv while
        # reporting wrong/.git as the ref/object authority.
        self.assertEqual(
            str(self.wrong / ".git"),
            _git(self.target, "rev-parse", "--path-format=absolute", "--git-common-dir", env=env),
        )
        self.assertEqual(
            str(self.target / ".git"),
            self._safe(("rev-parse", "--path-format=absolute", "--git-common-dir"), env),
        )

    def test_git_index_file_wrong_state_is_observable_then_removed(self) -> None:
        env = self._ambient(GIT_INDEX_FILE=str(self.wrong / ".git" / "index"))
        self.assertEqual(
            self.wrong_blob, _git(self.target, "rev-parse", ":tracked.txt", env=env)
        )
        self.assertEqual(
            self.target_blob, self._safe(("rev-parse", ":tracked.txt"), env)
        )

    def test_git_work_tree_wrong_state_is_observable_then_removed(self) -> None:
        env = self._ambient(GIT_WORK_TREE=str(self.wrong))
        self.assertEqual(
            str(self.wrong), _git(self.target, "rev-parse", "--show-toplevel", env=env)
        )
        self.assertEqual(
            str(self.target), self._safe(("rev-parse", "--show-toplevel"), env)
        )

    def test_artifact_only_present_in_wrong_git_dir_is_refused(self) -> None:
        """A wrong object store cannot manufacture parent-main artifact evidence."""

        relative = "only-in-ambient-wrong-repository.txt"
        (self.wrong / relative).write_text("not parent evidence\n", encoding="utf-8")
        _git(self.wrong, "add", relative)
        _git(self.wrong, "commit", "-qm", "wrong-only artifact")
        _git(self.wrong, "update-ref", "refs/remotes/origin/main", "HEAD")
        env = self._ambient(GIT_DIR=str(self.wrong / ".git"))

        # The planted unsafe call succeeds, proving the fixture can create the
        # false artifact result rather than merely testing an absent path.
        self.assertEqual(
            "blob",
            _git(
                verified_close.ROOT,
                "cat-file",
                "-t",
                f"origin/main:{relative}",
                env=env,
            ),
        )

        def no_network_fetch(command, **kwargs):
            if tuple(command[:2]) == ("with-proxy", "git") and "fetch" in command:
                return completed(command)
            return verified_close._run(command, **kwargs)

        with mock.patch.dict(os.environ, env, clear=True):
            evidence = verified_close.verify_artifact(relative, run=no_network_fetch)

        self.assertEqual("refused", evidence.state)
        self.assertIn("not on parent main", evidence.reason)


def completed(command, rc=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


class FakeRunner:
    def __init__(
        self,
        code_state="landed",
        *,
        artifact_present=True,
        artifact_type="blob",
        artifact_ancestry_rc=0,
        tags="implemented",
    ):
        self.tags = tags
        self.code_state = code_state
        self.artifact_present = artifact_present
        self.artifact_type = artifact_type
        self.artifact_ancestry_rc = artifact_ancestry_rc
        self.task_mutations: list[tuple[str, ...]] = []
        self.commands: list[tuple[str, ...]] = []

    def __call__(self, command, **_kwargs):
        command = tuple(str(item) for item in command)
        self.commands.append(command)
        if "protocol.py" in " ".join(command):
            if self.code_state == "landed":
                payload = {
                    "state": "landed",
                    "rc": 0,
                    "resolved_sha": "a" * 40,
                }
                return completed(command, stdout=json.dumps(payload))
            if self.code_state == "not-landed":
                payload = {
                    "state": "not-landed",
                    "rc": 1,
                    "resolved_sha": "b" * 40,
                }
                return completed(command, rc=1, stdout=json.dumps(payload))
            payload = {
                "state": "unverifiable",
                "rc": 2,
                "reason": "no mergeCommit.oid",
            }
            return completed(command, rc=2, stdout=json.dumps(payload))
        if command[:4] == ("with-proxy", "gh", "run", "view"):
            run_id = command[4]
            payload = {
                "databaseId": int(run_id),
                "url": f"https://github.test/actions/runs/{run_id}",
                "status": "completed",
                "conclusion": "success",
            }
            return completed(command, stdout=json.dumps(payload))
        if command[:3] == ("with-proxy", "git", "-C"):
            return completed(command)
        if command[:5] == (
            "git",
            "-C",
            str(verified_close.ROOT),
            "cat-file",
            "-t",
        ):
            if not self.artifact_present:
                return completed(
                    command, rc=1, stderr="fatal: path does not exist in 'origin/main'"
                )
            return completed(command, stdout=self.artifact_type + "\n")
        if command[:4] == ("git", "-C", str(verified_close.ROOT), "log"):
            return completed(command, stdout="d" * 40 + "\n")
        if command[:4] == (
            "git",
            "-C",
            str(verified_close.ROOT),
            "merge-base",
        ):
            return completed(command, rc=self.artifact_ancestry_rc)
        if command[:4] == ("git", "-C", str(verified_close.ROOT), "rev-parse"):
            return completed(command, stdout="c" * 40 + "\n")
        if command[:2] == ("tg", "show"):
            # A READ, not a mutation. Keeping it out of task_mutations is what
            # lets the "refused paths never touch the task" assertions stay
            # meaningful now that the gateway reads tags before closing.
            return completed(command, stdout=f"Status:    IN_PROGRESS\nTags:      {self.tags}\n")
        if command and command[0] == "tg":
            self.task_mutations.append(command)
            return completed(command)
        raise AssertionError(f"unexpected command: {command}")


class VerifiedCloseTest(unittest.TestCase):
    def test_unverifiable_reference_is_refused_without_task_mutation(self):
        runner = FakeRunner(code_state="unverifiable")
        rc = verified_close.main(
            ["fixture-task", "--code", "123", "--repo", "rrnewton/hermit", "--source", "."],
            run=runner,
        )
        self.assertEqual(verified_close.UNVERIFIABLE, rc)
        self.assertEqual([], runner.task_mutations)

    def test_not_landed_and_unverifiable_both_refuse_without_task_mutation(self):
        unlanded = FakeRunner(code_state="not-landed")
        unverifiable = FakeRunner(code_state="unverifiable")
        self.assertEqual(
            verified_close.REFUSED,
            verified_close.main(
                ["fixture-task", "--code", "b" * 40, "--source", "."],
                run=unlanded,
            ),
        )
        self.assertEqual(
            verified_close.UNVERIFIABLE,
            verified_close.main(
                ["fixture-task", "--code", "123", "--repo", "rrnewton/hermit", "--source", "."],
                run=unverifiable,
            ),
        )
        self.assertEqual([], unlanded.task_mutations)
        self.assertEqual([], unverifiable.task_mutations)

    def test_not_landed_is_refused_uniformly_for_every_repository(self):
        for repo in (
            "rrnewton/hermit",
            "rrnewton/reverie",
            "rrnewton/dev-hermit",
        ):
            with self.subTest(repo=repo):
                runner = FakeRunner(code_state="not-landed")
                rc = verified_close.main(
                    ["fixture-task", "--code", "b" * 40, "--repo", repo,
                     "--source", "."],
                    run=runner,
                )
                self.assertEqual(verified_close.REFUSED, rc)
                self.assertEqual([], runner.task_mutations)

    def test_landed_closure_records_landed_in_the_note(self):
        runner = FakeRunner(code_state="landed")
        self.assertEqual(
            verified_close.CLOSED,
            verified_close.main(
                ["fixture-task", "--code", "a" * 40, "--source", "."], run=runner
            ),
        )
        notes = [c for c in runner.task_mutations if c[:2] == ("tg", "note")]
        self.assertEqual(1, len(notes), notes)
        self.assertIn("landing=landed", notes[0][-1])

    def test_landed_close_requires_the_implemented_tag(self):
        runner = FakeRunner(code_state="landed", tags="infra, process")
        self.assertEqual(
            verified_close.REFUSED,
            verified_close.main(
                ["fixture-task", "--code", "a" * 40, "--source", "."], run=runner
            ),
        )
        self.assertEqual([], runner.task_mutations)

    def test_check_only_does_not_require_the_implemented_tag(self):
        runner = FakeRunner(code_state="landed", tags="infra, process")
        self.assertEqual(
            verified_close.CLOSED,
            verified_close.main(
                ["fixture-task", "--code", "a" * 40, "--source", ".",
                 "--check-only"],
                run=runner,
            ),
        )
        self.assertEqual([], runner.task_mutations)

    def test_three_legitimate_fixture_closures_succeed(self):
        runner = FakeRunner()
        cases = (
            ["code-task", "--code", "123", "--repo", "rrnewton/hermit", "--source", "."],
            ["artifact-task", "--artifact", "AGENTS.md"],
            ["run-task", "--run-id", "987", "--repo", "rrnewton/hermit"],
        )
        results = [verified_close.main(case, run=runner) for case in cases]

        self.assertEqual([0, 0, 0], results)
        notes = [command for command in runner.task_mutations if command[1] == "note"]
        closes = [command for command in runner.task_mutations if command[1] == "update"]
        self.assertEqual(3, len(notes))
        self.assertEqual(3, len(closes))
        self.assertTrue(all("CLOSURE-VERIFIED:" in command[3] for command in notes))
        self.assertTrue(all(command[-2:] == ("--status", "closed") for command in closes))
        self.assertEqual(
            ["note", "update"] * 3,
            [command[1] for command in runner.task_mutations],
        )
        artifact_note = next(
            command[3]
            for command in notes
            if "kind=artifact" in command[3]
        )
        self.assertIn("rrnewton/dev-hermit:AGENTS.md@" + "d" * 40, artifact_note)
        self.assertIn("target=main@" + "c" * 40, artifact_note)

    def test_missing_or_nonancestral_artifact_never_mutates_task(self):
        missing = FakeRunner(artifact_present=False)
        nonancestral = FakeRunner(artifact_ancestry_rc=1)

        self.assertEqual(
            verified_close.REFUSED,
            verified_close.main(
                ["missing-artifact", "--artifact", "AGENTS.md"], run=missing
            ),
        )
        self.assertEqual(
            verified_close.REFUSED,
            verified_close.main(
                ["orphan-artifact", "--artifact", "AGENTS.md"],
                run=nonancestral,
            ),
        )
        self.assertEqual([], missing.task_mutations)
        self.assertEqual([], nonancestral.task_mutations)

    def test_artifact_absent_from_this_working_tree_still_verifies(self):
        # The regression this brackets: a parent artifact published the ONLY
        # safe way -- from a worktree off origin/main -- is absent from the
        # chronically-behind parent primary. Gating on the working tree refused
        # it with "artifact is not a file" even though it was tracked, pushed,
        # and ancestry-present. Authority is origin/main, not this checkout.
        relative = "ai_docs/deliberately-absent-from-this-working-tree.md"
        self.assertFalse(
            (verified_close.ROOT / relative).exists(),
            "fixture path must not exist locally or the test proves nothing",
        )
        runner = FakeRunner()

        rc = verified_close.main(["absent-artifact", "--artifact", relative], run=runner)

        self.assertEqual(verified_close.CLOSED, rc)
        note = next(
            command[3] for command in runner.task_mutations if command[1] == "note"
        )
        self.assertIn(f"rrnewton/dev-hermit:{relative}@" + "d" * 40, note)
        self.assertNotIn(
            "ls-files",
            " ".join(" ".join(command) for command in runner.commands),
            "the stale-index gate must be gone, not merely bypassed",
        )

    def test_directory_on_main_is_refused_not_closed(self):
        # `cat-file -e origin/main:<dir>` succeeds for a TREE. Existence alone
        # would let a caller close against a directory, so the type is checked.
        runner = FakeRunner(artifact_type="tree")

        rc = verified_close.main(["tree-artifact", "--artifact", "ai_docs"], run=runner)

        self.assertEqual(verified_close.REFUSED, rc)
        self.assertEqual([], runner.task_mutations)

    def test_bare_pr_number_without_explicit_repo_is_refused(self):
        # Every repository has a #56. `execute-ambiguous-zero-fix-order-a3-a4-first`
        # -- a PARENT-repo task about compat-envelope/render-scorecard.rs -- was
        # closed with a bare `--code 56`, which the defaults resolved against
        # rrnewton/hermit and matched "docs: add Hermit error catalog (#56)"
        # from three weeks earlier. Real ancestry, wrong repository.
        runner = FakeRunner()

        rc = verified_close.main(["parent-task", "--code", "56"], run=runner)

        self.assertEqual(verified_close.REFUSED, rc)
        self.assertEqual([], runner.task_mutations)
        self.assertEqual([], runner.commands, "refusal must precede any verifier call")

    def test_full_sha_without_repo_still_works(self):
        # A 40-hex SHA is self-identifying -- the verifier can only resolve it
        # where it exists -- so the stricter rule must NOT catch it, or every
        # existing SHA-based caller breaks.
        runner = FakeRunner()

        rc = verified_close.main(["sha-task", "--code", "a" * 40], run=runner)

        self.assertEqual(verified_close.CLOSED, rc)

    def test_closure_note_records_which_repository_was_verified(self):
        runner = FakeRunner()

        verified_close.main(
            ["repo-task", "--code", "a" * 40, "--repo", "rrnewton/dev-hermit"],
            run=runner,
        )

        note = next(
            command[3] for command in runner.task_mutations if command[1] == "note"
        )
        self.assertIn("rrnewton/dev-hermit@" + "a" * 40, note)

    def test_code_closure_note_is_readable_by_its_downstream_consumer(self):
        # The note is not just for humans: ci-hub/directives/tg_landed.py derives
        # landing state from it. Its extractor takes an explicit SHA token or a
        # typed `@sha` tuple, so the OLD bare `resolved=<40hex>` matched NEITHER
        # and yielded []. Code closures were recording a SHA the consumer could
        # not read. Bind the two here so the format cannot drift apart again.
        spec = importlib.util.spec_from_file_location(
            "tg_landed",
            Path(verified_close.__file__).resolve().parents[1] / "directives/tg_landed.py",
        )
        assert spec and spec.loader
        tg_landed = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = tg_landed
        spec.loader.exec_module(tg_landed)

        runner = FakeRunner()
        verified_close.main(
            ["consumer-task", "--code", "a" * 40, "--repo", "rrnewton/dev-hermit"],
            run=runner,
        )
        note = next(
            command[3] for command in runner.task_mutations if command[1] == "note"
        )

        shas, _ = tg_landed.extract_implementation_refs([note])
        self.assertEqual(["a" * 40], shas)

    def test_artifact_outside_the_workspace_is_refused(self):
        runner = FakeRunner()

        rc = verified_close.main(
            ["escaped-artifact", "--artifact", "/etc/hostname"], run=runner
        )

        self.assertEqual(verified_close.REFUSED, rc)
        self.assertEqual([], runner.task_mutations)
        self.assertEqual([], runner.commands, "refusal must precede any git call")


if __name__ == "__main__":
    unittest.main()


class ContentLostRefusesClosure(unittest.TestCase):
    """THE CONSUMER TEST. Detectable-in-principle is not the same as refused-in-practice.

    A detector nothing calls has zero coverage. These assert that verify_code -- the
    engine behind `ci-hub/bin/close-task` -- actually REFUSES a landing whose content
    did not survive, and still ACCEPTS one whose content did.
    """

    def setUp(self) -> None:
        import tempfile
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self._tmp.name) / "r"
        self.repo.mkdir()

        def g(*a: str) -> str:
            import subprocess as sp
            p = sp.run(["git", "-C", str(self.repo), *a], capture_output=True, text=True)
            if p.returncode:
                raise AssertionError(f"git {a}: {p.stderr}")
            return p.stdout.strip()

        self.g = g
        g("init", "-q", "-b", "main")
        g("config", "user.email", "t@t")
        g("config", "user.name", "t")
        body = ["top"] + [f"pad{i}" for i in range(20)] + ["bottom"]
        (self.repo / "f.py").write_text("\n".join(body) + "\n")
        g("add", "f.py")
        g("commit", "-qm", "base")
        self.base = g("rev-parse", "HEAD")
        b = (self.repo / "f.py").read_text().splitlines()
        b[0] = "top CHANGED-A"
        b[-1] = "bottom CHANGED-B"
        (self.repo / "f.py").write_text("\n".join(b) + "\n")
        g("add", "f.py")
        g("commit", "-qm", "feature")
        self.feature = g("rev-parse", "HEAD")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _landed_verifier(self):
        """Stub the upstream verifier so it reports LANDED, as ancestry would."""
        import json as _json
        import subprocess as _sp

        def run(command, cwd=None, **kw):
            payload = _json.dumps(
                {"state": "landed", "resolved_sha": self.feature, "reason": "landed"}
            )
            return _sp.CompletedProcess(list(command), 0, payload, "")

        return run

    def test_lossy_reconcile_is_REFUSED_by_the_closure_gateway(self) -> None:
        self.g("checkout", "-q", "-b", "lossy", self.base)
        self.g("merge", "-q", "--no-commit", "--no-ff", self.feature)
        b = (self.repo / "f.py").read_text().splitlines()
        b[-1] = "bottom"                      # drop one hunk, keep the SHA an ancestor
        (self.repo / "f.py").write_text("\n".join(b) + "\n")
        self.g("add", "f.py")
        self.g("commit", "-qm", "lossy reconcile")
        target = self.g("rev-parse", "HEAD")

        ev = verified_close.verify_code(
            "PR-1", repo="rrnewton/dev-hermit", source=self.repo,
            target=target, run=self._landed_verifier(),
        )
        self.assertEqual(ev.state, "refused", ev.reason)
        self.assertIn("CONTENT-LOST", ev.reason)
        self.assertIn("1 of 2 hunk", ev.reason)

    def test_intact_landing_still_CLOSES(self) -> None:
        """A gate that refuses everything is useless; the honest case must still pass."""
        self.g("checkout", "-q", "-b", "clean", self.base)
        self.g("merge", "-q", "--no-ff", "--no-edit", self.feature)
        target = self.g("rev-parse", "HEAD")
        ev = verified_close.verify_code(
            "PR-2", repo="rrnewton/dev-hermit", source=self.repo,
            target=target, run=self._landed_verifier(),
        )
        self.assertEqual(ev.state, "verified", ev.reason)
        self.assertEqual(ev.landing, "landed")
