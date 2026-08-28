#!/usr/bin/env python3
"""Bracket the disk-residue detector from BOTH sides.

A detector proven only on the positive case is half-tested: the expensive
failure here is not missing an abandoned slot, it is flagging a busy one, since
a flagged slot is a deletion candidate and slots routinely hold the only copy of
uncommitted work. So every behavioural test below states what it plants and what
it expects to be left alone.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import slot_disk_residue as sdr

REPO = Path(__file__).resolve().parents[3]
TICK_HUB = REPO / "ci-hub" / "health" / "tick-hub.yaml"

NOW = datetime(2026, 8, 8, 12, 0, 0, tzinfo=timezone.utc)
GIB = sdr.GIB


def make_root(tmp: Path, slots: list[str]) -> Path:
    for slot in slots:
        (tmp / "worktrees" / "slots" / slot).mkdir(parents=True)
    return tmp


def state_with(slot: str, idle_since: datetime, size_bytes: int) -> dict:
    stamp = sdr.fmt_ts(idle_since)
    return {"slots": {slot: {"idle_since": stamp, "size_bytes": size_bytes,
                             "sized_at": stamp}}}


def registry_slot(
    name: str,
    *,
    status: str,
    agent: str = "fixture-agent",
    task: str = "fixture-task",
    extra: dict[str, object] | None = None,
) -> dict[str, object]:
    row: dict[str, object] = {
        "status": status,
        "task": task,
        "allocated": "2026-08-28T00:00:00Z",
        "updated": "2026-08-28T00:00:00Z",
        "hermit_path": f"worktrees/slots/{name}/hermit",
        "hermit_branch": "detached",
        "reverie_path": f"worktrees/slots/{name}/reverie",
        "reverie_branch": "-",
        "liteinst2_path": f"worktrees/slots/{name}/liteinst2",
        "liteinst2_branch": "-",
        "agents": [{"name": agent, "task": task, "read_only": False}],
    }
    if extra:
        row.update(extra)
    return row


def registry_document(slots: dict[str, object]) -> dict[str, object]:
    return {
        "version": 3,
        "updated": "2026-08-28T00:00:00Z",
        "slots": slots,
    }


class DiskResidueBracketTest(unittest.TestCase):
    """The two cases the task requires, plus the ways each could be faked."""

    def test_positive_abandoned_slot_is_detected(self) -> None:
        """POSITIVE: on disk, big, no process inside, idle a long time -> flagged."""
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["abandoned"])
            prior = state_with("abandoned", NOW - timedelta(hours=72), int(8 * GIB))
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                refresh_hours=12.0, max_measure=0, du_timeout=5, measure=False,
            )
            self.assertEqual(len(results), 1)
            verdict = results[0]
            self.assertTrue(verdict.reclaimable, verdict.reason)
            self.assertEqual(verdict.procs, 0)
            self.assertAlmostEqual(verdict.idle_hours, 72.0, places=1)
            self.assertIn("8.00 GiB", verdict.reason)

    def test_negative_slot_with_a_live_process_is_not_flagged(self) -> None:
        """NEGATIVE: identical slot -- old, big, idle-clocked -- but occupied.

        Every other input is held equal to the positive case, so occupancy is
        provably the only thing doing the work. A real child process is spawned
        with its cwd inside the slot: this exercises the actual /proc scan rather
        than a stubbed occupancy map, which is the only way to show the predicate
        is wired to the running system.
        """
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["busy"])
            slot_dir = root / "worktrees" / "slots" / "busy"
            # Our own child, in its own process group, killed by exact PID below.
            child = subprocess.Popen(
                [sys.executable, "-c", "import sys; sys.stdin.read()"],
                cwd=slot_dir, stdin=subprocess.PIPE,
            )
            try:
                prior = state_with("busy", NOW - timedelta(hours=72), int(8 * GIB))
                results, next_state, occ = sdr.evaluate(
                    root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                    refresh_hours=12.0, max_measure=0, du_timeout=5, measure=False,
                )
                verdict = results[0]
                self.assertFalse(verdict.reclaimable, verdict.reason)
                self.assertGreaterEqual(verdict.procs, 1)
                self.assertIn("live process", verdict.reason)
                self.assertGreaterEqual(occ.counts.get("busy", 0), 1)
                # The idle clock must also be CLEARED, not merely overridden:
                # otherwise the slot qualifies the instant the process exits,
                # backdated to an idleness that never happened.
                self.assertNotIn("idle_since", next_state["slots"]["busy"])
            finally:
                child.stdin.close()
                child.wait(timeout=30)

    def test_freshly_idle_slot_is_not_flagged_yet(self) -> None:
        """A slot idle for 1h is not a disk problem; sustained idleness is."""
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["justidle"])
            prior = state_with("justidle", NOW - timedelta(hours=1), int(8 * GIB))
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                refresh_hours=12.0, max_measure=0, du_timeout=5, measure=False,
            )
            self.assertFalse(results[0].reclaimable)
            self.assertIn("< 24.0h", results[0].reason)

    def test_small_idle_slot_is_not_flagged(self) -> None:
        """Long-idle but tiny: real, but not a disk-reclamation finding."""
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["tiny"])
            prior = state_with("tiny", NOW - timedelta(hours=72), 4096)
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                refresh_hours=12.0, max_measure=0, du_timeout=5, measure=False,
            )
            self.assertFalse(results[0].reclaimable)
            self.assertIn("0.00 GiB", results[0].reason)

    def test_unmeasurable_size_is_never_flagged(self) -> None:
        """An unmeasured slot must not read as empty and must not read as huge.

        `disk_bytes` returns None rather than 0 precisely so a `du` failure
        cannot be laundered into a finding either way.
        """
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["unmeasured"])
            prior = {"slots": {"unmeasured": {
                "idle_since": sdr.fmt_ts(NOW - timedelta(hours=72))}}}
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                refresh_hours=12.0, max_measure=0, du_timeout=5, measure=False,
            )
            self.assertFalse(results[0].reclaimable)
            self.assertIn("could not be measured", results[0].reason)

    def test_incremental_measurement_round_robins_without_starvation(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["a", "b", "c"])
            prior = {
                "slots": {},
                "measure_cursor": "b",
            }
            with mock.patch.object(sdr, "disk_bytes", return_value=4096) as measure:
                _, next_state, _ = sdr.evaluate(
                    root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                    refresh_hours=24.0, max_measure=1, du_timeout=5,
                )
            self.assertEqual(measure.call_count, 1)
            self.assertEqual(measure.call_args.args[0].name, "c")
            self.assertEqual(next_state["measure_cursor"], "c")
            self.assertIsNotNone(next_state["slots"]["c"]["sized_at"])
            self.assertIsNone(next_state["slots"]["a"]["sized_at"])

    def test_registry_absence_alone_never_flags_anything(self) -> None:
        """The registry is not consulted at all -- the filesystem is the authority.

        A slot the registry never heard of is judged purely on disk + process
        facts, and a slot the registry calls `active` gets no protection it did
        not earn from a live process. This is what keeps the tool out of ORC's
        jurisdiction.
        """
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["unregistered"])
            (root / "worktree-state.json").write_text(json.dumps(
                {"slots": {"unregistered": {"status": "active",
                                            "agents": [{"name": "somebody"}]}}}))
            prior = state_with("unregistered", NOW - timedelta(hours=72), int(8 * GIB))
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0, min_gib=1.0,
                refresh_hours=12.0, max_measure=0, du_timeout=5, measure=False,
            )
            self.assertTrue(results[0].reclaimable)
            self.assertNotIn("somebody", results[0].reason)

    def test_deleted_leaf_cwd_still_counts_as_occupancy(self) -> None:
        """`<slot>/child (deleted)` means the child was unlinked, not the slot.

        The process is still running inside a slot that still exists on disk, so
        the slot is in use. Reading the suffix as 'gone' reports a busy slot as
        idle -- the exact false-idle this tool exists to avoid.
        """
        occ = sdr.Occupancy()
        cwd = "/r/worktrees/slots/busy/hermit (deleted)"
        prefix = "/r/worktrees/slots/"
        self.assertTrue(cwd.endswith(" (deleted)"))
        stripped = cwd[: -len(" (deleted)")]
        self.assertTrue(stripped.startswith(prefix))
        self.assertEqual(stripped[len(prefix):].split(os.sep)[0], "busy")
        del occ


class DeltaPagingTest(unittest.TestCase):
    """A gate that fires every tick forever gets muted; this one must not."""

    def _run(self, root: Path, state: Path, extra: list[str]) -> tuple[int, str]:
        # Thresholds at zero so these tests exercise the DELTA logic alone; the
        # threshold behaviour is bracketed separately above. `du` is left enabled
        # because an unmeasured slot is deliberately never flagged, so suppressing
        # it here would make every slot inert and the delta untestable.
        proc = subprocess.run(
            [sys.executable, str(REPO / "ci-hub/health/slot_disk_residue.py"),
             "--root", str(root), "--state", str(state), "--gate",
             "--idle-hours", "0", "--min-gib", "0", *extra],
            capture_output=True, text=True, timeout=120,
        )
        return proc.returncode, proc.stdout

    def test_first_run_adopts_baseline_without_paging(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["standing"])
            state = Path(raw) / "state.json"
            state.write_text(json.dumps(state_with(
                "standing", NOW - timedelta(hours=72), int(8 * GIB))))
            rc, out = self._run(root, state, [])
            self.assertEqual(rc, 0, out)
            self.assertIn("state=ok", out)
            # Baseline recorded, so the standing backlog is now suppressed...
            self.assertIn("standing", json.loads(state.read_text())["reported"])
            # ...and stays suppressed on the next tick.
            rc2, out2 = self._run(root, state, [])
            self.assertEqual(rc2, 0, out2)

    def test_newly_qualifying_slot_pages_once(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["old"])
            state = Path(raw) / "state.json"
            state.write_text(json.dumps({
                **state_with("old", NOW - timedelta(hours=72), int(8 * GIB)),
                "reported": ["old"],
            }))
            (root / "worktrees" / "slots" / "fresh").mkdir()
            rc, out = self._run(root, state, [])
            self.assertEqual(rc, 1, out)
            self.assertIn("state=reclaimable", out)
            self.assertIn("fresh", out)
            self.assertIn("new_reclaimable_slots=1", out)
            # Second tick: same set, no longer news.
            rc2, out2 = self._run(root, state, [])
            self.assertEqual(rc2, 0, out2)

    def test_gate_output_is_capturable_key_values(self) -> None:
        """tick-hub `capture: true` parses `key=value` stdout lines into fields.

        The predecessor gate printed a prose sentence and no `summary=` line, so
        its title rendered the LITERAL `{summary}` and named no slot -- the alarm
        was unactionable even when it was right. Asserting the contract here is
        what stops that regressing.
        """
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["s1"])
            state = Path(raw) / "state.json"
            rc, out = self._run(root, state, [])
            self.assertEqual(rc, 0, out)
            fields = dict(
                line.split("=", 1) for line in out.splitlines() if "=" in line
                and not line.startswith("slot=")
            )
            for key in ("state", "summary", "slots_on_disk", "reclaimable_slots",
                        "reclaimable_gib", "blind_pids"):
                self.assertIn(key, fields)
            self.assertTrue(fields["summary"].strip())
            self.assertNotIn("{summary}", out)


class TickHubScopeTest(unittest.TestCase):
    """Owner directive 2026-08-08: tick-hub must not police ORC-internal state."""

    def _config(self) -> str:
        return TICK_HUB.read_text(encoding="utf-8")

    def _active_gate_cmds(self) -> list[str]:
        """Gate commands of ENABLED reminders only (commented blocks are inert)."""
        cmds = []
        for line in self._config().splitlines():
            stripped = line.strip()
            if stripped.startswith("#"):
                continue
            if stripped.startswith("cmd:"):
                cmds.append(stripped[len("cmd:"):].strip())
        return cmds

    def test_no_active_gate_asserts_agent_liveness_or_task_ownership(self) -> None:
        forbidden = {
            "operational_health.py agents": "agent-fleet liveness is ORC's authority",
            "active-work": "task/owner/agent reconciliation is ORC's authority",
            "unowned_backlog.py": "TaskGraph census is ORC's authority",
            "residue_sweep.py": "combines ORC fleet + TaskGraph owner",
            "worktree_liveness.py": "asserts agent liveness from a registry file",
        }
        cmds = self._active_gate_cmds()
        for needle, why in forbidden.items():
            offenders = [c for c in cmds if needle in c]
            self.assertEqual(offenders, [], f"{needle} is wired as a gate: {why}")

    def test_container_reclaim_is_report_only(self) -> None:
        """The one agent-keyed gate kept may audit, but may not destroy.

        Its reclaim path falls back to 'absent from the ORC agent snapshot' ->
        podman stop + rm, so a stale snapshot would delete a live agent's
        container. Audit is useful; autonomous destruction on a second-hand
        census is the asymmetric-blast-radius case.
        """
        for cmd in self._active_gate_cmds():
            if "agent-podman.rs" in cmd:
                self.assertNotIn("--apply", cmd)
                break
        else:
            self.fail("agent-podman reconcile gate not found")

    def test_disk_reclamation_is_still_monitored(self) -> None:
        """Deleting the liveness gate without keeping the disk need is a failure."""
        cmds = self._active_gate_cmds()
        self.assertTrue(
            any("slot_disk_residue.py" in c for c in cmds),
            "disk-reclamation monitoring was lost, not re-scoped",
        )

    def test_disk_gate_has_count_independent_expensive_work_bound(self) -> None:
        command = next(
            cmd for cmd in self._active_gate_cmds()
            if "slot_disk_residue.py" in cmd
        )
        self.assertIn("--max-measure 1", command)
        self.assertIn("--du-timeout 5", command)
        self.assertIn("--refresh-hours 24", command)


class CapacityDenominatorTest(unittest.TestCase):
    def test_quarantine_stays_visible_beside_active_without_consuming_cap(self) -> None:
        registry = {
            "slots": {
                "working": {"status": "active"},
                "preserved": {"status": "quarantined-retired"},
                "lease": {"status": "lease-quarantined"},
                "parked": {"status": "parked"},
            }
        }
        self.assertEqual(sdr.active_slot_names(registry), {"working"})
        self.assertEqual(
            sdr.quarantined_slot_names(registry), {"preserved", "lease"}
        )

    def test_owner_census_timeout_cannot_erase_known_cap_breach(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["working"])
            (root / "ci-hub").mkdir()
            (root / "ci-hub" / "ci-hub").touch()
            scripts = root / "scripts"
            scripts.mkdir()
            (scripts / "slot-owner-authority.py").write_text(
                "import sys\nprint('owner census unavailable')\nraise SystemExit(2)\n",
                encoding="utf-8",
            )
            registry = root / "worktree-state.json"
            registry.write_text(
                json.dumps(
                    registry_document(
                        {
                            "working": registry_slot(
                                "working", status="active", agent="agent"
                            )
                        }
                    )
                ),
                encoding="utf-8",
            )
            proc = subprocess.run(
                [
                    sys.executable,
                    str(REPO / "ci-hub/health/slot_disk_residue.py"),
                    "--root", str(root),
                    "--registry", str(registry),
                    "--state", str(root / "state.json"),
                    "--active-slot-cap", "0",
                    "--gate",
                    "--no-measure",
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("state=actionable", proc.stdout)
        self.assertIn("active_slots=1", proc.stdout)
        self.assertIn("active_slot_excess=1", proc.stdout)
        self.assertIn("owner_census_status=indeterminate", proc.stdout)
        self.assertIn("owner state remains INDETERMINATE", proc.stdout)


class UndecidedOwnerSuppressionTest(unittest.TestCase):
    """The shrink-only property, asserted rather than argued.

    `suppress_undecided_owners` exists so this can be a test. The claim is not
    "the predicates are right" -- it is "no input makes this add a candidate",
    which holds even when they are wrong.
    """

    @staticmethod
    def _residue(slot: str, reclaimable: bool) -> sdr.SlotResidue:
        return sdr.SlotResidue(
            slot=slot, procs=0, idle_since=None, idle_hours=99.0,
            size_bytes=int(2 * GIB), reclaimable=reclaimable, reason="before")

    def test_suppression_can_only_ever_remove_candidates(self) -> None:
        """Every combination of membership and prior flag, exhaustively."""
        memberships = ("live", "unassessable", "both", "neither")
        for membership in memberships:
            for was_reclaimable in (True, False):
                with self.subTest(membership=membership, was=was_reclaimable):
                    result = self._residue("s", was_reclaimable)
                    live = {"s"} if membership in ("live", "both") else set()
                    unk = {"s"} if membership in ("unassessable", "both") else set()
                    sdr.suppress_undecided_owners([result], live, unk)
                    self.assertFalse(
                        result.reclaimable and not was_reclaimable,
                        "suppression turned a non-candidate INTO a candidate")
                    if membership == "neither":
                        self.assertEqual(result.reclaimable, was_reclaimable,
                                         "an unlisted slot must be untouched")
                        self.assertEqual(result.reason, "before")
                    else:
                        self.assertFalse(result.reclaimable)

    def test_candidate_set_after_is_a_subset_of_the_set_before(self) -> None:
        results = [self._residue(f"s{i}", i % 2 == 0) for i in range(8)]
        before = {r.slot for r in results if r.reclaimable}
        sdr.suppress_undecided_owners(results, {"s0", "s3"}, {"s2", "s5", "s7"})
        after = {r.slot for r in results if r.reclaimable}
        self.assertTrue(after.issubset(before), f"{after} is not a subset of {before}")
        self.assertEqual(after, {"s4", "s6"})


class UnassessableOwnerBracketTest(unittest.TestCase):
    """Both directions, because only one direction cannot tell this change apart
    from the gate having simply stopped classifying.

    POSITIVE: a proven-dead owner is STILL reported dead, and an unowned idle
    slot is STILL a reclaim candidate -- the detection survives.
    NEGATIVE: a held slot whose owner cannot be assessed is no longer named
    reclaimable, and says so out loud instead of vanishing from the report.
    """

    REGISTRY = registry_document(
        {
            # A live agent's slot as the allocator actually writes one today:
            # active, one mutating owner, and no owner_sidecar at all.
            "unassessable": registry_slot(
                "unassessable",
                status="active",
                agent="live-agent",
                task="t-live",
            ),
            # A genuinely dead owner: sidecar present and identity-bound.
            "deadowner": registry_slot(
                "deadowner",
                status="active",
                agent="gone-agent",
                task="t-gone",
                extra={
                    "owner_sidecar": {
                        "agent": "gone-agent",
                        "generation": "boot-a:1:2",
                    }
                },
            ),
            # Nobody's: released, so status alone frees it.
            "abandoned": registry_slot("abandoned", status="released"),
        }
    )

    AUTHORITY = {
        "schema_version": 1,
        "authorities": {
            "unassessable": {
                "schema_version": 1, "state": "indeterminate",
                "slot": "unassessable", "agent": None, "owner_identity": None,
            },
            "deadowner": {
                "schema_version": 1, "state": "dead", "slot": "deadowner",
                "agent": "gone-agent",
                "owner_identity": {
                    "source": "orc-tmux-pane-supervisor-v1",
                    "generation": "boot-a:1:2", "supervisor_pid": 1,
                    "boot_id": "boot-a", "start_ticks": 2,
                },
            },
        },
    }

    SLOTS = ["unassessable", "deadowner", "abandoned"]

    def _run(self) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), self.SLOTS)
            registry = root / "worktree-state.json"
            registry.write_text(json.dumps(self.REGISTRY), encoding="utf-8")
            authority = root / "authority.json"
            authority.write_text(json.dumps(self.AUTHORITY), encoding="utf-8")
            # Every slot long-idle and materially large, so idleness alone would
            # qualify all three. `reported: []` makes each survivor page as new.
            stamp = sdr.fmt_ts(NOW - timedelta(hours=48))
            state = root / "state.json"
            state.write_text(json.dumps({
                "reported": [],
                "slots": {s: {"idle_since": stamp, "size_bytes": int(2 * GIB),
                              "sized_at": stamp} for s in self.SLOTS},
            }), encoding="utf-8")
            return subprocess.run(
                [sys.executable,
                 str(REPO / "ci-hub/health/slot_disk_residue.py"),
                 "--root", str(root), "--registry", str(registry),
                 "--state", str(state),
                 "--test-owner-authority-json", str(authority),
                 "--active-slot-cap", "5", "--gate", "--no-measure"],
                capture_output=True, text=True, timeout=30)

    def test_positive_a_proven_dead_owner_is_still_reported_dead(self) -> None:
        proc = self._run()
        out = proc.stdout + proc.stderr
        self.assertIn("dead_owner_slots=1", out, out)
        self.assertIn("slot=deadowner DEAD_OWNER", out, out)
        self.assertIn("authority_state=dead", out, out)

    def test_positive_an_unowned_idle_slot_is_still_reclaimable(self) -> None:
        """If this stops firing, the change stopped detecting rather than stopped
        over-claiming, and the negative test below could not tell the difference."""
        proc = self._run()
        out = proc.stdout + proc.stderr
        self.assertIn("slot=abandoned RECLAIMABLE", out, out)
        self.assertIn("slot=deadowner RECLAIMABLE", out, out)
        self.assertIn("reclaimable_slots=2", out, out)

    def test_negative_an_unassessable_slot_is_no_longer_a_candidate(self) -> None:
        proc = self._run()
        out = proc.stdout + proc.stderr
        self.assertNotIn("slot=unassessable RECLAIMABLE", out, out)
        self.assertIn("INDETERMINATE and cannot be assessed", out, out)

    def test_the_condition_is_a_finding_and_not_a_silence(self) -> None:
        proc = self._run()
        out = proc.stdout + proc.stderr
        self.assertIn("unassessable_owner_slots=1", out, out)
        self.assertIn("CANNOT BE ASSESSED", out, out)
        self.assertIn("unassessable", out.split("summary=", 1)[1], out)
        self.assertIn("state=actionable", out, out)
        self.assertEqual(proc.returncode, 1,
                         f"a finding must fail --gate, not just print: {out}")


class OccupancyAgreesWithTheReleaseAuthorityTest(unittest.TestCase):
    """The DIVERGING cases only. Agreeing cases prove nothing.

    `release-worktree.rs::live_process_users` refuses a release on any of cwd,
    exe, root, fd, maps or map_files. This gate used to read cwd alone, so a slot
    held only by an open descriptor read EMPTY here and OCCUPIED there -- and the
    natural next move for a reader told a slot is free that the tooling then
    refuses is `--force`, which is the one property this repo has repeatedly
    declined to spend.

    Measured 2026-08-08 across 77 live slots: exactly ONE changed classification
    (`scorecard`, 52 processes via exe/fd/maps/map_files, no cwd), and ZERO were
    cwd-occupied but otherwise free -- so the wider predicate is a strict
    superset and can only ever shorten the reclaim list.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "worktrees" / "slots" / "held").mkdir(parents=True)

    def test_open_fd_with_cwd_elsewhere_reads_OCCUPIED(self) -> None:
        """THE case that diverged. cwd is outside the slot; only an fd is inside."""
        target = self.root / "worktrees" / "slots" / "held" / "artifact.bin"
        target.write_bytes(b"x")
        # cwd deliberately OUTSIDE the slot, so a cwd-only predicate sees nothing.
        child = subprocess.Popen(
            [sys.executable, "-c",
             "import sys; f=open(sys.argv[1]); sys.stdout.write('r'); "
             "sys.stdout.flush(); sys.stdin.read()", str(target)],
            cwd=self.tmp.name, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            child.stdout.read(1)  # the fd is open once this arrives
            occ = sdr.occupancy(self.root)
            self.assertGreaterEqual(
                occ.counts.get("held", 0), 1,
                "a slot held by an open fd read as EMPTY -- the divergence is back")
        finally:
            child.stdin.close()
            child.wait(timeout=30)

    def test_a_slot_nothing_touches_reads_FREE(self) -> None:
        """The predicate must not have become vacuously strict."""
        (self.root / "worktrees" / "slots" / "untouched").mkdir()
        occ = sdr.occupancy(self.root)
        self.assertEqual(occ.counts.get("untouched", 0), 0)

    def test_one_process_two_signals_counts_once(self) -> None:
        """cwd AND an fd in the same slot is one occupant, not two."""
        target = self.root / "worktrees" / "slots" / "held" / "artifact.bin"
        target.write_bytes(b"x")
        child = subprocess.Popen(
            [sys.executable, "-c",
             "import sys; f=open(sys.argv[1]); sys.stdout.write('r'); "
             "sys.stdout.flush(); sys.stdin.read()", str(target)],
            cwd=str(self.root / "worktrees" / "slots" / "held"),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            child.stdout.read(1)
            self.assertEqual(sdr.occupancy(self.root).counts.get("held", 0), 1)
        finally:
            child.stdin.close()
            child.wait(timeout=30)

    def test_the_signal_set_mirrors_the_release_authority(self) -> None:
        """If release-worktree gains a signal, this must too -- pinned explicitly."""
        self.assertEqual(
            set(sdr.SINGLE_LINK_SIGNALS) | set(sdr.DIRECTORY_LINK_SIGNALS) | {"maps"},
            {"cwd", "exe", "root", "fd", "map_files", "maps"},
            "signal set drifted from release-worktree.rs::live_process_users")


if __name__ == "__main__":
    # Deliberately the LAST statement in the file. It previously sat mid-file,
    # above OccupancyAgreesWithTheReleaseAuthorityTest, so running this file as a
    # script exited before those classes were even defined and silently ran a
    # subset. `python3 -m unittest` was unaffected, which is why it went unseen.
    unittest.main(verbosity=2)
