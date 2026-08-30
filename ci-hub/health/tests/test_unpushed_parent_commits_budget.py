#!/usr/bin/env python3
"""Bracket the unpushed-parent-commits gate's RUNTIME and its SCOPE.

On 2026-08-08 this gate surfaced as ``ERROR: gate command could not run ...
timed out after 30s`` while a real unpushed stack existed on the box. The
reporting was CORRECT -- could-not-measure, not a clean pass -- and the tests
below exist to keep it that way while pinning down what actually costs time.

Two properties are asserted, because the incident turned on both:

1.  COST IS PER LOCAL-ONLY COMMIT, NOT PER WORKTREE. ``--scope all`` means "all
    REFS of the parent repo", not "all worktrees on the box". Detection is a
    single ``rev-list``; it is ``--rescue`` that spends two network round trips
    PER COMMIT FOUND. That gives the gate a vicious shape: it is instant when
    there is nothing to find and only becomes expensive once it finds
    something, so its timeout is anti-correlated with the risk it exists to
    cover. ``test_runtime_scales_with_commit_count`` measures that directly.

2.  SILENCE IS NOT COVERAGE. Every git call in the gate runs with ``cwd=PARENT``,
    so a commit living only in a product worktree is invisible to it no matter
    how long it is allowed to run. ``test_scope_all_does_not_see_a_nested_repo``
    pins that as a known, deliberate boundary rather than an assumed one.

The fixtures are fully inert: a local bare repo stands in for ``origin`` and a
stub replaces ``herdr-run``, so nothing here touches the network, the real
parent checkout, or any live tmux pane.
"""

from __future__ import annotations

import os
import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from unittest import mock

HEALTH = Path(__file__).resolve().parents[1]
REPO_ROOT = HEALTH.parents[1]
GATE_SOURCE = HEALTH / "unpushed_parent_commits.py"
SPEC = importlib.util.spec_from_file_location("unpushed_parent_commits_unit", GATE_SOURCE)
assert SPEC is not None and SPEC.loader is not None
GATE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = GATE
SPEC.loader.exec_module(GATE)


class ReadFailureBrackets(unittest.TestCase):
    def test_failed_rev_list_is_unknown_not_zero_commits(self) -> None:
        failed = subprocess.CompletedProcess(["git"], 128, "", "bad revision")
        with mock.patch.object(GATE.bounded_capture, "git", return_value=failed):
            with self.assertRaisesRegex(GATE.ScanUnavailable, "rev-list failed"):
                GATE.local_only("all")

    def test_malformed_log_row_is_unknown_not_a_commit(self) -> None:
        census = subprocess.CompletedProcess(["git"], 0, "a" * 40 + "\n", "")
        malformed = subprocess.CompletedProcess(["git"], 0, "not-tab-separated\n", "")
        with mock.patch.object(
            GATE.bounded_capture, "git", side_effect=[census, malformed]
        ):
            with self.assertRaisesRegex(GATE.ScanUnavailable, "unparseable row"):
                GATE.local_only("all")


def run(*args: str, cwd: Path) -> str:
    out = subprocess.run(args, cwd=str(cwd), capture_output=True, text=True)
    return out.stdout.strip()


class Fixture:
    """An inert parent-shaped tree: real git, local ``origin``, stub herdr-run.

    The gate derives PARENT from ``__file__.resolve().parents[2]``, so placing a
    copy at ``<root>/ci-hub/health/`` is what re-points it at the fixture. That
    is the whole isolation mechanism -- no env var and no flag can do it.
    """

    def __init__(self, tmp: Path, herdr_delay: float = 0.0) -> None:
        self.root = tmp / "parent"
        self.origin = tmp / "origin.git"
        (self.root / "ci-hub" / "health").mkdir(parents=True)
        (self.root / "ci-hub" / "lib").mkdir(parents=True)
        (self.root / "agent-utils" / "bin").mkdir(parents=True)
        shutil.copy(GATE_SOURCE, self.root / "ci-hub" / "health" / GATE_SOURCE.name)
        # The gate imports ci-hub/lib/bounded_capture, so the fixture tree has to
        # carry it too. Without this the copy raises ModuleNotFoundError and every
        # case here fails on import rather than on the behaviour it is testing.
        shutil.copy(
            REPO_ROOT / "ci-hub" / "lib" / "bounded_capture.py",
            self.root / "ci-hub" / "lib" / "bounded_capture.py",
        )
        self.gate = self.root / "ci-hub" / "health" / GATE_SOURCE.name
        self._write_herdr_stub(herdr_delay)

        subprocess.run(["git", "init", "-q", "--bare", str(self.origin)], check=True)
        subprocess.run(["git", "init", "-q", "-b", "main", str(self.root)], check=True)
        for key, value in (("user.email", "t@example.invalid"), ("user.name", "t")):
            run("git", "config", key, value, cwd=self.root)
        run("git", "remote", "add", "origin", str(self.origin), cwd=self.root)
        self.commit("seed")
        run("git", "push", "-q", "origin", "main", cwd=self.root)
        run("git", "fetch", "-q", "origin", cwd=self.root)

    def _write_herdr_stub(self, delay: float) -> None:
        """Stand in for agent-utils/bin/herdr-run.

        The real one injects into a tmux pane and reaches the network. The stub
        strips ``--agent NAME`` and the ``with-proxy`` prefix, then runs the rest
        locally, optionally after `delay` seconds so a network cost can be
        simulated without one.
        """
        stub = self.root / "agent-utils" / "bin" / "herdr-run"
        stub.write_text(textwrap.dedent(f"""\
            #!/usr/bin/env python3
            import subprocess, sys, time
            argv = sys.argv[1:]
            if argv[:1] == ["--agent"]:
                argv = argv[2:]
            time.sleep({delay!r})
            line = argv[0]
            if line.startswith("with-proxy "):
                line = line[len("with-proxy "):]
            sys.exit(subprocess.run(["bash", "-c", line]).returncode)
            """))
        stub.chmod(0o755)

    def commit(self, subject: str) -> str:
        path = self.root / "f.txt"
        path.write_text(f"{subject}\n")
        run("git", "add", "--", "f.txt", cwd=self.root)
        run("git", "commit", "-q", "-m", subject, cwd=self.root)
        return run("git", "rev-parse", "HEAD", cwd=self.root)

    def gate_run(self, *args: str) -> tuple[int, str, float]:
        started = time.monotonic()
        proc = subprocess.run(
            [sys.executable, str(self.gate), *args],
            capture_output=True, text=True,
        )
        return proc.returncode, proc.stdout + proc.stderr, time.monotonic() - started


class UnpushedParentCommitsBudget(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="unpushed-gate-")
        self.addCleanup(self._tmp.cleanup)
        self.tmp = Path(self._tmp.name)

    # ------------------------------------------------------------ positive --
    def test_detects_and_publishes_a_planted_unpushed_commit(self) -> None:
        """POSITIVE BRACKET: it finds a planted commit, publishes it, and the
        publication is confirmed by re-reading the remote -- not by a push exit
        code."""
        fx = Fixture(self.tmp)
        sha = fx.commit("planted local-only commit")

        rc, out, elapsed = fx.gate_run("--scope", "all", "--rescue")

        self.assertEqual(rc, 1, f"nonzero rc signals findings; got {rc}\n{out}")
        self.assertIn("count=1", out, out)
        self.assertIn(sha[:7], out, out)
        self.assertIn("[verified]", out,
                      f"publication must be verified at the remote:\n{out}")
        # The rescue ref really exists in the inert origin: publication is a
        # fact about the remote, not a claim in the log line.
        refs = run("git", "for-each-ref", "--format=%(refname)", cwd=fx.origin)
        self.assertIn(f"refs/heads/rescue/auto-{sha[:7]}", refs, refs)
        self.assertLess(elapsed, 30.0, f"must fit the 30s gate budget; took {elapsed:.2f}s")

    def test_clean_tree_reports_zero_and_is_cheap(self) -> None:
        """CONTROL: with nothing to find the gate passes, and it is fast for the
        very reason that makes it slow later -- the rescue loop never runs."""
        fx = Fixture(self.tmp, herdr_delay=1.0)
        rc, out, elapsed = fx.gate_run("--scope", "all", "--rescue")
        self.assertEqual(rc, 0, out)
        self.assertIn("count=0", out, out)
        self.assertLess(elapsed, 30.0, f"took {elapsed:.2f}s")

    # ------------------------------------------------------- cost structure --
    def test_runtime_scales_with_commit_count_not_worktree_count(self) -> None:
        """ROOT CAUSE: runtime grows with the number of local-only commits,
        because --rescue spends two herdr round trips on each one.

        Same fixture, same single repo, no worktrees anywhere: only the commit
        count changes, and the runtime tracks it. That is what makes a 30s
        budget unsurvivable once a backlog exists.
        """
        delay = 0.25
        timings = {}
        for count in (1, 3):
            with tempfile.TemporaryDirectory(prefix="unpushed-scale-") as raw:
                fx = Fixture(Path(raw), herdr_delay=delay)
                for i in range(count):
                    fx.commit(f"local-only {i}")
                rc, out, elapsed = fx.gate_run("--scope", "all", "--rescue")
                self.assertEqual(rc, 1, out)
                self.assertIn(f"count={count}", out, out)
                timings[count] = elapsed

        # Two herdr calls per commit (push + verify), so each extra commit adds
        # ~2*delay. Assert the SLOPE, which is the structural claim; absolute
        # times carry process-start noise and a loaded box.
        added = timings[3] - timings[1]
        self.assertGreater(
            added, 2 * delay,
            f"expected >=2 extra commits x 2 calls x {delay}s; "
            f"1->{timings[1]:.2f}s 3->{timings[3]:.2f}s")

    # -------------------------------------------------------------- scope ----
    def test_scope_all_does_not_see_a_nested_repo(self) -> None:
        """COVERAGE BOUNDARY, asserted rather than assumed.

        `--scope all` is all REFS of the parent repo. A separate repository
        nested inside the tree -- which is exactly what a product worktree is --
        is invisible, and no timeout increase would ever change that. Pinning
        it here means widening the scope later has to break this test on
        purpose.
        """
        fx = Fixture(self.tmp)
        nested = fx.root / "product"
        nested.mkdir()
        subprocess.run(["git", "init", "-q", "-b", "main", str(nested)], check=True)
        for key, value in (("user.email", "t@example.invalid"), ("user.name", "t")):
            run("git", "config", key, value, cwd=nested)
        (nested / "p.txt").write_text("hidden\n")
        run("git", "add", "--", "p.txt", cwd=nested)
        run("git", "commit", "-q", "-m", "unpushed inside a nested repo", cwd=nested)
        hidden = run("git", "rev-parse", "HEAD", cwd=nested)

        rc, out, _ = fx.gate_run("--scope", "all")

        self.assertEqual(rc, 0, f"parent itself is clean, so the gate passes:\n{out}")
        self.assertIn("count=0", out, out)
        self.assertNotIn(hidden[:7], out,
                         "nested-repo commit must not appear; it is out of scope")

    # ------------------------------------------------- negative / fail-closed --
    def test_overrunning_gate_reports_could_not_measure_not_success(self) -> None:
        """NEGATIVE BRACKET, against the REAL runner.

        This is the property the incident got right and that must not be traded
        away for a faster gate: a command that cannot finish inside its budget
        must surface as could-not-measure, never as a pass.
        """
        sys.path.insert(0, str(REPO_ROOT / "agent-utils" / "py"))
        self.addCleanup(lambda: sys.path.remove(str(REPO_ROOT / "agent-utils" / "py")))
        try:
            from tick_hub.probes import SubprocessGateRunner
        except ImportError as exc:  # pragma: no cover - agent-utils absent
            self.skipTest(f"tick_hub not importable: {exc}")

        result = SubprocessGateRunner(timeout=1).run("sleep 30")

        self.assertFalse(result.ok, "an overrun must NOT be reported as ok")
        self.assertIn("timed out", (result.error or "").lower(), result.error)
        # A pass is rc 0 and a finding is rc 1; neither may be forged here.
        self.assertNotEqual(result.returncode, 0,
                            "an overrun must never present as a clean pass")

    def test_completing_gate_is_reported_as_ran(self) -> None:
        """ADMIT CONTROL for the runner: without this, a runner that reported
        could-not-measure unconditionally would satisfy the test above."""
        sys.path.insert(0, str(REPO_ROOT / "agent-utils" / "py"))
        self.addCleanup(lambda: sys.path.remove(str(REPO_ROOT / "agent-utils" / "py")))
        try:
            from tick_hub.probes import SubprocessGateRunner
        except ImportError as exc:  # pragma: no cover - agent-utils absent
            self.skipTest(f"tick_hub not importable: {exc}")

        result = SubprocessGateRunner(timeout=30).run("echo measured")

        self.assertTrue(result.ok, f"a completing gate must report ok: {result}")
        self.assertEqual(result.returncode, 0, result)
        self.assertIn("measured", result.stdout)




class ObservationSurvivesRescue(unittest.TestCase):
    """The 2026-08-08 fix: the measurement is emitted BEFORE rescue is attempted.

    The gate had never emitted a measurement in a full day of ticks, and the
    reason was not that it could not measure. `main` computed the correct answer
    in 0.4s, then called `rescue`, and printed only afterwards -- so the 30s
    timeout killed a process that was already holding a complete result. These
    tests pin the ordering and the bounded-rescue reporting so the measurement
    can never again be discarded by a slow or broken publish leg.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="unpushed-order-")
        self.addCleanup(self._tmp.cleanup)
        self.tmp = Path(self._tmp.name)

    def test_measurement_survives_a_rescue_that_never_finishes(self) -> None:
        """POSITIVE: kill the gate mid-rescue; the count is already on stdout.

        This reproduces the tick timeout exactly -- an external killer stops the
        process while rescue is still running -- and asserts the observation is
        not lost with it.
        """
        fx = Fixture(self.tmp, herdr_delay=30.0)
        fx.commit("local-only-and-unpublished")

        proc = subprocess.Popen(
            [sys.executable, str(fx.gate), "--scope", "all", "--rescue"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        )
        try:
            proc.wait(timeout=4)
        except subprocess.TimeoutExpired:
            # Our own child, by exact pid, and reaped so it cannot linger on a
            # shared box (Hard Invariant 15).
            proc.kill()
            proc.wait(timeout=10)
        out = proc.stdout.read()
        proc.stdout.close()

        self.assertIn("unpushed-parent-commits scope=all count=1", out)
        self.assertIn("local-only-and-unpublished", out)
        self.assertIn("summary=", out)
        # And the publish leg genuinely had not finished when we killed it.
        self.assertNotIn("rescue attempted=", out)

    def test_deadline_reports_skipped_never_verified(self) -> None:
        """NEGATIVE: commits the budget never reached are named, not implied done."""
        fx = Fixture(self.tmp, herdr_delay=1.0)
        for n in range(3):
            fx.commit(f"unpublished-{n}")

        rc, out, _ = fx.gate_run(
            "--scope", "all", "--rescue", "--rescue-deadline", "0.2"
        )
        self.assertIn("skipped-deadline", out)
        skipped = out.count("skipped-deadline")
        self.assertGreaterEqual(skipped, 1)
        # Partial coverage is not success: nothing unpublished may exit 0.
        self.assertEqual(1, rc)
        # The count line states what was skipped rather than burying it.
        self.assertRegex(out, r"skipped_deadline=[1-9]")

    def test_summary_key_is_emitted_so_the_alarm_names_its_subject(self) -> None:
        """tick-hub resolves `{summary}` from key=value stdout lines.

        Without a `summary=` line the emitted warning renders the LITERAL
        `{summary}` -- the unactionable-alarm defect the worktree-liveness gate
        shipped. It was invisible here only because the gate never completed.
        """
        fx = Fixture(self.tmp)
        fx.commit("names-its-subject")
        _, out, _ = fx.gate_run("--scope", "all")

        summary = [l for l in out.splitlines() if l.startswith("summary=")]
        self.assertEqual(1, len(summary), out)
        self.assertIn("names-its-subject", summary[0])
        self.assertNotIn("{summary}", out)

    def test_clean_tree_still_emits_a_summary(self) -> None:
        """A gate that only names its subject when firing leaves `{summary}`
        literal on the quiet path. Both branches must emit the key."""
        fx = Fixture(self.tmp)
        rc, out, _ = fx.gate_run("--scope", "all")
        self.assertEqual(0, rc)
        self.assertIn("summary=no local-only commits", out)


class TickGateKeepsObservationSeparateFromRescue(unittest.TestCase):
    """The split is the fix; pin it so it cannot silently regress.

    Measured 2026-08-08: scan 0.40s versus one herdr-run rescue round-trip at
    65.8s and 74.8s (both failing), two per commit. Putting `--rescue` back on a
    30s tick budget restores a gate that can never report.
    """

    def test_tick_gate_runs_observation_only(self) -> None:
        config = (REPO_ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text()
        block = config.split("- name: unpushed_parent_commits", 1)
        self.assertEqual(2, len(block), "gate missing from tick-hub.yaml")
        cmd = [
            line for line in block[1].splitlines()
            if "unpushed_parent_commits.py" in line
        ]
        self.assertEqual(1, len(cmd), block[1][:400])
        self.assertIn("--scope all", cmd[0])
        self.assertNotIn("--rescue", cmd[0])


class PublicationContentClassification(unittest.TestCase):
    """A local-only OBJECT is not the same fact as unpublished WORK.

    The gate's first successful run flagged `ef7cd9b`, a pre-rebase object whose
    content was already on main as `f37bd1c8`, and it was reported upward as a
    real catch before anyone checked. Rebase-then-force-update produces one of
    those on every routine landing, so without this split the gate pages
    constantly, gets ignored, and is not believed on the day it is right.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def routine_landing(self, fx) -> str:
        """The normal shape: branch, rebase past an advanced main, land, and keep
        a local ref at the PRE-rebase object so it is still reachable locally."""
        run("git", "checkout", "-q", "-b", "feat", cwd=fx.root)
        (fx.root / "feature.txt").write_text("a feature\n")
        run("git", "add", "feature.txt", cwd=fx.root)
        run("git", "commit", "-q", "-m", "add feature", cwd=fx.root)
        orphan = run("git", "rev-parse", "HEAD", cwd=fx.root).strip()
        run("git", "branch", "-q", "keep-prerebase", cwd=fx.root)
        run("git", "checkout", "-q", "main", cwd=fx.root)
        (fx.root / "unrelated.txt").write_text("unrelated\n")
        run("git", "add", "unrelated.txt", cwd=fx.root)
        run("git", "commit", "-q", "-m", "main advances", cwd=fx.root)
        run("git", "push", "-q", "origin", "main", cwd=fx.root)
        run("git", "checkout", "-q", "feat", cwd=fx.root)
        run("git", "rebase", "-q", "main", cwd=fx.root)
        run("git", "checkout", "-q", "main", cwd=fx.root)
        run("git", "merge", "-q", "--ff-only", "feat", cwd=fx.root)
        run("git", "push", "-q", "origin", "main", cwd=fx.root)
        run("git", "fetch", "-q", "origin", cwd=fx.root)
        return orphan

    def plant_unpublished(self, fx, name: str, text: str) -> str:
        (fx.root / name).write_text(text)
        run("git", "add", name, cwd=fx.root)
        run("git", "commit", "-q", "-m", f"unpublished: {name}", cwd=fx.root)
        return run("git", "rev-parse", "HEAD", cwd=fx.root).strip()

    @staticmethod
    def ledger_rows(prefix: str, count: int) -> list[str]:
        return [
            json.dumps(
                {"event_id": f"{prefix}-{index:04d}", "result": "pass"},
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n"
            for index in range(count)
        ]

    def plant_local_ledger_commit(
        self,
        fx: Fixture,
        *,
        head_rows: list[str],
        target_rows: list[str],
        base_rows: list[str] | None = None,
        truncate_target: bool = False,
    ) -> str:
        path = fx.root / "ledger/hermit/devbig014/2026-08.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        if base_rows is not None:
            path.write_text("".join(base_rows))
            run("git", "add", str(path.relative_to(fx.root)), cwd=fx.root)
            run("git", "commit", "-q", "-m", "ledger baseline", cwd=fx.root)
            run("git", "push", "-q", "origin", "main", cwd=fx.root)
            run("git", "fetch", "-q", "origin", cwd=fx.root)

        run("git", "checkout", "-q", "-b", "local-ledger", cwd=fx.root)
        path.write_text("".join(head_rows))
        run("git", "add", str(path.relative_to(fx.root)), cwd=fx.root)
        run("git", "commit", "-q", "-m", "local ledger rows", cwd=fx.root)
        local = run("git", "rev-parse", "HEAD", cwd=fx.root).strip()

        run("git", "checkout", "-q", "main", cwd=fx.root)
        path.parent.mkdir(parents=True, exist_ok=True)
        target_text = "".join(target_rows)
        if truncate_target and target_text.endswith("\n"):
            target_text = target_text[:-1]
        path.write_text(target_text)
        run("git", "add", str(path.relative_to(fx.root)), cwd=fx.root)
        run("git", "commit", "-q", "-m", "squashed ledger rows", cwd=fx.root)
        run("git", "push", "-q", "origin", "main", cwd=fx.root)
        run("git", "fetch", "-q", "origin", cwd=fx.root)
        self.assertTrue(
            run("git", "cherry", "origin/main", local, cwd=fx.root).startswith("+"),
            "fixture must bypass patch-id so raw-row containment decides",
        )
        return local

    def test_negative_a_routine_landing_does_not_page(self) -> None:
        fx = Fixture(self.tmp)
        orphan = self.routine_landing(fx)
        # It really is local-only: that is precisely why the old gate paged.
        self.assertEqual(
            "", run("git", "branch", "-r", "--contains", orphan, cwd=fx.root).strip(),
            "fixture must reproduce a genuinely local-only object")
        rc, out, _ = fx.gate_run("--scope", "all", "--classify")
        self.assertEqual(rc, 0, f"a routine landing must not page:\n{out}")
        self.assertIn("unpublished=0", out, out)
        self.assertIn("superseded=1", out, out)
        # Reported, not hidden: it is prune-able and the reader should know.
        self.assertIn("[superseded, content published]", out, out)
        self.assertIn("no unpublished work", out, out)

    def test_positive_content_absent_from_the_remote_still_pages(self) -> None:
        """THE CASE THAT JUSTIFIES THE GATE.

        Proved with content that exists NOWHERE on the remote -- the blob is
        absent from origin's OBJECT STORE -- not merely with a local-only ref.
        Those are different tests and only this one proves the property; a
        local-only ref can still point at fully published content, which is
        exactly the false positive being fixed.
        """
        fx = Fixture(self.tmp)
        sha = self.plant_unpublished(fx, "precious.md", "IRREPLACEABLE ANALYSIS\n")
        blob = run("git", "rev-parse", f"{sha}:precious.md", cwd=fx.root).strip()
        probe = subprocess.run(["git", "cat-file", "-e", blob], cwd=fx.origin,
                               capture_output=True)
        self.assertNotEqual(0, probe.returncode,
                            "the blob must be ABSENT from origin's object store")
        rc, out, _ = fx.gate_run("--scope", "all", "--classify")
        self.assertEqual(rc, 1, f"unpublished work must page:\n{out}")
        self.assertIn("unpublished=1", out, out)
        self.assertIn(sha[:7], out, out)
        self.assertIn("hold UNPUBLISHED work", out, out)

    def test_mixed_reports_only_the_genuinely_unpublished_one(self) -> None:
        fx = Fixture(self.tmp)
        orphan = self.routine_landing(fx)
        sha = self.plant_unpublished(fx, "precious.md", "IRREPLACEABLE\n")
        rc, out, _ = fx.gate_run("--scope", "all", "--classify")
        self.assertEqual(rc, 1, out)
        self.assertIn("count=2", out, out)
        self.assertIn("unpublished=1", out, out)
        self.assertIn("superseded=1", out, out)
        # The alarm names the real one and NOT the twin.
        summary = [ln for ln in out.splitlines() if ln.startswith("summary=")][0]
        self.assertIn(sha[:7], summary, summary)
        self.assertNotIn(orphan[:7], summary, summary)

    def test_patch_id_still_recognises_a_landing_that_main_has_since_moved_past(self) -> None:
        """The case blob identity ALONE cannot decide, and the reason
        `git cherry` is in the classifier rather than a redundant second look.

        After a change lands, main keeps moving. Once someone edits the same
        file again, the pre-rebase object's blob no longer matches main even
        though its patch did land -- blob identity then says `unpublished` and
        the false positive is back. Patch-id equivalence survives that.
        """
        fx = Fixture(self.tmp)
        orphan = self.routine_landing(fx)
        # Main moves past the landing, touching the SAME file.
        (fx.root / "feature.txt").write_text("a feature, since revised\n")
        run("git", "add", "feature.txt", cwd=fx.root)
        run("git", "commit", "-q", "-m", "revise the feature", cwd=fx.root)
        run("git", "push", "-q", "origin", "main", cwd=fx.root)
        run("git", "fetch", "-q", "origin", cwd=fx.root)
        # Blob identity is now FALSE for the orphan's only path ...
        self.assertNotEqual(
            run("git", "rev-parse", f"{orphan}:feature.txt", cwd=fx.root).strip(),
            run("git", "rev-parse", "origin/main:feature.txt", cwd=fx.root).strip(),
            "fixture must make the blob test fail, or it proves nothing")
        # ... and the gate must STILL not page, on patch-id evidence alone.
        rc, out, _ = fx.gate_run("--scope", "all", "--classify")
        self.assertEqual(rc, 0, f"patch-id must still recognise this landing:\n{out}")
        self.assertIn("superseded=1", out, out)
        self.assertIn("git cherry", out, out)

    def test_ledger_rows_are_contained_when_main_has_appended_past_them(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 735)
        contributed = self.ledger_rows("contributed", 42)
        head = [*base, *contributed]
        target = [*head, *self.ledger_rows("later-main", 25)]
        self.plant_local_ledger_commit(
            fx, base_rows=base, head_rows=head, target_rows=target
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(0, rc, out)
        self.assertIn("unpublished=0", out, out)
        self.assertIn("superseded=1", out, out)
        self.assertIn("42/42 contributed raw ledger row(s)", out, out)
        self.assertIn("777/777 head raw ledger row(s)", out, out)
        self.assertIn("among 802 target row(s)", out, out)

    def test_ledger_row_missing_from_target_refuses_containment(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 735)
        contributed = self.ledger_rows("contributed", 42)
        head = [*base, *contributed]
        target = [*base, *contributed[:-1], *self.ledger_rows("later-main", 25)]
        local = self.plant_local_ledger_commit(
            fx, base_rows=base, head_rows=head, target_rows=target
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(1, rc, out)
        self.assertIn("unpublished=1", out, out)
        self.assertIn(local[:7], out, out)

    def test_rewritten_ledger_row_refuses_containment(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 735)
        contributed = self.ledger_rows("contributed", 42)
        head = [*base, *contributed]
        target = list(head)
        target[735 + 17] = target[735 + 17].replace(
            '"result":"pass"', '"result":"fail"'
        )
        target += self.ledger_rows("later-main", 25)
        self.plant_local_ledger_commit(
            fx, base_rows=base, head_rows=head, target_rows=target
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(1, rc, out)
        self.assertIn("unpublished=1", out, out)

    def test_duplicate_ledger_row_count_mismatch_refuses_containment(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 735)
        duplicate = self.ledger_rows("duplicate", 1)[0]
        unique = self.ledger_rows("unique", 40)
        contributed = [duplicate, duplicate, *unique]
        head = [*base, *contributed]
        target = [*base, duplicate, *unique, *self.ledger_rows("later-main", 25)]
        self.plant_local_ledger_commit(
            fx, base_rows=base, head_rows=head, target_rows=target
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(1, rc, out)
        self.assertIn("unpublished=1", out, out)

    def test_truncated_target_ledger_row_refuses_containment(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 735)
        contributed = self.ledger_rows("contributed", 42)
        head = [*base, *contributed]
        target = [*head, *self.ledger_rows("later-main", 25)]
        self.plant_local_ledger_commit(
            fx,
            base_rows=base,
            head_rows=head,
            target_rows=target,
            truncate_target=True,
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(1, rc, out)
        self.assertIn("unpublished=1", out, out)

    def test_target_deletion_of_base_ledger_row_refuses_containment(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 3)
        contributed = self.ledger_rows("branch", 1)
        head = [*base, *contributed]
        target = [*base[1:], *contributed, *self.ledger_rows("later-main", 1)]
        self.plant_local_ledger_commit(
            fx, base_rows=base, head_rows=head, target_rows=target
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(1, rc, out)
        self.assertIn("unpublished=1", out, out)

    def test_deleted_parent_ledger_row_refuses_containment(self) -> None:
        fx = Fixture(self.tmp)
        base = self.ledger_rows("base", 3)
        head = [*base[1:], *self.ledger_rows("branch", 1)]
        target = [*base, *self.ledger_rows("later-main", 1)]
        self.plant_local_ledger_commit(
            fx, base_rows=base, head_rows=head, target_rows=target
        )

        rc, out, _ = fx.gate_run("--scope", "all", "--classify")

        self.assertEqual(1, rc, out)
        self.assertIn("unpublished=1", out, out)

    def test_the_full_census_still_carries_every_local_only_object(self) -> None:
        """Narrowing the ALARM must not narrow the RECORD -- rescue and --json
        still have to see everything, or the refinement would lose the objects
        it declines to page about."""
        fx = Fixture(self.tmp)
        orphan = self.routine_landing(fx)
        sha = self.plant_unpublished(fx, "precious.md", "IRREPLACEABLE\n")
        rc, out, _ = fx.gate_run("--scope", "all", "--classify", "--json")
        payload = json.loads(out[out.index("{"):out.rindex("}") + 1])
        self.assertEqual(2, payload["count"])
        self.assertEqual(1, payload["unpublished_count"])
        self.assertEqual(1, payload["superseded_count"])
        got = {c["short"]: c["disposition"] for c in payload["commits"]}
        self.assertEqual("superseded", got[orphan[:7]])
        self.assertEqual("unpublished", got[sha[:7]])

class DefaultPathIsTheCensusAndSaysSo(unittest.TestCase):
    """The path the TICK actually runs: census only, no content comparison.

    Classification is opt-in since 2026-08-26 because it cost 744.85s against a
    30s bound on this repo's 1416 local-only commits and moved 32 of them (2.3%)
    -- it could not change the alarm, so it could not change anyone's action.
    These tests defend the two things that removal could quietly break.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def test_a_superseded_commit_still_alarms_and_the_line_does_not_overclaim(self):
        """⚠️ THE FAIL-OPEN THIS GUARDS.

        Without --classify no row is "unpublished". Keying the alarm on that
        list would report SILENCE over a full census -- indistinguishable from
        healthy. So the census itself alarms, and the wording drops to the
        weaker claim the gate can actually support: these commits exist only
        locally. It must NOT say they hold unpublished work, which is the
        classified claim and was not measured.
        """
        fx = Fixture(self.tmp)
        orphan = PublicationContentClassification.routine_landing(self, fx)
        rc, out, _ = fx.gate_run("--scope", "all")
        self.assertEqual(rc, 1, f"the census must still alarm:\n{out}")
        self.assertIn("count=1", out, out)
        self.assertIn(orphan[:7], out, out)
        self.assertIn("exist ONLY LOCALLY", out, out)
        self.assertNotIn("hold UNPUBLISHED work", out,
                         "the unclassified path must not make the classified claim")

    def test_the_split_counts_never_render_as_a_zero_nobody_measured(self):
        """`unpublished=0` and `superseded=0` are measurements. Not asking is
        not the same as measuring zero, and only one of them is true here."""
        fx = Fixture(self.tmp)
        PublicationContentClassification.plant_unpublished(
            self, fx, "precious.md", "IRREPLACEABLE\n")
        rc, out, _ = fx.gate_run("--scope", "all")
        self.assertEqual(rc, 1, out)
        self.assertIn("unpublished=not-classified", out, out)
        self.assertIn("superseded=not-classified", out, out)
        self.assertNotIn("unpublished=0", out, out)
        self.assertNotIn("superseded=0", out, out)

    def test_json_marks_the_counts_not_classified_rather_than_zero(self):
        fx = Fixture(self.tmp)
        PublicationContentClassification.plant_unpublished(
            self, fx, "precious.md", "IRREPLACEABLE\n")
        _, out, _ = fx.gate_run("--scope", "all", "--json")
        payload = json.loads(out[out.index("{"):out.rindex("}") + 1])
        self.assertEqual(1, payload["count"])
        self.assertFalse(payload["classified"])
        self.assertEqual("NOT CLASSIFIED", payload["unpublished_count"])
        self.assertEqual("NOT CLASSIFIED", payload["superseded_count"])
        self.assertEqual(
            ["unclassified"], [c["disposition"] for c in payload["commits"]])

    def test_an_empty_census_still_exits_zero(self):
        fx = Fixture(self.tmp)
        rc, out, _ = fx.gate_run("--scope", "all")
        self.assertEqual(rc, 0, out)
        self.assertIn("no local-only commits", out, out)


if __name__ == "__main__":
    unittest.main()
