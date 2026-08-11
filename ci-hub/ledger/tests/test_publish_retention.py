#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""A proven batch is RETAINED until the producing root can read it itself.

Dropping on ancestry alone blinds the producer: publication writes through a
separate dedicated clone, so between the drop and the producing root's next sync
that root has lost the spool copy and not yet gained the published shard.
Measured 2026-08-11 on devbig014 -- a producer answered NOT-VALIDATED with 0
records for a receipt it had published seconds earlier, which is
indistinguishable from "never ran".

The safety property is NOT weakened by retention and these tests pin that too:
retention happens only where the unlink used to, which is after ancestry is
proven. Retention makes the window shorter, never the guarantee weaker.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import publisher  # noqa: E402
import validate_rows  # noqa: E402


def event(identifier: str, commit: str = "a" * 40) -> dict:
    return {
        "schema": "validate-ledger/v1",
        "event_id": identifier,
        "event_type": "run.result",
        "run_id": identifier,
        "emitted_at": "2026-08-11T00:00:00Z",
        "team": "hermit",
        "host": "hosta",
        "outcome": "pass",
        "commit": commit,
        "producer": {"source": "observed", "tool": "t", "tool_version": "5"},
    }


class RetentionTest(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self._tmp = tempfile.TemporaryDirectory()
        self.base = Path(self._tmp.name)
        self.spool = self.base / "spool"
        self.spool.mkdir()
        self.source = self.base / "source"
        (self.source / "ledger" / "hermit" / "hosta").mkdir(parents=True)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _batch(self, identifier: str) -> Path:
        return publisher.spool(self.spool, "hermit", "hosta", [event(identifier)])

    def _durable(self, *identifiers: str) -> None:
        shard = self.source / "ledger" / "hermit" / "hosta" / "2026-08.jsonl"
        with shard.open("a", encoding="utf-8") as handle:
            for identifier in identifiers:
                handle.write(json.dumps(event(identifier)) + "\n")

    def test_drain_retains_instead_of_unlinking(self) -> None:
        self._batch("E1")
        files = publisher._spool_snapshot(self.spool)[1]
        publisher._drain(files)
        self.assertFalse(publisher.spooled(self.spool), "must not stay PENDING")
        self.assertTrue(publisher.retained(self.spool), "must be RETAINED, not lost")

    def test_reader_still_sees_a_retained_receipt(self) -> None:
        """Defect 1: the producer must never blink at its own green."""
        self._batch("E1")
        publisher._drain(publisher._spool_snapshot(self.spool)[1])
        seen = {
            identifier
            for events in publisher.retained(self.spool).values()
            for identifier in (e["event_id"] for e in events)
        }
        self.assertIn("E1", seen)

    def test_reap_refuses_until_the_SOURCE_ROOT_itself_has_the_event(self) -> None:
        """The whole point: another clone having it is not good enough."""
        self._batch("E1")
        publisher._drain(publisher._spool_snapshot(self.spool)[1])

        self.assertEqual(
            publisher.reap_retained(self.spool, self.source),
            0,
            "reaped while the producing root could not read it",
        )
        self.assertTrue(publisher.retained(self.spool), "batch must survive")

        # NOT INERT: once the source root itself carries the event, it is reaped.
        self._durable("E1")
        self.assertEqual(publisher.reap_retained(self.spool, self.source), 1)
        self.assertFalse(publisher.retained(self.spool))

    def test_partially_durable_batch_is_kept_whole(self) -> None:
        publisher.spool(self.spool, "hermit", "hosta", [event("E1"), event("E2")])
        publisher._drain(publisher._spool_snapshot(self.spool)[1])
        self._durable("E1")
        self.assertEqual(
            publisher.reap_retained(self.spool, self.source),
            0,
            "a batch must not be forgotten while any of its events is missing",
        )

    def test_retained_batches_are_never_republished(self) -> None:
        self._batch("E1")
        publisher._drain(publisher._spool_snapshot(self.spool)[1])
        pending, files = publisher._spool_snapshot(self.spool)
        self.assertFalse(pending)
        self.assertFalse(files)

    def test_reader_union_includes_retained(self) -> None:
        """`validate_rows.events` is the reader boundary; it must not blink."""
        root = self.base / "root"
        spool = validate_rows.live_spool_dir(root)
        spool.mkdir(parents=True)
        publisher.spool(spool, "hermit", "hosta", [event("E9")])
        publisher._drain(publisher._spool_snapshot(spool)[1])
        identifiers = {e.get("event_id") for e in validate_rows.events(root=root)}
        self.assertIn("E9", identifiers, "retained receipt vanished from the reader")


if __name__ == "__main__":
    unittest.main(verbosity=2)
