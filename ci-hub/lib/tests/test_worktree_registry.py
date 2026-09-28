#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import worktree_registry


class WorktreeRegistryTest(unittest.TestCase):
    def write_wrkslots_registry(
        self, root: Path, slots: list[dict[str, object]]
    ) -> Path:
        (root / ".wrkslots.yml").write_text(
            json.dumps({"machine": "testhost"}), encoding="utf-8"
        )
        control = root / "worktrees"
        control.mkdir()
        path = control / "ACTIVE.testhost.json"
        path.write_text(json.dumps({
            "schema": 2, "machine": "testhost", "revision": 1, "slots": slots,
        }), encoding="utf-8")
        return path

    def test_passes_exact_bytes_to_schema_checker(self) -> None:
        raw = b'{"version":3,"updated":"now","slots":{}}\n'
        completed = subprocess.CompletedProcess([], 0, b"", b"")
        with patch.object(
            worktree_registry.subprocess, "run", return_value=completed
        ) as run:
            self.assertEqual(worktree_registry.parse_registry_bytes(raw)["version"], 3)
        self.assertEqual(run.call_args.kwargs["input"], raw)
        self.assertEqual(run.call_args.args[0][-1], "--validate-state-stdin")

    def test_refusal_keeps_named_schema_error(self) -> None:
        completed = subprocess.CompletedProcess(
            [], 1, b"", b"worktree-registry: REFUSE missing field `status`\n"
        )
        with patch.object(worktree_registry.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError, "status"
            ):
                worktree_registry.parse_registry_bytes(b"{}")

    def test_checker_success_does_not_replace_the_original_document(self) -> None:
        document = {"version": 3, "updated": "now", "slots": {}, "future": [1, 2]}
        raw = json.dumps(document, separators=(",", ":")).encode()
        completed = subprocess.CompletedProcess([], 0, b"", b"")
        with patch.object(worktree_registry.subprocess, "run", return_value=completed):
            self.assertEqual(worktree_registry.parse_registry_bytes(raw), document)

    def test_owner_sidecar_results_are_attached_without_replacing_document(self) -> None:
        document = {
            "version": 3,
            "updated": "now",
            "slots": {"slot-a": {}, "slot-b": {}},
        }
        raw = json.dumps(document, separators=(",", ":")).encode()
        completed = subprocess.CompletedProcess(
            [], 0, b'{"slot-a":null,"slot-b":"field task is missing"}\n', b""
        )
        with patch.object(
            worktree_registry.subprocess, "run", return_value=completed
        ) as run:
            registry = worktree_registry.parse_registry_bytes(
                raw, include_owner_sidecars=True
            )
        self.assertEqual(registry, document)
        self.assertEqual(
            registry.owner_sidecar_errors,
            {"slot-a": None, "slot-b": "field task is missing"},
        )
        self.assertEqual(run.call_args.args[0][-1], "--owner-sidecar-errors-stdin")

    def test_malformed_owner_sidecar_results_are_refused(self) -> None:
        completed = subprocess.CompletedProcess([], 0, b'{"slot-a":7}\n', b"")
        with patch.object(worktree_registry.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError, "malformed per-slot result"
            ):
                worktree_registry.parse_registry_bytes(
                    b'{"version":3,"updated":"now","slots":{}}',
                    include_owner_sidecars=True,
                )

    def test_owner_sidecar_results_must_cover_exact_slots(self) -> None:
        completed = subprocess.CompletedProcess([], 0, b"{}\n", b"")
        with patch.object(worktree_registry.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError, "exact registry slots"
            ):
                worktree_registry.parse_registry_bytes(
                    b'{"version":3,"updated":"now","slots":{"slot-a":{}}}',
                    include_owner_sidecars=True,
                )

    def test_active_wrkslots_registry_is_keyed_by_slot(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_wrkslots_registry(root, [
                {"slot": "one", "machine": "testhost"},
                {"slot": "two", "machine": "testhost"},
            ])
            rows = worktree_registry.read_active_wrkslots_registry(root)
        self.assertEqual(set(rows), {"one", "two"})

    def test_empty_active_wrkslots_registry_refuses(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.write_wrkslots_registry(root, [])
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError, "unverifiable rather than clean"
            ):
                worktree_registry.read_active_wrkslots_registry(root)

    def test_missing_machine_binding_refuses_without_globbing(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / ".wrkslots.yml").write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError, "records no machine shard"
            ):
                worktree_registry.read_active_wrkslots_registry(root)


FIXTURES = Path(__file__).resolve().parent / "fixtures"
JSON_CONFIG = FIXTURES / "wrkslots-config.json"
CONVERTED_YAML_CONFIG = FIXTURES / "wrkslots-config.converted.yml"


class WrkslotsConfigFormTest(unittest.TestCase):
    """``.wrkslots.yml`` is read the same whether it is JSON or converted YAML.

    The YAML fixture is not hand-written: it is what agent-utils bada63e's
    ``wrkslots config convert --to yaml`` produced from a copy of the JSON
    fixture, which is the tracked file byte for byte (see the provenance file).
    A reader that only understood JSON refused it with "cannot read".
    """

    def setUp(self) -> None:
        environment = patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        os.environ.pop("WRKSLOTS_MACHINE", None)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def write_config(self, text: str) -> None:
        (self.root / ".wrkslots.yml").write_text(text, encoding="utf-8")

    def test_fixtures_are_the_recorded_bytes(self) -> None:
        provenance = json.loads(
            (FIXTURES / "wrkslots-config.provenance.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            hashlib.sha256(JSON_CONFIG.read_bytes()).hexdigest(),
            provenance["json_sha256"],
        )
        self.assertEqual(
            hashlib.sha256(CONVERTED_YAML_CONFIG.read_bytes()).hexdigest(),
            provenance["converted_yaml_sha256"],
        )
        converted = CONVERTED_YAML_CONFIG.read_text(encoding="utf-8")
        self.assertFalse(converted.lstrip().startswith("{"))
        self.assertIn("\nmachine: devbig014\n", converted)

    def test_json_form_reads_the_machine(self) -> None:
        self.write_config(JSON_CONFIG.read_text(encoding="utf-8"))
        self.assertEqual(worktree_registry.wrkslots_machine(self.root), "devbig014")

    def test_converted_yaml_form_reads_the_machine(self) -> None:
        self.write_config(CONVERTED_YAML_CONFIG.read_text(encoding="utf-8"))
        self.assertEqual(worktree_registry.wrkslots_machine(self.root), "devbig014")

    def test_both_forms_decode_to_the_same_document(self) -> None:
        self.write_config(JSON_CONFIG.read_text(encoding="utf-8"))
        from_json = worktree_registry.read_wrkslots_config(self.root)
        self.write_config(CONVERTED_YAML_CONFIG.read_text(encoding="utf-8"))
        from_yaml = worktree_registry.read_wrkslots_config(self.root)
        self.assertEqual(from_yaml, from_json)
        self.assertEqual(from_json, json.loads(JSON_CONFIG.read_text(encoding="utf-8")))
        self.assertEqual(len(from_json), 10)

    def test_converted_yaml_selects_the_active_registry(self) -> None:
        self.write_config(CONVERTED_YAML_CONFIG.read_text(encoding="utf-8"))
        control = self.root / "worktrees"
        control.mkdir()
        (control / "ACTIVE.devbig014.json").write_text(json.dumps({
            "schema": 2, "machine": "devbig014", "revision": 1,
            "slots": [{"slot": "one", "machine": "devbig014"}],
        }), encoding="utf-8")
        rows = worktree_registry.read_active_wrkslots_registry(self.root)
        self.assertEqual(set(rows), {"one"})

    def test_yaml_without_a_machine_key_refuses(self) -> None:
        converted = CONVERTED_YAML_CONFIG.read_text(encoding="utf-8")
        without = converted.replace("machine: devbig014\n", "", 1)
        self.assertNotEqual(without, converted)
        self.write_config(without)
        with self.assertRaisesRegex(
            worktree_registry.RegistrySchemaError, "records no machine shard"
        ):
            worktree_registry.wrkslots_machine(self.root)

    def test_yaml_with_a_non_string_machine_refuses(self) -> None:
        """Each value is null, an int or a list to wrkslots' reader as well."""
        for value in ("", "~", "14", "[devbig014]"):
            with self.subTest(value=value):
                self.write_config(f"schema: 2\nmachine: {value}\n")
                with self.assertRaisesRegex(
                    worktree_registry.RegistrySchemaError, "records no machine shard"
                ):
                    worktree_registry.wrkslots_machine(self.root)

    def test_yaml_11_scalars_are_a_known_difference_that_refuses(self) -> None:
        """KNOWN DIFFERENCE, pinned so it cannot flip silently: wrkslots accepts
        these and ci-hub refuses them.

        agent-utils bada63e resolves plain scalars by YAML 1.2, so a plain
        ``yes`` and a plain ``2026-09-28`` are strings to it. PyYAML resolves by
        YAML 1.1 and makes them a bool and a date, which ``wrkslots_machine``
        refuses because it demands a string. A converted file cannot contain a
        plain ``yes`` (the writer quotes YAML 1.1 words); it can contain a plain
        date-shaped string, but not as a hostname. The difference errs toward
        refusing: a shard name is never invented, only withheld.
        """
        for value in ("yes", "2026-09-28"):
            with self.subTest(value=value):
                self.write_config(f"schema: 2\nmachine: {value}\n")
                with self.assertRaisesRegex(
                    worktree_registry.RegistrySchemaError, "records no machine shard"
                ):
                    worktree_registry.wrkslots_machine(self.root)

    def test_yaml_values_pyyaml_cannot_construct_refuse_with_the_path(self) -> None:
        """PyYAML raises plain ValueError, KeyError or AttributeError while it
        builds these values. Uncaught, that escaped the RegistrySchemaError
        contract and slot_agent_parity.py exited 1 ("actionable") with a
        traceback instead of 2 ("could not be determined").

        The first case is the converter's real output with one glob added. It
        is a KNOWN DIFFERENCE: bada63e reads ``2026-13-45`` as a string and
        accepts the file (its writer emits such strings unquoted); PyYAML
        matches its date pattern and datetime refuses month 13, so ci-hub
        refuses the whole file. The tagged cases are refused by wrkslots too
        ("anchors, aliases, and tags are not supported").
        """
        converted = CONVERTED_YAML_CONFIG.read_text(encoding="utf-8")
        with_date_glob = converted.replace(
            "  - node_modules\n", "  - node_modules\n  - 2026-13-45\n", 1
        )
        self.assertNotEqual(with_date_glob, converted)
        with self.subTest(case="date-shaped glob in the converted file"):
            self.write_config(with_date_glob)
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError,
                r"cannot read .*\.wrkslots\.yml: a value could not be "
                r"constructed \(ValueError: month must be in 1\.\.12\)",
            ):
                worktree_registry.wrkslots_machine(self.root)
        # PyYAML 5.4.1 and 6.0.3 raise ValueError, ValueError, AttributeError
        # and KeyError here. Only the refusal is asserted, not which exception
        # PyYAML used, so a PyYAML that starts raising ConstructorError for
        # them still passes.
        for value in ("!!int abc", "!!float abc", "!!timestamp garbage", "!!bool maybe"):
            with self.subTest(value=value):
                self.write_config(f"schema: 2\nmachine: devbig014\nx: {value}\n")
                with self.assertRaisesRegex(
                    worktree_registry.RegistrySchemaError,
                    r"cannot read .*\.wrkslots\.yml: ",
                ):
                    worktree_registry.wrkslots_machine(self.root)

    def test_malformed_yaml_refuses_with_the_path(self) -> None:
        self.write_config("schema: 2\nmachine: [devbig014\n")
        with self.assertRaisesRegex(
            worktree_registry.RegistrySchemaError,
            r"cannot read .*\.wrkslots\.yml",
        ):
            worktree_registry.wrkslots_machine(self.root)

    def test_malformed_json_refuses_with_the_path(self) -> None:
        self.write_config('{"machine": "devbig014",')
        with self.assertRaisesRegex(
            worktree_registry.RegistrySchemaError,
            r"cannot read .*\.wrkslots\.yml",
        ):
            worktree_registry.wrkslots_machine(self.root)

    def test_duplicate_machine_keys_refuse_in_both_forms(self) -> None:
        """agent-utils bada63e refuses a duplicate key in both forms, so
        reading the last one would let ci-hub select a shard that wrkslots will
        not operate on. The pinned agent-utils 26dd3eaa keeps the last duplicate
        in JSON, so there ci-hub is the stricter reader, which errs toward
        refusing."""
        for text in (
            "machine: devbig014\nmachine: other\n",
            '{"machine": "devbig014", "machine": "other"}',
        ):
            with self.subTest(text=text):
                self.write_config(text)
                with self.assertRaisesRegex(
                    worktree_registry.RegistrySchemaError, "duplicate key 'machine'"
                ):
                    worktree_registry.wrkslots_machine(self.root)

    def test_missing_config_refuses(self) -> None:
        with self.assertRaisesRegex(
            worktree_registry.RegistrySchemaError, r"cannot read .*\.wrkslots\.yml"
        ):
            worktree_registry.wrkslots_machine(self.root)

    def test_machine_override_still_wins_over_either_form(self) -> None:
        self.write_config(CONVERTED_YAML_CONFIG.read_text(encoding="utf-8"))
        with patch.dict(os.environ, {"WRKSLOTS_MACHINE": "override"}):
            self.assertEqual(worktree_registry.wrkslots_machine(self.root), "override")

    def test_json_form_does_not_import_pyyaml(self) -> None:
        """Callers that still meet the JSON file must not start needing PyYAML."""
        self.write_config(JSON_CONFIG.read_text(encoding="utf-8"))
        with patch.dict(sys.modules, {"yaml": None}):
            self.assertEqual(
                worktree_registry.wrkslots_machine(self.root), "devbig014"
            )

    def test_yaml_form_without_pyyaml_names_the_dependency(self) -> None:
        self.write_config(CONVERTED_YAML_CONFIG.read_text(encoding="utf-8"))
        with patch.dict(sys.modules, {"yaml": None}):
            with self.assertRaisesRegex(
                worktree_registry.RegistrySchemaError, "PyYAML is not importable"
            ):
                worktree_registry.wrkslots_machine(self.root)


if __name__ == "__main__":
    unittest.main()
