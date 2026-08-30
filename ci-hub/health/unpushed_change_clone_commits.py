#!/usr/bin/env python3
"""Report commits held in ci-hub change clones and on no remote ref.

WHY THIS IS SEPARATE FROM `unpushed_parent_commits`

Change clones are SEPARATE REPOSITORIES. `git worktree list` in the parent
lists none of them, and no enumeration of one repository's refs can reach
another repository's objects. The boundary is structural, not a scoping choice
that could be widened, so the parent gate cannot be taught to see them. Its 0 is
the true answer for the parent and must stay that way; adding a second
population to it would forfeit the bracket that makes that 0 mean something.

WHY IT EXISTS

`dispose` refuses to remove a clone holding commits that exist on no remote ref.
That is a floor, not a fix: a refusal protects work already at risk, it does not
tell anyone the risk exists. Two commits sat in an abandoned clone for two days
and were found twice by accident, never by a signal.

THIS REPORTS AND DOES NOTHING ELSE. It never pushes, disposes or preserves. A
visibility mechanism that also mutates is two changes. It does fetch, which is
how it READS the remote; a fetch moves no branch, writes no commit and touches
no working tree, so no clone's work can be altered or lost by running this.

IT MUST ASK THE SAME QUESTION `dispose` ASKS

Two instruments reporting different counts for the same clones is worse than
either being slightly wrong, because the next reader cannot tell which is about
what. So the enumeration below is character-for-character the one in
`scripts/ci-hub-clone-change.py::unpushed_in_clone`, and the fetch matches its
`refresh_remote_tracking`. Both halves are needed. Measured 2026-08-15 over the
28 clones then present: the old `--all` enumeration counted 77 commits, of which
73 were `refs/pr-landing-planner/*` fetch destinations that exist on the remote
by construction; and all 4 survivors were ALSO already on their remotes, and
showed up only because those clones' remote-tracking refs were stale. Narrowing
the scope without refreshing would have left this gate reporting four alarms,
every one of them false.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import signal
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

MANIFEST = "ci-hub-change-manifest.json"
# The enclosing tick gives this gate 30 seconds.  A per-clone timeout longer
# than that is not a bound: one slow clone can consume the whole gate.  On
# 2026-08-20, 30 manifested clones took 30.01 seconds sequentially, while their
# local-only inspection took 4.70 seconds and warm fetches took 0.39-0.96
# seconds.  The whole inspection also has its own deadline below the outer
# timeout, so growth beyond today's population produces explicit undetermined
# rows instead of making the enclosing gate kill the process without a report.
OUTER_TIMEOUT_SECS = 30
INSPECTION_DEADLINE_SECS = 25
MAX_PARALLEL_CLONES = 12
FETCH_TIMEOUT_SECS = 5
LOCAL_GIT_TIMEOUT_SECS = 1
PROCESS_GROUP_TERM_GRACE_SECS = 0.2
# `ci-hub-clone-change.py create` defaults its destination to ~/temp/dev-hermit,
# so sibling clones land beside it. Scanning that one directory is what makes
# this tractable at all: the creation path knows where it puts clones.
DEFAULT_SEARCH_ROOT = Path("~/temp").expanduser()


class InspectionDeadlineExceeded(RuntimeError):
    pass


class MalformedManifest(RuntimeError):
    pass


def remaining_timeout(deadline: float, per_command_limit: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise InspectionDeadlineExceeded("whole-inspection deadline expired")
    return min(per_command_limit, remaining)


def terminate_process_group(process: subprocess.Popen[str]) -> None:
    """Stop and reap a timed-out command and descendants in its process group."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.communicate(timeout=PROCESS_GROUP_TERM_GRACE_SECS)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate()
        return
    # Reaping the direct wrapper does not prove every descendant exited: one
    # may ignore TERM and close the inherited stdout/stderr pipes.  The group
    # is unique because Popen used start_new_session, so a final group-directed
    # KILL cannot target another gate command.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def run_bounded(command: list[str], timeout: float) -> subprocess.CompletedProcess[str]:
    """Run one command in a killable process group and reap it on timeout."""
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        terminate_process_group(process)
        raise subprocess.TimeoutExpired(
            command, timeout, output=error.output, stderr=error.stderr
        ) from error
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def git(repo: Path, *args: str, deadline: float) -> str:
    out = run_bounded(
        ["git", "-C", str(repo), *args],
        remaining_timeout(deadline, LOCAL_GIT_TIMEOUT_SECS),
    )
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or f"git {' '.join(args)} failed")
    return out.stdout.strip()


def change_clones(root: Path) -> list[Path]:
    """Directories under `root` carrying a change manifest, sorted."""
    if not root.is_dir():
        raise RuntimeError(f"change-clone search root is unavailable: {root}")
    found = []
    for child in sorted(root.iterdir()):
        if child.is_dir() and (child / ".git" / MANIFEST).is_file():
            found.append(child)
    return found


def refresh_remote_tracking(clone: Path, deadline: float) -> tuple[bool, str | None]:
    """Update `refs/remotes/origin/*` from the remote; report whether it worked.

    Same refspec as the disposal tool, for the same reason: a clone's work may
    have been pushed to any branch, and a snapshot of unknown age is wrong in
    both directions -- it under-reports what is pushed, and it can over-report
    containment when a ref it still shows has since moved.
    """
    command = ["git", "-C", str(clone), "fetch", "--quiet", "origin",
               "+refs/heads/*:refs/remotes/origin/*"]
    if shutil.which("with-proxy"):
        command.insert(0, "with-proxy")
    try:
        result = run_bounded(
            command, remaining_timeout(deadline, FETCH_TIMEOUT_SECS)
        )
        if result.returncode == 0:
            return True, None
        return False, "remote fetch failed"
    except subprocess.TimeoutExpired:
        return False, "remote fetch timed out"


def read_manifest(clone: Path) -> dict[str, Any]:
    try:
        value = json.loads((clone / ".git" / MANIFEST).read_text())
    except (OSError, ValueError) as error:
        raise MalformedManifest(f"cannot read change manifest: {error}") from error
    if not isinstance(value, dict):
        raise MalformedManifest("change manifest is not a JSON object")
    return value


def inspect(clone: Path, deadline: float) -> dict[str, Any]:
    """Describe one clone, altering no work in it.

    The enumeration is `--branches` plus `HEAD`, matching
    `ci-hub-clone-change.py::unpushed_in_clone` exactly: every local branch, plus
    the detached HEAD an interrupted change leaves behind, which `--branches`
    alone would miss. It is deliberately NOT `--all`, which also sweeps
    fetch-destination namespaces such as `refs/pr-landing-planner/*` whose
    contents are on the remote by construction.

    `remote_reachable` is false when the refresh failed, and then the count is
    against a snapshot of unknown age. That is reported as undetermined, not as
    a number: a count nobody can date is not a measurement.
    """
    manifest = read_manifest(clone)
    reachable, fetch_reason = refresh_remote_tracking(clone, deadline)
    shas = [s for s in git(clone, "rev-list", "--branches", "HEAD",
                           "--not", "--remotes", deadline=deadline).splitlines() if s]
    dirty = [line for line in git(
        clone, "status", "--porcelain", deadline=deadline
    ).splitlines() if line]
    branch = git(clone, "rev-parse", "--abbrev-ref", "HEAD", deadline=deadline)
    return {
        "clone": str(clone),
        "task": manifest.get("task"),
        "branch": branch,
        # A stale snapshot may look unpushed in either direction.  Publish no
        # remote-containment claim unless the refresh that dates it succeeded.
        "unpushed_count": len(shas) if reachable else None,
        "tip": shas[0] if reachable and shas else None,
        "uncommitted_paths": len(dirty),
        "remote_reachable": reachable,
        "undetermined_reason": fetch_reason,
    }


def unfinished(clone: Path, reason: str) -> dict[str, Any]:
    """Represent an inspection failure without rounding it down to clean."""
    task = None
    try:
        value = json.loads((clone / ".git" / MANIFEST).read_text())
        if isinstance(value, dict):
            task = value.get("task")
    except (OSError, ValueError):
        pass
    return {
        "clone": str(clone),
        "task": task,
        "branch": None,
        "unpushed_count": None,
        "tip": None,
        "uncommitted_paths": None,
        "remote_reachable": False,
        "undetermined_reason": reason,
    }


def inspect_or_undetermined(clone: Path, deadline: float) -> dict[str, Any]:
    try:
        return inspect(clone, deadline)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        return unfinished(clone, f"inspection did not finish: {type(error).__name__}")


def inspect_all(
    clones: list[Path], inspection_budget: float = INSPECTION_DEADLINE_SECS
) -> list[dict[str, Any]]:
    """Inspect every manifested clone by one deadline, in deterministic order."""
    if not clones:
        return []
    deadline = time.monotonic() + inspection_budget
    workers = min(MAX_PARALLEL_CLONES, len(clones))
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(inspect_or_undetermined, clone, deadline) for clone in clones]
        return [future.result() for future in futures]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--search-root", default=str(DEFAULT_SEARCH_ROOT),
                    help="directory whose immediate children are inspected for change clones")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    root = Path(a.search_root).expanduser()
    try:
        clones = change_clones(root)
    except OSError as error:
        print("state=NO_RESULT")
        print(f"summary=NO_RESULT/change-clone-enumeration-failed: {error}")
        return 2
    except RuntimeError as error:
        print("state=NO_RESULT")
        print(f"summary=NO_RESULT/change-clone-enumeration-failed: {error}")
        return 2
    rows = inspect_all(clones)
    holding = [
        r for r in rows
        if (r["uncommitted_paths"] or 0)
        or (r["remote_reachable"] and (r["unpushed_count"] or 0))
    ]
    # A clone whose remote could not be reached is neither holding nor clean;
    # it is unmeasured, and saying so is the only honest report.
    undetermined = [r for r in rows if not r["remote_reachable"]]

    if a.json:
        print(json.dumps({"search_root": str(root), "clones": len(rows),
                          "holding": len(holding),
                          "undetermined": len(undetermined), "rows": rows},
                         sort_keys=True))
    else:
        print(f"unpushed-change-clone-commits search_root={root} "
              f"clones={len(rows)} holding={len(holding)} "
              f"undetermined={len(undetermined)}")
        for r in holding:
            unpushed = r["unpushed_count"]
            rendered_unpushed = unpushed if unpushed is not None else "UNDETERMINED"
            print(f"  {r['clone']}  task={r['task']} branch={r['branch']} "
                  f"unpushed={rendered_unpushed} "
                  f"uncommitted={r['uncommitted_paths']} "
                  f"tip={r['tip']}")
        for r in undetermined:
            print(f"  {r['clone']}  task={r['task']} branch={r['branch']} "
                  f"UNDETERMINED: {r['undetermined_reason']}; counts cannot be "
                  "treated as current")
        if not rows:
            print("  (no change clones present)")
        elif not holding and not undetermined:
            print("  (every change clone is clean and fully pushed)")
        # Reporting only. Disposal and preservation are separate operations with
        # their own refusals; nothing here mutates a clone.
        if undetermined:
            # `bounded_gate.py`'s spelling, so a reader already knows what it
            # means: the gate ran and could not determine its condition.  This
            # takes precedence over local dirty facts; stale remote refs cannot
            # support a definite remote-noncontainment claim.
            print("state=NO_RESULT")
            print(f"summary=NO_RESULT/remote-unreachable gate="
                  f"unpushed_change_clone_commits undetermined={len(undetermined)} "
                  f"holding_local={len(holding)} clones={len(rows)}")
        elif holding:
            print("summary=change clone(s) hold work that exists on no remote ref; "
                  "inspect and preserve or land it -- `dispose` will refuse them")
        else:
            print("summary=no change clone holds unpushed work")
    return 1 if holding or undetermined else 0


if __name__ == "__main__":
    sys.exit(main())
