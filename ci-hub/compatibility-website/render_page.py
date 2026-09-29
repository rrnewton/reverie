#!/usr/bin/env python3
"""Render a deterministic static compatibility site from Stage 1 artifacts.

The renderer consumes only the validated Stage 1 projection.  It does not read
the ledger and it does not decide which rows qualify: those decisions belong to
the Rust projection and the reducer.  Local JavaScript loads verified data and
keeps each live listing bounded to its current page.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import html
import importlib.util
import json
import math
import os
import posixpath
import re
import sys
import uuid
from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, replace
from datetime import datetime, timedelta, timezone
from fractions import Fraction
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlencode
from zoneinfo import ZoneInfo


WRITE_DATA_PATH = Path(__file__).resolve().with_name("write_data.py")
SITE_CSS_PATH = Path(__file__).resolve().parent / "assets/site.css"
SITE_JS_PATH = Path(__file__).resolve().parent / "assets/site.js"
FAVICON_PATH = Path(__file__).resolve().parent / "assets/favicon.svg"
FAVICON_SHA256 = "f011d128a46779a767e70bce4d66431cbade170151663ddc36325abbb5e2c4da"
STATE_ORDER = ("green", "red", "never-measured")
STATE_LABELS = {
    "green": "Green",
    "red": "Red",
    "never-measured": "Never measured",
}
DETAIL_DIRECTORIES = ("assets", "cells", "data", "runs", "tests")
BROWSER_SCHEMA_VERSION = 1
HEX_SHA256 = re.compile(r"[0-9a-f]{64}")
RFC3339_UTC = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|\+00:00)")
EASTERN_TIME_ZONE = ZoneInfo("America/New_York")
LIST_PAGE_SIZE = 50
SELECTED_GROUP_PAGE_SIZE = 8
SELECTED_GROUP_CHILD_LIMIT = 25
MAX_U64 = (1 << 64) - 1
# The website builder counts, per validation run, the test attempts that the
# run recorded and the published history lacks, with no record of why, under
# this schema (MISSING_CENSUS_ATTEMPT_SCHEMA in
# ci-hub/lib/compatibility_website.rs; a test checks that the two agree).
MISSING_CENSUS_ATTEMPT_SCHEMA = "census-attempt-missing-from-series/v1"
# The main page lists at most this many runs' missing attempts; the projection
# audit lists every run.
MISSING_ATTEMPT_RUNS_LISTED = 10
# The exact keys of one projected no-verdict attempt: the object that
# project_attempt_disposition in ci-hub/lib/compatibility_website.rs writes
# (a test checks that the two agree). The projection always writes `detail`,
# as null when the ledger row has none; the ledger row itself omits a null
# `detail`. This page reads projected rows only.
PROJECTED_NO_VERDICT_ATTEMPT_FIELDS = frozenset(
    {
        "attempt_outcome",
        "detail",
        "disposition",
        "error_kind",
        "index",
        "kind",
        "signal",
        "status",
        "timed_out",
        "verification_report_sha256",
    }
)
DIVERGENCE_COORDINATES = (
    ("first_divergent_record", "First divergent record"),
    ("first_divergent_syscall", "First divergent syscall"),
    ("first_divergent_scheduler_turn", "First divergent scheduler turn"),
    ("first_divergent_virtual_nanoseconds", "First divergent virtual nanoseconds"),
)
# The largest measured Stage 1 build record was 1,520,395 bytes in the
# previously published 8,006-file site.  Keep four times that observed
# file-count trajectory (32,024 files) while retaining a finite refusal bound.
MEASURED_STAGE1_BUILD_JSON_BYTES = 1_520_395
MEASURED_PUBLISHED_SITE_FILE_COUNT = 8_006
SUPPORTED_PUBLISHED_SITE_FILE_COUNT = 4 * MEASURED_PUBLISHED_SITE_FILE_COUNT
MAX_STAGE1_BUILD_JSON_BYTES = (
    MEASURED_STAGE1_BUILD_JSON_BYTES * SUPPORTED_PUBLISHED_SITE_FILE_COUNT
    + MEASURED_PUBLISHED_SITE_FILE_COUNT
    - 1
) // MEASURED_PUBLISHED_SITE_FILE_COUNT


class RenderRefused(Exception):
    """The supplied Stage 1 artifacts or output destination are unsafe."""


@dataclass(frozen=True)
class ProvenanceFact:
    """One visible provenance fact, optionally linked to its commit."""

    label: str
    value: str
    href: str | None = None


@dataclass(frozen=True)
class PageWording:
    """Explicit fixture, live, or historical wording for one rendered tree."""

    meta_description: str
    title_suffix: str
    banner: str | None
    footer: str
    provenance_heading: str
    landing_lead: str
    cell_table_caption: str
    cells_lead: str
    landing_introduction: bool = False
    disclosure_summary: str | None = None
    disclosure: tuple[ProvenanceFact, ...] = ()


PAGE_WORDING = {
    "fixture": PageWording(
        meta_description="Fixture-backed Hermit compatibility evidence",
        title_suffix="Hermit compatibility fixture",
        banner="Fixture data — not current Hermit main",
        footer=(
            "Generated locally from validated Stage 1 fixture artifacts. "
            "No network resources are used."
        ),
        provenance_heading="Fixture provenance",
        landing_lead=(
            "A navigable view of recorded facts in the deterministic Stage 1 fixture."
        ),
        cell_table_caption=("Green and red result rows in the fixture."),
        cells_lead=(
            "Cells that the Stage 1 projection can render, with links to their "
            "admitted canonical series rows."
        ),
    ),
    "live": PageWording(
        meta_description="Hermit compatibility evidence from validated Stage 1 artifacts",
        title_suffix="Hermit compatibility",
        banner=None,
        footer=(
            "Generated locally from validated Stage 1 artifacts. "
            "No network resources are used."
        ),
        provenance_heading="Build provenance",
        landing_lead=(
            "Current determinism, parity, and coverage evidence for Hermit's "
            "Linux execution backends."
        ),
        landing_introduction=True,
        cell_table_caption="Green and red result rows in this build.",
        cells_lead=(
            "Cells that the Stage 1 projection can render, with links to their "
            "admitted canonical series rows."
        ),
    ),
}


def _eastern_timestamp(value: Any, label: str, *, include_seconds: bool) -> str:
    if not isinstance(value, str) or RFC3339_UTC.fullmatch(value) is None:
        raise RenderRefused(f"{label} must be a UTC RFC3339 timestamp")
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        instant = datetime.fromisoformat(normalized)
    except ValueError as error:
        raise RenderRefused(f"{label} must be a UTC RFC3339 timestamp") from error
    if instant.utcoffset() != timedelta(0):
        raise RenderRefused(f"{label} must be a UTC RFC3339 timestamp")
    format_string = "%Y-%m-%d %H:%M:%S ET" if include_seconds else "%Y-%m-%d %H:%M ET"
    return instant.astimezone(EASTERN_TIME_ZONE).strftime(format_string)


def _historical_finished_at_label(value: Any) -> str:
    return _eastern_timestamp(value, "historical finished_at", include_seconds=False)


def _commit_url(repository: str, commit: str) -> str:
    return f"https://github.com/rrnewton/{repository}/commit/{commit}"


def _historical_wording(provenance: Mapping[str, Any]) -> PageWording:
    historical = provenance.get("historical")
    if not isinstance(historical, Mapping):
        raise RenderRefused("historical page wording requires historical provenance")
    fields = (
        "ledger_ref",
        "ledger_commit",
        "record_id",
        "run_id",
        "validate_path",
        "selected_by_full_cell_count",
        "measurement_host",
        "measured_hermit_commit",
        "measured_reverie_commit",
        "kernel_version",
        "manifest_source_commit",
        "started_at",
        "finished_at",
        "history_row_sha256",
    )
    strings = tuple(field for field in fields if field != "selected_by_full_cell_count")
    if any(
        not isinstance(historical.get(field), str) or not historical[field]
        for field in strings
    ):
        raise RenderRefused("historical page provenance is incomplete")
    selected_count = historical.get("selected_by_full_cell_count")
    recorded_only = selected_count is None
    if recorded_only:
        if (historical.get("comparison_authority") != "writer_verified_raw_census_v1"
                or historical.get("population_scope") != "recorded_only"
                or type(historical.get("recorded_cell_count")) is not int
                or historical["recorded_cell_count"] < 0):
            raise RenderRefused("historical recorded population lacks writer-derived authority")
    elif type(selected_count) is not int or selected_count < 0 or any(
        field in historical for field in ("recorded_cell_count", "population_scope", "comparison_authority")
    ):
        raise RenderRefused("historical page mixes planned and recorded-only populations")
    finished_at_label = _historical_finished_at_label(historical["finished_at"])
    canonical = provenance.get("canonical_input_provenance")
    if not isinstance(canonical, Mapping):
        raise RenderRefused(
            "historical page wording requires canonical input provenance"
        )
    reader = canonical.get("reader_hermit")
    if not isinstance(reader, Mapping):
        raise RenderRefused("historical page wording requires reader Hermit provenance")
    hashes = (
        canonical.get("reader_files_sha256"),
        canonical.get("reader_source_identity_sha256"),
        canonical.get("sources_sha256"),
    )
    commits = (
        reader.get("checkout_commit"),
        reader.get("gitlink_commit"),
        canonical.get("reader_commit"),
        provenance.get("hermit_main_commit"),
        historical.get("measured_reverie_commit"),
    )
    if any(
        not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{40}", value) is None
        for value in commits
    ) or any(
        not isinstance(value, str) or HEX_SHA256.fullmatch(value) is None
        for value in hashes
    ):
        raise RenderRefused("historical generator provenance is incomplete")
    capabilities = historical.get("host_capabilities")
    if not isinstance(capabilities, Mapping) or set(capabilities) != {
        "cpuid-faulting",
        "kvm",
    }:
        raise RenderRefused("historical host capability provenance is incomplete")
    capability_labels = []
    for name in sorted(capabilities):
        verdict = capabilities[name]
        if (
            not isinstance(verdict, Mapping)
            or not isinstance(verdict.get("present"), bool)
            or not isinstance(verdict.get("evidence"), str)
            or not verdict["evidence"].strip()
        ):
            raise RenderRefused("historical host capability provenance is incomplete")
        state = "present" if verdict["present"] else "absent"
        capability_labels.append(f"{name}: {state} ({verdict['evidence']})")
    clean = canonical.get("reader_worktree_clean")
    dirty = canonical.get("reader_worktree_dirty_paths")
    if (
        not isinstance(clean, bool)
        or not isinstance(dirty, list)
        or any(not isinstance(path, str) or not path for path in dirty)
        or clean != (not dirty)
    ):
        raise RenderRefused("historical generator source state is inconsistent")
    measured = str(historical["measured_hermit_commit"])
    manifest = str(historical["manifest_source_commit"])
    source_state = "clean" if clean else "uncommitted/dirty (" + ", ".join(dirty) + ")"
    disclosure = (
        ProvenanceFact("Generator parent commit", str(canonical["reader_commit"])),
        ProvenanceFact("Generator parent source state", source_state),
        ProvenanceFact(
            "Clean generator Hermit checkout", str(reader["checkout_commit"])
        ),
        ProvenanceFact(
            "Generator-base-recorded Hermit gitlink",
            str(reader["gitlink_commit"]),
            _commit_url("hermit", str(reader["gitlink_commit"])),
        ),
        ProvenanceFact("Ledger reference", str(historical["ledger_ref"])),
        ProvenanceFact(
            "Ledger commit",
            str(historical["ledger_commit"]),
            _commit_url("dev-hermit", str(historical["ledger_commit"])),
        ),
        ProvenanceFact(
            "Ledger-era Hermit gitlink", str(provenance["hermit_main_commit"])
        ),
        ProvenanceFact("Source validation record", str(historical["record_id"])),
        ProvenanceFact("Source validation run", str(historical["run_id"])),
        ProvenanceFact("Validate path", str(historical["validate_path"])),
        ProvenanceFact(
            "Retained recorded population (planned selection unavailable)" if recorded_only else f"Retained run selected by {historical['validate_path']}",
            f"{_integer(historical['recorded_cell_count'] if recorded_only else selected_count)} cells",
        ),
        ProvenanceFact(
            "Run started",
            _eastern_timestamp(
                historical["started_at"],
                "historical started_at",
                include_seconds=True,
            ),
        ),
        ProvenanceFact(
            "Run finished",
            _eastern_timestamp(
                historical["finished_at"],
                "historical finished_at",
                include_seconds=True,
            ),
        ),
        ProvenanceFact(
            "Source history row SHA-256", str(historical["history_row_sha256"])
        ),
        ProvenanceFact("Measurement host", str(historical["measurement_host"])),
        ProvenanceFact(
            "Measured Hermit commit",
            measured,
            _commit_url("hermit", measured),
        ),
        ProvenanceFact(
            "Measured Reverie gitlink",
            str(historical["measured_reverie_commit"]),
            _commit_url("reverie", str(historical["measured_reverie_commit"])),
        ),
        ProvenanceFact("Measurement kernel", str(historical["kernel_version"])),
        ProvenanceFact("Host capability set", "; ".join(capability_labels)),
        ProvenanceFact(
            "Manifest source commit",
            manifest,
            _commit_url("hermit", manifest),
        ),
        ProvenanceFact(
            "Manifest and measured commits",
            (
                "manifest source differs from measured Hermit"
                if manifest != measured
                else "manifest source is the same as measured Hermit"
            ),
        ),
        ProvenanceFact("Reader files SHA-256", str(hashes[0])),
        ProvenanceFact("Reader identity SHA-256", str(hashes[1])),
        ProvenanceFact("Captured source inventory SHA-256", str(hashes[2])),
    )
    return PageWording(
        meta_description="Hermit compatibility evidence from a recorded snapshot",
        title_suffix="Hermit compatibility snapshot",
        banner=None,
        footer=(
            "Generated from the published evidence named in the provenance disclosure. "
            "No external network resources are used."
        ),
        provenance_heading="Historical snapshot provenance",
        landing_lead=(
            "The Hermit container enables deterministic execution of guest Linux "
            "programs."
        ),
        cell_table_caption=(
            "Green and red result rows in the separately identified manifest source."
        ),
        cells_lead=(
            "Rendered cells from the separately identified manifest source, with "
            "links to admitted canonical series rows in this snapshot."
        ),
        landing_introduction=True,
        disclosure_summary=f"Full snapshot provenance ({finished_at_label})",
        disclosure=disclosure,
    )


def _page_wording(value: str, provenance: Mapping[str, Any]) -> PageWording:
    if value == "historical":
        return _historical_wording(provenance)
    try:
        return PAGE_WORDING[value]
    except (KeyError, TypeError) as error:
        raise RenderRefused(
            "page wording must be exactly 'fixture', 'live', or 'historical'"
        ) from error


@dataclass(frozen=True)
class SiteModel:
    build: Mapping[str, Any]
    data: Mapping[str, Any]
    cells: tuple[Mapping[str, Any], ...]
    tests: tuple[Mapping[str, Any], ...]
    custom_commands: tuple[Mapping[str, Any], ...]
    runs: Mapping[str, Any]
    series_rows: tuple[Mapping[str, Any], ...]
    history_rows: tuple[Mapping[str, Any], ...]
    current_history_indices: tuple[int, ...]
    cell_by_id: Mapping[str, Mapping[str, Any]]
    test_by_id: Mapping[str, Mapping[str, Any]]
    manifest_cell_ids: frozenset[str]
    series_by_cell: Mapping[str, tuple[Mapping[str, Any], ...]]
    series_by_event: Mapping[str, Mapping[str, Any]]
    recorded_outside_manifest: tuple[Mapping[str, Any], ...]
    host_capability_names: tuple[str, ...]
    cell_paths: Mapping[str, str]
    test_paths: Mapping[str, str]
    history_paths: tuple[str, ...]
    selection_run_references: Mapping[tuple[str, str, str, str], Mapping[str, Any]]
    event_run_references: Mapping[str, Mapping[str, Any]]
    wording: PageWording
    presentation: Mapping[str, Any] | None = None

    @functools.cached_property
    def backend_history(self) -> list[dict[str, Any]]:
        return _compute_backend_history(self)


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
    """Recompose the exact schema-v3 object from its four-file envelope."""
    return {
        "cells": payloads["data/cells.json.gz"]["cells"],
        "counts": build["counts"],
        "projection_audit": payloads["data/cells.json.gz"]["projection_audit"],
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
        raw_build = writer._read_file(
            directory, "build.json", MAX_STAGE1_BUILD_JSON_BYTES
        )
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


def _test_name(test: Any, category: Any) -> str:
    identity = str(test)
    prefix = f"{category}/"
    return identity[len(prefix) :] if identity.startswith(prefix) else identity


def _display_timestamp(value: Any, label: str) -> str:
    if value is None or value == "":
        return "Not recorded"
    return _eastern_timestamp(value, label, include_seconds=True)


def _time_element(value: Any, label: str) -> str:
    if value is None or value == "":
        return "Not recorded"
    return (
        f'<time datetime="{_escape(value)}">'
        f"{_escape(_display_timestamp(value, label))}</time>"
    )


def _duration_seconds(value: Any, label: str) -> str:
    """Render one recorded duration without deriving it from wall timestamps."""
    if value is None:
        return "Not recorded"
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(value)
        or value < 0
    ):
        raise RenderRefused(f"{label} must be a finite nonnegative number")
    unit = "second" if value == 1 else "seconds"
    return f"{value} {unit}"


def _column_heading(
    label: str,
    description: str,
    *,
    sort: str | None = None,
    sort_kind: str | None = None,
) -> str:
    if (sort is None) != (sort_kind is None):
        raise RenderRefused("sortable column heading is incomplete")
    if sort_kind not in {None, "cell", "list"}:
        raise RenderRefused(f"unsupported sortable column kind {sort_kind!r}")
    if sort is None:
        tooltip_id = _fragment("column-help", f"{label}\0{description}")
        body = (
            f'<span class="column-heading"><span>{_escape(label)}</span>'
            '<span class="column-help-wrap">'
            f'<button type="button" class="column-help" title="{_escape(description)}" '
            f'aria-label="Explain {_escape(label)}" aria-describedby="{tooltip_id}">'
            '<span aria-hidden="true">?</span></button>'
            f'<span class="column-tooltip" id="{tooltip_id}" role="tooltip">'
            f"{_escape(description)}</span></span></span>"
        )
        return f'<th scope="col">{body}</th>'
    attribute = "data-cell-sort" if sort_kind == "cell" else "data-list-sort"
    return (
        '<th scope="col" aria-sort="none">'
        f'<button type="button" class="table-sort" {attribute}="{_escape(sort)}" '
        f'title="{_escape(description)}" aria-label="Sort by {_escape(label)}. {_escape(description)}">'
        f'<span>{_escape(label)}</span><span class="sort-indicator" aria-hidden="true"></span>'
        "</button></th>"
    )


def _hermit_commit_link(commit: Any) -> str:
    value = str(commit)
    if re.fullmatch(r"[0-9a-f]{40}", value) is None:
        raise RenderRefused(f"Hermit commit is not a full SHA: {value!r}")
    return _github_link(_commit_url("hermit", value), value[:12], code=True)


def _github_link(href: str, label: str, *, code: bool = False) -> str:
    if not href.startswith("https://github.com/rrnewton/"):
        raise RenderRefused(f"external provenance link is not an approved GitHub URL: {href!r}")
    visible = f"<code>{_escape(label)}</code>" if code else _escape(label)
    return (
        f'<a class="external-link" href="{_escape(href)}" '
        f'aria-label="{_escape(label)} on GitHub (external link)">{visible}'
        '<span class="external-link-indicator" aria-hidden="true">↗</span></a>'
    )


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _full_digest(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


def _fragment(kind: str, value: str) -> str:
    return f"{kind}-{_full_digest(value)}"


def _hashed_paths(directory: str, identities: Sequence[str]) -> dict[str, str]:
    paths = {
        identity: f"{directory}/{_full_digest(identity)}.html"
        for identity in identities
    }
    if len(set(paths.values())) != len(paths):
        raise RenderRefused(f"duplicate {directory} identity")
    return paths


def _query_paths(
    directory: str, parameter: str, identities: Sequence[str]
) -> dict[str, str]:
    paths = {
        identity: f"{directory}/detail.html?{parameter}={_full_digest(identity)}"
        for identity in identities
    }
    if len(set(paths.values())) != len(paths):
        raise RenderRefused(f"duplicate {directory} identity")
    return paths


def _href(source: str, target: str, fragment: str | None = None) -> str:
    source_parts = source.split("?", 1)[0].split("#", 1)[0]
    target_before_fragment, separator, target_fragment = target.partition("#")
    target_path_text, query_separator, target_query = target_before_fragment.partition(
        "?"
    )
    source_path = PurePosixPath(source_parts)
    target_path = PurePosixPath(target_path_text)
    selected_fragment = (
        fragment if fragment is not None else (target_fragment if separator else None)
    )
    if (
        source_path == target_path
        and selected_fragment is not None
        and not target_query
    ):
        return f"#{selected_fragment}"
    relative = posixpath.relpath(target_path.as_posix(), source_path.parent.as_posix())
    if query_separator:
        relative += f"?{target_query}"
    return (
        f"{relative}#{selected_fragment}" if selected_fragment is not None else relative
    )


def _query_href(
    source: str,
    target: str,
    values: Sequence[tuple[str, str]],
    fragment: str,
) -> str:
    relative = _href(source, target)
    query = urlencode(values)
    return f"{relative}?{query}#{fragment}" if query else f"{relative}#{fragment}"


def _cell_identity(row: Mapping[str, Any]) -> str:
    return f"{row['test']}/{row['mode']}/{row['backend']}"


def _positive_run_number(row: Mapping[str, Any]) -> int | None:
    value = row.get("run_number")
    if type(value) is int and 0 < value <= MAX_U64:
        return value
    return None


def _run_display_label(row: Mapping[str, Any]) -> str:
    run_number = _positive_run_number(row)
    if run_number is not None:
        return f"Run {_integer(run_number)}"
    return f"Run id {row['run_id']}"


def _ledger_record_label(current: bool) -> str:
    return "Current ledger record" if current else "Superseded ledger record"


def _ledger_record_explanation(current: bool) -> str:
    if current:
        return (
            "Current ledger record means this is the terminal row after applying "
            "the ledger correction chain for this run id. It does not describe code age."
        )
    return (
        "Superseded ledger record means a later correction replaced this retained row. "
        "It does not describe code age, and a correction can preserve every measurement value."
    )


def _correction_partner_indices(
    rows: Sequence[Mapping[str, Any]],
) -> tuple[tuple[int | None, tuple[int, ...]], ...]:
    by_record_id: dict[str, list[int]] = {}
    corrected_by_record_id: dict[str, list[int]] = {}
    for candidate_index, candidate in enumerate(rows):
        record_id = str(candidate["record_id"])
        by_record_id.setdefault(record_id, []).append(candidate_index)
        corrects = candidate.get("corrects")
        if corrects is None:
            continue
        if not isinstance(corrects, str) or not corrects:
            raise RenderRefused(
                "validation history corrects must be a nonempty record id"
            )
        corrected_by_record_id.setdefault(corrects, []).append(candidate_index)
    result = []
    for row in rows:
        corrects = row.get("corrects")
        candidates = () if corrects is None else by_record_id.get(str(corrects), ())
        corrected_index = candidates[0] if len(candidates) == 1 else None
        corrected_by = tuple(corrected_by_record_id.get(str(row["record_id"]), ()))
        result.append((corrected_index, corrected_by))
    return tuple(result)


def _correction_partner_links(
    model: SiteModel,
    page: str,
    index: int,
    partners: Sequence[tuple[int | None, tuple[int, ...]]] | None = None,
) -> str:
    partner_indices = (
        _correction_partner_indices(model.history_rows)
        if partners is None
        else partners
    )
    corrected, corrected_by = partner_indices[index]
    links: list[str] = []
    if corrected is not None:
        row = model.history_rows[corrected]
        links.append(
            f'Corrects <a href="{_escape(_href(page, model.history_paths[corrected]))}">'
            f"{_escape(_run_display_label(row))}</a>"
        )
    for partner in corrected_by:
        row = model.history_rows[partner]
        links.append(
            f'Corrected by <a href="{_escape(_href(page, model.history_paths[partner]))}">'
            f"{_escape(_run_display_label(row))}</a>"
        )
    return "<br>".join(links)


def _run_reference(
    row: Mapping[str, Any], *, path: str, selection: str
) -> dict[str, Any]:
    reference: dict[str, Any] = {
        "commit": str(row["commit"]),
        "history_row_sha256": str(row["history_row_sha256"]),
        "path": path,
        "run_id": str(row["run_id"]),
        "selection": selection,
    }
    run_number = _positive_run_number(row)
    if run_number is not None:
        reference["run_number"] = run_number
    return reference


def _browser_run_reference(reference: Mapping[str, Any]) -> dict[str, Any]:
    result = dict(reference)
    run_number = _positive_run_number(reference)
    if run_number is not None:
        # JSON numbers above 2^53 cannot be represented exactly in JavaScript.
        # This is display metadata, so retain the exact decimal text instead.
        result["run_number"] = str(run_number)
    else:
        result.pop("run_number", None)
    return result


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


def _selected_cell_identity(
    identity: str,
    fact: Mapping[str, Any],
    series_by_event: Mapping[str, Mapping[str, Any]],
) -> str:
    """Return the cell id under which a selected_by fact's run recorded it.

    The projection publishes a row recorded under a retired Hermit test id
    under its successor's cell and keeps the id it recorded as
    ``series.recorded_cell`` (``resolve_prepared_cell`` in
    ci-hub/lib/compatibility_website.rs).  The run's own history row still
    names the id it selected, which is the recorded one, so the fact binds to
    its run through that id.  Every event of the fact must belong to this cell
    and name the same recorded id.
    """
    recorded = set()
    for event_id in fact["event_ids"]:
        row = series_by_event.get(str(event_id))
        if row is None or str(row["series"]["cell"]) != identity:
            raise RenderRefused("selected_by fact names a series event outside its cell")
        recorded.add(str(row["series"].get("recorded_cell", identity)))
    if len(recorded) != 1:
        raise RenderRefused("selected_by fact does not name exactly one recorded cell")
    return recorded.pop()


def _current_history_indices(
    history: Sequence[Mapping[str, Any]], current: Sequence[Mapping[str, Any]]
) -> tuple[int, ...]:
    candidates: dict[str, list[int]] = {}
    for index, row in enumerate(history):
        candidates.setdefault(_canonical(row), []).append(index)
    result: list[int] = []
    for row in current:
        matches = candidates.get(_canonical(row), [])
        if not matches:
            raise RenderRefused("a terminal validation row is absent from history")
        result.append(matches.pop(0))
    if len(set(result)) != len(result):
        raise RenderRefused(
            "terminal validation rows do not identify unique history rows"
        )
    return tuple(sorted(result))


def _recorded_outside_manifest(
    rows: Sequence[Mapping[str, Any]], manifest_cells: frozenset[str]
) -> tuple[Mapping[str, Any], ...]:
    grouped: dict[str, dict[str, Any]] = {}
    for row in rows:
        identity = str(row["series"]["cell"])
        if identity in manifest_cells:
            continue
        item = grouped.setdefault(
            identity,
            {
                "cell": identity,
                "event_ids": [],
                "physical_row_count": 0,
                "represented_run_count": 0,
            },
        )
        item["event_ids"].append(str(row["event_id"]))
        item["physical_row_count"] += 1
        item["represented_run_count"] += int(row["series"].get("num_runs", 1))
    return tuple(grouped[key] for key in sorted(grouped))


def _site_model(
    build: Mapping[str, Any], data: Mapping[str, Any], *, wording: str
) -> SiteModel:
    writer = _writer()
    validator = writer._build_data_module()
    try:
        validated = validator.validate_build_data(data)
    except validator.BuildRefused as error:
        raise RenderRefused(f"Stage 1 data is invalid: {error}") from error
    schema_version = validator.SCHEMA_VERSION
    if build.get("schema_version") != schema_version:
        raise RenderRefused("Stage 1 build schema_version is unsupported")
    if build.get("counts") != validated["counts"]:
        raise RenderRefused("Stage 1 build counts disagree with its data")
    if build.get("provenance") != validated["provenance"]:
        raise RenderRefused("Stage 1 build provenance disagrees with its data")
    generator = build.get("generator")
    if (
        not isinstance(generator, Mapping)
        or generator.get("build_data_schema_version") != schema_version
    ):
        raise RenderRefused("Stage 1 generator schema_version is unsupported")

    cells = tuple(validated["cells"])
    tests = tuple(validated["tests"])
    custom_commands = tuple(validated["selected_by_full_custom_commands"])
    runs = validated["runs"]
    series_rows = tuple(runs["series_rows"])
    history_rows = tuple(runs["validation_history_rows"])
    current = tuple(runs["current_validation_run_rows"])
    cell_by_id = _unique_rows(cells, "cell", "cell")
    test_by_id = _unique_rows(tests, "id", "test")
    manifest_cells = frozenset(
        str(identity) for test in tests for identity in test["cells"]
    )
    if len(manifest_cells) != validated["counts"]["in_manifest_cell_count"]:
        raise RenderRefused("in-manifest count disagrees with test cell identities")
    if not set(cell_by_id).issubset(manifest_cells):
        raise RenderRefused("a rendered cell is absent from the manifest")
    series_by_cell: dict[str, list[Mapping[str, Any]]] = {
        identity: [] for identity in cell_by_id
    }
    capability_names: set[str] = set()
    for row in series_rows:
        identity = str(row["series"]["cell"])
        if identity in series_by_cell:
            series_by_cell[identity].append(row)
        capabilities = row["series"].get("host_capabilities")
        if isinstance(capabilities, Mapping):
            capability_names.update(str(name) for name in capabilities)
    history_keys = [
        _canonical(
            {
                "commit": row["commit"],
                "history_row_sha256": row["history_row_sha256"],
                "index": index,
                "run_id": row["run_id"],
                "selection": row["profile"],
            }
        )
        for index, row in enumerate(history_rows)
    ]
    history_path_map = _hashed_paths("runs", history_keys)
    history_paths = tuple(history_path_map[key] for key in history_keys)
    selection_run_references: dict[tuple[str, str, str, str], dict[str, Any]] = {}
    for row_index in _current_history_indices(history_rows, current):
        row = history_rows[row_index]
        for path, selection in _cell_evidence(row)["selected_by"].items():
            for selected_cell in selection["cells"]:
                key = (
                    _cell_identity(selected_cell),
                    str(path),
                    str(row["run_id"]),
                    str(row["commit"]),
                )
                if key in selection_run_references:
                    raise RenderRefused(
                        "run history does not uniquely bind cell selection"
                    )
                selection_run_references[key] = _run_reference(
                    row, path=history_paths[row_index], selection=str(path)
                )
    series_by_event = _unique_rows(series_rows, "event_id", "series event")
    event_run_references: dict[str, Mapping[str, Any]] = {}
    for cell in cells:
        identity = str(cell["cell"])
        for path, facts in cell["selected_by"].items():
            for fact in facts:
                key = (
                    _selected_cell_identity(identity, fact, series_by_event),
                    str(path),
                    str(fact["run_id"]),
                    str(fact["tree"]),
                )
                reference = selection_run_references.get(key)
                if reference is None:
                    raise RenderRefused(
                        "selected_by fact has no exact terminal history binding"
                    )
                for event_id in fact["event_ids"]:
                    previous = event_run_references.setdefault(str(event_id), reference)
                    if previous != reference:
                        raise RenderRefused(
                            "series event has contradictory terminal history bindings"
                        )
    return SiteModel(
        build=build,
        data=validated,
        cells=cells,
        tests=tests,
        custom_commands=custom_commands,
        runs=runs,
        series_rows=series_rows,
        history_rows=history_rows,
        current_history_indices=_current_history_indices(history_rows, current),
        cell_by_id=cell_by_id,
        test_by_id=test_by_id,
        manifest_cell_ids=manifest_cells,
        series_by_cell={key: tuple(value) for key, value in series_by_cell.items()},
        series_by_event=series_by_event,
        recorded_outside_manifest=_recorded_outside_manifest(
            series_rows, manifest_cells
        ),
        host_capability_names=tuple(sorted(capability_names)),
        cell_paths=_query_paths(
            "cells",
            "cell",
            tuple(
                identity
                for identity, cell in cell_by_id.items()
                if _cell_state(cell) != "never-measured"
            ),
        ),
        test_paths=_query_paths("tests", "test", tuple(test_by_id)),
        history_paths=history_paths,
        selection_run_references=selection_run_references,
        event_run_references=event_run_references,
        wording=_page_wording(wording, validated["provenance"]),
    )


def _breadcrumbs(page: str, values: Sequence[tuple[str, str | None]]) -> str:
    items = []
    for label, target in values:
        if target is None:
            items.append(f'<li aria-current="page">{_escape(label)}</li>')
        else:
            items.append(
                f'<li><a href="{_escape(_href(page, target))}">{_escape(label)}</a></li>'
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
    wording: PageWording,
    scripts: Sequence[str] = ("assets/site.js",),
    *,
    page_kind: str = "static",
    show_page_heading: bool = True,
) -> bytes:
    def versioned_asset(path: str) -> str:
        if path == "assets/site.css":
            contents = _site_css()
        elif path == "assets/site.js":
            contents = _site_javascript()
        else:
            raise RenderRefused(f"layout references an unsupported asset: {path!r}")
        version = hashlib.sha256(contents).hexdigest()[:16]
        return f"{_href(page, path)}?v={version}"

    nav = (
        f'<a href="{_escape(_href(page, "index.html"))}">Overview</a>'
        f'<a href="{_escape(_href(page, "cells/index.html"))}">Cells</a>'
        f'<a href="{_escape(_href(page, "index.html", "tests"))}">Tests</a>'
        f'<a href="{_escape(_href(page, "runs/index.html"))}">Validation runs</a>'
    )
    script_tags = "".join(
        f'<script src="{_escape(versioned_asset(script))}" defer></script>'
        for script in scripts
    )
    feedback = _github_link(
        "https://github.com/rrnewton/hermit/issues",
        "Questions or feedback? Visit Hermit issues",
    )
    banner = (
        f'<p class="fixture-banner">{_escape(wording.banner)}</p>'
        if wording.banner is not None
        else ""
    )
    disclosure = ""
    if wording.disclosure:
        if wording.disclosure_summary is None:
            raise RenderRefused("snapshot provenance disclosure has no summary")
        disclosure = (
            '<details class="snapshot-disclosure"><summary>'
            + _escape(wording.disclosure_summary)
            + "</summary><dl>"
            + "".join(
                (
                    f"<div><dt>{_escape(fact.label)}</dt><dd>"
                    + (
                        _github_link(fact.href, fact.value)
                        if fact.href is not None
                        else _escape(fact.value)
                    )
                    + "</dd></div>"
                )
                for fact in wording.disclosure
            )
            + "</dl></details>"
        )
    page_heading = (
        f'<header class="page-heading"><h1>{_escape(title)}</h1><p>{_escape(lead)}</p></header>'
        if show_page_heading
        else ""
    )
    source = f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="{_escape(wording.meta_description)}">
  <title>{_escape(title)} · {_escape(wording.title_suffix)}</title>
  <link rel="icon" type="image/svg+xml" href="{_escape(_href(page, 'assets/favicon.svg'))}">
  <link rel="stylesheet" href="{_escape(versioned_asset('assets/site.css'))}">
  {script_tags}
</head>
<body data-page-kind="{_escape(page_kind)}" data-site-root="{_escape(_href(page, 'index.html'))}" data-site-manifest="{_escape(_href(page, 'data/site-manifest.json.gz'))}">
  <a class="skip-link" href="#main-content">Skip to main content</a>
  <header class="site-header">
    {banner}
    {disclosure}
    <p class="site-feedback">{feedback}</p>
    <div class="site-navigation"><p class="site-name">Hermit compatibility</p>
    <nav class="primary-navigation" aria-label="Primary navigation">{nav}</nav></div>
  </header>
  {_breadcrumbs(page, breadcrumbs)}
  <main id="main-content" tabindex="-1">
    {page_heading}
    {content}
  </main>
  <footer><p>{_escape(wording.footer)}</p></footer>
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
    visible = f"<strong>{_integer(count)}</strong> {_escape(label)}"
    if represented is not None:
        visible += f" representing <strong>{_integer(represented)}</strong> Hermit runs"
    return (
        f'<a href="{_escape(_href(source, target, fragment))}" data-count="{count}"'
        f"{represented_attribute}>{visible}</a>"
    )


def _cell_state(cell: Mapping[str, Any]) -> str:
    state = cell.get("state")
    if state not in STATE_ORDER:
        raise RenderRefused(
            f"cell {cell.get('cell')!r} has unsupported state {state!r}"
        )
    return str(state)


def _latest_result(cell: Mapping[str, Any]) -> str:
    value = cell.get("latest_result")
    if _cell_state(cell) == "never-measured":
        if value is not None:
            raise RenderRefused("never-measured cell contains a latest result")
        return "Never measured"
    labels = {"pass": "Pass", "product_failure": "Fail"}
    if value not in labels:
        raise RenderRefused(f"unsupported latest result {value!r}")
    return labels[str(value)]


def _cell_link(
    model: SiteModel, page: str, identity: str, *, label: str | None = None
) -> str:
    visible = identity if label is None else label
    path = model.cell_paths.get(identity)
    if path is None:
        return f"<code>{_escape(visible)}</code>"
    return f'<a href="{_escape(_href(page, path))}"><code>{_escape(visible)}</code></a>'


def _test_link(
    model: SiteModel, page: str, identity: str, *, label: str | None = None
) -> str:
    visible = identity if label is None else label
    path = model.test_paths.get(identity)
    if path is None:
        return f"<code>{_escape(visible)}</code>"
    return f'<a href="{_escape(_href(page, path))}"><code>{_escape(visible)}</code></a>'


def _series_filter_evidence(row: Mapping[str, Any]) -> dict[str, Any]:
    capabilities = row["series"].get("host_capabilities")
    if capabilities is None:
        capabilities = {}
    if not isinstance(capabilities, Mapping):
        raise RenderRefused("series host capabilities are malformed")
    values: dict[str, str] = {}
    for name, evidence in capabilities.items():
        if not isinstance(evidence, Mapping) or not isinstance(
            evidence.get("present"), bool
        ):
            raise RenderRefused("series host capability is malformed")
        values[str(name)] = str(evidence["present"]).lower()
    ancestry = row.get("current_main_ancestry")
    if ancestry is not None and not isinstance(ancestry, bool):
        raise RenderRefused("series ancestry value is malformed")
    return {
        "current_main_ancestry": "" if ancestry is None else str(ancestry).lower(),
        "host_capabilities": values,
    }


def _series_filter_facets_json(rows: Sequence[Mapping[str, Any]]) -> str:
    values = {
        _canonical(_series_filter_evidence(row)): _series_filter_evidence(row)
        for row in rows
    }
    return _canonical([values[key] for key in sorted(values)])


def _state_summary(model: SiteModel, page: str) -> str:
    result_denominator = (
        model.data["counts"]["green_cell_count"]
        + model.data["counts"]["red_cell_count"]
    )
    cards = []
    for state in STATE_ORDER:
        count = sum(_cell_state(cell) == state for cell in model.cells)
        denominator = (
            result_denominator if state in {"green", "red"} else len(model.cells)
        )
        target = (
            "cells/never-measured.html"
            if state == "never-measured"
            else "cells/index.html"
        )
        filters = (
            ()
            if state == "never-measured"
            else (("result", "pass" if state == "green" else "product_failure"),)
        )
        cards.append(
            f'<a class="metric state-{state}" href="{_escape(_query_href(page, target, filters, "all-cells"))}" '
            f'data-count="{count}" data-cell-filter-link="1"><strong>{_integer(count)} / {_integer(denominator)} cells</strong>'
            f"<span>{STATE_LABELS[state]}</span></a>"
        )
    return '<div class="metrics">' + "".join(cards) + "</div>"


def _result_reading(model: SiteModel) -> str:
    """State the denominator and the age of the red cells, from the data.

    Without this a reader takes "30 red" against the 5,744 in the manifest and
    understates the failure rate sevenfold, or takes it as 30 things broken right
    now when every one of them is a result retained from an earlier run. Both
    readings are available from the numbers alone, and the wrong one is the more
    natural. The site is read on a phone, where the metric cards arrive long
    before any provenance a reader might otherwise use to calibrate them.

    THE ARITHMETIC IS CHECKED, NOT ASSERTED. An earlier version of this paragraph
    said "the remaining 4895 cells have never been measured", which invited the
    subtraction 5744 - 835 and got 4909. The 14 it swallowed are the deliberate
    projection omissions that the sentence four lines above already names as exact
    arithmetic. A paragraph whose whole purpose is to stop a reader using the
    wrong denominator is the worst possible place to hide one, so every population
    is named and the total is refused if it does not close.
    """
    counts = model.data["counts"]
    in_manifest = counts["in_manifest_cell_count"]
    states = Counter(_cell_state(cell) for cell in model.cells)
    green = states["green"]
    red = states["red"]
    never = states["never-measured"]
    measured = green + red
    undecided = len(model.cells) - measured - never
    omitted = in_manifest - len(model.cells)
    if measured + never + undecided + omitted != in_manifest:
        raise RenderRefused(
            "overview reading note arithmetic does not close: "
            f"{measured} with a result + {never} never measured + {undecided} "
            f"undecided + {omitted} omitted != {in_manifest} in the manifest"
        )
    if undecided < 0 or omitted < 0:
        raise RenderRefused("overview reading note computed a negative population")

    red_cells = [cell for cell in model.cells if _cell_state(cell) == "red"]
    red_selected = sum(1 for cell in red_cells if cell.get("selected_by_full"))
    if red == 0:
        red_sentence = "No cell in this snapshot carries a recorded failure."
    elif red_selected == 0:
        red_sentence = (
            f"None of the {_integer(red)} red cells is selected by full in this snapshot. "
            "These retained failures fall outside this snapshot’s full-profile selection."
        )
    elif red_selected == red:
        red_sentence = f"All {_integer(red)} red cells are selected by full in this snapshot."
    else:
        red_sentence = (
            f"{_integer(red_selected)} of the {_integer(red)} red cells are selected by full in this "
            f"snapshot; the other {_integer(red - red_selected)} are results retained from "
            "earlier runs."
        )

    parts = [
        f"{_integer(measured)} have a recorded result",
        f"{_integer(never)} have never been measured",
    ]
    if undecided:
        parts.append(
            f"{_integer(undecided)} have evidence that does not establish a result"
        )
    parts.append(
        f"{_integer(omitted)} are deliberate omissions with no cell row on this site"
    )
    breakdown = ", ".join(parts[:-1]) + f" and {parts[-1]}"
    return (
        '<p class="result-reading"><strong>How to read these.</strong> '
        f"Green and red are counted over the {_integer(measured)} cells with a recorded "
        "result, "
        f"not over the {_integer(in_manifest)} in the manifest; quoting red against the "
        "manifest would understate the rate. "
        f"{red_sentence} "
        f"Of the {_integer(in_manifest)} cells in the manifest, {breakdown}. "
        "The manifest holds far more cells than any path selects, which is the "
        "intended arrangement and not a backlog.</p>"
    )


def _percentage(value: Fraction) -> str:
    return f"{100.0 * value:.1f}%"


def _integer(value: int) -> str:
    """Format a validated display count without changing its numeric identity."""
    if type(value) is not int:
        raise RenderRefused("display count is not an integer")
    return f"{value:,}"


def _passing_scheduler_turn_length(
    model: SiteModel,
    cell: Mapping[str, Any],
    commit: str,
) -> Fraction | None:
    """Return the complete paired-run length for a passing execution."""
    if _cell_state(cell) != "green":
        return None
    qualifying_event_ids = cell.get("qualifying_event_ids")
    if (
        not isinstance(qualifying_event_ids, Sequence)
        or isinstance(qualifying_event_ids, (str, bytes))
    ):
        return None
    qualifying = {str(event_id) for event_id in qualifying_event_ids}
    candidates = [
        row
        for row in model.series_by_cell.get(str(cell["cell"]), ())
        if str(row["event_id"]) in qualifying
        and row.get("product_result") == "pass"
        and row["series"].get("tree") == commit
    ]
    if not candidates:
        return None
    observed = [
        (datetime.fromisoformat(str(row["emitted_at"]).replace("Z", "+00:00")), row)
        for row in candidates
    ]
    latest_time = max(value[0] for value in observed)
    rows = tuple(row for instant, row in observed if instant == latest_time)
    weighted_total = 0
    weight_total = 0
    for row in rows:
        runtime = row["series"].get("runtime")
        if not isinstance(runtime, Mapping):
            return None
        turns = []
        for run_name in ("run1", "run2"):
            run = runtime.get(run_name)
            value = run.get("scheduler_turns") if isinstance(run, Mapping) else None
            if type(value) is not int or value <= 0:
                return None
            turns.append(value)
        weight = row["series"].get("num_runs")
        if type(weight) is not int or weight <= 0:
            return None
        weighted_total += (turns[0] + turns[1]) * weight
        weight_total += 2 * weight
    return Fraction(weighted_total, weight_total) if weight_total else None


def _ptrace_red_scheduler_turn_length(
    model: SiteModel, cell: Mapping[str, Any]
) -> Fraction | None:
    """Estimate a ptrace red row's complete length from its two successful runs."""
    rows = _latest_series_rows(model, cell)
    if rows is None:
        return None
    weighted_total = 0
    weight_total = 0
    for row in rows:
        if (
            row.get("product_result") != "product_failure"
            or row["series"].get("outcome") != "diverged"
        ):
            return None
        runtime = row["series"].get("runtime")
        if not isinstance(runtime, Mapping):
            return None
        turns = []
        for run_name in ("run1", "run2"):
            run = runtime.get(run_name)
            value = run.get("scheduler_turns") if isinstance(run, Mapping) else None
            if type(value) is not int or value <= 0:
                return None
            turns.append(value)
        weight = row["series"].get("num_runs")
        if type(weight) is not int or weight <= 0:
            return None
        weighted_total += (turns[0] + turns[1]) * weight
        weight_total += 2 * weight
    return Fraction(weighted_total, weight_total) if weight_total else None


def _red_cell_partial_credit(
    model: SiteModel, cell: Mapping[str, Any]
) -> dict[str, Any]:
    """Return partial credit or the precise reason it cannot be calculated."""
    divergence = _latest_divergence(model, cell)
    turn = (
        None
        if divergence is None
        else divergence["coordinates"]["first_divergent_scheduler_turn"]
    )
    estimated = cell["backend"] == "ptrace"
    if turn is None:
        return {
            "estimated": estimated,
            "reason": "the first divergent scheduler turn is not recorded",
            "value": None,
        }
    if estimated:
        length = _ptrace_red_scheduler_turn_length(model, cell)
        if length is None:
            return {
                "estimated": True,
                "reason": "the two recorded runs do not provide scheduler-turn lengths",
                "value": None,
            }
    else:
        ptrace_id = f"{cell['test']}/{cell['mode']}/ptrace"
        ptrace = model.cell_by_id.get(ptrace_id)
        if ptrace is None or _cell_state(ptrace) != "green":
            return {
                "estimated": False,
                "reason": "the matching test and mode are not green on ptrace",
                "value": None,
            }
        length = _passing_scheduler_turn_length(
            model, ptrace, str(cell["latest_result_tree"])
        )
        if length is None:
            return {
                "estimated": False,
                "reason": "the same-commit ptrace-green execution does not provide scheduler-turn lengths",
                "value": None,
            }
    return {
        "estimated": estimated,
        "reason": None,
        "value": min(Fraction(1), Fraction(turn) / length),
    }


def _cell_evidence(row: Mapping[str, Any]) -> Mapping[str, Any]:
    original, derived = row.get("cell_results"), row.get("derived_cell_results")
    if isinstance(original, Mapping) and derived is None:
        return original
    if (original is None and isinstance(derived, Mapping)
            and row.get("schema_version") == 5
            and derived.get("authority") == "writer_verified_raw_census_v1"
            and derived.get("population_scope") == "recorded_only"):
        return derived
    raise RenderRefused("validation row has conflicting or unrecognized cell authority")


def _evidence_path(row: Mapping[str, Any]) -> str:
    return "recorded" if row.get("derived_cell_results") is not None else str(row["profile"])


def _repeat_state(recorded: Mapping[str, Any]) -> str:
    """Interpret only the typed canonical comparison, never an aggregate GREEN."""
    verdict = recorded.get("cell_verdict", {})
    comparison = verdict.get("comparison", {})
    counts = verdict.get("compared_log_messages", {})
    if not isinstance(counts, Mapping) or not isinstance(comparison, Mapping):
        return "unavailable"
    if (
        verdict.get("comparison_tier") != "canonical-bitwise"
        or comparison.get("strictness") != "canonical"
        or comparison.get("log_scope") != "info"
        or comparison.get("compare_io_buffers") is not True
        or comparison.get("exact_remainder") is not True
        or any(type(counts.get(side)) is not int or counts[side] <= 0 for side in ("left", "right"))
    ):
        return "unavailable"
    state = verdict.get("state")
    if state == "compared-and-diverged":
        return "diverged"
    if (
        state == "compared-and-matched"
        and verdict.get("bitwise_parity") is True
        and counts["left"] == counts["right"]
    ):
        # Modern summaries retain prior adverse attempts separately.
        return str(recorded.get("ordinary_comparison", "matched"))
    return "unavailable"


def _cell_metrics(model: SiteModel, cell: Mapping[str, Any]) -> dict[str, Any]:
    rows = _latest_series_rows(model, cell)
    records = [] if rows is None else [
        _exact_validation_cell(model, cell, str(row["event_id"])) for row in rows
    ]
    result: dict[str, Any] = {}
    for name, reader in (("determinism", _repeat_state),):
        states = ["unavailable" if record is None else reader(record) for record in records]
        state = (
            "diverged" if "diverged" in states else
            "matched" if states and all(value == "matched" for value in states) else
            "unavailable"
        )
        partial = None
        if state == "matched":
            partial = Fraction(1)
        elif state == "diverged":
            partial = _red_cell_partial_credit(model, cell)["value"]
        # The outer series coordinate is authenticated against the typed
        # backend-parity witness by the Stage 1 reader. Use it only with the
        # complete same-commit ptrace execution length; a loose coordinate or
        # missing reference still earns no partial credit.
        result[name] = {"state": state, "partial": partial}
    return result


def _backend_rows(model: SiteModel) -> tuple[dict[str, Any], ...]:
    reference = {
        (str(cell["test"]), str(cell["mode"]))
        for cell in model.cells
        if cell["backend"] == "ptrace"
        and _cell_state(cell) in {"green", "red"}
    }
    by_backend: dict[str, dict[tuple[str, str], Mapping[str, Any]]] = {}
    for cell in model.cells:
        backend = str(cell["backend"])
        if backend in {"native", "e9patch"}:
            continue
        key = (str(cell["test"]), str(cell["mode"]))
        previous = by_backend.setdefault(backend, {}).get(key)
        if previous is not None and previous != cell:
            raise RenderRefused(f"conflicting backend-independent cell identity: {backend}/{key}")
        by_backend[backend][key] = cell
    rows = []
    for backend, cells in sorted(by_backend.items()):
        population = [cells.get(key) for key in sorted(reference)]
        metrics = [None if cell is None else _cell_metrics(model, cell) for cell in population]
        item: dict[str, Any] = {
            "backend": backend,
            "denominator": len(reference),
            "excluded": len(set(cells) - reference),
            "failed": sum(cell is not None and _cell_state(cell) == "red" for cell in population),
            "missing": sum(cell is None or _cell_state(cell) == "never-measured" for cell in population),
        }
        for name in ("determinism",):
            values = [None if metric is None else metric[name] for metric in metrics]
            full = sum(value is not None and value["state"] == "matched" for value in values)
            known = sum(value is not None and value["state"] != "unavailable" for value in values)
            partial = sum((value["partial"] for value in values if value is not None and value["partial"] is not None), Fraction())
            item[name] = {
                "full": full,
                "with_partial": partial,
                "unavailable": len(reference) - known,
                "partial_unavailable": sum(value is not None and value["state"] == "diverged" and value["partial"] is None for value in values),
                "percentage": None if not reference or not known else Fraction(full, len(reference)),
                "partial_percentage": None if not reference or not known else partial / len(reference),
            }
        rows.append(item)
    return tuple(sorted(rows, key=lambda row: (
        row["determinism"]["percentage"] is None,
        -(row["determinism"]["percentage"] or Fraction()),
        str(row["backend"]),
    )))


def _backend_snapshot_position(model: SiteModel) -> str:
    observations = []
    for backend in ("kvm", "liteinst"):
        cells = [cell for cell in model.cells if cell["backend"] == backend]
        observations.append(
            f"{backend}: {_integer(sum(_cell_state(cell) == 'green' for cell in cells))} recorded Green, "
            f"{_integer(sum(_cell_state(cell) == 'red' for cell in cells))} recorded failures"
        )
    return ('<details class="backend-position"><summary>Recorded outcomes and comparison evidence</summary><p><strong>KVM and LiteInst in this artifact.</strong> '
            'Recorded outcomes (' + _escape('; '.join(observations))
            + ') remain in Cells. These are positions in this artifact, not changes since an earlier snapshot '
            'and not counts of cross-backend matches. A Green outcome alone does not establish either comparison below.</p></details>')


def _metric_amount(value: Fraction | int) -> str:
    if not isinstance(value, Fraction) or value.denominator == 1:
        return _integer(int(value))
    # Keep weighted partial numerators exact; parentheses distinguish this
    # fraction from the separate common-population denominator.
    return f"({_integer(value.numerator)}/{_integer(value.denominator)})"


def _backend_leaderboard(model: SiteModel) -> str:
    rows = []
    for item in _backend_rows(model):
        columns = []
        metric = item["determinism"]
        for amount, percentage in (("full", "percentage"), ("with_partial", "partial_percentage")):
            value = metric[percentage]
            label = "Not available" if value is None else _percentage(value)
            columns.append(
                f'<td><strong>{label}</strong><span class="secondary">'
                f'{_metric_amount(metric[amount])} / {_integer(item["denominator"])} cells</span></td>'
            )
        details = (
            f'{_integer(item["failed"])} failed; {_integer(item["missing"])} missing/unmeasured; '
            f'{_integer(item["determinism"]["unavailable"])} without determinism evidence; '
            f'{_integer(item["excluded"])} outside the reference population'
        )
        rows.append(f'<tr><th scope="row"><code>{_escape(item["backend"])}</code></th>'
                    + ''.join(columns) + f'<td>{_escape(details)}</td></tr>')
    headings = ''.join(_column_heading(title, help_text) for title, help_text in (
        ("Backend", "Recorded instrumentation backend; native and e9patch preprocessing are excluded."),
        ("Determinism", "Canonical repeat matches / all distinct test-and-mode cells with a recorded ptrace result."),
        ("Det. w/partial", "Full matches plus recorded qualifying partial fractions / the same common denominator."),
        ("Evidence coverage", "Determinism failures and missing/unexecuted cells stay in the determinism denominator. Missing comparison evidence earns no credit."),
    ))
    return (
        '<section id="backends"><h2>Backends</h2>' + _backend_snapshot_position(model)
        + '<p>Every determinism percentage uses the same distinct test-and-mode population with a recorded ptrace result, including ptrace failures. '
        'The <strong>w/partial</strong> column adds qualifying partial fractions to full matches. Missing and failed cells stay in the denominator. '
        'Parity is not part of this table: <a href="#parity">Parity (measured after determinism)</a>, below it, is the only parity display.</p>'
        '<details><summary>How comparison evidence earns credit</summary><p>'
        'Numerators count demonstrated comparisons, not generic Green labels. '
        'An absent or empty reference population is unavailable. Partial determinism uses the recorded first divergent scheduler turn '
        'and a complete same-commit ptrace execution; no missing measurement is inferred as partial. '
        'A column with no supported observations is unavailable.</p></details>'
        '<div class="table-scroll"><table class="backend-leaderboard"><caption>Backend comparison coverage</caption>'
        f'<thead><tr>{headings}</tr></thead><tbody>{"".join(rows)}</tbody></table></div></section>'
    )


def _backend_history(model: SiteModel) -> list[dict[str, Any]]:
    return model.backend_history


BACKEND_HISTORY_COLORS = (
    "#2277aa",
    "#c45c00",
    "#8a63b8",
    "#008275",
    "#bd4f4f",
    "#777777",
)


#: (site-summary.json backend_history key, `_backend_rows` metric) pairs.
BACKEND_HISTORY_SERIES = (("determinism", "determinism"),)


def _compute_backend_history(model: SiteModel) -> list[dict[str, Any]]:
    """Replay the existing reducer as of each recorded UTC date, not ever-green."""
    reducer = _writer()._build_data_module()._modules().reducer
    rows = [reducer._normalise_series_row(row, index) for index, row in enumerate(model.series_rows)]
    by_cell: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        by_cell.setdefault(row["cell"], []).append(row)
    days = sorted({row["emitted_at"][0].astimezone(timezone.utc).date().isoformat() for row in rows})
    catalogue = []
    for identity in sorted(model.manifest_cell_ids):
        test, mode, backend = identity.rsplit("/", 2)
        metadata = model.test_by_id[test]
        catalogue.append({
            "selected_by": model.cell_by_id.get(identity, {}).get("selected_by", {}),
            "cell": identity, "test": test, "mode": mode, "backend": backend,
            "category": metadata["category"], "lane": metadata["lane"],
        })
    history = []
    for day in days:
        end = datetime.fromisoformat(day).replace(tzinfo=timezone.utc) + timedelta(days=1)
        cells = []
        past_by_cell = {
            identity: tuple(row for row in values if datetime.fromisoformat(str(row["emitted_at"]).replace("Z", "+00:00")) < end)
            for identity, values in model.series_by_cell.items()
        }
        for cell in catalogue:
            population = [row for row in by_cell.get(cell["cell"], ()) if row["emitted_at"][0] < end]
            reduced, omission = reducer._reduce_cell(
                cell, population, model.runs["current_main_ancestry"],
                model.runs["validate_run_selection"],
                {cell["cell"]: cell.get("selected_by", {})},
            )
            if reduced is None:
                # A later no-result/ambiguous execution must not carry an older
                # green value forward. Keep this population member unavailable.
                reduced = {**cell, "state": "never-measured", "latest_result": None,
                           "latest_event_ids": [], "history_unavailable": omission["reason"]}
            cells.append(reduced)
        snapshot = replace(model, cells=tuple(cells),
                           cell_by_id={cell["cell"]: cell for cell in cells},
                           series_by_cell=past_by_cell)
        for item in _backend_rows(snapshot):
            history.append({
                "date": day, "backend": item["backend"], "reference_cells": item["denominator"],
                **{key: {
                    "green_cells": None if item[name]["percentage"] is None else item[name]["full"],
                    "unavailable_cells": item[name]["unavailable"],
                } for key, name in BACKEND_HISTORY_SERIES},
            })
    return history


def _backend_history_chart(model: SiteModel) -> str:
    history = _backend_history(model)
    days = sorted({point["date"] for point in history})
    if not days:
        return '<section id="backend-history"><h2>Backend history</h2><p>No recorded comparison history is available.</p></section>'
    times = {day: datetime.fromisoformat(day).replace(tzinfo=timezone.utc).timestamp() for day in days}
    first, last = times[days[0]], times[days[-1]]
    width, height, left, top = 900, 320, 65, 25

    def x(day: str) -> float:
        return left + (times[day] - first) * 800 / max(86400, last - first)

    def percentage(point: Mapping[str, Any], name: str) -> Fraction | None:
        count = point[name]["green_cells"]
        reference = point["reference_cells"]
        if count is None or reference == 0:
            return None
        return Fraction(count, reference)

    def y(value: Fraction) -> float:
        return top + 240 * (1 - float(value))

    lines, legend = [], []
    markers: dict[tuple[float, float], list[dict[str, str]]] = {}
    for index, backend in enumerate(sorted({point["backend"] for point in history})):
        points = [point for point in history if point["backend"] == backend]
        color = BACKEND_HISTORY_COLORS[index % len(BACKEND_HISTORY_COLORS)]
        for name, _source in BACKEND_HISTORY_SERIES:
            commands = []
            connected = False
            for point in points:
                count = point[name]["green_cells"]
                value = percentage(point, name)
                if value is None:
                    connected = False
                    continue
                px, py = x(point["date"]), y(value)
                commands.append(f'H {px:.2f} V {py:.2f}' if connected else f'M {px:.2f} {py:.2f}')
                series_id = f"history-{index}-{name}"
                markers.setdefault((px, py), []).append({
                    "series": series_id,
                    "label": (
                        f'{backend} {name}, {point["date"]}: {_percentage(value)} '
                        f'({_integer(count)} / {_integer(point["reference_cells"])} recorded ptrace-result cells)'
                    ),
                })
                connected = True
            series_id = f"history-{index}-{name}"
            if commands:
                lines.append(
                    f'<path data-history-series="{series_id}" d="{" ".join(commands)}" '
                    f'fill="none" stroke="{color}" stroke-width="2"/>'
                )
                legend.append(
                    '<label class="history-series-toggle">'
                    f'<input type="checkbox" checked disabled data-history-series-toggle="{series_id}" '
                    f'aria-label="Show {_escape(backend)} {name} line">'
                    f'<svg width="26" height="12" aria-hidden="true"><path d="M 0 6 H 26" '
                    f'stroke="{color}" stroke-width="2"/></svg> '
                    f'<span><code>{_escape(backend)}</code> {name}</span></label>'
                )
            else:
                legend.append(
                    '<span class="history-series-unavailable">'
                    f'<svg width="26" height="12" aria-hidden="true"><path d="M 0 6 H 26" '
                    f'stroke="{color}" stroke-width="2"/></svg> '
                    f'<span><code>{_escape(backend)}</code> {name} (no data)</span></span>'
                )
    # Draw hit targets after every line. Co-located observations share a title,
    # so one backend's stroke or dot cannot hide another backend's details.
    for marker_index, ((px, py), values) in enumerate(markers.items()):
        label = '; '.join(value["label"] for value in values)
        lines.append(
            f'<circle class="history-point" data-history-point="{marker_index}" '
            f'data-history-point-values="{_escape(_canonical(values))}" '
            f'cx="{px:.2f}" cy="{py:.2f}" r="5" tabindex="0" role="img" '
            f'aria-label="{_escape(label)}"><title>{_escape(label)}</title></circle>'
        )
    anchor = model.data["provenance"].get("canonical_input_provenance", {})
    commit = anchor.get("series_commit", anchor.get("parent_commit", model.data["provenance"].get("parent_commit")))
    table_rows = []
    for point in history:
        values = []
        for name, _source in BACKEND_HISTORY_SERIES:
            count = point[name]["green_cells"]
            value = percentage(point, name)
            values.extend((
                "Not available" if count is None else _integer(count),
                "Not available" if value is None else _percentage(value),
            ))
        table_rows.append(
            f'<tr><td>{point["date"]}</td><td>{_escape(point["backend"])}</td>'
            + ''.join(f'<td>{value}</td>' for value in values)
            + f'<td>{_integer(point["reference_cells"])}</td></tr>'
        )
    return (
        '<section id="backend-history" data-history-chart><h2>Backend history</h2>'
        '<p>Latest recorded state at each UTC date for the current catalogue and current main ancestry, within that date’s recorded ptrace-result population. '
        'Percentages and counts can fall after regressions. Steps carry recorded state; they are not interpolated measurements. '
        'Gaps mean no supported comparison evidence, including legacy periods. The lines are determinism; parity is shown only in '
        '<a href="#parity">Parity (measured after determinism)</a>.</p>'
        '<p class="history-instructions"><strong>Use the chart:</strong> Hover over or focus a point to see its exact date and percentage. '
        'Clear or select a checkbox under <em>Visible lines</em> to hide or show that series.</p>'
        f'<div class="chart-scroll"><svg class="history-chart" viewBox="0 0 {width} {height}" role="group" aria-label="Recorded green-cell percentages over time, by backend and comparison">'
        f'<path d="M {left} {top} V 265 H 875" fill="none" stroke="currentColor"/>'
        f'<text x="{left}" y="18">Green percentage</text><text x="20" y="{top + 5}">100%</text><text x="35" y="270">0%</text>'
        f'<text x="{left}" y="294">{days[0]}</text><text x="875" y="294" text-anchor="end">{days[-1]} UTC</text>'
        + ''.join(lines) + '</svg></div><fieldset class="history-legend"><legend>Visible lines</legend>' + ''.join(legend) + '</fieldset>'
        '<output class="history-tooltip" data-history-tooltip role="status" aria-live="polite">Hover over or focus a point to see its exact date and percentage.</output>'
        '<noscript><p>The chart remains visible without JavaScript; exact values are also listed in the table below.</p></noscript>'
        f'<p>Ledger anchor: <code>{_escape(commit)}</code>. Latest recorded date: {days[-1]}. '
        '<a href="data/site-summary.json.gz">Machine-readable history and source provenance</a>.</p>'
        '<details><summary>Recorded values by date</summary><div class="table-scroll"><table><thead><tr><th>Date (UTC)</th><th>Backend</th><th>Determinism green</th><th>Determinism percentage</th><th>Ptrace reference</th></tr></thead><tbody>'
        + ''.join(table_rows) + '</tbody></table></div></details></section>'
    )


# The parity panel's backend order is the scorecard section's: the candidate
# backends by population, then dbt.
PARITY_BACKEND_ORDER = ("kvm", "liteinst", "sabre", "dbt")
PARITY_PANEL_TITLE = "Parity (measured after determinism)"


def _parity_reader() -> Any:
    """The build's parity summary checker (the captured one in a combined build)."""
    return _writer()._build_data_module()._parity_summary()


def _parity_view(model: Any) -> tuple[Mapping[str, Any] | None, Mapping[str, Any] | None]:
    """The build's parity carriage and its checked summary.

    `(None, None)` is a build without a parity capture (historical, or built
    before parity was captured). Otherwise the carriage is admitted against the
    build's series ledger commit: its summary is returned only when it is
    current or stale. A carriage the checker refuses is replaced by a refused
    carriage naming why, which only the parity panel shows.
    """
    carriage = model.runs.get("parity")
    if carriage is None:
        return None, None
    provenance = model.data.get("provenance")
    canonical = provenance.get("canonical_input_provenance") if isinstance(provenance, Mapping) else None
    series_commit = canonical.get("series_commit") if isinstance(canonical, Mapping) else None
    return _parity_reader().admit_carriage(carriage, series_commit, "parity carriage")


def _parity_credit(value: Any) -> str:
    """A credit to four decimals; a credit below 1 never prints as 1."""
    if value is None:
        return "n/a"
    text = f"{value:.4f}"
    if value < 1.0 and text == "1.0000":
        return "0.9999"
    return text


def _short_sha(value: Any) -> str:
    if value is None:
        return "not recorded"
    return f'<code title="{_escape(value)}">{_escape(str(value)[:12])}</code>'


def _parity_message(value: Any) -> str:
    if value is None:
        return '<span class="secondary">none</span>'
    return f'<code class="parity-message">{_escape(value)}</code>'


def _parity_optional(value: Any) -> str:
    return "none" if value is None else _escape(value)


def _parity_backends(run: Mapping[str, Any]) -> list[str]:
    known = [backend for backend in PARITY_BACKEND_ORDER if backend in run["per_backend"]]
    return known + sorted(set(run["per_backend"]) - set(known))


def _parity_absent_text(carriage: Mapping[str, Any] | None) -> str:
    reader = _parity_reader()
    if carriage is None:
        return (
            f'<p><code>{_escape(reader.ABSENT_LINE)}</code></p>'
            "<p>This build carries no parity capture: it is a historical build, or it was built "
            "before the website captured the ledger's parity summary. Nothing here is zero parity.</p>"
        )
    commit = _escape(carriage["ledger_commit"])
    if carriage["store_files"] == 0:
        return (
            f'<p><code>{_escape(reader.ABSENT_LINE)}</code></p>'
            f"<p>Ledger commit <code>{commit}</code> holds no parity records "
            f"({_integer(carriage['run_marker_files'])} parity run marker file(s)). "
            "Nothing here is zero parity.</p>"
        )
    return (
        f'<p><code>{_escape(reader.ABSENT_LINE)}</code></p>'
        f'<p class="notice"><strong>Ledger commit <code>{commit}</code> holds '
        f"{_integer(carriage['store_files'])} parity record shard file(s) and "
        f"{_integer(carriage['run_marker_files'])} run marker file(s), but no "
        f"<code>{_escape(carriage['path'])}</code>: its parity records were published and never "
        "summarized.</strong> Nothing here is zero parity.</p>"
    )


def _parity_refused_text(carriage: Mapping[str, Any]) -> str:
    facts = []
    if carriage["ledger_commit"] is not None:
        facts.append(f'ledger commit <code>{_escape(carriage["ledger_commit"])}</code>')
    if carriage["store_files"] is not None:
        facts.append(f'{_integer(carriage["store_files"])} parity record shard file(s)')
    if carriage["store_records"] is not None:
        facts.append(f'{_integer(carriage["store_records"])} record line(s)')
    if carriage["sha256"] is not None:
        facts.append(f'<code>{_escape(carriage["path"])}</code> sha256 {_short_sha(carriage["sha256"])}')
    return (
        '<div class="notice parity-refused"><p><strong>Parity summary refused; no parity figure is shown for this '
        "build.</strong> The determinism figures are unaffected.</p>"
        f'<p><code class="parity-message">{_escape(carriage["refusal"])}</code></p>'
        + (f'<p>Read at: {"; ".join(facts)}.</p>' if facts else "")
        + "</div>"
    )


def _parity_append_refusals(carriage: Mapping[str, Any] | None) -> str:
    if carriage is None or not carriage["append_refusals_total"]:
        return ""
    listed = carriage["append_refusals"]
    return (
        f'<div class="notice parity-append-refusals"><p><strong>{_integer(carriage["append_refusals_total"])} '
        "parity run(s) recorded no parity rows: <code>append-parity</code> refused their records and they were "
        f"not later recorded ({_integer(len(listed))} newest listed).</strong> These runs are not in any figure "
        "below.</p><ul>"
        + "".join(
            f'<li>{_escape(item["producer"])} run <code>{_escape(item["run_id"])}</code> at Hermit '
            f'{_short_sha(item["hermit_sha"])}, {_escape(item["emitted_at"])}: '
            f'<code class="parity-message">{_escape(item["error"])}</code></li>'
            for item in listed
        )
        + "</ul></div>"
    )


def _parity_run_section(run: Mapping[str, Any], carriage: Mapping[str, Any], summary: Mapping[str, Any]) -> str:
    total = run["total"]
    generated = summary["generated_from"]
    as_of = (
        f'<p class="parity-as-of">Data as of: last parity record emitted {_time_element(run["emitted_at"], "parity emitted_at")}; '
        f'Hermit commit {_short_sha(run["hermit_sha"])} measured; '
        + (
            f'parity store at ledger commit {_short_sha(carriage["summarized_commit"])} (stale: '
            f'{_integer(carriage["unsummarized_records"])} newer record line(s) at ledger commit '
            f'{_short_sha(carriage["ledger_commit"])} not summarized) '
            if carriage["state"] == "stale" else
            f'parity store at ledger commit {_short_sha(carriage["ledger_commit"])} '
        )
        + f'(<code>{_escape(carriage["path"])}</code>, sha256 {_short_sha(carriage["sha256"])}); '
        f'summary written by Hermit tool commit {_short_sha(generated["tool_hermit_commit"])}.</p>'
    )
    if total["credited"]:
        mean_basis = (
            f'Mean credit {_parity_credit(total["mean_credit"])} is the credit sum '
            f'{total["credit_sum"]:.4f} over the {_integer(total["credited"])} credited (measured) cell(s)'
        )
    else:
        mean_basis = "Mean credit is n/a: no selected cell was credited"
    floor_basis = (
        f'floor credit {_parity_credit(total["floor_credit"])} is the same sum over all '
        f'{_integer(total["selected"])} selected cell(s)'
        if total["selected"] else "floor credit is n/a: no cell was selected"
    )
    uncredited = total["selected"] - total["credited"]
    basis = (
        f"<p>{mean_basis}; {floor_basis}. The {_integer(uncredited)} selected cell(s) without a credit "
        "(unavailable, record-missing or refused) are left out of the mean and are never averaged as 0 or 1.</p>"
    )
    headings = "".join(
        _column_heading(title, help_text)
        for title, help_text in (
            ("Candidate backend", "The backend whose log is compared with the ptrace reference log."),
            ("Selected", "Cells the parity selection named for this run: measured + unavailable + record-missing + refused."),
            ("Measured", "Cells whose two logs were compared: matched + diverged."),
            ("Matched", "Cells whose candidate log matched the reference log completely (credit 1)."),
            ("Diverged", "Cells whose logs differ; credit is the matched prefix over the longer log, below 1."),
            ("Not compared (unavailable)", "Cells that could not be compared; the reasons are in the table below."),
            ("Record-missing", "Selected cells for which the run wrote no parity record."),
            ("Refused", "Records the scorecard could attribute to this run but not admit."),
            ("Mean credit (measured)", "Credit sum over the credited (measured) cells; n/a when none."),
            ("Credited", "The mean credit's denominator."),
            ("Floor credit (selected)", "Credit sum over every selected cell."),
        )
    )
    rows = []
    for backend, tally in [*((b, run["per_backend"][b]) for b in _parity_backends(run)), ("TOTAL", total)]:
        label = "TOTAL" if backend == "TOTAL" else f"<code>{_escape(backend)}</code>"
        rows.append(
            f'<tr><th scope="row">{label}</th>'
            f'<td>{_integer(tally["selected"])}</td><td>{_integer(tally["measured"])}</td>'
            f'<td>{_integer(tally["matched"])}</td><td>{_integer(tally["diverged"])}</td>'
            f'<td>{_integer(tally["unavailable"])}</td><td>{_integer(tally["record_missing"])}</td>'
            f'<td>{_integer(tally["refused"])}</td><td>{_parity_credit(tally["mean_credit"])}</td>'
            f'<td>{_integer(tally["credited"])}</td><td>{_parity_credit(tally["floor_credit"])}</td></tr>'
        )
    table = (
        '<div class="table-scroll"><table class="parity-summary">'
        f'<caption>Parity of {_escape(run["producer"])} run {_escape(run["run_id"])}</caption>'
        f'<thead><tr>{headings}</tr></thead><tbody>{"".join(rows)}</tbody></table></div>'
    )
    reasons = [name for name in total["unavailable_by_class"] if total["unavailable_by_class"][name]]
    backends = _parity_backends(run)
    if reasons:
        reason_rows = "".join(
            f'<tr><th scope="row"><code>{_escape(name)}</code></th>'
            + "".join(f'<td>{_integer(run["per_backend"][b]["unavailable_by_class"][name])}</td>' for b in backends)
            + f'<td>{_integer(total["unavailable_by_class"][name])}</td></tr>'
            for name in reasons
        )
        reason_table = (
            '<div class="table-scroll"><table class="parity-unavailable"><caption>Cells not compared (unavailable), by reason</caption>'
            '<thead><tr><th scope="col">Reason</th>'
            + "".join(f'<th scope="col"><code>{_escape(b)}</code></th>' for b in backends)
            + f'<th scope="col">TOTAL</th></tr></thead><tbody>{reason_rows}</tbody></table></div>'
        )
    else:
        reason_table = "<p>No selected cell was unavailable.</p>"
    divergence_rows = []
    for backend in backends:
        group = run["per_backend"][backend]["first_divergence"]
        if group is None:
            continue
        divergence_rows.append(
            f'<tr><th scope="row"><code>{_escape(backend)}</code></th>'
            f'<td>{_parity_optional(group["first_divergent_record"])}</td>'
            f'<td>{_parity_optional(group["syscall"])}</td>'
            f'<td>{_parity_message(group["field"])}</td>'
            f'<td>{_integer(group["cells"])} of {_integer(group["of_diverged"])}</td>'
            f'<td><code>{_escape(group["example_cell"])}</code></td>'
            f'<td>{_parity_message(group["reference_message"])}</td>'
            f'<td>{_parity_message(group["candidate_message"])}</td></tr>'
        )
    if divergence_rows:
        divergence_table = (
            '<div class="table-scroll"><table class="parity-first-divergence">'
            "<caption>Most common first divergence of each backend</caption>"
            '<thead><tr><th scope="col">Backend</th><th scope="col">Record</th><th scope="col">Syscall</th>'
            '<th scope="col">Differing field</th><th scope="col">Diverged cells sharing it</th>'
            '<th scope="col">Example cell</th><th scope="col">Reference (left) message</th>'
            '<th scope="col">Candidate (right) message</th></tr></thead>'
            f'<tbody>{"".join(divergence_rows)}</tbody></table></div>'
        )
    else:
        divergence_table = "<p>No measured cell diverged.</p>"
    unmatched = [cell for cell in run["cells"] if cell["verdict"] != "matched"]
    cell_rows = "".join(
        f'<tr><th scope="row"><code>{_escape(cell["cell"])}</code></th>'
        f'<td>{_escape(cell["verdict"])}{"" if cell["unavailable_class"] in (None, cell["verdict"]) else " (" + _escape(cell["unavailable_class"]) + ")"}</td>'
        f'<td>{_parity_credit(cell["credit"])}{"" if cell["credit_basis"] != "unequalized" else " (inputs not equalized)"}</td>'
        f'<td>{_parity_optional(cell["first_divergent_record"])}</td>'
        f'<td>{_parity_optional(cell["first_difference_syscall"])}</td>'
        f'<td>{_parity_message(cell["first_difference_field"])}</td>'
        f'<td>{_parity_message(cell["reference_message"])}</td>'
        f'<td>{_parity_message(cell["candidate_message"])}</td>'
        f'<td>{_parity_message(cell["reason"])}</td></tr>'
        for cell in unmatched
    )
    diverged = sum(cell["verdict"] == "diverged" for cell in unmatched)
    cell_table = (
        f"<details><summary>First divergence of each of the {_integer(diverged)} diverged cell(s), and every other "
        f"unmatched cell ({_integer(len(unmatched))} in all)</summary>"
        '<div class="table-scroll"><table class="parity-cells"><caption>Unmatched parity cells</caption>'
        '<thead><tr><th scope="col">Cell</th><th scope="col">Verdict</th><th scope="col">Credit</th>'
        '<th scope="col">Record</th><th scope="col">Syscall</th><th scope="col">Differing field</th>'
        '<th scope="col">Reference (left) message</th><th scope="col">Candidate (right) message</th>'
        f'<th scope="col">Reason</th></tr></thead><tbody>{cell_rows}</tbody></table></div></details>'
        if unmatched else "<p>Every selected cell matched.</p>"
    )
    return (
        f'<h3>{_escape(run["producer"])} run <code>{_escape(run["run_id"])}</code></h3>'
        + as_of
        + f'<p class="parity-line"><code>{_escape(run["line"])}</code></p>'
        + basis + table + reason_table + divergence_table + cell_table
    )


def _parity_legacy_section(summary: Mapping[str, Any]) -> str:
    legacy = summary["legacy_rerun"]
    dropped = summary["legacy_dropped"]
    rows = "".join(
        f'<tr><th scope="row"><code>{_escape(entry["cell"])}</code></th>'
        f'<td>{_escape(entry["verdict"])}</td><td>{_integer(entry["compared_records"])}</td>'
        f'<td>{_parity_optional(entry["first_divergent_record"])}</td>'
        f'<td>{_short_sha(entry["hermit_sha"])}</td>'
        f'<td>{_parity_optional(entry["current_parity_verdict"])}</td></tr>'
        for entry in legacy["entries"]
    )
    table = (
        '<div class="table-scroll"><table class="parity-legacy"><caption>legacy-rerun entries</caption>'
        '<thead><tr><th scope="col">Cell</th><th scope="col">legacy-rerun verdict</th>'
        '<th scope="col">Compared records</th><th scope="col">First divergent record</th>'
        '<th scope="col">Hermit commit</th><th scope="col">Current parity verdict</th></tr></thead>'
        f"<tbody>{rows}</tbody></table></div>"
        if legacy["entries"] else ""
    )
    dropped_text = (
        "<p>Legacy observations dropped rather than shown as verdicts: "
        + "; ".join(f"{_escape(reason)} {_integer(count)}" for reason, count in sorted(dropped.items()))
        + ".</p>"
        if dropped else ""
    )
    return (
        f'<details class="parity-legacy-rerun"><summary>{_escape(legacy["label"])} history: the retired ptrace rerun, '
        "never part of the parity above</summary>"
        f'<p><code>{_escape(legacy["line"])}</code></p>'
        f'{table}{dropped_text}</details>'
    )


def _parity_panel(model: Any) -> str:
    carriage, summary = _parity_view(model)
    intro = (
        f'<section id="parity"><h2>{PARITY_PANEL_TITLE}</h2>'
        "<p>After the determinism checks, each candidate backend's retained verify log is compared with the "
        "ptrace reference log. A cell's credit is the matched prefix of the two logs over the longer one, in "
        "[0, 1]; only a complete match earns 1. This panel shows the ledger's committed parity summary after "
        "checking it against the parity records it summarizes; it is separate from determinism and recomputes "
        "no credit.</p>"
    )
    if summary is None:
        body = _parity_refused_text(carriage) if carriage is not None and carriage["state"] == "refused" \
            else _parity_absent_text(carriage)
        return intro + body + _parity_append_refusals(carriage) + "</section>"
    parts = [intro]
    if carriage["state"] == "stale":
        parts.append(
            f'<p class="notice parity-stale"><strong>Stale: this summary covers the parity store at ledger commit '
            f'<code>{_escape(carriage["summarized_commit"])}</code>; '
            f'{_integer(carriage["unsummarized_records"])} newer parity record line(s) at ledger commit '
            f'<code>{_escape(carriage["ledger_commit"])}</code> are not summarized.</strong> The figures below '
            f'are as of <code>{_escape(carriage["summarized_commit"])}</code>. A parity-only publication appends '
            "records without rewriting the summary; the next scorecard publication summarizes them.</p>"
        )
    parts.append(_parity_append_refusals(carriage))
    if not summary["producers"]:
        parts.append(
            f"<p>The ledger's parity store holds no admitted parity run "
            f"({_integer(summary['rows_read'])} row(s) read).</p>"
        )
    for run in summary["producers"]:
        parts.append(_parity_run_section(run, carriage, summary))
    others = [brief for brief in summary["runs"] if not brief["headline"]]
    if others:
        parts.append(
            f"<details><summary>{_integer(len(others))} earlier parity run(s) in the store</summary><ul>"
            + "".join(
                f'<li>{_escape(brief["producer"])} run <code>{_escape(brief["run_id"])}</code> at Hermit '
                f'{_short_sha(brief["hermit_sha"])}: <code>{_escape(brief["line"])}</code></li>'
                for brief in others
            )
            + "</ul></details>"
        )
    if summary["duplicate_rows"]:
        parts.append(
            f"<p>{_integer(summary['duplicate_rows'])} repeated report(s) of a cell within one run were resolved "
            "to the most adverse report.</p>"
        )
    if summary["refusals"]:
        parts.append(
            f'<div class="notice"><p><strong>The scorecard refused {_integer(summary["refused_rows"])} parity '
            f'row(s) ({_integer(len(summary["refusals"]))} refusal(s) listed).</strong> A refused row that names its '
            "run and cell stays in that run's selected count as refused.</p><ul>"
            + "".join(
                f'<li><code>{_escape(refusal["path"])}{"" if refusal["line"] is None else ":" + str(refusal["line"])}</code>: '
                f'{_escape(refusal["message"])}</li>'
                for refusal in summary["refusals"][:LIST_PAGE_SIZE]
            )
            + "</ul></div>"
        )
    parts.append(_parity_legacy_section(summary))
    parts.append("</section>")
    return "".join(parts)


def _overview(model: SiteModel, page: str) -> str:
    counts = model.data["counts"]
    runs = model.runs
    omitted = counts["in_manifest_cell_count"] - len(model.cells)
    result_count = counts["green_cell_count"] + counts["red_cell_count"]
    return f"""
    <section id="overview">
      <h2>Overview</h2>
      <div class="metrics compact-metrics">
        <div class="metric"><strong>{_integer(counts['in_manifest_cell_count'])}</strong><span>cells in the manifest</span></div>
        <a class="metric" href="{_escape(_href(page, 'cells/index.html', 'all-cells'))}"><strong>{_integer(result_count)}</strong><span>result rows shown by default</span></a>
        <a class="metric" href="{_escape(_href(page, 'cells/never-measured.html', 'all-cells'))}"><strong>{_integer(counts['never_measured_cell_count'])}</strong><span>never measured cells in the explicit view</span></a>
        <a class="metric" href="{_escape(_href(page, 'cells/projection-audit.html', 'deliberate-omissions'))}"><strong>{_integer(omitted)}</strong><span>in the manifest minus rows with a cell page</span></a>
        <div class="metric"><strong>{_integer(result_count)}</strong><span>green plus red result denominator</span></div>
      </div>
      <p>The difference between cells in the manifest and rows with a cell page is retained as exact arithmetic. It is not assigned a cell result and has no cell row on this site.</p>
      {_state_summary(model, page)}
      {_result_reading(model)}
      {_backend_leaderboard(model)}
      {_parity_panel(model)}
    {_backend_history_chart(model)}
      <p class="evidence-counts">
        {_count_link(page, 'cells/index.html', 'series-rows', runs['series_physical_row_count'], 'admitted canonical series rows', runs['series_represented_run_count'])}.
        {_count_link(page, 'runs/index.html', 'validation-runs', len(model.history_rows), 'admitted validation ledger rows')}.
        {_count_link(page, 'cells/projection-audit.html', 'series-excluded', runs['projection_excluded_physical_row_count'], 'excluded physical series rows', runs['projection_excluded_represented_run_count'])}.
        {_count_link(page, 'tests/custom-commands.html', 'custom-commands', len(model.custom_commands), 'custom commands selected by full')}.
        The captured input contained {_integer(runs['projection_admitted_physical_row_count'] + runs['projection_excluded_physical_row_count'])} physical series rows representing {_integer(runs['projection_admitted_represented_run_count'] + runs['projection_excluded_represented_run_count'])} Hermit runs.
      </p>
    </section>"""


def _deliberate_omission_audit(model: SiteModel, page: str) -> str:
    omissions = tuple(model.data["projection_audit"]["deliberate_omissions"])
    physical = sum(int(item["physical_row_count"]) for item in omissions)
    represented = sum(int(item["represented_run_count"]) for item in omissions)
    rows = []
    for item in omissions[:LIST_PAGE_SIZE]:
        test = model.test_by_id[str(item["test"])]
        event_references = tuple(item["event_references"])
        visible_references = " ".join(
            f'<span id="{_fragment("event", str(reference["event_id"]))}">'
            f"Recorded evidence {_integer(index)}</span>"
            for index, reference in enumerate(event_references, start=1)
        )
        identifiers = "<br>".join(
            f'<code class="audit-identifier">{_escape(reference["event_id"])}</code>'
            for reference in event_references
        )
        references = (
            f"{visible_references}<details><summary>{_integer(len(event_references))} exact event "
            f'{"reference" if len(event_references) == 1 else "references"}</summary>'
            f"{identifiers}</details>"
        )
        rows.append(
            f'<tr id="{_fragment("omission", str(item["cell"]))}"><th scope="row"><code>{_escape(item["cell"])}</code></th>'
            f"<td>{_test_link(model, page, str(item['test']), label=_test_name(item['test'], test['category']))}</td>"
            f"<td>{_escape(_omission_summary(model, item))}</td>"
            f"<td>{_integer(item['physical_row_count'])}</td>"
            f"<td>{_integer(item['represented_run_count'])}</td>"
            f"<td>{references}</td></tr>"
        )
    headings = "".join(
        (
            _column_heading(
                "Cell",
                "The complete test, mode, and backend identity of the omitted manifest cell.",
            ),
            _column_heading(
                "TestName",
                "The test identifier with its Category prefix omitted.",
            ),
            _column_heading(
                "Reason",
                "The recorded reason this cell was deliberately omitted from published result rows.",
            ),
            _column_heading(
                "Physical rows",
                "The number of admitted stress-series/v3 observation rows retained for this cell; one row can represent several exactly equal Hermit runs.",
            ),
            _column_heading(
                "Represented Hermit runs",
                "The sum of num_runs across those rows. Explicit retry attempts stay separate; exactly equal repeated observations may share one row.",
            ),
            _column_heading(
                "Exact event references",
                "The ledger event identifiers for the rows behind this omission decision.",
            ),
        )
    )
    body = (
        '<div class="table-scroll"><table><caption>Deliberate omissions from the '
        f'published cell projection.</caption><thead><tr>{headings}</tr></thead><tbody>{"".join(rows)}</tbody></table></div>'
        if rows
        else "<p>No deliberate projection omission was recorded.</p>"
    )
    return f"""
    <section id="deliberate-omissions">
      <h2>Deliberate projection omissions</h2>
      <p>{_integer(len(omissions))} manifest cell identities are omitted from result rows by the recorded reducer decision: {_integer(physical)} physical rows representing {_integer(represented)} Hermit runs. Published cells plus these omissions equal the manifest population.</p>
      {body}
    </section>"""


def _counted(count: int, singular: str, plural: str | None = None) -> str:
    return f"{_integer(count)} {singular if count == 1 else (plural or singular + 's')}"


def _readable_recorded_term(value: Any) -> str:
    return "not recorded" if value is None else str(value).replace("_", " ")


def _joined_terms(values: Sequence[Any]) -> str:
    terms = sorted({_readable_recorded_term(value) for value in values})
    if not terms:
        return "not recorded"
    if len(terms) == 1:
        return terms[0]
    if len(terms) == 2:
        return f"{terms[0]} and {terms[1]}"
    return f"{', '.join(terms[:-1])}, and {terms[-1]}"


def _attempt_evidence_summary(rows: Sequence[Mapping[str, Any]]) -> str:
    evidence_rows = []
    missing = 0
    for row in rows:
        payload = row["series"]
        evidence = payload.get("no_verdict_evidence")
        if evidence is None:
            missing += 1
            continue
        if (
            not isinstance(evidence, Mapping)
            or set(evidence) != {"attempts", "evidence_sha256"}
            or not isinstance(evidence.get("attempts"), list)
            or not evidence["attempts"]
        ):
            raise RenderRefused("recorded no-verdict evidence is malformed")
        evidence_rows.extend(evidence["attempts"])
    if not evidence_rows:
        return "No narrower cause was recorded for this evidence."
    if any(
        not isinstance(attempt, Mapping)
        or set(attempt) != PROJECTED_NO_VERDICT_ATTEMPT_FIELDS
        # Hermit's SeriesAttemptDisposition: null (the projection's form of
        # an absent detail) or a nonblank string.
        or not (
            attempt["detail"] is None
            or (isinstance(attempt["detail"], str) and attempt["detail"].strip())
        )
        for attempt in evidence_rows
    ):
        raise RenderRefused("recorded no-verdict attempt evidence is malformed")

    fields = (
        ("attempt outcome", "attempt_outcome"),
        ("disposition", "disposition"),
        ("reason", "kind"),
        ("error kind", "error_kind"),
        ("status", "status"),
        ("signal", "signal"),
    )
    parts = []
    for label, field in fields:
        values = [
            attempt[field]
            for attempt in evidence_rows
            if attempt.get(field) is not None
        ]
        if values:
            parts.append(f"{label} {_joined_terms(values)}")
    if any("timed_out" not in attempt for attempt in evidence_rows):
        raise RenderRefused("recorded no-verdict attempt omits timed_out")
    parts.append(
        "timed out "
        + _joined_terms(
            "yes" if attempt["timed_out"] else "no" for attempt in evidence_rows
        )
    )
    summary = "Recorded attempt evidence: " + "; ".join(parts) + "."
    if missing:
        summary += (
            " No narrower cause was recorded for "
            f"{_counted(missing, 'other qualifying ledger row')}."
        )
    return summary


def _omission_summary(model: SiteModel, item: Mapping[str, Any]) -> str:
    event_ids = tuple(str(value) for value in item["qualifying_event_ids"])
    if len(event_ids) != len(set(event_ids)) or any(
        event_id not in model.series_by_event for event_id in event_ids
    ):
        raise RenderRefused("deliberate omission qualifying events are not exact")
    rows = [model.series_by_event[event_id] for event_id in event_ids]
    if len(rows) != int(item["qualifying_physical_row_count"]):
        raise RenderRefused("deliberate omission qualifying row count is not exact")

    reducer = _writer()._build_data_module()._modules().reducer
    try:
        normalised = [
            reducer._normalise_series_row(row, index) for index, row in enumerate(rows)
        ]
        groups: dict[tuple[Any, ...], list[Mapping[str, Any]]] = {}
        for row in normalised:
            groups.setdefault(reducer._execution_key(row), []).append(row)
        executions = [
            (reducer._execution(groups[key]), groups[key]) for key in sorted(groups)
        ]
    except reducer.ReductionRefused as error:
        raise RenderRefused(
            f"cannot group deliberate omission evidence: {error}"
        ) from error
    execution_count = len(groups)
    row_count = len(normalised)
    represented = sum(int(row["num_runs"]) for row in normalised)
    if represented != int(item["qualifying_represented_run_count"]):
        raise RenderRefused(
            "deliberate omission qualifying represented-run count is not exact"
        )
    explicit_attempt_rows = all(
        row["attempt"] is not None and int(row["num_runs"]) == 1 for row in normalised
    )
    producers = sorted({str(row["producer"]) for row in normalised})
    producer = producers[0] if len(producers) == 1 else " and ".join(producers)
    scope = f"this {item['mode']}/{item['backend']} cell"
    attempts_per_execution = {len(group) for group in groups.values()}
    indexed_repetitions = all(
        type(execution["run_index"]) is int for execution, _rows in executions
    )
    if explicit_attempt_rows and len(attempts_per_execution) == 1:
        attempts = next(iter(attempts_per_execution))
        population = (
            f"{_counted(execution_count, 'qualifying repetition')} × "
            f"{_counted(attempts, 'attempt')} ({_counted(row_count, 'attempt row')})"
            if indexed_repetitions
            else (
                f"{_counted(execution_count, 'qualifying execution')} "
                f"({_counted(row_count, 'attempt row')})"
            )
        )
        evidence = f"For {scope}, {producer} recorded {population}."
    else:
        population_name = (
            "qualifying repetition"
            if indexed_repetitions
            else "qualifying execution"
        )
        evidence = (
            f"For {scope}, {producer} recorded "
            f"{_counted(execution_count, population_name)} across "
            f"{_counted(row_count, 'qualifying ledger row')} representing "
            f"{_counted(represented, 'Hermit run')}."
        )

    execution_values = [execution for execution, _rows in executions]
    latest = reducer._latest_execution(execution_values)
    if latest is None:
        newest = max(execution["observed_at"][0] for execution in execution_values)
        latest_rows = [
            model.series_by_event[event_id]
            for execution, _grouped_rows in executions
            if execution["observed_at"][0] == newest
            for event_id in execution["event_ids"]
        ]
        latest_label = (
            "The newest tied repetitions"
            if indexed_repetitions
            else "The newest tied executions"
        )
        latest_verb = "record"
    else:
        latest_rows = [
            model.series_by_event[event_id] for event_id in latest["event_ids"]
        ]
        latest_label = (
            "The latest repetition"
            if indexed_repetitions
            else "The latest execution"
        )
        latest_verb = "records"
    outcomes = [row["series"]["outcome"] for row in latest_rows]
    failure_classes = [row["series"].get("failure_class") for row in latest_rows]
    recorded_values = (
        f"{latest_label} {latest_verb} outcome {_joined_terms(outcomes)} and "
        f"failure class {_joined_terms(failure_classes)}."
    )
    attempt_evidence = _attempt_evidence_summary(latest_rows)
    reason = str(item["reason"])
    if reason == reducer._NO_RESULT_REASON:
        conclusion = (
            "No pass or product failure result was recorded for that latest "
            "execution, so this cell is excluded."
        )
    elif reason == reducer._AMBIGUOUS_LATEST_REASON:
        conclusion = (
            "The latest executions do not identify one unique latest execution, "
            "so this cell is excluded."
        )
    else:
        raise RenderRefused("deliberate omission reason is unsupported")
    return f"{evidence} {recorded_values} {attempt_evidence} {conclusion}"


def _section_heading(title: str, *, show_heading: bool) -> str:
    return f"<h2>{_escape(title)}</h2>" if show_heading else ""


def _exclusion_audit(
    model: SiteModel, page: str, *, show_heading: bool = True
) -> str:
    runs = model.runs
    items = tuple(runs["projection_exclusion_counts"])
    rows = "".join(
        f"<tr {_collection_item_attributes(filters={}, sorts={})}>"
        f"<td>{_escape(item['schema'])}</td><td>{_escape(item['reason'])}</td>"
        f"<td>{_integer(item['physical_row_count'])}</td>"
        f"<td>{_integer(item['represented_run_count'])}</td></tr>"
        for item in items
    )
    controls = _collection_controls(
        "projection-audit",
        "projection_",
        "projection reasons",
        filters=(
            (
                "schema",
                "Schema",
                tuple(sorted({str(item["schema"]) for item in items})),
            ),
            (
                "reason",
                "Reason",
                tuple(sorted({str(item["reason"]) for item in items})),
            ),
        ),
        sorts=(
            ("schema", "Schema"),
            ("reason", "Reason"),
            ("physical", "Physical rows"),
            ("represented", "Represented Hermit runs"),
        ),
        default_sort="schema",
        filter_sources={"schema": "cell:0", "reason": "cell:1"},
        sort_sources={
            "schema": "cell:0",
            "reason": "cell:1",
            "physical": "cell:2",
            "represented": "cell:3",
        },
        sort_in_table=True,
        item_count=len(items),
    )
    headings = "".join(
        (
            _column_heading(
                "Schema",
                "The stress-series schema recorded on the excluded rows.",
                sort="schema",
                sort_kind="list",
            ),
            _column_heading(
                "Reason",
                "The exact projection rule that excluded these rows.",
                sort="reason",
                sort_kind="list",
            ),
            _column_heading(
                "Physical rows",
                "The number of captured series observation rows excluded for this schema and reason.",
                sort="physical",
                sort_kind="list",
            ),
            _column_heading(
                "Represented Hermit runs",
                "The sum of num_runs across those excluded rows; identical repeated observations may share one row.",
                sort="represented",
                sort_kind="list",
            ),
        )
    )
    body = (
        f'{controls}<div class="table-scroll"><table><caption>Recorded exclusion reasons. A row can have more than one reason, so reason counts are not added.</caption>'
        f"<thead><tr>{headings}</tr></thead><tbody>{rows}</tbody></table></div>"
        if rows
        else "<p>No projection exclusion reason was recorded.</p>"
    )
    heading = _section_heading("Projection audit", show_heading=show_heading)
    return f"""
    <section id="series-excluded">
      {heading}
      <p>{_integer(runs['projection_admitted_physical_row_count'])} physical rows were admitted and {_integer(runs['projection_excluded_physical_row_count'])} were excluded. Admitted and excluded counts are disjoint.</p>
      {body}
    </section>"""


def _test_listing(model: SiteModel, page: str) -> str:
    by_category: dict[str, list[Mapping[str, Any]]] = {}
    for test in model.tests:
        by_category.setdefault(str(test["category"]), []).append(test)
    groups = []
    for category, tests in sorted(by_category.items()):
        links = ", ".join(
            _test_link(
                model,
                page,
                str(test["id"]),
                label=_test_name(test["id"], category),
            )
            for test in sorted(tests, key=lambda item: str(item["id"]))
        )
        groups.append(
            '<section class="test-category">'
            f"<h3>{_escape(category)}</h3>"
            f'<p class="test-name-list">{links}</p>'
            "</section>"
        )
    return f'<section id="tests"><h2>Tests</h2>{"".join(groups)}</section>'


def _custom_commands(
    model: SiteModel, page: str, *, show_heading: bool = True
) -> str:
    if not model.custom_commands:
        return ""
    items = []
    for index, command in enumerate(model.custom_commands):
        reason = command.get("not_applicable_reason")
        detail = (
            f'<span class="secondary">Not applicable: {_escape(reason)}</span>'
            if reason
            else ""
        )
        reproducer = command.get("current_reproducer")
        command_text = ""
        if isinstance(reproducer, Mapping) and reproducer.get("shell_command"):
            command_text = (
                f'<code class="command">{_escape(reproducer["shell_command"])}</code>'
            )
        items.append(
            f'<li class="listing-card" {_collection_item_attributes(filters={"applicability": "not-applicable" if reason else "applicable"}, sorts={"manifest": index, "test": command["test"], "cell": command["cell"]})}>{_test_link(model, page, str(command["test"]))}'
            f'<span class="secondary"><code>{_escape(command["cell"])}</code></span>{detail}{command_text}</li>'
        )
    controls = _collection_controls(
        "custom-commands",
        "commands_",
        "custom commands",
        filters=(("applicability", "Applicability", ("applicable", "not-applicable")),),
        sorts=(("manifest", "Manifest order"), ("test", "TestName"), ("cell", "Cell")),
        default_sort="manifest",
        item_count=len(items),
    )
    heading = _section_heading(
        "Custom commands selected by full", show_heading=show_heading
    )
    return f'<section id="custom-commands">{heading}{controls}<ul class="card-list">{"".join(items)}</ul></section>'


def _run_listing(model: SiteModel, page: str, *, show_heading: bool = True) -> str:
    current = set(model.current_history_indices)
    correction_partners = _correction_partner_indices(model.history_rows)
    rows = []
    for index, row in enumerate(model.history_rows):
        results = _cell_evidence(row)
        selected = results["selected_by"].get(row["profile"], {"cell_count": None})
        record = "current" if index in current else "historical"
        run_number = _positive_run_number(row)
        collection_values = {
            "commit": row["commit"],
            "path": row["profile"],
            "record": record,
            "recorded": results["recorded"]["cell_count"],
            "result": row["result"],
            "run": run_number if run_number is not None else "",
            "run_id": row["run_id"],
            "selected": selected["cell_count"],
            "started": row["started_at"],
            "wall_clock": (
                "" if row.get("real_seconds") is None else row["real_seconds"]
            ),
        }
        partner_links = _correction_partner_links(
            model, page, index, correction_partners
        )
        record_label = _ledger_record_label(index in current)
        if partner_links:
            record_label += (
                f'<span class="secondary correction-partners">{partner_links}</span>'
            )
        run_label = _run_display_label(row)
        run_link_attributes = (
            f' class="run-id-link" title="{_escape(run_label)}"'
            if run_number is None
            else ""
        )
        rows.append(
            f'<tr data-run-row="1" data-run-current="{str(index in current).lower()}" {_collection_item_attributes(filters={"record": record}, sorts=collection_values)}>'
            f'<th scope="row"><a{run_link_attributes} href="{_escape(_href(page, model.history_paths[index]))}">{_escape(run_label)}</a></th>'
            f'<td>{record_label}</td><td>{_escape(row["profile"])}</td>'
            f'<td>{_integer(selected["cell_count"]) if selected["cell_count"] is not None else "Not recorded"}</td><td>{_integer(results["recorded"]["cell_count"])}</td>'
            f'<td>{_escape(row["result"])}</td><td>{_hermit_commit_link(row["commit"])}</td>'
            f'<td>{_time_element(row["started_at"], "validation run started_at")}</td>'
            f'<td>{_escape(_duration_seconds(row.get("real_seconds"), "validation run real_seconds"))}</td></tr>'
        )
    controls = _collection_controls(
        "validation-runs",
        "runs_",
        "validation runs",
        filters=(
            ("record", "Record", ("current", "historical")),
            (
                "path",
                "Validate path",
                tuple(sorted({str(row["profile"]) for row in model.history_rows})),
            ),
            (
                "result",
                "Run result",
                tuple(sorted({str(row["result"]) for row in model.history_rows})),
            ),
        ),
        sorts=(
            ("history", "Recorded order"),
            ("started", "Started date"),
            ("wall_clock", "Wall clock"),
            ("run", "Run number"),
            ("path", "Validate path"),
            ("selected", "Cells selected"),
            ("recorded", "Cells recorded"),
            ("result", "Run result"),
            ("record", "Record"),
            ("commit", "Hermit commit"),
        ),
        default_sort="started",
        default_direction="desc",
        filter_sources={
            "record": "value:record",
            "path": "value:path",
            "result": "value:result",
        },
        filter_value_labels={
            "record": {
                "current": "Current ledger record",
                "historical": "Superseded ledger record",
            }
        },
        sort_sources={
            "history": "ordinal",
            "run": "value:run",
            "path": "value:path",
            "selected": "value:selected",
            "recorded": "value:recorded",
            "result": "value:result",
            "record": "value:record",
            "commit": "value:commit",
            "started": "utc:started",
            "wall_clock": "value:wall_clock",
        },
        tie_sort="started",
        tie_direction="desc",
        sort_in_table=True,
        item_count=len(rows),
    )
    headings = "".join(
        (
            _column_heading(
                "Run",
                "The recorded positive run number when present; otherwise the full run id.",
                sort="run",
                sort_kind="list",
            ),
            _column_heading(
                "Ledger record",
                "Current means the terminal row selected after applying the ledger correction chain at this snapshot; superseded means an older row replaced by a later correction. It does not describe code age.",
                sort="record",
                sort_kind="list",
            ),
            _column_heading(
                "Validate path",
                "The quick, full, super, or other recorded validate path that selected this run's cells.",
                sort="path",
                sort_kind="list",
            ),
            _column_heading(
                "Cells selected by that path",
                "The number of cells this run's named validate path selected.",
                sort="selected",
                sort_kind="list",
            ),
            _column_heading(
                "Cells recorded",
                "The number of selected cells for which this validation row retained a CellResult.",
                sort="recorded",
                sort_kind="list",
            ),
            _column_heading(
                "Run result",
                "The terminal result recorded for the whole validation run, separate from each cell verdict.",
                sort="result",
                sort_kind="list",
            ),
            _column_heading(
                "Hermit commit",
                "The Hermit source commit measured by this validation run.",
                sort="commit",
                sort_kind="list",
            ),
            _column_heading(
                "Started",
                "When the validation run started, displayed in Eastern Time.",
                sort="started",
                sort_kind="list",
            ),
            _column_heading(
                "Wall clock",
                "The optional recorded outer makespan of the whole validation run, in seconds; it is not derived from the Started or Finished timestamps.",
                sort="wall_clock",
                sort_kind="list",
            ),
        )
    )
    heading = _section_heading("Validation runs", show_heading=show_heading)
    return f"""
    <section id="validation-runs">
      {heading}
      <p>Whole-run result, the cells selected by its named validate path, and each cell verdict are separate recorded facts.</p>
      {controls}
      <div class="table-scroll"><table class="validation-runs-table responsive-record-table"><caption>Current and superseded admitted validation ledger rows.</caption><colgroup><col class="run-column"><col class="record-column"><col class="path-column"><col class="count-column"><col class="count-column"><col class="result-column"><col class="commit-column"><col class="time-column"><col class="duration-column"></colgroup>
        <thead><tr>{headings}</tr></thead>
        <tbody>{"".join(rows)}</tbody></table></div>
    </section>"""


def presentation_commits(data: Mapping[str, Any]) -> tuple[tuple[str, str], ...]:
    """The exact source identities eligible for presentation-only commit dates."""
    provenance = data["provenance"]
    identities = {
        ("dev-hermit", str(provenance["parent_commit"])),
        ("hermit", str(provenance["hermit_commit"])),
        ("hermit", str(provenance["hermit_main_commit"])),
    }
    identities.update(
        ("hermit", str(row["series"]["tree"]))
        for row in data["runs"]["series_rows"]
    )
    canonical = provenance.get("canonical_input_provenance", {})
    if "series_commit" in canonical:
        if canonical.get("series_repository") != "https://github.com/rrnewton/hermit_test_ledger.git":
            raise RenderRefused("presentation cell ledger repository is unexpected")
        identities.add(("hermit_test_ledger", str(canonical["series_commit"])))
    return tuple(sorted(identities))


def validate_presentation(value: Any, data: Mapping[str, Any] | None = None) -> dict[str, Any] | None:
    if value is None:
        return None
    if not isinstance(value, Mapping) or set(value) != {"build_started_at", "commits"}:
        raise RenderRefused("presentation provenance fields are not exact")
    _eastern_timestamp(value["build_started_at"], "website build start", include_seconds=True)
    rows = value["commits"]
    if not isinstance(rows, list):
        raise RenderRefused("presentation commit dates must be an array")
    identities = []
    for row in rows:
        if not isinstance(row, Mapping) or set(row) != {"repository", "commit", "committed_at"}:
            raise RenderRefused("presentation commit date fields are not exact")
        if row["repository"] not in {"hermit", "dev-hermit", "hermit_test_ledger"} or not isinstance(row["commit"], str) or re.fullmatch(r"[0-9a-f]{40}", row["commit"]) is None:
            raise RenderRefused("presentation commit identity is invalid")
        if row["committed_at"] is not None:
            _eastern_timestamp(row["committed_at"], "source commit date", include_seconds=True)
        identities.append((row["repository"], row["commit"]))
    if identities != sorted(set(identities)):
        raise RenderRefused("presentation commit identities must be unique and sorted")
    if data is not None and tuple(identities) != presentation_commits(data):
        raise RenderRefused("presentation commits disagree with recorded source identities")
    return {"build_started_at": value["build_started_at"], "commits": [dict(row) for row in rows]}


def _readable_time(value: Any, label: str) -> str:
    if value is None:
        return "Not recorded"
    _eastern_timestamp(value, label, include_seconds=True)
    instant = datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(EASTERN_TIME_ZONE)
    return f'<time datetime="{_escape(value)}">{instant.strftime("%b %-d, %Y · %-I:%M %p %Z")}</time>'


def _measurement_freshness(model: SiteModel) -> str:
    values = [cell.get("latest_result_time") for cell in model.cells if _cell_state(cell) in {"green", "red"}]
    values = [value for value in values if isinstance(value, str)]
    latest = max(values, key=lambda value: datetime.fromisoformat(value.replace("Z", "+00:00"))) if values else None
    recorded = [str(row["emitted_at"]) for row in model.series_rows]
    latest_recorded = max(recorded, key=lambda value: datetime.fromisoformat(value.replace("Z", "+00:00"))) if recorded else None
    return (
        '<span class="measurement-freshness"><strong>Latest selected measurement:</strong> '
        + _readable_time(latest, "latest selected measurement")
        + '; <strong>latest history entry:</strong> '
        + _readable_time(latest_recorded, "latest retained history entry")
        + '. Totals may combine qualifying results from multiple runs; rebuilding '
        'this website does not rerun the tests.</span>'
    )


def _provenance(model: SiteModel, page: str = "index.html") -> str:
    provenance = model.data["provenance"]
    presentation = model.presentation or {}
    dates = {(row["repository"], row["commit"]): row["committed_at"] for row in presentation.get("commits", [])}
    revisions: dict[str, list[Mapping[str, Any]]] = {}
    for cell in model.cells:
        for row in _latest_series_rows(model, cell) or ():
            revisions.setdefault(str(row["series"]["tree"]), []).append(row)
    measured_rows = []
    for commit, rows in sorted(
        revisions.items(),
        key=lambda item: (
            max(datetime.fromisoformat(row["emitted_at"].replace("Z", "+00:00")) for row in item[1]),
            item[0],
        ),
        reverse=True,
    ):
        measured = max((row["emitted_at"] for row in rows), key=lambda value: datetime.fromisoformat(value.replace("Z", "+00:00")))
        depths = {_canonical(row["series"].get("depth", {}).get("hermit")) for row in rows}
        depth = json.loads(next(iter(depths))) if len(depths) == 1 else None
        complete_depth = isinstance(depth, Mapping) and all(type(depth.get(key)) is int and depth[key] >= 0 for key in ("first_parent", "commits"))
        first_parent = f'{depth["first_parent"]:,}' if complete_depth else "Not recorded consistently"
        reachable = f'{depth["commits"]:,}' if complete_depth else "Not recorded consistently"
        measured_rows.append(f'<tr><th scope="row">{_hermit_commit_link(commit)}</th><td>{_readable_time(dates.get(("hermit", commit)), "measured commit date")}</td><td>{_readable_time(measured, "measurement date")}</td><td>{first_parent}</td><td>{reachable}</td></tr>')
    source_rows = []
    for label, repository, field in (("Hermit catalogue", "hermit", "hermit_commit"), ("Hermit reference revision", "hermit", "hermit_main_commit"), ("Website data generator (private repository)", "dev-hermit", "parent_commit")):
        commit = str(provenance[field])
        source_rows.append(f'<tr><th scope="row">{label}</th><td>{_github_link(_commit_url(repository, commit), commit[:12], code=True)}</td><td>{_readable_time(dates.get((repository, commit)), "source commit date")}</td></tr>')
    ledger_commit = provenance.get("canonical_input_provenance", {}).get("series_commit")
    if ledger_commit:
        source_rows.append(f'<tr><th scope="row">Cell result ledger</th><td>{_github_link(_commit_url("hermit_test_ledger", ledger_commit), ledger_commit[:12], code=True)}</td><td>{_readable_time(dates.get(("hermit_test_ledger", ledger_commit)), "cell ledger commit date")}</td></tr>')
    return f"""
    <section id="provenance">
      <h2>{_escape(model.wording.provenance_heading)}</h2>
      <p><strong>Website build started:</strong> {_readable_time(presentation.get('build_started_at'), 'website build start')}. This is the website generation time, not the age of its measurements.</p>
      <p id="source-provenance-caption"><strong>Sources used to build this website.</strong> Commit dates are Git committer dates.</p>
      <div class="table-scroll"><table class="source-provenance" aria-labelledby="source-provenance-caption"><thead><tr>{_column_heading("Purpose", "The role this exact source revision played in producing the site data.")}{_column_heading("Source commit", "A short hash linked to the complete immutable Git commit.")}{_column_heading("Commit date", "The commit object’s committer timestamp; not a measurement or website build date.")}</tr></thead><tbody>{''.join(source_rows)}</tbody></table></div>
      <details><summary>Measured Hermit revisions and commit depth</summary>
      <p>Each row identifies a revision used by the selected cell results. Its measurement date is the latest selected result at that revision; its commit date describes the source, not a test run.</p>
      <p id="measurement-provenance-caption"><strong>Measured revisions, newest measurement first.</strong></p>
      <div class="table-scroll"><table class="measurement-provenance" aria-labelledby="measurement-provenance-caption"><thead><tr>{_column_heading("Hermit commit", "The exact Hermit revision used for these selected test results.")}{_column_heading("Commit date", "The measured commit object’s committer timestamp; not a test execution date.")}{_column_heading("Latest measurement", "The latest selected cell measurement at this exact Hermit revision.")}{_column_heading("First-parent depth", "Recorded commits along this Hermit revision’s first-parent chain, including itself.")}{_column_heading("All reachable commits", "All recorded Hermit commits reachable from this revision, including merged side branches.")}</tr></thead><tbody>{''.join(measured_rows)}</tbody></table></div>
      <p>First-parent depth counts commits along the revision’s first-parent chain, including the revision itself. All reachable commits also counts merged side branches. These are recorded Hermit history counts, not elapsed time or a claim that a revision is that many commits behind main. A distance requires a verified ancestry relationship. Missing or conflicting depths are not inferred.</p>
      </details>
      <details><summary>Full provenance</summary><dl class="facts">
        <div><dt>Hermit commit used for manifest policy</dt><dd>{_github_link(_commit_url('hermit', str(provenance['hermit_commit'])), str(provenance['hermit_commit']), code=True)}</dd></div>
        <div><dt>Hermit commit used for ancestry classification</dt><dd>{_github_link(_commit_url('hermit', str(provenance['hermit_main_commit'])), str(provenance['hermit_main_commit']), code=True)}</dd></div>
        <div><dt>dev-hermit source commit (private repository)</dt><dd>{_github_link(_commit_url('dev-hermit', str(provenance['parent_commit'])), str(provenance['parent_commit']), code=True)}</dd></div>
        <div><dt>Manifest digest</dt><dd><code>{_escape(provenance['manifest_sha256'])}</code></dd></div>
        <div><dt>Captured series source digest</dt><dd><code>{_escape(provenance['series_rows_sha256'])}</code></dd></div>
        <div><dt>Public series projection digest</dt><dd><code>{_escape(provenance['series_rows_projection_sha256'])}</code></dd></div>
        <div><dt>Stage 1 artifact digest</dt><dd><code>{_escape(model.build['build_data_sha256'])}</code></dd></div>
      </dl></details>
    </section>"""


def _historical_measurement_note(model: SiteModel) -> str:
    historical = model.data["provenance"].get("historical")
    if not isinstance(historical, Mapping):
        raise RenderRefused("historical introduction requires historical provenance")
    finished_at = historical.get("finished_at")
    source_run_finished = _time_element(finished_at, "historical finished_at")
    result_times = [
        cell.get("latest_result_time")
        for cell in model.cells
        if _cell_state(cell) in {"green", "red"}
    ]
    if not result_times or any(not isinstance(value, str) for value in result_times):
        raise RenderRefused(
            "historical introduction requires a timestamp for every selected result"
        )
    ordered = sorted(
        result_times,
        key=lambda value: datetime.fromisoformat(
            str(value)[:-1] + "+00:00" if str(value).endswith("Z") else str(value)
        ),
    )
    earliest = _time_element(ordered[0], "earliest selected result time")
    latest = _time_element(ordered[-1], "latest selected result time")
    return (
        '<p><strong>Historical measurement.</strong> This page is not a live '
        "status view. The time in <em>Full snapshot provenance</em> is when the "
        f"source validation run finished ({source_run_finished}); it is not the "
        "timestamp of every cell result. The result counts select qualifying "
        "measurements retained in this artifact from multiple runs. The selected "
        f"latest result for each cell spans {earliest} through {latest}. Work and "
        "measurements not retained in this artifact are outside these counts.</p>"
    )


def _historical_introduction(model: SiteModel) -> str:
    return f"""
    <div class="scorecard-introduction">
      <p>For more context, see the <a href="https://github.com/rrnewton/hermit/blob/main/README.md">README</a> and the high-level <a href="https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/">introduction in the blog post</a>.</p>
      <p>Hermit testing centers on a suite of test programs, each identified by a test ID. The central compatibility matrix is indexed by <strong>test ID × mode × backend</strong>. A mode selects deterministic execution, record/replay, or chaos concurrency testing. A backend identifies the Reverie implementation and the way it instruments the guest program.</p>
      {_historical_measurement_note(model)}
      <p>Comparing two dates means opening two snapshots and comparing them; no page on this site does that for you.</p>
      <p>Read the result counts against the right denominator. Green and red are counted only over cells for which this artifact has a selected result, which is a much smaller population than the manifest. The remaining cells are reported separately and are not counted as failures; folding them into the result denominator would understate the pass rate.</p>
    </div>"""


def _landing_introduction(model: SiteModel) -> str:
    if "historical" in model.data["provenance"]:
        return _historical_introduction(model)
    measurement_note = _measurement_freshness(model)
    return f"""
    <div class="scorecard-introduction">
      <p>Hermit makes Linux execution repeatable by controlling scheduling, time, and randomness. This scorecard compares its backends: <strong>determinism</strong> asks whether repeated runs agree, while <strong>parity</strong> asks whether a backend agrees with the reference ptrace backend. Each cell is one workload, execution mode, and backend; results distinguish passes, failures, and unmeasured cells. {measurement_note} For context, start with the <a href="https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/">introduction to Hermit</a>, or visit the <a href="https://github.com/rrnewton/hermit/blob/main/README.md">README for setup and usage</a>.</p>
    </div>"""


def _missing_attempts_notice(model: SiteModel, page: str) -> str:
    """Name, in plain words, the recorded test attempts that the published
    history lacks, by validation run; empty when there are none."""
    entries = [
        item
        for item in model.runs["projection_exclusion_counts"]
        if item["schema"] == MISSING_CENSUS_ATTEMPT_SCHEMA
    ]
    if not entries:
        return ""
    attempts = sum(int(item["physical_row_count"]) for item in entries)
    headline = (
        f"{_integer(attempts)} recorded test attempt{'' if attempts == 1 else 's'} "
        f"from {_integer(len(entries))} validation run{'' if len(entries) == 1 else 's'} "
        f"{'is' if attempts == 1 else 'are'} missing from the published history."
    )
    listed = "".join(
        f"<li>{_escape(item['reason'])}</li>"
        for item in entries[:MISSING_ATTEMPT_RUNS_LISTED]
    )
    unlisted = len(entries) - MISSING_ATTEMPT_RUNS_LISTED
    more = (
        f"<p>And {_integer(unlisted)} more validation "
        f"run{'' if unlisted == 1 else 's'}.</p>"
        if unlisted > 0
        else ""
    )
    audit = _href(page, "cells/projection-audit.html", "series-excluded")
    return f"""
    <div class="notice" id="missing-history-attempts">
      <p><strong>{headline}</strong> Each validation run keeps its own record of every test attempt it made, and this site publishes a history of those attempts. The site builder compares the two. These attempts are in their run's record but not in the published history, and nothing records why. The green, red and never-measured cell results on this site come from the published history, so these attempts are not counted in them. Each validation run's page still shows the per-cell verdicts that run recorded.</p>
      <ul>{listed}</ul>{more}
      <p>The <a href="{_escape(audit)}">projection audit</a> lists every run under {_escape(MISSING_CENSUS_ATTEMPT_SCHEMA)}.</p>
    </div>"""


def _landing_page(model: SiteModel) -> bytes:
    page = "index.html"
    content = (
        _missing_attempts_notice(model, page)
        + (_landing_introduction(model) if model.wording.landing_introduction else "")
        + _overview(model, page)
        + _test_listing(model, page)
        + ("" if model.wording.disclosure else _provenance(model))
    )
    return _layout(
        page,
        "Hermit compatibility scorecard" if model.wording.landing_introduction else "Compatibility evidence",
        model.wording.landing_lead,
        content,
        (("Overview", None),),
        model.wording,
    )


def _projection_audit_page(model: SiteModel) -> bytes:
    page = "cells/projection-audit.html"
    content = _exclusion_audit(
        model, page, show_heading=False
    ) + _deliberate_omission_audit(model, page)
    return _layout(
        page,
        "Projection audit",
        "Recorded exclusions from the published cell projection.",
        content,
        (
            ("Overview", "index.html"),
            ("Cells", "cells/index.html"),
            ("Projection audit", None),
        ),
        model.wording,
    )


def _custom_commands_page(model: SiteModel) -> bytes:
    page = "tests/custom-commands.html"
    content = _custom_commands(model, page, show_heading=False)
    if not content:
        content = (
            '<section id="custom-commands">'
            "<p>No custom command was selected by full.</p></section>"
        )
    return _layout(
        page,
        "Custom commands selected by full",
        "Custom commands recorded in the manifest and selected by full.",
        content,
        (
            ("Overview", "index.html"),
            ("Tests", "index.html#tests"),
            ("Custom commands selected by full", None),
        ),
        model.wording,
    )


def _runs_index(model: SiteModel) -> bytes:
    page = "runs/index.html"
    return _layout(
        page,
        "Validation runs",
        "Admitted validation ledger rows.",
        _run_listing(model, page, show_heading=False),
        (("Overview", "index.html"), ("Validation runs", None)),
        model.wording,
    )


def _options(
    values: Sequence[str],
    labels: Mapping[str, str] | None = None,
    *,
    all_label: str = "All",
) -> str:
    labels = {} if labels is None else labels
    return f'<option value="">{_escape(all_label)}</option>' + "".join(
        f'<option value="{_escape(value)}">{_escape(labels.get(value, value))}</option>'
        for value in values
    )


def _collection_sort_button(
    name: str,
    label: str,
    *,
    active: bool,
    direction: str,
) -> str:
    current_direction = "ascending" if direction == "asc" else "descending"
    next_direction = (
        "descending" if active and direction == "asc" else "ascending"
    )
    action = (
        f"Sorted by {label} {current_direction}. Activate to sort {next_direction}."
        if active
        else f"Sort by {label} ascending."
    )
    indicator = "▲" if active and direction == "asc" else "▼" if active else ""
    return (
        f'<button type="button" class="list-sort" data-list-sort="{_escape(name)}" '
        f'data-list-sort-label="{_escape(label)}" aria-pressed="{str(active).lower()}" '
        f'title="{_escape(action)}" aria-label="{_escape(action)}">'
        f'<span>{_escape(label)}</span><span class="sort-indicator" aria-hidden="true">'
        f"{indicator}</span></button>"
    )


def _collection_controls(
    collection_id: str,
    query_prefix: str,
    noun: str,
    *,
    filters: Sequence[tuple[str, str, Sequence[str]]] = (),
    sorts: Sequence[tuple[str, str]],
    default_sort: str,
    filter_sources: Mapping[str, str] | None = None,
    filter_value_labels: Mapping[str, Mapping[str, str]] | None = None,
    sort_sources: Mapping[str, str] | None = None,
    default_direction: str = "asc",
    tie_sort: str | None = None,
    tie_source: str = "ordinal",
    tie_direction: str = "asc",
    page_size: int = LIST_PAGE_SIZE,
    item_count: int,
    child_limit: int | None = None,
    sort_in_table: bool = False,
) -> str:
    if default_sort not in {name for name, _label in sorts}:
        raise RenderRefused(f"collection {collection_id!r} has no default sort")
    if tie_sort is not None and tie_sort not in {name for name, _label in sorts}:
        raise RenderRefused(f"collection {collection_id!r} has no tie-break sort")
    if default_direction not in {"asc", "desc"} or tie_direction not in {
        "asc",
        "desc",
    }:
        raise RenderRefused(f"collection {collection_id!r} has invalid direction")
    if (
        page_size <= 0
        or item_count < 0
        or child_limit is not None
        and child_limit <= 0
    ):
        raise RenderRefused(f"collection {collection_id!r} has invalid limits")
    filter_sources = {} if filter_sources is None else filter_sources
    filter_value_labels = {} if filter_value_labels is None else filter_value_labels
    sort_sources = {} if sort_sources is None else sort_sources
    source_pattern = re.compile(
        r"(?:ordinal|text|cell:[0-9]+|value:[a-z][a-z0-9_]*|utc:[a-z][a-z0-9_]*)"
    )
    if any(
        not source_pattern.fullmatch(source)
        for source in (*filter_sources.values(), *sort_sources.values(), tie_source)
    ):
        raise RenderRefused(f"collection {collection_id!r} has invalid value source")
    filter_controls = "".join(
        f'<label>{_escape(label)} <select name="{_escape(name)}" data-list-filter="{_escape(name)}" data-list-source="{_escape(filter_sources.get(name, f"value:{name}"))}">{_options(tuple(values), filter_value_labels.get(name))}</select></label>'
        for name, label, values in filters
    )
    sort_options = "".join(
        f'<option value="{_escape(name)}" data-list-source="{_escape(sort_sources.get(name, f"value:{name}"))}">{_escape(label)}</option>'
        for name, label in sorts
    )
    hidden_sort_controls = (
        f'<select name="sort" aria-label="Sort key for {_escape(noun)}" hidden>'
        f"{sort_options}</select>"
        f'<select name="direction" aria-label="Sort direction for {_escape(noun)}" hidden>'
        '<option value="asc">Ascending</option><option value="desc">Descending</option></select>'
    )
    if sort_in_table:
        sort_controls = (
            '<div class="mobile-table-order">'
            f'<label>Order <select name="sort" aria-label="Sort key for {_escape(noun)}">'
            f"{sort_options}</select></label>"
            f'<label>Direction <select name="direction" aria-label="Sort direction for {_escape(noun)}">'
            '<option value="asc">Ascending</option><option value="desc">Descending</option></select></label>'
            "</div>"
        )
    else:
        sort_buttons = "".join(
            _collection_sort_button(
                name,
                label,
                active=name == default_sort,
                direction=default_direction,
            )
            for name, label in sorts
        )
        sort_controls = (
            hidden_sort_controls
            + f'<div class="list-order-controls" role="group" aria-label="Order {_escape(noun)}">'
            '<span class="list-order-label">Order</span>'
            f"{sort_buttons}</div>"
        )
    actions = '<div class="filter-actions"><button type="button" data-list-clear>Clear filters</button></div>'
    child_attribute = (
        f' data-child-limit="{child_limit}"' if child_limit is not None else ""
    )
    tie_sort_attribute = (
        f' data-tie-sort="{_escape(tie_sort)}"' if tie_sort is not None else ""
    )
    pagination = (
        f'<nav class="list-pagination" data-list-pagination="{_escape(collection_id)}" aria-label="{_escape(noun)} pages" hidden>'
        '<button type="button" data-list-previous>Previous</button>'
        f'<span aria-live="polite" data-list-summary>Showing all {_escape(noun)}.</span>'
        '<button type="button" data-list-next>Next</button></nav>'
        if item_count > page_size
        else ""
    )
    return f"""
      <form class="list-controls" data-list-controls="{_escape(collection_id)}" data-query-prefix="{_escape(query_prefix)}" data-page-size="{page_size}" data-default-sort="{_escape(default_sort)}" data-default-direction="{_escape(default_direction)}" data-tie-source="{_escape(tie_source)}" data-tie-direction="{_escape(tie_direction)}"{tie_sort_attribute}{child_attribute} hidden>
        <fieldset><legend>Find and order {_escape(noun)}</legend>
          <label class="search-control">Search <input name="q" type="search" maxlength="256"></label>
          {filter_controls}
          {sort_controls}
        </fieldset>
        {actions}
      </form>
      <p class="notice" data-list-error="{_escape(collection_id)}" hidden></p>
      <p data-list-empty="{_escape(collection_id)}" hidden>No {_escape(noun)} match these filters.</p>
      {pagination}"""


def _collection_item_attributes(
    *,
    filters: Mapping[str, Any],
    sorts: Mapping[str, Any],
) -> str:
    values = dict(filters)
    for name, value in sorts.items():
        previous = values.setdefault(name, value)
        if previous != value:
            raise RenderRefused(f"collection item gives {name!r} contradictory values")
    attributes = []
    for name, value in sorted(values.items()):
        if not re.fullmatch(r"[a-z][a-z0-9_]*", name):
            raise RenderRefused(f"collection item has invalid field {name!r}")
        attribute = name.replace("_", "-")
        attributes.append(f'data-list-value-{attribute}="{_escape(value)}"')
    return 'data-list-item="1" ' + " ".join(attributes)


def _selection_paths(model: SiteModel) -> tuple[str, ...]:
    return tuple(
        sorted({str(path) for cell in model.cells for path in cell["selected_by"]})
    )


def _cell_filter_form(
    model: SiteModel,
    cells: Sequence[Mapping[str, Any]],
    *,
    fixed_state: str | None,
) -> str:
    values = lambda field: tuple(sorted({str(cell[field]) for cell in cells}))
    state_control = (
        '<input name="state" type="hidden" value="never-measured">'
        if fixed_state == "never-measured"
        else ('<input name="state" type="hidden" value="">')
    )
    return f"""
    <form class="cell-filters" data-cell-filter-form hidden>
      <fieldset><legend>Find cells</legend>
        <label class="search-control">Search cells (updates as you type) <input name="q" type="search" maxlength="256" aria-controls="cell-results"></label>
        <input name="page" type="hidden" value="1">
        <label>Lane <select name="lane">{_options(values('lane'))}</select></label>
        <label>Category <select name="category">{_options(values('category'))}</select></label>
        <label>Mode <select name="mode">{_options(values('mode'))}</select></label>
        <label>Backend <select name="backend">{_options(values('backend'))}</select></label>
        {state_control}
        <label>Latest result <select name="result">{_options(('pass', 'product_failure'), {'pass': 'Pass', 'product_failure': 'Fail'})}</select></label>
        <label>Selected by path <select name="selected_by_path">{_options(_selection_paths(model))}</select></label>
        <label>Current main ancestry <select name="current_main_ancestry">{_options(('true', 'false'), {'true': 'True', 'false': 'False'})}</select></label>
        <label>Host capability <select name="host_capability">{_options(model.host_capability_names)}</select></label>
        <label>Host capability present <select name="host_capability_present">{_options(('true', 'false'), {'true': 'Present', 'false': 'Not present'})}</select></label>
        <label>Population <select name="population"><option value="in_manifest">In the manifest</option><option value="not_in_current_manifest">Recorded identity outside current manifest</option></select></label>
        <select name="evidence" hidden><option value=""></option><option value="complete">Recorded</option></select>
      </fieldset>
      <div class="mobile-table-order">
        <label>Order <select name="sort" aria-label="Sort"><option value="manifest">Manifest order</option><option value="cell">Cell</option><option value="test">TestName</option><option value="lane">Lane</option><option value="category">Category</option><option value="mode">Mode</option><option value="backend">Backend</option><option value="selected_by_path">Selected by path</option><option value="state">State</option><option value="result">Latest result</option><option value="latest_result_time">Latest result time</option><option value="latest_result_tree">Latest result tree</option><option value="physical_row_count">Physical rows</option><option value="represented_run_count">Represented Hermit runs</option></select></label>
        <label>Direction <select name="direction" aria-label="Sort direction"><option value="asc">Ascending</option><option value="desc">Descending</option></select></label>
      </div>
      <div class="filter-actions"><button type="button" data-clear-cell-filters>Clear filters</button></div>
      <output class="filter-live-summary" data-cell-filter-live-summary role="status" aria-live="polite" aria-atomic="true">Loading filterable results…</output>
    </form>"""


def _cell_table_row(model: SiteModel, page: str, cell: Mapping[str, Any]) -> str:
    identity = str(cell["cell"])
    state = _cell_state(cell)
    result = str(cell.get("latest_result") or "")
    selected_paths = sorted(str(path) for path in cell["selected_by"])
    rows = model.series_by_cell[identity]
    search = " ".join(
        (
            identity,
            str(cell["test"]),
            str(cell["lane"]),
            str(cell["category"]),
            str(cell["mode"]),
            str(cell["backend"]),
        )
    )
    latest_time = str(cell.get("latest_result_time") or "")
    latest_tree = str(cell.get("latest_result_tree") or "")
    test_name = _test_name(cell["test"], cell["category"])
    test_link = _cell_link(model, page, identity, label=test_name)
    latest_tree_link = (
        _hermit_commit_link(latest_tree) if latest_tree else "No qualifying result"
    )
    return f"""
      <tr data-cell-row="1" data-cell="{_escape(identity)}" data-test="{_escape(cell['test'])}" data-lane="{_escape(cell['lane'])}" data-category="{_escape(cell['category'])}" data-mode="{_escape(cell['mode'])}" data-backend="{_escape(cell['backend'])}" data-state="{_escape(state)}" data-result="{_escape(result)}" data-selected-by-paths="{_escape(_canonical(selected_paths))}" data-evidence="complete" data-latest-result-time="{_escape(latest_time)}" data-latest-result-tree="{_escape(latest_tree)}" data-search="{_escape(search)}" data-series-filter-facets="{_escape(_series_filter_facets_json(rows))}" data-physical-row-count="{_escape(cell['physical_row_count'])}" data-represented-run-count="{_escape(cell['represented_run_count'])}">
        <td>{_escape(cell['category'])}</td><th scope="row">{test_link}</th><td>{_escape(cell['mode'])}</td><td>{_escape(cell['backend'])}</td><td>{_escape(cell['lane'])}</td><td>{_latest_result(cell)}</td><td>{_time_element(latest_time, 'cell latest_result_time')}</td><td>{latest_tree_link}</td><td>{_integer(cell['physical_row_count'])}</td><td>{_integer(cell['represented_run_count'])}</td>
      </tr>"""


def _outside_table(model: SiteModel, page: str) -> str:
    rows = []
    for item in model.recorded_outside_manifest[:LIST_PAGE_SIZE]:
        identity = str(item["cell"])
        rows.append(
            f'<tr data-recorded-not-in-manifest-row="1" data-cell="{_escape(identity)}" data-search="{_escape(identity)}" data-physical-row-count="{item["physical_row_count"]}" data-represented-run-count="{item["represented_run_count"]}"><th scope="row"><code>{_escape(identity)}</code></th><td>{_integer(item["physical_row_count"])}</td><td>{_integer(item["represented_run_count"])}</td></tr>'
        )
    headings = "".join(
        (
            _column_heading(
                "Cell",
                "The complete recorded test, mode, and backend identity, which is not in the current manifest.",
                sort="cell",
                sort_kind="cell",
            ),
            _column_heading(
                "Physical rows",
                "The number of admitted stress-series/v3 observation rows retained for this identity.",
                sort="physical_row_count",
                sort_kind="cell",
            ),
            _column_heading(
                "Represented Hermit runs",
                "The sum of num_runs across those rows; exactly equal repeated observations may share one row.",
                sort="represented_run_count",
                sort_kind="cell",
            ),
        )
    )
    return f"""
    <div id="recorded-not-in-current-manifest" hidden>
      <h3>Recorded cell identities outside the current manifest</h3>
      <div class="table-scroll"><table class="responsive-record-table"><caption>Recorded evidence whose cell identity is outside the current manifest.</caption><thead><tr>{headings}</tr></thead><tbody id="recorded-not-in-current-manifest-results">{"".join(rows)}</tbody></table></div>
    </div>"""


def _cell_table_headings(*, sortable: bool = True) -> str:
    definitions = (
        (
            "Category",
            "The manifest group that prefixes the test identifier.",
            "category",
        ),
        (
            "TestName",
            "The test identifier with its Category prefix omitted; the link opens this test, mode, and backend cell.",
            "test",
        ),
        ("Mode", "The Hermit action used for this cell.", "mode"),
        ("Backend", "The execution backend used for this cell.", "backend"),
        ("Lane", "The manifest lane that owns this test.", "lane"),
        (
            "Latest result",
            "The latest unambiguous qualifying logical execution: Pass or Fail. Never measured means no qualifying admitted row exists; a resultless or ambiguous latest execution is omitted.",
            "result",
        ),
        (
            "Latest result time",
            "When that latest qualifying logical execution was recorded, displayed in Eastern Time.",
            "latest_result_time",
        ),
        (
            "Latest result tree",
            "The Hermit source tree recorded for the chosen latest qualifying logical execution; the SHA links to that Hermit commit.",
            "latest_result_tree",
        ),
        (
            "Physical rows",
            "The number of admitted stress-series/v3 records retained for this cell, whether or not they qualify for the latest result.",
            "physical_row_count",
        ),
        (
            "Represented Hermit runs",
            "The sum of SeriesRow.num_runs: outer cell observations including stress repetitions and retries, not OS process invocations. A verify attempt counts once although Hermit runs twice internally.",
            "represented_run_count",
        ),
    )
    return "".join(
        _column_heading(
            label,
            description,
            sort=sort if sortable else None,
            sort_kind="cell" if sortable else None,
        )
        for label, description, sort in definitions
    )


def _cell_listing_path(*, explicit_never_measured: bool, page_number: int) -> str:
    if page_number < 1:
        raise RenderRefused("static cell page number must be positive")
    if explicit_never_measured:
        name = (
            "never-measured.html"
            if page_number == 1
            else f"never-measured-{page_number}.html"
        )
    else:
        name = "index.html" if page_number == 1 else f"results-{page_number}.html"
    return f"cells/{name}"


def _static_cell_pagination(
    page: str,
    *,
    item_count: int,
    page_number: int,
    explicit_never_measured: bool,
) -> str:
    page_count = max(1, (item_count + LIST_PAGE_SIZE - 1) // LIST_PAGE_SIZE)
    if page_number < 1 or page_number > page_count:
        raise RenderRefused("static cell page lies outside its population")
    if page_count == 1:
        return ""
    first = (page_number - 1) * LIST_PAGE_SIZE + 1
    last = min(page_number * LIST_PAGE_SIZE, item_count)
    links = []
    if page_number > 1:
        target = _cell_listing_path(
            explicit_never_measured=explicit_never_measured,
            page_number=page_number - 1,
        )
        links.append(
            f'<a rel="prev" href="{_escape(_href(page, target, "all-cells"))}">Previous page</a>'
        )
    links.append(
        f"<span>Showing {_integer(first)}–{_integer(last)} of {_integer(item_count)}; page {_integer(page_number)} of {_integer(page_count)}.</span>"
    )
    if page_number < page_count:
        target = _cell_listing_path(
            explicit_never_measured=explicit_never_measured,
            page_number=page_number + 1,
        )
        links.append(
            f'<a rel="next" href="{_escape(_href(page, target, "all-cells"))}">Next page</a>'
        )
    return (
        '<nav class="list-pagination matrix-pagination static-cell-pagination" '
        'aria-label="Static cell pages">' + "".join(links) + "</nav>"
    )


def _cells_listing_page(
    model: SiteModel,
    *,
    page: str,
    title: str,
    lead: str,
    cells: Sequence[Mapping[str, Any]],
    explicit_never_measured: bool,
) -> bytes:
    rows = "".join(
        _cell_table_row(model, page, cell) for cell in cells[:LIST_PAGE_SIZE]
    )
    other_view = (
        f'<a href="{_escape(_href(page, "cells/index.html", "all-cells"))}">Results</a>.'
        if explicit_never_measured
        else f'<a href="{_escape(_href(page, "cells/never-measured.html", "all-cells"))}">Never measured</a>.'
    )
    fixed_state = (
        "<p><strong>State:</strong> Never measured.</p>"
        if explicit_never_measured
        else ""
    )
    table_explanation = (
        "Every row below has zero qualifying admitted rows."
        if explicit_never_measured
        else "The default table contains only green and red result rows."
    )
    table_caption = (
        "Cells in the manifest with no qualifying admitted row."
        if explicit_never_measured
        else model.wording.cell_table_caption
    )
    headings = _cell_table_headings()
    pagination = f"""
      <nav class="list-pagination matrix-pagination" data-cell-pagination data-page-size="{LIST_PAGE_SIZE}" aria-label="Cell result pages" hidden>
        <button type="button" data-cell-previous>Previous</button>
        <span aria-live="polite" data-cell-page-summary>Showing all cells.</span>
        <button type="button" data-cell-next>Next</button>
      </nav>"""
    static_pagination = _static_cell_pagination(
        page,
        item_count=len(cells),
        page_number=1,
        explicit_never_measured=explicit_never_measured,
    )
    static_access = (
        "<noscript><p>Without JavaScript, every row remains available through "
        "these page links.</p>"
        f"{static_pagination}</noscript>"
        if static_pagination
        else ""
    )
    content = f"""
    <p class="data-status" data-load-status data-load-state="loading" role="status">Loading verified local data…</p>
    <section id="cell-filters"><h2>Search and filter</h2><p>{other_view} The table updates immediately as you type or choose a filter.</p>{fixed_state}{_cell_filter_form(model, cells, fixed_state="never-measured" if explicit_never_measured else None)}
      <p id="cell-filter-error" class="notice" hidden></p><p id="cell-filter-empty" hidden>No rows match these filters.</p>
      <p id="cell-filter-counts" hidden><strong data-matching-count>0</strong> <span data-matching-label>matching cells in the manifest</span>; <span data-evidence-label>those cells have</span> <strong data-physical-count>0</strong> physical rows representing <strong data-represented-count>0</strong> Hermit runs.</p>
    </section>
    <section id="test-matrix"><h2>Test Matrix</h2>{pagination}{static_access}
      <div id="all-cells" data-in-manifest-listing>
        <p>{_integer(model.data['counts']['in_manifest_cell_count'])} cells are in the manifest; this table has {_integer(len(cells))} rows. {table_explanation}</p>
        <div class="table-scroll"><table class="responsive-record-table"><caption>{_escape(table_caption)}</caption><thead><tr>{headings}</tr></thead><tbody id="cell-results">{rows}</tbody></table></div>
      </div>
      {_outside_table(model, page)}
    </section>
    <section id="series-rows" data-in-manifest-listing><h2>Recorded evidence</h2><p>{_integer(model.runs['series_physical_row_count'])} admitted physical rows representing {_integer(model.runs['series_represented_run_count'])} Hermit runs. Each cell row reports its qualifying count separately.</p></section>"""
    return _layout(
        page,
        title,
        lead,
        content,
        (("Overview", "index.html"), (title, None)),
        model.wording,
        ("assets/site.js",),
        page_kind="never-list" if explicit_never_measured else "cell-list",
    )


def _static_cells_listing_page(
    model: SiteModel,
    *,
    title: str,
    lead: str,
    cells: Sequence[Mapping[str, Any]],
    explicit_never_measured: bool,
    page_number: int,
) -> bytes:
    page = _cell_listing_path(
        explicit_never_measured=explicit_never_measured,
        page_number=page_number,
    )
    start = (page_number - 1) * LIST_PAGE_SIZE
    selected = cells[start : start + LIST_PAGE_SIZE]
    if not selected:
        raise RenderRefused("static cell page has no rows")
    rows = "".join(_cell_table_row(model, page, cell) for cell in selected)
    pagination = _static_cell_pagination(
        page,
        item_count=len(cells),
        page_number=page_number,
        explicit_never_measured=explicit_never_measured,
    )
    first = start + 1
    last = start + len(selected)
    table_caption = (
        "Cells in the manifest with no qualifying admitted row."
        if explicit_never_measured
        else model.wording.cell_table_caption
    )
    primary = _cell_listing_path(
        explicit_never_measured=explicit_never_measured,
        page_number=1,
    )
    content = f"""
    <p><a href="{_escape(_href(page, primary, "cell-filters"))}">Search and filter this population</a> with JavaScript, or continue through every static row below.</p>
    <section id="test-matrix"><h2>Test Matrix</h2>{pagination}
      <div id="all-cells" data-in-manifest-listing>
        <p>Showing rows {_integer(first)}–{_integer(last)} of {_integer(len(cells))}.</p>
        <div class="table-scroll"><table class="responsive-record-table"><caption>{_escape(table_caption)}</caption><thead><tr>{_cell_table_headings(sortable=False)}</tr></thead><tbody>{rows}</tbody></table></div>
      </div>
      {pagination}
    </section>"""
    page_title = f"{title}, page {_integer(page_number)}"
    return _layout(
        page,
        page_title,
        lead,
        content,
        (
            ("Overview", "index.html"),
            (title, primary),
            (f"Page {_integer(page_number)}", None),
        ),
        model.wording,
        ("assets/site.js",),
        page_kind="static",
    )


def _cells_index(model: SiteModel) -> bytes:
    cells = tuple(cell for cell in model.cells if _cell_state(cell) in {"green", "red"})
    return _cells_listing_page(
        model,
        page="cells/index.html",
        title="Cells",
        lead=model.wording.cells_lead,
        cells=cells,
        explicit_never_measured=False,
    )


def _never_measured_index(model: SiteModel) -> bytes:
    cells = tuple(cell for cell in model.cells if _cell_state(cell) == "never-measured")
    return _cells_listing_page(
        model,
        page="cells/never-measured.html",
        title="Never measured",
        lead="Cells in the manifest for which no qualifying admitted row exists.",
        cells=cells,
        explicit_never_measured=True,
    )


def _selected_groups(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    sections = []
    for path, selection in sorted(_cell_evidence(row)["selected_by"].items()):
        shown = list(selection["cells"])
        published_count = sum(
            _cell_identity(cell) in model.cell_paths for cell in shown
        )
        path_text = str(path)
        path_digest = _full_digest(path_text)
        collection_id = f"run-selected-{path_digest}"
        controls = _collection_controls(
            collection_id,
            f"selected_{path_digest}_",
            f"{path_text} selected-cell groups",
            filters=(
                ("mode", "Mode", tuple(sorted({str(cell["mode"]) for cell in shown}))),
                (
                    "backend",
                    "Backend",
                    tuple(sorted({str(cell["backend"]) for cell in shown})),
                ),
                (
                    "category",
                    "Category",
                    tuple(sorted({str(cell["category"]) for cell in shown})),
                ),
            ),
            sorts=(
                ("hierarchy", "Mode/backend/category"),
                ("test", "TestName"),
            ),
            default_sort="hierarchy",
            page_size=SELECTED_GROUP_PAGE_SIZE,
            item_count=len(
                {
                    (str(cell["mode"]), str(cell["backend"]), str(cell["category"]))
                    for cell in shown
                }
            ),
            child_limit=SELECTED_GROUP_CHILD_LIMIT,
        )
        description = (
            f'<h2>Recorded population</h2><p><strong>{_integer(selection["cell_count"])}</strong> cells have writer-verified recorded attempts. This is not the full planned selection or proof that the run completed.</p>'
            if row.get("derived_cell_results") is not None else
            f'<h2>Selected by {_escape(path)}</h2><p><strong>{_integer(selection["cell_count"])}</strong> cells were selected by {_escape(path)}. Of the selected cells in the manifest, {_integer(published_count)} have published cell pages. Selected cells with no qualifying admitted row have no cell detail page and are listed as plain text.</p>'
        )
        sections.append(
            f'<section id="{_fragment("selected-by", str(path))}">{description}'
            f"{controls}{_grouped_cell_links(model, page, shown, collection_id=collection_id)}</section>"
        )
    return "".join(sections)


def _grouped_cell_links(
    model: SiteModel,
    page: str,
    cells: Sequence[Mapping[str, Any]],
    *,
    collection_id: str = "selected-cells",
) -> str:
    rows = []
    labels: dict[tuple[str, str, str, str], str] = {}
    for cell in cells:
        identity = _cell_identity(cell)
        test = str(cell["test"])
        _, separator, bare_test = test.rpartition("/")
        if not separator or not bare_test:
            raise RenderRefused(f"cell {identity!r} has no bare test label")
        mode = str(cell["mode"])
        backend = str(cell["backend"])
        category = str(cell["category"])
        label_key = (mode, backend, category, bare_test)
        previous = labels.setdefault(label_key, identity)
        if previous != identity:
            raise RenderRefused(
                f"bare test label {bare_test!r} identifies multiple cells "
                f"in {mode}/{backend} {category!r}"
            )
        rows.append((mode, backend, category, bare_test, identity))

    sections: dict[tuple[str, str], dict[str, list[tuple[str, str]]]] = {}
    for mode, backend, category, bare_test, identity in sorted(rows):
        groups = sections.setdefault((mode, backend), {})
        groups.setdefault(category, []).append((bare_test, identity))

    result = []
    group_index = 0
    for (mode, backend), groups in sections.items():
        lines = []
        for category, entries in groups.items():
            links = ", ".join(
                (
                    f'<a href="{_escape(_href(page, detail_path))}">{_escape(bare_test)}</a>'
                    if (detail_path := model.cell_paths.get(identity)) is not None
                    else f'<span class="selected-cell-without-detail">{_escape(bare_test)}</span>'
                )
                for bare_test, identity in entries
            )
            group_id = f"g{group_index}"
            group_index += 1
            lines.append(
                f'<div class="selected-cell-group" data-list-group="{group_id}" data-mode="{_escape(mode)}" data-backend="{_escape(backend)}" data-category="{_escape(category)}"><strong>{_escape(category)}:</strong> {links}</div>'
            )
        mode_label = mode[:1].upper() + mode[1:]
        result.append(
            '<section class="selected-cells-by-mode-backend" data-list-parent="1">'
            f"<h3>{_escape(mode_label)}/{_escape(backend)}</h3>"
            + "".join(lines)
            + "</section>"
        )
    return "".join(result)


def _not_selected_reason(cell: Mapping[str, Any] | None, path: str) -> str:
    if path != "full" or cell is None or cell.get("not_selected_by_full_reason") is None:
        return "No recorded reason for this validate path."
    reason = cell["not_selected_by_full_reason"]
    if not isinstance(reason, Mapping):
        raise RenderRefused("validated not-selected-by-full reason is malformed")
    keys = set(reason)
    if keys not in ({"reason"}, {"reason", "evidence", "result"}):
        raise RenderRefused("validated not-selected-by-full reason has unsupported fields")
    if any(not isinstance(reason[key], str) or not reason[key] for key in keys):
        raise RenderRefused("validated not-selected-by-full reason is incomplete")
    values = [("Reason", str(reason["reason"]))]
    if "evidence" in reason:
        values.extend(
            (("Evidence", str(reason["evidence"])), ("Result", str(reason["result"])))
        )
    facts = "".join(
        f"<div><dt>{_escape(label)}</dt><dd>{_escape(value)}</dd></div>"
        for label, value in values
    )
    return f"<details><summary>Recorded reason</summary><dl class=\"facts compact-facts\">{facts}</dl></details>"


def _recorded_cells(model: SiteModel, page: str, row: Mapping[str, Any]) -> str:
    results = _cell_evidence(row)
    path_name = _evidence_path(row)
    selection = results["selected_by"].get(path_name, {"cells": ()})
    selected_identities = {_cell_identity(cell) for cell in selection["cells"]}
    records = []
    for cell in results["recorded"]["cells"]:
        verdict = cell["cell_verdict"]
        details = []
        if "bitwise_parity" in verdict:
            details.append(f"Bitwise parity: {str(verdict['bitwise_parity']).lower()}")
        if "compared_log_messages" in verdict:
            compared = verdict["compared_log_messages"]
            if isinstance(compared, Mapping):
                details.append(
                    f"Compared INFO messages: left {_integer(compared['left'])}; right {_integer(compared['right'])}"
                )
        if verdict.get("reason"):
            details.append(f"Reason: {verdict['reason']}")
        identity = _cell_identity(cell)
        detail_path = model.cell_paths.get(identity)
        records.append(
            {
                "cell": identity,
                "details": details,
                "href": None if detail_path is None else _href(page, detail_path),
                "selected": identity in selected_identities,
                "selection_reason": _not_selected_reason(
                    model.cell_by_id.get(identity), path_name
                ),
                "test_result": row["result"],
                "tier": verdict["comparison_tier"],
                "verdict": verdict["state"],
            }
        )

    selected_records = [record for record in records if record["selected"]]
    other_records = [record for record in records if not record["selected"]]

    def row_html(record: Mapping[str, Any], *, include_reason: bool) -> str:
        attributes = _collection_item_attributes(
            filters={
                "test_result": record["test_result"],
                "verdict": record["verdict"],
                "tier": record["tier"],
            },
            sorts={
                "cell": record["cell"],
                "details": " ".join(record["details"]),
            },
        )
        cell = (
            f'<a href="{_escape(record["href"])}"><code>{_escape(record["cell"])}</code></a>'
            if record["href"] is not None
            else f'<code>{_escape(record["cell"])}</code>'
        )
        details = "<br>".join(_escape(value) for value in record["details"])
        reason = f'<td>{record["selection_reason"]}</td>' if include_reason else ""
        return (
            f'<tr data-recorded-cell-row="1" {attributes}><th scope="row">{cell}</th>'
            f'<td>{_escape(record["test_result"])}</td><td>{_escape(record["verdict"])}</td>'
            f'<td>{_escape(record["tier"])}</td><td>{details}</td>{reason}</tr>'
        )

    def headings(*, include_reason: bool) -> str:
        values = [
            _column_heading(
                "Cell",
                "The complete test, mode, and backend identity recorded in this validation row.",
                sort="cell",
                sort_kind="list",
            ),
            _column_heading(
                "TEST-RESULT",
                "The process result recorded for this cell execution.",
                sort="test_result",
                sort_kind="list",
            ),
            _column_heading(
                "COMMIT-VERDICT",
                "Whether the recorded comparison supports the declared compatibility result at this commit.",
                sort="verdict",
                sort_kind="list",
            ),
            _column_heading(
                "Comparison tier",
                "The recorded comparison strength used to establish this cell verdict.",
                sort="tier",
                sort_kind="list",
            ),
            _column_heading(
                "Recorded comparison details",
                "The retained parity counts, reason, and other evidence used by the verdict.",
                sort="details",
                sort_kind="list",
            ),
        ]
        if include_reason:
            values.append(
                _column_heading(
                    f"Why not selected by {path_name}",
                    f"The recorded reason this cell was not selected by the {path_name} validate path, when that reason exists.",
                )
            )
        return "".join(values)

    def controls(
        subset: Sequence[Mapping[str, Any]], collection_id: str, prefix: str, noun: str
    ) -> str:
        return _collection_controls(
            collection_id,
            prefix,
            noun,
            filters=(
                (
                    "test_result",
                    "TEST-RESULT",
                    tuple(sorted({str(record["test_result"]) for record in subset})),
                ),
                (
                    "verdict",
                    "COMMIT-VERDICT",
                    tuple(sorted({str(record["verdict"]) for record in subset})),
                ),
                (
                    "tier",
                    "Comparison tier",
                    tuple(sorted({str(record["tier"]) for record in subset})),
                ),
            ),
            sorts=(
                ("recorded", "Recorded order"),
                ("cell", "Cell"),
                ("test_result", "TEST-RESULT"),
                ("verdict", "COMMIT-VERDICT"),
                ("tier", "Comparison tier"),
                ("details", "Recorded comparison details"),
            ),
            default_sort="recorded",
            # NEWEST FIRST. `recorded` resolves to sort_sources "ordinal", the
            # order rows were recorded -- a time order wearing another name. This
            # single call emits BOTH the "recorded-cells" and
            # "recorded-not-selected" sections, so one direction fixes both; a
            # change that touched only one would leave the other ascending.
            default_direction="desc",
            tie_direction="desc",
            filter_sources={
                "test_result": "value:test_result",
                "verdict": "value:verdict",
                "tier": "value:tier",
            },
            sort_sources={
                "recorded": "ordinal",
                "cell": "value:cell",
                "test_result": "value:test_result",
                "verdict": "value:verdict",
                "tier": "value:tier",
                "details": "value:details",
            },
            sort_in_table=True,
            item_count=len(subset),
        )

    sections = []
    if selected_records:
        inline_records = [
            {key: value for key, value in record.items() if key not in {"selected", "selection_reason"}}
            for record in selected_records
        ]
        sections.append(
            f'<section id="recorded-cells"><h2>Recorded results for cells selected by {_escape(path_name)}</h2>'
            f'<p>{_integer(len(selected_records))} selected cells have recorded results in this ledger row.</p>'
            f'{controls(selected_records, "recorded-cells", "recorded_", "recorded selected cells")}'
            f'<div class="table-scroll"><table class="responsive-record-table"><caption>TEST-RESULT and COMMIT-VERDICT are separate adjacent columns.</caption><thead><tr>{headings(include_reason=False)}</tr></thead>'
            f'<tbody data-list-target="recorded-cells">{"".join(row_html(record, include_reason=False) for record in selected_records[:LIST_PAGE_SIZE])}</tbody></table></div>'
            f'<template data-list-records="recorded-cells"><span data-list-records-json>{_escape(_canonical(inline_records))}</span></template></section>'
        )
    if other_records:
        sections.append(
            f'<section id="recorded-not-selected"><h2>Recorded but not selected by {_escape(path_name)}</h2>'
            f'<p>{_integer(len(other_records))} recorded cells are absent from this run\'s {_escape(path_name)} selection.</p>'
            f'{controls(other_records, "recorded-not-selected", "recorded_other_", "recorded cells not selected by this path")}'
            f'<div class="table-scroll"><table class="responsive-record-table"><caption>Recorded cell results outside this run\'s named selection.</caption><thead><tr>{headings(include_reason=True)}</tr></thead>'
            f'<tbody>{"".join(row_html(record, include_reason=True) for record in other_records)}</tbody></table></div></section>'
        )
    artifact = results["recorded"]["artifact"]
    summary = (
        f'<section id="recorded-cell-results"><h2>Recorded cell results</h2><p>Artifact rows: {_integer(artifact["row_count"])}; '
        f'artifact digest <code>{_escape(artifact["sha256"])}</code>.</p></section>'
    )
    return summary + "".join(sections)


def _run_page(model: SiteModel, index: int, row: Mapping[str, Any]) -> bytes:
    page = model.history_paths[index]
    current = index in model.current_history_indices
    results = _cell_evidence(row)
    run_number = _positive_run_number(row)
    run_number_fact = (
        f"<div><dt>Run number</dt><dd>{_integer(run_number)}</dd></div>"
        if run_number is not None
        else ""
    )
    run_label = _run_display_label(row)
    correction_links = _correction_partner_links(model, page, index)
    correction_fact = (
        f"<div><dt>Correction chain</dt><dd>{correction_links}</dd></div>"
        if correction_links
        else ""
    )
    evidence_notice = (
        '<p>Comparison operands were derived by a writer that verified the complete original raw census. Public readers authenticate this derived view, not private raw bytes. Only recorded cells are shown; the original failed or incomplete run status is unchanged.</p>'
        if row.get("derived_cell_results") is not None else ""
    )
    facts = f"""
    {evidence_notice}
    <section id="run-facts"><h2>Retained validation row</h2>
      <p>{_escape(_ledger_record_explanation(current))}</p><dl class="facts">
      <div><dt>Ledger record</dt><dd>{_ledger_record_label(current)}</dd></div>
      {correction_fact}
      {run_number_fact}
      <div><dt>Run id</dt><dd><code>{_escape(row['run_id'])}</code></dd></div>
      <div><dt>Record id</dt><dd><code>{_escape(row['record_id'])}</code></dd></div>
      <div><dt>Validate path</dt><dd>{_escape(row['profile'])}</dd></div>
      <div><dt>Selection mode</dt><dd>{_escape(row['selection_mode'])}</dd></div>
      <div><dt>Whole-run result</dt><dd>{_escape(row['result'])}</dd></div>
      <div><dt>Hermit commit</dt><dd>{_hermit_commit_link(row['commit'])}</dd></div>
      <div><dt>Started</dt><dd>{_time_element(row['started_at'], 'validation run started_at')}</dd></div>
      <div><dt>Wall clock</dt><dd>{_escape(_duration_seconds(row.get('real_seconds'), 'validation run real_seconds'))}</dd></div>
      <div><dt>Ledger schema</dt><dd>{_escape(row['schema_version'])}</dd></div>
      <div><dt>History row digest</dt><dd><code>{_escape(row['history_row_sha256'])}</code></dd></div>
      <div><dt>Cell-results run id</dt><dd><code>{_escape(results['run_id'])}</code></dd></div>
      <div><dt>Cell-results Hermit SHA</dt><dd>{_hermit_commit_link(results['hermit_sha'])}</dd></div>
      <div><dt>{"Raw census digest" if row.get("derived_cell_results") is not None else "Population digest"}</dt><dd><code>{_escape(results['population_sha256'])}</code></dd></div>
      <div><dt>Source tree dirty</dt><dd>{_escape(str(results['source_tree_dirty']).lower())}</dd></div>
    </dl></section>"""
    content = (
        facts + _selected_groups(model, page, row) + _recorded_cells(model, page, row)
    )
    return _layout(
        page,
        run_label,
        "One admitted validation history row. Whole-run result and per-cell verdict remain separate.",
        content,
        (
            ("Overview", "index.html"),
            ("Validation runs", "runs/index.html"),
            (run_label, None),
        ),
        model.wording,
    )


_BROWSER_FLAG_KEYS = frozenset({"admitted", "measured", "never_measured", "state"})


def _browser_copy(value: Any) -> Any:
    """Copy JSON data, refusing unexpected reducer-only browser flags."""
    if isinstance(value, Mapping):
        result: dict[str, Any] = {}
        for raw_key, item in value.items():
            key = str(raw_key)
            if key in _BROWSER_FLAG_KEYS:
                raise RenderRefused(
                    f"browser record contains an unexpected {key!r} field"
                )
            result[key] = _browser_copy(item)
        return result
    if isinstance(value, (list, tuple)):
        return [_browser_copy(item) for item in value]
    return value


def _assert_browser_record(value: Any, label: str) -> None:
    if isinstance(value, Mapping):
        forbidden = _BROWSER_FLAG_KEYS.intersection(str(key) for key in value)
        if forbidden:
            raise RenderRefused(
                f"{label} contains reducer-only browser keys: {sorted(forbidden)!r}"
            )
        for key, item in value.items():
            _assert_browser_record(item, f"{label}.{key}")
    elif isinstance(value, (list, tuple)):
        for index, item in enumerate(value):
            _assert_browser_record(item, f"{label}[{index}]")


def _series_run_reference(
    model: SiteModel, cell: Mapping[str, Any], row: Mapping[str, Any]
) -> dict[str, Any] | None:
    if row["producer"] != "validate":
        return None
    event_id = str(row["event_id"])
    reference = model.event_run_references.get(event_id)
    if reference is None:
        raise RenderRefused(f"validate series event {event_id!r} has no exact run link")
    if (
        reference["run_id"] != row["run_id"]
        or reference["commit"] != row["series"]["tree"]
    ):
        raise RenderRefused(
            f"validate series event {event_id!r} disagrees with its run link"
        )
    return _browser_run_reference(reference)


def _browser_cell_record(model: SiteModel, cell: Mapping[str, Any]) -> dict[str, Any]:
    identity = str(cell["cell"])
    browser_data = _browser_copy(
        {key: value for key, value in cell.items() if key != "state"}
    )
    for field in ("latest_result", "latest_result_time", "latest_result_tree"):
        browser_data.setdefault(field, None)
    evidence = []
    for row in model.series_by_cell[identity]:
        item = _browser_copy(row)
        item["anchor"] = _fragment("event", str(row["event_id"]))
        item["raw_path"] = "data/runs.json.gz"
        run = _series_run_reference(model, cell, row)
        if run is not None:
            item["run"] = run
        evidence.append(item)
    # NEWEST FIRST. Owner directive 2026-09-16: every time-ordered list on this
    # site defaults to reverse chronological.
    #
    # This list has NO sort control, so its order is simply the order emitted
    # here, and that order was `model.series_by_cell`, which is built by
    # appending rows in captured input order. The ledger is append-only, so that
    # was chronological ASCENDING -- oldest evidence first on every cell page.
    #
    # Sorted on `emitted_at` rather than on the row's `observed_at`, and the
    # choice is load-bearing: both exist, and a descending sort on the wrong one
    # looks correct. The page renders `emitted_at` under the label "Recorded", so
    # it is the date a reader can actually see, and asking for newest first means
    # that date should order the list. `event_id` breaks ties so the order stays
    # total and the build stays byte-identical for identical input.
    evidence.sort(key=lambda item: (str(item["emitted_at"]), str(item["event_id"])), reverse=True)
    selection: dict[str, list[dict[str, Any]]] = {}
    for path, facts in sorted(cell["selected_by"].items()):
        selected_facts = []
        for fact in facts:
            key = (
                _selected_cell_identity(identity, fact, model.series_by_event),
                str(path),
                str(fact["run_id"]),
                str(fact["tree"]),
            )
            reference = model.selection_run_references.get(key)
            if reference is None:
                raise RenderRefused(
                    "a selected_by fact did not match one exact terminal validation row"
                )
            selected_fact = _browser_copy(fact)
            selected_fact["run"] = _browser_run_reference(reference)
            selected_facts.append(selected_fact)
        selection[str(path)] = selected_facts
    latest_divergence = _latest_divergence(model, cell)
    partial_credit = None
    if latest_divergence is not None:
        partial_credit = _red_cell_partial_credit(model, cell)
        if partial_credit["value"] is not None:
            partial_credit["value"] = _percentage(partial_credit["value"])
        latest_divergence = _browser_copy(latest_divergence)
        latest_divergence["coordinates"] = {
            field: None if value is None else str(value)
            for field, value in latest_divergence["coordinates"].items()
        }
        difference = latest_divergence["scheduler_turn_difference"]
        if difference["value"] is not None:
            difference["value"] = str(difference["value"])
    result = {
        "data": browser_data,
        "digest": _full_digest(identity),
        "evidence": evidence,
        "identity": identity,
        "kind": "cell",
        "latest_divergence": latest_divergence,
        "partial_credit": partial_credit,
        "raw_path": "data/cells.json.gz",
        "selection": selection,
    }
    _assert_browser_record(result, f"cell {identity!r}")
    return result


def _test_source_path(test: Mapping[str, Any]) -> str | None:
    """Return one manifest-owned repository path, never shell command text."""
    def repository_path(raw_path: Any) -> str | None:
        if not isinstance(raw_path, str) or re.fullmatch(
            r"[A-Za-z0-9._+@/-]+", raw_path
        ) is None:
            return None
        path = PurePosixPath(raw_path)
        if (
            path.is_absolute()
            or not path.parts
            or any(part in {"", ".", ".."} for part in path.parts)
            or path.parts[0] == ".git"
        ):
            return None
        return path.as_posix()

    program_path = repository_path(test.get("program"))
    if program_path is not None:
        return program_path
    direct = test.get("direct")
    if isinstance(direct, Mapping):
        raw_direct: str | None = None
        if direct.get("kind") == "shell" and isinstance(direct.get("command"), str):
            command = str(direct["command"])
            if re.fullmatch(r"\./[A-Za-z0-9._+@/-]+", command):
                raw_direct = command
        elif direct.get("kind") == "argv":
            argv = direct.get("argv")
            if (
                isinstance(argv, Sequence)
                and not isinstance(argv, (str, bytes))
                and argv
                and isinstance(argv[0], str)
                and re.fullmatch(r"\./[A-Za-z0-9._+@/-]+", argv[0])
            ):
                raw_direct = argv[0]
        if raw_direct is not None:
            return repository_path(raw_direct.removeprefix("./"))
    return None


def _test_source(model: SiteModel, test: Mapping[str, Any]) -> dict[str, str] | None:
    """Bind a source path to the newest selected result's exact Hermit tree."""
    path = _test_source_path(test)
    if path is None:
        return None
    observations = []
    for identity in test["cells"]:
        cell = model.cell_by_id.get(str(identity))
        if cell is None or _cell_state(cell) not in {"green", "red"}:
            continue
        recorded_at = cell.get("latest_result_time")
        commit = cell.get("latest_result_tree")
        if not isinstance(recorded_at, str) or not isinstance(commit, str):
            raise RenderRefused("test result source revision is incomplete")
        try:
            instant = datetime.fromisoformat(recorded_at.replace("Z", "+00:00"))
        except ValueError as error:
            raise RenderRefused("test result source timestamp is invalid") from error
        if instant.tzinfo is None or re.fullmatch(r"[0-9a-f]{40}", commit) is None:
            raise RenderRefused("test result source revision is invalid")
        observations.append((instant, commit))
    if not observations:
        return None
    latest = max(instant for instant, _commit in observations)
    commits = {commit for instant, commit in observations if instant == latest}
    if len(commits) != 1:
        raise RenderRefused("newest test results disagree on their Hermit revision")
    return {"commit": next(iter(commits)), "path": path}


def _browser_test_record(
    model: SiteModel,
    test: Mapping[str, Any],
    omission_ids: frozenset[str],
) -> dict[str, Any]:
    identity = str(test["id"])
    published = [str(cell) for cell in test["cells"] if str(cell) in model.cell_by_id]
    cell_digests = [
        _full_digest(identity) for identity in published if identity in model.cell_paths
    ]
    omitted = [str(cell) for cell in test["cells"] if str(cell) in omission_ids]
    if len(published) + len(omitted) != len(test["cells"]):
        raise RenderRefused(
            f"test {identity!r} does not reconcile to cells and omissions"
        )
    data = _browser_copy(test)
    data["source"] = _test_source(model, test)
    result = {
        "cell_digests": cell_digests,
        "data": data,
        "digest": _full_digest(identity),
        "identity": identity,
        "kind": "test",
        "omitted_cells": omitted,
        "raw_path": "data/tests.json.gz",
    }
    _assert_browser_record(result, f"test {identity!r}")
    return result


def _latest_series_rows(
    model: SiteModel, cell: Mapping[str, Any]
) -> tuple[Mapping[str, Any], ...] | None:
    """Resolve a result cell's reducer-selected execution to exact series rows."""
    if cell.get("latest_result") not in {"pass", "product_failure"}:
        return None
    raw_event_ids = cell.get("latest_event_ids")
    if (
        not isinstance(raw_event_ids, Sequence)
        or isinstance(raw_event_ids, (str, bytes))
        or not raw_event_ids
    ):
        return None
    event_ids = tuple(str(value) for value in raw_event_ids)
    if len(event_ids) != len(set(event_ids)):
        return None
    rows_by_event: dict[str, list[Mapping[str, Any]]] = {}
    for row in model.series_by_cell.get(str(cell["cell"]), ()):
        rows_by_event.setdefault(str(row["event_id"]), []).append(row)
    rows = []
    for event_id in event_ids:
        matches = rows_by_event.get(event_id, ())
        if len(matches) != 1:
            return None
        rows.append(matches[0])
    expected = {
        (
            str(row.get("emitted_at")),
            str(row.get("product_result")),
            str(row["series"].get("tree")),
        )
        for row in rows
    }
    if expected != {
        (
            str(cell.get("latest_result_time")),
            str(cell.get("latest_result")),
            str(cell.get("latest_result_tree")),
        )
    }:
        return None
    return tuple(rows)


def _exact_validation_cell(
    model: SiteModel,
    cell: Mapping[str, Any],
    event_id: str,
) -> Mapping[str, Any] | None:
    """Join one validate series event to its exact terminal per-cell verdict."""
    row = model.series_by_event.get(event_id)
    if row is None or row.get("producer") != "validate":
        return None
    reference = model.event_run_references.get(event_id)
    if reference is None:
        return None
    series = row.get("series")
    if not isinstance(series, Mapping):
        return None
    if (
        str(series.get("cell")) != str(cell["cell"])
        or reference.get("run_id") != row.get("run_id")
        or reference.get("commit") != series.get("tree")
    ):
        return None
    path = str(reference.get("selection"))
    raw_latest_event_ids = cell.get("latest_event_ids")
    if (
        not isinstance(raw_latest_event_ids, Sequence)
        or isinstance(raw_latest_event_ids, (str, bytes))
        or not raw_latest_event_ids
    ):
        return None
    latest_event_ids = tuple(str(value) for value in raw_latest_event_ids)
    if len(latest_event_ids) != len(set(latest_event_ids)):
        return None
    matching_facts = []
    for fact in cell.get("selected_by", {}).get(path, ()):
        raw_fact_event_ids = fact.get("event_ids")
        if (
            not isinstance(raw_fact_event_ids, Sequence)
            or isinstance(raw_fact_event_ids, (str, bytes))
            or not raw_fact_event_ids
        ):
            continue
        fact_event_ids = tuple(str(value) for value in raw_fact_event_ids)
        if (
            len(fact_event_ids) != len(set(fact_event_ids))
            or set(fact_event_ids) != set(latest_event_ids)
            or event_id not in fact_event_ids
            or fact.get("run_id") != row.get("run_id")
            or fact.get("tree") != series.get("tree")
        ):
            continue
        group_is_exact = True
        for grouped_event_id in fact_event_ids:
            grouped_row = model.series_by_event.get(grouped_event_id)
            grouped_reference = model.event_run_references.get(grouped_event_id)
            if (
                grouped_row is None
                or grouped_reference != reference
                or grouped_row.get("producer") != "validate"
                or grouped_row.get("run_id") != row.get("run_id")
                or not isinstance(grouped_row.get("series"), Mapping)
                or str(grouped_row["series"].get("cell")) != str(cell["cell"])
                or grouped_row["series"].get("tree") != series.get("tree")
            ):
                group_is_exact = False
                break
        if group_is_exact:
            matching_facts.append(fact)
    if len(matching_facts) != 1:
        return None
    # The projection publishes a row recorded under a retired Hermit test id
    # under its successor's cell, but the run's history verdict still names the
    # id the run recorded.
    recorded_identity = _selected_cell_identity(
        str(cell["cell"]), matching_facts[0], model.series_by_event
    )
    recorded_category = (
        cell.get("category")
        if recorded_identity == str(cell["cell"])
        else recorded_identity.split("/", 1)[0]
    )
    history_indices = [
        index
        for index, candidate_path in enumerate(model.history_paths)
        if candidate_path == reference.get("path")
    ]
    if len(history_indices) != 1:
        return None
    history = model.history_rows[history_indices[0]]
    if (
        history.get("run_id") != row.get("run_id")
        or history.get("commit") != series.get("tree")
        or _evidence_path(history) != path
        or history.get("history_row_sha256")
        != reference.get("history_row_sha256")
    ):
        return None
    verdict_rows = [
        candidate
        for candidate in _cell_evidence(history)["recorded"]["cells"]
        if _cell_identity(candidate) == recorded_identity
        and candidate.get("category") == recorded_category
        and candidate.get("lane") == cell.get("lane")
    ]
    return verdict_rows[0] if len(verdict_rows) == 1 else None


def _exact_validation_verdict(
    model: SiteModel, cell: Mapping[str, Any], event_id: str
) -> Mapping[str, Any] | None:
    recorded = _exact_validation_cell(model, cell, event_id)
    return None if recorded is None else recorded["cell_verdict"]


def _consistent_coordinate(
    rows: Sequence[Mapping[str, Any]], field: str
) -> int | None:
    values = []
    for row in rows:
        coordinates = row["series"].get("coordinates")
        if not isinstance(coordinates, Mapping):
            return None
        value = coordinates.get(field)
        minimum = 1 if field == "first_divergent_record" else 0
        if type(value) is not int or value < minimum:
            return None
        values.append(value)
    return values[0] if values and len(set(values)) == 1 else None


def _scheduler_turn_difference(
    model: SiteModel,
    cell: Mapping[str, Any],
    latest_rows: Sequence[Mapping[str, Any]],
    current_turn: int | None,
) -> dict[str, Any]:
    unavailable = {"value": None, "previous_commit": None}
    if current_turn is None:
        return {**unavailable, "reason": "the latest scheduler turn is unavailable"}
    producers = {str(row.get("producer")) for row in latest_rows}
    times = {str(row.get("emitted_at")) for row in latest_rows}
    trees = {str(row["series"].get("tree")) for row in latest_rows}
    if len(producers) != 1 or len(times) != 1 or len(trees) != 1:
        return {**unavailable, "reason": "the latest recorded execution is ambiguous"}
    producer = next(iter(producers))
    current_time = datetime.fromisoformat(next(iter(times)).replace("Z", "+00:00"))
    current_tree = next(iter(trees))
    raw_qualifying = cell.get("qualifying_event_ids")
    if (
        not isinstance(raw_qualifying, Sequence)
        or isinstance(raw_qualifying, (str, bytes))
    ):
        return {**unavailable, "reason": "qualifying evidence is unavailable"}
    qualifying = {str(event_id) for event_id in raw_qualifying}
    candidates = []
    for row in model.series_by_cell[str(cell["cell"])]:
        if (
            str(row.get("event_id")) not in qualifying
            or row.get("producer") != producer
            or row.get("product_result") != "product_failure"
            or row["series"].get("outcome") != "diverged"
            or row["series"].get("tree") == current_tree
        ):
            continue
        observed = datetime.fromisoformat(
            str(row["emitted_at"]).replace("Z", "+00:00")
        )
        if observed < current_time:
            candidates.append((observed, row))
    if not candidates:
        return {
            **unavailable,
            "reason": "no previous recorded commit from the same producer has an earlier observation",
        }
    previous_time = max(value[0] for value in candidates)
    previous_rows = [row for observed, row in candidates if observed == previous_time]
    previous_trees = {str(row["series"].get("tree")) for row in previous_rows}
    previous_turn = _consistent_coordinate(
        previous_rows, "first_divergent_scheduler_turn"
    )
    if len(previous_trees) != 1 or previous_turn is None:
        return {
            **unavailable,
            "reason": "the previous recorded commit does not identify one scheduler turn",
        }
    return {
        "previous_commit": next(iter(previous_trees)),
        "reason": None,
        "value": current_turn - previous_turn,
    }


def _latest_divergence(
    model: SiteModel, cell: Mapping[str, Any]
) -> dict[str, Any] | None:
    if cell.get("latest_result") != "product_failure":
        return None
    rows = _latest_series_rows(model, cell)
    if rows is None:
        return {
            "coordinates": {field: None for field, _label in DIVERGENCE_COORDINATES},
            "scheduler_turn_difference": {
                "previous_commit": None,
                "reason": "the latest result does not identify one recorded execution",
                "value": None,
            },
        }
    coordinates = {
        field: _consistent_coordinate(rows, field)
        for field, _label in DIVERGENCE_COORDINATES
    }
    return {
        "coordinates": coordinates,
        "scheduler_turn_difference": _scheduler_turn_difference(
            model,
            cell,
            rows,
            coordinates["first_divergent_scheduler_turn"],
        ),
    }


def _recent_passing_execution(
    model: SiteModel, cell: Mapping[str, Any]
) -> dict[str, Any] | None:
    """Return counts only for the exact validate-to-verdict join we can prove."""
    if cell.get("latest_result") != "pass":
        return None
    rows = _latest_series_rows(model, cell)
    if rows is None or len(rows) != 1:
        return None
    row = rows[0]
    event_id = str(row["event_id"])
    if row.get("producer") != "validate" or row.get("product_result") != "pass":
        return None
    reference = model.event_run_references.get(event_id)
    verdict = _exact_validation_verdict(model, cell, event_id)
    if reference is None or verdict is None:
        return None
    comparison = verdict.get("comparison")
    counts = verdict.get("compared_log_messages")
    if (
        verdict.get("state") != "compared-and-matched"
        or verdict.get("bitwise_parity") is not True
        or not isinstance(comparison, Mapping)
        or comparison.get("log_scope") != "info"
        or not isinstance(counts, Mapping)
    ):
        return None
    left = counts.get("left")
    right = counts.get("right")
    if (
        type(left) is not int
        or type(right) is not int
        or left <= 0
        or right <= 0
        or left != right
    ):
        return None

    runtime = row["series"].get("runtime")
    if not isinstance(runtime, Mapping):
        return None
    syscall_counts: list[int] = []
    for name in ("run1", "run2"):
        measurement = runtime.get(name)
        if not isinstance(measurement, Mapping):
            return None
        syscalls = measurement.get("syscalls")
        if type(syscalls) is not int or syscalls < 0:
            return None
        syscall_counts.append(syscalls)

    series = row["series"]
    result: dict[str, Any] = {
        "attempt": series.get("attempt"),
        "emitted_at": row["emitted_at"],
        "event_id": event_id,
        "info_messages_run1": left,
        "info_messages_run2": right,
        "run": _browser_run_reference(reference),
        "run_index": series["run_index"],
        "syscalls_run1": syscall_counts[0],
        "syscalls_run2": syscall_counts[1],
    }
    return result


def _browser_summary(model: SiteModel, omission_ids: frozenset[str]) -> dict[str, Any]:
    omissions = tuple(model.data["projection_audit"]["deliberate_omissions"])
    omission_by_test = Counter(str(item["test"]) for item in omissions)
    cells = [
        {
            "backend": cell["backend"],
            "category": cell["category"],
            "cell": cell["cell"],
            "digest": _full_digest(str(cell["cell"])),
            "lane": cell["lane"],
            "latest_result": cell.get("latest_result"),
            "latest_result_time": cell.get("latest_result_time"),
            "latest_result_tree": cell.get("latest_result_tree"),
            "mode": cell["mode"],
            "not_selected_by_full_reason": cell["not_selected_by_full_reason"],
            "physical_row_count": cell["physical_row_count"],
            "recent_passing_execution": _recent_passing_execution(model, cell),
            "qualifying_physical_row_count": cell["qualifying_physical_row_count"],
            "qualifying_represented_run_count": cell[
                "qualifying_represented_run_count"
            ],
            "represented_run_count": cell["represented_run_count"],
            "selected_by_full": cell["selected_by_full"],
            "selected_by_paths": sorted(str(path) for path in cell["selected_by"]),
            "series_filter_facets": [
                _series_filter_evidence(row)
                for row in model.series_by_cell[str(cell["cell"])]
            ],
            "test": cell["test"],
            "url": model.cell_paths.get(str(cell["cell"])),
        }
        for cell in model.cells
    ]
    tests = []
    for test in model.tests:
        identity = str(test["id"])
        published = [
            str(cell) for cell in test["cells"] if str(cell) in model.cell_by_id
        ]
        omitted_count = omission_by_test[identity]
        if len(published) + omitted_count != len(test["cells"]):
            raise RenderRefused(f"test {identity!r} cell counts do not reconcile")
        tests.append(
            {
                "category": test["category"],
                "digest": _full_digest(identity),
                "id": identity,
                "lane": test["lane"],
                "manifest_cell_count": len(test["cells"]),
                "omission_count": omitted_count,
                "published_cell_count": len(published),
                "qualifying_zero_cell_count": sum(
                    model.cell_by_id[cell]["qualifying_physical_row_count"] == 0
                    for cell in published
                ),
                "url": model.test_paths[identity],
            }
        )
    runs = []
    for index, row in enumerate(model.history_rows):
        selected = _cell_evidence(row)["selected_by"].get(
            row["profile"], {"cell_count": None}
        )
        run: dict[str, Any] = {
            "commit": row["commit"],
            "digest": PurePosixPath(model.history_paths[index]).stem,
            "finished_at": row["finished_at"],
            "history_row_sha256": row["history_row_sha256"],
            "path": model.history_paths[index],
            "record": (
                "current" if index in model.current_history_indices else "historical"
            ),
            "recorded_cell_count": _cell_evidence(row)["recorded"]["cell_count"],
            "result": row["result"],
            "run_id": row["run_id"],
            "selected_cell_count": selected["cell_count"],
            "selection": row["profile"],
            "started_at": row["started_at"],
        }
        run_number = _positive_run_number(row)
        if run_number is not None:
            run["run_number"] = str(run_number)
        runs.append(run)
    if (
        len(model.cells) + len(omissions)
        != model.data["counts"]["in_manifest_cell_count"]
    ):
        raise RenderRefused("browser cell and omission counts do not reconcile")
    browser_omissions = []
    for omission in omissions:
        item = _browser_copy(omission)
        item["anchor"] = _fragment("omission", str(omission["cell"]))
        item["display_reason"] = _omission_summary(model, omission)
        for reference in item["event_references"]:
            reference["anchor"] = _fragment("event", str(reference["event_id"]))
        browser_omissions.append(item)
    result = {
        "cells": cells,
        "backend_history": _backend_history(model),
        "counts": _browser_copy(model.data["counts"]),
        "custom_commands": _browser_copy(model.custom_commands),
        "omissions": browser_omissions,
        "projection_audit": _browser_copy(model.data["projection_audit"]),
        "provenance": _browser_copy(model.data["provenance"]),
        "raw_artifacts": [
            {"label": "Stage 1 build", "path": "build.json"},
            {"label": "Stage 1 cells", "path": "data/cells.json.gz"},
            {"label": "Stage 1 runs", "path": "data/runs.json.gz"},
            {"label": "Stage 1 tests", "path": "data/tests.json.gz"},
        ],
        "recorded_outside_manifest": _browser_copy(model.recorded_outside_manifest),
        "runs": runs,
        "schema_version": BROWSER_SCHEMA_VERSION,
        "tests": tests,
    }
    _assert_browser_record(result, "site summary")
    return result


def _browser_json_bytes(value: Any) -> bytes:
    writer = _writer()
    try:
        return writer._json_bytes(value) + b"\n"
    except writer.WriteRefused as error:
        raise RenderRefused(
            f"browser payload is not canonical JSON: {error}"
        ) from error


def _browser_data_files(model: SiteModel) -> dict[str, bytes]:
    omissions = tuple(model.data["projection_audit"]["deliberate_omissions"])
    omission_ids = frozenset(str(item["cell"]) for item in omissions)
    if omission_ids.intersection(model.cell_by_id):
        raise RenderRefused("browser cells and deliberate omissions overlap")
    if omission_ids | set(model.cell_by_id) != model.manifest_cell_ids:
        raise RenderRefused(
            "browser cells and deliberate omissions do not exhaust manifest"
        )

    records: list[dict[str, Any]] = []
    records.extend(
        _browser_cell_record(model, cell)
        for cell in model.cells
        if str(cell["cell"]) in model.cell_paths
    )
    records.extend(
        _browser_test_record(model, test, omission_ids) for test in model.tests
    )
    shards: dict[str, list[dict[str, Any]]] = {
        prefix: [] for prefix in "0123456789abcdef"
    }
    for record in sorted(
        records, key=lambda item: (str(item["digest"]), str(item["kind"]))
    ):
        digest = str(record["digest"])
        if HEX_SHA256.fullmatch(digest) is None:
            raise RenderRefused("browser detail record digest is invalid")
        shards[digest[0]].append(record)

    decoded: dict[str, bytes] = {
        "data/site-summary.json.gz": _browser_json_bytes(
            _browser_summary(model, omission_ids)
        )
    }
    for prefix, shard_records in shards.items():
        decoded[f"data/detail-{prefix}.json.gz"] = _browser_json_bytes(
            {
                "records": shard_records,
                "schema_version": BROWSER_SCHEMA_VERSION,
            }
        )
    artifacts = [
        {
            "decoded_bytes": len(value),
            "decoded_sha256": hashlib.sha256(value).hexdigest(),
            "path": path,
        }
        for path, value in sorted(decoded.items())
    ]
    manifest = _browser_json_bytes(
        {"artifacts": artifacts, "schema_version": BROWSER_SCHEMA_VERSION}
    )
    writer = _writer()
    return {
        "data/site-manifest.json.gz": writer._gzip_bytes(manifest),
        **{path: writer._gzip_bytes(value) for path, value in sorted(decoded.items())},
    }


def _detail_shell(model: SiteModel, kind: str) -> bytes:
    if kind == "cell":
        page = "cells/detail.html"
        title = "Cell detail"
        lead = (
            "One manifest cell, its recorded evidence, and separate qualifying counts."
        )
        breadcrumbs = (
            ("Overview", "index.html"),
            ("Cells", "cells/index.html"),
            (title, None),
        )
    elif kind == "test":
        page = "tests/detail.html"
        title = "Test detail"
        lead = "One manifest test and its cells."
        breadcrumbs = (
            ("Overview", "index.html"),
            ("Tests", "index.html#tests"),
            (title, None),
        )
    else:
        raise RenderRefused(f"unsupported detail shell {kind!r}")
    fallback_provenance = "" if model.wording.disclosure else _provenance(model, page)
    content = f"""
    <section id="browser-detail" data-detail-kind="{_escape(kind)}">
      <p class="data-status" data-load-status data-load-state="loading" role="status">Loading verified local data…</p>
      <div data-detail-content></div>
      <noscript><p class="notice">JavaScript is required to open this query-addressed detail.</p></noscript>
    </section>
    {fallback_provenance}"""
    return _layout(
        page,
        title,
        lead,
        content,
        breadcrumbs,
        model.wording,
        page_kind=f"{kind}-detail",
        show_page_heading=False,
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


def _site_javascript() -> bytes:
    try:
        value = SITE_JS_PATH.read_bytes()
    except OSError as error:
        raise RenderRefused(f"cannot read {SITE_JS_PATH}: {error}") from error
    lowered = value.lower()
    hermit_commit_base = b"https://github.com/rrnewton/hermit/commit/"
    if lowered.count(hermit_commit_base) != 1:
        raise RenderRefused(
            "site JavaScript must contain exactly one reviewed Hermit commit link base"
        )
    audited = lowered.replace(hermit_commit_base, b"")
    forbidden = (
        b"http:",
        b"https:",
        b"xmlhttprequest",
        b"websocket",
        b"innerhtml",
        b"outerhtml",
        b"insertadjacenthtml",
        b"document.write",
        b"eval(",
    )
    if not value or any(token in audited for token in forbidden):
        raise RenderRefused("site JavaScript is empty or uses an unsafe API")
    if b"textcontent" not in lowered:
        raise RenderRefused(
            "site JavaScript does not use textContent for visible updates"
        )
    return value


def _site_favicon() -> bytes:
    try:
        value = FAVICON_PATH.read_bytes()
    except OSError as error:
        raise RenderRefused(f"cannot read {FAVICON_PATH}: {error}") from error
    if hashlib.sha256(value).hexdigest() != FAVICON_SHA256:
        raise RenderRefused("site favicon bytes do not match the reviewed asset")
    return value


def render_site(
    build: Mapping[str, Any], data: Mapping[str, Any], *, wording: str = "fixture",
    presentation: Mapping[str, Any] | None = None,
) -> dict[str, bytes]:
    model = _site_model(build, data, wording=wording)
    model = replace(model, presentation=validate_presentation(presentation, model.data))
    files = {
        "assets/favicon.svg": _site_favicon(),
        "assets/site.css": _site_css(),
        "assets/site.js": _site_javascript(),
        "cells/detail.html": _detail_shell(model, "cell"),
        "cells/index.html": _cells_index(model),
        "cells/never-measured.html": _never_measured_index(model),
        "cells/projection-audit.html": _projection_audit_page(model),
        "index.html": _landing_page(model),
        "runs/index.html": _runs_index(model),
        "tests/custom-commands.html": _custom_commands_page(model),
        "tests/detail.html": _detail_shell(model, "test"),
        **_browser_data_files(model),
    }
    result_cells = tuple(
        cell for cell in model.cells if _cell_state(cell) in {"green", "red"}
    )
    never_measured_cells = tuple(
        cell for cell in model.cells if _cell_state(cell) == "never-measured"
    )
    for title, lead, explicit_never_measured, cells in (
        ("Cells", model.wording.cells_lead, False, result_cells),
        (
            "Never measured",
            "Cells in the manifest for which no qualifying admitted row exists.",
            True,
            never_measured_cells,
        ),
    ):
        page_count = (len(cells) + LIST_PAGE_SIZE - 1) // LIST_PAGE_SIZE
        for page_number in range(2, page_count + 1):
            path = _cell_listing_path(
                explicit_never_measured=explicit_never_measured,
                page_number=page_number,
            )
            files[path] = _static_cells_listing_page(
                model,
                title=title,
                lead=lead,
                cells=cells,
                explicit_never_measured=explicit_never_measured,
                page_number=page_number,
            )
    for index, row in enumerate(model.history_rows):
        files[model.history_paths[index]] = _run_page(model, index, row)
    files = dict(sorted(files.items()))
    retired_placeholder = b"Unavail" + b"able"
    if any(
        retired_placeholder in value
        for path, value in files.items()
        if path.endswith(".html")
    ):
        raise RenderRefused("rendered output contains a retired placeholder")
    return files


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
four-file Stage 1 artifact tree, writes only local JavaScript, performs no
network request, and refuses an existing output directory.

Example:
  python3 ci-hub/compatibility-website/render_page.py \\
    --artifacts ci-hub/tests/fixtures/compatibility-website-stage-1 \\
    --output /tmp/hermit-compatibility-prototype""",
    )
    parser.add_argument(
        "--artifacts",
        required=True,
        type=Path,
        help="exact four-file Stage 1 fixture artifact directory",
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
