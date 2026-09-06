#!/usr/bin/env python3
"""Refuse unless this process can observe the host PID namespace."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Sequence

from agent_liveness_probe import (
    HOST_INIT_COMM,
    INITIAL_PID_NAMESPACE_LINK,
    UNVERIFIABLE,
    VERIFIED_DEAD,
    _process_view,
)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Verify the host process context required before process-based checks "
            "and a destructive mutation. Never changes any process or file."
        )
    )
    parser.add_argument("--json", action="store_true", dest="as_json")
    parser.add_argument(
        "--proc-root",
        type=Path,
        default=Path("/proc"),
        help="override the /proc root (tests only)",
    )
    args = parser.parse_args(argv)

    state, reason = _process_view(args.proc_root)
    rc = VERIFIED_DEAD if state == "full" else UNVERIFIABLE
    if args.as_json:
        rendered = json.dumps(
            {"process_context": state, "reason": reason, "rc": rc},
            sort_keys=True,
        )
    else:
        banner = "FULL PROCESS VIEW" if state == "full" else "UNVERIFIABLE"
        rendered = f"{banner} process_context={state} reason={reason} rc={rc}"
    print(rendered, file=sys.stdout if rc == VERIFIED_DEAD else sys.stderr)
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
