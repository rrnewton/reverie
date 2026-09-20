"""Identity and losslessness brackets for schema-v1 escalation events."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub/lib"))

from health_escalation_event import (  # noqa: E402
    InvalidEvent,
    load_event,
    make_event,
    parse_part,
    part_documents,
    stable_task_id,
    write_event,
)


def event(payload: str, *, host: str = "devbig014.example.com", suffix: str = "aaaaaaaa"):
    return make_event(
        kind="alarm",
        payload=payload,
        origin_host=host,
        findings=1,
        event_id=f"event-20260920T010203123456Z-123-{suffix}",
        created_at="2026-09-20T01:02:03Z",
    )


def reconstruct(envelope: dict) -> str:
    parsed = [parse_part(document) for document in part_documents(envelope)]
    assert all(part is not None for part in parsed)
    return "".join(part["payload"] for part in parsed if part is not None)


def test_exact_227307_byte_payload_round_trips_in_bounded_parts() -> None:
    payload = ("ACTION: α\\n\tfixture\n" * 20_000).encode("utf-8")[:227_307]
    # End on a UTF-8 boundary while retaining the exact measured byte size.
    while True:
        try:
            text = payload.decode("utf-8")
            break
        except UnicodeDecodeError:
            payload = payload[:-1] + b"x"
    assert len(text.encode("utf-8")) == 227_307
    envelope = event(text)

    documents = part_documents(envelope)

    assert len(documents) > 1
    assert max(len(document.encode("utf-8")) for document in documents) < 32_000
    assert reconstruct(envelope) == text


@pytest.mark.parametrize("ordinal", range(10))
def test_each_of_ten_over_limit_payloads_is_lossless(ordinal: int) -> None:
    size = 131_001 + ordinal * 9_731
    payload = (f"ACTION: over-limit-{ordinal} " + "x" * size)[:size]
    envelope = event(payload, suffix=f"{ordinal + 1:08x}")
    assert len(payload.encode("utf-8")) == size
    assert len(part_documents(envelope)) >= 5
    assert reconstruct(envelope) == payload


def test_host_scoped_keys_and_stable_ids_do_not_cross_hosts() -> None:
    first = event("ACTION: same\n", host="devbig014.example.com", suffix="11111111")
    second = event("ACTION: same\n", host="devbig030.example.com", suffix="22222222")

    assert first["key"] != second["key"]
    assert first["origin_host"] != second["origin_host"]
    assert stable_task_id(first) != stable_task_id(second)

    punctuation_a = event("ACTION: same\n", host="devbig014.a", suffix="33333333")
    punctuation_b = event("ACTION: same\n", host="devbig014-a", suffix="44444444")
    assert punctuation_a["key"] != punctuation_b["key"]


def test_atomic_envelope_is_private_and_tampering_is_refused(tmp_path: Path) -> None:
    envelope = event("ACTION: durable\n")
    path = write_event(tmp_path, envelope)
    assert path.name == f"{envelope['event_id']}.json"
    assert os.stat(path).st_mode & 0o777 == 0o600
    assert load_event(path) == envelope

    damaged = json.loads(path.read_text())
    damaged["payload"] += "changed"
    path.write_text(json.dumps(damaged))
    with pytest.raises(InvalidEvent, match="durable identity"):
        load_event(path)


def test_malformed_field_types_are_a_controlled_refusal(tmp_path: Path) -> None:
    envelope = event("ACTION: durable\n")
    path = write_event(tmp_path, envelope)
    malformed = dict(envelope, digest_ignore=7)
    path.write_text(json.dumps(malformed))
    with pytest.raises(InvalidEvent, match="list of strings"):
        load_event(path)
