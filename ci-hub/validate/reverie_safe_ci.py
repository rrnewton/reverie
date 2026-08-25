#!/usr/bin/env python3
"""Run one full Reverie validation through the fail-closed DAG runner."""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--checkout", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--pr", type=int)
    args = parser.parse_args(argv)
    root = args.root.resolve()
    checkout = args.checkout.resolve()
    node = [
        sys.executable,
        str(root / "ci-hub/validate/reverie_node.py"),
        "--root",
        str(root),
        "--checkout",
        str(checkout),
        "--target",
        args.target,
        "--log",
        str(args.log.resolve()),
    ]
    if args.pr is not None:
        node.extend(["--pr", str(args.pr)])
    dag = {
        "default_step_timeout": 7200,
        "steps": [
            {
                "group": "validate",
                "job": "reverie-full",
                "desc": "Reverie full local validation",
                "cmd": shlex.join(node),
                "timeout": 7200,
                "cpu_timeout": 14400,
                "jobs_flag": "",
                "hint": {
                    "classification": "cpu-bound",
                    "preferred_inner_jobs": 8,
                    "rss_baseline_bytes": 17179869184,
                    "hard_mem_max_bytes": 34359738368,
                },
            }
        ],
    }
    dag_dir = root / "ignored/validate"
    dag_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", prefix="reverie-dag-", dir=dag_dir, delete=False
    ) as handle:
        json.dump(dag, handle, sort_keys=True)
        handle.write("\n")
        dag_path = Path(handle.name)
    try:
        command = [
            str(root / "ci-hub/bin/agent-tool"),
            "dagrun",
            "run",
            "--dag",
            str(dag_path),
            "--max-steps",
            "1",
            "--max-cpus",
            "8",
            "--run-timeout",
            "7500",
            "--perf-dir",
            str(root / "ignored/validate/reverie-safe-ci-profiles"),
            "--no-profile-feedback",
            "-v",
        ]
        # Deliberately no --allow-cgroup-failure and no --unsafe-no-cgroups.
        return subprocess.run(command, cwd=checkout, check=False).returncode
    finally:
        dag_path.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())
