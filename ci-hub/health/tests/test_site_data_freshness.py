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
NOT_IN_MANIFEST = sdf.NOT_IN_MANIFEST_PREFIX + "c-programs/random-readv-stream/verify/ptrace"
# Incompressible filler ahead of the exclusion block, so the streamed search
# crosses several 1 MiB decompression chunks as it does on the real site.
FILLER = {hashlib.sha256(str(i).encode()).hexdigest(): (i % 3 == 0) for i in range(60000)}


def ledger_row(run_id, finished, *, schema=5, ahead=0, commit=MAIN_COMMIT, run_number=1950, **legacy):
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
    return {
        "commit": commit,
        "event_type": "run.result",
        "team": "hermit",
        "run_id": run_id,
        "run_number": run_number,
        "schema": "validate-ledger/v1",
        "legacy_row": body,
    }


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
            {"schema": "stress-series/v1", "reason": sdf.SCHEMA_REASON, "physical_row_count": 10, "represented_run_count": 10},
            {"schema": "stress-series/v3", "reason": sdf.NO_TERMINAL_REASON, "physical_row_count": 30, "represented_run_count": 30},
            {"schema": "5", "reason": "validation HistoryRow schema is not 7, 8, 9 or 10", "physical_row_count": 4, "represented_run_count": 4},
        ]
        self.build_started = BUILD_STARTED
        self.rows: list[dict] = []
        self.read_lines: int | None = None  # None: the build read every row
        self.tamper: set[str] = set()
        self.fail: set[str] = set()
        self.fetched: list[str] = []

    # -- published artifacts -------------------------------------------------
    def _write_ledger(self) -> bytes:
        path = self.root / LEDGER
        path.parent.mkdir(parents=True, exist_ok=True)
        data = b"".join(json.dumps(r, sort_keys=True).encode() + b"\n" for r in self.rows)
        path.write_bytes(data)
        return data

    def artifacts(self) -> dict[str, bytes]:
        data = self._write_ledger()
        lines = data.splitlines(keepends=True)
        read = len(lines) if self.read_lines is None else self.read_lines
        prefix = b"".join(lines[:read])
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
                    "sources": [
                        {
                            "path": LEDGER,
                            "store": "validation-ledger",
                            "location": "published",
                            "bytes": len(prefix),
                            "line_count": read,
                            "sha256": hashlib.sha256(prefix).hexdigest(),
                        }
                    ],
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
    site.rows = [ledger_row("r-late", "2026-09-28T19:20:00Z")]
    assert run_gate(site, tmp_path / "s.json") == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "current"


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


def test_a_ledger_that_does_not_extend_what_the_build_read_is_not_trusted(site, tmp_path, capsys):
    site.rows = [ledger_row("r1", "2026-09-28T17:13:51Z")]
    fetch_artifacts = site.artifacts()
    # Rewrite the tool-root ledger after the build recorded its digest: the
    # line index no longer proves the build read the row, so only the grace
    # rule can admit it, and two hours is inside the grace.
    (site.root / LEDGER).write_bytes(json.dumps(ledger_row("r1", "2026-09-28T17:13:51Z", note="x")).encode() + b"\n")
    rc = sdf.main(
        ["--gate", "--state", str(tmp_path / "s.json"), "--root", str(site.root)],
        fetch=lambda url, cap: fetch_artifacts[url[len(sdf.SITE_URL):]],
        run=Compare(),
        now=NOW,
    )
    assert rc == sdf.QUIET
    assert fields(capsys.readouterr().out)["state"] == "current"


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
    for argv in (["--gate"], ["--unknown-limit", "0"], ["--max-lag-hours", "0"], ["--time-budget-secs", "-1"]):
        with pytest.raises(SystemExit) as exc:
            sdf.main(argv, fetch=lambda url, cap: "unused", run=Compare(), now=NOW)
        assert exc.value.code == 2
