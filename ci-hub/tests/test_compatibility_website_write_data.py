from __future__ import annotations

import copy
import gzip
import hashlib
import importlib.util
import json
import os
import socket
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/write_data.py"
SPEC = importlib.util.spec_from_file_location(
    "compatibility_website_write_data", SCRIPT
)
assert SPEC is not None and SPEC.loader is not None
write_data = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = write_data
SPEC.loader.exec_module(write_data)


TEST_ID = "tests/hello"
CELL_ID = f"{TEST_ID}/verify/ptrace"
TREE = "1" * 40


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def build_data_value() -> dict[str, Any]:
    identity = {
        "backend": "ptrace",
        "category": "tests",
        "lane": "portable",
        "mode": "verify",
        "test": TEST_ID,
    }
    comparison = {
        "canonicalizations": ["host-address-to-first-appearance-ordinal/v1"],
        "canonicalize_addresses": True,
        "compare_io_buffers": True,
        "compare_logs": True,
        "display_name": "BitwiseInfoV1",
        "exact_remainder": True,
        "full_trace": True,
        "ignore_lines": False,
        "log_scope": "info",
        "record_envelope": "all_records_v1",
        "skip_commit": False,
        "skip_detlog": False,
        "strictness": "canonical",
        "strip_lines": False,
        "stripped_prefixes": ["real-wall-clock-prefix/v1"],
        "virtualize_time": True,
    }
    verdict = {
        "bitwise_parity": True,
        "compared_log_messages": {"left": 1, "right": 1},
        "comparison": comparison,
        "comparison_tier": "canonical-bitwise",
        "state": "compared-and-matched",
    }
    validation_row = {
        "cell_results": {
            "hermit_sha": TREE,
            "population_sha256": sha256(canonical_bytes([identity])),
            "recorded": {
                "artifact": {"row_count": 1, "sha256": "3" * 64},
                "cell_count": 1,
                "cells": [{**identity, "cell_verdict": verdict}],
            },
            "run_id": "validate-run-1",
            "selected_by": {
                "super": {"cell_count": 1, "cells": [identity]},
            },
            "source_tree_dirty": False,
        },
        "commit": TREE,
        "finished_at": "2026-08-31T00:01:00Z",
        "history_row_sha256": "4" * 64,
        "profile": "super",
        "record_id": "record-1",
        "repo": "hermit",
        "result": "pass",
        "run_id": "validate-run-1",
        "schema_version": 7,
        "selection_mode": "full",
        "started_at": "2026-08-31T00:00:00Z",
    }
    series_row = {
        "current_main_ancestry": True,
        "emitted_at": "2026-08-31T00:01:00Z",
        "event_id": "event-1",
        "event_type": "series.observation",
        "host": "ci-host",
        "producer": "validate",
        "product_result": "pass",
        "run_id": "validate-run-1",
        "schema": "stress-series/v3",
        "series": {
            "cell": CELL_ID,
            "failure_class": None,
            "host_capabilities": {
                "cpuid-faulting": {"present": True, "evidence": "fixture"},
                "kvm": {"present": True, "evidence": "fixture"},
            },
            "kernel_version": "fixture-kernel",
            "machine_shortname": "ci-host",
            "num_runs": 1,
            "outcome": "passed",
            "result": "pass",
            "run_index": 1,
            "source_tree_dirty": False,
            "tree": TREE,
        },
        "team": "hermit",
    }
    cell = {
        **identity,
        "cell": CELL_ID,
        "current_reproducer": {
            "argv": ["./ci/test-harness.rs", "--exact-cell", CELL_ID],
            "shell_command": f"./ci/test-harness.rs --exact-cell {CELL_ID}",
        },
        "current_reproducer_unavailable_reason": None,
        "event_ids": ["event-1"],
        "guest_args": [],
        "latest_event_ids": ["event-1"],
        "latest_result": "pass",
        "latest_result_time": "2026-08-31T00:01:00Z",
        "latest_result_tree": TREE,
        "not_applicable_reason": None,
        "not_selected_by_full_reason": None,
        "physical_row_count": 1,
        "qualifying_event_ids": ["event-1"],
        "qualifying_physical_row_count": 1,
        "qualifying_represented_run_count": 1,
        "represented_run_count": 1,
        "selected_by": {
            "super": [
                {
                    "event_ids": ["event-1"],
                    "observed_at": "2026-08-31T00:01:00Z",
                    "run_id": "validate-run-1",
                    "tree": TREE,
                }
            ]
        },
        "selected_by_full": True,
        "state": "green",
        "timeout_seconds": 60,
        "workdir": None,
    }
    test = {
        "build": None,
        "category": "tests",
        "cells": [CELL_ID],
        "description": "Small deterministic hello-world fixture.",
        "direct": None,
        "id": TEST_ID,
        "lane": "portable",
        "observation": {
            "artifacts": [],
            "status": True,
            "stderr": True,
            "stdout": True,
        },
        "occasional": False,
        "preprocessors": [],
        "program": "tests/hello",
        "requires": [],
    }
    counts = {
        "current_validation_run_row_count": 1,
        "green_cell_count": 1,
        "in_manifest_cell_count": 1,
        "never_measured_cell_count": 0,
        "physical_row_count": 1,
        "red_cell_count": 0,
        "represented_run_count": 1,
        "selected_by_full_custom_command_count": 0,
        "test_count": 1,
        "validation_history_row_count": 1,
    }
    runs = {
        "current_main_ancestry": {TREE: True},
        "current_validation_run_row_count": 1,
        "current_validation_run_rows": [validation_row],
        "projection_admitted_physical_row_count": 1,
        "projection_admitted_represented_run_count": 1,
        "projection_excluded_physical_row_count": 0,
        "projection_excluded_represented_run_count": 0,
        "projection_exclusion_counts": [],
        "product_results": {"event-1": "pass"},
        "projection_results": {"event-1": None},
        "schema_version": 3,
        "series_physical_row_count": 1,
        "series_represented_run_count": 1,
        "series_rows": [series_row],
        "validate_run_selection": {"event-1": {"path": "super", "selected": True}},
        "validation_history_row_count": 1,
        "validation_history_rows": [validation_row],
    }
    provenance = {
        "build_data_schema_version": 3,
        "canonical_input_bytes_sha256": "5" * 64,
        "canonical_input_provenance": {
            "parent_commit": "6" * 40,
            "published_ref": "6" * 40,
            "reader_ci_hub_tree": "7" * 40,
            "reader_commit": "6" * 40,
            "reader_files": [],
            "reader_files_sha256": sha256(canonical_bytes([])),
            "reader_hermit": {
                "checkout_commit": TREE,
                "gitlink_commit": TREE,
            },
            "source_count": 0,
            "sources": [],
            "sources_sha256": sha256(canonical_bytes([])),
        },
        "current_main_ancestry_sha256": sha256(canonical_bytes({TREE: True})),
        "current_validation_run_row_count": 1,
        "current_validation_run_rows_projection_sha256": "0" * 64,
        "current_validation_run_rows_sha256": "9" * 64,
        "hermit_commit": TREE,
        "hermit_main_commit": TREE,
        "manifest_metadata_bytes_sha256": "a" * 64,
        "manifest_metadata_projection_sha256": "0" * 64,
        "manifest_metadata_schema": 1,
        "manifest_schema": 3,
        "manifest_sha256": "b" * 64,
        "parent_commit": "6" * 40,
        "product_results_sha256": sha256(canonical_bytes({"event-1": "pass"})),
        "projection_checker_input_jsonl_sha256": "e" * 64,
        "projection_results_sha256": sha256(canonical_bytes({"event-1": None})),
        "reducer_schema_version": 3,
        "series_physical_row_count": 1,
        "series_represented_run_count": 1,
        "series_rows_projection_sha256": "0" * 64,
        "series_rows_sha256": "0" * 64,
        "series_schema_counts": {"stress-series/v3": 1},
        "validate_run_selection_sha256": sha256(
            canonical_bytes({"event-1": {"path": "super", "selected": True}})
        ),
        "validation_event_count": 1,
        "validation_events_sha256": "2" * 64,
        "validation_history_row_count": 1,
        "validation_history_rows_projection_sha256": "0" * 64,
        "validation_history_rows_sha256": "3" * 64,
    }
    value = {
        "cells": [cell],
        "counts": counts,
        "projection_audit": {"deliberate_omissions": []},
        "provenance": provenance,
        "runs": runs,
        "schema_version": 3,
        "selected_by_full_custom_commands": [],
        "tests": [test],
    }
    module = write_data._build_data_module()
    output = module._validated_output_runs(value)
    manifest = {
        "cells": {CELL_ID: {key: cell[key] for key in module._STAGE1_POLICY_CELL_KEYS}},
        "manifest_schema": provenance["manifest_schema"],
        "manifest_sha256": provenance["manifest_sha256"],
        "selected_by_full_custom_commands": value["selected_by_full_custom_commands"],
        "tests": value["tests"],
    }
    provenance.update(
        current_validation_run_rows_projection_sha256=module._digest(
            output["current_validation_run_rows"]
        ),
        manifest_metadata_projection_sha256=module._digest(manifest),
        series_rows_projection_sha256=module._digest(output["series_rows"]),
        validation_history_rows_projection_sha256=module._digest(
            output["validation_history_rows"]
        ),
    )
    return value


def test_cli_help_describes_authorities_and_atomic_refusal_contract() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--help"],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0
    assert result.stderr == ""
    help_text = " ".join(result.stdout.split())
    usage = result.stdout.split("\n\n", 1)[0]
    usage_options = {
        token
        for token in usage.replace("[", " ").replace("]", " ").split()
        if token.startswith("--")
    }
    assert usage_options == {
        "--parent",
        "--parent-commit",
        "--hermit",
        "--hermit-commit",
        "--hermit-main-commit",
        "--output",
    }
    for expected in (
        "Build, validate, and atomically publish the Stage 1 compatibility website data tree.",
        "--parent PARENT exact clean dev-hermit checkout whose canonical ledger and series are read",
        "--parent-commit PARENT_COMMIT full dev-hermit commit currently checked out by --parent",
        "--hermit HERMIT exact clean Hermit checkout used for manifest policy and ancestry",
        "--hermit-commit HERMIT_COMMIT full Hermit commit currently checked out by --hermit",
        "--hermit-main-commit HERMIT_MAIN_COMMIT full Hermit ancestry target; must equal the checkout's stable refs/remotes/origin/main",
        "--output OUTPUT destination directory for the complete four-file website data tree",
        "The complete data tree is validated before writing, then the whole destination tree is replaced atomically.",
        "A refusal leaves an existing destination unchanged or an absent destination absent and writes nothing to stdout.",
        "The output parent directory must already exist.",
        "An existing output must be a recognized prior compatibility artifact tree.",
        "Its root and data inventories must be exact; stale or extra entries are refused.",
        "Filesystem, system, home, checkout, workspace, and working-directory roots, paths containing '..', symlinks, and existing non-directory destinations are refused.",
        "Example, from a clean dev-hermit checkout:",
        "python3 ci-hub/compatibility-website/write_data.py --parent .",
        '--hermit-main-commit "$(git -C hermit rev-parse refs/remotes/origin/main)"',
        "--output /tmp/compatibility-website-data",
    ):
        assert expected in help_text


def test_render_and_write_one_coherent_artifact_tree(tmp_path: Path) -> None:
    source = build_data_value()
    first = write_data.render_tree(source)
    second = write_data.render_tree(source)

    expected_paths = {
        "build.json",
        "data/cells.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    }
    assert set(first) == expected_paths
    assert list(first) == sorted(expected_paths)
    assert first == second

    payloads = {
        "data/cells.json.gz": {
            "cells": source["cells"],
            "in_manifest_cell_count": 1,
            "projection_audit": {"deliberate_omissions": []},
            "schema_version": 3,
        },
        "data/runs.json.gz": source["runs"],
        "data/tests.json.gz": {
            "schema_version": 3,
            "selected_by_full_custom_command_count": 0,
            "selected_by_full_custom_commands": [],
            "test_count": 1,
            "tests": source["tests"],
        },
    }
    for path, payload in payloads.items():
        compressed = first[path]
        assert compressed[:4] == b"\x1f\x8b\x08\x00"
        assert compressed[4:8] == b"\x00\x00\x00\x00"
        assert compressed[9] == 255
        assert gzip.decompress(compressed) == canonical_bytes(payload) + b"\n"

    manifest = json.loads(first["build.json"])
    assert first["build.json"] == canonical_bytes(manifest) + b"\n"
    assert manifest["schema_version"] == 3
    assert manifest["generator"] == {
        "build_data_schema_version": 3,
        "name": "compatibility-website-stage-1-data",
        "write_data_schema_version": 3,
    }
    assert manifest["provenance"] == source["provenance"]
    assert manifest["counts"] == source["counts"]
    assert "incomplete_data_categories" not in manifest
    assert manifest["build_data_sha256"] == sha256(canonical_bytes(source))

    artifacts = manifest["artifacts"]
    assert [artifact["path"] for artifact in artifacts] == sorted(payloads)
    for artifact in artifacts:
        path = artifact["path"]
        compressed = first[path]
        uncompressed = gzip.decompress(compressed)
        assert artifact == {
            "compressed_bytes": len(compressed),
            "compressed_sha256": sha256(compressed),
            "content_encoding": "gzip",
            "content_type": "application/json",
            "path": path,
            "uncompressed_bytes": len(uncompressed),
            "uncompressed_sha256": sha256(uncompressed),
        }
    assert manifest["artifacts_sha256"] == sha256(canonical_bytes(artifacts))

    destination = tmp_path / "site-data"
    written_manifest = write_data.write_data(source, destination)
    assert written_manifest == manifest
    written_paths = {
        path.relative_to(destination).as_posix()
        for path in destination.rglob("*")
        if path.is_file()
    }
    assert written_paths == expected_paths
    assert {path: (destination / path).read_bytes() for path in expected_paths} == first


def test_private_renderer_matches_public_four_file_bytes_and_public_validates(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    source = build_data_value()
    module = write_data._build_data_module()
    validate = module.validate_build_data
    validated = validate(source)
    expected = write_data._render_validated_tree(validated)
    calls = []

    def record(value: Any) -> dict[str, Any]:
        calls.append(value)
        return validate(value)

    monkeypatch.setattr(module, "validate_build_data", record)
    assert write_data.render_tree(source) == expected
    manifest = write_data.write_data(source, tmp_path / "site-data")
    assert manifest == json.loads(expected["build.json"])
    assert calls == [source, source]
    assert len(expected) == 4


def test_public_write_refuses_mutation_without_partial_tree(tmp_path: Path) -> None:
    source = build_data_value()
    source["counts"]["in_manifest_cell_count"] = 2
    destination = tmp_path / "site-data"

    with pytest.raises(write_data.WriteRefused, match="exact persisted populations"):
        write_data.write_data(source, destination)

    assert not destination.exists()
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_cli_private_renderer_receives_only_immediate_build_result(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capfd: pytest.CaptureFixture[str],
) -> None:
    built = build_data_value()
    module = write_data._build_data_module()
    rendered = []
    render = write_data._render_validated_tree
    monkeypatch.setattr(module, "build_data", lambda *_args: built)
    monkeypatch.setattr(
        module,
        "validate_build_data",
        lambda _value: (_ for _ in ()).throw(
            AssertionError("CLI revalidated build_data result")
        ),
    )

    def record(value: Any) -> dict[str, bytes]:
        assert value is built
        rendered.append(value)
        return render(value)

    monkeypatch.setattr(write_data, "_render_validated_tree", record)
    destination = tmp_path / "site-data"
    result = write_data.main(
        [
            "--parent",
            "/unused",
            "--parent-commit",
            "1" * 40,
            "--hermit",
            "/unused",
            "--hermit-commit",
            "2" * 40,
            "--hermit-main-commit",
            "3" * 40,
            "--output",
            str(destination),
        ]
    )

    captured = capfd.readouterr()
    assert result == 0
    assert captured.err == ""
    assert json.loads(captured.out)["schema_version"] == 3
    assert rendered == [built]
    assert len(_tree_bytes(destination)) == 4


def _changed_current_row(value: dict[str, Any]) -> None:
    current = copy.deepcopy(value["runs"]["current_validation_run_rows"][0])
    current["result"] = "fail"
    value["runs"]["current_validation_run_rows"][0] = current


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        (lambda value: value.update(cells={}), "cells must be an array"),
        (_changed_current_row, "not a sub-multiset"),
        (
            lambda value: value["runs"].update(validation_history_row_count=0),
            "runs validation history count",
        ),
        (
            lambda value: value["cells"][0].update(measured=True),
            "unknown field",
        ),
        (lambda value: value["provenance"].update(token="secret"), "unknown field"),
        (
            lambda value: value["runs"]["current_main_ancestry"].update(
                {"2" * 40: False}
            ),
            "provenance digests disagree",
        ),
    ],
)
def test_render_refuses_invalid_build_data(mutation: Any, message: str) -> None:
    value = build_data_value()
    mutation(value)
    with pytest.raises(write_data.WriteRefused, match=message):
        write_data.render_tree(value)


def test_cli_reports_build_refusal_without_traceback_or_partial_stdout(
    monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
) -> None:
    module = write_data._build_data_module()
    monkeypatch.setattr(
        module,
        "build_data",
        lambda *_args: (_ for _ in ()).throw(module.BuildRefused("invalid authority")),
    )

    result = write_data.main(
        [
            "--parent",
            "/unused",
            "--parent-commit",
            "1" * 40,
            "--hermit",
            "/unused",
            "--hermit-commit",
            "2" * 40,
            "--hermit-main-commit",
            "3" * 40,
            "--output",
            "/unused",
        ]
    )

    captured = capfd.readouterr()
    assert result == 2
    assert captured.out == ""
    assert "REFUSED: build data refused: invalid authority" in captured.err
    assert "Traceback" not in captured.err


def _tree_bytes(root: Path) -> dict[str, bytes]:
    return {
        path.relative_to(root).as_posix(): path.read_bytes()
        for path in root.rglob("*")
        if path.is_file()
    }


def _tree_state(root: Path) -> dict[str, tuple[Any, ...]]:
    state = {}
    for path in root.rglob("*"):
        metadata = path.lstat()
        relative = path.relative_to(root).as_posix()
        if stat.S_ISREG(metadata.st_mode):
            state[relative] = ("file", path.read_bytes())
        else:
            state[relative] = (
                stat.S_IFMT(metadata.st_mode),
                metadata.st_dev,
                metadata.st_ino,
                metadata.st_rdev,
            )
    return state


def _subprocess_write(
    source: dict[str, Any], destination: Path, tmp_path: Path
) -> subprocess.CompletedProcess[bytes]:
    source_path = tmp_path / "source.json"
    source_path.write_bytes(canonical_bytes(source) + b"\n")
    program = """
import importlib.util
import json
import sys
from pathlib import Path

script, source_path, destination = map(Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location("compatibility_writer_probe", script)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
source = json.loads(source_path.read_bytes())
try:
    module.write_data(source, destination)
except module.WriteRefused as error:
    print(f"REFUSED: {error}", file=sys.stderr)
    raise SystemExit(2)
raise SystemExit(0)
"""
    environment = dict(os.environ)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return subprocess.run(
        [
            sys.executable,
            "-c",
            program,
            str(SCRIPT),
            str(source_path),
            str(destination),
        ],
        check=False,
        capture_output=True,
        env=environment,
        timeout=5,
    )


def _seed_prior_tree(destination: Path) -> dict[str, bytes]:
    write_data.write_data(build_data_value(), destination)
    return _tree_bytes(destination)


def test_success_replaces_an_exact_recognized_prior_tree(tmp_path: Path) -> None:
    destination = tmp_path / "site-data"
    _seed_prior_tree(destination)
    expected = write_data.render_tree(build_data_value())
    write_data.write_data(build_data_value(), destination)

    assert _tree_bytes(destination) == expected
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_parent_fsync_failure_rolls_back_without_partial_stdout(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capfd: pytest.CaptureFixture[str],
) -> None:
    destination = tmp_path / "site-data"
    before = _seed_prior_tree(destination)
    real_fsync = write_data._fsync_directory
    failed = False

    def fail_once(path: Path | int) -> None:
        nonlocal failed
        if (
            isinstance(path, int)
            and os.path.samestat(os.fstat(path), os.stat(destination.parent))
            and not failed
        ):
            failed = True
            raise OSError("injected parent fsync failure")
        real_fsync(path)

    module = write_data._build_data_module()
    monkeypatch.setattr(module, "build_data", lambda *_args: build_data_value())
    monkeypatch.setattr(write_data, "_fsync_directory", fail_once)
    result = write_data.main(
        [
            "--parent",
            "/unused",
            "--parent-commit",
            "1" * 40,
            "--hermit",
            "/unused",
            "--hermit-commit",
            "2" * 40,
            "--hermit-main-commit",
            "3" * 40,
            "--output",
            str(destination),
        ]
    )

    captured = capfd.readouterr()
    assert result == 2
    assert captured.out == ""
    assert "prior tree restored" in captured.err
    assert _tree_bytes(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_failed_rollback_preserves_the_prior_tree_beside_complete_new_tree(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination = tmp_path / "site-data"
    before = _seed_prior_tree(destination)
    expected = write_data.render_tree(build_data_value())
    real_rename, real_fsync = write_data._renameat2, write_data._fsync_directory
    exchanges = 0
    failed_sync = False

    def rename(directory: int, left: str, right: str, flags: int) -> None:
        nonlocal exchanges
        exchanges += 1
        if exchanges == 2:
            raise write_data.WriteRefused("injected rollback failure")
        real_rename(directory, left, right, flags)

    def fsync(path: Path | int) -> None:
        nonlocal failed_sync
        if (
            isinstance(path, int)
            and os.path.samestat(os.fstat(path), os.stat(destination.parent))
            and not failed_sync
        ):
            failed_sync = True
            raise OSError("injected parent fsync failure")
        real_fsync(path)

    monkeypatch.setattr(write_data, "_renameat2", rename)
    monkeypatch.setattr(write_data, "_fsync_directory", fsync)
    with pytest.raises(write_data.RollbackRefused, match="prior tree remains at"):
        write_data.write_data(build_data_value(), destination)

    assert _tree_bytes(destination) == expected
    preserved = list(tmp_path.glob(".site-data.tmp-*"))
    assert len(preserved) == 1
    assert _tree_bytes(preserved[0]) == before


@pytest.mark.parametrize("endpoint", ["destination", "temporary"])
def test_rollback_endpoint_spoof_refuses_without_exchange_or_deletion(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, endpoint: str
) -> None:
    destination = tmp_path / "site-data"
    prior = _seed_prior_tree(destination)
    expected = write_data.render_tree(build_data_value())
    moved = tmp_path / f"moved-{endpoint}"
    real_fsync = write_data._fsync_directory
    spoofed = False

    def fsync(path: Path | int) -> None:
        nonlocal spoofed
        if (
            isinstance(path, int)
            and os.path.samestat(os.fstat(path), os.stat(destination.parent))
            and not spoofed
        ):
            spoofed = True
            temporary = next(tmp_path.glob(".site-data.tmp-*"))
            target = destination if endpoint == "destination" else temporary
            target.rename(moved)
            target.mkdir()
            (target / "valuable").write_bytes(b"keep\n")
            raise OSError("injected parent fsync failure after endpoint spoof")
        real_fsync(path)

    monkeypatch.setattr(write_data, "_fsync_directory", fsync)
    with pytest.raises(write_data.RollbackRefused) as caught:
        write_data.write_data(build_data_value(), destination)

    assert spoofed
    assert "rollback was not attempted" in str(caught.value)
    assert "restored" not in str(caught.value)
    temporary = next(tmp_path.glob(".site-data.tmp-*"))
    if endpoint == "destination":
        assert _tree_bytes(destination) == {"valuable": b"keep\n"}
        assert _tree_bytes(moved) == expected
        assert _tree_bytes(temporary) == prior
    else:
        assert _tree_bytes(destination) == expected
        assert _tree_bytes(moved) == prior
        assert _tree_bytes(temporary) == {"valuable": b"keep\n"}


def test_failed_rollback_sync_reports_the_restored_prior_tree(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination = tmp_path / "site-data"
    before = _seed_prior_tree(destination)
    real_fsync = write_data._fsync_directory
    parent_syncs = 0

    def fsync(path: Path | int) -> None:
        nonlocal parent_syncs
        if isinstance(path, int) and os.path.samestat(
            os.fstat(path), os.stat(destination.parent)
        ):
            parent_syncs += 1
            if parent_syncs <= 2:
                raise OSError(f"injected parent fsync failure {parent_syncs}")
        real_fsync(path)

    monkeypatch.setattr(write_data, "_fsync_directory", fsync)
    with pytest.raises(
        write_data.WriteRefused,
        match="prior tree restored, but its directory sync failed",
    ):
        write_data.write_data(build_data_value(), destination)

    assert _tree_bytes(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_failed_initial_publish_rollback_does_not_invent_a_prior_tree(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination = tmp_path / "site-data"
    expected = write_data.render_tree(build_data_value())
    real_rename, real_fsync = write_data._renameat2, write_data._fsync_directory
    renames = 0

    def rename(directory: int, source: str, target: str, flags: int) -> None:
        nonlocal renames
        renames += 1
        if renames == 2:
            raise OSError("injected rollback failure")
        real_rename(directory, source, target, flags)

    def fsync(path: Path | int) -> None:
        if isinstance(path, int) and os.path.samestat(
            os.fstat(path), os.stat(destination.parent)
        ):
            raise OSError("injected parent fsync failure")
        real_fsync(path)

    monkeypatch.setattr(write_data, "_renameat2", rename)
    monkeypatch.setattr(write_data, "_fsync_directory", fsync)
    with pytest.raises(write_data.RollbackRefused, match="no prior tree existed"):
        write_data.write_data(build_data_value(), destination)

    assert _tree_bytes(destination) == expected
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_write_failure_and_symlink_destination_preserve_existing_bytes(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination = tmp_path / "site-data"
    before = _seed_prior_tree(destination)
    monkeypatch.setattr(
        write_data,
        "_write_file",
        lambda *_args: (_ for _ in ()).throw(OSError("injected write failure")),
    )
    with pytest.raises(write_data.WriteRefused, match="injected write failure"):
        write_data.write_data(build_data_value(), destination)
    assert _tree_bytes(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))

    target = tmp_path / "target"
    target.mkdir()
    link = tmp_path / "linked-output"
    link.symlink_to(target, target_is_directory=True)
    with pytest.raises(write_data.WriteRefused, match="symlink"):
        write_data.write_data(build_data_value(), link)
    assert _tree_bytes(target) == {}

    with pytest.raises(write_data.WriteRefused, match="path traversal"):
        write_data.write_data(build_data_value(), tmp_path / "child" / ".." / "escape")


def test_arbitrary_directory_and_checkout_are_never_replaced(tmp_path: Path) -> None:
    for name, checkout in (("arbitrary", False), ("checkout", True)):
        destination = tmp_path / name
        destination.mkdir()
        (destination / "valuable").write_bytes(b"keep\n")
        if checkout:
            (destination / ".git").mkdir()
        before = _tree_bytes(destination)
        message = "checkout or workspace" if checkout else "inventory is not exact"
        with pytest.raises(write_data.WriteRefused, match=message):
            write_data.write_data(build_data_value(), destination)
        assert _tree_bytes(destination) == before
        assert not list(tmp_path.glob(f".{name}.tmp-*"))


@pytest.mark.parametrize("location", ["root", "data"])
def test_extra_prior_inventory_refuses_and_preserves_every_byte(
    tmp_path: Path, location: str
) -> None:
    destination = tmp_path / "site-data"
    _seed_prior_tree(destination)
    extra = destination / ("stale.txt" if location == "root" else "data/stale.json.gz")
    extra.write_bytes(b"valuable stale bytes\n")
    before = _tree_bytes(destination)

    with pytest.raises(write_data.WriteRefused, match="inventory is not exact"):
        write_data.write_data(build_data_value(), destination)

    assert _tree_bytes(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))


@pytest.mark.parametrize("injected", [".git", "valuable"])
def test_post_recognition_injection_rolls_back_without_deleting_prior_bytes(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, injected: str
) -> None:
    destination = tmp_path / "site-data"
    before = _seed_prior_tree(destination)
    recognize = write_data._recognize_prior
    calls = 0

    def inject_after_recognition(directory: int) -> dict[str, str]:
        nonlocal calls
        result = recognize(directory)
        calls += 1
        if calls == 1:
            path = destination / injected
            if injected == ".git":
                path.mkdir()
            else:
                path.write_bytes(b"keep injected bytes\n")
        return result

    monkeypatch.setattr(write_data, "_recognize_prior", inject_after_recognition)
    with pytest.raises(write_data.WriteRefused, match="prior tree restored"):
        write_data.write_data(build_data_value(), destination)

    assert calls == 1
    assert all(
        (destination / path).read_bytes() == contents
        for path, contents in before.items()
    )
    if injected == ".git":
        assert (destination / injected).is_dir()
    else:
        assert (destination / injected).read_bytes() == b"keep injected bytes\n"
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_post_recognition_artifact_mutation_is_revalidated_before_deletion(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination = tmp_path / "site-data"
    before = _seed_prior_tree(destination)
    recognize = write_data._recognize_prior
    calls = 0

    def mutate_after_recognition(directory: int) -> dict[str, str]:
        nonlocal calls
        result = recognize(directory)
        calls += 1
        if calls == 1:
            artifact = destination / "data/runs.json.gz"
            artifact.write_bytes(artifact.read_bytes() + b"changed")
        return result

    monkeypatch.setattr(write_data, "_recognize_prior", mutate_after_recognition)
    with pytest.raises(write_data.WriteRefused, match="prior tree restored"):
        write_data.write_data(build_data_value(), destination)

    assert calls == 1
    after = _tree_bytes(destination)
    assert after["data/runs.json.gz"] == before["data/runs.json.gz"] + b"changed"
    assert {
        path: contents
        for path, contents in after.items()
        if path != "data/runs.json.gz"
    } == {
        path: contents
        for path, contents in before.items()
        if path != "data/runs.json.gz"
    }
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_expected_only_cleanup_refuses_unknown_names_before_deleting(
    tmp_path: Path,
) -> None:
    retained = tmp_path / "retained"
    retained.mkdir()
    (retained / "build.json").write_bytes(b"expected name\n")
    (retained / "valuable").write_bytes(b"keep\n")
    parent = os.open(tmp_path, write_data._DIR_FLAGS)
    directory = write_data._open_directory(parent, retained.name)
    try:
        with pytest.raises(write_data.WriteRefused, match="unexpected entries"):
            write_data._remove_expected_tree(
                parent,
                retained.name,
                directory,
                complete=False,
            )
    finally:
        os.close(directory)
        os.close(parent)

    assert (retained / "build.json").read_bytes() == b"expected name\n"
    assert (retained / "valuable").read_bytes() == b"keep\n"


def test_roots_home_workspace_and_dot_are_refused(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capfd: pytest.CaptureFixture[str]
) -> None:
    sentinel = tmp_path / "valuable"
    sentinel.write_bytes(b"keep\n")
    environment_home = tmp_path / "configured-home"
    environment_home.mkdir()
    account_home = Path(write_data.pwd.getpwuid(os.geteuid()).pw_dir).resolve()
    monkeypatch.setenv("HOME", str(environment_home))
    monkeypatch.chdir(tmp_path)
    module = write_data._build_data_module()
    monkeypatch.setattr(module, "build_data", lambda *_args: build_data_value())
    result = write_data.main(
        [
            "--parent",
            "/unused",
            "--parent-commit",
            "1" * 40,
            "--hermit",
            "/unused",
            "--hermit-commit",
            "2" * 40,
            "--hermit-main-commit",
            "3" * 40,
            "--output",
            ".",
        ]
    )
    captured = capfd.readouterr()
    assert (
        result == 2 and captured.out == "" and "working-directory root" in captured.err
    )
    for destination in (
        Path("/"),
        Path("/tmp"),
        Path.home(),
        account_home,
        REPOSITORY,
    ):
        with pytest.raises(
            write_data.WriteRefused,
            match="root|recognized compatibility artifact tree|checkout or workspace",
        ):
            write_data.write_data(build_data_value(), destination)
    assert sentinel.read_bytes() == b"keep\n"


def test_corrupt_prior_artifact_is_not_recognized_or_replaced(tmp_path: Path) -> None:
    destination = tmp_path / "site-data"
    _seed_prior_tree(destination)
    artifact = destination / "data/runs.json.gz"
    artifact.write_bytes(artifact.read_bytes() + b"changed")
    before = _tree_bytes(destination)

    with pytest.raises(write_data.WriteRefused, match="prior artifact file"):
        write_data.write_data(build_data_value(), destination)

    assert _tree_bytes(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))


@pytest.mark.parametrize("relative", ["build.json", "data/runs.json.gz"])
def test_fifo_in_prior_artifact_position_refuses_without_hanging_or_mutating(
    tmp_path: Path, relative: str
) -> None:
    destination = tmp_path / "site-data"
    _seed_prior_tree(destination)
    special = destination / relative
    special.unlink()
    os.mkfifo(special, 0o600)
    before = _tree_state(destination)

    result = _subprocess_write(build_data_value(), destination, tmp_path)

    assert result.returncode == 2
    assert result.stdout == b""
    assert b"not a regular file" in result.stderr
    assert _tree_state(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_unix_socket_in_prior_artifact_position_refuses_without_mutating(
    tmp_path: Path,
) -> None:
    destination = tmp_path / "site-data"
    _seed_prior_tree(destination)
    special = destination / "data/tests.json.gz"
    special.unlink()
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(str(special))
        before = _tree_state(destination)
        result = _subprocess_write(build_data_value(), destination, tmp_path)

        assert result.returncode == 2
        assert result.stdout == b""
        assert b"not a regular file" in result.stderr
        assert _tree_state(destination) == before
        assert not list(tmp_path.glob(".site-data.tmp-*"))
    finally:
        listener.close()


def test_available_device_refuses_as_nonregular_without_output(
    capfd: pytest.CaptureFixture[str],
) -> None:
    device = Path("/dev/null")
    if not device.exists():
        pytest.skip("/dev/null is unavailable")
    assert write_data._FILE_FLAGS & os.O_NONBLOCK
    before = device.stat()
    parent = os.open(device.parent, write_data._DIR_FLAGS)
    try:
        with pytest.raises(write_data.WriteRefused, match="not a regular file"):
            write_data._open_regular(parent, device.name)
    finally:
        os.close(parent)

    after = device.stat()
    assert (before.st_dev, before.st_ino, before.st_mode, before.st_rdev) == (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_rdev,
    )
    captured = capfd.readouterr()
    assert captured.out == "" and captured.err == ""


def test_fifo_swap_between_entry_check_and_open_refuses_nonblocking(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capfd: pytest.CaptureFixture[str],
) -> None:
    candidate = tmp_path / "build.json"
    candidate.write_bytes(b"regular before the race\n")
    parent = os.open(tmp_path, write_data._DIR_FLAGS)
    entry = write_data._entry
    swapped = False

    def swap_after_check(directory: int, name: str) -> os.stat_result | None:
        nonlocal swapped
        observed = entry(directory, name)
        if not swapped:
            swapped = True
            candidate.unlink()
            os.mkfifo(candidate, 0o600)
        return observed

    assert write_data._FILE_FLAGS & os.O_NONBLOCK
    monkeypatch.setattr(write_data, "_entry", swap_after_check)
    try:
        with pytest.raises(write_data.WriteRefused, match="stable regular file"):
            write_data._open_regular(parent, candidate.name)
    finally:
        os.close(parent)

    assert swapped and stat.S_ISFIFO(candidate.lstat().st_mode)
    captured = capfd.readouterr()
    assert captured.out == "" and captured.err == ""


@pytest.mark.parametrize("location", ["build.json", "artifact"])
def test_deep_prior_json_is_a_normal_cli_refusal_without_mutation(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capfd: pytest.CaptureFixture[str],
    location: str,
) -> None:
    destination = tmp_path / "site-data"
    _seed_prior_tree(destination)
    depth = 100_000
    deep = b"[" * depth + b"0" + b"]" * depth + b"\n"
    assert len(deep) < 1 << 20
    if location == "build.json":
        (destination / "build.json").write_bytes(deep)
    else:
        compressed = write_data._gzip_bytes(deep)
        artifact = destination / "data/runs.json.gz"
        artifact.write_bytes(compressed)
        manifest_path = destination / "build.json"
        manifest = json.loads(manifest_path.read_bytes())
        row = next(
            item
            for item in manifest["artifacts"]
            if item["path"] == "data/runs.json.gz"
        )
        row.update(
            compressed_bytes=len(compressed),
            compressed_sha256=sha256(compressed),
            uncompressed_bytes=len(deep),
            uncompressed_sha256=sha256(deep),
        )
        manifest["artifacts_sha256"] = sha256(
            write_data._json_bytes(manifest["artifacts"])
        )
        manifest_path.write_bytes(write_data._json_bytes(manifest) + b"\n")
    before = _tree_bytes(destination)

    module = write_data._build_data_module()
    monkeypatch.setattr(module, "build_data", lambda *_args: build_data_value())
    result = write_data.main(
        [
            "--parent",
            "/unused",
            "--parent-commit",
            "1" * 40,
            "--hermit",
            "/unused",
            "--hermit-commit",
            "2" * 40,
            "--hermit-main-commit",
            "3" * 40,
            "--output",
            str(destination),
        ]
    )

    captured = capfd.readouterr()
    assert result == 2
    assert captured.out == ""
    assert "REFUSED:" in captured.err
    assert "Traceback" not in captured.err
    assert _tree_bytes(destination) == before
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_absent_destination_race_cannot_replace_new_entry(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination, target = tmp_path / "site-data", tmp_path / "valuable-target"
    target.mkdir()
    (target / "valuable").write_bytes(b"keep\n")
    real_rename = write_data._renameat2
    raced = False

    def rename(directory: int, source: str, destination_name: str, flags: int) -> None:
        nonlocal raced
        if not raced and flags == write_data._RENAME_NOREPLACE:
            raced = True
            destination.symlink_to(target, target_is_directory=True)
        real_rename(directory, source, destination_name, flags)

    monkeypatch.setattr(write_data, "_renameat2", rename)
    with pytest.raises(write_data.WriteRefused, match="File exists"):
        write_data.write_data(build_data_value(), destination)
    assert destination.is_symlink()
    assert _tree_bytes(target) == {"valuable": b"keep\n"}
    assert not list(tmp_path.glob(".site-data.tmp-*"))


def test_existing_destination_spoof_refuses_rollback_without_deleting_any_tree(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    destination, displaced = tmp_path / "site-data", tmp_path / "moved-prior"
    prior = _seed_prior_tree(destination)
    expected = write_data.render_tree(build_data_value())
    real_rename = write_data._renameat2
    raced = False

    def rename(directory: int, source: str, target: str, flags: int) -> None:
        nonlocal raced
        if not raced and flags == write_data._RENAME_EXCHANGE:
            raced = True
            destination.rename(displaced)
            destination.mkdir()
            (destination / "valuable").write_bytes(b"keep\n")
        real_rename(directory, source, target, flags)

    monkeypatch.setattr(write_data, "_renameat2", rename)
    with pytest.raises(write_data.RollbackRefused, match="rollback was not attempted"):
        write_data.write_data(build_data_value(), destination)
    assert _tree_bytes(destination) == expected
    assert _tree_bytes(displaced) == prior
    retained = list(tmp_path.glob(".site-data.tmp-*"))
    assert len(retained) == 1
    assert _tree_bytes(retained[0]) == {"valuable": b"keep\n"}


def test_parent_symlink_race_rolls_back_through_bound_directory(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, displaced, target = (
        tmp_path / "publish",
        tmp_path / "moved",
        tmp_path / "target",
    )
    parent.mkdir()
    target.mkdir()
    destination = parent / "site-data"
    real_rename = write_data._renameat2
    raced = False

    def rename(directory: int, source: str, target_name: str, flags: int) -> None:
        nonlocal raced
        if not raced:
            raced = True
            parent.rename(displaced)
            parent.symlink_to(target, target_is_directory=True)
        real_rename(directory, source, target_name, flags)

    monkeypatch.setattr(write_data, "_renameat2", rename)
    with pytest.raises(write_data.WriteRefused, match="parent changed during"):
        write_data.write_data(build_data_value(), destination)
    assert parent.is_symlink() and _tree_bytes(target) == {}
    assert not (displaced / "site-data").exists()
    assert not list(displaced.glob(".site-data.tmp-*"))
