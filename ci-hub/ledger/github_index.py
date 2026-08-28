#!/usr/bin/env python3
"""GitHub commit comments as a per-SHA index over validation receipts.

WHAT THIS IS, AND MORE IMPORTANTLY WHAT IT IS NOT. This publishes a compact,
GitHub-visible pointer for each validation run so green/timeline/bisect tools can
ask "what happened at this exact SHA?" without a local ledger. It is a CACHE. The
immutable receipt remains the authority, and :func:`verify_receipt` refuses any
comment it cannot bind to a receipt it dereferenced and hashed. A forged, stale,
or relocated comment is inert rather than believed.

WHY COMMENTS AND NOT STATUSES OR CHECKS. Decided by measurement, not preference,
in ``ai_docs/2026-08-07-github-validation-index-mechanism-decision.md``:

  * combined-status COLLAPSES history -- three runs from two host classes went in
    and the endpoint most consumers read reported one. Disqualifying for a
    mechanism whose whole purpose is a history.
  * statuses CANNOT be rolled back: there is no delete endpoint, so a bad
    publisher run leaves permanent residue on real commits.
  * a status description caps at 140 characters and one index event serialises to
    ~329, so a status-backed offline cache is pointers only; a comment body caps
    at 65,535.
  * checks cannot run at all on a classic PAT (403, "must authenticate via a
    GitHub App").

The honest cost, recorded rather than buried: comments need the broad ``repo``
scope where statuses need only ``repo:status``. That is a real least-privilege
loss, accepted because history preservation is the point of the feature.

THE PUBLISHED PAYLOAD IS A LEDGER EVENT -- the same ``validate-ledger/v1``
envelope the team/machine shards use, not a parallel schema. That is deliberate:
one envelope means one verifier (:func:`ledger.validate_event`) judges what may
be written to a shard AND what may be published to GitHub, and it means
:func:`ledger.union_events` / :func:`ledger.fold` work unmodified on events read
back from GitHub. A second schema here would be a second thing to keep in sync,
and the first divergence would be silent.

PRIVACY IS ENFORCED AT THE WRITER, FAIL-CLOSED. The prototype learned this the
expensive way: a read-side check correctly refused to *index* an FQDN that had
already been published. A reader cannot unpublish. So nothing reaches the network
until :func:`ledger.validate_event` returns clean.

NOT A LANDING GATE. Nothing here may be added to branch protection or read as
merge authority; consumers must dereference the receipt and run the canonical
verifier. Publishing is opt-in (``enabled=False`` by default) so importing this
module cannot cause a write.

API surface used, all under a classic ``repo`` scope, no GitHub App:
  POST   /repos/{repo}/commits/{sha}/comments   publish
  GET    /repos/{repo}/commits/{sha}/comments   read back (paginated)
  DELETE /repos/{repo}/comments/{id}            rollback
"""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import time
from collections.abc import Callable, Iterable, Sequence
from pathlib import Path
from typing import Any

from ledger import fold
from ledger import union_events
from ledger import validate_events
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402

#: Body prefix identifying our comments. Stable: readers filter on it, and
#: changing it orphans every comment already published.
MARKER = "[hermit-validate-index]"

#: Bumped only when the BODY FRAMING changes. The payload's own schema is the
#: ledger envelope version, so this never duplicates that number.
INDEX_FORMAT = "hermit/validate-index/v1"

#: Measured on the live API, not assumed: 140 accepted / 141 rejected for a
#: status description; a comment body caps far higher. Kept as a guard so an
#: oversized body fails locally rather than as an opaque 422.
MAX_BODY = 65535

_FENCE = re.compile(r"```json\n(.*?)\n```", re.S)

_SHA_RE = re.compile(r"^[0-9a-f]{40}$")

# Use the same fleet-aware write boundary as interactive ``gh`` calls.  This
# publisher talks to the comments API directly, so relying only on ORC's PATH
# would leave daemon/systemd invocations able to impersonate the owner.
FLEET_GH = str(Path(__file__).resolve().parents[1] / "bin/gh")


class GitHubIndexError(RuntimeError):
    """Transport or protocol failure talking to GitHub."""


class PrivacyRefusal(RuntimeError):
    """Raised INSTEAD of publishing. See the module docstring: a read-side check
    cannot unpublish, so the guard has to fail closed before the POST."""


class PublishDisabled(RuntimeError):
    """Raised when a publish is attempted without opting in."""


# --------------------------------------------------------------------------
# Transport. Injected so every test runs offline; the default shells out to
# `gh` through the mandatory proxy.
# --------------------------------------------------------------------------


def gh_api(method: str, path: str, fields: dict[str, str] | None = None) -> tuple[int, Any]:
    """One ``gh api`` call. Returns ``(http_status, parsed_json_or_text)``.

    ``gh`` exits nonzero on HTTP error and prints the response, so the exit code
    alone cannot distinguish 403-rate-limited from 404-not-found; the status is
    recovered from the payload where possible and surfaced so the retry policy
    can tell a retryable condition from a permanent one.
    """
    cmd = ["with-proxy", FLEET_GH, "api", "-X", method, path]
    for key, value in (fields or {}).items():
        cmd += ["-f", f"{key}={value}"]
    if method == "GET":
        cmd.append("--paginate")
    proc = bounded_capture.capture(cmd)
    raw = proc.stdout or proc.stderr or ""
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        parsed = raw
    if proc.returncode == 0:
        return 200, parsed
    status = 0
    match = re.search(r"HTTP (\d{3})", raw)
    if match:
        status = int(match.group(1))
    return status, parsed


#: A transport is ``(method, path, fields) -> (status, payload)``.
Transport = Callable[[str, str, "dict[str, str] | None"], "tuple[int, Any]"]


# --------------------------------------------------------------------------
# Body framing
# --------------------------------------------------------------------------


def comment_body(event: dict, *, receipt: dict | None = None) -> str:
    """Render one ledger event as a comment body.

    THE EVENT IS EMBEDDED VERBATIM, and the receipt sits BESIDE it rather than
    inside it. That is not cosmetic. The receipt is a property of this index
    entry -- which artifact backs this publication -- not a field of the ledger
    event, and folding it in makes the published copy of an event differ from the
    shard copy of the same event. :func:`ledger.union_events` compares bodies for
    a repeated ``event_id`` and raises ``ConflictingEventId`` rather than picking
    a winner, so an in-event receipt makes merging GitHub with a local shard
    explode on every event the machine published itself. Caught by
    ``test_an_event_present_on_both_sides_collapses_to_one``.

    The human line exists so a person scrolling a commit sees what this is; the
    fenced JSON is what machines read. Both come from the same event, so they
    cannot drift.
    """
    payload = {"index_format": INDEX_FORMAT, "event": dict(event)}
    if receipt is not None:
        payload["receipt"] = receipt
    blob = json.dumps(payload, indent=2, sort_keys=True)
    outcome = event.get("outcome", "?")
    host = event.get("host", "?")
    body = (
        f"{MARKER} {INDEX_FORMAT}\n\n"
        f"validate `{outcome}` on `{host}` "
        f"(run `{event.get('run_id', '?')}`) -- this comment is an INDEX, not "
        f"authority; dereference the receipt and run the canonical verifier.\n\n"
        f"```json\n{blob}\n```\n"
    )
    if len(body) > MAX_BODY:
        raise GitHubIndexError(
            f"body is {len(body)} chars, over the {MAX_BODY} cap; "
            "shrink the event or split the publish"
        )
    return body


def parse_comment(
    comment: dict, commit_sha: str
) -> tuple[dict | None, dict | None, str]:
    """Extract ``(event, receipt, reason)`` from an UNTRUSTED comment body.

    Every refusal is a distinct predicate with its own reason so a negative test
    can assert WHICH one fired. A gate that refuses everything is exactly as
    broken as one that refuses nothing, and that failure is not hypothetical:
    the prototype's first mutation run had all five cases "refusing", including
    the positive control, because one wrong schema string made everything die at
    the same check.
    """
    body = comment.get("body") or ""
    if not body.startswith(MARKER):
        return None, None, "not-an-index-comment"
    match = _FENCE.search(body)
    if not match:
        return None, None, "no-json-block"
    try:
        payload = json.loads(match.group(1))
    except json.JSONDecodeError as error:
        return None, None, f"unparsable-json: {error}"
    if not isinstance(payload, dict):
        return None, None, "json-block-is-not-an-object"

    event = payload.get("event")
    if not isinstance(event, dict):
        return None, None, "no-event-object"
    receipt = payload.get("receipt") if isinstance(payload.get("receipt"), dict) else None

    codes = validate_events([event])[0]
    if codes:
        return None, None, codes[0]

    # WRONG-SHA BINDING. The event's own target must equal the commit the comment
    # is physically attached to. Without this, a perfectly valid index entry
    # copied onto another commit reads as evidence for that commit.
    target = event.get("commit")
    if target != commit_sha:
        return None, None, f"target-sha-mismatch: event {target} vs comment on {commit_sha}"

    return event, receipt, "ok"


# --------------------------------------------------------------------------
# Receipt binding -- the reason a comment is a cache and not evidence
# --------------------------------------------------------------------------


def verify_receipt(
    receipt: dict | None, opener: Callable[[str], bytes] | None
) -> tuple[bool, str]:
    """Believe the entry only if the receipt it names hashes to what it claims.

    ``opener`` dereferences the receipt reference to raw bytes. Passing ``None``
    means "no dereference available", which is a REFUSAL rather than a pass --
    an unverifiable claim is not a weaker true claim, it is no claim.
    """
    receipt = receipt or {}
    claimed = receipt.get("sha256")
    reference = receipt.get("ref")
    if not claimed:
        return False, "no-receipt-hash"
    if not reference:
        return False, "no-receipt-ref"
    if opener is None:
        return False, "receipt-not-retrievable"
    try:
        blob = opener(reference)
    except Exception as error:  # noqa: BLE001 - any failure is a refusal
        return False, f"receipt-unreadable: {error}"
    actual = hashlib.sha256(blob).hexdigest()
    if actual != claimed:
        return False, f"receipt-hash-mismatch: claims {claimed[:12]} actual {actual[:12]}"
    return True, "receipt-bound"


# --------------------------------------------------------------------------
# Read
# --------------------------------------------------------------------------


class IndexRead:
    """Result of reading one SHA's index, WITH its denominators.

    A bare list of accepted events is a proxy: it cannot distinguish "this commit
    has one validation" from "this commit has one validation and nine entries I
    threw away". Consumers need both numbers to know whether they are looking at
    a complete picture, so the counts travel with the value.
    """

    def __init__(
        self,
        commit: str,
        accepted: list[dict],
        rejected: list[tuple[Any, str]],
        *,
        comments_seen: int,
    ) -> None:
        self.commit = commit
        self.accepted = accepted
        self.rejected = rejected
        self.comments_seen = comments_seen

    @property
    def counts(self) -> dict[str, int]:
        return {
            "comments_seen": self.comments_seen,
            "accepted": len(self.accepted),
            "rejected": len(self.rejected),
        }

    def reasons(self) -> dict[str, int]:
        """Rejections bucketed by reason code, so a caller can tell an unrelated
        human comment from a tampered entry."""
        out: dict[str, int] = {}
        for _, reason in self.rejected:
            out[reason.split(":")[0]] = out.get(reason.split(":")[0], 0) + 1
        return out

    def runs(self) -> dict[str, dict]:
        """Fold the accepted events into run facts, exactly as the local ledger
        does. Enrichment published later lands on the run it enriches."""
        return fold(union_events(self.accepted))

    def __repr__(self) -> str:  # pragma: no cover - diagnostics only
        return f"<IndexRead {self.commit[:12]} {self.counts}>"


def fetch(
    commit: str,
    *,
    repo: str,
    transport: Transport = gh_api,
    opener: Callable[[str], bytes] | None = None,
    require_receipt: bool = True,
) -> IndexRead:
    """Read the index for an exact SHA. Needs nothing local but the SHA.

    That is the fresh-clone property: a machine that has never run validate can
    answer "what happened here?" from GitHub alone. ``require_receipt=False``
    exists only for inspecting what is published without dereferencing; it must
    never gate a decision.
    """
    if not _SHA_RE.match(commit):
        raise GitHubIndexError(f"not a 40-hex commit sha: {commit!r}")
    status, payload = transport("GET", f"/repos/{repo}/commits/{commit}/comments", None)
    if status != 200:
        raise GitHubIndexError(f"list failed: HTTP {status}: {payload}")
    comments = payload if isinstance(payload, list) else []

    accepted: list[dict] = []
    rejected: list[tuple[Any, str]] = []
    seen_ids: set[str] = set()
    for comment in comments:
        event, receipt, reason = parse_comment(comment, commit)
        if event is None:
            rejected.append((comment.get("id"), reason))
            continue
        if require_receipt:
            ok, why = verify_receipt(receipt, opener)
            if not ok:
                rejected.append((comment.get("id"), why))
                continue
        event_id = event["event_id"]
        if event_id in seen_ids:
            # A repeat is a duplicate publish, never an overwrite. Nothing is
            # edited in place, so the comment stream stays an audit trail.
            rejected.append((comment.get("id"), "duplicate-event-id"))
            continue
        seen_ids.add(event_id)
        accepted.append(event)
    return IndexRead(commit, accepted, rejected, comments_seen=len(comments))


# --------------------------------------------------------------------------
# Write
# --------------------------------------------------------------------------

#: Retryable HTTP conditions. 403 is included because GitHub returns it for
#: secondary rate limits, not only for authorization failures; the retry is
#: bounded so a genuine permission error still terminates.
_RETRYABLE = frozenset({403, 429, 500, 502, 503, 504})


def publish(
    events: Sequence[dict],
    *,
    commit: str,
    repo: str,
    receipt: dict | None = None,
    transport: Transport = gh_api,
    enabled: bool = False,
    max_attempts: int = 4,
    sleep: Callable[[float], None] = time.sleep,
    existing: IndexRead | None = None,
) -> dict:
    """Publish index entries for one commit. Opt-in, idempotent, fail-closed.

    Returns a record of what happened rather than a boolean, because "it
    published" is not a fact a caller can act on: it needs to know what was
    skipped as already-present and what was refused.
    """
    if not enabled:
        raise PublishDisabled(
            "publishing is opt-in; pass enabled=True (wired to --index-publish). "
            "Default-off means importing this module cannot write to GitHub."
        )
    if not _SHA_RE.match(commit):
        raise GitHubIndexError(f"not a 40-hex commit sha: {commit!r}")

    # WRITE-SIDE GATE. One verifier, the same one the shards use, so an event
    # that may not be written locally may not be published either.
    for event, codes in zip(events, validate_events(events)):
        if codes:
            raise PrivacyRefusal(
                f"refusing to publish event {event.get('event_id')}: {','.join(codes)}. "
                "A reader cannot unpublish, so this fails closed before the POST."
            )
        if event.get("commit") != commit:
            raise PrivacyRefusal(
                f"event {event.get('event_id')} targets {event.get('commit')} "
                f"but would be attached to {commit}; that mismatch is exactly what "
                "parse_comment refuses on read"
            )

    # IDEMPOTENCY. Read what is already there and skip those event_ids. A retry
    # after a partial failure must not double-publish, and event_id is the
    # identity the reader dedups on anyway.
    if existing is None:
        existing = fetch(
            commit, repo=repo, transport=transport, require_receipt=False
        )
    already = {event["event_id"] for event in existing.accepted}

    published: list[str] = []
    skipped: list[str] = []
    attempts_used = 0
    for event in events:
        if event["event_id"] in already:
            skipped.append(event["event_id"])
            continue
        body = comment_body(event, receipt=receipt)
        last: Any = None
        for attempt in range(1, max_attempts + 1):
            attempts_used += 1
            status, payload = transport(
                "POST", f"/repos/{repo}/commits/{commit}/comments", {"body": body}
            )
            if status in (200, 201):
                published.append(event["event_id"])
                break
            last = (status, payload)
            if status not in _RETRYABLE or attempt == max_attempts:
                raise GitHubIndexError(
                    f"publish of {event['event_id']} failed: HTTP {status}: {payload}"
                )
            # Exponential backoff. Bounded attempts mean a permanent 403 still
            # terminates instead of retrying forever against a bad token.
            sleep(2.0 ** (attempt - 1))
        else:  # pragma: no cover - loop always breaks or raises
            raise GitHubIndexError(f"publish exhausted attempts: {last}")

    return {
        "commit": commit,
        "published": published,
        "skipped_already_present": skipped,
        "attempts": attempts_used,
        "counts": {
            "offered": len(events),
            "published": len(published),
            "skipped": len(skipped),
        },
    }


# --------------------------------------------------------------------------
# Integration with the team/machine shards and the global union
# --------------------------------------------------------------------------


def events_for_commit(shard_paths: Sequence[Any], commit: str) -> list[dict]:
    """Select the local shard events that index one commit -- what to publish.

    Reads through :func:`ledger.union` so shard parsing, ordering, and duplicate
    handling are the ledger's, not a second implementation that could disagree
    with it about what the history is.
    """
    from ledger import union  # local import: keeps module import side-effect free

    return [event for event in union(shard_paths) if event.get("commit") == commit]


def merge_with_local(read: IndexRead, local_events: Iterable[dict]) -> dict[str, dict]:
    """Fold GitHub-published events together with local shard events.

    THIS IS THE POINT OF THE INDEX. A machine holds only its own shard, so its
    local union can see only its own runs. GitHub carries the others'. Merging
    lets one host answer "was this commit validated anywhere, by anyone?" without
    fetching every peer's shard.

    Safe to merge because both sides are the same envelope and
    :func:`ledger.union_events` is order-independent and dedups by event id: an
    event present both locally and on GitHub collapses to one, and the result is
    identical whichever side is passed first. A conflicting duplicate still
    raises, rather than one source silently outranking the other.
    """
    return fold(union_events([*read.accepted, *local_events]))


def unpublish(
    comment_ids: Iterable[int],
    *,
    repo: str,
    transport: Transport = gh_api,
) -> dict:
    """Delete published index comments -- the rollback that statuses cannot do.

    This is why comments won the comparison: a bad publisher run at fleet scale
    can be withdrawn. Deleting does not touch the ledger, which remains the
    authority.
    """
    deleted: list[int] = []
    failed: list[tuple[int, int]] = []
    for comment_id in comment_ids:
        status, _ = transport("DELETE", f"/repos/{repo}/comments/{comment_id}", None)
        if status in (200, 204):
            deleted.append(comment_id)
        else:
            failed.append((comment_id, status))
    return {"deleted": deleted, "failed": failed}
