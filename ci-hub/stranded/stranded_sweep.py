#!/usr/bin/env python3
"""STRANDED-WORK SWEEP: find work that exists only on this box, and rescue it
WITHOUT destroying anything.

Three confirmed instances of work stranded off main were each found BY ACCIDENT.
A closed task with a completion note reads as done: nothing fails, no check
fires, the artifact simply is not on main, and the agent that wrote it has since
recycled.  This tool makes that condition queryable instead of anecdotal.

STRANDEDNESS IS THREE TIERS, NOT ONE.  They differ in what destroys them, so
they need different handling and must never be reported as one number:

  T1 UNCOMMITTED  untracked / modified in a working tree.  Destroyed by
                  `git clean`, by a worktree reclaim, or by ANOTHER agent's
                  `git add -A` in a shared tree.  Most fragile.
  T2 UNPUSHED     committed locally, on no remote.  Survives `git clean`;
                  dies with the checkout.  This is the dominant tier whenever
                  egress is down.
  T3 UNLANDED     pushed somewhere, but the artifact a CLOSED task points at is
                  not on origin/main.  Survives everything except being
                  forgotten -- which is exactly what happened three times.

TWO MEASUREMENT TRAPS THIS TOOL EXISTS TO AVOID
-----------------------------------------------
1. THE SHARED OBJECT STORE.  Linked worktrees share one `.git`, so
   `git log --branches --not --remotes` enumerates EVERY stale local branch in
   the whole repo and returns the same number for every worktree.  Measured
   here: that phrasing reported 1050 per checkout, 54,706 "unpushed commits"
   in total.  The honest measure -- commits reachable from THIS checkout's HEAD
   and from no remote -- gives 63 across 14 checkouts.  A ~870x overcount.
   Always anchor to HEAD, never to --branches.

2. A STALE origin/main IS NOT EVIDENCE OF ABSENCE.  When egress is down,
   `origin/main` is frozen at the last fetch, so "path not on origin/main" may
   simply mean "landed after our last fetch".  Reporting that as STRANDED
   produces false positives, and a checker that flags everything gets disabled.
   Absence is therefore reported as UNVERIFIABLE unless the ref is fresh; only
   presence is ever treated as definitive.

SAFETY CONTRACT (hard):
  * READ-ONLY on every path that another agent might own.  Every git read uses
    `--no-optional-locks` so a sweep across ~90 checkouts cannot contend on an
    index.lock with a live agent mid-commit.
  * NEVER runs `git clean`, `git reset`, `git checkout --`, or `git stash`.
  * `rescue` COPIES into a quarantine directory and leaves the original exactly
    where it was.  Rescue is additive; the owning agent still owns its tree.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

SCHEMA_VERSION = 1

# Tiers, most fragile first.
T1_UNCOMMITTED = "T1_UNCOMMITTED"
T2_UNPUSHED = "T2_UNPUSHED"
T3_UNLANDED = "T3_UNLANDED"

# Artifact dispositions.
LANDED = "LANDED"                # on origin/main -- definitive, safe
PENDING_PUSH = "PENDING_PUSH"    # in local history, not on fetched origin/main
UNCOMMITTED = "UNCOMMITTED"      # exists only as a working-tree file
MISSING = "MISSING"              # not anywhere -- never created, or deleted
UNVERIFIABLE = "UNVERIFIABLE"    # origin/main too stale to call an absence
IGNORED_LOCAL = "IGNORED_LOCAL"  # git-ignored on purpose -- local by design, not stranded

# How old a fetched origin/main may be before an ABSENCE stops being evidence.
DEFAULT_FRESH_HOURS = 6.0


def _git(repo: Path, *args: str, timeout: int = 30, raw: bool = False) -> tuple[int, str]:
    """Read-only git. --no-optional-locks so we never take another agent's lock.

    `raw=True` preserves LEADING whitespace, which is MANDATORY for
    `status --porcelain`: its first column is the staged status and is a SPACE
    for an unstaged modification, so a plain `.strip()` silently eats it and
    every parsed path loses its first character (` M AGENTS.md` -> `GENTS.md`).
    That corruption is invisible in a summary count and only surfaces when a
    rescue cannot find the file it was asked to copy.
    """
    try:
        p = subprocess.run(
            ["git", "--no-optional-locks", "-C", str(repo), *args],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=timeout,
        )
        out = p.stdout.decode("utf-8", "replace")
        return p.returncode, out.rstrip("\n") if raw else out.strip()
    except (subprocess.TimeoutExpired, OSError):
        return 1, ""


# --------------------------------------------------------------------------- discovery


def find_checkouts(root: Path) -> list[Path]:
    """The parent plus canonical slots and any remaining legacy checkouts.

    Finds `.git` entries (file for a linked worktree, dir for a clone) rather
    than trusting any registry -- ACTIVE.md, `git worktree list`, and the
    filesystem are known to disagree, and stranded work hides in the gap.
    """
    out = [root]
    worktrees = root / "worktrees"
    if worktrees.is_dir():
        dots = set((worktrees / "slots").glob("*/*/.git"))
        dots.update(
            dot
            for dot in worktrees.glob("*/.git")
            if dot.relative_to(worktrees).parts[0] not in {"slots", "validate"}
        )
        dots.update(
            dot
            for dot in worktrees.glob("*/*/.git")
            if dot.relative_to(worktrees).parts[0] not in {"slots", "validate"}
        )
        out.extend(sorted(dot.parent for dot in dots))
    return out


# --------------------------------------------------------------------------- attribution


@dataclass
class Owner:
    """WHO to ask before touching this. Never guessed silently: every field
    records where it came from, and `unknown` stays unknown."""

    slot: str = ""
    registry_row: str = ""       # from worktrees/ACTIVE.md, if present
    branch: str = ""
    last_commit_author: str = ""
    last_commit_when: str = ""
    newest_mtime: str = ""

    def as_dict(self) -> dict[str, Any]:
        return {k: v for k, v in self.__dict__.items() if v}


def _active_md_rows(root: Path) -> dict[str, str]:
    """slot -> its ACTIVE.md row. Machine-local, ignored, often stale -- used as
    a HINT for attribution only, never as authority."""
    rows: dict[str, str] = {}
    path = root / "worktrees" / "ACTIVE.md"
    if not path.is_file():
        return rows
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip().startswith("|"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if cells and cells[0] and cells[0].lower() not in ("slot", "---", ":---"):
            rows.setdefault(cells[0], line.strip())
    return rows


def attribute(root: Path, checkout: Path, paths: list[str]) -> Owner:
    rel = checkout.relative_to(root) if checkout != root else Path(".")
    slot = ""
    if len(rel.parts) > 2 and rel.parts[:2] == ("worktrees", "slots"):
        slot = rel.parts[2]
    elif (
        len(rel.parts) > 1
        and rel.parts[0] == "worktrees"
        and rel.parts[1] != "validate"
    ):
        slot = rel.parts[1]
    own = Owner(slot=slot, registry_row=_active_md_rows(root).get(slot, ""))
    _, own.branch = _git(checkout, "branch", "--show-current")
    rc, val = _git(checkout, "log", "-1", "--format=%an <%ae>")
    if rc == 0:
        own.last_commit_author = val
    rc, val = _git(checkout, "log", "-1", "--format=%ci")
    if rc == 0:
        own.last_commit_when = val
    newest = 0.0
    for rel_p in paths[:200]:
        f = checkout / rel_p
        try:
            newest = max(newest, f.stat().st_mtime)
        except OSError:
            continue
    if newest:
        own.newest_mtime = dt.datetime.fromtimestamp(
            newest, dt.timezone.utc
        ).isoformat(timespec="seconds")
    return own


# --------------------------------------------------------------------------- T1 / T2 scan


@dataclass
class CheckoutReport:
    path: str
    branch: str = ""
    untracked: list[str] = field(default_factory=list)
    modified: list[str] = field(default_factory=list)
    unpushed_commits: list[str] = field(default_factory=list)
    no_remote_refs: bool = False
    unavailable: list[str] = field(default_factory=list)
    owner: dict[str, Any] = field(default_factory=dict)

    @property
    def tiers(self) -> list[str]:
        t = []
        if self.untracked or self.modified:
            t.append(T1_UNCOMMITTED)
        if self.unpushed_commits:
            t.append(T2_UNPUSHED)
        return t

    def as_dict(self) -> dict[str, Any]:
        d = dict(self.__dict__)
        d["tiers"] = self.tiers
        return d


def scan_checkout(root: Path, checkout: Path) -> CheckoutReport:
    rep = CheckoutReport(path=str(checkout.relative_to(root)) if checkout != root else ".")
    branch_rc, rep.branch = _git(checkout, "branch", "--show-current")
    if branch_rc != 0:
        rep.unavailable.append("cannot read current branch")
    rc, status = _git(checkout, "status", "--porcelain", raw=True)
    if rc != 0:
        rep.unavailable.append("cannot read working-tree status")
    elif status:
        for line in status.splitlines():
            if len(line) < 4:
                continue
            code, name = line[:2], line[3:]
            # A rename reads "R  old -> new"; the surviving path is the new one.
            if " -> " in name:
                name = name.split(" -> ", 1)[1]
            if name.startswith('"') and name.endswith('"'):
                name = name[1:-1]          # git quotes paths with odd characters
            if code == "??":
                rep.untracked.append(name)
            else:
                rep.modified.append(name)
    # T2: anchored to HEAD, NOT --branches (see the shared-object-store trap).
    #
    # GUARD: with NO remote-tracking refs at all, `--not --remotes` excludes
    # nothing and the ENTIRE history reads as unpushed.  That is not a finding,
    # it is a missing denominator -- and reporting thousands of "stranded"
    # commits for a remoteless checkout is precisely the over-flagging that gets
    # a checker switched off.  Report the condition once instead.
    rc, remotes = _git(checkout, "for-each-ref", "--count=1", "refs/remotes/")
    if rc != 0:
        rep.unavailable.append("cannot enumerate remote-tracking refs")
    rep.no_remote_refs = (rc != 0) or not remotes.strip()
    if rc == 0 and not remotes.strip():
        rep.unavailable.append("no remote-tracking refs; publication cannot be determined")
    if not rep.no_remote_refs:
        rc, log = _git(checkout, "log", "HEAD", "--not", "--remotes", "--oneline")
        if rc != 0:
            rep.unavailable.append("cannot enumerate commits absent from remote refs")
        elif log:
            rep.unpushed_commits = log.splitlines()
    if rep.tiers:
        rep.owner = attribute(root, checkout, rep.untracked + rep.modified).as_dict()
    return rep


def sweep_worktrees(root: Path) -> dict[str, Any]:
    reports = [scan_checkout(root, c) for c in find_checkouts(root)]
    flagged = [r for r in reports if r.tiers or r.unavailable]
    return {
        "schema_version": SCHEMA_VERSION,
        "scanned": len(reports),
        "flagged": len(flagged),
        "t1_checkouts": sum(1 for r in flagged if T1_UNCOMMITTED in r.tiers),
        "t2_checkouts": sum(1 for r in flagged if T2_UNPUSHED in r.tiers),
        "untracked_paths": sum(len(r.untracked) for r in flagged),
        "unpushed_commits": sum(len(r.unpushed_commits) for r in flagged),
        "unverifiable_checkouts": sum(1 for r in reports if r.unavailable),
        "reports": [r.as_dict() for r in flagged],
    }


# --------------------------------------------------------------------------- T3 artifacts


def origin_age_hours(root: Path, ref: str = "origin/main") -> Optional[float]:
    rc, when = _git(root, "log", "-1", "--format=%ct", ref)
    if rc != 0 or not when.isdigit():
        return None
    import time
    return (time.time() - int(when)) / 3600.0


def classify_artifact(
    root: Path, path: str, *, ref: str = "origin/main", fresh: bool = True
) -> str:
    """Where does this artifact actually live?

    Only PRESENCE on origin/main is definitive.  An absence is downgraded to
    UNVERIFIABLE when the ref is stale, because 'not on our last-fetched
    origin/main' does not mean 'not on main'.
    """
    if _git(root, "cat-file", "-e", f"{ref}:{path}")[0] == 0:
        return LANDED
    # ABSENT FROM THE TIP IS NOT "NEVER LANDED".  A doc written in July, landed,
    # then renamed or superseded is absent from today's tree but was never
    # stranded -- it reached the repo, which is the whole question.  Strandedness
    # means the work never got in, so consult HISTORY before calling it missing.
    # Without this the checker flags two thirds of a healthy corpus and gets
    # switched off, which is the documented failure mode.
    # Scoped to --remotes, NOT --all: the question is "did this ever reach the
    # remote", so local-only branches must not count as evidence of landing --
    # that would launder a PENDING_PUSH into a LANDED and hide the exact
    # condition this tool hunts.
    rc, seen = _git(root, "rev-list", "--remotes", "--max-count=1", "--", path)
    if rc == 0 and seen.strip():
        return LANDED
    if _git(root, "cat-file", "-e", f"HEAD:{path}")[0] == 0:
        return PENDING_PUSH if fresh else UNVERIFIABLE
    if (root / path).exists():
        # A git-IGNORED path is local BY DESIGN -- the repo's experiment hygiene
        # says bulky evidence is written under an ignored/ dir on purpose. That
        # is not stranded work, and flagging it trains readers to ignore the
        # tool. Both live hits here were ignored/*.log evidence files.
        if _git(root, "check-ignore", "-q", path)[0] == 0:
            return IGNORED_LOCAL
        return UNCOMMITTED          # worst case, and independent of ref freshness
    return MISSING if fresh else UNVERIFIABLE


_ARTIFACT_PREFIXES = ("ai_docs/", "experiments/", "ci-hub/", "scripts/", "compat-envelope/")


def extract_artifact_paths(text: str) -> list[str]:
    """Repo-relative artifact paths mentioned in a task note.

    Deliberately conservative: a prefix allowlist and a real file extension, so
    prose like 'see ci-hub/' or a bare directory does not become a phantom
    finding.  Over-flagging is the failure mode that gets a checker disabled.
    """
    out: list[str] = []
    for raw in text.replace("`", " ").replace("(", " ").replace(")", " ").split():
        tok = raw.strip().strip(",;:'\"<>[]").lstrip("./")
        # Sentence punctuation clings to a path in prose: "see ai_docs/x.md."
        # A real filename does not end in '.' or ',', and leaving it attached
        # guarantees a phantom MISSING.
        tok = tok.rstrip(".,;:")
        if not tok.startswith(_ARTIFACT_PREFIXES):
            continue
        if "://" in tok or tok.endswith("/"):
            continue
        # Globs and brace lists are prose shorthand, not resolvable paths:
        # "ai_docs/*.md", "experiments/x/{README.md,metadata.json}".
        if any(ch in tok for ch in "*?{}"):
            continue
        if "." not in Path(tok).name:
            continue
        if tok not in out:
            out.append(tok)
    return out


# --------------------------------------------------------------------------- rescue


def rescue(root: Path, checkout_rel: str, quarantine: Path) -> dict[str, Any]:
    """NON-DESTRUCTIVE rescue: COPY out, never move, never clean.

    T1 untracked/modified files are copied into the quarantine tree, preserving
    their relative layout.  T2 unpushed commits are captured as a `git bundle`,
    which is a complete, re-fetchable object pack -- so the commits survive even
    if the checkout is later removed.  The source tree is left BYTE-IDENTICAL:
    the owning agent still owns it, and this tool never decides its work is
    finished.
    """
    checkout = root / checkout_rel if checkout_rel != "." else root
    rep = scan_checkout(root, checkout)
    if rep.unavailable:
        raise RuntimeError("; ".join(rep.unavailable))
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    dest = quarantine / f"{checkout_rel.replace('/', '_')}-{stamp}"
    dest.mkdir(parents=True, exist_ok=True)

    # `git status --porcelain` collapses a wholly-untracked DIRECTORY into ONE
    # entry with a trailing slash (`sub/`), not its files.  Copying only regular
    # files would therefore silently drop an entire directory of stranded work
    # -- data loss in the one tool whose job is to prevent it.  Directories are
    # copied whole.
    copied: list[str] = []
    skipped: list[str] = []
    for rel_p in rep.untracked + rep.modified:
        src = checkout / rel_p
        target = dest / "files" / rel_p.rstrip("/")
        try:
            if src.is_symlink() or (not src.is_file() and not src.is_dir()):
                skipped.append(rel_p)      # recorded, never silently dropped
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            if src.is_dir():
                shutil.copytree(src, target, symlinks=True, dirs_exist_ok=True)
                copied.extend(
                    str(Path(rel_p.rstrip("/")) / p.relative_to(src))
                    for p in sorted(src.rglob("*")) if p.is_file()
                )
            else:
                shutil.copy2(src, target)  # copy2, NOT move -- source untouched
                copied.append(rel_p)
        except OSError as err:
            skipped.append(f"{rel_p} ({err})")

    bundle_ok = False
    if rep.unpushed_commits:
        b = dest / "unpushed.bundle"
        rc, _ = _git(checkout, "bundle", "create", str(b), "HEAD", "--not", "--remotes")
        bundle_ok = rc == 0 and b.is_file()

    manifest = {
        "schema_version": SCHEMA_VERSION,
        "rescued_at": stamp,
        "source_checkout": checkout_rel,
        "source_left_intact": True,
        "branch": rep.branch,
        "owner": rep.owner,
        "tiers": rep.tiers,
        "files_copied": copied,
        "skipped_not_regular": skipped,
        "no_remote_refs": rep.no_remote_refs,
        "unpushed_commits": rep.unpushed_commits,
        "bundle": "unpushed.bundle" if bundle_ok else None,
        "restore": (
            f"Files: copy back from {dest}/files/ into {checkout_rel}/. "
            f"Commits: git -C {checkout_rel} fetch {dest}/unpushed.bundle "
            "'refs/*:refs/rescued/*'"
        ),
    }
    (dest / "MANIFEST.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True), encoding="utf-8"
    )
    return manifest


# --------------------------------------------------------------------------- CLI


def _root() -> Path:
    return Path(__file__).resolve().parent.parent.parent


def _cmd_worktrees(args: argparse.Namespace) -> int:
    res = sweep_worktrees(_root())
    if args.json:
        print(json.dumps(res, indent=2, sort_keys=True))
        return 2 if res["unverifiable_checkouts"] else (1 if res["flagged"] else 0)
    print(f"stranded sweep: {res['scanned']} checkouts scanned, "
          f"{res['flagged']} flagged "
          f"(T1 uncommitted: {res['t1_checkouts']}, T2 unpushed: {res['t2_checkouts']})")
    print(f"  {res['untracked_paths']} untracked paths, "
          f"{res['unpushed_commits']} unpushed commits "
          "(HEAD-anchored, not --branches)")
    for r in res["reports"]:
        print(f"\n  {r['path']}  [{', '.join(r['tiers'])}]  branch={r['branch'] or 'DETACHED'}")
        for u in r["untracked"][:10]:
            print(f"      ?? {u}")
        for m in r["modified"][:5]:
            print(f"       M {m}")
        if r["unpushed_commits"]:
            print(f"      {len(r['unpushed_commits'])} unpushed commit(s), "
                  f"newest: {r['unpushed_commits'][0]}")
        for reason in r.get("unavailable", []):
            print(f"      INDETERMINATE {reason}")
        own = r.get("owner") or {}
        if own.get("last_commit_author"):
            print(f"      owner-hint: {own['last_commit_author']} "
                  f"({own.get('last_commit_when', '?')})")
    return 2 if res["unverifiable_checkouts"] else (1 if res["flagged"] else 0)


def _cmd_artifacts(args: argparse.Namespace) -> int:
    root = _root()
    age = origin_age_hours(root)
    fresh = age is not None and age <= args.fresh_hours
    rows = json.loads(Path(args.input).read_text(encoding="utf-8"))
    counts: dict[str, int] = {}
    flagged: list[dict[str, Any]] = []
    for row in rows:
        for p in row.get("paths", []):
            d = classify_artifact(root, p, fresh=fresh)
            counts[d] = counts.get(d, 0) + 1
            if d in (UNCOMMITTED, MISSING, PENDING_PUSH):
                flagged.append({"task": row.get("task"), "path": p, "disposition": d})
    out = {
        "schema_version": SCHEMA_VERSION,
        "origin_ref_age_hours": None if age is None else round(age, 2),
        "origin_ref_fresh": fresh,
        "absence_is_evidence": fresh,
        "counts": counts,
        "flagged": flagged,
    }
    if args.json:
        print(json.dumps(out, indent=2, sort_keys=True))
    else:
        freshness = (f"fresh ({age:.1f}h)" if fresh else
                     f"STALE ({'unknown' if age is None else f'{age:.1f}h'}) "
                     "-- absences reported as UNVERIFIABLE, not STRANDED")
        print(f"artifact check: origin/main {freshness}")
        for k in (LANDED, IGNORED_LOCAL, PENDING_PUSH, UNCOMMITTED, MISSING,
                  UNVERIFIABLE):
            if counts.get(k):
                print(f"  {k:<13} {counts[k]}")
        for f in flagged[:40]:
            print(f"  FLAG {f['disposition']:<13} {f['path']}  ({f['task']})")
    return 1 if flagged else 0


def _cmd_rescue(args: argparse.Namespace) -> int:
    try:
        man = rescue(_root(), args.checkout, Path(args.quarantine))
    except RuntimeError as error:
        print("state=indeterminate")
        print(f"summary=stranded-work rescue unavailable: {error}")
        return 2
    print(json.dumps(man, indent=2, sort_keys=True))
    return 0


def _cmd_selftest(_a: argparse.Namespace) -> int:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import tests.test_stranded_sweep as t
    return t.run_as_selftest()


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    w = sub.add_parser("worktrees", description="scan every checkout for T1/T2 stranded work", help="scan every checkout for T1/T2 stranded work")
    w.add_argument("--json", action="store_true")
    w.set_defaults(func=_cmd_worktrees)

    a = sub.add_parser("artifacts", description="check closed-task artifacts against origin/main", help="check closed-task artifacts against origin/main")
    a.add_argument("--input", required=True, help="JSON [{task, paths:[...]}]")
    a.add_argument("--fresh-hours", type=float, default=DEFAULT_FRESH_HOURS)
    a.add_argument("--json", action="store_true")
    a.set_defaults(func=_cmd_artifacts)

    r = sub.add_parser("rescue", description="NON-DESTRUCTIVE copy-out of one checkout", help="NON-DESTRUCTIVE copy-out of one checkout")
    r.add_argument("checkout", help="path relative to the parent, or '.'")
    r.add_argument("--quarantine", required=True)
    r.set_defaults(func=_cmd_rescue)

    s = sub.add_parser("selftest", description="run the decision-table tests", help="run the decision-table tests")
    s.set_defaults(func=_cmd_selftest)

    args = ap.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
