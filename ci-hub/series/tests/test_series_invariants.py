#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Invariants of the stress/flakiness series store.

⚠️ EVERY GUARD HERE IS DEMONSTRATED REFUSING, not merely passing. A validator
whose negative case is untested is indistinguishable from one that returns
`True`, and that pattern -- a gate nobody has watched refuse -- is the failure
this repository has hit repeatedly.
"""

import errno
import fcntl
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import series  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ledger"))
import publish_backlog  # noqa: E402
import publisher  # noqa: E402


def classification_for_series_outcome(outcome):
    return {
        "passed": ("pass", None),
        "diverged": ("determinism-failure", "product_failure"),
        "timeout": ("timeout", "no_result"),
        "errored": ("crash-error", "product_failure"),
        "no_result": (None, "no_result"),
        "skipped": (None, "understood_prerequisite_failure"),
    }.get(outcome, ("pass", None))


def row(**over):
    base = {
        "schema": series.ENVELOPE_VERSION,
        "event_id": "01J000000000000000000000AA",
        "event_type": "series.observation",
        "emitted_at": "2026-08-25T03:00:00Z",
        "team": "hermit",
        "host": "devbig014",
        "run_id": "campaign-1",
        "producer": "pressure-test",
        "series": {
            "cell": "backend-parity-c/flock-lifecycle/verify/ptrace",
            "tree": "a" * 40,
            "run_index": 0,
            "outcome": "passed",
            "machine_shortname": "devbig014",
            "kernel_version": "7.1.3-test",
            "host_capabilities": {
                "cpuid-faulting": {"present": False, "evidence": "fixture absence"},
                "kvm": {"present": True, "evidence": "fixture presence"},
            },
        },
    }
    for key, value in over.items():
        if key == "series":
            base["series"] = {**base["series"], **value}
        else:
            base[key] = value
    result, failure_class = classification_for_series_outcome(
        base["series"].get("outcome")
    )
    base["series"].setdefault("result", result)
    base["series"].setdefault("failure_class", failure_class)
    return base


class PressureSampleClassification(unittest.TestCase):
    def setUp(self):
        self.projection = CellResultProjection()
        self.projection.setUp()
        self.addCleanup(self.projection.tearDown)
        self.CELL = series._cell_name(self.projection._cell(1))

    def _cells(self, kinds):
        cells = []
        for repetition, kind in enumerate(kinds, 1):
            cell = self.projection._cell(repetition)
            if kind == "product":
                cell.update(outcome="FAIL", result="determinism-failure",
                            failure_class="product_failure")
                cell["attempts"] = [self.projection._attempt("diverged")]
            elif kind != "pass":
                cell.update(outcome="ERROR", result=None, failure_class={
                    "infrastructure": "understood_infrastructure_failure",
                    "prerequisite": "understood_prerequisite_failure",
                    "no_result": "no_result",
                }[kind])
                cell["attempts"] = [self.projection._attempt(
                    "infrastructure_error" if kind == "infrastructure" else "no_result"
                )]
                if kind == "prerequisite":
                    cell["attempts"][0]["error_kind"] = "backend-unavailable"
            cells.append(cell)
        return cells

    def _rows(self, kinds):
        rows = self.projection._project(self._cells(kinds))
        for item in rows:
            item["run_id"] = "campaign-10x"
        return rows

    def _classify_cells(self, cells):
        rows = self.projection._project(cells)
        cell = series._cell_name(cells[0])
        return series.classify_pressure_sample(rows, [cell], "pressure-one")[cell]

    def _classification(self, kinds):
        return series.classify_pressure_sample(
            self._rows(kinds), [self.CELL], "campaign-10x"
        )[self.CELL]

    def test_ten_qualifying_passes_are_the_only_promotion_candidate(self):
        candidate = self._classification(["pass"] * 10)
        self.assertEqual(candidate["classification"], "promotion-candidate")
        self.assertTrue(candidate["promotion_candidate"])
        compressed = self._rows(["pass"])[0]
        # A historical compressed row does not retain individual inner facts.
        compressed["series"].pop("pressure_evidence")
        compressed["series"].pop("attempt")
        compressed["series"].update({"num_runs": 10, "last_run_index": 10})
        compressed_candidate = series.classify_pressure_sample(
            [compressed], [self.CELL], "campaign-10x"
        )[self.CELL]
        self.assertEqual(
            compressed_candidate["classification"], "incomplete"
        )
        self.assertEqual(compressed_candidate["unknown_history_repetitions"], 10)
        self.assertEqual(compressed_candidate["terminal_passes"], 10)
        for passes in (1, 9):
            with self.subTest(passes=passes):
                result = self._classification(
                    ["pass"] * passes + ["product"] * (10 - passes)
                )
                self.assertEqual(result["classification"], "intermittent")
                self.assertFalse(result["promotion_candidate"])

    def test_only_ten_terminal_product_failures_are_confirmed_failing(self):
        failing = self._classification(["product"] * 10)
        self.assertEqual(failing["classification"], "confirmed-failing")
        for kind, classification, count_field in (
            ("infrastructure", "infrastructure-failure", "infrastructure_failures"),
            ("prerequisite", "prerequisite-failure", "prerequisite_failures"),
            ("no_result", "no-result", "no_results"),
        ):
            with self.subTest(kind=kind):
                result = self._classification([kind] * 10)
                self.assertEqual(result["classification"], classification)
                self.assertEqual(result[count_field], 10)
                self.assertFalse(result["promotion_candidate"])

        mixed = self._classification(["product"] * 9 + ["infrastructure"])
        self.assertEqual(mixed["classification"], "incomplete")
        self.assertEqual(mixed["terminal_product_failures"], 9)
        self.assertEqual(mixed["infrastructure_failures"], 1)

    def test_missing_duplicate_and_retry_evidence_cannot_promote(self):
        missing = self._classification(["pass"] * 9)
        self.assertEqual(missing["classification"], "incomplete")
        self.assertEqual(missing["missing_repetitions"], 1)

        duplicate_rows = self._rows(["pass"] * 10)
        publication_replay = duplicate_rows + [
            json.loads(json.dumps(duplicate_rows[0]))
        ]
        replay_result = series.classify_pressure_sample(
            publication_replay, [self.CELL], "campaign-10x"
        )[self.CELL]
        self.assertEqual(replay_result["classification"], "promotion-candidate")
        duplicate = dict(duplicate_rows[0])
        duplicate["event_id"] = "different-event-same-attempt"
        duplicate_rows.append(duplicate)
        duplicate_result = series.classify_pressure_sample(
            duplicate_rows, [self.CELL], "campaign-10x"
        )[self.CELL]
        self.assertEqual(duplicate_result["classification"], "incomplete")
        self.assertEqual(duplicate_result["duplicate_attempts"], 1)

        retried_rows = self._rows(["pass"] * 10)
        retried_rows[0]["series"]["attempt"] = 2
        first_attempt = self._rows(["product"])[0]
        first_attempt["event_id"] = "pressure-first-product"
        retried_rows.append(first_attempt)
        retried = series.classify_pressure_sample(
            retried_rows, [self.CELL], "campaign-10x"
        )[self.CELL]
        self.assertEqual(retried["classification"], "intermittent")
        self.assertEqual(retried["terminal_passes"], 10)
        self.assertEqual(retried["qualifying_passes"], 9)
        self.assertEqual(retried["retried_repetitions"], 1)

        regressed_rows = self._rows(["product"] * 10)
        regressed_rows[0]["series"]["attempt"] = 2
        first_pass = self._rows(["pass"])[0]
        first_pass["event_id"] = "pressure-first-pass"
        regressed_rows.append(first_pass)
        regressed = series.classify_pressure_sample(
            regressed_rows, [self.CELL], "campaign-10x"
        )[self.CELL]
        self.assertEqual(regressed["classification"], "incomplete")
        self.assertEqual(regressed["terminal_passes"], 0)
        self.assertEqual(regressed["mixed_repetitions"], 1)

    def test_inner_failures_behind_terminal_pass_never_qualify(self):
        for kind in ("product", "infrastructure", "prerequisite", "no_result"):
            with self.subTest(kind=kind):
                cells = self._cells(["pass"] * 10)
                adverse = self._cells([kind])[0]["attempts"][0]
                cells[0]["attempts"][0]["index"] = "2"
                cells[0]["attempts"].insert(0, adverse)
                original = json.dumps(cells, sort_keys=True)
                result = self._classify_cells(cells)
                self.assertEqual(result["classification"], "intermittent")
                self.assertEqual(result["terminal_passes"], 10)
                self.assertEqual(result["qualifying_passes"], 9)
                self.assertEqual(result["retried_repetitions"], 0)
                self.assertEqual(json.dumps(cells, sort_keys=True), original)

    def test_multiple_declared_successes_are_not_outer_retries(self):
        for mode, status, signal in (
            ("verify", 0, None), ("chaos", 17, None), ("custom", None, 15),
        ):
            with self.subTest(mode=mode):
                cells = self._cells(["pass"] * 10)
                for cell in cells:
                    cell["mode"] = mode
                    first = cell["attempts"][0]
                    first.update(status=status, signal=signal)
                    second = json.loads(json.dumps(first))
                    second["index"] = "seed-2"
                    cell["attempts"].append(second)
                result = self._classify_cells(cells)
                self.assertEqual(result["classification"], "promotion-candidate")
                self.assertEqual(result["qualifying_passes"], 10)
                self.assertEqual(result["retried_repetitions"], 0)

    def test_nonpassing_fold_retains_adverse_facts_and_neutral_successes(self):
        cells = self._cells(["product"] * 10)
        for cell in cells:
            good = self.projection._attempt()
            good["index"] = "2"
            cell["attempts"].append(good)
        result = self._classify_cells(cells)
        self.assertEqual(result["classification"], "confirmed-failing")
        bad = self.projection._attempt("infrastructure_error")
        bad["index"] = "3"
        cells[0]["attempts"].append(bad)
        result = self._classify_cells(cells)
        self.assertEqual(result["classification"], "incomplete")
        self.assertEqual(result["terminal_product_failures"], 9)
        self.assertEqual(result["mixed_repetitions"], 1)

    def test_every_inner_report_must_support_canonical_qualification(self):
        for change in ("unequal-counts", "wrong-time", "skip-commit", "bad-hash", "missing-report", "duplicate-index"):
            with self.subTest(change=change):
                cells = self._cells(["pass"] * 10)
                first = cells[0]["attempts"][0]
                second = json.loads(json.dumps(first))
                second["index"] = "2"
                cells[0]["attempts"].append(second)
                report = json.loads(first["verification_report"])
                if change == "unequal-counts":
                    report["compared_log_messages"]["right"] += 1
                elif change == "wrong-time":
                    report["comparison"]["virtualize_time"] = False
                elif change == "skip-commit":
                    report["comparison"]["skip_commit"] = True
                first["verification_report"] = json.dumps(report)
                first["verification_report_sha256"] = hashlib.sha256(
                    first["verification_report"].encode()
                ).hexdigest()
                if change == "bad-hash":
                    first["verification_report_sha256"] = "0" * 64
                elif change == "missing-report":
                    first["verification_report"] = None
                    first["verification_report_sha256"] = None
                elif change == "duplicate-index":
                    second["index"] = first["index"]
                if change in ("bad-hash", "missing-report", "duplicate-index"):
                    with self.assertRaises(series.MalformedRow):
                        self._classify_cells(cells)
                else:
                    result = self._classify_cells(cells)
                    self.assertEqual(result["classification"], "intermittent")
                    self.assertEqual(result["qualifying_passes"], 9)

    def test_retry_order_gaps_and_dirty_or_unknown_history(self):
        for history in (("product", "infrastructure"), ("infrastructure", "pass")):
            cells = self._cells(["product"] * 10)
            attempts = [self._cells([kind])[0] for kind in history]
            for ordinal, attempt in enumerate(attempts, 1):
                attempt["attempt"] = ordinal
            result = self._classify_cells(attempts + cells[1:])
            self.assertEqual(result["retried_repetitions"], 1)
            self.assertFalse(result["promotion_candidate"])
            self.assertEqual(result["terminal_passes"], int(history[-1] == "pass"))
            self.assertEqual(result["classification"],
                             "intermittent" if history[-1] == "pass" else "incomplete")
        for change in ("gap", "third-attempt", "dirty", "missing-history"):
            with self.subTest(change=change):
                rows = self._rows(["pass"] * 10)
                payload = rows[0]["series"]
                if change == "gap":
                    payload["attempt"] = 2
                elif change == "third-attempt":
                    payload["attempt"] = 3
                elif change == "dirty":
                    payload["source_tree_dirty"] = True
                else:
                    payload.pop("pressure_evidence")
                result = series.classify_pressure_sample(rows, [self.CELL], "campaign-10x")[self.CELL]
                self.assertEqual(result["classification"], "incomplete")
                self.assertFalse(result["promotion_candidate"])

    def test_first_run_rejected_is_product_failure_with_valid_guest_disposition(self):
        cells = self._cells(["product"] * 10)
        for cell in cells:
            cell["result"] = "crash-error"
            attempt = self.projection._attempt("no_result")
            attempt.update(outcome="FAIL", error_kind=None)
            report = json.loads(attempt["verification_report"])
            report.update(guest_exit_code=17, guest_signal=None,
                          no_result_reason={"kind": "first_run_rejected", "exit_code": 17, "signal": None})
            attempt["verification_report"] = json.dumps(report)
            attempt["verification_report_sha256"] = hashlib.sha256(attempt["verification_report"].encode()).hexdigest()
            cell["attempts"] = [attempt]
        result = self._classify_cells(cells)
        self.assertEqual(result["classification"], "confirmed-failing")
        self.assertEqual(result["terminal_product_failures"], 10)
        for value in ("invalid", True, -1, 2**31):
            with self.subTest(guest_exit_code=value):
                malformed = json.loads(json.dumps(cells))
                attempt = malformed[0]["attempts"][0]
                report = json.loads(attempt["verification_report"])
                report["guest_exit_code"] = value
                report["no_result_reason"]["exit_code"] = value
                attempt["verification_report"] = json.dumps(report)
                attempt["verification_report_sha256"] = hashlib.sha256(attempt["verification_report"].encode()).hexdigest()
                with self.assertRaisesRegex(series.MalformedRow, "invalid guest_exit_code"):
                    self._classify_cells(malformed)

    def test_matched_reports_require_typed_guest_dispositions_and_info_counts(self):
        for change in ("guest-string", "guest-bool", "signal-zero", "signal-overflow", "both", "counts-overflow"):
            with self.subTest(change=change):
                cells = self._cells(["pass"] * 10)
                attempt = cells[0]["attempts"][0]
                report = json.loads(attempt["verification_report"])
                if change == "counts-overflow":
                    report["compared_log_messages"] = {"left": 2**64, "right": 2**64}
                elif change == "both":
                    report.update(guest_exit_code=0, guest_signal=9)
                else:
                    field = "guest_exit_code" if change.startswith("guest") else "guest_signal"
                    report[field] = {"guest-string": "invalid", "guest-bool": True,
                                     "signal-zero": 0, "signal-overflow": 2**31}[change]
                attempt["verification_report"] = json.dumps(report)
                attempt["verification_report_sha256"] = hashlib.sha256(attempt["verification_report"].encode()).hexdigest()
                with self.assertRaises(series.MalformedRow):
                    self._classify_cells(cells)

    def test_timeouts_and_known_nonproduct_causes_keep_their_categories(self):
        for error_kind, outer_kind, count in (
            ("guest-launch-refused", "prerequisite", "prerequisite_failures"),
            ("backend-unavailable", "prerequisite", "prerequisite_failures"),
            ("infrastructure", "infrastructure", "infrastructure_failures"),
            ("result-publication", "infrastructure", "infrastructure_failures"),
            ("incomplete-verification-evidence", "no_result", "no_results"),
            ("invalid-backend-evidence", "no_result", "no_results"),
        ):
            with self.subTest(error_kind=error_kind):
                cells = self._cells([outer_kind] * 10)
                for cell in cells:
                    attempt = self.projection._attempt("no_result", index="2")
                    attempt["error_kind"] = error_kind
                    cell["attempts"].append(attempt)
                result = self._classify_cells(cells)
                self.assertEqual(result[count], 10)
                self.assertFalse(result["promotion_candidate"])
        for missing_report in (False, True):
            cells = self._cells(["no_result"] * 10)
            for cell in cells:
                cell["attempts"] = [self.projection._attempt("no_result", timed_out=True)]
                if missing_report:
                    cell["attempts"][0].pop("verification_report")
                    cell["attempts"][0].pop("verification_report_sha256")
            result = self._classify_cells(cells)
            self.assertEqual(result["classification"], "no-result")
            self.assertEqual(result["no_results"], 10)

    def test_infrastructure_report_requires_actual_count_and_consistent_flags(self):
        for change in ("zero", "negative", "overflow", "verified", "parity", "reason"):
            with self.subTest(change=change):
                cell = self._cells(["infrastructure"])[0]
                attempt = cell["attempts"][0]
                report = json.loads(attempt["verification_report"])
                if change in ("zero", "negative", "overflow"):
                    report["infrastructure_error"]["count"] = {"zero": 0, "negative": -1, "overflow": 2**64}[change]
                elif change == "reason":
                    report["no_result_reason"] = {"kind": "not_run"}
                else:
                    report["verified" if change == "verified" else "bitwise_parity"] = True
                attempt["verification_report"] = json.dumps(report)
                attempt["verification_report_sha256"] = hashlib.sha256(attempt["verification_report"].encode()).hexdigest()
                with self.assertRaisesRegex(series.MalformedRow, "inconsistent infrastructure evidence"):
                    self._classify_cells([cell])

    def test_noncomparison_product_failure_requires_a_process_disposition(self):
        cells = self._cells(["product"] * 10)
        for cell in cells:
            cell["mode"] = "custom"
            cell["result"] = "crash-error"
        self.assertEqual(self._classify_cells(cells)["classification"], "confirmed-failing")
        for cell in cells:
            cell["attempts"][0]["status"] = None
        with self.assertRaisesRegex(series.MalformedRow, "lacks a process disposition"):
            self._classify_cells(cells)

    def test_classification_is_bound_to_and_does_not_mutate_the_selected_plan(self):
        selected = [self.CELL]
        original = list(selected)
        result = series.classify_pressure_sample(
            self._rows(["pass"] * 10), selected, "campaign-10x"
        )
        self.assertEqual(selected, original)
        self.assertEqual(set(result), set(selected))

        foreign = self._rows(["pass"] * 10)
        foreign[0]["series"]["cell"] = "unselected/test/verify/ptrace"
        with self.assertRaisesRegex(series.SeriesError, "unselected cell"):
            series.classify_pressure_sample(foreign, selected, "campaign-10x")


class ShardPath(unittest.TestCase):
    def test_shape_matches_the_ledger_convention(self):
        self.assertEqual(
            series.shard_path("hermit", "devbig014", "2026-08"),
            "series/hermit/devbig014/2026-08.jsonl",
        )

    def test_refuses_a_month_that_is_not_yyyy_mm(self):
        for bad in ("2026-8", "2026", "", "2026-08-25"):
            with self.assertRaises(series.MalformedRow, msg=bad):
                series.shard_path("hermit", "devbig014", bad)

    def test_refuses_a_host_that_would_escape_its_shard(self):
        # A traversal here would let one machine write another's shard, which is
        # exactly what per-machine sharding exists to prevent.
        for bad in ("..", "a/b", ""):
            with self.assertRaises(series.MalformedRow, msg=bad):
                series.shard_path("hermit", bad, "2026-08")


class RowLinter(unittest.TestCase):
    def test_accepts_a_well_formed_row(self):
        self.assertIsNotNone(series.validate_row(row()))

    def test_refuses_a_missing_required_envelope_key(self):
        bad = row()
        del bad["event_id"]
        with self.assertRaises(series.MalformedRow):
            series.validate_row(bad)

    def test_refuses_a_foreign_schema(self):
        with self.assertRaises(series.MalformedRow):
            series.validate_row(row(schema="validate-ledger/v1"))

    def test_requires_the_machine_kernel_and_capabilities_recorded_by_the_cell(self):
        missing = row()
        del missing["series"]["kernel_version"]
        with self.assertRaises(series.MalformedRow):
            series.validate_row(missing)

        mismatched = row(host="other-host")
        with self.assertRaises(series.MalformedRow):
            series.validate_row(mismatched)

        missing = row()
        del missing["series"]["host_capabilities"]
        with self.assertRaisesRegex(series.MalformedRow, "host_capabilities"):
            series.validate_row(missing)

        for capability in ("cpuid-faulting", "kvm"):
            incomplete = row()
            del incomplete["series"]["host_capabilities"][capability]
            with self.assertRaisesRegex(series.MalformedRow, capability):
                series.validate_row(incomplete)

    def test_readers_refuse_incomplete_host_facts_by_capability_name_but_keep_v1_readable(self):
        legacy = row(schema=series.LEGACY_ENVELOPE_VERSION)
        for field in ("machine_shortname", "kernel_version", "host_capabilities"):
            del legacy["series"][field]
        self.assertEqual(series.validate_read_rows([legacy]), [legacy])

        missing = row(schema=series.HOST_FACTS_ENVELOPE_VERSION)
        del missing["series"]["host_capabilities"]
        with self.assertRaisesRegex(series.MalformedRow, "host_capabilities"):
            series.validate_read_rows([missing])

        for capability in ("cpuid-faulting", "kvm"):
            incomplete = row(schema=series.HOST_FACTS_ENVELOPE_VERSION)
            del incomplete["series"]["host_capabilities"][capability]
            with self.assertRaisesRegex(series.MalformedRow, capability):
                series.validate_read_rows([incomplete])

    def test_union_refuses_an_incomplete_v2_row_before_it_becomes_evidence(self):
        incomplete = row(schema=series.HOST_FACTS_ENVELOPE_VERSION)
        del incomplete["series"]["host_capabilities"]["cpuid-faulting"]
        with tempfile.TemporaryDirectory() as td:
            shard = Path(td) / "rows.jsonl"
            shard.write_text(json.dumps(incomplete) + "\n")
            with self.assertRaisesRegex(series.MalformedRow, "cpuid-faulting"):
                series.union([shard])

    def test_refuses_a_tree_that_is_not_a_full_sha(self):
        for bad in ("abc", "A" * 40, "", "a" * 39):
            with self.assertRaises(series.MalformedRow, msg=bad):
                series.validate_row(row(series={"tree": bad}))

    def test_refuses_a_negative_or_non_integer_run_index(self):
        for bad in (-1, "0", 1.5, True):
            with self.assertRaises(series.MalformedRow, msg=repr(bad)):
                series.validate_row(row(series={"run_index": bad}))

    def test_refuses_an_unknown_outcome(self):
        with self.assertRaises(series.MalformedRow):
            series.validate_row(row(series={"outcome": "green"}))

    def test_v3_refuses_missing_or_mismatched_classification_by_name(self):
        missing = row(series={"outcome": "diverged", "result": None})
        with self.assertRaisesRegex(series.MalformedRow, "result=None"):
            series.validate_row(missing)
        with self.assertRaisesRegex(series.MalformedRow, "result=None"):
            series.validate_read_rows([missing])

        missing = row(series={
            "outcome": "diverged",
            "result": "determinism-failure",
            "failure_class": None,
        })
        with self.assertRaisesRegex(series.MalformedRow, "failure_class=None"):
            series.validate_row(missing)

        mismatched = row(series={
            "outcome": "diverged",
            "result": "crash-error",
            "failure_class": "product_failure",
        })
        with self.assertRaisesRegex(series.MalformedRow, "CrashError"):
            series.validate_row(mismatched)

        retained_v2 = row(schema=series.HOST_FACTS_ENVELOPE_VERSION)
        del retained_v2["series"]["result"]
        del retained_v2["series"]["failure_class"]
        self.assertEqual(series.validate_read_rows([retained_v2]), [retained_v2])
        with self.assertRaisesRegex(
            series.MalformedRow, "new rows must use stress-series/v3"
        ):
            series.validate_rows([retained_v2])

    def test_refuses_an_unknown_coordinate_key(self):
        with self.assertRaises(series.MalformedRow):
            series.validate_row(row(series={
                "outcome": "diverged", "coordinates": {"first_divergent_bytes": 1},
            }))

    def test_accepts_first_divergent_messages_and_nonconsecutive_run_span(self):
        series.validate_row(row(series={
            "outcome": "diverged",
            "num_runs": 2,
            "last_run_index": 7,
            "coordinates": {"first_divergent_record": 98},
            "first_divergent_messages": {
                "left": "INFO detcore: inbound syscall: read(3) = ?",
                "right": "INFO detcore: finish syscall #37: read(3) = Ok(1)",
            },
        }))

    def test_refuses_malformed_first_divergent_messages_and_run_span(self):
        bad_payloads = [
            {"first_divergent_messages": {"left": "only one side"}},
            {"first_divergent_messages": {"left": None, "right": None}},
            {"first_divergent_messages": {"left": "", "right": "right"}},
            {"last_run_index": -1},
            {"last_run_index": "7"},
            {"num_runs": 3, "last_run_index": 1},
        ]
        for payload in bad_payloads:
            with self.assertRaises(series.MalformedRow, msg=payload):
                series.validate_row(row(series={"outcome": "diverged", **payload}))

    def test_refuses_coordinates_on_a_row_that_did_not_diverge(self):
        # THE CROSS-FIELD RULE. A passing run contributing a position would put a
        # number into a range that describes divergences.
        for outcome in ("passed", "timeout", "errored", "no_result", "skipped"):
            with self.assertRaises(series.MalformedRow, msg=outcome):
                series.validate_row(row(series={
                    "outcome": outcome,
                    "coordinates": {"first_divergent_record": 98},
                }))
            with self.assertRaises(series.MalformedRow, msg=outcome):
                series.validate_row(row(series={
                    "outcome": outcome,
                    "first_divergent_messages": {"left": "a", "right": "b"},
                }))

    def test_allows_diverged_with_no_located_coordinate(self):
        # ⚠️ THE DISTINCT STATE, and the reason the rule above is one-directional.
        # 'it diverged and we could not locate where' is a real, different answer
        # from 'it passed' -- refusing it would force a producer to lie.
        self.assertIsNotNone(series.validate_row(row(series={
            "outcome": "diverged",
            "coordinates": {k: None for k in series.COORDINATE_KEYS},
        })))

    def test_allows_diverged_with_the_coordinates_key_ABSENT(self):
        # THE SHAPE A PRODUCER ACTUALLY EMITS. The test above pins
        # `coordinates` PRESENT and all-null; a writer with nothing to report is
        # far likelier to omit the key entirely, and that path was accepted but
        # undefended -- so a later change making `coordinates` mandatory would
        # have passed the suite and silently forced producers to skip the row.
        #
        # ⚠️ SKIPPING IS THE FAILURE THIS GUARDS. A cell with NO rows reads
        # `never-measured`; a cell with one coordinate-less diverged row reads
        # `diverged-unlocated`. Those are different answers, so a producer that
        # drops the row does not merely lose it -- it RELABELS a divergence it
        # could not locate as a cell nobody ever ran, which is the exact
        # under-count the store exists to make visible.
        self.assertIsNotNone(series.validate_row(row(series={
            "outcome": "diverged",
        })))
        self.assertEqual(
            series.measurement_state([row(series={"outcome": "diverged"})]),
            "diverged-unlocated",
        )
        self.assertEqual(series.measurement_state([]), "never-measured")

    def test_refuses_a_workspace_local_owner_path(self):
        needle = "/" + "home" + "/newton/secret"
        with self.assertRaises(series.MalformedRow):
            series.validate_row(row(producer=needle))

    def test_accepts_runtime_counts_compression_and_existing_depth_shape(self):
        series.validate_row(row(series={
            "num_runs": 200,
            "main_ancestry": True,
            "depth": {
                "hermit": {"commits": 2031, "first_parent": 1957},
                "reverie": {"commits": 941, "first_parent": 925},
            },
            "runtime": {
                "run1": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
                "run2": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
                "wall_time_min_ms": 5_101,
                "wall_time_max_ms": 5_285,
            },
        }))

    def test_refuses_false_runtime_or_compression_counts(self):
        bad_payloads = [
            {"num_runs": 0},
            {"num_runs": True},
            {"main_ancestry": "yes"},
            {"runtime": {"run1": {"scheduler_turns": -1}}},
            {"runtime": {"run1": {"syscalls": True}}},
            {"runtime": {"wall_time_min_ms": 10, "wall_time_max_ms": 9}},
            {"depth": {"hermit": {"commits": 10, "first_parent": 0}}},
        ]
        for payload in bad_payloads:
            with self.assertRaises(series.MalformedRow, msg=payload):
                series.validate_row(row(series=payload))


class DerivedViews(unittest.TestCase):
    def _diverged(self, run_index, **coords):
        return row(
            event_id=f"01J00000000000000000000{run_index:03d}",
            series={"run_index": run_index, "outcome": "diverged",
                    "coordinates": {**{k: None for k in series.COORDINATE_KEYS}, **coords}},
        )

    def test_observed_range_is_derived_per_coordinate(self):
        rows = [
            self._diverged(0, first_divergent_record=98),
            self._diverged(1, first_divergent_record=12),
            self._diverged(2, first_divergent_record=250),
        ]
        self.assertEqual(
            series.observed_range(rows, "first_divergent_record"),
            {"earliest": 12, "latest": 250, "samples": 3},
        )

    def test_observed_range_counts_collapsed_runs(self):
        collapsed = self._diverged(4, first_divergent_record=98)
        collapsed["series"]["num_runs"] = 30
        collapsed["series"]["last_run_index"] = 87
        self.assertEqual(
            series.observed_range([collapsed], "first_divergent_record"),
            {"earliest": 98, "latest": 98, "samples": 30},
        )

    def test_a_coordinate_no_row_located_is_none_not_zero(self):
        rows = [self._diverged(0, first_divergent_record=98)]
        self.assertIsNone(series.observed_range(rows, "first_divergent_syscall"))

    def test_a_passing_run_contributes_nothing_to_a_range(self):
        rows = [row(), self._diverged(1, first_divergent_record=5)]
        self.assertEqual(
            series.observed_range(rows, "first_divergent_record")["samples"], 1
        )

    def test_reader_ignores_a_located_coordinate_on_a_non_diverged_row(self):
        # ⚠️ CONSTRUCTED DIRECTLY, DELIBERATELY BYPASSING THE LINTER, because
        # this is the case the reader exists to survive. The linter refuses this
        # row at the WRITE boundary -- but the ledger model is explicit that a
        # reader "applies whatever is in the log, in order, without
        # re-litigating it", so a row written before the rule existed, or by a
        # buggy producer, will be read. If `observed_range` did not filter on
        # outcome, that row's position would silently enter a range describing
        # divergences.
        #
        # I FOUND THIS BY MUTATION: removing the outcome filter failed NO test,
        # because the previous case used a passing row with no coordinates at
        # all and therefore could not discriminate.
        smuggled = row(series={
            "outcome": "passed",
            "coordinates": {"first_divergent_record": 7},
        })
        with self.assertRaises(series.MalformedRow):
            series.validate_row(smuggled)          # the writer refuses it
        rows = [smuggled, self._diverged(1, first_divergent_record=500)]
        observed = series.observed_range(rows, "first_divergent_record")
        self.assertEqual(
            observed, {"earliest": 500, "latest": 500, "samples": 1},
            "a non-diverged row's coordinate leaked into the derived range",
        )

    def test_refuses_to_range_across_axes(self):
        # Record 98, syscall 37 and turn 4 described ONE divergence. A function
        # that accepted a list of axes would invite min() across that.
        with self.assertRaises(series.SeriesError):
            series.observed_range([], "first_divergent_anything")

    def test_measurement_state_distinguishes_all_four(self):
        self.assertEqual(series.measurement_state([]), "never-measured")
        self.assertEqual(series.measurement_state([row()]), "measured-and-passed")
        self.assertEqual(
            series.measurement_state([self._diverged(0)]), "diverged-unlocated"
        )
        self.assertEqual(
            series.measurement_state([self._diverged(0, first_divergent_record=1)]),
            "diverged",
        )


class StoreMechanics(unittest.TestCase):
    def test_union_dedups_the_same_row_from_spool_and_shard(self, ):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            spool = Path(d) / "spool.jsonl"
            shard = Path(d) / "shard.jsonl"
            payload = json.dumps(row(), sort_keys=True) + "\n"
            spool.write_text(payload)
            shard.write_text(payload)
            merged = series.union([spool, shard])
            self.assertEqual(len(merged), 1, "event_id dedup did not collapse them")

    def test_publisher_reads_retained_v1_before_appending_current_v3(self):
        legacy = row(schema=series.LEGACY_ENVELOPE_VERSION)
        current = row(event_id="current-v3", series={"run_index": 1})
        merged = publisher.merge_shard(
            json.dumps(legacy, sort_keys=True) + "\n",
            [current],
            store="series",
        )
        self.assertEqual(len(merged.splitlines()), 2)

    def test_publisher_reads_a_retained_v2_spool_that_the_current_writer_refuses(self):
        import tempfile

        retained = row(schema=series.HOST_FACTS_ENVELOPE_VERSION)
        del retained["series"]["result"]
        del retained["series"]["failure_class"]
        with self.assertRaisesRegex(
            series.MalformedRow, "new rows must use stress-series/v3"
        ):
            series.validate_rows([retained])

        with tempfile.TemporaryDirectory() as temporary:
            spool = Path(temporary)
            batch = spool / "retained-v2.jsonl"
            batch.write_text(json.dumps(retained, sort_keys=True) + "\n")
            pending, files = publisher._series_spool_snapshot(spool)

        self.assertEqual(files, [batch])
        self.assertEqual(
            pending[(retained["team"], retained["host"], "2026-08")],
            [retained],
        )

    def test_series_for_returns_repetitions_in_run_order(self):
        import tempfile
        rows = [
            row(event_id=f"01J0000000000000000000{i:04d}", series={"run_index": i})
            for i in (2, 0, 1)
        ]
        ordered = series.series_for(rows, rows[0]["series"]["cell"], "a" * 40)
        self.assertEqual([r["series"]["run_index"] for r in ordered], [0, 1, 2])

    def test_append_refuses_the_whole_batch_before_writing_any(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "series" / "s.jsonl"
            good, bad = row(), row(series={"tree": "nope"})
            with self.assertRaises(series.MalformedRow):
                series.append_rows(path, [good, bad])
            self.assertFalse(
                path.exists(),
                "a rejected batch must leave no partial write behind",
            )

    def test_append_then_rewrite_is_a_detectable_violation(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "s.jsonl"
            series.append_rows(path, [row()])
            before = path.read_text()
            after = before.replace('"passed"', '"diverged"')
            self.assertTrue(
                series.verify_append_only(before, after),
                "an edited line must be reported, not silently accepted",
            )


class Publication(unittest.TestCase):
    def _git(self, repo, *args):
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()

    def test_a_series_run_one_parent_writes_is_readable_by_another(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bare = root / "origin.git"
            producer = root / "producer"
            publisher_clone = root / "publisher"
            reader = root / "reader"
            subprocess.run(
                ["git", "init", "-q", "--bare", "-b", "main", str(bare)],
                check=True,
            )
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(producer)], check=True,
            )
            (producer / "README").write_text("series\n")
            self._git(producer, "add", "README")
            self._git(
                producer,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            )
            self._git(producer, "remote", "add", "origin", str(bare))
            self._git(producer, "push", "-q", "origin", "main")
            subprocess.run(
                ["git", "clone", "-q", str(bare), str(publisher_clone)], check=True,
            )
            subprocess.run(["git", "clone", "-q", str(bare), str(reader)], check=True)

            test = "backend-parity-c/flock-lifecycle"
            observation = row(series={"cell": f"{test}/verify/ptrace"})
            spool = series.spool_dir(producer)
            spool.mkdir(parents=True)
            series.append_rows(spool / "campaign-1.jsonl", [observation])

            before = series.history_rows(series.union(series.store_paths(reader)), test)
            self.assertEqual(before, [], "an independent checkout must not see local spool data")
            self.assertEqual(set(publish_backlog.pending_stores(producer)), {"series"})

            report = publisher.publish(
                publisher_clone,
                spool,
                source_root=producer,
                store="series",
            )
            self.assertEqual(report["published"], 1)
            self.assertEqual(
                report["shards"], ["series/hermit/devbig014/2026-08.jsonl"]
            )
            self.assertEqual(
                len(series.history_rows(series.union(series.store_paths(producer)), test)),
                1,
                "the producer must retain visibility until it fetches the tracked shard",
            )

            reader_head = self._git(reader, "rev-parse", "HEAD")
            after = series.history_rows(series.published_rows(reader), test)
            self.assertEqual(len(after), 1)
            self.assertEqual(after[0]["event_id"], observation["event_id"])
            import contextlib
            import io

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                rc = series.main(["history", "--parent", str(reader), "--test", test])
            self.assertEqual(rc, 0)
            self.assertIn("rows=1 runs=1 source=parent-origin-main+local-spool", stdout.getvalue())
            self.assertEqual(
                self._git(reader, "rev-parse", "HEAD"),
                reader_head,
                "the query must read fetched parent main without moving the checkout",
            )

    def test_publish_drains_a_retained_v2_spool_without_reopening_v2_writes(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bare = root / "origin.git"
            producer = root / "producer"
            publisher_clone = root / "publisher"
            subprocess.run(
                ["git", "init", "-q", "--bare", "-b", "main", str(bare)],
                check=True,
            )
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(producer)], check=True,
            )
            (producer / "README").write_text("series\n")
            self._git(producer, "add", "README")
            self._git(
                producer,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            )
            self._git(producer, "remote", "add", "origin", str(bare))
            self._git(producer, "push", "-q", "origin", "main")
            subprocess.run(
                ["git", "clone", "-q", str(bare), str(publisher_clone)], check=True,
            )

            retained = row(schema=series.HOST_FACTS_ENVELOPE_VERSION)
            del retained["series"]["result"]
            del retained["series"]["failure_class"]
            with self.assertRaisesRegex(
                series.MalformedRow, "new rows must use stress-series/v3"
            ):
                series.validate_rows([retained])

            spool = series.spool_dir(producer)
            spool.mkdir(parents=True)
            batch = spool / "retained-v2.jsonl"
            batch.write_text(json.dumps(retained, sort_keys=True) + "\n")

            report = publisher.publish(
                publisher_clone,
                spool,
                source_root=producer,
                store="series",
            )
            self.assertEqual(report["published"], 1)
            self.assertFalse(batch.exists())
            published = self._git(
                publisher_clone,
                "show",
                "origin/main:series/hermit/devbig014/2026-08.jsonl",
            )
            parsed = json.loads(published)
            self.assertEqual(series.validate_read_rows([parsed]), [retained])
    def test_published_snapshot_stays_on_one_commit_when_the_ref_moves(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary) / "parent"
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(repo)], check=True
            )
            shard = repo / "series/hermit/devbig014/2026-08.jsonl"
            shard.parent.mkdir(parents=True)
            first = row()
            shard.write_text(json.dumps(first, sort_keys=True) + "\n")
            self._git(repo, "add", "series")
            self._git(
                repo,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "first row",
            )
            first_commit = self._git(repo, "rev-parse", "HEAD")

            second = row(
                event_id="01J000000000000000000000AB",
                series={"run_index": 1},
            )
            with shard.open("a") as output:
                output.write(json.dumps(second, sort_keys=True) + "\n")
            self._git(repo, "add", "series")
            self._git(
                repo,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "second row",
            )
            second_commit = self._git(repo, "rev-parse", "HEAD")
            published_ref = "refs/remotes/origin/main"
            self._git(repo, "update-ref", published_ref, first_commit)

            real_git = series._git

            def move_after_resolution(checkout, args):
                result = real_git(checkout, args)
                if args == [
                    "--no-replace-objects",
                    "rev-parse",
                    "--verify",
                    f"{published_ref}^{{commit}}",
                ]:
                    self._git(repo, "update-ref", published_ref, second_commit)
                return result

            with mock.patch.object(series, "_git", side_effect=move_after_resolution):
                snapshot = series.published_snapshot_at(repo, published_ref)

            self.assertEqual(snapshot.commit, first_commit)
            self.assertEqual([item["event_id"] for item in snapshot.rows], [first["event_id"]])
            self.assertEqual(self._git(repo, "rev-parse", published_ref), second_commit)

    def test_published_snapshot_fetches_and_resolves_origin_main_once(self):
        expected = series.PublishedSnapshot("a" * 40, [])
        with mock.patch.object(series, "refresh_origin_main") as refresh:
            with mock.patch.object(
                series, "published_snapshot_at", return_value=expected
            ) as snapshot_at:
                actual = series.published_snapshot(Path("/parent"))
        self.assertIs(actual, expected)
        refresh.assert_called_once_with(Path("/parent"), "parent")
        snapshot_at.assert_called_once_with(
            Path("/parent"), "refs/remotes/origin/main"
        )

    def test_series_publication_refuses_a_row_the_series_linter_rejects(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bare = root / "origin.git"
            clone = root / "clone"
            producer = root / "producer"
            subprocess.run(
                ["git", "init", "-q", "--bare", "-b", "main", str(bare)],
                check=True,
            )
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(producer)], check=True,
            )
            (producer / "README").write_text("series\n")
            self._git(producer, "add", "README")
            self._git(
                producer,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            )
            self._git(producer, "remote", "add", "origin", str(bare))
            self._git(producer, "push", "-q", "origin", "main")
            subprocess.run(["git", "clone", "-q", str(bare), str(clone)], check=True)

            spool = series.spool_dir(producer)
            spool.mkdir(parents=True)
            invalid = row(series={"outcome": "passed", "coordinates": {
                "first_divergent_record": 1,
            }})
            (spool / "invalid.jsonl").write_text(json.dumps(invalid) + "\n")

            with self.assertRaises(publisher.PublishRefused):
                publisher.publish(clone, spool, source_root=producer, store="series")
            self.assertTrue((spool / "invalid.jsonl").exists())
            self.assertFalse((clone / "series").exists())

    def test_scheduled_sweep_passes_the_series_store_to_the_publisher(self):
        from unittest import mock

        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout='{"published": 1}\n', stderr="",
        )
        with mock.patch.object(publish_backlog.subprocess, "run", return_value=completed) as run:
            report = publish_backlog.publish_parent(
                Path("/producer"),
                Path("/publisher"),
                store="series",
                spool=Path("/producer/ignored/ci-hub/series-spool"),
                timeout=60,
                attribution="test attribution",
            )
        command = run.call_args.args[0]
        self.assertEqual(command[command.index("--store") + 1], "series")
        self.assertEqual(
            command[command.index("--spool-dir") + 1],
            "/producer/ignored/ci-hub/series-spool",
        )
        self.assertEqual(report["store"], "series")
        self.assertEqual(report["published"], 1)


class ScorecardProjectionSnapshot(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name) / "parent"
        subprocess.run(
            ["git", "init", "-q", "-b", "main", str(self.repo)], check=True
        )
        shard = self.repo / "series/hermit/devbig014/2026-08.jsonl"
        shard.parent.mkdir(parents=True)
        self.published = row()
        shard.write_text(json.dumps(self.published, sort_keys=True) + "\n")
        self._git("add", "series")
        self._git(
            "-c", "user.name=t", "-c", "user.email=t@invalid",
            "commit", "-q", "-m", "published series",
        )
        self.commit = self._git("rev-parse", "HEAD")
        self.tree = self._git("rev-parse", f"{self.commit}:series")
        self._git("update-ref", "refs/remotes/origin/main", self.commit)

    def _git(self, *arguments):
        return subprocess.run(
            ["git", "-C", str(self.repo), *arguments],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def _write_local(self, relative, item):
        target = series.spool_dir(self.repo) / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(item, sort_keys=True) + "\n")
        return target

    def test_snapshot_binds_published_tree_and_exact_local_sources(self):
        local = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        self._write_local("live.jsonl", local)
        self._write_local("published/duplicate.jsonl", self.published)
        output = Path(self.temporary.name) / "artifacts/run-1/series-snapshot.json"

        with mock.patch.object(series, "refresh_origin_main"):
            report = series.write_scorecard_projection_snapshot(self.repo, output)

        snapshot = series.read_scorecard_projection_snapshot(
            output, expected_sha256=report["sha256"]
        )
        self.assertEqual(series.SCORECARD_SNAPSHOT_SCHEMA, snapshot["schema"])
        self.assertEqual(self.commit, snapshot["source"]["commit"])
        self.assertEqual(self.tree, snapshot["source"]["tree"])
        self.assertEqual(1, snapshot["source"]["published_rows"])
        self.assertEqual(2, snapshot["rows_read"])
        self.assertEqual(
            [self.published["event_id"], local["event_id"]],
            [item["event_id"] for item in snapshot["rows"]],
        )
        self.assertEqual(
            ["live.jsonl", "published/duplicate.jsonl"],
            [item["path"] for item in snapshot["source"]["local_sources"]],
        )
        for item in snapshot["source"]["local_sources"]:
            self.assertGreaterEqual(item["device"], 0)
            self.assertGreater(item["inode"], 0)
        self.assertEqual(report["rows_read"], snapshot["rows_read"])
        self.assertEqual(report["source_commit"], self.commit)
        self.assertEqual(report["source_tree"], self.tree)

    def test_existing_identical_snapshot_is_idempotent_but_conflict_is_refused(self):
        output = Path(self.temporary.name) / "snapshot.json"
        with mock.patch.object(series, "refresh_origin_main"):
            first = series.write_scorecard_projection_snapshot(self.repo, output)
            second = series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertEqual(first, second)
        original = output.read_bytes()
        output.write_text("foreign\n")
        with mock.patch.object(series, "refresh_origin_main"):
            with self.assertRaisesRegex(series.SeriesError, "different content"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertEqual(b"foreign\n", output.read_bytes())
        self.assertNotEqual(original, output.read_bytes())

    def test_failed_post_publish_readback_leaves_replacement_untouched(self):
        output = Path(self.temporary.name) / "snapshot.json"
        foreign = output.with_name("foreign.json")
        foreign.write_text("foreign\n")
        original_finish = series._finish_snapshot_publication

        def replace_then_fail(directory_fd, published, digest):
            os.replace(foreign, published)
            return original_finish(directory_fd, published, digest)

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series, "_finish_snapshot_publication", side_effect=replace_then_fail,
        ):
            with self.assertRaisesRegex(series.SeriesError, "digest mismatch"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertEqual(b"foreign\n", output.read_bytes())

    def test_writer_lock_is_held_through_output_readback(self):
        output = Path(self.temporary.name) / "snapshot.json"
        local = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        self._write_local("live.jsonl", local)
        original_finish = series._finish_snapshot_publication

        def require_lock(directory_fd, published, digest):
            lock_path = series.spool_dir(self.repo) / ".append.lock"
            with lock_path.open("rb") as contender:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(contender.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            return original_finish(directory_fd, published, digest)

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series, "_finish_snapshot_publication", side_effect=require_lock,
        ):
            series.write_scorecard_projection_snapshot(self.repo, output)

    def test_named_stage_replacement_refuses_without_deleting_foreign_file(self):
        output = Path(self.temporary.name) / "snapshot.json"
        original_rename = series._rename_noreplace

        def replace_stage_then_rename(directory_fd, stage_name, output_name):
            foreign = output.parent / "replacement"
            foreign.write_text("foreign staging replacement\n")
            os.replace(foreign, output.parent / stage_name)
            original_rename(directory_fd, stage_name, output_name)

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series,
            "_open_anonymous_snapshot_stage",
            side_effect=OSError(errno.EOPNOTSUPP, "forced named stage"),
        ), mock.patch.object(
            series, "_rename_noreplace", side_effect=replace_stage_then_rename,
        ):
            with self.assertRaisesRegex(series.SeriesError, "digest mismatch"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertEqual(b"foreign staging replacement\n", output.read_bytes())

    def test_hardlink_fallback_reports_non_success_and_retains_staging(self):
        output = Path(self.temporary.name) / "snapshot.json"
        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series,
            "_open_anonymous_snapshot_stage",
            side_effect=OSError(errno.EOPNOTSUPP, "forced named stage"),
        ), mock.patch.object(
            series,
            "_rename_noreplace",
            side_effect=OSError(errno.ENOSYS, "forced hardlink fallback"),
        ):
            with self.assertRaisesRegex(
                series.SeriesError, "refusing success with named staging residue"
            ):
                series.write_scorecard_projection_snapshot(self.repo, output)

        stages = list(output.parent.glob(f".{output.name}.staging-*"))
        self.assertTrue(output.is_file())
        self.assertEqual(len(stages), 1)
        self.assertEqual(output.read_bytes(), stages[0].read_bytes())
        self.assertEqual(output.stat().st_ino, stages[0].stat().st_ino)

    def test_identical_concurrent_output_does_not_hide_named_staging_residue(self):
        output = Path(self.temporary.name) / "snapshot.json"

        def link_then_report_exists(directory_fd, stage_name, output_name):
            os.link(
                stage_name,
                output_name,
                src_dir_fd=directory_fd,
                dst_dir_fd=directory_fd,
                follow_symlinks=False,
            )
            raise FileExistsError(errno.EEXIST, "simulated concurrent output")

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series,
            "_open_anonymous_snapshot_stage",
            side_effect=OSError(errno.EOPNOTSUPP, "forced named stage"),
        ), mock.patch.object(
            series, "_rename_noreplace", side_effect=link_then_report_exists,
        ):
            with self.assertRaisesRegex(
                series.SeriesError, "refusing success because the staging pathname"
            ):
                series.write_scorecard_projection_snapshot(self.repo, output)

        stages = list(output.parent.glob(f".{output.name}.staging-*"))
        self.assertTrue(output.is_file())
        self.assertEqual(len(stages), 1)
        self.assertEqual(output.stat().st_ino, stages[0].stat().st_ino)

    def test_consumer_refuses_tampered_bytes_and_row_digest(self):
        output = Path(self.temporary.name) / "snapshot.json"
        with mock.patch.object(series, "refresh_origin_main"):
            report = series.write_scorecard_projection_snapshot(self.repo, output)
        snapshot = json.loads(output.read_text())
        snapshot["rows_sha256"] = "0" * 64
        output.write_text(
            json.dumps(snapshot, sort_keys=True, separators=(",", ":")) + "\n"
        )
        tampered_digest = hashlib.sha256(output.read_bytes()).hexdigest()
        with self.assertRaisesRegex(series.SeriesError, "row digest"):
            series.read_scorecard_projection_snapshot(
                output, expected_sha256=tampered_digest
            )
        with self.assertRaisesRegex(series.SeriesError, "digest mismatch"):
            series.read_scorecard_projection_snapshot(
                output, expected_sha256=report["sha256"]
            )

    def test_conflicting_event_identity_refuses_without_output(self):
        conflict = row(series={"outcome": "diverged"})
        self._write_local("conflict.jsonl", conflict)
        output = Path(self.temporary.name) / "snapshot.json"
        with mock.patch.object(series, "refresh_origin_main"):
            with self.assertRaisesRegex(series.SeriesError, "different bodies"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertFalse(output.exists())

    def test_local_source_set_change_refuses_without_output(self):
        first = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        second = row(
            event_id="01J000000000000000000000AC",
            run_id="campaign-3",
            series={"run_index": 2},
        )
        self._write_local("first.jsonl", first)
        output = Path(self.temporary.name) / "snapshot.json"
        original = series._open_snapshot_source

        def add_source_after_read(directory_fd, name, *, kind, relative):
            captured = original(
                directory_fd, name, kind=kind, relative=relative,
            )
            self._write_local("second.jsonl", second)
            return captured

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series, "_open_snapshot_source", side_effect=add_source_after_read
        ):
            with self.assertRaisesRegex(series.SeriesError, "spool changed"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertFalse(output.exists())

    def test_local_source_symlink_is_refused_without_output(self):
        local = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        target = self._write_local("real", local)
        linked = target.with_name("linked.jsonl")
        linked.symlink_to(target.name)
        output = Path(self.temporary.name) / "snapshot.json"

        with mock.patch.object(series, "refresh_origin_main"):
            with self.assertRaisesRegex(
                series.SeriesError, "cannot open regular series snapshot source"
            ):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertFalse(output.exists())

    def test_local_source_symlink_replacement_is_refused_without_output(self):
        local = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        target = self._write_local("live.jsonl", local)
        output = Path(self.temporary.name) / "snapshot.json"
        original = series._open_snapshot_source

        def replace_with_symlink(directory_fd, name, *, kind, relative):
            captured = original(
                directory_fd, name, kind=kind, relative=relative,
            )
            saved = target.with_name("saved-source")
            target.rename(saved)
            target.symlink_to(saved.name)
            return captured

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series, "_open_snapshot_source", side_effect=replace_with_symlink,
        ):
            with self.assertRaisesRegex(series.SeriesError, "changed"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertFalse(output.exists())

    def test_local_source_inode_replacement_is_refused_without_output(self):
        local = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        target = self._write_local("live.jsonl", local)
        output = Path(self.temporary.name) / "snapshot.json"
        original = series._open_snapshot_source

        def replace_after_open(directory_fd, name, *, kind, relative):
            captured = original(
                directory_fd, name, kind=kind, relative=relative,
            )
            replacement = target.with_name("replacement")
            replacement.write_bytes(target.read_bytes())
            os.replace(replacement, target)
            return captured

        with mock.patch.object(series, "refresh_origin_main"), mock.patch.object(
            series, "_open_snapshot_source", side_effect=replace_after_open,
        ):
            with self.assertRaisesRegex(series.SeriesError, "changed"):
                series.write_scorecard_projection_snapshot(self.repo, output)
        self.assertFalse(output.exists())

    def test_unavailable_published_source_never_falls_back_to_tracked_output(self):
        import contextlib
        import io

        output = Path(self.temporary.name) / "snapshot.json"
        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(
            series, "published_snapshot", side_effect=series.SeriesError("fetch failed")
        ), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            rc = series.main([
                "snapshot", "--parent", str(self.repo), "--output", str(output)
            ])
        self.assertEqual(1, rc)
        self.assertFalse(output.exists())
        self.assertEqual("", stdout.getvalue())
        self.assertIn("snapshot unavailable", stderr.getvalue())
        self.assertIn("no new snapshot was published", stderr.getvalue())

    def test_publication_and_reap_cannot_cross_an_old_snapshot_boundary(self):
        local = row(
            event_id="01J000000000000000000000AB",
            run_id="campaign-2",
            series={"run_index": 1},
        )
        live = self._write_local("live.jsonl", local)
        output = Path(self.temporary.name) / "snapshot.json"
        publisher_started = threading.Event()
        transition_entered = threading.Event()
        thread_errors = []
        thread = None

        def publish_transition(*_args, **_kwargs):
            transition_entered.set()
            retained = live.parent / "published" / live.name
            retained.parent.mkdir(parents=True, exist_ok=True)
            os.replace(live, retained)
            retained.unlink()
            return {"published": 1, "reaped_retained": 1}

        def run_publisher():
            publisher_started.set()
            try:
                publisher.publish(
                    Path("/unused-publisher-checkout"),
                    live.parent,
                    store="series",
                )
            except Exception as error:  # pragma: no cover - asserted below
                thread_errors.append(error)

        def bind_old_commit(_parent):
            nonlocal thread
            thread = threading.Thread(target=run_publisher)
            thread.start()
            self.assertTrue(publisher_started.wait(1))
            self.assertFalse(
                transition_entered.wait(0.1),
                "series publication entered while snapshot held the shared lock",
            )
            return series.PublishedSnapshot(self.commit, [self.published])

        with mock.patch.object(
            series, "published_snapshot", side_effect=bind_old_commit,
        ), mock.patch.object(
            publisher,
            "_publish_with_transition_lock_held",
            side_effect=publish_transition,
        ):
            report = series.write_scorecard_projection_snapshot(self.repo, output)
            assert thread is not None
            thread.join(2)

        self.assertFalse(thread.is_alive())
        self.assertEqual(thread_errors, [])
        self.assertTrue(transition_entered.is_set())
        snapshot = series.read_scorecard_projection_snapshot(
            output, expected_sha256=report["sha256"]
        )
        self.assertEqual(
            [self.published["event_id"], local["event_id"]],
            [item["event_id"] for item in snapshot["rows"]],
            "old published commit plus publish/reap omitted the local row",
        )

    def test_independent_series_reap_uses_the_exclusive_transition_lock(self):
        spool = series.spool_dir(self.repo)
        spool.mkdir(parents=True, exist_ok=True)
        reaper_started = threading.Event()
        reap_entered = threading.Event()
        thread_errors = []

        def reap_transition(*_args, **_kwargs):
            reap_entered.set()
            return 0

        def run_reaper():
            reaper_started.set()
            try:
                publisher.reap_retained(spool, self.repo, store="series")
            except Exception as error:  # pragma: no cover - asserted below
                thread_errors.append(error)

        with mock.patch.object(
            publisher,
            "_reap_retained_with_transition_lock_held",
            side_effect=reap_transition,
        ):
            with series.series_spool_transition_lock(spool, exclusive=False):
                thread = threading.Thread(target=run_reaper)
                thread.start()
                self.assertTrue(reaper_started.wait(1))
                self.assertFalse(
                    reap_entered.wait(0.1),
                    "series reaping entered while a snapshot lock was held",
                )
            thread.join(2)

        self.assertFalse(thread.is_alive())
        self.assertEqual(thread_errors, [])
        self.assertTrue(reap_entered.is_set())


class WriteBoundaryCli(unittest.TestCase):
    """The CLI exists so a producer can CALL `validate_row` rather than
    reimplement it in another language. These pin the properties that make
    calling it worth more than matching it."""

    SHA = "a" * 40
    CELL = "c-programs/pipecap/verify/ptrace"

    def _row(self, event_id, run_index, outcome, coordinates=None, run_id="campaign-1"):
        result, failure_class = classification_for_series_outcome(outcome)
        payload = {
            "cell": self.CELL, "tree": self.SHA,
            "run_index": run_index, "outcome": outcome,
            "result": result, "failure_class": failure_class,
            "machine_shortname": "devbig014",
            "kernel_version": "7.1.3-test",
            "host_capabilities": {
                "cpuid-faulting": {"present": False, "evidence": "fixture absence"},
                "kvm": {"present": True, "evidence": "fixture presence"},
            },
        }
        if coordinates is not None:
            payload["coordinates"] = coordinates
        return {
            "schema": series.ENVELOPE_VERSION, "event_id": event_id,
            "event_type": "series.observation", "emitted_at": "2026-08-25T16:00:00Z",
            "team": "hermit", "host": "devbig014", "run_id": run_id,
            "producer": "pressure-test",
            "series": payload,
        }

    def _append(self, parent, rows):
        import io
        import contextlib
        stdin = io.StringIO("\n".join(json.dumps(r) for r in rows) + "\n")
        out, err = io.StringIO(), io.StringIO()
        real = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                rc = series.main(["append", "--parent", str(parent)])
        finally:
            sys.stdin = real
        return rc, out.getvalue() + err.getvalue()

    def _spool_lines(self, parent):
        d = series.spool_dir(parent)
        if not d.is_dir():
            return []
        return [l for f in sorted(d.glob("*.jsonl")) for l in f.read_text().splitlines() if l]

    def test_appends_to_the_spool_and_never_the_tracked_shard(self):
        import tempfile
        with tempfile.TemporaryDirectory() as parent:
            rc, _ = self._append(parent, [self._row("e1", 0, "passed")])
            self.assertEqual(rc, 0)
            self.assertEqual(len(self._spool_lines(parent)), 1)
            # The tracked root must not be created by a producer.
            self.assertFalse((Path(parent) / "series").exists())

    def test_a_malformed_row_refuses_the_WHOLE_batch(self):
        """Partial writes are the failure this must not have: a half-written
        campaign reads as a complete one with fewer repetitions."""
        import tempfile
        with tempfile.TemporaryDirectory() as parent:
            self._append(parent, [self._row("good", 0, "passed")])
            before = self._spool_lines(parent)
            rc, text = self._append(parent, [
                self._row("ok", 1, "diverged", {"first_divergent_record": 98}),
                self._row("bad", 2, "passed", {"first_divergent_record": 98}),
            ])
            self.assertEqual(rc, 1)
            self.assertIn("REFUSED", text)
            self.assertEqual(self._spool_lines(parent), before,
                             "a refused batch must leave the spool byte-identical")

    def test_diverged_without_a_coordinate_is_accepted(self):
        """diverged-unlocated is a legitimate state; a producer must never be
        forced to invent a position it does not have."""
        import tempfile
        with tempfile.TemporaryDirectory() as parent:
            rc, _ = self._append(parent, [self._row("e1", 0, "diverged")])
            self.assertEqual(rc, 0)
            rows = series.union(sorted(series.spool_dir(parent).glob("*.jsonl")))
            self.assertEqual(series.measurement_state(rows), "diverged-unlocated")

    def test_a_batch_spanning_two_run_ids_is_refused(self):
        """The batch is named after its run, so two runs in one batch would
        silently file half of them under the other's name."""
        import tempfile
        with tempfile.TemporaryDirectory() as parent:
            rc, text = self._append(parent, [
                self._row("e1", 0, "passed", run_id="campaign-1"),
                self._row("e2", 0, "passed", run_id="campaign-2"),
            ])
            self.assertEqual(rc, 1)
            self.assertIn("exactly one run_id", text)
            self.assertEqual(self._spool_lines(parent), [])

    def test_the_batch_name_is_stable_so_a_rerun_dedups(self):
        """Not clock-derived: re-running one campaign must land in the same file
        so union_events can dedup by event_id instead of double-counting."""
        import tempfile
        with tempfile.TemporaryDirectory() as parent:
            rows = [self._row("e1", 0, "passed"), self._row("e2", 1, "diverged")]
            self._append(parent, rows)
            self._append(parent, rows)
            files = sorted(series.spool_dir(parent).glob("*.jsonl"))
            self.assertEqual(len(files), 1, "a rerun must not create a second batch file")
            unioned = series.union(files)
            self.assertEqual(len(unioned), 2, "event_id dedup must collapse the rerun")

    def test_no_rows_is_not_an_error_but_is_not_silent(self):
        import tempfile
        with tempfile.TemporaryDirectory() as parent:
            rc, text = self._append(parent, [])
            self.assertEqual(rc, 0)
            self.assertIn("nothing appended", text)


class CellResultProjection(unittest.TestCase):
    SHA = ""

    def setUp(self):
        import tempfile

        self.temporary = tempfile.TemporaryDirectory()
        self.parent = Path(self.temporary.name)
        self.repo = self.parent / "hermit"
        self.repo.mkdir()
        subprocess.run(["git", "-C", str(self.repo), "init", "-q"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.name", "Series Test"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.email", "series@example.invalid"],
            check=True,
        )
        (self.repo / "one").write_text("one\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "one"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "one"], check=True,
        )
        self.detcore_tree = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        subprocess.run(
            [
                "git", "-C", str(self.repo), "update-index", "--add", "--cacheinfo",
                f"160000,{self.detcore_tree},detcore",
            ],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "record detcore"], check=True,
        )
        self.tree = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        subprocess.run(
            [
                "git", "-C", str(self.repo), "update-ref",
                "refs/remotes/origin/main", self.tree,
            ],
            check=True,
        )

    def tearDown(self):
        self.temporary.cleanup()

    def test_linked_parent_worktree_uses_the_primary_checkout_spool(self):
        primary = self.parent / "dev-hermit"
        (primary / "ci-hub" / "series").mkdir(parents=True)
        (primary / "ci-hub" / "series" / "series.py").write_text(
            "# producer marker\n", encoding="utf-8"
        )
        subprocess.run(["git", "init", "-q", "-b", "main", str(primary)], check=True)
        subprocess.run(["git", "-C", str(primary), "add", "."], check=True)
        subprocess.run(
            [
                "git", "-C", str(primary),
                "-c", "user.name=t", "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            ],
            check=True,
        )
        linked = self.parent / "nested" / "slot" / "dev-hermit"
        linked.parent.mkdir(parents=True)
        subprocess.run(
            ["git", "-C", str(primary), "worktree", "add", "--detach", str(linked), "HEAD"],
            check=True,
            capture_output=True,
        )

        self.assertEqual(
            series.spool_dir(linked),
            primary / "ignored" / "ci-hub" / "series-spool",
        )

        outside = self.parent / "plain-directory"
        outside.mkdir()
        self.assertEqual(
            series.spool_dir(outside),
            outside / "ignored" / "ci-hub" / "series-spool",
        )

    def _attempt(
        self,
        verdict="matched",
        *,
        record=None,
        syscall=None,
        scheduler_turn=None,
        virtual_nanoseconds=None,
        left=None,
        right=None,
        timed_out=False,
        index="1",
    ):
        comparison = {
            "strictness": "canonical",
            "display_name": "BitwiseInfoV1",
            "compare_logs": True,
            "compare_io_buffers": True,
            "log_scope": "info",
            "record_envelope": "all_records_v1",
            "virtualize_time": True,
            "strip_lines": False,
            "canonicalize_addresses": True,
            "full_trace": True,
            "exact_remainder": True,
            "stripped_prefixes": ["real-wall-clock-prefix/v1"],
            "canonicalizations": ["host-address-to-first-appearance-ordinal/v1"],
            "ignore_lines": False,
            "skip_commit": False,
            "skip_detlog": False,
        }
        compared = {"left": 17, "right": 17}
        report = {
            "verified": verdict == "matched",
            "bitwise_parity": verdict == "matched",
            "verdict": verdict,
            "infrastructure_error": None,
            "comparison": comparison if verdict in {"matched", "diverged"} else None,
            "compared_log_messages": compared if verdict in {"matched", "diverged"} else None,
            "runtime": None,
            "guest_exit_code": None,
            "guest_signal": None,
            "first_divergent_scheduler_turn": scheduler_turn,
            "first_divergent_virtual_nanoseconds": virtual_nanoseconds,
            "first_divergent_record": record,
            "first_divergent_syscall": syscall,
            "first_divergent_left_message": left,
            "first_divergent_right_message": right,
        }
        if verdict == "no_result":
            report["no_result_reason"] = {"kind": "not_run"}
        elif verdict == "infrastructure_error":
            report["infrastructure_error"] = {
                "kind": "skid_overshoot",
                "count": 1,
            }
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        return {
            "index": index,
            "outcome": (
                "PASS" if verdict == "matched"
                else "FAIL" if verdict == "diverged"
                else "ERROR"
            ),
            "error_kind": (
                "incomplete-verification-evidence"
                if timed_out or verdict == "no_result"
                else None
            ),
            "status": None if timed_out else (0 if verdict == "matched" else 1),
            "signal": 15 if timed_out else None,
            "timed_out": timed_out,
            "verification_report": raw,
            "verification_report_sha256": hashlib.sha256(raw.encode()).hexdigest(),
            "first_divergent_record": record,
            "first_divergent_syscall": syscall,
            "first_divergent_scheduler_turn": scheduler_turn,
            "first_divergent_virtual_nanoseconds": virtual_nanoseconds,
            "first_divergent_left_message": left,
            "first_divergent_right_message": right,
        }

    def _cell(self, run_index, outcome="PASS", duration_ms=100):
        result, failure_class = {
            "PASS": ("pass", None),
            "FAIL": ("determinism-failure", "product_failure"),
            "ERROR": (None, "understood_infrastructure_failure"),
            "HOST-INAPPLICABLE": (None, "understood_prerequisite_failure"),
        }.get(outcome, (None, None))
        return {
            "schema": 4,
            # The raw harness uses a separate evidence id per pressure
            # repetition. append-cells must NOT use it as the batch identity.
            "run_id": f"cell-evidence-{run_index}",
            "machine_shortname": "devbig014",
            "kernel_version": "7.1.3-test",
            "host_capabilities": {
                "cpuid-faulting": {"present": False, "evidence": "fixture absence"},
                "kvm": {"present": True, "evidence": "fixture presence"},
            },
            "hermit_sha": self.tree,
            "source_tree_dirty": False,
            "test": "applications/git-repository-workflow",
            "category": "applications",
            "lane": "portable",
            "mode": "verify",
            "backend": "ptrace",
            "classification": "required",
            "outcome": outcome,
            "result": result,
            "failure_class": failure_class,
            "duration_ms": duration_ms,
            "run_index": run_index,
            "runtime": {
                "run1": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
                "run2": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
            },
            "attempts": [self._attempt()],
        }

    def _project(self, cells, *, producer="pressure-test"):
        return series.rows_from_cell_results(
            cells,
            parent=self.parent,
            producer=producer,
            run_id="pressure-one",
            tree=self.tree,
            emitted_at="2026-08-26T10:00:00Z",
        )

    def _validate_cell(self, attempt, outcome="PASS", duration_ms=100):
        cell = self._cell(attempt - 1, outcome=outcome, duration_ms=duration_ms)
        cell.pop("run_index")
        cell["attempt"] = attempt
        return cell

    def _validate_diverged_cell(self, attempt, duration_ms=100):
        cell = self._diverged_cell(
            attempt - 1,
            record=1_718,
            syscall=7,
            scheduler_turn=1_248,
            virtual_nanoseconds=80_000,
            duration_ms=duration_ms,
        )
        cell.pop("run_index")
        cell["attempt"] = attempt
        return cell

    def _validate_no_verdict_cell(self, attempt, *, timed_out, backend):
        cell = self._validate_cell(attempt, outcome="ERROR")
        cell["backend"] = backend
        cell["result"] = None
        cell["failure_class"] = "no_result"
        inner = self._attempt("no_result", timed_out=timed_out)
        if not timed_out:
            inner["error_kind"] = "incomplete-verification-evidence"
            inner["status"] = 125
        cell["attempts"] = [inner]
        # The synthetic repository does not contain a Detcore gitlink. Real
        # retained results omit this field and the producer resolves it from
        # the immutable Hermit tree.
        cell["detcore_tree"] = self.detcore_tree
        return cell

    def _comparison_no_result_cell(self, reason, *, mode="verify"):
        cell = self._validate_no_verdict_cell(1, timed_out=False, backend="ptrace")
        cell["mode"] = mode
        report = json.loads(cell["attempts"][0]["verification_report"])
        report["no_result_reason"] = reason
        self._replace_comparison_report(cell["attempts"][0], report)
        return cell

    @staticmethod
    def _replace_comparison_report(attempt, report):
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        attempt["verification_report"] = raw
        attempt["verification_report_sha256"] = hashlib.sha256(raw.encode()).hexdigest()

    def test_unspecified_and_refused_comparisons_preserve_evidence(self):
        detail = "  second log exceeded its bound: left\\right\nπ  "
        for reason, expected_kind in (
            (None, "unspecified"),
            ({"kind": "comparison_refused", "detail": detail}, "comparison_refused"),
        ):
            for mode in ("verify", "replay", "chaos"):
                for producer in ("validate", "pressure-test"):
                    for signaled in (False, True):
                        with self.subTest(kind=expected_kind, mode=mode,
                                          producer=producer, signaled=signaled):
                            cell = self._comparison_no_result_cell(reason, mode=mode)
                            if producer == "pressure-test":
                                cell["run_index"] = 0
                            attempt = cell["attempts"][0]
                            if signaled:
                                attempt.update(status=None, signal=11)
                            original = json.dumps(cell, sort_keys=True)
                            rows = self._project([cell], producer=producer)
                            self.assertEqual(len(rows), 1)
                            payload = rows[0]["series"]
                            self.assertEqual(payload["outcome"], "no_result")
                            self.assertIsNone(payload["result"])
                            self.assertEqual(payload["failure_class"], "no_result")
                            evidence = payload["no_verdict_evidence"]
                            retained = evidence["attempts"][0]
                            self.assertEqual(retained["kind"], expected_kind)
                            self.assertEqual(retained.get("detail"),
                                             detail if reason is not None else None)
                            for field in ("error_kind", "status", "signal", "timed_out",
                                          "verification_report_sha256"):
                                self.assertEqual(retained[field], attempt[field])
                            self.assertEqual(retained["attempt_outcome"], "ERROR")
                            self.assertEqual(retained["disposition"], "no_result")
                            self.assertEqual(evidence["evidence_sha256"],
                                             series._cell_result_evidence_sha256(cell))
                            if producer == "pressure-test":
                                pressure = payload["pressure_evidence"]
                                self.assertEqual(pressure["evidence_sha256"],
                                                 evidence["evidence_sha256"])
                                self.assertEqual(pressure["attempts"][0]["comparison"], {
                                    "verdict": "no_result",
                                    "canonical": False,
                                    "report_sha256": attempt["verification_report_sha256"],
                                    "no_result_kind": expected_kind,
                                })
                            self.assertEqual(json.dumps(cell, sort_keys=True), original)

    def test_new_no_result_reasons_refuse_incomplete_or_contradictory_evidence(self):
        for reason in (None, {"kind": "comparison_refused", "detail": "truncated log"}):
            for mode in ("verify", "replay", "chaos"):
                cell = self._comparison_no_result_cell(reason, mode=mode)
                original = cell["attempts"][0]
                for mutation in (
                    {"outcome": "PASS"}, {"outcome": "FAIL"},
                    {"error_kind": None}, {"error_kind": " "},
                    {"status": 0}, {"status": None}, {"status": True},
                    {"signal": 11}, {"status": None, "signal": False},
                    {"timed_out": True}, {"timed_out": None},
                    {"verification_report_sha256": "0" * 64},
                ):
                    with self.subTest(reason=reason, mode=mode, mutation=mutation):
                        attempt = {**original, **mutation}
                        with self.assertRaises(series.MalformedRow):
                            series._typed_comparison_attempt(attempt, mode=mode, index=1)
                report = json.loads(original["verification_report"])
                for field, value in (
                    ("verified", True), ("bitwise_parity", True),
                    ("comparison", {}), ("compared_log_messages", {"left": 1, "right": 1}),
                    ("infrastructure_error", {"kind": "skid_overshoot", "count": 1}),
                    ("first_divergent_record", 1),
                    ("guest_exit_code", True), ("guest_exit_code", -1),
                    ("guest_signal", 0), ("guest_signal", 2**31),
                    ("no_result_reason", {"kind": "unknown"}),
                    ("no_result_reason", {"kind": "unspecified"}),
                    ("no_result_reason", {"kind": []}),
                    ("no_result_reason", {"kind": "comparison_refused"}),
                    ("no_result_reason", {"kind": "comparison_refused", "detail": " "}),
                    ("no_result_reason", {"kind": "comparison_refused", "detail": 1}),
                ):
                    with self.subTest(reason=reason, mode=mode, field=field, value=value):
                        attempt = dict(original)
                        self._replace_comparison_report(attempt, {**report, field: value})
                        with self.assertRaises(series.MalformedRow):
                            series._typed_comparison_attempt(attempt, mode=mode, index=1)
                for field in report:
                    with self.subTest(reason=reason, mode=mode, omitted=field):
                        if field == "runtime":
                            continue  # Runtime is not part of the current report contract.
                        omitted = dict(report)
                        del omitted[field]
                        attempt = dict(original)
                        self._replace_comparison_report(attempt, omitted)
                        with self.assertRaises(series.MalformedRow):
                            series._typed_comparison_attempt(attempt, mode=mode, index=1)
                other_error = {**original, "error_kind": "cli-error"}
                if reason is None:
                    self.assertEqual(series._typed_comparison_attempt(
                        other_error, mode=mode, index=1), "no_result")
                else:
                    with self.assertRaises(series.MalformedRow):
                        series._typed_comparison_attempt(other_error, mode=mode, index=1)

    def test_ten_refused_or_unspecified_comparisons_cannot_qualify(self):
        for reason in (None, {"kind": "comparison_refused", "detail": "truncated log"}):
            cells = []
            for index in range(1, 11):
                cell = self._comparison_no_result_cell(reason)
                cell["run_index"] = index
                cells.append(cell)
            rows = self._project(cells)
            name = series._cell_name(cells[0])
            result = series.classify_pressure_sample(rows, [name], "pressure-one")[name]
            self.assertEqual(result["classification"], "no-result")
            self.assertEqual(result["no_results"], 10)
            self.assertEqual(result["terminal_passes"], 0)
            self.assertFalse(result["promotion_candidate"])

    def test_current_no_result_reports_refuse_duplicate_fields(self):
        for reason in (None, {"kind": "comparison_refused", "detail": "truncated log"}):
            attempt = self._comparison_no_result_cell(reason)["attempts"][0]
            raw = attempt["verification_report"]
            encoded_reason = json.dumps(reason, separators=(",", ":"))
            replacements = [
                ('"verified":false', '"verified":true,"verified":false'),
                ('"verified":false', '"verified":false,"verified":false'),
                ('"no_result_reason":',
                 '"no_result_reason":{"kind":"not_run"},"no_result_reason":'),
                ('"no_result_reason":',
                 '"no_result_reason":' + encoded_reason + ',"no_result_reason":'),
            ]
            if reason is not None:
                replacements.extend([
                    ('"detail":"truncated log"', '"detail":" ","detail":"truncated log"'),
                    ('"detail":"truncated log"',
                     '"detail":"truncated log","detail":"truncated log"'),
                ])
            for old, new in replacements:
                with self.subTest(reason=reason, repeated=old, values=new):
                    self.assertEqual(raw.count(old), 1)
                    duplicated = raw.replace(old, new, 1)
                    changed = {**attempt, "verification_report": duplicated,
                               "verification_report_sha256": hashlib.sha256(
                                   duplicated.encode()).hexdigest()}
                    with self.assertRaisesRegex(series.MalformedRow, "duplicate"):
                        series._typed_comparison_attempt(changed, mode="verify", index=1)

    def _diverged_cell(
        self,
        run_index,
        *,
        record,
        syscall,
        scheduler_turn,
        virtual_nanoseconds,
        left="inbound read",
        right="finish read",
        duration_ms=100,
    ):
        cell = self._cell(run_index, outcome="FAIL", duration_ms=duration_ms)
        cell.update({
            "first_divergent_record": record,
            "first_divergent_syscall": syscall,
            "first_divergent_scheduler_turn": scheduler_turn,
            "first_divergent_virtual_nanoseconds": virtual_nanoseconds,
            "first_divergent_left_message": left,
            "first_divergent_right_message": right,
        })
        cell["attempts"] = [self._attempt(
            "diverged",
            record=record,
            syscall=syscall,
            scheduler_turn=scheduler_turn,
            virtual_nanoseconds=virtual_nanoseconds,
            left=left,
            right=right,
        )]
        return cell

    def test_two_hundred_raw_run_ids_become_one_batch_row(self):
        cells = [self._cell(i, duration_ms=5_100 + i) for i in range(200)]
        # Ordinary repeats still compress; pressure keeps individual evidence.
        rows = self._project(cells, producer="hermit-repeat")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["run_id"], "pressure-one")
        self.assertEqual(rows[0]["event_type"], "series.observation")
        self.assertEqual(rows[0]["producer"], "hermit-repeat")
        payload = rows[0]["series"]
        self.assertEqual(payload["run_index"], 0)
        self.assertEqual(payload["num_runs"], 200)
        self.assertEqual(payload["runtime"]["wall_time_min_ms"], 5_100)
        self.assertEqual(payload["runtime"]["wall_time_max_ms"], 5_299)
        self.assertEqual(payload["runtime"]["run1"]["syscalls"], 612)

        pressure = self._project(cells)
        self.assertEqual(len(pressure), 200)
        self.assertEqual({item["run_id"] for item in pressure}, {"pressure-one"})
        self.assertEqual({item["producer"] for item in pressure}, {"pressure-test"})
        self.assertEqual(len({item["event_id"] for item in pressure}), 200)
        self.assertEqual(len({item["series"]["pressure_evidence"]["evidence_sha256"]
                              for item in pressure}), 200)
        for index, item in enumerate(pressure):
            payload = item["series"]
            self.assertEqual(payload["run_index"], index)
            self.assertEqual(payload["attempt"], 1)
            self.assertEqual(payload["num_runs"], 1)
            self.assertEqual(payload["runtime"]["wall_time_min_ms"], 5_100 + index)
            self.assertEqual(payload["runtime"]["wall_time_max_ms"], 5_100 + index)
            self.assertEqual(payload["runtime"]["run1"]["syscalls"], 612)
            retained = payload["pressure_evidence"]["attempts"]
            self.assertEqual(len(retained), 1)
            self.assertEqual(retained[0]["comparison"]["report_sha256"],
                             cells[index]["attempts"][0]["verification_report_sha256"])

    def test_different_host_capability_sets_are_not_compressed_together(self):
        first = self._cell(0)
        second = self._cell(1)
        second["host_capabilities"]["cpuid-faulting"] = {
            "present": True,
            "evidence": "fixture presence",
        }
        rows = self._project([first, second])
        self.assertEqual(len(rows), 2)
        self.assertEqual(
            {
                row["series"]["host_capabilities"]["cpuid-faulting"]["present"]
                for row in rows
            },
            {False, True},
        )

    def test_two_hundred_runs_keep_two_exact_divergence_points_and_counts(self):
        cells = []
        for index in range(200):
            choice = index % 20
            if choice < 3:
                cells.append(self._diverged_cell(
                    index,
                    record=1_718,
                    syscall=7,
                    scheduler_turn=1_248,
                    virtual_nanoseconds=80_000,
                    duration_ms=4_000 + index,
                ))
            elif choice < 8:
                cells.append(self._diverged_cell(
                    index,
                    record=1_976,
                    syscall=7,
                    scheduler_turn=1_248,
                    virtual_nanoseconds=80_000,
                    duration_ms=4_000 + index,
                ))
            else:
                cells.append(self._cell(index, duration_ms=4_000 + index))

        rows = self._project(cells, producer="hermit-repeat")
        self.assertEqual(len(rows), 3)
        by_coordinates = {
            json.dumps(item["series"].get("coordinates"), sort_keys=True): item["series"]
            for item in rows
        }
        first = by_coordinates[json.dumps({
            "first_divergent_record": 1_718,
            "first_divergent_syscall": 7,
            "first_divergent_scheduler_turn": 1_248,
            "first_divergent_virtual_nanoseconds": 80_000,
        }, sort_keys=True)]
        second = by_coordinates[json.dumps({
            "first_divergent_record": 1_976,
            "first_divergent_syscall": 7,
            "first_divergent_scheduler_turn": 1_248,
            "first_divergent_virtual_nanoseconds": 80_000,
        }, sort_keys=True)]
        passed = by_coordinates["null"]
        self.assertEqual(first["num_runs"], 30)
        self.assertEqual(second["num_runs"], 50)
        self.assertEqual(passed["num_runs"], 120)
        self.assertEqual(first["coordinates"]["first_divergent_record"], 1_718)
        self.assertEqual(second["coordinates"]["first_divergent_record"], 1_976)
        self.assertEqual(first["first_divergent_messages"], {
            "left": "inbound read",
            "right": "finish read",
        })
        self.assertEqual(first["run_index"], 0)
        self.assertEqual(first["last_run_index"], 182)
        self.assertLess(len("\n".join(json.dumps(item) for item in rows)), 10_000)

        pressure = self._project(cells)
        self.assertEqual(len(pressure), 200)
        self.assertEqual(sum(item["series"]["outcome"] == "passed" for item in pressure), 120)
        for record, count in ((1_718, 30), (1_976, 50)):
            located = [item for item in pressure
                       if item["series"].get("coordinates", {}).get("first_divergent_record") == record]
            self.assertEqual(len(located), count)
            self.assertTrue(all(item["series"]["num_runs"] == 1 for item in located))
        self.assertEqual(series.observed_range(pressure, "first_divergent_record"),
                         series.observed_range(rows, "first_divergent_record"))

    def test_scattered_positions_remain_distinct_with_the_same_messages(self):
        cells = [
            self._diverged_cell(
                index,
                record=1_500 + index,
                syscall=7,
                scheduler_turn=1_248,
                virtual_nanoseconds=80_000,
            )
            for index in range(200)
        ]
        rows = self._project(cells, producer="hermit-repeat")
        self.assertEqual(len(rows), 200)
        self.assertEqual(
            len({row["series"]["coordinates"]["first_divergent_record"] for row in rows}),
            200,
        )
        self.assertTrue(all(row["series"]["num_runs"] == 1 for row in rows))
        self.assertLess(len("\n".join(json.dumps(item) for item in rows)), 300_000)

    def test_same_coordinates_with_different_messages_remain_distinct(self):
        coordinates = {
            "record": 1_718,
            "syscall": 7,
            "scheduler_turn": 1_248,
            "virtual_nanoseconds": 80_000,
        }
        rows = self._project([
            self._diverged_cell(0, **coordinates),
            self._diverged_cell(
                1,
                **coordinates,
                left="inbound write",
                right="finish write",
            ),
        ])
        self.assertEqual(len(rows), 2)

    def test_unmet_prerequisite_has_no_row_but_measured_zero_stays_zero(self):
        not_run = self._cell(0, outcome="HOST-INAPPLICABLE")
        not_run.pop("duration_ms")
        not_run["runtime"] = None
        self.assertEqual(self._project([not_run]), [])

        measured = self._cell(0, duration_ms=0)
        measured["runtime"] = None
        zero = self._project([measured])[0]["series"]["runtime"]
        self.assertEqual(zero["wall_time_min_ms"], 0)
        self.assertEqual(zero["wall_time_max_ms"], 0)

        for key, bad in (
            ("duration_ms", "0"),
            ("runtime", "not-an-object"),
            ("schema", 3),
            ("source_tree_dirty", "false"),
        ):
            malformed = self._cell(0)
            malformed[key] = bad
            with self.assertRaises(series.MalformedRow, msg=f"accepted malformed {key}"):
                self._project([malformed])

    def test_failed_retry_then_success_is_not_compressed_away(self):
        rows = series.rows_from_cell_results(
            [
                self._validate_diverged_cell(1, duration_ms=90),
                self._validate_cell(2, outcome="PASS", duration_ms=80),
            ],
            parent=self.parent,
            producer="validate",
            run_id="validate-one",
            tree=self.tree,
            emitted_at="2026-08-26T10:00:00Z",
        )
        self.assertEqual(
            [
                (
                    r["series"]["run_index"],
                    r["series"]["outcome"],
                    r["series"]["result"],
                    r["series"]["failure_class"],
                )
                for r in rows
            ],
            [
                (1, "diverged", "determinism-failure", "product_failure"),
                (2, "passed", "pass", None),
            ],
        )

    def test_validate_no_verdict_rows_keep_attempt_and_typed_disposition(self):
        timeout = self._validate_no_verdict_cell(
            1, timed_out=True, backend="sabre"
        )
        root = "/tmp/fixture/hermit"
        sibling = f"{root}-backup"
        embedded = f"--hermit={root}/target/debug/hermit"
        path_value = f"/usr/bin:{root}/bin"
        interior = f"prefix{root}/target/debug/hermit"
        timeout.update({
            "cwd": root,
            "argv": [
                f"{root}/target/debug/hermit",
                root,
                embedded,
                sibling,
                interior,
            ],
            "guest_argv": [f"{root}/guest"],
            "effective_args": [f"{root}/config", "π"],
            "env": {
                "HOME": f"{root}/home",
                "PATH": path_value,
                "SIBLING": sibling,
                "INTERIOR": interior,
            },
            "shell_command": "stale",
        })
        timeout["attempts"][0].update({
            "cwd": root,
            "argv": [f"{root}/target/debug/hermit"],
            "guest_argv": [f"{root}/guest"],
            "env": {"HOME": f"{root}/home"},
            "shell_command": "stale",
        })
        normalised = series._normalise_cell_result_for_evidence(timeout)
        self.assertEqual(normalised["cwd"], series.RECORDED_ROOT)
        self.assertEqual(
            normalised["argv"][:2],
            [f"{series.RECORDED_ROOT}/target/debug/hermit", series.RECORDED_ROOT],
        )
        self.assertEqual(
            normalised["argv"][2],
            f"--hermit={series.RECORDED_ROOT}/target/debug/hermit",
        )
        self.assertEqual(normalised["env"]["HOME"], f"{series.RECORDED_ROOT}/home")
        self.assertEqual(
            normalised["env"]["PATH"], f"/usr/bin:{series.RECORDED_ROOT}/bin"
        )
        self.assertEqual(normalised["argv"][3:], [sibling, interior])
        self.assertEqual(normalised["env"]["SIBLING"], sibling)
        self.assertEqual(normalised["env"]["INTERIOR"], interior)
        self.assertEqual(
            normalised["attempts"][0]["cwd"], series.RECORDED_ROOT
        )
        self.assertEqual(
            normalised["attempts"][0]["shell_command"],
            "cd /repo && env HOME=/repo/home /repo/target/debug/hermit",
        )
        encoded = json.dumps(normalised, sort_keys=True, ensure_ascii=False)
        self.assertEqual(
            series._replace_evidence_root_token(encoded, root, series.RECORDED_ROOT),
            encoded,
        )
        for delimiter in (
            " ", "\t", "\n", "\r", "\v", "\f", "\u00a0", "\u2003",
        ):
            self.assertEqual(
                series._replace_evidence_root_token(
                    f"{delimiter}{root}/nested", root, series.RECORDED_ROOT
                ),
                f"{delimiter}{series.RECORDED_ROOT}/nested",
            )
        for codepoint in range(0x1C, 0x20):
            control = chr(codepoint)
            value = f"{control}{root}/nested"
            self.assertEqual(
                series._replace_evidence_root_token(
                    value, root, series.RECORDED_ROOT
                ),
                value,
            )
        utf8_contract = json.loads(json.dumps(timeout))
        utf8_contract["hermit_sha"] = "a" * 40
        self.assertEqual(
            series._cell_result_evidence_sha256(utf8_contract),
            "6de0ee5d7b0c18ed4cf1bbd2c41f1df75e7d688ec59995c86cf6250e7c0b3096",
        )
        unavailable = self._validate_no_verdict_cell(
            2, timed_out=False, backend="kvm"
        )
        rows = series.rows_from_cell_results(
            [timeout, unavailable],
            parent=self.parent,
            producer="validate",
            run_id="validate-no-verdict",
            tree=self.tree,
            emitted_at="2026-08-30T20:00:00Z",
        )
        self.assertEqual(len(rows), 2)
        self.assertEqual(
            sorted([
                (
                    row["series"]["run_index"],
                    row["series"]["attempt"],
                    row["series"]["no_verdict_evidence"]["attempts"][0]["kind"],
                    row["series"]["no_verdict_evidence"]["attempts"][0]["attempt_outcome"],
                    row["series"]["no_verdict_evidence"]["attempts"][0]["disposition"],
                    row["series"]["no_verdict_evidence"]["attempts"][0]["timed_out"],
                )
                for row in rows
            ]),
            [
                (1, 1, "not_run", "ERROR", "timeout", True),
                (2, 2, "not_run", "ERROR", "no_result", False),
            ],
        )
        by_attempt = {row["series"]["attempt"]: row for row in rows}
        self.assertEqual(
            by_attempt[1]["series"]["no_verdict_evidence"]["evidence_sha256"],
            series._cell_result_evidence_sha256(timeout),
        )
        self.assertEqual(
            by_attempt[2]["series"]["no_verdict_evidence"]["evidence_sha256"],
            series._cell_result_evidence_sha256(unavailable),
        )
        self.assertNotEqual(rows[0]["event_id"], rows[1]["event_id"])
        self.assertEqual(
            [row["series"]["detcore_tree"] for row in rows],
            [self.detcore_tree, self.detcore_tree],
        )

        prelaunch = self._validate_no_verdict_cell(
            1, timed_out=True, backend="ptrace"
        )
        prelaunch["attempts"][0]["status"] = None
        prelaunch["attempts"][0]["signal"] = None
        recovered = self._validate_cell(2, outcome="PASS")
        recovered["detcore_tree"] = self.detcore_tree
        recovered_rows = series.rows_from_cell_results(
            [prelaunch, recovered],
            parent=self.parent,
            producer="validate",
            run_id="validate-recovered-prelaunch-not-run",
            tree=self.tree,
            emitted_at="2026-08-31T09:00:00Z",
        )
        self.assertEqual(
            [row["series"]["outcome"] for row in recovered_rows],
            ["no_result", "passed"],
        )
        prelaunch_disposition = recovered_rows[0]["series"][
            "no_verdict_evidence"
        ]["attempts"][0]
        self.assertEqual(
            (
                prelaunch_disposition["kind"],
                prelaunch_disposition["attempt_outcome"],
                prelaunch_disposition["disposition"],
                prelaunch_disposition["status"],
                prelaunch_disposition["signal"],
                prelaunch_disposition["timed_out"],
            ),
            ("not_run", "ERROR", "timeout", None, None, True),
        )

        for label, mutation in (
            ("not timed out", {"timed_out": False}),
            ("wrong error kind", {"error_kind": "infrastructure"}),
        ):
            malformed = json.loads(json.dumps(prelaunch))
            malformed["attempts"][0].update(mutation)
            with self.subTest(label=label), self.assertRaisesRegex(
                series.MalformedRow,
                "process or pre-launch timeout disposition",
            ):
                series.rows_from_cell_results(
                    [malformed],
                    parent=self.parent,
                    producer="validate",
                    run_id=f"validate-malformed-prelaunch-{label}",
                    tree=self.tree,
                    emitted_at="2026-08-31T09:00:00Z",
                )

        noncanonical = self._validate_cell(3, outcome="ERROR")
        noncanonical["result"] = None
        noncanonical["failure_class"] = "no_result"
        noncanonical["detcore_tree"] = self.detcore_tree
        report = json.loads(noncanonical["attempts"][0]["verification_report"])
        report["comparison"]["display_name"] = "Stripped"
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        noncanonical["attempts"][0]["verification_report"] = raw
        noncanonical["attempts"][0]["verification_report_sha256"] = hashlib.sha256(
            raw.encode()
        ).hexdigest()
        weak_row = series.rows_from_cell_results(
            [noncanonical],
            parent=self.parent,
            producer="validate",
            run_id="validate-noncanonical",
            tree=self.tree,
            emitted_at="2026-08-30T20:00:00Z",
        )[0]
        disposition = weak_row["series"]["no_verdict_evidence"]["attempts"][0]
        self.assertEqual(disposition["kind"], "noncanonical_match")
        self.assertEqual(disposition["attempt_outcome"], "PASS")
        self.assertEqual(disposition["disposition"], "no_result")

        inconsistent_match = json.loads(json.dumps(noncanonical))
        report = json.loads(
            inconsistent_match["attempts"][0]["verification_report"]
        )
        report["verified"] = False
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        inconsistent_match["attempts"][0]["verification_report"] = raw
        inconsistent_match["attempts"][0]["verification_report_sha256"] = (
            hashlib.sha256(raw.encode()).hexdigest()
        )
        with self.assertRaisesRegex(
            series.MalformedRow, "matched report is internally inconsistent"
        ):
            series.rows_from_cell_results(
                [inconsistent_match],
                parent=self.parent,
                producer="validate",
                run_id="validate-noncanonical-bad-match",
                tree=self.tree,
                emitted_at="2026-08-30T20:00:00Z",
            )

        noncanonical_divergence = json.loads(json.dumps(noncanonical))
        attempt = noncanonical_divergence["attempts"][0]
        report = json.loads(attempt["verification_report"])
        report["verdict"] = "diverged"
        report["verified"] = False
        report["bitwise_parity"] = False
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        attempt.update({
            "outcome": "FAIL",
            "status": 1,
            "verification_report": raw,
            "verification_report_sha256": hashlib.sha256(raw.encode()).hexdigest(),
        })
        divergence_row = series.rows_from_cell_results(
            [noncanonical_divergence],
            parent=self.parent,
            producer="validate",
            run_id="validate-noncanonical-divergence",
            tree=self.tree,
            emitted_at="2026-08-30T20:00:00Z",
        )[0]
        self.assertEqual(
            divergence_row["series"]["no_verdict_evidence"]["attempts"][0]["kind"],
            "noncanonical_divergence",
        )
        inconsistent_divergence = json.loads(json.dumps(noncanonical_divergence))
        attempt = inconsistent_divergence["attempts"][0]
        report = json.loads(attempt["verification_report"])
        report["verified"] = True
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        attempt["verification_report"] = raw
        attempt["verification_report_sha256"] = hashlib.sha256(
            raw.encode()
        ).hexdigest()
        with self.assertRaisesRegex(
            series.MalformedRow, "divergence report is internally inconsistent"
        ):
            series.rows_from_cell_results(
                [inconsistent_divergence],
                parent=self.parent,
                producer="validate",
                run_id="validate-noncanonical-bad-divergence",
                tree=self.tree,
                emitted_at="2026-08-30T20:00:00Z",
            )

        passing = series.rows_from_cell_results(
            [self._validate_cell(1)],
            parent=self.parent,
            producer="validate",
            run_id="validate-pass",
            tree=self.tree,
            emitted_at="2026-08-30T20:00:00Z",
        )[0]
        self.assertNotIn("attempt", passing["series"])
        self.assertNotIn("no_verdict_evidence", passing["series"])

    def test_no_verdict_contradiction_refuses_the_batch(self):
        contradictory = self._validate_no_verdict_cell(
            1, timed_out=True, backend="sabre"
        )
        contradictory["outcome"] = "FAIL"
        contradictory["result"] = "crash-error"
        contradictory["failure_class"] = "product_failure"
        with self.assertRaisesRegex(
            series.MalformedRow,
            "no supporting canonical typed attempt",
        ):
            series.rows_from_cell_results(
                [contradictory],
                parent=self.parent,
                producer="validate",
                run_id="validate-contradiction",
                tree=self.tree,
                emitted_at="2026-08-30T20:00:00Z",
            )

        for status in (0, None):
            incomplete = self._validate_no_verdict_cell(
                1, timed_out=False, backend="kvm"
            )
            incomplete["attempts"][0]["status"] = status
            with self.subTest(status=status), self.assertRaisesRegex(
                series.MalformedRow,
                "NotRun report has no complete process or pre-launch timeout disposition",
            ):
                series.rows_from_cell_results(
                    [incomplete],
                    parent=self.parent,
                    producer="validate",
                    run_id="validate-incomplete-disposition",
                    tree=self.tree,
                    emitted_at="2026-08-30T20:00:00Z",
                )

    def test_hermit_repeat_preserves_detcore_tree(self):
        import contextlib
        import io

        cell = self._cell(0)
        cell["detcore_tree"] = "b" * 40
        rows = series.rows_from_cell_results(
            [cell],
            parent=self.parent,
            producer="hermit-repeat",
            run_id="repeat-one",
            tree=self.tree,
            emitted_at="2026-08-27T10:00:00Z",
        )
        self.assertEqual(rows[0]["producer"], "hermit-repeat")
        self.assertEqual(rows[0]["series"]["detcore_tree"], "b" * 40)

        real_stdin = sys.stdin
        sys.stdin = io.StringIO(json.dumps(cell) + "\n")
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "hermit-repeat",
                    "--run-id", "repeat-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-27T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        stored = series.union(sorted(series.spool_dir(self.parent).glob("*.jsonl")))
        self.assertEqual(len(stored), 1)
        self.assertEqual(stored[0]["producer"], "hermit-repeat")
        self.assertEqual(stored[0]["series"]["detcore_tree"], "b" * 40)

        cell["detcore_tree"] = "not-an-object-id"
        with self.assertRaisesRegex(
            series.MalformedRow,
            "detcore_tree must be a 40-hex object id",
        ):
            series.rows_from_cell_results(
                [cell],
                parent=self.parent,
                producer="hermit-repeat",
                run_id="repeat-one",
                tree=self.tree,
                emitted_at="2026-08-27T10:00:00Z",
            )

    def test_pressure_retry_retains_both_attempts_at_one_repetition(self):
        first = self._diverged_cell(
            7,
            record=1_718,
            syscall=7,
            scheduler_turn=1_248,
            virtual_nanoseconds=80_000,
            duration_ms=90,
        )
        first["attempt"] = 1
        second = self._cell(7, outcome="PASS", duration_ms=80)
        second["attempt"] = 2
        rows = self._project([second, first])
        self.assertEqual(len(rows), 2)
        self.assertEqual(
            [
                (
                    row["series"]["run_index"],
                    row["series"]["attempt"],
                    row["series"]["outcome"],
                )
                for row in rows
            ],
            [(7, 1, "diverged"), (7, 2, "passed")],
        )
        self.assertNotEqual(rows[0]["event_id"], rows[1]["event_id"])
        self.assertEqual(
            rows[0]["series"]["coordinates"]["first_divergent_record"],
            1_718,
        )
        self.assertEqual(
            rows[0]["series"]["first_divergent_messages"],
            {"left": "inbound read", "right": "finish read"},
        )
        self.assertEqual(
            [row["series"]["attempt"] for row in series.history_rows(rows, self._cell(0)["test"])],
            [1, 2],
        )

    def test_pressure_attempt_must_be_positive(self):
        cell = self._cell(7)
        cell["attempt"] = 0
        with self.assertRaisesRegex(
            series.MalformedRow,
            "pressure-test cell result attempt must be a positive int",
        ):
            self._project([cell])

    def test_validate_requires_the_appended_attempt_ordinal(self):
        cell = self._cell(0)
        with self.assertRaisesRegex(
            series.MalformedRow,
            "validate cell result attempt must be a positive int",
        ):
            series.rows_from_cell_results(
                [cell],
                parent=self.parent,
                producer="validate",
                run_id="validate-one",
                tree=self.tree,
                emitted_at="2026-08-26T10:00:00Z",
            )

    def test_cell_outcome_mapping_is_closed(self):
        cases = []
        for raw, result, failure_class, projected in (
            ("PASS", "pass", None, "passed"),
            ("FAIL", "determinism-failure", "product_failure", "diverged"),
            ("FAIL", "crash-error", "product_failure", "errored"),
            ("FAIL", "timeout", "no_result", "timeout"),
            ("FAIL", "oom", "no_result", "no_result"),
            ("ERROR", None, "understood_infrastructure_failure", "errored"),
            ("ERROR", None, "understood_prerequisite_failure", "skipped"),
            ("ERROR", None, "no_result", "no_result"),
        ):
            cell = self._cell(0, outcome=raw)
            cell["result"] = result
            cell["failure_class"] = failure_class
            cases.append((cell, projected))
        for mode in ("naked", "custom"):
            for cell, projected in cases:
                with self.subTest(mode=mode, outcome=cell["outcome"]):
                    cell = dict(cell)
                    cell["mode"] = mode
                    if mode == "naked":
                        cell["backend"] = None
                    rows = self._project([cell])
                    self.assertEqual(rows[0]["series"]["outcome"], projected)
        self.assertEqual(
            self._project([self._cell(0, outcome="HOST-INAPPLICABLE")]),
            [],
        )
        with self.assertRaises(series.MalformedRow):
            self._project([self._cell(0, outcome="SOMETHING_NEW")])

    def test_cell_outcome_refuses_unknown_or_missing_mode(self):
        for mode in ("unknown", None):
            for outcome in ("PASS", "FAIL"):
                with self.subTest(mode=mode, outcome=outcome):
                    cell = self._cell(0, outcome=outcome)
                    if mode is None:
                        del cell["mode"]
                    else:
                        cell["mode"] = mode
                    with self.assertRaisesRegex(series.MalformedRow, "mode"):
                        series._cell_outcome(cell)
                    with self.assertRaisesRegex(series.MalformedRow, "mode"):
                        self._project([cell])

    def test_comparison_cell_outcome_validates_all_attempts_and_uses_framework_result(self):
        matched = self._attempt("matched")
        timed_out = self._attempt("no_result", timed_out=True, index="2")
        diverged = self._attempt(
            "diverged",
            record=98,
            syscall=37,
            scheduler_turn=4,
            virtual_nanoseconds=80_000,
            index="3",
        )

        for attempts in ([matched, timed_out], [timed_out, matched]):
            cell = self._cell(0, outcome="FAIL", duration_ms=30_453)
            cell["attempts"] = attempts
            cell["result"] = "timeout"
            cell["failure_class"] = "no_result"
            row = self._project([cell])[0]["series"]
            self.assertEqual(row["outcome"], "timeout")
            self.assertNotIn("coordinates", row)

        timeout_without_report = self._attempt("no_result", timed_out=True, index="2")
        del timeout_without_report["verification_report"]
        del timeout_without_report["verification_report_sha256"]
        cell = self._cell(0, outcome="ERROR", duration_ms=30_453)
        cell["attempts"] = [matched, timeout_without_report]
        cell["result"] = None
        cell["failure_class"] = "no_result"
        self.assertEqual(
            self._project([cell])[0]["series"]["outcome"],
            "no_result",
        )

        for attempts in ([matched, diverged], [diverged, timed_out]):
            cell = self._cell(0, outcome="FAIL")
            cell["attempts"] = attempts
            row = self._project([cell])[0]["series"]
            self.assertEqual(row["outcome"], "diverged")

        passing = self._cell(0, outcome="PASS")
        self.assertEqual(self._project([passing])[0]["series"]["outcome"], "passed")

        replay = self._cell(0, outcome="PASS")
        replay["mode"] = "replay"
        report = json.loads(replay["attempts"][0]["verification_report"])
        report["comparison"]["virtualize_time"] = False
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        replay["attempts"][0]["verification_report"] = raw
        replay["attempts"][0]["verification_report_sha256"] = hashlib.sha256(
            raw.encode()
        ).hexdigest()
        self.assertEqual(self._project([replay])[0]["series"]["outcome"], "passed")

        for mode, field, value in (
            ("replay", "virtualize_time", True),
            ("verify", "virtualize_time", False),
            ("chaos", "virtualize_time", False),
            ("verify", "display_name", "weakened-display"),
        ):
            with self.subTest(mode=mode, field=field, value=value):
                noncanonical_pass = self._cell(0, outcome="PASS")
                noncanonical_pass["mode"] = mode
                report = json.loads(
                    noncanonical_pass["attempts"][0]["verification_report"]
                )
                report["comparison"][field] = value
                raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
                noncanonical_pass["attempts"][0]["verification_report"] = raw
                noncanonical_pass["attempts"][0][
                    "verification_report_sha256"
                ] = hashlib.sha256(raw.encode()).hexdigest()
                with self.assertRaisesRegex(
                    series.MalformedRow,
                    "no supporting canonical typed attempt",
                ):
                    self._project([noncanonical_pass])

        chaos_nonzero = self._cell(0, outcome="PASS")
        chaos_nonzero["mode"] = "chaos"
        chaos_nonzero["attempts"][0]["status"] = 7
        self.assertEqual(
            self._project([chaos_nonzero])[0]["series"]["outcome"],
            "passed",
        )

        dbt = self._cell(0, outcome="PASS")
        report = json.loads(dbt["attempts"][0]["verification_report"])
        report["comparison"]["record_envelope"] = "dbt_evidence_transport_v1"
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        dbt["attempts"][0]["verification_report"] = raw
        dbt["attempts"][0]["verification_report_sha256"] = hashlib.sha256(
            raw.encode()
        ).hexdigest()
        self.assertEqual(self._project([dbt])[0]["series"]["outcome"], "passed")

        all_match_fail = self._cell(0, outcome="FAIL")
        all_match_fail["result"] = "crash-error"
        all_match_fail["failure_class"] = "product_failure"
        with self.assertRaisesRegex(
            series.MalformedRow,
            "no supporting canonical typed attempt",
        ):
            self._project([all_match_fail])

        crash_before_comparison = self._cell(0, outcome="FAIL")
        crash_before_comparison["attempts"] = [self._attempt("no_result")]
        crash_before_comparison["result"] = "crash-error"
        crash_before_comparison["failure_class"] = "product_failure"
        self.assertEqual(
            self._project([crash_before_comparison])[0]["series"]["outcome"],
            "errored",
        )

        mixed_no_result = self._cell(0, outcome="ERROR")
        mixed_no_result["attempts"] = [matched, self._attempt("no_result", index="2")]
        mixed_no_result["result"] = None
        mixed_no_result["failure_class"] = "no_result"
        self.assertEqual(
            self._project([mixed_no_result])[0]["series"]["outcome"],
            "no_result",
        )

        mixed_error = self._cell(0, outcome="ERROR")
        mixed_error["attempts"] = [matched, self._attempt("infrastructure_error", index="2")]
        mixed_error["result"] = None
        mixed_error["failure_class"] = "understood_infrastructure_failure"
        self.assertEqual(
            self._project([mixed_error])[0]["series"]["outcome"],
            "errored",
        )

    def test_prelaunch_timeout_cell_preserves_the_failed_producer_result(self):
        for mode in ("verify", "replay", "chaos"):
            for error_kind in ("cpu-timeout", "wall-timeout"):
                with self.subTest(mode=mode, error_kind=error_kind):
                    cell = self._cell(0, outcome="ERROR")
                    cell.update({
                        "mode": mode,
                        "result": "timeout",
                        "failure_class": "no_result",
                        "error_kind": error_kind,
                    })
                    attempt = self._attempt("no_result", timed_out=True)
                    attempt.update({"signal": None, "error_kind": error_kind})
                    cell["attempts"] = [attempt]
                    original = json.dumps(cell, sort_keys=True)
                    self.assertEqual(series._cell_outcome(cell), "timeout")
                    evidence = series._no_verdict_evidence(cell, "timeout")
                    self.assertEqual(evidence["attempts"], [{
                        "index": "1",
                        "kind": "not_run",
                        "attempt_outcome": "ERROR",
                        "disposition": "timeout",
                        "error_kind": error_kind,
                        "status": None,
                        "signal": None,
                        "timed_out": True,
                        "verification_report_sha256": attempt[
                            "verification_report_sha256"
                        ],
                    }])
                    self.assertEqual(json.dumps(cell, sort_keys=True), original)
                    for mutation in (
                        {"outcome": "PASS"},
                        {"outcome": "HOST-INAPPLICABLE"},
                        {"failure_class": "product_failure"},
                        {"result": "pass", "failure_class": None},
                    ):
                        with self.subTest(mutation=mutation):
                            with self.assertRaises(series.MalformedRow):
                                series._cell_outcome({**cell, **mutation})

    def test_launched_timeout_without_a_report_keeps_its_process_disposition(self):
        for error_kind in ("cpu-timeout", "wall-timeout"):
            for status, signal in ((124, None), (None, 15)):
                with self.subTest(error_kind=error_kind, status=status, signal=signal):
                    cell = self._cell(0, outcome="ERROR")
                    cell.update({
                        "result": "timeout",
                        "failure_class": "no_result",
                        "error_kind": error_kind,
                    })
                    attempt = self._attempt("no_result", timed_out=True)
                    attempt.update({
                        "error_kind": error_kind,
                        "status": status,
                        "signal": signal,
                        "verification_report": None,
                        "verification_report_sha256": None,
                    })
                    cell["attempts"] = [attempt]
                    self.assertEqual(series._cell_outcome(cell), "timeout")
                    retained = series._no_verdict_evidence(cell, "timeout")["attempts"][0]
                    self.assertEqual(retained["kind"], "missing_report_timeout")
                    self.assertEqual(retained["attempt_outcome"], "ERROR")
                    self.assertEqual(retained["disposition"], "timeout")
                    self.assertEqual((retained["status"], retained["signal"]), (status, signal))
                    self.assertIsNone(retained["verification_report_sha256"])
                    attempt.update({"status": None, "signal": None})
                    with self.assertRaisesRegex(series.MalformedRow, "did not time out"):
                        series._cell_outcome(cell)

    def test_prelaunch_timeout_accepts_only_the_recorded_error_kinds(self):
        for error_kind in (
            "incomplete-verification-evidence", "cpu-timeout", "wall-timeout",
        ):
            attempt = self._attempt("no_result", timed_out=True)
            attempt.update({"signal": None, "error_kind": error_kind})
            with self.subTest(error_kind=error_kind):
                self.assertEqual(
                    series._typed_comparison_attempt(attempt, mode="verify", index=1),
                    "timeout",
                )

        for error_kind in ("cpu-timeout", "wall-timeout"):
            for label, mutation in (
                ("not timed out", {"timed_out": False}),
                ("wrong attempt outcome", {"outcome": "FAIL"}),
                ("unrelated cause", {"error_kind": "infrastructure"}),
                ("missing cause", {"error_kind": None}),
                ("array cause", {"error_kind": ["cpu-timeout"]}),
                ("object cause", {"error_kind": {"kind": "cpu-timeout"}}),
                ("zero exit status", {"status": 0}),
                ("negative exit status", {"status": -1}),
                ("zero signal", {"signal": 0}),
                ("negative signal", {"signal": -9}),
                ("wrong report identity", {"verification_report_sha256": "0" * 64}),
                ("missing report", {"verification_report": None}),
            ):
                attempt = self._attempt("no_result", timed_out=True)
                attempt.update({"signal": None, "error_kind": error_kind})
                attempt.update(mutation)
                with self.subTest(error_kind=error_kind, label=label):
                    with self.assertRaises(series.MalformedRow):
                        series._typed_comparison_attempt(attempt, mode="verify", index=1)

    def test_comparison_cell_outcome_refuses_malformed_or_reidentified_reports(self):
        malformed = self._cell(0)
        malformed["attempts"][0]["verification_report"] = "{"
        malformed["attempts"][0]["verification_report_sha256"] = hashlib.sha256(
            b"{"
        ).hexdigest()
        with self.assertRaisesRegex(series.MalformedRow, "report is malformed"):
            self._project([malformed])

        reidentified = self._cell(0)
        reidentified["attempts"][0]["verification_report_sha256"] = "0" * 64
        with self.assertRaisesRegex(series.MalformedRow, "report hash mismatch"):
            self._project([reidentified])

        orphaned_hash = self._cell(0, outcome="ERROR")
        orphaned_hash["attempts"] = [self._attempt("no_result", timed_out=True)]
        del orphaned_hash["attempts"][0]["verification_report"]
        with self.assertRaisesRegex(series.MalformedRow, "hash without a report"):
            self._project([orphaned_hash])

        negative_unreported_status = self._cell(0, outcome="ERROR")
        negative_unreported_status["attempts"] = [
            self._attempt("no_result", timed_out=True)
        ]
        attempt = negative_unreported_status["attempts"][0]
        del attempt["verification_report"]
        del attempt["verification_report_sha256"]
        attempt.update({"status": -1, "signal": None})
        with self.assertRaisesRegex(series.MalformedRow, "did not time out"):
            self._project([negative_unreported_status])

        incomplete = self._cell(0)
        report = json.loads(incomplete["attempts"][0]["verification_report"])
        del report["comparison"]["compare_io_buffers"]
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        incomplete["attempts"][0]["verification_report"] = raw
        incomplete["attempts"][0]["verification_report_sha256"] = hashlib.sha256(
            raw.encode()
        ).hexdigest()
        with self.assertRaisesRegex(series.MalformedRow, "comparison omitted"):
            self._project([incomplete])

        contradictory = self._cell(0, outcome="ERROR")
        contradictory["attempts"] = [self._attempt("no_result", timed_out=True)]
        report = json.loads(contradictory["attempts"][0]["verification_report"])
        report["first_divergent_record"] = 1
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        contradictory["attempts"][0]["verification_report"] = raw
        contradictory["attempts"][0]["verification_report_sha256"] = hashlib.sha256(
            raw.encode()
        ).hexdigest()
        with self.assertRaisesRegex(series.MalformedRow, "internally inconsistent"):
            self._project([contradictory])

        contradictory_match = self._cell(0)
        contradictory_match["attempts"][0].update({
            "outcome": "ERROR",
            "error_kind": "incomplete-verification-evidence",
            "status": None,
            "signal": 15,
            "timed_out": True,
        })
        with self.assertRaisesRegex(series.MalformedRow, "internally inconsistent"):
            self._project([contradictory_match])

        nonzero_verify_match = self._cell(0)
        nonzero_verify_match["attempts"][0]["status"] = 7
        with self.assertRaisesRegex(series.MalformedRow, "internally inconsistent"):
            self._project([nonzero_verify_match])

        matched_with_no_result_reason = self._cell(0)
        attempt = matched_with_no_result_reason["attempts"][0]
        report = json.loads(attempt["verification_report"])
        report["no_result_reason"] = {"kind": "not_run"}
        raw = json.dumps(report, sort_keys=True, separators=(",", ":"))
        attempt["verification_report"] = raw
        attempt["verification_report_sha256"] = hashlib.sha256(raw.encode()).hexdigest()
        with self.assertRaisesRegex(series.MalformedRow, "internally inconsistent"):
            self._project([matched_with_no_result_reason])

        for label, mutation in (
            ("wrong outcome", {"outcome": "FAIL"}),
            ("missing error kind", {"error_kind": None}),
            ("zero signal", {"signal": 0}),
            ("negative signal", {"signal": -9}),
            ("negative status", {"status": -1, "signal": None}),
        ):
            incomplete_timeout = self._cell(0, outcome="ERROR")
            incomplete_timeout["attempts"] = [
                self._attempt("no_result", timed_out=True)
            ]
            incomplete_timeout["attempts"][0].update(mutation)
            with self.subTest(label=label), self.assertRaisesRegex(
                series.MalformedRow,
                "NotRun report has no complete process or pre-launch timeout disposition",
            ):
                self._project([incomplete_timeout])

    def test_projection_refuses_missing_or_disagreeing_framework_classification(self):
        missing = self._cell(0, outcome="FAIL")
        del missing["failure_class"]
        with self.assertRaisesRegex(series.MalformedRow, "failure_class=None"):
            self._project([missing])

        disagreeing = self._cell(0, outcome="FAIL")
        disagreeing["result"] = "pass"
        disagreeing["failure_class"] = None
        with self.assertRaisesRegex(series.MalformedRow, "outcome disagrees"):
            self._project([disagreeing])

        fixed_attempts = self._cell(0, outcome="FAIL")
        fixed_attempts["result"] = "crash-error"
        fixed_attempts["failure_class"] = "product_failure"
        with self.assertRaisesRegex(
            series.MalformedRow,
            "no supporting canonical typed attempt",
        ):
            self._project([fixed_attempts])
        fixed_attempts["result"] = "determinism-failure"
        fixed_attempts["attempts"] = [self._attempt("diverged")]
        self.assertEqual(
            self._project([fixed_attempts])[0]["series"]["outcome"],
            "diverged",
        )

    def test_projection_carries_the_recorded_machine_kernel_and_capabilities(self):
        projected = self._project([self._cell(0)])[0]
        self.assertEqual(projected["schema"], "stress-series/v3")
        self.assertEqual(projected["host"], "devbig014")
        self.assertEqual(projected["series"]["machine_shortname"], "devbig014")
        self.assertEqual(projected["series"]["kernel_version"], "7.1.3-test")
        self.assertEqual(
            projected["series"]["host_capabilities"]["cpuid-faulting"]["present"],
            False,
        )

        for missing in ("machine_shortname", "kernel_version", "host_capabilities"):
            malformed = self._cell(0)
            del malformed[missing]
            with self.assertRaises(series.MalformedRow, msg=missing):
                self._project([malformed])

    def test_append_cells_writes_nothing_for_an_unmet_prerequisite(self):
        import contextlib
        import io

        cell = self._cell(0, outcome="HOST-INAPPLICABLE")
        real_stdin = sys.stdin
        sys.stdin = io.StringIO(json.dumps(cell) + "\n")
        stderr = io.StringIO()
        try:
            with contextlib.redirect_stderr(stderr):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "unmet-prerequisite",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-27T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("no completed cell rows", stderr.getvalue())
        self.assertEqual(list(series.spool_dir(self.parent).glob("*.jsonl")), [])

    def test_projection_preserves_the_first_available_coordinate_per_axis(self):
        cell = self._cell(0, outcome="FAIL")
        cell["attempts"] = [
            self._attempt("diverged", syscall=37),
            self._attempt("diverged", record=98, syscall=41, index="2"),
        ]
        rows = self._project([cell])
        self.assertEqual(
            rows[0]["series"]["coordinates"],
            {"first_divergent_record": 98, "first_divergent_syscall": 37},
        )

        cell["attempts"] = [self._attempt("diverged")]
        rows = self._project([cell])
        self.assertNotIn("coordinates", rows[0]["series"])

    def test_projection_keeps_a_message_pair_from_one_attempt(self):
        cell = self._cell(0, outcome="FAIL")
        cell["attempts"] = [
            self._attempt("diverged", left="first left", right="first right"),
            self._attempt("diverged", left="second left", right="second right", index="2"),
        ]
        rows = self._project([cell])
        self.assertEqual(
            rows[0]["series"]["first_divergent_messages"],
            {"left": "first left", "right": "first right"},
        )

    def test_projection_makes_workspace_paths_in_divergence_messages_portable(self):
        owner_home = "/" + "home" + "/operator"
        workspace = owner_home + "/work/dev-hermit"
        cwd = workspace + "/worktrees/slots/pressure/ignored/pressure-fresh-one"
        cell = self._diverged_cell(
            0,
            record=98,
            syscall=37,
            scheduler_turn=4,
            virtual_nanoseconds=80_000,
            left=f'newfstatat("{cwd}", st_size=2234)',
            right=f'newfstatat("{cwd}/fixtures/program", st_size=2176)',
        )
        cell["cwd"] = cwd

        rows = self._project([cell])

        messages = rows[0]["series"]["first_divergent_messages"]
        self.assertEqual(
            messages,
            {
                "left": 'newfstatat("{{WORKSPACE_ROOT}}", st_size=2234)',
                "right": (
                    'newfstatat("{{WORKSPACE_ROOT}}/fixtures/program", '
                    'st_size=2176)'
                ),
            },
        )
        self.assertNotIn("/" + "home" + "/", json.dumps(rows))

    def test_projection_still_refuses_an_unattributed_owner_path(self):
        self._assert_divergence_message_is_refused(
            "path=" + "/" + "home" + "/someone-else/secret"
        )

    def test_projection_does_not_redact_a_workspace_sibling_prefix(self):
        owner_home = "/" + "home" + "/operator"
        workspace = owner_home + "/work/dev-hermit"
        self._assert_divergence_message_is_refused(
            f'path="{workspace}-foreign/secret"', cwd=workspace + "/hermit"
        )

    def test_projection_does_not_redact_an_owner_home_sibling_prefix(self):
        owner_home = "/" + "home" + "/operator"
        self._assert_divergence_message_is_refused(f'path="{owner_home}2/secret"')

    def test_projection_does_not_redact_cwd_inside_an_enclosing_path(self):
        owner_home = "/" + "home" + "/operator"
        cwd = owner_home + "/work/dev-hermit/hermit"
        self._assert_divergence_message_is_refused(f'path="/tmp{cwd}/secret"')

    def test_projection_does_not_redact_cwd_after_a_plain_character(self):
        owner_home = "/" + "home" + "/operator"
        cwd = owner_home + "/work/dev-hermit/hermit"
        self._assert_divergence_message_is_refused(f"path=x{cwd}/secret")

    def test_projection_does_not_treat_colon_as_a_trailing_path_boundary(self):
        self._assert_cwd_suffix_is_refused(":foreign")

    def test_projection_does_not_treat_semicolon_as_a_trailing_path_boundary(self):
        self._assert_cwd_suffix_is_refused(";foreign")

    def test_projection_does_not_treat_parenthesis_as_a_trailing_path_boundary(self):
        self._assert_cwd_suffix_is_refused(")foreign")

    def test_projection_does_not_treat_space_as_a_trailing_path_boundary(self):
        self._assert_cwd_suffix_is_refused(" foreign")

    def _assert_cwd_suffix_is_refused(self, suffix):
        owner_home = "/" + "home" + "/operator"
        cwd = owner_home + "/work/dev-hermit/hermit"
        self._assert_divergence_message_is_refused(f"path={cwd}{suffix}", cwd=cwd)

    def _assert_divergence_message_is_refused(self, message, *, cwd=None):
        owner_home = "/" + "home" + "/operator"
        cwd = cwd or owner_home + "/work/dev-hermit/hermit"
        cell = self._diverged_cell(
            0,
            record=98,
            syscall=37,
            scheduler_turn=4,
            virtual_nanoseconds=80_000,
            left="inbound read",
            right=message,
        )
        cell["cwd"] = cwd

        with self.assertRaisesRegex(series.MalformedRow, "workspace-local owner path"):
            self._project([cell])

    def test_append_cells_uses_the_supplied_run_id_for_the_whole_batch(self):
        import contextlib
        import io

        stdin = io.StringIO(
            "\n".join(json.dumps(self._cell(i)) for i in range(2)) + "\n"
        )
        stdout = io.StringIO()
        real_stdin = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-26T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("representing 2 run(s)", stdout.getvalue())
        files = list(series.spool_dir(self.parent).glob("*.jsonl"))
        self.assertEqual([path.name for path in files], ["pressure-one.jsonl"])
        stored = series.union(files)
        self.assertEqual(len(stored), 2)
        self.assertEqual({item["run_id"] for item in stored}, {"pressure-one"})
        self.assertCountEqual([item["series"]["run_index"] for item in stored], [0, 1])
        self.assertTrue(all(item["series"]["num_runs"] == 1 for item in stored))
        self.assertTrue(all(item["series"]["pressure_evidence"] for item in stored))

        stdin = io.StringIO(
            "\n".join(json.dumps(self._cell(i)) for i in range(2)) + "\n"
        )
        stdout = io.StringIO()
        real_stdin = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-27T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("appended 0 row(s) representing 2 run(s)", stdout.getvalue())
        self.assertEqual(len(files[0].read_text().splitlines()), 2)
        self.assertEqual(series.union(files), stored)

        tracked = self.parent / "series/hermit/host/2026-08.jsonl"
        tracked.parent.mkdir(parents=True)
        files[0].replace(tracked)
        stdin = io.StringIO(
            "\n".join(json.dumps(self._cell(i)) for i in range(2)) + "\n"
        )
        stdout = io.StringIO()
        real_stdin = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-28T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("appended 0 row(s) representing 2 run(s)", stdout.getvalue())
        self.assertFalse(files[0].exists())
        self.assertEqual(len(tracked.read_text().splitlines()), 2)
        self.assertEqual(series.union([tracked]), stored)

    def test_append_cells_retains_typed_pressure_rows_when_one_cell_is_untyped(self):
        import contextlib
        import io

        thread_output = self._cell(0)
        thread_output["test"] = "applications/thread-output"
        thread_output["backend"] = "kvm"
        thread_output_retry = self._cell(0)
        thread_output_retry["test"] = "applications/thread-output"
        thread_output_retry["backend"] = "kvm"
        thread_output_retry["attempt"] = 2
        subprocess_time = self._cell(0)
        subprocess_time["test"] = "applications/perl-io-subprocess-time"
        subprocess_time["backend"] = "kvm"
        untyped = self._cell(0, outcome="ERROR")
        untyped["test"] = "applications/invalid-without-typed-attempts"
        untyped["backend"] = "kvm"
        untyped["attempts"] = []
        cells = [thread_output, untyped, thread_output_retry, subprocess_time]

        with self.assertRaisesRegex(
            series.MalformedRow, "comparison cell result has no typed attempts"
        ):
            self._project(cells)
        self.assertEqual(list(series.spool_dir(self.parent).glob("*.jsonl")), [])

        def append(emitted_at):
            real_stdin = sys.stdin
            sys.stdin = io.StringIO(
                "\n".join(json.dumps(cell) for cell in cells) + "\n"
            )
            stdout = io.StringIO()
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    rc = series.main([
                        "append-cells",
                        "--parent", str(self.parent),
                        "--producer", "pressure-test",
                        "--run-id", "pressure-mixed-evidence",
                        "--tree", self.tree,
                        "--emitted-at", emitted_at,
                    ])
            finally:
                sys.stdin = real_stdin
            return rc, stdout.getvalue(), stderr.getvalue()

        rc, stdout, stderr = append("2026-08-26T10:00:00Z")
        self.assertEqual(rc, 1)
        self.assertIn("appended 3 row(s) representing 3 run(s)", stdout)
        self.assertIn("REJECTED non-qualifying cell", stderr)
        self.assertIn('"input_row":2', stderr)
        self.assertIn('"run_index":0', stderr)
        self.assertIn('"campaign_run_id":"pressure-mixed-evidence"', stderr)
        self.assertIn(f'"tree":"{self.tree}"', stderr)
        self.assertIn(
            '"test":"applications/invalid-without-typed-attempts"', stderr
        )
        self.assertIn("comparison cell result has no typed attempts", stderr)
        self.assertRegex(stderr, r'"cell_result_sha256":"[0-9a-f]{64}"')
        self.assertIn("retained 3 valid series row(s)", stderr)

        target = series.spool_dir(self.parent) / "pressure-mixed-evidence.jsonl"
        first_bytes = target.read_bytes()
        stored = series.union([target])
        self.assertEqual(len(stored), 3)
        self.assertEqual(
            {row["series"]["cell"] for row in stored},
            {
                "applications/thread-output/verify/kvm",
                "applications/perl-io-subprocess-time/verify/kvm",
            },
        )
        self.assertEqual(len({row["event_id"] for row in stored}), 3)
        thread_rows = [
            row for row in stored
            if row["series"]["cell"] == "applications/thread-output/verify/kvm"
        ]
        self.assertEqual({row["series"]["attempt"] for row in thread_rows}, {1, 2})
        self.assertTrue(all(row["run_id"] == "pressure-mixed-evidence" for row in stored))
        self.assertTrue(all(row["series"]["tree"] == self.tree for row in stored))
        self.assertTrue(all(row["series"]["run_index"] == 0 for row in stored))
        for row in stored:
            payload = row["series"]
            identity = {
                "run_id": "pressure-mixed-evidence",
                "producer": "pressure-test",
                "cell": payload["cell"],
                "tree": self.tree,
                "run_index": payload["run_index"],
            }
            if "attempt" in payload:
                identity["attempt"] = payload["attempt"]
            expected = "series-" + hashlib.sha256(json.dumps(
                identity, sort_keys=True, separators=(",", ":")
            ).encode()).hexdigest()
            self.assertEqual(row["event_id"], expected)
        self.assertEqual(sum(row["series"]["num_runs"] for row in stored), 3)
        self.assertNotIn("invalid-without-typed-attempts", json.dumps(stored))
        series.validate_read_rows(stored)

        rc, stdout, stderr = append("2026-08-27T10:00:00Z")
        self.assertEqual(rc, 1)
        self.assertIn("appended 0 row(s) representing 3 run(s)", stdout)
        self.assertIn("REJECTED non-qualifying cell", stderr)
        self.assertEqual(target.read_bytes(), first_bytes)

    def test_append_cells_rejects_an_all_untyped_pressure_batch_without_a_spool(self):
        import contextlib
        import io

        untyped = self._cell(0, outcome="ERROR")
        untyped["attempts"] = []
        real_stdin = sys.stdin
        sys.stdin = io.StringIO(json.dumps(untyped) + "\n")
        stderr = io.StringIO()
        try:
            with contextlib.redirect_stderr(stderr):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-untyped-only",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-26T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 1)
        self.assertIn("no valid completed cell rows were available", stderr.getvalue())
        self.assertEqual(list(series.spool_dir(self.parent).glob("*.jsonl")), [])

        # The narrow recovery must not hide another malformed provenance field
        # on the same untyped result or partially append its valid sibling.
        untyped["detcore_tree"] = "not-a-sha"
        valid = self._cell(0)
        valid["test"] = "applications/thread-output"
        real_stdin = sys.stdin
        sys.stdin = io.StringIO(
            json.dumps(valid) + "\n" + json.dumps(untyped) + "\n"
        )
        stderr = io.StringIO()
        try:
            with contextlib.redirect_stderr(stderr):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-untyped-and-malformed",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-26T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 1)
        self.assertIn("detcore_tree must be a 40-hex object id", stderr.getvalue())
        self.assertNotIn("REJECTED non-qualifying cell", stderr.getvalue())
        self.assertEqual(list(series.spool_dir(self.parent).glob("*.jsonl")), [])

    def test_append_cells_uses_the_producing_checkout_for_an_off_main_tree(self):
        import contextlib
        import io

        producer = self.parent / "producer-checkout"
        producer.mkdir()
        subprocess.run(["git", "-C", str(producer), "init", "-q", "-b", "main"], check=True)
        subprocess.run(
            ["git", "-C", str(producer), "config", "user.name", "Series Test"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(producer), "config", "user.email", "series@example.invalid"],
            check=True,
        )
        (producer / "base").write_text("base\n")
        subprocess.run(["git", "-C", str(producer), "add", "base"], check=True)
        subprocess.run(
            ["git", "-C", str(producer), "commit", "-qm", "base"], check=True,
        )
        base = subprocess.run(
            ["git", "-C", str(producer), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        subprocess.run(
            ["git", "-C", str(producer), "update-ref", "refs/remotes/origin/main", base],
            check=True,
        )
        (producer / "feature").write_text("feature\n")
        subprocess.run(["git", "-C", str(producer), "add", "feature"], check=True)
        subprocess.run(
            ["git", "-C", str(producer), "commit", "-qm", "feature"], check=True,
        )
        feature = subprocess.run(
            ["git", "-C", str(producer), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        missing = subprocess.run(
            ["git", "-C", str(self.repo), "cat-file", "-e", f"{feature}^{{commit}}"],
            check=False, capture_output=True, text=True,
        )
        self.assertNotEqual(missing.returncode, 0)

        cell = self._cell(0)
        cell["hermit_sha"] = feature
        real_stdin = sys.stdin
        sys.stdin = io.StringIO(json.dumps(cell) + "\n")
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--checkout", str(producer),
                    "--producer", "pressure-test",
                    "--run-id", "off-main-feature",
                    "--tree", feature,
                    "--emitted-at", "2026-08-27T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        stored = series.union(sorted(series.spool_dir(self.parent).glob("*.jsonl")))
        self.assertEqual(len(stored), 1)
        self.assertFalse(stored[0]["series"]["main_ancestry"])
        self.assertEqual(stored[0]["series"]["depth"]["hermit"]["commits"], 2)

    def test_append_cells_refuses_a_changed_replay_before_writing(self):
        import contextlib
        import io

        def append(cell):
            real_stdin = sys.stdin
            sys.stdin = io.StringIO(json.dumps(cell) + "\n")
            try:
                with contextlib.redirect_stderr(io.StringIO()):
                    return series.main([
                        "append-cells",
                        "--parent", str(self.parent),
                        "--producer", "pressure-test",
                        "--run-id", "pressure-one",
                        "--tree", self.tree,
                        "--emitted-at", "2026-08-26T10:00:00Z",
                    ])
            finally:
                sys.stdin = real_stdin

        self.assertEqual(append(self._cell(0, duration_ms=100)), 0)
        target = series.spool_dir(self.parent) / "pressure-one.jsonl"
        before = target.read_bytes()
        self.assertEqual(append(self._cell(0, duration_ms=101)), 1)
        self.assertEqual(target.read_bytes(), before)

    def test_append_cells_refuses_duplicate_run_indices_before_writing(self):
        import contextlib
        import io

        real_stdin = sys.stdin
        duplicate = self._validate_cell(1)
        sys.stdin = io.StringIO(json.dumps(duplicate) + "\n" + json.dumps(duplicate) + "\n")
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "validate",
                    "--run-id", "validate-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-26T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 1)
        self.assertFalse((series.spool_dir(self.parent) / "validate-one.jsonl").exists())

    def test_an_empty_submodule_directory_does_not_resolve_to_the_parent_repo(self):
        empty = self.repo / "reverie"
        empty.mkdir()
        self.assertIsNone(series.commit_depth(empty, self.tree) if series._is_checkout(empty) else None)
        self.assertEqual(series.main_ancestry(empty, [self.tree]), {self.tree: (None, None)})


class HistoryCli(unittest.TestCase):
    TEST = "applications/git-repository-workflow"

    def _git(self, repo, *args):
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def _publish_parent(self, parent: Path) -> None:
        bare = parent.parent / "parent-origin.git"
        subprocess.run(
            ["git", "init", "-q", "--bare", "-b", "main", str(bare)], check=True,
        )
        self._git(parent, "init", "-q", "-b", "main")
        self._git(parent, "config", "user.name", "Series Test")
        self._git(parent, "config", "user.email", "series@example.invalid")
        self._git(parent, "remote", "add", "origin", str(bare))
        (parent / "README").write_text("parent\n")
        self._git(parent, "add", "-A")
        self._git(parent, "commit", "-qm", "series")
        self._git(parent, "push", "-q", "origin", "main")

    def test_one_command_reads_tracked_and_live_rows_for_one_exact_test(self):
        import contextlib
        import io
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            parent = root / "parent"
            parent.mkdir()
            hermit_bare = root / "hermit-origin.git"
            subprocess.run(
                ["git", "init", "-q", "--bare", "-b", "main", str(hermit_bare)],
                check=True,
            )
            repo = parent / "hermit"
            repo.mkdir()
            self._git(repo, "init", "-q", "-b", "main")
            self._git(repo, "config", "user.name", "Series Test")
            self._git(repo, "config", "user.email", "series@example.invalid")
            (repo / "one").write_text("one\n")
            self._git(repo, "add", "one")
            self._git(repo, "commit", "-qm", "one")
            first_tree = self._git(repo, "rev-parse", "HEAD")
            self._git(repo, "remote", "add", "origin", str(hermit_bare))
            self._git(repo, "push", "-q", "-u", "origin", "main")

            writer = root / "hermit-writer"
            subprocess.run(["git", "clone", "-q", str(hermit_bare), str(writer)], check=True)
            self._git(writer, "config", "user.name", "Series Test")
            self._git(writer, "config", "user.email", "series@example.invalid")
            (writer / "two").write_text("two\n")
            self._git(writer, "add", "two")
            self._git(writer, "commit", "-qm", "two")
            self._git(writer, "push", "-q", "origin", "main")
            tree = self._git(writer, "rev-parse", "HEAD")
            self.assertEqual(self._git(repo, "rev-parse", "origin/main"), first_tree)
            missing = subprocess.run(
                ["git", "-C", str(repo), "cat-file", "-e", f"{tree}^{{commit}}"],
                check=False,
            )
            self.assertNotEqual(
                missing.returncode,
                0,
                "the history fixture must begin with a stale Hermit origin/main",
            )

            first = row(
                event_id="history-validate",
                emitted_at="2026-08-25T03:00:00Z",
                run_id="validate-1",
                producer="validate",
                series={
                    "cell": f"{self.TEST}/verify/ptrace",
                    "tree": tree,
                    "run_index": 0,
                    "outcome": "diverged",
                    "num_runs": 1,
                    "coordinates": {
                        "first_divergent_record": 98,
                        "first_divergent_syscall": 37,
                        "first_divergent_scheduler_turn": 4,
                        "first_divergent_virtual_nanoseconds": 55,
                    },
                    "first_divergent_messages": {
                        "left": "inbound read",
                        "right": "finish read",
                    },
                    "main_ancestry": True,
                    "depth": {"hermit": {"commits": 2, "first_parent": 2}},
                    "runtime": {
                        "run1": {
                            "scheduler_turns": 12,
                            "virtual_nanoseconds": 34,
                            "syscalls": 5,
                        },
                        "run2": {
                            "scheduler_turns": 13,
                            "virtual_nanoseconds": 35,
                            "syscalls": 6,
                        },
                        "wall_time_min_ms": 90,
                        "wall_time_max_ms": 90,
                    },
                },
            )
            second = row(
                event_id="history-pressure",
                emitted_at="2026-08-26T03:00:00Z",
                run_id="pressure-1",
                producer="pressure-test",
                series={
                    "cell": f"{self.TEST}/verify/ptrace",
                    "tree": tree,
                    "run_index": 0,
                    "outcome": "passed",
                    "num_runs": 200,
                    "main_ancestry": True,
                    "depth": {"hermit": {"commits": 2, "first_parent": 2}},
                    "runtime": {
                        "run1": {
                            "scheduler_turns": 1231,
                            "virtual_nanoseconds": 1_413_842_550,
                            "syscalls": 612,
                        },
                        "run2": {
                            "scheduler_turns": 1231,
                            "virtual_nanoseconds": 1_413_842_550,
                            "syscalls": 612,
                        },
                        "wall_time_min_ms": 5_101,
                        "wall_time_max_ms": 5_285,
                    },
                },
            )
            unrelated = row(
                event_id="history-other",
                series={"cell": "applications/other/verify/ptrace", "tree": tree},
            )
            tracked = parent / "series/hermit/host/2026-08.jsonl"
            tracked.parent.mkdir(parents=True)
            tracked.write_text(json.dumps(first) + "\n" + json.dumps(unrelated) + "\n")
            self._publish_parent(parent)
            live = series.spool_dir(parent) / "pressure-1.jsonl"
            live.parent.mkdir(parents=True)
            live.write_text(json.dumps(second) + "\n")

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "history", "--parent", str(parent), "--test", self.TEST,
                ])
            text = stdout.getvalue()
            self.assertEqual(rc, 0)
            self.assertIn(f"test={self.TEST} rows=2 runs=201", text)
            self.assertIn(
                f"cell={self.TEST}/verify/ptrace\truns=201\t"
                "wall_time_min_ms=90\twall_time_max_ms=5285",
                text,
            )
            self.assertIn("2026-08-25\t" + tree + "\tyes/yes\thermit:2/2", text)
            self.assertEqual(
                self._git(repo, "rev-parse", "origin/main"),
                tree,
                "history must refresh Hermit origin/main before labeling current ancestry",
            )
            self.assertIn(
                "\t12/13\t34/35\t5/6\t90\t98\t37\t4\t55\t"
                '{"left":"inbound read","right":"finish read"}'
                "\tvalidate\tvalidate-1",
                text,
            )
            self.assertIn("\t0-199 (200)\t1231/1231", text)
            self.assertIn(
                "\t5101-5285\t-\t-\t-\t-\t-\tpressure-test\tpressure-1",
                text,
            )
            self.assertNotIn("applications/other/verify/ptrace", text)

    def test_empty_history_is_an_explicit_nonzero_result(self):
        import contextlib
        import io
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary) / "parent"
            parent.mkdir()
            (parent / "series").mkdir()
            self._publish_parent(parent)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                rc = series.main([
                    "history", "--parent", str(parent), "--test", self.TEST,
                ])
            self.assertEqual(rc, 1, stderr.getvalue())
            self.assertIn("no runs recorded", stderr.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=2)
