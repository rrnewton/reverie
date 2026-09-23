#!/usr/bin/env python3
"""Refuse-before-start guard: does a head CONTAIN every producer anchor?

THE PROBLEM (memory: validate-producer-version-coupled-rebase-before-greening).
A hermit/reverie checkout runs ITS OWN validate.sh. That producer emits exactly
the receipt fields its commit knew how to emit. The landing / cache consumer
`ci-hub/lib/validate_status.rs::is_clean_full_pass` is FAIL-CLOSED: it requires
`selection_mode=="full" && commit_anchored==true && !tree_dirty` (and, since the
counts tightening, `executed_tests>0 && filtered_tests==0`). A head that PREDATES
the commit that taught validate.sh to emit a required field writes that field
NULL, so EVERY ledger record it produces is rejected -- no matter how full,
green, and exit-0 the run was. PROVEN: PR #1544 @4cdda392 ran full/5-gate/exit-0
yet stayed NOT-VALIDATED (all six records null-fielded); the drain-rebased head
wrote selection_mode=full commit_anchored=True and was ACCEPTED.

So: any consumer-side receipt requirement added after a PR was branched is
unsatisfiable by that PR until it rebases. Running a ~17-minute validate against
such a head is doomed before it starts. This guard makes that a 2-second REFUSE.

WHAT IT DOES. For each required floor in rebase-base-floors.json, decide whether
the target head CONTAINS the anchor commit:
  * local first: `git -C <checkout> merge-base --is-ancestor <anchor> <head>`
    (rc 0 == head contains anchor). Works whenever the head/ref is known locally.
  * fallback: `with-proxy "$FLEET_REAL_GH" api repos/<repo>/compare/<anchor>...<head> --jq
    .status`; status in {ahead, identical} == head contains anchor;
    {behind, diverged} == head PREDATES the anchor.
If ANY anchor is missing -> exit 2 (REFUSED) with a NAMED reason identifying the
specific missing anchor. If all are contained -> exit 0 (OK).

    exit 0  OK        -- head contains all anchors; validate can produce a receipt
    exit 2  REFUSED   -- head predates >=1 anchor; rebase before validating
    exit 3  ERROR     -- could not resolve the head or the anchors file

WIRING. Receipt-producing validation launchers call the composite
`preflight_validate.py`, which combines this fixed-floor authority with a fresh
moving-origin/main ancestry check. `reconcile_receipts.py` calls this module
directly because it is classifying historical receipts against fixed floors,
not authorizing a new validation run.

Usage:
  preflight_anchor.py [--head <sha-or-ref> | --pr <N>]
                      [--repo-checkout <path>]   # default <repo-root>/hermit
                      [--repo <owner/name>]      # default rrnewton/hermit
                      [--anchors <path>] [--json]
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

import gh_route

DEFAULT_REPO = "rrnewton/hermit"
DEFAULT_CHECKOUT = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "hermit")
# Single source of truth for every rebase-base floor (producer-anchor AND
# merge-gate kinds). gate_floors.py derives the effective (newest) floor from the
# same file; this guard refuses a head that predates ANY of them before a doomed
# validate starts.
DEFAULT_ANCHORS = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                               "rebase-base-floors.json")
# A local ancestry check or a single compare API call is a few seconds; the
# whole point is refusing FAST rather than launching a doomed ~17-min validate.
NETWORK_TIMEOUT = 60.0
LOCAL_TIMEOUT = 30.0

EXIT_OK = 0
EXIT_REFUSED = 2
EXIT_ERROR = 3


class PreflightError(Exception):
    """Head/anchors could not be resolved (distinct from a clean REFUSE)."""


def _short(sha: str) -> str:
    return sha[:12] if sha else "?"


def load_anchors(path: str) -> list[dict]:
    """Return the required anchor objects (the `anchors` array).

    The file also carries `_README` and `_pending` (documented, not enforced);
    only `anchors` is required. A missing/malformed file is an ERROR, not an
    empty OK -- silently passing when the policy file is unreadable would defeat
    the guard.
    """
    try:
        with open(path, encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError) as err:
        raise PreflightError(f"cannot read anchors file {path}: {err}") from err
    if isinstance(doc, list):  # tolerate a bare array form
        anchors = doc
    elif isinstance(doc, dict):
        anchors = doc.get("anchors", [])
    else:
        raise PreflightError(f"anchors file {path} is not an object or array")
    out = []
    for a in anchors:
        sha = (a.get("sha") or "").strip()
        if not sha or len(sha) != 40 or any(c not in "0123456789abcdef"
                                            for c in sha.lower()):
            raise PreflightError(
                f"anchor entry has a non-40-hex sha {sha!r} in {path}; a "
                "placeholder anchor would refuse every head -- keep it in "
                "`_pending` until its commit lands")
        out.append({
            "sha": sha.lower(),
            "kind": a.get("kind") or "producer-anchor",
            "field": a.get("field") or "?",
            "landed_utc": a.get("landed_utc") or "?",
            "reason": a.get("reason") or "",
        })
    return out


def _run(cmd: list[str], *, timeout: float) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


def _gh_command(*arguments: str) -> list[str]:
    try:
        return gh_route.command(*arguments)
    except ValueError as error:
        raise PreflightError(str(error)) from error


def resolve_pr_head(repo: str, pr: str) -> tuple[str, str]:
    """(headRefOid, headRefName) for a PR via `gh pr view`."""
    cp = _run(_gh_command("pr", "view", str(pr), "--repo", repo,
                          "--json", "headRefOid,headRefName"), timeout=NETWORK_TIMEOUT)
    if cp.returncode != 0:
        raise PreflightError(
            f"gh pr view {pr} failed: {(cp.stderr or cp.stdout).strip()}")
    try:
        data = json.loads(cp.stdout)
    except ValueError as err:
        raise PreflightError(f"gh pr view {pr} returned non-JSON: {err}") from err
    oid = (data.get("headRefOid") or "").strip()
    if not oid:
        raise PreflightError(f"gh pr view {pr} returned no headRefOid")
    return oid, (data.get("headRefName") or "?")


def _local_contains(checkout: str, anchor: str, head: str) -> bool | None:
    """True/False if a LOCAL ancestry check could decide, else None.

    Returns None (undecided) when the checkout is absent, or when either the
    anchor or the head is unknown to the local object database -- the caller
    then falls back to the compare API.
    """
    if not checkout or not os.path.isdir(os.path.join(checkout, ".git")) \
            and not os.path.isfile(os.path.join(checkout, ".git")):
        return None
    # Both revs must resolve locally, else --is-ancestor errors and we can't
    # trust its rc; fall back rather than guess.
    for rev in (anchor, head):
        cp = _run(["git", "-C", checkout, "rev-parse", "--verify", "-q",
                   f"{rev}^{{commit}}"], timeout=LOCAL_TIMEOUT)
        if cp.returncode != 0:
            return None
    cp = _run(["git", "-C", checkout, "merge-base", "--is-ancestor",
               anchor, head], timeout=LOCAL_TIMEOUT)
    if cp.returncode in (0, 1):
        return cp.returncode == 0
    return None  # unexpected git error -> undecided, fall back


def _compare_contains(repo: str, anchor: str, head: str) -> bool:
    """head CONTAINS anchor per the GitHub compare API.

    compare/<anchor>...<head> reports how HEAD relates to BASE=anchor:
    'ahead'/'identical' -> head contains anchor; 'behind'/'diverged' -> not.
    """
    cp = _run(_gh_command("api",
                         f"repos/{repo}/compare/{anchor}...{head}", "--jq", ".status"),
              timeout=NETWORK_TIMEOUT)
    if cp.returncode != 0:
        raise PreflightError(
            f"gh api compare {_short(anchor)}...{_short(head)} failed: "
            f"{(cp.stderr or cp.stdout).strip()}")
    status = cp.stdout.strip()
    if status in ("ahead", "identical"):
        return True
    if status in ("behind", "diverged"):
        return False
    raise PreflightError(
        f"gh api compare returned unexpected status {status!r}")


def head_contains(checkout: str, repo: str, anchor: str, head: str
                  ) -> tuple[bool, str]:
    """(contains?, how) -- prefer the local ancestry check, else compare API."""
    local = _local_contains(checkout, anchor, head)
    if local is not None:
        return local, "local-merge-base"
    return _compare_contains(repo, anchor, head), "github-compare"


def preflight(head: str, *, checkout: str, repo: str, anchors_path: str) -> dict:
    """Evaluate every required anchor against `head`. Never raises for a plain
    REFUSE -- only for an ERROR (unresolvable head/anchors)."""
    anchors = load_anchors(anchors_path)
    results = []
    missing = []
    for a in anchors:
        contains, how = head_contains(checkout, repo, a["sha"], head)
        rec = {**a, "contained": contains, "checked_via": how}
        results.append(rec)
        if not contains:
            missing.append(rec)
    return {
        "head": head,
        "repo": repo,
        "n_anchors": len(anchors),
        "anchors": results,
        "missing": missing,
        "ok": not missing,
    }


def refuse_line(head: str, a: dict) -> str:
    kind = a.get("kind", "producer-anchor")
    head_s, sha_s = _short(head), _short(a["sha"])
    remedy = (f"Rebase onto current origin/main (>= {sha_s}) before "
              f"validating/landing.")
    if kind == "merge-gate":
        # green-but-refused: the run is green, the gate still rejects it.
        return (f"REFUSE: head {head_s} predates merge-gate floor {sha_s} "
                f"({a['field']}, landed {a['landed_utc']}); a receipt from it "
                f"does not satisfy the merge gate -> validates GREEN yet the "
                f"landing is refused. {remedy}")
    # producer-anchor: green-but-null-fielded.
    return (f"REFUSE: head {head_s} predates producer anchor {sha_s} "
            f"({a['field']}, landed {a['landed_utc']}); its validate.sh emits "
            f"{a['field']} NULL and is_clean_full_pass is fail-closed -> no "
            f"qualifying receipt possible. {remedy}")


def render(res: dict) -> str:
    if res["ok"]:
        return (f"OK: head {_short(res['head'])} contains all "
                f"{res['n_anchors']} producer anchors; validate can produce a "
                f"qualifying receipt.")
    # Name the SPECIFIC missing anchor(s); the first is the headline reason.
    lines = [refuse_line(res["head"], a) for a in res["missing"]]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--head", help="sha or ref to check (local or remote)")
    g.add_argument("--pr", help="resolve the head OID of this PR number via gh")
    ap.add_argument("--repo-checkout", default=DEFAULT_CHECKOUT,
                    help="local checkout for the fast merge-base path")
    ap.add_argument("--repo", default=DEFAULT_REPO,
                    help="owner/name for the gh compare/pr fallbacks")
    ap.add_argument("--anchors", default=DEFAULT_ANCHORS,
                    help="rebase-base-floors.json path")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    try:
        if args.pr:
            head, _branch = resolve_pr_head(args.repo, args.pr)
        elif args.head:
            head = args.head
        else:
            ap.error("one of --head or --pr is required")
            return EXIT_ERROR  # unreachable; ap.error exits
        res = preflight(head, checkout=args.repo_checkout, repo=args.repo,
                        anchors_path=args.anchors)
    except PreflightError as err:
        if args.json:
            print(json.dumps({"error": str(err)}, indent=2))
        else:
            print(f"ERROR: {err}", file=sys.stderr)
        return EXIT_ERROR

    if args.json:
        print(json.dumps(res, indent=2))
    else:
        print(render(res))
    return EXIT_OK if res["ok"] else EXIT_REFUSED


if __name__ == "__main__":
    sys.exit(main())
