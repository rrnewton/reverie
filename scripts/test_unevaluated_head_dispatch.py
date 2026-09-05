#!/usr/bin/env python3
"""Whether a failing Actions run contradicts `github_actions_enabled=false`.

THE DEFECT THIS GUARDS. The gate concluded, from the mere EXISTENCE of a run
while ``.ops-state.yaml`` says ``github_actions_enabled=false``, that "the
observed run proves the flag is wrong", and escalated the head to
``configuration-mismatch``. Measured 2026-09-05 on the one EVALUATED_FAIL among
40 inspected open heads -- rrnewton/hermit PR 2904 at 226a6506daf9, run
33758411902 of ``.github/workflows/merge-gate.yml``:

    event: workflow_dispatch, actor rrnewton, conclusion failure

A human pressed the button, on a workflow whose trigger block is
``workflow_dispatch`` alone so it has no other way to start. That contradicts
nothing. ``hermit/scripts/check-github-actions-triggers.rs`` states the policy
the flag stands for and its own words are "every workflow may remain manually
dispatchable"; GitHub-level Actions are in fact ``enabled: true``, and what is
deactivated is AUTOMATIC evaluation of pull-request heads.

The gate fetched the whole run object, which carries ``event``, and never read
it -- so a run somebody asked for was indistinguishable from one a trigger
created, and only the second contradicts the directive.

WHAT IS DELIBERATELY UNCHANGED. The failure stays a real failure and stays
reported; a run with an automatic event still says the flag is wrong; and an
unreadable event keeps the original wording, so being unsure costs a
contradiction reported that may not be there rather than a real one going
unsaid.

WHY THIS FILE IS IN ``scripts/``. The Makefile's ``unittest discover -s scripts``
is the only scheduled Python discovery. It reaches neither ``ci-hub/health/tests/``
nor ``scripts/tests/``; the ci-hub health tests are referenced only from ai_docs
write-ups, so they run by hand.
"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "ci-hub/health/unevaluated_head.py"


def _gate():
    sys.path.insert(0, str(ROOT / "ci-hub/health"))
    spec = importlib.util.spec_from_file_location("unevaluated_head_dispatch", GATE)
    module = importlib.util.module_from_spec(spec)
    sys.modules["unevaluated_head_dispatch"] = module
    spec.loader.exec_module(module)
    return module


gate = _gate()

#: The 2904 run as the API returned it, trimmed to the fields the gate reads.
DISPATCHED_FAILURE = {
    "id": 33758411902,
    "name": "Merge Gate",
    "event": "workflow_dispatch",
    "status": "completed",
    "conclusion": "failure",
    "head_branch": "move-demos-directory-into-hermit",
}


def run_with(event: str | None) -> dict[str, object]:
    run = dict(DISPATCHED_FAILURE)
    if event is None:
        run.pop("event")
    else:
        run["event"] = event
    return run


class TheClassifierCarriesTheEvent(unittest.TestCase):
    def test_a_failing_run_reports_which_event_started_it(self) -> None:
        state, _stale, evidence = gate.classify([run_with("workflow_dispatch")])
        self.assertEqual(gate.EVALUATED_FAIL, state)
        self.assertEqual("workflow_dispatch", evidence.get("event"))

    def test_an_automatic_failing_run_reports_its_event_too(self) -> None:
        _state, _stale, evidence = gate.classify([run_with("push")])
        self.assertEqual("push", evidence.get("event"))


class WhatContradictsTheFlag(unittest.TestCase):
    def reason(self, event: str | None) -> str:
        _state, _stale, evidence = gate.classify([run_with(event)])
        return gate.likely_reason(
            {"number": 2904}, gate.EVALUATED_FAIL, False, evidence,
            actions_enabled=False, actions_disabled_directive="owner directive",
        )

    def test_a_dispatched_failure_does_not_claim_the_flag_is_wrong(self) -> None:
        """THE REGRESSION. This is the exact 2904 case."""
        reason = self.reason("workflow_dispatch")
        self.assertIn("workflow_dispatch", reason)
        self.assertIn("NOT contradicted", reason)
        self.assertNotIn("proves the local flag is wrong", reason)

    def test_a_dispatched_failure_is_still_reported_as_a_real_failure(self) -> None:
        """Not quieted: the run failed and that stands."""
        self.assertIn("failure is real", self.reason("workflow_dispatch"))

    def test_an_automatically_triggered_failure_still_says_the_flag_is_wrong(self) -> None:
        """The direction that must not be softened."""
        for event in ("push", "pull_request", "schedule", "workflow_run"):
            with self.subTest(event=event):
                reason = self.reason(event)
                self.assertIn("proves the local flag is wrong", reason)

    def test_an_unreadable_event_keeps_the_original_wording(self) -> None:
        """Being unsure must not buy silence."""
        for event in (None, ""):
            with self.subTest(event=event):
                self.assertIn("proves the local flag is wrong", self.reason(event))

    def test_with_actions_enabled_the_flag_is_not_mentioned_either_way(self) -> None:
        _state, _stale, evidence = gate.classify([run_with("workflow_dispatch")])
        reason = gate.likely_reason(
            {"number": 2904}, gate.EVALUATED_FAIL, False, evidence,
            actions_enabled=True,
        )
        self.assertNotIn("NOT contradicted", reason)
        self.assertNotIn("proves the local flag is wrong", reason)


class TheWholeGateThroughItsFixturePath(unittest.TestCase):
    """Drives collect_fixture and the summary, not just the pure helpers."""

    def gate_output(self, event: str, *extra_args: str) -> tuple[int, str, str]:
        payload = {
            "actions_enabled": False,
            "actions_disabled_directive": "owner directive",
            "prs": [{
                "number": 2904,
                "head": "226a6506daf9745508a430e8bc2997d90e9ba164",
                "mergeable": "MERGEABLE",
                "runs": [run_with(event)],
            }],
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump(payload, handle)
            path = handle.name
        try:
            done = subprocess.run(
                [sys.executable, str(GATE), "--from-json", path, *extra_args],
                capture_output=True, text=True, check=False,
            )
        finally:
            Path(path).unlink(missing_ok=True)
        return done.returncode, done.stdout, done.stderr

    def test_a_dispatched_failure_is_not_a_configuration_mismatch(self) -> None:
        _rc, out, err = self.gate_output("workflow_dispatch")
        self.assertIn("EVALUATED_FAIL(dispatched)", out + err)
        self.assertNotIn("config-mismatch", out + err)

    def test_an_automatic_failure_is_still_a_configuration_mismatch(self) -> None:
        _rc, out, err = self.gate_output("push")
        self.assertIn("config-mismatch", out + err)

    def test_the_two_populations_are_counted_separately(self) -> None:
        _rc, dispatched, _e1 = self.gate_output("workflow_dispatch", "--json")
        _rc2, automatic, _e2 = self.gate_output("push", "--json")
        first = json.loads(dispatched)
        second = json.loads(automatic)
        self.assertEqual(1, first["dispatched_but_failed"])
        self.assertEqual(0, first["disabled_but_failed"])
        self.assertEqual(0, second["dispatched_but_failed"])
        self.assertEqual(1, second["disabled_but_failed"])


if __name__ == "__main__":
    unittest.main()
