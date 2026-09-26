#!/usr/bin/env python3
"""Emit the canonical qualified PASS view of the validate ledger.

This is the guard for ad-hoc derived views. Two invariants are non-negotiable:

1. Order by event time (``finished_at``), never append/file position.
2. Drop incomplete, aborted, and zero-executed rows before bucketing or timing.

A row qualifies only when it records ``result == "pass"``, a positive executed
test count, a parseable completion time, and a satisfied positive gate contract.
A typed ``empty-manifest-bucket`` skip may account for a planned gate, but is
never an executed gate or PASS. Missing evidence fails closed. Callers needing
failure taxonomy must use ``flake_class.effective_result``; this accessor is the
canonical population for green timing/concurrency analysis only.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path
import sys
from typing import Iterable, Mapping

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ledger"))
import validate_rows as canonical_ledger  # noqa: E402
import qualifying_receipt  # noqa: E402

DEFAULT_LEDGER = Path(__file__).resolve().parents[2] / "ledger"

from gate_completeness import gates_complete
from green_class import HARD
from green_class import derive_class


def event_time(row: Mapping[str, object]) -> dt.datetime | None:
    value = row.get("finished_at")
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(dt.timezone.utc)


def is_qualified(row: Mapping[str, object]) -> bool:
    """Return whether ``row`` is a complete, nonempty PASS measurement."""
    # Check original claimed fields before the timing view copies a row. Old
    # absent-claim measurements retain their narrower historical population.
    if "admission_floor_evidence" in row and not qualifying_receipt.admission_accepted(
        qualifying_receipt.admission_verdict(row, qualifying_receipt.active())
    ):
        return False
    executed = row.get("executed_tests")
    # A SOFT (inherited) green is not a measurement AT THIS HEAD: validation ran
    # on an ancestor and is being speculatively trusted here. This view is the
    # canonical population for green timing/concurrency analysis, so admitting a
    # run that never executed at this head would corrupt exactly what it measures.
    # The class is DERIVED from provenance (`validated_head_sha`/`inherited_from`)
    # and a disagreeing `green_class` label is REFUSED, so this cannot be passed
    # by stamping a field. Rows predating the provenance fields default to HARD,
    # which is why adding this clause does not reject any existing producer.
    green_class, _green_reason = derive_class(dict(row))
    # A Buck full run times the release E2E payload (no debug assertions, no
    # overflow checks), not the Cargo debug binary every other green measures.
    # release_builder_of reads the builder and payload together.
    return (
        green_class == HARD
        and qualifying_receipt.release_builder_of(row, qualifying_receipt.active()) == "cargo"
        and row.get("result") == "pass"
        and isinstance(executed, int)
        and not isinstance(executed, bool)
        and executed > 0
        and gates_complete(row)[0]
        and event_time(row) is not None
    )


def qualified_rows(rows: Iterable[Mapping[str, object]]) -> list[dict[str, object]]:
    """Return qualified rows sorted by event time, independent of file order."""
    selected = [dict(row) for row in rows if is_qualified(row)]
    return sorted(
        selected,
        key=lambda row: (
            event_time(row),
            str(row.get("commit") or ""),
            str(row.get("slot") or ""),
            str(row.get("log_file") or ""),
        ),
    )


def load_rows(path: Path) -> tuple[list[dict[str, object]], int]:
    if path.resolve() == DEFAULT_LEDGER.resolve():
        return canonical_ledger.rows(preserve_admission=True), 0
    if path.is_dir():
        return canonical_ledger.rows(root=path.resolve().parent, preserve_admission=True), 0
    rows: list[dict[str, object]] = []
    malformed = 0
    with path.open(errors="replace") as handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                value = json.loads(line, object_pairs_hook=qualifying_receipt.AdmissionJsonObject)
            except json.JSONDecodeError:
                malformed += 1
                continue
            if isinstance(value, dict):
                rows.append(value)
            else:
                malformed += 1
    return rows, malformed


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args(argv)
    try:
        rows = canonical_ledger.rows(preserve_admission=True)
        malformed = 0
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"qualified-rows: canonical ledger union refused: {error}", file=sys.stderr)
        return 2
    selected = qualified_rows(rows)
    for row in selected:
        print(json.dumps(row, separators=(",", ":"), sort_keys=True))
    print(
        f"qualified-rows: {len(selected)}/{len(rows)} qualified; "
        f"malformed={malformed}; sorted=finished_at",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
