#!/usr/bin/env python3
"""A TOOL MUST STATE WHAT IT DID NOT CHECK.

THE CLASS, and its canonical instance
-------------------------------------
`scorecard-full-manifest-denominator` (closed 2026-08-01): a backend passing
131 of 194 tests rendered as a fraction of 28, because the scorecard counted
only the portable-CI lane buckets. Every parity figure was against the wrong
denominator and NOTHING LOOKED WRONG. That is the shape: a tool answering a
NARROWER question than the reader assumed, with no indication it was doing so.

THE RULE
--------
If an answer is SCOPED, the SCOPE IS PART OF THE ANSWER.

  "Consistent" is not "correct".
  "Green" is not "tested".
  "Enforced" is not "enforced everywhere".

A verdict that omits its scope is not a weaker answer -- it is a DIFFERENT
answer to a question the reader did not ask, and the reader cannot tell.

WHAT THIS MODULE PROVIDES
-------------------------
`Scope` -- the declaration: what WAS examined, what was NOT, and the
denominator (`examined` of `total`, and where `total` came from).

`ScopedVerdict` -- a verdict bound to its scope. `render()` always prints the
omissions, so a scoped OK cannot be quoted as an unscoped one.

`audit_scope()` -- the DETECTOR. Given a verdict, it refuses:
  * a partial view (`examined < total`) that declares no omissions;
  * any verdict whose denominator is unstated when it examined a subset;
  * a `total` that is itself a guess rather than sourced -- the denominator bug
    was not a miscount, it was counting the WRONG POPULATION confidently.

The detector exists because "remember to state your scope" is exactly the kind
of rule that decays. A partial view that forgets to declare itself is now a
FLAGGED DEFECT rather than an invisible one.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import json
from typing import Any, Optional

OK = "OK"
REFUSED = "REFUSED"

# A scoped verdict may never be reported with these words alone; each is a
# claim about a whole that a subset cannot support.
OVERCLAIM_WORDS = ("all", "every", "fully", "everywhere", "complete", "no issues")


@dataclass
class Scope:
    """What a tool examined, and — load-bearing — what it did not."""

    checks: str                       # the question this tool actually answers
    not_checked: list[str] = field(default_factory=list)
    examined: Optional[int] = None    # denominator: numerator
    total: Optional[int] = None       # denominator: population size
    total_source: str = ""            # WHERE `total` came from; "" == unsourced

    @property
    def is_partial(self) -> bool:
        if self.examined is None or self.total is None:
            return False
        return self.examined < self.total

    def as_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {"checks": self.checks, "not_checked": list(self.not_checked)}
        if self.examined is not None or self.total is not None:
            d["denominator"] = {"examined": self.examined, "total": self.total,
                                "total_source": self.total_source}
        return d

    def render(self) -> str:
        lines = [f"scope: this checks {self.checks}"]
        if self.examined is not None and self.total is not None:
            pct = (100.0 * self.examined / self.total) if self.total else 0.0
            lines.append(
                f"scope: examined {self.examined}/{self.total} ({pct:.0f}%)"
                + (f" [total from {self.total_source}]" if self.total_source else
                   " [total UNSOURCED]")
            )
        for item in self.not_checked:
            lines.append(f"scope: DOES NOT CHECK {item}")
        return "\n".join(lines)


@dataclass
class ScopedVerdict:
    tool: str
    verdict: str
    summary: str
    scope: Scope

    def render(self) -> str:
        # The omissions print WITH the verdict, never in a footnote a reader can
        # quote past. A scoped OK that can be copied without its scope is the
        # defect this module exists to prevent.
        return f"{self.tool}: {self.verdict} — {self.summary}\n" + self.scope.render()

    def as_dict(self) -> dict[str, Any]:
        return {"tool": self.tool, "verdict": self.verdict, "summary": self.summary,
                "scope": self.scope.as_dict()}


@dataclass
class ScopeFinding:
    tool: str
    problem: str
    detail: str = ""


def audit_scope(v: ScopedVerdict) -> list[ScopeFinding]:
    """Flag an undeclared or under-declared partial view. Empty list = clean."""
    out: list[ScopeFinding] = []

    if not v.scope.checks.strip():
        out.append(ScopeFinding(v.tool, "no-scope-declared",
                                "the tool does not say what question it answers"))

    if v.scope.is_partial and not v.scope.not_checked:
        out.append(ScopeFinding(
            v.tool, "undeclared-partial-view",
            f"examined {v.scope.examined}/{v.scope.total} but declares nothing it "
            "skipped -- a reader will take this for the whole population"))

    if v.scope.is_partial and not v.scope.total_source:
        out.append(ScopeFinding(
            v.tool, "unsourced-denominator",
            "the total is stated but not sourced; the scorecard bug was not a "
            "miscount, it was confidently counting the WRONG population"))

    if (v.scope.examined is not None) != (v.scope.total is not None):
        out.append(ScopeFinding(
            v.tool, "half-a-denominator",
            "a numerator without a denominator (or vice versa) cannot be read"))

    if v.verdict == OK and not v.scope.is_partial and v.scope.not_checked:
        # Full coverage of THIS question, but the tool still names blind spots.
        # Not a defect -- but the summary must not read as a whole-system OK.
        low = v.summary.lower()
        for word in OVERCLAIM_WORDS:
            if word in low:
                out.append(ScopeFinding(
                    v.tool, "overclaiming-summary",
                    f"summary says {word!r} while the scope names "
                    f"{len(v.scope.not_checked)} thing(s) it did not check"))
                break
    return out


def consistent_but_broken_is_not_ok(v: ScopedVerdict) -> bool:
    """THE NEGATIVE TEST the task names, as a reusable predicate.

    `check-reverie-pin.rs` answers ANCESTRY. At a pin where `detcore_misc`
    LIVELOCKS it still reports "a bump is OPTIONAL, not required" -- a green
    checkmark explaining why the drain needn't move. The tool is not wrong; the
    READING is, and the reading is only wrong because the scope was invisible.

    Returns True iff this verdict is safe to read as "the thing is fine":
    an OK is only safe when the tool declares nothing material unexamined.
    """
    if v.verdict != OK:
        return False
    return not v.scope.not_checked


# ------------------------------------------- instance 3: the label-backing check

# A label is a CACHE of a fact, never the fact. `locally-validated` was live on
# four PRs with no backing ledger record: the label answered "is the label
# present", which a reader took for "was this validated". The consumer is what
# must be fixed -- and testing the CONSUMER with a fixture is also the safe
# pattern (planting a real authorisation label on a cold PR can satisfy
# merge-gate and auto-merge).


def label_is_backed(*, label_present: bool, backing_record: Optional[dict],
                    head_sha: str) -> ScopedVerdict:
    """A `locally-validated` label counts only when a record backs it AT THIS HEAD.

    Deliberately three separate refusals rather than one boolean: "no label",
    "label with no record", and "label with a record for a DIFFERENT head" are
    different situations, and the third is the one that looks most like success.
    """
    scope = Scope(
        checks="whether a ledger record backs the label AT THIS EXACT HEAD",
        not_checked=["whether that record's run was itself complete "
                     "(use the qualifying-receipt predicate for that)"],
        examined=1, total=1, total_source="the head under test",
    )
    if not label_present:
        return ScopedVerdict("locally-validated", REFUSED, "no label present", scope)
    if backing_record is None:
        return ScopedVerdict(
            "locally-validated", REFUSED,
            "LABEL WITH NO BACKING RECORD -- the label is a cache of a fact that "
            "does not exist (live on four PRs)", scope)
    recorded = str(backing_record.get("commit") or "")
    if recorded.lower() != head_sha.lower():
        return ScopedVerdict(
            "locally-validated", REFUSED,
            f"backing record is for {recorded[:12] or '<none>'}, not this head "
            f"{head_sha[:12]} -- a record for another commit is not evidence here",
            scope)
    return ScopedVerdict("locally-validated", OK,
                         "label is backed by a record at this head", scope)


# --------------------------------------------------------------- the live registry

# The five instances the owner named, with their CURRENT declaration status.
# This registry is itself the denominator for this task -- it would be absurd to
# fix the "state your denominator" class without stating this one.
INSTANCES: dict[str, dict[str, Any]] = {
    "check-reverie-pin": {
        "answers": "whether the pinned reverie commit is an ANCESTOR of the target",
        "not_checked": ["runtime behaviour at that pin (detcore_misc LIVELOCKS there)"],
        "declared": False,
        "where": "hermit/scripts/check-reverie-pin.rs",
        "reachable_here": False,
        "why": "hermit submodule -- needs a slot; declaration text supplied below",
    },
    "green-zero-executed-tests": {
        "answers": "whether a run reported success",
        "not_checked": ["whether any test actually executed (--features gating)"],
        "declared": True,
        "where": "ci-hub/landing/preflight.py::check_green_carries_executed_tests",
        "reachable_here": True,
        "why": "closed: the Rust ledger-event reader and shared HistoryRow refuse absent, "
               "malformed, wrong-commit, missing, zero, and negative typed counts; prose is "
               "not a fallback",
    },
    "locally-validated-label": {
        "answers": "whether the label is present",
        "not_checked": ["whether a ledger record backs it"],
        "declared": True,
        "where": "ci-hub/validate/scope_declaration.py::label_is_backed",
        "reachable_here": True,
        "why": "closed: the CONSUMER predicate refuses a label with no record, and a "
               "record bound to a DIFFERENT head; fetching live labels still needs egress",
    },
    "backend-abstraction-lint": {
        "answers": "the commandment on the backends it enumerates",
        "not_checked": ["the backends it does not enumerate (3 of 6)"],
        "declared": False,
        "where": "hermit/scripts/check-detcore-backend-abstraction.sh",
        "reachable_here": False,
        "why": "hermit submodule -- needs a slot",
    },
    "is-ancestor-pr-head": {
        "answers": "whether the PR HEAD is an ancestor of main",
        "not_checked": ["landing, which is carried by mergeCommit.oid, not the head"],
        "declared": True,
        "where": "ci-hub/landing/preflight.py::check_landed_by_ancestry",
        "reachable_here": True,
        "why": "closed: the head is structurally never tested; a spy test pins it",
    },
}


def registry_report() -> dict[str, Any]:
    declared = [k for k, v in INSTANCES.items() if v["declared"]]
    undeclared = [k for k, v in INSTANCES.items() if not v["declared"]]
    return {
        "total_instances": len(INSTANCES),
        "declared": sorted(declared),
        "undeclared": sorted(undeclared),
        "declared_count": len(declared),
        "reachable_offline": sorted(k for k, v in INSTANCES.items() if v["reachable_here"]),
        "instances": INSTANCES,
    }


def render_registry() -> str:
    r = registry_report()
    out = ["PARTIAL-VIEW REGISTRY — a tool must state what it did not check", ""]
    for name, inst in sorted(INSTANCES.items()):
        mark = "DECLARED  " if inst["declared"] else "UNDECLARED"
        out.append(f"  [{mark}] {name}  ({inst['where']})")
        out.append(f"      answers: {inst['answers']}")
        for nc in inst["not_checked"]:
            out.append(f"      DOES NOT CHECK: {nc}")
        out.append(f"      status: {inst['why']}")
    out += ["", f"DECLARED {r['declared_count']}/{r['total_instances']}",
            f"reachable offline: {len(r['reachable_offline'])}/{r['total_instances']} "
            f"({', '.join(r['reachable_offline'])})"]
    return "\n".join(out)


def main(argv: Optional[list[str]] = None) -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    print(json.dumps(registry_report(), indent=2) if args.json else render_registry())
    # Non-zero while any named instance is still undeclared: the registry is a
    # ratchet, not a status page.
    return 0 if not registry_report()["undeclared"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
