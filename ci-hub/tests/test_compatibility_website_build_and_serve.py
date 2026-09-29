from __future__ import annotations

import copy
import errno
import fcntl
import gzip
import importlib.util
import json
import os
import signal
import select
import shutil
import socket
import stat
import subprocess
import sys
import uuid
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlsplit

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/build_and_serve.py"
RENDER_TEST = REPOSITORY / "ci-hub/tests/test_compatibility_website_render_page.py"
DOCUMENTED_COMMANDS = REPOSITORY / "ci-hub/tests/documented_commands.py"
CAPTURED_READER_ALIASES = (
    "bounded_capture",
    "rust_toolchain",
    "ledger",
    "series",
    "finalized_proof",
    "flaky_cells",
    "flake_class",
    "gate_completeness",
    "green_class",
    "qualifying_receipt",
    "admission_paths",
    "publisher",
    "validate_rows",
)
CAPTURED_READER_MODULE_NAMES = (
    *CAPTURED_READER_ALIASES,
    *(f"compatibility_website_combined_{name}" for name in CAPTURED_READER_ALIASES),
)


def load_module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


website = load_module("compatibility_website_combined_test", SCRIPT)
renderer_test = load_module("compatibility_website_combined_fixture", RENDER_TEST)
documented_commands = load_module(
    "compatibility_website_combined_documented_commands", DOCUMENTED_COMMANDS
)


def test_live_builder_keeps_tool_identity_and_explicit_state_separate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls = []
    data = SimpleNamespace(build_data=lambda *args, **kwargs: calls.append((args, kwargs)) or {"fixture": True}, BuildRefused=RuntimeError)
    monkeypatch.setattr(website, "_modules", lambda: SimpleNamespace(build_data=data))
    monkeypatch.setattr(website, "_git_oid", lambda *_: "a" * 40)
    state = tmp_path / "state"
    assert website._live_data(series_ref="b" * 40, published_only=True, state_root=state) == {"fixture": True}
    assert calls[0][0][0] == website.REPOSITORY
    assert calls[0][1] == {"series_ref": "b" * 40, "published_only": True, "state_root": state}


@pytest.mark.parametrize("historical", [False, True])
def test_source_overlapping_destination_refuses_before_source_export(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, historical: bool,
) -> None:
    modules = website._modules()
    home = tmp_path / "git-managed-home"
    checkout = home / "work/parent"
    checkout.mkdir(parents=True)
    (home / ".git").mkdir()
    monkeypatch.setattr(website, "REPOSITORY", checkout)
    monkeypatch.setattr(website, "_live_data", lambda **_: pytest.fail("must refuse before live export"))
    monkeypatch.setattr(modules.build_data, "build_historical_data",
                        lambda *_: pytest.fail("must refuse before historical export"))
    # Even a sibling of the parent checkout remains inside its Git-managed home.
    output = home / "work/parent-compatibility-website"
    with pytest.raises(website.WebsiteRefused, match="overlaps a source directory"):
        if historical:
            website.build_historical(checkout, "a" * 40, "record", "run",
                                     checkout / "hermit", "b" * 40, output)
        else:
            website.build(output)
    assert not output.exists()


def build_data(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, Any]:
    return renderer_test.representative_build_data(tmp_path, monkeypatch)


def historical_build_data(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> dict[str, Any]:
    data = build_data(tmp_path, monkeypatch)
    row = next(
        item
        for item in data["runs"]["validation_history_rows"]
        if item.get("run_id") == "validate-run-1"
    )
    source = {
        "finished_at": row["finished_at"],
        "history_row_sha256": row["history_row_sha256"],
        "host_capabilities": {
            "cpuid-faulting": {"present": True, "evidence": "fixture"},
            "kvm": {"present": True, "evidence": "fixture"},
        },
        "kernel_version": "fixture-kernel",
        "measured_hermit_commit": row["commit"],
        "measured_reverie_commit": "e" * 40,
        "measurement_host": "ci-host",
        "record_id": row["record_id"],
        "run_id": row["run_id"],
        "selected_by_full_cell_count": 1,
        "started_at": row["started_at"],
        "validate_path": "full",
    }
    manifest_commit = "d" * 40
    canonical = data["provenance"]["canonical_input_provenance"]
    canonical.update(
        historical=copy.deepcopy(source),
        mode="historical",
        published_only=True,
        reader_worktree_clean=True,
        reader_worktree_dirty_paths=[],
    )
    canonical["reader_source_identity_sha256"] = website._digest(
        {
            "ci_hub_tree": canonical["reader_ci_hub_tree"],
            "commit": canonical["reader_commit"],
            "dirty_paths": canonical["reader_worktree_dirty_paths"],
            "files": canonical["reader_files"],
            "hermit": canonical["reader_hermit"],
            "worktree_clean": canonical["reader_worktree_clean"],
        }
    )
    data["provenance"]["hermit_commit"] = manifest_commit
    data["provenance"]["historical"] = {
        **source,
        "ledger_commit": data["provenance"]["parent_commit"],
        "ledger_ref": data["provenance"]["parent_commit"],
        "manifest_source_commit": manifest_commit,
    }
    return website._modules().build_data.validate_build_data(data)


def set_historical_finished_at(data: dict[str, Any], value: str | None) -> None:
    sources = (
        data["provenance"]["historical"],
        data["provenance"]["canonical_input_provenance"]["historical"],
    )
    for source in sources:
        if value is None:
            source.pop("finished_at")
        else:
            source["finished_at"] = value


def snapshot(root: Path) -> dict[str, tuple[int, int, bytes | str]]:
    result = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        metadata = path.lstat()
        if path.is_symlink():
            value: bytes | str = os.readlink(path)
        elif path.is_file():
            value = path.read_bytes()
        else:
            value = "directory"
        result[relative] = (metadata.st_dev, metadata.st_ino, value)
    return result


@pytest.mark.parametrize(
    ("stream", "limit"),
    [
        ("stdout", website.MAX_GIT_OID_STDOUT_BYTES),
        ("stderr", website.MAX_GIT_OID_STDERR_BYTES),
    ],
)
def test_git_oid_refuses_oversized_output_before_the_child_exits(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    stream: str,
    limit: int,
) -> None:
    marker = tmp_path / "child-finished"
    fake_git = tmp_path / "git"
    descriptor = 1 if stream == "stdout" else 2
    fake_git.write_text(
        "#!/usr/bin/python3\n"
        "import os, pathlib, time\n"
        f"os.write({descriptor}, b'x' * {limit + 1})\n"
        "time.sleep(30)\n"
        "pathlib.Path(os.environ['WEBSITE_GIT_MARKER']).write_bytes(b'finished')\n"
    )
    fake_git.chmod(0o755)
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("WEBSITE_GIT_MARKER", str(marker))

    with pytest.raises(
        website.WebsiteRefused, match=rf"{stream} exceeded {limit} bytes"
    ):
        website._git_oid(tmp_path, "HEAD")

    assert not marker.exists()


def test_git_oid_uses_the_captured_shared_bounded_process(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    expected = "a" * 40
    observed: list[tuple[list[str], dict[str, Any]]] = []
    export_inputs = website._modules().export_inputs

    def capture(command: Any, **arguments: Any) -> subprocess.CompletedProcess[bytes]:
        observed.append((list(command), arguments))
        return subprocess.CompletedProcess(command, 0, f"{expected}\n".encode(), b"")

    monkeypatch.setattr(export_inputs, "capture_bounded_process", capture)

    assert website._git_oid(tmp_path, "HEAD") == expected
    assert not hasattr(website, "_capture_bounded_process")
    assert len(observed) == 1
    command, arguments = observed[0]
    assert command == [
        "git",
        "--no-replace-objects",
        "-C",
        str(tmp_path),
        "rev-parse",
        "HEAD^{commit}",
    ]
    assert arguments["timeout"] == 20
    assert arguments["stdout_limit"] == website.MAX_GIT_OID_STDOUT_BYTES
    assert arguments["stderr_limit"] == website.MAX_GIT_OID_STDERR_BYTES
    assert arguments["environment"]["GIT_CONFIG_NOSYSTEM"] == "1"
    assert arguments["environment"]["GIT_CONFIG_GLOBAL"] == os.devnull


def write_read_only_tree(root: Path, files: dict[str, bytes]) -> None:
    root.mkdir()
    for child in website.FIXED_DIRECTORIES:
        (root / child).mkdir()
    for relative, value in files.items():
        path = root / relative
        path.write_bytes(value)
        path.chmod(0o444)
    for child in website.FIXED_DIRECTORIES:
        (root / child).chmod(0o555)
    root.chmod(0o555)


def test_compose_is_deterministic_and_exactly_binds_fixture_tree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    first_manifest, first = website.compose(data)
    second_manifest, second = website.compose(data)
    stage_1 = website._modules().writer.render_tree(data)

    assert first_manifest == second_manifest
    assert first == second
    assert len(first) == len(first_manifest["artifacts"]) + 1
    assert set(first) == {
        "build.json",
        *(row["path"] for row in first_manifest["artifacts"]),
    }
    assert json.loads(first["build.json"]) == first_manifest
    assert first["build.json"] == website._json_bytes(first_manifest) + b"\n"
    assert first_manifest["counts"] == json.loads(stage_1["build.json"])["counts"]
    assert (
        first_manifest["provenance"] == json.loads(stage_1["build.json"])["provenance"]
    )
    for path in website._modules().writer._DATA_PATHS:
        assert first[path] == stage_1[path]
    html = "\n".join(
        value.decode() for path, value in first.items() if path.endswith(".html")
    )
    assert "Recorded data — source commits are on Overview" not in html
    assert "Hermit makes Linux execution repeatable" in html
    assert "Build provenance" in html
    assert "Fixture data — not current Hermit main" not in html
    assert first_manifest["freshness_sha256"] == website._digest(
        {
            "generator_files_sha256": first_manifest["generator"]["files_sha256"],
            "stage_1_build_json_sha256": first_manifest["inputs"][
                "stage_1_build_json_sha256"
            ],
            "tree_sha256": first_manifest["tree_sha256"],
            "wording": "live",
        }
    )


def test_compose_preserves_the_exact_four_file_stage_1_tree() -> None:
    data = renderer_test.representative_build_data()
    stage_1 = website._modules().writer.render_tree(data)

    _manifest, files = website.compose(data)

    raw_paths = {
        "data/cells.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    }
    assert set(stage_1) == {"build.json", *raw_paths}
    assert all(files[path] == stage_1[path] for path in raw_paths)
    assert "incomplete-data/index.html" not in files


def test_compose_preserves_the_round_four_reader_facing_contract() -> None:
    data = renderer_test.representative_build_data()

    _manifest, files = website.compose(data)

    raw_names = ("cells.json.gz", "runs.json.gz", "tests.json.gz")
    for path, value in files.items():
        if not path.endswith(".html"):
            continue
        source = value.decode()
        links = renderer_test.Links()
        links.feed(source)
        assert "Raw Stage 1" not in source, path
        assert "data-raw-artifact" not in source, path
        assert "Test name" not in source, path
        assert not any(
            urlsplit(link).path.endswith(raw_names) for link in links.links
        ), path

    headings = renderer_test.ColumnHeadings()
    headings.feed(files["cells/index.html"].decode())
    matrix_labels = [column["label"] for column in headings.columns[:10]]
    assert matrix_labels == [
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
    assert "Selected by full" not in matrix_labels
    assert "Selected by path" not in matrix_labels

    one_page_lists = [
        files["index.html"].decode(),
        *(
            value.decode()
            for path, value in files.items()
            if path.startswith("runs/") and path.endswith(".html")
        ),
    ]
    assert one_page_lists
    assert all('data-list-pagination=' not in source for source in one_page_lists)

    for path in (
        "cells/detail.html",
        "cells/index.html",
        "cells/never-measured.html",
        "tests/detail.html",
    ):
        source = files[path].decode()
        assert 'data-load-state="loading"' in source, path
        assert "Loading verified local data…" in source, path
    javascript = files["assets/site.js"].decode()
    assert "['TestName', testName(cell.test, cell.category)]" in javascript
    assert "['Test name'," not in javascript
    assert "Verified local data loaded" not in javascript
    assert "status.textContent = '';" in javascript
    assert "status.hidden = true;" in javascript


def test_compose_validates_the_consolidated_template_inventory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    stage_1 = website._modules().writer.render_tree(data)
    stage_1_build = json.loads(stage_1["build.json"])
    rendered = website._modules().renderer.render_site(stage_1_build, data)

    manifest, files = website.compose(data)

    history_count = data["counts"]["validation_history_row_count"]
    assert len(rendered) == 29 + history_count
    assert len(files) == 33 + history_count
    assert len(manifest["artifacts"]) == 32 + history_count
    assert manifest["schema_version"] == 1
    assert manifest["generator"]["schema_version"] == 1
    assert manifest["inputs"]["stage_1_build_json"]["schema_version"] == 3
    raw_paths = {
        "data/cells.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    }
    for path in raw_paths:
        assert files[path] == stage_1[path]
        assert json.loads(gzip.decompress(files[path]))["schema_version"] == 3
    browser_manifest = json.loads(gzip.decompress(files["data/site-manifest.json.gz"]))
    assert browser_manifest["schema_version"] == 1
    assert {path for path in files if path.startswith("cells/")} == {
        "cells/detail.html",
        "cells/index.html",
        "cells/never-measured.html",
        "cells/projection-audit.html",
    }
    assert {path for path in files if path.startswith("tests/")} == {
        "tests/custom-commands.html",
        "tests/detail.html"
    }
    assert {path for path in files if path.startswith("data/")} == {
        *raw_paths,
        "data/site-manifest.json.gz",
        "data/site-summary.json.gz",
        *(f"data/detail-{value}.json.gz" for value in "0123456789abcdef"),
    }
    run_paths = {path for path in files if path.startswith("runs/")}
    assert "runs/index.html" in run_paths
    run_detail_paths = run_paths - {"runs/index.html"}
    assert len(run_detail_paths) == history_count
    assert all(
        path.startswith("runs/")
        and path.endswith(".html")
        and len(path.removeprefix("runs/").removesuffix(".html")) == 64
        and all(
            character in "0123456789abcdef"
            for character in path.removeprefix("runs/").removesuffix(".html")
        )
        for path in run_detail_paths
    )
    assert "runs/index.html" in website.FIXED_RENDERER_ARTIFACT_PATHS
    assert not website._is_run_artifact_path("runs/index.html")
    artifacts = {row["path"]: row for row in manifest["artifacts"]}
    assert set(artifacts) == set(files) - {"build.json"}
    for path, value in files.items():
        if path == "build.json":
            continue
        assert artifacts[path]["bytes"] == len(value)
        assert artifacts[path]["sha256"] == website._sha256(value)
    assert website._validate_manifest(manifest) == manifest


def _retarget_manifest_artifact(
    manifest: dict[str, Any], old: str, new: str
) -> dict[str, Any]:
    changed = copy.deepcopy(manifest)
    row = next(item for item in changed["artifacts"] if item["path"] == old)
    row["path"] = new
    row["content_type"], row["content_encoding"] = website._media(new)
    changed["artifacts"].sort(key=lambda item: item["path"])
    changed["artifacts_sha256"] = website._digest(changed["artifacts"])
    changed["directories"] = website._directory_rows(changed["artifacts"])
    changed["directories_sha256"] = website._digest(changed["directories"])
    changed["tree_sha256"] = website._digest(
        {
            "artifacts_sha256": changed["artifacts_sha256"],
            "directories_sha256": changed["directories_sha256"],
        }
    )
    changed["freshness_sha256"] = website._digest(
        {
            "generator_files_sha256": changed["generator"]["files_sha256"],
            "stage_1_build_json_sha256": changed["inputs"]["stage_1_build_json_sha256"],
            "tree_sha256": changed["tree_sha256"],
            "wording": changed["generator"]["wording"],
        }
    )
    return changed


@pytest.mark.parametrize(
    ("old", "new"),
    [
        ("cells/detail.html", f"cells/{'a' * 64}.html"),
        ("runs/index.html", f"runs/{'c' * 64}.html"),
        ("tests/detail.html", f"tests/{'b' * 64}.html"),
        ("data/detail-0.json.gz", "data/stale.json.gz"),
    ],
)
def test_manifest_refuses_identity_named_and_stale_artifacts(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    old: str,
    new: str,
) -> None:
    manifest, _files = website.compose(build_data(tmp_path, monkeypatch))
    changed = _retarget_manifest_artifact(manifest, old, new)

    with pytest.raises(website.WebsiteRefused, match="artifact inventory"):
        website._validate_manifest(changed)


def test_artifact_inventory_accepts_only_exact_cell_continuation_pages() -> None:
    counts = {
        "green_cell_count": 51,
        "red_cell_count": 50,
        "never_measured_cell_count": 101,
        "validation_history_row_count": 0,
    }
    continuation = {
        "cells/results-2.html",
        "cells/results-3.html",
        "cells/never-measured-2.html",
        "cells/never-measured-3.html",
    }
    paths = website.FIXED_SITE_ARTIFACT_PATHS | continuation

    website._require_exact_artifact_inventory(paths, counts)

    with pytest.raises(website.WebsiteRefused, match="artifact inventory"):
        website._require_exact_artifact_inventory(
            paths | {"cells/results-4.html"}, counts
        )
    with pytest.raises(website.WebsiteRefused, match="artifact inventory"):
        website._require_exact_artifact_inventory(
            paths - {"cells/never-measured-3.html"}, counts
        )


@pytest.mark.parametrize("population", ["green_cell_count", "never_measured_cell_count"])
def test_artifact_inventory_refuses_unbounded_continuation_counts_before_enumeration(
    monkeypatch: pytest.MonkeyPatch, population: str
) -> None:
    counts = {
        "green_cell_count": 0,
        "red_cell_count": 0,
        "never_measured_cell_count": 0,
        "validation_history_row_count": 0,
    }
    counts[population] = 10**18

    def refuse_enumeration(*_args: int) -> range:
        pytest.fail("impossible continuation inventory reached page enumeration")

    monkeypatch.setattr(website, "range", refuse_enumeration, raising=False)
    with pytest.raises(website.WebsiteRefused, match="artifact inventory"):
        website._require_exact_artifact_inventory(
            website.FIXED_SITE_ARTIFACT_PATHS, counts
        )


def test_manifest_requires_embedded_stage_1_schema_three(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest, _files = website.compose(build_data(tmp_path, monkeypatch))
    changed = copy.deepcopy(manifest)
    changed["inputs"]["stage_1_build_json"]["schema_version"] = 2
    stage_1_bytes = website._json_bytes(changed["inputs"]["stage_1_build_json"]) + b"\n"
    changed["inputs"]["stage_1_build_json_bytes"] = len(stage_1_bytes)
    changed["inputs"]["stage_1_build_json_sha256"] = website._sha256(stage_1_bytes)
    changed["freshness_sha256"] = website._digest(
        {
            "generator_files_sha256": changed["generator"]["files_sha256"],
            "stage_1_build_json_sha256": changed["inputs"]["stage_1_build_json_sha256"],
            "tree_sha256": changed["tree_sha256"],
            "wording": changed["generator"]["wording"],
        }
    )

    with pytest.raises(website.WebsiteRefused, match="Stage 1 build.json schema"):
        website._validate_manifest(changed)


def test_fixture_wording_remains_available_without_changing_non_html_bytes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    live_manifest, live = website.compose(data, wording="live")
    fixture_manifest, fixture = website.compose(data, wording="fixture")

    assert live_manifest["tree_sha256"] != fixture_manifest["tree_sha256"]
    assert live_manifest["freshness_sha256"] != fixture_manifest["freshness_sha256"]
    assert all(
        live[path] == value
        for path, value in fixture.items()
        if path != "build.json" and not path.endswith(".html")
    )
    fixture_html = "\n".join(
        value.decode() for path, value in fixture.items() if path.endswith(".html")
    )
    assert "Fixture data — not current Hermit main" in fixture_html


def test_historical_compose_is_self_validating_and_discloses_every_page(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = historical_build_data(tmp_path, monkeypatch)

    manifest, files = website.compose(data, wording="historical")

    assert manifest["generator"]["wording"] == "historical"
    assert website._validate_manifest(manifest) == manifest
    historical = manifest["provenance"]["historical"]
    canonical = manifest["provenance"]["canonical_input_provenance"]
    linked_facts = (
        (
            "Generator-base-recorded Hermit gitlink",
            "hermit",
            canonical["reader_hermit"]["gitlink_commit"],
        ),
        ("Ledger commit", "dev-hermit", historical["ledger_commit"]),
        (
            "Measured Hermit commit",
            "hermit",
            historical["measured_hermit_commit"],
        ),
        (
            "Measured Reverie gitlink",
            "reverie",
            historical["measured_reverie_commit"],
        ),
        (
            "Manifest source commit",
            "hermit",
            historical["manifest_source_commit"],
        ),
    )
    unlinked_facts = (
        ("Generator parent commit", canonical["reader_commit"]),
        ("Generator parent source state", "clean"),
        (
            "Clean generator Hermit checkout",
            canonical["reader_hermit"]["checkout_commit"],
        ),
        ("Ledger reference", historical["ledger_ref"]),
        ("Ledger-era Hermit gitlink", manifest["provenance"]["hermit_main_commit"]),
        ("Source validation record", historical["record_id"]),
        ("Source validation run", historical["run_id"]),
        ("Validate path", historical["validate_path"]),
        (
            f"Retained run selected by {historical['validate_path']}",
            f"{historical['selected_by_full_cell_count']} cells",
        ),
        ("Run started", "2026-08-30 20:00:00 ET"),
        ("Run finished", "2026-08-30 20:01:00 ET"),
        ("Source history row SHA-256", historical["history_row_sha256"]),
        ("Measurement host", historical["measurement_host"]),
        ("Measurement kernel", historical["kernel_version"]),
        (
            "Host capability set",
            "cpuid-faulting: present (fixture); kvm: present (fixture)",
        ),
        (
            "Manifest and measured commits",
            "manifest source differs from measured Hermit",
        ),
        ("Reader files SHA-256", canonical["reader_files_sha256"]),
        ("Reader identity SHA-256", canonical["reader_source_identity_sha256"]),
        ("Captured source inventory SHA-256", canonical["sources_sha256"]),
    )
    historical_pages = {
        path: value.decode()
        for path, value in files.items()
        if path.endswith(".html")
    }
    assert historical_pages
    for path, value in files.items():
        if path.endswith(".html"):
            text = value.decode()
            title = text.split("<title>", 1)[1].split("</title>", 1)[0]
            assert title.endswith(" · Hermit compatibility snapshot"), path
            assert "historical" not in title.lower(), path
            assert "NOT CURRENT — historical compatibility snapshot" not in text, path
            assert "A navigable historical snapshot" not in text, path
            assert "not current" not in text.lower(), path
            assert "immutable" not in text.lower(), path
            assert '<p class="fixture-banner">' not in text, path
            assert (
                "<summary>Full snapshot provenance (2026-08-30 20:01 ET)</summary>"
                in text
            ), path
            assert "Current reproducer" not in text, path
            disclosure = text.split('<details class="snapshot-disclosure">', 1)[1]
            disclosure = disclosure.split("</details>", 1)[0]
            assert text.count('<details class="snapshot-disclosure">') == 1, path
            assert "Build provenance" not in text, path
            assert disclosure.count('href="https://github.com/rrnewton/') == 5, path
            for label, repository, commit in linked_facts:
                assert (
                    f'<div><dt>{label}</dt><dd><a class="external-link" '
                    f'href="https://github.com/rrnewton/{repository}/commit/{commit}" '
                    f'aria-label="{commit} on GitHub (external link)">{commit}'
                    '<span class="external-link-indicator" aria-hidden="true">↗</span>'
                    "</a></dd></div>"
                    in text
                ), (path, label)
            for label, fact in unlinked_facts:
                assert f"<div><dt>{label}</dt><dd>{fact}</dd></div>" in text, (
                    path,
                    label,
                )
    landing = historical_pages["index.html"]
    assert "A navigable snapshot of recorded compatibility evidence." not in landing
    assert (
        "The Hermit container enables deterministic execution of guest Linux "
        "programs."
        in landing
    )
    assert (
        'href="https://github.com/rrnewton/hermit/blob/main/README.md">README</a>'
        in landing
    )
    assert (
        'href="https://developers.facebook.com/blog/post/2022/11/22/'
        'hermit-deterministic-linux-testing/">introduction in the blog post</a>'
        in landing
    )
    for explanation in (
        "each identified by a test ID",
        "test ID × mode × backend",
        "deterministic execution, record/replay, or chaos concurrency testing",
        "A backend identifies the Reverie implementation",
        "Historical measurement",
        "This page is not a live status view",
        "source validation run finished",
        "not the timestamp of every cell result",
        "qualifying measurements retained in this artifact from multiple runs",
        "KVM and LiteInst in this artifact",
        "not changes since an earlier snapshot",
        "not counts of cross-backend matches",
    ):
        assert explanation in landing
    assert landing.index("The Hermit container enables") < landing.index(
        '<section id="overview">'
    )
    assert all(
        "A navigable snapshot of recorded compatibility evidence." not in text
        for path, text in historical_pages.items()
        if path != "index.html"
    )


@pytest.mark.parametrize(
    ("finished_at", "expected"),
    [
        ("2026-07-01T16:34:56.123Z", "2026-07-01 12:34 ET"),
        ("2026-01-01T16:34:56+00:00", "2026-01-01 11:34 ET"),
    ],
)
def test_historical_disclosure_uses_the_source_run_finish_in_eastern_time(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    finished_at: str,
    expected: str,
) -> None:
    data = historical_build_data(tmp_path, monkeypatch)
    set_historical_finished_at(data, finished_at)

    _manifest, files = website.compose(data, wording="historical")

    for path, value in files.items():
        if path.endswith(".html"):
            assert (
                f"<summary>Full snapshot provenance ({expected})</summary>"
                in value.decode()
            ), path


@pytest.mark.parametrize(
    "finished_at",
    [
        None,
        "2026-07-01 16:34:56Z",
        "2026-07-01T16:34:56-04:00",
        "2026-13-01T16:34:56Z",
    ],
)
def test_historical_compose_refuses_missing_or_non_utc_rfc3339_finished_at(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    finished_at: str | None,
) -> None:
    data = historical_build_data(tmp_path, monkeypatch)
    set_historical_finished_at(data, finished_at)

    with pytest.raises(website.WebsiteRefused, match="finished_at"):
        website.compose(data, wording="historical")


@pytest.mark.parametrize(
    "field",
    (
        "measurement_host",
        "measured_reverie_commit",
        "kernel_version",
        "host_capabilities",
    ),
)
@pytest.mark.parametrize("location", ("historical", "canonical"))
def test_historical_compose_refuses_missing_measurement_provenance(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    field: str,
    location: str,
) -> None:
    data = historical_build_data(tmp_path, monkeypatch)
    source = data["provenance"]["historical"]
    if location == "canonical":
        source = data["provenance"]["canonical_input_provenance"]["historical"]
    source.pop(field)

    with pytest.raises(website.WebsiteRefused, match=field):
        website.compose(data, wording="historical")


def test_compose_requires_historical_wording_exactly_for_historical_stage_1(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    live_root = tmp_path / "live"
    historical_root = tmp_path / "historical"
    live_root.mkdir()
    historical_root.mkdir()
    live = build_data(live_root, monkeypatch)
    historical = historical_build_data(historical_root, monkeypatch)

    with pytest.raises(website.WebsiteRefused, match="historical wording"):
        website.compose(live, wording="historical")
    with pytest.raises(website.WebsiteRefused, match="historical wording"):
        website.compose(historical, wording="live")


@pytest.mark.parametrize(
    ("historical", "changed_wording"),
    [(False, "historical"), (True, "live")],
)
def test_sealed_manifest_requires_historical_wording_exactly_for_stage_1(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    historical: bool,
    changed_wording: str,
) -> None:
    data = (
        historical_build_data(tmp_path, monkeypatch)
        if historical
        else build_data(tmp_path, monkeypatch)
    )
    wording = "historical" if historical else "live"
    manifest, _files = website.compose(data, wording=wording)
    changed = copy.deepcopy(manifest)
    changed["generator"]["wording"] = changed_wording
    changed["freshness_sha256"] = website._digest(
        {
            "generator_files_sha256": changed["generator"]["files_sha256"],
            "stage_1_build_json_sha256": changed["inputs"]["stage_1_build_json_sha256"],
            "tree_sha256": changed["tree_sha256"],
            "wording": changed_wording,
        }
    )

    with pytest.raises(website.WebsiteRefused, match="historical wording"):
        website._validate_manifest(changed)


def test_stage_1_reader_inventory_matches_the_exporter_exactly() -> None:
    export_inputs = website._modules().build_data._modules().export_inputs
    declared = tuple(path.as_posix() for path in export_inputs._READER_PATHS)

    assert website.STAGE_1_READER_PATHS == declared


@pytest.mark.parametrize("series_commit", [None, "c" * 40])
def test_historical_build_uses_only_the_separate_source_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, series_commit: str | None,
) -> None:
    data = historical_build_data(tmp_path, monkeypatch)
    modules = website._modules()
    calls: list[tuple[Any, ...]] = []

    def source(*arguments: Any, **keywords: Any) -> dict[str, Any]:
        assert keywords == ({"series_commit": series_commit} if series_commit else {})
        calls.append(arguments)
        return copy.deepcopy(data)

    monkeypatch.setattr(modules.build_data, "build_historical_data", source)
    monkeypatch.setattr(
        website,
        "_live_data",
        lambda: pytest.fail("historical build called the live data path"),
    )
    expected = object()

    def publish(
        value: Any,
        output_root: Path,
        *,
        wording: str,
        verify_sources: Any,
        presentation: dict[str, Any],
    ) -> object:
        assert value == data
        assert output_root == tmp_path / "site"
        assert wording == "historical"
        assert modules.renderer.validate_presentation(presentation, data) == presentation
        verify_sources()
        return expected

    monkeypatch.setattr(website, "publish", publish)
    result = website.build_historical(
        tmp_path / "parent",
        "a" * 40,
        "record-1",
        "run-1",
        tmp_path / "manifest-hermit",
        "b" * 40,
        tmp_path / "site",
        **({"series_commit": series_commit} if series_commit else {}),
    )

    assert result is expected
    assert calls == [
        (
            tmp_path / "parent",
            "a" * 40,
            "record-1",
            "run-1",
            tmp_path / "manifest-hermit",
            "b" * 40,
        ),
        (
            tmp_path / "parent",
            "a" * 40,
            "record-1",
            "run-1",
            tmp_path / "manifest-hermit",
            "b" * 40,
        ),
    ]


def test_freshness_changes_with_generator_asset_and_stage1_input(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    baseline_manifest, baseline_files = website.compose(data)
    identities = website._generator_files()
    changed_identities = copy.deepcopy(identities)
    changed_identities[0]["sha256"] = "f" * 64
    stage_1 = baseline_manifest["inputs"]["stage_1_build_json"]
    stage_1_bytes = website._json_bytes(stage_1) + b"\n"
    code_manifest = website._manifest(
        stage_1,
        stage_1_bytes,
        {path: value for path, value in baseline_files.items() if path != "build.json"},
        changed_identities,
        wording="live",
    )

    assert code_manifest["tree_sha256"] == baseline_manifest["tree_sha256"]
    assert code_manifest["freshness_sha256"] != baseline_manifest["freshness_sha256"]

    renderer = website._modules().renderer
    original_css = renderer._site_css
    monkeypatch.setattr(renderer, "_site_css", lambda: original_css() + b"\nbody{}\n")
    asset_manifest, _asset_files = website.compose(data)
    assert asset_manifest["tree_sha256"] != baseline_manifest["tree_sha256"]
    assert asset_manifest["freshness_sha256"] != baseline_manifest["freshness_sha256"]

    changed_stage_1 = copy.deepcopy(baseline_manifest["inputs"]["stage_1_build_json"])
    changed_stage_1["provenance"]["manifest_sha256"] = "e" * 64
    changed_bytes = website._json_bytes(changed_stage_1) + b"\n"
    input_manifest = website._manifest(
        changed_stage_1,
        changed_bytes,
        {path: value for path, value in baseline_files.items() if path != "build.json"},
        identities,
        wording="live",
    )
    assert input_manifest["tree_sha256"] == baseline_manifest["tree_sha256"]
    assert input_manifest["freshness_sha256"] != baseline_manifest["freshness_sha256"]


def test_compose_refuses_generator_change_between_its_two_identity_reads(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    original = website._generator_files
    before = original()
    after = copy.deepcopy(before)
    after[0]["sha256"] = "f" * 64
    observations = iter((before, after))
    monkeypatch.setattr(website, "_generator_files", lambda: next(observations))

    with pytest.raises(website.WebsiteRefused, match="changed during composition"):
        website.compose(data)


def test_generator_read_detects_same_inode_metadata_change(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = tmp_path / "generator.py"
    path.write_bytes(b"same bytes")
    original = website.os.fstat
    calls = 0

    def changed(descriptor: int) -> Any:
        nonlocal calls
        observed = original(descriptor)
        calls += 1
        if calls != 2:
            return observed
        return SimpleNamespace(
            st_dev=observed.st_dev,
            st_ino=observed.st_ino,
            st_mode=observed.st_mode,
            st_size=observed.st_size,
            st_mtime_ns=observed.st_mtime_ns + 1,
            st_ctime_ns=observed.st_ctime_ns + 1,
        )

    monkeypatch.setattr(website.os, "fstat", changed)
    with pytest.raises(website.WebsiteRefused, match="changed while being read"):
        website._regular_bytes(path)


def test_captured_stage1_reader_inventory_refuses_an_added_reader() -> None:
    captured = website._capture_generator_bytes()
    relative = "ci-hub/compatibility-website/export_inputs.py"
    source = captured[relative]
    closing_row = b'    Path("ci-hub/validate/service_result.py"),\n)'
    assert source.count(closing_row) == 1
    changed = source.replace(
        closing_row,
        b'    Path("ci-hub/validate/service_result.py"),\n'
        b'    Path("ci-hub/validate/future-reader.py"),\n)',
    )
    name = "compatibility_website_combined_added_reader_test"
    export_inputs = website._load_module(name, website.REPOSITORY / relative, changed)
    try:
        with pytest.raises(website.WebsiteRefused, match="inventory is not exact"):
            website._bind_export_authorities(export_inputs, captured, {})
    finally:
        sys.modules.pop(name, None)


def test_captured_stage1_reader_chain_uses_the_real_captured_modules() -> None:
    before_modules = {
        name: sys.modules.get(name) for name in CAPTURED_READER_MODULE_NAMES
    }
    present_modules = {
        name for name in CAPTURED_READER_MODULE_NAMES if name in sys.modules
    }
    before_path = list(sys.path)
    readers = website._captured_reader_modules(website._capture_generator_bytes())

    assert readers["rust_toolchain"].capture is readers["bounded_capture"].capture
    assert readers["series"].rust_toolchain is readers["rust_toolchain"]
    assert readers["flake_class"].flaky_registry is readers["flaky_cells"]
    assert (
        readers["gate_completeness"].gate_counts is readers["flake_class"].gate_counts
    )
    qualifying = readers["qualifying_receipt"]
    assert readers["admission_paths"].qualifying_receipt is qualifying
    assert readers["validate_rows"].AdmissionPaths is readers["admission_paths"].AdmissionPaths
    assert qualifying.CellResultsVerdict.SATISFIED.value == "satisfied"
    assert callable(qualifying.row_qualification)
    assert sys.path == before_path
    for name in CAPTURED_READER_MODULE_NAMES:
        assert (name in sys.modules) is (name in present_modules)
        if name in present_modules:
            assert sys.modules[name] is before_modules[name]


def test_captured_reader_graph_uses_one_full_qualifying_receipt_module() -> None:
    captured = website._capture_generator_bytes()
    relative = "ci-hub/ledger/validate_rows.py"
    source = captured[relative]
    needle = b"import qualifying_receipt  # noqa: E402\n"
    assert source.count(needle) == 1
    before_modules = {
        name: sys.modules.get(name) for name in CAPTURED_READER_MODULE_NAMES
    }
    present_modules = {
        name for name in CAPTURED_READER_MODULE_NAMES if name in sys.modules
    }
    before_path = list(sys.path)

    readers = website._captured_reader_modules(captured)
    assert callable(readers["qualifying_receipt"].row_qualification)
    assert sys.path == before_path
    for name in CAPTURED_READER_MODULE_NAMES:
        assert (name in sys.modules) is (name in present_modules)
        if name in present_modules:
            assert sys.modules[name] is before_modules[name]
    website._captured_reader_modules(captured)
    assert sys.path == before_path
    for name in CAPTURED_READER_MODULE_NAMES:
        assert (name in sys.modules) is (name in present_modules)
        if name in present_modules:
            assert sys.modules[name] is before_modules[name]


@pytest.mark.parametrize("relative", [
    "ci-hub/compatibility-website/build_data.py", "ci-hub/lib/rust_toolchain.py",
])
def test_preloaded_generator_refuses_a_later_dependency_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, relative: str,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    website._modules.cache_clear()
    website._modules()
    changed = website._capture_generator_bytes()
    changed[relative] += b"\n# replaced after module load\n"
    monkeypatch.setattr(website, "_capture_generator_bytes", lambda: changed)

    with pytest.raises(
        website.WebsiteRefused, match="differ from the captured executable bytes"
    ):
        website.compose(data)


def test_generator_capture_refuses_when_self_differs_from_imported_bytes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured = website._capture_generator_bytes()
    captured[website.COORDINATOR_PATH] += b"\n# replaced after import\n"
    monkeypatch.setattr(website, "_capture_generator_bytes", lambda: captured)
    website._modules.cache_clear()

    with pytest.raises(website.WebsiteRefused, match="coordinator bytes changed"):
        website._modules()


def test_entrypoint_refuses_self_replacement_after_bootstrap_capture(
    tmp_path: Path,
) -> None:
    repository = tmp_path / "repository"
    shutil.copytree(REPOSITORY / "ci-hub", repository / "ci-hub")
    script = repository / "ci-hub/compatibility-website/build_and_serve.py"
    original = script.read_bytes()
    replacement = tmp_path / "replacement.py"
    replacement.write_bytes(original + b"\n# replacement after bootstrap capture\n")
    hooks = tmp_path / "hooks"
    hooks.mkdir()
    (hooks / "sitecustomize.py").write_text(
        """import os
import sys

target = os.path.realpath(os.environ['WEBSITE_SELF_SWAP_TARGET'])
replacement = os.environ['WEBSITE_SELF_SWAP_REPLACEMENT']
swapped = False

def audit(event, arguments):
    global swapped
    if swapped or event != 'compile' or len(arguments) < 2:
        return
    filename = arguments[1]
    if not isinstance(filename, str) or os.path.realpath(filename) != target:
        return
    try:
        caller = os.path.realpath(sys._getframe(1).f_code.co_filename)
    except (AttributeError, ValueError):
        return
    if caller != target:
        return
    swapped = True
    os.replace(replacement, target)

sys.addaudithook(audit)
"""
    )
    output_root = tmp_path / "must-not-exist"
    environment = os.environ.copy()
    environment.update(
        PYTHONPATH=str(hooks),
        WEBSITE_SELF_SWAP_REPLACEMENT=str(replacement),
        WEBSITE_SELF_SWAP_TARGET=str(script),
    )
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(script),
                "build",
                "--output-root",
                str(output_root),
            ],
            cwd=repository,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
    finally:
        script.write_bytes(original)

    assert script.read_bytes() == original
    assert result.returncode == 2
    assert result.stdout == ""
    assert "executing website coordinator bytes changed before capture" in result.stderr
    assert not output_root.exists()


def test_generator_capture_refuses_a_dependency_changed_only_during_load(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    before = website._capture_generator_bytes()
    during = dict(before)
    relative = "ci-hub/compatibility-website/build_data.py"
    during[relative] += b"\n# transient replacement\n"
    observations = iter((before, during, before))
    monkeypatch.setattr(website, "_capture_generator_bytes", lambda: next(observations))
    website._modules.cache_clear()

    with pytest.raises(website.WebsiteRefused, match="changed while being captured"):
        website.compose(data)


def test_qualifying_receipt_reads_the_captured_policy_not_its_live_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured = website._capture_generator_bytes()
    qualifying = website._captured_reader_modules(captured)["qualifying_receipt"]

    def refuse_live_read(*_args: Any, **_kwargs: Any) -> str:
        raise AssertionError("qualifying receipt policy was read from its live path")

    monkeypatch.setattr(Path, "read_text", refuse_live_read)
    assert qualifying.active() == json.loads(
        captured["ci-hub/validate/qualifying-receipt.json"]
    )


def test_build_data_has_no_private_qualifying_receipt_bridge(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    builder = website._modules().build_data
    assert not hasattr(builder._modules(), "qualifying_receipt")
    source = website._capture_generator_bytes()[
        "ci-hub/compatibility-website/build_data.py"
    ]
    assert b"_RFC3339_Z" not in source
    assert b"cell_results_verdict" not in source
    assert data["runs"]["current_validation_run_rows"]


@pytest.mark.parametrize("variable", website.QUALIFICATION_OVERRIDE_ENVIRONMENT)
def test_build_reader_refuses_qualification_authority_overrides(
    monkeypatch: pytest.MonkeyPatch, variable: str
) -> None:
    monkeypatch.setenv(variable, "/fixture/external-authority")
    website._modules.cache_clear()
    with pytest.raises(website.WebsiteRefused, match="authority override"):
        website._modules()


def test_generator_capture_refuses_a_policy_restored_after_capture(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    before = website._capture_generator_bytes()
    during = dict(before)
    relative = "ci-hub/validate/qualifying-receipt.json"
    during[relative] += b"\n"
    observations = iter((during, before))
    monkeypatch.setattr(website, "_capture_generator_bytes", lambda: next(observations))
    website._modules.cache_clear()

    with pytest.raises(website.WebsiteRefused, match="changed while being captured"):
        website._modules()


def test_embedded_stage1_manifest_must_match_its_validated_data(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    changed_stage_1 = copy.deepcopy(manifest["inputs"]["stage_1_build_json"])
    changed_stage_1["build_data_sha256"] = "f" * 64
    changed_stage_1_bytes = website._json_bytes(changed_stage_1) + b"\n"
    pages = website._modules().renderer.render_site(
        changed_stage_1, data, wording="live"
    )
    changed_files = {
        path: value
        for path, value in files.items()
        if path != "build.json" and not path.endswith(".html")
    }
    changed_files.update(pages)
    changed_manifest = website._manifest(
        changed_stage_1,
        changed_stage_1_bytes,
        changed_files,
        website._generator_files(),
        wording="live",
    )

    with pytest.raises(website.WebsiteRefused, match="embedded Stage 1 build.json"):
        website._reproduce_expected_bytes(changed_manifest, changed_files)


def test_manifest_refuses_a_missing_consolidated_artifact(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, _files = website.compose(data)
    changed = copy.deepcopy(manifest)
    changed["artifacts"] = changed["artifacts"][:-1]
    changed["artifacts_sha256"] = website._digest(changed["artifacts"])
    changed["directories"] = website._directory_rows(changed["artifacts"])
    changed["directories_sha256"] = website._digest(changed["directories"])
    changed["tree_sha256"] = website._digest(
        {
            "artifacts_sha256": changed["artifacts_sha256"],
            "directories_sha256": changed["directories_sha256"],
        }
    )
    changed["freshness_sha256"] = website._digest(
        {
            "generator_files_sha256": changed["generator"]["files_sha256"],
            "stage_1_build_json_sha256": changed["inputs"]["stage_1_build_json_sha256"],
            "tree_sha256": changed["tree_sha256"],
            "wording": changed["generator"]["wording"],
        }
    )

    with pytest.raises(website.WebsiteRefused, match="artifact inventory"):
        website._validate_manifest(changed)


def test_publish_reuses_only_the_exact_valid_tree_without_touching_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    first = website.publish(data, output_root)
    path = first.path
    first.close()
    before = snapshot(path)
    root_before = path.stat()

    second = website.publish(data, output_root)
    assert second.reused is True
    assert second.path == path
    second.close()
    root_after = path.stat()

    assert snapshot(path) == before
    assert (root_after.st_dev, root_after.st_ino, root_after.st_mtime_ns) == (
        root_before.st_dev,
        root_before.st_ino,
        root_before.st_mtime_ns,
    )
    validated = website.validate_website(path)
    assert validated.manifest["freshness_sha256"] == path.name
    validated.close()


def test_semantic_reproduction_is_reused_only_within_one_validation_invocation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    original = website._reproduce_expected_bytes
    calls = []

    def reproduce(manifest: Any, files: Any) -> None:
        calls.append(manifest["freshness_sha256"])
        original(manifest, files)

    monkeypatch.setattr(website, "_reproduce_expected_bytes", reproduce)
    built = website.publish(data, tmp_path / "published")
    try:
        built.recheck()
        assert calls == [built.path.name]
        # A separate validation is not allowed to inherit this invocation's proof.
        separate = website.validate_website(built.path)
        try:
            separate.recheck()
            assert calls == [built.path.name, built.path.name]
        finally:
            separate.close()
    finally:
        built.close()


def test_semantic_reuse_rejects_same_path_changed_bytes_with_restored_metadata(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    try:
        artifact = built.path / "index.html"
        before = artifact.stat()
        original = artifact.read_bytes()
        artifact.chmod(0o644)
        artifact.write_bytes(bytes([original[0] ^ 1]) + original[1:])
        artifact.chmod(0o444)
        os.utime(artifact, ns=(before.st_atime_ns, before.st_mtime_ns))
        after = artifact.stat()
        assert (before.st_ino, before.st_size, before.st_mtime_ns) == (
            after.st_ino, after.st_size, after.st_mtime_ns
        )
        with pytest.raises(website.WebsiteRefused, match="artifact bytes disagree"):
            built.recheck()
    finally:
        built.close()


def test_semantic_reuse_does_not_admit_rehashed_changed_content_or_cache_a_refusal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    try:
        manifest = built.manifest
        files = {row["path"]: (built.path / row["path"]).read_bytes()
                 for row in manifest["artifacts"]}
        files["index.html"] += b"\nchanged despite self-consistent artifact hashes\n"
        stage = manifest["inputs"]["stage_1_build_json"]
        changed = website._manifest(
            stage, website._json_bytes(stage) + b"\n", files,
            manifest["generator"]["files"], wording=manifest["generator"]["wording"],
        )
        assert website._validate_manifest(changed) == changed
        for name, value in (("index.html", files["index.html"]),
                            ("build.json", website._json_bytes(changed) + b"\n")):
            path = built.path / name
            path.chmod(0o644)
            path.write_bytes(value)
            path.chmod(0o444)
        original = website._reproduce_expected_bytes
        attempts = []

        def reproduce(manifest: Any, files: Any) -> None:
            attempts.append(manifest["freshness_sha256"])
            original(manifest, files)

        monkeypatch.setattr(website, "_reproduce_expected_bytes", reproduce)
        for _ in range(2):
            # Ignore only the directory name to reach the exact semantic boundary;
            # all canonical manifest, file content and inventory checks still run.
            with pytest.raises(website.WebsiteRefused, match="fresh exact reproduction"):
                website._read_open_tree(
                    built.descriptor, built.path, require_name=False,
                    semantic_validation=built.semantic_validation,
                )
        assert attempts == [changed["freshness_sha256"]] * 2
    finally:
        built.close()


def test_semantic_reuse_refuses_a_changed_generator_capture(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    captured = website._capture_generator_bytes()
    captured["ci-hub/compatibility-website/assets/favicon.svg"] += b"\n"
    try:
        with monkeypatch.context() as patch:
            patch.setattr(website, "_capture_generator_bytes", lambda: captured)
            website._modules.cache_clear()
            with pytest.raises(website.WebsiteRefused, match="generator capture changed"):
                built.recheck()
    finally:
        website._modules.cache_clear()
        built.close()


def test_published_tree_has_read_only_files_and_directories(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    built = website.publish(data, tmp_path / "published")
    root = built.path
    built.close()

    assert stat.S_IMODE(root.stat().st_mode) == 0o555
    for path in root.rglob("*"):
        expected = 0o555 if path.is_dir() else 0o444
        assert stat.S_IMODE(path.stat().st_mode) == expected, path
    if os.geteuid() != 0:
        with pytest.raises(PermissionError):
            os.open(root / "index.html", os.O_WRONLY)
        with pytest.raises(PermissionError):
            os.open(root / "new-file", os.O_WRONLY | os.O_CREAT, 0o644)


@pytest.mark.parametrize(
    ("relative", "changed_mode", "expected_mode", "message"),
    [
        ("index.html", 0o644, 0o444, "not a read-only regular file"),
        ("assets", 0o755, 0o555, "mode is not read-only"),
        (".", 0o755, 0o555, "mode is not read-only"),
    ],
)
def test_mode_tampering_refuses_validation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    relative: str,
    changed_mode: int,
    expected_mode: int,
    message: str,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    built = website.publish(data, tmp_path / "published")
    root = built.path
    built.close()
    changed = root / relative
    changed.chmod(changed_mode)
    try:
        with pytest.raises(website.WebsiteRefused, match=message):
            website.validate_website(root)
    finally:
        changed.chmod(expected_mode)


def test_direct_serve_refuses_a_writable_output_parent(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    target = built.path
    built.close()
    target.parent.chmod(0o777)
    try:
        with pytest.raises(website.WebsiteRefused, match="parent permits other"):
            website.validate_website(target)
    finally:
        target.parent.chmod(0o755)


@pytest.mark.parametrize("location", ["parent", "root", "directory", "artifact"])
def test_direct_serve_refuses_foreign_owned_tree_entries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    location: str,
) -> None:
    class ForeignOwner:
        def __init__(self, metadata: os.stat_result):
            self._metadata = metadata
            self.st_uid = os.geteuid() + 1

        def __getattr__(self, name: str) -> Any:
            return getattr(self._metadata, name)

    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    target = built.path
    built.close()
    selected = {
        "parent": target.parent,
        "root": target,
        "directory": target / "assets",
    }.get(location)
    original_fstat = website.os.fstat
    writer = website._modules().writer
    original_entry = writer._entry

    def foreign_fstat(descriptor: int) -> Any:
        metadata = original_fstat(descriptor)
        try:
            opened = Path(os.readlink(f"/proc/self/fd/{descriptor}"))
        except OSError:
            return metadata
        return ForeignOwner(metadata) if opened == selected else metadata

    def foreign_entry(directory: int, name: str) -> Any:
        metadata = original_entry(directory, name)
        if location == "artifact" and name == "index.html" and metadata is not None:
            return ForeignOwner(metadata)
        return metadata

    monkeypatch.setattr(website.os, "fstat", foreign_fstat)
    monkeypatch.setattr(writer, "_entry", foreign_entry)
    with pytest.raises(website.WebsiteRefused, match="owned by the current user"):
        website.validate_website(target)


@pytest.mark.skipif(os.geteuid() != 0, reason="real foreign ownership requires euid 0")
@pytest.mark.parametrize("location", ["parent", "root", "artifact"])
def test_direct_serve_refuses_real_foreign_owned_entries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    location: str,
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    target = built.path
    built.close()
    changed = {
        "parent": target.parent,
        "root": target,
        "artifact": target / "index.html",
    }[location]
    os.chown(changed, 1, -1)
    try:
        with pytest.raises(website.WebsiteRefused, match="owned by the current user"):
            website.validate_website(target)
    finally:
        os.chown(changed, 0, -1)


@pytest.mark.parametrize("failure", ["unexpected", "signal", "keyboard"])
def test_open_validation_closes_target_descriptor_for_every_exception(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    failure: str,
) -> None:
    class UnexpectedFailure(Exception):
        pass

    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    target = built.path
    built.close()
    parent = os.open(
        target.parent,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    writer = website._modules().writer
    original_open = writer._open_directory
    opened: list[int] = []

    def capture_open(directory: int, name: str) -> int:
        descriptor = original_open(directory, name)
        opened.append(descriptor)
        return descriptor

    error: BaseException
    if failure == "signal":
        error = website.WebsiteSignal(signal.SIGTERM)
    elif failure == "keyboard":
        error = KeyboardInterrupt()
    else:
        error = UnexpectedFailure("injected validation failure")

    monkeypatch.setattr(writer, "_open_directory", capture_open)
    monkeypatch.setattr(
        website,
        "_read_open_tree",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(error),
    )
    try:
        with pytest.raises(type(error)):
            website._open_validated_at(parent, target.name, target)
    finally:
        os.close(parent)
    assert len(opened) == 1
    with pytest.raises(OSError) as closed:
        os.fstat(opened[0])
    assert closed.value.errno == errno.EBADF


def test_output_root_swap_before_publish_refuses_and_preserves_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    original = website._read_open_tree
    held = output.path.with_name("published-held")
    swapped = False

    def swap_after_candidate(*args: Any, **kwargs: Any) -> Any:
        nonlocal swapped
        result = original(*args, **kwargs)
        if kwargs.get("require_name") is False and not swapped:
            output.path.rename(held)
            output.path.mkdir()
            (output.path / "valuable").write_bytes(b"keep")
            swapped = True
        return result

    monkeypatch.setattr(website, "_read_open_tree", swap_after_candidate)
    try:
        with pytest.raises(website.WebsiteRefused, match="output path identity"):
            website._materialize(output, manifest, files)
    finally:
        output.close()
    assert (tmp_path / "published" / "valuable").read_bytes() == b"keep"
    retained = list(held.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700


def test_output_parent_swap_between_overlap_check_and_open_refuses(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    parent = tmp_path / "parent"
    parent.mkdir()
    held = tmp_path / "parent-held"
    original = website._output_location

    def swap(value: Path) -> Any:
        result = original(value)
        parent.rename(held)
        parent.mkdir()
        return result

    monkeypatch.setattr(website, "_output_location", swap)
    with pytest.raises(website.WebsiteRefused, match="parent changed"):
        website._lock_output_root(parent / "published")
    assert list(parent.iterdir()) == []
    assert list(held.iterdir()) == []


def test_output_root_marker_added_while_waiting_for_lock_refuses(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output_root = tmp_path / "published"
    output_root.mkdir()
    real_flock = website.fcntl.flock

    def add_marker(descriptor: int, operation: int) -> None:
        if operation == fcntl.LOCK_EX:
            (output_root / ".git").mkdir()
        real_flock(descriptor, operation)

    monkeypatch.setattr(website.fcntl, "flock", add_marker)
    with pytest.raises(website.WebsiteRefused, match="became a checkout"):
        website._lock_output_root(output_root)
    assert (output_root / ".git").is_dir()


def test_output_root_and_parent_must_not_be_writable_by_other_users(
    tmp_path: Path,
) -> None:
    unsafe_parent = tmp_path / "unsafe-parent"
    unsafe_parent.mkdir()
    unsafe_parent.chmod(0o777)
    output_root = unsafe_parent / "published"
    try:
        with pytest.raises(website.WebsiteRefused, match="parent permits other"):
            website._lock_output_root(output_root)
        assert not output_root.exists()
    finally:
        unsafe_parent.chmod(0o700)

    output_root = tmp_path / "published"
    output_root.mkdir()
    output_root.chmod(0o777)
    try:
        with pytest.raises(website.WebsiteRefused, match="root is writable by other"):
            website._lock_output_root(output_root)
        assert list(output_root.iterdir()) == []
    finally:
        output_root.chmod(0o755)


def test_direct_tmp_output_root_refuses_before_creation() -> None:
    metadata = Path("/tmp").stat()
    if metadata.st_uid == os.geteuid() and not metadata.st_mode & (
        stat.S_IWGRP | stat.S_IWOTH
    ):
        pytest.skip("host /tmp is current-user-owned and not writable by others")
    output_root = Path("/tmp") / f"compatibility-website-permission-{uuid.uuid4().hex}"
    assert not output_root.exists()

    with pytest.raises(website.WebsiteRefused, match="output parent"):
        website._lock_output_root(output_root)
    assert not output_root.exists()


def test_foreign_owned_output_parent_refuses_before_creation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent = tmp_path / "foreign-parent"
    parent.mkdir()
    output_root = parent / "published"
    original = Path.stat

    def foreign_owner(path: Path, *args: Any, **kwargs: Any) -> Any:
        observed = original(path, *args, **kwargs)
        if path != parent:
            return observed
        return SimpleNamespace(
            st_dev=observed.st_dev,
            st_ino=observed.st_ino,
            st_mode=observed.st_mode,
            st_uid=os.geteuid() + 1,
        )

    monkeypatch.setattr(Path, "stat", foreign_owner)
    with pytest.raises(website.WebsiteRefused, match="parent is not owned"):
        website._lock_output_root(output_root)
    assert not output_root.exists()


def test_output_root_swap_before_reuse_return_refuses_and_preserves_both_trees(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    built = website.publish(data, output_root)
    expected = snapshot(built.path)
    built.close()
    manifest, files = website.compose(data)
    output = website._lock_output_root(output_root)
    original = website._open_validated_at
    held = output.path.with_name("published-held")

    def swap_after_validation(*args: Any, **kwargs: Any) -> Any:
        value = original(*args, **kwargs)
        output.path.rename(held)
        output.path.mkdir()
        (output.path / "valuable").write_bytes(b"keep")
        return value

    monkeypatch.setattr(website, "_open_validated_at", swap_after_validation)
    try:
        with pytest.raises(website.WebsiteRefused, match="output path identity"):
            website._materialize(output, manifest, files)
    finally:
        output.close()
    assert (output_root / "valuable").read_bytes() == b"keep"
    assert snapshot(held / manifest["freshness_sha256"]) == expected


def test_corrupt_or_extra_existing_target_refuses_unchanged(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    built = website.publish(data, output_root)
    path = built.path
    built.close()
    path.chmod(0o755)
    (path / "index.html").chmod(0o644)
    (path / "index.html").write_bytes(b"corrupt")
    (path / "valuable.txt").write_bytes(b"keep")
    path.chmod(0o555)
    before = snapshot(path)

    with pytest.raises(website.WebsiteRefused):
        website.publish(data, output_root)

    assert snapshot(path) == before


def test_symlink_target_and_unsafe_roots_refuse_without_deletion(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output_root = tmp_path / "published"
    output_root.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "valuable").write_bytes(b"keep")
    (output_root / manifest["freshness_sha256"]).symlink_to(
        outside, target_is_directory=True
    )
    locked = website._lock_output_root(output_root)
    try:
        with pytest.raises(
            website.WebsiteRefused,
            match="website validation refused: output directory is unavailable",
        ):
            website._materialize(locked, manifest, files)
    finally:
        locked.close()
    assert (outside / "valuable").read_bytes() == b"keep"

    with pytest.raises(website.WebsiteRefused, match="path traversal"):
        website._output_location(tmp_path / "safe" / ".." / "escape")
    with pytest.raises(website.WebsiteRefused, match="overlaps"):
        website._output_location(REPOSITORY / "ci-hub/compatibility-website/output")


@pytest.mark.parametrize("kind", ["regular", "fifo", "socket", "device"])
def test_non_directory_target_refuses_without_deletion(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, kind: str
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output_root = tmp_path / "published"
    output_root.mkdir()
    target = output_root / manifest["freshness_sha256"]
    holder = None
    if kind == "regular":
        target.write_bytes(b"valuable")
    elif kind == "fifo":
        os.mkfifo(target)
    elif kind == "socket":
        holder = socket.socket(socket.AF_UNIX)
        short_socket = tmp_path / "target.sock"
        holder.bind(str(short_socket))
        short_socket.rename(target)
    else:
        try:
            os.mknod(target, stat.S_IFCHR | 0o600, os.makedev(1, 3))
        except PermissionError:
            pytest.skip("creating a device node requires host permission")
    before = target.lstat()
    locked = website._lock_output_root(output_root)
    try:
        with pytest.raises(
            website.WebsiteRefused,
            match="website validation refused: output directory is unavailable",
        ):
            website._materialize(locked, manifest, files)
    finally:
        locked.close()
        if holder is not None:
            holder.close()
    after = target.lstat()
    assert os.path.samestat(before, after)
    if kind == "regular":
        assert target.read_bytes() == b"valuable"


@pytest.mark.parametrize("kind", ["symlink", "fifo", "socket", "device"])
def test_non_regular_artifact_refuses_without_replacing_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, kind: str
) -> None:
    data = build_data(tmp_path, monkeypatch)
    built = website.publish(data, tmp_path / "published")
    root = built.path
    built.close()
    artifact = root / "index.html"
    root.chmod(0o755)
    artifact.unlink()
    holder = None
    outside = tmp_path / "outside.html"
    outside.write_bytes(b"valuable")
    if kind == "symlink":
        artifact.symlink_to(outside)
    elif kind == "fifo":
        os.mkfifo(artifact)
    elif kind == "socket":
        holder = socket.socket(socket.AF_UNIX)
        short_socket = tmp_path / "artifact.sock"
        holder.bind(str(short_socket))
        short_socket.rename(artifact)
    else:
        try:
            os.mknod(artifact, stat.S_IFCHR | 0o600, os.makedev(1, 3))
        except PermissionError:
            pytest.skip("creating a device node requires host permission")
    root.chmod(0o555)
    before = artifact.lstat()
    try:
        with pytest.raises(
            website.WebsiteRefused,
            match="website artifact index.html is not a read-only regular file",
        ):
            website.validate_website(root)
    finally:
        if holder is not None:
            holder.close()
    after = artifact.lstat()
    assert os.path.samestat(before, after)
    assert outside.read_bytes() == b"valuable"


def test_partial_failure_preserves_only_its_private_tree(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    original = website._write_owned
    writes = 0

    def fail_after_two(*args: Any, **kwargs: Any) -> None:
        nonlocal writes
        original(*args, **kwargs)
        writes += 1
        if writes == 2:
            raise OSError("injected write failure")

    monkeypatch.setattr(website, "_write_owned", fail_after_two)
    with pytest.raises(website.WebsiteRefused, match="publication refused"):
        website.publish(data, output_root)
    assert output_root.exists()
    retained = list(output_root.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700
    assert "WARNING: retained private tree" in capsys.readouterr().err


def test_keyboard_interrupt_preserves_private_tree_and_is_not_a_refusal(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    original = website._write_owned
    interrupted = False

    def interrupt(*args: Any, **kwargs: Any) -> None:
        nonlocal interrupted
        original(*args, **kwargs)
        if not interrupted:
            interrupted = True
            raise KeyboardInterrupt

    monkeypatch.setattr(website, "_write_owned", interrupt)
    with pytest.raises(KeyboardInterrupt):
        website.publish(data, output_root)
    assert output_root.exists()
    retained = list(output_root.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700
    assert "WARNING: retained private tree" in capsys.readouterr().err


def test_unexpected_failure_preserves_private_tree_and_original_exception(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    class UnexpectedFailure(Exception):
        pass

    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"

    def fail() -> None:
        raise UnexpectedFailure("injected unexpected failure")

    with pytest.raises(UnexpectedFailure, match="injected unexpected failure"):
        website.publish(data, output_root, verify_sources=fail)
    retained = list(output_root.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700
    assert "WARNING: retained private tree" in capsys.readouterr().err


@pytest.mark.parametrize("fail_at", [1, 2])
def test_source_change_refuses_and_preserves_the_complete_or_private_tree(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    fail_at: int,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    checks = 0

    def verify() -> None:
        nonlocal checks
        checks += 1
        if checks == fail_at:
            raise website.WebsiteRefused("source checkout changed")

    with pytest.raises(website.WebsiteRefused, match="source checkout changed"):
        website.publish(data, output_root, verify_sources=verify)
    assert checks == fail_at
    assert output_root.exists()
    retained = list(output_root.iterdir())
    assert len(retained) == 1
    if fail_at == 1:
        assert retained[0].name.startswith(".")
        assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700
        warning = "WARNING: retained private tree"
    else:
        assert retained[0].name == website.compose(data)[0]["freshness_sha256"]
        assert stat.S_IMODE(retained[0].stat().st_mode) == 0o555
        validated = website.validate_website(retained[0])
        validated.close()
        warning = "WARNING: retained published tree"
    assert warning in capsys.readouterr().err


@pytest.mark.parametrize(
    ("command", "change_at"),
    [
        ("build-and-serve", 2),
        ("build-and-serve", 3),
        ("build", 4),
        ("build-and-serve", 4),
    ],
)
def test_build_recaptures_and_compares_all_four_stage1_files_at_final_boundaries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    command: str,
    change_at: int,
) -> None:
    initial = build_data(tmp_path, monkeypatch)
    changed = copy.deepcopy(initial)
    changed["tests"][0]["description"] += " changed during recapture"
    builder = website._modules().build_data
    provenance = changed["provenance"]
    projection = builder._projected_manifest(
        {
            "cells": {cell["cell"]: cell for cell in changed["cells"]},
            "manifest_schema": provenance["manifest_schema"],
            "manifest_sha256": provenance["manifest_sha256"],
            "schema": 1,
            "selected_by_full_custom_commands": changed[
                "selected_by_full_custom_commands"
            ],
            "tests": changed["tests"],
        }
    )
    provenance["manifest_metadata_projection_sha256"] = builder._digest(projection)
    assert website._stage_1_files(initial) != website._stage_1_files(changed)
    calls = 0

    def live_data() -> dict[str, Any]:
        nonlocal calls
        calls += 1
        return changed if calls >= change_at else initial

    monkeypatch.setattr(website, "_live_data", live_data)
    monkeypatch.setattr(website, "_require_live_sources", lambda _data: None)
    monkeypatch.setattr(website, "_runtime_source_identity", lambda: ("same",) * 4)
    output_root = tmp_path / "published"
    served = False

    def serve(value: Any, *_args: Any) -> None:
        nonlocal served
        value.recheck()
        served = True

    monkeypatch.setattr(website, "_exec_server", serve)

    assert website.main([command, "--output-root", str(output_root)]) == 2
    assert calls == change_at
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "Stage 1 source bytes changed" in captured.err
    assert served is False
    retained = list(output_root.iterdir())
    assert len(retained) == 1
    if change_at == 2:
        assert retained[0].name.startswith(".")
        assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700
    else:
        assert len(retained[0].name) == 64
        assert stat.S_IMODE(retained[0].stat().st_mode) == 0o555


def test_output_root_swap_during_source_verification_refuses_and_preserves_both(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    held = output.path.with_name("published-held")

    def swap() -> None:
        output.path.rename(held)
        output.path.mkdir()
        (output.path / "valuable").write_bytes(b"keep")

    try:
        with pytest.raises(website.WebsiteRefused, match="output path identity"):
            website._materialize(output, manifest, files, verify_sources=swap)
    finally:
        output.close()
    assert (output.path / "valuable").read_bytes() == b"keep"
    retained = list(held.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700


@pytest.mark.parametrize("mutation", ["artifact", "target"])
def test_reused_target_mutation_during_source_verification_is_preserved(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    mutation: str,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    built = website.publish(data, output_root)
    target = built.path
    built.close()
    held = target.with_name(target.name + ".held")

    def mutate() -> None:
        if mutation == "target":
            target.rename(held)
            target.mkdir()
            (target / "valuable").write_bytes(b"replacement")
            return
        target.chmod(0o755)
        artifact = target / "index.html"
        artifact.chmod(0o644)
        artifact.write_bytes(b"changed while sources were checked")

    with pytest.raises(website.WebsiteRefused):
        website.publish(data, output_root, verify_sources=mutate)
    if mutation == "target":
        assert (target / "valuable").read_bytes() == b"replacement"
        assert held.is_dir()
    else:
        assert (target / "index.html").read_bytes() == (
            b"changed while sources were checked"
        )


def test_sigterm_during_write_preserves_private_tree_and_exits_by_signal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    source = tmp_path / "data.json"
    source.write_text(json.dumps(data, sort_keys=True))
    output_root = tmp_path / "published"
    program = """
import importlib.util, json, os, pathlib, signal, sys
script, source, output = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('compatibility_website_signal', script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
data = json.loads(source.read_text())
module._live_data = lambda: data
module._require_live_sources = lambda _data: None
original = module._write_owned
triggered = False
def terminate(*args, **kwargs):
    global triggered
    original(*args, **kwargs)
    if not triggered:
        triggered = True
        os.kill(os.getpid(), signal.SIGTERM)
module._write_owned = terminate
raise SystemExit(module._entrypoint(['build', '--output-root', str(output)]))
"""
    result = subprocess.run(
        [sys.executable, "-c", program, str(SCRIPT), str(source), str(output_root)],
        cwd=REPOSITORY,
        check=False,
        capture_output=True,
        text=True,
        timeout=60,
    )

    assert result.returncode == -signal.SIGTERM
    assert result.stdout == ""
    assert "WARNING: retained private tree" in result.stderr
    assert output_root.exists()
    retained = list(output_root.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700


def test_sigterm_interrupts_a_process_blocked_on_the_output_lock(
    tmp_path: Path,
) -> None:
    output_root = tmp_path / "published"
    output_root.mkdir()
    holder = os.open(
        output_root,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    fcntl.flock(holder, fcntl.LOCK_EX)
    program = """
import importlib.util, pathlib, sys
script, output = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('compatibility_website_blocked_lock', script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
real_flock = module.fcntl.flock
def announce_then_lock(descriptor, operation):
    if operation == module.fcntl.LOCK_EX:
        print('waiting', flush=True)
    return real_flock(descriptor, operation)
module.fcntl.flock = announce_then_lock
module.main = lambda _argv: module._lock_output_root(output)
raise SystemExit(module._entrypoint([]))
"""
    process = subprocess.Popen(
        [sys.executable, "-c", program, str(SCRIPT), str(output_root)],
        cwd=REPOSITORY,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert process.stdout is not None
        ready, _, _ = select.select([process.stdout], [], [], 30)
        assert ready, "child did not reach the blocking flock"
        assert process.stdout.readline() == "waiting\n"
        process.send_signal(signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=10)
        assert process.returncode == -signal.SIGTERM
        assert stdout == ""
        assert stderr == ""
    finally:
        fcntl.flock(holder, fcntl.LOCK_UN)
        os.close(holder)
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


@pytest.mark.parametrize("phase", ["private-root", "child-directory", "artifact"])
def test_sigterm_during_private_entry_acquisition_preserves_the_tree(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    phase: str,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    source = tmp_path / "data.json"
    source.write_text(json.dumps(data, sort_keys=True))
    output_root = tmp_path / "published"
    program = """
import importlib.util, json, os, pathlib, signal, sys
script, source, output = map(pathlib.Path, sys.argv[1:4])
phase = sys.argv[4]
spec = importlib.util.spec_from_file_location('compatibility_website_acquire_signal', script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
data = json.loads(source.read_text())
module._live_data = lambda: data
module._require_live_sources = lambda _data: None
triggered = False
real_mkdir = module.os.mkdir
real_open = module.os.open
def terminate_once():
    global triggered
    if not triggered:
        triggered = True
        os.kill(os.getpid(), signal.SIGTERM)
def mkdir_then_signal(path, mode=0o777, *, dir_fd=None):
    result = real_mkdir(path, mode, dir_fd=dir_fd)
    if phase == 'private-root' and str(path).startswith('.'):
        terminate_once()
    if phase == 'child-directory' and path == 'assets':
        terminate_once()
    return result
def open_then_signal(path, flags, mode=0o777, *, dir_fd=None):
    result = real_open(path, flags, mode, dir_fd=dir_fd)
    if phase == 'artifact' and flags & os.O_EXCL:
        terminate_once()
    return result
module.os.mkdir = mkdir_then_signal
module.os.open = open_then_signal
raise SystemExit(module._entrypoint(['build', '--output-root', str(output)]))
"""
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            program,
            str(SCRIPT),
            str(source),
            str(output_root),
            phase,
        ],
        cwd=REPOSITORY,
        check=False,
        capture_output=True,
        text=True,
        timeout=60,
    )

    assert result.returncode == -signal.SIGTERM
    assert result.stdout == ""
    assert "WARNING: retained private tree" in result.stderr
    retained = list(output_root.iterdir())
    assert len(retained) == 1 and retained[0].name.startswith(".")
    assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700


@pytest.mark.parametrize("failure", ["rename", "postcheck", "parent-fsync"])
def test_publication_failure_preserves_private_or_complete_tree(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    failure: str,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    writer = website._modules().writer
    original_rename = website._rename_noreplace
    original_read = website._read_open_tree
    original_fsync = writer._fsync_directory
    failed = False

    if failure == "rename":
        monkeypatch.setattr(
            website,
            "_rename_noreplace",
            lambda *_args: (_ for _ in ()).throw(OSError("injected rename failure")),
        )
    elif failure == "postcheck":

        def fail_postcheck(*args: Any, **kwargs: Any) -> Any:
            nonlocal failed
            path = args[1]
            if path.name == manifest["freshness_sha256"] and not failed:
                failed = True
                raise website.WebsiteRefused("injected postcheck failure")
            return original_read(*args, **kwargs)

        monkeypatch.setattr(website, "_read_open_tree", fail_postcheck)
    else:

        def fail_parent_once(descriptor: int) -> None:
            nonlocal failed
            if descriptor == output.descriptor and not failed:
                failed = True
                raise OSError("injected parent fsync failure")
            original_fsync(descriptor)

        monkeypatch.setattr(writer, "_fsync_directory", fail_parent_once)
    try:
        with pytest.raises(website.WebsiteRefused, match="publication refused"):
            website._materialize(output, manifest, files)
    finally:
        output.close()
        monkeypatch.setattr(website, "_rename_noreplace", original_rename)
    retained = list((tmp_path / "published").iterdir())
    assert len(retained) == 1
    if failure == "rename":
        assert retained[0].name.startswith(".")
        assert stat.S_IMODE(retained[0].stat().st_mode) == 0o700
        warning = "WARNING: retained private tree"
    else:
        assert retained[0].name == manifest["freshness_sha256"]
        assert stat.S_IMODE(retained[0].stat().st_mode) == 0o555
        warning = "WARNING: retained published tree"
    assert warning in capsys.readouterr().err


def test_signal_immediately_after_atomic_publication_retains_complete_target(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    original = website._rename_noreplace

    def publish_then_interrupt(directory: int, left: str, right: str) -> None:
        original(directory, left, right)
        raise website.WebsiteSignal(signal.SIGTERM)

    monkeypatch.setattr(website, "_rename_noreplace", publish_then_interrupt)
    try:
        with pytest.raises(website.WebsiteSignal):
            website._materialize(output, manifest, files)
    finally:
        output.close()
    target = output.path / manifest["freshness_sha256"]
    assert stat.S_IMODE(target.stat().st_mode) == 0o555
    validated = website.validate_website(target)
    validated.close()
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "WARNING: retained published tree" in captured.err


def test_noreplace_race_refuses_contradictory_winner_and_preserves_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    original = website._rename_noreplace
    target = str(manifest["freshness_sha256"])

    def race(directory: int, left: str, right: str) -> None:
        winner = output.path / target
        winner.mkdir()
        (winner / "valuable").write_bytes(b"keep")
        original(directory, left, right)

    monkeypatch.setattr(website, "_rename_noreplace", race)
    try:
        with pytest.raises(website.WebsiteRefused):
            website._materialize(output, manifest, files)
    finally:
        output.close()
    assert (output.path / target / "valuable").read_bytes() == b"keep"
    assert len(list(output.path.iterdir())) == 2
    assert any(path.name.startswith(".") for path in output.path.iterdir())


def test_noreplace_race_reuses_an_exact_same_digest_winner(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    original = website._rename_noreplace
    target = str(manifest["freshness_sha256"])

    def race(directory: int, left: str, right: str) -> None:
        winner = output.path / target
        write_read_only_tree(winner, files)
        original(directory, left, right)

    monkeypatch.setattr(website, "_rename_noreplace", race)
    try:
        result = website._materialize(output, manifest, files)
        assert result.reused is True
        assert result.manifest == manifest
        result.close()
    finally:
        output.close()
    names = {path.name for path in output.path.iterdir()}
    assert target in names
    assert len(names) == 2 and any(name.startswith(".") for name in names)


def test_noreplace_winner_rechecks_sources_after_preserving_loser(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    manifest, files = website.compose(data)
    output = website._lock_output_root(tmp_path / "published")
    original = website._rename_noreplace
    target = str(manifest["freshness_sha256"])
    checks = 0

    def race(directory: int, left: str, right: str) -> None:
        write_read_only_tree(output.path / target, files)
        original(directory, left, right)

    def verify() -> None:
        nonlocal checks
        checks += 1
        if checks == 3:
            raise website.WebsiteRefused("source changed during loser preservation")

    monkeypatch.setattr(website, "_rename_noreplace", race)
    try:
        with pytest.raises(
            website.WebsiteRefused, match="source changed during loser preservation"
        ):
            website._materialize(output, manifest, files, verify_sources=verify)
    finally:
        output.close()
    assert checks == 3
    names = {path.name for path in output.path.iterdir()}
    assert target in names
    assert len(names) == 2 and any(name.startswith(".") for name in names)


def test_private_tree_mutation_is_preserved_and_warned(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    original = website._write_owned
    injected = False

    def mutate(directory: int, name: str, value: bytes) -> None:
        nonlocal injected
        original(directory, name, value)
        if not injected:
            descriptor = os.open(
                "unexpected",
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
                dir_fd=directory,
            )
            os.close(descriptor)
            injected = True
            raise OSError("injected mutation")

    monkeypatch.setattr(website, "_write_owned", mutate)
    with pytest.raises(website.WebsiteRefused):
        website.publish(data, output_root)
    retained = list(output_root.iterdir())
    assert len(retained) == 1
    assert retained[0].name.startswith(".")
    assert injected is True
    unexpected = list(retained[0].rglob("unexpected"))
    assert len(unexpected) == 1
    assert unexpected[0].read_bytes() == b""
    assert "WARNING: retained private tree" in capsys.readouterr().err


def test_failed_publication_never_deletes_a_swapped_owned_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    original = website._write_owned
    changed = False

    def replace_then_fail(directory: int, name: str, value: bytes) -> None:
        nonlocal changed
        original(directory, name, value)
        if not changed:
            os.rename(name, name + ".owned", src_dir_fd=directory, dst_dir_fd=directory)
            descriptor = os.open(
                name,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
                dir_fd=directory,
            )
            os.write(descriptor, b"valuable replacement")
            os.close(descriptor)
            changed = True
            raise OSError("injected replacement")

    monkeypatch.setattr(website, "_write_owned", replace_then_fail)
    with pytest.raises(website.WebsiteRefused, match="publication refused"):
        website.publish(data, output_root)

    retained = next(output_root.iterdir())
    replacement = next(
        path
        for path in retained.rglob("*")
        if path.is_file() and path.read_bytes() == b"valuable replacement"
    )
    assert replacement.read_bytes() == b"valuable replacement"
    assert replacement.with_name(replacement.name + ".owned").exists()
    source = SCRIPT.read_text()
    assert "os.unlink(" not in source
    assert "os.rmdir(" not in source


def test_different_freshness_never_deletes_an_older_build(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    output_root = tmp_path / "published"
    first = website.publish(data, output_root)
    first_path = first.path
    first.close()
    first_snapshot = snapshot(first_path)
    second = website.publish(data, output_root, wording="fixture")
    second_path = second.path
    second.close()

    assert first_path != second_path
    assert snapshot(first_path) == first_snapshot
    assert {path.name for path in output_root.iterdir()} == {
        first_path.name,
        second_path.name,
    }


def test_two_processes_publish_one_tree_and_the_loser_reuses_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    source = tmp_path / "data.json"
    source.write_text(json.dumps(data, sort_keys=True))
    output_root = tmp_path / "published"
    program = """
import importlib.util, json, pathlib, sys
script, source, output = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('compatibility_website_race', script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
value = module.publish(json.loads(source.read_text()), output)
print(json.dumps(module._result(value), sort_keys=True, separators=(',', ':')), flush=True)
value.close()
"""
    command = [
        sys.executable,
        "-c",
        program,
        str(SCRIPT),
        str(source),
        str(output_root),
    ]
    processes = [
        subprocess.Popen(
            command,
            cwd=REPOSITORY,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        for _index in range(2)
    ]
    results: list[tuple[str, str]] = []
    try:
        for process in processes:
            results.append(process.communicate(timeout=60))
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
        for process in processes:
            if process.poll() is None:
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            if process.stdout is not None:
                process.stdout.close()
            if process.stderr is not None:
                process.stderr.close()

    assert [process.returncode for process in processes] == [0, 0], results
    assert all(process.poll() is not None for process in processes)
    rows = [json.loads(stdout) for stdout, _stderr in results]
    assert rows[0]["path"] == rows[1]["path"]
    assert rows[0]["freshness_sha256"] == rows[1]["freshness_sha256"]
    assert sorted(row["reused"] for row in rows) == [False, True]
    assert all(stderr == "" for _stdout, stderr in results)
    assert len(list(output_root.iterdir())) == 1
    lock = os.open(
        output_root,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fcntl.flock(lock, fcntl.LOCK_UN)
    finally:
        os.close(lock)


def test_published_result_holds_output_lock_until_close(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    built = website.publish(data, tmp_path / "published")
    contender = os.open(
        built.path.parent,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    try:
        with pytest.raises(BlockingIOError):
            fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        built.close()
        fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fcntl.flock(contender, fcntl.LOCK_UN)
    finally:
        built.close()
        os.close(contender)


def test_output_root_close_does_not_depend_on_an_explicit_unlock(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    output_root = built.path.parent
    original = website.fcntl.flock

    def reject_unlock(descriptor: int, operation: int) -> None:
        if operation == fcntl.LOCK_UN:
            raise OSError("explicit unlock must not be used")
        original(descriptor, operation)

    monkeypatch.setattr(website.fcntl, "flock", reject_unlock)
    built.close()
    contender = os.open(
        output_root,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    try:
        original(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        original(contender, fcntl.LOCK_UN)
    finally:
        os.close(contender)


def test_real_writer_recheck_error_is_a_prefixed_refusal(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    built.path.chmod(0o755)
    (built.path / "unexpected").write_bytes(b"keep")
    built.path.chmod(0o555)
    monkeypatch.setattr(website, "build", lambda _output_root: built)

    assert website.main(["build", "--output-root", str(tmp_path / "unused")]) == 2
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err.startswith("compatibility-website: REFUSED:")
    assert "inventory is not exact" in captured.err
    assert (built.path / "unexpected").read_bytes() == b"keep"


def test_default_output_root_is_a_deterministic_checkout_sibling() -> None:
    assert website.DEFAULT_OUTPUT_ROOT == (
        website.REPOSITORY.parent / f"{website.REPOSITORY.name}-compatibility-website"
    )


def test_output_inside_any_repo_child_refuses_before_creation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repository = tmp_path / "repo"
    (repository / ".git").mkdir(parents=True)
    parent = repository / "ci-hub/lib"
    parent.mkdir(parents=True)
    target = parent / "site"
    monkeypatch.setattr(website, "REPOSITORY", repository)

    with pytest.raises(website.WebsiteRefused, match="overlaps a source directory"):
        website._lock_output_root(target)
    assert not target.exists()


def test_default_sibling_inside_an_enclosing_checkout_refuses_before_creation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    enclosing = tmp_path / "enclosing"
    (enclosing / ".git").mkdir(parents=True)
    repository = enclosing / "nested"
    repository.mkdir()
    default = enclosing / "nested-compatibility-website"
    monkeypatch.setattr(website, "REPOSITORY", repository)
    monkeypatch.setattr(website, "DEFAULT_OUTPUT_ROOT", default)

    with pytest.raises(website.WebsiteRefused, match="overlaps a source directory"):
        website._lock_output_root(default)
    assert not default.exists()


def test_main_build_output_and_refusal_have_single_channel_contracts(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    data = build_data(tmp_path, monkeypatch)
    built = website.publish(data, tmp_path / "published")
    manifest = built.manifest
    path = built.path
    built.close()

    def supplied(_output_root: Path) -> Any:
        descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        return website.ValidatedWebsite(path, descriptor, manifest, reused=True)

    monkeypatch.setattr(website, "build", supplied)
    assert website.main(["build", "--output-root", str(tmp_path / "unused")]) == 0
    captured = capsys.readouterr()
    assert captured.err == ""
    assert json.loads(captured.out) == {
        "build_json_sha256": website._sha256(website._json_bytes(manifest) + b"\n"),
        "freshness_sha256": manifest["freshness_sha256"],
        "path": str(path),
        "provenance": manifest["provenance"],
        "reused": True,
        "schema_version": 1,
        "tree_sha256": manifest["tree_sha256"],
    }

    class StopServe(Exception):
        pass

    expected_result = {
        "build_json_sha256": website._sha256(website._json_bytes(manifest) + b"\n"),
        "freshness_sha256": manifest["freshness_sha256"],
        "path": str(path),
        "provenance": manifest["provenance"],
        "reused": True,
        "schema_version": 1,
        "tree_sha256": manifest["tree_sha256"],
    }

    def stop_serve(value: Any, port: int, result: Any) -> None:
        assert value.manifest == manifest
        assert port == 1234
        assert result == expected_result
        print(
            "compatibility-website: " + website._json_bytes(result).decode(),
            file=sys.stderr,
        )
        value.close()
        raise StopServe

    monkeypatch.setattr(website, "_exec_server", stop_serve)
    with pytest.raises(StopServe):
        website.main(
            [
                "build-and-serve",
                "--output-root",
                str(tmp_path / "unused"),
                "--port",
                "1234",
            ]
        )
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == (
        "compatibility-website: " + website._json_bytes(expected_result).decode() + "\n"
    )

    def refused(_output_root: Path) -> Any:
        raise website.WebsiteRefused("expected producer gap")

    monkeypatch.setattr(website, "build", refused)
    assert website.main(["build"]) == 2
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == "compatibility-website: REFUSED: expected producer gap\n"


def test_build_and_serve_never_falls_back_after_build_refusal(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    called = False

    def refused(_output_root: Path) -> Any:
        raise website.WebsiteRefused("missing manifest metadata binary")

    def serve(_value: Any, _port: int) -> None:
        nonlocal called
        called = True

    monkeypatch.setattr(website, "build", refused)
    monkeypatch.setattr(website, "_exec_server", serve)
    assert website.main([]) == 2
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "missing manifest metadata binary" in captured.err
    assert called is False


def test_server_exec_uses_the_validated_inherited_descriptor(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    value = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    descriptor = value.descriptor
    observed: list[str] = []

    def stop(_program: str, arguments: list[str]) -> None:
        observed.extend(arguments)
        raise OSError("stop before exec")

    monkeypatch.setattr(website, "_runtime_source_identity", lambda: ("same",) * 4)
    monkeypatch.setattr(os, "execvp", stop)
    with pytest.raises(website.WebsiteRefused, match="cannot execute"):
        website._exec_server(value, 1234)
    assert observed[-4:] == ["--root-fd", str(descriptor), "--port", "1234"]
    assert value.descriptor == -1


@pytest.mark.parametrize(
    "failure",
    [
        "first-runtime-check",
        "tree-recheck",
        "second-runtime-check",
        "runtime-identity-mismatch",
    ],
)
def test_server_refusal_closes_the_target_and_releases_the_output_lock(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    failure: str,
) -> None:
    value = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    output_root = value.path.parent
    calls = 0

    def runtime_identity() -> tuple[str, str, str, str]:
        nonlocal calls
        calls += 1
        if failure == "first-runtime-check" and calls == 1:
            raise website.WebsiteRefused("injected runtime refusal")
        if failure == "second-runtime-check" and calls == 2:
            raise website.WebsiteRefused("injected runtime refusal")
        if failure == "runtime-identity-mismatch":
            return (("first",) if calls == 1 else ("second",)) * 4
        return ("same",) * 4

    if failure == "tree-recheck":
        monkeypatch.setattr(
            value,
            "recheck",
            lambda: (_ for _ in ()).throw(
                website.WebsiteRefused("injected tree refusal")
            ),
        )
    monkeypatch.setattr(website, "_runtime_source_identity", runtime_identity)

    expected = (
        "website server sources changed"
        if failure == "runtime-identity-mismatch"
        else "injected .* refusal"
    )
    with pytest.raises(website.WebsiteRefused, match=expected):
        website._exec_server(value, 1234)
    assert value.descriptor == -1
    assert value.output_root is None

    contender = os.open(
        output_root,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
    )
    try:
        fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fcntl.flock(contender, fcntl.LOCK_UN)
    finally:
        os.close(contender)


def test_public_serve_wrapper_binds_build_json_and_propagates_sigterm(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    built = website.publish(data, tmp_path / "published")
    root = built.path
    expected_build = (root / "build.json").read_bytes()
    built.close()
    program = """
import importlib.util, pathlib, sys
script, root = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('compatibility_website_serve_wrapper', script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
module._runtime_source_identity = lambda: ('same',) * 4
raise SystemExit(module._entrypoint(['serve', '--root', str(root), '--port', '0']))
"""
    process = subprocess.Popen(
        [sys.executable, "-c", program, str(SCRIPT), str(root)],
        cwd=REPOSITORY,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        assert process.stdout is not None
        ready, _, _ = select.select([process.stdout], [], [], 60)
        if not ready:
            os.killpg(process.pid, signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=5)
            pytest.fail(
                f"serve wrapper did not become ready: rc={process.returncode} "
                f"stdout={stdout!r} stderr={stderr!r}"
            )
        url = process.stdout.readline().strip()
        parsed = urlsplit(url)
        assert parsed.hostname == "127.0.0.1" and parsed.port is not None
        held = root.with_name(root.name + ".held")
        root.rename(held)
        root.mkdir()
        (root / "build.json").write_bytes(b"replacement")
        with socket.create_connection(
            ("127.0.0.1", parsed.port), timeout=3
        ) as connection:
            connection.sendall(
                f"GET /build.json HTTP/1.1\r\nHost: 127.0.0.1:{parsed.port}\r\n\r\n".encode()
            )
            response = bytearray()
            while chunk := connection.recv(65536):
                response.extend(chunk)
        assert bytes(response).split(b"\r\n\r\n", 1)[1] == expected_build
        os.killpg(process.pid, signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=5)
        assert process.returncode == 0
        assert stdout == ""
        assert stderr == ""
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)


def test_build_and_serve_reports_and_serves_one_artifact_across_path_swap(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    source = tmp_path / "data.json"
    source.write_text(json.dumps(data, sort_keys=True))
    output_root = tmp_path / "published"
    program = """
import fcntl, importlib.util, json, os, pathlib, sys
script, source, output = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('compatibility_website_handoff', script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
data = json.loads(source.read_text())
module._live_data = lambda: data
module._require_live_sources = lambda _data: None
module._runtime_source_identity = lambda: ('same',) * 4
real_execvp = os.execvp
def swap_then_exec(program, arguments):
    lock = os.open(output, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    fcntl.flock(lock, fcntl.LOCK_UN)
    os.close(lock)
    targets = [path for path in output.iterdir() if len(path.name) == 64]
    assert len(targets) == 1
    target = targets[0]
    held = target.with_name(target.name + '.held')
    target.rename(held)
    target.mkdir()
    (target / 'build.json').write_bytes(b'replacement')
    real_execvp(program, arguments)
module.os.execvp = swap_then_exec
raise SystemExit(module._entrypoint(['build-and-serve', '--output-root', str(output), '--port', '0']))
"""
    process = subprocess.Popen(
        [sys.executable, "-c", program, str(SCRIPT), str(source), str(output_root)],
        cwd=REPOSITORY,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        assert process.stdout is not None and process.stderr is not None
        ready, _, _ = select.select([process.stderr], [], [], 60)
        assert ready, "combined command did not report its artifact identity"
        identity_line = process.stderr.readline()
        prefix = "compatibility-website: "
        assert identity_line.startswith(prefix)
        identity = json.loads(identity_line.removeprefix(prefix))
        assert identity["provenance"] == data["provenance"]
        assert (
            identity["provenance"]["parent_commit"]
            == data["provenance"]["parent_commit"]
        )
        assert (
            identity["provenance"]["hermit_commit"]
            == data["provenance"]["hermit_commit"]
        )
        assert (
            identity["provenance"]["hermit_main_commit"]
            == data["provenance"]["hermit_main_commit"]
        )
        ready, _, _ = select.select([process.stdout], [], [], 60)
        assert ready, "combined command did not report its server URL"
        url = process.stdout.readline().strip()
        parsed = urlsplit(url)
        assert parsed.hostname == "127.0.0.1" and parsed.port is not None
        with socket.create_connection(
            ("127.0.0.1", parsed.port), timeout=3
        ) as connection:
            connection.sendall(
                f"GET /build.json HTTP/1.1\r\nHost: 127.0.0.1:{parsed.port}\r\n\r\n".encode()
            )
            response = bytearray()
            while chunk := connection.recv(65536):
                response.extend(chunk)
        body = bytes(response).split(b"\r\n\r\n", 1)[1]
        assert body != b"replacement"
        assert website._sha256(body) == identity["build_json_sha256"]
        os.killpg(process.pid, signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=10)
        assert process.returncode == 0
        assert stdout == ""
        assert stderr == ""
        with pytest.raises(ProcessLookupError):
            os.killpg(process.pid, 0)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)


def test_help_is_bounded_and_names_all_modes() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--help"],
        cwd=REPOSITORY,
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0
    assert result.stderr == ""
    assert len(result.stdout) < 4096
    assert "build" in result.stdout
    assert "serve" in result.stdout
    assert "build-and-serve" in result.stdout
    assert "build-historical" in result.stdout
    assert "build-historical-and-serve" in result.stdout


@pytest.mark.parametrize(
    "mode",
    [
        "root",
        "build",
        "serve",
        "build-and-serve",
        "build-historical",
        "build-historical-and-serve",
    ],
)
def test_ci_hub_website_help_is_uncosted_and_side_effect_free(
    tmp_path: Path,
    mode: str,
) -> None:
    output_root = tmp_path / "must-not-exist"
    arguments = {
        "root": ["--help"],
        "build": ["build", "--output-root", str(output_root), "--help"],
        "serve": ["serve", "--root", str(output_root), "--help"],
        "build-and-serve": [
            "build-and-serve",
            "--output-root",
            str(output_root),
            "--help",
        ],
        "build-historical": ["build-historical", "--help"],
        "build-historical-and-serve": ["build-historical-and-serve", "--help"],
    }[mode]
    result = subprocess.run(
        [str(REPOSITORY / "ci-hub/ci-hub"), "compatibility-website", *arguments],
        cwd=REPOSITORY,
        check=False,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0
    assert "usage:" in result.stdout
    assert "ci-hub compatibility-website" in result.stdout
    assert "build_and_serve.py" not in result.stdout + result.stderr
    assert "COST " not in result.stdout + result.stderr
    assert result.stderr == ""
    assert not output_root.exists()


@pytest.mark.parametrize(
    "command",
    [
        "./ci-hub/ci-hub compatibility-website build",
        "./ci-hub/ci-hub compatibility-website build-historical --parent /absolute/path/to/dev-hermit --parent-commit 5bfa5d91f56e380631a549bf8dcf58631c97dabd --record-id devbig014-1788153725-3415639 --run-id validate-mega-lander-69b08cd09a60-1788152123591813608-3265480-d6eafa4c --manifest-hermit /absolute/path/to/hermit-pr2880 --manifest-commit bc04aa922e806e99222ee03ac81c87495be41f20",
        "./ci-hub/ci-hub compatibility-website serve --root /absolute/path/to/dev-hermit-compatibility-website/FRESHNESS_SHA256",
        "./ci-hub/ci-hub compatibility-website",
    ],
)
def test_documented_website_commands_reach_their_exact_parser_branch(
    command: str,
) -> None:
    assert documented_commands._classify(command) == "parse"
    assert documented_commands._parse_probe(command) == command


def test_documented_find_probe_is_read_only_and_rejects_near_misses(
    tmp_path: Path,
) -> None:
    command = "find ignored/ci-hub/landing -name '*-comment-failed.json' -print"
    assert documented_commands._classify(command) == "parse"
    assert documented_commands._parse_probe(command, find_root=tmp_path) == (
        f"find {tmp_path} -name '*-comment-failed.json' -print"
    )
    for unsafe in (
        "find ignored/ci-hub/landing -name '*-comment-failed.json' -delete",
        "find ignored/ci-hub -name '*-comment-failed.json' -print",
        "find ignored/ci-hub/landing -name $(touch /tmp/value) -print",
    ):
        with pytest.raises(documented_commands.DocsCommandError):
            documented_commands._classify(unsafe)


@pytest.mark.parametrize(
    "arguments",
    [
        ["build", "--output-root", "/path/that/must/not/be/created"],
        ["serve", "--root", "/path/that/need/not/exist", "--port", "4321"],
        [],
    ],
)
def test_docs_parse_only_accepts_exact_arguments_without_dispatch(
    arguments: list[str],
) -> None:
    environment = os.environ.copy()
    environment["CI_HUB_DOCS_PARSE_ONLY"] = "1"
    result = subprocess.run(
        [sys.executable, str(SCRIPT), *arguments],
        cwd=REPOSITORY,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0
    assert result.stdout.startswith("DOCS PARSE OK: compatibility-website")
    assert result.stderr == ""


def test_historical_parser_requires_all_immutable_source_coordinates() -> None:
    parser = website._parser()
    arguments = parser.parse_args(
        [
            "build-historical",
            "--parent-commit",
            "a" * 40,
            "--series-commit",
            "c" * 40,
            "--record-id",
            "record-1",
            "--run-id",
            "run-1",
            "--manifest-hermit",
            "/tmp/manifest-hermit",
            "--manifest-commit",
            "b" * 40,
        ]
    )

    assert arguments.command == "build-historical"
    assert arguments.parent_commit == "a" * 40
    assert arguments.series_commit == "c" * 40
    assert arguments.record_id == "record-1"
    assert arguments.run_id == "run-1"
    assert arguments.manifest_commit == "b" * 40


def test_historical_recorded_only_population_never_claims_planned_selection(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = historical_build_data(tmp_path, monkeypatch)
    for source in (data["provenance"]["historical"],
                   data["provenance"]["canonical_input_provenance"]["historical"]):
        source.update(selected_by_full_cell_count=None, recorded_cell_count=1,
                      population_scope="recorded_only",
                      comparison_authority="writer_verified_raw_census_v1")
    checked = website._modules().build_data.validate_build_data(data)
    _manifest, files = website.compose(checked, wording="historical")
    html = files["index.html"].decode()
    assert "Retained recorded population (planned selection unavailable)" in html
    assert "Selected by full" not in html
    for field, value in (("comparison_authority", "summary"),
                         ("population_scope", "full"),
                         ("recorded_cell_count", True)):
        bad = copy.deepcopy(data)
        for source in (bad["provenance"]["historical"],
                       bad["provenance"]["canonical_input_provenance"]["historical"]):
            source[field] = value
        with pytest.raises(website._modules().build_data.BuildRefused):
            website._modules().build_data.validate_build_data(bad)


@pytest.mark.parametrize(
    "arguments",
    [
        ["serve"],
        ["serve", "--root", "/tmp/site", "--prot", "1234"],
        ["serve", "--roo", "/tmp/site"],
        ["serve", "--root", "/tmp/site", "--por", "1234"],
        ["build", "--output-rot", "/tmp/site"],
        ["build", "--output-roo", "/tmp/site"],
    ],
)
def test_website_parser_rejects_missing_values_and_misspelled_options(
    arguments: list[str],
) -> None:
    environment = os.environ.copy()
    environment["CI_HUB_DOCS_PARSE_ONLY"] = "1"
    result = subprocess.run(
        [sys.executable, str(SCRIPT), *arguments],
        cwd=REPOSITORY,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 2
    assert result.stdout == ""
    assert "error:" in result.stderr
    assert "ci-hub compatibility-website" in result.stderr
    assert "build_and_serve.py" not in result.stderr


def test_presentation_record_is_reproducible_and_content_bound(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    data = build_data(tmp_path, monkeypatch)
    record = renderer_test._presentation(data)
    manifest, files = website.compose(data, presentation=record)
    assert website.compose(data, presentation=record) == (manifest, files)
    assert manifest["generator"]["presentation"] == record
    assert website._validate_manifest(manifest) == manifest
    website._reproduce_expected_bytes(manifest, {path: raw for path, raw in files.items() if path != "build.json"})
    changed = copy.deepcopy(manifest)
    changed["generator"]["presentation"]["commits"][0]["committed_at"] = "2026-08-19T12:00:00Z"
    with pytest.raises(website.WebsiteRefused, match="freshness digest"):
        website._validate_manifest(changed)
    assert files["data/runs.json.gz"] == website.compose(data)[1]["data/runs.json.gz"]
    record["build_started_at"] = "2026-09-22T20:00:00Z"
    later, _ = website.compose(data, presentation=record)
    assert later["freshness_sha256"] != manifest["freshness_sha256"]


def test_capture_presentation_reads_exact_commit_objects_not_head(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    data = build_data(tmp_path, monkeypatch)
    repo = tmp_path / "commit-dates"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    env = {**os.environ, "GIT_AUTHOR_NAME": "Fixture", "GIT_AUTHOR_EMAIL": "fixture@example.invalid", "GIT_COMMITTER_NAME": "Fixture", "GIT_COMMITTER_EMAIL": "fixture@example.invalid", "GIT_AUTHOR_DATE": "2026-08-20T12:00:00Z", "GIT_COMMITTER_DATE": "2026-08-20T12:00:00Z"}
    subprocess.run(["git", "-C", str(repo), "-c", "core.hooksPath=/dev/null", "commit", "--allow-empty", "-qm", "dated fixture"], env=env, check=True)
    commit = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
    data["provenance"]["parent_commit"] = commit
    result = website.capture_presentation(data, "2026-09-21T20:00:00Z", parent=repo, hermit=repo)
    actual = next(row for row in result["commits"] if row["repository"] == "dev-hermit")
    assert actual == {"repository": "dev-hermit", "commit": commit, "committed_at": "2026-08-20T12:00:00+00:00"}
    assert all(row["committed_at"] is None for row in result["commits"] if row["repository"] == "hermit")
    assert result["build_started_at"] == "2026-09-21T20:00:00Z"


def test_presentation_cell_ledger_date_uses_explicit_state_root(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    data = build_data(tmp_path, monkeypatch)
    parent = tmp_path / "tool-parent"
    state = tmp_path / "operational-state"
    ledger = state / "hermit_test_ledger"
    for path in (parent, state, ledger):
        path.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "init", "-q", str(path)], check=True)
    (state / ".gitmodules").write_text('[submodule "hermit_test_ledger"]\n path = hermit_test_ledger\n url = https://github.com/rrnewton/hermit_test_ledger.git\n')
    subprocess.run(["git", "-C", str(ledger), "remote", "add", "origin", "https://github.com/rrnewton/hermit_test_ledger.git"], check=True)
    env = {**os.environ, "GIT_AUTHOR_NAME": "Fixture", "GIT_AUTHOR_EMAIL": "fixture@example.invalid", "GIT_COMMITTER_NAME": "Fixture", "GIT_COMMITTER_EMAIL": "fixture@example.invalid", "GIT_AUTHOR_DATE": "2026-09-16T12:00:00Z", "GIT_COMMITTER_DATE": "2026-09-16T12:00:00Z"}
    subprocess.run(["git", "-C", str(ledger), "-c", "core.hooksPath=/dev/null", "commit", "--allow-empty", "-qm", "ledger date fixture"], env=env, check=True)
    commit = subprocess.check_output(["git", "-C", str(ledger), "rev-parse", "HEAD"], text=True).strip()
    data["provenance"]["canonical_input_provenance"].update(series_commit=commit, series_repository="https://github.com/rrnewton/hermit_test_ledger.git")
    monkeypatch.delenv("DEV_HERMIT_TEST_LEDGER_ROOT", raising=False)
    result = website.capture_presentation(data, "2026-09-21T20:00:00Z", parent=parent, hermit=parent, state_root=state)
    row = next(row for row in result["commits"] if row["repository"] == "hermit_test_ledger")
    assert row == {"repository": "hermit_test_ledger", "commit": commit, "committed_at": "2026-09-16T12:00:00+00:00"}
    with pytest.raises(website.WebsiteRefused, match="cannot resolve recorded cell ledger"):
        website.capture_presentation(data, "2026-09-21T20:00:00Z", parent=parent, hermit=parent, state_root=parent)


def test_live_builder_keeps_pinned_reader_and_fresh_history_commit_separate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls = []
    data = SimpleNamespace(build_data=lambda *args, **kwargs: calls.append((args, kwargs)) or {"fixture": True}, BuildRefused=RuntimeError)
    monkeypatch.setattr(website, "_modules", lambda: SimpleNamespace(build_data=data))
    monkeypatch.setattr(website, "_git_oid", lambda *_: "a" * 40)
    state = tmp_path / "actual-canonical-state"
    assert website._live_data(series_ref="b" * 40, published_only=True,
        state_root=state, validation_history_commit="c" * 40) == {"fixture": True}
    args, options = calls[0]
    assert args[0] == website.REPOSITORY and args[1] == "a" * 40
    assert options == {"series_ref": "b" * 40, "published_only": True,
                       "state_root": state, "validation_history_commit": "c" * 40}


def test_finalized_proof_reader_is_captured_and_bound_not_ambient():
    modules = website._modules()
    proof = modules.export_inputs._proof_reader()
    assert Path(proof.__file__) == website.REPOSITORY / "ci-hub/series/finalized_proof.py"
    assert proof is modules.export_inputs.finalized_proof
    assert "ci-hub/series/finalized_proof.py" in website.STAGE_1_READER_PATHS
    assert "ci-hub/series/finalized_proof.py" in website.GENERATOR_PATHS


def test_website_build_json_bound_admits_the_measured_proof_growth(tmp_path: Path) -> None:
    # 2026-09-28: a complete Stage 1 build at ledger 7d28c1f0 wrote a
    # 19,498,083-byte website build.json and the old 16 MiB reader bound then
    # refused it, so nothing was published. The bound stays under GitHub's
    # 100 MiB pushed-file limit, because the site is pushed to gh-pages.
    measured = 19_498_083
    assert measured < website.MAX_BUILD_JSON_BYTES < 100 << 20
    build = tmp_path / "build.json"
    build.write_bytes(b"{" + b" " * (measured - 2) + b"}")
    build.chmod(0o444)
    directory = os.open(tmp_path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        value = website._read_artifact(
            directory, "build.json", website.MAX_BUILD_JSON_BYTES, "website build.json"
        )
        assert len(value) == measured
        build.chmod(0o644)
        os.truncate(build, website.MAX_BUILD_JSON_BYTES + 1)
        build.chmod(0o444)
        with pytest.raises(Exception, match="not a bounded regular file: build.json"):
            website._read_artifact(
                directory, "build.json", website.MAX_BUILD_JSON_BYTES, "website build.json"
            )
    finally:
        os.close(directory)


def _published_files(root: Path) -> dict[str, bytes]:
    files = {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            files[str(path.relative_to(root))] = path.read_bytes()
    return files


def test_phase_timing_changes_no_output_file_and_no_stdout_result(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capfd: pytest.CaptureFixture[str],
) -> None:
    import re

    builder = website._modules().build_data
    data = build_data(tmp_path, monkeypatch)
    monkeypatch.setattr(website, "_live_data", lambda **_: copy.deepcopy(data))
    monkeypatch.setattr(website, "_require_live_sources", lambda _data: None)
    monkeypatch.setattr(website, "_runtime_source_identity", lambda: ("same",) * 4)

    class FixedClock(website.datetime):
        @classmethod
        def now(cls, tz: Any = None) -> Any:
            return website.datetime(2026, 9, 28, 12, 0, 0, tzinfo=tz)

    # The build start time is published, so both builds share one clock.
    monkeypatch.setattr(website, "datetime", FixedClock)
    capfd.readouterr()

    results = {}
    for setting in ("unset", "1"):
        if setting == "unset":
            monkeypatch.delenv(builder.PHASE_TIMING_ENVIRONMENT, raising=False)
        else:
            monkeypatch.setenv(builder.PHASE_TIMING_ENVIRONMENT, setting)
        output_root = tmp_path / f"site-{setting}"
        assert website.main(["build", "--output-root", str(output_root)]) == 0
        captured = capfd.readouterr()
        result = json.loads(captured.out)
        assert captured.out == website._json_bytes(result).decode() + "\n"
        results[setting] = (output_root, result, captured.err)

    silent_root, silent, silent_err = results["unset"]
    timed_root, timed, timed_err = results["1"]
    assert silent_err == ""
    # The stdout result differs only in the output path the caller chose.
    silent_tree = Path(silent.pop("path"))
    timed_tree = Path(timed.pop("path"))
    assert list(silent_root.iterdir()) == [silent_tree]
    assert list(timed_root.iterdir()) == [timed_tree]
    assert timed_tree.name == silent_tree.name
    assert timed == silent
    assert _published_files(timed_tree) == _published_files(silent_tree)

    line = re.compile(
        r"compatibility-website: phase (?P<name>[a-z0-9_.]+) "
        r"wall=\d+\.\ds cpu=\d+\.\ds children_cpu=\d+\.\ds"
    )
    names = []
    validations = []
    for text in timed_err.splitlines():
        match = line.fullmatch(text)
        assert match is not None, text
        if match["name"].startswith("validate."):
            validations.append(match["name"])
        else:
            names.append(match["name"])
    # Every Stage 1 validation in the timed build sees data the silent build
    # already validated in this process, so each one reuses that verdict.
    assert validations
    assert validations == ["validate.key", "validate.hit"] * (len(validations) // 2)
    # The live data is supplied directly here, so no stage1.* line appears.
    # verify_sources runs at both final publication boundaries inside
    # build.publish, and once more inside the CLI's recheck.
    assert names == [
        "build.stage1", "build.presentation", "build.stage1_tree", "compose",
        "verify_sources", "verify_sources", "build.publish",
        "verify_sources", "recheck",
    ]
    assert set(names) == {
        "build.stage1", "build.presentation", "build.stage1_tree", "compose",
        "verify_sources", "build.publish", "recheck",
    }
