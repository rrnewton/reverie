#!/usr/local/fbcode/platform010/bin/python3.12
"""Measure non-/tmp relocations with one newly created, owned cwd per attempt.

v1 (../relocated) shared one cwd per case; its httpcoro seed-0 attempt was
CENSORED at the 120 s safehermit bound and left a Hermit verify-summary file
in that shared cwd, so later attempts refused to start. v1 receipts and that
cwd are preserved untouched. Here every attempt gets
phase2-cases/<case>/tfa-phase2-repro/attempts/<label>/cwd, created exclusively
by this process and checked to be owned by this uid and empty before launch.

Search population is unchanged from v1: manifest and historical seeds, then
the historical campaign's measured seed range, never the fresh-stress seeds.
CENSORED attempts are never counted as PASS or FAIL. A seed may be re-run only
to resolve a CENSORED attempt or to confirm a witness, at most
MAX_ATTEMPTS_PER_SEED times, with the same argv shape and the same bounds.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from run_relocated import (
    CASES_ROOT,
    classify,
    E3,
    historical_seeds,
    inventory,
    MANIFEST,
    OUTER_TIMEOUT,
    record,
    relocated_argv,
    SAFEHERMIT,
    SAFEHERMIT_SHA256,
)
from testinfra.tfa.manifest import Case, load_manifest

OUT = E3 / "relocated-v2"
MAX_ATTEMPTS_PER_SEED = 4
DECIDED = ("PASS", "FAIL")


def owned_empty_dir(path: Path) -> dict[str, object]:
    path.mkdir()
    status = path.stat()
    if status.st_uid != os.getuid() or any(path.iterdir()):
        raise RuntimeError(f"attempt cwd is not an owned empty directory: {path}")
    return {"path": str(path), "st_dev": status.st_dev, "st_ino": status.st_ino}


def detailed_inventory(root: Path) -> list[dict[str, object]]:
    return [
        {
            "path": str(p.relative_to(root)),
            "size": p.stat().st_size,
            "mtime_ns": p.stat().st_mtime_ns,
            "sha256": hashlib.sha256(p.read_bytes()).hexdigest(),
        }
        for p in sorted(root.rglob("*"))
        if p.is_file()
    ]


def run(case: Case, role: str, seed: int, attempt: int) -> dict[str, object]:
    label = f"seed-{seed:03d}-attempt-{attempt}"
    run_dir = OUT / case.case_id / label
    receipt_path = run_dir / "receipt.json"
    if receipt_path.exists():
        return json.loads(receipt_path.read_text())
    attempt_root = CASES_ROOT / case.case_id / "tfa-phase2-repro" / "attempts" / label
    run_dir.mkdir(parents=True)
    (run_dir / "verify").mkdir()
    attempt_root.mkdir()
    cwd_identity = owned_empty_dir(attempt_root / "cwd")
    cwd = attempt_root / "cwd"
    guest_tmp = attempt_root / "guest-tmp"
    argv = relocated_argv(case, role, seed, cwd, guest_tmp, run_dir)
    hermit = Path(argv[argv.index("run") - 1])
    guest = Path(argv[argv.index("--") + 1])
    inputs = {
        "safehermit": record(SAFEHERMIT),
        "hermit": record(hermit),
        "guest": record(guest),
    }
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
    classification = (
        {"oracle": "CENSORED", "reason": "outer timeout", "returncode": None}
        if rc is None
        else classify(case, argv, rc, stdout, stderr)
    )
    receipt = {
        "argv": list(argv),
        "argv_sha256": hashlib.sha256("\0".join(argv).encode()).hexdigest(),
        "attempt": attempt,
        "case_id": case.case_id,
        "classification": classification,
        "cwd": str(cwd),
        "cwd_identity_before": cwd_identity,
        "cwd_inventory_after": detailed_inventory(cwd),
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
        "intended_role": role,
        "label": label,
        "safehermit_report": record(run_dir / "safehermit.txt")
        if (run_dir / "safehermit.txt").exists()
        else None,
        "schema": "tfa.relocated-reproducer-run.v2",
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


def oracle(receipt: dict[str, object]) -> str:
    return receipt["classification"]["oracle"]


def measure(case: Case) -> dict[str, object]:
    measured = historical_seeds(case)
    order = [
        case.fail_seed,
        case.pass_seed,
        case.historical_fail_seed,
        case.historical_pass_seed,
    ]
    order += list(range(max(measured) + 1))
    order = [
        seed
        for index, seed in enumerate(order)
        if seed not in order[:index] and seed not in case.fresh_stress_seeds
    ]
    attempts: dict[int, list[dict[str, object]]] = {}
    witness: dict[str, int] = {}

    def attempt(seed: int) -> dict[str, object]:
        role = "fail" if seed in (case.fail_seed, case.historical_fail_seed) else "pass"
        receipt = run(case, role, seed, len(attempts.setdefault(seed, [])) + 1)
        attempts[seed].append(receipt)
        return receipt

    for seed in order:
        if len(witness) == 2:
            break
        while len(attempts.get(seed, [])) < 2:
            if oracle(attempt(seed)) != "CENSORED":
                break
        decided = [oracle(r) for r in attempts[seed] if oracle(r) != "CENSORED"]
        if decided and decided[0] in DECIDED and decided[0].lower() not in witness:
            witness[decided[0].lower()] = seed
    for seed in witness.values():
        while (
            len([r for r in attempts[seed] if oracle(r) != "CENSORED"]) < 2
            and len(attempts[seed]) < MAX_ATTEMPTS_PER_SEED
        ):
            attempt(seed)
    summary: dict[str, object] = {
        "case_id": case.case_id,
        "schema": "tfa.relocated-reproducer-summary.v2",
        "search_order": order,
        "search_bound": {
            "historical_campaign_seed_range": [0, max(measured)],
            "historical_campaign_measured_seeds": measured,
            "excluded_fresh_stress_seeds": list(case.fresh_stress_seeds),
            "max_attempts_per_seed": MAX_ATTEMPTS_PER_SEED,
            "censored_counts_as": "neither PASS nor FAIL",
        },
        "manifest_seeds": {"pass": case.pass_seed, "fail": case.fail_seed},
        "historical_seeds": {
            "pass": case.historical_pass_seed,
            "fail": case.historical_fail_seed,
        },
        "attempts": [
            {
                "label": r["label"],
                "seed": r["seed"],
                "oracle": oracle(r),
                "returncode": r["classification"]["returncode"],
                "duration_seconds": r["duration_seconds"],
                "cwd_left_nonempty": bool(r["cwd_inventory_after"]),
                "receipt": str(OUT / case.case_id / r["label"] / "receipt.json"),
            }
            for seed in order
            for r in attempts.get(seed, [])
        ],
        "witness": {},
    }
    for role in ("pass", "fail"):
        if role not in witness:
            summary["witness"][role] = {"state": "NOT_WITNESSED_WITHIN_BOUND"}
            continue
        seed = witness[role]
        decided = [r for r in attempts[seed] if oracle(r) != "CENSORED"]
        first = decided[0]
        agree = len(decided) >= 2 and all(oracle(r) == role.upper() for r in decided)
        summary["witness"][role] = {
            "state": "REPRODUCED" if agree else "UNCONFIRMED",
            "seed": seed,
            "seed_changed_from_manifest": seed
            != (case.pass_seed if role == "pass" else case.fail_seed),
            "argv": first["argv"],
            "cwd": first["cwd"],
            "decided_attempts": [r["label"] for r in decided],
            "censored_attempts": [
                r["label"] for r in attempts[seed] if oracle(r) == "CENSORED"
            ],
            "stdout_identical_across_decided_attempts": len(
                {r["stdout"]["sha256"] for r in decided}
            )
            == 1,
        }
    path = OUT / case.case_id / "summary.json"
    path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    return summary


def main() -> int:
    manifest = load_manifest(MANIFEST)
    selected = sys.argv[1:] or ["thrift", "asyncssl", "httpcoro"]
    for case_id in selected:
        case = manifest.case(case_id)
        (CASES_ROOT / case_id / "tfa-phase2-repro" / "attempts").mkdir(exist_ok=True)
        summary = measure(case)
        print(
            json.dumps(
                {
                    role: {k: v for k, v in value.items() if k != "argv"}
                    for role, value in summary["witness"].items()
                },
                sort_keys=True,
            ),
            flush=True,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
