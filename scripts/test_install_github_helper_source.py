#!/usr/bin/env python3
"""How both installers choose the GitHub credential helper source.

THE DEFECT THIS GUARDS. Measured 2026-09-05 on devbig014: both installers read a
single fixed ``$HOME/bin/gh``, that path was a symlink into ``/home/newton/opt``,
and ``/home/newton/opt`` did not exist. The link dangled, so
``~/.local/libexec/dev-hermit/gh`` was absent, ``hermit-health-tick.sh`` refused
every networked git call, and the tick reported no signal for GitHub instead of a
verdict. Rerunning either installer could not repair it, because both read that
same missing path.

WHY THIS FILE SITS IN ``scripts/`` AND NOT ``scripts/tests/``. The Makefile's
``py-unittest`` line runs ``python3 -m unittest discover -s scripts -p
"test_*.py"``, and ``scripts/tests/`` has no ``__init__.py``, so discovery
reports ``Start directory is not importable`` and never descends into it.
Measured 2026-09-05: the 24 ``test_*.py`` files directly under ``scripts/`` are
discovered and the 6 under ``scripts/tests/`` are not. The two per-installer test
files live there and are therefore unscheduled today, so a test placed beside
them would not run. That gap is filed separately; this file is placed where the
counter already looks rather than depending on it being fixed first.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
INSTALLERS = {
    "owner-watch": ROOT / "scripts/install-owner-checkout-validate-watch.sh",
    "wrkslots-audit": ROOT / "scripts/install-wrkslots-audit.sh",
}

#: The resolution block runs from this comment line to the first line that is
#: exactly ``fi``. It is duplicated rather than sourced because this repository
#: has no shared shell library and introducing one for two callers would add a
#: file whose absence breaks both.
BLOCK_START = "# ⚠️ A SINGLE FIXED PATH UNDER $HOME WAS THE FAILURE"


def resolution_block(path: Path) -> str:
    lines = path.read_text().splitlines(keepends=True)
    for index, line in enumerate(lines):
        if line.startswith(BLOCK_START):
            break
    else:
        raise AssertionError(f"{path}: no credential-helper resolution block found")
    for end in range(index, len(lines)):
        if lines[end] == "fi\n":
            return "".join(lines[index : end + 1])
    raise AssertionError(f"{path}: resolution block is unterminated")


class BothInstallersShareOneResolution(unittest.TestCase):
    """Two copies that drift is the shape this repository keeps finding."""

    def test_the_resolution_block_is_byte_identical_in_both_installers(self) -> None:
        owner = resolution_block(INSTALLERS["owner-watch"])
        wrkslots = resolution_block(INSTALLERS["wrkslots-audit"])
        self.assertEqual(owner, wrkslots)
        # An empty or trivial match would satisfy equality without covering
        # anything, so require the block to actually contain the ordered list.
        self.assertIn('github_helper_candidates=(/usr/bin/gh /usr/local/bin/gh "$HOME/bin/gh")', owner)

    def test_the_package_path_is_tried_before_the_home_path(self) -> None:
        """Ordering IS the fix; a list containing both in the wrong order is not."""
        block = resolution_block(INSTALLERS["owner-watch"])
        default_list = re.search(
            r"github_helper_candidates=\((?P<entries>[^)]*)\)\nfi", block
        )
        self.assertIsNotNone(default_list, block)
        entries = default_list.group("entries").split()
        self.assertLess(
            entries.index("/usr/bin/gh"),
            entries.index('"$HOME/bin/gh"'),
            f"the package-managed path must precede the hand-made one: {entries}",
        )


class HelperSourceResolution(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        base = Path(self.temporary.name)
        self.home = base / "home"
        self.config = base / "config"
        self.fake_bin = base / "bin"
        self.log = base / "systemctl.log"
        self.home.mkdir()
        self.fake_bin.mkdir()
        (self.home / "bin").mkdir()

        self.fixture = base / "fixture-gh"
        self.fixture.write_text("#!/bin/sh\nexit 0\n")
        self.fixture.chmod(0o755)

        systemctl = self.fake_bin / "systemctl"
        systemctl.write_text(
            "#!/usr/bin/env bash\nset -eu\n"
            'printf \'%s\\n\' "$*" >> "$FAKE_SYSTEMCTL_LOG"\n'
        )
        systemctl.chmod(0o755)

        self.env = dict(
            os.environ,
            HOME=str(self.home),
            XDG_CONFIG_HOME=str(self.config),
            PATH=f"{self.fake_bin}:{os.environ.get('PATH', '')}",
            FAKE_SYSTEMCTL_LOG=str(self.log),
        )
        self.env.pop("DEV_HERMIT_GH_SOURCE", None)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def install(self, installer: Path, **overrides: str) -> subprocess.CompletedProcess[str]:
        env = dict(self.env, **overrides)
        return subprocess.run(
            [str(installer)], cwd=ROOT, env=env, text=True, capture_output=True, check=False
        )

    @staticmethod
    def reported_source(stdout: str) -> str:
        match = re.search(r"github_helper_source=(\S+)", stdout)
        assert match is not None, f"installer did not report its source:\n{stdout}"
        return match.group(1)

    def test_an_explicit_source_is_installed_as_a_regular_file(self) -> None:
        for name, installer in INSTALLERS.items():
            with self.subTest(installer=name):
                result = self.install(installer, DEV_HERMIT_GH_SOURCE=str(self.fixture))
                self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                installed = self.home / ".local/libexec/dev-hermit/gh"
                self.assertEqual(self.fixture.read_bytes(), installed.read_bytes())
                self.assertFalse(installed.is_symlink())
                self.assertTrue(os.access(installed, os.X_OK))
                self.assertFalse(installed.stat().st_mode & stat.S_IWUSR)
                self.assertEqual(str(self.fixture), self.reported_source(result.stdout))
                shutil.rmtree(self.home / ".local")

    def test_a_symlinked_source_is_dereferenced_rather_than_copied_as_a_link(self) -> None:
        """The historical shape: $HOME/bin/gh is a link at a real binary."""
        link = self.home / "bin" / "gh"
        link.symlink_to(self.fixture)
        for name, installer in INSTALLERS.items():
            with self.subTest(installer=name):
                result = self.install(installer, DEV_HERMIT_GH_SOURCE=str(link))
                self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                installed = self.home / ".local/libexec/dev-hermit/gh"
                self.assertFalse(installed.is_symlink())
                self.assertEqual(self.fixture.read_bytes(), installed.read_bytes())
                shutil.rmtree(self.home / ".local")

    def test_a_dangling_home_path_is_skipped_rather_than_fatal(self) -> None:
        """THE REGRESSION. Before this change a dangling $HOME/bin/gh exited 1.

        No override, so the real candidate list runs and must fall past the
        dangling link to a system binary. Skipped where no system gh exists,
        because the fall-through has nothing to fall to there; the hermetic
        cases above still run everywhere.
        """
        system_gh = next(
            (p for p in ("/usr/bin/gh", "/usr/local/bin/gh") if os.access(p, os.X_OK)), None
        )
        if system_gh is None:
            self.skipTest("no system gh at /usr/bin/gh or /usr/local/bin/gh")

        link = self.home / "bin" / "gh"
        link.symlink_to(self.home / "opt" / "gh-removed" / "bin" / "gh")
        self.assertFalse(link.exists(), "the fixture link must actually dangle")

        for name, installer in INSTALLERS.items():
            with self.subTest(installer=name):
                result = self.install(installer)
                self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                self.assertEqual(system_gh, self.reported_source(result.stdout))
                installed = self.home / ".local/libexec/dev-hermit/gh"
                self.assertEqual(Path(system_gh).read_bytes(), installed.read_bytes())
                self.assertFalse(installed.is_symlink())
                shutil.rmtree(self.home / ".local")

    def test_an_explicit_source_that_does_not_resolve_refuses_and_names_it(self) -> None:
        """An override REPLACES the list, so a broken one must not fall back."""
        missing = self.home / "opt" / "gh-removed" / "bin" / "gh"
        for name, installer in INSTALLERS.items():
            with self.subTest(installer=name):
                result = self.install(installer, DEV_HERMIT_GH_SOURCE=str(missing))
                self.assertEqual(1, result.returncode, result.stdout + result.stderr)
                self.assertIn("required GitHub credential helper is unavailable", result.stderr)
                self.assertIn(str(missing), result.stderr)
                self.assertFalse((self.home / ".local/libexec/dev-hermit/gh").exists())

    def test_a_source_that_is_not_executable_is_refused(self) -> None:
        plain = Path(self.temporary.name) / "not-executable"
        plain.write_text("#!/bin/sh\nexit 0\n")
        plain.chmod(0o644)
        for name, installer in INSTALLERS.items():
            with self.subTest(installer=name):
                result = self.install(installer, DEV_HERMIT_GH_SOURCE=str(plain))
                self.assertEqual(1, result.returncode, result.stdout + result.stderr)
                self.assertIn("required GitHub credential helper is unavailable", result.stderr)
                self.assertFalse((self.home / ".local/libexec/dev-hermit/gh").exists())


if __name__ == "__main__":
    unittest.main()
