#!/usr/bin/env python3
"""The wrkslots launcher keeps removal evidence in one host process view."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "ci-hub/bin/wrkslots"
LIFECYCLE_PATH = ROOT / "hermit/agent-utils/py/wrkslots/tests/test_lifecycle.py"
LIFECYCLE_SPEC = importlib.util.spec_from_file_location(
    "wrkslots_launcher_lifecycle", LIFECYCLE_PATH
)
assert LIFECYCLE_SPEC and LIFECYCLE_SPEC.loader
lifecycle = importlib.util.module_from_spec(LIFECYCLE_SPEC)
sys.modules[LIFECYCLE_SPEC.name] = lifecycle
LIFECYCLE_SPEC.loader.exec_module(lifecycle)


class WrkslotsLauncherTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "tool"
        (self.root / "ci-hub/bin").mkdir(parents=True)
        (self.root / "ci-hub/health").mkdir(parents=True)
        package = self.root / "hermit/agent-utils/py/wrkslots"
        package.mkdir(parents=True)
        shutil.copy2(LAUNCHER, self.root / "ci-hub/bin/wrkslots")
        shutil.copy2(
            ROOT / "ci-hub/health/removal_handoff_writer.py",
            self.root / "ci-hub/health/removal_handoff_writer.py",
        )

        (self.root / "ci-hub/health/host_process_context.py").write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env python3
                import os
                import sys
                print("fixture process view", file=sys.stderr)
                raise SystemExit(int(os.environ.get("FIXTURE_PROCESS_VIEW_RC", "0")))
                """
            ),
            encoding="utf-8",
        )
        (package / "__init__.py").write_text("", encoding="utf-8")
        (package / "__main__.py").write_text(
            textwrap.dedent(
                """\
                import json
                import os
                from pathlib import Path
                import sys

                def ancestry():
                    result = []
                    current = os.getpid()
                    while current > 0 and current not in result:
                        result.append(current)
                        try:
                            for line in Path(f"/proc/{current}/status").read_text().splitlines():
                                if line.startswith("PPid:"):
                                    current = int(line.split()[1])
                                    break
                            else:
                                break
                        except OSError:
                            break
                    return result

                Path(os.environ["INNER_RECORD"]).write_text(json.dumps({
                    "argv": sys.argv[1:],
                    "pid": os.getpid(),
                    "runner_pid": os.environ.get("WRKSLOTS_REMOVE_RUNNER_PID"),
                    "coordinator_start_ticks": os.environ.get(
                        "WRKSLOTS_REMOVE_COORDINATOR_START_TICKS"
                    ),
                    "proof_fd": os.environ.get("WRKSLOTS_REMOVE_PROOF_FD"),
                    "ancestry": ancestry(),
                }))
                sys.stdout.write(os.environ.get("FIXTURE_INNER_STDOUT", ""))
                raise SystemExit(int(os.environ.get("FIXTURE_INNER_RC", "0")))
                """
            ),
            encoding="utf-8",
        )

        self.bin = self.root / "fixture-bin"
        self.bin.mkdir()
        systemd_run = self.bin / "systemd-run"
        systemd_run.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ ${FIXTURE_SYSTEMD_FAIL:-0} == 1 ]]; then
                  exit 19
                fi
                : >"$SYSTEMD_RECORD"
                for argument in "$@"; do
                  case "$argument" in
                    --property=*) printf '%s\n' "$argument" >>"$SYSTEMD_RECORD" ;;
                  esac
                done
                while (($#)); do
                  case "$1" in
                    --setenv=WRKSLOTS_ORIGINAL_COORDINATOR_PID=*)
                      export "WRKSLOTS_ORIGINAL_COORDINATOR_PID=${FIXTURE_COORDINATOR_PID:-${1#*=}}"
                      ;;
                    --setenv=WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS=*)
                      if [[ ${FIXTURE_CORRUPT_START_TICKS:-0} == 1 ]]; then
                        export WRKSLOTS_ORIGINAL_COORDINATOR_START_TICKS=0
                      else
                        export "${1#--setenv=}"
                      fi
                      ;;
                    --setenv=*) export "${1#--setenv=}" ;;
                    --working-directory=*) cd -- "${1#--working-directory=}" ;;
                    --) shift; break ;;
                  esac
                  shift
                done
                exec "$@"
                """
            ),
            encoding="utf-8",
        )
        systemd_run.chmod(0o755)
        self.record = Path(self.temporary.name) / "inner.json"
        self.systemd_record = Path(self.temporary.name) / "systemd-argv"
        self.environment = os.environ | {
            "PATH": f"{self.bin}:{Path(sys.executable).parent}:/usr/bin:/bin",
            "INNER_RECORD": str(self.record),
            "SYSTEMD_RECORD": str(self.systemd_record),
            "FIXTURE_COORDINATOR_PID": str(os.getpid()),
        }

    def run_launcher(
        self, *arguments: str, **environment: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.root / "ci-hub/bin/wrkslots"), *arguments],
            cwd=self.root,
            env=self.environment | environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def removal_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "remove",
            "slot01",
            "--coordinator-pid",
            str(os.getpid()),
            "--expected-generation",
            "7",
        )

    def retirement_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "--allow-existing-unregistered-worktrees",
            "--wait-lock",
            "10",
            "retire-pending",
            "--limit",
            "8",
            "--coordinator-pid",
            str(os.getpid()),
            "--coordinator-authorized",
            "--format",
            "json",
        )

    def frozen_recovery_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "recover",
            "--coordinator-authorized",
            "--coordinator-pid",
            str(os.getpid()),
            "--frozen-validate-checkout",
            str(self.root.parent / ".tool-frozen-validate/validate-fresh-one"),
            "--completed-record",
            "ignored/validate/runs/validate-one.json",
            "--repository",
            "hermit",
        )

    def frozen_batch_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "recover-ownerless-validate-batch",
            "--coordinator-authorized",
            "--coordinator-pid",
            str(os.getpid()),
            "--frozen-validate-checkout",
            str(self.root.parent / ".tool-frozen-validate/validate-fresh-one"),
            "--completed-record",
            "ignored/validate/runs/validate-one.json",
            "--repository",
            "hermit",
        )

    def bare_recovery_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "recover",
            "--coordinator-pid",
            str(os.getpid()),
        )

    def use_real_wrkslots_package(self) -> None:
        package = self.root / "hermit/agent-utils/py/wrkslots"
        shutil.rmtree(package)
        shutil.copytree(ROOT / "hermit/agent-utils/py/wrkslots", package)

    def test_removal_reenters_then_preserves_slot_and_generation(self) -> None:
        completed = self.run_launcher(*self.removal_arguments())

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("slot01", record["argv"])
        self.assertEqual(
            "7", record["argv"][record["argv"].index("--expected-generation") + 1]
        )
        coordinator = record["argv"][record["argv"].index("--coordinator-pid") + 1]
        self.assertEqual(str(os.getpid()), coordinator)
        self.assertIsNotNone(record["runner_pid"])
        self.assertIsNotNone(record["coordinator_start_ticks"])
        self.assertEqual("0", record["proof_fd"])
        self.assertIn(int(record["runner_pid"]), record["ancestry"])
        systemd_arguments = self.systemd_record.read_text().splitlines()
        self.assertIn("--property=PrivateUsers=no", systemd_arguments)
        self.assertIn("--property=PrivateMounts=no", systemd_arguments)
        self.assertIn("--property=ProtectProc=default", systemd_arguments)
        self.assertIn("--property=ProcSubset=all", systemd_arguments)

    def test_restricted_reentry_refuses_without_running_inner_command(self) -> None:
        completed = self.run_launcher(
            *self.removal_arguments(), FIXTURE_PROCESS_VIEW_RC="2"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("does not have a verified full process view", completed.stderr)
        self.assertFalse(self.record.exists())

    def test_inner_refusal_is_preserved_without_fallback(self) -> None:
        completed = self.run_launcher(*self.removal_arguments(), FIXTURE_INNER_RC="3")

        self.assertEqual(3, completed.returncode)
        self.assertTrue(self.record.exists())

    def test_unexpected_inner_failure_does_not_claim_nothing_changed(self) -> None:
        completed = self.run_launcher(*self.removal_arguments(), FIXTURE_INNER_RC="1")

        self.assertEqual(3, completed.returncode)
        self.assertIn("state: UNKNOWN", completed.stderr)
        self.assertNotIn(
            "no worktree or lifecycle record was changed", completed.stderr
        )
        self.assertTrue(self.record.exists())

    def test_validate_batch_uses_the_same_host_handoff(self) -> None:
        completed = self.run_launcher(
            "--project-root",
            str(self.root),
            "remove-validate-batch",
            "--slot",
            "validate01=11",
            "--coordinator-pid",
            str(os.getpid()),
        )

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("remove-validate-batch", record["argv"])
        self.assertEqual(
            "validate01=11", record["argv"][record["argv"].index("--slot") + 1]
        )
        coordinator = record["argv"][record["argv"].index("--coordinator-pid") + 1]
        self.assertEqual(str(os.getpid()), coordinator)

    def validate_batch_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "remove-validate-batch",
            "--slot",
            "validate01=11",
            "--coordinator-pid",
            str(os.getpid()),
            "--format",
            "json",
        )

    def test_validate_batch_exit_one_passes_its_report_through(self) -> None:
        report = '{"retained": [{"slot": "validate01"}]}\n'
        completed = self.run_launcher(
            *self.validate_batch_arguments(),
            FIXTURE_INNER_RC="1",
            FIXTURE_INNER_STDOUT=report,
        )

        self.assertEqual(1, completed.returncode, completed.stderr)
        self.assertEqual(report, completed.stdout)
        self.assertIn(
            "remove-validate-batch exited 1: it retained at least one slot",
            completed.stderr,
        )
        self.assertNotIn("state: UNKNOWN", completed.stderr)
        self.assertTrue(self.record.exists())

    def test_validate_batch_other_inner_failure_is_still_unknown(self) -> None:
        completed = self.run_launcher(
            *self.validate_batch_arguments(), FIXTURE_INNER_RC="4"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("state: UNKNOWN", completed.stderr)
        self.assertIn("failed with exit 4", completed.stderr)

    def agent_batch_arguments(self) -> tuple[str, ...]:
        return (
            "--project-root",
            str(self.root),
            "remove-agent-batch",
            "--slot",
            "slot01=7",
            "--slot",
            "slot02=9",
            "--coordinator-pid",
            str(os.getpid()),
            "--format",
            "json",
        )

    def test_agent_batch_uses_the_same_host_handoff(self) -> None:
        completed = self.run_launcher(*self.agent_batch_arguments())

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("remove-agent-batch", record["argv"])
        slots = [
            record["argv"][position + 1]
            for position, argument in enumerate(record["argv"])
            if argument == "--slot"
        ]
        self.assertEqual(["slot01=7", "slot02=9"], slots)
        coordinator = record["argv"][record["argv"].index("--coordinator-pid") + 1]
        self.assertEqual(str(os.getpid()), coordinator)
        self.assertIsNotNone(record["runner_pid"])
        self.assertIsNotNone(record["coordinator_start_ticks"])
        self.assertEqual("0", record["proof_fd"])
        self.assertIn(int(record["runner_pid"]), record["ancestry"])
        properties = self.systemd_record.read_text().splitlines()
        self.assertIn("--property=PrivateUsers=no", properties)
        self.assertIn("--property=PrivateMounts=no", properties)
        self.assertIn("--property=ProtectProc=default", properties)
        self.assertIn("--property=ProcSubset=all", properties)

    def test_agent_batch_failed_host_handoff_never_falls_back(self) -> None:
        completed = self.run_launcher(
            *self.agent_batch_arguments(), FIXTURE_SYSTEMD_FAIL="1"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("handoff or command failed with exit 19", completed.stderr)
        self.assertIn("state: UNKNOWN", completed.stderr)
        self.assertFalse(self.record.exists())

    def test_agent_batch_refuses_restricted_reentry(self) -> None:
        completed = self.run_launcher(
            *self.agent_batch_arguments(), FIXTURE_PROCESS_VIEW_RC="2"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("does not have a verified full process view", completed.stderr)
        self.assertFalse(self.record.exists())

    def test_agent_batch_exit_one_passes_its_report_through(self) -> None:
        report = (
            '{"removed": [{"slot": "slot01"}], '
            '"refused": [{"slot": "slot02"}], "error": null}\n'
        )
        completed = self.run_launcher(
            *self.agent_batch_arguments(),
            FIXTURE_INNER_RC="1",
            FIXTURE_INNER_STDOUT=report,
        )

        self.assertEqual(1, completed.returncode, completed.stderr)
        self.assertEqual(report, completed.stdout)
        self.assertIn(
            "remove-agent-batch exited 1: it refused at least one slot",
            completed.stderr,
        )
        self.assertIn("read 'wrkslots status'", completed.stderr)
        self.assertNotIn("state: UNKNOWN", completed.stderr)
        self.assertNotIn("retained at least one slot", completed.stderr)
        self.assertTrue(self.record.exists())

    def test_agent_batch_other_inner_failure_is_still_unknown(self) -> None:
        for status in ("4", "125"):
            with self.subTest(status=status):
                completed = self.run_launcher(
                    *self.agent_batch_arguments(), FIXTURE_INNER_RC=status
                )

                self.assertEqual(3, completed.returncode)
                self.assertIn("state: UNKNOWN", completed.stderr)
                self.assertIn(f"failed with exit {status}", completed.stderr)
                self.assertNotIn("exited 1:", completed.stderr)

    def test_agent_batch_inner_refusal_is_preserved(self) -> None:
        completed = self.run_launcher(
            *self.agent_batch_arguments(), FIXTURE_INNER_RC="3"
        )

        self.assertEqual(3, completed.returncode)
        self.assertNotIn("exited 1:", completed.stderr)
        self.assertTrue(self.record.exists())
        self.assertTrue(self.systemd_record.exists())

    def test_exit_one_passes_through_only_for_the_removal_batches(self) -> None:
        for arguments in (
            self.removal_arguments(),
            self.retirement_arguments(),
            self.frozen_batch_arguments(),
        ):
            with self.subTest(command=arguments[2]):
                completed = self.run_launcher(*arguments, FIXTURE_INNER_RC="1")

                self.assertEqual(3, completed.returncode)
                self.assertIn("state: UNKNOWN", completed.stderr)
                self.assertNotIn("exited 1: it retained", completed.stderr)
                self.assertNotIn("exited 1: it refused", completed.stderr)

    def test_retire_pending_uses_the_same_host_handoff(self) -> None:
        completed = self.run_launcher(*self.retirement_arguments())

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("retire-pending", record["argv"])
        self.assertEqual("10", record["argv"][record["argv"].index("--wait-lock") + 1])
        self.assertEqual("8", record["argv"][record["argv"].index("--limit") + 1])
        coordinator = record["argv"][record["argv"].index("--coordinator-pid") + 1]
        self.assertEqual(str(os.getpid()), coordinator)
        self.assertIn("--coordinator-authorized", record["argv"])
        self.assertEqual("json", record["argv"][record["argv"].index("--format") + 1])
        self.assertTrue(self.systemd_record.exists())

    def test_frozen_recovery_forms_use_the_same_host_handoff(self) -> None:
        for arguments, command_name in (
            (self.frozen_recovery_arguments(), "recover"),
            (self.frozen_batch_arguments(), "recover-ownerless-validate-batch"),
        ):
            with self.subTest(command=command_name):
                self.record.unlink(missing_ok=True)
                self.systemd_record.unlink(missing_ok=True)
                completed = self.run_launcher(*arguments)

                self.assertEqual(0, completed.returncode, completed.stderr)
                record = json.loads(self.record.read_text())
                self.assertIn(command_name, record["argv"])
                self.assertIn("--frozen-validate-checkout", record["argv"])
                coordinator = record["argv"][
                    record["argv"].index("--coordinator-pid") + 1
                ]
                self.assertEqual(str(os.getpid()), coordinator)
                properties = self.systemd_record.read_text().splitlines()
                self.assertIn("--property=ProtectProc=default", properties)
                self.assertIn("--property=ProcSubset=all", properties)

    def test_abbreviated_frozen_recovery_option_cannot_bypass_host_handoff(
        self,
    ) -> None:
        arguments = list(self.frozen_recovery_arguments())
        option = arguments.index("--frozen-validate-checkout")
        arguments[option] = "--frozen-v"

        completed = self.run_launcher(*arguments)

        self.assertEqual(0, completed.returncode, completed.stderr)
        self.assertTrue(self.record.exists())
        self.assertTrue(self.systemd_record.exists())

    def test_bare_recovery_uses_host_handoff(self) -> None:
        completed = self.run_launcher(*self.bare_recovery_arguments())

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("recover", record["argv"])
        self.assertNotIn("--frozen-validate-checkout", record["argv"])
        self.assertTrue(self.systemd_record.exists())

    def test_bare_recovery_refuses_restricted_reentry(self) -> None:
        completed = self.run_launcher(
            *self.bare_recovery_arguments(), FIXTURE_PROCESS_VIEW_RC="2"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("does not have a verified full process view", completed.stderr)
        self.assertFalse(self.record.exists())

    def test_bare_recovery_never_falls_back_after_inner_refusal(self) -> None:
        completed = self.run_launcher(
            *self.bare_recovery_arguments(), FIXTURE_INNER_RC="3"
        )

        self.assertEqual(3, completed.returncode)
        self.assertTrue(self.record.exists())
        self.assertTrue(self.systemd_record.exists())

    def test_frozen_recovery_forms_refuse_restricted_reentry(self) -> None:
        for arguments in (
            self.frozen_recovery_arguments(),
            self.frozen_batch_arguments(),
        ):
            with self.subTest(command=arguments[2]):
                self.record.unlink(missing_ok=True)
                completed = self.run_launcher(*arguments, FIXTURE_PROCESS_VIEW_RC="2")

                self.assertEqual(3, completed.returncode)
                self.assertIn(
                    "does not have a verified full process view", completed.stderr
                )
                self.assertFalse(self.record.exists())

    def test_frozen_recovery_forms_never_fall_back_after_inner_refusal(self) -> None:
        for arguments in (
            self.frozen_recovery_arguments(),
            self.frozen_batch_arguments(),
        ):
            with self.subTest(command=arguments[2]):
                self.record.unlink(missing_ok=True)
                self.systemd_record.unlink(missing_ok=True)
                completed = self.run_launcher(*arguments, FIXTURE_INNER_RC="3")

                self.assertEqual(3, completed.returncode)
                self.assertTrue(self.record.exists())
                self.assertTrue(self.systemd_record.exists())

    def test_coordinator_pid_abbreviations_are_rewritten(self) -> None:
        for arguments, option in (
            (
                (
                    "--project-root",
                    str(self.root),
                    "remove",
                    "slot01",
                    "--coordinator-p",
                    str(os.getpid()),
                    "--expected-generation",
                    "7",
                ),
                "--coordinator-p",
            ),
            (
                (
                    "--project-root",
                    str(self.root),
                    "remove",
                    "slot01",
                    f"--coordinator-pi={os.getpid()}",
                    "--expected-generation",
                    "7",
                ),
                "--coordinator-pi",
            ),
        ):
            with self.subTest(arguments=arguments):
                self.record.unlink(missing_ok=True)
                completed = self.run_launcher(*arguments)

                self.assertEqual(0, completed.returncode, completed.stderr)
                record = json.loads(self.record.read_text())
                if option in record["argv"]:
                    coordinator = record["argv"][record["argv"].index(option) + 1]
                else:
                    coordinator = next(
                        item.split("=", 1)[1]
                        for item in record["argv"]
                        if item.startswith(option + "=")
                    )
                self.assertEqual(str(os.getpid()), coordinator)

    def test_argparse_global_abbreviation_cannot_bypass_handoff(self) -> None:
        arguments = self.removal_arguments()
        completed = self.run_launcher(*arguments[:2], "--wait-l", "0", *arguments[2:])

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("remove", record["argv"])

    def test_end_of_global_options_cannot_bypass_handoff(self) -> None:
        arguments = self.removal_arguments()
        completed = self.run_launcher(
            "--project-root", str(self.root), "--", *arguments[2:]
        )

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertIn("remove", record["argv"])

    def test_failed_host_handoff_never_falls_back(self) -> None:
        completed = self.run_launcher(
            *self.removal_arguments(), FIXTURE_SYSTEMD_FAIL="1"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("handoff or command failed with exit 19", completed.stderr)
        self.assertIn("state: UNKNOWN", completed.stderr)
        self.assertNotIn(
            "no worktree or lifecycle record was changed", completed.stderr
        )
        self.assertFalse(self.record.exists())

    def test_changed_original_coordinator_generation_refuses(self) -> None:
        completed = self.run_launcher(
            *self.removal_arguments(), FIXTURE_CORRUPT_START_TICKS="1"
        )

        self.assertEqual(3, completed.returncode)
        self.assertIn("coordinator process generation changed", completed.stderr)
        self.assertFalse(self.record.exists())

    def test_nonancestor_coordinator_is_refused_before_handoff(self) -> None:
        arguments = list(self.removal_arguments())
        arguments[arguments.index("--coordinator-pid") + 1] = "999999999"

        completed = self.run_launcher(*arguments)

        self.assertEqual(3, completed.returncode)
        self.assertIn(
            "cannot read the coordinator process generation", completed.stderr
        )
        self.assertFalse(self.record.exists())

    def test_read_only_command_does_not_use_host_handoff(self) -> None:
        completed = self.run_launcher(
            "--project-root", str(self.root), "status", FIXTURE_SYSTEMD_FAIL="1"
        )

        self.assertEqual(0, completed.returncode, completed.stderr)
        record = json.loads(self.record.read_text())
        self.assertEqual(["--project-root", str(self.root), "status"], record["argv"])

    def test_help_and_global_early_exits_do_not_require_host_handoff(self) -> None:
        removal = self.removal_arguments()[2:]
        cases = (
            ("--project-root", str(self.root), "remove", "--help"),
            ("--project-root", str(self.root), "remove", "-h"),
            ("--project-root", str(self.root), "remove", "--he"),
            ("--project-root", str(self.root), "--userguide", *removal),
            ("--project-root", str(self.root), "--version", "remove"),
        )
        for arguments in cases:
            with self.subTest(arguments=arguments):
                self.record.unlink(missing_ok=True)
                self.systemd_record.unlink(missing_ok=True)
                completed = self.run_launcher(
                    *arguments,
                    FIXTURE_SYSTEMD_FAIL="1",
                )

                self.assertEqual(0, completed.returncode, completed.stderr)
                self.assertFalse(self.systemd_record.exists())
                self.assertTrue(self.record.exists())

    def test_real_removal_refuses_live_then_removes_dead_unheld_slot(self) -> None:
        self.use_real_wrkslots_package()
        lifecycle_root = Path(self.temporary.name) / "lifecycle"
        lifecycle_root.mkdir()
        project, _repository, _remote = lifecycle.make_project(lifecycle_root)
        made = lifecycle.create(
            project,
            slot_type="validate",
            branch=None,
        )
        self.assertEqual(0, made.returncode, made.stderr)
        slot = lifecycle.checkout(project, slot_type="validate").parent

        live = self.run_launcher(
            "--project-root",
            str(project),
            "remove",
            "slot01",
            "--coordinator-pid",
            str(os.getpid()),
            "--expected-generation",
            "1",
        )

        self.assertEqual(3, live.returncode)
        self.assertIn("owner alive", live.stderr)
        self.assertTrue(slot.is_dir())
        self.assertEqual(1, len(lifecycle.active_slots(project)))

        lifecycle.mark_owner_dead(project)
        lifecycle.set_liveness(project, "dead")
        holder = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            cwd=slot,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(lifecycle.terminate_process, holder)
        held = self.run_launcher(
            "--project-root",
            str(project),
            "remove",
            "slot01",
            "--coordinator-pid",
            str(os.getpid()),
            "--expected-generation",
            "1",
        )

        self.assertEqual(3, held.returncode)
        self.assertIn("uses slot", held.stderr)
        self.assertTrue(slot.is_dir())
        self.assertEqual(1, len(lifecycle.active_slots(project)))
        lifecycle.terminate_process(holder)

        dead = self.run_launcher(
            "--project-root",
            str(project),
            "remove",
            "slot01",
            "--coordinator-pid",
            str(os.getpid()),
            "--expected-generation",
            "1",
        )

        self.assertEqual(0, dead.returncode, dead.stderr)
        self.assertFalse(slot.exists())
        self.assertEqual([], lifecycle.active_slots(project))

    def test_real_agent_batch_runs_every_item_in_the_host_handoff(self) -> None:
        if not hasattr(lifecycle, "dead_agent_batch_project"):
            self.skipTest(
                "the pinned agent-utils has no remove-agent-batch; this test runs once "
                "the pin includes it"
            )
        self.use_real_wrkslots_package()
        lifecycle_root = Path(self.temporary.name) / "lifecycle"
        lifecycle_root.mkdir()
        project, _repository, slots = lifecycle.dead_agent_batch_project(
            lifecycle_root, count=3
        )

        def batch(*names: str) -> subprocess.CompletedProcess[str]:
            return self.run_launcher(
                "--project-root",
                str(project),
                "remove-agent-batch",
                "--coordinator-pid",
                str(os.getpid()),
                *(f"--slot={name}=1" for name in names),
                "--format",
                "json",
            )

        def remove_leftovers() -> None:
            # A slot left active may hold a mounted slot image, which the
            # temporary directory cannot delete. Remove it even when an
            # assertion failed first.
            leftovers = [row["slot"] for row in lifecycle.active_slots(project)]
            if leftovers:
                batch(*leftovers)

        self.addCleanup(remove_leftovers)
        held = lifecycle.checkout(project, "slot02")
        holder = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            cwd=held,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(lifecycle.terminate_process, holder)
        try:
            completed = batch(*slots)
        finally:
            lifecycle.terminate_process(holder)

        # Exit 1 is a partial batch: the launcher passes the report through.
        self.assertEqual(1, completed.returncode, completed.stderr)
        self.assertIn(
            "remove-agent-batch exited 1: it refused at least one slot",
            completed.stderr,
        )
        self.assertTrue(self.systemd_record.exists())
        payload = json.loads(completed.stdout)
        self.assertEqual(
            ["slot01", "slot03"], [row["slot"] for row in payload["removed"]]
        )
        self.assertEqual(["slot02"], [row["slot"] for row in payload["refused"]])
        self.assertIn(
            f"live process {holder.pid} uses slot", payload["refused"][0]["reason"]
        )
        self.assertIsNone(payload["error"])
        self.assertFalse(payload["recovery_required"])
        self.assertTrue(held.is_dir())
        self.assertFalse(lifecycle.checkout(project, "slot01").exists())
        self.assertFalse(lifecycle.checkout(project, "slot03").exists())
        self.assertEqual(
            ["slot02"], [row["slot"] for row in lifecycle.active_slots(project)]
        )

        # With its holder gone, a second batch removes the refused slot.
        retried = batch("slot02")

        self.assertEqual(0, retried.returncode, retried.stderr)
        self.assertNotIn("exited 1:", retried.stderr)
        self.assertEqual(
            ["slot02"], [row["slot"] for row in json.loads(retried.stdout)["removed"]]
        )
        self.assertFalse(held.exists())
        self.assertEqual([], lifecycle.active_slots(project))


if __name__ == "__main__":
    unittest.main()
