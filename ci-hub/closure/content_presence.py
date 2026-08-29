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

FIVE VERDICTS, DELIBERATELY NOT TWO
-----------------------------------
  ABSENT          the SHA is not an ancestor of the target. Ancestry already catches this.
  PRESENT         ancestor AND every hunk reverse-applies. Content survived.
  CONTEXT-DRIFTED ancestor, one or more hunks do NOT reverse-apply, AND every one of
                  those hunks' ADDED lines is confirmed present at the target. The
                  content survived; only the surrounding context moved. See below.
  CONTENT-LOST    *** ancestor BUT one or more hunks do not reverse-apply AND their
                  added content could NOT be confirmed present. ***
                  This is the case ancestry passes and this module exists to catch.
  INDETERMINATE   the diff could not be computed or the commit is not readable here.
                  A no-result is not a pass and not a failure; see the workspace-wide rule
                  that a gate which cannot distinguish these has too small a verdict space.

HONEST LIMITATION, AND THE PART OF IT THAT IS NOW MEASURED
----------------------------------------------------------
A hunk that fails to reverse-apply is not PROOF of loss. A later legitimate commit that
rewrites the same region also breaks reverse-application. That limitation was stated here
from the start and went on to fire: closing `activate-refusal-message-misstates-its-own-
cause` against e3bd3a89 on 2026-08-15 reported CONTENT-LOST at 3 of 4 hunks. The whole
"missing" hunk was one added line, `+import shutil`, and that line was present on main.
ac181ae8 had added `import os` immediately above it, so the hunk's CONTEXT moved while its
CONTENT did not. A false CONTENT-LOST on surviving content is the inverse of the failure
this module hunts, and a two-valued present/lost split forces it onto the wrong side.

CONTEXT-DRIFTED separates the two. It does NOT relax CONTENT-LOST: that verdict still
fires whenever any failing hunk's added content cannot be confirmed present, and PRESENT
still requires every hunk to reverse-apply. CONTEXT-DRIFTED is reached only by ALSO
satisfying a new condition on top of the CONTENT-LOST trigger, so nothing that used to
fire stops firing.

The confirmation is deliberately strict, because "the added lines are somewhere in the
file" is too weak in the CONCEALING direction:
  - ZERO added lines is never drift. A pure-deletion hunk satisfies "all added lines are
    present" vacuously, and a 0/0 pass is the no-result this module refuses elsewhere.
  - The added lines must appear CONTIGUOUSLY and IN ORDER. A later commit that split them
    apart has changed the effect, and that stays CONTENT-LOST.
  - The match must be UNIQUE in the file. A hunk whose added line is `}` or a blank line
    matches all over a file, and accepting such a match would report survival of content
    that was genuinely dropped. A common-line hunk therefore fails confirmation and stays
    CONTENT-LOST -- a false alarm, which is the safe direction.
Each of these three sends an unconfirmable case to CONTENT-LOST, never the reverse.
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
import bounded_capture  # noqa: E402
import git_env  # noqa: E402

ABSENT = "ABSENT"
PRESENT = "PRESENT"
CONTEXT_DRIFTED = "CONTEXT-DRIFTED"
CONTENT_LOST = "CONTENT-LOST"
INDETERMINATE = "INDETERMINATE"

EXIT_OK = 0
EXIT_CONTENT_LOST = 1
EXIT_INDETERMINATE = 2
#: Distinct from EXIT_OK because the reverse-apply DID fail and a reader deserves to know
#: the answer came from the fallback confirmation, not from a clean match. Distinct from
#: EXIT_CONTENT_LOST because the content is confirmed present. `verified_close` selects on
#: the verdict string, not on this code, so closure is unaffected by the choice.
EXIT_CONTEXT_DRIFTED = 3


@dataclass
class Presence:
    sha: str
    target: str
    verdict: str
    is_ancestor: bool | None = None
    hunks_total: int = 0
    hunks_present: int = 0
    missing: tuple[str, ...] = field(default_factory=tuple)
    #: Hunks that did NOT reverse-apply but whose added lines were confirmed present.
    #: Reported separately from `missing` rather than folded into `hunks_present`, so
    #: that field keeps its one meaning: hunks that reverse-applied cleanly.
    drifted: tuple[str, ...] = field(default_factory=tuple)
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
            + f"\nDRIFTED={len(self.drifted)}"
            + (f" [{' '.join(self.drifted)}]" if self.drifted else "")
            + f"\nREASON={self.reason}"
        )

    def exit_code(self) -> int:
        if self.verdict in (PRESENT, ABSENT):
            return EXIT_OK
        if self.verdict == CONTENT_LOST:
            return EXIT_CONTENT_LOST
        if self.verdict == CONTEXT_DRIFTED:
            return EXIT_CONTEXT_DRIFTED
        return EXIT_INDETERMINATE


def _git(repo: Path, *args: str, text: bool = True) -> bounded_capture.CapturedProcess:
    return bounded_capture.git(
        args, cwd=repo, text=text, env=git_env.sanitized_git_env()
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


def added_lines(hunk: str) -> list[str]:
    """The lines a hunk ADDS, with the `+` marker stripped.

    The first line of a hunk block is its `@@` header and is skipped. `+++` file
    headers live in the header block, never inside a hunk, so they cannot appear here.
    """
    return [line[1:] for line in hunk.splitlines()[1:] if line.startswith("+")]


def _added_block_is_unique(lines: list[str], added: list[str]) -> bool:
    if not added:
        return False
    hits = 0
    for start in range(len(lines) - len(added) + 1):
        if lines[start:start + len(added)] == added:
            hits += 1
            if hits > 1:
                return False
    return hits == 1


def added_block_uniquely_present(worktree: Path, path: str, added: list[str]) -> bool:
    """Is this hunk's added text present at the target as ONE contiguous, in-order run?

    This is the fallback that separates CONTEXT-DRIFTED from CONTENT-LOST, and it is
    the only place where a hunk that failed reverse-apply can still be judged survived.
    It must therefore fail CLOSED: every uncertain answer returns False and the hunk
    stays CONTENT-LOST. The three refusals are the ones named in the module docstring --
    no added lines, non-contiguous or reordered, or a match that is not unique.
    """
    target_file = worktree / path
    if not target_file.is_file():
        return False
    try:
        lines = target_file.read_text().splitlines()
    except (OSError, UnicodeDecodeError):
        return False
    return _added_block_is_unique(lines, added)


def added_block_uniquely_present_at_target(
    repo: Path, target: str, path: str, added: list[str]
) -> bool:
    """Check one committed target without consulting the caller's working tree."""
    content = _git(repo, "show", f"{target}:{path}")
    if content.returncode != 0:
        return False
    return _added_block_is_unique(content.stdout.splitlines(), added)


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
    # the caller happens to have checked out. A private temporary index loaded from the
    # target asks that exact question without creating an unregistered worktree or
    # touching either the caller's index or working tree.
    missing: list[str] = []
    drifted: list[str] = []
    present = 0
    with tempfile.TemporaryDirectory(prefix="content-presence-index-") as td:
        index = Path(td) / "index"
        environment = git_env.sanitized_git_env()
        environment["GIT_INDEX_FILE"] = str(index)
        loaded = bounded_capture.git(["read-tree", target], cwd=repo, env=environment)
        if loaded.returncode != 0:
            return Presence(
                sha=sha,
                target=target,
                verdict=INDETERMINATE,
                is_ancestor=True,
                hunks_total=len(hunks),
                reason=(
                    "could not load the target into a private Git index: "
                    f"{loaded.stderr.strip()[:200]}"
                ),
            )
        for path, header, hunk in hunks:
            patch = header + hunk
            if not patch.endswith("\n"):
                patch += "\n"
            rev = bounded_capture.git(
                ["apply", "--cached", "--reverse", "--check", "-"],
                cwd=repo,
                input=patch,
                env=environment,
            )
            if rev.returncode == 0:
                present += 1
                continue
            # The hunk did not reverse-apply. Before calling that a loss, ask the
            # weaker question the reverse-apply cannot: are this hunk's ADDED lines
            # actually there? Only a confirmed yes downgrades it to context drift.
            label = f"{path}@{hunk.splitlines()[0].strip()}"
            if added_block_uniquely_present_at_target(
                repo, target, path, added_lines(hunk)
            ):
                drifted.append(label)
            else:
                missing.append(label)

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

    # ORDER MATTERS AND IS NOT COSMETIC. Any unconfirmable hunk decides the verdict, even
    # when other hunks merely drifted. A commit that lost one hunk and drifted another has
    # lost content, and reporting the gentler outcome would be exactly the concealment this
    # module exists to prevent.
    if missing:
        return Presence(
            sha=sha,
            target=target,
            verdict=CONTENT_LOST,
            is_ancestor=True,
            hunks_total=len(hunks),
            hunks_present=present,
            missing=tuple(missing),
            drifted=tuple(drifted),
            reason=(
                f"THE SHA IS AN ANCESTOR BUT {len(missing)} OF {len(hunks)} HUNK(S) "
                f"ARE NOT PRESENT AT THE TARGET. Ancestry passes this; the content did not "
                f"survive intact. Adjudicate: either the hunk was dropped by a lossy "
                f"reconcile, or a later commit legitimately superseded that region."
                + (
                    f" A FURTHER {len(drifted)} hunk(s) only drifted in context and are "
                    "confirmed present; they are not the problem."
                    if drifted
                    else ""
                )
            ),
        )

    return Presence(
        sha=sha,
        target=target,
        verdict=CONTEXT_DRIFTED,
        is_ancestor=True,
        hunks_total=len(hunks),
        hunks_present=present,
        drifted=tuple(drifted),
        reason=(
            f"{len(drifted)} of {len(hunks)} hunk(s) no longer reverse-apply because "
            f"later commits moved their surrounding context, but every line those hunks "
            f"ADD is present at the target as a unique contiguous run. The content "
            f"survived; only its context moved. NOT content loss, and not a clean match "
            f"either -- reported as its own outcome so neither claim is made for it."
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
