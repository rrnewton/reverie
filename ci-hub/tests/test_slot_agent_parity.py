#!/usr/bin/env python3
"""Brackets for the slot/agent parity gate.

A parity check added in response to a missed leak, that has never been seen to
fire, is the same class of defect as the leak.  So every case below asserts a
NAMED outcome, and the negative cases are the point:

  * a slot with no live agent must RED, and must name the slot;
  * an empty agent census must REFUSE (exit 2), never pass -- a denominator of
    zero computes zero orphans and would otherwise read as perfect health;
  * a stale snapshot must REFUSE.  The real one was 65.8 hours -- 790 ticks --
    old on 2026-08-24, which is why staleness is a refusal and not a caveat;
  * a genuine pass must report a NONZERO `live_agents_examined`, so a green
    verdict carries the denominator that produced it.
"""

from __future__ import annotations

import json
import importlib.util
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "ci-hub" / "health" / "slot_agent_parity.py"
CONVERTED_YAML_CONFIG = (
    ROOT / "ci-hub" / "lib" / "tests" / "fixtures" / "wrkslots-config.converted.yml"
)


def _load_gate_module():
    spec = importlib.util.spec_from_file_location("slot_agent_parity", GATE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["slot_agent_parity"] = module
    spec.loader.exec_module(module)
    return module


parity = _load_gate_module()


def run(root: Path, *args: str) -> tuple[int, dict[str, str]]:
    result = subprocess.run(
        (sys.executable, str(GATE), "--gate", "--root", str(root), *args),
        capture_output=True,
        text=True,
        check=False,
    )
    fields: dict[str, str] = {}
    for line in result.stdout.splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            fields[key] = value
    return result.returncode, fields


class SlotAgentParityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="slot-agent-parity-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "worktrees").mkdir()

    def write_registry(self, slots: dict[str, dict]) -> None:
        """The LIVE wrkslots registry, which is what the gate now reads.

        ⚠️ THESE TESTS USED TO WRITE worktree-state.json AND THEY PASSED WHILE THE
        GATE REPORTED A FOSSIL. That file's producer, scripts/release-worktree.rs,
        refuses every invocation in a project configured with .wrkslots.yml, so
        the real one froze on 2026-08-28 and the gate reported that day's fleet
        for six days. A fixture that writes a file nothing writes in production
        cannot notice that; writing the same registry production writes is what
        makes these tests measure the gate rather than themselves.
        """
        (self.root / ".wrkslots.yml").write_text(
            json.dumps({"machine": "testhost", "worktrees_dir": "worktrees/slots"})
        )
        (self.root / "worktrees" / "ACTIVE.testhost.json").write_text(
            json.dumps(
                {
                    "machine": "testhost",
                    "revision": 1,
                    "schema": 2,
                    "slots": list(slots.values()),
                }
            )
        )

    def slot(
        self,
        name: str,
        agent: str | None,
        slot_type: str = "agent",
    ) -> dict:
        """One wrkslots ACTIVE row.

        There is no `status` field and that is the point: the ACTIVE registry
        holds exactly the live rows and finished ones move to ARCHIVED, so
        PRESENCE is the active status the retired schema spelled out in a field.
        """
        subdir = "validate" if slot_type == "validate" else "slots"
        record: dict = {
            "slot": name,
            "slot_type": slot_type,
            "machine": "testhost",
            "generation": 1,
            "created_at": "2026-09-03T00:00:00Z",
            "agent": agent,
            "checkouts": [
                {
                    "name": "hermit",
                    "repository": "hermit",
                    "path": f"worktrees/{subdir}/{name}/hermit",
                    "branch": "detached",
                }
            ],
        }
        return record

    # --- the predicate itself -------------------------------------------------

    def test_slot_without_a_live_agent_reds_and_names_it(self) -> None:
        self.write_registry(
            {
                "alive": self.slot("alive", "hermit-001"),
                "abandoned": self.slot("abandoned", "hermit-999"),
            }
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 1, "a slot with no live agent must be actionable")
        self.assertEqual(fields["state"], "actionable")
        self.assertEqual(fields["active_slots_without_live_agent"], "1")
        self.assertIn("abandoned", fields["summary"])
        self.assertNotIn("alive", fields["summary"].split(": ")[-1].split(", "))

    def test_every_slot_covered_passes_and_reports_the_denominator(self) -> None:
        self.write_registry(
            {
                "one": self.slot("one", "hermit-001"),
                "two": self.slot("two", "hermit-002"),
            }
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001,hermit-002")
        self.assertEqual(code, 0)
        self.assertEqual(fields["state"], "ok")
        self.assertEqual(fields["active_slots_without_live_agent"], "0")
        # The green verdict must carry its denominator.
        self.assertEqual(fields["live_agents_examined"], "2")
        self.assertNotEqual(fields["live_agents_examined"], "0")

    def test_only_rows_in_the_active_registry_are_judged(self) -> None:
        """A finished slot leaves ACTIVE for ARCHIVED, so it stops being judged
        by being absent rather than by carrying a status."""
        self.write_registry({"live": self.slot("live", "hermit-001")})
        (self.root / "worktrees" / "ARCHIVED.testhost.json").write_text(
            json.dumps(
                {
                    "machine": "testhost",
                    "revision": 1,
                    "schema": 2,
                    "records": [self.slot("retired", "hermit-999")],
                }
            )
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 0, fields.get("summary"))
        self.assertEqual(fields["active_slots"], "1")

    # --- the defect this gate had ---------------------------------------------

    def test_the_figure_moves_when_a_slot_is_actually_reclaimed(self) -> None:
        """THE DEFECT, PINNED. Measured on the live fleet 2026-09-03: a real
        reclaim moved the wrkslots registry from 100 rows to 93 and the git
        worktree census from 181 to 178, and this gate's active_slots stayed at
        exactly 45 -- because it was reading a file whose producer had been
        retired six days earlier. A number that does not move when the thing it
        names is removed is not measuring that thing.
        """
        self.write_registry(
            {
                "alive": self.slot("alive", "hermit-001"),
                "doomed": self.slot("doomed", "hermit-999"),
            }
        )
        code, before = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 1)
        self.assertEqual(before["active_slots"], "2")
        self.assertEqual(before["active_slots_without_live_agent"], "1")

        # Reclaim it, exactly as wrkslots does: the row leaves ACTIVE.
        self.write_registry({"alive": self.slot("alive", "hermit-001")})

        code, after = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(after["active_slots"], "1", "the figure must follow the registry")
        self.assertEqual(after["active_slots_without_live_agent"], "0")
        self.assertEqual(code, 0, after.get("summary"))

    def test_a_validation_slot_is_looked_for_where_it_actually_lives(self) -> None:
        """Validation checkouts sit under worktrees/validate/, not
        worktrees/slots/. Assuming the latter would make a live validation run
        look unoccupied and therefore orphaned."""
        row = self.slot("validate-fresh-abc", None, slot_type="validate")
        self.write_registry({"validate-fresh-abc": row})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(fields["active_slots"], "1")
        # No live agent and no process inside, so it is named -- the point here
        # is that it was searched for under the validate root without error.
        self.assertEqual(code, 1)
        self.assertIn("validate-fresh-abc", fields["summary"])

    def test_an_empty_registry_refuses_rather_than_reporting_parity(self) -> None:
        """Zero rows computes zero orphans and would read as perfect health, the
        same reason a zero-agent census refuses."""
        self.write_registry({})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("no slots", fields["summary"])

    def test_an_unconfigured_machine_shard_refuses_rather_than_guessing(self) -> None:
        self.write_registry({"alive": self.slot("alive", "hermit-001")})
        (self.root / ".wrkslots.yml").write_text(json.dumps({"worktrees_dir": "x"}))
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertIn("machine shard", fields["summary"])

    def test_a_converted_yaml_configuration_selects_the_same_shard(self) -> None:
        """`wrkslots config convert --to yaml` rewrites .wrkslots.yml as YAML.

        The gate reached the file through json.loads, so a converted file made
        every tick refuse with "cannot read" instead of judging the fleet. The
        fixture is the converter's real output with only the machine renamed,
        and the case is an actionable one so the verdict proves the registry
        was actually read.
        """
        self.write_registry(
            {
                "alive": self.slot("alive", "hermit-001"),
                "abandoned": self.slot("abandoned", "hermit-999"),
            }
        )
        converted = CONVERTED_YAML_CONFIG.read_text(encoding="utf-8")
        renamed = converted.replace("machine: devbig014\n", "machine: testhost\n", 1)
        self.assertNotEqual(renamed, converted)
        (self.root / ".wrkslots.yml").write_text(renamed, encoding="utf-8")
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 1, fields.get("summary"))
        self.assertEqual(fields["state"], "actionable")
        self.assertEqual(fields["active_slots_without_live_agent"], "1")
        self.assertIn("abandoned", fields["summary"])

    def test_a_yaml_value_pyyaml_cannot_construct_refuses_rather_than_crashing(
        self,
    ) -> None:
        """PyYAML raises a plain ValueError for a plain `2026-13-45` (its date
        pattern matches, then datetime refuses month 13). That escaped the
        registry reader, and an uncaught exception exits 1 -- this gate's
        "actionable" verdict -- with a traceback, for a fleet it never read.

        agent-utils bada63e reads the value as a string and accepts the file, so
        refusing is a known difference, but it must be a refusal (exit 2), not
        a verdict. The registry holds an orphan, so an accidental read would
        also exit 1 and could not pass for this refusal.
        """
        self.write_registry(
            {
                "alive": self.slot("alive", "hermit-001"),
                "abandoned": self.slot("abandoned", "hermit-999"),
            }
        )
        converted = CONVERTED_YAML_CONFIG.read_text(encoding="utf-8")
        edited = converted.replace(
            "machine: devbig014\n", "machine: testhost\n", 1
        ).replace("  - node_modules\n", "  - node_modules\n  - 2026-13-45\n", 1)
        self.assertEqual(edited.count("testhost"), 1)
        self.assertIn("  - 2026-13-45\n", edited)
        (self.root / ".wrkslots.yml").write_text(edited, encoding="utf-8")
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2, fields.get("summary"))
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("cannot read", fields["summary"])
        self.assertIn("month must be in 1..12", fields["summary"])

    # --- the refusals ---------------------------------------------------------

    def test_empty_agent_census_refuses_rather_than_passes(self) -> None:
        """The failure mode that makes a monitor worse than none."""
        self.write_registry({"abandoned": self.slot("abandoned", "hermit-999")})
        code, fields = run(self.root, "--test-live-agents", "")
        self.assertEqual(code, 2, "an empty census must be unknown, never clean")
        self.assertEqual(fields["state"], "unknown")
        self.assertNotEqual(code, 0)
        self.assertEqual(fields["live_agents_examined"], "0")

    def test_stale_snapshot_refuses(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 65.8 * 3600,
                    "agents": [{"name": "hermit-001", "status": "busy"}],
                }
            )
        )
        code, fields = run(
            self.root,
            "--agent-source",
            "snapshot",
            "--agent-snapshot",
            str(snapshot),
        )
        self.assertEqual(code, 2, "a 65.8h-old snapshot must be refused")
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("older than", fields["summary"])

    def test_fresh_snapshot_is_accepted(self) -> None:
        """The staleness bound must not be a blanket refusal of snapshots."""
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 30,
                    "agents": [{"name": "hermit-001", "status": "busy"}],
                }
            )
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 0, fields.get("summary"))
        self.assertEqual(fields["live_agents_examined"], "1")

    def test_terminal_snapshot_agent_does_not_cover_an_active_slot(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 30,
                    "agents": [{"name": "hermit-001", "status": "unreachable"}],
                }
            )
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 2, "a terminal-only census must refuse its empty denominator")
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("no live agent identities", fields["summary"])

    def test_terminal_snapshot_agent_is_excluded_from_a_mixed_denominator(self) -> None:
        self.write_registry(
            {
                "live": self.slot("live", "hermit-001"),
                "terminal": self.slot("terminal", "hermit-002"),
            }
        )
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 30,
                    "agents": [
                        {"name": "hermit-001", "status": "busy"},
                        {"name": "hermit-002", "status": "unreachable"},
                    ],
                }
            )
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 1)
        self.assertEqual(fields["live_agents_examined"], "1")
        self.assertEqual(fields["active_slots_without_live_agent"], "1")
        self.assertIn("terminal", fields["summary"])

    def test_snapshot_with_no_identities_refuses(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps({"schema_version": 1, "captured_at": time.time(), "agents": []})
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")

    def test_missing_registry_refuses(self) -> None:
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")

    def test_unknown_live_verdict_still_names_the_bound_database(self) -> None:
        missing = self.root / "current-session.db"
        code, fields = run(self.root, "--db", str(missing))
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")
        self.assertEqual(fields["taskgraph_db"], str(missing))

    # --- item 4: the counts nothing else was counting -------------------------

    def test_git_worktree_counts_are_always_reported(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 0)
        self.assertIn("git_worktrees_hermit", fields)
        self.assertIn("git_worktrees_parent", fields)
        self.assertIn("git_worktrees_total", fields)


# --- the herdr + /proc source ---------------------------------------------------

BOOT = "11111111-2222-3333-4444-555555555555"


def stamp(epoch: float) -> str:
    """An ISO-8601 UTC timestamp as wrkslots writes `heartbeat_at`."""
    import datetime as dt

    return dt.datetime.fromtimestamp(epoch, dt.timezone.utc).isoformat(timespec="seconds")


OLD_BOOT = "99999999-8888-7777-6666-555555555555"
HOST = "0123456789abcdef0123456789abcdef"


class HerdrSourceTests(unittest.TestCase):
    """`--agent-source herdr`, run as the tick runs it, against a fake host.

    The fake host is a herdr executable that prints a fixture and a directory
    laid out like /proc.  Every refusal below is a case where the census could
    not see something, and each one must come out as exit 2 (NO-SIGNAL), not
    as a clean fleet and not as an orphan.
    """

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="slot-parity-herdr-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "root"
        (self.root / "worktrees").mkdir(parents=True)
        self.proc = self.base / "proc"
        (self.proc / "sys" / "kernel" / "random").mkdir(parents=True)
        (self.proc / "sys" / "kernel" / "random" / "boot_id").write_text(BOOT + "\n")
        # This boot started a day ago; heartbeats older than that predate it.
        self.boot_time = int(time.time()) - 86400
        (self.proc / "stat").write_text(f"cpu  1 2 3 4\nbtime {self.boot_time}\n")
        self.machine_id = self.base / "machine-id"
        self.machine_id.write_text(HOST + "\n")
        self.outside = self.base / "elsewhere"
        self.outside.mkdir()
        self.herdr = self.base / "herdr"
        self.slots: list[dict] = []

    # fixtures ---------------------------------------------------------------

    def pane(
        self, pane_id: str, name: str | None, status: str, cwd: Path | None = None
    ) -> dict:
        row: dict = {
            "agent": "claude",
            "agent_status": status,
            "focused": False,
            "pane_id": pane_id,
            "revision": 1,
            "state_change_seq": 1,
            "tab_id": pane_id.replace(":p", ":t"),
            "terminal_id": f"term_{pane_id}",
            "workspace_id": pane_id.split(":")[0],
        }
        if name is not None:
            row["name"] = name
        if cwd is not None:
            row["cwd"] = str(cwd)
            row["foreground_cwd"] = str(cwd)
        return row

    def write_herdr(self, panes: list[dict] | None = None, *, script: str | None = None) -> None:
        """A herdr executable.  By default it prints an agent_list document."""
        if script is None:
            fixture = self.base / "herdr-agent-list.json"
            fixture.write_text(
                json.dumps(
                    {
                        "id": "cli:agent:list",
                        "result": {"type": "agent_list", "agents": panes or []},
                    }
                )
            )
            script = (
                "import sys\n"
                "assert sys.argv[1:] == ['agent', 'list'], sys.argv\n"
                f"sys.stdout.write(open({str(fixture)!r}).read())\n"
            )
        self.herdr.write_text(f"#!{sys.executable}\n{script}")
        self.herdr.chmod(0o755)

    def process(
        self,
        pid: int,
        comm: str,
        *,
        ppid: int = 1,
        cwd: Path | None = None,
        pane: str | None = None,
        start: int = 1000,
        state: str = "S",
        unreadable_cwd: bool = False,
        stat: str | None = None,
        unreadable_stat: bool = False,
        unreadable_environ: bool = False,
    ) -> None:
        base = self.proc / str(pid)
        base.mkdir()
        tail = [state, str(ppid)] + ["0"] * 17 + [str(start)] + ["0"] * 10
        if unreadable_stat:
            # Reading a directory fails (EISDIR) with an OSError other than
            # ENOENT, the same path an EACCES on /proc/<pid>/stat takes.
            (base / "stat").mkdir()
        else:
            (base / "stat").write_text(
                stat if stat is not None else f"{pid} ({comm}) {' '.join(tail)}\n"
            )
        uid = os.getuid()
        (base / "status").write_text(f"Name:\t{comm}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n")
        (base / "cmdline").write_bytes(f"/usr/bin/{comm}".encode() + b"\0--flag\0")
        env = b"HOME=/home/test\0"
        if pane is not None:
            env += f"HERDR_PANE_ID={pane}".encode() + b"\0"
        if unreadable_environ:
            (base / "environ").mkdir()
        else:
            (base / "environ").write_bytes(env)
        if unreadable_cwd:
            # readlink on a regular file fails (EINVAL), as /proc/<pid>/cwd does
            # for a process this user may not inspect.
            (base / "cwd").write_text("")
        else:
            os.symlink(cwd or self.outside, base / "cwd")

    def slot_dir(self, name: str) -> Path:
        path = self.root / "worktrees" / "slots" / name / "hermit"
        path.mkdir(parents=True, exist_ok=True)
        return path

    def add_slot(
        self,
        name: str,
        agent: str | None,
        owner: dict | None,
        *,
        heartbeat_age: float | None = 60.0,
        lease: dict | None = None,
    ) -> None:
        """One wrkslots ACTIVE row, shaped as wrkslots writes it.

        The heartbeat defaults to one minute old, inside the 3600 s TTL, as for
        a slot its owner has just allocated or renewed.  ``heartbeat_age=None``
        leaves the stamp out.  ``owner=None`` writes ``"owner": null`` as
        wrkslots does; the coordinator lease defaults to the owner, and is left
        out when both are absent.
        """
        self.slot_dir(name)
        row: dict = {
            "slot": name,
            "slot_type": "agent",
            "machine": "testhost",
            "generation": 1,
            "created_at": "2026-09-28T00:00:00Z",
            "agent": agent,
            "heartbeat_ttl_seconds": 3600,
            "checkouts": [
                {
                    "name": "hermit",
                    "repository": "hermit",
                    "path": f"worktrees/slots/{name}/hermit",
                    "branch": "detached",
                }
            ],
            "owner": owner,
        }
        if heartbeat_age is not None:
            row["heartbeat_at"] = stamp(time.time() - heartbeat_age)
        if lease is None:
            lease = owner
        if lease is not None:
            row["coordinator_lease"] = lease
        self.slots.append(row)

    def owner(self, pid: int, *, start: int = 1000, boot: str = BOOT, host: str = HOST) -> dict:
        return {
            "boot_id": boot,
            "cgroup_path": "/user.slice/test.scope",
            "host_id": host,
            "pid": pid,
            "start_ticks": start,
        }

    def gate(self, *extra: str) -> tuple[int, dict[str, str]]:
        (self.root / ".wrkslots.yml").write_text(
            json.dumps({"machine": "testhost", "worktrees_dir": "worktrees/slots"})
        )
        (self.root / "worktrees" / "ACTIVE.testhost.json").write_text(
            json.dumps(
                {"machine": "testhost", "revision": 1, "schema": 2, "slots": self.slots}
            )
        )
        return run(
            self.root,
            "--agent-source",
            "herdr",
            "--herdr-bin",
            str(self.herdr),
            "--proc-root",
            str(self.proc),
            "--machine-id-file",
            str(self.machine_id),
            *extra,
        )

    # a working census -------------------------------------------------------

    def test_live_and_stale_panes_from_a_herdr_fixture(self) -> None:
        """A live pane's name covers its slot; a stale pane's name does not."""
        self.write_herdr(
            [
                self.pane("wT:p1", "alpha", "working", self.outside),
                self.pane("wT:p2", "beta", "exited", self.outside),
                self.pane("wT:p3", "gamma", "done", self.outside),
                self.pane("wT:p4", "delta", "stopped", self.outside),
            ]
        )
        self.process(100, "claude", pane="wT:p1")
        self.process(101, "claude", ppid=100, pane="wT:p1")
        self.process(300, "claude", pane="wT:p3")
        self.add_slot("slot-alpha", "alpha", self.owner(4242, boot=OLD_BOOT))
        self.add_slot("slot-beta", "beta", self.owner(4243, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 1, fields)
        self.assertEqual(fields["state"], "actionable")
        self.assertEqual(fields["herdr_panes_listed"], "4")
        self.assertEqual(fields["herdr_live_panes"], "2")
        self.assertEqual(fields["herdr_stale_panes"], "2")
        self.assertIn("wT:p2 beta exited", fields["herdr_stale_pane_detail"])
        self.assertIn("wT:p4 delta stopped", fields["herdr_stale_pane_detail"])
        self.assertEqual(fields["live_agents_examined"], "2")
        self.assertEqual(fields["background_agents"], "0")
        self.assertEqual(fields["matched_by_name"], "1")
        self.assertEqual(fields["matched_by_name_detail"], "slot-alpha")
        self.assertEqual(fields["ownerless"], "1")
        self.assertEqual(fields["ownerless_by_reason"], "owner from a previous boot: 1")
        self.assertIn("slot-beta", fields["summary"])
        self.assertNotIn("slot-alpha", fields["summary"])
        self.assertEqual(fields["unseen_agents"], "0")

    def test_live_owner_process_covers_its_slot(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(555, "codex", start=777)
        self.add_slot("owned", "someone-else", self.owner(555, start=777))
        code, fields = self.gate()
        self.assertEqual(code, 0, fields)
        self.assertEqual(fields["state"], "ok")
        self.assertEqual(fields["matched_by_owner"], "1")
        self.assertEqual(fields["background_agents"], "1")
        self.assertEqual(fields["live_agents_examined"], "2")
        self.assertIn("pid 555 codex", fields["background_agent_detail"])

    def test_dead_owner_is_named_by_how_it_died(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(555, "bash", start=999)
        self.add_slot("reused", None, self.owner(555, start=777))
        self.add_slot("gone", None, self.owner(556))
        self.add_slot("zombie", None, self.owner(557))
        self.process(557, "codex", state="Z")
        code, fields = self.gate()
        self.assertEqual(code, 1, fields)
        self.assertEqual(fields["ownerless"], "3")
        self.assertEqual(
            fields["ownerless_by_reason"],
            "owner is a zombie: 1, owner pid gone: 1, owner pid reused: 1",
        )
        self.assertEqual(fields["processes_vanished_or_zombie"], "1")

    def test_a_process_inside_the_slot_covers_it(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.add_slot("busy", None, self.owner(4242, boot=OLD_BOOT))
        self.add_slot("agent-here", None, self.owner(4243, boot=OLD_BOOT))
        self.process(700, "cargo", cwd=self.slot_dir("busy") / "target")
        self.process(800, "codex", cwd=self.slot_dir("agent-here"))
        code, fields = self.gate()
        self.assertEqual(code, 0, fields)
        self.assertEqual(fields["matched_by_process_cwd"], "1")
        self.assertEqual(fields["matched_by_agent_cwd"], "1")
        self.assertEqual(fields["ownerless"], "0")

    def test_an_agent_child_is_not_a_second_agent(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(200, "claude")
        self.process(201, "node", ppid=200)
        self.process(202, "codex", ppid=201)
        self.add_slot("owned", None, self.owner(200))
        code, fields = self.gate()
        self.assertEqual(code, 0, fields)
        self.assertEqual(fields["background_agents"], "1")
        self.assertEqual(fields["live_agents_examined"], "2")

    # what the census could not see ---------------------------------------------

    def test_an_unreadable_agent_cwd_is_unseen_and_withholds_ownerless(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(900, "codex", unreadable_cwd=True)
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["state"], "unknown")
        self.assertEqual(fields["unseen_agents"], "1")
        self.assertIn("pid 900 codex", fields["unseen_agent_detail"])
        self.assertIn("EINVAL", fields["unseen_agent_detail"])
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(fields["unverified"], "1")
        self.assertEqual(fields["ownerless_withheld_for_unseen_agents"], "1")
        self.assertIn("could not be seen", fields["summary"])
        self.assertIn("dead-owner", fields["summary"])

    def test_an_unreadable_non_agent_cwd_is_counted_not_blocking(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(901, "systemd", unreadable_cwd=True)
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 1, fields)
        self.assertEqual(fields["unseen_agents"], "0")
        self.assertEqual(fields["processes_cwd_unreadable_non_agent"], "1")
        self.assertEqual(fields["processes_cwd_unreadable_same_uid"], "systemd x1")

    def test_an_agent_in_a_pane_herdr_does_not_report_live_is_unseen(self) -> None:
        self.write_herdr(
            [
                self.pane("wT:p1", "alpha", "idle", self.outside),
                self.pane("wT:p2", "beta", "dead", self.outside),
            ]
        )
        self.process(100, "claude", pane="wT:p1")
        self.process(200, "claude", pane="wT:p2")
        self.process(300, "codex", pane="wT:p9")
        self.add_slot("beta-slot", "beta", self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["agent_processes_outside_live_panes"], "2")
        self.assertEqual(fields["unseen_agents"], "2")
        self.assertIn("herdr reports that pane dead", fields["unseen_agent_detail"])
        self.assertIn("herdr does not list that pane", fields["unseen_agent_detail"])
        self.assertEqual(fields["live_agents_examined"], "3")

    def test_a_slot_with_no_owner_is_unverified_not_a_breach(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.add_slot("legacy", "nobody-live", None)
        self.add_slot("foreign", None, self.owner(100, host="f" * 32))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["state"], "unknown")
        self.assertEqual(fields["unverified"], "2")
        self.assertEqual(
            fields["unverified_by_reason"],
            "no owner recorded; no usable coordinator lease: 1, owner on another host: 1",
        )
        self.assertIn("legacy", fields["summary"])

    # a live owner is not enough by itself (review finding 0) -------------------

    def test_a_live_owner_hoarding_idle_slots_is_a_breach(self) -> None:
        """One live lane, several slots, heartbeats past the TTL, nothing inside.

        Measured 2026-09-29: one codex process held 27 slots and one claude
        process 20, almost all with heartbeats older than the 3600 s TTL and no
        process inside.  "owner alive" cleared every one and the gate said
        "parity holds".
        """
        self.write_herdr([self.pane("wT:p1", "kvm", "working", self.outside)])
        self.process(555, "codex", start=777, pane="wT:p1")
        for index in range(3):
            self.add_slot(
                f"hoard-{index}", "kvm", self.owner(555, start=777), heartbeat_age=7200
            )
        self.add_slot("renewed", "kvm", self.owner(555, start=777), heartbeat_age=60)
        self.add_slot("in-use", "kvm", self.owner(555, start=777), heartbeat_age=7200)
        self.process(700, "cargo", cwd=self.slot_dir("in-use"))
        code, fields = self.gate()
        self.assertEqual(code, 1, fields)
        self.assertEqual(fields["state"], "actionable")
        self.assertNotIn("parity holds", fields["summary"])
        self.assertEqual(fields["idle_past_ttl"], "3")
        self.assertEqual(fields["idle_past_ttl_detail"], "hoard-0, hoard-1, hoard-2")
        self.assertEqual(fields["idle_past_ttl_by_owner"], "pid 555 codex pane wT:p1 kvm: 3")
        self.assertEqual(fields["live_owners"], "1")
        self.assertIn(
            "pid 555 codex pane wT:p1 kvm: 5 held, 3 idle past TTL",
            fields["slots_by_live_owner"],
        )
        self.assertEqual(
            fields["active_slots_against_live_agents"],
            "5 active slot(s) against 1 live agent(s)",
        )
        self.assertEqual(fields["matched_by_owner"], "1")
        self.assertEqual(fields["matched_by_process_cwd"], "1")
        self.assertEqual(fields["ownerless"], "0")
        self.assertIn("pid 555 codex pane wT:p1 kvm: 3", fields["summary"])
        self.assertIn("hoard-0", fields["summary"])

    def test_a_live_owner_with_an_unreadable_heartbeat_is_not_clean(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(555, "codex", start=777, pane="wT:p1")
        self.add_slot("no-stamp", "alpha", self.owner(555, start=777), heartbeat_age=None)
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertNotIn("parity holds", fields["summary"])
        self.assertEqual(
            fields["unverified_by_reason"],
            "owner alive, heartbeat unreadable, nothing inside: 1",
        )
        self.assertEqual(fields["matched_by_owner"], "0")

    def test_idle_past_ttl_is_withheld_while_an_agent_is_unseen(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(555, "codex", start=777, pane="wT:p1")
        self.process(900, "claude", unreadable_cwd=True)
        self.add_slot("hoard", "alpha", self.owner(555, start=777), heartbeat_age=7200)
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["idle_past_ttl"], "0")
        self.assertEqual(fields["idle_past_ttl_withheld_for_unseen_agents"], "1")
        self.assertEqual(
            fields["unverified_by_reason"],
            "owner alive, heartbeat past TTL, but an agent was unseen: 1",
        )

    # a row with no owner is judged by its lease (review finding 1) --------------

    def test_a_previous_boot_lease_with_no_owner_is_ownerless(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        before_boot = time.time() - self.boot_time + 3600
        self.add_slot(
            "leaked",
            "nobody-live",
            None,
            lease=self.owner(4242, boot=OLD_BOOT),
            heartbeat_age=before_boot,
        )
        code, fields = self.gate()
        self.assertEqual(code, 1, fields)
        self.assertEqual(fields["ownerless"], "1")
        self.assertEqual(
            fields["ownerless_by_reason"],
            "no owner; coordinator lease and last heartbeat from a previous boot: 1",
        )
        self.assertIn("leaked", fields["summary"])

    def test_a_lease_that_is_not_proof_leaves_the_row_unverified(self) -> None:
        """Each missing link in the lease proof names itself and is not a breach."""
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        before_boot = time.time() - self.boot_time + 3600
        old = self.owner(4242, boot=OLD_BOOT)
        self.add_slot("this-boot", None, None, lease=self.owner(4242), heartbeat_age=before_boot)
        self.add_slot("renewed", None, None, lease=old, heartbeat_age=7200)
        self.add_slot(
            "foreign", None, None, lease=self.owner(4242, boot=OLD_BOOT, host="f" * 32),
            heartbeat_age=before_boot,
        )
        self.add_slot("no-stamp", None, None, lease=old, heartbeat_age=None)
        self.add_slot("named", "alpha", None, lease=old, heartbeat_age=before_boot)
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(
            fields["unverified_by_reason"],
            "no owner recorded; coordinator lease from another host: 1, "
            "no owner recorded; coordinator lease from this boot: 1, "
            "no owner recorded; heartbeat renewed during this boot: 1, "
            "no owner recorded; heartbeat unreadable: 1, "
            "no owner recorded; lease from a previous boot, but a live herdr pane "
            "carries the agent name: 1",
        )
        self.assertEqual(fields["matched_by_name"], "0")

    def test_an_unreadable_boot_time_withholds_the_lease_proof(self) -> None:
        (self.proc / "stat").unlink()
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.add_slot(
            "leaked", None, None, lease=self.owner(4242, boot=OLD_BOOT),
            heartbeat_age=3 * 86400,
        )
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(
            fields["unverified_by_reason"], "no owner recorded; boot time unreadable: 1"
        )

    # a reused pane name does not revive a dead owner (review finding 2) ---------

    def test_a_dead_owner_is_not_revived_by_a_reused_pane_name(self) -> None:
        """kvm-pr628-publish-01a0d5c2 and liteinst2-upstream-20260922, 2026-09-29."""
        self.write_herdr([self.pane("wT:p1", "kvm", "working", self.outside)])
        self.process(100, "codex", pane="wT:p1")
        self.add_slot(
            "kvm-old", "kvm", self.owner(4242, boot=OLD_BOOT), heartbeat_age=3 * 86400
        )
        self.add_slot("kvm-recent", "kvm", self.owner(4243), heartbeat_age=60)
        code, fields = self.gate()
        self.assertEqual(code, 1, fields)
        self.assertEqual(fields["ownerless"], "1")
        self.assertEqual(fields["ownerless_despite_live_pane_name"], "1")
        self.assertEqual(fields["ownerless_despite_live_pane_name_detail"], "kvm-old")
        self.assertEqual(fields["ownerless_by_reason"], "owner from a previous boot: 1")
        # A dead owner whose heartbeat is still inside the TTL keeps the name match.
        self.assertEqual(fields["matched_by_name"], "1")
        self.assertEqual(fields["matched_by_name_detail"], "kvm-recent")
        self.assertIn("does not revive a dead owner", fields["summary"])

    # the guards that decide refusal or proof of death (review finding 3) --------

    def test_an_unreadable_boot_id_is_unverified_never_dead(self) -> None:
        (self.proc / "sys" / "kernel" / "random" / "boot_id").unlink()
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.add_slot("mine", None, self.owner(4242), heartbeat_age=7200)
        self.add_slot(
            "leased", None, None, lease=self.owner(4243, boot=OLD_BOOT),
            heartbeat_age=3 * 86400,
        )
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(
            fields["unverified_by_reason"],
            "boot id unreadable: 1, no owner recorded; boot id unreadable: 1",
        )

    def test_an_unreadable_machine_id_is_unverified_never_dead(self) -> None:
        self.machine_id.unlink()
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.add_slot("mine", None, self.owner(4242, boot=OLD_BOOT), heartbeat_age=7200)
        self.add_slot(
            "leased", None, None, lease=self.owner(4243, boot=OLD_BOOT),
            heartbeat_age=3 * 86400,
        )
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(
            fields["unverified_by_reason"],
            "machine id unreadable: 1, no owner recorded; machine id unreadable: 1",
        )

    def test_an_agent_whose_stat_is_unreadable_is_unseen(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(900, "claude", unreadable_stat=True)
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["unseen_agents"], "1")
        self.assertIn(
            "pid 900 (unidentifiable: stat EISDIR; argv0 claude)",
            fields["unseen_agent_detail"],
        )
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(fields["ownerless_withheld_for_unseen_agents"], "1")

    def test_a_process_that_cannot_be_identified_is_unseen(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(901, "codex", stat="no parentheses here\n")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["unseen_agents"], "1")
        self.assertIn(
            "pid 901 (unidentifiable: stat malformed stat; argv0 codex)",
            fields["unseen_agent_detail"],
        )
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(fields["ownerless_withheld_for_unseen_agents"], "1")

    def test_a_live_pane_with_no_cwd_and_no_process_is_unseen(self) -> None:
        self.write_herdr(
            [
                self.pane("wT:p1", "alpha", "idle", self.outside),
                self.pane("wT:p2", "ghost", "working"),
            ]
        )
        self.process(100, "claude", pane="wT:p1")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["unseen_agents"], "1")
        self.assertIn(
            "herdr pane wT:p2 ghost (working) has no cwd and no process carries its pane id",
            fields["unseen_agent_detail"],
        )
        self.assertEqual(fields["ownerless"], "0")
        self.assertEqual(fields["ownerless_withheld_for_unseen_agents"], "1")

    # what the census reports about itself (review finding 4) --------------------

    def test_an_agent_whose_environment_is_unreadable_is_unseen(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(902, "codex", unreadable_environ=True)
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["agent_processes_environ_unreadable"], "1")
        self.assertEqual(fields["unseen_agents"], "1")
        self.assertIn(
            "pid 902 codex", fields["unseen_agent_detail"]
        )
        self.assertIn("environment EISDIR", fields["unseen_agent_detail"])
        self.assertEqual(fields["ownerless"], "0")

    def test_the_claude_and_codex_limit_is_stated_and_other_programs_named(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "idle", self.outside)])
        self.process(100, "claude", pane="wT:p1")
        self.process(903, "muse")
        self.process(904, "agentterm")
        self.process(905, "agentterm")
        self.add_slot("owned", None, self.owner(100))
        code, fields = self.gate()
        self.assertEqual(code, 0, fields)
        self.assertEqual(fields["live_agents_examined"], "1")
        self.assertIn("counts only processes named claude or codex", fields["agent_census_limit"])
        self.assertEqual(
            fields["uncounted_agent_program_processes"], "agentterm x2, muse x1"
        )

    def test_every_captured_field_is_one_inert_line(self) -> None:
        """Braces and newlines from paths and pane names never reach tick-hub raw."""
        odd = self.base / "odd{dir}\nphantom=1"
        odd.mkdir()
        self.write_herdr(
            [
                self.pane("wT:p1", "alpha", "idle", self.outside),
                self.pane("wT:p2", "#{session_name}", "exited", self.outside),
            ]
        )
        self.process(100, "claude", pane="wT:p1")
        self.process(200, "codex", cwd=odd)
        self.add_slot("owned", None, self.owner(100))
        code, fields = self.gate()
        self.assertEqual(code, 0, fields)
        self.assertNotIn("phantom", fields)
        for key, value in fields.items():
            with self.subTest(key):
                self.assertNotIn("{", value)
                self.assertNotIn("}", value)
        self.assertIn("odd(dir) phantom=1", fields["background_agent_detail"])
        self.assertIn("#(session_name)", fields["herdr_stale_pane_detail"])

    # herdr cannot answer: NO-SIGNAL, never zero agents -------------------------

    def assert_no_signal(self, fields: dict[str, str], code: int, needle: str) -> None:
        self.assertEqual(code, 2, fields)
        self.assertEqual(fields["state"], "unknown")
        self.assertEqual(fields["live_agents_examined"], "0")
        self.assertEqual(fields["agent_source"], "herdr")
        self.assertIn(needle, fields["summary"])
        self.assertNotIn("ownerless", fields)

    def test_herdr_absent_is_no_signal(self) -> None:
        self.process(100, "claude")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()  # self.herdr was never written
        self.assert_no_signal(fields, code, "cannot run")

    def test_herdr_not_on_path_is_no_signal(self) -> None:
        self.process(100, "claude")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        (self.root / ".wrkslots.yml").write_text(json.dumps({"machine": "testhost"}))
        result = subprocess.run(
            (
                sys.executable, str(GATE), "--gate", "--root", str(self.root),
                "--agent-source", "herdr", "--proc-root", str(self.proc),
                "--machine-id-file", str(self.machine_id),
            ),
            capture_output=True,
            text=True,
            check=False,
            env={**os.environ, "PATH": str(self.base / "empty-path")},
        )
        fields = dict(
            line.partition("=")[::2] for line in result.stdout.splitlines() if "=" in line
        )
        self.assert_no_signal(fields, result.returncode, "herdr is not on PATH")

    def test_herdr_failure_is_no_signal(self) -> None:
        self.process(100, "claude")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        cases = {
            "exited 3": "import sys\nsys.stderr.write('server not running')\nsys.exit(3)\n",
            "did not print JSON": "print('not json')\n",
            "not an agent_list": "print('{\"result\": {\"type\": \"pane_list\"}}')\n",
            "repeats the JSON field": (
                "print('{\"result\": {\"type\": \"agent_list\", \"agents\": [], "
                "\"agents\": []}}')\n"
            ),
            "unclassified agent_status": (
                "import json\nprint(json.dumps({'result': {'type': 'agent_list', "
                "'agents': [{'pane_id': 'wT:p1', 'agent': 'claude', "
                "'agent_status': 'hibernating'}]}}))\n"
            ),
        }
        for needle, script in cases.items():
            with self.subTest(needle):
                self.write_herdr(script=script)
                code, fields = self.gate()
                self.assert_no_signal(fields, code, needle)

    def test_herdr_timeout_is_no_signal(self) -> None:
        self.process(100, "claude")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        self.write_herdr(script="import time\ntime.sleep(30)\n")
        started = time.monotonic()
        code, fields = self.gate("--herdr-timeout-secs", "0.5")
        self.assertLess(time.monotonic() - started, 20)
        self.assert_no_signal(fields, code, "did not answer within 0.5 s")

    def test_no_live_agent_at_all_refuses(self) -> None:
        self.write_herdr([self.pane("wT:p1", "alpha", "exited", self.outside)])
        self.process(100, "bash")
        self.add_slot("dead-owner", None, self.owner(4242, boot=OLD_BOOT))
        code, fields = self.gate()
        self.assert_no_signal(fields, code, "no live agent found")

    # the status table is the Rust one -----------------------------------------

    def test_herdr_status_table_matches_the_rust_liveness_contract(self) -> None:
        """The mirrored live set must agree with scripts/agent_snapshot.rs.

        The gate mirrors the table so the tick never compiles Rust; this is
        the check that the mirror has not drifted.
        """
        statuses = set(parity.HERDR_STATUS_MAP.values()) | parity.LIVE_SNAPSHOT_STATUSES
        for status in sorted(statuses):
            with self.subTest(status):
                self.assertEqual(
                    status in parity.LIVE_SNAPSHOT_STATUSES,
                    parity.agent_snapshot_contract.status_semantics(status).live,
                )

    def test_tick_runs_the_herdr_source(self) -> None:
        import yaml

        config = yaml.safe_load((ROOT / "ci-hub/health/tick-hub.yaml").read_text())
        gate = {row["name"]: row for row in config["reminders"]}["slot_agent_parity"][
            "gate"
        ]
        self.assertTrue(gate["cmd"].endswith("--agent-source herdr"), gate["cmd"])
        self.assertNotIn("--max-source-age-secs", gate["cmd"])


class TaskGraphBindingTests(unittest.TestCase):
    def test_explicit_database_wins_without_running_discovery(self) -> None:
        with mock.patch.object(parity.subprocess, "run") as discover:
            path = parity.live_taskgraph_db(Path("/tmp/explicit.db"))
        self.assertEqual(path, Path("/tmp/explicit.db"))
        discover.assert_not_called()

    def test_environment_database_wins_without_running_discovery(self) -> None:
        with mock.patch.dict(
            os.environ, {"TG_DB_PATH": "/tmp/current-session.db"}
        ), mock.patch.object(parity.subprocess, "run") as discover:
            path = parity.live_taskgraph_db()
        self.assertEqual(path, Path("/tmp/current-session.db"))
        discover.assert_not_called()

    def test_unbound_tick_derives_database_from_live_coordinator(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="/home/test/.tg/hermit3.db\n", stderr=""
        )
        with mock.patch.dict(os.environ, {}, clear=False), mock.patch.object(
            parity.subprocess, "run", return_value=completed
        ) as discover:
            os.environ.pop("TG_DB_PATH", None)
            path = parity.live_taskgraph_db()
        self.assertEqual(path, Path("/home/test/.tg/hermit3.db"))
        self.assertIn("--print-db-path", discover.call_args.args[0])

    def test_failed_live_discovery_refuses_instead_of_guessing(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="", stderr="two Hermit coordinators"
        )
        with mock.patch.dict(os.environ, {}, clear=False), mock.patch.object(
            parity.subprocess, "run", return_value=completed
        ):
            os.environ.pop("TG_DB_PATH", None)
            with self.assertRaises(parity.ParityUnavailable) as caught:
                parity.live_taskgraph_db()
        self.assertIn("two Hermit coordinators", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
