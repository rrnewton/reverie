#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.

"""`scripts/orphaned-task-detector.sh` must refuse an unreadable task graph.

The regression these lock: run bare, the detector used to read `tg`'s implicit
empty `tasks` default and print "scanned 0 ... ORPHANED: 0" at exit 0 — the same
verdict word and the same exit code as a full census that found nothing. A
detector that reports health because it could not see anything is worse than one
that does not run.

The database guard deliberately runs BEFORE the live-fleet read, so these cases
are exercisable without an orc socket: the refusal is decided before anything
report-shaped is printed.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sqlite3
import tempfile
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DETECTOR = ROOT / "scripts" / "orphaned-task-detector.sh"

UNAVAILABLE_RC = 2
#: The detector's OTHER fail-closed exit: a tool it needs is not installed, so it
#: aborts and flags nothing. Distinct from 2 on purpose — 2 says "the task graph
#: was unreadable", 3 says "I could not even start".
TOOL_MISSING_RC = 3

#: Tools the detector requires before it will look at anything (detector lines
#: 93-95). Their presence is a PRECONDITION of the database guard below, not part
#: of it: the guard is reached only after all three resolve.
REQUIRED_TOOLS = ("tg", "orc", "tmux")


def _stub_tools(directory: Path) -> str:
    """A PATH prefix supplying any of REQUIRED_TOOLS this host lacks.

    WHY THIS EXISTS, because a stub in a test is exactly the thing that deserves
    justification. These four cases test the DATABASE guard. That guard sits
    behind a tool-presence guard for three tools this repository's agents always
    have and hosted CI never does — `tg` is a large fbsource binary absent from
    ubuntu-latest by construction. So on CI the detector exited 3 at line 93 and
    the cases never reached their own subject: they were red for a reason
    unrelated to the property they assert.

    The stub is inert and cannot fake a measurement. The database guard resolves
    through `ci-hub/lib/taskgraph_db.py` and returns before any of these three is
    ever INVOKED, so what the stub does is irrelevant — only that it exists.
    Nothing here weakens an assertion; it supplies a precondition so the
    assertion can run at all. The tool-presence guard keeps its own coverage in
    `test_a_missing_tool_is_could_not_measure_not_zero_orphans` below, which
    runs with these stubs deliberately withheld.
    """
    directory.mkdir(parents=True, exist_ok=True)
    for tool in REQUIRED_TOOLS:
        if shutil.which(tool):
            continue
        stub = directory / tool
        stub.write_text("#!/bin/sh\nexit 0\n")
        stub.chmod(0o755)
    return str(directory)


def run(
    env_overrides: dict[str, str | None], *, supply_tools: bool = True
) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    if supply_tools:
        stubs = _stub_tools(Path(tempfile.mkdtemp(prefix="orphan-detector-tools-")))
        env["PATH"] = stubs + os.pathsep + env.get("PATH", "")
    for key, value in env_overrides.items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    return subprocess.run(
        [str(DETECTOR)], cwd=ROOT, env=env, text=True, capture_output=True
    )


class OrphanDetectorFailsClosed(unittest.TestCase):
    def assert_refused(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, UNAVAILABLE_RC, result.stderr)
        self.assertIn("COULD NOT MEASURE", result.stderr)
        # The load-bearing half: it must not emit a clean-looking census.
        self.assertNotIn("ORPHANED", result.stdout)
        self.assertNotIn("scanned", result.stdout)

    def test_unbound_database_is_could_not_measure_not_zero_orphans(self) -> None:
        self.assert_refused(run({"TG_DB_PATH": None}))

    def test_a_missing_tool_is_could_not_measure_not_zero_orphans(self) -> None:
        """The tool-presence guard must fail closed too, and be SAID to.

        Until now this was only ever exercised by accident: hosted CI happens to
        lack all three tools, so it took exit 3 there and exit 2 here, and the
        difference read as a flaky suite rather than as two guards. Asserting it
        turns an environment coincidence into a property. Runs with the stubs
        deliberately withheld, and with a database that WOULD resolve, so the
        only reason to refuse is the missing tool.
        """
        # Drop only the DIRECTORIES that hold the tools, keeping the rest of PATH
        # so the detector still has a shell and coreutils. Shadowing by name does
        # not work: a non-executable entry earlier in PATH is skipped, and the
        # real tool further along still resolves.
        # Every directory holding one, not just the first hit -- `tg` lives in
        # more than one PATH entry on an agent box, and dropping only the first
        # leaves the second resolvable.
        kept = []
        for directory in os.environ.get("PATH", "").split(os.pathsep):
            if not directory:
                continue
            if any(os.access(os.path.join(directory, t), os.X_OK)
                   for t in REQUIRED_TOOLS):
                continue
            kept.append(directory)
        path = os.pathsep.join(kept)
        still_there = [t for t in REQUIRED_TOOLS if shutil.which(t, path=path)]
        self.assertEqual(
            still_there, [],
            f"could not hide {still_there} from PATH, so this guard was not "
            f"exercised; the test must not silently pass in that case")
        result = run({"PATH": path}, supply_tools=False)
        self.assertEqual(result.returncode, TOOL_MISSING_RC, result.stderr)
        self.assertIn("not on PATH", result.stderr)
        # Same load-bearing half as assert_refused: no clean-looking census.
        self.assertNotIn("ORPHANED", result.stdout)
        self.assertNotIn("scanned", result.stdout)

    def test_blank_database_value_is_could_not_measure(self) -> None:
        self.assert_refused(run({"TG_DB_PATH": ""}))

    def test_missing_database_file_is_could_not_measure(self) -> None:
        missing = ROOT / "ignored" / "no-such-taskgraph-should-not-exist.db"
        self.assertFalse(missing.exists())
        self.assert_refused(run({"TG_DB_PATH": str(missing)}))

    def test_non_sqlite_bytes_are_could_not_measure(self) -> None:
        import tempfile

        with tempfile.NamedTemporaryFile(suffix=".db", delete=False) as handle:
            handle.write(b"not a database")
            junk = handle.name
        try:
            self.assert_refused(run({"TG_DB_PATH": junk}))
        finally:
            os.unlink(junk)

    def test_readable_but_ownerless_graph_stays_a_measured_zero(self) -> None:
        """Empty must NOT be swept into unverifiable — the whole point of the
        resolver validating SHAPE rather than contents. Without this the fix
        would over-correct and a genuinely quiet fleet would read as broken."""
        import tempfile

        source = os.environ.get("TG_DB_PATH")
        if not source or not Path(source).exists():
            self.skipTest("no bound TaskGraph to derive an ownerless copy from")
        with tempfile.NamedTemporaryFile(suffix=".db", delete=False) as handle:
            empty = handle.name
        try:
            shutil.copyfile(source, empty)
            with sqlite3.connect(empty) as conn:
                conn.execute("UPDATE tasks SET owner=NULL")
            result = run({"TG_DB_PATH": empty})
            if result.returncode == 3:
                self.skipTest("live fleet unreadable here; guard order untested")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("scanned 0 owned non-terminal task(s)", result.stdout)
            self.assertIn("ORPHANED (owner not in live fleet): 0", result.stdout)
        finally:
            os.unlink(empty)

    def test_resolver_stderr_cannot_become_part_of_the_database_path(self) -> None:
        """A runtime diagnostic on stderr must not corrupt a valid stdout path."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            socket = root / "tmux"
            socket.mkdir()
            database = root / "taskgraph.db"
            database.touch()

            scripts = {
                "python3": """#!/bin/sh
printf 'runtime diagnostic\\n' >&2
printf '%s\\n' "$FAKE_TG_DB"
""",
                "orc": """#!/bin/sh
printf 'orc socket (TMUX_TMPDIR): %s\\n' "$FAKE_TMUX_SOCKET"
""",
                "tmux": """#!/bin/sh
case "$1" in
  list-sessions) printf 'orc-session\\n' ;;
  list-windows) printf 'worker\\n' ;;
  *) exit 64 ;;
esac
""",
                "tg": """#!/bin/sh
[ "$TG_DB_PATH" = "$FAKE_TG_DB" ] || exit 17
printf 'result\\n------\\n\\n(0 rows)\\n'
""",
            }
            for name, body in scripts.items():
                path = fake_bin / name
                path.write_text(body)
                path.chmod(0o755)

            env = dict(os.environ)
            env.update(
                {
                    "PATH": str(fake_bin) + os.pathsep + env.get("PATH", ""),
                    "FAKE_TG_DB": str(database),
                    "FAKE_TMUX_SOCKET": str(socket),
                }
            )
            result = subprocess.run(
                [str(DETECTOR)],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("runtime diagnostic", result.stderr)
        self.assertIn(f"task graph: {database}", result.stdout)
        self.assertNotIn("runtime diagnostic", result.stdout)


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
