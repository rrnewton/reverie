from __future__ import annotations

import copy
import importlib.util
import json
import os
import posixpath
import re
import subprocess
import sys
from collections import Counter
from html.parser import HTMLParser
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/render_page.py"
STAGE1_TEST = REPOSITORY / "ci-hub/tests/test_compatibility_website_build_data.py"
FIXTURE = REPOSITORY / "ci-hub/tests/fixtures/compatibility-website-stage-1"
SITE_JS = REPOSITORY / "ci-hub/compatibility-website/assets/site.js"


def load_module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


render_page = load_module("compatibility_website_render_page_test", SCRIPT)
stage1 = load_module("compatibility_website_stage1_fixture_source", STAGE1_TEST)


GREEN = "fixture/green/verify/ptrace"
RED = "fixture/red/verify/kvm"
NEVER = "fixture/never-measured/verify/dbt"
UNAVAILABLE = "fixture/unavailable/verify/ptrace"
HISTORICAL = "fixture/historical/verify/ptrace"
CUSTOM = "fixture/custom-command/custom/ptrace"
CELLS = (GREEN, RED, NEVER, UNAVAILABLE, HISTORICAL)


def _validation_row(
    commit: str,
    *,
    run_id: str,
    profile: str,
    cell: str,
    record_id: str,
    result: str,
    minute: int,
    diverged: bool = False,
) -> dict[str, Any]:
    row = stage1.validation_row(
        commit,
        run_id=run_id,
        profile=profile,
        selected=[cell],
        record_id=record_id,
    )
    row.update(
        started_at=f"2026-08-31T00:0{minute}:00Z",
        finished_at=f"2026-08-31T00:0{minute}:30Z",
        result=result,
    )
    if diverged:
        verdict = row["cell_results"]["cells"][0]["cell_verdict"]
        verdict["state"] = "compared-and-diverged"
        verdict["bitwise_parity"] = False
    return row


def representative_build_data(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> dict[str, Any]:
    monkeypatch.setenv("GIT_AUTHOR_DATE", "2026-08-31T00:00:00Z")
    monkeypatch.setenv("GIT_COMMITTER_DATE", "2026-08-31T00:00:00Z")
    parent, parent_commit, hermit, hermit_commit = stage1.make_checkouts(tmp_path)
    manifest = stage1.manifest_metadata(*CELLS, CUSTOM)
    manifest["selected_by_full_custom_commands"] = [
        cell for cell in manifest["cells"] if cell["mode"] == "custom"
    ]
    manifest["cells"] = [cell for cell in manifest["cells"] if cell["mode"] != "custom"]
    for cell in manifest["cells"]:
        if cell["test"] in {"fixture/never-measured", "fixture/historical"}:
            cell["selected_by_full"] = False
            cell["not_selected_by_full_reason"] = {
                "reason": "not selected by full in this fixture"
            }

    rows = [
        stage1.series_row(
            "green-retry-1",
            hermit_commit,
            run_id="fixture-super-current",
            cell=GREEN,
            run_index=1,
            outcome="diverged",
            result="determinism-failure",
            failure_class="product_failure",
        ),
        stage1.series_row(
            "green-retry-2",
            hermit_commit,
            run_id="fixture-super-current",
            cell=GREEN,
            run_index=2,
            outcome="passed",
            result="pass",
        ),
        stage1.series_row(
            "red-attempt",
            hermit_commit,
            run_id="fixture-quick-current",
            cell=RED,
            outcome="diverged",
            result="determinism-failure",
            failure_class="product_failure",
        ),
        stage1.series_row(
            "historical-pressure",
            hermit_commit,
            producer="pressure-test",
            run_id="fixture-pressure-history",
            cell=HISTORICAL,
            num_runs=4,
            outcome="passed",
            result="pass",
        ),
    ]
    rows[0]["series"].update(
        attempt=1,
        first_divergent_messages={
            "left": "source-only fixture message from run 1",
            "right": "source-only fixture message from run 2",
        },
        main_ancestry=False,
    )
    rows[1]["series"].update(
        attempt=2,
        runtime={
            "run1": {
                "scheduler_turns": 11,
                "syscalls": 22,
                "virtual_nanoseconds": 33,
            },
            "run2": {
                "scheduler_turns": 12,
                "syscalls": 23,
                "virtual_nanoseconds": 34,
            },
            "wall_time_max_ms": 9,
            "wall_time_min_ms": 7,
        },
    )
    historical = _validation_row(
        hermit_commit,
        run_id="fixture-full-history",
        profile="full",
        cell=HISTORICAL,
        record_id="fixture-record-history",
        result="pass",
        minute=0,
    )
    unavailable_history = stage1.unavailable_schema6_row(
        hermit_commit, "renderer-fixture"
    )
    current = [
        _validation_row(
            hermit_commit,
            run_id="fixture-super-current",
            profile="super",
            cell=GREEN,
            record_id="fixture-record-super",
            result="pass",
            minute=1,
        ),
        _validation_row(
            hermit_commit,
            run_id="fixture-quick-current",
            profile="quick",
            cell=RED,
            record_id="fixture-record-quick",
            result="fail",
            minute=2,
            diverged=True,
        ),
        _validation_row(
            hermit_commit,
            run_id="fixture-full-current",
            profile="full",
            cell=UNAVAILABLE,
            record_id="fixture-record-full",
            result="pass",
            minute=3,
        ),
    ]
    canonical = stage1.canonical_input(
        parent_commit,
        rows,
        validation_rows=[unavailable_history, historical, *current],
        current_validation_rows=current,
        incomplete_data=[stage1.cell_results_gap(unavailable_history)],
    )
    stage1.install_inputs(monkeypatch, canonical, manifest)
    return stage1.build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )


def fixture_files() -> dict[str, bytes]:
    return {
        path.relative_to(FIXTURE).as_posix(): path.read_bytes()
        for path in FIXTURE.rglob("*")
        if path.is_file()
    }


def copy_fixture(destination: Path) -> None:
    for relative, value in fixture_files().items():
        path = destination / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)


def tree_snapshot(root: Path) -> dict[str, tuple[str, bytes]]:
    snapshot = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        snapshot[relative] = (
            ("directory", b"") if path.is_dir() else ("file", path.read_bytes())
        )
    return snapshot


class PageParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.ids: set[str] = set()
        self.hrefs: list[str] = []
        self.count_links: list[tuple[str, int]] = []
        self.filter_count_links: list[tuple[str, int]] = []
        self.row_counts: dict[str, int] = {}
        self.represented_links: list[tuple[str, int]] = []
        self.represented_counts: dict[str, int] = {}
        self.cell_states: Counter[str] = Counter()
        self.series_rows = 0
        self.scripts = 0
        self.script_sources: list[str] = []
        self.cell_rows: list[dict[str, str]] = []
        self.recorded_not_in_manifest_rows: list[dict[str, str]] = []
        self.tags: Counter[str] = Counter()
        self.html_lang: str | None = None
        self.h1_count = 0
        self.th_scopes: list[str | None] = []
        self.aria_labels: list[str] = []
        self.represented_link_texts: list[tuple[int, int, list[str]]] = []
        self._active_represented_link: list[str] | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        self.tags[tag] += 1
        if tag == "html":
            self.html_lang = values.get("lang")
        if tag == "h1":
            self.h1_count += 1
        if tag == "th":
            self.th_scopes.append(values.get("scope"))
        if values.get("aria-label") is not None:
            self.aria_labels.append(str(values["aria-label"]))
        identity = values.get("id")
        if identity is not None:
            assert identity not in self.ids
            self.ids.add(identity)
            if values.get("data-row-count") is not None:
                self.row_counts[identity] = int(values["data-row-count"])
            if values.get("data-represented-count") is not None:
                self.represented_counts[identity] = int(
                    values["data-represented-count"]
                )
        href = values.get("href")
        if href is not None:
            self.hrefs.append(href)
            if values.get("data-count") is not None:
                destination = (
                    self.filter_count_links
                    if values.get("data-cell-filter-link") == "1"
                    else self.count_links
                )
                destination.append((href, int(values["data-count"])))
            if values.get("data-represented-count") is not None:
                self.represented_links.append(
                    (href, int(values["data-represented-count"]))
                )
                assert values.get("data-count") is not None
                text_parts: list[str] = []
                self.represented_link_texts.append(
                    (
                        int(values["data-count"]),
                        int(values["data-represented-count"]),
                        text_parts,
                    )
                )
                self._active_represented_link = text_parts
        if values.get("data-cell-row") == "1":
            self.cell_states[str(values["data-state"])] += 1
            self.cell_rows.append({key: str(value) for key, value in values.items()})
        if values.get("data-recorded-not-in-manifest-row") == "1":
            self.recorded_not_in_manifest_rows.append(
                {key: str(value) for key, value in values.items()}
            )
        if values.get("data-series-row") == "1":
            self.series_rows += 1
        if tag == "script":
            self.scripts += 1
            if values.get("src") is not None:
                self.script_sources.append(str(values["src"]))

    def handle_endtag(self, tag: str) -> None:
        if tag == "a":
            self._active_represented_link = None

    def handle_data(self, data: str) -> None:
        if self._active_represented_link is not None:
            self._active_represented_link.append(data)


def parse_site(files: dict[str, bytes]) -> dict[str, PageParser]:
    parsed = {}
    for path, value in files.items():
        if not path.endswith(".html"):
            continue
        parser = PageParser()
        parser.feed(value.decode())
        parsed[path] = parser
    return parsed


def link_target(source: str, href: str) -> tuple[str, str, str]:
    split = urlsplit(href)
    assert split.scheme == ""
    assert split.netloc == ""
    target = (
        source
        if split.path == ""
        else posixpath.normpath(posixpath.join(posixpath.dirname(source), split.path))
    )
    assert not target.startswith("../")
    return target, split.query, split.fragment


def cell_filter_records(files: dict[str, bytes]) -> list[dict[str, Any]]:
    parser = parse_site(files)["cells/index.html"]
    records = []
    for index, row in enumerate(parser.cell_rows):
        records.append(
            {
                "population": "in_manifest",
                "manifest": index,
                "cell": row["data-cell"],
                "test": row["data-test"],
                "lane": row["data-lane"],
                "category": row["data-category"],
                "mode": row["data-mode"],
                "backend": row["data-backend"],
                "state": row["data-state"],
                "result": row["data-result"],
                "selected_by_full": row["data-selected-by-full"],
                "selected_by_paths": json.loads(row["data-selected-by-paths"]),
                "evidence": row["data-evidence"],
                "latest_result_time": row["data-latest-result-time"],
                "latest_result_tree": row["data-latest-result-tree"],
                "search": row["data-search"],
                "series_evidence": json.loads(row["data-series-evidence"]),
                "physical_row_count": int(row["data-physical-row-count"]),
                "represented_run_count": int(row["data-represented-run-count"]),
            }
        )
    for index, row in enumerate(parser.recorded_not_in_manifest_rows):
        records.append(
            {
                "population": "not_in_current_manifest",
                "manifest": index,
                "cell": row["data-cell"],
                "search": row["data-search"],
                "series_evidence": [],
                "physical_row_count": int(row["data-physical-row-count"]),
                "represented_run_count": int(row["data-represented-run-count"]),
            }
        )
    return records


def filter_allowed(model: Any) -> dict[str, list[str]]:
    return {
        "lane": sorted({str(cell["lane"]) for cell in model.cells}),
        "category": sorted({str(cell["category"]) for cell in model.cells}),
        "mode": sorted({str(cell["mode"]) for cell in model.cells}),
        "backend": sorted({str(cell["backend"]) for cell in model.cells}),
        "host_capability": list(model.host_capability_names),
    }


def run_filter_javascript(operation: str, payload: dict[str, Any]) -> dict[str, Any]:
    program = r"""
const fs = require("fs");
const api = require(process.argv[1]);
const input = JSON.parse(fs.readFileSync(0, "utf8"));
let output;
if (input.operation === "query") {
  const parsed = api.parseQuery(input.query, input.allowed);
  if (!parsed.ok) {
    output = parsed;
  } else {
    const matching = api.sortRecords(
      input.records.filter((record) => api.matches(record, parsed.state)),
      parsed.state,
    );
    output = {
      ok: true,
      state: parsed.state,
      serialized: api.serializeState(parsed.state),
      cells: matching.map((record) => record.cell),
      summary: api.summarize(matching),
    };
  }
} else if (input.operation === "sort") {
  output = { cells: api.sortRecords(input.records, input.state).map((record) => record.cell) };
} else {
  throw new Error(`unknown operation ${input.operation}`);
}
process.stdout.write(JSON.stringify(output));
"""
    result = subprocess.run(
        ["node", "-e", program, str(SITE_JS)],
        input=json.dumps({"operation": operation, **payload}),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
    return json.loads(result.stdout)


def test_checked_in_fixture_is_exact_current_stage1_output(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    value = representative_build_data(tmp_path, monkeypatch)
    expected = render_page._writer().render_tree(value)

    assert fixture_files() == expected


def test_fixture_exercises_the_distinctions_the_page_must_preserve() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    cells = {cell["cell"]: cell for cell in data["cells"]}
    series_rows = {row["event_id"]: row for row in data["runs"]["series_rows"]}
    provenance = data["provenance"]

    assert build["counts"] == {
        "current_validation_run_row_count": 3,
        "green_cell_count": 2,
        "in_manifest_cell_count": 5,
        "incomplete_data_count": 11,
        "never_measured_cell_count": 1,
        "physical_row_count": 4,
        "red_cell_count": 1,
        "represented_run_count": 7,
        "selected_by_full_custom_command_count": 1,
        "state_unavailable_cell_count": 1,
        "test_count": 6,
        "validation_history_row_count": 5,
    }
    assert cells[GREEN]["state"] == "green"
    assert cells[GREEN]["selected_by"].keys() == {"super"}
    assert cells[RED]["state"] == "red"
    assert cells[RED]["selected_by"].keys() == {"quick"}
    assert cells[NEVER]["state"] == "never-measured"
    assert cells[NEVER]["physical_row_count"] == 0
    assert cells[NEVER]["selected_by_full"] is False
    assert cells[UNAVAILABLE]["state"] is None
    assert cells[UNAVAILABLE]["selected_by"].keys() == {"full"}
    assert cells[HISTORICAL]["state"] == "green"
    assert cells[HISTORICAL]["selected_by_full"] is False
    assert cells[HISTORICAL]["physical_row_count"] == 1
    assert cells[HISTORICAL]["represented_run_count"] == 4
    assert data["runs"]["current_validation_run_row_count"] == 3
    assert data["runs"]["validation_history_row_count"] == 5
    unavailable_row = next(
        row
        for row in data["runs"]["validation_history_rows"]
        if row["run_id"] == "retired-renderer-fixture"
    )
    assert unavailable_row["profile"] == "quick"
    assert unavailable_row["cell_results"] is None
    assert [item["cell"] for item in data["selected_by_full_custom_commands"]] == [
        CUSTOM
    ]
    assert provenance[
        "series_rows_projection_sha256"
    ] == render_page._writer()._build_data_module()._digest(data["runs"]["series_rows"])
    assert (
        provenance["series_rows_sha256"] != provenance["series_rows_projection_sha256"]
    )
    assert "main_ancestry" not in series_rows["green-retry-1"]["series"]
    assert "first_divergent_messages" not in series_rows["green-retry-1"]["series"]
    assert series_rows["green-retry-1"]["current_main_ancestry"] is True


def test_site_is_deterministic_and_truthful() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    first = render_page.render_site(build, data)
    second = render_page.render_site(build, data)
    model = render_page._site_model(build, data)
    all_text = "\n".join(
        value.decode() for path, value in first.items() if path.endswith(".html")
    )
    landing = first["index.html"].decode()
    cells = first["cells/index.html"].decode()
    cells_parser = parse_site(first)["cells/index.html"]

    assert first == second
    assert len(first) == 21
    assert set(model.cell_paths.values()) <= set(first)
    assert set(model.test_paths.values()) <= set(first)
    assert set(model.run_paths.values()) <= set(first)
    assert cells_parser.cell_states == Counter(
        {"green": 2, "red": 1, "never-measured": 1, "unavailable": 1}
    )
    assert cells_parser.series_rows == 4
    assert cells_parser.scripts == 1
    assert cells_parser.script_sources == ["../assets/site.js"]
    assert all_text.count("Fixture data — not current Hermit main") == 19
    assert "3</strong> cells selected by full" in landing
    assert "2</strong> cells not selected by full" in landing
    assert "Cells selected by quick: Unavailable" in landing
    assert "Cells selected by super: Unavailable" in landing
    assert "Selected by quick" in all_text
    assert "Selected by full" in all_text
    assert "Selected by super" in all_text
    assert CUSTOM in landing
    unavailable_history = landing.index("retired-renderer-fixture")
    unavailable_history_row = landing[unavailable_history : unavailable_history + 900]
    assert "<td>quick</td>" in unavailable_history_row
    assert unavailable_history_row.count('class="unavailable">Unavailable') >= 2
    assert "Never measured" in all_text
    assert "no qualifying canonical row exists" in all_text
    assert "Product failure" in all_text
    assert "No projection exclusion reason — 4 canonical rows represent 7 runs" in cells
    assert (
        "Recorded projection exclusion reason — 0 canonical rows represent 0 runs"
        in cells
    )
    assert "green-retry-1" in cells
    assert "green-retry-2" in cells
    assert "Run index 1" in cells
    assert "Run index 2" in cells
    assert "Attempt 1" in cells
    assert "Attempt 2" in cells
    assert "Wall time: 7–9 ms" in cells
    assert "scheduler turns: 11" in cells
    assert cells.index("Producer outcome") < cells.index("Producer result")
    assert cells.index("Producer result") < cells.index("Product result")
    assert 'scope="rowgroup"' in cells
    assert 'scope="colgroup"' not in all_text
    assert "Captured series source digest" in landing
    assert "Public series projection digest" in landing
    assert "source-only fixture message" not in all_text
    assert "/home/" not in all_text
    assert cells.index(
        '<th scope="col">State from qualifying canonical rows'
    ) < cells.index('<th scope="col">Latest product result')
    assert "enabled" not in all_text.casefold()
    assert "selected_count" not in all_text
    assert render_page.render_page(build, data) == first["index.html"]


def test_every_artifact_is_read_and_bound(monkeypatch: pytest.MonkeyPatch) -> None:
    writer = render_page._writer()
    observed = []
    original = writer._read_file

    def record(directory: int, name: str, limit: int) -> bytes:
        observed.append(name)
        return original(directory, name, limit)

    monkeypatch.setattr(writer, "_read_file", record)
    render_page.load_artifacts(FIXTURE)

    assert Counter(observed) == Counter(
        {
            "build.json": 1,
            "cells.json.gz": 1,
            "incomplete-data.json.gz": 1,
            "runs.json.gz": 1,
            "tests.json.gz": 1,
        }
    )


def test_dynamic_text_is_html_escaped() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    mutated = copy.deepcopy(data)
    mutated["tests"][0]["description"] = '<script src="https://bad.invalid/x"></script>'

    text = render_page.render_page(build, mutated).decode()

    assert '<script src="https://bad.invalid/x"></script>' not in text
    assert "&lt;script src=&quot;https://bad.invalid/x&quot;&gt;" in text


def test_renderer_refuses_unknown_product_results() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    cell_mutation = copy.deepcopy(data)
    cell_mutation["cells"][0]["latest_result"] = "unknown"
    with pytest.raises(render_page.RenderRefused, match="latest product result"):
        render_page.render_page(build, cell_mutation)

    series_mutation = copy.deepcopy(data)
    series_mutation["runs"]["series_rows"][0]["product_result"] = "unknown"
    with pytest.raises(render_page.RenderRefused, match="series product result"):
        render_page.render_page(build, series_mutation)


def test_css_is_local_and_preserves_non_color_state_labels() -> None:
    css = render_page._site_css().decode()

    assert "http:" not in css.casefold()
    assert "https:" not in css.casefold()
    assert "@import" not in css.casefold()
    assert "url(" not in css.casefold()
    assert ":focus-visible" in css
    assert ".state-green" in css
    assert ".state-red" in css


def test_javascript_is_local_and_uses_safe_visible_updates() -> None:
    javascript = render_page._site_javascript().decode()
    check = subprocess.run(
        ["node", "--check", str(SITE_JS)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert check.returncode == 0, check.stderr
    assert check.stdout == ""
    assert check.stderr == ""
    lowered = javascript.casefold()
    assert "textcontent" in lowered
    assert "innerhtml" not in lowered
    assert "outerhtml" not in lowered
    assert "insertadjacenthtml" not in lowered
    assert "document.write" not in lowered
    assert "fetch(" not in lowered
    assert "xmlhttprequest" not in lowered
    assert "websocket" not in lowered
    assert "http:" not in lowered
    assert "https:" not in lowered


@pytest.mark.parametrize(
    "source",
    [
        b"fetch('x'); textContent = 'x';\n",
        b"node.innerHTML = 'x'; textContent = 'x';\n",
        b"const value = 'no visible update';\n",
    ],
)
def test_javascript_asset_refuses_network_html_injection_or_no_text_updates(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, source: bytes
) -> None:
    path = tmp_path / "site.js"
    path.write_bytes(source)
    monkeypatch.setattr(render_page, "SITE_JS_PATH", path)

    with pytest.raises(render_page.RenderRefused, match="site JavaScript"):
        render_page._site_javascript()


def test_every_internal_link_fragment_and_aggregate_resolves() -> None:
    files = render_page.render_site(*render_page.load_artifacts(FIXTURE))
    parsers = parse_site(files)

    for source, parser in parsers.items():
        for href in parser.hrefs:
            target, _query, fragment = link_target(source, href)
            assert target in files
            if fragment:
                assert target in parsers
                assert fragment in parsers[target].ids
        for href, count in parser.count_links:
            target, query, fragment = link_target(source, href)
            assert query == ""
            assert fragment
            assert parsers[target].row_counts[fragment] == count
        for href, count in parser.represented_links:
            target, query, fragment = link_target(source, href)
            assert query == ""
            assert fragment
            assert parsers[target].represented_counts[fragment] == count
        for physical, represented, text_parts in parser.represented_link_texts:
            visible_numbers = re.findall(r"\d+", " ".join(text_parts))
            required = Counter((str(physical), str(represented)))
            assert Counter(visible_numbers) >= required


def test_landing_filter_links_resolve_to_exact_matching_rows() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    model = render_page._site_model(build, data)
    files = render_page.render_site(build, data)
    parsers = parse_site(files)
    records = cell_filter_records(files)
    allowed = filter_allowed(model)

    links = [
        (source, href, count)
        for source, parser in parsers.items()
        for href, count in parser.filter_count_links
    ]
    assert len(links) == 7
    for source, href, count in links:
        target, query, fragment = link_target(source, href)
        assert target == "cells/index.html"
        assert query
        result = run_filter_javascript(
            "query", {"query": query, "allowed": allowed, "records": records}
        )
        assert result["ok"] is True
        assert result["summary"]["matching"] == count
        expected_fragment = (
            "recorded-not-in-current-manifest"
            if result["state"]["population"] == "not_in_current_manifest"
            else "all-cells"
        )
        assert fragment == expected_fragment


@pytest.mark.parametrize(
    ("query", "cells", "physical", "represented"),
    [
        ("", list(CELLS), 4, 7),
        ("state=green", [GREEN, HISTORICAL], 3, 6),
        ("state=red", [RED], 1, 1),
        ("state=never-measured", [NEVER], 0, 0),
        ("state=unavailable", [UNAVAILABLE], 0, 0),
        ("selected_by_full=true", [GREEN, RED, UNAVAILABLE], 3, 3),
        ("selected_by_full=false", [HISTORICAL, NEVER], 1, 4),
        ("selected_by_path=quick", [RED], 1, 1),
        ("selected_by_path=full", [UNAVAILABLE], 0, 0),
        ("selected_by_path=super", [GREEN], 2, 2),
        ("result=pass", [GREEN, HISTORICAL], 3, 6),
        ("result=product_failure", [RED], 1, 1),
        ("result=unavailable", [NEVER, UNAVAILABLE], 0, 0),
        ("evidence=complete", [NEVER], 0, 0),
        ("evidence=incomplete", [GREEN, HISTORICAL, RED, UNAVAILABLE], 4, 7),
        ("backend=ptrace", [GREEN, HISTORICAL, UNAVAILABLE], 3, 6),
        ("backend=dbt", [NEVER], 0, 0),
        ("backend=kvm", [RED], 1, 1),
        ("current_main_ancestry=true", [GREEN, HISTORICAL, RED], 4, 7),
        ("current_main_ancestry=false", [], 0, 0),
        ("current_main_ancestry=unavailable", [NEVER, UNAVAILABLE], 0, 0),
        (
            "host_capability=kvm&host_capability_present=true",
            [GREEN, HISTORICAL, RED],
            4,
            7,
        ),
        (
            "host_capability=kvm&host_capability_present=false",
            [],
            0,
            0,
        ),
        (
            "host_capability=kvm&host_capability_present=unavailable",
            [NEVER, UNAVAILABLE],
            0,
            0,
        ),
        ("q=fixture%2Fgreen", [GREEN], 2, 2),
        ("q=green%2Fverify", [GREEN], 2, 2),
        ("q=Fixture+for", [], 0, 0),
        ("q=not+selected+by+full", [], 0, 0),
    ],
)
def test_cell_filters_use_only_the_recorded_fields_for_each_axis(
    query: str, cells: list[str], physical: int, represented: int
) -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    model = render_page._site_model(build, data)
    files = render_page.render_site(build, data)
    result = run_filter_javascript(
        "query",
        {
            "query": query,
            "allowed": filter_allowed(model),
            "records": cell_filter_records(files),
        },
    )

    assert result["ok"] is True
    assert set(result["cells"]) == set(cells)
    assert result["summary"] == {
        "matching": len(cells),
        "physical": physical,
        "represented": represented,
    }


def test_row_scoped_filters_require_one_same_canonical_row() -> None:
    allowed = {
        "lane": [],
        "category": [],
        "mode": [],
        "backend": [],
        "host_capability": ["kvm"],
    }
    template = {
        "population": "in_manifest",
        "manifest": 0,
        "cell": "fixture/synthetic/verify/ptrace",
        "test": "fixture/synthetic",
        "lane": "portable",
        "category": "tests",
        "mode": "verify",
        "backend": "ptrace",
        "state": "green",
        "result": "pass",
        "selected_by_full": "true",
        "selected_by_paths": [],
        "evidence": "complete",
        "latest_result_time": "",
        "latest_result_tree": "tree",
        "search": "fixture/synthetic fixture/synthetic/verify/ptrace",
        "physical_row_count": 2,
        "represented_run_count": 2,
    }
    split_evidence = {
        **template,
        "series_evidence": [
            {
                "current_main_ancestry": "true",
                "host_capabilities": {"kvm": "false"},
            },
            {
                "current_main_ancestry": "false",
                "host_capabilities": {"kvm": "true"},
            },
        ],
    }
    zero_rows = {
        **template,
        "cell": "fixture/no-rows/verify/ptrace",
        "physical_row_count": 0,
        "represented_run_count": 0,
        "series_evidence": [],
    }
    impossible = run_filter_javascript(
        "query",
        {
            "query": (
                "current_main_ancestry=true&host_capability=kvm&"
                "host_capability_present=true"
            ),
            "allowed": allowed,
            "records": [split_evidence, zero_rows],
        },
    )
    unavailable = run_filter_javascript(
        "query",
        {
            "query": (
                "current_main_ancestry=unavailable&host_capability=kvm&"
                "host_capability_present=unavailable"
            ),
            "allowed": allowed,
            "records": [split_evidence, zero_rows],
        },
    )

    assert impossible["cells"] == []
    assert unavailable["cells"] == [zero_rows["cell"]]
    assert unavailable["summary"] == {"matching": 1, "physical": 0, "represented": 0}


@pytest.mark.parametrize(
    "query",
    [
        "unknown=value",
        "state=green&state=red",
        "state=blue",
        "q=",
        "q=%zz",
        "host_capability=kvm",
        "host_capability_present=true",
        "population=not_in_current_manifest&state=green",
        "population=not_in_current_manifest&sort=state",
    ],
)
def test_invalid_url_filter_state_is_refused(query: str) -> None:
    model = render_page._site_model(*render_page.load_artifacts(FIXTURE))
    result = run_filter_javascript(
        "query", {"query": query, "allowed": filter_allowed(model), "records": []}
    )

    assert result["ok"] is False
    assert result["error"]


def test_filter_url_round_trip_is_canonical_and_shareable() -> None:
    model = render_page._site_model(*render_page.load_artifacts(FIXTURE))
    allowed = filter_allowed(model)
    query = (
        "direction=desc&state=green&q=fixture%2Fgreen&"
        "sort=represented_run_count&backend=ptrace"
    )
    first = run_filter_javascript(
        "query", {"query": query, "allowed": allowed, "records": []}
    )
    second = run_filter_javascript(
        "query", {"query": first["serialized"], "allowed": allowed, "records": []}
    )
    outside = run_filter_javascript(
        "query",
        {
            "query": "population=not_in_current_manifest",
            "allowed": allowed,
            "records": [],
        },
    )

    assert first["ok"] is True
    assert first["serialized"] == (
        "q=fixture%2Fgreen&backend=ptrace&state=green&"
        "sort=represented_run_count&direction=desc"
    )
    assert second["state"] == first["state"]
    assert second["serialized"] == first["serialized"]
    assert outside["state"]["sort"] == "cell"
    assert outside["serialized"] == "population=not_in_current_manifest"


def test_sorting_has_explicit_ties_and_unavailable_values_last() -> None:
    records = [
        {"cell": "b", "latest_result_time": "", "represented_run_count": 1},
        {
            "cell": "d",
            "latest_result_time": "2026-09-01T00:00:00Z",
            "represented_run_count": 3,
        },
        {
            "cell": "c",
            "latest_result_time": "2026-08-31T00:00:00Z",
            "represented_run_count": 2,
        },
        {
            "cell": "a",
            "latest_result_time": "2026-08-31T00:00:00Z",
            "represented_run_count": 2,
        },
    ]
    ascending = run_filter_javascript(
        "sort",
        {
            "records": records,
            "state": {"sort": "latest_result_time", "direction": "asc"},
        },
    )
    descending = run_filter_javascript(
        "sort",
        {
            "records": records,
            "state": {"sort": "latest_result_time", "direction": "desc"},
        },
    )
    numeric_descending = run_filter_javascript(
        "sort",
        {
            "records": records,
            "state": {"sort": "represented_run_count", "direction": "desc"},
        },
    )

    assert ascending["cells"] == ["a", "c", "d", "b"]
    assert descending["cells"] == ["d", "a", "c", "b"]
    assert numeric_descending["cells"] == ["d", "a", "c", "b"]


def test_custom_commands_and_outside_identities_keep_separate_evidence() -> None:
    series_only = "fixture/series-only/verify/ptrace"
    ledger_only = "fixture/ledger-only/verify/kvm"
    current = "fixture/current/verify/ptrace"
    custom = "fixture/custom/custom/ptrace"
    history_id = "a" * 64
    series_rows = (
        {
            "event_id": "outside-event",
            "series": {"cell": series_only, "num_runs": 3},
        },
        {"event_id": "current-event", "series": {"cell": current, "num_runs": 1}},
        {"event_id": "custom-event", "series": {"cell": custom, "num_runs": 2}},
    )
    history_rows = (
        {
            "history_row_sha256": history_id,
            "cell_results": {
                "selected_by": {
                    "quick": {
                        "cells": [
                            {
                                "test": "fixture/ledger-only",
                                "mode": "verify",
                                "backend": "kvm",
                            },
                            {
                                "test": "fixture/custom",
                                "mode": "custom",
                                "backend": "ptrace",
                            },
                        ]
                    }
                },
                "recorded": {
                    "cells": [
                        {
                            "test": "fixture/ledger-only",
                            "mode": "verify",
                            "backend": "kvm",
                        },
                        {
                            "test": "fixture/current",
                            "mode": "verify",
                            "backend": "ptrace",
                        },
                        {
                            "test": "fixture/custom",
                            "mode": "custom",
                            "backend": "ptrace",
                        },
                    ]
                },
            },
        },
    )
    outside = render_page._recorded_identities_not_in_current_manifest(
        ({"cell": current},),
        ({"cell": custom},),
        series_rows,
        history_rows,
    )
    custom_evidence = render_page._recorded_identity_evidence(
        (custom,), series_rows, history_rows
    )

    assert [value["cell"] for value in outside] == [ledger_only, series_only]
    assert outside[0]["physical_row_count"] == 0
    assert outside[0]["represented_run_count"] == 0
    assert outside[0]["selected_history_rows"] == (history_id,)
    assert outside[0]["recorded_history_rows"] == (history_id,)
    assert outside[1]["physical_row_count"] == 1
    assert outside[1]["represented_run_count"] == 3
    assert custom_evidence == (
        {
            "cell": custom,
            "series_rows": (series_rows[2],),
            "physical_row_count": 1,
            "represented_run_count": 2,
            "selected_history_rows": (history_id,),
            "recorded_history_rows": (history_id,),
        },
    )


def test_custom_command_evidence_is_visible_linked_and_outside_cell_counts() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    changed_build = copy.deepcopy(build)
    changed = copy.deepcopy(data)

    event = copy.deepcopy(changed["runs"]["series_rows"][-1])
    event.update(event_id="custom-evidence", run_id="fixture-custom-history")
    event["series"].update(cell=CUSTOM, num_runs=3)
    changed["runs"]["series_rows"].append(event)
    for field, amount in (
        ("series_physical_row_count", 1),
        ("series_represented_run_count", 3),
        ("projection_admitted_physical_row_count", 1),
        ("projection_admitted_represented_run_count", 3),
    ):
        changed["runs"][field] += amount
    changed["counts"]["physical_row_count"] += 1
    changed["counts"]["represented_run_count"] += 3

    history = copy.deepcopy(changed["runs"]["validation_history_rows"][1])
    history_id = "f" * 64
    history.update(
        history_row_sha256=history_id,
        record_id="fixture-record-custom",
        run_id="fixture-custom-history",
    )
    identity = {
        "backend": "ptrace",
        "category": "tests",
        "lane": "portable",
        "mode": "custom",
        "test": "fixture/custom-command",
    }
    results = history["cell_results"]
    results["run_id"] = "fixture-custom-history"
    results["selected_by"] = {
        "full": {"cell_count": 1, "cells": [copy.deepcopy(identity)]}
    }
    recorded = copy.deepcopy(identity)
    recorded["cell_verdict"] = copy.deepcopy(
        results["recorded"]["cells"][0]["cell_verdict"]
    )
    results["recorded"]["cell_count"] = 1
    results["recorded"]["cells"] = [recorded]
    changed["runs"]["validation_history_rows"].append(history)
    changed["runs"]["validation_history_row_count"] += 1
    changed["counts"]["validation_history_row_count"] += 1
    changed_build["counts"] = copy.deepcopy(changed["counts"])

    model = render_page._site_model(changed_build, changed)
    files = render_page.render_site(changed_build, changed)
    cells = files["cells/index.html"].decode()
    fragment = render_page._fragment("custom-command", CUSTOM)
    row_start = cells.index(f'id="{fragment}"')
    row = cells[row_start : cells.index("</tr>", row_start)]

    assert len(model.cells) == len(CELLS)
    assert CUSTOM not in {
        value["cell"] for value in model.recorded_not_in_current_manifest
    }
    assert [value["cell"] for value in model.custom_command_evidence] == [CUSTOM]
    assert "Recorded evidence for custom commands" in cells
    assert "selected by full but are not cells in the manifest" in cells
    assert "without deriving a current cell state, never-measured result" in cells
    assert "custom-evidence" in row
    assert "Selected by full" in row
    assert ">1</td><td>3</td>" in row
    assert f'href="#{render_page._fragment("series", "custom-evidence")}"' in row
    assert f'href="../{model.run_paths[history_id]}"' in row
    assert f'href="../{model.test_paths["fixture/custom-command"]}"' in row
    assert CUSTOM not in {record["cell"] for record in cell_filter_records(files)}


def test_no_javascript_keeps_complete_tables_and_anchors_useful() -> None:
    files = render_page.render_site(*render_page.load_artifacts(FIXTURE))
    text = files["cells/index.html"].decode()
    parser = parse_site(files)["cells/index.html"]

    assert 'data-cell-filter-form action="index.html" method="get" hidden' in text
    assert len(parser.cell_rows) == len(CELLS)
    assert all("hidden" not in row for row in parser.cell_rows)
    assert 'id="all-cells"' in text
    assert 'id="recorded-not-in-current-manifest"' in text
    assert 'id="selected-by-full"' in text
    assert 'id="not-selected-by-full"' in text
    assert "The complete tables remain below" in text


def test_detail_paths_are_full_sha256_and_collision_safe(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    model = render_page._site_model(build, data)
    pattern = re.compile(r"(?:cells|tests|runs)/[0-9a-f]{64}\.html")

    assert all(pattern.fullmatch(path) for path in model.cell_paths.values())
    assert all(pattern.fullmatch(path) for path in model.test_paths.values())
    assert all(pattern.fullmatch(path) for path in model.run_paths.values())
    assert len(set(model.cell_paths.values())) == len(model.cells)
    assert len(set(model.test_paths.values())) == len(model.tests)
    assert len(set(model.run_paths.values())) == len(model.history_rows)
    assert all("fixture/" not in path for path in render_page.render_site(build, data))

    monkeypatch.setattr(render_page, "_full_digest", lambda _value: "0" * 64)
    with pytest.raises(render_page.RenderRefused, match="same page path"):
        render_page._site_model(build, data)


def test_pages_have_accessible_structure_and_no_external_resources() -> None:
    files = render_page.render_site(*render_page.load_artifacts(FIXTURE))
    parsers = parse_site(files)

    for path, parser in parsers.items():
        text = files[path].decode()
        assert parser.html_lang == "en"
        assert parser.h1_count == 1
        assert parser.tags["header"] >= 2
        assert parser.tags["main"] == 1
        assert parser.tags["footer"] == 1
        assert parser.tags["nav"] >= 2
        assert parser.tags["table"] == parser.tags["caption"]
        assert set(parser.th_scopes) <= {"col", "row", "rowgroup"}
        assert {"Primary navigation", "Breadcrumb"} <= set(parser.aria_labels)
        expected_scripts = 1 if path == "cells/index.html" else 0
        assert parser.scripts == expected_scripts
        assert "Fixture data — not current Hermit main" in text
        for source in parser.script_sources:
            target, query, fragment = link_target(path, source)
            assert query == ""
            assert fragment == ""
            assert target == "assets/site.js"
            assert target in files
        assert "http:" not in text.casefold()
        assert "https:" not in text.casefold()
        assert "file:" not in text.casefold()
        assert "/home/" not in text
        assert "token=" not in text.casefold()
        assert "secret=" not in text.casefold()


def test_detail_pages_preserve_evidence_and_unavailable_joins() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    model = render_page._site_model(build, data)
    files = render_page.render_site(build, data)
    green = files[model.cell_paths[GREEN]].decode()
    historical = files[model.cell_paths[HISTORICAL]].decode()
    unavailable_cell = files[model.cell_paths[UNAVAILABLE]].decode()
    green_test = files[model.test_paths["fixture/green"]].decode()
    unavailable_run = next(
        row for row in model.history_rows if row["run_id"] == "retired-renderer-fixture"
    )
    unavailable_page = files[
        model.run_paths[str(unavailable_run["history_row_sha256"])]
    ].decode()

    assert green.index("State from qualifying canonical rows") < green.index(
        "Latest product result"
    )
    assert "Selected by super" in green
    assert "green-retry-1" in green
    assert "green-retry-2" in green
    assert "fixture-kernel" in green
    assert "cpuid-faulting" in green
    assert "Wall time: 7–9 ms" in green
    assert "Validation history row selected this cell by super" in green
    assert "Guest arguments</dt><dd><pre><code>[]</code></pre>" in green
    assert "Preprocessors</dt><dd><pre><code>[]</code></pre>" in green_test
    assert "event ids: None recorded" in unavailable_cell
    assert "No single retained validation-run selection names this event" in historical
    assert "Historical invocation" in historical
    assert "Raw retained logs" in historical
    assert "Syscall histogram" in historical
    assert "Validate path</dt><dd>quick" in unavailable_page
    assert "This retained history row has no usable cell_results" in unavailable_page
    assert unavailable_page.count('class="unavailable">Unavailable') >= 2


def test_ambiguous_validation_history_join_is_unavailable() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    changed_build = copy.deepcopy(build)
    changed = copy.deepcopy(data)
    duplicate = copy.deepcopy(changed["runs"]["validation_history_rows"][2])
    duplicate["history_row_sha256"] = "f" * 64
    changed["runs"]["validation_history_rows"].append(duplicate)
    changed["runs"]["validation_history_row_count"] += 1
    changed["counts"]["validation_history_row_count"] += 1
    changed_build["counts"]["validation_history_row_count"] += 1

    model = render_page._site_model(changed_build, changed)
    page = render_page.render_site(changed_build, changed)[
        model.cell_paths[GREEN]
    ].decode()

    assert "Matching validation history row unavailable" in page
    assert "No single validation history row matches the retained selection" in page


def test_path_traversal_and_unsafe_generated_paths_refuse(tmp_path: Path) -> None:
    with pytest.raises(render_page.RenderRefused, match="path traversal"):
        render_page.load_artifacts(FIXTURE / ".." / FIXTURE.name)
    with pytest.raises(render_page.RenderRefused, match="path traversal"):
        render_page.write_page(FIXTURE, tmp_path / "safe" / ".." / "escape")
    with pytest.raises(render_page.RenderRefused, match="unsafe path"):
        render_page._validate_output_files({"../index.html": b"x"})


def test_output_below_artifact_tree_refuses_without_changing_source(
    tmp_path: Path,
) -> None:
    artifacts = tmp_path / "artifact-copy"
    copy_fixture(artifacts)
    before = tree_snapshot(artifacts)

    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--artifacts",
            str(artifacts),
            "--output",
            str(artifacts / "site"),
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 2
    assert result.stdout == ""
    assert "overlaps the Stage 1 artifact tree" in result.stderr
    assert tree_snapshot(artifacts) == before


def test_output_ancestor_and_symlinked_descendant_refuse_without_changes(
    tmp_path: Path,
) -> None:
    container = tmp_path / "container"
    artifacts = container / "artifacts"
    copy_fixture(artifacts)
    before = tree_snapshot(artifacts)

    ancestor = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--artifacts",
            str(artifacts),
            "--output",
            str(container),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    link = tmp_path / "artifact-link"
    link.symlink_to(artifacts, target_is_directory=True)
    through_link = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--artifacts",
            str(artifacts),
            "--output",
            str(link / "site"),
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert ancestor.returncode == 2
    assert ancestor.stdout == ""
    assert "overlaps the Stage 1 artifact tree" in ancestor.stderr
    assert through_link.returncode == 2
    assert through_link.stdout == ""
    assert "output destination" in through_link.stderr
    assert tree_snapshot(artifacts) == before


def test_symlink_loop_output_parent_refuses_without_traceback_or_changes(
    tmp_path: Path,
) -> None:
    artifacts = tmp_path / "artifact-copy"
    copy_fixture(artifacts)
    before = tree_snapshot(artifacts)
    loop = tmp_path / "loop"
    loop.symlink_to("loop")

    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--artifacts",
            str(artifacts),
            "--output",
            str(loop / "site"),
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 2
    assert result.stdout == ""
    assert "compatibility-website render page: REFUSED:" in result.stderr
    assert "Traceback" not in result.stderr
    assert tree_snapshot(artifacts) == before
    assert loop.is_symlink()


def test_cli_writes_deterministic_site_and_refuses_existing_output(
    tmp_path: Path,
) -> None:
    output = tmp_path / "prototype"
    command = [
        sys.executable,
        str(SCRIPT),
        "--artifacts",
        str(FIXTURE),
        "--output",
        str(output),
    ]
    first = subprocess.run(command, check=False, capture_output=True, text=True)

    assert first.returncode == 0
    assert first.stderr == ""
    assert first.stdout.strip() == (output / "index.html").resolve().as_uri()
    before = {
        path.relative_to(output).as_posix(): path.read_bytes()
        for path in output.rglob("*")
        if path.is_file()
    }
    model = render_page._site_model(*render_page.load_artifacts(FIXTURE))
    assert set(before) == {
        "assets/site.css",
        "assets/site.js",
        "cells/index.html",
        "incomplete-data/index.html",
        "index.html",
        *model.cell_paths.values(),
        *model.test_paths.values(),
        *model.run_paths.values(),
    }
    other = tmp_path / "prototype-second"
    repeated = subprocess.run(
        [*command[:-1], str(other)], check=False, capture_output=True, text=True
    )
    assert repeated.returncode == 0
    assert repeated.stderr == ""
    assert before == {
        path.relative_to(other).as_posix(): path.read_bytes()
        for path in other.rglob("*")
        if path.is_file()
    }

    second = subprocess.run(command, check=False, capture_output=True, text=True)

    assert second.returncode == 2
    assert second.stdout == ""
    assert "output destination already exists" in second.stderr
    assert before == {
        path.relative_to(output).as_posix(): path.read_bytes()
        for path in output.rglob("*")
        if path.is_file()
    }


@pytest.mark.parametrize(
    "relative",
    [
        "build.json",
        "data/cells.json.gz",
        "data/incomplete-data.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    ],
)
def test_changed_artifact_refuses_without_output(tmp_path: Path, relative: str) -> None:
    artifacts = tmp_path / "artifacts"
    for path, value in fixture_files().items():
        destination = artifacts / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(value)
    changed = artifacts / relative
    changed.write_bytes(changed.read_bytes() + b"changed")
    output = tmp_path / "prototype"

    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--artifacts",
            str(artifacts),
            "--output",
            str(output),
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 2
    assert result.stdout == ""
    assert not output.exists()


def test_cli_help_names_fixture_boundary_and_no_server() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--help"],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0
    assert result.stderr == ""
    help_text = " ".join(result.stdout.split())
    assert "fixture-backed prototype" in help_text
    assert "exact five-file Stage 1 fixture artifact directory" in help_text
    assert "writes only local JavaScript" in help_text
    assert "performs no network request" in help_text
    assert "performs no network request" in help_text
    assert "refuses an existing output directory" in help_text


def test_no_destructive_cleanup_operation_is_present() -> None:
    source = SCRIPT.read_text()

    for forbidden in ("unlink(", "rmdir(", "rmtree(", "remove("):
        assert forbidden not in source
