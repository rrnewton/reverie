#!/usr/bin/env python3
"""Run one full Reverie validation through the fail-closed DAG runner."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

from immutable_tool_authority import read_executing_tool_authority, resolve_authority_path

from final_validate_status import (
    FINAL_VALIDATE_STATUS_EXIT_CODES,
    FINAL_VALIDATE_STATUS_PREFIX,
    FinalValidateStatusError,
    read_final_validate_status,
)


ROOT = Path(__file__).resolve().parents[2]


def read_final_status(path: Path, dag_exit: int) -> tuple[str, int]:
    try:
        output = path.read_text()
    except OSError as error:
        print(f"reverie-safe-ci: final validate status is absent: {error}", file=sys.stderr)
        return "COULD_NOT_RUN", 75
    markers = [
        line for line in output.splitlines() if line.startswith(FINAL_VALIDATE_STATUS_PREFIX)
    ]
    if len(markers) != 1:
        print(
            f"reverie-safe-ci: expected one final validate status, found {len(markers)}",
            file=sys.stderr,
        )
        return "COULD_NOT_RUN", 75
    try:
        status = read_final_validate_status(output)
    except FinalValidateStatusError as error:
        print(
            f"reverie-safe-ci: final validate status is malformed: {error}",
            file=sys.stderr,
        )
        return "COULD_NOT_RUN", 75
    if status is None:
        print("reverie-safe-ci: final validate status is absent", file=sys.stderr)
        return "COULD_NOT_RUN", 75
    expected_dag_exit = 0 if status == "PASSED" else 1
    if dag_exit != expected_dag_exit:
        print(
            "reverie-safe-ci: final validate status disagrees with dagrun exit: "
            f"status={status} dagrun_exit={dag_exit}",
            file=sys.stderr,
        )
        return "COULD_NOT_RUN", 75
    return status, FINAL_VALIDATE_STATUS_EXIT_CODES[status]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--state-root", type=Path)
    parser.add_argument("--checkout", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--pr", type=int)
    args = parser.parse_args(argv)
    status = "COULD_NOT_RUN"
    exit_code = FINAL_VALIDATE_STATUS_EXIT_CODES[status]
    dag_path: Path | None = None
    status_path: Path | None = None
    try:
        state_root = resolve_authority_path(args.state_root or args.root)
        authority = read_executing_tool_authority(
            ROOT, state_root, os.environ, tool_root=args.root
        )
        if authority is not None and args.state_root is None:
            raise ValueError("immutable tool context requires explicit --state-root")
        child_environment = dict(os.environ, DEV_HERMIT_PARENT=str(state_root))
        # Preserve the retained capability for code; only mutable artifacts use
        # canonical state. Authentication above must precede any artifact writes.
        root = args.root if authority is not None else args.root.resolve()
        checkout = args.checkout.resolve()
        dag_dir = state_root / "ignored/validate"
        dag_dir.mkdir(parents=True, exist_ok=True)
        status_fd, status_name = tempfile.mkstemp(
            prefix="reverie-final-status-", dir=dag_dir
        )
        os.close(status_fd)
        status_path = Path(status_name)
        status_path.unlink()
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
            "--status-file",
            str(status_path),
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
        with tempfile.NamedTemporaryFile(
            mode="w",
            suffix=".json",
            prefix="reverie-dag-",
            dir=dag_dir,
            delete=False,
        ) as handle:
            json.dump(dag, handle, sort_keys=True)
            handle.write("\n")
            dag_path = Path(handle.name)
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
            str(state_root / "ignored/validate/reverie-safe-ci-profiles"),
            "--no-profile-feedback",
            "-v",
        ]
        # Deliberately no --allow-cgroup-failure and no --unsafe-no-cgroups.
        try:
            dag_exit = subprocess.run(
                command, cwd=checkout, check=False, env=child_environment
            ).returncode
        except OSError as error:
            print(f"reverie-safe-ci: could not run dagrun: {error}", file=sys.stderr)
        else:
            status, exit_code = read_final_status(status_path, dag_exit)
    except (OSError, ValueError) as error:
        print(f"reverie-safe-ci: could not prepare validation: {error}", file=sys.stderr)
    finally:
        if dag_path is not None:
            dag_path.unlink(missing_ok=True)
        if status_path is not None:
            status_path.unlink(missing_ok=True)
    print(f"{FINAL_VALIDATE_STATUS_PREFIX}{status}")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
