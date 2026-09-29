#!/usr/local/fbcode/platform010/bin/python3.12
"""Replay a measured relocated-v2 witness argv into a new, owned attempt directory.

The template is the REPRODUCED witness argv in relocated-v2/<case>/summary.json.
Only these tokens are rewritten, and every other token must stay identical:
  --sh-report, --verify-log-dir=, the cwd same-path mount, --tmp= (attempt paths)
  --sched-seed (only when --seed is given)
  the guest ELF and the buck-out same-path mount (only when --elf is given)
Path/argv changes can change virtual scheduling, so every replay is a new
measurement: its verdict comes from its own bytes, never from the template's.
CENSORED (bound fired or outer timeout) is never counted as PASS or FAIL.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from run_relocated import (
    classify,
    E3,
    inventory,
    MANIFEST,
    OUTER_TIMEOUT,
    record,
    SAFEHERMIT,
    SAFEHERMIT_SHA256,
)
from testinfra.tfa.manifest import load_manifest

FBSOURCE_BUCK_OUT = "/home/newton/work/tfa/fbsource/buck-out"
STRESS_CHECKOUTS = Path("/home/newton/work/tfa/phase2-cases/stress")


def allowed_out_roots(case_id: str) -> list[Path]:
    return [
        E3 / "investigation" / case_id / "agent-output" / "witness",
        E3 / "stress" / case_id,
    ]


def template(case_id: str, role: str) -> tuple[dict[str, object], list[str]]:
    summary_path = E3 / "relocated-v2" / case_id / "summary.json"
    summary = json.loads(summary_path.read_text())
    witness = summary["witness"][role]
    if witness["state"] != "REPRODUCED":
        raise SystemExit(f"{case_id} {role} witness is {witness['state']}")
    return summary, list(witness["argv"])


def value_of(argv: list[str], flag: str) -> str:
    return argv[argv.index(flag) + 1]


def attempt_tokens(argv: list[str]) -> dict[int, str]:
    boundary = argv.index("--")
    report = argv.index("--sh-report") + 1
    tokens = {report: "report"}
    for index, token in enumerate(argv[:boundary]):
        if token.startswith("--verify-log-dir="):
            tokens[index] = "verify"
        elif token.startswith("--tmp="):
            tokens[index] = "tmp"
        elif token.startswith("--mount=") and "/tfa-phase2-repro/attempts/" in token:
            tokens[index] = "cwd"
    if sorted(tokens.values()) != ["cwd", "report", "tmp", "verify"]:
        raise SystemExit(f"template attempt tokens are ambiguous: {tokens}")
    return tokens


def rewrite(
    argv: list[str],
    run_dir: Path,
    cwd: Path,
    guest_tmp: Path,
    seed: int | None,
    elf: Path | None,
    buck_out: Path | None,
) -> tuple[list[str], list[dict[str, object]]]:
    boundary = argv.index("--")
    replacements = {
        "report": str(run_dir / "safehermit.txt"),
        "verify": f"--verify-log-dir={run_dir / 'verify'}",
        "tmp": f"--tmp={guest_tmp}",
        "cwd": f"--mount=type=bind,source={cwd},target={cwd}",
    }
    new = list(argv)
    for index, role in attempt_tokens(argv).items():
        new[index] = replacements[role]
    if seed is not None:
        new[argv.index("--sched-seed") + 1] = str(seed)
    if elf is not None:
        assert buck_out is not None
        old_mount = (
            f"--mount=type=bind,source={FBSOURCE_BUCK_OUT},target={FBSOURCE_BUCK_OUT}"
        )
        if argv.count(old_mount) != 1 or not argv[boundary + 1].startswith(
            FBSOURCE_BUCK_OUT + "/"
        ):
            raise SystemExit("template does not use the fbsource buck-out ELF")
        new[argv.index(old_mount)] = (
            f"--mount=type=bind,source={buck_out},target={buck_out}"
        )
        new[boundary + 1] = str(elf)
    changes = [
        {"index": index, "template": old, "replay": token}
        for index, (old, token) in enumerate(zip(argv, new))
        if old != token
    ]
    if len(new) != len(argv) or new[boundary + 2 :] != argv[boundary + 2 :]:
        raise SystemExit("rewrite changed the argv shape or guest arguments")
    if any("/tmp/" in t or t.endswith("=/tmp") or t == "/tmp" for t in new):
        raise SystemExit(f"replay argv names /tmp: {new}")
    return new, changes


def owned_empty_dir(path: Path) -> dict[str, object]:
    path.mkdir()
    status = path.stat()
    if status.st_uid != os.getuid() or any(path.iterdir()):
        raise SystemExit(f"attempt cwd is not an owned empty directory: {path}")
    return {"path": str(path), "st_dev": status.st_dev, "st_ino": status.st_ino}


def detailed_inventory(root: Path) -> list[dict[str, object]]:
    return [
        {
            "path": str(p.relative_to(root)),
            "size": p.stat().st_size,
            "sha256": hashlib.sha256(p.read_bytes()).hexdigest(),
        }
        for p in sorted(root.rglob("*"))
        if p.is_file()
    ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", required=True, choices=["thrift", "asyncssl", "httpcoro"])
    parser.add_argument("--template", required=True, choices=["pass", "fail"])
    parser.add_argument("--seed", type=int, help="override --sched-seed")
    parser.add_argument("--out", required=True, type=Path, help="new attempt directory")
    parser.add_argument("--elf", type=Path, help="post-fix ELF in a stress checkout")
    parser.add_argument("--expect-elf-sha256")
    parser.add_argument("--note", default="")
    args = parser.parse_args()

    case = load_manifest(MANIFEST).case(args.case)
    summary, argv = template(args.case, args.template)
    other = "fail" if args.template == "pass" else "pass"
    _, other_argv = template(args.case, other)
    masked = {i for i in attempt_tokens(argv)} | {argv.index("--sched-seed") + 1}
    if len(other_argv) != len(argv) or any(
        a != b for i, (a, b) in enumerate(zip(argv, other_argv)) if i not in masked
    ):
        raise SystemExit("pass and fail templates differ beyond seed and attempt paths")

    run_dir = args.out.resolve()
    if run_dir != args.out.absolute() or not any(
        run_dir.parent == root or root in run_dir.parents
        for root in allowed_out_roots(args.case)
    ):
        raise SystemExit(f"--out must be a new directory under {allowed_out_roots(args.case)}")
    buck_out = None
    expected_guest = case.artifact_sha256
    if args.elf is not None:
        stress_root = STRESS_CHECKOUTS / args.case
        buck_out = stress_root / "buck-out"
        if args.elf.resolve() != args.elf or buck_out not in args.elf.parents:
            raise SystemExit(f"--elf must be a canonical path under {buck_out}")
        if not args.expect_elf_sha256:
            raise SystemExit("--elf requires --expect-elf-sha256")
        expected_guest = args.expect_elf_sha256

    run_dir.parent.mkdir(parents=True, exist_ok=True)
    run_dir.mkdir()
    (run_dir / "verify").mkdir()
    cwd_identity = owned_empty_dir(run_dir / "controller-cwd")
    cwd = run_dir / "controller-cwd"
    guest_tmp = run_dir / "guest-tmp"
    replay, changes = rewrite(
        argv, run_dir, cwd, guest_tmp, args.seed, args.elf, buck_out
    )
    hermit = Path(replay[replay.index("run") - 1])
    guest = Path(replay[replay.index("--") + 1])
    inputs = {
        "safehermit": record(SAFEHERMIT),
        "hermit": record(hermit),
        "guest": record(guest),
    }
    if (
        inputs["safehermit"]["sha256"] != SAFEHERMIT_SHA256
        or inputs["hermit"]["sha256"] != case.hermit_sha256
        or inputs["guest"]["sha256"] != expected_guest
    ):
        raise SystemExit(f"pinned input identity differs: {inputs}")
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in ("RUST_LOG", "HERMIT_BIN", "PYTHONPATH")
    }
    environment["TMPDIR"] = str(E3 / "tmpdir")
    environment["SAFEHERMIT_LOG_ROOT"] = str(run_dir / "safehermit-log-root")
    stdout = run_dir / "stdout.txt"
    stderr = run_dir / "stderr.txt"
    started = time.time()
    with stdout.open("xb") as out, stderr.open("xb") as err:
        try:
            completed = subprocess.run(
                replay,
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
        else classify(case, tuple(replay), rc, stdout, stderr)
    )
    receipt = {
        "argv": replay,
        "argv_sha256": hashlib.sha256("\0".join(replay).encode()).hexdigest(),
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
        "guest_tmp_inventory": inventory(guest_tmp),
        "inputs": inputs,
        "note": args.note,
        "rewrites_from_template": changes,
        "safehermit_report": record(run_dir / "safehermit.txt")
        if (run_dir / "safehermit.txt").exists()
        else None,
        "schema": "tfa.witness-replay-run.v1",
        "sched_seed": int(value_of(replay, "--sched-seed")),
        "started_unix": started,
        "stderr": record(stderr),
        "stdout": record(stdout),
        "template": {
            "role": args.template,
            "summary": record(E3 / "relocated-v2" / args.case / "summary.json"),
            "seed": summary["witness"][args.template]["seed"],
        },
        "verify_inventory": inventory(run_dir / "verify"),
    }
    (run_dir / "receipt.json").write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n"
    )
    print(
        f"{case.case_id} {run_dir.name} sched_seed={receipt['sched_seed']} rc={rc} "
        f"oracle={classification['oracle']} {finished - started:.1f}s "
        f"receipt={run_dir / 'receipt.json'}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
