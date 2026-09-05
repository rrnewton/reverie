#!/usr/bin/env python3
"""Bracket the change-clone gate's SCOPE and the FRESHNESS of its evidence.

This gate must ask exactly the question `ci-hub-clone-change.py dispose` asks.
When it did not, the two disagreed in public: measured 2026-08-15 over the 28
clones then present, the gate's `--all` enumeration counted 77 commits where the
own-branch count was 4, and all 4 of those were themselves already on their
remotes and appeared only because the clones' remote-tracking refs were stale.
A gate whose every alarm is false trains the reflex to ignore it, which is the
same way a floor stops holding.

Three properties, because a fix to only one of them still leaves a false report:

1.  SCOPE IS OWN-BRANCH. A fetch-destination ref such as
    `refs/pr-landing-planner/pr-<n>` holds commits that are on the remote by
    construction and must not be counted.
2.  EVIDENCE IS FRESH. A commit already on the remote must not be reported just
    because this clone last fetched before it landed.
3.  UNREACHABLE IS NOT CLEAN. If the remote cannot be reached the condition is
    undetermined, and the gate says so rather than printing a count nobody can
    date.

The refusal direction is bracketed against all three: genuinely unpushed
own-branch work is still reported, whether it sits on the checked-out branch, on
a branch that is not HEAD, or on a detached HEAD.

Fixtures are inert: a local bare repo stands in for `origin`, so nothing here
reaches the network or touches a real change clone.
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
from pathlib import Path

HEALTH = Path(__file__).resolve().parents[1]
GATE = HEALTH / "unpushed_change_clone_commits.py"
MANIFEST = "ci-hub-change-manifest.json"

SPEC = importlib.util.spec_from_file_location("unpushed_change_clone_commits", GATE)
assert SPEC is not None and SPEC.loader is not None
gate_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate_module)


def run(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=None if cwd is None else str(cwd),
                          capture_output=True, text=True)


def git(repo: Path, *args: str) -> str:
    out = run("git", "-C", str(repo), *args)
    if out.returncode != 0:
        raise AssertionError(out.stderr)
    return out.stdout.strip()


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.search = self.root / "clones"
        self.search.mkdir()
        self.origin = self.root / "origin.git"
        run("git", "init", "-q", "--bare", str(self.origin))
        self.seed = self.root / "seed"
        run("git", "init", "-q", "-b", "main", str(self.seed))
        git(self.seed, "config", "user.email", "t@example.invalid")
        git(self.seed, "config", "user.name", "t")
        (self.seed / "f.txt").write_text("seed\n")
        git(self.seed, "add", "--", "f.txt")
        git(self.seed, "commit", "-q", "-m", "seed")
        git(self.seed, "remote", "add", "origin", str(self.origin))
        git(self.seed, "push", "-q", "origin", "main")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_missing_search_root_is_unknown_not_zero_clones(self) -> None:
        missing = self.root / "absent"
        with self.assertRaisesRegex(RuntimeError, "search root is unavailable"):
            gate_module.change_clones(missing)

    def test_empty_existing_search_root_is_a_measured_zero(self) -> None:
        self.assertEqual([], gate_module.change_clones(self.search))

    def clone(self, name: str = "dev-hermit-fixture") -> Path:
        path = self.search / name
        run("git", "clone", "-q", "--single-branch", "--branch", "main",
            str(self.origin), str(path))
        git(path, "config", "user.email", "t@example.invalid")
        git(path, "config", "user.name", "t")
        (path / ".git" / MANIFEST).write_text(json.dumps({"task": "fixture-task"}))
        git(path, "switch", "-q", "-c", "ci-hub/fixture-task")
        return path

    def commit(self, repo: Path, text: str) -> str:
        (repo / "f.txt").write_text(text)
        git(repo, "add", "--", "f.txt")
        git(repo, "commit", "-q", "-m", text)
        return git(repo, "rev-parse", "HEAD")

    def assert_process_stopped(self, pid: int) -> None:
        """Accept a reaped process or zombie without a check-then-read race."""
        proc_stat = Path(f"/proc/{pid}/stat")
        for _ in range(50):
            try:
                state = proc_stat.read_text().split()[2]
            except (FileNotFoundError, ProcessLookupError):
                return
            # A zombie is dead and cannot continue mutating refs; PID 1 may
            # reap it shortly after the timed-out group leader is collected.
            if state == "Z":
                return
            time.sleep(0.01)
        self.fail(f"process {pid} remained live after group teardown")

    def gate(self) -> tuple[int, str]:
        out = run(sys.executable, str(GATE), "--search-root", str(self.search))
        return out.returncode, out.stdout

    def gate_json(self) -> tuple[int, dict]:
        out = run(sys.executable, str(GATE), "--search-root", str(self.search), "--json")
        return out.returncode, json.loads(out.stdout)

    # ---- reported: genuinely unpushed own-branch work, in all three shapes ----

    def test_reports_an_unpushed_commit_on_the_checked_out_branch(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "unpushed\n")
        remote_before = git(repo, "rev-parse", "refs/remotes/origin/main")
        (self.seed / "remote.txt").write_text("remote advanced\n")
        git(self.seed, "add", "--", "remote.txt")
        git(self.seed, "commit", "-q", "-m", "advance remote")
        remote_after = git(self.seed, "rev-parse", "HEAD")
        git(self.seed, "push", "-q", "origin", "main")
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn(head, text)
        self.assertIn("exists on no remote ref", text)
        # The report follows a real refresh, rather than preserving the result
        # by quietly skipping the operation whose cost this change bounds.
        self.assertNotEqual(remote_after, remote_before)
        self.assertEqual(git(repo, "rev-parse", "refs/remotes/origin/main"), remote_after)

    def test_reports_unpushed_work_on_a_branch_that_is_not_head(self) -> None:
        repo = self.clone()
        git(repo, "switch", "-q", "-c", "side")
        head = self.commit(repo, "parked on a side branch\n")
        git(repo, "switch", "-q", "ci-hub/fixture-task")
        # Scoping to HEAD alone would miss this entirely.
        self.assertEqual(git(repo, "rev-list", "HEAD", "--not", "--remotes"), "")
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn(head, text)

    def test_reports_unpushed_work_on_a_detached_head(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "left on a detached HEAD\n")
        git(repo, "switch", "-q", "--detach", head)
        git(repo, "branch", "-q", "-D", "ci-hub/fixture-task")
        # Scoping to --branches alone would miss this entirely.
        self.assertEqual(git(repo, "rev-list", "--branches", "--not", "--remotes"), "")
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn(head, text)

    # ---- not reported: on the remote, whatever this clone last saw ----

    def test_a_fetch_destination_ref_is_not_counted(self) -> None:
        repo = self.clone()
        git(repo, "switch", "-q", "-c", "scratch")
        fetched = self.commit(repo, "a fetched pull-request head\n")
        git(repo, "update-ref", "refs/pr-landing-planner/pr-1", fetched)
        git(repo, "switch", "-q", "ci-hub/fixture-task")
        git(repo, "branch", "-q", "-D", "scratch")
        # Precondition: the previous `--all` enumeration counted this and alarmed.
        self.assertIn(fetched, git(repo, "rev-list", "--exclude=refs/tags/*", "--all",
                                   "--not", "--remotes").split())
        code, text = self.gate()
        self.assertEqual(code, 0, text)
        self.assertIn("holding=0", text)
        self.assertIn("no change clone holds unpushed work", text)

    def test_a_commit_already_on_the_remote_is_not_counted_when_the_snapshot_is_stale(self) -> None:
        repo = self.clone()
        snapshot = git(repo, "rev-parse", "refs/remotes/origin/main")
        head = self.commit(repo, "pushed, but not seen yet\n")
        git(repo, "push", "-q", "origin", f"{head}:refs/heads/main")
        # Rewind only the remote-tracking ref, reproducing the measured state of
        # dev-hermit-p1-a-producer-path: on the remote, absent from the snapshot.
        git(repo, "update-ref", "refs/remotes/origin/main", snapshot)
        self.assertEqual(git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), head)
        code, text = self.gate()
        self.assertEqual(code, 0, text)
        self.assertIn("holding=0", text)

    # ---- unreachable: undetermined, and never silently clean ----

    def test_an_unreachable_remote_is_undetermined_not_clean(self) -> None:
        repo = self.clone()
        # Nothing is at risk by this clone's own snapshot, which is exactly when
        # an unverifiable answer is tempting to round down to "clean".
        self.assertEqual(git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), "")
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("undetermined=1", text)
        self.assertIn("state=NO_RESULT", text)
        self.assertIn("NO_RESULT/remote-unreachable", text)
        self.assertNotIn("no change clone holds unpushed work", text)

    def test_population_above_thirty_six_finishes_before_the_outer_bound(self) -> None:
        clones = [self.search / f"clone-{number:02d}" for number in range(49)]
        inspection_budget = 0.35

        def blocked_until_deadline(_clone: Path, deadline: float) -> dict:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise gate_module.InspectionDeadlineExceeded("fixture deadline")
                time.sleep(min(remaining, 0.01))

        started = time.monotonic()
        with mock.patch.object(gate_module, "inspect", side_effect=blocked_until_deadline):
            rows = gate_module.inspect_all(clones, inspection_budget=inspection_budget)
        elapsed = time.monotonic() - started

        # The budget is scaled down from the production 25s/30s inner/outer
        # ladder. All 49 clones still receive rows, including queued work that
        # could not begin before the common deadline.
        scaled_outer_bound = inspection_budget * (
            gate_module.OUTER_TIMEOUT_SECS / gate_module.INSPECTION_DEADLINE_SECS
        )
        self.assertLess(elapsed, scaled_outer_bound)
        self.assertEqual([row["clone"] for row in rows], [str(clone) for clone in clones])
        self.assertTrue(all(not row["remote_reachable"] for row in rows))
        self.assertTrue(all("InspectionDeadlineExceeded" in row["undetermined_reason"]
                            for row in rows))

    def test_an_unfinished_inspection_is_undetermined_not_clean(self) -> None:
        clone = self.clone()
        with mock.patch.object(
            gate_module,
            "inspect",
            side_effect=subprocess.TimeoutExpired(["git", "status"], 1),
        ):
            row = gate_module.inspect_or_undetermined(clone, time.monotonic() + 1)
        self.assertFalse(row["remote_reachable"])
        self.assertIn("inspection did not finish", row["undetermined_reason"])
        self.assertIsNone(row["unpushed_count"])

    def test_timeout_kills_and_reaps_a_background_helper(self) -> None:
        helper = self.root / "git-with-helper"
        pid_file = self.root / "helper.pid"
        helper.write_text(
            "#!/bin/sh\n"
            "sleep 30 &\n"
            "child=$!\n"
            "printf '%s\\n' \"$child\" > \"$1\"\n"
            "wait \"$child\"\n"
        )
        helper.chmod(0o755)

        with self.assertRaises(subprocess.TimeoutExpired):
            gate_module.run_bounded([str(helper), str(pid_file)], 0.1)

        child = int(pid_file.read_text().strip())
        self.assert_process_stopped(child)

    def test_timeout_kills_term_ignoring_helper_after_leader_exits(self) -> None:
        helper = self.root / "git-with-term-ignoring-helper"
        pid_file = self.root / "term-ignoring-helper.pid"
        helper.write_text(
            "#!/bin/sh\n"
            "trap 'exit 0' TERM\n"
            "(\n"
            "  trap '' TERM\n"
            "  exec </dev/null >/dev/null 2>&1\n"
            "  sleep 30\n"
            ") &\n"
            "child=$!\n"
            "printf '%s\\n' \"$child\" > \"$1\"\n"
            "while :; do sleep 1; done\n"
        )
        helper.chmod(0o755)

        with self.assertRaises(subprocess.TimeoutExpired):
            gate_module.run_bounded([str(helper), str(pid_file)], 0.1)

        child = int(pid_file.read_text().strip())
        self.assert_process_stopped(child)

    def test_timed_out_fetch_removes_only_new_temporary_pack_files(self) -> None:
        repo = self.clone()
        pack_dir = repo / ".git" / "objects" / "pack"
        before = pack_dir / "tmp_pack_before"
        completed = pack_dir / "pack-before.pack"
        before.write_bytes(b"pre-existing temporary pack")
        completed.write_bytes(b"completed pack")

        created = pack_dir / "tmp_pack_created_by_fetch"

        def time_out(
            command: list[str], timeout: float, while_running=None
        ) -> None:
            self.assertEqual(
                command,
                [
                    "git", "-C", str(repo), "fetch", "--quiet", "origin",
                    "+refs/heads/*:refs/remotes/origin/*",
                ],
            )
            created.write_bytes(b"partial pack")
            info = created.stat()
            with mock.patch.object(
                gate_module,
                "open_partial_packs_from_group",
                return_value=({(created, info.st_dev, info.st_ino)}, []),
            ):
                while_running(mock.Mock(pid=1234))
            raise subprocess.TimeoutExpired(command, timeout)

        with (
            mock.patch.object(gate_module.shutil, "which", return_value=None),
            mock.patch.object(gate_module, "run_bounded", side_effect=time_out),
        ):
            reachable, reason = gate_module.refresh_remote_tracking(
                repo, time.monotonic() + 1
            )

        self.assertFalse(reachable)
        self.assertEqual(reason, "remote fetch timed out")
        self.assertFalse(created.exists())
        self.assertEqual(before.read_bytes(), b"pre-existing temporary pack")
        self.assertEqual(completed.read_bytes(), b"completed pack")

    def test_partial_pack_capture_is_bound_to_the_fetch_process_group(self) -> None:
        repo = self.clone()
        pack_dir = repo / ".git" / "objects" / "pack"
        before = pack_dir / "tmp_pack_before"
        created = pack_dir / "tmp_pack_created_by_fetch"
        concurrent = pack_dir / "tmp_pack_concurrent_fetch"
        before.write_bytes(b"pre-existing")
        helper = self.root / "hold-pack-open"
        helper.write_text(
            "#!/bin/sh\n"
            "exec 3>\"$1\"\n"
            "printf '%s\\n' \"$2\" >&3\n"
            "while :; do sleep 1; done\n"
        )
        helper.chmod(0o755)
        concurrent_process = subprocess.Popen(
            [str(helper), str(concurrent), "concurrent fetch"],
            start_new_session=True,
        )
        captured: set[tuple[Path, int, int]] = set()
        errors: list[str] = []

        def capture(process: subprocess.Popen[str]) -> None:
            packs, failures = gate_module.open_partial_packs_from_group(
                repo, process.pid, {before}
            )
            captured.update(packs)
            errors.extend(failures)

        try:
            for _ in range(50):
                if concurrent.exists():
                    break
                time.sleep(0.01)
            else:
                self.fail("concurrent helper did not create its pack")

            with self.assertRaises(subprocess.TimeoutExpired):
                gate_module.run_bounded(
                    [str(helper), str(created), "timed-out fetch"],
                    0.1,
                    while_running=capture,
                )
        finally:
            gate_module.terminate_process_group(concurrent_process)

        self.assertEqual(errors, [])
        self.assertEqual({path for path, _device, _inode in captured}, {created})
        self.assertEqual(gate_module.remove_partial_packs(captured), [])
        self.assertFalse(created.exists())
        self.assertEqual(before.read_bytes(), b"pre-existing")
        self.assertEqual(concurrent.read_bytes(), b"concurrent fetch\n")

    def test_pack_closed_before_timeout_is_still_removed(self) -> None:
        repo = self.clone()
        pack = repo / ".git" / "objects" / "pack" / "tmp_pack_closed_early"
        helper = self.root / "close-pack-before-timeout"
        helper.write_text(
            "#!/bin/sh\n"
            "exec 3>\"$1\"\n"
            "printf partial-pack >&3\n"
            "sleep 0.05\n"
            "exec 3>&-\n"
            "sleep 1\n"
        )
        helper.chmod(0o755)
        captured: set[tuple[Path, int, int]] = set()

        def observe(process: subprocess.Popen[str]) -> None:
            packs, errors = gate_module.open_partial_packs_from_group(
                repo,
                process.pid,
                {path for path, _device, _inode in captured},
            )
            self.assertEqual(errors, [])
            captured.update(packs)

        with self.assertRaises(subprocess.TimeoutExpired):
            gate_module.run_bounded(
                [str(helper), str(pack)],
                0.15,
                while_running=observe,
            )

        self.assertTrue(pack.exists())
        self.assertEqual({path for path, _device, _inode in captured}, {pack})
        self.assertEqual(gate_module.remove_partial_packs(captured), [])
        self.assertFalse(pack.exists())

    def test_partial_pack_cleanup_preserves_a_replaced_path(self) -> None:
        repo = self.clone()
        path = repo / ".git" / "objects" / "pack" / "tmp_pack_fetch"
        path.write_bytes(b"timed-out fetch")
        info = path.stat()
        captured = {(path, info.st_dev, info.st_ino)}
        path.unlink()
        path.write_bytes(b"replacement")

        self.assertEqual(gate_module.remove_partial_packs(captured), [])
        self.assertEqual(path.read_bytes(), b"replacement")

    def test_successful_fetch_preserves_normal_pack_files(self) -> None:
        repo = self.clone()
        pack_dir = repo / ".git" / "objects" / "pack"
        pack = pack_dir / "pack-created.pack"
        index = pack_dir / "pack-created.idx"

        def succeed(
            command: list[str], _timeout: float, while_running=None
        ) -> subprocess.CompletedProcess[str]:
            temporary = pack_dir / "tmp_pack_created_by_fetch"
            temporary.write_bytes(b"completed pack")
            temporary.rename(pack)
            index.write_bytes(b"completed index")
            return subprocess.CompletedProcess(command, 0, "", "")

        with (
            mock.patch.object(gate_module.shutil, "which", return_value=None),
            mock.patch.object(gate_module, "run_bounded", side_effect=succeed),
        ):
            reachable, reason = gate_module.refresh_remote_tracking(
                repo, time.monotonic() + 1
            )

        self.assertTrue(reachable)
        self.assertIsNone(reason)
        self.assertEqual(pack.read_bytes(), b"completed pack")
        self.assertEqual(index.read_bytes(), b"completed index")

    def test_non_object_manifest_is_undetermined_and_does_not_abort_pool(self) -> None:
        malformed = self.clone("dev-hermit-malformed")
        self.clone("dev-hermit-good")
        (malformed / ".git" / MANIFEST).write_text("[]\n")
        code, payload = self.gate_json()
        self.assertEqual(code, 1)
        self.assertEqual(payload["clones"], 2)
        self.assertEqual(payload["undetermined"], 1)
        row = next(row for row in payload["rows"] if row["clone"] == str(malformed))
        self.assertIn("MalformedManifest", row["undetermined_reason"])

    def test_unreachable_stale_unpushed_shape_is_no_result_not_containment(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "looks unpushed against stale refs\n")
        (repo / "dirty.txt").write_text("local dirty fact\n")
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn("undetermined=1", text)
        self.assertIn("unpushed=UNDETERMINED", text)
        self.assertIn("uncommitted=1", text)
        self.assertIn("state=NO_RESULT", text)
        self.assertIn("holding_local=1", text)
        self.assertNotIn(head, text)
        self.assertNotIn("exists on no remote ref", text)

    def test_json_carries_reachability_per_clone(self) -> None:
        repo = self.clone()
        code, payload = self.gate_json()
        self.assertEqual(code, 0)
        self.assertEqual(payload["undetermined"], 0)
        self.assertTrue(payload["rows"][0]["remote_reachable"])
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        code, payload = self.gate_json()
        self.assertEqual(code, 1)
        self.assertEqual(payload["undetermined"], 1)
        self.assertFalse(payload["rows"][0]["remote_reachable"])
        self.assertIsNone(payload["rows"][0]["unpushed_count"])

    def test_a_clone_with_no_manifest_is_not_inspected(self) -> None:
        repo = self.clone()
        self.commit(repo, "unpushed\n")
        (repo / ".git" / MANIFEST).unlink()
        code, text = self.gate()
        self.assertEqual(code, 0, text)
        self.assertIn("clones=0", text)
        self.assertIn("no change clones present", text)


if __name__ == "__main__":
    unittest.main()
