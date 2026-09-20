#!/usr/bin/env python3
"""Fail-closed brackets for archival recovery of payload-only escalations."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import pytest


ROOT = Path(__file__).resolve().parents[2]
RECOVER = ROOT / "ci-hub/bin/recover-health-escalation-legacy"
RUNNER = ROOT / "ci-hub/bin/run-health-escalation"
ENQUEUE = ROOT / "ci-hub/bin/enqueue-health-escalation"
sys.path.insert(0, str(ROOT / "ci-hub/lib"))

import health_escalation_recovery as recovery  # noqa: E402
from health_escalation_event import make_event, write_event  # noqa: E402
from health_escalation_recovery import (  # noqa: E402
    RecoveryRefused,
    apply_recovery,
    write_plan,
)


def make_queue(tmp_path: Path) -> Path:
    queue = tmp_path / "queue"
    queue.mkdir()
    (queue / ".worker.lock").write_bytes(b"")
    return queue


def legacy(
    queue: Path,
    sequence: int,
    kind: str,
    payload: bytes,
    *,
    github_ack: bytes | None = None,
    taskgraph_ack: bytes | None = None,
) -> Path:
    name = f"event-20260906T{sequence:012d}Z-{sequence}-legacy{sequence:02d}.{kind}"
    event = queue / name
    event.write_bytes(payload)
    event.chmod(0o600)
    for sink, content in (("github", github_ack), ("taskgraph", taskgraph_ack)):
        if content is not None:
            acknowledgement = queue / f"{name}.{sink}.done"
            acknowledgement.write_bytes(content)
            acknowledgement.chmod(0o644)
    return event


def plan(tmp_path: Path, queue: Path) -> tuple[Path, str, dict]:
    manifest = tmp_path / "manifest.json"
    digest, value = write_plan(queue, manifest)
    assert hashlib.sha256(manifest.read_bytes()).hexdigest() == digest
    return manifest, digest, value


def apply(tmp_path: Path, queue: Path, manifest: Path, digest: str) -> Path:
    return apply_recovery(
        queue=queue,
        manifest_path=manifest,
        expected_manifest_sha256=digest,
        archive_root=tmp_path / "archive",
    )


def test_plan_is_deterministic_and_records_every_required_identity(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    first = legacy(
        queue,
        1,
        "alarm",
        b"ACTION: host text says devbig014 and devbig030\n",
        github_ack=b"github acknowledgement bytes\n",
    )
    second = legacy(queue, 2, "resolve", b"")
    hidden = queue / ".event-20260906T000000000000000Z-99-staged"
    hidden.write_bytes(b"unattributed staged bytes")
    extra = queue / f"{first.name}.unrecognized.done"
    extra.write_bytes(b"extra sidecar bytes")

    manifest_a = tmp_path / "manifest-a.json"
    digest_a, value = write_plan(queue, manifest_a)
    manifest_b = tmp_path / "manifest-b.json"
    digest_b, _ = write_plan(queue, manifest_b)

    assert digest_a == digest_b
    assert manifest_a.read_bytes() == manifest_b.read_bytes()
    assert value["counts"] == {
        "acknowledgements_present": 1,
        "archive_members": 5,
        "legacy_alarms": 1,
        "legacy_events": 2,
        "legacy_resolves": 1,
        "orphan_acknowledgements": 0,
        "population_files": 6,
        "retained_members": 1,
    }
    entries = {item["filename"]: item for item in value["legacy_events"]}
    github = entries[first.name]["acknowledgements"]["github"]
    assert github["sidecar_state"] == "present"
    assert github["delivery_state"] == "acknowledged-by-sidecar"
    assert github["correlated_attempts"] == []
    assert github["evidence"]["filename"] == f"{first.name}.github.done"
    assert github["evidence"]["mode"] == "0644"
    assert (
        github["evidence"]["sha256"]
        == hashlib.sha256(b"github acknowledgement bytes\n").hexdigest()
    )
    assert github["evidence"]["size"] == 29
    taskgraph = entries[first.name]["acknowledgements"]["taskgraph"]
    assert taskgraph == {
        "correlated_attempts": [],
        "delivery_state": "uncertain",
        "expected_filename": f"{first.name}.taskgraph.done",
        "sidecar_state": "absent",
    }
    assert (
        entries[second.name]["acknowledgements"]["github"]["sidecar_state"] == "absent"
    )
    assert all(item["attribution"]["origin_host"] is None for item in entries.values())
    assert all(
        item["attribution"]["confidence"] == "unknown" for item in entries.values()
    )
    assert [item["ordinal"] for item in value["legacy_events"]] == [1, 2]
    assert entries[first.name]["line_count"] == 1
    assert entries[first.name]["findings_count"] == 1
    assert (
        entries[first.name]["legacy_normalized_sha256"]
        == hashlib.sha256(
            b"ACTION: host text says devbig014 and devbig030\n"
        ).hexdigest()
    )
    assert entries[first.name]["disposition"] == "archive-preserve-no-replay"
    assert entries[first.name]["replacement_event_id"] is None
    assert "devbig014" not in json.dumps(entries[first.name]["attribution"])
    artifacts = {item["filename"]: item for item in value["additional_artifacts"]}
    assert artifacts[hidden.name]["anomalies"] == ["incomplete-staging-artifact"]
    assert artifacts[extra.name]["anomalies"] == ["unrecognized-sidecar"]
    assert "no summary alarm is authorized" in value["policy"]["owner_decision"]


def test_apply_moves_and_readback_verifies_oversize_payload_and_ack(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    payload = b"ACTION: oversize evidence\n" + b"x" * 300_000
    event = legacy(queue, 1, "alarm", payload, github_ack=b"ack\x00bytes\n")
    manifest, digest, value = plan(tmp_path, queue)

    archive = apply(tmp_path, queue, manifest, digest)

    assert archive.name == digest
    assert sorted(path.name for path in queue.iterdir()) == [".worker.lock"]
    assert (archive / "manifest.json").read_bytes() == manifest.read_bytes()
    assert (archive / "evidence" / event.name).read_bytes() == payload
    assert (archive / "snapshots" / event.name).read_bytes() == payload
    acknowledgement = f"{event.name}.github.done"
    assert (archive / "evidence" / acknowledgement).read_bytes() == b"ack\x00bytes\n"
    assert (archive / "snapshots" / acknowledgement).read_bytes() == b"ack\x00bytes\n"
    assert json.loads((archive / "released.json").read_text())["state"] == "applied"
    assert value["counts"]["archive_members"] == 2

    # Losing the caller's success response does not move, duplicate, or publish
    # anything on a retry with the same exact hash.
    assert apply(tmp_path, queue, manifest, digest) == archive


@pytest.mark.parametrize(
    "tamper",
    [
        "missing-transaction",
        "corrupt-ready",
        "missing-control-sentinel",
        "symlink-release",
        "extra-audit-file",
    ],
)
def test_idempotent_apply_refuses_incomplete_or_tampered_archive_receipts(
    tmp_path: Path, tamper: str
) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    archive = apply(tmp_path, queue, manifest, digest)
    if tamper == "missing-transaction":
        (archive / "transaction.json").unlink()
    elif tamper == "corrupt-ready":
        (archive / "archive-ready.json").write_text("{}\n")
    elif tamper == "missing-control-sentinel":
        next((archive / "control").iterdir()).unlink()
    elif tamper == "symlink-release":
        (archive / "released.json").unlink()
        (archive / "released.json").symlink_to(archive / "manifest.json")
    else:
        (archive / "unexpected-audit-record").write_text("unexpected\n")

    with pytest.raises(RecoveryRefused):
        apply(tmp_path, queue, manifest, digest)


def test_final_archive_cannot_hide_simultaneous_unfinished_staging(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    apply(tmp_path, queue, manifest, digest)
    staging = tmp_path / "archive" / f".{digest}.applying"
    staging.mkdir()
    (staging / "unreviewed-evidence").write_bytes(b"must not be hidden")

    with pytest.raises(RecoveryRefused, match="coexist"):
        apply(tmp_path, queue, manifest, digest)

    assert (staging / "unreviewed-evidence").read_bytes() == b"must not be hidden"


def test_completed_archive_retry_allows_only_schema_v1_queue_progress(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    retained = write_event(
        queue,
        make_event(
            kind="alarm",
            payload="ACTION: retained\n",
            origin_host="devbig014.example.com",
            findings=1,
            event_id="event-20260920T010203123456Z-123-aaaaaaaa",
            created_at="2026-09-20T01:02:03Z",
        ),
    )
    manifest, digest, _ = plan(tmp_path, queue)
    archive = apply(tmp_path, queue, manifest, digest)
    retained.unlink()  # ordinary worker progress after the sentinel was released
    later = write_event(
        queue,
        make_event(
            kind="resolve",
            payload="",
            origin_host="devbig014.example.com",
            findings=0,
            event_id="event-20260920T010204123456Z-123-bbbbbbbb",
            created_at="2026-09-20T01:02:04Z",
        ),
    )

    assert apply(tmp_path, queue, manifest, digest) == archive
    assert later.exists()

    # A legacy blocker after release is not ordinary schema-v1 progress.
    restored = queue / "event-restored.alarm"
    restored.write_bytes(b"ACTION: restored legacy\n")
    with pytest.raises(RecoveryRefused, match="legacy evidence reappeared"):
        apply(tmp_path, queue, manifest, digest)


def test_parent_directories_are_fsynced_before_visibility_boundaries(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    archive_root = (tmp_path / "archive").resolve()
    final = archive_root / digest
    staging = archive_root / f".{digest}.applying"
    sentinel = queue / f"event-legacy-recovery-{digest[:16]}.alarm"
    ordering: list[tuple[str, ...]] = []
    real_fsync = recovery.fsync_directory
    real_replace = recovery.os.replace

    def record_fsync(path: Path) -> None:
        ordering.append(("fsync", str(path)))
        real_fsync(path)

    def record_phase(phase: str) -> None:
        ordering.append(("phase", phase))

    def record_replace(source: Path, destination: Path) -> None:
        ordering.append(("replace", str(source), str(destination)))
        real_replace(source, destination)

    monkeypatch.setattr(recovery, "fsync_directory", record_fsync)
    monkeypatch.setattr(recovery.os, "replace", record_replace)
    apply_recovery(
        queue=queue,
        manifest_path=manifest,
        expected_manifest_sha256=digest,
        archive_root=archive_root,
        phase_hook=record_phase,
    )

    sentinel_index = ordering.index(("phase", "sentinel-created"))
    staging_durable_index = ordering.index(("phase", "staging-durable"))
    assert ("fsync", str(archive_root)) in ordering[:staging_durable_index]
    assert staging_durable_index < sentinel_index
    control_durable_index = ordering.index(("phase", "control-durable"))
    archive_published_index = ordering.index(("phase", "archive-published"))
    assert archive_published_index < control_durable_index
    assert ("fsync", str(final)) in ordering[
        archive_published_index:control_durable_index
    ]

    evidence_move = ordering.index(
        ("replace", str(queue / event.name), str(staging / "evidence" / event.name))
    )
    evidence_sync = ordering.index(("fsync", str(staging / "evidence")), evidence_move)
    queue_sync = ordering.index(("fsync", str(queue)), evidence_move)
    assert evidence_move < evidence_sync < queue_sync

    sentinel_move = ordering.index(
        ("replace", str(sentinel), str(final / "control" / sentinel.name))
    )
    control_sync = ordering.index(("fsync", str(final / "control")), sentinel_move)
    release_queue_sync = ordering.index(("fsync", str(queue)), sentinel_move)
    assert sentinel_move < control_sync < release_queue_sync


def test_all_157_github_acknowledgements_are_byte_preserved(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    expected: dict[str, bytes] = {}
    for sequence in range(1, 158):
        acknowledgement = f"ack-{sequence:03d}\x00\n".encode()
        event = legacy(
            queue,
            sequence,
            "alarm",
            f"ACTION: {sequence}\n".encode(),
            github_ack=acknowledgement,
        )
        expected[f"{event.name}.github.done"] = acknowledgement
    manifest, digest, value = plan(tmp_path, queue)
    assert value["counts"]["acknowledgements_present"] == 157

    archive = apply(tmp_path, queue, manifest, digest)

    for name, content in expected.items():
        assert (archive / "evidence" / name).read_bytes() == content
        assert (archive / "snapshots" / name).read_bytes() == content


def test_source_log_prefix_is_hashed_and_attempt_lines_are_correlated(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: reviewed\n", github_ack=b"")
    source_log = tmp_path / "delivery.log"
    source_log.write_text(
        "2026-09-20T01:02:03Z "
        f"event={event} sink=github escalation_rc=0 findings=1 "
        "detail=escalate:\\ sink=github\\ outcome=published\\ "
        "key=operational-tick-findings\\ "
        "issue=https://github.com/rrnewton/dev-hermit/issues/350\\ digest=abc123\n"
    )
    manifest = tmp_path / "manifest.json"
    digest, value = write_plan(queue, manifest, source_logs=(source_log,))

    attempt = value["legacy_events"][0]["acknowledgements"]["github"][
        "correlated_attempts"
    ][0]
    assert attempt["timestamp"] == "2026-09-20T01:02:03Z"
    assert attempt["returncode"] == 0
    assert attempt["outcome"] == "published"
    assert attempt["issue_url"] == "https://github.com/rrnewton/dev-hermit/issues/350"
    assert attempt["source_line_offset"] == 0
    assert attempt["source_line_length"] == value["source_logs"][0]["size"]
    assert attempt["source_line_sha256"] == value["source_logs"][0]["sha256"]

    # An append cannot alter the reviewed prefix and therefore does not make a
    # valid archival plan impossible merely because the worker logged later.
    with source_log.open("a") as handle:
        handle.write("later unrelated line\n")
    archive = apply(tmp_path, queue, manifest, digest)
    assert archive.is_dir()
    archived_log = archive / "source-logs" / value["source_logs"][0]["archive_name"]
    assert (
        hashlib.sha256(archived_log.read_bytes()).hexdigest()
        == value["source_logs"][0]["sha256"]
    )
    assert archived_log.stat().st_size == value["source_logs"][0]["size"]


def test_source_log_prefix_mutation_is_refused(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    source_log = tmp_path / "delivery.log"
    source_log.write_bytes(b"reviewed log prefix\n")
    manifest = tmp_path / "manifest.json"
    digest, _ = write_plan(queue, manifest, source_logs=(source_log,))
    source_log.write_bytes(b"tampered log prefix\n")

    with pytest.raises(RecoveryRefused, match="source-log reviewed prefix changed"):
        apply(tmp_path, queue, manifest, digest)


def test_source_log_mutation_after_sentinel_rolls_back(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    source_log = tmp_path / "delivery.log"
    source_log.write_bytes(b"reviewed source prefix\n")
    manifest = tmp_path / "manifest.json"
    digest, _ = write_plan(queue, manifest, source_logs=(source_log,))

    def mutate(phase: str) -> None:
        if phase == "sentinel-created":
            source_log.write_bytes(b"mutated! source prefix\n")

    with pytest.raises(RecoveryRefused, match="source-log reviewed prefix changed"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=tmp_path / "archive",
            phase_hook=mutate,
        )

    assert event.read_bytes() == b"ACTION: reviewed\n"
    assert not any("legacy-recovery" in path.name for path in queue.iterdir())
    assert len(list((tmp_path / "archive").glob(f"{digest}.failed.*"))) == 1


@pytest.mark.parametrize("finalization_path", ["initial", "retry"])
def test_source_log_mutation_at_control_boundary_never_releases_queue(
    tmp_path: Path, finalization_path: str
) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    source_log = tmp_path / "delivery.log"
    source_log.write_bytes(b"reviewed source prefix\n")
    manifest = tmp_path / "manifest.json"
    digest, _ = write_plan(queue, manifest, source_logs=(source_log,))
    archive_root = tmp_path / "archive"

    if finalization_path == "retry":

        def stop_after_publication(phase: str) -> None:
            if phase == "archive-published":
                raise RecoveryRefused("simulated crash after archive publication")

        with pytest.raises(RecoveryRefused, match="simulated crash"):
            apply_recovery(
                queue=queue,
                manifest_path=manifest,
                expected_manifest_sha256=digest,
                archive_root=archive_root,
                phase_hook=stop_after_publication,
            )

    def mutate(phase: str) -> None:
        if phase == "control-durable":
            source_log.write_bytes(b"mutated! source prefix\n")

    with pytest.raises(RecoveryRefused, match="source-log reviewed prefix changed"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=archive_root,
            phase_hook=mutate,
        )

    assert len(list(queue.glob("event-legacy-recovery-*.alarm"))) == 1
    assert not (archive_root / digest / "released.json").exists()


@pytest.mark.parametrize("alias_kind", ["direct", "hardlink", "symlink"])
def test_manifest_output_cannot_alias_source_log(
    tmp_path: Path, alias_kind: str
) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    source_log = tmp_path / "delivery.log"
    original = b"the only source-log evidence\n"
    source_log.write_bytes(original)
    output = source_log
    if alias_kind == "hardlink":
        output = tmp_path / "manifest-hardlink.json"
        os.link(source_log, output)
    elif alias_kind == "symlink":
        output = tmp_path / "manifest-link.json"
        output.symlink_to(source_log)

    with pytest.raises(RecoveryRefused, match="must not alias a source log"):
        write_plan(queue, output, source_logs=(source_log,))

    assert source_log.read_bytes() == original


@pytest.mark.parametrize(
    "mutation", ["content", "retimestamp", "extra-sidecar", "missing-sidecar"]
)
def test_apply_refuses_every_plan_to_apply_drift(tmp_path: Path, mutation: str) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: before\n", github_ack=b"ack\n")
    sidecar = queue / f"{event.name}.github.done"
    manifest, digest, _ = plan(tmp_path, queue)
    if mutation == "content":
        event.write_bytes(b"ACTION: after\n")
    elif mutation == "retimestamp":
        metadata = event.stat()
        os.utime(event, ns=(metadata.st_atime_ns, metadata.st_mtime_ns + 1_000_000_000))
    elif mutation == "extra-sidecar":
        (queue / f"{event.name}.taskgraph.done").write_bytes(b"late ack\n")
    else:
        sidecar.unlink()

    with pytest.raises(RecoveryRefused, match="differs from the reviewed manifest"):
        apply(tmp_path, queue, manifest, digest)

    assert not (tmp_path / "archive" / digest).exists()
    assert not any("legacy-recovery" in path.name for path in queue.iterdir())


def test_mutation_during_apply_rolls_back_exact_bytes_and_preserves_mutant(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    event = legacy(
        queue, 1, "alarm", b"ACTION: reviewed\n", github_ack=b"ack-reviewed\n"
    )
    manifest, digest, _ = plan(tmp_path, queue)

    def mutate(phase: str) -> None:
        if phase == "snapshots-complete":
            event.write_bytes(b"ACTION: raced mutation\n")

    with pytest.raises(RecoveryRefused, match="automatic rollback succeeded"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=tmp_path / "archive",
            phase_hook=mutate,
        )

    assert event.read_bytes() == b"ACTION: reviewed\n"
    assert (queue / f"{event.name}.github.done").read_bytes() == b"ack-reviewed\n"
    assert not any("legacy-recovery" in path.name for path in queue.iterdir())
    failures = list((tmp_path / "archive").glob(f"{digest}.failed.*"))
    assert len(failures) == 1
    assert (failures[0] / "mutated-during-apply" / event.name).read_bytes() == (
        b"ACTION: raced mutation\n"
    )
    assert (
        failures[0] / "snapshots" / event.name
    ).read_bytes() == b"ACTION: reviewed\n"


def test_dangling_symlink_race_is_preserved_while_reviewed_bytes_are_restored(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    raced_target = "missing-raced-target"

    def mutate(phase: str) -> None:
        if phase == "snapshots-complete":
            event.unlink()
            event.symlink_to(raced_target)

    with pytest.raises(RecoveryRefused, match="automatic rollback succeeded"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=tmp_path / "archive",
            phase_hook=mutate,
        )

    assert event.is_file() and not event.is_symlink()
    assert event.read_bytes() == b"ACTION: reviewed\n"
    failure = next((tmp_path / "archive").glob(f"{digest}.failed.*"))
    preserved = failure / "mutated-during-apply" / event.name
    assert preserved.is_symlink()
    assert os.readlink(preserved) == raced_target


def test_rollback_fsyncs_nested_audit_state_before_failed_archive_rename(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    archive_root = (tmp_path / "archive").resolve()
    staging = archive_root / f".{digest}.applying"
    ordering: list[tuple[str, ...]] = []
    real_fsync = recovery.fsync_directory
    real_replace = recovery.os.replace

    def record_fsync(path: Path) -> None:
        ordering.append(("fsync", str(path)))
        real_fsync(path)

    def record_replace(source: Path, destination: Path) -> None:
        ordering.append(("replace", str(source), str(destination)))
        real_replace(source, destination)

    def mutate(phase: str) -> None:
        if phase == "snapshots-complete":
            event.write_bytes(b"ACTION: raced mutation\n")

    monkeypatch.setattr(recovery, "fsync_directory", record_fsync)
    monkeypatch.setattr(recovery.os, "replace", record_replace)
    with pytest.raises(RecoveryRefused, match="automatic rollback succeeded"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=archive_root,
            phase_hook=mutate,
        )

    failure_move = next(
        index
        for index, operation in enumerate(ordering)
        if operation[0] == "replace"
        and operation[1] == str(staging)
        and f"{digest}.failed." in operation[2]
    )
    required = [
        staging / "control",
        staging / "mutated-during-apply",
        queue,
        staging / "evidence",
        staging / "snapshots",
        staging,
    ]
    positions = {
        path: max(
            index
            for index, operation in enumerate(ordering[:failure_move])
            if operation == ("fsync", str(path))
        )
        for path in required
    }
    assert positions[staging / "control"] < positions[queue]
    assert positions[staging / "mutated-during-apply"] < positions[queue]
    assert positions[queue] < positions[staging / "evidence"]


def test_population_race_during_apply_rolls_back_without_losing_new_file(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    event = legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    raced = queue / ".event-raced-staging"

    def add_file(phase: str) -> None:
        if phase == f"moved:{event.name}":
            raced.write_bytes(b"new concurrent bytes")

    with pytest.raises(RecoveryRefused, match="automatic rollback succeeded"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=tmp_path / "archive",
            phase_hook=add_file,
        )

    assert event.read_bytes() == b"ACTION: reviewed\n"
    assert raced.read_bytes() == b"new concurrent bytes"
    assert not any("legacy-recovery" in path.name for path in queue.iterdir())


def test_population_race_after_archive_publication_keeps_release_sentinel(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    raced = queue / ".event-raced-after-publication"

    def add_file(phase: str) -> None:
        if phase == "archive-published":
            raced.write_bytes(b"late concurrent bytes")

    with pytest.raises(RecoveryRefused, match="after archive publication"):
        apply_recovery(
            queue=queue,
            manifest_path=manifest,
            expected_manifest_sha256=digest,
            archive_root=tmp_path / "archive",
            phase_hook=add_file,
        )

    assert raced.read_bytes() == b"late concurrent bytes"
    assert len(list(queue.glob("event-legacy-recovery-*.alarm"))) == 1
    assert (tmp_path / "archive" / digest).is_dir()


def test_sentinel_prevents_schema_v1_overtaking_until_archive_is_complete(
    tmp_path: Path,
) -> None:
    queue = make_queue(tmp_path)
    old = legacy(queue, 1, "alarm", b"ACTION: legacy\n")
    new = write_event(
        queue,
        make_event(
            kind="alarm",
            payload="ACTION: schema-v1\n",
            origin_host="devbig014.example.com",
            findings=1,
            event_id="event-20260920T010203123456Z-123-aaaaaaaa",
            created_at="2026-09-20T01:02:03Z",
        ),
    )
    manifest, digest, _ = plan(tmp_path, queue)
    observed: list[subprocess.CompletedProcess[str]] = []

    def try_runner(phase: str) -> None:
        if phase == f"moved:{old.name}":
            observed.append(
                subprocess.run(
                    [str(RUNNER), "--queue", str(queue)],
                    text=True,
                    capture_output=True,
                    check=False,
                )
            )

    archive = apply_recovery(
        queue=queue,
        manifest_path=manifest,
        expected_manifest_sha256=digest,
        archive_root=tmp_path / "archive",
        phase_hook=try_runner,
    )

    assert archive.is_dir()
    assert len(observed) == 1
    assert observed[0].returncode == 2
    assert "legacy payload-only" in observed[0].stderr
    assert new.exists(), "schema-v1 event must not overtake the reviewed legacy archive"
    assert not old.exists()


def test_enqueue_waits_on_the_same_population_lock(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    payload = tmp_path / "payload"
    payload.write_text("ACTION: locked enqueue\n")
    lock = (queue / ".worker.lock").open("r+")
    fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
    process = subprocess.Popen(
        [
            str(ENQUEUE),
            "--queue",
            str(queue),
            "--kind",
            "alarm",
            "--payload-file",
            str(payload),
            "--origin-host",
            "devbig014.example.com",
            "--findings",
            "1",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        time.sleep(0.15)
        assert process.poll() is None
    finally:
        fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        lock.close()
    stdout, stderr = process.communicate(timeout=5)
    assert process.returncode == 0, stdout + stderr
    assert len(list(queue.glob("event-*.json"))) == 1


def test_recovery_sentinel_blocks_schema_v1_enqueue(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    sentinel = queue / "event-legacy-recovery-0123456789abcdef.alarm"
    sentinel.write_text("hash-bound recovery sentinel\n")
    payload = tmp_path / "payload"
    payload.write_text("ACTION: must wait\n")

    completed = subprocess.run(
        [
            str(ENQUEUE),
            "--queue",
            str(queue),
            "--kind",
            "alarm",
            "--payload-file",
            str(payload),
            "--origin-host",
            "devbig014.example.com",
            "--findings",
            "1",
        ],
        text=True,
        capture_output=True,
        check=False,
    )

    assert completed.returncode == 2
    assert "recovery sentinel blocks enqueue" in completed.stderr
    assert sentinel.read_text() == "hash-bound recovery sentinel\n"
    assert list(queue.glob("event-*.json")) == []


def test_wrong_manifest_hash_and_nonregular_member_are_refused(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    manifest, digest, _ = plan(tmp_path, queue)
    with pytest.raises(RecoveryRefused, match="manifest SHA-256 mismatch"):
        apply(tmp_path, queue, manifest, "0" * 64)

    (queue / "event-symlink.alarm").symlink_to(manifest)
    with pytest.raises(RecoveryRefused, match="not a regular file"):
        write_plan(queue, tmp_path / "replan.json")
    assert digest != "0" * 64


def test_orphan_acknowledgement_is_an_anomaly_and_is_preserved(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    orphan = queue / "event-missing.alarm.github.done"
    orphan.write_bytes(b"orphan ack evidence\n")
    manifest, digest, value = plan(tmp_path, queue)

    assert value["counts"]["acknowledgements_present"] == 0
    assert value["counts"]["orphan_acknowledgements"] == 1
    artifact = next(
        item
        for item in value["additional_artifacts"]
        if item["filename"] == orphan.name
    )
    assert artifact["anomalies"] == ["acknowledgement-without-primary"]
    archive = apply(tmp_path, queue, manifest, digest)
    assert (archive / "evidence" / orphan.name).read_bytes() == b"orphan ack evidence\n"


def test_malformed_schema_v1_event_is_not_silently_archived(tmp_path: Path) -> None:
    queue = make_queue(tmp_path)
    legacy(queue, 1, "alarm", b"ACTION: reviewed\n")
    malformed = queue / "event-20260920T010203123456Z-123-aaaaaaaa.json"
    malformed.write_text("{}\n")

    with pytest.raises(RecoveryRefused, match="schema-v1 event is invalid"):
        write_plan(queue, tmp_path / "manifest.json")
