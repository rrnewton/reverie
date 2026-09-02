#!/usr/bin/env python3
"""STANDING reconcile: earned validate receipts x FRESH open-PR heads.

WHY THIS EXISTS (memory: rebase-wrapper-eligible-query-relocates-mailbox-gap,
validate-record-keying-breaks-under-rebase). A validate receipt remains keyed to
the exact commit it tested. This tool joins those records to current open heads;
it does not decide whether preservation across a rebase is landable. Every
rebase / push / mark-ready REWRITES a PR's head, so the receipt earned at the old
head no longer matches this exact-head inventory. Measured 2026-08-04: of 58
distinct clean-full receipt commits only 2 still matched a current open-PR head
(97% orphan rate), and BOTH survivors were then refused by the merge-gate floor.
This inventory identifies exact-head matches cheaply; hard/soft-green landability
belongs to the validation-authority rule.

THE JOIN. For each distinct clean-full receipt commit, decide against a
FRESHLY-FETCHED remote and FULL 40-hex SHAs:

  * ORPHANED      -- no current open-PR head equals this commit. The head moved
                     or the PR already landed, so this exact-head join cannot
                     bind it. This is not a verdict that the receipt's evidence
                     is useless after a rebase; hard/soft-green landability is a
                     separate decision.
  * NOT-CERTIFIED -- the commit DOES match an open-PR head, but the authoritative
                     is_clean_full_pass certifier (`ci-hub validate-status`)
                     refuses it. local-history's `checks==5` prefilter is LOOSER
                     than the certifier: it admits schema-1 null-fielded rows
                     (e.g. 4cdda392, the #1544 head) that fail the real gate. A
                     local-history match is NOT landability proof.
  * FLOOR-BLOCKED -- matches an open head AND authoritatively certified, but the
                     head predates a rebase-base floor (merge-gate or producer
                     anchor). It validates GREEN yet landing is refused; the
                     lever is REBASE onto the named immutable floor.
  * VALID         -- matches an open head, authoritatively certified, AND clears
                     every immutable floor. The final lander still checks the
                     receipt's recorded base against freshly fetched main once,
                     immediately before merge.

STANDING, not one-shot: run it after every drain rebase wave. Each wave rewrites
heads and therefore changes the exact-head matches; only a re-run answers today's
inventory. It does not invalidate the receipts for the commits they tested.

Every count is stated WITH ITS DENOMINATOR ("valid 1 of 58", never "valid 1")
because the interesting number is the ratio: valid/total is the earned-but-unread
yield, orphaned/total is the ordering-defect cost.

Usage:
  reconcile_receipts.py [--repo rrnewton/hermit] [--json]
                        [--prs-json <path>]      # inject open PRs (offline/test)
                        [--history-json <path>]  # inject local-history rows
                        [--no-fetch]             # do not refresh the remote
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CI_HUB_ROOT = os.path.dirname(HERE)                       # .../ci-hub
REPO_ROOT = os.path.dirname(CI_HUB_ROOT)                  # .../dev-hermit
CI_HUB_BIN = os.path.join(CI_HUB_ROOT, "ci-hub")

# preflight_anchor lives beside this file's sibling validate/ dir; import it as a
# module so a REFUSE returns structured data instead of just an exit code.
sys.path.insert(0, os.path.join(CI_HUB_ROOT, "validate"))
import preflight_anchor  # noqa: E402

DEFAULT_REPO = "rrnewton/hermit"

# The looser ENUMERATOR predicate over local-history. Deliberately a prefilter:
# it names candidate receipt commits cheaply; the authoritative certifier
# (validate-status) is the arbiter of is_clean_full_pass.
def _is_clean_full_candidate(row: dict) -> bool:
    return (row.get("profile") == "full"
            and row.get("result") == "pass"
            and row.get("checks") == 5)


def _is_full_sha(sha: str) -> bool:
    return (isinstance(sha, str) and len(sha) == 40
            and all(c in "0123456789abcdef" for c in sha.lower()))


# ---- data sources (subprocess by default; injectable for tests/offline) ------

def fetch_open_prs(repo: str, *, prs_json: str | None) -> list[dict]:
    """Open PRs with FULL 40-hex head OIDs from a FRESHLY-FETCHED remote."""
    if prs_json:
        with open(prs_json, encoding="utf-8") as fh:
            return json.load(fh)
    cp = subprocess.run(
        ["with-proxy", "gh", "pr", "list", "--repo", repo, "--state", "open",
         "--json", "number,headRefOid,isDraft,title", "--limit", "500"],
        capture_output=True, text=True, timeout=120)
    if cp.returncode != 0:
        raise RuntimeError(
            f"gh pr list failed: {(cp.stderr or cp.stdout).strip()}")
    return json.loads(cp.stdout)


def load_history(*, history_json: str | None) -> list[dict]:
    if history_json:
        with open(history_json, encoding="utf-8") as fh:
            return json.load(fh)
    cp = subprocess.run([CI_HUB_BIN, "local-history", "--json"],
                        capture_output=True, text=True, timeout=120)
    if cp.returncode != 0:
        raise RuntimeError(
            f"ci-hub local-history failed: {(cp.stderr or cp.stdout).strip()}")
    return json.loads(cp.stdout)


def candidate_commits(rows: list[dict]) -> list[str]:
    """Distinct FULL-40-hex commits with a clean-full candidate receipt,
    ordered by newest receipt first (fastest to eyeball)."""
    newest: dict[str, str] = {}
    for r in rows:
        if not _is_clean_full_candidate(r):
            continue
        c = (r.get("commit") or "").strip().lower()
        if not _is_full_sha(c):
            continue
        fin = r.get("finished_at") or ""
        if c not in newest or fin > newest[c]:
            newest[c] = fin
    return [c for c, _ in sorted(newest.items(), key=lambda kv: kv[1],
                                 reverse=True)]


# ---- authoritative certifier + floor check (injectable) ----------------------

def certify_validated(sha: str) -> bool:
    """is_clean_full_pass per the authoritative consumer: exit 0 == VALIDATED."""
    cp = subprocess.run([CI_HUB_BIN, "validate-status", "--sha", sha, "--json"],
                        capture_output=True, text=True, timeout=60)
    # exit 0 is the ONLY landable verdict; 3/4 (FAILED/NEEDS-RERUN/NOT-VALIDATED)
    # are not. Trust the exit code, not a parsed string.
    return cp.returncode == 0


def floor_clearance(sha: str, repo: str) -> dict:
    """{'ok': bool, 'reason': str} -- structured merge-gate/producer floor check."""
    res = preflight_anchor.preflight(
        sha, checkout=preflight_anchor.DEFAULT_CHECKOUT, repo=repo,
        anchors_path=preflight_anchor.DEFAULT_ANCHORS)
    reason = "" if res["ok"] else preflight_anchor.refuse_line(sha, res["missing"][0])
    return {"ok": res["ok"], "reason": reason}


# ---- the pure join (fully unit-testable) -------------------------------------

def reconcile(open_prs: list[dict], commits: list[str], *,
              certify, floor, repo: str) -> dict:
    """Join earned receipt commits against fresh open-PR heads.

    certify(sha) -> bool          authoritative is_clean_full_pass
    floor(sha, repo) -> {ok,reason}  rebase-base floor clearance
    """
    head2prs: dict[str, list[dict]] = {}
    for p in open_prs:
        oid = (p.get("headRefOid") or "").strip().lower()
        if oid:
            head2prs.setdefault(oid, []).append(p)

    buckets = {"valid": [], "floor_blocked": [], "not_certified": [],
               "orphaned": []}
    for c in commits:
        prs = head2prs.get(c)
        if not prs:
            buckets["orphaned"].append({"commit": c})
            continue
        pr_ids = [p["number"] for p in prs]
        drafts = [p["number"] for p in prs if p.get("isDraft")]
        if not certify(c):
            buckets["not_certified"].append(
                {"commit": c, "prs": pr_ids,
                 "detail": "matches an open head but fails authoritative "
                           "is_clean_full_pass (looser local-history prefilter)"})
            continue
        fc = floor(c, repo)
        entry = {"commit": c, "prs": pr_ids, "drafts": drafts}
        if fc["ok"]:
            buckets["valid"].append(entry)
        else:
            entry["reason"] = fc["reason"]
            buckets["floor_blocked"].append(entry)

    total = len(commits)
    return {
        "schema_version": 1,
        "repo": repo,
        "total_receipt_commits": total,
        "open_prs": len(open_prs),
        "counts": {k: len(v) for k, v in buckets.items()},
        "buckets": buckets,
    }


# ---- rendering ---------------------------------------------------------------

def _pct(n: int, d: int) -> str:
    return f"{100 * n / d:.0f}%" if d else "n/a"


def render(report: dict) -> str:
    n = report["total_receipt_commits"]
    c = report["counts"]
    out = [
        "=== reconcile receipts x FRESH open-PR heads ===",
        f"repo {report['repo']} | open PRs {report['open_prs']} | "
        f"distinct clean-full receipt commits {n}",
        "",
        f"VALID (fixed floors; final base check required): {c['valid']} of {n}",
        f"FLOOR-BLOCKED (green, rebase needed):  {c['floor_blocked']} of {n}",
        f"NOT-CERTIFIED (matched head, cert refuses): {c['not_certified']} of {n}",
        f"ORPHANED (head moved/landed; no exact-head match): {c['orphaned']} of {n} "
        f"= {_pct(c['orphaned'], n)} (ordering-defect cost)",
    ]
    b = report["buckets"]
    if b["valid"]:
        out += ["", "--- VALID: fixed floors clear; lander rechecks recorded base ---"]
        for e in b["valid"]:
            prs = ",".join("#" + str(p) for p in e["prs"])
            out.append(f"  {e['commit']}  {prs}")
    if b["floor_blocked"]:
        out += ["", "--- FLOOR-BLOCKED: rebase onto the named fixed floor, "
                "then re-validate ---"]
        for e in b["floor_blocked"]:
            prs = ",".join("#" + str(p) for p in e["prs"])
            draft = " (DRAFT)" if e.get("drafts") else ""
            out.append(f"  {e['commit']}  {prs}{draft}")
            out.append(f"      {e['reason']}")
    if b["not_certified"]:
        out += ["", "--- NOT-CERTIFIED: matched an open head but the "
                "authoritative certifier refuses (do NOT hand as landable) ---"]
        for e in b["not_certified"]:
            prs = ",".join("#" + str(p) for p in e["prs"])
            out.append(f"  {e['commit']}  {prs}")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--prs-json", help="inject open-PR JSON (offline/test)")
    ap.add_argument("--history-json", help="inject local-history JSON (test)")
    ap.add_argument("--no-fetch", action="store_true",
                    help="skip refreshing the remote before the local floor check")
    args = ap.parse_args(argv)

    try:
        open_prs = fetch_open_prs(args.repo, prs_json=args.prs_json)
        rows = load_history(history_json=args.history_json)
    except (RuntimeError, OSError, ValueError) as err:
        supplied = []
        if args.prs_json:
            supplied.append(f"--prs-json {args.prs_json}")
        if args.history_json:
            supplied.append(f"--history-json {args.history_json}")
        source = ", ".join(supplied) if supplied else "the live GitHub and local-history inputs"
        print(
            f"reconcile-receipts: COULD NOT READ {source}: {err}. "
            "Pass readable JSON files for offline input, or omit those flags to read "
            "the live sources, then retry.",
            file=sys.stderr,
        )
        return 3

    commits = candidate_commits(rows)
    report = reconcile(open_prs, commits,
                       certify=certify_validated, floor=floor_clearance,
                       repo=args.repo)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(render(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
