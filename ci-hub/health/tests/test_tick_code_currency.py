#!/usr/bin/env python3
"""Bracket the tick-code-currency gate in a real repository, both directions.

THE PROPERTY UNDER TEST IS NOT "the current fix works". It is that a tick-hub fix
SURVIVES A `git restore` -- or rather, that it cannot be reverted SILENTLY. The
tick executes from the shared parent working tree, the parent is chronically
behind `origin/main`, and a behind-checkout renders landed content as modified.
So `git restore` is an ordinary, reasonable command that silently rewinds live
tick code. It did exactly that twice on `unpushed_parent_commits`.

The discriminator is what makes this gate usable rather than noise, so both
sides of it are asserted:

    stale     clean at HEAD, but origin/main moved -> the incident. PAGES.
    modified  differs from HEAD too -> someone's work in progress. Does NOT page.

Note the stale case is CLEAN by `git status`. That is precisely why the first two
reversions were invisible, and it is why this gate compares content against the
target ref instead of reading porcelain status.

Fixtures are self-contained git repositories in a tmpdir; nothing here touches
the real parent, its refs, or the network.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

MODULE = Path(__file__).resolve().parents[1] / "tick_code_currency.py"
SPEC = importlib.util.spec_from_file_location("tick_code_currency", MODULE)
assert SPEC and SPEC.loader
tcc = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = tcc
SPEC.loader.exec_module(tcc)


def git(*args: str, cwd: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, check=False
    ).stdout.strip()


class Repo:
    """A parent-shaped repo whose local main is BEHIND its origin -- the
    condition that makes a restore dangerous in the first place."""

    def __init__(self, tmp: Path) -> None:
        self.root = tmp / "parent"
        self.origin = tmp / "origin.git"
        (self.root / "ci-hub" / "health").mkdir(parents=True)
        subprocess.run(["git", "init", "-q", "--bare", str(self.origin)], check=True)
        subprocess.run(["git", "init", "-q", "-b", "main", str(self.root)], check=True)
        for k, v in (("user.email", "t@e.invalid"), ("user.name", "t")):
            git("config", k, v, cwd=self.root)
        git("remote", "add", "origin", str(self.origin), cwd=self.root)

        self.tool = self.root / "ci-hub" / "health" / "widget.py"
        self.config = self.root / "ci-hub" / "health" / "tick-hub.yaml"
        self.config.write_text("gates:\n  - cmd: ./ci-hub/health/widget.py --gate\n")
        self.tool.write_text("# v1 -- the buggy version\n")
        self._commit("v1")
        git("push", "-q", "origin", "main", cwd=self.root)

        # The fix lands on origin/main; local main deliberately stays behind.
        self.tool.write_text("# v2 -- the FIX\n")
        self.new_tool = self.root / "ci-hub" / "health" / "new_tool.py"
        self.new_tool.write_text("# newly landed tool\n")
        self._commit("v2 the fix")
        git("push", "-q", "origin", "main", cwd=self.root)
        git("reset", "-q", "--hard", "HEAD~1", cwd=self.root)
        git("fetch", "-q", "origin", cwd=self.root)

    def _commit(self, msg: str) -> None:
        git("add", "-A", cwd=self.root)
        git("commit", "-q", "-m", msg, cwd=self.root)

    def verdicts(self, ref: str = "origin/main"):
        tcc.ROOT = self.root
        tcc.TICK_CONFIG = self.config
        return tcc.compare(tcc.policed_paths(self.config), ref)

    def state_of(self, name: str, ref: str = "origin/main") -> str:
        for v in self.verdicts(ref):
            if v.path.endswith(name):
                return v.state
        raise AssertionError(f"{name} not policed; got {[v.path for v in self.verdicts(ref)]}")

    def state_of_path(self, path: Path, ref: str = "origin/main") -> str:
        relative = str(path.relative_to(self.root))
        return self.compare_paths([relative], ref)[0].state

    def compare_paths(self, paths: list[str], ref: str = "origin/main"):
        tcc.ROOT = self.root
        return tcc.compare(paths, ref)


class TickCodeCurrency(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="tick-currency-")
        self.addCleanup(self._tmp.cleanup)
        self.repo = Repo(Path(self._tmp.name))
        self.addCleanup(setattr, tcc, "ROOT", tcc.ROOT)
        self.addCleanup(setattr, tcc, "TICK_CONFIG", tcc.TICK_CONFIG)

    # ------------------------------------------------------------- positive --
    def test_the_planted_restore_is_caught(self) -> None:
        """THE bracket: the fix is live, `git restore` rewinds it, gate pages."""
        # Fix is live in the working tree even though local main is behind.
        self.repo.tool.write_text("# v2 -- the FIX\n")
        self.assertEqual("current", self.repo.state_of("widget.py"))

        # The exact command that caused both incidents.
        git("restore", "--worktree", "--", "ci-hub/health/widget.py", cwd=self.repo.root)

        self.assertEqual("# v1 -- the buggy version\n", self.repo.tool.read_text(),
                         "precondition: the restore really did rewind the file")
        self.assertEqual("", git("status", "--short", "--", "ci-hub/health/widget.py",
                                 cwd=self.repo.root),
                         "and `git status` calls it CLEAN -- why it was invisible")
        self.assertEqual("stale", self.repo.state_of("widget.py"))

    def test_stale_pages_and_names_the_path(self) -> None:
        code, text = tcc.render(self.repo.verdicts())
        self.assertEqual(tcc.DRIFTED, code)
        self.assertIn("widget.py", text)
        self.assertIn("summary=", text)
        self.assertNotIn("{summary}", text)

    # ------------------------------------------------------------- negative --
    def test_work_in_progress_does_not_page(self) -> None:
        """A gate that pages on ordinary editing gets ignored, and then it is
        worth nothing when the real thing happens."""
        self.repo.tool.write_text("# v3 -- someone is mid-edit\n")
        self.assertEqual("modified", self.repo.state_of("widget.py"))
        code, _ = tcc.render(self.repo.verdicts())
        self.assertEqual(tcc.CURRENT, code)

    def test_current_is_current(self) -> None:
        self.repo.tool.write_text("# v2 -- the FIX\n")
        code, text = tcc.render(self.repo.verdicts())
        self.assertEqual(tcc.CURRENT, code)
        self.assertIn("match", text)

    def test_untracked_landed_bytes_are_current(self) -> None:
        """A new file absent from stale HEAD is current when its bytes match the
        file that landed. Git's index does not represent this case."""
        self.assertFalse(self.repo.new_tool.exists())
        self.repo.new_tool.write_text("# newly landed tool\n")
        self.assertTrue(
            git("status", "--short", "--", "ci-hub/health/new_tool.py",
                cwd=self.repo.root).startswith("??"),
            "precondition: the freshly landed file is untracked by stale HEAD",
        )
        self.assertEqual("current", self.repo.state_of_path(self.repo.new_tool))

    def test_untracked_different_bytes_remain_stale(self) -> None:
        """Fixing the false alarm must not silence genuine untracked drift."""
        self.assertFalse(self.repo.new_tool.exists())
        self.repo.new_tool.write_text("# local bytes differ from landed tool\n")
        self.assertTrue(
            git("status", "--short", "--", "ci-hub/health/new_tool.py",
                cwd=self.repo.root).startswith("??"),
            "precondition: stale HEAD still does not track the path",
        )
        verdicts = self.repo.compare_paths(["ci-hub/health/new_tool.py"])
        self.assertEqual("stale", verdicts[0].state)
        code, text = tcc.render(verdicts)
        self.assertEqual(tcc.DRIFTED, code)
        self.assertIn("ci-hub/health/new_tool.py", text)

    def test_unresolvable_ref_is_unknown_never_current(self) -> None:
        """Fail closed: 'I could not check' must not read as 'it matches'."""
        self.repo.tool.write_text("# v2 -- the FIX\n")
        code, text = tcc.render(self.repo.verdicts("origin/no-such-ref"),
                                "origin/no-such-ref")
        self.assertEqual(tcc.UNKNOWN, code)
        self.assertIn("could not establish currency", text)
        self.assertTrue(all(v.state == "unknown" for v in
                            self.repo.verdicts("origin/no-such-ref")))

    def test_the_config_itself_is_policed(self) -> None:
        """A reverted tick-hub.yaml changes WHICH command runs -- that is how the
        2026-08-08 regression re-armed `--rescue` -- so the config counts too."""
        paths = tcc.policed_paths(self.repo.config)
        self.assertTrue(any(p.endswith("tick-hub.yaml") for p in paths), paths)
        self.assertTrue(any(p.endswith("widget.py") for p in paths), paths)

    def test_unparseable_config_is_unknown_not_empty_pass(self) -> None:
        """Zero policed paths must never render as 'everything matches'."""
        code, text = tcc.render(tcc.compare([]))
        self.assertEqual(tcc.UNKNOWN, code)
        self.assertIn("could not establish", text)


if __name__ == "__main__":
    unittest.main()
