from __future__ import annotations

import copy
import errno
import fcntl
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
    "ledger",
    "series",
    "flaky_cells",
    "flake_class",
    "gate_completeness",
    "green_class",
    "qualifying_receipt",
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


def build_data(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, Any]:
    return renderer_test.representative_build_data(tmp_path, monkeypatch)


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
    assert len(first) == 27
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
    assert (
        first_manifest["incomplete_data_categories"]
        == json.loads(stage_1["build.json"])["incomplete_data_categories"]
    )
    for path in website._modules().writer._DATA_PATHS:
        assert first[path] == stage_1[path]
    html = "\n".join(
        value.decode() for path, value in first.items() if path.endswith(".html")
    )
    assert (
        "Recorded data — source commits are on Overview; incomplete data is on "
        "Incomplete data"
    ) in html
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
    closing_row = b'    Path("ci-hub/validate/qualifying-receipt.json"),\n)'
    assert source.count(closing_row) == 1
    changed = source.replace(
        closing_row,
        b'    Path("ci-hub/validate/qualifying-receipt.json"),\n'
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

    assert readers["flake_class"].flaky_registry is readers["flaky_cells"]
    assert (
        readers["gate_completeness"].gate_counts is readers["flake_class"].gate_counts
    )
    qualifying = readers["qualifying_receipt"]
    assert qualifying.CellResultsVerdict.SATISFIED.value == "satisfied"
    with pytest.raises(RuntimeError, match="not available to the website reader"):
        qualifying.row_qualification
    assert sys.path == before_path
    for name in CAPTURED_READER_MODULE_NAMES:
        assert (name in sys.modules) is (name in present_modules)
        if name in present_modules:
            assert sys.modules[name] is before_modules[name]


def test_captured_reader_graph_refuses_a_new_qualifying_receipt_import() -> None:
    captured = website._capture_generator_bytes()
    relative = "ci-hub/ledger/validate_rows.py"
    source = captured[relative]
    needle = b"import qualifying_receipt  # noqa: E402\n"
    assert source.count(needle) == 1
    changed = dict(captured)
    changed[relative] = source.replace(
        needle,
        needle + b"from qualifying_receipt import row_qualification\n",
    )
    before_modules = {
        name: sys.modules.get(name) for name in CAPTURED_READER_MODULE_NAMES
    }
    present_modules = {
        name for name in CAPTURED_READER_MODULE_NAMES if name in sys.modules
    }
    before_path = list(sys.path)

    with pytest.raises(
        website.WebsiteRefused, match="not available to the website reader"
    ):
        website._captured_reader_modules(changed)
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


def test_preloaded_generator_refuses_a_later_dependency_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = build_data(tmp_path, monkeypatch)
    website._modules.cache_clear()
    website._modules()
    changed = website._capture_generator_bytes()
    relative = "ci-hub/compatibility-website/build_data.py"
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


def test_build_data_uses_only_the_captured_qualifying_receipt_surface(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    data = build_data(tmp_path, monkeypatch)
    builder = website._modules().build_data
    qualifying = builder._modules().qualifying_receipt

    def refuse_live_read(*_args: Any, **_kwargs: Any) -> str:
        raise AssertionError("build data read qualifying policy from a live path")

    monkeypatch.setattr(Path, "read_text", refuse_live_read)
    row = data["runs"]["current_validation_run_rows"][0]
    verdict = builder._cell_results_verdict(row)
    assert verdict is qualifying.cell_results_verdict(dict(row), qualifying.active())
    with pytest.raises(RuntimeError, match="not available to the website reader"):
        qualifying.producer_definition_allowed


@pytest.mark.parametrize("variable", website.QUALIFICATION_OVERRIDE_ENVIRONMENT)
def test_direct_serve_refuses_qualification_authority_overrides(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    variable: str,
) -> None:
    built = website.publish(build_data(tmp_path, monkeypatch), tmp_path / "published")
    target = built.path
    built.close()
    monkeypatch.setenv(variable, str(tmp_path / "external-authority"))

    with pytest.raises(website.WebsiteRefused, match="authority override"):
        website.validate_website(target)


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


def test_manifest_artifact_count_must_match_the_rows_that_require_pages(
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

    with pytest.raises(website.WebsiteRefused, match="artifact count"):
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
def test_build_recaptures_and_compares_all_five_stage1_files_at_final_boundaries(
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
    projection = builder._manifest_data(
        {
            "cells": changed["cells"],
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


def test_help_is_bounded_and_names_all_three_modes() -> None:
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


@pytest.mark.parametrize("mode", ["root", "build", "serve", "build-and-serve"])
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
