import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import validate_rows  # noqa: E402


def row(root: Path, *, record_id: str = "host-1-7", host: str = "hosta") -> dict:
    return {
        "schema_version": 5,
        "record_id": record_id,
        "repo": "hermit",
        "producer": "hermit-validate-rs",
        "host": host,
        "started_at": "2026-08-09T01:00:00Z",
        "finished_at": "2026-08-09T01:01:00Z",
        "result": "pass",
        "commit": "a" * 40,
        "cwd": str(root / "worktrees" / "slot" / "hermit"),
    }


def test_record_is_immediately_visible_through_canonical_union(tmp_path: Path):
    source = row(tmp_path)
    report = validate_rows.record(source, root=tmp_path)

    assert report["spool"].startswith(
        "ignored/ci-hub/validate-ledger-spool/hermit__hosta__2026-08__"
    )
    assert report["published_shard"] == "ledger/hermit/hosta/2026-08.jsonl"
    assert not (tmp_path / report["published_shard"]).exists()
    assert validate_rows.rows(root=tmp_path) == [source]


def test_canonical_reverie_repo_uses_top_level_reverie_shard(tmp_path: Path):
    source = row(tmp_path)
    source["repo"] = "rrnewton/reverie"
    source["cwd"] = str(tmp_path / "worktrees" / "slot" / "reverie")

    report = validate_rows.record(source, root=tmp_path)

    assert report["spool"].startswith(
        "ignored/ci-hub/validate-ledger-spool/reverie__hosta__2026-08__"
    )
    assert report["published_shard"] == "ledger/reverie/hosta/2026-08.jsonl"
    assert validate_rows.rows(root=tmp_path) == [source]


def test_unmapped_path_bearing_repo_is_refused_before_spool(tmp_path: Path):
    source = row(tmp_path)
    source["repo"] = "unknown/repository"

    with pytest.raises(ValueError, match="path-safe ledger team mapping"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_repeated_record_of_same_event_unions_once(tmp_path: Path):
    source = row(tmp_path)
    validate_rows.record(source, root=tmp_path)
    validate_rows.record(source, root=tmp_path)

    assert len(validate_rows.events(root=tmp_path)) == 1
    assert validate_rows.rows(root=tmp_path) == [source]


def test_every_published_machine_is_implicitly_unioned(tmp_path: Path):
    first = row(tmp_path, record_id="hosta-1-7", host="hosta")
    second = row(tmp_path, record_id="hostb-1-8", host="hostb")
    for source in (first, second):
        event = validate_rows._event_for_row(source, tmp_path)
        shard = (
            tmp_path
            / "ledger"
            / "hermit"
            / source["host"]
            / "2026-08.jsonl"
        )
        shard.parent.mkdir(parents=True)
        shard.write_text(json.dumps(event, sort_keys=True) + "\n")

    assert {item["host"] for item in validate_rows.rows(root=tmp_path)} == {
        "hosta",
        "hostb",
    }


def test_event_visible_in_spool_and_published_shard_collapses_to_one(tmp_path: Path):
    source = row(tmp_path)
    validate_rows.record(source, root=tmp_path)
    event = validate_rows._event_for_row(source, tmp_path)
    shard = tmp_path / "ledger/hermit/hosta/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_text(json.dumps(event, sort_keys=True) + "\n")

    assert [item["event_id"] for item in validate_rows.events(root=tmp_path)] == [
        source["record_id"]
    ]
    assert validate_rows.rows(root=tmp_path) == [source]


def test_recovery_shard_is_part_of_the_same_canonical_union(tmp_path: Path):
    first = row(tmp_path, record_id="primary")
    second = row(tmp_path, record_id="preserved")
    primary = tmp_path / "ledger/hermit/hosta/2026-08.jsonl"
    recovery = tmp_path / "ledger-recovery/hermit/hosta/2026-08/base.jsonl"
    primary.parent.mkdir(parents=True)
    recovery.parent.mkdir(parents=True)
    primary.write_text(json.dumps(validate_rows._event_for_row(first, tmp_path), sort_keys=True) + "\n")
    recovery.write_text(
        json.dumps(validate_rows._event_for_row(first, tmp_path), sort_keys=True)
        + "\n"
        + json.dumps(validate_rows._event_for_row(second, tmp_path), sort_keys=True)
        + "\n"
    )

    assert {event["event_id"] for event in validate_rows.events(root=tmp_path)} == {
        "primary",
        "preserved",
    }


def acceptance_fixture(root: Path) -> dict:
    source = row(root, record_id="accept-0000")
    source["producer"] = "ledger-live-sync-acceptance"
    return source


def test_acceptance_fixture_is_not_canonical_but_real_validation_remains(
    tmp_path: Path,
):
    """Fixture rejection must not silently empty the real population."""
    fixture = acceptance_fixture(tmp_path)
    real = row(tmp_path, record_id="real-validation")
    shard = tmp_path / "ledger/hermit/hosta/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_text(
        json.dumps(validate_rows._event_for_row(fixture, tmp_path), sort_keys=True)
        + "\n"
        + json.dumps(validate_rows._event_for_row(real, tmp_path), sort_keys=True)
        + "\n"
    )

    # Preserve the append-only bytes.  The authority view, not history, makes
    # the distinction between acceptance data and a validation observation.
    assert len(shard.read_text().splitlines()) == 2
    assert [event["event_id"] for event in validate_rows.events(root=tmp_path)] == [
        "real-validation"
    ]
    assert validate_rows.rows(root=tmp_path) == [real]


def test_production_record_refuses_acceptance_fixture_before_spool(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(validate_rows, "repository_root", lambda: tmp_path)

    with pytest.raises(ValueError, match="acceptance fixture.*production ledger"):
        validate_rows.record(acceptance_fixture(tmp_path), root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_isolated_fixture_store_does_not_enter_canonical_rows(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    production = tmp_path / "production"
    isolated = tmp_path / "fixture-store"
    monkeypatch.setattr(validate_rows, "repository_root", lambda: production)

    report = validate_rows.record(acceptance_fixture(isolated), root=isolated)

    assert (isolated / report["spool"]).is_file()
    assert validate_rows.events(root=isolated) == []
    assert validate_rows.rows(root=isolated) == []


def test_ignored_ephemeral_checkout_is_redacted_without_weakening_guard(
    tmp_path: Path,
):
    source = row(tmp_path)
    source["cwd"] = "/home/test/work/dev-hermit/ignored/validate-fresh-abc"
    source["first_error_line"] = "/home/test/.cargo/git/checkouts/reverie/file.rs:7"

    event = validate_rows._event_for_row(source, tmp_path)

    assert event["legacy_row"]["cwd"] == (
        "{{WORKSPACE_ROOT}}/ignored/validate-fresh-abc"
    )
    assert event["legacy_row"]["first_error_line"] == (
        "{{HOME}}/.cargo/git/checkouts/reverie/file.rs:7"
    )
    assert "/home/" not in json.dumps(event)


@pytest.mark.parametrize("field", ["finished_at"])
def test_incomplete_row_is_refused_without_creating_an_alternate_ledger(
    tmp_path: Path, field: str
):
    source = row(tmp_path)
    del source[field]
    with pytest.raises(ValueError):
        validate_rows.record(source, root=tmp_path)
    assert not (tmp_path / "ledger").exists()
