#!/usr/bin/env python3
"""Installation contract for the independent wrkslots audit timer."""

from __future__ import annotations

import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts/install-wrkslots-audit.sh"
BOOTSTRAP_SOURCE = ROOT / "scripts/hermit-health-tick.sh"


class InstallWrkslotsAuditTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
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

    def install(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(INSTALLER), *args],
            cwd=ROOT,
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_default_install_is_verified_and_only_enables(self) -> None:
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
        unit_dir = self.config / "systemd/user"
        service = (unit_dir / "hermit-wrkslots-audit.service").read_text()
        timer = (unit_dir / "hermit-wrkslots-audit.timer").read_text()
        health_service = (unit_dir / "hermit-health-tick.service").read_text()
        health_timer = (unit_dir / "hermit-health-tick.timer").read_text()
        self.assertIn(
            "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
            "--run-current-tool",
            service,
        )
        self.assertIn("--tool-relative ci-hub/bin/wrkslots-audit", service)
        self.assertIn("KillMode=control-group", service)
        self.assertIn("Nice=19", service)
        self.assertIn("IOSchedulingClass=idle", service)
        self.assertIn("OnCalendar=*:07", timer)
        self.assertNotIn("OnActiveSec=", timer)
        self.assertIn("Persistent=true", timer)
        self.assertNotIn("OnUnitActiveSec", timer)
        self.assertIn(
            "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
            "--run-current-tool --state-root %h/work/dev-hermit "
            "--tool-relative scripts/hermit-operational-tick",
            health_service,
        )
        self.assertIn(
            "Environment=FLEET_REAL_GH=%h/.local/libexec/dev-hermit/gh",
            health_service,
        )
        self.assertIn(
            "Environment=PATH=%h/.local/libexec/dev-hermit:%h/bin:",
            health_service,
        )
        self.assertEqual(
            (ROOT / "scripts/systemd/hermit-health-tick.timer").read_text(),
            health_timer,
        )
        self.assertIn("OnUnitActiveSec=30min", health_timer)
        self.assertIn("Persistent=true", health_timer)
        self.assertEqual(
            [
                "--user daemon-reload",
                "--user enable hermit-wrkslots-audit.timer hermit-health-tick.timer",
            ],
            self.log.read_text().splitlines(),
        )
        self.assertIn("timers enabled but not started", result.stdout)
        self.assertIn("source_parent_sha=", result.stdout)
        self.assertIn("bootstrap_sha256=", result.stdout)
        self.assertIn("github_helper_sha256=", result.stdout)

    def test_start_is_explicit(self) -> None:
        result = self.install("--start")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            [
                "--user daemon-reload",
                "--user enable hermit-wrkslots-audit.timer hermit-health-tick.timer",
                "--user restart hermit-wrkslots-audit.timer hermit-health-tick.timer",
                "--user status --no-pager hermit-wrkslots-audit.timer hermit-health-tick.timer",
            ],
            self.log.read_text().splitlines(),
        )

    def test_unknown_argument_refuses_before_systemd(self) -> None:
        result = self.install("--unexpected")

        self.assertEqual(2, result.returncode)
        self.assertIn("usage:", result.stderr)
        self.assertFalse(self.log.exists())

    def test_missing_github_helper_refuses_before_installation(self) -> None:
        (self.home / "bin" / "gh").unlink()

        result = self.install()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            "required GitHub credential helper is unavailable", result.stderr
        )
        self.assertIn(str(self.home / "bin" / "gh"), result.stderr)
        self.assertFalse(
            (self.home / ".local/libexec/dev-hermit/operational-tool").exists()
        )
        self.assertFalse(
            (self.home / ".local/libexec/dev-hermit/gh").exists()
        )
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
