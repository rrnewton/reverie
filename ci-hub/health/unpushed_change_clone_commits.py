#!/usr/bin/env python3
"""Report commits held in ci-hub change clones and on no remote ref.

WHY THIS IS SEPARATE FROM `unpushed_parent_commits`

Change clones are SEPARATE REPOSITORIES. `git worktree list` in the parent
lists none of them, and no enumeration of one repository's refs can reach
another repository's objects. The boundary is structural, not a scoping choice
that could be widened, so the parent gate cannot be taught to see them. Its 0 is
the true answer for the parent and must stay that way; adding a second
population to it would forfeit the bracket that makes that 0 mean something.

WHY IT EXISTS

`dispose` refuses to remove a clone holding commits that exist on no remote ref.
That is a floor, not a fix: a refusal protects work already at risk, it does not
tell anyone the risk exists. Two commits sat in an abandoned clone for two days
and were found twice by accident, never by a signal.

THIS REPORTS AND DOES NOTHING ELSE. It never pushes, disposes or preserves. A
visibility mechanism that also mutates is two changes. It does fetch, which is
how it READS the remote; a fetch moves no branch, writes no commit and touches
no working tree, so no clone's work can be altered or lost by running this.

IT MUST ASK THE SAME QUESTION `dispose` ASKS

Two instruments reporting different counts for the same clones is worse than
either being slightly wrong, because the next reader cannot tell which is about
what. So the enumeration below is character-for-character the one in
`scripts/ci-hub-clone-change.py::unpushed_in_clone`, and the fetch matches its
`refresh_remote_tracking`. Both halves are needed. Measured 2026-08-15 over the
28 clones then present: the old `--all` enumeration counted 77 commits, of which
73 were `refs/pr-landing-planner/*` fetch destinations that exist on the remote
by construction; and all 4 survivors were ALSO already on their remotes, and
showed up only because those clones' remote-tracking refs were stale. Narrowing
the scope without refreshing would have left this gate reporting four alarms,
every one of them false.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

MANIFEST = "ci-hub-change-manifest.json"
# One clone's remote must not be able to stall the tick. Measured 2026-08-15, a
# no-op fetch of these clones took 0.3s-2.7s, so this is a hang bound and not a
# budget anyone should hit.
FETCH_TIMEOUT_SECS = 60
# `ci-hub-clone-change.py create` defaults its destination to ~/temp/dev-hermit,
# so sibling clones land beside it. Scanning that one directory is what makes
# this tractable at all: the creation path knows where it puts clones.
DEFAULT_SEARCH_ROOT = Path("~/temp").expanduser()


def git(repo: Path, *args: str) -> str:
    out = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    return out.stdout.strip()


def change_clones(root: Path) -> list[Path]:
    """Directories under `root` carrying a change manifest, sorted."""
    if not root.is_dir():
        return []
    found = []
    for child in sorted(root.iterdir()):
        if child.is_dir() and (child / ".git" / MANIFEST).is_file():
            found.append(child)
    return found


def refresh_remote_tracking(clone: Path) -> bool:
    """Update `refs/remotes/origin/*` from the remote; report whether it worked.

    Same refspec as the disposal tool, for the same reason: a clone's work may
    have been pushed to any branch, and a snapshot of unknown age is wrong in
    both directions -- it under-reports what is pushed, and it can over-report
    containment when a ref it still shows has since moved.
    """
    command = ["git", "-C", str(clone), "fetch", "--quiet", "origin",
               "+refs/heads/*:refs/remotes/origin/*"]
    if shutil.which("with-proxy"):
        command.insert(0, "with-proxy")
    try:
        return subprocess.run(command, capture_output=True, text=True,
                              timeout=FETCH_TIMEOUT_SECS).returncode == 0
    except (subprocess.TimeoutExpired, OSError):
        return False


def inspect(clone: Path) -> dict[str, Any]:
    """Describe one clone, altering no work in it.

    The enumeration is `--branches` plus `HEAD`, matching
    `ci-hub-clone-change.py::unpushed_in_clone` exactly: every local branch, plus
    the detached HEAD an interrupted change leaves behind, which `--branches`
    alone would miss. It is deliberately NOT `--all`, which also sweeps
    fetch-destination namespaces such as `refs/pr-landing-planner/*` whose
    contents are on the remote by construction.

    `remote_reachable` is false when the refresh failed, and then the count is
    against a snapshot of unknown age. That is reported as undetermined, not as
    a number: a count nobody can date is not a measurement.
    """
    manifest: dict[str, Any] = {}
    try:
        manifest = json.loads((clone / ".git" / MANIFEST).read_text())
    except (OSError, ValueError):
        pass
    reachable = refresh_remote_tracking(clone)
    shas = [s for s in git(clone, "rev-list", "--branches", "HEAD",
                           "--not", "--remotes").splitlines() if s]
    dirty = [line for line in git(clone, "status", "--porcelain").splitlines() if line]
    return {
        "clone": str(clone),
        "task": manifest.get("task"),
        "branch": git(clone, "rev-parse", "--abbrev-ref", "HEAD"),
        "unpushed_count": len(shas),
        "tip": shas[0] if shas else None,
        "uncommitted_paths": len(dirty),
        "remote_reachable": reachable,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--search-root", default=str(DEFAULT_SEARCH_ROOT),
                    help="directory whose immediate children are inspected for change clones")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    root = Path(a.search_root).expanduser()
    rows = [inspect(c) for c in change_clones(root)]
    holding = [r for r in rows if r["unpushed_count"] or r["uncommitted_paths"]]
    # A clone whose remote could not be reached is neither holding nor clean;
    # it is unmeasured, and saying so is the only honest report.
    undetermined = [r for r in rows if not r["remote_reachable"]]

    if a.json:
        print(json.dumps({"search_root": str(root), "clones": len(rows),
                          "holding": len(holding),
                          "undetermined": len(undetermined), "rows": rows},
                         sort_keys=True))
    else:
        print(f"unpushed-change-clone-commits search_root={root} "
              f"clones={len(rows)} holding={len(holding)} "
              f"undetermined={len(undetermined)}")
        for r in holding:
            print(f"  {r['clone']}  task={r['task']} branch={r['branch']} "
                  f"unpushed={r['unpushed_count']} uncommitted={r['uncommitted_paths']} "
                  f"tip={r['tip']}")
        for r in undetermined:
            print(f"  {r['clone']}  task={r['task']} branch={r['branch']} "
                  "UNDETERMINED: remote unreachable, so its counts are against a "
                  "snapshot of unknown age")
        if not rows:
            print("  (no change clones present)")
        elif not holding and not undetermined:
            print("  (every change clone is clean and fully pushed)")
        # Reporting only. Disposal and preservation are separate operations with
        # their own refusals; nothing here mutates a clone.
        if holding:
            print("summary=change clone(s) hold work that exists on no remote ref; "
                  "inspect and preserve or land it -- `dispose` will refuse them")
        elif undetermined:
            # `bounded_gate.py`'s spelling, so a reader already knows what it
            # means: the gate ran and could not determine its condition.
            print("state=NO_RESULT")
            print(f"summary=NO_RESULT/remote-unreachable gate="
                  f"unpushed_change_clone_commits undetermined={len(undetermined)} "
                  f"clones={len(rows)}")
        else:
            print("summary=no change clone holds unpushed work")
    return 1 if holding or undetermined else 0


if __name__ == "__main__":
    sys.exit(main())
