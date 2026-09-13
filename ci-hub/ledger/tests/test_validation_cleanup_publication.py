#!/usr/bin/env python3
"""Remote-bound validation-cleanup publication controls.

Run: python3 -m pytest ci-hub/ledger/tests/test_validation_cleanup_publication.py -q
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from dataclasses import FrozenInstanceError, replace
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import publisher  # noqa: E402
import validation_cleanup_publication as cleanup_publication  # noqa: E402


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=check,
    )


def cleanup_event(marker: str = "1") -> dict:
    record_digest = marker * 64
    anchor = {
        "schema": 1,
        "record_path_sha256": record_digest,
        "target": "a" * 40,
        "unit": "hermit-validation.service",
        "checkout_path_sha256": "2" * 64,
        "materialized_target": True,
        "wrkslots_slot": "validate-hermit-proof",
        "wrkslots_generation": 7,
        "scorecard_handoff_path_sha256": "3" * 64,
        "service_result_schema": 3,
        "service_result_sha256": "4" * 64,
        "producer_schema_sha256": "5" * 64,
        "scorecard_handoff_manifest_sha256": "6" * 64,
    }
    anchor_digest = hashlib.sha256(
        json.dumps(anchor, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    return {
        "schema": "validate-ledger/v1",
        "event_id": f"historical-cleanup-proof-{anchor_digest}",
        "event_type": "run.annotate",
        "emitted_at": "2026-09-12T12:00:00.000000Z",
        "team": "hermit",
        "host": "hosta",
        "run_id": f"historical-cleanup-record-{record_digest}",
        "producer": {
            "source": "observed",
            "tool": "ci-hub/validate/start_unit.py",
            "tool_version": "1",
        },
        "historical_cleanup_proof": anchor,
    }


@pytest.fixture()
def publication_repos(tmp_path, monkeypatch):
    bare = tmp_path / "remote.git"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(bare)], check=True)
    seed = tmp_path / "seed"
    subprocess.run(["git", "init", "-q", "-b", "main", str(seed)], check=True)
    (seed / "README").write_text("validation history\n")
    git(seed, "add", "README")
    git(
        seed,
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@invalid",
        "commit",
        "-q",
        "-m",
        "seed",
    )
    base = git(seed, "rev-parse", "HEAD").stdout.strip()
    git(seed, "remote", "add", "origin", str(bare))
    git(seed, "push", "-q", "origin", "main")

    source = tmp_path / "source"
    publishing = tmp_path / "publisher"
    subprocess.run(["git", "clone", "-q", str(bare), str(source)], check=True)
    subprocess.run(["git", "clone", "-q", str(bare), str(publishing)], check=True)
    git(publishing, "config", "user.name", "Validation Publisher")
    git(publishing, "config", "user.email", "publisher@invalid")

    writer = tmp_path / "serialized-writer"
    writer.write_text(
        "#!/bin/sh\n"
        'test "$1" = publish || exit 2\n'
        'exec git push origin "$2:refs/heads/main"\n'
    )
    writer.chmod(0o755)
    net_wrapper = tmp_path / "net-wrapper"
    net_wrapper.write_text('#!/bin/sh\nexec "$@"\n')
    net_wrapper.chmod(0o755)
    monkeypatch.setattr(cleanup_publication, "DEFAULT_REPOSITORY_URL", str(bare))
    monkeypatch.setattr(cleanup_publication, "NETWORK_WRAPPER", str(net_wrapper))
    return bare, source, publishing, writer, base


def publish_exact(
    bare: Path,
    source: Path,
    publishing: Path,
    writer: Path,
    event: dict,
) -> cleanup_publication.ValidationCleanupPublication:
    return cleanup_publication.publish_validation_cleanup_proof(
        publishing,
        source,
        event,
        serialized_writer=writer,
        attribution="[impl agent, test] [validation-cleanup-publication, hosta]",
    )


def recheck(
    bare: Path,
    source: Path,
    publishing: Path,
    event: dict,
    identity: cleanup_publication.ValidationCleanupPublication,
) -> cleanup_publication.ValidationCleanupPublication:
    return cleanup_publication.recheck_validation_cleanup_proof(
        publishing,
        source,
        event,
        identity,
    )


def remote_shard(bare: Path, identity) -> str:
    return git(bare, "show", f"main:{identity.shard_path}").stdout


def test_publish_returns_immutable_exact_remote_identity(publication_repos):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()

    identity = publish_exact(bare, source, publishing, writer, event)

    assert identity.publication_commit == identity.verified_remote_tip
    assert identity.event_id == event["event_id"]
    assert identity.source_head == git(source, "rev-parse", "HEAD").stdout.strip()
    assert remote_shard(bare, identity) == json.dumps(event, sort_keys=True) + "\n"
    assert (
        cleanup_publication.ValidationCleanupPublication.from_dict(identity.to_dict())
        == identity
    )
    with pytest.raises(FrozenInstanceError):
        identity.event_id = "changed"  # type: ignore[misc]
    assert recheck(bare, source, publishing, event, identity) == identity


@pytest.mark.parametrize(
    "selector", sorted(cleanup_publication.git_env.GIT_REPOSITORY_ENV)
)
def test_each_ambient_repository_selector_is_removed_from_every_proof_child(
    publication_repos, monkeypatch, selector
):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    monkeypatch.setenv(selector, "/not/the/validation/repository")
    observed: set[str] = set()
    real_git = cleanup_publication.bounded_capture.git
    real_capture = cleanup_publication.bounded_capture.capture

    def require_sanitized(environment, role):
        assert environment is not None
        assert selector not in environment
        assert environment.get("GIT_NO_REPLACE_OBJECTS") == "1"
        observed.add(role)

    def checked_git(args, **kwargs):
        require_sanitized(kwargs.get("env"), "git")
        return real_git(args, **kwargs)

    def checked_capture(command, **kwargs):
        require_sanitized(kwargs.get("env"), "publisher-or-fetch")
        return real_capture(command, **kwargs)

    monkeypatch.setattr(cleanup_publication.bounded_capture, "git", checked_git)
    monkeypatch.setattr(cleanup_publication.bounded_capture, "capture", checked_capture)

    identity = publish_exact(bare, source, publishing, writer, event)
    assert recheck(bare, source, publishing, event, identity) == identity
    assert observed == {"git", "publisher-or-fetch"}


def test_unavailable_remote_refuses_even_though_pending_spool_is_readable(
    publication_repos, tmp_path
):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    unavailable = tmp_path / "remote-unavailable.git"
    bare.rename(unavailable)

    with pytest.raises(publisher.PublishRefused, match="fresh fetch.*unavailable"):
        publish_exact(bare, source, publishing, writer, event)

    pending = publisher.spooled(source / cleanup_publication.LIVE_SPOOL_RELATIVE)
    assert [item["event_id"] for events in pending.values() for item in events] == [
        event["event_id"]
    ]


def test_local_replace_object_cannot_forge_remote_publication(
    publication_repos, tmp_path, monkeypatch
):
    bare, source, publishing, _, base = publication_repos
    event = cleanup_event()
    relative = "ledger/hermit/hosta/2026-09.jsonl"

    builder = tmp_path / "replacement-builder"
    subprocess.run(["git", "clone", "-q", str(bare), str(builder)], check=True)
    shard = builder / relative
    shard.parent.mkdir(parents=True)
    shard.write_text(json.dumps(event, sort_keys=True) + "\n")
    git(builder, "add", "--", relative)
    tree = git(builder, "write-tree").stdout.strip()
    git(builder, "config", "user.name", "Replacement Builder")
    git(builder, "config", "user.email", "replacement@invalid")
    replacement = git(
        builder, "commit-tree", tree, "-m", "replacement tree"
    ).stdout.strip()
    git(publishing, "fetch", "-q", str(builder), replacement)
    git(publishing, "replace", base, replacement)

    assert (
        git(publishing, "show", f"origin/main:{relative}").stdout
        == json.dumps(event, sort_keys=True) + "\n"
    ), "the planted replacement must make the ordinary local view lie"
    assert (
        git(
            bare,
            "--no-replace-objects",
            "show",
            f"main:{relative}",
            check=False,
        ).returncode
        != 0
    ), "the genuine remote must independently lack the event"

    # First isolate the post-publish proof.  A local replacement made every
    # pre-fix ancestry/history/content read report this absent event as remote.
    # Even if the publisher stage reports completion, fresh remote inspection
    # must return no identity.
    real_publish = cleanup_publication._publish_spool
    monkeypatch.setattr(
        cleanup_publication, "_publish_spool", lambda *args, **kwargs: None
    )
    post_publish_identity = None
    with pytest.raises(
        publisher.PublishRefused,
        match="does not contain the exact validation cleanup event",
    ):
        post_publish_identity = cleanup_publication.publish_validation_cleanup_proof(
            publishing,
            source,
            event,
            serialized_writer=tmp_path / "unused-writer",
            attribution="[impl agent, test] [validation-cleanup-publication, hosta]",
            attempts=1,
        )
    assert post_publish_identity is None

    # Then exercise the publisher subprocess itself.  Without the inherited
    # no-replace environment it sees the invented event, skips its writer, and
    # falsely reports an already-published batch.
    monkeypatch.setattr(cleanup_publication, "_publish_spool", real_publish)
    lying_writer = tmp_path / "lying-writer"
    lying_writer.write_text("#!/bin/sh\nexit 0\n")
    lying_writer.chmod(0o755)
    identity = None
    with pytest.raises(publisher.PublishRefused, match="publication is unavailable"):
        identity = cleanup_publication.publish_validation_cleanup_proof(
            publishing,
            source,
            event,
            serialized_writer=lying_writer,
            attribution="[impl agent, test] [validation-cleanup-publication, hosta]",
            attempts=1,
        )

    assert identity is None
    assert (
        git(
            bare,
            "--no-replace-objects",
            "show",
            f"main:{relative}",
            check=False,
        ).returncode
        != 0
    ), "a local replacement must not cause remote publication"


@pytest.mark.parametrize(
    "field,value",
    [("commit", "a" * 40), ("outcome", "pass"), ("legacy_row", {})],
)
def test_cleanup_annotation_cannot_acquire_result_authority(
    publication_repos, field, value
):
    bare, source, publishing, writer, _ = publication_repos
    event = {**cleanup_event(), field: value}

    with pytest.raises(publisher.PublishRefused, match="result fields are forbidden"):
        publish_exact(bare, source, publishing, writer, event)

    assert git(bare, "ls-tree", "-r", "--name-only", "main").stdout.splitlines() == [
        "README"
    ]


def test_retry_is_idempotent_and_returns_the_original_publication_commit(
    publication_repos,
):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    first = publish_exact(bare, source, publishing, writer, event)

    second = publish_exact(bare, source, publishing, writer, event)

    assert second == first
    assert remote_shard(bare, first).count(json.dumps(event, sort_keys=True)) == 1


def test_concurrent_exact_publication_is_adopted_without_duplicate(
    publication_repos, tmp_path
):
    bare, source, publishing, _, _ = publication_repos
    event = cleanup_event()
    other = tmp_path / "other"
    subprocess.run(["git", "clone", "-q", str(bare), str(other)], check=True)
    shard = other / "ledger/hermit/hosta/2026-09.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_text(json.dumps(event, sort_keys=True) + "\n")
    git(other, "add", "--", str(shard.relative_to(other)))
    git(
        other,
        "-c",
        "user.name=concurrent",
        "-c",
        "user.email=concurrent@invalid",
        "commit",
        "-q",
        "-m",
        "concurrent publication",
    )
    other_commit = git(other, "rev-parse", "HEAD").stdout.strip()
    marker = tmp_path / "concurrent-pushed"
    writer = tmp_path / "concurrent-writer"
    writer.write_text(
        "#!/bin/sh\n"
        f"if [ ! -f '{marker}' ]; then\n"
        f"  touch '{marker}'\n"
        f"  git -C '{other}' push origin '{other_commit}:refs/heads/main' || exit 1\n"
        "fi\n"
        'exec git push origin "$2:refs/heads/main"\n'
    )
    writer.chmod(0o755)

    identity = publish_exact(bare, source, publishing, writer, event)

    assert marker.exists()
    assert identity.publication_commit == other_commit
    assert remote_shard(bare, identity).count(json.dumps(event, sort_keys=True)) == 1


def test_recheck_refreshes_across_an_append_only_remote_advance(
    publication_repos, tmp_path
):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event("1")
    identity = publish_exact(bare, source, publishing, writer, event)
    other_event = cleanup_event("7")
    other = tmp_path / "append"
    subprocess.run(["git", "clone", "-q", str(bare), str(other)], check=True)
    shard = other / identity.shard_path
    with shard.open("a") as stream:
        stream.write(json.dumps(other_event, sort_keys=True) + "\n")
    git(other, "add", "--", identity.shard_path)
    git(
        other,
        "-c",
        "user.name=append",
        "-c",
        "user.email=append@invalid",
        "commit",
        "-q",
        "-m",
        "append another annotation",
    )
    git(other, "push", "-q", "origin", "main")

    refreshed = recheck(bare, source, publishing, event, identity)

    assert refreshed.publication_commit == identity.publication_commit
    assert refreshed.verified_remote_tip != identity.verified_remote_tip
    assert refreshed.verified_shard_blob_oid != identity.verified_shard_blob_oid


def test_recheck_refuses_a_moved_source_head(publication_repos):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    identity = publish_exact(bare, source, publishing, writer, event)
    git(source, "config", "user.name", "Moved Source")
    git(source, "config", "user.email", "moved-source@invalid")
    git(source, "commit", "-q", "--allow-empty", "-m", "move source head")

    with pytest.raises(publisher.PublishRefused, match="source_head"):
        recheck(bare, source, publishing, event, identity)


def test_publish_refuses_source_head_move_before_returning_identity(
    publication_repos, monkeypatch
):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    real_publish = cleanup_publication._publish_spool

    def publish_then_move_source(*args, **kwargs):
        real_publish(*args, **kwargs)
        git(source, "config", "user.name", "Moved Source")
        git(source, "config", "user.email", "moved-source@invalid")
        git(source, "commit", "-q", "--allow-empty", "-m", "move source head")

    monkeypatch.setattr(cleanup_publication, "_publish_spool", publish_then_move_source)

    with pytest.raises(
        publisher.PublishRefused, match="HEAD changed during publication"
    ):
        publish_exact(bare, source, publishing, writer, event)


def test_recheck_refuses_invalid_utf8_shard_bytes(publication_repos, tmp_path):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    identity = publish_exact(bare, source, publishing, writer, event)
    mutator = tmp_path / "invalid-utf8"
    subprocess.run(["git", "clone", "-q", str(bare), str(mutator)], check=True)
    shard = mutator / identity.shard_path
    with shard.open("ab") as stream:
        stream.write(b'{"invalid":"\xff"}\n')
    git(mutator, "add", "--", identity.shard_path)
    git(
        mutator,
        "-c",
        "user.name=invalid-utf8",
        "-c",
        "user.email=invalid-utf8@invalid",
        "commit",
        "-q",
        "-m",
        "append invalid utf8",
    )
    git(mutator, "push", "-q", "origin", "main")
    remote_bytes = subprocess.run(
        ["git", "-C", str(bare), "show", f"main:{identity.shard_path}"],
        capture_output=True,
        check=True,
    ).stdout
    assert remote_bytes.endswith(b'{"invalid":"\xff"}\n')

    with pytest.raises(
        publisher.PublishRefused, match="remote-main shard bytes are not valid UTF-8"
    ):
        recheck(bare, source, publishing, event, identity)


def test_recheck_refuses_wrong_repository_source_path_and_event(
    publication_repos, tmp_path
):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    identity = publish_exact(bare, source, publishing, writer, event)

    wrong_publisher = tmp_path / "wrong-publisher"
    subprocess.run(["git", "clone", "-q", str(bare), str(wrong_publisher)], check=True)
    git(wrong_publisher, "remote", "set-url", "origin", str(tmp_path / "wrong.git"))
    with pytest.raises(publisher.PublishRefused, match="wrong.*repository"):
        recheck(bare, source, wrong_publisher, event, identity)

    other_source = tmp_path / "other-source"
    subprocess.run(["git", "clone", "-q", str(bare), str(other_source)], check=True)
    with pytest.raises(publisher.PublishRefused, match="source_identity"):
        recheck(bare, other_source, publishing, event, identity)

    wrong_path = replace(identity, shard_path="ledger/hermit/hostb/2026-09.jsonl")
    with pytest.raises(publisher.PublishRefused, match="shard_path"):
        recheck(bare, source, publishing, event, wrong_path)

    wrong_event = {**event, "emitted_at": "2026-09-12T12:00:01.000000Z"}
    with pytest.raises(publisher.PublishRefused, match="event_sha256"):
        recheck(bare, source, publishing, wrong_event, identity)


def test_recheck_refuses_remote_event_byte_change(publication_repos, tmp_path):
    bare, source, publishing, writer, _ = publication_repos
    event = cleanup_event()
    identity = publish_exact(bare, source, publishing, writer, event)
    mutator = tmp_path / "mutator"
    subprocess.run(["git", "clone", "-q", str(bare), str(mutator)], check=True)
    shard = mutator / identity.shard_path
    changed = json.loads(shard.read_text())
    changed["historical_cleanup_proof"]["service_result_sha256"] = "f" * 64
    shard.write_text(json.dumps(changed, sort_keys=True) + "\n")
    git(mutator, "add", "--", identity.shard_path)
    git(
        mutator,
        "-c",
        "user.name=mutator",
        "-c",
        "user.email=mutator@invalid",
        "commit",
        "-q",
        "-m",
        "rewrite event bytes",
    )
    git(mutator, "push", "-q", "origin", "main")

    with pytest.raises(publisher.PublishRefused, match="event bytes changed"):
        recheck(bare, source, publishing, event, identity)


def test_recheck_refuses_missing_publication_ancestry(publication_repos):
    bare, source, publishing, writer, base = publication_repos
    event = cleanup_event()
    identity = publish_exact(bare, source, publishing, writer, event)
    git(publishing, "checkout", "-q", "--detach", base)
    git(
        publishing,
        "-c",
        "user.name=ghost",
        "-c",
        "user.email=ghost@invalid",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "unpublished identity",
    )
    unpublished = git(publishing, "rev-parse", "HEAD").stdout.strip()
    wrong = replace(identity, publication_commit=unpublished)

    with pytest.raises(
        publisher.PublishRefused, match="missing from remote main ancestry"
    ):
        recheck(bare, source, publishing, event, wrong)


def test_recheck_refuses_stale_non_fast_forward_even_with_retained_spool(
    publication_repos,
):
    bare, source, publishing, writer, base = publication_repos
    event = cleanup_event()
    identity = publish_exact(bare, source, publishing, writer, event)
    retained = publisher.retained(source / cleanup_publication.LIVE_SPOOL_RELATIVE)
    assert any(
        item["event_id"] == event["event_id"]
        for items in retained.values()
        for item in items
    )
    git(bare, "update-ref", "refs/heads/main", base)

    with pytest.raises(publisher.PublishRefused, match="stale.*non-fast-forward"):
        recheck(bare, source, publishing, event, identity)
