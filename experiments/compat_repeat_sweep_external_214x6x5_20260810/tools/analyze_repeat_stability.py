#!/usr/bin/env python3
"""Classify repeat stability without turning missing evidence into a verdict."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path


EXPECTED_CELLS = 1284
EXPECTED_RUNS = 5


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def harness_form(program: dict[str, object]) -> str:
    argv = [str(item) for item in program["argv"]]
    label = str(program["label"])
    if "real_compat_workload.sh" in " ".join(argv):
        return "workload-script"
    first = Path(argv[0]).name
    if first == label or (label == "bracket" and first == "["):
        return "named-program-direct"
    if label == "bash" and first == "bash":
        return "named-program-direct"
    return "shell-or-launcher-wrapped"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("sweeps", type=Path, nargs="+")
    args = parser.parse_args()
    if len(args.sweeps) > EXPECTED_RUNS:
        raise SystemExit(f"refused: at most {EXPECTED_RUNS} sweeps")
    corpus_document = json.loads(args.corpus.read_text())
    programs = {
        str(program["label"]): program for program in corpus_document["programs"]
    }
    if len(programs) != 214:
        raise SystemExit(f"refused: expected 214 corpus programs, found {len(programs)}")

    observations: dict[tuple[str, str], list[dict[str, str]]] = defaultdict(list)
    hashes: list[str] = []
    expected_keys: set[tuple[str, str]] | None = None
    for path in args.sweeps:
        rows = list(csv.DictReader(path.open(newline="")))
        keys = {(row["program"], row["backend"]) for row in rows}
        if len(rows) != EXPECTED_CELLS or len(keys) != EXPECTED_CELLS:
            raise SystemExit(
                f"refused: {path} is not one complete unique {EXPECTED_CELLS}-cell sweep"
            )
        if expected_keys is not None and keys != expected_keys:
            raise SystemExit(f"refused: {path} has a different cell population")
        expected_keys = keys
        hashes.append(digest(path))
        for row in rows:
            observations[(row["program"], row["backend"])].append(row)
    if len(hashes) != len(set(hashes)):
        raise SystemExit("refused: duplicate sweep content is not an independent repeat")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    all_path = args.out_dir / f"stability-{len(args.sweeps)}.csv"
    candidate_path = args.out_dir / f"failure-candidates-{len(args.sweeps)}.csv"
    summary_path = args.out_dir / f"stability-{len(args.sweeps)}.json"
    all_counts: Counter[str] = Counter()
    candidate_counts: Counter[str] = Counter()
    backend_candidate_counts: dict[str, Counter[str]] = defaultdict(Counter)

    fields = [
        "program", "backend", "harness_form", "runs", "repeat_stability", "dominant_state",
        "n_pass", "n_fail", "n_timeout", "n_unqualifiable", "n_not_attempted",
        "distinct_states", "observed_sequence", "example_reason",
    ]
    candidate_fields = [
        "program", "backend", "harness_form", "required_runs", "observed_runs",
        "n_pass", "n_fail", "n_timeout", "n_unqualifiable", "n_not_attempted",
        "classification", "observed_sequence", "named_defect_publishable",
        "attribution", "example_reason",
    ]
    with all_path.open("w", newline="") as all_stream, candidate_path.open(
        "w", newline=""
    ) as candidate_stream:
        all_writer = csv.DictWriter(all_stream, fieldnames=fields)
        candidate_writer = csv.DictWriter(candidate_stream, fieldnames=candidate_fields)
        all_writer.writeheader()
        candidate_writer.writeheader()
        for program, backend in sorted(observations):
            rows = observations[(program, backend)]
            states = [row["state"] for row in rows]
            counts = Counter(states)
            if len(rows) < EXPECTED_RUNS:
                stability = "NOT_YET_DETERMINED"
            elif len(counts) > 1:
                stability = "FLAKE"
            else:
                stability = "CONSISTENT"
            all_counts[stability] += 1
            dominant = counts.most_common(1)[0][0]
            sequence = "|".join(states)
            reason = next((row["reason"] for row in rows if row["reason"]), "")
            form = harness_form(programs[program])
            all_writer.writerow(
                {
                    "program": program,
                    "backend": backend,
                    "harness_form": form,
                    "runs": len(rows),
                    "repeat_stability": stability,
                    "dominant_state": dominant,
                    "n_pass": counts["PASS"],
                    "n_fail": counts["FAIL"],
                    "n_timeout": counts["TIMEOUT"],
                    "n_unqualifiable": counts["ATTEMPTED_UNQUALIFIABLE"],
                    "n_not_attempted": counts["NOT_ATTEMPTED"],
                    "distinct_states": len(counts),
                    "observed_sequence": sequence,
                    "example_reason": reason,
                }
            )

            # The requested three-way split applies to cells whose first
            # qualifying sweep proposed a named FAIL.  Stable PASS/no-result
            # cells are not silently added to that candidate population.
            if states[0] != "FAIL":
                continue
            if len(rows) < EXPECTED_RUNS:
                classification = "NOT_YET_DETERMINED"
            elif len(counts) > 1:
                classification = "FLAKE"
            else:
                classification = "GAP"
            candidate_counts[classification] += 1
            backend_candidate_counts[backend][classification] += 1
            wrapper = form != "named-program-direct"
            publishable = classification == "GAP" and not wrapper
            candidate_writer.writerow(
                {
                    "program": program,
                    "backend": backend,
                    "harness_form": form,
                    "required_runs": EXPECTED_RUNS,
                    "observed_runs": len(rows),
                    "n_pass": counts["PASS"],
                    "n_fail": counts["FAIL"],
                    "n_timeout": counts["TIMEOUT"],
                    "n_unqualifiable": counts["ATTEMPTED_UNQUALIFIABLE"],
                    "n_not_attempted": counts["NOT_ATTEMPTED"],
                    "classification": classification,
                    "observed_sequence": sequence,
                    "named_defect_publishable": str(publishable).lower(),
                    "attribution": form,
                    "example_reason": reason,
                }
            )

    summary = {
        "schema_version": 1,
        "scope": {
            "programs": 214,
            "backends": 6,
            "cells": EXPECTED_CELLS,
            "completed_sweeps": len(args.sweeps),
            "required_sweeps": EXPECTED_RUNS,
        },
        "sweep_sha256s": hashes,
        "all_cells": dict(sorted(all_counts.items())),
        "first_sweep_failure_candidates": dict(sorted(candidate_counts.items())),
        "failure_candidates_by_backend": {
            backend: dict(sorted(counts.items()))
            for backend, counts in sorted(backend_candidate_counts.items())
        },
    }
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(all_path)
    print(candidate_path)
    print(summary_path)


if __name__ == "__main__":
    main()
