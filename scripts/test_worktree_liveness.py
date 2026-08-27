#!/usr/bin/env python3
"""Both-direction tests for worktree liveness detection.

The failure this guards against is not "the detector misses a dead slot" -- it is
"the detector flags everything and looks like it works". A detector that returns
DEAD for every input passes any planted-dead-owner test. So every positive case
here is paired with a negative that must NOT be flagged, and the tests assert the
UNFLAGGED count explicitly.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import worktree_liveness as wl  # noqa: E402

NOW = datetime(2026, 8, 7, 12, 0, 0, tzinfo=timezone.utc)


def build_root(tmp: Path, slots: dict) -> Path:
    (tmp / "worktrees" / "slots").mkdir(parents=True, exist_ok=True)
    for name, rec in slots.items():
        if rec.pop("_mkdir", True):
            (tmp / "worktrees" / "slots" / name).mkdir(exist_ok=True)
    (tmp / wl.STATE_FILE).write_text(
        json.dumps({"slots": slots, "version": 1}, indent=2), encoding="utf-8")
    return tmp


def classify_with(tmp: Path, slots: dict, occ=None, porc=None, stale=6.0):
    root = build_root(tmp, slots)
    state = json.loads((root / wl.STATE_FILE).read_text())
    verdicts = wl.classify(root, state, occ or {}, porc or set(), NOW, stale)
    return {v.slot: v for v in verdicts}


def ts(hours_ago: float) -> str:
    return (NOW - timedelta(hours=hours_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")


# --------------------------------------------------------------------------
# The planted dead owner, and its paired live controls.
# --------------------------------------------------------------------------

def test_planted_dead_owner_is_detected(tmp_path):
    v = classify_with(tmp_path, {
        "deadslot": {"status": "active", "agents": [{"name": "hermit-ghost"}]},
    })["deadslot"]
    assert v.classification == wl.DEAD_OWNER
    assert "no process in slot" in v.reason


def test_dead_owner_with_a_dead_recorded_pid_is_detected(tmp_path):
    # PID 2^22 is above the default pid_max on this box and is reliably absent.
    v = classify_with(tmp_path, {
        "deadslot": {"status": "active", "owner_pid": 4194303, "heartbeat": ts(99)},
    })["deadslot"]
    assert v.classification == wl.DEAD_OWNER
    assert "dead" in v.reason and "stale" in v.reason


def test_live_slot_with_a_process_is_NOT_flagged(tmp_path):
    v = classify_with(tmp_path,
                      {"busy": {"status": "active"}},
                      occ={"busy": 3})["busy"]
    assert v.classification == wl.LIVE
    assert v.procs == 3


def test_live_slot_with_a_live_pid_is_NOT_flagged(tmp_path):
    v = classify_with(tmp_path,
                      {"mine": {"status": "active", "owner_pid": os.getpid()}})["mine"]
    assert v.classification == wl.LIVE
    assert v.owner_pid_alive is True


def test_live_slot_with_a_fresh_heartbeat_is_NOT_flagged(tmp_path):
    v = classify_with(tmp_path,
                      {"warm": {"status": "active", "heartbeat": ts(1)}})["warm"]
    assert v.classification == wl.LIVE


def test_the_detector_does_not_flag_everything(tmp_path):
    """The control that makes the positive test meaningful.

    A detector returning DEAD unconditionally passes every test above except this
    one. Mixed population in, exact split out.
    """
    slots = {
        "dead1": {"status": "active"},
        "dead2": {"status": "active", "owner_pid": 4194303},
        "live1": {"status": "active"},
        "live2": {"status": "active", "owner_pid": os.getpid()},
        "live3": {"status": "active", "heartbeat": ts(0.5)},
    }
    got = classify_with(tmp_path, slots, occ={"live1": 2})
    flagged = sorted(k for k, v in got.items() if v.classification == wl.DEAD_OWNER)
    unflagged = sorted(k for k, v in got.items() if v.classification == wl.LIVE)
    assert flagged == ["dead1", "dead2"], flagged
    assert unflagged == ["live1", "live2", "live3"], unflagged
    assert len(flagged) + len(unflagged) == 5


# --------------------------------------------------------------------------
# Liveness evidence must be able to OVERRIDE a stale record, never the reverse.
# --------------------------------------------------------------------------

def test_a_live_process_beats_a_stale_heartbeat_and_dead_pid(tmp_path):
    """The asymmetry, stated as a test.

    A false DEAD discards someone's work; a false LIVE costs one more cycle of
    protection. So any positive evidence wins.
    """
    v = classify_with(tmp_path, {
        "busy": {"status": "active", "owner_pid": 4194303, "heartbeat": ts(500)},
    }, occ={"busy": 1})["busy"]
    assert v.classification == wl.LIVE


def test_stale_heartbeat_alone_does_not_rescue_a_slot(tmp_path):
    v = classify_with(tmp_path,
                      {"cold": {"status": "active", "heartbeat": ts(48)}})["cold"]
    assert v.classification == wl.DEAD_OWNER


# --------------------------------------------------------------------------
# The bypass class: registry looks complete, and is not.
# --------------------------------------------------------------------------

def test_bare_git_worktree_add_is_DETECTED(tmp_path):
    root = build_root(tmp_path, {"known": {"status": "active"}})
    (root / "worktrees" / "slots" / "snuck-in").mkdir()
    state = json.loads((root / wl.STATE_FILE).read_text())
    got = {v.slot: v for v in
           wl.classify(root, state, {}, {"snuck-in"}, NOW, 6.0)}
    assert got["snuck-in"].classification == wl.BARE_GIT_BYPASS
    assert got["known"].classification != wl.BARE_GIT_BYPASS


def test_porcelain_is_read_from_the_PRODUCT_repos_not_the_parent(tmp_path):
    """Regression: querying the parent repo made every slot look absent.

    `worktrees/slots/<slot>/hermit` is a worktree of the HERMIT repo; the parent knows
    nothing about it. An earlier version asked the parent and reported 104/104
    active slots as missing from porcelain -- pure artifact of the wrong authority.
    Here the parent is deliberately given a worktree that must NOT be counted,
    while the product repo holds the real slot child.
    """
    root = tmp_path
    (root / wl.STATE_FILE).write_text('{"slots": {}}', encoding="utf-8")
    (root / "worktrees" / "slots" / "realslot" / "hermit").mkdir(parents=True)
    for repo, entries in (
        (root, [root / "worktrees" / "slots" / "decoy"]),                     # parent: ignore
        (root / "hermit", [root / "worktrees" / "slots" / "realslot" / "hermit"]),
    ):
        repo.mkdir(parents=True, exist_ok=True)
        (repo / ".git").write_text("gitdir: x\n", encoding="utf-8")

    calls = []

    def fake_run(cmd, **kw):
        calls.append(cmd)
        target = cmd[2]
        body = ""
        if target.endswith("hermit"):
            body = f"worktree {root}/worktrees/slots/realslot/hermit\n"
        return subprocess.CompletedProcess(cmd, 0, body, "")

    orig = wl.subprocess.run
    wl.subprocess.run = fake_run
    try:
        got = wl.porcelain_slots(root, products=("hermit",))
    finally:
        wl.subprocess.run = orig
    assert got == {"realslot"}
    # It asked the product repo, with -C, and never the bare parent.
    assert all(c[1] == "-C" and c[2].endswith("hermit") for c in calls), calls


def test_registered_slot_is_not_reported_as_a_bypass(tmp_path):
    """Negative side of the bypass check -- it must not fire on normal slots."""
    root = build_root(tmp_path, {"known": {"status": "active"}})
    state = json.loads((root / wl.STATE_FILE).read_text())
    got = [v for v in wl.classify(root, state, {}, {"known"}, NOW, 6.0)
           if v.classification == wl.BARE_GIT_BYPASS]
    assert got == []


# --------------------------------------------------------------------------
# Other classes.
# --------------------------------------------------------------------------

def test_released_slot_is_not_a_finding(tmp_path):
    v = classify_with(tmp_path, {"gone": {"status": "released"}})["gone"]
    assert v.classification == wl.RELEASED


def test_active_slot_whose_directory_vanished_is_a_phantom(tmp_path):
    v = classify_with(tmp_path,
                      {"vanished": {"status": "active", "_mkdir": False}})["vanished"]
    assert v.classification == wl.PHANTOM_PATH


def test_every_slot_reaches_exactly_one_classification(tmp_path):
    slots = {f"s{i}": {"status": "active"} for i in range(7)}
    slots["rel"] = {"status": "released"}
    got = classify_with(tmp_path, slots, occ={"s0": 1, "s1": 2})
    assert len(got) == 8
    assert all(v.classification for v in got.values())


# --------------------------------------------------------------------------
# EPERM must read as ALIVE, not dead.
# --------------------------------------------------------------------------

def test_pid_alive_treats_permission_denied_as_alive(monkeypatch):
    def boom(pid, sig):
        raise PermissionError
    monkeypatch.setattr(wl.os, "kill", boom)
    assert wl.pid_alive(1234) is True


def test_pid_alive_treats_no_such_process_as_dead(monkeypatch):
    def boom(pid, sig):
        raise ProcessLookupError
    monkeypatch.setattr(wl.os, "kill", boom)
    assert wl.pid_alive(1234) is False


# --------------------------------------------------------------------------
# touch writes only the two liveness fields, and report never writes.
# --------------------------------------------------------------------------

def test_touch_records_liveness_and_preserves_other_fields(tmp_path):
    root = build_root(tmp_path, {"s": {"status": "active", "purpose": "keep me",
                                       "agents": [{"name": "a"}]}})
    rc = wl.main(["--root", str(root), "touch", "--slot", "s", "--pid", str(os.getpid())])
    assert rc == 0
    rec = json.loads((root / wl.STATE_FILE).read_text())["slots"]["s"]
    assert rec["owner_pid"] == os.getpid()
    assert rec["heartbeat"].endswith("Z")
    assert rec["purpose"] == "keep me"
    assert rec["agents"] == [{"name": "a"}]


def test_touch_refuses_an_unknown_slot(tmp_path):
    root = build_root(tmp_path, {"s": {"status": "active"}})
    before = (root / wl.STATE_FILE).read_text()
    assert wl.main(["--root", str(root), "touch", "--slot", "nope"]) == 2
    assert (root / wl.STATE_FILE).read_text() == before


def test_report_does_not_mutate_state(tmp_path):
    root = build_root(tmp_path, {"a": {"status": "active"}, "b": {"status": "released"}})
    before = (root / wl.STATE_FILE).read_text()
    wl.main(["--root", str(root), "report", "--json"])
    assert (root / wl.STATE_FILE).read_text() == before


def test_fail_on_dead_exit_codes(tmp_path):
    root = build_root(tmp_path, {"dead": {"status": "active"}})
    assert wl.main(["--root", str(root), "report", "--fail-on-dead"]) == 1
    root2 = build_root(tmp_path / "clean", {"ok": {"status": "released"}})
    assert wl.main(["--root", str(root2), "report", "--fail-on-dead"]) == 0


# --------------------------------------------------------------------------
# The DELTA gate -- the only form that is wireable as a recurring alarm.
# --------------------------------------------------------------------------

def test_first_run_adopts_the_backlog_without_alarming(tmp_path):
    """A standing backlog is not news. 70 slots are flagged today and none may be
    reclaimed, so a first tick that paged all of them would be muted immediately."""
    rep = {"verdicts": [{"slot": "a", "classification": wl.DEAD_OWNER},
                        {"slot": "b", "classification": wl.DEAD_OWNER}]}
    state = tmp_path / "s.json"
    fresh, current = wl.new_dead(rep, state)
    assert fresh == set()
    assert current == {"a", "b"}


def test_a_newly_dead_slot_DOES_alarm(tmp_path):
    state = tmp_path / "s.json"
    wl.write_state(state, {"a"})
    rep = {"verdicts": [{"slot": "a", "classification": wl.DEAD_OWNER},
                        {"slot": "b", "classification": wl.DEAD_OWNER}]}
    fresh, current = wl.new_dead(rep, state)
    assert fresh == {"b"}


def test_an_unchanged_backlog_does_NOT_alarm(tmp_path):
    state = tmp_path / "s.json"
    wl.write_state(state, {"a", "b"})
    rep = {"verdicts": [{"slot": "a", "classification": wl.DEAD_OWNER},
                        {"slot": "b", "classification": wl.DEAD_OWNER}]}
    fresh, _ = wl.new_dead(rep, state)
    assert fresh == set()


def test_a_slot_that_revives_and_dies_again_alarms_again(tmp_path):
    """State is the CURRENT flagged set, not a cumulative union."""
    state = tmp_path / "s.json"
    wl.write_state(state, {"a"})
    revived = {"verdicts": [{"slot": "a", "classification": wl.LIVE}]}
    fresh, current = wl.new_dead(revived, state)
    wl.write_state(state, current)
    assert fresh == set() and current == set()
    died = {"verdicts": [{"slot": "a", "classification": wl.DEAD_OWNER}]}
    fresh2, _ = wl.new_dead(died, state)
    assert fresh2 == {"a"}


def test_live_slots_never_enter_the_delta_set(tmp_path):
    state = tmp_path / "s.json"
    wl.write_state(state, set())
    rep = {"verdicts": [{"slot": "live1", "classification": wl.LIVE},
                        {"slot": "live2", "classification": wl.LIVE},
                        {"slot": "rel", "classification": wl.RELEASED},
                        {"slot": "dead", "classification": wl.DEAD_OWNER}]}
    fresh, current = wl.new_dead(rep, state)
    assert fresh == {"dead"} and current == {"dead"}


def test_corrupt_state_adopts_baseline_rather_than_paging_everything(tmp_path):
    state = tmp_path / "s.json"
    state.write_text("not json", encoding="utf-8")
    rep = {"verdicts": [{"slot": "a", "classification": wl.DEAD_OWNER}]}
    fresh, _ = wl.new_dead(rep, state)
    assert fresh == set()
