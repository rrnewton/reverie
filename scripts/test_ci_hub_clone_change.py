#!/usr/bin/env python3
"""Focused refusal/acceptance tests for ci-hub-clone-change.py."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SOURCE = Path(__file__).with_name("ci-hub-clone-change.py")
SPEC = importlib.util.spec_from_file_location("ci_hub_clone_change", SOURCE)
assert SPEC and SPEC.loader
TOOL = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TOOL
SPEC.loader.exec_module(TOOL)


def command(*argv: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, cwd=cwd, text=True, capture_output=True, check=False)


def git(repo: Path, *args: str) -> str:
    result = command("git", "-C", str(repo), *args)
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return result.stdout.strip()


def activation_row() -> dict[str, object]:
    return {
        "schema": TOOL.ACTIVATION_SCHEMA,
        "event_id": "c" * 32,
        "event_type": "opened",
        "recorded_at": "2026-08-28T08:15:00Z",
        "obligation_id": "20260828-081500-aaaaaaaaaaaa-abcdef",
        "task": "typed-activation-obligation",
        "candidate": "a" * 40,
        "primary": "/tmp/primary",
        "changed_paths": ["ci-hub/a", "ci-hub/b"],
        "diff_sha256": "b" * 64,
        "opened_at": "2026-08-28T08:15:00Z",
        "updated_at": "2026-08-28T08:15:00Z",
        "overall_state": "open",
        "satisfied_at": None,
        "activation": {
            "state": "awaiting_publication",
            "attempts": 0,
            "last_attempt_at": None,
            "last_reason": None,
            "activated_head": None,
        },
    }


class ActivationObligationContract(unittest.TestCase):
    def test_complete_row_round_trips(self) -> None:
        row = activation_row()
        parsed = TOOL.ActivationObligation.from_mapping(row)
        self.assertEqual(parsed.to_dict(), row)
        self.assertEqual(parsed.candidate, "a" * 40)
        self.assertEqual(parsed.activation.state, "awaiting_publication")

    def test_missing_and_unknown_fields_fail_by_name(self) -> None:
        missing = activation_row()
        del missing["diff_sha256"]
        with self.assertRaisesRegex(TOOL.Refusal, "diff_sha256"):
            TOOL.ActivationObligation.from_mapping(missing)

        unknown = activation_row()
        unknown["assumed_active"] = False
        with self.assertRaisesRegex(TOOL.Refusal, "assumed_active"):
            TOOL.ActivationObligation.from_mapping(unknown)

    def test_new_event_type_fails_by_name(self) -> None:
        row = activation_row()
        row["event_type"] = "future_event"
        with self.assertRaisesRegex(TOOL.Refusal, "event_type"):
            TOOL.ActivationObligation.from_mapping(row)

    def test_newer_activation_schema_is_refused_before_its_shape(self) -> None:
        row = activation_row()
        row["schema"] = "ci-hub-change-activation-obligation/v2"
        row["new_field"] = True
        with self.assertRaisesRegex(TOOL.Refusal, "schema") as caught:
            TOOL.ActivationObligation.from_mapping(row)
        self.assertNotIn("new_field", str(caught.exception))

    def test_event_and_state_disagreement_fails_by_name(self) -> None:
        row = activation_row()
        activation = row["activation"]
        assert isinstance(activation, dict)
        activation["state"] = "pending"
        with self.assertRaisesRegex(TOOL.Refusal, "event_type"):
            TOOL.ActivationObligation.from_mapping(row)

    def test_immutable_identity_drift_across_events_fails_by_name(self) -> None:
        first = activation_row()
        second = activation_row()
        second["event_id"] = "d" * 32
        second["task"] = "different-task"
        handle = io.StringIO(json.dumps(first) + "\n" + json.dumps(second) + "\n")
        with self.assertRaisesRegex(TOOL.Refusal, "identity changed"):
            TOOL._activation_latest_from_handle(handle)


class ChangeManifestSealContract(unittest.TestCase):
    def test_complete_manifest_and_seal_round_trip(self) -> None:
        manifest = {
            "schema": TOOL.SCHEMA,
            "task": "typed-change-manifest",
            "clone": "/tmp/change-clone",
            "created_at": "2026-08-28T08:30:00+00:00",
            "initial_base": "a" * 40,
            "claims": ["ci-hub/a", "ci-hub/b"],
        }
        seal = {
            "schema": TOOL.SCHEMA,
            "task": "typed-change-manifest",
            "base": "a" * 40,
            "candidate": "b" * 40,
            "changed_paths": ["ci-hub/a", "ci-hub/b"],
            "diff_sha256": "c" * 64,
            "tests": [{"command": "python3 -m unittest", "exit_code": 0}],
            "sealed_at": "2026-08-28T08:31:00+00:00",
        }
        self.assertEqual(TOOL.ChangeManifest.from_mapping(manifest).to_dict(), manifest)
        self.assertEqual(TOOL.ChangeSeal.from_mapping(seal).to_dict(), seal)

    def test_manifest_missing_and_unknown_fields_fail_by_name(self) -> None:
        missing = {
            "schema": TOOL.SCHEMA,
            "task": "typed-change-manifest",
            "clone": "/tmp/change-clone",
            "created_at": "2026-08-28T08:30:00+00:00",
            "claims": ["ci-hub/a"],
        }
        with self.assertRaisesRegex(TOOL.Refusal, "initial_base"):
            TOOL.ChangeManifest.from_mapping(missing)
        missing["initial_base"] = "a" * 40
        missing["unreviewed"] = True
        with self.assertRaisesRegex(TOOL.Refusal, "unreviewed"):
            TOOL.ChangeManifest.from_mapping(missing)

    def test_newer_manifest_schema_is_refused_before_its_shape(self) -> None:
        manifest = {
            "schema": "ci-hub-change/v2",
            "task": "typed-change-manifest",
            "clone": "/tmp/change-clone",
            "created_at": "2026-08-28T08:30:00+00:00",
            "initial_base": "a" * 40,
            "claims": ["ci-hub/a"],
            "new_field": True,
        }
        with self.assertRaisesRegex(TOOL.Refusal, "schema") as caught:
            TOOL.ChangeManifest.from_mapping(manifest)
        self.assertNotIn("new_field", str(caught.exception))

    def test_seal_refuses_an_unsuccessful_or_incomplete_test_by_name(self) -> None:
        seal = {
            "schema": TOOL.SCHEMA,
            "task": "typed-change-manifest",
            "base": "a" * 40,
            "candidate": "b" * 40,
            "changed_paths": ["ci-hub/a"],
            "diff_sha256": "c" * 64,
            "tests": [{"command": "false", "exit_code": 1}],
            "sealed_at": "2026-08-28T08:31:00+00:00",
        }
        with self.assertRaisesRegex(TOOL.Refusal, "tests-exit_code"):
            TOOL.ChangeSeal.from_mapping(seal)
        seal["tests"] = [{"command": "true"}]
        with self.assertRaisesRegex(TOOL.Refusal, "exit_code"):
            TOOL.ChangeSeal.from_mapping(seal)

    def test_seal_unknown_field_fails_by_name(self) -> None:
        seal = {
            "schema": TOOL.SCHEMA,
            "task": "typed-change-manifest",
            "base": "a" * 40,
            "candidate": "b" * 40,
            "changed_paths": ["ci-hub/a"],
            "diff_sha256": "c" * 64,
            "tests": [{"command": "true", "exit_code": 0}],
            "sealed_at": "2026-08-28T08:31:00+00:00",
            "assumed_clean": True,
        }
        with self.assertRaisesRegex(TOOL.Refusal, "assumed_clean"):
            TOOL.ChangeSeal.from_mapping(seal)


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.remote = self.root / "remote.git"
        self.seed = self.root / "seed"
        command("git", "init", "--bare", str(self.remote))
        command("git", "init", "-b", "main", str(self.seed))
        git(self.seed, "config", "user.name", "Test Agent")
        git(self.seed, "config", "user.email", "test@example.invalid")
        (self.seed / "ci-hub/bin").mkdir(parents=True)
        (self.seed / "ci-hub/bin/tool").write_text("old\n")
        (self.seed / "outside.txt").write_text("old\n")
        (self.seed / "scripts").mkdir()
        (self.seed / "scripts/setup-hooks.sh").write_text("#!/bin/sh\nexit 0\n")
        (self.seed / "scripts/setup-hooks.sh").chmod(0o755)
        (self.seed / "scripts/parent-main-write").write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "case \"$1\" in\n"
            "  publish)\n"
            "    if [ \"${FIXTURE_REFUSE_BEFORE_PUSH:-0}\" = 1 ]; then\n"
            "      echo 'fixture refused before push' >&2\n"
            "      exit 17\n"
            "    fi\n"
            "    git push origin \"${2:-HEAD}:main\"\n"
            "    if [ \"${FIXTURE_REFUSE_AFTER_PUSH:-0}\" = 1 ]; then\n"
            "      echo 'fixture refused after push' >&2\n"
            "      exit 18\n"
            "    fi\n"
            "    ;;\n"
            "  sync) git fetch origin main && git merge --ff-only origin/main ;;\n"
            "  *) exit 64 ;;\n"
            "esac\n"
        )
        (self.seed / "scripts/parent-main-write").chmod(0o755)

        self.agent_utils_remote = self.root / "agent-utils-remote.git"
        self.agent_utils_seed = self.root / "agent-utils-seed"
        command("git", "init", "--bare", str(self.agent_utils_remote))
        command("git", "init", "-b", "main", str(self.agent_utils_seed))
        git(self.agent_utils_seed, "config", "user.name", "Test Agent")
        git(self.agent_utils_seed, "config", "user.email", "test@example.invalid")
        (self.agent_utils_seed / "payload.txt").write_text("from hermit agent-utils\n")
        git(self.agent_utils_seed, "add", "payload.txt")
        git(self.agent_utils_seed, "commit", "-m", "agent-utils seed")
        git(self.agent_utils_seed, "remote", "add", "origin", str(self.agent_utils_remote))
        git(self.agent_utils_seed, "push", "-u", "origin", "main")

        self.hermit_remote = self.root / "hermit-remote.git"
        self.hermit_seed = self.root / "hermit-seed"
        command("git", "init", "--bare", str(self.hermit_remote))
        command("git", "init", "-b", "main", str(self.hermit_seed))
        git(self.hermit_seed, "config", "user.name", "Test Agent")
        git(self.hermit_seed, "config", "user.email", "test@example.invalid")
        (self.hermit_seed / "README").write_text("hermit fixture\n")
        git(self.hermit_seed, "add", "README")
        git(self.hermit_seed, "commit", "-m", "hermit seed")
        env = {**os.environ, **self.FILE_PROTOCOL_ENV}
        added = subprocess.run(
            ["git", "-C", str(self.hermit_seed), "submodule", "add",
             str(self.agent_utils_remote), "agent-utils"],
            capture_output=True, text=True, env=env,
        )
        self.assertEqual(added.returncode, 0, added.stderr)
        git(self.hermit_seed, "commit", "-m", "add agent-utils")
        git(self.hermit_seed, "remote", "add", "origin", str(self.hermit_remote))
        git(self.hermit_seed, "push", "-u", "origin", "main")
        command("git", "--git-dir", str(self.hermit_remote), "symbolic-ref",
                "HEAD", "refs/heads/main")

        added = subprocess.run(
            ["git", "-C", str(self.seed), "submodule", "add",
             str(self.hermit_remote), "hermit"],
            capture_output=True, text=True, env=env,
        )
        self.assertEqual(added.returncode, 0, added.stderr)
        (self.seed / "agent-utils").symlink_to("hermit/agent-utils")
        git(
            self.seed,
            "add",
            "agent-utils",
            "ci-hub/bin/tool",
            "outside.txt",
            "scripts/setup-hooks.sh",
            "scripts/parent-main-write",
        )
        git(self.seed, "commit", "-m", "seed")
        git(self.seed, "remote", "add", "origin", str(self.remote))
        git(self.seed, "push", "-u", "origin", "main")
        command("git", "--git-dir", str(self.remote), "symbolic-ref", "HEAD", "refs/heads/main")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def clone(self, name: str = "change") -> Path:
        path = self.root / name
        result = command(
            str(SOURCE), "create", "--task", "fixture-change", "--path",
            "ci-hub/bin", "--clone", str(path), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        git(path, "config", "user.name", "Test Agent")
        git(path, "config", "user.email", "test@example.invalid")
        return path

    def commit(self, repo: Path, path: str, text: str) -> str:
        (repo / path).write_text(text)
        git(repo, "add", path)
        git(repo, "commit", "-m", "candidate")
        return git(repo, "rev-parse", "HEAD")

    # ---- submodules, because the repository's own tests import them ----
    #
    # Local paths as submodule URLs need `protocol.file.allow`, which real
    # remotes do not; that is a fixture accommodation, passed through the
    # environment so the tool under test is invoked exactly as in production.

    FILE_PROTOCOL_ENV = {
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "protocol.file.allow",
        "GIT_CONFIG_VALUE_0": "always",
    }

    def add_submodule(self, name: str = "sub", payload: str = "from the submodule\n") -> Path:
        """Give the fixture remote a submodule carrying one recognizable file."""
        inner_remote = self.root / f"{name}-remote.git"
        inner = self.root / f"{name}-seed"
        command("git", "init", "--bare", str(inner_remote))
        command("git", "init", "-b", "main", str(inner))
        git(inner, "config", "user.name", "Test Agent")
        git(inner, "config", "user.email", "test@example.invalid")
        (inner / "payload.txt").write_text(payload)
        git(inner, "add", "payload.txt")
        git(inner, "commit", "-m", "submodule seed")
        git(inner, "remote", "add", "origin", str(inner_remote))
        git(inner, "push", "-u", "origin", "main")
        env = {**os.environ, **self.FILE_PROTOCOL_ENV}
        added = subprocess.run(
            ["git", "-C", str(self.seed), "submodule", "add", str(inner_remote), name],
            capture_output=True, text=True, env=env,
        )
        self.assertEqual(added.returncode, 0, added.stderr)
        git(self.seed, "commit", "-m", "add submodule")
        git(self.seed, "push", "origin", "main")
        return inner_remote

    def create_clone(self, name: str, *, env: dict[str, str] | None = None):
        return subprocess.run(
            [str(SOURCE), "create", "--task", "fixture-change", "--path", "ci-hub/bin",
             "--clone", str(self.root / name), "--remote", str(self.remote)],
            capture_output=True, text=True,
            env={**os.environ, **self.FILE_PROTOCOL_ENV, **(env or {})},
        )

    def publish_hermit_change(self) -> None:
        git(self.hermit_seed, "push", "origin", "main")
        git(self.seed / "hermit", "fetch", "origin", "main")
        git(self.seed / "hermit", "checkout", "--detach", "origin/main")
        git(self.seed, "add", "hermit")
        git(self.seed, "commit", "-m", "advance hermit")
        git(self.seed, "push", "origin", "main")

    def test_create_checks_out_submodules_so_the_repo_tests_can_run(self) -> None:
        self.add_submodule()
        result = self.create_clone("with-sub")
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.root / "with-sub"
        # The file the tests would import must be THERE, not merely the gitlink.
        self.assertTrue((path / "sub" / "payload.txt").is_file())
        self.assertEqual((path / "sub" / "payload.txt").read_text(), "from the submodule\n")
        self.assertEqual(TOOL.uninitialized_submodules(path), [])
        self.assertTrue((path / "agent-utils").is_symlink())
        self.assertTrue((path / "agent-utils" / "payload.txt").is_file())
        self.assertEqual(TOOL.hermit_agent_utils_problem(path), None)
        # `create` refuses an unclean fresh clone, so the targeted nested update
        # must not dirty either the parent or Hermit.
        self.assertTrue(TOOL.is_clean(path))
        self.assertEqual(git(path / "hermit", "status", "--porcelain"), "")

    def test_create_refuses_rather_than_producing_a_clone_missing_a_submodule(self) -> None:
        """The failure that "stops erroring" would hide: a silently absent submodule.

        `submodule.<name>.active=false` makes `git clone --recurse-submodules`
        SUCCEED while skipping that submodule, which is a real silent-skip path
        and not a contrived one. Without the post-condition the tool would hand
        back a clone that looks fine and cannot run the repository's tests.

        An earlier version of this test used `submodule.recurse=false`, which the
        explicit command-line flag overrides, so recursion happened anyway and
        the test passed without ever reaching the guard.
        """
        self.add_submodule()
        result = self.create_clone(
            "no-sub", env={"GIT_CONFIG_COUNT": "2",
                           "GIT_CONFIG_KEY_0": "protocol.file.allow",
                           "GIT_CONFIG_VALUE_0": "always",
                           "GIT_CONFIG_KEY_1": "submodule.sub.active",
                           "GIT_CONFIG_VALUE_1": "false"},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uninitialized", result.stderr)
        self.assertIn("cannot run the repository's own tests", result.stderr)
        self.assertIn("sub", result.stderr)
        # The clone git did produce is missing exactly what the tests import.
        self.assertFalse((self.root / "no-sub" / "sub" / "payload.txt").is_file())

    def test_create_refuses_when_nested_agent_utils_is_silently_skipped(self) -> None:
        git(
            self.hermit_seed,
            "config",
            "-f",
            ".gitmodules",
            "submodule.agent-utils.update",
            "none",
        )
        git(self.hermit_seed, "add", ".gitmodules")
        git(self.hermit_seed, "commit", "-m", "silently skip nested agent-utils")
        self.publish_hermit_change()

        result = self.create_clone("nested-skip")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("hermit/agent-utils unavailable", result.stderr)
        self.assertIn("uninitialized", result.stderr)
        self.assertIn("exposes no files", result.stderr)
        clone = self.root / "nested-skip"
        self.assertTrue((clone / "agent-utils").is_symlink())
        self.assertFalse((clone / "agent-utils" / "payload.txt").is_file())
        self.assertFalse((clone / "hermit" / "agent-utils" / "payload.txt").is_file())

    def test_create_refuses_a_wrong_parent_agent_utils_symlink(self) -> None:
        link = self.seed / "agent-utils"
        link.unlink()
        link.symlink_to("hermit/not-agent-utils")
        git(self.seed, "add", "agent-utils")
        git(self.seed, "commit", "-m", "break agent-utils symlink")
        git(self.seed, "push", "origin", "main")

        result = self.create_clone("dangling-link")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid agent-utils path", result.stderr)
        self.assertIn("not 'hermit/agent-utils'", result.stderr)
        clone = self.root / "dangling-link"
        self.assertTrue((clone / "agent-utils").is_symlink())
        self.assertFalse((clone / "agent-utils").exists())

    def test_uninitialized_submodules_reads_the_status_prefix(self) -> None:
        self.add_submodule()
        result = self.create_clone("prefix")
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.root / "prefix"
        self.assertEqual(TOOL.uninitialized_submodules(path), [])
        # Deinit reproduces exactly what the guard must catch.
        git(path, "submodule", "deinit", "-f", "sub")
        self.assertEqual(TOOL.uninitialized_submodules(path), ["sub"])

    def test_positive_one_commit_claimed_and_tested_seals(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "new\n")
        result = command(
            str(SOURCE),
            "seal",
            "--clone",
            str(repo),
            "--test",
            "test -f ci-hub/bin/tool",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        seal = json.loads((repo / ".git" / TOOL.SEAL).read_text())
        self.assertEqual(seal["candidate"], candidate)
        self.assertEqual(seal["changed_paths"], ["ci-hub/bin/tool"])
        self.assertEqual(seal["tests"][0]["exit_code"], 0)

    def test_positive_published_candidate_keeps_valid_activation_seal(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "published\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        git(repo, "push", "origin", "HEAD:main")
        manifest, seal = TOOL.verified_seal(repo, require_unpublished_base=False)
        self.assertEqual(manifest.task, "fixture-change")
        self.assertEqual(seal.candidate, candidate)

    def test_negative_published_candidate_is_not_publishable_twice(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "published\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        git(repo, "push", "origin", "HEAD:main")
        with self.assertRaisesRegex(TOOL.Refusal, "sealed base is stale"):
            TOOL.verified_seal(repo, require_unpublished_base=True)

    def test_publish_refusal_before_push_cancels_obligation(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        sealed = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(sealed.returncode, 0, sealed.stderr)
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        store = self.root / "activation-obligations.jsonl"

        refused = command(
            "env",
            "FIXTURE_REFUSE_BEFORE_PUSH=1",
            str(SOURCE),
            "publish",
            "--clone",
            str(repo),
            "--primary",
            str(primary),
            "--store",
            str(store),
        )
        self.assertEqual(refused.returncode, 2)
        record = next(iter(TOOL.activation_latest(store).values()))
        self.assertEqual(record.candidate, candidate)
        self.assertEqual(record.overall_state, "cancelled")
        self.assertEqual(record.activation.state, "publication_refused")
        self.assertEqual(record.event_type, "publication_refused")

    def test_publish_refusal_after_push_keeps_obligation_open(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        sealed = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(sealed.returncode, 0, sealed.stderr)
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        store = self.root / "activation-obligations.jsonl"

        refused = command(
            "env",
            "FIXTURE_REFUSE_AFTER_PUSH=1",
            str(SOURCE),
            "publish",
            "--clone",
            str(repo),
            "--primary",
            str(primary),
            "--store",
            str(store),
        )
        self.assertEqual(refused.returncode, 2)
        git(repo, "fetch", "origin", "main")
        self.assertEqual(git(repo, "rev-parse", "origin/main"), candidate)
        record = next(iter(TOOL.activation_latest(store).values()))
        self.assertEqual(record.overall_state, "open")
        self.assertEqual(record.activation.state, "pending")
        self.assertEqual(record.event_type, "publication_confirmed_after_refusal")

    def test_negative_unclaimed_path_refuses(self) -> None:
        repo = self.clone()
        self.commit(repo, "outside.txt", "not allowed\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unclaimed paths", result.stderr)

    def test_negative_missing_test_refuses(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "new\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo))
        self.assertEqual(result.returncode, 2)
        self.assertIn("at least one --test", result.stderr)

    def test_negative_two_commits_refuse(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "one\n")
        self.commit(repo, "ci-hub/bin/tool", "two\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 2)
        self.assertIn("exactly one commit", result.stderr)

    def test_negative_remote_move_invalidates_candidate(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        (self.seed / "other.txt").write_text("remote moved\n")
        git(self.seed, "add", "other.txt")
        git(self.seed, "commit", "-m", "move remote")
        git(self.seed, "push", "origin", "main")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 2)
        self.assertIn("current origin/main", result.stderr)

    def test_negative_existing_destination_preserved(self) -> None:
        occupied = self.root / "occupied"
        occupied.mkdir()
        marker = occupied / "preserve"
        marker.write_text("owned\n")
        result = command(
            str(SOURCE), "create", "--task", "second-change", "--path",
            "ci-hub/bin", "--clone", str(occupied), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(marker.read_text(), "owned\n")

    def test_negative_dirty_primary_candidate_path_refuses(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        primary = self.root / "primary"
        command("git", "clone", str(self.remote), str(primary))
        (primary / "ci-hub/bin/tool").write_text("somebody else\n")
        self.assertEqual(len(TOOL.dirty_candidate_paths(primary, ["ci-hub/bin/tool"])), 1)

    def test_positive_activation_accepts_later_containing_main(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        git(repo, "push", "origin", "HEAD:main")
        git(self.seed, "pull", "--ff-only", "origin", "main")
        (self.seed / "later.txt").write_text("later complete commit\n")
        git(self.seed, "add", "later.txt")
        git(self.seed, "commit", "-m", "later")
        latest = git(self.seed, "rev-parse", "HEAD")
        git(self.seed, "push", "origin", "main")
        result = command(
            str(SOURCE),
            "activate",
            "--clone",
            str(repo),
            "--primary",
            str(primary),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(git(primary, "rev-parse", "HEAD"), latest)
        self.assertEqual(git(primary, "merge-base", "--is-ancestor", candidate, latest), "")

    def test_negative_activation_refuses_noncontaining_main(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        (self.seed / "sibling.txt").write_text("noncontaining commit\n")
        git(self.seed, "add", "sibling.txt")
        git(self.seed, "commit", "-m", "sibling")
        git(self.seed, "push", "origin", "main")
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        result = command(
            str(SOURCE),
            "activate",
            "--clone",
            str(repo),
            "--primary",
            str(primary),
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("current main to contain candidate", result.stderr)

    # ---- what the dirty-paths refusal SAYS. It always refuses; only the stated
    # cause moves, because `git status` reporting an entry covers two situations
    # whose right response is opposite. Recorded live at 2026-08-15T22:51:06Z in
    # the activation obligation store: `A  <path> | M  <path> | M  <path>`, every
    # second column blank, described as dirty paths that did not exist.

    def _published_candidate(self, name: str = "change",
                             path: str = "ci-hub/bin/tool") -> tuple[Path, Path, str, str]:
        """A sealed candidate that is on main, and a primary that predates it."""
        repo = self.clone(name)
        # Content must differ per call: a later call clones a main that already
        # holds the earlier one, and an identical write commits nothing.
        candidate = self.commit(repo, path, f"candidate {name}\n")
        sealed = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(sealed.returncode, 0, sealed.stderr)
        primary = self.root / f"primary-{name}"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        git(repo, "push", "origin", "HEAD:main")
        return repo, primary, candidate, path

    def _activate(self, repo: Path, primary: Path) -> subprocess.CompletedProcess[str]:
        return command(str(SOURCE), "activate", "--clone", str(repo),
                       "--primary", str(primary))

    def test_a_mid_sync_index_is_not_described_as_dirty_paths(self) -> None:
        repo, primary, candidate, _ = self._published_candidate()
        # Index and working tree already carry the candidate while HEAD has not
        # moved: exactly the shape a concurrent parent-main-write sync presents.
        git(primary, "fetch", "origin")
        git(primary, "checkout", candidate, "--", "ci-hub/bin/tool")
        entries = TOOL.dirty_candidate_paths(primary, ["ci-hub/bin/tool"])
        self.assertEqual(len(entries), 1)
        self.assertEqual(entries[0][1], " ")  # nothing unsaved
        result = self._activate(repo, primary)
        # STILL REFUSES. The safe behaviour is not what is being changed.
        self.assertEqual(result.returncode, 2)
        self.assertIn("already hold this candidate's content", result.stderr)
        self.assertIn("re-run activation once the primary settles", result.stderr)
        self.assertNotIn("inspect and preserve it first", result.stderr)

    def test_genuinely_unsaved_work_is_still_described_as_work_to_preserve(self) -> None:
        repo, primary, _, _ = self._published_candidate()
        (primary / "ci-hub/bin/tool").write_text("somebody else's unsaved edit\n")
        result = self._activate(repo, primary)
        self.assertEqual(result.returncode, 2)
        self.assertIn("inspect and preserve it first", result.stderr)
        self.assertNotIn("already hold this candidate", result.stderr)

    def test_staged_content_that_is_not_the_candidate_is_work_to_preserve(self) -> None:
        """The adversarial case: blank second column, but somebody else's content.

        Column shape alone would call this a sync in flight and tell the reader
        there is nothing to preserve, which is the very error being fixed.
        """
        repo, primary, _, _ = self._published_candidate()
        (primary / "ci-hub/bin/tool").write_text("somebody else's staged work\n")
        git(primary, "add", "--", "ci-hub/bin/tool")
        entries = TOOL.dirty_candidate_paths(primary, ["ci-hub/bin/tool"])
        self.assertEqual(entries[0][1], " ")  # indistinguishable by columns alone
        result = self._activate(repo, primary)
        self.assertEqual(result.returncode, 2)
        self.assertIn("inspect and preserve it first", result.stderr)
        self.assertNotIn("already hold this candidate", result.stderr)

    def test_an_untracked_file_at_a_candidate_path_is_work_to_preserve(self) -> None:
        """The candidate ADDS a path the primary already holds untracked.

        Scoping note established while writing this: the check reads the seal's
        exact changed files, not the manifest's claimed directory, so an
        untracked SIBLING under `ci-hub/bin` never reaches it. Only a collision
        at a changed path itself does.
        """
        repo, primary, _, path = self._published_candidate(name="added", path="ci-hub/bin/fresh")
        (primary / path).write_text("somebody else's untracked file\n")
        result = self._activate(repo, primary)
        self.assertEqual(result.returncode, 2)
        self.assertIn("inspect and preserve it first", result.stderr)
        self.assertNotIn("already hold this candidate", result.stderr)

    def test_unsaved_edits_on_top_of_candidate_content_are_work_to_preserve(self) -> None:
        """The case where getting the wording wrong would cost somebody their work.

        The index holds the candidate, so "whose content is this" answers
        "ours" -- but the working tree has unsaved edits on top of it. Telling
        that reader there is nothing to preserve is the one wrong answer with a
        real cost, and only the second porcelain column distinguishes it.
        """
        repo, primary, candidate, _ = self._published_candidate(name="ontop")
        git(primary, "fetch", "origin")
        git(primary, "checkout", candidate, "--", "ci-hub/bin/tool")
        (primary / "ci-hub/bin/tool").write_text("unsaved edit on top\n")
        entries = TOOL.dirty_candidate_paths(primary, ["ci-hub/bin/tool"])
        self.assertEqual(entries[0][:2], "MM")  # staged AND unsaved
        result = self._activate(repo, primary)
        self.assertEqual(result.returncode, 2)
        self.assertIn("inspect and preserve it first", result.stderr)
        self.assertNotIn("already hold this candidate", result.stderr)

    def test_both_wordings_defer_the_obligation_identically(self) -> None:
        """Wording is the only thing that moved: each still records a deferral."""
        for label, prepare in (
            ("midsync", lambda p, c: (git(p, "fetch", "origin"),
                                      git(p, "checkout", c, "--", "ci-hub/bin/tool"))),
            ("realdirt", lambda p, c: (p / "ci-hub/bin/tool").write_text("theirs\n")),
        ):
            with self.subTest(label):
                repo, primary, candidate, _ = self._published_candidate(name=label)
                store = self.root / f"obligations-{label}.jsonl"
                prepare(primary, candidate)
                result = command(str(SOURCE), "activate", "--clone", str(repo),
                                 "--primary", str(primary), "--store", str(store))
                self.assertEqual(result.returncode, 2)
                record = next(iter(TOOL.activation_latest(store).values()))
                self.assertEqual(record.event_type, "activation_deferred")
                self.assertEqual(record.overall_state, "open")
                self.assertEqual(record.activation.state, "deferred")
                shutil.rmtree(primary)

    def test_deferred_activation_remains_open_until_primary_contains_it(self) -> None:
        """A correct refusal is durable, and later completion closes it."""
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        store = self.root / "activation-obligations.jsonl"

        published = command(
            str(SOURCE),
            "publish",
            "--clone",
            str(repo),
            "--primary",
            str(primary),
            "--store",
            str(store),
        )
        self.assertEqual(published.returncode, 0, published.stderr)
        records = TOOL.activation_latest(store)
        self.assertEqual(len(records), 1)
        opened = next(iter(records.values()))
        self.assertEqual(opened.candidate, candidate)
        self.assertEqual(opened.overall_state, "open")

        # Model parent-main-write's correct dirty-tree refusal without weakening
        # that separately-tested guard. The activation wrapper must retain it.
        writer = primary / "scripts/parent-main-write"
        writer.write_text(
            "#!/bin/sh\n"
            "echo 'parent-main-write: REFUSED: automatic sync refuses unrelated dirt' >&2\n"
            "exit 1\n"
        )
        writer.chmod(0o755)
        deferred = command(
            str(SOURCE),
            "activate",
            "--clone",
            str(repo),
            "--primary",
            str(primary),
            "--store",
            str(store),
        )
        self.assertEqual(deferred.returncode, 2)
        retained = next(iter(TOOL.activation_latest(store).values()))
        self.assertEqual(retained.overall_state, "open")
        self.assertEqual(retained.activation.state, "deferred")
        self.assertIn(
            "automatic sync refuses unrelated dirt", retained.activation.last_reason
        )

        visible = command(
            str(SOURCE),
            "activation-obligations",
            "--primary",
            str(primary),
            "--store",
            str(store),
        )
        self.assertEqual(visible.returncode, 1)
        self.assertIn("open=1", visible.stdout)
        self.assertIn(candidate, visible.stdout)

        # A coordinator later advances the primary through the authority. The
        # watcher, not that particular process, proves completion and closes.
        git(primary, "fetch", "origin", "main")
        git(primary, "merge", "--ff-only", "origin/main")
        closed = command(
            str(SOURCE),
            "activation-obligations",
            "--primary",
            str(primary),
            "--store",
            str(store),
        )
        self.assertEqual(closed.returncode, 0, closed.stderr)
        self.assertIn("newly_satisfied=1", closed.stdout)
        satisfied = next(iter(TOOL.activation_latest(store).values()))
        self.assertEqual(satisfied.overall_state, "satisfied")
        self.assertEqual(satisfied.activation.activated_head, candidate)

    def test_watcher_does_not_close_over_dirty_candidate_path(self) -> None:
        """Containment alone is not deployment while executing bytes differ."""
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        store = self.root / "activation-obligations.jsonl"
        published = command(
            str(SOURCE), "publish", "--clone", str(repo), "--primary", str(primary),
            "--store", str(store),
        )
        self.assertEqual(published.returncode, 0, published.stderr)
        git(primary, "fetch", "origin", "main")
        git(primary, "merge", "--ff-only", "origin/main")
        (primary / "ci-hub/bin/tool").write_text("different live bytes\n")

        watched = command(
            str(SOURCE), "activation-obligations", "--primary", str(primary),
            "--store", str(store),
        )
        self.assertEqual(watched.returncode, 1)
        self.assertIn("candidate paths differ", watched.stdout)
        retained = next(iter(TOOL.activation_latest(store).values()))
        self.assertEqual(retained.overall_state, "open")

    def test_old_deferred_obligation_does_not_age_out(self) -> None:
        """Elapsed time cannot turn an unresolved deferral into completion."""
        primary = self.root / "primary"
        self.assertEqual(command("git", "clone", str(self.remote), str(primary)).returncode, 0)
        store = self.root / "activation-obligations.jsonl"
        original_utc_now = TOOL.utc_now
        TOOL.utc_now = lambda: "2026-01-01T00:00:00Z"
        try:
            opened = TOOL.create_activation_obligation(
                store=store,
                task="old-deferred-activation",
                candidate="f" * 40,
                primary=primary.resolve(),
                changed_paths=["scripts/ci-hub-clone-change.py"],
                diff_sha256="0" * 64,
            )
            TOOL.transition_activation_obligation(
                store,
                opened.obligation_id,
                "activation_deferred",
                {
                    "activation": {
                        "state": "deferred",
                        "attempts": 1,
                        "last_attempt_at": "2026-01-01T00:00:00Z",
                        "last_reason": "legitimate unrelated dirt blocks fast-forward",
                    }
                },
            )
        finally:
            TOOL.utc_now = original_utc_now

        watched = command(
            str(SOURCE), "activation-obligations", "--primary", str(primary),
            "--store", str(store),
        )
        self.assertEqual(watched.returncode, 1)
        self.assertIn("open=1", watched.stdout)
        retained = next(iter(TOOL.activation_latest(store).values()))
        self.assertEqual(retained.opened_at, "2026-01-01T00:00:00Z")
        self.assertEqual(retained.overall_state, "open")

    # Disposal's predicate. The refuse arm is the floor and is bracketed three
    # ways -- attached branch, non-HEAD branch, detached HEAD -- because the
    # scope is a disjunction and a single refusal case would pin neither half.
    # The accept arm is bracketed once per cause, since scope and staleness made
    # the two real clones undisposable for different reasons.

    def test_dispose_refuses_an_unpushed_commit_on_its_own_branch(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "ci-hub/bin/tool", "unpushed\n")
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertNotEqual(result.returncode, 0)
        # The refresh ran and succeeded, so this count is the remote's own
        # answer; disposal must not launder real work through a fetch.
        self.assertIn("PRESERVE THEM FIRST", result.stderr)
        self.assertIn(head, result.stderr)
        self.assertTrue(repo.is_dir())

    def test_dispose_refuses_unpushed_work_on_a_branch_that_is_not_head(self) -> None:
        repo = self.clone()
        git(repo, "switch", "-c", "side")
        head = self.commit(repo, "ci-hub/bin/tool", "work parked on a side branch\n")
        git(repo, "switch", "ci-hub/fixture-change")
        # Scoping to HEAD alone would see nothing here and delete the branch.
        self.assertEqual(git(repo, "rev-list", "HEAD", "--not", "--remotes"), "")
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PRESERVE THEM FIRST", result.stderr)
        self.assertIn(head, result.stderr)
        self.assertTrue(repo.is_dir())

    def test_dispose_refuses_unpushed_work_on_a_detached_head(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "ci-hub/bin/tool", "work left on a detached HEAD\n")
        git(repo, "switch", "--detach", head)
        git(repo, "branch", "-D", "ci-hub/fixture-change")
        # Scoping to --branches alone would see nothing here, and a detached
        # HEAD is exactly the state an interrupted change leaves behind.
        self.assertEqual(git(repo, "rev-list", "--branches", "--not", "--remotes"), "")
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PRESERVE THEM FIRST", result.stderr)
        self.assertIn(head, result.stderr)
        self.assertTrue(repo.is_dir())

    def test_dispose_accepts_a_clone_whose_only_unpushed_ref_is_a_fetch_destination(self) -> None:
        repo = self.clone()
        git(repo, "switch", "-c", "scratch")
        fetched = self.commit(repo, "ci-hub/bin/tool", "a fetched pull-request head\n")
        git(repo, "update-ref", "refs/pr-landing-planner/pr-1", fetched)
        git(repo, "switch", "ci-hub/fixture-change")
        git(repo, "branch", "-D", "scratch")
        # Precondition: the previous `--all` scope saw this and refused.
        self.assertIn(
            fetched,
            git(repo, "rev-list", "--exclude=refs/tags/*", "--all", "--not", "--remotes").split(),
        )
        self.assertEqual(git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), "")
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(repo.exists())
        # Narrowed, not blinded: what was not weighed is still reported.
        self.assertIn("unconsidered=1", result.stdout)
        # Exact, not a substring: the fixture branch is `ci-hub/fixture-change`,
        # so a namespace filter that mishandles a slash in a branch name shows
        # up here as a spurious `refs/heads/*`.
        self.assertIn("namespaces=refs/pr-landing-planner/* (", result.stdout)

    def test_dispose_accepts_after_the_refresh_shows_the_commit_is_pushed(self) -> None:
        repo = self.clone()
        snapshot = git(repo, "rev-parse", "refs/remotes/origin/main")
        head = self.commit(repo, "ci-hub/bin/tool", "pushed, but not seen yet\n")
        git(repo, "push", "origin", f"{head}:refs/heads/main")
        # Rewind only the remote-tracking ref, reproducing the measured state of
        # dev-hermit-p1-a-producer-path: the commit is on the remote while this
        # clone's snapshot of the remote predates it.
        git(repo, "update-ref", "refs/remotes/origin/main", snapshot)
        self.assertEqual(
            git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), head
        )
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(repo.exists())
        self.assertIn(f"recovery_sha={head}", result.stdout)

    def test_dispose_refuses_when_the_remote_cannot_be_reached(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "unpushed\n")
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot reach the remote", result.stderr)
        self.assertTrue(repo.is_dir())

    def test_dispose_refuses_an_unreachable_remote_even_with_nothing_unpushed(self) -> None:
        repo = self.clone()
        # Nothing local is at risk by the clone's own snapshot, which is exactly
        # when an unverifiable answer is tempting to accept. A snapshot can also
        # over-report containment -- the remote ref it still shows may have been
        # moved off the commit since -- so unverifiable must refuse here too.
        self.assertEqual(git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), "")
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        result = command(str(SOURCE), "dispose", "--clone", str(repo))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot reach the remote", result.stderr)
        self.assertTrue(repo.is_dir())

    def test_recovery_refs_lead_with_main_and_count_the_rest(self) -> None:
        many = [f"origin/rescue/auto-{n:04d}" for n in range(9)] + ["origin/main"]
        summary = TOOL.summarize_refs(many)
        self.assertTrue(summary.startswith("origin/main,"))
        self.assertEqual(len(summary.split(",")), 6)
        self.assertTrue(summary.endswith(",+5-more"))
        self.assertEqual(TOOL.summarize_refs(["origin/main"]), "origin/main")


if __name__ == "__main__":
    unittest.main()
