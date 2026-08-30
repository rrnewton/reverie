#!/usr/bin/env python3
"""Collision controls for the outer validate-run rust-script cache."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "ci-hub/ci-hub"


class ValidateLauncherCacheTest(unittest.TestCase):
    def test_descriptor_backed_launcher_preserves_tool_and_state_roots(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = fake_bin / "rust-script"
            fake.write_text(
                "#!/bin/bash\n"
                "set -eu\n"
                "printf '%s\\n%s\\n%s\\n' \"$2\" \"$DEV_HERMIT_TOOL_ROOT\" "
                "\"$DEV_HERMIT_PARENT\" > \"$4\"\n"
            )
            fake.chmod(0o755)
            runtime = root / "runtime"
            runtime.mkdir()
            state_root = root / "state"
            state_root.mkdir()
            capture = root / "capture"
            root_fd = os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY)
            self.addCleanup(os.close, root_fd)
            retained = Path(f"/proc/{os.getpid()}/fd/{root_fd}")
            env = {
                **os.environ,
                "PATH": f"{fake_bin}:{os.environ.get('PATH', '')}",
                "XDG_RUNTIME_DIR": str(runtime),
                "DEV_HERMIT_TOOL_ROOT": str(retained),
                "DEV_HERMIT_PARENT": str(state_root),
            }

            result = subprocess.run(
                [
                    str(retained / "ci-hub/ci-hub"),
                    "validate-run",
                    str(capture),
                ],
                env=env,
                check=False,
            )

            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                capture.read_text().splitlines(),
                [
                    str(retained / "ci-hub/ci-hub.rs"),
                    str(retained),
                    str(state_root),
                ],
            )

    def test_two_validate_launchers_do_not_share_the_outer_rust_script_cache(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = fake_bin / "rust-script"
            fake.write_text(
                "#!/bin/bash\n"
                "set -eu\n"
                "output=$4\n"
                "value=$5\n"
                "mkdir -p \"$XDG_CACHE_HOME\"\n"
                "touch \"$XDG_CACHE_HOME/started-$value\"\n"
                "if [ \"${CI_HUB_TEST_SHARED_CACHE:-0}\" = 1 ]; then\n"
                "  for unused in 1 2 3 4 5 6 7 8 9 10; do\n"
                "    set -- \"$XDG_CACHE_HOME\"/started-*\n"
                "    [ \"$#\" -ge 2 ] && break\n"
                "    sleep 0.02\n"
                "  done\n"
                "fi\n"
                "printf '%s\\n' \"$XDG_CACHE_HOME\" > \"$output\"\n"
                "printf '%s\\n' \"$value\" > \"$XDG_CACHE_HOME/same-output\"\n"
                "sleep 0.2\n"
            )
            fake.chmod(0o755)
            runtime = root / "runtime"
            runtime.mkdir()
            env = {
                **os.environ,
                "PATH": f"{fake_bin}:{os.environ.get('PATH', '')}",
                "XDG_RUNTIME_DIR": str(runtime),
            }
            output_a = root / "a.path"
            output_b = root / "b.path"

            # Removed behavior: the tracked front door was a symlink to the
            # rust-script source, so both launches inherited the same cache.
            old_launcher = root / "old-ci-hub"
            old_launcher.symlink_to(ROOT / "ci-hub/ci-hub.rs")
            old_cache = root / "old-cache"
            old_env = {
                **env,
                "XDG_CACHE_HOME": str(old_cache),
                "CI_HUB_TEST_SHARED_CACHE": "1",
            }
            first = subprocess.Popen(
                [str(old_launcher), "validate-run", str(output_a), "run-a"],
                env=old_env,
            )
            second = subprocess.Popen(
                [str(old_launcher), "validate-run", str(output_b), "run-b"],
                env=old_env,
            )
            self.assertEqual(first.wait(timeout=3), 0)
            self.assertEqual(second.wait(timeout=3), 0)
            self.assertEqual(Path(output_a.read_text().strip()), old_cache)
            self.assertEqual(Path(output_b.read_text().strip()), old_cache)
            self.assertEqual(len(list(old_cache.glob("started-*"))), 2)
            self.assertIn(
                (old_cache / "same-output").read_text().strip(),
                {"run-a", "run-b"},
                "the shared path retains only one of the two run-specific outputs",
            )

            output_a.unlink()
            output_b.unlink()
            first = subprocess.Popen(
                [str(LAUNCHER), "validate-run", str(output_a), "run-a"], env=env
            )
            second = subprocess.Popen(
                [str(LAUNCHER), "validate-run", str(output_b), "run-b"], env=env
            )
            self.assertEqual(first.wait(timeout=3), 0)
            self.assertEqual(second.wait(timeout=3), 0)

            cache_a = Path(output_a.read_text().strip())
            cache_b = Path(output_b.read_text().strip())
            self.assertNotEqual(cache_a, cache_b)
            self.assertFalse(cache_a.exists(), "first private cache must be cleaned")
            self.assertFalse(cache_b.exists(), "second private cache must be cleaned")

    def test_non_validate_commands_keep_the_existing_rust_script_cache(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = fake_bin / "rust-script"
            capture = root / "capture"
            fake.write_text(
                "#!/bin/bash\n"
                "printf '%s\\n' \"${XDG_CACHE_HOME-unset}\" > \"$4\"\n"
            )
            fake.chmod(0o755)
            env = {
                **os.environ,
                "PATH": f"{fake_bin}:{os.environ.get('PATH', '')}",
            }
            env.pop("XDG_CACHE_HOME", None)
            result = subprocess.run(
                [str(LAUNCHER), "health", str(capture)], env=env, check=False
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(capture.read_text().strip(), "unset")


if __name__ == "__main__":
    unittest.main()
