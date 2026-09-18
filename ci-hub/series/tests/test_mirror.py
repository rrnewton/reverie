from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


MODULE = Path(__file__).resolve().parents[1] / "mirror.py"
SPEC = importlib.util.spec_from_file_location("series_mirror", MODULE)
assert SPEC is not None and SPEC.loader is not None
mirror = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mirror)


class SeriesMirrorTest(unittest.TestCase):
    def test_production_remote_is_bound_to_the_submodule(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            modules = root / ".gitmodules"
            modules.write_text(
                '[submodule "hermit_test_ledger"]\n'
                "\tpath = hermit_test_ledger\n"
                f"\turl = {mirror.REMOTE_URL}\n"
            )
            self.assertEqual(mirror.REMOTE_URL, mirror.submodule_remote(root))

            modules.write_text(
                '[submodule "hermit_test_ledger"]\n'
                "\tpath = hermit_test_ledger\n"
                "\turl = https://example.invalid/not-authorized.git\n"
            )
            with self.assertRaisesRegex(
                mirror.MirrorRefused, "submodule URL is absent or unexpected"
            ):
                mirror.submodule_remote(root)

    def test_scorecard_baseline_drops_preledger_observations_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ci/compat-envelope/cells.json"
            path.parent.mkdir(parents=True)
            path.write_text(
                json.dumps(
                    {
                        "schema": 8,
                        "projection": {"source": "series"},
                        "cells": [
                            {
                                "test": "one",
                                "status": "green",
                                "observations": [{"event_ids": []}],
                                "last_tested": {"hermit_sha": "a" * 40},
                            }
                        ],
                    }
                )
            )

            mirror._clear_scorecard_history(root)

            value = json.loads(path.read_text())
            self.assertEqual(8, value["schema"])
            self.assertEqual("green", value["cells"][0]["status"])
            self.assertEqual([], value["cells"][0]["observations"])
            self.assertIsNone(value["cells"][0]["last_tested"])
            self.assertIsNone(value["projection"])

    def test_complete_source_set_is_partitioned_by_recorded_shard(self) -> None:
        rows = [
            {
                "event_id": "first",
                "emitted_at": "2026-08-31T23:59:59Z",
                "team": "hermit",
                "host": "host-a",
            },
            {
                "event_id": "second",
                "emitted_at": "2026-09-01T00:00:00Z",
                "team": "hermit",
                "host": "host-b",
            },
        ]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def merge(previous: str, events: list[dict], **_kwargs: object) -> str:
                return previous + "".join(json.dumps(event) + "\n" for event in events)

            with mock.patch.object(mirror.publisher, "merge_shard", side_effect=merge):
                changed = mirror.write_series(root, rows)

            self.assertEqual(
                [
                    "series/hermit/host-a/2026-08.jsonl",
                    "series/hermit/host-b/2026-09.jsonl",
                ],
                changed,
            )
            self.assertIn(
                '"event_id": "first"',
                (root / changed[0]).read_text(),
            )
            self.assertIn(
                '"event_id": "second"',
                (root / changed[1]).read_text(),
            )

    def test_remote_only_shard_is_refused_not_deleted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            extra = root / "series/hermit/host-a/2026-07.jsonl"
            extra.parent.mkdir(parents=True)
            extra.write_text('{"event_id":"preserve-me"}\n')

            with self.assertRaisesRegex(
                mirror.MirrorRefused,
                "remote mirror has series shards absent from the canonical source",
            ):
                mirror.write_series(root, [])

            self.assertTrue(extra.exists())

    def test_cli_failure_is_named_and_nonzero(self) -> None:
        error = io.StringIO()
        with (
            mock.patch.object(
                mirror,
                "mirror",
                side_effect=mirror.MirrorRefused("remote push refused"),
            ),
            mock.patch.object(mirror, "submodule_remote", return_value=mirror.REMOTE_URL),
            contextlib.redirect_stderr(error),
        ):
            rc = mirror.main(
                [
                    "--parent",
                    "/tmp/parent",
                    "--source-checkout",
                    "/tmp/hermit",
                    "--target",
                    "a" * 40,
                ]
            )

        self.assertEqual(1, rc)
        self.assertEqual(
            "ledger mirror: FAILED: remote push refused\n",
            error.getvalue(),
        )

    def test_cli_success_reports_remote_commit_and_source_identity(self) -> None:
        report = {
            "state": "published",
            "commit": "a" * 40,
            "source_commit": "b" * 40,
            "source_tree": "c" * 40,
            "rows": 23,
        }
        output = io.StringIO()
        with (
            mock.patch.object(mirror, "mirror", return_value=report),
            mock.patch.object(mirror, "submodule_remote", return_value=mirror.REMOTE_URL),
            contextlib.redirect_stdout(output),
        ):
            rc = mirror.main(
                [
                    "--parent",
                    "/tmp/parent",
                    "--source-checkout",
                    "/tmp/hermit",
                    "--target",
                    "a" * 40,
                ]
            )

        self.assertEqual(0, rc)
        self.assertEqual(report, json.loads(output.getvalue()))


if __name__ == "__main__":
    unittest.main()
