#!/usr/bin/env python3
"""Name the validate runs whose tooling is no longer what the tree holds.

WHAT THIS ANSWERS, IN ONE GLANCE, THAT PREVIOUSLY COST AN INVESTIGATION.
Validation tooling executes from the SHARED PARENT WORKING TREE -- whatever is on
disk at the moment of each invocation, not what is on `origin/main`. The parent
is chronically behind: measured 2026-08-27, `origin/main` took 96 commits in 12
hours and 32% touched a path the tick polices, so a policed path is superseded
roughly every 23 minutes. A run that outlives one of those updates can execute
two different versions of its own tooling.

`tick_code_currency.py` already pages when the TREE drifts from `origin/main`.
It cannot say whether that drift landed in the middle of a RUN, because a run
recorded only `target` -- the subject being validated. Two runs, one split and
one not, were identical in every field. So the question "did the exposure
actually fire?" had to be reconstructed by hand, and was, on 2026-08-27, while a
headline number sat in doubt.

`start_unit.py` now records `parent_checkout_head`: the commit its tooling came
from. This compares that against the parent's HEAD now. Different means the
tree moved under the run, which is the split condition, visible immediately.

WHAT THIS IS NOT. It is not a lock and it prevents nothing -- a fast-forward of
a tree with live sibling processes was considered and declined, because it moves
files under agents mid-task. This makes the consequence visible instead, which
is a property that holds no matter when the parent updates.

FAIL CLOSED, AND PER RUN. A record whose commit cannot be established is
`unknown`, never `current` -- "I could not check" must not read as "it matches".
Reporting names each run, so one drifted run names itself instead of casting
doubt over all of them.

ONLY RUNNING RUNS ARE DRIFT. A finished run that started on older tooling is
history, not an exposure, and reporting it would make this fire constantly --
a check that fires on everything is ignored within a week.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Iterable, Sequence


ROOT = Path(__file__).resolve().parents[2]
RUNS_DIR = ROOT / "ignored" / "validate" / "runs"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
#: A run in any of these states is still able to invoke its tooling again.
LIVE_STATES = frozenset({"preparing", "running", "planned"})


def parent_head(root: Path = ROOT) -> str | None:
    """The commit the parent working tree is on, or None if unknowable."""

    try:
        completed = subprocess.run(
            ["git", "--no-optional-locks", "-C", str(root), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    head = completed.stdout.strip()
    return head if SHA_RE.match(head) else None


def assess_records(records: Iterable[tuple[str, dict]], head: str | None) -> list[dict]:
    """One verdict per live run. Pure, so both directions are testable."""

    verdicts = []
    for name, record in records:
        state = str(record.get("state", "")).strip()
        if state not in LIVE_STATES:
            continue
        recorded = record.get("parent_checkout_head")
        agent = record.get("agent")
        target = record.get("target")
        if head is None:
            verdict, detail = "unknown", "the parent checkout's HEAD could not be read"
        elif not isinstance(recorded, str) or not SHA_RE.match(recorded):
            verdict, detail = (
                "unknown",
                "this run recorded no usable parent_checkout_head, so the tooling "
                "it started with cannot be established",
            )
        elif recorded != head:
            verdict, detail = (
                "drifted",
                f"started with tooling at {recorded[:12]}, the parent checkout is "
                f"now at {head[:12]}; a later invocation in this run uses different "
                f"code than its earlier ones",
            )
        else:
            verdict, detail = "current", f"tooling at {recorded[:12]} matches the tree"
        verdicts.append(
            {
                "run": name,
                "agent": agent,
                "target": target,
                "state": state,
                "recorded_parent_checkout_head": recorded,
                "parent_checkout_head_now": head,
                "verdict": verdict,
                "detail": detail,
            }
        )
    return verdicts


def load_records(runs_dir: Path) -> list[tuple[str, dict]]:
    out = []
    if not runs_dir.is_dir():
        return out
    for path in sorted(runs_dir.glob("*.json")):
        try:
            value = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            # An unreadable record is reported as unknown rather than skipped:
            # a record we cannot parse is exactly the case that must not read
            # as "nothing to see".
            out.append((path.name, {"state": "running"}))
            continue
        if isinstance(value, dict):
            out.append((path.name, value))
    return out


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Report validate runs whose tooling commit differs from the parent "
            "checkout's current HEAD."
        )
    )
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--runs-dir", type=Path, default=RUNS_DIR)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)

    head = parent_head(args.root)
    verdicts = assess_records(load_records(args.runs_dir), head)
    drifted = [v for v in verdicts if v["verdict"] == "drifted"]
    unknown = [v for v in verdicts if v["verdict"] == "unknown"]

    if args.json:
        print(
            json.dumps(
                {
                    "schema_version": 1,
                    "parent_checkout_head": head,
                    "live_runs": len(verdicts),
                    "drifted": len(drifted),
                    "unknown": len(unknown),
                    "verdicts": verdicts,
                },
                sort_keys=True,
            )
        )
    else:
        print(
            f"live validate runs={len(verdicts)} drifted={len(drifted)} "
            f"unknown={len(unknown)} parent={(head or 'unreadable')[:12]}"
        )
        for v in drifted + unknown:
            print(f"  {v['verdict']}: {v['run']} (agent {v['agent']}) -- {v['detail']}")

    if drifted:
        return 1
    if unknown:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
