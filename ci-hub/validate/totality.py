#!/usr/bin/env python3
"""Decide whether a validate run was TOTAL, INCREMENTAL, or UNKNOWN.

WHY THIS EXISTS. The landing certifier
(`ci-hub/lib/validate_status.rs::is_clean_full_coverage`, lines 151-157) decides
"this was a full-coverage run" from five fields, of which the two that carry the
scope are ``profile == "full"`` and ``selection_mode == "full"``. Both record
what the run ASKED FOR. Neither records what it ACHIEVED.

That gap is not theoretical. In the live ledger, commit
``ee3038998fda5250904cb21a7f66a1ce245af87e`` (2026-08-04T03:16:57Z) satisfies all
five predicates with ``result=pass`` while its own ``coverage`` block records
``planned_test_nodes=19, executed_test_nodes=4`` -- fifteen test nodes, including
``test.strict_compat`` and ``test.detcore_misc``, never ran. A run that executed
21% of its planned nodes is certifiable as a total pass.

THE RULE THIS MODULE ENFORCES: a declared profile can DOWNGRADE a run to
incremental, but it can never UPGRADE one to total. Only observed execution
promotes. Where observation is missing, the answer is UNKNOWN -- never TOTAL.
Absence of evidence is the single most dangerous input here, because the
majority of the ledger has it: of 357 pass rows, 309 (86.6%) carry no
``coverage`` block at all.

EVERY VERDICT CARRIES ITS DENOMINATOR. ``executed_test_nodes`` alone is a count
without a scale; ``4`` means nothing until ``19`` is beside it. The returned
record always states both, plus which field decided the verdict, so a consumer
never has to re-derive the premise and a wrong call is visible in the record
rather than inferable only from source.

RELATIONSHIP TO THE TWO EXISTING PREDICATES -- read this before calling either,
because three coverage predicates in one tree is a drift hazard unless each says
what it is for:

* ``validate_status.rs::is_clean_full_coverage`` (Rust, landing certifier) and
  ``flake_class.is_full_coverage`` (Python) both answer "was this run DECLARED
  full-scope?" from ``profile``/``selection_mode``. That is the right question
  for flake classification, and it is the WRONG question for landing.
* This module answers "did this run OBSERVABLY execute everything?"

They are not interchangeable and they do not agree: measured on the live ledger,
``flake_class.is_full_coverage`` and ``is_total`` differ on 386 of 654 rows, all
in the same direction -- the declaration says full where execution does not show
it. That is expected, not a bug in either. Swapping one for the other silently
would either promote 386 unverified rows or reclassify every flake; decide which
question you are asking first.
"""

from __future__ import annotations

from pathlib import Path
import sys
from typing import Any, Mapping

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ledger"))
import validate_rows as canonical_ledger  # noqa: E402

TOTAL = "TOTAL"
INCREMENTAL = "INCREMENTAL"
UNKNOWN = "UNKNOWN"

# Profile names that DECLARE a narrowed scope. Matching one is sufficient to
# call a run incremental without any coverage evidence, because a narrowed
# declaration cannot describe a total run. The list is a downgrade-only input:
# NOT matching it proves nothing, which is why absence lands in UNKNOWN.
_PARTIAL_PROFILE_MARKERS = ("-only", "only-", "selective", "quick", "shallow")


def _int_or_none(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def coverage_fraction(row: Mapping[str, Any]) -> tuple[int | None, int | None]:
    """``(executed_test_nodes, planned_test_nodes)`` or ``(None, None)``.

    Returned as a pair on purpose: a consumer that wants the numerator cannot
    get it without also receiving the denominator.
    """
    cov = row.get("coverage")
    if not isinstance(cov, Mapping):
        return (None, None)
    return (
        _int_or_none(cov.get("executed_test_nodes")),
        _int_or_none(cov.get("planned_test_nodes")),
    )


def declared_partial(row: Mapping[str, Any]) -> bool:
    """Whether the run's own DECLARATION says its scope was narrowed."""
    profile = str(row.get("profile") or "")
    selection = str(row.get("selection_mode") or "")
    if profile and any(m in profile for m in _PARTIAL_PROFILE_MARKERS):
        return True
    # `selection_mode` is "full" for an unnarrowed run; anything else (e.g.
    # "only") selected a subset.
    return bool(selection) and selection != "full"


def classify(row: Mapping[str, Any]) -> dict[str, Any]:
    """Typed totality verdict for one ledger row.

    Returns ``{scope, reason, executed_test_nodes, planned_test_nodes,
    coverage_evidence, declared_profile, declared_selection}``.

    Precedence, and the ordering is the whole point:

    1. DIRECT EVIDENCE WINS. If a ``coverage`` block is present it decides,
       because it records what ran. ``executed < planned`` is INCREMENTAL even
       when the profile says ``full`` -- that is exactly the ee3038998f row, and
       any rule that let the profile win here would re-open the hole.
    2. A NARROWED DECLARATION DOWNGRADES. With no coverage block, a profile or
       selection that declares a subset is enough to call it INCREMENTAL. This
       direction is safe: a run that says it narrowed did narrow.
    3. EVERYTHING ELSE IS UNKNOWN. ``profile == "full"`` with no coverage block
       is NOT total. It is an unverified claim, and the whole defect is that it
       has been read as a verified one.
    """
    executed, planned = coverage_fraction(row)
    profile = row.get("profile")
    selection = row.get("selection_mode")
    base = {
        "executed_test_nodes": executed,
        "planned_test_nodes": planned,
        "coverage_evidence": executed is not None and planned is not None,
        "declared_profile": profile,
        "declared_selection": selection,
    }

    if executed is not None and planned is not None:
        if planned <= 0:
            # A zero denominator cannot certify anything. "0 of 0 nodes ran" is
            # not a total run; it is a run whose plan is unknown or empty.
            return {**base, "scope": UNKNOWN,
                    "reason": "coverage present but planned_test_nodes<=0 (no denominator)"}
        absent = row.get("coverage", {}).get("absent_nodes") or []
        zero = row.get("coverage", {}).get("zero_executed_nodes") or []
        if executed < planned:
            return {**base, "scope": INCREMENTAL,
                    "reason": f"observed {executed}/{planned} test nodes executed"
                              f" ({len(absent)} absent, {len(zero)} zero-executed)"}
        if zero or absent:
            # Counts can agree while named nodes are still missing or inert.
            return {**base, "scope": INCREMENTAL,
                    "reason": f"counts agree at {executed}/{planned} but"
                              f" {len(absent)} absent / {len(zero)} zero-executed nodes remain"}
        if declared_partial(row):
            # Full observed coverage under a narrowed declaration: contradictory.
            # Refuse to promote; a disagreement between the two sources is not a
            # basis for the stronger claim.
            return {**base, "scope": UNKNOWN,
                    "reason": f"coverage says {executed}/{planned} complete but declaration"
                              f" is narrowed (profile={profile!r}, selection={selection!r})"}
        return {**base, "scope": TOTAL,
                "reason": f"observed {executed}/{planned} test nodes executed,"
                          " no absent or zero-executed nodes"}

    if declared_partial(row):
        return {**base, "scope": INCREMENTAL,
                "reason": f"no coverage evidence; declaration is narrowed"
                          f" (profile={profile!r}, selection={selection!r})"}

    return {**base, "scope": UNKNOWN,
            "reason": f"no coverage evidence; profile={profile!r} selection={selection!r}"
                      " declare a full run but nothing records what executed"}


def is_total(row: Mapping[str, Any]) -> bool:
    """Strictly ``scope == TOTAL``. UNKNOWN is deliberately NOT total.

    Provided so no caller has to re-implement the comparison and accidentally
    write ``!= INCREMENTAL``, which would silently promote every UNKNOWN row --
    86.6% of recorded passes.
    """
    return classify(row)["scope"] == TOTAL


def incremental_chain_depth(rows, *, passes_only: bool = True) -> dict[str, Any]:
    """How many runs have accumulated since the last TOTAL one.

    ``rows`` is any iterable of ledger rows, consumed newest-first, so pass them
    in that order. Returns the depth plus the row that ended the chain, so the
    answer is auditable rather than a bare number.

    ``passes_only`` (default True) counts only PASSING runs, and this default is
    load-bearing. A first version counted every row and reported "depth=69 runs
    since the last total". Its actual composition was 60 fail / 9 pass /
    1 no_result. "69 incremental verifications have accumulated without a full
    one" and "9 passing runs have been accepted since the last provably-total
    pass" are different claims, and only the second is the drift the number is
    supposed to express -- a FAILED run is not a verification anyone relied on,
    it is a run that told you something was broken. Counting failures inflated
    the figure roughly sevenfold.

    The composition is returned either way, so the number can never again be
    read without seeing what it is made of.

    Depth carries an explicit ``anchored`` flag: a chain that never reaches a
    TOTAL row is NOT depth-N, it is depth-at-least-N with no known anchor, and
    conflating those understates drift exactly when drift is worst.
    """
    depth = 0
    composition: dict[str, int] = {}
    skipped_non_pass = 0
    for row in rows:
        verdict = classify(row)
        if verdict["scope"] == TOTAL:
            return {
                "depth": depth,
                "anchored": True,
                "anchor_commit": row.get("commit"),
                "anchor_finished_at": row.get("finished_at"),
                "anchor_reason": verdict["reason"],
                "composition": composition,
                "skipped_non_pass": skipped_non_pass,
                "passes_only": passes_only,
            }
        result = str(row.get("result") or "unknown")
        composition[result] = composition.get(result, 0) + 1
        if passes_only and result != "pass":
            skipped_non_pass += 1
            continue
        depth += 1
    return {
        "depth": depth,
        "anchored": False,
        "anchor_commit": None,
        "anchor_finished_at": None,
        "anchor_reason": "no TOTAL run found in the rows examined;"
                         " depth is a LOWER BOUND, not a measurement",
        "composition": composition,
        "skipped_non_pass": skipped_non_pass,
        "passes_only": passes_only,
    }


def _audit() -> int:
    """Report scope breakdown and chain depth for a ledger, with denominators."""
    import collections
    rows = canonical_ledger.rows()
    if not rows:
        print("totality: no rows in canonical ledger union")
        return 2

    def table(label, subset):
        counts = collections.Counter(classify(r)["scope"] for r in subset)
        print(f"{label} (denominator = {len(subset)}):")
        for scope in (TOTAL, INCREMENTAL, UNKNOWN):
            n = counts[scope]
            print(f"  {scope:<12} {n:>5}  ({100 * n / len(subset):5.1f}%)")
        return counts

    print(f"ledger: canonical-union  rows={len(rows)}\n")
    table("ALL ROWS", rows)
    passes = [r for r in rows if r.get("result") == "pass"]
    if passes:
        print()
        table("PASS ROWS", passes)

    # What the landing certifier accepts, split by what the rows actually show.
    accepted = [
        r for r in rows
        if r.get("commit_anchored") is True and r.get("tree_dirty") is False
        and r.get("selection_mode") == "full" and r.get("profile") == "full"
        and r.get("result") == "pass"
    ]
    if accepted:
        print()
        counts = table("CERTIFIER-ACCEPTED AS FULL GREEN", accepted)
        if counts[INCREMENTAL]:
            print(f"  WARNING: {counts[INCREMENTAL]} row(s) are PROVABLY PARTIAL yet certifiable.")

    ordered = sorted(rows, key=lambda r: str(r.get("finished_at") or ""), reverse=True)
    depth = incremental_chain_depth(ordered)
    print(f"\nINCREMENTAL CHAIN DEPTH (newest first): depth={depth['depth']} "
          f"anchored={depth['anchored']}")
    print(f"  anchor: {depth['anchor_commit']} @ {depth['anchor_finished_at']}")
    print(f"  {depth['anchor_reason']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(_audit())
