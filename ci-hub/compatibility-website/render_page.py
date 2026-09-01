#!/usr/bin/env python3
"""Render a deterministic static compatibility site from Stage 1 artifacts.

This remains a fixture-backed prototype, not current project status. It does
not read a ledger or infer a result. The Stage 1 artifacts have already done
that work; this renderer validates those artifacts through the same build-data
validator and presents their derived fields without JavaScript or a server.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import html
import importlib.util
import json
import os
import posixpath
import re
import sys
import uuid
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any


WRITE_DATA_PATH = Path(__file__).resolve().with_name("write_data.py")
SITE_CSS_PATH = Path(__file__).resolve().parent / "assets/site.css"
STATE_ORDER = ("green", "red", "never-measured", "unavailable")
STATE_LABELS = {
    "green": "Green",
    "red": "Red",
    "never-measured": "Never measured",
    "unavailable": "Unavailable",
}
STATE_EXPLANATIONS = {
    "green": "the latest qualifying execution passed after retries were reduced",
    "red": (
        "the latest qualifying execution ran and every retry ended in a "
        "product failure"
    ),
    "never-measured": "no qualifying canonical row exists",
    "unavailable": "the recorded evidence cannot support an honest green or red result",
}
DETAIL_DIRECTORIES = ("assets", "cells", "incomplete-data", "runs", "tests")
HEX_SHA256 = re.compile(r"[0-9a-f]{64}")


class RenderRefused(Exception):
    """The supplied Stage 1 artifacts or output destination are unsafe."""


@dataclass(frozen=True)
class SiteModel:
    build: Mapping[str, Any]
    data: Mapping[str, Any]
    cells: tuple[Mapping[str, Any], ...]
    tests: tuple[Mapping[str, Any], ...]
    custom_commands: tuple[Mapping[str, Any], ...]
    incomplete: tuple[Mapping[str, Any], ...]
    runs: Mapping[str, Any]
    series_rows: tuple[Mapping[str, Any], ...]
    history_rows: tuple[Mapping[str, Any], ...]
    current_history_ids: frozenset[str]
    cell_by_id: Mapping[str, Mapping[str, Any]]
    test_by_id: Mapping[str, Mapping[str, Any]]
    cell_paths: Mapping[str, str]
    test_paths: Mapping[str, str]
    run_paths: Mapping[str, str]


@functools.cache
def _writer() -> Any:
    spec = importlib.util.spec_from_file_location(
        "compatibility_website_stage1_reader", WRITE_DATA_PATH
    )
    if spec is None or spec.loader is None:
        raise RenderRefused(f"cannot load {WRITE_DATA_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as error:
        sys.modules.pop(spec.name, None)
        raise RenderRefused(f"cannot load {WRITE_DATA_PATH}: {error}") from error
    return module


def _artifact_source(
    build: Mapping[str, Any],
    generator: Mapping[str, Any],
    payloads: Mapping[str, Any],
) -> dict[str, Any]:
    return {
        "cells": payloads["data/cells.json.gz"]["cells"],
        "counts": build["counts"],
        "incomplete_data": payloads["data/incomplete-data.json.gz"]["incomplete_data"],
        "provenance": build["provenance"],
        "runs": payloads["data/runs.json.gz"],
        "schema_version": generator["build_data_schema_version"],
        "selected_by_full_custom_commands": payloads["data/tests.json.gz"][
            "selected_by_full_custom_commands"
        ],
        "tests": payloads["data/tests.json.gz"]["tests"],
    }


def load_artifacts(root_value: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    """Read one exact Stage 1 tree and return its build manifest and data."""
    writer = _writer()
    raw_path = root_value.expanduser()
    if ".." in raw_path.parts:
        raise RenderRefused("Stage 1 artifact path must not contain path traversal")
    root = Path(os.path.abspath(raw_path))
    directory = data_directory = None
    try:
        directory = writer._open_parent(root)
        writer._require_inventory(directory, writer._ROOT_NAMES, "Stage 1 root")
        raw_build = writer._read_file(directory, "build.json", 1 << 20)
        build, generator, artifacts = writer._prior_manifest(raw_build)
        data_directory = writer._open_directory(directory, "data")
        writer._require_inventory(data_directory, writer._DATA_NAMES, "Stage 1 data")
        payloads, encoded_files = writer._prior_payloads(data_directory, artifacts)
        source = _artifact_source(build, generator, payloads)
        build_data = writer._build_data_module()
        try:
            validated = build_data.validate_build_data(source)
        except build_data.BuildRefused as error:
            raise RenderRefused(f"Stage 1 artifact data is invalid: {error}") from error
        expected = {"build.json": raw_build, **encoded_files}
        if writer._render_validated_tree(validated) != expected:
            raise RenderRefused(
                "Stage 1 artifact bytes disagree with their validated data"
            )
        writer._require_inventory(directory, writer._ROOT_NAMES, "Stage 1 root")
        writer._require_inventory(data_directory, writer._DATA_NAMES, "Stage 1 data")
        return json.loads(raw_build), validated
    except (KeyError, TypeError, ValueError, OSError, writer.WriteRefused) as error:
        raise RenderRefused(f"Stage 1 artifacts refused: {error}") from error
    finally:
        if data_directory is not None:
            os.close(data_directory)
        if directory is not None:
            os.close(directory)


def _escape(value: Any) -> str:
    return html.escape(str(value), quote=True)


def _display(value: Any) -> str:
    if value is None or value == "":
        return '<span class="unavailable">Unavailable</span>'
    return _escape(value)


def _json_display(value: Any) -> str:
    if value is None:
        return _display(None)
    encoded = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True)
    return f"<pre><code>{_escape(encoded)}</code></pre>"


def _full_digest(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


def _fragment(kind: str, value: str) -> str:
    return f"{kind}-{_full_digest(value)}"


def _hashed_paths(directory: str, values: Sequence[str]) -> dict[str, str]:
    paths: dict[str, str] = {}
    owners: dict[str, str] = {}
    for value in values:
        digest = _full_digest(value)
        if not HEX_SHA256.fullmatch(digest):
            raise RenderRefused(f"cannot make a safe {directory} page path")
        previous = owners.get(digest)
        if previous is not None and previous != value:
            raise RenderRefused(f"two {directory} identities have the same page path")
        owners[digest] = value
        paths[value] = f"{directory}/{digest}.html"
    if len(paths) != len(values):
        raise RenderRefused(f"duplicate {directory} identity")
    return paths


def _href(source: str, target: str, fragment: str | None = None) -> str:
    source_path = PurePosixPath(source)
    target_path = PurePosixPath(target)
    if source_path == target_path and fragment is not None:
        return f"#{fragment}"
    relative = posixpath.relpath(target_path.as_posix(), source_path.parent.as_posix())
    return f"{relative}#{fragment}" if fragment is not None else relative


def _unique_rows(
    rows: Sequence[Mapping[str, Any]], key: str, label: str
) -> dict[str, Mapping[str, Any]]:
    result: dict[str, Mapping[str, Any]] = {}
    for row in rows:
        identity = str(row[key])
        if identity in result:
            raise RenderRefused(f"duplicate {label} {identity!r}")
        result[identity] = row
    return result


def _cell_identity(row: Mapping[str, Any]) -> str:
    return f"{row['test']}/{row['mode']}/{row['backend']}"


def _cell_state(cell: Mapping[str, Any]) -> str:
    value = cell.get("state")
    if value is None:
        return "unavailable"
    if value not in STATE_LABELS:
        raise RenderRefused(
            f"cell {cell.get('cell')!r} has unsupported state {value!r}"
        )
    return str(value)


def _series_by_cell(
    cells: Sequence[Mapping[str, Any]], rows: Sequence[Mapping[str, Any]]
) -> dict[str, tuple[Mapping[str, Any], ...]]:
    grouped: dict[str, list[Mapping[str, Any]]] = {
        str(cell["cell"]): [] for cell in cells
    }
    for row in rows:
        identity = str(row["series"]["cell"])
        if identity in grouped:
            grouped[identity].append(row)
    result = {identity: tuple(values) for identity, values in grouped.items()}
    for cell in cells:
        identity = str(cell["cell"])
        values = result[identity]
        represented = sum(int(row["series"]["num_runs"]) for row in values)
        if (
            len(values) != cell["physical_row_count"]
            or represented != cell["represented_run_count"]
        ):
            raise RenderRefused(f"cell {identity!r} series counts disagree")
    return result


def _validate_test_membership(
    tests: Sequence[Mapping[str, Any]],
    custom: Sequence[Mapping[str, Any]],
    cells: Mapping[str, Mapping[str, Any]],
) -> None:
    tests_by_id = _unique_rows(tests, "id", "test id")
    for test in tests:
        test_id = str(test["id"])
        for identity_value in test["cells"]:
            identity = str(identity_value)
            cell = cells.get(identity)
            if cell is None or str(cell["test"]) != test_id:
                raise RenderRefused(f"test {test_id!r} has an unknown cell")
    seen_custom: set[str] = set()
    for command in custom:
        identity = str(command["cell"])
        if identity in seen_custom:
            raise RenderRefused(f"duplicate custom command {identity!r}")
        seen_custom.add(identity)
        if str(command["test"]) not in tests_by_id:
            raise RenderRefused(f"custom command {identity!r} has an unknown test")


def _validate_counts(build: Mapping[str, Any], data: Mapping[str, Any]) -> None:
    cells = data["cells"]
    runs = data["runs"]
    counts = data["counts"]
    if build["counts"] != counts or build["provenance"] != data["provenance"]:
        raise RenderRefused("Stage 1 build manifest disagrees with rendered data")
    expected_states = {
        "green": counts["green_cell_count"],
        "red": counts["red_cell_count"],
        "never-measured": counts["never_measured_cell_count"],
        "unavailable": counts["state_unavailable_cell_count"],
    }
    observed_states = {
        state: sum(_cell_state(cell) == state for cell in cells)
        for state in STATE_ORDER
    }
    if (
        observed_states != expected_states
        or len(cells) != counts["in_manifest_cell_count"]
    ):
        raise RenderRefused("Stage 1 cell counts disagree with rendered populations")
    if (
        len(data["tests"]) != counts["test_count"]
        or len(data["selected_by_full_custom_commands"])
        != counts["selected_by_full_custom_command_count"]
        or len(data["incomplete_data"]) != counts["incomplete_data_count"]
    ):
        raise RenderRefused(
            "Stage 1 test, custom-command, or incomplete counts disagree"
        )
    series_rows = runs["series_rows"]
    represented = sum(int(row["series"]["num_runs"]) for row in series_rows)
    if (
        len(series_rows) != runs["series_physical_row_count"]
        or represented != runs["series_represented_run_count"]
        or len(runs["validation_history_rows"]) != runs["validation_history_row_count"]
        or len(runs["current_validation_run_rows"])
        != runs["current_validation_run_row_count"]
    ):
        raise RenderRefused("Stage 1 run counts disagree with rendered rows")
    _series_by_cell(cells, series_rows)


def _site_model(build: Mapping[str, Any], data: Mapping[str, Any]) -> SiteModel:
    cells = tuple(data["cells"])
    tests = tuple(data["tests"])
    custom = tuple(data["selected_by_full_custom_commands"])
    incomplete = tuple(data["incomplete_data"])
    runs = data["runs"]
    series_rows = tuple(runs["series_rows"])
    history_rows = tuple(runs["validation_history_rows"])
    current_rows = tuple(runs["current_validation_run_rows"])
    _validate_counts(build, data)
    cell_by_id = _unique_rows(cells, "cell", "cell")
    test_by_id = _unique_rows(tests, "id", "test id")
    history_by_id = _unique_rows(history_rows, "history_row_sha256", "history row")
    current_by_id = _unique_rows(current_rows, "history_row_sha256", "current row")
    for identity, row in history_by_id.items():
        if not HEX_SHA256.fullmatch(identity):
            raise RenderRefused(f"history row has an unsafe page identity {identity!r}")
        if identity in current_by_id and current_by_id[identity] != row:
            raise RenderRefused("current validation row disagrees with its history row")
    if not set(current_by_id) <= set(history_by_id):
        raise RenderRefused("current validation row is absent from validation history")
    _validate_test_membership(tests, custom, cell_by_id)
    return SiteModel(
        build=build,
        data=data,
        cells=cells,
        tests=tests,
        custom_commands=custom,
        incomplete=incomplete,
        runs=runs,
        series_rows=series_rows,
        history_rows=history_rows,
        current_history_ids=frozenset(current_by_id),
        cell_by_id=cell_by_id,
        test_by_id=test_by_id,
        cell_paths=_hashed_paths("cells", tuple(cell_by_id)),
        test_paths=_hashed_paths("tests", tuple(test_by_id)),
        run_paths={identity: f"runs/{identity}.html" for identity in history_by_id},
    )


def _reason(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, Mapping):
        return "; ".join(
            str(value[key])
            for key in ("reason", "result", "evidence")
            if value.get(key)
        )
    return str(value)


def _selected_by_full(cell: Mapping[str, Any]) -> str:
    if cell["selected_by_full"]:
        return "<strong>Selected by full</strong>"
    reason = _reason(cell.get("not_selected_by_full_reason"))
    suffix = f'<span class="secondary">{_escape(reason)}</span>' if reason else ""
    return f"<strong>Not selected by full</strong>{suffix}"


def _state_badge(cell: Mapping[str, Any]) -> str:
    state = _cell_state(cell)
    return (
        f'<span class="state state-{state}"><strong>{STATE_LABELS[state]}</strong>'
        f"<span>{_escape(STATE_EXPLANATIONS[state])}</span></span>"
    )


def _latest_result(cell: Mapping[str, Any]) -> str:
    value = cell.get("latest_result")
    labels = {"pass": "Pass", "product_failure": "Product failure"}
    if value is None:
        return _display(None)
    if value not in labels:
        raise RenderRefused(f"unsupported latest product result {value!r}")
    return labels[value]


def _series_product_result(row: Mapping[str, Any]) -> str:
    value = row.get("product_result")
    if not row["product_result_available"] or value is None:
        return _display(None)
    labels = {"pass": "Pass", "product_failure": "Product failure"}
    if value not in labels:
        raise RenderRefused(f"unsupported series product result {value!r}")
    return labels[value]


def _reproducer(cell: Mapping[str, Any]) -> str:
    value = cell.get("current_reproducer")
    if isinstance(value, Mapping):
        return f'<code class="command">{_escape(value["shell_command"])}</code>'
    return _display(cell.get("current_reproducer_unavailable_reason"))


def _series_position(payload: Mapping[str, Any]) -> str:
    parts = []
    if payload.get("attempt") is not None:
        parts.append(f"Attempt {_escape(payload['attempt'])}")
    if payload.get("run_index") is not None:
        parts.append(f"Run index {_escape(payload['run_index'])}")
    return "<br>".join(parts) if parts else _display(None)


def _runtime(payload: Mapping[str, Any]) -> str:
    runtime = payload.get("runtime")
    if not isinstance(runtime, Mapping):
        return _display(None)
    parts = []
    minimum = runtime.get("wall_time_min_ms")
    maximum = runtime.get("wall_time_max_ms")
    if minimum is not None or maximum is not None:
        parts.append(f"Wall time: {_display(minimum)}–{_display(maximum)} ms")
    for run in ("run1", "run2"):
        counts = runtime.get(run)
        if not isinstance(counts, Mapping):
            continue
        values = ", ".join(
            f"{_escape(field.replace('_', ' '))}: {_escape(value)}"
            for field in ("scheduler_turns", "syscalls", "virtual_nanoseconds")
            if (value := counts.get(field)) is not None
        )
        if values:
            parts.append(f"{_escape(run)}: {values}")
    return "<br>".join(parts) if parts else _display(None)


def _ancestry(value: Any) -> str:
    if value is True:
        return "On current main ancestry"
    if value is False:
        return "Not on current main ancestry"
    return _display(None)


def _cell_link(model: SiteModel, page: str, identity: str) -> str:
    path = model.cell_paths.get(identity)
    if path is None:
        return (
            f"<code>{_escape(identity)}</code>"
            '<span class="secondary">Not in the current manifest</span>'
        )
    return (
        f'<a href="{_escape(_href(page, path))}">'
        f"<code>{_escape(identity)}</code></a>"
    )


def _test_link(model: SiteModel, page: str, identity: str) -> str:
    return (
        f'<a href="{_escape(_href(page, model.test_paths[identity]))}">'
        f"<code>{_escape(identity)}</code></a>"
    )


def _run_selected_identities(
    row: Mapping[str, Any], path: str
) -> tuple[str, ...] | None:
    results = row.get("cell_results")
    if not isinstance(results, Mapping):
        return None
    selection = results["selected_by"].get(path)
    if not isinstance(selection, Mapping):
        return None
    return tuple(_cell_identity(cell) for cell in selection["cells"])


def _run_recorded_identities(row: Mapping[str, Any]) -> tuple[str, ...] | None:
    results = row.get("cell_results")
    if not isinstance(results, Mapping):
        return None
    return tuple(_cell_identity(cell) for cell in results["recorded"]["cells"])


def _selection_run_path(
    model: SiteModel,
    identity: str,
    path: str,
    fact: Mapping[str, Any],
) -> str | None:
    matches = []
    for row in model.history_rows:
        if row.get("run_id") != fact.get("run_id") or row["commit"] != fact.get("tree"):
            continue
        selected = _run_selected_identities(row, path)
        if selected is not None and identity in selected:
            matches.append(model.run_paths[str(row["history_row_sha256"])])
    return matches[0] if len(matches) == 1 else None


def _event_run_path(
    model: SiteModel, identity: str, event_id: str
) -> tuple[str | None, str]:
    cell = model.cell_by_id.get(identity)
    if cell is None:
        return None, "The series cell is not in the current manifest."
    facts = []
    for path, values in sorted(cell["selected_by"].items()):
        for fact in values:
            if event_id in fact["event_ids"]:
                facts.append((str(path), fact))
    if len(facts) != 1:
        return None, "No single retained validation-run selection names this event."
    path, fact = facts[0]
    run_path = _selection_run_path(model, identity, path, fact)
    if run_path is None:
        return None, "No single validation history row matches the retained selection."
    return run_path, f"Validation history row selected this cell by {path}."


def _incomplete_run_path(model: SiteModel, row: Mapping[str, Any]) -> str | None:
    record_id = row.get("record_id")
    run_id = row.get("run_id")
    if record_id is None and run_id is None:
        return None
    matches = [
        item
        for item in model.history_rows
        if (record_id is None or item.get("record_id") == record_id)
        and (run_id is None or item.get("run_id") == run_id)
    ]
    if len(matches) != 1:
        return None
    return model.run_paths[str(matches[0]["history_row_sha256"])]


def _breadcrumbs(page: str, values: Sequence[tuple[str, str | None]]) -> str:
    items = []
    for label, target in values:
        if target is None:
            items.append(f'<li aria-current="page">{_escape(label)}</li>')
        else:
            items.append(
                f'<li><a href="{_escape(_href(page, target))}">'
                f"{_escape(label)}</a></li>"
            )
    return (
        '<nav class="breadcrumbs" aria-label="Breadcrumb"><ol>'
        + "".join(items)
        + "</ol></nav>"
    )


def _layout(
    page: str,
    title: str,
    lead: str,
    content: str,
    breadcrumbs: Sequence[tuple[str, str | None]],
) -> bytes:
    nav = (
        f'<a href="{_escape(_href(page, "index.html"))}">Overview</a>'
        f'<a href="{_escape(_href(page, "cells/index.html"))}">Cells</a>'
        f'<a href="{_escape(_href(page, "index.html", "tests"))}">Tests</a>'
        f'<a href="{_escape(_href(page, "index.html", "validation-runs"))}">'
        "Validation runs</a>"
        f'<a href="{_escape(_href(page, "incomplete-data/index.html"))}">'
        "Incomplete data</a>"
    )
    source = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="Fixture-backed Hermit compatibility evidence">
  <title>{_escape(title)} · Hermit compatibility fixture</title>
  <link rel="stylesheet" href="{_escape(_href(page, 'assets/site.css'))}">
</head>
<body>
  <a class="skip-link" href="#main-content">Skip to main content</a>
  <header class="site-header">
    <p class="fixture-banner">Fixture data — not current Hermit main</p>
    <p class="site-name">Hermit compatibility</p>
    <nav aria-label="Primary navigation">{nav}</nav>
  </header>
  {_breadcrumbs(page, breadcrumbs)}
  <main id="main-content" tabindex="-1">
    <header class="page-heading"><h1>{_escape(title)}</h1><p>{_escape(lead)}</p></header>
    {content}
  </main>
  <footer><p>Generated locally from validated Stage 1 fixture artifacts. No network resources are used.</p></footer>
</body>
</html>
"""
    return source.encode()


def _count_link(
    source: str,
    target: str,
    fragment: str,
    count: int,
    label: str,
    represented: int | None = None,
) -> str:
    represented_attribute = (
        f' data-represented-count="{represented}"' if represented is not None else ""
    )
    visible = f"<strong>{count}</strong> {_escape(label)}"
    if represented is not None:
        visible += f" representing <strong>{represented}</strong> runs"
    return (
        f'<a href="{_escape(_href(source, target, fragment))}" data-count="{count}"'
        f"{represented_attribute}>{visible}</a>"
    )


def _state_summary(model: SiteModel, page: str) -> str:
    total = len(model.cells)
    cards = []
    for state in STATE_ORDER:
        count = sum(_cell_state(cell) == state for cell in model.cells)
        cards.append(
            f'<a class="metric state-{state}" '
            f'href="{_escape(_href(page, "cells/index.html", f"cells-{state}"))}" '
            f'data-count="{count}"><strong>{count} / {total}</strong>'
            f"<span>{STATE_LABELS[state]}</span></a>"
        )
    return '<div class="metrics">' + "".join(cards) + "</div>"


def _overview(model: SiteModel, page: str) -> str:
    runs = model.runs
    selected = sum(bool(cell["selected_by_full"]) for cell in model.cells)
    not_selected = len(model.cells) - selected
    return f"""
    <section id="overview">
      <h2>Overview</h2>
      <p class="population">{_count_link(page, 'cells/index.html', 'all-cells', len(model.cells), 'cells in the manifest')}</p>
      {_state_summary(model, page)}
      <div class="metrics compact-metrics">
        {_count_link(page, 'cells/index.html', 'selected-by-full', selected, 'cells selected by full')}
        {_count_link(page, 'cells/index.html', 'not-selected-by-full', not_selected, 'cells not selected by full')}
        {_count_link(page, 'index.html', 'custom-commands', len(model.custom_commands), 'custom commands selected by full')}
      </div>
      <div class="notice">
        <p><strong>Cells selected by quick: Unavailable.</strong></p>
        <p><strong>Cells selected by super: Unavailable.</strong></p>
        <p>The Stage 1 manifest export records the complete population selected by full only.
        A retained validation run may show that its named path selected a cell; that run evidence
        is not a complete quick or super population.</p>
      </div>
      <p class="evidence-counts">
        {_count_link(page, 'cells/index.html', 'series-rows', runs['series_physical_row_count'], 'public canonical series rows', runs['series_represented_run_count'])}.
        {_count_link(page, 'cells/index.html', 'series-admitted', runs['projection_admitted_physical_row_count'], 'rows with no projection exclusion reason', runs['projection_admitted_represented_run_count'])};
        {_count_link(page, 'cells/index.html', 'series-excluded', runs['projection_excluded_physical_row_count'], 'rows with a recorded projection exclusion reason', runs['projection_excluded_represented_run_count'])}.
        Physical rows and represented runs are different denominators.
      </p>
    </section>"""


def _custom_commands(model: SiteModel, page: str) -> str:
    items = []
    for command in model.custom_commands:
        applicability = command.get("not_applicable_reason")
        applicability_text = (
            f'<span class="secondary">Not applicable: {_escape(applicability)}</span>'
            if applicability
            else '<span class="secondary">Applicable</span>'
        )
        items.append(
            f'<li class="listing-card">{_test_link(model, page, str(command["test"]))}'
            f'<span class="secondary"><code>{_escape(command["cell"])}</code></span>'
            f'{applicability_text}<span class="secondary">Current reproducer</span>'
            f"{_reproducer(command)}</li>"
        )
    return f"""
    <section id="custom-commands" data-row-count="{len(items)}">
      <h2>Custom commands selected by full</h2>
      <p>These commands are not part of the {len(model.cells)}-cell manifest denominator.</p>
      <ul class="card-list">{"".join(items)}</ul>
    </section>"""


def _test_listing(model: SiteModel, page: str) -> str:
    items = []
    for test in model.tests:
        test_id = str(test["id"])
        items.append(
            '<li class="listing-card" data-test-row="1">'
            f"<h3>{_test_link(model, page, test_id)}</h3>"
            f"<p>{_escape(test['description'])}</p>"
            f'<p class="secondary">{len(test["cells"])} cells in the manifest · '
            f"lane {_escape(test['lane'])} · category {_escape(test['category'])}</p></li>"
        )
    return f"""
    <section id="tests" data-row-count="{len(items)}">
      <h2>Tests</h2>
      <p>Each test page shows its retained manifest metadata, cells, current reproducers,
      and facts the Stage 1 artifacts do not retain.</p>
      <ul class="card-list">{"".join(items)}</ul>
    </section>"""


def _selection_count(row: Mapping[str, Any]) -> tuple[str, int | None]:
    path = str(row["profile"])
    values = _run_selected_identities(row, path)
    return path, len(values) if values is not None else None


def _run_listing_row(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    identity = str(row["history_row_sha256"])
    target = model.run_paths[identity]
    path, selected_count = _selection_count(row)
    recorded = _run_recorded_identities(row)
    selected = (
        _count_link(
            page,
            target,
            _fragment("selected-by", path),
            selected_count,
            f"cells selected by {path}",
        )
        if selected_count is not None
        else _display(None)
    )
    recorded_link = (
        _count_link(page, target, "recorded-cells", len(recorded), "recorded cells")
        if recorded is not None
        else _display(None)
    )
    current = identity in model.current_history_ids
    return f"""
      <tr data-run-row="1" data-run-current="{str(current).lower()}">
        <th scope="row"><a href="{_escape(_href(page, target))}"><code>{_display(row.get('run_id'))}</code></a></th>
        <td>{'Current' if current else 'Historical'}</td><td>{_escape(path)}</td>
        <td>{selected}</td><td>{recorded_link}</td><td>{_display(row.get('result'))}</td>
        <td><code>{_escape(row['commit'])}</code></td><td>{_display(row.get('started_at'))}</td>
        <td>{_display(row.get('finished_at'))}</td>
      </tr>"""


def _run_listing(model: SiteModel, page: str) -> str:
    current = list(model.runs["current_validation_run_rows"])
    historical = [
        row
        for row in model.history_rows
        if str(row["history_row_sha256"]) not in model.current_history_ids
    ]
    return f"""
    <section id="validation-runs">
      <h2>Validation runs</h2>
      <p>{_count_link(page, page, 'current-validation-runs', len(current), 'current validation rows')} and
      {_count_link(page, page, 'historical-validation-runs', len(historical), 'historical validation rows')}.
      Whole-run result and cells selected by each named path are separate facts.</p>
      <div class="table-scroll"><table>
        <caption>Retained current and historical validation rows.</caption>
        <thead><tr><th scope="col">Run id</th><th scope="col">Record</th>
        <th scope="col">Validate path</th><th scope="col">Cells selected by that path</th>
        <th scope="col">Cells recorded</th><th scope="col">Run result</th>
        <th scope="col">Hermit commit</th><th scope="col">Started</th><th scope="col">Finished</th></tr></thead>
        <tbody id="current-validation-runs" data-row-count="{len(current)}">
          {"".join(_run_listing_row(model, page, row) for row in current)}
        </tbody>
        <tbody id="historical-validation-runs" data-row-count="{len(historical)}">
          {"".join(_run_listing_row(model, page, row) for row in historical)}
        </tbody>
      </table></div>
    </section>"""


def _provenance(model: SiteModel) -> str:
    provenance = model.build["provenance"]
    return f"""
    <section id="provenance">
      <h2>Fixture provenance</h2>
      <dl class="facts">
        <div><dt>Hermit commit used for manifest policy</dt><dd><code>{_escape(provenance['hermit_commit'])}</code></dd></div>
        <div><dt>Hermit main ancestry target</dt><dd><code>{_escape(provenance['hermit_main_commit'])}</code></dd></div>
        <div><dt>dev-hermit commit</dt><dd><code>{_escape(provenance['parent_commit'])}</code></dd></div>
        <div><dt>Manifest digest</dt><dd><code>{_escape(provenance['manifest_sha256'])}</code></dd></div>
        <div><dt>Captured series source digest</dt><dd><code>{_escape(provenance['series_rows_sha256'])}</code></dd></div>
        <div><dt>Public series projection digest</dt><dd><code>{_escape(provenance['series_rows_projection_sha256'])}</code></dd></div>
        <div><dt>Stage 1 artifact digest</dt><dd><code>{_escape(model.build['build_data_sha256'])}</code></dd></div>
      </dl>
    </section>"""


def _landing_page(model: SiteModel) -> bytes:
    page = "index.html"
    content = (
        _overview(model, page)
        + _custom_commands(model, page)
        + _test_listing(model, page)
        + _run_listing(model, page)
        + f"""
        <section id="incomplete-data-summary">
          <h2>Incomplete data</h2>
          <p>{_count_link(page, 'incomplete-data/index.html', 'incomplete-entries', len(model.incomplete), 'incomplete-data entries')}.
          Missing evidence is shown as unavailable, never as zero.</p>
        </section>"""
        + _provenance(model)
    )
    return _layout(
        page,
        "Hermit compatibility",
        "A navigable view of recorded facts in the deterministic Stage 1 fixture.",
        content,
        (("Overview", None),),
    )


def _cell_table_row(model: SiteModel, page: str, cell: Mapping[str, Any]) -> str:
    identity = str(cell["cell"])
    incomplete = cell["incomplete_fields"]
    incomplete_text = (
        ", ".join(_escape(field) for field in incomplete)
        if incomplete
        else "None recorded"
    )
    applicability = cell.get("not_applicable_reason")
    applicability_text = (
        "<strong>Not applicable</strong>"
        f'<span class="secondary">{_escape(applicability)}</span>'
        if applicability
        else "Applicable"
    )
    target = model.cell_paths[identity]
    return f"""
      <tr data-cell-row="1" data-state="{_cell_state(cell)}">
        <th scope="row"><a href="{_escape(_href(page, target))}"><code>{_escape(identity)}</code></a></th>
        <td>{_test_link(model, page, str(cell['test']))}</td><td>{_escape(cell['mode'])}</td>
        <td>{_escape(cell['backend'])}</td><td>{applicability_text}</td>
        <td>{_selected_by_full(cell)}</td><td>{_state_badge(cell)}</td><td>{_latest_result(cell)}</td>
        <td><a href="{_escape(_href(page, target, 'series-rows'))}" data-count="{cell['physical_row_count']}"
        data-represented-count="{cell['represented_run_count']}">{cell['physical_row_count']} rows / {cell['represented_run_count']} runs</a></td>
        <td>{incomplete_text}</td>
      </tr>"""


def _cell_table(model: SiteModel, page: str) -> str:
    bodies = []
    for state in STATE_ORDER:
        rows = [cell for cell in model.cells if _cell_state(cell) == state]
        bodies.append(
            f'<tbody id="cells-{state}" data-row-count="{len(rows)}">'
            f'<tr class="group-row"><th colspan="10" scope="rowgroup">'
            f"{STATE_LABELS[state]} — {len(rows)} / {len(model.cells)} cells in the manifest"
            f'</th></tr>{"".join(_cell_table_row(model, page, cell) for cell in rows)}</tbody>'
        )
    return f"""
      <div class="table-scroll"><table>
        <caption>Every cell in the fixture manifest, grouped by its derived state.</caption>
        <thead><tr><th scope="col">Cell</th><th scope="col">Test id</th>
        <th scope="col">Mode</th><th scope="col">Backend</th><th scope="col">Applicability</th>
        <th scope="col">Selected by full</th><th scope="col">State from qualifying canonical rows</th>
        <th scope="col">Latest product result</th><th scope="col">Canonical rows / represented runs</th>
        <th scope="col">Incomplete fields</th></tr></thead>{"".join(bodies)}
      </table></div>"""


def _selection_lists(model: SiteModel, page: str) -> str:
    selected = [cell for cell in model.cells if cell["selected_by_full"]]
    not_selected = [cell for cell in model.cells if not cell["selected_by_full"]]

    def items(values: Sequence[Mapping[str, Any]]) -> str:
        return "".join(
            f"<li>{_cell_link(model, page, str(cell['cell']))}</li>" for cell in values
        )

    return f"""
      <div class="selection-grid">
        <section id="selected-by-full" data-row-count="{len(selected)}">
          <h2>{len(selected)} / {len(model.cells)} cells selected by full</h2><ul>{items(selected)}</ul>
        </section>
        <section id="not-selected-by-full" data-row-count="{len(not_selected)}">
          <h2>{len(not_selected)} / {len(model.cells)} cells not selected by full</h2><ul>{items(not_selected)}</ul>
        </section>
      </div>"""


def _series_table_row(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    payload = row["series"]
    identity = str(payload["cell"])
    run_path, run_reason = _event_run_path(model, identity, str(row["event_id"]))
    run = (
        f'<a href="{_escape(_href(page, run_path))}">{_escape(run_reason)}</a>'
        if run_path is not None
        else f'<span class="unavailable">Unavailable</span><span class="secondary">{_escape(run_reason)}</span>'
    )
    event = (
        f'<a href="{_escape(_href(page, model.cell_paths[identity], _fragment("series", str(row["event_id"]))))}"><code>{_escape(row["event_id"])}</code></a>'
        if identity in model.cell_paths
        else f'<code>{_escape(row["event_id"])}</code>'
    )
    reason = row.get("projection_validation_reason")
    return f"""
      <tr data-series-row="1">
        <th scope="row">{_cell_link(model, page, identity)}</th>
        <td>{event}</td>
        <td>{_escape(row['producer'])}<span class="secondary"><code>{_escape(row['run_id'])}</code></span></td>
        <td>{run}</td><td>{_display(row.get('emitted_at'))}</td><td><code>{_escape(payload['tree'])}</code></td>
        <td>{_ancestry(row.get('current_main_ancestry'))}</td><td>{_series_position(payload)}</td>
        <td>{_display(payload.get('outcome'))}</td><td>{_display(payload.get('result'))}</td>
        <td>{_series_product_result(row)}</td><td>{_display(payload.get('failure_class'))}</td>
        <td>{_runtime(payload)}</td><td>1</td><td>{_escape(payload['num_runs'])}</td>
        <td>{_display(reason) if reason is not None else 'None recorded'}</td>
      </tr>"""


def _series_group(
    model: SiteModel,
    page: str,
    identity: str,
    label: str,
    rows: Sequence[Mapping[str, Any]],
) -> str:
    represented = sum(int(row["series"]["num_runs"]) for row in rows)
    return (
        f'<tbody id="{identity}" data-row-count="{len(rows)}" '
        f'data-represented-count="{represented}">'
        f'<tr class="group-row"><th colspan="16" scope="rowgroup">{_escape(label)} — '
        f"{len(rows)} canonical rows represent {represented} runs</th></tr>"
        f'{"".join(_series_table_row(model, page, row) for row in rows)}</tbody>'
    )


def _series_table(model: SiteModel, page: str) -> str:
    admitted = [
        row for row in model.series_rows if row["projection_validation_reason"] is None
    ]
    excluded = [
        row
        for row in model.series_rows
        if row["projection_validation_reason"] is not None
    ]
    represented = sum(int(row["series"]["num_runs"]) for row in model.series_rows)
    return f"""
    <section id="series-rows" data-row-count="{len(model.series_rows)}" data-represented-count="{represented}">
      <h2>Canonical series rows</h2>
      <p>Each table row is one physical canonical row. Represented runs comes from the row's
      recorded <code>num_runs</code>; neither number is inferred from the other.</p>
      <div class="table-scroll"><table>
        <caption>Public canonical series rows. Producer outcome, producer result, product result,
        and the validation-history join remain separate.</caption>
        <thead><tr><th scope="col">Cell</th><th scope="col">Event id</th>
        <th scope="col">Producer and run id</th><th scope="col">Validation history row</th>
        <th scope="col">Recorded at</th><th scope="col">Hermit tree</th>
        <th scope="col">Current main ancestry</th><th scope="col">Attempt / run index</th>
        <th scope="col">Producer outcome</th><th scope="col">Producer result</th>
        <th scope="col">Product result</th><th scope="col">Failure class</th><th scope="col">Runtime</th>
        <th scope="col">Physical rows</th><th scope="col">Represented runs</th>
        <th scope="col">Projection exclusion reason</th></tr></thead>
        {_series_group(model, page, 'series-admitted', 'No projection exclusion reason', admitted)}
        {_series_group(model, page, 'series-excluded', 'Recorded projection exclusion reason', excluded)}
      </table></div>
    </section>"""


def _cells_index(model: SiteModel) -> bytes:
    page = "cells/index.html"
    content = f"""
    <section id="all-cells" data-row-count="{len(model.cells)}">
      <h2>All cells in the manifest</h2>
      <p>Manifest membership, selection by a named path, derived state, and product result
      are distinct facts. A cell not selected by full is not a failed measurement.</p>
      {_cell_table(model, page)}
    </section>
    {_selection_lists(model, page)}
    {_series_table(model, page)}"""
    return _layout(
        page,
        "Cells and canonical rows",
        "Every cell in the manifest and every public canonical series row in the fixture.",
        content,
        (("Overview", "index.html"), ("Cells", None)),
    )


def _cell_selection_details(
    model: SiteModel, page: str, cell: Mapping[str, Any]
) -> str:
    items = []
    identity = str(cell["cell"])
    for path, facts in sorted(cell["selected_by"].items()):
        for fact in facts:
            run_path = _selection_run_path(model, identity, str(path), fact)
            run = (
                f'<a href="{_escape(_href(page, run_path))}">'
                f'<code>{_escape(fact["run_id"])}</code></a>'
                if run_path is not None
                else (
                    f'<code>{_escape(fact["run_id"])}</code>'
                    '<span class="unavailable">Matching validation history row unavailable</span>'
                )
            )
            events = ", ".join(_escape(value) for value in fact["event_ids"])
            events = events or "None recorded"
            items.append(
                f"<li><strong>Selected by {_escape(path)}</strong>{run}"
                f'<span class="secondary">Tree <code>{_escape(fact["tree"])}</code>; '
                f"observed at {_display(fact.get('observed_at'))}; event ids: {events}</span></li>"
            )
    if not items:
        return '<p class="unavailable">No retained validation-run selection</p>'
    return '<ul class="card-list">' + "".join(items) + "</ul>"


def _capabilities(payload: Mapping[str, Any]) -> str:
    values = payload.get("host_capabilities")
    if not isinstance(values, Mapping):
        return _display(None)
    if not values:
        return "None recorded"
    items = []
    for name, facts in sorted(values.items()):
        items.append(
            f"<li><strong>{_escape(name)}</strong>: present={_display(facts.get('present'))}; "
            f"evidence={_display(facts.get('evidence'))}</li>"
        )
    return "<ul>" + "".join(items) + "</ul>"


def _cell_series_row(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    payload = row["series"]
    run_path, reason = _event_run_path(
        model, str(payload["cell"]), str(row["event_id"])
    )
    run = (
        f'<a href="{_escape(_href(page, run_path))}">{_escape(reason)}</a>'
        if run_path is not None
        else f'<span class="unavailable">Unavailable</span><span class="secondary">{_escape(reason)}</span>'
    )
    projection_reason = row.get("projection_validation_reason")
    return f"""
      <article id="{_fragment('series', str(row['event_id']))}" class="evidence-card" data-series-row="1">
        <h3><code>{_escape(row['event_id'])}</code></h3>
        <dl class="facts">
          <div><dt>Producer</dt><dd>{_escape(row['producer'])}</dd></div>
          <div><dt>Run id</dt><dd><code>{_escape(row['run_id'])}</code></dd></div>
          <div><dt>Validation history row</dt><dd>{run}</dd></div>
          <div><dt>Recorded at</dt><dd>{_display(row.get('emitted_at'))}</dd></div>
          <div><dt>Hermit tree</dt><dd><code>{_escape(payload['tree'])}</code></dd></div>
          <div><dt>Current main ancestry</dt><dd>{_ancestry(row.get('current_main_ancestry'))}</dd></div>
          <div><dt>Attempt / run index</dt><dd>{_series_position(payload)}</dd></div>
          <div><dt>Producer outcome</dt><dd>{_display(payload.get('outcome'))}</dd></div>
          <div><dt>Producer result</dt><dd>{_display(payload.get('result'))}</dd></div>
          <div><dt>Product result</dt><dd>{_series_product_result(row)}</dd></div>
          <div><dt>Failure class</dt><dd>{_display(payload.get('failure_class'))}</dd></div>
          <div><dt>Runtime</dt><dd>{_runtime(payload)}</dd></div>
          <div><dt>Machine</dt><dd>{_display(payload.get('machine_shortname'))}</dd></div>
          <div><dt>Kernel</dt><dd>{_display(payload.get('kernel_version'))}</dd></div>
          <div><dt>Host capabilities</dt><dd>{_capabilities(payload)}</dd></div>
          <div><dt>Represented runs</dt><dd>{_escape(payload['num_runs'])}</dd></div>
          <div><dt>Projection exclusion reason</dt><dd>{_display(projection_reason) if projection_reason is not None else 'None recorded'}</dd></div>
        </dl>
      </article>"""


def _cell_incomplete(model: SiteModel, page: str, identity: str) -> str:
    rows = [
        (index, row)
        for index, row in enumerate(model.incomplete)
        if row.get("cell") == identity
    ]
    if not rows:
        return "<p>None recorded for this cell.</p>"
    return (
        "<ul>"
        + "".join(
            f'<li><a href="{_escape(_href(page, "incomplete-data/index.html", f"incomplete-{index}"))}">'
            f"{_escape(row['field'])}: {_escape(row['reason'])}</a></li>"
            for index, row in rows
        )
        + "</ul>"
    )


def _cell_page(model: SiteModel, identity: str) -> bytes:
    page = model.cell_paths[identity]
    cell = model.cell_by_id[identity]
    series_rows = _series_by_cell(model.cells, model.series_rows)[identity]
    represented = sum(int(row["series"]["num_runs"]) for row in series_rows)
    applicability = cell.get("not_applicable_reason")
    content = f"""
    <section id="cell-facts">
      <h2>Manifest policy and result</h2>
      <dl class="facts">
        <div><dt>Test id</dt><dd>{_test_link(model, page, str(cell['test']))}</dd></div>
        <div><dt>Mode</dt><dd>{_escape(cell['mode'])}</dd></div>
        <div><dt>Backend</dt><dd>{_escape(cell['backend'])}</dd></div>
        <div><dt>Lane</dt><dd>{_escape(cell['lane'])}</dd></div>
        <div><dt>Category</dt><dd>{_escape(cell['category'])}</dd></div>
        <div><dt>Applicability</dt><dd>{'Not applicable: ' + _escape(applicability) if applicability else 'Applicable'}</dd></div>
        <div><dt>Selected by full</dt><dd>{_selected_by_full(cell)}</dd></div>
        <div><dt>State from qualifying canonical rows</dt><dd>{_state_badge(cell)}</dd></div>
        <div><dt>Latest product result</dt><dd>{_latest_result(cell)}</dd></div>
        <div><dt>Latest tree</dt><dd><code>{_display(cell.get('latest_result_tree'))}</code></dd></div>
        <div><dt>Latest result time</dt><dd>{_display(cell.get('latest_result_time'))}</dd></div>
        <div><dt>Timeout</dt><dd>{_escape(cell['timeout_seconds'])} seconds</dd></div>
        <div><dt>Working directory</dt><dd>{_display(cell.get('workdir'))}</dd></div>
        <div><dt>Guest arguments</dt><dd>{_json_display(cell.get('guest_args'))}</dd></div>
      </dl>
    </section>
    <section id="current-reproducer"><h2>Current reproducer</h2>{_reproducer(cell)}</section>
    <section id="recorded-selection"><h2>Selection in retained validation runs</h2>
      <p>These are per-run facts. They do not establish a complete quick or super population.</p>
      {_cell_selection_details(model, page, cell)}
    </section>
    <section id="series-rows" data-row-count="{len(series_rows)}" data-represented-count="{represented}">
      <h2>Canonical series rows</h2><p>{len(series_rows)} physical rows represent {represented} runs.</p>
      {"".join(_cell_series_row(model, page, row) for row in series_rows) if series_rows else '<p>No qualifying canonical row exists.</p>'}
    </section>
    <section id="incomplete-data"><h2>Incomplete data for this cell</h2>{_cell_incomplete(model, page, identity)}</section>
    <section class="notice" aria-labelledby="cell-unavailable"><h2 id="cell-unavailable">Unavailable from Stage 1 artifacts</h2>
      <ul><li>Historical invocation</li><li>Raw retained logs</li><li>Syscall histogram</li></ul>
    </section>"""
    return _layout(
        page,
        identity,
        "Manifest membership, path-specific selection, state, product result, and retained evidence.",
        content,
        (("Overview", "index.html"), ("Cells", "cells/index.html"), (identity, None)),
    )


def _test_metadata(test: Mapping[str, Any]) -> str:
    requirements = ", ".join(_escape(item) for item in test["requires"])
    return f"""
    <dl class="facts">
      <div><dt>Description</dt><dd>{_escape(test['description'])}</dd></div>
      <div><dt>Category</dt><dd>{_escape(test['category'])}</dd></div>
      <div><dt>Lane</dt><dd>{_escape(test['lane'])}</dd></div>
      <div><dt>Program</dt><dd>{_display(test.get('program'))}</dd></div>
      <div><dt>Requirements</dt><dd>{requirements or 'None recorded'}</dd></div>
      <div><dt>Occasional</dt><dd>{_escape(str(test['occasional']).lower())}</dd></div>
      <div><dt>Build</dt><dd>{_json_display(test.get('build'))}</dd></div>
      <div><dt>Direct</dt><dd>{_json_display(test.get('direct'))}</dd></div>
      <div><dt>Preprocessors</dt><dd>{_json_display(test.get('preprocessors'))}</dd></div>
      <div><dt>Observation</dt><dd>{_json_display(test.get('observation'))}</dd></div>
    </dl>"""


def _test_cell_item(model: SiteModel, page: str, identity: str) -> str:
    cell = model.cell_by_id[identity]
    return f"""
    <li class="listing-card" data-test-cell-row="1">
      <h3>{_cell_link(model, page, identity)}</h3>
      <dl class="facts compact-facts">
        <div><dt>Selected by full</dt><dd>{_selected_by_full(cell)}</dd></div>
        <div><dt>State</dt><dd>{_state_badge(cell)}</dd></div>
        <div><dt>Latest product result</dt><dd>{_latest_result(cell)}</dd></div>
        <div><dt>Current reproducer</dt><dd>{_reproducer(cell)}</dd></div>
      </dl>
    </li>"""


def _test_page(model: SiteModel, identity: str) -> bytes:
    page = model.test_paths[identity]
    test = model.test_by_id[identity]
    cell_ids = tuple(str(value) for value in test["cells"])
    commands = [
        command for command in model.custom_commands if str(command["test"]) == identity
    ]
    custom_items = "".join(
        '<li class="listing-card" data-custom-row="1">'
        f'<h3><code>{_escape(command["cell"])}</code></h3>{_reproducer(command)}</li>'
        for command in commands
    )
    content = f"""
    <section id="manifest-metadata"><h2>Manifest metadata</h2>{_test_metadata(test)}</section>
    <section id="cells" data-row-count="{len(cell_ids)}"><h2>Cells in the manifest</h2>
      <ul class="card-list">{"".join(_test_cell_item(model, page, value) for value in cell_ids)}</ul>
    </section>
    <section id="custom-commands" data-row-count="{len(commands)}"><h2>Custom commands selected by full</h2>
      <ul class="card-list">{custom_items}</ul>{'' if commands else '<p>None recorded for this test.</p>'}
    </section>
    <section class="notice" aria-labelledby="test-unavailable"><h2 id="test-unavailable">Unavailable from Stage 1 artifacts</h2>
      <ul><li>Historical invocation</li><li>Raw retained logs</li><li>Syscall histogram</li></ul>
    </section>"""
    return _layout(
        page,
        identity,
        "Retained manifest metadata and the cells belonging to this test.",
        content,
        (("Overview", "index.html"), ("Tests", "index.html#tests"), (identity, None)),
    )


def _run_cell_link(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    return _cell_link(model, page, _cell_identity(row))


def _selected_groups(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    results = row.get("cell_results")
    if not isinstance(results, Mapping):
        return (
            '<section id="selected-cells"><h2>Cells selected by a validate path</h2>'
            '<p class="unavailable">Unavailable</p>'
            "<p>The retained row has no usable cell_results.</p></section>"
        )
    sections = []
    for path, selection in sorted(results["selected_by"].items()):
        cells = selection["cells"]
        items = "".join(
            f'<li data-selected-cell-row="1">{_run_cell_link(model, page, cell)}</li>'
            for cell in cells
        )
        sections.append(
            f'<section id="{_fragment("selected-by", str(path))}" '
            f'data-row-count="{len(cells)}"><h2>Cells selected by {_escape(path)}</h2>'
            f"<ul>{items}</ul></section>"
        )
    if not sections:
        return (
            '<section id="selected-cells"><h2>Cells selected by a validate path</h2>'
            "<p>None recorded.</p></section>"
        )
    return "".join(sections)


def _recorded_cells(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    results = row.get("cell_results")
    if not isinstance(results, Mapping):
        return f"""
        <section id="recorded-cells"><h2>Recorded cells</h2>
          <p class="unavailable">Unavailable</p><p>The retained row has no usable cell_results.</p>
        </section>"""
    rows = []
    for cell in results["recorded"]["cells"]:
        verdict = cell["cell_verdict"]
        compared = verdict.get("compared_log_messages")
        info = (
            f"left {_display(compared.get('left'))}; right {_display(compared.get('right'))}"
            if isinstance(compared, Mapping)
            else _display(None)
        )
        rows.append(
            f'<tr data-recorded-cell-row="1"><th scope="row">{_run_cell_link(model, page, cell)}</th>'
            f"<td>{_display(verdict.get('state'))}</td><td>{_display(verdict.get('bitwise_parity'))}</td>"
            f"<td>{info}</td><td>{_display(verdict.get('comparison_tier'))}</td></tr>"
        )
    artifact = results["recorded"]["artifact"]
    return f"""
    <section id="recorded-cells" data-row-count="{len(rows)}">
      <h2>Recorded cells</h2>
      <p>Artifact rows: {_escape(artifact['row_count'])}; artifact digest <code>{_escape(artifact['sha256'])}</code>.</p>
      <div class="table-scroll"><table>
        <caption>Recorded cells and their retained cell verdict fields.</caption>
        <thead><tr><th scope="col">Cell</th><th scope="col">Cell verdict state</th>
        <th scope="col">Bitwise parity</th><th scope="col">Compared INFO messages</th>
        <th scope="col">Comparison tier</th></tr></thead><tbody>{"".join(rows)}</tbody>
      </table></div>
    </section>"""


def _run_page(model: SiteModel, row: Mapping[str, Any]) -> bytes:
    identity = str(row["history_row_sha256"])
    page = model.run_paths[identity]
    results = row.get("cell_results")
    current = identity in model.current_history_ids
    if isinstance(results, Mapping):
        result_details = f"""
        <div><dt>Cell-results run id</dt><dd><code>{_escape(results['run_id'])}</code></dd></div>
        <div><dt>Cell-results Hermit SHA</dt><dd><code>{_escape(results['hermit_sha'])}</code></dd></div>
        <div><dt>Population digest</dt><dd><code>{_escape(results['population_sha256'])}</code></dd></div>
        <div><dt>Source tree dirty</dt><dd>{_escape(str(results['source_tree_dirty']).lower())}</dd></div>"""
    else:
        result_details = (
            '<div><dt>Cell results</dt><dd><span class="unavailable">Unavailable</span>'
            '<span class="secondary">This retained history row has no usable cell_results.</span></dd></div>'
        )
    content = f"""
    <section id="run-facts"><h2>Retained validation row</h2>
      <dl class="facts">
        <div><dt>Record</dt><dd>{'Current' if current else 'Historical'}</dd></div>
        <div><dt>Run id</dt><dd><code>{_display(row.get('run_id'))}</code></dd></div>
        <div><dt>Record id</dt><dd><code>{_display(row.get('record_id'))}</code></dd></div>
        <div><dt>Validate path</dt><dd>{_escape(row['profile'])}</dd></div>
        <div><dt>Selection mode</dt><dd>{_escape(row['selection_mode'])}</dd></div>
        <div><dt>Whole-run result</dt><dd>{_display(row.get('result'))}</dd></div>
        <div><dt>Hermit commit</dt><dd><code>{_escape(row['commit'])}</code></dd></div>
        <div><dt>Started</dt><dd>{_display(row.get('started_at'))}</dd></div>
        <div><dt>Finished</dt><dd>{_display(row.get('finished_at'))}</dd></div>
        <div><dt>Ledger schema</dt><dd>{_escape(row['schema_version'])}</dd></div>
        <div><dt>History row digest</dt><dd><code>{_escape(identity)}</code></dd></div>
        {result_details}
      </dl>
    </section>
    {_selected_groups(model, page, row)}
    {_recorded_cells(model, page, row)}
    <section class="notice" aria-labelledby="run-unavailable"><h2 id="run-unavailable">Unavailable from Stage 1 artifacts</h2>
      <ul><li>Historical invocation</li><li>Raw retained logs</li><li>Syscall histogram</li></ul>
    </section>"""
    title = str(row.get("run_id") or identity)
    return _layout(
        page,
        title,
        "One retained validation history row. Whole-run result and per-cell verdict remain separate.",
        content,
        (
            ("Overview", "index.html"),
            ("Validation runs", "index.html#validation-runs"),
            (title, None),
        ),
    )


def _affected_evidence(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    values = []
    cell = row.get("cell")
    if cell is not None:
        values.append(_cell_link(model, page, str(cell)))
    run_path = _incomplete_run_path(model, row)
    if run_path is not None:
        values.append(
            f'<a href="{_escape(_href(page, run_path))}">validation history row</a>'
        )
    elif row.get("run_id") is not None or row.get("record_id") is not None:
        values.append(
            '<span class="unavailable">Matching validation history row unavailable</span>'
        )
    for field in ("run_id", "record_id", "event_id", "path", "schema"):
        if row.get(field) is not None:
            values.append(f"{_escape(field)}={_escape(row[field])}")
    return "<br>".join(values) if values else "All affected data"


def _incomplete_page(model: SiteModel) -> bytes:
    page = "incomplete-data/index.html"
    rows = []
    for index, row in enumerate(model.incomplete):
        rows.append(
            f'<tr id="incomplete-{index}" data-incomplete-row="1">'
            f"<td>{_escape(row['input'])}</td><td>{_escape(row['field'])}</td>"
            f"<td>{_escape(row['reason'])}</td><td>{_affected_evidence(model, page, row)}</td></tr>"
        )
    content = f"""
    <section id="incomplete-entries" data-row-count="{len(rows)}">
      <h2>All incomplete-data entries</h2>
      <p>Missing evidence is shown as unavailable, never as zero or as a stored measured flag.</p>
      <div class="table-scroll"><table>
        <caption>Every Stage 1 fact this static site cannot render completely.</caption>
        <thead><tr><th scope="col">Input</th><th scope="col">Field</th>
        <th scope="col">Reason</th><th scope="col">Affected evidence</th></tr></thead>
        <tbody>{"".join(rows)}</tbody>
      </table></div>
    </section>"""
    return _layout(
        page,
        "Incomplete data",
        "Recorded gaps in the facts available to the compatibility site.",
        content,
        (("Overview", "index.html"), ("Incomplete data", None)),
    )


def _site_css() -> bytes:
    try:
        value = SITE_CSS_PATH.read_bytes()
    except OSError as error:
        raise RenderRefused(f"cannot read {SITE_CSS_PATH}: {error}") from error
    lowered = value.lower()
    if not value or any(
        token in lowered for token in (b"http:", b"https:", b"@import", b"url(")
    ):
        raise RenderRefused("site stylesheet is empty or contains an external resource")
    return value


def render_site(build: Mapping[str, Any], data: Mapping[str, Any]) -> dict[str, bytes]:
    model = _site_model(build, data)
    files = {
        "assets/site.css": _site_css(),
        "cells/index.html": _cells_index(model),
        "incomplete-data/index.html": _incomplete_page(model),
        "index.html": _landing_page(model),
    }
    for identity, path in model.cell_paths.items():
        files[path] = _cell_page(model, identity)
    for identity, path in model.test_paths.items():
        files[path] = _test_page(model, identity)
    for row in model.history_rows:
        identity = str(row["history_row_sha256"])
        files[model.run_paths[identity]] = _run_page(model, row)
    return dict(sorted(files.items()))


def render_page(build: Mapping[str, Any], data: Mapping[str, Any]) -> bytes:
    """Return the landing page for callers of the first-slice API."""
    return render_site(build, data)["index.html"]


def _validate_output_files(files: Mapping[str, bytes]) -> dict[str, frozenset[str]]:
    inventories: dict[str, set[str]] = {
        directory: set() for directory in DETAIL_DIRECTORIES
    }
    for path, value in files.items():
        parsed = PurePosixPath(path)
        if (
            parsed.is_absolute()
            or ".." in parsed.parts
            or len(parsed.parts) not in (1, 2)
        ):
            raise RenderRefused(f"generated page has an unsafe path {path!r}")
        if not value:
            raise RenderRefused(f"generated page is empty: {path}")
        if len(parsed.parts) == 1:
            if path != "index.html":
                raise RenderRefused(f"generated root file is unexpected: {path}")
            continue
        directory, name = parsed.parts
        if directory not in inventories:
            raise RenderRefused(f"generated page directory is unexpected: {path}")
        inventories[directory].add(name)
    expected = {"index.html"} | {
        f"{directory}/{name}"
        for directory, names in inventories.items()
        for name in names
    }
    if set(files) != expected:
        raise RenderRefused("generated page inventory is contradictory")
    return {
        "": frozenset({"index.html", *DETAIL_DIRECTORIES}),
        **{directory: frozenset(names) for directory, names in inventories.items()},
    }


def _at_or_below(path: Path, root: Path) -> bool:
    return path == root or root in path.parents


def _refuse_artifact_output_overlap(artifact_root: Path, output: Path) -> None:
    try:
        source = Path(os.path.abspath(artifact_root.expanduser())).resolve(strict=True)
        resolved_output = output.resolve(strict=False)
        resolved_parent = output.parent.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise RenderRefused(
            f"cannot resolve artifact and output paths: {error}"
        ) from error
    if (
        _at_or_below(resolved_output, source)
        or _at_or_below(resolved_parent, source)
        or _at_or_below(source, resolved_output)
    ):
        raise RenderRefused("output destination overlaps the Stage 1 artifact tree")


def write_page(artifact_root: Path, output_value: Path) -> Path:
    build, data = load_artifacts(artifact_root)
    files = render_site(build, data)
    inventories = _validate_output_files(files)
    writer = _writer()
    try:
        output = writer._destination(output_value)
        _refuse_artifact_output_overlap(artifact_root, output)
        parent = writer._open_parent(output.parent)
    except (OSError, RuntimeError, writer.WriteRefused) as error:
        raise RenderRefused(f"output destination refused: {error}") from error
    temporary = f".{output.name}.prototype-{os.getpid()}-{uuid.uuid4().hex}"
    root = None
    directories: dict[str, int] = {}
    published = False
    try:
        if writer._entry(parent, output.name) is not None:
            raise RenderRefused("output destination already exists")
        os.mkdir(temporary, 0o755, dir_fd=parent)
        root = writer._open_directory(parent, temporary)
        for directory in DETAIL_DIRECTORIES:
            os.mkdir(directory, 0o755, dir_fd=root)
            directories[directory] = writer._open_directory(root, directory)
        for path, value in files.items():
            parts = PurePosixPath(path).parts
            if len(parts) == 1:
                writer._write_file(root, parts[0], value)
            else:
                writer._write_file(directories[parts[0]], parts[1], value)
        for directory, descriptor in directories.items():
            writer._require_inventory(
                descriptor, inventories[directory], f"prototype {directory}"
            )
            writer._fsync_directory(descriptor)
        writer._require_inventory(root, inventories[""], "prototype root")
        writer._fsync_directory(root)
        writer._renameat2(parent, temporary, output.name, writer._RENAME_NOREPLACE)
        published = True
        writer._fsync_directory(parent)
    except (OSError, writer.WriteRefused) as error:
        retained = output if published else output.parent / temporary
        raise RenderRefused(
            "prototype publication refused; no existing output was removed; "
            f"generated files may remain at {retained}: {error}"
        ) from error
    finally:
        for descriptor in directories.values():
            os.close(descriptor)
        if root is not None:
            os.close(root)
        os.close(parent)
    return output / "index.html"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""This prototype always labels every page as fixture data. It reads the exact
five-file Stage 1 artifact tree, writes no JavaScript, performs no network
request, and refuses an existing output directory.

Example:
  python3 ci-hub/compatibility-website/render_page.py \\
    --artifacts ci-hub/tests/fixtures/compatibility-website-stage-1 \\
    --output /tmp/hermit-compatibility-prototype""",
    )
    parser.add_argument(
        "--artifacts",
        required=True,
        type=Path,
        help="exact five-file Stage 1 fixture artifact directory",
    )
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="new output directory; an existing path is refused",
    )
    arguments = parser.parse_args(argv)
    try:
        page = write_page(arguments.artifacts, arguments.output)
    except (RenderRefused, OSError, ValueError) as error:
        print(f"compatibility-website render page: REFUSED: {error}", file=sys.stderr)
        return 2
    print(page.resolve().as_uri())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
