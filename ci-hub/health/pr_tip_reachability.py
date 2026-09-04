#!/usr/bin/env python3
"""Report open pull requests whose head commit can no longer be fetched.

WHY THIS EXISTS. An open pull request whose branch has been deleted keeps
rendering normally: GitHub still serves its title, its head sha, its diff and its
comments from stored metadata. What it stops serving, eventually, is the OBJECT.
Once the branch ref is gone the tip is unreferenced, and it survives only until
the remote's garbage collection reaches it -- at which point the pull request is
still open, still listed, still reviewable in the web UI, and the work in it is
gone.

⚠️ NOTHING REPORTS THIS CLASS. Measured 2026-08-25: a sweep of all 27 open pull
requests took 55 seconds and found 0 unreachable tips -- but two commits were
observed going from readable to unreadable inside a ten-minute window earlier the
same evening. A retrospective count CANNOT see what was already collected, so any
number this gate produces is a LOWER BOUND on the population that has passed
through the condition, and only an exact count of the population standing at the
moment it runs. That distinction is the whole reason to run it on a clock rather
than to audit for it: the audit measures the survivors.

WHAT COUNTS AS UNREACHABLE, and why two probes rather than one. A tip is reported
only when BOTH routes fail:

  1. `git fetch <sha>`                -- the remote will serve an unreferenced
                                         object by explicit sha for as long as it
                                         has not been collected.
  2. `git fetch refs/pull/N/head`     -- GitHub retains its own ref per pull
                                         request, independently of the branch.

Either one succeeding means the work is recoverable, so either one succeeding
means this is not an incident. Requiring both to fail is what keeps this
DETECT-ONLY and quiet: a deleted branch alone is routine and is reported as a
note, never as the alarm.

⚠️ THE PROBE MUST NOT READ A LOCAL CLONE. The first version of this sweep asked
`git cat-file -e <sha>` in a working checkout, and 27 of 27 heads answered
"present" -- because the clone had fetched them earlier. A tip deleted on the
remote and still cached locally reads as healthy, which is precisely the failure
this gate exists to catch, committed inside the instrument. The uniformity was the
tell: 27 of 27 identical is what an instrument that cannot fail looks like. This
runs every probe from a scratch repository with ZERO objects, so every answer
necessarily comes from the remote.

ADMISSION TEST (tick-hub, owner directive 2026-08-08): can ORC observe this
directly? NO -- it is GitHub-side object retention, which is exactly the class the
hub exists for. DETECT-ONLY: it reports and never deletes, pushes, or rescues. The
rescue pattern is owned elsewhere; conflating detection with repair would put a
push behind a timer.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_DEFAULT = "rrnewton/hermit"
REMOTE_URL = "https://github.com/{repo}.git"
FETCH_TIMEOUT_SECS = 60


def _run(argv: list[str], cwd: Path | None = None) -> tuple[int, str]:
    try:
        r = subprocess.run(
            argv, cwd=cwd, capture_output=True, text=True, timeout=FETCH_TIMEOUT_SECS
        )
    except (OSError, subprocess.TimeoutExpired):
        return 128, ""
    return r.returncode, (r.stdout or "") + (r.stderr or "")


def open_pull_requests(repo: str) -> list[dict] | None:
    """Every OPEN pull request, drafts included.

    Drafts are in scope deliberately: a draft's tip decays exactly like a ready
    one's, and the salvage-tagged drafts in this repository are precisely the
    heads whose content exists nowhere else.
    """
    args = [
        "pr", "list", "-R", repo, "--state", "open", "--limit", "200",
        "--json", "number,headRefOid,headRefName,isDraft,title",
    ]
    for argv in (["with-proxy", "gh", *args], ["gh", *args]):
        rc, out = _run(argv)
        if rc == 0 and out.strip():
            try:
                return json.loads(out)
            except json.JSONDecodeError:
                continue
    return None


def probe(scratch: Path, repo: str, number: int, sha: str) -> dict:
    """Both routes, from a repository holding no objects of its own."""
    url = REMOTE_URL.format(repo=repo)
    by_sha, _ = _run(
        ["with-proxy", "git", "fetch", "-q", "--force", url,
         f"{sha}:refs/tmp/reach/{number}"], cwd=scratch
    )
    by_pull, _ = _run(
        ["with-proxy", "git", "fetch", "-q", "--force", url,
         f"refs/pull/{number}/head:refs/tmp/pull/{number}"], cwd=scratch
    )
    pull_matches = False
    if by_pull == 0:
        rc, out = _run(
            ["git", "rev-parse", f"refs/tmp/pull/{number}"], cwd=scratch
        )
        pull_matches = rc == 0 and out.strip() == sha
    return {
        "number": number,
        "head": sha,
        "by_sha": by_sha == 0,
        "by_pull_ref": pull_matches,
        "reachable": by_sha == 0 or pull_matches,
    }


def sweep(repo: str) -> dict:
    prs = open_pull_requests(repo)
    if prs is None:
        # A failed query is not an empty result. Reporting "0 unreachable" here
        # would be the fail-open this gate exists to remove.
        return {"error": "could not enumerate open pull requests", "indeterminate": True}

    scratch = Path(tempfile.mkdtemp(prefix="pr-tip-reach-"))
    try:
        rc, _ = _run(["git", "init", "-q", "."], cwd=scratch)
        if rc != 0:
            return {"error": "could not create the scratch repository", "indeterminate": True}
        results = [probe(scratch, repo, p["number"], p["headRefOid"]) for p in prs]
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    unreachable = [r for r in results if not r["reachable"]]
    pull_only = [r for r in results if r["reachable"] and not r["by_sha"]]
    return {
        "repo": repo,
        "swept": len(results),
        "unreachable": len(unreachable),
        "unreachable_prs": [r["number"] for r in unreachable],
        "reachable_via_pull_ref_only": len(pull_only),
        "pull_ref_only_prs": [r["number"] for r in pull_only],
        # Stated in the payload, not only in prose: a consumer that reports this
        # number without the qualifier is making a stronger claim than the data.
        "count_is": "a LOWER BOUND -- tips already garbage-collected cannot be counted",
        "results": results,
    }


def summarize(report: dict) -> str:
    if report.get("indeterminate"):
        return f"INDETERMINATE -- {report['error']}"
    n = report["unreachable"]
    if n:
        prs = ", ".join(f"#{x}" for x in report["unreachable_prs"])
        return (
            f"{n} of {report['swept']} open pull request(s) have an UNREACHABLE tip "
            f"({prs}); this is a lower bound"
        )
    extra = ""
    if report["reachable_via_pull_ref_only"]:
        prs = ", ".join(f"#{x}" for x in report["pull_ref_only_prs"])
        extra = (
            f"; {report['reachable_via_pull_ref_only']} reachable only via refs/pull "
            f"({prs}) -- recoverable, worth a look"
        )
    return f"0 of {report['swept']} open pull requests have an unreachable tip{extra}"


def self_test() -> int:
    """Both arms, including the one that matters: the probe must be able to say NO.

    A sweep that reports zero from an instrument incapable of reporting nonzero is
    not a measurement. This asserts the negative arm against a well-formed sha
    that certainly does not exist, and the positive arm against real main.
    """
    failures = 0
    scratch = Path(tempfile.mkdtemp(prefix="pr-tip-selftest-"))
    try:
        _run(["git", "init", "-q", "."], cwd=scratch)
        absent = probe(scratch, REPO_DEFAULT, 999999,
                       "f866aaf10e32ee7cc0bd58ad14b9e3b21eeb1d18")
        if absent["reachable"]:
            print("FAIL: a nonexistent sha and pull ref reported as reachable; "
                  "the probe cannot say NO", file=sys.stderr)
            failures += 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    # summarize() must not render an indeterminate as a clean zero.
    if "INDETERMINATE" not in summarize({"indeterminate": True, "error": "x"}):
        print("FAIL: an indeterminate report renders as a result", file=sys.stderr)
        failures += 1
    if "lower bound" not in summarize(
        {"swept": 3, "unreachable": 1, "unreachable_prs": [7],
         "reachable_via_pull_ref_only": 0, "pull_ref_only_prs": []}
    ):
        print("FAIL: a nonzero count is reported without the lower-bound qualifier",
              file=sys.stderr)
        failures += 1

    # ⚠️ THE ALARM PATH, WHICH IS THE ONE THAT HAS NEVER RUN IN ANGER. tick-hub
    # refuses to emit a title whose `{summary}` it cannot resolve, so a missing
    # `summary=` line turns the one report that matters into a NO-SIGNAL. Assert
    # the key is printed on BOTH arms, and that the nonzero arm names the PRs.
    import contextlib
    import io as _io
    global sweep
    real_sweep = sweep
    try:
        for label, rep, want in (
            ("nonzero", {"repo": "x", "swept": 3, "unreachable": 1,
                         "unreachable_prs": [7], "reachable_via_pull_ref_only": 0,
                         "pull_ref_only_prs": []}, "#7"),
            ("zero", {"repo": "x", "swept": 3, "unreachable": 0,
                      "unreachable_prs": [], "reachable_via_pull_ref_only": 0,
                      "pull_ref_only_prs": []}, "0 of 3"),
        ):
            sweep = lambda _repo, report=rep: report
            buf = _io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = main(["--repo", "x"])
            emitted = buf.getvalue()
            line = next((l for l in emitted.splitlines() if l.startswith("summary=")), None)
            if code != 0:
                print(f"FAIL: the {label} production arm exited {code}", file=sys.stderr)
                failures += 1
            elif line is None:
                print(f"FAIL: the {label} production arm printed no `summary=` line; tick-hub "
                      "would refuse the emission and report NO-SIGNAL instead",
                      file=sys.stderr)
                failures += 1
            elif want not in line:
                print(f"FAIL: the {label} production arm's summary does not name its subject "
                      f"({want!r} absent from {line!r})", file=sys.stderr)
                failures += 1
    finally:
        sweep = real_sweep

    if failures:
        print(f"pr_tip_reachability --self-test: {failures} case(s) failed", file=sys.stderr)
        return 1
    print("PASS: the probe can report unreachable, an indeterminate does not render "
          "as zero, a nonzero count carries its lower-bound qualifier, and both "
          "arms print the `summary=` line tick-hub needs to emit the alarm")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--repo", default=REPO_DEFAULT)
    p.add_argument("--gate", action="store_true",
                   help="exit 1 when any open head has an unreachable tip")
    p.add_argument("--json", action="store_true", help="emit one JSON report")
    p.add_argument("--self-test", action="store_true",
                   help="prove the probe can report the condition it exists to find")
    args = p.parse_args(argv)

    if args.self_test:
        return self_test()

    report = sweep(args.repo)
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(f"pr-tip-reachability: {summarize(report)}")
        # ⚠️ THE ALARM CANNOT BE EMITTED WITHOUT THIS LINE. tick-hub resolves the
        # `{summary}` in this reminder's title from `key=value` stdout lines
        # (engine.parse_kv_lines). With no `summary=` key the emission raises
        # UnresolvedPlaceholderError, and the reader gets
        # `NO-SIGNAL ... missing placeholder(s)=summary` INSTEAD OF THE PULL
        # REQUEST NUMBERS whose work is about to be collected.
        #
        # It went unnoticed for the same reason the timeout did: this gate emits
        # only `when: failure`, it has never returned nonzero, and before
        # 2026-08-26 it exceeded its bound anyway -- so the one path that needed
        # this line is the one path never exercised. Bracketed both ways in
        # self_test(), including the nonzero arm, by invoking this production
        # main path rather than printing a duplicate line inside the test.
        print(f"summary={summarize(report)}", flush=True)

    if not args.gate:
        return 0
    # An indeterminate is not a pass: a query that failed says nothing about
    # reachability, and reporting it as clean is the fail-open this gate removes.
    if report.get("indeterminate"):
        return 2
    return 1 if report["unreachable"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
