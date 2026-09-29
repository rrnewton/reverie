#!/usr/bin/env python3
"""The parity ledger store (`parity_ledger.py`), exercised on a real run's rows.

The fixture `ci-hub/tests/fixtures/parity-run-1953/ledger-sources.jsonl` is the
verbatim output of hermit's `test-harness parity export` over RUN 1953's two
post-pass files (192 cells; see its `provenance.json`). Every guard here is
shown REFUSING with its message asserted, not merely passing: a validator whose
negative case is untested cannot be told apart from one that returns True.

Design: https://github.com/rrnewton/hermit/issues/3301

Run: python3 -m pytest ci-hub/series/tests/test_parity_ledger.py -q
"""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import series  # noqa: E402

parity = series.parity_ledger_module()

FIXTURE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "parity-run-1953"
RUN_ID = "validate-coord-s17-d3a0a4ae3d16-1790649970094317174-3328074-da07a1da"
HERMIT_SHA = "d3a0a4ae3d168565595ae4157b25ab8efbb1861a"
EMITTED_AT = "2026-09-28T21:40:00Z"


def fixture_sources() -> list[dict]:
    text = (FIXTURE / "ledger-sources.jsonl").read_text(encoding="utf-8")
    return [parity.loads(line, f"fixture line {number}")
            for number, line in enumerate(text.splitlines(), 1)]


def build(sources, **overrides):
    options = dict(producer="validate", run_id=RUN_ID, hermit_sha=HERMIT_SHA,
                   emitted_at=EMITTED_AT, source_tree_dirty=False, host="fixturehost")
    options.update(overrides)
    return parity.build_batch(sources, **options)


def first(sources, backend, verdict):
    for index, source in enumerate(sources):
        if source["backend"] == backend and source["verdict"] == verdict:
            return index, source
    raise AssertionError(f"fixture has no {backend} {verdict} source")


def record_missing(test_id: str = "backend-parity-c/never-ran", backend: str = "kvm") -> dict:
    """What `parity::ledger_sources` emits for a node that left no status."""
    return {
        "cell": f"{test_id}@{backend}", "test_id": test_id, "backend": backend,
        "verdict": "record-missing",
        "reason": "the parity post-pass of privileged/manifest_backend_parity_c left no "
                  "parity.status.json",
        "run_id": None, "hermit_sha": None,
        "source": {"lane": "privileged", "node": "manifest_backend_parity_c",
                   "post_pass_state": "absent", "status_sha256": None,
                   "records_sha256": None, "hermit_bin_sha256": None,
                   "scope_source": "expected-scope"},
        "record": None,
    }


class RealRunTransport(unittest.TestCase):
    def test_fixture_matches_its_provenance(self):
        provenance = json.loads((FIXTURE / "provenance.json").read_text(encoding="utf-8"))
        import hashlib
        for name, binding in provenance["files"].items():
            data = (FIXTURE / name).read_bytes()
            self.assertEqual(binding["sha256"], hashlib.sha256(data).hexdigest(), name)
            self.assertEqual(binding["bytes"], len(data), name)
        self.assertEqual(HERMIT_SHA, provenance["hermit_sha"])
        self.assertEqual(RUN_ID, provenance["validate_unit"])

    def test_all_192_rows_wrap_and_validate(self):
        sources = fixture_sources()
        self.assertEqual(192, len(sources))
        events = build(sources)
        marker, rows = parity.validate_batch(events)
        self.assertEqual(192, marker["records"])
        self.assertEqual(
            {"record-missing": 0, "unavailable": 0, "reference-missing": 0,
             "candidate-missing": 3, "inputs-not-equalized": 14, "diverged": 175,
             "matched": 0},
            marker["verdicts"])
        self.assertEqual(
            [("portable", "manifest_backend_parity_c", 190),
             ("privileged", "manifest_backend_parity_c", 2)],
            [(node["lane"], node["node"], node["records"]) for node in marker["nodes"]])
        self.assertEqual(parity.run_event_id("validate", RUN_ID), marker["event_id"])
        for row, source in zip(rows, sources):
            self.assertEqual(list(parity.ROW_FIELDS), list(row))
            self.assertEqual(source["record"], row["record"])
            self.assertEqual(
                parity.event_id("validate", RUN_ID, source["cell"],
                                source["source"]["lane"], source["source"]["node"]),
                row["event_id"])
            self.assertIsNone(row["record"]["credit"] if row["record"] else None,
                              "RUN 1953 compared unequal inputs; clean credit stays null")

    def test_spool_round_trip_is_byte_stable(self):
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw) / "spool"
            result = parity.append_parity(
                spool, fixture_sources(), producer="validate", run_id=RUN_ID,
                hermit_sha=HERMIT_SHA, emitted_at=EMITTED_AT, source_tree_dirty=False,
                host="fixturehost")
            self.assertEqual(192, result["records"])
            path = spool / result["batch"]
            marker, rows = parity.read_batch(path)
            self.assertEqual(193, len(path.read_text(encoding="utf-8").splitlines()))
            self.assertEqual(path.read_text(encoding="utf-8"),
                             parity.encode_batch([marker, *rows]))
            self.assertEqual(f"hermit__fixturehost__2026-09__{marker['event_id']}.jsonl",
                             path.name)
            self.assertEqual("parity/hermit/fixturehost/2026-09.jsonl", parity.event_shard(rows[0]))
            self.assertEqual("parity-runs/hermit/fixturehost/2026-09.jsonl",
                             parity.event_shard(marker))


class MalformedRecordsAreRefusedLoudly(unittest.TestCase):
    def refuse(self, sources, message):
        with self.assertRaises(parity.ParityLedgerError) as caught:
            build(sources)
        self.assertIn(message, str(caught.exception))
        return str(caught.exception)

    def test_envelope_verdict_that_disagrees_with_its_record(self):
        sources = fixture_sources()
        index, source = first(sources, "kvm", "diverged")
        source["verdict"] = "matched"
        self.refuse(sources,
                    f"input row {index + 1}: parity ledger source {source['cell']}: row verdict "
                    "matched disagrees with record verdict diverged")

    def test_credit_that_is_not_the_matched_prefix(self):
        sources = fixture_sources()
        index, source = first(sources, "kvm", "diverged")
        source["record"]["unequalized_credit"] = 0.5
        record = source["record"]
        self.refuse(sources,
                    f"credit Some(0.5) disagrees with matched prefix Some({record['matched_prefix']}) "
                    f"of {record['left_len']} | {record['right_len']}")

    def test_a_divergence_can_never_claim_full_credit(self):
        sources = fixture_sources()
        _, source = first(sources, "liteinst", "diverged")
        source["record"].update(matched_prefix=source["record"]["left_len"],
                                right_len=source["record"]["left_len"],
                                unequalized_credit=1.0)
        self.refuse(sources, "a divergence cannot be full credit")

    def test_unknown_field(self):
        sources = fixture_sources()
        sources[0]["record"]["bonus"] = 1
        self.refuse(sources, "parity record fields are not exactly its schema's: unknown bonus")

    def test_dbt_cannot_report_a_measured_comparison(self):
        sources = fixture_sources()
        _, source = first(sources, "dbt", "inputs-not-equalized")
        source["verdict"] = source["record"]["verdict"] = "diverged"
        self.refuse(sources, "dbt inputs cannot be equalized")

    def test_record_missing_row_that_carries_a_record(self):
        sources = fixture_sources()
        broken = record_missing()
        broken["record"] = copy.deepcopy(sources[0]["record"])
        self.refuse([broken], "a record-missing row carries no record")

    def test_row_run_id_that_disagrees_with_its_record(self):
        self.refuse([dict(fixture_sources()[0], run_id="some-other-run")],
                    'row run_id "some-other-run" disagrees with record run_id '
                    f'"{RUN_ID}"')

    def test_node_run_id_that_is_not_the_run(self):
        source = fixture_sources()[0]
        source["run_id"] = source["record"]["run_id"] = "some-other-run"
        self.refuse([source], "its node's run_id 'some-other-run' is not --run-id")

    def test_owner_path_is_refused(self):
        sources = fixture_sources()
        sources[0]["record"]["candidate_log"] = "/" + "home" + "/someone/log"
        self.refuse(sources, "contains a workspace-local owner path; the parity ledger is public")

    def test_duplicate_keys_and_non_finite_numbers(self):
        with self.assertRaisesRegex(parity.ParityLedgerError,
                                    "stdin line 1: duplicate JSON key 'a'"):
            parity.loads('{"a": 1, "a": 2}', "stdin line 1")
        with self.assertRaisesRegex(parity.ParityLedgerError, "NaN"):
            parity.loads('{"a": NaN}', "stdin line 1")

    def test_a_refused_batch_writes_nothing(self):
        sources = fixture_sources()
        sources[5]["verdict"] = "matched"
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw) / "spool"
            with self.assertRaises(parity.ParityLedgerError):
                parity.append_parity(
                    spool, sources, producer="validate", run_id=RUN_ID,
                    hermit_sha=HERMIT_SHA, emitted_at=EMITTED_AT, source_tree_dirty=False,
                    host="fixturehost")
            self.assertEqual([], list(spool.glob("*")) if spool.exists() else [])


class RecordMissingAndEmptySelection(unittest.TestCase):
    def test_record_missing_with_a_null_record_is_kept(self):
        sources = [*fixture_sources()[:3], record_missing()]
        marker, rows = parity.validate_batch(build(sources))
        self.assertEqual(1, marker["verdicts"]["record-missing"])
        missing = rows[-1]
        self.assertEqual("record-missing", missing["verdict"])
        self.assertIsNone(missing["record"])
        self.assertEqual("absent", missing["source"]["post_pass_state"])
        self.assertEqual(
            [("portable", "manifest_backend_parity_c", 3), ("privileged", "manifest_backend_parity_c", 1)],
            [(node["lane"], node["node"], node["records"]) for node in marker["nodes"]])

    def test_absent_node_can_owe_only_record_missing(self):
        source = fixture_sources()[0]
        source["source"] = dict(record_missing()["source"])
        with self.assertRaisesRegex(
                parity.ParityLedgerError,
                "a node that left no status can only owe record-missing rows, got diverged"):
            build([source])

    def test_empty_selection_is_a_marker_with_zero_records(self):
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw) / "spool"
            result = parity.append_parity(
                spool, [], producer="validate", run_id="empty-run", hermit_sha=HERMIT_SHA,
                emitted_at=EMITTED_AT, source_tree_dirty=False, host="fixturehost")
            marker, rows = parity.read_batch(spool / result["batch"])
            self.assertEqual((0, [], []), (marker["records"], marker["nodes"], rows))
            # Absence of a marker is the other state: nothing for this run id.
            other = parity.run_event_id("validate", "never-appended")
            self.assertEqual([], list(spool.glob(f"*__{other}.jsonl")))


class AppendOnlyReplay(unittest.TestCase):
    def append(self, spool, sources, emitted_at=EMITTED_AT, published=None):
        return parity.append_parity(
            spool, sources, producer="validate", run_id=RUN_ID, hermit_sha=HERMIT_SHA,
            emitted_at=emitted_at, source_tree_dirty=False, host="fixturehost",
            published=published)

    def test_replay_with_a_new_emitted_at_writes_nothing(self):
        sources = fixture_sources()[:4]
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw) / "spool"
            first_result = self.append(spool, sources)
            again = self.append(spool, sources, emitted_at="2026-09-28T22:00:00Z")
            self.assertTrue(again["already_recorded"])
            self.assertIsNone(again["batch"])
            self.assertEqual([first_result["batch"]], [p.name for p in parity.batch_paths(spool)])

    def test_changed_record_is_refused_not_replaced(self):
        sources = fixture_sources()[:4]
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw) / "spool"
            self.append(spool, sources)
            changed = copy.deepcopy(sources)
            changed[1]["record"]["first_difference"]["candidate_message"] = "changed"
            with self.assertRaises(parity.ParityLedgerError) as caught:
                self.append(spool, changed)
            self.assertIn(f"row {changed[1]['cell']} from node portable/manifest_backend_parity_c "
                          "differs from the recorded copy", str(caught.exception))
            self.assertIn("the parity ledger is append-only", str(caught.exception))

    def test_a_published_run_is_checked_after_its_batch_drained(self):
        sources = fixture_sources()[:4]
        published = {event["event_id"]: event for event in build(sources)}
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw) / "spool"
            smaller = sources[:3]
            with self.assertRaisesRegex(parity.ParityLedgerError,
                                        "is recorded but not in this batch|run marker"):
                self.append(spool, smaller, published=published)
            self.assertFalse(list(spool.glob("*.jsonl")) if spool.exists() else [])


class PublicationPlanning(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.spool = Path(self.temporary.name)

    def spool_batch(self, sources, run_id=RUN_ID):
        result = parity.append_parity(
            self.spool, sources, producer="validate", run_id=run_id, hermit_sha=HERMIT_SHA,
            emitted_at=EMITTED_AT, source_tree_dirty=False, host="fixturehost")
        return self.spool / result["batch"]

    def test_same_event_id_with_changed_bytes_is_refused(self):
        sources = fixture_sources()[:3]
        published_events = build(sources)
        path = self.spool_batch(copy.deepcopy(sources))
        published_events[2]["record"]["first_difference"]["candidate_message"] = "other"
        published = parity.PublishedParity("c" * 40, {e["event_id"]: e for e in published_events}, {})
        plan = parity.plan_publication([path], published)
        self.assertEqual([], plan.fresh_batches)
        self.assertEqual(path.name, plan.refused[0]["batch"])
        self.assertIn("is already published with a different body", plan.refused[0]["error"])

    def test_partly_published_run_is_refused_whole(self):
        sources = fixture_sources()[:3]
        path = self.spool_batch(sources)
        events = build(sources)
        published = parity.PublishedParity("c" * 40, {events[1]["event_id"]: events[1]}, {})
        plan = parity.plan_publication([path], published)
        self.assertIn("1 of its 4 event(s) are already published; a run is published whole",
                      plan.refused[0]["error"])

    def test_emitted_at_only_replay_is_already_published(self):
        sources = fixture_sources()[:3]
        path = self.spool_batch(sources)
        earlier = build(sources, emitted_at="2026-09-28T20:00:00Z")
        published = parity.PublishedParity("c" * 40, {e["event_id"]: e for e in earlier}, {})
        plan = parity.plan_publication([path], published)
        self.assertEqual(([], [path], [], 0),
                         (plan.fresh_batches, plan.already_published, plan.refused, plan.appended))

    def test_fresh_batches_append_marker_and_rows_to_their_shards(self):
        a = self.spool_batch(fixture_sources()[:3])
        b = self.spool_batch([], run_id="empty-run")
        plan = parity.plan_publication([a, b], parity.PublishedParity("c" * 40, {}, {}))
        self.assertEqual(sorted([a, b]), sorted(plan.fresh_batches))
        self.assertEqual(5, plan.appended)
        self.assertEqual({"parity/hermit/fixturehost/2026-09.jsonl",
                          "parity-runs/hermit/fixturehost/2026-09.jsonl"}, set(plan.texts))
        self.assertEqual(2, len(plan.texts["parity-runs/hermit/fixturehost/2026-09.jsonl"].splitlines()))


class CreditIsNeverInvented(unittest.TestCase):
    def test_credit_function(self):
        self.assertIsNone(parity.credit(0, 0, 0))
        self.assertEqual(1.0, parity.credit(5, 5, 5))
        self.assertEqual(0.0, parity.credit(0, 3, 4))
        self.assertLess(parity.credit(999_999, 999_999, 1_000_000), 1.0)

    def test_unmeasured_verdicts_have_no_measured_credit(self):
        for source in fixture_sources():
            record = source["record"]
            measured = parity.measured_credit(record) if record else None
            if source["verdict"] in parity.MEASURED_VERDICTS:
                self.assertIsNotNone(measured, source["cell"])
            else:
                self.assertIsNone(measured, source["cell"])


class UnavailableClassReadsTheTwoWordRole(unittest.TestCase):
    """The post-pass names an operand in two words (`kvm candidate`,
    `ptrace reference`); the class must come from the role's last word."""

    def test_run_1953_candidate_missing_reasons(self):
        reasons = [(source["verdict"], source["reason"]) for source in fixture_sources()
                   if source["verdict"] == "candidate-missing"]
        self.assertEqual(3, len(reasons))
        for verdict, reason in reasons:
            self.assertEqual("candidate-missing", parity.unavailable_class(verdict, reason))

    def test_role_and_reason_prefixes(self):
        cell = "backend-parity-c/pthread-lifecycle"
        cases = [
            ("unavailable", f"the ptrace reference verify cell of {cell} has no result row in this run",
             "reference-missing"),
            ("unavailable", f"the kvm candidate verify cell of {cell} failed determinism: 2 of 3 runs differ",
             "determinism-fail"),
            ("unavailable", f"the sabre candidate verify cell of {cell} ended with signal 9", "operand-ended"),
            ("unavailable", f"the kvm candidate verify cell of {cell} was host-inapplicable, so it left no log",
             "host-inapplicable"),
            ("unavailable", "no operand phrase at all", "other"),
            ("inputs-not-equalized", "anything", "inputs-not-equalized"),
            ("diverged", None, None),
            ("record-missing", "the node left no status", None),
        ]
        for verdict, reason, expected in cases:
            with self.subTest(verdict=verdict, reason=reason):
                self.assertEqual(expected, parity.unavailable_class(verdict, reason))


class SeriesGlobIsUnaffected(unittest.TestCase):
    def test_parity_trees_are_outside_the_series_glob(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            events = build(fixture_sources()[:2])
            for event in events:
                path = root / parity.event_shard(event)
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open("a", encoding="utf-8") as handle:
                    handle.write(parity.canonical_line(event) + "\n")
            (root / series.shard_path("hermit", "fixturehost", "2026-09")).parent.mkdir(parents=True)
            (root / series.shard_path("hermit", "fixturehost", "2026-09")).write_text("")
            self.assertEqual(["series/hermit/fixturehost/2026-09.jsonl"],
                             [str(p.relative_to(root)) for p in sorted(root.glob("series/*/*/*.jsonl"))])


if __name__ == "__main__":
    unittest.main()
