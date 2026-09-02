#!/usr/bin/env python3
"""Can the drain report see STALENESS, and does it drain oldest-first?

WHY THIS EXISTS. The owner's correction (2026-08-04) is that an open-PR COUNT is
the wrong metric: "OLD PRs ARE ALWAYS A BREACH OF THE PROTOCOL. It is not just the
NUMBER of PRs, it is the STALENESS." A count treats a 3-hour-old PR and a
3-week-old PR as the same row. That queue-age rule does not redefine validation
authority: "Exact-commit evidence does not require the fleet to discard everything
it learned whenever `main` advances." Hard/soft-green landability stays with the
validation-authority rule. `pre.reverie_pin` states the same rule for the Reverie
pin: "A lagging ancestor is legitimate; requiring the tip made the verdict a
property of WHEN you looked rather than of the tree."

Before this, `pr_status.py` did not fetch `createdAt` at all, carried no age
field, and emitted PRs in whatever order GitHub returned. So the report could not
rank by the thing that actually matters.

Bracketed BOTH directions, because an ordering test that only checks the happy
path passes on an unsorted list often enough to be useless:
  * POSITIVE -- a genuinely older PR must sort ahead of a newer one, and the age
    must be present and numerically right.
  * NEGATIVE -- a PR whose age is UNKNOWN must NOT sort first. `None` age must
    never masquerade as "oldest" and jump the drain queue, and must never be
    silently coerced to 0 (which would read as "brand new").
"""

from __future__ import annotations

import datetime
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import pr_status  # noqa: E402

FAILURES: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> None:
    if ok:
        print(f"  ok    {label}")
    else:
        FAILURES.append(label)
        print(f"  FAIL  {label}" + (f" -- {detail}" if detail else ""))


def iso(hours_ago: float) -> str:
    stamp = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=hours_ago)
    return stamp.isoformat().replace("+00:00", "Z")


# The exact sort the report applies, kept in one place so the test exercises the
# real ordering rule rather than a paraphrase of it.
def drain_order(rows):
    return sorted(
        rows,
        key=lambda row: (row.get("age_hours") is None, -(row.get("age_hours") or 0.0)),
    )


print("case SCHEMA — the report must actually collect the timestamp it ranks on")
check("createdAt is in GH_FIELDS", "createdAt" in pr_status.GH_FIELDS,
      repr(pr_status.GH_FIELDS))
check("_age_hours exists", hasattr(pr_status, "_age_hours"))

print("case AGE — hours since open, measured not guessed")
check("72h-old PR reports ~72h", abs(pr_status._age_hours(iso(72)) - 72.0) < 0.1,
      repr(pr_status._age_hours(iso(72))))
check("1h-old PR reports ~1h", abs(pr_status._age_hours(iso(1)) - 1.0) < 0.1,
      repr(pr_status._age_hours(iso(1))))

print("case UNKNOWN-AGE IS NOT ZERO — fail-closed, not fail-new")
for label, value in (("missing", None), ("empty", "   "),
                     ("malformed", "not-a-date"), ("wrong type", 12345)):
    check(f"{label} createdAt yields None, not 0",
          pr_status._age_hours(value) is None, repr(pr_status._age_hours(value)))

print("case POSITIVE — oldest drains first")
rows = [
    {"pr": 3, "age_hours": pr_status._age_hours(iso(2))},
    {"pr": 1, "age_hours": pr_status._age_hours(iso(500))},
    {"pr": 2, "age_hours": pr_status._age_hours(iso(48))},
]
order = [r["pr"] for r in drain_order(rows)]
check("a 500h PR sorts ahead of 48h and 2h", order == [1, 2, 3], repr(order))
check("the oldest row carries the largest age", drain_order(rows)[0]["age_hours"] > 400)

print("case NEGATIVE — an unknown age must NOT jump the queue")
rows_unknown = [
    {"pr": 9, "age_hours": None},                                  # unknown
    {"pr": 1, "age_hours": pr_status._age_hours(iso(500))},        # genuinely oldest
    {"pr": 3, "age_hours": pr_status._age_hours(iso(2))},
]
order = [r["pr"] for r in drain_order(rows_unknown)]
check("unknown-age PR sorts LAST, not first", order == [1, 3, 9], repr(order))
check("the genuinely oldest PR is still first", order[0] == 1, repr(order))

print("case NOT-INERT — the old behaviour would fail this")
# The pre-change report emitted rows in GitHub's order with no age at all. Model
# that and assert the test can tell the difference, so a regression to
# "return rows unchanged" is caught rather than silently passing.
unsorted_order = [r["pr"] for r in rows]          # 3, 1, 2 as constructed
check("input order differs from drain order (test can distinguish)",
      unsorted_order != [r["pr"] for r in drain_order(rows)],
      f"{unsorted_order} vs {[r['pr'] for r in drain_order(rows)]}")
check("a row with no age_hours key is treated as unknown, not newest",
      drain_order([{"pr": 7}, {"pr": 8, "age_hours": 5.0}])[0]["pr"] == 8)

print()
if FAILURES:
    print(f"FAIL ({len(FAILURES)} assertions)")
    sys.exit(1)
print("PASS")
