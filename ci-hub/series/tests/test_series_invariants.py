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
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import series  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ledger"))
import publish_backlog  # noqa: E402
import publisher  # noqa: E402


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

    def test_accepts_runtime_counts_compression_and_existing_depth_shape(self):
        series.validate_row(row(series={
            "num_runs": 200,
            "main_ancestry": True,
            "depth": {
                "hermit": {"commits": 2031, "first_parent": 1957},
                "reverie": {"commits": 941, "first_parent": 925},
            },
            "runtime": {
                "run1": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
                "run2": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
                "wall_time_min_ms": 5_101,
                "wall_time_max_ms": 5_285,
            },
        }))

    def test_refuses_false_runtime_or_compression_counts(self):
        bad_payloads = [
            {"num_runs": 0},
            {"num_runs": True},
            {"main_ancestry": "yes"},
            {"runtime": {"run1": {"scheduler_turns": -1}}},
            {"runtime": {"run1": {"syscalls": True}}},
            {"runtime": {"wall_time_min_ms": 10, "wall_time_max_ms": 9}},
            {"depth": {"hermit": {"commits": 10, "first_parent": 0}}},
        ]
        for payload in bad_payloads:
            with self.assertRaises(series.MalformedRow, msg=payload):
                series.validate_row(row(series=payload))


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


class Publication(unittest.TestCase):
    def _git(self, repo, *args):
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()

    def test_a_series_run_one_parent_writes_is_readable_by_another(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bare = root / "origin.git"
            producer = root / "producer"
            publisher_clone = root / "publisher"
            reader = root / "reader"
            subprocess.run(
                ["git", "init", "-q", "--bare", "-b", "main", str(bare)],
                check=True,
            )
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(producer)], check=True,
            )
            (producer / "README").write_text("series\n")
            self._git(producer, "add", "README")
            self._git(
                producer,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            )
            self._git(producer, "remote", "add", "origin", str(bare))
            self._git(producer, "push", "-q", "origin", "main")
            subprocess.run(
                ["git", "clone", "-q", str(bare), str(publisher_clone)], check=True,
            )
            subprocess.run(["git", "clone", "-q", str(bare), str(reader)], check=True)

            test = "backend-parity-c/flock-lifecycle"
            observation = row(series={"cell": f"{test}/verify/ptrace"})
            spool = series.spool_dir(producer)
            spool.mkdir(parents=True)
            series.append_rows(spool / "campaign-1.jsonl", [observation])

            before = series.history_rows(series.union(series.store_paths(reader)), test)
            self.assertEqual(before, [], "an independent checkout must not see local spool data")
            self.assertEqual(set(publish_backlog.pending_stores(producer)), {"series"})

            report = publisher.publish(
                publisher_clone,
                spool,
                source_root=producer,
                store="series",
            )
            self.assertEqual(report["published"], 1)
            self.assertEqual(
                report["shards"], ["series/hermit/devbig014/2026-08.jsonl"]
            )
            self.assertEqual(
                len(series.history_rows(series.union(series.store_paths(producer)), test)),
                1,
                "the producer must retain visibility until it fetches the tracked shard",
            )

            reader_head = self._git(reader, "rev-parse", "HEAD")
            after = series.history_rows(series.published_rows(reader), test)
            self.assertEqual(len(after), 1)
            self.assertEqual(after[0]["event_id"], observation["event_id"])
            import contextlib
            import io

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                rc = series.main(["history", "--parent", str(reader), "--test", test])
            self.assertEqual(rc, 0)
            self.assertIn("rows=1 runs=1 source=parent-origin-main+local-spool", stdout.getvalue())
            self.assertEqual(
                self._git(reader, "rev-parse", "HEAD"),
                reader_head,
                "the query must read fetched parent main without moving the checkout",
            )

    def test_series_publication_refuses_a_row_the_series_linter_rejects(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bare = root / "origin.git"
            clone = root / "clone"
            producer = root / "producer"
            subprocess.run(
                ["git", "init", "-q", "--bare", "-b", "main", str(bare)],
                check=True,
            )
            subprocess.run(
                ["git", "init", "-q", "-b", "main", str(producer)], check=True,
            )
            (producer / "README").write_text("series\n")
            self._git(producer, "add", "README")
            self._git(
                producer,
                "-c", "user.name=t",
                "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            )
            self._git(producer, "remote", "add", "origin", str(bare))
            self._git(producer, "push", "-q", "origin", "main")
            subprocess.run(["git", "clone", "-q", str(bare), str(clone)], check=True)

            spool = series.spool_dir(producer)
            spool.mkdir(parents=True)
            invalid = row(series={"outcome": "passed", "coordinates": {
                "first_divergent_record": 1,
            }})
            (spool / "invalid.jsonl").write_text(json.dumps(invalid) + "\n")

            with self.assertRaises(publisher.PublishRefused):
                publisher.publish(clone, spool, source_root=producer, store="series")
            self.assertTrue((spool / "invalid.jsonl").exists())
            self.assertFalse((clone / "series").exists())

    def test_scheduled_sweep_passes_the_series_store_to_the_publisher(self):
        from unittest import mock

        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout='{"published": 1}\n', stderr="",
        )
        with mock.patch.object(publish_backlog.subprocess, "run", return_value=completed) as run:
            report = publish_backlog.publish_parent(
                Path("/producer"),
                Path("/publisher"),
                store="series",
                spool=Path("/producer/ignored/ci-hub/series-spool"),
                timeout=60,
                attribution="test attribution",
            )
        command = run.call_args.args[0]
        self.assertEqual(command[command.index("--store") + 1], "series")
        self.assertEqual(
            command[command.index("--spool-dir") + 1],
            "/producer/ignored/ci-hub/series-spool",
        )
        self.assertEqual(report["store"], "series")
        self.assertEqual(report["published"], 1)


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
            "event_type": "series.observation", "emitted_at": "2026-08-25T16:00:00Z",
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


class CellResultProjection(unittest.TestCase):
    SHA = ""

    def setUp(self):
        import tempfile

        self.temporary = tempfile.TemporaryDirectory()
        self.parent = Path(self.temporary.name)
        self.repo = self.parent / "hermit"
        self.repo.mkdir()
        subprocess.run(["git", "-C", str(self.repo), "init", "-q"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.name", "Series Test"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.email", "series@example.invalid"],
            check=True,
        )
        (self.repo / "one").write_text("one\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "one"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "one"], check=True,
        )
        self.tree = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        subprocess.run(
            [
                "git", "-C", str(self.repo), "update-ref",
                "refs/remotes/origin/main", self.tree,
            ],
            check=True,
        )

    def tearDown(self):
        self.temporary.cleanup()

    def test_linked_parent_worktree_uses_the_primary_checkout_spool(self):
        primary = self.parent / "dev-hermit"
        (primary / "ci-hub" / "series").mkdir(parents=True)
        (primary / "ci-hub" / "series" / "series.py").write_text(
            "# producer marker\n", encoding="utf-8"
        )
        subprocess.run(["git", "init", "-q", "-b", "main", str(primary)], check=True)
        subprocess.run(["git", "-C", str(primary), "add", "."], check=True)
        subprocess.run(
            [
                "git", "-C", str(primary),
                "-c", "user.name=t", "-c", "user.email=t@invalid",
                "commit", "-q", "-m", "seed",
            ],
            check=True,
        )
        linked = self.parent / "nested" / "slot" / "dev-hermit"
        linked.parent.mkdir(parents=True)
        subprocess.run(
            ["git", "-C", str(primary), "worktree", "add", "--detach", str(linked), "HEAD"],
            check=True,
            capture_output=True,
        )

        self.assertEqual(
            series.spool_dir(linked),
            primary / "ignored" / "ci-hub" / "series-spool",
        )

        outside = self.parent / "plain-directory"
        outside.mkdir()
        self.assertEqual(
            series.spool_dir(outside),
            outside / "ignored" / "ci-hub" / "series-spool",
        )

    def _cell(self, run_index, outcome="PASS", duration_ms=100):
        return {
            "schema": 4,
            # The raw harness uses a separate evidence id per pressure
            # repetition. append-cells must NOT use it as the batch identity.
            "run_id": f"cell-evidence-{run_index}",
            "hermit_sha": self.tree,
            "source_tree_dirty": False,
            "test": "applications/git-repository-workflow",
            "category": "applications",
            "lane": "portable",
            "mode": "verify",
            "backend": "ptrace",
            "classification": "required",
            "outcome": outcome,
            "duration_ms": duration_ms,
            "run_index": run_index,
            "runtime": {
                "run1": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
                "run2": {
                    "scheduler_turns": 1231,
                    "virtual_nanoseconds": 1_413_842_550,
                    "syscalls": 612,
                },
            },
            "attempts": [],
        }

    def _project(self, cells):
        return series.rows_from_cell_results(
            cells,
            parent=self.parent,
            producer="pressure-test",
            run_id="pressure-one",
            tree=self.tree,
            emitted_at="2026-08-26T10:00:00Z",
        )

    def _validate_cell(self, attempt, outcome="PASS", duration_ms=100):
        cell = self._cell(attempt - 1, outcome=outcome, duration_ms=duration_ms)
        cell.pop("run_index")
        cell["attempt"] = attempt
        return cell

    def test_two_hundred_raw_run_ids_become_one_batch_row(self):
        cells = [self._cell(i, duration_ms=5_100 + i) for i in range(200)]
        rows = self._project(cells)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["run_id"], "pressure-one")
        self.assertEqual(rows[0]["event_type"], "series.observation")
        self.assertEqual(rows[0]["producer"], "pressure-test")
        payload = rows[0]["series"]
        self.assertEqual(payload["run_index"], 0)
        self.assertEqual(payload["num_runs"], 200)
        self.assertEqual(payload["runtime"]["wall_time_min_ms"], 5_100)
        self.assertEqual(payload["runtime"]["wall_time_max_ms"], 5_299)
        self.assertEqual(payload["runtime"]["run1"]["syscalls"], 612)

    def test_missing_wall_time_stays_missing_but_measured_zero_stays_zero(self):
        not_run = self._cell(0, outcome="HOST-INAPPLICABLE")
        not_run.pop("duration_ms")
        not_run["runtime"] = None
        missing = self._project([not_run])[0]["series"]["runtime"]
        self.assertIsNone(missing["wall_time_min_ms"])
        self.assertIsNone(missing["wall_time_max_ms"])
        self.assertNotIn("run1", missing)
        self.assertNotIn("run2", missing)

        measured = self._cell(0, duration_ms=0)
        measured["runtime"] = None
        zero = self._project([measured])[0]["series"]["runtime"]
        self.assertEqual(zero["wall_time_min_ms"], 0)
        self.assertEqual(zero["wall_time_max_ms"], 0)

    def test_failed_retry_then_success_is_not_compressed_away(self):
        rows = series.rows_from_cell_results(
            [
                self._validate_cell(1, outcome="FAIL", duration_ms=90),
                self._validate_cell(2, outcome="PASS", duration_ms=80),
            ],
            parent=self.parent,
            producer="validate",
            run_id="validate-one",
            tree=self.tree,
            emitted_at="2026-08-26T10:00:00Z",
        )
        self.assertEqual(
            [(r["series"]["run_index"], r["series"]["outcome"]) for r in rows],
            [(1, "diverged"), (2, "passed")],
        )

    def test_pressure_retry_retains_both_attempts_at_one_repetition(self):
        first = self._cell(7, outcome="FAIL", duration_ms=90)
        first["attempt"] = 1
        second = self._cell(7, outcome="PASS", duration_ms=80)
        second["attempt"] = 2
        rows = self._project([second, first])
        self.assertEqual(len(rows), 2)
        self.assertEqual(
            [
                (
                    row["series"]["run_index"],
                    row["series"]["attempt"],
                    row["series"]["outcome"],
                )
                for row in rows
            ],
            [(7, 1, "diverged"), (7, 2, "passed")],
        )
        self.assertNotEqual(rows[0]["event_id"], rows[1]["event_id"])
        self.assertEqual(
            [row["series"]["attempt"] for row in series.history_rows(rows, self._cell(0)["test"])],
            [1, 2],
        )

    def test_pressure_attempt_must_be_positive(self):
        cell = self._cell(7)
        cell["attempt"] = 0
        with self.assertRaisesRegex(
            series.MalformedRow,
            "pressure-test cell result attempt must be a positive int",
        ):
            self._project([cell])

    def test_validate_requires_the_appended_attempt_ordinal(self):
        cell = self._cell(0)
        with self.assertRaisesRegex(
            series.MalformedRow,
            "validate cell result attempt must be a positive int",
        ):
            series.rows_from_cell_results(
                [cell],
                parent=self.parent,
                producer="validate",
                run_id="validate-one",
                tree=self.tree,
                emitted_at="2026-08-26T10:00:00Z",
            )

    def test_cell_outcome_mapping_is_closed(self):
        expected = {
            "PASS": "passed",
            "FAIL": "diverged",
            "TIMEOUT": "timeout",
            "NO_RESULT": "no_result",
            "ERROR": "errored",
            "HOST-INAPPLICABLE": "skipped",
        }
        for raw, projected in expected.items():
            rows = self._project([self._cell(0, outcome=raw)])
            self.assertEqual(rows[0]["series"]["outcome"], projected)
        with self.assertRaises(series.MalformedRow):
            self._project([self._cell(0, outcome="SOMETHING_NEW")])

    def test_projection_preserves_the_first_available_coordinate_per_axis(self):
        cell = self._cell(0, outcome="FAIL")
        cell["attempts"] = [
            {
                "first_divergent_record": None,
                "first_divergent_syscall": 37,
            },
            {
                "first_divergent_record": 98,
                "first_divergent_syscall": 41,
            },
        ]
        rows = self._project([cell])
        self.assertEqual(
            rows[0]["series"]["coordinates"],
            {"first_divergent_record": 98, "first_divergent_syscall": 37},
        )

        cell["attempts"] = []
        rows = self._project([cell])
        self.assertNotIn("coordinates", rows[0]["series"])

    def test_append_cells_uses_the_supplied_run_id_for_the_whole_batch(self):
        import contextlib
        import io

        stdin = io.StringIO(
            "\n".join(json.dumps(self._cell(i)) for i in range(2)) + "\n"
        )
        stdout = io.StringIO()
        real_stdin = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-26T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("representing 2 run(s)", stdout.getvalue())
        files = list(series.spool_dir(self.parent).glob("*.jsonl"))
        self.assertEqual([path.name for path in files], ["pressure-one.jsonl"])
        stored = series.union(files)
        self.assertEqual(len(stored), 1)
        self.assertEqual(stored[0]["series"]["num_runs"], 2)

        stdin = io.StringIO(
            "\n".join(json.dumps(self._cell(i)) for i in range(2)) + "\n"
        )
        stdout = io.StringIO()
        real_stdin = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-27T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("appended 0 row(s) representing 2 run(s)", stdout.getvalue())
        self.assertEqual(len(files[0].read_text().splitlines()), 1)

        tracked = self.parent / "series/hermit/host/2026-08.jsonl"
        tracked.parent.mkdir(parents=True)
        files[0].replace(tracked)
        stdin = io.StringIO(
            "\n".join(json.dumps(self._cell(i)) for i in range(2)) + "\n"
        )
        stdout = io.StringIO()
        real_stdin = sys.stdin
        sys.stdin = stdin
        try:
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "pressure-test",
                    "--run-id", "pressure-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-28T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 0)
        self.assertIn("appended 0 row(s) representing 2 run(s)", stdout.getvalue())
        self.assertFalse(files[0].exists())
        self.assertEqual(len(tracked.read_text().splitlines()), 1)

    def test_append_cells_refuses_a_changed_replay_before_writing(self):
        import contextlib
        import io

        def append(cell):
            real_stdin = sys.stdin
            sys.stdin = io.StringIO(json.dumps(cell) + "\n")
            try:
                with contextlib.redirect_stderr(io.StringIO()):
                    return series.main([
                        "append-cells",
                        "--parent", str(self.parent),
                        "--producer", "pressure-test",
                        "--run-id", "pressure-one",
                        "--tree", self.tree,
                        "--emitted-at", "2026-08-26T10:00:00Z",
                    ])
            finally:
                sys.stdin = real_stdin

        self.assertEqual(append(self._cell(0, duration_ms=100)), 0)
        target = series.spool_dir(self.parent) / "pressure-one.jsonl"
        before = target.read_bytes()
        self.assertEqual(append(self._cell(0, duration_ms=101)), 1)
        self.assertEqual(target.read_bytes(), before)

    def test_append_cells_refuses_duplicate_run_indices_before_writing(self):
        import contextlib
        import io

        real_stdin = sys.stdin
        duplicate = self._validate_cell(1)
        sys.stdin = io.StringIO(json.dumps(duplicate) + "\n" + json.dumps(duplicate) + "\n")
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                rc = series.main([
                    "append-cells",
                    "--parent", str(self.parent),
                    "--producer", "validate",
                    "--run-id", "validate-one",
                    "--tree", self.tree,
                    "--emitted-at", "2026-08-26T10:00:00Z",
                ])
        finally:
            sys.stdin = real_stdin
        self.assertEqual(rc, 1)
        self.assertFalse((series.spool_dir(self.parent) / "validate-one.jsonl").exists())

    def test_an_empty_submodule_directory_does_not_resolve_to_the_parent_repo(self):
        empty = self.repo / "reverie"
        empty.mkdir()
        self.assertIsNone(series.commit_depth(empty, self.tree) if series._is_checkout(empty) else None)
        self.assertEqual(series.main_ancestry(empty, [self.tree]), {self.tree: (None, None)})


class HistoryCli(unittest.TestCase):
    TEST = "applications/git-repository-workflow"

    def _git(self, repo, *args):
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def _publish_parent(self, parent: Path) -> None:
        bare = parent.parent / "parent-origin.git"
        subprocess.run(
            ["git", "init", "-q", "--bare", "-b", "main", str(bare)], check=True,
        )
        self._git(parent, "init", "-q", "-b", "main")
        self._git(parent, "config", "user.name", "Series Test")
        self._git(parent, "config", "user.email", "series@example.invalid")
        self._git(parent, "remote", "add", "origin", str(bare))
        (parent / "README").write_text("parent\n")
        self._git(parent, "add", "-A")
        self._git(parent, "commit", "-qm", "series")
        self._git(parent, "push", "-q", "origin", "main")

    def test_one_command_reads_tracked_and_live_rows_for_one_exact_test(self):
        import contextlib
        import io
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary) / "parent"
            parent.mkdir()
            repo = parent / "hermit"
            repo.mkdir()
            self._git(repo, "init", "-q")
            self._git(repo, "config", "user.name", "Series Test")
            self._git(repo, "config", "user.email", "series@example.invalid")
            (repo / "one").write_text("one\n")
            self._git(repo, "add", "one")
            self._git(repo, "commit", "-qm", "one")
            tree = self._git(repo, "rev-parse", "HEAD")
            self._git(repo, "update-ref", "refs/remotes/origin/main", tree)

            first = row(
                event_id="history-validate",
                emitted_at="2026-08-25T03:00:00Z",
                run_id="validate-1",
                producer={"tool": "validate"},
                series={
                    "cell": f"{self.TEST}/verify/ptrace",
                    "tree": tree,
                    "run_index": 0,
                    "outcome": "diverged",
                    "num_runs": 1,
                    "main_ancestry": True,
                    "depth": {"hermit": {"commits": 1, "first_parent": 1}},
                    "runtime": {
                        "run1": {
                            "scheduler_turns": 12,
                            "virtual_nanoseconds": 34,
                            "syscalls": 5,
                        },
                        "run2": {
                            "scheduler_turns": 13,
                            "virtual_nanoseconds": 35,
                            "syscalls": 6,
                        },
                        "wall_time_min_ms": 90,
                        "wall_time_max_ms": 90,
                    },
                },
            )
            second = row(
                event_id="history-pressure",
                emitted_at="2026-08-26T03:00:00Z",
                run_id="pressure-1",
                producer={"tool": "pressure-test"},
                series={
                    "cell": f"{self.TEST}/verify/ptrace",
                    "tree": tree,
                    "run_index": 0,
                    "outcome": "passed",
                    "num_runs": 200,
                    "main_ancestry": True,
                    "depth": {"hermit": {"commits": 1, "first_parent": 1}},
                    "runtime": {
                        "run1": {
                            "scheduler_turns": 1231,
                            "virtual_nanoseconds": 1_413_842_550,
                            "syscalls": 612,
                        },
                        "run2": {
                            "scheduler_turns": 1231,
                            "virtual_nanoseconds": 1_413_842_550,
                            "syscalls": 612,
                        },
                        "wall_time_min_ms": 5_101,
                        "wall_time_max_ms": 5_285,
                    },
                },
            )
            unrelated = row(
                event_id="history-other",
                series={"cell": "applications/other/verify/ptrace", "tree": tree},
            )
            tracked = parent / "series/hermit/host/2026-08.jsonl"
            tracked.parent.mkdir(parents=True)
            tracked.write_text(json.dumps(first) + "\n" + json.dumps(unrelated) + "\n")
            self._publish_parent(parent)
            live = series.spool_dir(parent) / "pressure-1.jsonl"
            live.parent.mkdir(parents=True)
            live.write_text(json.dumps(second) + "\n")

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                rc = series.main([
                    "history", "--parent", str(parent), "--test", self.TEST,
                ])
            text = stdout.getvalue()
            self.assertEqual(rc, 0)
            self.assertIn(f"test={self.TEST} rows=2 runs=201", text)
            self.assertIn(
                f"cell={self.TEST}/verify/ptrace\truns=201\t"
                "wall_time_min_ms=90\twall_time_max_ms=5285",
                text,
            )
            self.assertIn("2026-08-25\t" + tree + "\tyes/yes\thermit:1/1", text)
            self.assertIn("\t12/13\t34/35\t5/6\t90\tvalidate\tvalidate-1", text)
            self.assertIn("\t0-199 (200)\t1231/1231", text)
            self.assertIn("\t5101-5285\tpressure-test\tpressure-1", text)
            self.assertNotIn("applications/other/verify/ptrace", text)

    def test_empty_history_is_an_explicit_nonzero_result(self):
        import contextlib
        import io
        import tempfile

        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary) / "parent"
            parent.mkdir()
            (parent / "series").mkdir()
            self._publish_parent(parent)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                rc = series.main([
                    "history", "--parent", str(parent), "--test", self.TEST,
                ])
            self.assertEqual(rc, 1, stderr.getvalue())
            self.assertIn("no runs recorded", stderr.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=2)
