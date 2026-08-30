#!/usr/bin/env python3

import importlib.util
import contextlib
import fcntl
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "pressure_schedule.py"
SPEC = importlib.util.spec_from_file_location("pressure_schedule", SCRIPT)
assert SPEC and SPEC.loader
pressure_schedule = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = pressure_schedule
SPEC.loader.exec_module(pressure_schedule)


class PressureScheduleTest(unittest.TestCase):
    def test_tick_output_exposes_summary_as_its_own_key(self):
        with tempfile.TemporaryDirectory() as directory:
            output = io.StringIO()
            with mock.patch.object(
                pressure_schedule.shutil, "which", return_value=None
            ):
                with contextlib.redirect_stdout(output):
                    self.assertEqual(
                        pressure_schedule.launch(Path(directory)),
                        1,
                    )
            self.assertEqual(
                output.getvalue().splitlines(),
                [
                    "state=unavailable",
                    "summary=recurring pressure test not launched; missing "
                    "systemctl,systemd-run,with-proxy",
                ],
            )

    def test_concurrent_launcher_cannot_replace_the_run_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock_path = pressure_schedule.launch_lock_path(root)
            lock_path.parent.mkdir(parents=True)
            calls = []
            with lock_path.open("a+") as held:
                fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                with mock.patch.object(
                    pressure_schedule.shutil,
                    "which",
                    side_effect=lambda name: f"/bin/{name}",
                ):
                    self.assertEqual(
                        pressure_schedule.launch(
                            root,
                            lambda command, **kwargs: calls.append(command),
                        ),
                        0,
                    )
            self.assertEqual(calls, [])
            self.assertFalse(
                pressure_schedule.run_registry.record_path(
                    root, pressure_schedule.UNIT
                ).exists()
            )

    def test_launch_is_detached_and_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "/bin/systemctl":
                    return subprocess.CompletedProcess(command, 3, "", "")
                if "rev-parse" in command:
                    return subprocess.CompletedProcess(command, 0, "a" * 40 + "\n", "")
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.object(
                pressure_schedule.shutil,
                "which",
                side_effect=lambda name: {
                    "systemctl": "/bin/systemctl",
                    "systemd-run": "/bin/systemd-run",
                    "with-proxy": "/bin/with-proxy",
                }[name],
            ):
                self.assertEqual(pressure_schedule.launch(root, run), 0)

            launched = calls[3]
            self.assertIn("--unit=hermit-pressure-test", launched)
            self.assertIn(
                f"--property=RuntimeMaxSec={pressure_schedule.UNIT_RUNTIME_SECONDS}",
                launched,
            )
            self.assertIn(
                f"--working-directory={pressure_schedule.SOURCE_ROOT}", launched
            )
            self.assertTrue(any(arg.startswith("--setenv=PATH=") for arg in launched))
            self.assertIn(f"--setenv=DEV_HERMIT_PARENT={root}", launched)
            self.assertIn(
                f"--setenv=DEV_HERMIT_TOOL_ROOT={pressure_schedule.SOURCE_ROOT}",
                launched,
            )
            self.assertEqual(
                launched[launched.index("--") + 1 :],
                [
                    "/bin/with-proxy",
                    sys.executable,
                    str(
                        pressure_schedule.SOURCE_ROOT
                        / "ci-hub"
                        / "series"
                        / "pressure_schedule.py"
                    ),
                    "_run",
                    "--target",
                    "a" * 40,
                    "--record",
                    str(
                        root
                        / "ignored"
                        / "validate"
                        / "runs"
                        / "hermit-pressure-test.json"
                    ),
                    "--state-root",
                    str(root),
                ],
            )
            record = pressure_schedule.run_registry.read_record(
                root / "ignored" / "validate" / "runs" / "hermit-pressure-test.json"
            )
            self.assertEqual(record["schema_version"], 1)
            self.assertEqual(record["kind"], "bench")
            self.assertEqual(record["state"], "running")
            self.assertEqual(record["target"], "a" * 40)
            checkout = Path(record["checkout"])
            self.assertEqual(checkout.parent, root / "worktrees" / "validate")
            self.assertTrue(checkout.name.startswith("pressure-aaaaaaaaaaaa-"))

    def test_previous_failure_is_reported_while_next_run_launches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pressure_schedule.write_state(root, {"state": "failed", "target": "b" * 40})
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "/bin/systemctl":
                    return subprocess.CompletedProcess(command, 3, "", "")
                if "rev-parse" in command:
                    return subprocess.CompletedProcess(command, 0, "a" * 40 + "\n", "")
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.object(
                pressure_schedule.shutil,
                "which",
                side_effect=lambda name: f"/bin/{name}",
            ):
                self.assertEqual(pressure_schedule.launch(root, run), 1)
            self.assertTrue(any("systemd-run" in command[0] for command in calls))

    def test_active_unit_does_not_launch_another(self):
        with tempfile.TemporaryDirectory() as directory:
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.object(
                pressure_schedule.shutil,
                "which",
                side_effect=lambda name: f"/bin/{name}",
            ):
                self.assertEqual(pressure_schedule.launch(Path(directory), run), 0)
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][1:4], ["--user", "is-active", "--quiet"])

    def test_unreadable_unit_state_does_not_overwrite_the_run_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def run(command, **kwargs):
                return subprocess.CompletedProcess(command, 1, "", "no user manager")

            with mock.patch.object(
                pressure_schedule.shutil,
                "which",
                side_effect=lambda name: f"/bin/{name}",
            ):
                self.assertEqual(pressure_schedule.launch(root, run), 1)
            self.assertFalse(
                (
                    root / "ignored" / "validate" / "runs" / "hermit-pressure-test.json"
                ).exists()
            )

    def test_lock_reserves_both_slots_and_bounds_the_child(self):
        command = pressure_schedule.locked_command(
            Path("/parent"),
            Path("/parent/ignored/worktree"),
            "a" * 40,
            Path("/parent/ignored/results"),
            Path("/parent/ignored/validate/runs/hermit-pressure-test.json"),
        )
        self.assertEqual(command[1:4], ["validate-lock", "run", "--agent"])
        self.assertIn("bench", command)
        self.assertIn(str(pressure_schedule.LOCK_WAIT_SECONDS), command)
        self.assertIn(str(pressure_schedule.LOCK_CHILD_DEADLINE_SECONDS), command)
        self.assertIn("a" * 40, command)
        self.assertEqual(
            command[0], str(pressure_schedule.SOURCE_ROOT / "ci-hub" / "ci-hub")
        )
        self.assertIn("--state-root", command)
        self.assertEqual(command[command.index("--state-root") + 1], "/parent")
        self.assertIn(
            "/parent/ignored/validate/runs/hermit-pressure-test.json", command
        )

    def test_sample_is_bounded_and_emitted(self):
        run, emit = pressure_schedule.pressure_commands(
            Path("/checkout"), Path("/results")
        )
        self.assertEqual(run[1], "run")
        self.assertNotIn("--mode", run)
        self.assertEqual(run[run.index("--sample") + 1], "10")
        self.assertEqual(run[run.index("--jobs") + 1], "4")
        self.assertEqual(run[run.index("--cell-timeout") + 1], "60")
        self.assertEqual(run[run.index("--run-timeout") + 1], "1500")
        self.assertEqual(
            emit,
            [
                "/checkout/ci/compat-envelope/pressure-test.rs",
                "emit-series",
                "--results",
                "/results",
            ],
        )

    def test_campaign_uses_clean_worktree_and_finishes_run_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "hermit").mkdir()
            record = pressure_schedule.run_registry.record_path(
                root, pressure_schedule.UNIT
            )
            pressure_schedule.run_registry.write_record(
                record,
                {
                    "schema_version": 1,
                    "kind": "bench",
                    "state": "running",
                    "unit": f"{pressure_schedule.UNIT}.service",
                    "target": "a" * 40,
                    "repo": "rrnewton/hermit",
                    "checkout": str(root / "worktrees/validate/pressure-test"),
                    "log": str(root / "ignored/ci-hub/pressure-test.log"),
                    "agent": "tick-pressure-test",
                    "started_at": "2026-08-28T07:00:00Z",
                    "producer": pressure_schedule.run_registry.PRODUCER,
                    "admission": "ci-hub validate-lock",
                },
            )
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.object(
                pressure_schedule.subprocess, "run", side_effect=run
            ):
                self.assertEqual(
                    pressure_schedule.run_campaign("a" * 40, record, root), 0
                )
            self.assertTrue(calls[0][0].endswith("ci-hub/bin/wrkslots"))
            self.assertTrue(
                calls[0][0].startswith(str(pressure_schedule.SOURCE_ROOT))
            )
            self.assertIn("create", calls[0])
            self.assertIn("--slot-type", calls[0])
            self.assertIn("validate", calls[0])
            self.assertIn("submodule", calls[1])
            self.assertIn("validate-lock", calls[2])
            self.assertEqual(
                calls[2][0],
                str(pressure_schedule.SOURCE_ROOT / "ci-hub" / "ci-hub"),
            )
            self.assertEqual(
                calls[2][calls[2].index("--state-root") + 1], str(root)
            )
            self.assertIn("bench", calls[2])
            self.assertTrue(calls[3][0].endswith("ci-hub/bin/wrkslots"))
            self.assertIn("remove", calls[3])
            self.assertIn("--validate-complete", calls[3])
            finished = pressure_schedule.run_registry.read_record(record)
            self.assertEqual(finished["state"], "completed")
            self.assertEqual(finished["result"], "passed")

    def test_non_sha_main_is_a_visible_refusal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def run(command, **kwargs):
                if command[0] == "/bin/systemctl":
                    return subprocess.CompletedProcess(command, 3, "", "")
                if "rev-parse" in command:
                    return subprocess.CompletedProcess(command, 0, "not-a-sha\n", "")
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.object(
                pressure_schedule.shutil,
                "which",
                side_effect=lambda name: f"/bin/{name}",
            ):
                self.assertEqual(pressure_schedule.launch(root, run), 1)

    def test_failed_campaign_still_emits_completed_rows(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout = root / "checkout"
            results = root / "results"
            checkout.mkdir()
            results.mkdir()
            (results / "run.json").write_text("{}\n")
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                return subprocess.CompletedProcess(command, 1 if len(calls) == 1 else 0)

            with mock.patch.object(
                pressure_schedule.subprocess, "run", side_effect=run
            ):
                self.assertEqual(
                    pressure_schedule.run_locked(checkout, results, root), 1
                )
            self.assertEqual([command[1] for command in calls], ["run", "emit-series"])

    def test_missing_pressure_run_metadata_refuses_a_false_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout = root / "checkout"
            results = root / "results"
            checkout.mkdir()
            results.mkdir()
            with mock.patch.object(
                pressure_schedule.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(["pressure-test"], 0),
            ):
                self.assertEqual(
                    pressure_schedule.run_locked(checkout, results, root), 2
                )

    def test_missing_runtime_is_a_visible_refusal(self):
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(
                pressure_schedule.shutil, "which", return_value=None
            ):
                self.assertEqual(pressure_schedule.launch(Path(directory)), 1)


if __name__ == "__main__":
    unittest.main()
