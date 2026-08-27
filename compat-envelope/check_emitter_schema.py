#!/usr/bin/env python3
"""Compatibility entry point for the shared scorecard-row schema check."""

from __future__ import annotations

import os
import sys
from pathlib import Path


def main() -> None:
    checker = Path(__file__).with_name("check-scorecard-schema.rs")
    arguments = list(sys.argv[1:])
    if "--root" in arguments:
        index = arguments.index("--root") + 1
        if index >= len(arguments):
            raise SystemExit("--root needs a path")
        root = Path(arguments[index]).resolve()
        if (root / "scorecard-schema.json").is_file():
            arguments[index] = str(root.parent)
    else:
        arguments.extend(["--root", str(checker.parent.parent)])
    os.execv(checker, [str(checker), *arguments])


if __name__ == "__main__":
    main()
