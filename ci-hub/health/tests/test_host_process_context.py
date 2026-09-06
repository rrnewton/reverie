#!/usr/bin/env python3
"""Brackets for the host process context check."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


MODULE_PATH = Path(__file__).resolve().parents[1] / "host_process_context.py"
SPEC = importlib.util.spec_from_file_location("host_process_context", MODULE_PATH)
assert SPEC and SPEC.loader
check = importlib.util.module_from_spec(SPEC)
sys.path.insert(0, str(MODULE_PATH.parent))
SPEC.loader.exec_module(check)


def plant_process_view(root: Path, *, full: bool) -> None:
    namespace = root / "self" / "ns"
    namespace.mkdir(parents=True)
    target = (
        check.INITIAL_PID_NAMESPACE_LINK if full else "pid:[4026539999]"
    )
    (namespace / "pid").symlink_to(target)
    init = root / "1"
    init.mkdir()
    (init / "comm").write_text(
        check.HOST_INIT_COMM + "\n" if full else "sandbox-init\n",
        encoding="utf-8",
    )


class HostProcessContextTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_restricted_process_view_refuses(self) -> None:
        plant_process_view(self.root, full=False)
        self.assertEqual(
            check.UNVERIFIABLE,
            check.main(["--proc-root", str(self.root)]),
        )

    def test_full_process_view_succeeds(self) -> None:
        plant_process_view(self.root, full=True)
        self.assertEqual(
            check.VERIFIED_DEAD,
            check.main(["--proc-root", str(self.root)]),
        )

    def test_unreadable_process_view_refuses(self) -> None:
        self.assertEqual(
            check.UNVERIFIABLE,
            check.main(["--proc-root", str(self.root)]),
        )


if __name__ == "__main__":
    unittest.main()
