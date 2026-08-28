#!/usr/bin/env python3
"""Ingest Hermit's front-door real-program matrix as one compat-envelope bucket.

The producer lives in Hermit because it exercises the user-facing ``hermit run``
front door.  The scorecard lives in dev-hermit because it is measurement state.
This adapter is the boundary between them; it never executes a guest and never
guesses missing provenance.

Input is the CSV emitted by ``ci/compat/backend-matrix.rs`` plus an atomically
written metadata JSON bound to the CSV's SHA-256.  Output uses the authoritative
compat-envelope core schema with ``bucket=real-programs``.

Matrix v1 PASS means strict *within-backend* self-verification and is therefore
``unqualified-self-verify-only``. Matrix v2 may carry a narrower real
cross-backend stdout witness per row; the importer preserves it only when both
operands re-derive the verdict. It neither manufactures nor discards evidence.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, NoReturn


MATRIX_SCHEMA_V1 = "hermit-compat-backend-matrix/v1"
MATRIX_SCHEMA_V2 = "hermit-compat-backend-matrix/v2"
MATRIX_SCHEMAS = (MATRIX_SCHEMA_V1, MATRIX_SCHEMA_V2)
METADATA_SCHEMA_V1 = "hermit-compat-backend-matrix-metadata/v1"
METADATA_SCHEMA_V2 = "hermit-compat-backend-matrix-metadata/v2"
METADATA_SCHEMAS = (METADATA_SCHEMA_V1, METADATA_SCHEMA_V2)
ANNOTATIONS_SCHEMA = "real-program-annotations/v1"
CORPUS_SCHEMA = "hermit-compat-corpus/v1"
BUCKET = "real-programs"
OUTPUT_VARIANT = "real-programs-scorecard.csv"
PRODUCT_BACKENDS = ("ptrace", "dbt", "liteinst", "sabre", "kvm", "e9patch")
LEGACY_MODES = frozenset(("strict", "sabre", "e9patch", "rr"))
SCORECARD_BACKEND = {"dbt": "dbi"}
FRONT_DOOR_PROFILE = (
    "hermit",
    "--backend",
    "<backend>",
    "run",
    "--strict",
    "--verify",
    "--verify-strict",
    "--verify-json",
    "<verdict-json>",
    "--",
    "<program-argv>",
)
STATES = frozenset(
    ("PASS", "FAIL", "TIMEOUT", "NOT_ATTEMPTED", "ATTEMPTED_UNQUALIFIABLE")
)
ATTEMPTED = STATES - {"NOT_ATTEMPTED"}

MATRIX_COLUMNS = (
    "program",
    "backend",
    "state",
    "reason",
    "bitwise_parity",
    "verified",
    "verdict",
    "compared_left",
    "compared_right",
    "comparison_strictness",
    "rc",
    "duration_ms",
    "legacy_modes",
)
MATRIX_V2_COLUMNS = MATRIX_COLUMNS + (
    "comparison_tier",
    "stdout_parity",
    "output_hash",
    "ref_output_hash",
    "parity_comparator",
    "parity_tier",
)
OUTPUT_EXTRA_COLUMNS = (
    "evaluation_state",
    "evaluation_reason",
    "evaluation_evidence",
    "source_legacy_modes",
    "source_legacy_mode_count",
    "source_legacy_coverage",
    "program_classes",
    "concurrency_kind",
    "program_class_evidence",
    "classification_id",
    "harness_form",
    "corpus_id",
)
KNOWN_SOURCE_TIERS = frozenset(
    (
        "unqualified-no-comparison",
        "unqualified-self-verify-only",
        "unqualified-stdout-only",
    )
)
SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")


class Refused(ValueError):
    """The input cannot support an honest scorecard artifact."""


def die(message: str) -> NoReturn:
    print(f"ingest-real-program-matrix: REFUSED: {message}", file=sys.stderr)
    raise SystemExit(2)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def exact_sha(value: object) -> bool:
    text = str(value)
    return len(text) == 40 and all(c in "0123456789abcdefABCDEF" for c in text)


def bool_text(value: str) -> bool | None:
    value = value.strip().lower()
    if value in ("true", "1"):
        return True
    if value in ("false", "0"):
        return False
    if value == "":
        return None
    raise Refused(f"illegal boolean value {value!r}")


def positive_int(value: str) -> int | None:
    if value.strip() == "":
        return None
    try:
        out = int(value)
    except ValueError as error:
        raise Refused(f"expected integer, got {value!r}") from error
    return out


def bool_output(value: bool | None) -> str:
    return "1" if value is True else "0" if value is False else ""


def scorecard_columns(schema_path: Path) -> list[str]:
    try:
        schema = json.loads(schema_path.read_text(encoding="utf-8"))
        core = schema["core"]
        variant = schema["variants"][OUTPUT_VARIANT]
        columns = variant["columns"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise Refused(
            f"cannot read authoritative scorecard schema {schema_path}: {error}"
        ) from error
    if not isinstance(core, list) or not core or not all(isinstance(c, str) for c in core):
        raise Refused(f"{schema_path}: core columns are not a non-empty string list")
    if columns != core + list(OUTPUT_EXTRA_COLUMNS):
        raise Refused(
            f"{schema_path}: {OUTPUT_VARIANT} must be authoritative core plus "
            f"{list(OUTPUT_EXTRA_COLUMNS)!r}"
        )
    return columns


@dataclass(frozen=True)
class Metadata:
    run_id: str
    run_utc: str
    hermit_sha: str
    reverie_sha: str
    profile_flags: str
    matrix_schema: str
    evaluation_state: str
    evaluation_reason: str
    evaluation_evidence: str


@dataclass(frozen=True)
class Annotation:
    classes: tuple[str, ...]
    concurrency_kind: str
    evidence: str


@dataclass(frozen=True)
class Annotations:
    by_program: dict[str, Annotation]
    classification_id: str


@dataclass(frozen=True)
class CorpusProgram:
    modes: frozenset[str]
    harness_form: str


@dataclass(frozen=True)
class Corpus:
    programs: dict[str, CorpusProgram]
    corpus_id: str


def read_metadata(path: Path, matrix: Path, corpus: Path) -> Metadata:
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise Refused(f"cannot read metadata {path}: {error}") from error
    metadata_schema = doc.get("schema")
    if metadata_schema not in METADATA_SCHEMAS:
        raise Refused(
            f"metadata schema is {metadata_schema!r}, expected one of "
            f"{METADATA_SCHEMAS!r}"
        )
    expected_digest = str(doc.get("matrix_sha256", "")).lower()
    observed_digest = sha256(matrix)
    if expected_digest != observed_digest:
        raise Refused(
            f"metadata matrix_sha256={expected_digest or '<blank>'} does not bind "
            f"{matrix} sha256={observed_digest}"
        )
    expected_corpus_digest = str(doc.get("corpus_sha256", "")).lower()
    observed_corpus_digest = sha256(corpus)
    if expected_corpus_digest != observed_corpus_digest:
        raise Refused(
            f"metadata corpus_sha256={expected_corpus_digest or '<blank>'} does not bind "
            f"{corpus} sha256={observed_corpus_digest}"
        )
    matrix_schema = str(doc.get("matrix_schema", ""))
    if matrix_schema not in MATRIX_SCHEMAS:
        raise Refused(
            f"matrix schema is {matrix_schema!r}, expected one of {MATRIX_SCHEMAS!r}"
        )
    if doc.get("dirty") is not False:
        raise Refused("matrix producer was dirty; exact source provenance is required")
    for name in ("hermit_sha", "reverie_sha"):
        if not exact_sha(doc.get(name, "")):
            raise Refused(f"metadata {name} is not an exact 40-hex SHA")
    for name in ("run_id", "run_utc"):
        if not str(doc.get(name, "")).strip():
            raise Refused(f"metadata {name} is blank")
    profile = doc.get("profile_flags")
    if not isinstance(profile, dict):
        raise Refused("metadata profile_flags must be a JSON object")
    front_door = profile.get("front_door")
    if front_door != list(FRONT_DOOR_PROFILE):
        raise Refused(
            "metadata profile_flags.front_door does not name the exact shared user-facing "
            f"invocation: expected {list(FRONT_DOOR_PROFILE)!r}"
        )
    if metadata_schema == METADATA_SCHEMA_V1:
        evaluation_state = "EVALUATED"
        evaluation_reason = ""
        evaluation_evidence = "matrix-rows"
    else:
        evaluation = doc.get("evaluation")
        if not isinstance(evaluation, dict):
            raise Refused("metadata v2 evaluation must be a JSON object")
        evaluation_state = str(evaluation.get("state", ""))
        evaluation_reason = str(evaluation.get("reason", "")).strip()
        evaluation_evidence = str(evaluation.get("evidence", "")).strip()
        if evaluation_state not in ("EVALUATED", "NOT_ATTEMPTED"):
            raise Refused(
                "metadata v2 evaluation.state must be EVALUATED or NOT_ATTEMPTED"
            )
        if str(evaluation.get("head_sha", "")).lower() != str(doc["hermit_sha"]).lower():
            raise Refused("metadata evaluation does not bind the exact Hermit head SHA")
        if str(evaluation.get("observed_utc", "")) != str(doc["run_utc"]):
            raise Refused("metadata evaluation observation time differs from run_utc")
        if not evaluation_evidence:
            raise Refused("metadata evaluation.evidence is blank")
        if evaluation_state == "NOT_ATTEMPTED" and not evaluation_reason:
            raise Refused("metadata NOT_ATTEMPTED evaluation has no reason")
        if evaluation_state == "EVALUATED" and evaluation_reason:
            raise Refused("metadata EVALUATED evaluation must not carry an absence reason")
    return Metadata(
        run_id=str(doc["run_id"]),
        run_utc=str(doc["run_utc"]),
        hermit_sha=str(doc["hermit_sha"]).lower(),
        reverie_sha=str(doc["reverie_sha"]).lower(),
        profile_flags=json.dumps(profile, sort_keys=True, separators=(",", ":")),
        matrix_schema=matrix_schema,
        evaluation_state=evaluation_state,
        evaluation_reason=evaluation_reason,
        evaluation_evidence=evaluation_evidence,
    )


def read_annotations(path: Path) -> Annotations:
    try:
        raw = path.read_bytes()
        doc = json.loads(raw)
    except (OSError, json.JSONDecodeError) as error:
        raise Refused(f"cannot read annotations {path}: {error}") from error
    if doc.get("schema") != ANNOTATIONS_SCHEMA:
        raise Refused(
            f"annotations schema is {doc.get('schema')!r}, expected {ANNOTATIONS_SCHEMA!r}"
        )
    programs = doc.get("programs")
    if not isinstance(programs, dict):
        raise Refused("annotations programs must be a JSON object keyed by corpus label")
    by_program: dict[str, Annotation] = {}
    for program, value in programs.items():
        if not isinstance(program, str) or not program or not isinstance(value, dict):
            raise Refused("annotation entries require a nonblank program label and object value")
        classes = value.get("classes")
        concurrency_kind = str(value.get("concurrency_kind", ""))
        evidence = str(value.get("evidence", "")).strip()
        if not isinstance(classes, list) or not classes or not all(
            isinstance(item, str) and item for item in classes
        ):
            raise Refused(f"annotation {program}: classes must be nonblank strings")
        if concurrency_kind and "concurrency" not in classes:
            raise Refused(
                f"annotation {program}: concurrency_kind requires the concurrency class"
            )
        if not evidence:
            raise Refused(f"annotation {program}: evidence is blank")
        by_program[program] = Annotation(
            classes=tuple(sorted(set(classes))),
            concurrency_kind=concurrency_kind,
            evidence=evidence,
        )
    digest = hashlib.sha256(raw).hexdigest()
    return Annotations(by_program, f"sha256:{digest}")


def harness_form(argv: list[str]) -> str:
    if any(
        Path(arg).name in ("real_compat_workload.sh", "complex_shell_workload.sh")
        for arg in argv
    ):
        return "workload-script"
    command = list(argv)
    if command and Path(command[0]).name == "env":
        command = command[1:]
        while command and "=" in command[0] and not command[0].startswith("="):
            command = command[1:]
    if command and Path(command[0]).name in ("bash", "sh", "dash"):
        return "shell-wrapped"
    return "direct"


def read_corpus(path: Path) -> Corpus:
    try:
        raw = path.read_bytes()
        doc = json.loads(raw)
    except (OSError, json.JSONDecodeError) as error:
        raise Refused(f"cannot read corpus {path}: {error}") from error
    if doc.get("schema") != CORPUS_SCHEMA:
        raise Refused(f"corpus schema is {doc.get('schema')!r}, expected {CORPUS_SCHEMA!r}")
    entries = doc.get("programs")
    if not isinstance(entries, list) or not entries:
        raise Refused("corpus programs must be a non-empty JSON array")
    programs: dict[str, CorpusProgram] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise Refused("corpus program entry is not an object")
        label = str(entry.get("label", "")).strip()
        argv = entry.get("argv")
        modes = entry.get("modes")
        if not label or label in programs:
            raise Refused(f"corpus has blank or duplicate program label {label!r}")
        if not isinstance(argv, list) or not argv or not all(isinstance(arg, str) for arg in argv):
            raise Refused(f"corpus {label}: argv is empty or malformed")
        if not isinstance(modes, list) or not modes or not all(
            isinstance(mode, str) and mode in LEGACY_MODES for mode in modes
        ):
            raise Refused(f"corpus {label}: modes are empty or malformed")
        if len(modes) != len(set(modes)):
            raise Refused(f"corpus {label}: duplicate legacy mode")
        programs[label] = CorpusProgram(frozenset(modes), harness_form(argv))
    return Corpus(programs, f"sha256:{hashlib.sha256(raw).hexdigest()}")


def read_matrix(
    path: Path, matrix_schema: str, *, allow_empty: bool = False
) -> list[dict[str, str]]:
    try:
        handle = path.open(newline="", encoding="utf-8")
    except OSError as error:
        raise Refused(f"cannot read matrix {path}: {error}") from error
    with handle:
        raw = csv.reader(handle)
        header = next(raw, [])
        expected_header = (
            MATRIX_COLUMNS if matrix_schema == MATRIX_SCHEMA_V1 else MATRIX_V2_COLUMNS
        )
        if tuple(header) != expected_header:
            raise Refused(
                f"matrix header differs from {matrix_schema}: got {header!r}, "
                f"expected {list(expected_header)!r}"
            )
        rows: list[dict[str, str]] = []
        for line, fields in enumerate(raw, start=2):
            if len(fields) != len(header):
                raise Refused(
                    f"{path}:{line}: {len(fields)} fields against {len(header)} columns"
                )
            row = dict(zip(header, fields))
            state = row["state"].strip()
            if state not in STATES:
                raise Refused(f"{path}:{line}: unknown state {state!r}")
            if not row["program"].strip():
                raise Refused(f"{path}:{line}: blank program")
            if row["backend"] not in PRODUCT_BACKENDS:
                raise Refused(f"{path}:{line}: unknown backend {row['backend']!r}")
            if state == "NOT_ATTEMPTED" and not row["reason"].strip():
                raise Refused(f"{path}:{line}: NOT_ATTEMPTED has no reason")
            rows.append(row)
    if not rows and not allow_empty:
        raise Refused("matrix has zero rows; that is no result, not a scorecard")
    return rows


def synthesize_not_attempted(
    corpus: Corpus,
    meta: Metadata,
    expected_backends: tuple[str, ...],
) -> list[dict[str, str]]:
    """Make an explicit cell population for an observed never-evaluated head."""
    reason = f"head evaluation NOT_ATTEMPTED: {meta.evaluation_reason}"
    rows: list[dict[str, str]] = []
    for program, corpus_program in sorted(corpus.programs.items()):
        for backend in expected_backends:
            row = dict.fromkeys(MATRIX_V2_COLUMNS, "")
            row.update(
                program=program,
                backend=backend,
                state="NOT_ATTEMPTED",
                reason=reason,
                legacy_modes="|".join(sorted(corpus_program.modes)),
            )
            if meta.matrix_schema == MATRIX_SCHEMA_V2:
                row["comparison_tier"] = "unqualified-no-comparison"
            rows.append(row)
    return rows


def population_id(rows: Iterable[dict[str, str]]) -> str:
    # Must match check-scorecard-provenance.py's scorecard-population-v2
    # authority exactly. The receipt binds the identities AFTER the explicit
    # dbt -> dbi scorecard vocabulary mapping, because those are the rows a
    # consumer actually reads.
    identities = sorted(
        "\t".join(
            (
                "real-program-matrix",
                "local",
                BUCKET,
                f"{BUCKET}/{r['program']}",
                "verify",
                SCORECARD_BACKEND.get(r["backend"], r["backend"]),
                "enabled",
            )
        )
        for r in rows
    )
    h = hashlib.sha256(b"scorecard-population-v2\n")
    for identity in identities:
        h.update(identity.encode())
        h.update(b"\n")
    return f"sha256:{h.hexdigest()}"


def validate_population(
    rows: list[dict[str, str]],
    annotations: Annotations,
    corpus: Corpus,
    expected_programs: int,
    expected_backends: tuple[str, ...],
) -> tuple[list[str], int, int]:
    programs = sorted({r["program"] for r in rows})
    backends = sorted({r["backend"] for r in rows})
    if len(programs) != expected_programs:
        raise Refused(
            f"program population is {len(programs)}, expected {expected_programs}; "
            "a changed denominator needs an explicit task update"
        )
    if set(programs) != set(corpus.programs):
        raise Refused(
            "matrix program population differs from the bound corpus: "
            f"matrix-only={sorted(set(programs) - set(corpus.programs))[:5]!r} "
            f"corpus-only={sorted(set(corpus.programs) - set(programs))[:5]!r}"
        )
    if backends != sorted(expected_backends):
        raise Refused(f"backend population is {backends}, expected {sorted(expected_backends)}")
    keys = [(r["program"], r["backend"]) for r in rows]
    duplicates = [key for key, count in Counter(keys).items() if count != 1]
    if duplicates:
        raise Refused(f"duplicate matrix cells: {duplicates[:5]!r}")
    expected = {(p, b) for p in programs for b in expected_backends}
    missing = sorted(expected - set(keys))
    extra = sorted(set(keys) - expected)
    if missing or extra:
        raise Refused(
            f"matrix is not the Cartesian product: missing={missing[:5]!r} extra={extra[:5]!r}"
        )
    for program in programs:
        mode_values = {
            tuple(filter(None, row["legacy_modes"].split("|")))
            for row in rows
            if row["program"] == program
        }
        if len(mode_values) != 1:
            raise Refused(
                f"{program}: legacy corpus membership differs across backend rows: "
                f"{sorted(mode_values)!r}"
            )
        modes = next(iter(mode_values))
        if not modes or len(modes) != len(set(modes)) or not set(modes) <= LEGACY_MODES:
            raise Refused(f"{program}: invalid legacy_modes {modes!r}")
        if set(modes) != corpus.programs[program].modes:
            raise Refused(
                f"{program}: matrix legacy_modes {sorted(set(modes))!r} differ from "
                f"bound corpus {sorted(corpus.programs[program].modes)!r}"
            )
    unknown_annotations = sorted(set(annotations.by_program) - set(programs))
    if unknown_annotations:
        raise Refused(
            f"annotations name programs outside the matrix: {unknown_annotations[:5]!r}"
        )
    attempted = sum(r["state"] in ATTEMPTED for r in rows)
    # Canonical scorecard `evidence_count` counts cross-backend parity claims,
    # not same-backend self-verification fields. The provenance verifier
    # re-derives this as the number of nonblank stdout_parity verdicts.
    evidence = sum(bool_text(r.get("stdout_parity", "")) is not None for r in rows)
    return programs, attempted, evidence


def source_comparison(
    source: dict[str, str], meta: Metadata
) -> tuple[str, str, str, str, str, str]:
    """Return tier, parity, candidate, reference, comparator, and parity tier.

    Matrix v1 carries only same-backend strict verification, so its strongest
    truthful tier is self-verify-only. Matrix v2 may carry a narrower real
    cross-backend stdout witness. The witness is accepted only with both hash
    operands and the exact comparator contract; a tier label alone is not
    evidence.
    """
    state = source["state"]
    if meta.matrix_schema == MATRIX_SCHEMA_V1:
        tier = (
            "unqualified-no-comparison"
            if state == "NOT_ATTEMPTED"
            else "unqualified-self-verify-only"
        )
        return tier, "", "", "", "", ""

    tier = source["comparison_tier"].strip()
    if tier not in KNOWN_SOURCE_TIERS:
        raise Refused(
            f"{source['program']}/{source['backend']}: unknown or unsupported "
            f"source comparison_tier {tier!r}"
        )
    parity = bool_text(source["stdout_parity"])
    candidate = source["output_hash"].strip().lower()
    reference = source["ref_output_hash"].strip().lower()
    comparator = source["parity_comparator"].strip()
    parity_tier = source["parity_tier"].strip()
    if tier == "unqualified-stdout-only":
        if parity is None:
            raise Refused(
                f"{source['program']}/{source['backend']}: stdout-only tier has no parity verdict"
            )
        if not SHA256_HEX.fullmatch(candidate) or not SHA256_HEX.fullmatch(reference):
            raise Refused(
                f"{source['program']}/{source['backend']}: "
                "stdout-only tier lacks two SHA-256 operands"
            )
        if comparator != "stdout-sha256-exact-v1" or parity_tier != "stdout-exact":
            raise Refused(
                f"{source['program']}/{source['backend']}: "
                "stdout-only tier has the wrong comparator contract"
            )
        derived = candidate == reference
        if parity is not derived:
            raise Refused(
                f"{source['program']}/{source['backend']}: "
                "stdout parity does not match its hash operands"
            )
        profile = json.loads(meta.profile_flags)
        comparison = profile.get("comparison")
        if not isinstance(comparison, list) or "--strict" not in comparison:
            raise Refused(
                f"{source['program']}/{source['backend']}: "
                "stdout witness profile omits a strict comparison argv"
            )
    elif any((parity is not None, candidate, reference, comparator, parity_tier)):
        raise Refused(
            f"{source['program']}/{source['backend']}: {tier} row carries "
            "partial stdout witness fields"
        )
    if state == "NOT_ATTEMPTED" and tier != "unqualified-no-comparison":
        raise Refused(
            f"{source['program']}/{source['backend']}: "
            "NOT_ATTEMPTED cannot carry comparison evidence"
        )
    return tier, bool_output(parity), candidate, reference, comparator, parity_tier


def mapped_row(
    source: dict[str, str],
    meta: Metadata,
    annotation: Annotation | None,
    classification_id: str,
    corpus_program: CorpusProgram,
    corpus_id: str,
    pop_id: str,
    selected: int,
    executed: int,
    evidence: int,
) -> dict[str, str]:
    state = source["state"]
    verified = bool_text(source["verified"])
    bitwise = bool_text(source["bitwise_parity"])
    left = positive_int(source["compared_left"])
    right = positive_int(source["compared_right"])

    has_nonzero_comparison = (
        left is not None and left > 0 and right is not None and right > 0
    )
    if source["verdict"] == "infrastructure_error":
        if state not in ATTEMPTED:
            raise Refused(
                f"{source['program']}/{source['backend']}: infrastructure_error "
                "must come from an attempted row"
            )
        outcome = "error"
        deterministic = ""
        tier = "gap"
        compare = ""
    elif state == "PASS":
        if verified is not True or bitwise is not True or not has_nonzero_comparison:
            raise Refused(
                f"{source['program']}/{source['backend']}: "
                "PASS lacks nonzero strict self-verify evidence"
            )
        outcome = "pass"
        deterministic = "1"
        tier = "bitwise"
        compare = "canonical"
    elif state == "ATTEMPTED_UNQUALIFIABLE":
        no_result = source["verdict"] == "no_result" and source["rc"] == "0"
        if verified is not True and not no_result:
            raise Refused(
                f"{source['program']}/{source['backend']}: ATTEMPTED_UNQUALIFIABLE "
                "must record verified=true or the product's rc=0 verdict=no_result"
            )
        outcome = "pass"
        deterministic = "1" if bitwise is True and has_nonzero_comparison else ""
        tier = "bitwise" if deterministic == "1" else "gap"
        compare = "canonical" if deterministic == "1" else ""
    elif state == "FAIL":
        outcome, deterministic, tier, compare = "fail", "0", "gap", ""
    elif state == "TIMEOUT":
        outcome, deterministic, tier, compare = "timeout", "", "gap", ""
    else:
        outcome, deterministic, tier, compare = "unavailable", "", "gap", ""

    reason = source["reason"].strip()
    compared = f"{left}|{right}" if left is not None and right is not None else ""
    comparison_tier, stdout_parity, output_hash, ref_output_hash, parity_comparator, parity_tier = (
        source_comparison(source, meta)
    )
    if state == "PASS":
        reason = (
            "source PASS: strict within-backend self-verify plus a bound ptrace stdout witness"
            if comparison_tier == "unqualified-stdout-only"
            else "source PASS: strict within-backend self-verify only; "
            "no ptrace cross-backend witness"
        )
    if stdout_parity == "0":
        outcome = "fail"
        reason = (
            f"{reason}; " if reason else ""
        ) + "cross-backend stdout witness diverged from ptrace"
    legacy_modes = tuple(filter(None, source["legacy_modes"].split("|")))
    legacy_count = len(set(legacy_modes))
    if legacy_count == 0:
        raise Refused(f"{source['program']}/{source['backend']}: legacy_modes is blank")
    legacy_coverage = (
        "single-corpus"
        if legacy_count == 1
        else "all-four-corpora"
        if legacy_count == 4
        else "multi-corpus"
    )
    classes = "|".join(annotation.classes) if annotation else ""
    row = {
        "run_id": meta.run_id,
        "run_utc": meta.run_utc,
        "hermit_sha": meta.hermit_sha,
        "reverie_sha": meta.reverie_sha,
        "dirty": "false",
        "run_mode": "real-program-matrix",
        "lane": "local",
        "bucket": BUCKET,
        "test_id": f"{BUCKET}/{source['program']}",
        "test_mode": "verify",
        "backend": SCORECARD_BACKEND.get(source["backend"], source["backend"]),
        "cell_state": "enabled",
        "outcome": outcome,
        "deterministic": deterministic,
        "stdout_parity": stdout_parity,
        "output_hash": output_hash,
        "duration_ms": source["duration_ms"],
        "max_rss_kb": "",
        "reason": reason,
        "verify_compare": compare,
        "bitwise_parity": "1" if bitwise is True else "0" if bitwise is False else "",
        "compared_log_messages": compared,
        "tier": tier,
        "legacy_parity_unqualified": "",
        "ref_output_hash": ref_output_hash,
        "parity_comparator": parity_comparator,
        "parity_tier": parity_tier,
        "profile_flags": meta.profile_flags,
        "population_id": pop_id,
        "selected_count": str(selected),
        "executed_count": str(executed),
        "evidence_count": str(evidence),
        "comparison_tier": comparison_tier,
        "exit_code_parity": "",
        # These columns are deliberately blank until each has its own bound
        # reference operand in check_cell_comparison.py. The existing schema
        # names them as representable-but-unimplemented; filling them here would
        # turn the absence of a reference into an apparent comparison.
        "detlog_parity": "",
        "oracle_verdict": "",
        "evaluation_state": meta.evaluation_state,
        "evaluation_reason": meta.evaluation_reason,
        "evaluation_evidence": meta.evaluation_evidence,
        "source_legacy_modes": "|".join(sorted(set(legacy_modes))),
        "source_legacy_mode_count": str(legacy_count),
        "source_legacy_coverage": legacy_coverage,
        "program_classes": classes,
        "concurrency_kind": annotation.concurrency_kind if annotation else "",
        "program_class_evidence": annotation.evidence if annotation else "",
        "classification_id": classification_id,
        "harness_form": corpus_program.harness_form,
        "corpus_id": corpus_id,
    }
    return row


def summarize(
    rows: list[dict[str, str]], annotations: Annotations, corpus: Corpus
) -> list[str]:
    lines = []
    for backend in PRODUCT_BACKENDS:
        mine = [r for r in rows if r["backend"] == backend]
        counts = Counter(r["state"] for r in mine)
        attempted = sum(counts[s] for s in ATTEMPTED)
        passed = counts["PASS"]
        lines.append(
            f"{backend}: SOURCE_SELF_VERIFY_PASS/ATTEMPTED={passed}/{attempted}; "
            f"ATTEMPTED/CORPUS={attempted}/{len(mine)}; "
            f"FAIL={counts['FAIL']} TIMEOUT={counts['TIMEOUT']} "
            f"ATTEMPTED_UNQUALIFIABLE={counts['ATTEMPTED_UNQUALIFIABLE']} "
            f"NOT_ATTEMPTED={counts['NOT_ATTEMPTED']}"
        )
    by_program = {r["program"]: r for r in rows}
    singleton = sorted(
        program
        for program, row in by_program.items()
        if len(set(filter(None, row["legacy_modes"].split("|")))) == 1
    )
    lines.append(
        f"legacy singleton coverage holes={len(singleton)}/{len(by_program)} programs; "
        f"labels={'|'.join(singleton)}"
    )
    for mode in sorted(LEGACY_MODES):
        present = sum(
            mode in set(filter(None, row["legacy_modes"].split("|")))
            for row in by_program.values()
        )
        lines.append(
            f"legacy mode={mode}: PRESENT/CORPUS={present}/{len(by_program)}; "
            f"UNEXPLAINED_ABSENT/CORPUS={len(by_program) - present}/{len(by_program)}"
        )
    for form in ("direct", "shell-wrapped", "workload-script"):
        members = {
            program
            for program, corpus_program in corpus.programs.items()
            if corpus_program.harness_form == form
        }
        for backend in PRODUCT_BACKENDS:
            mine = [
                row for row in rows if row["backend"] == backend and row["program"] in members
            ]
            counts = Counter(row["state"] for row in mine)
            attempted = sum(counts[state] for state in ATTEMPTED)
            lines.append(
                f"harness_form={form} backend={backend}: "
                f"SOURCE_SELF_VERIFY_PASS/ATTEMPTED={counts['PASS']}/{attempted}; "
                f"ATTEMPTED/CORPUS={attempted}/{len(members)}"
            )
    classes = sorted(
        {item for annotation in annotations.by_program.values() for item in annotation.classes}
    )
    for class_name in classes:
        members = {
            program
            for program, annotation in annotations.by_program.items()
            if class_name in annotation.classes
        }
        for backend in PRODUCT_BACKENDS:
            mine = [
                row for row in rows if row["backend"] == backend and row["program"] in members
            ]
            counts = Counter(row["state"] for row in mine)
            attempted = sum(counts[state] for state in ATTEMPTED)
            lines.append(
                f"class={class_name} backend={backend}: "
                f"SOURCE_SELF_VERIFY_PASS/ATTEMPTED={counts['PASS']}/{attempted}; "
                f"ATTEMPTED/CORPUS={attempted}/{len(members)}"
            )
    return lines


def ingest(
    matrix: Path,
    metadata: Path,
    corpus_path: Path,
    annotations_path: Path,
    output: Path,
    schema: Path,
    expected_programs: int = 214,
    expected_backends: tuple[str, ...] = PRODUCT_BACKENDS,
) -> list[str]:
    meta = read_metadata(metadata, matrix, corpus_path)
    corpus = read_corpus(corpus_path)
    annotations = read_annotations(annotations_path)
    rows = read_matrix(
        matrix,
        meta.matrix_schema,
        allow_empty=meta.evaluation_state == "NOT_ATTEMPTED",
    )
    if meta.evaluation_state == "NOT_ATTEMPTED":
        if rows:
            raise Refused("NOT_ATTEMPTED head evaluation carries result rows")
        rows = synthesize_not_attempted(corpus, meta, expected_backends)
    programs, executed, evidence = validate_population(
        rows, annotations, corpus, expected_programs, expected_backends
    )
    selected = len(rows)
    pop_id = population_id(rows)
    columns = scorecard_columns(schema)
    mapped = [
        mapped_row(
            row,
            meta,
            annotations.by_program.get(row["program"]),
            annotations.classification_id,
            corpus.programs[row["program"]],
            corpus.corpus_id,
            pop_id,
            selected,
            executed,
            evidence,
        )
        for row in rows
    ]
    missing = sorted(set(columns) - set(mapped[0]))
    if missing:
        raise Refused(f"importer does not populate authoritative core columns: {missing}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(
            handle, fieldnames=columns, lineterminator="\n", extrasaction="ignore"
        )
        writer.writeheader()
        writer.writerows(mapped)
    lines = [
        f"real-programs: wrote {len(mapped)} cells = {len(programs)} programs x "
        f"{len(expected_backends)} backends to {output}",
        *summarize(rows, annotations, corpus),
    ]
    return lines


def main(argv: list[str] | None = None) -> int:
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument(
        "--annotations",
        type=Path,
        default=here / "real-program-annotations.json",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--schema", type=Path, default=here / "scorecard-schema.json")
    parser.add_argument("--expected-programs", type=int, default=214)
    args = parser.parse_args(argv)
    try:
        lines = ingest(
            args.matrix,
            args.metadata,
            args.corpus,
            args.annotations,
            args.output,
            args.schema,
            expected_programs=args.expected_programs,
        )
    except Refused as error:
        die(str(error))
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
