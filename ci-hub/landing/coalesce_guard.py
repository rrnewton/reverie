#!/usr/bin/env python3
"""Refuse to coalesce a change whose PR was closed ON THE MERITS.

WHY THIS EXISTS
---------------
A coalesce wave selects constituents by "ready and conflict-free" (see
`agent-utils/skills/pr-landing-operations/SKILL.md`, "Choose a landing shape",
step 2). Neither property says anything about whether a change was DELIBERATELY
REFUSED. The constituent list is hand-written into the wave's body -- PR #1633
enumerated 23 numbers in prose -- and nothing re-checks that list against PR
state at merge time, so a closed PR folded into a staging branch lands silently
under the wave's single approval.

THE TRAP THIS GUARD IS BUILT AROUND: "closed" does not mean "refused". Measured
on rrnewton/hermit 2026-08-07 over all 128 closed-not-merged PRs, the CLOSED
state is dominated by successful outcomes -- 54 carry a named successor and are
safe to fold. Blocking every closed PR would be as useless as blocking none; it
would stall the normal post-coalesce cleanup, where constituents are closed a
few minutes AFTER the wave lands (#1633's 23 were all closed 05:31:45-05:33:03
against a 05:28:47 merge, each with "LANDED via coalesce batch-2 (PR #1633,
squash mergeCommit b7f9c713...)").

The discriminator is what the close says, not that it happened:

  SUPERSEDED  names a successor -- "#1913", "landed via #1633", "superseded by
              main 4b9202c2", "CLOSED WITH NAMED SUCCESSOR". The work survives
              somewhere identifiable, so folding it is at worst redundant.
  MERITS      says the change is not to be landed -- "Closing this
              duplicate/vacuous aggregate WITHOUT LANDING" (#1726, #1701),
              "Closing without landing after exact-head adversarial audit"
              (#1641), "Closing this unsafe aggregate without landing"
              (#1672/#1670/#1668). Re-landing it reverses an explicit decision.

FAIL CLOSED ON SILENCE. A close with no comment, or a comment naming no
successor, is UNKNOWN -- and UNKNOWN is refused, not allowed. 53 of the 128 are
in that state. The asymmetry is the whole argument: wrongly refusing costs one
human sentence naming the successor, while wrongly allowing re-lands work an
owner spent review effort rejecting, inside a batch whose single approval hides
it. This mirrors the task's own predicate: refuse any change whose PR was closed
WITHOUT A NAMED SUCCESSOR.

Note what this guard does NOT claim: it does not verify that the named successor
actually landed. It proves "an owner pointed somewhere else", not "the work is
on main" -- that second claim belongs to ancestry verification, and conflating
them would be the same proxy-binding error this codebase keeps hitting.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


class Disposition(Enum):
    """Why a candidate may or may not be folded into a coalesce wave."""

    ALLOW_OPEN = "allow: open PR, ordinary candidate"
    ALLOW_MERGED = "allow: already merged"
    ALLOW_SUPERSEDED = "allow: closed with a named successor"
    REFUSE_MERITS = "refuse: closed on the merits"
    REFUSE_UNKNOWN = "refuse: closed with no named successor"


#: An identifiable successor: a PR reference or a commit-ish (>=7 hex).
_SUCCESSOR = re.compile(r"#\d{2,6}|\b[0-9a-f]{7,40}\b")

#: Language that states the change is not to be landed. "without landing" is the
#: house phrase and carries most of the weight; the rest are observed variants.
_MERITS = re.compile(
    r"without landing"
    r"|not landing|do not land|won'?t land|will not land"
    r"|clos\w*\s+(?:this\s+)?\w*\s*(?:as\s+)?reject"
    r"|reject(?:ed|ing)?\s+on\s+the\s+merits"
    r"|declin(?:e|ed|ing)",
    re.I,
)

#: Language that points the work somewhere else.
_SUPERSEDED = re.compile(
    r"supersed\w*|landed via|folded into|replaced by|already landed"
    r"|named successor|duplicate of|successor",
    re.I,
)


@dataclass(frozen=True)
class Candidate:
    """A prospective coalesce constituent, as the wave author listed it."""

    number: int
    state: str  # OPEN | CLOSED | MERGED
    merged: bool = False
    close_comment: str = ""


@dataclass(frozen=True)
class Verdict:
    number: int
    disposition: Disposition
    reason: str

    @property
    def allowed(self) -> bool:
        return self.disposition.name.startswith("ALLOW")


def classify(candidate: Candidate) -> Verdict:
    """Decide whether one candidate may be folded into a wave.

    Pure and injectable so the policy is testable without a network or a live
    PR -- a guard whose own logic is untested can pass for the wrong reason.
    """
    state = (candidate.state or "").strip().upper()
    if candidate.merged or state == "MERGED":
        return Verdict(candidate.number, Disposition.ALLOW_MERGED, "already merged")
    if state == "OPEN":
        return Verdict(candidate.number, Disposition.ALLOW_OPEN, "open")

    body = candidate.close_comment or ""
    merits = _MERITS.search(body)
    if merits:
        # MERITS OUTRANKS A SUCCESSOR MENTION. "Closing this duplicate/vacuous
        # aggregate without landing" also matches "duplicate", and a refusal
        # that a stray word can downgrade to ALLOW is not a refusal.
        return Verdict(
            candidate.number,
            Disposition.REFUSE_MERITS,
            f"closed on the merits: {merits.group(0)!r}",
        )
    if _SUPERSEDED.search(body) and _SUCCESSOR.search(body):
        return Verdict(
            candidate.number,
            Disposition.ALLOW_SUPERSEDED,
            f"named successor {_SUCCESSOR.search(body).group(0)}",
        )
    return Verdict(
        candidate.number,
        Disposition.REFUSE_UNKNOWN,
        "closed with no named successor; fail closed",
    )


#: Hand-classified dispositions for closes whose comment alone is not decisive.
#: Written by reading the FULL thread; see the file header for the method.
_ANNOTATIONS = Path(__file__).with_name("closed_pr_dispositions.tsv")


def load_annotations(path: Path | None = None) -> dict[int, tuple[str, str, str]]:
    """Read the hand-classified dispositions as {pr: (disposition, successor, reason)}.

    Kept as data rather than more regex because the remaining cases are not
    pattern-matchable: #1626 is superseded in one half and REJECTED in the other,
    #1637 names a successor that was itself closed on the merits, and #1703's
    closing rationale sits above a later reply. A human read those; the guard
    should consume that judgement, not re-derive it badly.
    """
    src = path or _ANNOTATIONS
    out: dict[int, tuple[str, str, str]] = {}
    if not src.exists():
        return out
    for line in src.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 4:
            continue
        out[int(parts[0])] = (parts[1], parts[2], parts[3])
    return out


def classify_with_annotations(candidate: Candidate, annotations=None) -> Verdict:
    """`classify`, but let a hand-classified disposition settle an UNKNOWN.

    An annotation may only resolve an UNKNOWN. It can never overturn a
    REFUSE_MERITS derived from the close text itself -- otherwise a stale or
    mistaken row in a data file could re-authorise work an owner rejected, which
    is the exact failure this guard exists to prevent.
    """
    verdict = classify(candidate)
    if verdict.disposition is not Disposition.REFUSE_UNKNOWN:
        return verdict
    ann = load_annotations() if annotations is None else annotations
    row = ann.get(candidate.number)
    if not row:
        return verdict
    disposition, successor, reason = row
    if disposition == "SUPERSEDED":
        return Verdict(
            candidate.number,
            Disposition.ALLOW_SUPERSEDED,
            f"annotated successor {successor or '(unnamed)'}: {reason}",
        )
    if disposition == "MERITS":
        return Verdict(
            candidate.number, Disposition.REFUSE_MERITS, f"annotated merits: {reason}"
        )
    return Verdict(candidate.number, Disposition.REFUSE_UNKNOWN, f"annotated: {reason}")


def screen(candidates, use_annotations: bool = True):
    """Split a proposed constituent list into (allowed, refused) verdicts."""
    decide = classify_with_annotations if use_annotations else classify
    verdicts = [decide(c) for c in candidates]
    return (
        [v for v in verdicts if v.allowed],
        [v for v in verdicts if not v.allowed],
    )


# ---------------------------------------------------------------------------
# Operator entry point.
#
# The guard existed for a day with NO CALLER: correct, tested, never invoked --
# the same shape as a lint that covers three of six backends. A wave is
# assembled BY HAND (see `pr-landing-operations`, "Choose a landing shape"), so
# the only place this can fire is an operator step run before staging. This CLI
# is that step: give it the constituent list, get a nonzero exit if any of them
# was closed on the merits.
# ---------------------------------------------------------------------------

REFUSE_EXIT = 3


def _fetch(numbers, repo: str, gh_cmd: str, wrap: str = ""):
    """Fetch state + last comment for each constituent via ONE GraphQL call.

    One call, not one per PR: a wave is 20+ constituents and the operator is
    holding the landing lock while this runs.

    The query goes through a FILE (`-F query=@path`) rather than an inline
    argument, so the composed command contains no quotes and survives being
    handed to a wrapper as a single string. That matters because the egress
    path on this box (`herdr-run --agent X "<command>"`) takes exactly one
    command argument, and inline GraphQL was being torn apart by the shell.
    """
    import json
    import shlex
    import subprocess
    import tempfile

    fields = " ".join(
        f"a{n}: pullRequest(number:{n}) {{ number state merged: mergedAt "
        f"comments(last:1){{nodes{{body}}}} }}"
        for n in numbers
    )
    owner, _, name = repo.partition("/")
    query = f'query {{ repository(owner:"{owner}", name:"{name}") {{ {fields} }} }}'
    with tempfile.NamedTemporaryFile("w", suffix=".graphql", delete=False) as handle:
        handle.write(query)
        query_path = handle.name
    composed = f"{gh_cmd} api graphql -F query=@{query_path}"
    argv = shlex.split(wrap) + [composed] if wrap else shlex.split(composed)
    proc = bounded_capture.capture(argv)
    if proc.returncode != 0:
        raise SystemExit(
            f"coalesce-guard: cannot reach GitHub: {proc.stderr.strip()[:400]}"
        )
    body = "\n".join(
        l for l in proc.stdout.splitlines() if not l.startswith("[herdr-run]")
    )
    data = json.loads(body)["data"]["repository"]
    out = []
    for n in numbers:
        rec = data.get(f"a{n}")
        if rec is None:
            # A number the operator typed that does not resolve is a REFUSAL,
            # not a skip: silently dropping it would stage an unscreened change.
            out.append(Candidate(number=n, state="CLOSED", close_comment=""))
            continue
        nodes = rec["comments"]["nodes"]
        out.append(
            Candidate(
                number=rec["number"],
                state=rec["state"],
                merged=bool(rec.get("merged")),
                close_comment=nodes[0]["body"] if nodes else "",
            )
        )
    return out


def main(argv=None) -> int:
    import argparse
    import json

    parser = argparse.ArgumentParser(
        prog="coalesce-guard",
        description="Refuse to stage a coalesce wave containing a change whose PR "
        "was closed ON THE MERITS. Run BEFORE building the staging branch.",
    )
    parser.add_argument(
        "constituents",
        nargs="*",
        type=int,
        help="PR numbers the wave would fold in",
    )
    parser.add_argument("--repo", default="rrnewton/hermit")
    parser.add_argument(
        "--gh-cmd",
        default="gh",
        help="how to invoke gh, e.g. 'with-proxy gh'",
    )
    parser.add_argument(
        "--wrap",
        default="",
        help="wrapper that takes the whole gh command as ONE argument, e.g. "
        "'herdr-run --agent <you>' for the sandbox egress path",
    )
    parser.add_argument(
        "--from-json",
        help="read pre-fetched records instead of calling GitHub; the offline "
        "path used by the tests so the policy is provable without a network",
    )
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args(argv)

    if args.from_json:
        raw = json.loads(Path(args.from_json).read_text())
        candidates = [
            Candidate(
                number=int(r["number"]),
                state=r.get("state", "CLOSED"),
                merged=bool(r.get("merged")),
                close_comment=r.get("close_comment", ""),
            )
            for r in raw
        ]
    elif args.constituents:
        candidates = _fetch(args.constituents, args.repo, args.gh_cmd, args.wrap)
    else:
        parser.error("give constituent PR numbers, or --from-json")

    allowed, refused = screen(candidates)
    if args.json:
        print(
            json.dumps(
                {
                    "allowed": [{"pr": v.number, "why": v.reason} for v in allowed],
                    "refused": [
                        {"pr": v.number, "disposition": v.disposition.name,
                         "why": v.reason}
                        for v in refused
                    ],
                },
                indent=2,
            )
        )
    else:
        for v in allowed:
            print(f"  ALLOW  #{v.number}  {v.reason}")
        for v in refused:
            print(f"  REFUSE #{v.number}  [{v.disposition.name}] {v.reason}")
        print(f"\n{len(allowed)} allowed, {len(refused)} refused")
    if refused:
        print(
            "\ncoalesce-guard: REFUSING this wave. Drop the refused constituents and "
            "re-run.\nAn UNKNOWN means the close is unannotated, not that it is safe: "
            "read the\nclose, add a row to closed_pr_dispositions.tsv, and re-run. Do "
            "not disable the\nguard to get past it.",
        )
        return REFUSE_EXIT
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
