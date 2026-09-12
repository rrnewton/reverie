#!/usr/bin/env python3
"""Close a TaskGraph task only after its recorded reference verifies."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
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
    # Which verified landing state the code evidence carried: "landed" (the
    # commit is an ancestor of freshly-fetched target main) or
    # "implemented-unlanded" (the PR/commit exists and resolved, but has not
    # merged yet). Both close. Recorded in the closure note so a reader can
    # tell them apart afterwards -- "closed" alone must never be readable as
    # "landed".
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
    if command and (command[0] == "tg" or str(command[0]).endswith("tg-note-verified")):
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
            # Mark this child as coming FROM the verifier. When the optional `tg`
            # PATH shim is installed and selected, it refuses direct closure for
            # callers without this marker. The marker does not prove deployment,
            # and an explicit `/usr/local/bin/tg` invocation bypasses the shim.
            # Set it here, at the single helper every `tg` call in this module goes
            # through, so a future call site cannot lock the verifier out of its
            # own closure step -- the exact regression the `tg note` guard caused
            # when it first shipped.
            env["CLOSE_TASK_VERIFIED"] = "1"
            # The closure note is staged through tempfile.NamedTemporaryFile, so
            # its path is random and created O_EXCL -- no other agent can have
            # written it. tg-note-verified otherwise refuses unnamespaced paths in
            # /tmp, and on 2026-08-25 that composition deadlocked the fleet: both
            # guards were individually correct and together NO task could be closed
            # by anyone. Declaring the file's provenance is what resolves it; the
            # attribution check still applies to this note like any other.
            env["TG_NOTE_PRIVATE_TEMP"] = "1"
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


_TASK_ID_OK = re.compile(r"\A[A-Za-z0-9_.-]+\Z")


def read_task_tags(task: str, *, run: Run = _run) -> tuple[str, ...] | None:
    """The task's exact tag list, or None if it could not be read.

    ⚠️ READ THE DATABASE, NOT `tg show`. Both tag gates in this module used to
    scan `tg show` output for a line starting with "tags:". `tg show` prints a
    mission-view TREE and has no such line, so the scan never matched and both
    gates silently answered "no tags" for every task alive -- one of them
    over-refusing, the other (the `--disposition` gate) failing OPEN, which is
    the direction that lets the guarded thing through. Measured 2026-08-25 by
    the gate declining to fire on a task tagged `implemented`.

    `tg sql` is read-only and returns the stored column, so it cannot drift from
    what the tag queries themselves select on.
    """
    if not _TASK_ID_OK.match(task):
        return None
    result = run(
        ("tg", "sql", f"select tags from tasks where local_id='{task}'"), cwd=ROOT
    )
    if result.returncode != 0:
        return None
    rows = [
        line.strip()
        for line in result.stdout.splitlines()
        if line.strip()
        and not line.startswith("tags")
        and not set(line.strip()) <= {"-"}
        and not re.match(r"\A\(\d+ rows?\)\Z", line.strip())
        and "jemalloc" not in line
    ]
    if not rows:
        return None
    try:
        value = json.loads(rows[0])
    except json.JSONDecodeError:
        return None
    if not isinstance(value, list) or any(not isinstance(tag, str) for tag in value):
        return None
    return tuple(value)


def task_has_implemented_tag(task: str, *, run: Run = _run) -> bool:
    """Is `implemented` on the task right now?

    Gate for closing an unlanded task. Landing pendency is tracked by the
    `drain-implemented-to-landed` query, which selects on this tag -- so
    closing an unlanded task WITHOUT it would drop the work out of every view
    at once: not ready, not active, and not in the drain. Requiring the tag is
    what keeps "closed but not yet landed" discoverable rather than lost.
    """
    tags = read_task_tags(task, run=run)
    if tags is None:
        return False
    return "implemented" in {tag.lower() for tag in tags}


def task_claims_landing(task: str, *, run: Run = _run) -> bool:
    """Does the task assert, by tag, that code exists or shipped?

    `--disposition` closes a task with no landing to verify. A task tagged
    `implemented` or `landed` is asserting the opposite, so letting it close by
    disposition would let the one population this verifier exists for opt out of
    it in a single flag. Read the tags and refuse instead.
    """
    tags = read_task_tags(task, run=run)
    if tags is None:
        # Fail CLOSED for this check: unreadable tags must not be read as
        # "claims nothing", which would silently permit the disposition.
        return True
    lowered = {tag.lower() for tag in tags}
    return "implemented" in lowered or "landed" in lowered


def tags_with_landed(tags: Sequence[str]) -> tuple[str, ...] | None:
    """Return the exact replacement tag list for a verified landed closure.

    `tg update --tags` replaces the whole list, so preserving every existing tag
    is part of the write. An unreadable or unrepresentable list refuses before
    the closure note is written; guessing here would turn an audit repair into
    silent metadata loss.
    """
    if any(not tag or "," in tag or "\n" in tag or "\r" in tag for tag in tags):
        return None
    if "landed" in tags:
        return tags
    return (*tags, "landed")


def count_exact_closure_records(
    task: str, note: str, *, run: Run = _run
) -> int | None:
    """Count byte-identical closure records for ``task``, or fail unreadable.

    ``close_task`` records its note and status through separate TaskGraph
    commands.  If the note succeeds and the status update is interrupted, the
    supported recovery is to rerun the same close command.  Matching the whole
    UTF-8 note makes that retry idempotent without treating a different closure
    record as authority for this one.

    The content is encoded as hexadecimal before it enters SQL.  Task ids use
    the same restricted alphabet as ``read_task_tags``, so neither value needs
    SQL quoting or escaping.
    """
    if not _TASK_ID_OK.match(task):
        return None
    note_hex = note.encode("utf-8").hex().upper()
    result = run(
        (
            "tg",
            "sql",
            "select json_object('exact_note_count', count(*)) "
            "from task_notes "
            f"where task_id='{task}' and hex(content)='{note_hex}'",
        ),
        cwd=ROOT,
    )
    if result.returncode != 0:
        return None
    try:
        rows = taskgraph_db.parse_json_rows(result.stdout)
    except taskgraph_db.TaskGraphUnavailable:
        return None
    if len(rows) != 1:
        return None
    count = rows[0].get("exact_note_count")
    if not isinstance(count, int) or isinstance(count, bool) or count < 0:
        return None
    return count


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
    require_content: bool = True,
) -> Evidence:
    """Verify `reference` landed on `target`.

    `require_content=False` keeps the ancestry proof and SKIPS the content-presence
    check. That is only correct for `--superseded-by`, where the caller's claim is
    precisely that the content was REPLACED: demanding its survival there would
    refuse every fix-forward, which is the bind this option exists to remove. The
    ancestry check is NOT relaxed, so a superseding commit that never landed still
    cannot close anything.
    """
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
    if result.returncode == CLOSED and state == "landed" and resolved and not require_content:
        # Ancestry only: the caller is asserting the content was superseded.
        return Evidence(
            "verified",
            "superseded",
            reference,
            resolved=f"{repo}@{resolved}",
            landing="superseded",
        )
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
        if presence.verdict == content_presence.CONTEXT_DRIFTED:
            # Some hunk failed to reverse-apply, but every line those hunks ADD was
            # confirmed present at the target as a unique contiguous run. Later commits
            # moved the surrounding context; the content is there.
            #
            # THE REFUSAL ABOVE IS UNCHANGED. CONTENT-LOST still fires whenever any hunk's
            # added content cannot be confirmed, so nothing that used to refuse stops
            # refusing; this branch is only reachable when that check has already been
            # satisfied for every failing hunk. Measured case: e3bd3a89 refused at 3 of 4
            # hunks on 2026-08-15 where the entire "missing" hunk was `+import shutil`,
            # present on main, displaced by ac181ae8 adding `import os` above it.
            #
            # RECORDED, NOT SILENT. A closure that passed through a drift must not read
            # identically to one where every hunk matched cleanly.
            return Evidence(
                "verified",
                "code",
                reference,
                resolved=f"{repo}@{resolved}",
                landing="landed",
                reason=(
                    f"CONTEXT-DRIFTED: {len(presence.drifted)} of "
                    f"{presence.hunks_total} hunk(s) no longer reverse-apply at {target} "
                    "because later commits moved their surrounding context; every added "
                    "line was confirmed present. Drifted: "
                    f"{'; '.join(presence.drifted[:3])}"
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
        # Parent tooling has no implemented-but-unlanded phase: the standing
        # publication path is a serialized direct commit to parent main. Treating
        # a real but non-ancestral parent object like an open product PR would
        # recreate the gateway bypass this evidence mode exists to remove.
        if repo == PARENT_REPO:
            return Evidence(
                "refused",
                "code",
                reference,
                resolved=f"{repo}@{resolved}" if resolved else None,
                reason=(
                    f"parent code is not landed on {target}: {reason}; direct-to-main "
                    "parent work must be an ancestor before closure"
                ),
            )
        # NOT-LANDED IS A VERIFIED STATE, NOT A FAILURE (2026-08-06). The
        # verifier dereferenced the reference and resolved a real commit; it
        # simply is not on target main yet. Under the close-on-implemented
        # rule that is a complete implementation, so it closes -- with the
        # landing state recorded, and only when the caller still needs no
        # further work. Note what is NOT relaxed: a reference that does not
        # resolve at all never reaches here, and still refuses below.
        if resolved:
            return Evidence(
                "verified",
                "code",
                reference,
                resolved=f"{repo}@{resolved}",
                reason=reason,
                landing="implemented-unlanded",
            )
        return Evidence("refused", "code", reference, resolved=resolved, reason=reason)
    return Evidence("unverifiable", "code", reference, resolved=resolved, reason=reason)


# A GitHub comment permalink carries its whole payload in the FRAGMENT, and a
# fragment is client-side: it is never sent to the server. So `curl --fail` on
# .../pull/2623#issuecomment-<id> fetches the PULL REQUEST PAGE and returns 200
# whatever the id is. Measured 2026-08-25 against one real id, one typo'd id and
# the literal 99999999999 -- all three returned 200.
#
# ⚠️ THAT IS A CHECK THAT CANNOT FAIL FOR THE REASON IT NAMES. It confirms the PR
# exists, which nobody doubted, while reporting that the COMMENT was verified. A
# closure citing a comment that never existed, one since deleted, or one with a
# transposed digit all recorded CLOSURE-VERIFIED. I did exactly that an hour
# before writing this, and the byte-for-byte note verifier happily stored the
# dead link because it verifies storage, not truth.
_COMMENT_PERMALINK = re.compile(
    r"^https?://github\.com/(?P<owner>[\w.-]+)/(?P<repo>[\w.-]+)/pull/(?P<pr>\d+)"
    r"#(?P<kind>issuecomment|pullrequestreview|discussion_r)-?(?P<id>\d+)$",
    re.IGNORECASE,
)


def _comment_endpoint(m: "re.Match[str]") -> str:
    """The API path that can actually answer whether this anchor exists."""
    slug = f"{m.group('owner')}/{m.group('repo')}"
    kind = m.group("kind").lower()
    ident = m.group("id")
    if kind == "issuecomment":
        return f"repos/{slug}/issues/comments/{ident}"
    if kind == "pullrequestreview":
        return f"repos/{slug}/pulls/{m.group('pr')}/reviews/{ident}"
    return f"repos/{slug}/pulls/comments/{ident}"


def verify_comment_permalink(reference: str, *, run: Run = _run) -> Evidence | None:
    """Resolve a comment anchor via the API. None if this is not one.

    THREE OUTCOMES, AND THE THIRD IS NOT A PASS:
      the comment resolves          -> verified
      the API says 404              -> REFUSED. The link points at nothing, and a
                                       citation that points at nothing is the
                                       absence of evidence, not a weaker form of
                                       it.
      the API could not be asked    -> unverifiable, i.e. could-not-determine.
                                       Distinct from 404 on purpose: "no such
                                       comment" and "I could not look" demand
                                       opposite reactions, and collapsing them is
                                       the defect this module exists to remove.
    """
    m = _COMMENT_PERMALINK.match(reference.strip())
    if m is None:
        return None
    result = run(("with-proxy", "gh", "api", _comment_endpoint(m), "--jq", ".id"), cwd=ROOT)
    if result.returncode == 0 and (result.stdout or "").strip():
        return Evidence("verified", "artifact", reference,
                        resolved=f"{_comment_endpoint(m)} -> {(result.stdout or '').strip()}")
    blob = ((result.stderr or "") + (result.stdout or "")).lower()
    if "not found" in blob or "404" in blob:
        return Evidence(
            "refused", "artifact", reference,
            reason=("the cited comment does not exist. `curl` cannot see this because a "
                    "URL fragment is never sent to the server. Remedy: post the verdict "
                    "and cite the id the API returns."),
        )
    return Evidence(
        "unverifiable", "artifact", reference,
        reason=("could not ask GitHub whether the cited comment exists: "
                + ((result.stderr or "").strip()[:120] or "no detail")
                + ". This is NOT a pass; retry when the API is reachable."),
    )


def github_object_endpoint(url: str) -> str | None:
    """The authenticated API path for a github.com web URL that is not a comment.

    ⚠️ WHY: THE GENERAL URL BRANCH BELOW USES ANONYMOUS `curl`, SO NO ARTIFACT IN
    A PRIVATE REPOSITORY CAN BE RECORDED AS EVIDENCE. GitHub answers an
    unauthenticated request for a private object with 404, `curl --fail` turns
    that into a nonzero exit, and the closure is reported `unverifiable` with a
    reason naming the URL rather than the missing credential. Measured
    2026-08-26: `rrnewton/dev-hermit` is PRIVATE and anonymous
    `curl --fail https://github.com/rrnewton/dev-hermit` exits 22, while
    `gh api repos/rrnewton/dev-hermit` exits 0. `rrnewton/hermit` and
    `rrnewton/reverie` are public and were unaffected -- which is why this
    survived: total failure in the parent, invisible from the repositories most
    closures cite.

    `verify_comment_permalink` already does this for comment permalinks, and is
    deliberately left alone; this is the same reasoning applied to the shapes it
    does not cover. Unknown shapes return None and fall through to `curl`, which
    is correct for non-GitHub hosts and no worse than today for anything else:
    mapping fewer shapes is safe, mapping one WRONGLY would be a false verdict.
    """
    m = re.match(
        r"https?://(?:www\.)?github\.com/([^/#?]+)/([^/#?]+)(/[^#?]*)?(?:\?[^#]*)?$",
        url,
    )
    if m is None:
        return None
    owner, repo, rest = m.group(1), m.group(2).removesuffix(".git"), m.group(3) or ""
    parts = [p for p in rest.split("/") if p]
    if not parts:
        return f"repos/{owner}/{repo}"
    if len(parts) >= 2 and parts[0] in {"pull", "issues"} and parts[1].isdigit():
        return f"repos/{owner}/{repo}/{'pulls' if parts[0] == 'pull' else 'issues'}/{parts[1]}"
    if len(parts) >= 2 and parts[0] == "commit":
        return f"repos/{owner}/{repo}/commits/{parts[1]}"
    return None


def verify_artifact(reference: str, *, run: Run = _run) -> Evidence:
    anchored = verify_comment_permalink(reference, run=run)
    if anchored is not None:
        return anchored
    if reference.startswith(("https://", "http://")):
        endpoint = github_object_endpoint(reference)
        if endpoint is not None:
            # AUTHENTICATED, so a private repository is reachable at all.
            probed = run(("with-proxy", "gh", "api", endpoint), cwd=ROOT)
            if probed.returncode == 0:
                return Evidence("verified", "artifact", reference, resolved=reference)
            # ⚠️ NO ANONYMOUS FALLBACK. A private object the authenticated probe
            # could not see is exactly the case `curl` answers WRONGLY: it 404s,
            # and that 404 reads as absence rather than as a missing credential.
            return Evidence(
                "unverifiable",
                "artifact",
                reference,
                reason=f"gh api {endpoint}: {(probed.stderr or 'did not resolve').strip()[:200]}",
            )
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


def close_task(
    task: str, evidence: Evidence, *, run: Run = _run, dry_run: bool = False
) -> int:
    """Close `task` against `evidence`, or with `dry_run` predict whether it would.

    ⚠️ `--check-only` RUNS THIS SAME FUNCTION, AND THAT IS THE POINT.
    It used to return early in main(), which applied the reference checks but
    NOT the tag gates below, so it printed `VERIFIED ... rc=0` for an operation
    that was then REFUSED. Measured 2026-08-25 by agent(hermit-006) on
    land_hermit_pr_2591: identical commands, opposite answers, differing only in
    the flag whose whole purpose is to predict the other.

    A dry run that answers a NARROWER question than the run it models is worse
    than no dry run: an agent uses it to decide whether closing is safe and it
    manufactures confidence. Sharing the function makes divergence structural
    rather than a thing to remember -- every gate added here is automatically in
    the preview, which is the property that failed when the disposition gate was
    added to this function alone.
    """
    landed_tags: tuple[str, ...] | None = None
    add_landed_tag = False
    if evidence.state != "verified" or not evidence.resolved:
        return evidence.rc
    if evidence.kind == "disposition" and task_claims_landing(task, run=run):
        print(
            f"REFUSED closing by --disposition a task tagged `implemented` or "
            f"`landed`: {task}. Those tags assert that code exists, so this "
            f"closure has a reference to give and must give it: pass --code "
            f"<PR-or-40-hex> --repo <owner/repo> --source <checkout>. If the tag "
            f"is wrong, drop it and say so in a note, then close by disposition.",
            file=sys.stderr,
        )
        return REFUSED
    if evidence.landing == "implemented-unlanded" and not task_has_implemented_tag(
        task, run=run
    ):
        print(
            "REFUSED closing an unlanded task that is not tagged `implemented`: "
            f"{task}. The drain query `drain-implemented-to-landed` selects on "
            "that tag, so closing without it would remove the work from ready, "
            "active, AND the drain in one step. Add the tag, then close.",
            file=sys.stderr,
        )
        return REFUSED
    if evidence.landing == "landed":
        current_tags = read_task_tags(task, run=run)
        if current_tags is None:
            print(
                "UNVERIFIABLE "
                f"task={task} reason=could-not-confirm-current-tags-before-landed-close; "
                "the closure note and task status were not changed",
                file=sys.stderr,
            )
            return UNVERIFIABLE
        landed_tags = tags_with_landed(current_tags)
        if landed_tags is None:
            print(
                "UNVERIFIABLE "
                f"task={task} reason=could-not-preserve-tags-before-landed-close; "
                "the closure note and task status were not changed",
                file=sys.stderr,
            )
            return UNVERIFIABLE
        add_landed_tag = "landed" not in current_tags
    # ⚠️ A DISPOSITION IS RECORDED, NOT VERIFIED, AND THE ANCHOR MUST SAY SO.
    # CLOSURE-VERIFIED is the evidence from which the queryable `landed` tag is
    # derived below. Writing it for a closure where nothing was resolved would
    # make the audit index rise for precisely the closures carrying no evidence.
    # Distinct anchors keep verified landings and recorded dispositions separately
    # countable.
    anchor = "CLOSURE-RECORDED" if evidence.kind == "disposition" else "CLOSURE-VERIFIED"
    note = (
        f"{anchor}: "
        f"kind={evidence.kind} reference={evidence.reference} "
        f"resolved={evidence.resolved} "
        # State the landing fact explicitly. `closed` no longer implies landed,
        # so a closure note that omits this would be read as a landing claim it
        # cannot support.
        f"landing={evidence.landing or 'n/a'} verifier=ci-hub/bin/close-task"
        # Carry any qualifier the verification attached. A CONTEXT-DRIFTED closure is
        # verified but not clean, and a note that omitted the qualifier would be read
        # afterwards as a clean match nobody actually observed. Appended, so the 40-hex
        # that ci-hub/directives/tg_landed.py regexes out is unaffected.
        + (f" detail={evidence.reason}" if evidence.reason else "")
    )
    existing_records = count_exact_closure_records(task, note, run=run)
    if existing_records is None:
        print(
            "UNVERIFIABLE "
            f"task={task} reason=could-not-check-existing-closure-record; "
            "the closure note, tags, and task status were not changed",
            file=sys.stderr,
        )
        return UNVERIFIABLE
    # THE DRY RUN STOPS HERE -- after every gate, before the first mutation. Any
    # check added above is in the preview by construction; anything below is a
    # write and is the only thing --check-only skips.
    if dry_run:
        tag_detail = " would-add-tag=landed" if add_landed_tag else ""
        record_detail = (
            f"would-reuse={anchor} existing-records={existing_records}"
            if existing_records
            else f"would-record={anchor}"
        )
        print(
            f"VERIFIED task={task} kind={evidence.kind} "
            f"reference={evidence.reference} resolved={evidence.resolved} "
            f"{record_detail}{tag_detail} rc=0"
        )
        return CLOSED
    # ⚠️ THE CLOSURE NOTE GOES THROUGH THE VERIFIED WRITER, NOT `tg note`.
    #
    # This is the single most load-bearing note in the system -- it is the only
    # producer of CLOSURE-VERIFIED, and `ci-hub/directives/tg_landed.py` regexes
    # the 40-hex out of it. A note that was stored differently from what was
    # composed would discharge landing debt against the wrong evidence.
    # `tg-note-verified` reads the row back out of `task_notes` and compares it
    # byte-for-byte before reporting success, so the confirmation reflects the
    # store rather than the call.
    #
    # This call was never shell-unsafe -- subprocess with an argv LIST does not
    # invoke a shell, so nothing here could be substituted. But the PATH guard
    # that enforces the safe path cannot distinguish a safe argv from a mangled
    # one (that is precisely why the mangling is undetectable at that layer), so
    # it refuses the form. Routing through the verified writer satisfies the
    # guard AND buys the read-back proof, which is strictly better than an
    # exemption would have been.
    #
    # An exact record can already exist when a preceding invocation stored and
    # verified it but lost the separate status write to SQLite contention. In
    # that state, reuse this record and continue with the missing writes. A
    # different record never satisfies this check.
    if existing_records == 0:
        with tempfile.NamedTemporaryFile(
            "w", suffix=".note", delete=False, encoding="utf-8"
        ) as handle:
            handle.write(note)
            note_path = handle.name
        try:
            noted = run(
                (str(ROOT / "ci-hub" / "bin" / "tg-note-verified"), task, "--file", note_path),
                cwd=ROOT,
            )
        finally:
            try:
                os.unlink(note_path)
            except OSError:
                pass
        if noted.returncode != 0:
            print(
                "UNVERIFIABLE "
                f"task={task} reason=failed-to-record-reference detail="
                f"{(noted.stderr or noted.stdout).strip()}",
                file=sys.stderr,
            )
            return UNVERIFIABLE
    # DERIVED, NOT A SECOND MANUAL FACT. The verified marker is durable before
    # its query index is written. A failed or unconfirmed tag update leaves the
    # task open, so no closed `landing=landed` task can silently omit `landed`.
    if add_landed_tag:
        assert landed_tags is not None
        tagged = run(
            ("tg", "update", task, "--tags", ",".join(landed_tags)), cwd=ROOT
        )
        if tagged.returncode != 0:
            print(
                "UNVERIFIABLE "
                f"task={task} reason=failed-to-record-landed-tag detail="
                f"{(tagged.stderr or tagged.stdout).strip()}; task remains open",
                file=sys.stderr,
            )
            return UNVERIFIABLE
        recorded_tags = read_task_tags(task, run=run)
        if recorded_tags is None or not set(landed_tags).issubset(recorded_tags):
            print(
                "UNVERIFIABLE "
                f"task={task} reason=landed-tag-readback-mismatch "
                f"expected={json.dumps(landed_tags)} actual="
                f"{json.dumps(recorded_tags)}; task remains open",
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
    recovery_detail = " closure-record=reused" if existing_records else ""
    print(
        f"CLOSED task={task} kind={evidence.kind} "
        f"reference={evidence.reference} resolved={evidence.resolved}"
        f"{recovery_detail} rc=0"
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
    # ⚠️ THE NON-LANDING CLOSURE, AND WHY IT HAD TO EXIST BEFORE ENFORCEMENT.
    # Measured 2026-08-25: of 477 tasks closed that day without a CLOSURE-VERIFIED
    # note, 380 DID name a PR or 40-hex in their notes -- they could have come
    # through this verifier and did not. But 97 named no reference at all: review
    # lanes, coordination, duplicates, questions answered. Those have no landing to
    # verify, so a closure path that only accepts --code/--artifact/--run-id is
    # unusable for a fifth of real closures, and a guard built on it would simply be
    # routed around. This option is what makes "every closure goes through the
    # verifier" a rule that can actually be followed.
    #
    # It records under a DIFFERENT anchor (CLOSURE-RECORDED, not CLOSURE-VERIFIED)
    # so it cannot inflate the landing-evidence metric, and close_task() REFUSES it
    # on any task tagged `implemented` or `landed` -- exactly the population that
    # must supply a reference. So it is an escape hatch for work that has no
    # landing, not an escape hatch from proving one.
    reference.add_argument("--disposition", metavar="REASON")
    # ⚠️ LANDED, THEN LEGITIMATELY SUPERSEDED -- the outcome every fix-forward
    # produces, and until now unclosable without misrepresenting something.
    #
    # Measured 2026-08-25 on land_hermit_pr_2591: the work landed as a5fef7ff and
    # IS an ancestor of main, but a later fix-forward rewrote the section, so
    #   --code a5fef7ff   -> REFUSED, CONTENT-LOST: 0 of 1 hunks present
    #   --disposition ... -> REFUSED, the task is tagged `landed`
    # and the tool's own advice -- drop the tag, then use --disposition -- would
    # have meant recording a FALSEHOOD to satisfy a checker, since a5fef7ff really
    # is on main. agent(hermit-006) left the row open rather than do that, which was
    # the right call and is exactly the cost this option removes.
    #
    # The content check is deliberately NOT applied to the superseded commit: the
    # whole claim is that the content was replaced. What IS verified is that the
    # SUPERSEDING commit resolves and is an ancestor of freshly-fetched main -- so
    # "superseded" cannot be used to close against a replacement that never landed.
    reference.add_argument("--superseded-by", metavar="PR_OR_SHA")
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
    elif args.disposition is not None:
        reason = args.disposition.strip()
        if not reason:
            print(
                f"REFUSED task={args.task} kind=disposition "
                f"reason=an empty disposition records nothing; say why this task is "
                f"closing rc={REFUSED}",
                file=sys.stderr,
            )
            return REFUSED
        # Nothing is verified here and the evidence says so: `landing="none"`.
        # close_task() branches on kind=="disposition" to write CLOSURE-RECORDED
        # rather than CLOSURE-VERIFIED, keeping the landing-evidence metric honest.
        evidence = Evidence(
            state="verified",
            kind="disposition",
            reference=reason,
            resolved="no-code-reference",
            landing="none",
        )
    elif args.superseded_by is not None:
        if args.superseded_by.isdecimal() and not repo_was_explicit:
            print(
                f"REFUSED task={args.task} kind=superseded "
                f"reference={args.superseded_by} reason=a bare PR number needs an "
                f"explicit --repo rc={REFUSED}",
                file=sys.stderr,
            )
            return REFUSED
        evidence = verify_code(
            args.superseded_by,
            repo=repo,
            source=source,
            target=args.target,
            run=run,
            require_content=False,
        )
    else:
        evidence = verify_run(args.run_id, repo=repo, run=run)

    if evidence.state != "verified":
        print(
            f"{evidence.state.upper()} task={args.task} kind={evidence.kind} "
            f"reference={evidence.reference} reason={evidence.reason or evidence.state} "
            f"rc={evidence.rc}",
            file=sys.stderr,
        )
        if evidence.kind == "artifact":
            reason = evidence.reason or ""
            if "outside the versioned workspace" in reason:
                action = (
                    "pass a URL, or pass a file inside the versioned dev-hermit "
                    "workspace, then rerun"
                )
            elif "not a file" in reason:
                action = "pass a tracked file instead of a directory, then rerun"
            elif evidence.reference.startswith(("http://", "https://")):
                action = (
                    "correct the URL or make it readable, then rerun; if this task "
                    "produced no artifact, use --disposition with the reason"
                )
            else:
                action = (
                    "commit and push that artifact to parent main, then rerun; if "
                    "this task produced no artifact, use --disposition with the reason"
                )
        elif evidence.kind in {"code", "superseded"}:
            action = (
                "land the referenced change or correct --repo and --source, then rerun; "
                "if no code was produced, use --disposition with the reason"
            )
        else:
            action = (
                "pass a GitHub Actions run id that exists in --repo and can be read, "
                "then rerun"
            )
        print(f"ACTION: {action}.", file=sys.stderr)
        return evidence.rc
    # ONE PATH. --check-only is a parameter of the real operation, not a separate
    # early return that reimplements part of it -- see close_task()'s docstring for
    # the measured divergence that produced this.
    return close_task(args.task, evidence, run=run, dry_run=args.check_only)


if __name__ == "__main__":
    raise SystemExit(main())
