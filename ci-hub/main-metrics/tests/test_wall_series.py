#!/usr/bin/env python3
"""Both-direction tests for the main-branch wall series.

The alarm this builds is only worth having if it can be WRONG. So every
"regression detected" case is paired with a case that must NOT fire, and the
refusal path (unconditioned inputs) is tested as a first-class outcome rather
than as an error branch.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import wall_series as ws  # noqa: E402


def pt(wall, cpu, conc, commit="c", started="2026-08-07T00:00:00Z"):
    return ws.Point(commit=commit, started_at=started, wall=wall, cpu=cpu,
                    ratio=round(cpu / wall, 3) if wall else None,
                    concurrency=conc, bucket=ws.bucket_of(conc),
                    dag_jobs=None, cache_state=None, peak_memory_kb=None)


def ledger_row(commit, finished, *, executed=1):
    return {
        "commit": commit,
        "profile": "full",
        "result": "pass",
        "executed_tests": executed,
        "gates_run": 1,
        "gates_expected": 1,
        "finished_at": finished,
        "started_at": finished,
        # The oldest ledger schema: pre-count, pre-coverage, and one a writer
        # emitted before the release-builder pair existed.
        "schema_version": 1,
        "real_seconds": 10,
        "user_seconds": 5,
        "sys_seconds": 5,
    }


# ---------------------------------------------------------------- buckets

def test_bucket_boundaries_follow_the_measured_knee():
    assert ws.bucket_of(0) == "0-3"
    assert ws.bucket_of(3) == "0-3"
    assert ws.bucket_of(4) == "4-6"
    assert ws.bucket_of(6) == "4-6"      # knee: budget holds up to here
    assert ws.bucket_of(7) == "7-9"      # and breaks above it
    assert ws.bucket_of(14) == "14+"
    assert ws.bucket_of(999) == "14+"


def test_missing_concurrency_is_unknown_not_zero():
    """The majority bucket. Calling it 0 would silently claim an idle box."""
    assert ws.bucket_of(None) == ws.UNKNOWN
    assert pt(500, 1400, None).conditioned is False
    assert pt(500, 1400, 2).conditioned is True


# ------------------------------------------------- regression, both ways

def test_a_real_regression_fires():
    base = [pt(500, 1400, 2) for _ in range(5)]
    cand = [pt(900, 2500, 2) for _ in range(5)]
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "REGRESSION"
    assert v["delta_fraction"] > 0.20


def test_a_flat_series_does_NOT_fire():
    """The control. An alarm that always fires passes the test above."""
    base = [pt(500, 1400, 2) for _ in range(5)]
    cand = [pt(510, 1430, 2) for _ in range(5)]
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "OK", v


def test_an_improvement_does_NOT_fire():
    base = [pt(900, 2500, 2) for _ in range(5)]
    cand = [pt(500, 1400, 2) for _ in range(5)]
    assert ws.compare(base, cand, threshold=0.20)["verdict"] == "OK"


def test_just_under_threshold_does_not_fire_and_just_over_does():
    base = [pt(500, 1400, 2)]
    assert ws.compare(base, [pt(599, 1650, 2)], threshold=0.20)["verdict"] == "OK"
    assert ws.compare(base, [pt(601, 1660, 2)], threshold=0.20)["verdict"] == "REGRESSION"


# --------------------------------------- the ratio separates the causes

def test_contention_is_named_when_cpu_stays_flat():
    base = [pt(500, 1450, 2)]          # ratio 2.9
    cand = [pt(900, 1450, 2)]          # wall up, CPU identical -> we WAITED
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "REGRESSION"
    assert "CONTENTION" in v["cause"]


def test_more_work_is_named_when_cpu_rises_with_wall():
    base = [pt(500, 1450, 2)]          # ratio 2.9
    cand = [pt(900, 2900, 2)]          # ratio 3.2 -> we DID MORE
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "REGRESSION"
    assert "MORE WORK" in v["cause"]


# ------------------------------------------------------- the refusal

def test_unconditioned_inputs_REFUSE_rather_than_guess():
    """A verdict that cannot say what both sides ran under is not a verdict."""
    base = [pt(500, 1400, None) for _ in range(5)]
    cand = [pt(900, 2500, None) for _ in range(5)]
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "INSUFFICIENT"
    assert v["baseline_n"] == 0 and v["candidate_n"] == 0


def test_the_refusal_can_be_overridden_explicitly():
    base = [pt(500, 1400, None) for _ in range(3)]
    cand = [pt(900, 2500, None) for _ in range(3)]
    v = ws.compare(base, cand, threshold=0.20, require_conditioned=False)
    assert v["verdict"] == "REGRESSION"


def test_one_sided_conditioning_still_refuses():
    base = [pt(500, 1400, 2)]
    cand = [pt(900, 2500, None)]
    assert ws.compare(base, cand, threshold=0.20)["verdict"] == "INSUFFICIENT"


# ------------------------------------------------------- summary shape

def test_summary_reports_unconditioned_count_and_missing_memory():
    pts = [pt(500, 1400, 2), pt(700, 1900, None), pt(900, 2500, 14)]
    s = ws.summarize(pts)
    assert s["n"] == 3
    assert s["conditioned"] == 2 and s["unconditioned"] == 1
    assert s["peak_memory_available"] is False
    assert s["over_budget"] == 2          # 700 and 900 exceed 600
    assert set(s["by_concurrency_bucket"]) == {"0-3", "14+", ws.UNKNOWN}


def test_gate_breakdown_attributes_to_a_node_and_ranks_by_median():
    p = pt(500, 1400, 2)
    p.gates = [{"name": "slow.gate", "real_seconds": 300},
               {"name": "fast.gate", "real_seconds": 5}]
    q = pt(520, 1450, 2)
    q.gates = [{"name": "slow.gate", "real_seconds": 320},
               {"name": "fast.gate", "real_seconds": 7}]
    out = ws.gate_breakdown([p, q])
    assert out[0]["gate"] == "slow.gate"
    assert out[0]["n"] == 2
    assert out[0]["median_seconds"] == 310


def test_gate_breakdown_ignores_gates_without_a_duration():
    p = pt(500, 1400, 2)
    p.gates = [{"name": "no-timing"}, {"name": "timed", "real_seconds": 9}]
    out = ws.gate_breakdown([p])
    assert [g["gate"] for g in out] == ["timed"]


# ------------------------------------------------------- loader hygiene

def test_loader_qualifies_rows_and_orders_by_finished_at(tmp_path):
    f = tmp_path / "l.jsonl"
    late = ledger_row("late", "2026-08-07T02:00:00Z")
    early = ledger_row("early", "2026-08-07T01:00:00Z")
    empty = ledger_row("zero-executed", "2026-08-07T00:00:00Z", executed=0)
    f.write_text(
        json.dumps(late) + "\n\nnot json\n[\"a list\"]\n"
        + json.dumps(empty) + "\n" + json.dumps(early) + "\n",
        encoding="utf-8",
    )
    rows = ws.load_rows(f)
    assert [row["commit"] for row in rows] == ["early", "late"]


def test_rows_without_wall_are_dropped_not_zeroed():
    rows = [{"commit": "c", "real_seconds": 0, "user_seconds": 1, "sys_seconds": 1},
            {"commit": "c", "user_seconds": 1, "sys_seconds": 1},
            {"commit": "c", "real_seconds": 100, "user_seconds": 200, "sys_seconds": 90}]
    pts = ws.to_points(rows, None)
    assert len(pts) == 1
    assert pts[0].wall == 100 and pts[0].ratio == 2.9


def test_only_main_filter_drops_non_ancestors():
    rows = [{"commit": "onmain", "real_seconds": 10, "user_seconds": 5, "sys_seconds": 5},
            {"commit": "offmain", "real_seconds": 10, "user_seconds": 5, "sys_seconds": 5}]
    pts = ws.to_points(rows, {"onmain"})
    assert [p.commit for p in pts] == ["onmain"]


def test_main_commits_admits_on_main_and_refuses_off_main(monkeypatch):
    onmain = "1" * 40
    offmain = "2" * 40
    calls = []

    def run(argv, **kwargs):
        calls.append((argv, kwargs))
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout=onmain + "\n",
                stderr="",
            )
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout="commit\n",
            stderr="",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    found = ws.main_commits(
        Path("/repo"),
        {onmain, offmain, ""},
        ws.HERMIT_REPOSITORY,
    )

    assert found.on_main == {onmain}
    assert found.absent_upstream == {}
    assert len(calls) == 2


def test_main_commits_refuses_malformed_commit_without_running_git(monkeypatch):
    def run(*_args, **_kwargs):
        raise AssertionError("malformed commit must refuse before invoking git")

    monkeypatch.setattr(ws.subprocess, "run", run)

    with pytest.raises(RuntimeError, match="refused malformed commit ID"):
        ws.main_commits(Path("/repo"), {"not-a-commit"}, ws.HERMIT_REPOSITORY)


def test_main_commits_uses_two_subprocesses_for_the_whole_population(monkeypatch):
    calls = []
    commits = {f"{index:040x}" for index in range(1_000)}
    onmain = {f"{0:040x}", f"{999:040x}"}
    offmain = commits - onmain

    def run(argv, **kwargs):
        calls.append((argv, kwargs))
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout="\n".join(sorted(onmain)) + "\n",
                stderr="",
            )
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout="commit\n" * len(offmain),
            stderr="",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    found = ws.main_commits(Path("/repo"), commits, ws.HERMIT_REPOSITORY)

    assert found.on_main == onmain
    assert len(calls) == 2
    assert calls[0][0] == [
        "git", "-C", "/repo", "rev-list", "origin/main",
    ]
    assert calls[1][0] == [
        "git", "-C", "/repo", "cat-file", "--batch-check=%(objecttype)",
    ]
    assert set(calls[1][1]["input"].splitlines()) == offmain


def test_main_commits_refuses_when_the_ancestry_query_fails(monkeypatch):
    def run(argv, **kwargs):
        return subprocess.CompletedProcess(
            argv,
            128,
            stdout="",
            stderr="fatal: bad revision",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    claimed_main = "1" * 40
    with pytest.raises(RuntimeError, match="rev-list exited 128"):
        ws.main_commits(Path("/repo"), {claimed_main}, ws.HERMIT_REPOSITORY)


def test_main_commits_refuses_whole_population_when_one_object_is_absent(monkeypatch):
    claimed_main = "1" * 40
    absent = "f" * 40

    def run(argv, **kwargs):
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout=claimed_main + "\n",
                stderr="",
            )
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout=absent + " missing\n",
            stderr="",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    with pytest.raises(RuntimeError, match="refused 1 unavailable") as exc:
        ws.main_commits(
            Path("/repo"), {claimed_main, absent}, ws.HERMIT_REPOSITORY)

    assert f"{ws.HERMIT_REPOSITORY}:{absent}" in str(exc.value)


def test_rows_on_main_queries_each_commit_in_its_own_repository(monkeypatch):
    hermit_sha = "1" * 40
    reverie_sha = "2" * 40
    calls = []

    def main_commits(repo, commits, repository, ref="origin/main", upstream=None):
        calls.append((repo, commits, repository, ref))
        return ws.Ancestry(set(commits))

    monkeypatch.setattr(ws, "main_commits", main_commits)
    rows = [
        {"commit": hermit_sha},
        {"commit": hermit_sha, "repo": "rrnewton/hermit"},
        {"commit": reverie_sha, "repo": "reverie"},
        {"commit": reverie_sha, "repo": "rrnewton/reverie"},
    ]

    assert ws.rows_on_main(rows, {
        ws.HERMIT_REPOSITORY: Path("/hermit"),
        ws.REVERIE_REPOSITORY: Path("/reverie"),
    }).rows == rows
    assert calls == [
        (Path("/hermit"), {hermit_sha}, ws.HERMIT_REPOSITORY, "origin/main"),
        (Path("/reverie"), {reverie_sha}, ws.REVERIE_REPOSITORY, "origin/main"),
    ]


def test_rows_on_main_keeps_same_sha_separate_between_repositories(monkeypatch):
    shared_sha = "3" * 40

    def main_commits(_repo, commits, repository, ref="origin/main", upstream=None):
        assert commits == {shared_sha}
        return ws.Ancestry(
            set() if repository == ws.HERMIT_REPOSITORY else {shared_sha})

    monkeypatch.setattr(ws, "main_commits", main_commits)
    hermit_row = {"commit": shared_sha, "repo": "hermit"}
    reverie_row = {"commit": shared_sha, "repo": "reverie"}

    assert ws.rows_on_main([hermit_row, reverie_row], {
        ws.HERMIT_REPOSITORY: Path("/hermit"),
        ws.REVERIE_REPOSITORY: Path("/reverie"),
    }).rows == [reverie_row]


def test_rows_on_main_checks_every_repository_before_refusing(monkeypatch):
    hermit_sha = "7" * 40
    reverie_sha = "8" * 40
    calls = []

    def main_commits(_repo, commits, repository, ref="origin/main", upstream=None):
        calls.append((repository, commits))
        raise RuntimeError(
            f"main ancestry check refused 1 unavailable object(s): "
            f"{repository}:{next(iter(commits))}")

    monkeypatch.setattr(ws, "main_commits", main_commits)

    with pytest.raises(RuntimeError) as exc:
        ws.rows_on_main([
            {"commit": hermit_sha, "repo": "hermit"},
            {"commit": reverie_sha, "repo": "reverie"},
        ], {
            ws.HERMIT_REPOSITORY: Path("/hermit"),
            ws.REVERIE_REPOSITORY: Path("/reverie"),
        })

    assert calls == [
        (ws.HERMIT_REPOSITORY, {hermit_sha}),
        (ws.REVERIE_REPOSITORY, {reverie_sha}),
    ]
    message = str(exc.value)
    assert f"{ws.HERMIT_REPOSITORY}:{hermit_sha}" in message
    assert f"{ws.REVERIE_REPOSITORY}:{reverie_sha}" in message


def test_rows_on_main_refuses_an_unknown_repository_without_querying_git(monkeypatch):
    def main_commits(*_args, **_kwargs):
        raise AssertionError("unknown repository must refuse before invoking git")

    monkeypatch.setattr(ws, "main_commits", main_commits)
    sha = "4" * 40

    with pytest.raises(RuntimeError, match="unsupported repository") as exc:
        ws.rows_on_main(
            [{"commit": sha, "repo": "rrnewton/unknown"}],
            {ws.HERMIT_REPOSITORY: Path("/hermit")},
        )

    assert f"rrnewton/unknown:{sha}" in str(exc.value)


def test_main_commits_distinguishes_non_commit_from_unavailable(monkeypatch):
    unavailable = "5" * 40
    blob = "6" * 40

    def run(argv, **kwargs):
        if "rev-list" in argv:
            return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")
        return subprocess.CompletedProcess(
            argv, 0, stdout=f"{unavailable} missing\nblob\n", stderr="")

    monkeypatch.setattr(ws.subprocess, "run", run)

    with pytest.raises(RuntimeError) as exc:
        ws.main_commits(
            Path("/reverie"), {unavailable, blob}, ws.REVERIE_REPOSITORY)

    message = str(exc.value)
    assert "1 unavailable object(s)" in message
    assert f"{ws.REVERIE_REPOSITORY}:{unavailable}" in message
    assert "1 non-commit object(s)" in message
    assert f"{ws.REVERIE_REPOSITORY}:{blob}" in message


def test_unavailable_object_refusal_names_the_remedy(monkeypatch):
    """A correct refusal that nobody can act on still stops the series.

    On 2026-08-20 this gate refused on every tick for a whole session and produced
    no wall-time verdict, because the message listed the missing objects and said
    nothing about what to do. The objects were on the server the entire time and the
    checkout had simply never fetched them. This asserts the refusal keeps carrying
    the two facts that made it resolvable in minutes: that a fetch by SHA is worth
    trying, and that `git cat-file`'s "not our ref" is not a statement about whether
    the object exists upstream.
    """
    unavailable = "5" * 40

    def run(argv, **kwargs):
        if "rev-list" in argv:
            return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")
        return subprocess.CompletedProcess(
            argv, 0, stdout=f"{unavailable} missing\n", stderr="")

    monkeypatch.setattr(ws.subprocess, "run", run)

    with pytest.raises(RuntimeError) as exc:
        ws.main_commits(
            Path("/reverie"), {unavailable}, ws.REVERIE_REPOSITORY)

    message = str(exc.value)
    # The object is still named -- the remedy is added, it does not replace anything.
    assert f"{ws.REVERIE_REPOSITORY}:{unavailable}" in message
    assert "fetch --no-tags origin" in message, message
    assert "not our ref" in message, message
    assert "No commit found" in message, message


@pytest.mark.parametrize("option", ["--gate-regression", "--readiness"])
def test_both_gate_commands_emit_refusal_when_ancestry_refuses(monkeypatch, capsys,
                                                               option):
    def build(*_args, **_kwargs):
        raise RuntimeError("main ancestry check refused 1 unavailable object")

    monkeypatch.setattr(ws, "build", build)

    assert ws.main([option]) == 1
    output = capsys.readouterr().out
    assert "state=refused" in output
    assert "summary=REFUSED: main ancestry check refused" in output


# ------------------------------------------ commits GitHub says do not exist
#
# From 2026-09-25 the ledger carried 9 reverie rows for commits validated in
# local slots (8 on devbig030, 1 on devbig014) and never pushed. The checkout
# can never fetch them, so the ancestry
# check refused every tick and this gate could not produce a wall-time verdict.
# GitHub's affirmative "No commit found" now excludes such a row as not on main;
# every other outcome still refuses. The fixtures below copy the bytes gh 2.x
# printed on 2026-09-28 for `gh api --include repos/rrnewton/reverie/commits/<sha>`
# (first line LF, header lines CRLF, blank CRLF separator).

MAIN_HERMIT = "a1" * 20
MAIN_REVERIE = "b2" * 20
LOCAL_ONLY = "c3" * 20  # validated on some host, never pushed
UNFETCHED = "d4" * 20
OFF_MAIN = "e5" * 20  # present in the checkout, not an ancestor of main


def _http(status_line, body):
    return (f"{status_line}\nContent-Type: application/json; charset=utf-8\r\n"
            f"X-Ratelimit-Remaining: 4975\r\n\r\n{json.dumps(body)}")


def gh_absent(sha, status=422):
    reason = {404: "Not Found", 422: "Unprocessable Entity"}[status]
    return (1, _http(f"HTTP/2.0 {status} {reason}", {
        "message": f"No commit found for SHA: {sha}",
        "documentation_url": "https://docs.github.com/rest/commits/commits#get-a-commit",
        "status": str(status)}),
        f"gh: No commit found for SHA: {sha} (HTTP {status})")


GH_REPO_NOT_FOUND = (1, _http("HTTP/2.0 404 Not Found", {"message": "Not Found"}),
                     "gh: Not Found (HTTP 404)")
GH_TRANSPORT = (1, "", 'error connecting to api.github.com\n'
                'check your internet connection or https://githubstatus.com')
GH_BAD_GATEWAY = (1, _http("HTTP/2.0 502 Bad Gateway", {"message": "Server Error"}),
                  "gh: Server Error (HTTP 502)")
GH_RATE_LIMIT = (1, _http("HTTP/2.0 403 Forbidden", {
    "message": "API rate limit exceeded for user ID 1."}),
    "gh: API rate limit exceeded for user ID 1. (HTTP 403)")
GH_BAD_CREDENTIALS = (1, _http("HTTP/2.0 401 Unauthorized", {
    "message": "Bad credentials"}), "gh: Bad credentials (HTTP 401)")


def gh_present(sha):
    return (0, _http("HTTP/2.0 200 OK", {"sha": sha}), "")


class FakeHost:
    """Fake git for both checkouts plus a scripted GitHub.

    ``on_main`` and ``missing`` are per checkout path; ``github`` maps a SHA to
    a (returncode, stdout, stderr) triple, an exception instance to raise, or is
    absent -- in which case asking GitHub fails the test.
    """

    def __init__(self, monkeypatch, tmp_path, *, on_main, missing, github):
        self.on_main = on_main
        self.missing = missing
        self.github = github
        self.gh_calls = []
        gh = tmp_path / "gh"
        gh.write_text("#!/bin/sh\nexit 99\n")
        gh.chmod(0o755)
        monkeypatch.setenv("FLEET_REAL_GH", str(gh))
        monkeypatch.setattr(ws.subprocess, "run", self.run)

    def run(self, argv, **kwargs):
        if argv[0] == "with-proxy":
            self.gh_calls.append(argv)
            assert argv[2:4] == ["api", "--include"], argv
            sha = argv[4].rsplit("/", 1)[1]
            assert argv[4] == f"repos/{ws.REVERIE_REPOSITORY}/commits/{sha}", argv
            assert kwargs.get("timeout"), "a GitHub lookup must be time-bounded"
            if sha not in self.github:
                raise AssertionError(f"GitHub must not be asked about {sha}")
            answer = self.github[sha]
            if isinstance(answer, BaseException):
                raise answer
            rc, stdout, stderr = answer
            return subprocess.CompletedProcess(argv, rc, stdout=stdout, stderr=stderr)
        checkout = argv[2]
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv, 0, stdout="".join(f"{c}\n" for c in self.on_main.get(checkout, ())),
                stderr="")
        assert "cat-file" in argv, argv
        missing = self.missing.get(checkout, set())
        lines = [f"{c} missing" if c in missing else "commit"
                 for c in kwargs["input"].splitlines()]
        return subprocess.CompletedProcess(argv, 0, stdout="\n".join(lines) + "\n",
                                           stderr="")


def _rows():
    return [
        ledger_row(MAIN_HERMIT, "2026-09-26T00:00:00Z"),
        ledger_row(MAIN_HERMIT, "2026-09-26T01:00:00Z"),
        {**ledger_row(MAIN_REVERIE, "2026-09-26T02:00:00Z"), "repo": "reverie"},
        {**ledger_row(LOCAL_ONLY, "2026-09-26T03:00:00Z"), "repo": "reverie"},
        {**ledger_row(LOCAL_ONLY, "2026-09-26T04:00:00Z"), "repo": "rrnewton/reverie"},
        # Off main by git's own answer; GitHub is never asked about it.
        {**ledger_row(OFF_MAIN, "2026-09-26T05:00:00Z"), "repo": "reverie"},
    ]


def _host(monkeypatch, tmp_path, github, *, missing=(LOCAL_ONLY,)):
    return FakeHost(monkeypatch, tmp_path,
                    on_main={"/hermit": [MAIN_HERMIT], "/reverie": [MAIN_REVERIE]},
                    missing={"/reverie": set(missing)}, github=github)


def _build(monkeypatch, cache, rows=None):
    rows = _rows() if rows is None else rows
    monkeypatch.setattr(ws, "load_population", lambda _path: (rows, len(rows), 0))
    return ws.build(Path("/ledger"), Path("/hermit"), True, "full", "pass",
                    Path("/reverie"), cache)


def _gate_fields(monkeypatch, capsys, cache, *extra, mode="--gate-regression"):
    rc = ws.main(["--ledger", "/ledger", "--repo", "/hermit", "--reverie-repo",
                  "/reverie", "--absent-cache", str(cache), mode, *extra])
    lines = capsys.readouterr().out.splitlines()
    return rc, dict(line.split("=", 1) for line in lines)


@pytest.mark.parametrize("status", [422, 404])
def test_a_commit_github_says_does_not_exist_is_excluded_and_counted(
        monkeypatch, capsys, tmp_path, status):
    """The gate stops refusing, excludes the row, and says so in its output."""
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY, status)})
    monkeypatch.setattr(ws, "load_population", lambda _path: (_rows(), 5, 0))

    rc, fields = _gate_fields(monkeypatch, capsys, tmp_path / "absent.json")

    assert rc == 0, fields
    assert fields["state"] == "holding", fields
    assert fields["absent_upstream_rows"] == "2"
    assert fields["absent_upstream_commits"] == "1"
    assert fields["absent_upstream_shas"] == f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}"
    assert fields["github_lookups"] == "1"
    assert fields["absent_cache_hits"] == "0"
    # Only the three main rows reach the series.
    assert fields["n"] == "3"
    assert host.gh_calls == [[
        "with-proxy", str(tmp_path / "gh"), "api", "--include",
        f"repos/{ws.REVERIE_REPOSITORY}/commits/{LOCAL_ONLY}"]]


def test_main_rows_still_count_next_to_an_excluded_row(monkeypatch, tmp_path):
    _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY)})
    rows = _rows()

    selection = ws.rows_on_main(rows, {
        ws.HERMIT_REPOSITORY: Path("/hermit"),
        ws.REVERIE_REPOSITORY: Path("/reverie"),
    }, ws.UpstreamAbsence(tmp_path / "absent.json"))

    assert selection.rows == rows[:3]
    assert selection.absent_upstream_rows == 2
    assert [a.commit for a in selection.absent_upstream] == [LOCAL_ONLY]

    rep = _build(monkeypatch, tmp_path / "absent.json")
    assert rep["rows_on_main"] == 3
    assert rep["summary"]["n"] == 3
    assert rep["summary"]["distinct_commits"] == 2
    assert rep["rows_absent_upstream"] == 2


def test_a_present_off_main_commit_is_excluded_without_asking_github(monkeypatch,
                                                                    tmp_path):
    """Only commits MISSING from the checkout are GitHub's question.

    A commit the checkout has but main does not contain is off main by git's
    own answer. Here GitHub would even say it does not exist (an unpushed local
    branch commit), so asking would miscount it as absent upstream and spend
    the lookup budget on a commit that was never in doubt.
    """
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY),
                                         OFF_MAIN: gh_absent(OFF_MAIN)})

    rep = _build(monkeypatch, tmp_path / "absent.json")

    assert [call[4] for call in host.gh_calls] == [
        f"repos/{ws.REVERIE_REPOSITORY}/commits/{LOCAL_ONLY}"]
    assert OFF_MAIN not in {point["commit"] for point in rep["points"]}
    assert rep["rows_on_main"] == 3
    assert rep["rows_absent_upstream"] == 2
    assert [r["commit"] for r in rep["commits_absent_upstream"]] == [LOCAL_ONLY]
    assert rep["github_absence_lookup"]["github_lookups"] == 1


@pytest.mark.parametrize("answer", [
    GH_TRANSPORT,
    subprocess.TimeoutExpired(["gh"], 20),
    FileNotFoundError(2, "No such file or directory", "with-proxy"),
    GH_BAD_GATEWAY,
    GH_RATE_LIMIT,
    GH_BAD_CREDENTIALS,
    GH_REPO_NOT_FOUND,
    # The right words about a DIFFERENT commit prove nothing about this one.
    gh_absent(UNFETCHED),
    # gh reporting success on a "No commit found" body is not an HTTP answer
    # this gate understands; it must not be read as absence.
    (0,) + gh_absent(LOCAL_ONLY)[1:],
], ids=["transport", "timeout", "gh-missing", "http-502", "rate-limit",
        "bad-credentials", "repo-not-found", "other-sha", "rc0"])
def test_a_failed_lookup_still_refuses_and_caches_nothing(monkeypatch, tmp_path,
                                                          answer):
    _host(monkeypatch, tmp_path, {LOCAL_ONLY: answer})
    cache = tmp_path / "absent.json"

    with pytest.raises(RuntimeError, match="refused 1 unavailable") as exc:
        _build(monkeypatch, cache)

    assert f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}" in str(exc.value)
    assert not cache.exists()


def test_no_github_route_still_refuses(monkeypatch, tmp_path):
    host = _host(monkeypatch, tmp_path, {})
    monkeypatch.delenv("FLEET_REAL_GH")

    with pytest.raises(RuntimeError, match="no GitHub route"):
        _build(monkeypatch, tmp_path / "absent.json")

    assert host.gh_calls == []
    assert not (tmp_path / "absent.json").exists()


def test_github_having_the_commit_still_refuses(monkeypatch, tmp_path):
    _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_present(LOCAL_ONLY)})

    with pytest.raises(RuntimeError, match="GitHub HAS this commit"):
        _build(monkeypatch, tmp_path / "absent.json")

    assert not (tmp_path / "absent.json").exists()


def test_a_cached_absence_is_reused_without_asking_github(monkeypatch, tmp_path):
    cache = tmp_path / "absent.json"
    _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY)})
    first = _build(monkeypatch, cache)
    assert first["github_absence_lookup"]["github_lookups"] == 1
    stored = json.loads(cache.read_text())
    assert stored["schema"] == ws.ABSENT_CACHE_SCHEMA
    entry = stored["entries"][f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}"]
    assert entry["http_status"] == 422
    assert entry["message"] == f"No commit found for SHA: {LOCAL_ONLY}"

    # GitHub is now unreachable -- and not consulted, because the answer is cached.
    host = _host(monkeypatch, tmp_path, {})
    second = _build(monkeypatch, cache)

    assert host.gh_calls == []
    assert second["github_absence_lookup"]["github_lookups"] == 0
    assert second["github_absence_lookup"]["cache_hits"] == 1
    assert second["rows_on_main"] == 3
    assert second["rows_absent_upstream"] == 2
    assert [r["source"] for r in second["commits_absent_upstream"]] == ["cache"]


def test_a_network_failure_never_enters_the_cache(monkeypatch, tmp_path):
    """Affirmative answers are kept even when the run refuses; failures never are."""
    cache = tmp_path / "absent.json"
    _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY),
                                  UNFETCHED: GH_TRANSPORT},
          missing=(LOCAL_ONLY, UNFETCHED))
    rows = _rows() + [{**ledger_row(UNFETCHED, "2026-09-26T05:00:00Z"),
                       "repo": "reverie"}]

    with pytest.raises(RuntimeError, match=UNFETCHED):
        _build(monkeypatch, cache, rows)
    assert set(json.loads(cache.read_text())["entries"]) == {
        f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}"}

    # A second run that cannot reach GitHub still refuses on the same commit.
    host = _host(monkeypatch, tmp_path, {UNFETCHED: GH_TRANSPORT},
                 missing=(LOCAL_ONLY, UNFETCHED))
    with pytest.raises(RuntimeError, match=UNFETCHED):
        _build(monkeypatch, cache, rows)
    assert len(host.gh_calls) == 1


def test_one_failed_lookup_stops_further_lookups(monkeypatch, tmp_path):
    """A dead network costs one timeout per run, not one per missing commit."""
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: GH_TRANSPORT,
                                         UNFETCHED: gh_absent(UNFETCHED)},
                 missing=(LOCAL_ONLY, UNFETCHED))
    rows = _rows() + [{**ledger_row(UNFETCHED, "2026-09-26T05:00:00Z"),
                       "repo": "reverie"}]

    with pytest.raises(RuntimeError, match="refused 2 unavailable") as exc:
        _build(monkeypatch, tmp_path / "absent.json", rows)

    assert len(host.gh_calls) == 1
    assert "GitHub lookups stopped at" in str(exc.value)


def _cache_with(tmp_path, observed_at, *, status=422, message=None):
    cache = tmp_path / "absent.json"
    cache.write_text(json.dumps({"schema": ws.ABSENT_CACHE_SCHEMA, "entries": {
        f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}": {
            "http_status": status,
            "message": message or f"No commit found for SHA: {LOCAL_ONLY}",
            "observed_at": observed_at}}}))
    return cache


def _stamp(delta):
    from datetime import datetime, timezone
    return (datetime.now(timezone.utc) + delta).strftime("%Y-%m-%dT%H:%M:%SZ")


@pytest.mark.parametrize("entry", [
    {"observed_at": "expired"},
    {"observed_at": "future"},
    {"observed_at": "fresh", "status": 200},
    {"observed_at": "fresh", "message": "Not Found"},
    {"observed_at": "2026-09-28T00:00:00"},  # no timezone
], ids=["expired", "future", "status-200", "wrong-message", "naive-time"])
def test_an_expired_or_invalid_cache_entry_is_asked_again(monkeypatch, tmp_path,
                                                          entry):
    from datetime import timedelta
    stamps = {"expired": _stamp(-ws.ABSENT_CACHE_TTL - timedelta(hours=1)),
              "future": _stamp(timedelta(hours=1)),
              "fresh": _stamp(-timedelta(hours=1))}
    observed = stamps.get(entry["observed_at"], entry["observed_at"])
    cache = _cache_with(tmp_path, observed, status=entry.get("status", 422),
                        message=entry.get("message"))
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: GH_TRANSPORT})

    with pytest.raises(RuntimeError, match="refused 1 unavailable"):
        _build(monkeypatch, cache)

    assert len(host.gh_calls) == 1
    # The rejected entry is dropped from the rewritten cache.
    assert json.loads(cache.read_text())["entries"] == {}


@pytest.mark.parametrize("content, error", [
    (b"{not json", "cache is not JSON, ignored"),
    # Not UTF-8: read_text raises UnicodeDecodeError, a ValueError rather than
    # an OSError, which used to escape as a traceback with no state= line.
    (b"\xff\xfe\x00", "cache is not UTF-8, ignored: 'utf-8' codec can't decode"),
    # Nested past the recursion limit: json raises RecursionError, a
    # RuntimeError, which main() would have reported as an ancestry refusal.
    (b"[" * 200_000, "cache is not JSON, ignored"),
], ids=["not-json", "not-utf8", "too-deep"])
def test_an_unreadable_cache_is_reported_and_costs_only_a_lookup(monkeypatch, capsys,
                                                                 tmp_path, content,
                                                                 error):
    cache = tmp_path / "absent.json"
    cache.write_bytes(content)
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY)})
    monkeypatch.setattr(ws, "load_population", lambda _path: (_rows(), 5, 0))

    rc, fields = _gate_fields(monkeypatch, capsys, cache)

    assert rc == 0, fields
    assert fields["state"] == "holding", fields
    assert len(host.gh_calls) == 1
    assert fields["absent_upstream_rows"] == "2"
    assert fields["absent_cache_error"].startswith(error), fields
    # The unreadable file is replaced by a valid one holding the fresh answer.
    assert set(json.loads(cache.read_text())["entries"]) == {
        f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}"}


def test_the_lookup_budget_bounds_one_run_and_the_next_run_resumes(monkeypatch,
                                                                  tmp_path):
    """tick-hub kills this gate at 30 s; lookups must not push it over."""
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY),
                                         UNFETCHED: gh_absent(UNFETCHED)},
                 missing=(LOCAL_ONLY, UNFETCHED))
    cache = tmp_path / "absent.json"
    now = [100.0]

    def slow_lookup(argv, **kwargs):
        # Each lookup takes 6 of the 10 budgeted seconds, so a second one
        # would no longer fit a whole 5 s timeout.
        now[0] += 6.0
        return FakeHost.run(host, argv, **kwargs)

    monkeypatch.setattr(ws.subprocess, "run", slow_lookup)
    upstream = ws.UpstreamAbsence(cache, budget=10.0, timeout=5.0,
                                  clock=lambda: now[0])

    absent, unresolved = upstream.classify(ws.REVERIE_REPOSITORY,
                                           [LOCAL_ONLY, UNFETCHED])
    upstream.save()

    assert len(host.gh_calls) == 1
    assert set(absent) == {min(LOCAL_ONLY, UNFETCHED)}
    assert "budget is spent" in unresolved[max(LOCAL_ONLY, UNFETCHED)]
    assert upstream.report()["github_lookup_seconds"] == 6.0

    nxt = ws.UpstreamAbsence(cache, budget=10.0, timeout=5.0, clock=lambda: now[0])
    absent, unresolved = nxt.classify(ws.REVERIE_REPOSITORY, [LOCAL_ONLY, UNFETCHED])

    assert unresolved == {}
    assert set(absent) == {LOCAL_ONLY, UNFETCHED}
    assert (nxt.cache_hits, nxt.lookups) == (1, 1)


def test_each_absence_is_on_disk_before_the_next_lookup_starts(monkeypatch,
                                                              tmp_path):
    """tick-hub's SIGKILL skips build()'s finally, so save() there can be lost.

    The second lookup reads the cache file at the moment it starts, which is
    the latest moment a kill could land and still leave the first answer
    unwritten. No save() is called by the test.
    """
    first, second = sorted((LOCAL_ONLY, UNFETCHED))
    host = _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY),
                                         UNFETCHED: gh_absent(UNFETCHED)},
                 missing=(LOCAL_ONLY, UNFETCHED))
    cache = tmp_path / "absent.json"
    on_disk_when_asked = {}

    def recording_lookup(argv, **kwargs):
        sha = argv[-1].rsplit("/", 1)[1]
        on_disk_when_asked[sha] = (set(json.loads(cache.read_text())["entries"])
                                   if cache.exists() else set())
        return FakeHost.run(host, argv, **kwargs)

    monkeypatch.setattr(ws.subprocess, "run", recording_lookup)
    upstream = ws.UpstreamAbsence(cache)

    absent, unresolved = upstream.classify(ws.REVERIE_REPOSITORY, [first, second])

    assert unresolved == {}
    assert set(absent) == {first, second}
    assert on_disk_when_asked == {
        first: set(), second: {f"{ws.REVERIE_REPOSITORY}:{first}"}}
    assert set(json.loads(cache.read_text())["entries"]) == {
        f"{ws.REVERIE_REPOSITORY}:{first}", f"{ws.REVERIE_REPOSITORY}:{second}"}


def _ready_rows(candidate_wall):
    """A series that meets BOTH readiness preconditions, next to excluded rows.

    Four conditioned main rows over 117 days: two baseline rows at 500 s, then
    two at ``candidate_wall``. The two LOCAL_ONLY rows (absent upstream) and the
    OFF_MAIN row would each change baseline_n/candidate_n if they leaked in.
    """
    def row(commit, finished, wall, repo=None):
        return {**ledger_row(commit, finished), "real_seconds": wall,
                "user_seconds": wall * 3, "sys_seconds": 0,
                "concurrent_validates": 2, **({"repo": repo} if repo else {})}
    return [
        row(MAIN_HERMIT, "2026-06-01T00:00:00Z", 500),
        row(MAIN_HERMIT, "2026-06-02T00:00:00Z", 500),
        row(LOCAL_ONLY, "2026-09-25T00:00:00Z", 5000, "reverie"),
        row(MAIN_REVERIE, "2026-09-25T01:00:00Z", candidate_wall, "reverie"),
        row(LOCAL_ONLY, "2026-09-25T02:00:00Z", 5000, "rrnewton/reverie"),
        row(OFF_MAIN, "2026-09-25T03:00:00Z", 5000, "reverie"),
        row(MAIN_HERMIT, "2026-09-26T00:00:00Z", candidate_wall),
    ]


@pytest.mark.parametrize("mode, candidate_wall, state, rc", [
    ("--gate-regression", 510, "ok", 0),
    ("--gate-regression", 900, "regression", 1),
    ("--readiness", 510, "ready", 0),
    ("--readiness", None, "not-ready", 1),
], ids=["ok", "regression", "ready", "not-ready"])
def test_every_verdict_names_the_rows_excluded_as_absent_upstream(
        monkeypatch, capsys, tmp_path, mode, candidate_wall, state, rc):
    """`holding` is covered above; the verdicts that PAGE must carry it too.

    `ok` and `regression` are unreachable in production until retention reaches
    90 days, so without this a refactor could drop the exclusion accounting
    from exactly the verdicts that page, and nothing would notice until then.
    """
    rows = _rows() if candidate_wall is None else _ready_rows(candidate_wall)
    _host(monkeypatch, tmp_path, {LOCAL_ONLY: gh_absent(LOCAL_ONLY)})
    monkeypatch.setattr(ws, "load_population", lambda _path: (rows, len(rows), 0))

    got_rc, fields = _gate_fields(monkeypatch, capsys, tmp_path / "absent.json",
                                  mode=mode)

    assert (got_rc, fields["state"]) == (rc, state), fields
    assert fields["absent_upstream_rows"] == "2", fields
    assert fields["absent_upstream_commits"] == "1", fields
    assert fields["absent_upstream_shas"] == f"{ws.REVERIE_REPOSITORY}:{LOCAL_ONLY}"
    assert fields["github_lookups"] == "1", fields
    assert fields["absent_cache_hits"] == "0", fields
    assert float(fields["github_lookup_seconds"]) >= 0.0
    if state in ("ok", "regression"):
        # Only the four main rows are compared: 2 baseline, 2 candidate.
        assert (fields["baseline_n"], fields["candidate_n"]) == ("2", "2"), fields
    if state == "regression":
        assert fields["baseline_median"] == "500.0", fields
        assert fields["candidate_median"] == "900.0", fields
    if state == "ready":
        assert (fields["n"], fields["conditioned"]) == ("4", "4"), fields
    if state == "not-ready":
        assert fields["n"] == "3", fields


# ------------------------------------------------- readiness / wireable gate

def _rep(n_cond, n_total, span_days):
    from datetime import datetime, timedelta
    a = datetime(2026, 5, 1)
    b = a + timedelta(days=span_days)
    return {"summary": {"n": n_total, "conditioned": n_cond,
                        "span": [a.isoformat() + "Z", b.isoformat() + "Z"]}}


def test_readiness_needs_BOTH_retention_and_conditioning():
    """Raising either alone fails -- 90 days of unqualified walls is still a refusal."""
    only_days = ws.series_readiness(_rep(15, 100, 120), 90, 0.9)
    assert only_days["ready"] is False
    assert any("conditioning" in s for s in only_days["shortfalls"])
    only_cond = ws.series_readiness(_rep(99, 100, 4), 90, 0.9)
    assert only_cond["ready"] is False
    assert any("retention" in s for s in only_cond["shortfalls"])
    neither = ws.series_readiness(_rep(4, 26, 4), 90, 0.9)
    assert len(neither["shortfalls"]) == 2


def test_readiness_passes_when_both_are_met():
    r = ws.series_readiness(_rep(95, 100, 120), 90, 0.9)
    assert r["ready"] is True and r["shortfalls"] == []


def test_todays_real_shape_is_not_ready_and_names_both_gaps():
    r = ws.series_readiness(_rep(4, 26, 3.91), 90, 0.9)
    assert r["ready"] is False
    assert r["conditioned_frac"] < 0.2
    assert len(r["shortfalls"]) == 2
