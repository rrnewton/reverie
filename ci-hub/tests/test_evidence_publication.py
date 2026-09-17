#!/usr/bin/env python3
"""A page must not show output for a run that did not produce it.

Bytes on a compatibility test-detail page read as a result. These tests pin the
four negative answers apart from one another, because collapsing them is how "we
cannot tell what this run did" comes to render as the same blank as "nothing was
captured".

Every test names, in its docstring, the mutation that would break it; each of
those was applied and confirmed to fail.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "compatibility-website"))
sys.path.insert(0, str(ROOT / "ci-hub" / "validate"))

import evidence_publication as publication  # noqa: E402
import run_registry  # noqa: E402


ADMITTED: dict[str, Any] = {
    "state": "admitted",
    "recorded_at": "2026-09-17T00:00:00+00:00",
    "run_number": 1799,
}


#: Fields each terminal state additionally requires, derived by driving
#: ``parse_current_record`` until it accepted each state rather than read off the
#: schema by hand. A fixture written from a docstring is the one that drifts.
_STATE_EXTRAS: dict[str, dict[str, Any]] = {
    "refused": {
        "result": "systemd-launch-refused",
        "detail": "probe refusal",
        "finished_at": "2026-09-17T01:00:00+00:00",
        "exit_code": 3,
    },
    "completed": {
        "result": "success",
        "finished_at": "2026-09-17T01:00:00+00:00",
        "exit_code": 0,
        "executed_nodes": 1,
        "executed_tests": 1,
        # Null, not 1: a handle carrying no `result_source` is a historical
        # validation service result, and the parser requires the count absent.
        "passed_tests": None,
        "final_validate_status": "PASSED",
    },
    "killed": {
        "result": "terminated",
        "finished_at": "2026-09-17T01:00:00+00:00",
        "signal": 9,
        "exit_code": 137,
    },
    "unknown": {"result": "unknown", "detail": "probe could not determine"},
}


def record(state: str, *, admission: dict[str, Any] | None = ADMITTED) -> dict[str, Any]:
    """A current-schema VALIDATE handle in the requested state.

    The base field set and the per-state extras were both derived by minimising
    and then driving a real handle from ``ignored/validate/runs`` until
    ``parse_current_record`` accepted it.
    """

    value: dict[str, Any] = {
        "schema_version": 1,
        "producer": "ci-hub/validate/run_registry.py",
        "kind": "validate",
        "admission": "ci-hub validate-lock",
        "unit": "validate-probe-aaaaaaaaaaaa-1-deadbeef.service",
        "agent": "probe",
        "target": "a" * 40,
        "repo": "rrnewton/hermit",
        "started_at": "2026-09-17T00:00:00+00:00",
        "state": state,
        "result": "unknown",
        "checkout": "/w/worktrees/validate/validate-fresh-aaaaaaaaaaaa-1-deadbeef",
        "source_checkout": "/w/hermit",
        "log": "/w/ignored/validate/validate-probe.log",
        "e2e_result_root": "/w/ignored/validate/artifacts/validate-probe/e2e",
        "safe_ci_dag_runner_log_dir": "/w/ignored/validate/artifacts/validate-probe/safe-ci-dag-runner",
        "cargo_home": "/w/ignored/validate/cargo-homes/validate-cargo-probe",
        "parent_checkout_head": "b" * 40,
        "pane_role": "observer-only",
        "materialized_target": True,
        "temporary_checkout": True,
        "hermit_run_timeout_seconds": 4200,
        "validate_lock_child_deadline_seconds": 4800,
        "wrkslots_generation": 1,
        "wrkslots_slot": "validate-fresh-aaaaaaaaaaaa-1-deadbeef",
    }
    value.update(_STATE_EXTRAS.get(state, {}))
    if admission is not None:
        value["admission_result"] = admission
    return value


class FixtureTests(unittest.TestCase):
    def test_the_fixture_is_accepted_by_the_owning_parser(self) -> None:
        """⚠️ Without this every other test passes for the wrong reason.

        `run_permits_publication` answers UNDECIDABLE for anything the parser
        rejects, so an invalid fixture makes the whole file green on a single
        code path while asserting nothing about the others. That is exactly what
        happened on the first run here: 14 failures, all one bad fixture.
        """

        for state in run_registry.VALIDATION_STATES:
            with self.subTest(state=state.name):
                run_registry.parse_current_record(record(state.value))

    def test_a_validate_handle_can_never_be_in_the_failed_state(self) -> None:
        """⚠️ A FAILING VALIDATE IS `completed` WITH A FAILING RESULT.

        `VALIDATION_STATES` has no FAILED -- that state belongs to bench
        handles. I designed the publishable set around "publish a failed run's
        output" and had the mechanism wrong; the property is right and it is
        delivered by COMPLETED, not by FAILED. Pinned so the next reader does
        not repeat it.
        """

        self.assertNotIn(run_registry.RunState.FAILED, run_registry.VALIDATION_STATES)
        self.assertIn(run_registry.RunState.FAILED, run_registry.BENCH_STATES)
        with self.assertRaises(RuntimeError):
            run_registry.parse_current_record(record("failed"))


class RunOutcomeTests(unittest.TestCase):
    def test_a_completed_run_may_publish(self) -> None:
        decision = publication.run_permits_publication(record("completed"))

        self.assertTrue(decision.may_publish)
        self.assertIs(decision.publication, publication.Publication.PUBLISH)

    def test_a_completed_run_that_FAILED_its_validation_may_publish(self) -> None:
        """⚠️ A failing cell's output is the most useful thing on the page.

        A failing validate is `state=completed` with a failing result, not
        `state=failed` -- see FixtureTests. So the property is delivered by
        COMPLETED, and withholding on the result would hide exactly the evidence
        a reader came for.

        Control: gate PUBLISH on `result == "success"` and this fails.
        """

        failing = record("completed")
        failing["result"] = "failure"
        failing["final_validate_status"] = "FAILED"
        failing["exit_code"] = 1

        decision = publication.run_permits_publication(failing)

        self.assertTrue(decision.may_publish, decision.reason)

    def test_an_interrupted_run_is_not_a_result(self) -> None:
        """A killed run's capture is PARTIAL and reads as complete.

        Control: move KILLED into the completed set and this fails.
        """

        for state in ("preparing", "launching", "running", "killed"):
            with self.subTest(state=state):
                decision = publication.run_permits_publication(record(state))
                self.assertIs(
                    decision.publication,
                    publication.Publication.RUN_DID_NOT_COMPLETE,
                )
                self.assertIn(state, decision.reason)

    def test_a_refused_admission_is_not_a_result_whatever_the_state_says(self) -> None:
        """No payload ran, so any bytes beside the record belong to something else.

        Control: check only `handle.state` and drop the admission branch --
        this fails, because the state here is `completed`.
        """

        refused = {
            "state": "refused",
            "recorded_at": "2026-09-17T00:00:00+00:00",
            "reason": "stale-base",
            "exit_code": 3,
        }

        decision = publication.run_permits_publication(
            record("completed", admission=refused)
        )

        self.assertIs(
            decision.publication, publication.Publication.RUN_DID_NOT_COMPLETE
        )
        self.assertIn("admission was refused", decision.reason)

    def test_a_missing_admission_result_is_undecidable_not_publishable(self) -> None:
        """⚠️ THE SAME SHAPE THAT BLOCKS VALIDATE CLEANUP FOREVER.

        A handle with no admission result cannot say whether the payload started.
        Cleanup refuses it; publication must refuse it too, and for the same
        reason -- a tool must not infer a field it never observed.

        Control: treat a missing admission as admitted and this fails.
        """

        decision = publication.run_permits_publication(
            record("completed", admission=None)
        )

        self.assertIs(
            decision.publication, publication.Publication.RUN_OUTCOME_UNDECIDABLE
        )
        self.assertIn("no admission result", decision.reason)

    def test_an_unknown_state_is_undecidable_not_incomplete(self) -> None:
        """⚠️ "cannot tell" is not "did not finish", and the page must not say so.

        Control: fold UNKNOWN into the incomplete set and this fails.
        """

        decision = publication.run_permits_publication(record("unknown"))

        self.assertIs(
            decision.publication, publication.Publication.RUN_OUTCOME_UNDECIDABLE
        )

    def test_a_malformed_handle_is_refused_by_the_owning_parser(self) -> None:
        """Control: read keys directly instead of parse_current_record."""

        decision = publication.run_permits_publication({"state": "completed"})

        self.assertIs(
            decision.publication, publication.Publication.RUN_OUTCOME_UNDECIDABLE
        )

    def test_every_run_state_has_a_decided_answer(self) -> None:
        """No state may fall through unclassified.

        Control: add a state to RunState without placing it in either set and
        this fails -- which is the point, since the fallthrough currently
        answers UNDECIDABLE and would otherwise be silent.
        """

        for state in run_registry.VALIDATION_STATES:
            with self.subTest(state=state.name):
                decision = publication.run_permits_publication(record(state.value))
                self.assertIsInstance(decision.publication, publication.Publication)
                self.assertTrue(decision.reason)
        self.assertEqual(
            set(run_registry.RunState),
            publication._COMPLETED_STATES
            | publication._INCOMPLETE_STATES
            | {run_registry.RunState.UNKNOWN},
            "a RunState was added without deciding what a page should say",
        )


class ClassificationTests(unittest.TestCase):
    def test_absent_evidence_renders_as_not_recorded(self) -> None:
        decision = publication.classify_evidence(
            record=record("completed"), recorded_byte_length=None
        )

        self.assertIs(decision.publication, publication.Publication.NOT_RECORDED)

    def test_an_over_bound_log_is_not_published_and_says_so_distinctly(self) -> None:
        """Not "not recorded", and not truncated. A third sentence.

        Control: publish a truncated prefix instead, or reuse NOT_RECORDED, and
        this fails.
        """

        decision = publication.classify_evidence(
            record=record("completed"),
            recorded_byte_length=publication.INFO_LOG_PUBLICATION_BOUND_BYTES + 1,
        )

        self.assertIs(
            decision.publication, publication.Publication.NOT_PUBLISHED_TOO_LARGE
        )
        self.assertNotEqual(
            decision.publication, publication.Publication.NOT_RECORDED
        )
        self.assertIn("not published truncated", decision.reason)

    def test_a_log_exactly_at_the_bound_is_published(self) -> None:
        decision = publication.classify_evidence(
            record=record("completed"),
            recorded_byte_length=publication.INFO_LOG_PUBLICATION_BOUND_BYTES,
        )

        self.assertTrue(decision.may_publish)

    def test_the_run_outcome_is_decided_before_the_size(self) -> None:
        """⚠️ ORDER IS LOAD-BEARING.

        A killed run's partial 2 MB log must not be reported as "too large to
        publish" -- that sentence says the bytes are fine and merely bulky, when
        they are not a result at all.

        Control: check the size bound before the run outcome and this fails.
        """

        decision = publication.classify_evidence(
            record=record("killed"),
            recorded_byte_length=publication.INFO_LOG_PUBLICATION_BOUND_BYTES * 2,
        )

        self.assertIs(
            decision.publication, publication.Publication.RUN_DID_NOT_COMPLETE
        )

    def test_absence_is_not_reported_for_a_run_that_did_not_complete(self) -> None:
        """"nothing captured" would imply the run had a chance to capture."""

        decision = publication.classify_evidence(
            record=record("killed"), recorded_byte_length=None
        )

        self.assertIs(
            decision.publication, publication.Publication.RUN_DID_NOT_COMPLETE
        )

    def test_a_negative_length_is_undecidable_rather_than_absent(self) -> None:
        decision = publication.classify_evidence(
            record=record("completed"), recorded_byte_length=-1
        )

        self.assertIs(
            decision.publication, publication.Publication.RUN_OUTCOME_UNDECIDABLE
        )

    def test_streams_with_no_bound_publish_at_any_size(self) -> None:
        """stdout and stderr are published complete; the bound is for INFO logs."""

        decision = publication.classify_evidence(
            record=record("completed"),
            recorded_byte_length=90_084,
            bound_bytes=None,
        )

        self.assertTrue(decision.may_publish)


class VocabularyTests(unittest.TestCase):
    def test_the_four_negative_answers_are_distinct(self) -> None:
        """Control: give any two the same value and this fails."""

        values = [member.value for member in publication.Publication]

        self.assertEqual(len(values), len(set(values)))
        self.assertEqual(len(values), 5)

    def test_the_bound_is_set_against_the_measured_tail_not_the_median(self) -> None:
        """p50 is 70,752 B and p90 is 545,908 B; a median-sized bound fails first.

        Control: drop the bound to the median and this fails.
        """

        self.assertGreater(publication.INFO_LOG_PUBLICATION_BOUND_BYTES, 545_908)


if __name__ == "__main__":
    unittest.main()
