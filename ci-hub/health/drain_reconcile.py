#!/usr/bin/env python3
"""Detect open PRs that no landing tracker is accounting for.

WHY THIS IS THE FIRST THING BUILT, not the last. Filing a PR is the moment work
enters the implemented-but-not-landed state, so filing is the event that must
extend the landing tracker. Two mechanisms can enforce that coupling -- a
wrapper script around PR creation, or a git hook -- and BOTH ARE BYPASSABLE. An
agent can call ``gh pr create`` directly; ``.git/hooks`` is per-clone and
un-versioned, so a fresh clone silently has none. A prevention with no detector
is indistinguishable from a working one. This is the detector, and it is also
the only mechanism that says anything about PRs filed BEFORE the coupling
existed.

THE FAILURE THIS EXISTS TO CATCH IS LIVE. At the time of writing, coverage was
0 of 133 open PRs across four repositories: the tracker's newest reference was
``hermit#1840`` while open PRs ran to ``#1876``. The reference ranges OVERLAP
(tracker 1120..1840, open 1665..1876), so the empty intersection is a real gap
and not an artifact of comparing disjoint eras.

DESIGN CONSTRAINT LEARNED FROM THAT MEASUREMENT. A reconciler that emitted one
finding per unrepresented PR would have emitted 133 on its first run and been
switched off the same day. So a STRUCTURAL fault -- the tracker missing, or
closed, or unparseable -- is reported as ONE finding that explains why coverage
is zero, and the per-PR list is suppressed behind ``--list-gaps``. Per-PR
findings are for the steady state, where they are few and each is actionable.

LOUDNESS DISCIPLINE, inherited from ``pr_status.py`` for the same reason: no
output plus a zero exit is indistinguishable from "everything is tracked". A
query that fails or is blocked is ALWAYS reported as UNAVAILABLE with a reason
and a non-zero exit, never as an empty-but-green report.

Exit codes:
  0  every open PR is represented, and the tracker is healthy
  1  gaps found (tracker healthy, but some open PRs are unrepresented)
  2  UNAVAILABLE -- a required authority could not be read, so the answer is
     unknown. NEVER conflated with 0.
  3  STRUCTURAL -- the tracker itself is missing or terminal, so coverage is
     meaningless rather than zero.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
# One resolver for the TaskGraph, shared with every other ci-hub consumer.
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ci-hub" / "lib"))
import taskgraph_db  # noqa: E402
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from provenance import query_provenance, require_fresh  # noqa: E402

# The tracker is named, not discovered, because it is a single designated
# authority. If it is ever renamed, this must fail loudly rather than silently
# reconcile against nothing -- which is precisely the STRUCTURAL exit.
DEFAULT_TRACKER = "drain-implemented-to-landed"

# Repositories whose open PRs create landing debt. Derived from a default here
# rather than hardcoded at a call site so a new repo is added in ONE place.
DEFAULT_REPOS = ("hermit", "reverie", "agent-utils", "dev-hermit", "liteinst2")
DEFAULT_OWNER = "rrnewton"

# A task status that still tracks work. Anything else means the tracker has
# stopped tracking, whatever its tags claim.
NONTERMINAL = {"OPEN", "IN_PROGRESS", "BACKLOG"}

_PR_URL = re.compile(r"github\.com/[\w.-]+/([\w.-]+)/pull/(\d+)")
_PR_SHORT = re.compile(r"\b(" + "|".join(DEFAULT_REPOS) + r")#(\d+)\b")


class Unavailable(Exception):
    """A required authority could not be read. Never downgraded to 'no gaps'."""


def _run(cmd: list[str], timeout: int) -> str:
    env = None
    if cmd and cmd[0] == "tg":
        # Unbound, `tg` reads an empty default and succeeds with 0 rows, which
        # this reconciler would read as "the tracker has no entries" -- exactly
        # the downgrade to "no gaps" the Unavailable class exists to prevent.
        try:
            env = taskgraph_db.child_env(taskgraph_db.resolve())
        except taskgraph_db.TaskGraphUnavailable as exc:
            raise Unavailable(str(exc)) from exc
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env)
    except subprocess.TimeoutExpired as exc:
        raise Unavailable(f"timed out after {timeout}s: {' '.join(cmd[:4])}") from exc
    except OSError as exc:
        raise Unavailable(f"could not execute {cmd[0]}: {exc}") from exc
    if p.returncode != 0:
        raise Unavailable(f"exit {p.returncode} from {' '.join(cmd[:4])}: {p.stderr.strip()[:200]}")
    return p.stdout


def read_tracker(tracker: str, timeout: int) -> tuple[str, str]:
    """Return (status, full text) for the tracker task.

    The text is description plus every note, because an entry may be added
    either way and a reconciler that read only one would invent gaps.
    """
    raw = _run(["tg", "show", tracker, "--json"], timeout)
    try:
        doc = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise Unavailable(f"tracker {tracker!r} did not return JSON: {exc}") from exc
    text = (doc.get("description") or "") + "\n"
    text += "\n".join(n.get("text", "") for n in doc.get("notes", []))
    return (doc.get("status") or "UNKNOWN"), text


def tracked_refs(text: str) -> set[tuple[str, int]]:
    """Every (repo, number) the tracker mentions, in either notation."""
    refs = {(m.group(1), int(m.group(2))) for m in _PR_URL.finditer(text)}
    refs |= {(m.group(1), int(m.group(2))) for m in _PR_SHORT.finditer(text)}
    return refs


OPEN_PR_LIMIT = 500


def open_prs(owner: str, repo: str, timeout: int) -> list[dict]:
    raw = _run(
        ["with-proxy", "gh", "pr", "list", "--repo", f"{owner}/{repo}", "--state", "open",
         # An explicit limit is mandatory: gh's default of 30 silently truncates
         # and would under-report the gap as a clean-looking number.
         "--limit", str(OPEN_PR_LIMIT), "--json",
         "number,title,url,createdAt,isDraft"],
        timeout,
    )
    try:
        listed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise Unavailable(f"{owner}/{repo}: gh did not return JSON: {exc}") from exc

    # ⚠️ THE EXPLICIT LIMIT MOVED THE CLIFF; IT DID NOT REMOVE IT. The comment
    # above records why the default 30 was refused -- it "would under-report the
    # gap as a clean-looking number" -- and 500 fails the same way, just later.
    # `returned == limit` is the only signal a cap was hit, and nothing compared
    # them, so at 500 open pull requests this reports a SMALLER drain gap with no
    # error and no warning: a truncated query answering "is anything missing?"
    # with a confident, well-formed, wrong "no".
    #
    # This module already states the rule for its own OUTPUT further down --
    # "Never truncate silently: a capped list that does not say it was capped".
    # Applying it to its own INPUT is that same rule.
    require_fresh(
        query_provenance(
            f"gh pr list -R {owner}/{repo} --state open",
            limit=OPEN_PR_LIMIT,
            returned=len(listed),
        )
    )
    return listed


def _age_hours(created: str | None) -> float | None:
    if not created:
        return None
    try:
        t = datetime.fromisoformat(created.replace("Z", "+00:00"))
    except ValueError:
        return None
    return (datetime.now(timezone.utc) - t).total_seconds() / 3600.0


def _age_sort_key(gap: dict) -> tuple[int, float]:
    """Oldest first; an unknown age sorts LAST.

    A missing timestamp must not be able to masquerade as the oldest item and
    jump the queue, so it is ranked behind every PR whose age is known.
    """
    age = gap["age_hours"]
    return (1, 0.0) if age is None else (0, -age)


def reconcile(owner: str, repos: tuple[str, ...], tracker: str, timeout: int) -> dict:
    status, text = read_tracker(tracker, timeout)
    refs = tracked_refs(text)

    gaps, examined, unreachable = [], 0, []
    for repo in repos:
        try:
            prs = open_prs(owner, repo, timeout)
        except Unavailable as exc:
            # Partial results are reported WITH the hole named. Dropping the
            # repo silently would shrink the denominator and make the gap look
            # smaller than it is.
            unreachable.append(f"{repo}: {exc}")
            continue
        examined += len(prs)
        for pr in prs:
            if (repo, pr["number"]) in refs:
                continue
            gaps.append({
                "repo": repo, "number": pr["number"], "url": pr["url"],
                "title": pr["title"], "draft": pr.get("isDraft", False),
                "age_hours": _age_hours(pr.get("createdAt")),
            })
    gaps.sort(key=_age_sort_key)
    return {
        "tracker": tracker, "tracker_status": status,
        "tracker_healthy": status in NONTERMINAL,
        "tracker_refs": len(refs), "open_examined": examined,
        "gaps": gaps, "unreachable": unreachable,
    }


def report(result: dict, list_gaps: bool, out=sys.stdout) -> int:
    # The heading always prints, so stdout is never empty and a blind run can
    # never be mistaken for a clean one.
    print("=== drain reconcile: open PRs vs the landing tracker ===", file=out)
    print(f"tracker   : {result['tracker']}  status={result['tracker_status']}", file=out)
    print(f"references: {result['tracker_refs']} PR(s) mentioned by the tracker", file=out)
    print(f"examined  : {result['open_examined']} open PR(s)", file=out)

    for hole in result["unreachable"]:
        print(f"UNAVAILABLE: {hole}", file=out)

    if not result["tracker_healthy"]:
        # ONE finding, not one per PR. Coverage is not zero here -- it is
        # undefined, and saying "133 PRs are untracked" would bury the single
        # fact that explains all 133 and is the only actionable one.
        print("", file=out)
        print(f"STRUCTURAL: the tracker is {result['tracker_status']}, not a live tracker.", file=out)
        print("  Every open PR is unrepresented as a CONSEQUENCE of this, so the", file=out)
        print("  per-PR list is suppressed -- it would be one finding restated N times.", file=out)
        print("  Restore the tracker to a non-terminal status, then re-run.", file=out)
        return 3

    if result["unreachable"]:
        print("REFUSED: at least one repository could not be read, so 'no gaps' "
              "cannot be asserted.", file=out)
        return 2

    gaps = result["gaps"]
    if not gaps:
        print("\nOK: every open PR is represented in the tracker.", file=out)
        return 0

    print(f"\nGAP: {len(gaps)} open PR(s) that no tracker entry accounts for, oldest first:", file=out)
    shown = gaps if list_gaps else gaps[:20]
    for g in shown:
        age = "     ?" if g["age_hours"] is None else f"{g['age_hours']:6.1f}"
        draft = " [draft]" if g["draft"] else ""
        print(f"  {age}h  {g['repo']}#{g['number']}{draft}  {g['title'][:60]}", file=out)
    if len(shown) < len(gaps):
        # Never truncate silently: a capped list that does not say it was capped
        # reads as complete coverage.
        print(f"  ... {len(gaps) - len(shown)} more suppressed; pass --list-gaps for all.", file=out)
    return 1


def self_test(out=sys.stdout) -> int:
    """Both directions, on inert fixtures.

    A detector shown only to fire could be firing unconditionally, and one shown
    only to stay quiet could be inert. Neither half alone is evidence, so both
    are asserted here and the counts are printed.
    """
    print("=== self-test: both directions ===", file=out)
    failures = []

    text = "landed via https://github.com/rrnewton/hermit/pull/1840 and reverie#362"
    refs = tracked_refs(text)
    if refs != {("hermit", 1840), ("reverie", 362)}:
        failures.append(f"parser: expected 2 refs in both notations, got {refs}")
    else:
        print("  PASS parser recovers both URL and short notation (2/2 refs)", file=out)

    # NEGATIVE direction: a represented PR must NOT be flagged.
    represented = {"repo": "hermit", "number": 1840}
    if (represented["repo"], represented["number"]) not in refs:
        failures.append("negative: a tracked PR was treated as a gap")
    else:
        print("  PASS a represented PR is not flagged (1 planted, 0 flagged)", file=out)

    # POSITIVE direction: an unrepresented PR MUST be flagged.
    planted = ("hermit", 999999)
    if planted in refs:
        failures.append("positive: a planted unrepresented PR was treated as tracked")
    else:
        print("  PASS an unrepresented PR is flagged (1 planted, 1 flagged)", file=out)

    # A structural fault must short-circuit rather than emit N per-PR findings.
    structural = {
        "tracker": "t", "tracker_status": "CLOSED", "tracker_healthy": False,
        "tracker_refs": 0, "open_examined": 133,
        "gaps": [{"repo": "hermit", "number": n, "url": "", "title": "x",
                  "draft": False, "age_hours": 1.0} for n in range(133)],
        "unreachable": [],
    }
    import io
    buf = io.StringIO()
    rc = report(structural, list_gaps=False, out=buf)
    if rc != 3:
        failures.append(f"structural: expected rc=3, got {rc}")
    elif "hermit#0" in buf.getvalue():
        failures.append("structural: per-PR findings leaked past the structural short-circuit")
    else:
        print("  PASS a closed tracker yields ONE structural finding, not 133", file=out)

    # An unknown age must sort last, never first.
    ordered = sorted(
        [{"age_hours": None}, {"age_hours": 5.0}, {"age_hours": 100.0}], key=_age_sort_key
    )
    if ordered[0]["age_hours"] != 100.0 or ordered[-1]["age_hours"] is not None:
        failures.append(f"age sort: unknown age did not sort last: {ordered}")
    else:
        print("  PASS oldest-first, and an unknown age sorts last (3 items)", file=out)

    # A blind run must never be reported as clean.
    blind = {"tracker": "t", "tracker_status": "OPEN", "tracker_healthy": True,
             "tracker_refs": 5, "open_examined": 0, "gaps": [],
             "unreachable": ["hermit: exit 1 from gh"]}
    buf = io.StringIO()
    rc = report(blind, list_gaps=False, out=buf)
    if rc != 2:
        failures.append(f"blind: an unreadable repo returned rc={rc}, not 2")
    else:
        print("  PASS an unreadable repo is UNAVAILABLE (rc=2), not 'no gaps'", file=out)

    for f in failures:
        print(f"  FAIL {f}", file=out)
    print(f"\nself-test: {6 - len(failures)}/6 passed", file=out)
    return 1 if failures else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--tracker", default=DEFAULT_TRACKER)
    ap.add_argument("--owner", default=DEFAULT_OWNER)
    ap.add_argument("--repos", default=",".join(DEFAULT_REPOS),
                    help="comma-separated repository names")
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--list-gaps", action="store_true", help="print every gap, uncapped")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()

    repos = tuple(r.strip() for r in args.repos.split(",") if r.strip())
    try:
        result = reconcile(args.owner, repos, args.tracker, args.timeout)
    except Unavailable as exc:
        print("=== drain reconcile: open PRs vs the landing tracker ===")
        print(f"UNAVAILABLE: {exc}")
        print("REFUSED: the tracker could not be read, so nothing can be asserted "
              "about coverage.")
        return 2

    if args.json:
        print(json.dumps(result, indent=2))
        return 0 if (result["tracker_healthy"] and not result["gaps"]
                     and not result["unreachable"]) else 1
    return report(result, args.list_gaps)


if __name__ == "__main__":
    sys.exit(main())
