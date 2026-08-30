#!/usr/bin/env python3
"""Fixture-only tests for the drain-task publication gate.

THE POINT OF THIS FILE IS THE FAILING DIRECTION. A rule that only ever passes
is the defect class this project has removed repeatedly, so the bracket below
drives one fixture task through every state and asserts BOTH verdicts each
time: `test_closing_with_no_link_is_refused_by_name` is the load-bearing one,
and it fails the gate and asserts the task's own id appears in the output. Two
further tests prove the rule cannot be satisfied by pasting a URL -- one that
does not exist, and one that exists on a different pull request.

No network: every GitHub answer is injected through `--pr-facts`, which is the
same path the live gate uses, so what is exercised here is the real decision
logic and not a stub of it.
"""

from __future__ import annotations

import datetime as dt
import importlib.util
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

MODULE = Path(__file__).resolve().parents[1] / "drain_publication.py"
SPEC = importlib.util.spec_from_file_location("drain_publication_unit", MODULE)
assert SPEC is not None and SPEC.loader is not None
DRAIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = DRAIN
SPEC.loader.exec_module(DRAIN)

TASK = "drain_hermit_pr_2304_fixture"
PR = "hermit#2304"
REAL_LINK = "https://github.com/rrnewton/hermit/pull/2304#issuecomment-5409609444"
# Well-formed, right repository, right PR number, and simply not a comment
# GitHub has. This is what "pasted a plausible URL" looks like.
ABSENT_LINK = "https://github.com/rrnewton/hermit/pull/2304#issuecomment-9999999999"
# A REAL comment, on the WRONG pull request.
OTHER_PR_LINK = "https://github.com/rrnewton/hermit/pull/2269#issuecomment-5409326096"

SCHEMA = """
CREATE TABLE tasks (local_id TEXT PRIMARY KEY, task_number INTEGER, fbid TEXT,
  title TEXT NOT NULL, description TEXT, status TEXT NOT NULL DEFAULT 'OPEN',
  priority TEXT NOT NULL DEFAULT 'P0', impact INTEGER NOT NULL DEFAULT 50,
  effort_days INTEGER NOT NULL DEFAULT 1, tags TEXT NOT NULL DEFAULT '[]',
  project TEXT, creator TEXT, owner TEXT,
  sync_state TEXT NOT NULL DEFAULT '{"type":"local"}',
  local_modified_at TEXT NOT NULL, server_modified_at TEXT, last_synced_at TEXT,
  created_at TEXT NOT NULL, last_modified_source TEXT NOT NULL DEFAULT 'local');
CREATE TABLE task_notes (id INTEGER PRIMARY KEY, task_id TEXT NOT NULL,
  content TEXT NOT NULL, created_at TEXT NOT NULL,
  server_comment_id TEXT, author_unixname TEXT);
"""

NOW = "2026-08-25T12:00:00+00:00"


class InputShapeBrackets(unittest.TestCase):
    def test_valid_drain_tags_select_the_task(self) -> None:
        self.assertTrue(DRAIN.is_drain_task({"id": "x", "tags": '["drain"]'}))

    def test_malformed_tags_are_unavailable_not_not_a_drain_task(self) -> None:
        with self.assertRaisesRegex(DRAIN.Unavailable, "malformed tags"):
            DRAIN.is_drain_task({"id": "x", "tags": "[not json"})

    def test_non_string_tags_are_unavailable(self) -> None:
        with self.assertRaisesRegex(DRAIN.Unavailable, "not all strings"):
            DRAIN.is_drain_task({"id": "x", "tags": "[1]"})


class _Fixture(unittest.TestCase):
    """Scratch TaskGraph plus injected GitHub answers. Carries no tests itself."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.db = root / "scratch.db"
        connection = sqlite3.connect(self.db)
        connection.executescript(SCHEMA)
        connection.execute(
            "INSERT INTO tasks (local_id,title,description,status,tags,"
            "local_modified_at,created_at) VALUES (?,?,?,?,?,?,?)",
            (
                TASK,
                "Drain hermit PR 2304 end to end -- order SaBRe shutdown evidence",
                "Fixture with the exact shape of the task that stranded.",
                "OPEN",
                json.dumps(["hermit", "drain"]),
                NOW,
                NOW,
            ),
        )
        connection.execute(
            "INSERT INTO task_notes (task_id,content,created_at) VALUES (?,?,?)",
            (TASK, "HOLD verdict reached. Full analysis recorded in this task.", NOW),
        )
        connection.commit()
        connection.close()

        # GitHub's answers, injected. hermit#2304 is open and carries exactly the
        # two comments it really had; hermit#2269 carries the one used to prove a
        # right-repo wrong-number link is rejected.
        self.facts = root / "facts.json"
        self.facts.write_text(
            json.dumps(
                {
                    PR: {
                        "merged": False,
                        "comment_urls": [
                            "https://github.com/rrnewton/hermit/pull/2304"
                            "#issuecomment-5395596544",
                            REAL_LINK,
                        ],
                    },
                    "hermit#2269": {"merged": False, "comment_urls": [OTHER_PR_LINK]},
                }
            ),
            encoding="utf-8",
        )
        self.merged_facts = root / "facts-merged.json"
        self.merged_facts.write_text(
            json.dumps({PR: {"merged": True, "comment_urls": []}}), encoding="utf-8"
        )

    def tearDown(self) -> None:
        self.tmp.cleanup()

    # -- helpers ----------------------------------------------------------

    def _sql(self, statement: str, *params: object) -> None:
        connection = sqlite3.connect(self.db)
        connection.execute(statement, params)
        connection.commit()
        connection.close()

    def _note(self, content: str) -> None:
        connection = sqlite3.connect(self.db)
        connection.execute(
            "INSERT INTO task_notes (task_id,content,created_at) VALUES (?,?,?)",
            (TASK, content, NOW),
        )
        connection.commit()
        connection.close()

    def _close(self) -> None:
        self._sql("UPDATE tasks SET status='CLOSED' WHERE local_id=?", TASK)

    def _gate(self, facts: Path | None = None) -> tuple[int, str]:
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE),
                "--db",
                str(self.db),
                "--pr-facts",
                str(facts or self.facts),
                "--task",
                TASK,
                # The fixtures inject GitHub's answers, so there is no
                # read-after-write lag to absorb and the grace window would
                # only hide the very verdicts these tests assert.
                "--grace-mins",
                "0",
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        return result.returncode, result.stdout + result.stderr

    @staticmethod
    def _parse_kv_lines(text: str) -> dict[str, str]:
        """Byte-for-byte the tick-hub parser (agent-utils/py/tick_hub/engine.py)."""
        out: dict[str, str] = {}
        for line in text.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                key, _, value = line.partition("=")
                if key.strip():
                    out[key.strip()] = value.strip()
        return out


class TickHubParseContract(_Fixture):
    """The gate's stdout must survive `tick_hub.engine.parse_kv_lines`.

    REGRESSION, observed live on the first tick. The gate printed every field
    on ONE line; parse_kv_lines partitions each LINE on its first `=`, so it
    captured `state` and nothing else, `{summary}` never resolved, and tick-hub
    REFUSED the emission -- the gate went NO-SIGNAL. Silence reads as health,
    which is the exact defect this gate exists to remove, reproduced in the
    gate itself. A passing unit suite said nothing about it.
    """

    def test_the_gap_verdict_exposes_a_nonempty_summary(self) -> None:
        self._close()
        code, out = self._gate()
        self.assertEqual(code, 1, out)
        captured = self._parse_kv_lines(out)
        self.assertIn("summary", captured)
        self.assertTrue(captured["summary"], "summary captured but EMPTY -> NO-SIGNAL")
        self.assertIn(TASK, captured["summary"])
        self.assertEqual(captured["state"], "gap")
        self.assertEqual(captured["unpublished"], "1")

    def test_the_clean_verdict_also_exposes_a_nonempty_summary(self) -> None:
        self._close()
        self._note(f"RELAYED AND PUBLISHED: {REAL_LINK}")
        captured = self._parse_kv_lines(self._gate()[1])
        self.assertEqual(captured["state"], "clean")
        self.assertTrue(captured["summary"])

    def test_the_nothing_measured_verdict_also_exposes_one(self) -> None:
        captured = self._parse_kv_lines(self._gate()[1])
        self.assertTrue(captured["summary"])
        self.assertIn("NOTHING MEASURED", captured["summary"])

    def test_an_unreadable_facts_file_exposes_why_no_result_was_produced(self) -> None:
        unreadable = Path(self.tmp.name) / "missing-facts.json"
        code, out = self._gate(unreadable)
        self.assertEqual(code, 75, out)
        captured = self._parse_kv_lines(out)
        self.assertEqual(captured["state"], "unavailable")
        self.assertIn("cannot read --pr-facts", captured["summary"])

    def test_an_out_of_scope_task_exposes_why_no_result_was_produced(self) -> None:
        self._sql("UPDATE tasks SET title=? WHERE local_id=?", "Drain all pull requests", TASK)
        code, out = self._gate()
        self.assertEqual(code, 75, out)
        captured = self._parse_kv_lines(out)
        self.assertEqual(captured["state"], "unavailable")
        self.assertIn("not an in-scope drain task", captured["summary"])

    def test_unavailable_json_is_valid_and_does_not_claim_clean(self) -> None:
        unreadable = Path(self.tmp.name) / "missing-facts.json"
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE),
                "--db",
                str(self.db),
                "--pr-facts",
                str(unreadable),
                "--task",
                TASK,
                "--json",
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 75, result.stdout + result.stderr)
        captured = json.loads(result.stdout)
        self.assertEqual(captured["state"], "unavailable")
        self.assertIn("cannot read --pr-facts", captured["summary"])

    def test_a_github_failure_exposes_why_no_result_was_produced(self) -> None:
        self._close()
        bindir = Path(self.tmp.name) / "bin"
        bindir.mkdir()
        proxy = bindir / "with-proxy"
        proxy.write_text(
            "#!/bin/sh\nprintf 'synthetic GitHub failure\\nsecond detail' >&2\nexit 17\n",
            encoding="utf-8",
        )
        proxy.chmod(0o755)
        env = dict(os.environ)
        env["PATH"] = f"{bindir}:{env['PATH']}"
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE),
                "--db",
                str(self.db),
                "--task",
                TASK,
                "--grace-mins",
                "0",
            ],
            capture_output=True,
            text=True,
            timeout=120,
            env=env,
        )
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 75, out)
        captured = self._parse_kv_lines(out)
        self.assertEqual(captured["state"], "unavailable")
        self.assertIn("cannot read GitHub", captured["summary"])
        self.assertIn("synthetic GitHub failure", captured["summary"])
        self.assertIn("second detail", captured["summary"])


class SubjectBinding(unittest.TestCase):
    """The link->subject binding, tested WITHOUT the GitHub layer behind it.

    Mutation-checked 2026-08-25: dropping the repo/number comparison from
    `claimed_links` was invisible to the end-to-end bracket, because the
    GitHub existence check happened to reject the wrong-PR link too. Two
    layers guarding one property is fine; a layer nothing tests is not, so
    this exercises the binding on its own.
    """

    def setUp(self) -> None:
        spec = importlib.util.spec_from_file_location("drain_publication", MODULE)
        assert spec and spec.loader
        self.dp = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = self.dp
        spec.loader.exec_module(self.dp)

    def test_a_link_to_another_pull_request_is_not_claimed(self) -> None:
        self.assertEqual(self.dp.claimed_links(OTHER_PR_LINK, "hermit", 2304), [])

    def test_a_link_to_another_repository_is_not_claimed(self) -> None:
        other_repo = REAL_LINK.replace("/hermit/", "/reverie/")
        self.assertEqual(self.dp.claimed_links(other_repo, "hermit", 2304), [])

    def test_the_matching_link_is_claimed(self) -> None:
        self.assertEqual(self.dp.claimed_links(REAL_LINK, "hermit", 2304), [REAL_LINK])

    def test_a_bare_pull_request_url_is_not_a_comment_link(self) -> None:
        """The PR's own URL proves the PR exists, not that a verdict was posted."""
        self.assertEqual(
            self.dp.claimed_links("https://github.com/rrnewton/hermit/pull/2304",
                                  "hermit", 2304),
            [],
        )

    def test_review_anchors_count_as_published(self) -> None:
        for anchor in ("pullrequestreview-12345", "discussion_r67890"):
            link = f"https://github.com/rrnewton/hermit/pull/2304#{anchor}"
            self.assertEqual(self.dp.claimed_links(link, "hermit", 2304), [link], anchor)

    def test_an_exemption_needs_a_reason(self) -> None:
        self.assertIsNone(self.dp.EXEMPTION.search("NO-PUBLICATION:"))
        self.assertIsNone(self.dp.EXEMPTION.search("NO-PUBLICATION:   "))
        self.assertIsNotNone(self.dp.EXEMPTION.search("NO-PUBLICATION: duplicate task"))


class DrainPublicationBracket(_Fixture):
    # -- the bracket ------------------------------------------------------

    def test_an_open_task_is_not_yet_owed_a_comment(self) -> None:
        """The obligation attaches at CLOSE, not before."""
        code, out = self._gate()
        self.assertEqual(code, 0, out)
        # And it must not read as a pass: nothing was measured.
        self.assertIn("NOTHING MEASURED", out)

    def test_closing_with_no_link_is_refused_by_name(self) -> None:
        """THE LOAD-BEARING TEST: the rule fires, and names the task."""
        self._close()
        code, out = self._gate()
        self.assertEqual(code, 1, out)
        self.assertIn("state=gap", out)
        self.assertIn(TASK, out)
        self.assertIn(PR, out)

    def test_a_permalink_github_does_not_have_is_not_publication(self) -> None:
        self._close()
        self._note(f"Published: {ABSENT_LINK}")
        code, out = self._gate()
        self.assertEqual(code, 1, out)
        self.assertIn(TASK, out)
        self.assertIn("GitHub does not have", out)

    def test_a_real_comment_on_another_pull_request_is_not_publication(self) -> None:
        self._close()
        self._note(f"Published: {OTHER_PR_LINK}")
        code, out = self._gate()
        self.assertEqual(code, 1, out)
        self.assertIn(TASK, out)

    def test_the_real_published_comment_satisfies_the_rule(self) -> None:
        self._close()
        self._note(f"RELAYED AND PUBLISHED: {REAL_LINK}")
        code, out = self._gate()
        self.assertEqual(code, 0, out)
        self.assertIn("state=clean", out)
        self.assertIn("checked=1", out)

    def test_a_written_exemption_satisfies_the_rule_and_is_named(self) -> None:
        """Shape (4): an unsatisfiable precondition with no written exemption
        randomises behaviour. The exemption exists -- and is never silent."""
        self._close()
        self._note("NO-PUBLICATION: duplicate of drain_hermit_pr_2304; I never read the PR.")
        code, out = self._gate()
        self.assertEqual(code, 0, out)
        self.assertIn("exempt=1", out)
        self.assertIn(TASK, out)
        self.assertIn("did not publish and said why", out)

    def test_a_bare_exemption_marker_with_no_reason_does_not_satisfy_it(self) -> None:
        self._close()
        self._note("NO-PUBLICATION:")
        code, out = self._gate()
        self.assertEqual(code, 1, out)
        self.assertIn(TASK, out)

    def test_a_merged_pull_request_is_out_of_scope(self) -> None:
        """A landing publishes itself; 7 of the 10 merged drain tasks measured on
        2026-08-25 recorded no comment, correctly."""
        self._close()
        code, out = self._gate(self.merged_facts)
        self.assertEqual(code, 0, out)
        self.assertIn("checked=0", out)

    def test_a_rollup_that_names_many_pull_requests_is_out_of_scope(self) -> None:
        self._sql(
            "UPDATE tasks SET title=?, local_id=local_id WHERE local_id=?",
            "Drain hermit PR 2304 and hermit PR 2269 and hermit PR 2275 end to end",
            TASK,
        )
        self._close()
        code, out = self._gate()
        # Not in scope at all -- so the gate cannot even address it by id.
        self.assertEqual(code, 75, out)
        self.assertIn("does not name exactly one pull request", out)

    def test_an_empty_taskgraph_refuses_rather_than_reporting_clean(self) -> None:
        """Silence because the data source vanished must not read as health."""
        self._sql("DELETE FROM tasks WHERE local_id=?", TASK)
        code, out = self._gate()
        self.assertEqual(code, 75, out)
        self.assertIn("refusing to report", out)
        captured = self._parse_kv_lines(out)
        self.assertEqual(captured["state"], "unavailable")
        self.assertIn("cannot read the TaskGraph", captured["summary"])


class CutoverSuppression(_Fixture):
    """The cutover suppresses the RED. It must never suppress the COUNT.

    A cutover that hid the backlog would be the same defect one layer up: the
    gate would go green because the evidence stopped being looked at.
    """

    def _gate_since(self, since: str) -> tuple[int, str]:
        result = subprocess.run(
            [
                sys.executable, str(MODULE), "--db", str(self.db),
                "--pr-facts", str(self.facts), "--task", TASK, "--since", since,
                "--grace-mins", "0",
            ],
            capture_output=True, text=True, timeout=120,
        )
        return result.returncode, result.stdout + result.stderr

    def test_a_task_closed_before_the_cutover_is_counted_but_not_failed(self) -> None:
        self._close()
        code, out = self._gate_since("2026-08-26T00:00:00+00:00")
        self.assertEqual(code, 0, out)
        self.assertIn("suppressed_pre_cutover=1", out)
        # Green because it was not scored -- and the output has to say so.
        self.assertIn("NOTHING MEASURED", out)

    def test_a_just_closed_task_is_deferred_not_accused(self) -> None:
        """READ-AFTER-WRITE GUARD. GitHub does not always show a just-written
        object to the very next read -- the same hazard that makes
        refs/pull/N/head report the OLD sha right after a push. A verdict
        posted seconds ago can be absent from the comment list this gate
        reads, so the youngest closures wait one tick."""
        self._sql(
            "UPDATE tasks SET status='CLOSED', local_modified_at=? WHERE local_id=?",
            dt.datetime.now(dt.timezone.utc).isoformat(),
            TASK,
        )
        self._sql("DELETE FROM task_notes WHERE task_id=?", TASK)
        result = subprocess.run(
            [sys.executable, str(MODULE), "--db", str(self.db), "--pr-facts",
             str(self.facts), "--task", TASK, "--grace-mins", "15"],
            capture_output=True, text=True, timeout=120,
        )
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("deferred_in_grace=1", out)
        # Deferred is NOT published. The count must say so, and name it.
        self.assertIn("summary=0 of 1", out)
        self.assertIn("will be scored next tick", out)
        self.assertIn(TASK, out)

    def test_the_deferral_expires_and_the_task_is_then_accused(self) -> None:
        """The grace window delays the accusation; it must not cancel it.

        Run WITH the window enabled, on a task closed well outside it -- the
        case a grace window that swallowed everything would pass and a
        grace-mins-0 test could never catch.
        """
        old = dt.datetime.now(dt.timezone.utc) - dt.timedelta(hours=4)
        self._sql(
            "UPDATE tasks SET status='CLOSED', local_modified_at=? WHERE local_id=?",
            old.isoformat(),
            TASK,
        )
        self._sql(
            "UPDATE task_notes SET created_at=? WHERE task_id=?", old.isoformat(), TASK
        )
        result = subprocess.run(
            [sys.executable, str(MODULE), "--db", str(self.db), "--pr-facts",
             str(self.facts), "--task", TASK, "--grace-mins", "15"],
            capture_output=True, text=True, timeout=120,
        )
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, out)
        self.assertIn("deferred_in_grace=0", out)
        self.assertIn(TASK, out)

    def test_the_pre_cutover_backlog_is_still_counted(self) -> None:
        """A cutover that zeroes the backlog is the defect, not the fix.

        The gated path fetches GitHub facts only for tasks closed since the
        cutover, so pre-cutover tasks fall out of `in_scope` and
        `suppressed_pre_cutover` reads 0. Observed live at the shipped cutover.
        `pre_cutover_no_link_recorded` is the cheap link-presence count that
        keeps the number that JUSTIFIED the cutover on screen.
        """
        self._close()
        result = subprocess.run(
            [sys.executable, str(MODULE), "--db", str(self.db), "--pr-facts",
             str(self.facts), "--since", "2026-08-26T00:00:00+00:00",
             "--grace-mins", "0"],
            capture_output=True, text=True, timeout=120,
        )
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("pre_cutover_no_link_recorded=1", out)
        self.assertNotIn("pre_cutover_no_link_recorded=0", out)

    def test_the_same_task_closed_after_the_cutover_still_fails(self) -> None:
        self._close()
        code, out = self._gate_since("2026-08-25T00:00:00+00:00")
        self.assertEqual(code, 1, out)
        self.assertIn(TASK, out)
        self.assertIn("suppressed_pre_cutover=0", out)


if __name__ == "__main__":
    unittest.main()
