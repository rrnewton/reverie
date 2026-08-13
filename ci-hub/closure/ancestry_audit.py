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
           f"--json number,state,mergeCommit,headRefOid,headRefName")
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
            # The raw note text, so PR attribution can look for the SECOND
            # signal. Without it a PR URL is a lone mention, which is exactly
            # how the phantom LANDED survived the first fix.
            "text": notes,
        })
    return out


class Ancestry:
    """Ancestry tests plus the subject-match fallback, memoized."""

    def __init__(self, root: Path):
        self.root = root
        self._anc: dict[tuple[str, str], tuple[bool, bool]] = {}
        self._subjects: dict[str, dict[str, list[str]]] = {}
        self._claims: dict[tuple[str, str, str], bool] = {}

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

    def claims_task(self, repo: str, sha: str, task: str) -> bool:
        """Does THIS COMMIT say it implements THIS TASK?

        AGENTS.md requires an implementing commit to carry `Task: <task-id>`.
        That trailer is the only thing in the system by which a commit ASSERTS
        which task it is, so it is the attribution used here for any SHA that
        did not arrive through a task-linked pull request.
        """
        key = (repo, sha, task)
        if key in self._claims:
            return self._claims[key]
        body = git(self.root, repo, "log", "-1", "--format=%B", sha).stdout
        found = bool(
            body
            and re.search(
                rf"(?mi)^\s*Task:\s*`?{re.escape(task)}`?\s*$", body
            )
        )
        self._claims[key] = found
        return found

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


# Characters that can be part of a branch-name-shaped token. A match flanked by
# one of these is a FRAGMENT of a longer token, not a reference to the branch.
_BRANCH_TOKEN = r"A-Za-z0-9._/\-"


def _bounded(needle: str, haystack: str, token_class: str) -> bool:
    """Is `needle` present in `haystack` as a WHOLE token?

    *** THE UNANCHORED VERSION WAS CORRECT ONLY BY ACCIDENT. ***
    This used to be `needle in haystack`. hermit-903 found it by EXECUTING
    pr_attribution rather than reading it, and its diagnosis is the reason this
    function exists: the gate held because of a NAMING CONVENTION plus
    REDUNDANT COVERAGE, not because the code checked anything. Measured at the
    time: 200 hermit pull requests sampled, shortest branch 18 characters, all
    multi-token, and of the 19 pull-request-attributed LANDED verdicts the 16
    carrying `branch` ALSO carried `head_sha`. So the hole could not have
    produced a verdict in that run -- and nothing forbids the branch name `fix`,
    which would match "prefix", "fixture", and "bugfix-notes" on the next run.
    A convention nobody enforces is not a check.

    `head_sha` shares the shape and is anchored here too, for the same reason
    rather than a measured miss: `head[:12] in text` can match inside a LONGER
    hex run, and 30 task notes in the live database carry hex runs over 40
    characters -- sha256 digests, which this tool's own seal records emit. Hex
    uses a hex-only boundary so a full 40-hex SHA still matches when it sits
    next to `=` or a bracket, while a 12-hex prefix buried at an offset inside a
    64-hex digest does not.
    """
    return re.search(
        rf"(?<![{token_class}]){re.escape(needle)}(?![{token_class}])", haystack
    ) is not None


def pr_attribution(item: dict, row: dict, *, url_seen: bool) -> tuple[str, list[str]]:
    """Is THIS pull request attributable to THIS task? Agreement, not mention.

    *** THIS IS THE SECOND HALF OF THE 2026-08-12 FIX AND THE MORE INSTRUCTIVE
    HALF. *** The first pass closed every SHA-shaped route to a phantom LANDED
    and left the URL-shaped one open, because `pile()` builds `item["prs"]`
    from `PR_URL.finditer(notes)` -- ANY full pull-request URL appearing in any
    note. That was read as "linked to the task" when it only ever meant
    "mentioned somewhere". hermit-904's independent verification reproduced the
    phantom LANDED with an unrelated full URL. The lesson recorded here for the
    next reader: the first fix generalised over the modes that had been
    DIAGNOSED (two of them SHA-shaped) instead of over the mechanism that
    produced them, so the one route with a single example stayed open.

    The standard is not invented here. `ci-hub/landing/task_pr_join.py` already
    settled it for the same question -- which task owns which PR -- after both
    single-signal methods failed, and its rule is AGREEMENT ACROSS INDEPENDENT
    SIGNALS: at least one STRONG identity signal AND at least two signals in
    total. A lone STRONG signal is PROBABLE, never CONFIRMED, and only
    CONFIRMED may authorise a close. A bare `#N` is WEAK there ("a rollup
    mention is not ownership") and is weak here.

        STRONG   pr_url    the full URL is in the task text
                 head_sha  the PR head SHA, or its 12-hex prefix, is in the text
                 branch    the PR head branch name appears verbatim
        WEAK     pr_num    a bare `#N` -- corroborator only, never alone

    Measured on the live pile when this landed: of the 19 LANDED verdicts that
    rested on a pull request, 17 are CONFIRMED and 2 are lone-URL PROBABLE. So
    the standard keeps genuinely landed work binding while refusing exactly the
    shape 904 exploited.
    """
    text = item.get("text") or ""
    num = row.get("number")
    strong: list[str] = []
    weak: list[str] = []
    if url_seen:
        strong.append("pr_url")
    head = row.get("headRefOid") or ""
    if head and (
        _bounded(head, text, "0-9a-fA-F")
        or _bounded(head[:12], text, "0-9a-fA-F")
    ):
        strong.append("head_sha")
    branch = row.get("headRefName") or ""
    if branch and _bounded(branch, text, _BRANCH_TOKEN):
        strong.append("branch")
    if num is not None and re.search(rf"#{num}\b", text):
        weak.append("pr_num")
    signals = strong + weak
    # DELIBERATE DEVIATION, STRICTER THAN task_pr_join's ">=1 strong and >=2
    # total": here the second signal must itself be STRONG. `pr_url` and a bare
    # `#N` routinely come from THE SAME textual mention -- "see
    # .../pull/2071 (#2071)" fires both -- so counting them as two is one
    # mention double-counted, not independent agreement, and this whole defect
    # class is mentions being read as attributions. `head_sha` and `branch` are
    # facts about the PR that a mention does not carry.
    #
    # MEASURED COST ON THE LIVE PILE: ZERO. Before and after the gate the audit
    # reports the same 19 PR-attributed LANDED verdicts, because every one of
    # them carries head_sha and/or branch agreement in its notes -- a validation
    # receipt quotes the head, and a landing note names the branch. An earlier
    # estimate here said "15 instead of 17"; that came from an offline harness
    # that split `tg sql` output on a separator multi-line notes break, so it
    # truncated each task's text and under-counted signals. The running code was
    # right and the estimate was wrong; the number above is what the audit
    # actually emits. The gate costs nothing on real data and closes the route
    # hermit-904 exploited.
    if len(strong) >= 2:
        return "CONFIRMED", signals
    if strong:
        return "PROBABLE", signals
    return "NONE", signals


def classify(item: dict, prs: dict, anc: Ancestry) -> dict:
    """Bucket one task by whether ITS OWN change landed.

    *** ANCESTRY ALONE IS NOT EVIDENCE, AND THAT IS THE WHOLE POINT. ***

    Until 2026-08-12 this collected every SHA and PR number that appeared
    anywhere in a task's notes, tested each for ancestry of `origin/main`, and
    returned LANDED on the first hit. Measured against the live pile it
    reported 58 LANDED and 0 NOT-LANDED while seventeen pull requests sat open
    and unlandable, because a commit does not have to be the task's change to
    be an ancestor of main:

      * `p1_compat_lsof_is` was LANDED on `why="note SHA in hermit"`,
        sha `1f4e430c1352` -- which is hermit MAIN'S OWN TIP, quoted in a note
        and trivially an ancestor of itself -- while the same record carried
        `pr_states=["hermit#2221:OPEN"]`.
      * `compat_bin_c_robust` and `p1_promoting_an_e2e` were LANDED on the
        mergeCommit of bare PR #2071, a DIFFERENT pull request mentioned in
        passing, while their own #2223 and #2220 were open.
      * `port-peer-snapshot-protocol-into-validate-rs` was LANDED on a
        dev-hermit SHA while its hermit PR #2222 was open.

    Acting on the resulting "closable on ancestry now: 58" would have closed 58
    tasks whose work was not landed -- undoing the close-only-after-landing rule
    automatically, at scale, and looking like routine cleanup.

    THE ORDER MATTERS. Identify WHICH commit is the change, THEN test ancestry.
    Reversing it reproduces the defect in better-looking code. A candidate is
    admissible only when something binds it to THIS task:

      linked-pr     the pull request is linked to the task and we take its
                    `mergeCommit.oid`, in that pull request's OWN repository.
      task-trailer  the commit message carries `Task: <task-id>`, i.e. the
                    commit itself asserts which task it implements.

    A SHA merely quoted in a note, the mergeCommit of an incidentally-mentioned
    bare PR, and a SHA tried against a repository it did not come from are all
    NON-EVIDENCE. They are still listed in `refused` so the record shows what
    was inspected and why it did not count.

    When candidates exist but none is admissible the bucket is
    NOT-DETERMINABLE, never LANDED and never NOT-LANDED. A stalled task costs a
    prompt; a wrongly closed one costs the work.
    """
    task = item["task"]
    admissible: list[tuple[str, str, str]] = []   # (repo, sha, provenance)
    refused: list[str] = []
    notes: list[str] = []
    seen: set[tuple[str, str]] = set()

    def consider(repo: str, sha: str, why: str, *, linked: bool) -> None:
        if (repo, sha) in seen:
            return
        seen.add((repo, sha))
        if linked:
            admissible.append((repo, sha, why))
        elif anc.claims_task(repo, sha, task):
            admissible.append((repo, sha, f"{why}; commit carries Task: {task}"))
        else:
            refused.append(f"{why} [{sha[:12]}]: not attributable to this task")

    # A. Pull requests whose URL appears in the task text. A URL IS A MENTION,
    #    NOT AN ATTRIBUTION -- it counts only when a second independent signal
    #    agrees. See pr_attribution().
    for repo, num in item["prs"]:
        row = prs.get(repo, {}).get(num)
        if row is None:
            notes.append(f"{repo}#{num}:not-in-window-or-no-API")
            continue
        notes.append(f"{repo}#{num}:{row['state']}")
        oid = (row.get("mergeCommit") or {}).get("oid")
        if not oid:
            continue
        verdict, signals = pr_attribution(item, {**row, "number": num}, url_seen=True)
        if verdict == "CONFIRMED":
            # mergeCommit.oid ONLY -- never headRefOid, never the MERGED flag.
            consider(repo, oid,
                     f"mergeCommit of {repo}#{num} CONFIRMED by {'+'.join(signals)}",
                     linked=True)
        else:
            refused.append(
                f"mergeCommit of {repo}#{num} [{oid[:12]}]: pull request is "
                f"{verdict} for this task ({'+'.join(signals) or 'no signal'}); "
                f"a mention is not an attribution")

    # B. Bare PR numbers found in prose. WEAK by construction -- it can never
    #    reach CONFIRMED on its own, so it only counts if the merge commit
    #    itself claims the task.
    for num in item["bare_prs"]:
        for repo in ("hermit", "reverie"):
            row = prs.get(repo, {}).get(num)
            if not row:
                continue
            notes.append(f"{repo}#{num}:{row['state']}(bare)")
            oid = (row.get("mergeCommit") or {}).get("oid")
            if oid:
                consider(repo, oid, f"mergeCommit of bare PR #{num}->{repo}", linked=False)
            break

    # C. Bare SHAs found in prose, in any repository. Non-evidence on their own.
    for sha in item["shas"]:
        for repo in REPOS:
            if anc.test(repo, sha)[1]:
                consider(repo, sha, f"note SHA in {repo}", linked=False)

    present_any = False
    for repo, sha, why in admissible:
        is_anc, present = anc.test(repo, sha)
        present_any |= present
        if is_anc:
            return {**item, "bucket": "LANDED", "repo": repo, "sha": sha,
                    "why": why, "refused": refused, "pr_states": notes}

    # CAVEAT 1: SHA miss -> try subject, but ONLY for an admissible candidate.
    for repo, sha, why in admissible:
        if not anc.test(repo, sha)[1]:
            continue
        main_sha, detail = anc.by_subject(repo, sha)
        if main_sha:
            return {**item, "bucket": "LANDED-BY-SUBJECT", "repo": repo, "sha": main_sha,
                    "why": f"{why}; recorded {sha[:12]} is not an ancestor, but {detail}",
                    "refused": refused, "pr_states": notes}

    if admissible:
        if present_any:
            return {**item, "bucket": "NOT-LANDED",
                    "why": "attributable commit(s) exist locally; none is an ancestor of "
                           "origin/main and no commit on main carries their subject",
                    "refused": refused, "pr_states": notes}
        return {**item, "bucket": "UNKNOWN",
                "why": "attributable commit(s) are not present in any known repo",
                "artifacts": item["artifacts"], "refused": refused, "pr_states": notes}

    if refused:
        return {**item, "bucket": "NOT-DETERMINABLE",
                "why": (f"{len(refused)} candidate(s) inspected, none attributable to this "
                        f"task: no linked-PR mergeCommit and no commit carrying "
                        f"Task: {task}"),
                "artifacts": item["artifacts"], "refused": refused, "pr_states": notes}
    return {**item, "bucket": "UNKNOWN",
            "why": "no PR link and no 40-hex SHA in any note",
            "artifacts": item["artifacts"], "refused": refused, "pr_states": notes}


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
    for b in ("LANDED", "LANDED-BY-SUBJECT", "NOT-LANDED",
              "NOT-DETERMINABLE", "UNKNOWN"):
        print(f"  {b:18s} {counts[b]:4d}")
    print(f"  {'TOTAL':18s} {len(results):4d}")
    closable = counts["LANDED"] + counts["LANDED-BY-SUBJECT"]
    real = counts["NOT-LANDED"]
    print(f"\nclosable on ancestry now : {closable}")
    print(f"real drain queue         : {real}")
    print(f"NOT-DETERMINABLE         : {counts['NOT-DETERMINABLE']}  "
          f"-- candidates inspected, none attributable to the task; NOT closable, "
          f"NOT proof of unlanded. Give the task a linked PR or an implementing "
          f"commit carrying `Task: <task-id>`.")
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
