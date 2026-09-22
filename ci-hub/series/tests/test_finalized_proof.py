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


if __name__ == "__main__":
    unittest.main()
