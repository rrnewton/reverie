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

    def install_agent_utils_submodule(self) -> Path:
        """Install a real nested submodule in the Hermit fixture."""
        remote = Path(self.temp.name) / "agent-utils.git"
        seed = Path(self.temp.name) / "agent-utils-seed"
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
        (seed / "README").write_text("agent-utils initial\n")
        git(seed, "add", "README")
        git(seed, "commit", "-m", "agent-utils initial")
        git(seed, "remote", "add", "origin", str(remote))
        git(seed, "push", "origin", "main")

        hermit_seed = self.seeds["hermit"]
        git(
            hermit_seed,
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            str(remote),
            primary_checkout.HERMIT_AGENT_UTILS_PATH,
        )
        git(hermit_seed, "add", ".gitmodules", primary_checkout.HERMIT_AGENT_UTILS_PATH)
        git(hermit_seed, "commit", "-m", "add agent-utils submodule")
        git(hermit_seed, "push", "origin", "main")

        hermit = self.root / "hermit"
        git(hermit, "pull", "--ff-only", "origin", "main")
        git(
            hermit,
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--checkout",
            "--",
            primary_checkout.HERMIT_AGENT_UTILS_PATH,
        )
        return seed

    def advance_agent_utils_gitlink(self, seed: Path) -> str:
        """Advance both agent-utils main and Hermit's recorded gitlink."""
        with (seed / "README").open("a") as readme:
            readme.write("agent-utils next\n")
        git(seed, "add", "README")
        git(seed, "commit", "-m", "agent-utils next")
        git(seed, "push", "origin", "main")
        target = git(seed, "rev-parse", "HEAD")

        hermit_seed = self.seeds["hermit"]
        nested = hermit_seed / primary_checkout.HERMIT_AGENT_UTILS_PATH
        git(nested, "fetch", "origin", "main")
        git(nested, "checkout", "--detach", target)
        git(hermit_seed, "add", primary_checkout.HERMIT_AGENT_UTILS_PATH)
        git(hermit_seed, "commit", "-m", "advance agent-utils gitlink")
        git(hermit_seed, "push", "origin", "main")
        return target

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

    def install_parent_attribution_checker(self) -> None:
        """Make the minimal parent fixture capable of publishing a snapshot.

        Production tracks this file already. The fixture starts with product
        gitlinks only, so commit the checker before creating a behind-parent
        condition; leaving it untracked would correctly trip automatic sync's
        unrelated-dirty-path refusal and mask the positive bracket.
        """
        scripts = self.root / "scripts"
        scripts.mkdir(exist_ok=True)
        checker = scripts / "check_commit_attribution.py"
        shutil.copy2(REPO_ROOT / "scripts" / checker.name, checker)
        checker.chmod(0o755)
        git(self.root, "add", "scripts/check_commit_attribution.py")
        git(self.root, "commit", "-m", "install fixture attribution checker")
        git(self.root, "push", "origin", "main")

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


class PrimaryCheckoutDeadlineTests(unittest.TestCase):
    def snapshot_boundary(
        self, *, published: bool
    ) -> tuple[tempfile.TemporaryDirectory, Path, str]:
        temp = tempfile.TemporaryDirectory()
        root = Path(temp.name)
        git(root, "init", "-b", "main")
        git(root, "config", "user.email", "snapshot-test@example.com")
        git(root, "config", "user.name", "Snapshot Test")
        (root / "seed").write_text("initial\n")
        git(root, "add", "seed")
        git(root, "commit", "-m", "initial parent")
        initial = git(root, "rev-parse", "HEAD")
        git(root, "update-ref", "refs/remotes/origin/main", initial)
        (root / "seed").write_text("snapshot\n")
        git(root, "add", "seed")
        git(root, "commit", "-m", primary_checkout.SNAPSHOT_COMMIT_MESSAGE)
        head = git(root, "rev-parse", "HEAD")
        if published:
            git(root, "update-ref", "refs/remotes/origin/main", head)
        return temp, root, head

    def test_active_deadline_bounds_each_owned_child(self) -> None:
        completed = subprocess.CompletedProcess(["git"], 0, "", "")
        token = primary_checkout._COMMAND_DEADLINE.set(110.0)
        try:
            with patch.object(
                primary_checkout.time, "monotonic", return_value=103.0
            ), patch.object(
                primary_checkout.subprocess, "run", return_value=completed
            ) as run:
                primary_checkout._run_process(["git", "status"], check=False)
        finally:
            primary_checkout._COMMAND_DEADLINE.reset(token)

        self.assertEqual(run.call_args.kwargs["timeout"], 7.0)

    def test_checkout_deadline_returns_named_block_instead_of_escaping(self) -> None:
        out, err = StringIO(), StringIO()
        command = ["with-proxy", "git", "ls-remote", "origin", "refs/heads/main"]
        with patch.object(
            primary_checkout,
            "_checkout_fresh_impl",
            side_effect=subprocess.TimeoutExpired(command, 24.0),
        ):
            result = primary_checkout.checkout_fresh(
                Path("/unused"), deadline_secs=24.0, out=out, err=err
            )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertIn("inner deadline exhausted", err.getvalue())
        self.assertIn("with-proxy git ls-remote", err.getvalue())
        self.assertIsNone(primary_checkout._COMMAND_DEADLINE.get())

    def test_slow_child_published_snapshot_reports_success_not_wrapper_error(self) -> None:
        temp, root, head = self.snapshot_boundary(published=True)
        self.addCleanup(temp.cleanup)
        timeout = subprocess.TimeoutExpired(
            ["scripts/parent-main-write", "commit"],
            20.0,
            output=f"[main {head[:7]}] {primary_checkout.SNAPSHOT_COMMIT_MESSAGE}\n",
        )
        timeout.primary_checkout_phase = primary_checkout.PARENT_MAIN_WRITE_PHASE
        out, err = StringIO(), StringIO()
        with patch.object(
            primary_checkout,
            "_checkout_fresh_impl",
            side_effect=timeout,
        ):
            result = primary_checkout.checkout_fresh(
                root, deadline_secs=24.0, out=out, err=err
            )

        self.assertEqual(primary_checkout.SNAPSHOT_PUBLISHED, result)
        self.assertIn(f"SUCCESS: parent snapshot {head}", out.getvalue())
        self.assertIn("exceeded its inner deadline after publication", err.getvalue())
        self.assertNotIn("ERROR:", err.getvalue())
        self.assertIn("timed child stdout (preserved)", err.getvalue())

    def test_timed_child_without_published_boundary_still_reports_error(self) -> None:
        temp, root, head = self.snapshot_boundary(published=False)
        self.addCleanup(temp.cleanup)
        timeout = subprocess.TimeoutExpired(
            ["scripts/parent-main-write", "commit"],
            20.0,
            output=(
                f"[main {head[:7]}] {primary_checkout.SNAPSHOT_COMMIT_MESSAGE}\n"
                "To github.com:rrnewton/dev-hermit.git old..new\n"
            ),
        )
        timeout.primary_checkout_phase = primary_checkout.PARENT_MAIN_WRITE_PHASE
        out, err = StringIO(), StringIO()
        with patch.object(
            primary_checkout,
            "_checkout_fresh_impl",
            side_effect=timeout,
        ):
            result = primary_checkout.checkout_fresh(
                root, deadline_secs=24.0, out=out, err=err
            )

        self.assertEqual(primary_checkout.SNAPSHOT_BLOCKED, result)
        self.assertIn("ERROR: primary checkout snapshot inner deadline exhausted", err.getvalue())
        self.assertNotIn("SUCCESS:", out.getvalue())


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

    def test_fresh_syncs_clean_agent_utils_to_advanced_committed_gitlink(self) -> None:
        seed = self.install_agent_utils_submodule()
        hermit = self.root / "hermit"
        nested = hermit / primary_checkout.HERMIT_AGENT_UTILS_PATH
        previous = git(nested, "rev-parse", "HEAD")
        expected = self.advance_agent_utils_gitlink(seed)
        self.assertNotEqual(previous, expected)
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, 0, err.getvalue())
        self.assertEqual(git(nested, "rev-parse", "HEAD"), expected)
        self.assertEqual(git(nested, "status", "--porcelain=v1"), "")
        self.assertEqual(git(hermit, "status", "--porcelain=v1"), "")
        self.assertIn(
            f"advanced clean nested checkout {previous} -> {expected}",
            out.getvalue(),
        )

    def test_fresh_refuses_agent_utils_with_uncommitted_work(self) -> None:
        seed = self.install_agent_utils_submodule()
        hermit = self.root / "hermit"
        nested = hermit / primary_checkout.HERMIT_AGENT_UTILS_PATH
        hermit_before = git(hermit, "rev-parse", "HEAD")
        nested_before = git(nested, "rev-parse", "HEAD")
        local = nested / "local-unpublished.txt"
        local.write_text("preserve me\n")
        self.advance_agent_utils_gitlink(seed)
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertEqual(git(hermit, "rev-parse", "HEAD"), hermit_before)
        self.assertEqual(git(nested, "rev-parse", "HEAD"), nested_before)
        self.assertEqual(local.read_text(), "preserve me\n")
        self.assertIn("contains real working-tree changes", err.getvalue())
        self.assertIn("?? local-unpublished.txt", err.getvalue())

    def test_fresh_refuses_clean_unpublished_agent_utils_commit(self) -> None:
        seed = self.install_agent_utils_submodule()
        hermit = self.root / "hermit"
        nested = hermit / primary_checkout.HERMIT_AGENT_UTILS_PATH
        unpublished = nested / "local-commit.txt"
        unpublished.write_text("preserve committed work\n")
        git(nested, "add", unpublished.name)
        git(nested, "commit", "-m", "local unpublished work")
        local_head = git(nested, "rev-parse", "HEAD")
        hermit_before = git(hermit, "rev-parse", "HEAD")
        self.advance_agent_utils_gitlink(seed)
        out, err = StringIO(), StringIO()

        result = primary_checkout.checkout_fresh(
            self.root,
            strict=True,
            use_proxy=False,
            out=out,
            err=err,
        )

        self.assertEqual(result, primary_checkout.SNAPSHOT_BLOCKED)
        self.assertEqual(git(hermit, "rev-parse", "HEAD"), hermit_before)
        self.assertEqual(git(nested, "rev-parse", "HEAD"), local_head)
        self.assertEqual(unpublished.read_text(), "preserve committed work\n")
        self.assertIn(
            "clean but unpublished, ahead, or divergent HEAD",
            err.getvalue(),
        )

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


class ProxyDisableFlagTests(unittest.TestCase):
    """PRIMARY_CHECKOUT_DISABLE_PROXY is VALUE-checked, not presence-checked.

    The presence form `os.environ.get(name)` is truthy for "0", because "0" is
    a non-empty string. So `DISABLE_PROXY=0` -- the natural way to write "do
    not disable" -- silently switched the proxy OFF, doing the opposite of what
    it reads. Empty and unset were already correct (`bool("")` is False); the
    broken values were "0" and every other non-empty value that is not "1".

    The inverse case is bracketed too: a seam that never engages is as broken
    as one that always does, so "1" must really suppress the proxy.
    """

    def _command(self, value: str | None) -> list[str]:
        captured: list[list[str]] = []

        def fake_run(command, **kwargs):
            captured.append(list(command))
            return subprocess.CompletedProcess(command, 0, "", "")

        env = dict(os.environ)
        env.pop("PRIMARY_CHECKOUT_DISABLE_PROXY", None)
        if value is not None:
            env["PRIMARY_CHECKOUT_DISABLE_PROXY"] = value
        with unittest.mock.patch.dict(os.environ, env, clear=True), \
             unittest.mock.patch.object(primary_checkout.shutil, "which", return_value="/usr/bin/with-proxy"), \
             unittest.mock.patch.object(primary_checkout.subprocess, "run", fake_run):
            primary_checkout.run_git(Path("/tmp"), "ls-remote", network=True)
        return captured[0]

    def test_unset_keeps_the_proxy(self) -> None:
        self.assertIn("/usr/bin/with-proxy", self._command(None))

    def test_zero_keeps_the_proxy(self) -> None:
        # THE REGRESSION CASE: "0" is truthy, so the presence form disabled here.
        self.assertIn("/usr/bin/with-proxy", self._command("0"))

    def test_empty_keeps_the_proxy(self) -> None:
        self.assertIn("/usr/bin/with-proxy", self._command(""))

    def test_arbitrary_value_keeps_the_proxy(self) -> None:
        # Only the exact "1" disables; we do not guess at "true".
        self.assertIn("/usr/bin/with-proxy", self._command("true"))

    def test_one_actually_disables_the_proxy(self) -> None:
        # INVERSE CONTROL: the seam must still work when correctly set.
        self.assertNotIn("/usr/bin/with-proxy", self._command("1"))

    def test_predicate_directly(self) -> None:
        for value, want in ((None, False), ("", False), ("0", False), ("true", False), ("1", True)):
            env = dict(os.environ)
            env.pop("PRIMARY_CHECKOUT_DISABLE_PROXY", None)
            if value is not None:
                env["PRIMARY_CHECKOUT_DISABLE_PROXY"] = value
            with unittest.mock.patch.dict(os.environ, env, clear=True):
                self.assertEqual(primary_checkout.proxy_disabled(), want, f"value={value!r}")


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
    """Self-heal a clean stale parent and refuse every ambiguous one.

    The gate used to defer every due snapshot while the parent was behind, but
    retrying could not change that state. The registered writer now catches up a
    clean ahead=0 parent under its lock. Local commits, unrelated dirt and staged
    gitlinks remain human decisions and refuse byte-identically.

    The expensive failure is automation reasoning through ambiguity: a dirty
    primary or mismatched Reverie pin would record an incoherent snapshot, while
    parent dirt could be somebody's only copy. Every test below states which side
    it defends.
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

    def test_clean_parent_behind_auto_syncs_and_publishes_snapshot(self) -> None:
        """The recurring manual cycle now converges through the registered writer."""
        self.install_parent_attribution_checker()
        liteinst = self.advance_product("liteinst2")
        self.advance_parent_remote(2)

        with patch.dict(os.environ, {"HERMIT_COMMIT_MACHINE": "devbig030"}):
            code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_PUBLISHED, err + out)
        self.assertNotIn("HARD WARNING", err)
        self.assertIn("automatic-boundary", out)
        self.assertIn("Published parent snapshot", out)
        self.assertEqual(
            git(self.root, "rev-parse", "HEAD"),
            git(self.root, "rev-parse", "origin/main"),
        )
        self.assertEqual(liteinst, git(self.root, "rev-parse", "HEAD:liteinst2"))

    def test_unrelated_dirty_parent_path_refuses_auto_sync_unchanged(self) -> None:
        self.advance_product("liteinst2")
        self.advance_parent_remote(1)
        private = self.root / "private-parent-work.txt"
        private.write_text("only copy\n")
        head_before = git(self.root, "rev-parse", "HEAD")
        index_before = git(self.root, "write-tree")

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_DEFERRED, err + out)
        self.assertIn("DIRTY OUTSIDE EXACT FETCHED TARGET", out)
        self.assertIn("human inspection is required", out)
        self.assertEqual(head_before, git(self.root, "rev-parse", "HEAD"))
        self.assertEqual(index_before, git(self.root, "write-tree"))
        self.assertEqual("only copy\n", private.read_text())

    def test_local_parent_commit_refuses_auto_sync_unchanged(self) -> None:
        self.advance_product("liteinst2")
        local = self.root / "local-parent-commit.txt"
        local.write_text("unpublished\n")
        git(self.root, "add", local.name)
        git(self.root, "commit", "-m", "local parent work", "--", local.name)
        head_before = git(self.root, "rev-parse", "HEAD")
        index_before = git(self.root, "write-tree")

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_DEFERRED, err + out)
        self.assertIn("REFUSED local commits", out)
        self.assertIn("Human inspection required", out)
        self.assertEqual(head_before, git(self.root, "rev-parse", "HEAD"))
        self.assertEqual(index_before, git(self.root, "write-tree"))
        self.assertEqual("unpublished\n", local.read_text())

    def test_staged_product_gitlink_yields_before_auto_sync(self) -> None:
        liteinst = self.advance_product("liteinst2")
        self.advance_parent_remote(1)
        git(self.root, "add", "liteinst2")
        head_before = git(self.root, "rev-parse", "HEAD")

        code, out, err = self.snapshot()

        self.assertEqual(code, primary_checkout.SNAPSHOT_DEFERRED, err + out)
        self.assertIn("already staged by another coordinator", out)
        self.assertEqual(head_before, git(self.root, "rev-parse", "HEAD"))
        self.assertEqual(liteinst, git(self.root, "rev-parse", ":liteinst2"))

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
        """A human-required refusal remains a typed deferral to the tick.

        Folding it into success hides work that never published; folding it into
        an immediate hard block pages on the first ambiguous sample. The tick is
        the layer that times whether the human-required condition persists.
        """
        self.advance_product("liteinst2")
        self.advance_parent_remote(1)
        (self.root / "unowned-parent-work.txt").write_text("preserve\n")
        out, err = StringIO(), StringIO()

        code = primary_checkout.checkout_fresh(
            self.root, publish_parent=True, strict=True,
            use_proxy=False, out=out, err=err,
        )

        self.assertEqual(code, primary_checkout.SNAPSHOT_DEFERRED, err)
        self.assertNotIn("HARD WARNING", err)
        self.assertIn("DIRTY OUTSIDE EXACT FETCHED TARGET", out.getvalue())


if __name__ == "__main__":
    unittest.main()
