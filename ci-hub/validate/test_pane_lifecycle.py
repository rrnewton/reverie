#!/usr/bin/env python3
"""Brackets for the Herdr pane cap, reaper and TTL backstop.

Both directions are exercised on purpose. A reaper that closes nothing is
indistinguishable from a reaper that is broken unless the POSITIVE side proves
it closes what it should; and a cap that never refuses is indistinguishable from
no cap at all unless the NEGATIVE side proves it refuses when it cannot free a
slot. Each case therefore asserts on counts and identities, not on "it ran".
"""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pane_lifecycle as pl  # noqa: E402


NOW = datetime(2026, 8, 11, 12, 0, tzinfo=timezone.utc)
NOW_TS = NOW.timestamp()
FAILURES: list[str] = []


def check(label: str, condition: bool, detail: object = "") -> None:
    if condition:
        print(f"ok   {label}")
    else:
        print(f"FAIL {label}  {detail}")
        FAILURES.append(label)


def iso(delta_minutes: float) -> str:
    return (NOW + timedelta(minutes=delta_minutes)).isoformat()


def tab(n: int, minutes_ago: float) -> dict:
    return {"tab_id": f"wC:t{n}", "label": f"PR #{n} | deadbeefcafe | since {iso(-minutes_ago)}"}


class FakeHerdr:
    """Records every herdr call and answers workspace/tab list and tab close."""

    def __init__(self, tabs, workspace="wC", close_fails=()):
        self.tabs = list(tabs)
        self.workspace = workspace
        self.close_fails = set(close_fails)
        self.closed: list[str] = []
        self.created = 0
        self.commands: list[list[str]] = []

    def __call__(self, command, **kwargs):
        self.commands.append(list(command))
        argv = [c for c in command if not c.startswith("--setenv") and "=" not in c]
        try:
            herdr = argv[argv.index("herdr"):]
        except ValueError:
            return subprocess.CompletedProcess(command, 0, "", "")
        verb = herdr[1:3]
        if herdr[1] == "status":
            return subprocess.CompletedProcess(
                command, 0, json.dumps({"server": {"running": True}}), ""
            )
        if verb == ["pane", "rename"] or verb == ["pane", "run"]:
            body = {"result": {}}
        elif verb == ["workspace", "list"]:
            body = {"result": {"workspaces": [{"label": pl.pane_owner.WORKSPACE_LABEL,
                                               "workspace_id": self.workspace}]}}
        elif verb == ["tab", "list"]:
            body = {"result": {"tabs": self.tabs}}
        elif verb == ["tab", "close"]:
            tab_id = herdr[3]
            if tab_id in self.close_fails:
                return subprocess.CompletedProcess(command, 1, "", "planted close failure")
            self.closed.append(tab_id)
            self.tabs = [t for t in self.tabs if t["tab_id"] != tab_id]
            body = {"result": {"closed": tab_id}}
        elif verb == ["tab", "create"]:
            self.created += 1
            body = {"result": {"tab": {"tab_id": "wC:tNEW"}, "root_pane": {"pane_id": "wC:pNEW"}}}
        else:
            body = {"result": {}}
        return subprocess.CompletedProcess(command, 0, json.dumps(body), "")


ENV = {"HOME": "/home/newton", "PATH": "/usr/bin"}


def write_records(root: Path, records: dict[str, dict]) -> None:
    runs = root / "ignored" / "validate" / "runs"
    runs.mkdir(parents=True, exist_ok=True)
    for name, record in records.items():
        (runs / f"{name}.json").write_text(json.dumps(record))


# --------------------------------------------------------------------------
# classify(): every branch, with a live process and a dead one
# --------------------------------------------------------------------------
import os  # noqa: E402

MY_IDENTITY = {
    "pid": os.getpid(),
    "boot_id": pl.boot_id(),
    "start_ticks": int(Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(") ", 1)[-1].split()[19]),
}
DEAD_IDENTITY = {"pid": 2, "boot_id": pl.boot_id(), "start_ticks": 999999999}

check("a live owner is detected from /proc", pl.process_alive(MY_IDENTITY) is True)
check("a recycled/dead pid is not called alive", pl.process_alive(DEAD_IDENTITY) is False)
check("a missing identity is not called alive", pl.process_alive(None) is False)
check(
    "a different boot is not called alive",
    pl.process_alive({**MY_IDENTITY, "boot_id": "00000000-0000-0000-0000-000000000000"}) is False,
)

tabs = [tab(1, 5), tab(2, 60), tab(3, 60), tab(4, 24 * 60), tab(5, 10)]
records = {
    "wC:t1": {"tab_id": "wC:t1", "process_identity": MY_IDENTITY, "started_at": iso(-5)},
    "wC:t2": {"tab_id": "wC:t2", "process_identity": DEAD_IDENTITY, "started_at": iso(-60),
              "finished_at": iso(-59)},
    "wC:t3": {"tab_id": "wC:t3", "process_identity": DEAD_IDENTITY, "started_at": iso(-60),
              "finished_at": iso(-1)},
}
decisions = {d.tab_id: d for d in pl.classify(tabs, records, now=NOW_TS)}

check("a RUNNING owner is never reaped", decisions["wC:t1"].reapable is False, decisions["wC:t1"])
check("...and says why", decisions["wC:t1"].reason == pl.KEEP_RUNNING)
check("finished past the linger IS reaped", decisions["wC:t2"].reapable is True, decisions["wC:t2"])
check("finished inside the linger is KEPT", decisions["wC:t3"].reapable is False, decisions["wC:t3"])
check("...for the cleanup-delay reason", decisions["wC:t3"].reason == pl.KEEP_LINGER)
check("a recordless pane past the TTL is reaped", decisions["wC:t4"].reapable is True)
check("a recordless young pane is kept", decisions["wC:t5"].reapable is False)
check("...as too-young, not as unknown", decisions["wC:t5"].reason == pl.KEEP_YOUNG)

undated = pl.classify([{"tab_id": "wC:tX", "label": "no timestamp here"}], {}, now=NOW_TS)
check("an undated pane is KEPT, never guessed at", undated[0].reapable is False)
check("...and is reported as unknown", undated[0].reason == pl.KEEP_UNDATED)

# --------------------------------------------------------------------------
# reap(): POSITIVE -- it really closes, and reports both sides
# --------------------------------------------------------------------------
import tempfile  # noqa: E402

with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    write_records(root, records)
    fake = FakeHerdr(tabs)
    report = pl.reap(root, run=fake, environment=ENV, now=NOW_TS)
    check("reap closes exactly the reapable panes",
          sorted(report.closed) == ["wC:t2", "wC:t4"], report.closed)
    check("reap actually issued the closes", sorted(fake.closed) == ["wC:t2", "wC:t4"], fake.closed)
    check("reap keeps the rest", len(report.kept) == 3, report.kept)
    rendered = report.render()
    check("the report names every kept pane with a reason",
          all(d.tab_id in rendered and d.reason in rendered for d in report.kept))
    check("the report states the closed count", "closed=2" in rendered, rendered)

# NEGATIVE control: nothing stale -> closes nothing, and says so distinguishably
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    live_tabs = [tab(1, 5)]
    write_records(root, {"wC:t1": records["wC:t1"]})
    fake = FakeHerdr(live_tabs)
    report = pl.reap(root, run=fake, environment=ENV, now=NOW_TS)
    check("a healthy workspace loses nothing", report.closed == () and fake.closed == [])
    check("...and 'reaped 0' is explained, not silent",
          pl.KEEP_RUNNING in report.render(), report.render())

# A close failure is surfaced, never swallowed
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    write_records(root, records)
    fake = FakeHerdr(tabs, close_fails={"wC:t2"})
    report = pl.reap(root, run=fake, environment=ENV, now=NOW_TS)
    check("a failed close is reported", [t for t, _ in report.failed] == ["wC:t2"], report.failed)
    check("...and appears in the render", "FAILED to close wC:t2" in report.render())

# --------------------------------------------------------------------------
# ensure_capacity(): the cap must both MAKE ROOM and REFUSE
# --------------------------------------------------------------------------
# POSITIVE: at cap, but stale panes exist -> reaped, room made, no exception.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    many = [tab(i, 24 * 60) for i in range(1, 21)]      # 20 panes, all past TTL
    write_records(root, {})
    fake = FakeHerdr(many)
    pl.ensure_capacity(root, cap=20, run=fake, environment=ENV, now=NOW_TS)
    check("at cap with stale panes: room is made", len(fake.closed) >= 1, fake.closed)
    check("...by reaping, not evicting", len(fake.closed) == 20, fake.closed)

# NEGATIVE: at cap and EVERY pane is provably running -> must refuse.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    running = [tab(i, 5) for i in range(1, 21)]
    write_records(root, {
        f"wC:t{i}": {"tab_id": f"wC:t{i}", "process_identity": MY_IDENTITY, "started_at": iso(-5)}
        for i in range(1, 21)
    })
    fake = FakeHerdr(running)
    refused = False
    try:
        pl.ensure_capacity(root, cap=20, run=fake, environment=ENV, now=NOW_TS)
    except pl.WorkspaceFull as exc:
        refused = True
        message = str(exc)
    check("at cap with everything alive: REFUSES", refused)
    check("...closes nothing on the way out", fake.closed == [], fake.closed)
    check("...creates nothing", fake.created == 0)
    check("...and names the cap and a holder", refused and "cap of 20" in message and "wC:t1" in message)

# The refusal is what stops create_pane, so prove create really is blocked.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    running = [tab(i, 5) for i in range(1, 21)]
    write_records(root, {
        f"wC:t{i}": {"tab_id": f"wC:t{i}", "process_identity": MY_IDENTITY, "started_at": iso(-5)}
        for i in range(1, 21)
    })
    fake = FakeHerdr(running)
    blocked = False
    try:
        pl.pane_owner.create_pane(
            root=root, checkout=root, unit="u", target="a" * 40, log=root / "l",
            record=root / "r", pr=1, started_at=iso(0), run=fake, environment=ENV,
            sleep=lambda _s: None,
        )
    except pl.WorkspaceFull:
        blocked = True
    check("create_pane itself refuses at the cap", blocked)
    check("...and no tab was created", fake.created == 0, fake.created)

# POSITIVE control on the same path: under the cap, create_pane still works.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    checkout = root / "worktrees" / "validate" / "validate-fresh-observed"
    checkout.mkdir(parents=True)
    fake = FakeHerdr([tab(1, 5)])
    write_records(root, {"wC:t1": records["wC:t1"]})
    handle = pl.pane_owner.create_pane(
        root=root, checkout=checkout, unit="u", target="a" * 40, log=root / "l",
        record=root / "r", pr=1, started_at=iso(0), run=fake, environment=ENV,
        sleep=lambda _s: None,
    )
    check(
        "under the cap, create_pane still creates",
        fake.created == 1 and handle.tab_id == "wC:tNEW",
    )
    tab_create = next(
        command for command in fake.commands if "tab" in command and "create" in command
    )
    tab_argv = tab_create[tab_create.index("herdr"):]
    check(
        "observer shell starts in the durable tool root",
        tab_argv[tab_argv.index("--cwd") + 1] == str(root),
        tab_argv,
    )
    pane_run = next(command for command in fake.commands if "pane" in command and "run" in command)
    pane_argv = pane_run[pane_run.index("herdr"):]
    check(
        "observer still receives the exact disposable checkout",
        pane_argv[pane_argv.index("--checkout") + 1] == str(checkout),
        pane_argv,
    )

# --------------------------------------------------------------------------
print()
if FAILURES:
    print(f"FAILED ({len(FAILURES)}): " + ", ".join(FAILURES))
    raise SystemExit(1)
print("PASS (all pane cap / reap / TTL brackets)")
