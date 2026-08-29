#!/usr/bin/env python3

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "signal_crosscheck.py"
SPEC = importlib.util.spec_from_file_location("signal_crosscheck", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
signal_crosscheck = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signal_crosscheck)


def base_report() -> dict:
    return {
        "unavailable": {},
        "validate_status": {
            "verdict": "NOT-VALIDATED",
            "qualifying_count": 0,
            "failed_record_count": 0,
            "disqualified_count": 0,
            "withheld_nonpass_record_count": 0,
        },
        "ledger": {"exact_rows": 0},
        "scorecard": {
            "status_table_matches_cells": True,
            "measurement_table_matches_cells": True,
            "prose_matches_cells": True,
            "projection_source_matches": True,
            "projection_row_count_current": True,
        },
        "series": {"published_ref_sha": "f" * 40},
        "run_handles": {"unmatched_terminal_handles": []},
        "github_status": {"published": False, "state": None},
    }


class SignalCrosscheckTests(unittest.TestCase):
    def test_empty_combined_status_is_absence_not_pending(self) -> None:
        status = signal_crosscheck.github_status_report(
            {"state": "pending", "total_count": 0, "statuses": []}
        )
        self.assertFalse(status["published"])
        self.assertIsNone(status["state"])
        self.assertEqual(signal_crosscheck.reconcile(base_report()), [])

    def test_exact_local_validation_context_controls_the_status(self) -> None:
        status = signal_crosscheck.github_status_report({
            "state": "success",
            "total_count": 2,
            "statuses": [
                {"context": "Local validation details", "state": "success"},
                {
                    "context": "Local validation",
                    "state": "failure",
                    "updated_at": "2026-08-29T00:00:00Z",
                },
            ],
        })
        self.assertTrue(status["published"])
        self.assertEqual(status["state"], "failure")

        report = base_report()
        report["validate_status"]["verdict"] = "VALIDATED"
        report["github_status"] = status
        self.assertIn(
            "GitHub Local validation says failure but validate-status says VALIDATED",
            signal_crosscheck.reconcile(report),
        )

        with self.assertRaisesRegex(
            signal_crosscheck.CrosscheckError, "unsupported state 'neutral'"
        ):
            signal_crosscheck.github_status_report({
                "state": "neutral",
                "total_count": 1,
                "statuses": [{
                    "context": "Local validation",
                    "state": "neutral",
                }],
            })

    def test_aligned_qualifying_receipt_and_published_status_agree(self) -> None:
        report = base_report()
        report["validate_status"].update({
            "verdict": "VALIDATED",
            "qualifying_count": 1,
        })
        report["ledger"]["exact_rows"] = 1
        report["github_status"] = {
            "published": True,
            "state": "success",
        }
        self.assertEqual(signal_crosscheck.reconcile(report), [])

    def test_scorecard_projection_mutation_is_a_named_disagreement(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection_row_count_current": False,
            "projection": {
                "source": "../../series",
                "refreshed_at": "2026-08-27T09:23:50Z",
                "rows_read": 9,
            },
            "current_published_series_rows": 10,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("STALE, not arithmetically contradictory", issues[0])
        self.assertIn("read 9 row(s)", issues[0])
        self.assertIn("now has 10 logical row(s)", issues[0])
        self.assertIn("does not record the parent commit or event frontier", issues[0])

    def test_unexpected_scorecard_source_is_distinct_from_staleness(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection_source_matches": False,
            "projection": {"source": "/tmp/other-series", "rows_read": 10},
            "current_published_series_rows": 10,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("names an unexpected series source", issues[0])

    def test_terminal_run_without_ledger_is_not_never_attempted(self) -> None:
        report = base_report()
        report["run_handles"]["unmatched_terminal_handles"] = [{
            "unit": "validate-example.service",
            "state": "completed",
            "admission": "admitted",
        }]
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("validate-example.service", issues[0])
        self.assertIn("state=completed", issues[0])

    def test_terminal_handle_never_grants_a_receipt(self) -> None:
        report = base_report()
        passed_handle = {
            "unit": "validate-passed.service",
            "state": "completed",
            "result": "success",
            "exit_code": 0,
            "admission": "admitted",
            "ledger_event": False,
        }
        report["run_handles"]["terminal_handles"] = [passed_handle]
        report["run_handles"]["unmatched_terminal_handles"] = [passed_handle]
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(report["validate_status"]["verdict"], "NOT-VALIDATED")
        self.assertEqual(len(issues), 1)
        self.assertIn("terminal validation run has no ledger event", issues[0])

    def test_validate_status_and_ledger_count_mutation_refuses(self) -> None:
        report = base_report()
        report["ledger"]["exact_rows"] = 1
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(
            issues,
            ["validate-status accounts for 0 exact row(s), canonical ledger contains 1"],
        )

    def test_validate_status_ledger_lag_is_never_reported_as_agreement(self) -> None:
        report = base_report()
        report["validate_status"]["verdict"] = "LEDGER-LAGGING"
        self.assertIn(
            "validate-status cannot establish the ledger: LEDGER-LAGGING",
            signal_crosscheck.reconcile(report),
        )

        report["validate_status"]["verdict"] = "FUTURE-VERDICT"
        self.assertIn(
            "validate-status emitted unsupported verdict 'FUTURE-VERDICT'",
            signal_crosscheck.reconcile(report),
        )

    def test_failure_and_withheld_counts_do_not_double_count_disqualified_rows(self) -> None:
        report = base_report()
        report["ledger"]["exact_rows"] = 2
        report["validate_status"].update({
            "disqualified_count": 2,
            "failed_record_count": 1,
            "withheld_nonpass_record_count": 1,
        })
        self.assertEqual(signal_crosscheck.reconcile(report), [])

    def test_series_union_collapses_identical_duplicate_and_refuses_conflict(self) -> None:
        event = {"event_id": "one", "emitted_at": "2026-08-29T00:00:00Z"}
        self.assertEqual(
            signal_crosscheck.logical_series_rows([event], [dict(event)]),
            [event],
        )
        conflicting = dict(event)
        conflicting["emitted_at"] = "2026-08-29T00:00:01Z"
        with self.assertRaises(signal_crosscheck.ledger_store.ConflictingEventId) as raised:
            signal_crosscheck.logical_series_rows([event], [conflicting])
        self.assertIn("appears twice with different bodies", str(raised.exception))

    def test_scorecard_reads_exact_git_objects_and_checks_both_tables(self) -> None:
        cells = {
            "schema": 6,
            "projection": {
                "source": "../../series",
                "refreshed_at": "2026-08-29T00:00:00Z",
                "rows_read": 2,
                "pre_series_corpus": False,
            },
            "cells": [
                {
                    "status": "green",
                    "measurement": "measured-and-passed",
                    "observations": [{"result": "matched"}],
                    "last_tested": {"detcore_tree": "tree"},
                },
                {
                    "status": "red",
                    "measurement": "never-measured",
                    "observations": [],
                },
            ],
        }
        markdown = "\n".join([
            "The current **0** manifest-disabled combinations as **Not applicable**.",
            "The current green/`never-measured` count is **0**, and the current "
            "red/`measured-and-passed` count is **0**.",
            "| **Total** | **1** | **1** | **0** | **2** |",
            "| **Total** | **1** | **1** | **0** | **0** | **0** | **2** |",
        ])
        original = signal_crosscheck.git_text
        try:
            def fake_git(_repo: Path, *args: str) -> str:
                target = args[-1]
                if target.endswith("cells.json"):
                    return json.dumps(cells)
                if target.endswith("SCORECARD.md"):
                    return markdown
                if target.endswith(":detcore"):
                    return "tree\n"
                raise AssertionError(args)

            signal_crosscheck.git_text = fake_git
            result = signal_crosscheck.scorecard_report(
                Path("hermit"), "a" * 40, published_series_rows=2
            )
        finally:
            signal_crosscheck.git_text = original
        self.assertTrue(result["status_table_matches_cells"])
        self.assertTrue(result["measurement_table_matches_cells"])
        self.assertTrue(result["prose_matches_cells"])
        self.assertTrue(result["projection_source_matches"])
        self.assertTrue(result["projection_row_count_current"])
        self.assertEqual(result["fresh_observed_cells"], 1)

        mutated = markdown.replace(
            "green/`never-measured` count is **0**",
            "green/`never-measured` count is **1**",
        )
        try:
            signal_crosscheck.git_text = lambda _repo, *args: (
                json.dumps(cells)
                if args[-1].endswith("cells.json")
                else mutated if args[-1].endswith("SCORECARD.md") else "tree\n"
            )
            result = signal_crosscheck.scorecard_report(
                Path("hermit"), "a" * 40, published_series_rows=2
            )
        finally:
            signal_crosscheck.git_text = original
        self.assertFalse(result["prose_matches_cells"])

    def test_run_handle_matching_uses_typed_projection_and_log_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            for state in ("completed", "killed", "unknown", "failed"):
                (run_root / f"{state}.json").write_text("{}")
            (run_root / "frozen.json").write_text("{}")
            (run_root / "bench.json").write_text("{}")
            original_read = signal_crosscheck.run_registry.read_record
            original_projection = signal_crosscheck.run_registry.projection
            try:
                def fake_read(path: Path) -> dict:
                    state = path.stem
                    return {
                        "unit": f"validate-{state}.service",
                        "log": f"/tmp/validate-{state}.log",
                        "result": "failure",
                        "exit_code": 1,
                        "fixture_state": state,
                    }

                def fake_projection(raw: dict) -> dict:
                    state = raw["fixture_state"]
                    return {
                        "target": "a" * 40,
                        "repo": "rrnewton/hermit",
                        "kind": (
                            "frozen-validate"
                            if state == "frozen"
                            else "bench" if state == "bench" else "validate"
                        ),
                        "state": "completed" if state in {"frozen", "bench"} else state,
                        "unit": raw["unit"],
                        "admission_result": {"state": "admitted"},
                    }

                signal_crosscheck.run_registry.read_record = fake_read
                signal_crosscheck.run_registry.projection = fake_projection
                for path in run_root.glob("*.json"):
                    path.write_text(json.dumps({
                        "target": "a" * 40,
                        "repo": "rrnewton/hermit",
                    }))
                matched = signal_crosscheck.run_handle_report(
                    root,
                    "a" * 40,
                    "rrnewton/hermit",
                    [
                        {"log_file": f"/tmp/validate-{state}.log"}
                        for state in ("completed", "killed", "unknown", "failed")
                    ],
                )
                unmatched = signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                )
            finally:
                signal_crosscheck.run_registry.read_record = original_read
                signal_crosscheck.run_registry.projection = original_projection
        self.assertEqual(matched["unmatched_terminal_handles"], [])
        self.assertEqual(len(matched["terminal_handles"]), 4)
        self.assertEqual(len(unmatched["unmatched_terminal_handles"]), 4)

    def test_malformed_handle_for_another_target_is_out_of_scope(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            path = run_root / "other.json"
            path.write_text(json.dumps({
                "target": "b" * 40,
                "repo": "rrnewton/hermit",
            }))
            original_read = signal_crosscheck.run_registry.read_record
            try:
                signal_crosscheck.run_registry.read_record = lambda _path: (_ for _ in ()).throw(
                    AssertionError("out-of-scope handle must not be parsed")
                )
                report = signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                )
            finally:
                signal_crosscheck.run_registry.read_record = original_read
        self.assertEqual(report["terminal_handles"], [])
        self.assertEqual(report["unreadable_handles"], [])


if __name__ == "__main__":
    unittest.main()
