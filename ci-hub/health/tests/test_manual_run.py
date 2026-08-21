#!/usr/bin/env python3
"""Unit tests for the pure parts of ci-hub/health/manual_run.py.

The network paths are not mocked here: what these guard is the reasoning that
a wrong answer would hide -- that a queued run with zero jobs is reported as
its own verdict rather than as "still running", that a workflow name the caller
typed and this tool does not recognise is a refusal rather than a skip, and
that a file's automatic triggers are read out of the file rather than assumed.
"""

from __future__ import annotations

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import manual_run  # noqa: E402


class TriggerReadingTest(unittest.TestCase):
    def test_reads_every_event_key(self) -> None:
        text = (
            "name: X\n"
            "on:\n"
            "  pull_request:\n"
            "  push:\n"
            "    branches: [main]\n"
            "  workflow_dispatch:\n"
            "\n"
            "jobs:\n"
        )
        self.assertEqual(
            manual_run.triggers_of(text),
            ["pull_request", "push", "workflow_dispatch"],
        )

    def test_comments_inside_the_on_block_are_not_events(self) -> None:
        text = (
            "on:\n"
            "  # NO pull_request trigger (owner directive)\n"
            "  workflow_dispatch:\n"
            "jobs:\n"
        )
        self.assertEqual(manual_run.triggers_of(text), ["workflow_dispatch"])

    def test_nested_keys_are_not_events(self) -> None:
        text = (
            "on:\n"
            "  schedule:\n"
            '    - cron: "17 08 * * *"\n'
            "  workflow_dispatch:\n"
            "    inputs:\n"
            "      lane:\n"
            "        required: true\n"
            "jobs:\n"
        )
        self.assertEqual(manual_run.triggers_of(text), ["schedule", "workflow_dispatch"])

    def test_a_file_with_no_on_block_reports_nothing_rather_than_raising(self) -> None:
        self.assertEqual(manual_run.triggers_of("name: X\njobs:\n"), [])

    def test_automatic_triggers_are_named_not_counted(self) -> None:
        self.assertEqual(
            manual_run.automatic_triggers(
                ["pull_request", "workflow_dispatch", "schedule", "merge_group"]
            ),
            ["pull_request", "schedule", "merge_group"],
        )

    def test_manual_only_file_has_no_automatic_trigger(self) -> None:
        self.assertEqual(manual_run.automatic_triggers(["workflow_dispatch"]), [])


class ClassifyTest(unittest.TestCase):
    def test_zero_job_queued_run_is_its_own_verdict(self) -> None:
        # This is the disabled-Actions trap. Reporting it as PENDING is how a
        # dispatch that started nothing looks like a dispatch that started.
        run = {"status": "queued", "conclusion": None, "job_count": 0}
        self.assertEqual(manual_run.classify(run), manual_run.NO_JOBS)

    def test_queued_run_that_has_jobs_is_pending(self) -> None:
        run = {"status": "queued", "conclusion": None, "job_count": 3}
        self.assertEqual(manual_run.classify(run), manual_run.PENDING)

    def test_unknown_job_count_is_not_reported_as_no_jobs(self) -> None:
        run = {"status": "queued", "conclusion": None}
        self.assertEqual(manual_run.classify(run), manual_run.PENDING)

    def test_success_and_failure(self) -> None:
        self.assertEqual(
            manual_run.classify({"status": "completed", "conclusion": "success"}),
            manual_run.GREEN,
        )
        for bad in ("failure", "timed_out", "startup_failure", "action_required"):
            self.assertEqual(
                manual_run.classify({"status": "completed", "conclusion": bad}),
                manual_run.RED,
                bad,
            )

    def test_cancelled_is_neither_green_nor_red(self) -> None:
        self.assertEqual(
            manual_run.classify({"status": "completed", "conclusion": "cancelled"}),
            manual_run.OTHER,
        )

    def test_in_progress_is_running(self) -> None:
        self.assertEqual(
            manual_run.classify({"status": "in_progress", "conclusion": None}),
            manual_run.RUNNING,
        )


class SelectionTest(unittest.TestCase):
    def workflows(self) -> list:
        return [
            manual_run.Workflow(1, "Portability", ".github/workflows/portability.yml", "active"),
            manual_run.Workflow(2, "Docs", ".github/workflows/docs.yml", "active"),
            manual_run.Workflow(3, "Old", ".github/workflows/old.yml", "disabled_manually"),
        ]

    def test_default_is_every_active_workflow(self) -> None:
        chosen = manual_run.select_workflows(self.workflows(), [])
        self.assertEqual([w.file for w in chosen], ["portability.yml", "docs.yml"])

    def test_a_name_matches_file_path_or_display_name(self) -> None:
        for spelling in ("portability.yml", ".github/workflows/portability.yml", "Portability"):
            chosen = manual_run.select_workflows(self.workflows(), [spelling])
            self.assertEqual([w.id for w in chosen], [1], spelling)

    def test_an_unknown_name_refuses_instead_of_skipping(self) -> None:
        with self.assertRaises(manual_run.ManualRunError) as caught:
            manual_run.select_workflows(self.workflows(), ["nope.yml"])
        self.assertIn("nope.yml", str(caught.exception))
        self.assertIn("portability.yml", str(caught.exception))


class RepoAndTagTest(unittest.TestCase):
    def test_repo_must_be_owner_slash_repo(self) -> None:
        self.assertEqual(manual_run.parse_repo("rrnewton/hermit"), "rrnewton/hermit")
        for bad in ("hermit", "", "a/b/c"):
            with self.assertRaises(manual_run.ManualRunError):
                manual_run.parse_repo(bad)

    def test_tag_name_is_the_first_twelve_hex_of_the_sha(self) -> None:
        self.assertEqual(
            manual_run.tag_for("0f1f6cd0e01446dd922f6a223bb815ef442fcbd8"),
            "manual-run/0f1f6cd0e014",
        )

    def test_a_non_sha_refuses(self) -> None:
        with self.assertRaises(manual_run.ManualRunError):
            manual_run.tag_for("main")


if __name__ == "__main__":
    unittest.main()
