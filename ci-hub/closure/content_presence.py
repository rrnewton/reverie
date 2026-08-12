#!/usr/bin/env python3
"""Does the commit's CONTENT survive on the target — not merely its SHA?

THE DEFECT THIS EXISTS FOR
--------------------------
`git merge-base --is-ancestor <sha> origin/main` proves the commit is REACHABLE. It does
NOT prove the commit's CHANGES are present. A merge resolved toward the wrong side, or a
reconcile that drops a hunk, leaves the SHA a perfectly good ancestor while its content is
gone. Ancestry is a PROXY for "the work landed", it reads authoritative, and it fails in
the CONCEALING direction: it reports success precisely when the content has been lost.

Measured on a scratch clone during the recovery-procedure audit: a two-sided conflict
resolved with `checkout --ours` gave `commit-reachability lost=0 of 2` while the other
side's edit was absent from the file. The losing commit stays reachable as a merge parent
forever; only its effect disappears.

`ci-hub/closure/landing_evidence.py` states the standing rule — "Landing is a fact about
`main`. It is established by ANCESTRY on a freshly fetched target." That rule is CORRECT
for establishing ABSENCE (not an ancestor => definitely not landed) and INSUFFICIENT for
establishing PRESENCE. This module supplies the missing half. It does not replace ancestry;
it is the second question you must ask once ancestry says yes.

THE CHECK
---------
For each hunk of the commit's own diff, attempt to REVERSE-APPLY that hunk to the target
tree. A hunk that reverse-applies is present in the target; one that does not is either
missing or has been legitimately superseded. Per-hunk rather than whole-patch, so the
answer carries a count and a denominator instead of a bare boolean — a partial loss is
visible as 7/9 rather than collapsing to "failed".

FOUR VERDICTS, DELIBERATELY NOT TWO
-----------------------------------
  ABSENT        the SHA is not an ancestor of the target. Ancestry already catches this.
  PRESENT       ancestor AND every hunk reverse-applies. Content survived.
  CONTENT-LOST  *** ancestor BUT one or more hunks do not reverse-apply. ***
                This is the case ancestry passes and this module exists to catch.
  INDETERMINATE the diff could not be computed or the commit is not readable here.
                A no-result is not a pass and not a failure; see the workspace-wide rule
                that a gate which cannot distinguish these has too small a verdict space.

HONEST LIMITATION, STATED BECAUSE IT BOUNDS THE CLAIM
------------------------------------------------------
A hunk that fails to reverse-apply is not PROOF of loss. A later legitimate commit that
rewrites the same region also breaks reverse-application. So CONTENT-LOST means "the
commit's effect is not literally present at the target and a human must adjudicate", not
"someone definitely dropped it". This check is a SCREEN that fires in the direction
ancestry is blind to; it is deliberately conservative, because the failure it hunts is
silent and the false-alarm cost is a review.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
import git_env  # noqa: E402

ABSENT = "ABSENT"
PRESENT = "PRESENT"
CONTENT_LOST = "CONTENT-LOST"
INDETERMINATE = "INDETERMINATE"

EXIT_OK = 0
EXIT_CONTENT_LOST = 1
EXIT_INDETERMINATE = 2


@dataclass
class Presence:
    sha: str
    target: str
    verdict: str
    is_ancestor: bool | None = None
    hunks_total: int = 0
    hunks_present: int = 0
    missing: tuple[str, ...] = field(default_factory=tuple)
    reason: str = ""

    def render(self) -> str:
        return (
            f"VERDICT={self.verdict}\n"
            f"SHA={self.sha}\n"
            f"TARGET={self.target}\n"
            f"IS_ANCESTOR={'-' if self.is_ancestor is None else self.is_ancestor}\n"
            f"HUNKS_PRESENT={self.hunks_present}/{self.hunks_total}\n"
            f"MISSING={len(self.missing)}"
            + (f" [{' '.join(self.missing)}]" if self.missing else "")
            + f"\nREASON={self.reason}"
        )

    def exit_code(self) -> int:
        if self.verdict in (PRESENT, ABSENT):
            return EXIT_OK
        if self.verdict == CONTENT_LOST:
            return EXIT_CONTENT_LOST
        return EXIT_INDETERMINATE


def _git(repo: Path, *args: str, text: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=text,
        check=False,
        env=git_env.sanitized_git_env(),
    )


def split_hunks(diff_text: str) -> list[tuple[str, str, str]]:
    """Split a unified diff into (path, header_block, hunk_block) triples.

    One patch per hunk is what lets the answer carry a denominator: whole-patch
    reverse-apply gives one bit, per-hunk gives present/total.
    """
    out: list[tuple[str, str, str]] = []
    header: list[str] = []
    path = ""
    hunk: list[str] = []

    def flush() -> None:
        if hunk and header:
            out.append((path, "".join(header), "".join(hunk)))

    for line in diff_text.splitlines(keepends=True):
        if line.startswith("diff --git "):
            flush()
            hunk = []
            header = [line]
            path = line.rstrip("\n").split(" b/")[-1]
        elif line.startswith("@@"):
            flush()
            hunk = [line]
        elif hunk:
            hunk.append(line)
        elif header:
            header.append(line)
    flush()
    return out


def check(repo: Path, sha: str, target: str) -> Presence:
    """Ancestry first, then content. Both are reported; neither is inferred."""
    anc = _git(repo, "merge-base", "--is-ancestor", sha, target)
    is_ancestor = anc.returncode == 0

    if not is_ancestor:
        return Presence(
            sha=sha,
            target=target,
            verdict=ABSENT,
            is_ancestor=False,
            reason=(
                "not an ancestor of the target; ancestry alone already establishes "
                "absence, so no content check is needed"
            ),
        )

    show = _git(repo, "show", "--format=", "--no-color", "-U3", sha)
    if show.returncode != 0:
        return Presence(
            sha=sha,
            target=target,
            verdict=INDETERMINATE,
            is_ancestor=True,
            reason=f"could not read the commit's diff: {show.stderr.strip()[:200]}",
        )

    hunks = split_hunks(show.stdout)
    if not hunks:
        return Presence(
            sha=sha,
            target=target,
            verdict=INDETERMINATE,
            is_ancestor=True,
            reason=(
                "the commit has ZERO hunks (merge commit, or empty). A content check "
                "over an empty hunk set would be a 0/0 pass, which is a no-result, "
                "not evidence that anything survived"
            ),
        )

    # REVERSE-APPLY MUST BE EVALUATED AGAINST THE TARGET'S CONTENT, not against whatever
    # the caller happens to have checked out. A temp worktree at the target is the only
    # honest way to ask "is this hunk present ON MAIN"; checking the caller's working
    # tree would answer a different question and would pass or fail for the wrong reason.
    missing: list[str] = []
    present = 0
    with tempfile.TemporaryDirectory(prefix="content-presence-") as td:
        wt = Path(td) / "t"
        add = _git(repo, "worktree", "add", "--detach", "--quiet", str(wt), target)
        if add.returncode != 0:
            return Presence(
                sha=sha,
                target=target,
                verdict=INDETERMINATE,
                is_ancestor=True,
                hunks_total=len(hunks),
                reason=(
                    "could not materialise the target for comparison: "
                    f"{add.stderr.strip()[:200]}"
                ),
            )
        try:
            for path, header, hunk in hunks:
                patch = header + hunk
                if not patch.endswith("\n"):
                    patch += "\n"
                rev = subprocess.run(
                    ["git", "-C", str(wt), "apply", "--reverse", "--check", "-"],
                    input=patch,
                    capture_output=True,
                    text=True,
                    check=False,
                    env=git_env.sanitized_git_env(),
                )
                if rev.returncode == 0:
                    present += 1
                else:
                    missing.append(f"{path}@{hunk.splitlines()[0].strip()}")
        finally:
            _git(repo, "worktree", "remove", "--force", str(wt))

    if present == len(hunks):
        return Presence(
            sha=sha,
            target=target,
            verdict=PRESENT,
            is_ancestor=True,
            hunks_total=len(hunks),
            hunks_present=present,
            reason="ancestor AND every hunk reverse-applies at the target",
        )

    return Presence(
        sha=sha,
        target=target,
        verdict=CONTENT_LOST,
        is_ancestor=True,
        hunks_total=len(hunks),
        hunks_present=present,
        missing=tuple(missing),
        reason=(
            f"THE SHA IS AN ANCESTOR BUT {len(hunks) - present} OF {len(hunks)} HUNK(S) "
            f"ARE NOT PRESENT AT THE TARGET. Ancestry passes this; the content did not "
            f"survive intact. Adjudicate: either the hunk was dropped by a lossy "
            f"reconcile, or a later commit legitimately superseded that region."
        ),
    )


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("sha")
    p.add_argument("--repo", type=Path, default=Path.cwd())
    p.add_argument("--target", default="origin/main")
    p.add_argument("--json", action="store_true")
    a = p.parse_args(argv)
    res = check(a.repo, a.sha, a.target)
    if a.json:
        print(json.dumps(res.__dict__, default=list, sort_keys=True))
    else:
        print(res.render())
    return res.exit_code()


if __name__ == "__main__":
    raise SystemExit(main())
