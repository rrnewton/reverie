#!/usr/bin/env python3
"""How both consumers locate the orc executable.

THE DEFECT THIS GUARDS. Measured 2026-09-05 on devbig014: two scripts hardcoded
``$HOME/orc-bin/orc`` as the orc executable. That directory exists and holds only
``tg`` -- there is no orc in it, and the running coordinator executes
``/usr/local/bin/orc`` (confirmed through ``/proc/<pid>/exe``). So
``scripts/orc-hermit-msg.py`` printed "orc executable does not exist ...; falling
back to direct tmux scan" on every invocation and ran degraded, and nothing
reported it because a warning is not a failure.

Same shape as the GitHub credential helper: one hardcoded path under ``$HOME``
that stopped existing. The remedy is the same -- prefer the local pin when it is
really there, otherwise take the tool from PATH, and keep an explicit override
that is honoured verbatim so a wrong explicit choice fails where it is named.

WHY THIS FILE SITS IN ``scripts/`` AND NOT ``scripts/tests/``. The Makefile's
``py-unittest`` line runs ``python3 -m unittest discover -s scripts -p
"test_*.py"``, and ``scripts/tests/`` has no ``__init__.py``, so discovery
reports "Start directory is not importable" and never descends into it. The
existing orc-hermit-msg tests live there and are therefore unscheduled. That gap
is filed separately; this file is placed where the counter already looks.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
NOTIFIER = ROOT / "scripts/orc-hermit-msg.py"
RELOADER = ROOT / "scripts/orc-workflow-reload.sh"

#: The shell resolution runs from this comment to the first line that is exactly
#: ``fi``. Extracting and executing it is a behavioural test of that exact code,
#: rather than a string match that keeps passing after the code changes shape.
SHELL_BLOCK_START = "# ⚠️ SAME DEFECT AS scripts/orc-hermit-msg.py CARRIED HERE."


def shell_resolution_block() -> str:
    lines = RELOADER.read_text().splitlines(keepends=True)
    for index, line in enumerate(lines):
        if line.startswith(SHELL_BLOCK_START):
            break
    else:
        raise AssertionError(f"{RELOADER}: no orc resolution block found")
    for end in range(index, len(lines)):
        if lines[end] == "fi\n":
            return "".join(lines[index : end + 1])
    raise AssertionError(f"{RELOADER}: orc resolution block is unterminated")


class OrcCommandResolution(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        base = Path(self.temporary.name)
        self.home = base / "home"
        self.path_bin = base / "path-bin"
        self.home.mkdir()
        self.path_bin.mkdir()

        self.path_orc = self.path_bin / "orc"
        self.path_orc.write_text("#!/bin/sh\nexit 0\n")
        self.path_orc.chmod(0o755)

        self.pinned = self.home / "orc-bin" / "orc"
        self.pinned.parent.mkdir(parents=True)

        for smuggled in ("/usr/bin/orc", "/bin/orc"):
            if os.path.exists(smuggled):
                self.skipTest(f"{smuggled} exists, so the no-orc case cannot be isolated")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def make_pinned_real(self) -> None:
        self.pinned.write_text("#!/bin/sh\nexit 0\n")
        self.pinned.chmod(0o755)

    def env(self, *, with_path_orc: bool = True, **overrides: str) -> dict[str, str]:
        # /usr/bin and /bin carry no orc on a supported host (checked: neither
        # /usr/bin/orc nor /bin/orc exists; the real one is /usr/local/bin/orc),
        # so including them keeps bash reachable without smuggling in a
        # resolvable orc and making the no-orc case pass for the wrong reason.
        base = str(self.path_bin) if with_path_orc else str(self.temporary.name)
        env = dict(os.environ, HOME=str(self.home), PATH=f"{base}:/usr/bin:/bin")
        env.pop("DEV_HERMIT_ORC_COMMAND", None)
        env.pop("ORC_BIN", None)
        env.update(overrides)
        return env

    def python_default(self, env: dict[str, str]) -> str:
        """The notifier prints its resolved default in --help, so read that."""
        result = subprocess.run(
            [sys.executable, str(NOTIFIER), "--help"],
            env=env, text=True, capture_output=True, check=False,
        )
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        match = re.search(r"session discovery \(default:\s*(\S+?)\)", result.stdout, re.S)
        self.assertIsNotNone(match, result.stdout)
        return match.group(1)

    def shell_resolved(self, env: dict[str, str]) -> str:
        script = "set -u\n" + shell_resolution_block() + '\nprintf "%s" "$ORC"\n'
        result = subprocess.run(
            ["/usr/bin/bash", "-c", script], env=env, text=True, capture_output=True, check=False
        )
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        return result.stdout.strip()

    # ---- the regression itself -------------------------------------------

    def test_an_absent_pin_falls_through_to_path_rather_than_degrading(self) -> None:
        """THE REGRESSION. $HOME/orc-bin/orc does not exist; PATH has orc."""
        self.assertFalse(self.pinned.exists())
        env = self.env()
        self.assertEqual(str(self.path_orc), self.python_default(env))
        self.assertEqual(str(self.path_orc), self.shell_resolved(env))

    def test_a_pin_that_exists_still_wins_over_path(self) -> None:
        """This fleet pins an orc version there; that must keep working."""
        self.make_pinned_real()
        env = self.env()
        self.assertEqual(str(self.pinned), self.python_default(env))
        self.assertEqual(str(self.pinned), self.shell_resolved(env))

    def test_a_pin_that_is_not_executable_is_not_selected(self) -> None:
        self.pinned.write_text("#!/bin/sh\nexit 0\n")
        self.pinned.chmod(0o644)
        env = self.env()
        self.assertEqual(str(self.path_orc), self.python_default(env))
        self.assertEqual(str(self.path_orc), self.shell_resolved(env))

    def test_an_explicit_override_is_honoured_verbatim(self) -> None:
        """A wrong explicit choice must fail where it is named, not resolve elsewhere."""
        self.make_pinned_real()
        chosen = str(Path(self.temporary.name) / "explicitly-chosen-orc")
        self.assertEqual(
            chosen, self.python_default(self.env(DEV_HERMIT_ORC_COMMAND=chosen))
        )
        self.assertEqual(chosen, self.shell_resolved(self.env(ORC_BIN=chosen)))

    def test_with_nothing_resolvable_the_pin_is_named_so_the_error_is_actionable(self) -> None:
        env = self.env(with_path_orc=False)
        self.assertEqual(str(self.pinned), self.python_default(env))
        self.assertEqual(str(self.pinned), self.shell_resolved(env))


if __name__ == "__main__":
    unittest.main()
