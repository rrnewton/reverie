#!/usr/local/fbcode/platform010/bin/python3.12
"""Measure non-/tmp relocations of the three TFA PASS/FAIL Hermit reproducers.

The historical manifest argv (cwd mount, safehermit report, verify-log dir under
/tmp/tfa-phase2) is never edited. Each relocated argv is executed verbatim, so
the recorded argv is exactly the command that was measured. A relocated
command is only called a reproducer after its classification matches the
target oracle; seed changes stay inside the historical campaign's already
measured seed range and never use the reserved fresh-stress seeds.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from testinfra.tfa.manifest import Case, load_manifest
from testinfra.tfa.pipeline import (
    _gtest_execution_evidence,
    _gtest_fatal_execution_evidence,
    FAILURE_MARKERS,
)

E3 = Path(
    "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/"
    "tfa-phase2-three-case"
)
OUT = E3 / "relocated"
CASES_ROOT = Path("/home/newton/work/tfa/phase2-cases")
MANIFEST = Path("/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/cases.v1.json")
SAFEHERMIT = Path("/home/newton/work/dev-hermit/bin/safehermit")
SAFEHERMIT_SHA256 = "a8a2965efa096001f919759884e18b1f1cffed223b3bfaed561be357bdc0b1d0"
OUTER_TIMEOUT = 240
DETERMINISM = ":: Success: deterministic. Determinism verified."


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def record(path: Path) -> dict[str, object]:
    return {"path": str(path), "sha256": sha256(path), "size": path.stat().st_size}


def inventory(root: Path) -> list[dict[str, object]]:
    if not root.exists():
        return []
    return [
        {"path": str(p.relative_to(root)), "size": p.stat().st_size}
        for p in sorted(root.rglob("*"))
        if p.is_file()
    ]


def historical_seeds(case: Case) -> list[int]:
    evidence = E3 / "authoritative" / case.case_id / "evidence"
    return sorted(
        {
            int(p.name.split("-sched-seed-")[1][:20])
            for p in evidence.glob("selection-*-replica-1.result.json")
        }
    )


def relocated_argv(
    case: Case, role: str, seed: int, cwd: Path, guest_tmp: Path, run_dir: Path
) -> tuple[str, ...]:
    source = case.pass_argv if role == "pass" else case.fail_argv
    old_mount = (
        f"--mount=type=bind,source={case.working_directory},"
        f"target={case.working_directory}"
    )
    boundary = source.index("--")
    argv: list[str] = []
    index = 0
    while index < len(source):
        token = source[index]
        if index < boundary and token == "--sh-report":
            argv += [token, str(run_dir / "safehermit.txt")]
            index += 2
            continue
        if index < boundary and token == "--sched-seed":
            argv += [token, str(seed)]
            index += 2
            continue
        if index < boundary and token.startswith("--verify-log-dir="):
            argv.append(f"--verify-log-dir={run_dir / 'verify'}")
        elif index < boundary and token == old_mount:
            argv.append(f"--mount=type=bind,source={cwd},target={cwd}")
            argv.append(f"--tmp={guest_tmp}")
        else:
            argv.append(token)
        index += 1
    if argv.count(f"--tmp={guest_tmp}") != 1 or len(argv) != len(source) + 1:
        raise RuntimeError("relocation did not replace exactly the /tmp-bound tokens")
    if any("/tmp/" in token or token.endswith("=/tmp") for token in argv):
        raise RuntimeError(f"relocated argv still names /tmp: {argv}")
    if argv[boundary + 1 :] != list(source[boundary:]):
        raise RuntimeError("relocation changed the guest argv")
    return tuple(argv)


def classify(
    case: Case, argv: tuple[str, ...], rc: int, stdout: Path, stderr: Path
) -> dict[str, object]:
    err = stderr.read_text(errors="replace")
    out = stdout.read_text(errors="replace")
    deterministic = DETERMINISM in err.splitlines()
    result: dict[str, object] = {
        "determinism_verified": deterministic,
        "returncode": rc,
    }
    if rc in (124, 125, 126):
        result["oracle"] = "CENSORED"
        result["reason"] = f"safehermit bound fired (rc={rc})"
        return result
    try:
        if rc == 0:
            result["gtest"] = _gtest_execution_evidence(argv, stdout, True)
            result["oracle"] = "PASS" if deterministic else "NOT_DETERMINISTIC"
            return result
        output = out + "\n" + err
        markers = FAILURE_MARKERS[case.case_id]
        if not all(marker in output for marker in markers):
            result["oracle"] = "OTHER_FAILURE"
            result["reason"] = "failure markers absent"
            return result
        if case.case_id == "httpcoro":
            if rc != 134:
                result["oracle"] = "OTHER_FAILURE"
                result["reason"] = "httpcoro fatal witness requires exit 134"
                return result
            result["gtest"] = _gtest_fatal_execution_evidence(
                argv, stdout, stderr, markers
            )
        else:
            result["gtest"] = _gtest_execution_evidence(argv, stdout, False)
        result["oracle"] = "FAIL" if deterministic else "NOT_DETERMINISTIC"
        result["failure_class_id"] = case.failure_class
    except RuntimeError as error:
        result["oracle"] = "OTHER_FAILURE" if rc else "OTHER"
        result["reason"] = str(error)
    return result


def run(case: Case, role: str, seed: int, replica: int) -> dict[str, object]:
    label = f"seed-{seed:03d}-replica-{replica}"
    run_dir = OUT / case.case_id / label
    receipt_path = run_dir / "receipt.json"
    if receipt_path.exists():
        return json.loads(receipt_path.read_text())
    repro_root = CASES_ROOT / case.case_id / "tfa-phase2-repro"
    cwd = repro_root / "cwd"
    guest_tmp = repro_root / "guest-tmp" / label
    run_dir.mkdir(parents=True)
    (run_dir / "verify").mkdir()
    if guest_tmp.exists():
        raise RuntimeError(f"guest tmp already exists: {guest_tmp}")
    if any(cwd.iterdir()):
        raise RuntimeError(f"guest cwd is not empty: {cwd}")
    argv = relocated_argv(case, role, seed, cwd, guest_tmp, run_dir)
    hermit = Path(argv[argv.index("run") - 1])
    guest = Path(argv[argv.index("--") + 1])
    inputs = {"safehermit": record(SAFEHERMIT), "hermit": record(hermit), "guest": record(guest)}
    if (
        inputs["safehermit"]["sha256"] != SAFEHERMIT_SHA256
        or inputs["hermit"]["sha256"] != case.hermit_sha256
        or inputs["guest"]["sha256"] != case.artifact_sha256
    ):
        raise RuntimeError(f"pinned input identity differs: {inputs}")
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in ("RUST_LOG", "HERMIT_BIN", "PYTHONPATH")
    }
    environment["TMPDIR"] = str(E3 / "tmpdir")
    environment["SAFEHERMIT_LOG_ROOT"] = str(OUT / "safehermit-log-root")
    stdout = run_dir / "stdout.txt"
    stderr = run_dir / "stderr.txt"
    started = time.time()
    with stdout.open("xb") as out, stderr.open("xb") as err:
        try:
            completed = subprocess.run(
                argv,
                cwd=cwd,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=err,
                timeout=OUTER_TIMEOUT,
                check=False,
            )
            rc: int | None = completed.returncode
        except subprocess.TimeoutExpired:
            rc = None
    finished = time.time()
    cwd_after = [str(p) for p in cwd.iterdir()]
    classification = (
        {"oracle": "CENSORED", "reason": "outer timeout", "returncode": None}
        if rc is None
        else classify(case, argv, rc, stdout, stderr)
    )
    receipt = {
        "argv": list(argv),
        "argv_sha256": hashlib.sha256("\0".join(argv).encode()).hexdigest(),
        "case_id": case.case_id,
        "classification": classification,
        "cwd": str(cwd),
        "cwd_entries_after": cwd_after,
        "duration_seconds": finished - started,
        "environment": {
            "SAFEHERMIT_LOG_ROOT": environment["SAFEHERMIT_LOG_ROOT"],
            "TMPDIR": environment["TMPDIR"],
            "removed": ["HERMIT_BIN", "PYTHONPATH", "RUST_LOG"],
        },
        "finished_unix": finished,
        "guest_tmp": str(guest_tmp),
        "guest_tmp_inventory": inventory(guest_tmp),
        "inputs": inputs,
        "label": label,
        "replica": replica,
        "intended_role": role,
        "safehermit_report": record(run_dir / "safehermit.txt")
        if (run_dir / "safehermit.txt").exists()
        else None,
        "schema": "tfa.relocated-reproducer-run.v1",
        "seed": seed,
        "started_unix": started,
        "stderr": record(stderr),
        "stdout": record(stdout),
        "verify_inventory": inventory(run_dir / "verify"),
    }
    receipt_path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    print(
        f"{case.case_id} {label} rc={rc} oracle={classification['oracle']} "
        f"{finished - started:.1f}s",
        flush=True,
    )
    return receipt


def measure(case: Case) -> dict[str, object]:
    measured = historical_seeds(case)
    order = [case.fail_seed, case.pass_seed, case.historical_fail_seed, case.historical_pass_seed]
    order += [seed for seed in range(max(measured) + 1)]
    order = [
        seed
        for index, seed in enumerate(order)
        if seed not in order[:index] and seed not in case.fresh_stress_seeds
    ]
    discovery: list[dict[str, object]] = []
    witness: dict[str, int] = {}
    for seed in order:
        if len(witness) == 2:
            break
        role = "fail" if seed in (case.fail_seed, case.historical_fail_seed) else "pass"
        receipt = run(case, role, seed, 1)
        discovery.append(receipt)
        oracle = receipt["classification"]["oracle"]
        if oracle in ("PASS", "FAIL") and oracle.lower() not in witness:
            witness[oracle.lower()] = seed
    confirmations = {
        role: run(case, role, seed, 2)
        for role, seed in witness.items()
    }
    summary: dict[str, object] = {
        "case_id": case.case_id,
        "search_order": order,
        "search_bound": {
            "historical_campaign_seed_range": [0, max(measured)],
            "historical_campaign_measured_seeds": measured,
            "excluded_fresh_stress_seeds": list(case.fresh_stress_seeds),
        },
        "manifest_seeds": {"pass": case.pass_seed, "fail": case.fail_seed},
        "historical_seeds": {
            "pass": case.historical_pass_seed,
            "fail": case.historical_fail_seed,
        },
        "discovery": [
            {
                "label": r["label"],
                "seed": r["seed"],
                "oracle": r["classification"]["oracle"],
                "returncode": r["classification"]["returncode"],
                "receipt": str(OUT / case.case_id / r["label"] / "receipt.json"),
            }
            for r in discovery
        ],
        "witness": {},
    }
    for role in ("pass", "fail"):
        if role not in witness:
            summary["witness"][role] = {"state": "NOT_WITNESSED_WITHIN_BOUND"}
            continue
        first = next(r for r in discovery if r["seed"] == witness[role])
        second = confirmations[role]
        confirmed = second["classification"]["oracle"] == role.upper()
        summary["witness"][role] = {
            "state": "REPRODUCED" if confirmed else "UNCONFIRMED",
            "seed": witness[role],
            "seed_changed_from_manifest": witness[role]
            != (case.pass_seed if role == "pass" else case.fail_seed),
            "argv": first["argv"],
            "cwd": first["cwd"],
            "replicas": [first["label"], second["label"]],
            "stdout_identical_across_replicas": first["stdout"]["sha256"]
            == second["stdout"]["sha256"],
            "confirmation_oracle": second["classification"]["oracle"],
        }
    path = OUT / case.case_id / "summary.json"
    path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    return summary


def main() -> int:
    manifest = load_manifest(MANIFEST)
    selected = sys.argv[1:] or ["thrift", "asyncssl", "httpcoro"]
    for case_id in selected:
        case = manifest.case(case_id)
        repro_root = CASES_ROOT / case_id / "tfa-phase2-repro"
        repro_root.mkdir(exist_ok=True)
        ignore = repro_root / ".gitignore"
        if not ignore.exists():
            ignore.write_text("*\n")
        (repro_root / "cwd").mkdir(exist_ok=True)
        (repro_root / "guest-tmp").mkdir(exist_ok=True)
        summary = measure(case)
        print(json.dumps(summary["witness"], sort_keys=True)[:2000], flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
