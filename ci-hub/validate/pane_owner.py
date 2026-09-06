#!/usr/bin/env python3
"""Create the Herdr observer for a boxed validation service.

Herdr control calls themselves cross the agent jail through short-lived user
services.  The pane only observes the durable log and systemd unit; it never
runs the validation driver. The actual compute remains in the separately admitted
validate-*.service and then safe-ci-dag-runner's delegated scope.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


Runner = Callable[..., subprocess.CompletedProcess[str]]
WORKSPACE_LABEL = "validate-hermit"


@dataclass(frozen=True)
class PaneHandle:
    workspace_id: str
    tab_id: str
    pane_id: str
    title: str


def run_command(command: Sequence[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return bounded_capture.capture(command, **kwargs)


def host_command(command: Sequence[str], environment: Mapping[str, str]) -> list[str]:
    home = environment.get("HOME", "")
    path = environment.get("PATH", "")
    if not home or not path:
        raise RuntimeError("HOME and PATH are required for the outside-jail Herdr broker")
    return [
        "systemd-run",
        "--user",
        "--wait",
        "--pipe",
        "--collect",
        "--quiet",
        "--setenv",
        f"HOME={home}",
        "--setenv",
        f"PATH={path}",
        *command,
    ]


def run_host(
    command: Sequence[str], *, run: Runner, environment: Mapping[str, str]
) -> subprocess.CompletedProcess[str]:
    return run(host_command(command, environment), check=False)


def _json_result(result: subprocess.CompletedProcess[str], purpose: str) -> dict[str, Any]:
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"{purpose}: {detail}")
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"{purpose}: Herdr returned non-JSON output") from exc
    payload = value.get("result") if isinstance(value, dict) else None
    if not isinstance(payload, dict):
        raise RuntimeError(f"{purpose}: Herdr response has no result object")
    return payload


def ensure_server(
    *, run: Runner, environment: Mapping[str, str], sleep: Callable[[float], None] = time.sleep
) -> None:
    status = run_host(["herdr", "status", "--json"], run=run, environment=environment)
    if status.returncode == 0:
        try:
            value = json.loads(status.stdout)
            if value.get("server", {}).get("running") is True:
                return
        except (AttributeError, json.JSONDecodeError):
            pass

    launch = run(
        [
            "systemd-run",
            "--user",
            "--collect",
            "--unit",
            "ci-hub-herdr",
            "--description",
            "ci-hub Herdr visibility server",
            # Herdr is a static musl binary and musl's mallocng has one global allocator lock.
            # Tokio's default of one worker per core turns that lock into a convoy on a many-core
            # host: measured on devbig014 (316 cores) the server pinned >1000% CPU with ~98% of
            # cycles in the kernel futex spinlock and every control call timed out. Capping the
            # runtime workers fixes it; the per-pane blocking threads are separate and unaffected.
            "--setenv",
            "TOKIO_WORKER_THREADS=16",
            "herdr",
            "server",
        ],
        check=False,
    )
    if launch.returncode != 0 and "already exists" not in (launch.stderr + launch.stdout):
        detail = launch.stderr.strip() or launch.stdout.strip() or f"exit {launch.returncode}"
        raise RuntimeError(f"cannot start ci-hub Herdr server: {detail}")

    for _attempt in range(20):
        sleep(0.1)
        status = run_host(["herdr", "status", "--json"], run=run, environment=environment)
        if status.returncode != 0:
            continue
        try:
            value = json.loads(status.stdout)
            if value.get("server", {}).get("running") is True:
                return
        except (AttributeError, json.JSONDecodeError):
            continue
    raise RuntimeError("ci-hub Herdr server did not become ready")


def ensure_workspace(
    root: Path, *, run: Runner, environment: Mapping[str, str]
) -> str:
    payload = _json_result(
        run_host(["herdr", "workspace", "list"], run=run, environment=environment),
        "cannot list Herdr workspaces",
    )
    matches = [
        item
        for item in payload.get("workspaces", [])
        if isinstance(item, dict) and item.get("label") == WORKSPACE_LABEL
    ]
    if len(matches) > 1:
        raise RuntimeError(f"multiple Herdr workspaces are named {WORKSPACE_LABEL!r}")
    if matches:
        workspace = matches[0].get("workspace_id")
        if isinstance(workspace, str) and workspace:
            return workspace
        raise RuntimeError(f"Herdr workspace {WORKSPACE_LABEL!r} has no id")

    payload = _json_result(
        run_host(
            [
                "herdr",
                "workspace",
                "create",
                "--label",
                WORKSPACE_LABEL,
                "--cwd",
                str(root),
                "--no-focus",
            ],
            run=run,
            environment=environment,
        ),
        "cannot create validate-hermit workspace",
    )
    workspace = payload.get("workspace", {}).get("workspace_id")
    if not isinstance(workspace, str) or not workspace:
        raise RuntimeError("created Herdr workspace has no id")
    return workspace


def create_pane(
    *,
    root: Path,
    checkout: Path,
    unit: str,
    target: str,
    log: Path,
    record: Path,
    pr: int | None,
    started_at: str,
    run: Runner = run_command,
    environment: Mapping[str, str] | None = None,
    sleep: Callable[[float], None] = time.sleep,
) -> PaneHandle:
    env = environment or os.environ
    ensure_server(run=run, environment=env, sleep=sleep)
    workspace = ensure_workspace(root, run=run, environment=env)
    # BOUND THE WORKSPACE BEFORE ADDING TO IT. Every create used to be
    # unconditional and nothing ever closed a pane, so this workspace grew
    # without limit (observed at 46 tabs). ensure_capacity reaps finished and
    # orphaned panes, evicts the oldest non-running pane if still full, and
    # RAISES rather than creating pane cap+1. Imported lazily: pane_lifecycle
    # imports this module.
    import pane_lifecycle  # noqa: PLC0415  (lazy, breaks the import cycle)

    pane_lifecycle.ensure_capacity(
        root,
        workspace_label=WORKSPACE_LABEL,
        cap=pane_lifecycle.cap_from_env(),
        run=run,
        environment=env,
        linger_seconds=pane_lifecycle.linger_from_env(),
        ttl_seconds=pane_lifecycle.ttl_from_env(),
        report=lambda message: print(message, file=sys.stderr),
    )
    identity = f"PR #{pr}" if pr is not None else unit
    title = f"{identity} | {target[:12]} | since {started_at}"
    payload = _json_result(
        run_host(
            [
                "herdr",
                "tab",
                "create",
                "--workspace",
                workspace,
                # The observer receives the disposable checkout explicitly
                # below. Keep the interactive shell rooted in the durable tool
                # checkout so its normal post-command shell cannot keep a
                # completed validation checkout live during the linger window.
                "--cwd",
                str(root),
                "--label",
                title,
                "--no-focus",
            ],
            run=run,
            environment=env,
        ),
        "cannot create validate Herdr tab",
    )
    pane = payload.get("root_pane", {}).get("pane_id")
    tab = payload.get("tab", {}).get("tab_id")
    if not isinstance(pane, str) or not pane or not isinstance(tab, str) or not tab:
        raise RuntimeError("created Herdr tab has no pane/tab id")

    for command, purpose in (
        (["herdr", "pane", "rename", pane, title], "cannot title validate pane"),
        (
            [
                "herdr",
                "pane",
                "run",
                pane,
                "/usr/bin/python3",
                str(root / "ci-hub/validate/pane_watch.py"),
                "--unit",
                f"{unit}.service",
                "--target",
                target,
                "--checkout",
                str(checkout),
                "--log",
                str(log),
                "--record",
                str(record),
                *(["--pr", str(pr)] if pr is not None else []),
            ],
            "cannot start validate pane observer",
        ),
    ):
        result = run_host(command, run=run, environment=env)
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
            raise RuntimeError(f"{purpose}: {detail}")
    return PaneHandle(workspace, tab, pane, title)
