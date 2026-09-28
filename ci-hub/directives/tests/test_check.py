#!/usr/bin/env python3
"""Tests for the owner tooling directive ancestry gate."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "check.py"
SPEC = importlib.util.spec_from_file_location("directive_check", MODULE_PATH)
assert SPEC and SPEC.loader
directive_check = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = directive_check
SPEC.loader.exec_module(directive_check)


def completed(command, rc=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


class FakeRunner:
    def __init__(
        self,
        known_tasks: set[str],
        task_status: dict[str, str] | None = None,
        closure_claim: dict[str, str] | None = None,
    ):
        self.known_tasks = known_tasks
        self.task_status = task_status or {}
        # What each task's closure recorded, in the gateway's own words. A task left out
        # here emits NO third column, which is how the production emitter behaves when it
        # was never asked -- and is what the pre-existing tests rely on.
        self.closure_claim = closure_claim or {}
        self.verifier_calls: list[str] = []

    def __call__(self, command, **_kwargs):
        command = tuple(str(item) for item in command)
        if command[:2] == ("tg", "sql"):
            # The production query selects `local_id || '\t' || status || '\t' || <claim>`,
            # where the claim is never empty on the wire -- `none-recorded` stands in for a
            # closure that asserted nothing, because `tg sql` strips a trailing empty column.
            # Emitting a bare id (no tab) is still valid and reads as an unknown/blank status.
            rows = []
            for task in sorted(self.known_tasks):
                row = f"{task}\t{self.task_status.get(task, 'IN_PROGRESS')}"
                if task in self.closure_claim:
                    row += f"\t{self.closure_claim[task] or 'none-recorded'}"
                rows.append(row)
            return completed(command, stdout="\n".join(rows) + "\n")
        if "protocol.py" in " ".join(command):
            identity = command[command.index("verify-landing") + 1]
            self.verifier_calls.append(identity)
            if identity.startswith("a"):
                payload = {
                    "state": "landed",
                    "ancestry": "ancestor",
                    "resolved_sha": identity,
                }
                return completed(command, stdout=json.dumps(payload))
            if identity.startswith("b"):
                payload = {
                    "state": "not-landed",
                    "ancestry": "not-ancestor",
                    "resolved_sha": identity,
                }
                return completed(command, rc=1, stdout=json.dumps(payload))
            if identity.startswith("f"):
                # The verifier reached the checkout but its fetch failed, exactly
                # as `fetch --no-tags origin <sha>: fatal: remote` did in the
                # field. The checker must not read this as a clean pass.
                payload = {
                    "state": "unverifiable",
                    "reason": (
                        "command failed: with-proxy git -C /checkout "
                        f"fetch --no-tags origin {identity}: fatal: remote error"
                    ),
                }
                return completed(command, rc=2, stdout=json.dumps(payload))
            payload = {"state": "unverifiable", "reason": "no mergeCommit.oid"}
            return completed(command, rc=2, stdout=json.dumps(payload))
        if command[:3] == ("git", "-C", str(directive_check.ROOT)) or (
            len(command) > 3 and command[0] == "git" and command[1] == "-C"
        ):
            return completed(command, stdout="d" * 40 + "\n")
        raise AssertionError(f"unexpected command: {command}")


def directive(
    item_id: str,
    *,
    identity: str | None,
    task: str | None = None,
    owner: str = "agent",
    parent_id: str | None = None,
    gate: str | None = None,
):
    record = {
        "id": item_id,
        "summary": item_id.replace("-", " "),
        "requested_at": "2026-08-04",
        "repository": "rrnewton/dev-hermit",
        "checkout": ".",
        "target": "main",
        "task": task if task is not None else f"task-{item_id}",
        "owner": owner,
        "source_row": item_id,
        "parent_id": parent_id,
        "implementation": None
        if identity is None
        else {"kind": "commit", "identity": identity},
    }
    if gate is not None:
        record["gate"] = gate
    return record


class DirectiveCheckTest(unittest.TestCase):
    def _item(self, report, directive_id):
        return next(i for i in report.directives if i.id == directive_id)

    def state_of(self, report, directive_id):
        return self._item(report, directive_id).state

    def reason_of(self, report, directive_id):
        return self._item(report, directive_id).reason or ""

    def evaluate(self, directives):
        tasks = {item["task"] for item in directives if item.get("task")}
        runner = FakeRunner(tasks)
        with tempfile.TemporaryDirectory() as temporary:
            ledger = Path(temporary) / "ledger.json"
            ledger.write_text(json.dumps({"schema_version": 1, "directives": directives}))
            report = directive_check.evaluate(
                ledger_path=ledger,
                run=runner,
                checked_at="2026-08-04T00:00:00+00:00",
            )
        return report, runner

    # --- the third state: waiting on a named external condition ---------------
    #
    # BOTH DIRECTIONS. A merely-gated ledger must render the new state; a ledger
    # with any real drift must still render red. Each test first asserts the
    # per-item state, so a pass cannot come from the fixture failing to plant
    # what it claims -- a refusing test that passes because nothing changed is
    # indistinguishable from one that works.

    def test_only_gated_rows_render_gated_not_red(self):
        directives = [
            directive("landed", identity="a" * 40),
            directive("waiting", identity=None, gate="the PR queue has drained"),
        ]
        report, _ = self.evaluate(directives)
        # the mutation took effect: the row really is gated, and names its condition
        self.assertEqual("gated", self.state_of(report, "waiting"))
        self.assertIn("the PR queue has drained", self.reason_of(report, "waiting"))
        self.assertEqual("satisfied", self.state_of(report, "landed"))
        # and the report separates it from drift
        self.assertEqual("gated", report.overall_state)
        self.assertEqual(0, len([i for i in report.directives
                                 if i.state in directive_check.DRIFT_STATES]))

    def test_gated_does_not_become_a_pass(self):
        # The relabelling must not silence the gate: exit stays non-zero and the
        # state is not green. This is the assertion that stops the repair from
        # turning into the bar-lowering it exists to avoid.
        directives = [directive("waiting", identity=None, gate="owner decision")]
        report, _ = self.evaluate(directives)
        self.assertEqual("gated", report.overall_state)
        self.assertNotEqual("green", report.overall_state)
        self.assertEqual(1, report.exit_code)

    def test_real_drift_alongside_a_gate_still_renders_red(self):
        # The discriminator that matters: one genuinely unsatisfied row must pull
        # the whole report back to red even when everything else is gated.
        directives = [
            directive("waiting", identity=None, gate="owner decision"),
            directive("drifted", identity="b" * 40),
        ]
        report, runner = self.evaluate(directives)
        runner  # the fake resolves "drifted" as a non-ancestor
        self.assertEqual("gated", self.state_of(report, "waiting"))
        self.assertNotEqual("satisfied", self.state_of(report, "drifted"))
        self.assertEqual("red", report.overall_state)
        self.assertEqual(1, report.exit_code)

    def test_all_satisfied_is_still_green(self):
        # Untouched control: adding a state must not move the clean case.
        report, _ = self.evaluate([directive("landed", identity="a" * 40)])
        self.assertEqual("green", report.overall_state)
        self.assertEqual(0, report.exit_code)

    def test_planted_nonancestor_is_refused_and_three_landed_stay_satisfied(self):
        directives = [
            directive("landed-one", identity="a" * 40),
            directive("landed-two", identity="a" * 39 + "1"),
            directive("landed-three", identity="a" * 39 + "2"),
            directive("planted-not-landed", identity="b" * 40),
        ]
        report, runner = self.evaluate(directives)

        self.assertEqual(1, report.exit_code)
        self.assertEqual(3, report.counts["satisfied"])
        self.assertEqual(1, report.counts["not_landed"])
        planted = next(
            item for item in report.directives if item.id == "planted-not-landed"
        )
        self.assertEqual("not-ancestor", planted.ancestry)
        self.assertEqual(4, len(runner.verifier_calls))

    def test_missing_task_and_owner_cannot_be_satisfied(self):
        directives = [
            directive("missing-task", identity="a" * 40, task=""),
            directive("missing-owner", identity="a" * 39 + "1", owner=""),
        ]
        report, _runner = self.evaluate(directives)

        self.assertEqual(1, report.exit_code)
        self.assertEqual(1, report.counts["missing_task"])
        self.assertEqual(1, report.counts["needs_owner"])
        self.assertEqual(0, report.counts.get("satisfied", 0))
        self.assertEqual(1, report.issue_counts["missing_task"])
        self.assertEqual(1, report.issue_counts["missing_owner"])

    def test_landed_parent_stays_partial_while_child_has_no_implementation(self):
        directives = [
            directive("parent", identity="a" * 40),
            directive("child", identity=None, parent_id="parent"),
        ]
        report, _runner = self.evaluate(directives)

        states = {item.id: item.state for item in report.directives}
        self.assertEqual({"parent": "partial", "child": "open"}, states)
        self.assertEqual(1, report.issue_counts["no_implementation"])

    def test_documentation_without_identity_remains_open(self):
        report, runner = self.evaluate([directive("quoted-only", identity=None)])

        self.assertEqual(1, report.exit_code)
        self.assertEqual(1, report.counts["open"])
        self.assertEqual([], runner.verifier_calls)

    def test_named_gate_is_gated_not_drift(self):
        report, runner = self.evaluate(
            [directive("deferred", identity=None, gate="zero open PRs")]
        )

        gated = next(item for item in report.directives if item.id == "deferred")
        self.assertEqual("gated", gated.state)
        self.assertEqual("zero open PRs", gated.gate)
        self.assertIn("zero open PRs", gated.reason)
        # A gated directive is deferred on a named condition, never counted as
        # drift, and never triggers an ancestry verification.
        self.assertNotIn(gated.state, directive_check.DRIFT_STATES)
        self.assertEqual(1, report.counts["gated"])
        self.assertEqual([], runner.verifier_calls)

    def test_unnamed_gate_is_invalid(self):
        report, _runner = self.evaluate(
            [directive("bare-gate", identity=None, gate="   ")]
        )

        item = next(item for item in report.directives if item.id == "bare-gate")
        self.assertEqual("invalid", item.state)
        self.assertIn("unnamed_gate", item.issues)

    def test_satisfied_ancestry_retires_stale_gate_text(self):
        report, _runner = self.evaluate(
            [directive("landed-but-gated", identity="a" * 40, gate="zero open PRs")]
        )

        item = next(
            item for item in report.directives if item.id == "landed-but-gated"
        )
        self.assertEqual("satisfied", item.state)
        self.assertIsNone(item.gate)
        self.assertEqual(1, report.counts["satisfied"])
        self.assertEqual(0, report.counts.get("gated", 0))

    def test_fetch_failure_is_distinct_from_not_checked_and_never_clean(self):
        # Positive side: a fetch failure while verifying a claimed commit must
        # land in `fetch_failed` — distinct from `not_checked` (never verified)
        # and from a clean pass — and must drive the overall verdict to unknown.
        directives = [
            directive("landed", identity="a" * 40),
            directive("fetch-broke", identity="f" * 40),
        ]
        report, _runner = self.evaluate(directives)

        broke = next(item for item in report.directives if item.id == "fetch-broke")
        self.assertEqual("fetch_failed", broke.state)
        self.assertEqual("fetch_failed", broke.ancestry)
        self.assertNotEqual("not_checked", broke.ancestry)
        self.assertIn("fetch failed", broke.reason)
        # `fetch_failed` is a no-result state, NOT drift: the checker could not
        # look, which is not the same as the work having regressed. This pair of
        # assertions was `assertIn(broke.state, DRIFT_STATES)` when written on
        # 2026-08-04 (2b3248e7), and that was TRUE THEN -- `fetch_failed` was a
        # member of DRIFT_STATES. 7af17d80 on 2026-08-09 ("an unfinished check is
        # unverifiable, never drift") removed it from DRIFT_STATES without
        # updating this line, 62f99796 on 2026-08-12 corrected the line to
        # NO_RESULT_STATES, and a3adf1fe on 2026-08-13 reverted that correction
        # while removing unrelated unactivated coordinator infrastructure.
        # BOTH DIRECTIONS ARE ASSERTED because membership in one tuple does not
        # exclude the other: the 2026-08-09 defect was a state being counted as
        # drift, and only the negative assertion discriminates that.
        self.assertIn(broke.state, directive_check.NO_RESULT_STATES)
        self.assertNotIn(broke.state, directive_check.DRIFT_STATES)
        # Never green, and unknown (exit 2) rather than a confirmed not_landed red.
        self.assertEqual("unknown", report.overall_state)
        self.assertEqual(2, report.exit_code)
        self.assertEqual(1, report.counts["fetch_failed"])

    def test_plain_unverifiable_is_not_misread_as_fetch_failure(self):
        # Negative side: an ambiguous verdict that is NOT a fetch failure (here
        # "no mergeCommit.oid") stays `unverifiable`, so `fetch_failed` fires
        # only on genuine fetch/network failures, not on every unknown.
        report, _runner = self.evaluate([directive("ambiguous", identity="c" * 40)])

        item = next(item for item in report.directives if item.id == "ambiguous")
        self.assertEqual("unverifiable", item.state)
        self.assertEqual(0, report.counts.get("fetch_failed", 0))




class ClosedTaskAccountabilityTest(unittest.TestCase):
    """A directive that is not satisfied must not have a CLOSED accountable task.

    Task lookup used to test EXISTENCE only, and a closed task exists -- so a row could sit
    unlanded while the one record a human reads said the work was done. Measured 2026-08-08 on
    the live ledger: 3 of 21 rows were in exactly that state, including `green-time-automatic-log`,
    which had neither an implementation nor a gate.

    Bracketed three ways on purpose. The two POSITIVE legs are not decoration: the first draft of
    this predicate made `closed_task` trip the invalid-metadata catch-all, which silently
    demoted a correctly LANDED row to unaccountable. Only the landed+closed leg caught it.
    """

    def evaluate(self, directives, task_status):
        tasks = {item["task"] for item in directives if item.get("task")}
        runner = FakeRunner(tasks, task_status=task_status)
        with tempfile.TemporaryDirectory() as temporary:
            ledger = Path(temporary) / "ledger.json"
            ledger.write_text(json.dumps({"schema_version": 1, "directives": directives}))
            return directive_check.evaluate(
                ledger_path=ledger,
                run=runner,
                checked_at="2026-08-08T00:00:00+00:00",
            )

    def test_negative_unlanded_row_with_closed_task_is_unaccountable(self):
        row = directive("orphaned", identity=None, task="t-closed")
        report = self.evaluate([row], {"t-closed": "CLOSED"})

        item = report.directives[0]
        self.assertEqual("unaccountable", item.state)
        self.assertIn("closed_task", item.issues)
        self.assertIn("CLOSED", item.reason)
        # It is drift: nobody is advancing it, because the record that would surface it is closed.
        self.assertIn(item.state, directive_check.DRIFT_STATES)
        self.assertEqual(1, report.counts["unaccountable"])
        self.assertEqual(0, report.counts.get("gated", 0))
        self.assertEqual(1, report.exit_code)

    def test_positive_unlanded_row_with_live_task_stays_open(self):
        row = directive("owned", identity=None, task="t-live")
        report = self.evaluate([row], {"t-live": "IN_PROGRESS"})

        item = report.directives[0]
        self.assertEqual("open", item.state)
        self.assertNotIn("closed_task", item.issues)

    def test_terminal_task_retires_stale_gate_and_exposes_unaccountable_work(self):
        row = directive(
            "deferred",
            identity=None,
            task="t-closed",
            gate="Hermit PR queue has drained",
        )
        report = self.evaluate([row], {"t-closed": "CLOSED"})

        item = report.directives[0]
        self.assertEqual("unaccountable", item.state)
        self.assertIn("closed_task", item.issues)
        self.assertIsNone(item.gate)
        self.assertIn(item.state, directive_check.DRIFT_STATES)
        self.assertEqual(0, report.counts.get("gated", 0))
        self.assertEqual(1, report.counts["unaccountable"])

    def test_positive_landed_row_with_closed_task_stays_satisfied(self):
        # A closed task is the CORRECT end state once the directive has landed; the predicate
        # keys on the PAIRING, not on closure alone.
        row = directive("done", identity="a" * 40, task="t-closed")
        report = self.evaluate([row], {"t-closed": "CLOSED"})

        item = report.directives[0]
        self.assertEqual("satisfied", item.state)
        self.assertIn("closed_task", item.issues)


class ClosureReasonTest(unittest.TestCase):
    """`CLOSED` is one bit with two meanings, and the gate used to render both the same.

    A task closes because the work was finished, and a task closes because its tracking moved
    elsewhere, was superseded, or was reconciled after its agent died. Measured 2026-08-20 on
    the live ledger, BOTH drifting rows were the second kind:
    `dbi-to-dbt-rename-and-install-path-factoring` closed with "Moved to
    https://github.com/rrnewton/hermit/issues/1857", and `make_unsupported_syscall_panic`
    closed by dead-agent slot reconciliation. Neither was finished, and the gate said the same
    thing about both.

    The state stays `unaccountable` for both ON PURPOSE -- they are equally unmet, and giving
    one of them its own state is how it gets filtered out of the drift count later. What
    differs is the REMEDY, so what differs is the reason and the reported claim.
    """

    def evaluate(self, directives, task_status, closure_claim):
        tasks = {item["task"] for item in directives if item.get("task")}
        runner = FakeRunner(tasks, task_status=task_status, closure_claim=closure_claim)
        with tempfile.TemporaryDirectory() as temporary:
            ledger = Path(temporary) / "ledger.json"
            ledger.write_text(json.dumps({"schema_version": 1, "directives": directives}))
            return directive_check.evaluate(
                ledger_path=ledger,
                run=runner,
                checked_at="2026-08-20T00:00:00+00:00",
            )

    def test_moved_and_completed_closures_report_differently(self):
        # THE WHOLE POINT, in one test: two rows identical except for what their closure
        # recorded must not produce the same output.
        moved = directive("moved", identity=None, task="t-moved")
        claimed = directive("claimed", identity=None, task="t-claimed")
        report = self.evaluate(
            [moved, claimed],
            {"t-moved": "CLOSED", "t-claimed": "CLOSED"},
            {"t-moved": "", "t-claimed": "implemented"},
        )
        by_id = {item.id: item for item in report.directives}

        # Same state, same severity, same count -- nothing was hidden or downgraded.
        self.assertEqual("unaccountable", by_id["moved"].state)
        self.assertEqual("unaccountable", by_id["claimed"].state)
        self.assertEqual(2, report.counts["unaccountable"])
        self.assertEqual(1, report.exit_code)

        # Different recorded claim, and it is the closure gateway's own word.
        self.assertEqual("", by_id["moved"].closure_claim)
        self.assertEqual("implemented", by_id["claimed"].closure_claim)

        # Different reason, naming the different remedy.
        self.assertIn("no completion claim", by_id["moved"].reason)
        self.assertIn("without anyone asserting the work was done", by_id["moved"].reason)
        self.assertIn("claims 'implemented'", by_id["claimed"].reason)
        self.assertIn("fresh ancestry contradicts", by_id["claimed"].reason)
        self.assertNotEqual(by_id["moved"].reason, by_id["claimed"].reason)

    def test_unread_closure_is_not_reported_as_a_silent_one(self):
        # A NO-RESULT IS NOT A NEGATIVE, the rule this file already enforces for
        # `unverifiable`. If the emitter never answered, the row must not read as "the
        # closure asserted nothing" -- that would be an unmeasured claim about the closure.
        row = directive("unread", identity=None, task="t-unknown")
        report = self.evaluate([row], {"t-unknown": "CLOSED"}, {})

        item = report.directives[0]
        self.assertEqual("unaccountable", item.state)
        self.assertIsNone(item.closure_claim)
        self.assertIn("could not read what that closure recorded", item.reason)
        self.assertNotIn("no completion claim", item.reason)

    def test_printed_field_separates_the_two_without_touching_the_drift_line(self):
        # `summary=drift=<ids>` is parsed by consumers this change cannot see, so the reason
        # goes in its own field and the drift line keeps emitting bare ids.
        moved = directive("moved", identity=None, task="t-moved")
        claimed = directive("claimed", identity=None, task="t-claimed")
        report = self.evaluate(
            [moved, claimed],
            {"t-moved": "CLOSED", "t-claimed": "CLOSED"},
            {"t-moved": "", "t-claimed": "landed"},
        )
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            directive_check._print_fields(report)
        lines = dict(
            line.split("=", 1) for line in printed.getvalue().splitlines() if "=" in line
        )

        self.assertEqual("moved:none-recorded,claimed:landed", lines["closure_claim"])
        self.assertEqual("drift=moved,claimed", lines["summary"])
        self.assertEqual("2", lines["drift"])

    def test_the_query_survives_a_renderer_that_strips_trailing_whitespace(self):
        """The claim must never travel the wire as an empty column.

        `tg sql` renders a table and strips each row, so `... || '\\t' || ''` arrives as a row
        with NO third column -- indistinguishable from an emitter that was never asked. It is
        not hypothetical: with the sentinel removed, the live ledger returned 15 statuses and
        only 12 claims, and the 3 lost rows were exactly the closures that recorded nothing.

        This runs check.py's OWN query text against a real SQLite with the production schema,
        then strips the rows the way the renderer does. Asserting on the query string instead
        would only re-read the code.
        """
        database = sqlite3.connect(":memory:")
        database.execute("CREATE TABLE tasks (local_id TEXT, status TEXT, tags TEXT)")
        database.execute("CREATE TABLE task_notes (task_id TEXT, content TEXT)")
        database.executemany(
            "INSERT INTO tasks VALUES (?,?,?)",
            [
                ("t-moved", "CLOSED", '["owner-directive"]'),
                ("t-claimed", "CLOSED", '["owner-directive","implemented"]'),
                ("t-gateway", "CLOSED", "[]"),
            ],
        )
        database.execute(
            "INSERT INTO task_notes VALUES (?,?)",
            ("t-gateway", "CLOSURE-VERIFIED: kind=code landing=landed verifier=x"),
        )

        def runner(command, **_kwargs):
            command = tuple(str(item) for item in command)
            if command[:2] != ("tg", "sql"):
                return completed(command, rc=1)
            rows = [
                # The renderer's strip is the whole point of the test.
                "\t".join(str(cell) for cell in row).rstrip()
                for row in database.execute(command[2].rstrip(";"))
            ]
            return completed(command, stdout="\n".join(rows) + "\n")

        rows = [
            directive("moved", identity=None, task="t-moved"),
            directive("claimed", identity=None, task="t-claimed"),
            directive("gateway", identity=None, task="t-gateway"),
        ]
        parsed = [directive_check._parse_directive(row) for row in rows]
        known, claims, error = directive_check._query_known_tasks(parsed, runner, 5.0)

        self.assertIsNone(error)
        self.assertEqual(3, len(known))
        # Three tasks in, three claims out. The empty one is present AS empty, not missing.
        self.assertEqual(
            {"t-moved": "", "t-claimed": "implemented", "t-gateway": "landed"}, claims
        )

    def test_a_landed_directive_never_reads_its_closure_claim_as_satisfaction(self):
        # The closure README lists both the `closed` status and the `implemented` tag as
        # NON-AUTHORITATIVE. A row is satisfied by fresh ancestry and by nothing else, so a
        # closure claiming `landed` on a row whose commit is NOT an ancestor must stay drift.
        row = directive("lying-closure", identity="b" * 40, task="t-closed")
        report = self.evaluate([row], {"t-closed": "CLOSED"}, {"t-closed": "landed"})

        item = report.directives[0]
        self.assertEqual("unaccountable", item.state)
        self.assertIn(item.state, directive_check.DRIFT_STATES)
        self.assertIn("claims 'landed'", item.reason)
        self.assertIn("fresh ancestry contradicts", item.reason)
        self.assertEqual(0, report.counts.get("satisfied", 0))



class ArchiveLineageGapTest(unittest.TestCase):
    """A pointer into a database that is not there must not read as a bad pointer.

    The archive is found by an alphabetic-stem glob. Measured on a scratch copy of the
    real databases: with `hermit.db` beside `hermit2.db`, 15 of 15 ledger task ids
    resolve and no record is `task_not_found`; rename THE SAME FILE to
    `hermit-archive-2026.db` and 1 of 15 resolve and 20 records become
    `task_not_found`, which is in DRIFT_STATES. Nothing about the data changed.

    That mislabel is expensive in a specific way: a reader would see twenty of the
    owner's directives reported as drifted and go looking for directive drift that
    does not exist.

    The discriminator is the live database's own NAME. `hermit2.db` carries an ordinal
    and therefore implies a predecessor; `hermit.db` does not. The two negative legs
    below are the ones that matter -- they are what stops this from being a blanket
    softening of `task_not_found`.
    """

    def setUp(self):
        self._saved = {
            key: os.environ.get(key)
            for key in ("TG_DB_PATH", "DIRECTIVE_TASK_DBS")
        }
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(self._restore)

    def _restore(self):
        for key, value in self._saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    def _live(self, name: str, siblings=()):
        root = Path(self.tmp.name)
        (root / name).write_text("")
        for sibling in siblings:
            (root / sibling).write_text("")
        os.environ.pop("DIRECTIVE_TASK_DBS", None)
        os.environ["TG_DB_PATH"] = str(root / name)

    def test_ordinal_live_database_with_no_predecessor_is_a_no_result(self):
        self._live("hermit2.db")
        gap = directive_check.archive_lineage_gap()
        self.assertIsNotNone(gap)
        self.assertIn("hermit2.db", gap)
        self.assertIn("hermit.db", gap)

        row = directive("orphan", identity=None, task="t-precutover")
        parsed = directive_check._parse_directive(row)
        issues = directive_check._metadata_issues(parsed, {}, None, gap)
        self.assertIn("task_lookup_unavailable", issues)
        self.assertNotIn("task_not_found", issues)
        # Loud, but pointing at the lookup rather than at the owner's directives.
        self.assertIn("unverifiable", directive_check.NO_RESULT_STATES)
        self.assertNotIn("unverifiable", directive_check.DRIFT_STATES)

    def test_predecessor_present_still_reports_a_genuinely_absent_pointer(self):
        self._live("hermit2.db", siblings=("hermit.db",))
        gap = directive_check.archive_lineage_gap()
        self.assertIsNone(gap)

        row = directive("orphan", identity=None, task="t-nowhere")
        parsed = directive_check._parse_directive(row)
        issues = directive_check._metadata_issues(parsed, {}, None, gap)
        self.assertIn("task_not_found", issues)

    def test_live_database_without_an_ordinal_implies_no_predecessor(self):
        # A fleet that never had an archive must keep reporting a wrong pointer as
        # wrong. This is the leg that stops the change from softening the check.
        self._live("hermit.db")
        gap = directive_check.archive_lineage_gap()
        self.assertIsNone(gap)

        row = directive("orphan", identity=None, task="t-nowhere")
        parsed = directive_check._parse_directive(row)
        issues = directive_check._metadata_issues(parsed, {}, None, gap)
        self.assertIn("task_not_found", issues)

    def test_an_explicitly_named_database_set_infers_no_lineage(self):
        self._live("hermit2.db")
        os.environ["DIRECTIVE_TASK_DBS"] = os.environ["TG_DB_PATH"]
        self.assertIsNone(directive_check.archive_lineage_gap())

class TaskDatabaseResolutionTest(unittest.TestCase):
    """Bracket the four task-resolution classes, one per class, with counts.

    Before this fix the checker queried only the live database and mapped BOTH
    "names no task" and "names a task I cannot see" to state `missing_task`.
    After the 2026-08-07 cutover renamed the fleet database, that reported
    `missing_task=21` on 21 of 21 records whose tasks were all alive in the
    previous file -- a 100% failure, which is a partition and never a fact.
    """

    LIVE = "/tmp/fixture-live.db"
    ARCHIVE = "/tmp/fixture-archive.db"

    class DbAwareRunner(FakeRunner):
        """Answers `tg sql` differently per TG_DB_PATH, so the test can only pass
        if the checker really varies the database it asks. A runner that ignored
        the environment would return identical rows and the archive case could
        not be distinguished from the live case."""

        def __init__(self, per_db):
            super().__init__(set())
            self.per_db = per_db
            self.queried: list[str] = []

        def __call__(self, command, **kwargs):
            command = tuple(str(item) for item in command)
            if command[:2] == ("tg", "sql"):
                db = (kwargs.get("env") or {}).get("TG_DB_PATH", "")
                self.queried.append(db)
                rows = [
                    f"{task}\t{status}"
                    for task, status in sorted(self.per_db.get(db, {}).items())
                ]
                return completed(command, stdout=("\n".join(rows) + "\n") if rows else "")
            return super().__call__(command, **kwargs)

    def evaluate(self, directives, per_db):
        runner = self.DbAwareRunner(per_db)
        env = dict(os.environ)
        env["DIRECTIVE_TASK_DBS"] = os.pathsep.join([self.LIVE, self.ARCHIVE])
        with unittest.mock.patch.dict(os.environ, env, clear=True):
            with tempfile.TemporaryDirectory() as temporary:
                ledger = Path(temporary) / "ledger.json"
                ledger.write_text(
                    json.dumps({"schema_version": 1, "directives": directives})
                )
                report = directive_check.evaluate(
                    ledger_path=ledger,
                    run=runner,
                    checked_at="2026-08-04T00:00:00+00:00",
                )
        return report, runner

    def state_of(self, report, directive_id):
        return next(item for item in report.directives if item.id == directive_id).state

    def issues_of(self, report, directive_id):
        return set(
            next(item for item in report.directives if item.id == directive_id).issues
        )

    def test_four_resolution_classes_are_four_distinct_states(self):
        directives = [
            directive("in-live", identity="a" * 40, task="task-live"),
            directive("in-archive", identity="a" * 39 + "1", task="task-archived"),
            directive("nowhere", identity="a" * 39 + "2", task="task-ghost"),
            directive("no-task-at-all", identity="a" * 39 + "3", task=""),
        ]
        per_db = {
            self.LIVE: {"task-live": "IN_PROGRESS"},
            self.ARCHIVE: {"task-archived": "IN_PROGRESS"},
        }
        report, runner = self.evaluate(directives, per_db)

        # POSITIVE: a task in the CURRENT database resolves and is not reported.
        self.assertEqual("satisfied", self.state_of(report, "in-live"))
        self.assertEqual(set(), self.issues_of(report, "in-live"))

        # POSITIVE: a task that exists only in an archive still resolves, and is
        # flagged as its own fact rather than being called missing.
        self.assertEqual("satisfied", self.state_of(report, "in-archive"))
        self.assertEqual({"task_in_archived_db"}, self.issues_of(report, "in-archive"))

        # NEGATIVE: a task in NO database is task_not_found -- DISTINCT from...
        self.assertEqual("task_not_found", self.state_of(report, "nowhere"))
        # ...NEGATIVE: a directive naming no task at all, which stays missing_task.
        self.assertEqual("missing_task", self.state_of(report, "no-task-at-all"))

        # Counts per class, both sides, in one assertion set.
        self.assertEqual(2, report.counts["satisfied"])
        self.assertEqual(1, report.counts["task_not_found"])
        self.assertEqual(1, report.counts["missing_task"])
        self.assertEqual(1, report.issue_counts["task_in_archived_db"])
        self.assertEqual(1, report.issue_counts["task_not_found"])
        self.assertEqual(1, report.issue_counts["missing_task"])

        # The archive is only consulted because the live database came up short.
        self.assertEqual([self.LIVE, self.ARCHIVE], runner.queried)

    def test_live_database_wins_over_an_archive_holding_the_same_id(self):
        # A recycled id must resolve to the LIVE row, never the stale archived one,
        # or a reopened task would read as archived and closed.
        directives = [directive("dup", identity="a" * 40, task="task-dup")]
        per_db = {
            self.LIVE: {"task-dup": "IN_PROGRESS"},
            self.ARCHIVE: {"task-dup": "CLOSED"},
        }
        report, runner = self.evaluate(directives, per_db)
        self.assertEqual(set(), self.issues_of(report, "dup"))
        # Short-circuits: every id was satisfied by the live database.
        self.assertEqual([self.LIVE], runner.queried)

    def test_archive_only_resolution_cannot_demote_a_satisfied_row(self):
        # The bug this guards: `task_in_archived_db` was initially outside the
        # exempt set, so it fell through to `invalid` and then `unaccountable`,
        # turning 18 ancestry-verified landed directives into drift.
        directives = [directive("landed", identity="a" * 40, task="task-archived")]
        per_db = {self.LIVE: {}, self.ARCHIVE: {"task-archived": "CLOSED"}}
        report, _runner = self.evaluate(directives, per_db)
        self.assertEqual("satisfied", self.state_of(report, "landed"))
        self.assertEqual(0, report.exit_code)

    def test_live_database_outage_is_unverifiable_not_a_mass_not_found(self):
        # A dead live database must never read as "nobody has a task".
        class DeadLive(self.DbAwareRunner):
            def __call__(self, command, **kwargs):
                command = tuple(str(item) for item in command)
                if command[:2] == ("tg", "sql"):
                    self.queried.append((kwargs.get("env") or {}).get("TG_DB_PATH", ""))
                    return completed(command, rc=1, stderr="unable to open database")
                return FakeRunner.__call__(self, command, **kwargs)

        runner = DeadLive({})
        env = dict(os.environ)
        env["DIRECTIVE_TASK_DBS"] = os.pathsep.join([self.LIVE, self.ARCHIVE])
        with unittest.mock.patch.dict(os.environ, env, clear=True):
            with tempfile.TemporaryDirectory() as temporary:
                ledger = Path(temporary) / "ledger.json"
                ledger.write_text(
                    json.dumps(
                        {
                            "schema_version": 1,
                            "directives": [
                                directive("x", identity="a" * 40, task="task-live")
                            ],
                        }
                    )
                )
                report = directive_check.evaluate(
                    ledger_path=ledger,
                    run=runner,
                    checked_at="2026-08-04T00:00:00+00:00",
                )
        self.assertEqual("unverifiable", self.state_of(report, "x"))
        self.assertEqual(0, report.counts.get("task_not_found", 0))
        self.assertEqual(0, report.counts.get("missing_task", 0))


class TaskDatabaseLineageTest(unittest.TestCase):
    def test_lineage_admits_numbered_siblings_and_excludes_other_projects(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in (
                "hermit.db",
                "hermit1.db",
                "hermit2.db",
                "hermit-policy-audit.db",
                "hatch2.db",
                "scx-sim.db",
            ):
                (root / name).write_text("")
            with unittest.mock.patch.dict(
                os.environ, {"TG_DB_PATH": str(root / "hermit2.db")}, clear=True
            ):
                found = directive_check.task_databases()
        names = [Path(p).name for p in found]
        self.assertEqual("hermit2.db", names[0], "live database must be queried first")
        self.assertEqual({"hermit.db", "hermit1.db"}, set(names[1:]))
        # A bare `hermit*` prefix would drag in an unrelated project's database and
        # resolve task ids out of it -- a false positive worse than the miss.
        self.assertNotIn("hermit-policy-audit.db", names)
        self.assertNotIn("hatch2.db", names)
        self.assertNotIn("scx-sim.db", names)


ISSUE = "https://github.com/rrnewton/hermit/issues/4242"
ISSUE2 = "https://github.com/rrnewton/hermit/issues/4243"


class GitHubIssueTaskTest(unittest.TestCase):
    """A directive's `task` may be a GitHub issue URL instead of a TaskGraph id.

    TaskGraph is ORC's own to-do list (AGENTS.md, 2026-09-28). An agent outside ORC records
    accountability as a GitHub issue, and the gate used to call that correct workflow drift
    (`task_not_found`), or refuse the whole ledger because a URL fails TASK_RE. Each test
    asserts the per-row state, not just the exit code.
    """

    def evaluate(self, directives, issues, *, tg=None):
        """`issues` maps URL -> gh JSON payload, or None for a failed lookup."""
        calls: list[tuple[str, ...]] = []
        verifier = FakeRunner(set())

        def runner(command, **kwargs):
            command = tuple(str(item) for item in command)
            calls.append(command)
            if command[:4] == ("with-proxy", "gh", "issue", "view"):
                payload = issues.get(command[4])
                if payload is None:
                    return completed(command, rc=1, stderr="GraphQL: Could not resolve to an issue")
                return completed(command, stdout=json.dumps(payload))
            if command[:2] == ("tg", "sql"):
                if tg is None:
                    raise AssertionError("TaskGraph must not be queried for issue-only ledgers")
                return tg(command)
            return verifier(command, **kwargs)

        with tempfile.TemporaryDirectory() as temporary:
            ledger = Path(temporary) / "ledger.json"
            ledger.write_text(json.dumps({"schema_version": 1, "directives": directives}))
            report = directive_check.evaluate(
                ledger_path=ledger, run=runner, checked_at="2026-09-28T00:00:00+00:00"
            )
        return report, calls

    def item(self, report, directive_id):
        return next(i for i in report.directives if i.id == directive_id)

    def test_open_issue_is_accountable_without_taskgraph(self):
        report, calls = self.evaluate(
            [directive("row", identity=None, task=ISSUE)], {ISSUE: {"state": "OPEN"}}
        )
        item = self.item(report, "row")
        self.assertEqual("open", item.state)
        for issue in ("missing_task", "task_not_found", "task_lookup_unavailable", "closed_task"):
            self.assertNotIn(issue, item.issues)
        self.assertFalse(any(call[:2] == ("tg", "sql") for call in calls))

    def test_issue_closed_completed_on_unlanded_row_is_unaccountable(self):
        report, _ = self.evaluate(
            [directive("row", identity=None, task=ISSUE)],
            {ISSUE: {"state": "CLOSED", "stateReason": "COMPLETED"}},
        )
        item = self.item(report, "row")
        self.assertEqual("unaccountable", item.state)
        self.assertIn(item.state, directive_check.DRIFT_STATES)
        self.assertEqual("completed", item.closure_claim)
        self.assertIn("the closure claims 'completed', which fresh ancestry contradicts", item.reason)

    def test_issue_closed_not_planned_says_so(self):
        report, _ = self.evaluate(
            [directive("row", identity=None, task=ISSUE)],
            {ISSUE: {"state": "CLOSED", "stateReason": "NOT_PLANNED"}},
        )
        item = self.item(report, "row")
        self.assertEqual("unaccountable", item.state)
        self.assertEqual("", item.closure_claim)
        self.assertIn("the issue was closed without being marked completed", item.reason)
        self.assertNotIn("CLOSURE-VERIFIED", item.reason)

    def test_landed_row_with_closed_issue_stays_satisfied(self):
        report, _ = self.evaluate(
            [directive("row", identity="a" * 40, task=ISSUE)],
            {ISSUE: {"state": "CLOSED", "stateReason": "COMPLETED"}},
        )
        item = self.item(report, "row")
        self.assertEqual("satisfied", item.state)
        self.assertIn("closed_task", item.issues)

    def test_failed_issue_lookup_is_unverifiable_never_drift(self):
        report, _ = self.evaluate([directive("row", identity=None, task=ISSUE)], {})
        item = self.item(report, "row")
        self.assertEqual("unverifiable", item.state)
        self.assertNotIn(item.state, directive_check.DRIFT_STATES)
        self.assertIn("task_lookup_unavailable", item.issues)
        self.assertNotIn("task_not_found", item.issues)
        self.assertIn(f"GitHub issue lookup failed for {ISSUE}", item.reason)
        self.assertIn("Could not resolve to an issue", item.reason)

    def test_taskgraph_outage_does_not_hide_a_resolved_issue(self):
        def tg_down(command):
            return completed(command, rc=2, stderr="tg: database locked")

        report, _ = self.evaluate(
            [
                directive("issue-row", identity=None, task=ISSUE),
                directive("tg-row", identity=None, task="t-live"),
            ],
            {ISSUE: {"state": "OPEN"}},
            tg=tg_down,
        )
        self.assertEqual("open", self.item(report, "issue-row").state)
        tg_item = self.item(report, "tg-row")
        self.assertEqual("unverifiable", tg_item.state)
        self.assertIn("tg: database locked", tg_item.reason)

    def test_mixed_ledger_resolves_both_kinds(self):
        def tg_up(command):
            return completed(command, stdout="t-live\tIN_PROGRESS\n")

        report, _ = self.evaluate(
            [
                directive("issue-row", identity=None, task=ISSUE2),
                directive("tg-row", identity=None, task="t-live"),
            ],
            {ISSUE2: {"state": "OPEN"}},
            tg=tg_up,
        )
        self.assertEqual("open", self.item(report, "issue-row").state)
        self.assertEqual("open", self.item(report, "tg-row").state)

    def test_pull_request_url_is_refused_with_the_message(self):
        def tg_up(command):
            return completed(command, stdout="")

        report, _ = self.evaluate(
            [directive("row", identity=None, task="https://github.com/rrnewton/hermit/pull/1")],
            {},
            tg=tg_up,
        )
        item = self.item(report, "row")
        self.assertEqual("unverifiable", item.state)
        self.assertIn(
            "or be a full https://github.com/<owner>/<repo>/issues/<n> URL", item.reason
        )


if __name__ == "__main__":
    unittest.main()
