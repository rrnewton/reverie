#!/usr/bin/env python3
"""Fixture-only tests for the task-closure evidence gate."""

from __future__ import annotations

import importlib.util
import json
import os
import re
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


def task_sql_result(command, *, tags: str, notes: tuple[str, ...] | list[str] = ()):
    if "exact_note_count" in command[2]:
        match = re.search(r"hex\(content\)='([0-9A-F]*)'", command[2])
        assert match is not None, command[2]
        expected = bytes.fromhex(match.group(1)).decode("utf-8")
        count = sum(note == expected for note in notes)
        return completed(
            command,
            stdout=(
                "json_object('exact_note_count', count(*))\n"
                "---------------\n"
                f'{{"exact_note_count":{count}}}\n\n(1 rows)\n'
            ),
        )
    return completed(command, stdout=f"tags\n----\n{tags}\n\n(1 row)\n")


class FakeRunner:
    def __init__(
        self,
        code_state="landed",
        *,
        artifact_present=True,
        artifact_type="blob",
        artifact_ancestry_rc=0,
        tags="implemented",
        status_failures=0,
    ):
        try:
            parsed_tags = json.loads(tags)
        except json.JSONDecodeError:
            parsed_tags = [tag.strip() for tag in tags.split(",") if tag.strip()]
        self.tags = json.dumps(parsed_tags)
        self.code_state = code_state
        self.artifact_present = artifact_present
        self.artifact_type = artifact_type
        self.artifact_ancestry_rc = artifact_ancestry_rc
        self.task_mutations: list[tuple[str, ...]] = []
        self.commands: list[tuple[str, ...]] = []
        self.notes: list[str] = []
        self.status = "IN_PROGRESS"
        self.status_failures = status_failures

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
        if command[:2] == ("tg", "sql"):
            result = task_sql_result(command, tags=self.tags, notes=self.notes)
            # A READ, not a mutation. Keeping it out of task_mutations is what
            # lets the "refused paths never touch the task" assertions stay
            # meaningful now that the gateway reads tags before closing.
            #
            # ⚠️ THIS MIRRORS `tg sql` FRAMING, NOT A BARE VALUE. The reader under
            # test strips a header line, a rule, and a trailing "(N rows)"; a stub
            # that returned only the value would exercise a parser the real tool
            # never sees. The gate this feeds previously scanned `tg show` for a
            # "Tags:" line that its mission-view output does not contain, so it
            # answered "no tags" for every task alive -- a stub that is too
            # forgiving is exactly how that survived.
            return result
        if command and str(command[0]).endswith("tg-note-verified"):
            # The durable closure note is written through the verified writer, not
            # `tg note`. It IS a mutation of the task's record, so it belongs in
            # task_mutations alongside the status change.
            #
            # ⚠️ RECORD IT AS ('tg','note',task,CONTENT), reading the --file back.
            # The content now travels as a temp-file path that close_task() unlinks
            # in a finally block, so a stub that stored the argv verbatim would
            # leave every "what did the note actually say" assertion inspecting a
            # filename. Those assertions are the only thing checking that the
            # landing state, the repository and the drift qualifier reach the
            # durable record, so they must keep seeing the text.
            content = ""
            if "--file" in command:
                path = command[command.index("--file") + 1]
                try:
                    with open(path, encoding="utf-8") as handle:
                        content = handle.read()
                except OSError:
                    content = ""
            self.task_mutations.append(("tg", "note", command[1], content))
            self.notes.append(content)
            return completed(command)
        if command and command[0] == "tg":
            if command[:2] == ("tg", "update") and "--tags" in command:
                raw = command[command.index("--tags") + 1]
                self.tags = json.dumps(raw.split(",") if raw else [])
            self.task_mutations.append(command)
            if command[:2] == ("tg", "update") and command[-2:] == (
                "--status",
                "closed",
            ):
                if self.status_failures:
                    self.status_failures -= 1
                    return completed(
                        command,
                        rc=1,
                        stderr=(
                            "SQLite error 517: Cannot promote read transaction to write "
                            "transaction because of writes by another connection."
                        ),
                    )
                self.status = "CLOSED"
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

    # CONTRACT CHANGE, 2026-08-06 (close-on-implemented + single drain). This
    # test previously asserted that a resolved-but-not-landed reference was
    # REFUSED, pairing it with the unverifiable case. Under the new rule those
    # two are no longer the same kind of thing: "not landed yet" is a VERIFIED
    # state that closes, while "did not resolve" still refuses. Rewritten in
    # place rather than deleted so the reversal is visible in review.
    def test_not_landed_closes_while_unverifiable_still_refuses(self):
        unlanded = FakeRunner(code_state="not-landed")
        unverifiable = FakeRunner(code_state="unverifiable")
        self.assertEqual(
            verified_close.CLOSED,
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
        self.assertNotEqual([], unlanded.task_mutations)
        self.assertEqual([], unverifiable.task_mutations)

    def test_parent_not_landed_is_refused_without_task_mutation(self):
        """Parent tooling is direct-to-main, so no unlanded close is legitimate."""
        runner = FakeRunner(code_state="not-landed")

        rc = verified_close.main(
            [
                "parent-task",
                "--code",
                "b" * 40,
                "--repo",
                verified_close.PARENT_REPO,
                "--source",
                ".",
            ],
            run=runner,
        )

        self.assertEqual(verified_close.REFUSED, rc)
        self.assertEqual([], runner.task_mutations)

    def test_unlanded_closure_records_the_landing_state_in_the_note(self):
        """`closed` no longer implies landed, so the note must say which it was.

        Without this the closure record is indistinguishable from a landing
        claim -- the exact proxy-binding error the drain rule exists to avoid.
        """
        runner = FakeRunner(code_state="not-landed")
        self.assertEqual(
            verified_close.CLOSED,
            verified_close.main(
                ["fixture-task", "--code", "b" * 40, "--source", "."], run=runner
            ),
        )
        notes = [c for c in runner.task_mutations if c[:2] == ("tg", "note")]
        self.assertEqual(1, len(notes), notes)
        self.assertIn("landing=implemented-unlanded", notes[0][-1])
        self.assertIn("b" * 40, notes[0][-1])

        landed = FakeRunner(code_state="landed")
        verified_close.main(
            ["fixture-task", "--code", "a" * 40, "--source", "."], run=landed
        )
        landed_notes = [c for c in landed.task_mutations if c[:2] == ("tg", "note")]
        self.assertIn("landing=landed", landed_notes[0][-1])

    def test_landed_marker_adds_the_landed_tag_before_closing(self):
        runner = FakeRunner(code_state="landed", tags='["implemented","drain"]')

        rc = verified_close.main(
            ["fixture-task", "--code", "a" * 40, "--source", "."], run=runner
        )

        self.assertEqual(verified_close.CLOSED, rc)
        self.assertEqual('["implemented", "drain", "landed"]', runner.tags)
        mutations = runner.task_mutations
        note_index = next(i for i, command in enumerate(mutations) if command[1] == "note")
        tag_index = next(
            i for i, command in enumerate(mutations)
            if command[1] == "update" and "--tags" in command
        )
        close_index = next(
            i for i, command in enumerate(mutations)
            if command[1] == "update" and command[-2:] == ("--status", "closed")
        )
        self.assertLess(note_index, tag_index)
        self.assertLess(tag_index, close_index)
        self.assertEqual(
            ("tg", "update", "fixture-task", "--tags", "implemented,drain,landed"),
            mutations[tag_index],
        )

    def test_implemented_unlanded_marker_does_not_add_the_landed_tag(self):
        runner = FakeRunner(code_state="not-landed", tags='["implemented","drain"]')

        rc = verified_close.main(
            ["fixture-task", "--code", "b" * 40, "--source", "."], run=runner
        )

        self.assertEqual(verified_close.CLOSED, rc)
        self.assertEqual('["implemented", "drain"]', runner.tags)
        self.assertFalse(
            any(command[1] == "update" and "--tags" in command
                for command in runner.task_mutations)
        )

    def test_n_a_marker_does_not_add_the_landed_tag(self):
        runner = FakeRunner(tags='["implemented","research"]')
        evidence = verified_close.Evidence(
            "verified",
            "artifact",
            "ai_docs/result.md",
            resolved="rrnewton/dev-hermit:ai_docs/result.md@" + "a" * 40,
        )

        rc = verified_close.close_task("fixture-task", evidence, run=runner)

        self.assertEqual(verified_close.CLOSED, rc)
        note = next(
            command[3] for command in runner.task_mutations if command[1] == "note"
        )
        self.assertIn("landing=n/a", note)
        self.assertFalse(
            any(command[1] == "update" and "--tags" in command
                for command in runner.task_mutations)
        )

    def test_an_existing_landed_tag_is_not_rewritten(self):
        runner = FakeRunner(code_state="landed", tags='["implemented","landed"]')

        rc = verified_close.main(
            ["fixture-task", "--code", "a" * 40, "--source", "."], run=runner
        )

        self.assertEqual(verified_close.CLOSED, rc)
        self.assertFalse(
            any(command[1] == "update" and "--tags" in command
                for command in runner.task_mutations)
        )
        self.assertIn(
            ("tg", "update", "fixture-task", "--status", "closed"),
            runner.task_mutations,
        )

    def test_failed_landed_tag_write_leaves_the_task_open(self):
        runner = FakeRunner(code_state="landed", tags='["implemented","drain"]')

        def fail_tag_update(command, **kwargs):
            if command[:2] == ("tg", "update") and "--tags" in command:
                runner.task_mutations.append(tuple(command))
                return completed(command, rc=1, stderr="tag update failed")
            return runner(command, **kwargs)

        rc = verified_close.main(
            ["fixture-task", "--code", "a" * 40, "--source", "."],
            run=fail_tag_update,
        )

        self.assertEqual(verified_close.UNVERIFIABLE, rc)
        self.assertTrue(any(command[1] == "note" for command in runner.task_mutations))
        self.assertFalse(
            any(command[1] == "update" and command[-2:] == ("--status", "closed")
                for command in runner.task_mutations)
        )

    def test_unconfirmed_landed_tag_write_leaves_the_task_open(self):
        runner = FakeRunner(code_state="landed", tags='["implemented","drain"]')

        def ignore_tag_update(command, **kwargs):
            if command[:2] == ("tg", "update") and "--tags" in command:
                runner.task_mutations.append(tuple(command))
                return completed(command)
            return runner(command, **kwargs)

        rc = verified_close.main(
            ["fixture-task", "--code", "a" * 40, "--source", "."],
            run=ignore_tag_update,
        )

        self.assertEqual(verified_close.UNVERIFIABLE, rc)
        self.assertFalse(
            any(command[1] == "update" and command[-2:] == ("--status", "closed")
                for command in runner.task_mutations)
        )

    def test_landed_close_refuses_before_writing_when_tags_are_unreadable(self):
        runner = FakeRunner(code_state="landed")

        def unreadable_tags(command, **kwargs):
            if command[:2] == ("tg", "sql"):
                return completed(command, rc=1, stderr="database unavailable")
            return runner(command, **kwargs)

        rc = verified_close.main(
            ["fixture-task", "--code", "a" * 40, "--source", "."],
            run=unreadable_tags,
        )

        self.assertEqual(verified_close.UNVERIFIABLE, rc)
        self.assertEqual([], runner.task_mutations)

    def test_unlanded_close_is_refused_when_the_implemented_tag_is_absent(self):
        """The negative bracket for the tag gate.

        `drain-implemented-to-landed` selects on the `implemented` tag. Closing
        an unlanded task without it would drop the work out of ready, active,
        AND the drain simultaneously -- invisible rather than pending. Plant the
        violating case (no tag) and confirm refusal with no task mutation.
        """
        runner = FakeRunner(code_state="not-landed", tags="infra, process")
        self.assertEqual(
            verified_close.REFUSED,
            verified_close.main(
                ["fixture-task", "--code", "b" * 40, "--source", "."], run=runner
            ),
        )
        self.assertEqual([], runner.task_mutations)

    def test_landed_close_does_not_require_the_implemented_tag(self):
        """Anti-vacuity guard for the gate above.

        A gate that refused every untagged close would also block the ordinary
        landed path, so assert the tag requirement is scoped to the unlanded
        state only.
        """
        runner = FakeRunner(code_state="landed", tags="infra, process")
        self.assertEqual(
            verified_close.CLOSED,
            verified_close.main(
                ["fixture-task", "--code", "a" * 40, "--source", "."], run=runner
            ),
        )
        self.assertNotEqual([], runner.task_mutations)

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
        closes = [
            command for command in runner.task_mutations
            if command[1] == "update" and command[-2:] == ("--status", "closed")
        ]
        self.assertEqual(3, len(notes))
        self.assertEqual(3, len(closes))
        self.assertTrue(all("CLOSURE-VERIFIED:" in command[3] for command in notes))
        self.assertTrue(all(command[-2:] == ("--status", "closed") for command in closes))
        self.assertEqual(
            ["note", "update", "update", "note", "update", "note", "update"],
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
        # A clean landing carries NO qualifier. This is the control for the drift case
        # below: if `reason` were populated here too, "detail=" in the closure note
        # would not distinguish a drifted close from a clean one.
        self.assertIsNone(ev.reason)

    def test_context_drift_CLOSES_but_is_RECORDED_not_silent(self) -> None:
        """A hunk displaced by a later adjacent insertion is not content loss.

        It must close -- the added content is confirmed present -- while still saying
        so, because a closure that passed through a drift must not read afterwards as
        a clean match nobody observed.
        """
        self.g("checkout", "-q", "-b", "drifted", self.feature)
        b = (self.repo / "f.py").read_text().splitlines()
        b.insert(0, "brand new first line")   # displaces the first hunk's context
        (self.repo / "f.py").write_text("\n".join(b) + "\n")
        self.g("add", "f.py")
        self.g("commit", "-qm", "later: insert above")
        target = self.g("rev-parse", "HEAD")

        ev = verified_close.verify_code(
            "PR-3", repo="rrnewton/dev-hermit", source=self.repo,
            target=target, run=self._landed_verifier(),
        )
        self.assertEqual(ev.state, "verified", ev.reason)
        self.assertEqual(ev.landing, "landed")
        self.assertIsNotNone(ev.reason)
        self.assertIn("CONTEXT-DRIFTED", ev.reason)
        self.assertNotIn("CONTENT-LOST", ev.reason)

    def test_the_drift_qualifier_reaches_the_durable_closure_note(self) -> None:
        """The record is the point. An in-memory `reason` nobody writes down is not one."""
        runner = FakeRunner()

        ev = verified_close.Evidence(
            "verified", "code", "PR-4", resolved="rrnewton/dev-hermit@" + self.feature,
            reason="CONTEXT-DRIFTED: 1 of 2 hunk(s) no longer reverse-apply",
            landing="landed",
        )
        verified_close.close_task("t", ev, run=runner)
        notes = [c for c in runner.task_mutations if len(c) > 1 and c[1] == "note"]
        self.assertTrue(notes, f"no tg note was written: {runner.task_mutations}")
        body = notes[0][-1]
        self.assertIn("CLOSURE-VERIFIED", body)
        self.assertIn("detail=CONTEXT-DRIFTED", body)
        # The 40-hex must survive the append, because tg_landed.py regexes it out.
        self.assertIn(self.feature, body)


class DispositionClosesWithoutInflatingTheMetric(unittest.TestCase):
    """`--disposition` exists so the closure guard can be TOTAL.

    Measured 2026-08-25: of 477 tasks closed that day without a CLOSURE-VERIFIED
    note, 380 named a PR or SHA and could have used this verifier -- but 97 named
    nothing at all, because review lanes, duplicates and answered questions have no
    landing to prove. A verifier that only accepts --code/--artifact/--run-id is
    unusable for a fifth of real closures, so a guard built on it gets routed
    around rather than obeyed. These tests pin the two properties that keep the
    escape hatch from becoming a hole.
    """

    def _harness(self, tags: str):
        recorded: list[tuple[str, ...]] = []
        notes: list[str] = []

        def run(command, cwd=None, **kw):
            import subprocess as _sp
            if command[:2] == ("tg", "sql"):
                return task_sql_result(command, tags=tags, notes=notes)
            if command and str(command[0]).endswith("tg-note-verified") and "--file" in command:
                path = list(command)[list(command).index("--file") + 1]
                with open(path, encoding="utf-8") as handle:
                    content = handle.read()
                    notes.append(content)
                    recorded.append(("tg", "note", command[1], content))
                return _sp.CompletedProcess(list(command), 0, "", "")
            recorded.append(tuple(command))
            return _sp.CompletedProcess(list(command), 0, "", "")

        return recorded, run

    def _disposition(self):
        return verified_close.Evidence(
            "verified", "disposition", "superseded by hermit#1234",
            resolved="no-code-reference", landing="none",
        )

    def test_disposition_records_under_a_DIFFERENT_anchor(self) -> None:
        """CLOSURE-VERIFIED must not be writable by a closure that verified nothing.

        Every landing-debt query and the health tick count that exact string. If a
        disposition wrote it, the metric would rise by precisely the closures
        carrying no evidence -- the instrument reporting improvement caused by the
        thing it exists to detect.
        """
        recorded, run = self._harness('["review"]')
        rc = verified_close.close_task("t", self._disposition(), run=run)
        self.assertEqual(rc, verified_close.CLOSED)
        notes = [c for c in recorded if len(c) > 1 and c[1] == "note"]
        self.assertTrue(notes, f"no note was written: {recorded}")
        body = notes[0][-1]
        self.assertIn("CLOSURE-RECORDED", body)
        self.assertNotIn("CLOSURE-VERIFIED", body)
        self.assertIn("kind=disposition", body)
        self.assertIn("landing=none", body)
        self.assertIn(("tg", "update", "t", "--status", "closed"), recorded)

    def test_disposition_is_refused_on_a_task_claiming_a_landing(self) -> None:
        """The one population this verifier exists for must not opt out in a flag."""
        for tags in ('["implemented"]', '["landed"]', '["drain","implemented"]'):
            with self.subTest(tags=tags):
                recorded, run = self._harness(tags)
                rc = verified_close.close_task("t", self._disposition(), run=run)
                self.assertEqual(rc, verified_close.REFUSED)
                self.assertNotIn(
                    ("tg", "update", "t", "--status", "closed"),
                    recorded,
                    "a refused disposition must leave the task open",
                )

    def test_unreadable_tags_refuse_the_disposition(self) -> None:
        """Fail CLOSED. 'I could not read the tags' must not be read as 'it claims nothing'."""
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            if command[:2] == ("tg", "sql"):
                return _sp.CompletedProcess(list(command), 1, "", "db unavailable")
            return _sp.CompletedProcess(list(command), 0, "", "")

        self.assertEqual(
            verified_close.close_task("t", self._disposition(), run=run),
            verified_close.REFUSED,
        )

    def test_tag_reader_survives_the_tg_sql_frame(self) -> None:
        """The gate this feeds was inert for exactly this reason.

        It used to scan `tg show` for a line starting with "Tags:"; `tg show`
        prints a mission-view tree with no such line, so it answered "no tags" for
        every task alive. Parse the real framing or do not claim to read tags.
        """
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            return _sp.CompletedProcess(
                list(command), 0, 'tags\n----------\n["drain","landed"]\n\n(1 row)\n', ""
            )

        self.assertEqual(
            verified_close.read_task_tags("t", run=run), ("drain", "landed")
        )
        self.assertTrue(verified_close.task_claims_landing("t", run=run))

    def test_tag_checks_do_not_match_substrings(self) -> None:
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            return _sp.CompletedProcess(
                list(command), 0,
                'tags\n----------\n["not-implemented","unlanded"]\n\n(1 row)\n',
                "",
            )

        self.assertEqual(
            verified_close.read_task_tags("t", run=run),
            ("not-implemented", "unlanded"),
        )
        self.assertFalse(verified_close.task_has_implemented_tag("t", run=run))
        self.assertFalse(verified_close.task_claims_landing("t", run=run))

    def test_a_task_id_that_is_not_an_identifier_is_refused(self) -> None:
        """The id is interpolated into SQL, so anything unexpected reads as unreadable."""
        self.assertIsNone(verified_close.read_task_tags("t'; drop table tasks--"))


class CheckOnlyPredictsTheRealRun(unittest.TestCase):
    """A dry run that answers a NARROWER question than the run it models is worse
    than no dry run: an agent uses it to decide whether closing is safe, and it
    manufactures confidence.

    Measured 2026-08-25 on land_hermit_pr_2591: `--check-only --disposition` printed
    VERIFIED rc=0 while the identical command without the flag was REFUSED on the
    `landed` tag, because --check-only returned early in main() and never reached
    the gates in close_task(). These tests pin the AGREEMENT, not the individual
    answers, so a gate added in future to only one path fails here.
    """

    def _harness(self, tags: str):
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            if command[:2] == ("tg", "sql"):
                return task_sql_result(command, tags=tags)
            return _sp.CompletedProcess(list(command), 0, "", "")
        return run

    def _disposition(self):
        return verified_close.Evidence(
            "verified", "disposition", "superseded elsewhere",
            resolved="no-code-reference", landing="none",
        )

    def test_dry_run_and_real_run_agree_when_the_tag_gate_refuses(self) -> None:
        run = self._harness('["landed"]')
        dry = verified_close.close_task("t", self._disposition(), run=run, dry_run=True)
        real = verified_close.close_task("t", self._disposition(), run=run)
        self.assertEqual(dry, real)
        self.assertEqual(dry, verified_close.REFUSED)

    def test_dry_run_and_real_run_agree_when_it_would_succeed(self) -> None:
        run = self._harness('["review"]')
        dry = verified_close.close_task("t", self._disposition(), run=run, dry_run=True)
        real = verified_close.close_task("t", self._disposition(), run=run)
        self.assertEqual(dry, real)
        self.assertEqual(dry, verified_close.CLOSED)

    def test_the_dry_run_writes_nothing(self) -> None:
        """It must reach every gate and stop before the first mutation."""
        seen = []
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            if command[:2] == ("tg", "sql"):
                return task_sql_result(command, tags='["review"]')
            seen.append(tuple(command))
            return _sp.CompletedProcess(list(command), 0, "", "")
        verified_close.close_task("t", self._disposition(), run=run, dry_run=True)
        self.assertEqual(
            [c for c in seen if c and (c[0] == "tg" or str(c[0]).endswith("tg-note-verified"))],
            [],
            "the preview must not write a note or change status",
        )


class InterruptedClosureRetryIsIdempotent(unittest.TestCase):
    """A retry must finish an interrupted close without duplicating its record."""

    @staticmethod
    def _evidence_cases():
        return (
            verified_close.Evidence(
                "verified", "code", "a" * 40,
                resolved="rrnewton/hermit@" + "a" * 40, landing="landed",
            ),
            verified_close.Evidence(
                "verified", "artifact", "ai_docs/result.md",
                resolved="rrnewton/dev-hermit:ai_docs/result.md@" + "b" * 40,
            ),
            verified_close.Evidence(
                "verified", "run", "123",
                resolved="https://github.com/rrnewton/hermit/actions/runs/123",
            ),
            verified_close.Evidence(
                "verified", "disposition", "review completed",
                resolved="no-code-reference", landing="none",
            ),
            verified_close.Evidence(
                "verified", "superseded", "c" * 40,
                resolved="rrnewton/hermit@" + "c" * 40, landing="superseded",
            ),
        )

    def test_retry_after_sqlite_517_closes_with_one_exact_record(self) -> None:
        for evidence in self._evidence_cases():
            with self.subTest(kind=evidence.kind):
                runner = FakeRunner(tags='["review"]', status_failures=1)

                first = verified_close.close_task("t", evidence, run=runner)
                self.assertEqual(verified_close.UNVERIFIABLE, first)
                self.assertEqual("IN_PROGRESS", runner.status)
                self.assertEqual(1, len(runner.notes))

                second = verified_close.close_task("t", evidence, run=runner)
                self.assertEqual(verified_close.CLOSED, second)
                self.assertEqual("CLOSED", runner.status)
                self.assertEqual(1, len(runner.notes))
                self.assertEqual(
                    ["review", "landed"] if evidence.landing == "landed" else ["review"],
                    json.loads(runner.tags),
                )

    def test_unreadable_record_check_refuses_before_any_write(self) -> None:
        runner = FakeRunner(tags='["review"]')

        def fail_record_read(command, **kwargs):
            if command[:2] == ("tg", "sql") and "exact_note_count" in command[2]:
                return completed(command, rc=1, stderr="database unavailable")
            return runner(command, **kwargs)

        evidence = self._evidence_cases()[1]
        self.assertEqual(
            verified_close.UNVERIFIABLE,
            verified_close.close_task("t", evidence, run=fail_record_read),
        )
        self.assertEqual("IN_PROGRESS", runner.status)
        self.assertEqual([], runner.task_mutations)

    def test_a_different_closure_record_does_not_authorize_the_status_write(self) -> None:
        runner = FakeRunner(tags='["review"]')
        runner.notes.append(
            "CLOSURE-RECORDED: kind=disposition reference=a different result "
            "resolved=no-code-reference landing=none verifier=ci-hub/bin/close-task"
        )
        evidence = self._evidence_cases()[3]

        self.assertEqual(
            verified_close.CLOSED,
            verified_close.close_task("t", evidence, run=runner),
        )
        self.assertEqual("CLOSED", runner.status)
        self.assertEqual(2, len(runner.notes))
        self.assertNotEqual(runner.notes[0], runner.notes[1])
        self.assertEqual(1, runner.notes.count(runner.notes[-1]))

    def test_check_only_recognizes_recovery_state_without_writing(self) -> None:
        runner = FakeRunner(tags='["review"]', status_failures=1)
        evidence = self._evidence_cases()[3]
        self.assertEqual(
            verified_close.UNVERIFIABLE,
            verified_close.close_task("t", evidence, run=runner),
        )
        mutations_after_failure = list(runner.task_mutations)

        self.assertEqual(
            verified_close.CLOSED,
            verified_close.close_task("t", evidence, run=runner, dry_run=True),
        )
        self.assertEqual(mutations_after_failure, runner.task_mutations)
        self.assertEqual("IN_PROGRESS", runner.status)
        self.assertEqual(1, len(runner.notes))


class SupersededClosesALandedThenReplacedRow(unittest.TestCase):
    """`landed, then legitimately superseded` is what every fix-forward produces.

    Before this existed the row was unclosable without misrepresenting something:
    --code refused with CONTENT-LOST because a later commit rewrote the region, and
    --disposition refused because the task is tagged `landed`. The tool's advice --
    drop the tag then use --disposition -- meant recording a falsehood to satisfy a
    checker, since the original commit really is on main.
    """

    def _run(self, tags='["landed"]'):
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            if command[:2] == ("tg", "sql"):
                return task_sql_result(command, tags=tags)
            return _sp.CompletedProcess(list(command), 0, "", "")
        return run

    def test_superseded_is_not_blocked_by_the_landed_tag(self) -> None:
        ev = verified_close.Evidence(
            "verified", "superseded", "abc", resolved="r@abc", landing="superseded",
        )
        self.assertEqual(
            verified_close.close_task("t", ev, run=self._run()), verified_close.CLOSED
        )

    def test_superseded_records_the_replacing_commit_under_CLOSURE_VERIFIED(self) -> None:
        recorded = []
        notes = []
        def run(command, cwd=None, **kw):
            import subprocess as _sp
            if command[:2] == ("tg", "sql"):
                return task_sql_result(command, tags='["landed"]', notes=notes)
            if command and str(command[0]).endswith("tg-note-verified") and "--file" in command:
                path = list(command)[list(command).index("--file") + 1]
                with open(path, encoding="utf-8") as h:
                    content = h.read()
                    notes.append(content)
                    recorded.append(content)
            return _sp.CompletedProcess(list(command), 0, "", "")
        ev = verified_close.Evidence(
            "verified", "superseded", "abc", resolved="r@abc", landing="superseded",
        )
        verified_close.close_task("t", ev, run=run)
        self.assertTrue(recorded)
        body = recorded[0]
        # It IS verified evidence -- the replacing commit was proven to be on main --
        # so it belongs under CLOSURE-VERIFIED, unlike a disposition.
        self.assertIn("CLOSURE-VERIFIED", body)
        self.assertIn("kind=superseded", body)
        self.assertIn("landing=superseded", body)


# THE DISPATCHER MUST BE THE LAST THING IN THE FILE. It previously sat above
# ContentLostRefusesClosure, so running this file as a script -- which is how the seal and
# every caller runs it -- executed unittest.main() before that class was defined and
# collected 20 of 24 tests. The four it skipped included
# test_lossy_reconcile_is_REFUSED_by_the_closure_gateway, the consumer-level proof that
# CONTENT-LOST actually refuses a closure. It passed as a module and was never run as a
# script, which is the shape of a check that exists and does not fire.
class CommentPermalinkResolution(unittest.TestCase):
    """A comment permalink must be RESOLVED, not merely fetched.

    ⚠️ THE FRAGMENT IS THE WHOLE PAYLOAD AND `curl` CANNOT SEE IT. A URL fragment
    is never sent to the server, so `curl --fail` on
    .../pull/2623#issuecomment-<id> retrieves the PULL REQUEST PAGE and returns
    200 for ANY id. Measured against a real id, a typo'd id, and the literal
    99999999999 -- all three 200. The check confirmed the pull request existed,
    which nobody doubted, while reporting that the COMMENT was verified.
    """

    URL = "https://github.com/rrnewton/hermit/pull/2623#issuecomment-5416878194"

    @staticmethod
    def _run(rc, out="", err=""):
        def run(cmd, cwd=None):
            return subprocess.CompletedProcess(cmd, rc, out, err)
        return run

    def test_a_resolving_comment_verifies(self) -> None:
        ev = verified_close.verify_comment_permalink(self.URL, run=self._run(0, "5416878194"))
        self.assertEqual("verified", ev.state)

    def test_a_comment_that_does_not_exist_is_REFUSED(self) -> None:
        ev = verified_close.verify_comment_permalink(
            self.URL, run=self._run(1, "", "gh: Not Found (HTTP 404)"))
        self.assertEqual("refused", ev.state)
        self.assertIn("does not exist", ev.reason)
        self.assertIn("Remedy", ev.reason)

    def test_could_not_ask_is_NOT_a_pass_and_NOT_a_404(self) -> None:
        ev = verified_close.verify_comment_permalink(
            self.URL, run=self._run(1, "", "dial tcp: network is unreachable"))
        self.assertEqual("unverifiable", ev.state)
        self.assertIn("NOT a pass", ev.reason)

    def test_a_non_permalink_is_left_to_the_existing_path(self) -> None:
        self.assertIsNone(verified_close.verify_comment_permalink(
            "https://github.com/rrnewton/hermit/pull/2623", run=self._run(0)))

    def test_the_resolver_is_WIRED_INTO_verify_artifact(self) -> None:
        """⚠️ Every other test here calls the resolver DIRECTLY, so none of them
        notice if `verify_artifact` stops calling it. Caught by mutation: replacing
        the call with `anchored = None` -- reverting to the curl path that cannot
        see a fragment -- left all of them green. This one enters at the real
        entry point.
        """
        ev = verified_close.verify_artifact(self.URL, run=self._run(1, "", "gh: Not Found (HTTP 404)"))
        self.assertEqual("refused", ev.state)
        # curl would have said verified here; that is the whole regression.
        self.assertNotEqual("verified", ev.state)

    def test_verify_artifact_still_verifies_a_live_comment(self) -> None:
        ev = verified_close.verify_artifact(self.URL, run=self._run(0, "5416878194"))
        self.assertEqual("verified", ev.state)

    def test_review_and_discussion_anchors_use_their_own_endpoints(self) -> None:
        seen = {}

        def run(cmd, cwd=None):
            seen["ep"] = cmd[3]
            return subprocess.CompletedProcess(cmd, 0, "1", "")

        verified_close.verify_comment_permalink(
            "https://github.com/o/r/pull/7#pullrequestreview-42", run=run)
        self.assertEqual("repos/o/r/pulls/7/reviews/42", seen["ep"])
        verified_close.verify_comment_permalink(
            "https://github.com/o/r/pull/7#discussion_r99", run=run)
        self.assertEqual("repos/o/r/pulls/comments/99", seen["ep"])



class PrivateRepositoryArtifactsAreReachable(unittest.TestCase):
    """⚠️ ANONYMOUS `curl` CANNOT SEE A PRIVATE REPOSITORY AT ALL.

    GitHub answers an unauthenticated request for a private object with 404 and
    `curl --fail` turns that into a nonzero exit, so every artifact citing
    `rrnewton/dev-hermit` -- which is private -- was `unverifiable`, with a reason
    naming the URL rather than the missing credential. Measured 2026-08-26:
    anonymous curl on that repository exits 22, `gh api` exits 0. The public
    repositories were unaffected, which is why it survived.

    `verify_comment_permalink` already fixed the comment shape; these cover the
    shapes it does not.
    """

    def test_a_github_pull_url_is_probed_with_credentials(self) -> None:
        seen = []

        def record(command, **kwargs):
            seen.append(tuple(command))
            return subprocess.CompletedProcess(list(command), 0, "", "")

        evidence = verified_close.verify_artifact(
            "https://github.com/owner/private/pull/7", run=record
        )
        self.assertEqual("verified", evidence.state)
        self.assertEqual(("with-proxy", "gh", "api", "repos/owner/private/pulls/7"), seen[0])
        self.assertNotIn("curl", " ".join(seen[0]))

    def test_a_failed_probe_does_not_retry_anonymously(self) -> None:
        """The retry would answer WRONGLY: a private object 404s to `curl`, and
        that 404 reads as absence rather than as a missing credential."""
        seen = []

        def failing(command, **kwargs):
            seen.append(tuple(command))
            return subprocess.CompletedProcess(list(command), 1, "", "gh: Not Found (HTTP 404)")

        evidence = verified_close.verify_artifact(
            "https://github.com/owner/private/pull/7", run=failing
        )
        self.assertEqual("unverifiable", evidence.state)
        self.assertEqual(1, len(seen), "must not fall back to anonymous curl")
        self.assertIn("gh api", evidence.reason)

    def test_a_non_github_url_still_uses_curl(self) -> None:
        seen = []

        def record(command, **kwargs):
            seen.append(tuple(command))
            return subprocess.CompletedProcess(list(command), 0, "", "")

        self.assertEqual(
            "verified",
            verified_close.verify_artifact("https://example.com/a.txt", run=record).state,
        )
        self.assertIn("curl", " ".join(seen[0]))

    def test_mapped_shapes_are_exact_and_unknown_shapes_are_not_guessed(self) -> None:
        for url, want in (
            ("https://github.com/o/r", "repos/o/r"),
            ("https://github.com/o/r/pull/9", "repos/o/r/pulls/9"),
            ("https://github.com/o/r/issues/9", "repos/o/r/issues/9"),
            ("https://github.com/o/r/commit/deadbeef", "repos/o/r/commits/deadbeef"),
            ("https://github.com/o/r/blob/main/README.md", None),
            ("https://example.com/o/r/pull/9", None),
        ):
            with self.subTest(url=url):
                self.assertEqual(want, verified_close.github_object_endpoint(url))


if __name__ == "__main__":
    unittest.main()
