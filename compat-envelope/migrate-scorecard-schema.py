#!/usr/bin/env python3
"""Migrate scorecards to the tier + full provenance schema without inventing evidence.

Historical parity booleans cannot be made qualified after the reference operand,
exact Reverie SHA, profile and run coverage have been discarded.  The migration
therefore moves them byte-for-byte into ``legacy_parity_unqualified`` and clears
the qualified observable.  This preserves the historical observation while
making it impossible for a consumer to count it as a current parity claim.
The independent ``comparison_tier`` is backfilled as ``legacy-unqualified``;
historical rows are never guessed to have met either strict comparison tier.

Idempotent, fail-closed on partial schemas, and serialized with ``flock``.
"""

from __future__ import annotations

import argparse
import csv
import fcntl
import json
import shutil
import sys
from pathlib import Path

TIER_COLUMNS = ("bitwise_parity", "compared_log_messages", "tier")
COMPARISON_TIER_COLUMN = "comparison_tier"
LEGACY_COMPARISON_TIER = "legacy-unqualified"
PROVENANCE_COLUMNS = (
    "legacy_parity_unqualified",
    "ref_output_hash",
    "parity_comparator",
    "parity_tier",
    "profile_flags",
    "population_id",
    "selected_count",
    "executed_count",
    "evidence_count",
)
ANCHOR = "verify_compare"
PARITY_NAMES = ("stdout_parity", "tool_count_parity", "parity")
PRESERVED = (
    "run_id", "run_utc", "hermit_sha", "reverie_sha", "dirty", "run_mode", "lane",
    "bucket", "test_id", "test_mode", "backend", "cell_state", "outcome",
    "deterministic", "output_hash", "duration_ms", "max_rss_kb", "reason",
)
SCHEMA = Path(__file__).with_name("scorecard-schema.json")


def shared_header(path: Path, old_header: list[str]) -> list[str]:
    variant = "scorecard.csv" if path.name == "scorecard-observations.csv" else path.name
    try:
        document = json.loads(SCHEMA.read_text(encoding="utf-8"))
        variants = document["variants"]
        if variant in variants:
            columns = variants[variant]["columns"]
        else:
            known = set(old_header) - {"parity"}
            candidates = [
                definition["columns"]
                for definition in variants.values()
                if known <= set(definition["columns"])
            ]
            if not candidates:
                raise KeyError(variant)
            columns = min(candidates, key=len)
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise ValueError(f"cannot read shared scorecard row for {variant}: {error}") from error
    if not isinstance(columns, list) or not columns or not all(
        isinstance(column, str) and column for column in columns
    ):
        raise ValueError(f"shared scorecard row for {variant} is malformed")
    return columns


def present_or_none(header: list[str], columns: tuple[str, ...], label: str) -> bool:
    present = [column for column in columns if column in header]
    if present and len(present) != len(columns):
        raise ValueError(
            f"partial {label} schema ({', '.join(present)}); refusing partial migration"
        )
    return bool(present)


def migrate(path: Path, *, apply: bool) -> int:
    backup = path.with_suffix(path.suffix + ".pre-provenance-migration")
    with path.open("r+", newline="", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            original = handle.read()
            if not original.strip():
                print(f"REFUSED: {path} is empty", file=sys.stderr)
                return 2
            lines = original.splitlines()
            header = next(csv.reader([lines[0]]))
            original_header = list(header)
            rows = list(csv.DictReader(lines))
            if any(None in row for row in rows):
                print(f"REFUSED: {path} has over-wide rows", file=sys.stderr)
                return 2
            preferred_parity = (
                "tool_count_parity"
                if path.name == "reverie-scorecard.csv"
                else "stdout_parity"
            )
            parity_columns = [name for name in PARITY_NAMES if name in header]
            if "parity" in parity_columns and len(parity_columns) != 1:
                print(
                    f"REFUSED: {path} legacy parity cannot coexist with named observables; found {parity_columns}",
                    file=sys.stderr,
                )
                return 2
            if preferred_parity in parity_columns:
                parity_column = preferred_parity
            elif parity_columns == ["parity"]:
                parity_column = "parity"
            else:
                print(
                    f"REFUSED: {path} has no {preferred_parity} observable; found {parity_columns}",
                    file=sys.stderr,
                )
                return 2

            if ANCHOR not in header:
                if "reason" not in header:
                    print(
                        f"REFUSED: {path} has neither {ANCHOR!r} nor 'reason'",
                        file=sys.stderr,
                    )
                    return 2
                at = header.index("reason") + 1
                header.insert(at, ANCHOR)
                for row in rows:
                    row[ANCHOR] = ""

            try:
                present_or_none(header, TIER_COLUMNS, "tier")
                present_or_none(header, PROVENANCE_COLUMNS, "provenance")
            except ValueError as error:
                print(f"REFUSED: {path} {error}", file=sys.stderr)
                return 2

            try:
                new_header = shared_header(path, header)
            except ValueError as error:
                print(f"REFUSED: {path} {error}", file=sys.stderr)
                return 2
            allowed_old = set(new_header) | {"parity"}
            unknown = [column for column in header if column not in allowed_old]
            if unknown:
                print(
                    f"REFUSED: {path} has columns outside the shared row: {unknown}",
                    file=sys.stderr,
                )
                return 2
            comparison_labelled = 0
            for row in rows:
                for column in new_header:
                    row.setdefault(column, "")
                # The old rows do not carry INFO/stack/heap evidence.  Record
                # that absence explicitly; never guess either strict tier.
                if not (row.get(COMPARISON_TIER_COLUMN) or "").strip():
                    row[COMPARISON_TIER_COLUMN] = LEGACY_COMPARISON_TIER
                    comparison_labelled += 1

            labelled = 0
            unqualified = 0
            for row in rows:
                mode = (row.get("test_mode") or "").strip()
                if (
                    row.get("deterministic") == "1"
                    and not (row.get("tier") or "").strip()
                ):
                    if mode == "counter":
                        row["verify_compare"] = row.get("verify_compare") or "syscall-count-across-reps"
                        row["tier"] = "counter"
                        labelled += 1
                    elif (row.get("verify_compare") or "").strip() == "stripped" and mode == "verify":
                        row["tier"] = "stripped-uncounted"
                        labelled += 1

                verdict = (row.get(parity_column) or "").strip()
                if verdict in ("0", "1"):
                    # No historical file had the complete provenance columns.  Do not
                    # infer any missing value from a neighbouring row or checkout.
                    qualified = all((row.get(column) or "").strip() for column in (
                        "ref_output_hash", "parity_comparator", "parity_tier",
                        "profile_flags", "population_id", "selected_count",
                        "executed_count", "evidence_count",
                    ))
                    if not qualified:
                        prior = (row.get("legacy_parity_unqualified") or "").strip()
                        carried = f"{parity_column}:{verdict}"
                        if prior and prior != carried:
                            print(
                                f"REFUSED: conflicting legacy parity {prior!r} vs {carried!r}",
                                file=sys.stderr,
                            )
                            return 2
                        row["legacy_parity_unqualified"] = carried
                        row[parity_column] = ""
                        unqualified += 1
                if parity_column == "parity":
                    row.pop("parity", None)

            changed = (
                new_header != original_header
                or labelled > 0
                or unqualified > 0
                or comparison_labelled > 0
            )
            print(
                f"rows={len(rows)} header={len(header)}->{len(new_header)} "
                f"tier_labels={labelled} comparison_labels={comparison_labelled} "
                f"parity_moved_unqualified={unqualified}"
            )
            if not changed:
                print(f"ALREADY MIGRATED: {path}")
                return 0
            if not apply:
                print("DRY RUN — pass --apply to write")
                return 0

            shutil.copyfile(path, backup)
            handle.seek(0)
            handle.truncate()
            writer = csv.DictWriter(handle, fieldnames=new_header, lineterminator="\n")
            writer.writeheader()
            writer.writerows(rows)
            handle.flush()
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)

    before = list(csv.DictReader(backup.open(encoding="utf-8")))
    after = list(csv.DictReader(path.open(encoding="utf-8")))
    if len(before) != len(after):
        print(f"FAILED: row count {len(before)} -> {len(after)}", file=sys.stderr)
        return 1
    target_parity_column = preferred_parity if parity_column == "parity" else parity_column
    if target_parity_column not in after[0]:
        print(
            f"FAILED: migrated row has no {target_parity_column} observable",
            file=sys.stderr,
        )
        return 1
    moved = 0
    for index, (old, new) in enumerate(zip(before, after), start=2):
        for column in PRESERVED:
            if column in old and old[column] != new.get(column):
                print(
                    f"FAILED: line {index} column {column} changed "
                    f"{old[column]!r} -> {new.get(column)!r}",
                    file=sys.stderr,
                )
                return 1
        old_parity = (old.get(parity_column) or "").strip()
        if old_parity in ("0", "1") and not (
            new.get(target_parity_column) or ""
        ).strip():
            if new.get("legacy_parity_unqualified") != f"{parity_column}:{old_parity}":
                print(f"FAILED: line {index} lost legacy parity", file=sys.stderr)
                return 1
            moved += 1
    print(
        f"OK: {len(after)} rows migrated; {moved} unbound parity values preserved "
        "as legacy-unqualified; no provenance invented"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "csv_path",
        type=Path,
        nargs="?",
        default=Path(__file__).resolve().parent / "scorecard.csv",
    )
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if not args.csv_path.exists():
        print(f"REFUSED: {args.csv_path} does not exist", file=sys.stderr)
        return 2
    return migrate(args.csv_path, apply=args.apply)


if __name__ == "__main__":
    raise SystemExit(main())
