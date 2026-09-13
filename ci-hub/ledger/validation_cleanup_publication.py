#!/usr/bin/env python3
"""Synchronous remote publication proof for one cleanup annotation.

The ordinary validation-ledger reader deliberately unions tracked history with
pending and retained local spool batches.  That makes it the right availability
view, but it cannot prove that an annotation is independently durable.  This
module uses the existing append-only publisher, then reads only a freshly
fetched ``origin/main`` to produce and recheck that stronger fact.

Callers must recheck the returned identity while they still hold the lock that
guards the data-lifecycle action.  A spool file, including a retained one, is
never read by the recheck and therefore can never satisfy it.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Mapping

import ledger
import publisher

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402
import git_env  # noqa: E402


ATTESTATION_SCHEMA = "validation-cleanup-publication/v1"
DEFAULT_REPOSITORY_URL = "https://github.com/rrnewton/dev-hermit.git"
NETWORK_WRAPPER = "with-proxy"
REMOTE = "origin"
BRANCH = "main"
REMOTE_REF = f"refs/remotes/{REMOTE}/{BRANCH}"
LIVE_SPOOL_RELATIVE = Path("ignored/ci-hub/validate-ledger-spool")
ANNOTATION_TOOL = "ci-hub/validate/start_unit.py"
ANNOTATION_TOOL_VERSION = "1"
FETCH_TIMEOUT_SECONDS = 120
PUBLISH_TIMEOUT_SECONDS = 600

_SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_ANNOTATION_KEYS = frozenset(
    {
        "schema",
        "event_id",
        "event_type",
        "emitted_at",
        "team",
        "host",
        "run_id",
        "producer",
        "historical_cleanup_proof",
    }
)
_ANCHOR_KEYS = frozenset(
    {
        "schema",
        "record_path_sha256",
        "target",
        "unit",
        "checkout_path_sha256",
        "materialized_target",
        "wrkslots_slot",
        "wrkslots_generation",
        "scorecard_handoff_path_sha256",
        "service_result_schema",
        "service_result_sha256",
        "producer_schema_sha256",
        "scorecard_handoff_manifest_sha256",
    }
)


@dataclass(frozen=True)
class ValidationCleanupPublication:
    """Immutable identity of exact annotation bytes on freshly fetched main."""

    schema: str
    repository_url: str
    repository_root_commits: tuple[str, ...]
    source_identity_sha256: str
    source_head: str
    remote_ref: str
    shard_path: str
    event_id: str
    event_sha256: str
    event_line_sha256: str
    publication_commit: str
    publication_blob_oid: str
    verified_remote_tip: str
    verified_shard_blob_oid: str

    def to_dict(self) -> dict[str, object]:
        """Return the exact JSON-compatible form suitable for a run record."""
        return {
            "schema": self.schema,
            "repository_url": self.repository_url,
            "repository_root_commits": list(self.repository_root_commits),
            "source_identity_sha256": self.source_identity_sha256,
            "source_head": self.source_head,
            "remote_ref": self.remote_ref,
            "shard_path": self.shard_path,
            "event_id": self.event_id,
            "event_sha256": self.event_sha256,
            "event_line_sha256": self.event_line_sha256,
            "publication_commit": self.publication_commit,
            "publication_blob_oid": self.publication_blob_oid,
            "verified_remote_tip": self.verified_remote_tip,
            "verified_shard_blob_oid": self.verified_shard_blob_oid,
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, object]) -> "ValidationCleanupPublication":
        """Parse the durable form without accepting aliases or extra fields."""
        expected = {
            "schema",
            "repository_url",
            "repository_root_commits",
            "source_identity_sha256",
            "source_head",
            "remote_ref",
            "shard_path",
            "event_id",
            "event_sha256",
            "event_line_sha256",
            "publication_commit",
            "publication_blob_oid",
            "verified_remote_tip",
            "verified_shard_blob_oid",
        }
        if set(value) != expected:
            raise publisher.PublishRefused(
                "validation cleanup publication identity has an invalid field set"
            )
        roots = value["repository_root_commits"]
        if not isinstance(roots, (list, tuple)) or not all(
            isinstance(root, str) for root in roots
        ):
            raise publisher.PublishRefused(
                "validation cleanup publication repository roots are malformed"
            )
        string_fields = expected - {"repository_root_commits"}
        if any(not isinstance(value[field], str) for field in string_fields):
            raise publisher.PublishRefused(
                "validation cleanup publication identity contains a non-string field"
            )
        identity = cls(
            schema=str(value["schema"]),
            repository_url=str(value["repository_url"]),
            repository_root_commits=tuple(roots),
            source_identity_sha256=str(value["source_identity_sha256"]),
            source_head=str(value["source_head"]),
            remote_ref=str(value["remote_ref"]),
            shard_path=str(value["shard_path"]),
            event_id=str(value["event_id"]),
            event_sha256=str(value["event_sha256"]),
            event_line_sha256=str(value["event_line_sha256"]),
            publication_commit=str(value["publication_commit"]),
            publication_blob_oid=str(value["publication_blob_oid"]),
            verified_remote_tip=str(value["verified_remote_tip"]),
            verified_shard_blob_oid=str(value["verified_shard_blob_oid"]),
        )
        _validate_identity_shape(identity)
        return identity


def _proof_git_env() -> dict[str, str]:
    environment = git_env.sanitized_git_env()
    environment["GIT_NO_REPLACE_OBJECTS"] = "1"
    return environment


def _git(repo: Path, *args: str, check: bool = True) -> bounded_capture.CapturedProcess:
    # A local refs/replace entry can make an authentic fetched commit id resolve
    # to an attacker-chosen tree.  Every proof read must inspect the object the
    # remote actually named, never the checkout's replacement-object view.
    return bounded_capture.git(
        ["--no-replace-objects", *args],
        cwd=repo,
        check=check,
        env=_proof_git_env(),
    )


def _git_bytes(
    repo: Path, *args: str, check: bool = True
) -> bounded_capture.CapturedProcess:
    return bounded_capture.git(
        ["--no-replace-objects", *args],
        cwd=repo,
        check=check,
        text=False,
        env=_proof_git_env(),
    )


def _failure_detail(result: subprocess.CompletedProcess) -> str:
    def diagnostic(value: str | bytes) -> str:
        if isinstance(value, bytes):
            return value.decode("utf-8", errors="replace")
        return value

    return (
        diagnostic(result.stderr).strip()
        or diagnostic(result.stdout).strip()
        or f"exited {result.returncode} with no output"
    )


def _canonical_json(value: Mapping[str, object]) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def _event_line(event: Mapping[str, object]) -> str:
    return json.dumps(event, sort_keys=True) + "\n"


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _require_exact_sha(value: object, *, length: int, field: str) -> str:
    pattern = _SHA1_RE if length == 40 else _SHA256_RE
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise publisher.PublishRefused(
            f"validation cleanup annotation {field} is not {length} lowercase hex characters"
        )
    return value


def _validate_annotation(event: Mapping[str, object]) -> tuple[str, str, str]:
    """Return ``(shard, semantic digest, exact-line digest)`` for a fixed shape."""
    if set(event) != _ANNOTATION_KEYS:
        raise publisher.PublishRefused(
            "validation cleanup annotation has an invalid field set; result fields are forbidden"
        )
    if event.get("schema") != ledger.ENVELOPE_VERSION:
        raise publisher.PublishRefused(
            "validation cleanup annotation has the wrong schema"
        )
    if event.get("event_type") != "run.annotate":
        raise publisher.PublishRefused(
            "validation cleanup annotation is not a run.annotate event"
        )
    if event.get("team") != "hermit":
        raise publisher.PublishRefused(
            "validation cleanup annotation has the wrong team"
        )
    producer = event.get("producer")
    if producer != {
        "source": "observed",
        "tool": ANNOTATION_TOOL,
        "tool_version": ANNOTATION_TOOL_VERSION,
    }:
        raise publisher.PublishRefused(
            "validation cleanup annotation has the wrong producer identity"
        )
    anchor = event.get("historical_cleanup_proof")
    if not isinstance(anchor, dict) or set(anchor) != _ANCHOR_KEYS:
        raise publisher.PublishRefused(
            "validation cleanup annotation has an invalid proof anchor field set"
        )
    if type(anchor.get("schema")) is not int or anchor["schema"] != 1:
        raise publisher.PublishRefused(
            "validation cleanup annotation has the wrong proof anchor schema"
        )
    record_digest = _require_exact_sha(
        anchor.get("record_path_sha256"), length=64, field="record_path_sha256"
    )
    _require_exact_sha(anchor.get("target"), length=40, field="target")
    for field in (
        "checkout_path_sha256",
        "scorecard_handoff_path_sha256",
        "service_result_sha256",
        "producer_schema_sha256",
        "scorecard_handoff_manifest_sha256",
    ):
        _require_exact_sha(anchor.get(field), length=64, field=field)
    if anchor.get("materialized_target") is not True:
        raise publisher.PublishRefused(
            "validation cleanup annotation does not identify a materialized target"
        )
    for field in ("unit", "wrkslots_slot"):
        if not isinstance(anchor.get(field), str) or not str(anchor[field]).strip():
            raise publisher.PublishRefused(
                f"validation cleanup annotation {field} is empty"
            )
    for field in ("wrkslots_generation", "service_result_schema"):
        if type(anchor.get(field)) is not int or int(anchor[field]) < 1:
            raise publisher.PublishRefused(
                f"validation cleanup annotation {field} is not a positive integer"
            )
    expected_event_id = "historical-cleanup-proof-" + _sha256(_canonical_json(anchor))
    if event.get("event_id") != expected_event_id:
        raise publisher.PublishRefused(
            "validation cleanup annotation event_id does not bind its exact proof anchor"
        )
    if event.get("run_id") != f"historical-cleanup-record-{record_digest}":
        raise publisher.PublishRefused(
            "validation cleanup annotation run_id does not bind its record path"
        )
    host = event.get("host")
    emitted_at = event.get("emitted_at")
    if not isinstance(host, str) or not isinstance(emitted_at, str):
        raise publisher.PublishRefused(
            "validation cleanup annotation host or emitted_at is malformed"
        )
    shard = ledger.shard_path("hermit", host, emitted_at)
    violations = ledger.validate_event(dict(event), shard)
    if violations:
        raise publisher.PublishRefused(
            "validation cleanup annotation violates the ledger contract: "
            + ",".join(violations)
        )
    return (
        shard,
        _sha256(_canonical_json(event)),
        _sha256(_event_line(event).encode()),
    )


def _remote_url(repo: Path) -> str:
    result = _git(repo, "remote", "get-url", REMOTE, check=False)
    if result.returncode != 0:
        raise publisher.PublishRefused(
            f"cannot identify validation cleanup repository: {_failure_detail(result)}"
        )
    value = result.stdout.strip()
    if not value or "\n" in value:
        raise publisher.PublishRefused(
            "validation cleanup repository has no single origin URL"
        )
    return value


def _root_commits(repo: Path) -> tuple[str, ...]:
    result = _git(repo, "rev-list", "--max-parents=0", "HEAD", check=False)
    roots = tuple(sorted(set(result.stdout.split()))) if result.returncode == 0 else ()
    if not roots or any(_SHA1_RE.fullmatch(root) is None for root in roots):
        raise publisher.PublishRefused(
            f"cannot establish validation cleanup repository roots for {repo}"
        )
    return roots


def _source_identity(
    source_root: Path, repository_url: str, roots: tuple[str, ...]
) -> str:
    git_dir = _git(source_root, "rev-parse", "--absolute-git-dir", check=False)
    if git_dir.returncode != 0:
        raise publisher.PublishRefused(
            f"cannot identify validation cleanup source checkout: {_failure_detail(git_dir)}"
        )
    payload = {
        "source_root": str(source_root),
        "git_dir": str(Path(git_dir.stdout.strip()).resolve()),
        "repository_url": repository_url,
        "repository_root_commits": list(roots),
    }
    return _sha256(_canonical_json(payload))


def _source_head(source_root: Path) -> str:
    resolved = _git(source_root, "rev-parse", "--verify", "HEAD^{commit}", check=False)
    head = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA1_RE.fullmatch(head) is None:
        raise publisher.PublishRefused(
            "cannot identify the exact validation cleanup source HEAD"
        )
    return head


def _repository_binding(
    repo: Path, source_root: Path, expected_repository_url: str
) -> tuple[tuple[str, ...], str, str]:
    if repo == source_root:
        raise publisher.PublishRefused(
            "validation cleanup publisher checkout is the producing source checkout"
        )
    for role, checkout in (("publisher", repo), ("source", source_root)):
        actual = _remote_url(checkout)
        if actual != expected_repository_url:
            raise publisher.PublishRefused(
                f"wrong validation cleanup repository for {role}: expected "
                f"{expected_repository_url!r}, got {actual!r}"
            )
    publisher_roots = _root_commits(repo)
    source_roots = _root_commits(source_root)
    shared = tuple(sorted(set(publisher_roots) & set(source_roots)))
    if not shared:
        raise publisher.PublishRefused(
            "validation cleanup publisher and source are different repositories"
        )
    return (
        shared,
        _source_identity(source_root, expected_repository_url, shared),
        _source_head(source_root),
    )


def _network_command(
    command: list[str],
    net_wrapper: str | os.PathLike[str],
    *,
    timeout_seconds: int,
) -> list[str]:
    return [
        "timeout",
        "--signal=TERM",
        "--kill-after=5s",
        f"{timeout_seconds}s",
        str(net_wrapper),
        *command,
    ]


def _fresh_remote_tip(repo: Path, *, net_wrapper: str | os.PathLike[str]) -> str:
    result = bounded_capture.capture(
        _network_command(
            [
                "git",
                "--no-replace-objects",
                "-C",
                str(repo),
                "fetch",
                "--quiet",
                "--no-tags",
                REMOTE,
                f"+refs/heads/{BRANCH}:{REMOTE_REF}",
            ],
            net_wrapper,
            timeout_seconds=FETCH_TIMEOUT_SECONDS,
        ),
        env=_proof_git_env(),
    )
    if result.returncode != 0:
        raise publisher.PublishRefused(
            "fresh fetch of validation cleanup publication is unavailable: "
            + _failure_detail(result)
        )
    resolved = _git(
        repo, "rev-parse", "--verify", f"{REMOTE_REF}^{{commit}}", check=False
    )
    tip = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA1_RE.fullmatch(tip) is None:
        raise publisher.PublishRefused(
            "freshly fetched validation cleanup remote main has no exact commit"
        )
    return tip


def _publish_spool(
    repo: Path,
    source_root: Path,
    spool_dir: Path,
    *,
    serialized_writer: str | os.PathLike[str],
    attribution: str,
    attempts: int,
    net_wrapper: str | os.PathLike[str],
) -> None:
    """Run the supported publisher with one network environment end to end."""
    command = [
        sys.executable,
        str(Path(publisher.__file__).resolve()),
        "publish",
        "--source-root",
        str(source_root),
        "--repo",
        str(repo),
        "--spool-dir",
        str(spool_dir),
        "--writer",
        str(serialized_writer),
        "--attribution",
        attribution,
        "--attempts",
        str(attempts),
        "--store",
        "ledger",
    ]
    result = bounded_capture.capture(
        _network_command(
            command,
            net_wrapper,
            timeout_seconds=PUBLISH_TIMEOUT_SECONDS,
        ),
        cwd=repo,
        env=_proof_git_env(),
    )
    if result.returncode != 0:
        raise publisher.PublishRefused(
            "validation cleanup publication is unavailable: " + _failure_detail(result)
        )


def _is_ancestor(repo: Path, older: str, newer: str, *, role: str) -> bool:
    result = _git(repo, "merge-base", "--is-ancestor", older, newer, check=False)
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    raise publisher.PublishRefused(f"cannot prove {role}: {_failure_detail(result)}")


def _decode_shard_bytes(payload: bytes, *, role: str) -> str:
    try:
        return payload.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise publisher.PublishRefused(
            f"validation cleanup {role} shard bytes are not valid UTF-8: {error}"
        ) from error


def _show_shard(repo: Path, commit: str, shard: str, *, role: str) -> bytes:
    shown = _git_bytes(repo, "show", f"{commit}:{shard}", check=False)
    if shown.returncode != 0:
        raise publisher.PublishRefused(
            f"validation cleanup {role} shard is unavailable at {commit}: "
            + _failure_detail(shown)
        )
    _decode_shard_bytes(shown.stdout, role=role)
    return shown.stdout


def _blob_oid(repo: Path, commit: str, shard: str, *, role: str) -> str:
    result = _git(repo, "rev-parse", f"{commit}:{shard}", check=False)
    oid = result.stdout.strip()
    if result.returncode != 0 or _SHA1_RE.fullmatch(oid) is None:
        raise publisher.PublishRefused(
            f"cannot identify validation cleanup {role} shard blob at {commit}"
        )
    return oid


def _require_exact_event(
    payload: bytes, event: Mapping[str, object], shard: str, *, role: str
) -> None:
    _decode_shard_bytes(payload, role=role)
    if payload and not payload.endswith(b"\n"):
        raise publisher.PublishRefused(
            f"validation cleanup {role} shard has a truncated final line"
        )
    parsed: list[dict] = []
    matches: list[bytes] = []
    event_id = event["event_id"]
    try:
        for line in payload.splitlines(keepends=True):
            if not line.strip():
                continue
            item = ledger.parse_line(line[:-1].decode("utf-8", errors="strict"))
            parsed.append(item)
            if item.get("event_id") == event_id:
                matches.append(line)
    except ledger.LedgerError as error:
        raise publisher.PublishRefused(
            f"validation cleanup {role} shard is malformed: {error}"
        ) from error
    violations = ledger.validate_events(parsed, shard)
    bad = [(index, codes) for index, codes in enumerate(violations, 1) if codes]
    if bad:
        raise publisher.PublishRefused(
            f"validation cleanup {role} shard violates the ledger contract: {bad}"
        )
    if len(matches) != 1:
        raise publisher.PublishRefused(
            f"validation cleanup {role} shard contains {len(matches)} copies of the exact event id"
        )
    if matches[0] != _event_line(event).encode():
        raise publisher.PublishRefused(f"validation cleanup {role} event bytes changed")


def _introduction_commit(
    repo: Path, remote_tip: str, shard: str, event: Mapping[str, object]
) -> str:
    event_id = str(event["event_id"])
    history = _git(
        repo,
        "log",
        "--format=%H",
        "--reverse",
        f"-S{event_id}",
        remote_tip,
        "--",
        shard,
        check=False,
    )
    if history.returncode != 0:
        raise publisher.PublishRefused(
            "cannot locate validation cleanup publication commit: "
            + _failure_detail(history)
        )
    expected = _event_line(event).encode()
    for commit in history.stdout.split():
        shown = _git_bytes(repo, "show", f"{commit}:{shard}", check=False)
        if shown.returncode != 0:
            continue
        payload = shown.stdout
        _decode_shard_bytes(payload, role="publication-history")
        if expected in payload.splitlines(keepends=True):
            return commit
    raise publisher.PublishRefused(
        "freshly fetched remote main does not contain the exact validation cleanup event"
    )


def _validate_identity_shape(identity: ValidationCleanupPublication) -> None:
    if identity.schema != ATTESTATION_SCHEMA or identity.remote_ref != REMOTE_REF:
        raise publisher.PublishRefused(
            "validation cleanup publication identity has the wrong schema or remote ref"
        )
    if not identity.repository_url:
        raise publisher.PublishRefused(
            "validation cleanup publication identity has no repository URL"
        )
    if not identity.repository_root_commits or any(
        _SHA1_RE.fullmatch(root) is None for root in identity.repository_root_commits
    ):
        raise publisher.PublishRefused(
            "validation cleanup publication identity has invalid repository roots"
        )
    for field in (
        "source_identity_sha256",
        "event_sha256",
        "event_line_sha256",
    ):
        if _SHA256_RE.fullmatch(getattr(identity, field)) is None:
            raise publisher.PublishRefused(
                f"validation cleanup publication identity has invalid {field}"
            )
    for field in (
        "source_head",
        "publication_commit",
        "publication_blob_oid",
        "verified_remote_tip",
        "verified_shard_blob_oid",
    ):
        if _SHA1_RE.fullmatch(getattr(identity, field)) is None:
            raise publisher.PublishRefused(
                f"validation cleanup publication identity has invalid {field}"
            )


def publish_validation_cleanup_proof(
    repo: str | os.PathLike[str],
    source_root: str | os.PathLike[str],
    event: Mapping[str, object],
    *,
    serialized_writer: str | os.PathLike[str],
    attribution: str,
    attempts: int = publisher.DEFAULT_ATTEMPTS,
) -> ValidationCleanupPublication:
    """Publish one exact annotation and attest it from freshly fetched main.

    The writer is mandatory: production publication must retain the existing
    machine-wide serialization and fresh-origin compare-and-swap boundary.
    A failure after the remote write may leave a retained spool batch; retrying
    is safe and discovers the original introduction commit.
    """
    publisher_repo = Path(repo).resolve()
    source = Path(source_root).resolve()
    shard, event_digest, line_digest = _validate_annotation(event)
    roots, source_identity, source_head = _repository_binding(
        publisher_repo, source, DEFAULT_REPOSITORY_URL
    )

    spool_dir = source / LIVE_SPOOL_RELATIVE
    try:
        publisher.spool(
            spool_dir,
            str(event["team"]),
            str(event["host"]),
            [dict(event)],
        )
    except (OSError, ledger.LedgerError, publisher.PublishRefused) as error:
        raise publisher.PublishRefused(
            f"cannot spool validation cleanup annotation: {error}"
        ) from error

    before_tip = _fresh_remote_tip(publisher_repo, net_wrapper=NETWORK_WRAPPER)
    _publish_spool(
        publisher_repo,
        source,
        spool_dir,
        serialized_writer=serialized_writer,
        attribution=attribution,
        attempts=attempts,
        net_wrapper=NETWORK_WRAPPER,
    )
    remote_tip = _fresh_remote_tip(publisher_repo, net_wrapper=NETWORK_WRAPPER)
    if not _is_ancestor(
        publisher_repo,
        before_tip,
        remote_tip,
        role="validation cleanup remote fast-forward",
    ):
        raise publisher.PublishRefused(
            "validation cleanup remote main moved non-fast-forward during publication"
        )

    publication_commit = _introduction_commit(publisher_repo, remote_tip, shard, event)
    if not _is_ancestor(
        publisher_repo,
        publication_commit,
        remote_tip,
        role="validation cleanup publication ancestry",
    ):
        raise publisher.PublishRefused(
            "validation cleanup publication commit is absent from freshly fetched remote main"
        )
    publication_bytes = _show_shard(
        publisher_repo, publication_commit, shard, role="publication"
    )
    current_bytes = _show_shard(publisher_repo, remote_tip, shard, role="remote-main")
    _require_exact_event(publication_bytes, event, shard, role="publication")
    _require_exact_event(current_bytes, event, shard, role="remote-main")
    if not current_bytes.startswith(publication_bytes):
        raise publisher.PublishRefused(
            "validation cleanup remote shard is not an append-only extension of its publication"
        )

    verified_roots, verified_source_identity, verified_source_head = (
        _repository_binding(publisher_repo, source, DEFAULT_REPOSITORY_URL)
    )
    if (
        verified_roots != roots
        or verified_source_identity != source_identity
        or verified_source_head != source_head
    ):
        raise publisher.PublishRefused(
            "validation cleanup source identity or HEAD changed during publication"
        )

    identity = ValidationCleanupPublication(
        schema=ATTESTATION_SCHEMA,
        repository_url=DEFAULT_REPOSITORY_URL,
        repository_root_commits=roots,
        source_identity_sha256=source_identity,
        source_head=source_head,
        remote_ref=REMOTE_REF,
        shard_path=shard,
        event_id=str(event["event_id"]),
        event_sha256=event_digest,
        event_line_sha256=line_digest,
        publication_commit=publication_commit,
        publication_blob_oid=_blob_oid(
            publisher_repo, publication_commit, shard, role="publication"
        ),
        verified_remote_tip=remote_tip,
        verified_shard_blob_oid=_blob_oid(
            publisher_repo, remote_tip, shard, role="remote-main"
        ),
    )
    _validate_identity_shape(identity)
    return identity


def recheck_validation_cleanup_proof(
    repo: str | os.PathLike[str],
    source_root: str | os.PathLike[str],
    event: Mapping[str, object],
    identity: ValidationCleanupPublication,
) -> ValidationCleanupPublication:
    """Fresh-fetch and refresh an identity immediately before lifecycle action."""
    _validate_identity_shape(identity)
    publisher_repo = Path(repo).resolve()
    source = Path(source_root).resolve()
    shard, event_digest, line_digest = _validate_annotation(event)
    roots, source_identity, source_head = _repository_binding(
        publisher_repo, source, DEFAULT_REPOSITORY_URL
    )
    expected_fields = {
        "repository_url": DEFAULT_REPOSITORY_URL,
        "repository_root_commits": roots,
        "source_identity_sha256": source_identity,
        "source_head": source_head,
        "remote_ref": REMOTE_REF,
        "shard_path": shard,
        "event_id": str(event["event_id"]),
        "event_sha256": event_digest,
        "event_line_sha256": line_digest,
    }
    for field, expected in expected_fields.items():
        if getattr(identity, field) != expected:
            raise publisher.PublishRefused(
                f"wrong validation cleanup publication {field}: identity does not match caller"
            )

    remote_tip = _fresh_remote_tip(publisher_repo, net_wrapper=NETWORK_WRAPPER)
    if not _is_ancestor(
        publisher_repo,
        identity.verified_remote_tip,
        remote_tip,
        role="validation cleanup attestation freshness",
    ):
        raise publisher.PublishRefused(
            "stale validation cleanup publication identity: remote main moved non-fast-forward"
        )
    if not _is_ancestor(
        publisher_repo,
        identity.publication_commit,
        remote_tip,
        role="validation cleanup publication remote ancestry",
    ):
        raise publisher.PublishRefused(
            "validation cleanup publication commit is missing from remote main ancestry"
        )

    publication_bytes = _show_shard(
        publisher_repo,
        identity.publication_commit,
        shard,
        role="publication",
    )
    publication_blob = _blob_oid(
        publisher_repo,
        identity.publication_commit,
        shard,
        role="publication",
    )
    if publication_blob != identity.publication_blob_oid:
        raise publisher.PublishRefused(
            "validation cleanup publication blob identity changed"
        )
    prior_bytes = _show_shard(
        publisher_repo,
        identity.verified_remote_tip,
        shard,
        role="previously-verified",
    )
    prior_blob = _blob_oid(
        publisher_repo,
        identity.verified_remote_tip,
        shard,
        role="previously-verified",
    )
    if prior_blob != identity.verified_shard_blob_oid:
        raise publisher.PublishRefused(
            "stale validation cleanup publication identity: prior shard blob changed"
        )
    current_bytes = _show_shard(publisher_repo, remote_tip, shard, role="remote-main")
    _require_exact_event(publication_bytes, event, shard, role="publication")
    _require_exact_event(current_bytes, event, shard, role="remote-main")
    if not prior_bytes.startswith(publication_bytes):
        raise publisher.PublishRefused(
            "validation cleanup previously verified shard rewrote publication bytes"
        )
    if not current_bytes.startswith(prior_bytes):
        raise publisher.PublishRefused(
            "validation cleanup remote shard changed bytes instead of appending"
        )

    return replace(
        identity,
        verified_remote_tip=remote_tip,
        verified_shard_blob_oid=_blob_oid(
            publisher_repo, remote_tip, shard, role="remote-main"
        ),
    )
