#!/usr/bin/env python3
"""Installation contract for the independent wrkslots audit timer."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts/install-wrkslots-audit.sh"
BOOTSTRAP_SOURCE = ROOT / "scripts/hermit-health-tick.sh"
STALENESS_SOURCE = ROOT / "scripts/hermit-health-staleness.sh"


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
        staleness_watchdog = self.home / ".local/libexec/dev-hermit/health-staleness"
        self.assertEqual(BOOTSTRAP_SOURCE.read_bytes(), bootstrap.read_bytes())
        self.assertTrue(os.access(bootstrap, os.X_OK))
        self.assertFalse(bootstrap.stat().st_mode & stat.S_IWUSR)
        self.assertEqual(self.github_helper_source.read_bytes(), github_helper.read_bytes())
        self.assertFalse(github_helper.is_symlink())
        self.assertTrue(os.access(github_helper, os.X_OK))
        self.assertFalse(github_helper.stat().st_mode & stat.S_IWUSR)
        self.assertEqual(STALENESS_SOURCE.read_bytes(), staleness_watchdog.read_bytes())
        self.assertFalse(staleness_watchdog.is_symlink())
        self.assertTrue(os.access(staleness_watchdog, os.X_OK))
        self.assertFalse(staleness_watchdog.stat().st_mode & stat.S_IWUSR)
        unit_dir = self.config / "systemd/user"
        service = (unit_dir / "hermit-wrkslots-audit.service").read_text()
        timer = (unit_dir / "hermit-wrkslots-audit.timer").read_text()
        health_service = (unit_dir / "hermit-health-tick.service").read_text()
        health_timer = (unit_dir / "hermit-health-tick.timer").read_text()
        staleness_service = (
            unit_dir / "hermit-health-staleness.service"
        ).read_text()
        staleness_timer = (unit_dir / "hermit-health-staleness.timer").read_text()
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
            (ROOT / "scripts/systemd/hermit-health-staleness.service").read_text(),
            staleness_service,
        )
        self.assertIn(
            "ExecStart=%h/.local/libexec/dev-hermit/health-staleness",
            staleness_service,
        )
        self.assertEqual(
            (ROOT / "scripts/systemd/hermit-health-staleness.timer").read_text(),
            staleness_timer,
        )
        self.assertIn("Unit=hermit-health-staleness.service", staleness_timer)
        self.assertIn("After=hermit-health-tick.timer", staleness_timer)
        self.assertIn("OnCalendar=*:0/10", staleness_timer)
        self.assertNotIn("OnBootSec=", staleness_timer)
        self.assertNotIn("OnUnitActiveSec=", staleness_timer)
        self.assertIn("Persistent=true", staleness_timer)
        self.assertEqual(
            [
                "--user daemon-reload",
                "--user enable hermit-wrkslots-audit.timer "
                "hermit-health-tick.timer hermit-health-staleness.timer",
            ],
            self.log.read_text().splitlines(),
        )
        self.assertIn("timers enabled but not started", result.stdout)
        self.assertIn("source_parent_sha=", result.stdout)
        self.assertIn("bootstrap_sha256=", result.stdout)
        self.assertIn("github_helper_sha256=", result.stdout)
        self.assertIn("staleness_watchdog_sha256=", result.stdout)

    def test_start_is_explicit(self) -> None:
        result = self.install("--start")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            [
                "--user daemon-reload",
                "--user enable hermit-wrkslots-audit.timer "
                "hermit-health-tick.timer hermit-health-staleness.timer",
                "--user restart hermit-wrkslots-audit.timer "
                "hermit-health-tick.timer hermit-health-staleness.timer",
                "--user status --no-pager hermit-wrkslots-audit.timer "
                "hermit-health-tick.timer hermit-health-staleness.timer",
            ],
            self.log.read_text().splitlines(),
        )

    def test_installed_staleness_unit_defers_first_tick_race(self) -> None:
        result = self.install()
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

        unit_dir = self.config / "systemd/user"
        service = (unit_dir / "hermit-health-staleness.service").read_text()
        exec_start = next(
            line.removeprefix("ExecStart=").replace("%h", str(self.home))
            for line in service.splitlines()
            if line.startswith("ExecStart=")
        )
        self.assertEqual(
            str(self.home / ".local/libexec/dev-hermit/health-staleness"),
            exec_start,
        )

        watchdog_systemctl = self.bin / "watchdog-systemctl"
        watchdog_systemctl.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            'case "$*" in\n'
            '  *LastTriggerUSec*) printf "\\n" ;;\n'
            '  *ActiveEnterTimestamp*) date ;;\n'
            '  *is-active*) printf "inactive\\n"; exit 3 ;;\n'
            '  *) printf "unhandled systemctl query: %s\\n" "$*" >&2; exit 64 ;;\n'
            "esac\n"
        )
        watchdog_systemctl.chmod(0o755)
        watchdog_log = self.home / "staleness.log"
        watchdog_env = dict(
            self.env,
            HERMIT_HEALTH_HEARTBEAT=str(self.home / "missing-heartbeat"),
            HERMIT_HEALTH_FIRED_STATE=str(self.home / "missing-fired-state"),
            HERMIT_HEALTH_LOG=str(watchdog_log),
            HERMIT_HEALTH_COOLDOWN_STAMP=str(self.home / "cooldown"),
            HERMIT_HEALTH_GATED_STAMP=str(self.home / "gated"),
            HERMIT_HEALTH_ORC_MSG=str(self.home / "must-not-run-orc"),
            HERMIT_HEALTH_ESCALATE=str(self.home / "must-not-run-escalate"),
            HERMIT_HEALTH_SYSTEMCTL=str(watchdog_systemctl),
        )
        executed = subprocess.run(
            [exec_start],
            cwd=self.home,
            env=watchdog_env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, executed.returncode, executed.stdout + executed.stderr)
        watchdog_output = watchdog_log.read_text()
        self.assertIn("BOOTSTRAP-GRACE", watchdog_output)
        self.assertNotIn("ALARM", watchdog_output)
        self.assertFalse((self.home / "cooldown").exists())

    @unittest.skipUnless(shutil.which("systemd-analyze"), "systemd-analyze unavailable")
    def test_installed_staleness_units_and_calendar_parse(self) -> None:
        result = self.install()
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        unit_dir = self.config / "systemd/user"
        verify_dir = self.home / "verify-units"
        verify_dir.mkdir()
        verify_service = verify_dir / "hermit-health-staleness.service"
        verify_timer = verify_dir / "hermit-health-staleness.timer"
        verify_service.write_text(
            (unit_dir / verify_service.name).read_text().replace(
                "%h", str(self.home)
            )
        )
        verify_timer.write_bytes((unit_dir / verify_timer.name).read_bytes())

        verified = subprocess.run(
            [
                shutil.which("systemd-analyze") or "systemd-analyze",
                "--user",
                "verify",
                str(verify_service),
                str(verify_timer),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, verified.returncode, verified.stdout + verified.stderr)
        calendar = subprocess.run(
            [
                shutil.which("systemd-analyze") or "systemd-analyze",
                "calendar",
                "*:0/10",
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, calendar.returncode, calendar.stdout + calendar.stderr)
        self.assertIn("Normalized form:", calendar.stdout)

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
