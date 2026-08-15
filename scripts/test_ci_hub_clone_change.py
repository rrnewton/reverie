#!/usr/bin/env python3
"""Focused refusal/acceptance tests for ci-hub-clone-change.py."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


SOURCE = Path(__file__).with_name("ci-hub-clone-change.py")
SPEC = importlib.util.spec_from_file_location("ci_hub_clone_change", SOURCE)
assert SPEC and SPEC.loader
TOOL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOL)


def command(*argv: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, cwd=cwd, text=True, capture_output=True, check=False)


def git(repo: Path, *args: str) -> str:
    result = command("git", "-C", str(repo), *args)
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return result.stdout.strip()


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
        git(
            self.seed,
            "add",
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
        self.assertEqual(manifest["task"], "fixture-change")
        self.assertEqual(seal["candidate"], candidate)

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
        self.assertEqual(record["candidate"], candidate)
        self.assertEqual(record["overall_state"], "cancelled")
        self.assertEqual(record["activation"]["state"], "publication_refused")
        self.assertEqual(record["event_type"], "publication_refused")

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
        self.assertEqual(record["overall_state"], "open")
        self.assertEqual(record["activation"]["state"], "pending")
        self.assertEqual(record["event_type"], "publication_confirmed_after_refusal")

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
        self.assertEqual(opened["candidate"], candidate)
        self.assertEqual(opened["overall_state"], "open")

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
        self.assertEqual(retained["overall_state"], "open")
        self.assertEqual(retained["activation"]["state"], "deferred")
        self.assertIn("automatic sync refuses unrelated dirt", retained["activation"]["last_reason"])

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
        self.assertEqual(satisfied["overall_state"], "satisfied")
        self.assertEqual(satisfied["activation"]["activated_head"], candidate)

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
        self.assertEqual(retained["overall_state"], "open")

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
                opened["obligation_id"],
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
        self.assertEqual(retained["opened_at"], "2026-01-01T00:00:00Z")
        self.assertEqual(retained["overall_state"], "open")

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
