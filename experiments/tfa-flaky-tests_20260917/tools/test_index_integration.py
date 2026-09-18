#!/usr/bin/env python3
"""Focused integration tests for schema-only experiment indexing."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


BUILD_INDEX = Path(__file__).resolve().parents[2] / "build-index.py"


def load_build_index():
    spec = importlib.util.spec_from_file_location("experiments_build_index", BUILD_INDEX)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {BUILD_INDEX}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SchemaOnlyIndexIntegrationTest(unittest.TestCase):
    def setUp(self) -> None:
        self.build_index = load_build_index()
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def write_experiment(self, slug: str, metadata: dict[str, str]) -> None:
        directory = self.root / slug
        directory.mkdir()
        (directory / "README.md").write_text(f"# {slug}\n", encoding="utf-8")
        (directory / "metadata.json").write_text(
            json.dumps(metadata), encoding="utf-8"
        )

    def rows(self) -> list[tuple[str, ...]]:
        with mock.patch.object(self.build_index, "ROOT", self.root):
            return self.build_index.build_rows()

    def test_schema_only_directory_is_excluded(self) -> None:
        self.write_experiment(
            "schema_contract_20260917",
            {
                "question": "How should future evidence be recorded?",
                "agent": "test",
                "status": "SCHEMA_ONLY_NO_EXPERIMENTS_RUN",
            },
        )

        self.assertEqual(self.rows(), [])

    def test_ordinary_directory_remains_indexed(self) -> None:
        self.write_experiment(
            "ordinary_measurement_20260917",
            {
                "question": "What was measured?",
                "agent": "test",
                "status": "COMPLETE",
            },
        )

        rows = self.rows()
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0][1], "ordinary_measurement_20260917")
        self.assertEqual(rows[0][2], "What was measured?")

    def test_any_record_overrides_stale_schema_only_status(self) -> None:
        fixtures = {
            "material_measurement_20260917": {"attempt_state": "MATERIAL"},
            "malformed_planned_20260917": {
                "attempt_state": "PLANNED",
                "events": [{"fabricated": True}],
            },
        }
        for slug, payload in fixtures.items():
            self.write_experiment(
                slug,
                {
                    "question": "What was measured?",
                    "agent": "test",
                    "status": "SCHEMA_ONLY_NO_EXPERIMENTS_RUN",
                },
            )
            record = self.root / slug / "services" / "example" / "target" / "experiment.json"
            record.parent.mkdir(parents=True)
            record.write_text(json.dumps(payload), encoding="utf-8")

        rows = self.rows()
        self.assertEqual({row[1] for row in rows}, set(fixtures))


if __name__ == "__main__":
    unittest.main()
