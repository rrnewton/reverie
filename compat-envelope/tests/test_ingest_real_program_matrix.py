#!/usr/bin/env python3
from __future__ import annotations

import csv
import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "ingest_real_program_matrix", HERE.parent / "ingest_real_program_matrix.py"
)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class IngestRealProgramMatrix(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="real-program-ingest-"))
        self.matrix = self.tmp / "matrix.csv"
        self.meta = self.tmp / "matrix.metadata.json"
        self.corpus = self.tmp / "corpus.json"
        self.annotations = self.tmp / "annotations.json"
        self.out = self.tmp / "scorecard.csv"
        self.schema = self.tmp / "schema.json"
        core = list(self._core())
        self.schema.write_text(
            json.dumps(
                {
                    "core": core,
                    "variants": {
                        mod.OUTPUT_VARIANT: {
                            "columns": core + list(mod.OUTPUT_EXTRA_COLUMNS)
                        }
                    },
                }
            )
        )
        self.annotations.write_text(
            json.dumps(
                {
                    "schema": mod.ANNOTATIONS_SCHEMA,
                    "programs": {
                        "echo": {
                            "classes": ["concurrency", "runtime"],
                            "concurrency_kind": "multi-threaded",
                            "evidence": "fixture measured two tasks",
                        }
                    },
                }
            )
        )
        self.corpus.write_text(
            json.dumps(
                {
                    "schema": mod.CORPUS_SCHEMA,
                    "programs": [
                        {
                            "label": "echo",
                            "argv": ["/bin/bash", "-c", "/bin/echo hi"],
                            "modes": ["strict"],
                        },
                        {
                            "label": "python",
                            "argv": ["/usr/bin/python3", "-c", "print(42)"],
                            "modes": ["strict", "sabre", "e9patch", "rr"],
                        },
                    ],
                }
            )
        )

    @staticmethod
    def _core():
        return (
            "run_id", "run_utc", "hermit_sha", "reverie_sha", "dirty", "run_mode",
            "lane", "bucket", "test_id", "test_mode", "backend", "cell_state", "outcome",
            "deterministic", "stdout_parity", "output_hash", "duration_ms", "max_rss_kb",
            "reason", "verify_compare", "bitwise_parity", "compared_log_messages", "tier",
            "legacy_parity_unqualified", "ref_output_hash", "parity_comparator", "parity_tier",
            "profile_flags", "population_id", "selected_count", "executed_count",
            "evidence_count", "comparison_tier", "exit_code_parity", "detlog_parity",
            "oracle_verdict",
        )

    def _write(
        self,
        rows,
        matrix_schema=mod.MATRIX_SCHEMA_V1,
        comparison_profile=False,
        evaluation=None,
    ):
        columns = (
            mod.MATRIX_COLUMNS
            if matrix_schema == mod.MATRIX_SCHEMA_V1
            else mod.MATRIX_V2_COLUMNS
        )
        with self.matrix.open("w", newline="") as f:
            writer = csv.DictWriter(
                f, fieldnames=columns, lineterminator="\n", extrasaction="ignore"
            )
            writer.writeheader()
            writer.writerows(rows)
        digest = hashlib.sha256(self.matrix.read_bytes()).hexdigest()
        corpus_digest = hashlib.sha256(self.corpus.read_bytes()).hexdigest()
        profile = {"front_door": list(mod.FRONT_DOOR_PROFILE)}
        if comparison_profile:
            profile["comparison"] = ["hermit", "run", "--strict", "--", "<program-argv>"]
        self.meta.write_text(
            json.dumps(
                {
                    "schema": (
                        mod.METADATA_SCHEMA_V2
                        if evaluation is not None
                        else mod.METADATA_SCHEMA_V1
                    ),
                    "matrix_schema": matrix_schema,
                    "matrix_sha256": digest,
                    "corpus_sha256": corpus_digest,
                    "run_id": "matrix-test",
                    "run_utc": "2026-08-10T01:00:00Z",
                    "hermit_sha": "a" * 40,
                    "reverie_sha": "b" * 40,
                    "dirty": False,
                    "profile_flags": profile,
                    **({"evaluation": evaluation} if evaluation is not None else {}),
                }
            )
        )

    @staticmethod
    def _row(program, backend, state, **overrides):
        row = dict.fromkeys(mod.MATRIX_V2_COLUMNS, "")
        row.update(
            program=program,
            backend=backend,
            state=state,
            duration_ms="1",
            legacy_modes=(
                "strict|sabre|e9patch|rr" if program == "python" else "strict"
            ),
        )
        if state == "PASS":
            row.update(
                verified="true", bitwise_parity="true", verdict="matched",
                compared_left="7", compared_right="7", comparison_strictness="canonical", rc="0",
            )
        elif state == "ATTEMPTED_UNQUALIFIABLE":
            row.update(
                verified="true", bitwise_parity="false", verdict="matched",
                reason="output-only comparison", rc="0",
            )
        elif state == "NOT_ATTEMPTED":
            row["reason"] = "backend unavailable: fixture"
        elif state == "FAIL":
            row.update(reason="exit 1", rc="1")
        elif state == "TIMEOUT":
            row["reason"] = "exceeded 1s"
        row.update(overrides)
        return row

    def _matrix(self):
        states = {
            "ptrace": "PASS",
            "dbt": "FAIL",
            "liteinst": "TIMEOUT",
            "sabre": "ATTEMPTED_UNQUALIFIABLE",
            "kvm": "NOT_ATTEMPTED",
            "e9patch": "PASS",
        }
        return [
            self._row(program, backend, states[backend])
            for program in ("echo", "python")
            for backend in mod.PRODUCT_BACKENDS
        ]

    def _ingest(self, rows=None):
        self._write(rows or self._matrix())
        return self._call_ingest()

    def _call_ingest(self):
        return mod.ingest(
            self.matrix,
            self.meta,
            self.corpus,
            self.annotations,
            self.out,
            self.schema,
            expected_programs=2,
        )

    def _read_out(self):
        with self.out.open(newline="") as handle:
            return list(csv.DictReader(handle))

    def test_all_states_and_both_ratio_terms_survive(self):
        lines = self._ingest()
        text = "\n".join(lines)
        self.assertIn(
            "ptrace: SOURCE_SELF_VERIFY_PASS/ATTEMPTED=2/2; ATTEMPTED/CORPUS=2/2",
            text,
        )
        self.assertIn(
            "kvm: SOURCE_SELF_VERIFY_PASS/ATTEMPTED=0/0; ATTEMPTED/CORPUS=0/2",
            text,
        )
        self.assertIn("legacy singleton coverage holes=1/2 programs; labels=echo", text)
        self.assertIn(
            "legacy mode=rr: PRESENT/CORPUS=1/2; UNEXPLAINED_ABSENT/CORPUS=1/2",
            text,
        )
        self.assertIn(
            "class=concurrency backend=ptrace: SOURCE_SELF_VERIFY_PASS/ATTEMPTED=1/1; ATTEMPTED/CORPUS=1/1",
            text,
        )
        self.assertIn(
            "harness_form=shell-wrapped backend=ptrace: "
            "SOURCE_SELF_VERIFY_PASS/ATTEMPTED=1/1; ATTEMPTED/CORPUS=1/1",
            text,
        )
        rows = self._read_out()
        self.assertEqual(len(rows), 12)
        self.assertEqual({r["bucket"] for r in rows}, {"real-programs"})
        self.assertEqual({r["selected_count"] for r in rows}, {"12"})
        self.assertEqual({r["executed_count"] for r in rows}, {"10"})
        self.assertEqual({r["evidence_count"] for r in rows}, {"0"})
        self.assertEqual(
            {r["backend"] for r in rows},
            {"ptrace", "dbi", "liteinst", "sabre", "kvm", "e9patch"},
        )
        echo = [r for r in rows if r["test_id"] == "real-programs/echo"]
        self.assertEqual({r["source_legacy_coverage"] for r in echo}, {"single-corpus"})
        self.assertEqual({r["program_classes"] for r in echo}, {"concurrency|runtime"})
        self.assertEqual({r["concurrency_kind"] for r in echo}, {"multi-threaded"})
        self.assertEqual({r["harness_form"] for r in echo}, {"shell-wrapped"})
        self.assertEqual(len({r["classification_id"] for r in rows}), 1)
        self.assertEqual(len({r["corpus_id"] for r in rows}), 1)
        receipt = rows[0]["population_id"]
        identities = sorted(
            "\t".join(r[field] for field in (
                "run_mode", "lane", "bucket", "test_id", "test_mode", "backend", "cell_state"
            ))
            for r in rows
        )
        digest = hashlib.sha256(b"scorecard-population-v2\n")
        for identity in identities:
            digest.update(identity.encode())
            digest.update(b"\n")
        self.assertEqual(receipt, f"sha256:{digest.hexdigest()}")

    def test_source_pass_is_not_laundered_into_cross_backend_green(self):
        self._ingest()
        rows = self._read_out()
        passes = [r for r in rows if r["reason"].startswith("source PASS:")]
        self.assertEqual(len(passes), 4)
        self.assertEqual({r["outcome"] for r in passes}, {"pass"})
        self.assertEqual({r["comparison_tier"] for r in passes}, {"unqualified-self-verify-only"})
        self.assertTrue(all("no ptrace cross-backend witness" in r["reason"] for r in passes))

    def test_infrastructure_error_is_not_recorded_as_product_failure(self):
        rows = self._matrix()
        dbt = next(r for r in rows if r["program"] == "echo" and r["backend"] == "dbt")
        dbt.update(
            state="FAIL",
            verdict="infrastructure_error",
            verified="false",
            bitwise_parity="false",
            reason="verification recorded 2 HERMIT_SKID_OVERSHOOT report(s)",
            rc="122",
        )
        self._ingest(rows)
        result = next(
            row
            for row in self._read_out()
            if row["test_id"] == "real-programs/echo" and row["backend"] == "dbi"
        )
        self.assertEqual(result["outcome"], "error")
        self.assertEqual(result["deterministic"], "")
        self.assertIn("HERMIT_SKID_OVERSHOOT", result["reason"])

    def test_output_earns_determinism_and_leaves_unimplemented_verdicts_blank(self):
        self._ingest()
        rows = self._read_out()
        for column in ("exit_code_parity", "detlog_parity", "oracle_verdict"):
            self.assertEqual({r[column] for r in rows}, {""})
        checked = subprocess.run(
            [str(HERE.parent / "check-determinism-earned.sh"), str(self.out)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        self.assertEqual(checked.returncode, 0, checked.stdout)
        provenance = subprocess.run(
            [sys.executable, str(HERE.parent / "check-scorecard-provenance.py"), str(self.out)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        self.assertEqual(provenance.returncode, 0, provenance.stdout)

    def test_declared_scorecard_variant_is_core_plus_classification_axes(self):
        schema = json.loads((HERE.parent / "scorecard-schema.json").read_text())
        variant = schema["variants"]["real-programs-scorecard.csv"]
        self.assertEqual(variant["emitter"], "ingest_real_program_matrix.py")
        self.assertEqual(variant["columns"], schema["core"] + list(mod.OUTPUT_EXTRA_COLUMNS))
        self.assertEqual(variant["extra_columns"], list(mod.OUTPUT_EXTRA_COLUMNS))
        self.assertTrue(variant["core_prefix_safe"])

    def test_not_attempted_requires_a_reason(self):
        rows = self._matrix()
        next(r for r in rows if r["state"] == "NOT_ATTEMPTED")["reason"] = ""
        self._write(rows)
        with self.assertRaisesRegex(mod.Refused, "NOT_ATTEMPTED has no reason"):
            self._call_ingest()

    def test_never_evaluated_head_synthesizes_not_attempted_population(self):
        evaluation = {
            "state": "NOT_ATTEMPTED",
            "reason": "no workflow runs exist for this exact head",
            "evidence": "github-actions-head-runs-v1:count=0",
            "head_sha": "a" * 40,
            "observed_utc": "2026-08-10T01:00:00Z",
        }
        self._write([], evaluation=evaluation)
        lines = self._call_ingest()
        rows = self._read_out()
        self.assertEqual(len(rows), 12)
        self.assertEqual({row["outcome"] for row in rows}, {"unavailable"})
        self.assertEqual({row["executed_count"] for row in rows}, {"0"})
        self.assertEqual({row["selected_count"] for row in rows}, {"12"})
        self.assertEqual({row["evaluation_state"] for row in rows}, {"NOT_ATTEMPTED"})
        self.assertEqual(
            {row["evaluation_reason"] for row in rows},
            {"no workflow runs exist for this exact head"},
        )
        self.assertEqual(
            {row["evaluation_evidence"] for row in rows},
            {"github-actions-head-runs-v1:count=0"},
        )
        self.assertTrue(
            all("head evaluation NOT_ATTEMPTED" in row["reason"] for row in rows)
        )
        self.assertIn(
            "ptrace: SOURCE_SELF_VERIFY_PASS/ATTEMPTED=0/0; "
            "ATTEMPTED/CORPUS=0/2",
            "\n".join(lines),
        )

    def test_evaluated_head_with_no_result_rows_is_refused(self):
        evaluation = {
            "state": "EVALUATED",
            "reason": "",
            "evidence": "github-actions-workflow-run:123",
            "head_sha": "a" * 40,
            "observed_utc": "2026-08-10T01:00:00Z",
        }
        self._write([], evaluation=evaluation)
        with self.assertRaisesRegex(mod.Refused, "matrix has zero rows"):
            self._call_ingest()

    def test_never_evaluated_receipt_binds_exact_head(self):
        evaluation = {
            "state": "NOT_ATTEMPTED",
            "reason": "no workflow runs exist for this exact head",
            "evidence": "github-actions-head-runs-v1:count=0",
            "head_sha": "c" * 40,
            "observed_utc": "2026-08-10T01:00:00Z",
        }
        self._write([], evaluation=evaluation)
        with self.assertRaisesRegex(mod.Refused, "exact Hermit head SHA"):
            self._call_ingest()

    def test_pass_requires_positive_comparison_counts(self):
        for bad in ("", "0", "-1"):
            with self.subTest(compared_left=bad):
                rows = self._matrix()
                next(r for r in rows if r["state"] == "PASS")["compared_left"] = bad
                self._write(rows)
                with self.assertRaisesRegex(mod.Refused, "PASS lacks nonzero"):
                    self._call_ingest()

    def test_failed_cell_cannot_become_not_attempted(self):
        self._ingest()
        rows = self._read_out()
        dbi = [r for r in rows if r["backend"] == "dbi"]
        self.assertEqual({r["outcome"] for r in dbi}, {"fail"})
        self.assertEqual({r["comparison_tier"] for r in dbi}, {"unqualified-self-verify-only"})

    def test_product_no_result_is_attempted_unqualifiable(self):
        rows = self._matrix()
        dbt = next(r for r in rows if r["backend"] == "dbt")
        dbt.update(
            state="ATTEMPTED_UNQUALIFIABLE",
            verified="false",
            bitwise_parity="false",
            verdict="no_result",
            reason="ran without producing a comparable result",
            rc="0",
        )
        self._ingest(rows)
        out = next(r for r in self._read_out() if r["backend"] == "dbi")
        self.assertEqual(out["outcome"], "pass")
        self.assertEqual(out["deterministic"], "")
        self.assertEqual(out["comparison_tier"], "unqualified-self-verify-only")

        dbt["rc"] = "1"
        self._write(rows)
        with self.assertRaisesRegex(mod.Refused, "verified=true or.*rc=0"):
            self._call_ingest()

    def test_missing_cartesian_cell_is_refused(self):
        rows = self._matrix()[:-1]
        self._write(rows)
        with self.assertRaisesRegex(mod.Refused, "Cartesian product"):
            self._call_ingest()

    def test_legacy_coverage_cannot_vary_by_backend(self):
        rows = self._matrix()
        next(
            row
            for row in rows
            if row["program"] == "echo" and row["backend"] == "dbt"
        )["legacy_modes"] = "strict|sabre"
        self._write(rows)
        with self.assertRaisesRegex(mod.Refused, "membership differs across backend rows"):
            self._call_ingest()

    def test_metadata_must_bind_exact_matrix_bytes(self):
        self._write(self._matrix())
        with self.matrix.open("a") as f:
            f.write("\n")
        with self.assertRaisesRegex(mod.Refused, "does not bind"):
            self._call_ingest()

    def test_metadata_must_bind_exact_corpus_bytes(self):
        self._write(self._matrix())
        self.corpus.write_text(self.corpus.read_text() + "\n")
        with self.assertRaisesRegex(mod.Refused, "corpus_sha256=.*does not bind"):
            self._call_ingest()

    def test_dirty_or_non_exact_provenance_is_refused(self):
        self._write(self._matrix())
        doc = json.loads(self.meta.read_text())
        doc["dirty"] = True
        self.meta.write_text(json.dumps(doc))
        with self.assertRaisesRegex(mod.Refused, "producer was dirty"):
            self._call_ingest()

    def test_non_front_door_profile_is_refused(self):
        self._write(self._matrix())
        doc = json.loads(self.meta.read_text())
        doc["profile_flags"] = {"front_door": ["backend-specific-test-binary"]}
        self.meta.write_text(json.dumps(doc))
        with self.assertRaisesRegex(mod.Refused, "exact shared user-facing invocation"):
            self._call_ingest()

    def test_v2_preserves_real_cross_backend_stdout_witness(self):
        rows = self._matrix()
        dbt = next(r for r in rows if r["program"] == "echo" and r["backend"] == "dbt")
        operand = "c" * 64
        dbt.update(
            state="PASS",
            verified="true",
            bitwise_parity="true",
            verdict="matched",
            compared_left="7",
            compared_right="7",
            comparison_tier="unqualified-stdout-only",
            stdout_parity="true",
            output_hash=operand,
            ref_output_hash=operand,
            parity_comparator="stdout-sha256-exact-v1",
            parity_tier="stdout-exact",
            rc="0",
        )
        for row in rows:
            if row is not dbt:
                row["comparison_tier"] = (
                    "unqualified-no-comparison"
                    if row["state"] == "NOT_ATTEMPTED"
                    else "unqualified-self-verify-only"
                )
        self._write(
            rows,
            matrix_schema=mod.MATRIX_SCHEMA_V2,
            comparison_profile=True,
        )
        self._call_ingest()
        out = next(
            row
            for row in self._read_out()
            if row["test_id"] == "real-programs/echo" and row["backend"] == "dbi"
        )
        self.assertEqual(out["comparison_tier"], "unqualified-stdout-only")
        self.assertEqual(out["stdout_parity"], "1")
        self.assertEqual(out["output_hash"], operand)
        self.assertEqual(out["ref_output_hash"], operand)
        self.assertEqual(out["evidence_count"], "1")
        self.assertIn("bound ptrace stdout witness", out["reason"])
        self.assertNotIn("no ptrace cross-backend witness", out["reason"])
        provenance = subprocess.run(
            [
                sys.executable,
                str(HERE.parent / "check-scorecard-provenance.py"),
                str(self.out),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        self.assertEqual(provenance.returncode, 0, provenance.stdout)

        dbt["stdout_parity"] = "false"
        dbt["output_hash"] = "d" * 64
        self._write(
            rows,
            matrix_schema=mod.MATRIX_SCHEMA_V2,
            comparison_profile=True,
        )
        self._call_ingest()
        diverged = next(
            row
            for row in self._read_out()
            if row["test_id"] == "real-programs/echo" and row["backend"] == "dbi"
        )
        self.assertEqual(diverged["outcome"], "fail")
        self.assertIn("diverged from ptrace", diverged["reason"])

    def test_v2_refuses_tier_label_without_bound_operands(self):
        rows = self._matrix()
        for row in rows:
            row["comparison_tier"] = (
                "unqualified-no-comparison"
                if row["state"] == "NOT_ATTEMPTED"
                else "unqualified-self-verify-only"
            )
        dbt = next(r for r in rows if r["program"] == "echo" and r["backend"] == "dbt")
        dbt["comparison_tier"] = "unqualified-stdout-only"
        self._write(
            rows,
            matrix_schema=mod.MATRIX_SCHEMA_V2,
            comparison_profile=True,
        )
        with self.assertRaisesRegex(mod.Refused, "stdout-only tier has no parity verdict"):
            self._call_ingest()

    def test_annotations_are_bound_and_cannot_name_absent_programs(self):
        doc = json.loads(self.annotations.read_text())
        doc["programs"]["not-in-corpus"] = {
            "classes": ["concurrency"],
            "concurrency_kind": "multi-process",
            "evidence": "fixture",
        }
        self.annotations.write_text(json.dumps(doc))
        self._write(self._matrix())
        with self.assertRaisesRegex(mod.Refused, "outside the matrix"):
            self._call_ingest()


if __name__ == "__main__":
    unittest.main()
