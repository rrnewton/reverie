#!/usr/bin/env python3
"""Installation contract for the periodic owner-checkout validate timer."""

from __future__ import annotations

import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts/install-owner-checkout-validate-watch.sh"
BOOTSTRAP_SOURCE = ROOT / "scripts/hermit-health-tick.sh"


class InstallOwnerCheckoutValidateWatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        base = Path(self.temporary.name)
        self.home = base / "home"
        self.config = base / "config"
        self.bin = base / "bin"
        self.log = base / "systemctl.log"
        self.home.mkdir()
        self.bin.mkdir()
        self.github_helper_source = self.home / "opt" / "gh" / "bin" / "gh"
        self.github_helper_source.parent.mkdir(parents=True)
        self.github_helper_source.write_text("#!/bin/sh\nexit 0\n")
        self.github_helper_source.chmod(0o755)
        home_bin = self.home / "bin"
        home_bin.mkdir()
        (home_bin / "gh").symlink_to(self.github_helper_source)
        systemctl = self.bin / "systemctl"
        systemctl.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            'printf \'%s\\n\' "$*" >> "$FAKE_SYSTEMCTL_LOG"\n'
        )
        systemctl.chmod(0o755)
        self.env = dict(
            os.environ,
            HOME=str(self.home),
            XDG_CONFIG_HOME=str(self.config),
            PATH=f"{self.bin}:{os.environ.get('PATH', '')}",
            FAKE_SYSTEMCTL_LOG=str(self.log),
            # The installer now resolves its source from an ordered list, so a
            # test that relied on $HOME/bin/gh being the ONLY candidate would
            # silently pick up the host's /usr/bin/gh instead. Naming the source
            # explicitly keeps this hermetic and still exercises the historical
            # shape, a symlink at a real binary. Ordering and fall-through are
            # covered by scripts/test_install_github_helper_source.py.
            DEV_HERMIT_GH_SOURCE=str(self.home / "bin" / "gh"),
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def install(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(INSTALLER), *args],
            cwd=ROOT,
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_default_install_enables_but_does_not_start_timer(self) -> None:
        result = self.install()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        bootstrap = self.home / ".local/libexec/dev-hermit/operational-tool"
        github_helper = self.home / ".local/libexec/dev-hermit/gh"
        self.assertEqual(BOOTSTRAP_SOURCE.read_bytes(), bootstrap.read_bytes())
        self.assertTrue(os.access(bootstrap, os.X_OK))
        self.assertFalse(bootstrap.stat().st_mode & stat.S_IWUSR)
        self.assertEqual(self.github_helper_source.read_bytes(), github_helper.read_bytes())
        self.assertFalse(github_helper.is_symlink())
        self.assertTrue(os.access(github_helper, os.X_OK))
        self.assertFalse(github_helper.stat().st_mode & stat.S_IWUSR)
        installed_service = (
            self.config / "systemd/user/hermit-owner-checkout-validate.service"
        ).read_text()
        self.assertIn(
            "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
            "--run-current-tool",
            installed_service,
        )
        self.assertNotIn(
            "ExecStart=%h/work/dev-hermit/scripts/owner-checkout-validate-watch.py",
            installed_service,
        )
        # The detached validation child has the canonical 3600s deadline;
        # preserve the watcher's existing 600s preflight/reporting allowance.
        # This is not a measured upper bound for every external reporting call.
        self.assertIn("TimeoutStartSec=4200\n", installed_service)
        self.assertIn("SuccessExitStatus=0 1 2 3 4 6\n", installed_service)
        calls = self.log.read_text().splitlines()
        self.assertEqual(
            calls,
            [
                "--user daemon-reload",
                "--user enable hermit-owner-checkout-validate.timer",
            ],
        )
        self.assertIn("timer enabled but not started", result.stdout)
        self.assertIn("source_parent_sha=", result.stdout)
        self.assertIn("bootstrap_sha256=", result.stdout)
        self.assertIn("github_helper_sha256=", result.stdout)

    def test_start_is_explicit_and_follows_install_verification(self) -> None:
        result = self.install("--start")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        calls = self.log.read_text().splitlines()
        self.assertEqual(
            calls,
            [
                "--user daemon-reload",
                "--user enable hermit-owner-checkout-validate.timer",
                "--user start hermit-owner-checkout-validate.timer",
                "--user status --no-pager hermit-owner-checkout-validate.timer",
            ],
        )

    def test_missing_fixed_credential_helper_refuses_before_installation(self) -> None:
        missing_helper = self.home / "bin" / "gh"
        missing_helper.unlink()

        result = self.install()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            "required GitHub credential helper is unavailable", result.stderr
        )
        self.assertIn(str(missing_helper), result.stderr)
        self.assertFalse(
            (self.home / ".local/libexec/dev-hermit/operational-tool").exists()
        )
        self.assertFalse(
            (self.home / ".local/libexec/dev-hermit/gh").exists()
        )
        self.assertFalse(
            (
                self.config
                / "systemd/user/hermit-owner-checkout-validate.service"
            ).exists()
        )
        self.assertFalse(
            (
                self.config
                / "systemd/user/hermit-owner-checkout-validate.timer"
            ).exists()
        )
        self.assertFalse(self.log.exists())

    def test_unknown_argument_refuses_before_systemd(self) -> None:
        result = self.install("--unexpected")

        self.assertEqual(2, result.returncode)
        self.assertIn("usage:", result.stderr)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
