#!/usr/bin/env python3
"""Emit the provenance of what a measurement READ, so a stale artifact cannot
pass for a discovery.

THREE STALE ARTIFACTS PRODUCED CONFIDENT WRONG READINGS IN ONE NIGHT (2026-08-07):

  1. A parent tree 273 COMMITS BEHIND origin/main produced TWO PHANTOM SCHEMA
     SKEWS. The emitters really did disagree with their targets -- in that tree.
     At origin/main they agreed.
  2. A `gh issue list --limit 400` that returned exactly 400 of 438 issues
     produced a FALSE "no duplicates". The 38 unsearched issues were invisible,
     and the answer was shaped exactly like a complete one.
  3. A hermit binary 23 COMMITS BEHIND main produced THREE FALSE NONDETERMINISM
     VERDICTS AGAINST THE GOLDEN REFERENCE, and a 60x-inflated comparator cost
     that had already spawned an 86-260 hour extrapolation and a cadence task.

WHAT MAKES THIS CLASS DANGEROUS. In every case the artifact was INTERNALLY
CONSISTENT and gave no signal it was stale. A surprising result from a stale
artifact looks IDENTICAL to a discovery -- same shape, same confidence, same
plausible mechanism to explain it. The reading is not wrong because the tool
misbehaved; the tool behaved perfectly against the wrong input.

So the defence cannot be "remember to check". It has to be an EMITTED FIELD:
every measurement states what it read, and a result with no provenance is not
reportable.

THE CAP CASE IS THE SUBTLEST OF THE THREE and gets its own emitter. A truncated
query does not error, does not warn, and returns a well-formed answer. It is
only detectable by comparing the row count against the limit -- which nothing
does unless it is written down. Every conclusion FROM ABSENCE ("no duplicates",
"no PR", "no orphans", "no other consumers") is invalid under a hit cap.

Pure functions plus subprocess reads. No writes, no network beyond the git
commands the caller already runs.
"""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path


class StaleArtifact(Exception):
    """A read artifact is too stale to report against. Never caught-and-defaulted."""


def _git(args: list[str], cwd: str | Path | None = None) -> str | None:
    try:
        r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.SubprocessError):
        return None
    return r.stdout.strip() if r.returncode == 0 else None


@dataclass(frozen=True)
class Provenance:
    """What was read, and how far from current it is. Renders as one line."""

    kind: str                 # "binary" | "tree" | "query"
    identity: str             # sha / path / query description
    behind: int | None = None # commits behind the reference ref, when knowable
    dirty: bool | None = None
    capped: bool | None = None
    detail: str = ""

    @property
    def suspect(self) -> bool:
        """True when this artifact could make a clean number wrong."""
        unknown = self.kind in {"binary", "tree"} and (
            self.behind is None or self.dirty is None
        )
        return unknown or bool(self.capped) or bool(self.dirty) or (self.behind or 0) > 0

    def render(self) -> str:
        bits = [f"{self.kind}={self.identity}"]
        if self.behind is not None:
            bits.append(f"behind={self.behind}")
        elif self.kind in {"binary", "tree"}:
            bits.append("behind=unknown")
        if self.dirty is not None:
            bits.append(f"dirty={str(self.dirty).lower()}")
        elif self.kind in {"binary", "tree"}:
            bits.append("dirty=unknown")
        if self.capped is not None:
            bits.append(f"capped={str(self.capped).lower()}")
        if self.detail:
            bits.append(self.detail)
        line = "provenance: " + "  ".join(bits)
        return ("WARNING " + line) if self.suspect else line


def binary_provenance(binary: str | Path, repo: str | Path, ref: str = "origin/main") -> Provenance:
    """Provenance of a BUILT BINARY, from what it self-reports.

    Asks the artifact, not a checkout: a checkout's HEAD says nothing about what
    a binary beside it was built from. `-dirty` means the commit does not
    identify the source at all.
    """
    try:
        out = subprocess.run([str(binary), "--version"], capture_output=True,
                             text=True, timeout=120)
        text = out.stdout if out.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return Provenance("binary", f"UNKNOWN(cannot-exec:{binary})", detail="UNATTRIBUTED")
    m = re.search(r",\s*g([0-9a-f]{7,40})(-dirty)?\)", text)
    if not m:
        return Provenance("binary", f"UNKNOWN(unparsable-version)", detail="UNATTRIBUTED")
    sha, dirty = m.group(1), bool(m.group(2))
    behind = None
    count = _git(["rev-list", "--count", f"{sha}..{ref}"], cwd=repo)
    if count is not None and count.isdigit():
        behind = int(count)
    return Provenance("binary", f"g{sha}", behind=behind, dirty=dirty)


def tree_provenance(repo: str | Path, ref: str = "origin/main") -> Provenance:
    """Provenance of a CHECKOUT: its head, and how far behind the reference."""
    head = _git(["rev-parse", "--short=12", "HEAD"], cwd=repo)
    if head is None:
        return Provenance("tree", f"UNKNOWN(not-a-repo:{repo})", detail="UNATTRIBUTED")
    behind = None
    count = _git(["rev-list", "--count", f"HEAD..{ref}"], cwd=repo)
    if count is not None and count.isdigit():
        behind = int(count)
    status = _git(["status", "--porcelain", "--untracked-files=no"], cwd=repo)
    dirty = None if status is None else bool(status)
    return Provenance("tree", head, behind=behind, dirty=dirty)


def query_provenance(description: str, limit: int, returned: int) -> Provenance:
    """Provenance of a LIMITED QUERY -- the subtlest of the three.

    A query that returns exactly its limit has almost certainly been TRUNCATED,
    and a truncated query answers "is X absent?" with a confident, well-formed,
    WRONG "yes". There is no error and no warning; the only signal is
    returned == limit, which is invisible unless something compares them.
    """
    capped = returned >= limit
    detail = f"returned={returned}/limit={limit}"
    if capped:
        detail += (" -- TRUNCATED: any conclusion FROM ABSENCE is INVALID; "
                   "re-run with a higher limit before concluding anything is missing")
    return Provenance("query", description, capped=capped, detail=detail)


def require_fresh(*provenances: Provenance, allow_behind: int = 0) -> None:
    """REFUSE to report when a read artifact is too stale. Fail-closed.

    `allow_behind` exists because "behind by some commits" is normal and usually
    harmless; being behind is only fatal when the caller says it is. A CAP or an
    UNATTRIBUTED artifact is always fatal, because neither has a safe reading.
    """
    problems = []
    for p in provenances:
        if p.capped:
            problems.append(f"{p.kind} {p.identity}: query was TRUNCATED ({p.detail})")
        if "UNKNOWN" in p.identity:
            problems.append(f"{p.kind}: UNATTRIBUTED -- {p.identity}")
        if p.kind in {"binary", "tree"} and p.behind is None:
            problems.append(f"{p.kind} {p.identity}: distance from target is UNKNOWN")
        if p.kind in {"binary", "tree"} and p.dirty is None:
            problems.append(f"{p.kind} {p.identity}: working-tree state is UNKNOWN")
        if (p.behind or 0) > allow_behind:
            problems.append(
                f"{p.kind} {p.identity}: {p.behind} commits behind (allowed {allow_behind})"
            )
    if problems:
        raise StaleArtifact(
            "refusing to report against a stale artifact:\n  " + "\n  ".join(problems)
            + "\nA surprising result from a stale artifact looks identical to a discovery."
        )


def stamp(*provenances: Provenance) -> str:
    """The block every measurement should print beside its numbers."""
    return "\n".join(p.render() for p in provenances)
