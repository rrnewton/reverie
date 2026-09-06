#!/usr/bin/env python3
"""Lightweight mutation bracket for the standalone 0.3 acceptance checker."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "ai_docs/release-0.3-acceptance-contract.json"
MODULE_PATH = ROOT / "scripts/release-0.3-acceptance.py"
SPEC = importlib.util.spec_from_file_location("release_0_3_acceptance", MODULE_PATH)
assert SPEC and SPEC.loader
acceptance = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = acceptance
SPEC.loader.exec_module(acceptance)

PARENT = "a" * 40
HERMIT = "b" * 40
REVERIE = "c" * 40


def load_contract() -> dict:
    return json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))


class ReleaseAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.contract = load_contract()
        self.subject = acceptance.Subject(PARENT, HERMIT, REVERIE)
        self.tempdir = tempfile.TemporaryDirectory()
        self.artifacts = Path(self.tempdir.name)
        self.evidence = {
            "schema": "hermit-release-acceptance-evidence-v2",
            "verified_hermit_base_sha": acceptance.VERIFIED_BASE_SHA,
            "parent_contract_base_sha": acceptance.PARENT_CONTRACT_BASE_SHA,
            "tested_rc_sha": HERMIT,
            "tested_rc_is_current_main": True,
            "criteria": {},
        }
        for criterion in self.contract["criteria"]:
            self.evidence["criteria"][criterion["id"]] = self.positive_item(criterion)

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def write_blob(self, name: str, blob: bytes, artifact_type: str) -> dict:
        (self.artifacts / name).write_bytes(blob)
        return {
            "type": artifact_type,
            "path": name,
            "sha256": hashlib.sha256(blob).hexdigest(),
        }

    def write_json(self, name: str, value: dict) -> dict:
        blob = (json.dumps(value, sort_keys=True) + "\n").encode()
        return self.write_blob(name, blob, "raw-json")

    def generic_raw(self, criterion: dict) -> dict:
        total = max(1, criterion["denominator"].get("minimum", 1))
        passed = total
        if criterion["id"] == "AC-RUNNERUP-01":
            total, passed = 3, 2
        return {
            "producer": criterion["producer"],
            "subject": self.subject.as_dict(),
            "denominator": {"total": total, "unit": criterion["denominator"]["unit"]},
            "measure": {"passed": passed, "forbidden_count": 0},
        }

    def scorecard_raw(self, criterion: dict) -> dict:
        return {
            "producer": criterion["producer"],
            "subject": self.subject.as_dict(),
            "denominator": {"total": 2, "unit": "scorecard cells"},
            "measure": {"forbidden_count": 0},
            "fields": {
                "cells": [
                    {
                        "size_class": "SHORT",
                        "comparison_tier": "full-stdout-info-stack-heap",
                        "hermit_sha": HERMIT,
                        "reverie_sha": REVERIE,
                        "executed_tests": 1,
                        "stdout_parity": True,
                        "info_records": {"left": 8, "right": 8},
                        "stack_parity": True,
                        "heap_parity": True,
                    },
                    {
                        "size_class": "LARGE",
                        "comparison_tier": "stdout-info-stack-heap-spot-check",
                        "hermit_sha": HERMIT,
                        "reverie_sha": REVERIE,
                        "executed_tests": 1,
                        "stdout_parity": True,
                        "info_records": {"left": 13, "right": 13},
                        "spot_check_receipt": {
                            "state": "CURRENT",
                            "hermit_sha": HERMIT,
                            "reverie_sha": REVERIE,
                            "stack_parity": True,
                            "heap_parity": True,
                        },
                    },
                ]
            },
        }

    def hosted_raw(self, criterion: dict) -> dict:
        required = criterion["threshold"]["required_jobs"]
        sha = PARENT if criterion["threshold"]["subject_sha"] == "parent_sha" else HERMIT
        return {
            "producer": criterion["producer"],
            "subject": self.subject.as_dict(),
            "sha": sha,
            "state": "green",
            "required_positive_count": len(required),
            "positive_count": len(required),
            "jobs": [
                {
                    "name": name,
                    "head_sha": sha,
                    "status": "completed",
                    "conclusion": "success",
                    "run_id": 1000 + index,
                    "job_id": 2000 + index,
                }
                for index, name in enumerate(required)
            ],
        }

    def manifest(self, criterion: dict, item: dict) -> dict:
        producer = criterion["producer"]
        item["subject"] = self.subject.as_dict()
        item["authority"] = {
            "runner": producer["runner"],
            "runner_identity": self.contract["runners"][producer["runner"]]["identity"],
            "signer": producer["signer"],
            "signer_identity": self.contract["signers"][producer["signer"]]["identity"],
        }
        return item

    def positive_item(self, criterion: dict) -> dict:
        criterion_id = criterion["id"]
        if criterion["evaluator"] == "manual_validate_result":
            log = (
                "presentation only: 0 passed; CACHE HIT; "
                "FINAL_VALIDATE_STATUS: FAILED\n"
            ).encode()
            return self.manifest(criterion, {
                "producer": criterion["producer"],
                "artifact": self.write_blob(f"{criterion_id}.log", log, "raw-log"),
                "exit_artifact": self.write_blob(f"{criterion_id}.exit", b"0\n", "raw-exit"),
                "result_artifact": self.write_json(
                    f"{criterion_id}.service-result.json",
                    {
                        "schema_version": acceptance.service_result.SCHEMA_VERSION,
                        "commit": HERMIT,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "passed_tests": 2129,
                        "scorecard_writeback": {"status": "completed"},
                    },
                ),
            })
        if criterion["evaluator"] == "hosted_set":
            return self.manifest(criterion, {"artifact": self.write_json(f"{criterion_id}.json", self.hosted_raw(criterion))})
        if criterion["evaluator"] == "scorecard_tiers":
            return self.manifest(criterion, {"artifact": self.write_json(f"{criterion_id}.json", self.scorecard_raw(criterion))})
        return self.manifest(criterion, {"artifact": self.write_json(f"{criterion_id}.json", self.generic_raw(criterion))})

    def mutate_json_artifact(self, criterion_id: str, mutation) -> None:
        reference = self.evidence["criteria"][criterion_id]["artifact"]
        path = self.artifacts / reference["path"]
        value = json.loads(path.read_text())
        mutation(value)
        blob = (json.dumps(value, sort_keys=True) + "\n").encode()
        path.write_bytes(blob)
        reference["sha256"] = hashlib.sha256(blob).hexdigest()

    def mutate_log(self, mutation) -> None:
        reference = self.evidence["criteria"]["AC-STRICT-03"]["artifact"]
        path = self.artifacts / reference["path"]
        blob = mutation(path.read_text()).encode()
        path.write_bytes(blob)
        reference["sha256"] = hashlib.sha256(blob).hexdigest()

    def mutate_exit(self, blob: bytes) -> None:
        reference = self.evidence["criteria"]["AC-STRICT-03"]["exit_artifact"]
        path = self.artifacts / reference["path"]
        path.write_bytes(blob)
        reference["sha256"] = hashlib.sha256(blob).hexdigest()

    def mutate_manual_result(self, mutation) -> None:
        reference = self.evidence["criteria"]["AC-STRICT-03"]["result_artifact"]
        path = self.artifacts / reference["path"]
        value = json.loads(path.read_text())
        mutation(value)
        blob = (json.dumps(value, sort_keys=True) + "\n").encode()
        path.write_bytes(blob)
        reference["sha256"] = hashlib.sha256(blob).hexdigest()

    def evaluate(self, contract=None, evidence=None, subject=None):
        return acceptance.evaluate(
            contract or self.contract,
            evidence or self.evidence,
            subject or self.subject,
            ROOT,
            self.artifacts,
        )

    @staticmethod
    def row(report, criterion_id: str):
        return next(result for result in report.results if result.criterion["id"] == criterion_id)

    def test_positive_full_set(self) -> None:
        report = self.evaluate()
        self.assertEqual(report.exit_code, acceptance.GO)
        self.assertEqual(report.as_dict()["counts"], {"PASS": 26, "FAIL": 0, "NO_RESULT": 0, "total": 26})
        self.assertEqual(report.as_dict()["verified_hermit_base_sha"], acceptance.VERIFIED_BASE_SHA)
        self.assertEqual(report.as_dict()["parent_contract_base_sha"], acceptance.PARENT_CONTRACT_BASE_SHA)
        self.assertEqual(report.as_dict()["parent_status"], "main unverified")
        self.assertEqual(report.as_dict()["tested_rc_sha"], HERMIT)

    def test_zero_framework_tests_is_no_result(self) -> None:
        log_reference = self.evidence["criteria"]["AC-STRICT-03"]["artifact"]
        log_path = self.artifacts / log_reference["path"]
        rendered_before = log_path.read_bytes()
        self.assertEqual(self.row(self.evaluate(), "AC-STRICT-03").verdict, "PASS")
        self.mutate_manual_result(
            lambda result: result.update(executed_tests=0, passed_tests=0)
        )
        report = self.evaluate()
        self.assertEqual(log_path.read_bytes(), rendered_before)
        self.assertEqual(self.row(report, "AC-STRICT-03").verdict, "NO_RESULT")
        self.assertIn("zero", self.row(report, "AC-STRICT-03").reason)

    def test_current_manual_result_requires_an_exact_passing_count(self) -> None:
        reference = self.evidence["criteria"]["AC-STRICT-03"]["result_artifact"]
        original = json.loads((self.artifacts / reference["path"]).read_text())
        for label, mutation, expected in (
            (
                "missing",
                lambda result: result.pop("passed_tests"),
                "schema 5 expected",
            ),
            (
                "mismatch",
                lambda result: result.update(passed_tests=2128),
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(label=label):
                def reset_and_mutate(result):
                    result.clear()
                    result.update(copy.deepcopy(original))
                    mutation(result)

                self.mutate_manual_result(reset_and_mutate)
                row = self.row(self.evaluate(), "AC-STRICT-03")
                self.assertEqual("NO_RESULT", row.verdict)
                self.assertIn(expected, row.reason)

    def test_historical_manual_results_remain_readable_but_not_authoritative(self) -> None:
        reference = self.evidence["criteria"]["AC-STRICT-03"]["result_artifact"]
        original = json.loads((self.artifacts / reference["path"]).read_text())
        for version in (1, 2, 3, 4):
            with self.subTest(version=version):
                def make_historical(result):
                    result.clear()
                    result.update(copy.deepcopy(original))
                    result["schema_version"] = version
                    result.pop("detail")
                    if version < 4:
                        result.pop("passed_tests")
                    if version < 3:
                        result.pop("selection_mode")
                    if version < 2:
                        result.pop("scorecard_writeback")

                self.mutate_manual_result(make_historical)
                row = self.row(self.evaluate(), "AC-STRICT-03")
                self.assertEqual("NO_RESULT", row.verdict)
                self.assertIn("historical validation service result", row.reason)
                if version < 4:
                    self.assertIsNone(row.observed["passed_tests"])
                else:
                    self.assertEqual(2129, row.observed["passed_tests"])

    def test_could_not_run_reports_exact_detail(self) -> None:
        self.mutate_manual_result(
            lambda result: result.update(
                final_validate_status="COULD_NOT_RUN",
                detail=["launcher refused: missing KVM device"],
                exit_code=75,
                executed_nodes=0,
                executed_tests=None,
                passed_tests=None,
                scorecard_writeback=None,
            )
        )
        self.mutate_exit(b"75\n")
        row = self.row(self.evaluate(), "AC-STRICT-03")
        self.assertEqual("NO_RESULT", row.verdict)
        self.assertEqual("launcher refused: missing KVM device", row.reason)

    def test_could_not_run_without_detail_is_explicit(self) -> None:
        self.mutate_manual_result(
            lambda result: result.update(
                final_validate_status="COULD_NOT_RUN",
                detail=None,
                exit_code=75,
                executed_nodes=0,
                executed_tests=None,
                passed_tests=None,
                scorecard_writeback=None,
            )
        )
        self.mutate_exit(b"75\n")
        row = self.row(self.evaluate(), "AC-STRICT-03")
        self.assertEqual("NO_RESULT", row.verdict)
        self.assertEqual(
            "validation reported COULD_NOT_RUN without detail", row.reason
        )

    def test_partial_three_of_four_is_no_result(self) -> None:
        def mutation(raw):
            raw["jobs"].pop()
            raw["positive_count"] = 3
        self.mutate_json_artifact("AC-PRE-01", mutation)
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-PRE-01").verdict, "NO_RESULT")
        self.assertIn("3/4", self.row(report, "AC-PRE-01").reason)

    def test_rebase_sha_mismatch_invalidates_all_evidence(self) -> None:
        report = self.evaluate(subject=acceptance.Subject(PARENT, "9" * 40, REVERIE))
        self.assertEqual(report.exit_code, acceptance.NO_RESULT)
        self.assertTrue(all(result.verdict == "NO_RESULT" for result in report.results))
        self.assertIn("current main unverified", report.as_dict()["tested_rc_status"])

    def test_missing_runner_or_signer_is_deployment_defect(self) -> None:
        mutant = copy.deepcopy(self.contract)
        del mutant["runners"]["manual-sequential-validation"]
        del mutant["signers"]["raw-artifact-sha256"]
        report = self.evaluate(contract=mutant)
        self.assertEqual(report.exit_code, acceptance.DEPLOYMENT_DEFECT)
        self.assertTrue(any("runner does not exist" in error for error in report.deployment_errors))
        self.assertTrue(any("signer does not exist" in error for error in report.deployment_errors))

    def test_short_tier_requires_stack_and_heap(self) -> None:
        self.mutate_json_artifact("AC-STRICT-01", lambda raw: raw["fields"]["cells"][0].pop("heap_parity"))
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-STRICT-01").verdict, "NO_RESULT")

    def test_large_tier_requires_current_spot_check(self) -> None:
        self.mutate_json_artifact(
            "AC-STRICT-01",
            lambda raw: raw["fields"]["cells"][1]["spot_check_receipt"].update(state="STALE"),
        )
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-STRICT-01").verdict, "NO_RESULT")

    def test_each_criterion_missing(self) -> None:
        for criterion in self.contract["criteria"]:
            with self.subTest(criterion=criterion["id"]):
                mutant = copy.deepcopy(self.evidence)
                del mutant["criteria"][criterion["id"]]
                report = self.evaluate(evidence=mutant)
                self.assertEqual(self.row(report, criterion["id"]).verdict, "NO_RESULT")

    def test_wrong_raw_digest_is_no_result(self) -> None:
        self.evidence["criteria"]["AC-UX-01"]["artifact"]["sha256"] = "0" * 64
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-UX-01").verdict, "NO_RESULT")

    def test_nonexistent_raw_artifact_is_no_result(self) -> None:
        self.evidence["criteria"]["AC-UX-01"]["artifact"]["path"] = "absent.json"
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-UX-01").verdict, "NO_RESULT")

    def test_raw_json_subject_mismatch_is_no_result(self) -> None:
        self.mutate_json_artifact(
            "AC-UX-01",
            lambda raw: raw["subject"].update(hermit_sha="9" * 40),
        )
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-UX-01").verdict, "NO_RESULT")
        self.assertIn("tuple", self.row(report, "AC-UX-01").reason)

    def test_manual_manifest_subject_mismatch_is_no_result(self) -> None:
        self.evidence["criteria"]["AC-STRICT-03"]["subject"]["parent_sha"] = "9" * 40
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-STRICT-03").verdict, "NO_RESULT")

    def test_captured_validate_exit_not_tee_exit_is_enforced(self) -> None:
        self.mutate_exit(b"7\n")
        report = self.evaluate()
        row = self.row(report, "AC-STRICT-03")
        self.assertEqual(row.verdict, "NO_RESULT")
        self.assertIn("disagrees", row.reason)
        self.assertEqual(row.observed["pipeline_exit"], 7)
        self.assertEqual(row.observed["exit_code"], 0)

    def test_failed_scorecard_writeback_preserves_pass_but_is_not_release_evidence(self) -> None:
        self.mutate_exit(b"75\n")
        self.mutate_manual_result(
            lambda result: result.update(
                exit_code=75,
                scorecard_writeback={
                    "status": "failed",
                    "error": "fixture refusal",
                },
            )
        )
        report = self.evaluate()
        row = self.row(report, "AC-STRICT-03")
        self.assertEqual("NO_RESULT", row.verdict)
        self.assertEqual("PASSED", row.observed["final_validate_status"])
        self.assertEqual(75, row.observed["exit_code"])
        self.assertIn("scorecard write-back failed", row.reason)
        self.assertIn("fixture refusal", row.reason)

    def test_runner_up_half_is_derived_from_denominator(self) -> None:
        self.mutate_json_artifact(
            "AC-RUNNERUP-01",
            lambda raw: raw["measure"].update(half_denominator=999999),
        )
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-RUNNERUP-01").verdict, "PASS")

    def test_manual_zero_executed_nodes_is_no_result(self) -> None:
        self.mutate_manual_result(lambda result: result.update(executed_nodes=0))
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-STRICT-03").verdict, "NO_RESULT")

    def test_manual_non_full_selection_is_no_result(self) -> None:
        self.mutate_manual_result(
            lambda result: result.update(selection_mode="selected")
        )
        report = self.evaluate()
        row = self.row(report, "AC-STRICT-03")
        self.assertEqual(row.verdict, "NO_RESULT")
        self.assertIn("selection is not full", row.reason)

    def test_manual_typed_status_absence_is_no_result(self) -> None:
        self.mutate_manual_result(lambda result: result.pop("final_validate_status"))
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-STRICT-03").verdict, "NO_RESULT")

    def test_manual_log_status_cannot_override_typed_result(self) -> None:
        self.mutate_log(
            lambda text: "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n" + text
        )
        report = self.evaluate()
        self.assertEqual(self.row(report, "AC-STRICT-03").verdict, "PASS")


if __name__ == "__main__":
    unittest.main(verbosity=2)
