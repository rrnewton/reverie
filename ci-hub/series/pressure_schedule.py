#!/usr/bin/env python3
"""Launch one bounded recurring pressure-test sample without overlapping validate."""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import sys
from pathlib import Path
from typing import Callable, Mapping, Sequence


SOURCE_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SOURCE_ROOT / "ci-hub" / "series"))
sys.path.insert(0, str(SOURCE_ROOT / "ci-hub" / "validate"))
import series  # noqa: E402
import run_registry  # noqa: E402
import cpu_placement  # noqa: E402
import unwrapped_git  # noqa: E402


STATE_ROOT = Path(
    os.environ.get("DEV_HERMIT_PARENT", series.shared_parent_root(SOURCE_ROOT))
).resolve()
UNIT = "hermit-pressure-test"
SAMPLE_COUNT = 10
# No per-cell bound is passed: pressure-test.rs derives each cell's enclosing
# allowance from its manifest budget and refuses any smaller --cell-timeout.
# The former fixed 60 s was below the requirement of every verify, replay, and
# chaos cell in the current manifests (at least 278 s, for a 57 s manifest
# timeout), so every sampled run refused before launch
# (https://github.com/rrnewton/dev-hermit/issues/431).  The whole run stays
# bounded by RUN_TIMEOUT_SECONDS, which pressure-test.rs also checks against
# the selected cells' declared worst-case occupancy before it starts.
RUN_TIMEOUT_SECONDS = 1500
LOCK_WAIT_SECONDS = 3600
LOCK_CHILD_DEADLINE_SECONDS = 1800
# wrkslots fails immediately on a busy configuration or state lock unless given
# --wait-lock; other agents' create/remove calls hold those locks routinely.
SLOT_LOCK_WAIT_SECONDS = 900
UNIT_RUNTIME_SECONDS = (
    SLOT_LOCK_WAIT_SECONDS  # wrkslots create
    + LOCK_WAIT_SECONDS
    + LOCK_CHILD_DEADLINE_SECONDS
    + SLOT_LOCK_WAIT_SECONDS  # wrkslots remove
    + 300
)
# The systemd user manager's environment has no GIT_ALLOW_PROTOCOL, and the
# user's url.insteadOf rewrites submodule URLs to local paths, so the
# post-provision `git submodule update` fails with "transport 'file' not
# allowed".  Same remedy as the integration-tip launcher in
# https://github.com/rrnewton/dev-hermit/commit/84ff6369d7f806fdac0bf5d7962aa20f534bcae3.
UNIT_GIT_SETENV: Mapping[str, str] = {"GIT_ALLOW_PROTOCOL": "https:file:ssh"}
SHA_RE = re.compile(r"[0-9a-f]{40}")
# PATH's original git, handed to the unit only when launch() replaced PATH's
# git with the unwrapped binary. run_campaign uses it for its one network
# call, the submodule fetch, so that call keeps the telemetry wrapper and its
# retry of HTTP 429 responses, as it had before the substitution.
NETWORK_GIT_ENV = unwrapped_git.NETWORK_GIT_ENV


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def state_path(root: Path) -> Path:
    return root / "ignored" / "ci-hub" / "pressure-test-state.json"


def log_path(root: Path) -> Path:
    return root / "ignored" / "ci-hub" / "pressure-test.log"


def launch_lock_path(root: Path) -> Path:
    return root / "ignored" / "ci-hub" / "pressure-test-launch.lock"


def unwrapped_git_root(root: Path) -> Path:
    """Where every pressure test's private git directory lives, one per slot."""
    return root / "ignored" / "ci-hub" / "pressure-test-git"


def unwrapped_git_directory(root: Path, slot: str) -> Path:
    """This run's private PATH entry holding only a `git` symlink."""
    return unwrapped_git_root(root) / slot


def network_git(environment: Mapping[str, str] | None = None) -> str:
    """The git for the campaign's submodule fetch: see NETWORK_GIT_ENV."""
    environment = os.environ if environment is None else environment
    value = environment.get(NETWORK_GIT_ENV, "")
    return value if os.path.isabs(value) else "git"


def remove_unwrapped_git_directory(
    root: Path, slot: str, substitution: object
) -> str | None:
    """Remove this slot's git directory when launch() created it.

    `substitution` is the run record's `git_substitution`. Nothing is touched
    unless it records an applied substitution whose directory is exactly this
    slot's; unwrapped_git.remove then deletes only a directory whose only
    entry is a `git` symlink to the recorded binary.
    """
    if not isinstance(substitution, Mapping) or substitution.get("applied") is not True:
        return None
    directory = unwrapped_git_directory(root, slot)
    if substitution.get("directory") != str(directory):
        return (
            f"run record names git directory {substitution.get('directory')!r}, "
            f"not {directory}; left in place"
        )
    real_git = substitution.get("real_git")
    if not isinstance(real_git, str) or not real_git:
        return f"run record names no unwrapped git for {directory}; left in place"
    return unwrapped_git.remove(directory, real_git)


def remove_recorded_unwrapped_git_directory(
    root: Path, substitution: object
) -> str | None:
    """remove_unwrapped_git_directory for a run record that names no slot.

    The slot is taken from the recorded directory, which must be a direct
    child of unwrapped_git_root; anything else is left in place.
    """
    if not isinstance(substitution, Mapping) or substitution.get("applied") is not True:
        return None
    recorded = substitution.get("directory")
    if not isinstance(recorded, str) or Path(recorded).parent != unwrapped_git_root(root):
        return (
            f"run record names git directory {recorded!r}, which is not a slot "
            f"under {unwrapped_git_root(root)}; left in place"
        )
    return remove_unwrapped_git_directory(root, Path(recorded).name, substitution)


def sweep_unwrapped_git_directories(root: Path) -> tuple[list[str], list[str]]:
    """Remove every leftover slot directory; returns (removed, problems).

    Only launch() calls this, holding the launch lock after systemd reports
    the pressure unit inactive: the directories belong to that unit's
    campaign, so none is in use. It catches what run_campaign could not
    remove itself, for example after the unit was killed at RuntimeMaxSec.
    Each entry goes through unwrapped_git.remove with the host's unwrapped
    git, so only the exact shape launch() creates is deleted. Anything else
    (another link target, extra entries, a file) is left in place and named
    in `problems`.
    """
    parent = unwrapped_git_root(root)
    try:
        names = sorted(os.listdir(parent))
    except FileNotFoundError:
        return [], []
    except OSError as error:
        return [], [f"cannot list {parent}: {error}"]
    removed: list[str] = []
    problems: list[str] = []
    for name in names:
        problem = unwrapped_git.remove(parent / name, unwrapped_git.REAL_GIT)
        if problem is None:
            removed.append(name)
        else:
            problems.append(problem)
    return removed, problems


def emit(state: str, summary: str) -> None:
    print(f"state={state}")
    print(f"summary={summary}")


def write_state(root: Path, value: dict) -> None:
    path = state_path(root)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f"{path.name}.tmp-{os.getpid()}")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, path)


def read_state(root: Path) -> dict | None:
    try:
        value = json.loads(state_path(root).read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def launch_command(
    root: Path,
    systemd_run: str,
    with_proxy: str,
    target: str,
    record: Path,
    environment: Mapping[str, str] | None = None,
    placement: cpu_placement.CpuPlacement | None = None,
    path: str | None = None,
    network_git_path: str | None = None,
) -> list[str]:
    environment = os.environ if environment is None else environment
    if path is None:
        path = environment.get("PATH", "")
    # Same placement as validate-run (https://github.com/rrnewton/hermit/issues/3265).
    # A malformed DEV_HERMIT_EXCLUDE_CPUS raises CpuPlacementError, a ValueError,
    # which launch() reports as a refusal before any unit exists.
    if placement is None:
        placement = cpu_placement.derive(environ=environment)
    log = log_path(root)
    operational_tool = environment.get("DEV_HERMIT_OPERATIONAL_TOOL", "")
    parent_sha = environment.get("DEV_HERMIT_TOOL_PARENT_SHA", "")
    cache_root = environment.get("DEV_HERMIT_TOOL_CACHE", "")
    has_handoff = bool(operational_tool or parent_sha or cache_root)
    if has_handoff:
        if (
            not operational_tool
            or not os.path.isabs(operational_tool)
            or not os.path.isfile(operational_tool)
            or not os.access(operational_tool, os.X_OK)
            or SHA_RE.fullmatch(parent_sha) is None
            or not cache_root
            or not os.path.isabs(cache_root)
            or not os.path.isdir(cache_root)
        ):
            raise ValueError(
                "incomplete operational-tool handoff; recurring pressure test "
                "not launched"
            )
        working_directory = root
        worker = [
            operational_tool,
            "--run-current-tool",
            "--state-root",
            str(root),
            "--cache-root",
            cache_root,
            "--tool-parent-sha",
            parent_sha,
            "--tool-relative",
            "ci-hub/series/pressure_schedule.py",
            "--",
        ]
    else:
        working_directory = SOURCE_ROOT
        worker = [
            sys.executable,
            str(SOURCE_ROOT / "ci-hub" / "series" / "pressure_schedule.py"),
        ]

    return [
        systemd_run,
        "--user",
        "--quiet",
        "--collect",
        f"--unit={UNIT}",
        "--description=run one recurring Hermit pressure-test sample",
        f"--property=RuntimeMaxSec={UNIT_RUNTIME_SECONDS}",
        f"--property=StandardOutput=append:{log}",
        f"--property=StandardError=append:{log}",
        *(
            [f"--property={placement.systemd_property()}"]
            if placement.systemd_property()
            else []
        ),
        f"--working-directory={working_directory}",
        f"--setenv=PATH={path}",
        *(
            [f"--setenv={NETWORK_GIT_ENV}={network_git_path}"]
            if network_git_path
            else []
        ),
        "--setenv=PYTHONUNBUFFERED=1",
        f"--setenv=DEV_HERMIT_PARENT={root}",
        f"--setenv=DEV_HERMIT_TOOL_ROOT={SOURCE_ROOT}",
        *(
            f"--setenv={name}={value}"
            for name, value in sorted(UNIT_GIT_SETENV.items())
        ),
        # pressure-test.rs re-executes into a dagrun scope in another slice,
        # where AllowedCPUs above no longer applies; safe_ci_scope reads these
        # and applies the same list to that scope.
        *(
            f"--setenv={key}={value}"
            for key, value in placement.environment().items()
        ),
        "--",
        with_proxy,
        *worker,
        "_run",
        "--target",
        target,
        "--record",
        str(record),
        "--state-root",
        str(root),
    ]


def locked_command(
    root: Path,
    checkout: Path,
    target: str,
    results: Path,
    record: Path,
) -> list[str]:
    return [
        str(SOURCE_ROOT / "ci-hub" / "ci-hub"),
        "validate-lock",
        "run",
        "--agent",
        "tick-pressure-test",
        "--kind",
        "bench",
        "--target",
        target,
        "--run-record",
        str(record),
        "--wait",
        str(LOCK_WAIT_SECONDS),
        "--hold",
        "600",
        "--child-deadline",
        str(LOCK_CHILD_DEADLINE_SECONDS),
        "--",
        sys.executable,
        str(SOURCE_ROOT / "ci-hub" / "series" / "pressure_schedule.py"),
        "_locked",
        "--checkout",
        str(checkout),
        "--results",
        str(results),
        "--state-root",
        str(root),
    ]


def pressure_commands(checkout: Path, results: Path) -> list[list[str]]:
    pressure = str(checkout / "ci" / "compat-envelope" / "pressure-test.rs")
    return [
        [
            pressure,
            "run",
            "--results",
            str(results),
            "--sample",
            str(SAMPLE_COUNT),
            "--jobs",
            "4",
            "--run-timeout",
            str(RUN_TIMEOUT_SECONDS),
        ],
        [pressure, "emit-series", "--results", str(results)],
    ]


def launch(
    root: Path = STATE_ROOT,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> int:
    log = log_path(root)
    log.parent.mkdir(parents=True, exist_ok=True)
    systemctl = shutil.which("systemctl")
    systemd_run = shutil.which("systemd-run")
    with_proxy = shutil.which("with-proxy")
    missing = [
        name
        for name, value in (
            ("systemctl", systemctl),
            ("systemd-run", systemd_run),
            ("with-proxy", with_proxy),
        )
        if value is None
    ]
    if missing:
        emit(
            "unavailable",
            "recurring pressure test not launched; missing " + ",".join(missing),
        )
        return 1

    lock_path = launch_lock_path(root)
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+") as launch_lock:
        try:
            fcntl.flock(launch_lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            emit(
                "running",
                f"recurring pressure test launch already in progress as {UNIT}",
            )
            return 0

        active = run(
            [systemctl, "--user", "is-active", "--quiet", f"{UNIT}.service"],
            capture_output=True,
            text=True,
            check=False,
        )
        if active.returncode == 0:
            emit("running", f"recurring pressure test already active as {UNIT}")
            return 0
        if active.returncode not in (3, 4):
            detail = (
                active.stderr or active.stdout
            ).strip() or f"exit {active.returncode}"
            emit(
                "unavailable",
                "recurring pressure test unit state could not be read: " + detail,
            )
            return 1

        # The unit is inactive and this process holds the launch lock, so no
        # campaign can be using a pressure-test-git directory.
        swept, sweep_problems = sweep_unwrapped_git_directories(root)
        sweep_error = "; ".join(sweep_problems) or None
        if swept or sweep_problems:
            with log.open("a", encoding="utf-8") as retained:
                print(
                    f"{utc_now()} pressure_schedule: swept "
                    f"{unwrapped_git_root(root)}: removed={swept} "
                    f"left={sweep_problems}",
                    file=retained,
                )

        def with_sweep(summary: str) -> str:
            return summary + (f" cleanup_error={sweep_error}" if sweep_error else "")

        fetched = run(
            [
                with_proxy,
                "git",
                "-C",
                str(root / "hermit"),
                "fetch",
                "--no-tags",
                "origin",
                "main",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if fetched.returncode != 0:
            detail = (fetched.stderr or fetched.stdout).strip() or "no output"
            emit(
                "refused",
                with_sweep(
                    f"recurring pressure test could not fetch Hermit main: {detail}"
                ),
            )
            return 1
        resolved = run(
            ["git", "-C", str(root / "hermit"), "rev-parse", "origin/main"],
            capture_output=True,
            text=True,
            check=False,
        )
        target = resolved.stdout.strip()
        if resolved.returncode != 0 or re.fullmatch(r"[0-9a-f]{40}", target) is None:
            detail = resolved.stderr.strip() or target or f"exit {resolved.returncode}"
            emit(
                "refused",
                with_sweep(
                    f"recurring pressure test could not resolve Hermit main: {detail}"
                ),
            )
            return 1

        record = run_registry.record_path(root, UNIT)
        slot = (
            f"pressure-{target[:12]}-"
            f"{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%d%H%M%S')}-"
            f"{secrets.token_hex(4)}"
        )
        try:
            placement = cpu_placement.derive()
            # Planned before the unit exists and created only once the command
            # is known to build; see ci-hub/validate/unwrapped_git.py.
            planned = unwrapped_git.plan(
                os.environ.get("PATH", ""), unwrapped_git_directory(root, slot)
            )
            command = launch_command(
                root,
                systemd_run,
                with_proxy,
                target,
                record,
                placement=placement,
                path=planned.path,
            )
        except ValueError as error:
            emit("refused", with_sweep(str(error)))
            return 1
        git_substitution = unwrapped_git.materialize(planned)
        previous = read_state(root)
        started_at = utc_now()
        try:
            # Rebuilt from the outcome: PATH is unchanged if the directory
            # could not be created, and when it was, the submodule fetch keeps
            # PATH's original git (NETWORK_GIT_ENV).
            command = launch_command(
                root,
                systemd_run,
                with_proxy,
                target,
                record,
                placement=placement,
                path=git_substitution.path,
                network_git_path=(
                    git_substitution.resolved_git if git_substitution.applied else None
                ),
            )
            # The unit appends its own output to this log; stdout here is the
            # health tick's state/summary protocol and must stay two lines.
            with log.open("a", encoding="utf-8") as retained:
                print(
                    f"{utc_now()} pressure_schedule: slot={slot} "
                    f"{git_substitution.summary()}",
                    file=retained,
                )
            run_registry.write_current_record(
                record,
                {
                    "schema_version": run_registry.SCHEMA_VERSION,
                    "kind": "bench",
                    "state": "launching",
                    "unit": f"{UNIT}.service",
                    "target": target,
                    "repo": "rrnewton/hermit",
                    "checkout": str(root / "worktrees" / "validate" / slot),
                    "log": str(log),
                    "agent": "tick-pressure-test",
                    "started_at": started_at,
                    "producer": run_registry.PRODUCER,
                    "admission": "ci-hub validate-lock",
                    "cpu_placement": placement.record(),
                    "git_substitution": git_substitution.record(),
                },
            )
        except BaseException:
            # No unit can exist yet: do not leave this slot's directory behind.
            remove_unwrapped_git_directory(root, slot, git_substitution.record())
            raise
        try:
            started = run(
                command,
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError:
            # systemd-run itself could not be started, so no unit exists.
            remove_unwrapped_git_directory(root, slot, git_substitution.record())
            raise
        if started.returncode != 0:
            detail = (started.stderr or started.stdout).strip() or "no output"
            # systemd refused the unit, so nothing will run in this slot.
            cleanup_error = remove_unwrapped_git_directory(
                root, slot, git_substitution.record()
            )
            run_registry.update_record(
                record,
                state="refused",
                result="systemd-launch-refused",
                exit_code=started.returncode,
                detail=detail,
                finished_at=utc_now(),
                **({"cleanup_error": cleanup_error} if cleanup_error else {}),
            )
            emit(
                "refused",
                with_sweep(
                    f"recurring pressure test was not launched: {detail}"
                    + (f" cleanup_error={cleanup_error}" if cleanup_error else "")
                ),
            )
            return 1
        run_registry.update_record(record, state="running")

        prior = ""
        if previous and previous.get("state") not in (None, "passed"):
            prior = (
                f"; previous state={previous.get('state')} "
                f"target={previous.get('target', '-')}"
            )
        emit(
            "launched",
            with_sweep(
                f"recurring pressure test launched as {UNIT}; "
                f"output appended to {log}{prior}"
            ),
        )
        # A directory the sweep had to leave needs a person, so the health
        # tick reports it the same way it reports a failed previous sample.
        return 1 if prior or sweep_error else 0


def wrkslots_failure_detail(completed: subprocess.CompletedProcess) -> str:
    output = completed.stderr.strip() or completed.stdout.strip()
    if not output:
        return f"exit {completed.returncode}"
    refusals = [line for line in output.splitlines() if line.startswith("REFUSED:")]
    if refusals and not output.startswith("REFUSED:"):
        # wrkslots prints unrelated registry WARNING/INCONSISTENT findings
        # before its refusal; lead with the refusal so the one-line summary
        # names the actual cause.  The full output follows unchanged.
        return "\n".join([*refusals, output])
    return output


def remove_worktree(root: Path, slot: str, machine: str) -> str | None:
    try:
        completed = subprocess.run(
            [
                str(SOURCE_ROOT / "ci-hub/bin/wrkslots"),
                # Executable authority is the immutable/current tool checkout;
                # --project-root below keeps all mutable worktree state in root.
                "--project-root",
                str(root),
                "--machine",
                machine,
                "--wait-lock",
                str(SLOT_LOCK_WAIT_SECONDS),
                "--allow-existing-unregistered-worktrees",
                "remove",
                slot,
                "--validate-complete",
                "--coordinator-pid",
                str(os.getpid()),
                "--expected-generation",
                "1",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        return f"could not launch wrkslots remove: {error}"
    if completed.returncode == 0:
        return None
    return wrkslots_failure_detail(completed)


def run_campaign(target: str, record: Path, root: Path = STATE_ROOT) -> int:
    # Bind creation and retirement to one identity.  Letting wrkslots derive
    # the machine independently at either boundary can strand the slot when
    # hostname presentation changes between a short name and an FQDN.
    machine = socket.gethostname().split(".", 1)[0]
    durable = run_registry.read_record(record)
    raw_checkout = durable.get("checkout")
    if not isinstance(raw_checkout, str):
        # No slot and no worktree, but launch() may have created the git
        # directory the record names.
        git_cleanup_error = remove_recorded_unwrapped_git_directory(
            root, durable.get("git_substitution")
        )
        raise RuntimeError(
            "pressure-test run record lacks its wrkslots checkout; "
            "state: REFUSED -- no worktree was created and no pressure test ran. "
            "remedy: relaunch the recurring pressure test to write a complete run record"
            + (f" cleanup_error={git_cleanup_error}" if git_cleanup_error else "")
        )
    checkout = Path(raw_checkout)
    slot = checkout.name
    if checkout.exists():
        git_cleanup_error = remove_unwrapped_git_directory(
            root, slot, durable.get("git_substitution")
        )
        raise RuntimeError(
            f"refusing existing pressure-test checkout {checkout}"
            + (f" cleanup_error={git_cleanup_error}" if git_cleanup_error else "")
        )

    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    results = root / "ignored" / "compat-envelope" / f"pressure-{target[:12]}-{stamp}"

    state = {
        "schema": 1,
        "state": "running",
        "started_at": utc_now(),
        "target": target,
        "results": str(results),
    }
    write_state(root, state)
    added = subprocess.run(
        [
            str(SOURCE_ROOT / "ci-hub/bin/wrkslots"),
            "--project-root",
            str(root),
            "--machine",
            machine,
            "--wait-lock",
            str(SLOT_LOCK_WAIT_SECONDS),
            "--allow-existing-unregistered-worktrees",
            "create",
            slot,
            "--slot-type",
            "validate",
            "--coordinator-authorized",
            "--agent",
            "tick-pressure-test",
            "--task",
            "recurring-pressure-test",
            "--purpose",
            "run the scheduled compatibility pressure measurement",
            "--owner-pid",
            str(os.getpid()),
            "--coordinator-pid",
            str(os.getpid()),
            "--repo",
            "checkout=hermit",
            "--start",
            f"checkout={target}",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if added.returncode != 0:
        detail = wrkslots_failure_detail(added)
        state.update(
            state="failed", finished_at=utc_now(), detail=f"wrkslots create: {detail}"
        )
        git_cleanup_error = remove_unwrapped_git_directory(
            root, slot, durable.get("git_substitution")
        )
        if git_cleanup_error:
            state["cleanup_error"] = git_cleanup_error
        write_state(root, state)
        raise RuntimeError(state["detail"])

    cleanup_error = None
    try:
        submodules = subprocess.run(
            [
                # This function's only git call that uses the network; see
                # NETWORK_GIT_ENV.
                network_git(),
                "-C",
                str(checkout),
                "submodule",
                "update",
                "--init",
                "--recursive",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if submodules.returncode != 0:
            detail = submodules.stderr.strip() or submodules.stdout.strip()
            raise RuntimeError(
                f"cannot materialize Hermit submodules: {detail or submodules.returncode}"
            )
        completed = subprocess.run(
            locked_command(root, checkout, target, results, record), check=False
        )
        state.update(
            state="passed" if completed.returncode == 0 else "failed",
            finished_at=utc_now(),
            exit_code=completed.returncode,
        )
    except Exception as error:
        state.update(state="failed", finished_at=utc_now(), detail=str(error))
    finally:
        cleanup_errors = [
            error
            for error in (
                remove_worktree(root, slot, machine),
                remove_unwrapped_git_directory(
                    root, slot, durable.get("git_substitution")
                ),
            )
            if error
        ]
        cleanup_error = "; ".join(cleanup_errors) or None
        if cleanup_error:
            state["cleanup_error"] = cleanup_error
            state["state"] = "failed"
            if state.get("exit_code") in (None, 0):
                state["exit_code"] = 1
        write_state(root, state)
        run_registry.update_record(
            record,
            state="completed" if state["state"] == "passed" else "failed",
            result=state["state"],
            exit_code=state.get("exit_code", 1),
            detail=state.get("detail"),
            finished_at=state["finished_at"],
            results=str(results),
            cleanup_error=cleanup_error,
        )

    emit(
        state["state"],
        f"target={target} results={results}"
        + (f" detail={state['detail']}" if state.get("detail") else "")
        + (f" cleanup_error={cleanup_error}" if cleanup_error else ""),
    )
    return 0 if state["state"] == "passed" else 1


def run_locked(checkout: Path, results: Path, root: Path = STATE_ROOT) -> int:
    environment = dict(os.environ)
    environment["DEV_HERMIT_PARENT"] = str(root)
    run_command, emit_command = pressure_commands(checkout, results)
    ran = subprocess.run(run_command, cwd=checkout, env=environment, check=False)
    emitted = None
    if (results / "run.json").is_file():
        emitted = subprocess.run(
            emit_command, cwd=checkout, env=environment, check=False
        )
    if ran.returncode != 0:
        return ran.returncode
    if emitted is None:
        print(
            "pressure_schedule: run produced no run.json; series was not emitted",
            file=sys.stderr,
        )
        return 2
    return emitted.returncode


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    if argv[:1] == ["_run"]:
        worker = argparse.ArgumentParser(add_help=False)
        worker.set_defaults(command="_run")
        worker.add_argument("_command")
        worker.add_argument("--target", required=True)
        worker.add_argument("--record", type=Path, required=True)
        worker.add_argument("--state-root", type=Path, default=STATE_ROOT)
        return worker.parse_args(argv)
    if argv[:1] == ["_locked"]:
        locked = argparse.ArgumentParser(add_help=False)
        locked.set_defaults(command="_locked")
        locked.add_argument("_command")
        locked.add_argument("--checkout", type=Path, required=True)
        locked.add_argument("--results", type=Path, required=True)
        locked.add_argument("--state-root", type=Path, default=STATE_ROOT)
        return locked.parse_args(argv)
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    launch_parser = subparsers.add_parser("launch", help="launch one detached sample")
    launch_parser.add_argument("--state-root", type=Path, default=STATE_ROOT)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.command == "launch":
        return launch(args.state_root)
    if args.command == "_run":
        try:
            return run_campaign(args.target, args.record, args.state_root)
        except RuntimeError as error:
            try:
                run_registry.update_record(
                    args.record,
                    state="failed",
                    result="failed",
                    exit_code=1,
                    detail=str(error),
                    finished_at=utc_now(),
                )
            except RuntimeError as record_error:
                emit(
                    "failed",
                    f"{error}; run record update also failed: {record_error}",
                )
                return 1
            emit("failed", str(error))
            return 1
    if args.command == "_locked":
        return run_locked(args.checkout, args.results, args.state_root)
    raise AssertionError(args.command)


if __name__ == "__main__":
    raise SystemExit(main())
