#!/usr/bin/env python3
"""Verdict producer for the strict-standard components — detlog, stack, heap.

OWNERSHIP: this module renders VERDICTS. It does not run guests, does not
capture output, and must never grow a collector. hermit-w7 owns collection (42
cells, 1260 runs at n=30, planted-divergence control) and calls in here with
already-captured text. Rewriting that collector would discard real measurement;
duplicating it here would create the two-implementations problem this split
exists to avoid.

WHAT A COLLECTOR PASSES IN: the raw combined stdout+stderr of a run, exactly as
captured. This module finds its own records. Callers do not pre-filter, because
a caller that filters is a second extractor that can silently disagree with this
one about what a record is.

=== THE THREE DECISIONS THAT MAKE THIS NON-VACUOUS ===

1. EMPTY IS NOT AGREEMENT, and for stack/heap this is the whole ballgame.
   `detlog_stack` and `detlog_heap` DEFAULT TO FALSE (hermit metadata.rs:213-214;
   the emitter is gated at detcore/src/lib.rs:719). A run without those flags
   emits ZERO stack records, two empty streams have an identical digest, and a
   bare `a == b` reports PASS. That single line would make the entire stack and
   heap dimension green by default, forever, on runs that measured nothing. Zero
   records is NOT_MEASURED — a third state, never a pass and never a failure.

2. STACK AND HEAP YIELD TWO VERDICTS EACH, NOT ONE. A memory record is
   `<addr-range> <perms> ... [stack|heap] -> <sha256>`, carrying an ADDRESS claim
   and a CONTENT claim in one line. These are measured to disagree: contents
   match across runs while addresses do not. Comparing the whole line collapses
   them, and the collapsed answer is dominated by the address half — so a
   content-deterministic backend scores FAIL for a layout reason. Reported
   separately, each with its own denominator.

3. SELF-DETERMINISM IS THE DEFAULT AXIS. Cross-backend equality is refused for
   detlog because the backends emit different record counts for the same guest
   (measured 141 / 368 / 1245): equality is false by construction and a single
   parity percentage has no denominator. See `cross_backend_prefix`.
"""

from __future__ import annotations

import hashlib
import re

PASS = "pass"
FAIL = "fail"
NOT_MEASURED = "not-measured"

#: Components of the strict standard. A cell is `strict` only with all four.
STRICT_COMPONENTS = ("stdout", "info_log", "stack", "heap")

MARKER = "DETLOG"
_TRAILING_WS = re.compile(r"[ \t]+$")

#: `<start>-<end> <perms> ... [stack] -> <sha256>` (detcore/src/logdiff.rs:1049).
_MEMORY_RECORD = re.compile(
    r"(?P<range>0x[0-9a-fA-F]+-0x[0-9a-fA-F]+).*?"
    r"\[(?P<region>stack|heap)\].*?->\s*(?P<content>[0-9a-fA-F]{8,})"
)


def _records(text: str) -> list[str]:
    """Every DETLOG record, prefix-normalised, in order.

    Only the tracing formatter's wall-clock/level prefix is dropped -- it is real
    time and comparing it would fail every run for a reason unrelated to
    determinism. Nothing INSIDE a record is touched, so virtual time, counts,
    syscall values and addresses all stay compared.
    """
    out: list[str] = []
    for line in text.splitlines():
        idx = line.find(MARKER)
        if idx >= 0:
            out.append(_TRAILING_WS.sub("", line[idx:]))
    return out


def extract(text: str, component: str) -> list[str]:
    """Records for one component. `stack`/`heap` return whole memory records."""
    recs = _records(text)
    if component == "detlog":
        return recs
    if component in ("stack", "heap"):
        keep = []
        for r in recs:
            m = _MEMORY_RECORD.search(r)
            if m and m.group("region") == component:
                keep.append(r)
        return keep
    raise ValueError(f"unknown component {component!r}")


def split_memory(record: str) -> tuple[str, str] | None:
    """`(address_range, content_hash)` for a memory record, or None."""
    m = _MEMORY_RECORD.search(record)
    return (m.group("range"), m.group("content")) if m else None


def digest(items: list[str]) -> str:
    h = hashlib.sha256()
    for i in items:
        h.update(i.encode("utf-8", "surrogateescape"))
        h.update(b"\n")
    return h.hexdigest()


def common_prefix(a: list[str], b: list[str]) -> int:
    n = 0
    for x, y in zip(a, b):
        if x != y:
            break
        n += 1
    return n


def _compare(a: list[str], b: list[str], *, what: str, empty_hint: str) -> dict:
    """The one comparison every verdict funnels through.

    One implementation so no component can drift into a weaker rule than its
    siblings, which is how `bitwise_parity` ended up hardcoded while `parity`
    stayed real.
    """
    if not a or not b:
        return {
            "dimension": what,
            "verdict": NOT_MEASURED,
            "reason": f"no {what} records (a={len(a)} b={len(b)}); {empty_hint}",
            "denominator_a": len(a),
            "denominator_b": len(b),
            "differing": None,
            "common_prefix": 0,
            "digest_a": None,
            "digest_b": None,
        }
    da, db = digest(a), digest(b)
    # Positional mismatches PLUS the length delta: a truncated stream agreeing
    # everywhere it overlaps is still a divergence and must not score 0.
    differing = sum(1 for x, y in zip(a, b) if x != y) + abs(len(a) - len(b))
    return {
        "dimension": what,
        "verdict": PASS if da == db else FAIL,
        "reason": "" if da == db else f"{differing} of {max(len(a), len(b))} {what} records differ",
        "denominator_a": len(a),
        "denominator_b": len(b),
        "differing": differing,
        "common_prefix": common_prefix(a, b),
        "digest_a": da,
        "digest_b": db,
    }


_EMPTY_HINT = {
    "detlog": "the run needs --log info for DETLOG records to be emitted at all",
    "stack": "detlog_stack defaults to FALSE, so a run without it emits none; "
    "zero records is not agreement",
    "heap": "detlog_heap defaults to FALSE, so a run without it emits none; "
    "zero records is not agreement",
}


def detlog_verdict(run_a: str, run_b: str) -> dict:
    """Self-determinism over the whole DETLOG stream."""
    return _compare(
        extract(run_a, "detlog"), extract(run_b, "detlog"),
        what="detlog", empty_hint=_EMPTY_HINT["detlog"],
    )


def memory_verdict(run_a: str, run_b: str, region: str) -> dict:
    """Stack or heap, as TWO verdicts: content and address.

    Deliberately returns no single combined boolean. Contents and addresses are
    measured to disagree, so any collapse of them is dominated by the address
    half and reports a content-deterministic backend as failing.
    """
    if region not in ("stack", "heap"):
        raise ValueError(f"region must be stack or heap, got {region!r}")
    ra, rb = extract(run_a, region), extract(run_b, region)
    pa = [p for p in (split_memory(r) for r in ra) if p]
    pb = [p for p in (split_memory(r) for r in rb) if p]
    hint = _EMPTY_HINT[region]
    return {
        "region": region,
        "content": _compare([c for _, c in pa], [c for _, c in pb],
                            what=f"{region} content", empty_hint=hint),
        "address": _compare([a for a, _ in pa], [a for a, _ in pb],
                            what=f"{region} address", empty_hint=hint),
        "combined": None,  # explicit: there is no single stack/heap boolean
    }


def cross_backend_prefix(a_text: str, b_text: str, component: str = "detlog") -> dict:
    """Cross-backend relationship WITHOUT a verdict.

    Refuses pass/fail on purpose: the backends emit different record counts for
    the same guest, so equality is false by construction and a lone percentage
    has no denominator. Both denominators are returned so neither can be quoted
    alone.
    """
    a, b = extract(a_text, component), extract(b_text, component)
    pre = common_prefix(a, b)
    return {
        "comparable": False,
        "component": component,
        "reason": f"{component} record counts are backend-specific; equality is "
        "not a defined comparison across backends",
        "denominator_a": len(a),
        "denominator_b": len(b),
        "common_prefix": pre,
        "prefix_over_a_pct": round(100.0 * pre / len(a), 1) if a else None,
        "prefix_over_b_pct": round(100.0 * pre / len(b), 1) if b else None,
    }


def compose_tier(verdicts: dict[str, str]) -> str:
    """Name what was actually compared. Never `strict` on a subset.

    A cell claiming strict while three of the four components have no producer
    is the defect this whole line of work came from. `partial:` keeps the claim
    legible instead of letting it inherit a label for checks that never ran.
    """
    passed = sorted(k for k, v in verdicts.items() if v == PASS)
    if not passed:
        return ""
    if set(STRICT_COMPONENTS).issubset(set(passed)):
        return "strict"
    return "partial:" + "+".join(passed)


#: A verdict is WRITTEN to its column only when a comparison actually decided
#: something. `NOT_MEASURED` and `""` are no-results and must leave the column
#: BLANK, never carry a string.
#:
#: This is not cosmetic. `check_cell_comparison.py` treats ANY non-blank value in
#: a verdict column as the assertion "a comparison was performed", and demands a
#: `ref_output_hash` beside it. Writing `not-measured` would therefore manufacture
#: an unreferenced verdict out of an absence -- turning a no-result into a
#: violation, which is the same collapse of the third outcome that this whole
#: line of work exists to prevent, just pointed the other way.
_DECIDED = (PASS, FAIL)


def _decided(verdict: str) -> bool:
    return verdict in _DECIDED


def scorecard_fields(
    *, stdout: str = "", info_log: str = "", detlog: dict | None = None,
    stack: dict | None = None, heap: dict | None = None,
    stack_flag: bool | None = None, heap_flag: bool | None = None,
    exit_code: str = "", oracle: str = "",
) -> dict:
    """Flatten verdicts into the columns a collector writes.

    Every value carries its denominator. A bare verdict cannot be audited:
    `pass` over 0 records and `pass` over 1245 are different facts.

    A DECIDED verdict must reach a COLUMN. Until 2026-08-07 this function took
    `stdout` and `info_log`, used them to compose the tier, and then dropped
    them: neither was ever written to `out`. So a cell could be tiered on a
    stdout comparison whose verdict appeared nowhere in the row -- the tier
    asserted the comparison, and the evidence for it was discarded on the way to
    the CSV. `stdout_parity` was consequently blank on all 2,290 published rows
    while 1,671 stdout observations sat in `legacy_parity_unqualified` as
    free text. `detlog_parity` had the mirror-image defect: written here since
    the detlog producer landed, but with no such column in the scorecard schema
    to receive it.
    """
    out: dict[str, object] = {}
    comp: dict[str, str] = {"stdout": stdout, "info_log": info_log}
    if _decided(stdout):
        out["stdout_parity"] = stdout
    if _decided(info_log):
        out["info_log_parity"] = info_log
    # No producer computes these yet; the parameters exist so that when one does,
    # it has a typed field to return rather than inventing a second convention.
    # They stay blank until then -- see `unimplemented_comparisons` in
    # scorecard-schema.json, which refuses a non-blank value in either column.
    if _decided(exit_code):
        out["exit_code_parity"] = exit_code
    if _decided(oracle):
        out["oracle_verdict"] = oracle
    if detlog:
        out["detlog_parity"] = detlog["verdict"]
        out["detlog_records"] = detlog["denominator_a"]
    for name, mv, requested in (
        ("stack", stack, stack_flag),
        ("heap", heap, heap_flag),
    ):
        # These scorecard columns record invocation MODE, not the comparison
        # outcome.  Mode is known before the guest runs: true means the flag was
        # passed, false means it was not, and absence is historical unknown.
        if requested is not None:
            out[f"{name}_parity"] = "1" if requested else "0"
        if not mv:
            continue
        if requested is False:
            raise ValueError(f"{name} verdict exists although its flag was not passed")
        out[f"{name}_content_parity"] = mv["content"]["verdict"]
        out[f"{name}_address_parity"] = mv["address"]["verdict"]
        out[f"{name}_records"] = mv["content"]["denominator_a"]
        # The strict component is CONTENT; address divergence is reported beside
        # it, not folded into it.
        comp[name] = mv["content"]["verdict"]
    out["tier"] = compose_tier(comp)
    return out
