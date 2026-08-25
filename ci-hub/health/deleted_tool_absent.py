#!/usr/bin/env python3
"""Report tools that were deliberately DELETED and have come back on this disk.

WHY THIS EXISTS
---------------
`ci-hub/bin/gh-pr-merge-verified` was added 2026-08-25 01:03 (f541231e) and
deleted 01:25 (8cc9a975) in favour of `gh-merge-verified`. At 04:38 the same day
it was back in the coordinator's checkout as an UNTRACKED, executable, 139-line
merger with no changes-request guard, and it merged hermit#2448 at 16:22:53Z.

⚠️ AN UNTRACKED REINTRODUCTION IS WORSE THAN THE ORIGINAL. The original was in
the tree, so anyone reading the repository could see it. This one existed on one
disk, in nobody's history, and `git log` for the path shows only the add and the
delete -- the repository actively reports it as gone while it sits there running.

The deletion was recorded in `gh-merge-verified`'s own docstring, under "WHY
`gh-pr-merge-verified` WAS DELETED RATHER THAN KEPT ALONGSIDE ... so the deleted
one is not reintroduced by someone who only sees the shape of the hole". That
note is correct, well-argued, and did not work. **A documented deterrent is not a
mechanism.** This file is the mechanism.

WHAT IT DOES NOT DO, DELIBERATELY
---------------------------------
It does not delete anything. tick-hub's standing preference is DETECT-ONLY: "a
gate that reports a fact leaves the decision where the authority is." Deleting on
sight would also destroy an agent's in-progress work without asking, and the file
came back once already -- a second silent deletion is the same move that did not
hold the first time.

WHY A CHECK RATHER THAN A TOMBSTONE FILE
----------------------------------------
Committing a refusing stub at the path would also make reintroduction visible.
It was rejected because a tracked stub at that path makes `git checkout` REFUSE
in every stale worktree that still has the real file untracked there, converting
a reporting problem into a checkout failure for other agents. A check reports;
it does not block anyone's tree.

⚠️ THE ATTRIBUTION CHANNELS FOR THIS CLASS ARE ALL DEAD, which is why presence is
the only thing worth measuring. Established 2026-08-25: `atime` is useless
(`/home/newton` is mounted `noatime`; the file's atime reads EARLIER than its
mtime). `__pycache__` only records imports, not direct runs. `bash_history` does
not capture agents' non-interactive shells. Neither lander writes a landing log,
and `gh-merge-verified`'s receipt is opt-in via `--receipt`. So "has it been
used?" cannot be answered after the fact -- only "is it here?" can, and only
before the next use.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

#: Paths that were deliberately removed and must not come back. Each entry names
#: the deleting commit so a reader can find the argument rather than re-litigate
#: it, and the replacement so the hole does not look unfilled.
#:
#: Add an entry when a REMOVAL IS A SAFETY DECISION, not merely a cleanup: the
#: test is whether someone re-creating the file from its shape alone would
#: reintroduce the hazard.
DELETED: dict[str, dict[str, str]] = {
    "ci-hub/bin/gh-pr-merge-verified": {
        "deleted_by": "8cc9a975",
        "replacement": "ci-hub/bin/gh-merge-verified",
        "why": (
            "it learns the merge commit from the API, so on a box where "
            "api.github.com is unreachable it cannot verify at all; and the "
            "reintroduced copy carries no changes-request guard, so it will "
            "merge over a bound CHANGES-REQUESTED-AT exactly as hermit#2591 was"
        ),
    },
}


def tracked(root: Path, rel: str) -> bool:
    """True when git has the path in its index for this checkout."""
    done = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--error-unmatch", rel],
        capture_output=True,
        text=True,
    )
    return done.returncode == 0


def scan(root: Path) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    for rel, meta in DELETED.items():
        path = root / rel
        if not path.exists():
            continue
        findings.append(
            {
                "path": rel,
                "bytes": path.stat().st_size,
                "executable": path.stat().st_mode & 0o111 != 0,
                # An untracked reintroduction is the worse case and is called out
                # as such: nothing in the repository records that it exists.
                "tracked": tracked(root, rel),
                **meta,
            }
        )
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="deleted_tool_absent.py",
        description="Report deliberately-deleted tools that are present on disk.",
    )
    parser.add_argument(
        "--root",
        default=".",
        help="checkout to inspect (default: the current directory)",
    )
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve()
    if not (root / ".git").exists():
        # Refuse rather than report a clean scan of somewhere that is not a
        # checkout: a clean result and a mislocated scan must not look alike.
        print(
            f"deleted_tool_absent: {root} is not a git checkout; refusing to "
            "report a clean scan of it",
            file=sys.stderr,
        )
        return 2
    if not DELETED:
        print(
            "deleted_tool_absent: the DELETED set is empty, so this check cannot "
            "find anything; that is a defect in the check, not a clean board",
            file=sys.stderr,
        )
        return 2

    findings = scan(root)

    if args.json:
        print(json.dumps({"root": str(root), "findings": findings}, indent=2))
    elif not findings:
        print(f"deleted_tool_absent: OK — {len(DELETED)} deleted path(s) absent from {root}")
    else:
        for f in findings:
            visibility = "TRACKED" if f["tracked"] else "UNTRACKED — in nobody's history"
            print(
                f"deleted_tool_absent: PRESENT  {f['path']}\n"
                f"  {f['bytes']} bytes, executable={f['executable']}, {visibility}\n"
                f"  deleted by {f['deleted_by']}; use {f['replacement']} instead\n"
                f"  why it was deleted: {f['why']}",
                file=sys.stderr,
            )
        print(
            f"deleted_tool_absent: {len(findings)} deleted tool(s) present in {root}",
            file=sys.stderr,
        )
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
