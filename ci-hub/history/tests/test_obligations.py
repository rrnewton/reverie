from __future__ import annotations

import json
from unittest import mock
import sys
import tempfile
import unittest
from pathlib import Path

HISTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HISTORY))

import obligations

SHA = "a" * 40
POLICY = {
    "schema_version": 2,
    "repo": "rrnewton/hermit",
    "github": {
        "required_jobs": [
            {
                "workflow_file": ".github/workflows/ci-portable.yml",
                "workflow_name": "CI (GitHub-managed portable)",
                "job_name": "Regular tests (GitHub-managed portable)",
            },
            {
                "workflow_file": ".github/workflows/ci-privileged.yml",
                "workflow_name": "CI (privileged)",
                "job_name": "Privileged capability and E2E tests",
            },
        ],
        "required_positive_count": 2,
    },
}


class ObligationStoreTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.store = Path(self.temporary.name) / "obligations.jsonl"

    def tearDown(self) -> None:
        obligations._MEMORY_CHECKPOINTS.clear()
        self.temporary.cleanup()

    def create(self) -> dict:
        return obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=SHA,
            land_mode="admin",
            verification_policy=POLICY,
            actor="test",
            obligation_id="test-obligation",
            path=self.store,
        )

    def test_create_has_join_keys_and_cost_slots(self) -> None:
        record = self.create()
        self.assertEqual(record["landed_sha"], SHA)
        self.assertEqual(record["repo"], "rrnewton/hermit")
        self.assertEqual(record["overall_state"], "open")
        self.assertEqual(record["verification_policy"], POLICY)
        self.assertEqual(record["local"]["cost"]["actual"], None)
        self.assertEqual(record["github"]["run_ids"], [])

    def test_transitions_append_full_latest_projection(self) -> None:
        self.create()
        updated = obligations.transition(
            "test-obligation",
            "github-observed",
            {"github": {"state": "running", "run_ids": [123]}},
            self.store,
        )
        self.assertEqual(updated["github"]["run_ids"], [123])
        self.assertEqual(updated["local"]["state"], "not_started")
        lines = [json.loads(line) for line in self.store.read_text().splitlines()]
        self.assertEqual(len(lines), 2)
        self.assertNotEqual(lines[0]["event_id"], lines[1]["event_id"])
        self.assertEqual(obligations.get_record("test-obligation", self.store), updated)

    def test_compare_and_append_allows_only_one_launch_claim(self) -> None:
        opened = self.create()
        first = obligations.transition_if_matches(
            "test-obligation",
            "launch-claimed",
            {"launch": {"state": "launching", "token": "winner"}},
            {"event_id": opened["event_id"]},
            self.store,
        )
        self.assertIsNotNone(first)
        lost = obligations.transition_if_matches(
            "test-obligation",
            "launch-claimed",
            {"launch": {"state": "launching", "token": "loser"}},
            {"event_id": opened["event_id"]},
            self.store,
        )
        self.assertIsNone(lost)
        latest = obligations.get_record("test-obligation", self.store)
        self.assertEqual(latest["launch"]["token"], "winner")

    def test_duplicate_open_sha_is_rejected(self) -> None:
        record = self.create()
        with self.assertRaises(obligations.DuplicateOpenObligation) as context:
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="speculative",
                path=self.store,
            )
        self.assertEqual(context.exception.record["obligation_id"], record["obligation_id"])

    def test_checkpoint_obligation_id_is_reserved(self) -> None:
        with self.assertRaisesRegex(obligations.StoreError, "reserved"):
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=SHA,
                land_mode="admin",
                obligation_id=obligations.CHECKPOINT_OBLIGATION_ID,
                path=self.store,
            )

    def test_closed_sha_can_open_a_new_obligation(self) -> None:
        self.create()
        obligations.transition(
            "test-obligation", "satisfied", {"overall_state": "satisfied"}, self.store
        )
        reopened = obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=SHA,
            land_mode="speculative",
            obligation_id="second-obligation",
            path=self.store,
        )
        self.assertEqual(reopened["overall_state"], "open")
        self.assertEqual([r["obligation_id"] for r in obligations.unresolved_records(self.store)], ["second-obligation"])

    def test_identity_is_immutable(self) -> None:
        self.create()
        with self.assertRaises(obligations.StoreError):
            obligations.transition(
                "test-obligation", "bad", {"landed_sha": "b" * 40}, self.store
            )

    def test_bound_policy_is_in_initial_event_and_immutable(self) -> None:
        self.create()
        opened = json.loads(self.store.read_text().splitlines()[0])
        self.assertEqual(opened["event_type"], "opened")
        self.assertEqual(opened["verification_policy"], POLICY)
        with self.assertRaisesRegex(obligations.StoreError, "bound verification"):
            obligations.transition(
                "test-obligation",
                "bad-policy",
                {"verification_policy": {**POLICY, "schema_version": 3}},
                self.store,
            )

    def test_legacy_policy_can_be_bound_exactly_once(self) -> None:
        obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha=SHA,
            land_mode="admin",
            actor="test",
            obligation_id="legacy",
            path=self.store,
        )
        bound = obligations.transition(
            "legacy",
            "verification-policy-bound",
            {"verification_policy": POLICY},
            self.store,
        )
        self.assertEqual(bound["verification_policy"], POLICY)
        repeated = obligations.transition(
            "legacy",
            "verification-policy-bound",
            {"verification_policy": POLICY},
            self.store,
        )
        self.assertEqual(repeated["verification_policy"], POLICY)
        with self.assertRaisesRegex(obligations.StoreError, "bound verification"):
            obligations.transition(
                "legacy",
                "policy-rebound",
                {"verification_policy": {**POLICY, "schema_version": 3}},
                self.store,
            )

    def test_latest_records_reuses_checkpoint_without_rescanning_prefix(self) -> None:
        created = self.create()
        self.assertEqual(obligations.latest_records(self.store)["test-obligation"], created)
        checkpoint = obligations._checkpoint_path(self.store.resolve())
        self.assertTrue(checkpoint.exists())
        checkpoint_mtime = checkpoint.stat().st_mtime_ns

        obligations._MEMORY_CHECKPOINTS.clear()
        with mock.patch.object(
            obligations, "_scan_binary_tail", wraps=obligations._scan_binary_tail
        ) as scan:
            reread = obligations.latest_records(self.store)
        self.assertEqual(reread["test-obligation"], created)
        self.assertEqual(scan.call_args.kwargs["offset"], self.store.stat().st_size)
        self.assertEqual(checkpoint.stat().st_mtime_ns, checkpoint_mtime)

    def test_checkpoint_commit_is_a_closed_record_for_legacy_readers(self) -> None:
        self.create()
        obligations.latest_records(self.store)
        commit = json.loads(self.store.read_text().splitlines()[-1])
        legacy_latest = {}
        obligations._apply_event(legacy_latest, commit, 2)
        legacy_record = legacy_latest[obligations.CHECKPOINT_OBLIGATION_ID]
        self.assertIn(legacy_record["overall_state"], obligations.CLOSED_STATES)
        self.assertEqual(legacy_record["local"]["state"], "checkpoint")
        self.assertEqual(legacy_record["github"]["state"], "checkpoint")

    def test_latest_records_reads_only_events_appended_after_checkpoint(self) -> None:
        self.create()
        obligations.latest_records(self.store)
        checkpointed_size = self.store.stat().st_size
        updated = obligations.transition(
            "test-obligation", "github-observed", {"github": {"state": "running"}}, self.store
        )

        obligations._MEMORY_CHECKPOINTS.clear()
        with mock.patch.object(
            obligations, "_scan_binary_tail", wraps=obligations._scan_binary_tail
        ) as scan:
            reread = obligations.latest_records(self.store)
        self.assertEqual(scan.call_args.kwargs["offset"], checkpointed_size)
        self.assertEqual(reread["test-obligation"], updated)

    def test_invalid_checkpoint_falls_back_to_full_authoritative_scan(self) -> None:
        created = self.create()
        obligations.latest_records(self.store)
        obligations._checkpoint_path(self.store.resolve()).write_text("not-json\n")
        obligations._MEMORY_CHECKPOINTS.clear()
        with mock.patch.object(
            obligations, "_scan_binary_tail", wraps=obligations._scan_binary_tail
        ) as scan:
            reread = obligations.latest_records(self.store)
        self.assertEqual(scan.call_args.kwargs["offset"], 0)
        self.assertEqual(reread["test-obligation"], created)

    def test_well_formed_but_tampered_projection_falls_back_to_full_scan(self) -> None:
        created = self.create()
        obligations.latest_records(self.store)
        checkpoint = obligations._checkpoint_path(self.store.resolve())
        document = json.loads(checkpoint.read_text())
        document["latest"] = {}
        checkpoint.write_text(json.dumps(document) + "\n")
        obligations._MEMORY_CHECKPOINTS.clear()
        with mock.patch.object(
            obligations, "_scan_binary_tail", wraps=obligations._scan_binary_tail
        ) as scan:
            reread = obligations.latest_records(self.store)
        self.assertEqual(scan.call_args.kwargs["offset"], 0)
        self.assertEqual(reread["test-obligation"], created)

    def test_forged_projection_with_recomputed_self_hash_is_rejected(self) -> None:
        created = self.create()
        obligations.latest_records(self.store)
        checkpoint = obligations._checkpoint_path(self.store.resolve())
        document = json.loads(checkpoint.read_text())
        document["latest"]["test-obligation"]["actor"] = "forged"
        document["latest_sha256"] = obligations._projection_fingerprint(
            document["latest"]
        )
        checkpoint.write_text(json.dumps(document) + "\n")
        obligations._MEMORY_CHECKPOINTS.clear()
        with mock.patch.object(
            obligations, "_scan_binary_tail", wraps=obligations._scan_binary_tail
        ) as scan:
            reread = obligations.latest_records(self.store)
        self.assertEqual(scan.call_args.kwargs["offset"], 0)
        self.assertEqual(reread["test-obligation"], created)

    def test_same_size_prefix_rewrite_fails_closed(self) -> None:
        self.create()
        for index in range(4):
            obligations.create_obligation(
                repo="rrnewton/hermit",
                landed_sha=f"{index + 1:040x}",
                land_mode="admin",
                actor="filler",
                obligation_id=f"filler-{index}",
                path=self.store,
            )
        obligations.latest_records(self.store)
        raw = self.store.read_bytes()
        old = b'"actor":"test"'
        new = b'"actor":"evil"'
        position = raw.index(old)
        self.assertLess(
            position,
            len(raw) - obligations.CHECKPOINT_FINGERPRINT_BYTES,
        )
        with self.store.open("r+b") as handle:
            handle.seek(position)
            handle.write(new)
        obligations._MEMORY_CHECKPOINTS.clear()
        with self.assertRaisesRegex(obligations.StoreError, "checkpoint.*inconsistent"):
            obligations.latest_records(self.store)

    def test_replaced_store_cannot_reuse_old_checkpoint(self) -> None:
        self.create()
        obligations.latest_records(self.store)
        checkpoint = obligations._checkpoint_path(self.store.resolve())
        old_checkpoint = checkpoint.read_text()
        self.store.unlink()
        second = obligations.create_obligation(
            repo="rrnewton/hermit",
            landed_sha="b" * 40,
            land_mode="admin",
            actor="test",
            obligation_id="replacement",
            path=self.store,
        )
        checkpoint.write_text(old_checkpoint)
        obligations._MEMORY_CHECKPOINTS.clear()
        with mock.patch.object(
            obligations, "_scan_binary_tail", wraps=obligations._scan_binary_tail
        ) as scan:
            reread = obligations.latest_records(self.store)
        self.assertEqual(scan.call_args.kwargs["offset"], 0)
        self.assertEqual(reread, {"replacement": second})


if __name__ == "__main__":
    unittest.main()
