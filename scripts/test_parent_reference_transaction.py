#!/usr/bin/env python3
"""Both-direction tests for the parent origin reference check."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
HOOK = ROOT / "scripts" / "parent-reference-transaction"
INSTALLER = ROOT / "scripts" / "setup-hooks.sh"
PARENT_URL = "https://github.com/rrnewton/dev-hermit.git"
PRODUCT_URL = "https://github.com/rrnewton/hermit.git"


def run(*args: str, cwd: Path, input_text: str | None = None):
    return subprocess.run(
        args,
        cwd=cwd,
        input=input_text,
        text=True,
        capture_output=True,
        check=False,
        env=dict(os.environ),
    )


def must_run(*args: str, cwd: Path) -> str:
    result = run(*args, cwd=cwd)
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return result.stdout.strip()


def initialize_repo(path: Path, content: str) -> str:
    path.mkdir()
    must_run("git", "init", "-q", "-b", "main", cwd=path)
    must_run("git", "config", "user.name", "test", cwd=path)
    must_run("git", "config", "user.email", "test@example.invalid", cwd=path)
    (path / "tracked").write_text(content)
    must_run("git", "add", "tracked", cwd=path)
    must_run("git", "commit", "-q", "-m", content, cwd=path)
    return must_run("git", "rev-parse", "HEAD", cwd=path)


def commit(path: Path, content: str) -> str:
    (path / "tracked").write_text(content)
    must_run("git", "add", "tracked", cwd=path)
    must_run("git", "commit", "-q", "-m", content, cwd=path)
    return must_run("git", "rev-parse", "HEAD", cwd=path)


class FetchBracketTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name)

        self.parent_work = self.base / "parent-work"
        self.parent_initial = initialize_repo(self.parent_work, "parent-initial")
        self.parent_bare = self.base / "parent.git"
        must_run(
            "git", "clone", "-q", "--bare", str(self.parent_work), str(self.parent_bare),
            cwd=self.base,
        )

        product_work = self.base / "product-work"
        self.product_tip = initialize_repo(product_work, "product-tip")
        must_run("git", "tag", "product-only", cwd=product_work)
        self.product_bare = self.base / "product.git"
        must_run(
            "git", "clone", "-q", "--bare", str(product_work), str(self.product_bare),
            cwd=self.base,
        )

        self.checkout = self.base / "checkout"
        must_run(
            "git", "clone", "-q", str(self.parent_bare), str(self.checkout), cwd=self.base
        )
        hooks = self.checkout / ".git" / "test-hooks"
        hooks.mkdir()
        (hooks / "reference-transaction").symlink_to(HOOK)
        must_run("git", "config", "core.hooksPath", str(hooks), cwd=self.checkout)

        tracked_hooks = self.checkout / ".githooks"
        tracked_hooks.mkdir()
        delegated = self.checkout / ".git" / "delegated-phases"
        repo_hook = tracked_hooks / "reference-transaction"
        repo_hook.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$1\" >>'{delegated}'\n"
            "cat >/dev/null\n"
        )
        repo_hook.chmod(0o755)

        must_run(
            "git", "config", f"url.{self.parent_bare.as_uri()}.insteadOf", PARENT_URL,
            cwd=self.checkout,
        )
        must_run(
            "git", "config", f"url.{self.product_bare.as_uri()}.insteadOf", PRODUCT_URL,
            cwd=self.checkout,
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def origin_main(self) -> str:
        return must_run("git", "rev-parse", "refs/remotes/origin/main", cwd=self.checkout)

    def test_product_origin_fetch_is_loudly_refused_without_ref_update(self) -> None:
        must_run("git", "remote", "set-url", "origin", PRODUCT_URL, cwd=self.checkout)
        result = run("git", "fetch", "origin", cwd=self.checkout)

        self.assertNotEqual(0, result.returncode)
        self.assertEqual(self.parent_initial, self.origin_main())
        self.assertNotEqual(self.product_tip, self.origin_main())
        tag = run("git", "rev-parse", "--verify", "refs/tags/product-only", cwd=self.checkout)
        self.assertNotEqual(0, tag.returncode)
        self.assertIn("parent-origin: REFUSED", result.stderr)
        self.assertIn("rrnewton/hermit.git", result.stderr)
        self.assertIn("No origin-tracking ref was updated", result.stderr)

    def test_parent_origin_fetch_advances_and_delegates_existing_hook(self) -> None:
        parent_next = commit(self.parent_work, "parent-next")
        must_run("git", "push", "-q", str(self.parent_bare), "main", cwd=self.parent_work)
        must_run("git", "remote", "set-url", "origin", PARENT_URL, cwd=self.checkout)

        result = run("git", "fetch", "origin", cwd=self.checkout)

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual(parent_next, self.origin_main())
        phases = (self.checkout / ".git" / "delegated-phases").read_text().splitlines()
        self.assertIn("prepared", phases)
        self.assertIn("committed", phases)

    def test_unrecognized_origin_is_refused_on_a_prepared_update(self) -> None:
        must_run(
            "git", "remote", "set-url", "origin", "https://example.invalid/dev-hermit.git",
            cwd=self.checkout,
        )
        update = f"{self.parent_initial} {self.product_tip} refs/remotes/origin/main\n"

        result = run(str(HOOK), "prepared", cwd=self.checkout, input_text=update)

        self.assertEqual(1, result.returncode)
        self.assertIn("example.invalid", result.stderr)
        self.assertEqual(self.parent_initial, self.origin_main())


class InstallerScopeTests(unittest.TestCase):
    def test_parent_gets_origin_check_while_product_keeps_dispatcher(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            must_run("git", "init", "-q", "-b", "main", cwd=repo)
            (repo / "scripts").mkdir()
            (repo / ".githooks").mkdir()
            shutil.copy2(INSTALLER, repo / "scripts" / "setup-hooks.sh")
            shutil.copy2(HOOK, repo / "scripts" / "parent-reference-transaction")

            dispatcher = repo / ".githooks" / "dev-hermit-hook-dispatch"
            dispatcher.write_text("#!/bin/sh\nexit 0\n")
            dispatcher.chmod(0o755)
            checker = repo / "scripts" / "check_commit_attribution.py"
            checker.write_text("#!/bin/sh\necho test-machine\n")
            checker.chmod(0o755)

            product = repo / "product"
            product.mkdir()
            must_run("git", "init", "-q", "-b", "main", cwd=product)
            (repo / ".gitmodules").write_text(
                "[submodule \"product\"]\n\tpath = product\n\turl = unused\n"
            )

            installed = run("scripts/setup-hooks.sh", cwd=repo)
            self.assertEqual(0, installed.returncode, installed.stderr)
            checked = run("scripts/setup-hooks.sh", "--check", cwd=repo)
            self.assertEqual(0, checked.returncode, checked.stderr)

            parent_common = Path(must_run(
                "git", "rev-parse", "--path-format=absolute", "--git-common-dir", cwd=repo
            ))
            product_common = Path(must_run(
                "git", "rev-parse", "--path-format=absolute", "--git-common-dir", cwd=product
            ))
            parent_hook = parent_common / "dev-hermit-hooks" / "reference-transaction"
            product_hook = product_common / "dev-hermit-hooks" / "reference-transaction"
            self.assertEqual(
                (repo / "scripts" / "parent-reference-transaction").resolve(),
                parent_hook.resolve(),
            )
            self.assertEqual(dispatcher.resolve(), product_hook.resolve())


if __name__ == "__main__":
    unittest.main()
