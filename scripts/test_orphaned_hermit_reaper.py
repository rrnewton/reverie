#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("orphaned-hermit-reaper.py")
SPEC = importlib.util.spec_from_file_location("orphaned_hermit_reaper", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
reaper = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = reaper
SPEC.loader.exec_module(reaper)


UID = 212630
WORKSPACE = Path("/home/test/work/dev-hermit")
CGROUP = (
    "/user.slice/user-212630.slice/user@212630.service/"
    "3pai_sandbox.slice/run-p847299-iABC.scope"
)


def snapshot(**overrides: object) -> reaper.ProcessSnapshot:
    values: dict[str, object] = {
        "pid": 1234,
        "start_ticks": 10_000,
        "ppid": 1,
        "uid": UID,
        "state": "R",
        "cpu_seconds": 900.0,
        "age_seconds": 1000.0,
        "exe": WORKSPACE / "hermit/target/release/hermit",
        "argv": ("hermit", "--backend", "kvm", "run", "--", "/bin/true"),
        "cgroup": CGROUP,
    }
    values.update(overrides)
    return reaper.ProcessSnapshot(**values)


class SelectionTests(unittest.TestCase):
    def decision(self, item: reaper.ProcessSnapshot) -> reaper.Decision:
        return reaper.decide(
            item,
            caller_uid=UID,
            workspace=WORKSPACE,
            bounds=reaper.Bounds(),
        )

    def test_proven_runaway_is_candidate(self) -> None:
        verdict = self.decision(snapshot())
        self.assertTrue(verdict.eligible)
        self.assertEqual(verdict.kind, "hermit-run")

    def test_every_binding_is_fail_closed(self) -> None:
        cases = {
            "live-parent": snapshot(ppid=42),
            "other-user": snapshot(uid=UID + 1),
            "other-cgroup": snapshot(cgroup="/user.slice/untrusted.scope"),
            "outside-workspace": snapshot(exe=Path("/tmp/hermit")),
            "not-run": snapshot(argv=("hermit", "coordinator")),
            "young": snapshot(age_seconds=100.0),
            "low-cpu": snapshot(cpu_seconds=100.0),
            "not-spinning": snapshot(cpu_seconds=600.0, age_seconds=2000.0),
            "zombie": snapshot(state="Z"),
        }
        for label, item in cases.items():
            with self.subTest(label=label):
                self.assertFalse(self.decision(item).eligible)

    def test_cargo_test_under_target_deps_is_candidate(self) -> None:
        item = snapshot(
            exe=WORKSPACE / "hermit/target/debug/deps/detcore_misc-abcdef",
            argv=("detcore_misc-abcdef", "--nocapture"),
        )
        verdict = self.decision(item)
        self.assertTrue(verdict.eligible)
        self.assertEqual(verdict.kind, "cargo-test")

    def test_similarly_named_path_is_not_a_test(self) -> None:
        item = snapshot(exe=WORKSPACE / "targetish/debug/deps/fake")
        self.assertFalse(self.decision(item).eligible)


class DiskCriterionTests(unittest.TestCase):
    """The low-CPU disk writer -- the orphan the CPU gate was blind to.

    Every suppression case below has a twin that asserts the reaper still acts,
    because a criterion that only ever says no is not a fix.
    """

    # A real orphan measured on 2026-08-19: 3h50m old, 41.6s CPU, ratio 0.003
    # against a 0.80 gate. It was writing to a log the whole time.
    LOW_CPU = {"cpu_seconds": 41.6, "age_seconds": 13808.0}

    def decision(self, rate, **overrides):
        values = dict(self.LOW_CPU)
        values.update(overrides)
        return reaper.decide(
            snapshot(**values),
            caller_uid=UID,
            workspace=WORKSPACE,
            bounds=reaper.Bounds(),
            write_bytes_per_second=rate,
        )

    def test_low_cpu_disk_writer_is_now_a_candidate(self) -> None:
        """THE POINT. ratio 0.003 against a 0.80 gate, qualified by bytes."""
        verdict = self.decision(5.0 * reaper.MIB)
        self.assertTrue(verdict.eligible, verdict.reasons)

    def test_low_cpu_quiet_orphan_is_still_refused(self) -> None:
        """THE TWIN. Same process, not writing -- must stay untouched.

        This is what stops the criterion becoming "kill every idle process",
        which is what lowering 0.80 to 0.003 would have done.
        """
        verdict = self.decision(0.0)
        self.assertFalse(verdict.eligible)
        self.assertIn("write 0.00 MiB/s < 1.00 MiB/s", verdict.reasons)

    def test_a_write_that_already_finished_reads_zero_and_is_refused(self) -> None:
        """A bounded emitter is zero across the window, so it cannot qualify.

        The liveness property is intrinsic to sampling twice in one invocation;
        there is no state file to hold a stale interval.
        """
        self.assertFalse(self.decision(0.0).eligible)

    def test_cpu_path_is_unchanged_when_nothing_was_measured(self) -> None:
        """Unmeasured must behave exactly as before this criterion existed."""
        self.assertTrue(
            reaper.decide(
                snapshot(), caller_uid=UID, workspace=WORKSPACE, bounds=reaper.Bounds()
            ).eligible
        )
        verdict = self.decision(None)
        self.assertFalse(verdict.eligible)
        self.assertIn("disk write rate not measured", verdict.reasons)

    def test_cpu_thresholds_are_not_widened(self) -> None:
        """Guard against anyone 'fixing' this by loosening the ratio instead."""
        self.assertEqual(reaper.Bounds().min_core_ratio, 0.80)
        self.assertEqual(reaper.Bounds().min_cpu_seconds, 600.0)

    def test_a_busy_writer_below_the_age_bound_is_still_refused(self) -> None:
        """The disk path widens WHAT counts as damage, never how new a process may be."""
        self.assertFalse(
            self.decision(500.0 * reaper.MIB, age_seconds=10.0).eligible
        )

    def _fake_proc(self, root: Path, pid: int, ppid: int, write_bytes: int) -> None:
        base = root / str(pid)
        base.mkdir(exist_ok=True)
        fields = ["S", str(ppid)] + ["0"] * 9 + ["0", "0"] + ["0"] * 6 + ["0"]
        (base / "stat").write_text(f"{pid} (proc) " + " ".join(fields))
        (base / "io").write_text(
            "rchar: 0\nwchar: 0\nwrite_bytes: {}\n".format(write_bytes)
        )

    def test_write_bytes_are_summed_over_the_TREE_not_the_root(self) -> None:
        """The trap. Writes land on the guest child; the hermit root reads zero.

        Measured 2026-08-19 on an orphaned tree: the hermit process showed
        write_bytes 0 -> 0 while its guest child showed 84.80 MiB/s. A gate
        reading /proc/<hermit>/io alone reproduces the old blindness.
        """
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self._fake_proc(root, 100, 1, 0)            # hermit root: writes NOTHING
            self._fake_proc(root, 101, 100, 5_000_000)  # guest child: the writer
            self._fake_proc(root, 102, 101, 3_000_000)  # grandchild, also counted
            kids = reaper._children_map(root)
            # root sees the whole tree: 0 + 5M + 3M
            self.assertEqual(reaper.tree_write_bytes(100, kids, root), 8_000_000)
            # and a subtree sees only itself and below: 5M + 3M
            self.assertEqual(reaper.tree_write_bytes(101, kids, root), 8_000_000)
            self.assertEqual(reaper.tree_write_bytes(102, kids, root), 3_000_000)

    def test_root_only_read_would_have_seen_zero(self) -> None:
        """Pins the trap itself, so a later refactor cannot quietly reintroduce it."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self._fake_proc(root, 100, 1, 0)
            self._fake_proc(root, 101, 100, 5_000_000)
            no_kids: dict[int, list[int]] = {}
            self.assertEqual(reaper.tree_write_bytes(100, no_kids, root), 0)
            kids = reaper._children_map(root)
            self.assertEqual(reaper.tree_write_bytes(100, kids, root), 5_000_000)

    def test_an_unreadable_child_contributes_zero_rather_than_failing(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self._fake_proc(root, 100, 1, 7)
            (root / "101").mkdir()  # no io file at all
            kids = {100: [101]}
            self.assertEqual(reaper.tree_write_bytes(100, kids, root), 7)

    def test_rate_is_a_delta_over_the_window_not_a_total(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self._fake_proc(root, 100, 1, 1_000_000)
            state = {"advanced": False}

            def advance(_seconds: float) -> None:
                self._fake_proc(root, 100, 1, 5_000_000)
                state["advanced"] = True

            rates = reaper.sample_write_rates(
                [100], interval_seconds=2.0, proc_root=root, sleep=advance
            )
            self.assertTrue(state["advanced"])
            self.assertEqual(rates[100], 2_000_000.0)

    def test_a_tree_that_stopped_writing_rates_zero(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self._fake_proc(root, 100, 1, 9_999_999)
            rates = reaper.sample_write_rates(
                [100], interval_seconds=2.0, proc_root=root, sleep=lambda _s: None
            )
            self.assertEqual(rates[100], 0.0)


class DetectionModeTests(unittest.TestCase):
    """--report-orphans: ORPHANHOOD is the signal, with no busy-ness threshold.

    The three orphans that contaminated measurements on 2026-08-19 sat at core
    ratios of 0.003 to 0.14. One was writing roughly FOUR BYTES A SECOND to a
    log. No busy threshold reports that: the 1 MiB/s disk gate misses it by six
    orders of magnitude, and the 0.80 CPU gate by two. Asking "is this orphaned
    and old" needs no threshold and catches all three.
    """

    def judge(self, item, *, detection: bool, rate=None):
        bounds = reaper.Bounds()
        if detection:
            bounds = __import__("dataclasses").replace(
                bounds, min_cpu_seconds=0.0, min_core_ratio=0.0
            )
        return reaper.decide(
            item,
            caller_uid=UID,
            workspace=WORKSPACE,
            bounds=bounds,
            write_bytes_per_second=rate,
        )

    def test_the_orphan_that_no_busy_threshold_would_report(self) -> None:
        """3h50m old, ratio 0.003, writing ~4 bytes/s. Reported; not killable."""
        item = snapshot(cpu_seconds=41.6, age_seconds=13808.0)
        four_bytes_per_second = 4.0

        self.assertTrue(
            self.judge(item, detection=True, rate=four_bytes_per_second).eligible,
            "detection must report it",
        )
        self.assertFalse(
            self.judge(item, detection=False, rate=four_bytes_per_second).eligible,
            "the KILL path must still refuse it -- 4 B/s is not proof of damage",
        )

    def test_detection_reports_a_completely_idle_orphan(self) -> None:
        """Zero CPU, zero writes. Still wrong, still reported."""
        item = snapshot(cpu_seconds=0.0, age_seconds=13808.0)
        self.assertTrue(self.judge(item, detection=True, rate=0.0).eligible)
        self.assertFalse(self.judge(item, detection=False, rate=0.0).eligible)

    def test_detection_does_NOT_relax_the_safety_gates(self) -> None:
        """Only the two CPU bounds are zeroed. Everything identity-shaped holds.

        If detection widened these it would report other agents' live runs, and
        a report that names innocent processes gets ignored, which is how a
        detector becomes noise.
        """
        cases = {
            "live-parent": snapshot(ppid=42, cpu_seconds=0.0),
            "other-user": snapshot(uid=UID + 1, cpu_seconds=0.0),
            "other-cgroup": snapshot(cgroup="/user.slice/untrusted.scope", cpu_seconds=0.0),
            "outside-workspace": snapshot(exe=Path("/tmp/hermit"), cpu_seconds=0.0),
            "not-a-run": snapshot(argv=("hermit", "coordinator"), cpu_seconds=0.0),
            "zombie": snapshot(state="Z", cpu_seconds=0.0),
            "too-young": snapshot(age_seconds=10.0, cpu_seconds=0.0),
        }
        for label, item in cases.items():
            with self.subTest(label=label):
                self.assertFalse(self.judge(item, detection=True, rate=0.0).eligible)

    def test_detection_summary_names_the_pid_and_how_to_reap_it(self) -> None:
        decision = self.judge(
            snapshot(cpu_seconds=41.6, age_seconds=13808.0), detection=True, rate=4.0
        )
        text = reaper.summary_line(
            [decision],
            scanned=1,
            disk_sampled=1,
            disk_criterion="on",
            scan_errors=0,
            detection=True,
        )
        self.assertIn("pid 1234", text)
        self.assertIn("orphaned 230m", text)
        self.assertIn("--apply", text)          # the operator's next step
        self.assertIn(reaper.CONFIRMATION, text)
        self.assertNotIn("\n", text)


class UnreadableProcessTests(unittest.TestCase):
    """A failed read is a THIRD OUTCOME, not a silence.

    Found live on 2026-08-20: pid 3089765, a hermit run orphaned 6h51m, appeared
    in NEITHER the candidate nor the refused list because its exe read
    "... (deleted)". The scan said `scanned=183 candidates=1` and dropped it
    without a word. A report-only tool's safety argument rests on the list being
    COMPLETE, so it must be able to say when it is not.

    The counter-defect is just as bad: a first cut reported 3403 unreadable
    processes, mostly root-owned kernel threads, and shouted "INCOMPLETE" every
    tick. These pin BOTH edges.
    """

    def test_a_deleted_exe_outside_the_workspace_is_out_of_scope(self) -> None:
        """The narrowing that took 53 warnings down to 5. Excludes only."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "42").mkdir()
            (root / "42" / "exe").symlink_to("/usr/lib/claude/claude")
            self.assertFalse(reaper._maybe_in_workspace(root, 42, WORKSPACE))

    def test_a_deleted_exe_INSIDE_the_workspace_stays_in_scope(self) -> None:
        """pid 3089765's shape: readlink still yields the path when unlinked."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "43").mkdir()
            target = WORKSPACE / "hermit/ignored/pdeathsig/target/debug/hermit"
            (root / "43" / "exe").symlink_to(target)
            self.assertTrue(reaper._maybe_in_workspace(root, 43, WORKSPACE))

    def test_unknown_placement_counts_as_in_scope(self) -> None:
        """Unknown must never silently drop one of ours."""
        with tempfile.TemporaryDirectory() as temp:
            self.assertTrue(reaper._maybe_in_workspace(Path(temp), 99999, WORKSPACE))

    def test_unreadable_uid_is_unknown_not_someone_elses(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            self.assertIsNone(reaper._readable_uid(Path(temp), 99999))

    def test_the_summary_says_when_the_list_is_incomplete(self) -> None:
        text = reaper.summary_line(
            [], scanned=183, disk_sampled=0, disk_criterion="on", scan_errors=1
        )
        self.assertIn("unreadable=1", text)
        self.assertIn("INCOMPLETE", text)

    def test_incompleteness_is_stated_even_when_orphans_WERE_found(self) -> None:
        """'1 orphan' and '1 orphan, 1 unreadable' are different statements."""
        decision = reaper.decide(
            snapshot(), caller_uid=UID, workspace=WORKSPACE, bounds=reaper.Bounds()
        )
        text = reaper.summary_line(
            [decision], scanned=9, disk_sampled=1, disk_criterion="on",
            scan_errors=2, detection=True,
        )
        self.assertIn("INCOMPLETE", text)
        self.assertIn("2 live process(es) UNREADABLE", text)

    def test_the_reap_instruction_it_prints_actually_works(self) -> None:
        """Following my own guidance is how I found it was wrong.

        The detection summary used to say 'reap with --pid <pid> --apply', which
        the kill path REFUSES for exactly the orphans detection finds -- ratio
        0.000, no writes, no proof of damage. It must name the relaxation.
        """
        decision = reaper.decide(
            snapshot(cpu_seconds=4.6, age_seconds=17351.0),
            caller_uid=UID, workspace=WORKSPACE, bounds=reaper.Bounds(),
            write_bytes_per_second=0.0,
        )
        text = reaper.summary_line(
            [decision], scanned=1, disk_sampled=1, disk_criterion="on",
            scan_errors=0, detection=True,
        )
        for flag in ("--min-cpu-seconds 0", "--min-core-ratio 0",
                     "--min-write-mib-per-second 0", "--apply", reaper.CONFIRMATION):
            self.assertIn(flag, text)


class SummaryRenderingTests(unittest.TestCase):
    """{summary} must ALWAYS resolve, or the gate silently never emits.

    Two gates were silenced exactly this way on 2026-08-19. These tests use the
    real tick-hub parser semantics: split each line on its FIRST '='.
    """

    @staticmethod
    def parse_kv_lines(text: str) -> dict[str, str]:
        """Mirror of tick_hub.engine.parse_kv_lines."""
        out: dict[str, str] = {}
        for line in text.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                key, _, value = line.partition("=")
                if key.strip():
                    out[key.strip()] = value.strip()
        return out

    def test_zero_candidates_still_produces_a_summary(self) -> None:
        """The path most likely to be forgotten, and the one that runs every tick."""
        text = reaper.summary_line(
            [], scanned=900, disk_sampled=0, disk_criterion="on", scan_errors=0
        )
        self.assertTrue(text)
        self.assertIn("no eligible orphans", text)
        self.assertNotIn("\n", text)

    def test_summary_names_the_pid_and_the_qualifying_evidence(self) -> None:
        decision = reaper.decide(
            snapshot(cpu_seconds=41.6, age_seconds=13808.0),
            caller_uid=UID,
            workspace=WORKSPACE,
            bounds=reaper.Bounds(),
            write_bytes_per_second=5.0 * reaper.MIB,
        )
        text = reaper.summary_line(
            [decision], scanned=1, disk_sampled=1, disk_criterion="on", scan_errors=0
        )
        self.assertIn("pid 1234", text)
        self.assertIn("5.00MiB/s", text)   # WHY it qualified
        self.assertIn("ratio 0.003", text)  # and that CPU did not
        self.assertNotIn("\n", text)

    def test_the_summary_survives_the_tick_hub_parser(self) -> None:
        """The header line alone would NOT yield a summary key -- pin that."""
        header = "orphaned-hermit-reaper: mode=dry-run candidates=2 scanned=9"
        self.assertNotIn("summary", self.parse_kv_lines(header))

        body = reaper.summary_line(
            [], scanned=9, disk_sampled=0, disk_criterion="on", scan_errors=0
        )
        stdout = f"summary={body}\n{header}\nCANDIDATE pid=1 kind=hermit-run\n"
        parsed = self.parse_kv_lines(stdout)
        self.assertIn("summary", parsed)
        self.assertEqual(parsed["summary"], body)
        self.assertTrue(parsed["summary"], "an empty summary renders as a blank title")


class ProcParsingTests(unittest.TestCase):
    def test_stat_parser_handles_spaces_and_parentheses(self) -> None:
        # field 2 deliberately contains both spaces and a right parenthesis.
        fields = ["R", "1"] + ["0"] * 9 + ["800", "100"] + ["0"] * 6 + ["10000"]
        state, ppid, utime, stime, start = reaper._parse_stat(
            "1234 (weird ) test) " + " ".join(fields)
        )
        self.assertEqual((state, ppid, utime, stime, start), ("R", 1, 800, 100, 10000))

    def test_reader_derives_cpu_age_and_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "uptime").write_text("20.00 0.00\n")
            base = root / "1234"
            base.mkdir()
            fields = ["R", "1"] + ["0"] * 9 + ["800", "100"] + ["0"] * 6 + ["1000"]
            (base / "stat").write_text("1234 (hermit) " + " ".join(fields))
            (base / "status").write_text(f"Name:\thermit\nUid:\t{UID}\t{UID}\t{UID}\t{UID}\n")
            (base / "cgroup").write_text(f"0::{CGROUP}\n")
            (base / "cmdline").write_bytes(b"hermit\0run\0--\0/bin/true\0")
            (base / "exe").symlink_to(WORKSPACE / "hermit/target/release/hermit")
            item = reaper.ProcReader(root, clock_ticks=100).read(1234)
            self.assertEqual(item.cpu_seconds, 9.0)
            self.assertEqual(item.age_seconds, 10.0)
            self.assertEqual(item.argv[1], "run")


class SignallingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.initial = snapshot()
        self.candidate = reaper.decide(
            self.initial,
            caller_uid=UID,
            workspace=WORKSPACE,
            bounds=reaper.Bounds(),
        )

    def run_reap(
        self,
        reads: list[reaper.ProcessSnapshot],
        *,
        exited_after_term: bool,
    ) -> tuple[reaper.ReapResult, list[signal.Signals]]:
        class Reader:
            def read(inner_self, pid: int) -> reaper.ProcessSnapshot:
                self.assertEqual(pid, 1234)
                return reads.pop(0)

        sent: list[signal.Signals] = []
        result = reaper.reap(
            self.candidate,
            reader=Reader(),
            caller_uid=UID,
            workspace=WORKSPACE,
            bounds=reaper.Bounds(),
            grace_seconds=0,
            pidfd_open=lambda pid: 99,
            pidfd_send=lambda fd, sig: sent.append(sig),
            wait_pidfd=lambda fd, grace: exited_after_term,
        )
        return result, sent

    def test_term_then_kill_uses_pidfd_after_two_revalidations(self) -> None:
        original_close = reaper.os.close
        reaper.os.close = lambda fd: None
        try:
            result, sent = self.run_reap(
                [self.initial, self.initial], exited_after_term=False
            )
        finally:
            reaper.os.close = original_close
        self.assertEqual(sent, [signal.SIGTERM, signal.SIGKILL])
        self.assertEqual(result.outcome, "killed-after-grace")

    def test_identity_change_refuses_kill_escalation(self) -> None:
        original_close = reaper.os.close
        reaper.os.close = lambda fd: None
        try:
            result, sent = self.run_reap(
                [self.initial, snapshot(start_ticks=10_001)], exited_after_term=False
            )
        finally:
            reaper.os.close = original_close
        self.assertEqual(sent, [signal.SIGTERM])
        self.assertEqual(result.outcome, "refused-identity-changed-before-kill")

    def test_clean_term_does_not_escalate(self) -> None:
        original_close = reaper.os.close
        reaper.os.close = lambda fd: None
        try:
            result, sent = self.run_reap([self.initial], exited_after_term=True)
        finally:
            reaper.os.close = original_close
        self.assertEqual(sent, [signal.SIGTERM])
        self.assertEqual(result.outcome, "terminated")


class CommandLineSafetyTests(unittest.TestCase):
    def run_script(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_apply_requires_acknowledgement(self) -> None:
        result = self.run_script("--apply", "--pid", "1234")
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires --confirm", result.stderr)

    def test_a_refusal_still_prints_a_summary_line(self) -> None:
        """THE SILENT-GATE HOLE. tick-hub captures STDOUT ONLY.

        argparse writes its refusal to stderr and exits 2. Without the
        _SummaryParser override, stdout would be empty, {summary} would not
        resolve, and the emission would be REFUSED -- the gate silent in exactly
        the case where something is wrong with it. Two gates died this way on
        2026-08-19.
        """
        result = self.run_script("--not-a-real-flag")
        self.assertNotEqual(result.returncode, 0)
        summary = [
            line for line in result.stdout.splitlines() if line.startswith("summary=")
        ]
        self.assertTrue(summary, f"refusal produced no summary= on stdout: {result.stdout!r}")
        self.assertIn("refused:", summary[0])

    def test_a_crash_still_prints_a_summary_line(self) -> None:
        """The other route to silence: a traceback goes to stderr, which is discarded."""
        import io
        import contextlib

        original = reaper._main
        try:
            def boom(_argv=None):
                raise RuntimeError("simulated /proc explosion")

            reaper._main = boom
            buffer = io.StringIO()
            with contextlib.redirect_stdout(buffer), contextlib.redirect_stderr(io.StringIO()):
                code = reaper.main([])
        finally:
            reaper._main = original
        self.assertNotEqual(code, 0)
        self.assertIn("summary=crashed:", buffer.getvalue())
        self.assertIn("simulated /proc explosion", buffer.getvalue())

    def test_report_orphans_cannot_be_combined_with_apply(self) -> None:
        """Detection reports everything orphaned; the kill path demands proof.

        Letting --apply run against the detection set would signal exactly the
        processes the strict gates exist to protect.
        """
        result = self.run_script(
            "--report-orphans", "--apply", "--pid", "1", "--confirm", reaper.CONFIRMATION
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot be combined with --apply", result.stderr)

    def test_apply_requires_explicit_pid(self) -> None:
        result = self.run_script("--apply", "--confirm", reaper.CONFIRMATION)
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires at least one explicit --pid", result.stderr)


if __name__ == "__main__":
    unittest.main()
