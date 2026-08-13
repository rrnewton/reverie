#!/usr/bin/env python3
"""Bind "awaiting landing" to REPOSITORY STATE instead of to a tag.

THE DEFECT
----------
`operational_health.py` computes the headline `awaiting_land` count from
`TaskRecord.awaiting_landing`, which is literally `return self.implemented` — the
`implemented` TAG and nothing else. Measured 2026-08-07 on the live TaskGraph:

    awaiting_land = 1625
      CLOSED       1610
      BACKLOG        11
      IN_PROGRESS     3
      OPEN            1

**A CLOSED TASK CANNOT BE AWAITING LANDING.** 1610 of 1625 rows are finished work whose
tag was simply never removed, so the headline reads as an enormous undrained backlog and
shapes priority accordingly. It is the same tag-as-proxy defect already found once in
`the-unlanded-count-query-misses-closed-tasks-whose-commits-never-landed`.

WHY EXCLUDING CLOSED ROWS WOULD NOT BE A FIX
--------------------------------------------
It trades one tag-proxy for a status-proxy. Status does not know whether the commit
landed either: under the close-on-implemented lifecycle a task is CLOSED the moment it is
implemented, *before* landing, so CLOSED rows legitimately include genuinely-unlanded
work. The binding has to be to the COMMIT.

THE BINDING
-----------
For each task tagged `implemented`, take the 40-hex SHA recorded in its notes and ask
whether that commit is an ancestor of the target repository's `main`. Ancestry is
evaluated by set membership against `git rev-list <main>`, built once per repo — measured
at 1.2s for all three repos together, versus one subprocess per task otherwise.

  AWAITING       tagged, SHA recorded, and NOT an ancestor of any candidate main.
  LANDED         tagged, SHA recorded, and IS an ancestor. EXCLUDED from the count even
                 though the tag is still present — that exclusion is the whole point.
  INDETERMINATE  tagged, but no 40-hex SHA is recoverable from its notes, or the SHA is
                 unknown to every candidate repository.

INDETERMINATE IS A REAL THIRD OUTCOME, NOT A ROUNDING ERROR. Measured: 1,395 of 1,626
implemented tasks carry a recoverable SHA, so **231 do not**. Those cannot be called
awaiting (no evidence they are unlanded) or landed (no evidence they are). Folding them
into either bucket would manufacture a number, so they are reported separately and the
count is published with its composition.

WHAT THIS DOES NOT CLAIM
------------------------
Ancestry proves the commit is REACHABLE, not that its content survived — see
`ci-hub/closure/content_presence.py` for that distinction and for the lossy-reconcile case
where ancestry passes and the content is gone. This module answers "did the commit reach
main", which is strictly the question the awaiting-land count is asking, and no more.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Sequence
# One resolver for the TaskGraph, shared with every other ci-hub consumer.
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ci-hub" / "lib"))
import taskgraph_db  # noqa: E402

SHA40 = re.compile(r"\b[0-9a-f]{40}\b")

AWAITING = "awaiting"
LANDED = "landed"
INDETERMINATE = "indeterminate"

#: Repositories a recorded SHA might belong to, and the ref that means "landed" there.
#: EVERY repository a recorded SHA might belong to must be listed. Omitting one does not
#: produce a gap -- it produces a FALSE "awaiting", because an unknown SHA is
#: indistinguishable from an unlanded one. Caught while validating this module: 2 of 12
#: sampled awaiting rows carried SHAs that exist only in `agent-utils`, which an earlier
#: draft did not index, and were therefore reported as unlanded work that does not exist.
DEFAULT_TARGETS: tuple[tuple[str, str], ...] = (
    (".", "origin/main"),
    ("hermit", "origin/main"),
    ("reverie", "origin/main"),
    ("agent-utils", "origin/main"),
    ("liteinst2", "origin/main"),
)


@dataclass(frozen=True)
class TaggedTask:
    id: str
    status: str
    recorded_sha: str = ""

    @property
    def closed(self) -> bool:
        return self.status.strip().upper() == "CLOSED"


@dataclass
class Composition:
    """The count and what it is made of. Never one without the other."""

    awaiting: tuple[TaggedTask, ...] = field(default_factory=tuple)
    landed_still_tagged: tuple[TaggedTask, ...] = field(default_factory=tuple)
    indeterminate: tuple[TaggedTask, ...] = field(default_factory=tuple)

    @property
    def total_tagged(self) -> int:
        return (
            len(self.awaiting) + len(self.landed_still_tagged) + len(self.indeterminate)
        )

    def render(self) -> str:
        def by_status(rows: Sequence[TaggedTask]) -> str:
            counts: dict[str, int] = {}
            for row in rows:
                key = row.status.strip().upper() or "?"
                counts[key] = counts.get(key, 0) + 1
            return " ".join(f"{k}={v}" for k, v in sorted(counts.items())) or "-"

        return (
            f"AWAITING_LAND={len(self.awaiting)}"
            f"  (of {self.total_tagged} tagged `implemented`)\n"
            f"  awaiting            {len(self.awaiting):5d}   {by_status(self.awaiting)}\n"
            f"  landed_still_tagged {len(self.landed_still_tagged):5d}"
            f"   {by_status(self.landed_still_tagged)}   <- EXCLUDED from the count\n"
            f"  indeterminate       {len(self.indeterminate):5d}"
            f"   {by_status(self.indeterminate)}   <- no recoverable SHA; NOT counted "
            f"either way"
        )

    def to_dict(self) -> dict:
        return {
            "awaiting_land": len(self.awaiting),
            "total_tagged_implemented": self.total_tagged,
            "composition": {
                "awaiting": len(self.awaiting),
                "landed_still_tagged": len(self.landed_still_tagged),
                "indeterminate": len(self.indeterminate),
            },
            "awaiting_ids": [t.id for t in self.awaiting],
            "indeterminate_ids": [t.id for t in self.indeterminate],
        }


class AncestorIndex:
    """Membership over every commit reachable from each candidate `main`.

    Built with one `git rev-list` per repository rather than one
    `merge-base --is-ancestor` per task: at ~1,400 tagged tasks the per-task form is
    ~1,400 subprocesses, and the set form is three.
    """

    def __init__(self, root: Path, targets: Iterable[tuple[str, str]] = DEFAULT_TARGETS):
        self.reachable: set[str] = set()
        self.sources: dict[str, int] = {}
        for rel, ref in targets:
            repo = root / rel if rel != "." else root
            if not (repo / ".git").exists() and not (repo / "HEAD").exists():
                continue
            proc = subprocess.run(
                ["git", "-C", str(repo), "rev-list", ref],
                capture_output=True,
                text=True,
                check=False,
                # A promisor/partial clone can lazily fetch during rev-list and stall;
                # this keeps the index build local and bounded.
                env={"GIT_NO_LAZY_FETCH": "1", "PATH": "/usr/bin:/bin"},
            )
            if proc.returncode != 0:
                self.sources[f"{rel}:{ref}"] = -1
                continue
            shas = {line.strip() for line in proc.stdout.splitlines() if line.strip()}
            self.sources[f"{rel}:{ref}"] = len(shas)
            self.reachable |= shas

    def contains(self, sha: str) -> bool:
        return sha in self.reachable


def extract_sha(text: str) -> str:
    """First 40-hex token in the task's recorded evidence, or ''.

    Deliberately first-match: the IMPLEMENTED note records the handoff SHA before any
    later commentary, and taking the last match would drift onto whatever SHA a
    subsequent comment happened to mention.
    """
    match = SHA40.search(text or "")
    return match.group(0) if match else ""


def classify(tasks: Sequence[TaggedTask], index: AncestorIndex) -> Composition:
    awaiting, landed, indeterminate = [], [], []
    for task in tasks:
        if not task.recorded_sha:
            indeterminate.append(task)
        elif index.contains(task.recorded_sha):
            landed.append(task)
        else:
            # Not reachable from any candidate main. That is the honest reading of
            # "awaiting landing" -- but note it also covers a SHA belonging to a repo
            # this index does not cover, which is why the target list is explicit.
            awaiting.append(task)
    return Composition(
        awaiting=tuple(awaiting),
        landed_still_tagged=tuple(landed),
        indeterminate=tuple(indeterminate),
    )


def load_tagged_tasks(root: Path) -> list[TaggedTask]:
    """Every task tagged `implemented`, with the SHA recorded in its notes."""
    sql = """
SELECT json_object(
  'id', t.local_id,
  'status', t.status,
  'evidence', COALESCE((
      SELECT group_concat(n.content, char(10))
      FROM task_notes n WHERE n.task_id = t.local_id
  ), '')
) AS row_json
FROM tasks t
WHERE EXISTS (
  SELECT 1 FROM json_each(t.tags) WHERE json_each.value = 'implemented'
)
ORDER BY t.local_id
""".strip()
    try:
        env = taskgraph_db.child_env(taskgraph_db.resolve())
    except taskgraph_db.TaskGraphUnavailable as error:
        raise RuntimeError(f"taskgraph query unavailable: {error}") from error
    proc = subprocess.run(
        ["tg", "sql", sql], cwd=root, capture_output=True, text=True, check=False,
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"taskgraph query failed: {proc.stderr.strip()[:200]}")
    out: list[TaggedTask] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            raw = json.loads(line)
        except json.JSONDecodeError:
            continue
        out.append(
            TaggedTask(
                id=str(raw.get("id") or ""),
                status=str(raw.get("status") or ""),
                recorded_sha=extract_sha(str(raw.get("evidence") or "")),
            )
        )
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    p.add_argument("--json", action="store_true")
    a = p.parse_args(argv)

    tasks = load_tagged_tasks(a.root)
    index = AncestorIndex(a.root)
    comp = classify(tasks, index)
    if a.json:
        payload = comp.to_dict()
        payload["ancestor_index"] = index.sources
        print(json.dumps(payload, sort_keys=True))
    else:
        print(comp.render())
        print(f"  ancestor index: {index.sources}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
