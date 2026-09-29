#!/usr/bin/python3 -I

from __future__ import annotations

import hashlib
import os
import stat
import sys
from pathlib import Path


CANDIDATE = Path("/tmp/tfa-phase2/codex-target-a708fc68/release/codex")
EXPECTED_SHA256 = "4378ab0cdb684cd877ce62a279ea29af98d36c249ad3b70a6e5b7af85bac9d94"
EXPECTED_SIZE = 1_388_938_544


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit("diagnostic wrapper received no packaged payload")
    descriptor = os.open(
        CANDIDATE,
        os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
    )
    metadata = os.fstat(descriptor)
    digest = hashlib.sha256()
    offset = 0
    while chunk := os.pread(descriptor, 1 << 20, offset):
        digest.update(chunk)
        offset += len(chunk)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_size != EXPECTED_SIZE
        or digest.hexdigest() != EXPECTED_SHA256
    ):
        raise SystemExit("diagnostic Codex candidate identity differs")
    os.set_inheritable(descriptor, True)
    executable = f"/proc/self/fd/{descriptor}"
    os.execve(executable, [str(CANDIDATE), *sys.argv[2:]], dict(os.environ))


if __name__ == "__main__":
    main()
