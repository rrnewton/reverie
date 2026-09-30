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


ORIGINAL_PROBE_VERSION = pressure_schedule.unwrapped_git.probe_version


def fake_host_gits(root):
    """A fake wrapper `git` beside a fake git.meta.real, and a plain git."""
    local = root / "fake-local-bin"
    plain = root / "fake-plain-bin"
    for directory, name, version in (
        (local, "git", "git version 2.53.0-Meta"),
        (local, "git.meta.real", "git version 2.53.0-Meta"),
        (plain, "git", "git version 2.52.0"),
    ):
        directory.mkdir(exist_ok=True)
        script = directory / name
        script.write_text(f"#!/bin/sh\necho '{version}'\n")
        script.chmod(0o755)
    # The wrapper's configuration, naming the fake git.meta.real; tests patch
    # unwrapped_git.WRAPPER_CONFIG to wrapper_config(root).
    config = wrapper_config(root)
    config.parent.mkdir(exist_ok=True)
    config.write_text(f'[binaries]\ngit = "{local / "git.meta.real"}"\n')
    return local, local / "git.meta.real", plain


def wrapper_config(root):
    return root / "fake-etc" / "scm-telemetry.toml"


def patch_wrapper_config(root):
    return mock.patch.object(
        pressure_schedule.unwrapped_git, "WRAPPER_CONFIG", wrapper_config(root)
    )


class PressureScheduleTest(unittest.TestCase):
    def setUp(self):
        # Keep launch() from resolving or running the host's git: the default
        # here is "no git.meta.real", which leaves PATH unchanged. The
        # UnwrappedGitLaunchTest cases below use fake binaries instead.
        absent = mock.patch.object(
            pressure_schedule.unwrapped_git,
            "REAL_GIT",
            Path("/nonexistent-unwrapped-git-fixture/git.meta.real"),
        )
        absent.start()
        self.addCleanup(absent.stop)
        probe = mock.patch.object(
            pressure_schedule.unwrapped_git,
            "probe_version",
            return_value=("git version fixture", ""),
        )
        probe.start()
        self.addCleanup(probe.stop)

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

    def test_health_tick_launch_is_detached_bounded_and_owns_tool_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            operational_tool = root / "operational-tool"
            operational_tool.write_text("#!/bin/sh\nexit 0\n")
            operational_tool.chmod(0o755)
            tool_cache = root / "tool-cache"
            tool_cache.mkdir()
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "/bin/systemctl":
                    return subprocess.CompletedProcess(command, 3, "", "")
                if "rev-parse" in command:
                    return subprocess.CompletedProcess(command, 0, "a" * 40 + "\n", "")
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.dict(
                pressure_schedule.os.environ,
                {
                    "DEV_HERMIT_OPERATIONAL_TOOL": str(operational_tool),
                    "DEV_HERMIT_TOOL_PARENT_SHA": "b" * 40,
                    "DEV_HERMIT_TOOL_CACHE": str(tool_cache),
                    "PATH": "/bin",
                },
                clear=True,
            ):
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
            self.assertIn(f"--working-directory={root}", launched)
            self.assertTrue(any(arg.startswith("--setenv=PATH=") for arg in launched))
            self.assertIn(f"--setenv=DEV_HERMIT_PARENT={root}", launched)
            self.assertIn(
                f"--setenv=DEV_HERMIT_TOOL_ROOT={pressure_schedule.SOURCE_ROOT}",
                launched,
            )
            # The user manager's environment lacks GIT_ALLOW_PROTOCOL; without
            # it the wrkslots post-provision submodule update refuses the
            # insteadOf-rewritten local submodule URLs
            # (https://github.com/rrnewton/dev-hermit/issues/431).
            self.assertIn(
                "--setenv=GIT_ALLOW_PROTOCOL=https:file:ssh",
                launched[: launched.index("--")],
            )
            self.assertEqual(
                launched[launched.index("--") + 1 :],
                [
                    "/bin/with-proxy",
                    str(operational_tool),
                    "--run-current-tool",
                    "--state-root",
                    str(root),
                    "--cache-root",
                    str(tool_cache),
                    "--tool-parent-sha",
                    "b" * 40,
                    "--tool-relative",
                    "ci-hub/series/pressure_schedule.py",
                    "--",
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
            self.assertNotIn(
                str(
                    pressure_schedule.SOURCE_ROOT
                    / "ci-hub"
                    / "series"
                    / "pressure_schedule.py"
                ),
                launched,
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
            # The recorded placement is the one the unit was launched with
            # (https://github.com/rrnewton/hermit/issues/3265).
            placement = record["cpu_placement"]
            self.assertIsNone(
                pressure_schedule.cpu_placement.validate_record(placement)
            )
            allowed = placement["allowed_cpus"]
            self.assertIn(
                f"--setenv=HERMIT_CI_ALLOWED_CPUS={allowed}", launched
            )
            self.assertEqual(
                [f"--property=AllowedCPUs={allowed}"] if allowed else [],
                [arg for arg in launched if arg.startswith("--property=AllowedCPUs=")],
            )

    def test_manual_launch_keeps_stable_checkout_path(self):
        command = pressure_schedule.launch_command(
            Path("/parent"),
            "/bin/systemd-run",
            "/bin/with-proxy",
            "a" * 40,
            Path("/parent/run.json"),
            {"PATH": "/bin"},
        )
        self.assertIn(f"--working-directory={pressure_schedule.SOURCE_ROOT}", command)
        self.assertEqual(
            command[command.index("--") + 1 : command.index("_run")],
            [
                "/bin/with-proxy",
                sys.executable,
                str(
                    pressure_schedule.SOURCE_ROOT
                    / "ci-hub"
                    / "series"
                    / "pressure_schedule.py"
                ),
            ],
        )

    def test_incomplete_operational_tool_handoff_refuses_before_run_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            operational_tool = root / "operational-tool"
            operational_tool.write_text("#!/bin/sh\nexit 0\n")
            operational_tool.chmod(0o755)
            tool_cache = root / "tool-cache"
            tool_cache.mkdir()
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "/bin/systemctl":
                    return subprocess.CompletedProcess(command, 3, "", "")
                if "rev-parse" in command:
                    return subprocess.CompletedProcess(command, 0, "a" * 40 + "\n", "")
                return subprocess.CompletedProcess(command, 0, "", "")

            output = io.StringIO()
            with mock.patch.dict(
                pressure_schedule.os.environ,
                {
                    "DEV_HERMIT_OPERATIONAL_TOOL": str(operational_tool),
                    "DEV_HERMIT_TOOL_CACHE": str(tool_cache),
                    "PATH": "/bin",
                },
                clear=True,
            ):
                with mock.patch.object(
                    pressure_schedule.shutil,
                    "which",
                    side_effect=lambda name: f"/bin/{name}",
                ):
                    with contextlib.redirect_stdout(output):
                        self.assertEqual(pressure_schedule.launch(root, run), 1)

            self.assertIn("state=refused", output.getvalue())
            self.assertIn("incomplete operational-tool handoff", output.getvalue())
            self.assertFalse(
                pressure_schedule.run_registry.record_path(
                    root, pressure_schedule.UNIT
                ).exists()
            )
            self.assertFalse(any(command[0] == "/bin/systemd-run" for command in calls))

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
        # pressure-test.rs derives each cell's bound from its manifest budget
        # and refuses any smaller --cell-timeout; the former fixed 60 s was
        # below every cell's requirement, so every sample refused before
        # launch (https://github.com/rrnewton/dev-hermit/issues/431).
        self.assertFalse(
            [arg for arg in run if arg.startswith("--cell-timeout")], run
        )
        self.assertFalse(hasattr(pressure_schedule, "CELL_TIMEOUT_SECONDS"))
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
                pressure_schedule.socket,
                "gethostname",
                return_value="devbig030.facebook.com",
            ) as gethostname, mock.patch.object(
                pressure_schedule.subprocess, "run", side_effect=run
            ):
                self.assertEqual(
                    pressure_schedule.run_campaign("a" * 40, record, root), 0
                )
            gethostname.assert_called_once_with()
            self.assertTrue(calls[0][0].endswith("ci-hub/bin/wrkslots"))
            self.assertTrue(
                calls[0][0].startswith(str(pressure_schedule.SOURCE_ROOT))
            )
            self.assertIn("create", calls[0])
            self.assertLess(calls[0].index("--machine"), calls[0].index("create"))
            self.assertLess(calls[0].index("--wait-lock"), calls[0].index("create"))
            self.assertEqual(
                calls[0][calls[0].index("--wait-lock") + 1],
                str(pressure_schedule.SLOT_LOCK_WAIT_SECONDS),
            )
            self.assertEqual(
                calls[0][calls[0].index("--machine") + 1], "devbig030"
            )
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
            self.assertLess(calls[3].index("--machine"), calls[3].index("remove"))
            self.assertLess(calls[3].index("--wait-lock"), calls[3].index("remove"))
            self.assertEqual(
                calls[3][calls[3].index("--wait-lock") + 1],
                str(pressure_schedule.SLOT_LOCK_WAIT_SECONDS),
            )
            self.assertEqual(
                calls[3][calls[3].index("--machine") + 1], "devbig030"
            )
            self.assertIn("--validate-complete", calls[3])
            finished = pressure_schedule.run_registry.read_record(record)
            self.assertEqual(finished["state"], "completed")
            self.assertEqual(finished["result"], "passed")

    def _campaign_record(self, root):
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
                "started_at": "2026-09-29T12:00:00Z",
                "producer": pressure_schedule.run_registry.PRODUCER,
                "admission": "ci-hub validate-lock",
            },
        )
        return record

    def test_provisioning_waits_on_a_busy_wrkslots_lock(self):
        # Model wrkslots' global --wait-lock contract: the default 0 refuses a
        # busy configuration or state lock at once with the message below; a
        # positive value waits up to that many seconds.  Hold the lock for
        # 540 s, the disk snapshot's documented lifecycle-lock hold
        # (ci-hub/validate/start_unit.py).  In ignored/ci-hub/pressure-test.log,
        # 12 of the 15 `wrkslots create` failures behind
        # https://github.com/rrnewton/dev-hermit/issues/431 were this refusal,
        # and 3 further ticks recorded it as their `wrkslots remove`
        # cleanup_error.
        held_seconds = 540.0
        wrkslots_calls = []

        def run(command, **kwargs):
            if not command[0].endswith("ci-hub/bin/wrkslots"):
                return subprocess.CompletedProcess(command, 0, "", "")
            subcommand = next(
                index
                for index, argument in enumerate(command)
                if argument in ("create", "remove")
            )
            global_options = command[1:subcommand]
            wait = 0.0
            if "--wait-lock" in global_options:
                wait = float(global_options[global_options.index("--wait-lock") + 1])
            wrkslots_calls.append((command[subcommand], wait))
            if wait < held_seconds:
                return subprocess.CompletedProcess(
                    command,
                    3,
                    "",
                    "REFUSED: state lock is busy for /parent/worktrees/ACTIVE; "
                    "retry after the current command exits\n",
                )
            return subprocess.CompletedProcess(command, 0, "", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            record = self._campaign_record(root)
            output = io.StringIO()
            with mock.patch.object(
                pressure_schedule.subprocess, "run", side_effect=run
            ), contextlib.redirect_stdout(output):
                self.assertEqual(
                    pressure_schedule.run_campaign("a" * 40, record, root),
                    0,
                    output.getvalue(),
                )
            self.assertEqual(
                [name for name, _wait in wrkslots_calls], ["create", "remove"]
            )
            finished = pressure_schedule.run_registry.read_record(record)
            self.assertEqual(finished["state"], "completed")
            self.assertIsNone(finished.get("cleanup_error"))

    def test_create_failure_summary_leads_with_the_wrkslots_refusal(self):
        # wrkslots prints unrelated registry findings before the refusal; the
        # 2026-09-25 and 2026-09-28 ticks were summarised as the WARNING line
        # while the real refusal was the failed post-provision hook.
        stderr = (
            "WARNING: registry/storage state remains INCONSISTENT (106 finding(s)); "
            "this create target is distinct and none of the reported state was changed\n"
            "INCONSISTENT kind=directory-without-row target=agent/x: exists\n"
            "post-provision hook 1/1 checkout=checkout: git submodule update --init --recursive\n"
            "fatal: transport 'file' not allowed\n"
            "REFUSED: post-provision hook 1 for checkout checkout failed with rc 1; "
            "slot was left for inspection\n"
            "REMEDY: run 'wrkslots create --help'\n"
        )

        def run(command, **kwargs):
            if "create" in command:
                return subprocess.CompletedProcess(command, 3, "", stderr)
            return subprocess.CompletedProcess(command, 0, "", "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            record = self._campaign_record(root)
            with mock.patch.object(
                pressure_schedule.subprocess, "run", side_effect=run
            ):
                with self.assertRaises(RuntimeError) as raised:
                    pressure_schedule.run_campaign("a" * 40, record, root)
            first_line = str(raised.exception).splitlines()[0]
            self.assertEqual(
                first_line,
                "wrkslots create: REFUSED: post-provision hook 1 for checkout "
                "checkout failed with rc 1; slot was left for inspection",
            )
            # The complete wrkslots output is retained after the headline.
            self.assertIn("fatal: transport 'file' not allowed", str(raised.exception))
            self.assertEqual(pressure_schedule.read_state(root)["state"], "failed")

    def test_unit_runtime_covers_every_bounded_phase(self):
        self.assertGreater(
            pressure_schedule.UNIT_RUNTIME_SECONDS,
            2 * pressure_schedule.SLOT_LOCK_WAIT_SECONDS
            + pressure_schedule.LOCK_WAIT_SECONDS
            + pressure_schedule.LOCK_CHILD_DEADLINE_SECONDS,
        )
        # pressure-test.rs puts the run in a scope with RuntimeMaxSec equal to
        # the whole-run bound plus max(60, run/10) seconds of grace; the
        # validate-lock child deadline must enclose that scope.
        run_timeout = pressure_schedule.RUN_TIMEOUT_SECONDS
        self.assertGreater(
            pressure_schedule.LOCK_CHILD_DEADLINE_SECONDS,
            run_timeout + max(60, run_timeout // 10),
        )

    def test_teardown_launch_error_after_pass_still_finishes_records(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
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
                    "started_at": "2026-09-06T18:23:18Z",
                    "producer": pressure_schedule.run_registry.PRODUCER,
                    "admission": "ci-hub validate-lock",
                },
            )
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if len(calls) == 4:
                    raise FileNotFoundError(2, "No such file or directory", command[0])
                return subprocess.CompletedProcess(command, 0, "", "")

            with mock.patch.object(
                pressure_schedule.subprocess, "run", side_effect=run
            ):
                self.assertEqual(
                    pressure_schedule.run_campaign("a" * 40, record, root), 1
                )

            state = pressure_schedule.read_state(root)
            self.assertEqual(state["state"], "failed")
            self.assertEqual(state["exit_code"], 1)
            self.assertIn("could not launch wrkslots remove", state["cleanup_error"])
            finished = pressure_schedule.run_registry.read_record(record)
            self.assertEqual(finished["state"], "failed")
            self.assertEqual(finished["result"], "failed")
            self.assertEqual(finished["exit_code"], 1)
            self.assertEqual(finished["cleanup_error"], state["cleanup_error"])

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


class UnwrappedGitLaunchTest(unittest.TestCase):
    """launch() hands the unit a PATH whose git is git.meta.real, or PATH unchanged."""

    def launch(self, root, path, systemd_rc=0, active_rc=3, **patches):
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            if command[0] == "/bin/systemctl":
                return subprocess.CompletedProcess(command, active_rc, "", "")
            if "rev-parse" in command:
                return subprocess.CompletedProcess(command, 0, "a" * 40 + "\n", "")
            if command[0] == "/bin/systemd-run" and systemd_rc:
                return subprocess.CompletedProcess(
                    command, systemd_rc, "", "unit fixture refused\n"
                )
            return subprocess.CompletedProcess(command, 0, "", "")

        output = io.StringIO()
        with contextlib.ExitStack() as stack:
            stack.enter_context(
                mock.patch.dict(pressure_schedule.os.environ, {"PATH": path}, clear=True)
            )
            stack.enter_context(
                mock.patch.object(
                    pressure_schedule.shutil,
                    "which",
                    side_effect=lambda name: {
                        "systemctl": "/bin/systemctl",
                        "systemd-run": "/bin/systemd-run",
                        "with-proxy": "/bin/with-proxy",
                    }[name],
                )
            )
            stack.enter_context(
                mock.patch.object(
                    pressure_schedule.unwrapped_git, "REAL_GIT", patches.pop("real")
                )
            )
            stack.enter_context(patch_wrapper_config(root))
            stack.enter_context(
                mock.patch.object(
                    pressure_schedule.unwrapped_git,
                    "probe_version",
                    ORIGINAL_PROBE_VERSION,
                )
            )
            for name, value in patches.items():
                stack.enter_context(mock.patch.object(pressure_schedule, name, value))
            stack.enter_context(contextlib.redirect_stdout(output))
            rc = pressure_schedule.launch(root, run)
        launched = [command for command in calls if command[0] == "/bin/systemd-run"]
        return rc, launched, output.getvalue()

    def slot(self, root):
        record = pressure_schedule.run_registry.read_record(
            pressure_schedule.run_registry.record_path(root, pressure_schedule.UNIT)
        )
        return Path(record["checkout"]).name

    def path_values(self, command):
        return [arg for arg in command if arg.startswith("--setenv=PATH=")]

    def network_git_values(self, command):
        prefix = f"--setenv={pressure_schedule.NETWORK_GIT_ENV}="
        return [arg for arg in command if arg.startswith(prefix)]

    def recorded(self, root):
        return pressure_schedule.run_registry.read_record(
            pressure_schedule.run_registry.record_path(root, pressure_schedule.UNIT)
        )

    def test_launch_runs_unwrapped_git_when_path_git_is_the_wrapper(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            rc, launched, output = self.launch(root, f"{local}:/bin", real=real)

            self.assertEqual(0, rc, output)
            self.assertEqual(1, len(launched))
            slot = self.slot(root)
            git_directory = root / "ignored/ci-hub/pressure-test-git" / slot
            self.assertEqual(
                [f"--setenv=PATH={git_directory}:{local}:/bin"],
                self.path_values(launched[0]),
            )
            self.assertEqual(["git"], sorted(p.name for p in git_directory.iterdir()))
            self.assertEqual(str(real), pressure_schedule.os.readlink(git_directory / "git"))
            log = pressure_schedule.log_path(root).read_text()
            self.assertIn(f"pressure_schedule: slot={slot} git substitution APPLIED: ", log)
            self.assertIn(f'"effective_git": "{real}"', log)
            # The submodule fetch, the campaign's one network git call, keeps
            # PATH's original git (the wrapper, with its 429 retry).
            self.assertEqual(
                [f"--setenv={pressure_schedule.NETWORK_GIT_ENV}={local / 'git'}"],
                self.network_git_values(launched[0]),
            )
            # The run handle carries the decision, machine-readably.
            substitution = self.recorded(root)["git_substitution"]
            self.assertEqual(True, substitution["applied"])
            self.assertEqual("applied", substitution["code"])
            self.assertEqual(str(git_directory), substitution["directory"])
            self.assertEqual(str(real), substitution["effective_git"])
            self.assertEqual(str(local / "git"), substitution["resolved_git"])
            # stdout stays the health tick's two-line protocol.
            self.assertEqual(
                ["state=launched", "summary"],
                [line.split("=", 1)[0] if line.startswith("summary=") else line
                 for line in output.splitlines()],
            )

    def test_launch_leaves_path_unchanged_when_git_is_not_the_wrapper(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, plain = fake_host_gits(root)
            path = f"{plain}:{local}:/bin"
            rc, launched, output = self.launch(root, path, real=real)

            self.assertEqual(0, rc, output)
            self.assertEqual([f"--setenv=PATH={path}"], self.path_values(launched[0]))
            self.assertFalse((root / "ignored/ci-hub/pressure-test-git").exists())
            log = pressure_schedule.log_path(root).read_text()
            self.assertIn("git substitution NOT-APPLIED: ", log)
            self.assertIn('"code": "not-the-wrapper"', log)
            self.assertIn('"effective_version": "git version 2.52.0"', log)
            self.assertEqual([], self.network_git_values(launched[0]))
            substitution = self.recorded(root)["git_substitution"]
            self.assertEqual("not-the-wrapper", substitution["code"])
            self.assertIsNone(substitution["directory"])

    def test_directory_failure_launches_with_path_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            blocker = root / "not-a-directory"
            blocker.write_text("")
            path = f"{local}:/bin"
            rc, launched, output = self.launch(
                root,
                path,
                real=real,
                unwrapped_git_directory=lambda _root, slot: blocker / slot,
            )

            self.assertEqual(0, rc, output)
            self.assertEqual([f"--setenv=PATH={path}"], self.path_values(launched[0]))
            self.assertEqual([], self.network_git_values(launched[0]))
            log = pressure_schedule.log_path(root).read_text()
            self.assertIn('"code": "materialize-failed"', log)
            self.assertEqual(
                "materialize-failed", self.recorded(root)["git_substitution"]["code"]
            )

    def test_systemd_refusal_removes_the_git_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            rc, launched, output = self.launch(
                root, f"{local}:/bin", systemd_rc=1, real=real
            )

            self.assertEqual(1, rc, output)
            self.assertEqual(1, len(launched))
            self.assertIn("state=refused", output)
            self.assertNotIn("cleanup_error", output)
            slot = self.slot(root)
            self.assertFalse(
                (root / "ignored/ci-hub/pressure-test-git" / slot).exists()
            )
            record = self.recorded(root)
            self.assertEqual("refused", record["state"])
            self.assertEqual("applied", record["git_substitution"]["code"])
            self.assertNotIn("cleanup_error", record)

    def test_record_write_failure_removes_the_git_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            with mock.patch.object(
                pressure_schedule.run_registry,
                "write_current_record",
                side_effect=RuntimeError("record fixture refused"),
            ), self.assertRaisesRegex(RuntimeError, "record fixture refused"):
                self.launch(root, f"{local}:/bin", real=real)

            # The directory was created (the log says APPLIED) and removed.
            log = pressure_schedule.log_path(root).read_text()
            self.assertIn("git substitution APPLIED: ", log)
            parent = root / "ignored/ci-hub/pressure-test-git"
            self.assertEqual([], sorted(parent.iterdir()))

    def test_systemd_run_start_failure_removes_the_git_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            def launch_with_missing_systemd_run():
                calls = []

                def run(command, **kwargs):
                    calls.append(command)
                    if command[0] == "/bin/systemctl":
                        return subprocess.CompletedProcess(command, 3, "", "")
                    if "rev-parse" in command:
                        return subprocess.CompletedProcess(
                            command, 0, "a" * 40 + "\n", ""
                        )
                    if command[0] == "/bin/systemd-run":
                        raise FileNotFoundError(2, "No such file", command[0])
                    return subprocess.CompletedProcess(command, 0, "", "")

                with mock.patch.dict(
                    pressure_schedule.os.environ, {"PATH": f"{local}:/bin"}, clear=True
                ), mock.patch.object(
                    pressure_schedule.shutil,
                    "which",
                    side_effect=lambda name: f"/bin/{name}",
                ), mock.patch.object(
                    pressure_schedule.unwrapped_git, "REAL_GIT", real
                ), patch_wrapper_config(root), mock.patch.object(
                    pressure_schedule.unwrapped_git,
                    "probe_version",
                    ORIGINAL_PROBE_VERSION,
                ), contextlib.redirect_stdout(io.StringIO()):
                    pressure_schedule.launch(root, run)

            with self.assertRaises(FileNotFoundError):
                launch_with_missing_systemd_run()
            parent = root / "ignored/ci-hub/pressure-test-git"
            self.assertEqual([], sorted(parent.iterdir()))

    def test_systemd_refusal_records_a_git_directory_it_cannot_remove(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            original_materialize = pressure_schedule.unwrapped_git.materialize

            def materialize_then_intrude(planned):
                made = original_materialize(planned)
                (Path(made.directory) / "intruder").write_text("not ours\n")
                return made

            with mock.patch.object(
                pressure_schedule.unwrapped_git,
                "materialize",
                side_effect=materialize_then_intrude,
            ):
                rc, _launched, output = self.launch(
                    root, f"{local}:/bin", systemd_rc=1, real=real
                )

            self.assertEqual(1, rc, output)
            slot = self.slot(root)
            git_directory = root / "ignored/ci-hub/pressure-test-git" / slot
            self.assertEqual(
                ["git", "intruder"], sorted(p.name for p in git_directory.iterdir())
            )
            record = self.recorded(root)
            self.assertIn("left in place", record["cleanup_error"])
            self.assertIn("cleanup_error=", output)

    def test_refused_handoff_creates_no_git_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, _plain = fake_host_gits(root)
            with mock.patch.dict(
                pressure_schedule.os.environ,
                {"DEV_HERMIT_OPERATIONAL_TOOL": str(root / "missing-tool")},
            ):
                calls = []

                def run(command, **kwargs):
                    calls.append(command)
                    if command[0] == "/bin/systemctl":
                        return subprocess.CompletedProcess(command, 3, "", "")
                    if "rev-parse" in command:
                        return subprocess.CompletedProcess(
                            command, 0, "a" * 40 + "\n", ""
                        )
                    return subprocess.CompletedProcess(command, 0, "", "")

                with mock.patch.object(
                    pressure_schedule.unwrapped_git, "REAL_GIT", real
                ), patch_wrapper_config(root), mock.patch.object(
                    pressure_schedule.unwrapped_git,
                    "probe_version",
                    ORIGINAL_PROBE_VERSION,
                ), mock.patch.dict(
                    pressure_schedule.os.environ, {"PATH": f"{local}:/bin"}
                ), mock.patch.object(
                    pressure_schedule.shutil,
                    "which",
                    side_effect=lambda name: f"/bin/{name}",
                ), contextlib.redirect_stdout(io.StringIO()) as output:
                    self.assertEqual(1, pressure_schedule.launch(root, run))
            self.assertIn("state=refused", output.getvalue())
            self.assertIn("incomplete operational-tool handoff", output.getvalue())
            self.assertFalse(any(command[0] == "/bin/systemd-run" for command in calls))
            self.assertFalse((root / "ignored/ci-hub/pressure-test-git").exists())

    def leftover(self, root, real, slot):
        """A slot directory of exactly the shape launch() creates."""
        planned = pressure_schedule.unwrapped_git.plan(
            f"{real.parent}:/bin",
            pressure_schedule.unwrapped_git_directory(root, slot),
            real_git=real,
            wrapper_config=wrapper_config(root),
        )
        made = pressure_schedule.unwrapped_git.materialize(planned)
        self.assertTrue(made.applied, made.reason)
        return Path(made.directory)

    def test_launch_sweeps_leftover_git_directories(self):
        # A campaign killed at RuntimeMaxSec never reaches its finally block,
        # so its directory is still there at the next launch.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, plain = fake_host_gits(root)
            exact = self.leftover(root, real, "pressure-old-exact")
            rc, launched, output = self.launch(root, f"{plain}:{local}:/bin", real=real)

            self.assertEqual(0, rc, output)
            self.assertEqual(1, len(launched))
            self.assertFalse(exact.exists())
            self.assertNotIn("cleanup_error", output)
            log = pressure_schedule.log_path(root).read_text()
            self.assertIn(
                f"pressure_schedule: swept {root / 'ignored/ci-hub/pressure-test-git'}: "
                "removed=['pressure-old-exact'] left=[]",
                log,
            )

    def test_launch_leaves_and_reports_foreign_entries_it_cannot_sweep(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, plain = fake_host_gits(root)
            exact = self.leftover(root, real, "pressure-old-exact")
            extra = self.leftover(root, real, "pressure-old-extra")
            (extra / "intruder").write_text("not ours\n")
            other = pressure_schedule.unwrapped_git_directory(root, "pressure-old-other")
            other.mkdir()
            (other / "git").symlink_to(plain / "git")
            stray = pressure_schedule.unwrapped_git_root(root) / "stray-file"
            stray.write_text("a file, not a slot\n")

            rc, launched, output = self.launch(root, f"{plain}:{local}:/bin", real=real)

            # The test still launches; the leftovers need a person, so the
            # health tick sees a nonzero exit and the named paths.
            self.assertEqual(1, rc, output)
            self.assertEqual(1, len(launched))
            self.assertIn("state=launched", output)
            self.assertFalse(exact.exists())
            self.assertEqual(["git", "intruder"], sorted(p.name for p in extra.iterdir()))
            self.assertEqual(str(plain / "git"), pressure_schedule.os.readlink(other / "git"))
            self.assertEqual("a file, not a slot\n", stray.read_text())
            summary = next(
                line for line in output.splitlines() if line.startswith("summary=")
            )
            self.assertIn(" cleanup_error=", summary)
            for leftover in (extra, other, stray):
                self.assertIn(str(leftover), summary)
            log = pressure_schedule.log_path(root).read_text()
            self.assertIn("removed=['pressure-old-exact'] left=[", log)

    def test_a_refused_launch_still_reports_what_the_sweep_left(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, plain = fake_host_gits(root)
            extra = self.leftover(root, real, "pressure-old-extra")
            (extra / "intruder").write_text("not ours\n")
            rc, _launched, output = self.launch(
                root, f"{plain}:{local}:/bin", systemd_rc=1, real=real
            )
            self.assertEqual(1, rc, output)
            self.assertIn("state=refused", output)
            self.assertIn(f"cleanup_error=", output)
            self.assertIn(str(extra), output)

    def test_launch_sweeps_nothing_while_the_unit_is_active(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            local, real, plain = fake_host_gits(root)
            exact = self.leftover(root, real, "pressure-running")
            rc, launched, output = self.launch(
                root, f"{plain}:{local}:/bin", active_rc=0, real=real
            )
            self.assertEqual(0, rc, output)
            self.assertEqual([], launched)
            self.assertIn("state=running", output)
            self.assertEqual(["git"], sorted(p.name for p in exact.iterdir()))
            self.assertNotIn("swept", pressure_schedule.log_path(root).read_text()
                             if pressure_schedule.log_path(root).exists() else "")

    def test_sweep_uses_the_hosts_unwrapped_git(self):
        # A directory linking to what REAL_GIT named at an earlier launch is
        # not this host's shape now, so it is reported, not deleted.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            _local, real, _plain = fake_host_gits(root)
            exact = self.leftover(root, real, "pressure-old")
            with mock.patch.object(
                pressure_schedule.unwrapped_git, "REAL_GIT", root / "moved/git.meta.real"
            ):
                removed, problems = pressure_schedule.sweep_unwrapped_git_directories(root)
            self.assertEqual([], removed)
            self.assertEqual(1, len(problems), problems)
            self.assertIn(str(exact), problems[0])
            self.assertTrue(exact.exists())
            with mock.patch.object(pressure_schedule.unwrapped_git, "REAL_GIT", real):
                self.assertEqual(
                    (["pressure-old"], []),
                    pressure_schedule.sweep_unwrapped_git_directories(root),
                )
            # No parent directory at all is nothing to do.
            pressure_schedule.unwrapped_git_root(root).rmdir()
            self.assertEqual(
                ([], []), pressure_schedule.sweep_unwrapped_git_directories(root)
            )


class RunCampaignGitDirectoryTest(unittest.TestCase):
    """run_campaign removes launch()'s git directory and uses NETWORK_GIT_ENV."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.local, self.real, _plain = fake_host_gits(self.root)
        self.slot = "pressure-aaaaaaaaaaaa-20260929000000-0123abcd"
        self.git_directory = pressure_schedule.unwrapped_git_directory(
            self.root, self.slot
        )
        planned = pressure_schedule.unwrapped_git.plan(
            f"{self.local}:/bin", self.git_directory, real_git=self.real,
            wrapper_config=wrapper_config(self.root),
        )
        self.substitution = pressure_schedule.unwrapped_git.materialize(planned)
        self.assertTrue(self.substitution.applied, self.substitution.reason)
        self.record = pressure_schedule.run_registry.record_path(
            self.root, pressure_schedule.UNIT
        )
        pressure_schedule.run_registry.write_record(
            self.record,
            {
                "schema_version": 1,
                "kind": "bench",
                "state": "running",
                "unit": f"{pressure_schedule.UNIT}.service",
                "target": "a" * 40,
                "repo": "rrnewton/hermit",
                "checkout": str(self.root / "worktrees/validate" / self.slot),
                "log": str(self.root / "ignored/ci-hub/pressure-test.log"),
                "agent": "tick-pressure-test",
                "started_at": "2026-09-29T00:00:00Z",
                "producer": pressure_schedule.run_registry.PRODUCER,
                "admission": "ci-hub validate-lock",
                "git_substitution": self.substitution.record(),
            },
        )

    def campaign(self, create_rc=0):
        calls = []

        def run(command, **kwargs):
            calls.append(command)
            if "create" in command and command[0].endswith("ci-hub/bin/wrkslots"):
                return subprocess.CompletedProcess(command, create_rc, "", "no slot\n")
            return subprocess.CompletedProcess(command, 0, "", "")

        with mock.patch.dict(
            pressure_schedule.os.environ,
            {pressure_schedule.NETWORK_GIT_ENV: str(self.local / "git")},
        ), mock.patch.object(
            pressure_schedule.subprocess, "run", side_effect=run
        ), contextlib.redirect_stdout(io.StringIO()):
            if create_rc:
                with self.assertRaises(RuntimeError):
                    pressure_schedule.run_campaign("a" * 40, self.record, self.root)
                rc = None
            else:
                rc = pressure_schedule.run_campaign("a" * 40, self.record, self.root)
        return rc, calls

    def test_campaign_removes_the_git_directory(self):
        rc, calls = self.campaign()

        self.assertEqual(0, rc)
        self.assertFalse(self.git_directory.exists())
        submodule = next(command for command in calls if "submodule" in command)
        self.assertEqual(str(self.local / "git"), submodule[0])
        state = pressure_schedule.read_state(self.root)
        self.assertEqual("passed", state["state"])
        self.assertNotIn("cleanup_error", state)
        finished = pressure_schedule.run_registry.read_record(self.record)
        self.assertEqual("completed", finished["state"])
        self.assertIsNone(finished.get("cleanup_error"))

    def test_campaign_leaves_and_reports_a_foreign_git_directory(self):
        (self.git_directory / "intruder").write_text("not ours\n")
        rc, _calls = self.campaign()

        self.assertEqual(1, rc)
        self.assertEqual(
            ["git", "intruder"],
            sorted(p.name for p in self.git_directory.iterdir()),
        )
        state = pressure_schedule.read_state(self.root)
        self.assertEqual("failed", state["state"])
        self.assertIn("left in place", state["cleanup_error"])
        finished = pressure_schedule.run_registry.read_record(self.record)
        self.assertEqual(state["cleanup_error"], finished["cleanup_error"])

    def test_failed_slot_creation_removes_the_git_directory(self):
        _rc, calls = self.campaign(create_rc=1)

        self.assertFalse(any("submodule" in command for command in calls))
        self.assertFalse(self.git_directory.exists())
        state = pressure_schedule.read_state(self.root)
        self.assertEqual("failed", state["state"])
        self.assertNotIn("cleanup_error", state)

    def test_an_existing_checkout_refusal_removes_the_git_directory(self):
        checkout = self.root / "worktrees/validate" / self.slot
        checkout.mkdir(parents=True)
        (checkout / "keep").write_text("someone else's checkout\n")
        with mock.patch.object(pressure_schedule.subprocess, "run") as run, \
                self.assertRaises(RuntimeError) as raised:
            pressure_schedule.run_campaign("a" * 40, self.record, self.root)
        self.assertEqual(
            f"refusing existing pressure-test checkout {checkout}", str(raised.exception)
        )
        run.assert_not_called()
        self.assertFalse(self.git_directory.exists())
        self.assertEqual("someone else's checkout\n", (checkout / "keep").read_text())

    def test_an_existing_checkout_refusal_reports_a_foreign_git_directory(self):
        (self.root / "worktrees/validate" / self.slot).mkdir(parents=True)
        (self.git_directory / "intruder").write_text("not ours\n")
        with self.assertRaisesRegex(RuntimeError, "cleanup_error=.*left in place"):
            pressure_schedule.run_campaign("a" * 40, self.record, self.root)
        self.assertEqual(
            ["git", "intruder"], sorted(p.name for p in self.git_directory.iterdir())
        )

    def without_checkout(self, **changes):
        record = pressure_schedule.run_registry.read_record(self.record)
        record.pop("checkout")
        record.update(changes)
        return mock.patch.object(
            pressure_schedule.run_registry, "read_record", return_value=record
        )

    def test_a_record_without_checkout_removes_the_recorded_git_directory(self):
        with self.without_checkout(), \
                mock.patch.object(pressure_schedule.subprocess, "run") as run, \
                self.assertRaises(RuntimeError) as raised:
            pressure_schedule.run_campaign("a" * 40, self.record, self.root)
        self.assertIn("lacks its wrkslots checkout", str(raised.exception))
        self.assertNotIn("cleanup_error", str(raised.exception))
        run.assert_not_called()
        self.assertFalse(self.git_directory.exists())

    def test_a_record_without_checkout_leaves_a_directory_outside_the_root(self):
        outside = self.root / "elsewhere" / self.slot
        outside.mkdir(parents=True)
        (outside / "git").symlink_to(self.real)
        substitution = {**self.substitution.record(), "directory": str(outside)}
        with self.without_checkout(git_substitution=substitution), \
                self.assertRaisesRegex(RuntimeError, "cleanup_error=.*left in place"):
            pressure_schedule.run_campaign("a" * 40, self.record, self.root)
        self.assertTrue((outside / "git").is_symlink())
        self.assertTrue(self.git_directory.exists())

    def test_network_git_is_the_recorded_absolute_path_or_plain_git(self):
        env = pressure_schedule.NETWORK_GIT_ENV
        self.assertEqual("/usr/local/bin/git", pressure_schedule.network_git(
            {env: "/usr/local/bin/git"}
        ))
        self.assertEqual("git", pressure_schedule.network_git({}))
        self.assertEqual("git", pressure_schedule.network_git({env: ""}))
        self.assertEqual("git", pressure_schedule.network_git({env: "bin/git"}))


if __name__ == "__main__":
    unittest.main()
