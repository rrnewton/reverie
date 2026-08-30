#!/usr/bin/env python3
"""DERIVE `landed` FOR tg TASKS — status asserted vs status computed.

THE GAP, in one line
--------------------
tg's status is ASSERTED BY AN AGENT. The directives ledger's status is DERIVED
by checking ancestry against a freshly-fetched target. That difference — not the
storage — is the entire reason two systems exist.

MEASURED COST: the `implemented` tag means "the code was written" and is
routinely read as "the work is done", so finished implementations sitting
unlanded read as complete. The taskgraph cannot tell the difference and never
could, because nothing in it dereferences a commit.

WHAT THIS DOES
--------------
Teaches the ancestry check to tg's own data, without forking tg: it reads the
task DB read-only (`tg sql`), extracts the IMPLEMENTATION REFERENCES an agent
named in canonical notes (40-hex commits and PR-number caches), and DERIVES a
landed state per task from membership in the freshly-scanned target ancestry.
Every reference is first bound to its repository; object presence alone is not
treated as ownership.

The derived state NEVER inherits the asserted one. A task tagged `implemented`
whose named commit is not an ancestor reports NOT-LANDED — that is the whole
point, and it is the direction that must be tested.

`PARTIAL` is the state tg cannot express at all: a task naming several commits
of which only some landed. It is reported here rather than rounded to either
end, because rounding it is how a half-landed obligation reads as done.

FRESHNESS IS PART OF THE ANSWER
-------------------------------
Ancestry against a stale remote answers about the past. When the target cannot
be freshly fetched (no egress), every derivation is reported as
`STALE-TARGET` — visibly, in the summary — rather than being quietly computed
against whatever was last pulled. A checker that hides its own staleness is the
defect it exists to catch.
"""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from pathlib import Path as _Path

# One resolver for the TaskGraph, shared with every other ci-hub consumer.
sys.path.insert(0, str(_Path(__file__).resolve().parents[1] / "lib"))
import taskgraph_db  # noqa: E402
from typing import Callable, Optional

LANDED = "landed"
NOT_LANDED = "not_landed"
PARTIAL = "partial"            # some named commits landed, others did not
UNVERIFIABLE = "unverifiable"  # no reference, or the commit is not present locally
NO_REFERENCE = "no_reference"  # tagged implemented but named nothing checkable
ABSENT = "absent"
AMBIGUOUS = "ambiguous_repository"

SHA_RE = re.compile(r"\b([0-9a-f]{40})\b")
PR_RE = re.compile(r"(?:pull/|PR\s*#|#)(\d{3,5})\b")
IMPLEMENTATION_NOTE_RE = re.compile(r"\b(?:IMPLEMENTED|CLOSURE-VERIFIED)\b", re.I)
EXPLICIT_SHA_RE = re.compile(
    r"\b(?:sha|commit|mergecommit(?:\.oid)?|main)\s*(?:[:=@]|is\b|at\b)?\s*"
    r"([0-9a-f]{40})\b",
    re.I,
)
TUPLE_SHA_RE = re.compile(r"@([0-9a-f]{40})\b", re.I)


@dataclass
class TaskRefs:
    task: str
    tags: list[str]
    status: str
    shas: list[str] = field(default_factory=list)
    prs: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class RepositoryTarget:
    """The repository identity is part of every ancestry claim.

    A SHA without this tuple is only a correlated proxy: the same 40 bytes can
    be absent from one object database, present in another, and compared to the
    wrong main.  `fetched_fresh` deliberately travels with the target rather
    than living as one process-global boolean.
    """

    repository: str
    checkout: str
    target: str = "origin/main"
    fetched_fresh: bool = False

    def as_dict(self) -> dict:
        return {
            "repository": self.repository,
            "checkout": self.checkout,
            "target": self.target,
            "target_freshly_fetched": self.fetched_fresh,
        }


@dataclass(frozen=True)
class ProbeResult:
    present: bool
    ancestor: Optional[bool]
    reachable: bool = False
    issue: Optional[str] = None


@dataclass
class ReferenceResolution:
    sha: str
    state: str
    repository: Optional[str] = None
    checkout: Optional[str] = None
    target: Optional[str] = None
    target_freshly_fetched: Optional[bool] = None
    repository_candidates: list[str] = field(default_factory=list)
    object_database_candidates: list[str] = field(default_factory=list)
    issue: Optional[str] = None

    def as_dict(self) -> dict:
        return {
            "sha": self.sha,
            "state": self.state,
            "repository": self.repository,
            "checkout": self.checkout,
            "target": self.target,
            "target_freshly_fetched": self.target_freshly_fetched,
            "repository_candidates": self.repository_candidates,
            "object_database_candidates": self.object_database_candidates,
            "issue": self.issue,
        }


@dataclass
class Derivation:
    task: str
    asserted_status: str
    asserted_implemented: bool
    derived: str
    reason: str
    landed_shas: list[str] = field(default_factory=list)
    unlanded_shas: list[str] = field(default_factory=list)
    absent_shas: list[str] = field(default_factory=list)
    ambiguous_shas: list[str] = field(default_factory=list)
    unverifiable_shas: list[str] = field(default_factory=list)
    stale_repositories: list[str] = field(default_factory=list)
    references: list[ReferenceResolution] = field(default_factory=list)

    def as_dict(self) -> dict:
        return {
            "task": self.task,
            "asserted_status": self.asserted_status,
            "asserted_implemented": self.asserted_implemented,
            "derived": self.derived,
            "reason": self.reason,
            "landed": self.landed_shas,
            "unlanded": self.unlanded_shas,
            "absent": self.absent_shas,
            "ambiguous": self.ambiguous_shas,
            "unverifiable": self.unverifiable_shas,
            "stale_repositories": self.stale_repositories,
            "references": [reference.as_dict() for reference in self.references],
        }


def extract_refs(text: str) -> tuple[list[str], list[str]]:
    """Implementation references an agent named. Order-preserving, deduped."""
    shas, prs = [], []
    for m in SHA_RE.findall(text or ""):
        if m not in shas:
            shas.append(m)
    for m in PR_RE.findall(text or ""):
        if m not in prs:
            prs.append(m)
    return shas, prs


def extract_implementation_refs(notes: list[str]) -> tuple[list[str], list[str]]:
    """Extract only identities asserted as implementation evidence.

    Arbitrary SHAs in progress notes are inputs, bases, stale heads, and
    validation context. Treating all of them as landing obligations made every
    long-lived task drift toward `partial`.  Until TaskGraph has typed fields,
    the canonical IMPLEMENTED/CLOSURE-VERIFIED note is the authority and an
    explicit SHA/commit/main/mergeCommit token (or typed `@sha` tuple) is the
    binding.  Ambiguous prose is refused as NO_REFERENCE.
    """
    shas: list[str] = []
    prs: list[str] = []
    for note in notes:
        if not IMPLEMENTATION_NOTE_RE.search(note or ""):
            continue
        for sha in EXPLICIT_SHA_RE.findall(note):
            sha = sha.lower()
            if sha not in shas:
                shas.append(sha)
        if "CLOSURE-VERIFIED" in note.upper():
            for sha in TUPLE_SHA_RE.findall(note):
                sha = sha.lower()
                if sha not in shas:
                    shas.append(sha)
        for pr in PR_RE.findall(note):
            if pr not in prs:
                prs.append(pr)
    return shas, prs


def derive_resolved(refs: TaskRefs,
                    resolutions: list[ReferenceResolution]) -> Derivation:
    """Compute task state from repository-bound reference resolutions.

    The asserted status is recorded for comparison and never participates in
    the verdict.  A proven non-ancestor is sufficient to refuse `landed`; an
    unresolved reference alone is unknown, never a manufactured negative.
    """
    implemented = "implemented" in refs.tags
    base = Derivation(task=refs.task, asserted_status=refs.status,
                      asserted_implemented=implemented, derived=UNVERIFIABLE,
                      reason="", references=resolutions)

    if not refs.shas:
        base.derived = NO_REFERENCE
        if refs.prs:
            base.reason = ("only PR reference(s) named; no locally resolved merge OID "
                           "was recorded, so there is nothing safe to dereference")
        else:
            base.reason = ("no 40-hex commit named in any note -- nothing to dereference, "
                           "so the asserted status is all there is (which is the gap)")
        return base

    if [resolution.sha for resolution in resolutions] != refs.shas:
        base.reason = "resolver output does not exactly cover the task's named SHAs"
        base.unverifiable_shas = list(refs.shas)
        return base

    for resolution in resolutions:
        if resolution.state == LANDED:
            base.landed_shas.append(resolution.sha)
        elif resolution.state == NOT_LANDED:
            base.unlanded_shas.append(resolution.sha)
        elif resolution.state == ABSENT:
            base.absent_shas.append(resolution.sha)
        elif resolution.state == AMBIGUOUS:
            base.ambiguous_shas.append(resolution.sha)
        else:
            base.unverifiable_shas.append(resolution.sha)
        if (resolution.repository
                and resolution.target_freshly_fetched is False
                and resolution.repository not in base.stale_repositories):
            base.stale_repositories.append(resolution.repository)

    unknown_count = (len(base.absent_shas) + len(base.ambiguous_shas)
                     + len(base.unverifiable_shas))

    if base.landed_shas and not base.unlanded_shas and not unknown_count:
        base.derived = LANDED
        base.reason = f"{len(base.landed_shas)} named commit(s), all ancestors of the target"
    elif base.landed_shas and (base.unlanded_shas or unknown_count):
        base.derived = PARTIAL
        base.reason = (f"{len(base.landed_shas)} landed, {len(base.unlanded_shas)} not, "
                       f"{unknown_count} unresolved -- the state tg cannot express")
    elif base.unlanded_shas and not base.landed_shas:
        base.derived = NOT_LANDED
        base.reason = ("named commit(s) are NOT ancestors of the target: never landed, "
                       "or rebased away")
    else:
        base.derived = UNVERIFIABLE
        base.reason = (f"{len(base.absent_shas)} absent, "
                       f"{len(base.ambiguous_shas)} repository-ambiguous, "
                       f"{len(base.unverifiable_shas)} comparison error; "
                       "ancestry cannot be computed")

    if base.stale_repositories and base.derived in (LANDED, PARTIAL, NOT_LANDED):
        names = ", ".join(base.stale_repositories)
        base.reason += (f" [STALE TARGET: {names} not freshly fetched -- "
                        "answers about the past]")
    return base


def derive(refs: TaskRefs, *, is_ancestor: Callable[[str], Optional[bool]],
           fetched_fresh: bool) -> Derivation:
    """Compatibility adapter for one-checkout callers.

    The CLI no longer uses this lossy API.  Keeping it makes the original
    mutation tests explicit and avoids silently changing downstream imports.
    """
    resolutions: list[ReferenceResolution] = []
    for sha in refs.shas:
        verdict = is_ancestor(sha)
        state = ABSENT if verdict is None else (LANDED if verdict else NOT_LANDED)
        resolutions.append(ReferenceResolution(
            sha=sha,
            state=state,
            repository="legacy-single-checkout",
            target_freshly_fetched=fetched_fresh,
            repository_candidates=["legacy-single-checkout"] if verdict is not None else [],
            object_database_candidates=(
                ["legacy-single-checkout"] if verdict is not None else []),
        ))
    return derive_resolved(refs, resolutions)


# ------------------------------------------------------------------ collectors


class TaskGraphQueryFailed(RuntimeError):
    """A `tg` query could not be run or bound. Never an empty result set."""


def _run(args: list[str], cwd: Optional[str] = None) -> tuple[int, str]:
    try:
        environment = dict(os.environ)
        environment["GIT_NO_LAZY_FETCH"] = "1"
        if args and args[0] == "tg":
            environment = taskgraph_db.child_env(
                taskgraph_db.resolve(), env=environment
            )
        p = subprocess.run(
            args, cwd=cwd, capture_output=True, text=True, timeout=180,
            env=environment)
        return p.returncode, (p.stdout or "") + (p.stderr or "")
    except taskgraph_db.TaskGraphUnavailable:
        # Must escape the blanket handler below: collapsing this into (127, "")
        # would discard the one diagnostic that says WHICH database was missing.
        raise
    except Exception:
        return 127, ""


def git_ancestor_factory(checkout: str, target: str) -> Callable[[str], Optional[bool]]:
    cache: dict[str, Optional[bool]] = {}

    def is_ancestor(sha: str) -> Optional[bool]:
        if sha in cache:
            return cache[sha]
        rc, _ = _run(["git", "cat-file", "-e", f"{sha}^{{commit}}"], cwd=checkout)
        if rc != 0:
            cache[sha] = None                     # absent != not-landed
            return None
        rc, _ = _run(["git", "merge-base", "--is-ancestor", sha, target], cwd=checkout)
        cache[sha] = rc == 0
        return cache[sha]
    return is_ancestor


RepositoryProbe = Callable[[RepositoryTarget, str], ProbeResult]


def _batch_present_commits(checkout: str, shas: set[str]) -> Optional[set[str]]:
    expressions = [f"{sha}^{{commit}}" for sha in sorted(shas)]
    try:
        environment = dict(os.environ)
        environment["GIT_NO_LAZY_FETCH"] = "1"
        process = subprocess.run(
            ["git", "cat-file", "--batch-check=%(objectname) %(objecttype)"],
            cwd=checkout,
            input="\n".join(expressions) + "\n",
            capture_output=True,
            text=True,
            timeout=180,
            env=environment,
        )
    except Exception:
        return None
    if process.returncode != 0:
        return None
    present: set[str] = set()
    for sha, line in zip(sorted(shas), process.stdout.splitlines()):
        if line.endswith(" commit"):
            present.add(sha)
    return present


def git_repository_probe(known_shas: Optional[set[str]] = None) -> RepositoryProbe:
    """Return a cached real-git repository/ancestry probe.

    The population path batches object and graph scans once per repository.
    Launching `cat-file`, `for-each-ref --contains`, and `merge-base` for every
    SHA made the first multi-repo implementation unsuitable for an hourly tick.
    """
    cache: dict[tuple[str, str, str], ProbeResult] = {}
    reachable_cache: dict[str, Optional[set[str]]] = {}
    target_cache: dict[tuple[str, str], Optional[set[str]]] = {}
    present_cache: dict[str, Optional[set[str]]] = {}

    def reachable_commits(repository: RepositoryTarget) -> Optional[set[str]]:
        if repository.checkout in reachable_cache:
            return reachable_cache[repository.checkout]
        rc, output = _run(["git", "rev-list", "--all"], cwd=repository.checkout)
        reachable_cache[repository.checkout] = (
            set(output.splitlines()) if rc == 0 else None)
        return reachable_cache[repository.checkout]

    def target_commits(repository: RepositoryTarget) -> Optional[set[str]]:
        key = (repository.checkout, repository.target)
        if key in target_cache:
            return target_cache[key]
        rc, output = _run(
            ["git", "rev-list", repository.target], cwd=repository.checkout)
        target_cache[key] = set(output.splitlines()) if rc == 0 else None
        return target_cache[key]

    def present_commits(repository: RepositoryTarget) -> Optional[set[str]]:
        if repository.checkout not in present_cache:
            present_cache[repository.checkout] = (
                _batch_present_commits(repository.checkout, known_shas)
                if known_shas is not None else None)
        return present_cache[repository.checkout]

    def probe(repository: RepositoryTarget, sha: str) -> ProbeResult:
        key = (repository.checkout, repository.target, sha)
        if key in cache:
            return cache[key]
        known_present = present_commits(repository)
        if known_present is None:
            rc, _ = _run(
                ["git", "cat-file", "-e", f"{sha}^{{commit}}"],
                cwd=repository.checkout,
            )
            present = rc == 0
        else:
            present = sha in known_present
        if not present:
            result = ProbeResult(present=False, ancestor=None, issue="object absent")
        else:
            reachable_set = reachable_commits(repository)
            reachable = reachable_set is not None and sha in reachable_set
            target_set = target_commits(repository)
            if target_set is not None and sha in target_set:
                result = ProbeResult(present=True, ancestor=True, reachable=reachable)
            elif target_set is not None:
                result = ProbeResult(present=True, ancestor=False, reachable=reachable)
            else:
                issue = "target ancestry scan failed"
                if reachable_set is None:
                    issue += "; repository reachability scan failed"
                result = ProbeResult(
                    present=True, ancestor=None, reachable=reachable, issue=issue)
        cache[key] = result
        return result

    return probe


def resolve_references(refs: TaskRefs, repositories: list[RepositoryTarget],
                       *, probe: RepositoryProbe) -> list[ReferenceResolution]:
    """Bind each SHA to exactly one repository, then compare its own target.

    Zero candidates is absence. Multiple candidates is ambiguity, not license
    to take the first convenient green.  This is the key correction to the
    original population reporter.
    """
    resolutions: list[ReferenceResolution] = []
    for sha in refs.shas:
        object_candidates: list[tuple[RepositoryTarget, ProbeResult]] = []
        for repository in repositories:
            result = probe(repository, sha)
            if result.present:
                object_candidates.append((repository, result))
        reachable_candidates = [item for item in object_candidates if item[1].reachable]
        candidates = reachable_candidates or object_candidates
        object_candidate_names = [repository.repository
                                  for repository, _ in object_candidates]
        candidate_names = [repository.repository for repository, _ in candidates]
        if not object_candidates:
            resolutions.append(ReferenceResolution(
                sha=sha,
                state=ABSENT,
                repository_candidates=[],
                object_database_candidates=[],
                issue="commit absent from every configured repository",
            ))
            continue
        if len(candidates) > 1:
            resolutions.append(ReferenceResolution(
                sha=sha,
                state=AMBIGUOUS,
                repository_candidates=candidate_names,
                object_database_candidates=object_candidate_names,
                issue=("commit has multiple repository provenance candidates; "
                       "explicit provenance required"),
            ))
            continue

        repository, result = candidates[0]
        if result.ancestor is True:
            state = LANDED
        elif result.ancestor is False:
            state = NOT_LANDED
        else:
            state = UNVERIFIABLE
        resolutions.append(ReferenceResolution(
            sha=sha,
            state=state,
            repository=repository.repository,
            checkout=repository.checkout,
            target=repository.target,
            target_freshly_fetched=repository.fetched_fresh,
            repository_candidates=candidate_names,
            object_database_candidates=object_candidate_names,
            issue=result.issue,
        ))
    return resolutions


def derive_across_repositories(refs: TaskRefs,
                               repositories: list[RepositoryTarget],
                               *, probe: RepositoryProbe) -> Derivation:
    return derive_resolved(refs, resolve_references(refs, repositories, probe=probe))


def _remote_identity(checkout: Path, fallback: str) -> str:
    rc, output = _run(["git", "remote", "get-url", "origin"], cwd=str(checkout))
    if rc != 0:
        return fallback
    remote = output.strip().splitlines()[0] if output.strip() else ""
    match = re.search(r"(?:github\.com[:/])([^/]+/[^/]+?)(?:\.git)?$", remote)
    return match.group(1) if match else fallback


def workspace_repositories(root: str, target: str = "origin/main") -> list[RepositoryTarget]:
    """Discover the canonical local repositories without touching the network."""
    workspace = Path(root).resolve()
    candidates = [
        ("rrnewton/dev-hermit", workspace),
        ("rrnewton/hermit", workspace / "hermit"),
        ("rrnewton/reverie", workspace / "reverie"),
        ("liteinst2", workspace / "liteinst2"),
        ("rrnewton/agent-utils", workspace / "agent-utils"),
    ]
    repositories: list[RepositoryTarget] = []
    for fallback, checkout in candidates:
        rc, _ = _run(["git", "rev-parse", "--git-dir"], cwd=str(checkout))
        if rc == 0:
            repositories.append(RepositoryTarget(
                repository=_remote_identity(checkout, fallback),
                checkout=str(checkout),
                target=target,
                fetched_fresh=False,
            ))
    return repositories


def load_tasks(tag: str = "implemented", limit: Optional[int] = None) -> list[TaskRefs]:
    """Read the tg DB read-only and gather each task's notes.

    Tag matching is on the JSON array element (`"implemented"`) rather than a
    bare substring. MEASURED 2026-08-06: both forms return the SAME count (812),
    so the element form is not currently buying anything -- it is kept because a
    future tag containing the word as a substring would silently inflate the
    population, and a denominator that can drift without notice is the defect
    this tool exists to surface. (An earlier draft of this docstring claimed the
    substring form over-matched 808 vs ~106; that was WRONG. 106 was the owner's
    2026-08-04 figure and the population has since grown -- it was never a
    substring artefact. The count moved 808 -> 810 -> 812 during this session as
    other agents tagged work, so the denominator is live, not fixed.)
    """
    # JSON aggregation preserves note boundaries and escapes embedded newlines.
    # A raw GROUP_CONCAT both merged authorities with incidental progress notes
    # and let one multi-line note render as several output rows.
    q = ("SELECT t.local_id, t.tags, t.status, "
         "COALESCE(json_group_array(n.content), '[]') "
         "FROM tasks t LEFT JOIN task_notes n ON n.task_id = t.local_id "
         f"WHERE t.tags LIKE '%\"{tag}\"%' GROUP BY t.local_id")
    if limit:
        q += f" LIMIT {int(limit)}"
    try:
        rc, out = _run(["tg", "sql", q])
    except taskgraph_db.TaskGraphUnavailable as error:
        raise TaskGraphQueryFailed(str(error)) from error
    if rc != 0:
        # Previously `return []`. An empty list is indistinguishable from "no
        # tagged tasks exist", so a failed or unbound query read as a clean,
        # empty population -- the fail-open this sweep exists to remove.
        raise TaskGraphQueryFailed(
            f"tg sql exited {rc} while listing tasks tagged for landing: {out.strip()[:200]}"
        )
    tasks: list[TaskRefs] = []
    reported_rows: int | None = None
    for line in out.splitlines():
        count_match = re.fullmatch(r"\((\d+) rows?\)", line.strip())
        if count_match:
            reported_rows = int(count_match.group(1))
            continue
        parts = line.split("|")
        if len(parts) < 4 or parts[0].strip() in ("", "local_id") or set(parts[0].strip()) == {"-"}:
            continue
        tid, tags_raw, status = parts[0].strip(), parts[1].strip(), parts[2].strip()
        notes_raw = "|".join(parts[3:])
        try:
            tags = json.loads(tags_raw)
        except json.JSONDecodeError as error:
            raise TaskGraphQueryFailed(f"task {tid} has malformed tags") from error
        if not isinstance(tags, list) or not all(isinstance(tag, str) for tag in tags):
            raise TaskGraphQueryFailed(f"task {tid} tags are not a list of strings")
        try:
            decoded_notes = json.loads(notes_raw)
        except (json.JSONDecodeError, TypeError) as error:
            raise TaskGraphQueryFailed(f"task {tid} has malformed notes") from error
        if not isinstance(decoded_notes, list) or not all(
            isinstance(note, str) for note in decoded_notes
        ):
            raise TaskGraphQueryFailed(f"task {tid} notes are not a list of strings")
        notes = decoded_notes
        shas, prs = extract_implementation_refs(notes)
        tasks.append(TaskRefs(task=tid, tags=tags, status=status, shas=shas, prs=prs))
    if reported_rows is None:
        raise TaskGraphQueryFailed("tg sql output omitted its row-count footer")
    if reported_rows != len(tasks):
        raise TaskGraphQueryFailed(
            f"tg sql reported {reported_rows} row(s) but {len(tasks)} task row(s) were readable"
        )
    return tasks


def report(derivations: list[Derivation], *, fetched_fresh: bool, target: str,
           repositories: Optional[list[RepositoryTarget]] = None) -> dict:
    counts = Counter(d.derived for d in derivations)
    asserted = sum(1 for d in derivations if d.asserted_implemented)
    repository_payload = ([repository.as_dict() for repository in repositories]
                          if repositories is not None else [])
    if repositories is not None:
        fetched_fresh = bool(repositories) and all(
            repository.fetched_fresh for repository in repositories
        )
    return {
        "schema_version": 2,
        "target": target,
        "target_freshly_fetched": fetched_fresh,
        "repositories": repository_payload,
        "tasks_examined": len(derivations),
        "asserted_implemented": asserted,
        "derived": dict(counts),
        "derived_landed": counts[LANDED],
        "assertion_gap": asserted - counts[LANDED],
        "tasks": [d.as_dict() for d in derivations],
    }


def render(rep: dict) -> str:
    out = ["tg DERIVED LANDING STATE", ""]
    if not rep["target_freshly_fetched"]:
        stale = [item["repository"] for item in rep.get("repositories", [])
                 if not item["target_freshly_fetched"]]
        suffix = ", ".join(stale) if stale else rep["target"]
        out.append(f"  !! TARGET(S) NOT FRESHLY FETCHED: {suffix}")
        out.append("     affected derivations answer about the last-pulled state, not now.")
        out.append("")
    for k in (LANDED, PARTIAL, NOT_LANDED, UNVERIFIABLE, NO_REFERENCE):
        if rep["derived"].get(k):
            out.append(f"  {k:<14} {rep['derived'][k]}")
    out += ["",
            f"  asserted `implemented`: {rep['asserted_implemented']}",
            f"  DERIVED landed:         {rep['derived_landed']}",
            f"  ASSERTION GAP:          {rep['assertion_gap']}  "
            "(tagged implemented but not derivably landed)"]
    return "\n".join(out)


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--checkout", help="legacy single-repository checkout")
    ap.add_argument("--workspace-root", default=".",
                    help="canonical workspace containing the repository set")
    ap.add_argument("--repository", action="append", default=[], metavar="IDENTITY=PATH",
                    help="explicit repository mapping (repeatable; replaces discovery)")
    ap.add_argument("--target", default="origin/main")
    ap.add_argument("--tag", default="implemented")
    ap.add_argument("--limit", type=int)
    ap.add_argument("--fetch", action="store_true", help="fetch the target first (needs egress)")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    if args.checkout:
        repositories = [RepositoryTarget(
            repository=_remote_identity(Path(args.checkout), "legacy-single-checkout"),
            checkout=str(Path(args.checkout).resolve()),
            target=args.target,
        )]
    elif args.repository:
        repositories = []
        for raw in args.repository:
            if "=" not in raw:
                ap.error("--repository must be IDENTITY=PATH")
            identity, checkout = raw.split("=", 1)
            repositories.append(RepositoryTarget(
                repository=identity,
                checkout=str(Path(checkout).resolve()),
                target=args.target,
            ))
    else:
        repositories = workspace_repositories(args.workspace_root, args.target)
    if not repositories:
        ap.error("no Git repositories discovered")

    if args.fetch:
        fetched: list[RepositoryTarget] = []
        for repository in repositories:
            branch = repository.target.split("/", 1)[-1]
            refspec = f"refs/heads/{branch}:refs/remotes/origin/{branch}"
            rc, _ = _run(["with-proxy", "git", "fetch", "--no-tags", "origin",
                          refspec],
                         cwd=repository.checkout)
            fetched.append(RepositoryTarget(
                repository=repository.repository,
                checkout=repository.checkout,
                target=repository.target,
                fetched_fresh=rc == 0,
            ))
        repositories = fetched

    try:
        tasks = load_tasks(args.tag, args.limit)
    except TaskGraphQueryFailed as error:
        print("state=unverifiable")
        print(f"summary=cannot read tasks tagged for landing: {error}")
        return 2
    probe = git_repository_probe({sha for task in tasks for sha in task.shas})
    derivations = [derive_across_repositories(t, repositories, probe=probe)
                   for t in tasks]
    fresh = all(repository.fetched_fresh for repository in repositories)
    rep = report(derivations, fetched_fresh=fresh, target=args.target,
                 repositories=repositories)
    print(json.dumps(rep, indent=2) if args.json else render(rep))
    # A stale target is not a pass: the answer is about the past.
    return 0 if fresh else 1


if __name__ == "__main__":
    raise SystemExit(main())
