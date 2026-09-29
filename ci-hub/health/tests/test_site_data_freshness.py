"""Unit tests for site_data_freshness.

Every published artifact is synthesized, so each state can be produced on
demand. `test_the_2026_09_28_site_reads_stale` reconstructs the live reading of
2026-09-28: data as of 2026-09-22T14:16:37Z, RUN 1950 (a clean schema-5 full
run on a main commit) finished 2026-09-28T18:38:52Z, 35 minutes before the
19:13:51Z build started, and the build left it among its finalized-proof gaps.
See https://github.com/rrnewton/dev-hermit/issues/394.
"""

from __future__ import annotations

import datetime as dt
import gzip
import hashlib
import importlib.util
import json
import re
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent.parent
MODULE = HERE / "site_data_freshness.py"
spec = importlib.util.spec_from_file_location("site_data_freshness", MODULE)
assert spec and spec.loader
sdf = importlib.util.module_from_spec(spec)
sys.modules["site_data_freshness"] = sdf
spec.loader.exec_module(sdf)

NOW = dt.datetime(2026, 9, 28, 21, 0, tzinfo=dt.timezone.utc)
BUILD_STARTED = "2026-09-28T19:13:51.951292+00:00"
AS_OF = "2026-09-22T14:16:37Z"
LEDGER = "ledger/hermit/devbig014/2026-09.jsonl"
MAIN_COMMIT = "1bd45c15f3a0e5ea7e86c0232d3f4ddc10e9ca63"
BRANCH_COMMIT = "84ea829b2ddd00000000000000000000000000aa"
# Written out rather than built from sdf.NOT_IN_MANIFEST_PREFIX, so a change to
# the constant cannot silently change the fixture with it. The constant itself is
# checked against the Rust builder in test_reason_strings_match_the_rust_builder.
NOT_IN_MANIFEST = (
    "cell is not in the website builder's manifest (not yet in its cell universe): "
    "c-programs/random-readv-stream/verify/ptrace"
)
# The builder's reason strings, written out for the same reason. Round 1 built
# the fixture from sdf.SCHEMA_REASON and sdf.NO_TERMINAL_REASON, so the fixture
# agreed with the gate whatever the gate said.
SCHEMA = "series schema is not stress-series/v3"
NO_TERMINAL = "validate series row has no admitted terminal validation row"
COMMIT_MISMATCH = "validate series row and terminal validation row record different Hermit commits"
CELL_NOT_SELECTED = "validate series row names a cell not selected by its validate path"
V1_HOST_FACTS = "stress-series/v1 does not record machine_shortname, kernel_version, and host_capabilities"
DIRTY_SOURCE = "source_tree_dirty is true; dirty source is not checked-in evidence"
# A second published hermit ledger file that the build did not read. By default
# it holds one recent row the site never admits (a hosted-portable run), so the
# scanned store is not silent; tests of the silence guard move or remove it.
HEARTBEAT_LEDGER = "ledger/hermit/devbig030/2026-09.jsonl"
HEARTBEAT_AT = "2026-09-28T20:00:00Z"
# Incompressible filler ahead of the exclusion block, so the streamed search
# crosses several 1 MiB decompression chunks as it does on the real site.
FILLER = {hashlib.sha256(str(i).encode()).hexdigest(): (i % 3 == 0) for i in range(60000)}


def ledger_row(
    run_id, finished, *, schema=5, ahead=0, commit=MAIN_COMMIT, run_number=1950, emitted_at=None, team="hermit",
    **legacy,
):
    body = {
        "schema_version": schema,
        "repo": "hermit",
        "run_id": run_id,
        "record_id": f"devbig014-{run_number}",
        "commit": commit,
        "commit_anchored": True,
        "tree_dirty": False,
        "profile": "full",
        "selection_mode": "full",
        "started_at": "2026-09-28T17:00:00Z",
        "finished_at": finished,
        "result": "fail",
        "git_ahead": ahead,
        "main_observation": {"ahead": ahead, "contract": "post-run-local-main-distance/v1"},
    }
    if schema == 5:
        body["cell_results"] = None
        body["raw_result_input_census_v1"] = {"files": [{"path": "x.jsonl", "rows": [{"line": 1}]}]}
    else:
        body["cell_results"] = {"recorded": {"a": "pass"}}
    body.update(legacy)
    row = {
        "commit": commit,
        "event_type": "run.result",
        "team": team,
        "run_id": run_id,
        "run_number": run_number,
        "schema": "validate-ledger/v1",
        "legacy_row": body,
    }
    if emitted_at is not None:
        row["emitted_at"] = emitted_at
    return row


def heartbeat_row(at):
    """A recent hermit run.result the site never admits (hosted-portable profile)."""
    return ledger_row(
        "heartbeat", at, emitted_at=at, run_number=9999, profile="only-hosted-portable", selection_mode="only",
    )


class Site:
    """A published site plus the tool-root ledger it is compared with."""

    def __init__(self, root: Path):
        self.root = root
        self.cells = [
            {"cell": "a/verify/ptrace", "latest_result": "pass", "latest_result_time": AS_OF},
            {"cell": "b/verify/ptrace", "latest_result": "product_failure", "latest_result_time": "2026-09-21T10:00:00Z"},
            {"cell": "c/verify/ptrace", "latest_result": None, "latest_result_time": None},
        ]
        self.green, self.red = 1, 1
        self.admitted_runs = ["validate-main-full-20260922-v4"]
        self.missing_terminals: list[str] = []
        self.schema_counts = {"stress-series/v1": 10, "stress-series/v3": 90}
        self.admitted_rows, self.excluded_rows = 60, 40
        self.build_admitted_rows = 60
        self.exclusions = [
            {"schema": "stress-series/v1", "reason": SCHEMA, "physical_row_count": 10, "represented_run_count": 10},
            {"schema": "stress-series/v3", "reason": NO_TERMINAL, "physical_row_count": 30, "represented_run_count": 30},
            {"schema": "5", "reason": "validation HistoryRow schema is not 7, 8, 9 or 10", "physical_row_count": 4, "represented_run_count": 4},
        ]
        self.build_started = BUILD_STARTED
        self.rows: list[dict] = []
        self.read_lines: int | None = None  # None: the build read every row
        # Rows of HEARTBEAT_LEDGER, a tool-root file the build did not read.
        self.unread_rows: list[dict] = [heartbeat_row(HEARTBEAT_AT)]
        # Further tool-root files the build read in full: path -> (store, location, rows).
        self.read_files: dict[str, tuple[str, str, list[dict]]] = {}
        # Source entries added to build.json verbatim, for sources this gate must refuse.
        self.raw_sources: list[dict] = []
        self.name_main_ledger = True
        self.tamper: set[str] = set()
        self.fail: set[str] = set()
        self.fetched: list[str] = []

    # -- published artifacts -------------------------------------------------
    def _write(self, relative: str, rows: list[dict]) -> bytes:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        data = b"".join(json.dumps(r, sort_keys=True).encode() + b"\n" for r in rows)
        path.write_bytes(data)
        return data

    def _write_ledger(self) -> bytes:
        if self.unread_rows:
            self._write(HEARTBEAT_LEDGER, self.unread_rows)
        return self._write(LEDGER, self.rows)

    @staticmethod
    def _source(path: str, store: str, location: str, prefix: bytes) -> dict:
        return {
            "path": path,
            "store": store,
            "location": location,
            "bytes": len(prefix),
            "line_count": prefix.count(b"\n"),
            "sha256": hashlib.sha256(prefix).hexdigest(),
        }

    def artifacts(self) -> dict[str, bytes]:
        data = self._write_ledger()
        lines = data.splitlines(keepends=True)
        read = len(lines) if self.read_lines is None else self.read_lines
        prefix = b"".join(lines[:read])
        sources = [self._source(LEDGER, "validation-ledger", "published", prefix)] if self.name_main_ledger else []
        for path, (store, location, rows) in self.read_files.items():
            sources.append(self._source(path, store, location, self._write(path, rows)))
        sources.extend(self.raw_sources)
        summary = gzip.compress(
            json.dumps({"cells": self.cells, "runs": [{"run_id": r} for r in self.admitted_runs]}).encode()
        )
        runs_doc = {
            "current_main_ancestry": FILLER,
            "projection_admitted_physical_row_count": self.admitted_rows,
            "projection_admitted_represented_run_count": self.admitted_rows,
            "projection_excluded_physical_row_count": self.excluded_rows,
            "projection_excluded_represented_run_count": self.excluded_rows,
            "projection_exclusion_counts": self.exclusions,
            "projection_results": [{"cell": "a/verify/ptrace"}],
        }
        runs = gzip.compress(json.dumps(runs_doc, sort_keys=True, separators=(",", ":")).encode(), compresslevel=1)
        build = {
            "generator": {"presentation": {"build_started_at": self.build_started}},
            "artifacts": [
                {"path": sdf.SUMMARY, "bytes": len(summary), "sha256": hashlib.sha256(summary).hexdigest()},
                {"path": sdf.RUNS, "bytes": len(runs), "sha256": hashlib.sha256(runs).hexdigest()},
            ],
            "counts": {
                "physical_row_count": self.build_admitted_rows,
                "green_cell_count": self.green,
                "red_cell_count": self.red,
            },
            "provenance": {
                "series_schema_counts": self.schema_counts,
                "validation_history_commit": "82050eff3fb66d6a854f212b014e5cc627416874",
                "canonical_input_provenance": {
                    "sources": sources,
                    "finalized_proof": {
                        "missing_current_terminals": [{"run_id": r} for r in self.missing_terminals]
                    },
                },
            },
        }
        out = {sdf.BUILD: json.dumps(build).encode(), sdf.SUMMARY: summary, sdf.RUNS: runs}
        for path in self.tamper:
            out[path] = out[path] + b"\0"
        return out

    def fetcher(self):
        published = self.artifacts()

        def fetch(url, cap):
            path = url[len(sdf.SITE_URL):]
            self.fetched.append(path)
            assert cap == sdf.CAPS[path]
            if path in self.fail:
                return f"could not fetch {url}: curl rc=22 HTTP 404"
            return published[path]

        return fetch


class Compare:
    def __init__(self, answers=None, rc=0):
        self.answers = answers or {}
        self.rc = rc
        self.calls: list[list[str]] = []

    def __call__(self, cmd):
        self.calls.append(cmd)
        assert cmd[:3] == ["with-proxy", "gh", "api"], cmd
        commit = cmd[3].split("/compare/")[1].split("...")[0]
        if self.rc:
            return self.rc, "", "HTTP 502"
        return 0, self.answers.get(commit, "diverged") + "\n", ""


def run_gate(site, state=None, *extra, compare=None, now=NOW, gate=True):
    argv = ["--root", str(site.root), *extra]
    if gate:
        argv = ["--gate", "--state", str(state), *argv]
    return sdf.main(argv, fetch=site.fetcher(), run=compare or Compare(), now=now)


def fields(out: str) -> dict[str, str]:
    return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)


@pytest.fixture
def site(tmp_path):
    return Site(tmp_path / "root")


def test_the_2026_09_28_site_reads_stale(site, tmp_path, capsys):
    site.rows = [
        ledger_row("validate-main-full-20260922-v4", "2026-09-22T14:17:27Z", run_number=1900),
        ledger_row("validate-coord-s14-1bd45c15f3a0", "2026-09-28T18:38:52Z"),
    ]
    site.missing_terminals = ["validate-coord-s14-1bd45c15f3a0"]
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "stale"
    assert got["outcome_key"] == "stale"
    assert got["data_as_of"] == AS_OF
    assert got["lag_hours"] == "148.4"
    assert got["excluded_not_in_manifest"] == "0"
    summary = got["summary"]
    assert "run 1950 validate-coord-s14-1bd45c15f3a0 on hermit 1bd45c15f3" in summary
    assert "148.4 h newer (limit 24.0 h)" in summary
    assert "site status unfinalized" in summary
    assert "no admitted terminal 30, schema 10, not in builder manifest 0" in summary
    assert "excluded 40 of 100 series rows" in summary
    assert summary.startswith("site-data: STALE (in this state since 2026-09-28T21:00:00Z) -- ")
    assert len(summary) <= sdf.SUMMARY_CAP
    # Every artifact came from the published site, and the summary and runs
    # files were checked against build.json (tampering is tested below).
    assert site.fetched == [sdf.BUILD, sdf.SUMMARY, sdf.RUNS]


def test_a_candidate_within_the_limit_is_current(site, tmp_path, capsys):
    # The qualifying case for the lag guard: 23.9 h newer than the data.
    site.rows = [ledger_row("r1", "2026-09-23T14:10:37Z")]
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    got = fields(capsys.readouterr().out)
    assert got["state"] == "current"
    assert got["lag_hours"] == "none"
    # The same run 24.1 h newer is refused as stale.
    site.rows = [ledger_row("r1", "2026-09-23T14:22:37Z")]
    assert run_gate(site, tmp_path / "s2.json") == sdf.FIRE
    assert fields(capsys.readouterr().out)["lag_hours"] == "24.1"


def test_the_threshold_is_stated_and_configurable(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-24T14:16:37Z")]  # 48.0 h newer
    assert run_gate(site, tmp_path / "s.json", "--max-lag-hours", "48") == sdf.QUIET
    assert run_gate(site, tmp_path / "s2.json", "--max-lag-hours", "47.9") == sdf.FIRE
    assert "(limit 47.9 h)" in capsys.readouterr().out


def test_a_run_that_finished_after_the_build_started_is_not_a_candidate(site, tmp_path, capsys):
    # The qualifying case for the rebuild-due window: the build did not read the
    # run, which finished 1.7 h before this evaluation, after the build started.
    site.rows = [ledger_row("r-read", "2026-09-22T15:00:00Z", run_number=1),
                 ledger_row("r-late", "2026-09-28T19:20:00Z", run_number=2)]
    site.read_lines = 1
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "current"


def test_a_stopped_builder_reads_stale_once_a_rebuild_is_overdue(site, tmp_path, capsys):
    # Probe P1 of the round-2 review: the published build started 2026-09-23,
    # a main run finished 2026-09-26, and it is now 2026-09-28. The run
    # finished after the build started, so ingest grace never applies; it used
    # to be cut outright and the site read CURRENT with the builder stopped.
    site.build_started = "2026-09-23T19:13:51Z"
    site.rows = [ledger_row("r-read", "2026-09-22T15:00:00Z", run_number=1),
                 ledger_row("r-after", "2026-09-26T10:00:00Z", run_number=2)]
    site.read_lines = 1
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "stale"
    assert "run 2 r-after" in got["summary"]
    assert "site status after-build" in got["summary"]
    # The bound is 36 h and stated: 35.5 h old is not yet held against the
    # site, 36.5 h old is.
    site.rows[1] = ledger_row("r-after", "2026-09-27T09:30:00Z", run_number=2)
    assert run_gate(site, tmp_path / "s2.json") == sdf.QUIET
    site.rows[1] = ledger_row("r-after", "2026-09-27T08:30:00Z", run_number=2)
    assert run_gate(site, tmp_path / "s3.json") == sdf.FIRE
    capsys.readouterr()
    # ... and configurable.
    assert run_gate(site, tmp_path / "s4.json", "--rebuild-due-hours", "37") == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "current"


def test_a_row_the_build_read_counts_even_if_it_finished_after_the_build_started(site, tmp_path, capsys):
    # The build's own provenance proves it read the row; not publishing it is
    # the site's decision, so no time window applies.
    site.rows = [ledger_row("r-late", "2026-09-28T19:20:00Z")]
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    assert "site status read-not-admitted" in fields(capsys.readouterr().out)["summary"]


def test_a_row_the_build_did_not_read_counts_only_after_the_ingest_grace(site, tmp_path, capsys):
    # Appended to the ledger after the build's snapshot: two hours before the
    # build started is inside the 6 h ingest grace and is not held against it.
    site.rows = [ledger_row("r-read", "2026-09-22T15:00:00Z", run_number=1),
                 ledger_row("r-unread", "2026-09-28T17:13:51Z", run_number=2)]
    site.read_lines = 1
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    capsys.readouterr()
    # Eight hours before the build started, the build should have read it.
    site.rows[1] = ledger_row("r-unread", "2026-09-28T11:13:51Z", run_number=2)
    assert run_gate(site, tmp_path / "s2.json") == sdf.FIRE
    summary = fields(capsys.readouterr().out)["summary"]
    assert "run 2 r-unread" in summary
    assert "site status not-read" in summary


def test_a_ledger_that_does_not_extend_what_the_build_read_is_unknown(site, tmp_path, capsys):
    # Round 1 treated such a ledger as "maybe read" and could still read
    # CURRENT from it. The tool root is then not what the build read, and no
    # comparison with it can say the site is current.
    site.rows = [ledger_row("r1", "2026-09-22T20:00:00Z")]
    fetch_artifacts = site.artifacts()

    def gate(state):
        return sdf.main(
            ["--gate", "--state", str(tmp_path / state), "--root", str(site.root)],
            fetch=lambda url, cap: fetch_artifacts[url[len(sdf.SITE_URL):]],
            run=Compare(),
            now=NOW,
        )

    # Qualifying: the untouched ledger verifies and reads current.
    assert gate("s0.json") == sdf.QUIET
    capsys.readouterr()
    (site.root / LEDGER).write_bytes(json.dumps(ledger_row("r1", "2026-09-22T20:00:00Z", note="x")).encode() + b"\n")
    assert gate("s.json") == sdf.UNKNOWN
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert f"tool-root {LEDGER}" in got["summary"]
    assert "it is not an extension of what the build read" in got["summary"]
    # A ledger shorter than what the build read is refused the same way.
    (site.root / LEDGER).write_bytes(b"")
    assert gate("s2.json") == sdf.UNKNOWN
    assert "not an extension" in fields(capsys.readouterr().out)["summary"]


def test_a_branch_run_counts_only_when_github_puts_it_on_main(site, tmp_path, capsys):
    site.rows = [ledger_row("branch", "2026-09-28T17:19:52Z", ahead=23, commit=BRANCH_COMMIT)]
    diverged = Compare({BRANCH_COMMIT: "diverged"})
    assert run_gate(site, tmp_path / "s.json", compare=diverged) == sdf.QUIET
    assert len(diverged.calls) == 1
    assert diverged.calls[0] == [
        "with-proxy", "gh", "api", f"repos/rrnewton/hermit/compare/{BRANCH_COMMIT}...main", "--jq", ".status",
    ]
    capsys.readouterr()
    merged = Compare({BRANCH_COMMIT: "ahead"})
    assert run_gate(site, tmp_path / "s2.json", compare=merged) == sdf.FIRE
    assert "main by GitHub compare: ancestor of main" in capsys.readouterr().out
    # A compare API failure is not a verdict either way.
    assert run_gate(site, tmp_path / "s3.json", compare=Compare(rc=1)) == sdf.UNKNOWN
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert "undetermined main ancestry" in got["summary"]


def test_the_compare_budget_is_bounded_and_exhausting_it_is_unknown(site, tmp_path, capsys):
    site.rows = [
        ledger_row(f"branch-{i}", f"2026-09-28T1{i}:00:00Z", ahead=2, commit=f"{i:040x}", run_number=i)
        for i in range(5)
    ]
    compare = Compare()
    assert run_gate(site, tmp_path / "s.json", "--max-compare", "2", compare=compare) == sdf.UNKNOWN
    assert len(compare.calls) == 2
    assert "3 candidate(s)" in fields(capsys.readouterr().out)["summary"]
    # No compare call starts once the time budget is spent, and that is unknown.
    compare = Compare()
    assert run_gate(site, tmp_path / "s3.json", "--time-budget-secs", "0", compare=compare) == sdf.UNKNOWN
    assert compare.calls == []
    assert "5 candidate(s)" in fields(capsys.readouterr().out)["summary"]
    # No API call is spent on a candidate inside the limit.
    site.rows = [ledger_row("near", "2026-09-22T20:00:00Z", ahead=2, commit=BRANCH_COMMIT)]
    compare = Compare()
    assert run_gate(site, tmp_path / "s2.json", compare=compare) == sdf.QUIET
    assert compare.calls == []


def test_rows_the_site_never_admits_are_not_candidates(site, tmp_path, capsys):
    site.rows = [
        ledger_row("dirty", "2026-09-28T10:00:00Z", tree_dirty=True),
        ledger_row("buck", "2026-09-28T10:00:00Z", release_builder="buck"),
        ledger_row("portable", "2026-09-28T10:00:00Z", profile="only-hosted-portable", selection_mode="only"),
        ledger_row("no-census", "2026-09-28T10:00:00Z", raw_result_input_census_v1=None),
        ledger_row("empty-census", "2026-09-28T10:00:00Z", raw_result_input_census_v1={"files": [{"rows": []}]}),
        ledger_row("reverie", "2026-09-28T10:00:00Z", repo="reverie"),
    ]
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "current"
    # A schema-7 row with cell results is admitted even from a dirty tree; the
    # site admitted six such rows on 2026-09-28.
    site.rows.append(ledger_row("s7", "2026-09-28T10:00:00Z", schema=7, tree_dirty=True, commit_anchored=False))
    assert run_gate(site, tmp_path / "s2.json") == sdf.FIRE
    assert "run 1950 s7" in fields(capsys.readouterr().out)["summary"]


def test_rows_excluded_for_cells_missing_from_the_manifest_warn(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-22T20:00:00Z")]
    site.exclusions.append(
        {"schema": "stress-series/v3", "reason": NOT_IN_MANIFEST, "physical_row_count": 3, "represented_run_count": 3}
    )
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "not-in-manifest"
    assert got["excluded_not_in_manifest"] == "3"
    assert "not in builder manifest 3 (c-programs/random-readv-stream/verify/ptrace)" in got["summary"]
    # Combined with staleness, both are named.
    site.rows.append(ledger_row("r2", "2026-09-28T10:00:00Z"))
    assert run_gate(site, tmp_path / "s2.json") == sdf.FIRE
    assert fields(capsys.readouterr().out)["state"] == "stale+not-in-manifest"


def test_a_persisting_warning_fires_on_every_evaluation(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-28T10:00:00Z")]
    state = tmp_path / "s.json"
    summaries = []
    for hour in range(3):
        assert run_gate(site, state, now=NOW + dt.timedelta(hours=hour)) == sdf.FIRE
        summaries.append(fields(capsys.readouterr().out)["summary"])
        assert "in this state since 2026-09-28T21:00:00Z" in summaries[-1]
    # Byte-identical while nothing changed, so the escalation sink's
    # quiet-while-unchanged digest applies instead of a comment every tick.
    assert len(set(summaries)) == 1
    # A newer stale run is a changed condition and changes the page.
    site.rows.append(ledger_row("r2", "2026-09-28T11:00:00Z", run_number=2))
    assert run_gate(site, state, now=NOW + dt.timedelta(hours=2, minutes=30)) == sdf.FIRE
    assert fields(capsys.readouterr().out)["summary"] != summaries[0]
    # Recovery is recorded quietly, and a later relapse fires again.
    site.rows = [ledger_row("r1", "2026-09-22T20:00:00Z")]
    assert run_gate(site, state, now=NOW + dt.timedelta(hours=3)) == sdf.QUIET
    assert json.loads(state.read_text())["outcome_key"] == "current"
    site.rows = [ledger_row("r1", "2026-09-28T10:00:00Z")]
    assert run_gate(site, state, now=NOW + dt.timedelta(hours=4)) == sdf.FIRE


def test_unknown_warns_from_the_third_consecutive_evaluation_and_resets(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-22T20:00:00Z")]
    state = tmp_path / "s.json"
    assert run_gate(site, state) == sdf.QUIET
    site.fail = {sdf.BUILD}
    assert run_gate(site, state, now=NOW + dt.timedelta(minutes=30)) == sdf.UNKNOWN
    assert run_gate(site, state, now=NOW + dt.timedelta(minutes=60)) == sdf.UNKNOWN
    capsys.readouterr()
    assert run_gate(site, state, now=NOW + dt.timedelta(minutes=90)) == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert got["outcome_key"] == "unknown-persistent"
    assert "HTTP 404" in got["summary"]
    assert "UNKNOWN (undetermined since 2026-09-28T21:30:00Z; the gate warns from 3 consecutive" in got["summary"]
    assert json.loads(state.read_text())["consecutive_unknown"] == 3
    assert run_gate(site, state, now=NOW + dt.timedelta(minutes=150)) == sdf.FIRE
    assert json.loads(state.read_text())["outcome_key"] == "current"
    site.fail = set()
    assert run_gate(site, state, now=NOW + dt.timedelta(hours=3)) == sdf.QUIET
    site.fail = {sdf.RUNS}
    assert run_gate(site, state, now=NOW + dt.timedelta(hours=4)) == sdf.UNKNOWN


@pytest.mark.parametrize("path", [sdf.SUMMARY, sdf.RUNS])
def test_an_artifact_that_differs_from_build_json_is_unknown(site, tmp_path, capsys, path):
    site.rows = [ledger_row("r1", "2026-09-28T10:00:00Z")]
    site.tamper = {path}
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert f"{path} is not the artifact build.json records" in got["summary"]


def test_exclusion_counts_that_do_not_add_up_are_unknown(site, tmp_path, capsys):
    site.excluded_rows = 39
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    assert "excludes 39 series rows" in fields(capsys.readouterr().out)["summary"]
    site.excluded_rows, site.build_admitted_rows = 40, 61
    assert run_gate(site, tmp_path / "s2.json") == sdf.UNKNOWN


def test_measured_cells_must_match_the_green_and_red_counts(site, tmp_path, capsys):
    site.green = 2
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    assert "2 measured cells; build.json counts 3" in fields(capsys.readouterr().out)["summary"]


def test_an_unparsable_ledger_line_is_unknown(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-22T20:00:00Z")]
    fetch = site.fetcher()
    with (site.root / LEDGER).open("ab") as handle:
        handle.write(b"{not json\n")
    rc = sdf.main(["--gate", "--state", str(tmp_path / "s.json"), "--root", str(site.root)],
                  fetch=fetch, run=Compare(), now=NOW)
    assert rc == sdf.UNKNOWN
    assert f"{LEDGER} line 2 does not parse" in fields(capsys.readouterr().out)["summary"]


def test_a_missing_exclusion_block_is_unknown():
    body = gzip.compress(b'{"cells":[],"projection_results":[]}')
    with pytest.raises(sdf.Unknown, match="does not carry the projection exclusion block"):
        sdf.exclusion_block(body)
    body = gzip.compress(b'{"a":1,"projection_admitted_physical_row_count":1,"projection_results":[]}')
    with pytest.raises(sdf.Unknown, match="exclusion block has keys"):
        sdf.exclusion_block(body)


def test_report_mode_exits_zero_and_json_carries_everything(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-28T10:00:00Z")]
    assert run_gate(site, gate=False) == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "stale"
    assert run_gate(site, None, "--json", gate=False) == sdf.QUIET
    doc = json.loads(capsys.readouterr().out)
    assert doc["stale_candidate"]["run_id"] == "r1"
    assert doc["exclusions"]["series_total"] == 100
    assert doc["ledger_sources"][LEDGER]["prefix_verified"] is True


def test_usage_errors():
    for argv in (
        ["--gate"], ["--unknown-limit", "0"], ["--max-lag-hours", "0"], ["--time-budget-secs", "-1"],
        ["--rebuild-due-hours", "0"], ["--max-ledger-silence-hours", "0"], ["--max-ledger-silence-hours", "-1"],
    ):
        with pytest.raises(SystemExit) as exc:
            sdf.main(argv, fetch=lambda url, cap: "unused", run=Compare(), now=NOW)
        assert exc.value.code == 2


# -- F1: every source the build read must be one this gate scans --------------

CURRENT_ROW = "2026-09-22T20:00:00Z"  # after the data, inside every limit


def _raw_source(site, path, store="validation-ledger", location="published", write=True):
    """A build.json source entry for `path`, with the file written when possible."""
    rows = [ledger_row("elsewhere", "2026-09-28T10:00:00Z", run_number=7)]
    data = b"".join(json.dumps(r, sort_keys=True).encode() + b"\n" for r in rows)
    if write and not path.startswith("/"):
        target = site.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return Site._source(path, store, location, data)


OUTSIDE = [
    # The review's case: a writer that moved to another tree still names a
    # ledger/hermit/ path, but not one this gate (or the builder layout) scans.
    ("hermit_test_ledger/ledger/hermit/devbig014/2026-09.jsonl", "validation-ledger", "published", "outside the ledger files"),
    ("ledger/hermit/devbig014/2026-09/extra.jsonl", "validation-ledger", "published", "outside the ledger files"),
    ("ledger-recovery/hermit/devbig014/2026-08/x.jsonl", "validation-ledger", "published", "outside the ledger files"),
    ("series/hermit/devbig014/2026-09.jsonl", "validation-ledger", "published", "outside the ledger files"),
    ("ledger/hermit/devbig031/2026-09.jsonl", "series", "published", "outside the ledger files"),
    ("ledger/../hermit/2026-09.jsonl", "validation-ledger", "published", "outside the ledger files"),
    ("/data/ledger/hermit/devbig014/2026-09.jsonl", "validation-ledger", "published", "outside the ledger files"),
    ("ledger/hermit/devbig031/2026-09.jsonl", "validation-ledger-v2", "published", "which this gate does not know"),
]


@pytest.mark.parametrize("path,store,location,message", OUTSIDE)
@pytest.mark.parametrize("with_main_ledger", [True, False])
def test_a_source_outside_the_scanned_ledger_files_is_unknown(site, tmp_path, capsys, path, store, location, message, with_main_ledger):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    # Qualifying: without the foreign source the same site reads current.
    assert run_gate(site, tmp_path / "q.json") == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "current"
    site.name_main_ledger = with_main_ledger
    site.raw_sources = [_raw_source(site, path, store, location)]
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert message in got["summary"]
    assert path in got["summary"]


def test_every_layout_the_builder_reads_is_accepted(site, tmp_path, capsys):
    # The live build of 2026-09-28 read all four kinds: hermit and reverie
    # published ledgers, a recovery file, and series files.
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    reverie = ledger_row("rev", "2026-09-20T10:00:00Z", team="reverie", repo="reverie")
    recovery = ledger_row("recovered", "2026-08-20T10:00:00Z", run_number=3)
    site.read_files = {
        "ledger/reverie/devbig014/2026-09.jsonl": ("validation-ledger", "published", [reverie]),
        "ledger-recovery/hermit/devbig014/2026-08/4b681068b432ff230051fd7e105a60c0ae849a1b.jsonl": (
            "validation-ledger", "recovery", [recovery]),
        "series/hermit/devbig014/2026-09.jsonl": ("series", "published", [{"event_type": "series.observation"}]),
    }
    assert run_gate(site, None, "--json", gate=False) == sdf.QUIET
    doc = json.loads(capsys.readouterr().out)
    assert doc["state"] == "current"
    assert sorted(doc["ledger_sources"]) == sorted([
        LEDGER,
        "ledger/reverie/devbig014/2026-09.jsonl",
        "ledger-recovery/hermit/devbig014/2026-08/4b681068b432ff230051fd7e105a60c0ae849a1b.jsonl",
    ])
    assert all(entry["prefix_verified"] for entry in doc["ledger_sources"].values())
    assert doc["ledger_sources"][LEDGER]["location"] == "published"


def test_a_build_that_read_no_published_hermit_ledger_is_unknown(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    site.name_main_ledger = False
    # Reverie's published ledger and hermit's recovery file are both verified,
    # and neither is the published hermit ledger the comparison needs.
    site.read_files = {
        "ledger/reverie/devbig014/2026-09.jsonl": ("validation-ledger", "published", [ledger_row("rev", CURRENT_ROW, team="reverie")]),
        "ledger-recovery/hermit/devbig014/2026-08/x.jsonl": ("validation-ledger", "recovery", [ledger_row("rec", "2026-08-20T10:00:00Z")]),
    }
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    assert "names no published ledger/hermit/ source" in fields(capsys.readouterr().out)["summary"]
    # No sources at all is refused the same way.
    site.read_files = {}
    assert run_gate(site, tmp_path / "s2.json") == sdf.UNKNOWN
    assert "names no published ledger/hermit/ source" in fields(capsys.readouterr().out)["summary"]


def test_a_source_the_tool_root_cannot_read_is_unknown(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    site.raw_sources = [_raw_source(site, "ledger/hermit/devbig099/2026-09.jsonl", write=False)]
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    summary = fields(capsys.readouterr().out)["summary"]
    assert "build.json read ledger/hermit/devbig099/2026-09.jsonl; the tool root cannot read it" in summary
    # A validation-ledger source without its recorded prefix cannot be verified.
    source = _raw_source(site, "ledger/hermit/devbig099/2026-09.jsonl")
    del source["sha256"]
    site.raw_sources = [source]
    assert run_gate(site, tmp_path / "s2.json") == sdf.UNKNOWN
    assert "records no bytes, line_count and sha256" in fields(capsys.readouterr().out)["summary"]


def test_classify_agrees_with_the_site_builder():
    spec = importlib.util.spec_from_file_location(
        "export_inputs_for_parity", HERE.parent / "compatibility-website" / "export_inputs.py"
    )
    assert spec and spec.loader
    exporter = importlib.util.module_from_spec(spec)
    sys.modules["export_inputs_for_parity"] = exporter
    spec.loader.exec_module(exporter)
    battery = [
        "ledger/hermit/devbig014/2026-09.jsonl",
        "ledger/reverie/devbig030/2026-10.jsonl",
        "ledger/hermit/devbig014/2026-9.jsonl",
        "ledger/hermit/devbig014/2026-09.json",
        "ledger/hermit/devbig014/2026-09.jsonl.bak",
        "ledger/hermit/2026-09.jsonl",
        "ledger/hermit/devbig014/2026-09/extra.jsonl",
        "ledger-recovery/hermit/devbig014/2026-08/4b68.jsonl",
        "ledger-recovery/hermit/devbig014/2026-8/4b68.jsonl",
        "ledger-recovery/hermit/devbig014/2026-08.jsonl",
        "series/hermit/devbig014/2026-09.jsonl",
        "series/hermit/2026-09.jsonl",
        "hermit_test_ledger/ledger/hermit/devbig014/2026-09.jsonl",
        "hermit_test_ledger/series/hermit/devbig014/2026-09.jsonl",
        "other/hermit/devbig014/2026-09.jsonl",
    ]
    for path in battery:
        assert sdf._classify(path) == exporter._input_store_and_location(path), path
    # The exporter refuses these paths before it classifies; this gate refuses
    # them in the classifier itself.
    for path in ("/ledger/hermit/devbig014/2026-09.jsonl", "ledger/../hermit/2026-09.jsonl"):
        assert sdf._classify(path) is None, path


# -- F1: the scanned store must still be written -------------------------------

def test_a_silent_ledger_makes_freshness_undetermined(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    # Qualifying: the newest hermit result 47.9 h old.
    site.unread_rows = [heartbeat_row("2026-09-26T21:06:00Z")]
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    got = fields(capsys.readouterr().out)
    assert got["state"] == "current"
    # 48.1 h old: validation stopped, or its writer moved out of the scan.
    site.unread_rows = [heartbeat_row("2026-09-26T20:54:00Z")]
    assert run_gate(site, tmp_path / "s2.json") == sdf.UNKNOWN
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert (
        f"the newest hermit run.result row in the scanned ledger files was written 2026-09-26T20:54:00Z "
        f"({HEARTBEAT_LEDGER}), more than 48.0 h before this evaluation" in got["summary"]
    )
    # The bound is configurable.
    assert run_gate(site, tmp_path / "s3.json", "--max-ledger-silence-hours", "49") == sdf.QUIET
    capsys.readouterr()
    # A persistent silence becomes the gate's own warning from the third tick.
    state = tmp_path / "s4.json"
    assert run_gate(site, state) == sdf.UNKNOWN
    assert run_gate(site, state, now=NOW + dt.timedelta(minutes=30)) == sdf.UNKNOWN
    capsys.readouterr()
    assert run_gate(site, state, now=NOW + dt.timedelta(minutes=60)) == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unknown"
    assert got["outcome_key"] == "unknown-persistent"


def test_the_silence_guard_times_a_row_by_emitted_at_then_finished_at(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    # emitted_at governs when present: a recent finish does not hide an old write.
    old_write = heartbeat_row("2026-09-28T20:00:00Z")
    old_write["emitted_at"] = "2026-09-25T00:00:00Z"
    site.unread_rows = [old_write]
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    assert "written 2026-09-25T00:00:00Z" in fields(capsys.readouterr().out)["summary"]
    # Without emitted_at, the legacy finished_at is used.
    legacy = heartbeat_row("2026-09-28T20:00:00Z")
    del legacy["emitted_at"]
    site.unread_rows = [legacy]
    assert run_gate(site, tmp_path / "s2.json") == sdf.QUIET


@pytest.mark.parametrize("variant", ["recovery", "reverie", "enrich", "none"])
def test_only_published_hermit_results_count_as_ledger_activity(site, tmp_path, capsys, variant):
    site.rows = [ledger_row("r1", CURRENT_ROW)]  # itself 48.3 h old at NOW
    recent = "2026-09-28T20:00:00Z"
    site.unread_rows = []
    if variant == "recovery":
        site.read_files = {"ledger-recovery/hermit/devbig014/2026-09/x.jsonl": ("validation-ledger", "recovery", [heartbeat_row(recent)])}
    elif variant == "reverie":
        site.read_files = {"ledger/reverie/devbig014/2026-09.jsonl": ("validation-ledger", "published", [heartbeat_row(recent) | {"team": "reverie"}])}
    elif variant == "enrich":
        site.unread_rows = [heartbeat_row(recent) | {"event_type": "run.enrich"}]
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    summary = fields(capsys.readouterr().out)["summary"]
    assert f"was written {CURRENT_ROW} ({LEDGER})" in summary
    # A published hermit result makes the same site current.
    site.unread_rows = [heartbeat_row(recent)]
    assert run_gate(site, tmp_path / "s2.json") == sdf.QUIET


def test_a_ledger_with_no_hermit_result_at_all_is_unknown(site, tmp_path, capsys):
    site.rows = []
    site.unread_rows = []
    assert run_gate(site, tmp_path / "s.json") == sdf.UNKNOWN
    assert "the scanned ledger files hold no hermit run.result row" in fields(capsys.readouterr().out)["summary"]


def test_silence_does_not_hide_a_warning(site, tmp_path, capsys):
    # Stale and silent: the stale verdict stands, and the silence is named.
    site.rows = [ledger_row("r1", "2026-09-26T10:00:00Z")]
    site.unread_rows = []
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "stale"
    assert "; also the newest hermit run.result row in the scanned ledger files was written 2026-09-26T10:00:00Z" in got["summary"]
    # A manifest exclusion with an undetermined freshness still warns, and says
    # the freshness half could not be decided.
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    site.exclusions.append({"schema": "stress-series/v3", "reason": NOT_IN_MANIFEST, "physical_row_count": 1, "represented_run_count": 1})
    assert run_gate(site, tmp_path / "s2.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "not-in-manifest"
    assert "freshness undetermined: the newest hermit run.result row" in got["summary"]


# -- F2: exclusion reasons ------------------------------------------------------

def test_the_v1_host_facts_reason_is_accepted_and_counted(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    site.exclusions.append({"schema": "stress-series/v1", "reason": V1_HOST_FACTS, "physical_row_count": 10, "represented_run_count": 10})
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    got = fields(capsys.readouterr().out)
    assert got["state"] == "current"
    assert got["excluded_unaccepted"] == "0"
    assert "stress-series/v1 host facts 10" in got["summary"]


@pytest.mark.parametrize("reason", [COMMIT_MISMATCH, CELL_NOT_SELECTED, DIRTY_SOURCE, "a reason this gate has never seen"])
def test_an_exclusion_reason_outside_the_accepted_set_warns(site, tmp_path, capsys, reason):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    site.exclusions.append({"schema": "stress-series/v3", "reason": reason, "physical_row_count": 3, "represented_run_count": 3})
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "unaccepted-exclusion"
    assert got["excluded_unaccepted"] == "3"
    assert got["summary"].startswith(
        "site-data: ROWS EXCLUDED FOR A REASON THIS GATE DOES NOT ACCEPT (in this state since"
    )
    assert f"NOT ACCEPTED 3 ({reason[:120]}: 3)" in got["summary"]
    # It fires on every evaluation, like every warning.
    assert run_gate(site, tmp_path / "s.json", now=NOW + dt.timedelta(minutes=30)) == sdf.FIRE


def test_unaccepted_reasons_are_listed_most_rows_first(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    for rows, reason in ((1, "reason one"), (5, "reason five"), (2, "reason two")):
        site.exclusions.append({"schema": "stress-series/v3", "reason": reason, "physical_row_count": rows, "represented_run_count": rows})
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["excluded_unaccepted"] == "8"
    assert "NOT ACCEPTED 8 (reason five: 5; reason two: 2; and 1 more reason(s))" in got["summary"]


def test_every_warning_component_is_named_in_order(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-28T10:00:00Z")]
    site.exclusions += [
        {"schema": "stress-series/v3", "reason": NOT_IN_MANIFEST, "physical_row_count": 2, "represented_run_count": 2},
        {"schema": "stress-series/v3", "reason": COMMIT_MISMATCH, "physical_row_count": 4, "represented_run_count": 4},
    ]
    assert run_gate(site, tmp_path / "s.json") == sdf.FIRE
    got = fields(capsys.readouterr().out)
    assert got["state"] == "stale+not-in-manifest+unaccepted-exclusion"
    assert got["summary"].startswith(
        "site-data: STALE, AND ROWS EXCLUDED FOR CELLS MISSING FROM THE BUILDER MANIFEST, AND "
        "ROWS EXCLUDED FOR A REASON THIS GATE DOES NOT ACCEPT (in this state since"
    )
    assert len(got["summary"]) <= sdf.SUMMARY_CAP


def test_a_validation_history_reason_is_counted_but_not_a_series_warning(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", CURRENT_ROW)]
    site.exclusions.append({"schema": "7", "reason": "a history reason", "physical_row_count": 2, "represented_run_count": 2})
    assert run_gate(site, None, "--json", gate=False) == sdf.QUIET
    doc = json.loads(capsys.readouterr().out)
    assert doc["state"] == "current"
    assert doc["exclusions"]["validation_history_reasons"]["a history reason"] == 2
    assert doc["exclusions"]["unaccepted"] == 0


# -- F2: the reason strings are the builder's ----------------------------------

RUST = HERE.parent / "lib" / "compatibility_website.rs"


def _rust_fn(source: str, name: str) -> str:
    start = source.index(f"fn {name}(")
    end = source.index("\n}\n", start)
    return source[start:end]


def _call_args(body: str, call: str) -> list[str]:
    out = []
    at = body.find(call)
    while at != -1:
        depth, index = 1, at + len(call)
        begin = index
        while depth:
            depth += {"(": 1, ")": -1}.get(body[index], 0)
            index += 1
        # rustfmt breaks a long literal's `.to_owned()` onto its own line and
        # leaves a trailing comma; normalize both.
        out.append(re.sub(r"\s+\.", ".", " ".join(body[begin:index - 1].split())).rstrip(",").strip())
        at = body.find(call, index)
    return out


def test_reason_strings_match_the_rust_builder():
    source = RUST.read_text(encoding="utf-8")
    manifest = _rust_fn(source, "cell_not_in_manifest_reason")
    assert f'format!("{sdf.NOT_IN_MANIFEST_PREFIX}{{cell}}")' in manifest
    assert sdf.NOT_IN_MANIFEST_PREFIX + "c-programs/random-readv-stream/verify/ptrace" == NOT_IN_MANIFEST

    body = _rust_fn(source, "series_exclusion_reasons")
    inserts = _call_args(body, "reasons.insert(")
    literals = sorted(a[1:-len('".to_owned()')] for a in inserts if a.startswith('"') and a.endswith('".to_owned()'))
    computed = sorted(a for a in inserts if not a.startswith('"'))
    # Every reason the builder can produce: four literals, the manifest reason,
    # and hermit's projection reason (checked against hermit below).
    assert len(inserts) == 6, inserts
    assert computed == ["cell_not_in_manifest_reason(&row.cell)", "reason.clone()"], computed
    assert literals == sorted([sdf.SCHEMA_REASON, sdf.NO_TERMINAL_REASON, sdf.COMMIT_MISMATCH_REASON, sdf.CELL_NOT_SELECTED_REASON])
    assert literals == sorted([SCHEMA, NO_TERMINAL, COMMIT_MISMATCH, CELL_NOT_SELECTED])
    assert "if let Some(reason) = &row.projection_reason {" in body
    assert "let projection_reason = row.validate_for_projection().err();" in source
    # The docstring's lag caveat: pressure-test and repeat rows skip the
    # terminal-row checks, so they can be admitted without a terminal row.
    assert "SeriesProducer::PressureTest | SeriesProducer::HermitRepeat => {}" in body
    # The accepted set is exactly these three; the other two literals warn.
    assert sdf.ACCEPTED_SERIES_REASONS == {
        NO_TERMINAL: "no_admitted_terminal", SCHEMA: "schema", V1_HOST_FACTS: "schema_v1_host_fields",
    }


def _pinned_hermit_file(relative: str) -> tuple[str | None, list[str]]:
    import subprocess

    repo = HERE.parent.parent
    tried: list[str] = []
    gitlink = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD:hermit"], capture_output=True, text=True)
    if gitlink.returncode != 0:
        return None, [f"git rev-parse HEAD:hermit failed: {gitlink.stderr.strip()}"]
    commit = gitlink.stdout.strip()
    common = subprocess.run(["git", "-C", str(repo), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                            capture_output=True, text=True).stdout.strip()
    candidates = [repo / "hermit" / ".git"]
    if common:
        candidates += [Path(common) / "modules" / "hermit", Path(common).parent / "hermit" / ".git"]
    for git_dir in candidates:
        tried.append(str(git_dir))
        if not git_dir.exists():
            continue
        shown = subprocess.run(["git", f"--git-dir={git_dir}", "show", f"{commit}:{relative}"],
                               capture_output=True, text=True)
        if shown.returncode == 0:
            return shown.stdout, tried
    return None, [f"hermit {commit} not found in any of"] + tried


def test_the_v1_reason_matches_the_pinned_hermit_source():
    text, tried = _pinned_hermit_file("ci/manifest-plan/src/stress_series.rs")
    if text is None:
        pytest.skip("pinned hermit source unavailable: " + " ".join(tried))
    constant = re.search(r'pub const STRESS_SERIES_SCHEMA_V1: &str = "([^"]+)";', text)
    assert constant, "STRESS_SERIES_SCHEMA_V1 not found"
    assert "Self::V1 => STRESS_SERIES_SCHEMA_V1," in text
    body = text[text.index("pub fn validate_for_projection(&self)"):]
    body = body[:body.index("\n    }\n")]
    v1 = re.search(
        r'if self\.schema == SeriesSchema::V1 \{\s*return Err\(format!\(\s*"\{\} ([^"]+)",\s*self\.schema\.as_str\(\)\s*\)\);',
        body,
    )
    assert v1, body
    assert f"{constant.group(1)} {v1.group(1)}" == sdf.V1_REASON == V1_HOST_FACTS
    # The dirty-source reason exists and is deliberately NOT accepted.
    assert f'"{DIRTY_SOURCE}"' in body
    assert DIRTY_SOURCE not in sdf.ACCEPTED_SERIES_REASONS


# -- F3: the rebuild gate covers what this gate leaves to it --------------------

def test_tick_hub_runs_the_rebuild_gate_wherever_it_runs_this_one():
    import yaml

    config = yaml.safe_load((HERE / "tick-hub.yaml").read_text(encoding="utf-8"))
    reminders = {row["name"]: row for row in config["reminders"]}
    data, rebuild = reminders["site_data_freshness"], reminders["site_rebuild_health"]
    assert "site_data_freshness.py --gate" in data["gate"]["cmd"]
    assert "site_rebuild_health.py --gate --dispatch" in rebuild["gate"]["cmd"]
    # Same cadence, and nothing that could make one run without the other.
    assert type(rebuild["cadence_secs"]) is int and rebuild["cadence_secs"] == data["cadence_secs"]
    for key in ("requires_flags", "depends_on", "cadence_offset_secs", "cadence_window_secs"):
        assert key not in rebuild and key not in data, key
    for key in ("--stale-hours", "--min-dispatch-interval-hours"):
        assert key not in rebuild["gate"]["cmd"], "the defaults the docstring states apply"
    # The split stated in the docstring: rebuild staleness 30 h, rebuild due 36 h.
    rebuild_module = (HERE / "site_rebuild_health.py").read_text(encoding="utf-8")
    assert 'ap.add_argument("--stale-hours", type=float, default=30.0)' in rebuild_module
    assert sdf.DEFAULT_REBUILD_DUE_HOURS == 36.0
