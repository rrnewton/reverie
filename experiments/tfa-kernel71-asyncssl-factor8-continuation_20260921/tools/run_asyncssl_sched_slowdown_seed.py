#!/usr/bin/env python3
"""Run exactly the next sealed AsyncSSL sched-slowdown sched-seed tuple.

This launcher writes only raw, ignored runtime material.  It deliberately does
not classify or admit evidence; ``admit_asyncssl_sched_slowdown_seed.py`` does that after
the invocation reaches a clean boundary.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import socket
import subprocess
import time
from pathlib import Path
from typing import Any

from create_asyncssl_sched_slowdown_plan import (
    HELPER_REL_PATHS,
    HERMIT_BINARY,
    HERMIT_SHA256,
    SCRATCH_ROOT,
    TEST_BINARY,
    TEST_BYTES,
    TEST_SHA256,
    VERIFY_DIR,
)


ROOT = Path(__file__).resolve().parent.parent
REPO_ROOT = ROOT.parents[1]
RECORD_REL = Path(
    "services/folly/async-ssl-socket-test--cefc173a09b4/experiment.json"
)
RECORD = ROOT / RECORD_REL
SAFEHERMIT = ROOT / "bin/safehermit"
SAFEHERMIT_LOG_ROOT = Path(
    "/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit"
)
BOUNDED_RUN_SPACE = REPO_ROOT / "scripts/bounded-run-space"
EXPECTED_HOSTNAME = "devbig030.atn3.facebook.com"
EXPECTED_KERNEL = "7.1.3-0_fbk0_0_g295ad0959f34"
EXPECTED_MACHINE = "x86_64"
EXPECTED_CPU = "AMD EPYC 9D85 158-Core Processor"
EXPECTED_PMU_TYPE = "4"
EXPECTED_PERF_PARANOID = "1"
QGROUP_LIMIT = 8_000_000_000
STDOUT_LIMIT = 67_108_864
STDOUT_CHUNK = 65_536
STDOUT_CAP_METHOD = "constant-memory-drain-and-systemd-cgroup-kill"


class Refusal(RuntimeError):
    """A prospective-run invariant was not satisfied."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: object) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    )
    temporary.replace(path)


def git_output(*args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(REPO_ROOT), *args],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise Refusal(
            f"git {' '.join(args)} failed ({result.returncode}): "
            f"{result.stderr.strip()}"
        )
    return result.stdout


def assert_clean_committed_record(record_path: Path = RECORD) -> None:
    status = git_output("status", "--porcelain=v1", "--untracked-files=all")
    if status:
        raise Refusal("worktree is not clean; commit the exact boundary before a run")
    repository_path = record_path.relative_to(REPO_ROOT).as_posix()
    committed = git_output("show", f"HEAD:{repository_path}").encode()
    if committed != record_path.read_bytes():
        raise Refusal("experiment record is not byte-identical to committed HEAD")


def helper_blobs_at(commit: str) -> list[dict[str, str]]:
    blobs: list[dict[str, str]] = []
    for path in HELPER_REL_PATHS:
        blob = git_output("rev-parse", f"{commit}:{path}").strip()
        if not re.fullmatch(r"[0-9a-f]{40}", blob):
            raise Refusal(f"could not resolve sealed helper blob: {path}")
        blobs.append({"path": path, "blob": blob})
    return blobs


def assert_live_helper_blobs(receipt: dict[str, Any]) -> None:
    expected = receipt.get("helper_blobs")
    if not isinstance(expected, list) or expected != sorted(
        expected, key=lambda item: item.get("path", "")
    ):
        raise Refusal("plan receipt lacks the sorted sealed helper blob identities")
    if expected != helper_blobs_at(receipt["commit"]):
        raise Refusal("plan receipt helper blobs differ from the plan commit")
    if expected != helper_blobs_at("HEAD"):
        raise Refusal("execution helper blobs drifted from the plan commit")


def execution_boundary(record: dict[str, Any]) -> dict[str, Any]:
    """Bind the prepared invocation to the exact committed pre-run boundary."""

    repository_path = RECORD.relative_to(REPO_ROOT).as_posix()
    head_commit = git_output("rev-parse", "HEAD").strip()
    record_blob = git_output("rev-parse", f"HEAD:{repository_path}").strip()
    if record["attempt_state"] == "PLANNED":
        receipt = {
            "commit": head_commit,
            "path": RECORD_REL.as_posix(),
            "blob": record_blob,
            "helper_blobs": helper_blobs_at(head_commit),
        }
    else:
        receipt = record["plan_receipt"]
    if not isinstance(receipt, dict):
        raise Refusal("pre-run boundary has no prospective/immutable plan receipt")
    assert_live_helper_blobs(receipt)
    route = record["plan"]["authorized_routes"][0]
    return {
        "head_commit": head_commit,
        "record_repository_path": repository_path,
        "record_blob": record_blob,
        "plan_sha256": record["plan"]["plan_sha256"],
        "route_id": route["route_id"],
        "route_sha256": route["route_sha256"],
        "plan_receipt": copy_json(receipt),
    }


def copy_json(value: Any) -> Any:
    """Copy a JSON value without importing mutable implementation details."""

    return json.loads(json.dumps(value))


def next_seed(record: dict[str, Any]) -> tuple[int, int]:
    plan = record["plan"]
    design = plan["measurement_design"]
    order = design["selection_order"]["seed_values"]
    population = design["seed_population"]["values"]
    if order != population or len(order) != len(set(order)):
        raise Refusal("sealed sched-seed population/order is inconsistent")
    samples = record["samples"]
    observed = [sample["seed"]["value"] for sample in samples]
    indexes = [sample["selection_index"] for sample in samples]
    flags = [sample["seed"]["flag"] for sample in samples]
    if observed != order[: len(observed)] or indexes != list(range(len(samples))):
        raise Refusal("material samples are not the exact sealed prefix")
    if any(flag != "--sched-seed" for flag in flags):
        raise Refusal("material sample varies a coordinate other than --sched-seed")
    if record["attempt_state"] == "PLANNED":
        if samples or record["current_outcome"] is not None:
            raise Refusal("PLANNED record already contains material")
    elif record["attempt_state"] == "MATERIAL":
        current = record["current_outcome"]
        if not isinstance(current, dict) or current.get("kind") != "NOT-DETERMINABLE":
            raise Refusal("record is terminal or does not have a resumable outcome")
        if current.get("measurement_budget_exhausted"):
            raise Refusal("sealed measurement budget is exhausted")
    else:
        raise Refusal(f"unsupported attempt_state {record['attempt_state']!r}")
    cap = design["max_attempted_seed_tuples"]
    if len(samples) >= cap or len(samples) >= len(order):
        raise Refusal("no scheduler seed remains in the sealed prefix")
    return len(samples), order[len(samples)]


def materialize_argv(record: dict[str, Any], seed: int, report: Path) -> list[str]:
    route = record["plan"]["authorized_routes"][0]
    argv = list(route["launch_argv"])
    if argv[0] != "bin/safehermit":
        raise Refusal("sealed route does not invoke the campaign bin/safehermit")
    if argv.count("{SWEEP}") != 1 or argv.count("{SAFEHERMIT_REPORT}") != 1:
        raise Refusal("sealed route placeholders are not singular")
    argv[argv.index("{SWEEP}")] = str(seed)
    argv[argv.index("{SAFEHERMIT_REPORT}")] = str(report)
    if argv[0] != SAFEHERMIT.relative_to(ROOT).as_posix():
        raise Refusal("material command would bypass campaign bin/safehermit")
    return argv


def evidence_report_locator(selection_index: int, seed: int, replica: int = 1) -> Path:
    prefix = f"selection-{selection_index:03d}-sched-seed-{seed:020d}"
    return RECORD_REL.parent / "evidence" / f"{prefix}-replica-{replica}.safehermit.txt"


def parse_qgroup_report(text: str) -> tuple[int, int, int]:
    match = re.fullmatch(
        r"referenced=([0-9]+) exclusive=([0-9]+) limit=([0-9]+)\n?", text
    )
    if match is None:
        raise Refusal(f"unparseable bounded-run-space report: {text!r}")
    referenced, exclusive, limit = map(int, match.groups())
    if limit != QGROUP_LIMIT or referenced >= limit or exclusive > referenced:
        raise Refusal(
            "qgroup is not the expected live 8,000,000,000-byte bounded space"
        )
    return referenced, exclusive, limit


def qgroup_report(path: Path) -> dict[str, object]:
    result = subprocess.run(
        [str(BOUNDED_RUN_SPACE), "report", str(path)],
        check=False,
        capture_output=True,
        text=True,
    )
    report = {
        "path": str(path),
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }
    if result.returncode != 0:
        raise Refusal(
            f"bounded-run-space report refused {path} ({result.returncode}): "
            f"{result.stderr.strip()}"
        )
    parse_qgroup_report(result.stdout)
    return report


def report_fields(path: Path) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line in path.read_text(errors="strict").splitlines():
        if not line.startswith("safehermit: ") or "=" not in line:
            continue
        key, value = line[len("safehermit: ") :].split("=", 1)
        if key in fields:
            raise Refusal(f"safehermit report duplicates field {key!r}")
        fields[key] = value
    return fields


def kill_stdout_overflow(
    report_path: Path, process: subprocess.Popen[bytes]
) -> dict[str, object]:
    """Kill the already-announced safehermit transient unit at stdout overflow."""

    unit: str | None = None
    for _ in range(100):
        try:
            candidate = report_fields(report_path).get("unit")
        except (OSError, UnicodeError, Refusal):
            candidate = None
        if candidate and candidate != "none" and candidate.startswith("safehermit-"):
            unit = candidate
            break
        if process.poll() is not None:
            break
        time.sleep(0.01)
    if unit is None:
        return {
            "unit": None,
            "command": None,
            "returncode": None,
            "error": "safehermit report did not expose a bounded transient unit",
        }
    command = ["systemctl", "--user", "kill", "--signal=SIGKILL", unit]
    completed = subprocess.run(
        command,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return {
        "unit": unit,
        "command": command,
        "returncode": completed.returncode,
        "error": None if completed.returncode == 0 else "systemd cgroup kill failed",
    }


def capture_stdout(
    stream: Any,
    destination: Path,
    report_path: Path,
    process: subprocess.Popen[bytes],
    *,
    limit: int = STDOUT_LIMIT,
) -> dict[str, object]:
    """Drain stdout in constant memory, retaining at most ``limit`` bytes."""

    observed = 0
    written = 0
    kill = {"unit": None, "command": None, "returncode": None, "error": None}
    truncated = False
    with destination.open("xb") as output:
        while True:
            chunk = stream.read(STDOUT_CHUNK)
            if not chunk:
                break
            observed += len(chunk)
            room = limit - written
            if room > 0:
                kept = chunk[:room]
                output.write(kept)
                written += len(kept)
            if observed > limit and not truncated:
                truncated = True
                output.flush()
                kill = kill_stdout_overflow(report_path, process)
        output.flush()
    return {
        "method": STDOUT_CAP_METHOD,
        "limit_bytes": limit,
        "chunk_bytes": STDOUT_CHUNK,
        "observed_bytes": observed,
        "written_bytes": written,
        "truncated": truncated,
        "kill": kill,
    }


def cpu_model() -> str:
    for line in Path("/proc/cpuinfo").read_text().splitlines():
        if line.startswith("model name"):
            return line.split(":", 1)[1].strip()
    raise Refusal("/proc/cpuinfo has no model name")


def assert_preflight(record: dict[str, Any]) -> dict[str, object]:
    if record["plan"]["resource_bounds"].get("stdout_bytes") != STDOUT_LIMIT:
        raise Refusal("sealed stdout resource bound differs from the live cap")
    if Path(record["plan"]["artifact_policy"]["scratch_root"]).resolve() != Path(
        SCRATCH_ROOT
    ).resolve():
        raise Refusal("sealed scratch root differs from the campaign namespace")
    if not SAFEHERMIT.is_file() or SAFEHERMIT.is_symlink() or not os.access(
        SAFEHERMIT, os.X_OK
    ):
        raise Refusal("campaign bin/safehermit is not a regular executable file")
    binary_checks = []
    for label, path_text, expected_bytes, expected_sha in (
        ("Hermit", HERMIT_BINARY, 415_330_552, HERMIT_SHA256),
        ("test", TEST_BINARY, TEST_BYTES, TEST_SHA256),
    ):
        path = Path(path_text)
        if not path.is_file() or path.stat().st_size != expected_bytes:
            raise Refusal(f"{label} binary missing or size drifted: {path}")
        observed_sha = sha256_file(path)
        if observed_sha != expected_sha:
            raise Refusal(f"{label} binary SHA-256 drifted: {path}")
        binary_checks.append(
            {"label": label, "path": str(path), "bytes": expected_bytes, "sha256": observed_sha}
        )

    host = {
        "hostname": socket.getfqdn(),
        "system": platform.system(),
        "kernel_release": platform.release(),
        "machine": platform.machine(),
        "cpu": cpu_model(),
        "pmu_type": Path("/sys/bus/event_source/devices/cpu/type").read_text().strip(),
        "perf_event_paranoid": Path("/proc/sys/kernel/perf_event_paranoid").read_text().strip(),
    }
    expected = {
        "hostname": EXPECTED_HOSTNAME,
        "system": "Linux",
        "kernel_release": EXPECTED_KERNEL,
        "machine": EXPECTED_MACHINE,
        "cpu": EXPECTED_CPU,
        "pmu_type": EXPECTED_PMU_TYPE,
        "perf_event_paranoid": EXPECTED_PERF_PARANOID,
    }
    if host != expected:
        raise Refusal(f"host facts drifted: observed={host!r}, expected={expected!r}")
    mount = subprocess.run(
        ["findmnt", "--noheadings", "--output", "SOURCE,FSTYPE", "--target", VERIFY_DIR],
        check=False,
        capture_output=True,
        text=True,
    )
    if mount.returncode != 0 or mount.stdout.split() != ["/dev/loop1", "btrfs"]:
        raise Refusal(f"verifier path is not on /dev/loop1 btrfs: {mount.stdout!r}")
    qgroup = qgroup_report(Path(VERIFY_DIR))
    return {"binaries": binary_checks, "host": host, "mount": mount.stdout.strip(), "qgroup": qgroup}


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--seed",
        type=int,
        help="optional assertion of the next predeclared scheduler seed",
    )
    args = parser.parse_args()
    try:
        record = json.loads(RECORD.read_text())
        assert_clean_committed_record()
        selection_index, seed = next_seed(record)
        if args.seed is not None and args.seed != seed:
            raise Refusal(
                f"requested seed {args.seed} skips the next sealed seed {seed}"
            )
        preflight = assert_preflight(record)
        boundary = execution_boundary(record)

        scratch = Path(SCRATCH_ROOT)
        scratch.mkdir(parents=True, exist_ok=True)
        if scratch.resolve() != Path(SCRATCH_ROOT).resolve():
            raise Refusal("scratch root resolution changed")
        run_dir = scratch / f"selection-{selection_index:03d}-sched-seed-{seed:020d}"
        try:
            run_dir.mkdir(mode=0o700)
        except FileExistsError as error:
            raise Refusal(
                f"raw run boundary already exists and cannot be overwritten: {run_dir}"
            ) from error

        stdout_path = run_dir / "stdout.txt"
        stderr_path = run_dir / "stderr.txt"
        report_locator = evidence_report_locator(selection_index, seed)
        report_path = ROOT / report_locator
        report_path.parent.mkdir(parents=True, exist_ok=True)
        if report_path.exists():
            raise Refusal(
                f"actual replica-1 safehermit report already exists: {report_path}"
            )
        verify_before_path = run_dir / "verifier-qgroup-before.txt"
        verify_after_path = run_dir / "verifier-qgroup-after.txt"
        safehermit_qgroup_path = run_dir / "safehermit-qgroup-after.txt"
        manifest_path = run_dir / "invocation.json"
        verify_before = preflight["qgroup"]
        assert isinstance(verify_before, dict)
        verify_before_path.write_text(str(verify_before["stdout"]))
        argv = materialize_argv(record, seed, report_locator)
        environment = os.environ.copy()
        environment["SAFEHERMIT_LOG_ROOT"] = str(SAFEHERMIT_LOG_ROOT)
        environment.pop("RUST_LOG", None)
        environment.pop("HERMIT_BIN", None)
        started_at = utc_now()
        manifest: dict[str, object] = {
            "schema_version": 1,
            "state": "PREPARED",
            "selection_index": selection_index,
            "seed": {"flag": "--sched-seed", "value": seed},
            "argv": argv,
            "cwd": str(ROOT),
            "host_environment": {
                "SAFEHERMIT_LOG_ROOT": str(SAFEHERMIT_LOG_ROOT),
                "RUST_LOG": None,
                "HERMIT_BIN": None,
            },
            "stdout_cap_policy": {
                "method": STDOUT_CAP_METHOD,
                "limit_bytes": STDOUT_LIMIT,
                "chunk_bytes": STDOUT_CHUNK,
                "overflow_action": "systemctl --user kill --signal=SIGKILL UNIT",
            },
            "started_at": started_at,
            "execution_boundary": boundary,
            "preflight": preflight,
            "paths": {
                "stdout": str(stdout_path),
                "stderr": str(stderr_path),
                "safehermit_report": str(report_path),
                "verifier_qgroup_before": str(verify_before_path),
                "verifier_qgroup_after": str(verify_after_path),
                "safehermit_qgroup_after": str(safehermit_qgroup_path),
            },
        }
        write_json(manifest_path, manifest)
        with stderr_path.open("xb") as stderr_handle:
            process = subprocess.Popen(
                argv,
                cwd=ROOT,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=stderr_handle,
            )
            if process.stdout is None:
                raise Refusal("could not capture safehermit stdout")
            stdout_cap = capture_stdout(
                process.stdout, stdout_path, report_path, process
            )
            process.stdout.close()
            process_returncode = process.wait()

        post_errors: list[str] = []
        if stdout_cap["truncated"] and stdout_cap["kill"]["returncode"] != 0:
            post_errors.append(f"stdout cap: {stdout_cap['kill']['error']}")
        verify_after: dict[str, object] | None = None
        safehermit_qgroup: dict[str, object] | None = None
        try:
            verify_after = qgroup_report(Path(VERIFY_DIR))
            verify_after_path.write_text(str(verify_after["stdout"]))
        except (OSError, Refusal) as error:
            post_errors.append(f"verifier qgroup: {error}")
        try:
            fields = report_fields(report_path)
            log_dir = Path(fields["log_dir"])
            if log_dir.parent != SAFEHERMIT_LOG_ROOT:
                raise Refusal("safehermit log_dir escaped SAFEHERMIT_LOG_ROOT")
            safehermit_qgroup = qgroup_report(log_dir)
            safehermit_qgroup_path.write_text(str(safehermit_qgroup["stdout"]))
        except (KeyError, OSError, UnicodeError, Refusal) as error:
            post_errors.append(f"safehermit qgroup: {error}")
        manifest.update(
            state="COMPLETE" if not post_errors else "POSTCHECK_FAILED",
            finished_at=utc_now(),
            process_returncode=process_returncode,
            stdout_cap=stdout_cap,
            verifier_qgroup_after=verify_after,
            safehermit_qgroup_after=safehermit_qgroup,
            postcheck_errors=post_errors,
        )
        write_json(manifest_path, manifest)
        print(run_dir)
        if post_errors:
            raise Refusal("; ".join(post_errors))
        return 0
    except (OSError, UnicodeError, json.JSONDecodeError, Refusal) as error:
        parser.exit(2, f"REFUSED: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
