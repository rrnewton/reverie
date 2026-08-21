#!/usr/bin/env python3
"""Kick off a GitHub Actions run BY HAND on a chosen commit, and report it.

WHY THIS EXISTS. Owner directive 2026-08-21: "I would like our CI on GitHub to
remain disabled in the sense that it does not run automatically. However ... I
want an agent that is investigating CI to be able to kick off runs manually on
recent main commits to see what they're doing on GitHub as well as see what
they're doing locally." Automatic triggers are gone from the workflow files;
this is the way back in. It starts runs, it reports them, and it decides
nothing: no label is written, no gate is minted, no landing consults it
(owner directive 2026-08-17 removed enforcement, and this must not reintroduce
it under a new name).

THE TRAP IT REFUSES TO WALK INTO, MEASURED TWICE. While a repository's Actions
switch is off, `POST .../dispatches` RETURNS SUCCESS (204) and creates a run
that has ZERO JOBS, stays `queued` forever, cannot be cancelled (HTTP 500) and
cannot be deleted (HTTP 403). Observed 2026-08-11 (hermit run 31519244713) and
again 2026-08-21 (dev-hermit run 32508433424). A dispatch tool that trusted the
204 would tell an investigating agent it had started CI when it had started
nothing, so this one READS THE SWITCH FIRST and refuses while it is off, naming
the exact command that turns it on. Costing an operator one refusal is cheaper
than an inert queued record nobody can remove.

RUNNING A COMMIT THAT IS NOT A BRANCH TIP. The dispatch API takes `ref`, and
GitHub documents it as a branch or a tag -- not an arbitrary SHA. So a commit
that is a branch tip is dispatched on that branch, and any other commit gets a
lightweight tag `manual-run/<sha12>` created through the API and dispatched on
that. The tag is kept: it is the only durable record of what an investigating
agent asked GitHub to build, and deleting refs to tidy up is not this tool's
business. `--delete-tag` removes one on request.

WHICH RUN IS MINE. A dispatch returns no run id. The workflow id plus a
created-at floor taken immediately BEFORE the POST identifies it without
guessing: any newer `workflow_dispatch` run of that workflow is the one this
process started, and the run's own `head_sha` is then checked against the
commit that was asked for, so a wrong answer is reported rather than believed.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone


DEFAULT_REPO = "rrnewton/dev-hermit"
DEFAULT_BRANCH = "main"
DEFAULT_POLL_SECONDS = 20
DEFAULT_TIMEOUT_SECONDS = 3600
GH_CALL_TIMEOUT = 120
TAG_PREFIX = "manual-run/"
SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")

# The verdicts this tool prints. `NO_JOBS` is separate from `RUNNING` on
# purpose: it is the exact shape the disabled-Actions trap produces, and
# collapsing it into "still going" is how that trap stays invisible.
PENDING = "PENDING"
RUNNING = "RUNNING"
NO_JOBS = "NO_JOBS"
GREEN = "GREEN"
RED = "RED"
OTHER = "OTHER"


class ManualRunError(RuntimeError):
    """Something the caller must fix; printed as a refusal, never a verdict."""


# --------------------------------------------------------------------------
# GitHub access. Every call goes through with-proxy, matching
# github_main_health.py: a bare `gh` cannot reach api.github.com from a
# devserver, and a missing proxy must be a loud refusal rather than a network
# error mistaken for "GitHub says no".
# --------------------------------------------------------------------------


def gh(args: list[str], *, allow_fail: bool = False) -> tuple[int, str, str]:
    if not shutil.which("with-proxy"):
        raise ManualRunError(
            "with-proxy was not found; GitHub queries from this box must use it"
        )
    proc = subprocess.run(
        ["with-proxy", "gh", *args],
        capture_output=True,
        text=True,
        timeout=GH_CALL_TIMEOUT,
    )
    if proc.returncode != 0 and not allow_fail:
        raise ManualRunError(
            f"gh {' '.join(args)} failed (rc={proc.returncode}): "
            f"{(proc.stderr or proc.stdout).strip()}"
        )
    return proc.returncode, proc.stdout, proc.stderr


def gh_json(args: list[str]):
    _, out, _ = gh(args)
    return json.loads(out) if out.strip() else None


# --------------------------------------------------------------------------
# Pure helpers (unit-tested; no network).
# --------------------------------------------------------------------------


def parse_repo(value: str) -> str:
    if not REPO_RE.match(value or ""):
        raise ManualRunError(f"--repo must be OWNER/REPO, got {value!r}")
    return value


def tag_for(sha: str) -> str:
    if not SHA_RE.match(sha or ""):
        raise ManualRunError(f"not a commit sha: {sha!r}")
    return TAG_PREFIX + sha[:12]


def triggers_of(yaml_text: str) -> list[str]:
    """Name the event triggers declared in a workflow file, by reading its
    `on:` block textually.

    Deliberately not a YAML parse: this runs against files fetched from
    arbitrary commits, and a file this tool cannot parse must still be
    reportable. Keys at exactly two-space indent inside the `on:` block are the
    events; `on:` written inline (`on: push`) is handled too.
    """
    lines = yaml_text.split("\n")
    start = None
    for index, line in enumerate(lines):
        if line.startswith("on:") or line.startswith('"on":') or line.startswith("'on':"):
            start = index
            break
    if start is None:
        return []
    inline = lines[start].split(":", 1)[1].strip()
    if inline and not inline.startswith("#"):
        return [inline.strip("[]").strip()]
    found: list[str] = []
    for line in lines[start + 1:]:
        if line and not line[0].isspace():
            break
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" "))
        if indent == 2 and stripped.endswith(":"):
            found.append(stripped[:-1])
        elif indent == 2 and ":" in stripped:
            found.append(stripped.split(":", 1)[0])
    return found


AUTOMATIC_TRIGGERS = frozenset(
    {"push", "pull_request", "pull_request_target", "schedule", "merge_group",
     "workflow_run", "issue_comment", "issues", "release", "create", "delete",
     "repository_dispatch", "check_run", "check_suite", "status", "label",
     "milestone", "page_build", "project", "watch", "fork", "gollum",
     "deployment", "deployment_status", "discussion", "discussion_comment",
     "registry_package", "public", "member", "branch_protection_rule"}
)


def automatic_triggers(triggers: list[str]) -> list[str]:
    return [t for t in triggers if t in AUTOMATIC_TRIGGERS]


def classify(run: dict) -> str:
    """One word for what a run is doing, with NO_JOBS kept distinct."""
    status = (run.get("status") or "").lower()
    conclusion = (run.get("conclusion") or "").lower()
    if status == "completed":
        if conclusion == "success":
            return GREEN
        if conclusion in {"failure", "timed_out", "startup_failure", "action_required"}:
            return RED
        return OTHER
    if status == "queued" and run.get("job_count") == 0:
        return NO_JOBS
    if status in {"queued", "waiting", "pending", "requested"}:
        return PENDING
    return RUNNING


@dataclass(frozen=True)
class Workflow:
    id: int
    name: str
    path: str
    state: str

    @property
    def file(self) -> str:
        return self.path.rsplit("/", 1)[-1]


def select_workflows(available: list[Workflow], requested: list[str]) -> list[Workflow]:
    """Resolve caller-named workflow files against the repository registry.

    An unmatched name is a REFUSAL, not a silent skip: "I ran the ones I
    recognised" is how an investigating agent ends up reading a green that
    never covered the thing they named.
    """
    if not requested:
        return [w for w in available if w.state == "active"]
    chosen: list[Workflow] = []
    for name in requested:
        matches = [w for w in available if w.file == name or w.path == name or w.name == name]
        if not matches:
            known = ", ".join(sorted(w.file for w in available))
            raise ManualRunError(f"no workflow named {name!r}; registry has: {known}")
        chosen.extend(matches)
    return chosen


# --------------------------------------------------------------------------
# Operations.
# --------------------------------------------------------------------------


def actions_enabled(repo: str) -> bool:
    data = gh_json(["api", f"repos/{repo}/actions/permissions"])
    enabled = (data or {}).get("enabled")
    if not isinstance(enabled, bool):
        raise ManualRunError(
            f"repos/{repo}/actions/permissions did not report a boolean enabled field"
        )
    return enabled


def require_actions_enabled(repo: str) -> None:
    if actions_enabled(repo):
        return
    raise ManualRunError(
        f"GitHub Actions is OFF for {repo}, so a dispatch would return success and\n"
        f"  create a queued run with zero jobs that never runs, cannot be cancelled\n"
        f"  (HTTP 500) and cannot be deleted (HTTP 403). Refusing to create one.\n"
        f"  Turn it on with:\n"
        f"    with-proxy gh api -X PUT repos/{repo}/actions/permissions \\\n"
        f"      -F enabled=true -f allowed_actions=all\n"
        f"  Leaving it on is only safe while no workflow in {repo} carries an\n"
        f"  automatic trigger. Check with: ci-hub/bin/manual-run --repo {repo} --list"
    )


def list_workflows(repo: str) -> list[Workflow]:
    data = gh_json(["api", f"repos/{repo}/actions/workflows", "--paginate"]) or {}
    return [
        Workflow(id=w["id"], name=w["name"], path=w["path"], state=w["state"])
        for w in data.get("workflows", [])
    ]


def file_at(repo: str, path: str, ref: str) -> str | None:
    rc, out, _ = gh(
        ["api", f"repos/{repo}/contents/{path}?ref={ref}", "--jq", ".content"],
        allow_fail=True,
    )
    if rc != 0 or not out.strip():
        return None
    import base64

    return base64.b64decode(out.strip()).decode("utf-8", "replace")


def resolve_commit(repo: str, commit: str | None, branch: str) -> tuple[str, str]:
    """Return (full_sha, ref_to_dispatch).

    A commit that is the branch tip is dispatched on the branch -- no ref is
    created for the common case of "run current main". Anything else gets a
    tag, because the API does not accept a bare SHA.
    """
    tip = gh_json(["api", f"repos/{repo}/commits/{branch}", "--jq", ".sha"])
    if not isinstance(tip, str):
        raise ManualRunError(f"{repo} does not resolve branch {branch!r}")
    if commit is None:
        return tip, branch
    full = gh_json(["api", f"repos/{repo}/commits/{commit}", "--jq", ".sha"])
    if not isinstance(full, str):
        raise ManualRunError(f"{repo} does not resolve commit {commit!r}")
    if full == tip:
        return full, branch
    tag = tag_for(full)
    existing = gh_json(["api", f"repos/{repo}/git/ref/tags/{tag}", "--jq", ".object.sha"])
    if existing != full:
        rc, _, err = gh(
            ["api", "-X", "POST", f"repos/{repo}/git/refs",
             "-f", f"ref=refs/tags/{tag}", "-f", f"sha={full}"],
            allow_fail=True,
        )
        if rc != 0 and "already exists" not in err.lower():
            raise ManualRunError(f"could not create tag {tag} at {full}: {err.strip()}")
    return full, tag


def newest_run_created_at(repo: str, workflow_id: int) -> str:
    data = gh_json([
        "api",
        f"repos/{repo}/actions/workflows/{workflow_id}/runs?per_page=1",
    ]) or {}
    runs = data.get("workflow_runs") or []
    return runs[0]["created_at"] if runs else "1970-01-01T00:00:00Z"


def dispatch(repo: str, workflow: Workflow, ref: str, inputs: dict[str, str]) -> None:
    args = ["api", "-X", "POST",
            f"repos/{repo}/actions/workflows/{workflow.id}/dispatches",
            "-f", f"ref={ref}"]
    for key, value in inputs.items():
        args += ["-f", f"inputs[{key}]={value}"]
    rc, _, err = gh(args, allow_fail=True)
    if rc != 0:
        raise ManualRunError(
            f"dispatch of {workflow.file} at {ref} was REFUSED: {err.strip()}"
        )


def find_run(repo: str, workflow: Workflow, floor: str) -> dict | None:
    """The newest workflow_dispatch run of this workflow created after `floor`.

    The floor is read immediately before the POST, so anything newer is this
    process's run. `head_sha` is NOT used to select -- it is checked in the
    report instead, because silently filtering on it would turn "GitHub built
    the wrong commit" into "no run appeared".
    """
    data = gh_json([
        "api",
        f"repos/{repo}/actions/workflows/{workflow.id}/runs"
        f"?event=workflow_dispatch&per_page=20",
    ]) or {}
    for run in data.get("workflow_runs") or []:
        if run["created_at"] > floor:
            return run
    return None


def job_summary(repo: str, run_id: int) -> tuple[int, list[tuple[str, str]]]:
    data = gh_json(["api", f"repos/{repo}/actions/runs/{run_id}/jobs?per_page=100"]) or {}
    jobs = data.get("jobs") or []
    return len(jobs), [
        (j.get("name", "?"), (j.get("conclusion") or j.get("status") or "?"))
        for j in jobs
    ]


# --------------------------------------------------------------------------
# Reporting.
# --------------------------------------------------------------------------


def print_listing(repo: str, workflows: list[Workflow], ref: str) -> int:
    enabled = actions_enabled(repo)
    print(f"repo {repo}   actions_enabled={enabled}   ref={ref}")
    if not enabled:
        print("  NOTE: with the switch off, nothing below can run, dispatch or not.")
    bad = 0
    for wf in sorted(workflows, key=lambda w: w.file):
        text = file_at(repo, wf.path, ref)
        if text is None:
            print(f"  {wf.file:<28} state={wf.state:<18} (not present at {ref})")
            continue
        trig = triggers_of(text)
        auto = automatic_triggers(trig)
        manual = "workflow_dispatch" in trig
        if auto:
            bad += 1
        print(
            f"  {wf.file:<28} state={wf.state:<18} "
            f"manual={'yes' if manual else 'NO':<3} "
            f"automatic={','.join(auto) if auto else 'none'}"
        )
    if bad:
        print(
            f"\n  {bad} workflow(s) still carry an automatic trigger. While the Actions\n"
            f"  switch is on, those fire by themselves -- which owner directive\n"
            f"  2026-08-21 forbids. Remove the triggers or keep the switch off."
        )
    return 0


def run_and_report(args: argparse.Namespace) -> int:
    repo = parse_repo(args.repo)
    require_actions_enabled(repo)
    available = list_workflows(repo)
    chosen = select_workflows(available, args.workflow)
    sha, ref = resolve_commit(repo, args.commit, args.branch)

    inputs = {}
    for pair in args.input or []:
        if "=" not in pair:
            raise ManualRunError(f"--input must be KEY=VALUE, got {pair!r}")
        key, value = pair.split("=", 1)
        inputs[key] = value

    print(f"repo   {repo}")
    print(f"commit {sha}")
    print(f"ref    {ref}" + ("  (tag created for a non-tip commit)"
                             if ref.startswith(TAG_PREFIX) else ""))
    print(f"kicking off {len(chosen)} workflow(s) by hand\n")

    started: list[tuple[Workflow, str]] = []
    for wf in chosen:
        floor = newest_run_created_at(repo, wf.id)
        dispatch(repo, wf, ref, inputs)
        started.append((wf, floor))
        print(f"  dispatched {wf.file}")

    # GitHub materializes the run a moment after the 204.
    found: dict[int, dict] = {}
    deadline = time.time() + 60
    while time.time() < deadline and len(found) < len(started):
        for wf, floor in started:
            if wf.id in found:
                continue
            run = find_run(repo, wf, floor)
            if run:
                found[wf.id] = run
        if len(found) < len(started):
            time.sleep(5)

    for wf, _ in started:
        run = found.get(wf.id)
        if run is None:
            print(f"  {wf.file}: dispatch accepted but NO RUN APPEARED within 60s")
        else:
            print(f"  {wf.file}: run {run['id']}  {run['html_url']}")
    if not args.wait:
        print("\nnot waiting; re-read with: "
              f"ci-hub/bin/manual-run --repo {repo} --status " +
              " ".join(str(r["id"]) for r in found.values()))
        return 0
    return wait_and_report(repo, [r["id"] for r in found.values()], sha, args)


def wait_and_report(repo: str, run_ids: list[int], sha: str | None,
                    args: argparse.Namespace) -> int:
    if not run_ids:
        print("no runs to watch")
        return 1
    deadline = time.time() + args.timeout
    print()
    verdicts: dict[int, dict] = {}
    while True:
        pending = 0
        for run_id in run_ids:
            run = gh_json(["api", f"repos/{repo}/actions/runs/{run_id}"]) or {}
            count, _ = job_summary(repo, run_id)
            run["job_count"] = count
            verdicts[run_id] = run
            if classify(run) in {PENDING, RUNNING, NO_JOBS}:
                pending += 1
        stamp = datetime.now(timezone.utc).strftime("%H:%M:%SZ")
        line = "  ".join(
            f"{v.get('name', '?')}={classify(v)}" for v in verdicts.values()
        )
        print(f"[{stamp}] {line}")
        if pending == 0:
            break
        if time.time() >= deadline:
            print(f"\ntimed out after {args.timeout}s with {pending} run(s) unfinished")
            break
        time.sleep(args.poll)

    print("\nRESULT")
    worst = 0
    for run_id, run in verdicts.items():
        verdict = classify(run)
        count, jobs = job_summary(repo, run_id)
        head = run.get("head_sha", "?")
        matched = "" if (sha is None or head == sha) else f"  !! head {head} != asked {sha}"
        print(f"  {verdict:<8} {run.get('name')}  run {run_id}  jobs={count}{matched}")
        for name, conclusion in jobs:
            print(f"             {conclusion:<12} {name}")
        print(f"             {run.get('html_url')}")
        if verdict == NO_JOBS:
            print("             ZERO JOBS: the run was created but nothing executed.")
        if verdict in {RED, OTHER, NO_JOBS}:
            worst = 1
    return worst


def status_only(args: argparse.Namespace) -> int:
    repo = parse_repo(args.repo)
    args.wait = True
    return wait_and_report(repo, [int(r) for r in args.status], None, args)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="ci-hub/bin/manual-run",
        description="Kick off GitHub Actions BY HAND on a chosen commit and report it.",
        epilog=(
            "Automatic triggers were removed from the workflow files (owner "
            "directive 2026-08-21); this is the only way to start CI on GitHub. "
            "It reports; it gates nothing."
        ),
    )
    parser.add_argument("--repo", default=DEFAULT_REPO, help=f"default {DEFAULT_REPO}")
    parser.add_argument("--branch", default=DEFAULT_BRANCH,
                        help=f"branch whose tip is the default commit (default {DEFAULT_BRANCH})")
    parser.add_argument("--commit", help="exact commit to run; default is the branch tip")
    parser.add_argument("--workflow", action="append", default=[],
                        help="workflow file name; repeatable; default is every active one")
    parser.add_argument("--input", action="append", default=[],
                        help="KEY=VALUE workflow_dispatch input; repeatable")
    parser.add_argument("--wait", action="store_true", help="poll until every run finishes")
    parser.add_argument("--poll", type=int, default=DEFAULT_POLL_SECONDS)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--list", action="store_true",
                        help="report each workflow's triggers at the chosen ref and exit")
    parser.add_argument("--status", nargs="+", metavar="RUN_ID",
                        help="report existing runs instead of starting any")
    parser.add_argument("--delete-tag", metavar="SHA",
                        help=f"remove the {TAG_PREFIX}<sha12> tag created for a commit")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        repo = parse_repo(args.repo)
        if args.delete_tag:
            full = gh_json(["api", f"repos/{repo}/commits/{args.delete_tag}", "--jq", ".sha"])
            tag = tag_for(full if isinstance(full, str) else args.delete_tag)
            gh(["api", "-X", "DELETE", f"repos/{repo}/git/refs/tags/{tag}"])
            print(f"deleted tag {tag} in {repo}")
            return 0
        if args.status:
            return status_only(args)
        if args.list:
            return print_listing(repo, list_workflows(repo), args.commit or args.branch)
        return run_and_report(args)
    except ManualRunError as error:
        print(f"manual-run: REFUSED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
