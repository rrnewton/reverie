#!/usr/bin/env python3
"""Keep the git objects the validate ledger names reachable, so gc cannot drop them.

WHY THIS EXISTS
===============
The ledger records a validate run against an exact commit ID, and consumers read
those IDs back and ask git about them later. Nothing kept those objects reachable.
An off-main commit -- a branch state that was validated and then squashed, rebased
or abandoned -- is present in the checkout but reachable from no ref, and
`git gc --prune=now` deletes it.

That is not hypothetical. On 2026-08-20 `wall_series.py --gate-regression` refused
on EVERY tick for a whole session and produced no wall-time verdict at all, because
three objects its ancestry check needed were gone from the checkout. Demonstrated
again while writing this, in a scratch repo built to the same shape: a commit pushed
to a branch that was then deleted survives a fetch, is reachable from no ref, and is
absent immediately after `git gc --prune=now`.

Measured on this checkout when this was written: of 303 distinct hermit commits the
ledger names, 294 were present and 39 of those were reachable from NO ref -- one
`git gc --prune=now` from vanishing. Reverie: 31 named, 31 present, 3 unreachable.

WHY PINNING RATHER THAN FETCHING ON DEMAND
==========================================
The alternative was to let the consumer re-fetch an object when it finds one absent.
That was rejected on measurement, not taste:

  * It cannot work for every object. Of 16 ledger-named hermit objects this checkout
    lacked, 9 do not exist upstream at all -- `gh api .../commits/<sha>` answers
    "No commit found for SHA". Those were validated on another host and never left
    its disk. No fetch will ever bring them back.
  * It puts the network inside a health gate. wall_series.py already records that an
    earlier design "made this gate grow linearly with retained evidence until
    tick-hub's 30-second bound killed it before it emitted a verdict".
  * It treats the symptom. A fetched object is still unreachable, so it is pruned
    again and re-fetched forever.

Re-fetching remains the right RECOVERY step for an object already lost, which is why
wall_series.py's refusal message spells that command out. This module is the
prevention half: stop them being lost in the first place.

WHAT KEEPS THIS CORRECT
=======================
Two properties, both of which are the point:

1. THE REF SET IS DERIVED, AND IT SHRINKS AS WELL AS GROWS. Every run recomputes the
   set from the ledger and DELETES refs for commits the ledger no longer names. The
   namespace can therefore never exceed the ledger's own distinct-commit count, and
   it tracks the ledger downward when rows are pruned. A keep-alive that only ever
   adds would be its own problem; `--json removed` is how you see it subtract.
2. IT REPORTS NUMBERS ON EVERY RUN. A mechanism that pins objects but is never
   watched is indistinguishable from one that does not work, which is a pattern this
   project hit repeatedly. Every run prints created/removed/pinned/absent so a tick
   can show the pinning actually happening.

Scale, so nobody has to guess: the ledger named 633 distinct commits over 18.1 days,
about 35 per day. This checkout already carries ~16,000 refs across a dozen
namespaces (refs/pr-triage alone has 1227), so this is a few percent on top of
established practice, not a new class of load.

WHAT THIS DELIBERATELY DOES NOT DO
==================================
It does not weaken any consumer's refusal. wall_series.py refusing its whole
population when it cannot classify an object stays exactly as it is: a checkout
missing objects cannot be trusted to have a complete `rev-list origin/main` either.
This removes the cause; it does not relax the check.
It also does not pin an object that is absent -- there is nothing to point a ref at.
Those are counted and reported so the gap stays visible.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Iterable

TOOL_ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", TOOL_ROOT)).resolve()
VALIDATE_DIR = TOOL_ROOT / "ci-hub" / "validate"
if str(VALIDATE_DIR) not in sys.path:
    sys.path.insert(0, str(VALIDATE_DIR))

from qualified_rows import load_rows  # noqa: E402

DEFAULT_LEDGER = STATE_ROOT / "ledger"

# Plain description, not a coined label: these refs exist because the ledger
# references the object. One ref per commit, named by the commit, so the mapping is
# obvious from `git for-each-ref` without consulting anything.
REF_PREFIX = "refs/ledger-referenced"

# Canonical checkout identities and their paths below the workspace root.
DEFAULT_CHECKOUTS = {"hermit": "hermit", "reverie": "reverie"}

# Every repository spelling currently present in the canonical ledger. Aliases
# are combined before syncing because sync() makes one checkout's ref namespace
# equal to the supplied set; syncing aliases separately would let the second
# spelling remove the first spelling's pins.
LEDGER_REPO_CHECKOUT = {
    None: "hermit",
    "hermit": "hermit",
    "reverie": "reverie",
    "rrnewton/reverie": "reverie",
}


def ledger_commits(path: Path) -> dict[str | None, set[str]]:
    """Distinct commit IDs the ledger names, grouped by its `repo` column."""
    rows, _malformed = load_rows(path)
    grouped: dict[str | None, set[str]] = {}
    for row in rows:
        commit = row.get("commit")
        repo = row.get("repo")
        # Canonical aggregate semantics: validation rows written before the
        # repository column existed came only from Hermit, so a missing/empty
        # value routes to Hermit while remaining a separate source group for
        # reporting. A nonempty unknown spelling must remain visible below.
        if repo is None or repo == "":
            repo = None
        elif not isinstance(repo, str):
            raise ValueError(
                f"ledger repo value is not a string or null: {repo!r}"
            )
        # Retain every repository spelling even when all of its commit fields
        # are malformed. Unknown values are a routing failure and must remain
        # visible rather than disappearing with an invalid commit.
        commits = grouped.setdefault(repo, set())
        if not commit:
            continue
        if not isinstance(commit, str) or len(commit) != 40:
            continue
        if any(c not in "0123456789abcdef" for c in commit):
            continue
        commits.add(commit)
    return grouped


def commits_by_checkout(
    grouped: dict[str | None, set[str]],
) -> tuple[dict[str, set[str]], dict[str, int], list[str]]:
    """Combine source groups by checkout, retaining pre-union source counts."""
    combined: dict[str, set[str]] = {}
    source_counts: dict[str, int] = {}
    unknown = []
    for ledger_repo, commits in grouped.items():
        checkout_repo = LEDGER_REPO_CHECKOUT.get(ledger_repo)
        if checkout_repo is None:
            assert ledger_repo is not None
            unknown.append(ledger_repo)
            continue
        combined.setdefault(checkout_repo, set()).update(commits)
        source_counts[checkout_repo] = source_counts.get(checkout_repo, 0) + len(commits)
    return combined, source_counts, sorted(unknown)


def source_repo_results(grouped: dict[str | None, set[str]]) -> list[dict]:
    """Report every ledger spelling before checkout-level deduplication."""
    return [
        {
            "repo": repo,
            "checkout_repo": LEDGER_REPO_CHECKOUT.get(repo),
            "ledger_commits": len(commits),
        }
        for repo, commits in sorted(
            grouped.items(), key=lambda item: (item[0] is not None, str(item[0]))
        )
    ]


def _git(checkout: Path, *args: str, stdin: str | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(checkout), *args],
        input=stdin, capture_output=True, text=True, timeout=120,
    )


def present_objects(checkout: Path, commits: Iterable[str]) -> set[str]:
    """Which of these commit IDs this checkout actually holds.

    GIT_NO_LAZY_FETCH is deliberate: this must report what is on disk, not trigger a
    network round trip per unknown object. Mirrors wall_series.py's batch check.
    """
    wanted = sorted(commits)
    if not wanted:
        return set()
    import os
    proc = subprocess.run(
        ["git", "-C", str(checkout), "cat-file", "--batch-check=%(objectname) %(objecttype)"],
        input="\n".join(wanted) + "\n", capture_output=True, text=True, timeout=120,
        env=os.environ | {"GIT_NO_LAZY_FETCH": "1"},
    )
    present = set()
    for line in proc.stdout.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1] == "commit":
            present.add(parts[0])
    return present


def existing_refs(checkout: Path) -> dict[str, str]:
    proc = _git(checkout, "for-each-ref", "--format=%(refname) %(objectname)", REF_PREFIX)
    refs: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        name, _, target = line.partition(" ")
        if name.startswith(REF_PREFIX + "/"):
            refs[name[len(REF_PREFIX) + 1:]] = target.strip()
    return refs


def sync(checkout: Path, commits: set[str], apply: bool) -> dict:
    """Make the ref set equal exactly the present subset of `commits`."""
    present = present_objects(checkout, commits)
    absent = sorted(commits - present)
    have = existing_refs(checkout)

    # A ref counts as correct only if it points at the commit it is named for.
    correct = {sha for sha, target in have.items() if target == sha}
    # Write anything not already correct: absent, or pointing somewhere else.
    # `update` is idempotent, so this repairs a mis-pointing ref in place.
    to_create = sorted(present - correct)
    # Remove only refs the ledger no longer names. THE TWO LISTS MUST BE DISJOINT:
    # an earlier version put a mis-pointing ref in BOTH, and because update-ref
    # --stdin applies commands in order the delete ran after the update and the pin
    # vanished. Its own test caught that, which is the reason the repair case is
    # tested separately rather than assumed to fall out of the add path.
    to_remove = sorted(set(have) - present)

    if apply and (to_create or to_remove):
        commands = []
        for sha in to_create:
            commands.append(f"update {REF_PREFIX}/{sha} {sha}\n")
        for sha in to_remove:
            commands.append(f"delete {REF_PREFIX}/{sha}\n")
        proc = _git(checkout, "update-ref", "--stdin", stdin="".join(commands))
        if proc.returncode != 0:
            raise RuntimeError(
                f"update-ref failed for {checkout}: {proc.stderr.strip()[:300]}")

    return {
        "checkout": str(checkout),
        "ledger_commits": len(commits),
        "present": len(present),
        "absent": len(absent),
        "absent_ids": absent[:8],
        "created": len(to_create),
        "removed": len(to_remove),
        "pinned_after": len(present) if apply else len(have),
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--ledger", default=str(DEFAULT_LEDGER))
    ap.add_argument("--root", default=str(STATE_ROOT))
    ap.add_argument("--dry-run", action="store_true",
                    help="report what would change without writing refs")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    root = Path(args.root)
    try:
        source_groups = ledger_commits(Path(args.ledger))
    except ValueError as error:
        failures = [str(error)]
        summary = "; ".join(failures) + " | ref synchronization not attempted"
        if args.json:
            print(json.dumps({"state": "failed", "summary": summary, "sources": [],
                              "repos": [], "failures": failures}, indent=2,
                             sort_keys=True))
        else:
            print("state=failed")
            print(f"summary={summary}")
        return 1
    source_repos = source_repo_results(source_groups)
    grouped, source_counts, unknown_repos = commits_by_checkout(source_groups)
    results = []
    failures = []
    if unknown_repos:
        failures.append(
            "unsupported ledger repo value(s): " + ", ".join(repr(repo) for repo in unknown_repos)
        )

    # An unknown repository has no authoritative checkout. Refuse before any
    # update-ref call rather than partially syncing the values we happened to
    # recognize and silently leaving the rest unprotected.
    if failures:
        state = "failed"
        summary = "; ".join(failures) + " | ref synchronization not attempted"
        if args.json:
            print(json.dumps({"state": state, "summary": summary, "sources": source_repos,
                              "repos": results, "failures": failures},
                             indent=2, sort_keys=True))
        else:
            print(f"state={state}")
            print(f"summary={summary}")
            for source in source_repos:
                source_name = source["repo"] if source["repo"] is not None else "(missing)"
                print(f"source_repo={source_name} checkout:{source['checkout_repo']} "
                      f"ledger:{source['ledger_commits']}")
        return 1

    for repo, relative in sorted(DEFAULT_CHECKOUTS.items()):
        checkout = root / relative
        if not (checkout / ".git").exists() and not (checkout / "HEAD").exists():
            # Not initialized here. Say so rather than silently reporting zero,
            # which would read exactly like "nothing needed pinning".
            failures.append(f"{repo}: no checkout at {checkout}")
            continue
        try:
            results.append({
                "repo": repo,
                "source_commits": source_counts.get(repo, 0),
                **sync(checkout, grouped.get(repo, set()), apply=not args.dry_run),
            })
        except (RuntimeError, OSError, subprocess.SubprocessError) as error:
            failures.append(f"{repo}: {error}")

    created = sum(r["created"] for r in results)
    removed = sum(r["removed"] for r in results)
    pinned = sum(r["pinned_after"] for r in results)
    absent = sum(r["absent"] for r in results)
    state = "failed" if failures else "ok"
    summary = (
        f"{pinned} ledger-referenced object(s) kept reachable "
        f"(+{created} -{removed}); {absent} named object(s) absent from the "
        f"checkout and cannot be pinned"
    )
    if failures:
        summary = "; ".join(failures) + " | " + summary

    if args.json:
        print(json.dumps({"state": state, "summary": summary, "sources": source_repos,
                          "repos": results, "failures": failures},
                         indent=2, sort_keys=True))
    else:
        print(f"state={state}")
        print(f"summary={summary}")
        for source in source_repos:
            source_name = source["repo"] if source["repo"] is not None else "(missing)"
            print(f"source_repo={source_name} checkout:{source['checkout_repo']} "
                  f"ledger:{source['ledger_commits']}")
        for r in results:
            print(f"{r['repo']}=source:{r['source_commits']} "
                  f"ledger:{r['ledger_commits']} present:{r['present']} "
                  f"pinned:{r['pinned_after']} created:{r['created']} "
                  f"removed:{r['removed']} absent:{r['absent']}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
