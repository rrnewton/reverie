#!/usr/bin/env python3
"""Gate COMPLETENESS for a ledger receipt: did all declared gates genuinely run?

THE BUG THIS FIXES.  `flake_class.gate_counts` substitutes a hardcoded
`FULL_GATES_EXPECTED = 5` whenever a schema-3+ full receipt does not declare
`gates_expected`.  `qualified_rows.is_qualified` then treats `ran >= expected`
as PROOF of completeness.  So a receipt that ran 5 gates of the current 6-gate
contract, and declared no contract of its own, resolves to `(5, 5)` and is
certified a complete green:

    plant: schema=5, profile=full, gates_run=5, gates_expected ABSENT
      gate_counts -> (5, 5)   is_qualified -> True     <-- a 5-of-6 partial, accepted
    same row with gates_expected=6 declared
      gate_counts -> (5, 6)   is_qualified -> False    <-- correctly refused

The defect is not the read order.  It is that **an INFERRED denominator cannot
prove completeness**.  `flake_class` says so itself: "the hardcoded
FULL_GATES_EXPECTED can lag the live plan (a full run today executes six gates
against a hardcoded expectation of five)".  Once the plan moves to six, the
hardcode silently reclassifies every 5-of-6 short run as complete, and nothing
reports it -- a green that never carried what it verified.

THE RULE.  A green must carry its own conditions.  A count-capable receipt
(schema >= DECLARE_REQUIRED_SCHEMA) must DECLARE `gates_expected`; an inferred
expectation is not evidence and cannot certify completeness.  Older receipts
predate the field and can never be improved, so they keep the legacy inference
-- explicitly labelled `inferred` rather than silently blessed.  This is a
RATCHET: it closes the hole for every receipt the current producer emits without
retroactively voiding history it cannot fix.

WHY NOT JUST BUMP THE CONSTANT TO 6.  That re-arms the same trap at the next
contract change and buys nothing: the receipt still would not carry its own
denominator.  It also breaks `flake_class.is_truncated`, which deliberately
treats an over-run (`ran > expected`) as a complete PASS precisely BECAUSE the
hardcode lags.  This module therefore does NOT touch `gate_counts`;
`is_truncated` keeps its own semantics.
"""
from __future__ import annotations

from typing import Any, Mapping, Optional

from flake_class import FULL_GATES_EXPECTED, gate_counts

SCHEMA_VERSION = 1

# The schema at which the producer is expected to record its own gate contract.
# Receipts at or above this must declare `gates_expected`; below it, the legacy
# inference stands because the field did not exist and never can.
DECLARE_REQUIRED_SCHEMA = 5

DECLARED = "declared"    # the receipt recorded its own contract -- evidence
INFERRED = "inferred"    # substituted from the hardcode -- a guess, not evidence
ABSENT = "absent"        # no contract available at all

# The ledger is an authority boundary, so this is deliberately a closed
# vocabulary. Adding a scheduler reason does not make it qualification-safe;
# this consumer must opt in separately after reviewing the reason's semantics.
ALLOWED_INTENTIONAL_SKIP_REASONS = frozenset({"empty-manifest-bucket"})
INTENTIONAL_SKIP_FIELD = "intentional_skipped_nodes"


def intentional_skips(
    record: Mapping[str, Any],
) -> tuple[Optional[list[tuple[str, str]]], Optional[str]]:
    """Parse qualification-safe intentional skips, refusing ambiguous shapes.

    A typed skip is accounting evidence, never an executed gate or PASS. The
    return value is separate from ``gates_run`` by construction.
    """
    raw = record.get(INTENTIONAL_SKIP_FIELD, [])
    if not isinstance(raw, list):
        return None, f"{INTENTIONAL_SKIP_FIELD} is not an array"
    parsed: list[tuple[str, str]] = []
    seen: set[str] = set()
    for item in raw:
        if not isinstance(item, Mapping):
            return None, f"{INTENTIONAL_SKIP_FIELD} contains a non-object"
        name = item.get("name")
        reason = item.get("reason")
        if not isinstance(name, str) or not name.strip():
            return None, "intentional skip has no node identity"
        if reason not in ALLOWED_INTENTIONAL_SKIP_REASONS:
            return None, f"intentional skip {name!r} has unknown reason {reason!r}"
        if name in seen:
            return None, f"intentional skip {name!r} is duplicated"
        seen.add(name)
        parsed.append((name, reason))
    return parsed, None


def _empty_node_list(record: Mapping[str, Any], field: str) -> tuple[bool, str]:
    """Require an explicitly empty typed node list for a skipped full run."""
    value = record.get(field)
    if not isinstance(value, list):
        return False, f"{field} is missing or not an array"
    if value:
        return False, f"{field} is nonempty"
    return True, ""


def expectation_source(record: Mapping[str, Any]) -> str:
    """Where did `expected` come from? The whole fix turns on this distinction."""
    if isinstance(record.get("gates_expected"), int):
        return DECLARED
    _ran, expected = gate_counts(dict(record))
    return INFERRED if isinstance(expected, int) else ABSENT


def resolve_gates(record: Mapping[str, Any]) -> tuple[Optional[int], Optional[int], str]:
    """(ran, expected, source). `ran` still honours the gates_run/checks fallback."""
    ran, expected = gate_counts(dict(record))
    return ran, expected, expectation_source(record)


def gates_complete(record: Mapping[str, Any]) -> tuple[bool, str]:
    """Did every gate of a KNOWN contract genuinely run?

    Returns (complete, reason). The reason names the first failing condition so a
    refusal is auditable rather than a bare False.
    """
    ran, expected, source = resolve_gates(record)
    schema = record.get("schema_version")
    schema = schema if isinstance(schema, int) else 0

    if not isinstance(ran, int) or isinstance(ran, bool):
        return False, "gates_run/checks missing or non-integer"
    if source == ABSENT or not isinstance(expected, int):
        return False, "no gate contract available"
    if expected <= 0:
        return False, f"vacuous contract (gates_expected={expected})"

    # A typed intentional skip is useful only relative to a contract the
    # producer actually declared. It must not turn the legacy inferred fallback
    # into modern completeness evidence by returning before this check.
    if source == INFERRED and schema >= DECLARE_REQUIRED_SCHEMA:
        return False, (
            f"schema {schema} receipt did not declare gates_expected; the inferred "
            f"expectation ({FULL_GATES_EXPECTED}) cannot prove completeness "
            "(a 5-of-6 short run is indistinguishable from a complete 5-of-5)"
        )

    skips, skip_error = intentional_skips(record)
    if skip_error is not None or skips is None:
        return False, skip_error or "intentional skip evidence is invalid"

    # Once a producer declares an intentional omission, every other planned
    # node must be accounted for explicitly. A dependency skip or unaccounted
    # node cannot hide behind the one allowed empty-bucket omission.
    if skips:
        for field in ("dependency_skipped_nodes", "unaccounted_nodes"):
            ok, why = _empty_node_list(record, field)
            if not ok:
                return False, why
        total_skipped = record.get("skipped_nodes")
        if (
            not isinstance(total_skipped, int)
            or isinstance(total_skipped, bool)
            or total_skipped != len(skips)
        ):
            return False, (
                f"skipped_nodes must equal the {len(skips)} typed intentional skip(s)"
            )
        accounted = ran + len(skips)
        if accounted != expected:
            return False, (
                "planned-node accounting mismatch: "
                f"executed={ran} intentional_skipped={len(skips)} expected={expected}"
            )
        return True, (
            f"{ran} executed + {len(skips)} intentionally skipped = "
            f"{expected} planned gates ({source}); skips are not PASS"
        )

    if ran < expected:
        return False, f"short run: {ran} of {expected} gates"
    return True, f"{ran} of {expected} gates ({source})"


def audit(rows) -> dict[str, Any]:
    """Population view: how many receipts can actually PROVE completeness."""
    counts: dict[str, int] = {}
    complete = 0
    for row in rows:
        ok, _reason = gates_complete(row)
        complete += bool(ok)
        counts[expectation_source(row)] = counts.get(expectation_source(row), 0) + 1
    return {"rows": len(list(rows)) if hasattr(rows, "__len__") else None,
            "complete": complete, "expectation_source": counts}
