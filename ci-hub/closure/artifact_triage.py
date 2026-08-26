#!/usr/bin/env python3
"""Triage the UNKNOWN bucket for artifact-based closure. Classify first, close second.

Ancestry is STRUCTURALLY the wrong test for a research task: its deliverable is a
durable artifact, not a merge commit, so it reads NOT-LANDED forever and inflates
the drain. This finds the (task, artifact) pairs that `close-task --artifact` can
verify, and -- just as important -- the ones it cannot.

THIS DOES NOT CLOSE ANYTHING. It emits a plan. Closing goes through
`ci-hub/bin/close-task --artifact`, which independently re-verifies (file, inside
the versioned workspace, version-controlled, content commit an ancestor of parent
main) and refuses otherwise. Two gates, not one: if my binding check is wrong the
gateway still refuses, and if the gateway is wrong my check still had to agree.

WHY A PATH STRING IS NOT EVIDENCE. The failure running through this whole session
is a name that matches while nothing binds it to the fact it asserts. A path
mentioned in a note may be someone else's artifact, a plan for a file never
written, or a doc that has nothing to do with the task's Verify field. So each
candidate must clear, in order:

  1. NOT EPHEMERAL. /tmp and scratch/ are rejected outright. /tmp was reaped
     mid-session today and destroyed working binaries twice; an artifact there is
     not evidence of anything tomorrow.
  2. RESOLVES. The path exists as a file on disk.
  3. VERSION-CONTROLLED and PUBLISHED. Tracked, and its last content commit is an
     ancestor of the repo's origin/main. A referenced-but-unpushed path is the
     same failure as an unbacked label.
  4. TOPICALLY BOUND. The artifact has to be plausibly THIS task's deliverable,
     not just a path that appeared in the notes: either the note names it as the
     deliverable (IMPLEMENTED/artifact/deliverable wording near the path), or the
     task's distinctive title words appear in the artifact itself.
  5. SUBSTANTIVE. Not a stub -- a few hundred bytes of prose is not an answer.

Anything failing 2 is ARTIFACT-MISSING: genuine unfinished work, reported as its
own bucket rather than folded in with bookkeeping gaps.
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import subprocess
import tempfile
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Resolved, never hardcoded: this literal named the predecessor's frozen
# database.  See ci-hub/lib/taskgraph_db.py.
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
import bounded_capture  # noqa: E402
import taskgraph_db  # noqa: E402

def _capture(command, **kwargs):
    """Deprecated local alias for lib/bounded_capture.capture.

    Kept only so this file's existing call sites read unchanged. The
    implementation moved to ci-hub/lib/bounded_capture.py because the same
    helper had been written independently in three files, and three copies of a
    subtle fix drift. Prefer `bounded_capture.capture` directly in new code.

    Capture output while waiting for THE CHILD, not for EOF on the pipe.

    ⚠️ `capture_output=True` hands the child a PIPE and reads to EOF, and EOF
    arrives when the LAST HOLDER closes it -- not when the child exits. A
    background grandchild that inherited the pipe blocks the caller after the
    child is already gone. git's `gc --auto` is the everyday trigger: on by
    default (gc.auto=6700) and detaching by default, so it inherits stderr. Every
    call routed through here shells out to git.

    Reproduced 2026-08-25: a child exiting immediately while leaving one
    background grandchild blocked capture_output indefinitely, while capturing to
    a file returned in 0.0s with identical stdout and exit code. A regular file
    has no EOF-waiting semantics, so wait() on the child is the only gate.

    NOT a timeout (bounds the wait and discards the reason) and NOT disabling
    auto-gc (hides the mechanism for the next background child).
    """
    return bounded_capture.capture(command, **kwargs)


EPHEMERAL = ("/tmp/", "scratch/", "ignored/")
MIN_BYTES = 400

PATHLIKE = re.compile(
    r"((?:/tmp/|scratch/|ignored/|ai_docs/|experiments/|compat-envelope/|ci-hub/|scripts/|docs/)"
    r"[A-Za-z0-9_./\-]+\.(?:md|csv|json|py|sh|rs|tsv|txt))"
)
STOP = {"the", "and", "for", "with", "that", "into", "not", "are", "is", "a", "of", "to",
        "in", "on", "by", "as", "it", "its", "be", "an", "or", "no", "from", "per",
        "task", "tasks", "test", "tests", "fix", "add", "run", "why", "how", "vs"}


def sh(*args: str, cwd: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return _capture(["git", "-C", str(cwd), *args])


def repo_for(path: str) -> tuple[Path, str]:
    if path.startswith("hermit/"):
        return ROOT / "hermit", path[len("hermit/"):]
    if path.startswith("reverie/"):
        return ROOT / "reverie", path[len("reverie/"):]
    return ROOT, path


def keywords(title: str) -> set[str]:
    words = re.findall(r"[a-z0-9]{4,}", (title or "").lower())
    return {w for w in words if w not in STOP}


def classify(task: str, title: str, notes: str) -> dict:
    cands = sorted(set(PATHLIKE.findall(notes)))
    if not cands:
        return {"task": task, "title": title, "state": "NO-ARTIFACT-NAMED",
                "detail": "no artifact path appears in any note"}

    rejected: list[str] = []
    for p in cands:
        if p.startswith(EPHEMERAL):
            rejected.append(f"{p}: EPHEMERAL location, not durable")
            continue
        repo, rel = repo_for(p)
        fp = repo / rel
        if not fp.is_file():
            rejected.append(f"{p}: does not resolve on disk")
            continue
        if sh("ls-files", "--error-unmatch", rel, cwd=repo).returncode != 0:
            rejected.append(f"{p}: exists but is UNTRACKED")
            continue
        cc = sh("log", "-1", "--format=%H", "--", rel, cwd=repo).stdout.strip()
        if not cc:
            rejected.append(f"{p}: tracked but no content commit")
            continue
        if sh("merge-base", "--is-ancestor", cc, "origin/main", cwd=repo).returncode != 0:
            rejected.append(f"{p}: content commit {cc[:12]} NOT on published origin/main")
            continue
        size = fp.stat().st_size
        if size < MIN_BYTES:
            rejected.append(f"{p}: only {size}B -- a stub, not an answer")
            continue
        # Binding: named as the deliverable, or the task's own words are in it.
        idx = notes.find(p)
        window = notes[max(0, idx - 160):idx].lower()
        named = any(k in window for k in
                    ("implemented", "artifact", "deliverable", "durable", "written to", "wrote"))
        kw = keywords(title)
        body = fp.read_text(errors="replace").lower()
        hits = {k for k in kw if k in body or k in p.lower()}
        bound = named or (kw and len(hits) >= max(1, len(kw) // 3))
        if not bound:
            rejected.append(
                f"{p}: NOT BOUND to this task -- not named as its deliverable and "
                f"only {len(hits)}/{len(kw)} title terms appear in it")
            continue
        return {"task": task, "title": title, "state": "CLOSABLE", "artifact": p,
                "content_commit": cc, "bytes": size,
                "binding": "named as deliverable" if named
                           else f"{len(hits)}/{len(kw)} title terms present",
                "also_rejected": rejected}

    missing = all("does not resolve" in r for r in rejected)
    return {"task": task, "title": title,
            "state": "ARTIFACT-MISSING" if missing else "NOT-CLOSABLE",
            "detail": "; ".join(rejected[:3])}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--audit-json", type=Path,
                    default=ROOT / "ignored/ancestry/standing-audit.json")
    ap.add_argument("--out", type=Path, default=ROOT / "ignored/ancestry/artifact-plan.json")
    ap.add_argument("--db", type=Path, default=None)
    args = ap.parse_args()

    try:
        db = taskgraph_db.resolve(args.db).path
    except taskgraph_db.TaskGraphUnavailable as error:
        print(f"UNVERIFIABLE: {error}", file=sys.stderr)
        return 2

    unknown = {r["task"] for r in json.load(open(args.audit_json))["results"]
               if r["bucket"] == "UNKNOWN"}
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    con.text_factory = lambda b: b.decode("utf-8", "replace")
    rows = con.execute(
        """select t.local_id, t.title, coalesce(group_concat(n.content,' ~~ '),'')
           from tasks t left join task_notes n on n.task_id=t.local_id
           where t.tags like '%"implemented"%'
           -- Close-on-implemented lifecycle: implemented tasks are CLOSED
           -- immediately, so filtering on IN_PROGRESS here selected ~nothing
           -- and silently made this audit inert. The `implemented` TAG is the
           -- population, at any status (same rule as status-log.rs
           -- classify_task and ci-hub/health awaiting_landing).
           group by t.local_id"""
    ).fetchall()

    plan = [classify(tid, title, notes) for tid, title, notes in rows if tid in unknown]
    counts = Counter(p["state"] for p in plan)
    print(f"UNKNOWN triaged: {len(plan)}")
    for s in ("CLOSABLE", "ARTIFACT-MISSING", "NOT-CLOSABLE", "NO-ARTIFACT-NAMED"):
        print(f"  {s:20s} {counts[s]:4d}")
    args.out.write_text(json.dumps(plan, indent=1) + "\n")
    print(f"\nwrote {args.out}  (NOTHING CLOSED -- close via ci-hub/bin/close-task --artifact)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
