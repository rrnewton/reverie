"""Transport opponents using exact retained operands and local Git only.

These tests do not stand in for canonical parent selection or native schema-10
verification. Those authorities remain required by the producer and consumer.
"""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

PARENT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(PARENT / "ci-hub/lib"))
SPEC = importlib.util.spec_from_file_location(
    "finalized_proof_tested", PARENT / "ci-hub/series/finalized_proof.py")
proof = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proof)


def git(root: Path, *arguments: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(root), "-c", "user.name=Fixture",
         "-c", "user.email=fixture@example.invalid", *arguments], text=True).strip()


class PortableProofTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="portable-proof-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        environment = mock.patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        for key in list(os.environ):
            if key.startswith("GIT_"):
                del os.environ[key]
        os.environ.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                          GIT_ALLOW_PROTOCOL="file")
        self.state = self.root / "state"
        self.writer = self.root / "ledger"
        self.state.mkdir()
        self.writer.mkdir()
        git(self.writer, "init", "-q")
        git(self.writer, "remote", "add", "origin", proof.LEDGER_REPOSITORY)
        fixtures = PARENT / "ci-hub/validate/tests/fixtures/schema10"
        self.row = json.loads((fixtures / "ordinary-only-row.json").read_bytes())
        self.row.update(repo="hermit", record_id="fixture-record")
        for role, reference in proof.row_references(self.row).items():
            destination = self.state / reference["path"]
            destination.parent.mkdir(parents=True, exist_ok=True)
            suffix = "plan.json" if role == "plan" else role + ".jsonl"
            destination.write_bytes((fixtures / ("ordinary-only-" + suffix)).read_bytes())
        self.artifacts, self.objects = proof.capture_artifacts(self.state, self.row)
        self.selection = {
            "row": self.row,
            "terminal": {"repository": "rrnewton/hermit", "run_id": self.row["run_id"],
                         "record_id": self.row["record_id"], "event_id": "original-event",
                         "event_sha256": "1" * 64, "hermit_commit": self.row["commit"]},
            "parent": {"repository": proof.PARENT_REPOSITORY, "commit": "2" * 40,
                       "shard": "ledger/hermit/fixture/2026-09.jsonl", "blob_oid": "3" * 40,
                       "event_line_sha256": "4" * 64},
        }
        self.snapshot = {"source": {"commit": "5" * 40, "tree": "6" * 40},
                         "rows_sha256": "7" * 64}
        self.manifest = proof.make_manifest(self.selection, self.artifacts,
                                           parent_commit="8" * 40, hermit_tool_commit="9" * 40,
                                           snapshot=self.snapshot)

    def publish(self, manifest=None, *, existing=None):
        manifest = self.manifest if manifest is None else manifest
        proof.stage_carrier(self.writer, manifest, self.objects, existing=existing)
        git(self.writer, "add", "--", "scorecard")
        git(self.writer, "commit", "-qm", "fixture carrier", "--allow-empty")
        return git(self.writer, "rev-parse", "HEAD")

    def test_exact_git_objects_restore_original_operands(self):
        commit = self.publish()
        index = proof.read_carriers(self.writer, commit)
        manifest = proof.carrier_for_row(index, self.row, self.selection["terminal"])
        self.assertEqual(self.manifest, manifest)
        objects = proof.read_artifact_objects(self.writer, commit, manifest)
        output = self.root / "projection-only-state"
        output.mkdir()
        proof.stage_artifacts(output, objects)
        proof.stage_artifacts(output, objects)
        for path, encoded in objects.items():
            self.assertEqual((self.state / path).read_bytes(), encoded)
            self.assertEqual(encoded, (output / path).read_bytes())
        self.assertFalse((output / ".git").exists())
        self.assertNotIn("row", manifest)

    def test_closed_fields_types_paths_and_original_references(self):
        for mutate in (
            lambda d: d.update(schema=2),
            lambda d: d.update(schema=True),
            lambda d: d.update(qualified=True),
            lambda d: d["artifacts"]["cells"].update(bytes=True),
            lambda d: d["artifacts"]["tests"].update(path="ignored/validate/artifacts/../foreign"),
            lambda d: d["artifacts"]["plan"].update(sha256="a" * 63),
            lambda d: d["parent"].update(repository="https://example.invalid/parent"),
            lambda d: d["terminal"].update(repository="hermit"),
        ):
            bad = copy.deepcopy(self.manifest)
            mutate(bad)
            with self.subTest(bad=bad), self.assertRaises(proof.ProofRefused):
                proof.validate_manifest(bad)
        changed = copy.deepcopy(self.row)
        changed["constructed_plan"]["sha256"] = "a" * 64
        with self.assertRaisesRegex(proof.ProofRefused, "original row"):
            proof.carrier_for_row({proof.carrier_identity(self.manifest): self.manifest},
                                  changed, self.selection["terminal"])

    def test_source_symlink_wrong_hash_and_nonregular_refuse(self):
        path = self.state / self.artifacts["plan"]["path"]
        original = path.read_bytes()
        path.write_bytes(original + b" ")
        with self.assertRaisesRegex(proof.ProofRefused, "differ"):
            proof.capture_artifacts(self.state, self.row)
        path.unlink()
        elsewhere = self.root / "original-plan"
        elsewhere.write_bytes(original)
        path.symlink_to(elsewhere)
        with self.assertRaises(proof.ProofRefused):
            proof.capture_artifacts(self.state, self.row)
        path.unlink()
        os.mkfifo(path)
        with self.assertRaisesRegex(proof.ProofRefused, "regular"):
            proof.capture_artifacts(self.state, self.row)

    def test_destination_collision_and_parent_symlink_refuse(self):
        output = self.root / "transport"
        output.mkdir()
        path = self.artifacts["plan"]["path"]
        proof.stage_artifacts(output, {path: b"original"})
        with self.assertRaisesRegex(proof.ProofRefused, "different"):
            proof.stage_artifacts(output, {path: b"changed"})
        self.assertEqual(b"original", (output / path).read_bytes())
        with self.assertRaises(proof.ProofRefused):
            proof.stage_artifacts(output, {"elsewhere/file": b"bad"})
        linked = self.root / "linked"
        linked.mkdir()
        (linked / "ignored").symlink_to(output / "ignored", target_is_directory=True)
        with self.assertRaises(OSError):
            proof.stage_artifacts(linked, {path: b"original"})

    def test_immutable_git_digest_mode_and_repository_opponents(self):
        old = self.publish()
        path = self.writer / proof.object_path(self.artifacts["plan"]["sha256"])
        original = path.read_bytes()
        path.write_bytes(bytes([original[0] ^ 1]) + original[1:])
        git(self.writer, "add", "--", "scorecard")
        git(self.writer, "commit", "-qm", "tamper fixture")
        new = git(self.writer, "rev-parse", "HEAD")
        self.assertTrue(proof.read_artifact_objects(self.writer, old, self.manifest))
        with self.assertRaisesRegex(proof.ProofRefused, "digest"):
            proof.read_artifact_objects(self.writer, new, self.manifest)
        path.unlink()
        path.symlink_to("/dev/null")
        git(self.writer, "add", "--", "scorecard")
        git(self.writer, "commit", "-qm", "symlink fixture")
        with self.assertRaisesRegex(proof.ProofRefused, "ordinary"):
            proof.read_artifact_objects(self.writer, git(self.writer, "rev-parse", "HEAD"), self.manifest)
        git(self.writer, "remote", "set-url", "origin", "https://example.invalid/foreign")
        with self.assertRaisesRegex(proof.ProofRefused, "repository"):
            proof.read_carriers(self.writer, old)

    def test_identical_retry_retains_original_provenance(self):
        self.publish()
        path = self.writer / proof.carrier_path(self.manifest)
        before = path.read_bytes()
        candidate = copy.deepcopy(self.manifest)
        candidate["parent"]["commit"] = "b" * 40
        candidate["series"]["commit"] = "c" * 40
        self.publish(candidate, existing=self.manifest)
        self.assertEqual(before, path.read_bytes())
        candidate["artifacts"]["cells"]["bytes"] += 1
        with self.assertRaisesRegex(proof.ProofRefused, "retry changes"):
            proof.stage_carrier(self.writer, candidate, self.objects, existing=self.manifest)

    def test_corrected_terminal_retained_but_conflicting_event_refused(self):
        self.publish()
        corrected = copy.deepcopy(self.manifest)
        corrected["terminal"].update(event_id="correction", event_sha256="c" * 64)
        commit = self.publish(corrected)
        index = proof.read_carriers(self.writer, commit)
        self.assertEqual(2, len(index))
        self.assertEqual(corrected, proof.carrier_for_row(index, self.row, corrected["terminal"]))
        absent = dict(corrected["terminal"], event_id="missing-newer-correction")
        self.assertIsNone(proof.carrier_for_row(index, self.row, absent))
        conflicting = copy.deepcopy(corrected)
        conflicting["terminal"]["event_sha256"] = "d" * 64
        commit = self.publish(conflicting)
        with self.assertRaisesRegex(proof.ProofRefused, "conflicting bodies"):
            proof.read_carriers(self.writer, commit)

    def test_publishability_keeps_safe_reports_exact_and_refuses_secrets(self):
        safe = {"stdout": '{"result":"safe"}', "stderr": "", "timeout": 10}
        proof.require_publishable_strings(safe)
        self.assertEqual('{"result":"safe"}', safe["stdout"])
        for value in ({"stdout": "/home/private/data"},
                      {"env": {"API_TOKEN": "fixture"}},
                      {"dag_json": '{"env":{"API_TOKEN":"fixture"}}'},
                      {"stderr": "Authorization: Bearer fixture"}):
            with self.subTest(value=value), self.assertRaises(proof.ProofRefused):
                proof.require_publishable_strings(value)

    def test_duplicate_json_and_extra_objects_refuse(self):
        with self.assertRaisesRegex(proof.ProofRefused, "duplicate"):
            proof._json(b'{"schema":1,"schema":2}')
        extra = dict(self.objects, **{proof.object_path("e" * 64): b"unused"})
        with self.assertRaisesRegex(proof.ProofRefused, "unreferenced"):
            proof.stage_carrier(self.writer, self.manifest, extra)

    def derived_fixture(self):
        # Synthetic transport operands only: real native raw verification has
        # separate coverage and is required before production emission.
        self.row.update(schema_version=5, cell_results=None, result="fail",
                        validation_complete=False)
        cell = {"lane": "portable", "category": "c-programs", "test": "fixture",
                "mode": "verify", "backend": "ptrace"}
        self.row["raw_result_input_census_v1"] = {
            "schema": 1, "run_id": self.row["run_id"], "hermit_sha": self.row["commit"],
            "files": [{"path": "empty/results.jsonl", "bytes": 0, "sha256": "a" * 64, "rows": []},
                      {"path": "portable/results.jsonl", "bytes": 42, "sha256": "b" * 64,
                       "rows": [{"line": n, "cell": cell, "attempt": n} for n in (1, 2)]}],
        }
        value = {"schema": 1, "authority": proof.DERIVED_AUTHORITY,
                 **proof.derived_row_binding(self.row), "attempts": [
                     {"input_path": "portable/results.jsonl", "input_line": n,
                      "cell": cell, "attempt": n, "source_row_sha256": str(n) * 64,
                      "outcome": "FAIL" if n == 1 else "PASS", "result": None,
                      "failure_class": None, "ordinary_verdict": {
                          "verdict": "unavailable_with_reason", "reason": "synthetic transport fixture"},
                      "backend_parity": None} for n in (1, 2)]}
        encoded = proof.canonical_bytes(value) + b"\n"
        self.artifacts, self.objects = proof.capture_derived_cells(self.row, encoded)
        self.manifest = proof.make_manifest(self.selection, self.artifacts, parent_commit="8" * 40,
                                            hermit_tool_commit="9" * 40, snapshot=self.snapshot)
        return value, encoded

    def test_derived_role_preserves_failed_terminal_and_all_census_spans(self):
        value, encoded = self.derived_fixture()
        original = copy.deepcopy(self.row)
        commit = self.publish()
        index = proof.read_carriers(self.writer, commit)
        manifest = proof.carrier_for_row(index, self.row, self.selection["terminal"])
        objects = proof.read_artifact_objects(self.writer, commit, manifest)
        self.assertEqual({proof.derived_path(value): encoded}, objects)
        self.assertEqual(value, proof.validate_derived_cells(encoded, self.row))
        self.assertEqual(original, self.row)
        self.assertEqual(["FAIL", "PASS"], [attempt["outcome"] for attempt in value["attempts"]])
        self.assertEqual({"schema", "raw_census_sha256", "history_binding_sha256", "derived_cells"},
                         set(manifest["artifacts"]))
        # Existing root rendering never enters the closed history binding.
        other_root = dict(self.row, cwd="/different/workspace/hermit", log_file="/different/log")
        self.assertEqual(proof.derived_row_binding(self.row), proof.derived_row_binding(other_root))

    def test_derived_same_census_correction_has_distinct_immutable_destination(self):
        value, encoded = self.derived_fixture()
        old_manifest = copy.deepcopy(self.manifest)
        old_objects = dict(self.objects)
        self.publish()
        corrected = dict(self.row, record_id="corrected-record", corrects=self.row["record_id"])
        newer = dict(value, **proof.derived_row_binding(corrected))
        newer_encoded = proof.canonical_bytes(newer) + b"\n"
        self.artifacts, self.objects = proof.capture_derived_cells(corrected, newer_encoded)
        selection = copy.deepcopy(self.selection)
        selection["row"] = corrected
        selection["terminal"].update(record_id="corrected-record", event_id="corrected-event", event_sha256="f" * 64)
        self.manifest = proof.make_manifest(selection, self.artifacts, parent_commit="8" * 40,
                                            hermit_tool_commit="9" * 40, snapshot=self.snapshot)
        commit = self.publish()
        self.assertNotEqual(proof.derived_path(value), proof.derived_path(newer))
        self.assertEqual(value["raw_census_sha256"], newer["raw_census_sha256"])
        self.assertEqual(encoded, proof.read_artifact_objects(self.writer, commit, old_manifest)[proof.derived_path(value)])
        self.assertEqual(newer_encoded, proof.read_artifact_objects(self.writer, commit, self.manifest)[proof.derived_path(newer)])
        output = self.root / "both-corrections"
        output.mkdir()
        proof.stage_artifacts(output, {proof.derived_path(value): encoded, proof.derived_path(newer): newer_encoded})
        for path, data in old_objects.items():
            self.assertEqual(data, (self.writer / path).read_bytes())

    def test_derived_role_refuses_missing_extra_reordered_or_changed_binding(self):
        value, _encoded = self.derived_fixture()
        for mutate in (
            lambda d: d["attempts"].pop(),
            lambda d: d["attempts"].append(copy.deepcopy(d["attempts"][0])),
            lambda d: d["attempts"].reverse(),
            lambda d: d["attempts"][0].update(input_line=3),
            lambda d: d.update(authority="raw_preimage_verified"),
            lambda d: d.update(history_binding_sha256="c" * 64),
            lambda d: d.update(raw_census_sha256="d" * 64),
            lambda d: d.update(run_id="another-run"),
            lambda d: d["attempts"][0].update(stdout="must not be a public operand"),
        ):
            bad = copy.deepcopy(value)
            mutate(bad)
            with self.subTest(bad=bad), self.assertRaises(proof.ProofRefused):
                proof.capture_derived_cells(self.row, proof.canonical_bytes(bad) + b"\n")
        for field, changed in (("result", "pass"), ("validation_complete", True),
                               ("commit", "f" * 40)):
            row = dict(self.row, **{field: changed})
            with self.subTest(field=field), self.assertRaises(proof.ProofRefused):
                proof.validate_derived_cells(proof.canonical_bytes(value) + b"\n", row)
        self.row["raw_result_input_census_v1"]["files"][0]["sha256"] = "e" * 64
        with self.assertRaises(proof.ProofRefused):
            proof.carrier_for_row({proof.carrier_identity(self.manifest): self.manifest},
                                  self.row, self.selection["terminal"])

    def test_derived_descriptor_and_publicability_refuse(self):
        value, encoded = self.derived_fixture()
        for mutate in (
            lambda d: d["artifacts"].update(schema=True),
            lambda d: d["artifacts"].update(derived_cells=None),
            lambda d: d["artifacts"]["derived_cells"].update(path="ignored/validate/artifacts/other.json"),
            lambda d: d["artifacts"].update(raw_census_sha256="z" * 64),
        ):
            bad = copy.deepcopy(self.manifest)
            mutate(bad)
            with self.subTest(bad=bad), self.assertRaises(proof.ProofRefused):
                proof.validate_manifest(bad)
        for reason in ("/home/private/file", "Authorization: Bearer fixture"):
            value["attempts"][0]["ordinary_verdict"]["reason"] = reason
            with self.assertRaises(proof.ProofRefused):
                proof.capture_derived_cells(self.row, proof.canonical_bytes(value) + b"\n")
        value["attempts"][0]["ordinary_verdict"] = {"nested": {"stdout": "safe-looking raw body"}}
        with self.assertRaisesRegex(proof.ProofRefused, "raw stream"):
            proof.capture_derived_cells(self.row, proof.canonical_bytes(value) + b"\n")
        with self.assertRaises(proof.ProofRefused):
            proof.capture_derived_cells(self.row, encoded.rstrip())


if __name__ == "__main__":
    unittest.main()
