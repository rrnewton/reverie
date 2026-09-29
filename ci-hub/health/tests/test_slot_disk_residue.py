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
import shutil
import signal
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


REGISTRY_CHECKER = REPO / "scripts" / "check-worktree-registry.rs"
# The first run of this rust-script checker from a checkout compiles it, and the
# rust-script cache is keyed per script path, so every fresh worktree pays that
# build once. It took 67 s at host load ~90 on devbig014 (2026-09-29). The 10 s
# and 30 s bounds in the gate subprocess tests below bound the GATE; this bound
# is for the build alone, and is generous because the rust-script build lock is
# shared with every other checkout on the host.
CHECKER_BUILD_BOUND_SECS = 900
_checker_build: dict[str, str] = {}


def require_built_registry_checker() -> None:
    """Build the gate's registry checker once, or refuse naming the precondition.

    A ``worktree-state.json`` fixture makes the gate call
    ``worktree_registry.parse_registry_bytes``, which runs this checker with no
    timeout. Without this step a cold rust-script cache turns a correct gate
    into a subprocess timeout in whichever of those tests happens to run first.
    """
    if "refusal" in _checker_build:
        raise RuntimeError(_checker_build["refusal"])
    if "built" in _checker_build:
        return
    precondition = (
        f"MISSING PRECONDITION: the worktree registry schema checker "
        f"{REGISTRY_CHECKER} must run through rust-script on PATH (its "
        f"#!/usr/bin/env rust-script line) and build within "
        f"{CHECKER_BUILD_BOUND_SECS} s before the gate's own 10 s/30 s bounds "
        f"mean anything; "
    )
    refusal = None
    try:
        proc = subprocess.Popen(
            [str(REGISTRY_CHECKER), "--validate-state-stdin"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        refusal = precondition + f"it could not be executed: {error}"
    else:
        try:
            _, err = proc.communicate(
                json.dumps(registry_document({})).encode(),
                timeout=CHECKER_BUILD_BOUND_SECS,
            )
        except subprocess.TimeoutExpired:
            # Kill rust-script's cargo and rustc children too, not only the
            # process this test started.
            os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate()
            refusal = precondition + "the build did not finish in time"
        else:
            if proc.returncode != 0:
                detail = err.decode("utf-8", errors="replace").strip()[-2000:]
                refusal = (
                    precondition
                    + f"it exited rc={proc.returncode} on an empty registry: "
                    + (detail or "no diagnostic")
                )
    if refusal is not None:
        _checker_build["refusal"] = refusal
        raise RuntimeError(refusal)
    _checker_build["built"] = str(REGISTRY_CHECKER)


class DiskResidueBracketTest(unittest.TestCase):
    """The two cases the task requires, plus the ways each could be faked."""

    def test_positive_abandoned_slot_is_detected(self) -> None:
        """POSITIVE: on disk, no process inside, idle a long time -> candidate."""
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["abandoned"])
            prior = state_with("abandoned", NOW - timedelta(hours=72), int(8 * GIB))
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0,
            )
            self.assertEqual(len(results), 1)
            verdict = results[0]
            self.assertTrue(verdict.reclaimable, verdict.reason)
            self.assertEqual(verdict.procs, 0)
            self.assertAlmostEqual(verdict.idle_hours, 72.0, places=1)
            self.assertNotIn("GiB", verdict.reason)

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
                    root, prior, NOW, idle_hours=24.0,
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
                root, prior, NOW, idle_hours=24.0,
            )
            self.assertFalse(results[0].reclaimable)
            self.assertIn("< 24.0h", results[0].reason)

    def test_legacy_cached_size_is_discarded(self) -> None:
        """A fossilized size must not survive into the next state or verdict."""
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["tiny"])
            prior = state_with("tiny", NOW - timedelta(hours=72), 4096)
            results, next_state, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0,
            )
            self.assertTrue(results[0].reclaimable)
            self.assertNotIn("size_bytes", next_state["slots"]["tiny"])
            self.assertNotIn("sized_at", next_state["slots"]["tiny"])

    def test_unmeasurable_candidate_set_is_never_flagged(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["unmeasured"])
            prior = {"slots": {"unmeasured": {
                "idle_since": sdr.fmt_ts(NOW - timedelta(hours=72))}}}
            results, _, _ = sdr.evaluate(
                root, prior, NOW, idle_hours=24.0,
            )
            count = sdr.apply_size_threshold(
                results,
                sdr.CandidateSetMeasurement(None, 10 * int(GIB), None, "unavailable"),
                min_gib=1.0,
            )
            self.assertEqual(count, 1)
            self.assertFalse(results[0].reclaimable)
            self.assertIn("current candidate-set size unavailable", results[0].reason)

    def test_current_candidate_set_below_threshold_is_not_flagged(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["a", "b", "c"])
            prior = {
                "slots": {
                    slot: {"idle_since": sdr.fmt_ts(NOW - timedelta(hours=72))}
                    for slot in ("a", "b", "c")
                }
            }
            results, _, _ = sdr.evaluate(root, prior, NOW, idle_hours=24.0)
            count = sdr.apply_size_threshold(
                results,
                sdr.CandidateSetMeasurement(int(0.5 * GIB), 10 * int(GIB), None),
                min_gib=1.0,
            )
            self.assertEqual(count, 3)
            self.assertFalse(any(r.reclaimable for r in results))
            self.assertTrue(all("complete candidate set" in r.reason for r in results))

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
                root, prior, NOW, idle_hours=24.0,
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


class CandidateSetMeasurementTest(unittest.TestCase):
    @staticmethod
    def _headroom(device_total: int = int(10 * GIB), data_headroom: int = int(4 * GIB)):
        result = mock.Mock()
        result.device_total = device_total
        result.pool_headroom.return_value = data_headroom
        return result

    def test_one_compsize_call_measures_the_complete_sorted_set(self) -> None:
        output = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=(
                "Processed 2 files, 1 regular extents (2 refs), 0 inline.\n"
                "Type Perc Disk Usage Uncompressed Referenced\n"
                "TOTAL 100% 4194304 4194304 8388608\n"
            ),
            stderr="",
        )
        with mock.patch.object(
            sdr.btrfs_headroom, "measure", return_value=self._headroom()
        ), mock.patch.object(subprocess, "run", return_value=output) as run:
            measured = sdr.measure_candidate_set(Path("/repo"), ["b", "a"], timeout=7)

        self.assertEqual(measured.size_upper_bound_bytes, 4194304)
        self.assertIsNone(measured.error)
        run.assert_called_once_with(
            [
                "compsize",
                "--bytes",
                "/repo/worktrees/slots/a",
                "/repo/worktrees/slots/b",
            ],
            capture_output=True,
            text=True,
            timeout=7,
            check=False,
        )

    def test_impossible_measurement_is_refused_not_printed(self) -> None:
        output = subprocess.CompletedProcess(
            args=[], returncode=0,
            stdout="TOTAL 100% 11811160064 11811160064 11811160064\n",
            stderr="",
        )
        with mock.patch.object(
            sdr.btrfs_headroom,
            "measure",
            return_value=self._headroom(device_total=int(10 * GIB)),
        ), mock.patch.object(subprocess, "run", return_value=output):
            measured = sdr.measure_candidate_set(Path("/repo"), ["a"], timeout=7)

        self.assertIsNone(measured.size_upper_bound_bytes)
        self.assertEqual(measured.error, "candidate-set size exceeds the filesystem capacity")

    def test_unexpected_compsize_output_is_unknown_not_an_exception(self) -> None:
        output = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="a format this reader does not know\n", stderr=""
        )
        with mock.patch.object(
            sdr.btrfs_headroom, "measure", return_value=self._headroom()
        ), mock.patch.object(subprocess, "run", return_value=output):
            measured = sdr.measure_candidate_set(Path("/repo"), ["a"], timeout=7)

        self.assertIsNone(measured.size_upper_bound_bytes)
        self.assertEqual(measured.error, "compsize did not emit exactly one TOTAL row")

    def test_real_reflinks_are_counted_once_for_the_set(self) -> None:
        if shutil.which("compsize") is None:
            self.skipTest("compsize is not installed")
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["a", "b"])
            try:
                sdr.btrfs_headroom.measure(str(root))
            except (sdr.btrfs_headroom.NotBtrfs, OSError, ValueError):
                self.skipTest("temporary directory is not on readable btrfs")
            source = root / "source.bin"
            source.write_bytes(os.urandom(4 * 1024 * 1024))
            copies = [root / "worktrees" / "slots" / slot / "copy.bin"
                      for slot in ("a", "b")]
            for copy in copies:
                copied = subprocess.run(
                    ["cp", "--reflink=always", str(source), str(copy)],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                if copied.returncode != 0:
                    self.skipTest(f"reflink unavailable: {copied.stderr.strip()}")

            one = sdr.measure_candidate_set(root, ["a"], timeout=5)
            both = sdr.measure_candidate_set(root, ["a", "b"], timeout=5)
            self.assertIsNone(one.error, one.error)
            self.assertIsNone(both.error, both.error)
            self.assertEqual(both.size_upper_bound_bytes, one.size_upper_bound_bytes)

    def test_real_nested_btrfs_subvolume_is_not_skipped(self) -> None:
        if shutil.which("compsize") is None or shutil.which("btrfs") is None:
            self.skipTest("compsize or btrfs is not installed")
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["a"])
            try:
                sdr.btrfs_headroom.measure(str(root))
            except (sdr.btrfs_headroom.NotBtrfs, OSError, ValueError):
                self.skipTest("temporary directory is not on readable btrfs")
            nested = root / "worktrees" / "slots" / "a" / "nested"
            created = subprocess.run(
                ["btrfs", "subvolume", "create", str(nested)],
                capture_output=True,
                text=True,
                check=False,
            )
            if created.returncode != 0:
                self.skipTest(f"subvolume creation unavailable: {created.stderr.strip()}")
            blob = nested / "blob"
            try:
                with blob.open("wb") as handle:
                    handle.write(os.urandom(4 * 1024 * 1024))
                    handle.flush()
                    os.fsync(handle.fileno())
                measured = sdr.measure_candidate_set(root, ["a"], timeout=5)
                self.assertIsNone(measured.error, measured.error)
                self.assertGreaterEqual(measured.size_upper_bound_bytes or 0, 4 * 1024 * 1024)
            finally:
                blob.unlink(missing_ok=True)
                nested.rmdir()


class CandidateSetReportTest(unittest.TestCase):
    @staticmethod
    def _old_candidate() -> sdr.SlotResidue:
        return sdr.SlotResidue(
            slot="old", procs=0, idle_since=None, idle_hours=48.0,
            reclaimable=True, reason="idle",
        )

    def test_current_set_replaces_the_fossilized_total(self) -> None:
        results = [self._old_candidate()]
        measurement = sdr.CandidateSetMeasurement(
            int(2.1 * GIB), int(3502.22 * GIB), int(1144.7 * GIB)
        )
        state, summary, fields = sdr.summarize(
            results, sdr.Occupancy(), set(), 1.0, 1, measurement, True
        )

        self.assertEqual(state, sdr.OK)
        self.assertEqual(fields["idle_unprotected_gib_upper_bound"], 2.1)
        self.assertEqual(fields["idle_unprotected_size_status"], "current")
        self.assertNotIn("reclaimable_gib", fields)
        self.assertIn("2.10 GiB as one measured set", summary)
        self.assertNotIn("208.9", summary)

    def test_impossible_or_unavailable_size_has_no_numeric_substitute(self) -> None:
        results = [self._old_candidate()]
        measurement = sdr.CandidateSetMeasurement(
            None, int(3502.22 * GIB), int(1144.7 * GIB),
            "candidate-set size exceeds the filesystem capacity",
        )
        measured_candidates = sdr.apply_size_threshold(results, measurement, 1.0)
        state, summary, fields = sdr.summarize(
            results,
            sdr.Occupancy(),
            set(),
            1.0,
            measured_candidates,
            measurement,
            True,
        )

        self.assertEqual(state, sdr.UNKNOWN)
        self.assertEqual(fields["idle_unprotected_size_status"], sdr.UNKNOWN)
        self.assertNotIn("idle_unprotected_gib_upper_bound", fields)
        self.assertIn("No reclaimable byte figure is reported", summary)

    def test_disabled_threshold_does_not_masquerade_as_a_zero_measurement(self) -> None:
        results = [self._old_candidate()]
        measurement = sdr.CandidateSetMeasurement(None, None, None)
        state, summary, fields = sdr.summarize(
            results, sdr.Occupancy(), set(), 0.0, 1, measurement, False
        )

        self.assertEqual(state, sdr.OK)
        self.assertEqual(fields["idle_unprotected_size_status"], "not-requested")
        self.assertNotIn("idle_unprotected_gib_upper_bound", fields)
        self.assertIn("were not measured", summary)
        self.assertNotIn("0.0 GiB", summary)

    def test_gate_refuses_when_a_required_measurement_is_disabled(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["old"])
            state_file = root / "state.json"
            state_file.write_text(json.dumps({
                "reported": ["gone", "old"],
                "slots": {
                    "old": {
                        "idle_since": sdr.fmt_ts(NOW - timedelta(hours=48)),
                        "size_bytes": int(208.9 * GIB),
                    }
                },
            }), encoding="utf-8")
            proc = subprocess.run(
                [
                    sys.executable,
                    str(REPO / "ci-hub/health/slot_disk_residue.py"),
                    "--root", str(root),
                    "--state", str(state_file),
                    "--gate",
                    "--idle-hours", "0",
                    "--min-gib", "1",
                    "--no-measure",
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            persisted = json.loads(state_file.read_text(encoding="utf-8"))

        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn("state=unknown", proc.stdout)
        self.assertIn("idle_unprotected_size_status=unknown", proc.stdout)
        self.assertNotIn("idle_unprotected_gib_upper_bound=", proc.stdout)
        self.assertNotIn("208.9", proc.stdout)
        self.assertEqual(persisted["reported"], ["old"])
        self.assertNotIn("size_bytes", persisted["slots"]["old"])


class DeltaPagingTest(unittest.TestCase):
    """A gate that fires every tick forever gets muted; this one must not."""

    def _run(self, root: Path, state: Path, extra: list[str]) -> tuple[int, str]:
        # A zero threshold needs no byte measurement, so these tests exercise
        # the delta logic without depending on the host filesystem.
        proc = subprocess.run(
            [sys.executable, str(REPO / "ci-hub/health/slot_disk_residue.py"),
             "--root", str(root), "--state", str(state), "--gate",
             "--idle-hours", "0", "--min-gib", "0", "--no-measure", *extra],
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
            self.assertEqual(rc, 2, out)
            self.assertIn("state=unknown", out)
            self.assertIn("baseline was absent", out)
            fields = dict(
                line.split("=", 1) for line in out.splitlines() if "=" in line
                and not line.startswith("slot=")
            )
            for key in (
                "state",
                "summary",
                "slots_on_disk",
                "reclaimable_slots",
                "idle_unprotected_size_status",
                "blind_pids",
            ):
                self.assertIn(key, fields)
            self.assertTrue(fields["summary"].strip())
            self.assertNotIn("{summary}", out)
            rc2, out2 = self._run(root, state, [])
            self.assertEqual(rc2, 0, out2)
            self.assertIn("state=ok", out2)

    def test_malformed_delta_state_is_unknown_not_a_first_run(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["s1"])
            state = Path(raw) / "state.json"
            state.write_text("{not json", encoding="utf-8")
            rc, out = self._run(root, state, [])
            self.assertEqual(rc, 2, out)
            self.assertIn("state=unknown", out)
            self.assertIn("cannot read delta state", out)

    def test_unanticipated_delta_state_shape_is_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["s1"])
            state = Path(raw) / "state.json"
            state.write_text('{"reported": "s1"}', encoding="utf-8")
            rc, out = self._run(root, state, [])
            self.assertEqual(rc, 2, out)
            self.assertIn("non-string-list 'reported'", out)


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

    def test_disk_gate_has_one_bounded_current_set_measurement(self) -> None:
        command = next(
            cmd for cmd in self._active_gate_cmds()
            if "slot_disk_residue.py" in cmd
        )
        self.assertIn("--size-timeout 5", command)
        self.assertNotIn("--max-measure", command)
        self.assertNotIn("--refresh-hours", command)
        self.assertNotIn("--du-timeout", command)

    def test_tick_does_not_pin_a_predecessor_taskgraph(self) -> None:
        commands = self._active_gate_cmds()
        parity = next(cmd for cmd in commands if "slot_agent_parity.py" in cmd)
        residue = next(cmd for cmd in commands if "slot_disk_residue.py" in cmd)
        # The herdr source reads the fleet live; it names no TaskGraph at all.
        # (a6be4a9f15 moved the tick to `--agent-source snapshot` on 2026-09-19
        # without updating this assertion, which stayed red until 2026-09-28.)
        self.assertEqual(
            'python3 ci-hub/health/slot_agent_parity.py --gate --root '
            '"$DEV_HERMIT_PARENT" --agent-source herdr',
            parity,
        )
        self.assertNotIn("--db", parity)
        self.assertNotIn("worktree-state.json", residue)


class WrkslotsRegistryInputTest(unittest.TestCase):
    @staticmethod
    def write_registry(root: Path, slots: list[dict[str, object]]) -> None:
        (root / ".wrkslots.yml").write_text(
            json.dumps({"machine": "testhost"}), encoding="utf-8")
        (root / "worktrees" / "ACTIVE.testhost.json").write_text(
            json.dumps({
                "schema": 2, "machine": "testhost", "revision": 1, "slots": slots,
            }), encoding="utf-8")

    def test_current_active_registry_produces_a_real_slot_count(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["working"])
            self.write_registry(root, [{
                "slot": "working", "machine": "testhost", "agent": "worker",
                "task": "task", "owner": None,
            }])
            proc = subprocess.run(
                [sys.executable,
                 str(REPO / "ci-hub/health/slot_disk_residue.py"),
                 "--root", str(root), "--state", str(root / "state.json"),
                 "--gate", "--no-measure"],
                capture_output=True, text=True, timeout=10)
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("active_slots=1", proc.stdout)
        self.assertIn("unassessable_owner_slots=1", proc.stdout)
        self.assertNotIn("no producer", proc.stdout)

    def test_missing_active_registry_refuses_instead_of_reporting_zero(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["working"])
            (root / ".wrkslots.yml").write_text(
                json.dumps({"machine": "testhost"}), encoding="utf-8")
            proc = subprocess.run(
                [sys.executable,
                 str(REPO / "ci-hub/health/slot_disk_residue.py"),
                 "--root", str(root), "--state", str(root / "state.json"),
                 "--gate", "--no-measure"],
                capture_output=True, text=True, timeout=10)
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn("state=unknown", proc.stdout)
        self.assertNotIn("active_slots=0", proc.stdout)

    def test_explicit_legacy_registry_keeps_the_no_producer_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = make_root(Path(raw), ["working"])
            self.write_registry(root, [])
            legacy = root / "worktree-state.json"
            legacy.write_text(json.dumps(registry_document({
                "working": registry_slot("working", status="active"),
            })), encoding="utf-8")
            proc = subprocess.run(
                [sys.executable,
                 str(REPO / "ci-hub/health/slot_disk_residue.py"),
                 "--root", str(root), "--registry", str(legacy),
                 "--gate", "--no-measure"],
                capture_output=True, text=True, timeout=10)
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn("state=unknown", proc.stdout)
        self.assertIn("has no producer in this project", proc.stdout)

    def test_exact_wrkslots_owner_is_live_and_reused_pid_is_dead(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            proc_root = root / "proc"
            cgroup_root = root / "cgroup"
            (proc_root / "123").mkdir(parents=True)
            fields = ["S", *("0" for _ in range(18)), "456"]
            (proc_root / "123" / "stat").write_text(
                f"123 (worker) {' '.join(fields)}\n", encoding="utf-8")
            (cgroup_root / "owner").mkdir(parents=True)
            (cgroup_root / "owner" / "cgroup.procs").write_text(
                "", encoding="utf-8")
            row = {
                "agents": [{"name": "worker", "read_only": False}],
                "wrkslots_owner": {
                    "pid": 123, "start_ticks": 456, "boot_id": "boot",
                    "host_id": "host", "cgroup_path": "/owner",
                },
            }
            live = sdr._wrkslots_owner_authority(
                "slot", row, "host", "boot", proc_root, cgroup_root)
            row["wrkslots_owner"]["start_ticks"] = 999
            dead = sdr._wrkslots_owner_authority(
                "slot", row, "host", "boot", proc_root, cgroup_root)
        self.assertEqual(live["state"], "alive")
        self.assertEqual(dead["state"], "dead")


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
        require_built_registry_checker()
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
            reclaimable=reclaimable, reason="before")

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
        require_built_registry_checker()
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
                 "--active-slot-cap", "5", "--gate", "--min-gib", "0",
                 "--no-measure"],
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
