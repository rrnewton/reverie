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


# Roots that are launchers rather than the program under test. A cell whose
# argv[0] is one of these ran the wrapper directly; whatever it went on to
# exec is not recoverable from argv alone, so the defect CANNOT be attributed
# to a named program and must stay unknown.
WRAPPER_ROOTS = frozenset({"bash", "sh", "env"})

# The workload driver is a wrapper too, distinguished only because it is named
# by path rather than by basename.
WORKLOAD_SCRIPT = "real_compat_workload.sh"


def root_executable(program: dict[str, object]) -> str | None:
    """The executable a cell actually launched, or None when it is a wrapper.

    ATTRIBUTION IS BY argv[0], NOT BY LABEL. The label is a SCENARIO name
    chosen by the corpus author -- `mpstat-softirqs`, `sar-resource-tables` --
    while argv[0] names the executable that ran. The earlier rule required
    `Path(argv[0]).name == label` and therefore filed every scenario whose name
    differs from its binary under "shell-or-launcher-wrapped", i.e. as
    unattributable, even though argv[0] named a real program outright. Six
    stable-GAP cells were mis-filed that way.
    """
    argv = [str(item) for item in program["argv"]]
    if WORKLOAD_SCRIPT in " ".join(argv):
        return None
    first = Path(argv[0]).name
    # `[` is the test builtin invoked directly; it is a program, not a launcher.
    if first == "[":
        return first
    if first in WRAPPER_ROOTS:
        return None
    return first


def harness_form(program: dict[str, object]) -> str:
    argv = [str(item) for item in program["argv"]]
    if WORKLOAD_SCRIPT in " ".join(argv):
        return "workload-script"
    if root_executable(program) is None:
        return "shell-or-launcher-wrapped"
    return "named-program-direct"


def named_defect_eligible(program: dict[str, object]) -> bool:
    """A defect may be published against a named program only if one ran.

    This is the property the campaign actually needs: not "is the harness
    shaped a particular way" but "is there an executable to name". Keeping it a
    separate predicate stops the two questions drifting apart again.
    """
    return root_executable(program) is not None


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
    stable_gap_attribution: Counter[str] = Counter()
    # A one-element list so the writer loop can increment it without `nonlocal`.
    wrapper_unattributable_unknown = [0]
    candidate_counts: Counter[str] = Counter()
    backend_candidate_counts: dict[str, Counter[str]] = defaultdict(Counter)

    fields = [
        "program", "backend", "harness_form", "root_executable",
        "runs", "repeat_stability", "dominant_state",
        "n_pass", "n_fail", "n_timeout", "n_unqualifiable", "n_not_attempted",
        "distinct_states", "observed_sequence", "example_reason",
    ]
    candidate_fields = [
        "program", "backend", "harness_form", "root_executable",
        "required_runs", "observed_runs",
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
            root = root_executable(programs[program])
            all_writer.writerow(
                {
                    "program": program,
                    "backend": backend,
                    "harness_form": form,
                    "root_executable": root or "",
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
            eligible = named_defect_eligible(programs[program])
            publishable = classification == "GAP" and eligible
            if classification == "GAP":
                stable_gap_attribution[form] += 1
                if not eligible:
                    wrapper_unattributable_unknown[0] += 1
            candidate_writer.writerow(
                {
                    "program": program,
                    "backend": backend,
                    "harness_form": form,
                    "root_executable": root or "",
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
        "schema_version": 2,
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
        "stable_gap_attribution": dict(sorted(stable_gap_attribution.items())),
        # NEVER "healthy": these are stable failures whose root executable is a
        # wrapper, so no named program can be blamed. Unknown is the honest
        # verdict; counting them as clean would be the defect this corrects.
        "wrapper_unattributable_unknown": wrapper_unattributable_unknown[0],
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
