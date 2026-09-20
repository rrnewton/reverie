#!/usr/bin/env python3
"""Check local Chat bridge user services for bounded resources and write churn.

This probe is deliberately detection-only.  It reads systemd's user-manager
view and writes only its own private comparison baseline.  It never starts,
stops, resets, signals, disables, or rewrites a unit.

WHY THE WRITE-RATE LIMIT IS 5 GiB/HOUR
======================================
The incident that motivated this check retained 604.3 GiB of systemd-accounted
writes in 12 hours 16 minutes: about 49.3 GiB/hour.  A Chat relay has no valid
steady-state reason to write even 5 GiB/hour (120 GiB/day).  That limit catches
the measured failure with roughly a ten-fold margin while leaving orders of
magnitude above bounded state, receipt, and journal traffic.

Each row is bound to systemd's InvocationID and NRestarts. NRestarts counts
automatic cycles caused by Restart=; any observed increase is an actionable
lower bound even though the new invocation's CPU/I/O counters cannot honestly
be added to the old invocation. (The counter can be reset between samples.) An
InvocationID change without an observed NRestarts increase proves a lifecycle
boundary and is consistent with an explicit restart, but does not prove its
cause; it is actionable without being mislabeled as automatic churn. An
NRestarts decrease is an evidence discontinuity: that tick refuses and
normally atomically establishes a new baseline rather than inventing work
across the reset. If the same sample also has an irreconcilable CPU/I/O counter
decrease, preserving the old baseline takes precedence. A first or migrated
baseline surfaces any positive NRestarts value as
unbaselined history rather than silently absorbing it. Output names every unit
whose CPU/I/O interval was unmeasured, so a zero
measured delta is never presented as zero actual I/O. The CPU counter is
compared over the same uninterrupted interval and emitted as millicores so an
idle-loop regression is visible. It is diagnostic rather than a gate: CPU cost
depends on the selected event provider, whereas sustained disk writes at the
limit above are unsafe regardless of provider. A long-lived invocation is
always compared, however old the prior sample. A monotonic CPU/I/O counter
decrease within one invocation is inconsistent evidence and refuses without
changing state.

Installed units are checked before first start as well as while active. The
event broker is capped at 1280 MiB and the bridge at 896 MiB; both are limited
to 128 MiB swap, 384 tasks, 1024 file descriptors, and four CPUs. Restart
backoff, stop deadlines, group teardown, OOM behavior, core limits, and resource
accounting are also part of the standing policy. These are the deployed bounds,
not merely checks that a very large finite number was written in the unit.
NRestarts is validated for stopped and failed units, but restart baselines are
persisted only while per-invocation CPU/I/O counters are live. A failed or
enabled-but-inactive unit is already actionable; if it becomes live, any
positive counter is surfaced as unbaselined restart history.

COST
====
An empty-host preflight uses two bounded systemctl discovery calls. A nonempty
host repeats those calls under the state lock before one batched property call,
so its complete authoritative sample is serialized. The user manager's glob
discovery is O(its installed/loaded unit catalog).
Probe-side parsing and policy work are O(number of matching units); stable
ordering makes state/comparison O(N log N). The restart proof adds one bounded
integer per live unit; the complete serialized state remains capped at 1 MiB.
Disk work is one small state read and one atomic state replacement per
30-minute tick, serialized by a private bounded flock. All state path
components are opened without following symlinks. There is no
filesystem/provider polling loop or tree walk. Lock contention alone uses a
bounded 25 ms retry for at most 0.5 seconds. A proven-empty discovery returns
before opening or writing state.
"""

from __future__ import annotations

import argparse
from collections.abc import Callable, Mapping
from contextlib import AbstractContextManager
from dataclasses import dataclass
import errno
import fcntl
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import time
from typing import Any


GIB = 1 << 30
MIB = 1 << 20
NSEC_PER_SEC = 1_000_000_000
STATE_VERSION = 3
WRITE_LIMIT_BYTES_PER_HOUR = 5 * GIB
SYSTEMCTL_TIMEOUT_SECS = 2
LOCK_WAIT_SECS = 0.5
LOCK_POLL_SECS = 0.025
MAX_STATE_BYTES = 1 << 20
MAX_NRESTARTS = (1 << 32) - 1
STATE_DIRECTORY = ".tick-hub"
STATE_NAME = "chat-bridge-health.json"
LOCK_NAME = "chat-bridge-health.lock"

UNIT_PATTERNS = (
    "agentctl-chat-*.service",
    "agentctl-gchat-events-*.service",
)
UNIT_NAME = re.compile(
    r"agentctl-(?:chat-|gchat-events-)[A-Za-z0-9_.:@\\-]+[.]service\Z"
)
EVENT_UNIT_PREFIX = "agentctl-gchat-events-"
CHAT_UNIT_PREFIX = "agentctl-chat-"
MAX_MEMORY_BY_PREFIX = {
    EVENT_UNIT_PREFIX: 1280 * MIB,
    CHAT_UNIT_PREFIX: 896 * MIB,
}
RESTART_BY_PREFIX = {
    EVENT_UNIT_PREFIX: "always",
    CHAT_UNIT_PREFIX: "on-failure",
}
MAX_STOP_USEC_BY_PREFIX = {
    EVENT_UNIT_PREFIX: 60 * 1_000_000,
    CHAT_UNIT_PREFIX: 180 * 1_000_000,
}
MAX_SWAP_BYTES = 128 * MIB
MAX_TASKS = 384
MAX_NOFILE = 1024
MAX_ALLOWED_CPUS = 4
MAX_START_LIMIT_BURST = 5
MIN_START_LIMIT_INTERVAL_USEC = 10 * 60 * 1_000_000
ENABLED_STATES = frozenset({"enabled", "enabled-runtime"})
ACTIVE_STATES = frozenset(
    {
        "active",
        "reloading",
        "inactive",
        "failed",
        "activating",
        "deactivating",
        "maintenance",
    }
)
SHOW_PROPERTIES = (
    "Id",
    "LoadState",
    "UnitFileState",
    "ActiveState",
    "SubState",
    "MemoryMax",
    "MemorySwapMax",
    "TasksMax",
    "LimitNOFILE",
    "LimitCORE",
    "KillMode",
    "OOMPolicy",
    "AllowedCPUs",
    "Restart",
    "StartLimitBurst",
    "StartLimitIntervalUSec",
    "TimeoutStopUSec",
    "MemoryAccounting",
    "TasksAccounting",
    "IOAccounting",
    "IOWriteBytes",
    "CPUUsageNSec",
    "InvocationID",
    "NRestarts",
)

CommandRunner = Callable[[tuple[str, ...]], str]


class EvidenceUnavailable(RuntimeError):
    """Systemd or persisted evidence cannot support a clean verdict."""


@dataclass(frozen=True)
class UnitEvidence:
    name: str
    properties: Mapping[str, str]

    @property
    def enabled(self) -> bool:
        return self.properties["UnitFileState"] in ENABLED_STATES

    @property
    def active(self) -> bool:
        return self.properties["ActiveState"] in {"active", "reloading"}

    @property
    def needs_resource_guards(self) -> bool:
        # Validate the installed policy before first start as well as while live.
        # A disabled/inactive unit is exactly where preflight must catch drift.
        return self.properties["LoadState"] in {"loaded", "masked"}

    @property
    def has_live_counters(self) -> bool:
        # Transitional cgroups still accumulate CPU and I/O. Preserve their
        # InvocationID-bound baseline until systemd proves the unit inactive.
        return self.properties["ActiveState"] not in {"inactive", "failed"}

    @property
    def failed(self) -> bool:
        return (
            self.properties["ActiveState"] == "failed"
            or self.properties["SubState"] == "failed"
        )


@dataclass(frozen=True)
class Counters:
    io_write_bytes: int
    cpu_usage_nsec: int
    invocation_id: str
    automatic_restarts: int


@dataclass(frozen=True)
class Baseline:
    observed_at_unix_ns: int
    units: Mapping[str, Counters]


@dataclass(frozen=True)
class LoadedBaseline:
    status: str
    baseline: Baseline | None
    error: str = ""


@dataclass(frozen=True)
class Report:
    returncode: int
    fields: Mapping[str, object]


@dataclass(frozen=True)
class Comparison:
    fields: Mapping[str, object]
    findings: tuple[str, ...] = ()
    unavailable: str = ""
    may_write: bool = True


def _clean_error(value: object) -> str:
    return " ".join(str(value).split())[:500]


def run_systemctl(args: tuple[str, ...]) -> str:
    command = ("systemctl", "--user", *args)
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=SYSTEMCTL_TIMEOUT_SECS,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceUnavailable(
            f"cannot execute {' '.join(command[:3])}: {_clean_error(error)}"
        ) from error
    if result.returncode != 0:
        detail = _clean_error(result.stderr or result.stdout or "no diagnostic")
        raise EvidenceUnavailable(
            f"{' '.join(command[:3])} returned {result.returncode}: {detail}"
        )
    return result.stdout


def _unit_names(output: str, source: str) -> set[str]:
    names: set[str] = set()
    for raw in output.splitlines():
        line = raw.strip()
        if not line:
            continue
        name = line.split(maxsplit=1)[0]
        if UNIT_NAME.fullmatch(name) is None:
            raise EvidenceUnavailable(
                f"{source} returned an unexpected row: {_clean_error(raw)}"
            )
        names.add(name)
    return names


def discover_unit_names(run: CommandRunner = run_systemctl) -> tuple[str, ...]:
    installed = run(
        (
            "list-unit-files",
            "--no-legend",
            "--no-pager",
            "--plain",
            *UNIT_PATTERNS,
        )
    )
    loaded = run(
        (
            "list-units",
            "--all",
            "--type=service",
            "--no-legend",
            "--no-pager",
            "--plain",
            *UNIT_PATTERNS,
        )
    )
    return tuple(
        sorted(
            _unit_names(installed, "list-unit-files")
            | _unit_names(loaded, "list-units")
        )
    )


def _parse_show(output: str, expected: tuple[str, ...]) -> list[UnitEvidence]:
    if not expected:
        if output.strip():
            raise EvidenceUnavailable("systemctl show returned rows for no units")
        return []
    blocks = re.split(r"\n[ \t]*\n", output.strip()) if output.strip() else []
    expected_set = set(expected)
    found: dict[str, UnitEvidence] = {}
    for block in blocks:
        properties: dict[str, str] = {}
        for raw in block.splitlines():
            if "=" not in raw:
                raise EvidenceUnavailable(
                    f"systemctl show returned a malformed row: {_clean_error(raw)}"
                )
            key, value = raw.split("=", 1)
            if key in properties:
                raise EvidenceUnavailable(
                    f"systemctl show repeated property {key!r}"
                )
            properties[key] = value
        missing = sorted(set(SHOW_PROPERTIES) - properties.keys())
        if missing:
            raise EvidenceUnavailable(
                "systemctl show omitted properties: " + ",".join(missing)
            )
        name = properties["Id"]
        if name not in expected_set or UNIT_NAME.fullmatch(name) is None:
            raise EvidenceUnavailable(
                f"systemctl show returned unexpected unit {name!r}"
            )
        if name in found:
            raise EvidenceUnavailable(f"systemctl show repeated unit {name}")
        if properties["LoadState"] not in {"loaded", "masked"}:
            raise EvidenceUnavailable(
                f"{name} has unusable LoadState={properties['LoadState']!r}"
            )
        if not properties["UnitFileState"]:
            raise EvidenceUnavailable(f"{name} has empty UnitFileState")
        if properties["ActiveState"] not in ACTIVE_STATES:
            raise EvidenceUnavailable(
                f"{name} has unknown ActiveState={properties['ActiveState']!r}"
            )
        if not properties["SubState"]:
            raise EvidenceUnavailable(f"{name} has empty SubState")
        found[name] = UnitEvidence(name=name, properties=properties)
    missing_units = sorted(expected_set - found.keys())
    if missing_units:
        raise EvidenceUnavailable(
            "systemctl show omitted unit(s): " + ",".join(missing_units)
        )
    return [found[name] for name in expected]


def collect_units(
    names: tuple[str, ...], run: CommandRunner = run_systemctl
) -> list[UnitEvidence]:
    if not names:
        return []
    property_options = tuple(
        option for name in SHOW_PROPERTIES for option in ("--property", name)
    )
    output = run(("show", "--no-pager", *property_options, *names))
    return _parse_show(output, names)


def _bounded_integer(value: str, *, name: str, unit: str) -> tuple[int | None, str]:
    if value == "infinity":
        return None, "infinity"
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise EvidenceUnavailable(
            f"{unit} has malformed {name}={value!r}"
        ) from error
    if parsed < 0:
        raise EvidenceUnavailable(f"{unit} has negative {name}={parsed}")
    # systemd has represented infinity as UINT64_MAX in some interfaces.
    if parsed == (1 << 64) - 1:
        return None, "uint64-max"
    return parsed, "finite"


def _unit_policy_value(unit: str, values: Mapping[str, object]) -> object:
    for prefix, value in values.items():
        if unit.startswith(prefix):
            return value
    raise EvidenceUnavailable(f"no resource policy for {unit}")


def _duration_usec(value: str, *, name: str, unit: str) -> int | None:
    if value == "infinity":
        return None
    if value.isdigit():
        return int(value, 10)
    multipliers = {
        "us": 1,
        "ms": 1_000,
        "s": 1_000_000,
        "min": 60 * 1_000_000,
        "h": 60 * 60 * 1_000_000,
        "d": 24 * 60 * 60 * 1_000_000,
    }
    if len(value) > 128:
        raise EvidenceUnavailable(f"{unit} has malformed {name}={value!r}")
    total = 0
    position = 0
    for match in re.finditer(r"([0-9]+)(us|ms|s|min|h|d)", value):
        if value[position : match.start()].strip():
            raise EvidenceUnavailable(f"{unit} has malformed {name}={value!r}")
        total += int(match.group(1), 10) * multipliers[match.group(2)]
        position = match.end()
    if position == 0 or value[position:].strip():
        raise EvidenceUnavailable(f"{unit} has malformed {name}={value!r}")
    return total


def _allowed_cpu_count(value: str, *, unit: str) -> int:
    if not value or len(value) > 1024:
        raise EvidenceUnavailable(f"{unit} has unbounded AllowedCPUs={value!r}")
    cpus: set[int] = set()
    for token in re.split(r"[ ,]+", value):
        if not token:
            continue
        match = re.fullmatch(r"([0-9]+)(?:-([0-9]+))?", token)
        if match is None:
            raise EvidenceUnavailable(
                f"{unit} has malformed AllowedCPUs={value!r}"
            )
        first = int(match.group(1), 10)
        last = int(match.group(2), 10) if match.group(2) is not None else first
        if last < first:
            raise EvidenceUnavailable(
                f"{unit} has malformed AllowedCPUs={value!r}"
            )
        if last - first + 1 > MAX_ALLOWED_CPUS:
            return MAX_ALLOWED_CPUS + 1
        cpus.update(range(first, last + 1))
        if len(cpus) > MAX_ALLOWED_CPUS:
            break
    if not cpus:
        raise EvidenceUnavailable(f"{unit} has empty AllowedCPUs")
    return len(cpus)


def _counter(value: str, *, name: str, unit: str) -> int:
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise EvidenceUnavailable(
            f"{unit} has unavailable {name}={value!r}"
        ) from error
    if parsed < 0:
        raise EvidenceUnavailable(f"{unit} has negative {name}={parsed}")
    return parsed


def _restart_counter(value: str, *, unit: str) -> int:
    parsed = _counter(value, name="NRestarts", unit=unit)
    # NRestarts is a D-Bus `u` property. Reject impossible wider values rather
    # than allowing adversarial decimal strings to inflate state or arithmetic.
    if parsed > MAX_NRESTARTS:
        raise EvidenceUnavailable(f"{unit} has out-of-range NRestarts={parsed}")
    return parsed


def active_policy_and_counters(
    units: list[UnitEvidence],
) -> tuple[list[str], dict[str, Counters]]:
    issues: list[str] = []
    counters: dict[str, Counters] = {}
    for unit in units:
        props = unit.properties
        if props["LoadState"] == "masked":
            issues.append(f"{unit.name}:masked")
        if unit.failed:
            issues.append(f"{unit.name}:failed")
        if unit.enabled and not unit.active:
            issues.append(f"{unit.name}:enabled-not-active")
        if not unit.needs_resource_guards:
            continue

        memory, memory_kind = _bounded_integer(
            props["MemoryMax"], name="MemoryMax", unit=unit.name
        )
        swap, swap_kind = _bounded_integer(
            props["MemorySwapMax"], name="MemorySwapMax", unit=unit.name
        )
        tasks, tasks_kind = _bounded_integer(
            props["TasksMax"], name="TasksMax", unit=unit.name
        )
        nofile, nofile_kind = _bounded_integer(
            props["LimitNOFILE"], name="LimitNOFILE", unit=unit.name
        )
        core, core_kind = _bounded_integer(
            props["LimitCORE"], name="LimitCORE", unit=unit.name
        )
        memory_limit = int(_unit_policy_value(unit.name, MAX_MEMORY_BY_PREFIX))
        if memory is None or memory <= 0:
            issues.append(f"{unit.name}:MemoryMax-{memory_kind}")
        elif memory > memory_limit:
            issues.append(f"{unit.name}:MemoryMax-above-{memory_limit}")
        if swap is None:
            issues.append(f"{unit.name}:MemorySwapMax-{swap_kind}")
        elif swap > MAX_SWAP_BYTES:
            issues.append(f"{unit.name}:MemorySwapMax-above-{MAX_SWAP_BYTES}")
        if tasks is None or tasks <= 0:
            issues.append(f"{unit.name}:TasksMax-{tasks_kind}")
        elif tasks > MAX_TASKS:
            issues.append(f"{unit.name}:TasksMax-above-{MAX_TASKS}")
        if nofile is None or nofile <= 0:
            issues.append(f"{unit.name}:LimitNOFILE-{nofile_kind}")
        elif nofile > MAX_NOFILE:
            issues.append(f"{unit.name}:LimitNOFILE-above-{MAX_NOFILE}")
        if core_kind != "finite" or core != 0:
            issues.append(f"{unit.name}:LimitCORE-not-zero")
        if props["KillMode"] != "control-group":
            issues.append(f"{unit.name}:KillMode-not-control-group")
        if props["OOMPolicy"] != "kill":
            issues.append(f"{unit.name}:OOMPolicy-not-kill")
        cpu_count = _allowed_cpu_count(props["AllowedCPUs"], unit=unit.name)
        if cpu_count > MAX_ALLOWED_CPUS:
            issues.append(f"{unit.name}:AllowedCPUs-above-{MAX_ALLOWED_CPUS}")
        expected_restart = str(
            _unit_policy_value(unit.name, RESTART_BY_PREFIX)
        )
        if props["Restart"] != expected_restart:
            issues.append(
                f"{unit.name}:Restart-not-{expected_restart}"
            )
        start_burst, start_burst_kind = _bounded_integer(
            props["StartLimitBurst"], name="StartLimitBurst", unit=unit.name
        )
        if start_burst is None or start_burst <= 0:
            issues.append(f"{unit.name}:StartLimitBurst-{start_burst_kind}")
        elif start_burst > MAX_START_LIMIT_BURST:
            issues.append(
                f"{unit.name}:StartLimitBurst-above-{MAX_START_LIMIT_BURST}"
            )
        start_interval = _duration_usec(
            props["StartLimitIntervalUSec"],
            name="StartLimitIntervalUSec",
            unit=unit.name,
        )
        if (
            start_interval is None
            or start_interval < MIN_START_LIMIT_INTERVAL_USEC
        ):
            issues.append(f"{unit.name}:StartLimitIntervalUSec-too-short")
        stop_timeout = _duration_usec(
            props["TimeoutStopUSec"], name="TimeoutStopUSec", unit=unit.name
        )
        max_stop = int(_unit_policy_value(unit.name, MAX_STOP_USEC_BY_PREFIX))
        if stop_timeout is None or not 1 <= stop_timeout <= max_stop:
            issues.append(f"{unit.name}:TimeoutStopUSec-out-of-range")
        for prop in ("MemoryAccounting", "TasksAccounting", "IOAccounting"):
            value = props[prop]
            if value not in {"yes", "no"}:
                raise EvidenceUnavailable(
                    f"{unit.name} has malformed {prop}={value!r}"
                )
            if value != "yes":
                issues.append(f"{unit.name}:{prop}-not-yes")
        if not unit.has_live_counters:
            # NRestarts is part of the standing evidence contract even while
            # stopped. A malformed value must not become healthy merely
            # because the per-invocation CPU/I/O counters are unavailable.
            _restart_counter(props["NRestarts"], unit=unit.name)
            continue
        invocation_id = props["InvocationID"]
        if re.fullmatch(r"[0-9a-f]{32}", invocation_id) is None:
            raise EvidenceUnavailable(
                f"{unit.name} has unavailable InvocationID={invocation_id!r}"
            )
        counters[unit.name] = Counters(
            io_write_bytes=_counter(
                props["IOWriteBytes"], name="IOWriteBytes", unit=unit.name
            ),
            cpu_usage_nsec=_counter(
                props["CPUUsageNSec"], name="CPUUsageNSec", unit=unit.name
            ),
            invocation_id=invocation_id,
            automatic_restarts=_restart_counter(
                props["NRestarts"], unit=unit.name
            ),
        )
    return sorted(set(issues)), counters


def _open_directory_nofollow(path: Path) -> int:
    """Open every path component with O_NOFOLLOW, returning the final dirfd."""
    absolute = Path(os.path.abspath(path))
    current = os.open(
        "/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
    )
    try:
        for component in absolute.parts[1:]:
            following = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=current,
            )
            os.close(current)
            current = following
        return current
    except BaseException:
        os.close(current)
        raise


def _require_owned(info: os.stat_result, *, label: str, exact_mode: int | None) -> None:
    if info.st_uid != os.geteuid():
        raise EvidenceUnavailable(
            f"{label} is owned by uid {info.st_uid}, expected {os.geteuid()}"
        )
    mode = stat.S_IMODE(info.st_mode)
    if exact_mode is not None and mode != exact_mode:
        raise EvidenceUnavailable(
            f"{label} mode is {mode:04o}, expected {exact_mode:04o}"
        )


def _require_regular_private(info: os.stat_result, *, label: str) -> None:
    if not stat.S_ISREG(info.st_mode):
        raise EvidenceUnavailable(f"{label} is not a regular file")
    _require_owned(info, label=label, exact_mode=0o600)


class StateTransaction(AbstractContextManager["StateTransaction"]):
    """One bounded, nofollow, flock-serialized state transaction."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.root_fd = -1
        self.directory_fd = -1
        self.lock_fd = -1
        self.state_identity: tuple[int, int] | None = None

    def __enter__(self) -> "StateTransaction":
        try:
            self.root_fd = _open_directory_nofollow(self.root)
            root_info = os.fstat(self.root_fd)
            _require_owned(root_info, label=f"state root {self.root}", exact_mode=None)
            if stat.S_IMODE(root_info.st_mode) & 0o022:
                raise EvidenceUnavailable(
                    f"state root {self.root} is group/world-writable"
                )
            try:
                os.mkdir(STATE_DIRECTORY, 0o700, dir_fd=self.root_fd)
            except FileExistsError:
                pass
            self.directory_fd = os.open(
                STATE_DIRECTORY,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=self.root_fd,
            )
            directory_info = os.fstat(self.directory_fd)
            _require_owned(
                directory_info,
                label=f"{self.root}/{STATE_DIRECTORY}",
                exact_mode=None,
            )
            if stat.S_IMODE(directory_info.st_mode) & 0o022:
                raise EvidenceUnavailable(
                    f"{self.root}/{STATE_DIRECTORY} is group/world-writable"
                )
            self._verify_directory_identity()
            self.lock_fd = os.open(
                LOCK_NAME,
                os.O_RDWR
                | os.O_CREAT
                | os.O_CLOEXEC
                | os.O_NOFOLLOW,
                0o600,
                dir_fd=self.directory_fd,
            )
            _require_regular_private(
                os.fstat(self.lock_fd), label=f"{self.root}/{STATE_DIRECTORY}/{LOCK_NAME}"
            )
            self._acquire_lock()
            self._verify_directory_identity()
            self._verify_lock_identity()
            return self
        except BaseException:
            self.close()
            raise

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def close(self) -> None:
        if self.lock_fd >= 0:
            try:
                fcntl.flock(self.lock_fd, fcntl.LOCK_UN)
            finally:
                os.close(self.lock_fd)
                self.lock_fd = -1
        if self.directory_fd >= 0:
            os.close(self.directory_fd)
            self.directory_fd = -1
        if self.root_fd >= 0:
            os.close(self.root_fd)
            self.root_fd = -1

    def _acquire_lock(self) -> None:
        deadline = time.monotonic() + LOCK_WAIT_SECS
        while True:
            try:
                fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return
            except BlockingIOError as error:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise EvidenceUnavailable(
                        f"state lock remained busy for {LOCK_WAIT_SECS:.3f}s"
                    ) from error
                time.sleep(min(LOCK_POLL_SECS, remaining))

    def _entry_info(self, name: str) -> os.stat_result | None:
        try:
            return os.stat(name, dir_fd=self.directory_fd, follow_symlinks=False)
        except FileNotFoundError:
            return None

    def _verify_directory_identity(self) -> None:
        try:
            path_info = os.stat(
                STATE_DIRECTORY, dir_fd=self.root_fd, follow_symlinks=False
            )
        except FileNotFoundError as error:
            raise EvidenceUnavailable("state directory disappeared") from error
        descriptor_info = os.fstat(self.directory_fd)
        if not stat.S_ISDIR(path_info.st_mode) or (
            path_info.st_dev,
            path_info.st_ino,
        ) != (descriptor_info.st_dev, descriptor_info.st_ino):
            raise EvidenceUnavailable("state directory path was replaced")
        _require_owned(
            path_info,
            label=f"{self.root}/{STATE_DIRECTORY}",
            exact_mode=None,
        )
        if stat.S_IMODE(path_info.st_mode) & 0o022:
            raise EvidenceUnavailable("state directory became group/world-writable")

    def _verify_lock_identity(self) -> None:
        path_info = self._entry_info(LOCK_NAME)
        descriptor_info = os.fstat(self.lock_fd)
        if path_info is None or not stat.S_ISREG(path_info.st_mode):
            raise EvidenceUnavailable("state lock path disappeared or is not regular")
        if (path_info.st_dev, path_info.st_ino) != (
            descriptor_info.st_dev,
            descriptor_info.st_ino,
        ):
            raise EvidenceUnavailable("state lock path was replaced during acquisition")
        _require_regular_private(
            path_info, label=f"{self.root}/{STATE_DIRECTORY}/{LOCK_NAME}"
        )

    def load_baseline(self) -> LoadedBaseline:
        try:
            fd = os.open(
                STATE_NAME,
                os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=self.directory_fd,
            )
        except FileNotFoundError:
            self.state_identity = None
            return LoadedBaseline("missing", None)
        except OSError as error:
            if error.errno == errno.ELOOP:
                raise EvidenceUnavailable("state path is a symlink") from error
            raise
        try:
            info = os.fstat(fd)
            _require_regular_private(
                info, label=f"{self.root}/{STATE_DIRECTORY}/{STATE_NAME}"
            )
            if info.st_size > MAX_STATE_BYTES:
                raise EvidenceUnavailable(
                    f"state is {info.st_size} bytes, above {MAX_STATE_BYTES} byte bound"
                )
            self.state_identity = (info.st_dev, info.st_ino)
            chunks: list[bytes] = []
            remaining = MAX_STATE_BYTES + 1
            while remaining > 0:
                chunk = os.read(fd, min(65_536, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            if sum(map(len, chunks)) > MAX_STATE_BYTES or (
                remaining <= 0 and os.read(fd, 1)
            ):
                raise EvidenceUnavailable(
                    f"state exceeded {MAX_STATE_BYTES} byte read bound"
                )
        finally:
            os.close(fd)
        self._verify_state_identity()
        try:
            document = json.loads(b"".join(chunks).decode("utf-8"))
            return _baseline_from_document(document)
        except (UnicodeError, json.JSONDecodeError, ValueError) as error:
            return LoadedBaseline("invalid", None, _clean_error(error))

    def _verify_state_identity(self) -> None:
        current = self._entry_info(STATE_NAME)
        if self.state_identity is None:
            if current is not None:
                raise EvidenceUnavailable("state path appeared during transaction")
            return
        if current is None or not stat.S_ISREG(current.st_mode):
            raise EvidenceUnavailable("state path disappeared or became non-regular")
        if (current.st_dev, current.st_ino) != self.state_identity:
            raise EvidenceUnavailable("state path was replaced during transaction")
        _require_regular_private(
            current, label=f"{self.root}/{STATE_DIRECTORY}/{STATE_NAME}"
        )

    def write_baseline(self, payload: bytes) -> None:
        if len(payload) > MAX_STATE_BYTES:
            raise EvidenceUnavailable(
                f"next baseline is {len(payload)} bytes, above "
                f"{MAX_STATE_BYTES} byte bound"
            )
        self._verify_directory_identity()
        self._verify_lock_identity()
        self._verify_state_identity()
        temporary = f".{STATE_NAME}.{os.getpid()}.{secrets.token_hex(8)}"
        temp_fd = -1
        try:
            temp_fd = os.open(
                temporary,
                os.O_WRONLY
                | os.O_CREAT
                | os.O_EXCL
                | os.O_CLOEXEC
                | os.O_NOFOLLOW,
                0o600,
                dir_fd=self.directory_fd,
            )
            _require_regular_private(
                os.fstat(temp_fd), label=f"temporary state {temporary}"
            )
            view = memoryview(payload)
            while view:
                written = os.write(temp_fd, view)
                if written <= 0:
                    raise OSError("short write of state baseline")
                view = view[written:]
            os.fsync(temp_fd)
            os.close(temp_fd)
            temp_fd = -1
            self._verify_directory_identity()
            self._verify_lock_identity()
            self._verify_state_identity()
            os.replace(
                temporary,
                STATE_NAME,
                src_dir_fd=self.directory_fd,
                dst_dir_fd=self.directory_fd,
            )
            temporary = ""
            os.fsync(self.directory_fd)
            state_info = self._entry_info(STATE_NAME)
            if state_info is None:
                raise OSError("atomic state replacement disappeared")
            _require_regular_private(
                state_info, label=f"{self.root}/{STATE_DIRECTORY}/{STATE_NAME}"
            )
            self.state_identity = (state_info.st_dev, state_info.st_ino)
        finally:
            if temp_fd >= 0:
                os.close(temp_fd)
            if temporary:
                try:
                    os.unlink(temporary, dir_fd=self.directory_fd)
                except FileNotFoundError:
                    pass


def _baseline_from_document(document: object) -> LoadedBaseline:
    if not isinstance(document, dict) or document.get("version") != STATE_VERSION:
        raise ValueError("unsupported state version or shape")
    observed = document.get("observed_at_unix_ns")
    rows = document.get("units")
    if isinstance(observed, bool) or not isinstance(observed, int) or observed <= 0:
        raise ValueError("observed_at_unix_ns is not a positive integer")
    if not isinstance(rows, dict):
        raise ValueError("units is not an object")
    units: dict[str, Counters] = {}
    for name, row in rows.items():
        if not isinstance(name, str) or UNIT_NAME.fullmatch(name) is None:
            raise ValueError(f"invalid unit name {name!r}")
        if not isinstance(row, dict) or set(row) != {
            "io_write_bytes",
            "cpu_usage_nsec",
            "invocation_id",
            "automatic_restarts",
        }:
            raise ValueError(f"invalid counter row for {name}")
        io_write = row["io_write_bytes"]
        cpu = row["cpu_usage_nsec"]
        invocation_id = row["invocation_id"]
        automatic_restarts = row["automatic_restarts"]
        if any(
            isinstance(value, bool) or not isinstance(value, int)
            for value in (io_write, cpu, automatic_restarts)
        ):
            raise ValueError(f"non-integer counter for {name}")
        if io_write < 0 or cpu < 0 or automatic_restarts < 0:
            raise ValueError(f"negative counter for {name}")
        if automatic_restarts > MAX_NRESTARTS:
            raise ValueError(f"out-of-range automatic_restarts for {name}")
        if (
            not isinstance(invocation_id, str)
            or re.fullmatch(r"[0-9a-f]{32}", invocation_id) is None
        ):
            raise ValueError(f"invalid invocation_id for {name}")
        units[name] = Counters(io_write, cpu, invocation_id, automatic_restarts)
    return LoadedBaseline("loaded", Baseline(observed, units))


def _state_document(now_ns: int, counters: Mapping[str, Counters]) -> dict[str, Any]:
    return {
        "version": STATE_VERSION,
        "observed_at_unix_ns": now_ns,
        "units": {
            name: {
                "io_write_bytes": row.io_write_bytes,
                "cpu_usage_nsec": row.cpu_usage_nsec,
                "invocation_id": row.invocation_id,
                "automatic_restarts": row.automatic_restarts,
            }
            for name, row in sorted(counters.items())
        },
    }


def serialize_baseline(now_ns: int, counters: Mapping[str, Counters]) -> bytes:
    """Encode and bound the exact bytes later handed to atomic replacement."""
    payload = (
        json.dumps(
            _state_document(now_ns, counters),
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")
    if len(payload) > MAX_STATE_BYTES:
        raise EvidenceUnavailable(
            f"next baseline is {len(payload)} bytes, above {MAX_STATE_BYTES} byte bound"
        )
    return payload


def _rate_per_hour(delta: int, elapsed_ns: int) -> int:
    return delta * 3600 * NSEC_PER_SEC // elapsed_ns


def _empty_comparison_fields(status: str) -> dict[str, object]:
    return {
        "sample_status": status,
        "sample_elapsed_secs": 0,
        "compared_units": 0,
        "baseline_units": 0,
        "invocation_reset_units": "none",
        "counter_reset_units": "none",
        "counter_decrease_units": "none",
        "automatic_restart_delta_lower_bound": 0,
        "automatic_restart_units": "none",
        "unbaselined_automatic_restart_history": 0,
        "unbaselined_automatic_restart_units": "none",
        "io_write_delta_bytes": 0,
        "io_write_rate_bytes_per_hour": 0,
        "io_write_limit_bytes_per_hour": WRITE_LIMIT_BYTES_PER_HOUR,
        "io_write_unmeasured_units": "none",
        "cpu_delta_nsec": 0,
        "cpu_millicores": 0,
        "runaway_units": "none",
    }


def _counter_discontinuity_summary(
    *,
    restart_counter_reset: list[str],
    cpu_io_counter_decrease: list[str],
    additional_findings: list[str],
    will_rebaseline: bool,
) -> str:
    parts: list[str] = []
    if restart_counter_reset:
        restart_detail = "systemd NRestarts decreased for " + ",".join(
            restart_counter_reset
        )
        if will_rebaseline:
            restart_detail += (
                "; replacing that discontinuous restart baseline after this refusal"
            )
        else:
            restart_detail += (
                "; preserving the prior restart baseline because this sample "
                "also has irreconcilable CPU/I/O evidence"
            )
        parts.append(restart_detail)
    if cpu_io_counter_decrease:
        parts.append(
            "monotonic CPU/I/O counters decreased without an InvocationID "
            "change for "
            + ",".join(cpu_io_counter_decrease)
            + "; preserving the prior baseline"
        )
    if additional_findings:
        parts.append("also observed " + "; ".join(additional_findings))
    return "refusing this tick: " + "; ".join(parts)


def compare_counters(
    prior: LoadedBaseline,
    current: Mapping[str, Counters],
    now_ns: int,
) -> Comparison:
    if prior.status in {"missing", "invalid"}:
        status = (
            "baseline-missing"
            if prior.status == "missing"
            else "baseline-invalid-replaced"
        )
        fields = _empty_comparison_fields(status)
        fields["baseline_units"] = len(current)
        fields["io_write_unmeasured_units"] = ",".join(sorted(current)) or "none"
        history_units = sorted(
            name for name, row in current.items() if row.automatic_restarts
        )
        history = sum(current[name].automatic_restarts for name in history_units)
        fields["unbaselined_automatic_restart_history"] = history
        fields["unbaselined_automatic_restart_units"] = (
            ",".join(history_units) or "none"
        )
        initial_findings: tuple[str, ...] = ()
        if history:
            initial_findings = (
                "unbaselined-automatic-restart-history="
                f"{history} across " + ",".join(history_units),
            )
        return Comparison(fields, findings=initial_findings)
    assert prior.baseline is not None
    elapsed_ns = now_ns - prior.baseline.observed_at_unix_ns
    if elapsed_ns <= 0:
        fields = _empty_comparison_fields("nonmonotonic-sample-time")
        return Comparison(
            fields,
            unavailable=(
                "current sample timestamp is not newer than the serialized baseline; "
                "refusing to overwrite newer evidence"
            ),
            may_write=False,
        )

    io_delta = 0
    cpu_delta = 0
    compared = 0
    new_units: list[str] = []
    invocation_reset: list[str] = []
    restart_counter_reset: list[str] = []
    counter_decrease: list[str] = []
    automatic_restart_units: list[str] = []
    automatic_restart_delta_lower_bound = 0
    unbaselined_restart_units: list[str] = []
    unbaselined_restart_history = 0
    per_unit_rates: dict[str, int] = {}
    for name, row in sorted(current.items()):
        old = prior.baseline.units.get(name)
        if old is None:
            new_units.append(name)
            if row.automatic_restarts:
                unbaselined_restart_units.append(name)
                unbaselined_restart_history += row.automatic_restarts
            continue
        if row.automatic_restarts < old.automatic_restarts:
            restart_counter_reset.append(name)
            if row.invocation_id != old.invocation_id:
                invocation_reset.append(name)
                continue
        elif row.automatic_restarts > old.automatic_restarts:
            unit_restart_delta = row.automatic_restarts - old.automatic_restarts
            automatic_restart_delta_lower_bound += unit_restart_delta
            automatic_restart_units.append(name)
            if row.invocation_id != old.invocation_id:
                invocation_reset.append(name)
            # A lifecycle occurred between samples. Even if systemd is
            # observed during a transition with the old InvocationID, do not
            # manufacture one cross-lifecycle CPU/I/O total.
            continue
        if row.invocation_id != old.invocation_id:
            invocation_reset.append(name)
            continue
        if (
            row.io_write_bytes < old.io_write_bytes
            or row.cpu_usage_nsec < old.cpu_usage_nsec
        ):
            counter_decrease.append(name)
            continue
        unit_io_delta = row.io_write_bytes - old.io_write_bytes
        io_delta += unit_io_delta
        cpu_delta += row.cpu_usage_nsec - old.cpu_usage_nsec
        per_unit_rates[name] = _rate_per_hour(unit_io_delta, elapsed_ns)
        compared += 1

    total_rate = _rate_per_hour(io_delta, elapsed_ns) if compared else 0
    runaway = sorted(
        name
        for name, rate in per_unit_rates.items()
        if rate >= WRITE_LIMIT_BYTES_PER_HOUR
    )
    findings: list[str] = []
    if automatic_restart_delta_lower_bound:
        findings.append(
            "automatic-restart-lower-bound="
            f"{automatic_restart_delta_lower_bound} across "
            + ",".join(automatic_restart_units)
        )
    if unbaselined_restart_history:
        findings.append(
            "unbaselined-automatic-restart-history="
            f"{unbaselined_restart_history} across "
            + ",".join(unbaselined_restart_units)
        )
    nonautomatic_rollovers = sorted(
        set(invocation_reset) - set(automatic_restart_units)
    )
    if nonautomatic_rollovers:
        findings.append(
            "invocation-rollover-without-observed-automatic-increase="
            f"{len(nonautomatic_rollovers)} across " + ",".join(nonautomatic_rollovers)
        )
    if total_rate >= WRITE_LIMIT_BYTES_PER_HOUR:
        findings.append(
            "aggregate-write-rate="
            f"{total_rate}B/h>={WRITE_LIMIT_BYTES_PER_HOUR}B/h"
        )
    unmeasured_units = sorted(
        set(new_units)
        | set(invocation_reset)
        | set(counter_decrease)
        | set(automatic_restart_units)
    )
    if restart_counter_reset and counter_decrease:
        sample_status = "inconsistent-counter-evidence"
    elif restart_counter_reset:
        sample_status = "restart-counter-decrease"
    elif counter_decrease:
        sample_status = "inconsistent-counter-decrease"
    elif compared and unmeasured_units:
        sample_status = "partial-counter-baseline"
    elif automatic_restart_units:
        sample_status = "automatic-restart-baseline"
    elif invocation_reset:
        sample_status = "invocation-reset-baseline"
    elif new_units:
        sample_status = "new-unit-baseline"
    else:
        sample_status = "compared"
    return Comparison(
        fields={
            "sample_status": sample_status,
            "sample_elapsed_secs": elapsed_ns // NSEC_PER_SEC,
            "compared_units": compared,
            "baseline_units": len(new_units),
            "invocation_reset_units": ",".join(invocation_reset) or "none",
            "counter_reset_units": (",".join(restart_counter_reset) or "none"),
            "counter_decrease_units": ",".join(counter_decrease) or "none",
            "automatic_restart_delta_lower_bound": (
                automatic_restart_delta_lower_bound
            ),
            "automatic_restart_units": (",".join(automatic_restart_units) or "none"),
            "unbaselined_automatic_restart_history": (unbaselined_restart_history),
            "unbaselined_automatic_restart_units": (
                ",".join(unbaselined_restart_units) or "none"
            ),
            "io_write_delta_bytes": io_delta,
            "io_write_rate_bytes_per_hour": total_rate,
            "io_write_limit_bytes_per_hour": WRITE_LIMIT_BYTES_PER_HOUR,
            "io_write_unmeasured_units": (",".join(unmeasured_units) or "none"),
            "cpu_delta_nsec": cpu_delta,
            "cpu_millicores": cpu_delta * 1000 // elapsed_ns,
            "runaway_units": ",".join(runaway) or "none",
        },
        findings=tuple(findings),
        unavailable=(
            _counter_discontinuity_summary(
                restart_counter_reset=restart_counter_reset,
                cpu_io_counter_decrease=counter_decrease,
                additional_findings=findings,
                will_rebaseline=not counter_decrease,
            )
            if restart_counter_reset or counter_decrease
            else ""
        ),
        # A CPU/I/O decrease with the same invocation remains irreconcilable;
        # preserve the old baseline. NRestarts may legitimately be reset by
        # the manager or service, so that discontinuity refuses one tick and
        # rebaselines for bounded recovery.
        may_write=not counter_decrease,
    )


def _unknown_report(summary: str) -> Report:
    fields: dict[str, object] = {
        "state": "unknown",
        "summary": summary,
        "units_discovered": "unknown",
        "units_enabled": "unknown",
        "units_active": "unknown",
        "units_failed": "unknown",
        "policy_issues": "unknown",
    }
    fields.update(_empty_comparison_fields("unavailable"))
    return Report(2, fields)


def _no_units_report() -> Report:
    fields: dict[str, object] = {
        "state": "ok",
        "summary": "no matching Chat bridge user services are installed or loaded",
        "units_discovered": 0,
        "units_enabled": 0,
        "units_active": 0,
        "units_failed": 0,
        "policy_issues": "none",
    }
    fields.update(_empty_comparison_fields("no-units"))
    return Report(0, fields)


def evaluate(
    *,
    root: Path,
    run: CommandRunner = run_systemctl,
    now_ns: int | None = None,
) -> Report:
    try:
        names = discover_unit_names(run)
    except (EvidenceUnavailable, OSError) as error:
        return _unknown_report(
            "REFUSING: user-systemd Chat service evidence is unavailable or malformed: "
            + _clean_error(error)
        )
    # A proven-empty host is a genuine no-op.  In particular, stale/corrupt
    # state from a prior installation must not turn absence into a failure or a
    # disk write.
    if not names:
        return _no_units_report()

    try:
        with StateTransaction(root) as transaction:
            # This ordering is load-bearing for concurrency: the lock covers
            # the baseline read, timestamp, authoritative property collection,
            # comparison, and replacement as one transaction.
            prior = transaction.load_baseline()
            observed_ns = time.time_ns() if now_ns is None else now_ns
            serialized_names = discover_unit_names(run)
            if not serialized_names:
                raise EvidenceUnavailable(
                    "matching units disappeared during the serialized sample"
                )
            units = collect_units(serialized_names, run)
            policy_issues, counters = active_policy_and_counters(units)
            comparison = compare_counters(prior, counters, observed_ns)
            if comparison.may_write:
                payload = serialize_baseline(observed_ns, counters)
                transaction.write_baseline(payload)
    except (EvidenceUnavailable, OSError) as error:
        return _unknown_report(
            "REFUSING: serialized private state transaction or systemd evidence "
            "is unavailable: "
            + _clean_error(error)
        )

    enabled = sum(unit.enabled for unit in units)
    active = sum(unit.active for unit in units)
    failed = sum(unit.failed for unit in units)
    fields: dict[str, object] = {
        "state": "ok",
        "summary": "",
        "units_discovered": len(units),
        "units_enabled": enabled,
        "units_active": active,
        "units_failed": failed,
        "policy_issues": "|".join(policy_issues) or "none",
    }
    fields.update(comparison.fields)

    if comparison.unavailable:
        fields["state"] = "unknown"
        fields["summary"] = "REFUSING: " + comparison.unavailable
        return Report(2, fields)

    if prior.status == "invalid":
        state_path = root / STATE_DIRECTORY / STATE_NAME
        fields["state"] = "unknown"
        invalid_summary = (
            f"REFUSING one tick: prior baseline {state_path} was invalid "
            f"({_clean_error(prior.error)}); replaced it atomically from current counters"
        )
        if comparison.findings:
            invalid_summary += "; current systemd finding(s): " + "; ".join(
                comparison.findings
            )
        fields["summary"] = invalid_summary
        return Report(2, fields)

    findings = [*policy_issues, *comparison.findings]
    if findings:
        fields["state"] = "actionable"
        fields["summary"] = "Chat bridge service health finding(s): " + "; ".join(
            findings
        )
        return Report(1, fields)

    if comparison.fields["sample_status"] != "compared":
        fields["summary"] = (
            f"{len(units)} matching unit(s) satisfy standing policy; "
            f"write/CPU comparison status={comparison.fields['sample_status']}"
        )
    else:
        fields["summary"] = (
            f"{len(units)} matching unit(s) satisfy standing policy; "
            "aggregate writes="
            f"{comparison.fields['io_write_rate_bytes_per_hour']}B/h; "
            f"CPU={comparison.fields['cpu_millicores']} millicores"
        )
    return Report(0, fields)


def _default_root() -> Path:
    configured = os.environ.get("DEV_HERMIT_PARENT")
    if configured:
        return Path(configured)
    return Path(__file__).resolve().parents[2]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Check Chat bridge user services and cumulative resource counters."
    )
    parser.add_argument("--root", type=Path, default=_default_root())
    args = parser.parse_args(argv)
    report = evaluate(root=args.root)
    for key, value in report.fields.items():
        print(f"{key}={value}")
    return report.returncode


if __name__ == "__main__":
    raise SystemExit(main())
