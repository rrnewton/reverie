"""Who is editing a shared working tree, recorded when they start.

WHY THIS EXISTS
---------------
TaskGraph records who owns a TASK, and through notes, which agent has DISCUSSED a
file. It records nothing about who made an UNCOMMITTED EDIT. A commit carries an
author and a disclosure tag; a dirty working tree carries neither. So a shared
checkout accumulates changes no query can attribute, and the only way to resolve
them is to ask every agent in turn.

Measured on `dev-hermit` 2026-08-25: eight tracked paths differed from
`origin/main` and not one had a claimant anywhere in the graph. Three independent
routes were tried and all three underdetermine it -- task titles, note text
mentioning the file, and the note stream at the edit's mtime. The third failed
most instructively: three of the paths share an mtime TO THE SECOND, so they are
one bulk operation rather than three edits, and no note in the surrounding
seventeen minutes mentions it. That is not a lapse in diligence. THERE IS NO
FIELD FOR IT.

WRITTEN AT EDIT TIME IS THE LOAD-BEARING PART. A claim inferred later from file
association is the guess that already failed -- it is how those eight became
unattributable in the first place. This module only records; it never infers an
owner for an edit that arrived without one, and `unclaimed` is a first-class
answer rather than a gap to be filled by the best-matching note.

THIS IS NOT A LOCK. Nothing here serialises anything, refuses an edit, or cares
whether two agents touch one file at once. The problem is unrecorded authorship,
not concurrent editing; serialising nineteen agents would trade an attribution
gap for a throughput collapse. Two claims on one path is a legitimate state and
is reported as two claimants.

WHY THE JOURNAL IS UNTRACKED AND LIVES BESIDE THE TREE
------------------------------------------------------
A claim is only meaningful for the working tree it describes: it must appear when
that tree goes dirty and stop mattering when the tree is reconciled. So the
journal sits at the checkout root and is NOT tracked.

If it were tracked it would itself be an uncommitted change with no recorded
owner, which is the exact defect it exists to close.

WHAT MAKES A CLAIM MORE THAN A NAME
-----------------------------------
Every claim carries `blob_before`: the git object id of the path's content AT THE
MOMENT THE CLAIM WAS MADE. This is the same discipline as the other four
convergent fixes -- binary identity in a test receipt, resolved-plus-corroborating
head in a landing receipt, the comment id written at post time, host
applicability recorded on a cell. RECORD WHAT YOU MEASURED AGAINST, AT THE MOMENT
YOU MEASURED IT.

It buys a distinction a bare name cannot make. Given a dirty path and a claim, a
reader can ask whether the content still descends from what the claimant started
on. If the file no longer matches and no later claim exists, SOMEONE ELSE EDITED
ON TOP without recording it -- which is a different finding from "unclaimed", and
one that a name-only claim would report as cleanly owned.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path
from typing import NamedTuple, Optional
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
import bounded_capture  # noqa: E402

SCHEMA = "edit-claim/v1"
JOURNAL_NAME = ".edit-claims.jsonl"


class Claim(NamedTuple):
    path: str
    agent: str
    team: str
    role: str
    intent: str
    claimed_at: str
    blob_before: Optional[str]
    pid: Optional[int]


def journal_path(root: Path) -> Path:
    return Path(root) / JOURNAL_NAME


def blob_id(root: Path, rel: str) -> Optional[str]:
    """git object id of the path's CURRENT content, or None if it does not exist.

    `git hash-object` rather than a plain digest so the value is directly
    comparable with anything git reports for the same content.
    """
    target = Path(root) / rel
    if not target.is_file():
        return None
    proc = bounded_capture.capture(
        ["git", "hash-object", "--", str(target)], cwd=str(root)
    )
    out = proc.stdout.strip()
    return out if proc.returncode == 0 and len(out) == 40 else None


def record_claim(
    root: Path,
    rel_paths: list[str],
    agent: str,
    team: str,
    role: str,
    intent: str,
    now: Optional[str] = None,
) -> list[Claim]:
    """Append one claim per path. Called BEFORE the edit, by whoever will edit.

    Appending, never rewriting: a superseded claim stays in the journal because
    the sequence of claimants on one path is itself the record. Collapsing them
    to "the current owner" would lose the handover that a reader most wants.
    """
    stamp = now or time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    claims: list[Claim] = []
    with journal_path(root).open("a", encoding="utf-8") as handle:
        for rel in rel_paths:
            claim = Claim(
                path=rel,
                agent=agent,
                team=team,
                role=role,
                intent=" ".join(intent.split()),
                claimed_at=stamp,
                blob_before=blob_id(root, rel),
                pid=os.getpid(),
            )
            handle.write(json.dumps({"schema": SCHEMA, **claim._asdict()}) + "\n")
            claims.append(claim)
    return claims


def read_claims(root: Path) -> list[Claim]:
    """Every well-formed claim, oldest first. Malformed lines are skipped.

    Skipping is safe HERE, unlike in the validate ledger, because a claim is
    additive evidence: a dropped line can only make a path look less attributed
    than it is, never more. The reporting path counts what it skipped so the
    reader is told rather than quietly given a smaller denominator.
    """
    path = journal_path(root)
    if not path.is_file():
        return []
    claims: list[Claim] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if not isinstance(row, dict) or row.get("schema") != SCHEMA:
            continue
        if not row.get("path") or not row.get("agent"):
            continue
        claims.append(
            Claim(
                path=str(row["path"]),
                agent=str(row["agent"]),
                team=str(row.get("team", "")),
                role=str(row.get("role", "")),
                intent=str(row.get("intent", "")),
                claimed_at=str(row.get("claimed_at", "")),
                blob_before=row.get("blob_before"),
                pid=row.get("pid"),
            )
        )
    return claims


def malformed_count(root: Path) -> int:
    path = journal_path(root)
    if not path.is_file():
        return 0
    bad = 0
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except ValueError:
            bad += 1
            continue
        if not isinstance(row, dict) or row.get("schema") != SCHEMA:
            bad += 1
    return bad


def dirty_paths(root: Path) -> list[str]:
    """Tracked paths differing from HEAD. Untracked files are deliberately out.

    An untracked file is visible as new and is not at risk of being silently
    destroyed by a reconciliation in the way a modified tracked file is, which is
    the harm this addresses.
    """
    proc = bounded_capture.capture(
        ["git", "diff", "--name-only", "HEAD"], cwd=str(root)
    )
    if proc.returncode != 0:
        raise RuntimeError(
            "cannot enumerate tracked edits: "
            + (proc.stderr.strip()[:160] or f"git diff exited {proc.returncode}")
        )
    return [p for p in proc.stdout.splitlines() if p.strip()]


class Attribution(NamedTuple):
    path: str
    claimants: list[Claim]
    unchanged_since_claim: bool

    @property
    def state(self) -> str:
        if not self.claimants:
            return "UNATTRIBUTED"
        if self.unchanged_since_claim:
            return "CLAIMED-BUT-UNCHANGED"
        return "CLAIMED"


def attribute(root: Path) -> list[Attribution]:
    """Pair each dirty path with its claims. Never guesses.

    ⚠️ WHAT A CLAIM DOES AND DOES NOT ESTABLISH, stated because overclaiming here
    would rebuild the defect one layer up. A claim proves that a named agent said
    it was about to edit this path, and what the content was at that instant. It
    does NOT prove the content now is that agent's work: another agent could edit
    afterwards without claiming, and nothing in this journal would show it.

    So `CLAIMED` means "a claimant is on record and the file moved after they
    claimed", which is the strongest statement the data supports. Detecting an
    unrecorded edit ON TOP of a claim would need a hash written when the claimant
    FINISHED, and nothing writes one -- so this does not pretend to detect it.

    The one mismatch it CAN see is the opposite case: the file is byte-identical
    to what the claimant found, so the claim does not account for the path being
    dirty at all. Either the claimed edit was never made, or the dirty content
    predates the claim and belongs to someone else. Named rather than folded into
    `CLAIMED`, because a claim that explains nothing should not read as ownership.
    """
    claims = read_claims(root)
    by_path: dict[str, list[Claim]] = {}
    for claim in claims:
        by_path.setdefault(claim.path, []).append(claim)

    out: list[Attribution] = []
    for rel in dirty_paths(root):
        mine = by_path.get(rel, [])
        unchanged = False
        if mine:
            newest = mine[-1]
            current = blob_id(root, rel)
            unchanged = (
                newest.blob_before is not None
                and current is not None
                and current == newest.blob_before
            )
        out.append(
            Attribution(path=rel, claimants=mine, unchanged_since_claim=unchanged)
        )
    return out
