#!/usr/bin/env python3
"""Bracket gate_runner_presence's captured tick-hub summary field."""

from __future__ import annotations

import importlib.util
import io
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

import yaml

MODULE = Path(__file__).resolve().parents[1] / "gate_runner_presence.py"
SPEC = importlib.util.spec_from_file_location("gate_runner_presence", MODULE)
assert SPEC and SPEC.loader
grp = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = grp
SPEC.loader.exec_module(grp)


class SummaryContract(unittest.TestCase):
    def _run(
        self, evaluation: grp.Evaluation, gate_on: str | None = None
    ) -> tuple[int, str]:
        output = io.StringIO()
        argv = [str(MODULE)]
        if gate_on:
            argv += ["--gate-on", gate_on]
        with (
            patch.object(grp, "evaluate", return_value=evaluation),
            patch.object(sys, "argv", argv),
            redirect_stdout(output),
        ):
            rc = grp.main()
        return rc, output.getvalue()

    @staticmethod
    def _evaluation(
        aggregate: str,
        surfaces: list[grp.Surface],
        *,
        alert: bool = False,
        notice: bool = False,
        summary: str | None = None,
    ) -> grp.Evaluation:
        return grp.Evaluation(
            aggregate=aggregate,
            state="test-state",
            summary=summary or grp.render_summary(aggregate, surfaces),
            surfaces=surfaces,
            alert=alert,
            notice=notice,
            actions=grp.ActionsState(True),
        )

    def test_present_result_is_clean_and_publishes_a_populated_summary(self) -> None:
        surfaces = [
            grp.Surface(name, grp.PRESENT, "confirmed")
            for name in ("installation", "credentials", "service", "registration")
        ]
        evaluation = self._evaluation(grp.PRESENT, surfaces)
        rc, output = self._run(evaluation)
        self.assertEqual(rc, 0)
        self.assertIn(
            "summary=PRESENT: installation, credentials, service, and registration confirmed",
            output,
        )
        self.assertNotIn("{summary}", output)

    def test_fault_result_names_each_failed_surface_in_one_summary_line(self) -> None:
        surfaces = [
            grp.Surface("installation", grp.ABSENT, "distribution missing"),
            grp.Surface("credentials", grp.PRESENT, "confirmed"),
            grp.Surface("service", grp.NO_RESULT, "could not query\nuser service"),
            grp.Surface("registration", grp.PRESENT, "confirmed"),
        ]
        evaluation = self._evaluation(grp.NO_RESULT, surfaces, alert=True)
        rc, output = self._run(evaluation)
        self.assertEqual(rc, 2)
        summary = [line for line in output.splitlines() if line.startswith("summary=")]
        self.assertEqual(len(summary), 1, output)
        self.assertIn("installation=ABSENT: distribution missing", summary[0])
        self.assertIn("service=NO_RESULT: could not query user service", summary[0])

    def test_inconsistent_result_refuses_instead_of_emitting_an_empty_finding(self) -> None:
        surfaces = [
            grp.Surface(name, grp.PRESENT, "confirmed")
            for name in ("installation", "credentials", "service", "registration")
        ]
        with self.assertRaisesRegex(ValueError, "no non-PRESENT surface"):
            grp.render_summary(grp.NO_RESULT, surfaces)

    def test_alert_and_notice_are_independent_emission_paths(self) -> None:
        surface = grp.Surface("registration", grp.PRESENT, "online")
        evaluation = self._evaluation(
            grp.PRESENT,
            [surface],
            notice=True,
            summary="PRESENT: expected runner online on another host",
        )
        alert_rc, _ = self._run(evaluation, "alert")
        notice_rc, output = self._run(evaluation, "notice")
        self.assertEqual(alert_rc, 0)
        self.assertEqual(notice_rc, 2)
        self.assertIn("summary=PRESENT: expected runner online on another host", output)


class ScopeEvaluation(unittest.TestCase):
    def _args(self) -> object:
        return type(
            "Args",
            (),
            {
                "install_root": "/does/not/exist",
                "service": "hermit-gate-runner.service",
                "system_service": False,
                "repo": "rrnewton/hermit",
                "runner_name": "hermit-gate-newton",
                "offline": False,
            },
        )()

    @staticmethod
    def _runner(status: str = "online") -> dict:
        return {
            "id": 24,
            "name": "hermit-gate-newton",
            "status": status,
            "labels": [{"name": "self-hosted"}, {"name": "gate"}],
        }

    def test_non_owning_host_reports_repository_truth_not_local_absence(self) -> None:
        service = grp.Surface(
            "service", grp.ABSENT, "not installed", {"UnitFileState": "not-found"}
        )
        actions = grp.ActionsState(
            False,
            directive="deactivate-gh-actions-on-hermit-main-and-dev-hermit-parent",
            queued_merge_gate_runs=1,
            zero_job_runs=1,
        )
        with (
            patch.object(grp, "check_service", return_value=(service, None)),
            patch.object(
                grp, "check_installation",
                return_value=grp.Surface("installation", grp.ABSENT, "missing"),
            ),
            patch.object(
                grp, "check_credentials",
                return_value=(grp.Surface("credentials", grp.ABSENT, "missing"), {}),
            ),
            patch.object(grp, "repository_runners", return_value=([self._runner()], "")),
            patch.object(grp, "check_actions", return_value=actions),
            patch.object(grp.socket, "gethostname", return_value="devbig030.example"),
        ):
            evaluation = grp.evaluate(self._args())

        self.assertEqual(evaluation.aggregate, grp.PRESENT)
        self.assertEqual(evaluation.state, "non-owning-host")
        self.assertFalse(evaluation.alert)
        self.assertTrue(evaluation.notice)
        self.assertEqual([surface.name for surface in evaluation.surfaces], ["registration"])
        self.assertIn("host devbig030 carries no local", evaluation.summary)
        self.assertIn("zero admitted jobs=1", evaluation.summary)
        self.assertIn("expected while disabled, neither pass nor failure", evaluation.summary)

    def test_local_trace_with_real_absence_keeps_all_four_surfaces_loud(self) -> None:
        service = grp.Surface(
            "service", grp.ABSENT, "enabled but failed", {"UnitFileState": "enabled"}
        )
        installation = grp.Surface("installation", grp.ABSENT, "distribution missing")
        credentials = grp.Surface("credentials", grp.ABSENT, "credentials missing")
        with (
            patch.object(grp, "check_service", return_value=(service, None)),
            patch.object(grp, "check_installation", return_value=installation),
            patch.object(grp, "check_credentials", return_value=(credentials, {})),
            patch.object(
                grp, "repository_runners", return_value=([self._runner("offline")], "")
            ),
            patch.object(grp, "check_actions", return_value=grp.ActionsState(True)),
            patch.object(grp.socket, "gethostname", return_value="devbig014.example"),
        ):
            evaluation = grp.evaluate(self._args())

        self.assertEqual(evaluation.aggregate, grp.NO_RESULT)
        self.assertTrue(evaluation.alert)
        self.assertEqual(
            [surface.name for surface in evaluation.surfaces],
            ["installation", "credentials", "service", "registration"],
        )
        registration = evaluation.surfaces[-1]
        self.assertEqual(registration.verdict, grp.NO_RESULT)
        self.assertIn("credentials surface failed first", registration.detail)

    def test_unavailable_repository_authority_is_not_inferred_from_empty_host(self) -> None:
        service = grp.Surface(
            "service", grp.ABSENT, "not installed", {"UnitFileState": "not-found"}
        )
        with (
            patch.object(grp, "check_service", return_value=(service, None)),
            patch.object(
                grp, "check_installation",
                return_value=grp.Surface("installation", grp.ABSENT, "missing"),
            ),
            patch.object(
                grp, "check_credentials",
                return_value=(grp.Surface("credentials", grp.ABSENT, "missing"), {}),
            ),
            patch.object(
                grp, "repository_runners", return_value=(None, "GitHub unavailable")
            ),
            patch.object(
                grp, "check_actions",
                return_value=grp.ActionsState(None, detail="GitHub unavailable"),
            ),
        ):
            evaluation = grp.evaluate(self._args())

        self.assertEqual(evaluation.aggregate, grp.NO_RESULT)
        self.assertTrue(evaluation.alert)
        self.assertTrue(evaluation.notice)
        self.assertIn("could not read repository runner list", evaluation.summary)


class TickWiring(unittest.TestCase):
    def test_action_and_note_paths_remain_distinct_and_captured(self) -> None:
        config_path = MODULE.parent / "tick-hub.yaml"
        config = yaml.safe_load(config_path.read_text())
        reminders = {row["name"]: row for row in config["reminders"]}

        alert = reminders["gate_runner_presence"]
        self.assertIn("--gate-on alert", alert["gate"]["cmd"])
        self.assertTrue(alert["gate"]["capture"])
        self.assertEqual(alert["emit"]["kind"], "action")
        self.assertIn("{summary}", alert["emit"]["title"])

        notice = reminders["gate_runner_presence_scope"]
        self.assertIn("--gate-on notice", notice["gate"]["cmd"])
        self.assertTrue(notice["gate"]["capture"])
        self.assertEqual(notice["emit"]["kind"], "note")
        self.assertIn("{summary}", notice["emit"]["title"])


if __name__ == "__main__":
    unittest.main()
