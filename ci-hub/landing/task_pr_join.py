#!/usr/bin/env python3
"""MULTI-SIGNAL join: which TaskGraph task owns which open PR (and the gaps).

WHY THIS EXISTS (found by hermit-ghdag during the PR-health lever-cross; task
no-reliable-join-between-taskgraph-tasks-and-open-prs). NOTHING reliably answers
"which task's JOB is to land THIS PR." Two single-signal methods were tried and
BOTH FAILED:

  * empty-corpus (grep `tg list`, which is not a real subcommand) -> corpus was
    empty -> EVERYTHING read as untracked. TOO STRICT: manufactures #244 hazards.
  * bare-number match (`#1471` appears somewhere in task text) -> every bulk
    drain-rollup that enumerates PR numbers "tracks" them all. TOO LOOSE: a
    rollup mention is not ownership, and a wrongly-matched PR gets closed.

THE STAKES ARE ASYMMETRIC AND PRECEDENTED. PR #244 was opened, forgotten because
no landing task tracked it, and REIMPLEMENTED FROM SCRATCH the next day. An
UNMATCHED PR merely invites a look; a WRONGLY-matched PR gets closed. So the join
must prefer "unmatched" to a guess, and must never bind a task to a *neighbour*
of the PR it actually names.

THE FIX IS AGREEMENT ACROSS INDEPENDENT SIGNALS. A third single signal would
fail the same way, so we require >=2, at least one of them an identity signal:

  STRONG (identity/causal, repo-disambiguating):
    * pr_url    -- task text contains the PR url / `owner/repo#N` / `pull/N`
    * branch    -- PR headRefName appears verbatim in task text
    * head_sha  -- PR head 40-hex (or 12-hex prefix) appears in task text
  MEDIUM:
    * title_sim -- normalized title-token Jaccard >= threshold
  WEAK (corroborators only; never a match on their own):
    * pr_num    -- bare `#N` in task text (the too-loose signal, demoted)
    * file_ov   -- PR changed files overlap file paths named in the task

DECISION per (task, PR) pair:
    CONFIRMED (HIGH) -- >=1 STRONG and >=2 signals total (identity + agreement)
    PROBABLE  (MED)  -- exactly one lone STRONG signal, OR >=2 non-strong signals
    (otherwise)      -- not a match; the PR stays a candidate #244 hazard

Only CONFIRMED authorizes a bulk close/abandon. PROBABLE and unmatched both mean
"a human looks first" -- the safe side of the asymmetry.

THREE POPULATIONS, each WITH ITS DENOMINATOR:
  A. open PRs with a CONFIRMED owning task        (out of all open PRs)
  B. open PRs with NO owning task  == #244 hazard (out of all open PRs)
       split: MENTIONED-ONLY (weak mention, no confident owner) vs
              UNMENTIONED    (no task references it at all -- the sharpest hazard)
  C. tasks tagged `implemented` with NO PR match  (out of implemented-tagged
       tasks) -- a claim of landed work the join cannot see; research tasks whose
       text names a durable artifact (ai_docs/ experiments/ memory) are split out
       because they legitimately have no PR.

STANDING, not one-shot: rebases rewrite heads and SHAs, new tasks/PRs appear.
Re-run after each drain wave.

Usage:
  task_pr_join.py [--repo rrnewton/hermit ...] [--json]
                  [--tasks-jsonl <path>]  # inject tasks (one json_object/line); else `tg sql`
                  [--prs-json <path>]     # inject open PRs; else `gh pr list`
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CI_HUB_ROOT = os.path.dirname(HERE)
REPO_ROOT = os.path.dirname(CI_HUB_ROOT)
# One resolver for the TaskGraph, shared with every other ci-hub consumer.
sys.path.insert(0, os.path.join(CI_HUB_ROOT, "lib"))
import taskgraph_db  # noqa: E402

STRONG = {"pr_url", "branch", "head_sha"}
DEFAULT_REPOS = ["rrnewton/hermit", "rrnewton/reverie"]

# A task CONFIRMED-linked to more than this many distinct PRs is a batch/drain/
# triage sweep, NOT a dedicated owner. Identity signals (head_sha, pr_url) fire
# genuinely on such a task -- it really does enumerate those PRs -- but being
# swept into a triage rollup is exactly what did NOT prevent #244 from being
# reimplemented. A dedicated landing task owns 1 PR (or a coordinated
# hermit+reverie pair, or a tight series). Above the threshold, the task's
# CONFIRMED links are demoted to "swept" and the PR, if it has no dedicated
# owner, falls to the #244-hazard bucket instead of hiding in "matched".
BATCH_FANOUT_THRESHOLD = 3

# Branch names too generic to be an identity signal.
GENERIC_BRANCHES = {"main", "master", "trunk", "develop", "dev", "release", "hotfix"}
# Tokens that carry no discriminating power for title similarity.
STOPWORDS = {
    "the", "a", "an", "and", "or", "of", "to", "for", "in", "on", "with", "add",
    "fix", "use", "via", "not", "is", "are", "be", "into", "at", "by", "pr",
    "test", "tests", "ci", "hermit", "reverie", "detcore", "make", "support",
}
TITLE_SIM_THRESHOLD = 0.5
TITLE_SIM_MIN_TOKENS = 3


# ---- data sources (subprocess by default; injectable for tests/offline) ------

def load_tasks_via_tg() -> list[dict]:
    """Export every task with its title, description, tags, status, and the
    concatenation of all its notes. Uses `tg sql` (read-only)."""
    q_tasks = (
        "SELECT json_object('id',local_id,'num',task_number,'title',title,"
        "'desc',coalesce(description,''),'status',status,'tags',tags,"
        "'owner',coalesce(owner,'')) FROM tasks"
    )
    q_notes = "SELECT json_object('task_id',task_id,'content',content) FROM task_notes"
    tasks = _tg_sql_rows(q_tasks)
    notes = _tg_sql_rows(q_notes)
    by_id: dict[str, dict] = {}
    for t in tasks:
        t["notes"] = []
        by_id[t["id"]] = t
    for n in notes:
        row = by_id.get(n["task_id"])
        if row is not None:
            row["notes"].append(n["content"])
    return list(by_id.values())


def _tg_sql_rows(query: str) -> list[dict]:
    try:
        env = taskgraph_db.child_env(taskgraph_db.resolve())
    except taskgraph_db.TaskGraphUnavailable as error:
        raise RuntimeError(f"tg sql unavailable: {error}") from error
    cp = subprocess.run(
        ["tg", "sql", query], cwd=REPO_ROOT,
        capture_output=True, text=True, timeout=120,
        env=env,
    )
    if cp.returncode != 0:
        raise RuntimeError(f"tg sql failed: {cp.stderr.strip()}")
    try:
        return taskgraph_db.parse_json_rows(cp.stdout)
    except taskgraph_db.TaskGraphUnavailable as error:
        raise RuntimeError(f"tg sql returned incomplete rows: {error}") from error


def load_prs_via_gh(repos: list[str]) -> list[dict]:
    prs = []
    for repo in repos:
        cp = subprocess.run(
            ["with-proxy", "gh", "pr", "list", "--repo", repo, "--state", "open",
             "--json", "number,title,body,headRefName,headRefOid,isDraft,url,files",
             "--limit", "500"],
            capture_output=True, text=True, timeout=180,
        )
        if cp.returncode != 0:
            raise RuntimeError(f"gh pr list {repo} failed: {cp.stderr.strip()}")
        for p in json.loads(cp.stdout):
            p["repo"] = repo
            prs.append(p)
    return prs


def load_tasks_jsonl(path: str) -> list[dict]:
    tasks = {}
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line.startswith("{"):
                continue
            r = json.loads(line)
            if "task_id" in r and "content" in r and "id" not in r:
                continue  # a note row; tasks-jsonl must carry notes inline
            r.setdefault("notes", [])
            tasks[r["id"]] = r
    return list(tasks.values())


# ---- pure core ---------------------------------------------------------------

def _task_fulltext(task: dict) -> str:
    parts = [task.get("title", ""), task.get("desc", "")]
    parts.extend(task.get("notes", []) or [])
    return "\n".join(parts).lower()


def _tokens(text: str) -> set[str]:
    return {w for w in re.split(r"[^a-z0-9]+", text.lower())
            if len(w) >= 3 and w not in STOPWORDS and not w.isdigit()}


def _pr_repo_short(pr: dict) -> str:
    return pr["repo"].split("/")[-1]


def _pr_num_patterns(pr: dict) -> tuple[list[str], list[str]]:
    """Return (strong_url_patterns, weak_bare_patterns) as lowercase substrings."""
    n = pr["number"]
    repo = pr["repo"].lower()
    short = _pr_repo_short(pr).lower()
    strong = [
        f"{repo}/pull/{n}",          # full url path
        f"{repo}#{n}",               # owner/repo#N
        f"{short}#{n}",              # repo#N
        f"{short} pr #{n}",
        f"{short} pr {n}",
    ]
    url = (pr.get("url") or "").lower()
    if url:
        strong.append(url)
    weak = [f"#{n}", f"pr {n}", f"pr#{n}"]
    return strong, weak


def signals_for(task_ft: str, task_titletok: set[str], task: dict,
                pr: dict, pr_titletok: set[str], pr_files: set[str]) -> dict:
    """Which signals fire for this (task, PR) pair. Pure; substring/token only."""
    fired: dict[str, str] = {}
    strong_pats, weak_pats = _pr_num_patterns(pr)

    for pat in strong_pats:
        if pat and pat in task_ft:
            fired["pr_url"] = pat
            break

    branch = (pr.get("headRefName") or "")
    bl = branch.lower()
    if branch and len(branch) >= 8 and bl not in GENERIC_BRANCHES and bl in task_ft:
        fired["branch"] = branch

    sha = (pr.get("headRefOid") or "").lower()
    if sha and (sha in task_ft or sha[:12] in task_ft):
        fired["head_sha"] = sha[:12]

    if pr_titletok and task_titletok:
        inter = pr_titletok & task_titletok
        union = pr_titletok | task_titletok
        if len(pr_titletok) >= TITLE_SIM_MIN_TOKENS and union:
            jac = len(inter) / len(union)
            if jac >= TITLE_SIM_THRESHOLD:
                fired["title_sim"] = f"{jac:.2f}:{','.join(sorted(inter))[:60]}"

    for pat in weak_pats:
        if pat in task_ft:
            fired["pr_num"] = pat
            break

    if pr_files:
        named = pr_files & _paths_in(task_ft)
        if named:
            fired["file_ov"] = ",".join(sorted(named)[:3])

    return fired


_PATH_RE = re.compile(r"[a-z0-9_./-]+\.[a-z0-9]+")


def _paths_in(text: str) -> set[str]:
    return {m for m in _PATH_RE.findall(text) if "/" in m and len(m) > 6}


def classify_pair(fired: dict) -> tuple[str | None, str]:
    """(tier, confidence) for a fired-signal set. tier in {CONFIRMED,PROBABLE,None}."""
    names = set(fired)
    n_strong = len(names & STRONG)
    n_total = len(names)
    if n_strong >= 1 and n_total >= 2:
        return "CONFIRMED", "HIGH"
    if n_strong >= 1 and n_total == 1:
        return "PROBABLE", "MEDIUM"
    if n_strong == 0 and n_total >= 2:
        return "PROBABLE", "MEDIUM"
    return None, "NONE"


def _task_is_open(task: dict) -> bool:
    return (task.get("status") or "").upper() not in {"CLOSED", "RESOLVED", "DELETED"}


def _task_tags(task: dict) -> list[str]:
    raw = task.get("tags")
    if isinstance(raw, list):
        if all(isinstance(tag, str) for tag in raw):
            return raw
        raise RuntimeError(
            f"task {task.get('id', '<unknown>')} tags are not a list of strings"
        )
    try:
        tags = json.loads(raw) if raw else []
    except (json.JSONDecodeError, TypeError) as error:
        raise RuntimeError(
            f"task {task.get('id', '<unknown>')} has malformed tags"
        ) from error
    if isinstance(tags, list) and all(isinstance(tag, str) for tag in tags):
        return tags
    raise RuntimeError(
        f"task {task.get('id', '<unknown>')} tags are not a list of strings"
    )


def compute_join(tasks: list[dict], prs: list[dict]) -> dict:
    # Precompute per-task and per-PR views once.
    t_views = []
    for t in tasks:
        t_views.append((t, _task_fulltext(t), _tokens(t.get("title", ""))))
    pr_matches: dict[tuple[str, int], dict] = {}
    for pr in prs:
        key = (pr["repo"], pr["number"])
        pr_titletok = _tokens(pr.get("title", ""))
        pr_files = {f["path"] for f in (pr.get("files") or []) if isinstance(f, dict) and f.get("path")}
        confirmed, probable, weak = [], [], []
        for task, ft, titletok in t_views:
            # cheap prefilter: skip tasks that cannot possibly reference this PR
            if (f"#{pr['number']}" not in ft
                    and (pr.get("headRefName") or "_none_").lower() not in ft
                    and (pr.get("headRefOid") or "_none_").lower()[:12] not in ft
                    and not (pr_titletok & titletok)):
                continue
            fired = signals_for(ft, titletok, task, pr, pr_titletok, pr_files)
            if not fired:
                continue
            tier, conf = classify_pair(fired)
            entry = {"task": task["id"], "status": task.get("status"),
                     "tags": _task_tags(task), "signals": fired, "confidence": conf}
            if tier == "CONFIRMED":
                confirmed.append(entry)
            elif tier == "PROBABLE":
                probable.append(entry)
            else:
                weak.append(entry)
        pr_matches[key] = {
            "repo": pr["repo"], "number": pr["number"], "title": pr.get("title", ""),
            "isDraft": pr.get("isDraft", False), "branch": pr.get("headRefName"),
            "confirmed": confirmed, "probable": probable, "weak": weak,
        }

    # Identify batch/sweep tasks: a task CONFIRMED-linked to > threshold distinct
    # PRs is a drain/triage rollup, not a dedicated owner. Its identity matches
    # are real but do not constitute ownership (the #244 distinction).
    confirmed_pr_count: dict[str, int] = {}
    for m in pr_matches.values():
        for e in m["confirmed"]:
            confirmed_pr_count[e["task"]] = confirmed_pr_count.get(e["task"], 0) + 1
    batch_tasks = {tid for tid, c in confirmed_pr_count.items()
                   if c > BATCH_FANOUT_THRESHOLD}
    # Annotate every confirmed entry with dedicated-vs-swept and the fan-out.
    for m in pr_matches.values():
        for e in m["confirmed"]:
            e["owner_fanout"] = confirmed_pr_count.get(e["task"], 0)
            e["dedicated"] = e["task"] not in batch_tasks

    # Population A/B over PRs. A PR is truly OWNED only if a DEDICATED task
    # (not a batch sweep) confirms it. A PR confirmed solely by batch tasks is
    # "swept, not owned" and is a #244 hazard (mentioned-only -> LOOK).
    matched, mentioned_only, unmentioned = [], [], []
    for m in pr_matches.values():
        dedicated = [e for e in m["confirmed"] if e.get("dedicated")]
        m["dedicated_confirmed"] = dedicated
        m["swept_only"] = bool(m["confirmed"]) and not dedicated
        if dedicated:
            matched.append(m)
        elif m["confirmed"] or m["probable"] or m["weak"]:
            mentioned_only.append(m)
        else:
            unmentioned.append(m)

    # Population C over implemented-tagged tasks with no CONFIRMED/PROBABLE PR.
    owned_task_ids = set()
    for m in pr_matches.values():
        for e in m["confirmed"] + m["probable"]:
            owned_task_ids.add(e["task"])
    impl_no_pr, impl_research = [], []
    # Close-on-implemented lifecycle: an implemented task is CLOSED immediately
    # and its landing debt is enumerated from CLOSED+implemented records. Also
    # requiring _task_is_open() here excluded exactly the population this join
    # exists to find, so population C silently emptied as the lifecycle took
    # effect. The `implemented` tag is the population, at any status.
    impl_tasks = [t for t in tasks if "implemented" in _task_tags(t)]
    for t in impl_tasks:
        if t["id"] in owned_task_ids:
            continue
        ft = _task_fulltext(t)
        if re.search(r"\b(ai_docs/|experiments/|memory:|\.md\b)", ft):
            impl_research.append(t["id"])
        else:
            impl_no_pr.append(t["id"])

    return {
        "schema_version": 1,
        "denominators": {
            "open_prs": len(prs),
            "tasks_total": len(tasks),
            "implemented_open_tasks": len(impl_tasks),
        },
        "population_A_matched": matched,
        "population_B_mentioned_only": mentioned_only,
        "population_B_unmentioned": unmentioned,
        "population_C_impl_no_pr": impl_no_pr,
        "population_C_impl_research_no_pr_expected": impl_research,
    }


# ---- reporting ---------------------------------------------------------------

def _best(entries: list[dict]) -> dict:
    # Prefer non-closed, then most signals.
    def rank(e):
        open_bonus = 0 if (e.get("status") or "").upper() in {"CLOSED", "RESOLVED"} else 1
        return (open_bonus, len(e["signals"]))
    return max(entries, key=rank)


def render(report: dict, repos: list[str]) -> str:
    d = report["denominators"]
    A = report["population_A_matched"]
    Bm = report["population_B_mentioned_only"]
    Bu = report["population_B_unmentioned"]
    C = report["population_C_impl_no_pr"]
    Cr = report["population_C_impl_research_no_pr_expected"]
    N = d["open_prs"]
    lines = []
    lines.append(f"TASK<->PR MULTI-SIGNAL JOIN  repos={','.join(repos)}")
    lines.append(f"  denominators: open_prs={N}  tasks={d['tasks_total']}  "
                 f"implemented_open_tasks={d['implemented_open_tasks']}")
    lines.append("")
    lines.append(f"A. DEDICATED owning task: {len(A)}/{N} open PRs")
    for m in sorted(A, key=lambda x: (x["repo"], x["number"])):
        e = _best(m["dedicated_confirmed"])
        sig = "+".join(sorted(e["signals"]))
        extra = f" (+{len(m['dedicated_confirmed'])-1} more)" if len(m["dedicated_confirmed"]) > 1 else ""
        lines.append(f"   {m['repo'].split('/')[-1]}#{m['number']} <- {e['task']} "
                     f"[{e['confidence']} {sig}]{extra}")
    lines.append("")
    lines.append(f"B. #244 HAZARD -- no dedicated owner: {len(Bm)+len(Bu)}/{N}")
    lines.append(f"   B1 MENTIONED-ONLY (swept by a batch task, or weak/probable, no dedicated owner -> LOOK): {len(Bm)}/{N}")
    for m in sorted(Bm, key=lambda x: (x["repo"], x["number"])):
        tag = "draft" if m["isDraft"] else "ready"
        if m.get("swept_only"):
            sw = _best(m["confirmed"])
            hint = f" SWEPT-BY {sw['task']}(fanout={sw.get('owner_fanout')})[{'+'.join(sorted(sw['signals']))}]"
        else:
            near = _best(m["probable"] or m["weak"]) if (m["probable"] or m["weak"]) else None
            hint = f" ~{near['task']}[{'+'.join(sorted(near['signals']))}]" if near else ""
        lines.append(f"      {m['repo'].split('/')[-1]}#{m['number']} ({tag}){hint}")
    lines.append(f"   B2 UNMENTIONED (no task references it at all -> SHARPEST HAZARD): {len(Bu)}/{N}")
    for m in sorted(Bu, key=lambda x: (x["repo"], x["number"])):
        tag = "draft" if m["isDraft"] else "ready"
        lines.append(f"      {m['repo'].split('/')[-1]}#{m['number']} ({tag}) {m['title'][:60]}")
    lines.append("")
    lines.append(f"C. `implemented` tag, NO PR match: {len(C)}/{d['implemented_open_tasks']} "
                 f"implemented-open tasks  (+{len(Cr)} research tasks w/ durable-artifact text, PR not expected)")
    for tid in sorted(C):
        lines.append(f"   {tid}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", action="append", dest="repos")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--tasks-jsonl")
    ap.add_argument("--prs-json")
    args = ap.parse_args(argv)
    repos = args.repos or DEFAULT_REPOS

    try:
        tasks = load_tasks_jsonl(args.tasks_jsonl) if args.tasks_jsonl else load_tasks_via_tg()
        if args.prs_json:
            with open(args.prs_json) as handle:
                prs = json.load(handle)
            for p in prs:
                p.setdefault("repo", repos[0])
        else:
            prs = load_prs_via_gh(repos)
        report = compute_join(tasks, prs)
    except (OSError, RuntimeError, json.JSONDecodeError) as error:
        print("state=indeterminate")
        print(f"summary=task/PR join unavailable: {error}")
        return 2
    if args.json:
        print(json.dumps(report, indent=1))
    else:
        print(render(report, repos))
    return 0


if __name__ == "__main__":
    sys.exit(main())
