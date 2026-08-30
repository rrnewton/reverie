#!/usr/bin/env python3
"""Bracket the one TaskGraph resolver, both directions.

The defect this module exists to prevent is asymmetric and easy to reintroduce:
a consumer that cannot find its database must say so, because ``tg`` with no
database named succeeds against an empty default and returns 0 rows.  Zero rows
and "I could not look" are indistinguishable downstream, and the zero reads as
health.  So the negatives here are the load-bearing half.
"""

from __future__ import annotations

import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(LIB))

import taskgraph_db  # noqa: E402


def make_db(path: Path, relation: str = "tasks", rows: int = 0) -> Path:
    con = sqlite3.connect(path)
    con.execute(f"CREATE TABLE {relation} (local_id TEXT, status TEXT)")
    for i in range(rows):
        con.execute(f"INSERT INTO {relation} VALUES (?,?)", (f"t{i}", "OPEN"))
    con.commit()
    con.close()
    return path


class ResolveTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())

    # ---------------------------------------------------------------- positive

    def test_explicit_argument_wins_and_reports_its_source(self) -> None:
        db = make_db(self.tmp / "explicit.db")
        resolution = taskgraph_db.resolve(db, env={taskgraph_db.ENV_VAR: "/nope.db"})
        self.assertEqual(db, resolution.path)
        self.assertEqual("explicit-argument", resolution.source)
        self.assertIn("explicit-argument", resolution.basis)

    def test_env_var_binds_when_no_argument_is_given(self) -> None:
        db = make_db(self.tmp / "env.db")
        resolution = taskgraph_db.resolve(env={taskgraph_db.ENV_VAR: str(db)})
        self.assertEqual(db, resolution.path)
        self.assertEqual(taskgraph_db.ENV_VAR, resolution.source)

    def test_an_empty_task_population_is_a_measured_zero_not_a_refusal(self) -> None:
        """The distinction the whole design turns on: 0 rows is a RESULT."""

        db = make_db(self.tmp / "empty.db", rows=0)
        self.assertEqual(db, taskgraph_db.resolve(db).path)

    def test_the_view_only_shape_resolves_too(self) -> None:
        db = make_db(self.tmp / "view.db", relation="tasks_v")
        self.assertEqual(db, taskgraph_db.resolve(db).path)

    def test_child_env_states_the_binding_for_a_subprocess(self) -> None:
        db = make_db(self.tmp / "child.db")
        env = taskgraph_db.child_env(taskgraph_db.resolve(db), env={"PATH": "/bin"})
        self.assertEqual(str(db), env[taskgraph_db.ENV_VAR])
        self.assertEqual("/bin", env["PATH"])

    def test_json_rows_accept_complete_output(self) -> None:
        rows = taskgraph_db.parse_json_rows(
            'row_json\n--------\n{"id":"a"}\n{"id":"b"}\n\n(2 rows)\n'
        )
        self.assertEqual([{"id": "a"}, {"id": "b"}], rows)

    def test_json_rows_refuse_a_silently_smaller_population(self) -> None:
        with self.assertRaisesRegex(taskgraph_db.TaskGraphUnavailable, "malformed JSON"):
            taskgraph_db.parse_json_rows(
                'row_json\n--------\n{"id":"a"}\n{bad json\n\n(2 rows)\n'
            )

    def test_json_rows_refuse_a_row_count_mismatch(self) -> None:
        with self.assertRaisesRegex(
            taskgraph_db.TaskGraphUnavailable, "reported 2 row.*1 JSON"
        ):
            taskgraph_db.parse_json_rows(
                'row_json\n--------\n{"id":"a"}\n\n(2 rows)\n'
            )

    def test_json_rows_refuse_output_without_a_denominator(self) -> None:
        with self.assertRaisesRegex(taskgraph_db.TaskGraphUnavailable, "row-count footer"):
            taskgraph_db.parse_json_rows('{"id":"a"}\n')

    # ---------------------------------------------------------------- negative

    def test_unbound_environment_refuses_instead_of_reading_tgs_empty_default(self) -> None:
        """The whole point. Unset must NOT become a clean zero."""

        with self.assertRaises(taskgraph_db.TaskGraphUnavailable) as caught:
            taskgraph_db.resolve(env={})
        self.assertIn(taskgraph_db.ENV_VAR, str(caught.exception))
        self.assertIn(taskgraph_db.IMPLICIT_FALLBACK_STEM, str(caught.exception))

    def test_blank_environment_value_is_unbound_not_a_path(self) -> None:
        with self.assertRaises(taskgraph_db.TaskGraphUnavailable):
            taskgraph_db.resolve(env={taskgraph_db.ENV_VAR: "   "})

    def test_missing_file_refuses(self) -> None:
        with self.assertRaises(taskgraph_db.TaskGraphUnavailable):
            taskgraph_db.resolve(self.tmp / "absent.db")

    def test_a_database_without_task_relations_refuses(self) -> None:
        db = make_db(self.tmp / "other.db", relation="unrelated")
        with self.assertRaises(taskgraph_db.TaskGraphUnavailable) as caught:
            taskgraph_db.resolve(db)
        self.assertIn("not a TaskGraph database", str(caught.exception))

    def test_a_non_sqlite_file_refuses(self) -> None:
        junk = self.tmp / "junk.db"
        junk.write_text("not a database")
        with self.assertRaises(taskgraph_db.TaskGraphUnavailable):
            taskgraph_db.resolve(junk)

    def test_no_hermit_db_symlink_is_ever_a_resolution_step(self) -> None:
        """A symlink would make stale literals work by accident.

        The successor session would inherit the identical confusion, so the
        resolver must refuse rather than follow a compatibility alias it was
        not explicitly pointed at.
        """

        real = make_db(self.tmp / "hermit2.db")
        alias = self.tmp / "hermit.db"
        alias.symlink_to(real)
        with self.assertRaises(taskgraph_db.TaskGraphUnavailable):
            taskgraph_db.resolve(env={})

    # -------------------------------------------------------------------- cli

    def test_cli_exit_codes_separate_bound_from_could_not_measure(self) -> None:
        db = make_db(self.tmp / "cli.db")
        script = str(LIB / "taskgraph_db.py")

        bound = subprocess.run(
            [sys.executable, script, "--db", str(db)],
            capture_output=True, text=True,
        )
        self.assertEqual(0, bound.returncode)
        self.assertIn("state=clean", bound.stdout)

        unbound = subprocess.run(
            [sys.executable, script],
            capture_output=True, text=True, env={"PATH": "/usr/bin:/bin"},
        )
        self.assertEqual(2, unbound.returncode)
        self.assertIn("state=unverifiable", unbound.stdout)


if __name__ == "__main__":
    unittest.main()
