from __future__ import annotations

import copy
import gzip
import hashlib
import html
import importlib.util
import json
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import Counter
from dataclasses import replace
from html.parser import HTMLParser
from pathlib import Path, PurePosixPath
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlsplit

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/render_page.py"
WRITE_TEST = REPOSITORY / "ci-hub/tests/test_compatibility_website_write_data.py"
FIXTURE = REPOSITORY / "ci-hub/tests/fixtures/compatibility-website-stage-1"


def _load(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


render_page = _load("compatibility_website_render_page_test", SCRIPT)
writer_test = _load("compatibility_website_render_writer_fixture", WRITE_TEST)


def representative_build_data(
    _tmp_path: Path | None = None,
    _monkeypatch: pytest.MonkeyPatch | None = None,
) -> dict[str, Any]:
    """Return one complete schema-v3 fixture for renderer and combined tests."""
    return copy.deepcopy(writer_test.build_data_value())


def _validated(data: dict[str, Any]) -> dict[str, Any]:
    module = render_page._writer()._build_data_module()
    return module.validate_build_data(data)


def _tree(data: dict[str, Any]) -> dict[str, bytes]:
    return render_page._writer().render_tree(_validated(data))


def _build(data: dict[str, Any]) -> dict[str, Any]:
    return json.loads(_tree(data)["build.json"])


def fixture_files() -> dict[str, bytes]:
    return {
        path.relative_to(FIXTURE).as_posix(): path.read_bytes()
        for path in FIXTURE.rglob("*")
        if path.is_file()
    }


def _digest(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def _digest_for_identity(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


def _identity(test: str, mode: str = "verify") -> str:
    return f"{test}/{mode}/ptrace"


def _policy_cell(source: dict[str, Any], identity: str) -> dict[str, Any]:
    test, mode, backend = identity.rsplit("/", 2)
    return {
        **{
            key: copy.deepcopy(value)
            for key, value in source.items()
            if key
            in {
                "category",
                "current_reproducer",
                "current_reproducer_unavailable_reason",
                "cpu_timeout_seconds",
                "guest_args",
                "lane",
                "not_applicable_reason",
                "not_selected_by_full_reason",
                "selected_by_full",
                "timeout_seconds",
                "workdir",
            }
        },
        "backend": backend,
        "cell": identity,
        "mode": mode,
        "test": test,
    }


def _refresh_projection_digests(data: dict[str, Any]) -> None:
    runs = data["runs"]
    provenance = data["provenance"]
    module = render_page._writer()._build_data_module()
    for field, value in (
        ("current_main_ancestry_sha256", runs["current_main_ancestry"]),
        ("product_results_sha256", runs["product_results"]),
        ("projection_results_sha256", runs["projection_results"]),
        ("validate_run_selection_sha256", runs["validate_run_selection"]),
        ("series_rows_projection_sha256", runs["series_rows"]),
        (
            "current_validation_run_rows_projection_sha256",
            runs["current_validation_run_rows"],
        ),
        (
            "validation_history_rows_projection_sha256",
            runs["validation_history_rows"],
        ),
    ):
        provenance[field] = module._digest(value)


_ABSENT = object()


def _set_run_number(data: dict[str, Any], value: Any = _ABSENT) -> None:
    for field in ("validation_history_rows", "current_validation_run_rows"):
        for row in data["runs"][field]:
            if value is _ABSENT:
                row.pop("run_number", None)
            else:
                row["run_number"] = copy.deepcopy(value)
    _refresh_projection_digests(data)


def data_with_all_three_states_and_one_omitted_row() -> dict[str, Any]:
    data = representative_build_data()
    green = data["cells"][0]
    test = data["tests"][0]

    never_id = _identity(str(test["id"]), "run")
    never = _policy_cell(green, never_id)
    never.update(
        event_ids=[],
        physical_row_count=0,
        qualifying_event_ids=[],
        qualifying_physical_row_count=0,
        qualifying_represented_run_count=0,
        represented_run_count=0,
        selected_by={},
        state="never-measured",
    )
    never.pop("latest_result", None)
    never.pop("latest_event_ids", None)
    never.pop("latest_result_time", None)
    never.pop("latest_result_tree", None)
    data["cells"].append(never)

    omitted_id = _identity(str(test["id"]), "chaos")
    test["cells"].extend((never_id, omitted_id))
    row = copy.deepcopy(data["runs"]["series_rows"][0])
    row.update(
        event_id="event-without-result",
        producer="pressure-test",
        run_id="pressure-run",
        product_result=None,
    )
    row["series"].update(
        cell=omitted_id,
        outcome="skipped",
        result=None,
        failure_class="understood_prerequisite_failure",
    )
    data["runs"]["series_rows"].append(row)
    data["runs"]["product_results"][row["event_id"]] = None
    data["runs"]["projection_results"][row["event_id"]] = None
    data["projection_audit"]["deliberate_omissions"] = [
        {
            "backend": "ptrace",
            "cell": omitted_id,
            "event_ids": [row["event_id"]],
            "event_references": [
                {
                    "event_id": row["event_id"],
                    "observed_at": row["emitted_at"],
                    "product_result": None,
                    "run_id": row["run_id"],
                    "tree": row["series"]["tree"],
                }
            ],
            "mode": "chaos",
            "physical_row_count": 1,
            "qualifying_event_ids": [row["event_id"]],
            "qualifying_physical_row_count": 1,
            "qualifying_represented_run_count": 1,
            "reason": "the latest qualifying execution does not establish pass or product failure",
            "represented_run_count": 1,
            "test": test["id"],
        }
    ]
    for field in (
        "projection_admitted_physical_row_count",
        "projection_admitted_represented_run_count",
        "series_physical_row_count",
        "series_represented_run_count",
    ):
        data["runs"][field] += 1
    data["counts"].update(
        in_manifest_cell_count=3,
        never_measured_cell_count=1,
        physical_row_count=2,
        represented_run_count=2,
    )
    data["provenance"].update(
        series_physical_row_count=2,
        series_represented_run_count=2,
    )
    _refresh_projection_digests(data)
    return _validated(data)


def _replace_verdict_with_reason(data: dict[str, Any]) -> dict[str, Any]:
    verdict = {
        "comparison_tier": "execution-only-self-consistent",
        "reason": "fixture does not compare",
        "state": "performs-no-comparison-by-design",
    }
    for field in ("validation_history_rows", "current_validation_run_rows"):
        data["runs"][field][0]["cell_results"]["recorded"]["cells"][0][
            "cell_verdict"
        ] = copy.deepcopy(verdict)
    _refresh_projection_digests(data)
    return _validated(data)


def _add_runtime_counts(data: dict[str, Any]) -> dict[str, Any]:
    data["runs"]["series_rows"][0]["series"]["runtime"] = {
        "run1": {
            "scheduler_turns": 11,
            "syscalls": 12,
            "virtual_nanoseconds": 13,
        },
        "run2": {
            "scheduler_turns": 21,
            "syscalls": 22,
            "virtual_nanoseconds": 23,
        },
        "wall_time_max_ms": 2.0,
        "wall_time_min_ms": 1.0,
    }
    _refresh_projection_digests(data)
    return _validated(data)


class Links(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.ids: set[str] = set()
        self.links: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if values.get("id"):
            self.ids.add(str(values["id"]))
        if tag in {"a", "link", "script"}:
            target = values.get("href") or values.get("src")
            if target:
                self.links.append(str(target))


class ListingStructure(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.selects: dict[str, list[tuple[str, str]]] = {}
        self.inputs: set[str] = set()
        self.links: list[tuple[str, str]] = []
        self.captions: list[str] = []
        self.headings: list[str] = []
        self._select: str | None = None
        self._option: tuple[str, list[str]] | None = None
        self._link: tuple[str, list[str]] | None = None
        self._caption: list[str] | None = None
        self._heading: list[str] | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "select" and values.get("name"):
            self._select = str(values["name"])
            self.selects[self._select] = []
        elif tag == "option" and self._select is not None:
            self._option = (str(values.get("value") or ""), [])
        elif tag == "input" and values.get("name"):
            self.inputs.add(str(values["name"]))
        elif tag == "a" and values.get("href"):
            self._link = (str(values["href"]), [])
        elif tag == "caption":
            self._caption = []
        elif tag == "h1":
            self._heading = []

    def handle_data(self, data: str) -> None:
        if self._option is not None:
            self._option[1].append(data)
        if self._link is not None:
            self._link[1].append(data)
        if self._caption is not None:
            self._caption.append(data)
        if self._heading is not None:
            self._heading.append(data)

    def handle_endtag(self, tag: str) -> None:
        if tag == "option" and self._select is not None and self._option is not None:
            value, text = self._option
            self.selects[self._select].append((value, "".join(text).strip()))
            self._option = None
        elif tag == "select":
            self._select = None
        elif tag == "a" and self._link is not None:
            href, text = self._link
            self.links.append((href, "".join(text).strip()))
            self._link = None
        elif tag == "caption" and self._caption is not None:
            self.captions.append("".join(self._caption).strip())
            self._caption = None
        elif tag == "h1" and self._heading is not None:
            self.headings.append("".join(self._heading).strip())
            self._heading = None


class CollectionStructure(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.controls: dict[str, dict[str, str | None]] = {}
        self.items: Counter[str] = Counter()
        self.item_attributes: list[dict[str, str | None]] = []
        self.children: Counter[str] = Counter()
        self.scripts: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        collection = values.get("data-list-controls")
        if collection:
            self.controls[str(collection)] = values
        item = values.get("data-list-item")
        if item:
            self.items[str(item)] += 1
            self.item_attributes.append(values)
        child = values.get("data-list-child")
        if child:
            self.children[str(child)] += 1
        if tag == "script" and values.get("src"):
            self.scripts.append(str(values["src"]))


class ColumnHeadings(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.columns: list[dict[str, str]] = []
        self._column: dict[str, str] | None = None
        self._text: list[str] = []
        self._help_depth = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "th" and values.get("scope") == "col":
            self._column = {"help": "", "sort": "", "title": ""}
            self._text = []
        if self._column is None:
            return
        if values.get("title"):
            self._column["title"] = str(values["title"])
        if tag == "span" and "column-help-wrap" in str(values.get("class", "")):
            self._help_depth = 1
        elif self._help_depth:
            self._help_depth += 1
        if tag == "button" and "column-help" in str(values.get("class", "")):
            if values.get("aria-describedby"):
                self._column["help"] = str(values["aria-describedby"])
        sort = values.get("data-cell-sort") or values.get("data-list-sort")
        if sort:
            self._column["sort"] = str(sort)

    def handle_data(self, data: str) -> None:
        if self._column is not None and not self._help_depth:
            self._text.append(data)

    def handle_endtag(self, tag: str) -> None:
        if self._help_depth:
            self._help_depth -= 1
            return
        if tag == "th" and self._column is not None:
            self._column["label"] = "".join(self._text).strip()
            self.columns.append(self._column)
            self._column = None
            self._text = []


class MatrixStructure(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.matrix_sections = 0
        self.pagination = 0
        self.current_tables = 0
        self.outside_tables = 0
        self._section_depth = 0
        self._matrix_depth: int | None = None
        self._div_depth = 0
        self._current_depth: int | None = None
        self._outside_depth: int | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "section":
            self._section_depth += 1
            if values.get("id") == "test-matrix":
                self.matrix_sections += 1
                self._matrix_depth = self._section_depth

        in_matrix = self._matrix_depth is not None
        if in_matrix and tag == "nav" and "data-cell-pagination" in values:
            self.pagination += 1

        if tag == "div":
            self._div_depth += 1
            if in_matrix and values.get("id") == "all-cells":
                self._current_depth = self._div_depth
            elif in_matrix and values.get("id") == "recorded-not-in-current-manifest":
                self._outside_depth = self._div_depth
        elif tag == "table":
            if in_matrix and self._current_depth is not None:
                self.current_tables += 1
            if in_matrix and self._outside_depth is not None:
                self.outside_tables += 1

    def handle_endtag(self, tag: str) -> None:
        if tag == "div":
            if self._current_depth == self._div_depth:
                self._current_depth = None
            if self._outside_depth == self._div_depth:
                self._outside_depth = None
            self._div_depth -= 1
        elif tag == "section":
            if self._matrix_depth == self._section_depth:
                self._matrix_depth = None
            self._section_depth -= 1


def _collection_structure(source: bytes) -> CollectionStructure:
    parser = CollectionStructure()
    parser.feed(source.decode())
    return parser


def test_renderer_emits_v3_inventory_without_retired_route_or_placeholder() -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)

    assert "incomplete-data/index.html" not in files
    assert {
        "cells/projection-audit.html",
        "runs/index.html",
        "tests/custom-commands.html",
    } <= files.keys()
    assert set(path.split("/", 1)[0] for path in files if "/" in path) == {
        "assets",
        "cells",
        "data",
        "runs",
        "tests",
    }
    assert all(
        b"Unavailable" not in value
        for path, value in files.items()
        if path.endswith(".html")
    )
    assert "incomplete-data" not in SCRIPT.read_text()
    assert "validation_incomplete_data" not in SCRIPT.read_text()
    assert "missing_executions" not in SCRIPT.read_text()


def test_cell_populations_and_result_denominator_remain_separate() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    landing = files["index.html"].decode()
    cells = files["cells/index.html"].decode()
    audit = files["cells/projection-audit.html"].decode()

    assert "<strong>3</strong><span>cells in the manifest</span>" in landing
    assert "<strong>1</strong><span>result rows shown by default</span>" in landing
    assert (
        "<strong>1</strong><span>never measured cells in the explicit view</span>"
        in landing
    )
    assert (
        "<strong>1</strong><span>in the manifest minus rows with a cell page</span>"
        in landing
    )
    assert "<strong>1</strong><span>green plus red result denominator</span>" in landing
    assert 'data-state="green"' in cells
    assert 'data-state="never-measured"' not in cells
    never = files["cells/never-measured.html"].decode()
    assert 'data-state="never-measured"' in never
    assert "event-without-result" not in landing
    assert "event-without-result" in audit
    assert (
        "No pass or product failure result was recorded for that latest execution"
        in audit
    )
    summary = _browser_payload(files, "data/site-summary.json.gz")
    assert summary["projection_audit"] == data["projection_audit"]
    omission = data["projection_audit"]["deliberate_omissions"][0]
    assert omission["event_references"] == [
        {
            "event_id": "event-without-result",
            "observed_at": omission["event_references"][0]["observed_at"],
            "product_result": None,
            "run_id": "pressure-run",
            "tree": omission["event_references"][0]["tree"],
        }
    ]
    shard_records = [
        item
        for prefix in "0123456789abcdef"
        for item in _browser_payload(files, f"data/detail-{prefix}.json.gz")["records"]
    ]
    assert not any(
        item["kind"] == "cell" and item["identity"] == omission["cell"]
        for item in shard_records
    )
    assert "state-unavailable" not in landing


def test_series_counts_explicitly_name_hermit_runs() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    html_sources = "\n".join(
        value.decode() for path, value in files.items() if path.endswith(".html")
    )
    javascript = files["assets/site.js"].decode()

    assert "representing <strong>2</strong> Hermit runs" in html_sources
    assert "Represented Hermit runs" in html_sources
    assert "physical rows representing 2 Hermit runs" in html_sources
    assert ">Represented runs<" not in html_sources
    assert "physical rows representing 2 runs" not in html_sources
    assert "['Represented Hermit runs', cell.represented_run_count]" in javascript
    assert (
        "['Qualifying represented Hermit runs', "
        "cell.qualifying_represented_run_count]" in javascript
    )
    assert "['Represented runs', cell.represented_run_count]" not in javascript


def test_test_matrix_uses_compact_columns_links_and_measure_definitions() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    source = files["cells/index.html"].decode()
    headings = ColumnHeadings()
    headings.feed(source)
    matrix = headings.columns[:10]

    assert [column["label"] for column in matrix] == [
        "Category",
        "TestName",
        "Mode",
        "Backend",
        "Lane",
        "Latest result",
        "Latest result time",
        "Latest result tree",
        "Physical rows",
        "Represented Hermit runs",
    ]
    for path, encoded in files.items():
        if not path.endswith(".html"):
            continue
        page_headings = ColumnHeadings()
        page_headings.feed(encoded.decode())
        assert all(column["title"] for column in page_headings.columns), path
        assert all(
            column["title"] != column["label"] for column in page_headings.columns
        ), path
        assert all(
            column["sort"] or column["help"] for column in page_headings.columns
        ), path
        help_ids = [
            column["help"] for column in page_headings.columns if column["help"]
        ]
        assert len(help_ids) == len(set(help_ids)), path
        assert all(
            f'id="{help_id}" role="tooltip"' in encoded.decode() for help_id in help_ids
        ), path
    by_label = {column["label"]: column for column in matrix}
    assert "admitted stress-series/v3 records" in by_label["Physical rows"]["title"]
    assert "SeriesRow.num_runs" in by_label["Represented Hermit runs"]["title"]
    assert "not OS process invocations" in by_label["Represented Hermit runs"]["title"]
    assert (
        "chosen latest qualifying logical execution"
        in by_label["Latest result tree"]["title"]
    )
    assert "State" not in by_label and "Cell" not in by_label
    assert "Selected by full" not in by_label and "Selected by path" not in by_label
    assert "Latest product result" not in source
    assert "Test name" not in source
    assert "<h2>Test Matrix</h2>" in source
    structure = MatrixStructure()
    structure.feed(source)
    assert structure.matrix_sections == 1
    assert structure.pagination == 1
    assert structure.current_tables == 1
    assert structure.outside_tables == 1
    assert (
        'button type="submit"'
        not in source[
            source.index('<form class="cell-filters"') : source.index(
                "</form>", source.index('<form class="cell-filters"')
            )
        ]
    )

    cell = next(item for item in data["cells"] if item["latest_result"] == "pass")
    digest = _digest_for_identity(cell["cell"])
    assert f'href="detail.html?cell={digest}"' in source
    assert (
        f"><code>{render_page._test_name(cell['test'], cell['category'])}</code></a>"
        in source
    )
    assert (
        f'href="https://github.com/rrnewton/hermit/commit/{cell["latest_result_tree"]}"'
        in source
    )
    assert re.search(r"<time datetime=\"[^\"]+\">[^<]+ ET</time>", source)


def test_never_measured_is_only_zero_qualifying_rows() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    never = next(cell for cell in data["cells"] if cell["state"] == "never-measured")
    assert never["qualifying_physical_row_count"] == 0
    assert never["qualifying_represented_run_count"] == 0
    files = render_page.render_site(_build(data), data)
    digest = _digest_for_identity(never["cell"])
    shard = _browser_payload(files, f"data/detail-{digest[0]}.json.gz")
    assert not any(
        item["kind"] == "cell" and item["digest"] == digest for item in shard["records"]
    )
    summary = _browser_payload(files, "data/site-summary.json.gz")
    browser_cell = next(
        item for item in summary["cells"] if item["cell"] == never["cell"]
    )
    assert browser_cell["qualifying_physical_row_count"] == 0
    assert browser_cell["latest_result"] is None
    assert browser_cell["url"] is None
    assert "derivedCellState" in files["assets/site.js"].decode()


def test_never_measured_is_reachable_without_javascript_but_not_default() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    default = files["cells/index.html"].decode()
    explicit = files["cells/never-measured.html"].decode()
    never = next(cell for cell in data["cells"] if cell["state"] == "never-measured")
    model = render_page._site_model(_build(data), data, wording="fixture")

    assert never["cell"] not in default
    assert never["cell"] in explicit
    assert never["cell"] not in model.cell_paths
    assert f'data-cell="{never["cell"]}"' in explicit
    assert f"cell={_digest_for_identity(never['cell'])}" not in explicit
    assert "site.js" in explicit
    assert "tests/detail.html" in files
    test_digest = _digest_for_identity(data["tests"][0]["id"])
    shard = _browser_payload(files, f"data/detail-{test_digest[0]}.json.gz")
    test_record = next(
        item
        for item in shard["records"]
        if item["kind"] == "test" and item["digest"] == test_digest
    )
    assert _digest_for_identity(never["cell"]) not in test_record["cell_digests"]


def test_result_and_never_measured_pages_have_one_latest_result_control() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    result_page = ListingStructure()
    result_page.feed(files["cells/index.html"].decode())
    never_page = ListingStructure()
    never_page.feed(files["cells/never-measured.html"].decode())

    assert "state" not in result_page.selects
    assert "state" in result_page.inputs
    assert result_page.selects["result"] == [
        ("", "All"),
        ("pass", "Pass"),
        ("product_failure", "Fail"),
    ]
    assert ("never-measured.html#all-cells", "Never measured") in result_page.links
    assert result_page.captions == [
        "Green and red result rows in the fixture.",
        "Recorded evidence whose cell identity is outside the current manifest.",
    ]

    assert never_page.headings == ["Never measured"]
    assert "state" not in never_page.selects
    assert "state" in never_page.inputs
    assert (
        "<strong>State:</strong> Never measured"
        in files["cells/never-measured.html"].decode()
    )
    assert ("index.html#all-cells", "Results") in never_page.links
    assert never_page.captions == [
        "Cells in the manifest with no qualifying admitted row.",
        "Recorded evidence whose cell identity is outside the current manifest.",
    ]
    assert "q" in never_page.inputs


def test_result_and_never_measured_population_split_retains_626_and_5103(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cells = tuple({"state": "green"} for _ in range(596))
    cells += tuple({"state": "red"} for _ in range(30))
    cells += tuple({"state": "never-measured"} for _ in range(5103))
    model = SimpleNamespace(cells=cells, wording=SimpleNamespace(cells_lead=""))
    captured: dict[str, tuple[dict[str, str], ...]] = {}

    def capture_listing(
        _model: Any, *, title: str, cells: Any, **_arguments: Any
    ) -> bytes:
        captured[title] = tuple(cells)
        return b""

    monkeypatch.setattr(render_page, "_cells_listing_page", capture_listing)

    render_page._cells_index(model)
    render_page._never_measured_index(model)

    assert len(captured["Cells"]) == 626
    assert sum(cell["state"] == "green" for cell in captured["Cells"]) == 596
    assert sum(cell["state"] == "red" for cell in captured["Cells"]) == 30
    assert len(captured["Never measured"]) == 5103
    assert all(cell["state"] == "never-measured" for cell in captured["Never measured"])


def test_every_generated_data_collection_has_file_safe_controls() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)

    for path, source in files.items():
        if not path.endswith(".html"):
            continue
        structure = _collection_structure(source)
        assert len(structure.scripts) == 1
        assert structure.scripts[0].endswith("assets/site.js")

    landing = _collection_structure(files["index.html"])
    runs = _collection_structure(files["runs/index.html"])
    audit = _collection_structure(files["cells/projection-audit.html"])
    commands = _collection_structure(files["tests/custom-commands.html"])
    assert not landing.controls
    assert set(runs.controls) == {"validation-runs"}
    if data["runs"]["projection_exclusion_counts"]:
        assert set(audit.controls) == {"projection-audit"}
    else:
        assert not audit.controls
    if data["selected_by_full_custom_commands"]:
        assert set(commands.controls) == {"custom-commands"}
    else:
        assert not commands.controls
        assert 'id="custom-commands"' in files["tests/custom-commands.html"].decode()
    assert runs.controls["validation-runs"]["data-page-size"] == "50"
    assert not landing.items
    assert runs.items["1"] == len(data["runs"]["validation_history_rows"])
    assert commands.items["1"] == len(data["selected_by_full_custom_commands"])

    default_cells = files["cells/index.html"].decode()
    never_cells = files["cells/never-measured.html"].decode()
    for source in (default_cells, never_cells):
        assert 'name="page" type="hidden" value="1"' in source
        assert "data-cell-pagination" in source
        assert 'data-page-size="50"' in source

    assert 'data-detail-kind="cell"' in files["cells/detail.html"].decode()
    assert 'data-detail-kind="test"' in files["tests/detail.html"].decode()

    run = _collection_structure(files[model.history_paths[0]])
    selected = [name for name in run.controls if name.startswith("run-selected-")]
    assert selected
    for name in selected:
        assert run.controls[name]["data-page-size"] == "8"
        assert run.controls[name]["data-child-limit"] == "25"
    assert "recorded-cells" in run.controls
    assert run.controls["recorded-cells"]["data-page-size"] == "50"

    javascript = files["assets/site.js"].decode()
    assert javascript.count("fetch(") == 1
    assert not re.search(
        r"\b(?:XMLHttpRequest|WebSocket|EventSource|sendBeacon|Worker)\b",
        javascript,
    )


def test_generic_collection_metadata_is_canonical_and_content_remains_embedded() -> (
    None
):
    data = representative_build_data()
    build = _build(data)
    files = render_page.render_site(build, data)
    landing_source = files["index.html"].decode()
    landing = _collection_structure(files["index.html"])
    runs_source = files["runs/index.html"].decode()
    runs = _collection_structure(files["runs/index.html"])

    assert not landing.items
    assert runs.items["1"] == len(data["runs"]["validation_history_rows"])
    assert 'data-list-item="1"' not in landing_source
    assert "data-filter-values" not in landing_source
    assert "data-sort-values" not in landing_source
    assert 'data-query-prefix="runs_"' not in landing_source
    assert 'data-query-prefix="runs_"' in runs_source
    assert "data-list-previous" not in landing_source
    assert "data-list-next" not in landing_source
    assert "data-list-clear" not in landing_source


def test_landing_keeps_only_tests_and_moved_pages_preserve_content_and_navigation() -> (
    None
):
    data = data_with_all_three_states_and_one_omitted_row()
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    landing = files["index.html"].decode()
    runs = files["runs/index.html"].decode()
    audit = files["cells/projection-audit.html"].decode()
    commands = files["tests/custom-commands.html"].decode()

    assert not _collection_structure(files["index.html"]).controls
    assert 'id="overview"' in landing
    assert 'id="tests"' in landing
    for section in (
        "validation-runs",
        "series-excluded",
        "deliberate-omissions",
        "custom-commands",
    ):
        assert f'id="{section}"' not in landing

    assert (
        render_page._run_listing(model, "runs/index.html", show_heading=False)
        in runs
    )
    assert (
        render_page._exclusion_audit(
            model, "cells/projection-audit.html", show_heading=False
        )
        in audit
    )
    assert (
        render_page._deliberate_omission_audit(
            model, "cells/projection-audit.html"
        )
        in audit
    )
    assert 'id="custom-commands"' in commands
    assert "No custom command was selected by full." in commands

    assert 'href="runs/index.html#validation-runs"' in landing
    assert 'href="cells/projection-audit.html#series-excluded"' in landing
    assert 'href="cells/projection-audit.html#deliberate-omissions"' in landing
    assert 'href="tests/custom-commands.html#custom-commands"' in landing
    assert 'href="../index.html">Overview</a>' in runs
    assert 'href="../index.html">Overview</a>' in audit
    assert 'href="index.html">Cells</a>' in audit
    assert 'href="../index.html#tests">Tests</a>' in commands

    run_page = files[model.history_paths[0]].decode()
    assert '<a href="index.html">Validation runs</a>' in run_page
    test_detail = files["tests/detail.html"].decode()
    assert '<a href="../index.html#tests">Tests</a>' in test_detail

    for path, title in (
        ("runs/index.html", "Validation runs"),
        ("cells/projection-audit.html", "Projection audit"),
        ("tests/custom-commands.html", "Custom commands selected by full"),
    ):
        source = files[path].decode()
        assert source.count(f"<h1>{title}</h1>") == 1
        assert f"<h2>{title}</h2>" not in source
    assert "<h2>Tests</h2>" in landing

    command = {
        "cell": "applications/custom/verify/ptrace",
        "current_reproducer": None,
        "not_applicable_reason": None,
        "test": "applications/custom",
    }
    command_model = replace(model, custom_commands=(command,))
    custom_listing = render_page._custom_commands(
        command_model, "tests/custom-commands.html", show_heading=False
    )
    assert custom_listing in render_page._custom_commands_page(command_model).decode()
    assert "<h2>Custom commands selected by full</h2>" in render_page._custom_commands(
        command_model, "index.html"
    )


def test_non_table_collections_render_compact_order_buttons_and_hidden_state() -> None:
    data = representative_build_data()
    model = render_page._site_model(_build(data), data, wording="fixture")
    row = model.history_rows[0]
    command = {
        "cell": "applications/custom/verify/ptrace",
        "current_reproducer": None,
        "not_applicable_reason": None,
        "test": "applications/custom",
    }
    tests_source = render_page._test_listing(model, "index.html")
    assert 'class="list-controls"' not in tests_source
    assert 'class="listing-card"' not in tests_source
    assert "cells in the manifest" not in tests_source
    assert "result rows shown" not in tests_source
    sources = (
        (
            render_page._custom_commands(
                replace(model, custom_commands=(command,)), "index.html"
            ),
            (
                ("manifest", "Manifest order"),
                ("test", "TestName"),
                ("cell", "Cell"),
            ),
        ),
        (
            render_page._selected_groups(model, model.history_paths[0], row),
            (("hierarchy", "Mode/backend/category"), ("test", "TestName")),
        ),
    )

    for source, expected_sorts in sources:
        form = re.search(r'<form class="list-controls".*?</form>', source, re.DOTALL)
        assert form is not None
        controls = form.group(0)
        assert re.search(r'<select name="sort"[^>]* hidden>', controls)
        assert re.search(r'<select name="direction"[^>]* hidden>', controls)
        assert '<label>Sort ' not in controls
        assert '<label>Direction ' not in controls
        assert 'type="submit"' not in controls
        assert controls.count("data-list-clear") == 1
        assert ">Clear filters</button>" in controls
        assert controls.count('class="list-order-controls"') == 1
        assert controls.count('aria-pressed="true"') == 1
        assert re.findall(
            r'data-list-sort="([^"]+)"[^>]*><span>([^<]+)</span>', controls
        ) == list(expected_sorts)
    assert "TestName" in sources[0][0]
    assert "TestName" in sources[1][0]
    assert "Test name" not in tests_source + "".join(source for source, _sorts in sources)


def test_validation_run_record_control_uses_exact_canonical_row_values() -> None:
    row = representative_build_data()["runs"]["validation_history_rows"][0]
    row["run_number"] = 10
    historical = copy.deepcopy(row)
    historical["run_id"] = "validate-run-historical"
    historical["run_number"] = 2
    duplicate_number = copy.deepcopy(historical)
    duplicate_number["run_id"] = "validate-run-other-occurrence"
    invalid_number = copy.deepcopy(historical)
    invalid_number["run_id"] = "validate-run-without-number"
    invalid_number["run_number"] = True
    model = SimpleNamespace(
        current_history_indices=(0,),
        history_paths=(
            "runs/current.html",
            "runs/historical.html",
            "runs/other-occurrence.html",
            "runs/without-number.html",
        ),
        history_rows=(row, historical, duplicate_number, invalid_number),
    )
    source = render_page._run_listing(model, "runs/index.html")
    listing = ListingStructure()
    listing.feed(source)
    structure = _collection_structure(source.encode())
    run_rows = [
        attributes
        for attributes in structure.item_attributes
        if attributes.get("data-run-row") == "1"
    ]

    assert listing.selects["record"] == [
        ("", "All"),
        ("current", "Current ledger record"),
        ("historical", "Superseded ledger record"),
    ]
    assert 'data-list-filter="record" data-list-source="value:record"' in source
    for value, _label in listing.selects["record"][1:]:
        matching = [
            attributes
            for attributes in run_rows
            if attributes["data-list-value-record"] == value
        ]
        expected = 1 if value == "current" else 3
        assert len(matching) == expected
        assert all(
            item["data-run-current"] == str(value == "current").lower()
            for item in matching
        )
    assert ("current.html", "Run 10") in listing.links
    assert ("historical.html", "Run 2") in listing.links
    assert ("other-occurrence.html", "Run 2") in listing.links
    assert (
        "without-number.html",
        "Run id validate-run-without-number",
    ) in listing.links
    run_links = [item for item in listing.links if item[0].endswith(".html")]
    assert len(run_links) == 4
    assert sum(label == "Run 2" for _target, label in run_links) == 2
    assert len({target for target, _label in run_links}) == 4
    assert ("run", "Run number") in listing.selects["sort"]
    assert [item["data-list-value-run"] for item in run_rows] == ["10", "2", "2", ""]
    assert [item["data-list-value-run-id"] for item in run_rows] == [
        "validate-run-1",
        "validate-run-historical",
        "validate-run-other-occurrence",
        "validate-run-without-number",
    ]


def test_positive_run_number_is_the_primary_label_without_becoming_identity() -> None:
    data = representative_build_data()
    _set_run_number(data, render_page.MAX_U64)
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    run_path = model.history_paths[0]
    runs = ListingStructure()
    runs.feed(files["runs/index.html"].decode())

    assert (
        PurePosixPath(run_path).name,
        f"Run {render_page.MAX_U64}",
    ) in runs.links
    run_page = files[run_path].decode()
    assert f"<h1>Run {render_page.MAX_U64}</h1>" in run_page
    assert f"<dt>Run number</dt><dd>{render_page.MAX_U64}</dd>" in run_page
    assert "<dt>Run id</dt><dd><code>validate-run-1</code></dd>" in run_page

    summary = _browser_payload(files, "data/site-summary.json.gz")
    assert summary["runs"][0]["run_number"] == str(render_page.MAX_U64)
    assert summary["runs"][0]["run_id"] == "validate-run-1"
    cell_record = next(
        record
        for prefix in "0123456789abcdef"
        for record in _browser_payload(files, f"data/detail-{prefix}.json.gz")[
            "records"
        ]
        if record["kind"] == "cell"
    )
    run_references = [
        fact["run"] for facts in cell_record["selection"].values() for fact in facts
    ] + [item["run"] for item in cell_record["evidence"] if "run" in item]
    assert run_references
    assert all(
        reference["run_number"] == str(render_page.MAX_U64)
        and reference["run_id"] == "validate-run-1"
        and reference["path"] == run_path
        for reference in run_references
    )

    assert not any(
        target in {"data/cells.json.gz", "data/runs.json.gz", "data/tests.json.gz"}
        for target, _label in runs.links
    )


@pytest.mark.parametrize(
    "value",
    [_ABSENT, None, True, "17", 17.0, 0, -1, render_page.MAX_U64 + 1],
    ids=("absent", "null", "bool", "string", "float", "zero", "negative", "over-u64"),
)
def test_invalid_or_missing_run_number_falls_back_without_refusal(value: Any) -> None:
    data = representative_build_data()
    _set_run_number(data, value)
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    run_path = model.history_paths[0]

    assert "Run id validate-run-1" in files["runs/index.html"].decode()
    assert "<h1>Run id validate-run-1</h1>" in files[run_path].decode()
    assert "<dt>Run number</dt>" not in files[run_path].decode()
    assert (
        "run_number"
        not in _browser_payload(files, "data/site-summary.json.gz")["runs"][0]
    )


def test_generic_collection_items_use_only_compact_scalar_metadata() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)

    for path, source in files.items():
        if not path.endswith(".html"):
            continue
        structure = _collection_structure(source)
        for attributes in structure.item_attributes:
            assert attributes["data-list-item"] == "1"
            assert all(
                name == "data-list-item"
                or name.startswith("data-list-value-")
                or name
                in {
                    "class",
                    "data-recorded-cell-row",
                    "data-run-current",
                    "data-run-row",
                }
                for name in attributes
            )
            assert all(
                "{" not in str(value) and "[" not in str(value)
                for name, value in attributes.items()
                if name.startswith("data-list-")
            )

    javascript = files["assets/site.js"].decode()
    assert "element.textContent" in javascript
    assert "Object.values(values)" in javascript


def test_dense_group_controls_preserve_all_links_and_exact_caps() -> None:
    cells = [
        {
            "test": f"backend-parity-c/test-{index:03d}",
            "category": "backend-parity-c",
            "mode": "verify",
            "backend": "ptrace",
        }
        for index in range(60)
    ]
    model = _grouped_link_model(cells)
    source = render_page._grouped_cell_links(
        model,
        "runs/example.html",
        cells,
        collection_id="run-selected-example",
    )
    root = ET.fromstring(f"<root>{source}</root>")

    links = root.findall("section/div/a")
    assert len(links) == len(cells)
    assert len({link.attrib["href"] for link in links}) == len(cells)
    groups = root.findall("section/div")
    assert len(groups) == 1
    assert groups[0].attrib["data-list-group"] == "g0"


@pytest.mark.parametrize(
    "reason",
    [
        {"reason": "full does not select <this> & cell"},
        {
            "evidence": 'recorded <evidence> & "details"',
            "reason": "full does not select <this> & cell",
            "result": "timeout",
        },
    ],
)
def test_not_selected_by_full_reason_renders_structured_escaped_fields(
    reason: dict[str, str],
) -> None:
    data = representative_build_data()
    data["cells"][0]["selected_by_full"] = False
    data["cells"][0]["not_selected_by_full_reason"] = reason
    data = _validated(data)
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    row = copy.deepcopy(model.history_rows[0])
    row["profile"] = "full"
    row["cell_results"]["selected_by"] = {"full": {"cell_count": 0, "cells": []}}
    page = render_page._run_page(model, 0, row).decode()

    assert "{'reason':" not in page
    assert "Recorded but not selected by full" in page
    assert "<summary>Recorded reason</summary>" in page
    assert "<dt>Reason</dt><dd>full does not select &lt;this&gt; &amp; cell</dd>" in page
    if "evidence" in reason:
        assert (
            '<dt>Evidence</dt><dd>recorded &lt;evidence&gt; &amp; '
            '&quot;details&quot;</dd>'
        ) in page
        assert "<dt>Result</dt><dd>timeout</dd>" in page
    else:
        assert "<dt>Evidence</dt>" not in page
        assert "<dt>Result</dt>" not in page
    matrix = files["cells/index.html"].decode()
    assert "Selected by full" not in matrix
    assert "Not selected by full" not in matrix
    digest = _digest_for_identity(data["cells"][0]["cell"])
    shard = _browser_payload(files, f"data/detail-{digest[0]}.json.gz")
    record = next(
        item
        for item in shard["records"]
        if item["kind"] == "cell" and item["digest"] == digest
    )
    assert record["data"]["not_selected_by_full_reason"] == reason


def test_renderer_refuses_malformed_selection_reason_at_stage1_boundary() -> None:
    original = representative_build_data()
    changed = copy.deepcopy(original)
    changed["cells"][0]["selected_by_full"] = False
    changed["cells"][0]["not_selected_by_full_reason"] = {
        "reason": "why",
        "unexpected": "field",
    }

    with pytest.raises(
        render_page.RenderRefused,
        match=r"Stage 1 data is invalid: .*unknown field",
    ):
        render_page.render_site(_build(original), changed)


def test_selected_by_path_is_distinct_from_in_manifest_population() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    page = render_page.render_site(_build(data), data)["cells/index.html"].decode()

    assert '<option value="in_manifest">In the manifest</option>' in page
    assert "Selected by path" in page
    assert '<option value="super">super</option>' in page
    assert "enabled" not in page.lower()


def test_noncomparison_verdict_keeps_reason_and_omits_comparison_details() -> None:
    data = _replace_verdict_with_reason(representative_build_data())
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    page = render_page.render_site(build, data)[model.history_paths[0]].decode()

    assert "fixture does not compare" in page
    assert "performs-no-comparison-by-design" in page
    assert "Bitwise parity:" not in page
    assert "Compared INFO messages:" not in page


def test_run_page_places_test_result_beside_commit_verdict() -> None:
    data = representative_build_data()
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    page = render_page.render_site(build, data)[model.history_paths[0]].decode()

    assert re.search(
        r'data-list-sort="test_result"[^>]*>.*?TEST-RESULT.*?'
        r'data-list-sort="verdict"[^>]*>.*?COMMIT-VERDICT',
        page,
        flags=re.DOTALL,
    )
    assert "Selected by super" in page
    assert "Recorded results for cells selected by super" in page
    assert "Recorded but not selected by super" not in page
    assert (
        "Current ledger record means this is the terminal row after applying the "
        "ledger correction chain for this run id." in page
    )


def test_superseded_run_page_explains_correction_position_not_code_age() -> None:
    data = representative_build_data()
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    model = replace(model, current_history_indices=())
    page = render_page._run_page(model, 0, model.history_rows[0]).decode()

    assert "Superseded ledger record" in page
    assert "a later correction replaced this retained row" in page
    assert "does not describe code age" in page


def test_run_listing_links_direct_correction_partners() -> None:
    old = copy.deepcopy(
        representative_build_data()["runs"]["validation_history_rows"][0]
    )
    old.update(record_id="record-old", run_id="run-old")
    current = copy.deepcopy(old)
    current.update(
        record_id="record-current", run_id="run-current", corrects="record-old"
    )
    model = SimpleNamespace(
        current_history_indices=(1,),
        history_paths=("runs/old.html", "runs/current.html"),
        history_rows=(old, current),
    )

    source = render_page._run_listing(model, "runs/index.html")

    assert (
        'Superseded ledger record<span class="secondary correction-partners">'
        'Corrected by <a href="current.html">Run id run-current</a>' in source
    )
    assert (
        'Current ledger record<span class="secondary correction-partners">'
        'Corrects <a href="old.html">Run id run-old</a>' in source
    )


def _run_page_with_recorded_cells(count: int) -> tuple[render_page.SiteModel, bytes]:
    data = representative_build_data()
    model = render_page._site_model(_build(data), data, wording="fixture")
    row = copy.deepcopy(model.history_rows[0])
    source = row["cell_results"]["recorded"]["cells"][0]
    selected_source = row["cell_results"]["selected_by"][row["profile"]]["cells"][0]
    recorded = []
    selected = []
    for index in range(count):
        cell = copy.deepcopy(source)
        cell["backend"] = f"ptrace-{index:03d}"
        recorded.append(cell)
        selected_cell = copy.deepcopy(selected_source)
        selected_cell["backend"] = cell["backend"]
        selected.append(selected_cell)
    row["cell_results"]["recorded"].update(
        cell_count=count,
        cells=recorded,
    )
    row["cell_results"]["selected_by"][row["profile"]].update(
        cell_count=count,
        cells=selected,
    )
    row["cell_results"]["recorded"]["artifact"]["row_count"] = count
    model = replace(
        model,
        current_history_indices=(0,),
        history_paths=("runs/recorded.html",),
        history_rows=(row,),
    )
    return model, render_page._run_page(model, 0, row)


def test_run_page_embeds_all_records_but_only_renders_the_first_slice() -> None:
    _model, page = _run_page_with_recorded_cells(125)
    source = page.decode()
    match = re.search(
        r'<template data-list-records="recorded-cells"><span '
        r"data-list-records-json>(.*?)</span></template>",
        source,
        flags=re.DOTALL,
    )

    assert match is not None
    records = json.loads(html.unescape(match.group(1)))
    assert len(records) == 125
    assert source.count('data-recorded-cell-row="1"') == render_page.LIST_PAGE_SIZE
    assert source.count('data-list-target="recorded-cells"') == 1


def test_run_page_pagination_replaces_the_active_dom_slice_without_fetch(
    chromium_browser: Any,
) -> None:
    _model, run_page = _run_page_with_recorded_cells(125)
    files = render_page.render_site(
        _build(representative_build_data()), representative_build_data()
    )
    files["runs/recorded.html"] = run_page
    page = chromium_browser.new_page()
    page.add_init_script(
        "window.__fetchCalls = 0; const originalFetch = window.fetch; "
        "window.fetch = (...args) => { window.__fetchCalls += 1; "
        "return originalFetch(...args); };"
    )
    _browser_route(page, files)
    page.goto("http://127.0.0.1/runs/recorded.html")
    controls = page.locator('[data-list-controls="recorded-cells"]')
    rows = page.locator('[data-list-target="recorded-cells"] > tr')

    assert rows.count() == 50
    assert page.evaluate("window.__fetchCalls") == 0
    page.locator('[data-list-pagination="recorded-cells"] [data-list-next]').click()
    assert rows.count() == 50
    assert "recorded_page=2" in page.url
    page.locator('[data-list-pagination="recorded-cells"] [data-list-next]').click()
    assert rows.count() == 25
    assert "recorded_page=3" in page.url

    controls.locator('[name="q"]').fill("ptrace-117")
    assert rows.count() == 1
    assert "ptrace-117" in rows.inner_text()
    assert page.evaluate("window.__fetchCalls") == 0
    page.close()


def _grouped_link_model(
    cells: list[dict[str, str]],
) -> render_page.SiteModel:
    data = representative_build_data()
    model = render_page._site_model(_build(data), data, wording="fixture")
    identities = [render_page._cell_identity(cell) for cell in cells]
    return replace(
        model,
        cell_by_id=dict(zip(identities, cells, strict=True)),
        cell_paths={
            identity: f"cells/{index:064x}.html"
            for index, identity in enumerate(identities, start=1)
        },
    )


def test_grouped_cell_links_render_exact_hierarchy_order_and_escaped_labels() -> None:
    cells = [
        {
            "test": "applications/zeta & last",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
        {
            "test": "applications/kvm-first",
            "category": "applications",
            "mode": "verify",
            "backend": "kvm",
        },
        {
            "test": "language-runtimes/python<3",
            "category": "language & runtimes",
            "mode": "verify",
            "backend": "ptrace",
        },
        {
            "test": "applications/alpha",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
        {
            "test": "applications/run-first",
            "category": "applications",
            "mode": "run",
            "backend": "ptrace",
        },
    ]
    model = _grouped_link_model(cells)

    source = render_page._grouped_cell_links(model, "runs/example.html", cells)
    root = ET.fromstring(f"<root>{source}</root>")

    sections = root.findall("section")
    assert [section.attrib["class"] for section in sections] == [
        "selected-cells-by-mode-backend",
        "selected-cells-by-mode-backend",
        "selected-cells-by-mode-backend",
    ]
    assert all(section.attrib["data-list-parent"] == "1" for section in sections)
    assert [section.findtext("h3") for section in sections] == [
        "Run/ptrace",
        "Verify/kvm",
        "Verify/ptrace",
    ]
    assert [
        [strong.text for strong in section.findall("div/strong")]
        for section in sections
    ] == [
        ["applications:"],
        ["applications:"],
        ["applications:", "language & runtimes:"],
    ]
    links = root.findall("section/div/a")
    assert ["".join(link.itertext()) for link in links] == [
        "run-first",
        "kvm-first",
        "alpha",
        "zeta & last",
        "python<3",
    ]
    expected_hrefs = [
        render_page._href("runs/example.html", model.cell_paths[identity])
        for identity in (
            "applications/run-first/run/ptrace",
            "applications/kvm-first/verify/kvm",
            "applications/alpha/verify/ptrace",
            "applications/zeta & last/verify/ptrace",
            "language-runtimes/python<3/verify/ptrace",
        )
    ]
    assert [link.attrib["href"] for link in links] == expected_hrefs
    assert len(links) == len(cells)
    assert len({link.attrib["href"] for link in links}) == len(cells)
    assert "zeta &amp; last" in source
    assert "language &amp; runtimes:" in source
    assert "python&lt;3" in source
    visible_link_text = "".join("".join(link.itertext()) for link in links)
    assert "/verify/" not in visible_link_text


def test_grouped_cell_links_refuse_ambiguous_bare_test_labels() -> None:
    cells = [
        {
            "test": "one/same-name",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
        {
            "test": "two/same-name",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
    ]
    model = _grouped_link_model(cells)

    with pytest.raises(render_page.RenderRefused, match="bare test label"):
        render_page._grouped_cell_links(model, "runs/example.html", cells)


def test_selected_groups_use_the_same_grouping_for_each_validate_path() -> None:
    cells = [
        {
            "test": "applications/alpha",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
        {
            "test": "applications/beta",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
    ]
    model = _grouped_link_model(cells)
    missing = {
        "test": "applications/not-on-site",
        "category": "applications",
        "mode": "verify",
        "backend": "ptrace",
    }
    missing_identity = render_page._cell_identity(missing)
    row = {
        "cell_results": {
            "selected_by": {
                "full": {"cell_count": 3, "cells": [cells[1], missing, cells[0]]},
                "cell-requalification": {"cell_count": 1, "cells": [cells[0]]},
            }
        }
    }

    source = render_page._selected_groups(model, "runs/example.html", row)
    parser = ListingStructure()
    parser.feed(source)

    assert source.index("Selected by cell-requalification") < source.index(
        "Selected by full"
    )
    assert (
        "<strong>1</strong> cells were selected by cell-requalification. Of the "
        "selected cells in the manifest, 1 have published cell pages." in source
    )
    assert (
        "<strong>3</strong> cells were selected by full. Of the selected cells "
        "in the manifest, 2 have published cell pages." in source
    )
    assert (
        "Selected cells with no qualifying admitted row have no cell detail page "
        "and are listed as plain text." in source
    )
    assert [text for _href, text in parser.links] == ["alpha", "alpha", "beta"]
    assert (
        '<span class="selected-cell-without-detail">not-on-site</span>' in source
    )
    assert missing_identity not in model.cell_paths


def test_grouped_cell_links_reduce_repeated_identity_bytes() -> None:
    cells = [
        {
            "test": f"backend-parity-c/test-{index:03d}",
            "category": "backend-parity-c",
            "mode": "verify",
            "backend": "ptrace",
        }
        for index in range(100)
    ]
    model = _grouped_link_model(cells)

    grouped = render_page._grouped_cell_links(model, "runs/example.html", cells)
    flat = (
        "<ul>"
        + "".join(
            f"<li>{render_page._cell_link(model, 'runs/example.html', render_page._cell_identity(cell))}</li>"
            for cell in cells
        )
        + "</ul>"
    )

    assert grouped.count("<a ") == flat.count("<a ") == len(cells)
    assert len(grouped.encode()) * 4 < len(flat.encode()) * 3


def test_optional_manifest_details_are_omitted() -> None:
    data = representative_build_data()
    data["tests"][0]["build"] = None
    data["tests"][0]["direct"] = None
    data["cells"][0]["current_reproducer"] = None
    data["cells"][0]["current_reproducer_unavailable_reason"] = "not recorded"
    data = _validated(data)
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")

    records = [
        item
        for prefix in "0123456789abcdef"
        for item in _browser_payload(files, f"data/detail-{prefix}.json.gz")["records"]
    ]
    test_record = next(item for item in records if item["kind"] == "test")
    cell_record = next(item for item in records if item["kind"] == "cell")
    assert test_record["data"]["build"] is None
    assert test_record["data"]["direct"] is None
    assert (
        cell_record["data"]["current_reproducer_unavailable_reason"] == "not recorded"
    )


def test_projection_audit_preserves_disjoint_totals_and_overlapping_reasons() -> None:
    data = representative_build_data()
    data["runs"].update(
        projection_excluded_physical_row_count=1,
        projection_excluded_represented_run_count=2,
        projection_exclusion_counts=[
            {
                "physical_row_count": 1,
                "reason": "projection rejected the row",
                "represented_run_count": 2,
                "schema": "stress-series/v1",
            },
            {
                "physical_row_count": 1,
                "reason": "schema is not stress-series/v3",
                "represented_run_count": 2,
                "schema": "stress-series/v1",
            },
        ],
    )
    data = _validated(data)
    page = render_page.render_site(_build(data), data)[
        "cells/projection-audit.html"
    ].decode()

    assert "1 physical rows were admitted and 1 were excluded" in page
    assert "reason counts are not added" in page
    assert page.count("<td>stress-series/v1</td>") == 2


def test_schema_v1_data_refuses_closed() -> None:
    data = representative_build_data()
    data["schema_version"] = 1
    with pytest.raises(render_page.RenderRefused, match="schema_version"):
        render_page.render_site(_build(representative_build_data()), data)


def test_build_manifest_must_match_validated_data() -> None:
    data = representative_build_data()
    build = _build(data)
    build["counts"]["green_cell_count"] += 1
    with pytest.raises(render_page.RenderRefused, match="counts disagree"):
        render_page.render_site(build, data)


def test_render_is_deterministic_and_every_internal_link_resolves() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    build = _build(data)
    first = render_page.render_site(build, data)
    assert first == render_page.render_site(build, data)
    complete_tree = {**_tree(data), **first}
    parsers: dict[str, Links] = {}
    for path, value in first.items():
        if path.endswith(".html"):
            parser = Links()
            parser.feed(value.decode())
            parsers[path] = parser
    for source, parser in parsers.items():
        for link in parser.links:
            split = urlsplit(link)
            if split.scheme or split.netloc:
                assert re.fullmatch(
                    r"https://github\.com/rrnewton/(?:hermit|dev-hermit)/commit/[0-9a-f]{40}",
                    link,
                )
                continue
            target = PurePosixPath(source).parent.joinpath(
                split.path or PurePosixPath(source).name
            )
            normalized = PurePosixPath(os.path.normpath(target.as_posix())).as_posix()
            assert normalized in complete_tree
            if split.fragment and normalized in parsers:
                assert split.fragment in parsers[normalized].ids


def test_assets_are_local_and_only_commit_navigation_is_external() -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    inventories = render_page._validate_output_files(files)

    assert inventories["assets"] == frozenset({"favicon.svg", "site.css", "site.js"})
    assert "incomplete-data" not in inventories[""]
    for path, value in files.items():
        if path.endswith(".html"):
            source = value.decode()
            parser = Links()
            parser.feed(source)
            external = [link for link in parser.links if urlsplit(link).scheme]
            assert all(
                re.fullmatch(
                    r"https://github\.com/rrnewton/(?:hermit|dev-hermit)/commit/[0-9a-f]{40}",
                    link,
                )
                for link in external
            )
            external_markup = re.findall(
                r'<a class="external-link"[^>]+href="https://github\.com/rrnewton/'
                r'(?:hermit|dev-hermit)/commit/[0-9a-f]{40}"[^>]*>.*?</a>',
                source,
                flags=re.DOTALL,
            )
            assert len(external_markup) == len(external)
            assert all('(external link)"' in link for link in external_markup)
            assert all('class="external-link-indicator"' in link for link in external_markup)
            assert not re.search(
                r'href="[^"]*data/(?:cells|runs|tests)\.json\.gz', source
            )
            assert "Raw Stage 1" not in source
    javascript = files["assets/site.js"]
    assert javascript.count(b"https://") == 1
    assert b"https://github.com/rrnewton/hermit/commit/" in javascript
    assert b"external-link-indicator" in javascript
    stage_one = _tree(data)
    assert {
        "data/cells.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    } <= set(stage_one)


def test_artifact_source_recomposes_only_four_file_v2_shape() -> None:
    data = representative_build_data()
    files = _tree(data)
    build = json.loads(files["build.json"])
    payloads = {
        path: json.loads(gzip.decompress(value))
        for path, value in files.items()
        if path.endswith(".gz")
    }
    source = render_page._artifact_source(build, build["generator"], payloads)

    assert set(payloads) == {
        "data/cells.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    }
    assert source == _validated(data)


def test_checked_fixture_exactly_matches_the_v2_generator() -> None:
    data = representative_build_data()

    assert fixture_files() == _tree(data)
    build, loaded = render_page.load_artifacts(FIXTURE)
    assert loaded == _validated(data)
    assert build == json.loads(_tree(data)["build.json"])


def test_load_artifacts_reads_and_binds_every_v2_file(tmp_path: Path) -> None:
    data = representative_build_data()
    files = _tree(data)
    root = tmp_path / "stage1"
    for path, value in files.items():
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(value)

    build, loaded = render_page.load_artifacts(root)
    assert loaded == _validated(data)
    assert build == json.loads(files["build.json"])


def test_load_artifacts_refuses_extra_retired_artifact(tmp_path: Path) -> None:
    data = representative_build_data()
    files = _tree(data)
    root = tmp_path / "stage1"
    for path, value in files.items():
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(value)
    (root / "data/incomplete-data.json.gz").write_bytes(b"retired")

    with pytest.raises(render_page.RenderRefused, match="inventory"):
        render_page.load_artifacts(root)


def test_output_paths_refuse_traversal() -> None:
    with pytest.raises(render_page.RenderRefused, match="unsafe path"):
        render_page._validate_output_files({"../index.html": b"x"})


def test_write_page_publishes_once_and_refuses_existing_output(tmp_path: Path) -> None:
    data = representative_build_data()
    artifact = tmp_path / "stage1"
    for path, value in _tree(data).items():
        target = artifact / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(value)
    output = tmp_path / "site"

    page = render_page.write_page(artifact, output)
    assert page == output / "index.html"
    assert page.is_file()
    with pytest.raises(render_page.RenderRefused, match="already exists"):
        render_page.write_page(artifact, output)


def test_cli_help_names_four_file_fixture_boundary() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--help"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0 and result.stderr == ""
    assert "four-file Stage 1" in result.stdout
    assert "five-file Stage 1" not in result.stdout


def test_source_contains_no_retired_placeholder_literal() -> None:
    source = SCRIPT.read_text()
    assert "Unavail" + "able" not in source
    assert "state_" + "unavailable" not in source
    assert "incomplete" + "-data" not in source


def test_renderer_emits_compact_query_routes_and_fixed_data_shards() -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")

    assert {"cells/detail.html", "tests/detail.html"} <= files.keys()
    identity_pages = {
        path
        for path in files
        if re.fullmatch(r"(?:cells|tests)/[0-9a-f]{64}\.html", path)
    }
    assert identity_pages == set()
    for identity, url in model.cell_paths.items():
        assert url == f"cells/detail.html?cell={_digest_for_identity(identity)}"
    for identity, url in model.test_paths.items():
        assert url == f"tests/detail.html?test={_digest_for_identity(identity)}"
    assert {path for path in files if path.startswith("data/")} == {
        "data/site-manifest.json.gz",
        "data/site-summary.json.gz",
        *(f"data/detail-{prefix}.json.gz" for prefix in "0123456789abcdef"),
    }


def _browser_payload(files: dict[str, bytes], path: str) -> Any:
    return json.loads(gzip.decompress(files[path]))


def _replace_browser_payload(files: dict[str, bytes], path: str, payload: Any) -> None:
    decoded = render_page._browser_json_bytes(payload)
    files[path] = render_page._writer()._gzip_bytes(decoded)
    manifest = _browser_payload(files, "data/site-manifest.json.gz")
    artifact = next(item for item in manifest["artifacts"] if item["path"] == path)
    artifact["decoded_bytes"] = len(decoded)
    artifact["decoded_sha256"] = hashlib.sha256(decoded).hexdigest()
    files["data/site-manifest.json.gz"] = render_page._writer()._gzip_bytes(
        render_page._browser_json_bytes(manifest)
    )


def _browser_route(
    page: Any,
    files: dict[str, bytes],
    *,
    automatically_decode_gzip: bool = False,
    requests: list[str] | None = None,
) -> None:
    def serve(route: Any) -> None:
        path = urlsplit(route.request.url).path.lstrip("/") or "index.html"
        if requests is not None:
            requests.append(path)
        value = files.get(path)
        if value is None:
            route.fulfill(status=404, body=b"not found")
            return
        if automatically_decode_gzip and path.endswith(".json.gz"):
            value = gzip.decompress(value)
        content_type = (
            "text/html; charset=utf-8"
            if path.endswith(".html")
            else (
                "text/javascript; charset=utf-8"
                if path.endswith(".js")
                else (
                    "text/css; charset=utf-8"
                    if path.endswith(".css")
                    else (
                        "application/json"
                        if path.endswith(".json.gz")
                        else "image/svg+xml"
                    )
                )
            )
        )
        route.fulfill(status=200, body=value, headers={"content-type": content_type})

    page.route("**/*", serve)


def _wait_for_verified_data(page: Any) -> None:
    page.wait_for_function(
        "document.querySelector('[data-load-status]').dataset.loadState === 'verified'"
    )
    status = page.locator("[data-load-status]")
    assert status.get_attribute("aria-busy") == "false"
    assert status.is_hidden()
    assert status.inner_text() == ""


@pytest.fixture(scope="module")
def chromium_browser() -> Any:
    try:
        from playwright.sync_api import sync_playwright
    except ImportError as error:  # pragma: no cover - required host dependency
        pytest.fail(f"Playwright is required for browser regressions: {error}")
    with sync_playwright() as playwright:
        try:
            browser = playwright.chromium.launch(headless=True)
        except Exception as error:  # pragma: no cover - host installation failure
            pytest.fail(f"Chromium is required for browser regressions: {error}")
        yield browser
        browser.close()


def test_selected_membership_keeps_linked_and_unlinked_cells_accessible_after_hydration(
    chromium_browser: Any,
) -> None:
    cells = [
        {
            "test": "applications/charlie",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
        {
            "test": "applications/alpha",
            "category": "applications",
            "mode": "verify",
            "backend": "ptrace",
        },
    ]
    missing = {
        "test": "applications/bravo-no-detail",
        "category": "applications",
        "mode": "verify",
        "backend": "ptrace",
    }
    model = _grouped_link_model(cells)
    row = copy.deepcopy(model.history_rows[0])
    row["profile"] = "full"
    row["cell_results"]["selected_by"] = {
        "full": {"cell_count": 3, "cells": [cells[0], missing, cells[1]]}
    }
    model = replace(
        model,
        current_history_indices=(0,),
        history_paths=("runs/mixed-membership.html",),
        history_rows=(row,),
    )
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    files["runs/mixed-membership.html"] = render_page._run_page(model, 0, row)

    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto("http://127.0.0.1/runs/mixed-membership.html")
    selected = page.locator(
        f'#{render_page._fragment("selected-by", "full")}'
    )
    group = selected.locator(".selected-cell-group")
    members = group.locator(
        ":scope > a:not([hidden]), "
        ":scope > .selected-cell-without-detail:not([hidden])"
    )

    assert "3 cells were selected by full" in selected.inner_text()
    assert members.all_inner_texts() == ["alpha", "bravo-no-detail", "charlie"]
    assert group.inner_text() == "applications: alpha, bravo-no-detail, charlie"
    assert group.get_by_role("link").all_inner_texts() == ["alpha", "charlie"]
    assert group.get_by_text("bravo-no-detail", exact=True).is_visible()
    assert page.locator('[data-list-pagination^="run-selected-"]').count() == 0
    accessibility = group.aria_snapshot()
    for text in ("applications:", "alpha", "bravo-no-detail", "charlie", ","):
        assert text in accessibility

    controls = selected.locator('[data-list-controls^="run-selected-"]')
    controls.locator('[name="q"]').fill("bravo-no-detail")
    assert group.inner_text() == "applications: bravo-no-detail"
    assert members.all_inner_texts() == ["bravo-no-detail"]

    controls.locator('[name="q"]').fill("")
    assert controls.locator('[name="sort"]').is_hidden()
    assert controls.locator('[name="direction"]').is_hidden()
    assert controls.locator('button[type="submit"]').count() == 0
    test_order = controls.locator('[data-list-sort="test"]')
    test_order.click()
    query_prefix = controls.get_attribute("data-query-prefix")
    assert query_prefix is not None
    assert f"{query_prefix}sort=test" in page.url
    assert test_order.get_attribute("aria-pressed") == "true"
    assert test_order.locator(".sort-indicator").inner_text() == "▲"
    assert "Activate to sort descending" in (test_order.get_attribute("title") or "")
    assert members.all_inner_texts() == ["alpha", "bravo-no-detail", "charlie"]

    test_order.press("Space")
    assert f"{query_prefix}direction=desc" in page.url
    assert test_order.locator(".sort-indicator").inner_text() == "▼"
    assert "Activate to sort ascending" in (
        test_order.get_attribute("aria-label") or ""
    )
    assert members.all_inner_texts() == ["charlie", "bravo-no-detail", "alpha"]
    assert group.inner_text() == "applications: charlie, bravo-no-detail, alpha"
    page.close()


def test_custom_command_cards_use_compact_order_buttons_and_url_state(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    model = render_page._site_model(_build(data), data, wording="fixture")
    commands = (
        {
            "cell": "applications/zeta/verify/ptrace",
            "current_reproducer": None,
            "not_applicable_reason": None,
            "test": "applications/zeta",
        },
        {
            "cell": "applications/alpha/verify/ptrace",
            "current_reproducer": None,
            "not_applicable_reason": None,
            "test": "applications/alpha",
        },
    )
    fragment = render_page._custom_commands(
        replace(model, custom_commands=commands), "index.html"
    )
    javascript = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.js"
    ).read_text()
    stylesheet = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.css"
    ).read_text()
    page = chromium_browser.new_page(viewport={"width": 360, "height": 700})
    page.set_content(
        f"<!doctype html><html><head><style>{stylesheet}</style></head>"
        "<body data-page-kind='custom-command-test'>"
        f"{fragment}<script>{javascript}</script></body></html>"
    )
    controls = page.locator('[data-list-controls="custom-commands"]')
    cards = page.locator("#custom-commands .listing-card:not([hidden])")
    test_order = controls.locator('[data-list-sort="test"]')

    assert controls.is_visible()
    assert controls.locator('select[name="sort"]').is_hidden()
    assert controls.locator('select[name="direction"]').is_hidden()
    assert controls.locator('button[type="submit"]').count() == 0
    assert controls.locator("[data-list-clear]").inner_text() == "Clear filters"
    assert controls.locator(".list-order-controls").evaluate(
        "element => getComputedStyle(element).display"
    ) == "flex"
    clear_box = controls.locator("[data-list-clear]").bounding_box()
    controls_box = controls.bounding_box()
    assert clear_box is not None and controls_box is not None
    assert clear_box["width"] < controls_box["width"] / 2
    assert cards.all_inner_texts()[0].startswith("applications/zeta")
    assert test_order.inner_text() == "TestName"

    test_order.click()
    assert "commands_sort=test" in page.url
    assert cards.all_inner_texts()[0].startswith("applications/alpha")
    assert test_order.get_attribute("aria-pressed") == "true"
    assert test_order.locator(".sort-indicator").inner_text() == "▲"

    test_order.press("Space")
    assert "commands_direction=desc" in page.url
    assert cards.all_inner_texts()[0].startswith("applications/zeta")
    assert test_order.locator(".sort-indicator").inner_text() == "▼"

    controls.locator("[data-list-clear]").click()
    assert "commands_sort=" not in page.url
    assert "commands_direction=" not in page.url
    assert cards.all_inner_texts()[0].startswith("applications/zeta")
    page.close()


def _all_mapping_keys(value: Any) -> list[str]:
    if isinstance(value, dict):
        return [
            key
            for name, item in value.items()
            for key in (name, *_all_mapping_keys(item))
        ]
    if isinstance(value, list):
        return [key for item in value for key in _all_mapping_keys(item)]
    return []


def test_non_sortable_column_help_is_visible_on_keyboard_focus(
    chromium_browser: Any,
) -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto("http://127.0.0.1/cells/projection-audit.html")
    help_button = page.locator(".column-help").first
    described_by = help_button.get_attribute("aria-describedby")

    assert described_by
    assert help_button.get_attribute("aria-label").startswith("Explain ")
    assert help_button.inner_text() == "?"
    tooltip = page.locator(f"#{described_by}")
    assert tooltip.get_attribute("role") == "tooltip"
    assert tooltip.text_content()
    assert (
        tooltip.evaluate("element => getComputedStyle(element).visibility") == "hidden"
    )

    help_button.focus()
    assert help_button.evaluate("element => document.activeElement === element")
    assert (
        tooltip.evaluate("element => getComputedStyle(element).visibility") == "visible"
    )
    assert tooltip.is_visible()
    assert tooltip.inner_text()
    assert (
        float(tooltip.evaluate("element => getComputedStyle(element).opacity")) == 1.0
    )
    page.keyboard.press("Tab")
    assert not help_button.evaluate("element => document.activeElement === element")
    page.close()


def test_cell_detail_omits_run_selection_aggregate_but_preserves_authority(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    data["cells"][0]["selected_by_full"] = False
    data["cells"][0]["not_selected_by_full_reason"] = {
        "evidence": 'recorded <evidence> & "detail"',
        "reason": "full excludes <unsafe> & this cell",
        "result": "timeout",
    }
    data = _validated(data)
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto(f"http://127.0.0.1/{model.cell_paths[data['cells'][0]['cell']]}")
    _wait_for_verified_data(page)
    text = page.locator("[data-detail-content]").inner_text()

    assert "Selection" not in text
    assert "Selected by full" not in text
    assert "not_selected_by_full_reason" not in text
    digest = _digest_for_identity(data["cells"][0]["cell"])
    record = next(
        item
        for item in _browser_payload(files, f"data/detail-{digest[0]}.json.gz")[
            "records"
        ]
        if item["kind"] == "cell" and item["digest"] == digest
    )
    assert record["data"]["not_selected_by_full_reason"] == data["cells"][0][
        "not_selected_by_full_reason"
    ]
    page.goto(f"http://127.0.0.1/{model.test_paths[data['tests'][0]['id']]}")
    _wait_for_verified_data(page)
    assert page.locator(".population-control").count() == 0
    page.close()


def test_cell_matrix_has_no_per_cell_selection_columns_before_or_after_hydration(
    chromium_browser: Any,
) -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    page = chromium_browser.new_page()
    _browser_route(page, files)

    for path in ("cells/index.html", "cells/never-measured.html"):
        headings = ColumnHeadings()
        headings.feed(files[path].decode())
        labels = [column["label"] for column in headings.columns[:10]]
        assert "Selected by full" not in labels
        assert "Selected by path" not in labels

        page.goto(f"http://127.0.0.1/{path}")
        _wait_for_verified_data(page)
        table = page.locator("#test-matrix table").first
        browser_labels = table.locator("thead th").all_inner_texts()
        assert all("Selected by" not in label for label in browser_labels)
        rows = table.locator("tbody tr")
        assert rows.count() > 0
        assert rows.evaluate_all(
            "(values, width) => values.every(row => row.children.length === width)",
            len(browser_labels),
        )
        assert all(
            table.locator(f"tbody tr > :nth-child({index})").evaluate_all(
                "values => values.some(value => value.textContent.trim().length > 0)"
            )
            for index in range(1, len(browser_labels) + 1)
        )
    page.close()


def test_detail_has_one_reader_title_and_omits_one_page_pagination(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    source = files["tests/detail.html"].decode()

    assert "<h1>Test detail</h1>" not in source
    assert "<h2>Recorded detail</h2>" not in source
    assert "data-raw-artifact" not in source

    page = chromium_browser.new_page()
    _browser_route(page, files)
    test = data["tests"][0]
    page.goto(f"http://127.0.0.1/{model.test_paths[test['id']]}")
    _wait_for_verified_data(page)
    assert page.locator("main h1").all_inner_texts() == [
        render_page._test_name(test["id"], test["category"])
    ]
    assert page.locator("[data-detail-content] .list-pagination").count() == 0
    page.close()


def test_run_number_sort_is_numeric_and_keeps_fallback_rows_last(
    chromium_browser: Any,
) -> None:
    row = representative_build_data()["runs"]["validation_history_rows"][0]
    rows = []
    for run_id, run_number in (
        ("validate-run-u64-max", render_page.MAX_U64),
        ("validate-run-ten", 10),
        ("validate-run-two-a", 2),
        ("validate-run-two-b", 2),
        ("validate-run-without-number", True),
    ):
        item = copy.deepcopy(row)
        item["run_id"] = run_id
        item["run_number"] = run_number
        rows.append(item)
    model = SimpleNamespace(
        current_history_indices=(0,),
        history_paths=tuple(f"runs/{index}.html" for index in range(len(rows))),
        history_rows=tuple(rows),
    )
    listing = render_page._run_listing(model, "runs/index.html")
    javascript = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.js"
    ).read_text()
    source = (
        "<!doctype html><html><body data-page-kind='run-list-test'>"
        f"{listing}<script>{javascript}</script></body></html>"
    )
    page = chromium_browser.new_page()
    page.set_content(source)

    controls = page.locator('[data-list-controls="validation-runs"]')
    run_heading = page.locator('[data-list-sort="run"]')
    run_heading.click()
    assert run_heading.locator(".sort-indicator").inner_text() == "▲"
    assert run_heading.locator("xpath=..").get_attribute("aria-sort") == "ascending"
    assert "runs_sort=run" in page.url
    assert page.locator("tbody tr:not([hidden]) th a").all_inner_texts() == [
        "Run 2",
        "Run 2",
        "Run 10",
        f"Run {render_page.MAX_U64}",
        "Run id validate-run-without-number",
    ]
    assert page.locator("tbody tr:not([hidden]) th a").evaluate_all(
        "links => links.map(link => link.getAttribute('href'))"
    )[:2] == ["2.html", "3.html"]

    run_heading.click()
    assert run_heading.locator(".sort-indicator").inner_text() == "▼"
    assert run_heading.locator("xpath=..").get_attribute("aria-sort") == "descending"
    assert "runs_sort=run" in page.url
    assert "runs_direction=" not in page.url
    assert page.locator("tbody tr:not([hidden]) th a").all_inner_texts() == [
        f"Run {render_page.MAX_U64}",
        "Run 10",
        "Run 2",
        "Run 2",
        "Run id validate-run-without-number",
    ]

    controls.locator('[name="q"]').fill("validate-run-two-b")
    assert page.locator("tbody tr:not([hidden]) th a").all_inner_texts() == ["Run 2"]
    assert page.locator("tbody tr:not([hidden]) th a").get_attribute("href") == (
        "3.html"
    )
    page.close()


def test_validation_runs_default_to_started_descending_and_use_recorded_wall_clock(
    chromium_browser: Any,
) -> None:
    template = representative_build_data()["runs"]["validation_history_rows"][0]
    rows = []
    for index, (started_at, real_seconds) in enumerate(
        (
            ("2026-08-31T00:01:00Z", 999.5),
            ("2026-08-31T00:03:00Z", None),
            ("2026-08-31T00:03:00Z", 7.25),
        ),
        1,
    ):
        row = copy.deepcopy(template)
        row.update(
            record_id=f"record-{index}",
            run_id=f"run-{index}",
            run_number=index,
            started_at=started_at,
            finished_at="2026-08-31T00:03:01Z",
        )
        row.pop("corrects", None)
        if real_seconds is None:
            row.pop("real_seconds", None)
        else:
            row["real_seconds"] = real_seconds
        rows.append(row)
    model = SimpleNamespace(
        current_history_indices=(0, 1, 2),
        history_paths=tuple(f"runs/{index}.html" for index in range(len(rows))),
        history_rows=tuple(rows),
    )
    listing = render_page._run_listing(model, "runs/index.html")
    javascript = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.js"
    ).read_text()
    stylesheet = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.css"
    ).read_text()
    source = (
        "<!doctype html><html><head>"
        f"<style>{stylesheet}</style></head>"
        "<body data-page-kind='run-list-test'>"
        f"{listing}<script>{javascript}</script></body></html>"
    )
    page = chromium_browser.new_page(viewport={"width": 600, "height": 700})
    page.set_content(source)

    started = page.locator('[data-list-sort="started"]')
    assert started.locator("xpath=..").get_attribute("aria-sort") == "descending"
    assert page.locator("tbody tr:not([hidden]) th a").all_inner_texts() == [
        "Run 3",
        "Run 2",
        "Run 1",
    ]
    visible_rows = page.locator("tbody tr:not([hidden])")
    assert visible_rows.nth(0).locator("td").last.inner_text() == "7.25 seconds"
    assert visible_rows.nth(1).locator("td").last.inner_text() == "Not recorded"
    assert visible_rows.nth(2).locator("td").last.inner_text() == "999.5 seconds"
    assert "Finished" not in page.locator("#validation-runs").inner_text()
    table = page.locator(".validation-runs-table")
    assert table.count() == 1
    assert table.locator("col.run-column").count() == 1
    assert table.evaluate("element => getComputedStyle(element).tableLayout") == "fixed"
    scroll = page.locator("#validation-runs .table-scroll")
    assert scroll.evaluate("element => element.scrollWidth > element.clientWidth")
    assert (
        table.locator("tbody th").first.evaluate(
            "element => element.getBoundingClientRect().width"
        )
        < 250
    )
    wall_clock = page.locator('[data-list-sort="wall_clock"]')
    assert wall_clock.get_attribute("title") == (
        "The optional recorded outer makespan of the whole validation run, in "
        "seconds; it is not derived from the Started or Finished timestamps."
    )
    wall_clock.click()
    assert page.locator("tbody tr:not([hidden]) th a").all_inner_texts() == [
        "Run 3",
        "Run 1",
        "Run 2",
    ]
    page.close()


@pytest.mark.parametrize(
    "viewport",
    ({"width": 1280, "height": 800}, {"width": 360, "height": 700}),
    ids=("desktop", "mobile"),
)
def test_long_fallback_run_id_is_single_line_clipped_and_fully_accessible(
    chromium_browser: Any, viewport: dict[str, int]
) -> None:
    template = representative_build_data()["runs"]["validation_history_rows"][0]
    long_run_id = "validate-" + "very-long-run-identifier-" * 12
    fallback = copy.deepcopy(template)
    fallback.update(record_id="record-fallback", run_id=long_run_id)
    fallback.pop("run_number", None)
    numbered = copy.deepcopy(template)
    numbered.update(record_id="record-numbered", run_id="numbered-run", run_number=42)
    for row in (fallback, numbered):
        row.pop("corrects", None)
    history_path = f"runs/{'a' * 64}.html"
    model = SimpleNamespace(
        current_history_indices=(0, 1),
        history_paths=(history_path, f"runs/{'b' * 64}.html"),
        history_rows=(fallback, numbered),
    )
    listing = render_page._run_listing(
        model, "runs/index.html", show_heading=False
    )
    javascript = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.js"
    ).read_text()
    stylesheet = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.css"
    ).read_text()
    page = chromium_browser.new_page(viewport=viewport)
    page.set_content(
        "<!doctype html><html><head>"
        f"<style>{stylesheet}</style></head>"
        "<body data-page-kind='run-list-test'>"
        f"{listing}<script>{javascript}</script></body></html>"
    )

    link = page.locator("a.run-id-link")
    cell = link.locator("xpath=..")
    full_label = f"Run id {long_run_id}"
    assert link.inner_text() == full_label
    assert link.get_attribute("title") == full_label
    assert link.get_attribute("href") == PurePosixPath(history_path).name
    assert full_label in link.aria_snapshot()
    assert link.evaluate(
        "element => ({"
        "overflow: getComputedStyle(element).overflow, "
        "textOverflow: getComputedStyle(element).textOverflow, "
        "whiteSpace: getComputedStyle(element).whiteSpace, "
        "clipped: element.scrollWidth > element.clientWidth"
        "})"
    ) == {
        "overflow": "hidden",
        "textOverflow": "ellipsis",
        "whiteSpace": "nowrap",
        "clipped": True,
    }
    cell_box = cell.bounding_box()
    link_box = link.bounding_box()
    assert cell_box is not None
    assert link_box is not None
    assert cell_box["width"] <= 190
    assert link_box["height"] <= 26
    assert cell_box["height"] <= 80

    numbered_link = page.get_by_role("link", name="Run 42", exact=True)
    assert numbered_link.get_attribute("class") is None
    assert numbered_link.get_attribute("title") is None
    controls = page.locator('[data-list-controls="validation-runs"]')
    controls.locator('[name="q"]').fill(long_run_id)
    assert page.locator("tbody tr:not([hidden])").count() == 1
    assert page.locator("tbody tr:not([hidden]) a.run-id-link").inner_text() == full_label
    page.close()


def test_invalid_run_query_restores_one_consistent_default_without_fetch(
    chromium_browser: Any,
) -> None:
    template = representative_build_data()["runs"]["validation_history_rows"][0]
    rows = []
    for index, started_at in enumerate(
        (
            "2026-08-31T00:01:00Z",
            "2026-08-31T00:03:00Z",
            "2026-08-31T00:03:00Z",
        ),
        1,
    ):
        row = copy.deepcopy(template)
        row.update(
            record_id=f"record-{index}",
            run_id=f"run-{index}",
            run_number=index,
            started_at=started_at,
        )
        row.pop("corrects", None)
        rows.append(row)
    model = SimpleNamespace(
        current_history_indices=(0, 1, 2),
        history_paths=tuple(f"runs/{index}.html" for index in range(len(rows))),
        history_rows=tuple(rows),
    )
    listing = render_page._run_listing(model, "runs/index.html")
    javascript = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.js"
    ).read_bytes()
    files = {
        "runs/index.html": (
            "<!doctype html><html><body data-page-kind='run-list-test'>"
            f"{listing}<script src='../assets/site.js'></script></body></html>"
        ).encode(),
        "assets/site.js": javascript,
    }
    requests: list[str] = []
    page = chromium_browser.new_page()
    _browser_route(page, files, requests=requests)
    page.goto(
        "http://127.0.0.1/runs/index.html?runs_sort=unsupported&runs_direction=asc"
    )
    controls = page.locator('[data-list-controls="validation-runs"]')
    error = page.locator('[data-list-error="validation-runs"]')
    started = page.locator('[data-list-sort="started"]')
    page.wait_for_function(
        "!document.querySelector('[data-list-controls=\"validation-runs\"]')"
        ".hidden"
    )

    assert error.is_visible()
    assert "runs_sort has unsupported value unsupported" in error.inner_text()
    assert page.locator("tbody tr:not([hidden]) th a").all_inner_texts() == [
        "Run 3",
        "Run 2",
        "Run 1",
    ]
    assert controls.locator('[name="sort"]').input_value() == "started"
    assert controls.locator('[name="direction"]').input_value() == "desc"
    assert started.locator("xpath=..").get_attribute("aria-sort") == "descending"
    assert started.locator(".sort-indicator").inner_text() == "▼"
    assert page.url.endswith("?runs_sort=unsupported&runs_direction=asc")
    assert not any(path.startswith("data/") for path in requests)
    page.close()


def test_run_detail_uses_recorded_wall_clock_and_does_not_derive_duration() -> None:
    data = representative_build_data()
    for field in ("validation_history_rows", "current_validation_run_rows"):
        data["runs"][field][0]["real_seconds"] = 7.25
        data["runs"][field][0]["started_at"] = "2026-08-31T00:00:00Z"
        data["runs"][field][0]["finished_at"] = "2026-08-31T00:59:59Z"
    _refresh_projection_digests(data)
    data = _validated(data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    source = render_page.render_site(_build(data), data)[model.history_paths[0]].decode()

    assert "<dt>Wall clock</dt><dd>7.25 seconds</dd>" in source
    assert "<dt>Finished</dt>" not in source
    assert "2026-08-30 20:00:00 ET" in source


def test_live_search_change_keeps_result_links_clickable(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    _set_run_number(data, 10)
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    page = chromium_browser.new_page()
    _browser_route(page, files)

    page.goto("http://127.0.0.1/runs/index.html")
    controls = page.locator('[data-list-controls="validation-runs"]')
    search = controls.locator('[name="q"]')
    search.fill("validate-run-1")
    run_link = page.locator("tbody tr:not([hidden]) th a")
    assert run_link.count() == 1
    run_handle = run_link.element_handle()
    assert run_handle is not None
    search.blur()
    assert run_handle.evaluate("element => element.isConnected")

    search.focus()
    with page.expect_navigation():
        run_link.click()
    assert page.url == f"http://127.0.0.1/{model.history_paths[0]}"
    assert page.locator("h1").inner_text() == "Run 10"

    page.goto("http://127.0.0.1/runs/index.html")
    controls = page.locator('[data-list-controls="validation-runs"]')
    search = controls.locator('[name="q"]')
    search.fill("validate-run-1")
    search.press("Enter")
    assert page.locator("tbody tr:not([hidden]) th a").count() == 1
    record = controls.locator('[name="record"]')
    record.select_option("historical")
    assert page.locator("tbody tr:not([hidden]) th a").count() == 0
    record.select_option("current")
    assert page.locator("tbody tr:not([hidden]) th a").count() == 1

    page.goto("http://127.0.0.1/cells/index.html#all-cells")
    _wait_for_verified_data(page)
    cell_search = page.locator("[data-cell-filter-form] [name='q']")
    cell_search.fill(data["cells"][0]["cell"])
    cell_link = page.locator('#cell-results a[href*="?cell="]')
    assert cell_link.count() == 1
    cell_handle = cell_link.element_handle()
    assert cell_handle is not None
    cell_search.blur()
    assert cell_handle.evaluate("element => element.isConnected")
    cell_search.focus()
    with page.expect_navigation():
        cell_link.click()
    assert page.url == f"http://127.0.0.1/{model.cell_paths[data['cells'][0]['cell']]}"
    page.close()


def test_browser_manifest_binds_every_fixed_payload_before_json_parse() -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    manifest = _browser_payload(files, "data/site-manifest.json.gz")
    expected = {
        "data/site-summary.json.gz",
        *(f"data/detail-{prefix}.json.gz" for prefix in "0123456789abcdef"),
    }

    assert manifest["schema_version"] == 1
    assert {item["path"] for item in manifest["artifacts"]} == expected
    for item in manifest["artifacts"]:
        assert set(item) == {"decoded_bytes", "decoded_sha256", "path"}
        decoded = gzip.decompress(files[item["path"]])
        assert item["decoded_bytes"] == len(decoded)
        assert item["decoded_sha256"] == hashlib.sha256(decoded).hexdigest()


def test_browser_records_derive_state_and_preserve_structured_manifest_values() -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    payloads = [
        _browser_payload(files, "data/site-summary.json.gz"),
        *(
            _browser_payload(files, f"data/detail-{prefix}.json.gz")
            for prefix in "0123456789abcdef"
        ),
    ]
    records = [record for payload in payloads[1:] for record in payload["records"]]
    cell = next(record for record in records if record["kind"] == "cell")
    test = next(record for record in records if record["kind"] == "test")
    all_states = data_with_all_three_states_and_one_omitted_row()
    all_state_files = render_page.render_site(_build(all_states), all_states)
    never_measured = next(
        record
        for record in _browser_payload(all_state_files, "data/site-summary.json.gz")[
            "cells"
        ]
        if record["qualifying_physical_row_count"] == 0
    )

    assert {"measured", "admitted", "state", "never_measured"}.isdisjoint(
        key for payload in payloads for key in _all_mapping_keys(payload)
    )
    assert cell["data"]["qualifying_physical_row_count"] >= 0
    assert test["data"]["build"] == data["tests"][0]["build"]
    assert test["data"]["direct"] == data["tests"][0]["direct"]
    for field in (
        "category",
        "current_reproducer",
        "current_reproducer_unavailable_reason",
        "lane",
        "not_selected_by_full_reason",
        "physical_row_count",
        "qualifying_physical_row_count",
        "qualifying_represented_run_count",
        "represented_run_count",
        "selected_by",
        "selected_by_full",
    ):
        assert cell["data"][field] == data["cells"][0][field]
    assert all(
        set(fact["run"])
        == {"commit", "history_row_sha256", "path", "run_id", "selection"}
        for facts in cell["selection"].values()
        for fact in facts
    )
    for field in (
        "observation",
        "occasional",
        "preprocessors",
        "requires",
    ):
        assert test["data"][field] == data["tests"][0][field]
    assert {
        field: never_measured[field]
        for field in ("latest_result", "latest_result_time", "latest_result_tree")
    } == {
        "latest_result": None,
        "latest_result_time": None,
        "latest_result_tree": None,
    }


def test_recent_passing_execution_requires_the_exact_validate_verdict_join() -> None:
    data = _add_runtime_counts(representative_build_data())
    model = render_page._site_model(_build(data), data, wording="fixture")
    cell = model.cells[0]

    assert render_page._recent_passing_execution(model, cell) == {
        "attempt": None,
        "emitted_at": "2026-08-31T00:01:00Z",
        "event_id": "event-1",
        "info_messages_run1": 1,
        "info_messages_run2": 1,
        "run": {
            "commit": "1" * 40,
            "history_row_sha256": "4" * 64,
            "path": model.history_paths[0],
            "run_id": "validate-run-1",
            "selection": "super",
        },
        "run_index": 1,
        "syscalls_run1": 12,
        "syscalls_run2": 22,
    }

    zero_index_row = copy.deepcopy(model.series_by_cell[str(cell["cell"])][0])
    zero_index_row["series"]["run_index"] = 0
    zero_index = replace(model, series_by_cell={str(cell["cell"]): (zero_index_row,)})
    assert render_page._recent_passing_execution(zero_index, cell)["run_index"] == 0

    assert (
        render_page._recent_passing_execution(
            replace(model, event_run_references={}), cell
        )
        is None
    )

    retry_ambiguous = copy.deepcopy(cell)
    retry_ambiguous["selected_by"]["super"][0]["event_ids"].append("event-retry")
    assert render_page._recent_passing_execution(model, retry_ambiguous) is None

    selection_mismatch = copy.deepcopy(cell)
    selection_mismatch["selected_by"]["super"][0]["run_id"] = "other-run"
    assert render_page._recent_passing_execution(model, selection_mismatch) is None

    history = copy.deepcopy(model.history_rows[0])
    history["cell_results"]["recorded"]["cells"][0]["cell_verdict"] = {
        "comparison_tier": "unavailable",
        "reason": "the comparison was not recorded",
        "state": "cell-verdict-unavailable",
    }
    unavailable = replace(model, history_rows=(history,))
    assert render_page._recent_passing_execution(unavailable, cell) is None


def _model_with_red_kvm_cell() -> tuple[Any, dict[str, Any]]:
    data = _add_runtime_counts(representative_build_data())
    model = render_page._site_model(_build(data), data, wording="fixture")
    ptrace = model.cells[0]
    current = copy.deepcopy(model.series_by_cell[str(ptrace["cell"])][0])
    current.update(
        event_id="event-kvm-current",
        emitted_at="2026-09-02T00:00:00Z",
        producer="pressure-test",
        product_result="product_failure",
        run_id="pressure-current",
    )
    current["series"].update(
        cell="tests/hello/verify/kvm",
        coordinates={
            "first_divergent_record": 9,
            "first_divergent_syscall": 7,
            "first_divergent_scheduler_turn": 8,
            "first_divergent_virtual_nanoseconds": 900,
        },
        outcome="diverged",
        result="product_failure",
    )
    previous = copy.deepcopy(current)
    previous.update(
        event_id="event-kvm-previous",
        emitted_at="2026-09-01T00:00:00Z",
        run_id="pressure-previous",
    )
    previous["series"].update(
        tree="2" * 40,
        coordinates={
            **current["series"]["coordinates"],
            "first_divergent_scheduler_turn": 3,
        },
    )
    kvm = {
        **copy.deepcopy(ptrace),
        "backend": "kvm",
        "cell": "tests/hello/verify/kvm",
        "event_ids": ["event-kvm-current", "event-kvm-previous"],
        "latest_event_ids": ["event-kvm-current"],
        "latest_result": "product_failure",
        "latest_result_time": "2026-09-02T00:00:00Z",
        "latest_result_tree": "1" * 40,
        "qualifying_event_ids": ["event-kvm-current", "event-kvm-previous"],
        "selected_by": {},
        "state": "red",
    }
    return (
        replace(
            model,
            cells=(ptrace, kvm),
            cell_by_id={str(ptrace["cell"]): ptrace, str(kvm["cell"]): kvm},
            series_rows=(*model.series_rows, previous, current),
            series_by_cell={
                str(ptrace["cell"]): model.series_by_cell[str(ptrace["cell"])],
                str(kvm["cell"]): (previous, current),
            },
            series_by_event={
                **model.series_by_event,
                str(previous["event_id"]): previous,
                str(current["event_id"]): current,
            },
            cell_paths={
                **model.cell_paths,
                str(kvm["cell"]): "cells/detail.html?cell="
                + _digest_for_identity(str(kvm["cell"])),
            },
        ),
        kvm,
    )


def test_backend_leaderboard_uses_same_commit_ptrace_scheduler_turn_length() -> None:
    model, kvm = _model_with_red_kvm_cell()

    rows = {row["backend"]: row for row in render_page._backend_rows(model)}
    assert rows["kvm"] == {
        "backend": "kvm",
        "denominator": 1,
        "excluded": 0,
        "green": 0,
        "green_percentage": 0.0,
        "partial_percentage": 0.5,
        "unavailable": 0,
    }
    source = render_page._backend_leaderboard(model)
    assert "Green cells" in source
    assert "Counting partial credit" in source
    assert "0.0%" in source
    assert "50.0%" in source
    assert "same-commit ptrace execution" in source
    assert [row["backend"] for row in render_page._backend_rows(model)] == [
        "ptrace",
        "kvm",
    ]
    no_score = {**copy.deepcopy(kvm), "backend": "liteinst", "mode": "run"}
    no_score["cell"] = "tests/hello/run/liteinst"
    ordered = render_page._backend_rows(replace(model, cells=(*model.cells, no_score)))
    assert [row["backend"] for row in ordered] == ["ptrace", "kvm", "liteinst"]

    wrong_commit_ptrace = copy.deepcopy(model.cell_by_id["tests/hello/verify/ptrace"])
    wrong_commit_ptrace["latest_result_tree"] = "3" * 40
    historical_reference_model = replace(
        model,
        cell_by_id={
            **model.cell_by_id,
            "tests/hello/verify/ptrace": wrong_commit_ptrace,
        },
    )
    still_available = {
        row["backend"]: row
        for row in render_page._backend_rows(historical_reference_model)
    }["kvm"]
    assert still_available["partial_percentage"] == 0.5

    unmatched = copy.deepcopy(kvm)
    unmatched["latest_result_tree"] = "3" * 40
    unmatched_current = copy.deepcopy(model.series_by_event["event-kvm-current"])
    unmatched_current["series"]["tree"] = "3" * 40
    unavailable_model = replace(
        model,
        cells=(model.cells[0], unmatched),
        cell_by_id={
            str(model.cells[0]["cell"]): model.cells[0],
            str(unmatched["cell"]): unmatched,
        },
        series_by_cell={
            **model.series_by_cell,
            str(unmatched["cell"]): (
                model.series_by_event["event-kvm-previous"],
                unmatched_current,
            ),
        },
        series_by_event={
            **model.series_by_event,
            "event-kvm-current": unmatched_current,
        },
    )
    unavailable = {
        row["backend"]: row for row in render_page._backend_rows(unavailable_model)
    }["kvm"]
    assert unavailable["green_percentage"] == 0.0
    assert unavailable["partial_percentage"] is None
    assert unavailable["unavailable"] == 1

    no_reference_model = replace(
        model,
        cell_by_id={str(kvm["cell"]): kvm},
        cells=(kvm,),
    )
    excluded = render_page._backend_rows(no_reference_model)[0]
    assert excluded["denominator"] == 0
    assert excluded["excluded"] == 1
    assert excluded["green_percentage"] is None
    assert excluded["partial_percentage"] is None
    assert render_page._red_cell_partial_credit(no_reference_model, kvm) == {
        "estimated": False,
        "reason": "the matching test and mode are not green on ptrace",
        "value": None,
    }

    native = {**copy.deepcopy(kvm), "backend": "native"}
    native["cell"] = "tests/hello/verify/native"
    e9patch = {**copy.deepcopy(kvm), "backend": "e9patch"}
    e9patch["cell"] = "tests/hello/verify/e9patch"
    without_non_backends = replace(model, cells=(*model.cells, native, e9patch))
    assert {"native", "e9patch"}.isdisjoint(
        row["backend"] for row in render_page._backend_rows(without_non_backends)
    )


def test_ptrace_scheduler_turn_length_weights_latest_passing_rows_by_num_runs() -> None:
    cell = {
        "cell": "tests/hello/verify/ptrace",
        "latest_result": "pass",
        "qualifying_event_ids": ["event-1", "event-2"],
        "state": "green",
    }

    def row(event_id: str, weight: int, left: int, right: int) -> dict[str, Any]:
        return {
            "emitted_at": "2026-09-02T00:00:00Z",
            "event_id": event_id,
            "product_result": "pass",
            "series": {
                "cell": cell["cell"],
                "num_runs": weight,
                "runtime": {
                    "run1": {"scheduler_turns": left},
                    "run2": {"scheduler_turns": right},
                },
                "tree": "1" * 40,
            },
        }

    rows = (row("event-1", 1, 10, 30), row("event-2", 3, 20, 40))
    model = SimpleNamespace(series_by_cell={str(cell["cell"]): rows})
    assert render_page._passing_scheduler_turn_length(
        model, cell, "1" * 40
    ) == render_page.Fraction(55, 2)


def test_red_cell_browser_record_has_four_coordinates_and_turn_difference() -> None:
    model, kvm = _model_with_red_kvm_cell()
    record = render_page._browser_cell_record(model, kvm)

    assert record["latest_divergence"] == {
        "coordinates": {
            "first_divergent_record": "9",
            "first_divergent_scheduler_turn": "8",
            "first_divergent_syscall": "7",
            "first_divergent_virtual_nanoseconds": "900",
        },
        "scheduler_turn_difference": {
            "previous_commit": "2" * 40,
            "reason": None,
            "value": "5",
        },
    }
    assert record["partial_credit"] == {
        "estimated": False,
        "reason": None,
        "value": "50.0%",
    }


def test_turn_difference_uses_qualifying_divergences() -> None:
    model, kvm = _model_with_red_kvm_cell()
    intervening = copy.deepcopy(model.series_by_event["event-kvm-current"])
    intervening.update(
        event_id="event-kvm-unqualified",
        emitted_at="2026-09-01T12:00:00Z",
        run_id="pressure-unqualified",
    )
    intervening["series"]["coordinates"]["first_divergent_scheduler_turn"] = 7
    with_unqualified = replace(
        model,
        series_by_cell={
            **model.series_by_cell,
            str(kvm["cell"]): (*model.series_by_cell[str(kvm["cell"])], intervening),
        },
    )

    difference = render_page._latest_divergence(with_unqualified, kvm)[
        "scheduler_turn_difference"
    ]
    assert difference == {
        "previous_commit": "2" * 40,
        "reason": None,
        "value": 5,
    }


def test_ptrace_red_partial_credit_is_labelled_as_an_estimate() -> None:
    model, kvm = _model_with_red_kvm_cell()
    ptrace_red = {**kvm, "backend": "ptrace"}

    partial_credit = render_page._red_cell_partial_credit(model, ptrace_red)

    assert partial_credit == {
        "estimated": True,
        "reason": None,
        "value": render_page.Fraction(1, 2),
    }


def test_overview_tests_are_complete_wrapping_name_lists_by_category() -> None:
    data = representative_build_data()
    model = render_page._site_model(_build(data), data, wording="fixture")
    second = copy.deepcopy(model.tests[0])
    second.update(category="applications", id="applications/z-last", cells=[])
    third = copy.deepcopy(second)
    third["id"] = "applications/a-first"
    paths = {
        **model.test_paths,
        str(second["id"]): "tests/detail.html?test=" + _digest_for_identity(second["id"]),
        str(third["id"]): "tests/detail.html?test=" + _digest_for_identity(third["id"]),
    }

    source = render_page._test_listing(
        replace(model, tests=(model.tests[0], second, third), test_paths=paths),
        "index.html",
    )

    assert source.count('class="test-category"') == 2
    assert '<h3>applications</h3>' in source
    assert "z-last" in source and "a-first" in source and "hello" in source
    assert re.search(r">a-first</code></a>, <a [^>]+><code>z-last</code></a>", source)
    assert 'class="test-name-list"' in source
    assert 'class="list-controls"' not in source
    assert 'class="listing-card"' not in source
    assert "cells in the manifest" not in source


def test_detail_shards_only_hold_runtime_detail_records_and_keep_evidence() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    records = [
        record
        for prefix in "0123456789abcdef"
        for record in _browser_payload(files, f"data/detail-{prefix}.json.gz")[
            "records"
        ]
    ]

    assert {record["kind"] for record in records} == {"cell", "test"}
    published_events = {
        event_id for cell in data["cells"] for event_id in cell["event_ids"]
    }
    detail_events = {
        item["event_id"]
        for record in records
        if record["kind"] == "cell"
        for item in record["evidence"]
    }
    omitted_events = {
        reference["event_id"]
        for omission in data["projection_audit"]["deliberate_omissions"]
        for reference in omission["event_references"]
    }
    assert detail_events == published_events
    assert omitted_events == {"event-without-result"}


def test_recorded_evidence_is_not_mislabeled_as_qualifying_evidence(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    cell = data["cells"][0]
    row = copy.deepcopy(data["runs"]["series_rows"][0])
    row.update(
        event_id="event-admitted-but-not-qualifying",
        producer="pressure-test",
        product_result=None,
        run_id="pressure-run",
    )
    row["series"].update(
        failure_class="understood_prerequisite_failure",
        outcome="skipped",
        result=None,
        tree="3333333333333333333333333333333333333333",
    )
    row["current_main_ancestry"] = False
    data["runs"]["series_rows"].append(row)
    data["runs"]["product_results"][row["event_id"]] = None
    data["runs"]["projection_results"][row["event_id"]] = None
    data["runs"]["current_main_ancestry"][row["series"]["tree"]] = False
    cell["event_ids"].append(row["event_id"])
    cell["physical_row_count"] += 1
    cell["represented_run_count"] += 1
    for field in (
        "projection_admitted_physical_row_count",
        "projection_admitted_represented_run_count",
        "series_physical_row_count",
        "series_represented_run_count",
    ):
        data["runs"][field] += 1
    data["counts"]["physical_row_count"] += 1
    data["counts"]["represented_run_count"] += 1
    data["provenance"]["series_physical_row_count"] += 1
    data["provenance"]["series_represented_run_count"] += 1
    _refresh_projection_digests(data)
    data = _validated(data)
    files = render_page.render_site(_build(data), data)
    digest = _digest_for_identity(cell["cell"])
    shard = _browser_payload(files, f"data/detail-{digest[0]}.json.gz")
    record = next(item for item in shard["records"] if item["digest"] == digest)

    assert len(record["evidence"]) == 2
    assert record["data"]["qualifying_physical_row_count"] == 1
    unlinked = next(
        item
        for item in record["evidence"]
        if item["event_id"] == "event-admitted-but-not-qualifying"
    )
    assert unlinked["run_id"] == "pressure-run"
    assert "run" not in unlinked

    model = render_page._site_model(_build(data), data, wording="fixture")
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto(f"http://127.0.0.1/{model.cell_paths[cell['cell']]}")
    _wait_for_verified_data(page)
    evidence = page.locator(
        f"#{render_page._fragment('event', 'event-admitted-but-not-qualifying')}"
    )
    assert "Run id pressure-run (no exact validation row link)" in evidence.inner_text()
    assert evidence.locator("a", has_text="pressure-run").count() == 0
    page.close()


def test_omission_rows_have_stable_event_anchors_without_raw_links() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    page = render_page.render_site(_build(data), data)[
        "cells/projection-audit.html"
    ].decode()

    event_anchor = render_page._fragment("event", "event-without-result")
    omission_anchor = render_page._fragment(
        "omission", data["projection_audit"]["deliberate_omissions"][0]["cell"]
    )
    assert f'id="{event_anchor}"' in page
    assert f'id="{omission_anchor}"' in page
    assert f'href="data/runs.json.gz#{event_anchor}"' not in page
    assert "event-without-result</code>" in page
    assert (
        "For this chaos/ptrace cell, pressure-test recorded 1 qualifying "
        "repetition across 1 qualifying ledger row representing 1 Hermit run. "
        "The latest repetition records outcome skipped and failure class "
        "understood prerequisite failure. No narrower cause was recorded for this "
        "evidence. No pass or product failure result was recorded for that "
        "latest execution, so this cell is excluded." in page
    )
    assert "<summary>1 exact event reference</summary>" in page
    assert (
        "Recorded evidence whose cell identity is outside the current manifest."
        in render_page.render_site(_build(data), data)["cells/index.html"].decode()
    )


def _omission_summary_for_rows(
    specifications: list[tuple[int, int | None, int]],
    *,
    producer: str = "pressure-test",
) -> str:
    data = data_with_all_three_states_and_one_omitted_row()
    model = render_page._site_model(_build(data), data, wording="fixture")
    omission = copy.deepcopy(data["projection_audit"]["deliberate_omissions"][0])
    template = model.series_by_event["event-without-result"]
    generated = []
    for ordinal, (run_index, attempt, num_runs) in enumerate(specifications):
        row = copy.deepcopy(template)
        row["event_id"] = f"omission-{ordinal:02d}"
        row["emitted_at"] = f"2026-08-31T00:{run_index:02d}:00Z"
        row["producer"] = producer
        row["series"]["run_index"] = run_index
        row["series"]["attempt"] = attempt
        row["series"]["num_runs"] = num_runs
        generated.append(row)
    events = dict(model.series_by_event)
    events.pop("event-without-result")
    events.update((str(row["event_id"]), row) for row in generated)
    model = replace(
        model,
        series_by_event=events,
        series_rows=tuple(
            row
            for row in model.series_rows
            if row["event_id"] != "event-without-result"
        )
        + tuple(generated),
    )
    omission.update(
        qualifying_event_ids=sorted(str(row["event_id"]) for row in generated),
        qualifying_physical_row_count=len(generated),
        qualifying_represented_run_count=sum(
            int(row["series"]["num_runs"]) for row in generated
        ),
    )
    return render_page._omission_summary(model, omission)


def test_omission_summary_counts_canonical_executions_and_explicit_attempt_rows() -> (
    None
):
    summary = _omission_summary_for_rows(
        [
            (run_index, attempt, 1)
            for run_index in range(1, 4)
            for attempt in range(1, 3)
        ]
    )

    assert summary == (
        "For this chaos/ptrace cell, pressure-test recorded 3 qualifying "
        "repetitions × 2 attempts (6 attempt rows). The latest repetition "
        "records outcome skipped and failure class understood prerequisite failure. "
        "No narrower cause was recorded for this evidence. No pass or product "
        "failure result was recorded for that latest execution, so this "
        "cell is excluded."
    )


def test_omission_summary_does_not_call_compressed_or_unlabeled_rows_attempts() -> None:
    summary = _omission_summary_for_rows(
        [(1, 1, 1), (1, 2, 1), (2, None, 3), (3, 1, 1)]
    )

    assert summary == (
        "For this chaos/ptrace cell, pressure-test recorded 3 qualifying "
        "repetitions across 4 qualifying ledger rows representing 6 Hermit runs. "
        "The latest repetition records outcome skipped and failure class "
        "understood prerequisite failure. No narrower cause was recorded for this "
        "evidence. No pass or product failure result was recorded for that "
        "latest execution, so this cell is excluded."
    )
    assert "attempt" not in summary


def test_omission_summary_uses_execution_for_rows_without_a_repetition_index() -> None:
    summary = _omission_summary_for_rows([(1, None, 3)], producer="validate")

    assert (
        "validate recorded 1 qualifying execution across 1 qualifying ledger row "
        "representing 3 Hermit runs" in summary
    )
    assert "The latest execution records outcome skipped" in summary
    assert "repetition" not in summary


def test_omission_summary_reports_typed_attempt_evidence_without_inference() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    model = render_page._site_model(_build(data), data, wording="fixture")
    omission = data["projection_audit"]["deliberate_omissions"][0]
    source = copy.deepcopy(model.series_by_event["event-without-result"])
    source["series"].update(
        attempt=1,
        outcome="no_result",
        failure_class="no_result",
        no_verdict_evidence={
            "attempts": [
                {
                    "attempt_outcome": "ERROR",
                    "disposition": "timeout",
                    "error_kind": "incomplete-verification-evidence",
                    "index": "seed-1",
                    "kind": "missing_report_timeout",
                    "signal": 15,
                    "status": None,
                    "timed_out": True,
                    "verification_report_sha256": None,
                }
            ],
            "evidence_sha256": "a" * 64,
        },
    )
    model = replace(
        model,
        series_by_event={**model.series_by_event, source["event_id"]: source},
        series_rows=tuple(
            source if row["event_id"] == source["event_id"] else row
            for row in model.series_rows
        ),
    )

    summary = render_page._omission_summary(model, omission)

    assert (
        "pressure-test recorded 1 qualifying repetition × 1 attempt "
        "(1 attempt row)" in summary
    )
    assert "outcome no result and failure class no result" in summary
    assert (
        "Recorded attempt evidence: attempt outcome ERROR; disposition timeout; "
        "reason missing report timeout; error kind incomplete-verification-evidence; "
        "signal 15; timed out yes." in summary
    )
    assert "No narrower cause was recorded" not in summary


def test_omission_summary_does_not_call_a_compressed_row_an_attempt_row() -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    model = render_page._site_model(_build(data), data, wording="fixture")
    omission = copy.deepcopy(data["projection_audit"]["deliberate_omissions"][0])
    template = model.series_by_event["event-without-result"]
    typed = copy.deepcopy(template)
    typed["event_id"] = "typed-attempt"
    typed["series"].update(
        attempt=1,
        num_runs=1,
        no_verdict_evidence={
            "attempts": [
                {
                    "attempt_outcome": "ERROR",
                    "disposition": "timeout",
                    "error_kind": "incomplete-verification-evidence",
                    "index": "seed-1",
                    "kind": "missing_report_timeout",
                    "signal": 15,
                    "status": None,
                    "timed_out": True,
                    "verification_report_sha256": None,
                }
            ],
            "evidence_sha256": "a" * 64,
        },
    )
    compressed = copy.deepcopy(template)
    compressed["event_id"] = "compressed-row"
    compressed["series"].update(attempt=None, num_runs=3)
    compressed["series"].pop("no_verdict_evidence", None)
    rows = (typed, compressed)
    events = dict(model.series_by_event)
    events.pop("event-without-result")
    events.update((row["event_id"], row) for row in rows)
    model = replace(
        model,
        series_by_event=events,
        series_rows=tuple(
            row
            for row in model.series_rows
            if row["event_id"] != "event-without-result"
        )
        + rows,
    )
    omission.update(
        qualifying_event_ids=sorted(row["event_id"] for row in rows),
        qualifying_physical_row_count=2,
        qualifying_represented_run_count=4,
    )

    summary = render_page._omission_summary(model, omission)

    assert (
        "1 qualifying repetition across 2 qualifying ledger rows "
        "representing 4 Hermit runs" in summary
    )
    assert "Recorded attempt evidence:" in summary
    assert (
        "No narrower cause was recorded for 1 other qualifying ledger row."
        in summary
    )
    assert "other attempt row" not in summary


def test_stage1_build_bound_covers_authentic_size_and_refuses_over_bound(
    tmp_path: Path,
) -> None:
    assert render_page.MEASURED_STAGE1_BUILD_JSON_BYTES == 1_520_395
    assert render_page.MEASURED_PUBLISHED_SITE_FILE_COUNT == 8_006
    assert (
        render_page.MAX_STAGE1_BUILD_JSON_BYTES
        >= 4 * render_page.MEASURED_STAGE1_BUILD_JSON_BYTES
    )
    root = tmp_path / "build-bound"
    root.mkdir()
    build = root / "build.json"
    build.write_bytes(b"x" * render_page.MEASURED_STAGE1_BUILD_JSON_BYTES)
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        assert (
            len(
                render_page._writer()._read_file(
                    directory, "build.json", render_page.MAX_STAGE1_BUILD_JSON_BYTES
                )
            )
            == render_page.MEASURED_STAGE1_BUILD_JSON_BYTES
        )
        build.write_bytes(b"x" * (render_page.MAX_STAGE1_BUILD_JSON_BYTES + 1))
        with pytest.raises(
            render_page._writer().WriteRefused, match="not a bounded regular file"
        ):
            render_page._writer()._read_file(
                directory, "build.json", render_page.MAX_STAGE1_BUILD_JSON_BYTES
            )
    finally:
        os.close(directory)


def test_file_url_refuses_before_fetch_and_removes_partial_preview(
    tmp_path: Path, chromium_browser: Any
) -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    root = tmp_path / "site"
    for path, value in files.items():
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(value)

    page = chromium_browser.new_page()
    page.add_init_script(
        "window.__fetchCalls = 0; const originalFetch = window.fetch; "
        "window.fetch = (...args) => { window.__fetchCalls += 1; return originalFetch(...args); };"
    )
    page.goto((root / "cells/index.html").as_uri())
    page.wait_for_function(
        "document.querySelector('[data-load-status]').textContent.includes('python3 -m http.server')"
    )

    status = page.locator("[data-load-status]").inner_text()
    assert page.evaluate("window.__fetchCalls") == 0
    assert (
        "python3 -m http.server 8000 --bind 127.0.0.1 "
        "--directory /absolute/path/to/output" in status
    )
    assert "convenience" in status.lower()
    assert "not equivalent to the Rust server" in status
    assert page.locator("#cell-results > tr").count() == 0
    assert page.locator("[data-in-manifest-listing]").evaluate_all(
        "elements => elements.every(element => element.hidden)"
    )
    assert page.locator("button, input, select").evaluate_all(
        "elements => elements.every(element => element.disabled)"
    )
    page.close()


@pytest.mark.parametrize("automatically_decode_gzip", [False, True])
def test_verified_http_detail_restores_metadata_labels_and_event_links(
    chromium_browser: Any, automatically_decode_gzip: bool
) -> None:
    data = _add_runtime_counts(data_with_all_three_states_and_one_omitted_row())
    _set_run_number(data, render_page.MAX_U64)
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    requests: list[str] = []
    page = chromium_browser.new_page()
    page.add_init_script(
        "window.__fetchOptions = []; const originalFetch = window.fetch; "
        "window.fetch = (input, options) => { "
        "window.__fetchOptions.push({...options}); return originalFetch(input, options); };"
    )
    _browser_route(
        page,
        files,
        automatically_decode_gzip=automatically_decode_gzip,
        requests=requests,
    )
    cell = data["cells"][0]
    page.goto(f"http://127.0.0.1/{model.cell_paths[cell['cell']]}")
    _wait_for_verified_data(page)

    text = page.locator("[data-detail-content]").inner_text()
    assert "Latest result\nPass" in text
    assert "State derived from qualifying evidence" not in text
    assert f"TestName\n{render_page._test_name(cell['test'], cell['category'])}" in text
    tree_href = (
        "https://github.com/rrnewton/hermit/commit/"
        f'{cell["latest_result_tree"]}'
    )
    all_tree_links = page.locator(f'a[href="{tree_href}"]')
    assert all_tree_links.count() > 1
    assert all_tree_links.evaluate_all(
        "links => links.every(link => "
        "link.classList.contains('external-link') && "
        "link.getAttribute('aria-label')?.endsWith('(external link)') && "
        "link.querySelector('.external-link-indicator[aria-hidden=\"true\"]')?.textContent === '↗')"
    )
    tree_link = page.locator("[data-detail-content]").locator(
        f'a[href="{tree_href}"]'
    )
    assert tree_link.count() == 2
    assert tree_link.locator(".external-link-indicator").all_inner_texts() == ["↗", "↗"]
    assert tree_link.evaluate_all(
        "links => links.every(link => link.getAttribute('aria-label').endsWith('(external link)'))"
    )
    assert page.locator("[data-detail-content] time").evaluate_all(
        "values => values.every(value => value.textContent.endsWith(' ET'))"
    )
    for label in (
        "Recorded evidence",
        "Qualifying physical rows",
        "Qualifying represented Hermit runs",
        "Physical rows",
        "Represented Hermit runs",
        "Lane",
        "Category",
        "Reproducer",
    ):
        assert label in text
    assert "Guest arguments" not in text
    assert "Command\n./ci/test-harness.rs --exact-cell" in text
    assert "Working directory\nNot recorded" in text
    assert page.locator(".evidence-row").count() == len(
        next(
            record
            for prefix in "0123456789abcdef"
            for record in _browser_payload(files, f"data/detail-{prefix}.json.gz")[
                "records"
            ]
            if record["kind"] == "cell" and record["identity"] == cell["cell"]
        )["evidence"]
    )
    event_anchor = render_page._fragment("event", cell["event_ids"][0])
    assert page.locator(f"#{event_anchor}").count() == 1
    assert page.locator(f"#{event_anchor} a", has_text="Raw Stage 1 runs").count() == 0
    evidence_row = page.locator(f"#{event_anchor}")
    assert (
        cell["event_ids"][0]
        not in evidence_row.locator(":scope > .evidence-context").inner_text()
    )
    assert (
        evidence_row.locator("details code", has_text=cell["event_ids"][0]).count() == 1
    )
    assert not any(
        heading.startswith("series-")
        for heading in page.locator(
            "[data-detail-content] h2, [data-detail-content] h3"
        ).all_inner_texts()
    )
    run_links = page.locator(
        "[data-detail-content] a", has_text=f"Run {render_page.MAX_U64}"
    )
    assert run_links.count() == 2
    assert set(run_links.evaluate_all("links => links.map(link => link.href)")) == {
        f"http://127.0.0.1/{model.history_paths[0]}"
    }
    assert requests.count("data/site-manifest.json.gz") == 1
    assert requests.count("data/site-summary.json.gz") == 1
    assert sum(path.startswith("data/detail-") for path in requests) == 1
    options = page.evaluate("window.__fetchOptions")
    assert len(options) == 3
    assert all("cache" not in item for item in options)
    assert all(item["credentials"] == "same-origin" for item in options)
    assert all(item["redirect"] == "error" for item in options)

    never_measured = next(
        item for item in data["cells"] if item["qualifying_physical_row_count"] == 0
    )
    assert never_measured["cell"] not in model.cell_paths
    page.close()

    page = chromium_browser.new_page()
    _browser_route(page, files, automatically_decode_gzip=automatically_decode_gzip)
    test = data["tests"][0]
    page.goto(f"http://127.0.0.1/{model.test_paths[test['id']]}")
    _wait_for_verified_data(page)
    metadata = page.locator("[data-detail-content]").inner_text()
    for label in (
        "Observation hash",
        "Occasional",
        "Preprocessors",
        "Required host capabilities",
        "Description",
        "Category",
        "Lane",
    ):
        assert label in metadata
    for expected in (
        "Recent passing execution with recorded counts",
        "does not include stdout or stderr bytes, or a public INFO-log file",
    ):
        assert expected in metadata
    count_groups = page.locator(".execution-count-group")
    assert count_groups.count() == 2
    comparison_counts = count_groups.nth(0).locator("table.execution-counts")
    activity_counts = count_groups.nth(1).locator("table.execution-counts")
    assert comparison_counts.locator("caption").inner_text() == (
        "Canonical comparison population"
    )
    assert activity_counts.locator("caption").inner_text() == (
        "Execution activity totals"
    )
    for table in (comparison_counts, activity_counts):
        assert table.locator("thead th").all_inner_texts() == [
            "Count",
            "Run 1",
            "Run 2",
        ]
        assert table.locator("tbody tr").count() == 1
    assert comparison_counts.locator("tbody tr").locator(
        "th, td"
    ).all_inner_texts() == ["INFO messages", "1", "1"]
    assert activity_counts.locator("tbody tr").locator(
        "th, td"
    ).all_inner_texts() == ["Hermit syscalls", "12", "22"]
    assert (
        "These are the INFO-message populations evaluated by the canonical comparison."
        in count_groups.nth(0).inner_text()
    )
    assert (
        "They may differ between runs and are not comparison evidence."
        in count_groups.nth(1).inner_text()
    )
    assert page.locator(".population-control").inner_text() == (
        "Include 1 never measured cells"
    )
    assert "run · ptrace" not in metadata
    include = page.locator(".population-control input")
    include.check()
    page.wait_for_function("location.search.includes('include_never_measured=true')")
    _wait_for_verified_data(page)
    never_row = page.locator(".browser-record-list li", has_text="run · ptrace")
    assert never_row.count() == 1
    assert never_row.locator("a").count() == 0
    omission_anchor = render_page._fragment(
        "omission", data["projection_audit"]["deliberate_omissions"][0]["cell"]
    )
    omission = page.locator(f"#{omission_anchor}")
    assert omission.count() == 1
    assert omission.locator("span", has_text="Recorded evidence 1").count() == 1
    assert omission.locator("a").count() == 0
    assert "1 qualifying repetition" in omission.inner_text()
    assert "outcome skipped" in omission.inner_text()
    assert "failure class understood prerequisite failure" in omission.inner_text()
    page.close()


def test_cell_detail_refuses_retired_selection_pagination_parameter(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    cell = data["cells"][0]
    digest = _digest_for_identity(cell["cell"])
    parameter = f"selection_{digest[:12]}_0_page"
    page = chromium_browser.new_page()
    _browser_route(page, files)

    page.goto(
        f"http://127.0.0.1/{model.cell_paths[cell['cell']]}&{parameter}=1"
    )
    page.wait_for_function(
        "document.querySelector('[data-load-status]').dataset.loadState === 'error'"
    )

    assert (
        f"the detail URL contains unsupported parameter {parameter}"
        in page.locator("[data-load-status]").inner_text()
    )
    assert page.locator("[data-detail-content]").inner_text() == ""
    page.close()


def test_cell_detail_selection_page_parameter_requires_rendered_records(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    cell = data["cells"][0]
    digest = _digest_for_identity(cell["cell"])
    shard_path = f"data/detail-{digest[0]}.json.gz"
    shard = _browser_payload(files, shard_path)
    record = next(item for item in shard["records"] if item["digest"] == digest)
    [path] = record["selection"]
    [template] = record["selection"][path]
    record["selection"] = {}
    _replace_browser_payload(files, shard_path, shard)

    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto(
        f"http://127.0.0.1/{model.cell_paths[cell['cell']]}"
        "&selection_records_page=1"
    )
    page.wait_for_function(
        "document.querySelector('[data-load-status]').dataset.loadState === 'error'"
    )
    assert (
        "the detail URL contains unsupported parameter selection_records_page"
        in page.locator("[data-load-status]").inner_text()
    )
    assert page.locator("[data-detail-content]").inner_text() == ""
    page.close()

    facts = []
    for run_number in range(1, render_page.LIST_PAGE_SIZE + 2):
        fact = copy.deepcopy(template)
        fact.update(
            event_ids=[f"selection-event-{run_number}"],
            observed_at=f"2026-08-31T00:{run_number - 1:02d}:00Z",
            run_id=f"selection-run-{run_number}",
            tree=f"{run_number:040x}",
        )
        fact["run"].update(
            commit=fact["tree"],
            run_id=fact["run_id"],
            run_number=str(run_number),
            selection=path,
        )
        facts.append(fact)
    record["selection"] = {path: facts}
    _replace_browser_payload(files, shard_path, shard)
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto(
        f"http://127.0.0.1/{model.cell_paths[cell['cell']]}"
        "&selection_records_page=2"
    )
    _wait_for_verified_data(page)
    assert page.locator(".selection-record").count() == 1
    assert page.locator(".selection-record a").first.inner_text() == "Run 1"
    assert page.url.endswith("&selection_records_page=2")
    page.close()


def test_cell_detail_lists_selection_records_newest_first_and_hides_argv(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    cell = data["cells"][0]
    digest = _digest_for_identity(cell["cell"])
    shard_path = f"data/detail-{digest[0]}.json.gz"
    shard = _browser_payload(files, shard_path)
    record = next(item for item in shard["records"] if item["digest"] == digest)
    [path] = record["selection"]
    [template] = record["selection"][path]
    facts = []
    for run_number, observed_at, tree in (
        (2, "2026-08-31T02:00:00Z", "2" * 40),
        (3, "2026-08-31T03:00:00Z", "3" * 40),
        (1, "2026-08-31T01:00:00Z", "1" * 40),
    ):
        fact = copy.deepcopy(template)
        fact.update(
            event_ids=[f"selection-event-{run_number}"],
            observed_at=observed_at,
            run_id=f"selection-run-{run_number}",
            tree=tree,
        )
        fact["run"].update(
            commit=tree,
            run_id=fact["run_id"],
            run_number=str(run_number),
            selection=path,
        )
        facts.append(fact)
    record["selection"][path] = facts
    record["data"]["guest_args"] = ["GUEST_ARGV_ONLY_TOKEN"]
    record["data"]["current_reproducer"]["argv"].append(
        "REPRODUCER_ARGV_ONLY_TOKEN"
    )
    record["data"]["current_reproducer"]["shell_command"] = (
        "./ci/test-harness.rs --exact-cell visible-shell-command"
    )
    _replace_browser_payload(files, shard_path, shard)
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto(f"http://127.0.0.1/{model.cell_paths[cell['cell']]}")
    _wait_for_verified_data(page)

    selection = page.locator(".selection-record")
    assert selection.count() == 3
    assert selection.locator(
        "a", has_text=re.compile(r"Run [123]$")
    ).all_inner_texts() == ["Run 3", "Run 2", "Run 1"]
    assert selection.locator("dt", has_text="Validate path").count() == 3
    assert selection.locator("dd", has_text=path).count() == 3
    assert selection.locator("time").all_inner_texts() == [
        "2026-08-30 23:00:00 ET",
        "2026-08-30 22:00:00 ET",
        "2026-08-30 21:00:00 ET",
    ]
    assert selection.locator("a.external-link").count() == 3
    text = page.locator("[data-detail-content]").inner_text()
    assert "Guest arguments" not in text
    assert "GUEST_ARGV_ONLY_TOKEN" not in text
    assert "REPRODUCER_ARGV_ONLY_TOKEN" not in text
    assert "visible-shell-command" in text
    retained = _browser_payload(files, shard_path)
    retained_record = next(
        item for item in retained["records"] if item["digest"] == digest
    )
    assert retained_record["data"]["guest_args"] == ["GUEST_ARGV_ONLY_TOKEN"]
    assert "REPRODUCER_ARGV_ONLY_TOKEN" in retained_record["data"][
        "current_reproducer"
    ]["argv"]
    page.close()


def test_test_detail_omits_argv_direct_command_but_keeps_shell_command(
    chromium_browser: Any,
) -> None:
    def rendered_text(direct: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        data = representative_build_data()
        data["tests"][0]["program"] = None
        data["tests"][0]["direct"] = direct
        data = _validated(data)
        files = render_page.render_site(_build(data), data)
        model = render_page._site_model(_build(data), data, wording="fixture")
        page = chromium_browser.new_page()
        _browser_route(page, files)
        page.goto(f"http://127.0.0.1/{model.test_paths[data['tests'][0]['id']]}")
        _wait_for_verified_data(page)
        text = page.locator("[data-detail-content]").inner_text()
        digest = _digest_for_identity(data["tests"][0]["id"])
        payload = _browser_payload(files, f"data/detail-{digest[0]}.json.gz")
        page.close()
        return text, next(
            item for item in payload["records"] if item["digest"] == digest
        )

    argv_text, argv_record = rendered_text(
        {"kind": "argv", "argv": ["/bin/echo", "ARGV_ONLY_TOKEN"]}
    )
    assert "Direct command" not in argv_text
    assert "ARGV_ONLY_TOKEN" not in argv_text
    assert argv_record["data"]["direct"] == {
        "kind": "argv",
        "argv": ["/bin/echo", "ARGV_ONLY_TOKEN"],
    }

    shell_text, shell_record = rendered_text(
        {"kind": "shell", "command": "/bin/echo readable-shell-command"}
    )
    assert "Direct command\n/bin/echo readable-shell-command" in shell_text
    assert shell_record["data"]["direct"]["command"] == (
        "/bin/echo readable-shell-command"
    )


def _many_browser_cells(files: dict[str, bytes], count: int) -> None:
    summary = _browser_payload(files, "data/site-summary.json.gz")
    original = summary["cells"][0]
    cells = []
    for index in range(count):
        cell = copy.deepcopy(original)
        identity = f"tests/hello/verify/ptrace-{index:03d}"
        cell["cell"] = identity
        cell["digest"] = _digest_for_identity(identity)
        cell["url"] = f"cells/detail.html?cell={cell['digest']}"
        cells.append(cell)
    summary["cells"] = cells
    _replace_browser_payload(files, "data/site-summary.json.gz", summary)


def test_cell_listing_uses_only_visible_dom_and_transactional_bounded_history(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    _many_browser_cells(files, 125)
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto("http://127.0.0.1/cells/index.html#all-cells")
    _wait_for_verified_data(page)

    assert page.locator("#cell-results > tr").count() == 50
    assert page.locator("[data-cell-filter-form]").get_attribute(
        "data-dependent-filter-visited-records"
    ) == str(125)
    assert page.evaluate(
        "!Object.prototype.hasOwnProperty.call(document.getElementById('cell-results'), 'compatibilityRows')"
    )
    initial_history = page.evaluate("history.length")
    page.locator("[data-cell-next]").click()
    page.wait_for_url(re.compile(r"[?&]page=2(?:&|#|$)"))
    assert page.locator("#cell-results > tr").count() == 50
    assert page.evaluate("history.length") == initial_history + 1
    page.go_back()
    page.wait_for_function("document.querySelector(\"[name='page']\").value === '1'")

    pre_search_url = page.url
    search = page.locator("[name='q']")
    search.press_sequentially("h")
    page.wait_for_function("location.search.includes('q=h')")
    search.press_sequentially("ello")
    page.wait_for_function("location.search.includes('q=hello')")
    search_url = page.url
    history_after_search = page.evaluate("history.length")
    search.press("Enter")
    assert page.url == search_url
    assert page.evaluate("history.length") == history_after_search
    page.go_back()
    page.wait_for_function("!location.search.includes('q=')")
    assert page.url == pre_search_url
    page.go_forward()
    page.wait_for_function("location.search.includes('q=hello')")
    assert page.url == search_url

    capability = page.locator("[name='host_capability']")
    presence = page.locator("[name='host_capability_present']")
    url_before_pair = page.url
    history_before_pair = page.evaluate("history.length")
    capability.select_option("cpuid-faulting")
    assert page.url == url_before_pair
    assert page.locator("#cell-filter-error").is_hidden()
    assert not presence.is_disabled()
    presence.select_option("true")
    page.wait_for_function(
        "location.search.includes('host_capability=cpuid-faulting') && "
        "location.search.includes('host_capability_present=true')"
    )
    paired_url = page.url
    page.go_back()
    page.wait_for_function(
        "!location.search.includes('host_capability=') && location.search.includes('q=hello')"
    )
    page.go_forward()
    page.wait_for_function(
        "location.search.includes('host_capability=cpuid-faulting') && "
        "location.search.includes('host_capability_present=true')"
    )
    assert page.url == paired_url
    assert page.evaluate("history.length") >= history_before_pair

    sort_heading = page.locator('[data-cell-sort="test"]')
    sort_heading.click()
    page.wait_for_function("location.search.includes('sort=test')")
    assert sort_heading.locator(".sort-indicator").inner_text() == "▲"
    assert sort_heading.locator("xpath=..").get_attribute("aria-sort") == "ascending"
    sort_heading.click()
    page.wait_for_function("location.search.includes('direction=desc')")
    assert sort_heading.locator(".sort-indicator").inner_text() == "▼"
    assert sort_heading.locator("xpath=..").get_attribute("aria-sort") == "descending"
    before_bounded = page.evaluate("history.length")
    for _index in range(30):
        sort_heading.click()
    assert 1 <= page.evaluate("history.length") - before_bounded <= 24
    assert page.locator("#cell-results > tr").count() <= 50
    page.close()


def test_verified_payload_yields_a_loading_macrotask_before_dom_initialization(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    _many_browser_cells(files, 125)
    page = chromium_browser.new_page()
    page.add_init_script(
        "window.__postVerifiedParseState = null; "
        "const originalParse = JSON.parse.bind(JSON); "
        "JSON.parse = (...args) => { "
        "const value = originalParse(...args); "
        "if (value && value.schema_version === 1 && Array.isArray(value.cells)) { "
        "const channel = new MessageChannel(); "
        "channel.port1.onmessage = () => { "
        "const form = document.querySelector('[data-cell-filter-form]'); "
        "const status = document.querySelector('[data-load-status]'); "
        "window.__postVerifiedParseState = { "
        "formHidden: form.hidden, "
        "statusBusy: status.getAttribute('aria-busy'), "
        "statusText: status.textContent, "
        "visibleRows: document.querySelectorAll('[data-cell-row=\"1\"]:not([hidden])').length "
        "}; channel.port1.close(); channel.port2.close(); }; "
        "channel.port2.postMessage(null); } "
        "return value; };"
    )
    _browser_route(page, files)
    page.goto("http://127.0.0.1/cells/index.html#all-cells")
    page.wait_for_function(
        "window.__postVerifiedParseState !== null && "
        "document.querySelector('[data-load-status]').textContent !== "
        "'Loading verified local data…'"
    )

    assert page.evaluate("window.__postVerifiedParseState") == {
        "formHidden": True,
        "statusBusy": "true",
        "statusText": "Loading verified local data…",
        "visibleRows": 1,
    }
    _wait_for_verified_data(page)
    assert page.locator("[data-cell-filter-form]").is_visible()
    page.close()


def test_latest_result_filter_replaces_redundant_state_filter(
    chromium_browser: Any,
) -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    summary = _browser_payload(files, "data/site-summary.json.gz")
    second = summary["cells"][1]
    second["qualifying_physical_row_count"] = 1
    second["qualifying_represented_run_count"] = 1
    second["latest_result"] = "product_failure"
    second["latest_result_time"] = "2025-01-02T00:00:00Z"
    second["latest_result_tree"] = "2" * 40
    second["url"] = f"cells/detail.html?cell={second['digest']}"
    _replace_browser_payload(files, "data/site-summary.json.gz", summary)

    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto("http://127.0.0.1/cells/index.html#all-cells")
    _wait_for_verified_data(page)
    result = page.locator("[name='result']")

    assert page.locator("select[name='state']").count() == 0
    assert page.locator("input[name='state'][type='hidden']").count() == 1
    result.select_option("product_failure")
    page.wait_for_function("location.search.includes('result=product_failure')")
    assert page.locator("#cell-results > tr").count() == 1
    assert page.locator("#cell-results > tr").get_attribute("data-state") == "red"
    assert "Fail" in page.locator("#cell-results > tr").inner_text()
    page.close()


def test_detail_pagination_uses_namespaced_queries_and_evidence_only_fragments(
    chromium_browser: Any,
) -> None:
    data = data_with_all_three_states_and_one_omitted_row()
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    _many_browser_cells(files, 51)
    summary = _browser_payload(files, "data/site-summary.json.gz")
    omission = copy.deepcopy(summary["projection_audit"]["deliberate_omissions"][0])
    browser_omission = copy.deepcopy(summary["omissions"][0])
    omissions = []
    browser_omissions = []
    for index in range(51):
        item = copy.deepcopy(omission)
        item["cell"] = f"tests/hello/chaos/ptrace-{index:03d}"
        item["event_references"][0]["event_id"] = f"omitted-event-{index:03d}"
        omissions.append(item)
        browser_item = copy.deepcopy(browser_omission)
        browser_item["cell"] = item["cell"]
        browser_item["anchor"] = render_page._fragment("omission", item["cell"])
        browser_item["event_references"][0]["event_id"] = item["event_references"][0][
            "event_id"
        ]
        browser_item["event_references"][0]["anchor"] = render_page._fragment(
            "event", item["event_references"][0]["event_id"]
        )
        browser_omissions.append(browser_item)
    summary["projection_audit"]["deliberate_omissions"] = omissions
    summary["omissions"] = browser_omissions
    _replace_browser_payload(files, "data/site-summary.json.gz", summary)

    page = chromium_browser.new_page()
    _browser_route(page, files)
    test_url = model.test_paths[data["tests"][0]["id"]]
    page.goto(
        f"http://127.0.0.1/{test_url}" "&published_cells_page=2&omitted_cells_page=2"
    )
    _wait_for_verified_data(page)
    lists = page.locator(".browser-record-list")
    assert lists.nth(0).locator(":scope > li").count() == 1
    assert lists.nth(1).locator(":scope > li").count() == 1
    assert page.url.endswith("&published_cells_page=2&omitted_cells_page=2")
    page.reload()
    _wait_for_verified_data(page)
    assert lists.nth(0).locator(":scope > li").count() == 1
    assert lists.nth(1).locator(":scope > li").count() == 1

    page.locator("nav[aria-label='published cells pages']").get_by_text(
        "Previous"
    ).click()
    page.wait_for_function(
        "!location.search.includes('published_cells_page=') && "
        "location.search.includes('omitted_cells_page=2')"
    )
    assert lists.nth(0).locator(":scope > li").count() == 50
    assert lists.nth(1).locator(":scope > li").count() == 1
    page.locator("nav[aria-label='omitted cells pages']").get_by_text(
        "Previous"
    ).click()
    page.wait_for_function(
        "!location.search.includes('published_cells_page=') && "
        "!location.search.includes('omitted_cells_page=')"
    )
    assert lists.nth(0).locator(":scope > li").count() == 50
    assert lists.nth(1).locator(":scope > li").count() == 50
    page.go_back()
    page.wait_for_function("location.search.includes('omitted_cells_page=2')")
    assert lists.nth(0).locator(":scope > li").count() == 50
    assert lists.nth(1).locator(":scope > li").count() == 1
    page.go_back()
    page.wait_for_function("location.search.includes('published_cells_page=2')")
    assert lists.nth(0).locator(":scope > li").count() == 1
    assert lists.nth(1).locator(":scope > li").count() == 1
    page.go_forward()
    page.wait_for_function("!location.search.includes('published_cells_page=')")
    assert lists.nth(0).locator(":scope > li").count() == 50
    assert lists.nth(1).locator(":scope > li").count() == 1

    final_event = browser_omissions[-1]["event_references"][0]["anchor"]
    page.goto(f"http://127.0.0.1/{test_url}#{final_event}")
    _wait_for_verified_data(page)
    assert page.locator(f"#{final_event}").is_visible()
    assert lists.nth(1).locator(":scope > li").count() == 1
    assert "_page=" not in page.url
    assert page.url.endswith(f"#{final_event}")
    page.close()


def test_tests_have_no_controls_and_run_controls_preserve_route_state(
    chromium_browser: Any,
) -> None:
    data = representative_build_data()
    files = render_page.render_site(_build(data), data)
    page = chromium_browser.new_page()
    _browser_route(page, files)
    page.goto("http://127.0.0.1/index.html#tests")

    assert page.locator("form[data-list-controls='tests']").count() == 0
    assert page.locator("form[data-list-controls='validation-runs']").count() == 0
    assert page.locator("#tests .test-category").count() == 1
    assert page.locator("#tests .test-name-list a").count() == len(data["tests"])

    page.goto("http://127.0.0.1/runs/index.html")
    runs = page.locator("form[data-list-controls='validation-runs']")
    assert page.locator("form[data-list-controls='tests']").count() == 0
    runs.locator("[name='result']").select_option("pass")
    page.wait_for_function("location.search.includes('runs_result=pass')")
    assert "tests_" not in page.url
    assert runs.locator("[name='result']").input_value() == "pass"
    page.reload()
    assert runs.locator("[name='result']").input_value() == "pass"
    page.close()


def test_moved_static_collections_work_from_file_urls_without_fetch(
    tmp_path: Path, chromium_browser: Any
) -> None:
    data = representative_build_data()
    data["runs"].update(
        projection_excluded_physical_row_count=1,
        projection_excluded_represented_run_count=2,
        projection_exclusion_counts=[
            {
                "physical_row_count": 1,
                "reason": "projection rejected the row",
                "represented_run_count": 2,
                "schema": "stress-series/v1",
            }
        ],
    )
    data = _validated(data)
    build = _build(data)
    model = render_page._site_model(build, data, wording="fixture")
    files = render_page.render_site(build, data)
    command = {
        "cell": "applications/custom/verify/ptrace",
        "current_reproducer": None,
        "not_applicable_reason": None,
        "test": "applications/custom",
    }
    files["tests/custom-commands.html"] = render_page._custom_commands_page(
        replace(model, custom_commands=(command,))
    )
    root = tmp_path / "static-pages"
    for path, value in files.items():
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(value)

    errors: list[str] = []
    page = chromium_browser.new_page()
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.on(
        "console",
        lambda message: errors.append(message.text)
        if message.type == "error"
        else None,
    )
    page.add_init_script(
        "window.__fetchCalls = 0; const originalFetch = window.fetch; "
        "window.fetch = (...args) => { window.__fetchCalls += 1; return originalFetch(...args); };"
    )
    cases = (
        (
            "runs/index.html",
            "Validation runs",
            "validation-runs",
            "runs_result=pass",
            "result",
            "pass",
        ),
        (
            "cells/projection-audit.html",
            "Projection audit",
            "projection-audit",
            "projection_sort=reason&projection_direction=desc",
            "sort",
            "reason",
        ),
        (
            "tests/custom-commands.html",
            "Custom commands selected by full",
            "custom-commands",
            "commands_sort=test&commands_direction=desc",
            "sort",
            "test",
        ),
    )
    for path, heading, collection, query, field, expected in cases:
        page.goto(f"{(root / path).as_uri()}?{query}")
        controls = page.locator(f'[data-list-controls="{collection}"]')
        assert page.locator("h1").inner_text() == heading
        assert controls.locator(f'[name="{field}"]').input_value() == expected
        assert page.evaluate("window.__fetchCalls") == 0
        page.reload()
        assert controls.locator(f'[name="{field}"]').input_value() == expected
        assert page.evaluate("window.__fetchCalls") == 0
    assert errors == []
    page.close()


def test_renderer_removes_retired_private_detail_helpers() -> None:
    for name in (
        "_cell_page",
        "_test_page",
        "_optional_fact",
        "_selection_run_path",
        "_selection_run_reference",
        "_selection_details",
        "_series_result",
        "_series_row",
        "_filtered_count_link",
    ):
        assert not hasattr(render_page, name)


def _cell_with_many_evidence_rows(count: int) -> tuple[dict[str, Any], dict[str, Any]]:
    """Give one cell enough recorded evidence to push a late occurrence off-screen.

    The defect only appears once the requested row is BELOW THE FOLD, and on the
    real site it also sat on a second page. A two-row fixture cell cannot show
    either, because the target is already in view and `scrollY == 0` is then the
    correct answer rather than the bug.
    """
    data = representative_build_data()
    cell = data["cells"][0]
    template = copy.deepcopy(data["runs"]["series_rows"][0])
    for index in range(count):
        row = copy.deepcopy(template)
        event_id = f"event-bulk-{index:04d}"
        tree = f"{index:040d}"
        row.update(
            # ASCENDING emitted_at, deliberately. Without distinct increasing
            # stamps every row ties and a reverse-order assertion is vacuous --
            # it holds whether or not anything sorts. This makes the INPUT
            # chronological ascending, which is the condition being fixed.
            emitted_at=f"2026-09-{1 + index // 24:02d}T{index % 24:02d}:00:00Z",
            event_id=event_id,
            producer="pressure-test",
            product_result=None,
            run_id=f"bulk-run-{index:04d}",
        )
        row["series"].update(
            failure_class="understood_prerequisite_failure",
            outcome="skipped",
            result=None,
            tree=tree,
        )
        row.pop("corrects", None)
        row["current_main_ancestry"] = False
        data["runs"]["series_rows"].append(row)
        data["runs"]["product_results"][event_id] = None
        data["runs"]["projection_results"][event_id] = None
        data["runs"]["current_main_ancestry"][tree] = False
        cell["event_ids"].append(event_id)
        cell["physical_row_count"] += 1
        cell["represented_run_count"] += 1
        for field in (
            "projection_admitted_physical_row_count",
            "projection_admitted_represented_run_count",
            "series_physical_row_count",
            "series_represented_run_count",
        ):
            data["runs"][field] += 1
        data["counts"]["physical_row_count"] += 1
        data["counts"]["represented_run_count"] += 1
        data["provenance"]["series_physical_row_count"] += 1
        data["provenance"]["series_represented_run_count"] += 1
    _refresh_projection_digests(data)
    return _validated(data), cell


def test_recorded_evidence_fragment_scrolls_the_requested_occurrence_into_view(
    chromium_browser: Any,
) -> None:
    """A shared occurrence link must land ON the row, not merely on its page.

    Measured on the published 2026-09-11 site before this fix: loading a cell URL
    whose only state was an `event-...` fragment left the document at scrollY 0
    with the requested row at y=8904. The fragment chose the right page and the
    row existed; the reader was simply not taken to it. The browser performs its
    own fragment scroll while the document is still the server-rendered shell,
    before this list has drawn the row, and never retries.
    """
    data, cell = _cell_with_many_evidence_rows(60)
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")
    target_event = "event-bulk-0059"
    fragment = render_page._fragment("event", target_event)

    page = chromium_browser.new_page(viewport={"width": 1440, "height": 900})
    _browser_route(page, files)
    page.goto(f"http://127.0.0.1/{model.cell_paths[cell['cell']]}#{fragment}")
    _wait_for_verified_data(page)
    page.wait_for_selector(f"#{fragment}")

    measured = page.evaluate(
        "fragment => {"
        "  const target = document.getElementById(fragment);"
        "  const box = target.getBoundingClientRect();"
        "  return {"
        "    documentTop: Math.round(box.top + window.scrollY),"
        "    scrollY: Math.round(window.scrollY),"
        "    viewportTop: Math.round(box.top),"
        "    viewportHeight: window.innerHeight,"
        "  };"
        "}",
        fragment,
    )
    page.close()

    # The row must genuinely be far down the document, or this test would pass
    # against the defect for the wrong reason.
    assert measured["documentTop"] > measured["viewportHeight"]
    assert measured["scrollY"] > 0, "the occurrence was never scrolled into view"
    assert 0 <= measured["viewportTop"] < measured["viewportHeight"]


def test_a_fragment_for_another_list_does_not_move_this_one(
    chromium_browser: Any,
) -> None:
    """Detail pages carry several independent paged lists.

    A fragment naming a row none of this list's records own must leave it alone,
    otherwise adding the scroll above would make unrelated lists fight over one
    fragment.
    """
    data, cell = _cell_with_many_evidence_rows(60)
    files = render_page.render_site(_build(data), data)
    model = render_page._site_model(_build(data), data, wording="fixture")

    page = chromium_browser.new_page(viewport={"width": 1440, "height": 900})
    _browser_route(page, files)
    page.goto(
        f"http://127.0.0.1/{model.cell_paths[cell['cell']]}"
        "#event-this-identity-is-not-in-any-record"
    )
    _wait_for_verified_data(page)
    scroll_y = page.evaluate("() => Math.round(window.scrollY)")
    page.close()

    assert scroll_y == 0


def test_ordinary_words_are_not_broken_mid_word(chromium_browser: Any) -> None:
    """`overflow-wrap: anywhere` on body split ordinary words, not just digests.

    It also shrinks a box's min-content contribution, which is why the damage
    showed up in table headings: the column was free to squeeze below the width
    of its own longest word. Digest-bearing elements keep `anywhere`.
    """
    data = representative_build_data()
    data = _validated(data)
    files = render_page.render_site(_build(data), data)

    page = chromium_browser.new_page(viewport={"width": 390, "height": 844})
    _browser_route(page, files)
    page.goto("http://127.0.0.1/runs/index.html")
    page.wait_for_selector(".validation-runs-table tbody td")
    styles = page.evaluate(
        "() => ({"
        "  body: getComputedStyle(document.body).overflowWrap,"
        "  runHeading: getComputedStyle("
        "    document.querySelector('.validation-runs-table thead th')).overflowWrap,"
        "  runCell: getComputedStyle("
        "    document.querySelector('.validation-runs-table tbody td')).overflowWrap,"
        "})"
    )
    page.close()

    assert styles["body"] == "break-word"
    assert styles["runHeading"] == "break-word"
    # Body cells still carry record ids and commit digests and must stay breakable.
    assert styles["runCell"] == "anywhere"


def test_cells_table_fits_a_narrow_viewport_and_the_run_table_keeps_its_fixed_layout(
    chromium_browser: Any,
) -> None:
    """Measured at 390x844 before this fix: 1856px of cells table inside a 322px
    scroller, 5.76 viewport widths of horizontal travel to relate an identity to
    its result.

    The validation-run table is deliberately NOT included. Its fixed layout and
    overflowing scroller are asserted elsewhere in this file at a 600px viewport,
    so releasing its width would mean editing those tests to match the code.
    """
    data = representative_build_data()
    data = _validated(data)
    files = render_page.render_site(_build(data), data)

    page = chromium_browser.new_page(viewport={"width": 390, "height": 844})
    _browser_route(page, files)
    page.goto("http://127.0.0.1/cells/index.html")
    _wait_for_verified_data(page)
    # DIFFERENTIAL, not an absolute bar. Ten columns of cell identity cannot be
    # made to fit 322px without dropping data, so an absolute threshold here
    # would either be unreachable or be whatever number happened to pass. This
    # measures the same page twice -- once as shipped, once with the narrow-width
    # rule defeated -- so it proves the rule is doing the work rather than
    # asserting a figure chosen after the fact.
    ratio = (
        "() => {"
        "  const scroll = document.querySelector('#all-cells .table-scroll');"
        "  return scroll.scrollWidth / Math.max(1, scroll.clientWidth);"
        "}"
    )
    cells_ratio = page.evaluate(ratio)
    page.add_style_tag(
        content=(
            "@media (max-width: 48rem) {"
            "  .table-scroll table:not(.validation-runs-table) {"
            "    width: max-content;"
            "  }"
            "}"
        )
    )
    unfixed_ratio = page.evaluate(ratio)
    page.goto("http://127.0.0.1/runs/index.html")
    page.wait_for_selector(".validation-runs-table")
    run_layout = page.evaluate(
        "() => getComputedStyle("
        "document.querySelector('.validation-runs-table')).tableLayout"
    )
    page.close()

    assert unfixed_ratio > cells_ratio, "the narrow-width rule changed nothing"
    assert cells_ratio <= unfixed_ratio * 0.75, (
        "the narrow-width rule should remove a substantial part of the "
        f"horizontal travel; measured {cells_ratio:.2f} against {unfixed_ratio:.2f}"
    )
    # Honest bound: this REDUCES the sideways travel, it does not remove it. Ten
    # columns still do not fit a phone, and the residual is reported as an
    # unfixed part of the review finding rather than hidden behind a passing
    # threshold.
    assert cells_ratio > 1.0
    assert run_layout == "fixed"


def test_landing_introduction_separates_a_dated_snapshot_from_current_progress() -> None:
    """The review found a reader could read snapshot counts as current progress,
    and could read the manifest as the denominator for red."""
    landing = render_page._landing_introduction()

    assert "it is not a live status view" in landing
    assert "single dated measurement run" in landing
    assert "Backend work that landed after that run is not represented" in landing
    assert "Green and red are counted only over cells this run actually measured" in landing
    assert "never measured" in landing
    # The old vocabulary must not come back with the new prose.
    assert "enabled" not in landing.lower()


def test_cell_identity_heading_breaks_only_at_its_separators(
    chromium_browser: Any,
) -> None:
    """Measured on the built site at 390x844 before this fix: the heading
    rendered as "applications/git-" / "repository-" / "workflow/verify/p" /
    "trace", splitting `ptrace` across two lines.

    Note the browser does NOT reliably break at a slash on its own -- it kept
    "workflow/verify/p" together and broke inside the next word -- which is the
    whole reason the joints have to be stated rather than hoped for.

    Driven through set_content with a realistic long identity rather than the
    rendered fixture page, because the fixture's only cell is
    `tests/hello/verify/ptrace`, 25 characters, which fits without ever breaking
    mid-word. An earlier version of this test used that page and PASSED WITH THE
    FIX REVERTED; it measured nothing. The control is asserted here instead: the
    same identity, same width, same stylesheet, with and without the <wbr>
    joints.
    """
    identity = "applications/git-repository-workflow/verify/ptrace"
    stylesheet = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.css"
    ).read_text()
    javascript = (
        REPOSITORY / "ci-hub/compatibility-website/assets/site.js"
    ).read_text()

    def lines_for(with_joints: bool) -> list[str]:
        page = chromium_browser.new_page(viewport={"width": 390, "height": 844})
        page.set_content(
            "<!doctype html><html><head>"
            f"<style>{stylesheet}</style></head>"
            "<body data-page-kind='heading-test'>"
            "<main><header class='page-heading'><h1 id='identity'></h1>"
            "</header></main>"
            f"<script>{javascript}</script></body></html>"
        )
        page.evaluate(
            """([identity, withJoints]) => {
              const heading = document.getElementById('identity');
              if (!withJoints) {
                heading.textContent = identity;
                return;
              }
              const segments = identity.split('/');
              segments.forEach((segment, index) => {
                const last = index === segments.length - 1;
                heading.appendChild(
                  document.createTextNode(last ? segment : segment + '/'));
                if (!last) { heading.appendChild(document.createElement('wbr')); }
              });
            }""",
            [identity, with_joints],
        )
        rows = page.evaluate(
            """() => {
              const heading = document.getElementById('identity');
              const walker = document.createTreeWalker(heading, NodeFilter.SHOW_TEXT);
              const out = [];
              let current = '';
              let lastTop = null;
              let node;
              while ((node = walker.nextNode())) {
                for (let index = 0; index < node.textContent.length; index += 1) {
                  const range = document.createRange();
                  range.setStart(node, index);
                  range.setEnd(node, index + 1);
                  const top = range.getBoundingClientRect().top;
                  if (lastTop !== null && Math.abs(top - lastTop) > 2) {
                    out.push(current);
                    current = '';
                  }
                  current += node.textContent[index];
                  lastTop = top;
                }
              }
              if (current) { out.push(current); }
              return out;
            }"""
        )
        page.close()
        return rows

    def splits_a_word(rows: list[str]) -> bool:
        return any(
            not (line.endswith("/") or line.endswith("-")) for line in rows[:-1]
        )

    without_joints = lines_for(False)
    with_joints = lines_for(True)

    assert len(with_joints) > 1, "meaningless unless the heading wrapped"
    # The control: this identity really does split a word when left to CSS.
    assert splits_a_word(without_joints), (
        "the control did not reproduce the defect, so the assertion below "
        f"proves nothing; measured {without_joints}"
    )
    assert not splits_a_word(with_joints), (
        f"the identity still breaks inside a word: {with_joints}"
    )


def test_cell_detail_heading_inserts_the_joints_through_the_product(
    chromium_browser: Any,
) -> None:
    """Assert the SITE inserts the break opportunities, not that Chromium honours
    them.

    The behaviour test above builds its own <wbr> DOM inside `page.evaluate`, so
    it demonstrates that Chromium breaks at a <wbr> and nothing about whether the
    renderer emits any: reverting only `site.js`, which deletes `identityNode`
    outright, still leaves it green. This drives the real cell detail page and
    counts the joints the product actually produced, which is the half that was
    previously carried only by a source-string grep.
    """
    data = _validated(representative_build_data())
    files = render_page.render_site(_build(data), data)
    cell = data["cells"][0]
    model = render_page._site_model(_build(data), data, wording="fixture")

    page = chromium_browser.new_page(viewport={"width": 390, "height": 844})
    _browser_route(page, files)
    page.goto(f"http://127.0.0.1/{model.cell_paths[cell['cell']]}")
    _wait_for_verified_data(page)
    page.wait_for_selector("h1")
    observed = page.evaluate(
        """() => {
          const heading = document.querySelector('h1');
          return {
            text: heading.textContent,
            joints: heading.querySelectorAll('wbr').length,
          };
        }"""
    )
    page.close()

    identity = str(cell["cell"])
    assert observed["text"] == identity
    # One joint per separator, and the identity really does have separators.
    assert identity.count("/") > 0
    assert observed["joints"] == identity.count("/"), (
        f"heading carries {observed['joints']} break opportunities for "
        f"{identity.count('/')} separators in {identity!r}"
    )


class _ReadingModel:
    """Minimal stand-in carrying only what `_result_reading` reads.

    The branch differential has to be driven from something with red cells, and
    the real fixtures cannot supply one cheaply: a red cell needs its own series
    row, its own validate-run selection and its own share of the physical-row
    partition, and the reducer refuses every shortcut. Driving the branches from
    a stub keeps the differential honest; the test below it runs the same
    function against a real rendered model so the stub cannot drift unnoticed.
    """

    def __init__(self, cells, in_manifest):
        self.cells = cells
        states = Counter(cell["state"] for cell in cells)
        self.data = {
            "counts": {
                "green_cell_count": states["green"],
                "in_manifest_cell_count": in_manifest,
                "never_measured_cell_count": states["never-measured"],
                "red_cell_count": states["red"],
            }
        }


def _reading_cell(state, selected_by_full=False):
    return {
        "state": state,
        "latest_result": {"green": "pass", "red": "product_failure"}.get(state),
        "qualifying_physical_row_count": 0 if state == "never-measured" else 1,
        "selected_by_full": selected_by_full,
    }


def test_overview_reading_note_names_every_population_and_they_sum_to_the_manifest() -> None:
    """Run against a REAL rendered model that has an omission.

    The one-cell fixture closes any arithmetic trivially, which is exactly how
    the earlier version of this paragraph shipped saying "the remaining 4895"
    when 5744 - 835 is 4909. This fixture has 3 in the manifest, 2 cell rows and
    1 deliberate omission, so a paragraph that swallows the omission cannot add
    up here either.
    """
    data = _validated(data_with_all_three_states_and_one_omitted_row())
    model = render_page._site_model(_build(data), data, wording="fixture")
    in_manifest = data["counts"]["in_manifest_cell_count"]
    assert in_manifest - len(model.cells) > 0, "fixture must contain an omission"

    reading = render_page._result_reading(model)
    text = re.sub(r"<[^>]+>", " ", reading)

    stated = [int(value) for value in re.findall(r"\b(\d+)\b", text)]
    assert in_manifest in stated
    # Every population named after "Of the N cells in the manifest," must sum to N.
    tail = text.split(f"Of the {in_manifest} cells in the manifest,", 1)[1]
    parts = [int(value) for value in re.findall(r"\b(\d+)\b", tail.split(".")[0])]
    assert sum(parts) == in_manifest, (
        f"named populations {parts} sum to {sum(parts)}, not {in_manifest}: {tail!r}"
    )
    assert "understate the rate" in text
    assert "not a backlog" in text
    assert "enabled" not in text.lower()


def test_reading_note_refuses_when_the_arithmetic_does_not_close() -> None:
    """Fail closed. A paragraph that exists to fix a denominator must not ship a
    wrong one quietly, so the populations are checked rather than trusted."""
    cells = [_reading_cell("green"), _reading_cell("red")]
    with pytest.raises(render_page.RenderRefused):
        # 2 cell rows against an in-manifest total of 1 cannot be partitioned.
        render_page._result_reading(_ReadingModel(cells, 1))


@pytest.mark.parametrize(
    "selected,expected,forbidden",
    [
        ((False, False), "None of the 2 red cells", "All 2 red cells"),
        ((True, True), "All 2 red cells", "retained from an earlier run"),
        ((True, False), "1 of the 2 red cells", "None of the 2 red cells"),
    ],
)
def test_red_cell_sentence_follows_the_data_rather_than_a_fixed_string(
    selected, expected, forbidden
) -> None:
    """The claim "these are old failures" is only true while it is true.

    A fixed sentence keeps asserting it after a run starts failing cells the full
    path does select, which is the reassuring direction to be wrong in. Each
    branch is driven and the other branches' wording is asserted ABSENT, so
    replacing the block with any single literal fails at least one case.
    """
    cells = [
        _reading_cell("green"),
        _reading_cell("never-measured"),
        *(_reading_cell("red", selected_by_full=value) for value in selected),
    ]
    reading = render_page._result_reading(_ReadingModel(cells, len(cells)))
    assert expected in reading
    assert forbidden not in reading


def test_red_cell_sentence_handles_a_snapshot_with_no_failures() -> None:
    reading = render_page._result_reading(
        _ReadingModel([_reading_cell("green")], 1)
    )
    assert "No cell in this snapshot carries a recorded failure." in reading



def test_every_time_ordered_list_defaults_to_newest_first() -> None:
    """Owner directive 2026-09-16: reverse chronological everywhere, by default.

    Covers all three instances in ONE test because they are one requirement in
    two mechanisms: the cell-detail evidence list has no sort control, so its
    order is the order the generator emits, while the two run-page lists are
    governed by a declared `default_direction`. A test touching only one
    mechanism would pass while the other stayed ascending.
    """
    data = _validated(data_with_all_three_states_and_one_omitted_row())
    files = render_page.render_site(_build(data), data)

    # 1. The run-page lists. One helper call emits both sections, so assert the
    #    declared default on each rendered section rather than on the call.
    run_pages = [p for p in files if p.startswith("runs/") and p.endswith(".html")]
    assert run_pages, "fixture must render at least one run page"
    checked = 0
    for path in run_pages:
        page = files[path].decode()
        for collection in ("recorded-cells", "recorded-not-selected"):
            marker = f'data-list-controls="{collection}"'
            index = page.find(marker)
            if index < 0:
                continue
            form = page[index : page.find(">", index)]
            assert 'data-default-sort="recorded"' in form, collection
            assert 'data-default-direction="desc"' in form, (
                f"{collection} on {path} still defaults to ascending; "
                "`recorded` resolves to an ordinal, which is a time order"
            )
            checked += 1
    assert checked, "no run-page recorded list was rendered, so this proved nothing"

    # 2. The cell-detail evidence list, which has no control at all.
    #    Driven from a cell with MANY rows carrying DISTINCT ASCENDING stamps.
    #    The three-state fixture's cell has exactly one evidence row, so the
    #    reverse-order assertion held against it whether or not anything sorted;
    #    the control caught that and this is the repair.
    bulk_data, bulk_cell = _cell_with_many_evidence_rows(8)
    bulk_files = render_page.render_site(_build(bulk_data), bulk_data)
    digest = _digest_for_identity(bulk_cell["cell"])
    shard = _browser_payload(bulk_files, f"data/detail-{digest[0]}.json.gz")
    record = next(item for item in shard["records"] if item["digest"] == digest)
    stamps = [item["emitted_at"] for item in record["evidence"]]
    assert len(set(stamps)) > 1, (
        "evidence stamps all tie, so ordering cannot be observed and this "
        f"assertion would prove nothing: {stamps}"
    )
    assert stamps == sorted(stamps, reverse=True), (
        f"recorded evidence is not newest first: {stamps}"
    )


def test_recorded_evidence_order_is_total_so_the_build_stays_reproducible() -> None:
    """Ties must not leave the order to chance.

    The artifact identity is a digest over the emitted bytes, so a comparison
    that treats two rows as equal would let the same input produce two different
    artifacts. The tiebreak on event_id is what keeps it total.
    """
    data = _validated(data_with_all_three_states_and_one_omitted_row())
    build = _build(data)
    first = render_page.render_site(build, data)
    second = render_page.render_site(build, data)
    cell = data["cells"][0]
    digest = _digest_for_identity(cell["cell"])
    path = f"data/detail-{digest[0]}.json.gz"

    def order(files):
        record = next(
            item
            for item in _browser_payload(files, path)["records"]
            if item["digest"] == digest
        )
        return [(i["emitted_at"], i["event_id"]) for i in record["evidence"]]

    assert order(first) == order(second)
    keys = order(first)
    assert keys == sorted(keys, reverse=True)
