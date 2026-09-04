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
        self.assertEqual(BOOTSTRAP_SOURCE.read_bytes(), bootstrap.read_bytes())
        self.assertTrue(os.access(bootstrap, os.X_OK))
        self.assertFalse(bootstrap.stat().st_mode & stat.S_IWUSR)
        unit_dir = self.config / "systemd/user"
        service = (unit_dir / "hermit-wrkslots-audit.service").read_text()
        timer = (unit_dir / "hermit-wrkslots-audit.timer").read_text()
        self.assertIn(
            "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
            "--run-current-tool",
            service,
        )
        self.assertIn("--tool-relative ci-hub/bin/wrkslots-audit", service)
        self.assertIn("KillMode=control-group", service)
        self.assertIn("Nice=19", service)
        self.assertIn("IOSchedulingClass=idle", service)
        self.assertIn("OnCalendar=hourly", timer)
        self.assertIn("OnActiveSec=30s", timer)
        self.assertIn("Persistent=true", timer)
        self.assertNotIn("OnUnitActiveSec", timer)
        self.assertEqual(
            [
                "--user daemon-reload",
                "--user enable hermit-wrkslots-audit.timer",
            ],
            self.log.read_text().splitlines(),
        )
        self.assertIn("timer enabled but not started", result.stdout)
        self.assertIn("source_parent_sha=", result.stdout)
        self.assertIn("bootstrap_sha256=", result.stdout)

    def test_start_is_explicit(self) -> None:
        result = self.install("--start")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            [
                "--user daemon-reload",
                "--user enable hermit-wrkslots-audit.timer",
                "--user start hermit-wrkslots-audit.timer",
                "--user status --no-pager hermit-wrkslots-audit.timer",
            ],
            self.log.read_text().splitlines(),
        )

    def test_unknown_argument_refuses_before_systemd(self) -> None:
        result = self.install("--unexpected")

        self.assertEqual(2, result.returncode)
        self.assertIn("usage:", result.stderr)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
