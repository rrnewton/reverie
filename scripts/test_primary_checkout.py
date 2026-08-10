#!/usr/bin/env python3
"""Tests for scripts/primary_checkout.py."""

from __future__ import annotations

from io import StringIO
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import unittest.mock
from unittest.mock import patch

from scripts import primary_checkout


# A fixture must supply its own commit identity rather than borrowing the
# host's. Without this, `git commit` falls back to auto-detecting
# `user@hostname`, which succeeds on a developer box and FAILS on a CI runner
# whose hostname has no domain -- git refuses the derived address and exits 128
# ("unable to auto-detect email address (got 'runner@fv-az....(none)')"). These
# are throwaway repos under a temp dir, so a fixed identity is also the only
# reproducible choice.
_GIT_IDENTITY = {
    "GIT_AUTHOR_NAME": "dev-hermit tests",
    "GIT_AUTHOR_EMAIL": "tests@dev-hermit.invalid",
    "GIT_COMMITTER_NAME": "dev-hermit tests",
    "GIT_COMMITTER_EMAIL": "tests@dev-hermit.invalid",
}


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ("git", "-C", str(repo), *args),
        text=True,
        capture_output=True,
        check=True,
        env={**os.environ, **_GIT_IDENTITY},
    )
    return result.stdout.strip()


def _runner_like_env(with_identity: bool) -> dict[str, str]:
    """A hosted runner's git identity conditions, reproduced ON a developer box.

    Clearing config is NOT enough, and assuming it is, is the trap. With nothing
    configured git still GUESSES `user@hostname`; that guess SUCCEEDS on a dev
    box and is what a hosted runner rejects. `user.useConfigOnly` forbids the
    guess, which is what actually reproduces the runner locally.
    """
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in _GIT_IDENTITY and key != "EMAIL"
    }
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_SYSTEM"] = os.devnull
    env["GIT_CONFIG_COUNT"] = "1"
    env["GIT_CONFIG_KEY_0"] = "user.useConfigOnly"
    env["GIT_CONFIG_VALUE_0"] = "true"
    if with_identity:
        env.update(_GIT_IDENTITY)
    return env


class GitFixtureIdentityControlTests(unittest.TestCase):
    """Prove `_GIT_IDENTITY` is load-bearing rather than decorative.

    None of the suites below can show this. On a developer box git auto-detects
    an identity, so deleting `_GIT_IDENTITY` outright would leave every one of
    them GREEN locally and take the parent-tooling shard red again on the next
    fresh runner -- which is exactly how this broke: 111 tests, 6 errors, all
    `git commit` -> exit 128, while every local run said OK.

    So this is a two-sided bracket. The negative case proves the identity really
    is absent before the positive case takes credit for supplying it.
    """

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        subprocess.run(
            ("git", "init", "-q", "--initial-branch=main", str(self.repo)),
            check=True,
            capture_output=True,
        )
        (self.repo / "probe.txt").write_text("x\n")
        subprocess.run(
            ("git", "-C", str(self.repo), "add", "probe.txt"),
            check=True,
            capture_output=True,
            env=_runner_like_env(with_identity=True),
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def _commit(self, *, with_identity: bool) -> subprocess.CompletedProcess:
        return subprocess.run(
            ("git", "-C", str(self.repo), "commit", "-m", "probe"),
            text=True,
            capture_output=True,
            env=_runner_like_env(with_identity=with_identity),
        )

    def test_without_the_fixture_identity_a_commit_fails_as_it_does_on_ci(self):
        """NEGATIVE CONTROL. If this ever passes, the identity has stopped being
        load-bearing and every commit in this file is silently relying on
        ambient host config again -- the original fake-green."""
        result = self._commit(with_identity=False)
        self.assertNotEqual(
            result.returncode,
            0,
            "a commit succeeded with no fixture identity, so ambient host "
            "config leaked in and the CI condition is no longer reproduced",
        )
        stderr = (result.stderr or "").lower()
        self.assertTrue(
            "tell me who you are" in stderr or "auto-detect" in stderr,
            f"expected git's missing-identity refusal, got: {result.stderr!r}",
        )

    def test_the_git_helper_supplies_the_identity_under_runner_conditions(self):
        """POSITIVE, through the REAL `git()` helper.

        Deliberately not a hand-built env: this is what pins the WIRING. The
        process environment is replaced with the runner-like one the negative
        case just proved is bare, so the commit can only succeed if `git()`
        itself still injects `_GIT_IDENTITY`. Drop that `env=` and this fails.
        """
        bare = _runner_like_env(with_identity=False)
        with unittest.mock.patch.dict(os.environ, bare, clear=True):
            git(self.repo, "commit", "-m", "probe")
            expected = (
                f"{_GIT_IDENTITY['GIT_AUTHOR_NAME']} "
                f"<{_GIT_IDENTITY['GIT_AUTHOR_EMAIL']}>"
            )
            # Author AND committer: git sources them separately, so asserting
            # one would leave the other free to come from the host.
            for fmt in ("%an <%ae>", "%cn <%ce>"):
                self.assertEqual(
                    git(self.repo, "log", "-1", f"--format={fmt}"),
                    expected,
                    f"{fmt} was not the fixture identity",
                )


class _ParentWorkspaceFixture(unittest.TestCase):
    """A miniature dev-hermit parent: three product gitlinks, one Reverie pin.

    Shared parent+products fixture. Holds no tests of its own so that the
    suites below do not re-run each other's cases.
    """

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name) / "parent"
        self.root.mkdir()

        # ISOLATE THE SERIALIZED-WRITER LOCK. `parent-main-write` defaults to
        # /tmp/dev-hermit-parent-main-<uid>.lock, which is MACHINE-WIDE and shared
        # with every live agent. A fixture publishing into its own throwaway remote
        # was contending with real parent-main publications, so any test that
        # publishes failed with "another parent-main writer owns ..." whenever the
        # box happened to be busy -- observed live 2026-08-08. The fixture must not
        # queue behind, or block, real work; scope the lock to the temp directory.
        lock = Path(self.temp.name) / "parent-main.lock"
        previous = os.environ.get("HERMIT_PARENT_MAIN_LOCK_PATH")
        os.environ["HERMIT_PARENT_MAIN_LOCK_PATH"] = str(lock)
        self.addCleanup(
            lambda: os.environ.__setitem__("HERMIT_PARENT_MAIN_LOCK_PATH", previous)
            if previous is not None
            else os.environ.pop("HERMIT_PARENT_MAIN_LOCK_PATH", None)
        )

        self.seeds: dict[str, Path] = {}
        for product in primary_checkout.PRODUCTS:
            remote = Path(self.temp.name) / f"{product}.git"
            seed = Path(self.temp.name) / f"{product}-seed"
            subprocess.run(
                ("git", "init", "--bare", "--initial-branch=main", str(remote)),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ("git", "init", "--initial-branch=main", str(seed)),
                check=True,
                capture_output=True,
            )
            git(seed, "config", "user.email", "test@example.com")
            git(seed, "config", "user.name", "Test")
            (seed / "README").write_text("initial\n")
            git(seed, "add", "README")
            git(seed, "commit", "-m", "initial")
            git(seed, "remote", "add", "origin", str(remote))
            git(seed, "push", "origin", "main")
            subprocess.run(
                ("git", "clone", str(remote), str(self.root / product)),
                check=True,
                capture_output=True,
            )
            self.seeds[product] = seed

        reverie_rev = git(self.seeds["reverie"], "rev-parse", "HEAD")
        hermit_manifest = self.seeds["hermit"] / "Cargo.toml"
        hermit_manifest.write_text(
            "[package]\n"
            'name = "hermit-test"\n'
            'version = "0.1.0"\n\n'
            "[dependencies]\n"
            "reverie = { git = \"https://github.com/rrnewton/reverie.git\", "
            f'rev = "{reverie_rev}" }}\n'
        )
        lock_text = (
            "version = 3\n\n"
            "[[package]]\n"
            'name = "reverie-core"\n'
            'version = "0.2.0"\n'
            f'source = "git+https://github.com/rrnewton/reverie.git?rev={reverie_rev}#{reverie_rev}"\n'
        )
        generated = {
            Path("Cargo.lock"): lock_text,
            Path("liteinst-runtime-build/Cargo.lock"): lock_text,
            # These four carry UNSUFFIXED bases, matching the real tree: the
            # 8-hex suffix is appended at run time by stage-liteinst-runtime.sh.
            # The fixture previously seeded literal suffixes here because the old
            # check REQUIRED them; that modelled a design hermit no longer has.
            Path("ci/dag/portable.json"): "$PWD/target/liteinst-runtime-build\n",
            Path("hermit-cli/tests/common/liteinst.rs"): (
                'target_dir.join("liteinst-runtime-build")\n'
            ),
            Path("hermit-install/build.rs"): 'build_root.join("liteinst-runtime")\n',
            Path("scripts/validate.rs"): "$ROOT_DIR/target/liteinst-runtime-build\n",
            # The derivation site the check now asserts.
            Path("scripts/stage-liteinst-runtime.sh"): (
                "#!/usr/bin/env bash\n"
                'reverie_pin=$("$root_dir/ci/run-reverie-pin-check.sh" --print-pin)\n'
                'liteinst_target_dir=$(realpath -m -- "$3-${reverie_pin:0:8}")\n'
            ),
        }
        for relative, contents in generated.items():
            path = self.seeds["hermit"] / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents)
        git(
            self.seeds["hermit"],
            "add",
            "Cargo.toml",
            *(str(path) for path in generated),
        )
        git(self.seeds["hermit"], "commit", "-m", "pin reverie")
        git(self.seeds["hermit"], "push", "origin", "main")
        git(self.root / "hermit", "pull", "--ff-only", "origin", "main")

        subprocess.run(
            ("git", "init", "--initial-branch=main", str(self.root)),
            check=True,
            capture_output=True,
        )
        git(self.root, "config", "user.email", "test@example.com")
        git(self.root, "config", "user.name", "Test")
        git(self.root, "add", *primary_checkout.PRODUCTS)
        git(self.root, "commit", "-m", "initial snapshot")
        parent_remote = Path(self.temp.name) / "parent.git"
        subprocess.run(
            ("git", "init", "--bare", "--initial-branch=main", str(parent_remote)),
            check=True,
            capture_output=True,
        )
        git(self.root, "remote", "add", "origin", str(parent_remote))
        git(self.root, "push", "origin", "main")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def advance(self, product: str) -> str:
        seed = self.seeds[product]
        with (seed / "README").open("a") as readme:
            readme.write("next\n")
        git(seed, "add", "README")
        git(seed, "commit", "-m", "next")
        git(seed, "push", "origin", "main")
        return git(seed, "rev-parse", "HEAD")

    def dirty_hermit_worktree(self) -> None:
        """Leave someone else's uncommitted work under hermit/, pin files included.

        Covers all three shapes seen in the field: a rewritten manifest rev, a
        tracked pin file missing from the working tree, and untracked scratch.
        """
        reverie_head = git(self.root / "reverie", "rev-parse", "HEAD")
        manifest = self.root / "hermit" / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace(reverie_head, "b" * 40))
        lock = self.root / "hermit" / "Cargo.lock"
        lock.write_text(lock.read_text().replace(reverie_head, "0" * 40))
        (self.root / "hermit" / "liteinst-runtime-build" / "Cargo.lock").unlink()
        (self.root / "hermit" / "scratch-note.md").write_text("someone else's work\n")

    def advance_parent_remote(self, commits: int = 1) -> str:
        """Land `commits` on the parent REMOTE without touching the parent tree.

        This reproduces the steady state on the real box: agents publish parent
        main from their own worktrees, so `origin/main` advances while the shared
        parent working tree stays where it was. Nothing pulls it automatically, so
        "parent is behind" is the normal condition, not an incident.
        """
        clone = Path(self.temp.name) / "parent-publisher"
        if not clone.exists():
            subprocess.run(
                ("git", "clone", str(Path(self.temp.name) / "parent.git"), str(clone)),
                check=True, capture_output=True,
            )
            git(clone, "config", "user.email", "other@example.com")
            git(clone, "config", "user.name", "Other Agent")
        for index in range(commits):
            (clone / f"other-agent-{index}.md").write_text("landed elsewhere\n")
            git(clone, "add", "-A")
            git(clone, "commit", "-m", f"another agent's commit {index}")
        git(clone, "push", "origin", "main")
        return git(clone, "rev-parse", "HEAD")

    def drift_hermit_commit(self) -> str:
        """Commit a real Reverie pin drift IN the Hermit submodule. Returns its SHA."""
        lock = self.root / "hermit" / "Cargo.lock"
        reverie_head = git(self.root / "reverie", "rev-parse", "HEAD")
        lock.write_text(lock.read_text().replace(reverie_head, "0" * 40))
        git(self.root / "hermit", "add", "Cargo.lock")
        git(self.root / "hermit", "commit", "-m", "drift the lock")
        return git(self.root / "hermit", "rev-parse", "HEAD")



# origin/main named this fixture `_PrimaryFixture`; its suites below still
# subclass that name. Alias rather than rename so neither side's tests move.
_PrimaryFixture = _ParentWorkspaceFixture


class PrimaryCheckoutTests(_ParentWorkspaceFixture):
    def install_owner_validate_exception(self, *, target: str = "scripts/validate.rs") -> Path:
        hermit = self.root / "hermit"
        (hermit / "Makefile").write_text(
            ".PHONY: validate\n\nvalidate:\n\t./scripts/validate.rs $(ARGS)\n"
        )
        git(hermit, "add", "Makefile")
        git(hermit, "commit", "-m", "add validate recipe")
        git(hermit, "push", "origin", "main")
        link = hermit / "validate"
        link.symlink_to(target)
        return link

    def test_snapshot_allows_and_discloses_exact_owner_validate_exception(self) -> None:
        link = self.install_owner_validate_exception()
        scripts = self.root / "scripts"
        scripts.mkdir(exist_ok=True)
        checker = scripts / "check_commit_attribution.py"
        shutil.copy2(REPO_ROOT / "scripts" / checker.name, checker)
        checker.chmod(0o755)
        before = os.readlink(link)
        out, err = StringIO(), StringIO()

        with patch.dict(os.environ, {"HERMIT_COMMIT_MACHINE": "devbig030"}):
            result = primary_checkout.checkout_fresh(
                self.root,
                publish_parent=True,
                strict=True,
                use_proxy=False,
                out=out,
                err=err,
            )

        self.assertEqual(result, 0, err.getvalue())
        self.assertTrue(link.is_symlink(), "snapshot must preserve the owner path")
        self.assertEqual(os.readlink(link), before)
        report = out.getvalue()
        self.assertIn("KNOWN DIRTY EXCEPTION APPLIED", report)
        self.assertIn("product=hermit path=validate", report)
        self.assertIn("dirty-with-known-exception (NOT CLEAN)", report)
        self.assertIn("known_dirty_exceptions=hermit:validate", report)
        self.assertNotIn("hermit is clean", report)

    def test_owner_validate_exception_refuses_any_additional_untracked_path(self) -> None:
        link = self.install_owner_validate_exception()
        extra = self.root / "hermit" / "other-untracked"
        extra.write_text("preserve\n")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertIn("hermit is dirty", err.getvalue())
        self.assertNotIn("KNOWN DIRTY EXCEPTION APPLIED", out.getvalue())
        self.assertTrue(link.is_symlink())
        self.assertTrue(extra.exists())

    def test_owner_validate_exception_refuses_tracked_modification(self) -> None:
        link = self.install_owner_validate_exception()
        readme = self.root / "hermit" / "README"
        readme.write_text("modified but preserved\n")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertIn("hermit is dirty", err.getvalue())
        self.assertNotIn("KNOWN DIRTY EXCEPTION APPLIED", out.getvalue())
        self.assertEqual(readme.read_text(), "modified but preserved\n")
        self.assertTrue(link.is_symlink())

    def test_owner_validate_exception_refuses_wrong_branch_without_checkout(self) -> None:
        link = self.install_owner_validate_exception()
        hermit = self.root / "hermit"
        git(hermit, "checkout", "-b", "owner-work")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertIn("branch=owner-work, expected main", err.getvalue())
        self.assertEqual(git(hermit, "branch", "--show-current"), "owner-work")
        self.assertTrue(link.is_symlink())
        self.assertNotIn("KNOWN DIRTY EXCEPTION APPLIED", out.getvalue())

    def test_owner_validate_exception_refuses_wrong_symlink_target(self) -> None:
        link = self.install_owner_validate_exception(target="README")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertIn("hermit is dirty", err.getvalue())
        self.assertEqual(os.readlink(link), "README")
        self.assertNotIn("KNOWN DIRTY EXCEPTION APPLIED", out.getvalue())

    def test_fresh_reuses_matching_live_tracking_refs_without_fetch_or_checkout(self) -> None:
        calls: list[tuple[Path, tuple[str, ...]]] = []
        real_run_git = primary_checkout.run_git

        def tracked(repo: Path, *args: str, **kwargs: object):
            calls.append((repo, args))
            return real_run_git(repo, *args, **kwargs)

        out, err = StringIO(), StringIO()
        with patch.object(primary_checkout, "run_git", side_effect=tracked):
            result = primary_checkout.checkout_fresh(
                self.root, use_proxy=False, out=out, err=err
            )

        self.assertEqual(result, 0, err.getvalue())
        for product in primary_checkout.PRODUCTS:
            repo = self.root / product
            commands = [args[0] for called_repo, args in calls if called_repo == repo]
            self.assertIn("ls-remote", commands)
            self.assertFalse(
                {"fetch", "checkout", "pull", "merge"}.intersection(commands),
                f"unchanged {product} performed a mutating/network refresh: {commands}",
            )
        self.assertIn("live identity checked", out.getvalue())

    def test_fresh_fetches_changed_product_once_and_never_pulls(self) -> None:
        hermit_remote = self.advance("hermit")
        calls: list[tuple[Path, tuple[str, ...]]] = []
        real_run_git = primary_checkout.run_git

        def tracked(repo: Path, *args: str, **kwargs: object):
            calls.append((repo, args))
            return real_run_git(repo, *args, **kwargs)

        out, err = StringIO(), StringIO()
        with patch.object(primary_checkout, "run_git", side_effect=tracked):
            result = primary_checkout.checkout_fresh(
                self.root, use_proxy=False, out=out, err=err
            )

        self.assertEqual(result, 0, err.getvalue())
        hermit_commands = [
            args[0]
            for repo, args in calls
            if repo == self.root / "hermit"
        ]
        self.assertEqual(hermit_commands.count("fetch"), 1, hermit_commands)
        self.assertEqual(hermit_commands.count("merge"), 1, hermit_commands)
        self.assertNotIn("pull", hermit_commands)
        self.assertEqual(git(self.root / "hermit", "rev-parse", "HEAD"), hermit_remote)

    def test_current_parent_snapshot_reuses_live_tracking_ref(self) -> None:
        calls: list[tuple[Path, tuple[str, ...]]] = []
        real_run_git = primary_checkout.run_git

        def tracked(repo: Path, *args: str, **kwargs: object):
            calls.append((repo, args))
            return real_run_git(repo, *args, **kwargs)

        out, err = StringIO(), StringIO()
        with patch.object(primary_checkout, "run_git", side_effect=tracked):
            result = primary_checkout.publish_parent_snapshot(
                self.root, use_proxy=False, out=out, err=err
            )

        self.assertEqual(result, 0, err.getvalue())
        parent_commands = [args[0] for repo, args in calls if repo == self.root]
        self.assertIn("ls-remote", parent_commands)
        self.assertNotIn("fetch", parent_commands)
        self.assertNotIn("add", parent_commands)
        self.assertIn("snapshot already current", out.getvalue())

    def test_fresh_updates_clean_repos_and_preserves_dirty_repo(self) -> None:
        hermit_remote = self.advance("hermit")
        git(self.root / "reverie", "checkout", "--detach")
        liteinst_head = git(self.root / "liteinst2", "rev-parse", "HEAD")
        self.advance("liteinst2")
        (self.root / "liteinst2" / "local-change").write_text("preserve\n")
        for index in range(24):
            (self.root / "liteinst2" / f"dirty-{index}").write_text("preserve\n")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root, use_proxy=False, out=out, err=err
        )

        self.assertEqual(result, 0)
        self.assertEqual(git(self.root / "hermit", "rev-parse", "HEAD"), hermit_remote)
        self.assertEqual(git(self.root / "hermit", "branch", "--show-current"), "main")
        self.assertEqual(git(self.root / "reverie", "branch", "--show-current"), "main")
        self.assertEqual(git(self.root / "liteinst2", "rev-parse", "HEAD"), liteinst_head)
        self.assertTrue((self.root / "liteinst2" / "local-change").is_file())
        self.assertIn("liteinst2 is dirty", err.getvalue())
        self.assertIn("... 5 more path(s)", err.getvalue())

    def test_check_warns_for_stale_and_detached_without_blocking(self) -> None:
        self.advance("hermit")
        git(self.root / "reverie", "checkout", "--detach")
        out, err = StringIO(), StringIO()

        result = primary_checkout.check_freshness(
            self.root, use_proxy=False, out=out, err=err
        )
        strict_result = primary_checkout.check_freshness(
            self.root, strict=True, use_proxy=False, out=out, err=err
        )

        self.assertEqual(result, 0)
        self.assertEqual(strict_result, 1)
        self.assertIn("hermit: HEAD", err.getvalue())
        self.assertIn("reverie: branch is DETACHED", err.getvalue())

    def test_fresh_publishes_one_consistent_parent_snapshot(self) -> None:
        hermit_remote = self.advance("hermit")
        liteinst_remote = self.advance("liteinst2")
        scripts = self.root / "scripts"
        scripts.mkdir(exist_ok=True)
        checker = scripts / "check_commit_attribution.py"
        shutil.copy2(REPO_ROOT / "scripts" / checker.name, checker)
        checker.chmod(0o755)
        out, err = StringIO(), StringIO()

        # Hosted runners deliberately have no devbigNNN hostname. Production
        # automation supplies this explicit machine identity; the fixture must
        # exercise that supported path instead of borrowing the developer host.
        with patch.dict(os.environ, {"HERMIT_COMMIT_MACHINE": "devbig030"}):
            result = primary_checkout.checkout_fresh(
                self.root,
                publish_parent=True,
                strict=True,
                use_proxy=False,
                out=out,
                err=err,
            )

        self.assertEqual(result, 0, err.getvalue())
        self.assertEqual(git(self.root, "rev-parse", "HEAD:hermit"), hermit_remote)
        self.assertEqual(
            git(self.root, "rev-parse", "HEAD:reverie"),
            git(self.root / "reverie", "rev-parse", "HEAD"),
        )
        self.assertEqual(
            git(self.root, "rev-parse", "HEAD:liteinst2"), liteinst_remote
        )
        self.assertEqual(
            git(self.root, "rev-parse", "HEAD"),
            git(self.root, "rev-parse", "origin/main"),
        )
        machine_attribution_pattern = r"^\[coordinator, primary-checkout\] \[hermit-coord, devbig[0-9]+\]$"
        self.assertRegex(
            git(self.root, "log", "-1", "--format=%B").rstrip().splitlines()[-1],
            machine_attribution_pattern,
        )
        self.assertIn("Published parent snapshot", out.getvalue())

    def test_snapshot_refuses_pin_lagging_the_recorded_gitlink(self) -> None:
        """An UPSTREAM ADVANCE, not a manifest mismatch.

        `self.advance("reverie")` lands a README-only commit upstream; the hermit
        manifests are untouched and still agree with each other. The snapshot must
        still refuse -- publishing hermit-pins-X beside reverie-gitlink-Y hands a
        clone a mismatched pair -- but it must SAY that, not claim the manifests
        are mutually inconsistent. This test previously asserted "not globally
        consistent" and so enshrined the conflation its own name describes.
        """
        original_parent = git(self.root, "rev-parse", "HEAD")
        self.advance("reverie")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, 1)
        self.assertEqual(git(self.root, "rev-parse", "HEAD"), original_parent)
        message = err.getvalue()
        self.assertIn("pin lags the reverie gitlink", message)
        self.assertIn("consistent with each other", message)
        self.assertNotIn("disagree with EACH OTHER", message)

    def test_snapshot_names_manifests_that_disagree_with_each_other(self) -> None:
        """The OTHER failure, which must be distinguishable from the one above.

        Two tracked Cargo.toml naming different revisions is unfixable by any pin
        bump -- no single value satisfies both -- so it must never be reported in
        the same words as "upstream moved".
        """
        subcrate = self.seeds["hermit"] / "subcrate"
        subcrate.mkdir(exist_ok=True)
        (subcrate / "Cargo.toml").write_text(
            "[dependencies]\nreverie = { git = "
            '"https://github.com/rrnewton/reverie.git", '
            f'rev = "{"c" * 40}" }}\n'
        )
        git(self.seeds["hermit"], "add", "subcrate/Cargo.toml")
        git(self.seeds["hermit"], "commit", "-m", "second toml, different rev")
        git(self.seeds["hermit"], "push", "origin", "main")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, 1)
        message = err.getvalue()
        self.assertIn("disagree with EACH OTHER", message)
        self.assertIn("c" * 40, message)
        self.assertNotIn("pin lags the reverie gitlink", message)

    def test_snapshot_refuses_stale_reverie_lock(self) -> None:
        original_parent = git(self.root, "rev-parse", "HEAD")
        lock = self.seeds["hermit"] / "Cargo.lock"
        reverie_head = git(self.root / "reverie", "rev-parse", "HEAD")
        lock.write_text(lock.read_text().replace(reverie_head, "0" * 40))
        git(self.seeds["hermit"], "add", "Cargo.lock")
        git(self.seeds["hermit"], "commit", "-m", "stale lock")
        git(self.seeds["hermit"], "push", "origin", "main")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, 1)
        self.assertEqual(git(self.root, "rev-parse", "HEAD"), original_parent)
        self.assertIn("Cargo.lock: stale Reverie source", err.getvalue())

    def test_snapshot_refuses_hardcoded_revision_cache_key(self) -> None:
        """A hardcoded suffix must be REFUSED, not required.

        This is the same plant as before the polarity flip, and it must still be
        refused -- but for the opposite reason. The suffix is appended at run time
        by stage-liteinst-runtime.sh from the canonical pin; a literal in the tree
        is the drift hazard, so pasting one in must not be a way to turn the gate
        green.
        """
        original_parent = git(self.root, "rev-parse", "HEAD")
        cache = self.seeds["hermit"] / "scripts" / "validate.rs"
        cache.write_text("liteinst-runtime-build-00000000\n")
        git(self.seeds["hermit"], "add", "scripts/validate.rs")
        git(self.seeds["hermit"], "commit", "-m", "stale cache")
        git(self.seeds["hermit"], "push", "origin", "main")
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            publish_parent=True,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, 1)
        self.assertEqual(git(self.root, "rev-parse", "HEAD"), original_parent)
        self.assertIn("scripts/validate.rs: hardcoded LiteInst cache key", err.getvalue())

    def test_check_pins_passes_on_consistent_tree(self) -> None:
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 0, err.getvalue())
        self.assertIn("Reverie pin is internally consistent", out.getvalue())

    # ---------------------------------------------------------------------
    # check-pins evaluates the Hermit tree at the gitlink the parent commit
    # RECORDS, never the Hermit working tree. Each blocking case below is
    # therefore bracketed by its non-recorded twin: same drift left in the
    # working tree must NOT block. See check_pins.__doc__.
    # ---------------------------------------------------------------------

    def test_check_pins_blocks_recorded_stale_lock(self) -> None:
        # NEGATIVE CONTROL (mutation): a genuine pin drift, recorded by the
        # gitlink this commit would take, is still refused -- fail-closed.
        drifted = self.drift_hermit_commit()
        git(self.root, "add", "hermit")
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 1)
        self.assertIn("REVERIE PIN DRIFT", err.getvalue())
        self.assertIn(drifted, err.getvalue())
        self.assertIn("Cargo.lock: stale Reverie source", err.getvalue())

    def test_check_pins_ignores_unrecorded_committed_drift(self) -> None:
        # Same drift committed in the submodule, but the parent index still
        # records the old gitlink: this commit introduces no drift, so it must
        # not be blocked. (Refuse only what the commit would actually record.)
        self.drift_hermit_commit()
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 0, err.getvalue())

    def test_check_pins_ignores_dirty_worktree_pin_drift(self) -> None:
        # THE REGRESSION THIS FIXES: another agent's uncommitted work under
        # hermit/ -- a rewritten manifest rev, a deleted tracked pin file, and
        # untracked scratch -- must not block an unrelated parent commit.
        self.dirty_hermit_worktree()
        self.assertNotEqual(git(self.root / "hermit", "status", "--porcelain"), "")
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 0, err.getvalue())

    def test_check_pins_ignores_submodule_index_only_drift(self) -> None:
        # Staging a drifted manifest inside the submodule's own index does not
        # change the gitlink the parent records, so it must not block either.
        manifest = self.root / "hermit" / "detcore" / "Cargo.toml"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            "[dependencies]\n"
            'reverie = { git = "https://github.com/rrnewton/reverie.git", '
            f'rev = "{"a" * 40}" }}\n'
        )
        git(self.root / "hermit", "add", "detcore/Cargo.toml")
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 0, err.getvalue())

    def test_check_pins_blocks_recorded_inconsistent_manifests(self) -> None:
        manifest = self.root / "hermit" / "detcore" / "Cargo.toml"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            "[dependencies]\n"
            'reverie = { git = "https://github.com/rrnewton/reverie.git", '
            f'rev = "{"a" * 40}" }}\n'
        )
        git(self.root / "hermit", "add", "detcore/Cargo.toml")
        git(self.root / "hermit", "commit", "-m", "second rev")
        git(self.root, "add", "hermit")
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 1)
        self.assertIn("not internally consistent", err.getvalue())

    def test_check_pins_ignores_cache_key_drift(self) -> None:
        # The cache key is outside the BLOCKING gate: it is derived at run time,
        # so there is nothing here for check-pins to compare. Planting a literal
        # in the RECORDED tree must still not block check-pins -- the snapshot
        # path reports it (see test_snapshot_refuses_hardcoded_revision_cache_key),
        # this one must not.
        cache = self.root / "hermit" / "scripts" / "validate.rs"
        cache.write_text("liteinst-runtime-build-00000000\n")
        git(self.root / "hermit", "add", "scripts/validate.rs")
        git(self.root / "hermit", "commit", "-m", "stale cache key")
        git(self.root, "add", "hermit")
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 0, err.getvalue())

    def test_check_pins_does_not_block_when_gitlink_is_unreadable(self) -> None:
        # Unevaluable is not evidence of drift: warn loudly, never block.
        subprocess.run(
            (
                "git",
                "-C",
                str(self.root),
                "update-index",
                "--add",
                "--cacheinfo",
                f"160000,{'c' * 40},hermit",
            ),
            check=True,
            capture_output=True,
        )
        out, err = StringIO(), StringIO()
        result = primary_checkout.check_pins(self.root, out=out, err=err)
        self.assertEqual(result, 0)
        self.assertIn("NOT evaluated", err.getvalue())
        self.assertNotIn("BLOCKED", err.getvalue())


REPO_ROOT = Path(primary_checkout.__file__).resolve().parent.parent


class PreCommitHookTests(_ParentWorkspaceFixture):
    """End-to-end through the REAL .githooks/pre-commit, via a real `git commit`.

    Only a real commit reproduces the outage, because git itself is what exports
    GIT_INDEX_FILE -- and for the pathspec form parent policy mandates
    (`git commit -m msg -- <paths>`) it exports an ABSOLUTE path to the parent's
    temporary index, which overrides `git -C hermit`.
    """

    def setUp(self) -> None:
        super().setUp()
        hooks = self.root / ".githooks"
        hooks.mkdir()
        shutil.copy2(REPO_ROOT / ".githooks" / "pre-commit", hooks / "pre-commit")
        (self.root / "scripts").mkdir()
        shutil.copy2(
            REPO_ROOT / "scripts" / "primary_checkout.py",
            self.root / "scripts" / "primary_checkout.py",
        )
        git(self.root, "config", "core.hooksPath", ".githooks")
        # A parent-tracked manifest at a path that does not exist under hermit/.
        # This is the shmem_exec_obj/** shape: harmless in the parent, fatal once
        # an unscrubbed `git -C hermit ls-files` enumerates the parent's index.
        decoy = self.root / "crates-squat" / "Cargo.toml"
        decoy.parent.mkdir()
        decoy.write_text('[package]\nname = "squat"\n')
        (self.root / "notes.md").write_text("parent notes\n")
        git(self.root, "add", "crates-squat/Cargo.toml", "notes.md")
        git(self.root, "commit", "-m", "parent files", "--no-verify")
        # These cases isolate pin/hygiene behavior. Parent-main serialization
        # has its own real-hook suite; use the normal feature-branch surface so
        # this suite does not need a writer receipt unrelated to its assertion.
        git(self.root, "switch", "-c", "test-pin-hook")

    def commit(self, *pathspec: str, message: str = "unrelated parent work", **env: str):
        environment = dict(os.environ, PRIMARY_CHECKOUT_DISABLE_PROXY="1", **env)
        return subprocess.run(
            ("git", "-C", str(self.root), "commit", "-m", message, "--", *pathspec),
            text=True,
            capture_output=True,
            check=False,
            env=environment,
        )

    def test_hook_allows_unrelated_pathspec_commit_with_dirty_hermit(self) -> None:
        # POSITIVE CONTROL. An unrelated explicit-path parent commit, with the
        # Hermit submodule dirty (pin files included) and a parent-tracked decoy
        # manifest staged in the index. This is the exact shape that forced the
        # whole fleet onto HERMIT_PIN_DRIFT_OVERRIDE=1.
        self.dirty_hermit_worktree()
        before = git(self.root, "rev-parse", "HEAD")
        (self.root / "notes.md").write_text("parent notes, revised\n")
        git(self.root, "add", "notes.md")

        result = self.commit("notes.md")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotEqual(git(self.root, "rev-parse", "HEAD"), before)
        self.assertNotIn("BLOCKED", result.stderr)
        self.assertNotIn("crates-squat", result.stderr)
        # The gate ran and reached a verdict -- it was not skipped into silence.
        self.assertIn("Reverie pin is internally consistent", result.stdout + result.stderr)
        # ...and the submodule's dirt is untouched (Hard Invariant 5).
        self.assertTrue((self.root / "hermit" / "scratch-note.md").is_file())

    def test_hook_refuses_recorded_pin_drift(self) -> None:
        # NEGATIVE CONTROL (mutation). Plant a genuine pin drift and let the
        # commit record it: the hook must still refuse, and refuse nothing.
        drifted = self.drift_hermit_commit()
        before = git(self.root, "rev-parse", "HEAD")
        git(self.root, "add", "hermit")

        result = self.commit("hermit", message="advance hermit gitlink")

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("REVERIE PIN DRIFT", result.stderr)
        self.assertIn(drifted, result.stderr)
        self.assertEqual(git(self.root, "rev-parse", "HEAD"), before)

    def test_hook_override_still_clears_a_real_refusal(self) -> None:
        # The documented escape hatch must remain functional -- but it is now
        # needed only for a deliberate in-flight drift, not for every commit.
        self.drift_hermit_commit()
        git(self.root, "add", "hermit")

        result = self.commit(
            "hermit", message="advance hermit gitlink", HERMIT_PIN_DRIFT_OVERRIDE="1"
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("HERMIT_PIN_DRIFT_OVERRIDE=1 set", result.stderr)


class GitEnvironmentScrubTests(unittest.TestCase):
    """`git -C <repo>` must mean <repo> even under an inherited git environment.

    Git exports GIT_INDEX_FILE into hook children; for the mandated pathspec
    commit form it is an ABSOLUTE path to the parent's temporary index, and it
    overrides `-C`. Unscrubbed, that made check-pins enumerate the parent's
    manifests and resolve them under hermit/.
    """

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.outer = Path(self.temp.name) / "outer"
        self.inner = self.outer / "inner"
        self.inner.mkdir(parents=True)
        for repo, name in ((self.outer, "outer.txt"), (self.inner, "inner.txt")):
            subprocess.run(
                ("git", "init", "-q", "--initial-branch=main", str(repo)),
                check=True,
                capture_output=True,
            )
            git(repo, "config", "user.email", "test@example.com")
            git(repo, "config", "user.name", "Test")
            (repo / name).write_text("x\n")
            git(repo, "add", name)
            git(repo, "commit", "-m", "initial")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_run_git_scrubs_inherited_index_by_default(self) -> None:
        leaked = str(self.outer / ".git" / "index")
        with unittest.mock.patch.dict(os.environ, {"GIT_INDEX_FILE": leaked}):
            scrubbed = primary_checkout.run_git(self.inner, "ls-files")
            inherited = primary_checkout.run_git(self.inner, "ls-files", inherit_repo_env=True)
        self.assertEqual(scrubbed.stdout.split(), ["inner.txt"])
        self.assertEqual(inherited.stdout.split(), ["outer.txt"])


class PrimaryFreshnessTests(_PrimaryFixture):
    """The single freshness invariant over every primary.

    Bracketed both ways throughout: a check that only ever reports drift proves
    nothing, so every negative case is paired with a positive one.
    """

    def drift_kinds(self, primary: str) -> set[str]:
        drifts, _ = primary_checkout.primary_freshness_report(self.root)
        return {d.kind for d in drifts if d.primary == primary}

    def test_clean_primary_reports_no_drift(self) -> None:
        """POSITIVE bracket: a fresh primary must come back empty."""
        self.assertEqual(self.drift_kinds("hermit"), set())

    def test_detects_bare_flip_that_a_refs_only_check_cannot_see(self) -> None:
        """The symptom that recurred four times and was invisible to `check`.

        Under core.bare=true the directory still has .git, `branch
        --show-current` still answers and `rev-parse HEAD` still answers -- so a
        refs-only inspection reports a perfectly healthy primary while every
        work-tree operation fails.
        """
        repo = self.root / "hermit"
        self.assertEqual(self.drift_kinds("hermit"), set())  # fresh beforehand
        git(repo, "config", "core.bare", "true")

        # The primitives the legacy check relies on still look healthy...
        self.assertTrue((repo / ".git").exists())
        self.assertEqual(git(repo, "rev-parse", "--is-bare-repository"), "true")
        # ...but a real work-tree op fails, which is the actual breakage.
        broken = subprocess.run(
            ("git", "-C", str(repo), "status", "--short"),
            text=True, capture_output=True, check=False,
        )
        self.assertNotEqual(broken.returncode, 0)

        self.assertIn("bare", self.drift_kinds("hermit"))

    def test_restore_safe_repairs_only_the_bare_flip(self) -> None:
        repo = self.root / "hermit"
        git(repo, "config", "core.bare", "true")
        out, err = StringIO(), StringIO()
        code = primary_checkout.check_primary_freshness(
            self.root, restore_safe=True, out=out, err=err
        )
        self.assertEqual(git(repo, "rev-parse", "--is-bare-repository"), "false")
        self.assertIn("RESTORED", out.getvalue())
        # Repaired, and the repair is verified by re-evaluating rather than
        # assumed: hermit must no longer report bare.
        self.assertNotIn("bare", self.drift_kinds("hermit"))
        self.assertEqual(code, 0)

    def test_behind_is_classified_not_merely_reported_as_differing(self) -> None:
        """`differs from origin/main` is not actionable; behind/ahead/diverged are."""
        self.advance("hermit")  # move the remote forward
        repo = self.root / "hermit"
        subprocess.run(("git", "-C", str(repo), "fetch", "origin", "main"),
                       check=True, capture_output=True)
        kinds = self.drift_kinds("hermit")
        self.assertIn("behind", kinds)
        self.assertNotIn("ahead", kinds)
        self.assertNotIn("diverged", kinds)

    def test_ahead_and_diverged_are_distinguished_from_behind(self) -> None:
        repo = self.root / "hermit"
        git(repo, "config", "user.email", "test@example.com")
        git(repo, "config", "user.name", "Test")
        (repo / "local.txt").write_text("local\n")
        git(repo, "add", "local.txt")
        git(repo, "commit", "-m", "local only")
        self.assertIn("ahead", self.drift_kinds("hermit"))

        self.advance("hermit")  # now both sides have moved
        subprocess.run(("git", "-C", str(repo), "fetch", "origin", "main"),
                       check=True, capture_output=True)
        kinds = self.drift_kinds("hermit")
        self.assertIn("diverged", kinds)
        self.assertNotIn("behind", kinds)

    def test_never_fast_forwards_or_resets_on_its_own(self) -> None:
        """#320: detect and report; do not move a shared integration surface."""
        self.advance("hermit")
        repo = self.root / "hermit"
        subprocess.run(("git", "-C", str(repo), "fetch", "origin", "main"),
                       check=True, capture_output=True)
        before = git(repo, "rev-parse", "HEAD")
        primary_checkout.check_primary_freshness(
            self.root, restore_safe=True, out=StringIO(), err=StringIO()
        )
        self.assertEqual(git(repo, "rev-parse", "HEAD"), before)

    def test_dirty_is_reported_without_proposing_to_discard(self) -> None:
        repo = self.root / "hermit"
        (repo / "someone-elses-work.txt").write_text("not mine\n")
        drifts, _ = primary_checkout.primary_freshness_report(self.root)
        dirty = [d for d in drifts if d.primary == "hermit" and d.kind == "dirty"]
        self.assertTrue(dirty)
        # Invariant 5: never suggest destroying changes we did not create.
        for drift in dirty:
            for banned in ("reset", "checkout --", "clean", "stash"):
                self.assertNotIn(banned, drift.remediation)

    def test_staged_gitlink_is_dirty_but_checkout_pin_drift_is_not(self) -> None:
        """Ignore a child's checkout SHA here, but never hide a staged parent pin."""
        self.advance("hermit")
        git(self.root / "hermit", "pull", "--ff-only", "origin", "main")
        self.assertNotIn("dirty", self.drift_kinds("parent"))

        git(self.root, "add", "hermit")
        self.assertIn("dirty", self.drift_kinds("parent"))

    def test_status_failure_is_unknown_not_fresh(self) -> None:
        """A failed dirt probe cannot authorize a clean/fresh result."""
        repo = self.root / "hermit"
        real_run_git = primary_checkout.run_git

        def fail_status(path: Path, *args: str, **kwargs: object) -> subprocess.CompletedProcess[str]:
            if path == repo and args[:1] == ("status",):
                return subprocess.CompletedProcess(
                    ["git", "status"], 128, "", "synthetic status failure"
                )
            return real_run_git(path, *args, **kwargs)

        with patch.object(primary_checkout, "run_git", side_effect=fail_status):
            drifts, _ = primary_checkout.primary_freshness_report(self.root)
        kinds = {d.kind for d in drifts if d.primary == "hermit"}
        self.assertIn("unknown", kinds)

    def test_bare_probe_failure_is_unknown_not_fresh(self) -> None:
        """Every required observation must succeed before freshness is proven."""
        repo = self.root / "hermit"
        real_run_git = primary_checkout.run_git

        def fail_bare_probe(
            path: Path, *args: str, **kwargs: object
        ) -> subprocess.CompletedProcess[str]:
            if path == repo and args == ("rev-parse", "--is-bare-repository"):
                return subprocess.CompletedProcess(
                    ["git", "rev-parse"], 128, "", "synthetic probe failure"
                )
            return real_run_git(path, *args, **kwargs)

        with patch.object(primary_checkout, "run_git", side_effect=fail_bare_probe):
            drifts, _ = primary_checkout.primary_freshness_report(self.root)
        kinds = {d.kind for d in drifts if d.primary == "hermit"}
        self.assertIn("unknown", kinds)

    def test_relationship_probe_failure_is_unknown_not_diverged(self) -> None:
        """Do not assert a graph relationship when rev-list did not prove it."""
        self.advance("hermit")
        repo = self.root / "hermit"
        subprocess.run(
            ("git", "-C", str(repo), "fetch", "origin", "main"),
            check=True,
            capture_output=True,
        )
        real_run_git = primary_checkout.run_git

        def fail_relationship(
            path: Path, *args: str, **kwargs: object
        ) -> subprocess.CompletedProcess[str]:
            if path == repo and args[:3] == ("rev-list", "--left-right", "--count"):
                return subprocess.CompletedProcess(
                    ["git", "rev-list"], 128, "", "synthetic graph failure"
                )
            return real_run_git(path, *args, **kwargs)

        with patch.object(primary_checkout, "run_git", side_effect=fail_relationship):
            drifts, _ = primary_checkout.primary_freshness_report(self.root)
        kinds = {d.kind for d in drifts if d.primary == "hermit"}
        self.assertIn("unknown", kinds)
        self.assertNotIn("diverged", kinds)

    def test_no_strict_reports_unknown_advisorially(self) -> None:
        """The explicit advisory mode reports findings but exits successfully."""
        git(self.root / "hermit", "remote", "set-url", "origin", str(self.root / "nope.git"))
        out, err = StringIO(), StringIO()
        code = primary_checkout.check_primary_freshness(
            self.root, strict=False, out=out, err=err
        )
        self.assertEqual(code, 0)
        self.assertIn("unknown", err.getvalue())

    def test_exit_codes_separate_undetermined_from_drift(self) -> None:
        """An unevaluable primary is exit 2 -- nothing proven is not a pass."""
        out, err = StringIO(), StringIO()
        clean = primary_checkout.check_primary_freshness(self.root, out=out, err=err)
        self.assertEqual(clean, 0)
        self.assertIn("PRIMARY FRESHNESS OK", out.getvalue())

        git(self.root / "hermit", "remote", "set-url", "origin", str(self.root / "nope.git"))
        out, err = StringIO(), StringIO()
        self.assertEqual(
            primary_checkout.check_primary_freshness(self.root, out=out, err=err), 2
        )
        self.assertIn("unknown", err.getvalue())


class LegacyCheckBlindSpotTests(_PrimaryFixture):
    """`check` is advisory and called from the pre-commit hook with `|| true`,
    so its exit policy is deliberately left alone -- but it must no longer be
    BLIND to the two classes it could not see."""

    def test_check_now_reports_the_bare_flip(self) -> None:
        out, err = StringIO(), StringIO()
        primary_checkout.check_freshness(self.root, out=out, err=err)
        self.assertNotIn("core.bare", err.getvalue())  # positive bracket

        git(self.root / "hermit", "config", "core.bare", "true")
        out, err = StringIO(), StringIO()
        code = primary_checkout.check_freshness(self.root, out=out, err=err)
        self.assertIn("core.bare=true", err.getvalue())
        self.assertEqual(code, 0)  # still advisory: must not start blocking commits


class SnapshotMovingReferenceTests(_ParentWorkspaceFixture):
    """Separate a LOST RACE from a REAL DEFECT, and bracket both directions.

    The gate used to answer "may I publish?" before "is there anything to
    publish?", and treated every no as a hard warning. With thirteen agents
    pushing parent main, that made it a check nobody could satisfy: the reference
    moved faster than any agent could chase it, and one looped trying.

    The cheap failure is a false page. The expensive failure is going quiet about
    a genuinely incoherent snapshot -- a dirty primary or a mismatched Reverie pin
    would record something actually wrong in the gitlink. So every test below
    states which side it is defending.
    """

    def snapshot(self) -> tuple[int, str, str]:
        out, err = StringIO(), StringIO()
        code = primary_checkout.publish_parent_snapshot(
            self.root, use_proxy=False, out=out, err=err
        )
        return code, out.getvalue(), err.getvalue()

    def advance_product(self, product: str = "liteinst2") -> str:
        """Move a product and bring its PRIMARY along, so a snapshot is genuinely due.

        `publish_parent_snapshot` reads primary HEADs and requires each to equal
        its own origin/main, so a product advanced only on its remote leaves the
        primary stale and the snapshot a no-op. `checkout_fresh` normally does
        this fast-forward first; calling the publish step directly does not.

        Defaults to liteinst2 on purpose: hermit's Cargo manifests pin reverie, so
        advancing reverie alone makes the pin inconsistent and the snapshot is then
        correctly BLOCKED for a real reason -- which would mask the deferral this
        suite is trying to observe.
        """
        head = self.advance(product)
        git(self.root / product, "fetch", "origin", "main")
        git(self.root / product, "merge", "--ff-only", "origin/main")
        return head

    # ---- NEGATIVE: must NOT hard-warn -------------------------------------

    def test_parent_behind_with_snapshot_due_defers_rather_than_warning(self) -> None:
        """The filed defect. Snapshot IS due and the parent is two commits behind.

        Publishing from here would sit the gitlink commit on a stale base, so
        declining is correct -- but it is a race, not a fault, and it must not
        page. Two commits is deliberately small: this is the everyday case.
        """
        self.advance_product("liteinst2")
        self.advance_parent_remote(2)

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_DEFERRED, err)
        self.assertNotIn("HARD WARNING", err)
        self.assertIn("DEFERRED", out)
        self.assertIn("behind=2", out)
        self.assertIn("stale base", out)

    def test_parent_behind_but_gitlinks_already_published_is_success(self) -> None:
        """The ordering bug, which is a false page with NO underlying defect.

        Nothing has moved in any product, so the published gitlinks are already
        exactly right and there is nothing to commit at all. The old code asked
        "may I publish?" first and hard-warned about a parent that was merely
        behind -- warning about work that did not need doing. It must now report
        plain success, and must not even reach the currency question.
        """
        self.advance_parent_remote(3)

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_PUBLISHED, err)
        self.assertNotIn("HARD WARNING", err)
        self.assertNotIn("DEFERRED", out)
        self.assertIn("already current on origin/main", out)

    def test_no_op_is_judged_against_published_gitlinks_not_the_local_view(self) -> None:
        """`origin/main:<product>`, never `HEAD:<product>`.

        A parent that is behind holds a stale view of its own gitlinks, so asking
        the local copy whether the PUBLISHED state is current repeats the same
        error one level down. Here another agent has ALREADY published the
        advanced gitlink while this parent tree still records the old one: the
        honest answer is "nothing to do", and only the published reference can
        give it. Reading HEAD:<product> instead would see a difference, conclude
        work was due, and then refuse it for being behind -- a page for a job
        somebody else already finished.
        """
        new_head = self.advance_product("liteinst2")

        publisher = Path(self.temp.name) / "parent-snapshotter"
        subprocess.run(
            ("git", "clone", str(Path(self.temp.name) / "parent.git"), str(publisher)),
            check=True, capture_output=True,
        )
        git(publisher, "config", "user.email", "other@example.com")
        git(publisher, "config", "user.name", "Other Agent")
        git(publisher, "update-index", "--cacheinfo",
            f"160000,{new_head},liteinst2")
        git(publisher, "commit", "-m", "another agent published the liteinst2 gitlink")
        git(publisher, "push", "origin", "main")

        stale_view = git(self.root, "rev-parse", "HEAD:liteinst2")
        self.assertNotEqual(stale_view, new_head,
                            "fixture failed to make the local gitlink view stale")

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_PUBLISHED, err + out)
        self.assertIn("already current on origin/main", out)
        self.assertNotIn("HARD WARNING", err)

    # ---- POSITIVE: must STILL block, immediately -------------------------

    def test_dirty_primary_still_blocks_immediately(self) -> None:
        """Not a moving reference. Retrying never fixes it, so never defer it."""
        self.advance_product("liteinst2")
        self.dirty_hermit_worktree()

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_BLOCKED, out)
        self.assertIn("HARD WARNING", err)
        self.assertIn("primary is dirty", err)
        self.assertNotIn("DEFERRED", out)

    def test_inconsistent_reverie_pin_still_blocks_immediately(self) -> None:
        """A gitlink recorded here WOULD capture something actually wrong."""
        self.drift_hermit_commit()
        git(self.root / "hermit", "push", "origin", "main")
        git(self.root / "hermit", "fetch", "origin", "main")

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_BLOCKED, out)
        self.assertIn("HARD WARNING", err)
        self.assertNotIn("DEFERRED", out)

    def test_primary_off_main_still_blocks_immediately(self) -> None:
        git(self.root / "reverie", "checkout", "-b", "someones-feature")

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_BLOCKED, out)
        self.assertIn("HARD WARNING", err)
        self.assertIn("not on main", err)

    # ---- the deferral must reach the caller intact ------------------------

    def test_checkout_fresh_propagates_deferred_without_counting_a_failure(self) -> None:
        """A deferral is neither success nor failure and must stay distinguishable.

        Folding it into 0 hides a snapshot that never publishes; folding it into 1
        recreates the unsatisfiable page. `checkout_fresh` has to carry the third
        code out to the tick, which is the only layer that can time it.
        """
        self.advance_product("liteinst2")
        self.advance_parent_remote(1)
        out, err = StringIO(), StringIO()

        code = primary_checkout.checkout_fresh(
            self.root, publish_parent=True, strict=True,
            use_proxy=False, out=out, err=err,
        )

        self.assertEqual(code, primary_checkout.SNAPSHOT_DEFERRED, err)
        self.assertNotIn("HARD WARNING", err)


if __name__ == "__main__":
    unittest.main()
