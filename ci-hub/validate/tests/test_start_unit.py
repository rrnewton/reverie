from __future__ import annotations

import contextlib
import io
import json
import shutil
import subprocess
import sys
import tempfile
import pathlib
import unittest
from unittest import mock
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import start_unit  # noqa: E402
import qualifying_receipt  # noqa: E402


SHA = "a" * 40


def completed(command: list[str], rc: int = 0, stdout: str = "", stderr: str = ""):
    return subprocess.CompletedProcess(command, rc, stdout, stderr)


def wrkslots_action(command: list[str], action: str) -> bool:
    return command[0].endswith("ci-hub/bin/wrkslots") and action in command


class FakeRun:
    def __init__(self, checkout: Path) -> None:
        self.checkout = checkout
        self.commands: list[list[str]] = []
        self.dirty = ""
        self.admission_rc = 0
        self.herdr_status_rc = 0
        # Fresh-checkout controls: `fresh` is what `mktemp -d` hands back,
        # `fresh_complete` decides whether the tree carries what validate needs,
        # and `ledger_rows` is what the canonical reader returns.
        self.fresh: Path | None = None
        self.fresh_complete = True
        self.fresh_runner_name = "safe-ci-dag-runner"
        self.fresh_runner_executable = True
        self.ledger_rows: list[str] | None = None
        self.canonical_verdict = "VALIDATED"
        self.canonical_status_rc = 0
        self.canonical_selected_row_index = -1
        self.removed: list[str] = []
        # What `git ls-files` reports as TRACKED inside the fresh checkout.
        # Empty is the real-world case: hermit tracks zero .jsonl files.
        self.fresh_tracked_jsonl = ""
        # Relative path of a receipt the RUN writes inside its own temp
        # checkout. Planted when the fresh tree is created, because that is the
        # first moment the directory exists.
        self.plant_receipt: str | None = None
        self.actual_exit = 0
        self.final_validate_status: str | None = "PASSED"
        self.executed_nodes: int | None = 55
        self.executed_tests: int | None = 862
        self.collected_unit = False
        self.current_main = "b" * 40
        self.target_contains_current_main = False
        self.source_common_dir_available = True
        self.source_head = SHA
        self.scorecard_update = False
        self.scorecard_rc = 0
        self.scorecard_stderr = ""

    def default_ledger_row(self) -> dict[str, object]:
        return {
            "commit": SHA,
            "cwd": str(self.fresh if self.fresh is not None else self.checkout),
            "started_at": "2099-01-01T00:00:01Z",
            "finished_at": "2099-01-01T00:00:02Z",
            "tree": "b" * 40,
            "host": "test-host",
            "slot": "test-slot",
            "log_file": "/tmp/test-validate.log",
        }

    @staticmethod
    def selected_receipt(row: dict[str, object]) -> dict[str, object]:
        return {
            "receipt_identity": {
                "tuple": {
                    "sha": row.get("commit"),
                    "tree": row.get("tree"),
                    "finished_at": row.get("finished_at"),
                    "host": row.get("host"),
                    "slot": row.get("slot"),
                    "log_file": row.get("log_file"),
                }
            }
        }

    def __call__(self, command: list[str], **_kwargs: object):
        self.commands.append(command)
        if command[0].endswith("ci-hub/bin/wrkslots") and "create" in command:
            slot = command[command.index("create") + 1]
            project_root = Path(command[command.index("--project-root") + 1])
            self.fresh = project_root / "worktrees" / "validate" / slot
            self._materialize_fresh_checkout()
            return completed(command, stdout=f"created slot={slot}\n")
        if command[0].endswith("ci-hub/bin/wrkslots") and "remove" in command:
            if self.fresh is not None:
                self.removed.append(str(self.fresh))
                shutil.rmtree(self.fresh, ignore_errors=True)
            return completed(command, stdout="removed and archived validation slot\n")
        if (
            command[0].endswith("ci-hub/bin/wrkslots")
            and "recover" in command
            and "--legacy-validate-checkout" in command
        ):
            project_root = Path(command[command.index("--project-root") + 1])
            relative = Path(
                command[command.index("--legacy-validate-checkout") + 1]
            )
            fresh = project_root / relative
            self.removed.append(str(fresh))
            shutil.rmtree(fresh, ignore_errors=True)
            return completed(
                command,
                stdout="recovered completed legacy validation checkout\n",
            )
        if command[:2] == ["mktemp", "-d"] and "validate-cargo-" in command[2]:
            private = Path(command[2].replace("XXXXXXXX", "abcd1234"))
            private.mkdir(parents=True, exist_ok=True)
            return completed(command, stdout=f"{private}\n")
        if command[:2] == ["mktemp", "-d"]:
            self.fresh = Path(command[2].replace("XXXXXXXX", "abcd1234"))
            self._materialize_fresh_checkout()
            return completed(command, stdout=f"{self.fresh}\n")
        return self._run_after_checkout_setup(command)

    def _materialize_fresh_checkout(self) -> None:
        assert self.fresh is not None
        self.fresh.mkdir(parents=True, exist_ok=True)
        (self.fresh / "scripts").mkdir(parents=True, exist_ok=True)
        (self.fresh / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
        if self.fresh_complete:
            dep = self.fresh / "agent-utils/rs" / self.fresh_runner_name
            dep.mkdir(parents=True, exist_ok=True)
            (dep / "Cargo.toml").write_text("[package]\n")
            if self.fresh_runner_executable:
                executable = self.fresh / "agent-utils/common/bin" / self.fresh_runner_name
                executable.parent.mkdir(parents=True, exist_ok=True)
                executable.write_text("#!/bin/sh\n")
        if self.plant_receipt is not None:
            receipt = self.fresh / self.plant_receipt
            receipt.parent.mkdir(parents=True, exist_ok=True)
            receipt.write_text(f'{{"commit": "{SHA}", "cwd": "{self.fresh}"}}\n')

    def _run_after_checkout_setup(self, command: list[str]):
        if command[:2] == ["rm", "-rf"]:
            shutil.rmtree(command[-1], ignore_errors=True)
            return completed(command)
        if command[:1] == ["timeout"] and "fetch" in command:
            return completed(command)
        if command[:2] == ["git", "clone"]:
            self.fresh = Path(command[-1])
            self._materialize_fresh_checkout()
            return completed(command)
        if command[:5] == ["git", "-C", str(self.checkout), "remote", "get-url"]:
            return completed(command, stdout="https://github.com/rrnewton/hermit.git\n")
        if (
            self.fresh is not None
            and command[:3] == ["git", "-C", str(self.fresh)]
            and command[3] in {"remote", "checkout", "update-ref"}
        ):
            return completed(command)
        if (
            self.fresh is not None
            and command[:1] == ["git"]
            and len(command) >= 5
            and command[1] == "-C"
            and command[3:] == ["rev-parse", "--show-toplevel"]
        ):
            location = Path(command[2])
            nested = self.fresh / "agent-utils"
            owner = nested if location == nested or nested in location.parents else self.fresh
            return completed(command, stdout=f"{owner}\n")
        if self.fresh is not None and command[:4] == ["git", "-C", str(self.fresh), "rev-parse"]:
            if command[-1] == "--git-common-dir":
                common = self.checkout / ".git"
                common.mkdir(exist_ok=True)
                return completed(command, stdout=f"{common}\n")
            return completed(command, stdout=f"{SHA}\n")
        if command[:3] == ["git", "-C", str(self.checkout)] and command[3] in {"worktree", "submodule"}:
            if command[3:5] == ["worktree", "remove"]:
                self.removed.append(command[-1])
                shutil.rmtree(command[-1], ignore_errors=True)
            return completed(command)
        if (
            self.fresh is not None
            and command[:3] == ["git", "-C", str(self.fresh)]
            and command[3] == "worktree"
        ):
            if command[3:5] == ["worktree", "remove"]:
                self.removed.append(command[-1])
                shutil.rmtree(command[-1], ignore_errors=True)
            return completed(command)
        if self.fresh is not None and command[:4] == ["git", "-C", str(self.fresh), "submodule"]:
            return completed(command)
        if (
            self.fresh is not None
            and command[:1] == ["git"]
            and len(command) >= 7
            and command[1] == "-C"
            and command[3:5] == ["ls-files", "--error-unmatch"]
        ):
            owner = Path(command[2])
            candidate = owner / command[-1]
            relative = str(candidate.relative_to(self.fresh))
            tracked = set(self.fresh_tracked_jsonl.splitlines())
            return completed(command, rc=0 if relative in tracked else 1)
        if command[0].endswith("ci-hub") and command[1:3] == ["validate-status", "--sha"]:
            verdict = self.canonical_verdict
            status_rc = self.canonical_status_rc
            rows = self.ledger_rows
            if rows is None:
                rows = [json.dumps(self.default_ledger_row())]
            qualifying = len(rows) if verdict == "VALIDATED" else 0
            parsed_rows = [json.loads(row) for row in rows]
            report = {
                "sha": SHA,
                "verdict": verdict,
                "exit_code": status_rc,
                "qualifying_count": qualifying,
                "disqualified_count": len(rows) - qualifying,
                "newest_qualifying": (
                    self.selected_receipt(parsed_rows[self.canonical_selected_row_index])
                    if qualifying
                    else None
                ),
                "qualifying_receipts": (
                    [self.selected_receipt(row) for row in parsed_rows]
                    if qualifying
                    else []
                ),
            }
            return completed(
                command,
                rc=status_rc,
                stdout=json.dumps(report),
            )
        if len(command) >= 3 and command[-2].endswith("validate_rows.py") and command[-1] == "rows":
            rows = self.ledger_rows
            if rows is None:
                rows = [json.dumps(self.default_ledger_row())]
            return completed(command, stdout="\n".join(rows) + ("\n" if rows else ""))
        if (
            command[:4] == ["git", "-C", str(self.checkout), "rev-parse"]
            and command[-1] == "refs/remotes/origin/main^{commit}"
        ):
            return completed(command, stdout=f"{self.current_main}\n")
        if (
            command[:4] == ["git", "-C", str(self.checkout), "merge-base"]
            and command[4] == "--is-ancestor"
        ):
            return completed(command, rc=0 if self.target_contains_current_main else 1)
        if command[:4] == ["git", "-C", str(self.checkout), "rev-parse"]:
            if command[-1] == "--show-toplevel":
                return completed(command, stdout=f"{self.checkout}\n")
            if command[-1] == "--git-common-dir":
                if not self.source_common_dir_available:
                    return completed(command, rc=1, stderr="source checkout missing")
                common = self.checkout / ".git"
                common.mkdir(exist_ok=True)
                return completed(command, stdout=f"{common}\n")
            return completed(command, stdout=f"{self.source_head}\n")
        if command[:4] == ["git", "-C", str(self.checkout), "status"]:
            return completed(command, stdout=self.dirty)
        if command[:1] == [str(self.checkout / "ci/compat-envelope/scorecard.rs")]:
            if self.scorecard_rc == 0 and self.scorecard_update:
                for relative in start_unit.SCORECARD_PATHS:
                    path = self.checkout / relative
                    path.write_text(path.read_text() + "updated\n")
            state = "changed" if self.scorecard_update else "unchanged"
            return completed(
                command,
                rc=self.scorecard_rc,
                stdout=(
                    "compatibility scorecard: merged test observations\n"
                    f"compatibility scorecard: generated files {state}\n"
                ),
                stderr=self.scorecard_stderr,
            )
        if command[0].endswith("preflight_validate.py"):
            return completed(
                command,
                rc=self.admission_rc,
                stderr="stale base" if self.admission_rc else "",
            )
        if command[0] == "systemd-run" and "herdr" in command:
            herdr = command[command.index("herdr") :]
            if herdr == ["herdr", "status", "--json"]:
                return completed(
                    command,
                    rc=self.herdr_status_rc,
                    stdout=json.dumps({"server": {"running": True}}),
                    stderr="jail denied" if self.herdr_status_rc else "",
                )
            if herdr == ["herdr", "server"]:
                return completed(command, stdout="Running as unit: ci-hub-herdr.service\n")
            if herdr == ["herdr", "workspace", "list"]:
                return completed(
                    command,
                    stdout=json.dumps(
                        {
                            "result": {
                                "workspaces": [
                                    {
                                        "workspace_id": "wV",
                                        "label": "validate-hermit",
                                    }
                                ]
                            }
                        }
                    ),
                )
            if herdr == ["herdr", "tab", "list", "--workspace", "wV"]:
                return completed(command, stdout=json.dumps({"result": {"tabs": []}}))
            if herdr[:3] == ["herdr", "tab", "create"]:
                return completed(
                    command,
                    stdout=json.dumps(
                        {
                            "result": {
                                "root_pane": {"pane_id": "wV:p2"},
                                "tab": {"tab_id": "wV:t2"},
                            }
                        }
                    ),
                )
            if herdr[:3] == ["herdr", "pane", "rename"]:
                return completed(command)
            if herdr[:3] == ["herdr", "pane", "run"]:
                return completed(command)
            raise AssertionError(command)
        if command[0] == "systemd-run" and "validate-lock" in command:
            output = next(
                value.removeprefix("StandardOutput=append:")
                for value in command
                if value.startswith("StandardOutput=append:")
            )
            lines: list[str] = []
            if self.executed_tests is not None:
                lines.append(f"   {self.executed_tests} test(s) executed, 0 filtered")
            if self.executed_nodes is not None:
                lines.append(
                    f"   nodes: {self.executed_nodes} executed, 0 failed, 0 skipped in 1s at -j 16"
                )
            if self.final_validate_status is not None:
                lines.append(
                    f"FINAL_VALIDATE_STATUS: {self.final_validate_status}"
                )
            lines.append(
                "# ci-hub/validate-lock tool COST ACTUAL wall=1.0s cpu=0.1s "
                f"cpu_user=0.1s cpu_system=0.0s exit={self.actual_exit}"
            )
            Path(output).write_text("\n".join(lines) + "\n")
            environment: dict[str, str] = {}
            for flag, value in zip(command, command[1:]):
                if flag == "--setenv" and "=" in value:
                    key, _, item = value.partition("=")
                    environment[key] = item
            if frozen := environment.get("HERMIT_VALIDATE_LEDGER"):
                row = self.default_ledger_row()
                row["result"] = "pass" if self.actual_exit == 0 else "fail"
                row["raw_result"] = row["result"]
                row["exit_code"] = self.actual_exit
                result_path = Path(frozen)
                result_path.parent.mkdir(parents=True, exist_ok=True)
                result_path.write_text(json.dumps(row) + "\n")
            return completed(command, stdout="Running as unit: validate-test.service\n")
        if command[:3] == ["systemctl", "--user", "show"]:
            if self.collected_unit:
                return completed(
                    command,
                    stdout=(
                        "LoadState=not-found\nActiveState=inactive\nSubState=dead\n"
                        "ExecMainCode=0\nExecMainStatus=0\nResult=success\nInvocationID=\n"
                    ),
                )
            return completed(
                command,
                stdout=(
                    "LoadState=loaded\nActiveState=inactive\nSubState=dead\n"
                    "ExecMainCode=exited\nExecMainStatus=0\nResult=success\n"
                    "InvocationID=fixture-invocation\n"
                ),
            )
        raise AssertionError(command)


class StartUnitTest(unittest.TestCase):
    def test_run_handle_kind_is_the_validate_lock_kind(self) -> None:
        self.assertEqual(
            "validate",
            start_unit.validation_kind("rrnewton/hermit", frozen_validate=False),
        )
        self.assertEqual(
            "reverie-validate",
            start_unit.validation_kind("rrnewton/reverie", frozen_validate=False),
        )
        self.assertEqual(
            "frozen-validate",
            start_unit.validation_kind("rrnewton/hermit", frozen_validate=True),
        )

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.original_host_tmp_root = start_unit.HOST_TMP_ROOT
        # Most unit fixtures live below the host's real /tmp. Model a separate
        # host-temp subtree so ordinary positive-path tests remain meaningful;
        # the planted real-/tmp negative restores the production constant.
        start_unit.HOST_TMP_ROOT = self.root / "modeled-host-tmp"
        self.checkout = self.root / "hermit"
        self.checkout.mkdir()
        (self.checkout / "scripts").mkdir()
        (self.checkout / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
        compat = self.checkout / "ci/compat-envelope"
        compat.mkdir(parents=True)
        (compat / "scorecard.rs").write_text("#!/usr/bin/env rust-script\n")
        (self.checkout / "SCORECARD.md").write_text("scorecard\n")
        (compat / "cells.json").write_text("{}\n")
        (self.root / "ci-hub/validate").mkdir(parents=True)
        self.fake = FakeRun(self.checkout)
        self.environment = {
            "HOME": "/home/test",
            "PATH": "/usr/bin:/bin",
            "XDG_RUNTIME_DIR": str(self.root),
        }

    def tearDown(self) -> None:
        start_unit.HOST_TMP_ROOT = self.original_host_tmp_root
        self.temporary.cleanup()

    def invoke(self, extra: list[str] | None = None) -> tuple[int, str, str]:
        out = io.StringIO()
        err = io.StringIO()
        argv = [
            "--checkout",
            str(self.checkout),
            "--agent",
            "hermit-test",
            "--target",
            SHA,
            "--unit",
            "validate-test",
            "--log",
            str(self.root / "run.log"),
            *(extra or []),
        ]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                argv,
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )
        return rc, out.getvalue(), err.getvalue()

    def removed_temporary_paths(self) -> list[Path]:
        return [
            Path(command[-1])
            for command in self.fake.commands
            if command[:2] == ["rm", "-rf"]
        ]

    def plant_per_cell_results(self, unit: str = "validate-test") -> Path:
        results = self.root / "ignored/validate/artifacts" / unit / "e2e"
        result_file = results / "portable" / "results.jsonl"
        result_file.parent.mkdir(parents=True, exist_ok=True)
        result_file.write_text("{}\n")
        return results

    def test_positive_launch_routes_systemd_service_through_validate_lock(self) -> None:
        rc, output, error = self.invoke(["--pr", "123", "--", "full", "--ignore-cache"])

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertIn(str(self.root / "ci-hub/ci-hub"), systemd)
        lock = systemd.index("validate-lock")
        self.assertEqual(["validate-lock", "run"], systemd[lock : lock + 2])
        self.assertEqual(
            [
                "/usr/bin/env",
                "PR_NUMBER=123",
                sys.executable,
                str(self.root / "ci-hub/validate/run_with_validate_deadline.py"),
                "with-proxy",
                "./scripts/validate.rs",
                "full",
                "--ignore-cache",
            ],
            systemd[-8:],
        )
        self.assertIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS=3239", systemd)
        pane_run = next(command for command in self.fake.commands if "pane_watch.py" in " ".join(command))
        self.assertIn("herdr", pane_run)
        self.assertNotIn("scripts/validate.rs", pane_run)
        self.assertFalse(any(command[0] == "herdr" for command in self.fake.commands))
        self.assertIn("HANDLE", output)
        self.assertIn("PANE workspace=wV tab=wV:t2 pane=wV:p2", output)
        self.assertIn("FINISHED", output)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(1, record["schema_version"])
        self.assertEqual("validate", record["kind"])
        self.assertEqual("completed", record["state"])
        self.assertEqual("success", record["result"])
        self.assertEqual("PASSED", record["final_validate_status"])
        self.assertEqual(55, record["executed_nodes"])
        self.assertEqual("observer-only", record["pane_role"])
        self.assertEqual(3600, record["validate_lock_child_deadline_seconds"])
        self.assertEqual(3239, record["hermit_run_timeout_seconds"])

    def test_completed_validate_updates_scorecard_after_receipt_before_cleanup(self) -> None:
        self.plant_per_cell_results()
        self.fake.scorecard_update = True

        rc, output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertIn("compatibility scorecard: generated files changed", output)
        self.assertIn(
            f"git -C {self.checkout} diff -- SCORECARD.md ci/compat-envelope/cells.json",
            output,
        )
        self.assertTrue((self.checkout / "SCORECARD.md").read_text().endswith("updated\n"))
        self.assertTrue(
            (self.checkout / "ci/compat-envelope/cells.json")
            .read_text()
            .endswith("updated\n")
        )
        receipt_index = next(
            index
            for index, command in enumerate(self.fake.commands)
            if command[0].endswith("ci-hub")
            and command[1:3] == ["validate-status", "--sha"]
        )
        scorecard_index = next(
            index
            for index, command in enumerate(self.fake.commands)
            if command[0].endswith("scorecard.rs")
        )
        cleanup_index = next(
            index
            for index, command in enumerate(self.fake.commands)
            if wrkslots_action(command, "remove")
        )
        self.assertLess(receipt_index, scorecard_index)
        self.assertLess(scorecard_index, cleanup_index)

    def test_no_per_cell_results_is_an_explicit_unchanged_success(self) -> None:
        rc, output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertIn("compatibility scorecard: generated files unchanged", output)
        self.assertTrue(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))

    def test_scorecard_refuses_a_moved_source_checkout(self) -> None:
        results = self.plant_per_cell_results()
        self.fake.source_head = "b" * 40
        out, error = io.StringIO(), io.StringIO()

        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(error):
            updated = start_unit.write_scorecard_from_results(
                self.checkout, SHA, results, run=self.fake, json_output=False
            )

        self.assertFalse(updated)
        self.assertIn("compatibility scorecard NOT UPDATED", error.getvalue())
        self.assertIn("established validate verdict is unchanged", error.getvalue())
        self.assertFalse(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))

    def test_generated_file_changes_are_left_to_the_scorecard_writer(self) -> None:
        self.plant_per_cell_results()
        (self.checkout / "SCORECARD.md").write_text("pending review\n")

        rc, _output, error = self.invoke()

        self.assertEqual(0, rc, error)
        self.assertTrue(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))

    def test_red_validate_still_updates_scorecard(self) -> None:
        self.plant_per_cell_results()
        self.fake.canonical_verdict = "FAILED"
        self.fake.canonical_status_rc = start_unit.EXIT_FAILED
        self.fake.scorecard_update = True

        rc, output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_FAILED, rc, error)
        self.assertIn("verdict=FAILED exit=3", output)
        self.assertIn("compatibility scorecard: generated files changed", output)
        self.assertTrue(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))

    def test_scorecard_failure_makes_green_command_nonzero_after_printing_verdict(self) -> None:
        self.plant_per_cell_results()
        self.fake.scorecard_rc = 2
        self.fake.scorecard_stderr = "observe-results refuses unrelated tracked changes\n"

        rc, output, error = self.invoke()

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("RECEIPT-CANONICAL", output)
        self.assertIn("verdict=VALIDATED exit=0", output)
        self.assertIn("refuses unrelated tracked changes", error)
        self.assertIn("compatibility scorecard NOT UPDATED", error)
        self.assertIn("established validate verdict is unchanged", error)

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_linked_parent_uses_shared_state_and_exact_tooling(
        self, _create_pane: mock.Mock
    ) -> None:
        linked = self.root / "worktrees/slots/147/dev-hermit"
        (linked / "ci-hub/validate").mkdir(parents=True)
        self.fake.current_main = "b" * 40
        self.fake.target_contains_current_main = False

        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                [
                    "--checkout",
                    str(self.checkout),
                    "--state-root",
                    str(self.root),
                    "--agent",
                    "hermit-test",
                    "--target",
                    SHA,
                    "--unit",
                    "validate-linked-frozen",
                    "--frozen-validate",
                    "--",
                    "full",
                ],
                run=self.fake,
                environment=self.environment,
                root=linked,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(0, rc, err.getvalue())
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-linked-frozen.json"
        )
        self.assertEqual(
            start_unit.frozen_checkout_parent(self.root),
            Path(record["checkout"]).parent,
        )
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertIn(str(linked / "ci-hub/ci-hub"), systemd)
        self.assertEqual(
            str(self.root / "ignored/validate/runs/validate-linked-frozen.json"),
            systemd[systemd.index("--run-record") + 1],
        )
        self.assertIn(f"DEV_HERMIT_PARENT={self.root}", systemd)
        self.assertNotIn(f"DEV_HERMIT_PARENT={linked}", systemd)

    def test_sweep_completed_archives_receipts_and_removes_checkout(self) -> None:
        fresh = self.root / "worktrees" / "validate" / "validate-fresh-sweep"
        receipt = fresh / "reports" / "result.jsonl"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(f'{{"commit": "{SHA}"}}\n')
        cargo_home = self.root / "worktrees/validate/validate-cargo-sweep"
        cargo_home.mkdir()
        self.fake.fresh = fresh
        record_path = self.root / "ignored/validate/runs/validate-sweep.json"
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": "validate-sweep.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "cargo_home": str(cargo_home),
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        out = io.StringIO()
        err = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=False
        ), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc, err.getvalue())
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertEqual([str(cargo_home)], report["removed_cargo_homes"])
        self.assertEqual([], report["retained"])
        self.assertFalse(fresh.exists())
        self.assertFalse(cargo_home.exists())
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-sweep/reports/result.jsonl"
        )
        self.assertEqual(f'{{"commit": "{SHA}"}}\n', archived.read_text())
        durable = start_unit.run_registry.read_record(record_path)
        self.assertIsInstance(durable.get("checkout_removed_at"), str)
        self.assertIsInstance(durable.get("cargo_home_removed_at"), str)
        self.assertEqual([str(archived)], durable["archived_orphaned_receipts"])

        out = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=False
        ), contextlib.redirect_stdout(out):
            second_rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )
        self.assertEqual(0, second_rc)
        self.assertEqual([], json.loads(out.getvalue())["removed"])
        self.assertEqual([], json.loads(out.getvalue())["removed_cargo_homes"])
        self.assertEqual(
            [str(archived)],
            start_unit.run_registry.read_record(record_path)[
                "archived_orphaned_receipts"
            ],
        )

    def test_sweep_completed_removes_legacy_checkout_under_ignored(self) -> None:
        fresh = self.root / "ignored" / "validate-fresh-legacy"
        receipt = fresh / "reports" / "result.jsonl"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(f'{{"commit": "{SHA}"}}\n')
        self.fake.fresh = fresh
        record_path = self.root / "ignored/validate/runs/validate-legacy.json"
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": "validate-legacy.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                # The historical record predates temporary_checkout.  Its exact
                # parent and validate-fresh- prefix establish what it is.
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        out = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=False
        ), contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertFalse(fresh.exists())
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-legacy/reports/result.jsonl"
        )
        self.assertEqual(f'{{"commit": "{SHA}"}}\n', archived.read_text())

    def test_sweep_completed_does_not_query_irrelevant_retained_records(self) -> None:
        start_unit.run_registry.write_record(
            self.root / "ignored/validate/runs/bench-old.json",
            {
                "schema_version": 1,
                "unit": "bench-old.service",
                "checkout": str(self.checkout),
                "state": "completed",
            },
        )

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc)
        self.assertEqual([], json.loads(out.getvalue())["retained"])
        self.assertFalse(
            any(command[:3] == ["systemctl", "--user", "show"] for command in self.fake.commands)
        )

    def test_sweep_completed_removes_checkout_from_pre_run_number_record(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-pre-run-number"
        fresh.mkdir(parents=True)
        self.fake.fresh = fresh
        record_path = self.root / "ignored/validate/runs/validate-pre-run-number.json"
        record_path.parent.mkdir(parents=True)
        record_path.write_text(
            json.dumps(
                {
                    "schema_version": start_unit.run_registry.SCHEMA_VERSION,
                    "kind": "validate",
                    "unit": "validate-pre-run-number.service",
                    "target": SHA,
                    "checkout": str(fresh),
                    "source_checkout": str(self.checkout),
                    "temporary_checkout": True,
                    "cargo_home": str(
                        self.root / "worktrees/validate/validate-cargo-already-gone"
                    ),
                    "repo": "rrnewton/hermit",
                    "log": str(self.root / "ignored/validate/pre-run-number.log"),
                    "agent": "old-agent",
                    "started_at": "2026-08-28T00:00:00Z",
                    "producer": start_unit.run_registry.PRODUCER,
                    "admission": "ci-hub validate-lock",
                    "pane_role": "observer-only",
                    "parent_checkout_head": None,
                    "validate_lock_child_deadline_seconds": 3600,
                    "e2e_result_root": str(self.root / "ignored/validate/artifacts/e2e"),
                    "safe_ci_dag_runner_log_dir": str(
                        self.root / "ignored/validate/artifacts/safe-ci-dag-runner"
                    ),
                    "hermit_run_timeout_seconds": 3239,
                    "admission_result": {
                        "state": "admitted",
                        "recorded_at": "2026-08-28T00:00:00Z",
                    },
                    "state": "completed",
                    "result": "success",
                    "exit_code": 0,
                    "finished_at": "2026-08-28T00:10:00Z",
                    "final_validate_status": "PASSED",
                }
            )
            + "\n"
        )

        out = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=False
        ), contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertEqual([], report["retained"])
        self.assertEqual(str(record_path), report["bookkeeping_errors"][0]["record"])
        self.assertIn("run_number", report["bookkeeping_errors"][0]["reason"])
        self.assertFalse(fresh.exists())

    def test_sweep_completed_refuses_checkout_outside_validate_parent(self) -> None:
        checkout = self.root / "worktrees" / "slots" / "slot01" / "hermit"
        checkout.mkdir(parents=True)
        start_unit.run_registry.write_record(
            self.root / "ignored/validate/runs/validate-wrong-parent.json",
            {
                "schema_version": 1,
                "unit": "validate-wrong-parent.service",
                "checkout": str(checkout),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "repo": "rrnewton/hermit",
                "state": "completed",
            },
        )

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([], report["removed"])
        self.assertEqual(
            [
                {
                    "checkout": str(checkout),
                    "reason": (
                        "temporary checkout is outside "
                        f"{self.root / 'worktrees' / 'validate'} or {self.root / 'ignored'}"
                    ),
                }
            ],
            report["retained"],
        )
        self.assertTrue(checkout.is_dir())

    def test_sweep_completed_removes_frozen_checkout_outside_dev_hermit(self) -> None:
        fresh = start_unit.frozen_checkout_parent(self.root) / "validate-fresh-frozen"
        (fresh / ".git").mkdir(parents=True)
        self.fake.fresh = fresh
        record_path = self.root / "ignored/validate/runs/validate-frozen-sweep.json"
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": "validate-frozen-sweep.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        out = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=False
        ), contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertFalse(fresh.exists())

    def test_sweep_completed_removes_frozen_checkout_from_short_lived_location(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-frozen-old"
        (fresh / ".git").mkdir(parents=True)
        self.fake.fresh = fresh
        record_path = self.root / "ignored/validate/runs/validate-frozen-old-sweep.json"
        start_unit.run_registry.write_record(
            record_path,
            {
                "schema_version": 1,
                "unit": "validate-frozen-old-sweep.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "admission": start_unit.FROZEN_RESULT_ADMISSION,
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 75,
                "final_validate_status": "COULD_NOT_RUN",
            },
        )

        out = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=False
        ), contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(0, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([str(fresh)], report["removed"])
        self.assertFalse(fresh.exists())

    def test_sweep_completed_retains_when_unit_and_terminal_record_are_absent(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-running"
        fresh.mkdir(parents=True)
        self.fake.collected_unit = True
        start_unit.run_registry.write_record(
            self.root / "ignored/validate/runs/validate-running.json",
            {
                "schema_version": 1,
                "unit": "validate-running.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "repo": "rrnewton/hermit",
                "state": "running",
            },
        )

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([], report["removed"])
        self.assertEqual(
            "unit state unavailable and run record is not terminal",
            report["retained"][0]["reason"],
        )
        self.assertTrue(fresh.is_dir())

    def test_sweep_completed_retains_terminal_checkout_used_by_a_process(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-held"
        fresh.mkdir(parents=True)
        start_unit.run_registry.write_record(
            self.root / "ignored/validate/runs/validate-held.json",
            {
                "schema_version": 1,
                "unit": "validate-held.service",
                "checkout": str(fresh),
                "source_checkout": str(self.checkout),
                "temporary_checkout": True,
                "repo": "rrnewton/hermit",
                "state": "completed",
                "exit_code": 0,
                "final_validate_status": "PASSED",
            },
        )

        out = io.StringIO()
        with mock.patch.object(
            start_unit, "path_may_have_live_process", return_value=True
        ), contextlib.redirect_stdout(out):
            rc = start_unit.main(
                ["--sweep-completed", "--json"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
            )

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        report = json.loads(out.getvalue())
        self.assertEqual([], report["removed"])
        self.assertEqual("a live process may still use the checkout", report["retained"][0]["reason"])
        self.assertTrue(fresh.is_dir())

    def test_sweep_completed_rejects_ignored_launch_flags(self) -> None:
        for flag in ("--make-validate", "--allow-owner-validate-symlink"):
            with self.subTest(flag=flag):
                out = io.StringIO()
                err = io.StringIO()
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                    rc = start_unit.main(
                        ["--sweep-completed", flag],
                        run=self.fake,
                        environment=self.environment,
                        root=self.root,
                    )
                self.assertEqual(2, rc)
                self.assertIn("cannot be combined", err.getvalue())

    def test_cleanup_liveness_includes_an_open_file_descriptor(self) -> None:
        checkout = self.root / "worktrees/validate/validate-fresh-held"
        checkout.mkdir(parents=True)
        artifact = checkout / "artifact"
        artifact.write_text("held\n")
        proc = self.root / "proc" / "123"
        (proc / "fd").mkdir(parents=True)
        (proc / "map_files").mkdir()
        (proc / "cwd").symlink_to(self.root)
        (proc / "exe").symlink_to(sys.executable)
        (proc / "root").symlink_to("/")
        (proc / "fd" / "7").symlink_to(artifact)

        self.assertTrue(
            start_unit.path_may_have_live_process(checkout, proc_root=proc.parent)
        )

    def test_orphan_archive_retry_requires_identical_regular_file(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-archive"
        receipt = fresh / "receipt.jsonl"
        receipt.parent.mkdir(parents=True)
        receipt.write_text("first\n")

        first = start_unit.archive_orphaned_receipts(
            self.root, fresh, ["receipt.jsonl"], unit="validate-archive"
        )
        second = start_unit.archive_orphaned_receipts(
            self.root, fresh, ["receipt.jsonl"], unit="validate-archive"
        )
        self.assertEqual(first, second)

        receipt.write_text("changed\n")
        with self.assertRaisesRegex(RuntimeError, "different content"):
            start_unit.archive_orphaned_receipts(
                self.root, fresh, ["receipt.jsonl"], unit="validate-archive"
            )

    def test_hermit_run_timeout_is_derived_inside_child_deadline(self) -> None:
        self.assertEqual(3239, start_unit.hermit_run_timeout_seconds(3600))
        inner = start_unit.hermit_run_timeout_seconds(3600)
        cleanup = max(start_unit.HERMIT_MIN_CLEANUP_GRACE_SECONDS, inner // 10)
        self.assertLess(inner + cleanup, 3600)

        # The minimum 60-second cleanup branch needs the explicit one-second
        # separation too. Without the final subtraction this becomes 60 + 60
        # == 120 and the outer deadline can fire at the same instant.
        floor_inner = start_unit.hermit_run_timeout_seconds(120)
        self.assertEqual(59, floor_inner)
        self.assertLess(floor_inner + 60, 120)

    def test_hermit_run_timeout_cannot_be_widened_past_ceiling(self) -> None:
        # The 3h40m incident used a 7200-second child deadline, which previously
        # widened Hermit's cumulative run budget to 6479 seconds.  Extra outer
        # time may remain available for cleanup, but not for more validation.
        self.assertEqual(4200, start_unit.hermit_run_timeout_seconds(7200))
        self.assertEqual(4200, start_unit.hermit_run_timeout_seconds(24 * 60 * 60))

        cleanup = max(start_unit.HERMIT_MIN_CLEANUP_GRACE_SECONDS, 4200 // 10)
        self.assertLess(4200 + cleanup, 7200)

    def test_hermit_child_deadline_cannot_be_widened_past_backstop(self) -> None:
        rc, _output, error = self.invoke(["--child-deadline", "7200", "--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIn("limiting Hermit --child-deadline from 7200s to 4800s", error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertEqual(systemd[systemd.index("--child-deadline") + 1], "4800")
        self.assertIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS=4200", systemd)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(4800, record["validate_lock_child_deadline_seconds"])
        self.assertEqual(4200, record["hermit_run_timeout_seconds"])

    def test_hermit_child_deadline_too_small_for_cleanup_is_refused(self) -> None:
        rc, _output, error = self.invoke(["--child-deadline", "60", "--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("cannot contain its required", error)
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )

    def test_caller_cannot_override_launcher_owned_run_timeout(self) -> None:
        for args in (["--run-timeout", "3599"], ["--run-timeout=3599"]):
            with self.subTest(args=args):
                self.fake.commands.clear()
                rc, _output, error = self.invoke(["--", *args])
                self.assertEqual(2, rc)
                self.assertIn("--run-timeout is owned by validate-run", error)
                self.assertFalse(
                    any(command[0] == "systemd-run" for command in self.fake.commands)
                )

    def test_ambient_ci_dag_jobs_is_not_inherited_without_explicit_override(self) -> None:
        self.environment["CI_DAG_JOBS"] = "64"
        with mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None):
            rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertFalse(any(item.startswith("CI_DAG_JOBS=") for item in systemd))
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertNotIn("dag_jobs", record)

    def test_explicit_ci_dag_jobs_reaches_unit_and_run_record(self) -> None:
        with mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None):
            rc, _output, error = self.invoke(["--ci-dag-jobs", "32", "--", "full"])

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertEqual(1, systemd.count("CI_DAG_JOBS=32"))
        index = systemd.index("CI_DAG_JOBS=32")
        self.assertEqual("--setenv", systemd[index - 1])
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(32, record["dag_jobs"])

    def test_nonpositive_ci_dag_jobs_is_refused_before_launch(self) -> None:
        for value in ("0", "-1"):
            with self.subTest(value=value):
                self.fake.commands.clear()
                rc, _output, error = self.invoke(["--ci-dag-jobs", value])
                self.assertEqual(2, rc)
                self.assertIn("--ci-dag-jobs must be positive", error)
                self.assertFalse(
                    any(command[0] == "systemd-run" for command in self.fake.commands)
                )

    def test_collected_sigterm_records_and_returns_143(self) -> None:
        self.fake.actual_exit = 143
        self.fake.final_validate_status = None
        self.fake.executed_nodes = None
        self.fake.executed_tests = None
        self.fake.collected_unit = True
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(143, rc, error)
        self.assertIn("inner validate exited 143", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("killed", record["state"])
        self.assertEqual("terminated", record["result"])
        self.assertEqual(143, record["exit_code"])
        self.assertEqual(15, record["signal"])

    def test_exit_zero_without_status_is_unknown(self) -> None:
        self.fake.actual_exit = 0
        self.fake.final_validate_status = None
        self.fake.executed_nodes = 0
        self.fake.executed_tests = 0
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(75, rc, error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual("unknown", record["state"])
        self.assertEqual("unknown", record["result"])
        self.assertEqual(0, record["exit_code"])
        self.assertEqual(0, record["executed_nodes"])

    def test_dry_run_is_non_mutating_but_exposes_exact_command(self) -> None:
        rc, output, error = self.invoke(["--dry-run"])

        self.assertEqual(0, rc, error)
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )
        self.assertIn("WOULD-START", output)
        self.assertIn("PANE-PLAN workspace=validate-hermit role=observer-only", output)
        self.assertIn("ci-hub validate-lock run", output)
        self.assertFalse((self.root / "run.log").exists())

    def test_dirty_checkout_is_refused_before_admission(self) -> None:
        self.fake.dirty = " M scripts/validate.rs\n"

        rc, _output, error = self.invoke()

        self.assertEqual(2, rc)
        self.assertIn("checkout is dirty", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_unchanged_sha_can_be_measured_again_without_status_lookup(self) -> None:
        rc, output, error = self.invoke(["--dry-run", "--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIn("WOULD-START", output)
        self.assertFalse(
            any(command[1:3] == ["validate-status", "--sha"] for command in self.fake.commands)
        )
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )

    @mock.patch.object(start_unit.pane_owner, "create_pane", return_value=None)
    def test_same_sha_can_run_again_after_main_moves_and_is_not_a_current_receipt(
        self, _create_pane: mock.Mock
    ) -> None:
        """The named end-to-end bracket: same SHA, main advances, second run completes."""
        self.fake.current_main = SHA
        self.fake.target_contains_current_main = True
        first_rc, _first_output, first_error = self.invoke(["--", "full"])
        self.assertEqual(0, first_rc, first_error)

        # The first run used ordinary current-main admission. The same target is
        # now missing the freshly observed main represented by FakeRun's second
        # SHA, so only the explicit frozen path may run it again.
        self.fake.current_main = "b" * 40
        self.fake.target_contains_current_main = False
        self.fake.fresh = None
        second_rc, second_output, second_error = self.invoke(
            [
                "--unit",
                "validate-test-repeat",
                "--log",
                str(self.root / "run-repeat.log"),
                "--frozen-validate",
                "--",
                "full",
            ]
        )

        self.assertEqual(0, second_rc, second_error)
        systemd_runs = [
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        ]
        self.assertEqual(2, len(systemd_runs))
        self.assertEqual(
            [SHA, SHA],
            [command[command.index("--target") + 1] for command in systemd_runs],
        )
        self.assertEqual(
            ["validate", start_unit.FROZEN_VALIDATE_KIND],
            [command[command.index("--kind") + 1] for command in systemd_runs],
        )
        frozen_env = {
            value.partition("=")[0]: value.partition("=")[2]
            for flag, value in zip(systemd_runs[1], systemd_runs[1][1:])
            if flag == "--setenv" and "=" in value
        }
        self.assertEqual(
            "1",
            frozen_env[
                "VALIDATE_SKIP_INNER_DIRTY_WORKING_TREE_AND_REBASE_FRESHNESS_CHECKS"
            ],
        )
        self.assertEqual("1", frozen_env["VALIDATE_IGNORE_CACHE"])
        self.assertNotIn("VALIDATE_LABEL_PR", frozen_env)
        self.assertIn("--no-label-pr", systemd_runs[1])
        clone_index = next(
            (
                index
                for index, command in enumerate(self.fake.commands)
                if command[:2] == ["git", "clone"]
            ),
            None,
        )
        self.assertIsNotNone(clone_index, "a frozen checkout needs independent refs")
        self.assertNotIn(
            "--shared",
            self.fake.commands[clone_index],
            "the frozen checkout must not borrow an object store hidden by the pinned root",
        )
        self.assertIn(
            [
                "git",
                "-C",
                str(self.fake.fresh),
                "update-ref",
                "refs/remotes/origin/main",
                SHA,
            ],
            self.fake.commands,
        )
        self.assertFalse(
            any(
                command[3:5] == ["worktree", "add"]
                for command in self.fake.commands[clone_index:]
            ),
            "a worktree would share origin/main with concurrent source checkouts",
        )
        self.assertIn("MEASURED-AGAINST-SUPERSEDED-TIP", second_output)
        self.assertIn("qualifying_receipt=false", second_output)

        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test-repeat.json"
        )
        self.assertEqual(start_unit.FROZEN_RESULT_ADMISSION, record["admission"])
        self.assertEqual(start_unit.FROZEN_VALIDATE_KIND, record["validation_kind"])
        self.assertTrue(record["measured_against_superseded_tip"])
        self.assertFalse(record["target_contains_current_main_before_launch"])
        self.assertFalse(record["qualifying_receipt"])
        self.assertEqual(self.fake.current_main, record["current_main_before_launch"])
        frozen_parent = start_unit.frozen_checkout_parent(self.root)
        self.assertEqual(frozen_parent, Path(record["checkout"]).parent)
        self.assertNotIn(
            self.root,
            Path(record["checkout"]).parents,
            "a frozen checkout inside dev-hermit triggers the canonical product front door, which correctly rejects frozen-validate authority",
        )

        result_path = Path(record["result_record"])
        frozen_row = json.loads(result_path.read_text().strip())
        self.assertEqual(start_unit.FROZEN_RESULT_ADMISSION, frozen_row["admission"])
        self.assertTrue(frozen_row["measured_against_superseded_tip"])
        self.assertFalse(frozen_row["qualifying_receipt"])
        self.assertEqual(self.fake.current_main, frozen_row["current_main_before_launch"])

        # Only the ordinary first run consulted the canonical ledger. The
        # frozen result is deliberately outside it and cannot inherit that run's
        # VALIDATED verdict merely because both target the same SHA.
        canonical_reads = [
            command
            for command in self.fake.commands
            if command[0].endswith("ci-hub")
            and command[1:3] == ["validate-status", "--sha"]
        ]
        self.assertEqual(1, len(canonical_reads))

    def test_frozen_validate_refuses_a_target_that_still_contains_current_main(self) -> None:
        self.fake.target_contains_current_main = True

        rc, _output, error = self.invoke(["--frozen-validate", "--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("current enough for ordinary validate-run", error)
        self.assertFalse(
            any(command[0] == "systemd-run" for command in self.fake.commands)
        )

    def test_frozen_validate_cannot_reenable_receipt_backed_label(self) -> None:
        rc, _output, error = self.invoke(
            ["--frozen-validate", "--", "full", "--label-pr"]
        )

        self.assertEqual(2, rc)
        self.assertIn("cannot publish a receipt-backed label", error)
        self.assertFalse(self.fake.commands)

    def test_marking_frozen_result_makes_canonical_admission_reject_it(self) -> None:
        row = {
            **self.fake.default_ledger_row(),
            "schema_version": 5,
            "producer": "hermit-validate-rs",
            "admission": "ci-hub-validate-lock",
            "concurrent_validates": 0,
            "concurrency_proof": "validate_lock_owner_ancestry",
            "cwd": str(self.checkout),
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
        }
        predicate = qualifying_receipt.active()
        self.assertEqual(
            qualifying_receipt.AdmissionVerdict.SATISFIED,
            qualifying_receipt.admission_verdict(row, predicate),
        )

        result_path = self.root / "ignored/validate/frozen/result.jsonl"
        result_path.parent.mkdir(parents=True)
        result_path.write_text(json.dumps(row) + "\n")
        start_unit.mark_and_read_frozen_result(
            result_path,
            SHA,
            self.checkout,
            "2099-01-01T00:00:00Z",
            self.fake.current_main,
            repo="rrnewton/hermit",
        )
        marked = json.loads(result_path.read_text())

        self.assertEqual(
            qualifying_receipt.AdmissionVerdict.ADMISSION_NONCANONICAL,
            qualifying_receipt.admission_verdict(marked, predicate),
        )

    def test_exact_owner_validate_symlink_is_the_only_dirty_exception(self) -> None:
        (self.checkout / "validate").symlink_to("scripts/validate.rs")
        self.fake.dirty = "?? validate\n"

        rc, _output, error = self.invoke(
            ["--in-place", "--make-validate", "--allow-owner-validate-symlink"]
        )

        self.assertEqual(0, rc, error)
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        self.assertIn(
            "VALIDATE_SKIP_INNER_DIRTY_WORKING_TREE_AND_REBASE_FRESHNESS_CHECKS=1",
            systemd,
        )
        self.assertEqual(systemd[-3:], ["with-proxy", "make", "validate"])

    def test_owner_symlink_exception_refuses_any_second_dirty_path(self) -> None:
        (self.checkout / "validate").symlink_to("scripts/validate.rs")
        self.fake.dirty = "?? validate\n?? other\n"

        rc, _output, error = self.invoke(
            ["--in-place", "--make-validate", "--allow-owner-validate-symlink"]
        )

        self.assertEqual(2, rc)
        self.assertIn("checkout is dirty", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_owner_symlink_exception_refuses_wrong_target(self) -> None:
        (self.checkout / "validate").symlink_to("other")
        self.fake.dirty = "?? validate\n"

        rc, _output, error = self.invoke(
            ["--in-place", "--make-validate", "--allow-owner-validate-symlink"]
        )

        self.assertEqual(2, rc)
        self.assertIn("checkout is dirty", error)

    def test_real_host_tmp_source_is_refused_without_admission_or_ledger_write(self) -> None:
        """Plant the production hazard: this fixture itself is below real /tmp."""
        start_unit.HOST_TMP_ROOT = Path("/tmp").resolve()
        ledger = self.root / "ledger"
        record = self.root / "ignored/validate/runs/validate-test.json"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("source checkout resolves beneath host /tmp", error)
        self.assertIn("canonical non-/tmp dev-hermit parent", error)
        self.assertFalse(any(command[:2] == ["mktemp", "-d"] for command in self.fake.commands))
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertFalse(ledger.exists(), "refusal must not mutate the validation ledger")
        self.assertFalse(record.exists(), "refusal must not create a service record")
        self.assertFalse((self.root / "run.log").exists())

    def test_canonical_non_tmp_parent_is_accepted_by_placement_guard(self) -> None:
        canonical = self.root / "canonical-workspace" / "dev-hermit"

        self.assertEqual(
            canonical.resolve(),
            start_unit.require_guest_visible_root(canonical, role="source checkout"),
        )

    def test_tmpfoo_sibling_is_not_mistaken_for_host_tmp(self) -> None:
        start_unit.HOST_TMP_ROOT = Path("/tmp").resolve()
        tmpfoo = start_unit.HOST_TMP_ROOT.with_name("tmpfoo") / "dev-hermit"

        self.assertEqual(
            tmpfoo.resolve(),
            start_unit.require_guest_visible_root(tmpfoo, role="source checkout"),
        )

    def test_symlink_to_host_tmp_is_refused_after_canonicalization(self) -> None:
        hidden = start_unit.HOST_TMP_ROOT / "hidden-hermit"
        hidden.mkdir(parents=True)
        (hidden / "scripts").mkdir()
        (hidden / "scripts/validate.rs").write_text("#!/usr/bin/env rust-script\n")
        checkout_link = self.root / "apparently-safe-checkout"
        checkout_link.symlink_to(hidden, target_is_directory=True)
        self.checkout = checkout_link
        self.fake.checkout = hidden.resolve()

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn(str(hidden.resolve()), error)
        self.assertFalse(self.fake.commands, "canonicalization refusal must precede git/admission")
        self.assertFalse((self.root / "ledger").exists())

    def test_fresh_parent_symlinked_into_host_tmp_is_refused_before_mktemp(self) -> None:
        hidden_parent = start_unit.HOST_TMP_ROOT / "fresh-parent"
        hidden_parent.mkdir(parents=True)
        (self.root / "worktrees").mkdir()
        (self.root / "worktrees" / "validate").symlink_to(
            hidden_parent, target_is_directory=True
        )

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("validation checkout parent resolves beneath host /tmp", error)
        self.assertFalse(any(command[:2] == ["mktemp", "-d"] for command in self.fake.commands))
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))
        self.assertFalse((hidden_parent / "ledger").exists())

    def test_stale_head_is_refused_before_systemd_admission(self) -> None:
        self.fake.admission_rc = 2

        rc, _output, error = self.invoke()

        self.assertEqual(2, rc)
        self.assertIn("validation admission refused", error)
        self.assertIn("stale base", error)
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_visibility_failure_refuses_before_validation_service(self) -> None:
        self.fake.herdr_status_rc = 1

        rc, _output, error = self.invoke()

        self.assertEqual(2, rc)
        self.assertIn("Herdr server did not become ready", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            )
        )

    def test_attach_waits_on_existing_handle_without_relaunching(self) -> None:
        self.fake.validate_status_calls = 1
        self.fake.scorecard_update = True
        record = self.root / "ignored/validate/runs/validate-test.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "running",
                "unit": "validate-test.service",
                "target": SHA,
                "checkout": str(self.checkout),
                "source_checkout": str(self.checkout),
                "e2e_result_root": str(self.plant_per_cell_results()),
                "log": str(self.root / "run.log"),
                "started_at": "2026-01-01T00:00:00Z",
                "workspace_id": "wV",
                "tab_id": "wV:t2",
                "pane_id": "wV:p2",
            },
        )
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(0, rc, err.getvalue())
        self.assertIn("ATTACHED", out.getvalue())
        self.assertIn("FINISHED", out.getvalue())
        self.assertIn(
            "compatibility scorecard: generated files changed", out.getvalue()
        )
        self.assertTrue(any(command[0].endswith("scorecard.rs") for command in self.fake.commands))
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_attach_returns_canonical_nonpass_not_child_zero(self) -> None:
        self.fake.validate_status_calls = 1
        self.fake.canonical_verdict = "NOT-VALIDATED"
        self.fake.canonical_status_rc = 4
        record = self.root / "ignored/validate/runs/validate-test.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "running",
                "unit": "validate-test.service",
                "target": SHA,
                "checkout": str(self.checkout),
                "source_checkout": str(self.checkout),
                "e2e_result_root": str(self.plant_per_cell_results()),
                "log": str(self.root / "run.log"),
                "started_at": "2026-01-01T00:00:00Z",
                "workspace_id": "wV",
                "tab_id": "wV:t2",
                "pane_id": "wV:p2",
            },
        )
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(4, rc, err.getvalue())
        self.assertIn("verdict=NOT-VALIDATED exit=4", out.getvalue())
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_attach_cannot_inherit_older_green_from_same_checkout(self) -> None:
        """The exact review plant: no current row must not inherit an old pass."""
        self.fake.validate_status_calls = 1
        old_row = self.fake.default_ledger_row()
        old_row["started_at"] = "2000-01-01T00:00:01Z"
        old_row["finished_at"] = "2000-01-01T00:00:02Z"
        self.fake.ledger_rows = [json.dumps(old_row)]
        record = self.root / "ignored/validate/runs/validate-test.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "running",
                "unit": "validate-test.service",
                "target": SHA,
                "checkout": str(self.checkout),
                "log": str(self.root / "missing-current-run.log"),
                "started_at": "2099-01-01T00:00:00Z",
                "workspace_id": "wV",
                "tab_id": "wV:t2",
                "pane_id": "wV:p2",
            },
        )
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        # 75, not 2. This asserts the run's verdict was UNREADABLE, which is a
        # different condition from "the command was malformed" -- both used to
        # return 2 and a caller could not tell them apart. What this test actually
        # defends is unchanged: an unreadable verdict must not inherit an older
        # green, so the code must not be EXIT_PASSED.
        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc, err.getvalue())
        self.assertNotEqual(start_unit.EXIT_PASSED, rc, err.getvalue())
        self.assertNotEqual(start_unit.EXIT_REFUSED, rc, err.getvalue())
        self.assertIn("CANONICAL-VERDICT-UNAVAILABLE", err.getvalue())
        self.assertIn("an older row cannot bind this run", err.getvalue())
        self.assertFalse(any(command[0] == "systemd-run" for command in self.fake.commands))

    def test_attach_current_row_may_be_older_than_another_qualifying_same_sha_run(self) -> None:
        self.fake.validate_status_calls = 1
        current = self.fake.default_ledger_row()
        old = dict(current)
        old["started_at"] = "2000-01-01T00:00:01Z"
        old["finished_at"] = "2000-01-01T00:00:02Z"
        old["log_file"] = "/tmp/old-validate.log"
        self.fake.ledger_rows = [json.dumps(current), json.dumps(old)]
        self.fake.canonical_selected_row_index = 1
        record = self.root / "ignored/validate/runs/validate-test.json"
        start_unit.run_registry.write_record(
            record,
            {
                "schema_version": 1,
                "state": "running",
                "unit": "validate-test.service",
                "target": SHA,
                "checkout": str(self.checkout),
                "source_checkout": str(self.checkout),
                "e2e_result_root": str(self.plant_per_cell_results()),
                "log": str(self.root / "run.log"),
                "started_at": "2099-01-01T00:00:00Z",
            },
        )
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = start_unit.main(
                ["--attach", "validate-test"],
                run=self.fake,
                environment=self.environment,
                root=self.root,
                sleep=lambda _seconds: None,
            )

        self.assertEqual(start_unit.EXIT_PASSED, rc, err.getvalue())

    # ---- fresh temp-dir checkout is the DEFAULT (owner directive #3) --------

    def _working_directory(self) -> str:
        systemd = next(
            command
            for command in self.fake.commands
            if command[0] == "systemd-run" and "validate-lock" in command
        )
        return systemd[systemd.index("--working-directory") + 1]

    def test_default_validates_a_fresh_checkout_not_the_slot_tree(self) -> None:
        """The DEFAULT must validate the commit, not the tree that claims to be at it.

        `validate_checkout` already refuses a dirty tree, so this is not about
        uncommitted files. It is about the 5.1 GB of IGNORED state (build output,
        caches, materialized submodules) that `git status --porcelain=v1` cannot
        see and that has twice been measured deciding a verdict.
        """
        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIsNotNone(self.fake.fresh)
        self.assertEqual(str(self.fake.fresh), self._working_directory())
        self.assertNotEqual(str(self.checkout), self._working_directory())
        self.assertEqual(
            self.root / "worktrees" / "validate",
            self.fake.fresh.parent,
        )

        create = next(
            command
            for command in self.fake.commands
            if wrkslots_action(command, "create")
        )
        slot = create[create.index("create") + 1]
        self.assertEqual(
            f"checkout=wrkslots/validate/{slot}/checkout",
            create[create.index("--branch") + 1],
            "validate-run must satisfy wrkslots versions that require an explicit branch",
        )
        self.assertEqual(
            f"checkout={SHA}",
            create[create.index("--start") + 1],
        )

    def test_in_place_opt_out_still_validates_the_slot_tree(self) -> None:
        rc, _output, error = self.invoke(["--in-place", "--", "full"])

        self.assertEqual(0, rc, error)
        self.assertEqual(str(self.checkout), self._working_directory())
        self.assertFalse(
            any(
                wrkslots_action(command, "create")
                for command in self.fake.commands
            ),
            "opt-out must not build a temp checkout",
        )
        self.assertTrue(
            any(
                command[:2] == ["mktemp", "-d"]
                and len(command) > 2
                and "validate-cargo-" in command[2]
                for command in self.fake.commands
            ),
            "in-place validation must still receive its private Cargo home",
        )

    def test_incomplete_fresh_checkout_refuses_before_admission(self) -> None:
        """A fresh worktree starts with EMPTY submodules, agent-utils among them.

        Launching anyway reproduces the measured 0.045s exit that reads like a
        fast pass. The tree must be PROVEN usable, and an unusable one must abort
        the launch rather than becoming a quick green.
        """
        self.fake.fresh_complete = False

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(2, rc)
        self.assertIn("safe-ci-dag-runner", error)
        self.assertFalse(
            any(
                command[0] == "systemd-run" and "validate-lock" in command
                for command in self.fake.commands
            ),
            "nothing may be admitted from an unusable tree",
        )

    def test_new_dagrun_name_is_a_usable_fresh_checkout(self) -> None:
        self.fake.fresh_runner_name = "dagrun"

        fresh = start_unit.prepare_fresh_checkout(
            self.checkout,
            SHA,
            run=self.fake,
            parent=self.root / "worktrees" / "validate",
            repo="rrnewton/hermit",
        )

        self.assertEqual(self.fake.fresh, fresh)

    def test_crate_without_runner_executable_is_not_usable(self) -> None:
        self.fake.fresh_runner_executable = False

        with self.assertRaisesRegex(RuntimeError, "common/bin/dagrun"):
            start_unit.prepare_fresh_checkout(
                self.checkout,
                SHA,
                run=self.fake,
                parent=self.root / "worktrees" / "validate",
                repo="rrnewton/hermit",
            )

    # ---- REQUIREMENT (a): the row must be canonically readable BEFORE delete --

    def test_receipt_is_reread_from_the_canonical_ledger_before_cleanup(self) -> None:
        rc, output, error = self.invoke(["--", "full"])

        self.assertEqual(0, rc, error)
        self.assertIn("RECEIPT-CANONICAL", output)
        # The read must happen BEFORE the removal, or a missing row would be
        # indistinguishable from a deleted one.
        kinds = [
            (
                "status"
                if command[1:3] == ["validate-status", "--sha"]
                else "identity"
                if len(command) >= 2 and command[-2].endswith("validate_rows.py")
                else "remove"
            )
            for command in self.fake.commands
            if command[1:3] == ["validate-status", "--sha"]
            or (len(command) >= 2 and command[-2].endswith("validate_rows.py"))
            or wrkslots_action(command, "remove")
        ]
        self.assertEqual(["status", "identity", "remove"], kinds)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_cleanup_uses_wrkslots_even_when_source_is_missing(self) -> None:
        fresh = self.root / "worktrees/validate/validate-fresh-source-gone"
        fresh.mkdir(parents=True)
        (fresh / ".git").write_text("gitdir: shared/worktrees/source-gone\n")
        self.fake.fresh = fresh
        self.fake.source_common_dir_available = False

        removed = start_unit.remove_fresh_checkout(
            self.checkout, fresh, run=self.fake
        )

        self.assertTrue(removed)
        self.assertEqual([str(fresh)], self.fake.removed)
        self.assertTrue(
            any(
                command[0].endswith("ci-hub/bin/wrkslots")
                and "remove" in command
                and "--validate-complete" in command
                for command in self.fake.commands
            )
        )

    def test_canonical_nonpass_is_a_real_receipt_not_an_orphan(self) -> None:
        """A nonpass is not landing authority, but it is still a receipt.

        The old passing-only lookup made every such row look absent. The
        wrapper must report and return the canonical NEEDS-RERUN rather than
        its child's exit or entering the orphan-recovery path.
        """
        self.fake.actual_exit = 1
        self.fake.final_validate_status = "FAILED"
        self.fake.canonical_verdict = "NEEDS-RERUN"
        self.fake.canonical_status_rc = 4

        rc, output, error = self.invoke(["--", "full"])

        self.assertEqual(4, rc)
        self.assertIn("RECEIPT-CANONICAL", output)
        self.assertIn("verdict=NEEDS-RERUN", output)
        self.assertNotIn("RECEIPT-NOT-CANONICAL", error)
        self.assertNotIn("ORPHANED-RECEIPT", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_child_exit_zero_cannot_override_canonical_not_validated(self) -> None:
        """Live #2087 regression: 57/57 nodes passed but authority refused it."""
        self.fake.actual_exit = 0
        self.fake.canonical_verdict = "NOT-VALIDATED"
        self.fake.canonical_status_rc = 4

        rc, output, error = self.invoke(["--", "full"])

        self.assertEqual(4, rc)
        self.assertIn("verdict=NOT-VALIDATED exit=4", output)
        self.assertNotIn("CANONICAL-VERDICT-UNAVAILABLE", error)
        record = start_unit.run_registry.read_record(
            self.root / "ignored/validate/runs/validate-test.json"
        )
        self.assertEqual(0, record["exit_code"], "child exit remains diagnostic evidence")
        self.assertEqual("NOT-VALIDATED", record["canonical_verdict"])
        self.assertEqual(4, record["wrapper_exit_code"])

    def test_unreadable_canonical_status_never_falls_back_to_child_zero(self) -> None:
        self.fake.actual_exit = 0
        self.fake.canonical_verdict = "NOT-VALIDATED"
        self.fake.canonical_status_rc = 2

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("CANONICAL-VERDICT-UNAVAILABLE", error)

    def test_uncanonical_receipt_is_archived_before_temp_checkout_removal(self) -> None:
        """The negative that matters: an invisible green must be impossible.

        A temp-dir validate whose receipt lands only in the temp checkout's own
        ledger, followed by deletion, manufactures a green nobody can dereference
        — strictly worse than validating in place. The audit measured this exact
        shape: 111 validate.rs fallback rows in two per-checkout ledgers that
        default consumers discover ZERO of.

        THE FIXTURE NOW PLANTS THE HAZARD IT DESCRIBES. It previously only
        emptied the canonical ledger, which models "the canonical lookup found
        nothing" — true of this case AND of an ordinary early failure. Those
        need opposite dispositions, so the fixture has to distinguish them.
        """
        self.fake.ledger_rows = []
        self.plant_orphan_receipt()

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("CANONICAL-VERDICT-UNAVAILABLE", error)
        self.assertIn("ORPHANED-RECEIPT", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
        archived = (
            self.root
            / "ignored/validate/orphaned-receipts/validate-test/.hermit-validate-ledger.jsonl"
        )
        self.assertTrue(archived.is_file(), "evidence must survive checkout removal")
        self.assertIn(
            self.root / "worktrees/validate/validate-cargo-abcd1234",
            self.removed_temporary_paths(),
        )

    def plant_orphan_receipt(self, rel: str = ".hermit-validate-ledger.jsonl") -> None:
        """Have the run write its receipt inside its own temp checkout."""
        self.fake.plant_receipt = rel

    def test_ordinary_early_failure_leaves_NO_retained_tree(self) -> None:
        """The reclaim half. The Rust driver is fail-fast: its first gate can abort
        in seconds, so this is the COMMON outcome, and it used to strand a 26 MB
        worktree every time. Nothing was produced, so nothing is preserved."""
        self.fake.ledger_rows = []

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("NO-RECEIPT-PRODUCED", error)
        self.assertNotIn("ORPHANED-RECEIPT", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_a_receipt_under_an_UNANTICIPATED_name_is_archived(self) -> None:
        """Why the test is SHAPE, not a whitelist of ledger filenames.

        A whitelist fails in the dangerous direction: a receipt written under a
        name nobody enumerated reads as "no evidence" and gets deleted. Any
        untracked *.jsonl in the tree counts.
        """
        self.fake.ledger_rows = []
        self.plant_orphan_receipt("ci/some-unanticipated-receipt.jsonl")

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("ORPHANED-RECEIPT", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)
        self.assertTrue(
            (
                self.root
                / "ignored/validate/orphaned-receipts/validate-test/ci/some-unanticipated-receipt.jsonl"
            ).is_file()
        )

    def test_a_TRACKED_jsonl_is_not_mistaken_for_a_produced_receipt(self) -> None:
        """Attribution is by tracked-ness. A .jsonl that git tracks at this
        commit came from the checkout, not from the run, and must not pin a
        26 MB tree forever."""
        self.fake.ledger_rows = []
        self.plant_orphan_receipt("fixtures/committed.jsonl")
        self.fake.fresh_tracked_jsonl = "fixtures/committed.jsonl\n"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("NO-RECEIPT-PRODUCED", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_a_TRACKED_jsonl_in_a_submodule_is_not_an_orphan(self) -> None:
        """Trackedness belongs to the nested repository, not its parent.

        The outer Hermit index contains only the agent-utils gitlink, so asking
        it about an agent-utils fixture falsely classified the fixture as a
        run-produced orphan.
        """
        self.fake.ledger_rows = []
        relative = "agent-utils/fixtures/committed.jsonl"
        self.plant_orphan_receipt(relative)
        self.fake.fresh_tracked_jsonl = f"{relative}\n"

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("NO-RECEIPT-PRODUCED", error)
        self.assertNotIn("ORPHANED-RECEIPT", error)
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)

    def test_a_scan_that_cannot_look_retains_rather_than_reclaims(self) -> None:
        """Fail-closed. A NO-RESULT is not a negative: if the tree cannot be
        inspected we do not know whether evidence is in it, so it stays."""
        self.fake.ledger_rows = []
        original = start_unit.orphaned_receipt_locations

        def explode(*_a: object, **_k: object) -> list[str]:
            raise OSError("permission denied")

        start_unit.orphaned_receipt_locations = explode
        self.addCleanup(setattr, start_unit, "orphaned_receipt_locations", original)

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("ORPHANED-RECEIPT", error)
        self.assertIn("scan failed", error)
        self.assertEqual([], self.fake.removed)
        self.assertIn(
            self.root / "worktrees/validate/validate-cargo-abcd1234",
            self.removed_temporary_paths(),
        )

    def test_a_row_for_the_same_sha_from_a_DIFFERENT_run_does_not_satisfy_it(self) -> None:
        """Identity binding, not a correlated proxy.

        Matching on the commit alone would accept some other run of the same SHA
        — including an in-place run, or a stale row from hours earlier. The row
        must carry this exact temp checkout's cwd.
        """
        self.fake.ledger_rows = [f'{{"commit": "{SHA}", "cwd": "/some/other/checkout"}}']

        rc, _output, error = self.invoke(["--", "full"])

        self.assertEqual(start_unit.EXIT_COULD_NOT_DETERMINE, rc)
        self.assertIn("CANONICAL-VERDICT-UNAVAILABLE", error)
        # The property under test is IDENTITY BINDING -- a foreign row must not
        # satisfy the check. Retention is a separate question: this run left no
        # receipt in its own tree, so there is nothing to preserve and the tree
        # is reclaimed. Asserting retention here conflated the two.
        self.assertEqual([str(self.fake.fresh)], self.fake.removed)


if __name__ == "__main__":
    unittest.main()
