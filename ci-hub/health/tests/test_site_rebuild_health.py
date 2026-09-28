"""Unit tests for site_rebuild_health.

Every state is driven from synthetic Actions API answers, including the two the
live repository cannot produce on demand: a dropped schedule (stale) and a
refused dispatch. The live reading on 2026-09-28 was `failed` for scheduled run
36453865612 ("validate-status ... selects unknown cells"), which
`test_failed_rebuild_fires_every_evaluation_until_fixed` reconstructs.

The firing policy (site_gate_policy.py) replaced "fire once per outcome key":
a warning fires on every evaluation for as long as it lasts, and an unknown
reading warns by itself from the third consecutive evaluation. See
https://github.com/rrnewton/dev-hermit/issues/394.
"""

from __future__ import annotations

import datetime as dt
import importlib.util
import json
import sys
from pathlib import Path

MODULE = Path(__file__).resolve().parent.parent / "site_rebuild_health.py"
spec = importlib.util.spec_from_file_location("site_rebuild_health", MODULE)
assert spec and spec.loader
srh = importlib.util.module_from_spec(spec)
sys.modules["site_rebuild_health"] = srh
spec.loader.exec_module(srh)

NOW = dt.datetime(2026, 9, 28, 17, 5, tzinfo=dt.timezone.utc)
REBUILD_STEPS = [
    {"name": "Check nightly source access and pins", "conclusion": "success"},
    {"name": "Rebuild from the current published cell ledger", "conclusion": "success"},
]
PLAIN_DOCS_STEPS = [
    {"name": "Check nightly source access and pins", "conclusion": "skipped"},
    {"name": "Rebuild from the current published cell ledger", "conclusion": "skipped"},
    {"name": "Build documentation", "conclusion": "success"},
]


def run_record(run_id, event, conclusion, created, finished, status="completed"):
    return {
        "databaseId": run_id,
        "event": event,
        "status": status,
        "conclusion": conclusion,
        "createdAt": created,
        "updatedAt": finished,
        "headSha": "ee2977cdec18592b64319201d7c4a22e104a44f8",
        "url": f"https://github.com/rrnewton/hermit/actions/runs/{run_id}",
    }


class FakeGh:
    def __init__(self, runs, steps=None, *, list_rc=0, dispatch_rc=0):
        self.runs = runs
        self.steps = steps or {}
        self.list_rc = list_rc
        self.dispatch_rc = dispatch_rc
        self.calls: list[list[str]] = []

    def __call__(self, cmd):
        self.calls.append(cmd)
        assert cmd[:2] == ["with-proxy", "gh"], cmd
        args = cmd[2:]
        if args[:2] == ["run", "list"]:
            if self.list_rc:
                return self.list_rc, "", "HTTP 403"
            return 0, json.dumps(self.runs), ""
        if args[:2] == ["run", "view"]:
            run_id = int(args[2])
            steps = self.steps.get(run_id, REBUILD_STEPS)
            return 0, json.dumps({"jobs": [{"steps": steps}]}), ""
        if args[:2] == ["workflow", "run"]:
            if self.dispatch_rc:
                return self.dispatch_rc, "", "HTTP 422: workflow not dispatchable"
            return 0, "", "Created workflow_dispatch event for docs.yml at main"
        raise AssertionError(f"unexpected gh call {cmd}")

    def dispatches(self):
        return [c for c in self.calls if c[2:4] == ["workflow", "run"]]


def summary_line(out):
    [line] = [line for line in out.splitlines() if line.startswith("summary=")]
    return line


def gate(fake, state, *extra, now=NOW):
    return srh.main(["--gate", "--state", str(state), *extra], run=fake, now=now)


def test_current_when_the_newest_rebuild_succeeded_recently(tmp_path, capsys):
    fake = FakeGh([run_record(2, "schedule", "success", "2026-09-28T13:00:00Z", "2026-09-28T13:50:00Z")])
    assert gate(fake, tmp_path / "s.json", "--dispatch") == srh.QUIET
    out = capsys.readouterr().out
    assert "state=current" in out
    assert fake.dispatches() == []


def test_failed_rebuild_fires_every_evaluation_until_fixed(tmp_path, capsys):
    runs = [
        run_record(36453865612, "schedule", "failure", "2026-09-28T16:50:00Z", "2026-09-28T17:01:22Z"),
        run_record(36324733377, "schedule", "success", "2026-09-27T14:06:40Z", "2026-09-27T15:05:46Z"),
    ]
    state = tmp_path / "s.json"
    fake = FakeGh(runs)
    assert gate(fake, state, "--dispatch") == srh.FIRE
    out = capsys.readouterr().out
    assert "state=failed" in out
    assert "36453865612" in out
    # The last success is 26 h old, under the 30 h window: warn, do not dispatch.
    assert fake.dispatches() == []
    assert "in this state since 2026-09-28T17:05:00Z" in out
    first_summary = summary_line(out)
    assert first_summary.startswith("summary=site-rebuild: FAILED (in this state since 2026-09-28T17:05:00Z) -- ")
    assert "Last successful rebuild: run 36324733377, finished 2026-09-27T15:05:46Z" in first_summary
    # The same failed run pages again an hour later, and says since when. Under
    # the old policy this was QUIET: a failure nobody fixed warned once.
    assert gate(FakeGh(runs), state, "--dispatch", now=NOW + dt.timedelta(hours=1)) == srh.FIRE
    out = capsys.readouterr().out
    assert "outcome_key=failed:36453865612" in out
    # Byte-identical while the condition is unchanged, so the escalation sink's
    # quiet-while-unchanged digest applies instead of a comment every tick.
    assert summary_line(out) == first_summary
    # A NEW failed run fires too, restarts the clock, and changes the page.
    newer = [run_record(36500000000, "workflow_dispatch", "failure", "2026-09-28T18:00:00Z",
                        "2026-09-28T18:10:00Z"), *runs]
    assert gate(FakeGh(newer), state, "--dispatch", now=NOW + dt.timedelta(hours=2)) == srh.FIRE
    out = capsys.readouterr().out
    assert "in this state since 2026-09-28T19:05:00Z" in out
    assert summary_line(out) != first_summary


def test_a_plain_docs_dispatch_is_not_a_rebuild(tmp_path, capsys):
    runs = [
        run_record(5, "workflow_dispatch", "success", "2026-09-28T16:00:00Z", "2026-09-28T16:05:00Z"),
        run_record(4, "schedule", "failure", "2026-09-28T13:00:00Z", "2026-09-28T13:10:00Z"),
    ]
    fake = FakeGh(runs, {5: PLAIN_DOCS_STEPS})
    assert gate(fake, tmp_path / "s.json") == srh.FIRE
    assert "state=failed" in capsys.readouterr().out


def test_dropped_schedule_goes_stale_and_is_dispatched_once(tmp_path, capsys):
    # GitHub skipped the cron: the newest success is 40 h old and nothing failed.
    runs = [run_record(3, "schedule", "success", "2026-09-27T00:00:00Z", "2026-09-27T01:00:00Z")]
    state = tmp_path / "s.json"
    fake = FakeGh(runs)
    assert gate(fake, state, "--dispatch") == srh.FIRE
    out = capsys.readouterr().out
    assert "state=stale" in out
    assert "dispatched docs.yml rebuild_compatibility=true" in out
    assert fake.dispatches() == [[
        "with-proxy", "gh", "workflow", "run", "docs.yml", "--repo", "rrnewton/hermit",
        "--ref", "main", "-f", "rebuild_compatibility=true",
    ]]
    recorded = json.loads(state.read_text())
    assert recorded["last_dispatch_at"] == "2026-09-28T17:05:00Z"
    # One hour later, still stale: no second dispatch inside the interval, but
    # the warning pages again.
    later = FakeGh(runs)
    assert gate(later, state, "--dispatch", now=NOW + dt.timedelta(hours=1)) == srh.FIRE
    assert later.dispatches() == []
    assert "in this state since 2026-09-28T17:05:00Z" in capsys.readouterr().out
    # Past the interval it dispatches again, and keeps warning.
    again = FakeGh(runs)
    assert gate(again, state, "--dispatch", now=NOW + dt.timedelta(hours=13)) == srh.FIRE
    assert len(again.dispatches()) == 1


def test_no_dispatch_while_a_run_is_in_flight(tmp_path, capsys):
    runs = [
        run_record(9, "workflow_dispatch", None, "2026-09-28T17:00:00Z", "2026-09-28T17:00:00Z",
                   status="in_progress"),
        run_record(3, "schedule", "success", "2026-09-27T00:00:00Z", "2026-09-27T01:00:00Z"),
    ]
    fake = FakeGh(runs)
    assert gate(fake, tmp_path / "s.json", "--dispatch") == srh.FIRE
    assert fake.dispatches() == []
    assert "state=stale" in capsys.readouterr().out


def test_a_refused_dispatch_is_loud(tmp_path, capsys):
    runs = [run_record(3, "schedule", "success", "2026-09-27T00:00:00Z", "2026-09-27T01:00:00Z")]
    state = tmp_path / "s.json"
    fake = FakeGh(runs, dispatch_rc=1)
    assert gate(fake, state, "--dispatch") == srh.FIRE
    out = capsys.readouterr().out
    assert "Dispatch: FAILED: gh workflow run refused" in out
    assert "outcome_key=stale:3:dispatch-failed" in out
    # Waiting out the interval does not flap the key back to plain stale, and
    # the refused dispatch keeps paging.
    waiting = FakeGh(runs)
    assert gate(waiting, state, "--dispatch", now=NOW + dt.timedelta(hours=1)) == srh.FIRE
    assert waiting.dispatches() == []
    out = capsys.readouterr().out
    assert "outcome_key=stale:3:dispatch-failed" in out
    assert "in this state since 2026-09-28T17:05:00Z" in out
    # An accepted retry clears the dispatch failure; the site is still stale.
    retry = FakeGh(runs)
    assert gate(retry, state, "--dispatch", now=NOW + dt.timedelta(hours=13)) == srh.FIRE
    assert "outcome_key=stale:3\n" in capsys.readouterr().out
    assert len(retry.dispatches()) == 1


def test_unreadable_actions_api_is_no_result_not_current(tmp_path, capsys):
    fake = FakeGh([], list_rc=1)
    assert gate(fake, tmp_path / "s.json", "--dispatch") == srh.UNKNOWN
    out = capsys.readouterr().out
    assert "state=unknown" in out
    assert "outcome_key=unknown\n" in out
    assert fake.dispatches() == []


def test_a_persistently_unreadable_api_warns_from_the_third_evaluation(tmp_path, capsys):
    # Qualifying and violating cases in one sequence: evaluations 1 and 2 are
    # NO_RESULT (tick-hub reruns them on the next outer tick), evaluation 3 and
    # every one after it fires the gate's own warning.
    state = tmp_path / "s.json"
    healthy = [run_record(2, "schedule", "success", "2026-09-28T13:00:00Z", "2026-09-28T13:50:00Z")]
    assert gate(FakeGh(healthy), state) == srh.QUIET
    capsys.readouterr()
    for tick in range(2):
        assert gate(FakeGh([], list_rc=1), state, now=NOW + dt.timedelta(minutes=30 * tick)) == srh.UNKNOWN
    out = capsys.readouterr().out
    assert "undetermined since 2026-09-28T17:05:00Z; the gate warns from 3 consecutive" in out
    assert json.loads(state.read_text())["consecutive_unknown"] == 2
    assert gate(FakeGh([], list_rc=1), state, now=NOW + dt.timedelta(hours=1)) == srh.FIRE
    out = capsys.readouterr().out
    assert "state=unknown" in out
    assert "outcome_key=unknown-persistent" in out
    assert "undetermined since 2026-09-28T17:05:00Z; the gate warns from 3 consecutive" in out
    assert gate(FakeGh([], list_rc=1), state, now=NOW + dt.timedelta(hours=2)) == srh.FIRE
    recorded = json.loads(state.read_text())
    assert recorded["consecutive_unknown"] == 4
    # Unknown never overwrote the last real outcome.
    assert recorded["outcome_key"] == "current"
    # A real verdict resets the count: the next unknown is NO_RESULT again.
    assert gate(FakeGh(healthy), state, now=NOW + dt.timedelta(hours=3)) == srh.QUIET
    assert json.loads(state.read_text())["consecutive_unknown"] == 0
    assert gate(FakeGh([], list_rc=1), state, now=NOW + dt.timedelta(hours=4)) == srh.UNKNOWN


def test_unknown_limit_is_configurable_and_bounded(tmp_path, capsys):
    state = tmp_path / "s.json"
    assert gate(FakeGh([], list_rc=1), state, "--unknown-limit", "1") == srh.FIRE
    try:
        gate(FakeGh([], list_rc=1), state, "--unknown-limit", "0")
    except SystemExit as exc:
        assert exc.code == 2
    else:
        raise AssertionError("--unknown-limit 0 must be a usage error")


def test_recovery_is_quiet_and_a_later_failure_fires_again(tmp_path):
    state = tmp_path / "s.json"
    failed = [run_record(7, "schedule", "failure", "2026-09-28T13:00:00Z", "2026-09-28T13:10:00Z"),
              run_record(6, "schedule", "success", "2026-09-28T01:00:00Z", "2026-09-28T02:00:00Z")]
    assert gate(FakeGh(failed), state) == srh.FIRE
    fixed = [run_record(8, "workflow_dispatch", "success", "2026-09-28T15:00:00Z",
                        "2026-09-28T15:10:00Z"), *failed]
    assert gate(FakeGh(fixed), state) == srh.QUIET
    assert json.loads(state.read_text())["outcome_key"] == "current"
    assert gate(FakeGh(fixed), state) == srh.QUIET
    assert gate(FakeGh(failed), state) == srh.FIRE


def test_gate_requires_a_state_file():
    try:
        srh.main(["--gate"], run=FakeGh([]), now=NOW)
    except SystemExit as exc:
        assert exc.code == 2
    else:
        raise AssertionError("--gate without --state must be a usage error")
