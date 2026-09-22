#!/usr/bin/env python3
"""Censor TLS aborts that did not satisfy the sealed GoogleTest FAIL class."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


CAMPAIGN = Path(__file__).resolve().parents[1]
TARGET = CAMPAIGN / "services/proxygen/tls-terminator-test--60719afd988e"
RECORD = TARGET / "experiment.json"
PLAN_COMMIT = "fee8d8629972a8f69b15527901770d961bece0c3"
REPAIR_ID = "tls-kernel71-oracle-integrity-repair-seed-012"
REPAIR_TIME = "2026-09-21T18:56:00Z"
OLD_CLASS = "gtest-selected-test-failure"
REPAIRED_SEEDS = {11, 12}
CENSOR_REASON = (
    "The sealed gtest-selected-test-failure class requires the exact selected "
    "[  FAILED  ] line and one-failed-test summary. Retained stdout stops after "
    "[ RUN ], while stderr records an uncaught certificate-not-yet-valid "
    "AsyncSocketException and SIGABRT under the fixed 2026-01-01 epoch."
)
DIAGNOSTIC = (
    "status=CENSORED; selected=1; "
    "oracle=predeclared-gtest-failure-class-not-satisfied; "
    "observed=certificate-not-yet-valid-asyncsocketexception-sigabrt; "
    "exit=134; BitwiseInfoV1=matched"
)


def encode(value: object, *, compact: bool = False) -> bytes:
    if compact:
        return (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def artifact(event: dict, kind: str) -> dict:
    matches = [item for item in event["artifacts"] if item["kind"] == kind]
    if len(matches) != 1:
        raise RuntimeError(f"{event['event_id']}: expected one {kind} artifact")
    return matches[0]


def artifact_path(item: dict) -> Path:
    path = CAMPAIGN / item["locator"]
    if not path.is_file():
        raise RuntimeError(f"missing tracked artifact {path}")
    return path


def result_payload(event: dict) -> dict:
    return {
        "schema_version": 1,
        "event_id": event["event_id"],
        "kind": event["kind"],
        "route_id": event["route_id"],
        "cell_id": event["cell_id"],
        "measurement_role": event["measurement_role"],
        "invocation_id": event["invocation_id"],
        "replica_index": event["replica_index"],
        "seed": event["seed"],
        "argv": event["argv"],
        "result": event["result"],
    }


def main() -> None:
    record = json.loads(RECORD.read_text())
    if record["plan_receipt"]["commit"] != PLAN_COMMIT:
        raise RuntimeError("TLS plan receipt changed; refusing contextual repair")
    if len(record["events"]) != 17 or len(record["samples"]) != 15:
        raise RuntimeError("TLS material boundary is not the expected 17 events/15 samples")

    repaired_event_ids: set[str] = set()
    for event in record["events"]:
        if event["seed"]["value"] not in REPAIRED_SEEDS:
            continue
        stdout = artifact_path(artifact(event, "STDOUT")).read_text(errors="replace")
        stderr = artifact_path(artifact(event, "STDERR")).read_text(errors="replace")
        selected = (
            "[ RUN      ] "
            "TlsTerminatorTest/TlsTerminatorTest.terminateTls/"
            "h1_innerTls_http_connect"
        )
        if stdout.count(selected) != 1:
            raise RuntimeError(f"{event['event_id']}: selected RUN marker mismatch")
        if "[  FAILED  ]" in stdout or "1 FAILED TEST" in stdout:
            raise RuntimeError(f"{event['event_id']}: raw output satisfies sealed class")
        if (
            "certificate verification failed: certificate is not yet valid" not in stderr
            or "folly::AsyncSocketException" not in stderr
            or "Signal 6 (SIGABRT)" not in stderr
        ):
            raise RuntimeError(f"{event['event_id']}: expected certificate abort missing")
        prior = event["result"]
        if prior["target_oracle"] not in {"FAIL", "AMBIGUOUS"}:
            raise RuntimeError(f"{event['event_id']}: unexpected prior classification")
        if prior["target_oracle"] == "FAIL" and prior["failure_class_id"] != OLD_CLASS:
            raise RuntimeError(f"{event['event_id']}: unexpected prior failure class")
        if prior["target_oracle"] == "AMBIGUOUS" and (
            prior["failure_class_id"] is not None
            or prior.get("diagnostic") != DIAGNOSTIC
        ):
            raise RuntimeError(f"{event['event_id']}: existing repair drifted")
        prior.update(
            target_oracle="AMBIGUOUS",
            failure_class_id=None,
            diagnostic=DIAGNOSTIC,
        )
        receipt = artifact(event, "RESULT")
        payload = encode(result_payload(event), compact=True)
        artifact_path(receipt).write_bytes(payload)
        receipt["sha256"] = hashlib.sha256(payload).hexdigest()
        receipt["bytes"] = len(payload)
        repaired_event_ids.add(event["event_id"])

    if len(repaired_event_ids) != 4:
        raise RuntimeError(f"expected four repaired replicas, got {len(repaired_event_ids)}")
    for sample in record["samples"]:
        if sample["seed"]["value"] not in REPAIRED_SEEDS:
            continue
        if set(sample["execution_event_ids"]).isdisjoint(repaired_event_ids):
            raise RuntimeError(f"{sample['sample_id']}: repaired events not referenced")
        sample.update(
            status="CENSORED",
            target_outcome=None,
            failure_class_id=None,
            censor_reason=CENSOR_REASON,
        )

    current = record["current_outcome"]
    if current["classification_id"] != REPAIR_ID:
        repaired = copy.deepcopy(current)
        repaired.update(
            classification_id=REPAIR_ID,
            classified_at=REPAIR_TIME,
            supersedes=current["classification_id"],
            missing_or_ambiguous_facts=[
                "Seeds 11 and 12 abort repeatably on certificate validity under the fixed January epoch, but retained output does not satisfy the predeclared GoogleTest failure class.",
                "The other 13 attempted tuples did not complete strict BitwiseInfoV1 matching; no qualifying PASS or FAIL arm exists.",
            ],
            measurement_budget_exhausted=True,
            pending_in_scope_measurements=[],
        )
        record["outcome_history"].append(repaired)
        record["current_outcome"] = repaired
    elif record["outcome_history"][-1] != current:
        raise RuntimeError("existing repair is not the history tail")

    RECORD.write_bytes(encode(record))


if __name__ == "__main__":
    main()
