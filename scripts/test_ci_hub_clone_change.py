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
        # Product-shaped content: a documentation file the clone protocol may
        # carry, and a source file it must refuse.
        (self.seed / "docs").mkdir()
        (self.seed / "docs/GUIDE.md").write_text("old\n")
        (self.seed / "AGENTS.md").write_text("old policy\n")
        (self.seed / "src").mkdir()
        (self.seed / "src/lib.rs").write_text("// old\n")
        git(
            self.seed,
            "add",
            "ci-hub/bin/tool",
            "outside.txt",
            "scripts/setup-hooks.sh",
            "scripts/parent-main-write",
            "docs/GUIDE.md",
            "AGENTS.md",
            "src/lib.rs",
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

    def product_clone(self, name: str = "product-change", *, path: str = "docs") -> Path:
        target = self.root / name
        result = command(
            str(SOURCE), "create", "--task", "fixture-doc-change",
            "--kind", "product", "--repo", "hermit", "--path", path,
            "--clone", str(target), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        git(target, "config", "user.name", "Test Agent")
        git(target, "config", "user.email", "test@example.invalid")
        return target

    def commit(self, repo: Path, path: str, text: str) -> str:
        (repo / path).write_text(text)
        git(repo, "add", path)
        git(repo, "commit", "-m", "candidate")
        return git(repo, "rev-parse", "HEAD")

    # --- PRODUCT KIND: the documentation route -------------------------------
    #
    # POSITIVE half first and asserted as hard as the negative. A route that
    # refuses everything unblocks nobody, and unblocking the documentation case
    # while slot allocation is down is the entire point of this kind.

    def test_product_positive_documentation_change_seals(self) -> None:
        repo = self.product_clone()
        candidate = self.commit(repo, "docs/GUIDE.md", "new documentation\n")
        result = command(
            str(SOURCE), "seal", "--clone", str(repo), "--test", "test -f docs/GUIDE.md",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        seal = json.loads((repo / ".git" / TOOL.SEAL).read_text())
        self.assertEqual(seal["candidate"], candidate)
        self.assertEqual(seal["changed_paths"], ["docs/GUIDE.md"])
        self.assertEqual(seal["tests"][0]["exit_code"], 0)

    def test_product_positive_publish_pushes_branch_and_never_main(self) -> None:
        repo = self.product_clone()
        candidate = self.commit(repo, "docs/GUIDE.md", "published documentation\n")
        main_before = git(repo, "rev-parse", "refs/remotes/origin/main")
        self.assertEqual(
            command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true").returncode,
            0,
        )
        result = command(str(SOURCE), "publish", "--clone", str(repo))
        self.assertEqual(result.returncode, 0, result.stderr)
        branch = "ci-hub/fixture-doc-change"
        pushed = git(repo, "ls-remote", "origin", f"refs/heads/{branch}").split()[0]
        self.assertEqual(pushed, candidate, "sealed candidate must be the published tip")
        # THE LOAD-BEARING ASSERTION: main is untouched. If a future edit lets a
        # product change reach parent-main-write, this is what catches it.
        self.assertEqual(
            git(repo, "ls-remote", "origin", "refs/heads/main").split()[0],
            main_before,
            "publishing a product change must not move main",
        )
        self.assertIn("does not merge", result.stdout)

    def test_product_negative_publish_does_not_open_an_activation_obligation(self) -> None:
        repo = self.product_clone()
        self.commit(repo, "docs/GUIDE.md", "no obligation\n")
        command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        result = command(str(SOURCE), "publish", "--clone", str(repo))
        self.assertEqual(result.returncode, 0, result.stderr)
        # The parent path prints an activation_obligation token here; a product
        # publication must not, because there is nothing to activate.
        self.assertNotIn("activation_obligation", result.stdout)

    def test_product_negative_activate_refuses(self) -> None:
        repo = self.product_clone()
        self.commit(repo, "docs/GUIDE.md", "activate me\n")
        command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        result = command(
            str(SOURCE), "activate", "--clone", str(repo), "--primary", str(self.seed),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("nothing to activate", result.stderr + result.stdout)

    # NEGATIVE half: the scope discipline is a real gate, not a comment.

    def test_product_negative_source_file_refuses_at_seal(self) -> None:
        repo = self.product_clone(path="src")
        self.commit(repo, "src/lib.rs", "// new\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertNotEqual(result.returncode, 0, "a product source edit must not seal")
        self.assertIn("may only carry .md", result.stderr + result.stdout)
        self.assertIn("registered worktree slot", result.stderr + result.stdout)

    def test_product_negative_mixed_doc_and_source_refuses(self) -> None:
        """A source file must not ride along beside a legitimate doc edit."""
        repo = self.product_clone(path=".")
        (repo / "docs/GUIDE.md").write_text("doc\n")
        (repo / "src/lib.rs").write_text("// smuggled\n")
        git(repo, "add", "docs/GUIDE.md", "src/lib.rs")
        git(repo, "commit", "-m", "candidate")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("src/lib.rs", result.stderr + result.stdout)

    def test_product_negative_claim_naming_ci_hub_refuses(self) -> None:
        result = command(
            str(SOURCE), "create", "--task", "bad-claim", "--kind", "product",
            "--repo", "hermit", "--path", "ci-hub/bin",
            "--clone", str(self.root / "never"), "--remote", str(self.remote),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot name a ci-hub path", result.stderr + result.stdout)
        self.assertFalse((self.root / "never").exists())

    def test_product_negative_missing_repo_refuses(self) -> None:
        result = command(
            str(SOURCE), "create", "--task", "no-repo", "--kind", "product",
            "--path", "docs", "--clone", str(self.root / "never"),
            "--remote", str(self.remote),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires --repo", result.stderr + result.stdout)

    def test_parent_negative_repo_flag_refuses(self) -> None:
        result = command(
            str(SOURCE), "create", "--task", "wrong-flag", "--repo", "hermit",
            "--path", "ci-hub/bin", "--clone", str(self.root / "never"),
            "--remote", str(self.remote),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("only applies to", result.stderr + result.stdout)

    # --- THE PARENT BOUNDARY: policy says "such as ci-hub/", not "only" -------

    def test_parent_positive_scripts_claim_is_accepted(self) -> None:
        """Coord's exact refusal: parent tooling under scripts/ is in policy scope.

        `scripts/checkout-hermit-pr-latest-reverie.sh` is a dev-hermit parent
        path, not a product path, and was refused. It is coordinator-executed
        harness tooling, so the protocol is supposed to govern it.
        """
        target = self.root / "scripts-change"
        result = command(
            str(SOURCE), "create", "--task", "parent-script-change",
            "--path", "scripts/checkout-hermit-pr-latest-reverie.sh",
            "--clone", str(target), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        git(target, "config", "user.name", "Test Agent")
        git(target, "config", "user.email", "test@example.invalid")
        self.commit(target, "scripts/setup-hooks.sh", "#!/bin/sh\nexit 0\n# edited\n")
        # Claimed one script, changed another: seal must still refuse. Widening
        # the OUTER bound must not weaken the per-task claim check.
        refused = command(str(SOURCE), "seal", "--clone", str(target), "--test", "true")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("unclaimed paths", refused.stderr + refused.stdout)

    def test_parent_positive_scripts_change_seals_and_publishes_to_main(self) -> None:
        target = self.root / "scripts-ok"
        result = command(
            str(SOURCE), "create", "--task", "parent-script-ok",
            "--path", "scripts/setup-hooks.sh",
            "--clone", str(target), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        git(target, "config", "user.name", "Test Agent")
        git(target, "config", "user.email", "test@example.invalid")
        candidate = self.commit(target, "scripts/setup-hooks.sh", "#!/bin/sh\nexit 0\n#v2\n")
        self.assertEqual(
            command(str(SOURCE), "seal", "--clone", str(target), "--test", "true").returncode,
            0,
        )
        published = command(
            str(SOURCE), "publish", "--clone", str(target), "--primary", str(self.seed),
        )
        self.assertEqual(published.returncode, 0, published.stderr)
        # A parent change DOES go to main, unlike a product change.
        self.assertEqual(
            git(target, "ls-remote", "origin", "refs/heads/main").split()[0], candidate
        )

    def test_parent_negative_claim_outside_the_declared_roots_refuses(self) -> None:
        """Widening to scripts/ is not widening to everything."""
        result = command(
            str(SOURCE), "create", "--task", "still-refused", "--path", "docs",
            "--clone", str(self.root / "never"), "--remote", str(self.remote),
        )
        self.assertNotEqual(result.returncode, 0)
        message = result.stderr + result.stdout
        self.assertIn("must stay below one of", message)
        # The refusal names the roots and the correct alternative route, so the
        # reader is not left to discover the boundary by trial and error.
        self.assertIn("ci-hub/", message)
        self.assertIn("scripts/", message)
        self.assertIn("parent-main-write", message)
        self.assertFalse((self.root / "never").exists())

    def test_parent_boundary_is_a_single_named_constant(self) -> None:
        """The set is owner-owned policy; keep it in one auditable place."""
        self.assertEqual(TOOL.PARENT_CLAIM_ROOTS, ("ci-hub", "scripts"))

    # --- THE SHARED-DESTINATION HAZARD ---------------------------------------

    def test_default_destination_is_per_task_not_shared(self) -> None:
        first = TOOL.default_clone_destination("task-one", TOOL.PARENT_KIND, None)
        second = TOOL.default_clone_destination("task-two", TOOL.PARENT_KIND, None)
        self.assertNotEqual(first, second)
        self.assertIn("task-one", first)
        # A product clone is named for its repository, so two kinds working the
        # same task slug still do not collide.
        product = TOOL.default_clone_destination("task-one", TOOL.PRODUCT_KIND, "hermit")
        self.assertNotEqual(first, product)
        self.assertIn("hermit", product)

    def test_occupied_destination_refusal_names_the_occupant(self) -> None:
        occupied = self.clone("occupied")
        self.commit(occupied, "ci-hub/bin/tool", "occupant work\n")
        command(str(SOURCE), "seal", "--clone", str(occupied), "--test", "true")
        result = command(
            str(SOURCE), "create", "--task", "second-agent", "--path", "ci-hub/bin",
            "--clone", str(occupied), "--remote", str(self.remote),
        )
        self.assertNotEqual(result.returncode, 0)
        message = result.stderr + result.stdout
        # Naming the occupant is the difference between a diagnosable collision
        # and a directory the finder has to reverse-engineer. Disposal is still
        # coordinator work; this only supplies the facts that decision needs.
        self.assertIn("task=fixture-change", message)
        self.assertIn("claims=ci-hub/bin", message)
        self.assertIn("sealed_candidate=", message)

    def test_occupied_destination_without_manifest_says_so(self) -> None:
        stray = self.root / "stray"
        stray.mkdir()
        result = command(
            str(SOURCE), "create", "--task", "third-agent", "--path", "ci-hub/bin",
            "--clone", str(stray), "--remote", str(self.remote),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("occupant unknown", result.stderr + result.stdout)

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


if __name__ == "__main__":
    unittest.main()
