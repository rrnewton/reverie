#!/usr/bin/env python3
"""On-demand CI status report for the Hermit repos (the `runner-health` command).

Non-mutating. Reports, per repo: self-hosted runner health, per-workflow QUEUE
DEPTH, WAIT TIMES (time-in-queue kept strictly separate from run duration),
TIME SINCE LAST GREEN (elapsed + how many runs back), and an eligibility-honest
capacity signal that does not infer a workflow constraint from runner labels.

The analysis lives in the importable `queue_health` module so the same code
backs both this human report and the ops-tick gate
(`ci-hub/health/operational_health.py queue-health`). This file remains the
stable entrypoint the `ci-hub runner-health` front door and the README refer to.

Usage:
    ./ci-status.py                 # default: rrnewton/hermit
    ./ci-status.py --all           # all three Hermit repos
    ./ci-status.py --repo owner/name [--limit 100] [--sample 15]

gh is invoked through `$GH` (default "with-proxy gh") so it works on the
development host behind the proxy without changing the machine-global gh account.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import queue_health  # noqa: E402  (path set above)


if __name__ == "__main__":
    raise SystemExit(queue_health.main())
