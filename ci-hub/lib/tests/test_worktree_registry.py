#!/usr/bin/env python3

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import worktree_registry


class WorktreeRegistryTest(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
