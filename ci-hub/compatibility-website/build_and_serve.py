#!/usr/bin/env python3
"""Build, validate, and serve one immutable compatibility website tree."""

from __future__ import annotations

import argparse
import ctypes
import errno
import fcntl
import functools
import hashlib
import importlib.util
import json
import os
import signal
import stat
import subprocess
import sys
import uuid
from collections.abc import Callable, Collection, Mapping, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from types import SimpleNamespace
from typing import Any


try:
    _BOOTSTRAP_SOURCE_BYTES
except NameError:
    _BOOTSTRAP_SOURCE_BYTES: bytes | None = None
_EXECUTING_MODULE_CODE = sys._getframe().f_code
_IMPORT_SOURCE_BYTES = Path(__file__).read_bytes()


REPOSITORY = Path(__file__).resolve().parents[2]
BUILD_DATA_PATH = Path(__file__).resolve().with_name("build_data.py")
WRITE_DATA_PATH = Path(__file__).resolve().with_name("write_data.py")
RENDER_PAGE_PATH = Path(__file__).resolve().with_name("render_page.py")
SERVE_PATH = Path(__file__).resolve().with_name("serve.rs")
SERVE_PRELUDE_PATH = REPOSITORY / "hermit/scripts/lib/rust_script_prelude.rs"
DEFAULT_OUTPUT_ROOT = REPOSITORY.parent / f"{REPOSITORY.name}-compatibility-website"
SCHEMA_VERSION = 1
STAGE_1_SCHEMA_VERSION = 3
# The website build.json embeds the Stage 1 build record twice (as
# `inputs.stage_1_build_json` and as `provenance`), and both copies carry the
# finalized-validation proof, which holds the exact cell-results bytes of every
# schema-10 terminal the ledger carries. It therefore grows with every finalized
# run, about 3.2 MB per full run (a 1.6 MB cells object, counted twice).
# Measured: 8,896,094 bytes in the build published 2026-09-27 (22 proof
# objects); 19,498,083 bytes on 2026-09-28 at ledger 7d28c1f0 (25 objects),
# which the old 16 MiB bound refused after a complete Stage 1 build, so no
# website was published. The bound that matters is external: the site is
# pushed to the gh-pages branch, and GitHub rejects a pushed file over 100 MiB.
# Stay under that; removing the doubled proof is the structural fix.
MAX_BUILD_JSON_BYTES = 96 << 20
MAX_ARTIFACT_BYTES = 1 << 30
MAX_GIT_OID_STDOUT_BYTES = 128
MAX_GIT_OID_STDERR_BYTES = 512
FIXED_DIRECTORIES = ("assets", "cells", "data", "runs", "tests")
CHECKOUT_MARKERS = frozenset({".git", ".hg", ".sl", ".svn", ".eden"})
COORDINATOR_PATH = "ci-hub/compatibility-website/build_and_serve.py"
STAGE_1_DATA_PATHS = frozenset(
    {
        "data/cells.json.gz",
        "data/runs.json.gz",
        "data/tests.json.gz",
    }
)
BROWSER_DATA_PATHS = frozenset(
    {
        "data/site-manifest.json.gz",
        "data/site-summary.json.gz",
        *(f"data/detail-{value}.json.gz" for value in "0123456789abcdef"),
    }
)
FIXED_RENDERER_ARTIFACT_PATHS = frozenset(
    {
        "assets/favicon.svg",
        "assets/site.css",
        "assets/site.js",
        "cells/detail.html",
        "cells/index.html",
        "cells/never-measured.html",
        "cells/projection-audit.html",
        "index.html",
        "runs/index.html",
        "tests/custom-commands.html",
        "tests/detail.html",
        *BROWSER_DATA_PATHS,
    }
)
FIXED_SITE_ARTIFACT_PATHS = STAGE_1_DATA_PATHS | FIXED_RENDERER_ARTIFACT_PATHS
GENERATOR_PATHS = (
    COORDINATOR_PATH,
    "ci-hub/compatibility-website/build_data.py",
    "ci-hub/compatibility-website/export_inputs.py",
    "ci-hub/compatibility-website/reduce.py",
    "ci-hub/compatibility-website/render_page.py",
    "ci-hub/compatibility-website/nightly_publication.py",
    "ci-hub/compatibility-website/write_data.py",
    "ci-hub/compatibility-website/assets/favicon.svg",
    "ci-hub/compatibility-website/assets/site.css",
    "ci-hub/compatibility-website/assets/site.js",
    "ci-hub/ledger/check-ledger-events.rs",
    "ci-hub/ledger/ledger.py",
    "ci-hub/ledger/publisher.py",
    "ci-hub/ledger/validate_rows.py",
    "ci-hub/lib/bounded_capture.py",
    "ci-hub/lib/rust_toolchain.py",
    "ci-hub/lib/ledger_event.rs",
    "ci-hub/qualifying_receipt.py",
    "ci-hub/series/check-series-rows.rs",
    "ci-hub/series/finalized_proof.py",
    "ci-hub/series/series.py",
    "ci-hub/validate/qualifying-receipt.json",
)
STAGE_1_READER_PATHS = (
    "ci-hub/admission_paths.py",
    "ci-hub/ci-hub",
    "ci-hub/ci-hub.rs",
    "ci-hub/compatibility-website/export_inputs.py",
    "ci-hub/ledger/check-ledger-parses.rs",
    "ci-hub/ledger/check-ledger-events.rs",
    "ci-hub/ledger/ledger.py",
    "ci-hub/ledger/publisher.py",
    "ci-hub/ledger/validate_rows.py",
    "ci-hub/lib/bounded_capture.py",
    "ci-hub/lib/bounded_output.rs",
    "ci-hub/lib/compatibility_census.rs",
    "ci-hub/lib/compatibility_website.rs",
    "ci-hub/lib/failure_obligations.rs",
    "ci-hub/lib/history_queries.rs",
    "ci-hub/lib/landing_lock.rs",
    "ci-hub/lib/lease.rs",
    "ci-hub/lib/ledger_event.rs",
    "ci-hub/lib/measured.rs",
    "ci-hub/lib/published_receipt.rs",
    "ci-hub/lib/qualifying_receipt.rs",
    "ci-hub/lib/records.rs",
    "ci-hub/lib/rust_toolchain.py",
    "ci-hub/lib/validate_lock.rs",
    "ci-hub/lib/validate_run_handle.rs",
    "ci-hub/lib/validate_status.rs",
    "ci-hub/lib/validation_evidence.rs",
    "ci-hub/qualifying_receipt.py",
    "ci-hub/series/check-series-rows.rs",
    "ci-hub/series/finalized_proof.py",
    "ci-hub/series/series.py",
    "ci-hub/validate/final_validate_status.py",
    "ci-hub/validate/flake_class.py",
    "ci-hub/validate/flaky_cells.py",
    "ci-hub/validate/gate_completeness.py",
    "ci-hub/validate/green_class.py",
    "ci-hub/validate/qualifying-receipt.json",
    "ci-hub/validate/run_registry.py",
    "ci-hub/validate/service_result.py",
)
CAPTURE_PATHS = tuple(dict.fromkeys((*GENERATOR_PATHS, *STAGE_1_READER_PATHS)))
QUALIFICATION_OVERRIDE_ENVIRONMENT = (
    "PRODUCER_DEFINITION_REPO",
    "PRODUCER_DEFINITION_VERIFIER",
    "QUALIFYING_RECEIPT_PREDICATE",
)


class WebsiteRefused(Exception):
    """The requested website operation cannot preserve its exact contract."""


@dataclass(frozen=True)
class Modules:
    build_data: Any
    series: Any
    export_inputs: Any
    writer: Any
    renderer: Any
    generator_files: tuple[dict[str, Any], ...]


@dataclass(frozen=True)
class _CapturedFile:
    path: Path
    encoded: bytes

    def read_bytes(self) -> bytes:
        return self.encoded

    def read_text(self, encoding: str | None = None, errors: str | None = None) -> str:
        return self.encoded.decode(encoding or "utf-8", errors or "strict")

    def __fspath__(self) -> str:
        return os.fspath(self.path)

    def __str__(self) -> str:
        return str(self.path)


@dataclass
class _TreeValidation:
    """One invocation's successful semantic check, never its filesystem checks."""

    modules: Modules
    manifest_sha256: str | None = None

    def reproduce(self, manifest: Mapping[str, Any], files: Mapping[str, bytes]) -> None:
        if _modules() is not self.modules:
            raise WebsiteRefused("website generator capture changed during validation")
        identity = _digest(manifest)
        if identity != self.manifest_sha256:
            _reproduce_expected_bytes(manifest, files)
            if _modules() is not self.modules:
                raise WebsiteRefused("website generator capture changed during validation")
            self.manifest_sha256 = identity


@dataclass
class ValidatedWebsite:
    path: Path
    descriptor: int
    manifest: dict[str, Any]
    reused: bool
    output_root: OutputRoot | None = None
    verify_sources: Callable[[], None] | None = None
    semantic_validation: _TreeValidation | None = None

    def recheck(self) -> None:
        with _modules().build_data.phase("recheck"):
            self._recheck()

    def _recheck(self) -> None:
        if self.output_root is not None:
            _return_target(
                self.output_root,
                self.path.name,
                self,
                self.verify_sources,
            )
            return
        _read_open_tree(
            self.descriptor, self.path, expected=self.manifest,
            semantic_validation=self.semantic_validation,
        )
        if self.verify_sources is not None:
            self.verify_sources()
        _read_open_tree(
            self.descriptor, self.path, expected=self.manifest,
            semantic_validation=self.semantic_validation,
        )

    def release_output_root(self) -> None:
        if self.output_root is not None:
            self.output_root.close()
            self.output_root = None

    def close(self) -> None:
        descriptor = self.descriptor
        self.descriptor = -1
        try:
            if descriptor >= 0:
                os.close(descriptor)
        finally:
            self.release_output_root()


@dataclass
class OutputRoot:
    path: Path
    parent: int
    descriptor: int

    def close(self) -> None:
        descriptor = self.descriptor
        parent = self.parent
        self.descriptor = -1
        self.parent = -1
        try:
            if descriptor >= 0:
                # Closing the final descriptor releases flock atomically; an
                # explicit LOCK_UN creates a failure window before either fd
                # is closed and provides no stronger ownership guarantee.
                os.close(descriptor)
        finally:
            if parent >= 0:
                os.close(parent)


class WebsiteSignal(BaseException):
    """A process signal interrupted a build that may need exact preservation."""

    def __init__(self, signum: int):
        super().__init__(signum)
        self.signum = signum


@contextmanager
def _defer_termination() -> Any:
    handled = {signal.SIGHUP, signal.SIGINT, signal.SIGTERM}
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, handled)
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)


def _load_module(name: str, path: Path, encoded: bytes) -> Any:
    spec = importlib.util.spec_from_loader(name, loader=None, origin=str(path))
    if spec is None or spec.loader is None:
        # A loader-less spec is intentional: the captured bytes below, not a
        # second pathname read, are the executable source authority.
        spec = importlib.util.spec_from_loader(name, loader=None)
    if spec is None:
        raise WebsiteRefused(f"cannot prepare captured generator {path}")
    module = importlib.util.module_from_spec(spec)
    module.__file__ = str(path)
    sys.modules[spec.name] = module
    try:
        code = compile(encoded, str(path), "exec", dont_inherit=True)
        exec(code, module.__dict__)
    except Exception as error:
        sys.modules.pop(spec.name, None)
        raise WebsiteRefused(
            f"cannot load captured generator {path}: {error}"
        ) from error
    return module


def _captured_reader_modules(
    captured: Mapping[str, bytes],
) -> dict[str, Any]:
    aliases = (
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
    missing = object()
    previous = {name: sys.modules.get(name, missing) for name in aliases}
    previous_path = list(sys.path)
    unique_previous: dict[str, Any] = {}

    def load(name: str, relative: str) -> Any:
        unique = f"compatibility_website_combined_{name}"
        unique_previous.setdefault(unique, sys.modules.get(unique, missing))
        module = _load_module(
            unique,
            REPOSITORY / relative,
            captured[relative],
        )
        sys.modules[name] = module
        return module

    try:
        bounded = load("bounded_capture", "ci-hub/lib/bounded_capture.py")
        rust_toolchain = load("rust_toolchain", "ci-hub/lib/rust_toolchain.py")
        ledger = load("ledger", "ci-hub/ledger/ledger.py")
        series = load("series", "ci-hub/series/series.py")
        finalized_proof = load("finalized_proof", "ci-hub/series/finalized_proof.py")
        flaky_cells = load("flaky_cells", "ci-hub/validate/flaky_cells.py")
        flake_class = load("flake_class", "ci-hub/validate/flake_class.py")
        gate_completeness = load(
            "gate_completeness", "ci-hub/validate/gate_completeness.py"
        )
        green_class = load("green_class", "ci-hub/validate/green_class.py")
        qualifying = load("qualifying_receipt", "ci-hub/qualifying_receipt.py")
        qualifying.predicate_path = lambda: _CapturedFile(
            REPOSITORY / "ci-hub/validate/qualifying-receipt.json",
            captured["ci-hub/validate/qualifying-receipt.json"],
        )
        qualifying._ACTIVE = None
        _refuse_qualification_overrides()
        captured_policy = json.loads(_json_bytes(qualifying.active()))
        qualifying.active = lambda: captured_policy
        sys.modules["qualifying_receipt"] = qualifying
        admission_paths = load("admission_paths", "ci-hub/admission_paths.py")
        publisher = load("publisher", "ci-hub/ledger/publisher.py")
        publisher._series_store = lambda: series
        validate_rows = load("validate_rows", "ci-hub/ledger/validate_rows.py")
        readers = {
            "bounded_capture": bounded,
            "rust_toolchain": rust_toolchain,
            "flake_class": flake_class,
            "flaky_cells": flaky_cells,
            "gate_completeness": gate_completeness,
            "green_class": green_class,
            "ledger": ledger,
            "publisher": publisher,
            "qualifying_receipt": qualifying,
            "admission_paths": admission_paths,
            "series": series,
            "finalized_proof": finalized_proof,
            "validate_rows": validate_rows,
        }
    finally:
        sys.path[:] = previous_path
        for name, module in previous.items():
            if module is missing:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = module
        for name, module in unique_previous.items():
            if module is missing:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = module
    return readers


def _refuse_qualification_overrides() -> None:
    present = [
        name for name in QUALIFICATION_OVERRIDE_ENVIRONMENT if name in os.environ
    ]
    if present:
        raise WebsiteRefused(
            "website reader refuses qualification authority override(s): "
            + ", ".join(present)
        )


def _bind_export_authorities(
    export_inputs: Any,
    captured: Mapping[str, bytes],
    readers: Mapping[str, Any],
) -> None:
    declared_paths = tuple(
        relative.as_posix() for relative in export_inputs._READER_PATHS
    )
    if declared_paths != STAGE_1_READER_PATHS:
        raise WebsiteRefused("Stage 1 reader file inventory is not exact")
    expected_files = [
        {
            "bytes": len(captured[relative.as_posix()]),
            "path": relative.as_posix(),
            "sha256": _sha256(captured[relative.as_posix()]),
        }
        for relative in export_inputs._READER_PATHS
    ]

    def bind(state: dict[str, Any]) -> dict[str, Any]:
        if state["files"] != expected_files:
            raise export_inputs.ExportRefused(
                "canonical reader files differ from captured generator bytes"
            )
        export_inputs.ledger_store = readers["ledger"]
        export_inputs.ledger_publisher = readers["publisher"]
        export_inputs.series_store = readers["series"]
        export_inputs.validation_ledger = readers["validate_rows"]
        export_inputs.finalized_proof = readers["finalized_proof"]
        return state

    def load_authorities() -> dict[str, Any]:
        return bind(export_inputs._reader_state())

    def load_historical_authorities() -> dict[str, Any]:
        return bind(export_inputs._historical_reader_state())

    export_inputs._load_authorities = load_authorities
    export_inputs._load_historical_authorities = load_historical_authorities


@functools.cache
def _modules() -> Modules:
    captured = _capture_generator_bytes()
    _require_executing_source(captured[COORDINATOR_PATH])

    def load(name: str, relative: str) -> Any:
        return _load_module(name, REPOSITORY / relative, captured[relative])

    readers = _captured_reader_modules(captured)
    export_inputs = load(
        "compatibility_website_combined_export_inputs",
        "ci-hub/compatibility-website/export_inputs.py",
    )
    _bind_export_authorities(export_inputs, captured, readers)
    reducer = load(
        "compatibility_website_combined_reducer",
        "ci-hub/compatibility-website/reduce.py",
    )
    build_data = load(
        "compatibility_website_combined_build_data",
        "ci-hub/compatibility-website/build_data.py",
    )
    build_data._LOADED_MODULES = build_data.Modules(
        export_inputs=export_inputs,
        reducer=reducer,
        rust_toolchain=readers["rust_toolchain"],
    )
    writer = load(
        "compatibility_website_combined_writer",
        "ci-hub/compatibility-website/write_data.py",
    )
    writer._build_data_module = lambda: build_data
    renderer = load(
        "compatibility_website_combined_renderer",
        "ci-hub/compatibility-website/render_page.py",
    )
    renderer._writer = lambda: writer
    renderer.SITE_CSS_PATH = _CapturedFile(
        renderer.SITE_CSS_PATH,
        captured["ci-hub/compatibility-website/assets/site.css"],
    )
    renderer.SITE_JS_PATH = _CapturedFile(
        renderer.SITE_JS_PATH,
        captured["ci-hub/compatibility-website/assets/site.js"],
    )
    renderer.FAVICON_PATH = _CapturedFile(
        renderer.FAVICON_PATH,
        captured["ci-hub/compatibility-website/assets/favicon.svg"],
    )
    if _capture_generator_bytes() != captured:
        raise WebsiteRefused("website generator files changed while being captured")
    return Modules(
        build_data=build_data,
        series=readers["series"],
        export_inputs=export_inputs,
        writer=writer,
        renderer=renderer,
        generator_files=tuple(_generator_rows(captured)),
    )


def _json_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value, sort_keys=True, separators=(",", ":"), allow_nan=False
        ).encode()
    except (TypeError, ValueError, RecursionError) as error:
        raise WebsiteRefused(f"website value is not canonical JSON: {error}") from error


def _digest(value: Any) -> str:
    return hashlib.sha256(_json_bytes(value)).hexdigest()


def _sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def _object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping) or not all(isinstance(key, str) for key in value):
        raise WebsiteRefused(f"{label} must be a string-keyed object")
    return value


def _array(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise WebsiteRefused(f"{label} must be an array")
    return value


def _string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise WebsiteRefused(f"{label} must be a nonempty string")
    return value


def _integer(value: Any, label: str, *, maximum: int | None = None) -> int:
    if type(value) is not int or value < 0 or (maximum is not None and value > maximum):
        raise WebsiteRefused(f"{label} must be a bounded nonnegative integer")
    return value


def _exact_fields(
    value: Mapping[str, Any], expected: frozenset[str], label: str
) -> None:
    if set(value) != expected:
        raise WebsiteRefused(f"{label} fields are not exact")


def _regular_bytes(path: Path, *, limit: int = 8 << 20) -> bytes:
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise WebsiteRefused(f"cannot open generator file {path}: {error}") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise WebsiteRefused(
                f"generator file is not a bounded regular file: {path}"
            )
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            value = source.read(limit + 1)
        after = os.fstat(descriptor)
        current = path.stat(follow_symlinks=False)
        before_identity = (
            before.st_dev,
            before.st_ino,
            before.st_mode,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        )
        after_identity = (
            after.st_dev,
            after.st_ino,
            after.st_mode,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        )
        current_identity = (
            current.st_dev,
            current.st_ino,
            current.st_mode,
            current.st_size,
            current.st_mtime_ns,
            current.st_ctime_ns,
        )
        if (
            len(value) != before.st_size
            or before_identity != after_identity
            or after_identity != current_identity
        ):
            raise WebsiteRefused(f"generator file changed while being read: {path}")
        return value
    finally:
        os.close(descriptor)


def _capture_generator_bytes() -> dict[str, bytes]:
    return {
        relative: _regular_bytes(REPOSITORY / relative) for relative in CAPTURE_PATHS
    }


def _generator_rows(captured: Mapping[str, bytes]) -> list[dict[str, Any]]:
    return [
        {"bytes": len(captured[path]), "path": path, "sha256": _sha256(captured[path])}
        for path in GENERATOR_PATHS
    ]


def _require_executing_source(encoded: bytes) -> None:
    expected = (
        _BOOTSTRAP_SOURCE_BYTES
        if _BOOTSTRAP_SOURCE_BYTES is not None
        else _IMPORT_SOURCE_BYTES
    )
    if encoded != expected:
        raise WebsiteRefused(
            "executing website coordinator bytes changed before capture"
        )
    try:
        observed = compile(
            encoded,
            _EXECUTING_MODULE_CODE.co_filename,
            "exec",
            dont_inherit=True,
            optimize=sys.flags.optimize,
        )
    except (SyntaxError, ValueError) as error:
        raise WebsiteRefused(
            f"website coordinator source cannot be compiled: {error}"
        ) from error
    if observed != _EXECUTING_MODULE_CODE:
        raise WebsiteRefused(
            "website coordinator path does not contain the executing program"
        )


def _generator_files() -> list[dict[str, Any]]:
    return _generator_rows(_capture_generator_bytes())


def _media(path: str) -> tuple[str, str | None]:
    parsed = PurePosixPath(path)
    if parsed.is_absolute() or ".." in parsed.parts or len(parsed.parts) not in (1, 2):
        raise WebsiteRefused(f"website artifact has an unsafe path {path!r}")
    parent = parsed.parts[0] if len(parsed.parts) == 2 else None
    name = parsed.name
    if parent == "assets" and name == "favicon.svg":
        return "image/svg+xml", None
    if name.endswith(".html"):
        return "text/html; charset=utf-8", None
    if name.endswith(".css"):
        return "text/css; charset=utf-8", None
    if name.endswith(".js"):
        return "text/javascript; charset=utf-8", None
    if parent == "data" and name.endswith(".json.gz"):
        return "application/json", "gzip"
    raise WebsiteRefused(f"website artifact has no approved media type: {path}")


def _artifact_rows(files: Mapping[str, bytes]) -> list[dict[str, Any]]:
    rows = []
    for path, value in sorted(files.items()):
        content_type, content_encoding = _media(path)
        rows.append(
            {
                "bytes": len(value),
                "content_encoding": content_encoding,
                "content_type": content_type,
                "path": path,
                "sha256": _sha256(value),
            }
        )
    return rows


def _directory_rows(artifacts: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[str, list[Mapping[str, Any]]] = {".": []}
    grouped.update({directory: [] for directory in FIXED_DIRECTORIES})
    for row in artifacts:
        parts = PurePosixPath(str(row["path"])).parts
        grouped["." if len(parts) == 1 else parts[0]].append(row)
    return [
        {
            "artifacts_sha256": _digest(rows),
            "file_count": len(rows),
            "path": directory,
        }
        for directory, rows in sorted(grouped.items())
    ]


def _stage_1_files(data: Mapping[str, Any]) -> dict[str, bytes]:
    modules = _modules()
    try:
        files = modules.writer.render_tree(data)
    except modules.writer.WriteRefused as error:
        raise WebsiteRefused(f"cannot render the Stage 1 tree: {error}") from error
    if set(modules.writer._DATA_PATHS) != STAGE_1_DATA_PATHS or set(files) != {
        "build.json",
        *STAGE_1_DATA_PATHS,
    }:
        raise WebsiteRefused(
            "Stage 1 renderer did not produce its exact four-file tree"
        )
    return dict(sorted(files.items()))


def _require_wording_matches_stage_1(
    wording: str, stage_1_build: Mapping[str, Any]
) -> None:
    provenance = _object(stage_1_build.get("provenance"), "Stage 1 build provenance")
    if (wording == "historical") != ("historical" in provenance):
        raise WebsiteRefused(
            "historical wording must be used exactly for historical Stage 1 provenance"
        )


def _manifest(
    stage_1_build: Mapping[str, Any],
    stage_1_build_bytes: bytes,
    files: Mapping[str, bytes],
    generator_files: list[dict[str, Any]],
    *,
    wording: str,
    presentation: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    _require_exact_artifact_inventory(files, stage_1_build["counts"])
    artifacts = _artifact_rows(files)
    directories = _directory_rows(artifacts)
    artifacts_sha256 = _digest(artifacts)
    directories_sha256 = _digest(directories)
    tree_sha256 = _digest(
        {
            "artifacts_sha256": artifacts_sha256,
            "directories_sha256": directories_sha256,
        }
    )
    generator = {
        "files": generator_files,
        "files_sha256": _digest(generator_files),
        "name": "compatibility-website",
        "schema_version": SCHEMA_VERSION,
        "wording": wording,
    }
    if presentation is not None:
        generator["presentation"] = presentation
    inputs = {
        "stage_1_build_json": stage_1_build,
        "stage_1_build_json_bytes": len(stage_1_build_bytes),
        "stage_1_build_json_sha256": _sha256(stage_1_build_bytes),
    }
    freshness = {
        "generator_files_sha256": generator["files_sha256"],
        "stage_1_build_json_sha256": inputs["stage_1_build_json_sha256"],
        "tree_sha256": tree_sha256,
        "wording": generator["wording"],
    }
    if presentation is not None:
        freshness["presentation_sha256"] = _digest(presentation)
    return {
        "artifacts": artifacts,
        "artifacts_sha256": artifacts_sha256,
        "counts": stage_1_build["counts"],
        "directories": directories,
        "directories_sha256": directories_sha256,
        "freshness_sha256": _digest(freshness),
        "generator": generator,
        "inputs": inputs,
        "provenance": stage_1_build["provenance"],
        "schema_version": SCHEMA_VERSION,
        "tree_sha256": tree_sha256,
    }


def compose(
    data: Mapping[str, Any], *, wording: str = "live",
    presentation: Mapping[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    with _modules().build_data.phase("compose"):
        return _compose(data, wording=wording, presentation=presentation)


def _compose(
    data: Mapping[str, Any], *, wording: str,
    presentation: Mapping[str, Any] | None,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    before = _generator_files()
    modules = _modules()
    bound = list(modules.generator_files)
    if before != bound:
        raise WebsiteRefused(
            "website generator files differ from the captured executable bytes"
        )
    try:
        stage_1_files = _stage_1_files(data)
        stage_1_build_bytes = stage_1_files["build.json"]
        stage_1_build = _object(json.loads(stage_1_build_bytes), "Stage 1 build.json")
        _require_wording_matches_stage_1(wording, stage_1_build)
        presentation = modules.renderer.validate_presentation(presentation, data)
        pages = modules.renderer.render_site(stage_1_build, data, wording=wording, presentation=presentation)
        modules.renderer._validate_output_files(pages)
    except (
        json.JSONDecodeError,
        modules.renderer.RenderRefused,
    ) as error:
        raise WebsiteRefused(
            f"cannot compose validated website trees: {error}"
        ) from error
    files = {
        path: value for path, value in stage_1_files.items() if path != "build.json"
    }
    overlap = set(files) & set(pages)
    if overlap:
        raise WebsiteRefused(
            "Stage 1 data and rendered pages overlap: " + ", ".join(sorted(overlap))
        )
    files.update(pages)
    after = _generator_files()
    if before != after or after != bound:
        raise WebsiteRefused("website generator files changed during composition")
    manifest = _manifest(
        stage_1_build, stage_1_build_bytes, files, bound, wording=wording,
        presentation=presentation,
    )
    complete = {"build.json": _json_bytes(manifest) + b"\n", **files}
    return manifest, dict(sorted(complete.items()))


def _validated_generator(value: Any) -> dict[str, Any]:
    generator = dict(_object(value, "website generator"))
    fields = {"files", "files_sha256", "name", "schema_version", "wording"}
    if "presentation" in generator:
        fields.add("presentation")
        try:
            if generator["presentation"] is None:
                raise _modules().renderer.RenderRefused("presentation provenance is null")
            _modules().renderer.validate_presentation(generator["presentation"])
        except _modules().renderer.RenderRefused as error:
            raise WebsiteRefused(str(error)) from error
    _exact_fields(
        generator,
        frozenset(fields),
        "website generator",
    )
    if (
        generator["name"] != "compatibility-website"
        or generator["schema_version"] != SCHEMA_VERSION
    ):
        raise WebsiteRefused("website generator identity is unsupported")
    wording = generator["wording"]
    if wording not in ("fixture", "historical", "live"):
        raise WebsiteRefused("website generator wording is unsupported")
    generator_files = _array(generator["files"], "website generator files")
    if [row.get("path") for row in generator_files if isinstance(row, Mapping)] != list(
        GENERATOR_PATHS
    ):
        raise WebsiteRefused("website generator file inventory is not exact")
    for index, raw in enumerate(generator_files):
        row = _object(raw, f"website generator files[{index}]")
        _exact_fields(
            row, frozenset({"bytes", "path", "sha256"}), "website generator file"
        )
        _integer(row["bytes"], "website generator file bytes", maximum=8 << 20)
        if not _is_sha256(row["sha256"]):
            raise WebsiteRefused("website generator file digest is invalid")
    if generator["files_sha256"] != _digest(generator_files):
        raise WebsiteRefused("website generator file digest disagrees with its rows")
    return generator


def _validated_inputs(value: Any) -> tuple[dict[str, Any], Mapping[str, Any]]:
    inputs = dict(_object(value, "website inputs"))
    _exact_fields(
        inputs,
        frozenset(
            {
                "stage_1_build_json",
                "stage_1_build_json_bytes",
                "stage_1_build_json_sha256",
            }
        ),
        "website inputs",
    )
    stage_1_bytes = _json_bytes(inputs["stage_1_build_json"]) + b"\n"
    if inputs["stage_1_build_json_bytes"] != len(stage_1_bytes) or inputs[
        "stage_1_build_json_sha256"
    ] != _sha256(stage_1_bytes):
        raise WebsiteRefused(
            "Stage 1 build.json identity disagrees with its exact bytes"
        )
    stage_1 = _object(inputs["stage_1_build_json"], "embedded Stage 1 build.json")
    if stage_1.get("schema_version") != STAGE_1_SCHEMA_VERSION:
        raise WebsiteRefused("embedded Stage 1 build.json schema is unsupported")
    return inputs, stage_1


def _validated_artifacts(value: Any) -> list[dict[str, Any]]:
    artifacts = _array(value, "website artifacts")
    normalized, paths = [], []
    for index, raw in enumerate(artifacts):
        row = dict(_object(raw, f"website artifacts[{index}]"))
        _exact_fields(
            row,
            frozenset({"bytes", "content_encoding", "content_type", "path", "sha256"}),
            "website artifact",
        )
        path = _string(row["path"], "website artifact path")
        expected_type, expected_encoding = _media(path)
        if (
            row["content_type"] != expected_type
            or row["content_encoding"] != expected_encoding
        ):
            raise WebsiteRefused(f"website artifact media metadata is invalid: {path}")
        _integer(row["bytes"], "website artifact bytes", maximum=MAX_ARTIFACT_BYTES)
        if not _is_sha256(row["sha256"]):
            raise WebsiteRefused("website artifact digest is invalid")
        paths.append(path)
        normalized.append(row)
    if paths != sorted(set(paths)) or "build.json" in paths:
        raise WebsiteRefused(
            "website artifact paths are not sorted, unique, and self-excluding"
        )
    return normalized


def _validated_tree_digest(
    manifest: Mapping[str, Any], artifacts: Sequence[Mapping[str, Any]]
) -> str:
    if manifest["artifacts_sha256"] != _digest(artifacts):
        raise WebsiteRefused("website artifact-list digest disagrees with its rows")
    directories = _array(manifest["directories"], "website directories")
    if directories != _directory_rows(artifacts):
        raise WebsiteRefused("website directory inventory disagrees with artifact rows")
    if manifest["directories_sha256"] != _digest(directories):
        raise WebsiteRefused("website directory digest disagrees with its rows")
    tree_sha256 = _digest(
        {
            "artifacts_sha256": manifest["artifacts_sha256"],
            "directories_sha256": manifest["directories_sha256"],
        }
    )
    if manifest["tree_sha256"] != tree_sha256:
        raise WebsiteRefused("website tree digest disagrees with its inventory")
    return tree_sha256


def _is_run_artifact_path(path: str) -> bool:
    parsed = PurePosixPath(path)
    return (
        len(parsed.parts) == 2
        and parsed.parts[0] == "runs"
        and parsed.name.endswith(".html")
        and _is_sha256(parsed.name.removesuffix(".html"))
    )


def _cell_continuation_artifact_paths(
    counts: Mapping[str, Any], *, actual_artifact_count: int
) -> set[str]:
    page_size = _integer(
        _modules().renderer.LIST_PAGE_SIZE,
        "renderer cell-list page size",
    )
    if page_size < 1:
        raise WebsiteRefused("renderer cell-list page size must be positive")

    populations = (
        (
            "results",
            _integer(counts.get("green_cell_count"), "website counts.green_cell_count")
            + _integer(counts.get("red_cell_count"), "website counts.red_cell_count"),
        ),
        (
            "never-measured",
            _integer(
                counts.get("never_measured_cell_count"),
                "website counts.never_measured_cell_count",
            ),
        ),
    )
    continuation_count = sum(
        max(0, (population + page_size - 1) // page_size - 1)
        for _, population in populations
    )
    if continuation_count > actual_artifact_count:
        raise WebsiteRefused("website artifact inventory is not exact")
    return {
        f"cells/{name}-{page}.html"
        for name, population in populations
        for page in range(2, (population + page_size - 1) // page_size + 1)
    }


def _require_exact_artifact_inventory(
    paths: Collection[str], counts_value: Any
) -> None:
    counts = _object(counts_value, "website counts")
    artifact_paths = set(paths)
    continuation_paths = _cell_continuation_artifact_paths(
        counts, actual_artifact_count=len(artifact_paths)
    )
    expected_fixed_paths = FIXED_SITE_ARTIFACT_PATHS | continuation_paths
    run_paths = artifact_paths - expected_fixed_paths
    expected_runs = _integer(
        counts.get("validation_history_row_count"),
        "website counts.validation_history_row_count",
    )
    if (
        len(artifact_paths) != len(paths)
        or not expected_fixed_paths <= artifact_paths
        or len(run_paths) != expected_runs
        or any(not _is_run_artifact_path(path) for path in run_paths)
    ):
        raise WebsiteRefused("website artifact inventory is not exact")


def _validate_manifest(value: Any) -> dict[str, Any]:
    manifest = dict(_object(value, "website build.json"))
    _exact_fields(
        manifest,
        frozenset(
            {
                "artifacts",
                "artifacts_sha256",
                "counts",
                "directories",
                "directories_sha256",
                "freshness_sha256",
                "generator",
                "inputs",
                "provenance",
                "schema_version",
                "tree_sha256",
            }
        ),
        "website build.json",
    )
    if manifest["schema_version"] != SCHEMA_VERSION:
        raise WebsiteRefused("website build.json schema is unsupported")
    generator = _validated_generator(manifest["generator"])
    inputs, stage_1 = _validated_inputs(manifest["inputs"])
    _require_wording_matches_stage_1(str(generator["wording"]), stage_1)
    if manifest["counts"] != stage_1.get("counts") or manifest[
        "provenance"
    ] != stage_1.get("provenance"):
        raise WebsiteRefused("website build.json disagrees with embedded Stage 1 facts")
    artifacts = _validated_artifacts(manifest["artifacts"])
    _require_exact_artifact_inventory(
        [str(row["path"]) for row in artifacts], manifest["counts"]
    )
    tree_sha256 = _validated_tree_digest(manifest, artifacts)
    freshness = {
        "generator_files_sha256": generator["files_sha256"],
        "stage_1_build_json_sha256": inputs["stage_1_build_json_sha256"],
        "tree_sha256": tree_sha256,
        "wording": generator["wording"],
    }
    if "presentation" in generator:
        freshness["presentation_sha256"] = _digest(generator["presentation"])
    if manifest["freshness_sha256"] != _digest(freshness):
        raise WebsiteRefused("website freshness digest disagrees with its inputs")
    return json.loads(_json_bytes(manifest))


def _inventories(artifacts: Sequence[Mapping[str, Any]]) -> dict[str, frozenset[str]]:
    values: dict[str, set[str]] = {"": {"build.json", *FIXED_DIRECTORIES}}
    values.update({directory: set() for directory in FIXED_DIRECTORIES})
    for row in artifacts:
        parts = PurePosixPath(str(row["path"])).parts
        if len(parts) == 1:
            values[""].add(parts[0])
        else:
            values[parts[0]].add(parts[1])
    return {directory: frozenset(names) for directory, names in values.items()}


def _require_directory_mode(descriptor: int, label: str) -> None:
    metadata = os.fstat(descriptor)
    if metadata.st_uid != os.geteuid():
        raise WebsiteRefused(f"{label} is not owned by the current user")
    if stat.S_IMODE(metadata.st_mode) != 0o555:
        raise WebsiteRefused(f"{label} mode is not read-only")


def _read_artifact(directory: int, name: str, limit: int, label: str) -> bytes:
    writer = _modules().writer
    before = writer._entry(directory, name)
    if (
        before is None
        or not stat.S_ISREG(before.st_mode)
        or stat.S_IMODE(before.st_mode) != 0o444
        or before.st_uid != os.geteuid()
    ):
        raise WebsiteRefused(
            f"{label} is not a read-only regular file owned by the current user"
        )
    value = writer._read_file(directory, name, limit)
    after = writer._entry(directory, name)
    if (
        after is None
        or not os.path.samestat(before, after)
        or stat.S_IMODE(after.st_mode) != 0o444
        or after.st_uid != os.geteuid()
    ):
        raise WebsiteRefused(f"{label} changed while being read")
    return value


def _read_open_tree_untranslated(
    root: int,
    path: Path,
    *,
    expected: Mapping[str, Any] | None = None,
    require_name: bool = True,
    semantic_validation: _TreeValidation | None = None,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    writer = _modules().writer
    _require_directory_mode(root, "website root")
    raw_build = _read_artifact(
        root, "build.json", MAX_BUILD_JSON_BYTES, "website build.json"
    )
    try:
        parsed = json.loads(raw_build)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise WebsiteRefused(f"website build.json is invalid JSON: {error}") from error
    manifest = _validate_manifest(parsed)
    if raw_build != _json_bytes(manifest) + b"\n":
        raise WebsiteRefused(
            "website build.json is not canonical JSON plus one newline"
        )
    if require_name and path.name != manifest["freshness_sha256"]:
        raise WebsiteRefused("website directory name disagrees with freshness_sha256")
    if expected is not None and manifest != expected:
        raise WebsiteRefused("existing website target contradicts this exact build")
    inventories = _inventories(manifest["artifacts"])
    directories: dict[str, int] = {}
    try:
        for directory in FIXED_DIRECTORIES:
            directories[directory] = writer._open_directory(root, directory)
            _require_directory_mode(
                directories[directory], f"website {directory} directory"
            )
        writer._require_inventory(root, inventories[""], "website root")
        files: dict[str, bytes] = {}
        for row in manifest["artifacts"]:
            relative = str(row["path"])
            parts = PurePosixPath(relative).parts
            directory = root if len(parts) == 1 else directories[parts[0]]
            value = _read_artifact(
                directory,
                parts[-1],
                int(row["bytes"]),
                f"website artifact {relative}",
            )
            if len(value) != row["bytes"] or _sha256(value) != row["sha256"]:
                raise WebsiteRefused(
                    f"website artifact bytes disagree with build.json: {relative}"
                )
            files[relative] = value
        for directory, descriptor in directories.items():
            writer._require_inventory(
                descriptor, inventories[directory], f"website {directory}"
            )
            _require_directory_mode(descriptor, f"website {directory} directory")
        _require_directory_mode(root, "website root")
    finally:
        for descriptor in directories.values():
            os.close(descriptor)
    # All bytes, canonical manifest, inventories, permissions and held-directory
    # checks above run every time. Their exact content identity permits reusing
    # only a completed reproduction within this invocation's captured modules.
    if semantic_validation is None:
        _reproduce_expected_bytes(manifest, files)
    else:
        semantic_validation.reproduce(manifest, files)
    return manifest, files


def _read_open_tree(
    root: int,
    path: Path,
    *,
    expected: Mapping[str, Any] | None = None,
    require_name: bool = True,
    semantic_validation: _TreeValidation | None = None,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    writer = _modules().writer
    try:
        return _read_open_tree_untranslated(
            root,
            path,
            expected=expected,
            require_name=require_name,
            semantic_validation=semantic_validation,
        )
    except WebsiteRefused:
        raise
    except (OSError, writer.WriteRefused) as error:
        raise WebsiteRefused(f"website tree read refused: {error}") from error


def _reproduce_expected_bytes(
    manifest: Mapping[str, Any], files: Mapping[str, bytes]
) -> None:
    modules = _modules()
    stage_1_build = _object(
        manifest["inputs"]["stage_1_build_json"], "embedded Stage 1 build"
    )
    stage_1_bytes = _json_bytes(stage_1_build) + b"\n"
    try:
        _build, generator, artifacts = modules.writer._prior_manifest(stage_1_bytes)
        payloads = {}
        for artifact in artifacts:
            path = str(artifact["path"])
            encoded = files.get(path)
            if encoded is None:
                raise WebsiteRefused(f"website is missing Stage 1 artifact {path}")
            if (
                len(encoded) != artifact["compressed_bytes"]
                or _sha256(encoded) != artifact["compressed_sha256"]
            ):
                raise WebsiteRefused(
                    f"website Stage 1 artifact disagrees with its source manifest: {path}"
                )
            raw = modules.writer._gunzip_bytes(
                encoded, int(artifact["uncompressed_bytes"])
            )
            if _sha256(raw) != artifact["uncompressed_sha256"]:
                raise WebsiteRefused(
                    f"website Stage 1 artifact content digest disagrees: {path}"
                )
            payloads[path] = json.loads(raw)
        source = modules.renderer._artifact_source(stage_1_build, generator, payloads)
        validated = modules.build_data.validate_build_data(source)
        expected_stage_1 = modules.writer._render_validated_tree(validated)
        if expected_stage_1["build.json"] != stage_1_bytes:
            raise WebsiteRefused(
                "embedded Stage 1 build.json disagrees with its validated data"
            )
        pages = modules.renderer.render_site(
            stage_1_build, validated, wording=str(manifest["generator"]["wording"]),
            presentation=manifest["generator"].get("presentation"),
        )
        modules.renderer._validate_output_files(pages)
    except (
        json.JSONDecodeError,
        modules.build_data.BuildRefused,
        modules.writer.WriteRefused,
        modules.renderer.RenderRefused,
    ) as error:
        raise WebsiteRefused(
            f"website cannot reproduce its validated sources: {error}"
        ) from error
    expected = {
        path: value for path, value in expected_stage_1.items() if path != "build.json"
    }
    expected.update(pages)
    if dict(sorted(files.items())) != dict(sorted(expected.items())):
        raise WebsiteRefused(
            "website artifacts disagree with a fresh exact reproduction"
        )


def _open_validated_at(
    parent: int, name: str, path: Path, *, expected: Mapping[str, Any] | None = None,
    semantic_validation: _TreeValidation | None = None,
) -> ValidatedWebsite:
    writer = _modules().writer
    descriptor = None
    try:
        descriptor = writer._open_directory(parent, name)
        validation = semantic_validation or _TreeValidation(_modules())
        manifest, _files = _read_open_tree(
            descriptor, path, expected=expected, semantic_validation=validation,
        )
        writer._require_entry(
            parent, name, descriptor, "website target changed during validation"
        )
        return ValidatedWebsite(
            path, descriptor, manifest, reused=expected is not None,
            semantic_validation=validation,
        )
    except BaseException as error:
        try:
            if descriptor is not None:
                os.close(descriptor)
        finally:
            if isinstance(error, WebsiteRefused):
                raise
            if isinstance(
                error,
                (
                    KeyError,
                    OSError,
                    RuntimeError,
                    TypeError,
                    ValueError,
                    writer.WriteRefused,
                ),
            ):
                raise WebsiteRefused(f"website validation refused: {error}") from error
            raise


def validate_website(path_value: Path) -> ValidatedWebsite:
    writer = _modules().writer
    try:
        path = writer._destination(path_value)
        parent = writer._open_parent(path.parent)
        try:
            _require_output_parent_permissions(os.fstat(parent))
            return _open_validated_at(parent, path.name, path)
        finally:
            os.close(parent)
    except (OSError, RuntimeError, writer.WriteRefused) as error:
        raise WebsiteRefused(f"website root refused: {error}") from error


def _overlaps(left: Path, right: Path) -> bool:
    return left == right or left in right.parents or right in left.parents


def _source_checkouts() -> tuple[Path, ...]:
    roots = {REPOSITORY.resolve(strict=True)}
    for ancestor in REPOSITORY.resolve(strict=True).parents:
        for marker in CHECKOUT_MARKERS:
            try:
                (ancestor / marker).lstat()
            except FileNotFoundError:
                continue
            roots.add(ancestor)
            break
    return tuple(sorted(roots))


def _require_output_parent_permissions(metadata: os.stat_result) -> None:
    if metadata.st_uid != os.geteuid():
        raise WebsiteRefused("website output parent is not owned by the current user")
    if metadata.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise WebsiteRefused(
            "website output parent permits other local users to replace entries"
        )


def _require_output_root_permissions(metadata: os.stat_result) -> None:
    if metadata.st_uid != os.geteuid():
        raise WebsiteRefused("website output root is not owned by the current user")
    if metadata.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise WebsiteRefused("website output root is writable by other local users")


def _output_location(value: Path) -> tuple[Path, os.stat_result]:
    writer = _modules().writer
    try:
        path = writer._destination(value)
        resolved = path.resolve(strict=False)
        for source in _source_checkouts():
            if _overlaps(resolved, source):
                raise WebsiteRefused("website output root overlaps a source directory")
        parent = path.parent.stat(follow_symlinks=False)
        _require_output_parent_permissions(parent)
        return path, parent
    except (OSError, RuntimeError, writer.WriteRefused) as error:
        raise WebsiteRefused(f"website output root refused: {error}") from error


def _lock_output_root(path_value: Path) -> OutputRoot:
    writer = _modules().writer
    path, expected_parent = _output_location(path_value)
    parent = writer._open_parent(path.parent)
    descriptor = None
    try:
        observed_parent = os.fstat(parent)
        if not os.path.samestat(expected_parent, observed_parent):
            raise WebsiteRefused("website output parent changed while being opened")
        _require_output_parent_permissions(observed_parent)
        if writer._entry(parent, path.name) is None:
            try:
                os.mkdir(path.name, 0o755, dir_fd=parent)
                writer._fsync_directory(parent)
            except FileExistsError:
                pass
        descriptor = writer._open_directory(parent, path.name)
        _require_output_root_permissions(os.fstat(descriptor))
        if CHECKOUT_MARKERS & writer._inventory(descriptor):
            raise WebsiteRefused("website output root is a checkout")
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        if CHECKOUT_MARKERS & writer._inventory(descriptor):
            raise WebsiteRefused("website output root became a checkout while waiting")
        writer._require_entry(
            parent, path.name, descriptor, "website output root changed"
        )
        return OutputRoot(path, parent, descriptor)
    except BaseException:
        if descriptor is not None:
            os.close(descriptor)
        os.close(parent)
        raise


def _require_output_root(output: OutputRoot) -> None:
    writer = _modules().writer
    try:
        writer._require_parent(
            output.parent,
            output.path.parent,
            "website output parent changed",
        )
        _require_output_parent_permissions(os.fstat(output.parent))
        writer._require_entry(
            output.parent,
            output.path.name,
            output.descriptor,
            "website output root changed",
        )
        _require_output_root_permissions(os.fstat(output.descriptor))
        if CHECKOUT_MARKERS & writer._inventory(output.descriptor):
            raise WebsiteRefused("website output root is a checkout")
    except (OSError, writer.WriteRefused) as error:
        raise WebsiteRefused(
            f"website output path identity changed: {error}"
        ) from error


def _return_target(
    output: OutputRoot,
    target: str,
    value: ValidatedWebsite,
    verify_sources: Callable[[], None] | None = None,
) -> ValidatedWebsite:
    writer = _modules().writer

    def require_target() -> None:
        _require_output_root(output)
        _read_open_tree(
            value.descriptor, value.path, expected=value.manifest,
            semantic_validation=value.semantic_validation,
        )
        try:
            writer._require_entry(
                output.descriptor,
                target,
                value.descriptor,
                "website target changed before return",
            )
        except (OSError, writer.WriteRefused) as error:
            raise WebsiteRefused(f"website target identity changed: {error}") from error
        _require_output_root(output)

    require_target()
    if verify_sources is not None:
        verify_sources()
    require_target()
    return value


def _write_owned(
    directory: int,
    name: str,
    value: bytes,
) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
    with _defer_termination():
        descriptor = os.open(name, flags, 0o644, dir_fd=directory)
        try:
            with os.fdopen(descriptor, "wb", closefd=False) as output:
                output.write(value)
                output.flush()
                os.fsync(output.fileno())
            os.fchmod(descriptor, 0o444)
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def _rename_noreplace(directory: int, left: str, right: str) -> None:
    renameat2 = getattr(ctypes.CDLL(None, use_errno=True), "renameat2", None)
    if renameat2 is None:
        raise WebsiteRefused("atomic directory rename is unavailable")
    renameat2.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    renameat2.restype = ctypes.c_int
    if renameat2(
        directory,
        os.fsencode(left),
        directory,
        os.fsencode(right),
        _modules().writer._RENAME_NOREPLACE,
    ):
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def _preserve_private_tree(
    output: OutputRoot,
    temporary: str,
    target: str,
    root: int,
) -> None:
    writer = _modules().writer
    retained = temporary
    try:
        with _defer_termination():
            held = os.fstat(root)
            temporary_entry = writer._entry(output.descriptor, temporary)
            target_entry = writer._entry(output.descriptor, target)
            at_temporary = temporary_entry is not None and os.path.samestat(
                temporary_entry, held
            )
            at_target = target_entry is not None and os.path.samestat(
                target_entry, held
            )
            if at_target:
                retained = target
            elif at_temporary:
                os.fchmod(root, 0o700)
                writer._fsync_directory(root)
            else:
                raise WebsiteRefused(
                    "cannot identify the private website tree after publication failure"
                )
            writer._fsync_directory(output.descriptor)
    except Exception as preservation_error:
        print(
            "compatibility-website: WARNING: retained private tree with uncertain "
            f"path: {preservation_error}",
            file=sys.stderr,
        )
        return
    kind = "published tree" if retained == target else "private tree"
    print(
        f"compatibility-website: WARNING: retained {kind} {retained!r}", file=sys.stderr
    )


def _materialize(
    output: OutputRoot,
    manifest: Mapping[str, Any],
    files: Mapping[str, bytes],
    *,
    verify_sources: Callable[[], None] | None = None,
) -> ValidatedWebsite:
    writer = _modules().writer
    validation = _TreeValidation(_modules())
    target = str(manifest["freshness_sha256"])
    path = output.path / target
    _require_output_root(output)
    if writer._entry(output.descriptor, target) is not None:
        existing = _open_validated_at(
            output.descriptor, target, path, expected=manifest,
            semantic_validation=validation,
        )
        try:
            return _return_target(output, target, existing, verify_sources)
        except BaseException:
            existing.close()
            raise
    temporary = f".{target}.build-{os.getpid()}-{uuid.uuid4().hex}"
    root: int | None = None
    root_created = False
    directories: dict[str, int] = {}
    keep_root = False

    def preserve_after_failure() -> None:
        if root is not None:
            _preserve_private_tree(output, temporary, target, root)
        elif root_created:
            print(
                f"compatibility-website: WARNING: retained private tree {temporary!r}",
                file=sys.stderr,
            )

    try:
        with _defer_termination():
            os.mkdir(temporary, 0o700, dir_fd=output.descriptor)
            root_created = True
            root = writer._open_directory(output.descriptor, temporary)
            writer._require_entry(
                output.descriptor,
                temporary,
                root,
                "private website changed while being opened",
            )
        for directory in FIXED_DIRECTORIES:
            with _defer_termination():
                os.mkdir(directory, 0o755, dir_fd=root)
                descriptor = writer._open_directory(root, directory)
                writer._require_entry(
                    root,
                    directory,
                    descriptor,
                    f"private website {directory} changed while being opened",
                )
                directories[directory] = descriptor
        for relative, value in files.items():
            parts = PurePosixPath(relative).parts
            destination = root if len(parts) == 1 else directories[parts[0]]
            _write_owned(destination, parts[-1], value)
        for descriptor in directories.values():
            os.fchmod(descriptor, 0o555)
            writer._fsync_directory(descriptor)
        os.fchmod(root, 0o555)
        writer._fsync_directory(root)
        _read_open_tree(
            root, output.path / temporary, expected=manifest, require_name=False,
            semantic_validation=validation,
        )
        _require_output_root(output)
        if verify_sources is not None:
            verify_sources()
        _require_output_root(output)
        _read_open_tree(
            root, output.path / temporary, expected=manifest, require_name=False,
            semantic_validation=validation,
        )
        writer._require_entry(
            output.descriptor,
            temporary,
            root,
            "private website changed before publication",
        )
        try:
            _rename_noreplace(output.descriptor, temporary, target)
        except OSError as error:
            if error.errno != errno.EEXIST:
                raise
            winner = _open_validated_at(
                output.descriptor, target, path, expected=manifest,
                semantic_validation=validation,
            )
            try:
                _return_target(output, target, winner, verify_sources)
                _preserve_private_tree(output, temporary, target, root)
                return _return_target(output, target, winner, verify_sources)
            except BaseException:
                winner.close()
                raise
        writer._fsync_directory(output.descriptor)
        validated = _read_open_tree(
            root, path, expected=manifest, semantic_validation=validation,
        )[0]
        writer._require_entry(
            output.descriptor, target, root, "published website changed"
        )
        _require_output_root(output)
        result = _return_target(
            output,
            target,
            ValidatedWebsite(
                path, root, validated, reused=False, semantic_validation=validation,
            ),
            verify_sources,
        )
        keep_root = True
        return result
    except (KeyboardInterrupt, SystemExit, WebsiteSignal):
        preserve_after_failure()
        raise
    except (
        KeyError,
        OSError,
        RuntimeError,
        TypeError,
        ValueError,
        WebsiteRefused,
        writer.WriteRefused,
    ) as error:
        preserve_after_failure()
        raise WebsiteRefused(f"website publication refused: {error}") from error
    except BaseException:
        preserve_after_failure()
        raise
    finally:
        for descriptor in directories.values():
            os.close(descriptor)
        if root is not None and not keep_root:
            os.close(root)


def _git_oid(checkout: Path, reference: str) -> str:
    environment = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    export_inputs = _modules().export_inputs
    try:
        result = export_inputs.capture_bounded_process(
            [
                "git",
                "--no-replace-objects",
                "-C",
                str(checkout),
                "rev-parse",
                f"{reference}^{{commit}}",
            ],
            environment=environment,
            timeout=20,
            stdout_limit=MAX_GIT_OID_STDOUT_BYTES,
            stderr_limit=MAX_GIT_OID_STDERR_BYTES,
        )
    except (
        OSError,
        subprocess.TimeoutExpired,
        export_inputs.OutputLimitExceeded,
    ) as error:
        raise WebsiteRefused(
            f"cannot resolve {reference} in {checkout}: {error}"
        ) from error
    if result.returncode != 0 or result.stderr:
        detail = (result.stderr or result.stdout)[:512].decode(errors="replace").strip()
        raise WebsiteRefused(f"cannot resolve {reference} in {checkout}: {detail}")
    value = result.stdout.decode().strip()
    if len(value) != 40 or not all(byte in "0123456789abcdef" for byte in value):
        raise WebsiteRefused(
            f"{reference} in {checkout} is not a full lowercase commit"
        )
    return value


def _live_data(*, series_ref: str | None = None, published_only: bool = False, state_root: Path | None = None, validation_history_commit: str | None = None) -> dict[str, Any]:
    modules = _modules()
    parent_commit = _git_oid(REPOSITORY, "HEAD")
    hermit = REPOSITORY / "hermit"
    hermit_commit = _git_oid(hermit, "HEAD")
    hermit_main = _git_oid(hermit, "refs/remotes/origin/main")
    options = {}
    if series_ref is not None or published_only:
        options.update(series_ref=series_ref, published_only=published_only)
    if state_root is not None:
        options["state_root"] = state_root
    if validation_history_commit is not None:
        options["validation_history_commit"] = validation_history_commit
    try:
        return modules.build_data.build_data(
            REPOSITORY, parent_commit, hermit, hermit_commit, hermit_main,
            **options,
        )
    except modules.build_data.BuildRefused as error:
        raise WebsiteRefused(f"Stage 1 build refused: {error}") from error


def publish(
    data: Mapping[str, Any],
    output_root: Path,
    *,
    wording: str = "live",
    verify_sources: Callable[[], None] | None = None,
    presentation: Mapping[str, Any] | None = None,
) -> ValidatedWebsite:
    manifest, files = compose(data, wording=wording, presentation=presentation)
    expected_generators = json.loads(_json_bytes(manifest["generator"]["files"]))

    def combined_verification() -> None:
        if _generator_files() != expected_generators:
            raise WebsiteRefused("website generator files changed after composition")
        if verify_sources is not None:
            verify_sources()
        if _generator_files() != expected_generators:
            raise WebsiteRefused(
                "website generator files changed during source verification"
            )

    output = _lock_output_root(output_root)
    try:
        result = _materialize(
            output, manifest, files, verify_sources=combined_verification
        )
    except BaseException:
        output.close()
        raise
    result.output_root = output
    result.verify_sources = combined_verification
    return result


def _require_live_sources(data: Mapping[str, Any]) -> None:
    modules = _modules()
    provenance = _object(data.get("provenance"), "website provenance")
    hermit = REPOSITORY / "hermit"
    try:
        modules.build_data._require_checkout(
            REPOSITORY,
            str(provenance["parent_commit"]),
            label="dev-hermit",
            clean=True,
        )
        modules.build_data._require_checkout(
            hermit,
            str(provenance["hermit_commit"]),
            label="Hermit",
            clean=True,
        )
        modules.build_data._require_main_target(
            hermit, str(provenance["hermit_main_commit"])
        )
    except (KeyError, modules.build_data.BuildRefused) as error:
        raise WebsiteRefused(
            f"website source checkout changed after Stage 1: {error}"
        ) from error


def _runtime_source_identity() -> tuple[str, str, str, str]:
    modules = _modules()
    parent_commit = _git_oid(REPOSITORY, "HEAD")
    hermit = REPOSITORY / "hermit"
    hermit_commit = _git_oid(hermit, "HEAD")
    try:
        modules.build_data._require_checkout(
            REPOSITORY, parent_commit, label="dev-hermit", clean=True
        )
        modules.build_data._require_checkout(
            hermit, hermit_commit, label="Hermit", clean=True
        )
    except modules.build_data.BuildRefused as error:
        raise WebsiteRefused(
            f"website server source checkout changed: {error}"
        ) from error
    return (
        parent_commit,
        hermit_commit,
        _sha256(_regular_bytes(SERVE_PATH)),
        _sha256(_regular_bytes(SERVE_PRELUDE_PATH)),
    )


def capture_presentation(
    data: Mapping[str, Any], build_started_at: str, *,
    parent: Path = REPOSITORY, hermit: Path | None = None,
    state_root: Path | None = None,
) -> dict[str, Any]:
    """Capture dates from immutable commit objects, separately from test times."""
    modules = _modules()
    repositories = {"dev-hermit": parent, "hermit": hermit or parent / "hermit"}
    identities = modules.renderer.presentation_commits(data)
    if any(repository == "hermit_test_ledger" for repository, _commit in identities):
        try:
            repositories["hermit_test_ledger"] = modules.series.ledger_root(state_root or parent)
        except modules.series.SeriesError as error:
            raise WebsiteRefused(f"cannot resolve recorded cell ledger: {error}") from error
    environment = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    rows = []
    for repository, commit in identities:
        if len(commit) != 40 or any(value not in "0123456789abcdef" for value in commit):
            raise WebsiteRefused("presentation source is not a full commit identity")
        try:
            result = modules.export_inputs.capture_bounded_process(
                ["git", "--no-replace-objects", "-C", str(repositories[repository]),
                 "show", "--no-patch", "--format=%cI", f"{commit}^{{commit}}"],
                environment=environment, timeout=20, stdout_limit=128, stderr_limit=512,
            )
        except (OSError, subprocess.TimeoutExpired, modules.export_inputs.OutputLimitExceeded) as error:
            raise WebsiteRefused(f"cannot capture source commit date: {error}") from error
        committed_at = None
        if result.returncode == 0:
            try:
                instant = datetime.fromisoformat(result.stdout.decode().strip())
                if instant.tzinfo is None or result.stderr:
                    raise ValueError("commit date is not an unambiguous Git timestamp")
                committed_at = instant.astimezone(timezone.utc).isoformat()
            except (UnicodeError, ValueError) as error:
                raise WebsiteRefused(f"cannot read source commit date: {error}") from error
        # Historical source objects may be absent in this checkout. Do not
        # substitute HEAD, the ledger emission date, or a filesystem timestamp.
        rows.append({"repository": repository, "commit": commit, "committed_at": committed_at})
    value = {"build_started_at": build_started_at, "commits": rows}
    try:
        return modules.renderer.validate_presentation(value, data)
    except modules.renderer.RenderRefused as error:
        raise WebsiteRefused(str(error)) from error


def build(output_root: Path = DEFAULT_OUTPUT_ROOT, *, series_ref: str | None = None, published_only: bool = False, state_root: Path | None = None, validation_history_commit: str | None = None) -> ValidatedWebsite:
    # Refuse an unusable destination before compiling or exporting source data.
    # Publication repeats this check while acquiring the output-root lock.
    _output_location(output_root)
    build_started_at = datetime.now(timezone.utc).isoformat()
    options = {}
    if series_ref is not None or published_only:
        options.update(series_ref=series_ref, published_only=published_only)
    if state_root is not None:
        options["state_root"] = state_root
    if validation_history_commit is not None:
        options["validation_history_commit"] = validation_history_commit
    def live_data() -> dict[str, Any]:
        return _live_data(**options)

    phase = _modules().build_data.phase
    with phase("build.stage1"):
        data = json.loads(_json_bytes(live_data()))
    with phase("build.presentation"):
        presentation = capture_presentation(data, build_started_at, state_root=state_root)
    with phase("build.stage1_tree"):
        expected_stage_1 = _stage_1_files(data)

    def verify_live_sources() -> None:
        with phase("verify_sources"):
            _require_live_sources(data)
            observed_stage_1 = _stage_1_files(live_data())
            _require_live_sources(data)
            if observed_stage_1 != expected_stage_1:
                raise WebsiteRefused("Stage 1 source bytes changed during website build")

    with phase("build.publish"):
        return publish(
            data,
            output_root,
            wording="live",
            verify_sources=verify_live_sources,
            presentation=presentation,
        )


def build_historical(
    parent: Path,
    parent_commit: str,
    record_id: str,
    run_id: str,
    manifest_hermit: Path,
    manifest_commit: str,
    output_root: Path = DEFAULT_OUTPUT_ROOT,
    *, series_commit: str | None = None,
) -> ValidatedWebsite:
    _output_location(output_root)
    build_started_at = datetime.now(timezone.utc).isoformat()
    modules = _modules()

    def source_data() -> dict[str, Any]:
        try:
            return modules.build_data.build_historical_data(
                parent,
                parent_commit,
                record_id,
                run_id,
                manifest_hermit,
                manifest_commit,
                **({"series_commit": series_commit} if series_commit is not None else {}),
            )
        except modules.build_data.BuildRefused as error:
            raise WebsiteRefused(
                f"historical Stage 1 build refused: {error}"
            ) from error

    data = json.loads(_json_bytes(source_data()))
    presentation = capture_presentation(data, build_started_at, parent=parent, hermit=manifest_hermit)
    expected_stage_1 = _stage_1_files(data)

    def verify_historical_sources() -> None:
        observed_stage_1 = _stage_1_files(source_data())
        if observed_stage_1 != expected_stage_1:
            raise WebsiteRefused(
                "historical Stage 1 source bytes changed during website build"
            )

    return publish(
        data,
        output_root,
        wording="historical",
        verify_sources=verify_historical_sources,
        presentation=presentation,
    )


def _result(value: ValidatedWebsite) -> dict[str, Any]:
    encoded = _json_bytes(value.manifest)
    return {
        "build_json_sha256": _sha256(encoded + b"\n"),
        "freshness_sha256": value.manifest["freshness_sha256"],
        "path": str(value.path),
        "provenance": json.loads(_json_bytes(value.manifest["provenance"])),
        "reused": value.reused,
        "schema_version": SCHEMA_VERSION,
        "tree_sha256": value.manifest["tree_sha256"],
    }


def _exec_server(
    value: ValidatedWebsite,
    port: int,
    result: Mapping[str, Any] | None = None,
) -> None:
    try:
        runtime_sources = _runtime_source_identity()
        value.recheck()
        if _runtime_source_identity() != runtime_sources:
            raise WebsiteRefused("website server sources changed before execution")
        if result is not None:
            print(
                "compatibility-website: " + _json_bytes(result).decode(),
                file=sys.stderr,
                flush=True,
            )
        os.set_inheritable(value.descriptor, True)
        value.release_output_root()
        os.execvp(
            "rust-script",
            [
                "rust-script",
                "--force",
                str(SERVE_PATH),
                "--root-fd",
                str(value.descriptor),
                "--port",
                str(port),
            ],
        )
    except BaseException as error:
        value.close()
        if isinstance(error, OSError):
            raise WebsiteRefused(
                f"cannot execute compatibility website server: {error}"
            ) from error
        raise


def _port(value: str) -> int:
    try:
        port = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "port must be an integer from 0 through 65535"
        ) from error
    if not 0 <= port <= 65535:
        raise argparse.ArgumentTypeError("port must be an integer from 0 through 65535")
    return port


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="ci-hub compatibility-website",
        description=__doc__,
        allow_abbrev=False,
    )
    commands = parser.add_subparsers(dest="command")
    build_parser = commands.add_parser(
        "build", help="build or reuse the exact live website", allow_abbrev=False
    )
    build_parser.add_argument(
        "--output-root",
        type=Path,
        default=DEFAULT_OUTPUT_ROOT,
        help="user-owned publication directory (default: checkout sibling)",
    )
    nightly = commands.add_parser("nightly", help="build and publish through the existing daily health scheduler", allow_abbrev=False)
    nightly.add_argument("action", choices=("launch", "run"))
    nightly.add_argument("--state-root", type=Path, required=True)
    serve_parser = commands.add_parser(
        "serve",
        help="validate and serve an existing website",
        allow_abbrev=False,
    )
    serve_parser.add_argument(
        "--root", required=True, type=Path, help="absolute validated digest directory"
    )
    serve_parser.add_argument(
        "--port", type=_port, default=0, help="loopback TCP port (default: 0)"
    )
    combined = commands.add_parser(
        "build-and-serve",
        help="build or reuse the live website, then serve it",
        allow_abbrev=False,
    )
    combined.add_argument(
        "--output-root",
        type=Path,
        default=DEFAULT_OUTPUT_ROOT,
        help="user-owned publication directory (default: checkout sibling)",
    )
    combined.add_argument(
        "--port", type=_port, default=0, help="loopback TCP port (default: 0)"
    )
    for name, help_text in (
        ("build-historical", "build or reuse one immutable historical snapshot"),
        (
            "build-historical-and-serve",
            "build or reuse one immutable historical snapshot, then serve it",
        ),
    ):
        historical = commands.add_parser(name, help=help_text, allow_abbrev=False)
        historical.add_argument(
            "--parent",
            type=Path,
            default=REPOSITORY,
            help="dev-hermit Git repository containing the immutable ledger commit",
        )
        historical.add_argument(
            "--parent-commit",
            required=True,
            help="full immutable dev-hermit commit containing published ledger and series",
        )
        historical.add_argument("--record-id", required=True)
        historical.add_argument("--run-id", required=True)
        historical.add_argument("--series-commit", help="full immutable hermit_test_ledger commit supplying historical series and finalized proof")
        historical.add_argument(
            "--manifest-hermit",
            required=True,
            type=Path,
            help="clean exact Hermit checkout providing manifest metadata",
        )
        historical.add_argument(
            "--manifest-commit",
            required=True,
            help="full commit checked out by --manifest-hermit",
        )
        historical.add_argument(
            "--output-root",
            type=Path,
            default=DEFAULT_OUTPUT_ROOT,
            help="user-owned publication directory (default: checkout sibling)",
        )
        if name.endswith("and-serve"):
            historical.add_argument(
                "--port",
                type=_port,
                default=0,
                help="loopback TCP port (default: 0)",
            )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    raw = list(sys.argv[1:] if argv is None else argv)
    arguments = _parser().parse_args(raw or ["build-and-serve"])
    if os.environ.get("CI_HUB_DOCS_PARSE_ONLY") is not None:
        print("DOCS PARSE OK: compatibility-website " + " ".join(raw))
        return 0
    try:
        if arguments.command == "nightly":
            module_path = REPOSITORY / "ci-hub/compatibility-website/nightly_publication.py"
            specification = importlib.util.spec_from_file_location("compatibility_website_nightly", module_path)
            if specification is None or specification.loader is None:
                raise WebsiteRefused("cannot load nightly publication module")
            nightly = importlib.util.module_from_spec(specification)
            specification.loader.exec_module(nightly)
            # The CLI re-executes its captured source in a private namespace;
            # sys.modules['__main__'] still names the earlier bootstrap module.
            return getattr(nightly, arguments.action)(SimpleNamespace(**globals()), arguments.state_root.resolve())
        if arguments.command == "serve":
            website = validate_website(arguments.root)
            _exec_server(website, arguments.port)
            raise AssertionError("exec returned")
        if arguments.command in ("build-historical", "build-historical-and-serve"):
            website = build_historical(
                arguments.parent,
                arguments.parent_commit,
                arguments.record_id,
                arguments.run_id,
                arguments.manifest_hermit,
                arguments.manifest_commit,
                arguments.output_root,
                **({"series_commit": arguments.series_commit} if arguments.series_commit is not None else {}),
            )
        else:
            website = build(arguments.output_root)
        result = _result(website)
        if arguments.command in ("build", "build-historical"):
            try:
                website.recheck()
                sys.stdout.buffer.write(_json_bytes(result) + b"\n")
                sys.stdout.buffer.flush()
            finally:
                website.close()
            return 0
        _exec_server(website, arguments.port, result)
        raise AssertionError("exec returned")
    except (WebsiteRefused, OSError, RuntimeError, ValueError) as error:
        print(f"compatibility-website: REFUSED: {error}", file=sys.stderr)
        return 2


def _entrypoint(argv: Sequence[str] | None = None) -> int:
    handled = (signal.SIGHUP, signal.SIGINT, signal.SIGTERM)
    previous = {signum: signal.getsignal(signum) for signum in handled}

    def interrupt(signum: int, _frame: Any) -> None:
        raise WebsiteSignal(signum)

    for signum in handled:
        signal.signal(signum, interrupt)
    try:
        return main(argv)
    except WebsiteSignal as interruption:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        signal.signal(interruption.signum, signal.SIG_DFL)
        os.kill(os.getpid(), interruption.signum)
        raise AssertionError("signal termination returned")
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


if __name__ == "__main__":
    if _BOOTSTRAP_SOURCE_BYTES is None:
        captured_self = _regular_bytes(Path(__file__).resolve())
        namespace = {
            "__builtins__": __builtins__,
            "__file__": str(Path(__file__).resolve()),
            "__name__": "__main__",
            "__package__": None,
            "_BOOTSTRAP_SOURCE_BYTES": captured_self,
        }
        exec(
            compile(captured_self, namespace["__file__"], "exec", dont_inherit=True),
            namespace,
        )
        raise AssertionError("captured compatibility website entrypoint returned")
    raise SystemExit(_entrypoint())
