#!/usr/bin/env python3
"""Keep the landing gate's two-sided contract inside the counted ci-hub suite."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BRACKET = ROOT / "ci-hub/landing/test_cherry_pick_context.py"


def test_cherry_pick_context_bracket_is_counted() -> None:
    result = subprocess.run(
        [sys.executable, str(BRACKET)],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Ran 14 tests" in output
    assert "OK" in output
