#!/usr/bin/env python3

import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
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
            "unknown_pre_admission_handles": [],
            "admitted_without_ledger_handles": [],
            "terminal_handles_missing_admission": [],
        },
        "github_status": {"published": False, "state": None},
    }


def current_validation_handle(name: str, **updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": signal_crosscheck.run_registry.SCHEMA_VERSION,
        "kind": "validate",
        "state": "preparing",
        "unit": f"{name}.service",
        "target": "a" * 40,
        "repo": "rrnewton/hermit",
        "checkout": "/tmp/checkout",
        "source_checkout": "/tmp/source",
        "temporary_checkout": True,
        "cargo_home": "/tmp/cargo",
        "log": "/tmp/validate.log",
        "agent": "signal-crosscheck-test",
        "pr": None,
        "started_at": "2026-08-30T07:00:00+00:00",
        "parent_checkout_head": "a" * 40,
        "producer": signal_crosscheck.run_registry.PRODUCER,
        "admission": "ci-hub validate-lock",
        "pane_role": "observer-only",
        "validate_lock_child_deadline_seconds": 3600,
        "e2e_result_root": "/tmp/e2e",
        "safe_ci_dag_runner_log_dir": "/tmp/dag",
        "hermit_run_timeout_seconds": 3300,
    }
    value.update(updates)
    return value


def current_service_result(**updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": signal_crosscheck.service_result.SCHEMA_VERSION,
        "commit": "a" * 40,
        "profile": "full",
        "selection_mode": None,
        "final_validate_status": "COULD_NOT_RUN",
        "exit_code": 75,
        "executed_nodes": 0,
        "executed_tests": None,
        "scorecard_writeback": None,
    }
    value.update(updates)
    return value


class SignalCrosscheckTests(unittest.TestCase):
    def test_scorecard_population_wording_refuses_ambiguous_or_malformed_forms(
        self,
    ) -> None:
        historical_table = "\n".join([
            "| Backend | Green | Red | Not applicable | Total |",
            "| --- | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **2** |",
        ])
        path_explicit_table = "\n".join([
            "| Backend | Selected by full | Not selected by full | Not applicable | "
            "In the manifest |",
            "| --- | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **2** |",
        ])
        self.assertEqual(
            signal_crosscheck._scorecard_status_total(
                historical_table + "\n\n" + path_explicit_table
            ),
            (None, None),
        )
        malformed_historical_table = historical_table.replace("**2**", "2")
        self.assertEqual(
            signal_crosscheck._scorecard_status_total(
                path_explicit_table + "\n\n" + malformed_historical_table
            ),
            (None, None),
        )
        self.assertEqual(
            signal_crosscheck._scorecard_status_total("No population table."),
            (None, None),
        )
        self.assertEqual(
            signal_crosscheck._scorecard_measurement_total(path_explicit_table),
            (None, None),
        )
        self.assertEqual(
            signal_crosscheck._scorecard_status_total(
                path_explicit_table.replace("**2**", "2")
            ),
            (signal_crosscheck.FULL_STATUS_COLUMNS, None),
        )
        self.assertEqual(
            signal_crosscheck._scorecard_status_total(
                path_explicit_table.replace(
                    "| --- | ---: | ---: | ---: | ---: |\n", ""
                )
            ),
            (signal_crosscheck.FULL_STATUS_COLUMNS, None),
        )
        self.assertEqual(
            signal_crosscheck._scorecard_status_total(
                path_explicit_table.replace(
                    "| **Total** |",
                    "| malformed | row |\n| **Total** |",
                )
            ),
            (signal_crosscheck.FULL_STATUS_COLUMNS, None),
        )
        self.assertEqual(
            signal_crosscheck._scorecard_status_total(
                path_explicit_table
                + "\n| **Total** | **1** | **1** | **0** | 2 |"
            ),
            (signal_crosscheck.FULL_STATUS_COLUMNS, None),
        )

        duplicated_prose = "\n".join([
            "The count table includes all **2** cells in the manifest; no row is omitted.",
            "The count table includes all **3** cells in the manifest; no row is omitted.",
            "Of the cells selected by full, **0** have `never-measured`; of the "
            "cells not selected by full, **0** have `measured-and-passed`.",
        ])
        self.assertEqual(
            signal_crosscheck._scorecard_prose_counts(
                duplicated_prose, signal_crosscheck.FULL_STATUS_COLUMNS
            ),
            {},
        )

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
                "run_id": "validate-canonical",
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
        self.assertEqual(report["ledger"]["exact_run_ids"], ["validate-canonical"])
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
            "| Backend | Green | Red | Not applicable | Total |",
            "| --- | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **2** |",
            "",
            "| Status | `never-measured` | `measured-and-passed` | "
            "`measured-no-verdict` | `diverged-unlocated` | `diverged` | Total |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **0** | **0** | **2** |",
        ])
        path_explicit = "\n".join([
            "The count table includes all **2** cells in the manifest; no row is omitted.",
            "Of the cells selected by full, **0** have `never-measured`; of the "
            "cells not selected by full, **0** have `measured-and-passed`.",
            "| Backend | Selected by full | Not selected by full | Not applicable | "
            "In the manifest |",
            "| --- | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **2** |",
            "",
            "| Status | `never-measured` | `measured-and-passed` | "
            "`measured-no-verdict` | `diverged-unlocated` | `diverged` | "
            "In the manifest |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **0** | **0** | **2** |",
        ])

        def report_for(markdown_value: str) -> dict:
            def fake_git(_repo: Path, *args: str) -> str:
                target = args[-1]
                if target.endswith("cells.json"):
                    return json.dumps(cells)
                if target.endswith("SCORECARD.md"):
                    return markdown_value
                if target.endswith(":detcore"):
                    return "tree\n"
                raise AssertionError(args)

            with (
                mock.patch.object(signal_crosscheck, "git_text", side_effect=fake_git),
                mock.patch.object(
                    signal_crosscheck.canonical_series,
                    "published_snapshot_at",
                    side_effect=lambda _root, revision: (
                        signal_crosscheck.canonical_series.PublishedSnapshot(
                            revision, series_rows
                        )
                    ),
                ),
                mock.patch.object(
                    signal_crosscheck,
                    "commit_is_ancestor",
                    return_value=True,
                ),
                mock.patch.object(
                    signal_crosscheck,
                    "git_tree_at",
                    return_value=source_tree,
                ),
            ):
                return signal_crosscheck.scorecard_report(
                    Path("parent"),
                    Path("hermit"),
                    "a" * 40,
                    published_series_snapshot=(
                        signal_crosscheck.canonical_series.PublishedSnapshot(
                            current_commit, list(series_rows)
                        )
                    ),
                )

        result = report_for(markdown)
        cells["projection"].update({
            "source": "alternate-series",
            "source_tree": "e" * 40,
        })
        alternate = report_for(markdown)
        cells["projection"].update({
            "source": "series",
            "source_tree": source_tree,
        })
        cells["schema"] = 8
        path_explicit_result = report_for(path_explicit)
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
        self.assertEqual(path_explicit_result["schema"], 8)
        self.assertTrue(path_explicit_result["status_table_matches_cells"])
        self.assertTrue(path_explicit_result["measurement_table_matches_cells"])
        self.assertTrue(path_explicit_result["prose_matches_cells"])
        invalid_cell_fields = [
            ("status", "enabled", "unsupported status 'enabled'"),
            ("status", "unknown", "unsupported status 'unknown'"),
            ("status", None, "unsupported status None"),
            ("measurement", "unknown", "unsupported measurement 'unknown'"),
            ("measurement", None, "unsupported measurement None"),
        ]
        for field, invalid, message in invalid_cell_fields:
            with self.subTest(field=field, invalid=invalid):
                original_value = cells["cells"][0].pop(field)
                if invalid is not None:
                    cells["cells"][0][field] = invalid
                try:
                    with self.assertRaisesRegex(
                        signal_crosscheck.CrosscheckError, message
                    ):
                        report_for(path_explicit)
                finally:
                    cells["cells"][0][field] = original_value
        cells["cells"][0]["enabled"] = True
        self.assertFalse(report_for(path_explicit)["status_table_matches_cells"])
        del cells["cells"][0]["enabled"]
        cells["cells"][0]["ci_disabled_reason"] = "legacy field"
        self.assertFalse(report_for(path_explicit)["status_table_matches_cells"])
        del cells["cells"][0]["ci_disabled_reason"]
        self.assertFalse(
            report_for(path_explicit + "\nEnabled cells use the full path.")[
                "prose_matches_cells"
            ]
        )
        full_measurement_header = (
            "| Status | `never-measured` | `measured-and-passed` | "
            "`measured-no-verdict` | `diverged-unlocated` | `diverged` | "
            "In the manifest |"
        )
        historical_measurement_header = full_measurement_header.replace(
            "In the manifest", "Total"
        )
        self.assertFalse(
            report_for(
                path_explicit.replace(
                    full_measurement_header, historical_measurement_header
                )
            )["measurement_table_matches_cells"]
        )
        mixed_measurement_tables = path_explicit + "\n\n" + "\n".join([
            historical_measurement_header,
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **0** | **0** | **2** |",
        ])
        self.assertFalse(
            report_for(mixed_measurement_tables)["measurement_table_matches_cells"]
        )
        self.assertFalse(
            report_for(
                path_explicit.replace(
                    "all **2** cells in the manifest",
                    "all **3** cells in the manifest",
                )
            )["prose_matches_cells"]
        )
        self.assertFalse(
            report_for(
                path_explicit.replace(
                    "| **Total** | **1** | **1** | **0** | **2** |",
                    "| **Total** | **1** | **1** | **0** | **3** |",
                    1,
                )
            )["status_table_matches_cells"]
        )
        missing_prose = path_explicit.replace(
            "Of the cells selected by full, **0** have `never-measured`; of the "
            "cells not selected by full, **0** have `measured-and-passed`.",
            "",
        )
        self.assertFalse(report_for(missing_prose)["prose_matches_cells"])
        ambiguous = path_explicit + "\n\n" + "\n".join([
            "| Backend | Green | Red | Not applicable | Total |",
            "| --- | ---: | ---: | ---: | ---: |",
            "| **Total** | **1** | **1** | **0** | **2** |",
        ])
        ambiguous_result = report_for(ambiguous)
        self.assertFalse(ambiguous_result["status_table_matches_cells"])
        self.assertFalse(ambiguous_result["prose_matches_cells"])
        stale_historical_prose = (
            path_explicit
            + "\nThe current **0** manifest-disabled combinations as "
            "**Not applicable**."
        )
        self.assertFalse(report_for(stale_historical_prose)["prose_matches_cells"])
        malformed_historical_prose = (
            path_explicit + "\nThe current count of manifest-disabled combinations."
        )
        self.assertFalse(
            report_for(malformed_historical_prose)["prose_matches_cells"]
        )
        malformed_new_population_prose = (
            path_explicit + "\nThe count table includes all cells in the manifest."
        )
        self.assertFalse(
            report_for(malformed_new_population_prose)["prose_matches_cells"]
        )
        malformed_new_selection_prose = (
            path_explicit
            + "\nOf the cells selected by full, some have no classification."
        )
        self.assertFalse(
            report_for(malformed_new_selection_prose)["prose_matches_cells"]
        )
        historical_on_schema_8 = report_for(markdown)
        self.assertFalse(historical_on_schema_8["status_table_matches_cells"])
        self.assertFalse(historical_on_schema_8["prose_matches_cells"])

        cells["schema"] = 6
        path_explicit_on_schema_6 = report_for(path_explicit)
        self.assertFalse(path_explicit_on_schema_6["status_table_matches_cells"])
        self.assertFalse(path_explicit_on_schema_6["prose_matches_cells"])

        mutated = markdown.replace(
            "green/`never-measured` count is **0**",
            "green/`never-measured` count is **1**",
        )
        self.assertFalse(report_for(mutated)["prose_matches_cells"])

    def test_run_handle_matching_uses_typed_projection_and_run_event_identity(self) -> None:
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
                            **(
                                {"run_number": raw["run_number"]}
                                if raw.get("run_number") is not None
                                else {}
                            ),
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
                        "canonical_verdict": "NOT-VALIDATED",
                        "wrapper_exit_code": 4,
                        "fixture_state": state,
                        # The completed handle deliberately lacks the optional
                        # display counter. Event identity still binds it.
                        "run_number": (
                            None if state == "completed" else run_numbers.get(state, 99)
                        ),
                    }))
                matched = signal_crosscheck.run_handle_report(
                    root,
                    "a" * 40,
                    "rrnewton/hermit",
                    [
                        {
                            "repo": "hermit",
                            "commit": "a" * 40,
                            "run_id": f"validate-{state}",
                            "record_id": f"event-{state}",
                            "host": "hosta",
                            "run_number": value,
                        }
                        for state, value in run_numbers.items()
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

    def test_run_handle_wrapper_outcome_mismatch_remains_fail_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "wrapper outcome disagrees"):
            signal_crosscheck._wrapper_outcome(
                {"canonical_verdict": "VALIDATED", "wrapper_exit_code": 4}
            )

    def test_pending_owner_watch_correction_is_unavailable_and_disagrees(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ignored/validate/runs").mkdir(parents=True)
            signal_crosscheck.run_registry.owner_watch_correction_transaction(
                root
            ).mkdir()
            unavailable: dict[str, str] = {}
            handles = signal_crosscheck._collect(
                "run-handles",
                unavailable,
                lambda: signal_crosscheck.run_handle_report(
                    root, "a" * 40, "rrnewton/hermit", []
                ),
            )

        report = base_report()
        report.pop("run_handles")
        report["unavailable"] = unavailable
        report["issues"] = signal_crosscheck.reconcile(report)
        report["result"] = "AGREE" if not report["issues"] else "DISAGREE"
        self.assertIsNone(handles)
        self.assertIn("run-handles", unavailable)
        self.assertIn("recovery is pending", unavailable["run-handles"])
        self.assertEqual("DISAGREE", report["result"])
        self.assertIn("run-handles unavailable", report["issues"][0])

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
                    None,
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
                    "canonical_verdict": (
                        "NOT-VALIDATED" if state in {"completed", "failed"} else None
                    ),
                    "wrapper_exit_code": (
                        4 if state in {"completed", "failed"} else None
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
                        # The optional machine-local counter does not define
                        # identity; this missing-number handle still binds by
                        # run id and exact commit.
                        {
                            "repo": "hermit",
                            "commit": "a" * 40,
                            "run_id": "validate-admitted-matched",
                            "record_id": "event-admitted-matched",
                            "host": "hostb",
                            "run_number": 99,
                        },
                        {
                            "repo": "hermit",
                            "commit": "a" * 40,
                            "run_id": "validate-admitted-fqdn-matched",
                            "record_id": "event-admitted-fqdn-matched",
                            "host": "hosta",
                            "run_number": 2,
                        },
                        # A missing display host does not erase the durable
                        # handle/event identity.
                        {
                            "repo": "hermit",
                            "commit": "a" * 40,
                            "run_id": "validate-admitted-missing-host",
                            "record_id": "event-admitted-missing-host",
                            "host": "hostc",
                            "run_number": 6,
                        },
                        # A colliding machine-local number and matching log
                        # basename do not establish identity.
                        {
                            "repo": "hermit",
                            "commit": "a" * 40,
                            "run_id": "different-run",
                            "record_id": "event-different-run",
                            "host": "hosta",
                            "run_number": 3,
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
            ["validate-admitted-cross-machine.service"],
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
        self.assertTrue(missing_host["ledger_event"])
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

    def test_run_handle_report_discriminates_mixed_record_families(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            (run_root / "validate-valid.json").write_text(
                json.dumps(current_validation_handle("validate-valid"))
            )
            for name, schema in (
                ("validate-historical", 1),
                ("validate-historical-hybrid", 1),
                ("validate-current", 3),
                ("validate-broken-result", 3),
                ("validate-wrong-commit", 3),
                ("validate-injected-other-target", 3),
                ("validate-injected-other-repo", 3),
                ("validate-path-mismatch", 3),
                ("validate-a", 3),
            ):
                (run_root / f"{name}.json").write_text(
                    json.dumps(
                        current_validation_handle(
                            name,
                            service_result_schema=schema,
                        )
                    )
                )
            (run_root / "validate-historical.service-result.json").write_text(json.dumps({
                "schema_version": 1,
                "commit": "a" * 40,
                "profile": "full",
                "final_validate_status": "FAILED",
                "exit_code": 1,
                "executed_nodes": 1,
                "executed_tests": 1,
            }))
            (run_root / "validate-current.service-result.json").write_text(json.dumps({
                "schema_version": 3,
                "commit": "a" * 40,
                "profile": "full",
                "selection_mode": None,
                "final_validate_status": "FAILED",
                "exit_code": 1,
                "executed_nodes": 1,
                "executed_tests": 1,
                "scorecard_writeback": None,
            }))
            (run_root / "validate-broken-result.service-result.json").write_text(
                json.dumps({
                    "schema_version": 3,
                    "commit": "a" * 40,
                    "profile": "full",
                    "selection_mode": None,
                    "final_validate_status": "FAILED",
                    "exit_code": 1,
                    "executed_nodes": 1,
                    "scorecard_writeback": None,
                })
            )
            (run_root / "validate-wrong-commit.service-result.json").write_text(
                json.dumps({
                    "schema_version": 3,
                    "commit": "b" * 40,
                    "profile": "full",
                    "selection_mode": None,
                    "final_validate_status": "FAILED",
                    "exit_code": 1,
                    "executed_nodes": 1,
                    "executed_tests": 1,
                    "scorecard_writeback": None,
                })
            )
            injected_other_target = current_service_result()
            injected_other_target.update(
                target="b" * 40,
                repo="rrnewton/hermit",
                unit="validate-injected-other-target.service",
            )
            (
                run_root / "validate-injected-other-target.service-result.json"
            ).write_text(json.dumps(injected_other_target))
            injected_other_repo = current_service_result()
            injected_other_repo.update(
                target="a" * 40,
                repo="rrnewton/reverie",
                unit="validate-injected-other-repo.service",
            )
            (
                run_root / "validate-injected-other-repo.service-result.json"
            ).write_text(json.dumps(injected_other_repo))
            historical_hybrid = {
                "schema_version": 1,
                "commit": "b" * 40,
                "profile": "full",
                "final_validate_status": "FAILED",
                "exit_code": 1,
                "executed_nodes": 1,
                "executed_tests": 1,
                "target": "b" * 40,
                "repo": "rrnewton/hermit",
                "unit": "validate-historical-hybrid.service",
            }
            (
                run_root / "validate-historical-hybrid.service-result.json"
            ).write_text(json.dumps(historical_hybrid))
            (
                run_root / "validate-path-mismatch.service-result.json"
            ).write_text(
                json.dumps(
                    current_validation_handle(
                        "validate-copied-other",
                        target="b" * 40,
                    )
                )
            )
            # Both names are valid handles. The latter must not be hidden even
            # though it collides with the former's service-result path.
            (run_root / "validate-a.service-result.json").write_text(
                json.dumps(current_validation_handle("validate-a.service-result"))
            )
            malformed_handle = current_validation_handle(
                "validate-malformed-handle.service-result"
            )
            del malformed_handle["checkout"]
            (run_root / "validate-malformed-handle.service-result.json").write_text(
                json.dumps(malformed_handle)
            )
            (run_root / "unattributable.json").write_text(json.dumps({
                "schema_version": 3,
                "commit": "a" * 40,
            }))
            (run_root / "malformed.json").write_text("{")

            report = signal_crosscheck.run_handle_report(
                root, "a" * 40, "rrnewton/hermit", []
            )

        self.assertEqual(
            {item["unit"] for item in report["matching_handles"]},
            {
                "validate-a.service",
                "validate-a.service-result.service",
                "validate-broken-result.service",
                "validate-current.service",
                "validate-historical.service",
                "validate-historical-hybrid.service",
                "validate-injected-other-repo.service",
                "validate-injected-other-target.service",
                "validate-path-mismatch.service",
                "validate-valid.service",
                "validate-wrong-commit.service",
            },
        )
        unreadable = {
            Path(item["path"]).name: item
            for item in report["unreadable_handles"]
        }
        self.assertEqual(
            set(unreadable),
            {
                "validate-broken-result.service-result.json",
                "validate-historical-hybrid.service-result.json",
                "validate-injected-other-repo.service-result.json",
                "validate-injected-other-target.service-result.json",
                "validate-malformed-handle.service-result.json",
                "validate-path-mismatch.service-result.json",
                "validate-wrong-commit.service-result.json",
                "unattributable.json",
                "malformed.json",
            },
        )
        self.assertEqual(
            unreadable["validate-broken-result.service-result.json"]["source"],
            "validation-service-result",
        )
        self.assertEqual(
            unreadable["validate-wrong-commit.service-result.json"]["source"],
            "validation-service-result",
        )
        self.assertEqual(
            unreadable["validate-injected-other-target.service-result.json"]["source"],
            "validation-service-result",
        )
        self.assertEqual(
            unreadable["validate-injected-other-repo.service-result.json"]["source"],
            "validation-service-result",
        )
        self.assertEqual(
            unreadable["validate-historical-hybrid.service-result.json"]["source"],
            "validation-service-result",
        )
        self.assertEqual(
            unreadable["validate-path-mismatch.service-result.json"]["source"],
            "validation-service-result",
        )
        self.assertNotIn(
            "source",
            unreadable["validate-malformed-handle.service-result.json"],
        )

    def test_only_exact_pre_plan_no_result_omits_the_ledger_requirement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)

            base_handle = {
                "state": "completed",
                "admission_result": {
                    "state": "admitted",
                    "recorded_at": "2026-08-30T07:00:01+00:00",
                    "run_number": 1,
                },
                "result": "no-result",
                "exit_code": 75,
                "finished_at": "2026-08-30T07:00:02+00:00",
                "service_result_schema": 3,
                "selection_mode": None,
                "scorecard_writeback": None,
                "executed_nodes": 0,
                "executed_tests": None,
                "final_validate_status": "COULD_NOT_RUN",
                "result_source": "validation-service-result",
            }
            cases = {
                "validate-pre-plan": ({}, {}, True),
                "validate-passed": (
                    {
                        "result": "success",
                        "exit_code": 0,
                        "executed_nodes": 1,
                        "executed_tests": 1,
                        "final_validate_status": "PASSED",
                    },
                    {
                        "exit_code": 0,
                        "executed_nodes": 1,
                        "executed_tests": 1,
                        "final_validate_status": "PASSED",
                    },
                    True,
                ),
                "validate-failed": (
                    {
                        "result": "failure",
                        "exit_code": 1,
                        "executed_nodes": 1,
                        "executed_tests": 1,
                        "final_validate_status": "FAILED",
                    },
                    {
                        "exit_code": 1,
                        "executed_nodes": 1,
                        "executed_tests": 1,
                        "final_validate_status": "FAILED",
                    },
                    True,
                ),
                "validate-executed": (
                    {"executed_nodes": 1},
                    {"executed_nodes": 1},
                    True,
                ),
                "validate-selected": (
                    {"selection_mode": "explicit-test"},
                    {"selection_mode": "explicit-test"},
                    True,
                ),
                "validate-non-full": (
                    {},
                    {"profile": "quick"},
                    True,
                ),
                "validate-wrapper-outcome": (
                    {
                        "canonical_verdict": "NO-RESULT",
                        "wrapper_exit_code": 4,
                    },
                    {},
                    True,
                ),
                "validate-writeback": (
                    {"scorecard_writeback": {"status": "completed"}},
                    {"scorecard_writeback": {"status": "completed"}},
                    True,
                ),
                "validate-missing-result": ({}, {}, False),
                "validate-mismatched-result": (
                    {},
                    {"commit": "b" * 40},
                    True,
                ),
            }
            for run_number, (name, (handle_updates, result_updates, write_result)) in enumerate(
                cases.items(), start=1
            ):
                handle_updates = {
                    **base_handle,
                    **handle_updates,
                    "admission_result": {
                        **base_handle["admission_result"],
                        "run_number": run_number,
                    },
                }
                handle_path = run_root / f"{name}.json"
                handle_path.write_text(
                    json.dumps(current_validation_handle(name, **handle_updates))
                )
                if write_result:
                    signal_crosscheck.service_result.result_path(handle_path).write_text(
                        json.dumps(current_service_result(**result_updates))
                    )

            report = signal_crosscheck.run_handle_report(
                root, "a" * 40, "rrnewton/hermit", []
            )

        by_unit = {item["unit"]: item for item in report["terminal_handles"]}
        self.assertTrue(by_unit["validate-pre-plan.service"]["no_receipt_attempt"])
        self.assertEqual(
            {
                item["unit"]
                for item in report["admitted_without_ledger_handles"]
            },
            {
                "validate-executed.service",
                "validate-failed.service",
                "validate-mismatched-result.service",
                "validate-missing-result.service",
                "validate-non-full.service",
                "validate-passed.service",
                "validate-selected.service",
                "validate-wrapper-outcome.service",
                "validate-writeback.service",
            },
        )
        complete = base_report()
        complete.update(sha="a" * 40, repo="rrnewton/hermit")
        complete["run_handles"] = report
        issues = signal_crosscheck.reconcile(complete)
        self.assertFalse(any("validate-pre-plan.service" in issue for issue in issues))
        self.assertTrue(
            all(
                any(unit in issue for issue in issues)
                for unit in {
                    "validate-executed.service",
                    "validate-failed.service",
                    "validate-mismatched-result.service",
                    "validate-missing-result.service",
                    "validate-non-full.service",
                    "validate-passed.service",
                    "validate-selected.service",
                    "validate-wrapper-outcome.service",
                    "validate-writeback.service",
                }
            )
        )
        output = io.StringIO()
        with redirect_stdout(output):
            signal_crosscheck.print_text(complete)
        self.assertIn("no-receipt-attempts=1", output.getvalue())

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

    def test_unknown_pre_admission_handle_is_visible_without_disagreement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            (run_root / "unknown.json").write_text(
                json.dumps(
                    current_validation_handle(
                        "validate-owner-checkout",
                        state="unknown",
                        result="unknown",
                        exit_code=None,
                        detail="service result was never written",
                        process_identity=None,
                        admission_result=None,
                        executed_nodes=None,
                        executed_tests=None,
                        final_validate_status=None,
                        observed_safe_ci_cgroups=[],
                    )
                )
            )
            report = signal_crosscheck.run_handle_report(
                root, "a" * 40, "rrnewton/hermit", []
            )

        self.assertEqual(report["unreadable_handles"], [])
        self.assertEqual(report["admitted_without_ledger_handles"], [])
        self.assertEqual(report["terminal_handles_missing_admission"], [])
        self.assertEqual(len(report["unknown_pre_admission_handles"]), 1)
        self.assertEqual(
            report["unknown_pre_admission_handles"][0]["unit"],
            "validate-owner-checkout.service",
        )
        self.assertIsNone(
            report["unknown_pre_admission_handles"][0]["host"]
        )
        self.assertEqual(
            report["unknown_pre_admission_handles"][0]["machine_shortname"],
            "",
        )
        complete = base_report()
        complete.update(sha="a" * 40, repo="rrnewton/hermit")
        complete["run_handles"] = report
        issues = signal_crosscheck.reconcile(complete)
        self.assertEqual(issues, [])
        complete["issues"] = issues
        complete["result"] = "AGREE"
        output = io.StringIO()
        with redirect_stdout(output):
            signal_crosscheck.print_text(complete)
        rendered = output.getvalue()
        self.assertIn("unknown-pre-admission=1", rendered)
        self.assertIn(
            "run-handle-history: unknown pre-admission "
            "validate-owner-checkout.service state=unknown",
            rendered,
        )
        self.assertIn("RECONCILIATION: AGREE", rendered)

    def test_started_or_admitted_malformed_handles_remain_disagreements(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_root = root / "ignored" / "validate" / "runs"
            run_root.mkdir(parents=True)
            identity = {
                "pid": 123,
                "start_ticks": 456,
                "boot_id": "fixture-boot",
            }
            evidence_cases = {
                "started-without-admission": {"process_identity": identity},
                "host-without-admission": {"host": "fixture-host"},
                "outcome-exit": {"exit_code": 3},
                "outcome-signal": {"signal": 15},
                "outcome-executed-nodes": {"executed_nodes": 0},
                "outcome-executed-tests": {"executed_tests": 0},
                "outcome-final-status": {
                    "final_validate_status": "COULD_NOT_RUN"
                },
                "outcome-wrapper": {
                    "canonical_verdict": "NEEDS-RERUN",
                    "wrapper_exit_code": 4,
                },
                "outcome-observed-cgroup": {
                    "observed_safe_ci_cgroups": ["/system.slice/example.scope"]
                },
            }
            for name, updates in evidence_cases.items():
                (run_root / f"{name}.json").write_text(
                    json.dumps(
                        current_validation_handle(
                            f"validate-{name}",
                            **{
                                "state": "unknown",
                                "result": "unknown",
                                "exit_code": None,
                                "detail": "service outcome is unknown",
                                "process_identity": None,
                                "admission_result": None,
                                **updates,
                            },
                        )
                    )
                )
            (run_root / "admitted-without-ledger.json").write_text(
                json.dumps(
                    current_validation_handle(
                        "validate-admitted-without-ledger",
                        state="completed",
                        result="failure",
                        exit_code=1,
                        finished_at="2026-08-30T07:01:00+00:00",
                        final_validate_status="FAILED",
                        process_identity=None,
                        admission_result={
                            "state": "admitted",
                            "recorded_at": "2026-08-30T07:00:01+00:00",
                            "run_number": 7,
                        },
                        canonical_verdict="FAILED",
                        wrapper_exit_code=3,
                    )
                )
            )
            report = signal_crosscheck.run_handle_report(
                root, "a" * 40, "rrnewton/hermit", []
            )

        self.assertEqual(report["unknown_pre_admission_handles"], [])
        expected_missing = {
            f"validate-{name}.service" for name in evidence_cases
        }
        self.assertEqual(
            {
                handle["unit"]
                for handle in report["terminal_handles_missing_admission"]
            },
            expected_missing,
        )
        self.assertEqual(
            [handle["unit"] for handle in report["admitted_without_ledger_handles"]],
            ["validate-admitted-without-ledger.service"],
        )
        complete = base_report()
        complete["run_handles"] = report
        issues = signal_crosscheck.reconcile(complete)
        self.assertEqual(len(issues), len(expected_missing) + 1)
        complained_units = {
            unit
            for unit in expected_missing
            if any(unit in issue for issue in issues)
        }
        self.assertEqual(complained_units, expected_missing)
        self.assertTrue(
            any("validate-admitted-without-ledger.service" in issue for issue in issues)
        )


if __name__ == "__main__":
    unittest.main()
