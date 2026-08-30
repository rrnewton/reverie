#!/usr/bin/env python3

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


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
            "projection": {
                "source": "series",
                "source_commit": "a" * 40,
                "source_tree": "b" * 40,
                "rows_read": 1,
            },
            "projection_source_declared": True,
            "projection_source_path_is_canonical": True,
            "projection_source_commit_on_published_history": True,
            "projection_source_tree": "b" * 40,
            "canonical_source_tree": "b" * 40,
            "projection_source_tree_matches": True,
            "projection_rows_read_matches_source": True,
            "projection_series_relation": "current",
            "projection_series_comparison": {
                "source_events": 1,
                "current_events": 1,
                "added_events": 0,
                "missing_events": 0,
                "replaced_events": 0,
            },
            "current_published_series_commit": "f" * 40,
        },
        "series": {"published_ref_sha": "f" * 40},
        "run_handles": {
            "admitted_without_ledger_handles": [],
            "terminal_handles_missing_admission": [],
        },
        "github_status": {"published": False, "state": None},
    }


class SignalCrosscheckTests(unittest.TestCase):
    def test_linked_tool_checkout_uses_primary_state_authority(self) -> None:
        sha = "a" * 40
        other_sha = "b" * 40
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "primary"
            linked = root / "linked"
            primary.mkdir()
            subprocess.run(
                ["git", "init", "-q", "-b", "main"], cwd=primary, check=True
            )
            subprocess.run(
                ["git", "config", "user.email", "crosscheck@example.invalid"],
                cwd=primary,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "crosscheck fixture"],
                cwd=primary,
                check=True,
            )
            (primary / "tracked.txt").write_text("fixture\n")
            subprocess.run(["git", "add", "tracked.txt"], cwd=primary, check=True)
            subprocess.run(
                ["git", "commit", "-qm", "fixture"], cwd=primary, check=True
            )
            subprocess.run(
                ["git", "worktree", "add", "-q", "-b", "linked", str(linked)],
                cwd=primary,
                check=True,
            )

            canonical_log = "/tmp/validate-canonical.log"
            primary_runs = primary / "ignored" / "validate" / "runs"
            primary_runs.mkdir(parents=True)
            handles = {
                "canonical": {
                    "target": sha,
                    "repo": "rrnewton/hermit",
                    "host": "hosta",
                    "unit": "validate-canonical.service",
                    "log": canonical_log,
                },
                "other-sha": {
                    "target": other_sha,
                    "repo": "rrnewton/hermit",
                    "unit": "validate-other-sha.service",
                    "log": "/tmp/validate-other-sha.log",
                },
                "other-repo": {
                    "target": sha,
                    "repo": "rrnewton/reverie",
                    "unit": "validate-other-repo.service",
                    "log": "/tmp/validate-other-repo.log",
                },
            }
            for name, value in handles.items():
                (primary_runs / f"{name}.json").write_text(json.dumps(value))

            slot_runs = linked / "ignored" / "validate" / "runs"
            slot_runs.mkdir(parents=True)
            (slot_runs / "poison.json").write_text(json.dumps({
                "target": sha,
                "repo": "rrnewton/hermit",
                "unit": "validate-slot-poison.service",
                "log": "/tmp/validate-slot-poison.log",
            }))

            original = {
                "commit": sha,
                "repo": "hermit",
                "run_id": "same-run",
                "run_number": 42,
                "host": "hosta.example.com",
                "record_id": "original",
                "log_file": "/tmp/validate-original.log",
            }
            corrected = {
                **original,
                "record_id": "corrected",
                "corrects": "original",
                "log_file": canonical_log,
            }
            rows = [
                original,
                corrected,
                {
                    "commit": other_sha,
                    "repo": "hermit",
                    "run_id": "other-sha",
                    "record_id": "other-sha",
                },
                {
                    "commit": sha,
                    "repo": "reverie",
                    "run_id": "other-repo",
                    "record_id": "other-repo",
                },
            ]
            projected_units: list[str] = []

            def ledger_rows(*, root: Path) -> list[dict]:
                self.assertEqual(root.resolve(), primary.resolve())
                return rows

            def validate_status(
                tool_root: Path, state_root: Path, target: str, repo: str
            ) -> dict:
                self.assertEqual(tool_root.resolve(), linked.resolve())
                self.assertEqual(state_root.resolve(), primary.resolve())
                self.assertEqual((target, repo), (sha, "rrnewton/hermit"))
                return {
                    "verdict": "VALIDATED",
                    "qualifying_count": 1,
                    "failed_record_count": 0,
                    "disqualified_count": 0,
                    "withheld_nonpass_record_count": 0,
                }

            def series_report(
                tool_root: Path, state_root: Path, target: str
            ) -> tuple[dict, signal_crosscheck.canonical_series.PublishedSnapshot]:
                self.assertEqual(tool_root.resolve(), linked.resolve())
                self.assertEqual(state_root.resolve(), primary.resolve())
                self.assertEqual(target, sha)
                return (
                    {"published_ref_sha": "f" * 40},
                    signal_crosscheck.canonical_series.PublishedSnapshot("f" * 40, []),
                )

            def projection(raw: dict) -> dict:
                projected_units.append(raw["unit"])
                return {
                    "target": raw["target"],
                    "repo": raw["repo"],
                    "kind": "validate",
                    "state": "completed",
                    "unit": raw["unit"],
                    "host": raw.get("host"),
                    "admission_result": {"state": "admitted", "run_number": 42},
                }

            with (
                mock.patch.object(signal_crosscheck.canonical_ledger, "rows", ledger_rows),
                mock.patch.object(
                    signal_crosscheck, "validate_status_report", validate_status
                ),
                mock.patch.object(signal_crosscheck, "series_report", series_report),
                mock.patch.object(
                    signal_crosscheck,
                    "scorecard_report",
                    return_value=dict(base_report()["scorecard"]),
                ),
                mock.patch.object(
                    signal_crosscheck,
                    "collect_github_status",
                    return_value={"published": False, "state": None},
                ),
                mock.patch.object(signal_crosscheck.run_registry, "projection", projection),
            ):
                report = signal_crosscheck.build_report(
                    linked, sha, "rrnewton/hermit"
                )

        self.assertEqual(report["tool_root"], str(linked.resolve()))
        self.assertEqual(report["state_root"], str(primary.resolve()))
        self.assertEqual(report["git_common_dir"], str((primary / ".git").resolve()))
        self.assertEqual(report["ledger"]["historical_rows"], 4)
        self.assertEqual(report["ledger"]["repository_historical_rows"], 3)
        self.assertEqual(report["ledger"]["logical_rows"], 2)
        self.assertEqual(report["ledger"]["exact_rows"], 1)
        self.assertEqual(report["ledger"]["exact_run_ids"], ["same-run"])
        self.assertEqual(projected_units, ["validate-canonical.service"])
        self.assertEqual(len(report["run_handles"]["terminal_handles"]), 1)
        self.assertEqual(
            report["run_handles"]["admitted_without_ledger_handles"], []
        )
        self.assertEqual(
            report["run_handles"]["terminal_handles_missing_admission"], []
        )
        self.assertNotIn("validate-slot-poison.service", json.dumps(report))
        self.assertEqual(report["result"], "AGREE")

    def test_malformed_state_authority_refuses_before_reading_stores(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = signal_crosscheck.build_report(
                root, "a" * 40, "rrnewton/hermit"
            )
        self.assertEqual(report["result"], "DISAGREE")
        self.assertIsNone(report["state_root"])
        self.assertIn("state-root", report["unavailable"])
        self.assertEqual(len(report["issues"]), 1)
        self.assertIn("state-root unavailable", report["issues"][0])

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

    def test_unchanged_strict_extension_is_stale(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection": {
                "source": "series",
                "source_commit": "a" * 40,
                "source_tree": "b" * 40,
                "refreshed_at": "2026-08-27T09:23:50Z",
                "rows_read": 9,
            },
            "projection_source_rows": 9,
            "projection_rows_read_matches_source": True,
            "projection_series_relation": "stale",
            "projection_series_comparison": {
                "source_events": 9,
                "current_events": 10,
                "added_events": 1,
                "missing_events": 0,
                "replaced_events": 0,
            },
            "current_published_series_rows": 10,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("projection is STALE", issues[0])
        self.assertIn("unchanged strict subset", issues[0])
        self.assertIn("9 -> 10 events", issues[0])

    def test_rows_read_is_an_invariant_not_the_snapshot_comparator(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection": {
                "source": "series",
                "source_commit": "a" * 40,
                "source_tree": "b" * 40,
                "rows_read": 8,
            },
            "projection_source_rows": 9,
            "projection_rows_read_matches_source": False,
            "projection_series_relation": "stale",
            "projection_series_comparison": {
                "source_events": 9,
                "current_events": 10,
                "added_events": 1,
                "missing_events": 0,
                "replaced_events": 0,
            },
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("projection is INCORRECT", issues[0])
        self.assertIn("rows_read is a source-snapshot invariant", issues[0])

    def test_equal_count_replacement_is_incorrect(self) -> None:
        source = [{"event_id": "one", "value": "before"}]
        current = [{"event_id": "one", "value": "after"}]
        comparison = signal_crosscheck.compare_series_snapshots(source, current)
        self.assertEqual(comparison["source_events"], comparison["current_events"])
        self.assertEqual(comparison["relation"], "incorrect")
        self.assertEqual(comparison["replaced_events"], 1)
        report = base_report()
        report["scorecard"].update({
            "projection": {
                "source": "series",
                "source_commit": "a" * 40,
                "source_tree": "b" * 40,
                "rows_read": 1,
            },
            "projection_source_rows": 1,
            "projection_series_relation": comparison["relation"],
            "projection_series_comparison": comparison,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("projection is INCORRECT", issues[0])
        self.assertIn("replaced=1", issues[0])

    def test_non_extension_with_more_current_rows_is_incorrect(self) -> None:
        source = [
            {"event_id": "one", "value": 1},
            {"event_id": "two", "value": 2},
        ]
        current = [
            {"event_id": "one", "value": 1},
            {"event_id": "three", "value": 3},
            {"event_id": "four", "value": 4},
        ]
        comparison = signal_crosscheck.compare_series_snapshots(source, current)
        self.assertEqual(comparison["relation"], "incorrect")
        self.assertEqual(comparison["missing_events"], 1)
        self.assertEqual(comparison["added_events"], 2)
        report = base_report()
        report["scorecard"].update({
            "projection": {
                "source": "series",
                "source_commit": "a" * 40,
                "source_tree": "b" * 40,
                "rows_read": 2,
            },
            "projection_source_rows": 2,
            "projection_series_relation": comparison["relation"],
            "projection_series_comparison": comparison,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("missing=1", issues[0])
        self.assertIn("current-only=2", issues[0])

    def test_equal_canonical_event_maps_are_current(self) -> None:
        source = [{"event_id": "one", "nested": {"a": 1, "b": 2}}]
        current = [{"nested": {"b": 2, "a": 1}, "event_id": "one"}]
        comparison = signal_crosscheck.compare_series_snapshots(source, current)
        self.assertEqual(comparison["relation"], "current")

    def test_missing_scorecard_source_declaration_is_distinct_from_staleness(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection_source_declared": False,
            "projection": {"source": "", "rows_read": 10},
            "current_published_series_rows": 10,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("does not name its series source", issues[0])

    def test_legacy_source_spellings_are_unverifiable_until_regenerated(self) -> None:
        for source in ("../../series", "../series"):
            with self.subTest(source=source):
                report = base_report()
                report["scorecard"].update({
                    "projection": {
                        "source": source,
                        "source_commit": "a" * 40,
                        "rows_read": 1,
                    },
                    "projection_source_tree": None,
                    "projection_series_relation": None,
                })
                issues = signal_crosscheck.reconcile(report)
                self.assertEqual(len(issues), 1)
                self.assertIn("records no immutable source_tree", issues[0])

    def test_alternate_source_path_and_tree_each_refuse(self) -> None:
        for mutation, expected in (
            (
                {
                    "projection_source_path_is_canonical": False,
                    "projection": {
                        "source": "alternate-series",
                        "source_commit": "a" * 40,
                        "source_tree": "b" * 40,
                        "rows_read": 1,
                    },
                },
                "non-canonical series path",
            ),
            (
                {
                    "projection_source_tree_matches": False,
                    "projection_source_tree": "c" * 40,
                    "canonical_source_tree": "b" * 40,
                    "projection": {
                        "source": "series",
                        "source_commit": "a" * 40,
                        "source_tree": "c" * 40,
                        "rows_read": 1,
                    },
                },
                "source_tree does not match",
            ),
        ):
            with self.subTest(expected=expected):
                report = base_report()
                report["scorecard"].update(mutation)
                issues = signal_crosscheck.reconcile(report)
                self.assertEqual(len(issues), 1)
                self.assertIn(expected, issues[0])

    def test_canonical_source_identity_qualifies(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection": {
                "source": "series",
                "source_commit": "a" * 40,
                "source_tree": "b" * 40,
                "rows_read": 1,
            },
        })
        self.assertEqual(signal_crosscheck.reconcile(report), [])

    def test_projection_commit_and_tree_are_bound_to_published_parent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            parent = fixture / "parent"
            foreign = fixture / "foreign"
            parent.mkdir()
            foreign.mkdir()

            def git(repo: Path, *args: str) -> str:
                result = subprocess.run(
                    ["git", *args],
                    cwd=repo,
                    check=True,
                    capture_output=True,
                    text=True,
                )
                return result.stdout.strip()

            def initialise(repo: Path, author: str, event: str) -> None:
                git(repo, "init", "-q", "-b", "main")
                git(repo, "config", "user.email", f"{author}@example.invalid")
                git(repo, "config", "user.name", author)
                shard = repo / "series" / "hermit" / "fixture" / "2026-08.jsonl"
                shard.parent.mkdir(parents=True)
                shard.write_text(f'{{"event_id":"{event}"}}\n')
                alternate = (
                    repo / "alternate-series" / "hermit" / "fixture" / "2026-08.jsonl"
                )
                alternate.parent.mkdir(parents=True)
                alternate.write_text('{"event_id":"different"}\n')
                git(repo, "add", "series", "alternate-series")
                git(repo, "commit", "-qm", f"{author} series")

            initialise(parent, "parent", "canonical")
            source_commit = git(parent, "rev-parse", "HEAD")
            source_tree = signal_crosscheck.git_tree_at(
                parent, source_commit, "series"
            )
            alternate_tree = signal_crosscheck.git_tree_at(
                parent, source_commit, "alternate-series"
            )
            self.assertNotEqual(source_tree, alternate_tree)

            (parent / "marker").write_text("published\n")
            git(parent, "add", "marker")
            git(parent, "commit", "-qm", "published frontier")
            published_commit = git(parent, "rev-parse", "HEAD")

            # A later worktree edit cannot change the immutable tree object.
            (parent / "series" / "hermit" / "fixture" / "2026-08.jsonl").write_text(
                "dirty worktree\n"
            )
            self.assertTrue(
                signal_crosscheck.commit_is_ancestor(
                    parent, source_commit, published_commit
                )
            )
            self.assertEqual(
                signal_crosscheck.git_tree_at(parent, source_commit, "series"),
                source_tree,
            )
            parent_link = fixture / "parent-link"
            parent_link.symlink_to(parent, target_is_directory=True)
            self.assertEqual(
                signal_crosscheck.git_tree_at(parent_link, source_commit, "series"),
                source_tree,
            )

            with self.assertRaisesRegex(
                signal_crosscheck.CrosscheckError,
                "git rev-parse --verify",
            ):
                signal_crosscheck.git_tree_at(parent, source_commit, "missing-series")

            with self.assertRaisesRegex(
                signal_crosscheck.CrosscheckError,
                "cannot compare series source commit",
            ):
                signal_crosscheck.commit_is_ancestor(
                    parent, "f" * 40, published_commit
                )

            initialise(foreign, "foreign", "canonical")
            foreign_commit = git(foreign, "rev-parse", "HEAD")
            git(parent, "fetch", "-q", str(foreign), foreign_commit)
            self.assertFalse(
                signal_crosscheck.commit_is_ancestor(
                    parent, foreign_commit, published_commit
                )
            )

        report = base_report()
        report["scorecard"].update({
            "projection_source_commit_on_published_history": False,
            "projection_series_relation": None,
            "projection": {
                "source": "series",
                "source_commit": foreign_commit,
                "source_tree": source_tree,
                "rows_read": 1,
            },
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("not on the current published parent history", issues[0])

    def test_historical_projection_without_source_commit_is_unbound(self) -> None:
        report = base_report()
        report["scorecard"].update({
            "projection": {"source": "../../series", "rows_read": 9},
            "projection_source_commit_on_published_history": None,
            "projection_source_rows": None,
            "projection_rows_read_matches_source": None,
            "projection_series_relation": None,
        })
        issues = signal_crosscheck.reconcile(report)
        self.assertEqual(len(issues), 1)
        self.assertIn("records no immutable source_commit", issues[0])
        self.assertIn("cannot be classified", issues[0])

    def test_terminal_run_without_ledger_is_not_never_attempted(self) -> None:
        report = base_report()
        report["run_handles"]["admitted_without_ledger_handles"] = [{
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
        report["run_handles"]["admitted_without_ledger_handles"] = [passed_handle]
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

    def test_ledger_report_uses_the_current_correction_chain_projection(self) -> None:
        sha = "a" * 40
        original = {
            "commit": sha,
            "repo": "hermit",
            "run_id": "same-run",
            "record_id": "original",
        }
        finalized = {
            "commit": sha,
            "repo": "hermit",
            "run_id": "same-run",
            "record_id": "finalized",
            "corrects": "original",
        }
        independent = {
            "commit": sha,
            "repo": "hermit",
            "run_id": "independent-run",
            "record_id": "independent",
        }
        foreign_same_run_id = {
            "commit": sha,
            "repo": "reverie",
            "run_id": "same-run",
            "record_id": "foreign",
        }
        original_rows = signal_crosscheck.canonical_ledger.rows
        try:
            signal_crosscheck.canonical_ledger.rows = lambda **_kwargs: [
                original,
                finalized,
                independent,
                foreign_same_run_id,
            ]
            report, exact = signal_crosscheck.ledger_report(
                Path("/parent"), sha, "rrnewton/hermit"
            )
        finally:
            signal_crosscheck.canonical_ledger.rows = original_rows
        self.assertEqual(report["historical_rows"], 4)
        self.assertEqual(report["repository_historical_rows"], 3)
        self.assertEqual(report["logical_rows"], 2)
        self.assertEqual(report["exact_rows"], 2)
        self.assertEqual(
            {row["record_id"] for row in exact}, {"finalized", "independent"}
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
        source_commit = "b" * 40
        source_tree = "d" * 40
        current_commit = "c" * 40
        series_rows = [
            {"event_id": "one", "value": 1},
            {"event_id": "two", "value": 2},
        ]
        cells = {
            "schema": 6,
            "projection": {
                "source": "series",
                "source_commit": source_commit,
                "source_tree": source_tree,
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
        original_snapshot_at = signal_crosscheck.canonical_series.published_snapshot_at
        original_commit_is_ancestor = signal_crosscheck.commit_is_ancestor
        original_git_tree_at = signal_crosscheck.git_tree_at
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
            signal_crosscheck.canonical_series.published_snapshot_at = (
                lambda _root, revision: signal_crosscheck.canonical_series.PublishedSnapshot(
                    revision, series_rows
                )
            )
            signal_crosscheck.commit_is_ancestor = (
                lambda _root, _source, _published: True
            )
            signal_crosscheck.git_tree_at = (
                lambda _root, _commit, _path: source_tree
            )
            result = signal_crosscheck.scorecard_report(
                Path("parent"),
                Path("hermit"),
                "a" * 40,
                published_series_snapshot=(
                    signal_crosscheck.canonical_series.PublishedSnapshot(
                        current_commit, list(series_rows)
                    )
                ),
            )
            cells["projection"].update({
                "source": "alternate-series",
                "source_tree": "e" * 40,
            })
            alternate = signal_crosscheck.scorecard_report(
                Path("parent"),
                Path("hermit"),
                "a" * 40,
                published_series_snapshot=(
                    signal_crosscheck.canonical_series.PublishedSnapshot(
                        current_commit, list(series_rows)
                    )
                ),
            )
            cells["projection"].update({
                "source": "series",
                "source_tree": source_tree,
            })
        finally:
            signal_crosscheck.git_text = original
            signal_crosscheck.canonical_series.published_snapshot_at = original_snapshot_at
            signal_crosscheck.commit_is_ancestor = original_commit_is_ancestor
            signal_crosscheck.git_tree_at = original_git_tree_at
        self.assertTrue(result["status_table_matches_cells"])
        self.assertTrue(result["measurement_table_matches_cells"])
        self.assertTrue(result["prose_matches_cells"])
        self.assertTrue(result["projection_source_declared"])
        self.assertTrue(result["projection_source_path_is_canonical"])
        self.assertTrue(result["projection_source_commit_on_published_history"])
        self.assertTrue(result["projection_source_tree_matches"])
        self.assertTrue(result["projection_rows_read_matches_source"])
        self.assertEqual(result["projection_series_relation"], "current")
        self.assertEqual(result["current_published_series_commit"], current_commit)
        self.assertEqual(result["fresh_observed_cells"], 1)
        self.assertFalse(alternate["projection_source_path_is_canonical"])
        self.assertFalse(alternate["projection_source_tree_matches"])
        alternate_report = base_report()
        alternate_report["scorecard"] = alternate
        alternate_issues = signal_crosscheck.reconcile(alternate_report)
        self.assertEqual(len(alternate_issues), 1)
        self.assertIn("non-canonical series path", alternate_issues[0])

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
            signal_crosscheck.canonical_series.published_snapshot_at = (
                lambda _root, revision: signal_crosscheck.canonical_series.PublishedSnapshot(
                    revision, series_rows
                )
            )
            signal_crosscheck.commit_is_ancestor = (
                lambda _root, _source, _published: True
            )
            signal_crosscheck.git_tree_at = (
                lambda _root, _commit, _path: source_tree
            )
            result = signal_crosscheck.scorecard_report(
                Path("parent"),
                Path("hermit"),
                "a" * 40,
                published_series_snapshot=(
                    signal_crosscheck.canonical_series.PublishedSnapshot(
                        current_commit, list(series_rows)
                    )
                ),
            )
        finally:
            signal_crosscheck.git_text = original
            signal_crosscheck.canonical_series.published_snapshot_at = original_snapshot_at
            signal_crosscheck.commit_is_ancestor = original_commit_is_ancestor
            signal_crosscheck.git_tree_at = original_git_tree_at
        self.assertFalse(result["prose_matches_cells"])

    def test_run_handle_matching_uses_typed_projection_and_run_number_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            run_numbers = {
                state: index
                for index, state in enumerate(
                    ("completed", "killed", "unknown", "failed"), start=1
                )
            }
            for state in run_numbers:
                (run_root / f"{state}.json").write_text("{}")
            (run_root / "frozen.json").write_text("{}")
            (run_root / "bench.json").write_text("{}")
            original_projection = signal_crosscheck.run_registry.projection
            try:
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
                        "host": raw["host"],
                        "admission_result": {
                            "state": "admitted",
                            "run_number": raw.get("run_number", 99),
                        },
                    }

                signal_crosscheck.run_registry.projection = fake_projection
                for path in run_root.glob("*.json"):
                    state = path.stem
                    path.write_text(json.dumps({
                        "target": "a" * 40,
                        "repo": "rrnewton/hermit",
                        "host": "hosta",
                        "unit": f"validate-{state}.service",
                        "log": f"/tmp/validate-{state}.log",
                        "result": "failure",
                        "exit_code": 1,
                        "fixture_state": state,
                        "run_number": run_numbers.get(state, 99),
                    }))
                matched = signal_crosscheck.run_handle_report(
                    root,
                    "a" * 40,
                    "rrnewton/hermit",
                    [
                        {"host": "hosta", "run_number": value}
                        for value in run_numbers.values()
                    ],
                )
                unmatched = signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                )
            finally:
                signal_crosscheck.run_registry.projection = original_projection
        self.assertEqual(matched["admitted_without_ledger_handles"], [])
        self.assertEqual(len(matched["terminal_handles"]), 4)
        self.assertEqual(len(unmatched["admitted_without_ledger_handles"]), 4)

    def test_only_exact_terminal_admitted_handles_require_ledger_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            fixtures = {
                "admitted-matched": (
                    "a" * 40,
                    "completed",
                    "admitted",
                    1,
                    "hosta",
                ),
                "admitted-fqdn-matched": (
                    "a" * 40,
                    "completed",
                    "admitted",
                    2,
                    "hosta.example.com",
                ),
                "admitted-cross-machine": (
                    "a" * 40,
                    "completed",
                    "admitted",
                    3,
                    "hosta",
                ),
                "admitted-missing-host": (
                    "a" * 40,
                    "completed",
                    "admitted",
                    6,
                    None,
                ),
                "refused": ("a" * 40, "refused", "refused", None, "hosta"),
                "refused-without-admission": (
                    "a" * 40,
                    "refused",
                    None,
                    None,
                    "hosta",
                ),
                "running": ("a" * 40, "running", "admitted", 4, "hosta"),
                "missing-admission": (
                    "a" * 40,
                    "failed",
                    None,
                    None,
                    "hosta",
                ),
                "other-admitted": (
                    "b" * 40,
                    "completed",
                    "admitted",
                    5,
                    "hosta",
                ),
                "other-refused": (
                    "b" * 40,
                    "refused",
                    "refused",
                    None,
                    "hosta",
                ),
                "other-missing": ("b" * 40, "failed", None, None, "hosta"),
            }
            for name, (target, state, admission, run_number, host) in fixtures.items():
                (run_root / f"{name}.json").write_text(json.dumps({
                    "producer": signal_crosscheck.run_registry.PRODUCER,
                    "target": target,
                    "repo": "rrnewton/hermit",
                    "unit": f"validate-{name}.service",
                    "log": f"/tmp/{name}.log",
                    "result": (
                        "systemd-launch-refused" if state == "refused" else None
                    ),
                    "fixture_state": state,
                    "fixture_admission": admission,
                    "fixture_run_number": run_number,
                    "fixture_host": host,
                }))

            def projection(raw: dict) -> dict:
                self.assertEqual(raw["target"], "a" * 40)
                admission = raw["fixture_admission"]
                run_number = raw["fixture_run_number"]
                return {
                    "target": raw["target"],
                    "repo": raw["repo"],
                    "kind": "validate",
                    "state": raw["fixture_state"],
                    "unit": raw["unit"],
                    "host": raw["fixture_host"],
                    "admission_result": (
                        {
                            "state": admission,
                            **(
                                {"run_number": run_number}
                                if admission == "admitted"
                                else {}
                            ),
                        }
                        if admission is not None
                        else None
                    ),
                }

            with mock.patch.object(
                signal_crosscheck.run_registry, "projection", side_effect=projection
            ):
                report = signal_crosscheck.run_handle_report(
                    root,
                    "a" * 40,
                    "rrnewton/hermit",
                    [
                        # The ledger compatibility row may retain the FQDN even
                        # though its canonical envelope uses the short host.
                        {"host": "hosta.example.com", "run_number": 1},
                        # Normalize the projected handle host in the reciprocal
                        # direction too.
                        {"host": "hosta", "run_number": 2},
                        # Per-machine counters may legitimately reuse a number.
                        {"host": "hostb", "run_number": 3},
                        # A missing handle host must not be inferred from here.
                        {"host": "hostc", "run_number": 6},
                        # A matching log basename must not establish identity.
                        {
                            "host": "hosta",
                            "run_number": 99,
                            "log_file": "/elsewhere/admitted-cross-machine.log",
                        },
                    ],
                )

        self.assertEqual(len(report["matching_handles"]), 8)
        self.assertEqual(len(report["terminal_handles"]), 7)
        self.assertIn(
            "validate-running.service",
            {item["unit"] for item in report["matching_handles"]},
        )
        self.assertNotIn(
            "validate-running.service",
            {item["unit"] for item in report["terminal_handles"]},
        )
        self.assertEqual(
            [item["unit"] for item in report["admitted_without_ledger_handles"]],
            [
                "validate-admitted-cross-machine.service",
                "validate-admitted-missing-host.service",
            ],
        )
        by_unit = {item["unit"]: item for item in report["terminal_handles"]}
        self.assertTrue(by_unit["validate-admitted-matched.service"]["ledger_event"])
        self.assertEqual(
            by_unit["validate-admitted-matched.service"]["machine_shortname"],
            "hosta",
        )
        fqdn = by_unit["validate-admitted-fqdn-matched.service"]
        self.assertTrue(fqdn["ledger_event"])
        self.assertEqual(fqdn["host"], "hosta.example.com")
        self.assertEqual(fqdn["machine_shortname"], "hosta")
        collision = by_unit["validate-admitted-cross-machine.service"]
        self.assertFalse(collision["ledger_event"])
        self.assertEqual(collision["machine_shortname"], "hosta")
        missing_host = by_unit["validate-admitted-missing-host.service"]
        self.assertIsNone(missing_host["host"])
        self.assertEqual(missing_host["machine_shortname"], "")
        self.assertFalse(missing_host["ledger_event"])
        self.assertEqual(
            [item["unit"] for item in report["terminal_handles_missing_admission"]],
            ["validate-missing-admission.service"],
        )
        refused = next(
            item for item in report["terminal_handles"] if item["admission"] == "refused"
        )
        self.assertFalse(refused["ledger_event"])
        refused_without_admission = next(
            item
            for item in report["terminal_handles"]
            if item["unit"] == "validate-refused-without-admission.service"
        )
        self.assertTrue(refused_without_admission["non_run_refusal"])

    def test_valid_handle_for_another_target_is_out_of_scope(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            path = run_root / "other.json"
            path.write_text(json.dumps({
                "target": "b" * 40,
                "repo": "rrnewton/hermit",
            }))
            original_projection = signal_crosscheck.run_registry.projection
            try:
                signal_crosscheck.run_registry.projection = lambda _raw: (_ for _ in ()).throw(
                    AssertionError("out-of-scope handle must not be parsed")
                )
                report = signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                )
            finally:
                signal_crosscheck.run_registry.projection = original_projection
        self.assertEqual(report["terminal_handles"], [])
        self.assertEqual(report["matching_handles"], [])
        self.assertEqual(report["unreadable_handles"], [])

    def test_missing_or_invalid_raw_handle_attribution_is_loud(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            fixtures = {
                "missing-target": {"repo": "rrnewton/hermit"},
                "invalid-target": {
                    "target": "not-a-commit",
                    "repo": "rrnewton/hermit",
                },
                "missing-repo-exact-target": {"target": "a" * 40},
                "invalid-repo-exact-target": {
                    "target": "a" * 40,
                    "repo": "hermit",
                },
                "missing-repo-other-target": {"target": "b" * 40},
                "invalid-repo-other-target": {
                    "target": "b" * 40,
                    "repo": "hermit",
                },
            }
            for name, value in fixtures.items():
                (run_root / f"{name}.json").write_text(json.dumps(value))

            original_projection = signal_crosscheck.run_registry.projection
            try:
                signal_crosscheck.run_registry.projection = lambda _raw: (
                    _ for _ in ()
                ).throw(AssertionError("invalid attribution must fail before typed parsing"))
                report = signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                )
            finally:
                signal_crosscheck.run_registry.projection = original_projection

        self.assertEqual(report["terminal_handles"], [])
        self.assertEqual(len(report["unreadable_handles"]), 4)
        paths = {
            Path(item["path"]).stem for item in report["unreadable_handles"]
        }
        self.assertEqual(
            paths,
            {
                "missing-target",
                "invalid-target",
                "missing-repo-exact-target",
                "invalid-repo-exact-target",
            },
        )
        errors = "\n".join(
            str(item["error"]) for item in report["unreadable_handles"]
        )
        self.assertIn("target must be an exact lowercase forty-hex commit", errors)
        self.assertIn("repo must be rrnewton/hermit or rrnewton/reverie", errors)

    def test_exact_target_unknown_legacy_handle_without_ledger_stays_unmatched(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            (run_root / "unknown.json").write_text(json.dumps({
                "producer": signal_crosscheck.run_registry.PRODUCER,
                "target": "a" * 40,
                "repo": "rrnewton/hermit",
                "kind": "validate",
                "state": "unknown",
                "result": "unknown",
                "exit_code": None,
                "unit": "validate-owner-checkout.service",
                "log": "/tmp/validate-owner-checkout.log",
            }))
            with mock.patch.object(
                signal_crosscheck.run_registry,
                "projection",
                return_value={
                    "target": "a" * 40,
                    "repo": "rrnewton/hermit",
                    "kind": "validate",
                    "state": "unknown",
                    "unit": "validate-owner-checkout.service",
                    "admission_result": None,
                },
            ):
                report = signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                )

        self.assertEqual(report["unreadable_handles"], [])
        self.assertEqual(report["admitted_without_ledger_handles"], [])
        self.assertEqual(len(report["terminal_handles_missing_admission"]), 1)
        self.assertEqual(
            report["terminal_handles_missing_admission"][0]["unit"],
            "validate-owner-checkout.service",
        )
        self.assertIsNone(
            report["terminal_handles_missing_admission"][0]["host"]
        )
        self.assertEqual(
            report["terminal_handles_missing_admission"][0]["machine_shortname"],
            "",
        )
        complete = base_report()
        complete["run_handles"] = report
        issues = signal_crosscheck.reconcile(complete)
        self.assertEqual(len(issues), 1)
        self.assertIn("has no typed admission_result", issues[0])


if __name__ == "__main__":
    unittest.main()
