#!/usr/bin/env python3
"""Render one browser-openable compatibility page from Stage 1 artifacts.

This is deliberately a fixture-backed prototype, not the complete website.  It
does not read a ledger or infer a result.  The Stage 1 artifacts have already
done that work; this renderer validates those artifacts through the same
build-data validator and presents their derived fields without JavaScript or a
server.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import html
import importlib.util
import json
import os
import sys
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
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


class RenderRefused(Exception):
    """The supplied Stage 1 artifacts or output destination are unsafe."""


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


def _anchor(kind: str, value: str) -> str:
    digest = hashlib.sha256(value.encode()).hexdigest()[:16]
    return f"{kind}-{digest}"


def _cell_state(cell: Mapping[str, Any]) -> str:
    value = cell.get("state")
    if value is None:
        return "unavailable"
    if value not in STATE_LABELS:
        raise RenderRefused(
            f"cell {cell.get('cell')!r} has unsupported state {value!r}"
        )
    return str(value)


def _cell_link(cell: Mapping[str, Any]) -> str:
    identity = str(cell["cell"])
    return (
        f'<a href="#{_anchor("cell", identity)}">'
        f"<code>{_escape(identity)}</code></a>"
    )


def _reason(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, Mapping):
        details = [
            str(value[key])
            for key in ("reason", "result", "evidence")
            if value.get(key)
        ]
        return "; ".join(details)
    return str(value)


def _selected_by_full(cell: Mapping[str, Any]) -> str:
    if cell["selected_by_full"]:
        return "<strong>Selected by full</strong>"
    reason = _reason(cell.get("not_selected_by_full_reason"))
    suffix = f'<span class="secondary">{_escape(reason)}</span>' if reason else ""
    return f"<strong>Not selected by full</strong>{suffix}"


def _recorded_path_selection(cell: Mapping[str, Any]) -> str:
    selected_by = cell["selected_by"]
    if not selected_by:
        return (
            '<span class="unavailable">' "No retained validation-run selection</span>"
        )
    items = []
    for path, facts in sorted(selected_by.items()):
        runs = ", ".join(_escape(row["run_id"]) for row in facts)
        items.append(
            f"<li><strong>Selected by {_escape(path)}</strong>"
            f'<span class="secondary">{runs}</span></li>'
        )
    return '<ul class="plain-list">' + "".join(items) + "</ul>"


def _state_badge(cell: Mapping[str, Any]) -> str:
    state = _cell_state(cell)
    return (
        f'<span class="state state-{state}">'
        f"<strong>{STATE_LABELS[state]}</strong>"
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


def _cell_row(cell: Mapping[str, Any]) -> str:
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
    return f"""
      <tr id="{_anchor('cell', identity)}" data-cell-row="1" data-state="{_cell_state(cell)}">
        <th scope="row"><a href="#{_anchor('test', str(cell['test']))}"><code>{_escape(cell['test'])}</code></a></th>
        <td>{_escape(cell['mode'])}</td>
        <td>{_escape(cell['backend'])}</td>
        <td>{applicability_text}</td>
        <td>{_selected_by_full(cell)}</td>
        <td>{_recorded_path_selection(cell)}</td>
        <td>{_state_badge(cell)}</td>
        <td>{_latest_result(cell)}</td>
        <td><code>{_display(cell.get('latest_result_tree'))}</code><span class="secondary">{_display(cell.get('latest_result_time'))}</span></td>
        <td><a href="#{_anchor('series-cell', identity)}" data-count="{_escape(cell['physical_row_count'])}">{_escape(cell['physical_row_count'])}</a></td>
        <td><a href="#{_anchor('series-cell', identity)}" data-represented-count="{_escape(cell['represented_run_count'])}">{_escape(cell['represented_run_count'])}</a></td>
        <td>{incomplete_text}</td>
      </tr>"""


def _cell_table(cells: Sequence[Mapping[str, Any]]) -> str:
    grouped = {
        state: [cell for cell in cells if _cell_state(cell) == state]
        for state in STATE_ORDER
    }
    bodies = []
    for state in STATE_ORDER:
        rows = grouped[state]
        bodies.append(
            f'<tbody id="cells-{state}" data-row-count="{len(rows)}">'
            f'<tr class="group-row"><th colspan="12" scope="rowgroup">'
            f"{STATE_LABELS[state]} — {len(rows)} / {len(cells)} cells in the manifest"
            f'</th></tr>{"".join(_cell_row(cell) for cell in rows)}</tbody>'
        )
    return f"""
      <div class="table-scroll">
        <table>
          <caption>Every cell in the fixture manifest, grouped by its derived state.</caption>
          <thead><tr>
            <th scope="col">Test id</th><th scope="col">Mode</th><th scope="col">Backend</th>
            <th scope="col">Applicability</th><th scope="col">Selected by full</th>
            <th scope="col">Selected by path in retained validation runs</th>
            <th scope="col">State from qualifying canonical rows</th><th scope="col">Latest product result</th>
            <th scope="col">Latest tree and time</th><th scope="col">Canonical rows</th>
            <th scope="col">Represented runs</th><th scope="col">Incomplete fields</th>
          </tr></thead>
          {"".join(bodies)}
        </table>
      </div>"""


def _state_summary(cells: Sequence[Mapping[str, Any]]) -> str:
    total = len(cells)
    cards = []
    for state in STATE_ORDER:
        count = sum(_cell_state(cell) == state for cell in cells)
        cards.append(
            f'<a class="metric state-{state}" href="#cells-{state}" '
            f'data-count="{count}"><strong>{count} / {total}</strong>'
            f"<span>{STATE_LABELS[state]}</span></a>"
        )
    return '<div class="metrics">' + "".join(cards) + "</div>"


def _selection_summary(
    cells: Sequence[Mapping[str, Any]], custom: Sequence[Mapping[str, Any]]
) -> str:
    selected = [cell for cell in cells if cell["selected_by_full"]]
    not_selected = [cell for cell in cells if not cell["selected_by_full"]]
    selected_rows = "".join(f"<li>{_cell_link(cell)}</li>" for cell in selected)
    not_selected_rows = "".join(f"<li>{_cell_link(cell)}</li>" for cell in not_selected)
    return f"""
      <div class="selection-grid">
        <section id="selected-by-full" data-row-count="{len(selected)}">
          <h3>{len(selected)} / {len(cells)} cells selected by full</h3>
          <ul>{selected_rows}</ul>
        </section>
        <section id="not-selected-by-full" data-row-count="{len(not_selected)}">
          <h3>{len(not_selected)} / {len(cells)} cells not selected by full</h3>
          <ul>{not_selected_rows}</ul>
        </section>
      </div>
      <p><a href="#custom-commands" data-count="{len(custom)}"><strong>{len(custom)}
      custom commands selected by full</strong></a>. They are not part of the
      {len(cells)}-cell manifest denominator.</p>
      {_custom_commands(custom)}
      <div class="notice">
        <p><strong>Cells selected by quick: Unavailable.</strong></p>
        <p><strong>Cells selected by super: Unavailable.</strong></p>
        <p>The Stage 1 manifest export records the complete population selected by full only.
        Retained validation runs below may show that an individual run selected a cell by quick,
        full, or super; that evidence is not a complete quick or super population.</p>
      </div>"""


def _custom_commands(commands: Sequence[Mapping[str, Any]]) -> str:
    items = []
    for command in commands:
        applicability = command.get("not_applicable_reason")
        applicability_text = (
            f'<span class="secondary">Not applicable: {_escape(applicability)}</span>'
            if applicability
            else '<span class="secondary">Applicable</span>'
        )
        items.append(
            f"<li><code>{_escape(command['cell'])}</code>{applicability_text}"
            '<span class="secondary">Current reproducer</span>'
            f"{_reproducer(command)}</li>"
        )
    return (
        f'<div id="custom-commands" data-row-count="{len(commands)}">'
        "<h3>Custom commands selected by full</h3>"
        f'<ul class="custom-commands">{"".join(items)}</ul></div>'
    )


def _test_sections(
    tests: Sequence[Mapping[str, Any]], cells: Sequence[Mapping[str, Any]]
) -> str:
    by_id = {str(cell["cell"]): cell for cell in cells}
    sections = []
    for test in tests:
        cell_items = []
        for identity in test["cells"]:
            cell = by_id[str(identity)]
            cell_items.append(
                f"<li>{_cell_link(cell)}"
                '<span class="secondary">Current reproducer</span>'
                f"{_reproducer(cell)}</li>"
            )
        requirements = ", ".join(_escape(item) for item in test["requires"])
        sections.append(
            f"""
        <article id="{_anchor('test', str(test['id']))}" class="test-card" data-test-row="1">
          <h3><code>{_escape(test['id'])}</code></h3>
          <p>{_escape(test['description'])}</p>
          <dl>
            <div><dt>Category</dt><dd>{_escape(test['category'])}</dd></div>
            <div><dt>Lane</dt><dd>{_escape(test['lane'])}</dd></div>
            <div><dt>Program</dt><dd>{_display(test.get('program'))}</dd></div>
            <div><dt>Requirements</dt><dd>{requirements or 'None recorded'}</dd></div>
          </dl>
          <ul class="test-cells">{"".join(cell_items)}</ul>
        </article>"""
        )
    return "".join(sections)


def _selection_count(row: Mapping[str, Any]) -> tuple[str, str | None]:
    path = str(row["profile"])
    results = row.get("cell_results")
    if not isinstance(results, Mapping):
        return path, None
    selected_by = results["selected_by"]
    selection = selected_by.get(path)
    if not isinstance(selection, Mapping):
        return path, None
    return path, str(selection["cell_count"])


def _run_rows(rows: Sequence[Mapping[str, Any]], *, current: bool) -> str:
    rendered = []
    for row in rows:
        label, selected_count = _selection_count(row)
        results = row.get("cell_results")
        recorded = (
            str(results["recorded"]["cell_count"])
            if isinstance(results, Mapping)
            else None
        )
        identity = str(row["history_row_sha256"])
        rendered.append(
            f"""
        <tr id="{_anchor('run', identity)}" data-run-row="1" data-run-current="{str(current).lower()}">
          <th scope="row"><code>{_display(row.get('run_id'))}</code></th>
          <td>{'Current' if current else 'Historical'}</td>
          <td>{_escape(label)}</td><td>{_display(selected_count)}</td><td>{_display(recorded)}</td>
          <td>{_display(row.get('result'))}</td><td><code>{_escape(row['commit'])}</code></td>
          <td>{_display(row.get('started_at'))}</td><td>{_display(row.get('finished_at'))}</td>
        </tr>"""
        )
    return "".join(rendered)


def _runs_table(runs: Mapping[str, Any]) -> str:
    current = runs["current_validation_run_rows"]
    current_ids = {row["history_row_sha256"] for row in current}
    historical = [
        row
        for row in runs["validation_history_rows"]
        if row["history_row_sha256"] not in current_ids
    ]
    return f"""
      <div class="table-scroll">
        <table>
          <caption>Retained validation rows. Whole-run result and the cells selected by each named path are separate.</caption>
          <thead><tr><th scope="col">Run id</th><th scope="col">Record</th>
          <th scope="col">Validate path</th><th scope="col">Cells selected by that path</th>
          <th scope="col">Cells recorded</th><th scope="col">Run result</th>
          <th scope="col">Hermit commit</th><th scope="col">Started</th><th scope="col">Finished</th></tr></thead>
          <tbody id="current-validation-runs" data-row-count="{len(current)}">
            {_run_rows(current, current=True)}
          </tbody>
          <tbody id="historical-validation-runs" data-row-count="{len(historical)}">
            {_run_rows(historical, current=False)}
          </tbody>
        </table>
      </div>"""


def _series_row(row: Mapping[str, Any], manifest_cells: frozenset[str]) -> str:
    payload = row["series"]
    identity = str(payload["cell"])
    reason = row.get("projection_validation_reason")
    result = payload.get("result")
    product = _series_product_result(row)
    cell = (
        f'<a href="#{_anchor("cell", identity)}"><code>{_escape(identity)}</code></a>'
        if identity in manifest_cells
        else (
            f"<code>{_escape(identity)}</code>"
            '<span class="secondary">Not in the current manifest</span>'
        )
    )
    return f"""
        <tr id="{_anchor('series', str(row['event_id']))}" data-series-row="1">
          <th scope="row">{cell}</th>
          <td><code>{_escape(row['event_id'])}</code></td>
          <td>{_escape(row['producer'])}<span class="secondary"><code>{_escape(row['run_id'])}</code></span></td>
          <td>{_display(row.get('emitted_at'))}</td>
          <td><code>{_escape(payload['tree'])}</code></td>
          <td>{_ancestry(row.get('current_main_ancestry'))}</td>
          <td>{_series_position(payload)}</td>
          <td>{_display(payload.get('outcome'))}</td>
          <td>{_display(result)}</td>
          <td>{product}</td>
          <td>{_runtime(payload)}</td>
          <td>1</td>
          <td>{_escape(payload['num_runs'])}</td>
          <td>{_display(reason) if reason is not None else 'None recorded'}</td>
        </tr>"""


def _series_cell_summaries(
    rows: Sequence[Mapping[str, Any]], cells: Sequence[Mapping[str, Any]]
) -> str:
    by_cell: dict[str, list[Mapping[str, Any]]] = {
        str(cell["cell"]): [] for cell in cells
    }
    for row in rows:
        identity = str(row["series"]["cell"])
        if identity in by_cell:
            by_cell[identity].append(row)
    sections = []
    for cell in cells:
        identity = str(cell["cell"])
        values = by_cell[identity]
        represented = sum(int(row["series"]["num_runs"]) for row in values)
        if (
            len(values) != cell["physical_row_count"]
            or represented != cell["represented_run_count"]
        ):
            raise RenderRefused(f"cell {identity!r} series counts disagree")
        items = "".join(
            f'<li><a href="#{_anchor("series", str(row["event_id"]))}"><code>{_escape(row["event_id"])}</code></a> '
            f"represents {_escape(row['series']['num_runs'])} run(s)</li>"
            for row in values
        )
        sections.append(
            f'<section id="{_anchor("series-cell", identity)}" data-row-count="{len(values)}" '
            f'data-represented-count="{represented}"><h3><code>{_escape(identity)}</code></h3>'
            f"<p>{len(values)} canonical rows represent {represented} runs.</p>"
            f"<ul>{items}</ul></section>"
        )
    return '<div class="series-cell-grid">' + "".join(sections) + "</div>"


def _series_table(
    rows: Sequence[Mapping[str, Any]], cells: Sequence[Mapping[str, Any]]
) -> str:
    admitted = [row for row in rows if row["projection_validation_reason"] is None]
    excluded = [row for row in rows if row["projection_validation_reason"] is not None]
    manifest_cells = frozenset(str(cell["cell"]) for cell in cells)

    def group(identity: str, label: str, values: Sequence[Mapping[str, Any]]) -> str:
        represented = sum(int(row["series"]["num_runs"]) for row in values)
        return (
            f'<tbody id="{identity}" data-row-count="{len(values)}" '
            f'data-represented-count="{represented}">'
            f'<tr class="group-row"><th colspan="14" scope="rowgroup">{label} — '
            f"{len(values)} canonical rows represent {represented} runs</th></tr>"
            f'{"".join(_series_row(row, manifest_cells) for row in values)}</tbody>'
        )

    return f"""
      <h3>Rows by cell in the manifest</h3>
      {_series_cell_summaries(rows, cells)}
      <h3>All public canonical series rows</h3>
      <div class="table-scroll">
        <table>
          <caption>Public canonical series rows, in artifact order within each section.
          Producer outcome, producer result, and product result remain separate.</caption>
          <thead><tr><th scope="col">Cell</th><th scope="col">Event id</th>
          <th scope="col">Producer and run id</th><th scope="col">Recorded at</th>
          <th scope="col">Hermit tree</th><th scope="col">Current main ancestry</th>
          <th scope="col">Attempt / run index</th><th scope="col">Producer outcome</th>
          <th scope="col">Producer result</th><th scope="col">Product result</th>
          <th scope="col">Runtime</th><th scope="col">Physical rows</th>
          <th scope="col">Represented runs</th><th scope="col">Projection exclusion reason</th></tr></thead>
          {group('series-admitted', 'No projection exclusion reason', admitted)}
          {group('series-excluded', 'Recorded projection exclusion reason', excluded)}
        </table>
      </div>"""


def _incomplete_table(
    rows: Sequence[Mapping[str, Any]], cells: Sequence[Mapping[str, Any]]
) -> str:
    cell_ids = {str(cell["cell"]) for cell in cells}
    rendered = []
    for row in rows:
        affected = []
        cell = row.get("cell")
        if cell in cell_ids:
            affected.append(
                f'<a href="#{_anchor("cell", str(cell))}">'
                f"<code>{_escape(cell)}</code></a>"
            )
        elif cell is not None:
            affected.append(f"<code>{_escape(cell)}</code>")
        for field in ("run_id", "record_id", "event_id", "path", "schema"):
            if row.get(field) is not None:
                affected.append(f"{_escape(field)}={_escape(row[field])}")
        rendered.append(
            '<tr data-incomplete-row="1">'
            f"<td>{_escape(row['input'])}</td><td>{_escape(row['field'])}</td>"
            f"<td>{_escape(row['reason'])}</td>"
            f"<td>{'<br>'.join(affected) if affected else 'All affected data'}</td>"
            "</tr>"
        )
    return f"""
      <div class="table-scroll">
        <table>
          <caption>Every Stage 1 fact that this prototype cannot render completely.</caption>
          <thead><tr><th scope="col">Input</th><th scope="col">Field</th>
          <th scope="col">Reason</th><th scope="col">Affected evidence</th></tr></thead>
          <tbody id="incomplete-entries" data-row-count="{len(rows)}">{"".join(rendered)}</tbody>
        </table>
      </div>"""


def render_page(build: Mapping[str, Any], data: Mapping[str, Any]) -> bytes:
    cells = list(data["cells"])
    tests = list(data["tests"])
    incomplete = list(data["incomplete_data"])
    counts = data["counts"]
    state_counts = {
        state: sum(_cell_state(cell) == state for cell in cells)
        for state in STATE_ORDER
    }
    expected = {
        "green": counts["green_cell_count"],
        "red": counts["red_cell_count"],
        "never-measured": counts["never_measured_cell_count"],
        "unavailable": counts["state_unavailable_cell_count"],
    }
    if state_counts != expected or len(cells) != counts["in_manifest_cell_count"]:
        raise RenderRefused(
            "Stage 1 cell counts disagree with the rendered populations"
        )
    if (
        len(tests) != counts["test_count"]
        or len(incomplete) != counts["incomplete_data_count"]
    ):
        raise RenderRefused("Stage 1 test or incomplete-data counts disagree")
    provenance = build["provenance"]
    runs = data["runs"]
    series_rows = list(runs["series_rows"])
    represented = sum(int(row["series"]["num_runs"]) for row in series_rows)
    if (
        len(series_rows) != runs["series_physical_row_count"]
        or represented != runs["series_represented_run_count"]
    ):
        raise RenderRefused("Stage 1 series counts disagree with the rendered rows")
    source = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Hermit compatibility fixture prototype</title>
  <link rel="stylesheet" href="assets/site.css">
</head>
<body>
  <header class="site-header">
    <p class="fixture-banner">Fixture data — not current Hermit main</p>
    <h1>Hermit compatibility</h1>
    <p>This browser-openable prototype renders only the recorded facts in a deterministic
    Stage 1 fixture. It does not read SCORECARD.md, infer a missing measurement, or report
    this fixture as current project status.</p>
    <nav aria-label="Page sections"><a href="#overview">Overview</a><a href="#all-cells">Cells</a>
    <a href="#tests">Tests</a><a href="#series-rows">Series rows</a>
    <a href="#runs">Validation runs</a><a href="#incomplete-data">Incomplete data</a></nav>
  </header>
  <main>
    <section id="overview">
      <h2>Overview</h2>
      <p class="population"><a href="#all-cells" data-count="{len(cells)}"><strong>{len(cells)}</strong>
      cells in the manifest</a></p>
      {_state_summary(cells)}
      <p class="evidence-counts"><a href="#series-rows" data-count="{runs['series_physical_row_count']}"
      data-represented-count="{runs['series_represented_run_count']}"><strong>{runs['series_physical_row_count']}</strong>
      public canonical series rows representing <strong>{runs['series_represented_run_count']}</strong> runs</a>.
      Of those, <a href="#series-admitted" data-count="{runs['projection_admitted_physical_row_count']}"
      data-represented-count="{runs['projection_admitted_represented_run_count']}"><strong>{runs['projection_admitted_physical_row_count']}</strong>
      rows representing <strong>{runs['projection_admitted_represented_run_count']}</strong> runs</a> have no
      projection exclusion reason, while <a href="#series-excluded" data-count="{runs['projection_excluded_physical_row_count']}"
      data-represented-count="{runs['projection_excluded_represented_run_count']}"><strong>{runs['projection_excluded_physical_row_count']}</strong>
      rows representing <strong>{runs['projection_excluded_represented_run_count']}</strong> runs</a> have one recorded.
      Physical rows and represented runs are different denominators.</p>
    </section>
    <section id="manifest-selection">
      <h2>Manifest and validation-path selection</h2>
      <p>Selection and measured result are shown separately. A cell not selected by full is not red
      unless qualifying canonical evidence independently makes it red.</p>
      {_selection_summary(cells, data['selected_by_full_custom_commands'])}
    </section>
    <section id="all-cells" data-row-count="{len(cells)}">
      <h2>All cells in the manifest</h2>
      {_cell_table(cells)}
    </section>
    <section id="tests" data-row-count="{len(tests)}">
      <h2>Tests</h2>
      {_test_sections(tests, cells)}
    </section>
    <section id="series-rows" data-row-count="{len(series_rows)}" data-represented-count="{represented}">
      <h2>Canonical series rows</h2>
      <p>Each table row is one physical canonical row. Represented runs comes from that row's
      recorded <code>num_runs</code>; neither number is inferred from the other.</p>
      {_series_table(series_rows, cells)}
    </section>
    <section id="runs">
      <h2>Validation runs</h2>
      <p><a href="#current-validation-runs" data-count="{counts['current_validation_run_row_count']}">
      {counts['current_validation_run_row_count']} current validation rows</a> and
      <a href="#historical-validation-runs" data-count="{counts['validation_history_row_count'] - counts['current_validation_run_row_count']}">
      {counts['validation_history_row_count'] - counts['current_validation_run_row_count']} historical validation rows</a>.</p>
      {_runs_table(runs)}
    </section>
    <section id="incomplete-data">
      <h2>Incomplete data</h2>
      <p><a href="#incomplete-entries" data-count="{len(incomplete)}"><strong>{len(incomplete)}</strong>
      incomplete-data entries</a>. Missing evidence is shown as unavailable, never as zero.</p>
      {_incomplete_table(incomplete, cells)}
    </section>
    <section id="provenance">
      <h2>Fixture provenance</h2>
      <dl class="provenance">
        <div><dt>Hermit commit used for manifest policy</dt><dd><code>{_escape(provenance['hermit_commit'])}</code></dd></div>
        <div><dt>Hermit main ancestry target</dt><dd><code>{_escape(provenance['hermit_main_commit'])}</code></dd></div>
        <div><dt>dev-hermit commit</dt><dd><code>{_escape(provenance['parent_commit'])}</code></dd></div>
        <div><dt>Manifest digest</dt><dd><code>{_escape(provenance['manifest_sha256'])}</code></dd></div>
        <div><dt>Captured series source digest</dt><dd><code>{_escape(provenance['series_rows_sha256'])}</code></dd></div>
        <div><dt>Public series projection digest</dt><dd><code>{_escape(provenance['series_rows_projection_sha256'])}</code></dd></div>
        <div><dt>Stage 1 artifact digest</dt><dd><code>{_escape(build['build_data_sha256'])}</code></dd></div>
      </dl>
    </section>
  </main>
  <footer><p>Generated locally from validated Stage 1 fixture artifacts. No network resources are used.</p></footer>
</body>
</html>
"""
    return source.encode()


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


def write_page(artifact_root: Path, output_value: Path) -> Path:
    build, data = load_artifacts(artifact_root)
    files = {
        "index.html": render_page(build, data),
        "assets/site.css": _site_css(),
    }
    writer = _writer()
    try:
        output = writer._destination(output_value)
        parent = writer._open_parent(output.parent)
    except (OSError, writer.WriteRefused) as error:
        raise RenderRefused(f"output destination refused: {error}") from error
    temporary = f".{output.name}.prototype-{os.getpid()}-{uuid.uuid4().hex}"
    root = assets = None
    published = False
    try:
        if writer._entry(parent, output.name) is not None:
            raise RenderRefused("output destination already exists")
        os.mkdir(temporary, 0o755, dir_fd=parent)
        root = writer._open_directory(parent, temporary)
        os.mkdir("assets", 0o755, dir_fd=root)
        assets = writer._open_directory(root, "assets")
        writer._write_file(root, "index.html", files["index.html"])
        writer._write_file(assets, "site.css", files["assets/site.css"])
        writer._require_inventory(assets, frozenset({"site.css"}), "prototype assets")
        writer._require_inventory(
            root, frozenset({"assets", "index.html"}), "prototype root"
        )
        writer._fsync_directory(assets)
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
        if assets is not None:
            os.close(assets)
        if root is not None:
            os.close(root)
        os.close(parent)
    return output / "index.html"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""This prototype always labels its input as fixture data. It reads the exact
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
