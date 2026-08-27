#!/usr/bin/env python3
"""Anchor Hermit's run timeout when validate-lock starts its child."""

from __future__ import annotations

import os
import sys
import time
from collections.abc import Sequence


STEP_STARTED_MONOTONIC_NS_ENV = "DAGRUN_STEP_STARTED_MONOTONIC_NS"


def main(argv: Sequence[str] | None = None) -> int:
    command = list(sys.argv[1:] if argv is None else argv)
    if not command:
        print("run-with-validate-deadline: no child command", file=sys.stderr)
        return 2
    environment = dict(os.environ)
    environment[STEP_STARTED_MONOTONIC_NS_ENV] = str(time.monotonic_ns())
    os.execvpe(command[0], command, environment)
    raise AssertionError("os.execvpe returned")


if __name__ == "__main__":
    raise SystemExit(main())
