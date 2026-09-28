#!/usr/bin/env python3
"""Hold the write end of the kernel-bound wrkslots removal proof pipe."""

from __future__ import annotations

import os
import time


def main() -> int:
    while True:
        try:
            os.write(1, b".")
        except BrokenPipeError:
            return 0
        time.sleep(0.2)


if __name__ == "__main__":
    raise SystemExit(main())
