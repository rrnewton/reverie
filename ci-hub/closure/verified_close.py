#!/usr/bin/env python3
"""Close a TaskGraph task only after its recorded reference verifies."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Sequence


sys.path.insert(0, str(Path(__file__).resolve().parent))
import content_presence  # noqa: E402  (sibling module in ci-hub/closure)

ROOT = Path(__file__).resolve().parents[2]
# One resolver for the TaskGraph, shared with every other ci-hub consumer.
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
import git_env  # noqa: E402
import taskgraph_db  # noqa: E402
PARENT_REPO = "rrnewton/dev-hermit"
Run = Callable[..., subprocess.CompletedProcess[str]]

CLOSED = 0
REFUSED = 1
UNVERIFIABLE = 2


@dataclass(frozen=True)
class Evidence:
    state: str
    kind: str
    reference: str
    resolved: str | None = None
    reason: str | None = None
    # Which verified landing state the code evidence carried. Code closes only
    # when the commit is an ancestor of freshly fetched target main, so the only
    # qualifying value is "landed". Artifact and run evidence use None.
    landing: str | None = None

    @property
    def rc(self) -> int:
        return {"verified": CLOSED, "refused": REFUSED}.get(
            self.state, UNVERIFIABLE
        )


def _run(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    timeout: float = 120,
) -> subprocess.CompletedProcess[str]:
    # `git -C <repo>` does not bind a repository when the caller exported
    # GIT_DIR/GIT_COMMON_DIR, and it does not bind the intended state when the
    # caller exported GIT_INDEX_FILE/GIT_WORK_TREE. Start every verifier child
    # from a clean repository environment. The code path's independent origin
    # check remains the authority that binds the requested owner/repository.
    env = git_env.sanitized_git_env()
    if command and command[0] == "tg":
        # This module WRITES: it is the only producer of CLOSURE-VERIFIED, and
        # it also runs `tg note` and `tg update --status closed`. An unbound
        # `tg` silently addresses an empty default database, so a misdirected
        # write would record closure evidence where no reader looks while the
        # real task keeps its landing debt. Refusing to bind is therefore
        # refusing to write -- UNVERIFIABLE, never a claimed close. git/gh calls
        # through this helper are deliberately left unbound and unaffected.
        try:
            resolution = taskgraph_db.resolve(env=env)
            env = taskgraph_db.child_env(resolution, env=env)
        except taskgraph_db.TaskGraphUnavailable as error:
            return subprocess.CompletedProcess(
                list(command), UNVERIFIABLE, "", f"taskgraph-unavailable: {error}"
            )
    try:
        return subprocess.run(
            list(command),
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=env,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return subprocess.CompletedProcess(list(command), UNVERIFIABLE, "", str(error))


def task_has_implemented_tag(task: str, *, run: Run = _run) -> bool:
    """Is `implemented` on the task right now?

    Publication records implementation evidence and adds this tag before the
    owning agent shepherds the task to landing. Requiring it for every real
    close preserves that ordering and keeps landing debt countable. Check-only
    verification does not close and therefore does not require the tag.
    """
    result = run(("tg", "show", task), cwd=ROOT)
    if result.returncode != 0:
        return False
    for line in result.stdout.splitlines():
        if line.strip().lower().startswith("tags:"):
            return "implemented" in line.lower()
    return False


def _json_object(output: str) -> dict[str, object] | None:
    try:
        value = json.loads(output)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def verify_code(
    reference: str,
    *,
    repo: str,
    source: Path,
    target: str,
    run: Run = _run,
) -> Evidence:
    result = run(
        (
            sys.executable,
            str(ROOT / "ci-hub/remediation/protocol.py"),
            "verify-landing",
            reference,
            "--repo",
            repo,
            "--source",
            str(source),
            "--target",
            target,
            "--json",
        ),
        cwd=ROOT,
    )
    payload = _json_object(result.stdout.strip())
    if payload is None:
        detail = (result.stderr or result.stdout or "verifier emitted no JSON").strip()
        return Evidence("unverifiable", "code", reference, reason=detail)
    state = str(payload.get("state") or "unverifiable")
    resolved = str(payload.get("resolved_sha") or "") or None
    reason = str(payload.get("reason") or state)
    if result.returncode == CLOSED and state == "landed" and resolved:
        # ANCESTRY SAID LANDED. NOW ASK WHETHER THE CONTENT ACTUALLY SURVIVED.
        #
        # `merge-base --is-ancestor` proves REACHABILITY, not that the commit's changes
        # are present. A merge resolved toward the wrong side, or a reconcile that drops
        # a hunk, leaves the SHA a perfectly good ancestor while its effect is gone --
        # demonstrated on a scratch clone, where a `checkout --ours` resolution gave
        # commit-reachability lost=0 while the other side's edit was absent from the file.
        # Closing such a task records a landing that did not happen.
        presence = content_presence.check(source, resolved, target)
        if presence.verdict == content_presence.CONTENT_LOST:
            return Evidence(
                "refused",
                "code",
                reference,
                resolved=f"{repo}@{resolved}",
                reason=(
                    "CONTENT-LOST: the commit is an ancestor of "
                    f"{target} but only {presence.hunks_present} of "
                    f"{presence.hunks_total} hunk(s) are present there. Ancestry passes "
                    "this; the content did not survive intact. Adjudicate before closing "
                    "-- either a reconcile dropped it, or a later commit superseded that "
                    f"region. Missing: {'; '.join(presence.missing[:3])}"
                ),
            )
        # INDETERMINATE is NOT converted into a refusal. A merge commit has zero hunks, and
        # a 0/0 content check is a NO-RESULT -- neither pass nor fail. Refusing on it would
        # reproduce the no-result-into-fail defect this check exists to expose, and would
        # block every legitimate merge-commit closure. It is recorded, not actioned.
        # Carry the repository WITH the SHA. `resolved=<sha>` alone was
        # unfalsifiable on inspection: a parent-repo task closed against
        # hermit's PR #56 recorded `CLOSURE-VERIFIED ... resolved=299e5b90`,
        # which reads correct until you discover that object does not exist in
        # dev-hermit at all. The 40-hex is still extractable by
        # ci-hub/directives/tg_landed.py, which regexes for it.
        return Evidence(
            "verified", "code", reference, resolved=f"{repo}@{resolved}", landing="landed"
        )
    if result.returncode == REFUSED and state == "not-landed":
        # Resolving a real commit proves publication, not landing. The owning
        # agent keeps the task open while shepherding it; the gateway refuses
        # every repository uniformly until fresh target-main ancestry passes.
        return Evidence(
            "refused",
            "code",
            reference,
            resolved=f"{repo}@{resolved}" if resolved else None,
            reason=f"code is not landed on {target}: {reason}",
        )
    return Evidence("unverifiable", "code", reference, resolved=resolved, reason=reason)


def verify_artifact(reference: str, *, run: Run = _run) -> Evidence:
    if reference.startswith(("https://", "http://")):
        result = run(
            (
                "with-proxy",
                "curl",
                "--fail",
                "--location",
                "--silent",
                "--output",
                "/dev/null",
                reference,
            ),
            cwd=ROOT,
        )
        if result.returncode == 0:
            return Evidence("verified", "artifact", reference, resolved=reference)
        detail = (result.stderr or "URL did not resolve").strip()
        return Evidence("unverifiable", "artifact", reference, reason=detail)

    path = Path(reference).expanduser()
    if not path.is_absolute():
        path = ROOT / path
    path = path.resolve()
    try:
        relative = str(path.relative_to(ROOT))
    except ValueError:
        return Evidence(
            "refused",
            "artifact",
            reference,
            reason="local artifact is outside the versioned workspace; use a URL",
        )
    # The authority for a parent artifact is `origin/main`, NOT this checkout.
    # Deliberately no `path.is_file()` and no `git ls-files` here: the parent
    # primary runs tens of commits behind origin (it was 41 behind on
    # 2026-08-06), and the only safe way to publish a parent artifact is from a
    # worktree off origin/main -- so a correctly published artifact is routinely
    # absent from this working tree and this index. Gating on either refused
    # every such closure with "artifact is not a file", which named the wrong
    # cause: the file was tracked, pushed, and ancestry-present.
    #
    # Nothing is weakened by dropping them. Existence, version-control, and
    # blob-ness are all re-established below against the freshly fetched
    # origin/main, which is strictly the stronger authority -- a working tree
    # can hold an untracked or locally-modified file that was never published.
    fetched = run(
        (
            "with-proxy",
            "git",
            "-C",
            str(ROOT),
            "fetch",
            "origin",
            "refs/heads/main:refs/remotes/origin/main",
        ),
        cwd=ROOT,
    )
    if fetched.returncode != 0:
        # An agent sandbox has no route to github, so this direct fetch always
        # 403s in-jail and artifact closure was therefore impossible for every
        # agent -- which is a large part of why the research tasks accumulated
        # unclosed. Retry the SAME fetch through herdr-run, the sanctioned egress
        # path, for which `git` is already allowlisted.
        #
        # This changes the TRANSPORT, never the guarantee: the ref is still
        # freshly fetched before ancestry is tested. A `--no-fetch` escape would
        # have been easier and wrong -- it would let a caller close against a
        # stale origin/main, which is the exact class of defect this gateway
        # exists to refuse.
        agent = os.environ.get("CI_HUB_HERDR_AGENT", "")
        herdr = ROOT / "agent-utils/bin/herdr-run"
        relayed = None
        if agent and herdr.is_file():
            relayed = run(
                (
                    str(herdr),
                    "--agent",
                    agent,
                    f"with-proxy git -C {ROOT} fetch origin "
                    f"refs/heads/main:refs/remotes/origin/main",
                ),
                cwd=ROOT,
            )
        if relayed is None or relayed.returncode != 0:
            detail = (fetched.stderr or "cannot fetch parent main").strip()
            if relayed is not None:
                detail += f" | herdr-run relay also failed: {(relayed.stderr or '').strip()[:160]}"
            elif not agent:
                detail += (" | set CI_HUB_HERDR_AGENT=<agent> to relay the fetch through"
                           " herdr-run when running inside an agent sandbox")
            return Evidence("unverifiable", "artifact", reference, reason=detail)
    # `-t` answers presence and object type in one call. The type check is
    # load-bearing: `cat-file -e origin/main:<dir>` succeeds for a TREE, so
    # existence alone would let a caller close a task against a directory. The
    # dropped `path.is_file()` used to supply that guard by accident.
    kind = run(
        ("git", "-C", str(ROOT), "cat-file", "-t", f"origin/main:{relative}"),
        cwd=ROOT,
    )
    if kind.returncode != 0:
        return Evidence(
            "refused", "artifact", reference, reason="artifact is not on parent main"
        )
    object_type = kind.stdout.strip()
    if object_type != "blob":
        return Evidence(
            "refused",
            "artifact",
            reference,
            reason=f"artifact on parent main is a {object_type or 'non-blob'}, not a file",
        )
    content = run(
        (
            "git",
            "-C",
            str(ROOT),
            "log",
            "-1",
            "--format=%H",
            "origin/main",
            "--",
            relative,
        ),
        cwd=ROOT,
    )
    content_oid = content.stdout.strip()
    if (
        content.returncode != 0
        or len(content_oid) != 40
        or any(character not in "0123456789abcdef" for character in content_oid)
    ):
        return Evidence(
            "unverifiable",
            "artifact",
            reference,
            reason="cannot resolve the artifact content commit on parent main",
        )
    ancestry = run(
        (
            "git",
            "-C",
            str(ROOT),
            "merge-base",
            "--is-ancestor",
            content_oid,
            "origin/main",
        ),
        cwd=ROOT,
    )
    if ancestry.returncode == 1:
        return Evidence(
            "refused",
            "artifact",
            reference,
            reason="artifact content commit is not an ancestor of parent main",
        )
    if ancestry.returncode != 0:
        return Evidence(
            "unverifiable",
            "artifact",
            reference,
            reason=(ancestry.stderr or "cannot verify artifact ancestry").strip(),
        )
    tip = run(("git", "-C", str(ROOT), "rev-parse", "origin/main"), cwd=ROOT)
    target_tip = tip.stdout.strip()
    if (
        tip.returncode != 0
        or len(target_tip) != 40
        or any(character not in "0123456789abcdef" for character in target_tip)
    ):
        return Evidence(
            "unverifiable",
            "artifact",
            reference,
            reason="cannot resolve parent main after fetching it",
        )
    resolved = (
        f"{PARENT_REPO}:{relative}@{content_oid};"
        f"target=main@{target_tip}"
    )
    return Evidence("verified", "artifact", reference, resolved=resolved)


def verify_run(run_id: str, *, repo: str, run: Run = _run) -> Evidence:
    if not run_id.isdecimal() or int(run_id) <= 0:
        return Evidence("unverifiable", "run", run_id, reason="run ID must be positive")
    result = run(
        (
            "with-proxy",
            "gh",
            "run",
            "view",
            run_id,
            "-R",
            repo,
            "--json",
            "databaseId,url,status,conclusion",
        ),
        cwd=ROOT,
    )
    payload = _json_object(result.stdout.strip())
    if result.returncode != 0 or payload is None:
        detail = (result.stderr or result.stdout or "run did not resolve").strip()
        return Evidence("unverifiable", "run", run_id, reason=detail)
    if str(payload.get("databaseId") or "") != run_id:
        return Evidence("unverifiable", "run", run_id, reason="GitHub returned another run")
    url = str(payload.get("url") or "")
    if not url:
        return Evidence("unverifiable", "run", run_id, reason="run has no URL")
    return Evidence("verified", "run", run_id, resolved=url)


def close_task(task: str, evidence: Evidence, *, run: Run = _run) -> int:
    if evidence.state != "verified" or not evidence.resolved:
        return evidence.rc
    if not task_has_implemented_tag(task, run=run):
        print(
            "REFUSED closing a task that is not tagged `implemented`: "
            f"{task}. Publication evidence and the tag must be recorded before "
            "the owning agent shepherds the task to landing and closes it.",
            file=sys.stderr,
        )
        return REFUSED
    note = (
        "CLOSURE-VERIFIED: "
        f"kind={evidence.kind} reference={evidence.reference} "
        f"resolved={evidence.resolved} "
        # State the landing fact explicitly. Status is a record of verification,
        # never the source of the landing fact.
        f"landing={evidence.landing or 'n/a'} verifier=ci-hub/bin/close-task"
    )
    noted = run(("tg", "note", task, note), cwd=ROOT)
    if noted.returncode != 0:
        print(
            "UNVERIFIABLE "
            f"task={task} reason=failed-to-record-reference detail="
            f"{(noted.stderr or noted.stdout).strip()}",
            file=sys.stderr,
        )
        return UNVERIFIABLE
    closed = run(("tg", "update", task, "--status", "closed"), cwd=ROOT)
    if closed.returncode != 0:
        print(
            "UNVERIFIABLE "
            f"task={task} reason=task-update-failed detail="
            f"{(closed.stderr or closed.stdout).strip()}",
            file=sys.stderr,
        )
        return UNVERIFIABLE
    print(
        f"CLOSED task={task} kind={evidence.kind} "
        f"reference={evidence.reference} resolved={evidence.resolved} rc=0"
    )
    return CLOSED


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Verify and record a durable reference before closing a task"
    )
    parser.add_argument("task")
    reference = parser.add_mutually_exclusive_group(required=True)
    reference.add_argument("--code", metavar="PR_OR_SHA")
    reference.add_argument("--artifact", metavar="PATH_OR_URL")
    reference.add_argument("--run-id")
    # Sentinel defaults so main() can tell "the caller chose hermit" from "the
    # caller said nothing and got hermit" -- the distinction that let a
    # parent-repo task close against a hermit PR.
    parser.add_argument("--repo", default=None)
    parser.add_argument("--source", type=Path, default=None)
    parser.add_argument("--target", default="main")
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="verify without recording or closing (safe for live tasks)",
    )
    return parser.parse_args(argv)


DEFAULT_REPO = "rrnewton/hermit"


def main(argv: Sequence[str] | None = None, *, run: Run = _run) -> int:
    args = parse_args(argv)
    repo_was_explicit = args.repo is not None
    repo = args.repo or DEFAULT_REPO
    source = args.source if args.source is not None else ROOT / "hermit"
    if args.code is not None:
        # A bare PR number is not self-identifying: every repository has a #56.
        # A 40-hex SHA is -- the verifier can only resolve it where it exists.
        # So the dangerous combination is exactly `--code <N>` with a DEFAULTED
        # repo, which silently means hermit. That is how
        # `execute-ambiguous-zero-fix-order-a3-a4-first`, a parent-repo task
        # about compat-envelope/render-scorecard.rs, was closed against
        # hermit's "docs: add Hermit error catalog (#56)" from three weeks
        # earlier. The ancestry check was real; nothing bound the REPOSITORY to
        # the task's subject.
        if args.code.isdecimal() and not repo_was_explicit:
            print(
                f"REFUSED task={args.task} kind=code reference={args.code} "
                f"reason=a bare PR number needs an explicit --repo: every repository "
                f"has a #{args.code}, so defaulting to {DEFAULT_REPO} would verify a "
                f"landing that may have nothing to do with this task. Pass "
                f"--repo <owner/repo> --source <checkout>, or give the 40-hex SHA. "
                f"rc={REFUSED}",
                file=sys.stderr,
            )
            return REFUSED
        evidence = verify_code(
            args.code,
            repo=repo,
            source=source,
            target=args.target,
            run=run,
        )
    elif args.artifact is not None:
        evidence = verify_artifact(args.artifact, run=run)
    else:
        evidence = verify_run(args.run_id, repo=repo, run=run)

    if evidence.state != "verified":
        print(
            f"{evidence.state.upper()} task={args.task} kind={evidence.kind} "
            f"reference={evidence.reference} reason={evidence.reason or evidence.state} "
            f"rc={evidence.rc}",
            file=sys.stderr,
        )
        return evidence.rc
    if args.check_only:
        print(
            f"VERIFIED task={args.task} kind={evidence.kind} "
            f"reference={evidence.reference} resolved={evidence.resolved} rc=0"
        )
        return CLOSED
    return close_task(args.task, evidence, run=run)


if __name__ == "__main__":
    raise SystemExit(main())
