#!/usr/bin/env python3
"""Pin-source and code-identity checks for the agent-utils tool adapter."""

from __future__ import annotations

import os
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
AGENT_TOOL = ROOT / "ci-hub" / "bin" / "agent-tool"


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout.strip()


class AgentToolPinEquivalenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="agent-tool-pin-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.parent = self.base / "parent"
        self.parent_remote = self.base / "parent-origin.git"
        self.tool_root = self.base / "current-tool-root"
        self.hermit = self.parent / "hermit"
        self.agent_utils = self.hermit / "agent-utils"

        self.agent_utils.mkdir(parents=True)
        git(self.agent_utils, "init", "-q")
        git(self.agent_utils, "config", "user.name", "Agent Tool Test")
        git(self.agent_utils, "config", "user.email", "agent-tool@example.invalid")
        (self.agent_utils / "py" / "tick_hub").mkdir(parents=True)
        (self.agent_utils / "py" / "dagrun").mkdir(parents=True)
        (self.agent_utils / "py" / "bin").mkdir(parents=True)
        (self.agent_utils / "py" / "tick_hub" / "__main__.py").write_text(
            "import sys\nprint('tick-hub ran ' + ' '.join(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        (self.agent_utils / "py" / "dagrun" / "__main__.py").write_text(
            "import sys\nprint('dagrun ran ' + ' '.join(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        (self.agent_utils / "py" / "bin" / "tick-hub").symlink_to("../tick_hub/__main__.py")
        (self.agent_utils / "py" / "bin" / "dagrun").symlink_to("../dagrun/__main__.py")
        git(self.agent_utils, "add", "py")
        git(self.agent_utils, "commit", "-qm", "expected tools")
        self.expected_agent_utils = git(self.agent_utils, "rev-parse", "HEAD")

        (self.agent_utils / "unrelated.txt").write_text("new work\n", encoding="utf-8")
        git(self.agent_utils, "add", "unrelated.txt")
        git(self.agent_utils, "commit", "-qm", "change outside tick-hub")
        self.same_tick_agent_utils = git(self.agent_utils, "rev-parse", "HEAD")

        (self.agent_utils / "py" / "tick_hub" / "__main__.py").write_text(
            "import sys\nprint('changed tick-hub ' + ' '.join(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        git(self.agent_utils, "add", "py/tick_hub/__main__.py")
        git(self.agent_utils, "commit", "-qm", "change tick-hub")
        self.changed_tick_agent_utils = git(self.agent_utils, "rev-parse", "HEAD")

        git(self.agent_utils, "checkout", "-q", self.expected_agent_utils)
        git(self.hermit, "init", "-q")
        git(self.hermit, "config", "user.name", "Agent Tool Test")
        git(self.hermit, "config", "user.email", "agent-tool@example.invalid")
        git(self.hermit, "add", "agent-utils")
        git(self.hermit, "commit", "-qm", "record expected agent-utils")
        self.parent_hermit = git(self.hermit, "rev-parse", "HEAD")

        git(self.agent_utils, "checkout", "-q", self.same_tick_agent_utils)
        git(self.hermit, "add", "agent-utils")
        git(self.hermit, "commit", "-qm", "advance agent-utils outside tick-hub")
        self.same_tick_hermit = git(self.hermit, "rev-parse", "HEAD")

        git(self.agent_utils, "checkout", "-q", self.changed_tick_agent_utils)
        git(self.hermit, "add", "agent-utils")
        git(self.hermit, "commit", "-qm", "advance tick-hub")
        self.changed_tick_hermit = git(self.hermit, "rev-parse", "HEAD")

        (self.parent / "ci-hub" / "bin").mkdir(parents=True)
        (self.parent / "ci-hub" / "health").mkdir()
        shutil.copy2(AGENT_TOOL, self.parent / "ci-hub" / "bin" / "agent-tool")
        shutil.copy2(
            ROOT / "ci-hub" / "health" / "requirements.txt",
            self.parent / "ci-hub" / "health" / "requirements.txt",
        )
        (self.parent / "agent-utils").symlink_to("hermit/agent-utils")
        (self.parent / "state-marker.txt").write_text("clean\n", encoding="utf-8")
        git(self.parent, "init", "-q")
        git(self.parent, "config", "user.name", "Agent Tool Test")
        git(self.parent, "config", "user.email", "agent-tool@example.invalid")
        git(
            self.parent,
            "add",
            "ci-hub/bin/agent-tool",
            "ci-hub/health/requirements.txt",
            "agent-utils",
            "state-marker.txt",
        )
        git(
            self.parent,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{self.parent_hermit},hermit",
        )
        git(self.parent, "commit", "-qm", "record parent Hermit pin")
        self.stale_parent = git(self.parent, "rev-parse", "HEAD")

        git(self.base, "init", "-q", "--bare", "--initial-branch=main", self.parent_remote)
        git(self.parent, "remote", "add", "origin", self.parent_remote)
        git(self.parent, "push", "-q", "-u", "origin", "main")

        git(
            self.parent,
            "update-index",
            "--cacheinfo",
            f"160000,{self.changed_tick_hermit},hermit",
        )
        git(self.parent, "commit", "-qm", "record current Hermit pin")
        self.current_parent = git(self.parent, "rev-parse", "HEAD")
        git(
            self.parent,
            "worktree",
            "add",
            "-q",
            "--detach",
            self.tool_root,
            self.current_parent,
        )
        git(self.parent, "reset", "-q", "--hard", self.stale_parent)
        (self.parent / "state-marker.txt").write_text("dirty primary\n", encoding="utf-8")

    def _select(self, hermit: str, agent_utils: str) -> None:
        git(self.hermit, "checkout", "-q", hermit)
        git(self.agent_utils, "checkout", "-q", agent_utils)

    def _materialize_tool_checkout(self) -> None:
        tool_hermit = self.tool_root / "hermit"
        shutil.rmtree(tool_hermit)
        git(self.base, "clone", "-q", "--no-local", self.hermit, tool_hermit)
        git(tool_hermit, "checkout", "-q", self.changed_tick_hermit)
        tool_agent_utils = tool_hermit / "agent-utils"
        shutil.rmtree(tool_agent_utils)
        git(self.base, "clone", "-q", "--no-local", self.agent_utils, tool_agent_utils)
        git(tool_agent_utils, "checkout", "-q", self.changed_tick_agent_utils)

    def _run(
        self,
        tool: str,
        *,
        executable_root: Path | None = None,
        explicit_tool_root: bool = True,
        argument: str = "marker",
        environment_overrides: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        environment = dict(os.environ)
        environment.pop("DEV_HERMIT_PARENT", None)
        environment.pop("DEV_HERMIT_TOOL_ROOT", None)
        environment["CI_HUB_PYTHON"] = sys.executable
        root = executable_root or self.parent
        if executable_root is not None:
            environment["DEV_HERMIT_PARENT"] = str(self.parent)
            if explicit_tool_root:
                environment["DEV_HERMIT_TOOL_ROOT"] = str(executable_root)
        if environment_overrides:
            environment.update(environment_overrides)
        return subprocess.run(
            [str(root / "ci-hub" / "bin" / "agent-tool"), tool, argument],
            cwd=self.parent,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )

    def _publish_current_parent(self) -> None:
        git(self.tool_root, "push", "-q", "origin", "HEAD:main")
        git(
            self.parent,
            "update-ref",
            "refs/remotes/origin/main",
            self.stale_parent,
        )

    def test_tick_runs_when_its_complete_code_matches_the_parent_pin(self) -> None:
        self._select(self.same_tick_hermit, self.same_tick_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "tick-hub ran marker\n")
        self.assertIn("complete tracked executable/package is byte-identical", result.stderr)
        self.assertEqual(git(self.hermit, "rev-parse", "HEAD"), self.same_tick_hermit)
        self.assertEqual(
            git(self.agent_utils, "rev-parse", "HEAD"), self.same_tick_agent_utils
        )

    def test_tick_refuses_when_its_code_differs_from_the_parent_pin(self) -> None:
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("agent-utils pin mismatch", result.stderr)

    def test_tick_refuses_an_untracked_module_inside_its_package(self) -> None:
        self._select(self.same_tick_hermit, self.same_tick_agent_utils)
        (self.agent_utils / "py" / "tick_hub" / "untracked.py").write_text(
            "raise RuntimeError('not parent-recorded')\n",
            encoding="utf-8",
        )

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("agent-utils pin mismatch", result.stderr)

    def test_actor_still_refuses_even_when_tick_code_matches(self) -> None:
        self._select(self.same_tick_hermit, self.same_tick_agent_utils)

        result = self._run("dagrun")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("agent-utils pin mismatch", result.stderr)

    def test_exact_parent_pin_runs_without_the_tolerance_path(self) -> None:
        self._select(self.parent_hermit, self.expected_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "tick-hub ran marker\n")
        self.assertNotIn("byte-identical", result.stderr)

    def test_stale_dirty_state_root_uses_current_immutable_tool_root_pin(self) -> None:
        git(self.parent, "remote", "set-url", "origin", self.base / "unreachable.git")
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)
        self._materialize_tool_checkout()
        status_before = git(self.parent, "status", "--porcelain=v1")
        self.assertIn("hermit", status_before)

        result = self._run("tick-hub", executable_root=self.tool_root)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "changed tick-hub marker\n")
        self.assertIn("immutable tool root", result.stderr)
        self.assertIn(self.current_parent, result.stderr)
        self.assertIn(self.stale_parent, result.stderr)
        self.assertIn("state root HEAD", result.stderr)
        self.assertIn("remains untouched", result.stderr)
        self.assertEqual(git(self.parent, "rev-parse", "HEAD"), self.stale_parent)
        self.assertEqual(
            (self.parent / "state-marker.txt").read_text(encoding="utf-8"),
            "dirty primary\n",
        )
        self.assertEqual(git(self.parent, "status", "--porcelain=v1"), status_before)

    def test_stale_dirty_state_root_freshly_fetches_origin_main_pin(self) -> None:
        self._publish_current_parent()
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)
        status_before = git(self.parent, "status", "--porcelain=v1")
        self.assertIn("hermit", status_before)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "changed tick-hub marker\n")
        self.assertIn("freshly fetched origin/main", result.stderr)
        self.assertIn(self.current_parent, result.stderr)
        self.assertEqual(git(self.parent, "rev-parse", "HEAD"), self.stale_parent)
        self.assertEqual(
            (self.parent / "state-marker.txt").read_text(encoding="utf-8"),
            "dirty primary\n",
        )
        self.assertEqual(
            git(
                self.parent,
                "for-each-ref",
                "--format=%(refname)",
                "refs/ci-hub/agent-tool-pin",
            ),
            "",
        )
        self.assertEqual(
            git(self.parent, "rev-parse", "refs/remotes/origin/main"),
            self.stale_parent,
        )
        self.assertEqual(git(self.parent, "status", "--porcelain=v1"), status_before)

    def test_help_defaults_to_tool_checkout_without_initializing_state(self) -> None:
        self._publish_current_parent()
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)
        self._materialize_tool_checkout()
        agent_remote = self.base / "agent-utils-origin.git"
        git(self.base, "clone", "-q", "--bare", self.agent_utils, agent_remote)
        (self.hermit / ".gitmodules").write_text(
            '[submodule "agent-utils"]\n'
            "\tpath = agent-utils\n"
            f"\turl = {agent_remote}\n",
            encoding="utf-8",
        )
        git(self.hermit, "config", "submodule.agent-utils.active", "true")
        git(self.hermit, "config", "submodule.agent-utils.url", agent_remote)
        shutil.rmtree(self.agent_utils)
        self.agent_utils.mkdir()
        fetch_head = Path(git(self.parent, "rev-parse", "--git-path", "FETCH_HEAD"))
        if not fetch_head.is_absolute():
            fetch_head = self.parent / fetch_head
        fetch_head.write_text("caller-owned fetch head\n", encoding="utf-8")

        def snapshot() -> tuple[object, ...]:
            return (
                git(self.parent, "rev-parse", "HEAD"),
                git(self.parent, "status", "--porcelain=v1"),
                git(self.parent, "rev-parse", "refs/remotes/origin/main"),
                fetch_head.read_bytes(),
                (self.parent / "state-marker.txt").read_bytes(),
                git(self.hermit, "rev-parse", "HEAD"),
                git(self.hermit, "status", "--porcelain=v1"),
                (self.hermit / ".git" / "config").read_bytes(),
                tuple(self.agent_utils.iterdir()),
                (self.hermit / ".git" / "modules" / "agent-utils").exists(),
                git(
                    self.parent,
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/ci-hub/agent-tool-pin",
                ),
            )

        before = snapshot()
        result = self._run(
            "tick-hub",
            executable_root=self.tool_root,
            explicit_tool_root=False,
            argument="--help",
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "changed tick-hub --help\n")
        self.assertIn("freshly fetched origin/main", result.stderr)
        self.assertIn(self.current_parent, result.stderr)
        self.assertEqual(snapshot(), before)

    def test_missing_yaml_uses_external_cache_with_read_only_tool_root(self) -> None:
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)
        self._materialize_tool_checkout()
        fake_python = self.base / "python-without-yaml"
        fake_python.write_text(
            "#!/usr/bin/env python3\n"
            "import os, pathlib, shlex, sys\n"
            f"real = {sys.executable!r}\n"
            "if sys.argv[1:] == ['-c', 'import yaml']:\n"
            "    raise SystemExit(1)\n"
            "if sys.argv[1:3] == ['-m', 'venv']:\n"
            "    target = pathlib.Path(sys.argv[3])\n"
            "    (target / 'bin').mkdir(parents=True)\n"
            "    runner = target / 'bin' / 'python'\n"
            "    runner.write_text('#!/bin/sh\\nexec ' + shlex.quote(real) + ' \"$@\"\\n')\n"
            "    runner.chmod(0o755)\n"
            "    raise SystemExit(0)\n"
            "os.execv(real, [real, *sys.argv[1:]])\n",
            encoding="utf-8",
        )
        fake_python.chmod(0o755)
        dependency_cache = self.base / "agent-tool-dependencies"
        state_before = (
            git(self.parent, "rev-parse", "HEAD"),
            git(self.parent, "status", "--porcelain=v1"),
            git(self.hermit, "rev-parse", "HEAD"),
            git(self.hermit, "status", "--porcelain=v1"),
        )
        tool_status_before = git(self.tool_root, "status", "--porcelain=v1")
        subprocess.run(["chmod", "-R", "a-w", str(self.tool_root)], check=True)
        self.addCleanup(
            subprocess.run,
            ["chmod", "-R", "u+w", str(self.tool_root)],
            check=False,
        )

        result = self._run(
            "tick-hub",
            executable_root=self.tool_root,
            argument="--help",
            environment_overrides={
                "CI_HUB_PYTHON": str(fake_python),
                "CI_HUB_AGENT_TOOL_CACHE": str(dependency_cache),
            },
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "changed tick-hub --help\n")
        self.assertEqual(tool_status_before, git(self.tool_root, "status", "--porcelain=v1"))
        self.assertEqual(
            state_before,
            (
                git(self.parent, "rev-parse", "HEAD"),
                git(self.parent, "status", "--porcelain=v1"),
                git(self.hermit, "rev-parse", "HEAD"),
                git(self.hermit, "status", "--porcelain=v1"),
            ),
        )
        self.assertFalse((self.tool_root / ".tick-hub").exists())
        self.assertFalse((self.parent / ".tick-hub").exists())
        environments = list(dependency_cache.glob("pyyaml-*/bin/python"))
        self.assertEqual(len(environments), 1, environments)

    def test_fetch_failure_never_falls_back_to_stale_state_head(self) -> None:
        git(self.parent, "remote", "set-url", "origin", self.base / "unreachable.git")
        self._select(self.parent_hermit, self.expected_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("live origin/main query failed", result.stderr)
        self.assertIn("stale local HEAD was not used", result.stderr)
        self.assertNotIn("tick-hub ran", result.stdout + result.stderr)

    def test_uninitialized_tool_root_fails_without_touching_state(self) -> None:
        status_before = git(self.parent, "status", "--porcelain=v1")

        result = self._run("tick-hub", executable_root=self.tool_root)

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("immutable tool checkout", result.stderr)
        self.assertIn("is not initialized", result.stderr)
        self.assertIn(f"state root {self.parent} was not modified", result.stderr)
        self.assertEqual(git(self.parent, "status", "--porcelain=v1"), status_before)

    def test_sigterm_after_private_ref_creation_removes_only_that_ref(self) -> None:
        self._publish_current_parent()
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)
        status_before = git(self.parent, "status", "--porcelain=v1")
        origin_main_before = git(self.parent, "rev-parse", "refs/remotes/origin/main")
        fetch_head = Path(git(self.parent, "rev-parse", "--git-path", "FETCH_HEAD"))
        if not fetch_head.is_absolute():
            fetch_head = self.parent / fetch_head
        fetch_head.write_text("caller-owned fetch head\n", encoding="utf-8")

        fake_bin = self.base / "interrupt-bin"
        fake_bin.mkdir()
        marker = self.base / "pin-resolution-blocked"
        real_git = shutil.which("git")
        assert real_git is not None
        fake_git = fake_bin / "git"
        fake_git.write_text(
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            f"if [[ ${{1:-}} == -C && ${{3:-}} == ls-tree && ${{4:-}} == {shlex.quote(self.current_parent)} && ${{6:-}} == hermit ]]; then\n"
            f"    printf ready > {shlex.quote(str(marker))}\n"
            "    sleep 60\n"
            "fi\n"
            f"exec {shlex.quote(real_git)} \"$@\"\n",
            encoding="utf-8",
        )
        fake_git.chmod(0o755)
        fake_proxy = fake_bin / "with-proxy"
        fake_proxy.write_text("#!/bin/sh\nexec \"$@\"\n", encoding="utf-8")
        fake_proxy.chmod(0o755)

        environment = dict(os.environ)
        environment["CI_HUB_PYTHON"] = sys.executable
        environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
        process = subprocess.Popen(
            [str(self.parent / "ci-hub" / "bin" / "agent-tool"), "tick-hub", "marker"],
            cwd=self.parent,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 10
            private_refs = ""
            while time.monotonic() < deadline:
                private_refs = git(
                    self.parent,
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/ci-hub/agent-tool-pin",
                )
                if marker.exists() and private_refs:
                    break
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "preflight never reached delayed pin resolution")
            self.assertNotEqual(private_refs, "", "private ref was never created")
            os.killpg(process.pid, signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=10)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()

        self.assertEqual(process.returncode, 143, stderr)
        self.assertEqual(stdout, "")
        self.assertEqual(
            git(
                self.parent,
                "for-each-ref",
                "--format=%(refname)",
                "refs/ci-hub/agent-tool-pin",
            ),
            "",
        )
        self.assertEqual(fetch_head.read_text(encoding="utf-8"), "caller-owned fetch head\n")
        self.assertEqual(
            git(self.parent, "rev-parse", "refs/remotes/origin/main"),
            origin_main_before,
        )
        self.assertEqual(git(self.parent, "status", "--porcelain=v1"), status_before)
        self.assertEqual(
            (self.parent / "state-marker.txt").read_text(encoding="utf-8"),
            "dirty primary\n",
        )

    def test_tool_root_pin_missing_from_tool_hermit_fails_loudly(self) -> None:
        unavailable_hermit = "1" * 40
        git(
            self.parent,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{unavailable_hermit},hermit",
        )
        git(self.parent, "commit", "-qm", "record unavailable Hermit pin")
        unavailable_parent = git(self.parent, "rev-parse", "HEAD")
        unavailable_root = self.base / "unavailable-tool-root"
        git(
            self.parent,
            "worktree",
            "add",
            "-q",
            "--detach",
            unavailable_root,
            unavailable_parent,
        )
        unavailable_tool_hermit = unavailable_root / "hermit"
        shutil.rmtree(unavailable_tool_hermit)
        git(
            self.base,
            "clone",
            "-q",
            "--no-local",
            self.hermit,
            unavailable_tool_hermit,
        )
        git(self.parent, "reset", "-q", "--hard", self.stale_parent)
        (self.parent / "state-marker.txt").write_text("dirty primary\n", encoding="utf-8")
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)
        status_before = git(self.parent, "status", "--porcelain=v1")

        result = self._run("tick-hub", executable_root=unavailable_root)

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("cannot reconcile immutable tool root", result.stderr)
        self.assertIn(unavailable_parent, result.stderr)
        self.assertIn(unavailable_hermit, result.stderr)
        self.assertIn(str(unavailable_tool_hermit), result.stderr)
        self.assertIn(
            f"with-proxy git -C {unavailable_tool_hermit} fetch origin {unavailable_hermit}",
            result.stderr,
        )
        self.assertIn("no checkout was moved", result.stderr)
        self.assertEqual(git(self.parent, "rev-parse", "HEAD"), self.stale_parent)
        self.assertEqual(
            (self.parent / "state-marker.txt").read_text(encoding="utf-8"),
            "dirty primary\n",
        )
        self.assertEqual(git(self.parent, "status", "--porcelain=v1"), status_before)


if __name__ == "__main__":
    unittest.main()
