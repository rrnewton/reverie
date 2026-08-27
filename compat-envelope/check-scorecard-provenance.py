#!/usr/bin/env python3
"""Fail closed on parity claims that do not carry both operands and provenance.

Qualified parity is deliberately narrower than a plausible CSV row.  A row is
accepted only when the recorded verdict can be re-derived from two nonempty
SHA-256 operands and the row carries the exact code state, comparison contract,
profile, population identity, and counted run coverage that conditioned it.

The checker is the single semantic authority used by both the validation gate
and the renderer.  ``--aggregate`` additionally refuses more than one run ID;
last-writer-wins pooling is not a measurement of any one run.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
PARITY_COLUMNS = {
    "stdout_parity": ("stdout-sha256-exact-v1", "stdout-exact"),
    "tool_count_parity": ("tool-count-sha256-exact-v1", "tool-count-exact"),
    # The old spelling is readable only when a producer has already supplied
    # the complete modern provenance.  Shape alone never grandfathered a claim.
    "parity": ("stdout-sha256-exact-v1", "stdout-exact"),
}
PROVENANCE_COLUMNS = (
    "ref_output_hash",
    "parity_comparator",
    "parity_tier",
    "profile_flags",
    "population_id",
    "selected_count",
    "executed_count",
    "evidence_count",
)
POPULATION_FIELDS = (
    "run_mode",
    "lane",
    "bucket",
    "test_id",
    "test_mode",
    "backend",
    "cell_state",
)


def positive_int(raw: str) -> int | None:
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return None
    return value if value >= 0 else None


def derive_population_id(members: list[tuple[int, dict[str, str]]]) -> str:
    keys = sorted(
        "\t".join((row.get(field) or "").strip() for field in POPULATION_FIELDS)
        for _, row in members
    )
    digest = hashlib.sha256()
    digest.update(b"scorecard-population-v2\n")
    for key in keys:
        digest.update(key.encode("utf-8"))
        digest.update(b"\n")
    return "sha256:" + digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("csv_path", type=Path)
    parser.add_argument("--run-id", help="validate only this run ID")
    parser.add_argument(
        "--observable", choices=("stdout", "tool-count"), default="stdout",
        help="meaning of the legacy `parity` spelling",
    )
    parser.add_argument(
        "--aggregate",
        action="store_true",
        help="validate an aggregate request; refuses more than one run ID",
    )
    args = parser.parse_args()

    try:
        with args.csv_path.open(newline="", encoding="utf-8") as handle:
            reader = csv.DictReader(handle)
            header = reader.fieldnames or []
            rows = list(reader)
    except OSError as error:
        print(f"check-scorecard-provenance: cannot read {args.csv_path}: {error}", file=sys.stderr)
        return 2
    if not rows:
        print("check-scorecard-provenance: empty CSV", file=sys.stderr)
        return 2
    if any(None in row for row in rows):
        print("REFUSED: CSV has over-wide rows beyond its declared header", file=sys.stderr)
        return 1

    parity_columns = [name for name in PARITY_COLUMNS if name in header]
    modern_columns = [name for name in ("stdout_parity", "tool_count_parity") if name in header]
    if "parity" in parity_columns and modern_columns:
        print(
            "REFUSED: expected exactly one parity observable column, found "
            f"{parity_columns}; legacy `parity` cannot coexist with a named observable",
            file=sys.stderr,
        )
        return 1
    if not parity_columns:
        print(
            "REFUSED: expected exactly one parity observable column, found none",
            file=sys.stderr,
        )
        return 1
    selected_modern = f"{args.observable.replace('-', '_')}_parity"
    if selected_modern in modern_columns:
        parity_column = selected_modern
    elif len(modern_columns) == 1:
        # Preserve the renderer's more specific observable-mismatch diagnostic.
        parity_column = modern_columns[0]
    elif parity_columns == ["parity"]:
        parity_column = "parity"
    else:
        print(
            f"REFUSED: scorecard has no `{selected_modern}` column for --observable {args.observable}",
            file=sys.stderr,
        )
        return 1
    if parity_column == "parity" and args.observable == "tool-count":
        expected_comparator, expected_tier = PARITY_COLUMNS["tool_count_parity"]
    else:
        expected_comparator, expected_tier = PARITY_COLUMNS[parity_column]
    is_tool_count = expected_comparator.startswith("tool-count-")

    missing_schema = [column for column in PROVENANCE_COLUMNS if column not in header]
    if missing_schema:
        print(
            "REFUSED: scorecard lacks required provenance columns: " + ", ".join(missing_schema),
            file=sys.stderr,
        )
        return 1

    scoped = [row for row in rows if not args.run_id or row.get("run_id") == args.run_id]
    if args.run_id and not scoped:
        print(f"REFUSED: run_id={args.run_id!r} has no rows", file=sys.stderr)
        return 1
    run_ids = {row.get("run_id", "") for row in scoped}
    if args.aggregate and len(run_ids) != 1:
        print(
            "REFUSED: MIXED_RUN_AGGREGATE contains "
            f"{len(run_ids)} run IDs ({', '.join(sorted(run_ids))}); select one run",
            file=sys.stderr,
        )
        return 1

    failures: list[str] = []
    by_run: dict[str, list[tuple[int, dict[str, str]]]] = defaultdict(list)
    claims = 0
    rederived = 0
    for line, row in enumerate(scoped, start=2):
        run_id = (row.get("run_id") or "").strip()
        by_run[run_id].append((line, row))
        test = row.get("test_id") or "<unknown-test>"
        prefix = f"line {line} {test}"
        verdict = (row.get(parity_column) or "").strip()
        for other in modern_columns:
            if other != parity_column and (row.get(other) or "").strip():
                failures.append(
                    f"{prefix}: non-selected observable {other} is populated"
                )
        if verdict not in ("0", "1"):
            continue
        claims += 1

        hermit_sha = (row.get("hermit_sha") or "").strip()
        reverie_sha = (row.get("reverie_sha") or "").strip()
        candidate = (row.get("output_hash") or "").strip()
        reference = (row.get("ref_output_hash") or "").strip()
        if not SHA40.fullmatch(hermit_sha):
            failures.append(f"{prefix}: hermit_sha is not exact 40-hex: {hermit_sha!r}")
        if not SHA40.fullmatch(reverie_sha):
            failures.append(f"{prefix}: reverie_sha is not exact 40-hex: {reverie_sha!r}")
        if (row.get("dirty") or "").strip().lower() not in ("false", "0"):
            failures.append(f"{prefix}: dirty code state cannot qualify parity")
        if not SHA256.fullmatch(candidate):
            failures.append(f"{prefix}: candidate output_hash is not SHA-256")
        if not SHA256.fullmatch(reference):
            failures.append(f"{prefix}: ref_output_hash is absent or not SHA-256")

        comparator = (row.get("parity_comparator") or "").strip()
        parity_tier = (row.get("parity_tier") or "").strip()
        if comparator != expected_comparator:
            failures.append(
                f"{prefix}: parity_comparator={comparator!r}, expected {expected_comparator!r}"
            )
        if parity_tier != expected_tier:
            failures.append(f"{prefix}: parity_tier={parity_tier!r}, expected {expected_tier!r}")

        try:
            profile = json.loads(row.get("profile_flags") or "")
        except (TypeError, json.JSONDecodeError):
            profile = None
        if not isinstance(profile, dict) or not isinstance(profile.get("comparison"), list):
            failures.append(f"{prefix}: profile_flags is not a JSON object with comparison argv")
        else:
            argv = profile["comparison"]
            if not argv or not all(isinstance(item, str) for item in argv):
                failures.append(f"{prefix}: comparison argv is empty or malformed")
            elif not is_tool_count and "--strict" not in argv:
                failures.append(f"{prefix}: stdout comparison profile omits --strict")
            elif is_tool_count:
                collector = profile.get("collector")
                if (
                    not isinstance(collector, list)
                    or len(collector) != 2
                    or collector[0] != "--reps"
                    or positive_int(collector[1]) in (None, 0)
                ):
                    failures.append(f"{prefix}: tool-count profile omits a positive --reps")

        population = (row.get("population_id") or "").strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", population):
            failures.append(f"{prefix}: population_id is absent or malformed")
        selected = positive_int(row.get("selected_count") or "")
        executed = positive_int(row.get("executed_count") or "")
        evidence = positive_int(row.get("evidence_count") or "")
        if None in (selected, executed, evidence):
            failures.append(f"{prefix}: selected/executed/evidence counts are malformed")
        elif not (0 < evidence <= executed <= selected):
            failures.append(
                f"{prefix}: invalid coverage counts selected={selected} executed={executed} evidence={evidence}"
            )

        if run_id == "" or (row.get("run_utc") or "").strip() == "":
            failures.append(f"{prefix}: population/run identity is incomplete")
        if SHA256.fullmatch(candidate) and SHA256.fullmatch(reference):
            derived = "1" if candidate == reference else "0"
            if verdict != derived:
                failures.append(
                    f"{prefix}: verdict={verdict} but operand hashes re-derive {derived}"
                )
            else:
                rederived += 1

    # Conditions that describe a run must be one-valued and their counts must
    # bind to the rows actually present, not merely look plausible on each row.
    for run_id, members in by_run.items():
        claims_in_run = sum(
            (row.get(parity_column) or "").strip() in ("0", "1") for _, row in members
        )
        # Historical rows whose verdict was moved to
        # legacy_parity_unqualified are retained only as data, not claims.  Their
        # missing/mixed conditions are exactly why they were de-qualified and do
        # not block newly qualified runs.
        if claims_in_run == 0:
            continue
        if not run_id:
            failures.append("run identity: blank run_id")
            continue
        fields = (
            "run_utc",
            "hermit_sha",
            "reverie_sha",
            "population_id",
            "selected_count",
            "executed_count",
            "evidence_count",
        )
        for field in fields:
            values = {(row.get(field) or "").strip() for _, row in members}
            if "" in values:
                failures.append(f"run {run_id}: row is missing {field}")
            if len(values) > 1:
                failures.append(
                    f"run {run_id}: mixed {field} values ({', '.join(sorted(values))})"
                )
        for line, row in members:
            hermit_sha = (row.get("hermit_sha") or "").strip()
            reverie_sha = (row.get("reverie_sha") or "").strip()
            if not SHA40.fullmatch(hermit_sha):
                failures.append(
                    f"run {run_id} line {line}: hermit_sha is not exact 40-hex"
                )
            if not SHA40.fullmatch(reverie_sha):
                failures.append(
                    f"run {run_id} line {line}: reverie_sha is not exact 40-hex"
                )
            if (row.get("dirty") or "").strip().lower() not in ("false", "0"):
                failures.append(f"run {run_id} line {line}: code state is dirty")
        first = members[0][1]
        selected = positive_int(first.get("selected_count") or "")
        evidence = positive_int(first.get("evidence_count") or "")
        if selected != len(members):
            failures.append(
                f"run {run_id}: selected_count={selected} but CSV contains {len(members)} rows"
            )
        if evidence != claims_in_run:
            failures.append(
                f"run {run_id}: evidence_count={evidence} but {claims_in_run} parity rows exist"
            )
        recorded_population = (first.get("population_id") or "").strip()
        derived_population = derive_population_id(members)
        if recorded_population != derived_population:
            failures.append(
                f"run {run_id}: population_id does not bind the {len(members)} selected rows"
            )

    if failures:
        print(
            f"check-scorecard-provenance: REFUSED {len(failures)} defect(s); "
            f"claims={claims} rederived={rederived}",
            file=sys.stderr,
        )
        for failure in failures[:30]:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(
        f"check-scorecard-provenance: PASS rows={len(scoped)} runs={len(run_ids)} "
        f"claims={claims} rederived={rederived} observable={parity_column}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
