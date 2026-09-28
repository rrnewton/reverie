#!/usr/bin/env python3
"""Reconcile `rescue/auto-*` refs against main and report only real losses.

`ci-hub/health/unpushed_parent_commits.py --rescue` pushes every local-only
parent commit to its own `rescue/auto-<short>` ref. That side already works:
on 2026-08-06 a plain `git reset HEAD~1` on shared parent main dropped another
agent's commit as collateral, and `origin/rescue/auto-29e2cf7` is why the work
still existed. What was missing is the OTHER side -- nothing ever compared
those refs back against `main`, so a dropped commit was silent for ~20 minutes
and was noticed only by luck.

WHY THIS CLASSIFIES INSTEAD OF JUST TESTING ANCESTRY. Measured over all 109
rescue refs on 2026-08-07: 78 are ancestors of origin/main and 31 are not. Of
those 31, 12 have their subject already on main -- the work was recovered by
RE-COMMITTING it under a new SHA, so the old SHA is orphaned but nothing was
lost. Several more are git-stash artifacts or merges. A pure-ancestry alarm
would therefore have opened with 31 findings of which most are noise, and an
alarm that noisy gets muted -- at which point the one real loss is invisible
again, which is the failure this tool exists to prevent.

Precedence matters: a commit is checked LANDED -> NOT-A-LOSS -> RECOVERED ->
UNRECONCILED, and only UNRECONCILED is reported. Counts for every class are
always printed so the denominator travels with the finding.

What each verdict actually proves, stated plainly because they are not equal:

  LANDED       proof. Ancestry on a freshly-fetched main is the real thing.
  NOT-A-LOSS   proof for stashes (a stash commit was never on a branch) and for
               a merge whose parents are all on main (its content is its
               parents'). Not a judgement call.
  RECOVERED    EVIDENCE, NOT PROOF. A matching patch-id is strong; a matching
               subject alone is weak -- two commits can share a subject and
               differ in content. Subject-only matches are marked so a reader
               can tell them apart rather than trusting the label.
  UNRECONCILED "none of the above", i.e. worth a human look. Not a claim that
               the commit is definitely lost.

WHERE RESCUE REFS LIVE (2026-09-27). dev-hermit is main-only, so the producer
now pushes custom refs ``refs/rescue/auto-*`` instead of branches, and mirrors
each verified one locally as ``refs/remotes/origin-rescue/auto-*``. A plain
fetch does not bring custom refs down; mirror another checkout's with

    git fetch origin '+refs/rescue/*:refs/remotes/origin-rescue/*'

This reads every location a rescue ref can be found locally: the mirror, a
local ``refs/rescue/`` copy, and the legacy branch forms
(``refs/remotes/origin/rescue/`` and ``refs/heads/rescue/``) so older rescues
still reconcile. The same commit found at several locations is one row.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

PARENT = str(Path(__file__).resolve().parents[2])

# Subject shapes git itself generates for stash commits. A stash entry is not,
# and never was, a commit on a branch, so its absence from main is not a loss.
STASH_PREFIXES = ("index on ", "On ")
# Commits whose own message says they are disposable.
THROWAWAY_MARKERS = ("canary:", "throwaway")


def git(repo: str, *args: str, check: bool = False) -> tuple[int, str]:
    proc = subprocess.run(
        ["git", "-C", repo, *args], capture_output=True, text=True, timeout=120
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.returncode, proc.stdout.strip()


def is_ancestor(repo: str, sha: str, ref: str) -> bool:
    rc, _ = git(repo, "merge-base", "--is-ancestor", sha, ref)
    return rc == 0


def patch_id(repo: str, sha: str) -> str | None:
    """Content identity of a single commit, independent of its SHA.

    Returns None for a merge or an empty diff -- `git patch-id` has nothing to
    hash there, and treating "no patch-id" as "no match" would silently
    downgrade merges into UNRECONCILED.
    """
    show = subprocess.run(
        ["git", "-C", repo, "show", "--no-color", "--format=%H", sha],
        capture_output=True, text=True, timeout=120,
    )
    if show.returncode != 0 or not show.stdout.strip():
        return None
    pid = subprocess.run(
        ["git", "patch-id", "--stable"],
        input=show.stdout, capture_output=True, text=True, timeout=120,
    )
    out = pid.stdout.split()
    return out[0] if out else None


def classify(repo: str, sha: str, main_ref: str, main_index: dict) -> dict:
    subject = git(repo, "log", "-1", "--format=%s", sha)[1]
    parents = git(repo, "log", "-1", "--format=%P", sha)[1].split()

    if is_ancestor(repo, sha, main_ref):
        return {"verdict": "LANDED", "sha": sha, "subject": subject, "why": "ancestor of main"}

    if subject.startswith(STASH_PREFIXES):
        return {"verdict": "NOT-A-LOSS", "sha": sha, "subject": subject,
                "why": "git-stash commit; never was a branch commit"}
    low = subject.lower()
    if any(m in low for m in THROWAWAY_MARKERS):
        return {"verdict": "NOT-A-LOSS", "sha": sha, "subject": subject,
                "why": "message declares the commit disposable"}
    if len(parents) > 1 and all(is_ancestor(repo, p, main_ref) for p in parents):
        return {"verdict": "NOT-A-LOSS", "sha": sha, "subject": subject,
                "why": "merge whose every parent is already on main"}

    pid = patch_id(repo, sha)
    if pid and pid in main_index["by_patch_id"]:
        return {"verdict": "RECOVERED", "sha": sha, "subject": subject,
                "why": f"identical content on main as {main_index['by_patch_id'][pid][:9]}",
                "strength": "patch-id"}
    if subject and subject in main_index["by_subject"]:
        return {"verdict": "RECOVERED", "sha": sha, "subject": subject,
                "why": f"same subject on main as {main_index['by_subject'][subject][:9]}"
                       " (subject match only -- content NOT compared)",
                "strength": "subject-only"}

    return {"verdict": "UNRECONCILED", "sha": sha, "subject": subject,
            "why": "not on main, not recovered, not a stash/merge/throwaway"}


def index_main(repo: str, main_ref: str, limit: int) -> dict:
    """Subject and patch-id lookup over the last `limit` commits of main.

    Bounded on purpose: a full-history patch-id walk is expensive and rescue
    refs are days old at most. The bound is reported in the output so a reader
    can tell "no match" from "looked no further".
    """
    by_subject: dict[str, str] = {}
    by_patch_id: dict[str, str] = {}
    _, out = git(repo, "log", main_ref, f"-{limit}", "--format=%H%x00%s")
    for line in out.splitlines():
        if "\x00" not in line:
            continue
        sha, subject = line.split("\x00", 1)
        by_subject.setdefault(subject, sha)
        pid = patch_id(repo, sha)
        if pid:
            by_patch_id.setdefault(pid, sha)
    return {"by_subject": by_subject, "by_patch_id": by_patch_id, "limit": limit}


def rescue_ref_patterns(glob: str) -> list[str]:
    """Every local location a rescue ref named by `glob` (``rescue/...``) can occupy.

    Current: the verified mirror under refs/remotes/origin-rescue/ and a local
    refs/rescue/ copy. Legacy: the fetched branch and a local branch.
    """
    tail = glob.removeprefix("rescue/")
    return [
        f"refs/remotes/origin-rescue/{tail}",
        f"refs/{glob}",
        f"refs/remotes/origin/{glob}",
        f"refs/heads/{glob}",
    ]


def dedupe_by_sha(rows) -> list[list[str]]:
    """Keep the first location of each commit, so one rescue is one row."""
    seen: set[str] = set()
    out: list[list[str]] = []
    for name, sha in rows:
        if sha not in seen:
            seen.add(sha)
            out.append([name, sha])
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=PARENT)
    ap.add_argument("--main-ref", default="origin/main")
    ap.add_argument("--glob", default="rescue/auto-*")
    ap.add_argument("--limit", type=int, default=400,
                    help="how many main commits to index for recovery matching")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--report-only", action="store_true",
                    help="always exit 0; without it, any UNRECONCILED exits 1")
    ap.add_argument("--baseline", type=Path, default=None,
                    help="file of already-triaged SHAs (one per line, '#' comments). "
                         "They are still listed, but do not fail the check. Without "
                         "this the initial 23-commit backlog would keep the check red "
                         "forever, and a check that can never go green gets muted -- "
                         "which is the failure this tool exists to prevent. The point "
                         "is to catch the NEXT drop, not to re-litigate the backlog.")
    a = ap.parse_args(argv)

    rc, out = git(a.repo, "for-each-ref", "--format=%(refname:short) %(objectname)",
                  *rescue_ref_patterns(a.glob))
    if rc != 0:
        print("UNVERIFIABLE: could not enumerate rescue refs", file=sys.stderr)
        return 2
    refs = dedupe_by_sha(line.split() for line in out.splitlines() if line.strip())
    if not refs:
        print(f"no refs matched {a.glob}; nothing to reconcile")
        return 0

    main_index = index_main(a.repo, a.main_ref, a.limit)
    results = []
    for name, sha in refs:
        row = classify(a.repo, sha, a.main_ref, main_index)
        row["ref"] = name
        results.append(row)

    baseline: set[str] = set()
    if a.baseline and a.baseline.exists():
        for line in a.baseline.read_text().splitlines():
            line = line.split("#", 1)[0].strip()
            if line:
                baseline.add(line)

    counts: dict[str, int] = {}
    for r in results:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1
    unreconciled = [r for r in results if r["verdict"] == "UNRECONCILED"]
    # Match on any unambiguous prefix so a baseline written with short SHAs
    # keeps working; the ref name embeds a 7-char short SHA already.
    def _known(sha: str) -> bool:
        return any(sha.startswith(b) or b.startswith(sha) for b in baseline)
    fresh = [r for r in unreconciled if not _known(r["sha"])]

    if a.json:
        print(json.dumps({"counts": counts, "total": len(results),
                          "main_index_limit": a.limit,
                          "unreconciled": unreconciled,
                          "unreconciled_new": fresh}, indent=2))
    else:
        total = len(results)
        print(f"rescue refs reconciled against {a.main_ref}: {total}")
        for verdict in ("LANDED", "NOT-A-LOSS", "RECOVERED", "UNRECONCILED"):
            print(f"  {verdict:<13} {counts.get(verdict, 0):>4}/{total}")
        print(f"  (recovery matching indexed the last {a.limit} commits of {a.main_ref})")
        if unreconciled:
            n_base = len(unreconciled) - len(fresh)
            print(f"\nUNRECONCILED -- on a rescue ref, not on main, not explained"
                  f" ({len(fresh)} new, {n_base} already triaged in the baseline):")
            for r in unreconciled:
                mark = "NEW " if not _known(r["sha"]) else "    "
                print(f"  {mark}{r['sha'][:9]}  {r['ref']}\n             {r['subject']}")
            print("\nEach line is a commit that exists only on its rescue ref. Check whether it "
                  "was meant to land; `git cherry-pick` it or delete the ref once judged.")
        else:
            print("\nno unreconciled rescue refs")

    return 0 if (a.report_only or not fresh) else 1


if __name__ == "__main__":
    raise SystemExit(main())
