#!/usr/bin/env python3
"""Derive and read the exact-cell measured-flake registry.

One registry entry is evidence from one exact ``cell`` and Hermit ``tree``.
Rows without a comparison do not contribute to the sample.  A cell is included
only when both ``passed`` and ``diverged`` were observed at that same tree.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
import json
import os
from pathlib import Path
import re
import sys
from typing import Iterable, Mapping

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "series"))
import series as series_store  # noqa: E402


SCHEMA = 2
DEFAULT_REGISTRY = Path(__file__).with_name("flaky-cells.json")
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_CELL_RE = re.compile(r"^[A-Za-z0-9._/-]+/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$")
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_COMMENT = (
    "Generated from clean parent stress/flakiness series rows. A cell appears "
    "only when passed and diverged comparisons were both observed at the exact "
    "Hermit tree. sample_size counts compared outcomes only; no_result, timeout, "
    "errored, and skipped rows are excluded. sample_size does not state a failure "
    "rate: 29 observations with none of one outcome are needed for a 95% upper "
    "bound below 10%, and 59 for a bound below 5%."
)


def validate_registry(value: object) -> dict:
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise ValueError(f"flake registry schema must be {SCHEMA}")
    entries = value.get("cells")
    if not isinstance(entries, list):
        raise ValueError("flake registry cells must be a list")
    seen: set[tuple[str, str]] = set()
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise ValueError(f"flake registry cell {index} must be an object")
        cell = entry.get("cell")
        tree = entry.get("tree")
        observed_pass = entry.get("observed_pass")
        observed_fail = entry.get("observed_fail")
        sample_size = entry.get("sample_size")
        measured_at = entry.get("measured_at")
        producers = entry.get("producers")
        if not isinstance(cell, str) or _CELL_RE.fullmatch(cell) is None:
            raise ValueError(
                f"flake registry cell {index} has an invalid exact cell id"
            )
        if not isinstance(tree, str) or _SHA_RE.fullmatch(tree) is None:
            raise ValueError(f"flake registry cell {cell!r} has an invalid tree")
        if entry.get("product") != "hermit" or entry.get("source") != "series":
            raise ValueError(
                f"flake registry cell {cell!r} is not derived from the Hermit series"
            )
        for name, count in (
            ("observed_pass", observed_pass),
            ("observed_fail", observed_fail),
            ("sample_size", sample_size),
        ):
            if not isinstance(count, int) or isinstance(count, bool) or count < 1:
                raise ValueError(f"flake registry cell {cell!r} has invalid {name}")
        if sample_size != observed_pass + observed_fail:
            raise ValueError(
                f"flake registry cell {cell!r} sample_size differs from pass+fail"
            )
        if not isinstance(measured_at, str) or _DATE_RE.fullmatch(measured_at) is None:
            raise ValueError(f"flake registry cell {cell!r} has invalid measured_at")
        if (
            not isinstance(producers, list)
            or not producers
            or any(not isinstance(item, str) or not item for item in producers)
            or producers != sorted(set(producers))
        ):
            raise ValueError(f"flake registry cell {cell!r} has invalid producers")
        identity = (cell, tree)
        if identity in seen:
            raise ValueError(
                f"flake registry repeats exact population {cell!r} at {tree}"
            )
        seen.add(identity)
    return value


def load_registry(path: Path = DEFAULT_REGISTRY) -> dict:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read flake registry {path}: {error}") from error
    return validate_registry(value)


def cells_for_tree(registry: Mapping[str, object], tree: str) -> set[str]:
    validate_registry(dict(registry))
    return {
        str(entry["cell"])
        for entry in registry["cells"]
        if isinstance(entry, Mapping) and entry.get("tree") == tree
    }


def derive(rows: Iterable[Mapping[str, object]]) -> dict:
    rows = series_store.validate_read_rows(dict(row) for row in rows)
    populations: dict[tuple[str, str], dict[str, object]] = defaultdict(
        lambda: {
            "observed_pass": 0,
            "observed_fail": 0,
            "measured_at": "",
            "producers": set(),
        }
    )
    for row in rows:
        if (
            row.get("schema") not in series_store.READABLE_ENVELOPE_VERSIONS
            or row.get("team") != "hermit"
        ):
            continue
        payload = row.get("series")
        if not isinstance(payload, Mapping):
            continue
        # A dirty source is not the exact checked-in tree named by ``tree``.
        if payload.get("source_tree_dirty") is not False:
            continue
        cell = payload.get("cell")
        tree = payload.get("tree")
        outcome = payload.get("outcome")
        count = payload.get("num_runs", 1)
        if (
            not isinstance(cell, str)
            or _CELL_RE.fullmatch(cell) is None
            or not isinstance(tree, str)
            or _SHA_RE.fullmatch(tree) is None
            or outcome not in {"passed", "diverged"}
            or not isinstance(count, int)
            or isinstance(count, bool)
            or count < 1
        ):
            continue
        population = populations[(cell, tree)]
        key = "observed_pass" if outcome == "passed" else "observed_fail"
        population[key] = int(population[key]) + count
        emitted_at = str(row.get("emitted_at") or "")[:10]
        if _DATE_RE.fullmatch(emitted_at) and emitted_at > str(
            population["measured_at"]
        ):
            population["measured_at"] = emitted_at
        producer = row.get("producer")
        if isinstance(producer, Mapping):
            producer = producer.get("tool")
        if isinstance(producer, str) and producer:
            producers = population["producers"]
            assert isinstance(producers, set)
            producers.add(producer)

    entries = []
    for (cell, tree), counts in sorted(populations.items()):
        observed_pass = int(counts["observed_pass"])
        observed_fail = int(counts["observed_fail"])
        if observed_pass == 0 or observed_fail == 0:
            continue
        producers = sorted(counts["producers"])
        entries.append(
            {
                "cell": cell,
                "product": "hermit",
                "tree": tree,
                "observed_pass": observed_pass,
                "observed_fail": observed_fail,
                "sample_size": observed_pass + observed_fail,
                "measured_at": counts["measured_at"],
                "source": "series",
                "producers": producers,
            }
        )
    return validate_registry({"schema": SCHEMA, "_comment": _COMMENT, "cells": entries})


def render(registry: Mapping[str, object]) -> str:
    validate_registry(dict(registry))
    return json.dumps(registry, indent=2, sort_keys=False) + "\n"


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(text)
    os.replace(temporary, path)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("derive",))
    parser.add_argument(
        "--parent", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)

    output = args.output or args.parent / "ci-hub/validate/flaky-cells.json"
    try:
        rows = series_store.published_rows(args.parent)
        registry = derive(rows)
        _write(output, render(registry))
    except (OSError, ValueError, series_store.SeriesError) as error:
        print(f"flaky-cells: cannot derive registry: {error}", file=sys.stderr)
        return 1
    print(
        f"flaky-cells: wrote {len(registry['cells'])} cell(s) from {len(rows)} row(s) to {output}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
