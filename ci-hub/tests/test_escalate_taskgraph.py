"""Brackets for the queue sink in ci-hub/bin/escalate.

These run the real `tg` against a real database in a temporary directory, so the
notes really are stored and read back and the refusals really are the ones the
installed tooling issues. Nothing reaches GitHub, and nothing reaches a shared
database.

⚠️ THE TEMPORARY DATABASE IS ADDRESSED BY PATH ON PURPOSE. `tg db create` writes
the new name into ~/.tg/active_db and prints "Set as active database", which
repoints EVERY headless caller on the box at once -- measured 2026-09-03 when
this work tripped over it. Addressing a file under tmp_path leaves that marker
untouched.

WHAT THESE ARE FOR. The hazard specific to a queue sink is not noise, it is
DUPLICATE ROWS: an alarm that files the same task every cycle also splits the
ownership picture the queue exists to hold. `tg add` does not deduplicate -- it
appends a suffixed second row, silently -- so most of what follows is about the
sink refusing to let that happen.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
ESCALATE = ROOT / "ci-hub/bin/escalate"
TG = shutil.which("tg")

pytestmark = pytest.mark.skipif(TG is None, reason="tg is not installed on this host")

GOAL = "root_goal"


def tg_run(db: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        [TG, *args],
        capture_output=True,
        text=True,
        env={**os.environ, "TG_DB_PATH": str(db)},
        timeout=180,
    )
    if check:
        assert completed.returncode == 0, completed.stderr or completed.stdout
    return completed


def tg_rows(db: Path, query: str) -> list[str]:
    """Rows from `tg sql`, with its header, rule and "(N rows)" trailer removed."""
    out = tg_run(db, "sql", query).stdout.splitlines()
    return [
        line.strip()
        for line in out[2:]
        if line.strip() and not line.strip().startswith("(")
    ]


@pytest.fixture()
def bench(tmp_path: Path):
    db = tmp_path / "queue.db"
    tg_run(db, "add", "Root goal", "--impact", "90", "--effort", "5", "--tags", "goal")
    log = tmp_path / "escalation.log"

    def run(*args: str, message: str | None = None):
        return subprocess.run(
            [
                sys.executable,
                str(ESCALATE),
                "--sink",
                "taskgraph",
                "--tg-db",
                str(db),
                "--tg-blocks",
                GOAL,
                "--log-file",
                str(log),
                *args,
            ],
            input=message,
            capture_output=True,
            text=True,
            env={**os.environ, "DG_AGENT_NAME": "health-tick"},
            timeout=300,
        )

    def rows_for(key: str) -> list[str]:
        return tg_rows(
            db,
            "SELECT local_id FROM tasks WHERE description LIKE "
            f"'%escalation-key: {key}%'",
        )

    def notes_on(local_id: str) -> list[str]:
        return tg_rows(
            db, f"SELECT id FROM task_notes WHERE task_id='{local_id}'"
        )

    def records() -> list[dict]:
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text().splitlines() if line.strip()]

    return run, db, rows_for, notes_on, records


def test_a_first_alarm_files_exactly_one_unowned_row(bench) -> None:
    run, db, rows_for, _notes, records = bench
    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    assert completed.returncode == 0, completed.stderr
    assert "sink=taskgraph outcome=published" in completed.stdout
    rows = rows_for("gate-failed")
    assert len(rows) == 1
    owner = tg_rows(db, f"SELECT COALESCE(owner,'<none>') FROM tasks WHERE local_id='{rows[0]}'")
    assert owner == ["<none>"], "an alarm nobody has picked up must stay claimable"
    assert records()[-1]["outcome"] == "published"


def test_the_row_is_attached_to_the_goal_so_the_queue_can_see_it(bench) -> None:
    run, db, rows_for, _notes, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    row = rows_for("gate-failed")[0]
    edges = tg_rows(db, f"SELECT to_task FROM task_edges WHERE from_task='{row}'")
    assert GOAL in edges, edges


def test_repeating_the_same_alarm_never_files_a_second_row(bench) -> None:
    """The hazard this sink is most exposed to. `tg add` appends a suffixed row
    rather than refusing, so the sink has to look the row up itself."""
    run, _db, rows_for, notes_on, _records = bench
    for _ in range(3):
        run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    rows = rows_for("gate-failed")
    assert len(rows) == 1, rows
    assert len(notes_on(rows[0])) == 1, "the repeats must add nothing, not even a note"


def test_an_unchanged_alarm_reports_unchanged_and_writes_nothing(bench) -> None:
    run, _db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    assert "outcome=unchanged" in completed.stdout
    assert len(notes_on(rows_for("gate-failed")[0])) == 1


def test_changed_content_appends_to_the_same_row(bench) -> None:
    run, _db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: first")
    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: second")
    assert "outcome=published" in completed.stdout
    rows = rows_for("gate-failed")
    assert len(rows) == 1
    assert len(notes_on(rows[0])) == 2


def test_a_persistent_alarm_says_so_again_after_the_repeat_window(bench) -> None:
    """Quiet is right for an hour and wrong for a week: a row nobody has touched
    in a month reads as history rather than as a live condition."""
    run, _db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    # The window is compared against the stored note time, so the two writes have
    # to be further apart than the window for this to measure anything.
    time.sleep(2)
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", "--repeat-after", "1",
        message="ACTION: x",
    )
    assert "outcome=published" in completed.stdout
    assert len(notes_on(rows_for("gate-failed")[0])) == 2


def test_repeat_after_zero_stays_quiet_forever(bench) -> None:
    run, _db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", "--repeat-after", "0",
        message="ACTION: x",
    )
    assert "outcome=unchanged" in completed.stdout
    assert len(notes_on(rows_for("gate-failed")[0])) == 1


def test_a_claimed_row_is_still_the_same_row(bench) -> None:
    """IN_PROGRESS counts as open. Filing a second row while an agent is working
    the first would duplicate the condition AND split its ownership."""
    run, db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    row = rows_for("gate-failed")[0]
    tg_run(db, "update", row, "--status", "in_progress", "--owner", "someone")
    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: changed")
    assert "outcome=published" in completed.stdout
    assert rows_for("gate-failed") == [row]
    assert len(notes_on(row)) == 2


def test_two_open_rows_for_one_key_refuse_rather_than_guess(bench) -> None:
    run, db, rows_for, _notes, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    row = rows_for("gate-failed")[0]
    description = tg_run(
        db, "sql", f"SELECT description FROM tasks WHERE local_id='{row}'"
    ).stdout
    assert "escalation-key: gate-failed" in description
    # A second row carrying the same key, as `tg add` would have produced.
    tg_run(
        db, "add", "A gate failed", "--impact", "60", "--effort", "1",
        "--tags", "monitoring", "--blocks", GOAL,
        "--description", "escalation-key: gate-failed",
    )
    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: y")
    assert completed.returncode == 1
    assert "refusing to guess" in completed.stderr
    assert "ownership picture" in completed.stderr


def test_resolving_closes_the_row_through_the_verifier(bench) -> None:
    run, db, rows_for, _notes, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    row = rows_for("gate-failed")[0]
    completed = run("--key", "gate-failed", "--resolve")
    assert completed.returncode == 0, completed.stderr
    assert "outcome=resolved" in completed.stdout
    status = tg_rows(db, f"SELECT status FROM tasks WHERE local_id='{row}'")
    assert status == ["CLOSED"], status


def test_resolving_with_nothing_open_is_not_an_error(bench) -> None:
    run, _db, _rows, _notes, _records = bench
    completed = run("--key", "gate-failed", "--resolve")
    assert completed.returncode == 0
    assert "outcome=nothing-open" in completed.stdout


def test_a_condition_that_returns_files_a_fresh_row(bench) -> None:
    """A settled alarm stays settled. Reviving a closed row would make the queue
    say the condition never stopped, and lose that it did."""
    run, db, rows_for, _notes, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    first = rows_for("gate-failed")[0]
    run("--key", "gate-failed", "--resolve")
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: back")

    all_rows = rows_for("gate-failed")
    assert len(all_rows) == 2, all_rows
    open_rows = tg_rows(
        db,
        "SELECT local_id FROM tasks WHERE status IN ('OPEN','IN_PROGRESS') "
        "AND description LIKE '%escalation-key: gate-failed%'",
    )
    assert len(open_rows) == 1, open_rows
    assert first not in open_rows, "the settled row must stay closed"


def test_an_absent_database_refuses_instead_of_inheriting_one(bench, tmp_path: Path) -> None:
    """The measured trap: under systemd, `tg` resolves a DIFFERENT database from
    the one coordinators read. Falling back would file the alarm where nobody
    looks, which is quieter than failing and much worse."""
    run, _db, _rows, _notes, _records = bench
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed",
        "--tg-db", str(tmp_path / "does-not-exist.db"),
        message="ACTION: x",
    )
    assert completed.returncode == 1
    assert "does not exist" in completed.stderr
    assert "refusing to file into" in completed.stderr


def test_an_unreachable_queue_is_never_reported_as_published(bench, tmp_path: Path) -> None:
    run, _db, _rows, _notes, records = bench
    broken = tmp_path / "tg-broken"
    broken.write_text("#!/bin/sh\necho 'tg exploded' >&2\nexit 1\n")
    broken.chmod(0o755)
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", "--tg", str(broken),
        message="ACTION: x",
    )
    assert completed.returncode == 1
    assert "outcome=recorded-locally-only" in completed.stderr
    assert "outcome=published" not in completed.stdout
    outcomes = [r["outcome"] for r in records()]
    assert "recorded" in outcomes, "the alarm reaches disk before anything can fail"
