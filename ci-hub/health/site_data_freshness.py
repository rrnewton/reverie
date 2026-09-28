#!/usr/bin/env python3
"""Warn when the compatibility website publishes stale data or silently drops rows.

WHY THIS EXISTS. https://github.com/rrnewton/dev-hermit/issues/394, round-1
review. site_rebuild_health.py asks whether hermit's docs.yml rebuild SUCCEEDS.
It does not ask what the rebuild published: on 2026-09-28 every rebuild was
green while https://rrnewton.github.io/hermit/compatibility/latest/ showed
measurements from 2026-09-22 next to validation history from 2026-09-28. And
since dev-hermit d262eae6bc a series row whose cell is missing from the
builder's manifest is excluded by name instead of refusing the build, and
nothing counted those exclusions.

WHAT IT READS. Only machine-readable artifacts of the PUBLISHED site, never page
text:
  build.json                 build start time, the SHA-256 of every data
                             artifact, row counts, and the provenance of every
                             ledger file the build read (path, line count,
                             SHA-256) plus its finalized-proof gaps;
  data/site-summary.json.gz  per-cell latest result and time, admitted runs;
  data/runs.json.gz          the projection exclusion audit (streamed; only
                             the small block of counts is parsed).
Both data files must match the SHA-256 that build.json records for them. The
exclusion counts must add up to build.json's series schema counts, and the
measured cells must equal build.json's green plus red counts; any mismatch is
`unknown`, never `current`.

It compares that with the validation ledger in the tool root (the fresh
origin/main checkout tick-hub runs every gate in).

WHAT IT MEASURES.
  data as of   the newest `latest_result_time` over published cells with a
               pass or product-failure result. The page shows the same value as
               "Latest selected measurement".
  candidates   terminal validation rows in ledger/hermit/*/*.jsonl that the
               site's admission accepts in form -- a clean, commit-anchored
               schema-5 full/full run with a non-empty raw census, or a
               schema-7..10 row carrying cell results, never a Buck release
               payload (calibrated below, in _admissible) -- that finished after
               the data-as-of time and that this build could have read: the
               build's provenance proves it read that line (the tool-root file's
               prefix is checked byte-for-byte against the recorded SHA-256), or
               the run finished at least --ingest-grace-hours before the build
               started. A candidate counts only when its Hermit commit is on
               main, as the site requires: a recorded post-run main distance of
               0, or GitHub's compare API (at most --max-compare calls).
  lag          candidate finish time minus data as of. A series measurement can
               be admitted only through its terminal row, so the newest
               admissible terminal row bounds the newest admissible measurement.
  exclusions   series rows the build excluded, by reason: no admitted terminal
               row, series schema older than stress-series/v3, and cell not in
               the builder's manifest. Reasons overlap on one row, so the
               categories are reported side by side and are not summed; the
               distinct excluded total is reported beside them.

STATES.
  current                 no main candidate is more than --max-lag-hours
                          newer than the data, and no row is excluded for a
                          cell missing from the manifest.
  stale                   a main candidate that the build could have admitted
                          is more than --max-lag-hours newer than the data.
  not-in-manifest         at least one row is excluded for a cell missing from
                          the builder's manifest.
  stale+not-in-manifest   both.
  unknown                 the site or its artifacts could not be read or did
                          not agree with each other, or a candidate over the
                          limit had undeterminable main ancestry. Exit 75.

FIRING (site_gate_policy.py). With --gate every evaluation in a warning state
exits 1, so tick-hub pages on every evaluation (cadence 1800, the 30-minute
tick; at least hourly) for as long as the condition lasts, and each page says
since when. Unknown exits 75 twice and
fires state `unknown` from the third consecutive evaluation.

    site_data_freshness.py                  # report, exit 0 (75 if unknown)
    site_data_freshness.py --json           # everything it measured
    site_data_freshness.py --gate --state S # the tick-hub form
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import subprocess
import sys
import time
import zlib
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
import site_gate_policy as policy  # noqa: E402
from site_gate_policy import locked_state  # noqa: E402

QUIET = 0
FIRE = 1
UNKNOWN = 75  # tick-hub's NO_RESULT_EXIT: the gate ran and could not decide.

SITE_URL = "https://rrnewton.github.io/hermit/compatibility/latest/"
DEFAULT_REPO = "rrnewton/hermit"
BUILD = "build.json"
SUMMARY = "data/site-summary.json.gz"
RUNS = "data/runs.json.gz"
# Download caps, about four times the 2026-09-28 sizes (19.5 MB, 1.1 MB, 14.7 MB).
CAPS = {BUILD: 96 << 20, SUMMARY: 16 << 20, RUNS: 96 << 20}
# The exclusion block of runs.json (37 entries, 5.4 KB on 2026-09-28).
MAX_BLOCK = 16 << 20

DEFAULT_MAX_LAG_HOURS = 24.0
DEFAULT_INGEST_GRACE_HOURS = 6.0
DEFAULT_MAX_COMPARE = 6
# The whole evaluation fits inside the reminder's 180 s gate timeout, so the
# gate reaches a verdict (or its own `unknown`) instead of being killed: three
# downloads of at most FETCH_SECS each, then compare calls of at most
# COMPARE_SECS each, started only while COMPARE_SECS remain in the budget.
# Measured 2026-09-28 from devbig014: 3.1 s for the whole evaluation.
DEFAULT_TIME_BUDGET_SECS = 150
FETCH_SECS = 45
COMPARE_SECS = 15

# Reason strings, verbatim from ci-hub/lib/compatibility_website.rs
# series_exclusion_reasons().
NOT_IN_MANIFEST_PREFIX = "cell is not in the website builder's manifest (not yet in its cell universe): "
SCHEMA_REASON = "series schema is not stress-series/v3"
NO_TERMINAL_REASON = "validate series row has no admitted terminal validation row"
BLOCK_START = b',"projection_admitted_physical_row_count":'
BLOCK_END = b',"projection_results":'
BLOCK_KEYS = frozenset(
    {
        "projection_admitted_physical_row_count",
        "projection_admitted_represented_run_count",
        "projection_excluded_physical_row_count",
        "projection_excluded_represented_run_count",
        "projection_exclusion_counts",
    }
)
MEASURED_RESULTS = frozenset({"pass", "product_failure"})

Fetcher = Callable[[str, int], "bytes | str"]
Runner = Callable[[list[str]], "tuple[int, str, str]"]


class Unknown(Exception):
    """The gate could not establish a verdict."""


def _fetch(url: str, cap: int) -> bytes | str:
    # Egress on the tick hosts goes through fwdproxy; a scheduled unit inherits
    # no proxy variables, so the wrapper is applied here rather than by callers.
    cmd = ["with-proxy", "curl", "-fsSL", "--max-time", str(FETCH_SECS), "--max-filesize", str(cap), url]
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=FETCH_SECS + 15)
    except (OSError, subprocess.SubprocessError) as exc:
        return f"could not fetch {url}: {exc}"
    if p.returncode != 0:
        return f"could not fetch {url}: curl rc={p.returncode} {p.stderr.decode(errors='replace').strip()[:200]}"
    return p.stdout


def _run(cmd: list[str], timeout: int = COMPARE_SECS) -> tuple[int, str, str]:
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except (OSError, subprocess.SubprocessError) as exc:  # fail closed
        return 1, "", str(exc)


def _hours(later: dt.datetime, earlier: dt.datetime) -> float:
    return round((later - earlier).total_seconds() / 3600.0, 1)


def _get(fetch: Fetcher, site_url: str, path: str) -> bytes:
    url = site_url.rstrip("/") + "/" + path
    body = fetch(url, CAPS[path])
    if isinstance(body, str):
        raise Unknown(body)
    return body


def _verified(build: dict, path: str, body: bytes) -> bytes:
    """Return `body` only if build.json records exactly these bytes for `path`."""
    entries = [a for a in build.get("artifacts") or [] if isinstance(a, dict) and a.get("path") == path]
    if len(entries) != 1:
        raise Unknown(f"build.json records {len(entries)} artifact entries for {path}")
    digest = hashlib.sha256(body).hexdigest()
    if entries[0].get("sha256") != digest or entries[0].get("bytes") != len(body):
        raise Unknown(
            f"{path} is not the artifact build.json records "
            f"(published sha256 {digest[:12]}, {len(body)} bytes; recorded "
            f"{str(entries[0].get('sha256'))[:12]}, {entries[0].get('bytes')} bytes)"
        )
    return body


def exclusion_block(gz_body: bytes) -> dict:
    """Stream-decompress runs.json and parse only its exclusion-count block."""
    decompressor = zlib.decompressobj(wbits=31)
    buffer = b""
    found = False
    step = 1 << 20
    try:
        for offset in range(0, len(gz_body), step):
            buffer += decompressor.decompress(gz_body[offset : offset + step])
            if not found:
                at = buffer.find(BLOCK_START)
                if at < 0:
                    buffer = buffer[-(len(BLOCK_START) - 1) :]
                    continue
                buffer = buffer[at + 1 :]
                found = True
            end = buffer.find(BLOCK_END)
            if end >= 0:
                block = json.loads(b"{" + buffer[:end] + b"}")
                break
            if len(buffer) > MAX_BLOCK:
                raise Unknown("runs.json exclusion block is larger than 16 MiB")
        else:
            raise Unknown("runs.json does not carry the projection exclusion block")
    except (zlib.error, ValueError) as exc:
        raise Unknown(f"runs.json exclusion block did not parse: {exc}") from exc
    if set(block) != BLOCK_KEYS or not isinstance(block["projection_exclusion_counts"], list):
        raise Unknown(f"runs.json exclusion block has keys {sorted(block)}")
    return block


def categorize(block: dict) -> dict:
    series = {"no_admitted_terminal": 0, "schema": 0, "not_in_manifest": 0}
    not_in_manifest_cells: dict[str, int] = {}
    other: dict[str, int] = {}
    history: dict[str, int] = {}
    for entry in block["projection_exclusion_counts"]:
        if not isinstance(entry, dict):
            raise Unknown("runs.json exclusion entry is not an object")
        reason, schema, rows = entry.get("reason"), entry.get("schema"), entry.get("physical_row_count")
        if not isinstance(reason, str) or not isinstance(schema, str) or not isinstance(rows, int):
            raise Unknown(f"runs.json exclusion entry is malformed: {entry}")
        if reason.startswith(NOT_IN_MANIFEST_PREFIX):
            series["not_in_manifest"] += rows
            cell = reason[len(NOT_IN_MANIFEST_PREFIX):]
            not_in_manifest_cells[cell] = not_in_manifest_cells.get(cell, 0) + rows
        elif not schema.startswith("stress-series/"):
            history[reason] = history.get(reason, 0) + rows
        elif reason == SCHEMA_REASON:
            series["schema"] += rows
        elif reason == NO_TERMINAL_REASON:
            series["no_admitted_terminal"] += rows
        else:
            other[reason] = other.get(reason, 0) + rows
    return {
        **series,
        "not_in_manifest_cells": dict(sorted(not_in_manifest_cells.items())),
        "other_series_reasons": dict(sorted(other.items())),
        "validation_history_reasons": dict(sorted(history.items())),
        "excluded_total": block["projection_excluded_physical_row_count"],
        "admitted_total": block["projection_admitted_physical_row_count"],
    }


def data_as_of(summary: dict, build: dict) -> tuple[dt.datetime | None, int]:
    cells = summary.get("cells")
    if not isinstance(cells, list):
        raise Unknown("site-summary.json has no cells list")
    times: list[dt.datetime] = []
    for cell in cells:
        if not isinstance(cell, dict) or cell.get("latest_result") not in MEASURED_RESULTS:
            continue
        moment = policy.parse_time(cell.get("latest_result_time"))
        if moment is None:
            raise Unknown(f"measured cell {cell.get('cell')!r} has no latest_result_time")
        times.append(moment)
    counts = build.get("counts") or {}
    expected = (counts.get("green_cell_count") or 0) + (counts.get("red_cell_count") or 0)
    if len(times) != expected:
        raise Unknown(
            f"site-summary.json has {len(times)} measured cells; build.json counts "
            f"{expected} green plus red"
        )
    return (max(times) if times else None), len(times)


def _admissible(row: dict) -> bool:
    """The form checks the site applies to a terminal validation row.

    Mirrors ci-hub/lib/compatibility_census.rs original_census() for schema 5
    and history_exclusion_reasons() in ci-hub/lib/compatibility_website.rs for
    schemas 7 to 10. A non-Cargo (Buck) release payload is never a candidate.
    Calibrated 2026-09-28 against the published build: every run the site
    admitted from the rows it read passes, and every passing row the site did
    not admit is one of its finalized-proof gaps.
    """
    legacy = row.get("legacy_row")
    if row.get("event_type") != "run.result" or row.get("team") != "hermit" or not isinstance(legacy, dict):
        return False

    def present(field: str) -> bool:
        value = legacy.get(field)
        return isinstance(value, str) and bool(value.strip())

    if (
        legacy.get("repo") not in (None, "hermit", "rrnewton/hermit")
        or legacy.get("release_builder") not in (None, "cargo")
        or not present("run_id")
        or not present("started_at")
        or policy.parse_time(legacy.get("finished_at")) is None
    ):
        return False
    schema = legacy.get("schema_version")
    if schema == 5:
        census = legacy.get("raw_result_input_census_v1")
        files = census.get("files") if isinstance(census, dict) else None
        return (
            legacy.get("repo") is not None
            and legacy.get("cell_results") is None
            and legacy.get("commit_anchored") is True
            and legacy.get("tree_dirty") is False
            and legacy.get("profile") == "full"
            and legacy.get("selection_mode") == "full"
            and legacy.get("result") in ("pass", "fail", "no_result")
            and isinstance(files, list)
            and any(isinstance(f, dict) and f.get("rows") for f in files)
        )
    if schema in (7, 8, 9, 10):
        return (
            isinstance(legacy.get("cell_results"), dict)
            and bool(legacy["cell_results"])
            and present("record_id")
            and present("profile")
            and present("selection_mode")
            and present("result")
        )
    return False


def _recorded_main(legacy: dict) -> bool:
    observation = legacy.get("main_observation")
    if isinstance(observation, dict) and observation.get("ahead") == 0:
        return True
    return legacy.get("git_ahead") == 0


def _read_sources(build: dict, root: Path) -> dict[str, dict]:
    """Which tool-root ledger files are provably extensions of what the build read."""
    provenance = (build.get("provenance") or {}).get("canonical_input_provenance") or {}
    sources = provenance.get("sources")
    if not isinstance(sources, list):
        raise Unknown("build.json has no canonical input sources")
    out: dict[str, dict] = {}
    for source in sources:
        if not isinstance(source, dict) or source.get("store") != "validation-ledger":
            continue
        path = str(source.get("path"))
        if source.get("location") != "published" or not path.startswith("ledger/hermit/"):
            continue
        entry = {"line_count": source.get("line_count"), "prefix_verified": False}
        local = root / path
        try:
            data = local.read_bytes()
        except OSError:
            out[path] = entry
            continue
        size = source.get("bytes")
        if isinstance(size, int) and len(data) >= size:
            prefix = data[:size]
            if (
                hashlib.sha256(prefix).hexdigest() == source.get("sha256")
                and prefix.count(b"\n") == source.get("line_count")
            ):
                entry["prefix_verified"] = True
        out[path] = entry
    return out


def candidates(
    root: Path,
    build: dict,
    summary: dict,
    as_of: dt.datetime | None,
    build_started: dt.datetime,
    grace_hours: float,
) -> tuple[list[dict], dict[str, dict]]:
    ledger = root / "ledger" / "hermit"
    if not ledger.is_dir():
        raise Unknown(f"tool root {root} has no ledger/hermit directory")
    read = _read_sources(build, root)
    admitted = {str(r.get("run_id")) for r in summary.get("runs") or [] if isinstance(r, dict)}
    proof = ((build.get("provenance") or {}).get("canonical_input_provenance") or {}).get("finalized_proof") or {}
    unfinalized = {
        str(m.get("run_id")) for m in proof.get("missing_current_terminals") or [] if isinstance(m, dict)
    }
    floor_month = as_of.strftime("%Y-%m") if as_of else ""
    grace_edge = build_started - dt.timedelta(hours=grace_hours)
    found: list[dict] = []
    for path in sorted(ledger.glob("*/*.jsonl")):
        if path.stem < floor_month:
            continue
        relative = path.relative_to(root).as_posix()
        source = read.get(relative)
        with path.open("rb") as handle:
            for index, line in enumerate(handle):
                try:
                    row = json.loads(line)
                except ValueError as exc:
                    raise Unknown(f"{relative} line {index + 1} does not parse: {exc}") from exc
                if not isinstance(row, dict) or not _admissible(row):
                    continue
                legacy = row["legacy_row"]
                finished = policy.parse_time(legacy["finished_at"])
                assert finished is not None
                if as_of is not None and finished <= as_of:
                    continue
                if finished >= build_started:
                    continue
                if source is None:
                    was_read: bool | None = False
                elif source["prefix_verified"]:
                    was_read = index < int(source["line_count"])
                else:
                    was_read = None
                if not (was_read is True or finished <= grace_edge):
                    continue
                run_id = str(legacy.get("run_id") or row.get("run_id"))
                if run_id in admitted:
                    status = "admitted"
                elif run_id in unfinalized:
                    status = "unfinalized"
                elif was_read is True:
                    status = "read-not-admitted"
                elif was_read is False:
                    status = "not-read"
                else:
                    status = "unverified-read"
                found.append(
                    {
                        "path": relative,
                        "line": index + 1,
                        "run_id": run_id,
                        "run_number": row.get("run_number") or legacy.get("run_number"),
                        "commit": str(row.get("commit") or legacy.get("commit")),
                        "schema_version": legacy.get("schema_version"),
                        "selection_mode": legacy.get("selection_mode"),
                        "finished_at": policy.iso(finished),
                        "recorded_main": _recorded_main(legacy),
                        "site_status": status,
                    }
                )
    found.sort(key=lambda c: c["finished_at"], reverse=True)
    return found, read


def main_ancestor(repo: str, commit: str, run: Runner) -> bool | None:
    rc, out, _err = run(["with-proxy", "gh", "api", f"repos/{repo}/compare/{commit}...main", "--jq", ".status"])
    if rc != 0:
        return None
    status = out.strip()
    if status in ("ahead", "identical"):
        return True
    if status in ("behind", "diverged"):
        return False
    return None


def measure(
    *,
    site_url: str,
    root: Path,
    repo: str,
    now: dt.datetime,
    max_lag_hours: float,
    grace_hours: float,
    max_compare: int,
    fetch: Fetcher = _fetch,
    run: Runner = _run,
    time_left: Callable[[], float] = lambda: float("inf"),
) -> dict:
    m: dict = {
        "measured_at": policy.iso(now),
        "site_url": site_url,
        "max_lag_hours": max_lag_hours,
        "ingest_grace_hours": grace_hours,
    }
    try:
        try:
            build = json.loads(_get(fetch, site_url, BUILD))
        except ValueError as exc:
            raise Unknown(f"build.json did not parse: {exc}") from exc
        if not isinstance(build, dict):
            raise Unknown("build.json is not an object")
        started = policy.parse_time(((build.get("generator") or {}).get("presentation") or {}).get("build_started_at"))
        if started is None:
            raise Unknown("build.json records no build_started_at")
        m["build_started_at"] = policy.iso(started)
        provenance = build.get("provenance") or {}
        m["validation_history_commit"] = provenance.get("validation_history_commit")
        try:
            summary = json.loads(gzip.decompress(_verified(build, SUMMARY, _get(fetch, site_url, SUMMARY))))
        except (OSError, EOFError, ValueError) as exc:
            raise Unknown(f"site-summary.json did not decompress and parse: {exc}") from exc
        as_of, measured = data_as_of(summary, build)
        m["data_as_of"] = policy.iso(as_of) if as_of else None
        m["measured_cells"] = measured

        block = exclusion_block(_verified(build, RUNS, _get(fetch, site_url, RUNS)))
        schema_counts = provenance.get("series_schema_counts")
        if not isinstance(schema_counts, dict) or not all(isinstance(v, int) for v in schema_counts.values()):
            raise Unknown("build.json records no series schema counts")
        total = sum(schema_counts.values())
        admitted_rows = block["projection_admitted_physical_row_count"]
        excluded_rows = block["projection_excluded_physical_row_count"]
        if admitted_rows != (build.get("counts") or {}).get("physical_row_count") or admitted_rows + excluded_rows != total:
            raise Unknown(
                f"runs.json admits {admitted_rows} and excludes {excluded_rows} series rows; "
                f"build.json counts {(build.get('counts') or {}).get('physical_row_count')} admitted "
                f"of {total}"
            )
        exclusions = categorize(block)
        exclusions["series_total"] = total
        m["exclusions"] = exclusions

        found, read = candidates(root, build, summary, as_of, started, grace_hours)
        m["ledger_sources"] = read
        m["candidate_count"] = len(found)
    except Unknown as exc:
        m.update(state="unknown", reason=str(exc))
        return m

    def lag(c: dict) -> float | None:
        if as_of is None:
            return None
        return _hours(policy.parse_time(c["finished_at"]), as_of)

    for c in found:
        c["lag_hours"] = lag(c)
    over = [c for c in found if c["lag_hours"] is None or c["lag_hours"] > max_lag_hours]
    stale_row: dict | None = None
    undetermined: list[dict] = []
    compares = 0
    for c in over:
        if c["recorded_main"]:
            c["main"] = "recorded post-run main distance 0"
            stale_row = c
            break
        if compares >= max_compare:
            c["main"] = "not checked (compare budget spent)"
            undetermined.append(c)
            continue
        if time_left() < COMPARE_SECS:
            c["main"] = "not checked (time budget spent)"
            undetermined.append(c)
            continue
        compares += 1
        ancestor = main_ancestor(repo, c["commit"], run)
        if ancestor is True:
            c["main"] = "GitHub compare: ancestor of main"
            stale_row = c
            break
        if ancestor is None:
            c["main"] = "GitHub compare failed"
            undetermined.append(c)
        else:
            c["main"] = "GitHub compare: not on main"
    m["compare_calls"] = compares
    m["newest_candidate"] = found[0] if found else None
    m["newest_recorded_main_candidate"] = next((c for c in found if c["recorded_main"]), None)
    m["stale_candidate"] = stale_row
    m["undetermined_candidates"] = undetermined[:5]
    m["lag_hours"] = stale_row["lag_hours"] if stale_row else None

    if stale_row is not None:
        freshness = "stale"
    elif undetermined:
        freshness = "unknown"
    else:
        freshness = "current"
    m["freshness"] = freshness
    excluded = m["exclusions"]["not_in_manifest"] > 0
    if freshness == "stale":
        m["state"] = "stale+not-in-manifest" if excluded else "stale"
    elif excluded:
        m["state"] = "not-in-manifest"
    elif freshness == "unknown":
        m["state"] = "unknown"
        m["reason"] = (
            f"{len(undetermined)} candidate(s) more than {max_lag_hours} h newer than the data "
            f"have undetermined main ancestry; newest {undetermined[0]['run_id']} "
            f"finished {undetermined[0]['finished_at']}"
        )
    else:
        m["state"] = "current"
    return m


def _exclusion_text(m: dict) -> str:
    x = m["exclusions"]
    text = (
        f"excluded {x['excluded_total']} of {x['series_total']} series rows (reasons overlap): "
        f"no admitted terminal {x['no_admitted_terminal']}, schema {x['schema']}, "
        f"not in builder manifest {x['not_in_manifest']}"
    )
    if x["not_in_manifest_cells"]:
        cells = list(x["not_in_manifest_cells"])
        text += f" ({', '.join(cells[:3])}{', ...' if len(cells) > 3 else ''})"
    return text


def render(m: dict) -> str:
    verdict = m.get("policy")
    # How long the condition has held comes right after the state, so a
    # truncated capture never loses it.
    persisting = f" ({policy.persistence_text(verdict)})" if verdict and m["state"] != "current" else ""
    if m["state"] == "unknown":
        text = f"site-data: UNKNOWN{persisting} -- {m['reason']}"
    else:
        head = {
            "current": "CURRENT",
            "stale": "STALE",
            "not-in-manifest": "ROWS EXCLUDED FOR CELLS MISSING FROM THE BUILDER MANIFEST",
            "stale+not-in-manifest": "STALE, AND ROWS EXCLUDED FOR CELLS MISSING FROM THE BUILDER MANIFEST",
        }[m["state"]]
        text = (
            f"site-data: {head}{persisting} -- published data as of {m['data_as_of']} "
            f"(build started {m['build_started_at']})"
        )
        row = m.get("stale_candidate")
        if row:
            text += (
                f"; run {row['run_number']} {row['run_id']} on hermit {row['commit'][:10]} "
                f"finished {row['finished_at']}, {row['lag_hours']} h newer (limit "
                f"{m['max_lag_hours']} h), main by {row['main']}, site status {row['site_status']}"
            )
        elif m.get("freshness") == "unknown":
            text += "; freshness undetermined (main ancestry of a newer candidate unknown)"
        else:
            text += f"; no main candidate more than {m['max_lag_hours']} h newer"
        text += "; " + _exclusion_text(m)
    return text


# The 2026-09-28 stale reading renders in about 520 characters; the cap leaves
# room for a long unknown reason and three not-in-manifest cell names.
SUMMARY_CAP = 1000


def captured_summary(m: dict) -> str:
    """One inert line for tick-hub's key=value capture."""
    summary = " ".join(render(m).split())
    return summary.replace("{", "(").replace("}", ")")[:SUMMARY_CAP]


def main(
    argv: list[str] | None = None,
    fetch: Fetcher = _fetch,
    run: Runner = _run,
    now: dt.datetime | None = None,
) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--site-url", default=SITE_URL)
    ap.add_argument("--repo", default=DEFAULT_REPO, help="repository whose main the site admits")
    ap.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2],
        help="checkout whose ledger/ is compared (default: this tool's checkout)",
    )
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--gate", action="store_true",
        help="exit 1 on every evaluation in a warning state, and on persistent unknown",
    )
    ap.add_argument("--state", type=Path, help="state file; required with --gate")
    ap.add_argument("--max-lag-hours", type=float, default=DEFAULT_MAX_LAG_HOURS)
    ap.add_argument("--ingest-grace-hours", type=float, default=DEFAULT_INGEST_GRACE_HOURS)
    ap.add_argument("--max-compare", type=int, default=DEFAULT_MAX_COMPARE)
    ap.add_argument(
        "--time-budget-secs", type=float, default=DEFAULT_TIME_BUDGET_SECS,
        help="no GitHub compare call starts once fewer than 15 s of this budget remain",
    )
    ap.add_argument(
        "--unknown-limit", type=int, default=policy.UNKNOWN_LIMIT,
        help="consecutive unknown evaluations before the gate itself warns",
    )
    args = ap.parse_args(argv)
    if args.gate and args.state is None:
        ap.error("--gate needs --state")
    if args.unknown_limit < 1:
        ap.error("--unknown-limit must be at least 1")
    if args.max_lag_hours <= 0 or args.ingest_grace_hours < 0 or args.max_compare < 0 or args.time_budget_secs < 0:
        ap.error("--max-lag-hours must be positive; grace, compare and time budgets non-negative")

    started = time.monotonic()
    now = now or dt.datetime.now(dt.timezone.utc)
    m = measure(
        site_url=args.site_url,
        root=args.root,
        repo=args.repo,
        now=now,
        max_lag_hours=args.max_lag_hours,
        grace_hours=args.ingest_grace_hours,
        max_compare=args.max_compare,
        fetch=fetch,
        run=run,
        time_left=lambda: args.time_budget_secs - (time.monotonic() - started),
    )
    verdict: dict | None = None
    if args.state is not None:
        with locked_state(args.state) as state:
            key = policy.UNKNOWN_KEY if m["state"] == "unknown" else m["state"]
            if m["state"] != "unknown":
                state["state"] = m["state"]
                state["data_as_of"] = m.get("data_as_of")
            verdict = policy.evaluate(state, key, now, unknown_limit=args.unknown_limit)
        m["policy"] = verdict
    m["outcome_key"] = verdict["outcome_key"] if verdict else m["state"]

    if args.json:
        print(json.dumps(m, indent=2, sort_keys=True))
    else:
        # Only key=value lines: tick-hub captures every stdout line with '='.
        print(f"state={m['state']}")
        print(f"summary={captured_summary(m)}")
        print(f"outcome_key={m['outcome_key']}")
        print(f"data_as_of={m.get('data_as_of') or 'none'}")
        print(f"lag_hours={m.get('lag_hours') if m.get('lag_hours') is not None else 'none'}")
        excluded = (m.get("exclusions") or {}).get("not_in_manifest")
        print(f"excluded_not_in_manifest={excluded if excluded is not None else 'unknown'}")

    if args.gate and verdict and verdict["fire"]:
        return FIRE
    if m["state"] == "unknown":
        return UNKNOWN
    return QUIET


if __name__ == "__main__":
    sys.exit(main())
