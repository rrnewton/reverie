#!/usr/bin/env python3
"""The parity ledger store: one `parity-ledger/v1` row per parity cell of a run.

Parity is measured after determinism: hermit's post-pass compares each
candidate backend's retained verify log against the ptrace reference and writes
one `ParityRecord` per cell to `parity.jsonl`. hermit builds one
`ParityLedgerSource` per cell (`parity::ledger_sources`); this module wraps
each in the published envelope, checks every invariant the envelope and its
record promise, and hands the batch to the ledger publisher. The design is
https://github.com/rrnewton/hermit/issues/3301.

Two trees in the test ledger, both outside the `series/*/*/*.jsonl` glob
every series reader uses:

* `parity/<team>/<host>/<YYYY-MM>.jsonl` holds the rows. Their fields are
  exactly hermit's `ParityLedgerRow` (`deny_unknown_fields`), so the
  scorecard reads them with the same Rust type the post-pass writes.
* `parity-runs/<team>/<host>/<YYYY-MM>.jsonl` holds one `parity.run` marker
  per appended run. A row type that refuses unknown fields cannot also carry
  "this run appended N rows", so the marker is its own event. It is what
  tells three states apart: no marker means `append-parity` never ran for the
  run (an older hermit, or a producer that never called it); a marker with
  `records: 0` means the run's parity selection was empty; and a node that
  never ran shows up as `record-missing` rows whose origin says `absent`.

A spooled batch is one file per run: the marker line, then the rows, in the
canonical compact encoding. The batch is published whole or not at all.

Everything here refuses loudly. A malformed record is never skipped, a
record-missing cell stays visible as a row, and credit that is null is never
turned into 0 or 1.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

SCHEMA = "parity-ledger/v1"
ROW_EVENT_TYPE = "parity.record"
RUN_EVENT_TYPE = "parity.run"
TEAM = "hermit"
RECORD_SCHEMA = 1
ROW_ROOT = "parity"
RUN_ROOT = "parity-runs"
PRODUCERS = ("validate", "pressure-test")
BACKENDS = ("dbt", "kvm", "liteinst", "sabre")
REFERENCE_BACKEND = "ptrace"
#: Most to least adverse, hermit's `LedgerVerdict` order.
LEDGER_VERDICTS = (
    "record-missing",
    "unavailable",
    "reference-missing",
    "candidate-missing",
    "inputs-not-equalized",
    "diverged",
    "matched",
)
RECORD_VERDICTS = LEDGER_VERDICTS[1:]
MEASURED_VERDICTS = frozenset({"matched", "diverged"})
POST_PASS_STATES = ("complete", "failed", "running", "absent")
SCOPE_SOURCES = ("status-scope", "status-count", "expected-scope")
#: hermit's `UnavailableClass`, in the order the scorecard prints them.
UNAVAILABLE_CLASSES = (
    "determinism-fail",
    "host-inapplicable",
    "operand-ended",
    "log-not-retained",
    "inputs-not-equalized",
    "reference-missing",
    "candidate-missing",
    "other",
)
#: The largest f64 below 1.0: a divergence can never round up to a match.
BELOW_ONE = 1.0 - sys.float_info.epsilon / 2
DBT_INPUTS_NOT_EQUALIZABLE = (
    "the dbt backend refuses --bind and --mount (hermit-cli/src/bin/hermit/run.rs: "
    '"its DynamoRIO adapter does not enter the guest mount namespace"), so its guest '
    "cannot be given the ptrace cell's input paths"
)
_U64_MAX = (1 << 64) - 1
_SHA1_RE = re.compile(r"[0-9a-f]{40}")
_SHA256_RE = re.compile(r"[0-9a-f]{64}")
_EMITTED_AT_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
# Built rather than written literally so this module does not itself contain
# the owner-path needle the portability check greps tracked files for.
_OWNER_PATH_NEEDLE = "/" + "home" + "/"

FIRST_DIFFERENCE_FIELDS = (
    "field",
    "syscall",
    "scheduler_turn",
    "virtual_nanoseconds",
    "reference_message",
    "candidate_message",
)
RECORD_FIELDS = (
    "schema",
    "test_id",
    "backend",
    "verdict",
    "inputs_equalized",
    "reason",
    "credit",
    "unequalized_credit",
    "first_divergent_record",
    "left_len",
    "right_len",
    "matched_prefix",
    "first_difference",
    "reference_log",
    "candidate_log",
    "run_id",
    "hermit_sha",
)
ORIGIN_FIELDS = (
    "lane",
    "node",
    "post_pass_state",
    "status_sha256",
    "records_sha256",
    "hermit_bin_sha256",
    "scope_source",
)
SOURCE_FIELDS = (
    "cell",
    "test_id",
    "backend",
    "verdict",
    "reason",
    "run_id",
    "hermit_sha",
    "source",
    "record",
)
ROW_FIELDS = (
    "schema",
    "event_type",
    "event_id",
    "team",
    "host",
    "emitted_at",
    "producer",
    "run_id",
    "hermit_sha",
    "source_tree_dirty",
    "cell",
    "test_id",
    "backend",
    "verdict",
    "reason",
    "source",
    "record",
)
RUN_NODE_FIELDS = ORIGIN_FIELDS + ("records",)
RUN_FIELDS = (
    "schema",
    "event_type",
    "event_id",
    "team",
    "host",
    "emitted_at",
    "producer",
    "run_id",
    "hermit_sha",
    "source_tree_dirty",
    "records",
    "verdicts",
    "nodes",
)
#: The fields a run marker and each of its rows must share.
SHARED_FIELDS = (
    "team",
    "host",
    "emitted_at",
    "producer",
    "run_id",
    "hermit_sha",
    "source_tree_dirty",
)


class ParityLedgerError(ValueError):
    """A parity row, marker, batch, or source that breaks the contract."""


# --------------------------------------------------------------------------- JSON


def _unique_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ParityLedgerError(f"duplicate JSON key {key!r}")
        value[key] = item
    return value


def _refuse_constant(name: str) -> Any:
    raise ParityLedgerError(f"{name} is not a JSON number")


def loads(text: str, label: str) -> Any:
    """Strict JSON: duplicate keys, NaN and Infinity are refused."""
    try:
        return json.loads(
            text, object_pairs_hook=_unique_pairs, parse_constant=_refuse_constant
        )
    except ParityLedgerError as error:
        raise ParityLedgerError(f"{label}: {error}") from error
    except (json.JSONDecodeError, RecursionError) as error:
        raise ParityLedgerError(f"{label}: not JSON: {error}") from error


def canonical_line(event: Mapping[str, Any]) -> str:
    """The one encoding a published event has: compact, fixed key order.

    The key order is the Rust struct's field order (serde writes fields in
    declaration order), so a record copied from `parity.jsonl` keeps its
    bytes.
    """
    try:
        return json.dumps(
            canonical_event(event), ensure_ascii=False, separators=(",", ":"),
            allow_nan=False,
        )
    except (TypeError, ValueError) as error:
        raise ParityLedgerError(f"cannot encode a parity event: {error}") from error


def _ordered(value: Mapping[str, Any], fields: Sequence[str]) -> dict[str, Any]:
    ordered = {field: value[field] for field in fields if field in value}
    ordered.update((key, item) for key, item in value.items() if key not in ordered)
    return ordered


def _canonical_record(record: Any) -> Any:
    if not isinstance(record, Mapping):
        return record
    ordered = _ordered(record, RECORD_FIELDS)
    if isinstance(ordered.get("first_difference"), Mapping):
        ordered["first_difference"] = _ordered(
            ordered["first_difference"], FIRST_DIFFERENCE_FIELDS
        )
    return ordered


def canonical_event(event: Mapping[str, Any]) -> dict[str, Any]:
    """Reorder a row, marker, or source to its schema's field order."""
    if event.get("event_type") == RUN_EVENT_TYPE:
        ordered = _ordered(event, RUN_FIELDS)
        if isinstance(ordered.get("verdicts"), Mapping):
            ordered["verdicts"] = _ordered(ordered["verdicts"], LEDGER_VERDICTS)
        if isinstance(ordered.get("nodes"), list):
            ordered["nodes"] = [
                _ordered(node, RUN_NODE_FIELDS) if isinstance(node, Mapping) else node
                for node in ordered["nodes"]
            ]
        return ordered
    if "event_type" not in event and "inputs_equalized" in event:
        return _canonical_record(event)
    fields = ROW_FIELDS if "event_type" in event else SOURCE_FIELDS
    ordered = _ordered(event, fields)
    if isinstance(ordered.get("source"), Mapping):
        ordered["source"] = _ordered(ordered["source"], ORIGIN_FIELDS)
    if "record" in ordered:
        ordered["record"] = _canonical_record(ordered["record"])
    return ordered


# ------------------------------------------------------------- Rust text forms


def _rust_display(value: float) -> str:
    """Rust's `{}` for an f64: never exponent notation, no trailing `.0`."""
    if math.isnan(value):
        return "NaN"
    if math.isinf(value):
        return "inf" if value > 0 else "-inf"
    text = format(Decimal(repr(value)), "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return "-0" if text in {"-0", "-0."} else text


def _rust_debug_float(value: float) -> str:
    """Rust's `{:?}` for an f64: `1.0`, `0.5`, `1e-5`."""
    if math.isnan(value):
        return "NaN"
    if math.isinf(value):
        return "inf" if value > 0 else "-inf"
    if value == 0:
        return "-0.0" if math.copysign(1.0, value) < 0 else "0.0"
    if 1e-4 <= abs(value) < 1e16:
        text = format(Decimal(repr(value)), "f")
        return text if "." in text else text + ".0"
    mantissa, _, exponent = repr(value).partition("e")
    return f"{mantissa}e{int(exponent)}"


def _rust_debug_option(value: Any) -> str:
    if value is None:
        return "None"
    if isinstance(value, float):
        return f"Some({_rust_debug_float(value)})"
    return f"Some({value})"


def _rust_debug_str(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def _camel(verdict: str) -> str:
    """Rust's `{:?}` of a `ParityVerdict`: `InputsNotEqualized`."""
    return "".join(part.capitalize() for part in verdict.split("-"))


# ------------------------------------------------------------------ primitives


def _exact_fields(value: Any, fields: Sequence[str], label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ParityLedgerError(f"{label} must be a JSON object, got {type(value).__name__}")
    missing = [field for field in fields if field not in value]
    unknown = sorted(set(value) - set(fields))
    if missing or unknown:
        parts = []
        if missing:
            parts.append(f"missing {', '.join(missing)}")
        if unknown:
            parts.append(f"unknown {', '.join(unknown)}")
        raise ParityLedgerError(f"{label} fields are not exactly its schema's: {'; '.join(parts)}")
    return value


def _string(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise ParityLedgerError(f"{label} must be a string, got {_kind(value)}")
    return value


def _optional_string(value: Any, label: str) -> str | None:
    return None if value is None else _string(value, label)


def _boolean(value: Any, label: str) -> bool:
    if type(value) is not bool:
        raise ParityLedgerError(f"{label} must be true or false, got {_kind(value)}")
    return value


def _count(value: Any, label: str) -> int:
    if type(value) is not int or not 0 <= value <= _U64_MAX:
        raise ParityLedgerError(f"{label} must be a nonnegative integer, got {_kind(value)}")
    return value


def _optional_count(value: Any, label: str) -> int | None:
    return None if value is None else _count(value, label)


def _optional_float(value: Any, label: str) -> float | None:
    if value is None:
        return None
    if type(value) not in (int, float):
        raise ParityLedgerError(f"{label} must be a number or null, got {_kind(value)}")
    number = float(value)
    if not math.isfinite(number):
        raise ParityLedgerError(f"{label} must be finite, got {value!r}")
    return number


def _optional_sha256(value: Any, label: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
        raise ParityLedgerError(f"{label} must be 64 lowercase hex digits or null, got {value!r}")
    return value


def _choice(value: Any, choices: Sequence[str], label: str) -> str:
    if not isinstance(value, str) or value not in choices:
        raise ParityLedgerError(
            f"{label} must be one of {', '.join(choices)}, got {value!r}"
        )
    return value


def _backend(value: Any, label: str) -> str:
    if value == REFERENCE_BACKEND:
        raise ParityLedgerError(
            f"{label}: {REFERENCE_BACKEND} is the parity reference, not a candidate backend"
        )
    if not isinstance(value, str) or value not in BACKENDS:
        shown = _rust_debug_str(value) if isinstance(value, str) else repr(value)
        raise ParityLedgerError(
            f"{label}: unknown parity backend {shown}; expected one of dbt, kvm, liteinst, sabre"
        )
    return value


def _segment(value: Any, label: str) -> str:
    text = _string(value, label)
    if not text or "/" in text or text in {".", ".."} or "\0" in text or "__" in text:
        raise ParityLedgerError(f"{label} must be a single path segment, got {text!r}")
    return text


def _kind(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "a boolean"
    if isinstance(value, (int, float)):
        return f"the number {value!r}"
    if isinstance(value, str):
        return f"the string {value!r}"
    return f"a {type(value).__name__}"


def _sha256_hex(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def contains_owner_path(value: Any) -> bool:
    if isinstance(value, str):
        return _OWNER_PATH_NEEDLE in value
    if isinstance(value, Mapping):
        return any(contains_owner_path(item) for item in value.values())
    if isinstance(value, (list, tuple)):
        return any(contains_owner_path(item) for item in value)
    return False


# ---------------------------------------------------------------------- credit


def credit(matched_prefix: int, left_len: int, right_len: int) -> float | None:
    """hermit's `parity::credit`: the matched prefix over the longer stream.

    Full credit only when every message on both sides matched. A divergence
    never rounds up to 1.0. None when nothing was compared: unmeasured is
    never 0.
    """
    longer = max(left_len, right_len)
    if longer == 0:
        return None
    matched = min(matched_prefix, min(left_len, right_len))
    if matched == longer:
        return 1.0
    return min(max(matched / longer, 0.0), BELOW_ONE)


def unavailable_class(verdict: str, reason: str | None) -> str | None:
    """hermit's `unavailable_class`: why a cell that is neither measured nor
    record-missing was not measured."""
    if verdict in MEASURED_VERDICTS or verdict == "record-missing":
        return None
    if verdict == "inputs-not-equalized":
        return "inputs-not-equalized"
    by_verdict = {
        "reference-missing": "reference-missing",
        "candidate-missing": "candidate-missing",
        "unavailable": "other",
    }[verdict]
    parsed = _operand_reason(reason) if reason is not None else None
    if parsed is None:
        return by_verdict
    role, rest = parsed
    if rest.startswith("failed determinism:"):
        return "determinism-fail"
    if rest.startswith("was host-inapplicable, so it left no log"):
        return "host-inapplicable"
    if rest.startswith("ended "):
        return "operand-ended"
    if rest.startswith("retained "):
        return "log-not-retained"
    if rest == "has no result row in this run":
        return "reference-missing" if role == "reference" else "candidate-missing"
    return by_verdict


def _operand_reason(reason: str) -> tuple[str, str] | None:
    """Split `the <role> verify cell of <test> <rest>` into the role's last
    word (`candidate` or `reference`) and `rest`.

    The post-pass names a role in two words, `<backend> candidate` and
    `ptrace reference` (hermit ci/manifest-plan/src/parity.rs), so the role is
    everything before ` verify cell of `, not one word.
    """
    if not reason.startswith("the "):
        return None
    role, found, rest = reason[len("the "):].partition(" verify cell of ")
    if not found:
        return None
    _test, space, rest = rest.partition(" ")
    if not space:
        return None
    return role.rsplit(" ", 1)[-1], rest


def measured_credit(record: Mapping[str, Any]) -> float | None:
    """Clean credit if set, else the unequalized measurement, else None."""
    value = record.get("credit")
    if value is None:
        value = record.get("unequalized_credit")
    return None if value is None else float(value)


# --------------------------------------------------------------------- records


def validate_record(record: Any) -> None:
    """hermit's `ParityRecord` shape and `ParityRecord::validate`."""
    value = _exact_fields(record, RECORD_FIELDS, "parity record")
    test_id = _string(value["test_id"], "parity record test_id")
    backend = _backend(value["backend"], f"parity record {test_id}")
    at = f"parity record {test_id}@{backend}"
    schema = _count(value["schema"], f"{at}: schema")
    verdict = _choice(value["verdict"], RECORD_VERDICTS, f"{at}: verdict")
    inputs_equalized = _boolean(value["inputs_equalized"], f"{at}: inputs_equalized")
    reason = _optional_string(value["reason"], f"{at}: reason")
    clean = _optional_float(value["credit"], f"{at}: credit")
    unequalized = _optional_float(value["unequalized_credit"], f"{at}: unequalized_credit")
    first_record = _optional_count(value["first_divergent_record"], f"{at}: first_divergent_record")
    left = _optional_count(value["left_len"], f"{at}: left_len")
    right = _optional_count(value["right_len"], f"{at}: right_len")
    prefix = _optional_count(value["matched_prefix"], f"{at}: matched_prefix")
    difference = value["first_difference"]
    if difference is not None:
        fields = _exact_fields(difference, FIRST_DIFFERENCE_FIELDS, f"{at}: first_difference")
        _optional_string(fields["field"], f"{at}: first_difference.field")
        for name in ("syscall", "scheduler_turn", "virtual_nanoseconds"):
            _optional_count(fields[name], f"{at}: first_difference.{name}")
        for name in ("reference_message", "candidate_message"):
            _optional_string(fields[name], f"{at}: first_difference.{name}")
    reference_log = _optional_string(value["reference_log"], f"{at}: reference_log")
    candidate_log = _optional_string(value["candidate_log"], f"{at}: candidate_log")
    run_id = _string(value["run_id"], f"{at}: run_id")
    hermit_sha = _string(value["hermit_sha"], f"{at}: hermit_sha")

    if schema != RECORD_SCHEMA:
        raise ParityLedgerError(f"{at}: schema must be {RECORD_SCHEMA}, got {schema}")
    if not test_id.strip() or not run_id.strip():
        raise ParityLedgerError(f"{at}: test_id and run_id must be non-empty")
    if _SHA1_RE.fullmatch(hermit_sha) is None:
        raise ParityLedgerError(
            f"{at}: hermit_sha must be 40 lowercase hex digits, got {_rust_debug_str(hermit_sha)}"
        )
    for amount in (clean, unequalized):
        if amount is not None and not 0.0 <= amount <= 1.0:
            raise ParityLedgerError(f"{at}: credit {_rust_display(amount)} is outside [0, 1]")
    if clean is not None and unequalized is not None:
        raise ParityLedgerError(f"{at}: credit and unequalized_credit are exclusive")
    if clean is not None and not inputs_equalized:
        raise ParityLedgerError(
            f"{at}: credit is clean credit and needs equalized inputs; this measurement "
            "belongs in unequalized_credit"
        )
    if unequalized is not None and inputs_equalized:
        raise ParityLedgerError(f"{at}: unequalized_credit is set but the inputs were equalized")
    measured = verdict in MEASURED_VERDICTS
    if backend == "dbt":
        if inputs_equalized or measured:
            raise ParityLedgerError(
                f"{at}: {backend} inputs cannot be equalized, so it reports no measured "
                f"comparison and never equal inputs: {DBT_INPUTS_NOT_EQUALIZABLE}"
            )
    elif verdict == "inputs-not-equalized":
        raise ParityLedgerError(
            f"{at}: {backend} inputs can be equalized; a comparison with unequal inputs is "
            "measured and reports unequalized_credit"
        )
    amount = clean if clean is not None else unequalized
    if measured:
        if reason is not None:
            raise ParityLedgerError(f"{at}: a measured verdict carries no reason")
        if left is None or right is None:
            raise ParityLedgerError(f"{at}: a measured verdict needs both lengths")
        if reference_log is None or candidate_log is None:
            raise ParityLedgerError(f"{at}: a measured verdict needs both log paths")
        if prefix is not None and prefix > min(left, right):
            raise ParityLedgerError(
                f"{at}: matched prefix {prefix} exceeds the shorter compared stream of "
                f"{left} | {right}"
            )
        if verdict == "matched" and (first_record is not None or difference is not None):
            raise ParityLedgerError(f"{at}: a match carries no divergence position")
        expected = None if prefix is None else credit(prefix, left, right)
        if amount != expected:
            raise ParityLedgerError(
                f"{at}: credit {_rust_debug_option(amount)} disagrees with matched prefix "
                f"{_rust_debug_option(prefix)} of {left} | {right}"
            )
        if verdict == "matched" and amount != 1.0:
            raise ParityLedgerError(
                f"{at}: a match must be full credit, got {_rust_debug_option(amount)}"
            )
        if verdict == "diverged" and amount == 1.0:
            raise ParityLedgerError(f"{at}: a divergence cannot be full credit")
        return
    name = _camel(verdict)
    if reason is None or not reason.strip():
        raise ParityLedgerError(f"{at}: {name} needs a reason")
    if amount is not None or prefix is not None or first_record is not None or difference is not None:
        raise ParityLedgerError(f"{at}: {name} is unmeasured and carries no credit or divergence")
    if left is not None or right is not None:
        raise ParityLedgerError(f"{at}: {name} is unmeasured and carries no compared lengths")
    if (verdict == "reference-missing" and reference_log is not None) or (
        verdict == "candidate-missing" and candidate_log is not None
    ):
        raise ParityLedgerError(f"{at}: {name} names the log it says is missing")


# -------------------------------------------------------------- source and row


def validate_origin(origin: Any, at: str) -> Mapping[str, Any]:
    value = _exact_fields(origin, ORIGIN_FIELDS, f"{at}: source")
    _string(value["lane"], f"{at}: source.lane")
    _string(value["node"], f"{at}: source.node")
    _choice(value["post_pass_state"], POST_PASS_STATES, f"{at}: source.post_pass_state")
    for name in ("status_sha256", "records_sha256", "hermit_bin_sha256"):
        _optional_sha256(value[name], f"{at}: source.{name}")
    _choice(value["scope_source"], SCOPE_SOURCES, f"{at}: source.scope_source")
    return value


def _check_ledger_fields(
    at: str,
    cell: str,
    test_id: str,
    backend: str,
    verdict: str,
    reason: str | None,
    run_id: str | None,
    hermit_sha: str | None,
    origin: Mapping[str, Any],
    record: Any,
) -> None:
    """hermit's `check_ledger_fields`, check for check and message for message."""
    if cell != f"{test_id}@{backend}":
        raise ParityLedgerError(f"{at}: cell {_rust_debug_str(cell)} is not {test_id}@{backend}")
    if not origin["lane"] or not origin["node"]:
        raise ParityLedgerError(f"{at}: source lane and node must be non-empty")
    if verdict not in MEASURED_VERDICTS and (reason is None or not reason.strip()):
        raise ParityLedgerError(f"{at}: verdict {verdict} needs a reason")
    if origin["post_pass_state"] == "absent" and verdict != "record-missing":
        raise ParityLedgerError(
            f"{at}: a node that left no status can only owe record-missing rows, got {verdict}"
        )
    if verdict == "record-missing":
        if record is not None:
            raise ParityLedgerError(f"{at}: a record-missing row carries no record")
        return
    if record is None:
        raise ParityLedgerError(f"{at}: verdict {verdict} needs its record")
    try:
        validate_record(record)
    except ParityLedgerError as error:
        raise ParityLedgerError(f"{at}: {error}") from error
    if record["verdict"] != verdict:
        raise ParityLedgerError(
            f"{at}: row verdict {verdict} disagrees with record verdict {record['verdict']}"
        )
    if record["test_id"] != test_id or record["backend"] != backend:
        raise ParityLedgerError(f"{at}: record names {record['test_id']}@{record['backend']}")
    if record["reason"] != reason:
        raise ParityLedgerError(f"{at}: row reason disagrees with the record's")
    if run_id is not None and run_id != record["run_id"]:
        raise ParityLedgerError(
            f"{at}: row run_id {_rust_debug_str(run_id)} disagrees with record run_id "
            f"{_rust_debug_str(record['run_id'])}"
        )
    if hermit_sha is not None and hermit_sha != record["hermit_sha"]:
        raise ParityLedgerError(
            f"{at}: row hermit_sha {_rust_debug_str(hermit_sha)} disagrees with record "
            f"hermit_sha {_rust_debug_str(record['hermit_sha'])}"
        )


def validate_source(source: Any) -> Mapping[str, Any]:
    """One `ParityLedgerSource` as hermit sends it on stdin."""
    value = _exact_fields(source, SOURCE_FIELDS, "parity ledger source")
    cell = _string(value["cell"], "parity ledger source cell")
    at = f"parity ledger source {cell}"
    test_id = _string(value["test_id"], f"{at}: test_id")
    backend = _backend(value["backend"], at)
    verdict = _choice(value["verdict"], LEDGER_VERDICTS, f"{at}: verdict")
    reason = _optional_string(value["reason"], f"{at}: reason")
    run_id = _optional_string(value["run_id"], f"{at}: run_id")
    hermit_sha = _optional_string(value["hermit_sha"], f"{at}: hermit_sha")
    origin = validate_origin(value["source"], at)
    _check_ledger_fields(
        at, cell, test_id, backend, verdict, reason, run_id, hermit_sha, origin, value["record"]
    )
    return value


def event_id(producer: str, run_id: str, cell: str, lane: str, node: str) -> str:
    """`sha256(producer \\0 run_id \\0 cell \\0 lane \\0 node)`, hermit's
    `parity_event_id`."""
    return _sha256_hex(f"{producer}\0{run_id}\0{cell}\0{lane}\0{node}")


def run_event_id(producer: str, run_id: str) -> str:
    """The run marker's identity; one marker per (producer, run)."""
    return _sha256_hex(f"{RUN_EVENT_TYPE}\0{producer}\0{run_id}")


def _envelope_fields(value: Mapping[str, Any], at: str) -> None:
    _segment(value["team"], f"{at}: team")
    _segment(value["host"], f"{at}: host")
    emitted_at = _string(value["emitted_at"], f"{at}: emitted_at")
    if _EMITTED_AT_RE.fullmatch(emitted_at) is None:
        raise ParityLedgerError(f"{at}: emitted_at must be YYYY-MM-DDTHH:MM:SSZ, got {emitted_at!r}")
    _choice(value["producer"], PRODUCERS, f"{at}: producer")
    _boolean(value["source_tree_dirty"], f"{at}: source_tree_dirty")


def validate_row(row: Any) -> Mapping[str, Any]:
    """One published `ParityLedgerRow` and `ParityLedgerRow::validate`."""
    value = _exact_fields(row, ROW_FIELDS, "parity ledger row")
    cell = _string(value["cell"], "parity ledger row cell")
    producer = _string(value["producer"], f"parity ledger row {cell}: producer")
    run_id = _string(value["run_id"], f"parity ledger row {cell}: run_id")
    at = f"parity ledger row {cell} ({producer} run {run_id})"
    schema = _string(value["schema"], f"{at}: schema")
    event_type = _string(value["event_type"], f"{at}: event_type")
    if schema != SCHEMA:
        raise ParityLedgerError(f"{at}: schema must be {SCHEMA}, got {_rust_debug_str(schema)}")
    if event_type != ROW_EVENT_TYPE:
        raise ParityLedgerError(
            f"{at}: event_type must be {ROW_EVENT_TYPE}, got {_rust_debug_str(event_type)}"
        )
    _envelope_fields(value, at)
    if not run_id.strip():
        raise ParityLedgerError(f"{at}: run_id must be non-empty")
    hermit_sha = _string(value["hermit_sha"], f"{at}: hermit_sha")
    if _SHA1_RE.fullmatch(hermit_sha) is None:
        raise ParityLedgerError(
            f"{at}: hermit_sha must be 40 lowercase hex digits, got {_rust_debug_str(hermit_sha)}"
        )
    test_id = _string(value["test_id"], f"{at}: test_id")
    backend = _backend(value["backend"], at)
    verdict = _choice(value["verdict"], LEDGER_VERDICTS, f"{at}: verdict")
    reason = _optional_string(value["reason"], f"{at}: reason")
    origin = validate_origin(value["source"], at)
    stated = _string(value["event_id"], f"{at}: event_id")
    expected = event_id(producer, run_id, cell, origin["lane"], origin["node"])
    if stated != expected:
        raise ParityLedgerError(
            f"{at}: event_id {stated} is not sha256(producer, run_id, cell, lane, node) = {expected}"
        )
    _check_ledger_fields(
        at, cell, test_id, backend, verdict, reason, run_id, hermit_sha, origin, value["record"]
    )
    return value


def _node_key(origin: Mapping[str, Any]) -> tuple[str, str]:
    return str(origin["lane"]), str(origin["node"])


def _verdict_counts(rows: Iterable[Mapping[str, Any]]) -> dict[str, int]:
    counts = {verdict: 0 for verdict in LEDGER_VERDICTS}
    for row in rows:
        counts[row["verdict"]] += 1
    return counts


def _run_nodes(rows: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    """One entry per node: its origin (identical on every row) and row count."""
    nodes: dict[tuple[str, str], dict[str, Any]] = {}
    for row in rows:
        origin = row["source"]
        key = _node_key(origin)
        known = nodes.get(key)
        if known is None:
            nodes[key] = {**{field: origin[field] for field in ORIGIN_FIELDS}, "records": 1}
            continue
        if any(known[field] != origin[field] for field in ORIGIN_FIELDS):
            raise ParityLedgerError(
                f"parity rows for node {key[0]}/{key[1]} disagree about the node's origin"
            )
        known["records"] += 1
    return [nodes[key] for key in sorted(nodes)]


def validate_run(marker: Any) -> Mapping[str, Any]:
    """One `parity.run` marker: what one run appended, and from which nodes."""
    value = _exact_fields(marker, RUN_FIELDS, "parity run marker")
    producer = _string(value["producer"], "parity run marker producer")
    run_id = _string(value["run_id"], "parity run marker run_id")
    at = f"parity run marker ({producer} run {run_id})"
    if value["schema"] != SCHEMA:
        raise ParityLedgerError(f"{at}: schema must be {SCHEMA}, got {value['schema']!r}")
    if value["event_type"] != RUN_EVENT_TYPE:
        raise ParityLedgerError(
            f"{at}: event_type must be {RUN_EVENT_TYPE}, got {value['event_type']!r}"
        )
    _envelope_fields(value, at)
    if not run_id.strip():
        raise ParityLedgerError(f"{at}: run_id must be non-empty")
    hermit_sha = _string(value["hermit_sha"], f"{at}: hermit_sha")
    if _SHA1_RE.fullmatch(hermit_sha) is None:
        raise ParityLedgerError(f"{at}: hermit_sha must be 40 lowercase hex digits, got {hermit_sha!r}")
    expected = run_event_id(producer, run_id)
    if value["event_id"] != expected:
        raise ParityLedgerError(
            f"{at}: event_id {value['event_id']!r} is not sha256(parity.run, producer, run_id) = {expected}"
        )
    records = _count(value["records"], f"{at}: records")
    verdicts = _exact_fields(value["verdicts"], LEDGER_VERDICTS, f"{at}: verdicts")
    if sum(_count(verdicts[name], f"{at}: verdicts.{name}") for name in LEDGER_VERDICTS) != records:
        raise ParityLedgerError(f"{at}: verdict counts do not sum to its {records} record(s)")
    nodes = value["nodes"]
    if not isinstance(nodes, list):
        raise ParityLedgerError(f"{at}: nodes must be a list")
    keys = []
    for index, node in enumerate(nodes):
        entry = _exact_fields(node, RUN_NODE_FIELDS, f"{at}: nodes[{index}]")
        validate_origin({field: entry[field] for field in ORIGIN_FIELDS}, f"{at}: nodes[{index}]")
        if not entry["lane"] or not entry["node"]:
            raise ParityLedgerError(f"{at}: nodes[{index}] lane and node must be non-empty")
        if _count(entry["records"], f"{at}: nodes[{index}].records") == 0:
            raise ParityLedgerError(f"{at}: nodes[{index}] names a node with no rows")
        keys.append(_node_key(entry))
    if keys != sorted(set(keys)):
        raise ParityLedgerError(f"{at}: nodes must be unique and sorted by lane and node")
    if sum(node["records"] for node in nodes) != records:
        raise ParityLedgerError(f"{at}: node record counts do not sum to its {records} record(s)")
    return value


def validate_batch(events: Sequence[Any], label: str = "parity batch") -> tuple[dict, list[dict]]:
    """One run's batch: its marker first, then every row of that run.

    Returns the marker and the rows. The marker's counts must be the rows'
    counts, every row must share the marker's run fields, and a node's rows
    must agree about the node's origin.
    """
    if not events:
        raise ParityLedgerError(f"{label}: a batch holds at least its run marker")
    marker = events[0]
    if not isinstance(marker, Mapping) or marker.get("event_type") != RUN_EVENT_TYPE:
        raise ParityLedgerError(f"{label}: the first event must be the {RUN_EVENT_TYPE} marker")
    try:
        validate_run(marker)
    except ParityLedgerError as error:
        raise ParityLedgerError(f"{label}: {error}") from error
    rows = list(events[1:])
    seen: set[str] = set()
    for index, row in enumerate(rows, 2):
        if isinstance(row, Mapping) and row.get("event_type") == RUN_EVENT_TYPE:
            raise ParityLedgerError(f"{label}: event {index} is a second run marker")
        try:
            validate_row(row)
        except ParityLedgerError as error:
            raise ParityLedgerError(f"{label}: event {index}: {error}") from error
        for field in SHARED_FIELDS:
            if row[field] != marker[field]:
                raise ParityLedgerError(
                    f"{label}: event {index} ({row['cell']}) has {field} {row[field]!r} but its "
                    f"run marker has {marker[field]!r}"
                )
        if row["event_id"] in seen:
            raise ParityLedgerError(
                f"{label}: event {index} repeats {row['cell']} from node "
                f"{row['source']['lane']}/{row['source']['node']}"
            )
        seen.add(row["event_id"])
    try:
        nodes = _run_nodes(rows)
    except ParityLedgerError as error:
        raise ParityLedgerError(f"{label}: {error}") from error
    if marker["records"] != len(rows):
        raise ParityLedgerError(
            f"{label}: run marker says {marker['records']} record(s) but the batch holds {len(rows)}"
        )
    counts = _verdict_counts(rows)
    if dict(marker["verdicts"]) != counts:
        raise ParityLedgerError(f"{label}: run marker verdict counts disagree with its rows")
    if [dict(node) for node in marker["nodes"]] != nodes:
        raise ParityLedgerError(f"{label}: run marker nodes disagree with its rows")
    return dict(marker), [dict(row) for row in rows]


# ---------------------------------------------------------------------- writer


def build_batch(
    sources: Sequence[Any],
    *,
    producer: str,
    run_id: str,
    hermit_sha: str,
    emitted_at: str,
    source_tree_dirty: bool,
    host: str,
    team: str = TEAM,
) -> list[dict]:
    """Wrap one run's sources: the run marker, then one row per source.

    A source whose own run_id or hermit_sha (from its node's status) differs
    from the run's is refused, as is anything carrying an owner path: the
    ledger is public.
    """
    _choice(producer, PRODUCERS, "--producer")
    if not isinstance(run_id, str) or not run_id.strip() or "\0" in run_id:
        raise ParityLedgerError(f"--run-id must be non-empty, got {run_id!r}")
    if not isinstance(hermit_sha, str) or _SHA1_RE.fullmatch(hermit_sha) is None:
        raise ParityLedgerError(f"--tree must be 40 lowercase hex digits, got {hermit_sha!r}")
    if not isinstance(emitted_at, str) or _EMITTED_AT_RE.fullmatch(emitted_at) is None:
        raise ParityLedgerError(f"--emitted-at must be YYYY-MM-DDTHH:MM:SSZ, got {emitted_at!r}")
    _segment(team, "team")
    _segment(host, "host")
    _boolean(source_tree_dirty, "source_tree_dirty")
    rows: list[dict] = []
    for index, source in enumerate(sources, 1):
        try:
            value = validate_source(source)
        except ParityLedgerError as error:
            raise ParityLedgerError(f"input row {index}: {error}") from error
        at = f"input row {index}: parity ledger source {value['cell']}"
        if value["run_id"] is not None and value["run_id"] != run_id:
            raise ParityLedgerError(
                f"{at}: its node's run_id {value['run_id']!r} is not --run-id {run_id!r}"
            )
        if value["hermit_sha"] is not None and value["hermit_sha"] != hermit_sha:
            raise ParityLedgerError(
                f"{at}: its node's hermit_sha {value['hermit_sha']!r} is not --tree {hermit_sha!r}"
            )
        if contains_owner_path(value):
            raise ParityLedgerError(
                f"{at}: contains a workspace-local owner path; the parity ledger is public"
            )
        origin = value["source"]
        row = {
            "schema": SCHEMA,
            "event_type": ROW_EVENT_TYPE,
            "event_id": event_id(producer, run_id, value["cell"], origin["lane"], origin["node"]),
            "team": team,
            "host": host,
            "emitted_at": emitted_at,
            "producer": producer,
            "run_id": run_id,
            "hermit_sha": hermit_sha,
            "source_tree_dirty": source_tree_dirty,
            "cell": value["cell"],
            "test_id": value["test_id"],
            "backend": value["backend"],
            "verdict": value["verdict"],
            "reason": value["reason"],
            "source": {field: origin[field] for field in ORIGIN_FIELDS},
            "record": _canonical_record(value["record"]),
        }
        rows.append(canonical_event(row))
    marker = {
        "schema": SCHEMA,
        "event_type": RUN_EVENT_TYPE,
        "event_id": run_event_id(producer, run_id),
        "team": team,
        "host": host,
        "emitted_at": emitted_at,
        "producer": producer,
        "run_id": run_id,
        "hermit_sha": hermit_sha,
        "source_tree_dirty": source_tree_dirty,
        "records": len(rows),
        "verdicts": _verdict_counts(rows),
        "nodes": _run_nodes(rows),
    }
    events = [marker, *rows]
    validate_batch(events, "parity batch being written")
    return events


def batch_name(marker: Mapping[str, Any]) -> str:
    """`<team>__<host>__<YYYY-MM>__<run event id>.jsonl`: one file per run."""
    return (
        f"{marker['team']}__{marker['host']}__{str(marker['emitted_at'])[:7]}__"
        f"{marker['event_id']}.jsonl"
    )


def shard_path(root: str, team: str, host: str, month: str) -> str:
    if root not in (ROW_ROOT, RUN_ROOT):
        raise ParityLedgerError(f"unknown parity tree {root!r}")
    if re.fullmatch(r"\d{4}-\d{2}", month or "") is None:
        raise ParityLedgerError(f"month must be YYYY-MM, got {month!r}")
    _segment(team, "team")
    _segment(host, "host")
    return f"{root}/{team}/{host}/{month}.jsonl"


def event_shard(event: Mapping[str, Any]) -> str:
    root = RUN_ROOT if event.get("event_type") == RUN_EVENT_TYPE else ROW_ROOT
    return shard_path(root, event["team"], event["host"], str(event["emitted_at"])[:7])


def encode_batch(events: Sequence[Mapping[str, Any]]) -> str:
    return "".join(canonical_line(event) + "\n" for event in events)


def parse_lines(text: str, label: str) -> list[dict]:
    """Events of one JSONL text, each line required in its canonical encoding."""
    if text and not text.endswith("\n"):
        raise ParityLedgerError(f"{label}: final line has no newline terminator")
    events = []
    for number, line in enumerate(text.split("\n")[:-1] if text else [], 1):
        if not line.strip():
            raise ParityLedgerError(f"{label}:{number}: blank line")
        value = loads(line, f"{label}:{number}")
        if not isinstance(value, dict):
            raise ParityLedgerError(f"{label}:{number}: not a JSON object")
        if canonical_line(value) != line:
            raise ParityLedgerError(f"{label}:{number}: not the canonical compact encoding")
        events.append(value)
    return events


def read_batch(path: Path) -> tuple[dict, list[dict]]:
    """One spooled batch file, fully validated."""
    try:
        text = Path(path).read_bytes().decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise ParityLedgerError(f"{path.name}: cannot read: {error}") from error
    events = parse_lines(text, path.name)
    marker, rows = validate_batch(events, path.name)
    if batch_name(marker) != path.name:
        raise ParityLedgerError(f"{path.name}: its run marker names the batch {batch_name(marker)}")
    return marker, rows


def batch_paths(spool: Path) -> list[Path]:
    return sorted(Path(spool).glob("*.jsonl"))


def without_emitted_at(event: Mapping[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in event.items() if key != "emitted_at"}


def same_event(left: Mapping[str, Any], right: Mapping[str, Any]) -> bool:
    """Equal bytes, ignoring only when the event was written."""
    return canonical_line(without_emitted_at(left)) == canonical_line(without_emitted_at(right))


def _first_difference(existing: Mapping[str, dict], fresh: Sequence[dict]) -> str:
    ids = {event["event_id"] for event in fresh}
    for event in fresh:
        prior = existing.get(event["event_id"])
        if prior is None:
            return f"{_describe(event)} is new, but part of the run is already recorded"
        if not same_event(prior, event):
            return f"{_describe(event)} differs from the recorded copy"
    for event_id_value, prior in existing.items():
        if event_id_value not in ids:
            return f"{_describe(prior)} is recorded but not in this batch"
    return "no difference"


def _describe(event: Mapping[str, Any]) -> str:
    if event.get("event_type") == RUN_EVENT_TYPE:
        return (f"run marker of {event.get('producer')} run {event.get('run_id')} "
                f"({event.get('records')} record(s))")
    return f"row {event.get('cell')} from node {event.get('source', {}).get('lane')}/{event.get('source', {}).get('node')}"


def recorded_run_events(
    spool: Path, run_id_value: str, *, published: Mapping[str, dict] | None = None,
) -> tuple[dict[str, dict], list[str]]:
    """Every already-recorded event of one run: pending, retained, published.

    A run's events all live in the one batch named by its run event id, so
    only that file is read from the spool and its `published/` directory.
    """
    found: dict[str, dict] = {}
    where: list[str] = []
    for directory in (Path(spool), Path(spool) / "published"):
        for path in sorted(directory.glob(f"*__{run_id_value}.jsonl")):
            marker, rows = read_batch(path)
            for event in (marker, *rows):
                found.setdefault(event["event_id"], event)
            where.append(str(path.relative_to(Path(spool))))
    return found, where


def append_parity(
    spool: Path,
    sources: Sequence[Any],
    *,
    producer: str,
    run_id: str,
    hermit_sha: str,
    emitted_at: str,
    source_tree_dirty: bool,
    host: str,
    published: Mapping[str, dict] | None = None,
    published_check: str | None = None,
) -> dict[str, Any]:
    """Validate one run's sources and spool its batch; caller holds the lock.

    Replaying a run whose rows are already recorded (pending, retained, or in
    `published`) with only a different `emitted_at` writes nothing. Any other
    difference is refused: the ledger is append-only and a changed record is
    never replaced.
    """
    events = build_batch(
        sources, producer=producer, run_id=run_id, hermit_sha=hermit_sha,
        emitted_at=emitted_at, source_tree_dirty=source_tree_dirty, host=host,
    )
    marker = events[0]
    existing, where = recorded_run_events(spool, marker["event_id"])
    if published:
        for event in events:
            prior = published.get(event["event_id"])
            if prior is not None:
                existing.setdefault(event["event_id"], prior)
        if any(event["event_id"] in published for event in events):
            where.append("the published ledger")
    result = {
        "schema": "parity-append/v1",
        "producer": producer,
        "run_id": run_id,
        "hermit_sha": hermit_sha,
        "records": marker["records"],
        "verdicts": dict(marker["verdicts"]),
        "batch": None,
        "already_recorded": False,
        "published_check": published_check,
    }
    if existing:
        fresh_ids = {event["event_id"] for event in events}
        if set(existing) == fresh_ids and all(
            same_event(existing[event["event_id"]], event) for event in events
        ):
            result["already_recorded"] = True
            result["recorded_in"] = where
            return result
        raise ParityLedgerError(
            f"{producer} run {run_id} is already recorded in {', '.join(where)} with different "
            f"content: {_first_difference(existing, events)}; the parity ledger is append-only, "
            "so a changed record is refused rather than replaced"
        )
    spool = Path(spool)
    spool.mkdir(parents=True, exist_ok=True)
    name = batch_name(marker)
    target = spool / name
    temporary = spool / f".{name}.{os.getpid()}.tmp"
    data = encode_batch(events).encode("utf-8")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o644)
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            view = view[written:]
        os.fsync(descriptor)
    except BaseException:
        os.close(descriptor)
        temporary.unlink(missing_ok=True)
        raise
    os.close(descriptor)
    # Publishers glob only `*.jsonl`, so the batch appears whole or not at all.
    os.replace(temporary, target)
    read_batch(target)
    result["batch"] = name
    return result


# -------------------------------------------------------------------- publish


@dataclass(frozen=True)
class PublishedParity:
    """Every parity event at one ledger commit, and the shard texts read."""

    commit: str
    events: dict[str, dict]
    texts: dict[str, str]


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    env = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    return subprocess.run(
        ["git", "--no-replace-objects", "-C", str(repo), *args],
        capture_output=True, text=True, timeout=120, check=False, env=env,
    )


def shard_events(text: str, label: str) -> list[dict]:
    """Events of one published shard: markers or rows, each validated."""
    events = parse_lines(text, label)
    for index, event in enumerate(events, 1):
        try:
            if event.get("event_type") == RUN_EVENT_TYPE:
                validate_run(event)
            else:
                validate_row(event)
        except ParityLedgerError as error:
            raise ParityLedgerError(f"{label}:{index}: {error}") from error
    return events


def published_parity_at_repo(repo: Path, ref: str) -> PublishedParity:
    """Read `parity/` and `parity-runs/` at one ledger ref, validated."""
    resolved = _git(Path(repo), "rev-parse", "--verify", f"{ref}^{{commit}}")
    commit = resolved.stdout.strip()
    if resolved.returncode != 0 or _SHA1_RE.fullmatch(commit) is None:
        raise ParityLedgerError(
            f"cannot resolve published parity revision {ref}: {resolved.stderr.strip() or 'no output'}"
        )
    listed = _git(Path(repo), "ls-tree", "-r", "--name-only", commit, "--", ROW_ROOT, RUN_ROOT)
    if listed.returncode != 0:
        raise ParityLedgerError(
            f"cannot list parity shards at {commit}: {listed.stderr.strip() or 'no output'}"
        )
    events: dict[str, dict] = {}
    texts: dict[str, str] = {}
    for relative in listed.stdout.splitlines():
        parts = Path(relative).parts
        if len(parts) != 4 or parts[0] not in (ROW_ROOT, RUN_ROOT) or not relative.endswith(".jsonl"):
            raise ParityLedgerError(f"{commit}:{relative} is not a parity shard path")
        shown = _git(Path(repo), "show", f"{commit}:{relative}")
        if shown.returncode != 0:
            raise ParityLedgerError(
                f"cannot read {commit}:{relative}: {shown.stderr.strip() or 'no output'}"
            )
        texts[relative] = shown.stdout
        for event in shard_events(shown.stdout, f"{commit}:{relative}"):
            if event_shard(event) != relative:
                raise ParityLedgerError(f"{commit}:{relative}: {_describe(event)} belongs in {event_shard(event)}")
            prior = events.get(event["event_id"])
            if prior is not None and canonical_line(prior) != canonical_line(event):
                raise ParityLedgerError(
                    f"{commit}:{relative}: event_id {event['event_id']} is published twice with different bodies"
                )
            events[event["event_id"]] = event
    return PublishedParity(commit=commit, events=events, texts=texts)


@dataclass
class PublicationPlan:
    """What one publication attempt appends, and what it cannot."""

    texts: dict[str, str]
    appended: int
    fresh_batches: list[Path]
    already_published: list[Path]
    refused: list[dict[str, str]]
    #: The published text of every shard in ``texts``, which each merged text
    #: must extend line for line.
    base_texts: dict[str, str]


def plan_publication(batches: Sequence[Path], published: PublishedParity) -> PublicationPlan:
    """Merge every pending batch that can be published onto `published`.

    A batch is refused (and left spooled) when it is unreadable, when one of
    its events is already published with a different body, when only part of
    it is published, or when it collides with an earlier batch. A batch whose
    every event is already published, differing at most in `emitted_at`, is
    already published: the first published copy stands.
    """
    texts: dict[str, str] = {}
    fresh: list[Path] = []
    already: list[Path] = []
    refused: list[dict[str, str]] = []
    claimed: dict[str, dict] = {}
    appended = 0
    for path in batches:
        try:
            marker, rows = read_batch(path)
            events = [marker, *rows]
            prior = [published.events.get(event["event_id"]) for event in events]
            if all(item is not None for item in prior):
                changed = [event for event, item in zip(events, prior) if not same_event(item, event)]
                if changed:
                    raise ParityLedgerError(
                        f"{_describe(changed[0])} is already published with a different body"
                    )
                already.append(path)
                continue
            if any(item is not None for item in prior):
                count = sum(item is not None for item in prior)
                raise ParityLedgerError(
                    f"{count} of its {len(events)} event(s) are already published; a run is "
                    "published whole or not at all"
                )
            for event in events:
                if event["event_id"] in claimed:
                    raise ParityLedgerError(
                        f"{_describe(event)} is also in another pending batch"
                    )
        except ParityLedgerError as error:
            refused.append({"batch": Path(path).name, "error": str(error)})
            continue
        for event in events:
            claimed[event["event_id"]] = event
            relative = event_shard(event)
            if relative not in texts:
                texts[relative] = published.texts.get(relative, "")
            texts[relative] += canonical_line(event) + "\n"
        appended += len(events)
        fresh.append(path)
    return PublicationPlan(
        texts=texts, appended=appended, fresh_batches=fresh,
        already_published=already, refused=refused,
        base_texts={relative: published.texts.get(relative, "") for relative in texts},
    )


def reapable(path: Path, published: PublishedParity) -> bool:
    """A retained batch may go once the producer root reads every event."""
    try:
        marker, rows = read_batch(path)
    except ParityLedgerError:
        return False
    return all(
        published.events.get(event["event_id"]) is not None
        and same_event(published.events[event["event_id"]], event)
        for event in (marker, *rows)
    )
