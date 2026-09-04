#!/usr/bin/env python3
"""Fast, gateable host-load precondition for timing-sensitive measurements.

Aggregate CPU, load, PSI, and memory counters remain host-wide inside 3pai's
PID namespace.  Process names and states do not.  In that case the report says
so explicitly and ranks cgroup CPU consumers instead of pretending the visible
process table is complete.
"""

from __future__ import annotations

import argparse
import json
import os
import resource
import sys
import time
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable, Mapping, Sequence

DEFAULT_SAMPLE_SECONDS = 1.0
# Policy, not an observation: the 2026-08-03 calibration at 31.45% executing
# CPU / 63.86% idle was suitable. A 50% ceiling preserves meaningful headroom
# while rejecting measurements made when most cores are executing other work.
DEFAULT_MAX_EXECUTING_PERCENT = 50.0
DEFAULT_MIN_MEMORY_AVAILABLE_PERCENT = 10.0
DEFAULT_TOP = 5


class ProbeUnavailable(RuntimeError):
    """A required measurement could not be collected."""


@dataclass(frozen=True)
class CpuCounters:
    total: int
    idle: int
    iowait: int


@dataclass(frozen=True)
class CpuMeasurement:
    executing_percent: float
    idle_percent: float
    iowait_percent: float
    executing_cores: float
    cpus: int


@dataclass(frozen=True)
class ProcessSample:
    ticks: int
    state: str
    name: str


@dataclass(frozen=True)
class Consumer:
    name: str
    cpu_percent: float


@dataclass(frozen=True)
class MemoryMeasurement:
    total_bytes: int
    available_bytes: int
    available_percent: float
    swap_used_bytes: int
    pressure_avg10: float | None


@dataclass(frozen=True)
class Verdict:
    suitable: bool
    reasons: tuple[str, ...]


def read_cpu_counters(path: Path) -> CpuCounters:
    try:
        first = path.read_text().splitlines()[0].split()
    except (OSError, IndexError) as error:
        raise ProbeUnavailable(f"cannot read aggregate CPU counters from {path}: {error}")
    if not first or first[0] != "cpu":
        raise ProbeUnavailable(f"{path}: missing aggregate 'cpu' line")
    try:
        values = [int(value) for value in first[1:]]
    except ValueError as error:
        raise ProbeUnavailable(f"{path}: malformed aggregate CPU counters") from error
    if len(values) < 5:
        raise ProbeUnavailable(f"{path}: expected at least five aggregate CPU counters")
    # guest/guest_nice are already included in user/nice; summing them again
    # would overstate the denominator.
    total = sum(values[:8])
    return CpuCounters(total=total, idle=values[3], iowait=values[4])


def cpu_measurement(
    before: CpuCounters, after: CpuCounters, *, cpus: int
) -> CpuMeasurement:
    total = after.total - before.total
    idle = after.idle - before.idle
    iowait = after.iowait - before.iowait
    if total <= 0 or min(idle, iowait) < 0:
        raise ProbeUnavailable("aggregate CPU counters did not advance monotonically")
    executing = max(0, total - idle - iowait)
    return CpuMeasurement(
        executing_percent=100.0 * executing / total,
        idle_percent=100.0 * idle / total,
        iowait_percent=100.0 * iowait / total,
        executing_cores=cpus * executing / total,
        cpus=cpus,
    )


def parse_process_stat(text: str) -> ProcessSample:
    left = text.find("(")
    right = text.rfind(")")
    if left < 0 or right <= left:
        raise ValueError("missing process name")
    name = text[left + 1 : right]
    fields = text[right + 1 :].split()
    if len(fields) < 13:
        raise ValueError("short process stat record")
    return ProcessSample(
        ticks=int(fields[11]) + int(fields[12]),
        state=fields[0][:1],
        name=name,
    )


def snapshot_processes(proc_root: Path) -> dict[int, ProcessSample]:
    samples: dict[int, ProcessSample] = {}
    try:
        entries = list(proc_root.iterdir())
    except OSError as error:
        raise ProbeUnavailable(f"cannot enumerate {proc_root}: {error}") from error
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            samples[int(entry.name)] = parse_process_stat(
                (entry / "stat").read_text()
            )
        except (OSError, ValueError):
            # Process exit races are normal during a snapshot.
            continue
    if not samples:
        raise ProbeUnavailable(f"{proc_root}: process table was empty or unreadable")
    return samples


def process_states(samples: Mapping[int, ProcessSample]) -> dict[str, int]:
    counts = Counter(sample.state for sample in samples.values())
    return {state: counts.get(state, 0) for state in ("R", "S", "D", "Z")}


def top_processes(
    before: Mapping[int, ProcessSample],
    after: Mapping[int, ProcessSample],
    *,
    elapsed: float,
    ticks_per_second: int,
    limit: int,
) -> list[Consumer]:
    by_name: Counter[str] = Counter()
    for pid, current in after.items():
        previous = before.get(pid)
        if previous is None or previous.name != current.name:
            continue
        delta = current.ticks - previous.ticks
        if delta > 0:
            by_name[current.name] += delta
    return [
        Consumer(name=name, cpu_percent=100.0 * ticks / (ticks_per_second * elapsed))
        for name, ticks in by_name.most_common(limit)
    ]


def read_cgroup_usage(path: Path) -> int | None:
    try:
        for line in (path / "cpu.stat").read_text().splitlines():
            key, value = line.split(maxsplit=1)
            if key == "usage_usec":
                return int(value)
    except (OSError, ValueError):
        return None
    return None


def snapshot_cgroups(root: Path) -> dict[Path, int]:
    samples: dict[Path, int] = {}
    try:
        candidates = [root, *(path for path in root.rglob("*") if path.is_dir())]
    except OSError as error:
        raise ProbeUnavailable(f"cannot enumerate cgroups under {root}: {error}") from error
    for path in candidates:
        usage = read_cgroup_usage(path)
        if usage is not None:
            samples[path] = usage
    if not samples:
        raise ProbeUnavailable(f"{root}: no readable cgroup cpu.stat files")
    return samples


def top_cgroups(
    before: Mapping[Path, int],
    after: Mapping[Path, int],
    *,
    root: Path,
    elapsed: float,
    limit: int,
) -> list[Consumer]:
    deltas = {
        path: max(0, value - before[path])
        for path, value in after.items()
        if path in before
    }
    children: dict[Path, list[Path]] = {}
    for path in deltas:
        children.setdefault(path.parent, []).append(path)
    exclusive: list[tuple[int, Path]] = []
    for path, delta in deltas.items():
        local = max(0, delta - sum(deltas[child] for child in children.get(path, ())))
        if local:
            exclusive.append((local, path))
    exclusive.sort(reverse=True)
    consumers: list[Consumer] = []
    for usec, path in exclusive[:limit]:
        try:
            relative = path.relative_to(root)
            name = relative.name or "/"
        except ValueError:
            name = str(path)
        consumers.append(Consumer(name=name, cpu_percent=100.0 * usec / (elapsed * 1e6)))
    return consumers


def read_meminfo(path: Path, pressure_path: Path) -> MemoryMeasurement:
    values: dict[str, int] = {}
    try:
        for line in path.read_text().splitlines():
            fields = line.replace(":", "").split()
            if len(fields) >= 2 and fields[0] in {
                "MemTotal",
                "MemAvailable",
                "SwapTotal",
                "SwapFree",
            }:
                values[fields[0]] = int(fields[1]) * 1024
    except (OSError, ValueError) as error:
        raise ProbeUnavailable(f"cannot read memory counters from {path}: {error}")
    if values.get("MemTotal", 0) <= 0 or "MemAvailable" not in values:
        raise ProbeUnavailable(f"{path}: missing MemTotal or MemAvailable")
    pressure = read_psi_avg10(pressure_path)
    total = values["MemTotal"]
    available = values["MemAvailable"]
    return MemoryMeasurement(
        total_bytes=total,
        available_bytes=available,
        available_percent=100.0 * available / total,
        swap_used_bytes=max(0, values.get("SwapTotal", 0) - values.get("SwapFree", 0)),
        pressure_avg10=pressure,
    )


def read_psi_avg10(path: Path) -> float | None:
    try:
        for line in path.read_text().splitlines():
            if not line.startswith("some "):
                continue
            for field in line.split()[1:]:
                if field.startswith("avg10="):
                    return float(field.split("=", 1)[1])
    except (OSError, ValueError):
        return None
    return None


def read_loadavg(path: Path) -> tuple[float, float, float]:
    try:
        values = path.read_text().split()
        return float(values[0]), float(values[1]), float(values[2])
    except (OSError, ValueError, IndexError) as error:
        raise ProbeUnavailable(f"cannot read load averages from {path}: {error}")


def pid_namespace_scope(proc_root: Path) -> str:
    try:
        pid1 = (proc_root / "1" / "comm").read_text().strip()
    except OSError:
        return "unknown"
    return "host" if pid1 in {"systemd", "init"} else "namespaced"


def own_cgroup_pids(proc_root: Path, cgroup_root: Path) -> int | None:
    try:
        for line in (proc_root / "self" / "cgroup").read_text().splitlines():
            hierarchy, _, relative = line.partition("::")
            if hierarchy == "0":
                return int((cgroup_root / relative.lstrip("/") / "pids.current").read_text())
    except (OSError, ValueError):
        return None
    return None


def decide(
    cpu: CpuMeasurement,
    memory: MemoryMeasurement,
    *,
    max_executing_percent: float,
    min_memory_available_percent: float,
) -> Verdict:
    failures: list[str] = []
    if cpu.executing_percent > max_executing_percent:
        failures.append(
            f"executing CPU {cpu.executing_percent:.2f}% > policy {max_executing_percent:.2f}%"
        )
    if memory.available_percent < min_memory_available_percent:
        failures.append(
            f"MemAvailable {memory.available_percent:.2f}% < policy "
            f"{min_memory_available_percent:.2f}%"
        )
    if failures:
        return Verdict(suitable=False, reasons=tuple(failures))
    reasons = [
        f"executing CPU {cpu.executing_percent:.2f}% <= policy {max_executing_percent:.2f}%",
        f"MemAvailable {memory.available_percent:.2f}% >= policy "
        f"{min_memory_available_percent:.2f}%",
    ]
    return Verdict(suitable=True, reasons=tuple(reasons))


def human_bytes(value: int) -> str:
    return f"{value / (1024**3):.1f}GiB"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--sample-seconds", type=float, default=DEFAULT_SAMPLE_SECONDS)
    parser.add_argument(
        "--max-executing-percent",
        type=float,
        default=DEFAULT_MAX_EXECUTING_PERCENT,
        help="policy ceiling for measured executing CPU (load average is not used)",
    )
    parser.add_argument(
        "--min-memory-available-percent",
        type=float,
        default=DEFAULT_MIN_MEMORY_AVAILABLE_PERCENT,
    )
    parser.add_argument("--top", type=int, default=DEFAULT_TOP)
    parser.add_argument("--json", action="store_true")
    return parser


def run(
    args: argparse.Namespace,
    *,
    proc_root: Path = Path("/proc"),
    cgroup_root: Path = Path("/sys/fs/cgroup"),
    sleeper: Callable[[float], None] = time.sleep,
) -> tuple[int, dict[str, object]]:
    if not 0.1 <= args.sample_seconds <= 30.0:
        raise ProbeUnavailable("--sample-seconds must be between 0.1 and 30")
    if not 0.0 <= args.max_executing_percent <= 100.0:
        raise ProbeUnavailable("--max-executing-percent must be between 0 and 100")
    if not 0.0 <= args.min_memory_available_percent <= 100.0:
        raise ProbeUnavailable("--min-memory-available-percent must be between 0 and 100")
    if not 1 <= args.top <= 20:
        raise ProbeUnavailable("--top must be between 1 and 20")

    cpu_before = read_cpu_counters(proc_root / "stat")
    started = time.monotonic()
    processes_before = snapshot_processes(proc_root)
    scope = pid_namespace_scope(proc_root)
    cgroups_before = snapshot_cgroups(cgroup_root) if scope != "host" else {}
    sleeper(args.sample_seconds)
    processes_after = snapshot_processes(proc_root)
    cgroups_after = snapshot_cgroups(cgroup_root) if scope != "host" else {}
    cpu_after = read_cpu_counters(proc_root / "stat")
    elapsed = time.monotonic() - started

    cpus = (
        len(os.sched_getaffinity(0))
        if hasattr(os, "sched_getaffinity")
        else (os.cpu_count() or 1)
    )
    cpu = cpu_measurement(cpu_before, cpu_after, cpus=cpus)
    memory = read_meminfo(proc_root / "meminfo", proc_root / "pressure" / "memory")
    load = read_loadavg(proc_root / "loadavg")
    states = process_states(processes_after)
    visible = len(processes_after)
    scope_pids = own_cgroup_pids(proc_root, cgroup_root) if scope != "host" else None
    hidden = max(0, scope_pids - visible) if scope_pids is not None else None
    ticks_per_second = int(os.sysconf("SC_CLK_TCK"))
    if scope == "host":
        consumers = top_processes(
            processes_before,
            processes_after,
            elapsed=elapsed,
            ticks_per_second=ticks_per_second,
            limit=args.top,
        )
        consumer_kind = "process"
    else:
        consumers = top_cgroups(
            cgroups_before,
            cgroups_after,
            root=cgroup_root,
            elapsed=elapsed,
            limit=args.top,
        )
        consumer_kind = "cgroup"
    verdict = decide(
        cpu,
        memory,
        max_executing_percent=args.max_executing_percent,
        min_memory_available_percent=args.min_memory_available_percent,
    )
    payload: dict[str, object] = {
        "schema": 1,
        "sample_seconds": round(elapsed, 6),
        "cpu": asdict(cpu),
        "load": {"one": load[0], "five": load[1], "fifteen": load[2]},
        "memory": asdict(memory),
        "processes": {
            "scope": scope,
            "visible": visible,
            "scope_pids": scope_pids,
            "hidden": hidden,
            "states": states,
        },
        "top_kind": consumer_kind,
        "top": [asdict(consumer) for consumer in consumers],
        "policy": {
            "max_executing_percent": args.max_executing_percent,
            "min_memory_available_percent": args.min_memory_available_percent,
        },
        "verdict": {"suitable": verdict.suitable, "reasons": list(verdict.reasons)},
    }
    return (0 if verdict.suitable else 1), payload


def print_human(payload: Mapping[str, object]) -> None:
    cpu = payload["cpu"]
    memory = payload["memory"]
    processes = payload["processes"]
    load = payload["load"]
    verdict = payload["verdict"]
    assert isinstance(cpu, dict) and isinstance(memory, dict)
    assert isinstance(processes, dict) and isinstance(load, dict) and isinstance(verdict, dict)
    print(
        f"CPU sample={payload['sample_seconds']:.2f}s executing={cpu['executing_percent']:.2f}% "
        f"({cpu['executing_cores']:.2f}/{cpu['cpus']} cores) "
        f"idle={cpu['idle_percent']:.2f}% iowait={cpu['iowait_percent']:.2f}%"
    )
    top = payload["top"]
    assert isinstance(top, list)
    rendered = ", ".join(
        f"{item['name']}={item['cpu_percent'] / 100.0:.2f} cores"
        for item in top
        if isinstance(item, dict)
    ) or "none observed"
    print(f"TOP {str(payload['top_kind']).upper()} CPU {rendered}")
    states = processes["states"]
    assert isinstance(states, dict)
    coverage = f"visible={processes['visible']}"
    if processes.get("scope_pids") is not None:
        coverage += f" scope_pids={processes['scope_pids']} hidden={processes['hidden']}"
    print(
        f"PROCESS STATES scope={processes['scope']} {coverage} "
        f"R={states['R']} S={states['S']} D={states['D']} Z={states['Z']}"
    )
    if processes["scope"] != "host":
        print(
            "PROCESS NOTE host process names/states hidden by PID namespace; "
            "cgroup CPU shown above"
        )
    psi = memory.get("pressure_avg10")
    psi_text = "unavailable" if psi is None else f"{psi:.2f}%"
    print(
        f"MEMORY available={memory['available_percent']:.2f}% "
        f"({human_bytes(int(memory['available_bytes']))}/"
        f"{human_bytes(int(memory['total_bytes']))}) "
        f"swap_used={human_bytes(int(memory['swap_used_bytes']))} PSI_avg10={psi_text}"
    )
    print(
        f"LOAD diagnostic_only one={load['one']:.2f} five={load['five']:.2f} "
        f"fifteen={load['fifteen']:.2f} (not used for verdict)"
    )
    label = "SUITABLE" if verdict["suitable"] else "NOT SUITABLE"
    print(f"VERDICT {label}: {'; '.join(verdict['reasons'])}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    self_costed = os.environ.get("CI_HUB_TOOL_COST_ACTIVE") is None
    wall_started = time.perf_counter()
    usage_started = resource.getrusage(resource.RUSAGE_SELF)
    # The unknown-estimate line was removed here too: it printed on every probe and
    # said only that nothing had been measured. See `tool_cost.print_estimate`.
    code = 2
    try:
        code, payload = run(args)
    except ProbeUnavailable as error:
        print(f"LOAD PROBE UNAVAILABLE: {error}", file=sys.stderr)
    else:
        if args.json:
            print(json.dumps(payload, sort_keys=True))
        else:
            print_human(payload)
    finally:
        if self_costed:
            usage = resource.getrusage(resource.RUSAGE_SELF)
            user = usage.ru_utime - usage_started.ru_utime
            system = usage.ru_stime - usage_started.ru_stime
            print(
                "# ci-hub/load-probe tool COST ACTUAL "
                f"wall={time.perf_counter() - wall_started:.6f}s "
                f"cpu={user + system:.6f}s cpu_user={user:.6f}s "
                f"cpu_system={system:.6f}s exit={code}",
                file=sys.stderr,
            )
    return code


if __name__ == "__main__":
    raise SystemExit(main())
