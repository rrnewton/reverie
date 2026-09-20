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
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
ESCALATE = ROOT / "ci-hub/bin/escalate"
TG = shutil.which("tg")
sys.path.insert(0, str(ROOT / "ci-hub/lib"))
from health_escalation_event import (  # noqa: E402
    PART_BEGIN,
    make_event,
    parse_part,
    stable_task_id,
    write_event,
)

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
        marker = f"escalation-key: {key}"
        return tg_rows(
            db,
            "SELECT local_id FROM tasks WHERE "
            f"instr(description, '{marker}\n') > 0 OR "
            f"substr(description, -{len(marker)}) = '{marker}'",
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


def test_changed_content_updates_a_backlog_row_without_filing_another(bench) -> None:
    """The production database creates alarm rows as BACKLOG. Missing that
    active state was the measured cause of one new row per tick."""
    run, db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: first")
    row = rows_for("gate-failed")[0]
    tg_run(db, "update", row, "--status", "backlog")

    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", message="ACTION: second"
    )

    assert completed.returncode == 0, completed.stderr
    assert "outcome=published" in completed.stdout
    assert rows_for("gate-failed") == [row]
    assert len(notes_on(row)) == 2


def test_a_distinct_condition_files_its_own_row(bench) -> None:
    run, _db, rows_for, notes_on, _records = bench
    # File the longer key first: a substring lookup for the shorter key would
    # incorrectly reuse this row instead of filing a distinct condition.
    run(
        "--key", "gate-failed-detailed", "--title", "A detailed gate failed",
        message="ACTION: detailed gate",
    )
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: gate")

    assert len(rows_for("gate-failed")) == 1
    assert len(rows_for("gate-failed-detailed")) == 1
    assert len(notes_on(rows_for("gate-failed")[0])) == 1
    assert len(notes_on(rows_for("gate-failed-detailed")[0])) == 1


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


def test_existing_unowned_duplicates_update_one_row_without_filing_another(bench) -> None:
    run, db, rows_for, notes_on, _records = bench
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
    duplicate_rows = rows_for("gate-failed")
    assert len(duplicate_rows) == 2
    for duplicate in duplicate_rows:
        tg_run(db, "update", duplicate, "--status", "backlog")

    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: y")
    assert completed.returncode == 0, completed.stderr
    assert rows_for("gate-failed") == duplicate_rows
    assert sum(len(notes_on(duplicate)) for duplicate in duplicate_rows) == 2


def test_resolving_after_unowned_duplicates_leaves_the_old_evidence(bench) -> None:
    run, db, rows_for, notes_on, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: first")
    tg_run(
        db, "add", "A gate failed", "--impact", "60", "--effort", "1",
        "--tags", "monitoring", "--blocks", GOAL,
        "--description", "escalation-key: gate-failed",
    )
    for row in rows_for("gate-failed"):
        tg_run(db, "update", row, "--status", "backlog")
    notes_before = {row: len(notes_on(row)) for row in rows_for("gate-failed")}
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: second")
    selected = next(
        row for row in rows_for("gate-failed")
        if len(notes_on(row)) == notes_before[row] + 1
    )

    resolved = run("--key", "gate-failed", "--resolve")

    assert resolved.returncode == 0, resolved.stderr
    assert "outcome=resolved" in resolved.stdout
    statuses = {
        row: tg_rows(db, f"SELECT status FROM tasks WHERE local_id='{row}'")[0]
        for row in rows_for("gate-failed")
    }
    assert statuses[selected] == "CLOSED"
    assert list(statuses.values()).count("BACKLOG") == 1

    returned = run(
        "--key", "gate-failed", "--title", "A gate failed", message="ACTION: returned"
    )
    assert returned.returncode == 0, returned.stderr
    assert len(rows_for("gate-failed")) == 3


def test_two_claimed_rows_for_one_key_refuse_rather_than_choose_an_owner(bench) -> None:
    run, db, rows_for, _notes, _records = bench
    run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: x")
    tg_run(
        db, "add", "A gate failed", "--impact", "60", "--effort", "1",
        "--tags", "monitoring", "--blocks", GOAL,
        "--description", "escalation-key: gate-failed",
    )
    for index, row in enumerate(rows_for("gate-failed")):
        tg_run(db, "update", row, "--status", "in_progress", "--owner", f"agent-{index}")

    completed = run("--key", "gate-failed", "--title", "A gate failed", message="ACTION: y")
    assert completed.returncode == 1
    assert "refusing to choose between multiple owners" in completed.stderr


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


def test_a_busy_shared_database_is_waited_out_not_reported_as_a_failure(
    bench, tmp_path: Path
) -> None:
    """hermit2 is written by every agent on this box at once. `database is
    locked` means try again in a moment; reporting it as an undelivered alarm
    would be wrong, and repeated hourly it is how a channel gets muted."""
    run, _db, _rows, _notes, _records = bench
    counter = tmp_path / "attempts"
    flaky = tmp_path / "tg-flaky"
    flaky.write_text(
        "#!/bin/sh\n"
        f'n=$(cat {counter} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {counter}\n'
        'if [ "$n" -le 2 ]; then echo "database is locked" >&2; exit 1; fi\n'
        f'exec {TG} "$@"\n'
    )
    flaky.chmod(0o755)
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", "--tg", str(flaky),
        "--tg-attempts", "4", message="ACTION: x",
    )
    assert completed.returncode == 0, completed.stderr
    assert "outcome=published" in completed.stdout
    assert int(counter.read_text()) > 2, "the first attempts must really have failed"


def test_a_failure_waiting_cannot_fix_is_not_retried(bench, tmp_path: Path) -> None:
    run, _db, _rows, _notes, _records = bench
    counter = tmp_path / "attempts"
    broken = tmp_path / "tg-broken"
    broken.write_text(
        "#!/bin/sh\n"
        f'n=$(cat {counter} 2>/dev/null || echo 0); echo $((n+1)) > {counter}\n'
        'echo "tg exploded for a reason waiting will not fix" >&2\nexit 1\n'
    )
    broken.chmod(0o755)
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", "--tg", str(broken),
        "--tg-attempts", "4", message="ACTION: x",
    )
    assert completed.returncode == 1
    assert int(counter.read_text()) == 1, "a permanent failure must not be retried"


def test_a_tg_failure_reports_its_root_cause_not_its_framing(bench, tmp_path: Path) -> None:
    """tg reports errors as a boxed chain whose root cause is at the BOTTOM, so a
    head-clamped copy stops at the framing and says "database ope" where it
    should say what went wrong. That happened once and cost a diagnosis."""
    run, _db, _rows, _notes, _records = bench
    noisy = tmp_path / "tg-noisy"
    noisy.write_text(
        "#!/bin/sh\n"
        "i=0; while [ $i -lt 60 ]; do echo \"| framing line $i padding padding padding\" >&2; i=$((i+1)); done\n"
        "echo 'the actual root cause is here at the end' >&2\nexit 1\n"
    )
    noisy.chmod(0o755)
    completed = run(
        "--key", "gate-failed", "--title", "A gate failed", "--tg", str(noisy),
        message="ACTION: x",
    )
    assert completed.returncode == 1
    assert "the actual root cause is here at the end" in completed.stderr


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


def test_event_uses_explicit_id_after_100_terminal_slugs_and_keeps_227307_bytes(
    tmp_path: Path,
) -> None:
    """The historical blocker was tg add's hard ceiling after suffix `_100`."""
    home = tmp_path / "home"
    home.mkdir()
    database = "future-events"
    environment = {**os.environ, "HOME": str(home)}

    def named_tg(*args: str) -> subprocess.CompletedProcess[str]:
        completed = subprocess.run(
            [TG, "--db", database, *args],
            capture_output=True,
            text=True,
            env=environment,
            timeout=180,
        )
        assert completed.returncode == 0, completed.stderr or completed.stdout
        return completed

    created = subprocess.run(
        [TG, "db", "create", database],
        capture_output=True,
        text=True,
        env=environment,
        timeout=180,
    )
    assert created.returncode == 0, created.stderr
    named_tg("add", "Root goal", "--impact", "90", "--effort", "5", "--tags", "goal")
    title = "operational health tick reports 1 finding(s) on devbig014"
    for _ in range(100):
        named_tg(
            "add", title, "--impact", "1", "--effort", "1", "--tags", "historical"
        )
    db = home / ".tg" / f"{database}.db"
    with sqlite3.connect(db) as connection:
        connection.execute("UPDATE tasks SET status='CLOSED' WHERE title=?", (title,))
        terminal = connection.execute(
            "SELECT COUNT(*) FROM tasks WHERE title=? AND status='CLOSED'", (title,)
        ).fetchone()[0]
    assert terminal == 100

    payload = "ACTION: taskgraph-large\n" + "q" * (227_307 - 24)
    assert len(payload.encode()) == 227_307
    envelope = make_event(
        kind="alarm",
        payload=payload,
        origin_host="devbig014",
        findings=1,
        event_id="event-20260920T020304123456Z-123-deadbeef",
        created_at="2026-09-20T02:03:04Z",
    )
    event_path = write_event(tmp_path / "events", envelope)

    completed = subprocess.run(
        [
            sys.executable,
            str(ESCALATE),
            "--sink", "taskgraph",
            "--tg-db", database,
            "--tg-blocks", GOAL,
            "--log-file", str(tmp_path / "event.log"),
            "--event-file", str(event_path),
        ],
        capture_output=True,
        text=True,
        env={**environment, "DG_AGENT_NAME": "health-tick"},
        timeout=300,
    )

    assert completed.returncode == 0, completed.stderr
    local_id = stable_task_id(envelope)
    with sqlite3.connect(db) as connection:
        row = connection.execute(
            "SELECT description FROM tasks WHERE local_id=?", (local_id,)
        ).fetchone()
        notes = connection.execute(
            "SELECT content FROM task_notes WHERE task_id=? ORDER BY created_at, id",
            (local_id,),
        ).fetchall()
    assert row is not None
    assert f"escalation-event-id: {envelope['event_id']}" in row[0]
    parsed = [parse_part(note[0]) for note in notes]
    assert all(part is not None for part in parsed)
    rebuilt = "".join(
        part["payload"] for part in sorted(parsed, key=lambda part: part["number"])
    )
    assert rebuilt == payload

    named_tg(
        "import",
        "--from-string",
        json.dumps({"ops": [{"note": {"id": local_id, "message": PART_BEGIN}}]}),
    )
    later = make_event(
        kind="alarm",
        payload="ACTION: later event\n",
        origin_host="devbig014",
        findings=1,
        event_id="event-20260920T020305123456Z-123-feedface",
        created_at="2026-09-20T02:03:05Z",
    )
    later_path = write_event(tmp_path / "events", later)
    refused = subprocess.run(
        [
            sys.executable,
            str(ESCALATE),
            "--sink", "taskgraph",
            "--tg-db", database,
            "--tg-blocks", GOAL,
            "--log-file", str(tmp_path / "event.log"),
            "--event-file", str(later_path),
        ],
        capture_output=True,
        text=True,
        env={**environment, "DG_AGENT_NAME": "health-tick"},
        timeout=300,
    )
    assert refused.returncode == 78
    assert "malformed or truncated" in refused.stderr
