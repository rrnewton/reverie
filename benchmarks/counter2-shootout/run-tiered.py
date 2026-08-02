#!/usr/bin/env python3
"""Compare exact counter2 with Hermit relaxed and strict execution tiers."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import random
import re
import signal
import socket
import statistics
import subprocess
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence

import run as counter2


TIERS = ("counter2", "relaxed", "strict")
RELAXED_EXCLUSIONS = {
    "dbi": "Hermit's DBI backend requires sequentialized threads",
}
TIER_ARGUMENTS = {
    "relaxed": ("--no-sequentialize-threads", "--max-timeslice=disabled"),
    "strict": ("--strict",),
}


@dataclass(frozen=True)
class Execution:
    workload: str
    tier: str
    backend: str
    variant: str
    iterations: int
    stride: int

    @property
    def key(self) -> tuple[str, str, str, str]:
        return self.workload, self.tier, self.backend, self.variant

    @property
    def label(self) -> str:
        return "/".join(self.key)


def comma_separated(raw: str, allowed: Sequence[str], option: str) -> list[str]:
    selected = [item.strip() for item in raw.split(",") if item.strip()]
    invalid = sorted(set(selected) - set(allowed))
    if not selected or invalid or len(selected) != len(set(selected)):
        counter2.fail(
            f"invalid {option} value {raw!r}; expected a unique subset of "
            f"{','.join(allowed)}"
        )
    return selected


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run the same calibrated workloads through exact counter2 and "
            "Hermit relaxed/strict tiers."
        )
    )
    parser.add_argument("--backends", default=",".join(counter2.BACKENDS))
    parser.add_argument("--tiers", default=",".join(TIERS))
    parser.add_argument("--workloads")
    parser.add_argument("--target-seconds", type=float, default=1.0)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--timeout-seconds", type=float, default=180.0)
    parser.add_argument("--profile", choices=("debug", "release"), default="release")
    parser.add_argument("--hermit", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--seed", type=int, default=1)
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_process(
    command: list[str], env: dict[str, str], timeout_seconds: float, root: Path
) -> counter2.Outcome:
    started = time.perf_counter_ns()
    process = subprocess.Popen(
        command,
        cwd=root,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired as error:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        stdout, stderr = process.communicate()
        raise RuntimeError(
            f"timeout after {timeout_seconds:.1f}s: {' '.join(command)}\n"
            f"stdout:\n{stdout[-4096:].decode(errors='replace')}\n"
            f"stderr:\n{stderr[-4096:].decode(errors='replace')}"
        ) from error
    duration_ms = (time.perf_counter_ns() - started) / 1_000_000.0
    total, processes, threads = counter2.parse_counter(stderr)
    return counter2.Outcome(
        command=command,
        duration_ms=duration_ms,
        returncode=process.returncode,
        stdout=stdout,
        stderr=stderr,
        counter_total=total,
        counter_processes=processes,
        counter_threads=threads,
    )


def execution_arguments(execution: Execution) -> list[str]:
    return [
        "--iterations",
        str(execution.iterations),
        "--stride",
        str(execution.stride),
    ]


def hermit_environment(
    execution: Execution, root: Path, target: Path
) -> dict[str, str]:
    env = counter2.base_environment()
    if execution.backend == "dbi" and not env.get("DYNAMORIO_HOME"):
        helper = counter2.require_file(
            target / "reverie-dbi-dynamorio-path", "DBI DynamoRIO path helper"
        )
        env["DYNAMORIO_HOME"] = subprocess.check_output(
            [str(helper), "home"], cwd=root, text=True
        ).strip()
    elif execution.backend == "sabre":
        env.update(
            {
                "HERMIT_SABRE_RUNNER": str(
                    counter2.require_file(
                        target / "reverie-sabre-strace", "SaBRe runner"
                    )
                ),
                "HERMIT_SABRE_BINARY": str(
                    counter2.require_file(root / "target/sabre/sabre", "SaBRe loader")
                ),
                "HERMIT_SABRE_PLUGIN": str(
                    counter2.require_file(
                        target / "libreverie_sabre_strace_plugin.so", "SaBRe plugin"
                    )
                ),
            }
        )
    elif execution.backend == "e9patch":
        env.update(
            {
                "HERMIT_E9TOOL": str(
                    counter2.require_file(
                        root / "third-party/e9patch/e9tool", "e9tool"
                    )
                ),
                "HERMIT_E9PATCH_BACKEND": str(
                    counter2.require_file(
                        root / "third-party/e9patch/e9patch", "e9patch backend"
                    )
                ),
            }
        )
    return env


def command_and_environment(
    execution: Execution,
    artifacts: dict[str, Path],
    hermit: Path | None,
    root: Path,
    target: Path,
) -> tuple[list[str], dict[str, str]]:
    artifact = artifacts[execution.variant]
    arguments = execution_arguments(execution)
    if execution.tier == "native":
        return [str(artifact), *arguments], counter2.base_environment()
    if execution.tier == "counter2":
        return (
            counter2.backend_command(
                execution.backend, artifact, arguments, root, target
            ),
            counter2.backend_environment(
                execution.backend, counter2.base_environment(), root, target
            ),
        )
    if hermit is None:
        counter2.fail(f"--hermit is required for tier {execution.tier}")
    mode = TIER_ARGUMENTS[execution.tier]
    return (
        [
            str(hermit),
            "--log=error",
            "--backend",
            execution.backend,
            "run",
            *mode,
            "--",
            str(artifact),
            *arguments,
        ],
        hermit_environment(execution, root, target),
    )


def validate_outcome(
    execution: Execution, outcome: counter2.Outcome, expected_stdout: bytes | None
) -> None:
    if outcome.returncode != 0:
        raise RuntimeError(
            f"{execution.label} exited {outcome.returncode}\n"
            f"command: {' '.join(outcome.command)}\n"
            f"stdout:\n{outcome.stdout[-4096:].decode(errors='replace')}\n"
            f"stderr:\n{outcome.stderr[-4096:].decode(errors='replace')}"
        )
    if expected_stdout is not None and outcome.stdout != expected_stdout:
        raise RuntimeError(
            f"{execution.label} stdout differs from matching native workload\n"
            f"expected: {expected_stdout!r}\nactual: {outcome.stdout!r}"
        )
    if execution.tier == "counter2" and not outcome.counter_total:
        raise RuntimeError(
            f"{execution.label} did not emit a nonzero exact-counter2 summary\n"
            f"stderr:\n{outcome.stderr[-4096:].decode(errors='replace')}"
        )


def geometric_mean(values: list[float]) -> float:
    return math.exp(math.fsum(math.log(value) for value in values) / len(values))


def calibrate(
    workloads: list[dict[str, Any]],
    artifact: Path,
    target_seconds: float,
    root: Path,
    env: dict[str, str],
) -> dict[str, int]:
    iterations = {}
    for workload in workloads:
        completed = subprocess.run(
            [
                str(artifact),
                "--calibrate-ms",
                str(round(target_seconds * 1000)),
                "--stride",
                str(workload["syscall_stride"]),
            ],
            cwd=root,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=True,
        )
        match = re.fullmatch(r"iterations=([0-9]+)\n", completed.stdout)
        if not match:
            counter2.fail(f"cannot parse calibration output: {completed.stdout!r}")
        iterations[workload["id"]] = int(match.group(1))
        print(
            f"CALIBRATE {workload['id']}: {completed.stdout.strip()} "
            f"({completed.stderr.strip()})",
            flush=True,
        )
    return iterations


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def main() -> None:
    args = parse_args()
    if not 0.1 <= args.target_seconds <= 10.0:
        counter2.fail("--target-seconds must be between 0.1 and 10")
    if args.repetitions < 1 or args.warmups < 0 or args.timeout_seconds <= 0:
        counter2.fail("repetitions/timeout must be positive and warmups nonnegative")

    selected_backends = comma_separated(args.backends, counter2.BACKENDS, "--backends")
    selected_tiers = comma_separated(args.tiers, TIERS, "--tiers")
    root = Path(__file__).resolve().parents[2]
    target = root / "target" / args.profile
    hermit = args.hermit.resolve() if args.hermit else None
    if any(tier != "counter2" for tier in selected_tiers):
        if hermit is None or not hermit.is_file() or not os.access(hermit, os.X_OK):
            counter2.fail("--hermit must name an executable for relaxed/strict tiers")

    manifest_path = Path(__file__).with_name("known-green.json")
    manifest = json.loads(manifest_path.read_text())
    requested = (
        {item.strip() for item in args.workloads.split(",") if item.strip()}
        if args.workloads
        else None
    )
    workloads = [
        workload
        for workload in manifest["workloads"]
        if requested is None or workload["id"] in requested
    ]
    if requested is not None and requested != {workload["id"] for workload in workloads}:
        missing = requested - {workload["id"] for workload in workloads}
        counter2.fail(f"unknown workload(s): {', '.join(sorted(missing))}")
    if not workloads:
        counter2.fail("select at least one workload")

    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = (args.output or root / "target/counter2-shootout-tiered" / run_id).resolve()
    output.mkdir(parents=True, exist_ok=False)
    build = root / "target/counter2-shootout-tiered/workloads"
    artifacts = counter2.compile_workloads(root, build)
    iterations = calibrate(
        workloads,
        artifacts["dynamic"],
        args.target_seconds,
        root,
        counter2.base_environment(),
    )

    native_executions = []
    executions = []
    excluded: dict[tuple[str, str, str, str], str] = {}
    for workload in workloads:
        variants = sorted(
            {workload["backends"][backend] for backend in selected_backends}
        )
        for variant in variants:
            native_executions.append(
                Execution(
                    workload["id"],
                    "native",
                    "native",
                    variant,
                    iterations[workload["id"]],
                    workload["syscall_stride"],
                )
            )
        for tier in selected_tiers:
            for backend in selected_backends:
                execution = Execution(
                    workload["id"],
                    tier,
                    backend,
                    workload["backends"][backend],
                    iterations[workload["id"]],
                    workload["syscall_stride"],
                )
                executions.append(execution)
                if tier == "relaxed" and backend in RELAXED_EXCLUSIONS:
                    excluded[execution.key] = RELAXED_EXCLUSIONS[backend]

    expected: dict[tuple[str, str], bytes] = {}
    active: dict[tuple[str, str, str, str], Execution] = {}
    statuses: dict[tuple[str, str, str, str], dict[str, Any]] = {}
    probes = []
    print("PROBE: validating native and every requested tier/backend cell", flush=True)
    for execution in [*native_executions, *executions]:
        command, env = command_and_environment(execution, artifacts, hermit, root, target)
        if execution.key in excluded:
            reason = excluded[execution.key]
            statuses[execution.key] = {"status": "excluded", "reason": reason}
            probes.append(
                {
                    "workload": execution.workload,
                    "tier": execution.tier,
                    "backend": execution.backend,
                    "variant": execution.variant,
                    "status": "excluded",
                    "reason": reason,
                    "command": command,
                }
            )
            print(f"  EXCLUDED {execution.label}: {reason}", flush=True)
            continue
        try:
            outcome = run_process(command, env, args.timeout_seconds, root)
            key = (execution.workload, execution.variant)
            if execution.tier == "native":
                validate_outcome(execution, outcome, None)
                expected[key] = outcome.stdout
            else:
                validate_outcome(execution, outcome, expected[key])
            active[execution.key] = execution
            statuses[execution.key] = {"status": "available"}
            probes.append(
                {
                    "workload": execution.workload,
                    "tier": execution.tier,
                    "backend": execution.backend,
                    "variant": execution.variant,
                    "status": "pass",
                    "duration_ms": round(outcome.duration_ms, 6),
                    "counter_total": outcome.counter_total,
                    "stdout_sha256": hashlib.sha256(outcome.stdout).hexdigest(),
                    "command": command,
                }
            )
            print(
                f"  PASS {execution.label}: {outcome.duration_ms:.1f} ms "
                f"counter={outcome.counter_total or '-'}",
                flush=True,
            )
        except RuntimeError as error:
            if execution.tier == "native":
                raise
            reason = str(error)
            statuses[execution.key] = {"status": "unavailable", "reason": reason}
            probes.append(
                {
                    "workload": execution.workload,
                    "tier": execution.tier,
                    "backend": execution.backend,
                    "variant": execution.variant,
                    "status": "unavailable",
                    "reason": reason,
                    "command": command,
                }
            )
            print(f"  UNAVAILABLE {execution.label}: {reason.splitlines()[0]}", flush=True)
    with (output / "probes.jsonl").open("w") as stream:
        for probe in probes:
            stream.write(json.dumps(probe, sort_keys=True) + "\n")

    for warmup in range(args.warmups):
        print(f"WARMUP {warmup + 1}/{args.warmups}", flush=True)
        for key, execution in list(active.items()):
            command, env = command_and_environment(execution, artifacts, hermit, root, target)
            try:
                outcome = run_process(command, env, args.timeout_seconds, root)
                validate_outcome(
                    execution, outcome, expected[(execution.workload, execution.variant)]
                )
            except RuntimeError as error:
                if execution.tier == "native":
                    raise
                statuses[key] = {"status": "failed", "reason": str(error)}
                active.pop(key)

    schedule = [
        (repetition, execution)
        for repetition in range(1, args.repetitions + 1)
        for execution in active.values()
    ]
    random.Random(args.seed).shuffle(schedule)
    samples: list[dict[str, Any]] = []
    with (output / "samples.jsonl").open("w") as stream:
        for index, (repetition, execution) in enumerate(schedule, 1):
            if execution.key not in active:
                continue
            command, env = command_and_environment(execution, artifacts, hermit, root, target)
            try:
                outcome = run_process(command, env, args.timeout_seconds, root)
                validate_outcome(
                    execution, outcome, expected[(execution.workload, execution.variant)]
                )
            except RuntimeError as error:
                if execution.tier == "native":
                    raise
                statuses[execution.key] = {"status": "failed", "reason": str(error)}
                active.pop(execution.key)
                print(f"FAILED {execution.label}: {str(error).splitlines()[0]}", flush=True)
                continue
            sample = {
                "run_id": run_id,
                "sequence": index,
                "repetition": repetition,
                "workload": execution.workload,
                "tier": execution.tier,
                "backend": execution.backend,
                "variant": execution.variant,
                "iterations": execution.iterations,
                "syscall_stride": execution.stride,
                "duration_ms": round(outcome.duration_ms, 6),
                "counter_total": outcome.counter_total,
                "counter_processes": outcome.counter_processes,
                "counter_threads": outcome.counter_threads,
                "stdout_sha256": hashlib.sha256(outcome.stdout).hexdigest(),
            }
            samples.append(sample)
            stream.write(json.dumps(sample, sort_keys=True) + "\n")
            stream.flush()
            print(
                f"SAMPLE {index}/{len(schedule)} {execution.label}: "
                f"{outcome.duration_ms:.1f} ms",
                flush=True,
            )

    groups: dict[tuple[str, str, str, str], list[dict[str, Any]]] = {}
    for sample in samples:
        key = (
            sample["workload"],
            sample["tier"],
            sample["backend"],
            sample["variant"],
        )
        groups.setdefault(key, []).append(sample)
    native_medians = {
        (workload, variant): statistics.median(item["duration_ms"] for item in group)
        for (workload, tier, backend, variant), group in groups.items()
        if tier == "native" and len(group) == args.repetitions
    }

    summary_rows = []
    for execution in [*native_executions, *executions]:
        group = groups.get(execution.key, [])
        state = statuses[execution.key]
        if len(group) == args.repetitions:
            durations = [item["duration_ms"] for item in group]
            native_median = native_medians[(execution.workload, execution.variant)]
            counters = sorted(
                {item["counter_total"] for item in group if item["counter_total"] is not None}
            )
            status = "ok"
            median_ms: float | str = round(statistics.median(durations), 3)
            geomean_ms: float | str = round(geometric_mean(durations), 3)
            native_ms: float | str = round(native_median, 3)
            slowdown: float | str = round(statistics.median(durations) / native_median, 6)
            counter_total = ";".join(str(value) for value in counters)
            reason = ""
        else:
            status = str(state["status"])
            median_ms = geomean_ms = native_ms = slowdown = counter_total = ""
            reason = str(state.get("reason", "incomplete sample set")).replace("\n", " | ")
        summary_rows.append(
            {
                "workload": execution.workload,
                "tier": execution.tier,
                "backend": execution.backend,
                "variant": execution.variant,
                "status": status,
                "repetitions": len(group),
                "median_ms": median_ms,
                "geomean_ms": geomean_ms,
                "native_median_ms": native_ms,
                "slowdown": slowdown,
                "counter_total": counter_total,
                "reason": reason,
            }
        )
    write_csv(output / "summary.csv", summary_rows)

    overall_rows = []
    for tier in selected_tiers:
        for backend in selected_backends:
            rows = [
                row
                for row in summary_rows
                if row["tier"] == tier
                and row["backend"] == backend
                and row["status"] == "ok"
            ]
            overall_rows.append(
                {
                    "tier": tier,
                    "backend": backend,
                    "workloads": len(rows),
                    "requested_workloads": len(workloads),
                    "complete": len(rows) == len(workloads),
                    "geomean_slowdown": (
                        round(geometric_mean([float(row["slowdown"]) for row in rows]), 6)
                        if rows
                        else ""
                    ),
                }
            )
    write_csv(output / "overall.csv", overall_rows)

    runner = Path(__file__)
    metadata = {
        "schema": 1,
        "run_id": run_id,
        "run_utc": datetime.now(timezone.utc).isoformat(),
        "host": socket.gethostname(),
        "logical_cpus": os.cpu_count(),
        "load_average_at_completion": os.getloadavg(),
        "reverie_sha": counter2.git_output(root, "rev-parse", "HEAD"),
        "source_dirty": bool(counter2.git_output(root, "status", "--short")),
        "runner_sha256": sha256(runner),
        "manifest_sha256": sha256(manifest_path),
        "workload_source_sha256": sha256(Path(__file__).with_name("workload.c")),
        "hermit_binary": str(hermit) if hermit else None,
        "hermit_binary_sha256": sha256(hermit) if hermit else None,
        "hermit_version": (
            subprocess.check_output([str(hermit), "--version"], text=True).strip()
            if hermit
            else None
        ),
        "backends": selected_backends,
        "tiers": selected_tiers,
        "workloads": [workload["id"] for workload in workloads],
        "target_seconds": args.target_seconds,
        "repetitions": args.repetitions,
        "warmups": args.warmups,
        "timeout_seconds": args.timeout_seconds,
        "seed": args.seed,
        "tier_arguments": {tier: list(arguments) for tier, arguments in TIER_ARGUMENTS.items()},
        "assurance": (
            "performance and per-sample exit/stdout correctness only; "
            "no Hermit --verify determinism level claimed"
        ),
    }
    (output / "metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    )

    lines = [
        "# Counter2 / Hermit tiered shootout",
        "",
        f"Run `{run_id}` on `{metadata['host']}`.",
        f"Each available cell has {args.repetitions} measured repetitions after "
        f"{args.warmups} warmup(s).",
        "Slowdown is median wall time / median matching native-variant wall time.",
        "Relaxed uses `--no-sequentialize-threads --max-timeslice=disabled`; "
        "strict uses `--strict` with its default PMU timeslice.",
        "Samples are exit/stdout correctness-gated performance runs, not `--verify` "
        "determinism evidence.",
        "",
        "| Tier | Backend | Workloads | Complete | Geomean slowdown |",
        "| --- | --- | ---: | --- | ---: |",
    ]
    for row in overall_rows:
        value = (
            f"{float(row['geomean_slowdown']):.3f}x"
            if row["geomean_slowdown"] != ""
            else "n/a"
        )
        lines.append(
            f"| {row['tier']} | {row['backend']} | {row['workloads']}/"
            f"{row['requested_workloads']} | {str(row['complete']).lower()} | {value} |"
        )
    lines.extend(
        [
            "",
            "| Workload | Tier | Backend | Variant | Status | Median ms | Native ms | Slowdown | Counter2 calls | Reason |",
            "| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- |",
        ]
    )
    for row in summary_rows:
        if row["tier"] == "native":
            continue
        slowdown = f"{float(row['slowdown']):.3f}x" if row["slowdown"] != "" else "n/a"
        lines.append(
            f"| {row['workload']} | {row['tier']} | {row['backend']} | "
            f"{row['variant']} | {row['status']} | {row['median_ms'] or 'n/a'} | "
            f"{row['native_median_ms'] or 'n/a'} | {slowdown} | "
            f"{row['counter_total'] or '-'} | {row['reason'] or '-'} |"
        )
    report = "\n".join(lines) + "\n"
    (output / "report.md").write_text(report)
    print(f"\nRESULTS {output}\n")
    print(report)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        counter2.fail(str(error))
