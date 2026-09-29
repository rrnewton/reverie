#!/usr/bin/env python3
"""Read, check and describe hermit's parity summary, `scorecard/parity.json`.

Parity is measured after determinism: hermit's post-pass compares each
candidate backend's retained verify log with the ptrace reference's and the
validate publishes one `parity-ledger/v1` row per cell into the test ledger's
`parity/` store (`ci-hub/series/parity_ledger.py`). hermit's ledger scorecard
(`ci/compat-envelope/scorecard.rs`) summarizes that store into
`scorecard/parity.json`, schema `parity-summary/v1`, and the ledger mirror
commits it beside the rest of the scorecard. The website renders its parity
panel from that committed summary and nothing else, so the scorecard and the
site cannot report two different parity figures. The design is
https://github.com/rrnewton/hermit/issues/3301.

This module checks a summary before anything renders it. The shape must be
exactly what the scorecard writes, and every count and credit must be what its
own per-cell list implies: each tally is recomputed from `cells`, each one-line
summary is re-rendered from its tally, and a credit that is null is never
counted as 0 or 1. Anything else is refused with a message naming the field.

The website carries the summary as its exact committed bytes (a "carriage",
`CARRIAGE_FIELDS`), so the same checks run when the export captures it, when
the build admits it, and when a page is rendered from a stored build.

`summarize_rows` is a Python reference of the scorecard's summary over ledger
rows, without the parts that need hermit's catalogue (retired-id
canonicalization, commit depth, legacy history). The website never calls it;
the fixture and cross-check tests use it to build a summary from real rows.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import math
import re
import sys
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


SCHEMA = "parity-summary/v1"
LEDGER_PATH = "scorecard/parity.json"
#: The ledger stores the summary is computed from (see parity_ledger.py).
ROW_STORE = "parity"
RUN_STORE = "parity-runs"
PRODUCERS = ("validate", "pressure-test")
BACKENDS = ("dbt", "kvm", "liteinst", "sabre")
LEDGER_VERDICTS = (
    "record-missing",
    "unavailable",
    "reference-missing",
    "candidate-missing",
    "inputs-not-equalized",
    "diverged",
    "matched",
)
MEASURED_VERDICTS = frozenset({"matched", "diverged"})
#: A cell verdict is a ledger verdict, or `refused` for a row the scorecard
#: could attribute to its run but could not admit.
CELL_VERDICTS = ("refused",) + LEDGER_VERDICTS
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
CREDIT_BASES = ("clean", "unequalized")
LEGACY_RERUN_LABEL = "legacy-rerun"
#: The one-line text for a ledger capture that has no summary at all.
ABSENT_LINE = "parity: no parity summary in this ledger capture"
#: Recomputing a credit sum in another order may differ in the last bits.
CREDIT_SUM_TOLERANCE = 1e-9

SUMMARY_FIELDS = (
    "schema",
    "generated_from",
    "store_present",
    "rows_read",
    "producers",
    "runs",
    "refused_rows",
    "duplicate_rows",
    "refusals",
    "legacy_rerun",
    "legacy_dropped",
)
GENERATED_FROM_FIELDS = ("store_sha256", "series_commit", "tool_hermit_commit")
RUN_FIELDS = (
    "producer",
    "run_id",
    "hermit_sha",
    "depth",
    "emitted_at",
    "line",
    "per_backend",
    "total",
    "cells",
)
BRIEF_FIELDS = ("producer", "run_id", "hermit_sha", "depth", "emitted_at", "headline", "line")
DEPTH_FIELDS = ("commits", "first_parent")
TALLY_FIELDS = (
    "selected",
    "measured",
    "matched",
    "diverged",
    "unavailable",
    "unavailable_by_class",
    "record_missing",
    "refused",
    "credited",
    "clean_credited",
    "unequalized_credited",
    "credit_sum",
    "mean_credit",
    "floor_credit",
    "first_divergence",
)
GROUP_FIELDS = (
    "first_divergent_record",
    "syscall",
    "field",
    "cells",
    "of_diverged",
    "example_cell",
    "reference_message",
    "candidate_message",
)
CELL_FIELDS = (
    "cell",
    "emitted_cell",
    "test_id",
    "backend",
    "verdict",
    "unavailable_class",
    "reason",
    "credit",
    "credit_basis",
    "matched_prefix",
    "left_len",
    "right_len",
    "first_divergent_record",
    "first_difference_field",
    "first_difference_syscall",
    "reference_message",
    "candidate_message",
    "lane",
    "node",
)
REFUSAL_FIELDS = ("path", "line", "message")
LEGACY_FIELDS = ("label", "line", "matched", "diverged", "last_hermit_sha", "superseded", "entries")
LEGACY_ENTRY_FIELDS = (
    "cell",
    "test_id",
    "backend",
    "verdict",
    "compared_records",
    "first_divergent_record",
    "hermit_sha",
    "run_id",
    "depth",
    "determinism",
    "current_parity_verdict",
    "label",
)
CARRIAGE_FIELDS = (
    "path",
    "ledger_commit",
    "present",
    "git_object",
    "bytes",
    "sha256",
    "summary_json",
    "store_files",
    "run_marker_files",
)

_SHA1_RE = re.compile(r"[0-9a-f]{40}")
_SHA256_RE = re.compile(r"[0-9a-f]{64}")
_OBJECT_ID_RE = re.compile(r"[0-9a-f]{40,64}")
_PARITY_LEDGER_MODULE = "ci_hub_series_parity_ledger"
_PARITY_LEDGER_PATH = Path(__file__).resolve().parents[1] / "series" / "parity_ledger.py"


class ParitySummaryError(ValueError):
    """A parity summary or carriage that breaks the contract."""


def parity_ledger_module() -> Any:
    """`ci-hub/series/parity_ledger.py`, shared with every other loader of it
    (`series.parity_ledger_module` registers the same module name)."""
    module = sys.modules.get(_PARITY_LEDGER_MODULE)
    if module is not None:
        return module
    spec = importlib.util.spec_from_file_location(_PARITY_LEDGER_MODULE, _PARITY_LEDGER_PATH)
    if spec is None or spec.loader is None:
        raise ParitySummaryError(f"cannot load the parity ledger store from {_PARITY_LEDGER_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[_PARITY_LEDGER_MODULE] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(_PARITY_LEDGER_MODULE, None)
        raise
    return module


# ------------------------------------------------------------------ primitives


def _exact(value: Any, fields: Sequence[str], at: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ParitySummaryError(f"{at} must be an object")
    missing = [field for field in fields if field not in value]
    unknown = sorted(set(value) - set(fields))
    if missing:
        raise ParitySummaryError(f"{at} is missing field(s): {', '.join(missing)}")
    if unknown:
        raise ParitySummaryError(f"{at} has unknown field(s): {', '.join(unknown)}")
    return value


def _count(value: Any, at: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ParitySummaryError(f"{at} must be a non-negative integer, got {value!r}")
    return value


def _optional_count(value: Any, at: str) -> int | None:
    return None if value is None else _count(value, at)


def _string(value: Any, at: str) -> str:
    if not isinstance(value, str) or not value:
        raise ParitySummaryError(f"{at} must be a non-empty string, got {value!r}")
    return value


def _optional_string(value: Any, at: str) -> str | None:
    return None if value is None else _string(value, at)


def _boolean(value: Any, at: str) -> bool:
    if not isinstance(value, bool):
        raise ParitySummaryError(f"{at} must be a boolean, got {value!r}")
    return value


def _number(value: Any, at: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ParitySummaryError(f"{at} must be a finite number, got {value!r}")
    return float(value)


def _credit_value(value: Any, at: str) -> float | None:
    if value is None:
        return None
    number = _number(value, at)
    if not 0.0 <= number <= 1.0:
        raise ParitySummaryError(f"{at} must be within [0, 1], got {number!r}")
    return number


def _choice(value: Any, choices: Sequence[str], at: str) -> str:
    if value not in choices:
        raise ParitySummaryError(f"{at} must be one of {', '.join(choices)}; got {value!r}")
    return value


def _commit(value: Any, at: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or _SHA1_RE.fullmatch(value) is None:
        raise ParitySummaryError(f"{at} must be 40 lowercase hex digits, got {value!r}")
    return value


def _depth(value: Any, at: str) -> tuple[int, int] | None:
    if value is None:
        return None
    depth = _exact(value, DEPTH_FIELDS, at)
    return _count(depth["commits"], f"{at}.commits"), _count(depth["first_parent"], f"{at}.first_parent")


# ---------------------------------------------------------------------- credit


def credit_text(value: float | None) -> str:
    """A credit as the scorecard prints it: three decimals, or `n/a`."""
    return "n/a" if value is None else f"{value:.3f}"


def render_line(tally: Mapping[str, Any]) -> str:
    """hermit's `ParityTally::line`: the one-line summary of a tally.

    Mean credit is over the credited (measured) cells and floor credit over
    every selected cell; a tally with nothing credited prints `n/a`, never 0.
    """
    refused = f"; refused {tally['refused']}" if tally["refused"] > 0 else ""
    over = f" over {tally['credited']} credited" if tally["credited"] != tally["measured"] else ""
    if tally["credited"] > 0 and tally["clean_credited"] == 0:
        inputs = " [inputs not equalized]"
    elif tally["unequalized_credited"] > 0:
        inputs = (
            f" [inputs equalized for {tally['clean_credited']} of {tally['credited']} credited]"
        )
    else:
        inputs = ""
    return (
        f"parity: {tally['matched']}/{tally['selected']} matched; measured {tally['measured']}; "
        f"unavailable {tally['unavailable']}; record-missing {tally['record_missing']}{refused}; "
        f"mean credit (measured) {credit_text(tally['mean_credit'])}{over}; "
        f"floor credit (selected) {credit_text(tally['floor_credit'])}{inputs}"
    )


def _group_key(cell: Mapping[str, Any]) -> tuple:
    # Rust orders `None` before any `Some`; the first key of the largest group
    # wins a tie, as in hermit's `ParityTally::over`.
    def option(value: Any) -> tuple:
        return (0,) if value is None else (1, value)

    return (
        option(cell["first_divergent_record"]),
        option(cell["first_difference_syscall"]),
        option(cell["first_difference_field"]),
    )


def tally(cells: Iterable[Mapping[str, Any]]) -> dict[str, Any]:
    """hermit's `ParityTally::over`: counts and credit over some cells.

    `selected = measured + unavailable + record_missing + refused` and
    `measured = matched + diverged`. Only a cell with a credit adds to the
    credit sum; `mean_credit` divides that sum by the credited cells and
    `floor_credit` by every selected cell. Neither is ever computed from a
    null credit.
    """
    result: dict[str, Any] = {
        "selected": 0,
        "measured": 0,
        "matched": 0,
        "diverged": 0,
        "unavailable": 0,
        "unavailable_by_class": {name: 0 for name in UNAVAILABLE_CLASSES},
        "record_missing": 0,
        "refused": 0,
        "credited": 0,
        "clean_credited": 0,
        "unequalized_credited": 0,
        "credit_sum": 0.0,
        "mean_credit": None,
        "floor_credit": None,
        "first_divergence": None,
    }
    groups: dict[tuple, list[Mapping[str, Any]]] = {}
    for cell in cells:
        result["selected"] += 1
        verdict = cell["verdict"]
        if verdict == "refused":
            result["refused"] += 1
        elif verdict == "record-missing":
            result["record_missing"] += 1
        elif verdict == "matched":
            result["measured"] += 1
            result["matched"] += 1
        elif verdict == "diverged":
            result["measured"] += 1
            result["diverged"] += 1
            groups.setdefault(_group_key(cell), []).append(cell)
        else:
            result["unavailable"] += 1
            result["unavailable_by_class"][cell["unavailable_class"] or "other"] += 1
        if cell["credit"] is not None:
            result["credited"] += 1
            result["credit_sum"] += cell["credit"]
            if cell["credit_basis"] == "clean":
                result["clean_credited"] += 1
            else:
                result["unequalized_credited"] += 1
    if result["credited"] > 0:
        result["mean_credit"] = result["credit_sum"] / result["credited"]
    if result["selected"] > 0:
        result["floor_credit"] = result["credit_sum"] / result["selected"]
    best = None
    for key in sorted(groups):
        if best is None or len(groups[key]) > len(groups[best]):
            best = key
    if best is not None:
        example = groups[best][0]
        result["first_divergence"] = {
            "first_divergent_record": example["first_divergent_record"],
            "syscall": example["first_difference_syscall"],
            "field": example["first_difference_field"],
            "cells": len(groups[best]),
            "of_diverged": result["diverged"],
            "example_cell": example["cell"],
            "reference_message": example["reference_message"],
            "candidate_message": example["candidate_message"],
        }
    return result


# ------------------------------------------------------------------ validation


def _validate_cell(value: Any, at: str) -> Mapping[str, Any]:
    cell = _exact(value, CELL_FIELDS, at)
    test_id = _string(cell["test_id"], f"{at}.test_id")
    backend = _choice(cell["backend"], BACKENDS, f"{at}.backend")
    name = _string(cell["cell"], f"{at}.cell")
    at = f"{at} ({name})"
    if name != f"{test_id}@{backend}":
        raise ParitySummaryError(f"{at}: cell must be {test_id}@{backend}")
    _optional_string(cell["emitted_cell"], f"{at}.emitted_cell")
    verdict = _choice(cell["verdict"], CELL_VERDICTS, f"{at}.verdict")
    reason = _optional_string(cell["reason"], f"{at}.reason")
    credit = _credit_value(cell["credit"], f"{at}.credit")
    basis = cell["credit_basis"]
    if basis is not None:
        _choice(basis, CREDIT_BASES, f"{at}.credit_basis")
    if (credit is None) != (basis is None):
        raise ParitySummaryError(f"{at}: credit and credit_basis must both be set or both be null")
    if credit is not None and verdict not in MEASURED_VERDICTS:
        raise ParitySummaryError(f"{at}: a {verdict} cell carries credit {credit!r}; only a measured cell is credited")
    if verdict == "diverged" and credit is not None and credit >= 1.0:
        raise ParitySummaryError(f"{at}: a diverged cell's credit {credit!r} must be below 1")
    if verdict == "matched" and credit is not None and credit != 1.0:
        raise ParitySummaryError(f"{at}: a matched cell's credit must be 1, got {credit!r}")
    for field in ("matched_prefix", "left_len", "right_len", "first_divergent_record", "first_difference_syscall"):
        _optional_count(cell[field], f"{at}.{field}")
    for field in ("first_difference_field", "reference_message", "candidate_message"):
        if cell[field] is not None and not isinstance(cell[field], str):
            raise ParitySummaryError(f"{at}.{field} must be a string or null")
    stated_class = cell["unavailable_class"]
    if verdict in MEASURED_VERDICTS or verdict in {"record-missing", "refused"}:
        if stated_class is not None:
            raise ParitySummaryError(f"{at}: a {verdict} cell has no unavailable class, got {stated_class!r}")
    else:
        _choice(stated_class, UNAVAILABLE_CLASSES, f"{at}.unavailable_class")
        derived = parity_ledger_module().unavailable_class(verdict, reason)
        if stated_class != derived:
            raise ParitySummaryError(
                f"{at}: unavailable class {stated_class} is not the {derived} its verdict and reason imply"
            )
    if verdict == "refused":
        if reason is None:
            raise ParitySummaryError(f"{at}: a refused cell must carry its refusal as its reason")
        if cell["lane"] is not None or cell["node"] is not None:
            raise ParitySummaryError(f"{at}: a refused cell names no lane or node")
    else:
        _string(cell["lane"], f"{at}.lane")
        _string(cell["node"], f"{at}.node")
    return cell


def _same_number(left: float | None, right: float | None) -> bool:
    if left is None or right is None:
        return left is None and right is None
    return left == right


def _validate_tally(value: Any, cells: Sequence[Mapping[str, Any]], at: str) -> Mapping[str, Any]:
    stated = _exact(value, TALLY_FIELDS, at)
    for field in (
        "selected",
        "measured",
        "matched",
        "diverged",
        "unavailable",
        "record_missing",
        "refused",
        "credited",
        "clean_credited",
        "unequalized_credited",
    ):
        _count(stated[field], f"{at}.{field}")
    by_class = _exact(stated["unavailable_by_class"], UNAVAILABLE_CLASSES, f"{at}.unavailable_by_class")
    for name in UNAVAILABLE_CLASSES:
        _count(by_class[name], f"{at}.unavailable_by_class.{name}")
    credit_sum = _number(stated["credit_sum"], f"{at}.credit_sum")
    mean = _credit_value(stated["mean_credit"], f"{at}.mean_credit")
    floor = _credit_value(stated["floor_credit"], f"{at}.floor_credit")
    if stated["first_divergence"] is not None:
        group = _exact(stated["first_divergence"], GROUP_FIELDS, f"{at}.first_divergence")
        _count(group["cells"], f"{at}.first_divergence.cells")
        _count(group["of_diverged"], f"{at}.first_divergence.of_diverged")
    if stated["selected"] != (
        stated["measured"] + stated["unavailable"] + stated["record_missing"] + stated["refused"]
    ):
        raise ParitySummaryError(
            f"{at}: selected {stated['selected']} is not measured + unavailable + record-missing + refused"
        )
    if stated["measured"] != stated["matched"] + stated["diverged"]:
        raise ParitySummaryError(f"{at}: measured {stated['measured']} is not matched + diverged")
    if stated["unavailable"] != sum(by_class.values()):
        raise ParitySummaryError(f"{at}: unavailable {stated['unavailable']} is not the sum of its classes")
    if stated["credited"] != stated["clean_credited"] + stated["unequalized_credited"]:
        raise ParitySummaryError(f"{at}: credited is not clean_credited + unequalized_credited")
    if stated["credited"] > stated["measured"]:
        raise ParitySummaryError(f"{at}: {stated['credited']} credited cell(s) exceed {stated['measured']} measured")
    # Null credit is never averaged: the mean exists exactly when a cell was
    # credited, and it and the floor are the stated sum over their own
    # denominators.
    expected_mean = credit_sum / stated["credited"] if stated["credited"] else None
    expected_floor = credit_sum / stated["selected"] if stated["selected"] else None
    if not _same_number(mean, expected_mean):
        raise ParitySummaryError(
            f"{at}: mean_credit {mean!r} is not credit_sum / credited = {expected_mean!r}"
        )
    if not _same_number(floor, expected_floor):
        raise ParitySummaryError(
            f"{at}: floor_credit {floor!r} is not credit_sum / selected = {expected_floor!r}"
        )
    recomputed = tally(cells)
    for field in TALLY_FIELDS:
        if field in {"credit_sum", "mean_credit", "floor_credit"}:
            continue
        if stated[field] != recomputed[field]:
            raise ParitySummaryError(
                f"{at}: {field} {stated[field]!r} disagrees with its cells, which give {recomputed[field]!r}"
            )
    if abs(credit_sum - recomputed["credit_sum"]) > CREDIT_SUM_TOLERANCE:
        raise ParitySummaryError(
            f"{at}: credit_sum {credit_sum!r} disagrees with its cells, which give {recomputed['credit_sum']!r}"
        )
    return stated


def _validate_run(value: Any, at: str) -> Mapping[str, Any]:
    run = _exact(value, RUN_FIELDS, at)
    producer = _choice(run["producer"], PRODUCERS, f"{at}.producer")
    run_id = _string(run["run_id"], f"{at}.run_id")
    at = f"{at} ({producer} run {run_id})"
    _commit(run["hermit_sha"], f"{at}.hermit_sha")
    _depth(run["depth"], f"{at}.depth")
    _optional_string(run["emitted_at"], f"{at}.emitted_at")
    raw_cells = run["cells"]
    if not isinstance(raw_cells, list):
        raise ParitySummaryError(f"{at}.cells must be a list")
    cells = [_validate_cell(cell, f"{at}.cells[{index}]") for index, cell in enumerate(raw_cells)]
    seen: set[tuple[str, str]] = set()
    for cell in cells:
        key = (cell["test_id"], cell["backend"])
        if key in seen:
            raise ParitySummaryError(f"{at}: cell {cell['cell']} is reported twice")
        seen.add(key)
    per_backend = _exact(run["per_backend"], BACKENDS, f"{at}.per_backend")
    for backend in BACKENDS:
        _validate_tally(
            per_backend[backend],
            [cell for cell in cells if cell["backend"] == backend],
            f"{at}.per_backend.{backend}",
        )
    total = _validate_tally(run["total"], cells, f"{at}.total")
    line = _string(run["line"], f"{at}.line")
    if line != render_line(total):
        raise ParitySummaryError(f"{at}: line {line!r} is not its total's {render_line(total)!r}")
    return run


def _validate_legacy(value: Any, at: str) -> Mapping[str, Any]:
    legacy = _exact(value, LEGACY_FIELDS, at)
    if legacy["label"] != LEGACY_RERUN_LABEL:
        raise ParitySummaryError(f"{at}.label must be {LEGACY_RERUN_LABEL}, got {legacy['label']!r}")
    entries = legacy["entries"]
    if not isinstance(entries, list):
        raise ParitySummaryError(f"{at}.entries must be a list")
    depths: list[tuple[tuple[int, int], str]] = []
    for index, raw in enumerate(entries):
        entry_at = f"{at}.entries[{index}]"
        entry = _exact(raw, LEGACY_ENTRY_FIELDS, entry_at)
        _string(entry["cell"], f"{entry_at}.cell")
        _choice(entry["verdict"], ("matched", "diverged"), f"{entry_at}.verdict")
        _count(entry["compared_records"], f"{entry_at}.compared_records")
        _optional_count(entry["first_divergent_record"], f"{entry_at}.first_divergent_record")
        sha = _commit(entry["hermit_sha"], f"{entry_at}.hermit_sha")
        depth = _depth(entry["depth"], f"{entry_at}.depth")
        if depth is None:
            raise ParitySummaryError(f"{entry_at}.depth must be set")
        if entry["label"] != LEGACY_RERUN_LABEL:
            raise ParitySummaryError(f"{entry_at}.label must be {LEGACY_RERUN_LABEL}")
        depths.append((depth, str(sha)))
    matched = sum(entry["verdict"] == "matched" for entry in entries)
    if _count(legacy["matched"], f"{at}.matched") != matched:
        raise ParitySummaryError(f"{at}.matched {legacy['matched']} disagrees with its {matched} matched entries")
    if _count(legacy["diverged"], f"{at}.diverged") != len(entries) - matched:
        raise ParitySummaryError(f"{at}.diverged disagrees with its entries")
    _count(legacy["superseded"], f"{at}.superseded")
    last = _commit(legacy["last_hermit_sha"], f"{at}.last_hermit_sha")
    if depths:
        deepest = max(depth for depth, _sha in depths)
        if last not in {sha for depth, sha in depths if depth == deepest}:
            raise ParitySummaryError(f"{at}.last_hermit_sha is not the commit of its deepest entry")
        expected = (
            f"{LEGACY_RERUN_LABEL} (retired ptrace rerun, last at {last[:12]}): "
            f"{matched} matched / {len(entries) - matched} diverged"
        )
    else:
        if last is not None:
            raise ParitySummaryError(f"{at}.last_hermit_sha must be null without entries")
        expected = f"{LEGACY_RERUN_LABEL}: no retired ptrace-rerun comparison is retained"
    if legacy["line"] != expected:
        raise ParitySummaryError(f"{at}.line {legacy['line']!r} is not {expected!r}")
    return legacy


def validate_summary(value: Any) -> Mapping[str, Any]:
    """Check one decoded `parity-summary/v1` document and return it."""
    at = "parity summary"
    summary = _exact(value, SUMMARY_FIELDS, at)
    if summary["schema"] != SCHEMA:
        raise ParitySummaryError(f"{at}: schema must be {SCHEMA}, got {summary['schema']!r}")
    generated = _exact(summary["generated_from"], GENERATED_FROM_FIELDS, f"{at}.generated_from")
    store_sha256 = generated["store_sha256"]
    if store_sha256 is not None and (
        not isinstance(store_sha256, str) or _SHA256_RE.fullmatch(store_sha256) is None
    ):
        raise ParitySummaryError(f"{at}.generated_from.store_sha256 must be 64 lowercase hex digits")
    _commit(generated["series_commit"], f"{at}.generated_from.series_commit")
    _commit(generated["tool_hermit_commit"], f"{at}.generated_from.tool_hermit_commit")
    store_present = _boolean(summary["store_present"], f"{at}.store_present")
    rows_read = _count(summary["rows_read"], f"{at}.rows_read")
    refused_rows = _count(summary["refused_rows"], f"{at}.refused_rows")
    _count(summary["duplicate_rows"], f"{at}.duplicate_rows")
    if refused_rows > rows_read:
        raise ParitySummaryError(f"{at}: {refused_rows} refused row(s) exceed the {rows_read} read")
    refusals = summary["refusals"]
    if not isinstance(refusals, list):
        raise ParitySummaryError(f"{at}.refusals must be a list")
    for index, raw in enumerate(refusals):
        refusal = _exact(raw, REFUSAL_FIELDS, f"{at}.refusals[{index}]")
        _string(refusal["path"], f"{at}.refusals[{index}].path")
        _optional_count(refusal["line"], f"{at}.refusals[{index}].line")
        _string(refusal["message"], f"{at}.refusals[{index}].message")
    if len(refusals) < refused_rows:
        raise ParitySummaryError(f"{at}: {refused_rows} refused row(s) but only {len(refusals)} refusal(s) listed")
    producers = summary["producers"]
    runs = summary["runs"]
    if not isinstance(producers, list):
        raise ParitySummaryError(f"{at}.producers must be a list")
    if not isinstance(runs, list):
        raise ParitySummaryError(f"{at}.runs must be a list")
    if not store_present and (rows_read or producers or runs or store_sha256 is not None):
        raise ParitySummaryError(f"{at}: a summary without a parity store reports no rows or runs")
    headline_runs = [_validate_run(run, f"{at}.producers[{index}]") for index, run in enumerate(producers)]
    order = [run["producer"] for run in headline_runs]
    if order != sorted(set(order), key=PRODUCERS.index):
        raise ParitySummaryError(f"{at}.producers must hold one run per producer, in producer order")
    for run in headline_runs:
        if run["total"]["refused"] > refused_rows:
            raise ParitySummaryError(
                f"{at}: {run['producer']} run {run['run_id']} counts more refused cells than the store refused"
            )
    briefs = []
    for index, raw in enumerate(runs):
        brief_at = f"{at}.runs[{index}]"
        brief = _exact(raw, BRIEF_FIELDS, brief_at)
        _choice(brief["producer"], PRODUCERS, f"{brief_at}.producer")
        _string(brief["run_id"], f"{brief_at}.run_id")
        _commit(brief["hermit_sha"], f"{brief_at}.hermit_sha")
        _depth(brief["depth"], f"{brief_at}.depth")
        _optional_string(brief["emitted_at"], f"{brief_at}.emitted_at")
        _boolean(brief["headline"], f"{brief_at}.headline")
        _string(brief["line"], f"{brief_at}.line")
        briefs.append(brief)
    keys = [(brief["producer"], brief["run_id"]) for brief in briefs]
    if len(set(keys)) != len(keys):
        raise ParitySummaryError(f"{at}.runs names a run twice")
    headlines = {(brief["producer"], brief["run_id"]): brief for brief in briefs if brief["headline"]}
    if len(headlines) != len(headline_runs):
        raise ParitySummaryError(
            f"{at}: {len(headlines)} headline run(s) in runs but {len(headline_runs)} in producers"
        )
    for run in headline_runs:
        brief = headlines.get((run["producer"], run["run_id"]))
        if brief is None:
            raise ParitySummaryError(
                f"{at}: {run['producer']} run {run['run_id']} is not a headline in runs"
            )
        for field in ("hermit_sha", "depth", "emitted_at", "line"):
            if brief[field] != run[field]:
                raise ParitySummaryError(
                    f"{at}: {run['producer']} run {run['run_id']} has {field} {run[field]!r} "
                    f"in producers but {brief[field]!r} in runs"
                )
    _validate_legacy(summary["legacy_rerun"], f"{at}.legacy_rerun")
    dropped = summary["legacy_dropped"]
    if not isinstance(dropped, Mapping):
        raise ParitySummaryError(f"{at}.legacy_dropped must be an object")
    for reason, count in dropped.items():
        _string(reason, f"{at}.legacy_dropped key")
        _count(count, f"{at}.legacy_dropped.{reason}")
    return summary


def load_summary(text: str) -> Mapping[str, Any]:
    """Decode and check the exact text of `scorecard/parity.json`."""

    def unique(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ParitySummaryError(f"parity summary repeats the key {key!r}")
            value[key] = item
        return value

    def constant(name: str) -> Any:
        raise ParitySummaryError(f"parity summary contains the non-JSON constant {name}")

    try:
        value = json.loads(text, object_pairs_hook=unique, parse_constant=constant)
    except json.JSONDecodeError as error:
        raise ParitySummaryError(f"{LEDGER_PATH} is not JSON: {error}") from error
    return validate_summary(value)


# -------------------------------------------------------------------- carriage


def absent_carriage(ledger_commit: str, store_files: int, run_marker_files: int) -> dict[str, Any]:
    """The carriage of a ledger commit that holds no `scorecard/parity.json`."""
    return {
        "path": LEDGER_PATH,
        "ledger_commit": ledger_commit,
        "present": False,
        "git_object": None,
        "bytes": None,
        "sha256": None,
        "summary_json": None,
        "store_files": store_files,
        "run_marker_files": run_marker_files,
    }


def present_carriage(
    ledger_commit: str, git_object: str, data: bytes, store_files: int, run_marker_files: int
) -> dict[str, Any]:
    """The carriage of a committed summary: its exact bytes, checked."""
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ParitySummaryError(f"{LEDGER_PATH} is not UTF-8: {error}") from error
    carriage = {
        "path": LEDGER_PATH,
        "ledger_commit": ledger_commit,
        "present": True,
        "git_object": git_object,
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "summary_json": text,
        "store_files": store_files,
        "run_marker_files": run_marker_files,
    }
    validate_carriage(carriage)
    return carriage


def validate_carriage(value: Any) -> Mapping[str, Any] | None:
    """Check a carriage and return its decoded summary, or None when absent."""
    at = "parity carriage"
    carriage = _exact(value, CARRIAGE_FIELDS, at)
    if carriage["path"] != LEDGER_PATH:
        raise ParitySummaryError(f"{at}.path must be {LEDGER_PATH}, got {carriage['path']!r}")
    if _commit(carriage["ledger_commit"], f"{at}.ledger_commit") is None:
        raise ParitySummaryError(f"{at}.ledger_commit must be set")
    store_files = _count(carriage["store_files"], f"{at}.store_files")
    _count(carriage["run_marker_files"], f"{at}.run_marker_files")
    present = _boolean(carriage["present"], f"{at}.present")
    if not present:
        for field in ("git_object", "bytes", "sha256", "summary_json"):
            if carriage[field] is not None:
                raise ParitySummaryError(f"{at}: an absent summary has no {field}")
        return None
    git_object = carriage["git_object"]
    if not isinstance(git_object, str) or _OBJECT_ID_RE.fullmatch(git_object) is None:
        raise ParitySummaryError(f"{at}.git_object must be a lowercase 40- or 64-hex object id")
    text = carriage["summary_json"]
    if not isinstance(text, str):
        raise ParitySummaryError(f"{at}.summary_json must be a string")
    data = text.encode("utf-8")
    if _count(carriage["bytes"], f"{at}.bytes") != len(data):
        raise ParitySummaryError(f"{at}.bytes {carriage['bytes']} is not the {len(data)} byte(s) carried")
    if carriage["sha256"] != hashlib.sha256(data).hexdigest():
        raise ParitySummaryError(f"{at}.sha256 disagrees with the bytes carried")
    summary = load_summary(text)
    if not summary["store_present"]:
        raise ParitySummaryError(
            f"{at}: the scorecard writes {LEDGER_PATH} only for a ledger with a {ROW_STORE}/ store"
        )
    if store_files == 0:
        raise ParitySummaryError(
            f"{at}: the summary reports a {ROW_STORE}/ store but ledger commit "
            f"{carriage['ledger_commit']} has no {ROW_STORE}/ shard"
        )
    return summary


# ------------------------------------------------------------------- reference


def _cell_from_row(row: Mapping[str, Any]) -> dict[str, Any]:
    """hermit's `ParityCellSummary::from_row`, with the row's own test id."""
    ledger = parity_ledger_module()
    record = row["record"]
    difference = (record or {}).get("first_difference") or {}
    credit = None
    if record is not None and row["verdict"] in MEASURED_VERDICTS:
        credit = ledger.measured_credit(record)
    basis = None
    if credit is not None:
        basis = "clean" if record.get("credit") is not None else "unequalized"
    return {
        "cell": f"{row['test_id']}@{row['backend']}",
        "emitted_cell": None,
        "test_id": row["test_id"],
        "backend": row["backend"],
        "verdict": row["verdict"],
        "unavailable_class": ledger.unavailable_class(row["verdict"], row["reason"]),
        "reason": row["reason"],
        "credit": credit,
        "credit_basis": basis,
        "matched_prefix": (record or {}).get("matched_prefix"),
        "left_len": (record or {}).get("left_len"),
        "right_len": (record or {}).get("right_len"),
        "first_divergent_record": (record or {}).get("first_divergent_record"),
        "first_difference_field": difference.get("field"),
        "first_difference_syscall": difference.get("syscall"),
        "reference_message": difference.get("reference_message"),
        "candidate_message": difference.get("candidate_message"),
        "lane": row["source"]["lane"],
        "node": row["source"]["node"],
    }


def _rank(cell: Mapping[str, Any]) -> int:
    # A refused row is the most adverse, then the ledger verdicts in order.
    return CELL_VERDICTS.index(cell["verdict"])


def summarize_rows(
    rows: Sequence[Mapping[str, Any]],
    *,
    store_sha256: str | None = None,
    series_commit: str | None = None,
    tool_hermit_commit: str | None = None,
) -> dict[str, Any]:
    """A `parity-summary/v1` document over already admitted ledger rows.

    The Python reference of hermit's `summarize_parity` for rows that pass
    `parity_ledger.validate_row`: one run per (producer, run_id), the most
    adverse report of a cell kept, every tally from `tally`. It has no
    catalogue, so a test id is its own canonical id, no run has a depth, the
    headline of a producer is its latest emission, and the legacy history is
    empty. A row that does not validate raises; the scorecard's refusal
    accounting is not reproduced here.
    """
    ledger = parity_ledger_module()
    runs: dict[tuple[str, str], dict[str, Any]] = {}
    duplicates = 0
    for row in rows:
        ledger.validate_row(row)
        key = (row["producer"], row["run_id"])
        run = runs.setdefault(key, {"hermit_sha": row["hermit_sha"], "emitted_at": None, "cells": {}})
        if run["hermit_sha"] != row["hermit_sha"]:
            raise ParitySummaryError(
                f"parity ledger row {row['cell']} ({row['producer']} run {row['run_id']}): "
                f"hermit_sha {row['hermit_sha']} differs from the run's {run['hermit_sha']}"
            )
        if run["emitted_at"] is None or row["emitted_at"] > run["emitted_at"]:
            run["emitted_at"] = row["emitted_at"]
        cell = _cell_from_row(row)
        cell_key = (cell["test_id"], cell["backend"])
        kept = run["cells"].get(cell_key)
        if kept is None:
            run["cells"][cell_key] = cell
            continue
        duplicates += 1
        if _rank(cell) < _rank(kept) or (
            _rank(cell) == _rank(kept) and (cell["credit"] or 0.0) < (kept["credit"] or 0.0)
        ):
            run["cells"][cell_key] = cell
    summaries = []
    for (producer, run_id) in sorted(runs, key=lambda key: (PRODUCERS.index(key[0]), key[1])):
        run = runs[(producer, run_id)]
        cells = [run["cells"][key] for key in sorted(run["cells"], key=lambda key: (key[0].encode(), key[1]))]
        total = tally(cells)
        summaries.append(
            {
                "producer": producer,
                "run_id": run_id,
                "hermit_sha": run["hermit_sha"],
                "depth": None,
                "emitted_at": run["emitted_at"],
                "line": render_line(total),
                "per_backend": {
                    backend: tally(cell for cell in cells if cell["backend"] == backend)
                    for backend in BACKENDS
                },
                "total": total,
                "cells": cells,
            }
        )
    headline: dict[str, int] = {}
    for index, run in enumerate(summaries):
        current = headline.get(run["producer"])
        if current is None or (run["emitted_at"] or "", run["run_id"]) > (
            summaries[current]["emitted_at"] or "",
            summaries[current]["run_id"],
        ):
            headline[run["producer"]] = index
    chosen = set(headline.values())
    return {
        "schema": SCHEMA,
        "generated_from": {
            "store_sha256": store_sha256,
            "series_commit": series_commit,
            "tool_hermit_commit": tool_hermit_commit,
        },
        "store_present": True,
        "rows_read": len(rows),
        "producers": [summaries[headline[producer]] for producer in PRODUCERS if producer in headline],
        "runs": [
            {
                "producer": run["producer"],
                "run_id": run["run_id"],
                "hermit_sha": run["hermit_sha"],
                "depth": None,
                "emitted_at": run["emitted_at"],
                "headline": index in chosen,
                "line": run["line"],
            }
            for index, run in enumerate(summaries)
        ],
        "refused_rows": 0,
        "duplicate_rows": duplicates,
        "refusals": [],
        "legacy_rerun": {
            "label": LEGACY_RERUN_LABEL,
            "line": f"{LEGACY_RERUN_LABEL}: no retired ptrace-rerun comparison is retained",
            "matched": 0,
            "diverged": 0,
            "last_hermit_sha": None,
            "superseded": 0,
            "entries": [],
        },
        "legacy_dropped": {},
    }


def encode_summary(summary: Mapping[str, Any]) -> bytes:
    """The summary as the scorecard writes it: pretty JSON and a newline."""
    return (json.dumps(summary, indent=2, ensure_ascii=False, allow_nan=False) + "\n").encode()
