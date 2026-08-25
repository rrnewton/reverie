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

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import series  # noqa: E402


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
        },
    }
    for key, value in over.items():
        if key == "series":
            base["series"] = {**base["series"], **value}
        else:
            base[key] = value
    return base


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

    def test_refuses_each_missing_envelope_key_individually(self):
        for key in series.ENVELOPE_KEYS:
            bad = row()
            del bad[key]
            with self.assertRaises(series.MalformedRow, msg=key):
                series.validate_row(bad)

    def test_refuses_a_foreign_schema(self):
        with self.assertRaises(series.MalformedRow):
            series.validate_row(row(schema="validate-ledger/v1"))

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

    def test_refuses_an_unknown_coordinate_key(self):
        with self.assertRaises(series.MalformedRow):
            series.validate_row(row(series={
                "outcome": "diverged", "coordinates": {"first_divergent_bytes": 1},
            }))

    def test_refuses_coordinates_on_a_row_that_did_not_diverge(self):
        # THE CROSS-FIELD RULE. A passing run contributing a position would put a
        # number into a range that describes divergences.
        for outcome in ("passed", "timeout", "errored", "no_result", "skipped"):
            with self.assertRaises(series.MalformedRow, msg=outcome):
                series.validate_row(row(series={
                    "outcome": outcome,
                    "coordinates": {"first_divergent_record": 98},
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


class WriteBoundaryCli(unittest.TestCase):
    """The CLI exists so a producer can CALL `validate_row` rather than
    reimplement it in another language. These pin the properties that make
    calling it worth more than matching it."""

    SHA = "a" * 40
    CELL = "c-programs/pipecap/verify/ptrace"

    def _row(self, event_id, run_index, outcome, coordinates=None, run_id="campaign-1"):
        payload = {
            "cell": self.CELL, "tree": self.SHA,
            "run_index": run_index, "outcome": outcome,
        }
        if coordinates is not None:
            payload["coordinates"] = coordinates
        return {
            "schema": series.ENVELOPE_VERSION, "event_id": event_id,
            "event_type": "series.cell", "emitted_at": "2026-08-25T16:00:00Z",
            "team": "hermit", "host": "devbig014", "run_id": run_id,
            "producer": {"source": "observed", "tool": "pressure-test", "tool_version": "1"},
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
