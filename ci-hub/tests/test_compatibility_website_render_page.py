from __future__ import annotations

import copy
import importlib.util
import os
import subprocess
import sys
from collections import Counter
from html.parser import HTMLParser
from pathlib import Path
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/render_page.py"
STAGE1_TEST = REPOSITORY / "ci-hub/tests/test_compatibility_website_build_data.py"
FIXTURE = REPOSITORY / "ci-hub/tests/fixtures/compatibility-website-stage-1"


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


class PageParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.ids: set[str] = set()
        self.hrefs: list[str] = []
        self.count_links: list[tuple[str, int]] = []
        self.row_counts: dict[str, int] = {}
        self.represented_links: list[tuple[str, int]] = []
        self.represented_counts: dict[str, int] = {}
        self.cell_states: Counter[str] = Counter()
        self.series_rows = 0
        self.scripts = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
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
                self.count_links.append((href, int(values["data-count"])))
            if values.get("data-represented-count") is not None:
                self.represented_links.append(
                    (href, int(values["data-represented-count"]))
                )
        if values.get("data-cell-row") == "1":
            self.cell_states[str(values["data-state"])] += 1
        if values.get("data-series-row") == "1":
            self.series_rows += 1
        if tag == "script":
            self.scripts += 1


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


def test_page_is_deterministic_linked_and_truthful() -> None:
    build, data = render_page.load_artifacts(FIXTURE)
    first = render_page.render_page(build, data)
    second = render_page.render_page(build, data)
    text = first.decode()
    parser = PageParser()
    parser.feed(text)

    assert first == second
    assert parser.scripts == 0
    assert parser.cell_states == Counter(
        {"green": 2, "red": 1, "never-measured": 1, "unavailable": 1}
    )
    assert parser.series_rows == 4
    assert all(not href.startswith(("http:", "https:", "//")) for href in parser.hrefs)
    assert set(parser.hrefs).difference({"assets/site.css"}) <= {
        f"#{identity}" for identity in parser.ids
    }
    for href, count in parser.count_links:
        assert href.startswith("#")
        assert parser.row_counts[href[1:]] == count
    for href, count in parser.represented_links:
        assert href.startswith("#")
        assert parser.represented_counts[href[1:]] == count
    assert "Fixture data — not current Hermit main" in text
    assert "3 / 5 cells selected by full" in text
    assert "2 / 5 cells not selected by full" in text
    assert "Cells selected by quick: Unavailable" in text
    assert "Cells selected by super: Unavailable" in text
    assert "Selected by quick" in text
    assert "Selected by full" in text
    assert "Selected by super" in text
    assert "1\n      custom commands selected by full" in text
    assert CUSTOM in text
    unavailable_history = text.index("retired-renderer-fixture")
    unavailable_history_row = text[unavailable_history : unavailable_history + 700]
    assert "<td>quick</td>" in unavailable_history_row
    assert unavailable_history_row.count('class="unavailable">Unavailable') >= 2
    assert "Never measured" in text
    assert "no qualifying canonical row exists" in text
    assert "Product failure" in text
    assert (
        "4</strong>\n      public canonical series rows representing <strong>7</strong> runs</a>"
        in text
    )
    assert "No projection exclusion reason — 4 canonical rows represent 7 runs" in text
    assert (
        "Recorded projection exclusion reason — 0 canonical rows represent 0 runs"
        in text
    )
    assert "green-retry-1" in text
    assert "green-retry-2" in text
    assert "Run index 1" in text
    assert "Run index 2" in text
    assert "Attempt 1" in text
    assert "Attempt 2" in text
    assert "Wall time: 7–9 ms" in text
    assert "scheduler turns: 11" in text
    assert "Producer outcome" in text
    assert text.index("Producer outcome") < text.index("Producer result")
    assert text.index("Producer result") < text.index("Product result")
    assert "Runtime" in text
    assert 'scope="rowgroup"' in text
    assert 'scope="colgroup"' not in text
    assert "Captured series source digest" in text
    assert "Public series projection digest" in text
    assert "source-only fixture message" not in text
    assert "/home/" not in text
    assert text.index("State from qualifying canonical rows") < text.index(
        "Latest product result"
    )
    assert "enabled" not in text.casefold()
    assert "selected_count" not in text


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


def test_cli_writes_one_openable_page_and_refuses_existing_output(
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
    assert {
        path.relative_to(output).as_posix()
        for path in output.rglob("*")
        if path.is_file()
    } == {"assets/site.css", "index.html"}
    before = {
        path.relative_to(output).as_posix(): path.read_bytes()
        for path in output.rglob("*")
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
    assert "writes no JavaScript" in help_text
    assert "performs no network request" in help_text
    assert "refuses an existing output directory" in help_text


def test_no_destructive_cleanup_operation_is_present() -> None:
    source = SCRIPT.read_text()

    for forbidden in ("unlink(", "rmdir(", "rmtree(", "remove("):
        assert forbidden not in source
