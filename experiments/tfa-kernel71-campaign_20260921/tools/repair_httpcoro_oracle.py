#!/usr/bin/env python3
"""Censor HTTPCoro outcomes that missed their predeclared failure class.

The raw executions terminate in the selected test's fatal CHECK at line 1685,
but the sealed plan only authorized a GoogleTest ``[  FAILED  ]`` line plus a
one-failed-test summary.  Preserve the observations and strict matchers while
removing the unsupported FAIL interpretation.
"""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path


CAMPAIGN = Path(__file__).resolve().parents[1]
TARGET = CAMPAIGN / "services/proxygen/httpcorosessiontests--e3f69b337304"
RECORD = TARGET / "experiment.json"
REPAIR_ID = "httpcoro-kernel71-oracle-integrity-repair-seed-015"
REPAIR_TIME = "2026-09-21T18:05:52Z"
OLD_CLASS = "gtest-selected-test-failure"
CENSOR_REASON = (
    "The sealed gtest-selected-test-failure class requires a [  FAILED  ] line "
    "and one-failed-test summary; retained stdout ends after [ RUN ], while "
    "stderr records the selected test's line-1685 fatal CHECK/SIGABRT, a "
    "target-owned class that was not declared before these executions."
)
DIAGNOSTIC = (
    "status=CENSORED; selected=1; oracle=predeclared-gtest-failure-class-not-satisfied; "
    "observed=line-1685-fatal-check-sigabrt; exit=134; BitwiseInfoV1=matched"
)
PENDING_MEASUREMENTS = [
    "Seeds 16-99 remain in this predeclared fuzz-seed population; before any further HTTPCoro run, the owner-directed virtual-time search must be prospectively sealed in a separate schema-v2 campaign cell."
]


def encode(value: object, *, compact: bool = False) -> bytes:
    if compact:
        return (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def artifact(event: dict, kind: str) -> dict:
    matches = [item for item in event["artifacts"] if item["kind"] == kind]
    if len(matches) != 1:
        raise RuntimeError(
            f"{event['event_id']}: expected exactly one {kind} artifact, got {len(matches)}"
        )
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
    if record["plan_receipt"]["commit"] != "cbfa263ec100079bad45c0c95b2beab241c97bca":
        raise RuntimeError("HTTPCoro plan receipt changed; refuse a contextual repair")
    events = record["events"]
    samples = record["samples"]
    if len(events) != 32 or len(samples) != 16:
        raise RuntimeError(
            f"expected repaired boundary of 32 events/16 samples, got {len(events)}/{len(samples)}"
        )

    for event in events:
        if event["kind"] != "HERMIT_EXECUTION":
            raise RuntimeError(f"unexpected non-Hermit event {event['event_id']}")
        stdout = artifact_path(artifact(event, "STDOUT")).read_text(errors="replace")
        stderr = artifact_path(artifact(event, "STDERR")).read_text(errors="replace")
        if "[ RUN      ] HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2" not in stdout:
            raise RuntimeError(f"{event['event_id']}: selected test RUN marker missing")
        if "[  FAILED  ]" in stdout or "1 FAILED TEST" in stdout:
            raise RuntimeError(f"{event['event_id']}: raw output unexpectedly satisfies sealed class")
        if (
            "HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()"
            not in stderr
            or "Signal 6 (SIGABRT)" not in stderr
        ):
            raise RuntimeError(f"{event['event_id']}: expected fatal CHECK evidence missing")

        prior = event["result"]
        if prior["target_oracle"] not in {"FAIL", "AMBIGUOUS"}:
            raise RuntimeError(f"{event['event_id']}: unexpected prior oracle")
        if prior["target_oracle"] == "FAIL" and prior["failure_class_id"] != OLD_CLASS:
            raise RuntimeError(f"{event['event_id']}: unexpected prior failure class")
        prior.update(
            target_oracle="AMBIGUOUS",
            failure_class_id=None,
            diagnostic=DIAGNOSTIC,
        )

        receipt = artifact(event, "RESULT")
        payload = encode(result_payload(event), compact=True)
        result_path = artifact_path(receipt)
        result_path.write_bytes(payload)
        receipt["sha256"] = digest(payload)
        receipt["bytes"] = len(payload)

    event_ids = {event["event_id"] for event in events}
    for sample in samples:
        if not set(sample["execution_event_ids"]).issubset(event_ids):
            raise RuntimeError(f"{sample['sample_id']}: unknown event reference")
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
                "Seeds 0-15 terminate in a repeatable target-owned fatal CHECK, but that CHECK was not a predeclared failure class; all 16 tuples are censored rather than FAIL.",
                "A complete distinct-seed PASS/target-owned-FAIL bracket is not present.",
            ],
            pending_in_scope_measurements=PENDING_MEASUREMENTS,
            measurement_budget_exhausted=False,
        )
        record["outcome_history"].append(repaired)
        record["current_outcome"] = repaired
    elif record["outcome_history"][-1] != current:
        raise RuntimeError("existing repair outcome is not the history tail")

    for outcome in (record["outcome_history"][-1], record["current_outcome"]):
        outcome["pending_in_scope_measurements"] = PENDING_MEASUREMENTS

    RECORD.write_bytes(encode(record))


if __name__ == "__main__":
    main()
