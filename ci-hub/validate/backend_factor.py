#!/usr/bin/env python3
"""Compare two backends within one qualified validation run.

The validation ledger binds the run and its compact cell population.  Timing
is retained in the schema-4 test-harness ``results.jsonl`` files beside that
compact artifact, not in the compact ledger projection.  This reader verifies
the compact artifact digest, requires the raw result identities to match it,
and only compares first-attempt PASS rows from the same run and provenance.
"""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import shlex
import statistics
import sys
from typing import Any, Iterable, Mapping, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qualified_rows  # noqa: E402


IDENTITY_FIELDS = ("lane", "category", "test", "mode")
PAIR_PROVENANCE_FIELDS = (
    "run_id",
    "machine_shortname",
    "kernel_version",
    "host_capabilities",
    "hermit_sha",
    "source_tree_dirty",
    "binary_sha256",
    "binary_build_sha",
    "test_sha256",
    "classification",
    "timeout_seconds",
    "execution_cpu_timeout_seconds",
    "execution_wall_timeout_seconds",
    "lane",
    "category",
    "test",
    "mode",
    "log_level",
)
METRICS = (
    ("cpu_usage_usec", "CPU microseconds"),
    ("duration_ms", "elapsed milliseconds"),
)
TIMING_MAXIMUMS = {
    "cpu_usage_usec": (1 << 64) - 1,
    "duration_ms": (1 << 128) - 1,
}
SABRE_PATH_EVIDENCE_ENV = "HERMIT_SABRE_PATH_EVIDENCE"
HOST_CAPABILITY_KEYS = frozenset(("cpuid-faulting", "kvm"))


class BackendFactorError(ValueError):
    """The requested comparison is not supported by the retained evidence."""


def _is_timing_integer(value: object, field: str) -> bool:
    maximum = TIMING_MAXIMUMS[field]
    return (
        isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= maximum
    )


def _metric_value(row: Mapping[str, object], field: str) -> float:
    value = row.get(field)
    if not _is_timing_integer(value, field):
        raise BackendFactorError(f"cell result has invalid {field}")
    assert isinstance(value, int) and not isinstance(value, bool)
    return float(value)


def _backend(row: Mapping[str, object]) -> str:
    value = row.get("backend")
    if value is None:
        if row.get("mode") != "naked":
            raise BackendFactorError("only a naked cell may omit its backend")
        return "native"
    if not isinstance(value, str) or not value:
        raise BackendFactorError("cell result has no valid backend")
    return value


def _identity(row: Mapping[str, object]) -> tuple[str, str, str, str]:
    values = tuple(row.get(field) for field in IDENTITY_FIELDS)
    if not all(isinstance(value, str) and value for value in values):
        raise BackendFactorError("cell result has an incomplete cell identity")
    return values  # type: ignore[return-value]


def _identity_json(identity: tuple[str, str, str, str]) -> dict[str, str]:
    return dict(zip(IDENTITY_FIELDS, identity, strict=True))


def _read_jsonl(path: Path) -> tuple[list[dict[str, object]], bytes]:
    raw = path.read_bytes()
    if raw and not raw.endswith(b"\n"):
        raise BackendFactorError(f"{path}: truncated final JSONL line")
    rows: list[dict[str, object]] = []
    for number, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise BackendFactorError(f"{path}:{number}: malformed JSON") from error
        if not isinstance(value, dict):
            raise BackendFactorError(f"{path}:{number}: row is not an object")
        rows.append(value)
    return rows, raw


def _qualified_run(state_root: Path, run_id: str) -> dict[str, object]:
    try:
        rows = qualified_rows.canonical_ledger.rows(root=state_root)
        current = qualified_rows.canonical_ledger.current_validation_run_rows(rows)
        matches = [row for row in current if row.get("run_id") == run_id]
    except (OSError, ValueError, json.JSONDecodeError) as error:
        raise BackendFactorError(
            f"cannot read canonical validation ledger: {error}"
        ) from error
    if not matches:
        raise BackendFactorError(
            f"run {run_id!r} is absent from the current validation ledger"
        )
    if len(matches) != 1:
        raise BackendFactorError(
            f"run {run_id!r} has {len(matches)} current ledger rows; expected one"
        )
    try:
        qualified = qualified_rows.qualified_rows(matches)
    except (ValueError, json.JSONDecodeError) as error:
        raise BackendFactorError(f"cannot qualify validation run: {error}") from error
    if len(qualified) != 1:
        raise BackendFactorError(
            f"run {run_id!r} is absent from the qualified validation ledger"
        )
    return matches[0]


def _retained_path(state_root: Path, relative: object) -> Path:
    if not isinstance(relative, str) or not relative or Path(relative).is_absolute():
        raise BackendFactorError("cell_results artifact path is not parent-relative")
    retained = (state_root / "ignored" / "validate" / "artifacts").resolve()
    path = (state_root / relative).resolve()
    try:
        path.relative_to(retained)
    except ValueError as error:
        raise BackendFactorError(
            "cell_results artifact is outside ignored/validate/artifacts"
        ) from error
    if not path.is_file():
        raise BackendFactorError(f"retained cell_results artifact is absent: {path}")
    return path


def _compact_verdicts(
    state_root: Path, ledger_row: Mapping[str, object], run_id: str
) -> tuple[Path, dict[tuple[str, str, str, str, str], str]]:
    evidence = ledger_row.get("cell_results")
    if not isinstance(evidence, Mapping) or evidence.get("run_id") != run_id:
        raise BackendFactorError(
            "qualified ledger row has no matching cell_results run_id"
        )
    artifact = evidence.get("artifact")
    if not isinstance(artifact, Mapping):
        raise BackendFactorError("qualified ledger row has no cell_results artifact")
    path = _retained_path(state_root, artifact.get("path"))
    if path.parent.name != run_id:
        raise BackendFactorError(
            "cell_results artifact directory does not match run_id"
        )
    rows, raw = _read_jsonl(path)
    expected_digest = artifact.get("sha256")
    observed_digest = hashlib.sha256(raw).hexdigest()
    if expected_digest != observed_digest:
        raise BackendFactorError("cell_results artifact sha256 differs from ledger")
    expected_count = artifact.get("row_count")
    if expected_count != len(rows):
        raise BackendFactorError("cell_results artifact row count differs from ledger")

    commit = ledger_row.get("commit")
    verdicts: dict[tuple[str, str, str, str, str], str] = {}
    for row in rows:
        if row.get("run_id") != run_id:
            raise BackendFactorError(
                "cell_results artifact contains a different run_id"
            )
        if row.get("hermit_sha") != commit:
            raise BackendFactorError(
                "cell_results artifact contains a different Hermit SHA"
            )
        if row.get("source_tree_dirty") is not False:
            raise BackendFactorError(
                "cell_results artifact contains a dirty source row"
            )
        key = (*_identity(row), _backend(row))
        if key in verdicts:
            raise BackendFactorError(
                "cell_results artifact contains a duplicate cell identity"
            )
        verdict = row.get("cell_verdict")
        state = verdict.get("state") if isinstance(verdict, Mapping) else None
        if not isinstance(state, str) or not state:
            raise BackendFactorError(
                "cell_results artifact contains a cell without a typed verdict"
            )
        verdicts[key] = state
    return path, verdicts


def _raw_results(
    compact_path: Path,
    ledger_row: Mapping[str, object],
    run_id: str,
    compact_identities: set[tuple[str, str, str, str, str]],
) -> tuple[list[dict[str, object]], list[dict[str, object]], str]:
    run_root = compact_path.parent.resolve()
    e2e_root = (run_root / "e2e").resolve()
    try:
        e2e_root.relative_to(run_root)
    except ValueError as error:
        raise BackendFactorError(
            "retained E2E result directory is outside the retained run directory"
        ) from error
    if not e2e_root.is_dir():
        raise BackendFactorError(f"retained E2E result directory is absent: {e2e_root}")
    files = sorted(e2e_root.rglob("results.jsonl"))
    if not files:
        raise BackendFactorError(f"no results.jsonl files below {e2e_root}")

    all_rows: list[dict[str, object]] = []
    sources: list[dict[str, object]] = []
    combined = hashlib.sha256()
    raw_identities: set[tuple[str, str, str, str, str]] = set()
    commit = ledger_row.get("commit")
    host = str(ledger_row.get("host") or "").split(".", 1)[0]
    for path in files:
        resolved = path.resolve()
        try:
            relative = resolved.relative_to(e2e_root).as_posix()
        except ValueError as error:
            raise BackendFactorError(
                f"result path escapes the retained E2E root: {path}"
            ) from error
        rows, raw = _read_jsonl(resolved)
        digest = hashlib.sha256(raw).hexdigest()
        sources.append({"path": relative, "sha256": digest, "rows": len(rows)})
        combined.update(relative.encode())
        combined.update(b"\0")
        combined.update(raw)
        combined.update(b"\0")
        for row in rows:
            if row.get("schema") != 4:
                raise BackendFactorError(f"{relative}: cell result schema is not 4")
            if row.get("run_id") != run_id:
                raise BackendFactorError(f"{relative}: cell result run_id differs")
            if row.get("hermit_sha") != commit:
                raise BackendFactorError(f"{relative}: cell result Hermit SHA differs")
            if row.get("source_tree_dirty") is not False:
                raise BackendFactorError(
                    f"{relative}: cell result source tree is dirty"
                )
            if row.get("machine_shortname") != host:
                raise BackendFactorError(f"{relative}: cell result host differs")
            binary_sha256 = row.get("binary_sha256")
            if not _is_lower_hex(binary_sha256, 64):
                raise BackendFactorError(
                    f"{relative}: cell result has no exact binary sha256"
                )
            test_sha256 = row.get("test_sha256")
            if not _is_lower_hex(test_sha256, 64):
                raise BackendFactorError(
                    f"{relative}: cell result has no exact test sha256"
                )
            binary_build_sha = row.get("binary_build_sha")
            if (
                not isinstance(binary_build_sha, str)
                or not 12 <= len(binary_build_sha) <= 40
                or not _is_lower_hex(binary_build_sha, len(binary_build_sha))
                or not isinstance(commit, str)
                or not commit.startswith(binary_build_sha)
            ):
                raise BackendFactorError(
                    f"{relative}: binary build sha is absent, dirty, or stale"
                )
            attempt = row.get("attempt")
            if not isinstance(attempt, int) or isinstance(attempt, bool) or attempt < 1:
                raise BackendFactorError(
                    f"{relative}: cell result attempt is not positive"
                )
            raw_identities.add((*_identity(row), _backend(row)))
        all_rows.extend(rows)
    if raw_identities != compact_identities:
        missing = len(compact_identities - raw_identities)
        extra = len(raw_identities - compact_identities)
        raise BackendFactorError(
            "retained E2E cell identities differ from the ledger-bound compact artifact "
            f"(missing={missing}, extra={extra})"
        )
    return all_rows, sources, combined.hexdigest()


def _passes(row: Mapping[str, object]) -> bool:
    return row.get("outcome") == "PASS" and row.get("result") == "pass"


def _is_lower_hex(value: object, length: int) -> bool:
    return (
        isinstance(value, str)
        and len(value) == length
        and all(character in "0123456789abcdef" for character in value)
    )


def _replace_at_path_boundary(value: str, old: str, new: str) -> str:
    """Replace a known path without consuming a sibling path's prefix."""
    start = 0
    pieces: list[str] = []
    while True:
        found = value.find(old, start)
        if found < 0:
            pieces.append(value[start:])
            return "".join(pieces)
        end = found + len(old)
        preceding = value[found - 1 : found]
        following = value[end : end + 1]
        before_preceding = value[found - 2 : found - 1]
        opening_quote = preceding in "\"'" and (
            not before_preceding
            or before_preceding.isspace()
            or before_preceding in "=([{,:>"
        )
        starts_at_boundary = (
            not preceding
            or preceding.isspace()
            or preceding in "=([{,:"
            or opening_quote
        )
        ends_at_boundary = (
            not following
            or following == "/"
            or (opening_quote and following == preceding)
        )
        if starts_at_boundary and ends_at_boundary:
            pieces.extend((value[start:found], new))
            start = end
        else:
            pieces.append(value[start:end])
            start = end


def _normalized_execution_value(row: Mapping[str, object], field: str) -> object:
    """Remove only the cell-specific artifact directory from execution inputs."""
    artifact_dir = row.get("artifact_dir")

    def normalize(value: object) -> object:
        if isinstance(value, str):
            if isinstance(artifact_dir, str) and artifact_dir:
                return _replace_at_path_boundary(
                    value, artifact_dir, "{CELL_ARTIFACT_DIR}"
                )
            return value
        if isinstance(value, list):
            return [normalize(item) for item in value]
        if isinstance(value, dict):
            return {key: normalize(item) for key, item in value.items()}
        return value

    return normalize(row.get(field))


def _normalized_env(row: Mapping[str, object]) -> object:
    value = _normalized_execution_value(row, "env")
    evidence = _expected_sabre_path_evidence(row)
    original_env = row.get("env")
    if (
        evidence is None
        or not isinstance(original_env, Mapping)
        or original_env.get(SABRE_PATH_EVIDENCE_ENV) != evidence
        or not isinstance(value, dict)
    ):
        return value
    # The manifest runner sets this only to collect SaBRe engagement evidence;
    # it is not part of the guest workload environment.
    normalized = dict(value)
    normalized.pop(SABRE_PATH_EVIDENCE_ENV, None)
    return normalized


def _expected_sabre_path_evidence(row: Mapping[str, object]) -> str | None:
    if _backend(row) != "sabre":
        return None
    artifact_dir = row.get("artifact_dir")
    mode = row.get("mode")
    attempt = row.get("attempt")
    if (
        not isinstance(artifact_dir, str)
        or not artifact_dir
        or not isinstance(mode, str)
        or not mode
        or not isinstance(attempt, int)
        or isinstance(attempt, bool)
        or attempt < 1
    ):
        return None
    return str(Path(artifact_dir) / "captures" / f"{mode}-{attempt}.sabre-path.jsonl")


def _normalized_argv(row: Mapping[str, object], field: str) -> object:
    value = _normalized_execution_value(row, field)
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        return value
    return _normalized_backend_arguments(row, value)


def _normalized_backend_arguments(
    row: Mapping[str, object], value: Sequence[str]
) -> list[str]:
    backend = _backend(row)
    normalized = list(value)
    for index, argument in enumerate(normalized):
        if argument == "--":
            break
        if index > 0 and normalized[index - 1] == "--backend" and argument == backend:
            normalized[index] = "{BACKEND}"
        elif argument == f"--backend={backend}":
            normalized[index] = "--backend={BACKEND}"
    return normalized


def _has_exact_backend_selector(value: object, expected: str) -> bool:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        return False
    selectors: list[str] = []
    index = 0
    while index < len(value):
        argument = value[index]
        if argument == "--":
            break
        if argument == "--backend":
            if index + 1 >= len(value) or value[index + 1] == "--":
                return False
            selectors.append(value[index + 1])
            index += 2
            continue
        if argument.startswith("--backend="):
            selectors.append(argument.partition("=")[2])
        index += 1
    return selectors == [expected]


def _normalized_shell_command(row: Mapping[str, object]) -> object:
    value = row.get("shell_command")
    if not isinstance(value, str):
        return value
    try:
        words = shlex.split(value)
    except ValueError:
        artifact_dir = row.get("artifact_dir")
        if isinstance(artifact_dir, str) and artifact_dir:
            return _replace_at_path_boundary(value, artifact_dir, "{CELL_ARTIFACT_DIR}")
        return value
    evidence = _expected_sabre_path_evidence(row)
    if evidence is not None:
        env = row.get("env")
        if isinstance(env, Mapping) and env.get(SABRE_PATH_EVIDENCE_ENV) == evidence:
            evidence_assignment = f"{SABRE_PATH_EVIDENCE_ENV}={evidence}"
            for index, word in enumerate(words):
                if word != "env":
                    continue
                assignment = index + 1
                while assignment < len(words) and "=" in words[assignment]:
                    if words[assignment] == evidence_assignment:
                        del words[assignment]
                        break
                    assignment += 1
                break
    artifact_dir = row.get("artifact_dir")
    if isinstance(artifact_dir, str) and artifact_dir:
        words = [
            _replace_at_path_boundary(word, artifact_dir, "{CELL_ARTIFACT_DIR}")
            for word in words
        ]
    return _normalized_backend_arguments(row, words)


def _provenance_difference(
    baseline: Mapping[str, object], comparison: Mapping[str, object]
) -> list[str]:
    differences = [
        field
        for field in PAIR_PROVENANCE_FIELDS
        if baseline.get(field) != comparison.get(field)
    ]
    for field in ("guest_argv", "cwd", "relaxations"):
        if _normalized_execution_value(baseline, field) != _normalized_execution_value(
            comparison, field
        ):
            differences.append(field)
    if _normalized_env(baseline) != _normalized_env(comparison):
        differences.append("env")
    for field in ("argv", "effective_args"):
        if _normalized_argv(baseline, field) != _normalized_argv(comparison, field):
            differences.append(field)
    if _normalized_shell_command(baseline) != _normalized_shell_command(comparison):
        differences.append("shell_command")
    return differences


def _invalid_pairing_evidence(row: Mapping[str, object]) -> list[str]:
    invalid: list[str] = []
    for field in (
        "kernel_version",
        "classification",
        "artifact_dir",
        "cwd",
    ):
        value = row.get(field)
        if not isinstance(value, str) or not value:
            invalid.append(field)
    capabilities = row.get("host_capabilities")
    if (
        not isinstance(capabilities, Mapping)
        or set(capabilities) != HOST_CAPABILITY_KEYS
        or any(
            not isinstance(verdict, Mapping)
            or set(verdict) != {"present", "evidence"}
            or type(verdict.get("present")) is not bool
            or not isinstance(verdict.get("evidence"), str)
            or not verdict.get("evidence", "").strip()
            for verdict in capabilities.values()
        )
    ):
        invalid.append("host_capabilities")
    for field in (
        "timeout_seconds",
        "execution_cpu_timeout_seconds",
        "execution_wall_timeout_seconds",
    ):
        value = row.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            invalid.append(field)
    guest_argv = row.get("guest_argv")
    if (
        not isinstance(guest_argv, list)
        or not guest_argv
        or not all(isinstance(item, str) and item for item in guest_argv)
    ):
        invalid.append("guest_argv")
    relaxations = row.get("relaxations")
    if not isinstance(relaxations, list) or not all(
        isinstance(item, str) for item in relaxations
    ):
        invalid.append("relaxations")
    env = row.get("env")
    if (
        not isinstance(env, Mapping)
        or not env
        or not all(
            isinstance(key, str) and key and isinstance(value, str)
            for key, value in env.items()
        )
    ):
        invalid.append("env")

    argv = row.get("argv")
    effective_args = row.get("effective_args")
    if (
        not isinstance(argv, list)
        or not argv
        or not all(isinstance(item, str) and item for item in argv)
    ):
        invalid.append("argv")
    if not isinstance(effective_args, list) or not all(
        isinstance(item, str) and item for item in effective_args
    ):
        invalid.append("effective_args")
    elif isinstance(argv, list) and effective_args != argv[1:]:
        invalid.append("effective_args")
    shell_command = row.get("shell_command")
    if not isinstance(shell_command, str) or not shell_command:
        invalid.append("shell_command")
    elif isinstance(argv, list):
        try:
            shell_words = shlex.split(shell_command)
        except ValueError:
            invalid.append("shell_command")
        else:
            if len(shell_words) < len(argv) or shell_words[-len(argv) :] != argv:
                invalid.append("shell_command")

    if row.get("mode") == "naked":
        if row.get("backend") is not None:
            invalid.append("backend")
        if isinstance(argv, list) and argv != guest_argv:
            invalid.append("argv")
    else:
        backend = row.get("backend")
        if not isinstance(backend, str) or not backend:
            invalid.append("backend")
        elif (
            "argv" not in invalid
            and "effective_args" not in invalid
            and (
                not _has_exact_backend_selector(argv, backend)
                or not _has_exact_backend_selector(effective_args, backend)
            )
        ):
            invalid.append("backend_selector")
        log_level = row.get("log_level")
        if not isinstance(log_level, str) or not log_level:
            invalid.append("log_level")
    return invalid


def _metric_summary(
    pairs: Sequence[dict[str, Any]], field: str, unit: str
) -> dict[str, object]:
    baseline = [float(pair["baseline"][field]) for pair in pairs]
    comparison = [float(pair["comparison"][field]) for pair in pairs]
    ratios = [right / left for left, right in zip(baseline, comparison, strict=True)]
    count = len(ratios)
    return {
        "unit": unit,
        "measurement": (
            "preparation plus launched process-group CPU"
            if field == "cpu_usage_usec"
            else "whole-cell elapsed time including preparation and descheduling"
        ),
        "factor_direction": "comparison backend value divided by baseline backend value",
        "sample_count": count,
        "geometric_mean_factor": math.exp(
            math.fsum(math.log(value) for value in ratios) / count
        ),
        "arithmetic_mean_factor": math.fsum(ratios) / count,
        "median_factor": statistics.median(ratios),
        "minimum_factor": min(ratios),
        "maximum_factor": max(ratios),
        "baseline_total": math.fsum(baseline),
        "comparison_total": math.fsum(comparison),
        "ratio_of_totals": math.fsum(comparison) / math.fsum(baseline),
        "baseline_arithmetic_mean": math.fsum(baseline) / count,
        "comparison_arithmetic_mean": math.fsum(comparison) / count,
        "comparison_faster_cells": sum(value < 1 for value in ratios),
        "comparison_slower_cells": sum(value > 1 for value in ratios),
        "equal_cells": sum(value == 1 for value in ratios),
    }


def compare_rows(
    rows: Iterable[Mapping[str, object]],
    *,
    baseline_backend: str,
    comparison_backend: str,
    mode: str | None,
    ledger_verdicts: Mapping[tuple[str, str, str, str, str], str],
) -> dict[str, Any]:
    if not baseline_backend or not comparison_backend:
        raise BackendFactorError("backend names must be nonempty")
    if baseline_backend == comparison_backend:
        raise BackendFactorError("baseline and comparison backends must differ")

    grouped: dict[tuple[str, str, str, str], dict[str, list[Mapping[str, object]]]] = {}
    for row in rows:
        backend = _backend(row)
        if backend not in {baseline_backend, comparison_backend}:
            continue
        identity = _identity(row)
        if mode is not None and identity[-1] != mode:
            continue
        grouped.setdefault(identity, {}).setdefault(backend, []).append(row)

    exclusions: list[dict[str, object]] = []
    included: list[dict[str, Any]] = []
    baseline_cells = sum(baseline_backend in backends for backends in grouped.values())
    comparison_cells = sum(
        comparison_backend in backends for backends in grouped.values()
    )
    cells_with_both = sum(
        baseline_backend in backends and comparison_backend in backends
        for backends in grouped.values()
    )

    for identity, backends in sorted(grouped.items()):
        base_rows = backends.get(baseline_backend, [])
        other_rows = backends.get(comparison_backend, [])
        reason: str | None = None
        detail: object = None
        if not base_rows:
            reason = "missing_baseline"
        elif not other_rows:
            reason = "missing_comparison"
        elif len(base_rows) != 1 or len(other_rows) != 1:
            reason = "retry_or_ambiguous"
            detail = {
                "baseline_attempts": [row.get("attempt") for row in base_rows],
                "comparison_attempts": [row.get("attempt") for row in other_rows],
                "baseline_outcomes": [row.get("outcome") for row in base_rows],
                "comparison_outcomes": [row.get("outcome") for row in other_rows],
            }
        elif base_rows[0].get("attempt") != 1 or other_rows[0].get("attempt") != 1:
            reason = "not_first_attempt"
        elif not _passes(base_rows[0]) or not _passes(other_rows[0]):
            reason = "non_pass"
            detail = {
                "baseline_outcome": base_rows[0].get("outcome"),
                "baseline_result": base_rows[0].get("result"),
                "comparison_outcome": other_rows[0].get("outcome"),
                "comparison_result": other_rows[0].get("result"),
            }
        else:
            baseline_verdict = ledger_verdicts.get((*identity, baseline_backend))
            comparison_verdict = ledger_verdicts.get((*identity, comparison_backend))
            if (
                baseline_verdict != "compared-and-matched"
                or comparison_verdict != "compared-and-matched"
            ):
                reason = "ledger_verdict_not_matched"
                detail = {
                    "baseline_state": baseline_verdict,
                    "comparison_state": comparison_verdict,
                }
            else:
                invalid_baseline = _invalid_pairing_evidence(base_rows[0])
                invalid_comparison = _invalid_pairing_evidence(other_rows[0])
                if invalid_baseline or invalid_comparison:
                    reason = "invalid_provenance"
                    detail = {
                        "baseline_fields": invalid_baseline,
                        "comparison_fields": invalid_comparison,
                    }
                else:
                    differences = _provenance_difference(base_rows[0], other_rows[0])
                    if differences:
                        reason = "provenance_mismatch"
                        detail = {"fields": differences}
                    else:
                        bad_metrics = [
                            field
                            for field, _unit in METRICS
                            if not all(
                                _is_timing_integer(row.get(field), field)
                                and _metric_value(row, field) > 0
                                for row in (base_rows[0], other_rows[0])
                            )
                        ]
                        if bad_metrics:
                            reason = "missing_or_nonpositive_timing"
                            detail = {"fields": bad_metrics}
        if reason is not None:
            exclusion: dict[str, object] = {
                **_identity_json(identity),
                "reason": reason,
            }
            if detail is not None:
                exclusion["detail"] = detail
            exclusions.append(exclusion)
            continue

        baseline = base_rows[0]
        comparison = other_rows[0]
        included.append(
            {
                **_identity_json(identity),
                "baseline": {
                    "backend": baseline_backend,
                    **{field: baseline[field] for field, _unit in METRICS},
                },
                "comparison": {
                    "backend": comparison_backend,
                    **{field: comparison[field] for field, _unit in METRICS},
                },
                "factors": {
                    field: _metric_value(comparison, field)
                    / _metric_value(baseline, field)
                    for field, _unit in METRICS
                },
            }
        )

    counts = Counter(str(item["reason"]) for item in exclusions)
    pair_set = hashlib.sha256()
    for pair in included:
        pair_set.update(
            "\t".join(str(pair[field]) for field in IDENTITY_FIELDS).encode()
        )
        pair_set.update(b"\n")
    return {
        "comparison": {
            "baseline_backend": baseline_backend,
            "comparison_backend": comparison_backend,
            "factor_formula": "comparison value / baseline value",
            "mode": mode,
        },
        "population": {
            "baseline_cells": baseline_cells,
            "comparison_cells": comparison_cells,
            "union_cells": len(grouped),
            "cells_with_both_backends": cells_with_both,
            "included_pairs": len(included),
            "included_pair_set_sha256": pair_set.hexdigest(),
            "excluded_counts": dict(sorted(counts.items())),
            "exclusions": exclusions,
        },
        "metrics": (
            {field: _metric_summary(included, field, unit) for field, unit in METRICS}
            if included
            else {
                field: {
                    "unit": unit,
                    "measurement": (
                        "preparation plus launched process-group CPU"
                        if field == "cpu_usage_usec"
                        else "whole-cell elapsed time including preparation and descheduling"
                    ),
                    "factor_direction": "comparison backend value divided by baseline backend value",
                    "sample_count": 0,
                    "geometric_mean_factor": None,
                    "arithmetic_mean_factor": None,
                    "unavailable_reason": "no comparable first-attempt PASS cell pairs",
                }
                for field, unit in METRICS
            }
        ),
        "pairs": included,
    }


def build_report(
    state_root: Path,
    *,
    run_id: str,
    baseline_backend: str,
    comparison_backend: str,
    mode: str | None,
) -> dict[str, Any]:
    state_root = state_root.resolve()
    ledger_row = _qualified_run(state_root, run_id)
    compact_path, compact = _compact_verdicts(state_root, ledger_row, run_id)
    cell_results = ledger_row["cell_results"]
    assert isinstance(cell_results, Mapping)
    compact_artifact = cell_results["artifact"]
    assert isinstance(compact_artifact, Mapping)
    rows, sources, source_digest = _raw_results(
        compact_path, ledger_row, run_id, set(compact)
    )
    comparison = compare_rows(
        rows,
        baseline_backend=baseline_backend,
        comparison_backend=comparison_backend,
        mode=mode,
        ledger_verdicts=compact,
    )
    return {
        "schema": "ci-hub-backend-factor/v1",
        "run": {
            "run_id": run_id,
            "run_number": ledger_row.get("run_number"),
            "commit": ledger_row.get("commit"),
            "tree": ledger_row.get("tree"),
            "started_at": ledger_row.get("started_at"),
            "finished_at": ledger_row.get("finished_at"),
            "host": ledger_row.get("host"),
            "profile": ledger_row.get("profile"),
            "selection_mode": ledger_row.get("selection_mode"),
            "compact_artifact": compact_path.relative_to(state_root).as_posix(),
            "compact_artifact_sha256": compact_artifact.get("sha256"),
            "compact_artifact_rows": compact_artifact.get("row_count"),
            "selected_population_sha256": cell_results.get("population_sha256"),
            "selected_cells": cell_results.get("selected_count"),
            "recorded_cells": cell_results.get("recorded_count"),
            "raw_result_root": compact_path.parent.joinpath("e2e")
            .relative_to(state_root)
            .as_posix(),
            "raw_result_rows": len(rows),
            "raw_result_digest": source_digest,
            "raw_result_digest_method": "sha256 over each sorted relative path, NUL, file bytes, NUL",
            "timing_binding": (
                "snapshot digest calculated by this command; the validation ledger "
                "binds cell identities and verdicts but does not bind timing bytes"
            ),
            "raw_result_files": sources,
        },
        **comparison,
    }


def _print_text(report: Mapping[str, Any]) -> None:
    run = report["run"]
    comparison = report["comparison"]
    population = report["population"]
    print(
        f"run_id={run['run_id']} run_number={run['run_number']} "
        f"commit={run['commit']} tree={run['tree']} host={run['host']}"
    )
    print(
        f"source={run['raw_result_root']} rows={run['raw_result_rows']} "
        f"sha256={run['raw_result_digest']}"
    )
    print(f"timing_binding={run['timing_binding']}")
    print(
        f"factor={comparison['comparison_backend']}/{comparison['baseline_backend']} "
        f"mode={comparison['mode'] or 'all'}"
    )
    print(
        "population "
        f"baseline_cells={population['baseline_cells']} "
        f"comparison_cells={population['comparison_cells']} "
        f"union_cells={population['union_cells']} "
        f"cells_with_both={population['cells_with_both_backends']} "
        f"included_pairs={population['included_pairs']}"
    )
    metrics = report["metrics"]
    for field, _unit in METRICS:
        metric = metrics[field]
        if metric["sample_count"] == 0:
            print(f"metric={field} unavailable={metric['unavailable_reason']}")
            continue
        print(
            f"metric={field} geometric_mean_factor={metric['geometric_mean_factor']:.6f} "
            f"arithmetic_mean_factor={metric['arithmetic_mean_factor']:.6f} "
            f"ratio_of_totals={metric['ratio_of_totals']:.6f} "
            f"comparison_faster={metric['comparison_faster_cells']} "
            f"comparison_slower={metric['comparison_slower_cells']} "
            f"equal={metric['equal_cells']}"
        )
    for item in population["exclusions"]:
        detail = item.get("detail")
        detail_text = (
            f" detail={json.dumps(detail, sort_keys=True, separators=(',', ':'))}"
            if detail is not None
            else ""
        )
        print(
            f"excluded reason={item['reason']} lane={item['lane']} "
            f"category={item['category']} test={item['test']} mode={item['mode']}"
            f"{detail_text}"
        )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-root", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--comparison", required=True)
    parser.add_argument("--mode")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        report = build_report(
            args.state_root,
            run_id=args.run_id,
            baseline_backend=args.baseline,
            comparison_backend=args.comparison,
            mode=args.mode,
        )
    except (BackendFactorError, OSError, json.JSONDecodeError) as error:
        print(f"backend-factor: {error}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    else:
        _print_text(report)
    return 0 if report["population"]["included_pairs"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
