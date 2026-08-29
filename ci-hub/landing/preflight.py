#!/usr/bin/env python3
"""THE LANDING PREFLIGHT — the checks that must pass before trusting any green.

Every check here is a defect that ACTUALLY FIRED on 2026-08-04. This file exists
because the rules were being retyped into agent dispatches by hand, and a rule
that gets retyped is a rule that decays. Here they are executable, and each has a
negative test proving it refuses the bad case.

  1. THE SHA YOU WERE HANDED IS A CACHE; THE BRANCH IS THE SOURCE.
     Four handed SHAs went stale in one night, one of them quoted into agent
     instructions for hours after main advanced twice.

  2. A GREEN MUST CARRY A NONZERO EXECUTED TEST COUNT IN ITS TYPED ROW.
     `--features` gating produces a build that succeeds, a target that runs, ZERO
     tests executed, and a reported SUCCESS. Proven by planting a feature-gated
     fixture. N==0 is a no-result wearing a success badge.
     The previously-recorded KNOWN GAP -- "an EMPTY log still passes" -- is
     CLOSED here: an absent row and a missing, zero, negative, malformed, or
     wrong-commit count are all non-passing. Human log text is not consulted.

  3. LANDING IS VERIFIED BY ANCESTRY, NEVER BY THE `MERGED` FLAG.
     Ancestry must be tested on `mergeCommit.oid`, never the PR head: after a
     rebase replay the head is NEVER an ancestor, and that misread 79 PRs as
     unlanded when 46 had landed. `MERGED` alone is also insufficient -- a later
     force-push orphans the replay SHA, which happened to ~12 PRs on 2026-08-03.

  Plus two standing traps that cost real time:
  4. An uncommitted `[patch."...reverie.git"]` override in a slot's Cargo.toml.
  5. A byte-identical branch already on the remote before you open anything.

DESIGN: every check is a PURE FUNCTION over data that is passed in, and the I/O
that fetches that data lives in thin, separately-testable collectors. That is
deliberate -- it is what lets the negative tests run with no network, and it
keeps each refusal reproducible from the recorded inputs rather than from a live
API that has since moved on.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Callable, Optional, Sequence

PASS = "PASS"
REFUSE = "REFUSE"
UNKNOWN = "UNKNOWN"


@dataclass
class CheckResult:
    check: str
    verdict: str
    reason: str
    detail: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return self.verdict == PASS

    def as_dict(self) -> dict:
        return {
            "check": self.check,
            "verdict": self.verdict,
            "reason": self.reason,
            **({"detail": self.detail} if self.detail else {}),
        }


# --------------------------------------------------------------- 1. SHA is a cache

def check_sha_is_current(handed_sha: str, live_head: Optional[str]) -> CheckResult:
    """The handed SHA must STILL be the PR's head.

    `live_head is None` means the head could not be resolved. That is UNKNOWN,
    not PASS: an unanswerable question must never read as a satisfied one, which
    is the whole failure mode this preflight exists to stop.
    """
    name = "sha-is-current"
    if not handed_sha:
        return CheckResult(name, REFUSE, "no SHA was handed; nothing to verify")
    if live_head is None:
        return CheckResult(
            name, UNKNOWN,
            "could not resolve the live head (network/gh unavailable) -- "
            "UNKNOWN is not PASS; resolve it before acting",
            {"handed": handed_sha},
        )
    if not re.fullmatch(r"[0-9a-f]{40}", handed_sha.lower()):
        return CheckResult(
            name, REFUSE,
            "handed SHA is not a full 40-hex commit; a prefix cannot be compared safely",
            {"handed": handed_sha},
        )
    if handed_sha.lower() != live_head.lower():
        return CheckResult(
            name, REFUSE,
            "the handed SHA is STALE -- the branch has moved",
            {"handed": handed_sha, "live_head": live_head},
        )
    return CheckResult(name, PASS, "handed SHA is still the PR head", {"head": live_head})


# ------------------------------------------------- 2. green carries executed tests

TypedCountReader = Callable[[str, str], tuple[int, str, str]]


def _read_typed_executed_tests(event_path: str, sha: str) -> tuple[int, str, str]:
    """Delegate ledger-event and HistoryRow parsing to their Rust types."""
    reader = Path(__file__).resolve().parents[1] / "ci-hub.rs"
    try:
        completed = subprocess.run(
            [
                str(reader),
                "receipt-digest",
                "--sha",
                sha,
                "--input",
                event_path,
                "--ledger-event",
                "--json",
                "--require-executed-tests",
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
    except (OSError, subprocess.SubprocessError) as error:
        return 2, "", str(error)
    return completed.returncode, completed.stdout.strip(), completed.stderr.strip()


def check_green_carries_executed_tests(
    event_path: Optional[str],
    sha: Optional[str],
    *,
    typed_reader: Optional[TypedCountReader] = None,
) -> CheckResult:
    """A green must prove nonzero execution through the shared HistoryRow.

    The Python consumer deliberately does not interpret ``executed_tests``.
    Rust deserializes Hermit's producer-owned type and owns the decision. Exit
    0 means the typed row carries a positive count, exit 1 means the row itself
    refuses, and every other result is an unreadable authority. None may fall
    back to a still-present human banner.
    """
    name = "green-carries-executed-tests"
    if not event_path:
        return CheckResult(
            name,
            REFUSE,
            "typed validation ledger event is ABSENT -- no evidence is not a pass",
        )
    if not sha:
        return CheckResult(
            name,
            REFUSE,
            "receipt SHA is ABSENT -- the typed row cannot be bound to the candidate",
            {"ledger_event": event_path},
        )
    if not Path(event_path).is_file():
        return CheckResult(
            name,
            REFUSE,
            "typed validation ledger event is ABSENT -- no evidence is not a pass",
            {"ledger_event": event_path, "sha": sha},
        )
    status, output, error = (typed_reader or _read_typed_executed_tests)(event_path, sha)
    detail = {"ledger_event": event_path, "sha": sha}
    if output:
        try:
            detail["typed_reader"] = json.loads(output)
        except json.JSONDecodeError:
            return CheckResult(
                name,
                UNKNOWN,
                "shared HistoryRow reader emitted malformed JSON",
                {**detail, "stdout": output},
            )
    if status == 0:
        return CheckResult(
            name,
            PASS,
            "shared HistoryRow carries a positive executed-test count",
            detail,
        )
    if status == 1:
        return CheckResult(
            name,
            REFUSE,
            "shared HistoryRow carries no positive executed-test count",
            detail,
        )
    return CheckResult(
        name,
        UNKNOWN,
        "shared HistoryRow reader failed; prose is not a fallback",
        {**detail, **({"reader_error": error} if error else {})},
    )


# ------------------------------------------------------- 3. landing by ancestry

def check_landed_by_ancestry(
    *,
    pr_state: str,
    merge_commit_oid: Optional[str],
    is_ancestor: Callable[[str], Optional[bool]],
    fetched_fresh: bool,
    pr_head: Optional[str] = None,
) -> CheckResult:
    """Landing is proven by `mergeCommit.oid` ancestry on a freshly-fetched main.

    Three ways this is deliberately stricter than the obvious implementation:
      * it REFUSES to answer on a stale remote -- a stale ref answers about the past;
      * it never accepts `MERGED` alone -- a later force-push orphans the replay SHA;
      * it never tests the PR HEAD, which after a rebase replay is never an
        ancestor and produced the 79-vs-46 misread.
    """
    name = "landed-by-ancestry"
    if not fetched_fresh:
        return CheckResult(
            name, UNKNOWN,
            "remote was not freshly fetched -- a stale ref answers about the past",
        )
    if str(pr_state).upper() != "MERGED":
        return CheckResult(name, REFUSE, f"PR state is {pr_state!r}, not MERGED")
    if not merge_commit_oid:
        return CheckResult(
            name, REFUSE,
            "PR is MERGED but carries no mergeCommit.oid -- `MERGED` alone is not "
            "landing evidence",
        )
    verdict = is_ancestor(merge_commit_oid)
    if verdict is None:
        return CheckResult(
            name, UNKNOWN, "ancestry could not be determined (commit absent locally?)",
            {"merge_commit": merge_commit_oid},
        )
    if not verdict:
        return CheckResult(
            name, REFUSE,
            "mergeCommit.oid is NOT an ancestor of the freshly-fetched target -- "
            "the replay SHA was orphaned (force-push) or never landed",
            {"merge_commit": merge_commit_oid},
        )
    detail = {"merge_commit": merge_commit_oid}
    if pr_head:
        detail["pr_head_not_used_deliberately"] = pr_head
    return CheckResult(name, PASS, "mergeCommit.oid is an ancestor of target main", detail)


# ------------------------------------------------------------ 4. patch override

_PATCH_SECTION = re.compile(r'^\s*\[patch\s*\.\s*"[^"]*reverie[^"]*"\s*\]', re.MULTILINE)


def check_no_uncommitted_patch_override(diff_text: str) -> CheckResult:
    """A `[patch."...reverie.git"]` override must never ride along in a commit.

    Keyed on ADDED lines of the diff, so an override already committed upstream
    (not this agent's doing) does not fire, while one being introduced does.
    """
    name = "no-uncommitted-patch-override"
    added = "\n".join(
        line[1:] for line in diff_text.splitlines()
        if line.startswith("+") and not line.startswith("+++")
    )
    if _PATCH_SECTION.search(added):
        return CheckResult(
            name, REFUSE,
            "an uncommitted [patch.\"...reverie.git\"] override is in the diff -- "
            "it would redirect the dependency for everyone who builds this commit",
        )
    return CheckResult(name, PASS, "no reverie patch override in the staged diff")


# --------------------------------------------------------- 5. duplicate branch

def check_no_byte_identical_branch(
    *, candidate_tree: str, remote_trees: dict[str, str]
) -> CheckResult:
    """Refuse to open work that already exists byte-identically on the remote."""
    name = "no-byte-identical-branch"
    matches = sorted(b for b, t in remote_trees.items() if t and t == candidate_tree)
    if matches:
        return CheckResult(
            name, REFUSE,
            f"a byte-identical branch already exists: {', '.join(matches)}",
            {"tree": candidate_tree, "branches": matches},
        )
    return CheckResult(name, PASS, "no byte-identical branch on the remote")


# --------------------------------------------------------------------- collectors

def _run(args: Sequence[str], cwd: Optional[str] = None) -> tuple[int, str]:
    try:
        p = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=120)
        return p.returncode, (p.stdout or "").strip()
    except Exception:
        return 127, ""


def gh_pr_head(pr: int, repo: str) -> Optional[str]:
    rc, out = _run(["gh", "pr", "view", str(pr), "--repo", repo, "--json", "headRefOid",
                    "-q", ".headRefOid"])
    return out if rc == 0 and out else None


def gh_pr_merge_info(pr: int, repo: str) -> tuple[str, Optional[str]]:
    rc, out = _run(["gh", "pr", "view", str(pr), "--repo", repo, "--json",
                    "state,mergeCommit"])
    if rc != 0 or not out:
        return "UNKNOWN", None
    try:
        d = json.loads(out)
    except json.JSONDecodeError:
        return "UNKNOWN", None
    return d.get("state", "UNKNOWN"), (d.get("mergeCommit") or {}).get("oid")


def git_is_ancestor_factory(checkout: str, target: str) -> Callable[[str], Optional[bool]]:
    def is_ancestor(sha: str) -> Optional[bool]:
        rc, _ = _run(["git", "cat-file", "-e", f"{sha}^{{commit}}"], cwd=checkout)
        if rc != 0:
            return None
        rc, _ = _run(["git", "merge-base", "--is-ancestor", sha, target], cwd=checkout)
        return rc == 0
    return is_ancestor


# --------------------------------------------------------------------------- CLI

def run_preflight(args: argparse.Namespace) -> list[CheckResult]:
    results: list[CheckResult] = []

    if args.sha is not None:
        live = None if args.no_network else (
            gh_pr_head(args.pr, args.repo) if args.pr else None
        )
        if args.live_head:                      # injectable for testing / offline use
            live = args.live_head
        results.append(check_sha_is_current(args.sha, live))

    if args.ledger_event is not None or args.receipt_sha is not None:
        results.append(check_green_carries_executed_tests(args.ledger_event, args.receipt_sha))

    if args.landed_pr:
        fetched = False
        if not args.no_network:
            rc, _ = _run(["git", "fetch", "origin", args.target], cwd=args.checkout)
            fetched = rc == 0
        state, oid = ("UNKNOWN", None) if args.no_network else gh_pr_merge_info(
            args.landed_pr, args.repo)
        results.append(check_landed_by_ancestry(
            pr_state=state, merge_commit_oid=oid, fetched_fresh=fetched,
            is_ancestor=git_is_ancestor_factory(args.checkout, f"origin/{args.target}"),
        ))

    if args.diff_of:
        rc, out = _run(["git", "diff", "HEAD"], cwd=args.diff_of)
        results.append(check_no_uncommitted_patch_override(out if rc == 0 else ""))

    return results


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sha", help="the SHA you were handed (check 1)")
    ap.add_argument("--pr", type=int, help="PR number for the live head lookup")
    ap.add_argument("--live-head", help="inject the live head instead of calling gh")
    ap.add_argument(
        "--ledger-event",
        help="validation run.result JSON to check for nonzero executed tests (check 2)",
    )
    ap.add_argument(
        "--receipt-sha",
        help="exact commit the --ledger-event must describe (check 2)",
    )
    ap.add_argument("--landed-pr", type=int, help="PR to verify LANDED by ancestry (check 3)")
    ap.add_argument("--checkout", default=".", help="git checkout for ancestry/diff")
    ap.add_argument("--target", default="main")
    ap.add_argument("--repo", default="rrnewton/hermit")
    ap.add_argument("--diff-of", help="worktree to scan for a patch override (check 4)")
    ap.add_argument("--no-network", action="store_true",
                    help="never call gh/git-fetch; unresolvable checks report UNKNOWN")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    results = run_preflight(args)
    if not results:
        print("landing-preflight: no checks requested; see --help", file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps([r.as_dict() for r in results], indent=2))
    else:
        for r in results:
            print(f"landing-preflight: {r.verdict:<7} {r.check}: {r.reason}")

    # UNKNOWN is NOT success. An unanswerable check must block, or the preflight
    # becomes a way to launder "I could not tell" into "it is fine".
    return 0 if all(r.ok for r in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
