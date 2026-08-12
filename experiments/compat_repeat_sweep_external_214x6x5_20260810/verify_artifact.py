#!/usr/bin/env python3
"""Verify hashes, population shape, partition, and derived outputs."""

from __future__ import annotations

import csv
import hashlib
import json
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parent


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def classify(row: dict[str, str]) -> str:
    stability = row["repeat_stability"]
    state = row["dominant_state"]
    if stability == "FLAKE":
        return "unstable"
    if stability != "CONSISTENT":
        raise ValueError(f"unknown repeat_stability: {stability}")
    if state == "PASS":
        return "green"
    if state == "FAIL":
        return "stable_fail"
    if state in {"TIMEOUT", "ATTEMPTED_UNQUALIFIABLE", "NOT_ATTEMPTED"}:
        return "no_verdict"
    raise ValueError(f"unknown consistent dominant_state: {state}")


def main() -> int:
    metadata = json.loads((ROOT / "metadata.json").read_text())
    for relative, expected in metadata["file_sha256"].items():
        actual = sha256(ROOT / relative)
        if actual != expected:
            raise SystemExit(f"hash mismatch {relative}: {actual} != {expected}")

    with (ROOT / "results/stability-5.csv").open(newline="") as source:
        rows = list(csv.DictReader(source))
    keys = [(row["program"], row["backend"]) for row in rows]
    programs = {row["program"] for row in rows}
    backends = {row["backend"] for row in rows}
    if len(rows) != 1284 or len(set(keys)) != 1284:
        raise SystemExit(f"expected 1284 rows and unique keys; got {len(rows)}, {len(set(keys))}")
    if len(programs) != 214 or len(backends) != 6:
        raise SystemExit(f"expected 214 programs x 6 backends; got {len(programs)} x {len(backends)}")
    for row in rows:
        counters = sum(int(row[name]) for name in (
            "n_pass", "n_fail", "n_timeout", "n_unqualifiable", "n_not_attempted"
        ))
        if int(row["runs"]) != 5 or counters != 5 or len(row["observed_sequence"].split("|")) != 5:
            raise SystemExit(f"invalid five-run row: {row['program']}/{row['backend']}")

    counts = Counter(classify(row) for row in rows)
    expected_counts = Counter({
        "green": 495,
        "stable_fail": 348,
        "unstable": 118,
        "no_verdict": 323,
    })
    if counts != expected_counts or sum(counts.values()) != 1284:
        raise SystemExit(f"partition mismatch: {counts} != {expected_counts}")

    with tempfile.TemporaryDirectory(prefix="verify-1284-") as temp:
        command = [
            sys.executable,
            str(ROOT / "tools/analyze_repeat_stability.py"),
            "--corpus", str(ROOT / "inputs/corpus.json"),
            "--out-dir", temp,
            *[str(ROOT / f"inputs/sweep-{index}.csv") for index in range(1, 6)],
        ]
        subprocess.run(command, check=True)
        for name in ("stability-5.csv", "stability-5.json", "failure-candidates-5.csv"):
            generated = Path(temp) / name
            published = ROOT / "results" / name
            if generated.read_bytes() != published.read_bytes():
                raise SystemExit(f"regeneration mismatch: {name}")

    print("PASS hashes=13 rows=1284 unique_keys=1284 programs=214 backends=6 sweeps=5")
    print("PASS partition green=495 stable_fail=348 unstable=118 no_verdict=323 total=1284")
    print("PASS regenerated=3/3 byte-identical")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
