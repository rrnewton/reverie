"""Brackets for the one-shot historical validate identity bridge.

Every case runs the real tool against a fixture tree and a fixture status
snapshot. Nothing reaches the live registry, and nothing removes storage.

WHAT THESE ARE FOR. The bridge writes into run records that the cleanup consumer
then acts on, so the cases that matter most are the REFUSALS: an identity bound
to the wrong run, or bound on the strength of a plan made about different bytes,
would make a removal decision on the binding rather than on the evidence.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
TOOL = ROOT / "ci-hub/validate/migrate_run_identities.py"


def write_snapshot(root: Path, rows: list[dict]) -> tuple[Path, str]:
    path = root / "snapshot.json"
    path.write_text(json.dumps({"active": rows}))
    return path, hashlib.sha256(path.read_bytes()).hexdigest()


def row(slot: str, *, slot_type: str = "validate", generation: int = 1, inconsistencies=()) -> dict:
    return {
        "slot": slot,
        "slot_type": slot_type,
        "generation": generation,
        "storage_inconsistencies": list(inconsistencies),
        "checkouts": [{"path": f"worktrees/validate/{slot}"}],
    }


def write_record(root: Path, slot: str, **overrides) -> Path:
    checkout = root / "worktrees/validate" / slot
    checkout.mkdir(parents=True, exist_ok=True)
    record = {
        "schema_version": 1,
        "unit": f"{slot}.service",
        "checkout": str(checkout),
        "state": "completed",
        "temporary_checkout": True,
    }
    record.update(overrides)
    path = root / "ignored/validate/runs" / f"{slot}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(record))
    return path


def run(root: Path, snapshot: Path, sha: str, *args: str) -> dict:
    completed = subprocess.run(
        [
            sys.executable, str(TOOL),
            "--root", str(root),
            "--snapshot", str(snapshot),
            "--snapshot-sha256", sha,
            "--json", *args,
        ],
        capture_output=True, text=True, timeout=300,
    )
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


@pytest.fixture()
def bench(tmp_path: Path):
    return tmp_path


def test_a_dry_run_writes_nothing(bench: Path) -> None:
    record = write_record(bench, "validate-fresh-a")
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])
    before = record.read_bytes()

    report = run(bench, snapshot, sha)

    assert report["mode"] == "dry-run"
    assert report["eligible"] == 1
    assert report["bound"] == 0
    assert record.read_bytes() == before, "a dry run must not touch the record"


def test_binding_is_exact_and_idempotent(bench: Path) -> None:
    record = write_record(bench, "validate-fresh-a")
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a", generation=3)])

    first = run(bench, snapshot, sha, "--apply")
    assert first["bound"] == 1
    stored = json.loads(record.read_text())
    assert stored["wrkslots_slot"] == "validate-fresh-a"
    assert stored["wrkslots_generation"] == 3

    second = run(bench, snapshot, sha, "--apply")
    assert second["bound"] == 0
    assert second["exact_present"] == 1, "replay must be a no-op, not a rewrite"


def test_a_record_that_changed_after_the_plan_is_not_overwritten(bench: Path) -> None:
    """The reason the plan carries record hashes. Binding on a plan made about
    different bytes would attach an identity chosen for another record."""
    record = write_record(bench, "validate-fresh-a")
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])
    plan = bench / "plan.json"

    run(bench, snapshot, sha, "--plan-out", str(plan))

    changed = json.loads(record.read_text())
    changed["note"] = "another agent touched this"
    record.write_text(json.dumps(changed))

    report = run(bench, snapshot, sha, "--plan-in", str(plan), "--apply")

    assert report["bound"] == 0
    assert "record changed after the plan was made" in json.dumps(report["applied"])
    assert "wrkslots_slot" not in json.loads(record.read_text())


def test_a_run_that_is_not_proven_finished_is_retained(bench: Path) -> None:
    """An identity is what makes a record eligible for removal, so granting one
    to a run nobody can prove finished would decide the removal on the binding."""
    record = write_record(bench, "validate-fresh-a", state="unknown")
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert report["retained"] == 1
    assert "not proven finished" in json.dumps(report["plan"])
    assert "wrkslots_slot" not in json.loads(record.read_text())


def test_a_row_that_is_not_a_validation_slot_is_retained(bench: Path) -> None:
    write_record(bench, "validate-fresh-a")
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a", slot_type="agent")])

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert "not validate" in json.dumps(report["plan"])


def test_a_row_with_storage_inconsistencies_is_retained(bench: Path) -> None:
    write_record(bench, "validate-fresh-a")
    snapshot, sha = write_snapshot(
        bench, [row("validate-fresh-a", inconsistencies=["row-without-directory"])]
    )

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert "storage inconsistencies" in json.dumps(report["plan"])


def test_a_non_temporary_checkout_is_retained(bench: Path) -> None:
    """Only disposable validation checkouts are in scope. A checkout that is not
    marked temporary is somebody's, and this bridge must not make it removable."""
    write_record(bench, "validate-fresh-a", temporary_checkout=False)
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert "temporary_checkout is not exactly true" in json.dumps(report["plan"])


def test_an_aliased_checkout_is_retained(bench: Path) -> None:
    """Two rows naming one physical checkout is ambiguous, and picking either
    would bind an identity the tool cannot show is the right one."""
    write_record(bench, "validate-fresh-a")
    duplicate = row("validate-fresh-a")
    duplicate["generation"] = 2
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a"), duplicate])

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert "2 snapshot rows" in json.dumps(report["plan"])


def test_a_conflicting_identity_already_present_is_retained(bench: Path) -> None:
    record = write_record(
        bench, "validate-fresh-a", wrkslots_slot="somewhere-else", wrkslots_generation=9
    )
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert "conflicts with the snapshot" in json.dumps(report["plan"])
    assert json.loads(record.read_text())["wrkslots_slot"] == "somewhere-else"


def test_a_partial_identity_is_retained(bench: Path) -> None:
    write_record(bench, "validate-fresh-a", wrkslots_slot="validate-fresh-a")
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])

    report = run(bench, snapshot, sha, "--apply")

    assert report["bound"] == 0
    assert "partial identity" in json.dumps(report["plan"])


def test_the_batch_is_bounded_and_reports_what_remains(bench: Path) -> None:
    rows = []
    for index in range(11):
        slot = f"validate-fresh-{index:02d}"
        write_record(bench, slot)
        rows.append(row(slot))
    snapshot, sha = write_snapshot(bench, rows)

    first = run(bench, snapshot, sha, "--apply")
    assert first["batch"] == 8
    assert first["bound"] == 8
    assert first["remaining_after_batch"] == 3

    second = run(bench, snapshot, sha, "--apply")
    assert second["bound"] == 3
    assert second["exact_present"] == 8
    assert second["remaining_after_batch"] == 0


def test_a_snapshot_that_is_not_the_planned_one_refuses(bench: Path) -> None:
    write_record(bench, "validate-fresh-a")
    snapshot, _sha = write_snapshot(bench, [row("validate-fresh-a")])
    completed = subprocess.run(
        [
            sys.executable, str(TOOL), "--root", str(bench),
            "--snapshot", str(snapshot), "--snapshot-sha256", "0" * 64, "--apply",
        ],
        capture_output=True, text=True, timeout=120,
    )
    assert completed.returncode != 0
    assert "not the same read" in completed.stderr


def test_a_missing_checkout_is_not_a_candidate(bench: Path) -> None:
    """Storage must be present. A record whose checkout is gone is the absent-row
    case and belongs to the recovery commands, not to this bridge."""
    record = write_record(bench, "validate-fresh-a")
    (bench / "worktrees/validate/validate-fresh-a").rmdir()
    snapshot, sha = write_snapshot(bench, [row("validate-fresh-a")])

    report = run(bench, snapshot, sha, "--apply")

    assert report["groups"] == 0
    assert "wrkslots_slot" not in json.loads(record.read_text())
