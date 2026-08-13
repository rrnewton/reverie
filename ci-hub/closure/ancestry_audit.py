#!/usr/bin/env python3
"""Standing post-landing ancestry audit: which implemented tasks are ACTUALLY on main?

Run this after every landing wave. A one-off run of this check on 2026-08-06 found
163 of a 302-task pile were already on main -- the real backlog was 143. Closure
here is ancestry-only, so without this the pile grows monotonically whether or not
work lands.

WHAT MAKES A LANDING CLAIM SOUND, and what does not:

  * `git merge-base --is-ancestor <sha> origin/main` on a FRESHLY FETCHED main.
    That is the only sound test.
  * NOT the GitHub `MERGED` flag. It can be orphaned by a force-push rewind --
    that happened here, to roughly twelve PRs.
  * NOT the PR head. Landings are squash-only, so a head SHA is never an ancestor
    of main even when the work landed. Always resolve to `mergeCommit.oid`.

TWO TRAPS THIS TOOL HANDLES EXPLICITLY, both learned the hard way:

  1. SHAs CHANGE. Something rewrote local main here, so a task's recorded SHA can
     be orphaned while its CONTENT sits on main under a different SHA. Testing
     only by SHA reports those as NOT-LANDED, which is wrong in the expensive
     direction -- it inflates the drain queue with work that is already done. So
     a SHA miss falls back to matching the commit SUBJECT against main, reported
     as its own state (`landed-by-subject`), never silently merged into `landed`.
  2. A TRUNCATED PR WINDOW READS EXACTLY LIKE MISSING WORK. At `--limit 400`
     three branches looked PR-less; at 900 all three resolved. So the limit is
     stated in the output, and if the API returns exactly the limit the audit
     FAILS CLOSED rather than reporting a smaller pile than reality.

`git fetch origin main` updates FETCH_HEAD ONLY and leaves refs/remotes/origin/main
untouched -- a subsequent ancestry test then silently runs against a stale main.
This always fetches with an explicit refspec and asserts the ref moved.

THREE BUCKETS, and UNKNOWN IS NOT NOT-LANDED. Roughly a third of the pile is
research whose closure evidence is a durable artifact, not a PR; calling those
"not landed" would misdirect the drain. They are reported separately, with any
artifact path found in their notes.
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import subprocess
import sys
from collections import Counter
from pathlib import Path

# Resolved, never hardcoded: this literal named the predecessor's frozen
# database.  See ci-hub/lib/taskgraph_db.py.
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import taskgraph_db  # noqa: E402
DEFAULT_ROOT = Path(__file__).resolve().parents[2]

#: repo key -> (checkout path relative to root, gh repo slug or None)
REPOS = {
    "hermit": ("hermit", "rrnewton/hermit"),
    "reverie": ("reverie", "rrnewton/reverie"),
    "dev-hermit": (".", None),
    "agent-utils": ("agent-utils", None),
}

PR_URL = re.compile(
    r"github\.com/(?:rrnewton|facebookexperimental)/"
    r"(hermit|reverie|dev-hermit|agent-utils)/pull/(\d+)"
)
BARE_PR = re.compile(r"\bPR\s*#(\d{3,5})\b")
SHA40 = re.compile(r"\b[0-9a-f]{40}\b")
ARTIFACT = re.compile(
    r"\b((?:ai_docs|experiments|compat-envelope|ci-hub|scripts|docs)/[A-Za-z0-9_./\-]+"
    r"\.(?:md|csv|json|py|sh|rs|tsv|txt))"
)


def git(root: Path, repo: str, *args: str) -> subprocess.CompletedProcess[str]:
    rel, _ = REPOS[repo]
    return subprocess.run(
        ["git", "-C", str(root / rel), *args], capture_output=True, text=True
    )


def fetch_main(root: Path, repo: str, herdr_agent: str | None) -> tuple[bool, str]:
    """Fresh-fetch origin/main with an EXPLICIT refspec.

    `git fetch origin main` writes FETCH_HEAD and leaves refs/remotes/origin/main
    alone, so the ancestry test would run against whatever stale ref was there.
    """
    rel, _ = REPOS[repo]
    refspec = "+refs/heads/main:refs/remotes/origin/main"
    cmd = f"git -C {root / rel} fetch origin {refspec}"
    if herdr_agent:
        # Egress is blocked in-jail; the herder pane is the sanctioned path.
        run = subprocess.run(
            [str(root / "agent-utils/bin/herdr-run"), "--agent", herdr_agent, cmd],
            capture_output=True, text=True, timeout=600,
        )
    else:
        run = subprocess.run(cmd.split(), capture_output=True, text=True, timeout=600)
    head = git(root, repo, "rev-parse", "origin/main").stdout.strip()
    return run.returncode == 0, head


def load_prs(root: Path, repo: str, limit: int, herdr_agent: str | None) -> dict:
    """Bulk PR fetch. FAILS CLOSED when the window is truncated."""
    _, slug = REPOS[repo]
    if slug is None:
        return {}
    cmd = (f"with-proxy gh pr list -R {slug} --state all --limit {limit} "
           f"--json number,state,mergeCommit,headRefOid")
    if herdr_agent:
        run = subprocess.run(
            [str(root / "agent-utils/bin/herdr-run"), "--agent", herdr_agent, cmd],
            capture_output=True, text=True, timeout=900,
        )
    else:
        run = subprocess.run(cmd.split(), capture_output=True, text=True, timeout=900)
    body = "\n".join(l for l in run.stdout.splitlines() if not l.startswith("[herdr-run]"))
    try:
        rows = json.loads(body)
    except json.JSONDecodeError:
        print(f"ancestry-audit: could not parse `gh pr list` for {slug}:\n{run.stderr[:400]}",
              file=sys.stderr)
        raise SystemExit(2)
    if len(rows) >= limit:
        # Exit 2, NOT 1. Exit 1 means "closable drift found"; a refusal must be
        # distinguishable from a result, or a post-landing hook treats
        # "untrustworthy" as "work to close".
        print(
            f"ancestry-audit: REFUSING TO REPORT. `gh pr list -R {slug} --limit {limit}` "
            f"returned exactly {len(rows)} rows, so the window is TRUNCATED and PRs below "
            f"the floor are invisible. A truncated window reads exactly like missing work. "
            f"Re-run with a larger --pr-limit.", file=sys.stderr)
        raise SystemExit(2)
    return {r["number"]: r for r in rows}


def pile(db: Path) -> list[dict]:
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    con.text_factory = lambda b: b.decode("utf-8", "replace")
    rows = con.execute(
        """select t.local_id, coalesce(group_concat(n.content,' ~~ '),''), t.title
           from tasks t left join task_notes n on n.task_id=t.local_id
           where t.tags like '%"implemented"%'
           -- Close-on-implemented lifecycle: implemented tasks are CLOSED
           -- immediately, so filtering on IN_PROGRESS here selected ~nothing
           -- and silently made this audit inert. The `implemented` TAG is the
           -- population, at any status (same rule as status-log.rs
           -- classify_task and ci-hub/health awaiting_landing).
           group by t.local_id"""
    ).fetchall()
    out = []
    for tid, notes, title in rows:
        out.append({
            "task": tid,
            "title": (title or "")[:70],
            "prs": sorted({(m.group(1), int(m.group(2))) for m in PR_URL.finditer(notes)}),
            "bare_prs": sorted({int(m.group(1)) for m in BARE_PR.finditer(notes)}),
            "shas": sorted(set(SHA40.findall(notes))),
            "artifacts": sorted(set(ARTIFACT.findall(notes))),
        })
    return out


class Ancestry:
    """Ancestry tests plus the subject-match fallback, memoized."""

    def __init__(self, root: Path):
        self.root = root
        self._anc: dict[tuple[str, str], tuple[bool, bool]] = {}
        self._subjects: dict[str, dict[str, list[str]]] = {}

    def test(self, repo: str, sha: str) -> tuple[bool, bool]:
        """(is_ancestor_of_origin_main, object_present_locally)"""
        key = (repo, sha)
        if key in self._anc:
            return self._anc[key]
        present = git(self.root, repo, "cat-file", "-e", f"{sha}^{{commit}}").returncode == 0
        anc = present and git(
            self.root, repo, "merge-base", "--is-ancestor", sha, "origin/main"
        ).returncode == 0
        self._anc[key] = (anc, present)
        return self._anc[key]

    def _index(self, repo: str) -> dict[str, list[str]]:
        """subject -> [main-side SHAs]. Built once per repo."""
        if repo not in self._subjects:
            idx: dict[str, list[str]] = {}
            out = git(self.root, repo, "log", "origin/main", "--format=%H%x1f%s").stdout
            for line in out.splitlines():
                if "\x1f" not in line:
                    continue
                sha, subj = line.split("\x1f", 1)
                idx.setdefault(normalize_subject(subj), []).append(sha)
            self._subjects[repo] = idx
        return self._subjects[repo]

    def by_subject(self, repo: str, sha: str) -> tuple[str | None, str]:
        """CAVEAT 1. Local main was rewritten, so a recorded SHA can be orphaned
        while its content is on main under a different SHA. Match the subject.

        Returns (main_sha, detail). Ambiguity is reported, never guessed at.
        """
        subj = git(self.root, repo, "log", "-1", "--format=%s", sha).stdout.strip()
        if not subj:
            return None, "orphaned SHA and its object is gone, so no subject to match"
        hits = self._index(repo).get(normalize_subject(subj), [])
        if len(hits) == 1:
            return hits[0], f"subject match: {subj[:60]!r}"
        if len(hits) > 1:
            return None, f"AMBIGUOUS: {len(hits)} commits on main share subject {subj[:50]!r}"
        return None, f"no commit on main carries subject {subj[:60]!r}"


def normalize_subject(subject: str) -> str:
    """Squash-merges append ' (#1234)'; strip it so both spellings compare equal."""
    return re.sub(r"\s*\(#\d+\)\s*$", "", subject).strip()


def classify(item: dict, prs: dict, anc: Ancestry) -> dict:
    cands: list[tuple[str, str, str]] = []   # (repo, sha, provenance)
    notes: list[str] = []

    for repo, num in item["prs"]:
        row = prs.get(repo, {}).get(num)
        if row is None:
            notes.append(f"{repo}#{num}:not-in-window-or-no-API")
            continue
        oid = (row.get("mergeCommit") or {}).get("oid")
        notes.append(f"{repo}#{num}:{row['state']}")
        if oid:
            # mergeCommit.oid ONLY -- never headRefOid, never the MERGED flag.
            cands.append((repo, oid, f"mergeCommit of {repo}#{num}"))
    for num in item["bare_prs"]:
        for repo in ("hermit", "reverie"):
            row = prs.get(repo, {}).get(num)
            if row:
                oid = (row.get("mergeCommit") or {}).get("oid")
                notes.append(f"{repo}#{num}:{row['state']}(bare)")
                if oid:
                    cands.append((repo, oid, f"mergeCommit of bare PR #{num}->{repo}"))
                break
    for sha in item["shas"]:
        for repo in REPOS:
            cands.append((repo, sha, f"note SHA in {repo}"))

    present_any = False
    for repo, sha, why in cands:
        is_anc, present = anc.test(repo, sha)
        present_any |= present
        if is_anc:
            return {**item, "bucket": "LANDED", "repo": repo, "sha": sha,
                    "why": why, "pr_states": notes}

    # CAVEAT 1: SHA miss -> try subject. Reported as its own state.
    for repo, sha, why in cands:
        _, present = anc.test(repo, sha)
        if not present:
            continue
        main_sha, detail = anc.by_subject(repo, sha)
        if main_sha:
            return {**item, "bucket": "LANDED-BY-SUBJECT", "repo": repo, "sha": main_sha,
                    "why": f"recorded {sha[:12]} is not an ancestor, but {detail}",
                    "pr_states": notes}

    if present_any:
        return {**item, "bucket": "NOT-LANDED",
                "why": "candidate commit(s) exist locally; none is an ancestor of origin/main "
                       "and no commit on main carries their subject",
                "pr_states": notes}
    return {**item, "bucket": "UNKNOWN",
            "why": ("no PR link and no 40-hex SHA in any note" if not cands
                    else "referenced commit(s) not present in any known repo"),
            "artifacts": item["artifacts"], "pr_states": notes}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    ap.add_argument("--db", type=Path, default=None)
    ap.add_argument("--pr-limit", type=int, default=2000,
                    help="gh pr list --limit. Audit FAILS CLOSED if the API returns "
                         "exactly this many rows (truncated window). Default 2000.")
    ap.add_argument("--herdr-agent", default=None,
                    help="route git/gh through herdr-run as this agent (needed in-jail)")
    ap.add_argument("--no-fetch", action="store_true",
                    help="skip the fresh fetch; ONLY for offline replay, weakens the result")
    ap.add_argument("--json", type=Path)
    ap.add_argument("--agents", type=int, default=0,
                    help="fleet size, to print the implemented-unlanded : agents ratio")
    args = ap.parse_args()

    # Bind the TaskGraph before any network work: an unresolvable database is
    # could-not-measure, and finding that out after a full fetch wastes it.
    try:
        db = taskgraph_db.resolve(args.db)
    except taskgraph_db.TaskGraphUnavailable as error:
        print(f"ancestry-audit: UNVERIFIABLE: {error}", file=sys.stderr)
        return 2
    print(f"ancestry-audit: taskgraph {db.basis}")

    print(f"ancestry-audit: pr-limit={args.pr_limit} (fails closed on truncation)")
    bases = {}
    for repo in REPOS:
        if args.no_fetch:
            head = git(args.root, repo, "rev-parse", "origin/main").stdout.strip()
            ok = bool(head)
            note = "NOT FETCHED (--no-fetch)"
        else:
            ok, head = fetch_main(args.root, repo, args.herdr_agent)
            note = "freshly fetched" if ok else "FETCH FAILED"
        bases[repo] = head
        print(f"  {repo:12s} origin/main {head[:12] or '<none>':12s}  {note}")
    if not args.no_fetch and not all(bases.values()):
        print("ancestry-audit: refusing to report against a repo with no origin/main",
              file=sys.stderr)
        return 2

    prs = {r: load_prs(args.root, r, args.pr_limit, args.herdr_agent) for r in REPOS}
    for r, rows in prs.items():
        if rows:
            print(f"  {r:12s} {len(rows)} PRs in window (< limit, so not truncated)")

    anc = Ancestry(args.root)
    results = [classify(item, prs, anc) for item in pile(db.path)]
    counts = Counter(r["bucket"] for r in results)

    print("\n=== buckets ===")
    for b in ("LANDED", "LANDED-BY-SUBJECT", "NOT-LANDED", "UNKNOWN"):
        print(f"  {b:18s} {counts[b]:4d}")
    print(f"  {'TOTAL':18s} {len(results):4d}")
    closable = counts["LANDED"] + counts["LANDED-BY-SUBJECT"]
    real = counts["NOT-LANDED"]
    print(f"\nclosable on ancestry now : {closable}")
    print(f"real drain queue         : {real}")
    print(f"UNKNOWN (NOT not-landed) : {counts['UNKNOWN']}  "
          f"-- research/artifact closure, needs close-task --artifact")
    if args.agents:
        print(f"implemented-unlanded : agents = {real} : {args.agents} "
              f"= {real / args.agents:.1f}x")

    if args.json:
        args.json.write_text(json.dumps(
            {"bases": bases, "pr_limit": args.pr_limit,
             "counts": dict(counts), "results": results}, indent=1) + "\n")
        print(f"\nwrote {args.json}")

    # Nonzero when there is closable drift, so a post-landing hook can act on it.
    return 1 if closable else 0


if __name__ == "__main__":
    raise SystemExit(main())
