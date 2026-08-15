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
visibility mechanism that also mutates is two changes.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

MANIFEST = "ci-hub-change-manifest.json"
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


def inspect(clone: Path) -> dict[str, Any]:
    """Describe one clone without changing it.

    The enumeration matches the parent gate's: `--exclude=refs/tags/*` before
    `--all`, so branches, HEAD and every worktree's HEAD count while tags do
    not. `--branches` alone would miss a detached HEAD, which is the state an
    interrupted change leaves a clone in.
    """
    manifest: dict[str, Any] = {}
    try:
        manifest = json.loads((clone / ".git" / MANIFEST).read_text())
    except (OSError, ValueError):
        pass
    shas = [s for s in git(clone, "rev-list", "--exclude=refs/tags/*", "--all",
                           "--not", "--remotes").splitlines() if s]
    dirty = [line for line in git(clone, "status", "--porcelain").splitlines() if line]
    return {
        "clone": str(clone),
        "task": manifest.get("task"),
        "branch": git(clone, "rev-parse", "--abbrev-ref", "HEAD"),
        "unpushed_count": len(shas),
        "tip": shas[0] if shas else None,
        "uncommitted_paths": len(dirty),
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

    if a.json:
        print(json.dumps({"search_root": str(root), "clones": len(rows),
                          "holding": len(holding), "rows": rows}, sort_keys=True))
    else:
        print(f"unpushed-change-clone-commits search_root={root} "
              f"clones={len(rows)} holding={len(holding)}")
        for r in holding:
            print(f"  {r['clone']}  task={r['task']} branch={r['branch']} "
                  f"unpushed={r['unpushed_count']} uncommitted={r['uncommitted_paths']} "
                  f"tip={r['tip']}")
        if not rows:
            print("  (no change clones present)")
        elif not holding:
            print("  (every change clone is clean and fully pushed)")
        # Reporting only. Disposal and preservation are separate operations with
        # their own refusals; nothing here mutates a clone.
        if holding:
            print("summary=change clone(s) hold work that exists on no remote ref; "
                  "inspect and preserve or land it -- `dispose` will refuse them")
        else:
            print("summary=no change clone holds unpushed work")
    return 1 if holding else 0


if __name__ == "__main__":
    sys.exit(main())
