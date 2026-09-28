#!/usr/bin/env python3
"""Integration tests for git-push-verified file detection on new branches."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(
    os.environ.get(
        "GIT_PUSH_VERIFIED_UNDER_TEST",
        ROOT / "ci-hub" / "bin" / "git-push-verified",
    )
)


class GitPushVerifiedNewBranchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="git-push-verified-new-branch-"
        )
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        self.fake_bin = self.root / "bin"
        self.hooks = self.root / "empty-hooks"
        self.rustfmt_log = self.root / "rustfmt.log"
        self.fake_bin.mkdir()
        self.hooks.mkdir()
        self._write_executable(
            self.fake_bin / "cargo",
            "#!/bin/sh\n" "echo 'error: could not find `Cargo.toml`' >&2\n" "exit 1\n",
        )
        self._write_executable(
            self.fake_bin / "rustfmt",
            "#!/bin/sh\n"
            'for argument in "$@"; do file=$argument; done\n'
            'printf \'%s\\n\' "$file" >> "$FAKE_RUSTFMT_LOG"\n'
            "if grep -q 'fn main() {' \"$file\"; then exit 0; fi\n"
            "printf 'Diff in %s:1:\\n' \"$file\"\n"
            "exit 1\n",
        )
        self._run(["git", "init", "--bare", str(self.remote)], cwd=self.root)
        self._run(["git", "symbolic-ref", "HEAD", "refs/heads/master"], cwd=self.remote)
        self._run(["git", "init", str(self.checkout)], cwd=self.root)
        self._run(["git", "config", "user.name", "Test Author"])
        self._run(["git", "config", "user.email", "test@example.com"])
        self._run(["git", "config", "core.hooksPath", str(self.hooks)])

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_executable(self, path: Path, source: str) -> None:
        path.write_text(source)
        path.chmod(0o755)

    def _run(
        self,
        command: list[str],
        *,
        cwd: Path | None = None,
        check: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            command,
            cwd=cwd or self.checkout,
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=20,
        )

    def _commit(self, path: str, contents: str) -> str:
        target = self.checkout / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)
        self._run(["git", "add", path])
        self._run(["git", "commit", "-m", f"Add {path}"])
        return self._run(["git", "rev-parse", "HEAD"]).stdout.strip()

    def _push(
        self, branch: str, *, source: str = "HEAD"
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["PATH"] = f"{self.fake_bin}:{environment['PATH']}"
        environment["FAKE_RUSTFMT_LOG"] = str(self.rustfmt_log)
        return subprocess.run(
            [str(SCRIPT), str(self.remote), f"{source}:refs/heads/{branch}"],
            cwd=self.checkout,
            env=environment,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=20,
        )

    def _remote_head(self, branch: str) -> str | None:
        output = self._run(
            ["git", "ls-remote", str(self.remote), f"refs/heads/{branch}"]
        ).stdout.strip()
        return output.split()[0] if output else None

    def test_new_branch_with_rust_refuses_unformatted_then_pushes_formatted(
        self,
    ) -> None:
        branch = "rust-change"
        self._commit("tool.rs", 'fn main(){println!("hello");}\n')

        refused = self._push(branch)

        self.assertEqual(4, refused.returncode, refused.stdout + refused.stderr)
        self.assertIn("format CHECK FAILED in: tool.rs", refused.stdout)
        self.assertIn("REFUSING THE PUSH", refused.stderr)
        self.assertIsNone(self._remote_head(branch))
        self.assertEqual(["tool.rs"], self.rustfmt_log.read_text().splitlines())

        head = self._commit("tool.rs", 'fn main() {\n    println!("hello");\n}\n')
        passed = self._push(branch)

        self.assertEqual(0, passed.returncode, passed.stdout + passed.stderr)
        self.assertIn(
            "format CHECKED and clean via rustfmt (1 .rs file(s)", passed.stdout
        )
        self.assertEqual(head, self._remote_head(branch))
        self.assertEqual(
            ["tool.rs", "tool.rs"], self.rustfmt_log.read_text().splitlines()
        )

    def test_new_branch_without_rust_skips_format_check_and_pushes(self) -> None:
        branch = "documentation-only"
        self._commit("existing.rs", 'fn existing() {\n    println!("base");\n}\n')
        self._run(["git", "push", str(self.remote), "HEAD:refs/heads/master"])
        self._run(["git", "switch", "-c", "documentation"])
        head = self._commit("README.md", "No Rust files in this commit.\n")

        result = self._push(branch)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("format check SKIPPED (no .rs in this push)", result.stdout)
        self.assertEqual(head, self._remote_head(branch))
        self.assertFalse(self.rustfmt_log.exists())

    def test_new_branch_checks_rust_changed_before_a_documentation_tip(self) -> None:
        self._commit("README.md", "Base branch.\n")
        self._run(["git", "push", str(self.remote), "HEAD:refs/heads/master"])
        self._run(["git", "switch", "-c", "feature"])
        self._commit("earlier.rs", 'fn main(){println!("earlier");}\n')
        self._commit("notes.md", "Documentation-only tip.\n")

        refused = self._push("multi-commit-rust")

        self.assertEqual(4, refused.returncode, refused.stdout + refused.stderr)
        self.assertIn("format CHECK FAILED in: earlier.rs", refused.stdout)
        self.assertIsNone(self._remote_head("multi-commit-rust"))
        self.assertEqual(["earlier.rs"], self.rustfmt_log.read_text().splitlines())

    def test_non_head_source_with_rust_is_refused_before_formatting(self) -> None:
        source = self._commit("source.rs", 'fn main(){println!("source");}\n')
        self._commit("README.md", "Checked-out tip differs from source.\n")

        refused = self._push("non-head-source", source=source)

        self.assertEqual(4, refused.returncode, refused.stdout + refused.stderr)
        self.assertIn("source commit is not the checked-out HEAD", refused.stdout)
        self.assertIsNone(self._remote_head("non-head-source"))
        self.assertFalse(self.rustfmt_log.exists())

    def test_dirty_pushed_rust_is_refused_before_formatting(self) -> None:
        self._commit("dirty.rs", 'fn main(){println!("committed");}\n')
        (self.checkout / "dirty.rs").write_text(
            'fn main() {\n    println!("working tree");\n}\n'
        )

        refused = self._push("dirty-rust")

        self.assertEqual(4, refused.returncode, refused.stdout + refused.stderr)
        self.assertIn("pushed Rust files differ", refused.stdout)
        self.assertIsNone(self._remote_head("dirty-rust"))
        self.assertFalse(self.rustfmt_log.exists())


class GitPushVerifiedMainOnlyTests(unittest.TestCase):
    """dev-hermit and agent-utils are main-only: no branch but main (and the
    receipt store) may be pushed there. Other repositories are unaffected.

    The remote is configured with its REAL GitHub URL and redirected to a local
    bare repository through `insteadOf`, so the refusal is keyed on the
    repository a push is addressed to while allowed pushes really land.
    """

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="git-push-verified-main-only-")
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        self.hooks = self.root / "empty-hooks"
        self.hooks.mkdir()
        self._git(["init", "--bare", str(self.remote)], cwd=self.root)
        self._git(["init", "-b", "main", str(self.checkout)], cwd=self.root)
        for key, value in (
            ("user.name", "Test Author"),
            ("user.email", "test@example.com"),
            ("core.hooksPath", str(self.hooks)),
        ):
            self._git(["config", key, value])
        (self.checkout / "notes.txt").write_text("content\n")
        self._git(["add", "notes.txt"])
        self._git(["commit", "-m", "seed"])
        self.head = self._git(["rev-parse", "HEAD"]).stdout.strip()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _git(self, args: list[str], *, cwd: Path | None = None):
        return subprocess.run(
            ["git", *args], cwd=cwd or self.checkout, check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20,
        )

    def _address(self, url: str) -> None:
        self._git(["remote", "add", "origin", url])
        self._git(["config", f"url.{self.remote}.insteadOf", url])

    def _push(self, refspec: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(SCRIPT), "origin", refspec], cwd=self.checkout, check=False,
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20,
        )

    def _remote_refs(self) -> list[str]:
        return self._git(
            ["for-each-ref", "--format=%(refname)"], cwd=self.remote
        ).stdout.splitlines()

    def test_main_only_repositories_refuse_a_feature_branch_before_pushing(self) -> None:
        for url in (
            "https://github.com/rrnewton/dev-hermit.git",
            "git@github.com:rrnewton/dev-hermit.git",
            "https://github.com/rrnewton/agent-utils",
            "ssh://git@github.com/RRNewton/Agent-Utils.git/",
        ):
            for refspec in ("HEAD:feature", "HEAD:refs/heads/salvage/x/y",
                            "HEAD:refs/heads/rescue/auto-abc"):
                with self.subTest(url=url, refspec=refspec):
                    self.tearDown()
                    self.setUp()
                    self._address(url)
                    refused = self._push(refspec)
                    self.assertEqual(5, refused.returncode, refused.stdout + refused.stderr)
                    self.assertIn("MAIN-ONLY", refused.stderr)
                    self.assertIn("Nothing was pushed", refused.stderr)
                    self.assertEqual([], self._remote_refs())

    def test_main_only_repository_still_accepts_main_and_the_receipt_store(self) -> None:
        self._address("https://github.com/rrnewton/dev-hermit.git")
        for refspec, ref in (("HEAD:main", "refs/heads/main"),
                             ("HEAD:refs/heads/validation-receipts",
                              "refs/heads/validation-receipts")):
            with self.subTest(refspec=refspec):
                passed = self._push(refspec)
                self.assertEqual(0, passed.returncode, passed.stdout + passed.stderr)
                self.assertIn(ref, self._remote_refs())

    def test_other_repositories_keep_branch_pushes(self) -> None:
        for url in ("https://github.com/rrnewton/hermit.git",
                    "git@github.com:rrnewton/dev-hermit-fork.git",
                    "https://github.com/other/dev-hermit.git"):
            with self.subTest(url=url):
                self.tearDown()
                self.setUp()
                self._address(url)
                passed = self._push("HEAD:feature")
                self.assertEqual(0, passed.returncode, passed.stdout + passed.stderr)
                self.assertEqual(["refs/heads/feature"], self._remote_refs())


if __name__ == "__main__":
    unittest.main()
